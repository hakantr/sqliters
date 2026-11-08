//! `sqliters-core`: High-level SQLite 3.54.1 database engine, schema catalog, compiler,
//! and public `Connection` / `Statement` / `Row` API (`main.c`, `legacy.c`, `vdbeapi.c`).

pub mod compiler;
pub mod engine;
pub mod schema;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_pager::TransactionState;
use sqliters_syntax::{
    BindParam, Expr, FromClause, FunctionArgs, InsertSource, JoinConstraint, ResultColumn,
    SelectCore, SelectStmt, Stmt, TableOrSubquery, parse_one,
};
use sqliters_types::{ErrorCode, Pgno, Result, SqliteError, Value};
use sqliters_vdbe::{VdbeProgram, VdbeVm, VmRuntimeContext, VmStep};
use sqliters_vfs::{OpenFlags, Vfs};

pub use engine::QueryOutput;
pub use schema::{ColumnSchema, ForeignKeySchema, IndexSchema, Schema, TableSchema};

/// Single result row returned by a query (`sqlite3_column_*`).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[inline]
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&Value> {
        self.values.get(idx)
    }

    #[inline]
    pub fn get_i64(&self, idx: usize) -> Option<i64> {
        self.values.get(idx).and_then(Value::as_i64)
    }

    #[inline]
    pub fn get_f64(&self, idx: usize) -> Option<f64> {
        self.values.get(idx).and_then(Value::as_f64)
    }

    #[inline]
    pub fn get_str(&self, idx: usize) -> Option<&str> {
        self.values.get(idx).and_then(Value::as_str)
    }

    #[inline]
    pub fn get_blob(&self, idx: usize) -> Option<&[u8]> {
        match self.values.get(idx)? {
            Value::Blob(b) => Some(b.as_slice()),
            _ => None,
        }
    }
}

impl std::ops::Index<usize> for Row {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        &self.values[index]
    }
}

/// Complete result set of a query (`column_names` + `rows`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub column_names: Vec<String>,
    pub rows: Vec<Row>,
}

/// Outcome of [`Statement::step`] (`SQLITE_ROW` vs `SQLITE_DONE`).
#[derive(Debug, Clone, PartialEq)]
pub enum StepResult {
    Row(Row),
    Done,
}

/// SQLite database handle (`sqlite3*` in `sqlite.h.in`).
pub struct Connection {
    btree: Btree,
    schema: Schema,
    schema_loaded: bool,
    runtime: VmRuntimeContext,
    explicit_tx: bool,
    implicit_savepoint_tx: bool,
    foreign_keys: bool,
    count_changes: bool,
    rollback_count: u64,
    deferred_fk_suspects: Vec<(String, i64, usize)>,
}

