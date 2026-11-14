//! Core-independent virtual table traits and data structures (`sqlite/src/vtab.c`).
#![forbid(unsafe_code)]

use crate::{ErrorCode, Result, SqliteError, Value};

/// Security classification for a virtual table module (`SQLITE_VTAB_RISK_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VTabRiskFlags {
    /// Standard module: allowed inside views/triggers only when `PRAGMA trusted_schema = ON`.
    #[default]
    Normal,
    /// Safe inside views and triggers regardless of `PRAGMA trusted_schema`.
    Innocuous,
    /// Allowed only in direct top-level SQL; forbidden inside views and triggers.
    DirectOnly,
}

/// Configuration options passed to [`VTabHostContext::set_vtab_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VTabConfigOption {
    /// `SQLITE_VTAB_CONSTRAINT_SUPPORT`
    ConstraintSupport,
    /// `SQLITE_VTAB_INNOCUOUS`
    Innocuous,
    /// `SQLITE_VTAB_DIRECTONLY`
    DirectOnly,
}

/// Active conflict-resolution mode reported by [`VTabHostContext::vtab_on_conflict`] (`sqlite3_vtab_on_conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VTabConflictMode {
    Rollback,
    Ignore,
    Fail,
    #[default]
    Abort,
    Replace,
}

/// Virtual table constraint operator (`SQLITE_INDEX_CONSTRAINT_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VTabConstraintOp {
    Eq,
    Gt,
    Le,
    Lt,
    Ge,
    Match,
    Like,
    Glob,
    Regexp,
    Ne,
    IsNot,
    IsNotNull,
    IsNull,
    Is,
    Limit,
    Offset,
    Function(u8),
}

/// Single `WHERE` or `LIMIT`/`OFFSET` constraint presented to [`VirtualTable::best_index`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VTabConstraint {
    /// Column index (`-1` for rowid, or non-negative column index; ignored for `Limit`/`Offset`).
    pub i_column: i32,
    /// Constraint operator.
    pub op: VTabConstraintOp,
    /// True if the constraint is usable by this plan.
    pub usable: bool,
    /// True if this equality constraint originates from an `IN (...)` list.
    pub in_values: bool,
}

/// Output usage specification filled by [`VirtualTable::best_index`] for a constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VTabConstraintUsage {
    /// 1-based index into the `args` slice passed to [`VirtualCursor::filter`], or `0` if unused.
    pub argv_index: usize,
    /// If true, the outer query engine may omit post-filtering for this constraint.
    pub omit: bool,
}

/// Single `ORDER BY` term presented to [`VirtualTable::best_index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VTabOrderBy {
    /// Column index (`-1` for rowid, or non-negative column index).
    pub i_column: i32,
    /// True if `DESC`, false if `ASC`.
    pub desc: bool,
}

/// Index selection structure passed to [`VirtualTable::best_index`] (`sqlite3_index_info`).
#[derive(Debug, Clone, PartialEq)]
pub struct VTabIndexInfo {
    /// Input constraints.
    pub constraints: Vec<VTabConstraint>,
    /// Input `ORDER BY` terms.
    pub order_by: Vec<VTabOrderBy>,
    /// Output usage entries (parallel to `constraints`).
    pub constraint_usage: Vec<VTabConstraintUsage>,
    /// Module-chosen index identifier passed to [`VirtualCursor::filter`].
    pub idx_num: i32,
    /// Optional module-chosen index string passed to [`VirtualCursor::filter`].
    pub idx_str: Option<String>,
    /// True if the virtual table cursor guarantees rows are already ordered by `order_by`.
    pub order_by_consumed: bool,
    /// Estimated cost of the scan.
    pub estimated_cost: f64,
    /// Estimated row count of the scan.
    pub estimated_rows: i64,
    /// Optional index flags (`SQLITE_INDEX_SCAN_UNIQUE`, etc.).
    pub idx_flags: u32,
    /// Bitmask of columns referenced by the query (`colUsed`).
    pub col_used: u64,
    /// Module-private value handed to [`VirtualCursor::set_plan_hint`] before `filter`; it is
    /// not part of the plan the host reports (`idxNum` / `idxStr`).
    pub plan_hint: u64,
}

