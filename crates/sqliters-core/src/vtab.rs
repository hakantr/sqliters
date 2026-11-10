//! Virtual table host context, module/instance registry, lifecycle hooks, `dbstat`,
//! and eponymous `pragma_*` virtual table modules (`sqlite/src/vtab.c`, `pragma.c`, `dbstat.c`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use sqliters_ext::GenerateSeriesModule;
use sqliters_syntax::{
    Expr, FromClause, FunctionArgs, InsertSource, JoinConstraint, PragmaValue, QualifiedName,
    ResultColumn, SelectCore, SelectStmt, Stmt, TableOrSubquery, TriggerStep, WindowRef, dequote,
    parse_one,
};
use sqliters_types::{
    DatabaseId, ErrorCode, Pgno, Result, SqliteError, VTabConfigOption, VTabConstraintOp,
    VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor,
    VirtualTable, VirtualTableModule, get_varint,
};

use crate::db_set::{DatabaseSet, ResolvedRelation};
use crate::engine::ExecContext;
pub use crate::execution_context::catch_callback_panic;
use crate::schema::{ColumnSchema, TableSchema, build_table_schema_from_ast};
use crate::trigger::TriggerEventKind;

/// Alias for [`VTabDeclaredSchema`].
pub type DeclaredVTabSchema = VTabDeclaredSchema;

/// Key `(db_name, table_name)` identifying a connected virtual table instance.
pub type VTabInstanceKey = (String, String);

/// Declared virtual table schema produced by `VTabHostContext::declare_vtab`.
#[derive(Debug, Clone)]
pub struct VTabDeclaredSchema {
    pub table_schema: TableSchema,
    pub hidden_cols: Vec<bool>,
    pub config_risk_flags: Option<VTabRiskFlags>,
    pub constraint_support: bool,
}

impl VTabDeclaredSchema {
    /// Returns true if column `col_idx` was declared with the `HIDDEN` keyword.
    #[inline]
    pub fn is_hidden(&self, col_idx: usize) -> bool {
        self.hidden_cols.get(col_idx).copied().unwrap_or(false)
    }

    /// Returns a clone of `table_schema` containing only non-hidden columns (for wildcard `*` expansion),
    /// alongside a mapping from visible column index to physical virtual table column index.
    pub fn visible_columns(&self) -> Vec<(usize, ColumnSchema)> {
        self.table_schema
            .columns
            .iter()
            .enumerate()
            .filter(|(idx, _)| !self.is_hidden(*idx))
            .map(|(idx, col)| (idx, col.clone()))
            .collect()
    }
}

/// Strips any `HIDDEN` keyword token from a column's `type_name`, returning `(cleaned_type_name, is_hidden)`.
pub fn strip_hidden_from_type_name(type_name: &str) -> (String, bool) {
    let mut is_hidden = false;
    let mut kept = Vec::new();
    for word in type_name.split_whitespace() {
        if word.eq_ignore_ascii_case("hidden") {
            is_hidden = true;
        } else {
            kept.push(word);
        }
    }
    (kept.join(" "), is_hidden)
}

/// Parses a `CREATE TABLE ...` declaration passed to `declare_vtab`.
pub fn parse_declare_vtab_sql(
    table_name: &str,
    create_table_sql: &str,
) -> Result<(TableSchema, Vec<bool>)> {
    let (Some(Stmt::CreateTable(mut ct)), tail) = parse_one(create_table_sql)? else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "vtable constructor did not declare schema",
        ));
    };
    if !tail.trim().is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "extra input in declare_vtab SQL",
        ));
    }
    let mut hidden_cols = Vec::with_capacity(ct.columns.len());
    for cdef in &mut ct.columns {
        let (cleaned, is_hidden) = strip_hidden_from_type_name(&cdef.type_name);
        cdef.type_name = cleaned;
        hidden_cols.push(is_hidden);
    }
    let (mut tbl, _) = build_table_schema_from_ast(&ct, 0)?;
    tbl.name = table_name.to_owned();
    Ok((tbl, hidden_cols))
}

/// Checkout state of a connected virtual table instance slot.
pub enum VTabInstanceState {
    Available(Box<dyn VirtualTable>),
    Busy,
}

impl std::fmt::Debug for VTabInstanceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available(_) => f.write_str("Available(..)"),
            Self::Busy => f.write_str("Busy"),
        }
    }
}

/// Entry for a connected virtual table in [`VTabRegistry`].
#[derive(Debug)]
pub struct ConnectedVTabEntry {
    pub db_name: String,
    pub table_name: String,
    pub module_name: String,
    pub module_risk_flags: VTabRiskFlags,
    pub shadow_suffixes: Vec<String>,
    pub declared: VTabDeclaredSchema,
    pub state: VTabInstanceState,
    pub in_tx: bool,
}

impl ConnectedVTabEntry {
    #[inline]
    pub fn effective_risk_flags(&self) -> VTabRiskFlags {
        self.declared
            .config_risk_flags
            .unwrap_or(self.module_risk_flags)
    }
}

/// Per-connection registry of virtual table modules and connected virtual table instances.
pub struct VTabRegistry {
    modules: BTreeMap<String, (String, Arc<dyn VirtualTableModule>)>,
    connected: BTreeMap<(String, String), ConnectedVTabEntry>,
    pub trusted_schema: bool,
    pub defensive_mode: bool,
    pub internal_sql_depth: usize,
}

impl std::fmt::Debug for VTabRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VTabRegistry")
            .field("module_count", &self.modules.len())
            .field("connected", &self.connected)
            .field("trusted_schema", &self.trusted_schema)
            .field("defensive_mode", &self.defensive_mode)
            .field("internal_sql_depth", &self.internal_sql_depth)
            .finish()
    }
}

impl Default for VTabRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl VTabRegistry {
    /// Creates a new `VTabRegistry` pre-populated with built-in modules (`generate_series`, `dbstat`).
    pub fn new() -> Self {
        let mut reg = Self {
            modules: BTreeMap::new(),
            connected: BTreeMap::new(),
            trusted_schema: true,
            defensive_mode: false,
            internal_sql_depth: 0,
        };
        reg.register_module(Arc::new(GenerateSeriesModule));
        reg.register_module(Arc::new(DbStatModule));
        reg
    }

    /// Registers a virtual table module on the connection.
    pub fn register_module(&mut self, module: Arc<dyn VirtualTableModule>) {
        let orig_name = module.name().to_owned();
        let key = orig_name.to_ascii_lowercase();
        self.modules.insert(key, (orig_name, module));
    }

    /// Unregisters a virtual table module by name (`sqlite3_create_module_v2` with `pModule == NULL`).
    pub fn unregister_module(&mut self, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        self.modules.remove(&key).is_some()
    }

    /// Looks up a registered module by name (case-insensitive), lazily registering `pragma_<name>`
    /// eponymous modules when requested (`sqlite3PragmaVtabRegister` parity).
    pub fn get_or_register_module(&mut self, name: &str) -> Option<Arc<dyn VirtualTableModule>> {
        let key = name.to_ascii_lowercase();
        if let Some((_, m)) = self.modules.get(&key) {
            return Some(Arc::clone(m));
        }
        if let Some(pragma_mod) = PragmaVTabModule::try_for_module_name(name) {
            let arc: Arc<dyn VirtualTableModule> = Arc::new(pragma_mod);
            self.modules
                .insert(key, (name.to_ascii_lowercase(), Arc::clone(&arc)));
            return Some(arc);
        }
        None
    }

    /// Looks up an already-registered module without lazy `pragma_*` registration.
    pub fn get_module(&self, name: &str) -> Option<Arc<dyn VirtualTableModule>> {
        let key = name.to_ascii_lowercase();
        self.modules.get(&key).map(|(_, m)| Arc::clone(m))
    }

