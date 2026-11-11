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
    /// Executes an internal SQL statement within the active connection/transaction context.
    fn execute_sql(&mut self, sql: &str, params: &[Value]) -> Result<usize>;
    /// Executes an internal SQL query within the active connection/transaction context and returns all rows.
    fn query_sql(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>>;
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