impl VTabIndexInfo {
    /// Creates a new `VTabIndexInfo` initialized for the given constraints and `ORDER BY` terms.
    pub fn new(constraints: Vec<VTabConstraint>, order_by: Vec<VTabOrderBy>) -> Self {
        let constraint_usage = vec![VTabConstraintUsage::default(); constraints.len()];
        Self {
            constraints,
            order_by,
            constraint_usage,
            idx_num: 0,
            idx_str: None,
            order_by_consumed: false,
            estimated_cost: 1_000_000.0,
            estimated_rows: 1_000_000,
            idx_flags: 0,
            col_used: u64::MAX,
            plan_hint: 0,
        }
    }
}

impl Default for VTabIndexInfo {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

/// Mutation operation passed to [`VirtualTable::update`] (`xUpdate`).
#[derive(Debug, Clone, PartialEq)]
pub enum VTabUpdateOperation {
    /// Delete the row identified by `old_rowid`.
    Delete { old_rowid: Value },
    /// Insert a new row with optional explicit rowid/PK and column values.
    Insert {
        new_rowid: Option<Value>,
        values: Vec<Value>,
    },
    /// Update an existing row identified by `old_rowid` with `new_rowid` and updated column values.
    Update {
        old_rowid: Value,
        new_rowid: Value,
        values: Vec<Value>,
    },
}

/// Controlled host execution context provided to virtual table callbacks during a host call.
pub trait VTabHostContext {
    /// Schema name where the virtual table resides (e.g. `"main"`, `"temp"`).
    fn db_name(&self) -> &str;
    /// Name of the virtual table instance.
    fn table_name(&self) -> &str;
    /// Declares the schema of the virtual table using a `CREATE TABLE ...` statement (`sqlite3_declare_vtab`).
    fn declare_vtab(&mut self, create_table_sql: &str) -> Result<()>;
    /// Sets virtual table configuration options (`sqlite3_vtab_config`).
    fn set_vtab_config(&mut self, opt: VTabConfigOption) -> Result<()>;
    /// Returns the active DML conflict resolution mode (`sqlite3_vtab_on_conflict`).
    fn vtab_on_conflict(&self) -> VTabConflictMode {
        VTabConflictMode::Abort
    }
    /// True if column `col_idx` of the row passed to an `Update` operation was not assigned by
    /// the UPDATE statement (`sqlite3_value_nochange`). Always false for other operations.
    fn vtab_nochange(&self, _col_idx: usize) -> bool {
        false
    }
    /// Executes an internal SQL statement within the active connection/transaction context.
    fn execute_sql(&mut self, sql: &str, params: &[Value]) -> Result<usize>;
    /// Executes an internal SQL query within the active connection/transaction context and returns all rows.
    fn query_sql(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>>;

    /// Point-reads a single row by `rowid` from a standard rowid shadow table in `self.db_name()`.
    /// Defaults to `query_sql` so custom/mock [`VTabHostContext`] implementations work unchanged.
    fn shadow_get_rowid(&mut self, shadow_table: &str, rowid: i64) -> Result<Option<Vec<Value>>> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("SELECT * FROM {db_q}.{tbl_q} WHERE rowid = ?1");
        let mut rows = self.query_sql(&sql, &[Value::Integer(rowid)])?;
        Ok(rows.pop())
    }

    /// Inserts (or replaces when `or_replace` is `true`) a single row at `rowid` in `shadow_table`.
    /// `cols` matches the declared columns of `shadow_table` (with `rowid` at the `INTEGER PRIMARY KEY` slot).
    fn shadow_put_rowid(
        &mut self,
        shadow_table: &str,
        rowid: i64,
        cols: &[Value],
        or_replace: bool,
    ) -> Result<()> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let verb = if or_replace {
            "INSERT OR REPLACE"
        } else {
            "INSERT"
        };
        if cols.is_empty() {
            let sql = format!("{verb} INTO {db_q}.{tbl_q}(rowid) VALUES (?1)");
            self.execute_sql(&sql, &[Value::Integer(rowid)])?;
            return Ok(());
        }
        let mut ph = String::with_capacity(cols.len() * 4);
        for i in 0..cols.len() {
            if i > 0 {
                ph.push_str(", ");
            }
            ph.push('?');
            ph.push_str(&(i + 1).to_string());
        }
        let sql = format!("{verb} INTO {db_q}.{tbl_q} VALUES ({ph})");
        self.execute_sql(&sql, cols)?;
        Ok(())
    }