    /// Returns the sorted list of currently registered module names (`PRAGMA module_list`).
    pub fn module_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.modules.values().map(|(n, _)| n.clone()).collect();
        names.sort();
        names
    }

    /// Alias for [`Self::module_names`].
    #[inline]
    pub fn module_names_sorted(&self) -> Vec<String> {
        self.module_names()
    }

    #[inline]
    fn slot_key(db_name: &str, table_name: &str) -> (String, String) {
        (
            db_name.to_ascii_lowercase(),
            table_name.to_ascii_lowercase(),
        )
    }

    /// Returns a reference to a connected virtual table entry if present.
    pub fn get_connected(&self, db_name: &str, table_name: &str) -> Option<&ConnectedVTabEntry> {
        self.connected.get(&Self::slot_key(db_name, table_name))
    }

    /// Returns a mutable reference to a connected virtual table entry if present.
    pub fn get_connected_mut(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Option<&mut ConnectedVTabEntry> {
        self.connected.get_mut(&Self::slot_key(db_name, table_name))
    }

    /// Inserts a newly created or connected virtual table entry.
    pub fn insert_connected(&mut self, entry: ConnectedVTabEntry) {
        let key = Self::slot_key(&entry.db_name, &entry.table_name);
        self.connected.insert(key, entry);
    }

    /// Removes and returns a connected virtual table entry.
    pub fn remove_connected(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Option<ConnectedVTabEntry> {
        self.connected.remove(&Self::slot_key(db_name, table_name))
    }

    /// Renames a connected virtual table entry from `old_name` to `new_name` in `db_name`.
    pub fn rename_connected(&mut self, db_name: &str, old_name: &str, new_name: &str) {
        if let Some(mut entry) = self.remove_connected(db_name, old_name) {
            entry.table_name = new_name.to_owned();
            entry.declared.table_schema.name = new_name.to_owned();
            self.insert_connected(entry);
        }
    }

    /// Disconnects all connected virtual tables belonging to `db_name` (e.g. on `DETACH` or schema reload).
    pub fn disconnect_schema(&mut self, db_name: &str) {
        let db_lower = db_name.to_ascii_lowercase();
        let keys: Vec<(String, String)> = self
            .connected
            .keys()
            .filter(|(d, _)| d == &db_lower)
            .cloned()
            .collect();
        for k in keys {
            if let Some(mut entry) = self.connected.remove(&k)
                && let VTabInstanceState::Available(ref mut vt) = entry.state
            {
                let _ = vt.disconnect();
            }
        }
    }

    /// Disconnects all connected virtual tables across all schemas (`sqlite3_close`).
    pub fn disconnect_all(&mut self) {
        let keys: Vec<(String, String)> = self.connected.keys().cloned().collect();
        for k in keys {
            if let Some(mut entry) = self.connected.remove(&k)
                && let VTabInstanceState::Available(ref mut vt) = entry.state
            {
                let _ = vt.disconnect();
            }
        }
    }

    /// Checks out the `Box<dyn VirtualTable>` from `(db_name, table_name)`, marking the slot `Busy`.
    /// Returns an error if the virtual table is already checked out (`Busy`) to prevent recursive aliasing.
    pub fn checkout_vtab(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Result<(Box<dyn VirtualTable>, VTabDeclaredSchema, VTabRiskFlags)> {
        let key = Self::slot_key(db_name, table_name);
        let entry = self.connected.get_mut(&key).ok_or_else(|| {
            SqliteError::new(ErrorCode::Error, format!("no such table: {table_name}"))
        })?;
        let prev = std::mem::replace(&mut entry.state, VTabInstanceState::Busy);
        match prev {
            VTabInstanceState::Available(vt) => {
                let decl = entry.declared.clone();
                let risk = entry.effective_risk_flags();
                Ok((vt, decl, risk))
            }
            VTabInstanceState::Busy => Err(SqliteError::new(
                ErrorCode::Locked,
                format!("virtual table \"{table_name}\" is locked"),
            )),
        }
    }

    /// Restores a previously checked-out `Box<dyn VirtualTable>` back into its slot.
    pub fn restore_vtab(&mut self, db_name: &str, table_name: &str, vt: Box<dyn VirtualTable>) {
        let key = Self::slot_key(db_name, table_name);
        if let Some(entry) = self.connected.get_mut(&key) {
            entry.state = VTabInstanceState::Available(vt);
        }
    }

    /// Returns all `(db_name, table_name)` keys currently marked `in_tx`.
    pub fn active_tx_vtabs(&self) -> Vec<(String, String)> {
        self.connected
            .values()
            .filter(|e| e.in_tx)
            .map(|e| (e.db_name.clone(), e.table_name.clone()))
            .collect()
    }
}

/// Validates whether a virtual table or table-valued function with `effective_risk` is allowed
/// in the current execution context (`trusted_schema` + top-level vs view/trigger).
pub fn check_vtab_risk(
    table_display_name: &str,
    effective_risk: VTabRiskFlags,
    trusted_schema: bool,
    in_view_or_trigger: bool,
) -> Result<()> {
    if in_view_or_trigger {
        match effective_risk {
            VTabRiskFlags::DirectOnly => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unsafe use of virtual table \"{table_display_name}\""),
                ));
            }
            VTabRiskFlags::Normal if !trusted_schema => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unsafe use of virtual table \"{table_display_name}\""),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Checks if `(db_id, table_name)` is a shadow table of any virtual table in `db_id` when
/// `SQLITE_DBCONFIG_DEFENSIVE` is active and not inside internal `VTabHostContext` sub-SQL.
pub fn check_defensive_shadow_table_write(
    db_set: &DatabaseSet,
    db_id: DatabaseId,
    table_name: &str,
) -> Result<()> {
    let reg = db_set.vtab_registry();
    if !reg.defensive_mode || reg.internal_sql_depth > 0 {
        return Ok(());
    }
    let Some(slot) = db_set.slot(db_id) else {
        return Ok(());
    };
    let candidate_lower = table_name.to_ascii_lowercase();
    for vt_def in &slot.schema.virtual_tables {
        let prefix = format!("{}_", vt_def.name.to_ascii_lowercase());
        if let Some(suffix) = candidate_lower.strip_prefix(&prefix)
            && let Some(module) = reg.get_module(&vt_def.module_name)
            && module
                .shadow_table_suffixes()
                .iter()
                .any(|s| s.eq_ignore_ascii_case(suffix))
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {table_name} may not be modified"),
            ));
        }
    }
    Ok(())
}

/// Concrete [`VTabHostContext`] passed to virtual table callbacks with controlled internal sub-SQL access.
pub struct CoreVTabHostContext<'a, 'b> {
    pub ctx: &'a mut ExecContext<'b>,
    pub db_name: String,
    pub table_name: String,
    pub declared: Option<VTabDeclaredSchema>,
    pub config_risk_flags: Option<VTabRiskFlags>,
    pub constraint_support: bool,
}

impl<'a, 'b> CoreVTabHostContext<'a, 'b> {
    pub fn new(
        ctx: &'a mut ExecContext<'b>,
        db_name: impl Into<String>,
        table_name: impl Into<String>,
    ) -> Self {
        Self {
            ctx,
            db_name: db_name.into(),
            table_name: table_name.into(),
            declared: None,
            config_risk_flags: None,
            constraint_support: false,
        }
    }

    pub fn take_declared_schema(&mut self) -> Result<VTabDeclaredSchema> {
        let mut decl = self.declared.take().ok_or_else(|| {
            SqliteError::new(
                ErrorCode::Error,
                "vtable constructor did not declare schema",
            )
        })?;
        decl.config_risk_flags = self.config_risk_flags;
        decl.constraint_support = self.constraint_support;
        Ok(decl)
    }
}

impl VTabHostContext for CoreVTabHostContext<'_, '_> {
    fn db_name(&self) -> &str {
        &self.db_name
    }

    fn table_name(&self) -> &str {
        &self.table_name
    }

    fn declare_vtab(&mut self, create_table_sql: &str) -> Result<()> {
        let (table_schema, hidden_cols) =
            parse_declare_vtab_sql(&self.table_name, create_table_sql)?;
        self.declared = Some(VTabDeclaredSchema {
            table_schema,
            hidden_cols,
            config_risk_flags: self.config_risk_flags,
            constraint_support: self.constraint_support,
        });
        Ok(())
    }

    fn set_vtab_config(&mut self, opt: VTabConfigOption) -> Result<()> {
        match opt {
            VTabConfigOption::ConstraintSupport => {
                self.constraint_support = true;
            }
            VTabConfigOption::Innocuous => {
                self.config_risk_flags = Some(VTabRiskFlags::Innocuous);
            }
            VTabConfigOption::DirectOnly => {
                self.config_risk_flags = Some(VTabRiskFlags::DirectOnly);
            }
        }
        if let Some(ref mut decl) = self.declared {
            decl.config_risk_flags = self.config_risk_flags;
            decl.constraint_support = self.constraint_support;
        }
        Ok(())
    }

    fn execute_sql(&mut self, sql: &str, params: &[Value]) -> Result<usize> {
        crate::engine::execute_internal_vtab_sql(self.ctx, sql, params)
    }

    fn query_sql(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>> {
        crate::engine::query_internal_vtab_sql(self.ctx, sql, params)
    }
}

/// Ensures that a persistent virtual table `(db_id, vt_def)` or an eponymous module is connected
/// in `ctx.db_set.vtab_registry`, returning its [`VTabDeclaredSchema`] and effective [`VTabRiskFlags`].
pub fn ensure_vtab_connected(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    table_name: &str,
    allow_eponymous: bool,
) -> Result<Option<(String, String, VTabDeclaredSchema, VTabRiskFlags)>> {
    let db_name = ctx
        .db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());

    // 1. Check if already connected in `(db_name, table_name)`
    if let Some(entry) = ctx
        .db_set
        .vtab_registry()
        .get_connected(&db_name, table_name)
    {
        let still_in_schema = ctx
            .db_set
            .slot(db_id)
            .is_some_and(|s| s.schema.find_virtual_table(table_name).is_some());
        let is_eponymous = ctx
            .db_set
            .vtab_registry()
            .get_module(&entry.module_name)
            .is_some_and(|m| m.is_eponymous());
        if still_in_schema || (allow_eponymous && is_eponymous) {
            return Ok(Some((
                entry.db_name.clone(),
                entry.table_name.clone(),
                entry.declared.clone(),
                entry.effective_risk_flags(),
            )));
        }
    }

