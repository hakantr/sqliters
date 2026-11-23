//! Virtual table host context, module/instance registry, lifecycle hooks, `dbstat`,
//! and eponymous `pragma_*` virtual table modules (`sqlite/src/vtab.c`, `pragma.c`, `dbstat.c`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use sqliters_ext::{
    Fts5AuxRegistry, Fts5Module, Fts5VocabModule, JsonEachModule,
    JsonTreeModule, JsonbEachModule, JsonbTreeModule, RTreeModule,
};
use sqliters_syntax::{
    Expr, FromClause, FunctionArgs, InsertSource, JoinConstraint, PragmaValue, QualifiedName,
    ResultColumn, SelectCore, SelectStmt, Stmt, TableOrSubquery, TriggerStep, WindowRef, dequote,
    parse_one,
};
use sqliters_types::{
    DatabaseId, ErrorCode, Pgno, Result, SqliteError, VTabConfigOption, VTabConflictMode,
    VTabConstraintOp, VTabHostContext, VTabIndexInfo, VTabRiskFlags, VTabUpdateOperation, Value,
    VirtualCursor, VirtualTable, VirtualTableModule, get_varint,
};

use crate::db_set::{DatabaseSet, RelationRef};
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
    /// The module implements xFindFunction (FTS5's auxiliary functions).
    pub find_function: bool,
    /// `VTabConfigOption::NochangeUnread`.
    pub nochange_unread: bool,
    /// The text the constructor passed to `sqlite3_declare_vtab()` (read by the port of C's
    /// code generator, which builds its own Table object from it).
    pub declare_sql: String,
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

/// Strips the first `HIDDEN` keyword token from a column's `type_name`, returning `(cleaned_type_name, is_hidden)`.
pub fn strip_hidden_from_type_name(type_name: &str) -> (String, bool) {
    let mut is_hidden = false;
    let mut kept = Vec::new();
    for word in type_name.split_whitespace() {
        if !is_hidden && word.eq_ignore_ascii_case("hidden") {
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
    if create_table_sql.trim().is_empty() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "vtable constructor did not declare schema",
        ));
    }
    let parsed = parse_one(create_table_sql)
        .map_err(|_| SqliteError::new(ErrorCode::Error, "syntax error"))?;
    let mut ct = match parsed {
        (Some(Stmt::CreateTable(ct)), tail) => {
            if !tail.trim().is_empty() {
                return Err(SqliteError::new(ErrorCode::Error, "syntax error"));
            }
            ct
        }
        (Some(Stmt::CreateTableAs { .. }), _) => {
            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
        }
        _ => return Err(SqliteError::new(ErrorCode::Error, "syntax error")),
    };
    let mut hidden_cols = Vec::with_capacity(ct.columns.len());
    for cdef in &mut ct.columns {
        let raw_trimmed = cdef.type_name.trim();
        let pre_std = match raw_trimmed.to_ascii_uppercase().as_str() {
            "TEXT" | "INT" | "INTEGER" | "REAL" | "BLOB" | "NUM" => {
                raw_trimmed.to_ascii_uppercase()
            }
            _ => cdef.type_name.clone(),
        };
        let (cleaned, is_hidden) = strip_hidden_from_type_name(&pre_std);
        cdef.type_name = cleaned;
        hidden_cols.push(is_hidden);
    }
    let (mut tbl, _) = build_table_schema_from_ast(&ct, 0)?;
    // The standard-type spelling was decided above on the type as declared (before the
    // HIDDEN keyword was removed): "b integer hidden" stays "integer", like in C.
    for (col, cdef) in tbl.columns.iter_mut().zip(&ct.columns) {
        col.type_name.clone_from(&cdef.type_name);
    }
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
    pub is_eponymous: bool,
    pub is_eponymous_only: bool,
    /// The module has no `xUpdate` (`VirtualTableModule::is_read_only`).
    pub is_read_only: bool,
    pub shadow_suffixes: Vec<String>,
    pub declared: Arc<VTabDeclaredSchema>,
    pub state: VTabInstanceState,
    pub in_tx: bool,
    /// Position in the order in which tables joined the current transaction (assigned when
    /// `in_tx` is set; meaningful only while it is). Transaction callbacks fan out in this
    /// order, as C SQLite walks `db->aVTrans`.
    pub tx_seq: u64,
}

impl ConnectedVTabEntry {
    #[inline]
    pub fn effective_risk_flags(&self) -> VTabRiskFlags {
        self.declared
            .config_risk_flags
            .unwrap_or(self.module_risk_flags)
    }
}

/// ASCII-case-insensitive name comparison that first tries the names as they are (the usual
/// case: the caller hands back the spelling the registry gave it).
#[inline(always)]
fn name_eq_ignore_case(a: &str, b: &str) -> bool {
    a == b || a.eq_ignore_ascii_case(b)
}

const MAX_SUB_SQL_CACHE_ENTRIES: usize = 128;

/// Up to this many distinct connected table names, [`VTabRegistry`] lookups scan the keys
/// instead of hashing the requested name.
const SMALL_REGISTRY_SCAN_LIMIT: usize = 4;

/// Cached parse outcome for internal `VTabHostContext` sub-SQL.
pub enum CachedSubSql {
    Empty,
    Single(Arc<Stmt>),
    Multi,
}

struct RegisteredModule {
    name: String,
    implementation: Arc<dyn VirtualTableModule>,
    automatic_pragma: bool,
}

/// Per-connection registry of virtual table modules and connected virtual table instances.
pub struct VTabRegistry {
    modules: BTreeMap<String, RegisteredModule>,
    module_order: sqliters_codegen::trigger::CHash<()>,
    connected: HashMap<String, Vec<ConnectedVTabEntry>>,
    sub_sql_cache: HashMap<String, Arc<Stmt>>,
    fts5_aux: Arc<Fts5AuxRegistry>,
    /// Recycled name buffer for [`Self::take_active_tx_vtabs`] (carries no state).
    tx_names_scratch: Vec<(u64, String, String)>,
    /// Next [`ConnectedVTabEntry::tx_seq`] to hand out.
    next_tx_seq: u64,
    pub trusted_schema: bool,
    pub defensive_mode: bool,
    /// The `SQLITE_WriteSchema` flag bit (see `DatabaseSet::writable_schema_flag`).
    pub writable_schema_flag: bool,
    pub internal_sql_depth: usize,
    /// Recycled record buffer of the direct shadow-table writes (carries no state).
    pub shadow_scratch: Vec<u8>,
}