    /// Deletes a single row by `rowid` from `shadow_table`, returning `true` if a row was removed.
    fn shadow_delete_rowid(&mut self, shadow_table: &str, rowid: i64) -> Result<bool> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("DELETE FROM {db_q}.{tbl_q} WHERE rowid = ?1");
        Ok(self.execute_sql(&sql, &[Value::Integer(rowid)])? > 0)
    }

    /// Zero-allocation fast check: returns `Ok(Some(true))` if `rowid` in `shadow_table` has a
    /// `BLOB` at `col_idx` equal to `expected`, `Ok(Some(false))` if missing or different, or
    /// `Ok(None)` if unsupported so the caller falls back to `shadow_get_rowid`.
    fn shadow_rowid_blob_eq(
        &mut self,
        _shadow_table: &str,
        _rowid: i64,
        _col_idx: usize,
        _expected: &[u8],
    ) -> Result<Option<bool>> {
        Ok(None)
    }

    /// Returns `MAX(rowid)` from `shadow_table`, or `None` if the table is empty.
    fn shadow_max_rowid(&mut self, shadow_table: &str) -> Result<Option<i64>> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("SELECT max(rowid) FROM {db_q}.{tbl_q}");
        let rows = self.query_sql(&sql, &[])?;
        match rows.first().and_then(|r| r.first()) {
            Some(Value::Integer(v)) => Ok(Some(*v)),
            _ => Ok(None),
        }
    }

    // ---------------------------------------------------------------------------------------
    // Nested module statements, as C runs them (`sqlite3_step` of a statement the module
    // prepared on the host connection). Such a statement is a top-level statement of its own:
    // when it halts, `sqlite3VdbeHalt` stores its row count in `db->nChange` and adds it to
    // `db->nTotalChange`, and an `OP_Insert` it runs sets `db->lastRowid`. The methods below
    // have exactly those effects; `execute_sql` / `shadow_put_rowid` / `shadow_delete_rowid`
    // above hide them from the connection counters.
    // ---------------------------------------------------------------------------------------

    /// [`Self::execute_sql`] whose changes are counted on the connection (`changes()`,
    /// `total_changes()`) and whose inserts set `last_insert_rowid()`, as a statement stepped
    /// by a C module does.
    fn execute_sql_counted(&mut self, sql: &str, params: &[Value]) -> Result<usize> {
        self.execute_sql(sql, params)
    }

    /// Asks virtual table `table` of schema `db_name` (default: this table's schema) to write
    /// its pending in-memory data to its shadow tables (`VirtualTable::flush_before_shadow_access`),
    /// as `fts5VocabOpenMethod` does with the FTS5 table it reads (`sqlite3Fts5FlushToDisk`).
    fn flush_vtab_pending(&mut self, _db_name: Option<&str>, _table: &str) -> Result<()> {
        Ok(())
    }

    /// `sqlite3_last_insert_rowid(db)`.
    fn last_insert_rowid(&mut self) -> i64 {
        match self.query_sql("SELECT last_insert_rowid()", &[]) {
            Ok(rows) => rows
                .first()
                .and_then(|r| r.first())
                .and_then(Value::as_i64)
                .unwrap_or(0),
            Err(_) => 0,
        }
    }

    /// `sqlite3_set_last_insert_rowid(db, rowid)`.
    fn set_last_insert_rowid(&mut self, _rowid: i64) {}

    /// Whether another statement of the connection that is still running has a cursor of
    /// this virtual table positioned on a row (it is a scan that has returned a row and has
    /// neither reached its end nor been reset).
    fn vtab_cursor_positioned(&self) -> bool {
        false
    }

    /// `sqlite3_blob_open(db, zDb, shadow_table, <column col_idx>, rowid, 0)` followed by
    /// `sqlite3_blob_bytes` and, when the size equals `out.len()`, `sqlite3_blob_read` of the
    /// whole value into `out`. `hint` is an opaque per-table cache slot owned by the caller
    /// (start it at [`ShadowHint::NONE`]); the host uses it to reach the table without looking
    /// its name up again.
    fn shadow_blob_read(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: i64,
        col_idx: usize,
        out: &mut [u8],
    ) -> Result<ShadowBlobRead> {
        let Some(row) = self.shadow_get_rowid(shadow_table, rowid)? else {
            return Ok(ShadowBlobRead::NoRow);
        };
        let bytes: &[u8] = match row.get(col_idx) {
            Some(Value::Blob(b)) => b,
            Some(Value::Text(s)) => s.as_bytes(),
            _ => return Ok(ShadowBlobRead::NotBlob),
        };
        if bytes.len() == out.len() {
            out.copy_from_slice(bytes);
        }
        Ok(ShadowBlobRead::Size(bytes.len()))
    }

    /// `sqlite3_blob_open(db, zDb, shadow_table, <column col_idx>, rowid, 1)` followed by
    /// `sqlite3_blob_write(pBlob, data, data.len(), offset)` and `sqlite3_blob_close`: the
    /// bytes of the stored value are overwritten where they are. It is not a statement, so
    /// nothing is counted. Returns `false` when there is no such row or the value is not a
    /// blob / text of at least `offset + data.len()` bytes.
    fn shadow_blob_write(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: i64,
        col_idx: usize,
        offset: usize,
        data: &[u8],
    ) -> Result<bool> {
        let Some(mut row) = self.shadow_get_rowid(shadow_table, rowid)? else {
            return Ok(false);
        };
        match row.get_mut(col_idx) {
            Some(Value::Blob(b)) if b.len() >= offset + data.len() => {
                b[offset..offset + data.len()].copy_from_slice(data);
            }
            _ => return Ok(false),
        }
        self.shadow_put_rowid(shadow_table, rowid, &row, true)?;
        Ok(true)
    }

    /// `SELECT <column col_idx> FROM shadow_table WHERE rowid = ?1` read with
    /// `sqlite3_column_int64`: `None` when there is no such row.
    fn shadow_int_read(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: i64,
        col_idx: usize,
    ) -> Result<Option<i64>> {
        let Some(row) = self.shadow_get_rowid(shadow_table, rowid)? else {
            return Ok(None);
        };
        Ok(Some(match row.get(col_idx) {
            Some(Value::Integer(i)) => *i,
            Some(Value::Real(f)) => *f as i64,
            _ => 0,
        }))
    }

    /// `INSERT INTO shadow_table VALUES(?1, ?2, ...)` (`replace`: `REPLACE INTO`) on a table
    /// whose first column is the INTEGER PRIMARY KEY and whose other columns have no declared
    /// type: `?1` is `rowid` (`None` binds NULL: a new rowid is chosen by `OP_NewRowid`), the
    /// rest `values`. `sql` is the text of that statement, used when the host cannot write
    /// the table directly. Counted as a nested statement; returns the rowid written
    /// (`sqlite3_last_insert_rowid`). An existing rowid without `replace` is SQLITE_CONSTRAINT.
    fn shadow_insert_row(
        &mut self,
        _shadow_table: &str,
        _hint: &mut ShadowHint,
        sql: &str,
        rowid: Option<i64>,
        values: &[Value],
        _replace: bool,
    ) -> Result<i64> {
        let mut row = Vec::with_capacity(values.len() + 1);
        row.push(rowid.map_or(Value::Null, Value::Integer));
        row.extend(values.iter().cloned());
        self.execute_sql_counted(sql, &row)?;
        Ok(self.last_insert_rowid())
    }

    /// `INSERT OR REPLACE INTO shadow_table VALUES(?1, ?2)` on a two-column table whose first
    /// column is the INTEGER PRIMARY KEY, with `?2` bound to a blob; `rowid == None` binds
    /// NULL (a new rowid is chosen by `OP_NewRowid`). Counted as a nested statement; returns
    /// the rowid written (`sqlite3_last_insert_rowid`).
    fn shadow_replace_blob(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: Option<i64>,
        blob: &[u8],
    ) -> Result<i64> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("INSERT OR REPLACE INTO {db_q}.{tbl_q} VALUES(?1, ?2)");
        let key = rowid.map_or(Value::Null, Value::Integer);
        self.execute_sql_counted(&sql, &[key, Value::Blob(blob.to_vec())])?;
        match rowid {
            Some(r) => Ok(r),
            None => {
                let rows = self.query_sql("SELECT last_insert_rowid()", &[])?;
                Ok(rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(Value::as_i64)
                    .unwrap_or(0))
            }
        }
    }

    /// `INSERT OR REPLACE INTO shadow_table VALUES(?1, ?2)` on a two-column table whose first
    /// column is the INTEGER PRIMARY KEY, with `?2` bound to an integer or NULL. Counted as a
    /// nested statement; returns the rowid written.
    fn shadow_replace_int(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: Option<i64>,
        value: Option<i64>,
    ) -> Result<i64> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("INSERT OR REPLACE INTO {db_q}.{tbl_q} VALUES(?1, ?2)");
        let key = rowid.map_or(Value::Null, Value::Integer);
        let val = value.map_or(Value::Null, Value::Integer);
        self.execute_sql_counted(&sql, &[key, val])?;
        match rowid {
            Some(r) => Ok(r),
            None => {
                let rows = self.query_sql("SELECT last_insert_rowid()", &[])?;
                Ok(rows
                    .first()
                    .and_then(|r| r.first())
                    .and_then(Value::as_i64)
                    .unwrap_or(0))
            }
        }
    }

    /// `DELETE FROM shadow_table WHERE rowid = ?1`, counted as a nested statement.
    fn shadow_delete_counted(
        &mut self,
        shadow_table: &str,
        _hint: &mut ShadowHint,
        rowid: i64,
    ) -> Result<()> {
        let db_q = quote_vtab_sql_ident(self.db_name());
        let tbl_q = quote_vtab_sql_ident(shadow_table);
        let sql = format!("DELETE FROM {db_q}.{tbl_q} WHERE rowid = ?1");
        self.execute_sql_counted(&sql, &[Value::Integer(rowid)])?;
        Ok(())
    }
}