impl Connection {
    /// Opens or creates a SQLite Format 3 database file on disk (or `:memory:` if `path` is `":memory:"` or `""`).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let p = path.as_ref();
        if p == Path::new(":memory:") || p.as_os_str().is_empty() {
            return Self::open_in_memory();
        }
        Self::open_with_flags(p, OpenFlags::READWRITE | OpenFlags::CREATE)
    }

    /// Opens an isolated in-memory database.
    pub fn open_in_memory() -> Result<Self> {
        let btree = Btree::open_in_memory()?;
        Self::from_btree(btree)
    }

    /// Opens a database file with explicit [`OpenFlags`] (e.g., `OpenFlags::READONLY`).
    pub fn open_with_flags(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        let btree = Btree::open_file(path, flags)?;
        Self::from_btree(btree)
    }

    /// Opens a database backed by a custom [`Vfs`].
    pub fn open_with_vfs(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
    ) -> Result<Self> {
        let btree = Btree::open_with_vfs(vfs, path, flags)?;
        Self::from_btree(btree)
    }

    fn from_btree(mut btree: Btree) -> Result<Self> {
        // Eagerly load schema when possible; if another connection currently holds an EXCLUSIVE lock,
        // defer schema loading until the first statement (`sqlite3_open` behavior).
        let (schema, schema_loaded) = match Schema::load(&mut btree) {
            Ok(s) => (s, true),
            Err(e) if e.code == ErrorCode::Busy => (Schema::default(), false),
            Err(e) => return Err(e),
        };
        Ok(Self {
            btree,
            schema,
            schema_loaded,
            runtime: VmRuntimeContext::default(),
            explicit_tx: false,
            implicit_savepoint_tx: false,
            foreign_keys: false,
            count_changes: false,
            rollback_count: 0,
            deferred_fk_suspects: Vec::new(),
        })
    }

    fn ensure_schema_loaded(&mut self) -> Result<()> {
        if !self.schema_loaded
            || (self.btree.pager().page_count() > 0
                && self.btree.pager().header().schema_cookie != self.schema.schema_cookie)
        {
            self.schema = Schema::load(&mut self.btree)?;
            self.schema_loaded = true;
        }
        Ok(())
    }

    /// Executes one or more semicolon-separated SQL statements (`sqlite3_exec`) and returns `self.changes()`.
    pub fn execute(&mut self, sql: &str) -> Result<u64> {
        self.execute_batch(sql)?;
        Ok(self.changes())
    }

    /// Executes a single parameterized SQL statement and returns `self.changes()`.
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let (maybe_stmt, _) = parse_one(sql)?;
        if let Some(stmt) = maybe_stmt {
            self.run_stmt(&stmt, params)?;
        }
        Ok(self.changes())
    }

    /// Executes an arbitrary multi-statement SQL script (`sqlite3_exec`).
    pub fn execute_batch(&mut self, mut sql: &str) -> Result<()> {
        while !sql.is_empty() {
            let (maybe_stmt, tail) = parse_one(sql)?;
            if let Some(stmt) = maybe_stmt {
                self.run_stmt(&stmt, &[])?;
            }
            if tail.len() == sql.len() {
                break;
            }
            sql = tail;
        }
        Ok(())
    }

    /// Executes a query with optional positional parameters and returns all columns & rows.
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let mut stmt = self.prepare(sql)?;
        stmt.bind_all(params)?;
        let rows = stmt.collect_rows()?;
        let cols = stmt.column_names().to_vec();
        Ok(QueryResult {
            column_names: cols,
            rows,
        })
    }

    /// Executes a query and returns the first [`Row`] if any (`None` if 0 rows).
    pub fn query_row(&mut self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        let mut stmt = self.prepare(sql)?;
        stmt.bind_all(params)?;
        match stmt.step()? {
            StepResult::Row(r) => Ok(Some(r)),
            StepResult::Done => Ok(None),
        }
    }

    /// Compiles the first SQL statement in `sql` into a steppable [`Statement`] (`sqlite3_prepare_v2`).
    pub fn prepare(&mut self, sql: &str) -> Result<Statement<'_>> {
        let (stmt_opt, tail) = parse_one(sql)?;
        self.ensure_schema_loaded()?;
        Statement::new(self, stmt_opt, tail.to_owned())
    }

    /// Reloads the in-memory schema catalog from Page 1 (`sqlite_schema`).
    pub fn reload_schema(&mut self) -> Result<()> {
        self.schema = Schema::load(&mut self.btree)?;
        self.schema_loaded = true;
        Ok(())
    }

    /// Rowid of the most recent successful `INSERT` into a rowid table (`sqlite3_last_insert_rowid`).
    #[inline]
    pub const fn last_insert_rowid(&self) -> i64 {
        self.runtime.last_insert_rowid
    }

    /// Number of rows modified by the most recent `INSERT`, `UPDATE`, or `DELETE` (`sqlite3_changes64`).
    #[inline]
    pub const fn changes(&self) -> u64 {
        self.runtime.last_changes
    }

    /// Total number of rows modified since this connection was opened (`sqlite3_total_changes64`).
    #[inline]
    pub const fn total_changes(&self) -> u64 {
        self.runtime.total_changes
    }

    /// Returns `true` if the connection is in autocommit mode (`sqlite3_get_autocommit`).
    #[inline]
    pub const fn is_autocommit(&self) -> bool {
        !self.explicit_tx && !self.implicit_savepoint_tx
    }

    /// Monotonic counter incremented on every transaction-level `ROLLBACK` (`sqlite3RollbackAll`).
    #[inline]
    pub const fn rollback_count(&self) -> u64 {
        self.rollback_count
    }

    #[inline]
    pub const fn is_readonly(&self) -> bool {
        self.btree.pager().is_readonly()
    }

    #[inline]
    pub fn db_path(&self) -> Option<&Path> {
        self.btree.pager().db_path()
    }

    #[inline]
    pub const fn page_size(&self) -> u32 {
        self.btree.pager().page_size()
    }

    #[inline]
    pub const fn page_count(&self) -> Pgno {
        self.btree.pager().page_count()
    }

    #[inline]
    pub const fn schema(&self) -> &Schema {
        &self.schema
    }

    #[inline]
    pub fn btree_mut(&mut self) -> &mut Btree {
        &mut self.btree
    }

    fn run_stmt(&mut self, stmt: &Stmt, params: &[Value]) -> Result<QueryOutput> {
        self.ensure_schema_loaded()?;
        let prev_ts = self.runtime.stmt_timestamp_ms;
        if prev_ts.is_none() {
            self.runtime.stmt_timestamp_ms = Some(self.btree.pager().vfs().current_unix_millis());
        }
        let res = {
            let mut ctx = engine::ExecContext {
                btree: &mut self.btree,
                schema: &mut self.schema,
                runtime: &mut self.runtime,
                explicit_tx: &mut self.explicit_tx,
                implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
                foreign_keys: &mut self.foreign_keys,
                count_changes: &mut self.count_changes,
                rollback_count: &mut self.rollback_count,
                deferred_fk_suspects: &mut self.deferred_fk_suspects,
            };
            engine::execute_statement(&mut ctx, stmt, params)
        };
        self.runtime.stmt_timestamp_ms = prev_ts;
        res
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if self.btree.transaction_state() != TransactionState::Open {
            let _ = self.btree.rollback();
        }
    }
}