impl std::fmt::Debug for VTabRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VTabRegistry")
            .field("module_count", &self.modules.len())
            .field("connected", &self.connected)
            .field("sub_sql_cache_len", &self.sub_sql_cache.len())
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
    /// Registers the virtual tables from SQLite's built-in extension initializers.
    pub fn new() -> Self {
        let fts5_aux = Arc::new(Fts5AuxRegistry::new());
        let mut reg = Self {
            modules: BTreeMap::new(),
            module_order: Default::default(),
            connected: HashMap::new(),
            sub_sql_cache: HashMap::new(),
            fts5_aux: Arc::clone(&fts5_aux),
            tx_names_scratch: Vec::new(),
            next_tx_seq: 0,
            trusted_schema: true,
            defensive_mode: false,
            writable_schema_flag: false,
            internal_sql_depth: 0,
            shadow_scratch: Vec::new(),
        };
        reg.register_module(Arc::new(Fts5Module::new(Arc::clone(&fts5_aux))));
        reg.register_module(Arc::new(Fts5VocabModule::new(fts5_aux)));
        reg.register_module(Arc::new(RTreeModule::new_f32()));
        reg.register_module(Arc::new(RTreeModule::new_i32()));
        reg.register_module(Arc::new(DbStatModule));
        reg
    }

    /// Returns the connection-scoped FTS5 auxiliary function registry (`bm25`, `highlight`, `snippet`).
    #[inline]
    pub fn fts5_aux_registry(&self) -> Arc<Fts5AuxRegistry> {
        Arc::clone(&self.fts5_aux)
    }

    /// Registers a virtual table module on the connection.
    pub fn register_module(&mut self, module: Arc<dyn VirtualTableModule>) {
        let name = module.name().to_owned();
        self.register_module_named(name, module, false);
    }

    fn register_module_named(
        &mut self,
        name: String,
        module: Arc<dyn VirtualTableModule>,
        automatic_pragma: bool,
    ) {
        self.module_order.insert(&name, ());
        self.modules.insert(name.to_ascii_lowercase(), RegisteredModule {
            name,
            implementation: module,
            automatic_pragma,
        });
    }

    /// Unregisters a virtual table module by name (`sqlite3_create_module_v2` with `pModule == NULL`).
    pub fn unregister_module(&mut self, name: &str) -> bool {
        self.module_order.remove(name);
        self.modules.remove(&name.to_ascii_lowercase()).is_some()
    }

    /// The eponymous module that sqlite3LocateTable can register by name.
    pub(crate) fn lazy_module(name: &str) -> Option<(String, Arc<dyn VirtualTableModule>)> {
        let module: Arc<dyn VirtualTableModule> = match name.to_ascii_lowercase().as_str() {
            "json_each" => Arc::new(JsonEachModule),
            "json_tree" => Arc::new(JsonTreeModule),
            "jsonb_each" => Arc::new(JsonbEachModule),
            "jsonb_tree" => Arc::new(JsonbTreeModule),
            _ => {
                let pragma = PragmaVTabModule::try_for_module_name(name)?;
                return Some((name.to_owned(), Arc::new(pragma)));
            }
        };
        Some((module.name().to_owned(), module))
    }

    /// sqlite3LocateTable may register a built-in eponymous module on first resolution.
    pub fn get_or_register_module(&mut self, name: &str) -> Option<Arc<dyn VirtualTableModule>> {
        if let Some(module) = self.get_module(name) {
            return Some(module);
        }
        let (name, module) = Self::lazy_module(name)?;
        let automatic_pragma = name.get(..7).is_some_and(|prefix| prefix.eq_ignore_ascii_case("pragma_"));
        self.register_module_named(name, Arc::clone(&module), automatic_pragma);
        Some(module)
    }

    pub(crate) fn module_names_in_hash_order(&self) -> Vec<String> {
        self.module_order.keys().iter()
            .filter_map(|key| self.registered_module_name(key).map(str::to_owned))
            .collect()
    }

    /// The name module `name` was registered with (`Module.zName`), if it is registered.
    pub fn registered_module_name(&self, name: &str) -> Option<&str> {
        self.modules.get(&name.to_ascii_lowercase()).map(|module| module.name.as_str())
    }

    /// Looks up an already-registered module without lazy `pragma_*` registration.
    pub fn get_module(&self, name: &str) -> Option<Arc<dyn VirtualTableModule>> {
        if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            return self.modules.get(name).map(|module| Arc::clone(&module.implementation));
        }
        let key = name.to_ascii_lowercase();
        self.modules.get(&key).map(|module| Arc::clone(&module.implementation))
    }

    pub(crate) fn has_automatic_pragma_module(&self, name: &str) -> bool {
        self.modules.get(&name.to_ascii_lowercase())
            .is_some_and(|module| module.automatic_pragma)
    }

    /// Returns the sorted list of currently registered module names (`PRAGMA module_list`).
    pub fn module_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.modules.values().map(|module| module.name.clone()).collect();
        names.sort();
        names
    }

    /// Alias for [`Self::module_names`].
    #[inline]
    pub fn module_names_sorted(&self) -> Vec<String> {
        self.module_names()
    }

    #[inline]
    fn get_bucket(&self, table_name: &str) -> Option<&Vec<ConnectedVTabEntry>> {
        if self.connected.is_empty() {
            return None;
        }
        // Keys are ASCII-lowercased table names: with only a few connected tables, comparing
        // them directly is cheaper than case-folding and hashing the name on every lookup.
        if self.connected.len() <= SMALL_REGISTRY_SCAN_LIMIT {
            return self
                .connected
                .iter()
                .find(|(key, _)| name_eq_ignore_case(key, table_name))
                .map(|(_, bucket)| bucket);
        }
        if !table_name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.connected.get(table_name)
        } else {
            let key = table_name.to_ascii_lowercase();
            self.connected.get(key.as_str())
        }
    }

    #[inline]
    fn get_bucket_mut(&mut self, table_name: &str) -> Option<&mut Vec<ConnectedVTabEntry>> {
        if self.connected.is_empty() {
            return None;
        }
        if self.connected.len() <= SMALL_REGISTRY_SCAN_LIMIT {
            return self
                .connected
                .iter_mut()
                .find(|(key, _)| name_eq_ignore_case(key, table_name))
                .map(|(_, bucket)| bucket);
        }
        if !table_name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.connected.get_mut(table_name)
        } else {
            let key = table_name.to_ascii_lowercase();
            self.connected.get_mut(key.as_str())
        }
    }

    /// The entry the port of C's code generator named by its address (`P4_VTAB` operand id,
    /// see `cgen.rs` `vtab_connect`), else the first connected table of that name.
    pub fn get_connected_by_id(&self, table_name: &str, id: u64) -> Option<&ConnectedVTabEntry> {
        let bucket = self.get_bucket(table_name)?;
        bucket
            .iter()
            .find(|e| (*e as *const ConnectedVTabEntry) as usize as u64 == id)
            .or_else(|| bucket.iter().find(|e| name_eq_ignore_case(&e.table_name, table_name)))
    }

    /// Returns a reference to a connected virtual table entry if present.
    #[inline]
    pub fn get_connected(&self, db_name: &str, table_name: &str) -> Option<&ConnectedVTabEntry> {
        self.get_bucket(table_name)?
            .iter()
            .find(|e| name_eq_ignore_case(&e.db_name, db_name))
    }

    /// The connected eponymous table `table_name` of any database (the engine connects an
    /// eponymous table under the schema a statement names, `temp.pragma_table_info(...)`; C
    /// keeps one eponymous table per module, in main).
    pub fn get_connected_eponymous(&self, table_name: &str) -> Option<&ConnectedVTabEntry> {
        self.get_bucket(table_name)?.iter().find(|e| e.is_eponymous)
    }

    /// Returns `true` if a connected virtual table belongs to an eponymous-only read-only module.
    #[inline]
    pub fn is_connected_read_only(&self, db_name: &str, table_name: &str) -> bool {
        self.get_connected(db_name, table_name)
            .is_some_and(|entry| entry.is_eponymous_only || entry.is_read_only)
    }

    /// Returns a mutable reference to a connected virtual table entry if present.
    #[inline]
    pub fn get_connected_mut(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Option<&mut ConnectedVTabEntry> {
        self.get_bucket_mut(table_name)?
            .iter_mut()
            .find(|e| name_eq_ignore_case(&e.db_name, db_name))
    }

    /// Inserts a newly created or connected virtual table entry.
    pub fn insert_connected(&mut self, entry: ConnectedVTabEntry) {
        let key = entry.table_name.to_ascii_lowercase();
        let bucket = self.connected.entry(key).or_default();
        if let Some(slot) = bucket
            .iter_mut()
            .find(|e| e.db_name.eq_ignore_ascii_case(&entry.db_name))
        {
            *slot = entry;
        } else {
            bucket.push(entry);
        }
    }

    /// Removes and returns a connected virtual table entry.
    pub fn remove_connected(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Option<ConnectedVTabEntry> {
        let key = table_name.to_ascii_lowercase();
        let bucket = self.connected.get_mut(&key)?;
        let pos = bucket
            .iter()
            .position(|e| e.db_name.eq_ignore_ascii_case(db_name))?;
        let removed = bucket.swap_remove(pos);
        if bucket.is_empty() {
            self.connected.remove(&key);
        }
        Some(removed)
    }

    /// Renames a connected virtual table entry from `old_name` to `new_name` in `db_name`.
    pub fn rename_connected(&mut self, db_name: &str, old_name: &str, new_name: &str) {
        if let Some(mut entry) = self.remove_connected(db_name, old_name) {
            entry.table_name = new_name.to_owned();
            let decl = Arc::make_mut(&mut entry.declared);
            decl.table_schema.name = new_name.to_owned();
            // C reconnects the table after a rename and the module declares its schema
            // again: a hidden column named after the table (FTS5: `ft MATCH ...`) carries the
            // new name from then on.
            let mut renamed_hidden = false;
            for (i, col) in decl.table_schema.columns.iter_mut().enumerate() {
                if decl.hidden_cols.get(i).copied().unwrap_or(false)
                    && col.name.eq_ignore_ascii_case(old_name)
                {
                    col.name = new_name.to_owned();
                    renamed_hidden = true;
                }
            }
            // The text the port of C's code generator builds its Table object from: the
            // hidden column `"<old>" HIDDEN` the module declared is `"<new>" HIDDEN` now.
            if renamed_hidden {
                let quote = |n: &str| format!("\"{}\" HIDDEN", n.replace('"', "\"\""));
                let (from, to) = (quote(old_name).to_ascii_lowercase(), quote(new_name));
                if let Some(pos) = decl.declare_sql.to_ascii_lowercase().find(&from) {
                    decl.declare_sql.replace_range(pos..pos + from.len(), &to);
                }
            }
            self.insert_connected(entry);
        }
    }

    /// Invalidates shadow-table caches across all currently available virtual table instances.
    pub fn invalidate_all_shadow_caches(&mut self) {
        for bucket in self.connected.values_mut() {
            for entry in bucket.iter_mut() {
                if let VTabInstanceState::Available(ref mut vt) = entry.state {
                    vt.invalidate_shadow_cache();
                }
            }
        }
    }

    /// Disconnects all connected virtual tables belonging to `db_name` (e.g. on `DETACH` or schema reload).
    pub fn disconnect_schema(&mut self, db_name: &str) {
        let mut removed_entries = Vec::new();
        self.connected.retain(|_, bucket| {
            let mut i = 0;
            while i < bucket.len() {
                if bucket[i].db_name.eq_ignore_ascii_case(db_name) {
                    removed_entries.push(bucket.swap_remove(i));
                } else {
                    i += 1;
                }
            }
            !bucket.is_empty()
        });
        for mut entry in removed_entries {
            if let VTabInstanceState::Available(ref mut vt) = entry.state {
                let _ = vt.disconnect();
            }
        }
    }

    /// Disconnects all connected virtual tables across all schemas (`sqlite3_close`).
    pub fn disconnect_all(&mut self) {
        let drained: Vec<Vec<ConnectedVTabEntry>> =
            self.connected.drain().map(|(_, b)| b).collect();
        for bucket in drained {
            for mut entry in bucket {
                if let VTabInstanceState::Available(ref mut vt) = entry.state {
                    let _ = vt.disconnect();
                }
            }
        }
    }

    /// Checks out only the `Box<dyn VirtualTable>` from `(db_name, table_name)` with zero allocations,
    /// marking the slot `Busy`.
    #[inline]
    pub fn checkout_vtab_instance(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Result<Box<dyn VirtualTable>> {
        let entry = self.get_connected_mut(db_name, table_name).ok_or_else(|| {
            SqliteError::new(ErrorCode::Error, format!("no such table: {table_name}"))
        })?;
        let prev = std::mem::replace(&mut entry.state, VTabInstanceState::Busy);
        match prev {
            VTabInstanceState::Available(vt) => Ok(vt),
            VTabInstanceState::Busy => Err(SqliteError::new(
                ErrorCode::Locked,
                format!("virtual table \"{table_name}\" is locked"),
            )),
        }
    }

    /// Checks out the `Box<dyn VirtualTable>` from `(db_name, table_name)`, marking the slot `Busy`.
    /// Returns an error if the virtual table is already checked out (`Busy`) to prevent recursive aliasing.
    pub fn checkout_vtab(
        &mut self,
        db_name: &str,
        table_name: &str,
    ) -> Result<(
        Box<dyn VirtualTable>,
        Arc<VTabDeclaredSchema>,
        VTabRiskFlags,
    )> {
        let entry = self.get_connected_mut(db_name, table_name).ok_or_else(|| {
            SqliteError::new(ErrorCode::Error, format!("no such table: {table_name}"))
        })?;
        let prev = std::mem::replace(&mut entry.state, VTabInstanceState::Busy);
        match prev {
            VTabInstanceState::Available(vt) => {
                let decl = Arc::clone(&entry.declared);
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
    #[inline]
    pub fn restore_vtab(&mut self, db_name: &str, table_name: &str, vt: Box<dyn VirtualTable>) {
        if let Some(entry) = self.get_connected_mut(db_name, table_name) {
            entry.state = VTabInstanceState::Available(vt);
        }
    }

    /// Returns `true` if any connected virtual table is currently participating in a transaction (`in_tx`).
    #[inline]
    pub fn has_any_in_tx(&self) -> bool {
        !self.connected.is_empty() && self.connected.values().flatten().any(|e| e.in_tx)
    }

    /// Marks `(db_name, table_name)` as participating in the current transaction, after the
    /// tables that joined it earlier.
    pub fn mark_joined_tx(&mut self, db_name: &str, table_name: &str) {
        let seq = self.next_tx_seq;
        if let Some(entry) = self.get_connected_mut(db_name, table_name) {
            entry.in_tx = true;
            entry.tx_seq = seq;
            self.next_tx_seq = seq + 1;
        }
    }

    /// Returns all `(db_name, table_name)` keys currently marked `in_tx`, in the order in which
    /// the tables joined the transaction (never in the map's hash order, which differs from
    /// process to process and would make the order of shadow-table writes, and with it the
    /// page layout of the database file, random).
    pub fn active_tx_vtabs(&self) -> Vec<(String, String)> {
        let mut active: Vec<&ConnectedVTabEntry> = self
            .connected
            .values()
            .flatten()
            .filter(|e| e.in_tx)
            .collect();
        active.sort_unstable_by_key(|e| e.tx_seq);
        active
            .into_iter()
            .map(|e| (e.db_name.clone(), e.table_name.clone()))
            .collect()
    }

    /// Same tables in the same order as [`Self::active_tx_vtabs`], as `(tx_seq, db_name,
    /// table_name)` written into a buffer recycled between calls so the per-statement savepoint
    /// fan-out does not allocate. Hand the vector back with [`Self::recycle_tx_vtab_names`]
    /// when done.
    pub fn take_active_tx_vtabs(&mut self) -> Vec<(u64, String, String)> {
        let mut out = std::mem::take(&mut self.tx_names_scratch);
        let mut n = 0usize;
        for e in self.connected.values().flatten().filter(|e| e.in_tx) {
            if let Some((seq, db_name, table_name)) = out.get_mut(n) {
                *seq = e.tx_seq;
                db_name.clear();
                db_name.push_str(&e.db_name);
                table_name.clear();
                table_name.push_str(&e.table_name);
            } else {
                out.push((e.tx_seq, e.db_name.clone(), e.table_name.clone()));
            }
            n += 1;
        }
        out.truncate(n);
        if n > 1 {
            out.sort_unstable_by_key(|(seq, _, _)| *seq);
        }
        out
    }

    /// Returns a vector obtained from [`Self::take_active_tx_vtabs`] for reuse.
    #[inline]
    pub fn recycle_tx_vtab_names(&mut self, names: Vec<(u64, String, String)>) {
        self.tx_names_scratch = names;
    }

    /// Looks up a cached single-statement AST for internal `VTabHostContext` sub-SQL, or parses and
    /// caches it when `sql` consists of a single statement with no trailing statements.
    pub fn get_or_parse_single_sub_sql(&mut self, sql: &str) -> Result<CachedSubSql> {
        if let Some(cached) = self.sub_sql_cache.get(sql) {
            return Ok(CachedSubSql::Single(Arc::clone(cached)));
        }
        let (opt_stmt, tail) = parse_one(sql)?;
        if !tail.trim().is_empty() {
            return Ok(CachedSubSql::Multi);
        }
        let Some(stmt) = opt_stmt else {
            return Ok(CachedSubSql::Empty);
        };
        let arc = Arc::new(stmt);
        if self.sub_sql_cache.len() >= MAX_SUB_SQL_CACHE_ENTRIES {
            self.sub_sql_cache.clear();
        }
        self.sub_sql_cache.insert(sql.to_owned(), Arc::clone(&arc));
        Ok(CachedSubSql::Single(arc))
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
    if slot.schema.virtual_tables.is_empty() {
        return Ok(());
    }
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
pub struct CoreVTabHostContext<'a, 'b, 'c> {
    pub ctx: &'a mut ExecContext<'b>,
    pub db_name: &'c str,
    pub table_name: &'c str,
    pub declared: Option<VTabDeclaredSchema>,
    pub config_risk_flags: Option<VTabRiskFlags>,
    pub constraint_support: bool,
    pub find_function: bool,
    pub nochange_unread: bool,
    pub conflict_mode: VTabConflictMode,
    /// For an UPDATE: `true` for every column the statement does not assign
    /// (`sqlite3_value_nochange`).
    pub nochange_cols: Vec<bool>,
}

impl<'a, 'b, 'c> CoreVTabHostContext<'a, 'b, 'c> {
    #[inline]
    pub fn new(ctx: &'a mut ExecContext<'b>, db_name: &'c str, table_name: &'c str) -> Self {
        Self {
            ctx,
            db_name,
            table_name,
            declared: None,
            config_risk_flags: None,
            constraint_support: false,
            find_function: false,
            nochange_unread: false,
            conflict_mode: VTabConflictMode::Abort,
            nochange_cols: Vec::new(),
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

impl CoreVTabHostContext<'_, '_, '_> {
    /// vdbe.c `OP_Transaction`: a statement that writes (the ones a module steps on its shadow
    /// tables, an incremental-blob handle opened for writing) is refused with SQLITE_CORRUPT
    /// while `SQLITE_CorruptRdOnly` is set.
    ///
    /// `counted`: the statement is an INSERT / REPLACE / DELETE (`changeCntOn`); it halts with
    /// its statement transaction rolled back, so `sqlite3VdbeHalt` stores 0 in `db->nChange`
    /// (`sqlite3VdbeSetChanges(db, 0)`). The VDBE of an incremental-blob handle counts nothing.
    #[inline]
    fn refuse_write_after_corruption(&mut self, counted: bool) -> Result<()> {
        if self.ctx.db_set.corrupt_rdonly() {
            if counted {
                self.ctx.runtime.last_changes = 0;
            }
            return Err(SqliteError::new(
                ErrorCode::Corrupt,
                "database disk image is malformed",
            ));
        }
        Ok(())
    }
}

impl VTabHostContext for CoreVTabHostContext<'_, '_, '_> {
    fn with_table(
        &mut self,
        db_name: &str,
        table_name: &str,
        f: &mut dyn FnMut(&mut dyn VTabHostContext) -> Result<()>,
    ) -> Result<()> {
        let mut sub = CoreVTabHostContext::new(&mut *self.ctx, db_name, table_name);
        f(&mut sub)
    }

    fn db_name(&self) -> &str {
        self.db_name
    }

    fn table_name(&self) -> &str {
        self.table_name
    }

    fn declare_vtab(&mut self, create_table_sql: &str) -> Result<()> {
        if self.ctx.stmt_tx.interrupt_vtab_declaration {
            // C validates the two keywords before it runs the parser. A valid prefix
            // then encounters a separator and the pending interrupt before TABLE.
            let message = if sqliters_codegen::vtab::has_declare_vtab_prefix(create_table_sql) {
                "interrupted"
            } else {
                "syntax error"
            };
            return Err(SqliteError::new(ErrorCode::Error, message));
        }
        let (table_schema, hidden_cols) =
            parse_declare_vtab_sql(self.table_name, create_table_sql)?;
        self.declared = Some(VTabDeclaredSchema {
            table_schema,
            hidden_cols,
            config_risk_flags: self.config_risk_flags,
            constraint_support: self.constraint_support,
            find_function: self.find_function,
            nochange_unread: self.nochange_unread,
            declare_sql: create_table_sql.to_owned(),
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
            VTabConfigOption::FindFunction => {
                self.find_function = true;
            }
            VTabConfigOption::NochangeUnread => {
                self.nochange_unread = true;
            }
        }
        if let Some(ref mut decl) = self.declared {
            decl.config_risk_flags = self.config_risk_flags;
            decl.constraint_support = self.constraint_support;
            decl.find_function = self.find_function;
            decl.nochange_unread = self.nochange_unread;
        }
        Ok(())
    }

    fn vtab_on_conflict(&self) -> VTabConflictMode {
        self.conflict_mode
    }

    fn vtab_nochange(&self, col_idx: usize) -> bool {
        self.nochange_cols.get(col_idx).copied().unwrap_or(false)
    }

    fn execute_sql(&mut self, sql: &str, params: &[Value]) -> Result<usize> {
        crate::engine::execute_internal_vtab_sql(self.ctx, sql, params)
    }

    fn query_sql(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>> {
        let result = crate::engine::query_internal_vtab_sql(self.ctx, sql, params);
        if let Err(error) = &result
            && error.extended_code == ErrorCode::Error as i32
            && self.table_name.eq_ignore_ascii_case("pragma_locking_mode")
            && self.ctx.db_set.vtab_registry().has_automatic_pragma_module(self.table_name)
            && let Some(schema) = error.message.strip_prefix("unknown database ")
            && sql == format!("{PRAGMA_SQL_PREFIX}{}.locking_mode", quote_string_literal(schema))
        {
            // sqlite3TwoPartName records the qualifier's location within the nested SQL.
            self.ctx.stmt_tx.pragma_schema_error_offset = Some(PRAGMA_SQL_PREFIX.len());
        }
        result
    }

    fn shadow_get_rowid(&mut self, shadow_table: &str, rowid: i64) -> Result<Option<Vec<Value>>> {
        crate::engine::shadow_get_rowid_direct(self.ctx, self.db_name, shadow_table, rowid)
    }

    fn shadow_blob_get(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: i64,
        col_idx: usize,
        out: &mut Vec<u8>,
    ) -> Result<bool> {
        crate::engine::shadow_blob_get_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            col_idx,
            out,
        )
    }

    fn shadow_wr_seek(
        &mut self,
        shadow_table: &str,
        k0: i64,
        k1: &[u8],
        le: bool,
    ) -> Result<Option<Option<Vec<Value>>>> {
        crate::engine::shadow_wr_seek_direct(self.ctx, self.db_name, shadow_table, k0, k1, le)
    }

    fn shadow_put_rowid(
        &mut self,
        shadow_table: &str,
        rowid: i64,
        cols: &[Value],
        or_replace: bool,
    ) -> Result<()> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_put_rowid_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            rowid,
            cols,
            or_replace,
        )
    }

    fn shadow_delete_rowid(&mut self, shadow_table: &str, rowid: i64) -> Result<bool> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_delete_rowid_direct(self.ctx, self.db_name, shadow_table, rowid)
    }

    fn shadow_rowid_blob_eq(
        &mut self,
        shadow_table: &str,
        rowid: i64,
        col_idx: usize,
        expected: &[u8],
    ) -> Result<Option<bool>> {
        crate::engine::shadow_rowid_blob_eq_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            rowid,
            col_idx,
            expected,
        )
    }

    fn shadow_max_rowid(&mut self, shadow_table: &str) -> Result<Option<i64>> {
        crate::engine::shadow_max_rowid_direct(self.ctx, self.db_name, shadow_table)
    }

    fn execute_sql_counted(&mut self, sql: &str, params: &[Value]) -> Result<usize> {
        crate::engine::execute_internal_vtab_sql_counted(self.ctx, sql, params)
    }

    fn flush_vtab_pending(&mut self, db_name: Option<&str>, table: &str) -> Result<()> {
        let db = db_name.unwrap_or(self.db_name).to_owned();
        vtab_flush_one(self.ctx, &db, table)
    }

    fn sql_function_exists(&self, name: &str) -> bool {
        self.ctx.functions.has_scalar_name(name)
            || self.ctx.functions.has_aggregate_name(name)
            || sqliters_vdbe::builtin_scalar_flags(name, true).is_some()
    }

    fn last_insert_rowid(&mut self) -> i64 {
        self.ctx.runtime.last_insert_rowid
    }

    fn set_last_insert_rowid(&mut self, rowid: i64) {
        self.ctx.runtime.last_insert_rowid = rowid;
    }

    fn vtab_cursor_positioned(&self) -> bool {
        let slots = &self.ctx.activity.other_vtab_cursors;
        if slots[0] == 0 {
            return false;
        }
        let key = crate::engine::vtab_cursor_key(self.db_name, self.table_name);
        slots.iter().any(|&k| k == key || k == 1)
    }

    fn shadow_blob_read(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: i64,
        col_idx: usize,
        out: &mut [u8],
    ) -> Result<sqliters_types::ShadowBlobRead> {
        crate::engine::shadow_blob_read_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            col_idx,
            out,
        )
    }

    fn shadow_blob_write(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: i64,
        col_idx: usize,
        offset: usize,
        data: &[u8],
    ) -> Result<bool> {
        self.refuse_write_after_corruption(false)?;
        crate::engine::shadow_blob_write_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            col_idx,
            offset,
            data,
        )
    }

    fn shadow_int_read(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: i64,
        col_idx: usize,
    ) -> Result<Option<i64>> {
        crate::engine::shadow_int_read_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            col_idx,
        )
    }

    fn shadow_insert_row(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        sql: &str,
        rowid: Option<i64>,
        values: &[Value],
        replace: bool,
    ) -> Result<i64> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_insert_row_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            sql,
            rowid,
            values,
            replace,
        )
    }

    fn shadow_replace_blob(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: Option<i64>,
        blob: &[u8],
    ) -> Result<i64> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_replace_blob_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            blob,
        )
    }

    fn shadow_replace_int(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: Option<i64>,
        value: Option<i64>,
    ) -> Result<i64> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_replace_int_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
            value,
        )
    }

    fn shadow_delete_counted(
        &mut self,
        shadow_table: &str,
        hint: &mut sqliters_types::ShadowHint,
        rowid: i64,
    ) -> Result<()> {
        self.refuse_write_after_corruption(true)?;
        crate::engine::shadow_delete_counted_direct(
            self.ctx,
            self.db_name,
            shadow_table,
            hint,
            rowid,
        )
    }
}