    // 2. Check if `(db_id, table_name)` is a persistent `VirtualTableDef` in `slot.schema`
    let vt_def_opt = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.schema.find_virtual_table(table_name).cloned());

    if let Some(vt_def) = vt_def_opt {
        let module = ctx
            .db_set
            .vtab_registry_mut()
            .get_or_register_module(&vt_def.module_name)
            .ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("no such module: {}", vt_def.module_name),
                )
            })?;
        let mut host_ctx = CoreVTabHostContext::new(ctx, db_name.clone(), vt_def.name.clone());
        let vt_box = catch_callback_panic("vtab xConnect", || {
            module.connect(&mut host_ctx, &vt_def.args)
        })?;
        let declared = host_ctx.take_declared_schema()?;
        let effective_risk = declared
            .config_risk_flags
            .unwrap_or_else(|| module.risk_flags());
        let shadow_suffixes = module
            .shadow_table_suffixes()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let entry = ConnectedVTabEntry {
            db_name: db_name.clone(),
            table_name: vt_def.name.clone(),
            module_name: vt_def.module_name.clone(),
            module_risk_flags: module.risk_flags(),
            shadow_suffixes,
            declared: declared.clone(),
            state: VTabInstanceState::Available(vt_box),
            in_tx: false,
        };
        ctx.db_set.vtab_registry_mut().insert_connected(entry);
        return Ok(Some((db_name, vt_def.name, declared, effective_risk)));
    }

    // 3. Check if `table_name` matches an eponymous module
    if allow_eponymous {
        let module_opt = ctx
            .db_set
            .vtab_registry_mut()
            .get_or_register_module(table_name)
            .filter(|m| m.is_eponymous());
        if let Some(module) = module_opt {
            let mod_name = module.name().to_owned();
            let mut host_ctx = CoreVTabHostContext::new(ctx, db_name.clone(), mod_name.clone());
            let vt_box =
                catch_callback_panic("vtab xConnect", || module.connect(&mut host_ctx, &[]))?;
            let declared = host_ctx.take_declared_schema()?;
            let effective_risk = declared
                .config_risk_flags
                .unwrap_or_else(|| module.risk_flags());
            let shadow_suffixes = module
                .shadow_table_suffixes()
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            let entry = ConnectedVTabEntry {
                db_name: db_name.clone(),
                table_name: mod_name.clone(),
                module_name: mod_name.clone(),
                module_risk_flags: module.risk_flags(),
                shadow_suffixes,
                declared: declared.clone(),
                state: VTabInstanceState::Available(vt_box),
                in_tx: false,
            };
            ctx.db_set.vtab_registry_mut().insert_connected(entry);
            return Ok(Some((db_name, mod_name, declared, effective_risk)));
        }
    }

    Ok(None)
}

/// Lazily connects any persistent virtual tables or eponymous modules referenced by `stmt`
/// (including through views and triggers) and enforces [`VTabRiskFlags`] (`trusted_schema`,
/// `DirectOnly`, `Innocuous`).
pub fn ensure_stmt_vtabs_connected(ctx: &mut ExecContext<'_>, stmt: &Stmt) -> Result<()> {
    let mut visited_views = HashSet::new();
    ensure_stmt_vtabs_connected_inner(ctx, stmt, &HashSet::new(), false, &mut visited_views)
}