enum StatementExecState {
    Unstarted,
    Vdbe {
        vm: VdbeVm,
        started_autocommit_read: bool,
    },
    Materialized {
        rows: std::vec::IntoIter<Vec<Value>>,
    },
    Finished,
}

/// Prepared SQL statement (`sqlite3_stmt` in `sqlite.h.in`).
pub struct Statement<'conn> {
    conn: &'conn mut Connection,
    stmt: Option<Stmt>,
    compiled_vdbe: Option<VdbeProgram>,
    column_names: Vec<String>,
    params: Vec<Value>,
    named_params: HashMap<String, usize>,
    unparsed_tail: String,
    state: StatementExecState,
}

impl<'conn> Statement<'conn> {
    fn new(
        conn: &'conn mut Connection,
        stmt: Option<Stmt>,
        unparsed_tail: String,
    ) -> Result<Self> {
        let mut max_param = 0usize;
        let mut named = HashMap::new();
        if let Some(ref s) = stmt {
            collect_stmt_params(s, &mut max_param, &mut named);
        }

        let mut compiled_vdbe = None;
        let mut column_names = Vec::new();
        if let Some(Stmt::Select(ref sel)) = stmt
            && let Some(prog) = compiler::try_compile_select(sel, &conn.schema)?
        {
            column_names.clone_from(&prog.column_names);
            compiled_vdbe = Some(prog);
        } else if let Some(ref s) = stmt {
            column_names = infer_stmt_column_names(s, &conn.schema);
        }

        Ok(Self {
            conn,
            stmt,
            compiled_vdbe,
            column_names,
            params: vec![Value::Null; max_param],
            named_params: named,
            unparsed_tail,
            state: StatementExecState::Unstarted,
        })
    }

    /// Remaining uncompiled SQL text after the first semicolon (`pzTail` in `sqlite3_prepare_v2`).
    #[inline]
    pub fn tail(&self) -> &str {
        &self.unparsed_tail
    }

    /// Number of bind parameters in this statement (`sqlite3_bind_parameter_count`).
    #[inline]
    pub fn parameter_count(&self) -> usize {
        self.params.len()
    }

    /// Named parameter map (`:name` / `$name` / `@name` -> 1-based parameter index).
    #[inline]
    pub fn named_parameters(&self) -> &HashMap<String, usize> {
        &self.named_params
    }