/// Ensures that a persistent virtual table `(db_id, vt_def)` or an eponymous module is connected
/// in `ctx.db_set.vtab_registry`, returning its [`VTabDeclaredSchema`] and effective [`VTabRiskFlags`].
#[allow(clippy::type_complexity)]
pub fn ensure_vtab_connected(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    table_name: &str,
    allow_eponymous: bool,
) -> Result<Option<(String, String, Arc<VTabDeclaredSchema>, VTabRiskFlags)>> {
    // 1. Zero-alloc check if already connected in `(db_name, table_name)`
    {
        let db_name_ref = ctx
            .db_set
            .slot(db_id)
            .map(|s| s.name.as_str())
            .unwrap_or("main");
        if let Some(entry) = ctx
            .db_set
            .vtab_registry()
            .get_connected(db_name_ref, table_name)
        {
            let still_in_schema = ctx
                .db_set
                .slot(db_id)
                .is_some_and(|s| s.schema.find_virtual_table(table_name).is_some());
            if still_in_schema || (allow_eponymous && entry.is_eponymous) {
                return Ok(Some((
                    entry.db_name.clone(),
                    entry.table_name.clone(),
                    Arc::clone(&entry.declared),
                    entry.effective_risk_flags(),
                )));
            }
        }
    }

    let db_name = ctx
        .db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());

    // 2. Check if `(db_id, table_name)` is a persistent `VirtualTableDef` in `slot.schema`
    let vt_def_opt = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.schema.find_virtual_table(table_name).cloned());

    if let Some(vt_def) = vt_def_opt {
        let module = ctx
            .db_set
            .vtab_registry()
            .get_module(&vt_def.module_name)
            .ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("no such module: {}", vt_def.module_name),
                )
            })?;
        let (vt_box, declared) = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &vt_def.name);
            let vt_box = catch_callback_panic("vtab xConnect", || {
                module.connect(&mut host_ctx, &vt_def.args)
            })?;
            let declared = Arc::new(host_ctx.take_declared_schema()?);
            (vt_box, declared)
        };
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
            is_eponymous: module.is_eponymous(),
            is_eponymous_only: module.is_eponymous_only(),
            is_read_only: module.is_read_only(),
            shadow_suffixes,
            declared: Arc::clone(&declared),
            state: VTabInstanceState::Available(vt_box),
            in_tx: false,
            tx_seq: 0,
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
            let (vt_box, declared) = {
                let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &mod_name);
                let vt_box =
                    catch_callback_panic("vtab xConnect", || module.connect(&mut host_ctx, &[]))?;
                let declared = Arc::new(host_ctx.take_declared_schema()?);
                (vt_box, declared)
            };
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
                is_eponymous: module.is_eponymous(),
                is_eponymous_only: module.is_eponymous_only(),
            is_read_only: module.is_read_only(),
                shadow_suffixes,
                declared: Arc::clone(&declared),
                state: VTabInstanceState::Available(vt_box),
                in_tx: false,
                tx_seq: 0,
            };
            ctx.db_set.vtab_registry_mut().insert_connected(entry);
            return Ok(Some((db_name, mod_name, declared, effective_risk)));
        }
    }

    Ok(None)
}