/// Opaque cache slot a module keeps per shadow table for the direct shadow-table methods of
/// [`VTabHostContext`]. Only the host gives its fields a meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowHint {
    pub slot: u32,
    pub stamp: u64,
}

impl ShadowHint {
    /// A hint that refers to nothing yet.
    pub const NONE: Self = Self {
        slot: u32::MAX,
        stamp: 0,
    };
}

/// Outcome of [`VTabHostContext::shadow_blob_read`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowBlobRead {
    /// No row with that rowid (`sqlite3_blob_open` fails with SQLITE_ERROR).
    NoRow,
    /// The column holds NULL, an integer or a real ("cannot open value of type ...").
    NotBlob,
    /// Size of the value in bytes; the output buffer was filled iff it has that size.
    Size(usize),
}

fn quote_vtab_sql_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Factory and metadata trait for a virtual table module (`sqlite3_module`).
pub trait VirtualTableModule: Send + Sync + 'static {
    /// Module name as referenced in `USING <name>` or eponymous queries.
    fn name(&self) -> &str;

    /// Default security risk classification for this module.
    fn risk_flags(&self) -> VTabRiskFlags {
        VTabRiskFlags::Normal
    }

    /// True if the module can be queried directly by its module name without `CREATE VIRTUAL TABLE`.
    fn is_eponymous(&self) -> bool {
        false
    }

    /// True if the module is eponymous-only (`CREATE VIRTUAL TABLE` is forbidden).
    fn is_eponymous_only(&self) -> bool {
        false
    }

    /// Creates a new persistent virtual table (`xCreate`).
    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>>;

    /// Connects to an existing or eponymous virtual table (`xConnect`).
    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        args: &[String],
    ) -> Result<Box<dyn VirtualTable>>;

    /// Suffixes of shadow tables managed by this module (e.g. `["data", "idx", "config"]`).
    fn shadow_table_suffixes(&self) -> &[&str] {
        &[]
    }
}