    /// Binds a 1-based parameter (`?1`, `?`, etc.) (`sqlite3_bind_*`).
    pub fn bind(&mut self, index_1_based: usize, value: impl Into<Value>) -> Result<&mut Self> {
        if index_1_based == 0 || index_1_based > self.params.len() {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!("column/bind index {index_1_based} out of range"),
            ));
        }
        self.params[index_1_based - 1] = value.into();
        Ok(self)
    }

    /// Binds a named parameter (`:name`, `@name`, or `$name`).
    pub fn bind_named(&mut self, name: &str, value: impl Into<Value>) -> Result<&mut Self> {
        let Some(&idx) = self.named_params.get(name) else {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!("unknown bind parameter: {name}"),
            ));
        };
        self.bind(idx, value)
    }

    /// Binds a slice of positional parameters (`1..=values.len()`).
    pub fn bind_all(&mut self, values: &[Value]) -> Result<&mut Self> {
        if values.len() > self.params.len() {
            self.params.resize(values.len(), Value::Null);
        }
        for (i, v) in values.iter().enumerate() {
            self.params[i] = v.clone();
        }
        Ok(self)
    }

    /// Result column names (`sqlite3_column_name`).
    #[inline]
    pub fn column_names(&self) -> &[String] {
        &self.column_names
    }

    /// Resets execution back to the start while preserving bound parameter values (`sqlite3_reset`).
    pub fn reset(&mut self) {
        self.finish_vdbe_read_if_needed();
        self.state = StatementExecState::Unstarted;
    }

    /// Clears all bound parameters back to `NULL` (`sqlite3_clear_bindings`).
    pub fn clear_bindings(&mut self) {
        self.params.fill(Value::Null);
    }

    /// Advances statement execution by one step (`sqlite3_step`).
    pub fn step(&mut self) -> Result<StepResult> {
        let Some(ref stmt) = self.stmt else {
            self.state = StatementExecState::Finished;
            return Ok(StepResult::Done);
        };

        if matches!(self.state, StatementExecState::Unstarted) {
            if let Some(ref prog) = self.compiled_vdbe {
                let started_read = self.conn.btree.transaction_state() == TransactionState::Open
                    && self.conn.is_autocommit();
                let mut vm_rt = self.conn.runtime;
                vm_rt.stmt_timestamp_ms =
                    Some(self.conn.btree.pager().vfs().current_unix_millis());
                let vm = VdbeVm::new(prog.clone(), self.params.clone(), vm_rt);
                self.state = StatementExecState::Vdbe {
                    vm,
                    started_autocommit_read: started_read,
                };
            } else {
                let stmt_clone = stmt.clone();
                let out = self.conn.run_stmt(&stmt_clone, &self.params)?;
                if !out.column_names.is_empty() {
                    self.column_names = out.column_names;
                }
                self.state = StatementExecState::Materialized {
                    rows: out.rows.into_iter(),
                };
            }
        }

        match &mut self.state {
            StatementExecState::Vdbe {
                vm,
                started_autocommit_read,
            } => match vm.step(&mut self.conn.btree) {
                Ok(VmStep::Row(vals)) => {
                    self.conn.runtime = vm.runtime_ctx;
                    self.conn.runtime.stmt_timestamp_ms = None;
                    Ok(StepResult::Row(Row::new(vals)))
                }
                Ok(VmStep::Done) => {
                    self.conn.runtime = vm.runtime_ctx;
                    self.conn.runtime.stmt_timestamp_ms = None;
                    if *started_autocommit_read
                        && self.conn.is_autocommit()
                        && self.conn.btree.transaction_state() == TransactionState::Read
                    {
                        let _ = self.conn.btree.commit();
                    }
                    self.state = StatementExecState::Finished;
                    Ok(StepResult::Done)
                }
                Err(e) => {
                    self.conn.runtime.stmt_timestamp_ms = None;
                    if *started_autocommit_read
                        && self.conn.is_autocommit()
                        && self.conn.btree.transaction_state() == TransactionState::Read
                    {
                        let _ = self.conn.btree.commit();
                    }
                    self.state = StatementExecState::Finished;
                    Err(e)
                }
            },
            StatementExecState::Materialized { rows } => {
                if let Some(vals) = rows.next() {
                    Ok(StepResult::Row(Row::new(vals)))
                } else {
                    self.state = StatementExecState::Finished;
                    Ok(StepResult::Done)
                }
            }
            StatementExecState::Finished => Ok(StepResult::Done),
            StatementExecState::Unstarted => unreachable!(),
        }
    }

    /// Steps through all remaining rows and collects them into a `Vec<Row>`.
    pub fn collect_rows(&mut self) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        while let StepResult::Row(r) = self.step()? {
            rows.push(r);
        }
        Ok(rows)
    }

    fn finish_vdbe_read_if_needed(&mut self) {
        if let StatementExecState::Vdbe {
            started_autocommit_read: true,
            ..
        } = self.state
            && self.conn.is_autocommit()
            && self.conn.btree.transaction_state() == TransactionState::Read
        {
            let _ = self.conn.btree.commit();
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        self.finish_vdbe_read_if_needed();
    }
}