/// Returns `true` if `expr` could contain a subquery or `IN <table>` that might reference a virtual table.
fn expr_may_reference_relation(expr: &Expr) -> bool {
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
        | Expr::Raise { .. } => false,
        Expr::InSubquery { .. }
        | Expr::InTable { .. }
        | Expr::Subquery(_)
        | Expr::Exists { .. } => true,
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => expr_may_reference_relation(inner),
        Expr::Binary { left, right, .. } => {
            expr_may_reference_relation(left) || expr_may_reference_relation(right)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_may_reference_relation(e)
                || expr_may_reference_relation(low)
                || expr_may_reference_relation(high)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_may_reference_relation(e) || list.iter().any(expr_may_reference_relation)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_may_reference_relation(e)
                || expr_may_reference_relation(pattern)
                || escape.as_deref().is_some_and(expr_may_reference_relation)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_some_and(expr_may_reference_relation)
                || branches
                    .iter()
                    .any(|(w, t)| expr_may_reference_relation(w) || expr_may_reference_relation(t))
                || else_expr
                    .as_deref()
                    .is_some_and(expr_may_reference_relation)
        }
        Expr::RowValue(elems) => elems.iter().any(expr_may_reference_relation),
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            let args_ref = match args {
                FunctionArgs::Exprs(es) => es.iter().any(expr_may_reference_relation),
                _ => false,
            };
            args_ref
                || filter.as_deref().is_some_and(expr_may_reference_relation)
                || match over {
                    Some(WindowRef::Spec(ws)) => {
                        ws.partition_by.iter().any(expr_may_reference_relation)
                            || ws
                                .order_by
                                .iter()
                                .any(|ob| expr_may_reference_relation(&ob.expr))
                    }
                    _ => false,
                }
        }
    }
}