fn ensure_stmt_vtabs_connected_inner(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    match stmt {
        Stmt::Explain { stmt: inner, .. } => ensure_stmt_vtabs_connected_inner(
            ctx,
            inner,
            cte_names,
            in_view_or_trigger,
            visited_views,
        ),
        Stmt::Select(sel) => {
            ensure_select_vtabs_connected(ctx, sel, cte_names, in_view_or_trigger, visited_views)
        }
        Stmt::CreateTableAs { query, .. } => {
            ensure_select_vtabs_connected(ctx, query, cte_names, in_view_or_trigger, visited_views)
        }
        Stmt::Insert(ins) => {
            let mut local_ctes = cte_names.clone();
            for cte in &ins.ctes {
                local_ctes.insert(cte.name.to_ascii_lowercase());
                ensure_select_vtabs_connected(
                    ctx,
                    &cte.query,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            ensure_qname_vtab_connected(
                ctx,
                &ins.table,
                &local_ctes,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            match &ins.source {
                InsertSource::Select(sel) => ensure_select_vtabs_connected(
                    ctx,
                    sel,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?,
                InsertSource::Values(rows) => {
                    for row in rows {
                        for e in row {
                            ensure_expr_vtabs_connected(
                                ctx,
                                e,
                                &local_ctes,
                                in_view_or_trigger,
                                visited_views,
                            )?;
                        }
                    }
                }
                InsertSource::DefaultValues => {}
            }
            for uc in &ins.upsert_clauses {
                if let Some(ref t) = uc.target
                    && let Some(ref w) = t.where_clause
                {
                    ensure_expr_vtabs_connected(
                        ctx,
                        w,
                        &local_ctes,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
                if let sqliters_syntax::UpsertAction::DoUpdate {
                    assignments,
                    where_clause,
                } = &uc.action
                {
                    for a in assignments {
                        ensure_expr_vtabs_connected(
                            ctx,
                            &a.expr,
                            &local_ctes,
                            in_view_or_trigger,
                            visited_views,
                        )?;
                    }
                    if let Some(w) = where_clause {
                        ensure_expr_vtabs_connected(
                            ctx,
                            w,
                            &local_ctes,
                            in_view_or_trigger,
                            visited_views,
                        )?;
                    }
                }
            }
            for rc in &ins.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        &local_ctes,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            ensure_dml_triggers_vtabs_connected(
                ctx,
                &ins.table,
                TriggerEventKind::Insert,
                visited_views,
            )?;
            Ok(())
        }
        Stmt::Update(upd) => {
            let mut local_ctes = cte_names.clone();
            for cte in &upd.ctes {
                local_ctes.insert(cte.name.to_ascii_lowercase());
                ensure_select_vtabs_connected(
                    ctx,
                    &cte.query,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            ensure_qname_vtab_connected(
                ctx,
                &upd.table,
                &local_ctes,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            for a in &upd.assignments {
                ensure_expr_vtabs_connected(
                    ctx,
                    &a.expr,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            if let Some(ref from) = upd.from {
                ensure_from_vtabs_connected(
                    ctx,
                    from,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            if let Some(ref w) = upd.selection {
                ensure_expr_vtabs_connected(
                    ctx,
                    w,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            for rc in &upd.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        &local_ctes,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            ensure_dml_triggers_vtabs_connected(
                ctx,
                &upd.table,
                TriggerEventKind::Update,
                visited_views,
            )?;
            Ok(())
        }
        Stmt::Delete(del) => {
            let mut local_ctes = cte_names.clone();
            for cte in &del.ctes {
                local_ctes.insert(cte.name.to_ascii_lowercase());
                ensure_select_vtabs_connected(
                    ctx,
                    &cte.query,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            ensure_qname_vtab_connected(
                ctx,
                &del.table,
                &local_ctes,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            if let Some(ref w) = del.selection {
                ensure_expr_vtabs_connected(
                    ctx,
                    w,
                    &local_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            for rc in &del.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        &local_ctes,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            ensure_dml_triggers_vtabs_connected(
                ctx,
                &del.table,
                TriggerEventKind::Delete,
                visited_views,
            )?;
            Ok(())
        }
        Stmt::DropTable { name, .. } => {
            if let Some((db_id, vt)) = ctx
                .db_set
                .find_virtual_table_in_set(name)
                .map(|(id, v)| (id, v.clone()))
            {
                let _ = ensure_vtab_connected(ctx, db_id, &vt.name, false)?;
            }
            Ok(())
        }
        Stmt::AlterTable { table, .. } => {
            if let Some((db_id, vt)) = ctx
                .db_set
                .find_virtual_table_in_set(table)
                .map(|(id, v)| (id, v.clone()))
            {
                let _ = ensure_vtab_connected(ctx, db_id, &vt.name, false)?;
            }
            Ok(())
        }
        Stmt::Pragma {
            schema,
            name,
            value: Some(pv),
        } => {
            if matches!(
                name.to_ascii_uppercase().as_str(),
                "TABLE_INFO" | "TABLE_XINFO"
            ) {
                let tname = match pv {
                    PragmaValue::Ident(s) | PragmaValue::String(s) => s.clone(),
                    PragmaValue::Integer(n) => n.to_string(),
                };
                let qn = match schema {
                    Some(s) => QualifiedName::qualified(s.clone(), tname),
                    None => QualifiedName::unqualified(tname),
                };
                ensure_qname_vtab_connected(
                    ctx,
                    &qn,
                    cte_names,
                    true,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn ensure_dml_triggers_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    target: &QualifiedName,
    event: TriggerEventKind,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    let Ok(Some((db_id, rel))) = ctx.db_set.find_relation_in_set(target) else {
        return Ok(());
    };
    let rel_name = match &rel {
        ResolvedRelation::Table(t) => t.name.clone(),
        ResolvedRelation::View(v) => v.name.clone(),
    };
    let triggers = ctx.db_set.triggers_for_relation(db_id, &rel_name);
    for (trig_db_id, tr) in triggers {
        if !tr.matches_event_and_columns(event, None) {
            continue;
        }
        let trig_visit_key = (
            trig_db_id,
            format!("__trigger__:{}", tr.name.to_ascii_lowercase()),
        );
        if !visited_views.insert(trig_visit_key) {
            continue;
        }
        if let Some(ref w) = tr.when_clause {
            ensure_expr_vtabs_connected(ctx, w, &HashSet::new(), true, visited_views)?;
        }
        for step in &tr.body {
            let step_stmt = match step {
                TriggerStep::Select(s) => Stmt::Select(s.clone()),
                TriggerStep::Insert(i) => Stmt::Insert(i.clone()),
                TriggerStep::Update(u) => Stmt::Update(u.clone()),
                TriggerStep::Delete(d) => Stmt::Delete(d.clone()),
            };
            ensure_stmt_vtabs_connected_inner(
                ctx,
                &step_stmt,
                &HashSet::new(),
                true,
                visited_views,
            )?;
        }
    }
    Ok(())
}

fn ensure_qname_vtab_connected(
    ctx: &mut ExecContext<'_>,
    qname: &QualifiedName,
    cte_names: &HashSet<String>,
    allow_eponymous: bool,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    if qname.schema.is_none() && cte_names.contains(&qname.name.to_ascii_lowercase()) {
        return Ok(());
    }
    if let Ok(Some((db_id, rel))) = ctx.db_set.find_relation_in_set(qname) {
        if let ResolvedRelation::View(v) = rel {
            let v_select = v.select.clone();
            let v_key = (db_id, v.name.to_ascii_lowercase());
            if visited_views.insert(v_key.clone()) {
                let res = ensure_select_vtabs_connected(
                    ctx,
                    &v_select,
                    &HashSet::new(),
                    true,
                    visited_views,
                );
                visited_views.remove(&v_key);
                res?;
            }
        }
        return Ok(());
    }
    if let Some((db_id, vt)) = ctx
        .db_set
        .find_virtual_table_in_set(qname)
        .map(|(id, v)| (id, v.clone()))
    {
        if let Some((_, vt_name, _, risk)) = ensure_vtab_connected(ctx, db_id, &vt.name, false)? {
            check_vtab_risk(
                &vt_name,
                risk,
                ctx.db_set.vtab_registry().trusted_schema,
                in_view_or_trigger,
            )?;
        }
        return Ok(());
    }
    if allow_eponymous {
        let db_id = match &qname.schema {
            Some(s) => match ctx.db_set.resolve_schema_or_err(Some(s)) {
                Ok(id) => id,
                Err(_) => return Ok(()),
            },
            None => DatabaseId::MAIN,
        };
        if let Some((_, mod_name, _, risk)) = ensure_vtab_connected(ctx, db_id, &qname.name, true)?
        {
            check_vtab_risk(
                &mod_name,
                risk,
                ctx.db_set.vtab_registry().trusted_schema,
                in_view_or_trigger,
            )?;
        }
    }
    Ok(())
}

fn ensure_select_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    sel: &SelectStmt,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    let mut local_ctes = cte_names.clone();
    for cte in &sel.ctes {
        local_ctes.insert(cte.name.to_ascii_lowercase());
        ensure_select_vtabs_connected(
            ctx,
            &cte.query,
            &local_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
    }
    ensure_select_core_vtabs_connected(
        ctx,
        &sel.body.head,
        &local_ctes,
        in_view_or_trigger,
        visited_views,
    )?;
    for (_, right) in &sel.body.compounds {
        ensure_select_core_vtabs_connected(
            ctx,
            right,
            &local_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
    }
    for ob in &sel.order_by {
        ensure_expr_vtabs_connected(
            ctx,
            &ob.expr,
            &local_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
    }
    if let Some(ref lim) = sel.limit {
        ensure_expr_vtabs_connected(
            ctx,
            &lim.limit,
            &local_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
        if let Some(ref off) = lim.offset {
            ensure_expr_vtabs_connected(ctx, off, &local_ctes, in_view_or_trigger, visited_views)?;
        }
    }
    Ok(())
}

fn ensure_select_core_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    core: &SelectCore,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            windows,
            ..
        } => {
            if let Some(f) = from {
                ensure_from_vtabs_connected(ctx, f, cte_names, in_view_or_trigger, visited_views)?;
            }
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            if let Some(w) = selection {
                ensure_expr_vtabs_connected(ctx, w, cte_names, in_view_or_trigger, visited_views)?;
            }
            for g in group_by {
                ensure_expr_vtabs_connected(ctx, g, cte_names, in_view_or_trigger, visited_views)?;
            }
            if let Some(h) = having {
                ensure_expr_vtabs_connected(ctx, h, cte_names, in_view_or_trigger, visited_views)?;
            }
            for (_, ws) in windows {
                for p in &ws.partition_by {
                    ensure_expr_vtabs_connected(
                        ctx,
                        p,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
                for ob in &ws.order_by {
                    ensure_expr_vtabs_connected(
                        ctx,
                        &ob.expr,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            Ok(())
        }
        SelectCore::Values(rows) => {
            for row in rows {
                for e in row {
                    ensure_expr_vtabs_connected(
                        ctx,
                        e,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            Ok(())
        }
    }
}

fn ensure_from_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    from: &FromClause,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    ensure_tos_vtabs_connected(
        ctx,
        &from.head,
        cte_names,
        in_view_or_trigger,
        visited_views,
    )?;
    for join in &from.joins {
        ensure_tos_vtabs_connected(
            ctx,
            &join.right,
            cte_names,
            in_view_or_trigger,
            visited_views,
        )?;
        if let Some(JoinConstraint::On(ref on_expr)) = join.constraint {
            ensure_expr_vtabs_connected(
                ctx,
                on_expr,
                cte_names,
                in_view_or_trigger,
                visited_views,
            )?;
        }
    }
    Ok(())
}

fn ensure_tos_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    match tos {
        TableOrSubquery::Table {
            name, table_args, ..
        } => {
            ensure_qname_vtab_connected(
                ctx,
                name,
                cte_names,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            if let Some(args) = table_args {
                for a in args {
                    ensure_expr_vtabs_connected(
                        ctx,
                        a,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            Ok(())
        }
        TableOrSubquery::TableFunction {
            schema, name, args, ..
        } => {
            let qn = QualifiedName {
                schema: schema.clone(),
                name: name.clone(),
            };
            ensure_qname_vtab_connected(
                ctx,
                &qn,
                cte_names,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            for a in args {
                ensure_expr_vtabs_connected(ctx, a, cte_names, in_view_or_trigger, visited_views)?;
            }
            Ok(())
        }
        TableOrSubquery::Subquery { query, .. } => {
            ensure_select_vtabs_connected(ctx, query, cte_names, in_view_or_trigger, visited_views)
        }
    }
}

fn ensure_expr_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    cte_names: &HashSet<String>,
    in_view_or_trigger: bool,
    visited_views: &mut HashSet<(DatabaseId, String)>,
) -> Result<()> {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::OversizedHex(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_)
        | Expr::Column(_)
        | Expr::Raise { .. } => Ok(()),
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            ensure_expr_vtabs_connected(ctx, inner, cte_names, in_view_or_trigger, visited_views)
        }
        Expr::Binary { left, right, .. } => {
            ensure_expr_vtabs_connected(ctx, left, cte_names, in_view_or_trigger, visited_views)?;
            ensure_expr_vtabs_connected(ctx, right, cte_names, in_view_or_trigger, visited_views)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            ensure_expr_vtabs_connected(ctx, e, cte_names, in_view_or_trigger, visited_views)?;
            ensure_expr_vtabs_connected(ctx, low, cte_names, in_view_or_trigger, visited_views)?;
            ensure_expr_vtabs_connected(ctx, high, cte_names, in_view_or_trigger, visited_views)
        }
        Expr::InList { expr: e, list, .. } => {
            ensure_expr_vtabs_connected(ctx, e, cte_names, in_view_or_trigger, visited_views)?;
            for item in list {
                ensure_expr_vtabs_connected(
                    ctx,
                    item,
                    cte_names,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            ensure_expr_vtabs_connected(ctx, e, cte_names, in_view_or_trigger, visited_views)?;
            ensure_select_vtabs_connected(
                ctx,
                subquery,
                cte_names,
                in_view_or_trigger,
                visited_views,
            )
        }
        Expr::InTable { expr: e, table, .. } => {
            ensure_expr_vtabs_connected(ctx, e, cte_names, in_view_or_trigger, visited_views)?;
            ensure_qname_vtab_connected(
                ctx,
                table,
                cte_names,
                true,
                in_view_or_trigger,
                visited_views,
            )
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            ensure_select_vtabs_connected(ctx, sub, cte_names, in_view_or_trigger, visited_views)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            ensure_expr_vtabs_connected(ctx, e, cte_names, in_view_or_trigger, visited_views)?;
            ensure_expr_vtabs_connected(
                ctx,
                pattern,
                cte_names,
                in_view_or_trigger,
                visited_views,
            )?;
            if let Some(esc) = escape {
                ensure_expr_vtabs_connected(
                    ctx,
                    esc,
                    cte_names,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                ensure_expr_vtabs_connected(ctx, op, cte_names, in_view_or_trigger, visited_views)?;
            }
            for (w, t) in branches {
                ensure_expr_vtabs_connected(ctx, w, cte_names, in_view_or_trigger, visited_views)?;
                ensure_expr_vtabs_connected(ctx, t, cte_names, in_view_or_trigger, visited_views)?;
            }
            if let Some(el) = else_expr {
                ensure_expr_vtabs_connected(ctx, el, cte_names, in_view_or_trigger, visited_views)?;
            }
            Ok(())
        }
        Expr::RowValue(elems) => {
            for item in elems {
                ensure_expr_vtabs_connected(
                    ctx,
                    item,
                    cte_names,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            Ok(())
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    ensure_expr_vtabs_connected(
                        ctx,
                        a,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            if let Some(f) = filter {
                ensure_expr_vtabs_connected(ctx, f, cte_names, in_view_or_trigger, visited_views)?;
            }
            if let Some(WindowRef::Spec(ws)) = over {
                for p in &ws.partition_by {
                    ensure_expr_vtabs_connected(
                        ctx,
                        p,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
                for ob in &ws.order_by {
                    ensure_expr_vtabs_connected(
                        ctx,
                        &ob.expr,
                        cte_names,
                        in_view_or_trigger,
                        visited_views,
                    )?;
                }
            }
            Ok(())
        }
    }
}

/// Ensures `VirtualTable::begin` has been called on `(db_name, table_name)` prior to `update`.
pub fn vtab_begin_if_needed(
    ctx: &mut ExecContext<'_>,
    db_name: &str,
    table_name: &str,
) -> Result<()> {
    let already_in_tx = ctx
        .db_set
        .vtab_registry()
        .get_connected(db_name, table_name)
        .is_some_and(|e| e.in_tx);
    if already_in_tx {
        return Ok(());
    }
    let active_sp_depth = ctx.savepoints.len() + usize::from(ctx.stmt_tx.stmt_sp_slot.is_some());
    let (mut vt, _, _) = ctx
        .db_set
        .vtab_registry_mut()
        .checkout_vtab(db_name, table_name)?;
    let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
    let mut res = catch_callback_panic("vtab xBegin", || vt.begin(&mut host_ctx));
    if res.is_ok() {
        for sp_idx in 0..active_sp_depth {
            res = catch_callback_panic("vtab xSavepoint", || vt.savepoint(&mut host_ctx, sp_idx));
            if res.is_err() {
                break;
            }
        }
    }
    ctx.db_set
        .vtab_registry_mut()
        .restore_vtab(db_name, table_name, vt);
    res?;
    if let Some(entry) = ctx
        .db_set
        .vtab_registry_mut()
        .get_connected_mut(db_name, table_name)
    {
        entry.in_tx = true;
    }
    Ok(())
}

/// Performs `VirtualTable::update` on `(db_name, table_name)`.
pub fn vtab_perform_update(
    ctx: &mut ExecContext<'_>,
    db_name: &str,
    table_name: &str,
    op: VTabUpdateOperation,
) -> Result<Option<i64>> {
    vtab_begin_if_needed(ctx, db_name, table_name)?;
    let (mut vt, _, _) = ctx
        .db_set
        .vtab_registry_mut()
        .checkout_vtab(db_name, table_name)?;
    let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
    let res = catch_callback_panic("vtab xUpdate", || vt.update(&mut host_ctx, op));
    ctx.db_set
        .vtab_registry_mut()
        .restore_vtab(db_name, table_name, vt);
    res
}

/// Invokes `VirtualTable::sync` on all virtual tables participating in the current transaction.
pub fn vtab_sync_all(ctx: &mut ExecContext<'_>) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let (mut vt, _, _) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)?;
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let res = catch_callback_panic("vtab xSync", || vt.sync(&mut host_ctx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        res?;
    }
    Ok(())
}

/// Invokes `VirtualTable::commit` on all virtual tables participating in the current transaction.
pub fn vtab_commit_all(ctx: &mut ExecContext<'_>) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    let mut first_err: Option<SqliteError> = None;
    for (db_name, table_name) in active {
        let Ok((mut vt, _, _)) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)
        else {
            continue;
        };
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let res = catch_callback_panic("vtab xCommit", || vt.commit(&mut host_ctx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        if let Some(entry) = ctx
            .db_set
            .vtab_registry_mut()
            .get_connected_mut(&db_name, &table_name)
        {
            entry.in_tx = false;
        }
        if let Err(e) = res
            && first_err.is_none()
        {
            first_err = Some(e);
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Invokes `VirtualTable::rollback` on all virtual tables participating in the current transaction.
pub fn vtab_rollback_all(ctx: &mut ExecContext<'_>) {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let Ok((mut vt, _, _)) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)
        else {
            continue;
        };
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let _ = catch_callback_panic("vtab xRollback", || vt.rollback(&mut host_ctx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        if let Some(entry) = ctx
            .db_set
            .vtab_registry_mut()
            .get_connected_mut(&db_name, &table_name)
        {
            entry.in_tx = false;
        }
    }
}

/// Invokes `VirtualTable::savepoint` on all virtual tables participating in the current transaction.
pub fn vtab_savepoint_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let (mut vt, _, _) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)?;
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let res = catch_callback_panic("vtab xSavepoint", || vt.savepoint(&mut host_ctx, idx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        res?;
    }
    Ok(())
}

/// Invokes `VirtualTable::release` on all virtual tables participating in the current transaction.
pub fn vtab_release_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let (mut vt, _, _) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)?;
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let res = catch_callback_panic("vtab xRelease", || vt.release(&mut host_ctx, idx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        res?;
    }
    Ok(())
}

/// Invokes `VirtualTable::rollback_to` on all virtual tables participating in the current transaction.
pub fn vtab_rollback_to_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let (mut vt, _, _) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab(&db_name, &table_name)?;
        let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
        let res = catch_callback_panic("vtab xRollbackTo", || vt.rollback_to(&mut host_ctx, idx));
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        res?;
    }
    Ok(())
}

// ============================================================================
// Built-in `dbstat` Virtual Table (`sqlite/src/dbstat.c`)
// ============================================================================

const DBSTAT_SCHEMA: &str = "CREATE TABLE x(\
    name TEXT,\
    path TEXT,\
    pageno INTEGER,\
    pagetype TEXT,\
    ncell INTEGER,\
    payload INTEGER,\
    unused INTEGER,\
    mx_payload INTEGER,\
    pgoffset INTEGER,\
    pgsize INTEGER,\
    schema TEXT HIDDEN,\
    aggregate BOOLEAN HIDDEN\
)";

/// Built-in `dbstat` virtual table module (`VTabRiskFlags::DirectOnly`).
#[derive(Debug, Default, Clone, Copy)]
pub struct DbStatModule;

impl VirtualTableModule for DbStatModule {
    fn name(&self) -> &str {
        "dbstat"
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::DirectOnly
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn is_eponymous_only(&self) -> bool {
        false
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        self.connect(ctx, args)
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        if args.len() > 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "too many arguments to dbstat",
            ));
        }
        let default_schema = args
            .first()
            .map(|s| dequote(s.trim()))
            .filter(|s| !s.is_empty());
        ctx.declare_vtab(DBSTAT_SCHEMA)?;
        let _ = ctx.set_vtab_config(VTabConfigOption::DirectOnly);
        Ok(Box::new(DbStatTable { default_schema }))
    }
}

#[derive(Debug)]
struct DbStatTable {
    default_schema: Option<String>,
}

impl VirtualTable for DbStatTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let mut idx_num = 0i32;
        let mut schema_ci = None;
        let mut agg_ci = None;
        for (i, c) in info.constraints.iter().enumerate() {
            if c.op != VTabConstraintOp::Eq {
                continue;
            }
            if c.i_column == 10 {
                if !c.usable {
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable dbstat schema constraint",
                    ));
                }
                schema_ci = Some(i);
                idx_num |= 0x01;
            } else if c.i_column == 11 {
                if !c.usable {
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable dbstat aggregate constraint",
                    ));
                }
                agg_ci = Some(i);
                idx_num |= 0x02;
            }
        }
        let mut arg_pos = 0usize;
        if let Some(ci) = schema_ci {
            arg_pos += 1;
            info.constraint_usage[ci].argv_index = arg_pos;
            info.constraint_usage[ci].omit = true;
        }
        if let Some(ci) = agg_ci {
            arg_pos += 1;
            info.constraint_usage[ci].argv_index = arg_pos;
            info.constraint_usage[ci].omit = true;
        }
        info.idx_num = idx_num;
        info.estimated_cost = 100.0;
        info.estimated_rows = 100;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(DbStatCursor {
            default_schema: self.default_schema.clone(),
            rows: Vec::new(),
            pos: 0,
        }))
    }
}

#[derive(Debug, Clone)]
struct DbStatRow {
    name: String,
    path: Option<String>,
    pageno: i64,
    pagetype: Option<String>,
    ncell: i64,
    payload: i64,
    unused: i64,
    mx_payload: i64,
    pgoffset: Option<i64>,
    pgsize: i64,
    schema: String,
    aggregate: i64,
}

#[derive(Debug)]
struct DbStatCursor {
    default_schema: Option<String>,
    rows: Vec<DbStatRow>,
    pos: usize,
}

impl VirtualCursor for DbStatCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        let mut arg_pos = 0usize;
        let schema_arg = if (idx_num & 0x01) != 0 {
            let v = args.get(arg_pos).map(Value::to_text_lossy);
            arg_pos += 1;
            v
        } else {
            self.default_schema.clone()
        };
        let agg_arg = if (idx_num & 0x02) != 0 {
            let v = args.get(arg_pos);
            v.is_some_and(|val| val.truthiness() == Some(true) || val.to_integer_lossy() != 0)
        } else {
            false
        };

        let target_schema = schema_arg.unwrap_or_else(|| ctx.db_name().to_owned());
        // Query an internal helper or inspect via CoreVTabHostContext
        let sql = format!(
            "PRAGMA {}.__sqliters_internal_dbstat({})",
            quote_ident(&target_schema),
            i32::from(agg_arg)
        );
        let raw_rows = ctx.query_sql(&sql, &[])?;
        self.rows = raw_rows
            .into_iter()
            .filter_map(|r| {
                if r.len() < 12 {
                    return None;
                }
                Some(DbStatRow {
                    name: r[0].to_text_lossy(),
                    path: if r[1].is_null() {
                        None
                    } else {
                        Some(r[1].to_text_lossy())
                    },
                    pageno: r[2].to_integer_lossy(),
                    pagetype: if r[3].is_null() {
                        None
                    } else {
                        Some(r[3].to_text_lossy())
                    },
                    ncell: r[4].to_integer_lossy(),
                    payload: r[5].to_integer_lossy(),
                    unused: r[6].to_integer_lossy(),
                    mx_payload: r[7].to_integer_lossy(),
                    pgoffset: if r[8].is_null() {
                        None
                    } else {
                        Some(r[8].to_integer_lossy())
                    },
                    pgsize: r[9].to_integer_lossy(),
                    schema: r[10].to_text_lossy(),
                    aggregate: r[11].to_integer_lossy(),
                })
            })
            .collect();
        self.pos = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(row) = self.rows.get(self.pos) else {
            return Ok(Value::Null);
        };
        let val = match col_idx {
            0 => Value::Text(row.name.clone()),
            1 => row.path.clone().map_or(Value::Null, Value::Text),
            2 => Value::Integer(row.pageno),
            3 => row.pagetype.clone().map_or(Value::Null, Value::Text),
            4 => Value::Integer(row.ncell),
            5 => Value::Integer(row.payload),
            6 => Value::Integer(row.unused),
            7 => Value::Integer(row.mx_payload),
            8 => row.pgoffset.map_or(Value::Null, Value::Integer),
            9 => Value::Integer(row.pgsize),
            10 => Value::Text(row.schema.clone()),
            11 => Value::Integer(row.aggregate),
            _ => Value::Null,
        };
        Ok(val)
    }

    fn rowid(&self) -> Result<i64> {
        Ok((self.pos + 1) as i64)
    }
}

/// Computes `dbstat` rows directly from the B-Tree pages of `db_id` in `db_set`.
pub fn compute_dbstat_rows_for_slot(
    db_set: &mut DatabaseSet,
    db_id: DatabaseId,
    aggregate: bool,
) -> Result<Vec<Vec<Value>>> {
    let schema_name = db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .ok_or_else(|| SqliteError::new(ErrorCode::Error, "unknown database"))?;

    if db_id == DatabaseId::TEMP && db_set.slot(db_id).is_some_and(|s| s.btree.is_none()) {
        return Ok(Vec::new());
    }

    let slot = db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::new(ErrorCode::Error, "unknown database"))?;
    let Some(ref mut btree) = slot.btree else {
        return Ok(Vec::new());
    };
    let _ = slot.schema.ensure_fresh_in_tx_for_schema(btree, &slot.name);

    let page_size = i64::from(btree.pager().page_size());
    let usable_size = i64::from(btree.pager().usable_size());
    let page_count = btree.pager().page_count();

    let mut roots: Vec<(String, Pgno)> = vec![("sqlite_schema".to_owned(), 1)];
    for tbl in &slot.schema.tables {
        if tbl.root_pgno > 1 {
            roots.push((tbl.name.clone(), tbl.root_pgno));
        }
    }
    for idx in &slot.schema.indexes {
        if idx.root_pgno > 1 {
            roots.push((idx.name.clone(), idx.root_pgno));
        }
    }

    let mut page_rows: Vec<DbStatRow> = Vec::new();
    if page_count == 0 {
        page_rows.push(DbStatRow {
            name: "sqlite_schema".to_owned(),
            path: Some("/".to_owned()),
            pageno: 1,
            pagetype: Some("leaf".to_owned()),
            ncell: 0,
            payload: 0,
            unused: (usable_size - 108).max(0),
            mx_payload: 0,
            pgoffset: Some(0),
            pgsize: page_size,
            schema: schema_name.clone(),
            aggregate: i64::from(aggregate),
        });
    } else {
        let need_read_tx = btree.transaction_state() == sqliters_pager::TransactionState::Open;
        if need_read_tx {
            let _ = btree.begin_read();
        }
        let mut visited_pages = HashSet::new();
        for (obj_name, root_pgno) in roots {
            collect_btree_dbstat_pages(
                btree,
                &obj_name,
                root_pgno,
                "/",
                page_size,
                usable_size,
                &schema_name,
                aggregate,
                &mut visited_pages,
                &mut page_rows,
            );
        }
        if need_read_tx {
            let _ = btree.pager_mut().end_read_if_unused();
        }
    }

    page_rows.sort_by_key(|r| r.pageno);

    if aggregate {
        let mut by_name: BTreeMap<String, DbStatRow> = BTreeMap::new();
        for row in page_rows {
            let entry = by_name
                .entry(row.name.clone())
                .or_insert_with(|| DbStatRow {
                    name: row.name.clone(),
                    path: None,
                    pageno: 0,
                    pagetype: None,
                    ncell: 0,
                    payload: 0,
                    unused: 0,
                    mx_payload: 0,
                    pgoffset: None,
                    pgsize: 0,
                    schema: schema_name.clone(),
                    aggregate: 1,
                });
            entry.pageno += 1;
            entry.ncell += row.ncell;
            entry.payload += row.payload;
            entry.unused += row.unused;
            entry.mx_payload = entry.mx_payload.max(row.mx_payload);
            entry.pgsize += row.pgsize;
        }
        return Ok(by_name.into_values().map(dbstat_row_to_values).collect());
    }

    Ok(page_rows.into_iter().map(dbstat_row_to_values).collect())
}

fn dbstat_row_to_values(r: DbStatRow) -> Vec<Value> {
    vec![
        Value::Text(r.name),
        r.path.map_or(Value::Null, Value::Text),
        Value::Integer(r.pageno),
        r.pagetype.map_or(Value::Null, Value::Text),
        Value::Integer(r.ncell),
        Value::Integer(r.payload),
        Value::Integer(r.unused),
        Value::Integer(r.mx_payload),
        r.pgoffset.map_or(Value::Null, Value::Integer),
        Value::Integer(r.pgsize),
        Value::Text(r.schema),
        Value::Integer(r.aggregate),
    ]
}

#[allow(clippy::too_many_arguments)]
fn collect_btree_dbstat_pages(
    btree: &mut sqliters_btree::Btree,
    obj_name: &str,
    pgno: Pgno,
    path: &str,
    page_size: i64,
    usable_size: i64,
    schema_name: &str,
    aggregate: bool,
    visited: &mut HashSet<Pgno>,
    out: &mut Vec<DbStatRow>,
) {
    if pgno == 0 || !visited.insert(pgno) {
        return;
    }
    let Ok(page_buf) = btree.pager_mut().read_page(pgno).map(|p| p.to_vec()) else {
        return;
    };
    let hdr_off = if pgno == 1 { 100usize } else { 0usize };
    if hdr_off + 8 > page_buf.len() {
        return;
    }
    let flags = page_buf[hdr_off];
    let is_interior = matches!(flags, 0x02 | 0x05);
    let is_table = matches!(flags, 0x05 | 0x0d);
    let hdr_size = if is_interior { 12usize } else { 8usize };
    if hdr_off + hdr_size > page_buf.len() {
        return;
    }
    let ncell = usize::from(u16::from_be_bytes([
        page_buf[hdr_off + 3],
        page_buf[hdr_off + 4],
    ]));
    let raw_content_off = u16::from_be_bytes([page_buf[hdr_off + 5], page_buf[hdr_off + 6]]);
    let content_off = if raw_content_off == 0 {
        65536usize
    } else {
        usize::from(raw_content_off)
    };
    let ptr_array_end = hdr_off + hdr_size + ncell * 2;
    let mut unused = content_off.saturating_sub(ptr_array_end) as i64;

    // Add freeblock bytes
    let mut fb_off = usize::from(u16::from_be_bytes([
        page_buf[hdr_off + 1],
        page_buf[hdr_off + 2],
    ]));
    let mut fb_guard = 0usize;
    while fb_off > 0 && fb_off + 4 <= page_buf.len() && fb_guard < 4096 {
        let next_fb = usize::from(u16::from_be_bytes([page_buf[fb_off], page_buf[fb_off + 1]]));
        let fb_sz = usize::from(u16::from_be_bytes([
            page_buf[fb_off + 2],
            page_buf[fb_off + 3],
        ]));
        unused += fb_sz as i64;
        if next_fb <= fb_off {
            break;
        }
        fb_off = next_fb;
        fb_guard += 1;
    }

    let mut page_payload = 0i64;
    let mut mx_payload = 0i64;
    let mut children: Vec<(Pgno, String)> = Vec::new();
    let mut overflow_roots: Vec<(Pgno, i64, String)> = Vec::new();

    let u = usable_size as usize;
    let max_local = if is_table && !is_interior {
        u.saturating_sub(35)
    } else {
        ((u.saturating_sub(12)) * 64 / 255).saturating_sub(23)
    };
    let min_local = ((u.saturating_sub(12)) * 32 / 255).saturating_sub(23);

    for cell_idx in 0..ncell {
        let ptr_pos = hdr_off + hdr_size + cell_idx * 2;
        if ptr_pos + 2 > page_buf.len() {
            break;
        }
        let mut cell_off = usize::from(u16::from_be_bytes([
            page_buf[ptr_pos],
            page_buf[ptr_pos + 1],
        ]));
        if cell_off >= page_buf.len() {
            continue;
        }

        if is_interior {
            if cell_off + 4 > page_buf.len() {
                continue;
            }
            let child_pgno = u32::from_be_bytes([
                page_buf[cell_off],
                page_buf[cell_off + 1],
                page_buf[cell_off + 2],
                page_buf[cell_off + 3],
            ]);
            children.push((child_pgno, format!("{path}{cell_idx:03x}/")));
            cell_off += 4;
        }

        if is_interior && is_table {
            // Interior table cells carry only child_pgno + rowid varint (no payload)
            continue;
        }

        let Ok((payload_sz_u64, n_sz)) = get_varint(&page_buf[cell_off..]) else {
            continue;
        };
        cell_off += n_sz;
        if is_table && !is_interior {
            let Ok((_, n_rowid)) = get_varint(&page_buf[cell_off..]) else {
                continue;
            };
            cell_off += n_rowid;
        }

        let payload_sz = payload_sz_u64 as usize;
        mx_payload = mx_payload.max(payload_sz as i64);
        if payload_sz <= max_local {
            page_payload += payload_sz as i64;
        } else {
            let k = min_local + (payload_sz - min_local) % (u.saturating_sub(4).max(1));
            let local_bytes = if k <= max_local { k } else { min_local };
            page_payload += local_bytes as i64;
            let ovf_ptr_off = cell_off + local_bytes;
            if ovf_ptr_off + 4 <= page_buf.len() {
                let first_ovf = u32::from_be_bytes([
                    page_buf[ovf_ptr_off],
                    page_buf[ovf_ptr_off + 1],
                    page_buf[ovf_ptr_off + 2],
                    page_buf[ovf_ptr_off + 3],
                ]);
                let rem = (payload_sz - local_bytes) as i64;
                overflow_roots.push((first_ovf, rem, format!("{path}{cell_idx:03x}+000000")));
            }
        }
    }

    if is_interior && hdr_off + 12 <= page_buf.len() {
        let right_child = u32::from_be_bytes([
            page_buf[hdr_off + 8],
            page_buf[hdr_off + 9],
            page_buf[hdr_off + 10],
            page_buf[hdr_off + 11],
        ]);
        children.push((right_child, format!("{path}{ncell:03x}/")));
    }

    out.push(DbStatRow {
        name: obj_name.to_owned(),
        path: Some(path.to_owned()),
        pageno: i64::from(pgno),
        pagetype: Some(if is_interior { "internal" } else { "leaf" }.to_owned()),
        ncell: ncell as i64,
        payload: page_payload,
        unused: unused.max(0),
        mx_payload,
        pgoffset: Some((i64::from(pgno) - 1) * page_size),
        pgsize: page_size,
        schema: schema_name.to_owned(),
        aggregate: i64::from(aggregate),
    });

    for (ovf_pgno, mut rem_payload, base_path) in overflow_roots {
        let mut cur_ovf = ovf_pgno;
        let mut chain_idx = 0usize;
        let max_per_ovf = (usable_size - 4).max(1);
        while cur_ovf != 0 && rem_payload > 0 && visited.insert(cur_ovf) {
            let Ok(ovf_buf) = btree.pager_mut().read_page(cur_ovf).map(|p| p.to_vec()) else {
                break;
            };
            if ovf_buf.len() < 4 {
                break;
            }
            let next_ovf = u32::from_be_bytes([ovf_buf[0], ovf_buf[1], ovf_buf[2], ovf_buf[3]]);
            let chunk = rem_payload.min(max_per_ovf);
            let ovf_unused = usable_size - 4 - chunk;
            let ovf_path = if chain_idx == 0 {
                base_path.clone()
            } else {
                format!(
                    "{}+{:06x}",
                    base_path.split('+').next().unwrap_or(&base_path),
                    chain_idx
                )
            };
            out.push(DbStatRow {
                name: obj_name.to_owned(),
                path: Some(ovf_path),
                pageno: i64::from(cur_ovf),
                pagetype: Some("overflow".to_owned()),
                ncell: 0,
                payload: chunk,
                unused: ovf_unused.max(0),
                mx_payload: 0,
                pgoffset: Some((i64::from(cur_ovf) - 1) * page_size),
                pgsize: page_size,
                schema: schema_name.to_owned(),
                aggregate: i64::from(aggregate),
            });
            rem_payload -= chunk;
            cur_ovf = next_ovf;
            chain_idx += 1;
        }
    }

    for (child_pgno, child_path) in children {
        collect_btree_dbstat_pages(
            btree,
            obj_name,
            child_pgno,
            &child_path,
            page_size,
            usable_size,
            schema_name,
            aggregate,
            visited,
            out,
        );
    }
}

// ============================================================================
// Eponymous `pragma_*` Virtual Table Modules (`sqlite/src/pragma.c`)
// ============================================================================

#[derive(Debug, Clone)]
pub struct PragmaVTabModule {
    module_name: String,
    pragma_name: String,
    declare_sql: &'static str,
    visible_cols: usize,
    has_arg: bool,
    has_schema: bool,
    requires_arg: bool,
}

impl PragmaVTabModule {
    /// Checks if `name` is an eponymous `pragma_<name>` module and returns its descriptor.
    pub fn try_for_module_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let pragma_name = lower.strip_prefix("pragma_")?.to_owned();
        let (declare_sql, visible_cols, has_arg, has_schema, requires_arg) = match pragma_name
            .as_str()
        {
            "pragma_list" => ("CREATE TABLE x(name)", 1, false, false, false),
            "function_list" => (
                "CREATE TABLE x(name,builtin,type,enc,narg,flags)",
                6,
                false,
                false,
                false,
            ),
            "module_list" => ("CREATE TABLE x(name)", 1, false, false, false),
            "table_info" => (
                "CREATE TABLE x(cid,name,type,\"notnull\",dflt_value,pk,arg hidden,schema hidden)",
                6,
                true,
                true,
                true,
            ),
            "table_xinfo" => (
                "CREATE TABLE x(cid,name,type,\"notnull\",dflt_value,pk,hidden,arg hidden,schema hidden)",
                7,
                true,
                true,
                true,
            ),
            "index_list" => (
                "CREATE TABLE x(seq,name,\"unique\",origin,partial,arg hidden,schema hidden)",
                5,
                true,
                true,
                true,
            ),
            "index_info" => (
                "CREATE TABLE x(seqno,cid,name,arg hidden,schema hidden)",
                3,
                true,
                true,
                true,
            ),
            "index_xinfo" => (
                "CREATE TABLE x(seqno,cid,name,\"desc\",coll,\"key\",arg hidden,schema hidden)",
                6,
                true,
                true,
                true,
            ),
            "foreign_key_list" => (
                "CREATE TABLE x(id,seq,\"table\",\"from\",\"to\",on_update,on_delete,\"match\",arg hidden,schema hidden)",
                8,
                true,
                true,
                true,
            ),
            "database_list" => ("CREATE TABLE x(seq,name,file)", 3, false, false, false),
            "collation_list" => ("CREATE TABLE x(seq,name)", 2, false, false, false),
            "compile_options" => ("CREATE TABLE x(compile_options)", 1, false, false, false),
            "table_list" => (
                "CREATE TABLE x(schema,name,type,ncol,wr,strict,arg hidden)",
                6,
                true,
                false,
                false,
            ),
            "integrity_check" => (
                "CREATE TABLE x(integrity_check,arg hidden,schema hidden)",
                1,
                true,
                true,
                false,
            ),
            "quick_check" => (
                "CREATE TABLE x(quick_check,arg hidden,schema hidden)",
                1,
                true,
                true,
                false,
            ),
            "foreign_key_check" => (
                "CREATE TABLE x(\"table\",rowid,parent,fkid,arg hidden,schema hidden)",
                4,
                true,
                true,
                false,
            ),
            _ => return None,
        };
        Some(Self {
            module_name: lower,
            pragma_name,
            declare_sql,
            visible_cols,
            has_arg,
            has_schema,
            requires_arg,
        })
    }
}

impl VirtualTableModule for PragmaVTabModule {
    fn name(&self) -> &str {
        &self.module_name
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Innocuous
    }

    fn is_eponymous(&self) -> bool {
        true
    }

    fn is_eponymous_only(&self) -> bool {
        true
    }

    fn create(
        &self,
        _ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such module: {}", self.module_name),
        ))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        ctx.declare_vtab(self.declare_sql)?;
        let _ = ctx.set_vtab_config(VTabConfigOption::Innocuous);
        Ok(Box::new(PragmaVTabTable { spec: self.clone() }))
    }
}

#[derive(Debug)]
struct PragmaVTabTable {
    spec: PragmaVTabModule,
}

impl VirtualTable for PragmaVTabTable {
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let arg_col = self.spec.visible_cols as i32;
        let schema_col = (self.spec.visible_cols + 1) as i32;
        let mut idx_num = 0i32;
        let mut arg_ci = None;
        let mut schema_ci = None;

        for (i, c) in info.constraints.iter().enumerate() {
            if self.spec.has_arg && c.i_column == arg_col {
                if !c.usable {
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable pragma arg constraint",
                    ));
                }
                if c.op == VTabConstraintOp::Eq {
                    arg_ci = Some(i);
                    idx_num |= 0x01;
                }
            } else if self.spec.has_schema && c.i_column == schema_col {
                if !c.usable {
                    return Err(SqliteError::new(
                        ErrorCode::Constraint,
                        "unusable pragma schema constraint",
                    ));
                }
                if c.op == VTabConstraintOp::Eq {
                    schema_ci = Some(i);
                    idx_num |= 0x02;
                }
            }
        }

        let mut argv_pos = 0usize;
        if let Some(ci) = arg_ci {
            argv_pos += 1;
            info.constraint_usage[ci].argv_index = argv_pos;
            info.constraint_usage[ci].omit = true;
        }
        if let Some(ci) = schema_ci {
            argv_pos += 1;
            info.constraint_usage[ci].argv_index = argv_pos;
            info.constraint_usage[ci].omit = true;
        }

        info.idx_num = idx_num;
        info.estimated_cost = 10.0;
        info.estimated_rows = 10;
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(PragmaVTabCursor {
            spec: self.spec.clone(),
            rows: Vec::new(),
            pos: 0,
            arg_val: Value::Null,
            schema_val: Value::Null,
        }))
    }
}

#[derive(Debug)]
struct PragmaVTabCursor {
    spec: PragmaVTabModule,
    rows: Vec<Vec<Value>>,
    pos: usize,
    arg_val: Value,
    schema_val: Value,
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_string_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl VirtualCursor for PragmaVTabCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        self.rows.clear();
        self.pos = 0;
        self.arg_val = Value::Null;
        self.schema_val = Value::Null;

        let mut arg_pos = 0usize;
        if (idx_num & 0x01) != 0 {
            self.arg_val = args.get(arg_pos).cloned().unwrap_or(Value::Null);
            arg_pos += 1;
        }
        if (idx_num & 0x02) != 0 {
            self.schema_val = args.get(arg_pos).cloned().unwrap_or(Value::Null);
        }

        if self.spec.requires_arg && self.arg_val.is_null() {
            return Ok(());
        }

        let schema_prefix = if !self.schema_val.is_null() {
            format!("{}.", quote_ident(&self.schema_val.to_text_lossy()))
        } else {
            String::new()
        };

        let pragma_sql = if !self.arg_val.is_null() {
            format!(
                "PRAGMA {schema_prefix}{}({})",
                self.spec.pragma_name,
                quote_string_literal(&self.arg_val.to_text_lossy())
            )
        } else {
            format!("PRAGMA {schema_prefix}{}", self.spec.pragma_name)
        };

        self.rows = ctx.query_sql(&pragma_sql, &[])?;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        if self.pos < self.rows.len() {
            self.pos += 1;
        }
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, col_idx: usize) -> Result<Value> {
        let Some(row) = self.rows.get(self.pos) else {
            return Ok(Value::Null);
        };
        if col_idx < self.spec.visible_cols {
            return Ok(row.get(col_idx).cloned().unwrap_or(Value::Null));
        }
        if self.spec.has_arg && col_idx == self.spec.visible_cols {
            return Ok(self.arg_val.clone());
        }
        if self.spec.has_schema && col_idx == self.spec.visible_cols + 1 {
            return Ok(self.schema_val.clone());
        }
        Ok(Value::Null)
    }

    fn rowid(&self) -> Result<i64> {
        Ok((self.pos + 1) as i64)
    }
}

/// Complete sorted list of 66 SQLite 3.54.1 pragma names (`PRAGMA pragma_list`).
pub const SQLITE_PRAGMA_LIST: &[&str] = &[
    "analysis_limit",
    "application_id",
    "auto_vacuum",
    "automatic_index",
    "busy_timeout",
    "cache_size",
    "cache_spill",
    "case_sensitive_like",
    "cell_size_check",
    "checkpoint_fullfsync",
    "collation_list",
    "compile_options",
    "count_changes",
    "data_version",
    "database_list",
    "default_cache_size",
    "defer_foreign_keys",
    "empty_result_callbacks",
    "encoding",
    "foreign_key_check",
    "foreign_key_list",
    "foreign_keys",
    "freelist_count",
    "full_column_names",
    "fullfsync",
    "function_list",
    "hard_heap_limit",
    "ignore_check_constraints",
    "incremental_vacuum",
    "index_info",
    "index_list",
    "index_xinfo",
    "integrity_check",
    "journal_mode",
    "journal_size_limit",
    "legacy_alter_table",
    "locking_mode",
    "max_page_count",
    "mmap_size",
    "module_list",
    "optimize",
    "page_count",
    "page_size",
    "pragma_list",
    "query_only",
    "quick_check",
    "read_uncommitted",
    "recursive_triggers",
    "reverse_unordered_selects",
    "schema_version",
    "secure_delete",
    "short_column_names",
    "shrink_memory",
    "soft_heap_limit",
    "synchronous",
    "table_info",
    "table_list",
    "table_xinfo",
    "temp_store",
    "temp_store_directory",
    "threads",
    "trusted_schema",
    "user_version",
    "wal_autocheckpoint",
    "wal_checkpoint",
    "writable_schema",
];

/// Built-in SQLite 3.54.1 functions reported by `PRAGMA function_list`: `(name, type, narg, flags)`.
pub const SQLITE_BUILTIN_FUNCTION_LIST: &[(&str, &str, i64, i64)] = &[
    ("->", "s", 2, 2099200),
    ("->>", "s", 2, 2099200),
    ("abs", "s", 1, 2099200),
    ("avg", "w", 1, 2097152),
    ("changes", "s", 0, 2097152),
    ("char", "s", -1, 2099200),
    ("coalesce", "s", -4, 2099200),
    ("concat", "s", -3, 2099200),
    ("concat_ws", "s", -4, 2099200),
    ("count", "w", 0, 2097152),
    ("count", "w", 1, 2097152),
    ("cume_dist", "w", 0, 2097152),
    ("current_date", "s", 0, 2097152),
    ("current_time", "s", 0, 2097152),
    ("current_timestamp", "s", 0, 2097152),
    ("date", "s", -1, 2099200),
    ("datetime", "s", -1, 2099200),
    ("dense_rank", "w", 0, 2097152),
    ("first_value", "w", 1, 2097152),
    ("format", "s", -1, 2099200),
    ("glob", "s", 2, 2099200),
    ("group_concat", "w", 1, 2097152),
    ("group_concat", "w", 2, 2097152),
    ("hex", "s", 1, 2099200),
    ("if", "s", -4, 2099200),
    ("ifnull", "s", 2, 2099200),
    ("iif", "s", -4, 2099200),
    ("instr", "s", 2, 2099200),
    ("json", "s", 1, 2099200),
    ("json_array", "s", -1, 3147776),
    ("json_array_insert", "s", -1, 3147776),
    ("json_array_length", "s", 1, 2099200),
    ("json_array_length", "s", 2, 2099200),
    ("json_error_position", "s", 1, 2099200),
    ("json_extract", "s", -1, 2099200),
    ("json_group_array", "w", 1, 3147776),
    ("json_group_object", "w", 2, 3147776),
    ("json_insert", "s", -1, 3147776),
    ("json_object", "s", -1, 3147776),
    ("json_patch", "s", 2, 2099200),
    ("json_pretty", "s", 1, 2099200),
    ("json_pretty", "s", 2, 2099200),
    ("json_quote", "s", 1, 3147776),
    ("json_remove", "s", -1, 2099200),
    ("json_replace", "s", -1, 3147776),
    ("json_set", "s", -1, 3147776),
    ("json_type", "s", 1, 2099200),
    ("json_type", "s", 2, 2099200),
    ("json_valid", "s", 1, 2099200),
    ("json_valid", "s", 2, 2099200),
    ("jsonb", "s", 1, 2099200),
    ("jsonb_array", "s", -1, 3147776),
    ("jsonb_array_insert", "s", -1, 3147776),
    ("jsonb_extract", "s", -1, 2099200),
    ("jsonb_group_array", "w", 1, 3147776),
    ("jsonb_group_object", "w", 2, 3147776),
    ("jsonb_insert", "s", -1, 3147776),
    ("jsonb_object", "s", -1, 3147776),
    ("jsonb_patch", "s", 2, 2099200),
    ("jsonb_remove", "s", -1, 2099200),
    ("jsonb_replace", "s", -1, 3147776),
    ("jsonb_set", "s", -1, 3147776),
    ("julianday", "s", -1, 2099200),
    ("lag", "w", 1, 2097152),
    ("lag", "w", 2, 2097152),
    ("lag", "w", 3, 2097152),
    ("last_insert_rowid", "s", 0, 2097152),
    ("last_value", "w", 1, 2097152),
    ("lead", "w", 1, 2097152),
    ("lead", "w", 2, 2097152),
    ("lead", "w", 3, 2097152),
    ("length", "s", 1, 2099200),
    ("like", "s", 2, 2099200),
    ("like", "s", 3, 2099200),
    ("likelihood", "s", 2, 2099200),
    ("likely", "s", 1, 2099200),
    ("load_extension", "s", 1, 524288),
    ("load_extension", "s", 2, 524288),
    ("lower", "s", 1, 2099200),
    ("ltrim", "s", 1, 2099200),
    ("ltrim", "s", 2, 2099200),
    ("max", "s", -3, 2099200),
    ("max", "w", 1, 2097152),
    ("min", "s", -3, 2099200),
    ("min", "w", 1, 2097152),
    ("nth_value", "w", 2, 2097152),
    ("ntile", "w", 1, 2097152),
    ("nullif", "s", 2, 2099200),
    ("octet_length", "s", 1, 2099200),
    ("percent_rank", "w", 0, 2097152),
    ("printf", "s", -1, 2099200),
    ("quote", "s", 1, 2099200),
    ("random", "s", 0, 2097152),
    ("randomblob", "s", 1, 2097152),
    ("rank", "w", 0, 2097152),
    ("replace", "s", 3, 2099200),
    ("round", "s", 1, 2099200),
    ("round", "s", 2, 2099200),
    ("row_number", "w", 0, 2097152),
    ("rtrim", "s", 1, 2099200),
    ("rtrim", "s", 2, 2099200),
    ("sign", "s", 1, 2099200),
    ("sqlite_compileoption_get", "s", 1, 2097152),
    ("sqlite_compileoption_used", "s", 1, 2097152),
    ("sqlite_log", "s", 2, 524288),
    ("sqlite_source_id", "s", 0, 2097152),
    ("sqlite_version", "s", 0, 2097152),
    ("strftime", "s", -1, 2099200),
    ("string_agg", "w", 2, 2097152),
    ("substr", "s", 2, 2099200),
    ("substr", "s", 3, 2099200),
    ("substring", "s", 2, 2099200),
    ("substring", "s", 3, 2099200),
    ("subtype", "s", 1, 3147776),
    ("sum", "w", 1, 2097152),
    ("time", "s", -1, 2099200),
    ("timediff", "s", 2, 2099200),
    ("total", "w", 1, 2097152),
    ("total_changes", "s", 0, 2097152),
    ("trim", "s", 1, 2099200),
    ("trim", "s", 2, 2099200),
    ("typeof", "s", 1, 2099200),
    ("unhex", "s", 1, 2099200),
    ("unhex", "s", 2, 2099200),
    ("unicode", "s", 1, 2099200),
    ("unistr", "s", 1, 2099200),
    ("unistr_quote", "s", 1, 2099200),
    ("unixepoch", "s", -1, 2099200),
    ("unlikely", "s", 1, 2099200),
    ("upper", "s", 1, 2099200),
    ("zeroblob", "s", 1, 2099200),
];