// ============================================================================
// Prepare-Time Result Column Name Inference (`sqlite3_column_name`)
// ============================================================================

fn infer_stmt_column_names(stmt: &Stmt, schema: &Schema) -> Vec<String> {
    match stmt {
        Stmt::Explain { query_plan, .. } => {
            if *query_plan {
                vec![
                    "id".into(),
                    "parent".into(),
                    "notused".into(),
                    "detail".into(),
                ]
            } else {
                vec![
                    "addr".into(),
                    "opcode".into(),
                    "p1".into(),
                    "p2".into(),
                    "p3".into(),
                    "p4".into(),
                    "p5".into(),
                    "comment".into(),
                ]
            }
        }
        Stmt::Select(sel) => infer_select_column_names(sel, schema, &[]),
        Stmt::Insert(ins) => infer_returning_column_names(&ins.table, &ins.returning, schema),
        Stmt::Update(upd) => infer_returning_column_names(&upd.table, &upd.returning, schema),
        Stmt::Delete(del) => infer_returning_column_names(&del.table, &del.returning, schema),
        Stmt::Pragma { name, value, .. } => infer_pragma_column_names(name, value.is_some()),
        _ => Vec::new(),
    }
}

fn infer_select_column_names(
    sel: &SelectStmt,
    schema: &Schema,
    outer_ctes: &[(String, Vec<String>)],
) -> Vec<String> {
    let mut ctes = outer_ctes.to_vec();
    for cte in &sel.ctes {
        let cols = cte
            .columns
            .clone()
            .unwrap_or_else(|| infer_select_column_names(&cte.query, schema, &ctes));
        ctes.push((cte.name.clone(), cols));
    }
    infer_select_core_column_names(&sel.body.head, schema, &ctes)
}

fn infer_select_core_column_names(
    core: &SelectCore,
    schema: &Schema,
    ctes: &[(String, Vec<String>)],
) -> Vec<String> {
    match core {
        SelectCore::Values(rows) => {
            let width = rows.first().map_or(0, Vec::len);
            (1..=width).map(|i| format!("column{i}")).collect()
        }
        SelectCore::Select {
            projection, from, ..
        } => {
            let from_cols = from
                .as_ref()
                .map(|fc| infer_from_columns(fc, schema, ctes))
                .unwrap_or_default();
            expand_result_column_names(projection, &from_cols)
        }
    }
}

fn infer_from_columns(
    fc: &FromClause,
    schema: &Schema,
    ctes: &[(String, Vec<String>)],
) -> Vec<(Option<String>, String)> {
    let mut left = infer_tos_columns(&fc.head, schema, ctes);
    for j in &fc.joins {
        let right = infer_tos_columns(&j.right, schema, ctes);
        let using_cols: Vec<String> = if j.natural {
            let mut shared = Vec::new();
            for (_, lname) in &left {
                if right.iter().any(|(_, rname)| rname.eq_ignore_ascii_case(lname))
                    && !shared.iter().any(|s: &String| s.eq_ignore_ascii_case(lname))
                {
                    shared.push(lname.clone());
                }
            }
            shared
        } else if let Some(JoinConstraint::Using(cols)) = &j.constraint {
            cols.clone()
        } else {
            Vec::new()
        };
        for (ralias, rname) in right {
            if using_cols.iter().any(|u| u.eq_ignore_ascii_case(&rname)) {
                continue;
            }
            left.push((ralias, rname));
        }
    }
    left
}

fn infer_tos_columns(
    tos: &TableOrSubquery,
    schema: &Schema,
    ctes: &[(String, Vec<String>)],
) -> Vec<(Option<String>, String)> {
    match tos {
        TableOrSubquery::Table { name, alias } => {
            let eff = alias.clone().unwrap_or_else(|| name.clone());
            if let Some((_, cte_cols)) = ctes.iter().rev().find(|(n, _)| n.eq_ignore_ascii_case(name))
            {
                return cte_cols
                    .iter()
                    .map(|c| (Some(eff.clone()), c.clone()))
                    .collect();
            }
            if let Some(tbl) = schema.find_table(name) {
                return tbl
                    .columns
                    .iter()
                    .map(|c| (Some(eff.clone()), c.name.clone()))
                    .collect();
            }
            Vec::new()
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_cols = infer_select_column_names(query, schema, ctes);
            sub_cols
                .into_iter()
                .map(|c| (alias.clone(), c))
                .collect()
        }
    }
}

