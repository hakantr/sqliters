//! Idiomatic Rust driver callback & hook layer (`sqlite3_set_authorizer`, `sqlite3_update_hook`,
//! `sqlite3_commit_hook`, `sqlite3_rollback_hook`, `sqlite3_trace_v2`, `sqlite3_busy_handler`,
//! `sqlite3_progress_handler`, scalar/aggregate/window UDFs, and custom collations) with
//! `std::panic::catch_unwind` isolation and reentrancy protection (`sqlite/src/auth.c`, `main.c`).
#![forbid(unsafe_code)]

use std::collections::HashMap;

use sqliters_syntax::{
    AlterTableAction, BinaryOp, ColumnRef, CommonTableExpr, CreateIndexStmt, CreateTableStmt,
    CreateTriggerStmt, CreateViewStmt, DeleteStmt, Expr, FkAction, FromClause, FunctionArgs,
    InsertSource, InsertStmt, JoinConstraint, LikeOp, PragmaValue, QualifiedName, ResultColumn,
    SelectCore, SelectStmt, SetAssignment, Stmt, TableOrSubquery, TriggerEvent, TriggerStep,
    UpdateStmt, UpsertAction, WindowFrameBound, WindowRef, WindowSpec,
};
use sqliters_types::{DatabaseId, ErrorCode, Result, SqliteError};

use crate::db_set::{DatabaseSet, ResolvedRelation};
use crate::execution_context::{FunctionRegistry, catch_infallible_callback_panic};
use crate::schema::{ForeignKeySchema, Schema, TableSchema};
use crate::trigger::TriggerEventKind;

/// Action code passed as the first parameter to an [`AuthorizerCallback`] (`sqlite3_set_authorizer`
/// in `sqlite.h.in:3113-3148`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum AuthorizerAction {
    Copy = 0,
    CreateIndex = 1,
    CreateTable = 2,
    CreateTempIndex = 3,
    CreateTempTable = 4,
    CreateTempTrigger = 5,
    CreateTempView = 6,
    CreateTrigger = 7,
    CreateView = 8,
    Delete = 9,
    DropIndex = 10,
    DropTable = 11,
    DropTempIndex = 12,
    DropTempTable = 13,
    DropTempTrigger = 14,
    DropTempView = 15,
    DropTrigger = 16,
    DropView = 17,
    Insert = 18,
    Pragma = 19,
    Read = 20,
    Select = 21,
    Transaction = 22,
    Update = 23,
    Attach = 24,
    Detach = 25,
    AlterTable = 26,
    Reindex = 27,
    Analyze = 28,
    CreateVtable = 29,
    DropVtable = 30,
    Function = 31,
    Savepoint = 32,
    Recursive = 33,
}

impl AuthorizerAction {
    /// Returns the integer action code matching `SQLITE_CREATE_INDEX..=SQLITE_RECURSIVE`.
    #[inline]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Converts a raw integer code (`0..=33`) into an [`AuthorizerAction`].
    #[inline]
    pub const fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Copy),
            1 => Some(Self::CreateIndex),
            2 => Some(Self::CreateTable),
            3 => Some(Self::CreateTempIndex),
            4 => Some(Self::CreateTempTable),
            5 => Some(Self::CreateTempTrigger),
            6 => Some(Self::CreateTempView),
            7 => Some(Self::CreateTrigger),
            8 => Some(Self::CreateView),
            9 => Some(Self::Delete),
            10 => Some(Self::DropIndex),
            11 => Some(Self::DropTable),
            12 => Some(Self::DropTempIndex),
            13 => Some(Self::DropTempTable),
            14 => Some(Self::DropTempTrigger),
            15 => Some(Self::DropTempView),
            16 => Some(Self::DropTrigger),
            17 => Some(Self::DropView),
            18 => Some(Self::Insert),
            19 => Some(Self::Pragma),
            20 => Some(Self::Read),
            21 => Some(Self::Select),
            22 => Some(Self::Transaction),
            23 => Some(Self::Update),
            24 => Some(Self::Attach),
            25 => Some(Self::Detach),
            26 => Some(Self::AlterTable),
            27 => Some(Self::Reindex),
            28 => Some(Self::Analyze),
            29 => Some(Self::CreateVtable),
            30 => Some(Self::DropVtable),
            31 => Some(Self::Function),
            32 => Some(Self::Savepoint),
            33 => Some(Self::Recursive),
            _ => None,
        }
    }

    /// Returns the canonical constant name (`"SQLITE_SELECT"`, `"SQLITE_READ"`, etc.).
    #[inline]
    pub const fn as_tcl_str(self) -> &'static str {
        match self {
            Self::Copy => "SQLITE_COPY",
            Self::CreateIndex => "SQLITE_CREATE_INDEX",
            Self::CreateTable => "SQLITE_CREATE_TABLE",
            Self::CreateTempIndex => "SQLITE_CREATE_TEMP_INDEX",
            Self::CreateTempTable => "SQLITE_CREATE_TEMP_TABLE",
            Self::CreateTempTrigger => "SQLITE_CREATE_TEMP_TRIGGER",
            Self::CreateTempView => "SQLITE_CREATE_TEMP_VIEW",
            Self::CreateTrigger => "SQLITE_CREATE_TRIGGER",
            Self::CreateView => "SQLITE_CREATE_VIEW",
            Self::Delete => "SQLITE_DELETE",
            Self::DropIndex => "SQLITE_DROP_INDEX",
            Self::DropTable => "SQLITE_DROP_TABLE",
            Self::DropTempIndex => "SQLITE_DROP_TEMP_INDEX",
            Self::DropTempTable => "SQLITE_DROP_TEMP_TABLE",
            Self::DropTempTrigger => "SQLITE_DROP_TEMP_TRIGGER",
            Self::DropTempView => "SQLITE_DROP_TEMP_VIEW",
            Self::DropTrigger => "SQLITE_DROP_TRIGGER",
            Self::DropView => "SQLITE_DROP_VIEW",
            Self::Insert => "SQLITE_INSERT",
            Self::Pragma => "SQLITE_PRAGMA",
            Self::Read => "SQLITE_READ",
            Self::Select => "SQLITE_SELECT",
            Self::Transaction => "SQLITE_TRANSACTION",
            Self::Update => "SQLITE_UPDATE",
            Self::Attach => "SQLITE_ATTACH",
            Self::Detach => "SQLITE_DETACH",
            Self::AlterTable => "SQLITE_ALTER_TABLE",
            Self::Reindex => "SQLITE_REINDEX",
            Self::Analyze => "SQLITE_ANALYZE",
            Self::CreateVtable => "SQLITE_CREATE_VTABLE",
            Self::DropVtable => "SQLITE_DROP_VTABLE",
            Self::Function => "SQLITE_FUNCTION",
            Self::Savepoint => "SQLITE_SAVEPOINT",
            Self::Recursive => "SQLITE_RECURSIVE",
        }
    }
}

/// Return value of an [`AuthorizerCallback`] (`SQLITE_OK = 0`, `SQLITE_DENY = 1`, `SQLITE_IGNORE = 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum AuthorizerResult {
    /// Allow the operation normally (`SQLITE_OK`).
    Ok = 0,
    /// Abort statement compilation with [`ErrorCode::Auth`] (`SQLITE_DENY`).
    Deny = 1,
    /// Mask the specific column read (`NULL`), column update, function call (`NULL`),
    /// `DELETE` truncate fast-path, or statement (`SQLITE_IGNORE`).
    Ignore = 2,
}

/// Connection-level compile-time authorization callback (`sqlite3_set_authorizer`).
///
/// Arguments are `(action, arg1, arg2, db_name, inner_most_trigger_or_view)`.
pub type AuthorizerCallback = Box<
    dyn FnMut(
            AuthorizerAction,
            Option<&str>,
            Option<&str>,
            Option<&str>,
            Option<&str>,
        ) -> AuthorizerResult
        + Send,
>;

/// Connection-owned slot holding an optional [`AuthorizerCallback`].
#[derive(Default)]
pub struct AuthorizerSlot {
    callback: Option<AuthorizerCallback>,
}

impl std::fmt::Debug for AuthorizerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizerSlot")
            .field("has_callback", &self.callback.is_some())
            .finish()
    }
}

impl AuthorizerSlot {
    /// Returns `true` if an authorizer callback is currently registered.
    #[inline]
    pub fn is_some(&self) -> bool {
        self.callback.is_some()
    }

    /// Installs or clears the authorizer callback.
    #[inline]
    pub fn set(&mut self, callback: Option<AuthorizerCallback>) {
        self.callback = callback;
    }

    /// Invokes the authorizer callback with panic isolation (`sqlite3AuthCheck` in `auth.c:199-250`).
    pub fn check(
        &mut self,
        action: AuthorizerAction,
        arg1: Option<&str>,
        arg2: Option<&str>,
        db_name: Option<&str>,
        auth_context: Option<&str>,
    ) -> Result<AuthorizerResult> {
        let Some(ref mut cb) = self.callback else {
            return Ok(AuthorizerResult::Ok);
        };
        let res = catch_infallible_callback_panic("authorizer", || {
            cb(action, arg1, arg2, db_name, auth_context)
        })?;
        if res == AuthorizerResult::Deny {
            return Err(SqliteError::new(ErrorCode::Auth, "not authorized"));
        }
        Ok(res)
    }

