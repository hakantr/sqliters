//! Relational SQL query engine, DML/DDL executor, constraint & index manager, `VACUUM`, and `PRAGMA`s.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use sqliters_btree::{Btree, RootPageMove};
use sqliters_pager::{
    AutoVacuumMode, JournalMode, LockingMode, TransactionState, WalCheckpointMode,
    WalCheckpointResult,
};
use sqliters_syntax::{
    AlterTableAction, BinaryOp, BindParam, ColumnDef, ColumnRef, CommonTableExpr, CompoundOp,
    ConflictAction, CreateIndexStmt, CreateTableStmt, CurrentTimeKind, DeleteStmt, Expr, FkAction,
    FromClause, FunctionArgs, IndexHint, InsertSource, InsertStmt, JoinClause, JoinConstraint,
    JoinType, LikeOp, LimitClause, OrderByTerm, PragmaValue, QualifiedName, ResultColumn,
    SelectBody, SelectCore, SelectStmt, Stmt, TableOrSubquery, Token, TokenKind, UnaryOp,
    UpdateStmt, dequote, keyword_kind, parse_one, tokenize,
};
use sqliters_types::{
    Collation, ConflictResolutionEffect, DatabaseKind, ErrorCode, ExecutionServices, KeySortOrder,
    Pgno, Result, RuntimeValue, SqliteError, TextEncoding, TypeAffinity, Value,
    decode_record_with_encoding, encode_record_with_encoding, extended,
};
use sqliters_vdbe::{
    AggregateAccumulator, AggregateKind, Opcode, VmRuntimeContext, eval_cast_rt_with_encoding,
    eval_comparison_with_affinity, eval_comparison_with_encoding, eval_comparison_with_services,
    eval_unary_negate, format_utc_date, format_utc_time, format_utc_timestamp, parse_escape_char,
    sqlite_compare_affinity, sqlite_glob_match, sqlite_like_match,
};

use crate::compiler::{
    AccessPlan, choose_access_plan, compile_for_explain_with_functions, explain_query_plan,
    expr_column_label, expr_contains_aggregate,
};
use crate::execution_context::{
    CollationRegistry, ExecutionContext, FunctionRegistry, ProgressTracker,
};
use crate::schema::{
    ColumnSchema, ForeignKeySchema, IndexSchema, Schema, TableRowKey, TableSchema,
    build_column_schema_from_def, build_index_schema_from_ast, build_table_schema_from_ast,
    delete_schema_entry, insert_schema_entry, scan_index_incremental,
    scan_index_incremental_with_services, scan_table_incremental,
    scan_table_incremental_with_services, update_schema_entry,
};

/// Materialized result set (`column_names` + `rows`) produced by executing a statement.
#[derive(Debug, Clone, Default)]
pub struct QueryOutput {
    pub column_names: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Connection-level named SQL savepoint (`sqlite/src/vdbe.c` `OP_Savepoint`).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct SqlSavepoint {
    pub name: String,
    /// 0-based physical `Pager` subsavepoint slot index corresponding to this savepoint.
    pub subsavepoint_depth: usize,
    /// `true` when opened in autocommit mode (marking an implicit transaction savepoint).
    pub is_transaction_savepoint: bool,
    pub schema_at_open: Schema,
    pub schema_gen_at_open: u64,
    pub deferred_fk_suspects: Vec<(String, TableRowKey, usize)>,
}

/// Snapshot of other active statements on the connection (excluding the currently executing handle).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivitySummary {
    pub other_active: usize,
    pub other_readers: usize,
    pub other_writers: usize,
}

/// Default WAL automatic checkpoint threshold (`SQLITE_DEFAULT_WAL_AUTOCHECKPOINT = 1000`).
pub const SQLITE_DEFAULT_WAL_AUTOCHECKPOINT: i32 = 1000;

/// Connection-level busy retry policy (`sqlite3_busy_timeout` / `sqlite3_busy_handler`, `main.c:1765-1955`).
#[derive(Default)]
pub enum BusyPolicy {
    #[default]
    None,
    TimeoutMs(u64),
    Custom(Box<dyn FnMut(u32) -> bool + Send>),
}

impl BusyPolicy {
    #[inline]
    pub(crate) fn has_handler(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn invoke_once(&mut self, count: u32, start: std::time::Instant) -> bool {
        const DELAYS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];
        match self {
            Self::None => false,
            Self::Custom(cb) => cb(count),
            Self::TimeoutMs(tm) => {
                let limit = *tm;
                if limit == 0 {
                    return false;
                }
                let elapsed = start.elapsed().as_millis() as u64;
                if elapsed >= limit {
                    return false;
                }
                let base_delay = DELAYS_MS.get(count as usize).copied().unwrap_or(100);
                let sleep_ms = base_delay.min(limit - elapsed).max(1);
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                true
            }
        }
    }

    /// Retries `op(btree)` on external lock contention (`ErrorCode::Busy` excluding `SQLITE_BUSY_SNAPSHOT`),
    /// matching `btreeBeginTrans` (`btree.c:3040`).
    pub(crate) fn retry_btree<T, F>(&mut self, btree: &mut Btree, mut op: F) -> Result<T>
    where
        F: FnMut(&mut Btree) -> Result<T>,
    {
        if !self.has_handler() {
            return op(btree);
        }
        let start = std::time::Instant::now();
        let mut count = 0u32;
        loop {
            match op(btree) {
                Ok(v) => return Ok(v),
                Err(e)
                    if e.code == ErrorCode::Busy
                        && e.extended_code != extended::SQLITE_BUSY_SNAPSHOT =>
                {
                    btree.check_interrupted()?;
                    if !self.invoke_once(count, start) {
                        return Err(e);
                    }
                    count = count.saturating_add(1);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Runs `btree.wal_checkpoint`, passing `busy_handler` only when `mode > Passive` (`pager.c:7608`, `wal.c:4408`).
    pub(crate) fn checkpoint_btree(
        &mut self,
        btree: &mut Btree,
        mode: WalCheckpointMode,
    ) -> Result<WalCheckpointResult> {
        if mode > WalCheckpointMode::Passive && self.has_handler() {
            let start = std::time::Instant::now();
            let mut cb = |count: u32| self.invoke_once(count, start);
            btree.wal_checkpoint(mode, Some(&mut cb))
        } else {
            btree.wal_checkpoint(mode, None)
        }
    }
}

/// Mutable database state passed into statement execution.
pub struct ExecContext<'a> {
    pub btree: &'a mut Btree,
    pub schema: &'a mut Schema,
    pub runtime: &'a mut VmRuntimeContext,
    pub interrupt_flag: &'a std::sync::atomic::AtomicBool,
    pub functions: &'a FunctionRegistry,
    pub collations: &'a CollationRegistry,
    pub progress: &'a mut ProgressTracker,
    pub strict_subtype: bool,
    pub explicit_tx: &'a mut bool,
    pub implicit_savepoint_tx: &'a mut bool,
    pub in_user_btree_tx: &'a mut bool,
    pub savepoints: &'a mut Vec<SqlSavepoint>,
    pub schema_generation: &'a mut u64,
    pub schema_changed_in_tx: &'a mut bool,
    pub activity: ActivitySummary,
    pub foreign_keys: &'a mut bool,
    pub count_changes: &'a mut bool,
    pub wal_autocheckpoint_threshold: &'a mut i32,
    pub default_locking_mode: &'a mut LockingMode,
    pub next_auto_vacuum: &'a mut Option<AutoVacuumMode>,
    pub next_page_size: &'a mut Option<u32>,
    pub writable_schema: &'a mut bool,
    pub working_dir: &'a Option<std::path::PathBuf>,
    pub busy_policy: &'a mut BusyPolicy,
    pub rollback_count: &'a mut u64,
    pub deferred_fk_suspects: &'a mut Vec<(String, TableRowKey, usize)>,
    pub publish_mirrors: &'a mut dyn FnMut(&VmRuntimeContext, bool, bool, u64),
}

impl ExecContext<'_> {
    #[inline]
    pub(crate) fn publish(&mut self) {
        (self.publish_mirrors)(
            self.runtime,
            *self.explicit_tx,
            *self.implicit_savepoint_tx,
            *self.rollback_count,
        );
    }

    #[inline]
    pub(crate) fn step_tick(&mut self) -> Result<()> {
        if self
            .interrupt_flag
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        self.progress.tick()
    }

    pub(crate) fn call_scalar_rt(
        &mut self,
        name: &str,
        args: &[RuntimeValue],
    ) -> Result<RuntimeValue> {
        let enc = self.btree.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        svc.call_scalar(name, args)
    }

    pub(crate) fn scan_table(&mut self, root_pgno: Pgno) -> Result<Vec<(i64, Vec<u8>)>> {
        let enc = self.btree.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        scan_table_incremental_with_services(self.btree, root_pgno, &mut svc)
    }

    pub(crate) fn scan_index(
        &mut self,
        root_pgno: Pgno,
        orders: Vec<KeySortOrder>,
    ) -> Result<Vec<Vec<u8>>> {
        let enc = self.btree.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        scan_index_incremental_with_services(self.btree, root_pgno, orders, &mut svc)
    }
}

/// Resolves `qn` against `schema` for `SELECT` / `IN [schema.]table` (`DatabaseId::MAIN` + empty `temp` catalog).
/// Returns `(table_schema, resolved_schema_label)`.
fn resolve_table_for_query(
    schema: &Schema,
    qn: &QualifiedName,
) -> Result<(TableSchema, &'static str)> {
    if let Some(ref s) = qn.schema {
        if s.eq_ignore_ascii_case("temp") {
            if Schema::is_schema_table_name(&qn.name) {
                let mut t = TableSchema::sqlite_schema_table();
                t.name = "sqlite_temp_schema".to_owned();
                t.root_pgno = 0;
                return Ok((t, "temp"));
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", qn.display_name()),
            ));
        }
        if s.eq_ignore_ascii_case("main") {
            if Schema::is_temp_schema_table_name(&qn.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", qn.display_name()),
                ));
            }
            if let Some(t) = schema.find_table(&qn.name) {
                return Ok((t, "main"));
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", qn.display_name()),
            ));
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", qn.display_name()),
        ));
    }
    if Schema::is_temp_schema_table_name(&qn.name) {
        let mut t = TableSchema::sqlite_schema_table();
        t.name = "sqlite_temp_schema".to_owned();
        t.root_pgno = 0;
        return Ok((t, "temp"));
    }
    if let Some(t) = schema.find_table(&qn.name) {
        return Ok((t, "main"));
    }
    Err(SqliteError::new(
        ErrorCode::Error,
        format!("no such table: {}", qn.name),
    ))
}

#[derive(Debug, Clone)]
struct ScopedColumn {
    schema_name: Option<String>,
    table_alias: Option<String>,
    name: String,
    collation: Collation,
    affinity: Option<TypeAffinity>,
    hidden_rowid: bool,
    merged_using: bool,
}

impl ScopedColumn {
    #[inline]
    fn matches_qualifier(&self, schema_qual: Option<&str>, table_qual: Option<&str>) -> bool {
        if let Some(sq) = schema_qual {
            let Some(ref sn) = self.schema_name else {
                return false;
            };
            if !sn.eq_ignore_ascii_case(sq) {
                return false;
            }
        }
        if let Some(tq) = table_qual {
            let Some(ref ta) = self.table_alias else {
                return false;
            };
            if !ta.eq_ignore_ascii_case(tq) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
struct EvalRow {
    cols: Rc<[ScopedColumn]>,
    vals: Vec<Value>,
}

impl Default for EvalRow {
    fn default() -> Self {
        Self {
            cols: Rc::from([]),
            vals: Vec::new(),
        }
    }
}

impl EvalRow {
    fn lookup(&self, qual: Option<&str>, col_name: &str) -> Result<Option<(Value, Collation)>> {
        Ok(self
            .lookup_qual_full(None, qual, col_name)?
            .map(|(v, c, _)| (v, c)))
    }

    fn lookup_full(
        &self,
        qual: Option<&str>,
        col_name: &str,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        self.lookup_qual_full(None, qual, col_name)
    }

    fn lookup_cref(&self, cref: &ColumnRef) -> Result<Option<(Value, Collation)>> {
        Ok(self
            .lookup_qual_full(cref.schema.as_deref(), cref.table.as_deref(), &cref.column)?
            .map(|(v, c, _)| (v, c)))
    }

    fn lookup_cref_full(
        &self,
        cref: &ColumnRef,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        self.lookup_qual_full(cref.schema.as_deref(), cref.table.as_deref(), &cref.column)
    }

    fn lookup_qual_full(
        &self,
        schema_qual: Option<&str>,
        table_qual: Option<&str>,
        col_name: &str,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        let mut matches = Vec::new();
        for (i, sc) in self.cols.iter().enumerate() {
            if sc.hidden_rowid {
                continue;
            }
            if !sc.matches_qualifier(schema_qual, table_qual) {
                continue;
            }
            if sc.name.eq_ignore_ascii_case(col_name) {
                matches.push((
                    self.vals[i].clone(),
                    sc.collation,
                    sc.affinity,
                    sc.merged_using,
                ));
                if schema_qual.is_none() && table_qual.is_none() && sc.merged_using {
                    // Deduplicated USING / NATURAL column takes unambiguous precedence
                    return Ok(Some((self.vals[i].clone(), sc.collation, sc.affinity)));
                }
            }
        }

        if matches.len() == 1 {
            return Ok(Some((matches[0].0.clone(), matches[0].1, matches[0].2)));
        }
        if matches.len() > 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("ambiguous column name: {col_name}"),
            ));
        }

        // Check hidden implicit rowid (`rowid`, `_rowid_`, `oid`)
        if col_name.eq_ignore_ascii_case("rowid")
            || col_name.eq_ignore_ascii_case("_rowid_")
            || col_name.eq_ignore_ascii_case("oid")
        {
            for (i, sc) in self.cols.iter().enumerate() {
                if !sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(schema_qual, table_qual) {
                    continue;
                }
                matches.push((
                    self.vals[i].clone(),
                    Collation::Binary,
                    Some(TypeAffinity::Integer),
                    false,
                ));
            }
            if matches.len() == 1 {
                return Ok(Some((
                    matches[0].0.clone(),
                    Collation::Binary,
                    Some(TypeAffinity::Integer),
                )));
            }
        }

        Ok(None)
    }
}

#[derive(Clone, Default)]
struct CteStore {
    /// Lexical stack of `WITH` scopes (`(scope_idx, cte_idx)`).
    scopes: Vec<Vec<CommonTableExpr>>,
    /// Active 1-row working tables during recursive CTE queue steps (`(scope_idx, cte_idx, QueryOutput)`).
    working: Vec<(usize, usize, QueryOutput)>,
    /// Active CTE evaluation chain `(scope_idx, cte_idx)` to detect circular references.
    resolving: Vec<(usize, usize)>,
    /// Optional outer row limit hint `(scope_idx, cte_idx, max_rows)` for unbounded recursive CTEs.
    limit_hint: Option<(usize, usize, usize)>,
}

impl CteStore {
    fn with_scope(&self, ctes: &[CommonTableExpr]) -> Self {
        let mut next = self.clone();
        if !ctes.is_empty() {
            next.scopes.push(ctes.to_vec());
        }
        next.limit_hint = None;
        next
    }

    fn locate(&self, name: &str) -> Option<(usize, usize)> {
        for (s_idx, scope) in self.scopes.iter().enumerate().rev() {
            for (c_idx, cte) in scope.iter().enumerate() {
                if cte.name.eq_ignore_ascii_case(name) {
                    return Some((s_idx, c_idx));
                }
            }
        }
        None
    }

    fn find_working(&self, s_idx: usize, c_idx: usize) -> Option<&QueryOutput> {
        self.working
            .iter()
            .rev()
            .find(|(ws, wc, _)| *ws == s_idx && *wc == c_idx)
            .map(|(_, _, q)| q)
    }
}

///Statement execution category used by `Statement` (`sqliters-core`) to coordinate `ActivityTracker`
/// and `Pager` transaction leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StmtCategory {
    /// `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`
    Control,
    /// Pragmas / `ANALYZE` that do not open a B-tree read/write transaction on `step()`
    NoLockPragma,
    /// `SELECT`, `EXPLAIN`, and read-transaction `PRAGMA`s
    Read,
    /// `INSERT`, `UPDATE`, `DELETE`, DDL, `REINDEX`, and header-writing `PRAGMA`s
    Write,
    /// Streaming `PRAGMA [schema.]incremental_vacuum[(N)]`
    IncrementalVacuum,
    /// `VACUUM [schema] [INTO expr]`
    Vacuum,
}

/// Returns `true` when `stmt` must load `sqlite_schema` at `prepare` time (`PragFlg_NeedSchema` / `prepare.c`).
pub(crate) fn stmt_loads_schema_on_prepare(stmt: &Stmt) -> bool {
    if stmt_requires_schema(stmt) {
        return true;
    }
    matches!(
        stmt,
        Stmt::Pragma { name, .. }
            if matches!(
                name.to_ascii_uppercase().as_str(),
                "AUTO_VACUUM" | "INCREMENTAL_VACUUM" | "DEFAULT_CACHE_SIZE"
            )
    )
}

/// Returns `true` when `stmt` verifies `sqlite_schema` cookie on `step()` (`OP_Transaction` `P3 != 0`).
pub(crate) fn stmt_requires_schema(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_)
        | Stmt::AttachDatabase { .. }
        | Stmt::DetachDatabase { .. } => false,
        Stmt::Pragma { name, .. } => matches!(
            name.to_ascii_uppercase().as_str(),
            "TABLE_INFO"
                | "INDEX_LIST"
                | "INDEX_INFO"
                | "FOREIGN_KEY_LIST"
                | "FOREIGN_KEY_CHECK"
                | "INTEGRITY_CHECK"
                | "QUICK_CHECK"
                | "WAL_CHECKPOINT"
        ),
        _ => true,
    }
}

/// Parses `PRAGMA auto_vacuum = <val>` matching `getAutoVacuum` (`sqlite/src/pragma.c:125-132`).
pub(crate) fn parse_auto_vacuum_pragma(pv: &PragmaValue) -> AutoVacuumMode {
    match pv {
        PragmaValue::Integer(1) => AutoVacuumMode::Full,
        PragmaValue::Integer(2) => AutoVacuumMode::Incremental,
        PragmaValue::Integer(_) => AutoVacuumMode::None,
        PragmaValue::Ident(s) | PragmaValue::String(s) => {
            let trimmed = s.trim();
            if trimmed.eq_ignore_ascii_case("none") {
                return AutoVacuumMode::None;
            }
            if trimmed.eq_ignore_ascii_case("full") {
                return AutoVacuumMode::Full;
            }
            if trimmed.eq_ignore_ascii_case("incremental") {
                return AutoVacuumMode::Incremental;
            }
            match trimmed.parse::<i32>() {
                Ok(1) => AutoVacuumMode::Full,
                Ok(2) => AutoVacuumMode::Incremental,
                _ => AutoVacuumMode::None,
            }
        }
    }
}

/// Parses `PRAGMA incremental_vacuum[(N) | = N]` matching `sqlite3GetInt32` in `pragma.c:856-859`:
/// omitted, non-integer, out-of-`i32`-range, or `<= 0` all map to `i32::MAX as u32` (`0x7fff_ffff`).
pub(crate) fn parse_incremental_vacuum_limit(value: Option<&PragmaValue>) -> u32 {
    let Some(pv) = value else {
        return i32::MAX as u32;
    };
    let parsed_i32: Option<i32> = match pv {
        PragmaValue::Integer(i) => i32::try_from(*i).ok(),
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.trim().parse::<i32>().ok(),
    };
    match parsed_i32 {
        Some(n) if n > 0 => n as u32,
        _ => i32::MAX as u32,
    }
}

/// Classifies `stmt` into its execution lease category.
pub(crate) fn classify_stmt_category(stmt: &Stmt) -> StmtCategory {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_) => StmtCategory::Control,
        Stmt::Analyze(_) | Stmt::AttachDatabase { .. } | Stmt::DetachDatabase { .. } => {
            StmtCategory::NoLockPragma
        }
        Stmt::Vacuum { .. } => StmtCategory::Vacuum,
        Stmt::Select(_) | Stmt::Explain { .. } => StmtCategory::Read,
        Stmt::Pragma {
            schema,
            name,
            value,
        } => match name.to_ascii_uppercase().as_str() {
            "INTEGRITY_CHECK" | "QUICK_CHECK" | "PAGE_COUNT" | "FREELIST_COUNT"
            | "SCHEMA_VERSION" | "FOREIGN_KEY_LIST" | "FOREIGN_KEY_CHECK" | "TABLE_INFO"
            | "INDEX_LIST" | "INDEX_INFO" => StmtCategory::Read,
            "INCREMENTAL_VACUUM" => StmtCategory::IncrementalVacuum,
            "USER_VERSION" | "APPLICATION_ID" | "DEFAULT_CACHE_SIZE" => {
                if value.is_some() {
                    if schema
                        .as_deref()
                        .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
                    {
                        StmtCategory::NoLockPragma
                    } else {
                        StmtCategory::Write
                    }
                } else {
                    StmtCategory::Read
                }
            }
            _ => StmtCategory::NoLockPragma,
        },
        Stmt::Insert(_)
        | Stmt::Update(_)
        | Stmt::Delete(_)
        | Stmt::CreateTable(_)
        | Stmt::CreateTableAs { .. }
        | Stmt::DropTable { .. }
        | Stmt::CreateIndex(_)
        | Stmt::DropIndex { .. }
        | Stmt::AlterTable { .. }
        | Stmt::Reindex(_) => StmtCategory::Write,
    }
}

/// Checks all outstanding deferred foreign key suspects (`sqlite3VdbeCheckFkDeferred`).
pub(crate) fn check_deferred_fks(ctx: &mut ExecContext<'_>) -> Result<()> {
    if *ctx.foreign_keys && !ctx.deferred_fk_suspects.is_empty() {
        let suspects = ctx.deferred_fk_suspects.clone();
        verify_fk_suspects(ctx, &suspects)?;
    }
    Ok(())
}

/// Executes `COMMIT` (`OP_AutoCommit` with `desiredAutoCommit = 1, iRollback = 0`).
/// If `commit_retaining_read` returns `ErrorCode::Busy`, leaves the transaction open so it can be retried.
pub(crate) fn commit_transaction(ctx: &mut ExecContext<'_>) -> Result<()> {
    if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "cannot commit - no transaction is active",
        ));
    }
    if ctx.activity.other_writers > 0 {
        return Err(SqliteError::new(
            ErrorCode::Busy,
            "cannot commit transaction - SQL statements in progress",
        ));
    }
    check_deferred_fks(ctx)?;

    let retain_read = ctx.activity.other_readers > 0;
    match ctx.btree.transaction_state() {
        TransactionState::Open => {}
        TransactionState::Read => {
            if !retain_read {
                ctx.btree.pager_mut().end_read_if_unused()?;
            }
        }
        TransactionState::Write => {
            ctx.btree.commit_retaining_read(retain_read)?;
        }
    }

    *ctx.explicit_tx = false;
    *ctx.implicit_savepoint_tx = false;
    *ctx.in_user_btree_tx = false;
    ctx.savepoints.clear();
    *ctx.schema_changed_in_tx = false;
    ctx.deferred_fk_suspects.clear();
    ctx.publish();
    Ok(())
}

/// Executes `RELEASE [SAVEPOINT] <name>` (`OP_Savepoint` `SAVEPOINT_RELEASE`).
pub(crate) fn release_savepoint_op(ctx: &mut ExecContext<'_>, sp_name: &str) -> Result<()> {
    let Some(pos) = ctx
        .savepoints
        .iter()
        .rposition(|sp| sp.name.eq_ignore_ascii_case(sp_name))
    else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such savepoint: {sp_name}"),
        ));
    };
    if ctx.activity.other_writers > 0 {
        return Err(SqliteError::new(
            ErrorCode::Busy,
            "cannot release savepoint - SQL statements in progress",
        ));
    }

    let is_tx_sp = pos == 0 && ctx.savepoints[0].is_transaction_savepoint;
    if is_tx_sp {
        check_deferred_fks(ctx)?;
        let retain_read = ctx.activity.other_readers > 0;
        match ctx.btree.transaction_state() {
            TransactionState::Open => {}
            TransactionState::Read => {
                if !retain_read {
                    ctx.btree.pager_mut().end_read_if_unused()?;
                }
            }
            TransactionState::Write => {
                ctx.btree.commit_retaining_read(retain_read)?;
            }
        }
        *ctx.implicit_savepoint_tx = false;
        *ctx.in_user_btree_tx = false;
        ctx.savepoints.clear();
        *ctx.schema_changed_in_tx = false;
        ctx.deferred_fk_suspects.clear();
        ctx.publish();
        Ok(())
    } else {
        if ctx.btree.transaction_state() == TransactionState::Write {
            let slot = ctx.savepoints[pos].subsavepoint_depth;
            ctx.btree.pager_mut().release_subsavepoint(slot)?;
        }
        ctx.savepoints.truncate(pos);
        Ok(())
    }
}

/// Executes transaction / savepoint control statements (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`).
pub(crate) fn execute_control_stmt(ctx: &mut ExecContext<'_>, stmt: &Stmt) -> Result<()> {
    match stmt {
        Stmt::Begin { mode } => {
            if !is_in_autocommit(ctx) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot start a transaction within a transaction",
                ));
            }
            match mode {
                sqliters_syntax::TransactionMode::Deferred => {
                    *ctx.explicit_tx = true;
                    *ctx.implicit_savepoint_tx = false;
                    ctx.publish();
                }
                sqliters_syntax::TransactionMode::Immediate => {
                    ctx.busy_policy
                        .retry_btree(ctx.btree, |bt| bt.begin_write())?;
                    *ctx.explicit_tx = true;
                    *ctx.implicit_savepoint_tx = false;
                    *ctx.in_user_btree_tx = true;
                    ctx.publish();
                }
                sqliters_syntax::TransactionMode::Exclusive => {
                    ctx.busy_policy
                        .retry_btree(ctx.btree, |bt| bt.begin_exclusive())?;
                    *ctx.explicit_tx = true;
                    *ctx.implicit_savepoint_tx = false;
                    *ctx.in_user_btree_tx = true;
                    ctx.publish();
                }
            }
            Ok(())
        }
        Stmt::Commit => commit_transaction(ctx),
        Stmt::Rollback { savepoint } => {
            if let Some(sp_name) = savepoint {
                let Some(pos) = ctx
                    .savepoints
                    .iter()
                    .rposition(|sp| sp.name.eq_ignore_ascii_case(sp_name))
                else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such savepoint: {sp_name}"),
                    ));
                };
                let sp = ctx.savepoints[pos].clone();
                let schema_changed_since_sp = *ctx.schema_generation != sp.schema_gen_at_open;
                let is_schema_change = *ctx.schema_changed_in_tx || schema_changed_since_sp;
                let write_only = !is_schema_change;
                let trip_res = ctx
                    .btree
                    .trip_cursors(extended::SQLITE_ABORT_ROLLBACK, write_only);
                if ctx.btree.transaction_state() == TransactionState::Write {
                    ctx.btree
                        .pager_mut()
                        .rollback_to_subsavepoint(sp.subsavepoint_depth)?;
                }
                *ctx.deferred_fk_suspects = sp.deferred_fk_suspects;
                ctx.savepoints.truncate(pos + 1);
                if schema_changed_since_sp {
                    *ctx.schema = sp.schema_at_open;
                }
                if is_schema_change {
                    *ctx.schema_generation += 1;
                    *ctx.schema_changed_in_tx = true;
                }
                *ctx.rollback_count += 1;
                ctx.publish();
                trip_res?;
                Ok(())
            } else {
                if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot rollback - no transaction is active",
                    ));
                }
                let write_only = !*ctx.schema_changed_in_tx;
                let retain_read = ctx.activity.other_readers > 0 && write_only;
                match ctx.btree.transaction_state() {
                    TransactionState::Open => {}
                    TransactionState::Read => {
                        let trip_res = ctx
                            .btree
                            .trip_cursors(extended::SQLITE_ABORT_ROLLBACK, write_only);
                        if !retain_read {
                            ctx.btree.pager_mut().end_read_if_unused()?;
                        }
                        trip_res?;
                    }
                    TransactionState::Write => {
                        ctx.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            write_only,
                            retain_read,
                        )?;
                    }
                }
                let had_schema_change = *ctx.schema_changed_in_tx;
                *ctx.rollback_count += 1;
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                if had_schema_change {
                    *ctx.schema = Schema::load(ctx.btree)?;
                    *ctx.schema_generation += 1;
                }
                ctx.publish();
                Ok(())
            }
        }
        Stmt::Savepoint(name) => {
            if ctx.activity.other_writers > 0 {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "cannot open savepoint - SQL statements in progress",
                ));
            }
            let was_autocommit = is_in_autocommit(ctx);
            let slot_idx = ctx.savepoints.len();
            if ctx.btree.transaction_state() == TransactionState::Write {
                ctx.btree.pager_mut().ensure_subsavepoints(slot_idx + 1)?;
            }
            ctx.savepoints.push(SqlSavepoint {
                name: name.clone(),
                subsavepoint_depth: slot_idx,
                is_transaction_savepoint: was_autocommit,
                schema_at_open: ctx.schema.clone(),
                schema_gen_at_open: *ctx.schema_generation,
                deferred_fk_suspects: ctx.deferred_fk_suspects.clone(),
            });
            if was_autocommit {
                *ctx.implicit_savepoint_tx = true;
                ctx.publish();
            }
            Ok(())
        }
        Stmt::Release(name) => release_savepoint_op(ctx, name),
        _ => unreachable!("execute_control_stmt called on non-control statement"),
    }
}

fn validate_analyze_target(schema: &Schema, target: Option<&QualifiedName>) -> Result<()> {
    let Some(qn) = target else {
        return Ok(());
    };
    if let Some(ref s) = qn.schema {
        if !s.eq_ignore_ascii_case("main") && !s.eq_ignore_ascii_case("temp") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database {s}"),
            ));
        }
        if s.eq_ignore_ascii_case("main") {
            if !Schema::is_temp_schema_table_name(&qn.name)
                && (Schema::is_schema_table_name(&qn.name)
                    || schema.find_table(&qn.name).is_some()
                    || schema.find_index(&qn.name).is_some())
            {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", qn.display_name()),
            ));
        }
        if Schema::is_schema_table_name(&qn.name) {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", qn.display_name()),
        ));
    }
    if qn.name.eq_ignore_ascii_case("main")
        || qn.name.eq_ignore_ascii_case("temp")
        || schema.find_table(&qn.name).is_some()
        || schema.find_index(&qn.name).is_some()
    {
        return Ok(());
    }
    Err(SqliteError::new(
        ErrorCode::Error,
        format!("no such table: {}", qn.name),
    ))
}

/// Executes non-locking pragmas (`LOCK_STATUS`, `JOURNAL_MODE`, `SYNCHRONOUS`, etc.), `ANALYZE`, and `ATTACH`/`DETACH`.
pub(crate) fn execute_nolock_stmt(ctx: &mut ExecContext<'_>, stmt: &Stmt) -> Result<QueryOutput> {
    match stmt {
        Stmt::Analyze(target) => {
            validate_analyze_target(ctx.schema, target.as_ref())?;
            Ok(QueryOutput::default())
        }
        Stmt::AttachDatabase { .. } => Err(SqliteError::new(
            ErrorCode::Error,
            "ATTACH DATABASE is not yet supported before DatabaseSet (Step 2-C)",
        )),
        Stmt::DetachDatabase { .. } => Err(SqliteError::new(
            ErrorCode::Error,
            "DETACH DATABASE is not yet supported before DatabaseSet (Step 2-C)",
        )),
        Stmt::Pragma {
            schema,
            name,
            value,
        } => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
        _ => unreachable!("execute_nolock_stmt called on non-nolock statement"),
    }
}

/// Executes a `Read` statement body (`SELECT`, `EXPLAIN`, or read `PRAGMA`) inside an already-open
/// `Read` or `Write` B-tree transaction without ending the transaction.
pub(crate) fn execute_read_body(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    match stmt {
        Stmt::Explain { query_plan, stmt } => {
            if *query_plan {
                let rows = explain_query_plan(stmt, ctx.schema)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "id".to_owned(),
                        "parent".to_owned(),
                        "notused".to_owned(),
                        "detail".to_owned(),
                    ],
                    rows,
                })
            } else {
                let prog = compile_for_explain_with_functions(stmt, ctx.schema, ctx.functions)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "addr".to_owned(),
                        "opcode".to_owned(),
                        "p1".to_owned(),
                        "p2".to_owned(),
                        "p3".to_owned(),
                        "p4".to_owned(),
                        "p5".to_owned(),
                        "comment".to_owned(),
                    ],
                    rows: prog.explain_rows(),
                })
            }
        }
        Stmt::Select(sel) => exec_select(ctx, sel, params, &[], &CteStore::default()),
        Stmt::Pragma {
            schema,
            name,
            value,
        } => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
        _ => unreachable!("execute_read_body called on non-read statement"),
    }
}

/// Executes a `Write` statement body (`INSERT`, `UPDATE`, `DELETE`, DDL, `REINDEX`, or header-writing `PRAGMA`)
/// inside an already-open `Write` B-tree transaction without committing or rolling back.
pub(crate) fn execute_write_body(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    match stmt {
        Stmt::Insert(ins) => exec_insert(ctx, ins, params),
        Stmt::Update(upd) => exec_update(ctx, upd, params),
        Stmt::Delete(del) => exec_delete(ctx, del, params),
        Stmt::CreateTable(ct) => {
            exec_create_table(ctx, ct)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateTableAs {
            temp,
            if_not_exists,
            name,
            query,
        } => {
            exec_create_table_as(ctx, *temp, *if_not_exists, name, query, params)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropTable { if_exists, name } => {
            exec_drop_table(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateIndex(ci) => {
            exec_create_index(ctx, ci)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropIndex { if_exists, name } => {
            exec_drop_index(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::AlterTable { table, action } => {
            exec_alter_table(ctx, table, action)?;
            Ok(QueryOutput::default())
        }
        Stmt::Reindex(target) => {
            exec_reindex(ctx, target.as_ref())?;
            Ok(QueryOutput::default())
        }
        Stmt::Pragma {
            schema,
            name,
            value,
        } => {
            if let Some(s) = schema.as_deref()
                && !s.eq_ignore_ascii_case("main")
                && !s.eq_ignore_ascii_case("temp")
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            if schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
            {
                return Ok(QueryOutput::default());
            }
            let Some(pv) = value.as_ref() else {
                return Ok(QueryOutput::default());
            };
            match name.to_ascii_uppercase().as_str() {
                "USER_VERSION" => {
                    let v = pragma_to_i64(pv) as i32;
                    ctx.btree.pager_mut().header_mut()?.user_version = v;
                    Ok(QueryOutput::default())
                }
                "APPLICATION_ID" => {
                    let v = pragma_to_i64(pv) as i32;
                    ctx.btree.pager_mut().header_mut()?.application_id = v;
                    Ok(QueryOutput::default())
                }
                "DEFAULT_CACHE_SIZE" => {
                    let size = pragma_to_i64(pv).saturating_abs().min(i64::from(i32::MAX)) as i32;
                    ctx.btree.pager_mut().header_mut()?.default_cache_size = size;
                    ctx.btree.set_cache_size(size);
                    Ok(QueryOutput::default())
                }
                "AUTO_VACUUM" => {
                    let e_auto = parse_auto_vacuum_pragma(pv);
                    *ctx.next_auto_vacuum = Some(e_auto);
                    if ctx.btree.pager().header().largest_root_btree_page != 0 {
                        let incr_flag = u32::from(e_auto == AutoVacuumMode::Incremental);
                        ctx.btree.pager_mut().header_mut()?.incremental_vacuum_mode = incr_flag;
                        let _ = ctx.btree.set_configured_auto_vacuum(e_auto);
                    }
                    Ok(QueryOutput::default())
                }
                _ => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
            }
        }
        _ => unreachable!("execute_write_body called on non-write statement"),
    }
}

/// Top-level statement dispatcher for all SQL statements (`SELECT`, DML, DDL, `EXPLAIN`, `PRAGMA`, transactions).
pub fn execute_statement(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    if stmt_loads_schema_on_prepare(stmt) && ctx.btree.transaction_state() != TransactionState::Open
    {
        ctx.schema.ensure_fresh_in_tx(ctx.btree)?;
    }

    let out = match classify_stmt_category(stmt) {
        StmtCategory::Control => {
            execute_control_stmt(ctx, stmt)?;
            Ok(QueryOutput::default())
        }
        StmtCategory::NoLockPragma => execute_nolock_stmt(ctx, stmt),
        StmtCategory::Vacuum => {
            let Stmt::Vacuum { schema, into } = stmt else {
                unreachable!();
            };
            exec_vacuum(ctx, schema.as_deref(), into.as_ref(), params)
        }
        StmtCategory::IncrementalVacuum => {
            let Stmt::Pragma { schema, value, .. } = stmt else {
                unreachable!();
            };
            if let Some(s) = schema.as_deref()
                && !s.eq_ignore_ascii_case("main")
                && !s.eq_ignore_ascii_case("temp")
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            if schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
            {
                Ok(QueryOutput::default())
            } else {
                let mut remaining = parse_incremental_vacuum_limit(value.as_ref());
                run_write_autocommit(ctx, |ctx| {
                    let mut rows = Vec::new();
                    while remaining > 0 {
                        remaining -= 1;
                        if ctx.btree.incremental_vacuum_step()? {
                            rows.push(Vec::new());
                        } else {
                            break;
                        }
                    }
                    Ok(QueryOutput {
                        column_names: Vec::new(),
                        rows,
                    })
                })
            }
        }
        StmtCategory::Read => {
            if stmt_requires_schema(stmt) {
                run_read_autocommit(ctx, |ctx| execute_read_body(ctx, stmt, params))
            } else {
                run_header_read_autocommit(ctx, |ctx| execute_read_body(ctx, stmt, params))
            }
        }
        StmtCategory::Write => {
            run_write_autocommit(ctx, |ctx| execute_write_body(ctx, stmt, params))
        }
    }?;

    maybe_run_wal_autocheckpoint_in_ctx(ctx);
    Ok(out)
}

pub(crate) fn maybe_run_wal_autocheckpoint_in_ctx(ctx: &mut ExecContext<'_>) {
    if !is_in_autocommit(ctx) {
        return;
    }
    let n_entry = ctx.btree.pager_mut().take_wal_callback_frame_count();
    let threshold = *ctx.wal_autocheckpoint_threshold;
    if n_entry > 0
        && threshold > 0
        && i64::from(n_entry) >= i64::from(threshold)
        && ctx.btree.transaction_state() == TransactionState::Open
        && ctx.activity.other_readers == 0
        && ctx.activity.other_writers == 0
    {
        let _ = ctx.btree.wal_checkpoint(WalCheckpointMode::Passive, None);
    }
}

fn is_in_autocommit(ctx: &ExecContext<'_>) -> bool {
    !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx
}

fn run_header_read_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let started = ctx.btree.transaction_state() == TransactionState::Open;
    if started {
        ctx.busy_policy
            .retry_btree(ctx.btree, |bt| bt.begin_read())?;
        if !is_in_autocommit(ctx) {
            *ctx.in_user_btree_tx = true;
        }
    }
    let res = f(ctx);
    if started
        && !*ctx.in_user_btree_tx
        && ctx.activity.other_readers == 0
        && ctx.activity.other_writers == 0
        && ctx.btree.transaction_state() == TransactionState::Read
    {
        let _ = ctx.btree.pager_mut().end_read_if_unused();
    }
    res
}

fn run_read_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let started = ctx.btree.transaction_state() == TransactionState::Open;
    if started {
        ctx.busy_policy
            .retry_btree(ctx.btree, |bt| bt.begin_read())?;
        if !is_in_autocommit(ctx) {
            *ctx.in_user_btree_tx = true;
        }
    }
    let res = ctx
        .schema
        .ensure_fresh_in_tx(ctx.btree)
        .and_then(|()| f(ctx));
    if started
        && !*ctx.in_user_btree_tx
        && ctx.activity.other_readers == 0
        && ctx.activity.other_writers == 0
        && ctx.btree.transaction_state() == TransactionState::Read
    {
        let _ = ctx.btree.pager_mut().end_read_if_unused();
    }
    res
}

fn run_write_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let autocommit = is_in_autocommit(ctx);
    let need_stmt_sp = !autocommit || ctx.activity.other_writers > 0;
    let sql_sp_depth = ctx.savepoints.len();
    let stmt_sp_slot = need_stmt_sp.then_some(sql_sp_depth);
    let required_depth = sql_sp_depth + usize::from(need_stmt_sp);

    if ctx.btree.transaction_state() == TransactionState::Write {
        ctx.btree.pager_mut().ensure_subsavepoints(required_depth)?;
    } else {
        ctx.busy_policy.retry_btree(ctx.btree, |bt| {
            bt.pager_mut()
                .begin_write_with_savepoint_depth(required_depth)
        })?;
    }
    if !autocommit {
        *ctx.in_user_btree_tx = true;
    }

    if let Err(e) = ctx.schema.ensure_fresh_in_tx(ctx.btree) {
        if let Some(slot) = stmt_sp_slot {
            let _ = ctx.btree.pager_mut().rollback_to_subsavepoint(slot);
            let _ = ctx.btree.pager_mut().release_subsavepoint(slot);
        } else if autocommit && ctx.activity.other_writers == 0 {
            let retain_read = ctx.activity.other_readers > 0;
            let _ = ctx.btree.pager_mut().rollback_retaining_read(retain_read);
        }
        return Err(e);
    }

    let prev_total_changes = ctx.runtime.total_changes;
    let prev_last_rowid = ctx.runtime.last_insert_rowid;
    let prev_deferred = ctx.deferred_fk_suspects.clone();
    let prev_schema = ctx.schema.clone();
    let prev_schema_gen = *ctx.schema_generation;
    let prev_schema_changed = *ctx.schema_changed_in_tx;

    let step_res = f(ctx);
    let retain_read = ctx.activity.other_readers > 0;

    match step_res {
        Ok(out) => {
            if let Some(slot) = stmt_sp_slot {
                ctx.btree.pager_mut().release_subsavepoint(slot)?;
            }
            if autocommit && ctx.activity.other_writers == 0 {
                if let Err(fk_err) = check_deferred_fks(ctx) {
                    let write_only = !*ctx.schema_changed_in_tx;
                    let _ = ctx.btree.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        write_only,
                        retain_read && write_only,
                    );
                    *ctx.schema_changed_in_tx = false;
                    ctx.deferred_fk_suspects.clear();
                    ctx.runtime.total_changes = prev_total_changes;
                    ctx.runtime.last_changes = 0;
                    ctx.runtime.last_insert_rowid = prev_last_rowid;
                    if *ctx.schema_generation != prev_schema_gen {
                        *ctx.schema = prev_schema;
                        *ctx.schema_generation += 1;
                    }
                    ctx.publish();
                    return Err(fk_err);
                }
                if let Err(commit_err) = ctx.btree.commit_retaining_read(retain_read) {
                    let write_only = !*ctx.schema_changed_in_tx;
                    let _ = ctx.btree.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        write_only,
                        retain_read && write_only,
                    );
                    *ctx.schema_changed_in_tx = false;
                    ctx.deferred_fk_suspects.clear();
                    ctx.runtime.total_changes = prev_total_changes;
                    ctx.runtime.last_changes = 0;
                    ctx.runtime.last_insert_rowid = prev_last_rowid;
                    if *ctx.schema_generation != prev_schema_gen {
                        *ctx.schema = prev_schema;
                        *ctx.schema_generation += 1;
                    }
                    ctx.publish();
                    return Err(commit_err);
                }
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
            } else if ctx.activity.other_writers == 0 && ctx.activity.other_readers == 0 {
                ctx.btree.spill_if_over_cache_size()?;
            }
            ctx.publish();
            Ok(out)
        }
        Err(err) => {
            if err.conflict_effect == ConflictResolutionEffect::Fail
                && err.code == ErrorCode::Constraint
            {
                if let Some(slot) = stmt_sp_slot {
                    ctx.btree.pager_mut().release_subsavepoint(slot)?;
                }
                if autocommit && ctx.activity.other_writers == 0 {
                    if check_deferred_fks(ctx).is_err() {
                        let write_only = !*ctx.schema_changed_in_tx;
                        let _ = ctx.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            write_only,
                            retain_read && write_only,
                        );
                        *ctx.explicit_tx = false;
                        *ctx.implicit_savepoint_tx = false;
                        *ctx.in_user_btree_tx = false;
                        ctx.savepoints.clear();
                        *ctx.schema_changed_in_tx = false;
                        ctx.deferred_fk_suspects.clear();
                        ctx.runtime.total_changes = prev_total_changes;
                        ctx.runtime.last_changes = 0;
                        ctx.runtime.last_insert_rowid = prev_last_rowid;
                        if *ctx.schema_generation != prev_schema_gen {
                            *ctx.schema = prev_schema;
                            *ctx.schema_generation += 1;
                        }
                        ctx.publish();
                        return Err(err);
                    }
                    ctx.btree.commit_retaining_read(retain_read)?;
                    *ctx.schema_changed_in_tx = false;
                    ctx.deferred_fk_suspects.clear();
                }
                ctx.publish();
                Err(err)
            } else if err.conflict_effect == ConflictResolutionEffect::Rollback
                || (autocommit && ctx.activity.other_writers == 0)
                || err.code == ErrorCode::Full
            {
                let write_only = !*ctx.schema_changed_in_tx;
                let _ = ctx.btree.rollback_with_trip(
                    extended::SQLITE_ABORT_ROLLBACK,
                    write_only,
                    retain_read && write_only,
                );
                if !autocommit {
                    *ctx.rollback_count += 1;
                }
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                ctx.runtime.total_changes = prev_total_changes;
                ctx.runtime.last_changes = 0;
                ctx.runtime.last_insert_rowid = prev_last_rowid;
                if *ctx.schema_generation != prev_schema_gen {
                    *ctx.schema = prev_schema;
                    *ctx.schema_generation += 1;
                }
                ctx.publish();
                Err(err)
            } else {
                let stmt_changed_schema = *ctx.schema_generation != prev_schema_gen;
                let _ = ctx
                    .btree
                    .trip_cursors(extended::SQLITE_ABORT_ROLLBACK, !stmt_changed_schema);
                if let Some(slot) = stmt_sp_slot {
                    let _ = ctx.btree.pager_mut().rollback_to_subsavepoint(slot);
                    let _ = ctx.btree.pager_mut().release_subsavepoint(slot);
                }
                *ctx.deferred_fk_suspects = prev_deferred;
                ctx.runtime.total_changes = prev_total_changes;
                ctx.runtime.last_changes = 0;
                ctx.runtime.last_insert_rowid = prev_last_rowid;
                if stmt_changed_schema {
                    *ctx.schema = prev_schema;
                    *ctx.schema_generation += 1;
                    *ctx.schema_changed_in_tx = prev_schema_changed;
                }
                ctx.publish();
                Err(err)
            }
        }
    }
}

#[inline]
fn conflict_action_to_effect(action: ConflictAction) -> ConflictResolutionEffect {
    match action {
        ConflictAction::Rollback => ConflictResolutionEffect::Rollback,
        ConflictAction::Fail => ConflictResolutionEffect::Fail,
        ConflictAction::Abort | ConflictAction::Ignore | ConflictAction::Replace => {
            ConflictResolutionEffect::Abort
        }
    }
}

// ============================================================================
// Relational SELECT Engine (Joins, CTEs, Aggregates, Compounds, Subqueries)
// ============================================================================

fn exec_select(
    ctx: &mut ExecContext<'_>,
    stmt: &SelectStmt,
    params: &[Value],
    outer_scopes: &[EvalRow],
    parent_ctes: &CteStore,
) -> Result<QueryOutput> {
    let mut ctes = parent_ctes.with_scope(&stmt.ctes);

    // Propagate outer `LIMIT [OFFSET]` cap when `SELECT ... FROM <cte> LIMIT k` has no filtering/sorting/grouping,
    // allowing unbounded recursive CTEs (`WITH i(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM i) SELECT x FROM i LIMIT 10`)
    // to terminate immediately after `k` rows just like SQLite's co-routine generator.
    if stmt.body.compounds.is_empty()
        && stmt.order_by.is_empty()
        && let Some(ref lim) = stmt.limit
        && let SelectCore::Select {
            distinct: false,
            projection,
            from: Some(fc),
            selection: None,
            group_by,
            having: None,
        } = &stmt.body.head
        && group_by.is_empty()
        && fc.joins.is_empty()
        && !projection.iter().any(|rc| match rc {
            ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
            _ => false,
        })
        && let TableOrSubquery::Table { name, .. } = &fc.head
        && name.schema.is_none()
        && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
    {
        let lim_val =
            eval_expr(ctx, &lim.limit, params, outer_scopes, &ctes, None)?.to_integer_lossy();
        if lim_val >= 0 {
            let off_val = match &lim.offset {
                Some(oe) => eval_expr(ctx, oe, params, outer_scopes, &ctes, None)?
                    .to_integer_lossy()
                    .max(0) as usize,
                None => 0,
            };
            ctes.limit_hint = Some((s_idx, c_idx, off_val.saturating_add(lim_val as usize)));
        }
    }

    if stmt.body.compounds.is_empty() {
        exec_single_select_core(
            ctx,
            &stmt.body.head,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    } else {
        exec_compound_select(
            ctx,
            &stmt.body,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CteRecursionKind {
    NonRecursive,
    Recursive,
    InvalidCircular,
    MultipleRecursive,
}

fn count_table_refs_in_select(stmt: &SelectStmt, target: &str) -> usize {
    if stmt
        .ctes
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(target))
    {
        return 0;
    }
    let mut n = 0;
    for cte in &stmt.ctes {
        n += count_table_refs_in_select(&cte.query, target);
    }
    n += count_table_refs_in_core(&stmt.body.head, target);
    for (_, rhs) in &stmt.body.compounds {
        n += count_table_refs_in_core(rhs, target);
    }
    for ob in &stmt.order_by {
        n += count_table_refs_in_expr(&ob.expr, target);
    }
    if let Some(ref lim) = stmt.limit {
        n += count_table_refs_in_expr(&lim.limit, target);
        if let Some(ref off) = lim.offset {
            n += count_table_refs_in_expr(off, target);
        }
    }
    n
}

fn count_table_refs_in_core(core: &SelectCore, target: &str) -> usize {
    match core {
        SelectCore::Values(rows) => rows
            .iter()
            .flat_map(|r| r.iter())
            .map(|e| count_table_refs_in_expr(e, target))
            .sum(),
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let mut n = 0;
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    n += count_table_refs_in_expr(expr, target);
                }
            }
            if let Some(fc) = from {
                n += count_table_refs_in_tos(&fc.head, target);
                for j in &fc.joins {
                    n += count_table_refs_in_tos(&j.right, target);
                    if let Some(JoinConstraint::On(ref e)) = j.constraint {
                        n += count_table_refs_in_expr(e, target);
                    }
                }
            }
            if let Some(w) = selection {
                n += count_table_refs_in_expr(w, target);
            }
            for g in group_by {
                n += count_table_refs_in_expr(g, target);
            }
            if let Some(h) = having {
                n += count_table_refs_in_expr(h, target);
            }
            n
        }
    }
}

fn count_table_refs_in_tos(tos: &TableOrSubquery, target: &str) -> usize {
    match tos {
        TableOrSubquery::Table { name, .. } => {
            usize::from(name.schema.is_none() && name.name.eq_ignore_ascii_case(target))
        }
        TableOrSubquery::Subquery { query, .. } => count_table_refs_in_select(query, target),
    }
}

fn count_table_refs_in_expr(expr: &Expr, target: &str) -> usize {
    match expr {
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::IsNull { expr: e, .. }
        | Expr::Cast { expr: e, .. } => count_table_refs_in_expr(e, target),
        Expr::RowValue(es) => es.iter().map(|e| count_table_refs_in_expr(e, target)).sum(),
        Expr::Binary { left, right, .. } => {
            count_table_refs_in_expr(left, target) + count_table_refs_in_expr(right, target)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            count_table_refs_in_expr(e, target)
                + count_table_refs_in_expr(low, target)
                + count_table_refs_in_expr(high, target)
        }
        Expr::InList { expr: e, list, .. } => {
            count_table_refs_in_expr(e, target)
                + list
                    .iter()
                    .map(|x| count_table_refs_in_expr(x, target))
                    .sum::<usize>()
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => count_table_refs_in_expr(e, target) + count_table_refs_in_select(subquery, target),
        Expr::InTable { expr: e, table, .. } => {
            count_table_refs_in_expr(e, target)
                + usize::from(table.schema.is_none() && table.name.eq_ignore_ascii_case(target))
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            count_table_refs_in_expr(e, target)
                + count_table_refs_in_expr(pattern, target)
                + escape
                    .as_ref()
                    .map_or(0, |esc| count_table_refs_in_expr(esc, target))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_ref()
                .map_or(0, |o| count_table_refs_in_expr(o, target))
                + branches
                    .iter()
                    .map(|(w, t)| {
                        count_table_refs_in_expr(w, target) + count_table_refs_in_expr(t, target)
                    })
                    .sum::<usize>()
                + else_expr
                    .as_ref()
                    .map_or(0, |el| count_table_refs_in_expr(el, target))
        }
        Expr::FunctionCall { args, filter, .. } => {
            let a_cnt = match args {
                FunctionArgs::Star => 0,
                FunctionArgs::Exprs(es) => {
                    es.iter().map(|e| count_table_refs_in_expr(e, target)).sum()
                }
            };
            a_cnt
                + filter
                    .as_ref()
                    .map_or(0, |f| count_table_refs_in_expr(f, target))
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            count_table_refs_in_select(sub, target)
        }
        _ => 0,
    }
}

fn classify_cte_recursion(cte: &CommonTableExpr) -> CteRecursionKind {
    let total_refs = count_table_refs_in_select(&cte.query, &cte.name);
    if total_refs == 0 {
        return CteRecursionKind::NonRecursive;
    }
    let Some((last_op, last_rhs)) = cte.query.body.compounds.last() else {
        return CteRecursionKind::InvalidCircular;
    };
    if !matches!(last_op, CompoundOp::Union | CompoundOp::UnionAll) {
        return CteRecursionKind::InvalidCircular;
    }
    // Initial arm(s) must not reference the CTE itself
    let mut prefix_refs = count_table_refs_in_core(&cte.query.body.head, &cte.name);
    for (_, mid_core) in &cte.query.body.compounds[..cte.query.body.compounds.len() - 1] {
        prefix_refs += count_table_refs_in_core(mid_core, &cte.name);
    }
    if prefix_refs > 0 {
        return CteRecursionKind::InvalidCircular;
    }
    let rhs_total = count_table_refs_in_core(last_rhs, &cte.name);
    let mut direct_from_refs = 0usize;
    if let SelectCore::Select { from: Some(fc), .. } = last_rhs {
        if matches!(&fc.head, TableOrSubquery::Table { name, .. } if name.schema.is_none() && name.name.eq_ignore_ascii_case(&cte.name))
        {
            direct_from_refs += 1;
        }
        for j in &fc.joins {
            if matches!(&j.right, TableOrSubquery::Table { name, .. } if name.schema.is_none() && name.name.eq_ignore_ascii_case(&cte.name))
            {
                direct_from_refs += 1;
            }
        }
    }
    if direct_from_refs == 0 {
        return CteRecursionKind::InvalidCircular;
    }
    if rhs_total > 1 {
        return CteRecursionKind::MultipleRecursive;
    }
    CteRecursionKind::Recursive
}

fn resolve_cte(
    ctx: &mut ExecContext<'_>,
    ctes: &CteStore,
    name: &str,
    params: &[Value],
    outer_scopes: &[EvalRow],
) -> Result<Option<QueryOutput>> {
    let Some((s_idx, c_idx)) = ctes.locate(name) else {
        return Ok(None);
    };
    if let Some(working_out) = ctes.find_working(s_idx, c_idx) {
        return Ok(Some(working_out.clone()));
    }
    let cte = ctes.scopes[s_idx][c_idx].clone();
    if ctes.resolving.contains(&(s_idx, c_idx)) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        ));
    }

    let outer_limit_opt = match ctes.limit_hint {
        Some((hs, hc, max_rows)) if hs == s_idx && hc == c_idx => Some(max_rows),
        _ => None,
    };

    let mut eval_ctes = CteStore {
        scopes: ctes.scopes[..=s_idx].to_vec(),
        working: ctes.working.clone(),
        resolving: {
            let mut r = ctes.resolving.clone();
            r.push((s_idx, c_idx));
            r
        },
        limit_hint: None,
    };

    let mut cte_out = match classify_cte_recursion(&cte) {
        CteRecursionKind::NonRecursive => {
            exec_select(ctx, &cte.query, params, outer_scopes, &eval_ctes)?
        }
        CteRecursionKind::InvalidCircular => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("circular reference: {}", cte.name),
            ));
        }
        CteRecursionKind::MultipleRecursive => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("multiple recursive references: {}", cte.name),
            ));
        }
        CteRecursionKind::Recursive => exec_recursive_cte(
            ctx,
            s_idx,
            c_idx,
            &cte,
            outer_limit_opt,
            params,
            outer_scopes,
            &mut eval_ctes,
        )?,
    };

    if let Some(explicit_cols) = &cte.columns {
        if explicit_cols.len() != cte_out.column_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "table {} has {} values for {} columns",
                    cte.name,
                    cte_out.column_names.len(),
                    explicit_cols.len()
                ),
            ));
        }
        cte_out.column_names.clone_from(explicit_cols);
    }

    Ok(Some(cte_out))
}

const MAX_RECURSIVE_CTE_STEPS: usize = 250_000;

#[allow(clippy::too_many_arguments)]
fn exec_recursive_cte(
    ctx: &mut ExecContext<'_>,
    s_idx: usize,
    c_idx: usize,
    cte: &CommonTableExpr,
    outer_limit_opt: Option<usize>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    eval_ctes: &mut CteStore,
) -> Result<QueryOutput> {
    let rec_ctes = eval_ctes.with_scope(&cte.query.ctes);
    let last_idx = cte.query.body.compounds.len() - 1;
    let (rec_op, ref rec_core) = cte.query.body.compounds[last_idx];
    let is_union_distinct = rec_op == CompoundOp::Union;

    // 1. Evaluate non-recursive seed query (`head` + any preceding compound arms)
    let seed_body = SelectBody {
        head: cte.query.body.head.clone(),
        compounds: cte.query.body.compounds[..last_idx].to_vec(),
    };
    let seed_out = if seed_body.compounds.is_empty() {
        exec_single_select_core(
            ctx,
            &seed_body.head,
            &[],
            None,
            params,
            outer_scopes,
            &rec_ctes,
        )?
    } else {
        exec_compound_select(ctx, &seed_body, &[], None, params, outer_scopes, &rec_ctes)?
    };

    let head_col_names = seed_out.column_names;
    let working_col_names = match &cte.columns {
        Some(explicit) => {
            if explicit.len() != head_col_names.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} values for {} columns",
                        cte.name,
                        head_col_names.len(),
                        explicit.len()
                    ),
                ));
            }
            explicit.clone()
        }
        None => head_col_names.clone(),
    };

    // Validate recursive arm column count even if seed returns 0 rows!
    {
        let mut dry_ctes = rec_ctes.clone();
        dry_ctes.working.push((
            s_idx,
            c_idx,
            QueryOutput {
                column_names: working_col_names.clone(),
                rows: Vec::new(),
            },
        ));
        let dry_rhs =
            exec_single_select_core(ctx, rec_core, &[], None, params, outer_scopes, &dry_ctes)?;
        if dry_rhs.column_names.len() != working_col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    rec_op.as_sql_str()
                ),
            ));
        }
    }

    // Evaluate optional CTE-level `LIMIT` / `OFFSET`
    let (cte_limit_opt, cte_offset) = if let Some(ref lim) = cte.query.limit {
        let lv =
            eval_expr(ctx, &lim.limit, params, outer_scopes, &rec_ctes, None)?.to_integer_lossy();
        let ov = match &lim.offset {
            Some(oe) => eval_expr(ctx, oe, params, outer_scopes, &rec_ctes, None)?
                .to_integer_lossy()
                .max(0) as usize,
            None => 0,
        };
        if lv < 0 {
            (None, ov)
        } else {
            (Some(lv as usize), ov)
        }
    } else {
        (None, 0usize)
    };

    if cte_limit_opt == Some(0) || outer_limit_opt == Some(0) {
        return Ok(QueryOutput {
            column_names: working_col_names,
            rows: Vec::new(),
        });
    }

    // Prepare optional `ORDER BY` specs for priority-queue ordering (`with1-5.2.*`)
    let order_by = &cte.query.order_by;
    let order_specs: Vec<KeySortOrder> = order_by
        .iter()
        .map(|ob| {
            let coll = match &ob.expr {
                Expr::Collate { collation, .. } => {
                    Collation::from_name(collation).unwrap_or_default()
                }
                _ => Collation::Binary,
            };
            KeySortOrder {
                desc: ob.desc,
                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                collation: coll,
            }
        })
        .collect();

    let row_scoped_cols: Rc<[ScopedColumn]> = working_col_names
        .iter()
        .map(|cn| ScopedColumn {
            schema_name: None,
            table_alias: Some(cte.name.clone()),
            name: cn.clone(),
            collation: Collation::Binary,
            affinity: None,
            hidden_rowid: false,
            merged_using: false,
        })
        .collect::<Vec<_>>()
        .into();

    let compute_sort_keys = |ctx: &mut ExecContext<'_>, row: &[Value]| -> Result<Vec<Value>> {
        if order_by.is_empty() {
            return Ok(Vec::new());
        }
        let erow = EvalRow {
            cols: Rc::clone(&row_scoped_cols),
            vals: row.to_vec(),
        };
        let scopes = push_scope(outer_scopes, &erow);
        let mut keys = Vec::with_capacity(order_by.len());
        for (i, ob) in order_by.iter().enumerate() {
            let inner = match &ob.expr {
                Expr::Collate { expr, .. } => expr.as_ref(),
                other => other,
            };
            let v = match inner {
                Expr::Integer(ord) => {
                    if *ord < 1 || (*ord as usize) > working_col_names.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} ORDER BY term out of range - should be between 1 and {}",
                                ordinal_suffix(i + 1),
                                working_col_names.len()
                            ),
                        ));
                    }
                    row[(*ord as usize) - 1].clone()
                }
                _ => eval_expr(ctx, &ob.expr, params, &scopes, &rec_ctes, None)?,
            };
            keys.push(v);
        }
        Ok(keys)
    };

    let enc = ctx.btree.encoding();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut queue: VecDeque<(Vec<Value>, Vec<Value>)> = VecDeque::new();

    let enqueue_row = |queue: &mut VecDeque<(Vec<Value>, Vec<Value>)>,
                       seen: &mut HashSet<Vec<u8>>,
                       row: Vec<Value>,
                       sort_keys: Vec<Value>| {
        if is_union_distinct && !seen.insert(encode_record_with_encoding(&row, enc)) {
            return;
        }
        if order_specs.is_empty() {
            queue.push_back((row, sort_keys));
        } else {
            let pos = queue
                .iter()
                .position(|(_, existing_sk)| {
                    compare_value_slices(&sort_keys, existing_sk, &order_specs, enc)
                        == Ordering::Less
                })
                .unwrap_or(queue.len());
            queue.insert(pos, (row, sort_keys));
        }
    };

    for r in seed_out.rows {
        let sk = compute_sort_keys(ctx, &r)?;
        enqueue_row(&mut queue, &mut seen, r, sk);
    }

    let mut out_rows = Vec::new();
    let mut skipped = 0usize;
    let mut steps = 0usize;

    while let Some((cur_row, _)) = queue.pop_front() {
        ctx.step_tick()?;
        steps += 1;
        if steps > MAX_RECURSIVE_CTE_STEPS {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "recursive query exceeded maximum step limit",
            ));
        }

        if skipped < cte_offset {
            skipped += 1;
        } else {
            out_rows.push(cur_row.clone());
            if let Some(cl) = cte_limit_opt
                && out_rows.len() >= cl
            {
                break;
            }
            if let Some(ol) = outer_limit_opt
                && out_rows.len() >= ol
            {
                break;
            }
        }

        // Run recursive arm with `cur_row` as the single row in the working table
        let mut step_ctes = rec_ctes.clone();
        step_ctes.working.push((
            s_idx,
            c_idx,
            QueryOutput {
                column_names: working_col_names.clone(),
                rows: vec![cur_row],
            },
        ));
        let rhs_out =
            exec_single_select_core(ctx, rec_core, &[], None, params, outer_scopes, &step_ctes)?;
        if rhs_out.column_names.len() != working_col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    rec_op.as_sql_str()
                ),
            ));
        }
        for nr in rhs_out.rows {
            let sk = compute_sort_keys(ctx, &nr)?;
            enqueue_row(&mut queue, &mut seen, nr, sk);
        }
    }

    Ok(QueryOutput {
        column_names: working_col_names,
        rows: out_rows,
    })
}

fn exec_compound_select(
    ctx: &mut ExecContext<'_>,
    body: &SelectBody,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    let body_meta = infer_select_body_meta(body, outer_scopes, ctx.schema, ctes);
    let head_out = exec_single_select_core(ctx, &body.head, &[], None, params, outer_scopes, ctes)?;
    let col_names = head_out.column_names;
    let mut all_arm_col_names = vec![col_names.clone()];
    let mut acc_rows = head_out.rows;
    let col_collations: Vec<Collation> = (0..col_names.len())
        .map(|i| body_meta.get(i).map_or(Collation::Binary, |(_, c)| *c))
        .collect();
    let default_specs: Vec<KeySortOrder> = col_collations
        .iter()
        .map(|&coll| KeySortOrder {
            desc: false,
            nulls_last: false,
            collation: coll,
        })
        .collect();

    for (op, rhs_core) in &body.compounds {
        let rhs_out =
            exec_single_select_core(ctx, rhs_core, &[], None, params, outer_scopes, ctes)?;
        if rhs_out.column_names.len() != col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    op.as_sql_str()
                ),
            ));
        }
        all_arm_col_names.push(rhs_out.column_names);
        let enc = ctx.btree.encoding();
        match op {
            CompoundOp::UnionAll => {
                acc_rows.extend(rhs_out.rows);
            }
            CompoundOp::Union => {
                acc_rows.extend(rhs_out.rows);
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
            }
            CompoundOp::Intersect => {
                let mut rhs_sorted = rhs_out.rows;
                rhs_sorted.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
                acc_rows.retain(|r| {
                    rhs_sorted
                        .binary_search_by(|probe| {
                            compare_value_slices(probe, r, &default_specs, enc)
                        })
                        .is_ok()
                });
            }
            CompoundOp::Except => {
                let mut rhs_sorted = rhs_out.rows;
                rhs_sorted.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
                acc_rows.retain(|r| {
                    rhs_sorted
                        .binary_search_by(|probe| {
                            compare_value_slices(probe, r, &default_specs, enc)
                        })
                        .is_err()
                });
            }
        }
    }

    if !order_by.is_empty() {
        let enc = ctx.btree.encoding();
        sort_materialized_output(
            &mut acc_rows,
            &all_arm_col_names,
            &col_collations,
            order_by,
            enc,
        )?;
    }
    let acc_rows = apply_limit_offset(ctx, acc_rows, limit, params, outer_scopes, ctes)?;
    Ok(QueryOutput {
        column_names: col_names,
        rows: acc_rows,
    })
}

fn strip_collate_and_unary_plus(mut e: &Expr) -> Option<i64> {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    match e {
        Expr::Integer(i) => Some(*i),
        Expr::Unary {
            op: UnaryOp::Negate,
            expr,
        } => {
            if let Expr::Integer(i) = expr.as_ref() {
                Some(i.saturating_neg())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn ordinal_suffix(n: usize) -> String {
    match n % 100 {
        11..=13 => format!("{n}th"),
        _ => match n % 10 {
            1 => format!("{n}st"),
            2 => format!("{n}nd"),
            3 => format!("{n}rd"),
            _ => format!("{n}th"),
        },
    }
}

fn exec_single_select_core(
    ctx: &mut ExecContext<'_>,
    core: &SelectCore,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    match core {
        SelectCore::Values(expr_rows) => {
            if expr_rows.is_empty() {
                return Ok(QueryOutput::default());
            }
            let width = expr_rows[0].len();
            let col_names: Vec<String> = (1..=width).map(|i| format!("column{i}")).collect();
            let mut rows = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                if erow.len() != width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "all VALUES must have the same number of terms",
                    ));
                }
                let mut vals = Vec::with_capacity(width);
                for e in erow {
                    vals.push(eval_expr(ctx, e, params, outer_scopes, ctes, None)?);
                }
                rows.push(vals);
            }
            if !order_by.is_empty() {
                let enc = ctx.btree.encoding();
                let arm_cols = vec![col_names.clone()];
                let default_colls = vec![Collation::Binary; col_names.len()];
                sort_materialized_output(&mut rows, &arm_cols, &default_colls, order_by, enc)?;
            }
            let rows = apply_limit_offset(ctx, rows, limit, params, outer_scopes, ctes)?;
            Ok(QueryOutput {
                column_names: col_names,
                rows,
            })
        }
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            // 1. Evaluate FROM & JOINs
            let source_rows = match from {
                None => vec![EvalRow::default()],
                Some(fc) => {
                    eval_from_clause(ctx, fc, selection.as_ref(), params, outer_scopes, ctes)?
                }
            };

            // 2. Filter via WHERE
            let mut filtered = Vec::with_capacity(source_rows.len());
            if let Some(where_expr) = selection {
                for r in source_rows {
                    ctx.step_tick()?;
                    let scopes = push_scope(outer_scopes, &r);
                    let v = eval_expr(ctx, where_expr, params, &scopes, ctes, None)?;
                    if v.truthiness() == Some(true) {
                        filtered.push(r);
                    }
                }
            } else {
                filtered = source_rows;
            }

            // 3. Determine whether query is aggregated
            let is_agg = !group_by.is_empty()
                || having.is_some()
                || projection.iter().any(|rc| match rc {
                    ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                    _ => false,
                })
                || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr));

            let sample_cols: Vec<ScopedColumn> = match filtered.first() {
                Some(r) => r.cols.to_vec(),
                None => match from {
                    Some(fc) => {
                        schema_cols_preview(ctx, fc, params, outer_scopes, ctes).unwrap_or_default()
                    }
                    None => Vec::new(),
                },
            };

            let (col_names, expanded_proj) = expand_projection(projection, &sample_cols)?;

            // Validate & resolve GROUP BY / ORDER BY integer ordinals (1..=N) & SELECT aliases
            let mut resolved_group_by = Vec::with_capacity(group_by.len());
            for (gi, ge) in group_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(ge) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} GROUP BY term out of range - should be between 1 and {}",
                                ordinal_suffix(gi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                    let target_expr = expanded_proj[(ord as usize) - 1].0.clone();
                    if let Expr::Collate { collation, .. } = ge {
                        resolved_group_by.push(Expr::Collate {
                            expr: Box::new(target_expr),
                            collation: collation.clone(),
                        });
                    } else {
                        resolved_group_by.push(target_expr);
                    }
                } else {
                    resolved_group_by.push(subst_select_aliases_in_expr(
                        ge,
                        &expanded_proj,
                        &sample_cols,
                        outer_scopes,
                    ));
                }
            }
            let resolved_having = having.as_ref().map(|hav| {
                subst_select_aliases_in_expr(hav, &expanded_proj, &sample_cols, outer_scopes)
            });
            for (oi, ob) in order_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(&ob.expr)
                    && (ord < 1 || (ord as usize) > expanded_proj.len())
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "{} ORDER BY term out of range - should be between 1 and {}",
                            ordinal_suffix(oi + 1),
                            expanded_proj.len()
                        ),
                    ));
                }
            }

            let mut result_rows_with_sort: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();

            if is_agg {
                let mut minmax_specs: Vec<(AggregateKind, Expr, Option<Expr>, Collation)> =
                    Vec::new();
                for (pe, _, _) in &expanded_proj {
                    collect_minmax_aggregates(pe, &sample_cols, &mut minmax_specs);
                }
                if minmax_specs.is_empty() {
                    if let Some(ref hav) = resolved_having {
                        collect_minmax_aggregates(hav, &sample_cols, &mut minmax_specs);
                    }
                    for ob in order_by {
                        collect_minmax_aggregates(&ob.expr, &sample_cols, &mut minmax_specs);
                    }
                }

                let groups = build_groups(
                    ctx,
                    filtered,
                    &resolved_group_by,
                    params,
                    outer_scopes,
                    ctes,
                )?;
                for group_rows in groups {
                    let rep_row = choose_aggregate_representative_row(
                        ctx,
                        &group_rows,
                        &sample_cols,
                        &minmax_specs,
                        params,
                        outer_scopes,
                        ctes,
                    )?;
                    let scopes = push_scope(outer_scopes, &rep_row);
                    if let Some(ref hav) = resolved_having {
                        let hv = eval_expr(
                            ctx,
                            hav,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        if hv.truthiness() != Some(true) {
                            continue;
                        }
                    }

                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(
                            ctx,
                            pe,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            } else {
                if filtered.is_empty() && !sample_cols.is_empty() {
                    // Validate projection & ORDER BY column resolution even when 0 rows match!
                    let dummy_row = EvalRow {
                        cols: Rc::from(sample_cols.as_slice()),
                        vals: vec![Value::Null; sample_cols.len()],
                    };
                    let scopes = push_scope(outer_scopes, &dummy_row);
                    let mut dummy_proj = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        dummy_proj.push(eval_expr(ctx, pe, params, &scopes, ctes, None)?);
                    }
                    for ob in order_by {
                        let _ = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &dummy_proj,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            None,
                        )?;
                    }
                }
                for row in filtered {
                    ctx.step_tick()?;
                    let scopes = push_scope(outer_scopes, &row);
                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(ctx, pe, params, &scopes, ctes, None)?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            None,
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            }

            if *distinct {
                let enc = ctx.btree.encoding();
                let dist_specs: Vec<KeySortOrder> = expanded_proj
                    .iter()
                    .map(|(pe, _, _)| KeySortOrder {
                        desc: false,
                        nulls_last: false,
                        collation: infer_expr_collation_from_cols(pe, &sample_cols),
                    })
                    .collect();
                let has_custom_coll = dist_specs.iter().any(|s| s.collation != Collation::Binary);
                if !has_custom_coll {
                    let mut unique_rows: Vec<Vec<Value>> = Vec::new();
                    let mut seen_bytes = HashSet::new();
                    result_rows_with_sort.retain(|(proj, _)| {
                        let norm: Vec<Value> = proj
                            .iter()
                            .map(|v| match v {
                                Value::Real(r)
                                    if (*r as i64 as f64) == *r
                                        && *r >= (i64::MIN as f64)
                                        && *r < 9223372036854775808.0 =>
                                {
                                    Value::Integer(*r as i64)
                                }
                                other => other.clone(),
                            })
                            .collect();
                        let _ = &mut unique_rows;
                        seen_bytes.insert(encode_record_with_encoding(&norm, enc))
                    });
                } else {
                    let mut seen_rows: Vec<Vec<Value>> = Vec::new();
                    result_rows_with_sort.retain(|(proj, _)| {
                        if seen_rows.iter().any(|prev| {
                            compare_value_slices(prev, proj, &dist_specs, enc) == Ordering::Equal
                        }) {
                            false
                        } else {
                            seen_rows.push(proj.clone());
                            true
                        }
                    });
                }
            }

            if !order_by.is_empty() {
                let enc = ctx.btree.encoding();
                let specs: Vec<(KeySortOrder, Option<String>)> = order_by
                    .iter()
                    .map(|ob| {
                        let custom_coll = match &ob.expr {
                            Expr::Collate { collation, .. }
                                if Collation::from_name(collation).is_none() =>
                            {
                                Some(collation.clone())
                            }
                            _ => None,
                        };
                        let coll = infer_order_by_collation(&ob.expr, &expanded_proj, &sample_cols);
                        (
                            KeySortOrder {
                                desc: ob.desc,
                                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                                collation: coll,
                            },
                            custom_coll,
                        )
                    })
                    .collect();

                let collations = ctx.collations;
                result_rows_with_sort.sort_by(|(_, sa), (_, sb)| {
                    for (i, (spec, custom_opt)) in specs.iter().enumerate() {
                        let a = &sa[i];
                        let b = &sb[i];
                        let ord = match (a.is_null(), b.is_null()) {
                            (true, true) => Ordering::Equal,
                            (true, false) => {
                                if spec.nulls_last {
                                    Ordering::Greater
                                } else {
                                    Ordering::Less
                                }
                            }
                            (false, true) => {
                                if spec.nulls_last {
                                    Ordering::Less
                                } else {
                                    Ordering::Greater
                                }
                            }
                            (false, false) => {
                                let raw = if let (Some(cname), Value::Text(ls), Value::Text(rs)) =
                                    (custom_opt.as_deref(), a, b)
                                {
                                    collations
                                        .compare(cname, ls, rs, enc)
                                        .unwrap_or(Ordering::Equal)
                                } else {
                                    a.cmp_sqlite_with_encoding(b, spec.collation, enc)
                                };
                                if spec.desc { raw.reverse() } else { raw }
                            }
                        };
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    Ordering::Equal
                });
            }

            let out_rows: Vec<Vec<Value>> =
                result_rows_with_sort.into_iter().map(|(p, _)| p).collect();
            let out_rows = apply_limit_offset(ctx, out_rows, limit, params, outer_scopes, ctes)?;

            Ok(QueryOutput {
                column_names: col_names,
                rows: out_rows,
            })
        }
    }
}

fn collect_minmax_aggregates(
    expr: &Expr,
    sample_cols: &[ScopedColumn],
    out: &mut Vec<(AggregateKind, Expr, Option<Expr>, Collation)>,
) {
    match expr {
        Expr::FunctionCall {
            name,
            distinct: _,
            args: FunctionArgs::Exprs(es),
            filter,
        } if es.len() == 1
            && (name.eq_ignore_ascii_case("MIN") || name.eq_ignore_ascii_case("MAX")) =>
        {
            let kind = if name.eq_ignore_ascii_case("MIN") {
                AggregateKind::Min
            } else {
                AggregateKind::Max
            };
            let coll = infer_expr_collation_from_cols(&es[0], sample_cols);
            out.push((kind, es[0].clone(), filter.as_deref().cloned(), coll));
        }
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(es),
            filter,
            ..
        } => {
            if !expr_contains_aggregate(expr) {
                for a in es {
                    collect_minmax_aggregates(a, sample_cols, out);
                }
                if let Some(f) = filter {
                    collect_minmax_aggregates(f, sample_cols, out);
                }
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => collect_minmax_aggregates(inner, sample_cols, out),
        Expr::Binary { left, right, .. } => {
            collect_minmax_aggregates(left, sample_cols, out);
            collect_minmax_aggregates(right, sample_cols, out);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_minmax_aggregates(e, sample_cols, out);
            collect_minmax_aggregates(low, sample_cols, out);
            collect_minmax_aggregates(high, sample_cols, out);
        }
        Expr::InList { expr: e, list, .. } => {
            collect_minmax_aggregates(e, sample_cols, out);
            for item in list {
                collect_minmax_aggregates(item, sample_cols, out);
            }
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_minmax_aggregates(e, sample_cols, out);
            collect_minmax_aggregates(pattern, sample_cols, out);
            if let Some(esc) = escape {
                collect_minmax_aggregates(esc, sample_cols, out);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_minmax_aggregates(op, sample_cols, out);
            }
            for (w, t) in branches {
                collect_minmax_aggregates(w, sample_cols, out);
                collect_minmax_aggregates(t, sample_cols, out);
            }
            if let Some(el) = else_expr {
                collect_minmax_aggregates(el, sample_cols, out);
            }
        }
        Expr::RowValue(elems) => {
            for el in elems {
                collect_minmax_aggregates(el, sample_cols, out);
            }
        }
        _ => {}
    }
}

/// Chooses the group representative row for bare columns per SQLite `select.c` (`updateAccumulator`):
/// when `MIN(x)` or `MAX(x)` is present, bare columns come from the row achieving the extremum.
fn choose_aggregate_representative_row(
    ctx: &mut ExecContext<'_>,
    group_rows: &[EvalRow],
    sample_cols: &[ScopedColumn],
    minmax_specs: &[(AggregateKind, Expr, Option<Expr>, Collation)],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<EvalRow> {
    let Some(first_row) = group_rows.first() else {
        return Ok(EvalRow {
            cols: Rc::from(sample_cols),
            vals: vec![Value::Null; sample_cols.len()],
        });
    };
    let Some((kind, arg_expr, filt_expr, coll)) = minmax_specs.last() else {
        return Ok(first_row.clone());
    };

    let mut best_val: Option<Value> = None;
    let mut best_idx: Option<usize> = None;
    for (idx, gr) in group_rows.iter().enumerate() {
        let row_scopes = push_scope(outer_scopes, gr);
        if let Some(fe) = filt_expr {
            let fv = eval_expr(ctx, fe, params, &row_scopes, ctes, None)?;
            if fv.truthiness() != Some(true) {
                continue;
            }
        }
        let val = eval_expr(ctx, arg_expr, params, &row_scopes, ctes, None)?;
        if val.is_null() {
            continue;
        }
        let is_better = match &best_val {
            None => true,
            Some(cur) => match kind {
                AggregateKind::Min => val.cmp_sqlite(cur, *coll).is_lt(),
                AggregateKind::Max => val.cmp_sqlite(cur, *coll).is_gt(),
                _ => false,
            },
        };
        if is_better {
            best_val = Some(val);
            best_idx = Some(idx);
        }
    }

    Ok(group_rows[best_idx.unwrap_or(0)].clone())
}

fn split_where_conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        split_where_conjuncts(left, out);
        split_where_conjuncts(right, out);
    } else {
        out.push(expr);
    }
}

fn resolve_unique_col_idx_cref(cols: &[ScopedColumn], cref: &ColumnRef) -> Result<Option<usize>> {
    let mut found = Vec::new();
    for (i, sc) in cols.iter().enumerate() {
        if sc.hidden_rowid {
            continue;
        }
        if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
            continue;
        }
        if sc.name.eq_ignore_ascii_case(&cref.column) {
            if cref.schema.is_none() && cref.table.is_none() && sc.merged_using {
                return Ok(Some(i));
            }
            found.push(i);
        }
    }
    if found.len() == 1 {
        return Ok(Some(found[0]));
    }
    if found.len() > 1 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("ambiguous column name: {}", cref.column),
        ));
    }
    if cref.column.eq_ignore_ascii_case("rowid")
        || cref.column.eq_ignore_ascii_case("_rowid_")
        || cref.column.eq_ignore_ascii_case("oid")
    {
        for (i, sc) in cols.iter().enumerate() {
            if !sc.hidden_rowid {
                continue;
            }
            if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                continue;
            }
            found.push(i);
        }
        if found.len() == 1 {
            return Ok(Some(found[0]));
        }
        if found.len() > 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("ambiguous column name: {}", cref.column),
            ));
        }
    }
    Ok(None)
}

fn expr_can_eval_with_cols(
    expr: &Expr,
    avail_cols: &[ScopedColumn],
    full_scope_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => true,
        Expr::Subquery(_) | Expr::Exists { .. } => false,
        Expr::Column(cref) => match resolve_unique_col_idx_cref(full_scope_cols, cref) {
            Err(_) => false, // Ambiguous in full join scope: do NOT push down!
            Ok(Some(full_idx)) => {
                let full_sc = &full_scope_cols[full_idx];
                if full_sc.merged_using {
                    return false;
                }
                match resolve_unique_col_idx_cref(avail_cols, cref) {
                    Ok(Some(avail_idx)) => {
                        let avail_sc = &avail_cols[avail_idx];
                        !avail_sc.merged_using
                            && avail_sc.schema_name == full_sc.schema_name
                            && avail_sc.table_alias == full_sc.table_alias
                            && avail_sc.name == full_sc.name
                            && avail_sc.hidden_rowid == full_sc.hidden_rowid
                    }
                    _ => false,
                }
            }
            Ok(None) => {
                // Not in `full_scope_cols`: resolves in `outer_scopes` iff unambiguous there
                // and not accidentally matching anything in `avail_cols`.
                if resolve_unique_col_idx_cref(avail_cols, cref)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    return false;
                }
                outer_scopes
                    .iter()
                    .rev()
                    .find_map(|os| match os.lookup_cref_full(cref) {
                        Err(_) => Some(false),
                        Ok(Some(_)) => Some(true),
                        Ok(None) => None,
                    })
                    .unwrap_or(false)
            }
        },
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            expr_can_eval_with_cols(inner, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::Binary { left, op: _, right } => {
            expr_can_eval_with_cols(left, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(right, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(low, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(high, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && list.iter().all(|it| {
                    expr_can_eval_with_cols(it, avail_cols, full_scope_cols, outer_scopes)
                })
        }
        Expr::InSubquery { .. } | Expr::InTable { .. } => false,
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(pattern, avail_cols, full_scope_cols, outer_scopes)
                && escape.as_deref().is_none_or(|esc| {
                    expr_can_eval_with_cols(esc, avail_cols, full_scope_cols, outer_scopes)
                })
        }
        Expr::FunctionCall { args, filter, .. } => {
            if expr_contains_aggregate(expr) || filter.is_some() {
                return false;
            }
            match args {
                FunctionArgs::Star => true,
                FunctionArgs::Exprs(es) => es
                    .iter()
                    .all(|a| expr_can_eval_with_cols(a, avail_cols, full_scope_cols, outer_scopes)),
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_none_or(|op| {
                expr_can_eval_with_cols(op, avail_cols, full_scope_cols, outer_scopes)
            }) && branches.iter().all(|(w, t)| {
                expr_can_eval_with_cols(w, avail_cols, full_scope_cols, outer_scopes)
                    && expr_can_eval_with_cols(t, avail_cols, full_scope_cols, outer_scopes)
            }) && else_expr.as_deref().is_none_or(|el| {
                expr_can_eval_with_cols(el, avail_cols, full_scope_cols, outer_scopes)
            })
        }
        Expr::RowValue(elems) => elems
            .iter()
            .all(|el| expr_can_eval_with_cols(el, avail_cols, full_scope_cols, outer_scopes)),
    }
}

fn eval_from_clause(
    ctx: &mut ExecContext<'_>,
    fc: &FromClause,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    if 1 + fc.joins.len() > 64 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "at most 64 tables in a join",
        ));
    }
    let no_using_anywhere = fc
        .joins
        .iter()
        .all(|j| !j.natural && !matches!(j.constraint, Some(JoinConstraint::Using(_))));
    let full_from_cols_opt = if no_using_anywhere && !fc.joins.is_empty() && where_hint.is_some() {
        schema_cols_preview(ctx, fc, params, outer_scopes, ctes)
    } else {
        None
    };
    let mut pushable_conjuncts: Vec<&Expr> = Vec::new();
    if let (Some(wh), Some(_)) = (where_hint, &full_from_cols_opt) {
        split_where_conjuncts(wh, &mut pushable_conjuncts);
    }
    let mut applied = vec![false; pushable_conjuncts.len()];

    let single_table_hint = if fc.joins.is_empty() {
        where_hint
    } else {
        None
    };
    let mut left_rows =
        eval_table_or_subquery(ctx, &fc.head, single_table_hint, params, outer_scopes, ctes)?;
    let mut left_cols_fallback: Vec<ScopedColumn> = left_rows
        .first()
        .map(|r| r.cols.to_vec())
        .or_else(|| schema_cols_for_tos(ctx, &fc.head, params, outer_scopes, ctes))
        .unwrap_or_default();

    // Push single-table WHERE conjuncts on `fc.head` when no later join is RIGHT or FULL
    let no_right_or_full_after_head = fc
        .joins
        .iter()
        .all(|j| !matches!(j.join_type, JoinType::Right | JoinType::Full));
    if no_right_or_full_after_head && let Some(ref full_cols) = full_from_cols_opt {
        for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
            if !applied[ci]
                && expr_can_eval_with_cols(conj, &left_cols_fallback, full_cols, outer_scopes)
            {
                applied[ci] = true;
                let mut kept = Vec::with_capacity(left_rows.len());
                for r in left_rows {
                    let sc = push_scope(outer_scopes, &r);
                    if eval_expr(ctx, conj, params, &sc, ctes, None)?.truthiness() == Some(true) {
                        kept.push(r);
                    }
                }
                left_rows = kept;
            }
        }
    }

    for (j_idx, join) in fc.joins.iter().enumerate() {
        let right_rows =
            eval_table_or_subquery(ctx, &join.right, None, params, outer_scopes, ctes)?;
        let right_cols_fallback: Vec<ScopedColumn> = right_rows
            .first()
            .map(|r| r.cols.to_vec())
            .or_else(|| schema_cols_for_tos(ctx, &join.right, params, outer_scopes, ctes))
            .unwrap_or_default();
        let next_merged_cols =
            merge_join_scoped_columns(&left_cols_fallback, &right_cols_fallback, join);

        let no_later_right_or_full = fc.joins[(j_idx + 1)..]
            .iter()
            .all(|j| !matches!(j.join_type, JoinType::Right | JoinType::Full));

        let mut in_join_where_conjuncts: Vec<&Expr> = Vec::new();
        if no_later_right_or_full
            && matches!(join.join_type, JoinType::Inner | JoinType::Cross)
            && let Some(ref full_cols) = full_from_cols_opt
        {
            for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
                if !applied[ci]
                    && expr_can_eval_with_cols(conj, &next_merged_cols, full_cols, outer_scopes)
                {
                    applied[ci] = true;
                    in_join_where_conjuncts.push(conj);
                }
            }
        }

        left_rows = apply_join(
            ctx,
            left_rows,
            right_rows,
            &left_cols_fallback,
            &right_cols_fallback,
            join,
            &in_join_where_conjuncts,
            params,
            outer_scopes,
            ctes,
        )?;
        left_cols_fallback = next_merged_cols;

        // Post-join WHERE pushdown whenever all remaining joins in `fc.joins[(j_idx + 1)..]`
        // are `INNER`/`CROSS`/`LEFT` (none can NULL-extend or right-match `left_rows`)!
        if no_later_right_or_full && let Some(ref full_cols) = full_from_cols_opt {
            for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
                if !applied[ci]
                    && expr_can_eval_with_cols(conj, &left_cols_fallback, full_cols, outer_scopes)
                {
                    applied[ci] = true;
                    let mut kept = Vec::with_capacity(left_rows.len());
                    for r in left_rows {
                        if outer_scopes.is_empty() {
                            if eval_expr(ctx, conj, params, std::slice::from_ref(&r), ctes, None)?
                                .truthiness()
                                == Some(true)
                            {
                                kept.push(r);
                            }
                        } else {
                            let sc = push_scope(outer_scopes, &r);
                            if eval_expr(ctx, conj, params, &sc, ctes, None)?.truthiness()
                                == Some(true)
                            {
                                kept.push(r);
                            }
                        }
                    }
                    left_rows = kept;
                }
            }
        }
    }

    Ok(left_rows)
}

fn eval_table_or_subquery(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    match tos {
        TableOrSubquery::Table {
            name,
            alias,
            indexed_by,
        } => {
            let eff_alias = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some(cte_out) = resolve_cte(ctx, ctes, &name.name, params, outer_scopes)?
            {
                if indexed_by.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot use INDEXED BY or NOT INDEXED on CTE or view",
                    ));
                }
                let cte_meta = infer_cte_meta_by_name(&name.name, outer_scopes, ctx.schema, ctes);
                let cols: Rc<[ScopedColumn]> = cte_out
                    .column_names
                    .iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = cte_meta
                            .get(i)
                            .copied()
                            .unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: Some(eff_alias.clone()),
                            name: cn.clone(),
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            merged_using: false,
                        }
                    })
                    .collect::<Vec<_>>()
                    .into();
                return Ok(cte_out
                    .rows
                    .into_iter()
                    .map(|r| EvalRow {
                        cols: Rc::clone(&cols),
                        vals: r,
                    })
                    .collect());
            }

            let (tbl, schema_lbl) = resolve_table_for_query(ctx.schema, name)?;

            let raw_entries = if tbl.root_pgno == 0 {
                Vec::new()
            } else {
                read_table_with_plan(
                    ctx,
                    &tbl,
                    alias.as_deref(),
                    indexed_by.as_ref(),
                    where_hint,
                    params,
                    outer_scopes,
                    ctes,
                )?
            };

            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff_alias.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff_alias),
                    name: "rowid".to_owned(),
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    merged_using: false,
                });
            }

            let shared_cols: Rc<[ScopedColumn]> = cols.into();
            let mut out = Vec::with_capacity(raw_entries.len());
            for (rkey, payload) in raw_entries {
                let mut vals = decode_table_logical_row(ctx, &tbl, &rkey, &payload, params)?;
                if let TableRowKey::Rowid(rowid) = rkey {
                    vals.push(Value::Integer(rowid));
                }
                out.push(EvalRow {
                    cols: Rc::clone(&shared_cols),
                    vals,
                });
            }
            Ok(out)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, ctx.schema, ctes);
            let sub_out = exec_select(ctx, query, params, outer_scopes, ctes)?;
            let cols: Rc<[ScopedColumn]> = sub_out
                .column_names
                .iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = sub_meta
                        .get(i)
                        .copied()
                        .unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: None,
                        table_alias: alias.clone(),
                        name: cn.clone(),
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        merged_using: false,
                    }
                })
                .collect::<Vec<_>>()
                .into();
            Ok(sub_out
                .rows
                .into_iter()
                .map(|vals| EvalRow {
                    cols: Rc::clone(&cols),
                    vals,
                })
                .collect())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn read_table_with_plan(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    alias: Option<&str>,
    indexed_by: Option<&IndexHint>,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<(TableRowKey, Vec<u8>)>> {
    if let Some(hint) = indexed_by {
        match hint {
            IndexHint::NotIndexed => {
                if ctx.btree.pager().page_count() == 0 {
                    return Ok(Vec::new());
                }
                return scan_table_raw_entries(ctx, tbl);
            }
            IndexHint::IndexedBy(idx_name) => {
                let Some(idx) = ctx
                    .schema
                    .indexes_for_table(&tbl.name)
                    .into_iter()
                    .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))
                else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such index: {idx_name}"),
                    ));
                };
                if let Some(ref pw) = idx.where_clause {
                    let usable = where_hint.is_some_and(|wh| expr_contains_conjunct(wh, pw));
                    if !usable {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("cannot use index: {idx_name}"),
                        ));
                    }
                }
                if ctx.btree.pager().page_count() == 0 {
                    return Ok(Vec::new());
                }
                let enc = ctx.btree.encoding();
                let entries = ctx.scan_index(idx.root_pgno, idx.key_sort_orders())?;
                let mut rows = Vec::with_capacity(entries.len());
                for rec_bytes in entries {
                    let fields = decode_record_with_encoding(&rec_bytes, enc)?;
                    if tbl.without_rowid {
                        let pk = idx.extract_without_rowid_pk_from_hit(tbl, &fields);
                        let rkey = TableRowKey::WithoutRowidPk(pk);
                        if let Some(payload) = seek_table_raw_entry(ctx, tbl, &rkey)? {
                            rows.push((rkey, payload));
                        }
                    } else if let Some(Value::Integer(rid)) = fields.last()
                        && let Some(payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, *rid)?
                    {
                        rows.push((TableRowKey::Rowid(*rid), payload));
                    }
                }
                return Ok(rows);
            }
        }
    }

    if ctx.btree.pager().page_count() == 0 {
        return Ok(Vec::new());
    }
    let plan = choose_access_plan(tbl, alias, where_hint, ctx.schema);
    match plan {
        AccessPlan::FullScan => scan_table_raw_entries(ctx, tbl),
        AccessPlan::RowidSeek { key_expr } => {
            let kv = eval_expr(ctx, &key_expr, params, outer_scopes, ctes, None)?;
            let target = match kv {
                Value::Integer(i) => Some(i),
                Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                Value::Text(ref s) => sqliters_types::parse_numeric_text(s).and_then(|v| match v {
                    Value::Integer(i) => Some(i),
                    Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                    _ => None,
                }),
                _ => None,
            };
            let Some(rid) = target else {
                return Ok(Vec::new());
            };
            if let Some(payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, rid)? {
                Ok(vec![(TableRowKey::Rowid(rid), payload)])
            } else {
                Ok(Vec::new())
            }
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
        } => {
            let mut prefix_vals = Vec::with_capacity(prefix_exprs.len());
            for (i, ke) in prefix_exprs.iter().enumerate() {
                let raw = eval_expr(ctx, ke, params, outer_scopes, ctes, None)?;
                let aff = index.columns[i]
                    .col_idx
                    .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                let coerced = aff.apply(raw);
                if coerced.is_null() {
                    // `col = NULL` is never true in SQL `WHERE`!
                    return Ok(Vec::new());
                }
                prefix_vals.push(coerced);
            }
            if tbl.without_rowid && index.origin == "pk" && index.root_pgno == tbl.root_pgno {
                let pk_orders = tbl.without_rowid_pk_orders();
                let enc = ctx.btree.encoding();
                let raw_hits = ctx.btree.seek_index_equal_raw(
                    tbl.root_pgno,
                    &prefix_vals,
                    &pk_orders[..prefix_vals.len()],
                )?;
                let n_pk = tbl.without_rowid_pk_cols.len();
                let mut rows = Vec::with_capacity(raw_hits.len());
                for rec_bytes in raw_hits {
                    let decoded = decode_record_with_encoding(&rec_bytes, enc)?;
                    let pk = decoded[..n_pk.min(decoded.len())].to_vec();
                    rows.push((TableRowKey::WithoutRowidPk(pk), rec_bytes));
                }
                return Ok(rows);
            }
            let orders = index.key_sort_orders();
            let idx_hits = ctx
                .btree
                .seek_index_equal(index.root_pgno, &prefix_vals, &orders)?;
            let mut rows = Vec::with_capacity(idx_hits.len());
            for hit in idx_hits {
                if tbl.without_rowid {
                    let pk = index.extract_without_rowid_pk_from_hit(tbl, &hit);
                    let rkey = TableRowKey::WithoutRowidPk(pk);
                    if let Some(payload) = seek_table_raw_entry(ctx, tbl, &rkey)? {
                        rows.push((rkey, payload));
                    }
                } else if let Some(Value::Integer(rid)) = hit.last()
                    && let Some(payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, *rid)?
                {
                    rows.push((TableRowKey::Rowid(*rid), payload));
                }
            }
            Ok(rows)
        }
    }
}

fn compute_join_using_cols(
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    join: &JoinClause,
) -> Vec<String> {
    if join.natural {
        let mut shared = Vec::new();
        for lc in left_cols {
            if lc.hidden_rowid {
                continue;
            }
            if right_cols
                .iter()
                .any(|rc| !rc.hidden_rowid && rc.name.eq_ignore_ascii_case(&lc.name))
                && !shared
                    .iter()
                    .any(|s: &String| s.eq_ignore_ascii_case(&lc.name))
            {
                shared.push(lc.name.clone());
            }
        }
        shared
    } else if let Some(JoinConstraint::Using(cols)) = &join.constraint {
        cols.clone()
    } else {
        Vec::new()
    }
}

fn merge_join_scoped_columns(
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    join: &JoinClause,
) -> Vec<ScopedColumn> {
    let using_cols = compute_join_using_cols(left_cols, right_cols, join);
    let mut cols = Vec::with_capacity(left_cols.len() + right_cols.len());
    for lc in left_cols {
        let mut sc = lc.clone();
        if !sc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&sc.name)) {
            sc.merged_using = true;
        }
        cols.push(sc);
    }
    for rc in right_cols {
        if !rc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&rc.name)) {
            continue;
        }
        cols.push(rc.clone());
    }
    cols
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HashJoinKey {
    Null,
    Int(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl HashJoinKey {
    fn from_value_exact(val: &Value, comp_aff: TypeAffinity) -> Option<Self> {
        const MAX_EXACT_F64_INT: i64 = 9_007_199_254_740_992; // 2^53
        let coerced;
        let v = if comp_aff != TypeAffinity::Blob {
            coerced = comp_aff.apply(val.clone());
            &coerced
        } else {
            val
        };
        match v {
            Value::Null => Some(Self::Null),
            Value::Integer(i) => {
                if *i < -MAX_EXACT_F64_INT || *i > MAX_EXACT_F64_INT {
                    None
                } else {
                    Some(Self::Int(*i))
                }
            }
            Value::Real(r) => {
                if !r.is_finite()
                    || *r < -(MAX_EXACT_F64_INT as f64)
                    || *r > (MAX_EXACT_F64_INT as f64)
                {
                    None
                } else if r.fract() == 0.0 {
                    Some(Self::Int(*r as i64))
                } else if *r == 0.0 {
                    Some(Self::RealBits(0.0f64.to_bits()))
                } else {
                    Some(Self::RealBits(r.to_bits()))
                }
            }
            Value::Text(s) => Some(Self::Text(s.clone())),
            Value::Blob(b) => Some(Self::Blob(b.clone())),
        }
    }
}

struct EquiHashIndex {
    l_idx: usize,
    is_null_eq: bool,
    comp_aff: TypeAffinity,
    buckets: HashMap<HashJoinKey, Vec<usize>>,
}

fn try_build_equi_hash_index(
    conjuncts: &[&Expr],
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    merged_cols: &[ScopedColumn],
    left_rows: &[EvalRow],
    right_rows: &[EvalRow],
    eligible_right: &[usize],
) -> Option<EquiHashIndex> {
    let resolve_side = |e: &Expr| -> Option<(bool, usize)> {
        let Expr::Column(cref) = e else {
            return None;
        };
        // Must resolve unambiguously in `merged_cols` first!
        let m_idx = resolve_unique_col_idx_cref(merged_cols, cref)
            .ok()
            .flatten()?;
        let m_sc = &merged_cols[m_idx];
        if m_sc.merged_using || m_sc.collation != Collation::Binary {
            return None;
        }
        let in_l = resolve_unique_col_idx_cref(left_cols, cref).ok().flatten();
        let in_r = resolve_unique_col_idx_cref(right_cols, cref).ok().flatten();
        match (in_l, in_r) {
            (Some(li), None) if left_cols[li].collation == Collation::Binary => Some((true, li)),
            (None, Some(ri)) if right_cols[ri].collation == Collation::Binary => Some((false, ri)),
            _ => None,
        }
    };

    for &conj in conjuncts {
        let Expr::Binary { left, op, right } = conj else {
            continue;
        };
        let is_null_eq = match op {
            BinaryOp::Eq => false,
            BinaryOp::Is => true,
            _ => continue,
        };
        let Some((l_is_left, a_idx)) = resolve_side(left) else {
            continue;
        };
        let Some((r_is_left, b_idx)) = resolve_side(right) else {
            continue;
        };
        if l_is_left == r_is_left {
            continue;
        }
        let (l_idx, r_idx) = if l_is_left {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        let comp_aff =
            sqlite_compare_affinity(left_cols[l_idx].affinity, right_cols[r_idx].affinity);

        // Verify all values in `left_rows[..][l_idx]` fit exact HashJoinKey representation
        if left_rows
            .iter()
            .any(|lr| HashJoinKey::from_value_exact(&lr.vals[l_idx], comp_aff).is_none())
        {
            continue;
        }

        let mut buckets: HashMap<HashJoinKey, Vec<usize>> =
            HashMap::with_capacity(eligible_right.len());
        let mut exact_ok = true;
        for &ri in eligible_right {
            let rv = &right_rows[ri].vals[r_idx];
            let Some(key) = HashJoinKey::from_value_exact(rv, comp_aff) else {
                exact_ok = false;
                break;
            };
            if matches!(key, HashJoinKey::Null) && !is_null_eq {
                continue;
            }
            buckets.entry(key).or_default().push(ri);
        }
        if exact_ok {
            return Some(EquiHashIndex {
                l_idx,
                is_null_eq,
                comp_aff,
                buckets,
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn apply_join(
    ctx: &mut ExecContext<'_>,
    left_rows: Vec<EvalRow>,
    right_rows: Vec<EvalRow>,
    left_cols_fallback: &[ScopedColumn],
    right_cols_fallback: &[ScopedColumn],
    join: &JoinClause,
    extra_where_conjuncts: &[&Expr],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    let left_sample_cols: Vec<ScopedColumn> = left_rows
        .first()
        .map_or_else(|| left_cols_fallback.to_vec(), |r| r.cols.to_vec());
    let right_sample_cols: Vec<ScopedColumn> = right_rows
        .first()
        .map_or_else(|| right_cols_fallback.to_vec(), |r| r.cols.to_vec());

    let using_cols = compute_join_using_cols(&left_sample_cols, &right_sample_cols, join);
    let merged_cols = merge_join_scoped_columns(&left_sample_cols, &right_sample_cols, join);
    let shared_merged_cols: Rc<[ScopedColumn]> = Rc::from(merged_cols.as_slice());
    let right_keep_indices: Vec<usize> = right_sample_cols
        .iter()
        .enumerate()
        .filter_map(|(j, rc)| {
            if !rc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&rc.name)) {
                None
            } else {
                Some(j)
            }
        })
        .collect();
    let left_is_using: Vec<bool> = left_sample_cols
        .iter()
        .map(|lc| !lc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&lc.name)))
        .collect();

    let combine_rows = |l: Option<&EvalRow>, r: Option<&EvalRow>| -> EvalRow {
        let mut vals = Vec::with_capacity(merged_cols.len());
        for (i, lc) in left_sample_cols.iter().enumerate() {
            let mut val = l.map_or(Value::Null, |row| row.vals[i].clone());
            if left_is_using[i]
                && val.is_null()
                && let Some(r_row) = r
                && let Some((rv, _)) = r_row.lookup(None, &lc.name).ok().flatten()
            {
                val = rv;
            }
            vals.push(val);
        }
        for &j in &right_keep_indices {
            vals.push(r.map_or(Value::Null, |row| row.vals[j].clone()));
        }
        EvalRow {
            cols: Rc::clone(&shared_merged_cols),
            vals,
        }
    };

    let mut all_conjuncts: Vec<&Expr> = Vec::new();
    if using_cols.is_empty() {
        if let Some(JoinConstraint::On(on_expr)) = &join.constraint {
            split_where_conjuncts(on_expr, &mut all_conjuncts);
        }
        all_conjuncts.extend_from_slice(extra_where_conjuncts);
    }

    let mut left_only_conjuncts: Vec<&Expr> = Vec::new();
    let mut right_only_conjuncts: Vec<&Expr> = Vec::new();
    if using_cols.is_empty() && !left_rows.is_empty() && !right_rows.is_empty() {
        for &c in &all_conjuncts {
            if expr_can_eval_with_cols(c, &left_sample_cols, &merged_cols, outer_scopes) {
                left_only_conjuncts.push(c);
            } else if expr_can_eval_with_cols(c, &right_sample_cols, &merged_cols, outer_scopes) {
                right_only_conjuncts.push(c);
            }
        }
    }

    let mut eligible_right: Vec<usize> = Vec::with_capacity(right_rows.len());
    if !left_rows.is_empty() {
        for (ri, r) in right_rows.iter().enumerate() {
            let mut ok = true;
            if !right_only_conjuncts.is_empty() {
                let r_scopes = push_scope(outer_scopes, r);
                for &rc in &right_only_conjuncts {
                    if eval_expr(ctx, rc, params, &r_scopes, ctes, None)?.truthiness() != Some(true)
                    {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                eligible_right.push(ri);
            }
        }
    }

    let hash_index = if using_cols.is_empty() && !left_rows.is_empty() && !eligible_right.is_empty()
    {
        try_build_equi_hash_index(
            &all_conjuncts,
            &left_sample_cols,
            &right_sample_cols,
            &merged_cols,
            &left_rows,
            &right_rows,
            &eligible_right,
        )
    } else {
        None
    };

    let mut out = Vec::new();
    let mut right_matched = vec![false; right_rows.len()];

    for l in &left_rows {
        ctx.step_tick()?;
        let mut any_match = false;
        let mut left_ok = true;
        if !right_rows.is_empty() && !left_only_conjuncts.is_empty() {
            let l_scopes = push_scope(outer_scopes, l);
            for &lc in &left_only_conjuncts {
                if eval_expr(ctx, lc, params, &l_scopes, ctes, None)?.truthiness() != Some(true) {
                    left_ok = false;
                    break;
                }
            }
        }

        if left_ok && !eligible_right.is_empty() {
            let candidates: &[usize] = if let Some(ref hi) = hash_index {
                let key = HashJoinKey::from_value_exact(&l.vals[hi.l_idx], hi.comp_aff)
                    .expect("verified exact key");
                if matches!(key, HashJoinKey::Null) && !hi.is_null_eq {
                    &[]
                } else {
                    hi.buckets.get(&key).map_or(&[][..], Vec::as_slice)
                }
            } else {
                &eligible_right
            };

            for &ri in candidates {
                ctx.step_tick()?;
                let r = &right_rows[ri];
                let mut candidate_opt: Option<EvalRow> = None;
                let matches = if !using_cols.is_empty() {
                    let mut ok = true;
                    for u in &using_cols {
                        let (lv, lcoll, laff) = l.lookup_full(None, u)?.unwrap_or((
                            Value::Null,
                            Collation::Binary,
                            None,
                        ));
                        let (rv, rcoll, raff) = r.lookup_full(None, u)?.unwrap_or((
                            Value::Null,
                            Collation::Binary,
                            None,
                        ));
                        let coll = if lcoll != Collation::Binary {
                            lcoll
                        } else {
                            rcoll
                        };
                        let comp_aff = sqlite_compare_affinity(laff, raff);
                        if eval_comparison_with_affinity(
                            Opcode::Eq,
                            &lv,
                            &rv,
                            comp_aff,
                            coll,
                            ctx.btree.encoding(),
                        )
                        .truthiness()
                            != Some(true)
                        {
                            ok = false;
                            break;
                        }
                    }
                    ok
                } else if join.constraint.is_some() || !extra_where_conjuncts.is_empty() {
                    let cand = combine_rows(Some(l), Some(r));
                    let mut ok = true;
                    if outer_scopes.is_empty() {
                        let single_scope = std::slice::from_ref(&cand);
                        if let Some(JoinConstraint::On(on_expr)) = &join.constraint
                            && eval_expr(ctx, on_expr, params, single_scope, ctes, None)?
                                .truthiness()
                                != Some(true)
                        {
                            ok = false;
                        }
                        if ok {
                            for &wc in extra_where_conjuncts {
                                if eval_expr(ctx, wc, params, single_scope, ctes, None)?
                                    .truthiness()
                                    != Some(true)
                                {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    } else {
                        let scopes = push_scope(outer_scopes, &cand);
                        if let Some(JoinConstraint::On(on_expr)) = &join.constraint
                            && eval_expr(ctx, on_expr, params, &scopes, ctes, None)?.truthiness()
                                != Some(true)
                        {
                            ok = false;
                        }
                        if ok {
                            for &wc in extra_where_conjuncts {
                                if eval_expr(ctx, wc, params, &scopes, ctes, None)?.truthiness()
                                    != Some(true)
                                {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    }
                    if ok {
                        candidate_opt = Some(cand);
                    }
                    ok
                } else {
                    true
                };

                if matches {
                    any_match = true;
                    right_matched[ri] = true;
                    out.push(candidate_opt.unwrap_or_else(|| combine_rows(Some(l), Some(r))));
                }
            }
        }

        if !any_match && matches!(join.join_type, JoinType::Left | JoinType::Full) {
            out.push(combine_rows(Some(l), None));
        }
    }

    if matches!(join.join_type, JoinType::Right | JoinType::Full) {
        for (ri, r) in right_rows.iter().enumerate() {
            if !right_matched[ri] {
                out.push(combine_rows(None, Some(r)));
            }
        }
    }

    Ok(out)
}

fn schema_cols_preview(
    ctx: &mut ExecContext<'_>,
    fc: &FromClause,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    let mut cols = schema_cols_for_tos(ctx, &fc.head, params, outer_scopes, ctes)?;
    for j in &fc.joins {
        if let Some(rcols) = schema_cols_for_tos(ctx, &j.right, params, outer_scopes, ctes) {
            cols = merge_join_scoped_columns(&cols, &rcols, j);
        }
    }
    Some(cols)
}

fn schema_cols_for_tos(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Ok(Some(cte)) = resolve_cte(ctx, ctes, &name.name, params, outer_scopes)
            {
                let cte_meta = infer_cte_meta_by_name(&name.name, outer_scopes, ctx.schema, ctes);
                return Some(
                    cte.column_names
                        .iter()
                        .enumerate()
                        .map(|(i, cn)| {
                            let (aff, coll) = cte_meta
                                .get(i)
                                .copied()
                                .unwrap_or((None, Collation::Binary));
                            ScopedColumn {
                                schema_name: None,
                                table_alias: Some(eff.clone()),
                                name: cn.clone(),
                                collation: coll,
                                affinity: aff,
                                hidden_rowid: false,
                                merged_using: false,
                            }
                        })
                        .collect(),
                );
            }
            let (tbl, schema_lbl) = resolve_table_for_query(ctx.schema, name).ok()?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff),
                    name: "rowid".to_owned(),
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    merged_using: false,
                });
            }
            Some(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, ctx.schema, ctes);
            let sub_out = exec_select(ctx, query, params, outer_scopes, ctes).ok()?;
            Some(
                sub_out
                    .column_names
                    .iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = sub_meta
                            .get(i)
                            .copied()
                            .unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: alias.clone(),
                            name: cn.clone(),
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            merged_using: false,
                        }
                    })
                    .collect(),
            )
        }
    }
}

#[allow(clippy::type_complexity)]
fn expand_returning_projection(
    projection: &[ResultColumn],
    sample_cols: &[ScopedColumn],
) -> Result<(Vec<String>, Vec<(Expr, String, Option<String>)>)> {
    if projection
        .iter()
        .any(|rc| matches!(rc, ResultColumn::TableStar { .. }))
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "RETURNING may not use \"TABLE.*\" wildcards",
        ));
    }
    expand_projection(projection, sample_cols)
}

#[allow(clippy::type_complexity)]
fn expand_projection(
    projection: &[ResultColumn],
    sample_cols: &[ScopedColumn],
) -> Result<(Vec<String>, Vec<(Expr, String, Option<String>)>)> {
    let mut names = Vec::new();
    let mut expanded = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for sc in sample_cols {
                    if sc.hidden_rowid {
                        continue;
                    }
                    let e = Expr::Column(ColumnRef {
                        schema: if sc.merged_using {
                            None
                        } else {
                            sc.schema_name.clone()
                        },
                        table: if sc.merged_using {
                            None
                        } else {
                            sc.table_alias.clone()
                        },
                        column: sc.name.clone(),
                    });
                    names.push(sc.name.clone());
                    expanded.push((e, sc.name.clone(), None));
                }
            }
            ResultColumn::TableStar {
                schema: tschema,
                table: tname,
            } => {
                let mut found = false;
                for sc in sample_cols {
                    if sc.hidden_rowid {
                        continue;
                    }
                    if sc.matches_qualifier(tschema.as_deref(), Some(tname)) {
                        found = true;
                        let e = Expr::Column(ColumnRef {
                            schema: tschema.clone(),
                            table: Some(tname.clone()),
                            column: sc.name.clone(),
                        });
                        names.push(sc.name.clone());
                        expanded.push((e, sc.name.clone(), None));
                    }
                }
                if !found {
                    let disp = match tschema {
                        Some(s) => format!("{s}.{tname}"),
                        None => tname.clone(),
                    };
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {disp}"),
                    ));
                }
            }
            ResultColumn::Expr { expr, alias } => {
                let label = alias.clone().unwrap_or_else(|| expr_column_label(expr));
                names.push(label.clone());
                expanded.push((expr.clone(), label, alias.clone()));
            }
        }
    }
    Ok((names, expanded))
}

fn build_groups(
    ctx: &mut ExecContext<'_>,
    rows: Vec<EvalRow>,
    group_by: &[Expr],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<EvalRow>>> {
    if group_by.is_empty() {
        // Ungrouped aggregate (`SELECT COUNT(*) FROM t`): always yields 1 group even if `rows` is empty!
        return Ok(vec![rows]);
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut groups: Vec<(Vec<Value>, Vec<Collation>, Vec<EvalRow>)> = Vec::new();
    for row in rows {
        ctx.step_tick()?;
        let scopes = push_scope(outer_scopes, &row);
        let mut g_vals = Vec::with_capacity(group_by.len());
        let mut g_colls = Vec::with_capacity(group_by.len());
        for ge in group_by {
            let v = eval_expr(ctx, ge, params, &scopes, ctes, None)?;
            let coll = match ge {
                Expr::Collate { collation, .. } => {
                    Collation::from_name(collation).unwrap_or_default()
                }
                Expr::Column(cref) => row.lookup_cref(cref)?.map_or(Collation::Binary, |(_, c)| c),
                _ => Collation::Binary,
            };
            g_vals.push(v);
            g_colls.push(coll);
        }

        let enc = ctx.btree.encoding();
        let existing = groups.iter_mut().find(|(kv, colls, _)| {
            kv.iter()
                .zip(g_vals.iter())
                .zip(colls.iter())
                .all(|((a, b), &c)| match (a.is_null(), b.is_null()) {
                    (true, true) => true,
                    (true, false) | (false, true) => false,
                    (false, false) => a.cmp_sqlite_with_encoding(b, c, enc) == Ordering::Equal,
                })
        });
        if let Some((_, _, bucket)) = existing {
            bucket.push(row);
        } else {
            groups.push((g_vals, g_colls, vec![row]));
        }
    }

    // Sort groups in SQLite's natural GROUP BY B-tree order!
    let enc = ctx.btree.encoding();
    groups.sort_by(|(ka, ca, _), (kb, _, _)| {
        let specs: Vec<KeySortOrder> = ca
            .iter()
            .map(|&c| KeySortOrder {
                desc: false,
                nulls_last: false,
                collation: c,
            })
            .collect();
        compare_value_slices(ka, kb, &specs, enc)
    });

    Ok(groups.into_iter().map(|(_, _, b)| b).collect())
}

fn infer_expr_collation_from_cols(expr: &Expr, sample_cols: &[ScopedColumn]) -> Collation {
    match expr {
        Expr::Collate { collation, .. } => Collation::from_name(collation).unwrap_or_default(),
        Expr::Column(cref) => {
            for sc in sample_cols {
                if sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                    continue;
                }
                if sc.name.eq_ignore_ascii_case(&cref.column) {
                    return sc.collation;
                }
            }
            Collation::Binary
        }
        _ => Collation::Binary,
    }
}

fn infer_order_by_collation(
    ob_expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
) -> Collation {
    if let Expr::Collate { collation, .. } = ob_expr {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Integer(ord) = ob_expr
        && *ord >= 1
        && (*ord as usize) <= expanded_proj.len()
    {
        return infer_expr_collation_from_cols(&expanded_proj[(*ord as usize) - 1].0, sample_cols);
    }
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = ob_expr
        && let Some((idx, _)) = expanded_proj
            .iter()
            .enumerate()
            .find(|(_, (_, label, al))| {
                al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                    || label.eq_ignore_ascii_case(column)
            })
    {
        let in_row = sample_cols
            .iter()
            .any(|sc| !sc.hidden_rowid && sc.name.eq_ignore_ascii_case(column));
        if expanded_proj[idx].2.is_some() || !in_row {
            return infer_expr_collation_from_cols(&expanded_proj[idx].0, sample_cols);
        }
    }
    infer_expr_collation_from_cols(ob_expr, sample_cols)
}

#[allow(clippy::too_many_arguments)]
fn eval_order_term_in_scope(
    ctx: &mut ExecContext<'_>,
    ob_expr: &Expr,
    proj_vals: &[Value],
    expanded_proj: &[(Expr, String, Option<String>)],
    scopes: &[EvalRow],
    params: &[Value],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    let mut inner = ob_expr;
    while let Expr::Collate { expr, .. } = inner {
        inner = expr;
    }
    if let Expr::Integer(ord) = inner
        && *ord >= 1
        && (*ord as usize) <= proj_vals.len()
    {
        return Ok(proj_vals[(*ord as usize) - 1].clone());
    }
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = inner
        && let Some((idx, _)) = expanded_proj
            .iter()
            .enumerate()
            .find(|(_, (_, label, al))| {
                al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                    || label.eq_ignore_ascii_case(column)
            })
    {
        // Prefer alias match if column is not directly in scope or is an explicit alias
        let in_row = scopes
            .last()
            .and_then(|r| r.lookup(None, column).ok().flatten())
            .is_some();
        if expanded_proj[idx].2.is_some() || !in_row {
            return Ok(proj_vals[idx].clone());
        }
    }
    if let Some((idx, _)) = expanded_proj
        .iter()
        .enumerate()
        .find(|(_, (pe, _, _))| pe == inner)
    {
        return Ok(proj_vals[idx].clone());
    }
    let substituted = subst_order_by_aliases_in_expr(ob_expr, proj_vals, expanded_proj, scopes);
    eval_expr(ctx, &substituted, params, scopes, ctes, agg_group)
}

fn value_to_literal_expr(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::Null,
        Value::Integer(i) => Expr::Integer(*i),
        Value::Real(f) => Expr::Float(*f),
        Value::Text(s) => Expr::String(s.clone()),
        Value::Blob(b) => Expr::Blob(b.clone()),
    }
}

fn subst_order_by_aliases_in_expr(
    expr: &Expr,
    proj_vals: &[Value],
    expanded_proj: &[(Expr, String, Option<String>)],
    scopes: &[EvalRow],
) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            let in_row = scopes
                .last()
                .and_then(|r| r.lookup(None, column).ok().flatten())
                .is_some();
            if !in_row
                && let Some((idx, _)) =
                    expanded_proj
                        .iter()
                        .enumerate()
                        .find(|(_, (_, label, al))| {
                            al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                                || label.eq_ignore_ascii_case(column)
                        })
            {
                return value_to_literal_expr(&proj_vals[idx]);
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            collation: collation.clone(),
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            type_name: type_name.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            negated: *negated,
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_order_by_aliases_in_expr(
                left,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            op: *op,
            right: Box::new(subst_order_by_aliases_in_expr(
                right,
                proj_vals,
                expanded_proj,
                scopes,
            )),
        },
        Expr::Between {
            expr: val,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            low: Box::new(subst_order_by_aliases_in_expr(
                low,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            high: Box::new(subst_order_by_aliases_in_expr(
                high,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: val,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            list: list
                .iter()
                .map(|e| subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes))
                .collect(),
            negated: *negated,
        },
        Expr::Like {
            expr: val,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            pattern: Box::new(subst_order_by_aliases_in_expr(
                pattern,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            escape: escape.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_order_by_aliases_in_expr(w, proj_vals, expanded_proj, scopes),
                        subst_order_by_aliases_in_expr(t, proj_vals, expanded_proj, scopes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| {
                            subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes)
                        })
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

fn sort_materialized_output(
    rows: &mut [Vec<Value>],
    all_arm_col_names: &[Vec<String>],
    col_collations: &[Collation],
    order_by: &[OrderByTerm],
    enc: TextEncoding,
) -> Result<()> {
    let width = all_arm_col_names.first().map_or(0, Vec::len);
    let mut specs = Vec::with_capacity(order_by.len());
    for (ob_idx, ob) in order_by.iter().enumerate() {
        let mut inner = &ob.expr;
        while let Expr::Collate { expr, .. } = inner {
            inner = expr.as_ref();
        }
        if let Some(ord) = strip_collate_and_unary_plus(inner) {
            if ord < 1 || (ord as usize) > width {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term out of range - should be between 1 and {}",
                        ordinal_suffix(ob_idx + 1),
                        width
                    ),
                ));
            }
            let cidx = (ord as usize) - 1;
            let coll = match &ob.expr {
                Expr::Collate { collation, .. } => {
                    Collation::from_name(collation).unwrap_or_default()
                }
                _ => col_collations
                    .get(cidx)
                    .copied()
                    .unwrap_or(Collation::Binary),
            };
            specs.push((
                cidx,
                KeySortOrder {
                    desc: ob.desc,
                    nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                    collation: coll,
                },
            ));
            continue;
        }
        let target_label = match inner {
            Expr::Column(ColumnRef {
                schema: None,
                table: None,
                column,
            }) => Some(column.clone()),
            other => Some(expr_column_label(other)),
        };
        let col_idx = target_label
            .and_then(|lbl| {
                all_arm_col_names.iter().find_map(|arm_cols| {
                    arm_cols.iter().position(|cn| cn.eq_ignore_ascii_case(&lbl))
                })
            })
            .ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term does not match any column in the result set",
                        ordinal_suffix(ob_idx + 1)
                    ),
                )
            })?;
        let coll = match &ob.expr {
            Expr::Collate { collation, .. } => Collation::from_name(collation).unwrap_or_default(),
            _ => col_collations
                .get(col_idx)
                .copied()
                .unwrap_or(Collation::Binary),
        };
        specs.push((
            col_idx,
            KeySortOrder {
                desc: ob.desc,
                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                collation: coll,
            },
        ));
    }

    rows.sort_by(|a, b| {
        for &(idx, spec) in &specs {
            let ord = compare_single_value(&a[idx], &b[idx], spec, enc);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    Ok(())
}

fn apply_limit_offset(
    ctx: &mut ExecContext<'_>,
    rows: Vec<Vec<Value>>,
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<Value>>> {
    let Some(lim) = limit else {
        return Ok(rows);
    };
    let lim_val = eval_expr(ctx, &lim.limit, params, outer_scopes, ctes, None)?.to_integer_lossy();
    let off_val = match &lim.offset {
        Some(oe) => eval_expr(ctx, oe, params, outer_scopes, ctes, None)?
            .to_integer_lossy()
            .max(0) as usize,
        None => 0,
    };

    let iter = rows.into_iter().skip(off_val);
    if lim_val < 0 {
        Ok(iter.collect())
    } else {
        Ok(iter.take(lim_val as usize).collect())
    }
}

fn expr_contains_conjunct(haystack: &Expr, needle: &Expr) -> bool {
    if haystack == needle {
        return true;
    }
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = haystack
    {
        return expr_contains_conjunct(left, needle) || expr_contains_conjunct(right, needle);
    }
    false
}

fn subst_select_aliases_in_expr(
    expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            let in_sample = sample_cols.iter().any(|sc| {
                if sc.hidden_rowid {
                    column.eq_ignore_ascii_case("rowid")
                        || column.eq_ignore_ascii_case("_rowid_")
                        || column.eq_ignore_ascii_case("oid")
                } else {
                    sc.name.eq_ignore_ascii_case(column)
                }
            });
            let in_outer = outer_scopes
                .iter()
                .rev()
                .any(|s| s.lookup(None, column).ok().flatten().is_some());
            if !in_sample
                && !in_outer
                && let Some((pe, _, _)) = expanded_proj
                    .iter()
                    .find(|(_, _, al)| al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column)))
            {
                return pe.clone();
            }
            expr.clone()
        }
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_select_aliases_in_expr(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            collation: collation.clone(),
        },
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_select_aliases_in_expr(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_select_aliases_in_expr(
                left,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            op: *op,
            right: Box::new(subst_select_aliases_in_expr(
                right,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_select_aliases_in_expr(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::Between {
            expr: ve,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_select_aliases_in_expr(
                ve,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            low: Box::new(subst_select_aliases_in_expr(
                low,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            high: Box::new(subst_select_aliases_in_expr(
                high,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: le,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_select_aliases_in_expr(
                le,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            list: list
                .iter()
                .map(|e| subst_select_aliases_in_expr(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
            negated: *negated,
        },
        Expr::InSubquery {
            expr: le,
            subquery,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(subst_select_aliases_in_expr(
                le,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            subquery: subquery.clone(),
            negated: *negated,
        },
        Expr::InTable {
            expr: le,
            table,
            negated,
        } => Expr::InTable {
            expr: Box::new(subst_select_aliases_in_expr(
                le,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            table: table.clone(),
            negated: *negated,
        },
        Expr::Like {
            expr: te,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_select_aliases_in_expr(
                te,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            pattern: Box::new(subst_select_aliases_in_expr(
                pattern,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            escape: escape.as_ref().map(|ee| {
                Box::new(subst_select_aliases_in_expr(
                    ee,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_select_aliases_in_expr(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            type_name: type_name.clone(),
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|oe| {
                Box::new(subst_select_aliases_in_expr(
                    oe,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_select_aliases_in_expr(w, expanded_proj, sample_cols, outer_scopes),
                        subst_select_aliases_in_expr(t, expanded_proj, sample_cols, outer_scopes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|ee| {
                Box::new(subst_select_aliases_in_expr(
                    ee,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| {
                            subst_select_aliases_in_expr(
                                e,
                                expanded_proj,
                                sample_cols,
                                outer_scopes,
                            )
                        })
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|fe| {
                Box::new(subst_select_aliases_in_expr(
                    fe,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_select_aliases_in_expr(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

fn infer_cte_meta_by_name(
    name: &str,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    infer_cte_meta_guarded(name, outer_scopes, schema, ctes, &mut visited)
}

fn infer_cte_meta_guarded(
    name: &str,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let Some((s_idx, c_idx)) = ctes.locate(name) else {
        return Vec::new();
    };
    if visited.contains(&(s_idx, c_idx)) {
        return Vec::new();
    }
    visited.push((s_idx, c_idx));
    let cte = &ctes.scopes[s_idx][c_idx];
    let mut sub_ctes = CteStore {
        scopes: ctes.scopes[..=s_idx].to_vec(),
        working: Vec::new(),
        resolving: Vec::new(),
        limit_hint: None,
    };
    if !cte.query.ctes.is_empty() {
        sub_ctes.scopes.push(cte.query.ctes.clone());
    }
    let res =
        infer_select_body_meta_guarded(&cte.query.body, outer_scopes, schema, &sub_ctes, visited);
    visited.pop();
    res
}

fn infer_select_stmt_meta(
    sel: &SelectStmt,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    let sub_ctes = ctes.with_scope(&sel.ctes);
    infer_select_body_meta_guarded(&sel.body, outer_scopes, schema, &sub_ctes, &mut visited)
}

fn infer_select_body_meta(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    infer_select_body_meta_guarded(body, outer_scopes, schema, ctes, &mut visited)
}

fn infer_select_body_meta_guarded(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let head_meta = infer_select_core_meta(&body.head, outer_scopes, schema, ctes, visited);
    let mut combined: Vec<(Option<TypeAffinity>, Option<Collation>)> = head_meta;
    for (_, rhs_core) in &body.compounds {
        let rhs_meta = infer_select_core_meta(rhs_core, outer_scopes, schema, ctes, visited);
        for (i, (r_aff, r_coll)) in rhs_meta.into_iter().enumerate() {
            if let Some((c_aff, c_coll)) = combined.get_mut(i) {
                if c_coll.is_none() && r_coll.is_some() {
                    *c_coll = r_coll;
                }
                if *c_aff != r_aff {
                    // Per SQLite `select.c` (`multiSelect`): if compound arms disagree on affinity, check numeric compatibility or fall back to BLOB/None
                    *c_aff = match (*c_aff, r_aff) {
                        (Some(a), Some(b)) if a == b => Some(a),
                        (Some(a), None) | (None, Some(a)) => Some(a),
                        _ => Some(TypeAffinity::Blob),
                    };
                }
            }
        }
    }
    combined
        .into_iter()
        .map(|(a, c)| (a, c.unwrap_or(Collation::Binary)))
        .collect()
}

fn infer_select_core_meta(
    core: &SelectCore,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Option<Collation>)> {
    match core {
        SelectCore::Values(rows) => {
            let Some(first) = rows.first() else {
                return Vec::new();
            };
            first
                .iter()
                .map(|e| {
                    let aff = infer_expr_affinity_in_scopes_guarded(
                        e,
                        outer_scopes,
                        schema,
                        ctes,
                        visited,
                    );
                    let coll = infer_expr_explicit_or_col_collation(e, &[], outer_scopes);
                    (aff, coll)
                })
                .collect()
        }
        SelectCore::Select {
            projection, from, ..
        } => {
            let sample_cols = from
                .as_ref()
                .and_then(|fc| static_schema_cols_for_from(fc, outer_scopes, schema, ctes, visited))
                .unwrap_or_default();
            let Ok((_, expanded)) = expand_projection(projection, &sample_cols) else {
                return Vec::new();
            };
            let dummy_row = EvalRow {
                cols: Rc::from(sample_cols.as_slice()),
                vals: vec![Value::Null; sample_cols.len()],
            };
            let full_scopes = push_scope(outer_scopes, &dummy_row);
            expanded
                .into_iter()
                .map(|(pe, _, _)| {
                    let aff = infer_expr_affinity_in_scopes_guarded(
                        &pe,
                        &full_scopes,
                        schema,
                        ctes,
                        visited,
                    );
                    let coll =
                        infer_expr_explicit_or_col_collation(&pe, &sample_cols, outer_scopes);
                    (aff, coll)
                })
                .collect()
        }
    }
}

fn static_schema_cols_for_from(
    fc: &FromClause,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<Vec<ScopedColumn>> {
    let mut cols = static_schema_cols_for_tos(&fc.head, outer_scopes, schema, ctes, visited)?;
    for j in &fc.joins {
        if let Some(rcols) =
            static_schema_cols_for_tos(&j.right, outer_scopes, schema, ctes, visited)
        {
            cols = merge_join_scoped_columns(&cols, &rcols, j);
        }
    }
    Some(cols)
}

fn static_schema_cols_for_tos(
    tos: &TableOrSubquery,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                if visited.contains(&(s_idx, c_idx)) {
                    return None;
                }
                let cte = &ctes.scopes[s_idx][c_idx];
                let meta = infer_cte_meta_guarded(&name.name, outer_scopes, schema, ctes, visited);
                let cnames: Vec<String> = if let Some(ref explicit) = cte.columns {
                    explicit.clone()
                } else {
                    visited.push((s_idx, c_idx));
                    let mut sub_ctes = CteStore {
                        scopes: ctes.scopes[..=s_idx].to_vec(),
                        working: Vec::new(),
                        resolving: Vec::new(),
                        limit_hint: None,
                    };
                    if !cte.query.ctes.is_empty() {
                        sub_ctes.scopes.push(cte.query.ctes.clone());
                    }
                    let names = static_select_col_names(
                        &cte.query.body,
                        outer_scopes,
                        schema,
                        &sub_ctes,
                        visited,
                    );
                    visited.pop();
                    names
                };
                return Some(
                    cnames
                        .into_iter()
                        .enumerate()
                        .map(|(i, cn)| {
                            let (aff, coll) =
                                meta.get(i).copied().unwrap_or((None, Collation::Binary));
                            ScopedColumn {
                                schema_name: None,
                                table_alias: Some(eff.clone()),
                                name: cn,
                                collation: coll,
                                affinity: aff,
                                hidden_rowid: false,
                                merged_using: false,
                            }
                        })
                        .collect(),
                );
            }
            let (tbl, schema_lbl) = resolve_table_for_query(schema, name).ok()?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff),
                    name: "rowid".to_owned(),
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    merged_using: false,
                });
            }
            Some(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_ctes = ctes.with_scope(&query.ctes);
            let meta = infer_select_body_meta_guarded(
                &query.body,
                outer_scopes,
                schema,
                &sub_ctes,
                visited,
            );
            let cnames =
                static_select_col_names(&query.body, outer_scopes, schema, &sub_ctes, visited);
            Some(
                cnames
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: alias.clone(),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            merged_using: false,
                        }
                    })
                    .collect(),
            )
        }
    }
}

fn static_select_col_names(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<String> {
    match &body.head {
        SelectCore::Values(rows) => {
            let w = rows.first().map_or(0, Vec::len);
            (1..=w).map(|i| format!("column{i}")).collect()
        }
        SelectCore::Select {
            projection, from, ..
        } => {
            let sample_cols = from
                .as_ref()
                .and_then(|fc| static_schema_cols_for_from(fc, outer_scopes, schema, ctes, visited))
                .unwrap_or_default();
            expand_projection(projection, &sample_cols)
                .map(|(names, _)| names)
                .unwrap_or_default()
        }
    }
}

fn infer_expr_explicit_or_col_collation(
    expr: &Expr,
    sample_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> Option<Collation> {
    match expr {
        Expr::Collate { collation, .. } => {
            Some(Collation::from_name(collation).unwrap_or_default())
        }
        Expr::RowValue(elems) if elems.len() == 1 => {
            infer_expr_explicit_or_col_collation(&elems[0], sample_cols, outer_scopes)
        }
        Expr::Column(cref) => {
            for sc in sample_cols {
                if sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                    continue;
                }
                if sc.name.eq_ignore_ascii_case(&cref.column) {
                    return Some(sc.collation);
                }
            }
            for s in outer_scopes.iter().rev() {
                if let Ok(Some((_, c))) = s.lookup_cref(cref) {
                    return Some(c);
                }
            }
            None
        }
        _ => None,
    }
}

fn infer_expr_affinity_in_scopes(
    expr: &Expr,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> Option<TypeAffinity> {
    let mut visited = Vec::new();
    infer_expr_affinity_in_scopes_guarded(expr, scopes, schema, ctes, &mut visited)
}

fn infer_expr_affinity_in_scopes_guarded(
    expr: &Expr,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<TypeAffinity> {
    match expr {
        Expr::Collate { expr: inner, .. } => {
            infer_expr_affinity_in_scopes_guarded(inner, scopes, schema, ctes, visited)
        }
        Expr::RowValue(elems) if elems.len() == 1 => {
            infer_expr_affinity_in_scopes_guarded(&elems[0], scopes, schema, ctes, visited)
        }
        Expr::Cast { type_name, .. } => Some(TypeAffinity::from_declared_type(type_name)),
        Expr::Column(cref) => {
            for s in scopes.iter().rev() {
                if let Ok(Some((_, _, aff))) = s.lookup_cref_full(cref) {
                    return aff;
                }
            }
            None
        }
        Expr::Subquery(sub) => {
            let sub_ctes = ctes.with_scope(&sub.ctes);
            infer_select_body_meta_guarded(&sub.body, scopes, schema, &sub_ctes, visited)
                .first()
                .and_then(|(aff, _)| *aff)
        }
        _ => None,
    }
}

fn compare_value_slices(
    a: &[Value],
    b: &[Value],
    specs: &[KeySortOrder],
    enc: TextEncoding,
) -> Ordering {
    for (i, spec) in specs.iter().enumerate() {
        let ord = compare_single_value(&a[i], &b[i], *spec, enc);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn compare_single_value(va: &Value, vb: &Value, spec: KeySortOrder, enc: TextEncoding) -> Ordering {
    match (va.is_null(), vb.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if spec.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => {
            if spec.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, false) => {
            let raw = va.cmp_sqlite_with_encoding(vb, spec.collation, enc);
            if spec.desc { raw.reverse() } else { raw }
        }
    }
}

fn push_scope(outer: &[EvalRow], inner: &EvalRow) -> Vec<EvalRow> {
    let mut v = Vec::with_capacity(outer.len() + 1);
    v.extend_from_slice(outer);
    v.push(inner.clone());
    v
}

// ============================================================================
// Expression Evaluator (`eval_expr_rt` / `eval_expr`)
// ============================================================================

fn eval_expr(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    Ok(eval_expr_rt(ctx, expr, params, scopes, ctes, agg_group)?.value)
}

fn eval_expr_rt(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<RuntimeValue> {
    match expr {
        Expr::Null => Ok(RuntimeValue::null()),
        Expr::Bool(b) => Ok(RuntimeValue::plain(Value::Integer(i64::from(*b)))),
        Expr::Integer(i) => Ok(RuntimeValue::plain(Value::Integer(*i))),
        Expr::Float(f) => Ok(RuntimeValue::plain(Value::real(*f))),
        Expr::String(s) => Ok(RuntimeValue::plain(Value::Text(s.clone()))),
        Expr::Blob(b) => Ok(RuntimeValue::plain(Value::Blob(b.clone()))),
        Expr::CurrentTimeKw(kind) => {
            let ts_ms = ctx.runtime.effective_timestamp_ms();
            Ok(RuntimeValue::plain(Value::Text(match kind {
                CurrentTimeKind::Date => format_utc_date(ts_ms),
                CurrentTimeKind::Time => format_utc_time(ts_ms),
                CurrentTimeKind::Timestamp => format_utc_timestamp(ts_ms),
            })))
        }
        Expr::Bind(bp) => {
            let idx = match bp {
                BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
            };
            Ok(RuntimeValue::plain(
                params
                    .get(idx.saturating_sub(1))
                    .cloned()
                    .unwrap_or(Value::Null),
            ))
        }
        Expr::Column(cref) => {
            for scope in scopes.iter().rev() {
                if let Some((val, _)) = scope.lookup_cref(cref)? {
                    return Ok(RuntimeValue::plain(val));
                }
            }
            // In SQLite, double-quoted string fallback is deprecated; return clear error
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {}", cref.display_name()),
            ))
        }
        Expr::Collate { expr: inner, .. } => {
            eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group)
        }
        Expr::Unary { op, expr: inner } => {
            if *op == UnaryOp::Plus {
                return eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group);
            }
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(RuntimeValue::plain(match op {
                UnaryOp::Plus => unreachable!(),
                UnaryOp::Negate => eval_unary_negate(&v),
                UnaryOp::BitNot => {
                    if v.is_null() {
                        Value::Null
                    } else {
                        Value::Integer(!v.to_integer_lossy())
                    }
                }
                UnaryOp::Not => match v.truthiness() {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(!b)),
                },
            }))
        }
        Expr::RowValue(elems) => {
            if elems.len() == 1 {
                eval_expr_rt(ctx, &elems[0], params, scopes, ctes, agg_group)
            } else {
                Err(SqliteError::new(ErrorCode::Error, "row value misused"))
            }
        }
        Expr::Binary { left, op, right } => {
            // Short-circuit AND / OR
            if *op == BinaryOp::And {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(false) {
                    return Ok(RuntimeValue::plain(Value::Integer(0)));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(RuntimeValue::plain(
                    match (lv.truthiness(), rv.truthiness()) {
                        (_, Some(false)) => Value::Integer(0),
                        (Some(true), Some(true)) => Value::Integer(1),
                        _ => Value::Null,
                    },
                ));
            }
            if *op == BinaryOp::Or {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(true) {
                    return Ok(RuntimeValue::plain(Value::Integer(1)));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(RuntimeValue::plain(
                    match (lv.truthiness(), rv.truthiness()) {
                        (_, Some(true)) => Value::Integer(1),
                        (Some(false), Some(false)) => Value::Integer(0),
                        _ => Value::Null,
                    },
                ));
            }

            if matches!(
                op,
                BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Is
                    | BinaryOp::IsNot
            ) && (is_row_value_expr(left) || is_row_value_expr(right))
            {
                return eval_row_value_comparison(
                    ctx, left, *op, right, params, scopes, ctes, agg_group,
                )
                .map(RuntimeValue::plain);
            }

            let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
            let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;

            let val = match op {
                BinaryOp::Concat => {
                    if lv.is_null() || rv.is_null() {
                        Value::Null
                    } else {
                        let mut s = lv.to_text_lossy();
                        s.push_str(&rv.to_text_lossy());
                        Value::Text(s)
                    }
                }
                BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Remainder => eval_binary_arith(*op, &lv, &rv),
                BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                    if lv.is_null() || rv.is_null() {
                        Value::Null
                    } else {
                        let a = lv.to_integer_lossy();
                        let b = rv.to_integer_lossy();
                        let res = match op {
                            BinaryOp::BitAnd => a & b,
                            BinaryOp::BitOr => a | b,
                            BinaryOp::ShiftLeft => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 {
                                        if a < 0 { -1 } else { 0 }
                                    } else {
                                        a >> (nb as u32)
                                    }
                                } else if b >= 64 {
                                    0
                                } else {
                                    a.wrapping_shl(b as u32)
                                }
                            }
                            BinaryOp::ShiftRight => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 {
                                        0
                                    } else {
                                        a.wrapping_shl(nb as u32)
                                    }
                                } else if b >= 64 {
                                    if a < 0 { -1 } else { 0 }
                                } else {
                                    a >> (b as u32)
                                }
                            }
                            _ => unreachable!(),
                        };
                        Value::Integer(res)
                    }
                }
                BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Is
                | BinaryOp::IsNot => {
                    let laff = infer_expr_affinity_in_scopes(left, scopes, ctx.schema, ctes);
                    let raff = infer_expr_affinity_in_scopes(right, scopes, ctx.schema, ctes);
                    let comp_aff = sqlite_compare_affinity(laff, raff);
                    let vdbe_op = match op {
                        BinaryOp::Lt => Opcode::Lt,
                        BinaryOp::Le => Opcode::Le,
                        BinaryOp::Gt => Opcode::Gt,
                        BinaryOp::Ge => Opcode::Ge,
                        BinaryOp::Eq => Opcode::Eq,
                        BinaryOp::Ne => Opcode::Ne,
                        BinaryOp::Is => Opcode::Is,
                        BinaryOp::IsNot => Opcode::IsNot,
                        _ => unreachable!(),
                    };
                    eval_scalar_comparison_in_scopes(
                        ctx, vdbe_op, left, &lv, right, &rv, comp_aff, scopes,
                    )?
                }
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            };
            Ok(RuntimeValue::plain(val))
        }
        Expr::IsNull {
            expr: inner,
            negated,
        } => {
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            let is_n = v.is_null();
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !is_n } else { is_n },
            ))))
        }
        Expr::Between {
            expr: val_e,
            low,
            high,
            negated,
        } => {
            if is_row_value_expr(val_e) || is_row_value_expr(low) || is_row_value_expr(high) {
                let ge = eval_row_value_comparison(
                    ctx,
                    val_e,
                    BinaryOp::Ge,
                    low,
                    params,
                    scopes,
                    ctes,
                    agg_group,
                )?
                .truthiness();
                let le = eval_row_value_comparison(
                    ctx,
                    val_e,
                    BinaryOp::Le,
                    high,
                    params,
                    scopes,
                    ctes,
                    agg_group,
                )?
                .truthiness();
                let res = match (ge, le) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                return Ok(RuntimeValue::plain(match res {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
                }));
            }
            let v = eval_expr(ctx, val_e, params, scopes, ctes, agg_group)?;
            let lo = eval_expr(ctx, low, params, scopes, ctes, agg_group)?;
            let hi = eval_expr(ctx, high, params, scopes, ctes, agg_group)?;
            let v_aff = infer_expr_affinity_in_scopes(val_e, scopes, ctx.schema, ctes);
            let lo_aff = sqlite_compare_affinity(
                v_aff,
                infer_expr_affinity_in_scopes(low, scopes, ctx.schema, ctes),
            );
            let hi_aff = sqlite_compare_affinity(
                v_aff,
                infer_expr_affinity_in_scopes(high, scopes, ctx.schema, ctes),
            );
            let ge = eval_scalar_comparison_in_scopes(
                ctx,
                Opcode::Ge,
                val_e,
                &v,
                low,
                &lo,
                lo_aff,
                scopes,
            )?
            .truthiness();
            let le = eval_scalar_comparison_in_scopes(
                ctx,
                Opcode::Le,
                val_e,
                &v,
                high,
                &hi,
                hi_aff,
                scopes,
            )?
            .truthiness();
            let res = match (ge, le) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            Ok(RuntimeValue::plain(match res {
                None => Value::Null,
                Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
            }))
        }
        Expr::InList {
            expr: lhs_e,
            list,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            // Validate RHS element tuple widths upfront before short-circuiting (`rowvalueA-2.0..2.3`)
            for item in list {
                let m = static_row_value_width(item, scopes, ctx.schema, ctes);
                if m != n {
                    if n >= 2 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
            }
            if list.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.schema, ctes);
            let enc = ctx.btree.encoding();
            let mut saw_null = false;
            for item in list {
                let iv_vec = if is_row_value_expr(item) {
                    eval_row_value_tuple(ctx, item, params, scopes, ctes, agg_group)?
                } else {
                    vec![eval_expr(ctx, item, params, scopes, ctes, agg_group)?]
                };
                if iv_vec.len() != n {
                    if n >= 2 {
                        let m = iv_vec.len();
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                let r_meta = infer_row_value_elem_meta(item, n, scopes, ctx.schema, ctes);
                let (affs, colls) = combine_row_value_meta(&l_meta, &r_meta);
                let eq_res = if n == 1
                    && let Some(cname) = resolve_custom_binary_collation_name(lhs_e, item)
                {
                    let mut svc = ExecutionContext {
                        interrupt_flag: ctx.interrupt_flag,
                        runtime: ctx.runtime,
                        encoding: enc,
                        functions: ctx.functions,
                        collations: ctx.collations,
                        progress: ctx.progress,
                        strict_subtype: ctx.strict_subtype,
                    };
                    eval_comparison_with_services(
                        Opcode::Eq,
                        &lv_vec[0],
                        &iv_vec[0],
                        affs[0],
                        cname,
                        enc,
                        &mut svc,
                    )?
                    .truthiness()
                } else {
                    compare_row_value_eq(&lv_vec, &iv_vec, &affs, &colls, enc)
                };
                match eq_res {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::InSubquery {
            expr: lhs_e,
            subquery,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            let sub_meta = infer_select_stmt_meta(subquery, scopes, ctx.schema, ctes);
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            if !sub_out.column_names.is_empty() && sub_out.column_names.len() != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected {}",
                        sub_out.column_names.len(),
                        n
                    ),
                ));
            }
            if sub_out.rows.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.schema, ctes);
            let mut affs = Vec::with_capacity(n);
            let mut colls = Vec::with_capacity(n);
            for i in 0..n {
                let (laff, lcoll, lexp) = l_meta.get(i).copied().unwrap_or((None, None, false));
                let (raff, rcoll) = sub_meta
                    .get(i)
                    .copied()
                    .unwrap_or((None, Collation::Binary));
                affs.push(sqlite_compare_affinity(laff, raff));
                let c = if lexp {
                    lcoll.unwrap_or(Collation::Binary)
                } else if let Some(lc) = lcoll {
                    lc
                } else {
                    rcoll
                };
                colls.push(c);
            }
            let enc = ctx.btree.encoding();
            let mut saw_null = false;
            for r in &sub_out.rows {
                match compare_row_value_eq(&lv_vec, r, &affs, &colls, enc) {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::InTable {
            expr: lhs_e,
            table,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            let (rows, rhs_meta) = if table.schema.is_none()
                && let Some(cte_out) = resolve_cte(ctx, ctes, &table.name, params, scopes)?
            {
                let m = cte_out.column_names.len();
                if m != n {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("sub-select returns {m} columns - expected {n}"),
                    ));
                }
                let meta = infer_cte_meta_by_name(&table.name, scopes, ctx.schema, ctes);
                (cte_out.rows, meta)
            } else {
                let (tbl, _) = resolve_table_for_query(ctx.schema, table)?;
                let m = tbl.columns.len();
                if m != n {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("sub-select returns {m} columns - expected {n}"),
                    ));
                }
                let meta: Vec<(Option<TypeAffinity>, Collation)> = tbl
                    .columns
                    .iter()
                    .map(|c| (Some(c.affinity), c.collation))
                    .collect();
                let raw = if tbl.root_pgno == 0 {
                    Vec::new()
                } else {
                    scan_table_raw_entries(ctx, &tbl)?
                };
                let mut out_rows = Vec::with_capacity(raw.len());
                for (rkey, payload) in raw {
                    out_rows.push(decode_table_logical_row(
                        ctx, &tbl, &rkey, &payload, params,
                    )?);
                }
                (out_rows, meta)
            };
            if rows.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.schema, ctes);
            let mut affs = Vec::with_capacity(n);
            let mut colls = Vec::with_capacity(n);
            for i in 0..n {
                let (laff, lcoll, lexp) = l_meta.get(i).copied().unwrap_or((None, None, false));
                let (raff, rcoll) = rhs_meta
                    .get(i)
                    .copied()
                    .unwrap_or((None, Collation::Binary));
                affs.push(sqlite_compare_affinity(laff, raff));
                let c = if lexp {
                    lcoll.unwrap_or(Collation::Binary)
                } else if let Some(lc) = lcoll {
                    lc
                } else {
                    rcoll
                };
                colls.push(c);
            }
            let enc = ctx.btree.encoding();
            let mut saw_null = false;
            for r in &rows {
                match compare_row_value_eq(&lv_vec, r, &affs, &colls, enc) {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            op,
            negated,
        } => {
            let tv = eval_expr(ctx, txt_e, params, scopes, ctes, agg_group)?;
            let pv = eval_expr(ctx, pattern, params, scopes, ctes, agg_group)?;
            if tv.is_null() || pv.is_null() {
                return Ok(RuntimeValue::null());
            }
            let esc = if let Some(ee) = escape {
                let ev = eval_expr(ctx, ee, params, scopes, ctes, agg_group)?;
                if ev.is_null() {
                    return Ok(RuntimeValue::null());
                }
                Some(parse_escape_char(&ev.to_text_lossy())?)
            } else {
                None
            };
            let matched = match op {
                LikeOp::Like => sqlite_like_match(&pv.to_text_lossy(), &tv.to_text_lossy(), esc),
                LikeOp::Glob => sqlite_glob_match(&pv.to_text_lossy(), &tv.to_text_lossy()),
            };
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !matched } else { matched },
            ))))
        }
        Expr::Cast {
            expr: inner,
            type_name,
        } => {
            let rv = eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(eval_cast_rt_with_encoding(
                rv,
                type_name,
                ctx.btree.encoding(),
            ))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let op_val = match operand {
                Some(oe) => Some(eval_expr(ctx, oe, params, scopes, ctes, agg_group)?),
                None => None,
            };
            let op_aff = operand
                .as_ref()
                .and_then(|oe| infer_expr_affinity_in_scopes(oe, scopes, ctx.schema, ctes));
            for (we, te) in branches {
                let cond_true = if let (Some(base), Some(oe)) = (&op_val, operand.as_ref()) {
                    let wv = eval_expr(ctx, we, params, scopes, ctes, agg_group)?;
                    let w_aff = infer_expr_affinity_in_scopes(we, scopes, ctx.schema, ctes);
                    let comp_aff = sqlite_compare_affinity(op_aff, w_aff);
                    eval_scalar_comparison_in_scopes(
                        ctx,
                        Opcode::Eq,
                        oe,
                        base,
                        we,
                        &wv,
                        comp_aff,
                        scopes,
                    )?
                    .truthiness()
                        == Some(true)
                } else {
                    eval_expr(ctx, we, params, scopes, ctes, agg_group)?.truthiness() == Some(true)
                };
                if cond_true {
                    return eval_expr_rt(ctx, te, params, scopes, ctes, agg_group);
                }
            }
            if let Some(el) = else_expr {
                eval_expr_rt(ctx, el, params, scopes, ctes, agg_group)
            } else {
                Ok(RuntimeValue::null())
            }
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            // Check if this is an aggregate call (`count(*)` and 0-arg `count()` are both `CountStar`)
            let is_count_zero = name.eq_ignore_ascii_case("COUNT")
                && matches!(args, FunctionArgs::Exprs(es) if es.is_empty())
                && ctx.functions.find_scalar(name, 0).is_none();
            let is_star = matches!(args, FunctionArgs::Star) || is_count_zero;
            let upper = name.to_ascii_uppercase();
            let agg_kind_opt = if is_star {
                AggregateKind::from_name(name, true)
            } else if let FunctionArgs::Exprs(es) = args {
                if ctx.functions.find_scalar(name, es.len()).is_some()
                    || ((upper == "MIN" || upper == "MAX") && es.len() != 1)
                {
                    None
                } else {
                    AggregateKind::from_name(name, false)
                }
            } else {
                None
            };

            if let Some(agg_kind) = agg_kind_opt {
                if is_count_zero && *distinct {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "DISTINCT aggregates must have exactly one argument",
                    ));
                }
                let Some((group_rows, base_outer)) = agg_group else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                };
                let arg_coll = match args {
                    FunctionArgs::Exprs(es) if es.len() == 1 => {
                        let ref_cols = group_rows.first().map_or(&[][..], |r| &r.cols[..]);
                        infer_expr_collation_from_cols(&es[0], ref_cols)
                    }
                    _ => Collation::Binary,
                };
                let mut acc =
                    AggregateAccumulator::new_with_collation(agg_kind, *distinct, arg_coll);
                for gr in group_rows {
                    let row_scopes = push_scope(base_outer, gr);
                    if let Some(f_expr) = filter {
                        let fv = eval_expr(ctx, f_expr, params, &row_scopes, ctes, None)?;
                        if fv.truthiness() != Some(true) {
                            continue;
                        }
                    }
                    if is_star {
                        acc.step(&[])?;
                    } else if let FunctionArgs::Exprs(es) = args {
                        let mut a_vals = Vec::with_capacity(es.len());
                        for ae in es {
                            a_vals.push(eval_expr(ctx, ae, params, &row_scopes, ctes, None)?);
                        }
                        acc.step(&a_vals)?;
                    }
                }
                return acc.finalize().map(RuntimeValue::plain);
            }

            let (star_args, expr_list) = match args {
                FunctionArgs::Star => (true, &[][..]),
                FunctionArgs::Exprs(es) => (false, es.as_slice()),
            };
            {
                let _tf_guard =
                    sqliters_vdbe::TestFunctionsScope::enter(ctx.runtime.enable_test_functions);
                ctx.functions.validate_call(
                    name,
                    star_args,
                    expr_list.len(),
                    *distinct,
                    filter.is_some(),
                    false,
                )?;
            }

            if !star_args
                && !*distinct
                && filter.is_none()
                && ctx.functions.find_scalar(name, expr_list.len()).is_none()
            {
                match upper.as_str() {
                    "COALESCE" | "IFNULL" => {
                        for ae in expr_list {
                            let rv = eval_expr_rt(ctx, ae, params, scopes, ctes, agg_group)?;
                            if !rv.value.is_null() {
                                return Ok(rv);
                            }
                        }
                        return Ok(RuntimeValue::null());
                    }
                    "IIF" | "IF" => {
                        let mut idx = 0;
                        while idx + 1 < expr_list.len() {
                            if eval_expr(ctx, &expr_list[idx], params, scopes, ctes, agg_group)?
                                .truthiness()
                                == Some(true)
                            {
                                return eval_expr_rt(
                                    ctx,
                                    &expr_list[idx + 1],
                                    params,
                                    scopes,
                                    ctes,
                                    agg_group,
                                );
                            }
                            idx += 2;
                        }
                        return if idx < expr_list.len() {
                            eval_expr_rt(ctx, &expr_list[idx], params, scopes, ctes, agg_group)
                        } else {
                            Ok(RuntimeValue::null())
                        };
                    }
                    _ => {}
                }
            }

            let mut a_vals = Vec::with_capacity(expr_list.len());
            for ae in expr_list {
                a_vals.push(eval_expr_rt(ctx, ae, params, scopes, ctes, agg_group)?);
            }
            ctx.call_scalar_rt(name, &a_vals)
        }
        Expr::Subquery(sub) => {
            let sub_out = exec_select(ctx, sub, params, scopes, ctes)?;
            if sub_out.column_names.len() > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected 1",
                        sub_out.column_names.len()
                    ),
                ));
            }
            Ok(RuntimeValue::plain(
                sub_out
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .cloned()
                    .unwrap_or(Value::Null),
            ))
        }
        Expr::Exists { subquery, negated } => {
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            let exists = !sub_out.rows.is_empty();
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !exists } else { exists },
            ))))
        }
    }
}

fn is_row_value_expr(mut e: &Expr) -> bool {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    matches!(e, Expr::RowValue(_) | Expr::Subquery(_))
}

fn eval_row_value_tuple(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Vec<Value>> {
    match expr {
        Expr::Collate { expr: inner, .. } => {
            eval_row_value_tuple(ctx, inner, params, scopes, ctes, agg_group)
        }
        Expr::RowValue(elems) => {
            let mut out = Vec::with_capacity(elems.len());
            for el in elems {
                out.push(eval_expr(ctx, el, params, scopes, ctes, agg_group)?);
            }
            Ok(out)
        }
        Expr::Subquery(sub) => {
            let sub_out = exec_select(ctx, sub, params, scopes, ctes)?;
            let width = sub_out.column_names.len().max(1);
            if let Some(first_row) = sub_out.rows.into_iter().next() {
                Ok(first_row)
            } else {
                Ok(vec![Value::Null; width])
            }
        }
        other => Ok(vec![eval_expr(
            ctx, other, params, scopes, ctes, agg_group,
        )?]),
    }
}

fn static_row_value_width(
    mut e: &Expr,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> usize {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    match e {
        Expr::RowValue(elems) => elems.len(),
        Expr::Subquery(sub) => infer_select_stmt_meta(sub, scopes, schema, ctes)
            .len()
            .max(1),
        _ => 1,
    }
}

fn infer_row_value_elem_meta(
    mut expr: &Expr,
    n: usize,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Option<Collation>, bool)> {
    let mut top_collate: Option<Collation> = None;
    while let Expr::Collate {
        expr: inner,
        collation,
    } = expr
    {
        if top_collate.is_none() {
            top_collate = Some(Collation::from_name(collation).unwrap_or_default());
        }
        expr = inner;
    }
    match expr {
        Expr::RowValue(elems) if elems.len() == n => elems
            .iter()
            .map(|el| {
                let aff = infer_expr_affinity_in_scopes(el, scopes, schema, ctes);
                let is_exp = matches!(el, Expr::Collate { .. });
                let coll = infer_expr_explicit_or_col_collation(el, &[], scopes);
                (aff, coll, is_exp)
            })
            .collect(),
        Expr::Subquery(sub) => {
            // Note: scalar/row-value `(SELECT ...)` propagates column affinities, NOT collations (`rowvalueA 6.1..6.10`)!
            let sub_meta = infer_select_stmt_meta(sub, scopes, schema, ctes);
            (0..n)
                .map(|i| {
                    let aff = sub_meta.get(i).and_then(|(a, _)| *a);
                    let (coll, is_exp) = if n == 1 && top_collate.is_some() {
                        (top_collate, true)
                    } else {
                        (None, false)
                    };
                    (aff, coll, is_exp)
                })
                .collect()
        }
        other if n == 1 => {
            let aff = infer_expr_affinity_in_scopes(other, scopes, schema, ctes);
            if let Some(tc) = top_collate {
                vec![(aff, Some(tc), true)]
            } else {
                let coll = infer_expr_explicit_or_col_collation(other, &[], scopes);
                vec![(aff, coll, false)]
            }
        }
        _ => vec![(None, None, false); n],
    }
}

fn combine_row_value_meta(
    l_meta: &[(Option<TypeAffinity>, Option<Collation>, bool)],
    r_meta: &[(Option<TypeAffinity>, Option<Collation>, bool)],
) -> (Vec<TypeAffinity>, Vec<Collation>) {
    let n = l_meta.len().max(r_meta.len());
    let mut affs = Vec::with_capacity(n);
    let mut colls = Vec::with_capacity(n);
    for i in 0..n {
        let (la, lc, le) = l_meta.get(i).copied().unwrap_or((None, None, false));
        let (ra, rc, re) = r_meta.get(i).copied().unwrap_or((None, None, false));
        affs.push(sqlite_compare_affinity(la, ra));
        let c = if le {
            lc.unwrap_or(Collation::Binary)
        } else if re {
            rc.unwrap_or(Collation::Binary)
        } else {
            lc.or(rc).unwrap_or(Collation::Binary)
        };
        colls.push(c);
    }
    (affs, colls)
}

fn compare_row_value_eq(
    lhs: &[Value],
    rhs: &[Value],
    affs: &[TypeAffinity],
    colls: &[Collation],
    enc: TextEncoding,
) -> Option<bool> {
    let mut saw_null = false;
    for i in 0..lhs.len() {
        let aff = affs.get(i).copied().unwrap_or(TypeAffinity::Blob);
        let coll = colls.get(i).copied().unwrap_or(Collation::Binary);
        match eval_comparison_with_affinity(Opcode::Eq, &lhs[i], &rhs[i], aff, coll, enc)
            .truthiness()
        {
            Some(false) => return Some(false),
            None => saw_null = true,
            Some(true) => {}
        }
    }
    if saw_null { None } else { Some(true) }
}

#[allow(clippy::too_many_arguments)]
fn eval_row_value_comparison(
    ctx: &mut ExecContext<'_>,
    left: &Expr,
    op: BinaryOp,
    right: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    let lv = eval_row_value_tuple(ctx, left, params, scopes, ctes, agg_group)?;
    let rv = eval_row_value_tuple(ctx, right, params, scopes, ctes, agg_group)?;
    if lv.len() != rv.len() {
        let is_left_sub = matches!(left, Expr::Subquery(_));
        let is_right_sub = matches!(right, Expr::Subquery(_));
        if is_left_sub && !is_right_sub {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "sub-select returns {} columns - expected {}",
                    lv.len(),
                    rv.len()
                ),
            ));
        }
        if is_right_sub && !is_left_sub {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "sub-select returns {} columns - expected {}",
                    rv.len(),
                    lv.len()
                ),
            ));
        }
        return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
    }

    let n = lv.len();
    let l_meta = infer_row_value_elem_meta(left, n, scopes, ctx.schema, ctes);
    let r_meta = infer_row_value_elem_meta(right, n, scopes, ctx.schema, ctes);
    let (affs, colls) = combine_row_value_meta(&l_meta, &r_meta);
    let enc = ctx.btree.encoding();

    match op {
        BinaryOp::Eq => Ok(match compare_row_value_eq(&lv, &rv, &affs, &colls, enc) {
            None => Value::Null,
            Some(b) => Value::Integer(i64::from(b)),
        }),
        BinaryOp::Ne => Ok(match compare_row_value_eq(&lv, &rv, &affs, &colls, enc) {
            None => Value::Null,
            Some(b) => Value::Integer(i64::from(!b)),
        }),
        BinaryOp::Is => {
            let all_eq = (0..n).all(|i| {
                eval_comparison_with_affinity(Opcode::Is, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    == Some(true)
            });
            Ok(Value::Integer(i64::from(all_eq)))
        }
        BinaryOp::IsNot => {
            let any_diff = (0..n).any(|i| {
                eval_comparison_with_affinity(Opcode::Is, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    != Some(true)
            });
            Ok(Value::Integer(i64::from(any_diff)))
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let strict_op = match op {
                BinaryOp::Lt | BinaryOp::Le => Opcode::Lt,
                BinaryOp::Gt | BinaryOp::Ge => Opcode::Gt,
                _ => unreachable!(),
            };
            let inclusive = matches!(op, BinaryOp::Le | BinaryOp::Ge);
            for i in 0..n {
                if lv[i].is_null() || rv[i].is_null() {
                    return Ok(Value::Null);
                }
                if eval_comparison_with_affinity(Opcode::Eq, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    == Some(true)
                {
                    if i + 1 == n {
                        return Ok(Value::Integer(i64::from(inclusive)));
                    }
                    continue;
                }
                return Ok(eval_comparison_with_affinity(
                    strict_op, &lv[i], &rv[i], affs[i], colls[i], enc,
                ));
            }
            Ok(Value::Integer(i64::from(inclusive)))
        }
        _ => unreachable!(),
    }
}

fn resolve_custom_binary_collation_name<'a>(left: &'a Expr, right: &'a Expr) -> Option<&'a str> {
    if let Expr::Collate { collation, .. } = left {
        return if Collation::from_name(collation).is_none() {
            Some(collation.as_str())
        } else {
            None
        };
    }
    if let Expr::Collate { collation, .. } = right {
        return if Collation::from_name(collation).is_none() {
            Some(collation.as_str())
        } else {
            None
        };
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn eval_scalar_comparison_in_scopes(
    ctx: &mut ExecContext<'_>,
    vdbe_op: Opcode,
    left_expr: &Expr,
    left_val: &Value,
    right_expr: &Expr,
    right_val: &Value,
    comp_aff: TypeAffinity,
    scopes: &[EvalRow],
) -> Result<Value> {
    let enc = ctx.btree.encoding();
    if let Some(cname) = resolve_custom_binary_collation_name(left_expr, right_expr) {
        let mut svc = ExecutionContext {
            interrupt_flag: ctx.interrupt_flag,
            runtime: ctx.runtime,
            encoding: enc,
            functions: ctx.functions,
            collations: ctx.collations,
            progress: ctx.progress,
            strict_subtype: ctx.strict_subtype,
        };
        eval_comparison_with_services(vdbe_op, left_val, right_val, comp_aff, cname, enc, &mut svc)
    } else {
        let coll = infer_comparison_collation(left_expr, right_expr, scopes);
        Ok(eval_comparison_with_affinity(
            vdbe_op, left_val, right_val, comp_aff, coll, enc,
        ))
    }
}

fn infer_comparison_collation(left: &Expr, right: &Expr, scopes: &[EvalRow]) -> Collation {
    if let Expr::Collate { collation, .. } = left {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Collate { collation, .. } = right {
        return Collation::from_name(collation).unwrap_or_default();
    }
    for e in [left, right] {
        if let Expr::Column(cref) = e {
            for s in scopes.iter().rev() {
                if let Ok(Some((_, c))) = s.lookup_cref(cref) {
                    return c;
                }
            }
        }
    }
    Collation::Binary
}

fn eval_binary_arith(op: BinaryOp, lhs: &Value, rhs: &Value) -> Value {
    if lhs.is_null() || rhs.is_null() {
        return Value::Null;
    }
    let coerce = |v: &Value| match v {
        Value::Integer(_) | Value::Real(_) => v.clone(),
        Value::Text(s) => sqliters_types::parse_numeric_text(s)
            .unwrap_or_else(|| Value::Integer(v.to_integer_lossy())),
        Value::Blob(_) => Value::Integer(v.to_integer_lossy()),
        Value::Null => Value::Null,
    };
    let a = coerce(lhs);
    let b = coerce(rhs);
    match (a, b) {
        (Value::Integer(ia), Value::Integer(ib)) => match op {
            BinaryOp::Add => ia
                .checked_add(ib)
                .map_or_else(|| Value::real((ia as f64) + (ib as f64)), Value::Integer),
            BinaryOp::Subtract => ia
                .checked_sub(ib)
                .map_or_else(|| Value::real((ia as f64) - (ib as f64)), Value::Integer),
            BinaryOp::Multiply => ia
                .checked_mul(ib)
                .map_or_else(|| Value::real((ia as f64) * (ib as f64)), Value::Integer),
            BinaryOp::Divide => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::real(-(ia as f64))
                } else {
                    Value::Integer(ia / ib)
                }
            }
            BinaryOp::Remainder => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::Integer(0)
                } else {
                    Value::Integer(ia % ib)
                }
            }
            _ => unreachable!(),
        },
        (va, vb) => {
            let fa = va.to_real_lossy();
            let fb = vb.to_real_lossy();
            match op {
                BinaryOp::Add => Value::real(fa + fb),
                BinaryOp::Subtract => Value::real(fa - fb),
                BinaryOp::Multiply => Value::real(fa * fb),
                BinaryOp::Divide => {
                    if fb == 0.0 {
                        Value::Null
                    } else {
                        Value::real(fa / fb)
                    }
                }
                BinaryOp::Remainder => {
                    let ia = va.to_integer_lossy();
                    let ib = vb.to_integer_lossy();
                    if ib == 0 {
                        Value::Null
                    } else {
                        Value::Integer(ia.wrapping_rem(ib))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

// ============================================================================
// Table Record <-> Logical Row Helpers & Constraints / Indexes
// ============================================================================

fn scan_table_raw_entries(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
) -> Result<Vec<(TableRowKey, Vec<u8>)>> {
    if !tbl.without_rowid {
        let rows = ctx.scan_table(tbl.root_pgno)?;
        return Ok(rows
            .into_iter()
            .map(|(rid, p)| (TableRowKey::Rowid(rid), p))
            .collect());
    }
    let enc = ctx.btree.encoding();
    let entries = ctx.scan_index(tbl.root_pgno, tbl.without_rowid_pk_orders())?;
    let n_pk = tbl.without_rowid_pk_cols.len();
    let mut out = Vec::with_capacity(entries.len());
    for rec in entries {
        let decoded = decode_record_with_encoding(&rec, enc)?;
        let pk = decoded[..n_pk.min(decoded.len())].to_vec();
        out.push((TableRowKey::WithoutRowidPk(pk), rec));
    }
    Ok(out)
}

fn seek_table_raw_entry(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    rkey: &TableRowKey,
) -> Result<Option<Vec<u8>>> {
    match rkey {
        TableRowKey::Rowid(rid) => ctx.btree.seek_table_rowid(tbl.root_pgno, *rid),
        TableRowKey::WithoutRowidPk(pk) => {
            let pk_orders = tbl.without_rowid_pk_orders();
            Ok(ctx
                .btree
                .seek_index_equal_raw(tbl.root_pgno, pk, &pk_orders)?
                .into_iter()
                .next())
        }
    }
}

fn table_keys_equal(
    tbl: &TableSchema,
    a: &TableRowKey,
    b: &TableRowKey,
    enc: TextEncoding,
) -> bool {
    match (a, b) {
        (TableRowKey::Rowid(r1), TableRowKey::Rowid(r2)) => r1 == r2,
        (TableRowKey::WithoutRowidPk(pk1), TableRowKey::WithoutRowidPk(pk2)) => {
            if pk1.len() != pk2.len() {
                return false;
            }
            pk1.iter()
                .zip(pk2.iter())
                .zip(tbl.without_rowid_pk_cols.iter())
                .all(|((v1, v2), ic)| {
                    v1.cmp_sqlite_with_encoding(v2, ic.sort_order.collation, enc) == Ordering::Equal
                })
        }
        _ => false,
    }
}

fn decode_table_logical_row(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    rkey: &TableRowKey,
    payload: &[u8],
    params: &[Value],
) -> Result<Vec<Value>> {
    let enc = ctx.btree.encoding();
    let phys_vals = decode_record_with_encoding(payload, enc)?;

    if tbl.without_rowid {
        let mut logical_vals = vec![Value::Null; tbl.columns.len()];
        let mut assigned = vec![false; tbl.columns.len()];
        for (pk_slot, ic) in tbl.without_rowid_pk_cols.iter().enumerate() {
            if let Some(ci) = ic.col_idx
                && !assigned[ci]
                && pk_slot < phys_vals.len()
            {
                logical_vals[ci] = phys_vals[pk_slot].clone();
                assigned[ci] = true;
            }
        }
        let n_pk = tbl.without_rowid_pk_cols.len();
        for (extra_slot, &ci) in tbl.without_rowid_extra_cols.iter().enumerate() {
            let phys_pos = n_pk + extra_slot;
            let col = &tbl.columns[ci];
            let v = if phys_pos < phys_vals.len() {
                phys_vals[phys_pos].clone()
            } else {
                let def_val = match &col.default_expr {
                    Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                    None => Value::Null,
                };
                let mut coerced = col.affinity.apply(def_val);
                if tbl.strict
                    && col.type_name.trim().eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                coerced
            };
            logical_vals[ci] = v;
            assigned[ci] = true;
        }
        for ci in 0..tbl.columns.len() {
            if let Some((ref gexpr, false)) = tbl.columns[ci].generated {
                let scope = make_single_table_scope(tbl, None, 0, &logical_vals);
                let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
                let mut coerced = tbl.columns[ci].affinity.apply(gv);
                if tbl.strict
                    && tbl.columns[ci]
                        .type_name
                        .trim()
                        .eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                logical_vals[ci] = coerced;
            }
        }
        return Ok(logical_vals);
    }

    let rowid = rkey.rowid_or_zero();
    let has_virtual = tbl
        .columns
        .iter()
        .any(|c| c.generated.as_ref().is_some_and(|(_, stored)| !*stored));

    if !has_virtual {
        let mut vals = phys_vals;
        while vals.len() < tbl.columns.len() {
            let missing_idx = vals.len();
            let col = &tbl.columns[missing_idx];
            let def_val = match &col.default_expr {
                Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                None => Value::Null,
            };
            let mut coerced = col.affinity.apply(def_val);
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            vals.push(coerced);
        }
        vals.truncate(tbl.columns.len());
        if let Some(ipk) = tbl.rowid_alias_col
            && ipk < vals.len()
        {
            vals[ipk] = Value::Integer(rowid);
        }
        return Ok(vals);
    }

    let mut logical_vals = Vec::with_capacity(tbl.columns.len());
    let mut phys_idx = 0usize;
    for (ci, col) in tbl.columns.iter().enumerate() {
        let is_virt = col.generated.as_ref().is_some_and(|(_, stored)| !*stored);
        if is_virt {
            logical_vals.push(Value::Null);
        } else {
            let v = if tbl.rowid_alias_col == Some(ci) {
                phys_idx += 1;
                Value::Integer(rowid)
            } else if phys_idx < phys_vals.len() {
                let pv = phys_vals[phys_idx].clone();
                phys_idx += 1;
                pv
            } else {
                let def_val = match &col.default_expr {
                    Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                    None => Value::Null,
                };
                let mut coerced = col.affinity.apply(def_val);
                if tbl.strict
                    && col.type_name.trim().eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                coerced
            };
            logical_vals.push(v);
        }
    }

    for ci in 0..tbl.columns.len() {
        if let Some((ref gexpr, false)) = tbl.columns[ci].generated {
            let scope = make_single_table_scope(tbl, None, rowid, &logical_vals);
            let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
            let mut coerced = tbl.columns[ci].affinity.apply(gv);
            if tbl.strict
                && tbl.columns[ci]
                    .type_name
                    .trim()
                    .eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            logical_vals[ci] = coerced;
        }
    }
    Ok(logical_vals)
}

fn encode_table_physical_record(
    tbl: &TableSchema,
    logical_vals: &[Value],
    enc: TextEncoding,
) -> Vec<u8> {
    if tbl.without_rowid {
        let mut slots = Vec::with_capacity(
            tbl.without_rowid_pk_cols.len() + tbl.without_rowid_extra_cols.len(),
        );
        for ic in &tbl.without_rowid_pk_cols {
            let ci = ic.col_idx.expect("WITHOUT ROWID PK col has col_idx");
            slots.push(logical_vals[ci].clone());
        }
        for &ci in &tbl.without_rowid_extra_cols {
            slots.push(logical_vals[ci].clone());
        }
        return encode_record_with_encoding(&slots, enc);
    }

    let mut slots = Vec::with_capacity(logical_vals.len());
    for (i, v) in logical_vals.iter().enumerate() {
        let is_virt = tbl
            .columns
            .get(i)
            .and_then(|c| c.generated.as_ref())
            .is_some_and(|(_, stored)| !*stored);
        if is_virt {
            continue;
        }
        // Per C SQLite `insert.c:1361`: put NULL in the INTEGER PRIMARY KEY slot of the on-disk record!
        if tbl.rowid_alias_col == Some(i) {
            slots.push(Value::Null);
        } else {
            slots.push(v.clone());
        }
    }
    encode_record_with_encoding(&slots, enc)
}

fn make_single_table_scope(
    tbl: &TableSchema,
    alias: Option<&str>,
    rowid: i64,
    vals: &[Value],
) -> EvalRow {
    let eff = alias.unwrap_or(&tbl.name).to_owned();
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            schema_name: Some("main".to_owned()),
            table_alias: Some(eff.clone()),
            name: c.name.clone(),
            collation: c.collation,
            affinity: Some(c.affinity),
            hidden_rowid: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            schema_name: Some("main".to_owned()),
            table_alias: Some(eff),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            affinity: Some(TypeAffinity::Integer),
            hidden_rowid: true,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow {
        cols: cols.into(),
        vals: row_vals,
    }
}

/// Builds the single-table evaluation scope for `RETURNING` (`sqlite/src/trigger.c:1062-1064`,
/// `sqlite/src/resolve.c:522-536`), where `zDb == 0` and only unqualified column names or
/// `<table_name>.<col>` (not statement `AS alias` or `schema.table.col`) resolve.
fn make_returning_table_scope(tbl: &TableSchema, rowid: i64, vals: &[Value]) -> EvalRow {
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            schema_name: None,
            table_alias: Some(tbl.name.clone()),
            name: c.name.clone(),
            collation: c.collation,
            affinity: Some(c.affinity),
            hidden_rowid: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            schema_name: None,
            table_alias: Some(tbl.name.clone()),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            affinity: Some(TypeAffinity::Integer),
            hidden_rowid: true,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow {
        cols: cols.into(),
        vals: row_vals,
    }
}

fn check_strict_column_type(tbl_name: &str, col: &ColumnSchema, val: &Value) -> Result<()> {
    if val.is_null() {
        return Ok(());
    }
    let upper = col.type_name.trim().to_ascii_uppercase();
    let ok = match upper.as_str() {
        "INT" | "INTEGER" => matches!(val, Value::Integer(_)),
        "REAL" => matches!(val, Value::Real(_) | Value::Integer(_)),
        "TEXT" => matches!(val, Value::Text(_)),
        "BLOB" => matches!(val, Value::Blob(_)),
        "ANY" | "" => true,
        _ => true,
    };
    if !ok {
        return Err(SqliteError::constraint(
            extended::SQLITE_CONSTRAINT_DATATYPE,
            format!(
                "cannot store {} value in {} column {}.{}",
                val.storage_class().typeof_str().to_ascii_uppercase(),
                col.type_name,
                tbl_name,
                col.name
            ),
        ));
    }
    Ok(())
}

fn index_row_passes_partial_where(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rowid: i64,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<bool> {
    let Some(ref w) = idx.where_clause else {
        return Ok(true);
    };
    let scope = make_single_table_scope(tbl, None, rowid, logical_vals);
    Ok(eval_expr(ctx, w, params, &[scope], &CteStore::default(), None)?.truthiness() == Some(true))
}

fn eval_index_column_values(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rowid: i64,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<Vec<Value>> {
    let mut vals = Vec::with_capacity(idx.columns.len());
    let needs_scope = idx.columns.iter().any(|ic| ic.col_idx.is_none());
    let scope_arr = if needs_scope {
        vec![make_single_table_scope(tbl, None, rowid, logical_vals)]
    } else {
        Vec::new()
    };
    for ic in &idx.columns {
        if let Some(ci) = ic.col_idx {
            vals.push(logical_vals[ci].clone());
        } else if let Some(ref e) = ic.expr {
            vals.push(eval_expr(
                ctx,
                e,
                params,
                &scope_arr,
                &CteStore::default(),
                None,
            )?);
        } else {
            vals.push(Value::Null);
        }
    }
    Ok(vals)
}

fn eval_index_key_values(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rkey: &TableRowKey,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<Vec<Value>> {
    let rowid = rkey.rowid_or_zero();
    let mut keys = eval_index_column_values(ctx, tbl, idx, rowid, logical_vals, params)?;
    if tbl.without_rowid {
        for tc in &idx.trailing_pk_cols {
            let ci = tc
                .col_idx
                .expect("WITHOUT ROWID trailing PK col has col_idx");
            keys.push(logical_vals[ci].clone());
        }
    } else {
        keys.push(Value::Integer(rowid));
    }
    Ok(keys)
}

fn format_unique_index_failure(tbl: &TableSchema, idx: &IndexSchema) -> String {
    if idx.columns.iter().any(|c| c.col_idx.is_none()) {
        format!("UNIQUE constraint failed: index '{}'", idx.name)
    } else {
        let cols_str: Vec<String> = idx
            .columns
            .iter()
            .map(|c| format!("{}.{}", tbl.name, c.name))
            .collect();
        format!("UNIQUE constraint failed: {}", cols_str.join(", "))
    }
}

/// Deletes `rkey` AND all its secondary index entries from `tbl`.
fn remove_row_and_indexes(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    rkey: &TableRowKey,
    params: &[Value],
) -> Result<bool> {
    if indexes.is_empty() {
        return match rkey {
            TableRowKey::Rowid(rowid) => ctx.btree.delete_table(tbl.root_pgno, *rowid),
            TableRowKey::WithoutRowidPk(pk) => {
                let pk_orders = tbl.without_rowid_pk_orders();
                ctx.btree.delete_index(tbl.root_pgno, pk, &pk_orders)
            }
        };
    }
    let Some(old_payload) = seek_table_raw_entry(ctx, tbl, rkey)? else {
        return Ok(false);
    };
    let old_vals = decode_table_logical_row(ctx, tbl, rkey, &old_payload, params)?;
    let rowid = rkey.rowid_or_zero();
    for idx in indexes {
        if index_row_passes_partial_where(ctx, tbl, idx, rowid, &old_vals, params)? {
            let keys = eval_index_key_values(ctx, tbl, idx, rkey, &old_vals, params)?;
            let orders = idx.key_sort_orders();
            ctx.btree.delete_index(idx.root_pgno, &keys, &orders)?;
        }
    }
    match rkey {
        TableRowKey::Rowid(rid) => ctx.btree.delete_table(tbl.root_pgno, *rid),
        TableRowKey::WithoutRowidPk(pk) => {
            let pk_orders = tbl.without_rowid_pk_orders();
            ctx.btree.delete_index(tbl.root_pgno, pk, &pk_orders)
        }
    }
}

/// Validates constraints (NOT NULL, STRICT, CHECK, Rowid/WITHOUT ROWID PK, UNIQUE indexes), applies conflict resolution
/// (`IGNORE` / `REPLACE` / error with `ConflictResolutionEffect`), and writes the row + secondary index entries!
/// Returns `Ok(Some((final_key, final_logical_vals)))` if written, or `Ok(None)` if skipped via `IGNORE`.
#[allow(clippy::too_many_arguments)]
fn validate_and_write_row(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    candidate_rowid: Option<i64>,
    mut raw_vals: Vec<Value>,
    stmt_conflict: Option<ConflictAction>,
    existing_self_key: Option<&TableRowKey>,
    params: &[Value],
    bump_last_rowid: bool,
    count_as_top_level_change: bool,
    stmt_fk_suspects: &mut Vec<(String, TableRowKey, usize)>,
    fk_depth: usize,
) -> Result<Option<(TableRowKey, Vec<Value>)>> {
    // 1. Determine final rowid (for rowid tables) & apply column affinities
    let rowid = if tbl.without_rowid {
        0
    } else {
        let mut final_rowid = match candidate_rowid {
            Some(r) => Some(r),
            None => match tbl.rowid_alias_col {
                Some(ipk) => {
                    if raw_vals[ipk].is_null() {
                        None
                    } else {
                        let coerced = TypeAffinity::Integer.apply(raw_vals[ipk].clone());
                        let Some(i) = coerced.as_i64() else {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        };
                        Some(i)
                    }
                }
                None => None,
            },
        };
        if final_rowid.is_none() {
            final_rowid = Some(allocate_table_rowid(ctx, tbl)?);
        }
        final_rowid.unwrap()
    };

    for (i, col) in tbl.columns.iter().enumerate() {
        if col.generated.is_some() {
            continue;
        }
        if tbl.rowid_alias_col == Some(i) {
            raw_vals[i] = Value::Integer(rowid);
        } else {
            raw_vals[i] = col.affinity.apply(raw_vals[i].clone());
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = raw_vals[i]
            {
                raw_vals[i] = Value::real(iv as f64);
            }
        }
    }

    // Evaluate VIRTUAL and STORED generated columns in declaration order
    for i in 0..tbl.columns.len() {
        if let Some((ref gexpr, _)) = tbl.columns[i].generated {
            let scope = make_single_table_scope(tbl, None, rowid, &raw_vals);
            let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
            let mut coerced = tbl.columns[i].affinity.apply(gv);
            if tbl.strict
                && tbl.columns[i].type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            raw_vals[i] = coerced;
        }
    }

    // 2. STRICT type check
    if tbl.strict {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        for (i, col) in tbl.columns.iter().enumerate() {
            if let Err(err) = check_strict_column_type(&tbl.name, col, &raw_vals[i]) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(err.with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    // 3. NOT NULL constraint check
    for (i, col) in tbl.columns.iter().enumerate() {
        if let Some(col_conflict) = col.not_null
            && raw_vals[i].is_null()
        {
            let eff = stmt_conflict.unwrap_or(col_conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace if col.default_expr.is_some() => {
                    let dv = eval_expr(
                        ctx,
                        col.default_expr.as_ref().unwrap(),
                        params,
                        &[],
                        &CteStore::default(),
                        None,
                    )?;
                    if dv.is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                        ));
                    }
                    let mut coerced = col.affinity.apply(dv);
                    if tbl.strict
                        && col.type_name.trim().eq_ignore_ascii_case("REAL")
                        && let Value::Integer(iv) = coerced
                    {
                        coerced = Value::real(iv as f64);
                    }
                    raw_vals[i] = coerced;
                }
                _ => {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    }

    // 4. CHECK constraints (`NULL` or `true` passes in SQL; only `false` fails!)
    if !tbl.checks.is_empty() {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        let scope = make_single_table_scope(tbl, None, rowid, &raw_vals);
        for chk in &tbl.checks {
            let cv = eval_expr(
                ctx,
                &chk.expr,
                params,
                std::slice::from_ref(&scope),
                &CteStore::default(),
                None,
            )?;
            if cv.truthiness() == Some(false) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_CHECK,
                    format!("CHECK constraint failed: {}", chk.label),
                )
                .with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    let enc = ctx.btree.encoding();
    let final_key = if tbl.without_rowid {
        TableRowKey::WithoutRowidPk(tbl.extract_without_rowid_pk(&raw_vals))
    } else {
        TableRowKey::Rowid(rowid)
    };

    // 5. Primary B-Tree uniqueness check (Rowid / INTEGER PRIMARY KEY or WITHOUT ROWID PRIMARY KEY)
    if tbl.without_rowid {
        let is_self =
            existing_self_key.is_some_and(|sk| table_keys_equal(tbl, &final_key, sk, enc));
        if !is_self && let Some(conflict_payload) = seek_table_raw_entry(ctx, tbl, &final_key)? {
            let decoded_conflict = decode_record_with_encoding(&conflict_payload, enc)?;
            let n_pk = tbl.without_rowid_pk_cols.len();
            let conflict_key = TableRowKey::WithoutRowidPk(
                decoded_conflict[..n_pk.min(decoded_conflict.len())].to_vec(),
            );
            let eff = stmt_conflict.unwrap_or(tbl.without_rowid_pk_conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace => {
                    let conflict_vals = decode_table_logical_row(
                        ctx,
                        tbl,
                        &conflict_key,
                        &conflict_payload,
                        params,
                    )?;
                    if *ctx.foreign_keys {
                        check_parent_delete_restrict(ctx, tbl, &conflict_vals, params)?;
                    }
                    remove_row_and_indexes(ctx, tbl, indexes, &conflict_key, params)?;
                    if *ctx.foreign_keys {
                        apply_parent_delete_fk_actions(
                            ctx,
                            tbl,
                            &conflict_vals,
                            params,
                            stmt_fk_suspects,
                            fk_depth + 1,
                        )?;
                    }
                }
                _ => {
                    let pk_idx = tbl
                        .without_rowid_pk_as_index()
                        .expect("WITHOUT ROWID table has PK");
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                        format_unique_index_failure(tbl, &pk_idx),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    } else if existing_self_key != Some(&final_key)
        && let Some(conflict_payload) = ctx.btree.seek_table_rowid(tbl.root_pgno, rowid)?
    {
        let conflict_key = TableRowKey::Rowid(rowid);
        let eff = stmt_conflict.unwrap_or(tbl.rowid_pk_conflict);
        match eff {
            ConflictAction::Ignore => return Ok(None),
            ConflictAction::Replace => {
                let conflict_vals =
                    decode_table_logical_row(ctx, tbl, &conflict_key, &conflict_payload, params)?;
                if *ctx.foreign_keys {
                    check_parent_delete_restrict(ctx, tbl, &conflict_vals, params)?;
                }
                remove_row_and_indexes(ctx, tbl, indexes, &conflict_key, params)?;
                if *ctx.foreign_keys {
                    apply_parent_delete_fk_actions(
                        ctx,
                        tbl,
                        &conflict_vals,
                        params,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
            _ => {
                let (ext_code, col_label) = match tbl.rowid_alias_col {
                    Some(ipk) => (
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                        tbl.columns[ipk].name.as_str(),
                    ),
                    None => (extended::SQLITE_CONSTRAINT_ROWID, "rowid"),
                };
                return Err(SqliteError::constraint(
                    ext_code,
                    format!("UNIQUE constraint failed: {}.{}", tbl.name, col_label),
                )
                .with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    // 6. Secondary UNIQUE index constraint checks
    for idx in indexes {
        if !idx.unique {
            continue;
        }
        if !index_row_passes_partial_where(ctx, tbl, idx, rowid, &raw_vals, params)? {
            continue;
        }
        let indexed_vals = eval_index_column_values(ctx, tbl, idx, rowid, &raw_vals, params)?;
        // SQL standard & SQLite: NULLs are distinct in UNIQUE indexes!
        if indexed_vals.iter().any(Value::is_null) {
            continue;
        }
        let orders = idx.key_sort_orders();
        let hits = ctx.btree.seek_index_equal(
            idx.root_pgno,
            &indexed_vals,
            &orders[..indexed_vals.len()],
        )?;
        let mut conflicting_keys = Vec::new();
        for hit in hits {
            let hit_key_opt = if tbl.without_rowid {
                Some(TableRowKey::WithoutRowidPk(
                    idx.extract_without_rowid_pk_from_hit(tbl, &hit),
                ))
            } else {
                hit.last().and_then(Value::as_i64).map(TableRowKey::Rowid)
            };
            if let Some(hit_key) = hit_key_opt {
                let is_self =
                    existing_self_key.is_some_and(|sk| table_keys_equal(tbl, &hit_key, sk, enc));
                let is_same_target = table_keys_equal(tbl, &hit_key, &final_key, enc);
                if !is_self && !is_same_target {
                    conflicting_keys.push(hit_key);
                }
            }
        }
        if !conflicting_keys.is_empty() {
            let eff = stmt_conflict.unwrap_or(idx.conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace => {
                    for ckey in conflicting_keys {
                        if let Some(cpayload) = seek_table_raw_entry(ctx, tbl, &ckey)? {
                            let cvals =
                                decode_table_logical_row(ctx, tbl, &ckey, &cpayload, params)?;
                            if *ctx.foreign_keys {
                                check_parent_delete_restrict(ctx, tbl, &cvals, params)?;
                            }
                            remove_row_and_indexes(ctx, tbl, indexes, &ckey, params)?;
                            if *ctx.foreign_keys {
                                apply_parent_delete_fk_actions(
                                    ctx,
                                    tbl,
                                    &cvals,
                                    params,
                                    stmt_fk_suspects,
                                    fk_depth + 1,
                                )?;
                            }
                        }
                    }
                }
                _ => {
                    let ext_code = if idx.origin == "pk" {
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY
                    } else {
                        extended::SQLITE_CONSTRAINT_UNIQUE
                    };
                    return Err(SqliteError::constraint(
                        ext_code,
                        format_unique_index_failure(tbl, idx),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    }

    // 7. If updating an existing row in-place or moving keys, check parent UPDATE RESTRICT before mutating
    let old_logical_vals_opt = if let Some(old_key) = existing_self_key {
        let old_vals = if let Some(op) = seek_table_raw_entry(ctx, tbl, old_key)? {
            Some(decode_table_logical_row(ctx, tbl, old_key, &op, params)?)
        } else {
            None
        };
        if *ctx.foreign_keys
            && let Some(ref ov) = old_vals
        {
            check_parent_update_restrict(ctx, tbl, ov, &raw_vals, params)?;
        }
        remove_row_and_indexes(ctx, tbl, indexes, old_key, params)?;
        old_vals
    } else {
        None
    };

    // 8. Write table primary B-Tree record & secondary index records
    let phys_rec = encode_table_physical_record(tbl, &raw_vals, enc);
    if tbl.without_rowid {
        let pk_orders = tbl.without_rowid_pk_orders();
        ctx.btree
            .insert_index(tbl.root_pgno, &phys_rec, &pk_orders)?;
    } else {
        ctx.btree.insert_table(tbl.root_pgno, rowid, &phys_rec)?;
    }

    for idx in indexes {
        if index_row_passes_partial_where(ctx, tbl, idx, rowid, &raw_vals, params)? {
            let keys = eval_index_key_values(ctx, tbl, idx, &final_key, &raw_vals, params)?;
            let idx_rec = encode_record_with_encoding(&keys, enc);
            let orders = idx.key_sort_orders();
            ctx.btree.insert_index(idx.root_pgno, &idx_rec, &orders)?;
        }
    }

    if tbl.autoincrement {
        update_sqlite_sequence(ctx, &tbl.name, rowid)?;
    }

    // 9. Foreign key checks on child row & cascading parent UPDATE actions
    if *ctx.foreign_keys {
        register_child_fk_checks(ctx, tbl, &final_key, &raw_vals, stmt_fk_suspects)?;
        if let Some(ref old_vals) = old_logical_vals_opt {
            apply_parent_update_fk_actions(
                ctx,
                tbl,
                old_vals,
                &raw_vals,
                params,
                stmt_fk_suspects,
                fk_depth + 1,
            )?;
        }
    }

    if bump_last_rowid && !tbl.without_rowid {
        ctx.runtime.last_insert_rowid = rowid;
    }
    if count_as_top_level_change {
        ctx.runtime.last_changes += 1;
    }
    ctx.runtime.total_changes += 1;
    ctx.publish();

    Ok(Some((final_key, raw_vals)))
}

fn allocate_table_rowid(ctx: &mut ExecContext<'_>, tbl: &TableSchema) -> Result<i64> {
    if !tbl.autoincrement {
        return ctx.btree.next_rowid(tbl.root_pgno);
    }

    let max_existing = ctx.btree.max_table_rowid(tbl.root_pgno)?.unwrap_or(0);
    let mut prev_seq = 0i64;
    if let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() {
        let enc = ctx.btree.encoding();
        let rows = scan_table_incremental(ctx.btree, seq_tbl.root_pgno)?;
        for (_, payload) in rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.len() >= 2 && cols[0].as_str() == Some(tbl.name.as_str()) {
                prev_seq = cols[1].to_integer_lossy();
                break;
            }
        }
    }
    prev_seq
        .max(max_existing)
        .checked_add(1)
        .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database or disk is full"))
}

fn update_sqlite_sequence(ctx: &mut ExecContext<'_>, tbl_name: &str, rowid: i64) -> Result<()> {
    let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() else {
        return Ok(());
    };
    let enc = ctx.btree.encoding();
    let rows = scan_table_incremental(ctx.btree, seq_tbl.root_pgno)?;
    for (seq_rid, payload) in rows {
        let cols = decode_record_with_encoding(&payload, enc)?;
        if cols.len() >= 2 && cols[0].as_str() == Some(tbl_name) {
            let cur = cols[1].to_integer_lossy();
            if rowid > cur {
                let updated = encode_record_with_encoding(
                    &[Value::Text(tbl_name.to_owned()), Value::Integer(rowid)],
                    enc,
                );
                ctx.btree
                    .insert_table(seq_tbl.root_pgno, seq_rid, &updated)?;
            }
            return Ok(());
        }
    }
    if rowid > 0 {
        let new_rid = ctx.btree.next_rowid(seq_tbl.root_pgno)?;
        let rec = encode_record_with_encoding(
            &[Value::Text(tbl_name.to_owned()), Value::Integer(rowid)],
            enc,
        );
        ctx.btree.insert_table(seq_tbl.root_pgno, new_rid, &rec)?;
    }
    Ok(())
}

// ============================================================================
// Foreign Key Enforcement & Referential Actions (`sqlite/src/fkey.c`)
// ============================================================================

const MAX_FK_CASCADE_DEPTH: usize = 1000;

fn fk_constraint_err() -> SqliteError {
    SqliteError::constraint(
        extended::SQLITE_CONSTRAINT_FOREIGNKEY,
        "FOREIGN KEY constraint failed",
    )
}

/// Resolves the target parent table and parent column indices for `fk` on `child_tbl_name`,
/// verifying SQLite's parent key existence and `PRIMARY KEY` / `UNIQUE` index requirement (`sqlite3FkLocateIndex`).
fn resolve_fk_parent_cols(
    schema: &Schema,
    child_tbl_name: &str,
    fk: &ForeignKeySchema,
) -> Result<(TableSchema, Vec<usize>)> {
    let Some(ptbl) = schema.find_user_table(&fk.ref_table).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: main.{}", fk.ref_table),
        ));
    };

    let mismatch_err = || {
        SqliteError::new(
            ErrorCode::Error,
            format!(
                "foreign key mismatch - \"{}\" referencing \"{}\"",
                child_tbl_name, ptbl.name
            ),
        )
    };

    let parent_col_idxs: Vec<usize> = match &fk.ref_cols {
        None => {
            let mut pk_cols: Vec<(usize, usize)> = ptbl
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_primary_key)
                .map(|(idx, c)| (c.pk_order, idx))
                .collect();
            pk_cols.sort_by_key(|&(ord, _)| ord);
            if pk_cols.is_empty() || pk_cols.len() != fk.from_cols.len() {
                return Err(mismatch_err());
            }
            pk_cols.into_iter().map(|(_, idx)| idx).collect()
        }
        Some(names) => {
            if names.len() != fk.from_cols.len() {
                return Err(mismatch_err());
            }
            let mut idxs = Vec::with_capacity(names.len());
            for n in names {
                let Some((ci, _)) = ptbl.find_column(n) else {
                    return Err(mismatch_err());
                };
                idxs.push(ci);
            }
            // Verify `idxs` is covered by rowid PK or a non-partial UNIQUE/PK index on `ptbl`
            let is_rowid_pk = idxs.len() == 1 && ptbl.rowid_alias_col == Some(idxs[0]);
            let has_matching_unique_idx = if is_rowid_pk {
                true
            } else {
                let mut cand_idxs = schema.indexes_for_table(&ptbl.name);
                if let Some(pk_idx) = ptbl.without_rowid_pk_as_index() {
                    cand_idxs.push(pk_idx);
                }
                cand_idxs.iter().any(|idx| {
                    idx.unique
                        && idx.where_clause.is_none()
                        && idx.columns.len() == idxs.len()
                        && idxs.iter().all(|&c_idx| {
                            idx.columns.iter().any(|ic| {
                                ic.col_idx == Some(c_idx)
                                    && ic.sort_order.collation == ptbl.columns[c_idx].collation
                            })
                        })
                })
            };
            if !has_matching_unique_idx {
                return Err(mismatch_err());
            }
            idxs
        }
    };

    Ok((ptbl, parent_col_idxs))
}

/// Checks whether a non-NULL child key tuple `child_vals[fk.from_cols]` has a matching parent row in `ptbl`.
fn parent_row_exists_for_fk(
    ctx: &mut ExecContext<'_>,
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    child_vals: &[Value],
    from_cols: &[usize],
) -> Result<bool> {
    if from_cols.iter().any(|&ci| child_vals[ci].is_null()) {
        return Ok(true);
    }

    // Fast path: single-column `INTEGER PRIMARY KEY` (`rowid_alias_col`)
    if parent_col_idxs.len() == 1 && ptbl.rowid_alias_col == Some(parent_col_idxs[0]) {
        let coerced = TypeAffinity::Integer.apply(child_vals[from_cols[0]].clone());
        let Value::Integer(target_rid) = coerced else {
            return Ok(false);
        };
        return Ok(ctx
            .btree
            .seek_table_rowid(ptbl.root_pgno, target_rid)?
            .is_some());
    }

    let enc = ctx.btree.encoding();
    let coerced_keys: Vec<(Value, Collation)> = parent_col_idxs
        .iter()
        .zip(from_cols.iter())
        .map(|(&p_idx, &c_idx)| {
            let pcol = &ptbl.columns[p_idx];
            (
                pcol.affinity.apply(child_vals[c_idx].clone()),
                pcol.collation,
            )
        })
        .collect();

    let raw_rows = scan_table_raw_entries(ctx, ptbl)?;
    for (pkey, ppayload) in raw_rows {
        let pvals = decode_table_logical_row(ctx, ptbl, &pkey, &ppayload, &[])?;
        let matches =
            parent_col_idxs
                .iter()
                .zip(coerced_keys.iter())
                .all(|(&p_idx, (ck, coll))| {
                    !pvals[p_idx].is_null()
                        && eval_comparison_with_encoding(Opcode::Eq, &pvals[p_idx], ck, *coll, enc)
                            .truthiness()
                            == Some(true)
                });
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

fn child_row_matches_parent_key(
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    parent_vals: &[Value],
    child_vals: &[Value],
    from_cols: &[usize],
    enc: TextEncoding,
) -> bool {
    if from_cols.iter().any(|&ci| child_vals[ci].is_null())
        || parent_col_idxs.iter().any(|&pi| parent_vals[pi].is_null())
    {
        return false;
    }
    parent_col_idxs
        .iter()
        .zip(from_cols.iter())
        .all(|(&p_idx, &c_idx)| {
            let pcol = &ptbl.columns[p_idx];
            let coerced_child = pcol.affinity.apply(child_vals[c_idx].clone());
            eval_comparison_with_encoding(
                Opcode::Eq,
                &parent_vals[p_idx],
                &coerced_child,
                pcol.collation,
                enc,
            )
            .truthiness()
                == Some(true)
        })
}

fn register_child_fk_checks(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    rkey: &TableRowKey,
    written_vals: &[Value],
    stmt_fk_suspects: &mut Vec<(String, TableRowKey, usize)>,
) -> Result<()> {
    for fk in &tbl.foreign_keys {
        let (ptbl, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &tbl.name, fk)?;
        if fk.from_cols.iter().any(|&ci| written_vals[ci].is_null()) {
            continue;
        }
        if !parent_row_exists_for_fk(ctx, &ptbl, &parent_col_idxs, written_vals, &fk.from_cols)? {
            if fk.deferred {
                ctx.deferred_fk_suspects
                    .push((tbl.name.clone(), rkey.clone(), fk.id));
            } else {
                // Per SQLite `OP_FkCounter` / `sqlite3VdbeCheckFkImmediate`, non-RESTRICT immediate FKs
                // are verified at statement completion so multi-row / multi-cascade updates settle cleanly.
                stmt_fk_suspects.push((tbl.name.clone(), rkey.clone(), fk.id));
            }
        }
    }
    Ok(())
}

fn verify_fk_suspects(
    ctx: &mut ExecContext<'_>,
    suspects: &[(String, TableRowKey, usize)],
) -> Result<()> {
    for (ctbl_name, ckey, fk_id) in suspects {
        let Some(ctbl) = ctx.schema.find_user_table(ctbl_name).cloned() else {
            continue;
        };
        let Some(fk) = ctbl.foreign_keys.iter().find(|f| f.id == *fk_id).cloned() else {
            continue;
        };
        let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, ckey)? else {
            continue;
        };
        let cvals = decode_table_logical_row(ctx, &ctbl, ckey, &cpayload, &[])?;
        let (ptbl, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &ctbl.name, &fk)?;
        if !parent_row_exists_for_fk(ctx, &ptbl, &parent_col_idxs, &cvals, &fk.from_cols)? {
            return Err(fk_constraint_err());
        }
    }
    Ok(())
}

fn find_referencing_fks(
    schema: &Schema,
    parent_tbl_name: &str,
) -> Vec<(TableSchema, ForeignKeySchema)> {
    let mut out = Vec::new();
    for ctbl in &schema.tables {
        for fk in &ctbl.foreign_keys {
            if fk.ref_table.eq_ignore_ascii_case(parent_tbl_name) {
                out.push((ctbl.clone(), fk.clone()));
            }
        }
    }
    out
}

fn check_parent_delete_restrict(
    ctx: &mut ExecContext<'_>,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    params: &[Value],
) -> Result<()> {
    let refs = find_referencing_fks(ctx.schema, &ptbl.name);
    let enc = ctx.btree.encoding();
    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &ctbl.name, &fk)?;
        if fk.on_delete != FkAction::Restrict {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, &ctbl)?;
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                return Err(fk_constraint_err());
            }
        }
    }
    Ok(())
}

fn parent_key_changed(
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    old_vals: &[Value],
    new_vals: &[Value],
    enc: TextEncoding,
) -> bool {
    parent_col_idxs.iter().any(|&pi| {
        eval_comparison_with_encoding(
            Opcode::Is,
            &old_vals[pi],
            &new_vals[pi],
            ptbl.columns[pi].collation,
            enc,
        )
        .truthiness()
            != Some(true)
    })
}

fn check_parent_update_restrict(
    ctx: &mut ExecContext<'_>,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    new_parent_vals: &[Value],
    params: &[Value],
) -> Result<()> {
    let refs = find_referencing_fks(ctx.schema, &ptbl.name);
    let enc = ctx.btree.encoding();
    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &ctbl.name, &fk)?;
        if !parent_key_changed(
            ptbl,
            &parent_col_idxs,
            old_parent_vals,
            new_parent_vals,
            enc,
        ) {
            continue;
        }
        if fk.on_update != FkAction::Restrict {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, &ctbl)?;
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                return Err(fk_constraint_err());
            }
        }
    }
    Ok(())
}

fn apply_parent_delete_fk_actions(
    ctx: &mut ExecContext<'_>,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    params: &[Value],
    stmt_fk_suspects: &mut Vec<(String, TableRowKey, usize)>,
    fk_depth: usize,
) -> Result<()> {
    if fk_depth > MAX_FK_CASCADE_DEPTH {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "too many levels of trigger recursion",
        ));
    }
    let refs = find_referencing_fks(ctx.schema, &ptbl.name);
    let enc = ctx.btree.encoding();

    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &ctbl.name, &fk)?;
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, &ctbl)?;
        let mut matching_children = Vec::new();
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                matching_children.push((ckey, cvals));
            }
        }
        if matching_children.is_empty() {
            continue;
        }

        match fk.on_delete {
            FkAction::Restrict => return Err(fk_constraint_err()),
            FkAction::NoAction => {
                for (ckey, _) in matching_children {
                    if fk.deferred {
                        ctx.deferred_fk_suspects
                            .push((ctbl.name.clone(), ckey, fk.id));
                    } else {
                        stmt_fk_suspects.push((ctbl.name.clone(), ckey, fk.id));
                    }
                }
            }
            FkAction::Cascade => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    // Re-check child existence in case an earlier cascade already deleted it
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let cur_cvals = decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    check_parent_delete_restrict(ctx, &ctbl, &cur_cvals, params)?;
                    if remove_row_and_indexes(ctx, &ctbl, &cindexes, &ckey, params)? {
                        ctx.runtime.total_changes += 1;
                        apply_parent_delete_fk_actions(
                            ctx,
                            &ctbl,
                            &cur_cvals,
                            params,
                            stmt_fk_suspects,
                            fk_depth + 1,
                        )?;
                    }
                }
            }
            FkAction::SetNull => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    let new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        new_cvals[fcol] = Value::Null;
                        if ctbl.rowid_alias_col == Some(fcol) {
                            // INTEGER PRIMARY KEY column cannot hold NULL
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                    }
                    validate_and_write_row(
                        ctx,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
            FkAction::SetDefault => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        let dval = match &ctbl.columns[fcol].default_expr {
                            Some(de) => {
                                eval_expr(ctx, de, params, &[], &CteStore::default(), None)?
                            }
                            None => Value::Null,
                        };
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if dval.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = dval;
                    }
                    validate_and_write_row(
                        ctx,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn apply_parent_update_fk_actions(
    ctx: &mut ExecContext<'_>,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    new_parent_vals: &[Value],
    params: &[Value],
    stmt_fk_suspects: &mut Vec<(String, TableRowKey, usize)>,
    fk_depth: usize,
) -> Result<()> {
    if fk_depth > MAX_FK_CASCADE_DEPTH {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "too many levels of trigger recursion",
        ));
    }
    let refs = find_referencing_fks(ctx.schema, &ptbl.name);
    let enc = ctx.btree.encoding();

    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) = resolve_fk_parent_cols(ctx.schema, &ctbl.name, &fk)?;
        if !parent_key_changed(
            ptbl,
            &parent_col_idxs,
            old_parent_vals,
            new_parent_vals,
            enc,
        ) {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, &ctbl)?;
        let mut matching_children = Vec::new();
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                matching_children.push(ckey);
            }
        }
        if matching_children.is_empty() {
            continue;
        }

        match fk.on_update {
            FkAction::Restrict => return Err(fk_constraint_err()),
            FkAction::NoAction => {
                for ckey in matching_children {
                    if fk.deferred {
                        ctx.deferred_fk_suspects
                            .push((ctbl.name.clone(), ckey, fk.id));
                    } else {
                        stmt_fk_suspects.push((ctbl.name.clone(), ckey, fk.id));
                    }
                }
            }
            FkAction::Cascade => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for (&fcol, &pcol) in fk.from_cols.iter().zip(parent_col_idxs.iter()) {
                        let nv = new_parent_vals[pcol].clone();
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if nv.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = nv;
                    }
                    validate_and_write_row(
                        ctx,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
            FkAction::SetNull => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    let new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        if ctbl.rowid_alias_col == Some(fcol) {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                        new_cvals[fcol] = Value::Null;
                    }
                    validate_and_write_row(
                        ctx,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
            FkAction::SetDefault => {
                let cindexes = ctx.schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        let dval = match &ctbl.columns[fcol].default_expr {
                            Some(de) => {
                                eval_expr(ctx, de, params, &[], &CteStore::default(), None)?
                            }
                            None => Value::Null,
                        };
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if dval.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = dval;
                    }
                    validate_and_write_row(
                        ctx,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                    )?;
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// DML Executors: `INSERT`, `UPDATE`, `DELETE`
// ============================================================================

fn maybe_count_changes_output(ctx: &ExecContext<'_>, out: QueryOutput) -> QueryOutput {
    if *ctx.count_changes && out.column_names.is_empty() {
        QueryOutput {
            column_names: vec!["count".to_owned()],
            rows: vec![vec![Value::Integer(ctx.runtime.last_changes as i64)]],
        }
    } else {
        out
    }
}

fn resolve_dml_target_table<'a>(schema: &'a Schema, qn: &QualifiedName) -> Result<&'a TableSchema> {
    if let Some(ref s) = qn.schema {
        if s.eq_ignore_ascii_case("main") {
            if Schema::is_schema_table_name(&qn.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} may not be modified", qn.name),
                ));
            }
            if !Schema::is_temp_schema_table_name(&qn.name)
                && let Some(t) = schema.find_user_table(&qn.name)
            {
                return Ok(t);
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", qn.display_name()),
            ));
        }
        if s.eq_ignore_ascii_case("temp") && Schema::is_schema_table_name(&qn.name) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} may not be modified", qn.name),
            ));
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", qn.display_name()),
        ));
    }
    if Schema::is_schema_table_name(&qn.name) || Schema::is_temp_schema_table_name(&qn.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} may not be modified", qn.name),
        ));
    }
    schema
        .find_user_table(&qn.name)
        .ok_or_else(|| SqliteError::new(ErrorCode::Error, format!("no such table: {}", qn.name)))
}

fn validate_create_table_target(temp: bool, qn: &QualifiedName) -> Result<bool> {
    if let Some(ref s) = qn.schema {
        if !s.eq_ignore_ascii_case("main") && !s.eq_ignore_ascii_case("temp") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database {s}"),
            ));
        }
        if temp && !s.eq_ignore_ascii_case("temp") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "temporary table name must be unqualified",
            ));
        }
        return Ok(s.eq_ignore_ascii_case("temp"));
    }
    Ok(temp)
}

fn exec_insert(
    ctx: &mut ExecContext<'_>,
    ins: &InsertStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let tbl = resolve_dml_target_table(ctx.schema, &ins.table)?.clone();
    let indexes = ctx.schema.indexes_for_table(&tbl.name);
    let dml_ctes = CteStore::default().with_scope(&ins.ctes);

    // Resolve target column mapping (`None` = all non-generated columns in order; `Some(names)` = mapped columns, `usize::MAX` for implicit `rowid`)
    let col_targets: Option<Vec<usize>> = if let Some(ref cnames) = ins.columns {
        let mut idxs = Vec::with_capacity(cnames.len());
        for cn in cnames {
            if let Some((ci, col)) = tbl.find_column(cn) {
                if col.generated.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot INSERT into generated column \"{}\"", col.name),
                    ));
                }
                idxs.push(ci);
            } else if tbl.is_implicit_rowid_name(cn) {
                idxs.push(usize::MAX);
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} has no column named {cn}", tbl.name),
                ));
            }
        }
        Some(idxs)
    } else {
        None
    };
    let implicit_non_gen_cols: Vec<usize> = tbl
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.generated.is_none())
        .map(|(i, _)| i)
        .collect();

    // Materialize input value rows BEFORE mutating target table (handles `INSERT INTO t SELECT * FROM t` safely!)
    let source_value_rows: Vec<Vec<Value>> = match &ins.source {
        InsertSource::DefaultValues => vec![Vec::new()],
        InsertSource::Values(expr_rows) => {
            let mut out = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                let mut vrow = Vec::with_capacity(erow.len());
                for e in erow {
                    vrow.push(eval_expr(ctx, e, params, &[], &dml_ctes, None)?);
                }
                out.push(vrow);
            }
            out
        }
        InsertSource::Select(sel) => exec_select(ctx, sel, params, &[], &dml_ctes)?.rows,
    };

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if ins.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&ins.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();

    for src_row in source_value_rows {
        let mut explicit_rowid: Option<i64> = None;
        let mut col_vals = Vec::with_capacity(tbl.columns.len());
        for col in &tbl.columns {
            let def = match &col.default_expr {
                Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                None => Value::Null,
            };
            col_vals.push(def);
        }

        if matches!(ins.source, InsertSource::DefaultValues) {
            // Keep defaults
        } else if let Some(ref targets) = col_targets {
            if src_row.len() != targets.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{} values for {} columns", src_row.len(), targets.len()),
                ));
            }
            for (&t_idx, val) in targets.iter().zip(src_row) {
                if t_idx == usize::MAX {
                    if !val.is_null() {
                        let c = TypeAffinity::Integer.apply(val);
                        let rid = c.as_i64().ok_or_else(|| {
                            SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")
                        })?;
                        explicit_rowid = Some(rid);
                    }
                } else {
                    col_vals[t_idx] = val;
                }
            }
        } else {
            if src_row.len() != implicit_non_gen_cols.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        tbl.name,
                        implicit_non_gen_cols.len(),
                        src_row.len()
                    ),
                ));
            }
            for (&ci, val) in implicit_non_gen_cols.iter().zip(src_row) {
                col_vals[ci] = val;
            }
        }

        let write_res = validate_and_write_row(
            ctx,
            &tbl,
            &indexes,
            explicit_rowid,
            col_vals,
            ins.or_conflict,
            None,
            params,
            true,
            true,
            &mut stmt_fk_suspects,
            0,
        );
        let written_opt = match write_res {
            Ok(w) => w,
            Err(e) => {
                if e.conflict_effect == ConflictResolutionEffect::Fail
                    && *ctx.foreign_keys
                    && !stmt_fk_suspects.is_empty()
                {
                    verify_fk_suspects(ctx, &stmt_fk_suspects)?;
                }
                return Err(e);
            }
        };
        if let Some((written_key, written_vals)) = written_opt
            && !ret_expanded.is_empty()
        {
            let rscope =
                make_returning_table_scope(&tbl, written_key.rowid_or_zero(), &written_vals);
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    std::slice::from_ref(&rscope),
                    &dml_ctes,
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    Ok(maybe_count_changes_output(
        ctx,
        QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        },
    ))
}

fn exec_update(
    ctx: &mut ExecContext<'_>,
    upd: &UpdateStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let tbl = resolve_dml_target_table(ctx.schema, &upd.table)?.clone();
    let indexes = ctx.schema.indexes_for_table(&tbl.name);
    let dml_ctes = CteStore::default().with_scope(&upd.ctes);

    // Resolve assignment target columns (`usize::MAX` = implicit `rowid`)
    let mut resolved_assigns = Vec::with_capacity(upd.assignments.len());
    for (cname, expr) in &upd.assignments {
        if let Some((ci, col)) = tbl.find_column(cname) {
            if col.generated.is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot UPDATE generated column \"{}\"", col.name),
                ));
            }
            resolved_assigns.push((ci, expr));
        } else if tbl.is_implicit_rowid_name(cname) {
            resolved_assigns.push((usize::MAX, expr));
        } else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {cname}"),
            ));
        }
    }

    let candidates = read_table_with_plan(
        ctx,
        &tbl,
        upd.alias.as_deref(),
        upd.indexed_by.as_ref(),
        upd.selection.as_ref(),
        params,
        &[],
        &dml_ctes,
    )?;

    // Two-phase update: collect all matching `(old_key, new_rowid_opt, new_col_vals)` before mutating B-Trees!
    let mut pending_updates = Vec::new();
    for (old_key, payload) in candidates {
        let old_vals = decode_table_logical_row(ctx, &tbl, &old_key, &payload, params)?;
        let scope = make_single_table_scope(
            &tbl,
            upd.alias.as_deref(),
            old_key.rowid_or_zero(),
            &old_vals,
        );
        if let Some(ref w) = upd.selection {
            let cond = eval_expr(
                ctx,
                w,
                params,
                std::slice::from_ref(&scope),
                &dml_ctes,
                None,
            )?;
            if cond.truthiness() != Some(true) {
                continue;
            }
        }

        let mut new_vals = old_vals.clone();
        let mut new_rowid_opt = match &old_key {
            TableRowKey::Rowid(r) => Some(*r),
            TableRowKey::WithoutRowidPk(_) => None,
        };
        for &(target_idx, val_expr) in &resolved_assigns {
            let v = eval_expr(
                ctx,
                val_expr,
                params,
                std::slice::from_ref(&scope),
                &dml_ctes,
                None,
            )?;
            if target_idx == usize::MAX {
                if v.is_null() {
                    new_rowid_opt = None;
                } else {
                    let c = TypeAffinity::Integer.apply(v);
                    let rid = c.as_i64().ok_or_else(|| {
                        SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")
                    })?;
                    new_rowid_opt = Some(rid);
                }
            } else {
                new_vals[target_idx] = v;
                if tbl.rowid_alias_col == Some(target_idx) {
                    new_rowid_opt = None; // Let `validate_and_write_row` derive from `new_vals[ipk]`!
                }
            }
        }
        pending_updates.push((old_key, new_rowid_opt, new_vals));
    }

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if upd.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&upd.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();

    for (old_key, new_rowid_opt, new_vals) in pending_updates {
        let write_res = validate_and_write_row(
            ctx,
            &tbl,
            &indexes,
            new_rowid_opt,
            new_vals,
            upd.or_conflict,
            Some(&old_key),
            params,
            false,
            true,
            &mut stmt_fk_suspects,
            0,
        );
        let written_opt = match write_res {
            Ok(w) => w,
            Err(e) => {
                if e.conflict_effect == ConflictResolutionEffect::Fail
                    && *ctx.foreign_keys
                    && !stmt_fk_suspects.is_empty()
                {
                    verify_fk_suspects(ctx, &stmt_fk_suspects)?;
                }
                return Err(e);
            }
        };
        if let Some((written_key, written_vals)) = written_opt
            && !ret_expanded.is_empty()
        {
            let rscope =
                make_returning_table_scope(&tbl, written_key.rowid_or_zero(), &written_vals);
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    std::slice::from_ref(&rscope),
                    &dml_ctes,
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    Ok(maybe_count_changes_output(
        ctx,
        QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        },
    ))
}

fn exec_delete(
    ctx: &mut ExecContext<'_>,
    del: &DeleteStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let tbl = resolve_dml_target_table(ctx.schema, &del.table)?.clone();
    let indexes = ctx.schema.indexes_for_table(&tbl.name);
    let dml_ctes = CteStore::default().with_scope(&del.ctes);

    // Fast path (`OP_Clear`) for unconditional `DELETE FROM tbl` without `RETURNING` when no FKs reference `tbl`!
    if del.selection.is_none()
        && del.returning.is_empty()
        && !*ctx.foreign_keys
        && del.indexed_by.is_none()
    {
        let count = if tbl.without_rowid {
            ctx.scan_index(tbl.root_pgno, tbl.without_rowid_pk_orders())?
                .len() as u64
        } else {
            ctx.scan_table(tbl.root_pgno)?.len() as u64
        };
        ctx.btree.clear_tree(tbl.root_pgno)?;
        for idx in &indexes {
            ctx.btree.clear_tree(idx.root_pgno)?;
        }
        ctx.runtime.last_changes = count;
        ctx.runtime.total_changes += count;
        ctx.publish();
        return Ok(maybe_count_changes_output(ctx, QueryOutput::default()));
    }

    let candidates = read_table_with_plan(
        ctx,
        &tbl,
        del.alias.as_deref(),
        del.indexed_by.as_ref(),
        del.selection.as_ref(),
        params,
        &[],
        &dml_ctes,
    )?;

    let mut targets = Vec::new();
    for (rkey, payload) in candidates {
        let vals = decode_table_logical_row(ctx, &tbl, &rkey, &payload, params)?;
        let scope =
            make_single_table_scope(&tbl, del.alias.as_deref(), rkey.rowid_or_zero(), &vals);
        if let Some(ref w) = del.selection {
            let cond = eval_expr(
                ctx,
                w,
                params,
                std::slice::from_ref(&scope),
                &dml_ctes,
                None,
            )?;
            if cond.truthiness() != Some(true) {
                continue;
            }
        }
        targets.push((rkey, vals, scope));
    }

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if del.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&del.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();

    for (rkey, old_vals, _where_scope) in targets {
        if *ctx.foreign_keys {
            // Re-check row existence in case a self-referential ON DELETE CASCADE already removed it
            if seek_table_raw_entry(ctx, &tbl, &rkey)?.is_none() {
                continue;
            }
            check_parent_delete_restrict(ctx, &tbl, &old_vals, params)?;
        }
        if remove_row_and_indexes(ctx, &tbl, &indexes, &rkey, params)? {
            ctx.runtime.last_changes += 1;
            ctx.runtime.total_changes += 1;
            ctx.publish();
            if *ctx.foreign_keys {
                apply_parent_delete_fk_actions(
                    ctx,
                    &tbl,
                    &old_vals,
                    params,
                    &mut stmt_fk_suspects,
                    0,
                )?;
            }
            if !ret_expanded.is_empty() {
                let rscope = make_returning_table_scope(&tbl, rkey.rowid_or_zero(), &old_vals);
                let mut rrow = Vec::with_capacity(ret_expanded.len());
                for (pe, _, _) in &ret_expanded {
                    rrow.push(eval_expr(
                        ctx,
                        pe,
                        params,
                        std::slice::from_ref(&rscope),
                        &dml_ctes,
                        None,
                    )?);
                }
                ret_rows.push(rrow);
            }
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    Ok(maybe_count_changes_output(
        ctx,
        QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        },
    ))
}

// ============================================================================
// DDL Executors: `CREATE/DROP TABLE`, `CREATE/DROP INDEX`, `ALTER TABLE`
// ============================================================================

fn is_reserved_sqlite_name(name: &str) -> bool {
    name.as_bytes()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sqlite_"))
}

fn validate_drop_or_alter_schema_table(name: &QualifiedName, verb: &str) -> Result<()> {
    let schema_is_main_or_temp = match name.schema.as_deref() {
        None => true,
        Some(s) => s.eq_ignore_ascii_case("main") || s.eq_ignore_ascii_case("temp"),
    };
    if schema_is_main_or_temp {
        let is_sys = match name.schema.as_deref() {
            None => {
                Schema::is_schema_table_name(&name.name)
                    || Schema::is_temp_schema_table_name(&name.name)
            }
            Some(s) if s.eq_ignore_ascii_case("temp") => {
                Schema::is_schema_table_name(&name.name)
                    || Schema::is_temp_schema_table_name(&name.name)
            }
            Some(_) => Schema::is_schema_table_name(&name.name),
        };
        if is_sys {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} may not be {verb}", name.name),
            ));
        }
    }
    Ok(())
}

fn exec_create_table(ctx: &mut ExecContext<'_>, ct: &CreateTableStmt) -> Result<()> {
    let is_temp = validate_create_table_target(ct.temp, &ct.name)?;
    if is_reserved_sqlite_name(&ct.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ct.name.name),
        ));
    }
    if is_temp {
        let _ = build_table_schema_from_ast(ct, 2)?;
        return Err(SqliteError::new(
            ErrorCode::Error,
            "TEMP tables are not yet supported before DatabaseSet (Step 2-C)",
        ));
    }
    if ctx.schema.find_user_table(&ct.name.name).is_some()
        || ctx.schema.find_index(&ct.name.name).is_some()
    {
        if ct.if_not_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} already exists", ct.name.name),
        ));
    }

    // Validate AST & inline autoindexes before allocating pages
    let (tbl_preview, pending_autos) = build_table_schema_from_ast(ct, 2)?;
    let tbl_root = if tbl_preview.without_rowid {
        ctx.btree.create_index()?
    } else {
        ctx.btree.create_table()?
    };
    insert_schema_entry(
        ctx.btree,
        "table",
        &ct.name.name,
        &ct.name.name,
        tbl_root,
        Some(&ct.catalog_sql),
    )?;

    for auto in pending_autos {
        let idx_root = ctx.btree.create_index()?;
        insert_schema_entry(
            ctx.btree,
            "index",
            &auto.name,
            &ct.name.name,
            idx_root,
            None,
        )?;
    }

    // Create `sqlite_sequence` table if `AUTOINCREMENT` is used and `sqlite_sequence` doesn't exist yet
    if tbl_preview.autoincrement && ctx.schema.find_user_table("sqlite_sequence").is_none() {
        let seq_root = ctx.btree.create_table()?;
        insert_schema_entry(
            ctx.btree,
            "table",
            "sqlite_sequence",
            "sqlite_sequence",
            seq_root,
            Some("CREATE TABLE sqlite_sequence(name,seq)"),
        )?;
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    *ctx.schema_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_create_table_as(
    ctx: &mut ExecContext<'_>,
    temp: bool,
    if_not_exists: bool,
    name: &QualifiedName,
    query: &SelectStmt,
    params: &[Value],
) -> Result<()> {
    let is_temp = validate_create_table_target(temp, name)?;
    if is_reserved_sqlite_name(&name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", name.name),
        ));
    }
    if is_temp {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "TEMP tables are not yet supported before DatabaseSet (Step 2-C)",
        ));
    }
    if ctx.schema.find_user_table(&name.name).is_some()
        || ctx.schema.find_index(&name.name).is_some()
    {
        if if_not_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} already exists", name.name),
        ));
    }

    let meta = infer_select_stmt_meta(query, &[], ctx.schema, &CteStore::default());
    let q_out = exec_select(ctx, query, params, &[], &CteStore::default())?;

    let mut dedup_names: Vec<String> = Vec::with_capacity(q_out.column_names.len());
    for raw_cn in &q_out.column_names {
        if !dedup_names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(raw_cn))
        {
            dedup_names.push(raw_cn.clone());
        } else {
            let mut k = 1usize;
            loop {
                let candidate = format!("{raw_cn}:{k}");
                if !dedup_names
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&candidate))
                {
                    dedup_names.push(candidate);
                    break;
                }
                k += 1;
            }
        }
    }

    let mut columns = Vec::with_capacity(dedup_names.len());
    let mut rendered_cols = Vec::with_capacity(dedup_names.len());
    for (i, cname) in dedup_names.into_iter().enumerate() {
        let aff = meta.get(i).and_then(|(a, _)| *a);
        let type_str = match aff {
            Some(TypeAffinity::Integer) => "INT",
            Some(TypeAffinity::Text) => "TEXT",
            Some(TypeAffinity::Real) => "REAL",
            Some(TypeAffinity::Numeric) => "NUM",
            Some(TypeAffinity::Blob) | None => "",
        };
        let cdef = ColumnDef {
            name: cname,
            type_name: type_str.to_owned(),
            constraints: Vec::new(),
        };
        rendered_cols.push(render_column_def_sql(&cdef));
        columns.push(cdef);
    }

    let catalog_sql = format!(
        "CREATE TABLE {}({})",
        format_replacement_ident("", &name.name),
        rendered_cols.join(",")
    );
    let ct = CreateTableStmt {
        temp: false,
        if_not_exists: false,
        name: QualifiedName::unqualified(name.name.clone()),
        columns,
        constraints: Vec::new(),
        without_rowid: false,
        strict: false,
        catalog_sql: catalog_sql.clone(),
        raw_sql: catalog_sql,
    };
    exec_create_table(ctx, &ct)?;

    let Some(tbl) = ctx.schema.find_user_table(&name.name).cloned() else {
        return Ok(());
    };
    let mut stmt_fk_suspects = Vec::new();
    for row in q_out.rows {
        validate_and_write_row(
            ctx,
            &tbl,
            &[],
            None,
            row,
            None,
            None,
            params,
            false,
            false,
            &mut stmt_fk_suspects,
            0,
        )?;
    }
    Ok(())
}

fn apply_root_page_move(btree: &mut Btree, schema: &mut Schema, mv: RootPageMove) -> Result<()> {
    if let Some(t) = schema.tables.iter_mut().find(|t| t.root_pgno == mv.from) {
        update_schema_entry(
            btree,
            t.schema_rowid,
            "table",
            &t.name,
            &t.name,
            mv.to,
            Some(&t.raw_sql),
        )?;
        t.root_pgno = mv.to;
        return Ok(());
    }
    if let Some(idx) = schema
        .indexes
        .iter_mut()
        .find(|idx| idx.root_pgno == mv.from)
    {
        update_schema_entry(
            btree,
            idx.schema_rowid,
            "index",
            &idx.name,
            &idx.table_name,
            mv.to,
            idx.raw_sql.as_deref(),
        )?;
        idx.root_pgno = mv.to;
        return Ok(());
    }
    Err(SqliteError::corrupt(format!(
        "relocated root page {} not found in schema catalog",
        mv.from
    )))
}

fn exec_drop_table(ctx: &mut ExecContext<'_>, if_exists: bool, name: &QualifiedName) -> Result<()> {
    validate_drop_or_alter_schema_table(name, "dropped")?;
    if name
        .schema
        .as_deref()
        .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
    {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    }
    let Some(tbl) = ctx.schema.find_user_table(&name.name).cloned() else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    };
    if ctx.activity.other_readers > 0 || ctx.btree.has_any_other_open_cursor(None) {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }
    // Per SQLite `sqlite3FkDropTable`, when `foreign_keys = ON`, delete rows first so FK constraints / actions fire!
    if *ctx.foreign_keys {
        let existing_rows = scan_table_raw_entries(ctx, &tbl)?;
        if !existing_rows.is_empty() {
            let indexes = ctx.schema.indexes_for_table(&tbl.name);
            let mut stmt_fk_suspects = Vec::new();
            for (rkey, payload) in existing_rows {
                if seek_table_raw_entry(ctx, &tbl, &rkey)?.is_none() {
                    continue;
                }
                let old_vals = decode_table_logical_row(ctx, &tbl, &rkey, &payload, &[])?;
                check_parent_delete_restrict(ctx, &tbl, &old_vals, &[])?;
                if remove_row_and_indexes(ctx, &tbl, &indexes, &rkey, &[])? {
                    apply_parent_delete_fk_actions(
                        ctx,
                        &tbl,
                        &old_vals,
                        &[],
                        &mut stmt_fk_suspects,
                        0,
                    )?;
                }
            }
            if !stmt_fk_suspects.is_empty() {
                verify_fk_suspects(ctx, &stmt_fk_suspects)?;
            }
        }
    }

    enum DropRootKind {
        Table,
        Index(String),
    }

    let indexes = ctx.schema.indexes_for_table(&tbl.name);
    let mut roots: Vec<(Pgno, i64, DropRootKind)> = Vec::with_capacity(1 + indexes.len());
    roots.push((tbl.root_pgno, tbl.schema_rowid, DropRootKind::Table));
    for idx in indexes {
        if idx.root_pgno != tbl.root_pgno {
            roots.push((
                idx.root_pgno,
                idx.schema_rowid,
                DropRootKind::Index(idx.name),
            ));
        } else {
            if idx.schema_rowid != 0 {
                delete_schema_entry(ctx.btree, idx.schema_rowid)?;
            }
            ctx.schema
                .indexes
                .retain(|i| !i.name.eq_ignore_ascii_case(&idx.name));
        }
    }
    // Drop roots in strictly descending `root_pgno` order (`sqlite/src/build.c:destroyTable`)!
    roots.sort_by_key(|a| std::cmp::Reverse(a.0));

    for (root_pgno, schema_rowid, kind) in roots {
        let mv = ctx.btree.drop_tree_relocating(root_pgno)?;
        if schema_rowid != 0 {
            delete_schema_entry(ctx.btree, schema_rowid)?;
        }
        match kind {
            DropRootKind::Table => {
                ctx.schema
                    .tables
                    .retain(|t| !t.name.eq_ignore_ascii_case(&tbl.name));
            }
            DropRootKind::Index(ref idx_name) => {
                ctx.schema
                    .indexes
                    .retain(|i| !i.name.eq_ignore_ascii_case(idx_name));
            }
        }
        if let Some(mv) = mv {
            apply_root_page_move(ctx.btree, ctx.schema, mv)?;
        }
    }

    // Also delete entry from `sqlite_sequence` if present (using its possibly-relocated `root_pgno`!)
    if let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() {
        let enc = ctx.btree.encoding();
        let rows = scan_table_incremental(ctx.btree, seq_tbl.root_pgno)?;
        for (rid, payload) in rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.first().and_then(Value::as_str) == Some(tbl.name.as_str()) {
                ctx.btree.delete_table(seq_tbl.root_pgno, rid)?;
            }
        }
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    *ctx.schema_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_create_index(ctx: &mut ExecContext<'_>, ci: &CreateIndexStmt) -> Result<()> {
    if let Some(ref s) = ci.name.schema
        && !s.eq_ignore_ascii_case("main")
        && !s.eq_ignore_ascii_case("temp")
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown database {s}"),
        ));
    }
    if is_reserved_sqlite_name(&ci.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ci.name.name),
        ));
    }
    if ci
        .name
        .schema
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
    {
        if ctx.schema.find_user_table(&ci.table_name).is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "cannot create a TEMP index on non-TEMP table \"{}\"",
                    ci.table_name
                ),
            ));
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: temp.{}", ci.table_name),
        ));
    }
    if ctx.schema.find_index(&ci.name.name).is_some()
        || ctx.schema.find_user_table(&ci.name.name).is_some()
    {
        if ci.if_not_exists {
            if ctx.schema.find_user_table(&ci.table_name).is_none() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ci.table_name),
                ));
            }
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("index {} already exists", ci.name.name),
        ));
    }
    let Some(tbl) = ctx.schema.find_user_table(&ci.table_name).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", ci.table_name),
        ));
    };

    let idx_root = ctx.btree.create_index()?;
    let idx_schema = build_index_schema_from_ast(ci, &tbl, idx_root)?;
    populate_index_btree(ctx, &tbl, &idx_schema)?;

    insert_schema_entry(
        ctx.btree,
        "index",
        &ci.name.name,
        &tbl.name,
        idx_root,
        Some(&ci.catalog_sql),
    )?;
    *ctx.schema = Schema::load(ctx.btree)?;
    *ctx.schema_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn populate_index_btree(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    idx_schema: &IndexSchema,
) -> Result<()> {
    let orders = idx_schema.key_sort_orders();
    let existing_rows = scan_table_raw_entries(ctx, tbl)?;
    for (rkey, payload) in existing_rows {
        let vals = decode_table_logical_row(ctx, tbl, &rkey, &payload, &[])?;
        let rowid = rkey.rowid_or_zero();
        if !index_row_passes_partial_where(ctx, tbl, idx_schema, rowid, &vals, &[])? {
            continue;
        }
        let indexed_only = eval_index_column_values(ctx, tbl, idx_schema, rowid, &vals, &[])?;
        if idx_schema.unique && indexed_only.iter().all(|v| !v.is_null()) {
            let hits = ctx.btree.seek_index_equal(
                idx_schema.root_pgno,
                &indexed_only,
                &orders[..indexed_only.len()],
            )?;
            if !hits.is_empty() {
                let ext_code = if idx_schema.origin == "pk" {
                    extended::SQLITE_CONSTRAINT_PRIMARYKEY
                } else {
                    extended::SQLITE_CONSTRAINT_UNIQUE
                };
                return Err(SqliteError::constraint(
                    ext_code,
                    format_unique_index_failure(tbl, idx_schema),
                ));
            }
        }
        let keys = eval_index_key_values(ctx, tbl, idx_schema, &rkey, &vals, &[])?;
        let idx_rec = encode_record_with_encoding(&keys, ctx.btree.encoding());
        ctx.btree
            .insert_index(idx_schema.root_pgno, &idx_rec, &orders)?;
    }
    Ok(())
}

fn exec_reindex(ctx: &mut ExecContext<'_>, target: Option<&QualifiedName>) -> Result<()> {
    let mut primary_without_rowid_tables: Vec<TableSchema> = Vec::new();
    let targets: Vec<IndexSchema> = match target {
        None => {
            primary_without_rowid_tables = ctx
                .schema
                .tables
                .iter()
                .filter(|t| t.without_rowid)
                .cloned()
                .collect();
            ctx.schema.indexes.clone()
        }
        Some(qn) => {
            if let Some(ref s) = qn.schema {
                if !s.eq_ignore_ascii_case("main") && !s.eq_ignore_ascii_case("temp") {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("unknown database {s}"),
                    ));
                }
                if s.eq_ignore_ascii_case("main") {
                    if let Some(tbl) = ctx.schema.find_user_table(&qn.name).cloned() {
                        if tbl.without_rowid {
                            primary_without_rowid_tables.push(tbl);
                        }
                        ctx.schema.indexes_for_table(&qn.name)
                    } else if let Some(idx) = ctx.schema.find_index(&qn.name).cloned() {
                        vec![idx]
                    } else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "unable to identify the object to be reindexed",
                        ));
                    }
                } else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unable to identify the object to be reindexed",
                    ));
                }
            } else if let Some(coll) = Collation::from_name(&qn.name) {
                primary_without_rowid_tables = ctx
                    .schema
                    .tables
                    .iter()
                    .filter(|t| {
                        t.without_rowid
                            && t.without_rowid_pk_cols
                                .iter()
                                .any(|c| c.sort_order.collation == coll)
                    })
                    .cloned()
                    .collect();
                ctx.schema
                    .indexes
                    .iter()
                    .filter(|idx| idx.columns.iter().any(|c| c.sort_order.collation == coll))
                    .cloned()
                    .collect()
            } else if ctx.collations.has_collation(&qn.name) {
                Vec::new()
            } else if let Some(tbl) = ctx.schema.find_user_table(&qn.name).cloned() {
                if tbl.without_rowid {
                    primary_without_rowid_tables.push(tbl);
                }
                ctx.schema.indexes_for_table(&qn.name)
            } else if let Some(idx) = ctx.schema.find_index(&qn.name).cloned() {
                vec![idx]
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "unable to identify the object to be reindexed",
                ));
            }
        }
    };

    for tbl in primary_without_rowid_tables {
        let pk_orders = tbl.without_rowid_pk_orders();
        let entries = ctx.scan_index(tbl.root_pgno, pk_orders.clone())?;
        ctx.btree.clear_tree(tbl.root_pgno)?;
        for rec in entries {
            ctx.btree.insert_index(tbl.root_pgno, &rec, &pk_orders)?;
        }
    }

    for idx in targets {
        let Some(tbl) = ctx.schema.find_user_table(&idx.table_name).cloned() else {
            continue;
        };
        ctx.btree.clear_tree(idx.root_pgno)?;
        populate_index_btree(ctx, &tbl, &idx)?;
    }
    Ok(())
}

fn exec_drop_index(ctx: &mut ExecContext<'_>, if_exists: bool, name: &QualifiedName) -> Result<()> {
    if name
        .schema
        .as_deref()
        .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
    {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such index: {}", name.display_name()),
        ));
    }
    let Some(idx) = ctx.schema.find_index_including_without_rowid_pk(&name.name) else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such index: {}", name.display_name()),
        ));
    };
    if idx.is_autoindex {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped",
        ));
    }
    if ctx.activity.other_readers > 0 || ctx.btree.has_any_other_open_cursor(None) {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }
    let mv = ctx.btree.drop_tree_relocating(idx.root_pgno)?;
    delete_schema_entry(ctx.btree, idx.schema_rowid)?;
    ctx.schema
        .indexes
        .retain(|i| !i.name.eq_ignore_ascii_case(&idx.name));
    if let Some(mv) = mv {
        apply_root_page_move(ctx.btree, ctx.schema, mv)?;
    }
    *ctx.schema = Schema::load(ctx.btree)?;
    *ctx.schema_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_alter_table(
    ctx: &mut ExecContext<'_>,
    table: &QualifiedName,
    action: &AlterTableAction,
) -> Result<()> {
    validate_drop_or_alter_schema_table(table, "altered")?;
    if table
        .schema
        .as_deref()
        .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", table.display_name()),
        ));
    }
    let Some(tbl) = ctx.schema.find_user_table(&table.name).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", table.display_name()),
        ));
    };

    match action {
        AlterTableAction::RenameTo(new_name) => {
            if is_reserved_sqlite_name(new_name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {new_name}"),
                ));
            }
            if ctx.schema.find_user_table(new_name).is_some()
                || ctx.schema.find_index(new_name).is_some()
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("there is already another table or index with this name: {new_name}"),
                ));
            }
            let new_sql =
                rewrite_table_name_in_create_table_sql(&tbl.raw_sql, &tbl.name, new_name)?;
            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                new_name,
                new_name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
            let indexes = ctx.schema.indexes_for_table(&tbl.name);
            let mut auto_n = 0usize;
            for idx in indexes {
                if idx.is_autoindex {
                    auto_n += 1;
                    let suffix_num = idx
                        .name
                        .rsplit('_')
                        .next()
                        .and_then(|s| s.parse::<usize>().ok())
                        .unwrap_or(auto_n);
                    let new_auto = format!("sqlite_autoindex_{new_name}_{suffix_num}");
                    update_schema_entry(
                        ctx.btree,
                        idx.schema_rowid,
                        "index",
                        &new_auto,
                        new_name,
                        idx.root_pgno,
                        None,
                    )?;
                } else {
                    let updated_idx_sql = match idx.raw_sql.as_deref() {
                        Some(s) => Some(rewrite_table_name_in_create_index_sql(
                            s, &tbl.name, new_name,
                        )?),
                        None => None,
                    };
                    update_schema_entry(
                        ctx.btree,
                        idx.schema_rowid,
                        "index",
                        &idx.name,
                        new_name,
                        idx.root_pgno,
                        updated_idx_sql.as_deref(),
                    )?;
                }
            }
            // Rewrite `REFERENCES <old_tbl>` in other user tables
            let other_tables: Vec<TableSchema> = ctx
                .schema
                .tables
                .iter()
                .filter(|t| !t.name.eq_ignore_ascii_case(&tbl.name))
                .cloned()
                .collect();
            for otbl in other_tables {
                if let Some(updated_other_sql) =
                    rewrite_fk_ref_table_in_create_table_sql(&otbl.raw_sql, &tbl.name, new_name)?
                {
                    update_schema_entry(
                        ctx.btree,
                        otbl.schema_rowid,
                        "table",
                        &otbl.name,
                        &otbl.name,
                        otbl.root_pgno,
                        Some(&updated_other_sql),
                    )?;
                }
            }
            // Update `sqlite_sequence` entry if present
            if let Some(seq_tbl) = ctx.schema.find_user_table("sqlite_sequence").cloned() {
                let enc = ctx.btree.encoding();
                let rows = scan_table_incremental(ctx.btree, seq_tbl.root_pgno)?;
                for (rid, payload) in rows {
                    let cols = decode_record_with_encoding(&payload, enc)?;
                    if cols.len() >= 2 && cols[0].as_str() == Some(tbl.name.as_str()) {
                        let rec = encode_record_with_encoding(
                            &[Value::Text(new_name.clone()), cols[1].clone()],
                            enc,
                        );
                        ctx.btree.insert_table(seq_tbl.root_pgno, rid, &rec)?;
                    }
                }
            }
        }
        AlterTableAction::RenameColumn { old_name, new_name } => {
            if tbl.find_column(old_name).is_none() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: \"{old_name}\""),
                ));
            }
            if !old_name.eq_ignore_ascii_case(new_name) && tbl.find_column(new_name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {new_name}"),
                ));
            }

            let new_sql =
                rewrite_column_in_create_table_sql(&tbl.raw_sql, &tbl.name, old_name, new_name)?;
            // Validate rewritten CREATE TABLE parses cleanly before committing schema updates
            let (Some(Stmt::CreateTable(_)), _) = parse_one(&new_sql)? else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {} after rename column", tbl.name),
                ));
            };
            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                &tbl.name,
                &tbl.name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;

            // Rewrite all explicit indexes (`CREATE [UNIQUE] INDEX ... ON tbl(...) [WHERE ...]`) on `tbl`
            let indexes = ctx.schema.indexes_for_table(&tbl.name);
            for idx in indexes {
                if idx.is_autoindex {
                    continue;
                }
                if let Some(ref idx_sql) = idx.raw_sql {
                    let new_idx_sql =
                        rewrite_column_in_create_index_sql(idx_sql, &tbl.name, old_name, new_name)?;
                    if new_idx_sql != *idx_sql {
                        let (Some(Stmt::CreateIndex(_)), _) = parse_one(&new_idx_sql)? else {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("error in index {} after rename column", idx.name),
                            ));
                        };
                        update_schema_entry(
                            ctx.btree,
                            idx.schema_rowid,
                            "index",
                            &idx.name,
                            &tbl.name,
                            idx.root_pgno,
                            Some(&new_idx_sql),
                        )?;
                    }
                }
            }

            // Rewrite `REFERENCES tbl(old_name)` in child tables
            let other_tables: Vec<TableSchema> = ctx
                .schema
                .tables
                .iter()
                .filter(|t| !t.name.eq_ignore_ascii_case(&tbl.name))
                .cloned()
                .collect();
            for otbl in other_tables {
                if let Some(updated_child_sql) = rewrite_fk_ref_column_in_create_table_sql(
                    &otbl.raw_sql,
                    &tbl.name,
                    old_name,
                    new_name,
                )? {
                    update_schema_entry(
                        ctx.btree,
                        otbl.schema_rowid,
                        "table",
                        &otbl.name,
                        &otbl.name,
                        otbl.root_pgno,
                        Some(&updated_child_sql),
                    )?;
                }
            }
        }
        AlterTableAction::AddColumn(cdef) => {
            if tbl.find_column(&cdef.name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {}", cdef.name),
                ));
            }
            let validated = build_column_schema_from_def(&tbl.name, tbl.strict, cdef)?;
            if validated
                .generated
                .as_ref()
                .is_some_and(|(_, stored)| *stored)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot add a STORED column",
                ));
            }
            let has_fk = cdef
                .constraints
                .iter()
                .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::ForeignKey { .. }));
            if has_fk
                && *ctx.foreign_keys
                && validated
                    .default_expr
                    .as_ref()
                    .is_some_and(|e| !matches!(e, Expr::Null))
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "Cannot add a REFERENCES column with non-NULL default value",
                ));
            }
            let rendered_col = render_column_def_sql(cdef);
            let new_sql = append_column_to_create_table_sql(&tbl.raw_sql, &rendered_col)?;
            let (Some(Stmt::CreateTable(new_ct)), _) = parse_one(&new_sql)? else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {} after add column", tbl.name),
                ));
            };
            let (mut new_tbl, _) = build_table_schema_from_ast(&new_ct, tbl.root_pgno)?;
            new_tbl.name.clone_from(&tbl.name);

            let has_new_check = cdef
                .constraints
                .iter()
                .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::Check { .. }));
            let new_col_idx = new_tbl.columns.len() - 1;
            if has_new_check
                || validated.not_null.is_some()
                || new_tbl.strict
                || validated.generated.is_some()
            {
                let existing_rows = scan_table_raw_entries(ctx, &tbl)?;
                for (rkey, payload) in existing_rows {
                    let vals = decode_table_logical_row(ctx, &new_tbl, &rkey, &payload, &[])?;
                    let new_col = &new_tbl.columns[new_col_idx];
                    if new_tbl.strict {
                        check_strict_column_type(&new_tbl.name, new_col, &vals[new_col_idx])?;
                    }
                    if new_col.not_null.is_some() && vals[new_col_idx].is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!(
                                "NOT NULL constraint failed: {}.{}",
                                new_tbl.name, new_col.name
                            ),
                        ));
                    }
                    if has_new_check {
                        let scope =
                            make_single_table_scope(&new_tbl, None, rkey.rowid_or_zero(), &vals);
                        for chk in &new_tbl.checks[tbl.checks.len()..] {
                            let cv = eval_expr(
                                ctx,
                                &chk.expr,
                                &[],
                                std::slice::from_ref(&scope),
                                &CteStore::default(),
                                None,
                            )?;
                            if cv.truthiness() == Some(false) {
                                return Err(SqliteError::constraint(
                                    extended::SQLITE_CONSTRAINT_CHECK,
                                    format!("CHECK constraint failed: {}", chk.label),
                                ));
                            }
                        }
                    }
                }
            }

            update_schema_entry(
                ctx.btree,
                tbl.schema_rowid,
                "table",
                &tbl.name,
                &tbl.name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
        }
    }

    *ctx.schema = Schema::load(ctx.btree)?;
    *ctx.schema_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

// ============================================================================
// Token-Aware SQL Rewriting for `ALTER TABLE` (`sqlite/src/alter.c`)
// ============================================================================

fn is_ident_like_token(kind: TokenKind) -> bool {
    match kind {
        TokenKind::Id | TokenKind::QuotedId => true,
        TokenKind::Semi
        | TokenKind::Lp
        | TokenKind::Rp
        | TokenKind::Comma
        | TokenKind::Dot
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Rem
        | TokenKind::Eq
        | TokenKind::Ne
        | TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Gt
        | TokenKind::Ge
        | TokenKind::BitAnd
        | TokenKind::BitOr
        | TokenKind::BitNot
        | TokenKind::LShift
        | TokenKind::RShift
        | TokenKind::Concat
        | TokenKind::Ptr
        | TokenKind::PtrText
        | TokenKind::StringLiteral
        | TokenKind::BlobLiteral
        | TokenKind::Integer
        | TokenKind::Float
        | TokenKind::Variable
        | TokenKind::Eof => false,
        _ => true, // SQLite keywords allowed in identifier positions
    }
}

fn token_ident_name(tok: &Token<'_>) -> String {
    if tok.kind == TokenKind::QuotedId {
        dequote(tok.text)
    } else {
        tok.text.to_owned()
    }
}

fn token_matches_ident(tok: &Token<'_>, target: &str) -> bool {
    is_ident_like_token(tok.kind) && token_ident_name(tok).eq_ignore_ascii_case(target)
}

fn format_replacement_ident(orig_tok_text: &str, new_name: &str) -> String {
    let was_quoted = orig_tok_text.starts_with('"')
        || orig_tok_text.starts_with('`')
        || orig_tok_text.starts_with('[');
    let needs_quotes = was_quoted
        || new_name.is_empty()
        || keyword_kind(new_name).is_some()
        || new_name.as_bytes()[0].is_ascii_digit()
        || !new_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if needs_quotes {
        format!("\"{}\"", new_name.replace('"', "\"\""))
    } else {
        new_name.to_owned()
    }
}

fn apply_token_replacements(sql: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    if edits.is_empty() {
        return sql.to_owned();
    }
    edits.sort_by_key(|&(start, _, _)| start);
    let mut out = String::with_capacity(sql.len() + 16);
    let mut cursor = 0usize;
    for (start, len, rep) in edits {
        if start >= cursor {
            out.push_str(&sql[cursor..start]);
            out.push_str(&rep);
            cursor = start + len;
        }
    }
    out.push_str(&sql[cursor..]);
    out
}

fn rewrite_table_name_in_create_table_sql(
    sql: &str,
    old_tbl: &str,
    new_tbl: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() && tokens[i].kind != TokenKind::Table {
        i += 1;
    }
    if i < tokens.len() {
        i += 1; // past TABLE
        if i + 2 < tokens.len()
            && tokens[i].kind == TokenKind::If
            && tokens[i + 1].kind == TokenKind::Not
            && tokens[i + 2].kind == TokenKind::Exists
        {
            i += 3;
        }
        // Optional `main.`
        if i + 2 < tokens.len() && tokens[i + 1].kind == TokenKind::Dot {
            i += 2;
        }
        if i < tokens.len() && token_matches_ident(&tokens[i], old_tbl) {
            edits.push((
                tokens[i].offset,
                tokens[i].text.len(),
                format_replacement_ident(tokens[i].text, new_tbl),
            ));
        }
    }
    // Also rewrite self-referencing `REFERENCES <old_tbl>` inside the same table
    for idx in 0..tokens.len() {
        if tokens[idx].kind == TokenKind::References
            && idx + 1 < tokens.len()
            && token_matches_ident(&tokens[idx + 1], old_tbl)
        {
            let t = &tokens[idx + 1];
            edits.push((
                t.offset,
                t.text.len(),
                format_replacement_ident(t.text, new_tbl),
            ));
        }
    }
    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_table_name_in_create_index_sql(
    sql: &str,
    old_tbl: &str,
    new_tbl: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    for i in 0..tokens.len() {
        if tokens[i].kind == TokenKind::On && i + 1 < tokens.len() {
            let t = &tokens[i + 1];
            if token_matches_ident(t, old_tbl) {
                edits.push((
                    t.offset,
                    t.text.len(),
                    format_replacement_ident(t.text, new_tbl),
                ));
            }
            break;
        }
    }
    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_fk_ref_table_in_create_table_sql(
    sql: &str,
    old_tbl: &str,
    new_tbl: &str,
) -> Result<Option<String>> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    for i in 0..tokens.len() {
        if tokens[i].kind == TokenKind::References
            && i + 1 < tokens.len()
            && token_matches_ident(&tokens[i + 1], old_tbl)
        {
            let t = &tokens[i + 1];
            edits.push((
                t.offset,
                t.text.len(),
                format_replacement_ident(t.text, new_tbl),
            ));
        }
    }
    if edits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(apply_token_replacements(sql, edits)))
    }
}

fn collect_expr_column_edits(
    tokens: &[Token<'_>],
    start: usize,
    end: usize,
    self_tbl: &str,
    old_col: &str,
    new_col: &str,
    edits: &mut Vec<(usize, usize, String)>,
) {
    let mut i = start;
    while i < end {
        let tok = &tokens[i];
        // Skip `COLLATE <collation_name>`
        if tok.kind == TokenKind::Collate {
            i += 2;
            continue;
        }
        // Skip `CAST(... AS <type_name>)`: when seeing `AS`, skip until matching `)` if inside CAST, or skip alias
        if tok.kind == TokenKind::As {
            i += 2;
            continue;
        }
        // Qualified reference `<qual>.<col>`
        if i + 2 < end && tokens[i + 1].kind == TokenKind::Dot {
            if token_matches_ident(tok, self_tbl) && token_matches_ident(&tokens[i + 2], old_col) {
                let col_tok = &tokens[i + 2];
                edits.push((
                    col_tok.offset,
                    col_tok.text.len(),
                    format_replacement_ident(col_tok.text, new_col),
                ));
            }
            i += 3;
            continue;
        }
        // Skip function calls `<func_name>(`
        if i + 1 < end && tokens[i + 1].kind == TokenKind::Lp && is_ident_like_token(tok.kind) {
            i += 1;
            continue;
        }
        if token_matches_ident(tok, old_col) {
            edits.push((
                tok.offset,
                tok.text.len(),
                format_replacement_ident(tok.text, new_col),
            ));
        }
        i += 1;
    }
}

fn find_matching_rparen(tokens: &[Token<'_>], lp_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(lp_idx) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_paren_col_list_edits(
    tokens: &[Token<'_>],
    lp_idx: usize,
    rp_idx: usize,
    old_col: &str,
    new_col: &str,
    edits: &mut Vec<(usize, usize, String)>,
) {
    // Handles `(col1 [COLLATE c] [ASC|DESC], col2, ...)`
    let mut item_start = true;
    let mut i = lp_idx + 1;
    while i < rp_idx {
        if tokens[i].kind == TokenKind::Comma {
            item_start = true;
            i += 1;
            continue;
        }
        if item_start {
            if token_matches_ident(&tokens[i], old_col) {
                edits.push((
                    tokens[i].offset,
                    tokens[i].text.len(),
                    format_replacement_ident(tokens[i].text, new_col),
                ));
            }
            item_start = false;
        }
        i += 1;
    }
}

fn is_col_constraint_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Constraint
            | TokenKind::Primary
            | TokenKind::Not
            | TokenKind::Null
            | TokenKind::Unique
            | TokenKind::Check
            | TokenKind::Default
            | TokenKind::Collate
            | TokenKind::References
            | TokenKind::Generated
            | TokenKind::As
    )
}

fn rewrite_column_in_create_table_sql(
    sql: &str,
    self_tbl: &str,
    old_col: &str,
    new_col: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(outer_lp) = tokens.iter().position(|t| t.kind == TokenKind::Lp) else {
        return Ok(sql.to_owned());
    };
    let Some(outer_rp) = find_matching_rparen(&tokens, outer_lp) else {
        return Ok(sql.to_owned());
    };

    // Split top-level items `[item_start..item_end]` inside `( ... )`
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut cur_start = outer_lp + 1;
    for (idx, tok) in tokens.iter().enumerate().take(outer_rp).skip(outer_lp + 1) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => depth = depth.saturating_sub(1),
            TokenKind::Comma if depth == 0 => {
                if cur_start < idx {
                    items.push((cur_start, idx));
                }
                cur_start = idx + 1;
            }
            _ => {}
        }
    }
    if cur_start < outer_rp {
        items.push((cur_start, outer_rp));
    }

    let mut edits = Vec::new();

    for (istart, iend) in items {
        let mut p = istart;
        // Check if table-level constraint (`[CONSTRAINT <name>] PRIMARY|UNIQUE|CHECK|FOREIGN`)
        let is_table_cons = if tokens[p].kind == TokenKind::Constraint && p + 2 < iend {
            matches!(
                tokens[p + 2].kind,
                TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
            )
        } else {
            matches!(
                tokens[p].kind,
                TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
            )
        };

        if is_table_cons {
            if tokens[p].kind == TokenKind::Constraint {
                p += 2;
            }
            match tokens[p].kind {
                TokenKind::Primary | TokenKind::Unique => {
                    if let Some(rel_lp) =
                        tokens[p..iend].iter().position(|t| t.kind == TokenKind::Lp)
                    {
                        let lp = p + rel_lp;
                        if let Some(rp) = find_matching_rparen(&tokens, lp) {
                            collect_paren_col_list_edits(
                                &tokens, lp, rp, old_col, new_col, &mut edits,
                            );
                        }
                    }
                }
                TokenKind::Check => {
                    if p + 1 < iend
                        && tokens[p + 1].kind == TokenKind::Lp
                        && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                    {
                        collect_expr_column_edits(
                            &tokens,
                            p + 2,
                            rp,
                            self_tbl,
                            old_col,
                            new_col,
                            &mut edits,
                        );
                    }
                }
                TokenKind::Foreign => {
                    // `FOREIGN KEY ( <from_cols> ) REFERENCES <ref_tbl> [ ( <ref_cols> ) ]`
                    if let Some(rel_lp) =
                        tokens[p..iend].iter().position(|t| t.kind == TokenKind::Lp)
                    {
                        let lp = p + rel_lp;
                        if let Some(rp) = find_matching_rparen(&tokens, lp) {
                            collect_paren_col_list_edits(
                                &tokens, lp, rp, old_col, new_col, &mut edits,
                            );
                            let mut r = rp + 1;
                            while r < iend && tokens[r].kind != TokenKind::References {
                                r += 1;
                            }
                            if r + 1 < iend
                                && token_matches_ident(&tokens[r + 1], self_tbl)
                                && r + 2 < iend
                                && tokens[r + 2].kind == TokenKind::Lp
                                && let Some(ref_rp) = find_matching_rparen(&tokens, r + 2)
                            {
                                collect_paren_col_list_edits(
                                    &tokens,
                                    r + 2,
                                    ref_rp,
                                    old_col,
                                    new_col,
                                    &mut edits,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Column definition: `<col_name> [type_tokens...] [column_constraints...]`
            if token_matches_ident(&tokens[p], old_col) {
                edits.push((
                    tokens[p].offset,
                    tokens[p].text.len(),
                    format_replacement_ident(tokens[p].text, new_col),
                ));
            }
            p += 1;
            // Skip declared type name (including optional `(len, scale)`)
            while p < iend && !is_col_constraint_keyword(tokens[p].kind) {
                if tokens[p].kind == TokenKind::Lp
                    && let Some(rp) = find_matching_rparen(&tokens, p)
                {
                    p = rp + 1;
                } else {
                    p += 1;
                }
            }
            // Scan column constraints for `CHECK(...)` or self-referencing `REFERENCES self_tbl(...)`
            while p < iend {
                match tokens[p].kind {
                    TokenKind::Constraint => {
                        p += 2;
                    }
                    TokenKind::Check => {
                        if p + 1 < iend
                            && tokens[p + 1].kind == TokenKind::Lp
                            && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                        {
                            collect_expr_column_edits(
                                &tokens,
                                p + 2,
                                rp,
                                self_tbl,
                                old_col,
                                new_col,
                                &mut edits,
                            );
                            p = rp + 1;
                        } else {
                            p += 1;
                        }
                    }
                    TokenKind::Default => {
                        if p + 1 < iend
                            && tokens[p + 1].kind == TokenKind::Lp
                            && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                        {
                            p = rp + 1;
                        } else {
                            p += 2;
                        }
                    }
                    TokenKind::References => {
                        if p + 1 < iend && token_matches_ident(&tokens[p + 1], self_tbl) {
                            if p + 2 < iend
                                && tokens[p + 2].kind == TokenKind::Lp
                                && let Some(ref_rp) = find_matching_rparen(&tokens, p + 2)
                            {
                                collect_paren_col_list_edits(
                                    &tokens,
                                    p + 2,
                                    ref_rp,
                                    old_col,
                                    new_col,
                                    &mut edits,
                                );
                                p = ref_rp + 1;
                                continue;
                            }
                        } else if p + 2 < iend
                            && tokens[p + 2].kind == TokenKind::Lp
                            && let Some(ref_rp) = find_matching_rparen(&tokens, p + 2)
                        {
                            p = ref_rp + 1;
                            continue;
                        }
                        p += 2;
                    }
                    _ => {
                        p += 1;
                    }
                }
            }
        }
    }

    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_column_in_create_index_sql(
    sql: &str,
    tbl_name: &str,
    old_col: &str,
    new_col: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(on_pos) = tokens.iter().position(|t| t.kind == TokenKind::On) else {
        return Ok(sql.to_owned());
    };
    let Some(rel_lp) = tokens[on_pos..]
        .iter()
        .position(|t| t.kind == TokenKind::Lp)
    else {
        return Ok(sql.to_owned());
    };
    let lp = on_pos + rel_lp;
    let Some(rp) = find_matching_rparen(&tokens, lp) else {
        return Ok(sql.to_owned());
    };

    let mut edits = Vec::new();
    collect_paren_col_list_edits(&tokens, lp, rp, old_col, new_col, &mut edits);

    // Also rewrite `WHERE <expr>` on partial indexes if present
    if let Some(rel_where) = tokens[rp + 1..]
        .iter()
        .position(|t| t.kind == TokenKind::Where)
    {
        let where_idx = rp + 1 + rel_where;
        let end_idx = tokens
            .iter()
            .position(|t| matches!(t.kind, TokenKind::Semi | TokenKind::Eof))
            .unwrap_or(tokens.len());
        if where_idx + 1 < end_idx {
            collect_expr_column_edits(
                &tokens,
                where_idx + 1,
                end_idx,
                tbl_name,
                old_col,
                new_col,
                &mut edits,
            );
        }
    }

    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_fk_ref_column_in_create_table_sql(
    sql: &str,
    target_tbl: &str,
    old_col: &str,
    new_col: &str,
) -> Result<Option<String>> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    let mut i = 0usize;
    while i + 2 < tokens.len() {
        if tokens[i].kind == TokenKind::References
            && token_matches_ident(&tokens[i + 1], target_tbl)
            && tokens[i + 2].kind == TokenKind::Lp
            && let Some(rp) = find_matching_rparen(&tokens, i + 2)
        {
            collect_paren_col_list_edits(&tokens, i + 2, rp, old_col, new_col, &mut edits);
            i = rp + 1;
            continue;
        }
        i += 1;
    }
    if edits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(apply_token_replacements(sql, edits)))
    }
}

fn render_column_def_sql(cdef: &sqliters_syntax::ColumnDef) -> String {
    let mut parts = vec![format_replacement_ident("", &cdef.name)];
    if !cdef.type_name.is_empty() {
        parts.push(cdef.type_name.clone());
    }
    for cons in &cdef.constraints {
        match cons {
            sqliters_syntax::ColumnConstraint::NotNull { conflict } => {
                let mut s = "NOT NULL".to_owned();
                if *conflict != ConflictAction::Abort {
                    s.push_str(&format!(" ON CONFLICT {}", format_conflict_kw(*conflict)));
                }
                parts.push(s);
            }
            sqliters_syntax::ColumnConstraint::Null => parts.push("NULL".to_owned()),
            sqliters_syntax::ColumnConstraint::Default(e) => {
                parts.push(format!("DEFAULT ({})", expr_column_label(e)));
            }
            sqliters_syntax::ColumnConstraint::Check { name, expr_sql, .. } => {
                if let Some(n) = name {
                    parts.push(format!(
                        "CONSTRAINT {} CHECK ({expr_sql})",
                        format_replacement_ident("", n)
                    ));
                } else {
                    parts.push(format!("CHECK ({expr_sql})"));
                }
            }
            sqliters_syntax::ColumnConstraint::Generated { expr, stored } => {
                parts.push(format!(
                    "GENERATED ALWAYS AS ({}) {}",
                    expr_column_label(expr),
                    if *stored { "STORED" } else { "VIRTUAL" }
                ));
            }
            sqliters_syntax::ColumnConstraint::Collate(c) => parts.push(format!("COLLATE {c}")),
            sqliters_syntax::ColumnConstraint::ForeignKey {
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                deferred,
            } => {
                let mut fk_s = format!("REFERENCES {}", format_replacement_ident("", ref_table));
                if !ref_columns.is_empty() {
                    let cols: Vec<String> = ref_columns
                        .iter()
                        .map(|c| format_replacement_ident("", c))
                        .collect();
                    fk_s.push_str(&format!("({})", cols.join(", ")));
                }
                if *on_delete != FkAction::NoAction {
                    fk_s.push_str(&format!(" ON DELETE {}", on_delete.as_sql_str()));
                }
                if *on_update != FkAction::NoAction {
                    fk_s.push_str(&format!(" ON UPDATE {}", on_update.as_sql_str()));
                }
                if *deferred {
                    fk_s.push_str(" DEFERRABLE INITIALLY DEFERRED");
                }
                parts.push(fk_s);
            }
            _ => {}
        }
    }
    parts.join(" ")
}

fn format_conflict_kw(c: ConflictAction) -> &'static str {
    match c {
        ConflictAction::Abort => "ABORT",
        ConflictAction::Rollback => "ROLLBACK",
        ConflictAction::Fail => "FAIL",
        ConflictAction::Ignore => "IGNORE",
        ConflictAction::Replace => "REPLACE",
    }
}

fn append_column_to_create_table_sql(sql: &str, rendered_col: &str) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(outer_lp) = tokens.iter().position(|t| t.kind == TokenKind::Lp) else {
        return Err(SqliteError::corrupt("CREATE TABLE missing opening '('"));
    };
    let Some(outer_rp_idx) = find_matching_rparen(&tokens, outer_lp) else {
        return Err(SqliteError::corrupt("CREATE TABLE missing closing ')'"));
    };

    // Match C SQLite `addColOffset` (`sqlite/src/parse.y:223,461`, `sqlite/src/alter.c:442-449`):
    // Insert right before the comma that introduces the first top-level table constraint
    // (`CONSTRAINT | PRIMARY | UNIQUE | CHECK | FOREIGN`), or before the closing `)`
    // if there are no table-level constraints.
    let mut insert_offset = tokens[outer_rp_idx].offset;
    let mut depth = 0usize;
    let mut last_comma_idx: Option<usize> = None;
    let mut item_start = true;
    for idx in (outer_lp + 1)..outer_rp_idx {
        match tokens[idx].kind {
            TokenKind::Lp => {
                if depth == 0 {
                    item_start = false;
                }
                depth += 1;
            }
            TokenKind::Rp => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => {
                last_comma_idx = Some(idx);
                item_start = true;
            }
            kind if depth == 0 && item_start => {
                if matches!(
                    kind,
                    TokenKind::Constraint
                        | TokenKind::Primary
                        | TokenKind::Unique
                        | TokenKind::Check
                        | TokenKind::Foreign
                ) {
                    insert_offset = last_comma_idx
                        .map(|c| tokens[c].offset)
                        .unwrap_or(tokens[idx].offset);
                    break;
                }
                item_start = false;
            }
            _ => {}
        }
    }

    let mut out = String::with_capacity(sql.len() + rendered_col.len() + 4);
    out.push_str(sql[..insert_offset].trim_end());
    out.push_str(", ");
    out.push_str(rendered_col);
    out.push_str(&sql[insert_offset..]);
    Ok(out)
}

// ============================================================================
// `VACUUM` & `PRAGMA` Execution (`vacuum.c`, `pragma.c`)
// ============================================================================

pub(crate) enum VacuumFail {
    Clean(SqliteError),
    Poison(SqliteError),
}

pub(crate) fn exec_vacuum(
    ctx: &mut ExecContext<'_>,
    vac_schema: Option<&str>,
    into: Option<&Expr>,
    params: &[Value],
) -> Result<QueryOutput> {
    exec_vacuum_checked(ctx, vac_schema, into, params).map_err(|vf| match vf {
        VacuumFail::Clean(e) | VacuumFail::Poison(e) => e,
    })
}

fn compute_effective_vacuum_page_size(base_ps: u32, requested: Option<u32>, reserve: u8) -> u32 {
    let Some(mut req) = requested else {
        return base_ps;
    };
    if !sqliters_pager::DatabaseHeader::is_valid_page_size(req) {
        return base_ps;
    }
    if req == 512 && reserve > 32 {
        req = 1024;
    }
    if req.saturating_sub(u32::from(reserve)) >= 480 {
        req
    } else {
        base_ps
    }
}

fn build_vacuum_image(
    ctx: &mut ExecContext<'_>,
    orig_hdr: &sqliters_pager::DatabaseHeader,
    target_btree: &mut Btree,
) -> Result<()> {
    target_btree.begin_write()?;
    {
        let fhdr = target_btree.pager_mut().header_mut()?;
        fhdr.default_cache_size = orig_hdr.default_cache_size;
        fhdr.user_version = orig_hdr.user_version;
        fhdr.application_id = orig_hdr.application_id;
        fhdr.text_encoding = orig_hdr.text_encoding;
        fhdr.schema_cookie = orig_hdr.schema_cookie.wrapping_add(1);
    }

    let tables_snapshot = ctx.schema.tables.clone();
    let indexes_snapshot = ctx.schema.indexes.clone();
    let has_any_autoincrement = tables_snapshot
        .iter()
        .any(|t| !t.name.eq_ignore_ascii_case("sqlite_sequence") && t.autoincrement);

    // Phase A: Allocate roots & insert `sqlite_schema` entries for all tables FIRST (`vacuum.c:298-307`)!
    let mut mapped_tables: Vec<(TableSchema, Pgno)> = Vec::with_capacity(tables_snapshot.len());
    for tbl in tables_snapshot {
        if tbl.name.eq_ignore_ascii_case("sqlite_sequence") {
            if !has_any_autoincrement {
                // Omit orphaned `sqlite_sequence` when no `AUTOINCREMENT` table remains (`vacuum2-1.1, 3.1`)!
                continue;
            }
            let seq_root = target_btree.create_table()?;
            insert_schema_entry(
                target_btree,
                "table",
                &tbl.name,
                &tbl.name,
                seq_root,
                Some(&tbl.raw_sql),
            )?;
            mapped_tables.push((tbl, seq_root));
        } else {
            let new_tbl_root = if tbl.without_rowid {
                target_btree.create_index()?
            } else {
                target_btree.create_table()?
            };
            insert_schema_entry(
                target_btree,
                "table",
                &tbl.name,
                &tbl.name,
                new_tbl_root,
                Some(&tbl.raw_sql),
            )?;
            mapped_tables.push((tbl, new_tbl_root));
        }
    }

    // Phase B: Allocate roots & insert `sqlite_schema` entries for all indexes (`vacuum.c:309-315`)!
    let mut mapped_indexes: Vec<(IndexSchema, Pgno)> = Vec::with_capacity(indexes_snapshot.len());
    for idx in indexes_snapshot {
        let new_idx_root = target_btree.create_index()?;
        insert_schema_entry(
            target_btree,
            "index",
            &idx.name,
            &idx.table_name,
            new_idx_root,
            idx.raw_sql.as_deref(),
        )?;
        mapped_indexes.push((idx, new_idx_root));
    }

    // Phase C: Copy table rows & populate indexes (`vacuum.c:317-323`)!
    for (tbl, new_tbl_root) in &mapped_tables {
        let tbl_indexes: Vec<(IndexSchema, Pgno)> = mapped_indexes
            .iter()
            .filter(|(idx, _)| idx.table_name.eq_ignore_ascii_case(&tbl.name))
            .cloned()
            .collect();

        if tbl.name.eq_ignore_ascii_case("sqlite_sequence") {
            let rows = scan_table_incremental(ctx.btree, tbl.root_pgno)?;
            let mut next_rid = 0i64;
            for (_, payload) in rows {
                next_rid += 1;
                target_btree.insert_table(*new_tbl_root, next_rid, &payload)?;
            }
        } else if tbl.without_rowid {
            let pk_orders = tbl.without_rowid_pk_orders();
            let entries = scan_index_incremental(ctx.btree, tbl.root_pgno, pk_orders.clone())?;
            for rec in entries {
                target_btree.insert_index(*new_tbl_root, &rec, &pk_orders)?;
            }
            for (idx, new_idx_root) in &tbl_indexes {
                let orders = idx.key_sort_orders();
                let idx_entries = scan_index_incremental(ctx.btree, idx.root_pgno, orders.clone())?;
                for rec in idx_entries {
                    target_btree.insert_index(*new_idx_root, &rec, &orders)?;
                }
            }
        } else if tbl.rowid_alias_col.is_some() {
            // Explicit `INTEGER PRIMARY KEY`: preserve exact rowids!
            let rows = scan_table_incremental(ctx.btree, tbl.root_pgno)?;
            for (rid, payload) in rows {
                target_btree.insert_table(*new_tbl_root, rid, &payload)?;
            }
            for (idx, new_idx_root) in &tbl_indexes {
                let orders = idx.key_sort_orders();
                let idx_entries = scan_index_incremental(ctx.btree, idx.root_pgno, orders.clone())?;
                for rec in idx_entries {
                    target_btree.insert_index(*new_idx_root, &rec, &orders)?;
                }
            }
        } else {
            // Implicit rowid table without `INTEGER PRIMARY KEY`: reassign sequential rowids `1..=N`
            // and rebuild secondary/unique index entries against the compacted rowids!
            let rows = scan_table_incremental(ctx.btree, tbl.root_pgno)?;
            let mut next_rid = 0i64;
            for (_, payload) in rows {
                next_rid += 1;
                target_btree.insert_table(*new_tbl_root, next_rid, &payload)?;
                if !tbl_indexes.is_empty() {
                    let new_rkey = TableRowKey::Rowid(next_rid);
                    let vals = decode_table_logical_row(ctx, tbl, &new_rkey, &payload, &[])?;
                    for (idx_schema, new_idx_root) in &tbl_indexes {
                        if !index_row_passes_partial_where(
                            ctx,
                            tbl,
                            idx_schema,
                            next_rid,
                            &vals,
                            &[],
                        )? {
                            continue;
                        }
                        let keys =
                            eval_index_key_values(ctx, tbl, idx_schema, &new_rkey, &vals, &[])?;
                        let orders = idx_schema.key_sort_orders();
                        let idx_rec = encode_record_with_encoding(&keys, target_btree.encoding());
                        target_btree.insert_index(*new_idx_root, &idx_rec, &orders)?;
                    }
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn exec_vacuum_checked(
    ctx: &mut ExecContext<'_>,
    vac_schema: Option<&str>,
    into: Option<&Expr>,
    params: &[Value],
) -> std::result::Result<QueryOutput, VacuumFail> {
    if !is_in_autocommit(ctx) {
        return Err(VacuumFail::Clean(SqliteError::new(
            ErrorCode::Error,
            "cannot VACUUM from within a transaction",
        )));
    }
    if ctx.activity.other_active > 0 {
        return Err(VacuumFail::Clean(SqliteError::new(
            ErrorCode::Error,
            "cannot VACUUM - SQL statements in progress",
        )));
    }
    if let Some(s) = vac_schema {
        if s.eq_ignore_ascii_case("temp") {
            return Ok(QueryOutput::default());
        }
        if !s.eq_ignore_ascii_case("main") {
            return Err(VacuumFail::Clean(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database {s}"),
            )));
        }
    }

    if let Some(into_expr) = into {
        let opened_read = if ctx.btree.transaction_state() == TransactionState::Open {
            ctx.busy_policy
                .retry_btree(ctx.btree, |bt| bt.begin_read())
                .map_err(VacuumFail::Clean)?;
            true
        } else {
            false
        };

        let into_res: Result<()> = (|| {
            ctx.schema.ensure_fresh_in_tx(ctx.btree)?;
            let out_val = eval_expr(ctx, into_expr, params, &[], &CteStore::default(), None)?;
            let Value::Text(out_name) = out_val else {
                return Err(SqliteError::new(ErrorCode::Error, "non-text filename"));
            };

            let orig_hdr = ctx.btree.pager().header().clone();
            let lock_geometry = ctx.btree.pager().lock_geometry();
            let src_ps = ctx.btree.pager().page_size();
            let is_src_memdb = ctx.btree.pager().is_pager_memdb();

            let mut target_is_memory = out_name.is_empty() || out_name == ":memory:";
            let mut target_reserve = orig_hdr.reserved_bytes;
            let mut raw_target_path: &str = out_name.as_str();

            if let Some(rest) = out_name.strip_prefix("file:") {
                let (rp, query_opt) = match rest.split_once('?') {
                    Some((p, q)) => (p, Some(q)),
                    None => (rest, None),
                };
                raw_target_path = rp;
                if rp.is_empty() || rp == ":memory:" {
                    target_is_memory = true;
                }
                if let Some(q) = query_opt {
                    for pair in q.split('&') {
                        if let Some((k, v)) = pair.split_once('=') {
                            if k == "mode" && v.eq_ignore_ascii_case("memory") {
                                target_is_memory = true;
                            } else if k == "reserve"
                                && let Ok(r) = v.parse::<i64>()
                                && (0..=255).contains(&r)
                            {
                                target_reserve = r as u8;
                            }
                        }
                    }
                }
            }

            let target_ps = compute_effective_vacuum_page_size(
                src_ps,
                if is_src_memdb {
                    None
                } else {
                    *ctx.next_page_size
                },
                target_reserve,
            );
            let target_autovac = ctx
                .next_auto_vacuum
                .unwrap_or_else(|| ctx.btree.auto_vacuum_mode());

            if target_is_memory {
                let mem_vfs: std::sync::Arc<dyn sqliters_vfs::Vfs> =
                    std::sync::Arc::new(sqliters_vfs::MemoryVfs::with_lock_geometry(lock_geometry));
                let opts = sqliters_pager::PagerOpenOptions {
                    default_page_size: target_ps,
                    lock_geometry: Some(lock_geometry),
                    db_kind: Some(DatabaseKind::Memory),
                    ..sqliters_pager::PagerOpenOptions::default()
                };
                let mut target_bt = Btree::open_with_options(
                    mem_vfs,
                    Some(std::path::Path::new(":memory:")),
                    sqliters_vfs::OpenFlags::READWRITE
                        | sqliters_vfs::OpenFlags::CREATE
                        | sqliters_vfs::OpenFlags::MEMORY,
                    opts,
                )?;
                target_bt.pager_mut().set_page_size(target_ps)?;
                target_bt.pager_mut().set_reserved_bytes(target_reserve)?;
                target_bt.pager_mut().set_encoding(orig_hdr.encoding())?;
                let _ = target_bt.set_configured_auto_vacuum(target_autovac);
                build_vacuum_image(ctx, &orig_hdr, &mut target_bt)?;
                target_bt.commit_retaining_read(false)?;
                return Ok(());
            }

            let resolved_path = {
                let p = std::path::Path::new(raw_target_path);
                if p.is_relative() {
                    if let Some(ref wd) = *ctx.working_dir {
                        wd.join(p)
                    } else {
                        p.to_path_buf()
                    }
                } else {
                    p.to_path_buf()
                }
            };

            let target_vfs: std::sync::Arc<dyn sqliters_vfs::Vfs> = if is_src_memdb {
                std::sync::Arc::new(sqliters_vfs::OsVfs::with_lock_geometry(lock_geometry))
            } else {
                std::sync::Arc::clone(ctx.btree.pager().vfs())
            };
            let is_os_vfs = matches!(target_vfs.name(), "unix" | "win32");
            if is_os_vfs && std::fs::metadata(&resolved_path).is_ok_and(|m| m.is_dir()) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "output file already exists",
                ));
            }
            let full_target = target_vfs.full_pathname(&resolved_path)?;
            if is_os_vfs
                && let Ok(meta) = std::fs::metadata(&full_target)
                && (meta.is_dir() || meta.len() > 0)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "output file already exists",
                ));
            }
            let existed_before = target_vfs.exists(&full_target)?;
            if !is_os_vfs && existed_before {
                let existing_f = target_vfs.open(
                    Some(&full_target),
                    sqliters_vfs::OpenFlags::READONLY | sqliters_vfs::OpenFlags::MAIN_DB,
                )?;
                let existing_len = existing_f.file_size()?;
                drop(existing_f);
                if existing_len > 0 {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "output file already exists",
                    ));
                }
            }

            let opts = sqliters_pager::PagerOpenOptions {
                default_page_size: target_ps,
                lock_geometry: Some(lock_geometry),
                db_kind: Some(DatabaseKind::NamedDisk),
                ..sqliters_pager::PagerOpenOptions::default()
            };
            let mut target_bt = Btree::open_with_options(
                std::sync::Arc::clone(&target_vfs),
                Some(&full_target),
                sqliters_vfs::OpenFlags::READWRITE | sqliters_vfs::OpenFlags::CREATE,
                opts,
            )?;
            let build_res = (|| -> Result<()> {
                target_bt.pager_mut().set_page_size(target_ps)?;
                target_bt.pager_mut().set_reserved_bytes(target_reserve)?;
                target_bt.pager_mut().set_encoding(orig_hdr.encoding())?;
                target_bt
                    .pager_mut()
                    .set_synchronous(ctx.btree.pager().synchronous());
                let _ = target_bt.set_configured_auto_vacuum(target_autovac);
                build_vacuum_image(ctx, &orig_hdr, &mut target_bt)?;
                target_bt.commit_retaining_read(false)?;
                Ok(())
            })();
            drop(target_bt);
            if let Err(e) = build_res {
                if !existed_before {
                    let _ = target_vfs.delete(&full_target, false);
                }
                return Err(e);
            }
            Ok(())
        })();

        *ctx.next_page_size = None;
        if opened_read {
            let _ = ctx.btree.pager_mut().end_read_if_unused();
        }
        return into_res
            .map(|()| QueryOutput::default())
            .map_err(VacuumFail::Clean);
    }

    // In-place `VACUUM [main]` (`into.is_none()`):
    ctx.busy_policy
        .retry_btree(ctx.btree, |bt| bt.begin_write())
        .map_err(VacuumFail::Clean)?;
    if let Err(e) = ctx.schema.ensure_fresh_in_tx(ctx.btree) {
        *ctx.next_page_size = None;
        if let Err(rb_err) = ctx.btree.rollback() {
            return Err(VacuumFail::Poison(rb_err));
        }
        return Err(VacuumFail::Clean(e));
    }

    let is_wal_db = ctx.btree.journal_mode() == JournalMode::Wal || ctx.btree.pager().is_wal_mode();
    if is_wal_db {
        *ctx.next_page_size = None;
    }

    let res = (|| -> Result<Schema> {
        let src_ps = ctx.btree.pager().page_size();
        let is_src_memdb = ctx.btree.pager().is_pager_memdb();
        let lock_geometry = ctx.btree.pager().lock_geometry();
        let orig_hdr = ctx.btree.pager().header().clone();

        let target_ps = compute_effective_vacuum_page_size(
            src_ps,
            if is_wal_db || is_src_memdb {
                None
            } else {
                *ctx.next_page_size
            },
            orig_hdr.reserved_bytes,
        );
        let target_autovac = ctx
            .next_auto_vacuum
            .unwrap_or_else(|| ctx.btree.auto_vacuum_mode());

        let mem_vfs: std::sync::Arc<dyn sqliters_vfs::Vfs> =
            std::sync::Arc::new(sqliters_vfs::MemoryVfs::with_lock_geometry(lock_geometry));
        let fresh_opts = sqliters_pager::PagerOpenOptions {
            default_page_size: target_ps,
            lock_geometry: Some(lock_geometry),
            db_kind: Some(DatabaseKind::Memory),
            ..sqliters_pager::PagerOpenOptions::default()
        };
        let mut fresh = Btree::open_with_options(
            mem_vfs,
            None,
            sqliters_vfs::OpenFlags::READWRITE
                | sqliters_vfs::OpenFlags::CREATE
                | sqliters_vfs::OpenFlags::MEMORY,
            fresh_opts,
        )?;
        fresh.pager_mut().set_page_size(target_ps)?;
        fresh
            .pager_mut()
            .set_reserved_bytes(orig_hdr.reserved_bytes)?;
        fresh.pager_mut().set_encoding(orig_hdr.encoding())?;
        let _ = fresh.set_configured_auto_vacuum(target_autovac);

        build_vacuum_image(ctx, &orig_hdr, &mut fresh)?;
        fresh.commit_retaining_read(true)?;
        let rebuilt_schema = Schema::load(&mut fresh)?;

        ctx.btree.pager_mut().copy_pages_from(fresh.pager_mut())?;
        Ok(rebuilt_schema)
    })();

    *ctx.next_page_size = None;
    let new_schema = match res {
        Ok(s) => s,
        Err(e) => {
            if let Err(rb_err) = ctx.btree.rollback() {
                return Err(VacuumFail::Poison(rb_err));
            }
            return Err(VacuumFail::Clean(e));
        }
    };
    if let Err(cm_err) = ctx.btree.commit_retaining_read(false) {
        if let Err(rb_err) = ctx.btree.rollback() {
            let root = if cm_err.code == ErrorCode::Busy {
                rb_err
            } else {
                cm_err
            };
            return Err(VacuumFail::Poison(root));
        }
        return Err(VacuumFail::Clean(cm_err));
    }
    *ctx.schema = new_schema;
    *ctx.schema_generation += 1;
    Ok(QueryOutput::default())
}

fn exec_pragma(
    ctx: &mut ExecContext<'_>,
    schema: Option<&str>,
    name: &str,
    value: Option<&PragmaValue>,
) -> Result<QueryOutput> {
    if let Some(s) = schema
        && !s.eq_ignore_ascii_case("main")
        && !s.eq_ignore_ascii_case("temp")
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown database {s}"),
        ));
    }
    let is_temp = schema.is_some_and(|s| s.eq_ignore_ascii_case("temp"));
    let upper = name.to_ascii_uppercase();
    let single_col = |cname: &str, v: Value| QueryOutput {
        column_names: vec![cname.to_owned()],
        rows: vec![vec![v]],
    };

    match upper.as_str() {
        "INTEGRITY_CHECK" | "QUICK_CHECK" => run_read_autocommit(ctx, |ctx| {
            let roots = ctx.schema.all_btree_roots();
            let errs = ctx.btree.integrity_check(&roots)?;
            let rows = if errs.is_empty() {
                vec![vec![Value::Text("ok".to_owned())]]
            } else {
                errs.into_iter().map(|e| vec![Value::Text(e)]).collect()
            };
            Ok(QueryOutput {
                column_names: vec!["integrity_check".to_owned()],
                rows,
            })
        }),
        "PAGE_SIZE" => {
            if is_temp {
                if value.is_some() {
                    return Ok(QueryOutput::default());
                }
                return Ok(single_col(
                    "page_size",
                    Value::Integer(i64::from(sqliters_types::SQLITE_DEFAULT_PAGE_SIZE)),
                ));
            }
            if let Some(pv) = value {
                if ctx.btree.transaction_state() == TransactionState::Open
                    && !ctx.btree.pager().is_header_loaded()
                    && ctx.btree.pager().db_kind() == DatabaseKind::NamedDisk
                {
                    let _ = ctx.busy_policy.retry_btree(ctx.btree, |bt| {
                        bt.pager_mut().ensure_header_loaded().map(|_| ())
                    });
                }
                let raw = pragma_to_i64(pv);
                if let Ok(mut sz) = u32::try_from(raw)
                    && sqliters_pager::DatabaseHeader::is_valid_page_size(sz)
                {
                    let reserve = ctx.btree.pager().header().reserved_bytes;
                    if sz == 512 && reserve > 32 {
                        sz = 1024;
                    }
                    if sz.saturating_sub(u32::from(reserve)) >= 480 {
                        *ctx.next_page_size = Some(sz);
                        if !ctx.btree.pager().is_page_size_fixed()
                            && ctx.btree.pager().page_count() == 0
                            && is_in_autocommit(ctx)
                        {
                            let _ = ctx.btree.pager_mut().set_page_size(sz);
                        }
                    }
                }
                Ok(QueryOutput::default())
            } else {
                let ps = ctx.btree.pager_mut().effective_page_size()?;
                Ok(single_col("page_size", Value::Integer(i64::from(ps))))
            }
        }
        "AUTO_VACUUM" => {
            if is_temp {
                if value.is_some() {
                    return Ok(QueryOutput::default());
                }
                return Ok(single_col("auto_vacuum", Value::Integer(0)));
            }
            if let Some(pv) = value {
                let e_auto = parse_auto_vacuum_pragma(pv);
                *ctx.next_auto_vacuum = Some(e_auto);
                let ok = ctx.btree.set_configured_auto_vacuum(e_auto);
                if ok && matches!(e_auto, AutoVacuumMode::Full | AutoVacuumMode::Incremental) {
                    run_write_autocommit(ctx, |ctx| {
                        if ctx.btree.pager().header().largest_root_btree_page != 0 {
                            let incr_flag = u32::from(e_auto == AutoVacuumMode::Incremental);
                            ctx.btree.pager_mut().header_mut()?.incremental_vacuum_mode = incr_flag;
                            let _ = ctx.btree.set_configured_auto_vacuum(e_auto);
                        }
                        Ok(QueryOutput::default())
                    })?;
                }
                Ok(QueryOutput::default())
            } else {
                let mode = ctx.btree.auto_vacuum_mode();
                Ok(single_col("auto_vacuum", Value::Integer(mode.as_int())))
            }
        }
        "DEFAULT_CACHE_SIZE" => {
            if is_temp {
                if value.is_some() {
                    return Ok(QueryOutput::default());
                }
                return Ok(single_col("cache_size", Value::Integer(2000)));
            }
            if let Some(pv) = value {
                let size = pragma_to_i64(pv).saturating_abs().min(i64::from(i32::MAX)) as i32;
                run_write_autocommit(ctx, |ctx| {
                    ctx.btree.pager_mut().header_mut()?.default_cache_size = size;
                    ctx.btree.set_cache_size(size);
                    Ok(QueryOutput::default())
                })
            } else {
                run_header_read_autocommit(ctx, |ctx| {
                    let raw = ctx.btree.pager().header().default_cache_size;
                    let val = if raw == 0 {
                        2000
                    } else {
                        i64::from(raw.unsigned_abs())
                    };
                    Ok(single_col("cache_size", Value::Integer(val)))
                })
            }
        }
        "WRITABLE_SCHEMA" => {
            if let Some(pv) = value {
                let on = pragma_to_bool(pv)
                    || matches!(pv, PragmaValue::Ident(s) | PragmaValue::String(s) if s.trim().eq_ignore_ascii_case("reset"));
                *ctx.writable_schema = on;
                ctx.btree.set_writable_schema(on);
                if matches!(pv, PragmaValue::Ident(s) | PragmaValue::String(s) if s.trim().eq_ignore_ascii_case("reset"))
                {
                    *ctx.writable_schema = false;
                    ctx.btree.set_writable_schema(false);
                    *ctx.schema = Schema::default();
                    *ctx.schema_generation += 1;
                }
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "writable_schema",
                    Value::Integer(i64::from(*ctx.writable_schema)),
                ))
            }
        }
        "PAGE_COUNT" => run_header_read_autocommit(ctx, |ctx| {
            Ok(single_col(
                "page_count",
                Value::Integer(i64::from(ctx.btree.pager().page_count())),
            ))
        }),
        "FREELIST_COUNT" => run_header_read_autocommit(ctx, |ctx| {
            Ok(single_col(
                "freelist_count",
                Value::Integer(i64::from(ctx.btree.pager().header().total_freelist_pages)),
            ))
        }),
        "USER_VERSION" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                run_write_autocommit(ctx, |ctx| {
                    ctx.btree.pager_mut().header_mut()?.user_version = v;
                    Ok(QueryOutput::default())
                })
            } else {
                run_header_read_autocommit(ctx, |ctx| {
                    Ok(single_col(
                        "user_version",
                        Value::Integer(i64::from(ctx.btree.pager().header().user_version)),
                    ))
                })
            }
        }
        "APPLICATION_ID" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                run_write_autocommit(ctx, |ctx| {
                    ctx.btree.pager_mut().header_mut()?.application_id = v;
                    Ok(QueryOutput::default())
                })
            } else {
                run_header_read_autocommit(ctx, |ctx| {
                    Ok(single_col(
                        "application_id",
                        Value::Integer(i64::from(ctx.btree.pager().header().application_id)),
                    ))
                })
            }
        }
        "SCHEMA_VERSION" => run_header_read_autocommit(ctx, |ctx| {
            Ok(single_col(
                "schema_version",
                Value::Integer(i64::from(ctx.btree.pager().header().schema_cookie)),
            ))
        }),
        "ENCODING" => {
            if let Some(pv) = value {
                let s = pragma_to_string(pv);
                if let Some(enc) = TextEncoding::from_pragma_name(&s)
                    && is_in_autocommit(ctx)
                {
                    ctx.btree.pager_mut().set_encoding(enc)?;
                }
                Ok(QueryOutput::default())
            } else {
                let enc = ctx.btree.pager_mut().effective_encoding()?;
                Ok(single_col(
                    "encoding",
                    Value::Text(enc.as_pragma_str().to_owned()),
                ))
            }
        }
        "JOURNAL_MODE" => {
            if is_temp {
                let active = if let Some(pv) = value {
                    let s = pragma_to_string(pv);
                    match JournalMode::from_pragma_str(s.trim()) {
                        Some(JournalMode::Off) => JournalMode::Off,
                        Some(JournalMode::Memory) => JournalMode::Memory,
                        _ => JournalMode::Delete,
                    }
                } else {
                    JournalMode::Delete
                };
                return Ok(single_col(
                    "journal_mode",
                    Value::Text(active.as_str().to_owned()),
                ));
            }
            if ctx.btree.pager().transaction_state() == TransactionState::Open
                && !ctx.btree.pager().is_header_loaded()
                && ctx.btree.pager().db_kind() == DatabaseKind::NamedDisk
            {
                ctx.busy_policy.retry_btree(ctx.btree, |bt| {
                    bt.pager_mut().ensure_header_loaded().map(|_| ())
                })?;
            }

            let active = if let Some(pv) = value {
                let s = pragma_to_string(pv);
                if let Some(target) = JournalMode::from_pragma_str(s.trim()) {
                    if ctx.btree.pager().ok_to_change_journal_mode() {
                        let old_mode = ctx.btree.pager().journal_mode();
                        let target_mode = ctx.btree.pager().effective_journal_mode_target(target);
                        if target_mode != old_mode
                            && (old_mode == JournalMode::Wal || target_mode == JournalMode::Wal)
                            && (!is_in_autocommit(ctx) || ctx.activity.other_readers > 0)
                        {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                if target_mode == JournalMode::Wal {
                                    "cannot change into wal mode from within a transaction"
                                } else {
                                    "cannot change out of wal mode from within a transaction"
                                },
                            ));
                        }
                    }
                    ctx.busy_policy
                        .retry_btree(ctx.btree, |bt| bt.set_journal_mode(target))?
                } else {
                    ctx.btree.pager().journal_mode()
                }
            } else {
                ctx.btree.pager().journal_mode()
            };
            Ok(single_col(
                "journal_mode",
                Value::Text(active.as_str().to_owned()),
            ))
        }
        "WAL_CHECKPOINT" => {
            let mode = match value {
                None => WalCheckpointMode::Passive,
                Some(pv) => {
                    let s = pragma_to_string(pv);
                    WalCheckpointMode::from_pragma_str(s.trim())
                        .unwrap_or(WalCheckpointMode::Passive)
                }
            };
            let res = if is_temp {
                WalCheckpointResult::not_wal()
            } else {
                if ctx.btree.transaction_state() != TransactionState::Open
                    || ctx.activity.other_readers > 0
                    || ctx.activity.other_writers > 0
                {
                    return Err(SqliteError::new(
                        ErrorCode::Locked,
                        "database table is locked",
                    ));
                }
                if ctx.btree.pager().db_kind() == DatabaseKind::NamedDisk
                    && (!ctx.btree.pager().is_header_loaded()
                        || (ctx.btree.pager().journal_mode() == JournalMode::Wal
                            && ctx.btree.pager().wal().is_none()))
                {
                    ctx.busy_policy
                        .retry_btree(ctx.btree, |bt| bt.begin_read())?;
                    ctx.btree.pager_mut().end_read_if_unused()?;
                }
                if ctx.btree.pager().wal().is_none() {
                    WalCheckpointResult::not_wal()
                } else {
                    ctx.busy_policy.checkpoint_btree(ctx.btree, mode)?
                }
            };
            Ok(QueryOutput {
                column_names: vec![
                    "busy".to_owned(),
                    "log".to_owned(),
                    "checkpointed".to_owned(),
                ],
                rows: vec![vec![
                    Value::Integer(i64::from(res.busy)),
                    Value::Integer(i64::from(res.n_log)),
                    Value::Integer(i64::from(res.n_ckpt)),
                ]],
            })
        }
        "WAL_AUTOCHECKPOINT" => {
            if let Some(pv) = value {
                let raw = pragma_to_i64(pv).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                *ctx.wal_autocheckpoint_threshold = if raw > 0 { raw } else { 0 };
            }
            let effective = (*ctx.wal_autocheckpoint_threshold).max(0);
            Ok(single_col(
                "wal_autocheckpoint",
                Value::Integer(i64::from(effective)),
            ))
        }
        "LOCKING_MODE" => {
            if is_temp {
                return Ok(single_col(
                    "locking_mode",
                    Value::Text("exclusive".to_owned()),
                ));
            }
            if let Some(pv) = value {
                let s = pragma_to_string(pv);
                if let Some(req) = LockingMode::from_pragma_str(s.trim()) {
                    let _ = ctx.btree.pager_mut().set_locking_mode(req);
                    if schema.is_none() {
                        *ctx.default_locking_mode = req;
                    }
                }
            }
            let cur = ctx.btree.pager().locking_mode();
            Ok(single_col(
                "locking_mode",
                Value::Text(cur.as_str().to_owned()),
            ))
        }
        "JOURNAL_SIZE_LIMIT" => {
            if is_temp {
                return Ok(single_col("journal_size_limit", Value::Integer(-1)));
            }
            let res = if let Some(pv) = value {
                let limit = pragma_to_i64(pv).max(-1);
                ctx.btree.pager_mut().set_journal_size_limit(limit)
            } else {
                ctx.btree.pager_mut().set_journal_size_limit(-2)
            };
            Ok(single_col("journal_size_limit", Value::Integer(res)))
        }
        #[cfg(feature = "test-diagnostics")]
        "LOCK_STATUS" => {
            let main_status = match ctx.btree.pager().lock_state_for_diagnostics()? {
                Some(lvl) => lvl.as_diagnostic_str(),
                None => "unknown",
            };
            Ok(QueryOutput {
                column_names: vec!["database".to_owned(), "status".to_owned()],
                rows: vec![
                    vec![
                        Value::Text("main".to_owned()),
                        Value::Text(main_status.to_owned()),
                    ],
                    vec![
                        Value::Text("temp".to_owned()),
                        Value::Text("closed".to_owned()),
                    ],
                ],
            })
        }
        "FOREIGN_KEYS" => {
            if let Some(pv) = value {
                // Per SQLite `pragma.c`, `PRAGMA foreign_keys = bool` is a no-op inside a transaction!
                if is_in_autocommit(ctx) {
                    *ctx.foreign_keys = pragma_to_bool(pv);
                }
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "foreign_keys",
                    Value::Integer(i64::from(*ctx.foreign_keys)),
                ))
            }
        }
        "FOREIGN_KEY_LIST" => run_read_autocommit(ctx, |ctx| {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let cols = vec![
                "id".to_owned(),
                "seq".to_owned(),
                "table".to_owned(),
                "from".to_owned(),
                "to".to_owned(),
                "on_update".to_owned(),
                "on_delete".to_owned(),
                "match".to_owned(),
            ];
            let Some(tbl) = ctx.schema.find_user_table(&tname) else {
                return Ok(QueryOutput {
                    column_names: cols,
                    rows: Vec::new(),
                });
            };
            let mut rows = Vec::new();
            for fk in &tbl.foreign_keys {
                for (seq, from_name) in fk.from_col_names.iter().enumerate() {
                    let to_val = fk
                        .ref_cols
                        .as_ref()
                        .and_then(|rc| rc.get(seq))
                        .map_or(Value::Null, |s| Value::Text(s.clone()));
                    rows.push(vec![
                        Value::Integer(fk.id as i64),
                        Value::Integer(seq as i64),
                        Value::Text(fk.ref_table.clone()),
                        Value::Text(from_name.clone()),
                        to_val,
                        Value::Text(fk.on_update.as_sql_str().to_owned()),
                        Value::Text(fk.on_delete.as_sql_str().to_owned()),
                        Value::Text("NONE".to_owned()),
                    ]);
                }
            }
            Ok(QueryOutput {
                column_names: cols,
                rows,
            })
        }),
        "FOREIGN_KEY_CHECK" => run_read_autocommit(ctx, |ctx| {
            let target_tbl = value.map(pragma_to_string);
            let tables: Vec<TableSchema> = ctx
                .schema
                .tables
                .iter()
                .filter(|t| {
                    target_tbl
                        .as_ref()
                        .is_none_or(|tn| t.name.eq_ignore_ascii_case(tn))
                })
                .cloned()
                .collect();
            let mut rows = Vec::new();
            for tbl in tables {
                if tbl.foreign_keys.is_empty() {
                    continue;
                }
                let raw_rows = scan_table_raw_entries(ctx, &tbl)?;
                for (rkey, payload) in raw_rows {
                    let cvals = decode_table_logical_row(ctx, &tbl, &rkey, &payload, &[])?;
                    for fk in &tbl.foreign_keys {
                        if fk.from_cols.iter().any(|&ci| cvals[ci].is_null()) {
                            continue;
                        }
                        let (ptbl, pcols) = resolve_fk_parent_cols(ctx.schema, &tbl.name, fk)?;
                        if !parent_row_exists_for_fk(ctx, &ptbl, &pcols, &cvals, &fk.from_cols)? {
                            let rowid_val = match &rkey {
                                TableRowKey::Rowid(rid) => Value::Integer(*rid),
                                TableRowKey::WithoutRowidPk(_) => Value::Null,
                            };
                            rows.push(vec![
                                Value::Text(tbl.name.clone()),
                                rowid_val,
                                Value::Text(fk.ref_table.clone()),
                                Value::Integer(fk.id as i64),
                            ]);
                        }
                    }
                }
            }
            Ok(QueryOutput {
                column_names: vec![
                    "table".to_owned(),
                    "rowid".to_owned(),
                    "parent".to_owned(),
                    "fkid".to_owned(),
                ],
                rows,
            })
        }),
        "SYNCHRONOUS" => {
            if let Some(pv) = value {
                if is_in_autocommit(ctx) {
                    let level = match pv {
                        PragmaValue::Integer(i) if (0..=3).contains(i) => Some(*i as u8),
                        PragmaValue::Ident(s) | PragmaValue::String(s) => {
                            match s.trim().to_ascii_uppercase().as_str() {
                                "0" | "OFF" | "FALSE" | "NO" => Some(0),
                                "1" | "NORMAL" | "ON" | "TRUE" | "YES" => Some(1),
                                "2" | "FULL" => Some(2),
                                "3" | "EXTRA" => Some(3),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(lvl) = level {
                        ctx.btree.pager_mut().set_synchronous(lvl);
                    }
                }
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "synchronous",
                    Value::Integer(i64::from(ctx.btree.pager().synchronous())),
                ))
            }
        }
        "CACHE_SIZE" => {
            if let Some(pv) = value {
                let sz = pragma_to_i64(pv).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                if !is_temp {
                    ctx.btree.set_cache_size(sz);
                    if ctx.activity.other_readers == 0 && ctx.activity.other_writers == 0 {
                        ctx.btree.spill_if_over_cache_size()?;
                    }
                }
                Ok(QueryOutput::default())
            } else {
                let cur = if is_temp {
                    -2000
                } else {
                    ctx.btree.cache_size()
                };
                Ok(single_col("cache_size", Value::Integer(i64::from(cur))))
            }
        }
        "DATABASE_LIST" => {
            let file_str = ctx
                .btree
                .pager()
                .db_path()
                .map_or_else(String::new, |p| p.to_string_lossy().into_owned());
            Ok(QueryOutput {
                column_names: vec!["seq".to_owned(), "name".to_owned(), "file".to_owned()],
                rows: vec![vec![
                    Value::Integer(0),
                    Value::Text("main".to_owned()),
                    Value::Text(file_str),
                ]],
            })
        }
        "TABLE_INFO" => run_read_autocommit(ctx, |ctx| {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let Some(tbl) = ctx.schema.find_table(&tname) else {
                return Ok(QueryOutput {
                    column_names: vec![
                        "cid".to_owned(),
                        "name".to_owned(),
                        "type".to_owned(),
                        "notnull".to_owned(),
                        "dflt_value".to_owned(),
                        "pk".to_owned(),
                    ],
                    rows: Vec::new(),
                });
            };
            let rows = tbl
                .columns
                .iter()
                .enumerate()
                .map(|(cid, col)| {
                    let dflt = col
                        .default_expr
                        .as_ref()
                        .map_or(Value::Null, |e| Value::Text(expr_column_label(e)));
                    vec![
                        Value::Integer(cid as i64),
                        Value::Text(col.name.clone()),
                        Value::Text(col.type_name.clone()),
                        Value::Integer(i64::from(col.not_null.is_some())),
                        dflt,
                        Value::Integer(col.pk_order as i64),
                    ]
                })
                .collect();
            Ok(QueryOutput {
                column_names: vec![
                    "cid".to_owned(),
                    "name".to_owned(),
                    "type".to_owned(),
                    "notnull".to_owned(),
                    "dflt_value".to_owned(),
                    "pk".to_owned(),
                ],
                rows,
            })
        }),
        "INDEX_LIST" => run_read_autocommit(ctx, |ctx| {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let mut idxs = ctx.schema.indexes_for_table(&tname);
            if let Some(tbl) = ctx.schema.find_user_table(&tname)
                && let Some(pk_idx) = tbl.without_rowid_pk_as_index()
            {
                idxs.insert(0, pk_idx);
            }
            let rows = idxs
                .iter()
                .enumerate()
                .map(|(seq, idx)| {
                    vec![
                        Value::Integer(seq as i64),
                        Value::Text(idx.name.clone()),
                        Value::Integer(i64::from(idx.unique)),
                        Value::Text(idx.origin.to_owned()),
                        Value::Integer(i64::from(idx.where_clause.is_some())),
                    ]
                })
                .collect();
            Ok(QueryOutput {
                column_names: vec![
                    "seq".to_owned(),
                    "name".to_owned(),
                    "unique".to_owned(),
                    "origin".to_owned(),
                    "partial".to_owned(),
                ],
                rows,
            })
        }),
        "COUNT_CHANGES" => {
            if let Some(pv) = value {
                *ctx.count_changes = pragma_to_bool(pv);
                Ok(QueryOutput::default())
            } else {
                Ok(single_col(
                    "count_changes",
                    Value::Integer(i64::from(*ctx.count_changes)),
                ))
            }
        }
        "INDEX_INFO" => run_read_autocommit(ctx, |ctx| {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let iname = pragma_to_string(pv);
            let cols = vec!["seqno".to_owned(), "cid".to_owned(), "name".to_owned()];
            let Some(idx) = ctx.schema.find_index_including_without_rowid_pk(&iname) else {
                return Ok(QueryOutput {
                    column_names: cols,
                    rows: Vec::new(),
                });
            };
            let rows = idx
                .columns
                .iter()
                .enumerate()
                .map(|(seqno, ic)| match ic.col_idx {
                    Some(ci) => vec![
                        Value::Integer(seqno as i64),
                        Value::Integer(ci as i64),
                        Value::Text(ic.name.clone()),
                    ],
                    None => vec![
                        Value::Integer(seqno as i64),
                        Value::Integer(-2),
                        Value::Null,
                    ],
                })
                .collect();
            Ok(QueryOutput {
                column_names: cols,
                rows,
            })
        }),
        "COMPILE_OPTIONS" => Ok(QueryOutput {
            column_names: vec!["compile_options".to_owned()],
            rows: vec![
                vec![Value::Text("PURE_SAFE_RUST".to_owned())],
                vec![Value::Text("SQLITE_FORMAT_3".to_owned())],
                vec![Value::Text("THREADSAFE=1".to_owned())],
            ],
        }),
        _ => {
            // Unknown PRAGMAs in SQLite are silently ignored (no error)
            let _ = HashMap::<u8, u8>::new();
            Ok(QueryOutput::default())
        }
    }
}

fn pragma_to_i64(pv: &PragmaValue) -> i64 {
    match pv {
        PragmaValue::Integer(i) => *i,
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.trim().parse::<i64>().unwrap_or(0),
    }
}

fn pragma_to_bool(pv: &PragmaValue) -> bool {
    match pv {
        PragmaValue::Integer(i) => *i != 0,
        PragmaValue::Ident(s) | PragmaValue::String(s) => matches!(
            s.trim().to_ascii_uppercase().as_str(),
            "1" | "ON" | "TRUE" | "YES"
        ),
    }
}

fn pragma_to_string(pv: &PragmaValue) -> String {
    match pv {
        PragmaValue::Integer(i) => i.to_string(),
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.clone(),
    }
}

// ============================================================================
// Prepare-Time Static Semantic Validation & Column Metadata Inference (`prepare.c`)
// ============================================================================

/// Lease cursor descriptor opened for interpreter `Read` statements so cursor-trip and
/// `OP_Destroy` table-lock semantics match VDBE cursors.
#[derive(Debug, Clone)]
pub(crate) enum LeaseCursorSpec {
    Table(Pgno),
    Index(Pgno, Vec<KeySortOrder>),
}

/// Collects B-tree roots referenced by a `Read` statement (`SELECT` / `PRAGMA`) so interpreter
/// queries keep open read cursors alive while paused at `StepResult::Row`.
pub(crate) fn collect_select_referenced_roots(
    stmt: &Stmt,
    schema: &Schema,
) -> Vec<LeaseCursorSpec> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    match stmt {
        Stmt::Select(sel) => {
            let mut cte_names = Vec::new();
            collect_select_roots_rec(sel, schema, &mut cte_names, &mut seen, &mut out);
        }
        Stmt::Pragma { name, .. } => {
            let upper = name.to_ascii_uppercase();
            if matches!(
                upper.as_str(),
                "TABLE_INFO"
                    | "INDEX_LIST"
                    | "INDEX_INFO"
                    | "FOREIGN_KEY_LIST"
                    | "FOREIGN_KEY_CHECK"
                    | "INTEGRITY_CHECK"
                    | "QUICK_CHECK"
            ) && seen.insert(1)
            {
                out.push(LeaseCursorSpec::Table(1));
            }
        }
        _ => {}
    }
    if out.is_empty() {
        out.push(LeaseCursorSpec::Table(1));
    }
    out
}

fn collect_select_roots_rec(
    sel: &SelectStmt,
    schema: &Schema,
    cte_stack: &mut Vec<String>,
    seen: &mut HashSet<Pgno>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    let base_len = cte_stack.len();
    for cte in &sel.ctes {
        cte_stack.push(cte.name.clone());
        collect_select_roots_rec(&cte.query, schema, cte_stack, seen, out);
    }
    collect_core_roots_rec(&sel.body.head, schema, cte_stack, seen, out);
    for (_, rhs) in &sel.body.compounds {
        collect_core_roots_rec(rhs, schema, cte_stack, seen, out);
    }
    for ob in &sel.order_by {
        collect_expr_roots_rec(&ob.expr, schema, cte_stack, seen, out);
    }
    if let Some(ref lim) = sel.limit {
        collect_expr_roots_rec(&lim.limit, schema, cte_stack, seen, out);
        if let Some(ref off) = lim.offset {
            collect_expr_roots_rec(off, schema, cte_stack, seen, out);
        }
    }
    cte_stack.truncate(base_len);
}

fn collect_core_roots_rec(
    core: &SelectCore,
    schema: &Schema,
    cte_stack: &mut Vec<String>,
    seen: &mut HashSet<Pgno>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_expr_roots_rec(e, schema, cte_stack, seen, out);
                }
            }
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            if let Some(fc) = from {
                collect_tos_roots_rec(&fc.head, schema, cte_stack, seen, out);
                for j in &fc.joins {
                    collect_tos_roots_rec(&j.right, schema, cte_stack, seen, out);
                    if let Some(JoinConstraint::On(ref e)) = j.constraint {
                        collect_expr_roots_rec(e, schema, cte_stack, seen, out);
                    }
                }
            }
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_roots_rec(expr, schema, cte_stack, seen, out);
                }
            }
            if let Some(w) = selection {
                collect_expr_roots_rec(w, schema, cte_stack, seen, out);
            }
            for g in group_by {
                collect_expr_roots_rec(g, schema, cte_stack, seen, out);
            }
            if let Some(h) = having {
                collect_expr_roots_rec(h, schema, cte_stack, seen, out);
            }
        }
    }
}

fn collect_table_root_spec(
    name: &QualifiedName,
    schema: &Schema,
    cte_stack: &[String],
    seen: &mut HashSet<Pgno>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    if name.schema.is_none() && cte_stack.iter().any(|c| c.eq_ignore_ascii_case(&name.name)) {
        return;
    }
    if name
        .schema
        .as_deref()
        .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
    {
        return;
    }
    if Schema::is_schema_table_name(&name.name) {
        if seen.insert(1) {
            out.push(LeaseCursorSpec::Table(1));
        }
        return;
    }
    if let Some(tbl) = schema.find_user_table(&name.name)
        && tbl.root_pgno != 0
        && seen.insert(tbl.root_pgno)
    {
        if tbl.without_rowid {
            out.push(LeaseCursorSpec::Index(
                tbl.root_pgno,
                tbl.without_rowid_pk_orders(),
            ));
        } else {
            out.push(LeaseCursorSpec::Table(tbl.root_pgno));
        }
    }
}

fn collect_tos_roots_rec(
    tos: &TableOrSubquery,
    schema: &Schema,
    cte_stack: &mut Vec<String>,
    seen: &mut HashSet<Pgno>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match tos {
        TableOrSubquery::Table { name, .. } => {
            collect_table_root_spec(name, schema, cte_stack, seen, out);
        }
        TableOrSubquery::Subquery { query, .. } => {
            collect_select_roots_rec(query, schema, cte_stack, seen, out);
        }
    }
}

fn collect_expr_roots_rec(
    expr: &Expr,
    schema: &Schema,
    cte_stack: &mut Vec<String>,
    seen: &mut HashSet<Pgno>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match expr {
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::IsNull { expr: e, .. }
        | Expr::Cast { expr: e, .. } => collect_expr_roots_rec(e, schema, cte_stack, seen, out),
        Expr::RowValue(es) => {
            for e in es {
                collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_roots_rec(left, schema, cte_stack, seen, out);
            collect_expr_roots_rec(right, schema, cte_stack, seen, out);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            collect_expr_roots_rec(low, schema, cte_stack, seen, out);
            collect_expr_roots_rec(high, schema, cte_stack, seen, out);
        }
        Expr::InList { expr: e, list, .. } => {
            collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            for x in list {
                collect_expr_roots_rec(x, schema, cte_stack, seen, out);
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            collect_select_roots_rec(subquery, schema, cte_stack, seen, out);
        }
        Expr::InTable { expr: e, table, .. } => {
            collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            collect_table_root_spec(table, schema, cte_stack, seen, out);
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_expr_roots_rec(e, schema, cte_stack, seen, out);
            collect_expr_roots_rec(pattern, schema, cte_stack, seen, out);
            if let Some(esc) = escape {
                collect_expr_roots_rec(esc, schema, cte_stack, seen, out);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_expr_roots_rec(op, schema, cte_stack, seen, out);
            }
            for (w, t) in branches {
                collect_expr_roots_rec(w, schema, cte_stack, seen, out);
                collect_expr_roots_rec(t, schema, cte_stack, seen, out);
            }
            if let Some(el) = else_expr {
                collect_expr_roots_rec(el, schema, cte_stack, seen, out);
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    collect_expr_roots_rec(e, schema, cte_stack, seen, out);
                }
            }
            if let Some(f) = filter {
                collect_expr_roots_rec(f, schema, cte_stack, seen, out);
            }
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            collect_select_roots_rec(sub, schema, cte_stack, seen, out);
        }
        _ => {}
    }
}

/// Rewrites a bare top-level identifier (`Expr::Column(ColumnRef { schema: None, table: None, column })`)
/// in `ATTACH ... AS <schema_expr>` / `DETACH <schema_expr>` into a string literal (`sqlite/src/attach.c:codeAttach`).
fn normalize_attach_schema_expr(expr: &Expr) -> std::borrow::Cow<'_, Expr> {
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = expr
    {
        std::borrow::Cow::Owned(Expr::String(column.clone()))
    } else {
        std::borrow::Cow::Borrowed(expr)
    }
}

/// Validates semantic bindings (tables, columns, functions, subqueries, DML/DDL widths) with
/// ZERO side effects and returns the exact result column names for `Connection::prepare`.
pub(crate) fn validate_and_infer_columns(
    stmt: &Stmt,
    schema: &Schema,
    foreign_keys: bool,
    count_changes: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Result<Vec<String>> {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_) => Ok(Vec::new()),

        Stmt::Analyze(target) => {
            validate_analyze_target(schema, target.as_ref())?;
            Ok(Vec::new())
        }

        Stmt::AttachDatabase {
            expr,
            schema_name,
            key,
        } => {
            let mut visited = Vec::new();
            validate_expr_static(
                expr,
                &[],
                schema,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            let norm_schema = normalize_attach_schema_expr(schema_name);
            validate_expr_static(
                &norm_schema,
                &[],
                schema,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            if let Some(k) = key {
                validate_expr_static(
                    k,
                    &[],
                    schema,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::DetachDatabase { schema_name } => {
            let mut visited = Vec::new();
            let norm_schema = normalize_attach_schema_expr(schema_name);
            validate_expr_static(
                &norm_schema,
                &[],
                schema,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            Ok(Vec::new())
        }

        Stmt::Vacuum {
            schema: vac_schema,
            into,
        } => {
            let mut is_temp = false;
            if let Some(s) = vac_schema.as_deref() {
                if s.eq_ignore_ascii_case("temp") {
                    is_temp = true;
                } else if !s.eq_ignore_ascii_case("main") {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("unknown database {s}"),
                    ));
                }
            }
            if !is_temp && let Some(expr) = into {
                let mut visited = Vec::new();
                validate_expr_static(
                    expr,
                    &[],
                    schema,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::Reindex(target) => {
            if let Some(qn) = target {
                if let Some(ref s) = qn.schema {
                    if !s.eq_ignore_ascii_case("main") && !s.eq_ignore_ascii_case("temp") {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("unknown database {s}"),
                        ));
                    }
                    let ok = s.eq_ignore_ascii_case("main")
                        && (schema.find_user_table(&qn.name).is_some()
                            || schema.find_index(&qn.name).is_some());
                    if !ok {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "unable to identify the object to be reindexed",
                        ));
                    }
                } else {
                    let ok = Collation::from_name(&qn.name).is_some()
                        || collations.has_collation(&qn.name)
                        || schema.find_user_table(&qn.name).is_some()
                        || schema.find_index(&qn.name).is_some();
                    if !ok {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "unable to identify the object to be reindexed",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::Pragma {
            schema: pragma_schema,
            name,
            value,
        } => {
            if let Some(s) = pragma_schema.as_deref()
                && !s.eq_ignore_ascii_case("main")
                && !s.eq_ignore_ascii_case("temp")
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            Ok(infer_pragma_columns(name, value.as_ref()))
        }

        Stmt::Explain { query_plan, stmt } => {
            let _ = validate_and_infer_columns(
                stmt,
                schema,
                foreign_keys,
                count_changes,
                functions,
                collations,
            )?;
            if *query_plan {
                Ok(vec![
                    "id".to_owned(),
                    "parent".to_owned(),
                    "notused".to_owned(),
                    "detail".to_owned(),
                ])
            } else {
                Ok(vec![
                    "addr".to_owned(),
                    "opcode".to_owned(),
                    "p1".to_owned(),
                    "p2".to_owned(),
                    "p3".to_owned(),
                    "p4".to_owned(),
                    "p5".to_owned(),
                    "comment".to_owned(),
                ])
            }
        }

        Stmt::Select(sel) => {
            let mut visited = Vec::new();
            validate_select_stmt_static(
                sel,
                &[],
                schema,
                &CteStore::default(),
                functions,
                &mut visited,
            )
        }

        Stmt::CreateTable(ct) => {
            let is_temp = validate_create_table_target(ct.temp, &ct.name)?;
            if is_reserved_sqlite_name(&ct.name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", ct.name.name),
                ));
            }
            if !is_temp
                && (schema.find_user_table(&ct.name.name).is_some()
                    || schema.find_index(&ct.name.name).is_some())
            {
                if ct.if_not_exists {
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} already exists", ct.name.name),
                ));
            }
            let (tbl_preview, _) = build_table_schema_from_ast(ct, 2)?;
            let tbl_scope = make_static_table_scope(&tbl_preview, None);
            let mut visited = Vec::new();
            for col in &tbl_preview.columns {
                if let Some(ref de) = col.default_expr {
                    validate_expr_static(
                        de,
                        &[],
                        schema,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
                if let Some((ref ge, _)) = col.generated {
                    validate_expr_static(
                        ge,
                        std::slice::from_ref(&tbl_scope),
                        schema,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
            for chk in &tbl_preview.checks {
                validate_expr_static(
                    &chk.expr,
                    std::slice::from_ref(&tbl_scope),
                    schema,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::CreateTableAs {
            temp,
            if_not_exists,
            name,
            query,
        } => {
            let is_temp = validate_create_table_target(*temp, name)?;
            if is_reserved_sqlite_name(&name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", name.name),
                ));
            }
            if !is_temp
                && (schema.find_user_table(&name.name).is_some()
                    || schema.find_index(&name.name).is_some())
            {
                if *if_not_exists {
                    let mut visited = Vec::new();
                    let _ = validate_select_stmt_static(
                        query,
                        &[],
                        schema,
                        &CteStore::default(),
                        functions,
                        &mut visited,
                    )?;
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} already exists", name.name),
                ));
            }
            let mut visited = Vec::new();
            let _ = validate_select_stmt_static(
                query,
                &[],
                schema,
                &CteStore::default(),
                functions,
                &mut visited,
            )?;
            Ok(Vec::new())
        }

        Stmt::DropTable { if_exists, name } => {
            validate_drop_or_alter_schema_table(name, "dropped")?;
            let exists_in_target = name
                .schema
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case("main"))
                && schema.find_user_table(&name.name).is_some();
            if !exists_in_target && !if_exists {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", name.display_name()),
                ));
            }
            Ok(Vec::new())
        }

        Stmt::CreateIndex(ci) => {
            if let Some(ref s) = ci.name.schema
                && !s.eq_ignore_ascii_case("main")
                && !s.eq_ignore_ascii_case("temp")
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            if is_reserved_sqlite_name(&ci.name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", ci.name.name),
                ));
            }
            if ci
                .name
                .schema
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
            {
                if schema.find_user_table(&ci.table_name).is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "cannot create a TEMP index on non-TEMP table \"{}\"",
                            ci.table_name
                        ),
                    ));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: temp.{}", ci.table_name),
                ));
            }
            if schema.find_index(&ci.name.name).is_some()
                || schema.find_user_table(&ci.name.name).is_some()
            {
                if ci.if_not_exists {
                    if schema.find_user_table(&ci.table_name).is_none() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such table: {}", ci.table_name),
                        ));
                    }
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("index {} already exists", ci.name.name),
                ));
            }
            let Some(tbl) = schema.find_user_table(&ci.table_name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ci.table_name),
                ));
            };
            let idx_preview = build_index_schema_from_ast(ci, tbl, 2)?;
            let tbl_scope = make_static_table_scope(tbl, None);
            let mut visited = Vec::new();
            for ic in &idx_preview.columns {
                if let Some(ref ie) = ic.expr {
                    validate_expr_static(
                        ie,
                        std::slice::from_ref(&tbl_scope),
                        schema,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
            if let Some(ref wc) = idx_preview.where_clause {
                validate_expr_static(
                    wc,
                    std::slice::from_ref(&tbl_scope),
                    schema,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::DropIndex { if_exists, name } => {
            if name
                .schema
                .as_deref()
                .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
            {
                if !if_exists {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such index: {}", name.display_name()),
                    ));
                }
                return Ok(Vec::new());
            }
            match schema.find_index_including_without_rowid_pk(&name.name) {
                None => {
                    if !if_exists {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such index: {}", name.display_name()),
                        ));
                    }
                }
                Some(idx) => {
                    if idx.is_autoindex {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::AlterTable { table, action } => {
            validate_drop_or_alter_schema_table(table, "altered")?;
            if table
                .schema
                .as_deref()
                .is_some_and(|s| !s.eq_ignore_ascii_case("main"))
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", table.display_name()),
                ));
            }
            let Some(tbl) = schema.find_user_table(&table.name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", table.display_name()),
                ));
            };
            match action {
                AlterTableAction::RenameTo(new_name) => {
                    if is_reserved_sqlite_name(new_name) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("object name reserved for internal use: {new_name}"),
                        ));
                    }
                    if schema.find_user_table(new_name).is_some()
                        || schema.find_index(new_name).is_some()
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "there is already another table or index with this name: {new_name}"
                            ),
                        ));
                    }
                }
                AlterTableAction::RenameColumn { old_name, new_name } => {
                    if tbl.find_column(old_name).is_none() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such column: \"{old_name}\""),
                        ));
                    }
                    if !old_name.eq_ignore_ascii_case(new_name)
                        && tbl.find_column(new_name).is_some()
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("duplicate column name: {new_name}"),
                        ));
                    }
                }
                AlterTableAction::AddColumn(cdef) => {
                    if tbl.find_column(&cdef.name).is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("duplicate column name: {}", cdef.name),
                        ));
                    }
                    let validated = build_column_schema_from_def(&tbl.name, tbl.strict, cdef)?;
                    if validated
                        .generated
                        .as_ref()
                        .is_some_and(|(_, stored)| *stored)
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "cannot add a STORED column",
                        ));
                    }
                    let has_fk = cdef
                        .constraints
                        .iter()
                        .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::ForeignKey { .. }));
                    if has_fk
                        && foreign_keys
                        && validated
                            .default_expr
                            .as_ref()
                            .is_some_and(|e| !matches!(e, Expr::Null))
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "Cannot add a REFERENCES column with non-NULL default value",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::Insert(ins) => validate_insert_static(ins, schema, functions, count_changes),
        Stmt::Update(upd) => validate_update_static(upd, schema, functions, count_changes),
        Stmt::Delete(del) => validate_delete_static(del, schema, functions, count_changes),
    }
}

fn infer_pragma_columns(name: &str, value: Option<&PragmaValue>) -> Vec<String> {
    match name.to_ascii_uppercase().as_str() {
        "INTEGRITY_CHECK" | "QUICK_CHECK" => vec!["integrity_check".to_owned()],
        "PAGE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["page_size".to_owned()]
            }
        }
        "AUTO_VACUUM" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["auto_vacuum".to_owned()]
            }
        }
        "INCREMENTAL_VACUUM" => Vec::new(),
        "DEFAULT_CACHE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["cache_size".to_owned()]
            }
        }
        "WRITABLE_SCHEMA" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["writable_schema".to_owned()]
            }
        }
        "PAGE_COUNT" => vec!["page_count".to_owned()],
        "FREELIST_COUNT" => vec!["freelist_count".to_owned()],
        "USER_VERSION" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["user_version".to_owned()]
            }
        }
        "APPLICATION_ID" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["application_id".to_owned()]
            }
        }
        "SCHEMA_VERSION" => vec!["schema_version".to_owned()],
        "ENCODING" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["encoding".to_owned()]
            }
        }
        "JOURNAL_MODE" => vec!["journal_mode".to_owned()],
        "WAL_CHECKPOINT" => vec![
            "busy".to_owned(),
            "log".to_owned(),
            "checkpointed".to_owned(),
        ],
        "WAL_AUTOCHECKPOINT" => vec!["wal_autocheckpoint".to_owned()],
        "LOCKING_MODE" => vec!["locking_mode".to_owned()],
        "JOURNAL_SIZE_LIMIT" => vec!["journal_size_limit".to_owned()],
        #[cfg(feature = "test-diagnostics")]
        "LOCK_STATUS" => vec!["database".to_owned(), "status".to_owned()],
        "FOREIGN_KEYS" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["foreign_keys".to_owned()]
            }
        }
        "FOREIGN_KEY_LIST" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "id".to_owned(),
                    "seq".to_owned(),
                    "table".to_owned(),
                    "from".to_owned(),
                    "to".to_owned(),
                    "on_update".to_owned(),
                    "on_delete".to_owned(),
                    "match".to_owned(),
                ]
            }
        }
        "FOREIGN_KEY_CHECK" => vec![
            "table".to_owned(),
            "rowid".to_owned(),
            "parent".to_owned(),
            "fkid".to_owned(),
        ],
        "SYNCHRONOUS" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["synchronous".to_owned()]
            }
        }
        "CACHE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["cache_size".to_owned()]
            }
        }
        "DATABASE_LIST" => vec!["seq".to_owned(), "name".to_owned(), "file".to_owned()],
        "TABLE_INFO" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "cid".to_owned(),
                    "name".to_owned(),
                    "type".to_owned(),
                    "notnull".to_owned(),
                    "dflt_value".to_owned(),
                    "pk".to_owned(),
                ]
            }
        }
        "INDEX_LIST" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "seq".to_owned(),
                    "name".to_owned(),
                    "unique".to_owned(),
                    "origin".to_owned(),
                    "partial".to_owned(),
                ]
            }
        }
        "COUNT_CHANGES" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["count_changes".to_owned()]
            }
        }
        "INDEX_INFO" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec!["seqno".to_owned(), "cid".to_owned(), "name".to_owned()]
            }
        }
        "COMPILE_OPTIONS" => vec!["compile_options".to_owned()],
        _ => Vec::new(),
    }
}

fn make_static_table_scope(tbl: &TableSchema, alias: Option<&str>) -> EvalRow {
    make_single_table_scope(tbl, alias, 0, &vec![Value::Null; tbl.columns.len()])
}

fn make_static_returning_scope(tbl: &TableSchema) -> EvalRow {
    make_returning_table_scope(tbl, 0, &vec![Value::Null; tbl.columns.len()])
}

fn validate_index_hint_static(
    schema: &Schema,
    tbl: &TableSchema,
    indexed_by: Option<&IndexHint>,
    where_hint: Option<&Expr>,
) -> Result<()> {
    if let Some(IndexHint::IndexedBy(idx_name)) = indexed_by {
        let Some(idx) = schema
            .indexes_for_table(&tbl.name)
            .into_iter()
            .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such index: {idx_name}"),
            ));
        };
        if let Some(ref pw) = idx.where_clause {
            let usable = where_hint.is_some_and(|wh| expr_contains_conjunct(wh, pw));
            if !usable {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot use index: {idx_name}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_cte_scope_static(ctes_list: &[CommonTableExpr]) -> Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(ctes_list.len());
    for cte in ctes_list {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&cte.name)) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("duplicate WITH table name: {}", cte.name),
            ));
        }
        seen.push(&cte.name);
        if let Some(ref cols) = cte.columns {
            let mut cseen: Vec<&str> = Vec::with_capacity(cols.len());
            for cn in cols {
                if cseen.iter().any(|x| x.eq_ignore_ascii_case(cn)) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("duplicate column name: {cn}"),
                    ));
                }
                cseen.push(cn);
            }
        }
    }
    Ok(())
}

fn validate_insert_static(
    ins: &InsertStmt,
    schema: &Schema,
    functions: &FunctionRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&ins.ctes)?;
    let tbl = resolve_dml_target_table(schema, &ins.table)?;
    let dml_ctes = CteStore::default().with_scope(&ins.ctes);
    let mut visited = Vec::new();

    let expected_width = if let Some(ref cnames) = ins.columns {
        for cn in cnames {
            if let Some((_, col)) = tbl.find_column(cn) {
                if col.generated.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot INSERT into generated column \"{}\"", col.name),
                    ));
                }
            } else if !tbl.is_implicit_rowid_name(cn) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} has no column named {cn}", tbl.name),
                ));
            }
        }
        cnames.len()
    } else {
        tbl.columns.iter().filter(|c| c.generated.is_none()).count()
    };

    match &ins.source {
        InsertSource::DefaultValues => {}
        InsertSource::Values(expr_rows) => {
            for erow in expr_rows {
                if erow.len() != expected_width {
                    if ins.columns.is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{} values for {} columns", erow.len(), expected_width),
                        ));
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} columns but {} values were supplied",
                            tbl.name,
                            expected_width,
                            erow.len()
                        ),
                    ));
                }
                for e in erow {
                    validate_expr_static(
                        e,
                        &[],
                        schema,
                        &dml_ctes,
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
        }
        InsertSource::Select(sel) => {
            let sel_cols =
                validate_select_stmt_static(sel, &[], schema, &dml_ctes, functions, &mut visited)?;
            if sel_cols.len() != expected_width {
                if ins.columns.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("{} values for {} columns", sel_cols.len(), expected_width),
                    ));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        tbl.name,
                        expected_width,
                        sel_cols.len()
                    ),
                ));
            }
        }
    }

    if !ins.returning.is_empty() {
        let sample_scope = make_static_returning_scope(tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&ins.returning, &sample_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&sample_scope),
                schema,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_update_static(
    upd: &UpdateStmt,
    schema: &Schema,
    functions: &FunctionRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&upd.ctes)?;
    let tbl = resolve_dml_target_table(schema, &upd.table)?;
    validate_index_hint_static(schema, tbl, upd.indexed_by.as_ref(), upd.selection.as_ref())?;
    let dml_ctes = CteStore::default().with_scope(&upd.ctes);
    let scope = make_static_table_scope(tbl, upd.alias.as_deref());
    let mut visited = Vec::new();

    for (cname, expr) in &upd.assignments {
        if let Some((_, col)) = tbl.find_column(cname) {
            if col.generated.is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot UPDATE generated column \"{}\"", col.name),
                ));
            }
        } else if !tbl.is_implicit_rowid_name(cname) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {cname}"),
            ));
        }
        validate_expr_static(
            expr,
            std::slice::from_ref(&scope),
            schema,
            &dml_ctes,
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    if let Some(ref w) = upd.selection {
        validate_expr_static(
            w,
            std::slice::from_ref(&scope),
            schema,
            &dml_ctes,
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    if !upd.returning.is_empty() {
        let ret_scope = make_static_returning_scope(tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&upd.returning, &ret_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&ret_scope),
                schema,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_delete_static(
    del: &DeleteStmt,
    schema: &Schema,
    functions: &FunctionRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&del.ctes)?;
    let tbl = resolve_dml_target_table(schema, &del.table)?;
    validate_index_hint_static(schema, tbl, del.indexed_by.as_ref(), del.selection.as_ref())?;
    let dml_ctes = CteStore::default().with_scope(&del.ctes);
    let scope = make_static_table_scope(tbl, del.alias.as_deref());
    let mut visited = Vec::new();

    if let Some(ref w) = del.selection {
        validate_expr_static(
            w,
            std::slice::from_ref(&scope),
            schema,
            &dml_ctes,
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    if !del.returning.is_empty() {
        let ret_scope = make_static_returning_scope(tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&del.returning, &ret_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&ret_scope),
                schema,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_select_stmt_static(
    stmt: &SelectStmt,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    parent_ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&stmt.ctes)?;
    let ctes = parent_ctes.with_scope(&stmt.ctes);

    if let Some(ref lim) = stmt.limit {
        validate_expr_static(
            &lim.limit,
            outer_scopes,
            schema,
            &ctes,
            functions,
            false,
            false,
            visited_ctes,
        )?;
        if let Some(ref off) = lim.offset {
            validate_expr_static(
                off,
                outer_scopes,
                schema,
                &ctes,
                functions,
                false,
                false,
                visited_ctes,
            )?;
        }
    }

    if stmt.body.compounds.is_empty() {
        validate_single_select_core_static(
            &stmt.body.head,
            &stmt.order_by,
            outer_scopes,
            schema,
            &ctes,
            functions,
            visited_ctes,
        )
    } else {
        let head_cols = validate_single_select_core_static(
            &stmt.body.head,
            &[],
            outer_scopes,
            schema,
            &ctes,
            functions,
            visited_ctes,
        )?;
        let mut all_arm_cols = vec![head_cols.clone()];
        for (op, rhs_core) in &stmt.body.compounds {
            let rhs_cols = validate_single_select_core_static(
                rhs_core,
                &[],
                outer_scopes,
                schema,
                &ctes,
                functions,
                visited_ctes,
            )?;
            if rhs_cols.len() != head_cols.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "SELECTs to the left and right of {} do not have the same number of result columns",
                        op.as_sql_str()
                    ),
                ));
            }
            all_arm_cols.push(rhs_cols);
        }

        let width = head_cols.len();
        for (ob_idx, ob) in stmt.order_by.iter().enumerate() {
            let mut inner = &ob.expr;
            while let Expr::Collate { expr, .. } = inner {
                inner = expr.as_ref();
            }
            if let Some(ord) = strip_collate_and_unary_plus(inner) {
                if ord < 1 || (ord as usize) > width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "{} ORDER BY term out of range - should be between 1 and {}",
                            ordinal_suffix(ob_idx + 1),
                            width
                        ),
                    ));
                }
                continue;
            }
            let target_label = match inner {
                Expr::Column(ColumnRef {
                    schema: None,
                    table: None,
                    column,
                }) => column.clone(),
                other => expr_column_label(other),
            };
            let matched = all_arm_cols
                .iter()
                .any(|arm| arm.iter().any(|cn| cn.eq_ignore_ascii_case(&target_label)));
            if !matched {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term does not match any column in the result set",
                        ordinal_suffix(ob_idx + 1)
                    ),
                ));
            }
        }
        Ok(head_cols)
    }
}

fn validate_single_select_core_static(
    core: &SelectCore,
    order_by: &[OrderByTerm],
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    match core {
        SelectCore::Values(expr_rows) => {
            let Some(first) = expr_rows.first() else {
                return Ok(Vec::new());
            };
            let width = first.len();
            for erow in expr_rows {
                if erow.len() != width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "all VALUES must have the same number of terms",
                    ));
                }
                for e in erow {
                    validate_expr_static(
                        e,
                        outer_scopes,
                        schema,
                        ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }
            let col_names: Vec<String> = (1..=width).map(|i| format!("column{i}")).collect();
            if !order_by.is_empty() {
                for (ob_idx, ob) in order_by.iter().enumerate() {
                    let mut inner = &ob.expr;
                    while let Expr::Collate { expr, .. } = inner {
                        inner = expr.as_ref();
                    }
                    if let Some(ord) = strip_collate_and_unary_plus(inner) {
                        if ord < 1 || (ord as usize) > width {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!(
                                    "{} ORDER BY term out of range - should be between 1 and {}",
                                    ordinal_suffix(ob_idx + 1),
                                    width
                                ),
                            ));
                        }
                        continue;
                    }
                    let target_label = match inner {
                        Expr::Column(ColumnRef {
                            schema: None,
                            table: None,
                            column,
                        }) => column.clone(),
                        other => expr_column_label(other),
                    };
                    if !col_names
                        .iter()
                        .any(|cn| cn.eq_ignore_ascii_case(&target_label))
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} ORDER BY term does not match any column in the result set",
                                ordinal_suffix(ob_idx + 1)
                            ),
                        ));
                    }
                }
            }
            Ok(col_names)
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let sample_cols = match from {
                None => Vec::new(),
                Some(fc) => validate_from_clause_static(
                    fc,
                    selection.as_ref(),
                    outer_scopes,
                    schema,
                    ctes,
                    functions,
                    visited_ctes,
                )?,
            };

            let (col_names, expanded_proj) = expand_projection(projection, &sample_cols)?;
            let row_eval = EvalRow {
                cols: Rc::from(sample_cols.as_slice()),
                vals: vec![Value::Null; sample_cols.len()],
            };
            let full_scopes = push_scope(outer_scopes, &row_eval);

            if let Some(where_expr) = selection {
                validate_expr_static(
                    where_expr,
                    &full_scopes,
                    schema,
                    ctes,
                    functions,
                    false,
                    false,
                    visited_ctes,
                )?;
            }

            let is_agg = !group_by.is_empty()
                || having.is_some()
                || projection.iter().any(|rc| match rc {
                    ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                    _ => false,
                })
                || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr));

            for (pe, _, _) in &expanded_proj {
                validate_expr_static(
                    pe,
                    &full_scopes,
                    schema,
                    ctes,
                    functions,
                    is_agg,
                    false,
                    visited_ctes,
                )?;
            }

            for (gi, ge) in group_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(ge) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} GROUP BY term out of range - should be between 1 and {}",
                                ordinal_suffix(gi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                } else {
                    let subst = subst_select_aliases_in_expr(
                        ge,
                        &expanded_proj,
                        &sample_cols,
                        outer_scopes,
                    );
                    validate_expr_static(
                        &subst,
                        &full_scopes,
                        schema,
                        ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }

            if let Some(hav) = having {
                let subst =
                    subst_select_aliases_in_expr(hav, &expanded_proj, &sample_cols, outer_scopes);
                validate_expr_static(
                    &subst,
                    &full_scopes,
                    schema,
                    ctes,
                    functions,
                    true,
                    false,
                    visited_ctes,
                )?;
            }

            for (oi, ob) in order_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(&ob.expr) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} ORDER BY term out of range - should be between 1 and {}",
                                ordinal_suffix(oi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                } else {
                    let subst = subst_order_by_aliases_static(
                        &ob.expr,
                        &expanded_proj,
                        &sample_cols,
                        outer_scopes,
                    );
                    validate_expr_static(
                        &subst,
                        &full_scopes,
                        schema,
                        ctes,
                        functions,
                        is_agg,
                        false,
                        visited_ctes,
                    )?;
                }
            }

            Ok(col_names)
        }
    }
}

fn subst_order_by_aliases_static(
    expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            let in_sample = sample_cols.iter().any(|sc| {
                if sc.hidden_rowid {
                    column.eq_ignore_ascii_case("rowid")
                        || column.eq_ignore_ascii_case("_rowid_")
                        || column.eq_ignore_ascii_case("oid")
                } else {
                    sc.name.eq_ignore_ascii_case(column)
                }
            });
            let in_outer = outer_scopes
                .iter()
                .rev()
                .any(|s| s.lookup(None, column).ok().flatten().is_some());
            if !in_sample
                && !in_outer
                && expanded_proj.iter().any(|(_, label, al)| {
                    al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                        || label.eq_ignore_ascii_case(column)
                })
            {
                return Expr::Null;
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            collation: collation.clone(),
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            type_name: type_name.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_order_by_aliases_static(
                left,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            op: *op,
            right: Box::new(subst_order_by_aliases_static(
                right,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::Between {
            expr: val,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            low: Box::new(subst_order_by_aliases_static(
                low,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            high: Box::new(subst_order_by_aliases_static(
                high,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: val,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            list: list
                .iter()
                .map(|e| subst_order_by_aliases_static(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
            negated: *negated,
        },
        Expr::InTable {
            expr: val,
            table,
            negated,
        } => Expr::InTable {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            table: table.clone(),
            negated: *negated,
        },
        Expr::Like {
            expr: val,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            pattern: Box::new(subst_order_by_aliases_static(
                pattern,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            escape: escape.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_order_by_aliases_static(w, expanded_proj, sample_cols, outer_scopes),
                        subst_order_by_aliases_static(t, expanded_proj, sample_cols, outer_scopes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| {
                            subst_order_by_aliases_static(
                                e,
                                expanded_proj,
                                sample_cols,
                                outer_scopes,
                            )
                        })
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_order_by_aliases_static(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

fn validate_from_clause_static(
    fc: &FromClause,
    where_hint: Option<&Expr>,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<ScopedColumn>> {
    let head_hint = if fc.joins.is_empty() {
        where_hint
    } else {
        None
    };
    let mut cur_cols = validate_tos_static(
        &fc.head,
        head_hint,
        outer_scopes,
        schema,
        ctes,
        functions,
        visited_ctes,
    )?;

    for join in &fc.joins {
        let right_cols = validate_tos_static(
            &join.right,
            None,
            outer_scopes,
            schema,
            ctes,
            functions,
            visited_ctes,
        )?;

        if join.natural && join.constraint.is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "a NATURAL join may not have an ON or USING clause",
            ));
        }

        if let Some(JoinConstraint::Using(ref ucols)) = join.constraint {
            for u in ucols {
                let in_left = cur_cols
                    .iter()
                    .any(|c| !c.hidden_rowid && c.name.eq_ignore_ascii_case(u));
                let in_right = right_cols
                    .iter()
                    .any(|c| !c.hidden_rowid && c.name.eq_ignore_ascii_case(u));
                if !in_left || !in_right {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot join using column {u} - column not present in both tables"),
                    ));
                }
            }
        }

        let merged_cols = merge_join_scoped_columns(&cur_cols, &right_cols, join);
        if let Some(JoinConstraint::On(ref on_expr)) = join.constraint {
            let join_eval = EvalRow {
                cols: Rc::from(merged_cols.as_slice()),
                vals: vec![Value::Null; merged_cols.len()],
            };
            let join_scopes = push_scope(outer_scopes, &join_eval);
            validate_expr_static(
                on_expr,
                &join_scopes,
                schema,
                ctes,
                functions,
                false,
                false,
                visited_ctes,
            )?;
        }
        cur_cols = merged_cols;
    }

    Ok(cur_cols)
}

fn validate_tos_static(
    tos: &TableOrSubquery,
    where_hint: Option<&Expr>,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table {
            name,
            alias,
            indexed_by,
        } => {
            let eff_alias = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                if indexed_by.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        match indexed_by {
                            Some(IndexHint::NotIndexed) => {
                                format!("cannot use NOT INDEXED on CTE: {}", name.name)
                            }
                            Some(IndexHint::IndexedBy(idx)) => format!("no such index: {idx}"),
                            None => unreachable!(),
                        },
                    ));
                }
                let cte_cols = resolve_cte_columns_static(
                    s_idx,
                    c_idx,
                    outer_scopes,
                    schema,
                    ctes,
                    functions,
                    visited_ctes,
                )?;
                let meta = infer_cte_meta_by_name(&name.name, outer_scopes, schema, ctes);
                return Ok(cte_cols
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: Some(eff_alias.clone()),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            merged_using: false,
                        }
                    })
                    .collect());
            }

            let (tbl, schema_lbl) = resolve_table_for_query(schema, name)?;
            validate_index_hint_static(schema, &tbl, indexed_by.as_ref(), where_hint)?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff_alias.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl.to_owned()),
                    table_alias: Some(eff_alias),
                    name: "rowid".to_owned(),
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    merged_using: false,
                });
            }
            Ok(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_cols = validate_select_stmt_static(
                query,
                outer_scopes,
                schema,
                ctes,
                functions,
                visited_ctes,
            )?;
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, schema, ctes);
            Ok(sub_cols
                .into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = sub_meta
                        .get(i)
                        .copied()
                        .unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: None,
                        table_alias: alias.clone(),
                        name: cn,
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        merged_using: false,
                    }
                })
                .collect())
        }
    }
}

fn resolve_cte_columns_static(
    s_idx: usize,
    c_idx: usize,
    outer_scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    if let Some(w) = ctes.find_working(s_idx, c_idx) {
        return Ok(w.column_names.clone());
    }
    let cte = &ctes.scopes[s_idx][c_idx];
    if visited_ctes.contains(&(s_idx, c_idx)) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        ));
    }
    visited_ctes.push((s_idx, c_idx));

    let eval_ctes = CteStore {
        scopes: ctes.scopes[..=s_idx].to_vec(),
        working: ctes.working.clone(),
        resolving: Vec::new(),
        limit_hint: None,
    };

    let res = match classify_cte_recursion(cte) {
        CteRecursionKind::InvalidCircular => Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        )),
        CteRecursionKind::MultipleRecursive => Err(SqliteError::new(
            ErrorCode::Error,
            format!("multiple recursive references: {}", cte.name),
        )),
        CteRecursionKind::NonRecursive => {
            let q_cols = validate_select_stmt_static(
                &cte.query,
                outer_scopes,
                schema,
                &eval_ctes,
                functions,
                visited_ctes,
            )?;
            if let Some(ref explicit_cols) = cte.columns {
                if explicit_cols.len() != q_cols.len() {
                    Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} values for {} columns",
                            cte.name,
                            q_cols.len(),
                            explicit_cols.len()
                        ),
                    ))
                } else {
                    Ok(explicit_cols.clone())
                }
            } else {
                Ok(q_cols)
            }
        }
        CteRecursionKind::Recursive => {
            let n_comp = cte.query.body.compounds.len();
            let seed_body = SelectBody {
                head: cte.query.body.head.clone(),
                compounds: cte.query.body.compounds[..(n_comp - 1)].to_vec(),
            };
            let seed_stmt = SelectStmt {
                ctes: cte.query.ctes.clone(),
                body: seed_body,
                order_by: Vec::new(),
                limit: None,
            };
            let seed_cols = validate_select_stmt_static(
                &seed_stmt,
                outer_scopes,
                schema,
                &eval_ctes,
                functions,
                visited_ctes,
            )?;
            let working_cols = if let Some(ref explicit_cols) = cte.columns {
                if explicit_cols.len() != seed_cols.len() {
                    visited_ctes.pop();
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} values for {} columns",
                            cte.name,
                            seed_cols.len(),
                            explicit_cols.len()
                        ),
                    ));
                }
                explicit_cols.clone()
            } else {
                seed_cols
            };

            let (rec_op, rec_core) = &cte.query.body.compounds[n_comp - 1];
            if matches!(rec_core, SelectCore::Select { distinct: true, .. })
                && *rec_op == CompoundOp::UnionAll
            {
                // Valid in SQLite (`UNION ALL` with `SELECT DISTINCT` is disallowed in recursive term? Let runtime check if needed)
            }
            if let SelectCore::Select {
                projection,
                group_by,
                having,
                ..
            } = rec_core
            {
                let has_agg = !group_by.is_empty()
                    || having.is_some()
                    || projection.iter().any(|rc| match rc {
                        ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                        _ => false,
                    });
                if has_agg {
                    visited_ctes.pop();
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "recursive aggregate queries not supported",
                    ));
                }
            }

            let mut rec_ctes = eval_ctes.with_scope(&cte.query.ctes);
            rec_ctes.working.push((
                s_idx,
                c_idx,
                QueryOutput {
                    column_names: working_cols.clone(),
                    rows: Vec::new(),
                },
            ));
            let rhs_cols = validate_single_select_core_static(
                rec_core,
                &[],
                outer_scopes,
                schema,
                &rec_ctes,
                functions,
                visited_ctes,
            )?;
            if rhs_cols.len() != working_cols.len() {
                visited_ctes.pop();
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "SELECTs to the left and right of {} do not have the same number of result columns",
                        rec_op.as_sql_str()
                    ),
                ));
            }
            if let Some(ref lim) = cte.query.limit {
                validate_expr_static(
                    &lim.limit,
                    outer_scopes,
                    schema,
                    &rec_ctes,
                    functions,
                    false,
                    false,
                    visited_ctes,
                )?;
                if let Some(ref off) = lim.offset {
                    validate_expr_static(
                        off,
                        outer_scopes,
                        schema,
                        &rec_ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }
            Ok(working_cols)
        }
    };

    visited_ctes.pop();
    res
}

#[allow(clippy::too_many_arguments)]
fn validate_expr_static(
    expr: &Expr,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    allow_agg: bool,
    inside_agg: bool,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<()> {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => Ok(()),

        Expr::Column(cref) => {
            for scope in scopes.iter().rev() {
                if scope.lookup_cref_full(cref)?.is_some() {
                    return Ok(());
                }
            }
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {}", cref.display_name()),
            ))
        }

        Expr::Collate { expr: inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => validate_expr_static(
            inner,
            scopes,
            schema,
            ctes,
            functions,
            allow_agg,
            inside_agg,
            visited_ctes,
        ),

        Expr::RowValue(elems) => {
            if elems.len() == 1 {
                validate_expr_static(
                    &elems[0],
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )
            } else {
                Err(SqliteError::new(ErrorCode::Error, "row value misused"))
            }
        }

        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Is
                    | BinaryOp::IsNot
            ) && (is_row_value_expr(left) || is_row_value_expr(right))
            {
                let lw = validate_tuple_expr_static(
                    left,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let rw = validate_tuple_expr_static(
                    right,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if lw != rw {
                    let is_left_sub = matches!(left.as_ref(), Expr::Subquery(_));
                    let is_right_sub = matches!(right.as_ref(), Expr::Subquery(_));
                    if is_left_sub && !is_right_sub {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("sub-select returns {lw} columns - expected {rw}"),
                        ));
                    }
                    if is_right_sub && !is_left_sub {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("sub-select returns {rw} columns - expected {lw}"),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                return Ok(());
            }
            validate_expr_static(
                left,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                right,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )
        }

        Expr::Between {
            expr: val_e,
            low,
            high,
            ..
        } => {
            if is_row_value_expr(val_e) || is_row_value_expr(low) || is_row_value_expr(high) {
                let vw = validate_tuple_expr_static(
                    val_e,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let lw = validate_tuple_expr_static(
                    low,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let hw = validate_tuple_expr_static(
                    high,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if vw != lw || vw != hw {
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                return Ok(());
            }
            validate_expr_static(
                val_e,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                low,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                high,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )
        }

        Expr::InList {
            expr: lhs_e, list, ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            for item in list {
                let m = validate_tuple_expr_static(
                    item,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if m != n {
                    if n >= 2 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
            }
            Ok(())
        }

        Expr::InSubquery {
            expr: lhs_e,
            subquery,
            ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            let sub_cols = validate_select_stmt_static(
                subquery,
                scopes,
                schema,
                ctes,
                functions,
                visited_ctes,
            )?;
            if !sub_cols.is_empty() && sub_cols.len() != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected {n}",
                        sub_cols.len()
                    ),
                ));
            }
            Ok(())
        }

        Expr::InTable {
            expr: lhs_e, table, ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            let m = if table.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&table.name)
            {
                resolve_cte_columns_static(
                    s_idx,
                    c_idx,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    visited_ctes,
                )?
                .len()
            } else {
                let (tbl, _) = resolve_table_for_query(schema, table)?;
                tbl.columns.len()
            };
            if m != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("sub-select returns {m} columns - expected {n}"),
                ));
            }
            Ok(())
        }

        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            ..
        } => {
            validate_expr_static(
                txt_e,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                pattern,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            if let Some(esc) = escape {
                validate_expr_static(
                    esc,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
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
                validate_expr_static(
                    op,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            for (w, t) in branches {
                validate_expr_static(
                    w,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                validate_expr_static(
                    t,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            if let Some(el) = else_expr {
                validate_expr_static(
                    el,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(())
        }

        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            let (is_star, arg_count) = match args {
                FunctionArgs::Star => (true, 0),
                FunctionArgs::Exprs(es) => (false, es.len()),
            };
            functions.validate_call(
                name,
                is_star,
                arg_count,
                *distinct,
                filter.is_some(),
                allow_agg && !inside_agg,
            )?;
            let is_agg_fn = if !is_star && functions.find_scalar(name, arg_count).is_some() {
                false
            } else {
                match name.to_ascii_uppercase().as_str() {
                    "COUNT" | "SUM" | "TOTAL" | "AVG" | "GROUP_CONCAT" | "STRING_AGG" => true,
                    "MIN" | "MAX" => arg_count == 1,
                    _ => false,
                }
            };
            if is_agg_fn {
                if inside_agg || !allow_agg {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                }
                if let FunctionArgs::Exprs(es) = args {
                    for ae in es {
                        validate_expr_static(
                            ae,
                            scopes,
                            schema,
                            ctes,
                            functions,
                            false,
                            true,
                            visited_ctes,
                        )?;
                    }
                }
                if let Some(fe) = filter {
                    validate_expr_static(
                        fe,
                        scopes,
                        schema,
                        ctes,
                        functions,
                        false,
                        true,
                        visited_ctes,
                    )?;
                }
            } else if let FunctionArgs::Exprs(es) = args {
                for ae in es {
                    validate_expr_static(
                        ae,
                        scopes,
                        schema,
                        ctes,
                        functions,
                        allow_agg,
                        inside_agg,
                        visited_ctes,
                    )?;
                }
            }
            Ok(())
        }

        Expr::Subquery(sub) => {
            let sub_cols =
                validate_select_stmt_static(sub, scopes, schema, ctes, functions, visited_ctes)?;
            if sub_cols.len() > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("sub-select returns {} columns - expected 1", sub_cols.len()),
                ));
            }
            Ok(())
        }

        Expr::Exists { subquery, .. } => {
            let _ = validate_select_stmt_static(
                subquery,
                scopes,
                schema,
                ctes,
                functions,
                visited_ctes,
            )?;
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_tuple_expr_static(
    mut expr: &Expr,
    scopes: &[EvalRow],
    schema: &Schema,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    allow_agg: bool,
    inside_agg: bool,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<usize> {
    while let Expr::Collate { expr: inner, .. } = expr {
        expr = inner;
    }
    match expr {
        Expr::RowValue(elems) => {
            for el in elems {
                validate_expr_static(
                    el,
                    scopes,
                    schema,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(elems.len())
        }
        Expr::Subquery(sub) => {
            let sub_cols =
                validate_select_stmt_static(sub, scopes, schema, ctes, functions, visited_ctes)?;
            Ok(sub_cols.len().max(1))
        }
        other => {
            validate_expr_static(
                other,
                scopes,
                schema,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            Ok(1)
        }
    }
}