fn expand_result_column_names(
    projection: &[ResultColumn],
    from_cols: &[(Option<String>, String)],
) -> Vec<String> {
    let mut names = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for (_, cname) in from_cols {
                    names.push(cname.clone());
                }
            }
            ResultColumn::TableStar(tname) => {
                for (talias, cname) in from_cols {
                    if talias
                        .as_ref()
                        .is_some_and(|ta| ta.eq_ignore_ascii_case(tname))
                    {
                        names.push(cname.clone());
                    }
                }
            }
            ResultColumn::Expr { expr, alias } => {
                names.push(
                    alias
                        .clone()
                        .unwrap_or_else(|| compiler::expr_column_label(expr)),
                );
            }
        }
    }
    names
}

fn infer_returning_column_names(
    table_name: &str,
    returning: &[ResultColumn],
    schema: &Schema,
) -> Vec<String> {
    if returning.is_empty() {
        return Vec::new();
    }
    let from_cols: Vec<(Option<String>, String)> = schema
        .find_table(table_name)
        .map(|tbl| {
            tbl.columns
                .into_iter()
                .map(|c| (Some(table_name.to_owned()), c.name))
                .collect()
        })
        .unwrap_or_default();
    expand_result_column_names(returning, &from_cols)
}

fn infer_pragma_column_names(name: &str, has_arg: bool) -> Vec<String> {
    let upper = name.to_ascii_uppercase();
    match upper.as_str() {
        "INTEGRITY_CHECK" | "QUICK_CHECK" => vec!["integrity_check".into()],
        "JOURNAL_MODE" => vec!["journal_mode".into()],
        "PAGE_SIZE" | "PAGE_COUNT" | "FREELIST_COUNT" | "USER_VERSION" | "APPLICATION_ID"
        | "SCHEMA_VERSION" | "ENCODING" | "FOREIGN_KEYS" | "SYNCHRONOUS" | "CACHE_SIZE" => {
            if has_arg {
                Vec::new()
            } else {
                vec![upper.to_ascii_lowercase()]
            }
        }
        "DATABASE_LIST" => vec!["seq".into(), "name".into(), "file".into()],
        "TABLE_INFO" if has_arg => vec![
            "cid".into(),
            "name".into(),
            "type".into(),
            "notnull".into(),
            "dflt_value".into(),
            "pk".into(),
        ],
        "INDEX_LIST" if has_arg => vec![
            "seq".into(),
            "name".into(),
            "unique".into(),
            "origin".into(),
            "partial".into(),
        ],
        "INDEX_INFO" if has_arg => vec!["seqno".into(), "cid".into(), "name".into()],
        "FOREIGN_KEY_LIST" if has_arg => vec![
            "id".into(),
            "seq".into(),
            "table".into(),
            "from".into(),
            "to".into(),
            "on_update".into(),
            "on_delete".into(),
            "match".into(),
        ],
        "FOREIGN_KEY_CHECK" => vec![
            "table".into(),
            "rowid".into(),
            "parent".into(),
            "fkid".into(),
        ],
        "COMPILE_OPTIONS" => vec!["compile_options".into()],
        _ => Vec::new(),
    }
}

// ============================================================================
// AST Bind Parameter Discovery
// ============================================================================

fn collect_stmt_params(
    stmt: &Stmt,
    max_idx: &mut usize,
    named: &mut HashMap<String, usize>,
) {
    match stmt {
        Stmt::Explain { stmt: inner, .. } => collect_stmt_params(inner, max_idx, named),
        Stmt::Select(sel) => collect_select_params(sel, max_idx, named),
        Stmt::Insert(ins) => {
            for cte in &ins.ctes {
                collect_select_params(&cte.query, max_idx, named);
            }
            match &ins.source {
                InsertSource::Values(rows) => {
                    for r in rows {
                        for e in r {
                            collect_expr_params(e, max_idx, named);
                        }
                    }
                }
                InsertSource::Select(sel) => collect_select_params(sel, max_idx, named),
                InsertSource::DefaultValues => {}
            }
            for rc in &ins.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_params(expr, max_idx, named);
                }
            }
        }
        Stmt::Update(upd) => {
            for cte in &upd.ctes {
                collect_select_params(&cte.query, max_idx, named);
            }
            for (_, e) in &upd.assignments {
                collect_expr_params(e, max_idx, named);
            }
            if let Some(w) = &upd.selection {
                collect_expr_params(w, max_idx, named);
            }
            for rc in &upd.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_params(expr, max_idx, named);
                }
            }
        }
        Stmt::Delete(del) => {
            for cte in &del.ctes {
                collect_select_params(&cte.query, max_idx, named);
            }
            if let Some(w) = &del.selection {
                collect_expr_params(w, max_idx, named);
            }
            for rc in &del.returning {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_params(expr, max_idx, named);
                }
            }
        }
        _ => {}
    }
}