/// Lazily connects any persistent virtual tables or eponymous modules referenced by `stmt`
/// (including through views and triggers) and enforces [`VTabRiskFlags`] (`trusted_schema`,
/// `DirectOnly`, `Innocuous`).
pub fn ensure_stmt_vtabs_connected(ctx: &mut ExecContext<'_>, stmt: &Stmt) -> Result<()> {
    // Ultra-fast zero-alloc path for ordinary-table and already-connected virtual-table DML / single-table SELECT
    // with no triggers and no subqueries in expressions.
    match stmt {
        Stmt::Insert(ins)
            if ins.ctes.is_empty()
                && ins.upsert_clauses.is_empty()
                && ins.returning.is_empty()
                && !ctx.db_set.has_any_triggers() =>
        {
            let simple_source = match &ins.source {
                InsertSource::DefaultValues => true,
                InsertSource::Values(rows) => rows
                    .iter()
                    .all(|r| !r.iter().any(expr_may_reference_relation)),
                InsertSource::Select(_) => false,
            };
            if simple_source {
                match ctx.db_set.find_relation_ref_in_set(&ins.table) {
                    Ok(Some((db_id, RelationRef::Table(tbl)))) => {
                        if ctx.db_set.vtab_registry().internal_sql_depth == 0
                            && ctx.db_set.is_shadow_table_metadata(db_id, &tbl.name)
                        {
                            vtab_on_user_shadow_access(ctx)?;
                        }
                        return Ok(());
                    }
                    Ok(None) => {
                        if let Some((_db_id, _db_name, entry)) =
                            ctx.db_set.find_connected_vtab_entry_in_set(&ins.table, true)
                            && !entry.module_name.eq_ignore_ascii_case("fts5vocab")
                            && !entry.module_name.eq_ignore_ascii_case("dbstat")
                        {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
        Stmt::Select(sel)
            if sel.ctes.is_empty()
                && sel.body.compounds.is_empty()
                && !sel
                    .order_by
                    .iter()
                    .any(|ob| expr_may_reference_relation(&ob.expr))
                && sel.limit.as_ref().is_none_or(|l| {
                    !expr_may_reference_relation(&l.limit)
                        && !l
                            .offset
                            .as_ref()
                            .is_some_and(expr_may_reference_relation)
                }) =>
        {
            if let SelectCore::Select {
                projection,
                from: Some(fc),
                selection,
                group_by,
                having,
                windows,
                ..
            } = &sel.body.head
                && fc.joins.is_empty()
                && windows.is_empty()
                && let TableOrSubquery::Table {
                    name, table_args, ..
                } = &fc.head
                && table_args
                    .as_ref()
                    .is_none_or(|a| !a.iter().any(expr_may_reference_relation))
                && !projection.iter().any(|rc| {
                    matches!(rc, ResultColumn::Expr { expr, .. } if expr_may_reference_relation(expr))
                })
                && !selection.as_ref().is_some_and(expr_may_reference_relation)
                && !group_by.iter().any(expr_may_reference_relation)
                && !having.as_ref().is_some_and(expr_may_reference_relation)
            {
                match ctx.db_set.find_relation_ref_in_set(name) {
                    Ok(Some((db_id, RelationRef::Table(tbl)))) if table_args.is_none() => {
                        if ctx.db_set.vtab_registry().internal_sql_depth == 0
                            && ctx.db_set.is_shadow_table_metadata(db_id, &tbl.name)
                        {
                            vtab_on_user_shadow_access(ctx)?;
                        }
                        return Ok(());
                    }
                    Ok(None) => {
                        if let Some((_db_id, _db_name, entry)) =
                            ctx.db_set.find_connected_vtab_entry_in_set(name, true)
                            && !entry.module_name.eq_ignore_ascii_case("fts5vocab")
                            && !entry.module_name.eq_ignore_ascii_case("dbstat")
                        {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
            }
        }
        Stmt::Update(upd)
            if upd.ctes.is_empty()
                && upd.from.is_none()
                && upd.returning.is_empty()
                && !ctx.db_set.has_any_triggers()
                && !upd
                    .assignments
                    .iter()
                    .any(|a| expr_may_reference_relation(&a.expr))
                && !upd
                    .selection
                    .as_ref()
                    .is_some_and(expr_may_reference_relation) =>
        {
            if let Ok(Some((db_id, RelationRef::Table(tbl)))) =
                ctx.db_set.find_relation_ref_in_set(&upd.table)
            {
                if ctx.db_set.vtab_registry().internal_sql_depth == 0
                    && ctx.db_set.is_shadow_table_metadata(db_id, &tbl.name)
                {
                    vtab_on_user_shadow_access(ctx)?;
                }
                return Ok(());
            }
        }
        Stmt::Delete(del)
            if del.ctes.is_empty()
                && del.returning.is_empty()
                && !ctx.db_set.has_any_triggers()
                && !del
                    .selection
                    .as_ref()
                    .is_some_and(expr_may_reference_relation) =>
        {
            if let Ok(Some((db_id, RelationRef::Table(tbl)))) =
                ctx.db_set.find_relation_ref_in_set(&del.table)
            {
                if ctx.db_set.vtab_registry().internal_sql_depth == 0
                    && ctx.db_set.is_shadow_table_metadata(db_id, &tbl.name)
                {
                    vtab_on_user_shadow_access(ctx)?;
                }
                return Ok(());
            }
        }
        _ => {}
    }

    let mut visited_views = Vec::new();
    ensure_stmt_vtabs_connected_inner(ctx, stmt, None, false, &mut visited_views)
}

struct CteScope<'a> {
    parent: Option<&'a CteScope<'a>>,
    definitions: &'a [sqliters_syntax::CommonTableExpr],
    visited: std::cell::RefCell<Vec<bool>>,
}

impl<'a> CteScope<'a> {
    fn new(parent: Option<&'a Self>, definitions: &'a [sqliters_syntax::CommonTableExpr]) -> Option<Self> {
        (!definitions.is_empty()).then(|| Self {
            parent,
            definitions,
            visited: std::cell::RefCell::new(vec![false; definitions.len()]),
        })
    }
}

fn ensure_stmt_vtabs_connected_inner(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
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
        // C's ALTER TABLE ADD COLUMN verifies CHECK / NOT NULL / STRICT constraints with a
        // nested "SELECT ... FROM pragma_quick_check(..)" (sqlite3AlterFinishAddColumn): the
        // port of C's code generator reaches that eponymous table through a connected instance.
        Stmt::AlterTable {
            action: sqliters_syntax::AlterTableAction::AddColumn { .. },
            ..
        } => {
            let q = QualifiedName::unqualified("pragma_quick_check");
            let _ =
                ensure_qname_vtab_connected(ctx, &q, None, true, in_view_or_trigger, visited_views);
            Ok(())
        }
        Stmt::Insert(ins) => {
            let scope = CteScope::new(cte_names, &ins.ctes);
            let active_ctes = scope.as_ref().or(cte_names);
            ensure_qname_vtab_connected(
                ctx,
                &ins.table,
                None,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            match &ins.source {
                InsertSource::Select(sel) => ensure_select_vtabs_connected(
                    ctx,
                    sel,
                    active_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?,
                InsertSource::Values(rows) => {
                    for row in rows {
                        for e in row {
                            ensure_expr_vtabs_connected(
                                ctx,
                                e,
                                active_ctes,
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
                        active_ctes,
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
                            active_ctes,
                            in_view_or_trigger,
                            visited_views,
                        )?;
                    }
                    if let Some(w) = where_clause {
                        ensure_expr_vtabs_connected(
                            ctx,
                            w,
                            active_ctes,
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
                        active_ctes,
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
            let scope = CteScope::new(cte_names, &upd.ctes);
            let active_ctes = scope.as_ref().or(cte_names);
            ensure_qname_vtab_connected(
                ctx,
                &upd.table,
                None,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            for a in &upd.assignments {
                ensure_expr_vtabs_connected(
                    ctx,
                    &a.expr,
                    active_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            if let Some(ref from) = upd.from {
                ensure_from_vtabs_connected(
                    ctx,
                    from,
                    active_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            if let Some(ref w) = upd.selection {
                ensure_expr_vtabs_connected(
                    ctx,
                    w,
                    active_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            for rc in &upd.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        active_ctes,
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
            let scope = CteScope::new(cte_names, &del.ctes);
            let active_ctes = scope.as_ref().or(cte_names);
            ensure_qname_vtab_connected(
                ctx,
                &del.table,
                None,
                true,
                in_view_or_trigger,
                visited_views,
            )?;
            if let Some(ref w) = del.selection {
                ensure_expr_vtabs_connected(
                    ctx,
                    w,
                    active_ctes,
                    in_view_or_trigger,
                    visited_views,
                )?;
            }
            for rc in &del.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    ensure_expr_vtabs_connected(
                        ctx,
                        expr,
                        active_ctes,
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
            if let Some((db_id, vt_name)) = ctx
                .db_set
                .find_virtual_table_in_set(name)
                .map(|(id, v)| (id, v.name.clone()))
            {
                let _ = ensure_vtab_connected(ctx, db_id, &vt_name, false)?;
            }
            Ok(())
        }
        Stmt::AlterTable { table, .. } => {
            if let Some((db_id, vt_name)) = ctx
                .db_set
                .find_virtual_table_in_set(table)
                .map(|(id, v)| (id, v.name.clone()))
            {
                let _ = ensure_vtab_connected(ctx, db_id, &vt_name, false)?;
            }
            Ok(())
        }
        Stmt::Pragma {
            schema,
            name,
            value: Some(pv),
        } => {
            if name.eq_ignore_ascii_case("table_info") || name.eq_ignore_ascii_case("table_xinfo") {
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
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    if !ctx.db_set.has_any_triggers() {
        return Ok(());
    }
    let Ok(Some((db_id, rel))) = ctx.db_set.find_relation_ref_in_set(target) else {
        return Ok(());
    };
    let rel_name = match rel {
        RelationRef::CatalogTable => return Ok(()),
        RelationRef::Table(t) => t.name.clone(),
        RelationRef::View(v) => v.name.clone(),
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
        if visited_views.contains(&trig_visit_key) {
            continue;
        }
        visited_views.push(trig_visit_key);
        if let Some(ref w) = tr.when_clause {
            ensure_expr_vtabs_connected(ctx, w, None, true, visited_views)?;
        }
        for step in &tr.body {
            let step_stmt = match step {
                TriggerStep::Select(s) => Stmt::Select(s.clone()),
                TriggerStep::Insert(i) => Stmt::Insert(i.clone()),
                TriggerStep::Update(u) => Stmt::Update(u.clone()),
                TriggerStep::Delete(d) => Stmt::Delete(d.clone()),
            };
            ensure_stmt_vtabs_connected_inner(ctx, &step_stmt, None, true, visited_views)?;
        }
    }
    Ok(())
}

fn ensure_qname_vtab_connected(
    ctx: &mut ExecContext<'_>,
    qname: &QualifiedName,
    cte_names: Option<&CteScope<'_>>,
    allow_eponymous: bool,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    if qname.schema.is_none() {
        for scope in std::iter::successors(cte_names, |scope| scope.parent) {
            if let Some((index, definition)) = scope.definitions.iter().enumerate()
                .find(|(_, definition)| definition.name.eq_ignore_ascii_case(&qname.name))
            {
                // A recursive reference is already being resolved in its lexical scope.
                if std::mem::replace(&mut scope.visited.borrow_mut()[index], true) {
                    return Ok(());
                }
                return ensure_select_vtabs_connected(
                    ctx, &definition.query, Some(scope), in_view_or_trigger, visited_views,
                );
            }
        }
    }
    if let Ok(Some((db_id, rel))) = ctx.db_set.find_relation_ref_in_set(qname) {
        match rel {
            RelationRef::CatalogTable => return Ok(()),
            RelationRef::Table(tbl) => {
                if ctx.db_set.vtab_registry().internal_sql_depth == 0
                    && ctx.db_set.is_shadow_table_metadata(db_id, &tbl.name)
                {
                    vtab_on_user_shadow_access(ctx)?;
                }
                return Ok(());
            }
            RelationRef::View(v) => {
                let v_key = (db_id, v.name.to_ascii_lowercase());
                if !visited_views.contains(&v_key) {
                    let v_select = v.select.clone();
                    visited_views.push(v_key.clone());
                    let res =
                        ensure_select_vtabs_connected(ctx, &v_select, None, true, visited_views);
                    if let Some(pos) = visited_views.iter().rposition(|k| k == &v_key) {
                        visited_views.remove(pos);
                    }
                    res?;
                }
                return Ok(());
            }
        }
    }
    if let Some((db_id, vt_name, vt_mod, vt_args)) = ctx
        .db_set
        .find_virtual_table_in_set(qname)
        .map(|(id, v)| (id, v.name.clone(), v.module_name.clone(), v.args.clone()))
    {
        let _ = (&vt_mod, &vt_args);
        if let Some((_, connected_name, _, risk)) =
            ensure_vtab_connected(ctx, db_id, &vt_name, false)?
        {
            check_vtab_risk(
                &connected_name,
                risk,
                ctx.db_set.vtab_registry().trusted_schema,
                in_view_or_trigger,
            )?;
        }
        return Ok(());
    }
    if allow_eponymous {
        if ctx.db_set.vtab_registry().internal_sql_depth == 0
            && ctx.db_set.vtab_registry().has_any_in_tx()
            && qname.name.eq_ignore_ascii_case("dbstat")
        {
            vtab_flush_shadow_all(ctx)?;
        }
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
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    let scope = CteScope::new(cte_names, &sel.ctes);
    let active_ctes = scope.as_ref().or(cte_names);
    ensure_select_core_vtabs_connected(
        ctx,
        &sel.body.head,
        active_ctes,
        in_view_or_trigger,
        visited_views,
    )?;
    for (_, right) in &sel.body.compounds {
        ensure_select_core_vtabs_connected(
            ctx,
            right,
            active_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
    }
    for ob in &sel.order_by {
        ensure_expr_vtabs_connected(
            ctx,
            &ob.expr,
            active_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
    }
    if let Some(ref lim) = sel.limit {
        ensure_expr_vtabs_connected(
            ctx,
            &lim.limit,
            active_ctes,
            in_view_or_trigger,
            visited_views,
        )?;
        if let Some(ref off) = lim.offset {
            ensure_expr_vtabs_connected(ctx, off, active_ctes, in_view_or_trigger, visited_views)?;
        }
    }
    Ok(())
}

fn ensure_select_core_vtabs_connected(
    ctx: &mut ExecContext<'_>,
    core: &SelectCore,
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
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
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
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
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
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
    cte_names: Option<&CteScope<'_>>,
    in_view_or_trigger: bool,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    if !expr_may_reference_relation(expr) {
        return Ok(());
    }
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

/// 1 when the outermost SQL savepoint is a transaction savepoint (`SAVEPOINT` issued in
/// autocommit mode): C does not count it in `db->nSavepoint`, so the savepoint numbers a
/// virtual table sees (`sqlite3VtabSavepoint`) are one lower than this engine's slot numbers
/// while it is open; the transaction savepoint itself is number -1 (`usize::MAX` here).
#[inline]
fn vtab_tx_savepoint_offset(ctx: &ExecContext<'_>) -> usize {
    usize::from(
        ctx.savepoints
            .first()
            .is_some_and(|sp| sp.is_transaction_savepoint),
    )
}

/// Ensures `VirtualTable::begin` has been called on `(db_name, table_name)` prior to `update`.
#[inline]
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
    // vtab.c `sqlite3VtabBegin`: `iSvpt = db->nStatement + db->nSavepoint`; when it is not
    // zero the table is told about the innermost open savepoint only (`xSavepoint(iSvpt-1)`).
    // A transaction savepoint (SAVEPOINT in autocommit mode) is not counted in nSavepoint.
    let active_sp_depth = ctx.savepoints.len() - vtab_tx_savepoint_offset(ctx)
        + usize::from(ctx.stmt_tx.stmt_sp_slot.is_some());
    let mut vt = ctx
        .db_set
        .vtab_registry_mut()
        .checkout_vtab_instance(db_name, table_name)?;
    let res = {
        let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
        let mut res = catch_callback_panic("vtab xBegin", || vt.begin(&mut host_ctx));
        if res.is_ok() && active_sp_depth > 0 {
            res = catch_callback_panic("vtab xSavepoint", || {
                vt.savepoint(&mut host_ctx, active_sp_depth - 1)
            })
            .map_err(vtab_savepoint_hook_error);
        }
        res
    };
    ctx.db_set
        .vtab_registry_mut()
        .restore_vtab(db_name, table_name, vt);
    res?;
    ctx.db_set
        .vtab_registry_mut()
        .mark_joined_tx(db_name, table_name);
    Ok(())
}

/// Performs `VirtualTable::update` on `(db_name, table_name)`.
pub fn vtab_perform_update(
    ctx: &mut ExecContext<'_>,
    db_name: &str,
    table_name: &str,
    op: VTabUpdateOperation,
) -> Result<Option<i64>> {
    vtab_perform_update_with_conflict(ctx, db_name, table_name, op, VTabConflictMode::Abort)
}

/// Performs `VirtualTable::update` on `(db_name, table_name)` with active `conflict_mode`.
pub fn vtab_perform_update_with_conflict(
    ctx: &mut ExecContext<'_>,
    db_name: &str,
    table_name: &str,
    op: VTabUpdateOperation,
    conflict_mode: VTabConflictMode,
) -> Result<Option<i64>> {
    vtab_perform_update_with_nochange(ctx, db_name, table_name, op, conflict_mode, &[])
}

/// Like [`vtab_perform_update_with_conflict`]; `nochange_cols[i]` is true for every column an
/// UPDATE statement leaves unassigned (reported through `VTabHostContext::vtab_nochange`).
pub fn vtab_perform_update_with_nochange(
    ctx: &mut ExecContext<'_>,
    db_name: &str,
    table_name: &str,
    op: VTabUpdateOperation,
    conflict_mode: VTabConflictMode,
    nochange_cols: &[bool],
) -> Result<Option<i64>> {
    vtab_begin_if_needed(ctx, db_name, table_name)?;
    let mut vt = ctx
        .db_set
        .vtab_registry_mut()
        .checkout_vtab_instance(db_name, table_name)?;
    let res = {
        let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
        host_ctx.conflict_mode = conflict_mode;
        host_ctx.nochange_cols = nochange_cols.to_vec();
        catch_callback_panic("vtab xUpdate", || vt.update(&mut host_ctx, op))
    };
    ctx.db_set
        .vtab_registry_mut()
        .restore_vtab(db_name, table_name, vt);
    res
}

/// Invokes `VirtualTable::sync` on all virtual tables participating in the current transaction.
pub fn vtab_sync_all(ctx: &mut ExecContext<'_>) -> Result<()> {
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let mut vt = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(&db_name, &table_name)?;
        let res = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
            catch_callback_panic("vtab xSync", || vt.sync(&mut host_ctx))
        };
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
        let Ok(mut vt) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(&db_name, &table_name)
        else {
            continue;
        };
        let res = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
            catch_callback_panic("vtab xCommit", || vt.commit(&mut host_ctx))
        };
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
        let Ok(mut vt) = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(&db_name, &table_name)
        else {
            continue;
        };
        {
            let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
            let _ = catch_callback_panic("vtab xRollback", || vt.rollback(&mut host_ctx));
        }
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

/// vtab.c `sqlite3VtabSavepoint`: the result code of xSavepoint / xRelease / xRollbackTo is all
/// the core takes — the module's `zErrMsg` is not imported (no `sqlite3VtabImportErrmsg`), so
/// the statement fails with `sqlite3ErrStr(rc)`.
#[cold]
fn vtab_savepoint_hook_error(mut e: sqliters_types::SqliteError) -> sqliters_types::SqliteError {
    e.message = e.code.as_str().to_owned();
    e
}

/// Invokes `VirtualTable::savepoint` on all virtual tables participating in the current transaction.
pub fn vtab_savepoint_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    let idx = idx.wrapping_sub(vtab_tx_savepoint_offset(ctx));
    let active = ctx.db_set.vtab_registry_mut().take_active_tx_vtabs();
    let mut result = Ok(());
    for (_, db_name, table_name) in &active {
        let mut vt = match ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(db_name, table_name)
        {
            Ok(vt) => vt,
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        let res = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
            catch_callback_panic("vtab xSavepoint", || vt.savepoint(&mut host_ctx, idx))
                .map_err(vtab_savepoint_hook_error)
        };
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(db_name, table_name, vt);
        if let Err(e) = res {
            result = Err(e);
            break;
        }
    }
    ctx.db_set.vtab_registry_mut().recycle_tx_vtab_names(active);
    result
}

/// Invokes `VirtualTable::release` on all virtual tables participating in the current transaction.
pub fn vtab_release_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    // The statement savepoint being released is already closed on the pagers: a write a
    // module makes from xRelease must not reopen it.
    if ctx.stmt_tx.stmt_sp_slot == Some(idx) {
        ctx.stmt_tx.stmt_sp_slot = None;
    }
    let idx = idx.wrapping_sub(vtab_tx_savepoint_offset(ctx));
    let active = ctx.db_set.vtab_registry_mut().take_active_tx_vtabs();
    let mut result = Ok(());
    for (_, db_name, table_name) in &active {
        let mut vt = match ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(db_name, table_name)
        {
            Ok(vt) => vt,
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        let res = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, db_name, table_name);
            catch_callback_panic("vtab xRelease", || vt.release(&mut host_ctx, idx))
                .map_err(vtab_savepoint_hook_error)
        };
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(db_name, table_name, vt);
        if let Err(e) = res {
            result = Err(e);
            break;
        }
    }
    ctx.db_set.vtab_registry_mut().recycle_tx_vtab_names(active);
    result
}

/// Invokes `VirtualTable::rollback_to` on all virtual tables participating in the current transaction.
pub fn vtab_rollback_to_all(ctx: &mut ExecContext<'_>, idx: usize) -> Result<()> {
    let idx = idx.wrapping_sub(vtab_tx_savepoint_offset(ctx));
    let active = ctx.db_set.vtab_registry().active_tx_vtabs();
    for (db_name, table_name) in active {
        let mut vt = ctx
            .db_set
            .vtab_registry_mut()
            .checkout_vtab_instance(&db_name, &table_name)?;
        let res = {
            let mut host_ctx = CoreVTabHostContext::new(ctx, &db_name, &table_name);
            catch_callback_panic("vtab xRollbackTo", || vt.rollback_to(&mut host_ctx, idx))
                .map_err(vtab_savepoint_hook_error)
        };
        ctx.db_set
            .vtab_registry_mut()
            .restore_vtab(&db_name, &table_name, vt);
        res?;
    }
    Ok(())
}

/// C never writes a module's pending data out because some SQL statement is about to read:
/// what FTS5 holds in its pending-terms hash stays there until one of FTS5's own flush points
/// (xSync, xSavepoint, a nested xRelease, `sqlite3Fts5IndexBeginWrite`). Kept as the single
/// place the former call sites go through.
#[inline]
pub fn vtab_flush_shadow_all(_ctx: &mut ExecContext<'_>) -> Result<()> {
    Ok(())
}

/// `fts5VocabOpenMethod` -> `sqlite3Fts5FlushToDisk(pFts5)`: a cursor on an `fts5vocab` table
/// writes the pending data of the FTS5 table it describes (and of no other table) to disk.
pub fn vtab_flush_one(ctx: &mut ExecContext<'_>, target_db: &str, target_tbl: &str) -> Result<()> {
    // (With nothing pending the flush writes nothing; C still runs it on every cursor open.)
    if ctx
        .db_set
        .vtab_registry()
        .get_connected(target_db, target_tbl)
        .is_none()
    {
        return Ok(());
    }
    let Ok(mut vt) = ctx
        .db_set
        .vtab_registry_mut()
        .checkout_vtab_instance(target_db, target_tbl)
    else {
        return Ok(());
    };
    let res = {
        let mut host_ctx = CoreVTabHostContext::new(ctx, target_db, target_tbl);
        catch_callback_panic("vtab flush_before_shadow_access", || {
            vt.flush_before_shadow_access(&mut host_ctx)
        })
    };
    ctx.db_set
        .vtab_registry_mut()
        .restore_vtab(target_db, target_tbl, vt);
    res
}

/// Flushes buffered `in_tx` virtual tables and invalidates clean shadow caches when user SQL
/// directly touches a shadow table.
pub fn vtab_on_user_shadow_access(ctx: &mut ExecContext<'_>) -> Result<()> {
    vtab_flush_shadow_all(ctx)?;
    ctx.db_set
        .vtab_registry_mut()
        .invalidate_all_shadow_caches();
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

    /// (`dbstat` has no `xUpdate`)
    fn is_read_only(&self) -> bool {
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
    /// `statBestIndex()` of dbstat.c (3.54.1), used by the port of C's code generator; the
    /// engine's executor plans with [`Self::best_index`].
    fn best_index_calls(&self, info: &mut VTabIndexInfo, _calls: &mut dyn sqliters_types::VTabIndexInfoCalls) -> Result<()> {
        let mut i_schema = None;
        let mut i_name = None;
        let mut i_agg = None;

        // Look for a valid schema=? constraint. If found, change the idxNum to 1 and request
        // the value of that constraint be sent to xFilter. And lower the cost estimate to
        // encourage the constrained version to be used.
        for (i, c) in info.constraints.iter().enumerate() {
            if c.op != VTabConstraintOp::Eq {
                continue;
            }
            if !c.usable {
                // Force DBSTAT table should always be the right-most table in a join
                return Err(SqliteError::new(ErrorCode::Constraint, "unusable dbstat constraint"));
            }
            match c.i_column {
                0 => i_name = Some(i),    // name
                10 => i_schema = Some(i), // schema
                11 => i_agg = Some(i),    // aggregate
                _ => {}
            }
        }
        let mut i = 0usize;
        if let Some(k) = i_schema {
            i += 1;
            info.constraint_usage[k].argv_index = i;
            info.constraint_usage[k].omit = true;
            info.idx_num |= 0x01;
        }
        if let Some(k) = i_name {
            i += 1;
            info.constraint_usage[k].argv_index = i;
            info.idx_num |= 0x02;
        }
        if let Some(k) = i_agg {
            i += 1;
            info.constraint_usage[k].argv_index = i;
            info.idx_num |= 0x04;
        }
        info.estimated_cost = 1.0;

        // Records are always returned in ascending order of (name, path). If this will satisfy
        // the client, set the orderByConsumed flag so that SQLite does not do an external sort.
        let ob = &info.order_by;
        if (ob.len() == 1 && ob[0].i_column == 0 && !ob[0].desc)
            || (ob.len() == 2 && ob[0].i_column == 0 && !ob[0].desc && ob[1].i_column == 1 && !ob[1].desc)
        {
            info.order_by_consumed = true;
            info.idx_num |= 0x08;
        }
        info.idx_flags |= sqliters_types::SQLITE_INDEX_SCAN_HEX;
        Ok(())
    }

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
    /// `statFilter()` of dbstat.c with the plan of `statBestIndex()` (idxNum 0x01 schema=,
    /// 0x02 name=, 0x04 aggregate=, 0x08 ORDER BY name; the arguments in that order).
    fn filter_c(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
        _in_lists: &[Option<Vec<Value>>],
    ) -> Result<()> {
        let mut i_arg = 0usize;
        let mut eng_idx = 0i32;
        let mut eng_args: Vec<Value> = Vec::new();
        if idx_num & 0x01 != 0 {
            // "pCsr->iDb = sqlite3FindDbName(pTab->db, zDbase); if( pCsr->iDb<0 ){ isEof }"
            let v = args.get(i_arg).cloned().unwrap_or(Value::Null);
            i_arg += 1;
            if v.is_null() {
                self.rows.clear();
                self.pos = 0;
                return Ok(());
            }
            eng_idx |= 0x01;
            eng_args.push(v);
        }
        let mut z_name: Option<String> = None;
        if idx_num & 0x02 != 0 {
            // "zName = (const char*)sqlite3_value_text(argv[iArg++]);" (NULL: no WHERE)
            let v = args.get(i_arg).cloned().unwrap_or(Value::Null);
            i_arg += 1;
            if !v.is_null() {
                z_name = Some(v.to_text_lossy());
            }
        }
        if idx_num & 0x04 != 0 {
            // "pCsr->isAgg = sqlite3_value_double(argv[iArg++])!=0.0;"
            let v = args.get(i_arg).cloned().unwrap_or(Value::Null);
            eng_idx |= 0x02;
            eng_args.push(Value::Integer(i64::from(v.to_real_lossy() != 0.0)));
        }
        match self.filter(ctx, eng_idx, None, &eng_args) {
            Ok(()) => {}
            Err(e) if e.message.starts_with("unknown database") => {
                self.rows.clear();
                self.pos = 0;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        // The b-trees in the order of C's query: "SELECT 'sqlite_schema' AS name, ... UNION
        // ALL SELECT name, rootpage, type FROM "%w".sqlite_schema WHERE rootpage!=0" (the
        // engine's aggregate rows come sorted by name); the pages of one b-tree keep their order.
        let schema = match eng_args.first() {
            Some(v) if eng_idx & 0x01 != 0 => v.to_text_lossy(),
            _ => self.default_schema.clone().unwrap_or_else(|| ctx.db_name().to_owned()),
        };
        let order_sql = format!("SELECT name FROM {}.sqlite_schema WHERE rootpage!=0", quote_ident(&schema));
        if let Ok(order) = ctx.query_sql(&order_sql, &[]) {
            let mut names: Vec<String> = vec!["sqlite_schema".to_owned()];
            names.extend(order.into_iter().filter_map(|r| r.first().map(Value::to_text_lossy)));
            let pos = |n: &str| names.iter().position(|x| x == n).unwrap_or(usize::MAX);
            self.rows.sort_by_key(|r| pos(&r.name));
        }
        if let Some(n) = z_name {
            // "WHERE name=%Q" (BINARY)
            self.rows.retain(|r| r.name == n);
        }
        if idx_num & 0x08 != 0 {
            // " ORDER BY name" (the pages of one b-tree stay in path order)
            self.rows.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        }
        Ok(())
    }

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
            let _ = btree.end_read_if_unused();
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
    let Ok(page_buf) = btree
        .pager_mut()
        .read_page_unchecked(pgno)
        .map(|p| p.to_vec())
    else {
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
            let Ok(ovf_buf) = btree
                .pager_mut()
                .read_page_unchecked(cur_ovf)
                .map(|p| p.to_vec())
            else {
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
    pub(crate) fn declared_table(&self) -> Result<TableSchema> {
        parse_declare_vtab_sql(&self.module_name, self.declare_sql).map(|(table, _)| table)
    }

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
            "temp_store" => ("CREATE TABLE x(temp_store)", 1, false, false, false),
            "threads" => ("CREATE TABLE x(threads)", 1, false, false, false),
            "encoding" => ("CREATE TABLE x(encoding)", 1, false, false, false),
            "cache_size" => ("CREATE TABLE x(cache_size,schema hidden)", 1, false, true, false),
            "locking_mode" => (
                "CREATE TABLE x(locking_mode,schema hidden)",
                1,
                false,
                true,
                false,
            ),
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

    /// `pragmaVtabConnect()` does not call `sqlite3_vtab_config()`: the tables are
    /// SQLITE_VTABRISK_Normal (refused inside views and triggers without trusted_schema).
    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Normal
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
        Ok(Box::new(PragmaVTabTable { spec: self.clone() }))
    }
}

#[derive(Debug)]
struct PragmaVTabTable {
    spec: PragmaVTabModule,
}

impl VirtualTable for PragmaVTabTable {
    /// `pragmaVtabBestIndex()` of pragma.c (3.54.1), used by the port of C's code generator;
    /// the engine's executor plans with [`Self::best_index`].
    fn best_index_calls(&self, info: &mut VTabIndexInfo, _calls: &mut dyn sqliters_types::VTabIndexInfoCalls) -> Result<()> {
        // PragmaVtab.iHidden: index of the first hidden column; nHidden: number of hidden
        // columns (the argument, then the schema)
        let i_hidden = self.spec.visible_cols as i32;
        let n_hidden = usize::from(self.spec.has_arg) + usize::from(self.spec.has_schema);
        info.estimated_cost = 1.0;
        if n_hidden == 0 {
            return Ok(());
        }
        let mut seen = [0usize; 2];
        for (i, c) in info.constraints.iter().enumerate() {
            if c.i_column < i_hidden {
                continue;
            }
            if c.op != VTabConstraintOp::Eq {
                continue;
            }
            if !c.usable {
                return Err(SqliteError::new(ErrorCode::Constraint, "unusable pragma constraint"));
            }
            let j = (c.i_column - i_hidden) as usize;
            seen[j] = i + 1;
        }
        if seen[0] == 0 {
            info.estimated_cost = 2_147_483_647.0;
            info.estimated_rows = 2_147_483_647;
            return Ok(());
        }
        let j = seen[0] - 1;
        info.constraint_usage[j].argv_index = 1;
        info.constraint_usage[j].omit = true;
        info.estimated_cost = 20.0;
        info.estimated_rows = 20;
        if seen[1] != 0 {
            let j = seen[1] - 1;
            info.constraint_usage[j].argv_index = 2;
            info.constraint_usage[j].omit = true;
        }
        Ok(())
    }

    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()> {
        let arg_col = self.spec.visible_cols as i32;
        let schema_col = (self.spec.visible_cols + usize::from(self.spec.has_arg)) as i32;
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

const PRAGMA_SQL_PREFIX: &str = "PRAGMA ";

fn quote_string_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl VirtualCursor for PragmaVTabCursor {
    /// `pragmaVtabFilter()` of pragma.c with the plan of `pragmaVtabBestIndex()`: idxNum 0,
    /// argv[0] the first hidden column (the argument, or the schema of a pragma without
    /// one), argv[1] the schema.
    fn filter_c(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        args: &[Value],
        _in_lists: &[Option<Vec<Value>>],
    ) -> Result<()> {
        let mut eng_idx = 0i32;
        let mut eng_args: Vec<Value> = Vec::new();
        // "j = (pTab->pName->mPragFlg & PragFlg_Result1)!=0 ? 0 : 1;" — azArg[0] the argument,
        // azArg[1] the schema; a NULL value leaves its slot empty
        let mut j = if self.spec.has_arg { 0 } else { 1 };
        for v in args {
            if !v.is_null() {
                if j == 0 {
                    eng_idx |= 0x01;
                } else {
                    eng_idx |= 0x02;
                }
            }
            j += 1;
        }
        let mut j = if self.spec.has_arg { 0 } else { 1 };
        for v in args {
            if !v.is_null() && (j == 0 || (j == 1 && eng_idx & 0x02 != 0)) {
                eng_args.push(v.clone());
            }
            j += 1;
        }
        self.filter(ctx, eng_idx, None, &eng_args)
    }

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

        let schema = (!self.schema_val.is_null()).then(|| self.schema_val.to_text_lossy());
        let schema_prefix = schema.as_deref()
            .map(|name| format!("{}.", quote_string_literal(name)))
            .unwrap_or_default();

        let pragma_sql = if !self.arg_val.is_null() {
            format!(
                "{PRAGMA_SQL_PREFIX}{schema_prefix}{}({})",
                self.spec.pragma_name,
                quote_string_literal(&self.arg_val.to_text_lossy())
            )
        } else {
            format!("{PRAGMA_SQL_PREFIX}{schema_prefix}{}", self.spec.pragma_name)
        };

        self.rows = ctx.query_sql(&pragma_sql, &[]).map_err(|mut error| {
            // pragmaVtabFilter constructs the qualifier with %Q. Preserve that token
            // in the sqlite3TwoPartName diagnostic from the nested PRAGMA.
            if let Some(name) = schema.as_deref()
                && error.code == ErrorCode::Error
                && error.message.strip_prefix("unknown database ") == Some(name)
            {
                error.message = format!("unknown database {}", quote_string_literal(name));
            }
            error
        })?;
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
        if self.spec.has_schema && col_idx == self.spec.visible_cols + usize::from(self.spec.has_arg) {
            // pragma.c:3048–3051 indexes azArg by the hidden-column offset.
            // Schema-only pragmas store their input in azArg[1], while this
            // column reads azArg[0]. Preserve C's NULL result.
            return Ok(if self.spec.has_arg { self.schema_val.clone() } else { self.arg_val.clone() });
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
pub const SQLITE_BUILTIN_FUNCTION_LIST: &[(&str, &str, i64, i64)] = &builtin_function_list();

const fn builtin_function_list() -> [(&'static str, &'static str, i64, i64); 161] {
    use sqliters_codegen::builtin_funcs::BUILTIN_FUNCS;
    use sqliters_codegen::consts::SQLITE_FUNC_INTERNAL;
    use sqliters_types::FunctionFlags;
    const MASK: u32 = FunctionFlags::DETERMINISTIC.0
        | FunctionFlags::DIRECTONLY.0
        | FunctionFlags::SUBTYPE.0
        | FunctionFlags::INNOCUOUS.0;

    // The pinned native dump preserves the sqlite3BuiltinFunctions hash walk.
    // pragmaFunclistLine hides internal functions and inverts SQLITE_FUNC_UNSAFE.
    let mut rows = [("", "", 0, 0); 161];
    let mut builtin = 0;
    let mut row = 0;
    while builtin < BUILTIN_FUNCS.len() {
        let (name, arity, flags, _, aggregate, window) = BUILTIN_FUNCS[builtin];
        if flags & SQLITE_FUNC_INTERNAL == 0 {
            let kind = if window { "w" } else if aggregate { "a" } else { "s" };
            rows[row] = (
                name,
                kind,
                arity as i64,
                ((flags & MASK) ^ FunctionFlags::INNOCUOUS.0) as i64,
            );
            row += 1;
        }
        builtin += 1;
    }
    assert!(row == rows.len());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NullTable;

    impl VirtualTable for NullTable {
        fn best_index(&self, _info: &mut VTabIndexInfo) -> Result<()> {
            Ok(())
        }

        fn open(&self) -> Result<Box<dyn VirtualCursor>> {
            Err(SqliteError::new(ErrorCode::Error, "no cursor"))
        }
    }

    fn entry(db_name: &str, table_name: &str) -> ConnectedVTabEntry {
        let (table_schema, hidden_cols) =
            parse_declare_vtab_sql(table_name, "CREATE TABLE x(a)").unwrap();
        ConnectedVTabEntry {
            db_name: db_name.to_owned(),
            table_name: table_name.to_owned(),
            module_name: "null".to_owned(),
            module_risk_flags: VTabRiskFlags::Normal,
            is_eponymous: false,
            is_eponymous_only: false,
            is_read_only: false,
            shadow_suffixes: Vec::new(),
            declared: Arc::new(VTabDeclaredSchema {
                table_schema,
                hidden_cols,
                config_risk_flags: None,
                constraint_support: false,
                find_function: false,
                nochange_unread: false,
                declare_sql: String::new(),
            }),
            state: VTabInstanceState::Available(Box::new(NullTable)),
            in_tx: false,
            tx_seq: 0,
        }
    }

    /// The tables of a transaction are reported in the order in which they joined it, whatever
    /// order the registry's hash map happens to iterate in (every `VTabRegistry` has its own
    /// hash seed, so several registries are tried).
    #[test]
    fn transaction_fan_out_follows_join_order() {
        for round in 0..16usize {
            let mut reg = VTabRegistry::new();
            let names: Vec<String> = (0..24usize)
                .map(|i| format!("T{}", (i * 7 + round) % 24))
                .collect();
            for (i, name) in names.iter().enumerate() {
                reg.insert_connected(entry(if i % 5 == 0 { "temp" } else { "main" }, name));
            }
            assert!(reg.active_tx_vtabs().is_empty());
            assert!(!reg.has_any_in_tx());

            // Join in an order unrelated to the connection order, leaving some tables out.
            let mut want: Vec<(String, String)> = Vec::new();
            for (i, name) in names.iter().enumerate().rev().step_by(2) {
                let db_name = if i % 5 == 0 { "temp" } else { "main" };
                // Lookups are case-insensitive; the stored spelling is reported.
                reg.mark_joined_tx(&db_name.to_ascii_uppercase(), &name.to_ascii_lowercase());
                want.push((db_name.to_owned(), name.clone()));
            }
            assert!(reg.has_any_in_tx());
            assert_eq!(reg.active_tx_vtabs(), want, "round {round}");
            for _ in 0..2 {
                let taken = reg.take_active_tx_vtabs();
                let got: Vec<(String, String)> = taken
                    .iter()
                    .map(|(_, db_name, table_name)| (db_name.clone(), table_name.clone()))
                    .collect();
                assert_eq!(got, want, "round {round}");
                reg.recycle_tx_vtab_names(taken);
            }

            // A table that leaves the transaction and joins the next one goes to the back.
            let (first_db, first_name) = want.remove(0);
            reg.get_connected_mut(&first_db, &first_name).unwrap().in_tx = false;
            assert_eq!(reg.active_tx_vtabs(), want, "round {round}");
            reg.mark_joined_tx(&first_db, &first_name);
            want.push((first_db, first_name));
            assert_eq!(reg.active_tx_vtabs(), want, "round {round}");
            let taken = reg.take_active_tx_vtabs();
            let got: Vec<(String, String)> = taken
                .iter()
                .map(|(_, db_name, table_name)| (db_name.clone(), table_name.clone()))
                .collect();
            assert_eq!(got, want, "round {round}");
            reg.recycle_tx_vtab_names(taken);

            // Unknown tables do not consume a position.
            reg.mark_joined_tx("main", "no_such_table");
            assert_eq!(reg.active_tx_vtabs(), want, "round {round}");
        }
    }
}