    /// Invokes the authorizer callback for a column read (`sqlite3AuthReadCol` in `auth.c:104-125`).
    pub fn check_read(
        &mut self,
        table_name: &str,
        column_name: &str,
        db_name: &str,
        has_attached_or_non_main: bool,
        auth_context: Option<&str>,
    ) -> Result<AuthorizerResult> {
        let Some(ref mut cb) = self.callback else {
            return Ok(AuthorizerResult::Ok);
        };
        let res = catch_infallible_callback_panic("authorizer", || {
            cb(
                AuthorizerAction::Read,
                Some(table_name),
                Some(column_name),
                Some(db_name),
                auth_context,
            )
        })?;
        if res == AuthorizerResult::Deny {
            let target = if has_attached_or_non_main {
                format!("{db_name}.{table_name}.{column_name}")
            } else {
                format!("{table_name}.{column_name}")
            };
            return Err(SqliteError::new(
                ErrorCode::Auth,
                format!("access to {target} is prohibited"),
            ));
        }
        Ok(res)
    }
}

#[derive(Clone, Debug)]
struct AuthSourceTable {
    db_id: Option<DatabaseId>,
    db_name: Option<String>,
    table_name: String,
    alias: Option<String>,
    columns: Vec<String>,
    has_rowid: bool,
    ipk_column: Option<String>,
    /// `true` for physical tables and views (which fire `SQLITE_READ`), `false` for CTEs and subqueries.
    fires_read: bool,
    /// `true` for `OLD` / `NEW` trigger pseudo-relations which only match qualified `OLD.x` / `NEW.x`.
    qualified_only: bool,
}

impl AuthSourceTable {
    fn matches_qualifier(&self, schema_q: Option<&str>, table_q: &str) -> bool {
        if let Some(sq) = schema_q {
            if self.alias.is_some() {
                return false;
            }
            let Some(ref dbn) = self.db_name else {
                return false;
            };
            if !dbn.eq_ignore_ascii_case(sq) {
                return false;
            }
        }
        if let Some(ref al) = self.alias {
            al.eq_ignore_ascii_case(table_q)
        } else {
            self.table_name.eq_ignore_ascii_case(table_q)
        }
    }

    fn resolve_column(&self, col: &str) -> Option<String> {
        if let Some(found) = self.columns.iter().find(|c| c.eq_ignore_ascii_case(col)) {
            return Some(found.clone());
        }
        if self.has_rowid && is_rowid_alias(col) {
            if let Some(ref ipk) = self.ipk_column {
                return Some(ipk.clone());
            }
            return Some("ROWID".to_owned());
        }
        None
    }
}

fn is_rowid_alias(name: &str) -> bool {
    name.eq_ignore_ascii_case("rowid")
        || name.eq_ignore_ascii_case("_rowid_")
        || name.eq_ignore_ascii_case("oid")
}

struct AuthWalkCtx<'a> {
    db_set: &'a DatabaseSet,
    functions: &'a FunctionRegistry,
    slot: &'a mut AuthorizerSlot,
    foreign_keys: bool,
    in_subprogram: bool,
    context_stack: Vec<String>,
    visited_views: Vec<(DatabaseId, String)>,
    visited_triggers: Vec<(DatabaseId, String)>,
    visited_fk_actions: Vec<(DatabaseId, String, usize, bool)>,
    ignored_fk_parent_tables: Vec<(DatabaseId, String)>,
}

impl<'a> AuthWalkCtx<'a> {
    #[inline]
    fn current_context(&self) -> Option<&str> {
        self.context_stack.last().map(String::as_str)
    }

    fn check(
        &mut self,
        action: AuthorizerAction,
        arg1: Option<&str>,
        arg2: Option<&str>,
        db_name: Option<&str>,
    ) -> Result<AuthorizerResult> {
        let z_ctx = self.context_stack.last().cloned();
        self.slot
            .check(action, arg1, arg2, db_name, z_ctx.as_deref())
    }

    fn check_read(
        &mut self,
        db_id: DatabaseId,
        db_name: &str,
        table_name: &str,
        col_name: &str,
    ) -> Result<AuthorizerResult> {
        let z_ctx = self.context_stack.last().cloned();
        let has_attached = self.db_set.slot_count() > 2 || db_id != DatabaseId::MAIN;
        self.slot.check_read(
            table_name,
            col_name,
            db_name,
            has_attached,
            z_ctx.as_deref(),
        )
    }

    fn slot_name(&self, db_id: DatabaseId) -> String {
        self.db_set
            .slot(db_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "main".to_owned())
    }

    fn resolve_source_relation(
        &mut self,
        qn: &QualifiedName,
        alias: Option<&str>,
        ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<(AuthSourceTable, Option<SelectStmt>)> {
        if qn.schema.is_none()
            && let Some(cte_cols) = ctes
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&qn.name))
                .map(|(_, v)| v.clone())
        {
            return Ok((
                AuthSourceTable {
                    db_id: None,
                    db_name: None,
                    table_name: qn.name.clone(),
                    alias: alias.map(str::to_owned),
                    columns: cte_cols,
                    has_rowid: false,
                    ipk_column: None,
                    fires_read: false,
                    qualified_only: false,
                },
                None,
            ));
        }

        if let Ok(Some(cat_db)) = self.db_set.resolve_catalog_table(qn) {
            let db_name = self.slot_name(cat_db);
            let tbl_name = if cat_db == DatabaseId::TEMP {
                "sqlite_temp_master".to_owned()
            } else {
                "sqlite_master".to_owned()
            };
            return Ok((
                AuthSourceTable {
                    db_id: Some(cat_db),
                    db_name: Some(db_name),
                    table_name: tbl_name,
                    alias: alias.map(str::to_owned),
                    columns: vec![
                        "type".to_owned(),
                        "name".to_owned(),
                        "tbl_name".to_owned(),
                        "rootpage".to_owned(),
                        "sql".to_owned(),
                    ],
                    has_rowid: true,
                    ipk_column: None,
                    fires_read: true,
                    qualified_only: false,
                },
                None,
            ));
        }

        if let Ok(Some((db_id, rel))) = self.db_set.find_relation_in_set(qn) {
            let db_name = self.slot_name(db_id);
            match rel {
                ResolvedRelation::Table(tbl) => {
                    let ipk = tbl
                        .rowid_alias_col
                        .and_then(|idx| tbl.columns.get(idx))
                        .map(|c| c.name.clone());
                    let cols = tbl.columns.iter().map(|c| c.name.clone()).collect();
                    return Ok((
                        AuthSourceTable {
                            db_id: Some(db_id),
                            db_name: Some(db_name),
                            table_name: tbl.name.clone(),
                            alias: alias.map(str::to_owned),
                            columns: cols,
                            has_rowid: !tbl.without_rowid,
                            ipk_column: ipk,
                            fires_read: true,
                            qualified_only: false,
                        },
                        None,
                    ));
                }
                ResolvedRelation::View(view) => {
                    let visit_key = (db_id, view.name.to_ascii_uppercase());
                    let mut rewritten_view_select = None;
                    let mut inferred_cols = view.columns.clone().unwrap_or_default();
                    if !self.visited_views.contains(&visit_key) {
                        self.visited_views.push(visit_key);
                        self.context_stack.push(view.name.clone());
                        let mut view_sel = view.select.clone();
                        let before_sel = view_sel.clone();
                        let auth_res = self.authorize_select_stmt(&mut view_sel, outer_scopes);
                        self.context_stack.pop();
                        self.visited_views.pop();
                        let keep = auth_res?;
                        if inferred_cols.is_empty() {
                            inferred_cols = infer_select_col_names(&view_sel, self.db_set);
                        }
                        if !keep {
                            rewritten_view_select =
                                Some(make_null_select_for_columns(&inferred_cols));
                        } else if view_sel != before_sel {
                            rewritten_view_select = Some(view_sel);
                        }
                    } else if inferred_cols.is_empty() {
                        inferred_cols = infer_select_col_names(&view.select, self.db_set);
                    }
                    return Ok((
                        AuthSourceTable {
                            db_id: Some(db_id),
                            db_name: Some(db_name),
                            table_name: view.name.clone(),
                            alias: alias.map(str::to_owned),
                            columns: inferred_cols,
                            has_rowid: false,
                            ipk_column: None,
                            fires_read: true,
                            qualified_only: false,
                        },
                        rewritten_view_select,
                    ));
                }
            }
        }