fn collect_select_params(
    sel: &SelectStmt,
    max_idx: &mut usize,
    named: &mut HashMap<String, usize>,
) {
    for cte in &sel.ctes {
        collect_select_params(&cte.query, max_idx, named);
    }
    collect_core_params(&sel.body.head, max_idx, named);
    for (_, core) in &sel.body.compounds {
        collect_core_params(core, max_idx, named);
    }
    for ob in &sel.order_by {
        collect_expr_params(&ob.expr, max_idx, named);
    }
    if let Some(lim) = &sel.limit {
        collect_expr_params(&lim.limit, max_idx, named);
        if let Some(off) = &lim.offset {
            collect_expr_params(off, max_idx, named);
        }
    }
}

fn collect_core_params(
    core: &SelectCore,
    max_idx: &mut usize,
    named: &mut HashMap<String, usize>,
) {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_expr_params(e, max_idx, named);
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
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_params(expr, max_idx, named);
                }
            }
            if let Some(fc) = from {
                if let TableOrSubquery::Subquery { query, .. } = &fc.head {
                    collect_select_params(query, max_idx, named);
                }
                for j in &fc.joins {
                    if let TableOrSubquery::Subquery { query, .. } = &j.right {
                        collect_select_params(query, max_idx, named);
                    }
                    if let Some(sqliters_syntax::JoinConstraint::On(e)) = &j.constraint {
                        collect_expr_params(e, max_idx, named);
                    }
                }
            }
            if let Some(w) = selection {
                collect_expr_params(w, max_idx, named);
            }
            for g in group_by {
                collect_expr_params(g, max_idx, named);
            }
            if let Some(h) = having {
                collect_expr_params(h, max_idx, named);
            }
        }
    }
}

fn collect_expr_params(
    expr: &Expr,
    max_idx: &mut usize,
    named: &mut HashMap<String, usize>,
) {
    match expr {
        Expr::Bind(bp) => match bp {
            BindParam::Anonymous(i) | BindParam::Numbered(i) => {
                *max_idx = (*max_idx).max(*i);
            }
            BindParam::Named(name, i) => {
                *max_idx = (*max_idx).max(*i);
                named.insert(name.clone(), *i);
            }
        },
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => collect_expr_params(inner, max_idx, named),
        Expr::Binary { left, right, .. } => {
            collect_expr_params(left, max_idx, named);
            collect_expr_params(right, max_idx, named);
        }
        Expr::Between {
            expr: v, low, high, ..
        } => {
            collect_expr_params(v, max_idx, named);
            collect_expr_params(low, max_idx, named);
            collect_expr_params(high, max_idx, named);
        }
        Expr::InList { expr: v, list, .. } => {
            collect_expr_params(v, max_idx, named);
            for item in list {
                collect_expr_params(item, max_idx, named);
            }
        }
        Expr::InSubquery {
            expr: v, subquery, ..
        } => {
            collect_expr_params(v, max_idx, named);
            collect_select_params(subquery, max_idx, named);
        }
        Expr::Like {
            expr: v,
            pattern,
            escape,
            ..
        } => {
            collect_expr_params(v, max_idx, named);
            collect_expr_params(pattern, max_idx, named);
            if let Some(esc) = escape {
                collect_expr_params(esc, max_idx, named);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_expr_params(op, max_idx, named);
            }
            for (w, t) in branches {
                collect_expr_params(w, max_idx, named);
                collect_expr_params(t, max_idx, named);
            }
            if let Some(el) = else_expr {
                collect_expr_params(el, max_idx, named);
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    collect_expr_params(a, max_idx, named);
                }
            }
            if let Some(f) = filter {
                collect_expr_params(f, max_idx, named);
            }
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            collect_select_params(sub, max_idx, named);
        }
        Expr::RowValue(elems) => {
            for e in elems {
                collect_expr_params(e, max_idx, named);
            }
        }
        _ => {}
    }
}