/// Connected virtual table instance (`sqlite3_vtab`).
pub trait VirtualTable: Send + Sync {
    /// Evaluates query constraints and populates `info` (`xBestIndex`).
    fn best_index(&self, info: &mut VTabIndexInfo) -> Result<()>;

    /// Opens a new cursor over this virtual table (`xOpen`).
    fn open(&self) -> Result<Box<dyn VirtualCursor>>;

    /// Performs an `INSERT`, `UPDATE`, or `DELETE` operation (`xUpdate`).
    /// Returns `Ok(Some(rowid))` when inserting a row into a rowid virtual table.
    fn update(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        Err(SqliteError::new(
            ErrorCode::ReadOnly,
            "table may not be modified",
        ))
    }

    /// Begins a transaction on this virtual table (`xBegin`).
    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Two-phase commit prepare/sync hook (`xSync`).
    fn sync(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Commits a transaction on this virtual table (`xCommit`).
    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Rolls back a transaction on this virtual table (`xRollback`).
    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Starts or records savepoint `idx` (`xSavepoint`).
    fn savepoint(&mut self, _ctx: &mut dyn VTabHostContext, _idx: usize) -> Result<()> {
        Ok(())
    }

    /// Releases savepoint `idx` (`xRelease`).
    fn release(&mut self, _ctx: &mut dyn VTabHostContext, _idx: usize) -> Result<()> {
        Ok(())
    }

    /// Rolls back to savepoint `idx` (`xRollbackTo`).
    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, _idx: usize) -> Result<()> {
        Ok(())
    }