        let db_id = qn
            .schema
            .as_deref()
            .and_then(|s| self.db_set.find_db_id(s))
            .unwrap_or(DatabaseId::MAIN);
        let db_name = self.slot_name(db_id);
        Ok((
            AuthSourceTable {
                db_id: Some(db_id),
                db_name: Some(db_name),
                table_name: qn.name.clone(),
                alias: alias.map(str::to_owned),
                columns: Vec::new(),
                has_rowid: true,
                ipk_column: None,
                fires_read: true,
                qualified_only: false,
            },
            None,
        ))
    }

    fn authorize_stmt(
        &mut self,
        stmt: &mut Stmt,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        match stmt {
            Stmt::Explain { stmt: inner, .. } => self.authorize_stmt(inner, outer_scopes),
            Stmt::Select(sel) => self.authorize_select_stmt(sel, outer_scopes),
            Stmt::Insert(ins) => self.authorize_insert_stmt(ins, outer_scopes),
            Stmt::Update(upd) => self.authorize_update_stmt(upd, outer_scopes),
            Stmt::Delete(del) => self.authorize_delete_stmt(del, outer_scopes),
            Stmt::CreateTable(ct) => self.authorize_create_table(ct),
            Stmt::CreateTableAs {
                temp, name, query, ..
            } => {
                let db_id = if *temp
                    || name
                        .schema
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
                {
                    DatabaseId::TEMP
                } else {
                    name.schema
                        .as_deref()
                        .and_then(|s| self.db_set.find_db_id(s))
                        .unwrap_or(DatabaseId::MAIN)
                };
                let db_name = self.slot_name(db_id);
                let action = if db_id == DatabaseId::TEMP {
                    AuthorizerAction::CreateTempTable
                } else {
                    AuthorizerAction::CreateTable
                };
                if self.check(action, Some(&name.name), None, Some(&db_name))?
                    == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                self.authorize_select_stmt(query, outer_scopes)
            }
            Stmt::CreateVirtualTable {
                name, module_name, ..
            } => {
                let db_id = name
                    .schema
                    .as_deref()
                    .and_then(|s| self.db_set.find_db_id(s))
                    .unwrap_or(DatabaseId::MAIN);
                let db_name = self.slot_name(db_id);
                if self.check(
                    AuthorizerAction::CreateVtable,
                    Some(&name.name),
                    Some(module_name),
                    Some(&db_name),
                )? == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                Ok(true)
            }
            Stmt::DropTable { name, .. } => {
                let db_id = if let Some(ref s) = name.schema {
                    self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN)
                } else {
                    self.db_set
                        .find_relation_in_set(name)
                        .ok()
                        .flatten()
                        .map(|(id, _)| id)
                        .unwrap_or(DatabaseId::MAIN)
                };
                let db_name = self.slot_name(db_id);
                let action = if db_id == DatabaseId::TEMP {
                    AuthorizerAction::DropTempTable
                } else {
                    AuthorizerAction::DropTable
                };
                if self.check(action, Some(&name.name), None, Some(&db_name))?
                    == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                if let Some(slot) = self.db_set.slot(db_id)
                    && slot.schema.find_user_table("sqlite_stat1").is_some()
                {
                    for stat_tbl in [
                        "sqlite_stat1",
                        "sqlite_stat2",
                        "sqlite_stat3",
                        "sqlite_stat4",
                    ] {
                        if slot.schema.find_user_table(stat_tbl).is_some() {
                            let _ = self.check(
                                AuthorizerAction::Delete,
                                Some(stat_tbl),
                                None,
                                Some(&db_name),
                            )?;
                        }
                    }
                }
                Ok(true)
            }
            Stmt::CreateIndex(ci) => self.authorize_create_index(ci),
            Stmt::DropIndex { name, .. } => {
                let (db_id, tbl_name) = if let Some(ref s) = name.schema {
                    let id = self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN);
                    let tname = self
                        .db_set
                        .slot(id)
                        .and_then(|sl| sl.schema.find_index(&name.name))
                        .map(|ix| ix.table_name.clone())
                        .unwrap_or_default();
                    (id, tname)
                } else if let Some((id, ix)) = self
                    .db_set
                    .find_index_including_without_rowid_pk_in_set(name)
                {
                    (id, ix.table_name)
                } else {
                    (DatabaseId::MAIN, String::new())
                };
                let db_name = self.slot_name(db_id);
                let action = if db_id == DatabaseId::TEMP {
                    AuthorizerAction::DropTempIndex
                } else {
                    AuthorizerAction::DropIndex
                };
                let arg2 = (!tbl_name.is_empty()).then_some(tbl_name.as_str());
                Ok(self.check(action, Some(&name.name), arg2, Some(&db_name))?
                    != AuthorizerResult::Ignore)
            }
            Stmt::CreateView(cv) => self.authorize_create_view(cv),
            Stmt::DropView { name, .. } => {
                let db_id = if let Some(ref s) = name.schema {
                    self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN)
                } else {
                    self.db_set
                        .find_relation_in_set(name)
                        .ok()
                        .flatten()
                        .map(|(id, _)| id)
                        .unwrap_or(DatabaseId::MAIN)
                };
                let db_name = self.slot_name(db_id);
                let action = if db_id == DatabaseId::TEMP {
                    AuthorizerAction::DropTempView
                } else {
                    AuthorizerAction::DropView
                };
                Ok(self.check(action, Some(&name.name), None, Some(&db_name))?
                    != AuthorizerResult::Ignore)
            }
            Stmt::CreateTrigger(ct) => self.authorize_create_trigger(ct),
            Stmt::DropTrigger { name, .. } => {
                let (db_id, tbl_name) =
                    if let Some((id, tr)) = self.db_set.find_trigger_in_set(name) {
                        (id, tr.tbl_name.clone())
                    } else {
                        let id = name
                            .schema
                            .as_deref()
                            .and_then(|s| self.db_set.find_db_id(s))
                            .unwrap_or(DatabaseId::MAIN);
                        (id, String::new())
                    };
                let db_name = self.slot_name(db_id);
                let action = if db_id == DatabaseId::TEMP {
                    AuthorizerAction::DropTempTrigger
                } else {
                    AuthorizerAction::DropTrigger
                };
                let arg2 = (!tbl_name.is_empty()).then_some(tbl_name.as_str());
                Ok(self.check(action, Some(&name.name), arg2, Some(&db_name))?
                    != AuthorizerResult::Ignore)
            }
            Stmt::AlterTable { table, action } => {
                let db_id = if let Some(ref s) = table.schema {
                    self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN)
                } else {
                    self.db_set
                        .find_relation_in_set(table)
                        .ok()
                        .flatten()
                        .map(|(id, _)| id)
                        .unwrap_or(DatabaseId::MAIN)
                };
                let db_name = self.slot_name(db_id);
                if self.check(
                    AuthorizerAction::AlterTable,
                    Some(&db_name),
                    Some(&table.name),
                    None,
                )? == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                if let AlterTableAction::AddColumn(col_def) = action {
                    let empty_ctes = HashMap::new();
                    self.authorize_column_def(col_def, &empty_ctes, outer_scopes)?;
                }
                Ok(true)
            }
            Stmt::Pragma {
                schema,
                name,
                value,
            } => {
                let db_id = schema
                    .as_deref()
                    .and_then(|s| self.db_set.find_db_id(s))
                    .unwrap_or(DatabaseId::MAIN);
                let db_name = self.slot_name(db_id);
                let val_str = value.as_ref().map(|pv| match pv {
                    PragmaValue::Ident(s) | PragmaValue::String(s) => s.clone(),
                    PragmaValue::Integer(i) => i.to_string(),
                });
                Ok(self.check(
                    AuthorizerAction::Pragma,
                    Some(name),
                    val_str.as_deref(),
                    Some(&db_name),
                )? != AuthorizerResult::Ignore)
            }
            Stmt::Begin { .. } => {
                Ok(
                    self.check(AuthorizerAction::Transaction, Some("BEGIN"), None, None)?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Commit => {
                Ok(
                    self.check(AuthorizerAction::Transaction, Some("COMMIT"), None, None)?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Rollback { savepoint: None } => {
                Ok(
                    self.check(AuthorizerAction::Transaction, Some("ROLLBACK"), None, None)?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Savepoint(name) => {
                Ok(
                    self.check(AuthorizerAction::Savepoint, Some("BEGIN"), Some(name), None)?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Release(name) => Ok(self.check(
                AuthorizerAction::Savepoint,
                Some("RELEASE"),
                Some(name),
                None,
            )? != AuthorizerResult::Ignore),
            Stmt::Rollback {
                savepoint: Some(name),
            } => Ok(self.check(
                AuthorizerAction::Savepoint,
                Some("ROLLBACK"),
                Some(name),
                None,
            )? != AuthorizerResult::Ignore),
            Stmt::AttachDatabase {
                expr,
                schema_name,
                key,
            } => {
                let filename = match expr {
                    Expr::String(s) => Some(s.clone()),
                    _ => None,
                };
                if self.check(AuthorizerAction::Attach, filename.as_deref(), None, None)?
                    == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                let empty_ctes = HashMap::new();
                self.authorize_expr(expr, &empty_ctes, outer_scopes)?;
                self.authorize_expr(schema_name, &empty_ctes, outer_scopes)?;
                if let Some(k) = key {
                    self.authorize_expr(k, &empty_ctes, outer_scopes)?;
                }
                Ok(true)
            }
            Stmt::DetachDatabase { schema_name } => {
                let db_arg = match schema_name {
                    Expr::String(s) => Some(s.clone()),
                    Expr::Column(c) if c.schema.is_none() && c.table.is_none() => {
                        Some(c.column.clone())
                    }
                    _ => None,
                };
                if self.check(AuthorizerAction::Detach, db_arg.as_deref(), None, None)?
                    == AuthorizerResult::Ignore
                {
                    return Ok(false);
                }
                let empty_ctes = HashMap::new();
                self.authorize_expr(schema_name, &empty_ctes, outer_scopes)?;
                Ok(true)
            }
            Stmt::Reindex(target) => {
                let (arg1, db_name) = match target {
                    Some(qn) => {
                        let db_id = qn
                            .schema
                            .as_deref()
                            .and_then(|s| self.db_set.find_db_id(s))
                            .unwrap_or(DatabaseId::MAIN);
                        (Some(qn.name.as_str()), Some(self.slot_name(db_id)))
                    }
                    None => (None, None),
                };
                Ok(
                    self.check(AuthorizerAction::Reindex, arg1, None, db_name.as_deref())?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Analyze(target) => {
                let (arg1, db_name) = match target {
                    Some(qn) => {
                        let db_id = qn
                            .schema
                            .as_deref()
                            .and_then(|s| self.db_set.find_db_id(s))
                            .unwrap_or(DatabaseId::MAIN);
                        (Some(qn.name.as_str()), Some(self.slot_name(db_id)))
                    }
                    None => (None, Some("main".to_owned())),
                };
                Ok(
                    self.check(AuthorizerAction::Analyze, arg1, None, db_name.as_deref())?
                        != AuthorizerResult::Ignore,
                )
            }
            Stmt::Vacuum { into, .. } => {
                if let Some(e) = into {
                    let empty_ctes = HashMap::new();
                    self.authorize_expr(e, &empty_ctes, outer_scopes)?;
                }
                Ok(true)
            }
        }
    }

    fn authorize_create_table(&mut self, ct: &mut CreateTableStmt) -> Result<bool> {
        let db_id = if ct.temp
            || ct
                .name
                .schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
        {
            DatabaseId::TEMP
        } else {
            ct.name
                .schema
                .as_deref()
                .and_then(|s| self.db_set.find_db_id(s))
                .unwrap_or(DatabaseId::MAIN)
        };
        let db_name = self.slot_name(db_id);
        let action = if db_id == DatabaseId::TEMP {
            AuthorizerAction::CreateTempTable
        } else {
            AuthorizerAction::CreateTable
        };
        if self.check(action, Some(&ct.name.name), None, Some(&db_name))?
            == AuthorizerResult::Ignore
        {
            return Ok(false);
        }
        let empty_ctes = HashMap::new();
        let cols: Vec<String> = ct.columns.iter().map(|c| c.name.clone()).collect();
        let self_scope = vec![AuthSourceTable {
            db_id: Some(db_id),
            db_name: Some(db_name),
            table_name: ct.name.name.clone(),
            alias: None,
            columns: cols,
            has_rowid: !ct.without_rowid,
            ipk_column: None,
            fires_read: false,
            qualified_only: false,
        }];
        for col_def in &mut ct.columns {
            self.authorize_column_def(col_def, &empty_ctes, std::slice::from_ref(&self_scope))?;
        }
        for tc in &mut ct.constraints {
            if let sqliters_syntax::TableConstraint::Check { expr, .. } = tc {
                self.authorize_expr(expr, &empty_ctes, std::slice::from_ref(&self_scope))?;
            }
        }
        Ok(true)
    }

    fn authorize_column_def(
        &mut self,
        col_def: &mut sqliters_syntax::ColumnDef,
        ctes: &HashMap<String, Vec<String>>,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Result<()> {
        for cc in &mut col_def.constraints {
            match cc {
                sqliters_syntax::ColumnConstraint::Default(e)
                | sqliters_syntax::ColumnConstraint::Check { expr: e, .. }
                | sqliters_syntax::ColumnConstraint::Generated { expr: e, .. } => {
                    self.authorize_expr(e, ctes, scopes)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn authorize_create_index(&mut self, ci: &mut CreateIndexStmt) -> Result<bool> {
        let db_id = if let Some(ref s) = ci.name.schema {
            self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN)
        } else {
            self.db_set
                .find_relation_in_set(&QualifiedName::unqualified(ci.table_name.clone()))
                .ok()
                .flatten()
                .map(|(id, _)| id)
                .unwrap_or(DatabaseId::MAIN)
        };
        let db_name = self.slot_name(db_id);
        let action = if db_id == DatabaseId::TEMP {
            AuthorizerAction::CreateTempIndex
        } else {
            AuthorizerAction::CreateIndex
        };
        if self.check(
            action,
            Some(&ci.name.name),
            Some(&ci.table_name),
            Some(&db_name),
        )? == AuthorizerResult::Ignore
        {
            return Ok(false);
        }
        let empty_ctes = HashMap::new();
        let (src, _) = self.resolve_source_relation(
            &QualifiedName {
                schema: Some(db_name),
                name: ci.table_name.clone(),
            },
            None,
            &empty_ctes,
            &[],
        )?;
        let scopes = vec![vec![src]];
        for col in &mut ci.columns {
            if let Some(ref mut e) = col.expr {
                self.authorize_expr(e, &empty_ctes, &scopes)?;
            }
        }
        if let Some(ref mut w) = ci.where_clause {
            self.authorize_expr(w, &empty_ctes, &scopes)?;
        }
        Ok(true)
    }

    fn authorize_create_view(&mut self, cv: &mut CreateViewStmt) -> Result<bool> {
        let db_id = if cv.temp
            || cv
                .name
                .schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
        {
            DatabaseId::TEMP
        } else {
            cv.name
                .schema
                .as_deref()
                .and_then(|s| self.db_set.find_db_id(s))
                .unwrap_or(DatabaseId::MAIN)
        };
        let db_name = self.slot_name(db_id);
        let action = if db_id == DatabaseId::TEMP {
            AuthorizerAction::CreateTempView
        } else {
            AuthorizerAction::CreateView
        };
        Ok(
            self.check(action, Some(&cv.name.name), None, Some(&db_name))?
                != AuthorizerResult::Ignore,
        )
    }

    fn authorize_create_trigger(&mut self, ct: &mut CreateTriggerStmt) -> Result<bool> {
        let db_id = if ct.temp
            || ct
                .name
                .schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
        {
            DatabaseId::TEMP
        } else if let Some(ref s) = ct.name.schema {
            self.db_set.find_db_id(s).unwrap_or(DatabaseId::MAIN)
        } else if matches!(
            self.db_set.find_relation_in_set(&ct.on_table),
            Ok(Some((DatabaseId::TEMP, _)))
        ) {
            DatabaseId::TEMP
        } else {
            DatabaseId::MAIN
        };
        let db_name = self.slot_name(db_id);
        let action = if db_id == DatabaseId::TEMP {
            AuthorizerAction::CreateTempTrigger
        } else {
            AuthorizerAction::CreateTrigger
        };
        Ok(self.check(
            action,
            Some(&ct.name.name),
            Some(&ct.on_table.name),
            Some(&db_name),
        )? != AuthorizerResult::Ignore)
    }

    fn authorize_ctes(
        &mut self,
        ctes: &mut [CommonTableExpr],
        outer_ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<HashMap<String, Vec<String>>> {
        let mut local_ctes = outer_ctes.clone();
        if ctes.iter().any(|c| !c.query.body.compounds.is_empty()) {
            let _ = self.check(AuthorizerAction::Recursive, None, None, None)?;
        }
        for cte in ctes.iter_mut() {
            let cols = cte
                .columns
                .clone()
                .unwrap_or_else(|| infer_select_col_names(&cte.query, self.db_set));
            local_ctes.insert(cte.name.clone(), cols);
        }
        for cte in ctes.iter_mut() {
            let _ =
                self.authorize_select_stmt_with_ctes(&mut cte.query, &local_ctes, outer_scopes)?;
        }
        Ok(local_ctes)
    }

    fn authorize_select_stmt(
        &mut self,
        sel: &mut SelectStmt,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        let empty_ctes = HashMap::new();
        self.authorize_select_stmt_with_ctes(sel, &empty_ctes, outer_scopes)
    }

    fn authorize_select_stmt_with_ctes(
        &mut self,
        sel: &mut SelectStmt,
        outer_ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        if self.check(AuthorizerAction::Select, None, None, None)? == AuthorizerResult::Ignore {
            return Ok(false);
        }
        let local_ctes = self.authorize_ctes(&mut sel.ctes, outer_ctes, outer_scopes)?;

        let head_scopes =
            self.authorize_select_core(&mut sel.body.head, &local_ctes, outer_scopes)?;
        for (_, core) in &mut sel.body.compounds {
            let _ = self.authorize_select_core(core, &local_ctes, outer_scopes)?;
        }

        let mut order_scopes = Vec::with_capacity(outer_scopes.len() + 1);
        order_scopes.extend_from_slice(outer_scopes);
        order_scopes.push(head_scopes);
        for ob in &mut sel.order_by {
            self.authorize_expr(&mut ob.expr, &local_ctes, &order_scopes)?;
        }
        if let Some(ref mut lim) = sel.limit {
            self.authorize_expr(&mut lim.limit, &local_ctes, outer_scopes)?;
            if let Some(ref mut off) = lim.offset {
                self.authorize_expr(off, &local_ctes, outer_scopes)?;
            }
        }
        Ok(true)
    }

    fn authorize_select_core(
        &mut self,
        core: &mut SelectCore,
        ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<Vec<AuthSourceTable>> {
        match core {
            SelectCore::Values(rows) => {
                for r in rows {
                    for e in r {
                        self.authorize_expr(e, ctes, outer_scopes)?;
                    }
                }
                Ok(Vec::new())
            }
            SelectCore::Select {
                projection,
                from,
                selection,
                group_by,
                having,
                windows,
                ..
            } => {
                let mut current_scope = Vec::new();
                if let Some(fc) = from {
                    current_scope = self.authorize_from_clause(fc, ctes, outer_scopes)?;
                }
                let mut scopes = Vec::with_capacity(outer_scopes.len() + 1);
                scopes.extend_from_slice(outer_scopes);
                scopes.push(current_scope.clone());

                self.authorize_projection(projection, &current_scope, ctes, &scopes)?;

                if let Some(w) = selection {
                    self.authorize_expr(w, ctes, &scopes)?;
                }
                for g in group_by {
                    self.authorize_expr(g, ctes, &scopes)?;
                }
                if let Some(h) = having {
                    self.authorize_expr(h, ctes, &scopes)?;
                }
                for (_, ws) in windows {
                    self.authorize_window_spec(ws, ctes, &scopes)?;
                }
                Ok(current_scope)
            }
        }
    }

    fn authorize_from_clause(
        &mut self,
        fc: &mut FromClause,
        ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<Vec<AuthSourceTable>> {
        let mut tables = Vec::with_capacity(1 + fc.joins.len());
        tables.push(self.authorize_table_or_subquery(&mut fc.head, ctes, outer_scopes)?);
        for join in &mut fc.joins {
            tables.push(self.authorize_table_or_subquery(&mut join.right, ctes, outer_scopes)?);
            let mut scopes = Vec::with_capacity(outer_scopes.len() + 1);
            scopes.extend_from_slice(outer_scopes);
            scopes.push(tables.clone());
            if let Some(ref mut constraint) = join.constraint {
                match constraint {
                    JoinConstraint::On(e) => {
                        self.authorize_expr(e, ctes, &scopes)?;
                    }
                    JoinConstraint::Using(cols) => {
                        for col_name in cols {
                            for src in &tables {
                                if src.fires_read
                                    && let (Some(db_id), Some(db_name)) =
                                        (src.db_id, src.db_name.as_deref())
                                    && let Some(canon) = src.resolve_column(col_name)
                                {
                                    let _ =
                                        self.check_read(db_id, db_name, &src.table_name, &canon)?;
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(tables)
    }

    fn authorize_table_or_subquery(
        &mut self,
        tos: &mut TableOrSubquery,
        ctes: &HashMap<String, Vec<String>>,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<AuthSourceTable> {
        match tos {
            TableOrSubquery::Table {
                name,
                table_args,
                alias,
                ..
            } => {
                if let Some(args) = table_args {
                    for a in args {
                        self.authorize_expr(a, ctes, outer_scopes)?;
                    }
                }
                let (src, rewritten_view) =
                    self.resolve_source_relation(name, alias.as_deref(), ctes, outer_scopes)?;
                if let Some(view_sel) = rewritten_view {
                    let eff_alias = alias.clone().unwrap_or_else(|| src.table_name.clone());
                    *tos = TableOrSubquery::Subquery {
                        query: Box::new(view_sel),
                        alias: Some(eff_alias),
                    };
                }
                Ok(src)
            }
            TableOrSubquery::TableFunction {
                schema,
                name,
                args,
                alias,
            } => {
                for a in args {
                    self.authorize_expr(a, ctes, outer_scopes)?;
                }
                let qn = match schema {
                    Some(s) => QualifiedName::qualified(s.clone(), name.clone()),
                    None => QualifiedName::unqualified(name.clone()),
                };
                let (src, _) =
                    self.resolve_source_relation(&qn, alias.as_deref(), ctes, outer_scopes)?;
                Ok(src)
            }
            TableOrSubquery::Subquery { query, alias } => {
                let keep = self.authorize_select_stmt_with_ctes(query, ctes, outer_scopes)?;
                let cols = infer_select_col_names(query, self.db_set);
                if !keep {
                    **query = make_null_select_for_columns(&cols);
                }
                Ok(AuthSourceTable {
                    db_id: None,
                    db_name: None,
                    table_name: alias.clone().unwrap_or_default(),
                    alias: alias.clone(),
                    columns: cols,
                    has_rowid: false,
                    ipk_column: None,
                    fires_read: false,
                    qualified_only: false,
                })
            }
        }
    }

    fn authorize_projection(
        &mut self,
        projection: &mut Vec<ResultColumn>,
        current_scope: &[AuthSourceTable],
        ctes: &HashMap<String, Vec<String>>,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Result<()> {
        let mut expanded = Vec::with_capacity(projection.len());
        for rc in projection.drain(..) {
            match rc {
                ResultColumn::Star => {
                    let mut any_ignored = false;
                    let mut star_items = Vec::new();
                    for src in current_scope {
                        if src.qualified_only {
                            continue;
                        }
                        let qual = src.alias.clone().unwrap_or_else(|| src.table_name.clone());
                        for col in &src.columns {
                            let mut is_ignored = false;
                            if src.fires_read
                                && let (Some(db_id), Some(db_name)) =
                                    (src.db_id, src.db_name.as_deref())
                                && self.check_read(db_id, db_name, &src.table_name, col)?
                                    == AuthorizerResult::Ignore
                            {
                                is_ignored = true;
                                any_ignored = true;
                            }
                            let expr = if is_ignored {
                                Expr::Null
                            } else {
                                Expr::Column(ColumnRef::table_qualified(qual.clone(), col.clone()))
                            };
                            star_items.push(ResultColumn::Expr {
                                expr,
                                alias: Some(col.clone()),
                                span_text: None,
                            });
                        }
                    }
                    if any_ignored && !star_items.is_empty() {
                        expanded.extend(star_items);
                    } else {
                        expanded.push(ResultColumn::Star);
                    }
                }
                ResultColumn::TableStar {
                    schema,
                    table: tname,
                } => {
                    let mut any_ignored = false;
                    let mut star_items = Vec::new();
                    for src in current_scope {
                        if src.matches_qualifier(schema.as_deref(), &tname) {
                            let qual = src.alias.clone().unwrap_or_else(|| src.table_name.clone());
                            for col in &src.columns {
                                let mut is_ignored = false;
                                if src.fires_read
                                    && let (Some(db_id), Some(db_name)) =
                                        (src.db_id, src.db_name.as_deref())
                                    && self.check_read(db_id, db_name, &src.table_name, col)?
                                        == AuthorizerResult::Ignore
                                {
                                    is_ignored = true;
                                    any_ignored = true;
                                }
                                let expr = if is_ignored {
                                    Expr::Null
                                } else {
                                    Expr::Column(ColumnRef::table_qualified(
                                        qual.clone(),
                                        col.clone(),
                                    ))
                                };
                                star_items.push(ResultColumn::Expr {
                                    expr,
                                    alias: Some(col.clone()),
                                    span_text: None,
                                });
                            }
                        }
                    }
                    if any_ignored && !star_items.is_empty() {
                        expanded.extend(star_items);
                    } else {
                        expanded.push(ResultColumn::TableStar {
                            schema,
                            table: tname,
                        });
                    }
                }
                ResultColumn::Expr {
                    mut expr,
                    mut alias,
                    span_text,
                } => {
                    let orig_col_label = match &expr {
                        Expr::Column(c) if alias.is_none() => Some(c.column.clone()),
                        _ => None,
                    };
                    self.authorize_expr(&mut expr, ctes, scopes)?;
                    if matches!(expr, Expr::Null)
                        && alias.is_none()
                        && let Some(lbl) = orig_col_label
                    {
                        alias = Some(lbl);
                    }
                    expanded.push(ResultColumn::Expr {
                        expr,
                        alias,
                        span_text,
                    });
                }
            }
        }
        *projection = expanded;
        Ok(())
    }

    fn authorize_insert_stmt(
        &mut self,
        ins: &mut InsertStmt,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        let local_ctes = self.authorize_ctes(&mut ins.ctes, &HashMap::new(), outer_scopes)?;
        let (target_src, _) = self.resolve_source_relation(
            &ins.table,
            ins.alias.as_deref(),
            &local_ctes,
            outer_scopes,
        )?;
        let db_id = target_src.db_id.unwrap_or(DatabaseId::MAIN);
        let db_name = target_src
            .db_name
            .clone()
            .unwrap_or_else(|| self.slot_name(db_id));

        let ins_auth = self.check(
            AuthorizerAction::Insert,
            Some(&target_src.table_name),
            None,
            Some(&db_name),
        )?;

        match &mut ins.source {
            InsertSource::Values(rows) => {
                for r in rows {
                    for e in r {
                        self.authorize_expr(e, &local_ctes, outer_scopes)?;
                    }
                }
            }
            InsertSource::Select(sel) => {
                let keep = self.authorize_select_stmt_with_ctes(sel, &local_ctes, outer_scopes)?;
                if !keep {
                    return Ok(false);
                }
            }
            InsertSource::DefaultValues => {}
        }

        if ins_auth == AuthorizerResult::Ignore {
            return Ok(false);
        }

        if !ins.upsert_clauses.is_empty() {
            let mut excluded_src = target_src.clone();
            excluded_src.alias = Some("excluded".to_owned());
            excluded_src.qualified_only = true;
            let mut upsert_scopes = Vec::with_capacity(outer_scopes.len() + 1);
            upsert_scopes.extend_from_slice(outer_scopes);
            upsert_scopes.push(vec![target_src.clone(), excluded_src]);

            for uc in &mut ins.upsert_clauses {
                if let Some(ref mut target) = uc.target {
                    for ic in &mut target.columns {
                        if let Some(ref mut e) = ic.expr {
                            self.authorize_expr(e, &local_ctes, &upsert_scopes)?;
                        }
                    }
                    if let Some(ref mut w) = target.where_clause {
                        self.authorize_expr(w, &local_ctes, &upsert_scopes)?;
                    }
                }
                if let UpsertAction::DoUpdate {
                    ref mut assignments,
                    ref mut where_clause,
                } = uc.action
                {
                    self.authorize_set_assignments(
                        assignments,
                        &target_src,
                        &db_name,
                        &local_ctes,
                        &upsert_scopes,
                    )?;
                    if let Some(w) = where_clause {
                        self.authorize_expr(w, &local_ctes, &upsert_scopes)?;
                    }
                }
            }
        }

        let mut ret_scopes = Vec::with_capacity(outer_scopes.len() + 1);
        ret_scopes.extend_from_slice(outer_scopes);
        ret_scopes.push(vec![target_src.clone()]);
        self.authorize_projection(
            &mut ins.returning,
            std::slice::from_ref(&target_src),
            &local_ctes,
            &ret_scopes,
        )?;

        self.authorize_firing_triggers(db_id, &target_src, TriggerEventKind::Insert, None)?;
        if self.foreign_keys {
            let is_single_row_toplevel_insert = !self.in_subprogram
                && ins.upsert_clauses.is_empty()
                && matches!(&ins.source, InsertSource::Values(rows) if rows.len() == 1);
            self.authorize_fk_check(
                db_id,
                &target_src,
                false,
                true,
                None,
                !is_single_row_toplevel_insert,
            )?;
        }
        Ok(true)
    }

    fn authorize_update_stmt(
        &mut self,
        upd: &mut UpdateStmt,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        let local_ctes = self.authorize_ctes(&mut upd.ctes, &HashMap::new(), outer_scopes)?;
        let (target_src, _) = self.resolve_source_relation(
            &upd.table,
            upd.alias.as_deref(),
            &local_ctes,
            outer_scopes,
        )?;
        let db_id = target_src.db_id.unwrap_or(DatabaseId::MAIN);
        let db_name = target_src
            .db_name
            .clone()
            .unwrap_or_else(|| self.slot_name(db_id));

        let mut current_scope = vec![target_src.clone()];
        if let Some(ref mut fc) = upd.from {
            let from_tables = self.authorize_from_clause(fc, &local_ctes, outer_scopes)?;
            current_scope.extend(from_tables);
        }
        let mut scopes = Vec::with_capacity(outer_scopes.len() + 1);
        scopes.extend_from_slice(outer_scopes);
        scopes.push(current_scope.clone());

        self.authorize_set_assignments(
            &mut upd.assignments,
            &target_src,
            &db_name,
            &local_ctes,
            &scopes,
        )?;

        if let Some(ref mut w) = upd.selection {
            self.authorize_expr(w, &local_ctes, &scopes)?;
        }

        self.authorize_projection(&mut upd.returning, &current_scope, &local_ctes, &scopes)?;

        let mod_cols: Vec<String> = upd
            .assignments
            .iter()
            .flat_map(|a| a.columns.iter().cloned())
            .collect();
        self.authorize_firing_triggers(
            db_id,
            &target_src,
            TriggerEventKind::Update,
            Some(&mod_cols),
        )?;

        if upd.assignments.is_empty() && upd.returning.is_empty() {
            return Ok(false);
        }
        if self.foreign_keys {
            self.authorize_fk_check(db_id, &target_src, true, false, Some(&mod_cols), true)?;
            self.authorize_fk_check(db_id, &target_src, false, true, Some(&mod_cols), true)?;
            self.authorize_fk_actions(db_id, &target_src, Some(&mod_cols))?;
        }
        Ok(true)
    }

    fn authorize_set_assignments(
        &mut self,
        assignments: &mut Vec<sqliters_syntax::SetAssignment>,
        target_src: &AuthSourceTable,
        db_name: &str,
        ctes: &HashMap<String, Vec<String>>,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Result<()> {
        let mut kept = Vec::with_capacity(assignments.len());
        for mut assign in assignments.drain(..) {
            self.authorize_expr(&mut assign.expr, ctes, scopes)?;
            let mut kept_cols = Vec::with_capacity(assign.columns.len());
            for col in assign.columns {
                let canon = target_src
                    .resolve_column(&col)
                    .unwrap_or_else(|| col.clone());
                let res = self.check(
                    AuthorizerAction::Update,
                    Some(&target_src.table_name),
                    Some(&canon),
                    Some(db_name),
                )?;
                if res != AuthorizerResult::Ignore {
                    kept_cols.push(col);
                }
            }
            if !kept_cols.is_empty() {
                assign.columns = kept_cols;
                kept.push(assign);
            }
        }
        *assignments = kept;
        Ok(())
    }

    fn authorize_delete_stmt(
        &mut self,
        del: &mut DeleteStmt,
        outer_scopes: &[Vec<AuthSourceTable>],
    ) -> Result<bool> {
        let local_ctes = self.authorize_ctes(&mut del.ctes, &HashMap::new(), outer_scopes)?;
        let (target_src, _) = self.resolve_source_relation(
            &del.table,
            del.alias.as_deref(),
            &local_ctes,
            outer_scopes,
        )?;
        let db_id = target_src.db_id.unwrap_or(DatabaseId::MAIN);
        let db_name = target_src
            .db_name
            .clone()
            .unwrap_or_else(|| self.slot_name(db_id));

        let del_auth = self.check(
            AuthorizerAction::Delete,
            Some(&target_src.table_name),
            None,
            Some(&db_name),
        )?;
        if del_auth == AuthorizerResult::Ignore && del.selection.is_none() {
            // `R-17228-37124` (`delete.c:466-471`): `SQLITE_DELETE` returning `SQLITE_IGNORE`
            // proceeds with `DELETE` but disables the `OP_Clear` truncate optimization so rows
            // are deleted individually!
            del.selection = Some(Expr::Integer(1));
        }

        let current_scope = vec![target_src.clone()];
        let mut scopes = Vec::with_capacity(outer_scopes.len() + 1);
        scopes.extend_from_slice(outer_scopes);
        scopes.push(current_scope.clone());

        if let Some(ref mut w) = del.selection {
            self.authorize_expr(w, &local_ctes, &scopes)?;
        }

        self.authorize_projection(&mut del.returning, &current_scope, &local_ctes, &scopes)?;

        self.authorize_firing_triggers(db_id, &target_src, TriggerEventKind::Delete, None)?;
        if self.foreign_keys {
            self.authorize_fk_check(db_id, &target_src, true, false, None, true)?;
            self.authorize_fk_actions(db_id, &target_src, None)?;
        }
        Ok(true)
    }

    fn resolve_fk_parent_col_names(ptbl: &TableSchema, fk: &ForeignKeySchema) -> Vec<String> {
        match &fk.ref_cols {
            Some(names) => names
                .iter()
                .filter_map(|n| ptbl.find_column(n).map(|(_, c)| c.name.clone()))
                .collect(),
            None => {
                if let Some(ipk_idx) = ptbl.rowid_alias_col
                    && let Some(c) = ptbl.columns.get(ipk_idx)
                {
                    return vec![c.name.clone()];
                }
                let mut pk_cols: Vec<(usize, String)> = ptbl
                    .columns
                    .iter()
                    .filter(|c| c.is_primary_key)
                    .map(|c| (c.pk_order, c.name.clone()))
                    .collect();
                pk_cols.sort_by_key(|(ord, _)| *ord);
                pk_cols.into_iter().map(|(_, n)| n).collect()
            }
        }
    }

    fn fk_child_is_modified(
        tbl: &TableSchema,
        fk: &ForeignKeySchema,
        modified_cols: &[String],
    ) -> bool {
        let chng_rowid = !tbl.without_rowid
            && (modified_cols.iter().any(|c| is_rowid_alias(c))
                || tbl.rowid_alias_col.is_some_and(|idx| {
                    modified_cols
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&tbl.columns[idx].name))
                }));
        fk.from_cols.iter().any(|&ci| {
            (tbl.rowid_alias_col == Some(ci) && chng_rowid)
                || tbl.columns.get(ci).is_some_and(|col| {
                    modified_cols
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(&col.name))
                })
        })
    }

    fn fk_parent_is_modified(
        ptbl: &TableSchema,
        parent_cols: &[String],
        modified_cols: &[String],
    ) -> bool {
        let chng_rowid = !ptbl.without_rowid
            && (modified_cols.iter().any(|c| is_rowid_alias(c))
                || ptbl.rowid_alias_col.is_some_and(|idx| {
                    modified_cols
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&ptbl.columns[idx].name))
                }));
        parent_cols.iter().any(|pcol| {
            modified_cols.iter().any(|m| m.eq_ignore_ascii_case(pcol))
                || (chng_rowid
                    && ptbl
                        .rowid_alias_col
                        .and_then(|idx| ptbl.columns.get(idx))
                        .is_some_and(|c| c.name.eq_ignore_ascii_case(pcol)))
        })
    }

    fn authorize_fk_check(
        &mut self,
        db_id: DatabaseId,
        target_src: &AuthSourceTable,
        has_old: bool,
        has_new: bool,
        modified_cols: Option<&[String]>,
        is_multi_or_subprogram: bool,
    ) -> Result<()> {
        if Schema::is_schema_table_name(&target_src.table_name) {
            return Ok(());
        }
        let Some((db_name, slot_schema, p_tab)) = self.db_set.slot(db_id).and_then(|s| {
            let t = s.schema.find_user_table(&target_src.table_name)?.clone();
            Some((s.name.clone(), s.schema.clone(), t))
        }) else {
            return Ok(());
        };

        // 1. Child-side FKs (`pTab->u.tab.pFKey` in `fkey.c:916-1015`)
        for fk in p_tab.foreign_keys.iter().rev() {
            if let Some(mods) = modified_cols
                && !p_tab.name.eq_ignore_ascii_case(&fk.ref_table)
                && !Self::fk_child_is_modified(&p_tab, fk, mods)
            {
                continue;
            }
            let Some(p_to) = slot_schema.find_user_table(&fk.ref_table) else {
                continue;
            };
            let parent_cols = Self::resolve_fk_parent_col_names(p_to, fk);
            if parent_cols.len() != fk.from_cols.len() {
                continue;
            }
            for p_col_name in &parent_cols {
                let res = self.check_read(db_id, &db_name, &p_to.name, p_col_name)?;
                if res == AuthorizerResult::Ignore
                    && !self
                        .ignored_fk_parent_tables
                        .iter()
                        .any(|(d, t)| *d == db_id && t.eq_ignore_ascii_case(&p_to.name))
                {
                    self.ignored_fk_parent_tables
                        .push((db_id, p_to.name.clone()));
                }
            }
        }

        // 2. Parent-side FKs (`sqlite3FkReferences(pTab)` in `fkey.c:1019-1086`)
        for ctbl in slot_schema.tables.iter().rev() {
            for fk in ctbl.foreign_keys.iter().rev() {
                if !fk.ref_table.eq_ignore_ascii_case(&p_tab.name) {
                    continue;
                }
                let parent_cols = Self::resolve_fk_parent_col_names(&p_tab, fk);
                if parent_cols.len() != fk.from_cols.len() {
                    continue;
                }
                if let Some(mods) = modified_cols
                    && !Self::fk_parent_is_modified(&p_tab, &parent_cols, mods)
                {
                    continue;
                }
                if !has_old
                    && has_new
                    && modified_cols.is_none()
                    && !fk.deferred
                    && !is_multi_or_subprogram
                {
                    continue;
                }
                if has_new {
                    for &from_ci in &fk.from_cols {
                        if let Some(col) = ctbl.columns.get(from_ci) {
                            let _ = self.check_read(db_id, &db_name, &ctbl.name, &col.name)?;
                        }
                    }
                }
                if has_old {
                    for &from_ci in &fk.from_cols {
                        if let Some(col) = ctbl.columns.get(from_ci) {
                            let _ = self.check_read(db_id, &db_name, &ctbl.name, &col.name)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn authorize_fk_actions(
        &mut self,
        db_id: DatabaseId,
        target_src: &AuthSourceTable,
        modified_cols: Option<&[String]>,
    ) -> Result<()> {
        if !self.db_set.enable_trigger() || Schema::is_schema_table_name(&target_src.table_name) {
            return Ok(());
        }
        let Some((db_name, slot_schema, p_tab)) = self.db_set.slot(db_id).and_then(|s| {
            let t = s.schema.find_user_table(&target_src.table_name)?.clone();
            Some((s.name.clone(), s.schema.clone(), t))
        }) else {
            return Ok(());
        };
        let is_update = modified_cols.is_some();

        for ctbl in slot_schema.tables.iter().rev() {
            for fk in ctbl.foreign_keys.iter().rev() {
                if !fk.ref_table.eq_ignore_ascii_case(&p_tab.name) {
                    continue;
                }
                let parent_cols = Self::resolve_fk_parent_col_names(&p_tab, fk);
                if parent_cols.len() != fk.from_cols.len() {
                    continue;
                }
                if let Some(mods) = modified_cols
                    && !Self::fk_parent_is_modified(&p_tab, &parent_cols, mods)
                {
                    continue;
                }
                let action = if is_update {
                    fk.on_update
                } else {
                    fk.on_delete
                };
                if action == FkAction::NoAction {
                    continue;
                }
                let visit_key = (db_id, ctbl.name.to_ascii_uppercase(), fk.id, is_update);
                if self.visited_fk_actions.contains(&visit_key) {
                    continue;
                }

                let mut pseudo_scope = Vec::new();
                let mut old_src = target_src.clone();
                old_src.alias = Some("old".to_owned());
                old_src.qualified_only = true;
                pseudo_scope.push(old_src);
                if is_update {
                    let mut new_src = target_src.clone();
                    new_src.alias = Some("new".to_owned());
                    new_src.qualified_only = true;
                    pseudo_scope.push(new_src);
                }
                let trig_scopes = vec![pseudo_scope];

                // Build `WHERE old.to_col1 = from_col1 AND ...`
                let mut where_expr: Option<Expr> = None;
                for (&from_ci, to_col_name) in fk.from_cols.iter().zip(parent_cols.iter()) {
                    let Some(from_col) = ctbl.columns.get(from_ci) else {
                        continue;
                    };
                    let eq = Expr::Binary {
                        left: Box::new(Expr::Column(ColumnRef::table_qualified(
                            "old",
                            to_col_name.clone(),
                        ))),
                        op: BinaryOp::Eq,
                        right: Box::new(Expr::Column(ColumnRef::unqualified(
                            from_col.name.clone(),
                        ))),
                    };
                    where_expr = Some(match where_expr {
                        None => eq,
                        Some(prev) => Expr::Binary {
                            left: Box::new(prev),
                            op: BinaryOp::And,
                            right: Box::new(eq),
                        },
                    });
                }

                let prev_sub = self.in_subprogram;
                self.in_subprogram = true;
                self.visited_fk_actions.push(visit_key);
                let res = (|| -> Result<()> {
                    if is_update {
                        // Synthetic `WHEN NOT(old.col1 IS new.col1 AND ...)` (`fkey.c:1284-1293`)
                        for to_col_name in &parent_cols {
                            let _ = self.check_read(db_id, &db_name, &p_tab.name, to_col_name)?;
                            let _ = self.check_read(db_id, &db_name, &p_tab.name, to_col_name)?;
                        }
                    }
                    match action {
                        FkAction::NoAction => Ok(()),
                        FkAction::Restrict => {
                            let _ = self.check(AuthorizerAction::Select, None, None, None)?;
                            for (&from_ci, to_col_name) in
                                fk.from_cols.iter().zip(parent_cols.iter())
                            {
                                let _ =
                                    self.check_read(db_id, &db_name, &p_tab.name, to_col_name)?;
                                if let Some(from_col) = ctbl.columns.get(from_ci) {
                                    let _ = self.check_read(
                                        db_id,
                                        &db_name,
                                        &ctbl.name,
                                        &from_col.name,
                                    )?;
                                }
                            }
                            Ok(())
                        }
                        FkAction::Cascade if !is_update => {
                            let mut syn_del = DeleteStmt {
                                ctes: Vec::new(),
                                table: QualifiedName {
                                    schema: Some(db_name.clone()),
                                    name: ctbl.name.clone(),
                                },
                                alias: None,
                                indexed_by: None,
                                selection: where_expr,
                                returning: Vec::new(),
                            };
                            let _ = self.authorize_delete_stmt(&mut syn_del, &trig_scopes)?;
                            Ok(())
                        }
                        FkAction::Cascade | FkAction::SetNull | FkAction::SetDefault => {
                            let mut assignments = Vec::with_capacity(fk.from_cols.len());
                            for (&from_ci, to_col_name) in
                                fk.from_cols.iter().zip(parent_cols.iter())
                            {
                                let Some(from_col) = ctbl.columns.get(from_ci) else {
                                    continue;
                                };
                                let rhs = match action {
                                    FkAction::Cascade => Expr::Column(ColumnRef::table_qualified(
                                        "new",
                                        to_col_name.clone(),
                                    )),
                                    FkAction::SetDefault => {
                                        from_col.default_expr.clone().unwrap_or(Expr::Null)
                                    }
                                    _ => Expr::Null,
                                };
                                assignments.push(SetAssignment {
                                    columns: vec![from_col.name.clone()],
                                    row_value: false,
                                    expr: rhs,
                                });
                            }
                            let mut syn_upd = UpdateStmt {
                                ctes: Vec::new(),
                                or_conflict: None,
                                table: QualifiedName {
                                    schema: Some(db_name.clone()),
                                    name: ctbl.name.clone(),
                                },
                                alias: None,
                                indexed_by: None,
                                assignments,
                                from: None,
                                selection: where_expr,
                                returning: Vec::new(),
                            };
                            let _ = self.authorize_update_stmt(&mut syn_upd, &trig_scopes)?;
                            Ok(())
                        }
                    }
                })();
                self.visited_fk_actions.pop();
                self.in_subprogram = prev_sub;
                res?;
            }
        }
        Ok(())
    }

    fn authorize_firing_triggers(
        &mut self,
        target_db_id: DatabaseId,
        target_src: &AuthSourceTable,
        event: TriggerEventKind,
        modified_cols: Option<&[String]>,
    ) -> Result<()> {
        if !self.db_set.enable_trigger() || Schema::is_schema_table_name(&target_src.table_name) {
            return Ok(());
        }
        let mod_refs: Option<Vec<&str>> =
            modified_cols.map(|cs| cs.iter().map(String::as_str).collect());
        let triggers = self
            .db_set
            .triggers_for_relation(target_db_id, &target_src.table_name);
        for (trig_db_id, trig) in triggers {
            if !trig.matches_event_and_columns(event, mod_refs.as_deref()) {
                continue;
            }
            let visit_key = (trig_db_id, trig.name.to_ascii_uppercase());
            if self.visited_triggers.contains(&visit_key) {
                continue;
            }
            let mut pseudo_scope = Vec::new();
            let push_pseudo = |out: &mut Vec<AuthSourceTable>, alias: &str| {
                let mut p = target_src.clone();
                p.alias = Some(alias.to_owned());
                p.qualified_only = true;
                out.push(p);
            };
            match trig.event {
                TriggerEvent::Insert => push_pseudo(&mut pseudo_scope, "new"),
                TriggerEvent::Delete => push_pseudo(&mut pseudo_scope, "old"),
                TriggerEvent::Update { .. } => {
                    push_pseudo(&mut pseudo_scope, "old");
                    push_pseudo(&mut pseudo_scope, "new");
                }
            }
            let trig_scopes = vec![pseudo_scope];
            let empty_ctes = HashMap::new();

            let prev_sub = self.in_subprogram;
            self.in_subprogram = true;
            self.visited_triggers.push(visit_key);
            self.context_stack.push(trig.name.clone());
            let res = (|| -> Result<()> {
                if let Some(mut when_expr) = trig.when_clause.clone() {
                    self.authorize_expr(&mut when_expr, &empty_ctes, &trig_scopes)?;
                }
                for step in &trig.body {
                    match step.clone() {
                        TriggerStep::Select(mut s) => {
                            let _ = self.authorize_select_stmt(&mut s, &trig_scopes)?;
                        }
                        TriggerStep::Insert(mut i) => {
                            let _ = self.authorize_insert_stmt(&mut i, &trig_scopes)?;
                        }
                        TriggerStep::Update(mut u) => {
                            let _ = self.authorize_update_stmt(&mut u, &trig_scopes)?;
                        }
                        TriggerStep::Delete(mut d) => {
                            let _ = self.authorize_delete_stmt(&mut d, &trig_scopes)?;
                        }
                    }
                }
                Ok(())
            })();
            self.context_stack.pop();
            self.visited_triggers.pop();
            self.in_subprogram = prev_sub;
            res?;
        }
        Ok(())
    }

    fn authorize_window_spec(
        &mut self,
        ws: &mut WindowSpec,
        ctes: &HashMap<String, Vec<String>>,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Result<()> {
        for pe in &mut ws.partition_by {
            self.authorize_expr(pe, ctes, scopes)?;
        }
        for ob in &mut ws.order_by {
            self.authorize_expr(&mut ob.expr, ctes, scopes)?;
        }
        if let Some(ref mut wf) = ws.frame {
            for b in [&mut wf.start, &mut wf.end] {
                if let WindowFrameBound::Preceding(e) | WindowFrameBound::Following(e) = b {
                    self.authorize_expr(e, ctes, scopes)?;
                }
            }
        }
        Ok(())
    }

    fn authorize_expr(
        &mut self,
        expr: &mut Expr,
        ctes: &HashMap<String, Vec<String>>,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Result<()> {
        match expr {
            Expr::Column(cref) => {
                if let Some((db_id, db_name, tbl_name, canon_col)) =
                    Self::resolve_column_in_scopes(cref, scopes)
                    && self.check_read(db_id, &db_name, &tbl_name, &canon_col)?
                        == AuthorizerResult::Ignore
                {
                    *expr = Expr::Null;
                }
                Ok(())
            }
            Expr::Unary { expr: inner, .. }
            | Expr::Collate { expr: inner, .. }
            | Expr::IsNull { expr: inner, .. }
            | Expr::Cast { expr: inner, .. } => self.authorize_expr(inner, ctes, scopes),
            Expr::Binary { left, right, .. } => {
                self.authorize_expr(left, ctes, scopes)?;
                self.authorize_expr(right, ctes, scopes)
            }
            Expr::Between {
                expr: v, low, high, ..
            } => {
                self.authorize_expr(v, ctes, scopes)?;
                self.authorize_expr(low, ctes, scopes)?;
                self.authorize_expr(high, ctes, scopes)
            }
            Expr::InList { expr: v, list, .. } => {
                self.authorize_expr(v, ctes, scopes)?;
                for item in list {
                    self.authorize_expr(item, ctes, scopes)?;
                }
                Ok(())
            }
            Expr::InSubquery {
                expr: v, subquery, ..
            } => {
                self.authorize_expr(v, ctes, scopes)?;
                let keep = self.authorize_select_stmt_with_ctes(subquery, ctes, scopes)?;
                if !keep {
                    *expr = Expr::Null;
                }
                Ok(())
            }
            Expr::InTable { expr: v, table, .. } => {
                self.authorize_expr(v, ctes, scopes)?;
                let (src, _) = self.resolve_source_relation(table, None, ctes, scopes)?;
                if src.fires_read
                    && let (Some(db_id), Some(db_name)) = (src.db_id, src.db_name.as_deref())
                {
                    let col = src
                        .columns
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "ROWID".to_owned());
                    if self.check_read(db_id, db_name, &src.table_name, &col)?
                        == AuthorizerResult::Ignore
                    {
                        *expr = Expr::Null;
                    }
                }
                Ok(())
            }
            Expr::Like {
                op,
                expr: v,
                pattern,
                escape,
                ..
            } => {
                self.authorize_expr(v, ctes, scopes)?;
                self.authorize_expr(pattern, ctes, scopes)?;
                if let Some(esc) = escape {
                    self.authorize_expr(esc, ctes, scopes)?;
                }
                let fn_name = match op {
                    LikeOp::Like => "like",
                    LikeOp::Glob => "glob",
                    LikeOp::Regexp => "regexp",
                    LikeOp::Match => "match",
                };
                if self.check(AuthorizerAction::Function, None, Some(fn_name), None)?
                    == AuthorizerResult::Ignore
                {
                    *expr = Expr::Null;
                }
                Ok(())
            }
            Expr::Case {
                operand,
                branches,
                else_expr,
            } => {
                if let Some(op) = operand {
                    self.authorize_expr(op, ctes, scopes)?;
                }
                for (w, t) in branches {
                    self.authorize_expr(w, ctes, scopes)?;
                    self.authorize_expr(t, ctes, scopes)?;
                }
                if let Some(el) = else_expr {
                    self.authorize_expr(el, ctes, scopes)?;
                }
                Ok(())
            }
            Expr::FunctionCall {
                name,
                args,
                filter,
                over,
                ..
            } => {
                let _ = self.functions;
                let fn_auth = self.check(
                    AuthorizerAction::Function,
                    None,
                    Some(&name.to_ascii_lowercase()),
                    None,
                )?;
                if let FunctionArgs::Exprs(es) = args {
                    for a in es {
                        self.authorize_expr(a, ctes, scopes)?;
                    }
                }
                if let Some(f) = filter {
                    self.authorize_expr(f, ctes, scopes)?;
                }
                if let Some(WindowRef::Spec(ws)) = over {
                    self.authorize_window_spec(ws, ctes, scopes)?;
                }
                if fn_auth == AuthorizerResult::Ignore {
                    *expr = Expr::Null;
                }
                Ok(())
            }
            Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
                let keep = self.authorize_select_stmt_with_ctes(sub, ctes, scopes)?;
                if !keep {
                    *expr = Expr::Null;
                }
                Ok(())
            }
            Expr::RowValue(items) => {
                for item in items {
                    self.authorize_expr(item, ctes, scopes)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn resolve_column_in_scopes(
        cref: &ColumnRef,
        scopes: &[Vec<AuthSourceTable>],
    ) -> Option<(DatabaseId, String, String, String)> {
        for scope in scopes.iter().rev() {
            if let Some(ref tq) = cref.table {
                for src in scope {
                    if src.matches_qualifier(cref.schema.as_deref(), tq)
                        && let Some(canon) = src.resolve_column(&cref.column)
                    {
                        if src.fires_read
                            && let (Some(db_id), Some(db_name)) = (src.db_id, &src.db_name)
                        {
                            return Some((db_id, db_name.clone(), src.table_name.clone(), canon));
                        }
                        return None;
                    }
                }
            } else {
                for src in scope {
                    if src.qualified_only {
                        continue;
                    }
                    if let Some(canon) = src.resolve_column(&cref.column) {
                        if src.fires_read
                            && let (Some(db_id), Some(db_name)) = (src.db_id, &src.db_name)
                        {
                            return Some((db_id, db_name.clone(), src.table_name.clone(), canon));
                        }
                        return None;
                    }
                }
            }
        }
        None
    }
}

fn infer_select_col_names(sel: &SelectStmt, db_set: &DatabaseSet) -> Vec<String> {
    match &sel.body.head {
        SelectCore::Values(rows) => rows
            .first()
            .map(|r| (1..=r.len()).map(|i| format!("column{i}")).collect())
            .unwrap_or_default(),
        SelectCore::Select {
            projection, from, ..
        } => {
            let mut out = Vec::new();
            for rc in projection {
                match rc {
                    ResultColumn::Expr { expr, alias, .. } => {
                        if let Some(a) = alias {
                            out.push(a.clone());
                        } else if let Expr::Column(c) = expr {
                            out.push(c.column.clone());
                        } else {
                            out.push("?".to_owned());
                        }
                    }
                    ResultColumn::Star | ResultColumn::TableStar { .. } => {
                        if let Some(fc) = from
                            && let TableOrSubquery::Table { name, .. } = &fc.head
                            && let Ok(Some((_, rel))) = db_set.find_relation_in_set(name)
                        {
                            match rel {
                                ResolvedRelation::Table(t) => {
                                    out.extend(t.columns.iter().map(|c| c.name.clone()));
                                }
                                ResolvedRelation::View(v) => {
                                    if let Some(ref cols) = v.columns
                                        && !cols.is_empty()
                                    {
                                        out.extend(cols.iter().cloned());
                                    } else {
                                        out.extend(infer_select_col_names(&v.select, db_set));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            out
        }
    }
}

fn make_null_select_for_columns(cols: &[String]) -> SelectStmt {
    let proj = if cols.is_empty() {
        vec![ResultColumn::Expr {
            expr: Expr::Null,
            alias: None,
            span_text: None,
        }]
    } else {
        cols.iter()
            .map(|c| ResultColumn::Expr {
                expr: Expr::Null,
                alias: Some(c.clone()),
                span_text: None,
            })
            .collect()
    };
    SelectStmt {
        ctes: Vec::new(),
        body: sqliters_syntax::SelectBody {
            head: SelectCore::Select {
                distinct: false,
                projection: proj,
                from: None,
                selection: Some(Expr::Integer(0)),
                group_by: Vec::new(),
                having: None,
                windows: Vec::new(),
            },
            compounds: Vec::new(),
        },
        order_by: Vec::new(),
        limit: None,
    }
}

/// Runs compile-time authorization (`sqlite3_set_authorizer`) on `stmt`, returning:
/// - `Ok(Some(rewritten_stmt))` when permitted (with any `SQLITE_IGNORE` column/function/delete rewrites applied),
/// - `Ok(None)` when a statement-level `SQLITE_IGNORE` turns the statement into a no-op (`SQLITE_DONE`),
/// - `Err(SqliteError)` (`ErrorCode::Auth` on `AuthorizerResult::Deny`, or `ErrorCode::Error` if the callback panics).
pub fn authorize_stmt_with_slot(
    stmt: &Stmt,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    slot: &mut AuthorizerSlot,
) -> Result<Option<Stmt>> {
    authorize_stmt_with_slot_and_fks(stmt, db_set, functions, false, slot).map(|(st, _)| st)
}

/// Runs compile-time authorization (`sqlite3_set_authorizer`) on `stmt` including foreign key
/// read/action checks when `foreign_keys` is `true`, returning `(rewritten_stmt_opt, ignored_fk_parent_tables)`.
#[allow(clippy::type_complexity)]
pub fn authorize_stmt_with_slot_and_fks(
    stmt: &Stmt,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    foreign_keys: bool,
    slot: &mut AuthorizerSlot,
) -> Result<(Option<Stmt>, Vec<(DatabaseId, String)>)> {
    if !slot.is_some() {
        return Ok((Some(stmt.clone()), Vec::new()));
    }
    let mut cloned = stmt.clone();
    let mut ctx = AuthWalkCtx {
        db_set,
        functions,
        slot,
        foreign_keys,
        in_subprogram: false,
        context_stack: Vec::new(),
        visited_views: Vec::new(),
        visited_triggers: Vec::new(),
        visited_fk_actions: Vec::new(),
        ignored_fk_parent_tables: Vec::new(),
    };
    let _ = ctx.current_context();
    if ctx.authorize_stmt(&mut cloned, &[])? {
        Ok((Some(cloned), ctx.ignored_fk_parent_tables))
    } else {
        Ok((None, ctx.ignored_fk_parent_tables))
    }
}