    /// Notifies the virtual table that it is being renamed to `new_name` (`xRename`).
    fn rename(&mut self, _ctx: &mut dyn VTabHostContext, _new_name: &str) -> Result<()> {
        Ok(())
    }

    /// Destroys persistent storage when `DROP TABLE` is executed (`xDestroy`).
    fn destroy(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Disconnects the in-memory virtual table instance (`xDisconnect`).
    fn disconnect(&mut self) -> Result<()> {
        Ok(())
    }

    /// `xIntegrity` (run by `PRAGMA integrity_check` / `quick_check` for a connected table):
    /// `Ok(None)` when nothing is wrong, `Ok(Some(text))` for a problem to report as a result
    /// row of the pragma, `Err` to fail the pragma.
    fn integrity(
        &mut self,
        _ctx: &mut dyn VTabHostContext,
        _schema: &str,
        _table: &str,
        _is_quick: bool,
    ) -> Result<Option<String>> {
        Ok(None)
    }

    /// Flushes any pending in-memory shadow mutations before external SQL reads or modifies a shadow table.
    fn flush_before_shadow_access(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        Ok(())
    }

    /// Invalidates any in-memory shadow-table cache when external SQL touches shadow tables directly.
    fn invalidate_shadow_cache(&mut self) {}
}

/// Cursor over a virtual table (`sqlite3_vtab_cursor`).
pub trait VirtualCursor: Send + Sync {
    /// Initializes or resets the cursor scan with arguments selected by `best_index` (`xFilter`).
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()>;

    /// Receives [`VTabIndexInfo::plan_hint`] of the chosen plan right before `filter`.
    fn set_plan_hint(&mut self, _hint: u64) {}

    /// Advances the cursor to the next row (`xNext`).
    fn next(&mut self) -> Result<()>;

    /// Returns true when the cursor has no current row (`xEof`).
    fn eof(&self) -> bool;

    /// Returns the value of column `col_idx` for the current row (`xColumn`).
    fn column(&self, col_idx: usize) -> Result<Value>;

    /// Returns the ephemeral result subtype (`sqlite3_result_subtype`) for column `col_idx`
    /// of the current row (`0` when no subtype applies, e.g. `74` for JSON values).
    fn column_subtype(&self, _col_idx: usize) -> u8 {
        0
    }

    /// Returns the 64-bit rowid for the current row (`xRowid`).
    fn rowid(&self) -> Result<i64>;
}
