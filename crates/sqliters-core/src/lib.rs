//! `sqliters-core`: High-level SQLite 3.54.1 database engine, schema catalog, compiler,
//! and public `Connection` / `Statement` / `Row` API (`main.c`, `legacy.c`, `vdbeapi.c`).
#![forbid(unsafe_code)]

pub mod backup;
pub mod blob;
pub mod compiler;
pub mod db_set;
pub mod driver;
pub mod engine;
pub mod execution_context;
pub mod fast_read;
pub mod schema;
pub mod trigger;
pub mod vtab;
pub mod window;

pub use backup::{Backup, BackupStepResult};
pub use blob::Blob;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use sqliters_btree::{Btree, BtreeCursorState, CursorId, PointLookupRow, TableRowRef};
pub use sqliters_ext::{
    Fts5AuxRegistry, Fts5Module, Fts5VocabModule, GenerateSeriesModule, JsonEachModule,
    JsonTreeModule, JsonbEachModule, JsonbTreeModule, RTreeCoordMode, RTreeModule,
};
pub use sqliters_pager::{
    AutoVacuumMode, DatabaseHeader, JournalMode, LockingMode, WalCheckpointMode,
    WalCheckpointResult,
};
use sqliters_pager::{PagerOpenOptions, TransactionState};
use sqliters_syntax::{
    BindParam, Expr, FromClause, FunctionArgs, InsertSource, ResultColumn, SelectCore, SelectStmt,
    Stmt, TableOrSubquery, UpsertAction, WindowFrameBound, WindowRef, WindowSpec, parse_one,
};
pub use sqliters_syntax::{ColumnRef, QualifiedName};
pub use sqliters_types::{
    AggregateStepId, CustomCollation, DatabaseId, ExecutionServices, FunctionFlags, MAX_DB_SLOTS,
    RowMutationEvent, RowMutationOp, RuntimeValue, SQLITE_MAX_ATTACHED, VTabConfigOption,
    VTabConflictMode, VTabConstraint, VTabConstraintOp, VTabConstraintUsage, VTabHostContext,
    VTabIndexInfo, VTabOrderBy, VTabRiskFlags, VTabUpdateOperation, ValueRef, VirtualCursor,
    VirtualTable, VirtualTableModule,
};
use sqliters_types::{
    ConflictResolutionEffect, DatabaseKind, ErrorCode, InlineRow, InlineVal, LockGeometry, Pgno,
    Result, SQLITE_DEFAULT_PAGE_SIZE, SqliteError, TextEncoding, TypeAffinity, Value,
    encode_value_refs_record_into, extended,
};
pub use sqliters_vdbe::BtreeAccess;
use sqliters_vdbe::{
    InsertValSource, Opcode, RowidKeySource, SingleRowDeletePlan, SingleRowInsertPlan,
    SingleRowUpdatePlan, SingleRowUpdateRhs, TestFunctionsScope, VdbeProgram, VdbeVm,
    VmRuntimeContext, VmStepOut, eval_arith,
};
use sqliters_vfs::{MemoryVfs, OpenFlags, OsVfs, Vfs};

use crate::engine::{
    ActivitySummary, BusyPolicy, CommitFailureDisposition, ExecContext, LeaseCursorSpec,
    SqlSavepoint, StmtCategory, StmtTxTracker, classify_stmt_category,
    collect_select_referenced_roots, commit_all_databases, commit_transaction,
    execute_control_stmt, execute_nolock_stmt, execute_read_body_for_step, execute_write_body,
    parse_auto_vacuum_pragma, parse_incremental_vacuum_limit, parse_sqlite_uri,
    release_savepoint_op, rollback_all_databases, stmt_loads_schema_on_prepare,
    stmt_requires_schema, validate_and_infer_columns,
};
pub use db_set::{
    DatabaseSet, DbSlot, ResolvedRelation, SQLITE_MAX_PAGE_COUNT, SchemaInstanceId, TempStoreMode,
    TempTriggerBinding,
};
pub use driver::{AuthorizerAction, AuthorizerCallback, AuthorizerResult, AuthorizerSlot};
pub use engine::{QueryOutput, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT};
pub use execution_context::{
    CollationCallback, CollationRegistry, CommitCoordinatorHooks, CommitHookCallback,
    ExecutionContext, FallibleCollationCallback, FunctionRegistry, MutationContext, MutationOrigin,
    MutationPipeline, MutationRow, ProgressCallback, ProgressTracker, RollbackHookCallback,
    ScalarFunctionCallback, ScalarFunctionDef, TraceHookCallback, UpdateHookCallback,
    UpdateHookEvent, WindowAggregateInstance, WindowFunctionDef,
};
pub use schema::{
    ColumnSchema, ForeignKeySchema, IndexSchema, Schema, TableSchema, ViewSchema, VirtualTableDef,
};
pub use trigger::{
    SQLITE_MAX_TRIGGER_DEPTH, SQLITE_MAX_TRIGGER_STEPS, TriggerEventKind, TriggerSchema,
};
pub use vtab::{DbStatModule, DeclaredVTabSchema, PragmaVTabModule, VTabInstanceKey, VTabRegistry};

/// Maximum automatic `PrepareMode::V2` schema-change reprepare retries (`SQLITE_MAX_SCHEMA_RETRY`).
const SQLITE_MAX_SCHEMA_RETRY: usize = 50;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static ACTIVE_CONN_TOP: Cell<(u64, u32)> = const { Cell::new((0, 0)) };
    static EXTRA_ACTIVE_CONNS: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

struct ActiveConnStackGuard {
    conn_id: u64,
    in_top: bool,
}

impl ActiveConnStackGuard {
    #[inline(always)]
    fn try_push(conn_id: u64) -> Result<Self> {
        let mut in_top = false;
        let ok = ACTIVE_CONN_TOP.with(|cell| {
            let (top, extra) = cell.get();
            if top == conn_id {
                false
            } else if top == 0 && extra == 0 {
                cell.set((conn_id, 0));
                in_top = true;
                true
            } else if extra > 0
                && EXTRA_ACTIVE_CONNS
                    .with(|stack| stack.try_borrow().is_ok_and(|s| s.contains(&conn_id)))
            {
                false
            } else if top == 0 {
                cell.set((conn_id, extra));
                in_top = true;
                true
            } else {
                cell.set((top, extra + 1));
                EXTRA_ACTIVE_CONNS.with(|stack| {
                    if let Ok(mut s) = stack.try_borrow_mut() {
                        s.push(conn_id);
                    }
                });
                true
            }
        });
        if ok {
            Ok(Self { conn_id, in_top })
        } else {
            Err(SqliteError::misuse(
                "reentrant call into active Connection on the same thread",
            ))
        }
    }

    #[inline(always)]
    fn push(conn_id: u64) -> Self {
        let mut in_top = false;
        ACTIVE_CONN_TOP.with(|cell| {
            let (top, extra) = cell.get();
            if top == 0 {
                cell.set((conn_id, extra));
                in_top = true;
            } else {
                cell.set((top, extra + 1));
                EXTRA_ACTIVE_CONNS.with(|stack| {
                    if let Ok(mut s) = stack.try_borrow_mut() {
                        s.push(conn_id);
                    }
                });
            }
        });
        Self { conn_id, in_top }
    }
}

impl Drop for ActiveConnStackGuard {
    #[inline(always)]
    fn drop(&mut self) {
        if self.in_top {
            let _ = ACTIVE_CONN_TOP.try_with(|cell| {
                let (_, extra) = cell.get();
                cell.set((0, extra));
            });
        } else {
            let cid = self.conn_id;
            let _ = ACTIVE_CONN_TOP.try_with(|cell| {
                let (top, extra) = cell.get();
                cell.set((top, extra.saturating_sub(1)));
            });
            let _ = EXTRA_ACTIVE_CONNS.try_with(|stack| {
                if let Ok(mut s) = stack.try_borrow_mut()
                    && let Some(pos) = s.iter().rposition(|&id| id == cid)
                {
                    s.remove(pos);
                }
            });
        }
    }
}

static DEFAULT_OS_VFS: OnceLock<Arc<dyn Vfs>> = OnceLock::new();

#[inline]
fn os_vfs_for_geometry(lock_geometry: LockGeometry) -> Arc<dyn Vfs> {
    if lock_geometry == LockGeometry::default() {
        Arc::clone(DEFAULT_OS_VFS.get_or_init(|| Arc::new(OsVfs::new())))
    } else {
        Arc::new(OsVfs::with_lock_geometry(lock_geometry))
    }
}

#[inline(always)]
fn is_conn_active_on_current_thread(conn_id: u64) -> bool {
    ACTIVE_CONN_TOP
        .try_with(|cell| {
            let (top, extra) = cell.get();
            if top == conn_id {
                true
            } else if extra == 0 {
                false
            } else {
                EXTRA_ACTIVE_CONNS
                    .try_with(|stack| stack.try_borrow().is_ok_and(|s| s.contains(&conn_id)))
                    .unwrap_or(false)
            }
        })
        .unwrap_or(false)
}

/// Compile-time `Send` / `Sync` guarantees achieved purely with safe `std` synchronization.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<Connection>();
    assert_sync::<Connection>();
    assert_send::<Statement>();
    assert_send::<Backup>();
    assert_send::<Blob>();
    assert_send::<InterruptHandle>();
    assert_sync::<InterruptHandle>();
    assert_send::<TestBlobHandle>();
    assert_send::<Row>();
    assert_sync::<Row>();
};

enum RowRepr {
    Inline {
        row: InlineRow,
        materialized: OnceLock<Box<[Value]>>,
    },
    Heap(Vec<Value>),
}

/// Single result row returned by a query (`sqlite3_column_*`).
///
/// Stores rows with `<= 4` columns (including UTF-8 short `TEXT` and `BLOB` `<= 22` bytes)
/// inline on the stack without heap allocation, lazily materializing owned [`Value`]s only if
/// [`Row::values`], [`Row::get`], or `row[idx]` is requested.
pub struct Row {
    repr: RowRepr,
}

impl Clone for Row {
    #[inline]
    fn clone(&self) -> Self {
        match &self.repr {
            RowRepr::Inline { row, .. } => Self::from_inline(*row),
            RowRepr::Heap(values) => Self {
                repr: RowRepr::Heap(values.clone()),
            },
        }
    }
}

impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        let n = self.len();
        if n != other.len() {
            return false;
        }
        for i in 0..n {
            let Some(a) = self.get_ref(i) else {
                return false;
            };
            let Some(b) = other.get_ref(i) else {
                return false;
            };
            if !a.eq_value_ref(b) {
                return false;
            }
        }
        true
    }
}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Row")
            .field("values", &self.values())
            .finish()
    }
}

impl Row {
    #[inline]
    pub fn new(values: Vec<Value>) -> Self {
        if values.is_empty() && values.capacity() == 0 {
            return Self::from_inline(InlineRow::empty());
        }
        Self {
            repr: RowRepr::Heap(values),
        }
    }

    #[inline]
    pub fn from_slice(values: &[Value]) -> Self {
        match InlineRow::try_from_values(values) {
            Some(inline) => Self::from_inline(inline),
            None => Self {
                repr: RowRepr::Heap(values.to_vec()),
            },
        }
    }

    #[inline]
    pub(crate) const fn from_inline(row: InlineRow) -> Self {
        Self {
            repr: RowRepr::Inline {
                row,
                materialized: OnceLock::new(),
            },
        }
    }

    #[inline]
    fn from_lookup_row(row: PointLookupRow) -> Self {
        match row {
            PointLookupRow::Inline(ir) => Self::from_inline(ir),
            PointLookupRow::Heap(vals) => Self::new(vals),
        }
    }

    /// Returns `true` when this row's columns are stored inline without a heap `Vec<Value>`
    /// (and without having materialized the lazy `OnceLock<Box<[Value]>>` slice).
    #[inline]
    pub fn is_inline_unmaterialized(&self) -> bool {
        match &self.repr {
            RowRepr::Inline { materialized, .. } => materialized.get().is_none(),
            RowRepr::Heap(_) => false,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.len(),
            RowRepr::Heap(values) => values.len(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn values(&self) -> &[Value] {
        match &self.repr {
            RowRepr::Inline { row, materialized } => {
                materialized.get_or_init(|| row.to_boxed_values())
            }
            RowRepr::Heap(values) => values.as_slice(),
        }
    }

    pub fn into_values(self) -> Vec<Value> {
        match self.repr {
            RowRepr::Inline { row, materialized } => match materialized.into_inner() {
                Some(boxed) => boxed.into_vec(),
                None => row.to_vec_values(),
            },
            RowRepr::Heap(values) => values,
        }
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<ValueRef<'_>> {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.get(idx).map(InlineVal::as_value_ref),
            RowRepr::Heap(values) => values.get(idx).map(ValueRef::from_value),
        }
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&Value> {
        match &self.repr {
            RowRepr::Inline { row, materialized } => {
                if idx >= row.len() {
                    return None;
                }
                materialized.get_or_init(|| row.to_boxed_values()).get(idx)
            }
            RowRepr::Heap(values) => values.get(idx),
        }
    }

    #[inline]
    pub fn get_i64(&self, idx: usize) -> Option<i64> {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.get(idx).and_then(InlineVal::as_i64),
            RowRepr::Heap(values) => values.get(idx).and_then(Value::as_i64),
        }
    }

    #[inline]
    pub fn get_f64(&self, idx: usize) -> Option<f64> {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.get(idx).and_then(InlineVal::as_f64),
            RowRepr::Heap(values) => values.get(idx).and_then(Value::as_f64),
        }
    }

    #[inline]
    pub fn get_str(&self, idx: usize) -> Option<&str> {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.get(idx).and_then(InlineVal::as_str),
            RowRepr::Heap(values) => values.get(idx).and_then(Value::as_str),
        }
    }

    #[inline]
    pub fn get_blob(&self, idx: usize) -> Option<&[u8]> {
        match &self.repr {
            RowRepr::Inline { row, .. } => row.get(idx).and_then(InlineVal::as_blob),
            RowRepr::Heap(values) => match values.get(idx)? {
                Value::Blob(b) => Some(b.as_slice()),
                _ => None,
            },
        }
    }
}

impl std::ops::Index<usize> for Row {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        &self.values()[index]
    }
}

/// Complete result set of a query (`column_names` + `rows`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub column_names: Vec<String>,
    pub rows: Vec<Row>,
}

/// Outcome of [`Statement::step`] (`SQLITE_ROW` vs `SQLITE_DONE`).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum StepResult {
    Row(Row),
    Done,
}

/// Statement preparation mode (`sqlite3_prepare_v2` vs legacy `sqlite3_prepare`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrepareMode {
    /// Modern `sqlite3_prepare_v2` mode: automatically reprepares on schema changes.
    #[default]
    V2,
    /// Legacy `sqlite3_prepare` mode: does NOT auto-reprepare on schema changes (`ErrorCode::Schema`).
    V1,
}

/// Connection open options configuring initial page-size candidate, lock-byte geometry, and
/// private-temporary backing policy (`PagerOpenOptions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionOpenOptions {
    pub default_page_size: u32,
    pub lock_geometry: LockGeometry,
    pub temp_store_memory: bool,
    pub uri: bool,
}

impl Default for ConnectionOpenOptions {
    fn default() -> Self {
        Self {
            default_page_size: SQLITE_DEFAULT_PAGE_SIZE,
            lock_geometry: LockGeometry::default(),
            temp_store_memory: false,
            uri: false,
        }
    }
}

impl ConnectionOpenOptions {
    fn to_pager_options(self, db_kind: DatabaseKind) -> PagerOpenOptions {
        PagerOpenOptions {
            default_page_size: self.default_page_size,
            lock_geometry: Some(self.lock_geometry),
            db_kind: Some(db_kind),
            temp_in_memory: self.temp_store_memory,
            ..PagerOpenOptions::default()
        }
    }
}

/// Error returned by [`Connection::try_close`] when the connection cannot be closed yet
/// (for example, `SQLITE_BUSY` due to unfinalized [`Statement`] handles), returning ownership
/// of the intact [`Connection`] facade alongside the [`SqliteError`].
#[derive(Debug)]
pub struct CloseError<T> {
    pub connection: T,
    pub error: SqliteError,
}

impl<T> fmt::Display for CloseError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.error, f)
    }
}

use fmt::Display;

impl<T: fmt::Debug> std::error::Error for CloseError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Lock-free, cloneable handle for signaling `sqlite3_interrupt` from another thread or callback
/// without entering the connection execution gate or locking `ConnectionInner`.
#[derive(Debug, Clone)]
pub struct InterruptHandle {
    flag: Arc<AtomicBool>,
}

impl InterruptHandle {
    /// Signals an asynchronous interrupt (`SQLITE_INTERRUPT`) to any in-progress operation.
    #[inline]
    pub fn interrupt(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// Clears a previously signaled interrupt flag.
    #[inline]
    pub fn reset(&self) {
        self.flag.store(false, Ordering::SeqCst);
    }

    /// Returns `true` if the interrupt flag is currently set.
    #[inline]
    pub fn is_interrupted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Returns a reference to the underlying atomic flag.
    #[inline]
    pub fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct HandleId(pub(crate) u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandleKind {
    Statement,
    Blob { writable: bool },
    BackupDestination,
    TestBlob { writable: bool },
}

/// Inline small vector for [`SchemaInstanceId`] sets (`<= 2` slots inline with zero heap
/// allocations, spilling to `Vec` only when `>= 3` attached databases participate).
#[derive(Debug, Clone)]
pub(crate) struct SmallInstVec {
    inline: [SchemaInstanceId; 2],
    len: u8,
    spilled: Vec<SchemaInstanceId>,
}

impl Default for SmallInstVec {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl SmallInstVec {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self {
            inline: [SchemaInstanceId(0), SchemaInstanceId(0)],
            len: 0,
            spilled: Vec::new(),
        }
    }

    #[inline]
    pub(crate) const fn single(id: SchemaInstanceId) -> Self {
        Self {
            inline: [id, SchemaInstanceId(0)],
            len: 1,
            spilled: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn as_slice(&self) -> &[SchemaInstanceId] {
        if self.spilled.is_empty() {
            &self.inline[..usize::from(self.len)]
        } else {
            &self.spilled
        }
    }

    #[inline]
    pub(crate) fn push_unique(&mut self, id: SchemaInstanceId) {
        if self.as_slice().contains(&id) {
            return;
        }
        if !self.spilled.is_empty() {
            self.spilled.push(id);
        } else if self.len < 2 {
            self.inline[usize::from(self.len)] = id;
            self.len += 1;
        } else {
            self.spilled.reserve(4);
            self.spilled.push(self.inline[0]);
            self.spilled.push(self.inline[1]);
            self.spilled.push(id);
        }
    }

    #[inline]
    pub(crate) fn merge_slice(&mut self, src: &[SchemaInstanceId]) {
        for &id in src {
            self.push_unique(id);
        }
    }
}

impl std::ops::Deref for SmallInstVec {
    type Target = [SchemaInstanceId];

    #[inline]
    fn deref(&self) -> &[SchemaInstanceId] {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a SmallInstVec {
    type Item = &'a SchemaInstanceId;
    type IntoIter = std::slice::Iter<'a, SchemaInstanceId>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl IntoIterator for SmallInstVec {
    type Item = SchemaInstanceId;
    type IntoIter = SmallInstIntoIter;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        if self.spilled.is_empty() {
            SmallInstIntoIter::Inline {
                items: self.inline,
                pos: 0,
                len: self.len,
            }
        } else {
            SmallInstIntoIter::Spilled(self.spilled.into_iter())
        }
    }
}

pub(crate) enum SmallInstIntoIter {
    Inline {
        items: [SchemaInstanceId; 2],
        pos: u8,
        len: u8,
    },
    Spilled(std::vec::IntoIter<SchemaInstanceId>),
}

impl Iterator for SmallInstIntoIter {
    type Item = SchemaInstanceId;

    #[inline]
    fn next(&mut self) -> Option<SchemaInstanceId> {
        match self {
            Self::Inline { items, pos, len } => {
                if *pos < *len {
                    let v = items[usize::from(*pos)];
                    *pos += 1;
                    Some(v)
                } else {
                    None
                }
            }
            Self::Spilled(it) => it.next(),
        }
    }
}

impl From<Vec<SchemaInstanceId>> for SmallInstVec {
    #[inline]
    fn from(v: Vec<SchemaInstanceId>) -> Self {
        match v.len() {
            0 => Self::new(),
            1 => Self::single(v[0]),
            2 => Self {
                inline: [v[0], v[1]],
                len: 2,
                spilled: Vec::new(),
            },
            _ => Self {
                inline: [SchemaInstanceId(0), SchemaInstanceId(0)],
                len: 0,
                spilled: v,
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActiveLease {
    pub(crate) is_reader: bool,
    pub(crate) is_writer: bool,
    pub(crate) read_instances: SmallInstVec,
    pub(crate) write_instances: SmallInstVec,
}

#[inline]
fn push_unique_instance(dst: &mut SmallInstVec, id: SchemaInstanceId) {
    dst.push_unique(id);
}

#[inline]
fn merge_instances(dst: &mut SmallInstVec, src: &[SchemaInstanceId]) {
    dst.merge_slice(src);
}

#[inline]
fn mask_to_instances(db_set: &DatabaseSet, mask: u16) -> SmallInstVec {
    let mut out = SmallInstVec::new();
    if mask == 0 {
        return out;
    }
    if mask == 1 {
        return SmallInstVec::single(db_set.slots()[0].schema_instance_id);
    }
    let mut rem = mask;
    while rem != 0 {
        let idx = rem.trailing_zeros() as usize;
        rem &= rem - 1;
        if let Some(slot) = db_set.slots().get(idx) {
            out.push_unique(slot.schema_instance_id);
        }
    }
    out
}

#[derive(Debug, Default)]
pub(crate) struct ActivityTracker {
    pub(crate) leases: Vec<(HandleId, ActiveLease)>,
}

impl ActivityTracker {
    #[inline]
    pub(crate) fn new() -> Self {
        Self { leases: Vec::new() }
    }

    #[inline]
    pub(crate) fn contains(&self, id: HandleId) -> bool {
        self.leases.iter().any(|&(hid, _)| hid == id)
    }

    #[inline]
    pub(crate) fn start(&mut self, id: HandleId, is_reader: bool, is_writer: bool) {
        let lease = ActiveLease {
            is_reader: is_reader || is_writer,
            is_writer,
            read_instances: SmallInstVec::new(),
            write_instances: SmallInstVec::new(),
        };
        if let Some(slot) = self.leases.iter_mut().find(|(hid, _)| *hid == id) {
            slot.1 = lease;
        } else {
            self.leases.push((id, lease));
        }
    }

    #[inline]
    pub(crate) fn start_single_read(&mut self, id: HandleId, inst: SchemaInstanceId) {
        let lease = ActiveLease {
            is_reader: true,
            is_writer: false,
            read_instances: SmallInstVec::single(inst),
            write_instances: SmallInstVec::new(),
        };
        if !self.leases.is_empty()
            && let Some(slot) = self.leases.iter_mut().find(|(hid, _)| *hid == id)
        {
            slot.1 = lease;
        } else {
            self.leases.push((id, lease));
        }
    }

    #[inline]
    pub(crate) fn start_with_instances(
        &mut self,
        id: HandleId,
        is_reader: bool,
        is_writer: bool,
        read_instances: impl Into<SmallInstVec>,
        write_instances: impl Into<SmallInstVec>,
    ) {
        let read_instances = read_instances.into();
        let write_instances = write_instances.into();
        let has_w = is_writer || !write_instances.is_empty();
        let has_r = is_reader || has_w || !read_instances.is_empty();
        let lease = ActiveLease {
            is_reader: has_r,
            is_writer: has_w,
            read_instances,
            write_instances,
        };
        if let Some(slot) = self.leases.iter_mut().find(|(hid, _)| *hid == id) {
            slot.1 = lease;
        } else {
            self.leases.push((id, lease));
        }
    }

    #[inline]
    pub(crate) fn update_instances(
        &mut self,
        id: HandleId,
        read_instances: &[SchemaInstanceId],
        write_instances: &[SchemaInstanceId],
    ) {
        if let Some((_, lease)) = self.leases.iter_mut().find(|(hid, _)| *hid == id) {
            lease.read_instances.merge_slice(read_instances);
            lease.write_instances.merge_slice(write_instances);
            if !write_instances.is_empty() {
                lease.is_writer = true;
                lease.is_reader = true;
            } else if !read_instances.is_empty() {
                lease.is_reader = true;
            }
        }
    }

    #[inline]
    pub(crate) fn remove(&mut self, id: HandleId) -> Option<ActiveLease> {
        if let Some(pos) = self.leases.iter().rposition(|&(hid, _)| hid == id) {
            Some(self.leases.swap_remove(pos).1)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn summary_excluding(
        &self,
        exclude: HandleId,
        db_set: &DatabaseSet,
    ) -> ActivitySummary {
        if self.leases.is_empty() || (self.leases.len() == 1 && self.leases[0].0 == exclude) {
            return ActivitySummary::default();
        }
        let mut other_active = 0usize;
        let mut other_readers = 0usize;
        let mut other_writers = 0usize;
        let mut other_read_db_mask = 0u16;
        let mut other_write_db_mask = 0u16;
        for &(hid, ref lease) in &self.leases {
            if hid == exclude {
                continue;
            }
            other_active += 1;
            if lease.is_reader {
                other_readers += 1;
            }
            if lease.is_writer {
                other_writers += 1;
            }
            for &inst in &lease.read_instances {
                if let Some(db_id) = db_set.find_db_id_by_instance(inst) {
                    other_read_db_mask |= 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                }
            }
            for &inst in &lease.write_instances {
                if let Some(db_id) = db_set.find_db_id_by_instance(inst) {
                    let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                    other_write_db_mask |= bit;
                    other_read_db_mask |= bit;
                }
            }
        }
        ActivitySummary {
            other_active,
            other_readers,
            other_writers,
            other_read_db_mask,
            other_write_db_mask,
        }
    }

    #[inline]
    pub(crate) fn has_no_active_tx(&self) -> bool {
        !self.leases.iter().any(|(_, l)| l.is_reader || l.is_writer)
    }

    #[inline]
    pub(crate) fn has_active_tx_on_instance(&self, inst: SchemaInstanceId) -> bool {
        self.leases
            .iter()
            .any(|(_, l)| l.read_instances.contains(&inst) || l.write_instances.contains(&inst))
    }

    #[inline]
    pub(crate) fn has_active_writer_on_instance(&self, inst: SchemaInstanceId) -> bool {
        self.leases
            .iter()
            .any(|(_, l)| l.write_instances.contains(&inst))
    }
}

const CONN_STATE_OPEN: u8 = 0;
const CONN_STATE_CLOSED: u8 = 1;
const CONN_STATE_POISONED: u8 = 2;
const ELIDED_STMT_SP: usize = usize::MAX;

#[derive(Debug, Clone)]
pub(crate) enum ConnectionState {
    Open,
    Closing,
    Poisoned(SqliteError),
    Closed,
}

#[derive(Clone)]
pub(crate) struct WriteAbortSnapshot {
    pub(crate) stmt_sp_slot: Option<usize>,
    pub(crate) touched_write_instances: SmallInstVec,
    pub(crate) started_read_instances: SmallInstVec,
    pub(crate) fk_before: Vec<(SchemaInstanceId, String, schema::TableRowKey, usize)>,
    pub(crate) rt_before: VmRuntimeContext,
    pub(crate) is_dml: bool,
    pub(crate) mutation_len_before: usize,
    pub(crate) schemas_before: Vec<(SchemaInstanceId, Schema)>,
    pub(crate) schema_gen_before: u64,
    pub(crate) v1_expire_gen_before: u64,
    pub(crate) schema_changed_before: bool,
}

pub(crate) enum RunningExec {
    Vdbe {
        vm: VdbeVm,
        read_instances: SmallInstVec,
    },
    PointSelectRowYielded {
        main_inst: SchemaInstanceId,
    },
    MaterializedRead {
        rows: std::vec::IntoIter<Vec<Value>>,
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: SmallInstVec,
    },
    MaterializedInlineRead {
        rows: std::vec::IntoIter<InlineRow>,
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: SmallInstVec,
    },
    FastSingleTextArena {
        arena: String,
        spans: Vec<(u32, u32)>,
        pos: usize,
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: SmallInstVec,
    },
    MaterializedNoLock {
        rows: std::vec::IntoIter<Vec<Value>>,
    },
    ReturningWrite {
        rows: std::vec::IntoIter<Vec<Value>>,
        abort_snapshot: WriteAbortSnapshot,
    },
    IncrementalVacuum {
        db_instance: SchemaInstanceId,
        remaining: u32,
        abort_snapshot: WriteAbortSnapshot,
    },
    PendingCommit {
        release_savepoint: Option<String>,
        last_busy: SqliteError,
    },
}

#[allow(clippy::large_enum_variant)]
enum StatementLifecycle {
    Ready,
    Running(RunningExec),
    Halted { last_error: Option<SqliteError> },
}

pub(crate) enum PendingDrop {
    Statement {
        handle_id: HandleId,
        running: Option<Box<RunningExec>>,
    },
    Blob {
        handle_id: HandleId,
        schema_instance_id: SchemaInstanceId,
        writable: bool,
    },
    BackupDestination {
        handle_id: HandleId,
        dst_instance_id: SchemaInstanceId,
        in_write_tx: bool,
        active_lease: bool,
    },
    BackupSource {
        src_instance_id: SchemaInstanceId,
        tracker: Arc<Mutex<sqliters_btree::BackupTracker>>,
    },
    TestBlob {
        handle_id: HandleId,
        lease_cursor: Option<(SchemaInstanceId, CursorId)>,
        writable: bool,
    },
}

pub(crate) enum ActivityFinish {
    NoLock,
    ReadDone {
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: SmallInstVec,
    },
    ReadAborted {
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: SmallInstVec,
    },
    WriteCompleted {
        abort_snapshot: WriteAbortSnapshot,
    },
    PendingCommitAborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LitArgShape {
    Param,
    JsonbCall,
}

pub(crate) struct CachedLiteralInsert {
    pub(crate) table_name: String,
    pub(crate) shapes: Vec<LitArgShape>,
    pub(crate) schema_generation: u64,
    pub(crate) v1_expire_generation: u64,
    pub(crate) advisory_expire_generation: u64,
    pub(crate) main_instance_id: SchemaInstanceId,
    pub(crate) main_cookie: u32,
    pub(crate) main_had_pages: bool,
    pub(crate) temp_open: bool,
    pub(crate) program: Arc<VdbeProgram>,
}

pub(crate) struct ConnectionInner {
    pub(crate) db_set: DatabaseSet,
    pub(crate) connection_encoding: TextEncoding,
    pub(crate) state: ConnectionState,
    pub(crate) next_handle_id: u64,
    pub(crate) open_handles: BTreeMap<HandleId, HandleKind>,
    pub(crate) activity: ActivityTracker,
    pub(crate) savepoints: Vec<SqlSavepoint>,
    pub(crate) runtime: VmRuntimeContext,
    pub(crate) active_interrupt_flag: Arc<AtomicBool>,
    pub(crate) functions: FunctionRegistry,
    pub(crate) collations: CollationRegistry,
    pub(crate) progress: ProgressTracker,
    pub(crate) commit_hooks: CommitCoordinatorHooks,
    pub(crate) mutation_pipeline: MutationPipeline,
    pub(crate) authorizer: AuthorizerSlot,
    pub(crate) trace_hook: Option<TraceHookCallback>,
    pub(crate) strict_subtype: bool,
    pub(crate) explicit_tx: bool,
    pub(crate) implicit_savepoint_tx: bool,
    pub(crate) in_user_btree_tx: bool,
    pub(crate) schema_changed_in_tx: bool,
    pub(crate) foreign_keys: bool,
    pub(crate) recursive_triggers: bool,
    pub(crate) legacy_alter_table: bool,
    pub(crate) count_changes: bool,
    pub(crate) wal_autocheckpoint_threshold: i32,
    pub(crate) default_locking_mode: LockingMode,
    pub(crate) writable_schema: bool,
    pub(crate) working_dir: Option<PathBuf>,
    pub(crate) busy_policy: BusyPolicy,
    pub(crate) rollback_count: u64,
    pub(crate) deferred_fk_suspects: Vec<(SchemaInstanceId, String, schema::TableRowKey, usize)>,
    pub(crate) schema_generation: u64,
    pub(crate) v1_expire_generation: u64,
    pub(crate) advisory_expire_generation: u64,
    pub(crate) simulated_cleanup_io_error: Option<SqliteError>,
    pub(crate) panic_on_next_pending_drop: bool,
    pub(crate) cached_lit_insert: Option<CachedLiteralInsert>,
    pub(crate) lit_insert_neg_table: Option<(String, u64)>,
    pub(crate) lit_insert_params: Vec<Value>,
    pub(crate) lit_insert_shapes: Vec<LitArgShape>,
    pub(crate) lit_insert_scratch: sqliters_vdbe::VdbeWriteScratch,
    pub(crate) pooled_read_vm: Option<VdbeVm>,
    pub(crate) pooled_write_scratch: sqliters_vdbe::VdbeWriteScratch,
}

const GATE_BUSY: u8 = 1;
const GATE_PENDING_DROPS: u8 = 2;

pub(crate) struct ConnectionShared {
    pub(crate) conn_id: u64,
    pub(crate) db_path: Option<PathBuf>,
    pub(crate) database_kind: DatabaseKind,
    pub(crate) is_readonly: bool,
    pub(crate) open_options: ConnectionOpenOptions,
    pub(crate) interrupt_flag: Arc<AtomicBool>,
    pub(crate) has_external_interrupt: AtomicBool,
    pub(crate) gate_state: AtomicU8,
    pub(crate) conn_state: AtomicU8,
    pub(crate) mirror_last_insert_rowid: AtomicI64,
    pub(crate) mirror_changes: AtomicU64,
    pub(crate) mirror_total_changes: AtomicU64,
    pub(crate) mirror_autocommit: AtomicBool,
    pub(crate) mirror_rollback_count: AtomicU64,
    pub(crate) mirror_attached_limit: AtomicUsize,
    pub(crate) pending_drops: Mutex<Vec<PendingDrop>>,
    pub(crate) inner: Mutex<ConnectionInner>,
}

impl ConnectionShared {
    #[inline]
    pub(crate) fn sync_conn_state(&self, state: &ConnectionState) {
        let code = match state {
            ConnectionState::Open => CONN_STATE_OPEN,
            ConnectionState::Closing | ConnectionState::Closed => CONN_STATE_CLOSED,
            ConnectionState::Poisoned(_) => CONN_STATE_POISONED,
        };
        if self.conn_state.load(Ordering::Relaxed) != code {
            self.conn_state.store(code, Ordering::Release);
        }
    }

    pub(crate) fn publish_mirrors(
        &self,
        rt: &VmRuntimeContext,
        explicit_tx: bool,
        implicit_savepoint_tx: bool,
        rollback_count: u64,
    ) {
        self.mirror_last_insert_rowid
            .store(rt.last_insert_rowid, Ordering::Release);
        self.mirror_changes
            .store(rt.last_changes, Ordering::Release);
        self.mirror_total_changes
            .store(rt.total_changes, Ordering::Release);
        self.mirror_autocommit
            .store(!explicit_tx && !implicit_savepoint_tx, Ordering::Release);
        self.mirror_rollback_count
            .store(rollback_count, Ordering::Release);
    }

    pub(crate) fn lock_pending_drops(&self) -> MutexGuard<'_, Vec<PendingDrop>> {
        self.pending_drops
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn lock_inner(&self) -> MutexGuard<'_, ConnectionInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[inline(always)]
    fn claim_gate_busy(&self) -> Result<bool> {
        match self
            .gate_state
            .compare_exchange(0, GATE_BUSY, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => Ok(false),
            Err(prev) => self.claim_gate_busy_slow(prev),
        }
    }

    #[cold]
    #[inline(never)]
    fn claim_gate_busy_slow(&self, mut cur: u8) -> Result<bool> {
        loop {
            if (cur & GATE_BUSY) != 0 {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "database connection is busy",
                ));
            }
            match self.gate_state.compare_exchange_weak(
                cur,
                cur | GATE_BUSY,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok((cur & GATE_PENDING_DROPS) != 0),
                Err(actual) => cur = actual,
            }
        }
    }

    #[inline]
    pub(crate) fn enter(&self) -> Result<ConnSession<'_>> {
        let stack_guard = ActiveConnStackGuard::try_push(self.conn_id)?;
        let had_pending = self.claim_gate_busy()?;

        let mut gate_unwind = GateReleaseOnUnwind {
            shared: self,
            armed: true,
        };
        let inner_guard = self.lock_inner();
        // Transfer gate release responsibility to `ConnSession` BEFORE running `process_pending_drops`!
        gate_unwind.armed = false;
        let mut session = ConnSession {
            shared: self,
            guard: Some(inner_guard),
            _stack_guard: stack_guard,
        };
        if had_pending || (self.gate_state.load(Ordering::Acquire) & GATE_PENDING_DROPS) != 0 {
            self.drain_initial_drops(&mut session);
        }
        Ok(session)
    }

    #[cold]
    #[inline(never)]
    fn drain_initial_drops(&self, session: &mut ConnSession<'_>) {
        let initial_drops = {
            let mut pq = self.lock_pending_drops();
            let batch = std::mem::take(&mut *pq);
            self.gate_state
                .fetch_and(!GATE_PENDING_DROPS, Ordering::Release);
            batch
        };
        if !initial_drops.is_empty() {
            session
                .inner_mut()
                .process_pending_drops(initial_drops, self);
        }
    }

    pub(crate) fn enter_dual<'a>(src: &'a Self, dst: &'a Self) -> Result<DualConnSession<'a>> {
        if src.conn_id == dst.conn_id {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "source and destination must be distinct",
            ));
        }
        if is_conn_active_on_current_thread(src.conn_id)
            || is_conn_active_on_current_thread(dst.conn_id)
        {
            return Err(SqliteError::misuse(
                "reentrant call into active Connection on the same thread",
            ));
        }

        let (first, second, src_is_first) = if src.conn_id < dst.conn_id {
            (src, dst, true)
        } else {
            (dst, src, false)
        };

        let first_had_pending = first.claim_gate_busy()?;
        let mut first_unwind = GateReleaseOnUnwind {
            shared: first,
            armed: true,
        };

        let second_had_pending = second.claim_gate_busy()?;
        let mut second_unwind = GateReleaseOnUnwind {
            shared: second,
            armed: true,
        };

        let first_stack = ActiveConnStackGuard::push(first.conn_id);
        let second_stack = ActiveConnStackGuard::push(second.conn_id);
        let first_guard = first.lock_inner();
        let second_guard = second.lock_inner();
        first_unwind.armed = false;
        second_unwind.armed = false;

        let mut session = DualConnSession {
            first,
            second,
            first_guard: Some(first_guard),
            second_guard: Some(second_guard),
            src_is_first,
            _second_stack: second_stack,
            _first_stack: first_stack,
        };

        if first_had_pending || (first.gate_state.load(Ordering::Acquire) & GATE_PENDING_DROPS) != 0
        {
            let first_drops = {
                let mut pq = first.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                first
                    .gate_state
                    .fetch_and(!GATE_PENDING_DROPS, Ordering::Release);
                batch
            };
            if !first_drops.is_empty() {
                session
                    .first_guard
                    .as_mut()
                    .unwrap()
                    .process_pending_drops(first_drops, first);
            }
        }

        if second_had_pending
            || (second.gate_state.load(Ordering::Acquire) & GATE_PENDING_DROPS) != 0
        {
            let second_drops = {
                let mut pq = second.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                second
                    .gate_state
                    .fetch_and(!GATE_PENDING_DROPS, Ordering::Release);
                batch
            };
            if !second_drops.is_empty() {
                session
                    .second_guard
                    .as_mut()
                    .unwrap()
                    .process_pending_drops(second_drops, second);
            }
        }

        Ok(session)
    }

    pub(crate) fn enqueue_or_run_drop(&self, item: PendingDrop) {
        let claimed = {
            let mut pq = self.lock_pending_drops();
            pq.push(item);
            let prev = self
                .gate_state
                .fetch_or(GATE_BUSY | GATE_PENDING_DROPS, Ordering::AcqRel);
            (prev & GATE_BUSY) == 0
        };
        if claimed {
            let _stack_guard = ActiveConnStackGuard::push(self.conn_id);
            self.drain_and_release_gate_slow();
        }
    }

    #[inline]
    fn drain_and_release_gate(&self) {
        if self
            .gate_state
            .compare_exchange(GATE_BUSY, 0, Ordering::Release, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
        self.drain_and_release_gate_slow();
    }

    #[cold]
    #[inline(never)]
    fn drain_and_release_gate_slow(&self) {
        let mut gate_unwind = GateReleaseOnUnwind {
            shared: self,
            armed: true,
        };
        loop {
            let batch = {
                let mut pq = self.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                if batch.is_empty() {
                    self.gate_state.store(0, Ordering::Release);
                    gate_unwind.armed = false;
                    return;
                }
                self.gate_state
                    .fetch_and(!GATE_PENDING_DROPS, Ordering::Release);
                batch
            };
            let mut inner = self.lock_inner();
            inner.process_pending_drops(batch, self);
            self.sync_conn_state(&inner.state);
            drop(inner);
        }
    }
}

struct GateReleaseOnUnwind<'a> {
    shared: &'a ConnectionShared,
    armed: bool,
}

impl Drop for GateReleaseOnUnwind<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.shared
                .gate_state
                .fetch_and(!GATE_BUSY, Ordering::Release);
        }
    }
}

impl Drop for ConnectionShared {
    fn drop(&mut self) {
        self.conn_state.store(CONN_STATE_CLOSED, Ordering::Release);
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        let pending = self
            .pending_drops
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let batch = std::mem::take(pending);
        self.gate_state.store(0, Ordering::Relaxed);
        for item in batch {
            match item {
                PendingDrop::Statement { handle_id, running } => {
                    if let Some(boxed_run) = running {
                        match *boxed_run {
                            RunningExec::Vdbe { mut vm, .. } => {
                                vm.close_cursors(&mut inner.db_set);
                            }
                            RunningExec::MaterializedRead { lease_cursors, .. }
                            | RunningExec::MaterializedInlineRead { lease_cursors, .. }
                            | RunningExec::FastSingleTextArena { lease_cursors, .. } => {
                                for (inst, cid) in lease_cursors {
                                    if let Some(slot) = inner.db_set.slot_by_instance_mut(inst)
                                        && let Some(ref mut bt) = slot.btree
                                    {
                                        bt.close_cursor(cid);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    inner.activity.remove(handle_id);
                    inner.open_handles.remove(&handle_id);
                }
                PendingDrop::Blob { handle_id, .. } => {
                    inner.db_set.open_blobs_mut().remove(&handle_id.0);
                    inner.activity.remove(handle_id);
                    inner.open_handles.remove(&handle_id);
                }
                PendingDrop::BackupDestination {
                    handle_id,
                    dst_instance_id,
                    in_write_tx,
                    ..
                } => {
                    if let Some(slot) = inner.db_set.slot_by_instance_mut(dst_instance_id) {
                        slot.active_backups = slot.active_backups.saturating_sub(1);
                        if in_write_tx && let Some(ref mut bt) = slot.btree {
                            bt.pager_mut().backup_reset_raw_staging();
                            if bt.transaction_state() == TransactionState::Write {
                                let _ = bt.rollback_with_trip(
                                    extended::SQLITE_ABORT_ROLLBACK,
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                    inner.activity.remove(handle_id);
                    inner.open_handles.remove(&handle_id);
                }
                PendingDrop::BackupSource {
                    src_instance_id,
                    tracker,
                } => {
                    if let Some(slot) = inner.db_set.slot_by_instance_mut(src_instance_id) {
                        slot.active_backups = slot.active_backups.saturating_sub(1);
                        slot.detach_backup_tracker(&tracker);
                    }
                }
                PendingDrop::TestBlob {
                    handle_id,
                    lease_cursor,
                    ..
                } => {
                    if let Some((inst, cid)) = lease_cursor
                        && let Some(slot) = inner.db_set.slot_by_instance_mut(inst)
                        && let Some(ref mut bt) = slot.btree
                    {
                        bt.close_cursor(cid);
                    }
                    inner.activity.remove(handle_id);
                    inner.open_handles.remove(&handle_id);
                }
            }
        }
        if !matches!(inner.state, ConnectionState::Closed) {
            for slot in inner.db_set.slots_mut().iter_mut().rev() {
                if let Some(ref mut bt) = slot.btree {
                    bt.close_all_cursors();
                    if bt.transaction_state() != TransactionState::Open {
                        let _ =
                            bt.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
                    }
                }
            }
            let _ = inner.db_set.close_all();
            inner.state = ConnectionState::Closed;
        }
    }
}

pub(crate) struct ConnSession<'a> {
    pub(crate) shared: &'a ConnectionShared,
    guard: Option<MutexGuard<'a, ConnectionInner>>,
    _stack_guard: ActiveConnStackGuard,
}

impl ConnSession<'_> {
    #[inline]
    pub(crate) fn inner_mut(&mut self) -> &mut ConnectionInner {
        self.guard
            .as_mut()
            .expect("ConnSession inner guard is live")
    }

    #[inline]
    pub(crate) fn inner(&self) -> &ConnectionInner {
        self.guard
            .as_ref()
            .expect("ConnSession inner guard is live")
    }
}

impl Drop for ConnSession<'_> {
    #[inline]
    fn drop(&mut self) {
        if let Some(g) = self.guard.take() {
            self.shared.sync_conn_state(&g.state);
            drop(g);
        }
        self.shared.drain_and_release_gate();
    }
}

pub(crate) struct DualConnSession<'a> {
    first: &'a ConnectionShared,
    second: &'a ConnectionShared,
    first_guard: Option<MutexGuard<'a, ConnectionInner>>,
    second_guard: Option<MutexGuard<'a, ConnectionInner>>,
    src_is_first: bool,
    _second_stack: ActiveConnStackGuard,
    _first_stack: ActiveConnStackGuard,
}

impl DualConnSession<'_> {
    #[inline]
    pub(crate) fn src_and_dst_mut(&mut self) -> (&mut ConnectionInner, &mut ConnectionInner) {
        let f = self
            .first_guard
            .as_mut()
            .expect("DualConnSession first guard is live");
        let s = self
            .second_guard
            .as_mut()
            .expect("DualConnSession second guard is live");
        if self.src_is_first { (f, s) } else { (s, f) }
    }
}

impl Drop for DualConnSession<'_> {
    fn drop(&mut self) {
        if let Some(ref g) = self.second_guard {
            self.second.sync_conn_state(&g.state);
        }
        if let Some(ref g) = self.first_guard {
            self.first.sync_conn_state(&g.state);
        }
        drop(self.second_guard.take());
        drop(self.first_guard.take());
        self.second.drain_and_release_gate();
        self.first.drain_and_release_gate();
    }
}

impl ConnectionInner {
    pub(crate) fn ensure_usable(&self) -> Result<()> {
        match &self.state {
            ConnectionState::Open => Ok(()),
            ConnectionState::Closing | ConnectionState::Closed => {
                Err(SqliteError::misuse("cannot use a closed connection"))
            }
            ConnectionState::Poisoned(err) => Err(err.clone()),
        }
    }

    #[inline]
    pub(crate) fn effective_encoding(&self) -> TextEncoding {
        if self.db_set.main_btree().pager().page_count() > 0 {
            self.db_set.main_btree().encoding()
        } else {
            self.connection_encoding
        }
    }

    #[inline]
    pub(crate) fn check_any_interrupted(&self) -> Result<()> {
        if self.active_interrupt_flag.load(Ordering::Relaxed) {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        if let Some(bt_flag) = self.db_set.main_btree().interrupt_flag()
            && !Arc::ptr_eq(&self.active_interrupt_flag, bt_flag)
            && bt_flag.load(Ordering::Relaxed)
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    pub(crate) fn publish(&self, shared: &ConnectionShared) {
        shared.publish_mirrors(
            &self.runtime,
            self.explicit_tx,
            self.implicit_savepoint_tx,
            self.rollback_count,
        );
    }

    pub(crate) fn is_autocommit(&self) -> bool {
        !self.explicit_tx && !self.implicit_savepoint_tx
    }

    pub(crate) fn register_handle(&mut self, kind: HandleKind) -> HandleId {
        let id = HandleId(self.next_handle_id);
        self.next_handle_id += 1;
        self.open_handles.insert(id, kind);
        id
    }

    pub(crate) fn take_simulated_cleanup_error(&mut self) -> Option<SqliteError> {
        self.simulated_cleanup_io_error.take()
    }

    pub(crate) fn poison_with(&mut self, err: SqliteError) -> SqliteError {
        if !matches!(self.state, ConnectionState::Poisoned(_)) {
            self.state = ConnectionState::Poisoned(err.clone());
        }
        err
    }

    pub(crate) fn snapshot_open_schemas(&self) -> Vec<(SchemaInstanceId, Schema)> {
        self.db_set
            .slots()
            .iter()
            .filter(|s| s.btree.is_some())
            .map(|s| (s.schema_instance_id, s.schema.clone()))
            .collect()
    }

    pub(crate) fn ensure_schemas_loaded_for_blob(
        &mut self,
        force_recheck_open: bool,
    ) -> Result<bool> {
        self.ensure_schemas_loaded_for_prepare(force_recheck_open)
    }

    pub(crate) fn finish_blob_activity(
        &mut self,
        hid: HandleId,
        inst: SchemaInstanceId,
        writable: bool,
        shared: &ConnectionShared,
    ) -> Result<()> {
        self.db_set.open_blobs_mut().remove(&hid.0);
        if !self.activity.contains(hid) {
            return Ok(());
        }
        if writable {
            let snap = WriteAbortSnapshot {
                stmt_sp_slot: None,
                touched_write_instances: SmallInstVec::single(inst),
                started_read_instances: SmallInstVec::new(),
                fk_before: self.deferred_fk_suspects.clone(),
                rt_before: self.runtime,
                is_dml: false,
                mutation_len_before: self.mutation_pipeline.len(),
                schemas_before: self.snapshot_open_schemas(),
                schema_gen_before: self.schema_generation,
                v1_expire_gen_before: self.v1_expire_generation,
                schema_changed_before: self.schema_changed_in_tx,
            };
            self.finish_activity(
                hid,
                ActivityFinish::WriteCompleted {
                    abort_snapshot: snap,
                },
                shared,
            )
        } else {
            self.finish_activity(
                hid,
                ActivityFinish::ReadDone {
                    lease_cursors: Vec::new(),
                    read_instances: SmallInstVec::single(inst),
                },
                shared,
            )
        }
    }

    pub(crate) fn close_blob_handle(
        &mut self,
        hid: HandleId,
        inst: SchemaInstanceId,
        writable: bool,
        shared: &ConnectionShared,
    ) -> Result<()> {
        let res = self.finish_blob_activity(hid, inst, writable, shared);
        self.open_handles.remove(&hid);
        res
    }

    fn snapshot_slot_cookies(&self) -> Vec<(SchemaInstanceId, u32, bool)> {
        self.db_set
            .slots()
            .iter()
            .filter(|s| s.btree.is_some())
            .map(|s| {
                (
                    s.schema_instance_id,
                    s.schema.schema_cookie,
                    s.schema.had_pages,
                )
            })
            .collect()
    }

    fn restore_schemas_snapshot(&mut self, prev: &[(SchemaInstanceId, Schema)]) {
        for slot in self.db_set.slots_mut() {
            if let Some((_, saved)) = prev
                .iter()
                .find(|(inst, _)| *inst == slot.schema_instance_id)
            {
                slot.schema = saved.clone();
            } else if let Some(ref mut bt) = slot.btree
                && let Ok(fresh) = Schema::load_for_schema(bt, Some(&slot.name))
            {
                slot.schema = fresh;
            }
        }
        self.db_set.sync_temp_trigger_bindings_from_schema();
        self.db_set.rebind_all_collations(&self.collations);
    }

    /// Ensures `sqlite_schema` catalogs across open database slots are loaded for `prepare`,
    /// releasing any temporary `Read` transaction immediately if no user/statement B-tree
    /// transaction was active (`sqlite3InitOne.openedTransaction`).
    fn ensure_schemas_loaded_for_prepare(&mut self, force_recheck_open: bool) -> Result<bool> {
        let mut any_changed = false;
        let slot_cnt = self.db_set.slot_count();
        for idx in 0..slot_cnt {
            let inst_id = self.db_set.slots()[idx].schema_instance_id;
            let already_loaded = self.db_set.slots()[idx].schema.loaded;
            let Some(tx_state) = self.db_set.slots()[idx]
                .btree
                .as_ref()
                .map(Btree::transaction_state)
            else {
                continue;
            };
            if tx_state == TransactionState::Open && already_loaded && !force_recheck_open {
                continue;
            }
            if tx_state == TransactionState::Open {
                {
                    let slot = &mut self.db_set.slots_mut()[idx];
                    let bt = slot.btree.as_mut().unwrap();
                    self.busy_policy.retry_btree(bt, |b| b.begin_read())?;
                }
                let can_end = !self.in_user_btree_tx
                    && self.activity.has_no_active_tx()
                    && !self.activity.has_active_tx_on_instance(inst_id);
                let slot = &mut self.db_set.slots_mut()[idx];
                let slot_name = slot.name.clone();
                let bt = slot.btree.as_mut().unwrap();
                let prev_cookie = slot.schema.schema_cookie;
                let prev_had_pages = slot.schema.had_pages;
                let prev_loaded = slot.schema.loaded;
                let load_res = slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name);
                if idx == 0 && bt.pager().page_count() > 0 {
                    self.connection_encoding = bt.encoding();
                }
                let end_res = if can_end {
                    bt.pager_mut().end_read_if_unused()
                } else {
                    Ok(())
                };
                load_res?;
                end_res?;
                if !prev_loaded
                    || prev_cookie != slot.schema.schema_cookie
                    || prev_had_pages != slot.schema.had_pages
                {
                    any_changed = true;
                }
            } else {
                let slot = &mut self.db_set.slots_mut()[idx];
                let slot_name = slot.name.clone();
                let bt = slot.btree.as_mut().unwrap();
                let prev_cookie = slot.schema.schema_cookie;
                let prev_had_pages = slot.schema.had_pages;
                let prev_loaded = slot.schema.loaded;
                slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name)?;
                if idx == 0 && bt.pager().page_count() > 0 {
                    self.connection_encoding = bt.encoding();
                }
                if !prev_loaded
                    || prev_cookie != slot.schema.schema_cookie
                    || prev_had_pages != slot.schema.had_pages
                {
                    any_changed = true;
                }
            }
        }
        if any_changed {
            self.db_set.rebind_all_collations(&self.collations);
            self.db_set.reconcile_temp_triggers()?;
            self.schema_generation += 1;
            self.v1_expire_generation += 1;
        }
        Ok(any_changed)
    }

    fn refresh_open_tx_schemas(&mut self) -> Result<()> {
        let mut any_changed = false;
        for (idx, slot) in self.db_set.slots_mut().iter_mut().enumerate() {
            let Some(ref mut bt) = slot.btree else {
                continue;
            };
            if bt.transaction_state() == TransactionState::Open {
                continue;
            }
            let has_pages = bt.pager().page_count() > 0;
            let cur_cookie = bt.pager().header().schema_cookie;
            if !slot.schema.loaded
                || slot.schema.had_pages != has_pages
                || slot.schema.schema_cookie != cur_cookie
            {
                slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot.name)?;
                if idx == 0 && has_pages {
                    self.connection_encoding = bt.encoding();
                }
                any_changed = true;
            }
        }
        if any_changed {
            self.db_set.rebind_all_collations(&self.collations);
            self.db_set.reconcile_temp_triggers()?;
            self.schema_generation += 1;
            self.v1_expire_generation += 1;
        }
        Ok(())
    }

    fn maybe_run_wal_autocheckpoint(&mut self) {
        if !self.is_autocommit() {
            return;
        }
        let threshold = self.wal_autocheckpoint_threshold;
        let slot_cnt = self.db_set.slot_count();
        for idx in 0..slot_cnt {
            let inst_id = self.db_set.slots()[idx].schema_instance_id;
            let busy = self.activity.has_active_tx_on_instance(inst_id);
            if let Some(ref mut bt) = self.db_set.slots_mut()[idx].btree {
                let n_entry = bt.pager_mut().take_wal_callback_frame_count();
                if n_entry > 0
                    && threshold > 0
                    && i64::from(n_entry) >= i64::from(threshold)
                    && bt.transaction_state() == TransactionState::Open
                    && !busy
                {
                    let _ = bt.wal_checkpoint(WalCheckpointMode::Passive, None);
                }
            }
        }
    }

    fn wal_checkpoint_internal(
        &mut self,
        db_name: Option<&str>,
        mode: WalCheckpointMode,
    ) -> Result<WalCheckpointResult> {
        let targets: Vec<DatabaseId> = match db_name {
            None | Some("") => (0..self.db_set.slot_count()).map(DatabaseId).collect(),
            Some(s) => {
                let Some(id) = self.db_set.find_db_id(s) else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("unknown database: {s}"),
                    ));
                };
                vec![id]
            }
        };

        let mut final_res = WalCheckpointResult::not_wal();
        for db_id in targets {
            if db_id == DatabaseId::TEMP {
                continue;
            }
            let Some(inst_id) = self.db_set.slot(db_id).map(|s| s.schema_instance_id) else {
                continue;
            };
            let has_bt = self.db_set.slot(db_id).is_some_and(|s| s.btree.is_some());
            if !has_bt {
                continue;
            }
            let active_on_slot = self.activity.has_active_tx_on_instance(inst_id)
                || (db_id == DatabaseId::MAIN && !self.activity.has_no_active_tx());
            let slot = self.db_set.slot_mut(db_id).unwrap();
            let bt = slot.btree.as_mut().unwrap();
            if bt.transaction_state() != TransactionState::Open || active_on_slot {
                return Err(SqliteError::new(
                    ErrorCode::Locked,
                    "database table is locked",
                ));
            }
            if bt.pager().db_kind() == DatabaseKind::NamedDisk
                && (!bt.pager().is_header_loaded()
                    || (bt.pager().journal_mode() == JournalMode::Wal
                        && bt.pager().wal().is_none()))
            {
                self.busy_policy.retry_btree(bt, |b| b.begin_read())?;
                bt.pager_mut().end_read_if_unused()?;
            }
            if bt.pager().wal().is_none() {
                continue;
            }
            let r = self.busy_policy.checkpoint_btree(bt, mode)?;
            final_res.busy |= r.busy;
            if final_res.n_log < 0 {
                final_res.n_log = r.n_log;
                final_res.n_ckpt = r.n_ckpt;
            }
        }
        Ok(final_res)
    }

    #[inline]
    fn finish_point_select_main_read(
        &mut self,
        hid: HandleId,
        main_inst: SchemaInstanceId,
        shared: &ConnectionShared,
    ) -> Result<()> {
        if self.simulated_cleanup_io_error.is_none()
            && self.activity.leases.len() == 1
            && self.activity.leases[0].0 == hid
            && self.db_set.slots().len() == 2
            && self.db_set.slots()[1].btree.is_none()
            && self.db_set.slots()[0].schema_instance_id == main_inst
        {
            self.activity.leases.clear();
            if !self.in_user_btree_tx
                && self.is_autocommit()
                && let Some(ref mut bt) = self.db_set.slots_mut()[0].btree
                && bt.transaction_state() == TransactionState::Read
                && let Err(e) = bt.pager_mut().end_read_if_unused()
            {
                return Err(self.poison_with(e));
            }
            return Ok(());
        }
        self.finish_activity(
            hid,
            ActivityFinish::ReadDone {
                lease_cursors: Vec::new(),
                read_instances: SmallInstVec::single(main_inst),
            },
            shared,
        )
    }

    fn finish_activity(
        &mut self,
        hid: HandleId,
        outcome: ActivityFinish,
        shared: &ConnectionShared,
    ) -> Result<()> {
        let _removed = self.activity.remove(hid);
        let summary = self.activity.summary_excluding(hid, &self.db_set);

        if let Some(sim_err) = self.take_simulated_cleanup_error() {
            match outcome {
                ActivityFinish::ReadDone { lease_cursors, .. }
                | ActivityFinish::ReadAborted { lease_cursors, .. } => {
                    for (inst, cid) in lease_cursors {
                        if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                            && let Some(ref mut bt) = slot.btree
                        {
                            bt.close_cursor(cid);
                        }
                    }
                }
                _ => {}
            }
            return Err(self.poison_with(sim_err));
        }

        match outcome {
            ActivityFinish::NoLock | ActivityFinish::PendingCommitAborted => Ok(()),

            ActivityFinish::ReadDone {
                lease_cursors,
                read_instances,
            }
            | ActivityFinish::ReadAborted {
                lease_cursors,
                read_instances,
            } => {
                for (inst, cid) in lease_cursors {
                    if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                        && let Some(ref mut bt) = slot.btree
                    {
                        bt.close_cursor(cid);
                    }
                }
                if !self.in_user_btree_tx && self.is_autocommit() {
                    let has_explicit_insts = !read_instances.is_empty();
                    let active_mask = summary.other_read_db_mask | summary.other_write_db_mask;
                    for inst in read_instances {
                        if let Some(db_id) = self.db_set.find_db_id_by_instance(inst) {
                            let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                            if (active_mask & bit) == 0
                                && summary.other_writers == 0
                                && let Some(slot) = self.db_set.slot_mut(db_id)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Read
                                && let Err(e) = bt.pager_mut().end_read_if_unused()
                            {
                                return Err(self.poison_with(e));
                            }
                        }
                    }
                    let need_sweep = !has_explicit_insts
                        || self.db_set.slots().len() > 2
                        || self
                            .db_set
                            .slots()
                            .get(1)
                            .is_some_and(|s| s.btree.is_some());
                    if need_sweep && summary.other_readers == 0 && summary.other_writers == 0 {
                        for slot in self.db_set.slots_mut() {
                            if let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Read
                                && let Err(e) = bt.pager_mut().end_read_if_unused()
                            {
                                return Err(self.poison_with(e));
                            }
                        }
                    }
                }
                Ok(())
            }

            ActivityFinish::WriteCompleted { abort_snapshot } => {
                if !self.is_autocommit() {
                    if let Some(sp_slot) = abort_snapshot.stmt_sp_slot
                        && sp_slot != ELIDED_STMT_SP
                    {
                        for &inst in &abort_snapshot.touched_write_instances {
                            if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(e) = bt.pager_mut().release_subsavepoint(sp_slot)
                            {
                                return Err(self.poison_with(e));
                            }
                        }
                        if self.db_set.vtab_registry().has_any_in_tx()
                            && let Err(e) = self.with_default_exec_ctx(shared, summary, |ctx| {
                                crate::vtab::vtab_release_all(ctx, sp_slot)
                            })
                        {
                            return Err(self.poison_with(e));
                        }
                    }
                    if summary.other_writers == 0 && summary.other_readers == 0 {
                        for slot in self.db_set.slots_mut() {
                            if let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(e) = bt.spill_if_over_cache_size()
                            {
                                return Err(self.poison_with(e));
                            }
                        }
                    }
                    self.publish(shared);
                    return Ok(());
                }

                if summary.other_writers > 0 {
                    if let Some(sp_slot) = abort_snapshot.stmt_sp_slot {
                        for &inst in &abort_snapshot.touched_write_instances {
                            if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(e) = bt.pager_mut().release_subsavepoint(sp_slot)
                            {
                                return Err(self.poison_with(e));
                            }
                        }
                        if self.db_set.vtab_registry().has_any_in_tx()
                            && let Err(e) = self.with_default_exec_ctx(shared, summary, |ctx| {
                                crate::vtab::vtab_release_all(ctx, sp_slot)
                            })
                        {
                            return Err(self.poison_with(e));
                        }
                    }
                    self.publish(shared);
                    return Ok(());
                }

                // Autocommit write completion with no other active writers: run 6-step 2PC coordinator!
                let mut expire_self_unused = false;
                let mut stmt_tx_unused = StmtTxTracker::default();
                let commit_res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        db_set: &mut self.db_set,
                        connection_encoding: &mut self.connection_encoding,
                        uri_enabled: shared.open_options.uri,
                        is_connection_readonly: shared.is_readonly,
                        runtime: &mut self.runtime,
                        interrupt_flag: &self.active_interrupt_flag,
                        functions: &self.functions,
                        collations: &self.collations,
                        progress: &mut self.progress,
                        commit_hooks: &mut self.commit_hooks,
                        mutation_pipeline: &mut self.mutation_pipeline,
                        strict_subtype: self.strict_subtype,
                        explicit_tx: &mut self.explicit_tx,
                        implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
                        in_user_btree_tx: &mut self.in_user_btree_tx,
                        savepoints: &mut self.savepoints,
                        schema_generation: &mut self.schema_generation,
                        v1_expire_generation: &mut self.v1_expire_generation,
                        expire_self_only: &mut expire_self_unused,
                        schema_changed_in_tx: &mut self.schema_changed_in_tx,
                        activity: summary,
                        stmt_tx: &mut stmt_tx_unused,
                        foreign_keys: &mut self.foreign_keys,
                        recursive_triggers: &mut self.recursive_triggers,
                        legacy_alter_table: &mut self.legacy_alter_table,
                        count_changes: &mut self.count_changes,
                        wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut self.default_locking_mode,
                        writable_schema: &mut self.writable_schema,
                        working_dir: &self.working_dir,
                        busy_policy: &mut self.busy_policy,
                        rollback_count: &mut self.rollback_count,
                        deferred_fk_suspects: &mut self.deferred_fk_suspects,
                        trace_hook: &mut self.trace_hook,
                        publish_mirrors: &mut publish_fn,
                    };
                    match commit_all_databases(&mut ctx) {
                        Ok(()) => Ok(()),
                        Err((cm_err, disp)) => {
                            let rb_res = if disp == CommitFailureDisposition::LeaveOpen {
                                rollback_all_databases(&mut ctx, false)
                            } else {
                                Ok(())
                            };
                            Err((cm_err, rb_res))
                        }
                    }
                };

                if let Err((cm_err, rb_res)) = commit_res {
                    if let Err(rb_err) = rb_res {
                        let root_err = if cm_err.code == ErrorCode::Busy {
                            rb_err
                        } else {
                            cm_err
                        };
                        self.deferred_fk_suspects.clear();
                        self.schema_changed_in_tx = false;
                        self.publish(shared);
                        return Err(self.poison_with(root_err));
                    }
                    self.runtime.last_changes = if abort_snapshot.is_dml {
                        0
                    } else {
                        abort_snapshot.rt_before.last_changes
                    };
                    self.runtime.total_changes = abort_snapshot.rt_before.total_changes;
                    self.runtime.last_insert_rowid = abort_snapshot.rt_before.last_insert_rowid;
                    self.mutation_pipeline
                        .truncate(abort_snapshot.mutation_len_before);
                    self.deferred_fk_suspects.clear();
                    if self.schema_changed_in_tx
                        || self.schema_generation != abort_snapshot.schema_gen_before
                    {
                        self.restore_schemas_snapshot(&abort_snapshot.schemas_before);
                        self.schema_generation += 1;
                        self.v1_expire_generation += 1;
                        self.schema_changed_in_tx = false;
                    }
                    self.publish(shared);
                    return Err(cm_err);
                }

                self.schema_changed_in_tx = false;
                self.deferred_fk_suspects.clear();
                self.publish(shared);
                self.maybe_run_wal_autocheckpoint();
                Ok(())
            }
        }
    }

    fn with_default_exec_ctx<T>(
        &mut self,
        shared: &ConnectionShared,
        activity: ActivitySummary,
        f: impl FnOnce(&mut ExecContext<'_>) -> T,
    ) -> T {
        let mut expire_self_unused = false;
        let mut stmt_tx_unused = StmtTxTracker::default();
        self.with_exec_ctx(
            shared,
            activity,
            &mut stmt_tx_unused,
            &mut expire_self_unused,
            f,
        )
    }

    fn abort_write_statement(
        &mut self,
        hid: HandleId,
        err: &SqliteError,
        snap: WriteAbortSnapshot,
        shared: &ConnectionShared,
    ) -> SqliteError {
        self.activity.remove(hid);
        if self.activity.leases.is_empty() {
            shared.interrupt_flag.store(false, Ordering::SeqCst);
        }
        let summary = self.activity.summary_excluding(hid, &self.db_set);

        if let Some(sim_err) = self.take_simulated_cleanup_error() {
            return self.poison_with(sim_err);
        }

        // `SQLITE_FULL` in SQLite (`vdbeaux.c` / `btree.c`) rolls back the full transaction
        // unless handled inside a savepoint? Wait: in `run_write_autocommit` (`engine.rs:2279`),
        // `ErrorCode::Full` rolls back the full transaction! Check if `err.conflict_effect == Rollback` or `err.code == ErrorCode::Full`!
        let effective_effect = if err.code == ErrorCode::Full
            || (snap.stmt_sp_slot == Some(ELIDED_STMT_SP)
                && matches!(
                    err.code,
                    ErrorCode::IoErr | ErrorCode::NoMem | ErrorCode::Corrupt
                )) {
            ConflictResolutionEffect::Rollback
        } else {
            err.conflict_effect
        };

        match effective_effect {
            ConflictResolutionEffect::Rollback => {
                let was_autocommit = self.is_autocommit();
                let schema_changed =
                    self.schema_changed_in_tx || self.schema_generation != snap.schema_gen_before;
                let mut expire_self_unused = false;
                let mut stmt_tx_unused = StmtTxTracker::default();
                let rb_res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        db_set: &mut self.db_set,
                        connection_encoding: &mut self.connection_encoding,
                        uri_enabled: shared.open_options.uri,
                        is_connection_readonly: shared.is_readonly,
                        runtime: &mut self.runtime,
                        interrupt_flag: &self.active_interrupt_flag,
                        functions: &self.functions,
                        collations: &self.collations,
                        progress: &mut self.progress,
                        commit_hooks: &mut self.commit_hooks,
                        mutation_pipeline: &mut self.mutation_pipeline,
                        strict_subtype: self.strict_subtype,
                        explicit_tx: &mut self.explicit_tx,
                        implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
                        in_user_btree_tx: &mut self.in_user_btree_tx,
                        savepoints: &mut self.savepoints,
                        schema_generation: &mut self.schema_generation,
                        v1_expire_generation: &mut self.v1_expire_generation,
                        expire_self_only: &mut expire_self_unused,
                        schema_changed_in_tx: &mut self.schema_changed_in_tx,
                        activity: summary,
                        stmt_tx: &mut stmt_tx_unused,
                        foreign_keys: &mut self.foreign_keys,
                        recursive_triggers: &mut self.recursive_triggers,
                        legacy_alter_table: &mut self.legacy_alter_table,
                        count_changes: &mut self.count_changes,
                        wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut self.default_locking_mode,
                        writable_schema: &mut self.writable_schema,
                        working_dir: &self.working_dir,
                        busy_policy: &mut self.busy_policy,
                        rollback_count: &mut self.rollback_count,
                        deferred_fk_suspects: &mut self.deferred_fk_suspects,
                        trace_hook: &mut self.trace_hook,
                        publish_mirrors: &mut publish_fn,
                    };
                    rollback_all_databases(&mut ctx, false)
                };
                if let Err(rb_err) = rb_res {
                    return self.poison_with(rb_err);
                }
                self.explicit_tx = false;
                self.implicit_savepoint_tx = false;
                self.in_user_btree_tx = false;
                self.savepoints.clear();
                self.deferred_fk_suspects.clear();
                if !was_autocommit || err.conflict_effect == ConflictResolutionEffect::Rollback {
                    self.rollback_count += 1;
                }
                self.runtime.last_changes = if snap.is_dml {
                    0
                } else {
                    snap.rt_before.last_changes
                };
                self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
                if was_autocommit {
                    self.runtime.total_changes = snap.rt_before.total_changes;
                    self.mutation_pipeline.truncate(snap.mutation_len_before);
                } else {
                    self.mutation_pipeline.clear();
                }
                if schema_changed {
                    self.schema_changed_in_tx = false;
                    self.restore_schemas_snapshot(&snap.schemas_before);
                    for slot in self.db_set.slots_mut() {
                        if let Some(ref mut bt) = slot.btree
                            && let Ok(s) = Schema::load_for_schema(bt, Some(&slot.name))
                        {
                            slot.schema = s;
                        }
                    }
                    self.db_set.rebind_all_collations(&self.collations);
                    let _ = self.db_set.reconcile_temp_triggers();
                    self.schema_generation += 1;
                    self.v1_expire_generation += 1;
                }
                self.publish(shared);
                if let Err(hk_err) = self.commit_hooks.invoke_rollback_hook() {
                    return hk_err;
                }
                err.clone()
            }

            ConflictResolutionEffect::Fail => {
                let mut committed_autocommit = false;
                if !self.is_autocommit() || summary.other_writers > 0 {
                    if let Some(sp_slot) = snap.stmt_sp_slot
                        && sp_slot != ELIDED_STMT_SP
                    {
                        for &inst in &snap.touched_write_instances {
                            if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(sp_err) = bt.pager_mut().release_subsavepoint(sp_slot)
                            {
                                return self.poison_with(sp_err);
                            }
                        }
                        if self.db_set.vtab_registry().has_any_in_tx()
                            && let Err(sp_err) =
                                self.with_default_exec_ctx(shared, summary, |ctx| {
                                    crate::vtab::vtab_release_all(ctx, sp_slot)
                                })
                        {
                            return self.poison_with(sp_err);
                        }
                    }
                    if summary.other_writers == 0 && summary.other_readers == 0 {
                        for slot in self.db_set.slots_mut() {
                            if let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(sp_err) = bt.spill_if_over_cache_size()
                            {
                                return self.poison_with(sp_err);
                            }
                        }
                    }
                } else if self.db_set.any_in_transaction() {
                    let mut expire_self_unused = false;
                    let mut stmt_tx_unused = StmtTxTracker::default();
                    let commit_res = {
                        let mut publish_fn =
                            |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                                shared.publish_mirrors(rt, exp, imp, rb);
                            };
                        let mut ctx = ExecContext {
                            db_set: &mut self.db_set,
                            connection_encoding: &mut self.connection_encoding,
                            uri_enabled: shared.open_options.uri,
                            is_connection_readonly: shared.is_readonly,
                            runtime: &mut self.runtime,
                            interrupt_flag: &self.active_interrupt_flag,
                            functions: &self.functions,
                            collations: &self.collations,
                            progress: &mut self.progress,
                            commit_hooks: &mut self.commit_hooks,
                            mutation_pipeline: &mut self.mutation_pipeline,
                            strict_subtype: self.strict_subtype,
                            explicit_tx: &mut self.explicit_tx,
                            implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
                            in_user_btree_tx: &mut self.in_user_btree_tx,
                            savepoints: &mut self.savepoints,
                            schema_generation: &mut self.schema_generation,
                            v1_expire_generation: &mut self.v1_expire_generation,
                            expire_self_only: &mut expire_self_unused,
                            schema_changed_in_tx: &mut self.schema_changed_in_tx,
                            activity: summary,
                            stmt_tx: &mut stmt_tx_unused,
                            foreign_keys: &mut self.foreign_keys,
                            recursive_triggers: &mut self.recursive_triggers,
                            legacy_alter_table: &mut self.legacy_alter_table,
                            count_changes: &mut self.count_changes,
                            wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                            default_locking_mode: &mut self.default_locking_mode,
                            writable_schema: &mut self.writable_schema,
                            working_dir: &self.working_dir,
                            busy_policy: &mut self.busy_policy,
                            rollback_count: &mut self.rollback_count,
                            deferred_fk_suspects: &mut self.deferred_fk_suspects,
                            trace_hook: &mut self.trace_hook,
                            publish_mirrors: &mut publish_fn,
                        };
                        match commit_all_databases(&mut ctx) {
                            Ok(()) => Ok(()),
                            Err((cm_err, disp)) => {
                                let rb_res = if disp == CommitFailureDisposition::LeaveOpen {
                                    rollback_all_databases(&mut ctx, false)
                                } else {
                                    Ok(())
                                };
                                Err((cm_err, rb_res))
                            }
                        }
                    };
                    if let Err((cm_err, rb_res)) = commit_res {
                        if let Err(rb_err) = rb_res {
                            let root_err = if cm_err.code == ErrorCode::Busy {
                                rb_err
                            } else {
                                cm_err
                            };
                            self.deferred_fk_suspects.clear();
                            self.schema_changed_in_tx = false;
                            self.publish(shared);
                            return self.poison_with(root_err);
                        }
                        self.runtime.last_changes = if snap.is_dml {
                            0
                        } else {
                            snap.rt_before.last_changes
                        };
                        self.runtime.total_changes = snap.rt_before.total_changes;
                        self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
                        self.mutation_pipeline.truncate(snap.mutation_len_before);
                        self.deferred_fk_suspects.clear();
                        if self.schema_changed_in_tx
                            || self.schema_generation != snap.schema_gen_before
                        {
                            self.restore_schemas_snapshot(&snap.schemas_before);
                            self.schema_generation += 1;
                            self.v1_expire_generation += 1;
                            self.schema_changed_in_tx = false;
                        }
                        self.publish(shared);
                        return cm_err;
                    }
                    self.schema_changed_in_tx = false;
                    self.deferred_fk_suspects.clear();
                    committed_autocommit = true;
                }
                self.publish(shared);
                if committed_autocommit {
                    self.maybe_run_wal_autocheckpoint();
                }
                err.clone()
            }

            ConflictResolutionEffect::Abort => {
                let stmt_changed_schema = self.schema_generation != snap.schema_gen_before;
                self.runtime.last_changes = if snap.is_dml {
                    0
                } else {
                    snap.rt_before.last_changes
                };
                self.runtime.total_changes = snap.rt_before.total_changes;
                self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
                self.mutation_pipeline.truncate(snap.mutation_len_before);
                self.deferred_fk_suspects = snap.fk_before;

                if let Some(sp_slot) = snap.stmt_sp_slot {
                    // Inside `BEGIN` / `SAVEPOINT` (or concurrent writer): roll back ONLY this
                    // statement's subsavepoint on databases written by this statement!
                    for &inst in &snap.touched_write_instances {
                        if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                            && let Some(ref mut bt) = slot.btree
                        {
                            let _ = bt.trip_cursors(
                                extended::SQLITE_ABORT_ROLLBACK,
                                !stmt_changed_schema,
                            );
                            if sp_slot != ELIDED_STMT_SP
                                && bt.transaction_state() == TransactionState::Write
                            {
                                if let Err(rb_err) =
                                    bt.pager_mut().rollback_to_subsavepoint(sp_slot)
                                {
                                    return self.poison_with(rb_err);
                                }
                                if let Err(rel_err) = bt.pager_mut().release_subsavepoint(sp_slot) {
                                    return self.poison_with(rel_err);
                                }
                            }
                        }
                    }
                    if sp_slot != ELIDED_STMT_SP && self.db_set.vtab_registry().has_any_in_tx() {
                        self.with_default_exec_ctx(shared, summary, |ctx| {
                            let _ = crate::vtab::vtab_rollback_to_all(ctx, sp_slot);
                            let _ = crate::vtab::vtab_release_all(ctx, sp_slot);
                        });
                    }
                    if stmt_changed_schema {
                        self.restore_schemas_snapshot(&snap.schemas_before);
                        self.schema_generation += 1;
                        self.v1_expire_generation = snap.v1_expire_gen_before + 1;
                    }
                    self.schema_changed_in_tx = snap.schema_changed_before;
                } else {
                    // Autocommit statement abort: roll back write DBs and end reads started here!
                    self.with_default_exec_ctx(shared, summary, |ctx| {
                        crate::vtab::vtab_rollback_all(ctx)
                    });
                    let write_only = !stmt_changed_schema && !self.schema_changed_in_tx;
                    for &inst in &snap.touched_write_instances {
                        if let Some(db_id) = self.db_set.find_db_id_by_instance(inst) {
                            let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                            let retain_read =
                                ((summary.other_read_db_mask & bit) != 0) && write_only;
                            if let Some(slot) = self.db_set.slot_mut(db_id)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(rb_err) = bt.rollback_with_trip(
                                    extended::SQLITE_ABORT_ROLLBACK,
                                    write_only,
                                    retain_read,
                                )
                            {
                                return self.poison_with(rb_err);
                            }
                        }
                    }
                    let active_mask = summary.other_read_db_mask | summary.other_write_db_mask;
                    for &inst in &snap.started_read_instances {
                        if let Some(db_id) = self.db_set.find_db_id_by_instance(inst) {
                            let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                            if (active_mask & bit) == 0
                                && let Some(slot) = self.db_set.slot_mut(db_id)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Read
                                && let Err(end_err) = bt.pager_mut().end_read_if_unused()
                            {
                                return self.poison_with(end_err);
                            }
                        }
                    }
                    if stmt_changed_schema || self.schema_changed_in_tx {
                        self.restore_schemas_snapshot(&snap.schemas_before);
                        self.schema_generation += 1;
                        self.v1_expire_generation += 1;
                        self.schema_changed_in_tx = false;
                    }
                }
                self.publish(shared);
                err.clone()
            }
        }
    }

    fn finish_running_exec(
        &mut self,
        hid: HandleId,
        running: RunningExec,
        shared: &ConnectionShared,
    ) -> Result<()> {
        match running {
            RunningExec::Vdbe {
                mut vm,
                read_instances,
            } => {
                vm.close_cursors(&mut self.db_set);
                if self.pooled_read_vm.is_none() {
                    self.pooled_read_vm = Some(vm);
                }
                self.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                        read_instances,
                    },
                    shared,
                )
            }
            RunningExec::PointSelectRowYielded { main_inst } => {
                self.finish_point_select_main_read(hid, main_inst, shared)
            }
            RunningExec::MaterializedRead {
                lease_cursors,
                read_instances,
                ..
            }
            | RunningExec::MaterializedInlineRead {
                lease_cursors,
                read_instances,
                ..
            }
            | RunningExec::FastSingleTextArena {
                lease_cursors,
                read_instances,
                ..
            } => self.finish_activity(
                hid,
                ActivityFinish::ReadDone {
                    lease_cursors,
                    read_instances,
                },
                shared,
            ),
            RunningExec::MaterializedNoLock { .. } => {
                self.finish_activity(hid, ActivityFinish::NoLock, shared)
            }
            RunningExec::ReturningWrite { abort_snapshot, .. }
            | RunningExec::IncrementalVacuum { abort_snapshot, .. } => self.finish_activity(
                hid,
                ActivityFinish::WriteCompleted { abort_snapshot },
                shared,
            ),
            RunningExec::PendingCommit { last_busy, .. } => {
                let _ = self.finish_activity(hid, ActivityFinish::PendingCommitAborted, shared);
                Err(last_busy)
            }
        }
    }

    fn process_pending_drops(&mut self, items: Vec<PendingDrop>, shared: &ConnectionShared) {
        for item in items {
            match item {
                PendingDrop::Statement { handle_id, running } => {
                    if let Some(run) = running {
                        let _ = self.finish_running_exec(handle_id, *run, shared);
                    }
                    self.open_handles.remove(&handle_id);
                }
                PendingDrop::Blob {
                    handle_id,
                    schema_instance_id,
                    writable,
                } => {
                    let _ = self.close_blob_handle(handle_id, schema_instance_id, writable, shared);
                }
                PendingDrop::BackupDestination {
                    handle_id,
                    dst_instance_id,
                    in_write_tx,
                    active_lease,
                } => {
                    if let Some(slot) = self.db_set.slot_by_instance_mut(dst_instance_id) {
                        slot.active_backups = slot.active_backups.saturating_sub(1);
                        if in_write_tx && let Some(ref mut bt) = slot.btree {
                            bt.pager_mut().backup_reset_raw_staging();
                            if bt.transaction_state() == TransactionState::Write {
                                let _ = bt.rollback_with_trip(
                                    extended::SQLITE_ABORT_ROLLBACK,
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                    if active_lease {
                        self.activity.remove(handle_id);
                    }
                    self.open_handles.remove(&handle_id);
                }
                PendingDrop::BackupSource {
                    src_instance_id,
                    tracker,
                } => {
                    if let Some(slot) = self.db_set.slot_by_instance_mut(src_instance_id) {
                        slot.active_backups = slot.active_backups.saturating_sub(1);
                        slot.detach_backup_tracker(&tracker);
                    }
                }
                PendingDrop::TestBlob {
                    handle_id,
                    lease_cursor,
                    writable,
                } => {
                    let main_inst = self.db_set.main_slot().schema_instance_id;
                    if writable {
                        let dummy_snap = WriteAbortSnapshot {
                            stmt_sp_slot: None,
                            touched_write_instances: SmallInstVec::single(main_inst),
                            started_read_instances: SmallInstVec::new(),
                            fk_before: self.deferred_fk_suspects.clone(),
                            rt_before: self.runtime,
                            is_dml: false,
                            mutation_len_before: self.mutation_pipeline.len(),
                            schemas_before: self.snapshot_open_schemas(),
                            schema_gen_before: self.schema_generation,
                            v1_expire_gen_before: self.v1_expire_generation,
                            schema_changed_before: self.schema_changed_in_tx,
                        };
                        let _ = self.finish_activity(
                            handle_id,
                            ActivityFinish::WriteCompleted {
                                abort_snapshot: dummy_snap,
                            },
                            shared,
                        );
                    } else {
                        let read_insts = match lease_cursor {
                            Some((inst, _)) => SmallInstVec::single(inst),
                            None => SmallInstVec::single(main_inst),
                        };
                        let cursors = lease_cursor.into_iter().collect();
                        let _ = self.finish_activity(
                            handle_id,
                            ActivityFinish::ReadDone {
                                lease_cursors: cursors,
                                read_instances: read_insts,
                            },
                            shared,
                        );
                    }
                    self.open_handles.remove(&handle_id);
                }
            }
        }
        if std::mem::replace(&mut self.panic_on_next_pending_drop, false) {
            panic!("simulated panic during process_pending_drops");
        }
    }
}

/// SQLite database connection facade (`sqlite3*` in `sqlite.h.in`).
///
/// Wraps a thread-safe `Arc<ConnectionShared>` (`Connection: Send + Sync`) while [`Statement`]s
/// borrow no `'conn` lifetime and coordinate access through the connection entry gate.
pub struct Connection {
    shared: Arc<ConnectionShared>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("conn_id", &self.shared.conn_id)
            .field("database_kind", &self.shared.database_kind)
            .field("db_path", &self.shared.db_path)
            .field("is_readonly", &self.shared.is_readonly)
            .finish()
    }
}

impl Connection {
    /// Opens or creates a SQLite Format 3 database (`":memory:"` -> [`DatabaseKind::Memory`],
    /// `""` -> isolated [`DatabaseKind::PrivateTemporary`], named path -> [`DatabaseKind::NamedDisk`]).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(
            path,
            OpenFlags::READWRITE | OpenFlags::CREATE,
            ConnectionOpenOptions::default(),
        )
    }

    /// Opens an isolated in-memory database (`":memory:"`, [`DatabaseKind::Memory`]).
    pub fn open_in_memory() -> Result<Self> {
        Self::open_in_memory_with_options(ConnectionOpenOptions::default())
    }

    /// Opens an isolated in-memory database with explicit [`ConnectionOpenOptions`].
    pub fn open_in_memory_with_options(options: ConnectionOpenOptions) -> Result<Self> {
        let pager_opts = options.to_pager_options(DatabaseKind::Memory);
        let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(options.lock_geometry));
        let disk_vfs = os_vfs_for_geometry(options.lock_geometry);
        let btree = Btree::open_with_options(
            mem_vfs,
            Some(Path::new(":memory:")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
            pager_opts,
        )?;
        Self::from_btree_with_disk_vfs(btree, disk_vfs, options, false)
    }

    /// Opens an isolated private temporary database (`""`, [`DatabaseKind::PrivateTemporary`]).
    pub fn open_private_temporary() -> Result<Self> {
        Self::open_private_temporary_with_options(ConnectionOpenOptions::default())
    }

    /// Opens an isolated private temporary database (`""`) with explicit [`ConnectionOpenOptions`].
    pub fn open_private_temporary_with_options(options: ConnectionOpenOptions) -> Result<Self> {
        let pager_opts = options.to_pager_options(DatabaseKind::PrivateTemporary);
        let disk_vfs = os_vfs_for_geometry(options.lock_geometry);
        let btree = Btree::open_private_temporary(Arc::clone(&disk_vfs), pager_opts)?;
        Self::from_btree_with_disk_vfs(btree, disk_vfs, options, false)
    }

    /// Opens a database with explicit [`OpenFlags`] (e.g., `OpenFlags::READONLY`).
    pub fn open_with_flags(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        Self::open_with_options(path, flags, ConnectionOpenOptions::default())
    }

    /// Opens an existing disk database in read-only `immutable` mode (`file:...?immutable=1`):
    /// skips OS file locking and `-shm` creation while still replaying any existing `-wal` frames
    /// into a private heap-backed WAL index.
    pub fn open_immutable(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_immutable_with_options(path, ConnectionOpenOptions::default())
    }

    /// Opens an existing disk database in read-only `immutable` mode with explicit
    /// [`ConnectionOpenOptions`].
    pub fn open_immutable_with_options(
        path: impl AsRef<Path>,
        options: ConnectionOpenOptions,
    ) -> Result<Self> {
        let p = path.as_ref();
        let disk_vfs = os_vfs_for_geometry(options.lock_geometry);
        let mut pager_opts = options.to_pager_options(DatabaseKind::NamedDisk);
        pager_opts.immutable = true;
        let btree = Btree::open_with_options(
            Arc::clone(&disk_vfs),
            Some(p),
            OpenFlags::READONLY,
            pager_opts,
        )?;
        Self::from_btree_with_disk_vfs(btree, disk_vfs, options, false)
    }

    /// Opens a database with explicit [`OpenFlags`] and [`ConnectionOpenOptions`].
    pub fn open_with_options(
        path: impl AsRef<Path>,
        flags: OpenFlags,
        options: ConnectionOpenOptions,
    ) -> Result<Self> {
        let p = path.as_ref();
        let uri_enabled = flags.contains(OpenFlags::URI) || options.uri;
        let mut effective_options = options;
        if flags.contains(OpenFlags::URI) {
            effective_options.uri = true;
        }
        let disk_vfs = os_vfs_for_geometry(options.lock_geometry);

        if uri_enabled
            && let Some(s) = p.to_str()
            && s.starts_with("file:")
        {
            let parsed = parse_sqlite_uri(
                s,
                true,
                flags.contains(OpenFlags::READONLY),
                flags.contains(OpenFlags::CREATE),
                None,
            )?;
            if parsed.is_memory {
                let pager_opts = effective_options.to_pager_options(DatabaseKind::Memory);
                let mem_vfs: Arc<dyn Vfs> =
                    Arc::new(MemoryVfs::with_lock_geometry(options.lock_geometry));
                let btree = Btree::open_with_options(
                    mem_vfs,
                    Some(Path::new(":memory:")),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
                    pager_opts,
                )?;
                return Self::from_btree_with_disk_vfs(
                    btree,
                    disk_vfs,
                    effective_options,
                    parsed.enable_8_3_names,
                );
            }
            if parsed.is_private_temp {
                if parsed.readonly && !parsed.create_if_missing {
                    return Err(SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("unable to open database: {s}"),
                    ));
                }
                let pager_opts = effective_options.to_pager_options(DatabaseKind::PrivateTemporary);
                let btree = Btree::open_private_temporary(Arc::clone(&disk_vfs), pager_opts)?;
                return Self::from_btree_with_disk_vfs(
                    btree,
                    disk_vfs,
                    effective_options,
                    parsed.enable_8_3_names,
                );
            }
            let mut pager_opts = effective_options.to_pager_options(DatabaseKind::NamedDisk);
            pager_opts.immutable = parsed.immutable;
            let effective_flags = if parsed.readonly {
                OpenFlags::READONLY
            } else if parsed.create_if_missing {
                OpenFlags::READWRITE | OpenFlags::CREATE
            } else {
                OpenFlags::READWRITE
            };
            let btree = Btree::open_with_options(
                Arc::clone(&disk_vfs),
                Some(Path::new(&parsed.path)),
                effective_flags,
                pager_opts,
            )?;
            return Self::from_btree_with_disk_vfs(
                btree,
                disk_vfs,
                effective_options,
                parsed.enable_8_3_names,
            );
        }

        if p == Path::new(":memory:") || flags.contains(OpenFlags::MEMORY) {
            let mem_vfs: Arc<dyn Vfs> =
                Arc::new(MemoryVfs::with_lock_geometry(options.lock_geometry));
            let pager_opts = effective_options.to_pager_options(DatabaseKind::Memory);
            let btree = Btree::open_with_options(
                mem_vfs,
                Some(p),
                OpenFlags(flags.0 & !OpenFlags::URI.0) | OpenFlags::MEMORY,
                pager_opts,
            )?;
            return Self::from_btree_with_disk_vfs(btree, disk_vfs, effective_options, false);
        }

        Self::open_with_vfs_and_options(disk_vfs, Some(p), flags, effective_options)
    }

    /// Opens a database backed by a custom [`Vfs`].
    pub fn open_with_vfs(vfs: Arc<dyn Vfs>, path: Option<&Path>, flags: OpenFlags) -> Result<Self> {
        let options = ConnectionOpenOptions {
            lock_geometry: vfs.lock_geometry(),
            uri: flags.contains(OpenFlags::URI),
            ..ConnectionOpenOptions::default()
        };
        Self::open_with_vfs_and_options(vfs, path, flags, options)
    }

    /// Opens a database backed by a custom [`Vfs`] and [`ConnectionOpenOptions`].
    pub fn open_with_vfs_and_options(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
        options: ConnectionOpenOptions,
    ) -> Result<Self> {
        let mut effective_options = options;
        if flags.contains(OpenFlags::URI) {
            effective_options.uri = true;
        }
        if effective_options.uri
            && let Some(p) = path
            && let Some(s) = p.to_str()
            && s.starts_with("file:")
        {
            let parsed = parse_sqlite_uri(
                s,
                true,
                flags.contains(OpenFlags::READONLY),
                flags.contains(OpenFlags::CREATE),
                Some(vfs.name()),
            )?;
            if parsed.is_memory {
                let pager_opts = effective_options.to_pager_options(DatabaseKind::Memory);
                let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(
                    effective_options.lock_geometry,
                ));
                let btree = Btree::open_with_options(
                    mem_vfs,
                    Some(Path::new(":memory:")),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
                    pager_opts,
                )?;
                return Self::from_btree_with_disk_vfs(
                    btree,
                    vfs,
                    effective_options,
                    parsed.enable_8_3_names,
                );
            }
            if parsed.is_private_temp {
                if parsed.readonly && !parsed.create_if_missing {
                    return Err(SqliteError::new(
                        ErrorCode::CantOpen,
                        format!("unable to open database: {s}"),
                    ));
                }
                let pager_opts = effective_options.to_pager_options(DatabaseKind::PrivateTemporary);
                let btree = Btree::open_private_temporary(Arc::clone(&vfs), pager_opts)?;
                return Self::from_btree_with_disk_vfs(
                    btree,
                    vfs,
                    effective_options,
                    parsed.enable_8_3_names,
                );
            }
            let mut pager_opts = effective_options.to_pager_options(DatabaseKind::NamedDisk);
            pager_opts.immutable = parsed.immutable;
            let effective_flags = if parsed.readonly {
                OpenFlags::READONLY
            } else if parsed.create_if_missing {
                OpenFlags::READWRITE | OpenFlags::CREATE
            } else {
                OpenFlags::READWRITE
            };
            let btree = Btree::open_with_options(
                Arc::clone(&vfs),
                Some(Path::new(&parsed.path)),
                effective_flags,
                pager_opts,
            )?;
            return Self::from_btree_with_disk_vfs(
                btree,
                vfs,
                effective_options,
                parsed.enable_8_3_names,
            );
        }

        let inferred_kind = if flags.contains(OpenFlags::MEMORY) {
            DatabaseKind::Memory
        } else {
            match path {
                None => DatabaseKind::PrivateTemporary,
                Some(p) if p.as_os_str().is_empty() => DatabaseKind::PrivateTemporary,
                Some(p) if p == Path::new(":memory:") => DatabaseKind::Memory,
                Some(_) => DatabaseKind::NamedDisk,
            }
        };
        let pager_opts = effective_options.to_pager_options(inferred_kind);
        let (main_vfs, disk_vfs): (Arc<dyn Vfs>, Arc<dyn Vfs>) =
            if inferred_kind == DatabaseKind::Memory {
                let mem_vfs: Arc<dyn Vfs> = if vfs.name() == "memdb" {
                    Arc::clone(&vfs)
                } else {
                    Arc::new(MemoryVfs::with_lock_geometry(
                        effective_options.lock_geometry,
                    ))
                };
                let d_vfs: Arc<dyn Vfs> = if vfs.name() == "memdb" {
                    os_vfs_for_geometry(effective_options.lock_geometry)
                } else {
                    vfs
                };
                (mem_vfs, d_vfs)
            } else {
                (Arc::clone(&vfs), vfs)
            };
        let btree = if inferred_kind == DatabaseKind::PrivateTemporary
            && !flags.contains(OpenFlags::TEMP_DB)
        {
            Btree::open_private_temporary(main_vfs, pager_opts)?
        } else {
            let effective_flags = if inferred_kind == DatabaseKind::Memory {
                OpenFlags((flags.0 & !OpenFlags::URI.0) | OpenFlags::MEMORY.0)
            } else {
                OpenFlags(flags.0 & !OpenFlags::URI.0)
            };
            Btree::open_with_options(main_vfs, path, effective_flags, pager_opts)?
        };
        Self::from_btree_with_disk_vfs(btree, disk_vfs, effective_options, false)
    }

    fn from_btree_with_disk_vfs(
        mut btree: Btree,
        disk_vfs: Arc<dyn Vfs>,
        open_options: ConnectionOpenOptions,
        main_enable_8_3_names: bool,
    ) -> Result<Self> {
        let interrupt_flag = Arc::new(AtomicBool::new(false));
        btree.set_interrupt_flag(Some(Arc::clone(&interrupt_flag)));

        let db_path = btree.pager().db_path().map(Path::to_path_buf);
        let database_kind = btree.pager().db_kind();
        let is_readonly = btree.pager().is_readonly();
        let connection_encoding = btree.encoding();
        let temp_store = if open_options.temp_store_memory {
            TempStoreMode::Memory
        } else {
            TempStoreMode::Default
        };
        let db_set = DatabaseSet::new(
            btree,
            Schema::default(),
            disk_vfs,
            open_options.default_page_size,
            open_options.lock_geometry,
            temp_store,
            main_enable_8_3_names,
            Arc::clone(&interrupt_flag),
        );

        let functions =
            FunctionRegistry::with_lazy_builtin_extensions(db_set.vtab_registry().lazy_fts5_aux());

        let inner = ConnectionInner {
            db_set,
            connection_encoding,
            state: ConnectionState::Open,
            next_handle_id: 1,
            open_handles: BTreeMap::new(),
            activity: ActivityTracker::new(),
            savepoints: Vec::new(),
            runtime: VmRuntimeContext::default(),
            active_interrupt_flag: Arc::clone(&interrupt_flag),
            functions,
            collations: CollationRegistry::default(),
            progress: ProgressTracker::default(),
            commit_hooks: CommitCoordinatorHooks::default(),
            mutation_pipeline: MutationPipeline::default(),
            authorizer: AuthorizerSlot::default(),
            trace_hook: None,
            strict_subtype: true,
            explicit_tx: false,
            implicit_savepoint_tx: false,
            in_user_btree_tx: false,
            schema_changed_in_tx: false,
            foreign_keys: false,
            recursive_triggers: false,
            legacy_alter_table: false,
            count_changes: false,
            wal_autocheckpoint_threshold: SQLITE_DEFAULT_WAL_AUTOCHECKPOINT,
            default_locking_mode: LockingMode::Normal,
            writable_schema: false,
            working_dir: None,
            busy_policy: BusyPolicy::default(),
            rollback_count: 0,
            deferred_fk_suspects: Vec::new(),
            schema_generation: 0,
            v1_expire_generation: 0,
            advisory_expire_generation: 0,
            simulated_cleanup_io_error: None,
            panic_on_next_pending_drop: false,
            cached_lit_insert: None,
            lit_insert_neg_table: None,
            lit_insert_params: Vec::new(),
            lit_insert_shapes: Vec::new(),
            lit_insert_scratch: sqliters_vdbe::VdbeWriteScratch::default(),
            pooled_read_vm: None,
            pooled_write_scratch: sqliters_vdbe::VdbeWriteScratch::default(),
        };

        let shared = Arc::new(ConnectionShared {
            conn_id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
            db_path,
            database_kind,
            is_readonly,
            open_options,
            interrupt_flag,
            has_external_interrupt: AtomicBool::new(false),
            gate_state: AtomicU8::new(0),
            conn_state: AtomicU8::new(CONN_STATE_OPEN),
            mirror_last_insert_rowid: AtomicI64::new(0),
            mirror_changes: AtomicU64::new(0),
            mirror_total_changes: AtomicU64::new(0),
            mirror_autocommit: AtomicBool::new(true),
            mirror_rollback_count: AtomicU64::new(0),
            mirror_attached_limit: AtomicUsize::new(SQLITE_MAX_ATTACHED),
            pending_drops: Mutex::new(Vec::new()),
            inner: Mutex::new(inner),
        });

        Ok(Self { shared })
    }

    /// Consumes this [`Connection`] facade and attempts to close the underlying database
    /// (`sqlite3_close`).
    ///
    /// If any prepared [`Statement`] or open handle remains unfinalized (or if the connection gate
    /// is busy / reentered), returns `Err(CloseError { connection: self, error })` with `self`
    /// completely intact and usable.
    pub fn try_close(self) -> std::result::Result<(), CloseError<Self>> {
        let shared = Arc::clone(&self.shared);
        let mut session = match shared.enter() {
            Ok(s) => s,
            Err(error) => {
                return Err(CloseError {
                    connection: self,
                    error,
                });
            }
        };

        let inner = session.inner_mut();
        if matches!(inner.state, ConnectionState::Closed) {
            drop(session);
            return Ok(());
        }

        if !inner.open_handles.is_empty() {
            drop(session);
            return Err(CloseError {
                connection: self,
                error: SqliteError::new(
                    ErrorCode::Busy,
                    "unable to close due to unfinalized statements or unfinished backups",
                ),
            });
        }

        inner.state = ConnectionState::Closing;
        let mut any_in_tx = false;
        for slot in inner.db_set.slots_mut().iter_mut().rev() {
            if let Some(ref mut bt) = slot.btree {
                bt.close_all_cursors();
                if bt.transaction_state() != TransactionState::Open {
                    any_in_tx = true;
                    let _ = bt.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
                }
            }
        }
        if any_in_tx || !inner.db_set.vtab_registry().active_tx_vtabs().is_empty() {
            inner.with_default_exec_ctx(&self.shared, ActivitySummary::default(), |ctx| {
                crate::vtab::vtab_rollback_all(ctx)
            });
            inner.rollback_count += 1;
        }
        inner.explicit_tx = false;
        inner.implicit_savepoint_tx = false;
        inner.in_user_btree_tx = false;
        inner.savepoints.clear();
        inner.deferred_fk_suspects.clear();
        inner.publish(&self.shared);

        if let Err(close_err) = inner.db_set.close_all() {
            let poisoned = inner.poison_with(close_err);
            drop(session);
            return Err(CloseError {
                connection: self,
                error: poisoned,
            });
        }

        inner.state = ConnectionState::Closed;
        drop(session);
        Ok(())
    }

    /// Compiles the first SQL statement in `sql` into a steppable [`Statement`] using
    /// [`PrepareMode::V2`] (`sqlite3_prepare_v2`).
    pub fn prepare(&self, sql: &str) -> Result<Statement> {
        self.prepare_with_mode(sql, PrepareMode::V2)
    }

    /// Compiles the first SQL statement in `sql` using legacy [`PrepareMode::V1`]
    /// (`sqlite3_prepare` — does not auto-reprepare on schema change).
    pub fn prepare_v1(&self, sql: &str) -> Result<Statement> {
        self.prepare_with_mode(sql, PrepareMode::V1)
    }

    /// Compiles the first SQL statement in `sql` with explicit [`PrepareMode`].
    pub fn prepare_with_mode(&self, sql: &str, mode: PrepareMode) -> Result<Statement> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        Statement::prepare_in_session(&self.shared, session.inner_mut(), sql, mode)
    }

    /// Executes one or more semicolon-separated SQL statements (`sqlite3_exec`) and returns `self.changes()`.
    pub fn execute(&self, sql: &str) -> Result<u64> {
        self.execute_batch(sql)?;
        Ok(self.changes())
    }

    /// Executes a single parameterized SQL statement and returns `self.changes()`.
    pub fn execute_params(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let (mut stmt, _) = Statement::prepare_in_session_opts(
            &self.shared,
            session.inner_mut(),
            sql,
            PrepareMode::V2,
            false,
        )?;
        let res = (|| -> Result<()> {
            stmt.bind_all_in_session(session.inner(), params)?;
            while let StepResult::Row(_) =
                stmt.step_in_session(&self.shared, session.inner_mut())?
            {}
            Ok(())
        })();
        let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
        res?;
        fin_res?;
        Ok(self.changes())
    }

    /// Executes an arbitrary multi-statement SQL script (`sqlite3_exec`).
    pub fn execute_batch(&self, mut sql: &str) -> Result<()> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        while !sql.is_empty() {
            if session.inner().trace_hook.is_none() {
                let trimmed = sql.trim_start_matches([' ', '\t', '\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some((res, tail)) = session
                    .inner_mut()
                    .try_fast_exec_literal_insert_step(&self.shared, trimmed)
                {
                    res?;
                    sql = tail;
                    continue;
                }
            }
            let (mut stmt, next_tail_len) = Statement::prepare_in_session_opts(
                &self.shared,
                session.inner_mut(),
                sql,
                PrepareMode::V2,
                false,
            )?;
            let cur_len = sql.len();
            let step_res = (|| -> Result<()> {
                while let StepResult::Row(_) =
                    stmt.step_in_session(&self.shared, session.inner_mut())?
                {}
                Ok(())
            })();
            let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
            step_res?;
            fin_res?;
            if next_tail_len == cur_len {
                break;
            }
            sql = &sql[(cur_len - next_tail_len)..];
        }
        Ok(())
    }

    /// Executes a newline-separated batch of complete non-returning DML/TCL lines under a single
    /// connection session, invoking `on_error` (`true` => bail immediately, `false` => skip to the
    /// next `\n`-delimited line and continue) whenever a statement errors.
    #[doc(hidden)]
    pub fn execute_non_returning_lines<F>(&self, mut sql: &str, mut on_error: F) -> Result<bool>
    where
        F: FnMut(SqliteError) -> bool,
    {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        fn skip_line(s: &str) -> &str {
            match s.find('\n') {
                Some(idx) => &s[(idx + 1)..],
                None => "",
            }
        }
        while !sql.is_empty() {
            if session.inner().trace_hook.is_none() {
                let trimmed = sql.trim_start_matches([' ', '\t', '\r', '\n']);
                if trimmed.is_empty() {
                    break;
                }
                if let Some((res, tail)) = session
                    .inner_mut()
                    .try_fast_exec_literal_insert_step(&self.shared, trimmed)
                {
                    if let Err(e) = res
                        && on_error(e)
                    {
                        return Ok(true);
                    }
                    sql = tail;
                    continue;
                }
            }
            let (mut stmt, next_tail_len) = match Statement::prepare_in_session_opts(
                &self.shared,
                session.inner_mut(),
                sql,
                PrepareMode::V2,
                false,
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    if on_error(e) {
                        return Ok(true);
                    }
                    sql = skip_line(sql);
                    continue;
                }
            };
            let cur_len = sql.len();
            let step_res = (|| -> Result<()> {
                while let StepResult::Row(_) =
                    stmt.step_in_session(&self.shared, session.inner_mut())?
                {}
                Ok(())
            })();
            let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
            if let Err(e) = step_res.and(fin_res)
                && on_error(e)
            {
                return Ok(true);
            }
            if next_tail_len == cur_len {
                break;
            }
            sql = &sql[(cur_len - next_tail_len)..];
        }
        Ok(false)
    }

    /// Compiles, steps, and finalizes the first SQL statement in `sql` within a single connection
    /// session (`keep_tail = false`), streaming `(column_names, row)` to `on_row` and returning
    /// the unparsed tail byte length.
    #[doc(hidden)]
    pub fn execute_cli_statement<F>(&self, sql: &str, mut on_row: F) -> Result<usize>
    where
        F: FnMut(&[String], &Row),
    {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let (mut stmt, next_tail_len) = Statement::prepare_in_session_opts(
            &self.shared,
            session.inner_mut(),
            sql,
            PrepareMode::V2,
            false,
        )?;
        let step_res = (|| -> Result<()> {
            while let StepResult::Row(r) =
                stmt.step_in_session(&self.shared, session.inner_mut())?
            {
                on_row(&stmt.column_names, &r);
            }
            Ok(())
        })();
        let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
        step_res?;
        fin_res?;
        Ok(next_tail_len)
    }

    /// Executes a query with optional positional parameters and returns all columns & rows.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let (mut stmt, _) = Statement::prepare_in_session_opts(
            &self.shared,
            session.inner_mut(),
            sql,
            PrepareMode::V2,
            false,
        )?;
        let res = (|| -> Result<QueryResult> {
            stmt.bind_all_in_session(session.inner(), params)?;
            let mut rows = Vec::new();
            while let StepResult::Row(r) =
                stmt.step_in_session(&self.shared, session.inner_mut())?
            {
                rows.push(r);
            }
            Ok(QueryResult {
                column_names: stmt.column_names.clone(),
                rows,
            })
        })();
        let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
        let out = res?;
        fin_res?;
        Ok(out)
    }

    /// Executes a query and returns the first [`Row`] if any (`None` if 0 rows).
    pub fn query_row(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let (mut stmt, _) = Statement::prepare_in_session_opts(
            &self.shared,
            session.inner_mut(),
            sql,
            PrepareMode::V2,
            false,
        )?;
        let res = (|| -> Result<Option<Row>> {
            stmt.bind_all_in_session(session.inner(), params)?;
            match stmt.step_in_session(&self.shared, session.inner_mut())? {
                StepResult::Row(r) => Ok(Some(r)),
                StepResult::Done => Ok(None),
            }
        })();
        let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
        let out = res?;
        fin_res?;
        Ok(out)
    }

    /// Enables or disables recording of [`MutationContext`] events dispatched through the shared
    /// row-mutation pipeline.
    pub fn set_mutation_trace_enabled(&self, enabled: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.mutation_pipeline.set_trace_enabled(enabled);
        Ok(())
    }

    /// Drains and returns all [`MutationContext`] events recorded since tracing was enabled or
    /// last drained.
    pub fn take_mutation_trace(&self) -> Result<Vec<MutationContext>> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        Ok(inner.mutation_pipeline.take_events())
    }

    /// Replaces the `main` database with an in-memory SQLite Format 3 image (`sqlite3_deserialize`).
    pub fn deserialize(&self, data: &[u8], readonly: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        if !inner.activity.has_no_active_tx() {
            return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
        }
        inner.with_default_exec_ctx(&self.shared, ActivitySummary::default(), |ctx| {
            crate::vtab::vtab_rollback_all(ctx)
        });
        for slot in inner.db_set.slots_mut() {
            if let Some(ref mut bt) = slot.btree
                && bt.transaction_state() != TransactionState::Open
            {
                let _ = bt.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
            }
        }
        let writable_schema = inner.writable_schema;
        {
            let main_bt = inner.db_set.main_btree_mut();
            main_bt
                .pager_mut()
                .load_deserialized_image(data, readonly)?;
            main_bt.set_writable_schema(writable_schema);
            if main_bt.pager().page_count() > 0 {
                inner.connection_encoding = main_bt.encoding();
            }
        }
        {
            let main_slot = inner.db_set.main_slot_mut();
            main_slot.schema = Schema::default();
            main_slot.next_auto_vacuum = None;
            main_slot.next_page_size = None;
        }
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        inner.explicit_tx = false;
        inner.implicit_savepoint_tx = false;
        inner.in_user_btree_tx = false;
        inner.savepoints.clear();
        inner.deferred_fk_suspects.clear();
        inner.publish(&self.shared);
        Ok(())
    }

    /// Sets an optional base working directory used when resolving relative output paths in
    /// statements such as `VACUUM INTO 'out.db'` inside sandboxed test harnesses.
    pub fn set_working_dir(&self, dir: Option<PathBuf>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.working_dir = dir;
        Ok(())
    }

    /// Enables or disables `testfixture` SQL scalar functions (`randstr`, `sqlite/src/test_func.c`)
    /// on this connection.
    pub fn set_enable_test_functions(&self, enabled: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.runtime.enable_test_functions = enabled;
        Ok(())
    }

    /// Reloads the in-memory schema catalog from Page 1 (`sqlite_schema`).
    pub fn reload_schema(&self) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        for slot in inner.db_set.slots_mut() {
            if let Some(ref mut bt) = slot.btree {
                slot.schema = Schema::load_for_schema(bt, Some(&slot.name))?;
            }
        }
        inner.db_set.rebind_all_collations(&inner.collations);
        if inner.db_set.main_btree().pager().page_count() > 0 {
            inner.connection_encoding = inner.db_set.main_btree().encoding();
        }
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Returns an owned snapshot of the current in-memory [`Schema`] catalog, loading or
    /// refreshing it from disk if needed.
    pub fn schema_snapshot(&self) -> Result<Schema> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let _ = inner.ensure_schemas_loaded_for_prepare(true)?;
        Ok(inner.db_set.main_schema().clone())
    }

    /// Returns an owned copy of the authoritative 100-byte [`DatabaseHeader`], performing a brief
    /// locked header read if the connection is currently in `Open` (`NoLock`) state.
    pub fn database_header(&self) -> Result<DatabaseHeader> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let main_inst = inner.db_set.main_slot().schema_instance_id;
        let can_end = !inner.in_user_btree_tx
            && inner.activity.has_no_active_tx()
            && !inner.activity.has_active_tx_on_instance(main_inst);
        if inner.db_set.main_btree().transaction_state() == TransactionState::Open {
            {
                let slot = &mut inner.db_set.slots_mut()[0];
                let bt = slot.btree.as_mut().unwrap();
                inner.busy_policy.retry_btree(bt, |b| b.begin_read())?;
            }
            let hdr = inner.db_set.main_btree().pager().header().clone();
            if can_end {
                inner
                    .db_set
                    .main_btree_mut()
                    .pager_mut()
                    .end_read_if_unused()?;
            }
            Ok(hdr)
        } else {
            Ok(inner.db_set.main_btree().pager().header().clone())
        }
    }

    /// Returns the authoritative database page size (`512..=65536`), performing a brief locked
    /// header read if needed.
    pub fn page_size(&self) -> Result<u32> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner
            .db_set
            .main_btree_mut()
            .pager_mut()
            .effective_page_size()
    }

    /// Returns the authoritative database page count, performing a brief locked header read if
    /// the connection is currently in `Open` (`NoLock`) state.
    pub fn page_count(&self) -> Result<Pgno> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let main_inst = inner.db_set.main_slot().schema_instance_id;
        let can_end = !inner.in_user_btree_tx
            && inner.activity.has_no_active_tx()
            && !inner.activity.has_active_tx_on_instance(main_inst);
        if inner.db_set.main_btree().transaction_state() == TransactionState::Open {
            {
                let slot = &mut inner.db_set.slots_mut()[0];
                let bt = slot.btree.as_mut().unwrap();
                inner.busy_policy.retry_btree(bt, |b| b.begin_read())?;
            }
            let cnt = inner.db_set.main_btree().pager().page_count();
            if can_end {
                inner
                    .db_set
                    .main_btree_mut()
                    .pager_mut()
                    .end_read_if_unused()?;
            }
            Ok(cnt)
        } else {
            Ok(inner.db_set.main_btree().pager().page_count())
        }
    }

    /// Rowid of the most recent successful `INSERT` into a rowid table (`sqlite3_last_insert_rowid`).
    /// Lock-free atomic load safe to call from callbacks/hooks and other threads.
    #[inline]
    pub fn last_insert_rowid(&self) -> i64 {
        self.shared.mirror_last_insert_rowid.load(Ordering::Acquire)
    }

    /// Number of rows modified by the most recent `INSERT`, `UPDATE`, or `DELETE` (`sqlite3_changes64`).
    /// Lock-free atomic load safe to call from callbacks/hooks and other threads.
    #[inline]
    pub fn changes(&self) -> u64 {
        self.shared.mirror_changes.load(Ordering::Acquire)
    }

    /// Total number of rows modified since this connection was opened (`sqlite3_total_changes64`).
    /// Lock-free atomic load safe to call from callbacks/hooks and other threads.
    #[inline]
    pub fn total_changes(&self) -> u64 {
        self.shared.mirror_total_changes.load(Ordering::Acquire)
    }

    /// Returns `true` if the connection is in autocommit mode (`sqlite3_get_autocommit`).
    /// Lock-free atomic load safe to call from callbacks/hooks and other threads.
    #[inline]
    pub fn is_autocommit(&self) -> bool {
        self.shared.mirror_autocommit.load(Ordering::Acquire)
    }

    /// Monotonic counter incremented on every transaction/savepoint `ROLLBACK`.
    /// Lock-free atomic load safe to call from callbacks/hooks and other threads.
    #[inline]
    pub fn rollback_count(&self) -> u64 {
        self.shared.mirror_rollback_count.load(Ordering::Acquire)
    }

    /// Returns `true` if this connection was opened read-only.
    #[inline]
    pub fn is_readonly(&self) -> bool {
        self.shared.is_readonly
    }

    /// Returns the canonical path of the main database file (`None` for `:memory:` and `""`).
    #[inline]
    pub fn db_path(&self) -> Option<&Path> {
        self.shared.db_path.as_deref()
    }

    /// Returns the [`DatabaseKind`] (`NamedDisk`, `Memory`, or `PrivateTemporary`) of `main`.
    #[inline]
    pub fn database_kind(&self) -> DatabaseKind {
        self.shared.database_kind
    }

    /// Returns the [`ConnectionOpenOptions`] used to open this connection.
    #[inline]
    pub fn open_options(&self) -> ConnectionOpenOptions {
        self.shared.open_options
    }

    /// Returns a lock-free [`InterruptHandle`] (`Send + Sync + Clone`) for `sqlite3_interrupt`.
    #[inline]
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.shared
            .has_external_interrupt
            .store(true, Ordering::Release);
        InterruptHandle {
            flag: Arc::clone(&self.shared.interrupt_flag),
        }
    }

    /// Returns the default atomic interrupt flag attached to this connection.
    #[inline]
    pub fn interrupt_flag(&self) -> &Arc<AtomicBool> {
        self.shared
            .has_external_interrupt
            .store(true, Ordering::Release);
        &self.shared.interrupt_flag
    }

    /// Returns the canonical path of the named database (`sqlite3_db_filename`), or `None` for
    /// `"temp"`, `:memory:`, `""` (`PrivateTemporary`), unknown schemas, or a closed connection.
    pub fn db_filename(&self, db_name: &str) -> Option<PathBuf> {
        if let Ok(session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return None;
            }
            session
                .inner()
                .db_set
                .db_filename(db_name)
                .map(Path::to_path_buf)
        } else if db_name.eq_ignore_ascii_case("main") {
            self.shared.db_path.clone()
        } else {
            None
        }
    }

    /// Returns the current `SQLITE_LIMIT_ATTACHED` limit on this connection (`sqlite3_limit(..., -1)`).
    #[inline]
    pub fn attached_limit(&self) -> usize {
        self.shared.mirror_attached_limit.load(Ordering::Acquire)
    }

    /// Queries (`limit < 0`) or updates (`limit >= 0`, clamped to `0..=SQLITE_MAX_ATTACHED`)
    /// `SQLITE_LIMIT_ATTACHED` (`sqlite3_limit`), returning the **previous** limit.
    pub fn set_attached_limit(&self, limit: isize) -> usize {
        if limit < 0 {
            return self.attached_limit();
        }
        let clamped = (limit as usize).min(SQLITE_MAX_ATTACHED);
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return self.attached_limit();
            }
            let prev = session.inner().db_set.max_attached();
            let actual = session.inner_mut().db_set.set_max_attached(clamped);
            self.shared
                .mirror_attached_limit
                .store(actual, Ordering::Release);
            prev
        } else {
            self.attached_limit()
        }
    }

    /// Queries (`enabled == None`) or updates (`enabled == Some(b)`) `SQLITE_DBCONFIG_ENABLE_VIEW`,
    /// returning the current/updated state.
    pub fn db_config_enable_view(&self, enabled: Option<bool>) -> bool {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return true;
            }
            if let Some(flag) = enabled {
                let inner = session.inner_mut();
                let prev = inner.db_set.enable_view();
                let cur = inner.db_set.set_enable_view(flag);
                if prev != cur {
                    inner.schema_generation += 1;
                }
                cur
            } else {
                session.inner().db_set.enable_view()
            }
        } else {
            true
        }
    }

    /// Queries (`enabled = None`) or updates (`enabled = Some(...)`) `SQLITE_DBCONFIG_ENABLE_TRIGGER`,
    /// returning the current/updated state.
    pub fn db_config_enable_trigger(&self, enabled: Option<bool>) -> bool {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return true;
            }
            if let Some(flag) = enabled {
                let inner = session.inner_mut();
                let prev = inner.db_set.enable_trigger();
                let cur = inner.db_set.set_enable_trigger(flag);
                if prev != cur {
                    inner.schema_generation += 1;
                }
                cur
            } else {
                session.inner().db_set.enable_trigger()
            }
        } else {
            true
        }
    }

    /// Queries (`enabled = None`) or updates (`enabled = Some(...)`) `SQLITE_DBCONFIG_DQS_DML`,
    /// returning the current/updated state.
    pub fn db_config_dqs_dml(&self, enabled: Option<bool>) -> bool {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return false;
            }
            if let Some(flag) = enabled {
                let inner = session.inner_mut();
                let prev = inner.db_set.dqs_dml();
                let cur = inner.db_set.set_dqs_dml(flag);
                if prev != cur {
                    inner.schema_generation += 1;
                }
                cur
            } else {
                session.inner().db_set.dqs_dml()
            }
        } else {
            false
        }
    }

    /// Queries (`enabled = None`) or updates (`enabled = Some(...)`) `PRAGMA recursive_triggers`,
    /// returning the current/updated state.
    pub fn recursive_triggers(&self, enabled: Option<bool>) -> bool {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return false;
            }
            if let Some(flag) = enabled {
                let inner = session.inner_mut();
                if inner.recursive_triggers != flag {
                    inner.recursive_triggers = flag;
                    inner.schema_generation += 1;
                }
                inner.recursive_triggers
            } else {
                session.inner().recursive_triggers
            }
        } else {
            false
        }
    }

    /// Queries (`limit < 0`) or updates (`limit >= 0`, clamped to `0..=SQLITE_MAX_TRIGGER_STEPS`)
    /// `SQLITE_LIMIT_TRIGGER_STEPS`, returning the **previous** limit.
    pub fn set_trigger_steps_limit(&self, limit: isize) -> usize {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return SQLITE_MAX_TRIGGER_STEPS;
            }
            let prev = session.inner().db_set.trigger_steps_limit();
            if limit >= 0 {
                let inner = session.inner_mut();
                let actual = inner.db_set.set_trigger_steps_limit(limit as usize);
                if actual != prev {
                    inner.schema_generation += 1;
                }
            }
            prev
        } else {
            SQLITE_MAX_TRIGGER_STEPS
        }
    }

    /// Queries (`limit < 0`) or updates (`limit >= 0`, clamped to `1..=SQLITE_MAX_TRIGGER_DEPTH`)
    /// `SQLITE_LIMIT_TRIGGER_DEPTH`, returning the **previous** limit.
    pub fn set_trigger_depth_limit(&self, limit: isize) -> usize {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return SQLITE_MAX_TRIGGER_DEPTH;
            }
            let prev = session.inner().db_set.trigger_depth_limit();
            if limit >= 0 {
                let inner = session.inner_mut();
                let actual = inner.db_set.set_trigger_depth_limit(limit as usize);
                if actual != prev {
                    inner.schema_generation += 1;
                }
            }
            prev
        } else {
            SQLITE_MAX_TRIGGER_DEPTH
        }
    }

    /// Returns all active triggers attached to `(target_db_id, relation_name)` (`TEMP` triggers
    /// followed by permanent triggers in `target_db_id`).
    pub fn triggers_for_relation(
        &self,
        target_db_id: DatabaseId,
        relation_name: &str,
    ) -> Vec<(DatabaseId, TriggerSchema)> {
        if let Ok(mut session) = self.shared.enter() {
            let inner = session.inner_mut();
            if inner.ensure_usable().is_err() {
                return Vec::new();
            }
            let _ = inner.ensure_schemas_loaded_for_prepare(true);
            inner
                .db_set
                .triggers_for_relation(target_db_id, relation_name)
        } else {
            Vec::new()
        }
    }

    /// Queries (`limit < 0`) or updates (`limit >= 0`, clamped to `0..=SQLITE_MAX_EXPR_DEPTH`)
    /// `SQLITE_LIMIT_EXPR_DEPTH`, returning the **previous** limit.
    pub fn set_expr_depth_limit(&self, limit: isize) -> usize {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return db_set::SQLITE_MAX_EXPR_DEPTH;
            }
            let prev = session.inner().db_set.expr_depth_limit();
            if limit >= 0 {
                let inner = session.inner_mut();
                let actual = inner.db_set.set_expr_depth_limit(limit as usize);
                if actual != prev {
                    inner.schema_generation += 1;
                }
            }
            prev
        } else {
            db_set::SQLITE_MAX_EXPR_DEPTH
        }
    }

    /// Queries (`limit < 0`) or updates (`limit >= 0`, clamped to `0..=max_column_ceiling`)
    /// `SQLITE_LIMIT_COLUMN`, returning the **previous** limit.
    pub fn set_column_limit(&self, limit: isize) -> usize {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return db_set::SQLITE_MAX_COLUMN;
            }
            let prev = session.inner().db_set.column_limit();
            if limit >= 0 {
                let inner = session.inner_mut();
                let actual = inner.db_set.set_column_limit(limit as usize);
                if actual != prev {
                    inner.schema_generation += 1;
                }
            }
            prev
        } else {
            db_set::SQLITE_MAX_COLUMN
        }
    }

    /// Configures the connection's hard `SQLITE_MAX_COLUMN` ceiling (`1..=32767`, default `2000`)
    /// and sets `SQLITE_LIMIT_COLUMN` to the same effective ceiling, returning the effective ceiling.
    pub fn set_max_column_ceiling(&self, ceiling: usize) -> usize {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return db_set::SQLITE_MAX_COLUMN;
            }
            let prev = session.inner().db_set.column_limit();
            let inner = session.inner_mut();
            let actual = inner.db_set.set_max_column_ceiling(ceiling);
            if actual != prev {
                inner.schema_generation += 1;
            }
            actual
        } else {
            db_set::SQLITE_MAX_COLUMN
        }
    }

    /// Drains accumulated `INSERT` transfer-optimization activations (`sqlite3_xferopt_count`).
    pub fn take_xferopt_delta(&self) -> u64 {
        if let Ok(session) = self.shared.enter() {
            session.inner().db_set.take_xferopt_delta()
        } else {
            0
        }
    }

    /// Returns `true` if `sql` is a `SELECT` statement whose query plan uses a temporary B-tree
    /// sorter for `ORDER BY` (`USE TEMP B-TREE FOR ORDER BY`).
    pub fn select_uses_temp_sort(&self, sql: &str) -> bool {
        if !sql.to_ascii_uppercase().contains("ORDER") {
            return false;
        }
        let Ok((Some(stmt), _)) = sqliters_syntax::parse_one(sql) else {
            return false;
        };
        if !matches!(stmt, Stmt::Select(_)) {
            return false;
        }
        let Ok(session) = self.shared.enter() else {
            return false;
        };
        let Ok(rows) = compiler::explain_query_plan_in_db_set(&stmt, &session.inner().db_set)
        else {
            return false;
        };
        rows.iter().any(|r| {
            matches!(
                r.get(3),
                Some(Value::Text(s)) if s.contains("USE TEMP B-TREE FOR ORDER BY")
            )
        })
    }

    /// Installs or clears the connection commit hook (`sqlite3_commit_hook`).
    /// Returning `true` (non-zero) aborts the commit with `SQLITE_CONSTRAINT_COMMITHOOK` and rolls back.
    pub fn commit_hook(&self, hook: Option<CommitHookCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.commit_hooks.set_commit_hook(hook);
        Ok(())
    }

    /// Alias for [`Connection::commit_hook`].
    pub fn set_commit_hook(&self, hook: Option<CommitHookCallback>) -> Result<()> {
        self.commit_hook(hook)
    }

    /// Installs or clears the connection rollback hook (`sqlite3_rollback_hook`).
    pub fn rollback_hook(&self, hook: Option<RollbackHookCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.commit_hooks.set_rollback_hook(hook);
        Ok(())
    }

    /// Alias for [`Connection::rollback_hook`].
    pub fn set_rollback_hook(&self, hook: Option<RollbackHookCallback>) -> Result<()> {
        self.rollback_hook(hook)
    }

    /// Installs or clears the connection row-level update hook (`sqlite3_update_hook`).
    pub fn update_hook(&self, hook: Option<UpdateHookCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.mutation_pipeline.set_update_hook(hook);
        Ok(())
    }

    /// Alias for [`Connection::update_hook`].
    pub fn set_update_hook(&self, hook: Option<UpdateHookCallback>) -> Result<()> {
        self.update_hook(hook)
    }

    /// Installs or clears the connection SQL statement trace hook (`sqlite3_trace_v2` / `SQLITE_TRACE_STMT`).
    pub fn trace_hook(&self, hook: Option<TraceHookCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.trace_hook = hook;
        Ok(())
    }

    /// Alias for [`Connection::trace_hook`].
    pub fn set_trace_hook(&self, hook: Option<TraceHookCallback>) -> Result<()> {
        self.trace_hook(hook)
    }

    /// Installs or clears the prepare-time authorization callback (`sqlite3_set_authorizer`).
    ///
    /// Marks all existing prepared statements on this connection as advisory-expired
    /// (`sqlite3ExpirePreparedStatements(db, 1)`): statements that have not yet started (or are
    /// restarted after halting) will automatically reprepare under the new authorizer on the next
    /// [`Statement::step`], while statements already in `Running` state continue to completion.
    pub fn set_authorizer(&self, authorizer: Option<AuthorizerCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.authorizer.set(authorizer);
        inner.advisory_expire_generation += 1;
        Ok(())
    }

    /// Clears the prepare-time authorization callback (`sqlite3_set_authorizer(db, NULL, NULL)`).
    pub fn clear_authorizer(&self) -> Result<()> {
        self.set_authorizer(None)
    }

    /// Replaces or restores the `Btree` atomic interrupt flag under the connection gate.
    pub fn set_interrupt_flag(&self, flag: Option<Arc<AtomicBool>>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        if flag.is_some() {
            self.shared
                .has_external_interrupt
                .store(true, Ordering::Release);
        }
        let eff = flag.unwrap_or_else(|| Arc::clone(&self.shared.interrupt_flag));
        inner.active_interrupt_flag = Arc::clone(&eff);
        inner.db_set.set_interrupt_flag(eff);
        Ok(())
    }

    /// Registers or replaces a connection-local scalar SQL function (`sqlite3_create_function_v2`).
    ///
    /// Pass `num_args` in `0..=127` for a fixed-arity overload or `num_args == -1` for variadic arity.
    pub fn register_scalar_function(
        &self,
        name: &str,
        num_args: i32,
        flags: FunctionFlags,
        func: impl Fn(&[RuntimeValue]) -> Result<RuntimeValue> + Send + Sync + 'static,
    ) -> Result<()> {
        let arity = if num_args == -1 {
            None
        } else if (0..=127).contains(&num_args) {
            Some(num_args as usize)
        } else {
            return Err(SqliteError::misuse("invalid function argument count"));
        };
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.functions.register_scalar(name, arity, flags, func);
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Idiomatic alias for [`Connection::register_scalar_function`].
    pub fn create_scalar_function(
        &self,
        name: &str,
        num_args: i32,
        flags: FunctionFlags,
        func: impl Fn(&[RuntimeValue]) -> Result<RuntimeValue> + Send + Sync + 'static,
    ) -> Result<()> {
        self.register_scalar_function(name, num_args, flags, func)
    }

    /// Unregisters a connection-local scalar SQL function overload (`sqlite3_create_function_v2`
    /// with `xFunc == NULL`).
    pub fn unregister_scalar_function(&self, name: &str, num_args: i32) -> Result<bool> {
        let arity = if num_args == -1 {
            None
        } else if (0..=127).contains(&num_args) {
            Some(num_args as usize)
        } else {
            return Err(SqliteError::misuse("invalid function argument count"));
        };
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let removed = inner.functions.unregister_scalar(name, arity);
        if removed {
            inner.schema_generation += 1;
            inner.v1_expire_generation += 1;
        }
        Ok(removed)
    }

    /// Idiomatic alias for [`Connection::unregister_scalar_function`].
    pub fn remove_scalar_function(&self, name: &str, num_args: i32) -> Result<bool> {
        self.unregister_scalar_function(name, num_args)
    }

    /// Registers or replaces a connection-local aggregate SQL function (`sqlite3_create_function_v2`).
    pub fn create_aggregate_function(
        &self,
        name: &str,
        num_args: i32,
        flags: FunctionFlags,
        factory: impl Fn() -> Box<dyn execution_context::WindowAggregateInstance>
        + Send
        + Sync
        + 'static,
    ) -> Result<()> {
        self.register_window_function(name, num_args, flags, false, false, factory)
    }

    /// Registers or replaces a connection-local aggregate or window SQL function
    /// (`sqlite3_create_window_function` / `sqlite3_create_function_v2`).
    pub fn register_window_function(
        &self,
        name: &str,
        num_args: i32,
        flags: FunctionFlags,
        supports_window: bool,
        supports_inverse: bool,
        factory: impl Fn() -> Box<dyn execution_context::WindowAggregateInstance>
        + Send
        + Sync
        + 'static,
    ) -> Result<()> {
        let arity = if num_args == -1 {
            None
        } else if (0..=127).contains(&num_args) {
            Some(num_args as usize)
        } else {
            return Err(SqliteError::misuse("invalid function argument count"));
        };
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.functions.register_window_aggregate(
            name,
            arity,
            flags,
            supports_window,
            supports_inverse,
            factory,
        );
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Idiomatic alias for [`Connection::register_window_function`] with window `xValue`/`xInverse` enabled.
    pub fn create_window_function(
        &self,
        name: &str,
        num_args: i32,
        flags: FunctionFlags,
        factory: impl Fn() -> Box<dyn execution_context::WindowAggregateInstance>
        + Send
        + Sync
        + 'static,
    ) -> Result<()> {
        self.register_window_function(name, num_args, flags, true, true, factory)
    }

    /// Registers or replaces a connection-local collation sequence (`sqlite3_create_collation_v2`).
    pub fn register_collation(
        &self,
        name: &str,
        cmp: impl Fn(&str, &str) -> std::cmp::Ordering + Send + Sync + 'static,
    ) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.collations.register_collation(name, cmp);
        inner.db_set.rebind_all_collations(&inner.collations);
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Idiomatic alias for [`Connection::register_collation`].
    pub fn create_collation(
        &self,
        name: &str,
        cmp: impl Fn(&str, &str) -> std::cmp::Ordering + Send + Sync + 'static,
    ) -> Result<()> {
        self.register_collation(name, cmp)
    }

    /// Registers or replaces a fallible connection-local collation sequence (`Result<Ordering>`).
    pub fn register_fallible_collation(
        &self,
        name: &str,
        cmp: impl Fn(&str, &str) -> Result<std::cmp::Ordering> + Send + Sync + 'static,
    ) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.collations.register_fallible_collation(name, cmp);
        inner.db_set.rebind_all_collations(&inner.collations);
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Idiomatic alias for [`Connection::register_fallible_collation`].
    pub fn create_fallible_collation(
        &self,
        name: &str,
        cmp: impl Fn(&str, &str) -> Result<std::cmp::Ordering> + Send + Sync + 'static,
    ) -> Result<()> {
        self.register_fallible_collation(name, cmp)
    }

    /// Removes a connection-local collation sequence (`sqlite3_create_collation_v2` with `xCompare == NULL`).
    pub fn remove_collation(&self, name: &str) -> Result<bool> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let removed = inner.collations.unregister_collation(name);
        if removed {
            inner.db_set.rebind_all_collations(&inner.collations);
            inner.schema_generation += 1;
            inner.v1_expire_generation += 1;
        }
        Ok(removed)
    }

    /// Registers or replaces a virtual table module on this connection (`sqlite3_create_module_v2`).
    pub fn create_module(&self, module: Arc<dyn VirtualTableModule>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.db_set.vtab_registry_mut().register_module(module);
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
    }

    /// Idiomatic alias for [`Connection::create_module`].
    pub fn register_module(&self, module: Arc<dyn VirtualTableModule>) -> Result<()> {
        self.create_module(module)
    }

    /// Idiomatic alias for [`Connection::create_module`].
    pub fn register_virtual_table_module(&self, module: Arc<dyn VirtualTableModule>) -> Result<()> {
        self.create_module(module)
    }

    /// Unregisters a virtual table module by name (`sqlite3_create_module_v2` with `pModule == NULL`).
    pub fn unregister_module(&self, name: &str) -> Result<bool> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let removed = inner.db_set.vtab_registry_mut().unregister_module(name);
        if removed {
            inner.schema_generation += 1;
            inner.v1_expire_generation += 1;
        }
        Ok(removed)
    }

    /// Idiomatic alias for [`Connection::unregister_module`].
    pub fn remove_module(&self, name: &str) -> Result<bool> {
        self.unregister_module(name)
    }

    /// Enables or disables `SQLITE_DBCONFIG_DEFENSIVE` mode on this connection
    /// (protecting virtual table shadow tables against direct user DML/DDL).
    pub fn set_defensive(&self, enabled: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let prev = inner.db_set.defensive();
        inner.db_set.set_defensive(enabled);
        if prev != enabled {
            inner.schema_generation += 1;
            inner.v1_expire_generation += 1;
        }
        Ok(())
    }

    /// Returns whether `SQLITE_DBCONFIG_DEFENSIVE` mode is currently enabled on this connection.
    pub fn defensive(&self) -> Result<bool> {
        let session = self.shared.enter()?;
        let inner = session.inner();
        inner.ensure_usable()?;
        Ok(inner.db_set.defensive())
    }

    /// Queries (`enabled == None`) or updates (`enabled == Some(b)`) `SQLITE_DBCONFIG_DEFENSIVE`,
    /// returning the current/updated state.
    pub fn db_config_defensive(&self, enabled: Option<bool>) -> bool {
        if let Ok(mut session) = self.shared.enter() {
            if session.inner().ensure_usable().is_err() {
                return false;
            }
            if let Some(flag) = enabled {
                let inner = session.inner_mut();
                let prev = inner.db_set.defensive();
                let cur = inner.db_set.set_defensive(flag);
                if prev != cur {
                    inner.schema_generation += 1;
                    inner.v1_expire_generation += 1;
                }
                cur
            } else {
                session.inner().db_set.defensive()
            }
        } else {
            false
        }
    }

    /// Enables or disables `PRAGMA trusted_schema` (`SQLITE_DBCONFIG_TRUSTED_SCHEMA`) on this connection.
    pub fn set_trusted_schema(&self, enabled: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let prev = inner.db_set.trusted_schema();
        inner.db_set.set_trusted_schema(enabled);
        if prev != enabled {
            inner.schema_generation += 1;
            inner.v1_expire_generation += 1;
        }
        Ok(())
    }

    /// Returns whether `PRAGMA trusted_schema` (`SQLITE_DBCONFIG_TRUSTED_SCHEMA`) is currently enabled.
    pub fn trusted_schema(&self) -> Result<bool> {
        let session = self.shared.enter()?;
        let inner = session.inner();
        inner.ensure_usable()?;
        Ok(inner.db_set.trusted_schema())
    }

    /// Installs or clears the periodic VM/engine progress callback (`sqlite3_progress_handler`).
    ///
    /// When `period_ops >= 1` and `handler` is `Some(cb)`, `cb` is invoked once every `period_ops`
    /// VM/interpreter steps; returning `true` aborts the active statement with
    /// [`ErrorCode::Interrupt`]. Passing `period_ops <= 0` or `None` disables the callback.
    pub fn progress_handler(
        &self,
        period_ops: i32,
        handler: Option<Box<dyn FnMut() -> bool + Send>>,
    ) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let interval = u32::try_from(period_ops).unwrap_or(0);
        inner.progress.set_handler(interval, handler);
        Ok(())
    }

    /// Alias for [`Connection::progress_handler`].
    pub fn set_progress_handler(
        &self,
        period_ops: i32,
        handler: Option<Box<dyn FnMut() -> bool + Send>>,
    ) -> Result<()> {
        self.progress_handler(period_ops, handler)
    }

    /// Returns the cumulative VM/interpreter progress-tick count on this connection.
    pub fn progress_op_count(&self) -> Result<u64> {
        let session = self.shared.enter()?;
        let inner = session.inner();
        inner.ensure_usable()?;
        Ok(inner.progress.op_count())
    }

    /// Resets the cumulative VM/interpreter progress-tick counter to `0`.
    pub fn reset_progress_op_count(&self) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.progress.reset_op_count();
        Ok(())
    }

    /// Runs a WAL checkpoint on `db_name` (`None` / `Some("")` for all schemas, `Some("main")`,
    /// or `Some("temp")`) with explicit [`WalCheckpointMode`] (`sqlite3_wal_checkpoint_v2` in
    /// `sqlite/src/main.c:2604-2703`).
    ///
    /// - Returns `Ok(WalCheckpointResult { busy: false, n_log: -1, n_ckpt: -1 })` when the target
    ///   database is not in WAL mode (e.g., rollback journal mode or `temp`).
    /// - Returns `Ok(WalCheckpointResult { busy: true, n_log, n_ckpt })` when checkpoint progress
    ///   was blocked by another active reader, writer, or checkpointer (preserving `(n_log, n_ckpt)`
    ///   even when busy, matching `sqlite3_wal_checkpoint_v2`).
    /// - Returns `Err(SqliteError)` on hard errors (`ErrorCode::Error` for unknown schema,
    ///   `ErrorCode::Locked` when this connection holds an active B-tree read/write transaction,
    ///   `ErrorCode::ReadOnly`, `ErrorCode::Corrupt`, `ErrorCode::IoErr`, `ErrorCode::Interrupt`).
    pub fn wal_checkpoint_v2(
        &self,
        db_name: Option<&str>,
        mode: WalCheckpointMode,
    ) -> Result<WalCheckpointResult> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let res = inner.wal_checkpoint_internal(db_name, mode);
        if inner.activity.leases.is_empty() {
            self.shared.interrupt_flag.store(false, Ordering::SeqCst);
        }
        res
    }

    /// Runs a `PASSIVE` WAL checkpoint on `db_name` (`sqlite3_wal_checkpoint` in
    /// `sqlite/src/main.c:2582-2589`), returning `Err(ErrorCode::Busy)` if blocked by an active
    /// reader, writer, or checkpointer.
    pub fn wal_checkpoint(&self, db_name: Option<&str>) -> Result<()> {
        let res = self.wal_checkpoint_v2(db_name, WalCheckpointMode::Passive)?;
        if res.busy {
            return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
        }
        Ok(())
    }

    /// Configures the automatic post-commit `PASSIVE` WAL checkpoint frame threshold
    /// (`sqlite3_wal_autocheckpoint` in `sqlite/src/main.c:2531-2546`).
    ///
    /// Passing `n_frames <= 0` disables automatic checkpointing (`0`); every new connection starts
    /// with [`SQLITE_DEFAULT_WAL_AUTOCHECKPOINT`] (`1000`).
    pub fn wal_autocheckpoint(&self, n_frames: i32) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.wal_autocheckpoint_threshold = if n_frames <= 0 { 0 } else { n_frames };
        Ok(())
    }

    /// Returns the currently configured `wal_autocheckpoint` threshold (`0` when disabled).
    pub fn wal_autocheckpoint_threshold(&self) -> Result<i32> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        Ok(inner.wal_autocheckpoint_threshold)
    }

    /// Configures a millisecond retry timeout on lock contention (`sqlite3_busy_timeout`).
    /// Passing `0` disables busy retries.
    pub fn busy_timeout(&self, ms: u64) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.busy_policy = if ms == 0 {
            BusyPolicy::None
        } else {
            BusyPolicy::TimeoutMs(ms)
        };
        Ok(())
    }

    /// Installs or clears a custom busy-handler callback (`sqlite3_busy_handler`).
    /// The callback receives the 0-based retry count and returns `true` to retry or `false` to
    /// propagate `ErrorCode::Busy`.
    pub fn busy_handler(&self, handler: Option<Box<dyn FnMut(u32) -> bool + Send>>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.busy_policy = match handler {
            Some(cb) => BusyPolicy::Custom(cb),
            None => BusyPolicy::None,
        };
        Ok(())
    }

    /// Alias for [`Connection::busy_handler`].
    pub fn set_busy_handler(
        &self,
        handler: Option<Box<dyn FnMut(u32) -> bool + Send>>,
    ) -> Result<()> {
        self.busy_handler(handler)
    }

    /// Initializes an online backup copying pages from `(self, src_db)` into `(dst, dst_db)`
    /// (`sqlite3_backup_init(pDest, zDestName, pSource, zSourceName)` in `sqlite/src/backup.c`).
    pub fn backup_init(&self, src_db: &str, dst: &Connection, dst_db: &str) -> Result<Backup> {
        Backup::init(self, src_db, dst, dst_db)
    }

    /// Opens an incremental BLOB handle on `(db_name, table_name, column_name, rowid)`
    /// (`sqlite3_blob_open` in `sqlite/src/vdbeblob.c`).
    pub fn blob_open(
        &self,
        db_name: &str,
        table_name: &str,
        column_name: &str,
        rowid: i64,
        read_write: bool,
    ) -> Result<Blob> {
        Blob::open(self, db_name, table_name, column_name, rowid, read_write)
    }

    #[inline]
    pub(crate) fn shared(&self) -> &Arc<ConnectionShared> {
        &self.shared
    }

    /// Queries or updates `SQLITE_FCNTL_PERSIST_WAL` on `db_name` (`None`, `Some("")`, or
    /// `Some("main")` / attached schema).
    ///
    /// Passing `mode < 0` queries the current flag without modifying it; `mode == 0` disables
    /// persistent `-wal`/`-shm` across last-connection close; `mode > 0` enables it.
    pub fn file_control_persist_wal(&self, db_name: Option<&str>, mode: i32) -> Result<bool> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let db_id = match db_name {
            None | Some("") => DatabaseId::MAIN,
            Some(s) => match inner.db_set.find_db_id(s) {
                Some(id) if id != DatabaseId::TEMP => id,
                _ => return Err(SqliteError::new(ErrorCode::NotFound, "not found")),
            },
        };
        let Some(slot) = inner.db_set.slot_mut(db_id) else {
            return Err(SqliteError::new(ErrorCode::NotFound, "not found"));
        };
        let Some(ref mut bt) = slot.btree else {
            return Err(SqliteError::new(ErrorCode::NotFound, "not found"));
        };
        if mode >= 0 {
            bt.pager_mut().set_persist_wal(mode != 0);
        }
        Ok(bt.pager().persist_wal())
    }

    /// Test/internal hook invoking `f(self)` while holding the active execution gate and
    /// `ConnectionInner` guard (simulating an in-engine UDF / commit / progress callback).
    #[doc(hidden)]
    pub fn with_active_execution_hook_for_testing<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> R,
    {
        let session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let out = f(self);
        drop(session);
        Ok(out)
    }

    /// Test/internal hook executing `sql` and then invoking `f(self)` inside the SAME active
    /// execution lease and `ConnectionInner` guard so callback scalar mirror visibility,
    /// same-thread `Misuse`, cross-thread `Busy`, and `pending_drops` handoff can be verified.
    #[doc(hidden)]
    pub fn execute_and_invoke_hook_for_testing<F, R>(&self, sql: &str, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> R,
    {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        if !sql.trim().is_empty() {
            let mut stmt = Statement::prepare_in_session(
                &self.shared,
                session.inner_mut(),
                sql,
                PrepareMode::V2,
            )?;
            let step_res = (|| -> Result<()> {
                while let StepResult::Row(_) =
                    stmt.step_in_session(&self.shared, session.inner_mut())?
                {}
                Ok(())
            })();
            let fin_res = stmt.finalize_in_session(&self.shared, session.inner_mut());
            step_res?;
            fin_res?;
        }
        let out = f(self);
        drop(session);
        Ok(out)
    }

    /// Opens a representative active `Blob` handle (`writable` or read-only) for Phase 1.1
    /// `ActivityTracker`, `try_close` `Busy`, and `pending_drops` lifecycle testing before full
    /// `sqlite3_blob` arrives in Phase 6.
    #[doc(hidden)]
    pub fn open_test_blob_handle(&self, writable: bool) -> Result<TestBlobHandle> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let main_inst = inner.db_set.main_slot().schema_instance_id;
        if writable {
            let sp_depth = inner.savepoints.len();
            if inner.db_set.main_btree().transaction_state() != TransactionState::Write {
                inner
                    .db_set
                    .main_btree_mut()
                    .pager_mut()
                    .begin_write_with_savepoint_depth(sp_depth)?;
            }
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            let hid = inner.register_handle(HandleKind::TestBlob { writable: true });
            inner
                .activity
                .start_with_instances(hid, true, true, vec![main_inst], vec![main_inst]);
            Ok(TestBlobHandle {
                shared: Arc::clone(&self.shared),
                handle_id: Some(hid),
                lease_cursor: None,
                writable: true,
            })
        } else {
            if inner.db_set.main_btree().transaction_state() == TransactionState::Open {
                inner.db_set.main_btree_mut().begin_read()?;
            }
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            let cid = inner.db_set.main_btree_mut().open_table_cursor(1, false);
            if inner.db_set.main_btree().pager().page_count() >= 1 {
                let _ = inner.db_set.main_btree_mut().cursor_first(cid);
            }
            let hid = inner.register_handle(HandleKind::TestBlob { writable: false });
            inner
                .activity
                .start_with_instances(hid, true, false, vec![main_inst], Vec::new());
            Ok(TestBlobHandle {
                shared: Arc::clone(&self.shared),
                handle_id: Some(hid),
                lease_cursor: Some((main_inst, cid)),
                writable: false,
            })
        }
    }

    /// Injects a one-shot simulated I/O error into the next `finish_activity` / `abort_write_statement`
    /// cleanup path to deterministically test `ConnectionState::Poisoned(SqliteError)`.
    #[doc(hidden)]
    pub fn set_simulated_cleanup_io_error_for_testing(
        &self,
        err: Option<SqliteError>,
    ) -> Result<()> {
        let mut session = self.shared.enter()?;
        session.inner_mut().simulated_cleanup_io_error = err;
        Ok(())
    }

    /// Returns the number of currently open low-level `BtreeCursor`s under the connection gate.
    #[doc(hidden)]
    pub fn open_cursor_count_for_testing(&self) -> Result<usize> {
        let session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        Ok(session
            .inner()
            .db_set
            .slots()
            .iter()
            .filter_map(|s| s.btree.as_ref())
            .map(Btree::open_cursor_count)
            .sum())
    }

    /// Arms a one-shot panic inside `process_pending_drops` to deterministically verify that
    /// `gate_busy` and `ACTIVE_CONN_STACK` never leak across unwinds in `enter()` or
    /// `drain_and_release_gate()`.
    #[doc(hidden)]
    pub fn arm_panic_on_next_pending_drop_for_testing(&self) -> Result<()> {
        let mut session = self.shared.enter()?;
        session.inner_mut().panic_on_next_pending_drop = true;
        Ok(())
    }

    /// Enqueues `stmt` onto `pending_drops` without draining immediately and arms a one-shot
    /// panic so the next `ConnectionShared::enter()` exercises the `initial_drops` unwind path.
    #[doc(hidden)]
    pub fn enqueue_pending_drop_for_enter_unwind_testing(&self, mut stmt: Statement) -> Result<()> {
        {
            let mut session = self.shared.enter()?;
            session.inner_mut().panic_on_next_pending_drop = true;
        }
        if let Some(hid) = stmt.handle_id.take() {
            let prev = std::mem::replace(&mut stmt.lifecycle, StatementLifecycle::Ready);
            let running = match prev {
                StatementLifecycle::Running(r) => Some(Box::new(r)),
                _ => None,
            };
            let mut pq = self.shared.lock_pending_drops();
            pq.push(PendingDrop::Statement {
                handle_id: hid,
                running,
            });
            self.shared
                .gate_state
                .fetch_or(GATE_PENDING_DROPS, Ordering::Release);
        }
        Ok(())
    }
}

/// Internal/test handle representing an open `sqlite3_blob` lease for Phase 1.1 ownership,
/// `try_close` `Busy`, `ActivityTracker`, and `pending_drops` tests.
#[doc(hidden)]
pub struct TestBlobHandle {
    shared: Arc<ConnectionShared>,
    handle_id: Option<HandleId>,
    lease_cursor: Option<(SchemaInstanceId, CursorId)>,
    writable: bool,
}

impl TestBlobHandle {
    pub fn close(mut self) -> Result<()> {
        let mut session = self.shared.enter()?;
        let Some(hid) = self.handle_id.take() else {
            return Ok(());
        };
        let lc = self.lease_cursor.take();
        let inner = session.inner_mut();
        let main_inst = inner.db_set.main_slot().schema_instance_id;
        let res = if self.writable {
            let snap = WriteAbortSnapshot {
                stmt_sp_slot: None,
                touched_write_instances: SmallInstVec::single(main_inst),
                started_read_instances: SmallInstVec::new(),
                fk_before: inner.deferred_fk_suspects.clone(),
                rt_before: inner.runtime,
                is_dml: false,
                mutation_len_before: inner.mutation_pipeline.len(),
                schemas_before: inner.snapshot_open_schemas(),
                schema_gen_before: inner.schema_generation,
                v1_expire_gen_before: inner.v1_expire_generation,
                schema_changed_before: inner.schema_changed_in_tx,
            };
            inner.finish_activity(
                hid,
                ActivityFinish::WriteCompleted {
                    abort_snapshot: snap,
                },
                &self.shared,
            )
        } else {
            let read_insts = match lc {
                Some((inst, _)) => SmallInstVec::single(inst),
                None => SmallInstVec::single(main_inst),
            };
            inner.finish_activity(
                hid,
                ActivityFinish::ReadDone {
                    lease_cursors: lc.into_iter().collect(),
                    read_instances: read_insts,
                },
                &self.shared,
            )
        };
        inner.open_handles.remove(&hid);
        res
    }
}

impl Drop for TestBlobHandle {
    fn drop(&mut self) {
        if let Some(hid) = self.handle_id.take() {
            self.shared.enqueue_or_run_drop(PendingDrop::TestBlob {
                handle_id: hid,
                lease_cursor: self.lease_cursor.take(),
                writable: self.writable,
            });
        }
    }
}

/// Prepared SQL statement (`sqlite3_stmt*` in `sqlite.h.in`).
///
/// Owns an `Arc` reference to the shared connection state (`Statement: Send`, no `'conn` lifetime).
pub struct Statement {
    shared: Arc<ConnectionShared>,
    handle_id: Option<HandleId>,
    mode: PrepareMode,
    raw_sql: String,
    stmt: Option<Stmt>,
    requires_schema: bool,
    category: Option<StmtCategory>,
    compiled_vdbe: Option<Arc<VdbeProgram>>,
    compiled_fast_read: Option<Arc<fast_read::FastReadPlan>>,
    cached_read_vm: Option<VdbeVm>,
    write_vm_scratch: sqliters_vdbe::VdbeWriteScratch,
    compiled_schema_generation: u64,
    compiled_v1_expire_generation: u64,
    compiled_advisory_expire_generation: u64,
    compiled_slot_cookies: Vec<(SchemaInstanceId, u32, bool)>,
    expired: bool,
    column_names: Vec<String>,
    params: Vec<Value>,
    named_params: HashMap<String, usize>,
    ignored_fk_parent_tables: Vec<(DatabaseId, String)>,
    unparsed_tail: String,
    lifecycle: StatementLifecycle,
}

impl fmt::Debug for Statement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Statement")
            .field("mode", &self.mode)
            .field("raw_sql", &self.raw_sql)
            .field("column_names", &self.column_names)
            .field("parameter_count", &self.params.len())
            .finish()
    }
}

struct CompiledArtifacts {
    stmt: Option<Stmt>,
    requires_schema: bool,
    category: Option<StmtCategory>,
    compiled_vdbe: Option<Arc<VdbeProgram>>,
    compiled_fast_read: Option<Arc<fast_read::FastReadPlan>>,
    column_names: Vec<String>,
    max_param: usize,
    named_params: HashMap<String, usize>,
    ignored_fk_parent_tables: Vec<(DatabaseId, String)>,
}

#[allow(clippy::too_many_arguments)]
fn compile_first_sql(
    raw_sql: &str,
    db_set: &DatabaseSet,
    writable_schema: bool,
    foreign_keys: bool,
    recursive_triggers: bool,
    count_changes: bool,
    enable_test_functions: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    authorizer: &mut AuthorizerSlot,
) -> Result<CompiledArtifacts> {
    let (stmt_opt, _) = parse_one(raw_sql)?;
    compile_parsed_stmt(
        stmt_opt,
        db_set,
        writable_schema,
        foreign_keys,
        recursive_triggers,
        count_changes,
        enable_test_functions,
        functions,
        collations,
        authorizer,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_parsed_stmt(
    stmt_opt: Option<Stmt>,
    db_set: &DatabaseSet,
    writable_schema: bool,
    foreign_keys: bool,
    recursive_triggers: bool,
    count_changes: bool,
    enable_test_functions: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    authorizer: &mut AuthorizerSlot,
) -> Result<CompiledArtifacts> {
    let _tf_guard = TestFunctionsScope::enter(enable_test_functions);
    let Some(stmt) = stmt_opt else {
        return Ok(CompiledArtifacts {
            stmt: None,
            requires_schema: false,
            category: None,
            compiled_vdbe: None,
            compiled_fast_read: None,
            column_names: Vec::new(),
            max_param: 0,
            named_params: HashMap::new(),
            ignored_fk_parent_tables: Vec::new(),
        });
    };

    let requires_schema = stmt_requires_schema(&stmt);
    let mut max_param = 0usize;
    let mut named_params = HashMap::new();
    collect_stmt_params(&stmt, &mut max_param, &mut named_params);

    let column_names = validate_and_infer_columns(
        &stmt,
        db_set,
        writable_schema,
        foreign_keys,
        recursive_triggers,
        count_changes,
        functions,
        collations,
    )?;

    let (stmt_opt_auth, ignored_fk_parent_tables) = driver::authorize_stmt_owned_with_slot_and_fks(
        stmt,
        db_set,
        functions,
        foreign_keys,
        authorizer,
    )?;
    let Some(stmt) = stmt_opt_auth else {
        return Ok(CompiledArtifacts {
            stmt: None,
            requires_schema,
            category: None,
            compiled_vdbe: None,
            compiled_fast_read: None,
            column_names: Vec::new(),
            max_param,
            named_params,
            ignored_fk_parent_tables,
        });
    };

    let category = Some(classify_stmt_category(&stmt));
    let mut compiled_vdbe = None;
    let mut compiled_fast_read = None;
    // Only use fast-path `VdbeVm` when all `DatabaseId` slots are `0` (`main`) or `1` (`temp`),
    // which never shift on `DETACH`. Attached `slots[2..]` queries run via `MaterializedRead`
    // keyed by `SchemaInstanceId` so `DETACH` slot-collapse never shifts active cursor indices!
    match &stmt {
        Stmt::Select(sel) => {
            if let Some(fp) =
                fast_read::try_compile_fast_read_plan(sel, db_set, functions, collations)
                && fp.db_id == DatabaseId::MAIN
            {
                compiled_fast_read = Some(Arc::new(fp));
            } else if let Some(prog) =
                compiler::try_compile_select_in_db_set(sel, db_set, functions)?
                && (prog.referenced_db_mask() & !0b11) == 0
            {
                compiled_vdbe = Some(Arc::new(prog));
            }
        }
        Stmt::Insert(ins) if !count_changes => {
            if let Some(prog) = compiler::try_compile_insert_in_db_set(ins, db_set, functions)?
                && (prog.referenced_db_mask() & !0b11) == 0
            {
                compiled_vdbe = Some(Arc::new(prog));
            }
        }
        Stmt::Update(upd) if !count_changes => {
            if let Some(prog) = compiler::try_compile_update_in_db_set(upd, db_set, functions)?
                && (prog.referenced_db_mask() & !0b11) == 0
            {
                compiled_vdbe = Some(Arc::new(prog));
            }
        }
        Stmt::Delete(del) if !count_changes => {
            if let Some(prog) = compiler::try_compile_delete_in_db_set(del, db_set, functions)?
                && (prog.referenced_db_mask() & !0b11) == 0
            {
                compiled_vdbe = Some(Arc::new(prog));
            }
        }
        _ => {}
    }

    Ok(CompiledArtifacts {
        stmt: Some(stmt),
        requires_schema,
        category,
        compiled_vdbe,
        compiled_fast_read,
        column_names,
        max_param,
        named_params,
        ignored_fk_parent_tables,
    })
}

fn select_references_temp_catalog(sel: &SelectStmt, db_set: &DatabaseSet) -> bool {
    if sel
        .ctes
        .iter()
        .any(|cte| select_references_temp_catalog(&cte.query, db_set))
    {
        return true;
    }
    let core_refs = |core: &SelectCore| -> bool {
        match core {
            SelectCore::Values(rows) => rows
                .iter()
                .any(|r| r.iter().any(|e| expr_references_temp_catalog(e, db_set))),
            SelectCore::Select {
                projection,
                from,
                selection,
                group_by,
                having,
                windows,
                ..
            } => {
                for rc in projection {
                    if let ResultColumn::Expr { expr, .. } = rc
                        && expr_references_temp_catalog(expr, db_set)
                    {
                        return true;
                    }
                }
                if let Some(fc) = from {
                    let tos_refs = |tos: &TableOrSubquery| -> bool {
                        match tos {
                            TableOrSubquery::Table { name, .. } => matches!(
                                db_set.resolve_catalog_table(name),
                                Ok(Some(DatabaseId::TEMP))
                            ),
                            TableOrSubquery::TableFunction { args, .. } => {
                                args.iter().any(|a| expr_references_temp_catalog(a, db_set))
                            }
                            TableOrSubquery::Subquery { query, .. } => {
                                select_references_temp_catalog(query, db_set)
                            }
                        }
                    };
                    if tos_refs(&fc.head) {
                        return true;
                    }
                    for j in &fc.joins {
                        if tos_refs(&j.right) {
                            return true;
                        }
                        if let Some(sqliters_syntax::JoinConstraint::On(ref e)) = j.constraint
                            && expr_references_temp_catalog(e, db_set)
                        {
                            return true;
                        }
                    }
                }
                if let Some(w) = selection
                    && expr_references_temp_catalog(w, db_set)
                {
                    return true;
                }
                if group_by
                    .iter()
                    .any(|g| expr_references_temp_catalog(g, db_set))
                {
                    return true;
                }
                if let Some(h) = having
                    && expr_references_temp_catalog(h, db_set)
                {
                    return true;
                }
                if windows
                    .iter()
                    .any(|(_, ws)| window_spec_references_temp_catalog(ws, db_set))
                {
                    return true;
                }
                false
            }
        }
    };
    if core_refs(&sel.body.head)
        || sel.body.compounds.iter().any(|(_, c)| core_refs(c))
        || sel
            .order_by
            .iter()
            .any(|ob| expr_references_temp_catalog(&ob.expr, db_set))
    {
        return true;
    }
    if let Some(ref lim) = sel.limit {
        if expr_references_temp_catalog(&lim.limit, db_set) {
            return true;
        }
        if let Some(ref off) = lim.offset
            && expr_references_temp_catalog(off, db_set)
        {
            return true;
        }
    }
    false
}

fn expr_references_temp_catalog(expr: &Expr, db_set: &DatabaseSet) -> bool {
    match expr {
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => expr_references_temp_catalog(inner, db_set),
        Expr::Binary { left, right, .. } => {
            expr_references_temp_catalog(left, db_set)
                || expr_references_temp_catalog(right, db_set)
        }
        Expr::Between {
            expr: v, low, high, ..
        } => {
            expr_references_temp_catalog(v, db_set)
                || expr_references_temp_catalog(low, db_set)
                || expr_references_temp_catalog(high, db_set)
        }
        Expr::InList { expr: v, list, .. } => {
            expr_references_temp_catalog(v, db_set)
                || list
                    .iter()
                    .any(|item| expr_references_temp_catalog(item, db_set))
        }
        Expr::InSubquery {
            expr: v, subquery, ..
        } => {
            expr_references_temp_catalog(v, db_set)
                || select_references_temp_catalog(subquery, db_set)
        }
        Expr::InTable { table, .. } => matches!(
            db_set.resolve_catalog_table(table),
            Ok(Some(DatabaseId::TEMP))
        ),
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            select_references_temp_catalog(subquery, db_set)
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_ref()
                .is_some_and(|e| expr_references_temp_catalog(e, db_set))
                || branches.iter().any(|(w, t)| {
                    expr_references_temp_catalog(w, db_set)
                        || expr_references_temp_catalog(t, db_set)
                })
                || else_expr
                    .as_ref()
                    .is_some_and(|e| expr_references_temp_catalog(e, db_set))
        }
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            let args_ref = match args {
                FunctionArgs::Exprs(es) => {
                    es.iter().any(|e| expr_references_temp_catalog(e, db_set))
                }
                _ => false,
            };
            args_ref
                || filter
                    .as_ref()
                    .is_some_and(|f| expr_references_temp_catalog(f, db_set))
                || matches!(over, Some(WindowRef::Spec(ws)) if window_spec_references_temp_catalog(ws, db_set))
        }
        Expr::RowValue(items) => items
            .iter()
            .any(|e| expr_references_temp_catalog(e, db_set)),
        _ => false,
    }
}

fn window_spec_references_temp_catalog(ws: &WindowSpec, db_set: &DatabaseSet) -> bool {
    ws.partition_by
        .iter()
        .any(|pe| expr_references_temp_catalog(pe, db_set))
        || ws
            .order_by
            .iter()
            .any(|ob| expr_references_temp_catalog(&ob.expr, db_set))
}

fn from_clause_references_temp_catalog(fc: &FromClause, db_set: &DatabaseSet) -> bool {
    let tos_refs = |tos: &TableOrSubquery| -> bool {
        match tos {
            TableOrSubquery::Table { name, .. } => matches!(
                db_set.resolve_catalog_table(name),
                Ok(Some(DatabaseId::TEMP))
            ),
            TableOrSubquery::TableFunction { args, .. } => {
                args.iter().any(|a| expr_references_temp_catalog(a, db_set))
            }
            TableOrSubquery::Subquery { query, .. } => {
                select_references_temp_catalog(query, db_set)
            }
        }
    };
    if tos_refs(&fc.head) {
        return true;
    }
    for j in &fc.joins {
        if tos_refs(&j.right) {
            return true;
        }
        if let Some(sqliters_syntax::JoinConstraint::On(ref e)) = j.constraint
            && expr_references_temp_catalog(e, db_set)
        {
            return true;
        }
    }
    false
}

fn stmt_references_temp_catalog(stmt: &Stmt, db_set: &DatabaseSet) -> bool {
    match stmt {
        Stmt::Explain { .. } => false,
        Stmt::Select(sel) => select_references_temp_catalog(sel, db_set),
        Stmt::CreateTableAs { query, .. } => select_references_temp_catalog(query, db_set),
        Stmt::Insert(ins) => {
            matches!(
                db_set.resolve_catalog_table(&ins.table),
                Ok(Some(DatabaseId::TEMP))
            ) || ins
                .ctes
                .iter()
                .any(|c| select_references_temp_catalog(&c.query, db_set))
                || match &ins.source {
                    InsertSource::Select(sel) => select_references_temp_catalog(sel, db_set),
                    InsertSource::Values(rows) => rows
                        .iter()
                        .any(|r| r.iter().any(|e| expr_references_temp_catalog(e, db_set))),
                    InsertSource::DefaultValues => false,
                }
                || ins.upsert_clauses.iter().any(|uc| {
                    uc.target.as_ref().is_some_and(|t| {
                        t.columns.iter().any(|ic| {
                            ic.expr
                                .as_ref()
                                .is_some_and(|e| expr_references_temp_catalog(e, db_set))
                        }) || t
                            .where_clause
                            .as_ref()
                            .is_some_and(|w| expr_references_temp_catalog(w, db_set))
                    }) || match &uc.action {
                        UpsertAction::DoNothing => false,
                        UpsertAction::DoUpdate {
                            assignments,
                            where_clause,
                        } => {
                            assignments
                                .iter()
                                .any(|a| expr_references_temp_catalog(&a.expr, db_set))
                                || where_clause
                                    .as_ref()
                                    .is_some_and(|w| expr_references_temp_catalog(w, db_set))
                        }
                    }
                })
                || ins.returning.iter().any(|rc| {
                    matches!(rc, ResultColumn::Expr { expr, .. } if expr_references_temp_catalog(expr, db_set))
                })
        }
        Stmt::Update(upd) => {
            matches!(
                db_set.resolve_catalog_table(&upd.table),
                Ok(Some(DatabaseId::TEMP))
            ) || upd
                .ctes
                .iter()
                .any(|c| select_references_temp_catalog(&c.query, db_set))
                || upd
                    .assignments
                    .iter()
                    .any(|a| expr_references_temp_catalog(&a.expr, db_set))
                || upd
                    .from
                    .as_ref()
                    .is_some_and(|fc| from_clause_references_temp_catalog(fc, db_set))
                || upd
                    .selection
                    .as_ref()
                    .is_some_and(|w| expr_references_temp_catalog(w, db_set))
                || upd.returning.iter().any(|rc| {
                    matches!(rc, ResultColumn::Expr { expr, .. } if expr_references_temp_catalog(expr, db_set))
                })
        }
        Stmt::Delete(del) => {
            matches!(
                db_set.resolve_catalog_table(&del.table),
                Ok(Some(DatabaseId::TEMP))
            ) || del
                .ctes
                .iter()
                .any(|c| select_references_temp_catalog(&c.query, db_set))
                || del
                    .selection
                    .as_ref()
                    .is_some_and(|w| expr_references_temp_catalog(w, db_set))
                || del.returning.iter().any(|rc| {
                    matches!(rc, ResultColumn::Expr { expr, .. } if expr_references_temp_catalog(expr, db_set))
                })
        }
        _ => false,
    }
}

fn apply_prepare_auto_vacuum_pragma(
    inner: &mut ConnectionInner,
    artifacts: &mut CompiledArtifacts,
) {
    // `sqlite3Pragma` (`pragma.c:802-845`) updates `nextAutovac` and `sqlite3BtreeSetAutoVacuum`
    // at `prepare` time, emitting `setMeta6` (`StmtCategory::Write`) only when `ok && e_auto != None`!
    if let Some(Stmt::Pragma {
        ref schema,
        ref name,
        value: Some(ref pv),
    }) = artifacts.stmt
        && name.eq_ignore_ascii_case("auto_vacuum")
    {
        let target_db = match schema.as_deref() {
            None => Some(DatabaseId::MAIN),
            Some(s) => inner.db_set.find_db_id(s),
        };
        match target_db {
            None | Some(DatabaseId::TEMP) => {
                artifacts.category = Some(StmtCategory::NoLockPragma);
            }
            Some(db_id) => {
                let e_auto = parse_auto_vacuum_pragma(pv);
                let ok = if let Some(slot) = inner.db_set.slot_mut(db_id) {
                    slot.next_auto_vacuum = Some(e_auto);
                    slot.btree
                        .as_mut()
                        .is_some_and(|bt| bt.set_configured_auto_vacuum(e_auto))
                } else {
                    false
                };
                artifacts.category = Some(
                    if ok && matches!(e_auto, AutoVacuumMode::Full | AutoVacuumMode::Incremental) {
                        StmtCategory::Write
                    } else {
                        StmtCategory::NoLockPragma
                    },
                );
            }
        }
    }
}

fn target_write_db_for_stmt(stmt: &Stmt, db_set: &DatabaseSet) -> Option<DatabaseId> {
    let resolve_qn_table = |qn: &QualifiedName| -> Option<DatabaseId> {
        if let Some(ref s) = qn.schema {
            db_set.find_db_id(s)
        } else if let Ok(Some(cat_db)) = db_set.resolve_catalog_table(qn) {
            Some(cat_db)
        } else {
            db_set
                .find_relation_in_set(qn)
                .ok()
                .flatten()
                .map(|(id, _)| id)
                .or(Some(DatabaseId::MAIN))
        }
    };
    let resolve_dml_target = |qn: &QualifiedName| -> Option<DatabaseId> {
        if let Ok(Some(cat_db)) = db_set.resolve_catalog_table(qn) {
            return Some(cat_db);
        }
        if let Some((id, _)) = db_set.find_virtual_table_in_set(qn) {
            return Some(id);
        }
        match db_set.find_relation_in_set(qn) {
            // `if (!isView) sqlite3BeginWriteOperation(...)` (`insert.c:1058`): DML on a `VIEW`
            // does NOT pre-acquire a `Write` transaction on the view's database!
            Ok(Some((_, ResolvedRelation::View(_)))) => None,
            Ok(Some((id, ResolvedRelation::Table(_)))) => Some(id),
            _ => {
                if let Some(ref s) = qn.schema {
                    db_set.find_db_id(s)
                } else {
                    Some(DatabaseId::MAIN)
                }
            }
        }
    };
    match stmt {
        Stmt::Insert(ins) => resolve_dml_target(&ins.table),
        Stmt::Update(upd) => resolve_dml_target(&upd.table),
        Stmt::Delete(del) => resolve_dml_target(&del.table),
        Stmt::CreateVirtualTable { name, .. } => {
            if let Some(ref s) = name.schema {
                db_set.find_db_id(s)
            } else {
                Some(DatabaseId::MAIN)
            }
        }
        Stmt::CreateTable(ct) => {
            if ct.temp {
                Some(DatabaseId::TEMP)
            } else if let Some(ref s) = ct.name.schema {
                db_set.find_db_id(s)
            } else {
                Some(DatabaseId::MAIN)
            }
        }
        Stmt::CreateTableAs { temp, name, .. } => {
            if *temp {
                Some(DatabaseId::TEMP)
            } else if let Some(ref s) = name.schema {
                db_set.find_db_id(s)
            } else {
                Some(DatabaseId::MAIN)
            }
        }
        Stmt::CreateView(cv) => {
            if cv.temp {
                Some(DatabaseId::TEMP)
            } else if let Some(ref s) = cv.name.schema {
                db_set.find_db_id(s)
            } else {
                Some(DatabaseId::MAIN)
            }
        }
        Stmt::CreateTrigger(ct) => {
            if ct.temp {
                Some(DatabaseId::TEMP)
            } else if let Some(ref s) = ct.name.schema {
                db_set.find_db_id(s)
            } else if matches!(
                db_set.find_relation_in_set(&ct.on_table),
                Ok(Some((DatabaseId::TEMP, _)))
            ) {
                Some(DatabaseId::TEMP)
            } else {
                Some(DatabaseId::MAIN)
            }
        }
        Stmt::DropTrigger { name, .. } => db_set
            .find_trigger_in_set(name)
            .map(|(id, _)| id)
            .or_else(|| name.schema.as_deref().and_then(|s| db_set.find_db_id(s)))
            .or(Some(DatabaseId::MAIN)),
        Stmt::DropTable { name, .. }
        | Stmt::DropView { name, .. }
        | Stmt::AlterTable { table: name, .. } => resolve_qn_table(name),
        Stmt::CreateIndex(ci) => {
            if let Some(ref s) = ci.name.schema {
                db_set.find_db_id(s)
            } else {
                db_set
                    .find_relation_in_set(&QualifiedName::unqualified(ci.table_name.clone()))
                    .ok()
                    .flatten()
                    .map(|(id, _)| id)
                    .or(Some(DatabaseId::MAIN))
            }
        }
        Stmt::DropIndex { name, .. } => {
            if let Some(ref s) = name.schema {
                db_set.find_db_id(s)
            } else {
                db_set
                    .find_index_including_without_rowid_pk_in_set(name)
                    .map(|(id, _)| id)
                    .or(Some(DatabaseId::MAIN))
            }
        }
        Stmt::Reindex(Some(qn)) => {
            if let Some(ref s) = qn.schema {
                db_set.find_db_id(s)
            } else {
                db_set
                    .find_relation_in_set(qn)
                    .ok()
                    .flatten()
                    .map(|(id, _)| id)
                    .or_else(|| db_set.find_index_in_set(qn).map(|(id, _)| id))
            }
        }
        Stmt::Reindex(None) => None,
        Stmt::Analyze(Some(qn)) => {
            if let Some(ref s) = qn.schema {
                db_set.find_db_id(s)
            } else if let Some(id) = db_set.find_db_id(&qn.name) {
                Some(id)
            } else if let Some(id) = db_set.find_index_in_set(qn).map(|(id, _)| id) {
                Some(id)
            } else {
                db_set
                    .find_relation_in_set(qn)
                    .ok()
                    .flatten()
                    .map(|(id, _)| id)
                    .or_else(|| db_set.resolve_catalog_table(qn).ok().flatten())
            }
        }
        Stmt::Analyze(None) => Some(DatabaseId::MAIN),
        Stmt::Pragma { schema, .. } => match schema.as_deref() {
            Some(s) => db_set.find_db_id(s),
            None => Some(DatabaseId::MAIN),
        },
        _ => None,
    }
}

fn is_noop_if_exists_ddl(stmt: &Stmt, db_set: &DatabaseSet) -> bool {
    match stmt {
        Stmt::CreateTable(ct) if ct.if_not_exists => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some()
                    && (sl.schema.find_user_table(&ct.name.name).is_some()
                        || sl.schema.find_view(&ct.name.name).is_some()
                        || sl.schema.find_virtual_table(&ct.name.name).is_some())
            }),
        Stmt::CreateVirtualTable {
            if_not_exists: true,
            name,
            ..
        } => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some()
                    && (sl.schema.find_user_table(&name.name).is_some()
                        || sl.schema.find_view(&name.name).is_some()
                        || sl.schema.find_virtual_table(&name.name).is_some())
            }),
        Stmt::CreateTableAs {
            if_not_exists: true,
            name,
            ..
        } => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some()
                    && (sl.schema.find_user_table(&name.name).is_some()
                        || sl.schema.find_view(&name.name).is_some()
                        || sl.schema.find_virtual_table(&name.name).is_some())
            }),
        Stmt::CreateView(cv) if cv.if_not_exists => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some()
                    && (sl.schema.find_user_table(&cv.name.name).is_some()
                        || sl.schema.find_view(&cv.name.name).is_some()
                        || sl.schema.find_virtual_table(&cv.name.name).is_some())
            }),
        Stmt::CreateTrigger(ct) if ct.if_not_exists => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some() && sl.schema.find_trigger(&ct.name.name).is_some()
            }),
        Stmt::CreateIndex(ci) if ci.if_not_exists => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| sl.btree.is_some() && sl.schema.find_index(&ci.name.name).is_some()),
        Stmt::DropTable {
            if_exists: true,
            name,
        } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                return true;
            }
            matches!(db_set.find_relation_in_set(name), Ok(None))
        }
        Stmt::DropView {
            if_exists: true,
            name,
        } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                return true;
            }
            matches!(db_set.find_relation_in_set(name), Ok(None))
        }
        Stmt::DropTrigger {
            if_exists: true,
            name,
        } => db_set.find_trigger_in_set(name).is_none(),
        Stmt::DropIndex {
            if_exists: true,
            name,
        } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                return true;
            }
            db_set
                .find_index_including_without_rowid_pk_in_set(name)
                .is_none()
        }
        _ => false,
    }
}

impl ConnectionInner {
    fn with_exec_ctx<R>(
        &mut self,
        shared: &ConnectionShared,
        summary: ActivitySummary,
        stmt_tx: &mut StmtTxTracker,
        expire_self_only: &mut bool,
        f: impl FnOnce(&mut ExecContext<'_>) -> R,
    ) -> R {
        let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
            shared.publish_mirrors(rt, exp, imp, rb);
        };
        let mut ctx = ExecContext {
            db_set: &mut self.db_set,
            connection_encoding: &mut self.connection_encoding,
            uri_enabled: shared.open_options.uri,
            is_connection_readonly: shared.is_readonly,
            runtime: &mut self.runtime,
            interrupt_flag: &self.active_interrupt_flag,
            functions: &self.functions,
            collations: &self.collations,
            progress: &mut self.progress,
            commit_hooks: &mut self.commit_hooks,
            mutation_pipeline: &mut self.mutation_pipeline,
            strict_subtype: self.strict_subtype,
            explicit_tx: &mut self.explicit_tx,
            implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
            in_user_btree_tx: &mut self.in_user_btree_tx,
            savepoints: &mut self.savepoints,
            schema_generation: &mut self.schema_generation,
            v1_expire_generation: &mut self.v1_expire_generation,
            expire_self_only,
            schema_changed_in_tx: &mut self.schema_changed_in_tx,
            activity: summary,
            stmt_tx,
            foreign_keys: &mut self.foreign_keys,
            recursive_triggers: &mut self.recursive_triggers,
            legacy_alter_table: &mut self.legacy_alter_table,
            count_changes: &mut self.count_changes,
            wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
            default_locking_mode: &mut self.default_locking_mode,
            writable_schema: &mut self.writable_schema,
            working_dir: &self.working_dir,
            busy_policy: &mut self.busy_policy,
            rollback_count: &mut self.rollback_count,
            deferred_fk_suspects: &mut self.deferred_fk_suspects,
            trace_hook: &mut self.trace_hook,
            publish_mirrors: &mut publish_fn,
        };
        f(&mut ctx)
    }

    fn end_autocommit_started_reads(&mut self, started_mask: u16, summary: ActivitySummary) {
        if self.in_user_btree_tx || !self.is_autocommit() || started_mask == 0 {
            return;
        }
        let active_mask = summary.other_read_db_mask | summary.other_write_db_mask;
        for idx in 0..self.db_set.slot_count() {
            let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
            if (started_mask & bit) != 0
                && (active_mask & bit) == 0
                && summary.other_writers == 0
                && let Some(ref mut bt) = self.db_set.slots_mut()[idx].btree
                && bt.transaction_state() == TransactionState::Read
            {
                let _ = bt.pager_mut().end_read_if_unused();
            }
        }
    }

    pub(crate) fn try_fast_exec_literal_insert_step<'a>(
        &mut self,
        shared: &Arc<ConnectionShared>,
        sql: &'a str,
    ) -> Option<(Result<()>, &'a str)> {
        if !matches!(self.state, ConnectionState::Open)
            || self.trace_hook.is_some()
            || self.authorizer.is_some()
            || self.count_changes
            || self.writable_schema
            || self.mutation_pipeline.has_update_hook()
            || self.mutation_pipeline.is_trace_enabled()
            || !self.activity.leases.is_empty()
            || !self.db_set.open_blobs().is_empty()
            || self.db_set.vtab_registry().has_any_in_tx()
            || self.effective_encoding() != TextEncoding::Utf8
        {
            return None;
        }

        // Quick first-byte rejection before scanning: must start with `I` or `i` (`INSERT`).
        let first_byte = *sql.as_bytes().first()?;
        if first_byte != b'I' && first_byte != b'i' {
            return None;
        }

        if !self.db_set.main_slot().schema.loaded {
            return None;
        }
        if self.foreign_keys
            && self
                .db_set
                .slots()
                .iter()
                .any(|s| s.schema.tables.iter().any(|t| !t.foreign_keys.is_empty()))
        {
            return None;
        }

        let (table_name, n_args, tail) = parse_simple_literal_insert_prefix(
            sql,
            &mut self.lit_insert_params,
            &mut self.lit_insert_shapes,
        )?;

        if let Some((ref neg_tbl, neg_gen)) = self.lit_insert_neg_table
            && neg_gen == self.schema_generation
            && neg_tbl.eq_ignore_ascii_case(table_name)
        {
            return None;
        }

        let temp_open = self.db_set.is_temp_open();
        let main_inst = self.db_set.main_slot().schema_instance_id;
        if self.db_set.main_btree().transaction_state() != TransactionState::Open {
            let bt = self.db_set.main_btree();
            let has_pages = bt.pager().page_count() > 0;
            let cur_cookie = bt.pager().header().schema_cookie;
            let main_schema = &self.db_set.main_slot().schema;
            if main_schema.had_pages != has_pages || main_schema.schema_cookie != cur_cookie {
                return None;
            }
        }
        let main_cookie = self.db_set.main_slot().schema.schema_cookie;
        let main_had_pages = self.db_set.main_slot().schema.had_pages;

        let cache_hit = self.cached_lit_insert.as_ref().is_some_and(|c| {
            c.schema_generation == self.schema_generation
                && c.v1_expire_generation == self.v1_expire_generation
                && c.advisory_expire_generation == self.advisory_expire_generation
                && c.main_instance_id == main_inst
                && c.main_cookie == main_cookie
                && c.main_had_pages == main_had_pages
                && c.temp_open == temp_open
                && c.shapes == self.lit_insert_shapes
                && c.table_name.eq_ignore_ascii_case(table_name)
        });

        let prog = if cache_hit {
            Arc::clone(&self.cached_lit_insert.as_ref().unwrap().program)
        } else {
            let mut synth = String::with_capacity(32 + table_name.len() + n_args * 12);
            synth.push_str("INSERT INTO \"");
            synth.push_str(table_name);
            synth.push_str("\" VALUES (");
            for (i, shape) in self.lit_insert_shapes.iter().enumerate() {
                if i > 0 {
                    synth.push(',');
                }
                let p_idx = i + 1;
                match shape {
                    LitArgShape::Param => {
                        use std::fmt::Write as _;
                        let _ = write!(synth, "?{p_idx}");
                    }
                    LitArgShape::JsonbCall => {
                        use std::fmt::Write as _;
                        let _ = write!(synth, "jsonb(?{p_idx})");
                    }
                }
            }
            synth.push(')');
            let Ok((Some(Stmt::Insert(ins)), _)) = parse_one(&synth) else {
                return None;
            };
            if !temp_open
                && matches!(
                    self.db_set.resolve_catalog_table(&ins.table),
                    Ok(Some(DatabaseId::TEMP))
                )
            {
                return None;
            }
            let compiled = {
                let _tf_guard = TestFunctionsScope::enter(self.runtime.enable_test_functions);
                compiler::try_compile_insert_in_db_set(&ins, &self.db_set, &self.functions)
            };
            let Ok(Some(compiled_prog)) = compiled else {
                self.lit_insert_neg_table = Some((table_name.to_owned(), self.schema_generation));
                return None;
            };
            if compiled_prog.primary_write_db() != DatabaseId::MAIN
                || compiled_prog.referenced_db_mask() != 1u16
            {
                self.lit_insert_neg_table = Some((table_name.to_owned(), self.schema_generation));
                return None;
            }
            let arc_prog = Arc::new(compiled_prog);
            self.cached_lit_insert = Some(CachedLiteralInsert {
                table_name: table_name.to_owned(),
                shapes: self.lit_insert_shapes.clone(),
                schema_generation: self.schema_generation,
                v1_expire_generation: self.v1_expire_generation,
                advisory_expire_generation: self.advisory_expire_generation,
                main_instance_id: main_inst,
                main_cookie,
                main_had_pages,
                temp_open,
                program: Arc::clone(&arc_prog),
            });
            arc_prog
        };

        let base_sp_depth = self.savepoints.len();
        let prev_tx_state = self.db_set.main_btree().transaction_state();
        let opened_write_here = prev_tx_state != TransactionState::Write;
        if opened_write_here {
            let res = {
                let bt = self.db_set.slots_mut()[0].btree.as_mut().unwrap();
                self.busy_policy.retry_btree(bt, |b| {
                    b.pager_mut()
                        .begin_write_with_savepoint_depth(base_sp_depth)
                })
            };
            if let Err(e) = res {
                return Some((Err(e), tail));
            }
            let bt = self.db_set.main_btree();
            let has_pages = bt.pager().page_count() > 0;
            let cur_cookie = bt.pager().header().schema_cookie;
            if cur_cookie != main_cookie || has_pages != main_had_pages {
                let retain_read = prev_tx_state == TransactionState::Read;
                if let Some(ref mut bt_mut) = self.db_set.slots_mut()[0].btree
                    && let Err(rb_err) = bt_mut.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        true,
                        retain_read,
                    )
                {
                    return Some((Err(self.poison_with(rb_err)), tail));
                }
                self.cached_lit_insert = None;
                return None;
            }
        } else if base_sp_depth > 0
            && let Err(e) = self.db_set.slots_mut()[0]
                .btree
                .as_mut()
                .unwrap()
                .pager_mut()
                .ensure_subsavepoints(base_sp_depth)
        {
            return Some((Err(e), tail));
        }

        let is_autocommit = self.is_autocommit();
        let can_elide_sp = !prog.needs_stmt_subsavepoint()
            && (!prog.has_function_call() || !self.functions.has_user_functions())
            && !self.progress.is_enabled()
            && !shared.has_external_interrupt.load(Ordering::Acquire)
            && !self.active_interrupt_flag.load(Ordering::Relaxed)
            && self.db_set.main_slot().max_page_count >= SQLITE_MAX_PAGE_COUNT;
        let stmt_sp_slot = if !is_autocommit {
            if can_elide_sp {
                self.in_user_btree_tx = true;
                Some(ELIDED_STMT_SP)
            } else {
                let sp_idx = self.savepoints.len();
                if let Err(e) = self.db_set.slots_mut()[0]
                    .btree
                    .as_mut()
                    .unwrap()
                    .pager_mut()
                    .ensure_subsavepoints(sp_idx + 1)
                {
                    if opened_write_here {
                        let retain_read = prev_tx_state == TransactionState::Read;
                        if let Some(ref mut bt_mut) = self.db_set.slots_mut()[0].btree
                            && let Err(rb_err) = bt_mut.rollback_with_trip(
                                extended::SQLITE_ABORT_ROLLBACK,
                                true,
                                retain_read,
                            )
                        {
                            return Some((Err(self.poison_with(rb_err)), tail));
                        }
                    }
                    return Some((Err(e), tail));
                }
                self.in_user_btree_tx = true;
                Some(sp_idx)
            }
        } else {
            None
        };

        let rt_before = self.runtime;
        let mutation_len_before = self.mutation_pipeline.len();
        let schema_gen_before = self.schema_generation;
        let v1_expire_gen_before = self.v1_expire_generation;
        let schema_changed_before = self.schema_changed_in_tx;

        if let Err(e) = self.check_any_interrupted() {
            let snap = WriteAbortSnapshot {
                stmt_sp_slot,
                touched_write_instances: SmallInstVec::single(main_inst),
                started_read_instances: SmallInstVec::new(),
                fk_before: self.deferred_fk_suspects.clone(),
                rt_before,
                is_dml: true,
                mutation_len_before,
                schemas_before: Vec::new(),
                schema_gen_before,
                v1_expire_gen_before,
                schema_changed_before,
            };
            return Some((
                Err(self.abort_write_statement(HandleId(0), &e, snap, shared)),
                tail,
            ));
        }

        let (mut run_res, new_last_rowid, new_changes) = if let Some(ins_plan) =
            prog.single_row_insert()
            && (!ins_plan.has_builtin_jsonb || !self.functions.has_user_functions())
            && !self.progress.is_enabled()
        {
            let bt = self.db_set.slots_mut()[0].btree.as_mut().unwrap();
            let (scratch_rec, scratch_jb) = self.lit_insert_scratch.scratch_buffers_mut();
            match execute_single_row_insert_plan(
                ins_plan,
                &self.lit_insert_params[..n_args],
                scratch_rec,
                scratch_jb,
                bt,
            ) {
                Ok(rid) => {
                    self.progress
                        .add_ticks_without_handler(prog.ops.len() as u64);
                    (Ok(()), rid, 1u64)
                }
                Err(e) => (Err(e), self.runtime.last_insert_rowid, 0u64),
            }
        } else {
            let prev_ts = self.runtime.stmt_timestamp_ms;
            let stmt_ts = if prev_ts.is_some() {
                prev_ts
            } else if prog.needs_stmt_timestamp() {
                Some(self.db_set.main_btree().pager().vfs().current_unix_millis())
            } else {
                None
            };
            let enc = self.effective_encoding();
            let mut vm_rt = VmRuntimeContext {
                last_insert_rowid: self.runtime.last_insert_rowid,
                last_changes: 0,
                total_changes: 0,
                stmt_timestamp_ms: stmt_ts,
                enable_test_functions: self.runtime.enable_test_functions,
            };
            let svc_rt = vm_rt;
            let r = {
                let mut exec_svc = ExecutionContext {
                    interrupt_flag: &self.active_interrupt_flag,
                    runtime: &svc_rt,
                    encoding: enc,
                    functions: &self.functions,
                    collations: &self.collations,
                    progress: &mut self.progress,
                    strict_subtype: self.strict_subtype,
                };
                sqliters_vdbe::VdbeVm::run_write_program(
                    &prog,
                    &self.lit_insert_params[..n_args],
                    &mut vm_rt,
                    &mut self.lit_insert_scratch,
                    &mut self.db_set,
                    &mut exec_svc,
                )
            };
            (r, vm_rt.last_insert_rowid, vm_rt.last_changes)
        };
        if new_last_rowid != self.runtime.last_insert_rowid {
            self.runtime.last_insert_rowid = new_last_rowid;
        }
        if run_res.is_ok() {
            let slot = self.db_set.main_slot();
            if let Some(ref bt) = slot.btree
                && bt.pager().page_count() > slot.max_page_count
            {
                run_res = Err(SqliteError::new(
                    ErrorCode::Full,
                    "database or disk is full",
                ));
            }
        }
        if let Err(err) = run_res {
            let snap = WriteAbortSnapshot {
                stmt_sp_slot,
                touched_write_instances: SmallInstVec::single(main_inst),
                started_read_instances: SmallInstVec::new(),
                fk_before: self.deferred_fk_suspects.clone(),
                rt_before,
                is_dml: true,
                mutation_len_before,
                schemas_before: Vec::new(),
                schema_gen_before,
                v1_expire_gen_before,
                schema_changed_before,
            };
            return Some((
                Err(self.abort_write_statement(HandleId(0), &err, snap, shared)),
                tail,
            ));
        }

        self.runtime.last_changes = new_changes;
        self.runtime.total_changes += new_changes;

        if !is_autocommit && self.simulated_cleanup_io_error.is_none() {
            if let Some(sp_slot) = stmt_sp_slot
                && let Some(ref mut bt) = self.db_set.slots_mut()[0].btree
                && bt.transaction_state() == TransactionState::Write
            {
                if sp_slot != ELIDED_STMT_SP
                    && let Err(e) = bt.pager_mut().release_subsavepoint(sp_slot)
                {
                    return Some((Err(self.poison_with(e)), tail));
                }
                if let Err(e) = bt.spill_if_over_cache_size() {
                    return Some((Err(self.poison_with(e)), tail));
                }
            }
            self.publish(shared);
            return Some((Ok(()), tail));
        }

        let snap = WriteAbortSnapshot {
            stmt_sp_slot,
            touched_write_instances: SmallInstVec::single(main_inst),
            started_read_instances: SmallInstVec::new(),
            fk_before: self.deferred_fk_suspects.clone(),
            rt_before,
            is_dml: true,
            mutation_len_before,
            schemas_before: Vec::new(),
            schema_gen_before,
            v1_expire_gen_before,
            schema_changed_before,
        };
        let fin_res = self.finish_activity(
            HandleId(0),
            ActivityFinish::WriteCompleted {
                abort_snapshot: snap,
            },
            shared,
        );
        Some((fin_res, tail))
    }
}

#[inline]
fn skip_ascii_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

#[inline]
fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn parse_sql_string_into(sql: &str, mut pos: usize, slot: &mut Value) -> Option<usize> {
    let b = sql.as_bytes();
    if pos >= b.len() || b[pos] != b'\'' {
        return None;
    }
    pos += 1;
    let start = pos;
    let mut has_escaped = false;
    while pos < b.len() {
        if b[pos] == b'\'' {
            if pos + 1 < b.len() && b[pos + 1] == b'\'' {
                has_escaped = true;
                pos += 2;
                continue;
            }
            let raw = &sql[start..pos];
            if let Value::Text(s) = slot {
                s.clear();
                if !has_escaped {
                    s.push_str(raw);
                } else {
                    s.reserve(raw.len());
                    let mut chars = raw.chars();
                    while let Some(ch) = chars.next() {
                        s.push(ch);
                        if ch == '\'' {
                            let _ = chars.next();
                        }
                    }
                }
            } else if !has_escaped {
                *slot = Value::Text(raw.to_owned());
            } else {
                let mut s = String::with_capacity(raw.len());
                let mut chars = raw.chars();
                while let Some(ch) = chars.next() {
                    s.push(ch);
                    if ch == '\'' {
                        let _ = chars.next();
                    }
                }
                *slot = Value::Text(s);
            }
            return Some(pos + 1);
        }
        pos += 1;
    }
    None
}

fn parse_sql_hex_blob_into(b: &[u8], mut pos: usize, slot: &mut Value) -> Option<usize> {
    if pos + 1 >= b.len() || (b[pos] != b'X' && b[pos] != b'x') || b[pos + 1] != b'\'' {
        return None;
    }
    pos += 2;
    let start = pos;
    while pos < b.len() && b[pos] != b'\'' {
        pos += 1;
    }
    if pos >= b.len() {
        return None;
    }
    let hex_len = pos - start;
    if !hex_len.is_multiple_of(2) {
        return None;
    }
    let out_len = hex_len / 2;
    let buf = if let Value::Blob(v) = slot {
        v.clear();
        v.reserve(out_len);
        v
    } else {
        *slot = Value::Blob(Vec::with_capacity(out_len));
        let Value::Blob(v) = slot else {
            unreachable!();
        };
        v
    };
    let mut k = start;
    while k < pos {
        let hi = hex_nibble(b[k])?;
        let lo = hex_nibble(b[k + 1])?;
        buf.push((hi << 4) | lo);
        k += 2;
    }
    Some(pos + 1)
}

fn parse_sql_hex_utf8_into(b: &[u8], mut pos: usize, slot: &mut Value) -> Option<usize> {
    if pos + 1 >= b.len() || (b[pos] != b'X' && b[pos] != b'x') || b[pos + 1] != b'\'' {
        return None;
    }
    pos += 2;
    let start = pos;
    while pos < b.len() && b[pos] != b'\'' {
        pos += 1;
    }
    if pos >= b.len() {
        return None;
    }
    let hex_len = pos - start;
    if !hex_len.is_multiple_of(2) {
        return None;
    }
    let out_len = hex_len / 2;
    if out_len <= 256 {
        let mut stack_buf = [0u8; 256];
        let mut k = start;
        let mut idx = 0usize;
        while k < pos {
            let hi = hex_nibble(b[k])?;
            let lo = hex_nibble(b[k + 1])?;
            stack_buf[idx] = (hi << 4) | lo;
            idx += 1;
            k += 2;
        }
        let raw = &stack_buf[..out_len];
        match std::str::from_utf8(raw) {
            Ok(valid) => {
                if let Value::Text(s) = slot {
                    s.clear();
                    s.push_str(valid);
                } else {
                    *slot = Value::Text(valid.to_owned());
                }
            }
            Err(_) => {
                *slot = Value::Text(String::from_utf8_lossy(raw).into_owned());
            }
        }
    } else {
        let mut heap_buf = Vec::with_capacity(out_len);
        let mut k = start;
        while k < pos {
            let hi = hex_nibble(b[k])?;
            let lo = hex_nibble(b[k + 1])?;
            heap_buf.push((hi << 4) | lo);
            k += 2;
        }
        match String::from_utf8(heap_buf) {
            Ok(valid) => *slot = Value::Text(valid),
            Err(e) => *slot = Value::Text(String::from_utf8_lossy(e.as_bytes()).into_owned()),
        }
    }
    Some(pos + 1)
}

fn parse_simple_literal_insert_prefix<'a>(
    sql: &'a str,
    params_out: &mut Vec<Value>,
    shapes_out: &mut Vec<LitArgShape>,
) -> Option<(&'a str, usize, &'a str)> {
    let b = sql.as_bytes();
    if b.len() < 20 || !b[..6].eq_ignore_ascii_case(b"INSERT") {
        return None;
    }
    let mut i = skip_ascii_ws(b, 6);
    if i == 6 || i + 4 >= b.len() || !b[i..i + 4].eq_ignore_ascii_case(b"INTO") {
        return None;
    }
    let after_into = i + 4;
    i = skip_ascii_ws(b, after_into);
    if i == after_into || i >= b.len() {
        return None;
    }
    let tbl_start = i;
    if !(b[i].is_ascii_alphabetic() || b[i] == b'_') {
        return None;
    }
    i += 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    let table_name = &sql[tbl_start..i];
    let after_tbl = i;
    i = skip_ascii_ws(b, after_tbl);
    if i == after_tbl || i + 6 >= b.len() || !b[i..i + 6].eq_ignore_ascii_case(b"VALUES") {
        return None;
    }
    i = skip_ascii_ws(b, i + 6);
    if i >= b.len() || b[i] != b'(' {
        return None;
    }
    i += 1;

    shapes_out.clear();
    let mut arg_idx = 0usize;
    loop {
        i = skip_ascii_ws(b, i);
        if i >= b.len() {
            return None;
        }
        if arg_idx == params_out.len() {
            params_out.push(Value::Null);
        }
        let slot = &mut params_out[arg_idx];
        let c = b[i];
        if c == b'\'' {
            i = parse_sql_string_into(sql, i, slot)?;
            shapes_out.push(LitArgShape::Param);
        } else if (c == b'X' || c == b'x') && i + 1 < b.len() && b[i + 1] == b'\'' {
            i = parse_sql_hex_blob_into(b, i, slot)?;
            shapes_out.push(LitArgShape::Param);
        } else if (c == b'N' || c == b'n')
            && i + 4 <= b.len()
            && b[i..i + 4].eq_ignore_ascii_case(b"NULL")
            && (i + 4 == b.len() || matches!(b[i + 4], b',' | b')' | b' ' | b'\t' | b'\r' | b'\n'))
        {
            *slot = Value::Null;
            shapes_out.push(LitArgShape::Param);
            i += 4;
        } else if (c == b'C' || c == b'c')
            && i + 4 < b.len()
            && b[i..i + 4].eq_ignore_ascii_case(b"CAST")
        {
            let mut p = skip_ascii_ws(b, i + 4);
            if p >= b.len() || b[p] != b'(' {
                return None;
            }
            p = skip_ascii_ws(b, p + 1);
            if p < b.len() && b[p] == b'\'' {
                p = parse_sql_string_into(sql, p, slot)?;
            } else {
                p = parse_sql_hex_utf8_into(b, p, slot)?;
            }
            let after_inner = p;
            p = skip_ascii_ws(b, after_inner);
            if p == after_inner || p + 2 >= b.len() || !b[p..p + 2].eq_ignore_ascii_case(b"AS") {
                return None;
            }
            let after_as = p + 2;
            p = skip_ascii_ws(b, after_as);
            if p == after_as || p + 4 >= b.len() || !b[p..p + 4].eq_ignore_ascii_case(b"TEXT") {
                return None;
            }
            p = skip_ascii_ws(b, p + 4);
            if p >= b.len() || b[p] != b')' {
                return None;
            }
            shapes_out.push(LitArgShape::Param);
            i = p + 1;
        } else if (c == b'J' || c == b'j')
            && i + 5 < b.len()
            && b[i..i + 5].eq_ignore_ascii_case(b"jsonb")
        {
            let mut p = skip_ascii_ws(b, i + 5);
            if p >= b.len() || b[p] != b'(' {
                return None;
            }
            p = skip_ascii_ws(b, p + 1);
            p = parse_sql_string_into(sql, p, slot)?;
            p = skip_ascii_ws(b, p);
            if p >= b.len() || b[p] != b')' {
                return None;
            }
            shapes_out.push(LitArgShape::JsonbCall);
            i = p + 1;
        } else if c == b'-' || c == b'+' || c.is_ascii_digit() || c == b'.' {
            let num_start = i;
            if c == b'-' || c == b'+' {
                i += 1;
            }
            let mut has_digits = false;
            let mut is_float = false;
            if i + 1 < b.len() && b[i] == b'0' && (b[i + 1] == b'x' || b[i + 1] == b'X') {
                return None;
            }
            while i < b.len() && b[i].is_ascii_digit() {
                has_digits = true;
                i += 1;
            }
            if i < b.len() && b[i] == b'.' {
                is_float = true;
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    has_digits = true;
                    i += 1;
                }
            }
            if !has_digits {
                return None;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                is_float = true;
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                let exp_start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == exp_start {
                    return None;
                }
            }
            let num_str = &sql[num_start..i];
            if !is_float {
                if let Ok(iv) = num_str.parse::<i64>() {
                    *slot = Value::Integer(iv);
                } else {
                    let fv = num_str.parse::<f64>().ok()?;
                    if !fv.is_finite() {
                        return None;
                    }
                    *slot = Value::real(fv);
                }
            } else {
                let fv = num_str.parse::<f64>().ok()?;
                if !fv.is_finite() {
                    return None;
                }
                *slot = Value::real(fv);
            }
            shapes_out.push(LitArgShape::Param);
        } else {
            return None;
        }

        arg_idx += 1;
        i = skip_ascii_ws(b, i);
        if i >= b.len() {
            return None;
        }
        if b[i] == b',' {
            i += 1;
            continue;
        }
        if b[i] == b')' {
            i += 1;
            break;
        }
        return None;
    }

    if arg_idx == 0 {
        return None;
    }
    i = skip_ascii_ws(b, i);
    if i == b.len() {
        Some((table_name, arg_idx, ""))
    } else if b[i] == b';' {
        Some((table_name, arg_idx, &sql[i + 1..]))
    } else {
        None
    }
}

impl Statement {
    fn prepare_in_session(
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
        sql: &str,
        mode: PrepareMode,
    ) -> Result<Self> {
        Self::prepare_in_session_opts(shared, inner, sql, mode, true).map(|(st, _)| st)
    }

    fn prepare_in_session_opts(
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
        sql: &str,
        mode: PrepareMode,
        keep_tail: bool,
    ) -> Result<(Self, usize)> {
        let (stmt_opt, tail) = parse_one(sql)?;
        let tail_len = tail.len();
        let consumed_len = sql.len().saturating_sub(tail_len);
        let raw_sql = sql[..consumed_len].to_owned();
        let unparsed_tail = if keep_tail && !tail.is_empty() {
            tail.to_owned()
        } else {
            String::new()
        };

        let needs_schema = stmt_opt.as_ref().is_some_and(stmt_loads_schema_on_prepare);
        if !inner.db_set.is_temp_open()
            && let Some(ref st) = stmt_opt
            && stmt_references_temp_catalog(st, &inner.db_set)
        {
            let enc = inner.effective_encoding();
            inner.db_set.ensure_temp_open(enc, inner.writable_schema)?;
        }
        if needs_schema {
            inner.refresh_open_tx_schemas()?;
            let any_unloaded = inner
                .db_set
                .slots()
                .iter()
                .any(|s| s.btree.is_some() && !s.schema.loaded);
            if any_unloaded {
                let _ = inner.ensure_schemas_loaded_for_prepare(false)?;
            }
        }

        let try_compile =
            |inner: &mut ConnectionInner, stmt_opt: Option<Stmt>| -> Result<CompiledArtifacts> {
                if let Some(ref st) = stmt_opt {
                    inner.with_default_exec_ctx(shared, ActivitySummary::default(), |ctx| {
                        crate::vtab::ensure_stmt_vtabs_connected(ctx, st)
                    })?;
                }
                compile_parsed_stmt(
                    stmt_opt,
                    &inner.db_set,
                    inner.writable_schema,
                    inner.foreign_keys,
                    inner.recursive_triggers,
                    inner.count_changes,
                    inner.runtime.enable_test_functions,
                    &inner.functions,
                    &inner.collations,
                    &mut inner.authorizer,
                )
            };

        let mut artifacts = match try_compile(inner, stmt_opt) {
            Ok(a) => a,
            Err(err)
                if needs_schema
                    && err.code == ErrorCode::Error
                    && inner.db_set.slots().iter().any(|s| {
                        s.btree
                            .as_ref()
                            .is_some_and(|bt| bt.transaction_state() == TransactionState::Open)
                    }) =>
            {
                // Cached schema might be stale due to an external writer: check on-disk cookies once!
                if inner.ensure_schemas_loaded_for_prepare(true)? {
                    let (reparsed_opt, _) = parse_one(&raw_sql)?;
                    try_compile(inner, reparsed_opt)?
                } else {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        };

        apply_prepare_auto_vacuum_pragma(inner, &mut artifacts);

        let hid = inner.register_handle(HandleKind::Statement);
        let params = if artifacts.max_param == 0 {
            Vec::new()
        } else {
            vec![Value::Null; artifacts.max_param]
        };
        Ok((
            Self {
                shared: Arc::clone(shared),
                handle_id: Some(hid),
                mode,
                raw_sql,
                stmt: artifacts.stmt,
                requires_schema: artifacts.requires_schema,
                category: artifacts.category,
                compiled_vdbe: artifacts.compiled_vdbe,
                compiled_fast_read: artifacts.compiled_fast_read,
                cached_read_vm: inner.pooled_read_vm.take(),
                write_vm_scratch: std::mem::take(&mut inner.pooled_write_scratch),
                compiled_schema_generation: inner.schema_generation,
                compiled_v1_expire_generation: inner.v1_expire_generation,
                compiled_advisory_expire_generation: inner.advisory_expire_generation,
                compiled_slot_cookies: inner.snapshot_slot_cookies(),
                expired: false,
                column_names: artifacts.column_names,
                params,
                named_params: artifacts.named_params,
                ignored_fk_parent_tables: artifacts.ignored_fk_parent_tables,
                unparsed_tail,
                lifecycle: StatementLifecycle::Ready,
            },
            tail_len,
        ))
    }

    /// Remaining uncompiled SQL text after the first semicolon (`pzTail` in `sqlite3_prepare_v2`).
    #[inline]
    pub fn tail(&self) -> &str {
        &self.unparsed_tail
    }

    /// Original SQL text compiled for this statement (`sqlite3_sql`).
    #[inline]
    pub fn sql(&self) -> &str {
        &self.raw_sql
    }

    /// Preparation mode (`V2` or `V1`).
    #[inline]
    pub fn prepare_mode(&self) -> PrepareMode {
        self.mode
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

    /// Number of result columns (`sqlite3_column_count`).
    #[inline]
    pub fn column_count(&self) -> usize {
        self.column_names.len()
    }

    /// Result column names (`sqlite3_column_name`).
    #[inline]
    pub fn column_names(&self) -> &[String] {
        &self.column_names
    }

    /// Declared column type string (`sqlite3_column_decltype`) for result column `col_idx`, if
    /// the column directly references a table or virtual table column.
    pub fn column_decltype(&self, col_idx: usize) -> Option<String> {
        let Some(Stmt::Select(ref sel)) = self.stmt else {
            return None;
        };
        if !sel.body.compounds.is_empty() {
            return None;
        }
        let SelectCore::Select {
            projection: ref columns,
            from: Some(ref fc),
            ..
        } = sel.body.head
        else {
            return None;
        };
        let guard = self.shared.inner.try_lock().ok()?;
        let db_set = &guard.db_set;

        struct SrcTable {
            alias: String,
            all_cols: Vec<(String, String, bool)>,
        }
        let load_tos = |tos: &sqliters_syntax::TableOrSubquery| -> Option<SrcTable> {
            let sqliters_syntax::TableOrSubquery::Table {
                ref name,
                ref alias,
                ..
            } = *tos
            else {
                return None;
            };
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if let Ok(Some((_, crate::db_set::ResolvedRelation::Table(tbl)))) =
                db_set.find_relation_in_set(name)
            {
                let cols = tbl
                    .columns
                    .iter()
                    .map(|c| (c.name.clone(), c.type_name.clone(), false))
                    .collect();
                return Some(SrcTable {
                    alias: eff,
                    all_cols: cols,
                });
            }
            if let Some((_, _, _, decl)) = db_set.find_connected_vtab_in_set(name, true) {
                let cols = decl
                    .table_schema
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (c.name.clone(), c.type_name.clone(), decl.is_hidden(i)))
                    .collect();
                return Some(SrcTable {
                    alias: eff,
                    all_cols: cols,
                });
            }
            None
        };

        let mut srcs = Vec::new();
        if let Some(h) = load_tos(&fc.head) {
            srcs.push(h);
        }
        for j in &fc.joins {
            if let Some(r) = load_tos(&j.right) {
                srcs.push(r);
            }
        }

        let mut decls: Vec<Option<String>> = Vec::new();
        for rc in columns {
            match rc {
                sqliters_syntax::ResultColumn::Star => {
                    for s in &srcs {
                        for (_, dt, hidden) in &s.all_cols {
                            if !*hidden {
                                decls.push(Some(dt.clone()));
                            }
                        }
                    }
                }
                sqliters_syntax::ResultColumn::TableStar { table: tname, .. } => {
                    for s in &srcs {
                        if s.alias.eq_ignore_ascii_case(tname) {
                            for (_, dt, hidden) in &s.all_cols {
                                if !*hidden {
                                    decls.push(Some(dt.clone()));
                                }
                            }
                        }
                    }
                }
                sqliters_syntax::ResultColumn::Expr { expr, .. } => {
                    if let sqliters_syntax::Expr::Column(cref) = expr {
                        let mut found = None;
                        for s in &srcs {
                            if cref
                                .table
                                .as_ref()
                                .is_some_and(|t| !s.alias.eq_ignore_ascii_case(t))
                            {
                                continue;
                            }
                            if let Some((_, dt, _)) = s
                                .all_cols
                                .iter()
                                .find(|(cn, _, _)| cn.eq_ignore_ascii_case(&cref.column))
                            {
                                found = Some(dt.clone());
                                break;
                            }
                        }
                        decls.push(found);
                    } else {
                        decls.push(None);
                    }
                }
            }
        }
        decls.into_iter().nth(col_idx).flatten()
    }

    /// Returns `true` if this statement is currently paused in `Running` (`SQLITE_ROW` / retryable `Busy`)
    /// (`sqlite3_stmt_busy`).
    #[inline]
    pub fn is_busy(&self) -> bool {
        matches!(self.lifecycle, StatementLifecycle::Running(_))
    }

    /// Marks this prepared statement as expired (`sqlite3ExpirePreparedStatements`), forcing V2
    /// statements to reprepare on the next [`Statement::step`] (`PrepareMode::V1` returns
    /// `ErrorCode::Schema`).
    #[inline]
    pub fn expire(&mut self) {
        self.expired = true;
    }

    fn ensure_ready_for_bind(&self) -> Result<()> {
        if !matches!(self.lifecycle, StatementLifecycle::Ready) {
            return Err(SqliteError::misuse(format!(
                "bind on a busy prepared statement: [{}]",
                self.raw_sql.trim()
            )));
        }
        Ok(())
    }

    fn bind_one_in_session(
        &mut self,
        inner: &ConnectionInner,
        index_1_based: usize,
        value: Value,
    ) -> Result<()> {
        inner.ensure_usable()?;
        self.ensure_ready_for_bind()?;
        if index_1_based == 0 || index_1_based > self.params.len() {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!("column/bind index {index_1_based} out of range"),
            ));
        }
        self.params[index_1_based - 1] = value;
        Ok(())
    }

    fn bind_all_in_session(&mut self, inner: &ConnectionInner, values: &[Value]) -> Result<()> {
        inner.ensure_usable()?;
        self.ensure_ready_for_bind()?;
        if values.len() > self.params.len() {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!(
                    "bind_all supplied {} values for {} parameters",
                    values.len(),
                    self.params.len()
                ),
            ));
        }
        for (i, v) in values.iter().enumerate() {
            self.params[i] = v.clone();
        }
        Ok(())
    }

    #[inline]
    fn can_fast_bind(&self) -> bool {
        self.handle_id.is_some()
            && self.shared.conn_state.load(Ordering::Acquire) == CONN_STATE_OPEN
            && self.shared.gate_state.load(Ordering::Acquire) == 0
    }

    /// Binds a 1-based parameter (`?1`, `?`, etc.) (`sqlite3_bind_*`).
    /// Requires the statement to be in `Ready` state (`ErrorCode::Misuse` if `Running` or `Halted` without `reset()`).
    #[inline]
    pub fn bind(&mut self, index_1_based: usize, value: impl Into<Value>) -> Result<&mut Self> {
        if self.can_fast_bind() {
            self.ensure_ready_for_bind()?;
            if index_1_based == 0 || index_1_based > self.params.len() {
                return Err(SqliteError::new(
                    ErrorCode::Range,
                    format!("column/bind index {index_1_based} out of range"),
                ));
            }
            self.params[index_1_based - 1] = value.into();
            return Ok(self);
        }
        let shared = Arc::clone(&self.shared);
        let session = shared.enter()?;
        self.bind_one_in_session(session.inner(), index_1_based, value.into())?;
        drop(session);
        Ok(self)
    }

    /// Binds a 1-based text parameter (`sqlite3_bind_text`) from a borrowed `&str`, reusing any
    /// existing `String` buffer in the parameter slot without locking the connection gate.
    #[inline]
    pub fn bind_text(&mut self, index_1_based: usize, text: &str) -> Result<&mut Self> {
        if self.can_fast_bind() {
            self.ensure_ready_for_bind()?;
            if index_1_based == 0 || index_1_based > self.params.len() {
                return Err(SqliteError::new(
                    ErrorCode::Range,
                    format!("column/bind index {index_1_based} out of range"),
                ));
            }
            let slot = &mut self.params[index_1_based - 1];
            if let Value::Text(s) = slot {
                s.clear();
                s.push_str(text);
            } else {
                *slot = Value::Text(text.to_owned());
            }
            return Ok(self);
        }
        self.bind(index_1_based, text)
    }

    /// Binds a named parameter (`:name`, `@name`, or `$name`).
    /// Requires the statement to be in `Ready` state (`ErrorCode::Misuse` if `Running` or `Halted` without `reset()`).
    #[inline]
    pub fn bind_named(&mut self, name: &str, value: impl Into<Value>) -> Result<&mut Self> {
        if self.can_fast_bind() {
            self.ensure_ready_for_bind()?;
            let Some(&idx) = self.named_params.get(name) else {
                return Err(SqliteError::new(
                    ErrorCode::Range,
                    format!("unknown bind parameter: {name}"),
                ));
            };
            self.params[idx - 1] = value.into();
            return Ok(self);
        }
        let shared = Arc::clone(&self.shared);
        let session = shared.enter()?;
        session.inner().ensure_usable()?;
        self.ensure_ready_for_bind()?;
        let Some(&idx) = self.named_params.get(name) else {
            return Err(SqliteError::new(
                ErrorCode::Range,
                format!("unknown bind parameter: {name}"),
            ));
        };
        self.params[idx - 1] = value.into();
        drop(session);
        Ok(self)
    }

    /// Binds a slice of positional parameters (`1..=values.len()`).
    /// Requires `Ready` state and `values.len() <= self.parameter_count()`.
    #[inline]
    pub fn bind_all(&mut self, values: &[Value]) -> Result<&mut Self> {
        if self.can_fast_bind() {
            self.ensure_ready_for_bind()?;
            if values.len() > self.params.len() {
                return Err(SqliteError::new(
                    ErrorCode::Range,
                    format!(
                        "bind_all supplied {} values for {} parameters",
                        values.len(),
                        self.params.len()
                    ),
                ));
            }
            for (i, v) in values.iter().enumerate() {
                self.params[i] = v.clone();
            }
            return Ok(self);
        }
        let shared = Arc::clone(&self.shared);
        let session = shared.enter()?;
        self.bind_all_in_session(session.inner(), values)?;
        drop(session);
        Ok(self)
    }

    /// Clears all bound parameters to `NULL` (`sqlite3_clear_bindings`).
    ///
    /// Matches C `vdbeapi.c:sqlite3_clear_bindings`: enters the connection gate, does **not**
    /// require `Ready` state (`Ok(())` in `Ready`, `Running`, and `Halted`), and does **not**
    /// reset execution position, close cursors, or end active `ActivityTracker` leases.
    pub fn clear_bindings(&mut self) -> Result<()> {
        if self.can_fast_bind() {
            self.params.fill(Value::Null);
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let session = shared.enter()?;
        session.inner().ensure_usable()?;
        self.params.fill(Value::Null);
        Ok(())
    }

    /// Resets execution back to `Ready` while preserving bound parameter values (`sqlite3_reset`).
    ///
    /// Surfaces any error recorded on the prior `step()` or encountered while halting/committing
    /// active execution (`sqlite3VdbeReset`), while leaving the statement rewound to `Ready`
    /// (unless cleanup I/O poisoned the connection).
    pub fn reset(&mut self) -> Result<()> {
        if matches!(
            self.lifecycle,
            StatementLifecycle::Ready | StatementLifecycle::Halted { last_error: None }
        ) && self.can_fast_bind()
        {
            self.lifecycle = StatementLifecycle::Ready;
            return Ok(());
        }
        let shared = Arc::clone(&self.shared);
        let mut session = shared.enter()?;
        self.reset_in_session(&shared, session.inner_mut())
    }

    fn reset_in_session(
        &mut self,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<()> {
        let Some(hid) = self.handle_id else {
            return Err(SqliteError::misuse("statement already finalized"));
        };
        if let ConnectionState::Poisoned(ref p_err) = inner.state {
            let p_err = p_err.clone();
            let prev = std::mem::replace(&mut self.lifecycle, StatementLifecycle::Ready);
            if let StatementLifecycle::Running(run) = prev {
                let _ = inner.finish_running_exec(hid, run, shared);
            }
            return Err(p_err);
        }
        inner.ensure_usable()?;

        let prev = std::mem::replace(&mut self.lifecycle, StatementLifecycle::Ready);
        match prev {
            StatementLifecycle::Ready => Ok(()),
            StatementLifecycle::Halted { last_error } => match last_error {
                Some(err) => Err(err),
                None => Ok(()),
            },
            StatementLifecycle::Running(RunningExec::Vdbe {
                mut vm,
                read_instances,
            }) => {
                vm.close_cursors(&mut inner.db_set);
                self.cached_read_vm = Some(vm);
                inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                        read_instances,
                    },
                    shared,
                )
            }
            StatementLifecycle::Running(run) => inner.finish_running_exec(hid, run, shared),
        }
    }

    /// Explicitly finalizes and consumes this [`Statement`] (`sqlite3_finalize`), finishing any
    /// active execution, unregistering the handle from the connection **once** (`Drop` becomes a
    /// no-op), and returning any prior execution or halt/cleanup error.
    pub fn finalize(mut self) -> Result<()> {
        let shared = Arc::clone(&self.shared);
        let mut session = shared.enter()?;
        self.finalize_in_session(&shared, session.inner_mut())
    }

    fn finalize_in_session(
        &mut self,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<()> {
        let Some(hid) = self.handle_id.take() else {
            return Ok(());
        };
        let prev = std::mem::replace(&mut self.lifecycle, StatementLifecycle::Ready);
        let res = match prev {
            StatementLifecycle::Ready => {
                if let ConnectionState::Poisoned(ref p_err) = inner.state {
                    Err(p_err.clone())
                } else {
                    Ok(())
                }
            }
            StatementLifecycle::Halted { last_error } => match last_error {
                Some(err) => Err(err),
                None => {
                    if let ConnectionState::Poisoned(ref p_err) = inner.state {
                        Err(p_err.clone())
                    } else {
                        Ok(())
                    }
                }
            },
            StatementLifecycle::Running(run) => inner.finish_running_exec(hid, run, shared),
        };
        if inner.pooled_read_vm.is_none() {
            inner.pooled_read_vm = self.cached_read_vm.take();
        }
        if inner.pooled_write_scratch.is_empty_capacity()
            && !self.write_vm_scratch.is_empty_capacity()
        {
            inner.pooled_write_scratch = std::mem::take(&mut self.write_vm_scratch);
        }
        inner.open_handles.remove(&hid);
        res
    }

    fn has_detached_slot(&self, inner: &ConnectionInner) -> bool {
        self.compiled_slot_cookies.iter().any(|&(inst, _, _)| {
            !inner
                .db_set
                .slots()
                .iter()
                .any(|s| s.schema_instance_id == inst && s.btree.is_some())
        })
    }

    #[inline]
    fn slot_cookies_changed(&self, inner: &ConnectionInner) -> bool {
        if let [(inst, cookie, had_pages)] = *self.compiled_slot_cookies.as_slice() {
            let main_slot = inner.db_set.main_slot();
            if main_slot.schema_instance_id == inst && main_slot.btree.is_some() {
                return main_slot.schema.schema_cookie != cookie
                    || main_slot.schema.had_pages != had_pages;
            }
        }
        slot_cookies_changed_slice(&self.compiled_slot_cookies, inner)
    }

    fn needs_reprepare(&self, inner: &ConnectionInner) -> bool {
        if self.expired
            || self.compiled_advisory_expire_generation != inner.advisory_expire_generation
        {
            return true;
        }
        if self.category != Some(StmtCategory::Control) && self.has_detached_slot(inner) {
            return true;
        }
        if !self.requires_schema {
            return false;
        }
        match self.mode {
            PrepareMode::V1 => {
                self.compiled_v1_expire_generation != inner.v1_expire_generation
                    || self.slot_cookies_changed(inner)
            }
            PrepareMode::V2 => {
                self.compiled_schema_generation != inner.schema_generation
                    || self.compiled_v1_expire_generation != inner.v1_expire_generation
                    || self.slot_cookies_changed(inner)
            }
        }
    }

    fn reprepare_in_open_tx(&mut self, inner: &mut ConnectionInner) -> Result<()> {
        for _ in 0..SQLITE_MAX_SCHEMA_RETRY {
            let parsed_stmt = parse_one(&self.raw_sql).ok().and_then(|(st, _)| st);
            if !inner.db_set.is_temp_open()
                && let Some(ref st) = parsed_stmt
                && stmt_references_temp_catalog(st, &inner.db_set)
            {
                let enc = inner.effective_encoding();
                inner.db_set.ensure_temp_open(enc, inner.writable_schema)?;
            }
            inner.refresh_open_tx_schemas()?;
            if parsed_stmt
                .as_ref()
                .is_some_and(stmt_loads_schema_on_prepare)
            {
                let any_unloaded = inner
                    .db_set
                    .slots()
                    .iter()
                    .any(|s| s.btree.is_some() && !s.schema.loaded);
                if any_unloaded {
                    let _ = inner.ensure_schemas_loaded_for_prepare(false)?;
                }
            }
            let shared = Arc::clone(&self.shared);
            let try_compile = |inner: &mut ConnectionInner,
                               parsed_stmt: &Option<Stmt>|
             -> Result<CompiledArtifacts> {
                if let Some(st) = parsed_stmt {
                    inner.with_default_exec_ctx(&shared, ActivitySummary::default(), |ctx| {
                        crate::vtab::ensure_stmt_vtabs_connected(ctx, st)
                    })?;
                }
                compile_first_sql(
                    &self.raw_sql,
                    &inner.db_set,
                    inner.writable_schema,
                    inner.foreign_keys,
                    inner.recursive_triggers,
                    inner.count_changes,
                    inner.runtime.enable_test_functions,
                    &inner.functions,
                    &inner.collations,
                    &mut inner.authorizer,
                )
            };
            let mut artifacts = match try_compile(inner, &parsed_stmt) {
                Ok(a) => a,
                Err(err)
                    if err.code == ErrorCode::Error
                        && inner.db_set.slots().iter().any(|s| {
                            s.btree
                                .as_ref()
                                .is_some_and(|bt| bt.transaction_state() == TransactionState::Open)
                        }) =>
                {
                    if inner.ensure_schemas_loaded_for_prepare(true)? {
                        try_compile(inner, &parsed_stmt)?
                    } else {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            };
            apply_prepare_auto_vacuum_pragma(inner, &mut artifacts);
            let mut transferred = vec![Value::Null; artifacts.max_param];
            let copy_len = self.params.len().min(artifacts.max_param);
            transferred[..copy_len].clone_from_slice(&self.params[..copy_len]);

            self.stmt = artifacts.stmt;
            self.requires_schema = artifacts.requires_schema;
            self.category = artifacts.category;
            self.compiled_vdbe = artifacts.compiled_vdbe;
            self.compiled_fast_read = artifacts.compiled_fast_read;
            self.column_names = artifacts.column_names;
            self.params = transferred;
            self.named_params = artifacts.named_params;
            self.ignored_fk_parent_tables = artifacts.ignored_fk_parent_tables;
            self.compiled_schema_generation = inner.schema_generation;
            self.compiled_v1_expire_generation = inner.v1_expire_generation;
            self.compiled_advisory_expire_generation = inner.advisory_expire_generation;
            self.compiled_slot_cookies = inner.snapshot_slot_cookies();
            self.expired = false;
            if !self.needs_reprepare(inner) {
                return Ok(());
            }
        }
        Err(SqliteError::new(
            ErrorCode::Schema,
            "database schema has changed",
        ))
    }

    /// Advances statement execution by one step (`sqlite3_step`).
    ///
    /// Implements C SQLite default **autoreset** (`SQLITE_OMIT_AUTORESET` off): calling `step()`
    /// on a statement in `Halted` (`Done` or post-error) automatically rewinds under the held
    /// connection lease, preserves bound parameters, and starts a fresh execution.
    pub fn step(&mut self) -> Result<StepResult> {
        // Zero-`Arc::clone` fast completion for step #2 of a single-row `rowid` point `SELECT`.
        if let StatementLifecycle::Running(RunningExec::PointSelectRowYielded { main_inst }) =
            self.lifecycle
            && let Some(hid) = self.handle_id
        {
            let fin_res = {
                let mut session = self.shared.enter()?;
                let inner = session.inner_mut();
                inner.ensure_usable()?;
                inner.runtime.stmt_timestamp_ms = None;
                inner.finish_point_select_main_read(hid, main_inst, &self.shared)
            };
            self.lifecycle = StatementLifecycle::Halted {
                last_error: fin_res.as_ref().err().cloned(),
            };
            fin_res?;
            return Ok(StepResult::Done);
        }

        // Zero-`Arc::clone` fast advance for already-running multi-row queries (`RunningExec`).
        if matches!(self.lifecycle, StatementLifecycle::Running(_))
            && let Some(hid) = self.handle_id
        {
            let Self {
                ref shared,
                ref mut lifecycle,
                ref mut cached_read_vm,
                ..
            } = *self;
            let mut session = shared.enter()?;
            let inner = session.inner_mut();
            inner.ensure_usable()?;
            return advance_running_fields(hid, shared, inner, lifecycle, cached_read_vm);
        }

        // Zero-`Arc::clone` / zero-`VdbeVm` / zero-`BtreeCursor` fast path for step #1 of
        // `SELECT <cols> FROM main.<tbl> WHERE <rowid/ipk> = ?`.
        if let Some(hid) = self.handle_id
            && matches!(
                self.lifecycle,
                StatementLifecycle::Ready | StatementLifecycle::Halted { .. }
            )
            && self.category == Some(StmtCategory::Read)
            && !self.expired
            && let Some(ref prog) = self.compiled_vdbe
            && prog.referenced_db_mask() == 1u16
            && let Some(plan) = prog.rowid_point_select()
            && plan.db == DatabaseId::MAIN
        {
            #[allow(clippy::large_enum_variant)]
            enum PointOutcome {
                Fallback,
                EarlyErr(SqliteError),
                HaltedErr(SqliteError),
                Hit(PointLookupRow, SchemaInstanceId),
                Miss,
            }
            let outcome = {
                let mut session = self.shared.enter()?;
                let inner = session.inner_mut();
                if let Err(e) = inner.ensure_usable() {
                    PointOutcome::EarlyErr(e)
                } else if self.compiled_advisory_expire_generation
                    != inner.advisory_expire_generation
                    || self.compiled_schema_generation != inner.schema_generation
                    || self.compiled_v1_expire_generation != inner.v1_expire_generation
                    || inner.trace_hook.is_some()
                    || inner.authorizer.is_some()
                    || inner.progress.is_enabled()
                    || inner.simulated_cleanup_io_error.is_some()
                    || inner.db_set.vtab_registry().has_any_in_tx()
                {
                    PointOutcome::Fallback
                } else {
                    let main_slot = inner.db_set.main_slot();
                    if !main_slot.schema.loaded || main_slot.btree.is_none() {
                        PointOutcome::Fallback
                    } else {
                        let main_inst = main_slot.schema_instance_id;
                        let main_cookie = main_slot.schema.schema_cookie;
                        let main_had_pages = main_slot.schema.had_pages;
                        let cookie_mismatch = if inner.db_set.main_btree().transaction_state()
                            != TransactionState::Open
                        {
                            let bt = inner.db_set.main_btree();
                            (bt.pager().page_count() > 0) != main_had_pages
                                || bt.pager().header().schema_cookie != main_cookie
                        } else {
                            false
                        };
                        if cookie_mismatch || self.slot_cookies_changed(inner) {
                            PointOutcome::Fallback
                        } else {
                            let started_here = inner.db_set.main_btree().transaction_state()
                                == TransactionState::Open;
                            let mut begin_err = None;
                            let mut stale_after_begin = false;
                            if started_here {
                                let res = {
                                    let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
                                    inner.busy_policy.retry_btree(bt, |b| b.begin_read())
                                };
                                if let Err(e) = res {
                                    begin_err = Some(e);
                                } else {
                                    let bt = inner.db_set.main_btree();
                                    if (bt.pager().page_count() > 0) != main_had_pages
                                        || bt.pager().header().schema_cookie != main_cookie
                                    {
                                        stale_after_begin = true;
                                        if !inner.in_user_btree_tx && inner.is_autocommit() {
                                            let sum = inner
                                                .activity
                                                .summary_excluding(hid, &inner.db_set);
                                            if (sum.other_read_db_mask & 1) == 0
                                                && (sum.other_write_db_mask & 1) == 0
                                                && sum.other_writers == 0
                                                && let Some(ref mut bt_mut) =
                                                    inner.db_set.slots_mut()[0].btree
                                                && bt_mut.transaction_state()
                                                    == TransactionState::Read
                                            {
                                                let _ = bt_mut.pager_mut().end_read_if_unused();
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(e) = begin_err {
                                PointOutcome::HaltedErr(e)
                            } else if stale_after_begin {
                                PointOutcome::Fallback
                            } else {
                                let rowid_opt = match plan.key {
                                    RowidKeySource::ConstRowid(r) => Some(r),
                                    RowidKeySource::Param1Based(p) => self
                                        .params
                                        .get((p as usize) - 1)
                                        .and_then(Value::to_exact_rowid_i64),
                                };
                                let lookup_res = match inner.check_any_interrupted() {
                                    Err(e) => Err(e),
                                    Ok(()) => match rowid_opt {
                                        None => Ok(None),
                                        Some(rowid) => inner.db_set.slots_mut()[0]
                                            .btree
                                            .as_mut()
                                            .unwrap()
                                            .point_lookup_table_columns_row(
                                                plan.root_pgno,
                                                rowid,
                                                &plan.cols,
                                                plan.max_col_idx,
                                            ),
                                    },
                                };

                                match lookup_res {
                                    Ok(Some(vals)) => {
                                        if !inner.is_autocommit() {
                                            inner.in_user_btree_tx = true;
                                        }
                                        inner.activity.start_single_read(hid, main_inst);
                                        PointOutcome::Hit(vals, main_inst)
                                    }
                                    Ok(None) => {
                                        if !inner.is_autocommit() {
                                            inner.in_user_btree_tx = true;
                                            PointOutcome::Miss
                                        } else if inner.in_user_btree_tx {
                                            PointOutcome::Miss
                                        } else {
                                            let sum = inner
                                                .activity
                                                .summary_excluding(hid, &inner.db_set);
                                            if (sum.other_read_db_mask & 1) == 0
                                                && (sum.other_write_db_mask & 1) == 0
                                                && sum.other_writers == 0
                                                && let Some(ref mut bt) =
                                                    inner.db_set.slots_mut()[0].btree
                                                && bt.transaction_state() == TransactionState::Read
                                                && let Err(e) = bt.pager_mut().end_read_if_unused()
                                            {
                                                PointOutcome::HaltedErr(inner.poison_with(e))
                                            } else {
                                                PointOutcome::Miss
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        if !inner.in_user_btree_tx && inner.is_autocommit() {
                                            let sum = inner
                                                .activity
                                                .summary_excluding(hid, &inner.db_set);
                                            if (sum.other_read_db_mask & 1) == 0
                                                && (sum.other_write_db_mask & 1) == 0
                                                && sum.other_writers == 0
                                                && let Some(ref mut bt) =
                                                    inner.db_set.slots_mut()[0].btree
                                                && bt.transaction_state() == TransactionState::Read
                                            {
                                                let _ = bt.pager_mut().end_read_if_unused();
                                            }
                                        }
                                        PointOutcome::HaltedErr(e)
                                    }
                                }
                            }
                        }
                    }
                }
            };

            match outcome {
                PointOutcome::Fallback => {}
                PointOutcome::EarlyErr(e) => return Err(e),
                PointOutcome::HaltedErr(e) => {
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }
                PointOutcome::Hit(lookup_row, main_inst) => {
                    self.lifecycle =
                        StatementLifecycle::Running(RunningExec::PointSelectRowYielded {
                            main_inst,
                        });
                    return Ok(StepResult::Row(Row::from_lookup_row(lookup_row)));
                }
                PointOutcome::Miss => {
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
                    return Ok(StepResult::Done);
                }
            }
        }

        // Zero-`Arc::clone` fast path for compiled single-DB `main` writes (`INSERT`/`UPDATE`/`DELETE`).
        if let Some(hid) = self.handle_id
            && matches!(
                self.lifecycle,
                StatementLifecycle::Ready | StatementLifecycle::Halted { .. }
            )
            && self.category == Some(StmtCategory::Write)
            && !self.expired
            && self.compiled_vdbe.is_some()
        {
            let fast_write = {
                let Self {
                    ref shared,
                    category,
                    expired,
                    compiled_advisory_expire_generation,
                    compiled_schema_generation,
                    compiled_v1_expire_generation,
                    ref compiled_slot_cookies,
                    ref compiled_vdbe,
                    ref params,
                    ref mut lifecycle,
                    ref mut write_vm_scratch,
                    ..
                } = *self;
                let mut session = shared.enter()?;
                let inner = session.inner_mut();
                if let Err(e) = inner.ensure_usable() {
                    Some(Err(e))
                } else {
                    try_fast_compiled_main_write_fields(
                        hid,
                        shared,
                        inner,
                        category,
                        expired,
                        compiled_advisory_expire_generation,
                        compiled_schema_generation,
                        compiled_v1_expire_generation,
                        compiled_slot_cookies,
                        compiled_vdbe.as_deref(),
                        params,
                        lifecycle,
                        write_vm_scratch,
                    )
                }
            };
            if let Some(res) = fast_write {
                return res;
            }
        }

        // Zero-`Arc::clone` fast path for pre-compiled `FastReadPlan` on `main`.
        if let Some(hid) = self.handle_id
            && matches!(
                self.lifecycle,
                StatementLifecycle::Ready | StatementLifecycle::Halted { .. }
            )
            && self.category == Some(StmtCategory::Read)
            && !self.expired
            && self.compiled_fast_read.is_some()
        {
            let fast_read_res = {
                let Self {
                    ref shared,
                    category,
                    expired,
                    compiled_advisory_expire_generation,
                    compiled_schema_generation,
                    compiled_v1_expire_generation,
                    ref compiled_slot_cookies,
                    ref compiled_fast_read,
                    ref params,
                    ref mut lifecycle,
                    ..
                } = *self;
                let mut session = shared.enter()?;
                let inner = session.inner_mut();
                if let Err(e) = inner.ensure_usable() {
                    Some(Err(e))
                } else if inner.trace_hook.is_some() || inner.authorizer.is_some() {
                    None
                } else {
                    try_fast_read_plan_fields(
                        hid,
                        shared,
                        inner,
                        category,
                        expired,
                        compiled_advisory_expire_generation,
                        compiled_schema_generation,
                        compiled_v1_expire_generation,
                        compiled_slot_cookies,
                        compiled_fast_read.as_deref(),
                        params,
                        lifecycle,
                    )
                }
            };
            if let Some(res) = fast_read_res {
                return res;
            }
        }

        let shared = Arc::clone(&self.shared);
        let mut session = shared.enter()?;
        self.step_in_session(&shared, session.inner_mut())
    }

    fn step_in_session(
        &mut self,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<StepResult> {
        inner.ensure_usable()?;
        let Some(hid) = self.handle_id else {
            return Err(SqliteError::misuse("statement already finalized"));
        };

        // Autoreset if previously `Halted` (`Done` or post-error): internal rewind preserving `params`!
        if matches!(self.lifecycle, StatementLifecycle::Halted { .. }) {
            self.lifecycle = StatementLifecycle::Ready;
        }

        // Empty / comment-only SQL (`stmt.is_none()` with no parsed statement)
        if self.stmt.is_none() && matches!(parse_one(&self.raw_sql), Ok((None, _))) {
            self.lifecycle = StatementLifecycle::Halted { last_error: None };
            return Ok(StepResult::Done);
        }

        if matches!(self.lifecycle, StatementLifecycle::Ready) {
            if let Some(ref mut cb) = inner.trace_hook
                && let Err(e) =
                    execution_context::catch_infallible_callback_panic("trace_hook", || {
                        cb(&self.raw_sql)
                    })
            {
                self.lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Err(e);
            }
            if self.stmt.is_none()
                && !self.expired
                && self.compiled_advisory_expire_generation == inner.advisory_expire_generation
            {
                self.lifecycle = StatementLifecycle::Halted { last_error: None };
                return Ok(StepResult::Done);
            }
            return self.start_execution_in_session(hid, shared, inner);
        }

        // Advance already-running execution (`StatementLifecycle::Running`)
        self.advance_running_in_session(hid, shared, inner)
    }

    fn try_fast_compiled_main_write(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Option<Result<StepResult>> {
        try_fast_compiled_main_write_fields(
            hid,
            shared,
            inner,
            self.category,
            self.expired,
            self.compiled_advisory_expire_generation,
            self.compiled_schema_generation,
            self.compiled_v1_expire_generation,
            &self.compiled_slot_cookies,
            self.compiled_vdbe.as_deref(),
            &self.params,
            &mut self.lifecycle,
            &mut self.write_vm_scratch,
        )
    }

    fn try_fast_read_plan(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Option<Result<StepResult>> {
        try_fast_read_plan_fields(
            hid,
            shared,
            inner,
            self.category,
            self.expired,
            self.compiled_advisory_expire_generation,
            self.compiled_schema_generation,
            self.compiled_v1_expire_generation,
            &self.compiled_slot_cookies,
            self.compiled_fast_read.as_deref(),
            &self.params,
            &mut self.lifecycle,
        )
    }

    fn try_fast_compiled_main_read(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Option<Result<StepResult>> {
        if self.category != Some(StmtCategory::Read)
            || self.expired
            || self.compiled_advisory_expire_generation != inner.advisory_expire_generation
            || self.compiled_schema_generation != inner.schema_generation
            || self.compiled_v1_expire_generation != inner.v1_expire_generation
            || inner.db_set.vtab_registry().has_any_in_tx()
        {
            return None;
        }
        let prog = self.compiled_vdbe.as_ref()?;
        if prog.referenced_db_mask() != 1u16 {
            return None;
        }
        let main_slot = inner.db_set.main_slot();
        if !main_slot.schema.loaded || main_slot.btree.is_none() {
            return None;
        }
        let main_inst = main_slot.schema_instance_id;
        let main_cookie = main_slot.schema.schema_cookie;
        let main_had_pages = main_slot.schema.had_pages;
        if inner.db_set.main_btree().transaction_state() != TransactionState::Open {
            let bt = inner.db_set.main_btree();
            let has_pages = bt.pager().page_count() > 0;
            let cur_cookie = bt.pager().header().schema_cookie;
            if main_had_pages != has_pages || main_cookie != cur_cookie {
                return None;
            }
        }
        if self.slot_cookies_changed(inner) {
            return None;
        }

        let started_here = inner.db_set.main_btree().transaction_state() == TransactionState::Open;
        if started_here {
            let res = {
                let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
                inner.busy_policy.retry_btree(bt, |b| b.begin_read())
            };
            if let Err(e) = res {
                self.lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Some(Err(e));
            }
            let bt = inner.db_set.main_btree();
            let has_pages = bt.pager().page_count() > 0;
            let cur_cookie = bt.pager().header().schema_cookie;
            if has_pages != main_had_pages || cur_cookie != main_cookie {
                if !inner.in_user_btree_tx && inner.is_autocommit() {
                    let sum = inner.activity.summary_excluding(hid, &inner.db_set);
                    if (sum.other_read_db_mask & 1) == 0
                        && (sum.other_write_db_mask & 1) == 0
                        && sum.other_writers == 0
                        && let Some(ref mut bt_mut) = inner.db_set.slots_mut()[0].btree
                        && bt_mut.transaction_state() == TransactionState::Read
                    {
                        let _ = bt_mut.pager_mut().end_read_if_unused();
                    }
                }
                return None;
            }
        }

        if !inner.is_autocommit() {
            inner.in_user_btree_tx = true;
        }
        let read_instances = SmallInstVec::single(main_inst);
        inner.activity.start_with_instances(
            hid,
            true,
            false,
            read_instances.clone(),
            SmallInstVec::new(),
        );
        let mut vm_rt = inner.runtime;
        if vm_rt.stmt_timestamp_ms.is_none() && prog.needs_stmt_timestamp() {
            vm_rt.stmt_timestamp_ms = Some(
                inner
                    .db_set
                    .main_btree()
                    .pager()
                    .vfs()
                    .current_unix_millis(),
            );
        }
        let vm = if let Some(mut reused) = self.cached_read_vm.take() {
            reused.prepare_for_reuse(prog, &self.params, vm_rt);
            reused
        } else {
            VdbeVm::from_shared(Arc::clone(prog), &self.params, vm_rt)
        };
        self.lifecycle = StatementLifecycle::Running(RunningExec::Vdbe { vm, read_instances });
        Some(self.advance_running_in_session(hid, shared, inner))
    }

    fn start_execution_in_session(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<StepResult> {
        if let Some(fast_res) = self.try_fast_compiled_main_write(hid, shared, inner) {
            return fast_res;
        }
        if let Some(fast_res) = self.try_fast_read_plan(hid, shared, inner) {
            return fast_res;
        }
        if let Some(fast_res) = self.try_fast_compiled_main_read(hid, shared, inner) {
            return fast_res;
        }
        let is_advisory_expired = self.expired
            || self.compiled_advisory_expire_generation != inner.advisory_expire_generation;
        if self.category != Some(StmtCategory::Control)
            && let Err(e) = inner.refresh_open_tx_schemas()
        {
            self.lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            return Err(e);
        }

        if is_advisory_expired
            || (self.category != Some(StmtCategory::Control) && self.needs_reprepare(inner))
        {
            if self.mode == PrepareMode::V1 {
                let err = SqliteError::new(ErrorCode::Schema, "database schema has changed");
                self.lifecycle = StatementLifecycle::Halted {
                    last_error: Some(err.clone()),
                };
                return Err(err);
            }
            if let Err(e) = self.reprepare_in_open_tx(inner) {
                self.lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Err(e);
            }
            if let Some(fast_res) = self.try_fast_read_plan(hid, shared, inner) {
                return fast_res;
            }
        }

        let Some(category) = self.category else {
            self.lifecycle = StatementLifecycle::Halted { last_error: None };
            return Ok(StepResult::Done);
        };

        match category {
            StmtCategory::Control => {
                let stmt_clone = self.stmt.clone().expect("non-empty statement");
                inner.activity.start(hid, false, false);
                let summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let mut expire_self_only = false;
                let mut stmt_tx = StmtTxTracker::default();
                let res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| execute_control_stmt(ctx, &stmt_clone),
                );
                match res {
                    Ok(()) => {
                        inner.activity.remove(hid);
                        inner.maybe_run_wal_autocheckpoint();
                        self.lifecycle = StatementLifecycle::Halted { last_error: None };
                        Ok(StepResult::Done)
                    }
                    Err(e)
                        if e.code == ErrorCode::Busy
                            && summary.other_writers == 0
                            && matches!(stmt_clone, Stmt::Commit | Stmt::Release(_)) =>
                    {
                        let rel_sp = match stmt_clone {
                            Stmt::Release(name) => Some(name),
                            _ => None,
                        };
                        self.lifecycle = StatementLifecycle::Running(RunningExec::PendingCommit {
                            release_savepoint: rel_sp,
                            last_busy: e.clone(),
                        });
                        Err(e)
                    }
                    Err(e) => {
                        inner.activity.remove(hid);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                }
            }

            StmtCategory::NoLockPragma => {
                let stmt_clone = self.stmt.clone().expect("non-empty statement");
                inner.activity.start(hid, false, false);
                let summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                if prev_ts.is_none() {
                    inner.runtime.stmt_timestamp_ms = Some(
                        inner
                            .db_set
                            .main_btree()
                            .pager()
                            .vfs()
                            .current_unix_millis(),
                    );
                }
                let mut expire_self_only = false;
                let mut stmt_tx = StmtTxTracker::default();
                let res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| {
                        if let Stmt::Pragma {
                            ref name,
                            value: Some(_),
                            ..
                        } = stmt_clone
                            && name.eq_ignore_ascii_case("auto_vacuum")
                        {
                            // Prepare-time already handled `next_auto_vacuum` and `set_configured_auto_vacuum`;
                            // when classified as `NoLockPragma`, execution is a 0-op `SQLITE_DONE`!
                            Ok(QueryOutput::default())
                        } else {
                            execute_nolock_stmt(ctx, &stmt_clone, &self.params)
                        }
                    },
                );
                inner.runtime.stmt_timestamp_ms = prev_ts;
                inner.end_autocommit_started_reads(stmt_tx.started_read_mask, summary);
                if expire_self_only {
                    self.expired = true;
                }
                match res {
                    Ok(out) => {
                        if !out.column_names.is_empty() {
                            self.column_names = out.column_names;
                        }
                        let mut iter = out.rows.into_iter();
                        if let Some(first_row) = iter.next() {
                            self.lifecycle =
                                StatementLifecycle::Running(RunningExec::MaterializedNoLock {
                                    rows: iter,
                                });
                            Ok(StepResult::Row(Row::new(first_row)))
                        } else {
                            let _ = inner.finish_activity(hid, ActivityFinish::NoLock, shared);
                            inner.maybe_run_wal_autocheckpoint();
                            self.lifecycle = StatementLifecycle::Halted { last_error: None };
                            Ok(StepResult::Done)
                        }
                    }
                    Err(e) => {
                        let _ = inner.finish_activity(hid, ActivityFinish::NoLock, shared);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                }
            }

            StmtCategory::Vacuum => {
                let Some(Stmt::Vacuum {
                    schema: vac_schema,
                    into: vac_into,
                }) = self.stmt.clone()
                else {
                    unreachable!("StmtCategory::Vacuum requires Stmt::Vacuum");
                };
                inner.activity.start(hid, true, true);
                let summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let mut expire_self_only = false;
                let mut stmt_tx = StmtTxTracker::default();
                let res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| {
                        engine::exec_vacuum_checked(
                            ctx,
                            vac_schema.as_deref(),
                            vac_into.as_ref(),
                            &self.params,
                        )
                    },
                );
                inner.activity.remove(hid);
                if let Some(sim_err) = inner.take_simulated_cleanup_error() {
                    let p_err = inner.poison_with(sim_err);
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(p_err.clone()),
                    };
                    return Err(p_err);
                }
                match res {
                    Ok(_) => {
                        if self.compiled_schema_generation != inner.schema_generation
                            || self.compiled_v1_expire_generation != inner.v1_expire_generation
                        {
                            self.compiled_schema_generation = inner.schema_generation;
                            self.compiled_v1_expire_generation = inner.v1_expire_generation;
                            self.compiled_slot_cookies = inner.snapshot_slot_cookies();
                        }
                        inner.maybe_run_wal_autocheckpoint();
                        self.lifecycle = StatementLifecycle::Halted { last_error: None };
                        Ok(StepResult::Done)
                    }
                    Err(engine::VacuumFail::Clean(e)) => {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                    Err(engine::VacuumFail::Poison(e)) => {
                        let p_err = inner.poison_with(e);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(p_err.clone()),
                        };
                        Err(p_err)
                    }
                }
            }

            StmtCategory::Read => {
                let mut started_here = SmallInstVec::new();
                let mut read_instances = SmallInstVec::new();

                let cleanup_started_reads =
                    |inner: &mut ConnectionInner, started: &[SchemaInstanceId]| {
                        if !inner.in_user_btree_tx && inner.is_autocommit() {
                            let sum = inner.activity.summary_excluding(hid, &inner.db_set);
                            let active = sum.other_read_db_mask | sum.other_write_db_mask;
                            for &inst in started {
                                if let Some(db_id) = inner.db_set.find_db_id_by_instance(inst) {
                                    let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                                    if (active & bit) == 0
                                        && sum.other_writers == 0
                                        && let Some(slot) = inner.db_set.slot_mut(db_id)
                                        && let Some(ref mut bt) = slot.btree
                                        && bt.transaction_state() == TransactionState::Read
                                    {
                                        let _ = bt.pager_mut().end_read_if_unused();
                                    }
                                }
                            }
                        }
                    };

                // If any virtual table has buffered in-tx writes, flush them before a `Read`
                // statement executes so direct shadow-table queries or VDBE reads see fresh rows.
                if inner.db_set.vtab_registry().has_any_in_tx() {
                    let pre_sum = inner.activity.summary_excluding(hid, &inner.db_set);
                    if let Err(e) = inner.with_default_exec_ctx(shared, pre_sum, |ctx| {
                        crate::vtab::vtab_flush_shadow_all(ctx)
                    }) {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                }

                if let Some(ref prog) = self.compiled_vdbe {
                    let tx_db_mask = prog.referenced_db_mask();
                    let mut rem = tx_db_mask;
                    while rem != 0 {
                        let idx = rem.trailing_zeros() as usize;
                        rem &= rem - 1;
                        let db_id = DatabaseId(idx);
                        let Some(inst) = inner
                            .db_set
                            .slot(db_id)
                            .filter(|s| s.btree.is_some())
                            .map(|s| s.schema_instance_id)
                        else {
                            continue;
                        };
                        let is_open = inner
                            .db_set
                            .slot(db_id)
                            .unwrap()
                            .btree
                            .as_ref()
                            .unwrap()
                            .transaction_state()
                            == TransactionState::Open;
                        if is_open {
                            let res = {
                                let bt = inner
                                    .db_set
                                    .slot_mut(db_id)
                                    .unwrap()
                                    .btree
                                    .as_mut()
                                    .unwrap();
                                inner.busy_policy.retry_btree(bt, |b| b.begin_read())
                            };
                            if let Err(e) = res {
                                cleanup_started_reads(inner, &started_here);
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            push_unique_instance(&mut started_here, inst);
                        }
                        push_unique_instance(&mut read_instances, inst);
                    }

                    if !started_here.is_empty() && tx_db_mask != 0 && self.requires_schema {
                        if let Err(e) = inner.refresh_open_tx_schemas() {
                            cleanup_started_reads(inner, &started_here);
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                        if self.needs_reprepare(inner) {
                            if self.mode == PrepareMode::V1 {
                                cleanup_started_reads(inner, &started_here);
                                let err = SqliteError::new(
                                    ErrorCode::Schema,
                                    "database schema has changed",
                                );
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(err.clone()),
                                };
                                return Err(err);
                            }
                            if let Err(e) = self.reprepare_in_open_tx(inner) {
                                cleanup_started_reads(inner, &started_here);
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            if self.category != Some(StmtCategory::Read) {
                                cleanup_started_reads(inner, &started_here);
                                return self.start_execution_in_session(hid, shared, inner);
                            }
                            if let Some(ref prog2) = self.compiled_vdbe {
                                let mut rem2 = prog2.referenced_db_mask();
                                while rem2 != 0 {
                                    let idx2 = rem2.trailing_zeros() as usize;
                                    rem2 &= rem2 - 1;
                                    let db_id = DatabaseId(idx2);
                                    let Some(inst) = inner
                                        .db_set
                                        .slot(db_id)
                                        .filter(|s| s.btree.is_some())
                                        .map(|s| s.schema_instance_id)
                                    else {
                                        continue;
                                    };
                                    let is_open = inner
                                        .db_set
                                        .slot(db_id)
                                        .unwrap()
                                        .btree
                                        .as_ref()
                                        .unwrap()
                                        .transaction_state()
                                        == TransactionState::Open;
                                    if is_open {
                                        let res = {
                                            let bt = inner
                                                .db_set
                                                .slot_mut(db_id)
                                                .unwrap()
                                                .btree
                                                .as_mut()
                                                .unwrap();
                                            inner.busy_policy.retry_btree(bt, |b| b.begin_read())
                                        };
                                        if let Err(e) = res {
                                            cleanup_started_reads(inner, &started_here);
                                            self.lifecycle = StatementLifecycle::Halted {
                                                last_error: Some(e.clone()),
                                            };
                                            return Err(e);
                                        }
                                        push_unique_instance(&mut started_here, inst);
                                    }
                                    push_unique_instance(&mut read_instances, inst);
                                }
                            }
                        }
                    }

                    if let Some(ref prog_final) = self.compiled_vdbe {
                        if !inner.is_autocommit()
                            && (!read_instances.is_empty() || inner.db_set.any_in_transaction())
                        {
                            inner.in_user_btree_tx = true;
                        }
                        inner.activity.start_with_instances(
                            hid,
                            true,
                            false,
                            read_instances.clone(),
                            SmallInstVec::new(),
                        );
                        let mut vm_rt = inner.runtime;
                        if vm_rt.stmt_timestamp_ms.is_none() && prog_final.needs_stmt_timestamp() {
                            vm_rt.stmt_timestamp_ms = Some(
                                inner
                                    .db_set
                                    .main_btree()
                                    .pager()
                                    .vfs()
                                    .current_unix_millis(),
                            );
                        }
                        let vm = if let Some(mut reused) = self.cached_read_vm.take() {
                            reused.prepare_for_reuse(prog_final, &self.params, vm_rt);
                            reused
                        } else {
                            VdbeVm::from_shared(Arc::clone(prog_final), &self.params, vm_rt)
                        };
                        self.lifecycle =
                            StatementLifecycle::Running(RunningExec::Vdbe { vm, read_instances });
                        return self.advance_running_in_session(hid, shared, inner);
                    }
                }

                // Interpreter `Read` path (`SELECT`, `EXPLAIN`, or read `PRAGMA`):
                if let Err(e) = inner.check_any_interrupted() {
                    cleanup_started_reads(inner, &started_here);
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                if let Some(Stmt::Pragma {
                    schema: Some(ref s),
                    ..
                }) = self.stmt
                    && s.eq_ignore_ascii_case("temp")
                {
                    let enc = inner.effective_encoding();
                    if let Err(e) = inner.db_set.ensure_temp_open(enc, inner.writable_schema) {
                        cleanup_started_reads(inner, &started_here);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                }

                let mut specs = collect_select_referenced_roots(
                    self.stmt.as_ref().expect("non-empty statement"),
                    &inner.db_set,
                );
                for spec in &specs {
                    let db_id = match spec {
                        LeaseCursorSpec::Table(id, _) | LeaseCursorSpec::Index(id, _, _) => *id,
                    };
                    let Some(inst) = inner
                        .db_set
                        .slot(db_id)
                        .filter(|s| s.btree.is_some())
                        .map(|s| s.schema_instance_id)
                    else {
                        continue;
                    };
                    let is_open = inner
                        .db_set
                        .slot(db_id)
                        .unwrap()
                        .btree
                        .as_ref()
                        .unwrap()
                        .transaction_state()
                        == TransactionState::Open;
                    if is_open {
                        let res = {
                            let bt = inner
                                .db_set
                                .slot_mut(db_id)
                                .unwrap()
                                .btree
                                .as_mut()
                                .unwrap();
                            inner.busy_policy.retry_btree(bt, |b| b.begin_read())
                        };
                        if let Err(e) = res {
                            cleanup_started_reads(inner, &started_here);
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                        push_unique_instance(&mut started_here, inst);
                    }
                    push_unique_instance(&mut read_instances, inst);
                }

                if !read_instances.is_empty() && self.requires_schema {
                    if let Err(e) = inner.refresh_open_tx_schemas() {
                        cleanup_started_reads(inner, &started_here);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    if self.needs_reprepare(inner) {
                        if self.mode == PrepareMode::V1 {
                            cleanup_started_reads(inner, &started_here);
                            let err =
                                SqliteError::new(ErrorCode::Schema, "database schema has changed");
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(err.clone()),
                            };
                            return Err(err);
                        }
                        if let Err(e) = self.reprepare_in_open_tx(inner) {
                            cleanup_started_reads(inner, &started_here);
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                        if self.category != Some(StmtCategory::Read) {
                            cleanup_started_reads(inner, &started_here);
                            return self.start_execution_in_session(hid, shared, inner);
                        }
                        specs = collect_select_referenced_roots(
                            self.stmt.as_ref().expect("non-empty statement"),
                            &inner.db_set,
                        );
                        for spec in &specs {
                            let db_id = match spec {
                                LeaseCursorSpec::Table(id, _)
                                | LeaseCursorSpec::Index(id, _, _) => *id,
                            };
                            let Some(inst) = inner
                                .db_set
                                .slot(db_id)
                                .filter(|s| s.btree.is_some())
                                .map(|s| s.schema_instance_id)
                            else {
                                continue;
                            };
                            let is_open = inner
                                .db_set
                                .slot(db_id)
                                .unwrap()
                                .btree
                                .as_ref()
                                .unwrap()
                                .transaction_state()
                                == TransactionState::Open;
                            if is_open {
                                let res = {
                                    let bt = inner
                                        .db_set
                                        .slot_mut(db_id)
                                        .unwrap()
                                        .btree
                                        .as_mut()
                                        .unwrap();
                                    inner.busy_policy.retry_btree(bt, |b| b.begin_read())
                                };
                                if let Err(e) = res {
                                    cleanup_started_reads(inner, &started_here);
                                    self.lifecycle = StatementLifecycle::Halted {
                                        last_error: Some(e.clone()),
                                    };
                                    return Err(e);
                                }
                                push_unique_instance(&mut started_here, inst);
                            }
                            push_unique_instance(&mut read_instances, inst);
                        }
                    }
                }

                if !inner.is_autocommit()
                    && (!read_instances.is_empty() || inner.db_set.any_in_transaction())
                {
                    inner.in_user_btree_tx = true;
                }
                inner.activity.start_with_instances(
                    hid,
                    true,
                    false,
                    read_instances.clone(),
                    Vec::new(),
                );

                let mut lease_cursors: Vec<(SchemaInstanceId, CursorId)> =
                    Vec::with_capacity(specs.len());
                for spec in specs {
                    match spec {
                        LeaseCursorSpec::Table(db_id, root) => {
                            let Some(slot) = inner.db_set.slot_mut(db_id) else {
                                continue;
                            };
                            let inst = slot.schema_instance_id;
                            let Some(ref mut bt) = slot.btree else {
                                continue;
                            };
                            let page_cnt = bt.pager().page_count();
                            let cid = bt.open_table_cursor(root, false);
                            if page_cnt >= root
                                && let Err(e) = bt.cursor_first(cid)
                            {
                                lease_cursors.push((inst, cid));
                                let _ = inner.finish_activity(
                                    hid,
                                    ActivityFinish::ReadAborted {
                                        lease_cursors,
                                        read_instances,
                                    },
                                    shared,
                                );
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            lease_cursors.push((inst, cid));
                        }
                        LeaseCursorSpec::Index(db_id, root, orders) => {
                            let Some(slot) = inner.db_set.slot_mut(db_id) else {
                                continue;
                            };
                            let inst = slot.schema_instance_id;
                            let Some(ref mut bt) = slot.btree else {
                                continue;
                            };
                            let page_cnt = bt.pager().page_count();
                            let cid = bt.open_index_cursor(root, orders, false);
                            if page_cnt >= root
                                && let Err(e) = bt.cursor_first(cid)
                            {
                                lease_cursors.push((inst, cid));
                                let _ = inner.finish_activity(
                                    hid,
                                    ActivityFinish::ReadAborted {
                                        lease_cursors,
                                        read_instances,
                                    },
                                    shared,
                                );
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            lease_cursors.push((inst, cid));
                        }
                    }
                }

                let stmt_ref = self.stmt.as_ref().expect("non-empty statement");
                let summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                if prev_ts.is_none() {
                    inner.runtime.stmt_timestamp_ms = Some(
                        inner
                            .db_set
                            .main_btree()
                            .pager()
                            .vfs()
                            .current_unix_millis(),
                    );
                }
                let mut expire_self_only = false;
                let mut stmt_tx = StmtTxTracker::default();
                let mut step_inline_rows: Vec<InlineRow> = Vec::new();
                let exec_res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| {
                        crate::vtab::ensure_stmt_vtabs_connected(ctx, stmt_ref)?;
                        execute_read_body_for_step(
                            ctx,
                            stmt_ref,
                            &self.params,
                            &mut step_inline_rows,
                        )
                    },
                );
                inner.runtime.stmt_timestamp_ms = prev_ts;

                let extra_read = mask_to_instances(&inner.db_set, stmt_tx.read_db_mask);
                merge_instances(&mut read_instances, &extra_read);
                inner.activity.update_instances(hid, &read_instances, &[]);
                if !inner.is_autocommit()
                    && (!read_instances.is_empty() || inner.db_set.any_in_transaction())
                {
                    inner.in_user_btree_tx = true;
                }

                match exec_res {
                    Ok(out) => {
                        if !out.column_names.is_empty() {
                            self.column_names = out.column_names;
                        }
                        if !step_inline_rows.is_empty() {
                            let mut iter = step_inline_rows.into_iter();
                            let first_inline = iter.next().expect("non-empty step_inline_rows");
                            self.lifecycle =
                                StatementLifecycle::Running(RunningExec::MaterializedInlineRead {
                                    rows: iter,
                                    lease_cursors,
                                    read_instances,
                                });
                            return Ok(StepResult::Row(Row::from_inline(first_inline)));
                        }
                        let mut iter = out.rows.into_iter();
                        if let Some(first_row) = iter.next() {
                            self.lifecycle =
                                StatementLifecycle::Running(RunningExec::MaterializedRead {
                                    rows: iter,
                                    lease_cursors,
                                    read_instances,
                                });
                            Ok(StepResult::Row(Row::new(first_row)))
                        } else {
                            let fin_res = inner.finish_activity(
                                hid,
                                ActivityFinish::ReadDone {
                                    lease_cursors,
                                    read_instances,
                                },
                                shared,
                            );
                            if let Err(e) = fin_res {
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            self.lifecycle = StatementLifecycle::Halted { last_error: None };
                            Ok(StepResult::Done)
                        }
                    }
                    Err(e) => {
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors,
                                read_instances,
                            },
                            shared,
                        );
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                }
            }

            StmtCategory::IncrementalVacuum => {
                let Some(Stmt::Pragma {
                    ref schema,
                    ref value,
                    ..
                }) = self.stmt
                else {
                    unreachable!("StmtCategory::IncrementalVacuum requires Stmt::Pragma");
                };
                let db_id = match inner.db_set.resolve_schema_or_err(schema.as_deref()) {
                    Ok(id) => id,
                    Err(e) => {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                };
                if db_id == DatabaseId::TEMP {
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
                    return Ok(StepResult::Done);
                }
                let mut remaining = parse_incremental_vacuum_limit(value.as_ref());
                let Some(db_inst) = inner
                    .db_set
                    .slot(db_id)
                    .filter(|s| s.btree.is_some())
                    .map(|s| s.schema_instance_id)
                else {
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
                    return Ok(StepResult::Done);
                };

                let prev_tx_state = inner
                    .db_set
                    .slot(db_id)
                    .unwrap()
                    .btree
                    .as_ref()
                    .unwrap()
                    .transaction_state();
                let opened_write_here = prev_tx_state != TransactionState::Write;
                let base_sp_depth = inner.savepoints.len();
                if opened_write_here {
                    let res = {
                        let bt = inner
                            .db_set
                            .slot_mut(db_id)
                            .unwrap()
                            .btree
                            .as_mut()
                            .unwrap();
                        inner.busy_policy.retry_btree(bt, |b| {
                            b.pager_mut()
                                .begin_write_with_savepoint_depth(base_sp_depth)
                        })
                    };
                    if let Err(e) = res {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                } else if base_sp_depth > 0 {
                    let bt = inner
                        .db_set
                        .slot_mut(db_id)
                        .unwrap()
                        .btree
                        .as_mut()
                        .unwrap();
                    if let Err(e) = bt.pager_mut().ensure_subsavepoints(base_sp_depth) {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                }

                let pre_summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let db_bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                let cleanup_unstarted_write = |inner: &mut ConnectionInner| {
                    if opened_write_here
                        && (pre_summary.other_write_db_mask & db_bit) == 0
                        && pre_summary.other_writers == 0
                    {
                        let retain_read = prev_tx_state == TransactionState::Read
                            || (pre_summary.other_read_db_mask & db_bit) != 0;
                        if let Some(slot) = inner.db_set.slot_mut(db_id)
                            && let Some(ref mut bt) = slot.btree
                            && let Err(rb_err) = bt.rollback_with_trip(
                                extended::SQLITE_ABORT_ROLLBACK,
                                true,
                                retain_read,
                            )
                        {
                            let _ = inner.poison_with(rb_err);
                        }
                    }
                };

                let stmt_sp_slot = if !inner.is_autocommit() || pre_summary.other_writers > 0 {
                    let sp_idx = inner.savepoints.len();
                    let ensure_res = inner
                        .db_set
                        .slot_mut(db_id)
                        .unwrap()
                        .btree
                        .as_mut()
                        .unwrap()
                        .pager_mut()
                        .ensure_subsavepoints(sp_idx + 1);
                    if let Err(e) = ensure_res {
                        cleanup_unstarted_write(inner);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    Some(sp_idx)
                } else {
                    None
                };

                if !inner.is_autocommit() {
                    inner.in_user_btree_tx = true;
                }

                let abort_snapshot = WriteAbortSnapshot {
                    stmt_sp_slot,
                    touched_write_instances: SmallInstVec::single(db_inst),
                    started_read_instances: SmallInstVec::new(),
                    fk_before: inner.deferred_fk_suspects.clone(),
                    rt_before: inner.runtime,
                    is_dml: false,
                    mutation_len_before: inner.mutation_pipeline.len(),
                    schemas_before: inner.snapshot_open_schemas(),
                    schema_gen_before: inner.schema_generation,
                    v1_expire_gen_before: inner.v1_expire_generation,
                    schema_changed_before: inner.schema_changed_in_tx,
                };

                inner.activity.start_with_instances(
                    hid,
                    true,
                    true,
                    SmallInstVec::single(db_inst),
                    SmallInstVec::single(db_inst),
                );
                if let Err(e) = inner.check_any_interrupted() {
                    let final_err = inner.abort_write_statement(hid, &e, abort_snapshot, shared);
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(final_err.clone()),
                    };
                    return Err(final_err);
                }

                if remaining == 0 {
                    return match inner.finish_activity(
                        hid,
                        ActivityFinish::WriteCompleted { abort_snapshot },
                        shared,
                    ) {
                        Ok(()) => {
                            self.lifecycle = StatementLifecycle::Halted { last_error: None };
                            Ok(StepResult::Done)
                        }
                        Err(e) => {
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            Err(e)
                        }
                    };
                }

                remaining -= 1;
                let step_res = inner
                    .db_set
                    .slot_mut(db_id)
                    .unwrap()
                    .btree
                    .as_mut()
                    .unwrap()
                    .incremental_vacuum_step();
                match step_res {
                    Ok(true) => {
                        self.lifecycle =
                            StatementLifecycle::Running(RunningExec::IncrementalVacuum {
                                db_instance: db_inst,
                                remaining,
                                abort_snapshot,
                            });
                        Ok(StepResult::Row(Row::new(Vec::new())))
                    }
                    Ok(false) => match inner.finish_activity(
                        hid,
                        ActivityFinish::WriteCompleted { abort_snapshot },
                        shared,
                    ) {
                        Ok(()) => {
                            self.lifecycle = StatementLifecycle::Halted { last_error: None };
                            Ok(StepResult::Done)
                        }
                        Err(e) => {
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            Err(e)
                        }
                    },
                    Err(err) => {
                        let final_err =
                            inner.abort_write_statement(hid, &err, abort_snapshot, shared);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(final_err.clone()),
                        };
                        Err(final_err)
                    }
                }
            }

            StmtCategory::Write => {
                let pre_summary = inner.activity.summary_excluding(hid, &inner.db_set);
                let base_sp_depth = inner.savepoints.len();
                let (target_db, skip_prelock) = if let Some(ref prog) = self.compiled_vdbe {
                    (Some(prog.primary_write_db()), false)
                } else {
                    let stmt_ref = self.stmt.as_ref().expect("non-empty statement");
                    (
                        target_write_db_for_stmt(stmt_ref, &inner.db_set),
                        is_noop_if_exists_ddl(stmt_ref, &inner.db_set),
                    )
                };

                let mut prelocked_write: Option<(DatabaseId, SchemaInstanceId)> = None;
                if !skip_prelock
                    && let Some(db_id) = target_db
                    && let Some(inst) = inner
                        .db_set
                        .slot(db_id)
                        .filter(|s| s.btree.is_some())
                        .map(|s| s.schema_instance_id)
                {
                    let prev_tx_state = inner
                        .db_set
                        .slot(db_id)
                        .unwrap()
                        .btree
                        .as_ref()
                        .unwrap()
                        .transaction_state();
                    let opened_write_here = prev_tx_state != TransactionState::Write;
                    if opened_write_here {
                        let res = {
                            let bt = inner
                                .db_set
                                .slot_mut(db_id)
                                .unwrap()
                                .btree
                                .as_mut()
                                .unwrap();
                            inner.busy_policy.retry_btree(bt, |b| {
                                b.pager_mut()
                                    .begin_write_with_savepoint_depth(base_sp_depth)
                            })
                        };
                        if let Err(e) = res {
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    } else if base_sp_depth > 0 {
                        let bt = inner
                            .db_set
                            .slot_mut(db_id)
                            .unwrap()
                            .btree
                            .as_mut()
                            .unwrap();
                        if let Err(e) = bt.pager_mut().ensure_subsavepoints(base_sp_depth) {
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    }

                    let db_bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                    let cleanup_unstarted_write = |inner: &mut ConnectionInner| {
                        if opened_write_here
                            && (pre_summary.other_write_db_mask & db_bit) == 0
                            && pre_summary.other_writers == 0
                        {
                            let retain_read = prev_tx_state == TransactionState::Read
                                || (pre_summary.other_read_db_mask & db_bit) != 0;
                            if let Some(slot) = inner.db_set.slot_mut(db_id)
                                && let Some(ref mut bt) = slot.btree
                                && let Err(rb_err) = bt.rollback_with_trip(
                                    extended::SQLITE_ABORT_ROLLBACK,
                                    true,
                                    retain_read,
                                )
                            {
                                let _ = inner.poison_with(rb_err);
                            }
                        }
                    };

                    if self.requires_schema
                        && let Err(e) = inner.refresh_open_tx_schemas()
                    {
                        cleanup_unstarted_write(inner);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }

                    if self.needs_reprepare(inner) {
                        if self.mode == PrepareMode::V1 {
                            cleanup_unstarted_write(inner);
                            let err =
                                SqliteError::new(ErrorCode::Schema, "database schema has changed");
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(err.clone()),
                            };
                            return Err(err);
                        }
                        if let Err(e) = self.reprepare_in_open_tx(inner) {
                            cleanup_unstarted_write(inner);
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                        if self.category != Some(StmtCategory::Write)
                            || target_write_db_for_stmt(
                                self.stmt.as_ref().expect("non-empty statement"),
                                &inner.db_set,
                            ) != Some(db_id)
                        {
                            cleanup_unstarted_write(inner);
                            return self.start_execution_in_session(hid, shared, inner);
                        }
                    }

                    if !inner.is_autocommit() || pre_summary.other_writers > 0 {
                        let sp_idx = inner.savepoints.len();
                        let ensure_res = inner
                            .db_set
                            .slot_mut(db_id)
                            .unwrap()
                            .btree
                            .as_mut()
                            .unwrap()
                            .pager_mut()
                            .ensure_subsavepoints(sp_idx + 1);
                        if let Err(e) = ensure_res {
                            cleanup_unstarted_write(inner);
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    }

                    prelocked_write = Some((db_id, inst));
                } else if !skip_prelock && self.requires_schema {
                    if let Err(e) = inner.refresh_open_tx_schemas() {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    if self.needs_reprepare(inner) {
                        if self.mode == PrepareMode::V1 {
                            let err =
                                SqliteError::new(ErrorCode::Schema, "database schema has changed");
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(err.clone()),
                            };
                            return Err(err);
                        }
                        if let Err(e) = self.reprepare_in_open_tx(inner) {
                            self.lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                        if self.category != Some(StmtCategory::Write)
                            || target_write_db_for_stmt(
                                self.stmt.as_ref().expect("non-empty statement"),
                                &inner.db_set,
                            )
                            .is_some()
                        {
                            return self.start_execution_in_session(hid, shared, inner);
                        }
                    }
                }

                // Open an anonymous statement subsavepoint whenever an outer tx or another writer exists!
                let stmt_sp_slot = (!inner.is_autocommit() || pre_summary.other_writers > 0)
                    .then_some(inner.savepoints.len());
                let top_level_sql = inner.trace_hook.is_some().then(|| self.raw_sql.clone());
                let mut stmt_tx = StmtTxTracker {
                    stmt_sp_slot,
                    top_level_sql,
                    ignored_fk_parent_tables: self.ignored_fk_parent_tables.clone(),
                    ..StmtTxTracker::default()
                };
                let initial_insts = match prelocked_write {
                    Some((db_id, inst)) => {
                        let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                        stmt_tx.touched_write_mask |= bit;
                        stmt_tx.write_db_mask |= bit;
                        stmt_tx.read_db_mask |= bit;
                        if !inner.is_autocommit() {
                            inner.in_user_btree_tx = true;
                        }
                        SmallInstVec::single(inst)
                    }
                    None => SmallInstVec::new(),
                };

                let is_dml = self.compiled_vdbe.is_some()
                    || self.stmt.as_ref().is_some_and(|st| {
                        matches!(st, Stmt::Insert(_) | Stmt::Update(_) | Stmt::Delete(_))
                    });
                let schemas_before = if is_dml && !inner.writable_schema {
                    Vec::new()
                } else {
                    inner.snapshot_open_schemas()
                };
                let fk_before = inner.deferred_fk_suspects.clone();
                let rt_before = inner.runtime;
                let mutation_len_before = inner.mutation_pipeline.len();
                let schema_gen_before = inner.schema_generation;
                let v1_expire_gen_before = inner.v1_expire_generation;
                let schema_changed_before = inner.schema_changed_in_tx;

                inner.activity.start_with_instances(
                    hid,
                    true,
                    true,
                    initial_insts.clone(),
                    initial_insts.clone(),
                );
                let summary = inner.activity.summary_excluding(hid, &inner.db_set);

                if let Err(e) = inner.check_any_interrupted() {
                    let early_snap = WriteAbortSnapshot {
                        stmt_sp_slot,
                        touched_write_instances: initial_insts,
                        started_read_instances: SmallInstVec::new(),
                        fk_before,
                        rt_before,
                        is_dml,
                        mutation_len_before,
                        schemas_before,
                        schema_gen_before,
                        v1_expire_gen_before,
                        schema_changed_before,
                    };
                    let final_err = inner.abort_write_statement(hid, &e, early_snap, shared);
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(final_err.clone()),
                    };
                    return Err(final_err);
                }

                let stmt_ref = self.stmt.as_ref().expect("non-empty statement");
                let compiled_prog_ref = self.compiled_vdbe.as_ref();
                let write_scratch = &mut self.write_vm_scratch;
                let params_ref = &self.params;
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                let needs_ts_for_compiled =
                    compiled_prog_ref.is_none_or(|p| p.needs_stmt_timestamp());
                if prev_ts.is_none() && needs_ts_for_compiled {
                    inner.runtime.stmt_timestamp_ms = Some(
                        inner
                            .db_set
                            .main_btree()
                            .pager()
                            .vfs()
                            .current_unix_millis(),
                    );
                }
                let mut expire_self_only = false;
                let exec_res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| {
                        if let Some(sp_slot) = ctx.stmt_tx.stmt_sp_slot
                            && ctx.db_set.vtab_registry().has_any_in_tx()
                        {
                            crate::vtab::vtab_savepoint_all(ctx, sp_slot)?;
                        }
                        if let (Some(prog), Stmt::Insert(_) | Stmt::Update(_) | Stmt::Delete(_)) =
                            (compiled_prog_ref, stmt_ref)
                            && let Some(out) = engine::try_execute_compiled_insert(
                                ctx,
                                prog,
                                write_scratch,
                                params_ref,
                            )?
                        {
                            return Ok(out);
                        }
                        if ctx.runtime.stmt_timestamp_ms.is_none() {
                            ctx.runtime.stmt_timestamp_ms =
                                Some(ctx.db_set.main_btree().pager().vfs().current_unix_millis());
                        }
                        crate::vtab::ensure_stmt_vtabs_connected(ctx, stmt_ref)?;
                        execute_write_body(ctx, stmt_ref, params_ref)
                    },
                );
                inner.runtime.stmt_timestamp_ms = prev_ts;

                let mut touched_write_instances =
                    mask_to_instances(&inner.db_set, stmt_tx.touched_write_mask);
                merge_instances(&mut touched_write_instances, &initial_insts);
                let started_read_instances =
                    mask_to_instances(&inner.db_set, stmt_tx.started_read_mask);
                let mut read_insts = mask_to_instances(&inner.db_set, stmt_tx.read_db_mask);
                merge_instances(&mut read_insts, &touched_write_instances);
                inner
                    .activity
                    .update_instances(hid, &read_insts, &touched_write_instances);
                if !inner.is_autocommit()
                    && (!touched_write_instances.is_empty()
                        || !read_insts.is_empty()
                        || inner.db_set.any_in_transaction())
                {
                    inner.in_user_btree_tx = true;
                }

                let abort_snapshot = WriteAbortSnapshot {
                    stmt_sp_slot,
                    touched_write_instances,
                    started_read_instances,
                    fk_before,
                    rt_before,
                    is_dml,
                    mutation_len_before,
                    schemas_before,
                    schema_gen_before,
                    v1_expire_gen_before,
                    schema_changed_before,
                };

                match exec_res {
                    Ok(out) => {
                        if !out.column_names.is_empty() {
                            self.column_names = out.column_names;
                        }
                        if self.compiled_schema_generation != inner.schema_generation
                            || self.compiled_v1_expire_generation != inner.v1_expire_generation
                        {
                            self.compiled_schema_generation = inner.schema_generation;
                            self.compiled_v1_expire_generation = inner.v1_expire_generation;
                            self.compiled_slot_cookies = inner.snapshot_slot_cookies();
                        }

                        let mut iter = out.rows.into_iter();
                        if let Some(first_row) = iter.next() {
                            // Keep write lease & transaction open while paused at `StepResult::Row`!
                            self.lifecycle =
                                StatementLifecycle::Running(RunningExec::ReturningWrite {
                                    rows: iter,
                                    abort_snapshot,
                                });
                            Ok(StepResult::Row(Row::new(first_row)))
                        } else {
                            match inner.finish_activity(
                                hid,
                                ActivityFinish::WriteCompleted { abort_snapshot },
                                shared,
                            ) {
                                Ok(()) => {
                                    self.lifecycle =
                                        StatementLifecycle::Halted { last_error: None };
                                    Ok(StepResult::Done)
                                }
                                Err(e) => {
                                    self.lifecycle = StatementLifecycle::Halted {
                                        last_error: Some(e.clone()),
                                    };
                                    Err(e)
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let final_err =
                            inner.abort_write_statement(hid, &err, abort_snapshot, shared);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(final_err.clone()),
                        };
                        Err(final_err)
                    }
                }
            }
        }
    }

    #[inline]
    fn advance_running_in_session(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<StepResult> {
        advance_running_fields(
            hid,
            shared,
            inner,
            &mut self.lifecycle,
            &mut self.cached_read_vm,
        )
    }

    /// Steps through all remaining rows and collects them into a `Vec<Row>`.
    pub fn collect_rows(&mut self) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        while let StepResult::Row(r) = self.step()? {
            rows.push(r);
        }
        Ok(rows)
    }
}

fn advance_running_fields(
    hid: HandleId,
    shared: &ConnectionShared,
    inner: &mut ConnectionInner,
    lifecycle: &mut StatementLifecycle,
    cached_read_vm: &mut Option<VdbeVm>,
) -> Result<StepResult> {
    let StatementLifecycle::Running(ref mut running) = *lifecycle else {
        unreachable!("advance_running_fields requires Running state");
    };

    match running {
        RunningExec::PointSelectRowYielded { main_inst } => {
            let inst = *main_inst;
            inner.runtime.stmt_timestamp_ms = None;
            let fin_res = inner.finish_point_select_main_read(hid, inst, shared);
            *lifecycle = StatementLifecycle::Halted {
                last_error: fin_res.as_ref().err().cloned(),
            };
            fin_res?;
            Ok(StepResult::Done)
        }
        RunningExec::Vdbe { vm, read_instances } => {
            let step_res = {
                let encoding = inner.effective_encoding();
                let rt_snapshot = vm.runtime_ctx;
                let mut services = ExecutionContext {
                    interrupt_flag: &inner.active_interrupt_flag,
                    runtime: &rt_snapshot,
                    encoding,
                    functions: &inner.functions,
                    collations: &inner.collations,
                    progress: &mut inner.progress,
                    strict_subtype: inner.strict_subtype,
                };
                vm.step_out(&mut inner.db_set, &mut services)
            };
            match step_res {
                Ok(VmStepOut::InlineRow(ir)) => {
                    let rt_changed = inner.runtime.last_insert_rowid
                        != vm.runtime_ctx.last_insert_rowid
                        || inner.runtime.last_changes != vm.runtime_ctx.last_changes
                        || inner.runtime.total_changes != vm.runtime_ctx.total_changes;
                    inner.runtime = vm.runtime_ctx;
                    inner.runtime.stmt_timestamp_ms = None;
                    if rt_changed {
                        inner.publish(shared);
                    }
                    Ok(StepResult::Row(Row::from_inline(ir)))
                }
                Ok(VmStepOut::Row(vals)) => {
                    let rt_changed = inner.runtime.last_insert_rowid
                        != vm.runtime_ctx.last_insert_rowid
                        || inner.runtime.last_changes != vm.runtime_ctx.last_changes
                        || inner.runtime.total_changes != vm.runtime_ctx.total_changes;
                    inner.runtime = vm.runtime_ctx;
                    inner.runtime.stmt_timestamp_ms = None;
                    if rt_changed {
                        inner.publish(shared);
                    }
                    Ok(StepResult::Row(Row::new(vals)))
                }
                Ok(VmStepOut::Done) => {
                    vm.close_cursors(&mut inner.db_set);
                    let r_insts = std::mem::take(read_instances);
                    let rt_changed = inner.runtime.last_insert_rowid
                        != vm.runtime_ctx.last_insert_rowid
                        || inner.runtime.last_changes != vm.runtime_ctx.last_changes
                        || inner.runtime.total_changes != vm.runtime_ctx.total_changes;
                    inner.runtime = vm.runtime_ctx;
                    inner.runtime.stmt_timestamp_ms = None;
                    if rt_changed {
                        inner.publish(shared);
                    }
                    let fin_res = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadDone {
                            lease_cursors: Vec::new(),
                            read_instances: r_insts,
                        },
                        shared,
                    );
                    let prev = std::mem::replace(
                        lifecycle,
                        StatementLifecycle::Halted {
                            last_error: fin_res.as_ref().err().cloned(),
                        },
                    );
                    if let StatementLifecycle::Running(RunningExec::Vdbe { vm, .. }) = prev {
                        *cached_read_vm = Some(vm);
                    }
                    fin_res?;
                    Ok(StepResult::Done)
                }
                Err(e) => {
                    vm.close_cursors(&mut inner.db_set);
                    let r_insts = std::mem::take(read_instances);
                    inner.runtime.stmt_timestamp_ms = None;
                    let _ = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadAborted {
                            lease_cursors: Vec::new(),
                            read_instances: r_insts,
                        },
                        shared,
                    );
                    let prev = std::mem::replace(
                        lifecycle,
                        StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        },
                    );
                    if let StatementLifecycle::Running(RunningExec::Vdbe { vm, .. }) = prev {
                        *cached_read_vm = Some(vm);
                    }
                    Err(e)
                }
            }
        }

        RunningExec::MaterializedRead {
            rows,
            lease_cursors,
            read_instances,
        } => {
            if let Err(e) = inner.check_any_interrupted() {
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let _ = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadAborted {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Err(e);
            }

            for &(inst, cid) in lease_cursors.iter() {
                let Some(slot) = inner.db_set.slot_by_instance_mut(inst) else {
                    continue;
                };
                let Some(ref mut bt) = slot.btree else {
                    continue;
                };
                match bt.cursor_state(cid) {
                    Some(&BtreeCursorState::Fault(code)) => {
                        let msg = if code == extended::SQLITE_ABORT_ROLLBACK {
                            "abort due to ROLLBACK"
                        } else {
                            ErrorCode::from_raw_primary(code).as_str()
                        };
                        let err = SqliteError::with_extended(code, msg);
                        let cursors = std::mem::take(lease_cursors);
                        let r_insts = std::mem::take(read_instances);
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: cursors,
                                read_instances: r_insts,
                            },
                            shared,
                        );
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
                    Some(BtreeCursorState::RequireSeek { .. }) => {
                        if let Err(e) = bt.restore_cursor_position(cid) {
                            let cursors = std::mem::take(lease_cursors);
                            let r_insts = std::mem::take(read_instances);
                            let _ = inner.finish_activity(
                                hid,
                                ActivityFinish::ReadAborted {
                                    lease_cursors: cursors,
                                    read_instances: r_insts,
                                },
                                shared,
                            );
                            *lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    }
                    _ => {}
                }
            }

            if let Some(vals) = rows.next() {
                Ok(StepResult::Row(Row::new(vals)))
            } else {
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let fin_res = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                if let Err(e) = fin_res {
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }
                *lifecycle = StatementLifecycle::Halted { last_error: None };
                Ok(StepResult::Done)
            }
        }

        RunningExec::MaterializedInlineRead {
            rows,
            lease_cursors,
            read_instances,
        } => {
            if let Err(e) = inner.check_any_interrupted() {
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let _ = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadAborted {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Err(e);
            }

            for &(inst, cid) in lease_cursors.iter() {
                let Some(slot) = inner.db_set.slot_by_instance_mut(inst) else {
                    continue;
                };
                let Some(ref mut bt) = slot.btree else {
                    continue;
                };
                match bt.cursor_state(cid) {
                    Some(&BtreeCursorState::Fault(code)) => {
                        let msg = if code == extended::SQLITE_ABORT_ROLLBACK {
                            "abort due to ROLLBACK"
                        } else {
                            ErrorCode::from_raw_primary(code).as_str()
                        };
                        let err = SqliteError::with_extended(code, msg);
                        let cursors = std::mem::take(lease_cursors);
                        let r_insts = std::mem::take(read_instances);
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: cursors,
                                read_instances: r_insts,
                            },
                            shared,
                        );
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
                    Some(BtreeCursorState::RequireSeek { .. }) => {
                        if let Err(e) = bt.restore_cursor_position(cid) {
                            let cursors = std::mem::take(lease_cursors);
                            let r_insts = std::mem::take(read_instances);
                            let _ = inner.finish_activity(
                                hid,
                                ActivityFinish::ReadAborted {
                                    lease_cursors: cursors,
                                    read_instances: r_insts,
                                },
                                shared,
                            );
                            *lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    }
                    _ => {}
                }
            }

            if let Some(ir) = rows.next() {
                Ok(StepResult::Row(Row::from_inline(ir)))
            } else {
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let fin_res = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                if let Err(e) = fin_res {
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }
                *lifecycle = StatementLifecycle::Halted { last_error: None };
                Ok(StepResult::Done)
            }
        }

        RunningExec::FastSingleTextArena {
            arena,
            spans,
            pos,
            lease_cursors,
            read_instances,
        } => {
            if let Err(e) = inner.check_any_interrupted() {
                std::mem::take(arena);
                std::mem::take(spans);
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let _ = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadAborted {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Err(e);
            }

            for &(inst, cid) in lease_cursors.iter() {
                let Some(slot) = inner.db_set.slot_by_instance_mut(inst) else {
                    continue;
                };
                let Some(ref mut bt) = slot.btree else {
                    continue;
                };
                match bt.cursor_state(cid) {
                    Some(&BtreeCursorState::Fault(code)) => {
                        let msg = if code == extended::SQLITE_ABORT_ROLLBACK {
                            "abort due to ROLLBACK"
                        } else {
                            ErrorCode::from_raw_primary(code).as_str()
                        };
                        let err = SqliteError::with_extended(code, msg);
                        std::mem::take(arena);
                        std::mem::take(spans);
                        let cursors = std::mem::take(lease_cursors);
                        let r_insts = std::mem::take(read_instances);
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: cursors,
                                read_instances: r_insts,
                            },
                            shared,
                        );
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
                    Some(BtreeCursorState::RequireSeek { .. }) => {
                        if let Err(e) = bt.restore_cursor_position(cid) {
                            std::mem::take(arena);
                            std::mem::take(spans);
                            let cursors = std::mem::take(lease_cursors);
                            let r_insts = std::mem::take(read_instances);
                            let _ = inner.finish_activity(
                                hid,
                                ActivityFinish::ReadAborted {
                                    lease_cursors: cursors,
                                    read_instances: r_insts,
                                },
                                shared,
                            );
                            *lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            return Err(e);
                        }
                    }
                    _ => {}
                }
            }

            if *pos < spans.len() {
                let (off, len) = spans[*pos];
                *pos += 1;
                let row = if len == u32::MAX {
                    Row::from_inline(InlineRow::single(InlineVal::Null))
                } else {
                    let s = &arena[off as usize..(off + len) as usize];
                    if let Some(iv) = InlineVal::try_short_text(s) {
                        Row::from_inline(InlineRow::single(iv))
                    } else {
                        Row::new(vec![Value::Text(s.to_owned())])
                    }
                };
                Ok(StepResult::Row(row))
            } else {
                std::mem::take(arena);
                std::mem::take(spans);
                let cursors = std::mem::take(lease_cursors);
                let r_insts = std::mem::take(read_instances);
                let fin_res = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: cursors,
                        read_instances: r_insts,
                    },
                    shared,
                );
                if let Err(e) = fin_res {
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }
                *lifecycle = StatementLifecycle::Halted { last_error: None };
                Ok(StepResult::Done)
            }
        }

        RunningExec::MaterializedNoLock { rows } => {
            if let Some(vals) = rows.next() {
                Ok(StepResult::Row(Row::new(vals)))
            } else {
                let _ = inner.finish_activity(hid, ActivityFinish::NoLock, shared);
                inner.maybe_run_wal_autocheckpoint();
                *lifecycle = StatementLifecycle::Halted { last_error: None };
                Ok(StepResult::Done)
            }
        }

        RunningExec::ReturningWrite {
            rows,
            abort_snapshot,
        } => {
            if let Err(e) = inner.check_any_interrupted() {
                let snap = abort_snapshot.clone();
                let final_err = inner.abort_write_statement(hid, &e, snap, shared);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(final_err.clone()),
                };
                return Err(final_err);
            }

            if let Some(vals) = rows.next() {
                Ok(StepResult::Row(Row::new(vals)))
            } else {
                let snap = abort_snapshot.clone();
                match inner.finish_activity(
                    hid,
                    ActivityFinish::WriteCompleted {
                        abort_snapshot: snap,
                    },
                    shared,
                ) {
                    Ok(()) => {
                        *lifecycle = StatementLifecycle::Halted { last_error: None };
                        Ok(StepResult::Done)
                    }
                    Err(e) => {
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                }
            }
        }

        RunningExec::IncrementalVacuum {
            db_instance,
            remaining,
            abort_snapshot,
        } => {
            if let Err(e) = inner.check_any_interrupted() {
                let snap = abort_snapshot.clone();
                let final_err = inner.abort_write_statement(hid, &e, snap, shared);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(final_err.clone()),
                };
                return Err(final_err);
            }

            if *remaining == 0 {
                let snap = abort_snapshot.clone();
                return match inner.finish_activity(
                    hid,
                    ActivityFinish::WriteCompleted {
                        abort_snapshot: snap,
                    },
                    shared,
                ) {
                    Ok(()) => {
                        *lifecycle = StatementLifecycle::Halted { last_error: None };
                        Ok(StepResult::Done)
                    }
                    Err(e) => {
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        Err(e)
                    }
                };
            }

            *remaining -= 1;
            let step_res = match inner
                .db_set
                .slot_by_instance_mut(*db_instance)
                .and_then(|s| s.btree.as_mut())
            {
                Some(bt) => bt.incremental_vacuum_step(),
                None => Ok(false),
            };
            match step_res {
                Ok(true) => Ok(StepResult::Row(Row::new(Vec::new()))),
                Ok(false) => {
                    let snap = abort_snapshot.clone();
                    match inner.finish_activity(
                        hid,
                        ActivityFinish::WriteCompleted {
                            abort_snapshot: snap,
                        },
                        shared,
                    ) {
                        Ok(()) => {
                            *lifecycle = StatementLifecycle::Halted { last_error: None };
                            Ok(StepResult::Done)
                        }
                        Err(e) => {
                            *lifecycle = StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            };
                            Err(e)
                        }
                    }
                }
                Err(err) => {
                    let snap = abort_snapshot.clone();
                    let final_err = inner.abort_write_statement(hid, &err, snap, shared);
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(final_err.clone()),
                    };
                    Err(final_err)
                }
            }
        }

        RunningExec::PendingCommit {
            release_savepoint,
            last_busy,
        } => {
            let summary = inner.activity.summary_excluding(hid, &inner.db_set);
            let rel_opt = release_savepoint.clone();
            let mut expire_self_only = false;
            let mut stmt_tx = StmtTxTracker::default();
            let res = inner.with_exec_ctx(
                shared,
                summary,
                &mut stmt_tx,
                &mut expire_self_only,
                |ctx| match rel_opt {
                    Some(ref sp) => release_savepoint_op(ctx, sp),
                    None => commit_transaction(ctx),
                },
            );

            match res {
                Ok(()) => {
                    inner.activity.remove(hid);
                    inner.maybe_run_wal_autocheckpoint();
                    *lifecycle = StatementLifecycle::Halted { last_error: None };
                    Ok(StepResult::Done)
                }
                Err(e) if e.code == ErrorCode::Busy && summary.other_writers == 0 => {
                    *last_busy = e.clone();
                    Err(e)
                }
                Err(e) => {
                    inner.activity.remove(hid);
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    Err(e)
                }
            }
        }
    }
}

#[inline]
fn slot_cookies_changed_slice(
    compiled_slot_cookies: &[(SchemaInstanceId, u32, bool)],
    inner: &ConnectionInner,
) -> bool {
    if let [(inst, cookie, had_pages)] = *compiled_slot_cookies {
        let main_slot = inner.db_set.main_slot();
        if main_slot.schema_instance_id == inst && main_slot.btree.is_some() {
            return main_slot.schema.schema_cookie != cookie
                || main_slot.schema.had_pages != had_pages;
        }
    }
    for &(inst, cookie, had_pages) in compiled_slot_cookies {
        let Some(slot) = inner
            .db_set
            .slots()
            .iter()
            .find(|s| s.schema_instance_id == inst && s.btree.is_some())
        else {
            return true;
        };
        if slot.schema.schema_cookie != cookie || slot.schema.had_pages != had_pages {
            return true;
        }
    }
    false
}

#[inline(always)]
fn affinity_is_noop_for_insert(aff: TypeAffinity, val: &Value) -> bool {
    match (aff, val) {
        (TypeAffinity::Blob, _) => true,
        (TypeAffinity::Text, Value::Null | Value::Blob(_) | Value::Text(_)) => true,
        (
            TypeAffinity::Numeric | TypeAffinity::Integer,
            Value::Null | Value::Blob(_) | Value::Integer(_),
        ) => true,
        (TypeAffinity::Real, Value::Null | Value::Blob(_)) => true,
        (TypeAffinity::Real, Value::Real(r)) => r.to_bits() != (-0.0_f64).to_bits(),
        _ => false,
    }
}

#[inline(always)]
fn resolve_insert_val_source<'a>(src: &'a InsertValSource, params: &'a [Value]) -> &'a Value {
    match src {
        InsertValSource::Null => &Value::Null,
        InsertValSource::Const(v)
        | InsertValSource::JsonbConstOk(v)
        | InsertValSource::JsonbConstErr(v) => v,
        InsertValSource::Param1Based(p) | InsertValSource::JsonbParam1Based(p) => {
            params.get((*p as usize) - 1).unwrap_or(&Value::Null)
        }
    }
}

#[inline]
fn reclaim_jsonb_scratch(coerced: &mut [Value], scratch_jsonb: &mut Vec<u8>) {
    for v in coerced {
        if let Value::Blob(b) = v
            && b.capacity() > scratch_jsonb.capacity()
        {
            *scratch_jsonb = std::mem::take(b);
        }
    }
}

#[inline]
fn resolve_explicit_insert_rowid(
    rowid_src: &Option<InsertValSource>,
    params: &[Value],
) -> Result<Option<i64>> {
    match rowid_src {
        None => Ok(None),
        Some(src) => {
            let raw_rid = resolve_insert_val_source(src, params);
            if raw_rid.is_null() {
                Ok(None)
            } else if let Value::Integer(rid) = *raw_rid {
                Ok(Some(rid))
            } else {
                match TypeAffinity::Integer.apply(raw_rid.clone()) {
                    Value::Integer(rid) => Ok(Some(rid)),
                    _ => Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")),
                }
            }
        }
    }
}

#[inline]
fn finish_single_row_insert_write(
    plan: &SingleRowInsertPlan,
    explicit_rowid: Option<i64>,
    val_refs: &[&Value],
    scratch_rec: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<i64> {
    let (rowid, need_dup_check) = match explicit_rowid {
        Some(rid) => (rid, true),
        None => (bt.next_rowid(plan.root_pgno)?, false),
    };

    let enc = bt.encoding();
    if bt.is_p3v2_utf8() {
        if !bt.insert_table_pax_refs(
            plan.root_pgno,
            rowid,
            val_refs,
            !need_dup_check,
            scratch_rec,
        )? {
            return Err(SqliteError::constraint(
                plan.dup_ext_code,
                plan.dup_msg.clone(),
            ));
        }

        if !plan.indexes.is_empty() {
            let rid_val = Value::Integer(rowid);
            let mut idx_refs: [&Value; 17] = [&Value::Null; 17];
            for idx in &plan.indexes {
                let k_len = idx.col_slots.len();
                for (k, &slot) in idx.col_slots.iter().enumerate() {
                    idx_refs[k] = match slot {
                        None => &rid_val,
                        Some(ci) => val_refs[ci],
                    };
                }
                idx_refs[k_len] = &rid_val;
                if let Some(n_keys) = sqliters_btree::orders_allow_pax_index(&idx.orders)
                    && let Some(entry) =
                        sqliters_btree::value_refs_as_pax_index_entry(&idx_refs[..=k_len], n_keys)
                {
                    bt.insert_index_pax_entry(idx.root_pgno, n_keys, entry, &idx.orders)?;
                } else {
                    encode_value_refs_record_into(&idx_refs[..=k_len], enc, scratch_rec);
                    bt.insert_index(idx.root_pgno, scratch_rec, &idx.orders)?;
                }
            }
        }

        return Ok(rowid);
    }

    encode_value_refs_record_into(val_refs, enc, scratch_rec);
    if need_dup_check {
        if !bt.insert_table_unique(plan.root_pgno, rowid, scratch_rec)? {
            return Err(SqliteError::constraint(
                plan.dup_ext_code,
                plan.dup_msg.clone(),
            ));
        }
    } else {
        bt.insert_table(plan.root_pgno, rowid, scratch_rec)?;
    }

    if !plan.indexes.is_empty() {
        let rid_val = Value::Integer(rowid);
        let mut idx_refs: [&Value; 17] = [&Value::Null; 17];
        for idx in &plan.indexes {
            let k_len = idx.col_slots.len();
            for (k, &slot) in idx.col_slots.iter().enumerate() {
                idx_refs[k] = match slot {
                    None => &rid_val,
                    Some(ci) => val_refs[ci],
                };
            }
            idx_refs[k_len] = &rid_val;
            encode_value_refs_record_into(&idx_refs[..=k_len], enc, scratch_rec);
            bt.insert_index(idx.root_pgno, scratch_rec, &idx.orders)?;
        }
    }

    Ok(rowid)
}

fn execute_single_row_insert_plan(
    plan: &SingleRowInsertPlan,
    params: &[Value],
    scratch_rec: &mut Vec<u8>,
    scratch_jsonb: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<i64> {
    if bt.transaction_state() != TransactionState::Write {
        bt.begin_write()?;
    }

    let n_cols = plan.cols.len();

    // Ultra-light zero-`Value::Drop` fast path when no `jsonb(...)` expression is present and
    // every column already matches its target affinity (`W01` 10,000-row bulk insert!).
    if !plan.has_builtin_jsonb {
        let explicit_rowid = resolve_explicit_insert_rowid(&plan.rowid_src, params)?;
        let mut val_refs: [&Value; 16] = [&Value::Null; 16];
        let mut need_coercion = false;
        for (i, col) in plan.cols.iter().enumerate() {
            let raw_val = resolve_insert_val_source(&col.src, params);
            if affinity_is_noop_for_insert(col.affinity, raw_val) {
                if raw_val.is_null()
                    && let Some(ref msg) = col.notnull_msg
                {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        msg.clone(),
                    ));
                }
                val_refs[i] = raw_val;
            } else {
                need_coercion = true;
                break;
            }
        }
        if !need_coercion {
            return finish_single_row_insert_write(
                plan,
                explicit_rowid,
                &val_refs[..n_cols],
                scratch_rec,
                bt,
            );
        }
    }

    // Compact 4-column fast path when `n_cols <= 4` (`W10`: `id, bucket, jsonb(?3)`), avoiding
    // initializing and dropping a 16-slot `[Value; 16]` array on every row!
    if n_cols <= 4 {
        let mut coerced: [Value; 4] = [Value::Null, Value::Null, Value::Null, Value::Null];
        let mut is_coerced = [false; 4];

        if plan.has_builtin_jsonb {
            for (i, col) in plan.cols.iter().enumerate() {
                match col.src {
                    InsertValSource::JsonbParam1Based(p) => {
                        let raw_arg = params.get((p as usize) - 1).unwrap_or(&Value::Null);
                        let mut jb_buf = std::mem::take(scratch_jsonb);
                        match sqliters_types::eval_jsonb_value_into(raw_arg, &mut jb_buf) {
                            Ok(true) => {
                                coerced[i] = Value::Blob(jb_buf);
                                is_coerced[i] = true;
                            }
                            Ok(false) => {
                                *scratch_jsonb = jb_buf;
                                coerced[i] = Value::Null;
                                is_coerced[i] = true;
                            }
                            Err(e) => {
                                *scratch_jsonb = jb_buf;
                                reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
                                return Err(e);
                            }
                        }
                    }
                    InsertValSource::JsonbConstErr(ref bad_val) => {
                        let mut jb_buf = std::mem::take(scratch_jsonb);
                        let res = sqliters_types::eval_jsonb_value_into(bad_val, &mut jb_buf);
                        *scratch_jsonb = jb_buf;
                        if let Err(e) = res {
                            reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
                            return Err(e);
                        }
                    }
                    _ => {}
                }
            }
        }

        let run_res = (|| -> Result<i64> {
            let explicit_rowid = resolve_explicit_insert_rowid(&plan.rowid_src, params)?;
            for (i, col) in plan.cols.iter().enumerate() {
                if is_coerced[i] {
                    if coerced[i].is_null()
                        && let Some(ref msg) = col.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }
                    continue;
                }
                let raw_val = resolve_insert_val_source(&col.src, params);
                if affinity_is_noop_for_insert(col.affinity, raw_val) {
                    if raw_val.is_null()
                        && let Some(ref msg) = col.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }
                } else {
                    let v = col.affinity.apply(raw_val.clone());
                    if v.is_null()
                        && let Some(ref msg) = col.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }
                    coerced[i] = v;
                    is_coerced[i] = true;
                }
            }

            let mut val_refs: [&Value; 4] = [&Value::Null; 4];
            for (i, col) in plan.cols.iter().enumerate() {
                val_refs[i] = if is_coerced[i] {
                    &coerced[i]
                } else {
                    resolve_insert_val_source(&col.src, params)
                };
            }
            finish_single_row_insert_write(
                plan,
                explicit_rowid,
                &val_refs[..n_cols],
                scratch_rec,
                bt,
            )
        })();

        if plan.has_builtin_jsonb {
            reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
        }
        return run_res;
    }

    let mut coerced: [Value; 16] = std::array::from_fn(|_| Value::Null);
    let mut is_coerced = [false; 16];

    // 1. Evaluate any built-in `jsonb(...)` expressions first (matching VDBE step-2 expression order).
    if plan.has_builtin_jsonb {
        for (i, col) in plan.cols.iter().enumerate() {
            match col.src {
                InsertValSource::JsonbParam1Based(p) => {
                    let raw_arg = params.get((p as usize) - 1).unwrap_or(&Value::Null);
                    let mut jb_buf = std::mem::take(scratch_jsonb);
                    match sqliters_types::eval_jsonb_value_into(raw_arg, &mut jb_buf) {
                        Ok(true) => {
                            coerced[i] = Value::Blob(jb_buf);
                            is_coerced[i] = true;
                        }
                        Ok(false) => {
                            *scratch_jsonb = jb_buf;
                            coerced[i] = Value::Null;
                            is_coerced[i] = true;
                        }
                        Err(e) => {
                            *scratch_jsonb = jb_buf;
                            reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
                            return Err(e);
                        }
                    }
                }
                InsertValSource::JsonbConstErr(ref bad_val) => {
                    let mut jb_buf = std::mem::take(scratch_jsonb);
                    let res = sqliters_types::eval_jsonb_value_into(bad_val, &mut jb_buf);
                    *scratch_jsonb = jb_buf;
                    if let Err(e) = res {
                        reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
                        return Err(e);
                    }
                }
                _ => {}
            }
        }
    }

    let run_res = (|| -> Result<i64> {
        let explicit_rowid = resolve_explicit_insert_rowid(&plan.rowid_src, params)?;

        for (i, col) in plan.cols.iter().enumerate() {
            if is_coerced[i] {
                if coerced[i].is_null()
                    && let Some(ref msg) = col.notnull_msg
                {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        msg.clone(),
                    ));
                }
                continue;
            }
            let raw_val = resolve_insert_val_source(&col.src, params);
            if affinity_is_noop_for_insert(col.affinity, raw_val) {
                if raw_val.is_null()
                    && let Some(ref msg) = col.notnull_msg
                {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        msg.clone(),
                    ));
                }
            } else {
                let v = col.affinity.apply(raw_val.clone());
                if v.is_null()
                    && let Some(ref msg) = col.notnull_msg
                {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        msg.clone(),
                    ));
                }
                coerced[i] = v;
                is_coerced[i] = true;
            }
        }

        let mut val_refs: [&Value; 16] = [&Value::Null; 16];
        for (i, col) in plan.cols.iter().enumerate() {
            val_refs[i] = if is_coerced[i] {
                &coerced[i]
            } else {
                resolve_insert_val_source(&col.src, params)
            };
        }

        finish_single_row_insert_write(plan, explicit_rowid, &val_refs[..n_cols], scratch_rec, bt)
    })();

    if plan.has_builtin_jsonb {
        reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
    }
    run_res
}

#[derive(Copy, Clone)]
enum RawRecField<'a> {
    Empty,
    Slice(&'a [u8]),
    Inline8([u8; 8], u8),
}

impl<'a> RawRecField<'a> {
    #[inline(always)]
    fn len(self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Slice(s) => s.len(),
            Self::Inline8(_, n) => n as usize,
        }
    }

    #[inline(always)]
    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Empty => &[],
            Self::Slice(s) => s,
            Self::Inline8(buf, n) => &buf[..*n as usize],
        }
    }
}

#[inline(always)]
fn encode_i64_raw_field(i: i64) -> (u64, RawRecField<'static>) {
    const MAX_6B: u64 = (0x0000_8000_u64 << 32) - 1;
    if i == 0 {
        return (8, RawRecField::Empty);
    }
    if i == 1 {
        return (9, RawRecField::Empty);
    }
    let u = if i < 0 { (!i) as u64 } else { i as u64 };
    if u <= 127 {
        (1, RawRecField::Inline8([i as u8, 0, 0, 0, 0, 0, 0, 0], 1))
    } else if u <= 32_767 {
        let b = (i as i16).to_be_bytes();
        (2, RawRecField::Inline8([b[0], b[1], 0, 0, 0, 0, 0, 0], 2))
    } else if u <= 8_388_607 {
        let b = (i as i32).to_be_bytes();
        (
            3,
            RawRecField::Inline8([b[1], b[2], b[3], 0, 0, 0, 0, 0], 3),
        )
    } else if u <= 2_147_483_647 {
        let b = (i as i32).to_be_bytes();
        (
            4,
            RawRecField::Inline8([b[0], b[1], b[2], b[3], 0, 0, 0, 0], 4),
        )
    } else if u <= MAX_6B {
        let b = i.to_be_bytes();
        (
            5,
            RawRecField::Inline8([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0], 6),
        )
    } else {
        (6, RawRecField::Inline8(i.to_be_bytes(), 8))
    }
}

#[inline(always)]
fn decode_raw_int_field(st: u64, b: &[u8]) -> i64 {
    match st {
        8 => 0,
        9 => 1,
        1 => (b[0] as i8) as i64,
        2 => i16::from_be_bytes([b[0], b[1]]) as i64,
        3 => {
            let sign_ext = (b[0] as i8) as i32;
            ((sign_ext << 16) | ((b[1] as i32) << 8) | (b[2] as i32)) as i64
        }
        4 => i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64,
        5 => {
            let hi = i16::from_be_bytes([b[0], b[1]]) as i64;
            let lo = u32::from_be_bytes([b[2], b[3], b[4], b[5]]) as i64;
            (hi << 32) | lo
        }
        6 => i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        _ => 0,
    }
}

#[inline(always)]
fn canonicalize_raw_utf8_field(st_in: u64, b: &[u8]) -> (u64, RawRecField<'_>) {
    if st_in == 0 {
        (0, RawRecField::Empty)
    } else if st_in == 7 {
        let f = f64::from_bits(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]));
        if f.is_nan() {
            (0, RawRecField::Empty)
        } else if f == 0.0 {
            (7, RawRecField::Inline8([0; 8], 8))
        } else {
            (7, RawRecField::Slice(b))
        }
    } else if st_in < 12 {
        encode_i64_raw_field(decode_raw_int_field(st_in, b))
    } else {
        (st_in, RawRecField::Slice(b))
    }
}

#[inline(always)]
fn value_to_utf8_raw_field(v: &Value) -> (u64, RawRecField<'_>) {
    match v {
        Value::Null => (0, RawRecField::Empty),
        Value::Integer(i) => encode_i64_raw_field(*i),
        Value::Real(r) => {
            if r.is_nan() {
                (0, RawRecField::Empty)
            } else if *r == 0.0 {
                (7, RawRecField::Inline8([0; 8], 8))
            } else {
                (7, RawRecField::Inline8(r.to_bits().to_be_bytes(), 8))
            }
        }
        Value::Blob(b) => (12 + 2 * (b.len() as u64), RawRecField::Slice(b.as_slice())),
        Value::Text(s) => (13 + 2 * (s.len() as u64), RawRecField::Slice(s.as_bytes())),
    }
}

#[inline]
fn pack_raw_fields_into(sts: &[u64], srcs: &[RawRecField<'_>], out: &mut Vec<u8>) {
    if sts.len() < 127 && sts.iter().all(|&s| s < 0x80) {
        let hdr_len = 1 + sts.len();
        let mut body_len = 0usize;
        for src in srcs {
            body_len += src.len();
        }
        out.clear();
        out.reserve(hdr_len + body_len);
        out.push(hdr_len as u8);
        for &st in sts {
            out.push(st as u8);
        }
        for src in srcs {
            match *src {
                RawRecField::Empty => {}
                RawRecField::Slice(s) => out.extend_from_slice(s),
                RawRecField::Inline8(ref buf, n) => out.extend_from_slice(&buf[..n as usize]),
            }
        }
        return;
    }

    let mut serial_bytes_len = 0usize;
    let mut body_len = 0usize;
    for (&st, &src) in sts.iter().zip(srcs.iter()) {
        serial_bytes_len += sqliters_types::varint_len(st);
        body_len += src.len();
    }
    let mut hdr_len = serial_bytes_len + sqliters_types::varint_len((serial_bytes_len + 1) as u64);
    if sqliters_types::varint_len(hdr_len as u64) + serial_bytes_len != hdr_len {
        hdr_len = serial_bytes_len + sqliters_types::varint_len((serial_bytes_len + 2) as u64);
    }
    let total = hdr_len + body_len;
    out.clear();
    out.reserve(total);
    sqliters_types::push_varint(out, hdr_len as u64);
    for &st in sts {
        sqliters_types::push_varint(out, st);
    }
    for src in srcs {
        match *src {
            RawRecField::Empty => {}
            RawRecField::Slice(s) => out.extend_from_slice(s),
            RawRecField::Inline8(ref buf, n) => out.extend_from_slice(&buf[..n as usize]),
        }
    }
}

#[inline(always)]
fn scalar_to_static_raw_field(v: &Value) -> Option<(u64, RawRecField<'static>)> {
    match v {
        Value::Null => Some((0, RawRecField::Empty)),
        Value::Integer(i) => Some(encode_i64_raw_field(*i)),
        Value::Real(r) => Some(if r.is_nan() {
            (0, RawRecField::Empty)
        } else if *r == 0.0 {
            (7, RawRecField::Inline8([0; 8], 8))
        } else {
            (7, RawRecField::Inline8(r.to_bits().to_be_bytes(), 8))
        }),
        _ => None,
    }
}

fn parse_raw_record_prefix<const N: usize>(
    raw_rec: &[u8],
    max_col: usize,
    rec_st: &mut [u64; N],
    rec_off: &mut [usize; N],
    rec_len: &mut [usize; N],
) -> Result<usize> {
    if raw_rec.is_empty() {
        return Ok(0);
    }
    let limit = max_col.min(N - 1);
    if let Some((hdr_len, st_word, swar_cols)) = sqliters_types::swar_decode_hdr(raw_rec)
        && (hdr_len <= 8 || limit < swar_cols)
    {
        let target = swar_cols.min(limit + 1);
        let mut body_pos = hdr_len;
        let mut ok = true;
        for i in 0..target {
            let st = ((st_word >> (i * 8)) & 0xff) as u8;
            let l = sqliters_types::swar_serial_len_1b(st);
            if (l & 0x80) != 0 {
                ok = false;
                break;
            }
            let len = usize::from(l);
            let next_b = body_pos + len;
            if next_b > raw_rec.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            rec_st[i] = u64::from(st);
            rec_off[i] = body_pos;
            rec_len[i] = len;
            body_pos = next_b;
        }
        if ok {
            return Ok(target);
        }
    }
    let (hdr_u64, mut hdr_pos) = sqliters_types::get_varint(raw_rec)?;
    let hdr_len = usize::try_from(hdr_u64)
        .ok()
        .filter(|&h| h <= raw_rec.len() && hdr_pos <= h)
        .ok_or_else(|| SqliteError::corrupt("record header out of bounds"))?;
    let mut body_pos = hdr_len;
    let mut parsed = 0usize;
    while parsed <= limit && hdr_pos < hdr_len {
        let (st, used) = sqliters_types::get_varint(&raw_rec[hdr_pos..hdr_len])?;
        let len = sqliters_types::serial_type_len(st)?;
        let next_b = body_pos
            .checked_add(len)
            .filter(|&nb| nb <= raw_rec.len())
            .ok_or_else(|| SqliteError::corrupt("record payload truncated"))?;
        rec_st[parsed] = st;
        rec_off[parsed] = body_pos;
        rec_len[parsed] = len;
        hdr_pos += used;
        body_pos = next_b;
        parsed += 1;
    }
    Ok(parsed)
}

#[inline]
fn exact_bits_value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Integer(x), Value::Integer(y)) => x == y,
        (Value::Real(x), Value::Real(y)) => {
            (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
        }
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Blob(x), Value::Blob(y)) => x == y,
        _ => false,
    }
}

fn execute_single_row_delete_plan(
    plan: &SingleRowDeletePlan,
    params: &[Value],
    scratch_rec: &mut Vec<u8>,
    scratch_old_rec: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<u64> {
    if bt.transaction_state() != TransactionState::Write {
        bt.begin_write()?;
    }
    let key_val = resolve_insert_val_source(&plan.key_src, params);
    let Some(target_rowid) = key_val.to_exact_rowid_i64() else {
        return Ok(0);
    };

    if plan.indexes.is_empty() {
        return Ok(u64::from(bt.delete_table(plan.root_pgno, target_rowid)?));
    }

    if bt.is_p3v2_utf8() && plan.max_needed_col.is_none_or(|m| m < 32) {
        let mut needed_mask = 0u32;
        for idx in &plan.indexes {
            for &slot in &idx.col_slots {
                if let Some(ci) = slot {
                    needed_mask |= 1u32 << ci;
                }
            }
        }
        let mut old_vals: [Value; 32] = std::array::from_fn(|_| Value::Null);
        let probe =
            bt.with_table_rowid_morsel_row(
                plan.root_pgno,
                target_rowid,
                |row_ref| match row_ref {
                    TableRowRef::Pax { view, row_idx } => {
                        let mut m = needed_mask;
                        while m != 0 {
                            let ci = m.trailing_zeros() as usize;
                            m &= m - 1;
                            old_vals[ci] = view.read_value(ci, row_idx)?.unwrap_or(Value::Null);
                        }
                        Ok(true)
                    }
                    TableRowRef::Raw(_) => Ok(false),
                },
            )?;
        match probe {
            None => return Ok(0),
            Some(true) => {
                let deleted = bt.delete_table(plan.root_pgno, target_rowid)?;
                debug_assert!(deleted);
                let enc = bt.encoding();
                let rid_val = Value::Integer(target_rowid);
                let mut idx_refs: [&Value; 17] = [&Value::Null; 17];
                for idx in &plan.indexes {
                    let k_len = idx.col_slots.len();
                    for (k, &slot) in idx.col_slots.iter().enumerate() {
                        idx_refs[k] = match slot {
                            None => &rid_val,
                            Some(ci) => &old_vals[ci],
                        };
                    }
                    idx_refs[k_len] = &rid_val;
                    if let Some(n_keys) = sqliters_btree::orders_allow_pax_index(&idx.orders)
                        && let Some(entry) = sqliters_btree::value_refs_as_pax_index_entry(
                            &idx_refs[..=k_len],
                            n_keys,
                        )
                    {
                        bt.delete_index_pax_entry(idx.root_pgno, n_keys, entry, &idx.orders)?;
                    } else {
                        encode_value_refs_record_into(&idx_refs[..=k_len], enc, scratch_rec);
                        bt.delete_index_raw(idx.root_pgno, scratch_rec, &idx.orders)?;
                    }
                }
                return Ok(1);
            }
            Some(false) => {}
        }
    }

    if !bt.delete_table_returning_raw_into(plan.root_pgno, target_rowid, scratch_old_rec)? {
        return Ok(0);
    }

    let mut rec_st = [0u64; 32];
    let mut rec_off = [0usize; 32];
    let mut rec_len = [0usize; 32];
    let parsed_cols = if let Some(max_ci) = plan.max_needed_col {
        parse_raw_record_prefix(
            scratch_old_rec,
            max_ci,
            &mut rec_st,
            &mut rec_off,
            &mut rec_len,
        )?
    } else {
        0
    };

    let (rid_st, rid_src) = encode_i64_raw_field(target_rowid);
    let mut idx_st = [0u64; 16];
    let mut idx_src = [RawRecField::Empty; 16];
    for idx in &plan.indexes {
        let k_len = idx.col_slots.len();
        for (k, &slot) in idx.col_slots.iter().enumerate() {
            let (st, src) = match slot {
                None => (rid_st, rid_src),
                Some(ci) if ci < parsed_cols => {
                    let b = &scratch_old_rec[rec_off[ci]..rec_off[ci] + rec_len[ci]];
                    canonicalize_raw_utf8_field(rec_st[ci], b)
                }
                Some(_) => (0, RawRecField::Empty),
            };
            idx_st[k] = st;
            idx_src[k] = src;
        }
        idx_st[k_len] = rid_st;
        idx_src[k_len] = rid_src;
        pack_raw_fields_into(&idx_st[..=k_len], &idx_src[..=k_len], scratch_rec);
        bt.delete_index_raw(idx.root_pgno, scratch_rec, &idx.orders)?;
    }

    Ok(1)
}

fn execute_single_row_update_plan(
    plan: &SingleRowUpdatePlan,
    params: &[Value],
    scratch_rec: &mut Vec<u8>,
    scratch_old_rec: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<u64> {
    if bt.transaction_state() != TransactionState::Write {
        bt.begin_write()?;
    }
    let key_val = resolve_insert_val_source(&plan.key_src, params);
    let Some(target_rowid) = key_val.to_exact_rowid_i64() else {
        return Ok(0);
    };

    if bt.is_p3v2_utf8() && plan.n_cols <= 16 {
        let mut read_mask = 0u16;
        for a in &plan.assignments {
            if let SingleRowUpdateRhs::ColArith { src_col, .. } = a.rhs
                && plan.rowid_alias_col != Some(src_col)
            {
                read_mask |= 1u16 << src_col;
            }
        }
        for idx in &plan.affected_indexes {
            for &slot in &idx.col_slots {
                if let Some(ci) = slot {
                    read_mask |= 1u16 << ci;
                }
            }
        }

        let mut old_vals: [Value; 16] = std::array::from_fn(|_| Value::Null);
        let probe =
            bt.with_table_rowid_morsel_row(
                plan.root_pgno,
                target_rowid,
                |row_ref| match row_ref {
                    TableRowRef::Pax { view, row_idx } => {
                        let mut m = read_mask;
                        while m != 0 {
                            let ci = m.trailing_zeros() as usize;
                            m &= m - 1;
                            old_vals[ci] = view.read_value(ci, row_idx)?.unwrap_or(Value::Null);
                        }
                        Ok(Some((view.pgno(), row_idx)))
                    }
                    TableRowRef::Raw(_) => Ok(None),
                },
            )?;

        match probe {
            None => return Ok(0),
            Some(Some((leaf_pgno, row_idx))) => {
                let mut assigned_mask = 0u16;
                let mut new_vals: [Value; 16] = std::array::from_fn(|_| Value::Null);
                let mut scalar_updates: [(usize, Value); 16] =
                    std::array::from_fn(|_| (0usize, Value::Null));
                let mut all_scalar = true;

                for (ai, a) in plan.assignments.iter().enumerate() {
                    assigned_mask |= 1u16 << a.col_idx;
                    let final_v = match &a.rhs {
                        SingleRowUpdateRhs::Value(src) => {
                            let raw_val = resolve_insert_val_source(src, params);
                            let v = if affinity_is_noop_for_insert(a.affinity, raw_val) {
                                raw_val.clone()
                            } else {
                                a.affinity.apply(raw_val.clone())
                            };
                            match v {
                                Value::Real(r) if r.is_nan() => Value::Null,
                                other => other,
                            }
                        }
                        SingleRowUpdateRhs::ColArith { src_col, op, rhs } => {
                            let rhs_val = resolve_insert_val_source(rhs, params);
                            let lhs_ref = if plan.rowid_alias_col == Some(*src_col) {
                                None
                            } else {
                                Some(&old_vals[*src_col])
                            };
                            let computed = match (lhs_ref, rhs_val) {
                                (None, Value::Integer(delta)) => match op {
                                    Opcode::Add => target_rowid
                                        .checked_add(*delta)
                                        .map(Value::Integer)
                                        .unwrap_or_else(|| {
                                            eval_arith(*op, &Value::Integer(target_rowid), rhs_val)
                                        }),
                                    Opcode::Subtract => target_rowid
                                        .checked_sub(*delta)
                                        .map(Value::Integer)
                                        .unwrap_or_else(|| {
                                            eval_arith(*op, &Value::Integer(target_rowid), rhs_val)
                                        }),
                                    _ => eval_arith(*op, &Value::Integer(target_rowid), rhs_val),
                                },
                                (Some(Value::Integer(lhs_i)), Value::Integer(delta)) => match op {
                                    Opcode::Add => (*lhs_i)
                                        .checked_add(*delta)
                                        .map(Value::Integer)
                                        .unwrap_or_else(|| {
                                            eval_arith(*op, lhs_ref.unwrap(), rhs_val)
                                        }),
                                    Opcode::Subtract => (*lhs_i)
                                        .checked_sub(*delta)
                                        .map(Value::Integer)
                                        .unwrap_or_else(|| {
                                            eval_arith(*op, lhs_ref.unwrap(), rhs_val)
                                        }),
                                    _ => eval_arith(*op, lhs_ref.unwrap(), rhs_val),
                                },
                                (None, _) => {
                                    eval_arith(*op, &Value::Integer(target_rowid), rhs_val)
                                }
                                (Some(lv), _) => eval_arith(*op, lv, rhs_val),
                            };
                            let v = if affinity_is_noop_for_insert(a.affinity, &computed) {
                                computed
                            } else {
                                a.affinity.apply(computed)
                            };
                            match v {
                                Value::Real(r) if r.is_nan() => Value::Null,
                                other => other,
                            }
                        }
                    };

                    if final_v.is_null()
                        && let Some(ref msg) = a.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }

                    if matches!(final_v, Value::Null | Value::Integer(_) | Value::Real(_)) {
                        scalar_updates[ai] = (a.col_idx, final_v.clone());
                    } else {
                        all_scalar = false;
                    }
                    new_vals[a.col_idx] = final_v;
                }

                if !plan.affected_indexes.is_empty() {
                    let enc = bt.encoding();
                    let rid_val = Value::Integer(target_rowid);
                    let mut old_refs: [&Value; 17] = [&Value::Null; 17];
                    let mut new_refs: [&Value; 17] = [&Value::Null; 17];
                    for idx in &plan.affected_indexes {
                        let k_len = idx.col_slots.len();
                        let mut key_changed = false;
                        for (k, &slot) in idx.col_slots.iter().enumerate() {
                            match slot {
                                None => {
                                    old_refs[k] = &rid_val;
                                    new_refs[k] = &rid_val;
                                }
                                Some(ci) => {
                                    let ov = &old_vals[ci];
                                    let nv = if (assigned_mask & (1u16 << ci)) != 0 {
                                        &new_vals[ci]
                                    } else {
                                        ov
                                    };
                                    if !exact_bits_value_eq(ov, nv) {
                                        key_changed = true;
                                    }
                                    old_refs[k] = ov;
                                    new_refs[k] = nv;
                                }
                            }
                        }
                        if !key_changed {
                            continue;
                        }
                        old_refs[k_len] = &rid_val;
                        new_refs[k_len] = &rid_val;
                        let pax_n_keys = sqliters_btree::orders_allow_pax_index(&idx.orders);

                        if let Some(n_keys) = pax_n_keys
                            && let Some(old_entry) = sqliters_btree::value_refs_as_pax_index_entry(
                                &old_refs[..=k_len],
                                n_keys,
                            )
                        {
                            bt.delete_index_pax_entry(
                                idx.root_pgno,
                                n_keys,
                                old_entry,
                                &idx.orders,
                            )?;
                        } else {
                            encode_value_refs_record_into(&old_refs[..=k_len], enc, scratch_rec);
                            bt.delete_index_raw(idx.root_pgno, scratch_rec, &idx.orders)?;
                        }

                        if let Some(n_keys) = pax_n_keys
                            && let Some(new_entry) = sqliters_btree::value_refs_as_pax_index_entry(
                                &new_refs[..=k_len],
                                n_keys,
                            )
                        {
                            bt.insert_index_pax_entry(
                                idx.root_pgno,
                                n_keys,
                                new_entry,
                                &idx.orders,
                            )?;
                        } else {
                            encode_value_refs_record_into(&new_refs[..=k_len], enc, scratch_rec);
                            bt.insert_index(idx.root_pgno, scratch_rec, &idx.orders)?;
                        }
                    }
                }

                if all_scalar
                    && bt.try_update_table_pax_scalars_at(
                        plan.root_pgno,
                        leaf_pgno,
                        row_idx,
                        target_rowid,
                        &scalar_updates[..plan.assignments.len()],
                    )?
                {
                    return Ok(1);
                }

                // Fallback when updating a `TEXT`/`BLOB` column or widening a PAX scalar mode:
                // read only the remaining unmodified non-IPK columns and replace the PAX row!
                let unread_mask = ((1u32 << plan.n_cols) - 1) as u16 & !assigned_mask & !read_mask;
                if unread_mask != 0 {
                    bt.with_table_rowid_morsel_row(plan.root_pgno, target_rowid, |row_ref| {
                        if let TableRowRef::Pax { view, row_idx: r } = row_ref {
                            let mut m = unread_mask;
                            while m != 0 {
                                let ci = m.trailing_zeros() as usize;
                                m &= m - 1;
                                if plan.rowid_alias_col != Some(ci) {
                                    old_vals[ci] = view.read_value(ci, r)?.unwrap_or(Value::Null);
                                }
                            }
                        }
                        Ok(())
                    })?;
                }
                let mut full_refs: [&Value; 16] = [&Value::Null; 16];
                for ci in 0..plan.n_cols {
                    full_refs[ci] = if plan.rowid_alias_col == Some(ci) {
                        &Value::Null
                    } else if (assigned_mask & (1u16 << ci)) != 0 {
                        &new_vals[ci]
                    } else {
                        &old_vals[ci]
                    };
                }
                bt.insert_table_pax_refs(
                    plan.root_pgno,
                    target_rowid,
                    &full_refs[..plan.n_cols],
                    true,
                    scratch_rec,
                )?;
                return Ok(1);
            }
            Some(None) => {}
        }
    }

    let Some(local_span) =
        bt.read_table_rowid_into_with_local_span(plan.root_pgno, target_rowid, scratch_old_rec)?
    else {
        return Ok(0);
    };

    let n_cols = plan.n_cols;
    let mut rec_st = [0u64; 16];
    let mut rec_off = [0usize; 16];
    let mut rec_len = [0usize; 16];
    let parsed_cols = parse_raw_record_prefix(
        scratch_old_rec,
        n_cols.saturating_sub(1),
        &mut rec_st,
        &mut rec_off,
        &mut rec_len,
    )?;

    // Encode assigned values directly into `(new_st, new_src)` without initializing or dropping
    // a 16-slot `Value` array on the common integer/real/null/noop-affinity path (`W08`, `W14`).
    let mut coerced_opt: Option<Vec<Value>> = None;
    let mut col_to_ai = [0u8; 16];
    let mut deferred_mask = 0u16;
    let mut assigned_mask = 0u16;
    let mut new_st = [0u64; 16];
    let mut new_src = [RawRecField::Empty; 16];

    for (ai, a) in plan.assignments.iter().enumerate() {
        assigned_mask |= 1u16 << a.col_idx;
        match &a.rhs {
            SingleRowUpdateRhs::Value(src) => {
                let raw_val = resolve_insert_val_source(src, params);
                if affinity_is_noop_for_insert(a.affinity, raw_val) {
                    if raw_val.is_null()
                        && let Some(ref msg) = a.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }
                    let (st, rf) = value_to_utf8_raw_field(raw_val);
                    new_st[a.col_idx] = st;
                    new_src[a.col_idx] = rf;
                } else {
                    let v = a.affinity.apply(raw_val.clone());
                    if v.is_null()
                        && let Some(ref msg) = a.notnull_msg
                    {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            msg.clone(),
                        ));
                    }
                    if let Some((st, rf)) = scalar_to_static_raw_field(&v) {
                        new_st[a.col_idx] = st;
                        new_src[a.col_idx] = rf;
                    } else {
                        let cvec = coerced_opt
                            .get_or_insert_with(|| vec![Value::Null; plan.assignments.len()]);
                        cvec[ai] = v;
                        col_to_ai[a.col_idx] = ai as u8;
                        deferred_mask |= 1u16 << a.col_idx;
                    }
                }
            }
            SingleRowUpdateRhs::ColArith { src_col, op, rhs } => {
                let rhs_val = resolve_insert_val_source(rhs, params);
                let computed = if let Value::Integer(delta) = rhs_val {
                    let fast_lhs = if plan.rowid_alias_col == Some(*src_col) {
                        Some(target_rowid)
                    } else if *src_col < parsed_cols {
                        let st = rec_st[*src_col];
                        let b = &scratch_old_rec
                            [rec_off[*src_col]..rec_off[*src_col] + rec_len[*src_col]];
                        if (1..=6).contains(&st) || st == 8 || st == 9 {
                            Some(decode_raw_int_field(st, b))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(lhs_i) = fast_lhs {
                        let ov = match op {
                            Opcode::Add => lhs_i.checked_add(*delta),
                            Opcode::Subtract => lhs_i.checked_sub(*delta),
                            _ => None,
                        };
                        if let Some(res_i) = ov {
                            Value::Integer(res_i)
                        } else {
                            eval_arith(*op, &Value::Integer(lhs_i), rhs_val)
                        }
                    } else {
                        let lhs_val = if plan.rowid_alias_col == Some(*src_col) {
                            Value::Integer(target_rowid)
                        } else if *src_col < parsed_cols {
                            let b = &scratch_old_rec
                                [rec_off[*src_col]..rec_off[*src_col] + rec_len[*src_col]];
                            sqliters_types::decode_serial_value(rec_st[*src_col], b)?
                        } else {
                            Value::Null
                        };
                        eval_arith(*op, &lhs_val, rhs_val)
                    }
                } else {
                    let lhs_val = if plan.rowid_alias_col == Some(*src_col) {
                        Value::Integer(target_rowid)
                    } else if *src_col < parsed_cols {
                        let b = &scratch_old_rec
                            [rec_off[*src_col]..rec_off[*src_col] + rec_len[*src_col]];
                        sqliters_types::decode_serial_value(rec_st[*src_col], b)?
                    } else {
                        Value::Null
                    };
                    eval_arith(*op, &lhs_val, rhs_val)
                };
                let v = if affinity_is_noop_for_insert(a.affinity, &computed) {
                    computed
                } else {
                    a.affinity.apply(computed)
                };
                if v.is_null()
                    && let Some(ref msg) = a.notnull_msg
                {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        msg.clone(),
                    ));
                }
                if let Some((st, rf)) = scalar_to_static_raw_field(&v) {
                    new_st[a.col_idx] = st;
                    new_src[a.col_idx] = rf;
                } else {
                    let cvec = coerced_opt
                        .get_or_insert_with(|| vec![Value::Null; plan.assignments.len()]);
                    cvec[ai] = v;
                    col_to_ai[a.col_idx] = ai as u8;
                    deferred_mask |= 1u16 << a.col_idx;
                }
            }
        }
    }

    if let Some(ref cvec) = coerced_opt {
        for ci in 0..n_cols {
            if (deferred_mask & (1u16 << ci)) != 0 {
                let (st, rf) = value_to_utf8_raw_field(&cvec[col_to_ai[ci] as usize]);
                new_st[ci] = st;
                new_src[ci] = rf;
            }
        }
    }

    for ci in 0..n_cols {
        if plan.rowid_alias_col == Some(ci) {
            new_st[ci] = 0;
            new_src[ci] = RawRecField::Empty;
        } else if (assigned_mask & (1u16 << ci)) != 0 {
            // Already populated in `new_st[ci]` / `new_src[ci]`.
        } else if ci < parsed_cols {
            let b = &scratch_old_rec[rec_off[ci]..rec_off[ci] + rec_len[ci]];
            let (st, src) = canonicalize_raw_utf8_field(rec_st[ci], b);
            new_st[ci] = st;
            new_src[ci] = src;
        } else {
            new_st[ci] = 0;
            new_src[ci] = RawRecField::Empty;
        }
    }

    if !plan.affected_indexes.is_empty() {
        let (rid_st, rid_src) = encode_i64_raw_field(target_rowid);
        let mut idx_st = [0u64; 16];
        let mut idx_src = [RawRecField::Empty; 16];
        for idx in &plan.affected_indexes {
            let k_len = idx.col_slots.len();
            let mut key_changed = false;
            for (k, &slot) in idx.col_slots.iter().enumerate() {
                let (st, src) = match slot {
                    None => (rid_st, rid_src),
                    Some(ci) if ci < parsed_cols => {
                        let b = &scratch_old_rec[rec_off[ci]..rec_off[ci] + rec_len[ci]];
                        canonicalize_raw_utf8_field(rec_st[ci], b)
                    }
                    Some(_) => (0, RawRecField::Empty),
                };
                if let Some(ci) = slot
                    && (st != new_st[ci] || src.as_bytes() != new_src[ci].as_bytes())
                {
                    key_changed = true;
                }
                idx_st[k] = st;
                idx_src[k] = src;
            }
            if !key_changed {
                continue;
            }
            idx_st[k_len] = rid_st;
            idx_src[k_len] = rid_src;
            pack_raw_fields_into(&idx_st[..=k_len], &idx_src[..=k_len], scratch_rec);
            bt.delete_index_raw(idx.root_pgno, scratch_rec, &idx.orders)?;

            for (k, &slot) in idx.col_slots.iter().enumerate() {
                let (st, src) = match slot {
                    None => (rid_st, rid_src),
                    Some(ci) => (new_st[ci], new_src[ci]),
                };
                idx_st[k] = st;
                idx_src[k] = src;
            }
            idx_st[k_len] = rid_st;
            idx_src[k_len] = rid_src;
            pack_raw_fields_into(&idx_st[..=k_len], &idx_src[..=k_len], scratch_rec);
            bt.insert_index(idx.root_pgno, scratch_rec, &idx.orders)?;
        }
    }

    pack_raw_fields_into(&new_st[..n_cols], &new_src[..n_cols], scratch_rec);
    if let Some((leaf_pgno, p_start, p_end)) = local_span
        && usize::from(p_end - p_start) == scratch_rec.len()
    {
        bt.overwrite_table_leaf_span_in_place(
            plan.root_pgno,
            leaf_pgno,
            p_start,
            p_end,
            scratch_rec,
        )?;
    } else {
        bt.insert_table(plan.root_pgno, target_rowid, scratch_rec)?;
    }
    Ok(1)
}

#[allow(clippy::too_many_arguments)]
fn try_fast_read_plan_fields(
    hid: HandleId,
    shared: &Arc<ConnectionShared>,
    inner: &mut ConnectionInner,
    category: Option<StmtCategory>,
    expired: bool,
    compiled_advisory_expire_generation: u64,
    compiled_schema_generation: u64,
    compiled_v1_expire_generation: u64,
    compiled_slot_cookies: &[(SchemaInstanceId, u32, bool)],
    fast_plan: Option<&fast_read::FastReadPlan>,
    params: &[Value],
    lifecycle: &mut StatementLifecycle,
) -> Option<Result<StepResult>> {
    if category != Some(StmtCategory::Read)
        || expired
        || compiled_advisory_expire_generation != inner.advisory_expire_generation
        || compiled_schema_generation != inner.schema_generation
        || compiled_v1_expire_generation != inner.v1_expire_generation
        || inner.progress.is_enabled()
        || inner.simulated_cleanup_io_error.is_some()
        || inner.db_set.vtab_registry().has_any_in_tx()
    {
        return None;
    }
    let fast_plan = fast_plan?;
    if fast_plan.db_id != DatabaseId::MAIN {
        return None;
    }
    let main_slot = inner.db_set.main_slot();
    if !main_slot.schema.loaded || main_slot.btree.is_none() {
        return None;
    }
    let main_inst = main_slot.schema_instance_id;
    let main_cookie = main_slot.schema.schema_cookie;
    let main_had_pages = main_slot.schema.had_pages;
    if inner.db_set.main_btree().transaction_state() != TransactionState::Open {
        let bt = inner.db_set.main_btree();
        let has_pages = bt.pager().page_count() > 0;
        let cur_cookie = bt.pager().header().schema_cookie;
        if main_had_pages != has_pages || main_cookie != cur_cookie {
            return None;
        }
    }
    if slot_cookies_changed_slice(compiled_slot_cookies, inner) {
        return None;
    }

    let started_here = inner.db_set.main_btree().transaction_state() == TransactionState::Open;
    if started_here {
        let res = {
            let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
            inner.busy_policy.retry_btree(bt, |b| b.begin_read())
        };
        if let Err(e) = res {
            *lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            return Some(Err(e));
        }
        let bt = inner.db_set.main_btree();
        let has_pages = bt.pager().page_count() > 0;
        let cur_cookie = bt.pager().header().schema_cookie;
        if has_pages != main_had_pages || cur_cookie != main_cookie {
            if !inner.in_user_btree_tx && inner.is_autocommit() {
                let sum = inner.activity.summary_excluding(hid, &inner.db_set);
                if (sum.other_read_db_mask & 1) == 0
                    && (sum.other_write_db_mask & 1) == 0
                    && sum.other_writers == 0
                    && let Some(ref mut bt_mut) = inner.db_set.slots_mut()[0].btree
                    && bt_mut.transaction_state() == TransactionState::Read
                {
                    let _ = bt_mut.pager_mut().end_read_if_unused();
                }
            }
            return None;
        }
    }

    if let Err(e) = inner.check_any_interrupted() {
        if !inner.in_user_btree_tx && inner.is_autocommit() {
            let sum = inner.activity.summary_excluding(hid, &inner.db_set);
            if (sum.other_read_db_mask & 1) == 0
                && (sum.other_write_db_mask & 1) == 0
                && sum.other_writers == 0
                && let Some(ref mut bt) = inner.db_set.slots_mut()[0].btree
                && bt.transaction_state() == TransactionState::Read
            {
                let _ = bt.pager_mut().end_read_if_unused();
            }
        }
        *lifecycle = StatementLifecycle::Halted {
            last_error: Some(e.clone()),
        };
        return Some(Err(e));
    }

    let enc = inner.effective_encoding();
    let exec_res = {
        let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
        fast_plan.execute(bt, params, enc)
    };

    match exec_res {
        Ok(fast_read::FastReadExecOut::Empty) => {
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            } else if !inner.in_user_btree_tx {
                let sum = inner.activity.summary_excluding(hid, &inner.db_set);
                if (sum.other_read_db_mask & 1) == 0
                    && (sum.other_write_db_mask & 1) == 0
                    && sum.other_writers == 0
                    && let Some(ref mut bt) = inner.db_set.slots_mut()[0].btree
                    && bt.transaction_state() == TransactionState::Read
                    && let Err(e) = bt.pager_mut().end_read_if_unused()
                {
                    let p_err = inner.poison_with(e);
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(p_err.clone()),
                    };
                    return Some(Err(p_err));
                }
            }
            *lifecycle = StatementLifecycle::Halted { last_error: None };
            Some(Ok(StepResult::Done))
        }
        Ok(fast_read::FastReadExecOut::SingleInlineRow(ir)) => {
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            inner.activity.start_single_read(hid, main_inst);
            *lifecycle =
                StatementLifecycle::Running(RunningExec::PointSelectRowYielded { main_inst });
            Some(Ok(StepResult::Row(Row::from_inline(ir))))
        }
        Ok(fast_read::FastReadExecOut::SingleRow(vals)) => {
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            inner.activity.start_single_read(hid, main_inst);
            *lifecycle =
                StatementLifecycle::Running(RunningExec::PointSelectRowYielded { main_inst });
            Some(Ok(StepResult::Row(Row::new(vals))))
        }
        Ok(out_multi) => {
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            let read_instances = SmallInstVec::single(main_inst);
            inner.activity.start_with_instances(
                hid,
                true,
                false,
                read_instances.clone(),
                SmallInstVec::new(),
            );
            let mut lease_cursors: Vec<(SchemaInstanceId, CursorId)> =
                Vec::with_capacity(fast_plan.lease_roots.len());
            for &root in fast_plan.lease_roots.iter() {
                let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
                let page_cnt = bt.pager().page_count();
                let cid = bt.open_table_cursor(root, false);
                if page_cnt >= root
                    && let Err(e) = bt.cursor_first(cid)
                {
                    lease_cursors.push((main_inst, cid));
                    let _ = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadAborted {
                            lease_cursors,
                            read_instances,
                        },
                        shared,
                    );
                    *lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Some(Err(e));
                }
                lease_cursors.push((main_inst, cid));
            }

            match out_multi {
                fast_read::FastReadExecOut::InlineRows(rows) => {
                    let mut iter = rows.into_iter();
                    let first = iter.next().expect("InlineRows has >= 2 elements");
                    *lifecycle = StatementLifecycle::Running(RunningExec::MaterializedInlineRead {
                        rows: iter,
                        lease_cursors,
                        read_instances,
                    });
                    Some(Ok(StepResult::Row(Row::from_inline(first))))
                }
                fast_read::FastReadExecOut::Rows(rows) => {
                    let mut iter = rows.into_iter();
                    let first = iter.next().expect("Rows has >= 2 elements");
                    *lifecycle = StatementLifecycle::Running(RunningExec::MaterializedRead {
                        rows: iter,
                        lease_cursors,
                        read_instances,
                    });
                    Some(Ok(StepResult::Row(Row::new(first))))
                }
                fast_read::FastReadExecOut::SingleTextArena { arena, spans } => {
                    let (off0, len0) = spans[0];
                    let first_row = if len0 == u32::MAX {
                        Row::from_inline(InlineRow::single(InlineVal::Null))
                    } else {
                        let s = &arena[off0 as usize..(off0 + len0) as usize];
                        if let Some(iv) = InlineVal::try_short_text(s) {
                            Row::from_inline(InlineRow::single(iv))
                        } else {
                            Row::new(vec![Value::Text(s.to_owned())])
                        }
                    };
                    *lifecycle = StatementLifecycle::Running(RunningExec::FastSingleTextArena {
                        arena,
                        spans,
                        pos: 1,
                        lease_cursors,
                        read_instances,
                    });
                    Some(Ok(StepResult::Row(first_row)))
                }
                _ => unreachable!(),
            }
        }
        Err(e) => {
            if !inner.in_user_btree_tx && inner.is_autocommit() {
                let sum = inner.activity.summary_excluding(hid, &inner.db_set);
                if (sum.other_read_db_mask & 1) == 0
                    && (sum.other_write_db_mask & 1) == 0
                    && sum.other_writers == 0
                    && let Some(ref mut bt) = inner.db_set.slots_mut()[0].btree
                    && bt.transaction_state() == TransactionState::Read
                {
                    let _ = bt.pager_mut().end_read_if_unused();
                }
            }
            *lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            Some(Err(e))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn try_fast_compiled_main_write_fields(
    hid: HandleId,
    shared: &Arc<ConnectionShared>,
    inner: &mut ConnectionInner,
    category: Option<StmtCategory>,
    expired: bool,
    compiled_advisory_expire_generation: u64,
    compiled_schema_generation: u64,
    compiled_v1_expire_generation: u64,
    compiled_slot_cookies: &[(SchemaInstanceId, u32, bool)],
    prog: Option<&VdbeProgram>,
    params: &[Value],
    lifecycle: &mut StatementLifecycle,
    write_vm_scratch: &mut sqliters_vdbe::VdbeWriteScratch,
) -> Option<Result<StepResult>> {
    if category != Some(StmtCategory::Write)
        || expired
        || compiled_advisory_expire_generation != inner.advisory_expire_generation
        || compiled_schema_generation != inner.schema_generation
        || compiled_v1_expire_generation != inner.v1_expire_generation
        || !inner.activity.leases.is_empty()
        || inner.authorizer.is_some()
        || inner.trace_hook.is_some()
        || inner.count_changes
        || inner.writable_schema
        || inner.mutation_pipeline.has_update_hook()
        || inner.mutation_pipeline.is_trace_enabled()
        || !inner.db_set.open_blobs().is_empty()
        || inner.db_set.vtab_registry().has_any_in_tx()
    {
        return None;
    }
    let prog = prog?;
    if prog.primary_write_db() != DatabaseId::MAIN || prog.referenced_db_mask() != 1u16 {
        return None;
    }
    if !inner.db_set.main_slot().schema.loaded {
        return None;
    }
    if inner.foreign_keys
        && inner
            .db_set
            .slots()
            .iter()
            .any(|s| s.schema.tables.iter().any(|t| !t.foreign_keys.is_empty()))
    {
        return None;
    }

    let main_inst = inner.db_set.main_slot().schema_instance_id;
    let main_cookie = inner.db_set.main_slot().schema.schema_cookie;
    let main_had_pages = inner.db_set.main_slot().schema.had_pages;
    if inner.db_set.main_btree().transaction_state() != TransactionState::Open {
        let bt = inner.db_set.main_btree();
        let has_pages = bt.pager().page_count() > 0;
        let cur_cookie = bt.pager().header().schema_cookie;
        if main_had_pages != has_pages || main_cookie != cur_cookie {
            return None;
        }
    }
    if slot_cookies_changed_slice(compiled_slot_cookies, inner) {
        return None;
    }

    let base_sp_depth = inner.savepoints.len();
    let prev_tx_state = inner.db_set.main_btree().transaction_state();
    let opened_write_here = prev_tx_state != TransactionState::Write;
    if opened_write_here {
        let res = {
            let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
            inner.busy_policy.retry_btree(bt, |b| {
                b.pager_mut()
                    .begin_write_with_savepoint_depth(base_sp_depth)
            })
        };
        if let Err(e) = res {
            *lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            return Some(Err(e));
        }
        let bt = inner.db_set.main_btree();
        let has_pages = bt.pager().page_count() > 0;
        let cur_cookie = bt.pager().header().schema_cookie;
        if cur_cookie != main_cookie || has_pages != main_had_pages {
            let retain_read = prev_tx_state == TransactionState::Read;
            if let Some(ref mut bt_mut) = inner.db_set.slots_mut()[0].btree
                && let Err(rb_err) =
                    bt_mut.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, true, retain_read)
            {
                let p_err = inner.poison_with(rb_err);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(p_err.clone()),
                };
                return Some(Err(p_err));
            }
            return None;
        }
    } else if base_sp_depth > 0
        && let Err(e) = inner.db_set.slots_mut()[0]
            .btree
            .as_mut()
            .unwrap()
            .pager_mut()
            .ensure_subsavepoints(base_sp_depth)
    {
        *lifecycle = StatementLifecycle::Halted {
            last_error: Some(e.clone()),
        };
        return Some(Err(e));
    }

    let is_autocommit = inner.is_autocommit();
    let can_elide_sp = !prog.needs_stmt_subsavepoint()
        && (!prog.has_function_call() || !inner.functions.has_user_functions())
        && !inner.progress.is_enabled()
        && !shared.has_external_interrupt.load(Ordering::Acquire)
        && !inner.active_interrupt_flag.load(Ordering::Relaxed)
        && inner.db_set.main_slot().max_page_count >= SQLITE_MAX_PAGE_COUNT;
    let stmt_sp_slot = if !is_autocommit {
        if can_elide_sp {
            inner.in_user_btree_tx = true;
            Some(ELIDED_STMT_SP)
        } else {
            let sp_idx = inner.savepoints.len();
            if let Err(e) = inner.db_set.slots_mut()[0]
                .btree
                .as_mut()
                .unwrap()
                .pager_mut()
                .ensure_subsavepoints(sp_idx + 1)
            {
                if opened_write_here {
                    let retain_read = prev_tx_state == TransactionState::Read;
                    if let Some(ref mut bt_mut) = inner.db_set.slots_mut()[0].btree
                        && let Err(rb_err) = bt_mut.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            true,
                            retain_read,
                        )
                    {
                        let p_err = inner.poison_with(rb_err);
                        *lifecycle = StatementLifecycle::Halted {
                            last_error: Some(p_err.clone()),
                        };
                        return Some(Err(p_err));
                    }
                }
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(e.clone()),
                };
                return Some(Err(e));
            }
            inner.in_user_btree_tx = true;
            Some(sp_idx)
        }
    } else {
        None
    };

    let rt_before = inner.runtime;
    let mutation_len_before = inner.mutation_pipeline.len();
    let schema_gen_before = inner.schema_generation;
    let v1_expire_gen_before = inner.v1_expire_generation;
    let schema_changed_before = inner.schema_changed_in_tx;

    if let Err(e) = inner.check_any_interrupted() {
        let snap = WriteAbortSnapshot {
            stmt_sp_slot,
            touched_write_instances: SmallInstVec::single(main_inst),
            started_read_instances: SmallInstVec::new(),
            fk_before: inner.deferred_fk_suspects.clone(),
            rt_before,
            is_dml: true,
            mutation_len_before,
            schemas_before: Vec::new(),
            schema_gen_before,
            v1_expire_gen_before,
            schema_changed_before,
        };
        let final_err = inner.abort_write_statement(hid, &e, snap, shared);
        *lifecycle = StatementLifecycle::Halted {
            last_error: Some(final_err.clone()),
        };
        return Some(Err(final_err));
    }

    let (mut run_res, new_last_rowid, new_changes) = if let Some(ins_plan) =
        prog.single_row_insert()
        && (!ins_plan.has_builtin_jsonb || !inner.functions.has_user_functions())
        && !inner.progress.is_enabled()
    {
        let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
        let (scratch_rec, scratch_jb) = write_vm_scratch.scratch_buffers_mut();
        match execute_single_row_insert_plan(ins_plan, params, scratch_rec, scratch_jb, bt) {
            Ok(rid) => {
                inner
                    .progress
                    .add_ticks_without_handler(prog.ops.len() as u64);
                (Ok(()), rid, 1u64)
            }
            Err(e) => (Err(e), inner.runtime.last_insert_rowid, 0u64),
        }
    } else if let Some(upd_plan) = prog.single_row_update()
        && !inner.progress.is_enabled()
        && inner.effective_encoding() == TextEncoding::Utf8
    {
        let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
        let (scratch_rec, scratch_old) = write_vm_scratch.scratch_buffers_mut();
        match execute_single_row_update_plan(upd_plan, params, scratch_rec, scratch_old, bt) {
            Ok(changed) => {
                inner
                    .progress
                    .add_ticks_without_handler(prog.ops.len() as u64);
                (Ok(()), inner.runtime.last_insert_rowid, changed)
            }
            Err(e) => (Err(e), inner.runtime.last_insert_rowid, 0u64),
        }
    } else if let Some(del_plan) = prog.single_row_delete()
        && !inner.progress.is_enabled()
        && inner.effective_encoding() == TextEncoding::Utf8
    {
        let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
        let (scratch_rec, scratch_old) = write_vm_scratch.scratch_buffers_mut();
        match execute_single_row_delete_plan(del_plan, params, scratch_rec, scratch_old, bt) {
            Ok(changed) => {
                inner
                    .progress
                    .add_ticks_without_handler(prog.ops.len() as u64);
                (Ok(()), inner.runtime.last_insert_rowid, changed)
            }
            Err(e) => (Err(e), inner.runtime.last_insert_rowid, 0u64),
        }
    } else {
        let prev_ts = inner.runtime.stmt_timestamp_ms;
        let stmt_ts = if prev_ts.is_some() {
            prev_ts
        } else if prog.needs_stmt_timestamp() {
            Some(
                inner
                    .db_set
                    .main_btree()
                    .pager()
                    .vfs()
                    .current_unix_millis(),
            )
        } else {
            None
        };
        let enc = inner.effective_encoding();
        let mut vm_rt = VmRuntimeContext {
            last_insert_rowid: inner.runtime.last_insert_rowid,
            last_changes: 0,
            total_changes: 0,
            stmt_timestamp_ms: stmt_ts,
            enable_test_functions: inner.runtime.enable_test_functions,
        };
        let svc_rt = vm_rt;
        let r = {
            let mut exec_svc = ExecutionContext {
                interrupt_flag: &inner.active_interrupt_flag,
                runtime: &svc_rt,
                encoding: enc,
                functions: &inner.functions,
                collations: &inner.collations,
                progress: &mut inner.progress,
                strict_subtype: inner.strict_subtype,
            };
            sqliters_vdbe::VdbeVm::run_write_program(
                prog,
                params,
                &mut vm_rt,
                write_vm_scratch,
                &mut inner.db_set,
                &mut exec_svc,
            )
        };
        (r, vm_rt.last_insert_rowid, vm_rt.last_changes)
    };

    if new_last_rowid != inner.runtime.last_insert_rowid {
        inner.runtime.last_insert_rowid = new_last_rowid;
    }
    if run_res.is_ok() {
        let slot = inner.db_set.main_slot();
        if let Some(ref bt) = slot.btree
            && bt.pager().page_count() > slot.max_page_count
        {
            run_res = Err(SqliteError::new(
                ErrorCode::Full,
                "database or disk is full",
            ));
        }
    }
    if let Err(err) = run_res {
        let snap = WriteAbortSnapshot {
            stmt_sp_slot,
            touched_write_instances: SmallInstVec::single(main_inst),
            started_read_instances: SmallInstVec::new(),
            fk_before: inner.deferred_fk_suspects.clone(),
            rt_before,
            is_dml: true,
            mutation_len_before,
            schemas_before: Vec::new(),
            schema_gen_before,
            v1_expire_gen_before,
            schema_changed_before,
        };
        let final_err = inner.abort_write_statement(hid, &err, snap, shared);
        *lifecycle = StatementLifecycle::Halted {
            last_error: Some(final_err.clone()),
        };
        return Some(Err(final_err));
    }

    inner.runtime.last_changes = new_changes;
    inner.runtime.total_changes += new_changes;

    if !is_autocommit && inner.simulated_cleanup_io_error.is_none() {
        if let Some(sp_slot) = stmt_sp_slot
            && let Some(ref mut bt) = inner.db_set.slots_mut()[0].btree
            && bt.transaction_state() == TransactionState::Write
        {
            if sp_slot != ELIDED_STMT_SP
                && let Err(e) = bt.pager_mut().release_subsavepoint(sp_slot)
            {
                let p_err = inner.poison_with(e);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(p_err.clone()),
                };
                return Some(Err(p_err));
            }
            if let Err(e) = bt.spill_if_over_cache_size() {
                let p_err = inner.poison_with(e);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(p_err.clone()),
                };
                return Some(Err(p_err));
            }
        }
        inner.publish(shared);
        *lifecycle = StatementLifecycle::Halted { last_error: None };
        return Some(Ok(StepResult::Done));
    }

    if is_autocommit
        && inner.simulated_cleanup_io_error.is_none()
        && inner.deferred_fk_suspects.is_empty()
        && !inner.commit_hooks.has_commit_hook()
        && inner.db_set.slot_count() == 2
        && inner.db_set.slots()[1].btree.is_none()
    {
        let fast_commit: Result<()> = {
            let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
            if bt.transaction_state() == TransactionState::Write {
                inner
                    .busy_policy
                    .retry_btree(bt, Btree::acquire_exclusive_lock)
                    .and_then(|()| bt.commit_phase_one(None))
                    .and_then(|()| bt.commit_phase_two(false, false))
            } else {
                bt.commit_phase_two(false, false)
            }
        };
        if let Ok(()) = fast_commit {
            inner.schema_changed_in_tx = false;
            inner.publish(shared);
            inner.maybe_run_wal_autocheckpoint();
            *lifecycle = StatementLifecycle::Halted { last_error: None };
            return Some(Ok(StepResult::Done));
        } else if let Err(cm_err) = fast_commit {
            let mut expire_self_unused = false;
            let mut stmt_tx_unused = StmtTxTracker::default();
            let rb_res = inner.with_exec_ctx(
                shared,
                ActivitySummary::default(),
                &mut stmt_tx_unused,
                &mut expire_self_unused,
                |ctx| rollback_all_databases(ctx, false),
            );
            if let Err(rb_err) = rb_res {
                let root_err = if cm_err.code == ErrorCode::Busy {
                    rb_err
                } else {
                    cm_err
                };
                inner.deferred_fk_suspects.clear();
                inner.schema_changed_in_tx = false;
                inner.publish(shared);
                let p_err = inner.poison_with(root_err);
                *lifecycle = StatementLifecycle::Halted {
                    last_error: Some(p_err.clone()),
                };
                return Some(Err(p_err));
            }
            inner.runtime.last_changes = 0;
            inner.runtime.total_changes = rt_before.total_changes;
            inner.runtime.last_insert_rowid = rt_before.last_insert_rowid;
            inner.mutation_pipeline.truncate(mutation_len_before);
            inner.deferred_fk_suspects.clear();
            inner.schema_changed_in_tx = false;
            inner.publish(shared);
            *lifecycle = StatementLifecycle::Halted {
                last_error: Some(cm_err.clone()),
            };
            return Some(Err(cm_err));
        }
    }

    let snap = WriteAbortSnapshot {
        stmt_sp_slot,
        touched_write_instances: SmallInstVec::single(main_inst),
        started_read_instances: SmallInstVec::new(),
        fk_before: inner.deferred_fk_suspects.clone(),
        rt_before,
        is_dml: true,
        mutation_len_before,
        schemas_before: Vec::new(),
        schema_gen_before,
        v1_expire_gen_before,
        schema_changed_before,
    };
    match inner.finish_activity(
        hid,
        ActivityFinish::WriteCompleted {
            abort_snapshot: snap,
        },
        shared,
    ) {
        Ok(()) => {
            *lifecycle = StatementLifecycle::Halted { last_error: None };
            Some(Ok(StepResult::Done))
        }
        Err(e) => {
            *lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            Some(Err(e))
        }
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        let Some(hid) = self.handle_id.take() else {
            return;
        };
        let prev = std::mem::replace(&mut self.lifecycle, StatementLifecycle::Ready);
        let running = match prev {
            StatementLifecycle::Running(run) => Some(Box::new(run)),
            StatementLifecycle::Ready | StatementLifecycle::Halted { .. } => None,
        };
        self.shared.enqueue_or_run_drop(PendingDrop::Statement {
            handle_id: hid,
            running,
        });
    }
}

// ============================================================================
// AST Bind Parameter Discovery
// ============================================================================

fn collect_stmt_params(stmt: &Stmt, max_idx: &mut usize, named: &mut HashMap<String, usize>) {
    match stmt {
        Stmt::Explain { stmt: inner, .. } => collect_stmt_params(inner, max_idx, named),
        Stmt::Vacuum {
            into: Some(expr), ..
        } => collect_expr_params(expr, max_idx, named),
        Stmt::AttachDatabase {
            expr,
            schema_name,
            key,
        } => {
            collect_expr_params(expr, max_idx, named);
            collect_expr_params(schema_name, max_idx, named);
            if let Some(k) = key {
                collect_expr_params(k, max_idx, named);
            }
        }
        Stmt::DetachDatabase { schema_name } => {
            collect_expr_params(schema_name, max_idx, named);
        }
        Stmt::Select(sel) => collect_select_params(sel, max_idx, named),
        Stmt::CreateTableAs { query, .. } => collect_select_params(query, max_idx, named),
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
            for uc in &ins.upsert_clauses {
                if let Some(target) = &uc.target {
                    for ic in &target.columns {
                        if let Some(e) = &ic.expr {
                            collect_expr_params(e, max_idx, named);
                        }
                    }
                    if let Some(w) = &target.where_clause {
                        collect_expr_params(w, max_idx, named);
                    }
                }
                if let UpsertAction::DoUpdate {
                    assignments,
                    where_clause,
                } = &uc.action
                {
                    for assign in assignments {
                        collect_expr_params(&assign.expr, max_idx, named);
                    }
                    if let Some(w) = where_clause {
                        collect_expr_params(w, max_idx, named);
                    }
                }
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
            for assign in &upd.assignments {
                collect_expr_params(&assign.expr, max_idx, named);
            }
            if let Some(fc) = &upd.from {
                collect_from_params(fc, max_idx, named);
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

fn collect_from_params(fc: &FromClause, max_idx: &mut usize, named: &mut HashMap<String, usize>) {
    match &fc.head {
        TableOrSubquery::Subquery { query, .. } => collect_select_params(query, max_idx, named),
        TableOrSubquery::TableFunction { args, .. } => {
            for a in args {
                collect_expr_params(a, max_idx, named);
            }
        }
        TableOrSubquery::Table { .. } => {}
    }
    for j in &fc.joins {
        match &j.right {
            TableOrSubquery::Subquery { query, .. } => {
                collect_select_params(query, max_idx, named);
            }
            TableOrSubquery::TableFunction { args, .. } => {
                for a in args {
                    collect_expr_params(a, max_idx, named);
                }
            }
            TableOrSubquery::Table { .. } => {}
        }
        if let Some(sqliters_syntax::JoinConstraint::On(e)) = &j.constraint {
            collect_expr_params(e, max_idx, named);
        }
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

fn collect_core_params(core: &SelectCore, max_idx: &mut usize, named: &mut HashMap<String, usize>) {
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
            windows,
            ..
        } => {
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_params(expr, max_idx, named);
                }
            }
            if let Some(fc) = from {
                collect_from_params(fc, max_idx, named);
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
            for (_, ws) in windows {
                collect_window_spec_params(ws, max_idx, named);
            }
        }
    }
}

fn collect_expr_params(expr: &Expr, max_idx: &mut usize, named: &mut HashMap<String, usize>) {
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
        Expr::InTable { expr: v, .. } => {
            collect_expr_params(v, max_idx, named);
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
        Expr::FunctionCall {
            args, filter, over, ..
        } => {
            if let FunctionArgs::Exprs(es) = args {
                for a in es {
                    collect_expr_params(a, max_idx, named);
                }
            }
            if let Some(f) = filter {
                collect_expr_params(f, max_idx, named);
            }
            if let Some(WindowRef::Spec(ws)) = over {
                collect_window_spec_params(ws, max_idx, named);
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

fn collect_window_spec_params(
    ws: &WindowSpec,
    max_idx: &mut usize,
    named: &mut HashMap<String, usize>,
) {
    for pe in &ws.partition_by {
        collect_expr_params(pe, max_idx, named);
    }
    for ob in &ws.order_by {
        collect_expr_params(&ob.expr, max_idx, named);
    }
    if let Some(ref wf) = ws.frame {
        for b in [&wf.start, &wf.end] {
            if let WindowFrameBound::Preceding(e) | WindowFrameBound::Following(e) = b
                && window::is_window_bound_constant(e)
            {
                collect_expr_params(e, max_idx, named);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqliters_vfs::MemoryVfs;

    #[test]
    fn multi_db_attach_detach_temp_ddl_dml_tx_and_pragmas_smoke() {
        let vfs = Arc::new(MemoryVfs::new());
        let opts = ConnectionOpenOptions {
            uri: true,
            ..ConnectionOpenOptions::default()
        };
        let conn = Connection::open_with_vfs_and_options(
            vfs.clone(),
            Some(Path::new("/main.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            opts,
        )
        .unwrap();

        assert_eq!(conn.db_filename("main"), Some(PathBuf::from("/main.db")));
        assert_eq!(conn.db_filename("temp"), None);
        assert_eq!(conn.attached_limit(), 10);
        assert_eq!(conn.set_attached_limit(-1), 10);
        assert_eq!(conn.set_attached_limit(2), 10);
        assert_eq!(conn.attached_limit(), 2);

        // Unopened `temp` cannot be attached over ("already in use"), and DETACH before open says "no such database"
        assert_eq!(
            conn.execute("ATTACH ':memory:' AS TEMP;")
                .unwrap_err()
                .message,
            "database TEMP is already in use"
        );
        assert_eq!(
            conn.execute("DETACH Temp;").unwrap_err().message,
            "no such database: Temp"
        );
        assert_eq!(
            conn.execute("DETACH Main;").unwrap_err().message,
            "cannot detach database Main"
        );

        // Lazy temp open + TEMP shadowing main + attached
        conn.execute(
            "CREATE TABLE shared(src TEXT, v INT);
             INSERT INTO shared VALUES ('main', 1);
             CREATE TEMP TABLE shared(src TEXT, v INT);
             INSERT INTO shared VALUES ('temp', 2);",
        )
        .unwrap();
        assert_eq!(
            conn.execute("DETACH Temp;").unwrap_err().message,
            "cannot detach database Temp"
        );

        // 9000-char URI with mode=memory + disk-backed aux1 via VFS
        let long_uri = format!("file:/{}/memdb?mode=memory", "a".repeat(9000));
        let mut att = conn.prepare("ATTACH ?1 AS aux_mem;").unwrap();
        att.bind(1, Value::Text(long_uri)).unwrap();
        assert_eq!(att.step().unwrap(), StepResult::Done);
        // ATTACH expires itself on V1 if re-stepped? On V2 prepare() it reprepares cleanly; check attached_limit(2):
        conn.execute("ATTACH '/aux1.db' AS aux1;").unwrap();
        assert_eq!(
            conn.execute("ATTACH ':memory:' AS over_limit;")
                .unwrap_err()
                .message,
            "too many attached databases - max 2"
        );
        conn.set_attached_limit(10);
        assert_eq!(conn.db_filename("aux1"), Some(PathBuf::from("/aux1.db")));
        assert_eq!(conn.db_filename("aux_mem"), None);

        // Per-DB AUTOINCREMENT & cross-DB query/DML + strict intra-DB FK isolation
        conn.execute(
            "CREATE TABLE aux1.items(id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT UNIQUE);
             INSERT INTO aux1.items(label) VALUES ('a1'), ('a2');
             CREATE TABLE aux_mem.mirror(id INT, src TEXT);
             INSERT INTO aux_mem.mirror SELECT v, src FROM main.shared;",
        )
        .unwrap();
        let seq_cnt = conn
            .query_row(
                "SELECT seq FROM aux1.sqlite_sequence WHERE name='items';",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(seq_cnt.get_i64(0), Some(2));

        // Shadow resolution: unqualified `shared` hits `temp`, `main.shared` hits `main`
        assert_eq!(
            conn.query_row("SELECT src FROM shared;", &[])
                .unwrap()
                .unwrap()
                .get_str(0),
            Some("temp")
        );
        assert_eq!(
            conn.query_row("SELECT src FROM main.shared;", &[])
                .unwrap()
                .unwrap()
                .get_str(0),
            Some("main")
        );

        // Idle/reset Statement does NOT block DETACH, active stepped Statement DOES block DETACH with SQLITE_ERROR!
        let mut s_aux = conn
            .prepare("SELECT id, label FROM aux1.items ORDER BY id;")
            .unwrap();
        assert!(matches!(s_aux.step().unwrap(), StepResult::Row(_)));
        let det_locked = conn.execute("DETACH aux1;").unwrap_err();
        assert_eq!(det_locked.code, ErrorCode::Error);
        assert_eq!(det_locked.message, "database aux1 is locked");
        s_aux.reset().unwrap();

        // Slot collapse: DETACH aux_mem (slot 2) shifts aux1 from slot 3 -> slot 2 seamlessly!
        conn.execute("DETACH aux_mem;").unwrap();
        let rows = s_aux.collect_rows().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_str(1), Some("a1"));

        // Multi-DB BEGIN + subsavepoint statement abort preserves outer transaction across both DBs!
        conn.execute("BEGIN;").unwrap();
        conn.execute("INSERT INTO main.shared VALUES ('main2', 10);")
            .unwrap();
        conn.execute("INSERT INTO aux1.items(label) VALUES ('a3');")
            .unwrap();
        //Untouched newly attached DB inside BEGIN can be detached, touched aux1 cannot!
        conn.execute("ATTACH ':memory:' AS transient_db;").unwrap();
        conn.execute("DETACH transient_db;").unwrap();
        assert_eq!(
            conn.execute("DETACH aux1;").unwrap_err().message,
            "database aux1 is locked"
        );
        // Failing multi-row INSERT on aux1 rolls back ONLY this statement on aux1, keeping 'main2' and 'a3' alive!
        let dup_err = conn
            .execute("INSERT INTO aux1.items(label) VALUES ('a4_temp'), ('a1');")
            .unwrap_err();
        assert_eq!(dup_err.code, ErrorCode::Constraint);
        assert!(!conn.is_autocommit());
        conn.execute("COMMIT;").unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM main.shared;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(2)
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM aux1.items;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(3)
        );

        // Commit hook returning true aborts with SQLITE_CONSTRAINT_COMMITHOOK and rolls back BOTH DBs atomically!
        conn.commit_hook(Some(Box::new(|| true))).unwrap();
        conn.execute("BEGIN; INSERT INTO main.shared VALUES ('hook_m', 99); INSERT INTO aux1.items(label) VALUES ('hook_a');")
            .unwrap();
        let hk_err = conn.execute("COMMIT;").unwrap_err();
        assert_eq!(hk_err.code, ErrorCode::Constraint);
        assert_eq!(
            hk_err.extended_code,
            sqliters_types::extended::SQLITE_CONSTRAINT_COMMITHOOK
        );
        assert!(conn.is_autocommit());
        conn.commit_hook(None).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM main.shared;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(2)
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM aux1.items;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(3)
        );

        // Pragmas & VACUUM aux1 + VACUUM aux1 INTO
        let db_list = conn.query("PRAGMA database_list;", &[]).unwrap();
        assert_eq!(db_list.rows.len(), 3); // main (0), temp (1), aux1 (2)
        let lock_st = conn.query("PRAGMA lock_status;", &[]).unwrap();
        assert_eq!(lock_st.rows.len(), 3);
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(ic.rows[0].get_str(0), Some("ok"));
        conn.execute("VACUUM aux1;").unwrap();
        conn.execute("VACUUM aux1 INTO '/aux1_copy.db';").unwrap();
        conn.execute("ATTACH '/aux1_copy.db' AS copy_db;").unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM copy_db.items;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(3)
        );
    }

    #[test]
    fn test_morsel_row_inline_sso_and_heap_parity() {
        // Test 0..=8 columns and 0..=64-byte ASCII + multi-byte UTF-8 strings across all Row getters
        for ncols in 0..=8usize {
            for str_len in [0usize, 1, 15, 21, 22, 23, 32, 64] {
                let s = if str_len >= 4 {
                    let mut u = String::from("ç🦀"); // 6 bytes
                    while u.len() < str_len {
                        u.push('x');
                    }
                    while !u.is_char_boundary(str_len) {
                        u.push('y');
                    }
                    u[..str_len].to_owned()
                } else {
                    "z".repeat(str_len)
                };
                let vals: Vec<Value> = (0..ncols)
                    .map(|ci| match ci % 5 {
                        0 => Value::Text(s.clone()),
                        1 => Value::Integer(ci as i64 * 101 - 50),
                        2 => Value::real(ci as f64 * 1.25),
                        3 => Value::Blob(vec![0x5Au8; str_len]),
                        _ => Value::Null,
                    })
                    .collect();

                let row_from_slice = Row::from_slice(&vals);
                let row_heap = Row::new(vals.clone());
                assert_eq!(row_from_slice.len(), ncols);
                assert_eq!(row_from_slice.is_empty(), ncols == 0);
                assert_eq!(row_from_slice, row_heap);
                assert_eq!(row_from_slice.clone(), row_heap);

                for (ci, v) in vals.iter().enumerate() {
                    assert_eq!(row_from_slice.get_str(ci), v.as_str());
                    let want_blob = match v {
                        Value::Blob(b) => Some(b.as_slice()),
                        _ => None,
                    };
                    assert_eq!(row_from_slice.get_blob(ci), want_blob);
                    assert_eq!(row_from_slice.get_i64(ci), v.as_i64());
                    assert_eq!(row_from_slice.get_f64(ci), v.as_f64());
                    assert_eq!(row_from_slice.get(ci), Some(v));
                    assert_eq!(&row_from_slice[ci], v);
                }
                assert!(row_from_slice.get_ref(ncols).is_none());
                assert_eq!(row_from_slice.values(), vals.as_slice());
                assert_eq!(row_from_slice.into_values(), vals);
            }
        }
    }

    #[test]
    fn test_morsel_jsonb_scratch_rtree_savepoint_and_single_row_dml() {
        let conn = Connection::open_in_memory().unwrap();

        // 1. JSONB scratch reuse (`SingleRowInsertPlan` jsonb(?2) + jsonb('const'), `->>` grouping, `jsonb_set`, `json_each`)
        conn.execute_batch(
            "CREATE TABLE jdocs(id INTEGER PRIMARY KEY, doc BLOB NOT NULL, meta BLOB NOT NULL);",
        )
        .unwrap();
        let mut ins = conn
            .prepare("INSERT INTO jdocs VALUES (?1, jsonb(?2), jsonb('{\"fixed\":true}'))")
            .unwrap();
        for i in 1..=60i64 {
            let tier = if i % 2 == 0 { "gold" } else { "silver" };
            ins.bind(1, i).unwrap();
            ins.bind(
                2,
                format!("{{\"tier\":\"{tier}\",\"amt\":{},\"arr\":[1,2,3]}}", i * 10),
            )
            .unwrap();
            assert_eq!(ins.step().unwrap(), StepResult::Done);
            ins.reset().unwrap();
        }
        ins.finalize().unwrap();

        let grp = conn
            .query(
                "SELECT doc ->> '$.tier' AS t, count(*), sum(CAST(doc ->> '$.amt' AS INTEGER)) \
                 FROM jdocs GROUP BY t ORDER BY t",
                &[],
            )
            .unwrap();
        assert_eq!(grp.rows.len(), 2);
        assert_eq!(grp.rows[0].get_str(0), Some("gold"));
        assert_eq!(grp.rows[0].get_i64(1), Some(30));
        assert_eq!(grp.rows[0].get_i64(2), Some(9300));
        assert_eq!(grp.rows[1].get_str(0), Some("silver"));
        assert_eq!(grp.rows[1].get_i64(1), Some(30));
        assert_eq!(grp.rows[1].get_i64(2), Some(9000));

        conn.execute_batch("UPDATE jdocs SET doc = jsonb_set(doc, '$.amt', 999) WHERE id <= 5;")
            .unwrap();
        let je_sum = conn
            .query_row(
                "SELECT sum(CAST(je.value AS INTEGER)) FROM jdocs, json_each(jdocs.doc, '$.arr') je WHERE jdocs.id <= 10",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(je_sum.get_i64(0), Some(60));

        // 2. SingleRowUpdatePlan (`Value` + `ColArith` `col + ?`, `col - ?`, shrinking/growing, NOT NULL, secondary index) & SingleRowDeletePlan & ArenaIndexEntry ties
        conn.execute_batch(
            "CREATE TABLE accounts(
                id INTEGER PRIMARY KEY,
                grp INTEGER NOT NULL,
                bal INTEGER NOT NULL,
                tag TEXT NOT NULL,
                note TEXT NOT NULL
            );
            INSERT INTO accounts VALUES
                (1, 10, 100, 'A-01', 'short'),
                (2, 10, 100, 'A-01', 'medium-length-note-payload'),
                (3, 20, 250, 'B-02', 'another-note');
            CREATE INDEX idx_acc_grp_bal ON accounts(grp, bal);
            CREATE INDEX idx_acc_tag ON accounts(tag);",
        )
        .unwrap();

        let mut upd_arith = conn
            .prepare("UPDATE accounts SET bal = bal + ?1, tag = ?2 WHERE id = ?3")
            .unwrap();
        upd_arith.bind(1, 55i64).unwrap();
        upd_arith.bind(2, "Z-99").unwrap();
        upd_arith.bind(3, 1i64).unwrap();
        assert_eq!(upd_arith.step().unwrap(), StepResult::Done);
        upd_arith.reset().unwrap();

        // Missing rowid is a clean 0-change no-op
        upd_arith.bind(1, 10i64).unwrap();
        upd_arith.bind(2, "NOOP").unwrap();
        upd_arith.bind(3, 999i64).unwrap();
        assert_eq!(upd_arith.step().unwrap(), StepResult::Done);
        assert_eq!(conn.changes(), 0);

        // NOT NULL constraint violation aborts cleanly without corrupting indexes
        upd_arith.reset().unwrap();
        upd_arith.bind(1, Value::Null).unwrap();
        upd_arith.bind(2, "Z-99").unwrap();
        upd_arith.bind(3, 1i64).unwrap();
        assert_eq!(upd_arith.step().unwrap_err().code, ErrorCode::Constraint);
        assert_eq!(
            upd_arith.finalize().unwrap_err().code,
            ErrorCode::Constraint
        );

        let mut del_stmt = conn.prepare("DELETE FROM accounts WHERE id = ?1").unwrap();
        del_stmt.bind(1, 2i64).unwrap();
        assert_eq!(del_stmt.step().unwrap(), StepResult::Done);
        assert_eq!(conn.changes(), 1);
        del_stmt.finalize().unwrap();

        assert_eq!(
            conn.query_row("SELECT bal, tag FROM accounts WHERE tag = 'Z-99'", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(155)
        );
        assert_eq!(
            conn.query_row("PRAGMA integrity_check;", &[])
                .unwrap()
                .unwrap()
                .get_str(0),
            Some("ok")
        );

        // 3. R-Tree decoded node cache, splits, savepoint rollback, duplicate rowid abort, and `rtreecheck`
        conn.execute_batch(
            "CREATE VIRTUAL TABLE boxes USING rtree(id, minX, maxX, minY, maxY);
             BEGIN;",
        )
        .unwrap();
        for id in 1..=120i64 {
            let x = (id % 15) as f64 * 4.0;
            let y = (id / 15) as f64 * 4.0;
            conn.execute_params(
                "INSERT INTO boxes VALUES (?1, ?2, ?3, ?4, ?5)",
                &[
                    Value::Integer(id),
                    Value::real(x),
                    Value::real(x + 3.0),
                    Value::real(y),
                    Value::real(y + 3.0),
                ],
            )
            .unwrap();
        }
        // Duplicate rowid inside BEGIN..COMMIT aborts statement only and leaves tree consistent
        let dup = conn
            .execute("INSERT INTO boxes VALUES (50, 0, 1, 0, 1);")
            .unwrap_err();
        assert_eq!(dup.code, ErrorCode::Constraint);

        // Nested SAVEPOINT with additional splits + deletes, then ROLLBACK TO!
        conn.execute_batch("SAVEPOINT sp_rtree;").unwrap();
        for id in 121..=220i64 {
            let x = (id % 20) as f64 * 2.0;
            conn.execute_params(
                "INSERT INTO boxes VALUES (?1, ?2, ?3, ?4, ?5)",
                &[
                    Value::Integer(id),
                    Value::real(x),
                    Value::real(x + 5.0),
                    Value::real(0.0),
                    Value::real(5.0),
                ],
            )
            .unwrap();
        }
        conn.execute_batch(
            "DELETE FROM boxes WHERE id <= 30; ROLLBACK TO sp_rtree; RELEASE sp_rtree; COMMIT;",
        )
        .unwrap();

        assert_eq!(
            conn.query_row("SELECT count(*) FROM boxes;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(120)
        );
        assert_eq!(
            conn.query_row("SELECT rtreecheck('boxes');", &[])
                .unwrap()
                .unwrap()
                .get_str(0),
            Some("ok")
        );
        let hits = conn
            .query_row(
                "SELECT count(*) FROM boxes WHERE maxX >= 8.0 AND minX <= 16.0 AND maxY >= 4.0 AND minY <= 12.0;",
                &[],
            )
            .unwrap()
            .unwrap()
            .get_i64(0)
            .unwrap();
        assert!(hits > 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_pragma_mmap_size_lifecycle_grow_vacuum_truncate_and_active_lease() {
        // `:memory:` and `temp` return 0 rows with `mmap_size` column, matching SQLite 3.54.1.
        let mem_conn = Connection::open_in_memory().unwrap();
        let mem_q = mem_conn.query("PRAGMA mmap_size;", &[]).unwrap();
        assert_eq!(mem_q.column_names, vec!["mmap_size".to_owned()]);
        assert!(mem_q.rows.is_empty());
        let mem_set = mem_conn.query("PRAGMA mmap_size = 65536;", &[]).unwrap();
        assert_eq!(mem_set.column_names, vec!["mmap_size".to_owned()]);
        assert!(mem_set.rows.is_empty());

        let dir = std::env::temp_dir().join(format!(
            "sqliters_mmap_lifecycle_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("main.db");
        let aux_path = dir.join("aux.db");

        let conn = Connection::open(&db_path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA mmap_size;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(0)
        );
        // Set unqualified `mmap_size`: applies to `main` and is inherited by future `ATTACH`.
        assert_eq!(
            conn.query_row("PRAGMA mmap_size = 262144;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(262_144)
        );
        assert_eq!(
            conn.query_row("PRAGMA mmap_size = 9999999999999;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(sqliters_vfs::MAX_MMAP_SIZE as i64)
        );
        assert_eq!(
            conn.query_row("PRAGMA mmap_size = 524288;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(524_288)
        );

        // Grow file across many pages, close & reopen cold with `mmap_size` active to exercise
        // zero-copy mapped clean page reads + `VACUUM` shrink/truncate safety.
        conn.execute(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, payload BLOB);
             WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM seq WHERE x < 200)
             INSERT INTO items SELECT x, zeroblob(1500) FROM seq;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        conn.execute(&format!("ATTACH '{}' AS aux1;", aux_path.display()))
            .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA aux1.mmap_size;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(524_288)
        );
        // `temp` returns 0 rows.
        conn.execute("CREATE TEMP TABLE t_tmp(v INT);").unwrap();
        assert!(
            conn.query("PRAGMA temp.mmap_size;", &[])
                .unwrap()
                .rows
                .is_empty()
        );
        conn.try_close().unwrap();

        let reader = Connection::open(&db_path).unwrap();
        reader.execute("PRAGMA mmap_size = 67108864;").unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*), sum(length(payload)) FROM items;", &[])
                .unwrap()
                .unwrap()
                .get_i64(1),
            Some(300_000)
        );

        // Active read lease while `mmap_size > 0` preserves current limit (`mmap3-1.4`).
        let mut stmt = reader
            .prepare("SELECT id FROM items WHERE id BETWEEN 10 AND 15 ORDER BY id;")
            .unwrap();
        assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
        assert_eq!(
            reader
                .query_row("PRAGMA mmap_size = 0;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(67_108_864)
        );
        stmt.reset().unwrap();
        drop(stmt);

        // Once lease ends, `PRAGMA mmap_size = -1` resets to default `0`, and while `0` an active
        // lease DOES allow raising `mmap_size` (`mmap3-1.8`).
        assert_eq!(
            reader
                .query_row("PRAGMA mmap_size = -1;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(0)
        );
        let mut stmt2 = reader
            .prepare("SELECT id FROM items WHERE id BETWEEN 10 AND 15 ORDER BY id;")
            .unwrap();
        assert!(matches!(stmt2.step().unwrap(), StepResult::Row(_)));
        assert_eq!(
            reader
                .query_row("PRAGMA mmap_size = 131072;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(131_072)
        );
        stmt2.reset().unwrap();
        drop(stmt2);

        // Shrink via DELETE + VACUUM while `mmap_size > 0` must unmap cleanly before truncate.
        reader
            .execute("DELETE FROM items WHERE id > 5; VACUUM;")
            .unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM items;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0),
            Some(5)
        );
        reader.try_close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_gate_state_reentrancy_busy_and_concurrent_drop_drain() {
        let conn = Arc::new(Connection::open_in_memory().unwrap());
        conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v INTEGER); INSERT INTO t VALUES (1, 10), (2, 20);")
            .unwrap();

        let reentrant_stmt = Arc::new(Mutex::new(Some(
            conn.prepare("SELECT v FROM t WHERE id = ?1;").unwrap(),
        )));
        let cross_thread_stmt = Arc::new(Mutex::new(Some(
            conn.prepare("SELECT v FROM t WHERE id = 2;").unwrap(),
        )));
        // Advance `cross_thread_stmt` to `Running` (`PointSelectRowYielded`) before entering callback
        // so dropping it on another thread exercises active-lease cleanup via `GATE_PENDING_DROPS`.
        assert!(matches!(
            cross_thread_stmt
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .step()
                .unwrap(),
            StepResult::Row(_)
        ));

        let conn_for_cb = Arc::clone(&conn);
        let reent_for_cb = Arc::clone(&reentrant_stmt);
        let cross_for_cb = Arc::clone(&cross_thread_stmt);

        conn.register_scalar_function("gate_probe", 0, FunctionFlags::INNOCUOUS, move |_| {
            // 1. Same-thread re-entrant `execute` and `bind` MUST return `ErrorCode::Misuse`
            // (never bypassing via `can_fast_bind` while `GATE_BUSY` is held).
            let err_exec = conn_for_cb.execute("SELECT 1;").unwrap_err();
            assert_eq!(err_exec.code, ErrorCode::Misuse);
            {
                let mut g = reent_for_cb.lock().unwrap();
                let s = g.as_mut().unwrap();
                assert_eq!(s.bind(1, 1_i64).unwrap_err().code, ErrorCode::Misuse);
            }

            // 2. Cross-thread contention MUST return `ErrorCode::Busy`, and dropping a `Running`
            // `Statement` from another thread while `GATE_BUSY` is held MUST queue onto
            // `GATE_PENDING_DROPS` and drain cleanly on outer session exit.
            let conn_bg = Arc::clone(&conn_for_cb);
            let stmt_to_drop = cross_for_cb.lock().unwrap().take().unwrap();
            std::thread::spawn(move || {
                let busy_err = conn_bg.prepare("SELECT 1;").unwrap_err();
                assert_eq!(busy_err.code, ErrorCode::Busy);
                drop(stmt_to_drop);
            })
            .join()
            .unwrap();

            Ok(RuntimeValue::plain(Value::Integer(42)))
        })
        .unwrap();

        let probe_val = conn
            .query_row("SELECT gate_probe();", &[])
            .unwrap()
            .unwrap()
            .get_i64(0);
        assert_eq!(probe_val, Some(42));

        // Drop the remaining prepared statement and verify `try_close()` succeeds cleanly
        // (proving `cross_thread_stmt` was drained from `pending_drops` on session exit!).
        drop(reentrant_stmt.lock().unwrap().take());
        drop(reentrant_stmt);
        drop(cross_thread_stmt);
        // Clear registered closure holding `Arc<Connection>` so `Arc::try_unwrap` succeeds.
        conn.register_scalar_function("gate_probe", 0, FunctionFlags::INNOCUOUS, |_| {
            Ok(RuntimeValue::plain(Value::Integer(0)))
        })
        .unwrap();
        let owned = Arc::try_unwrap(conn).expect("unique Connection owner");
        owned.try_close().unwrap();
    }

    #[test]
    fn test_single_copy_memdb_and_lazy_builtin_registries() {
        let conn = Connection::open_in_memory().unwrap();

        // 1. Verify lazy `module_list` and `function_list` parity before any VTab/ext calls.
        let mods = conn.query("PRAGMA module_list;", &[]).unwrap();
        let mod_names: Vec<String> = mods
            .rows
            .iter()
            .map(|r| r.get_str(0).unwrap().to_owned())
            .collect();
        assert!(mod_names.contains(&"fts5".to_owned()));
        assert!(mod_names.contains(&"rtree".to_owned()));
        assert!(mod_names.contains(&"generate_series".to_owned()));

        let fns = conn
            .query("SELECT name, builtin FROM pragma_function_list WHERE name IN ('bm25','rtreedepth','fts5_source_id') ORDER BY name;", &[])
            .unwrap();
        assert_eq!(fns.rows.len(), 3);

        // 2. Exercise single-copy `:memory:` commit, rollback, `journal_mode=OFF` rollback,
        // and FTS5 `bm25()` / `highlight()` lazy auxiliary binding.
        conn.execute_batch(
            "CREATE TABLE kv(id INTEGER PRIMARY KEY, body TEXT);
             INSERT INTO kv VALUES (1, 'alpha'), (2, 'beta');
             BEGIN;
             INSERT INTO kv VALUES (3, 'gamma');
             ROLLBACK;
             PRAGMA journal_mode = OFF;
             BEGIN;
             INSERT INTO kv VALUES (4, 'delta');
             ROLLBACK;
             PRAGMA journal_mode = MEMORY;
             CREATE VIRTUAL TABLE docs USING fts5(title);
             INSERT INTO docs VALUES ('zero copy mmap window'), ('scatter gather pwritev');",
        )
        .unwrap();

        let cnt = conn
            .query_row("SELECT COUNT(*), SUM(id) FROM kv;", &[])
            .unwrap()
            .unwrap();
        // Row 3 rolled back under MEMORY journal; row 4 retained under journal_mode=OFF on MEMDB.
        assert_eq!(cnt.get_i64(0), Some(3));
        assert_eq!(cnt.get_i64(1), Some(7));

        let hl = conn
            .query_row(
                "SELECT highlight(docs, 0, '[', ']') FROM docs WHERE docs MATCH 'pwritev' ORDER BY bm25(docs);",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(hl.get_str(0), Some("scatter gather [pwritev]"));

        // 3. Cross-page-size `Backup` (`1024 -> 4096`) into `:memory:` destination.
        let src = Connection::open_in_memory().unwrap();
        src.execute_batch(
            "PRAGMA page_size = 1024;
             CREATE TABLE s(k INTEGER PRIMARY KEY, v TEXT);
             INSERT INTO s VALUES (10, 'ten'), (20, 'twenty');",
        )
        .unwrap();
        let dst = Connection::open_in_memory().unwrap();
        dst.execute_batch("CREATE TABLE dummy(x);").unwrap();
        {
            let mut backup = Backup::init(&src, "main", &dst, "main").unwrap();
            assert_eq!(backup.step(-1).unwrap(), BackupStepResult::Done);
            backup.finish().unwrap();
        }
        let ps_after = dst
            .query_row("PRAGMA page_size;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0);
        let sum_after = dst
            .query_row("SELECT SUM(k) FROM s;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0);
        assert_eq!(ps_after, Some(1024));
        assert_eq!(sum_after, Some(30));

        // 4. Unregistering / overriding lazy built-in extension scalar works identically.
        assert!(
            conn.unregister_scalar_function("fts5_source_id", 0)
                .unwrap()
        );
        assert!(conn.prepare("SELECT fts5_source_id();").is_err());

        conn.try_close().unwrap();
        src.try_close().unwrap();
        dst.try_close().unwrap();
    }

    #[test]
    fn test_p3v2_pax_column_modes_manifest_typing_and_page_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "sqliters_p3v2_modes_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("p3v2_modes.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE pax_t(
                    id INTEGER PRIMARY KEY,
                    c_null,
                    c_int,
                    c_real,
                    c_bytes,
                    c_mixed
                 );
                 CREATE INDEX idx_pax_1k ON pax_t(c_int);
                 CREATE INDEX idx_pax_2k ON pax_t(c_int, id);",
            )
            .unwrap();

            // Row 1: ALL_NULL, I32, F64 (-0.0), empty TEXT '', I32 in c_mixed
            conn.execute_params(
                "INSERT INTO pax_t VALUES (1, NULL, -42, ?1, '', 7);",
                &[Value::Real(-0.0)],
            )
            .unwrap();
            // Row 2: ALL_NULL, I32, F64 (NaN -> NULL), empty BLOB X'', I32 in c_mixed
            conn.execute_params(
                "INSERT INTO pax_t VALUES (2, NULL, 100, ?1, X'', 8);",
                &[Value::Real(f64::NAN)],
            )
            .unwrap();
            // Row 3: ALL_NULL, I64 promotion (> i32::MAX), F64, TEXT 'abc', I64 promotion in c_mixed
            let big_i64 = i64::from(i32::MAX) + 5000;
            conn.execute_params(
                "INSERT INTO pax_t VALUES (3, NULL, ?1, 3.25, 'abc', ?1);",
                &[Value::Integer(big_i64)],
            )
            .unwrap();
            // Row 4: ALL_NULL, NULL in c_int, F64, BLOB X'616263', MIXED promotion (TEXT in c_mixed!)
            conn.execute_params(
                "INSERT INTO pax_t VALUES (4, NULL, NULL, -12.5, X'616263', 'manifest');",
                &[],
            )
            .unwrap();
            // Row 5: Real + Blob in c_mixed within the same PAX leaf!
            conn.execute_params(
                "INSERT INTO pax_t VALUES (5, NULL, 100, 0.0, 'hello', X'deadbeef');",
                &[],
            )
            .unwrap();

            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );

            // Verify typeof() and exact values across all 6 column encodings
            let rows = conn
                .query(
                    "SELECT id, typeof(c_null), typeof(c_int), c_int, typeof(c_real), c_real,
                            typeof(c_bytes), length(c_bytes), typeof(c_mixed), c_mixed
                       FROM pax_t ORDER BY id;",
                    &[],
                )
                .unwrap()
                .rows;
            assert_eq!(rows.len(), 5);
            // Row 1: -0.0 preserves IEEE-754 sign bit in PAX_COL_F64, '' is text of len 0
            assert_eq!(rows[0].get_str(1), Some("null"));
            assert_eq!(rows[0].get_str(2), Some("integer"));
            assert_eq!(rows[0].get_i64(3), Some(-42));
            assert_eq!(rows[0].get_str(4), Some("real"));
            assert_eq!(rows[0].get_f64(5).unwrap().to_bits(), (-0.0f64).to_bits());
            assert_eq!(rows[0].get_str(6), Some("text"));
            assert_eq!(rows[0].get_i64(7), Some(0));
            assert_eq!(rows[0].get_str(8), Some("integer"));
            assert_eq!(rows[0].get_i64(9), Some(7));

            // Row 2: NaN canonicalized to NULL, X'' is blob of len 0
            assert_eq!(rows[1].get_str(4), Some("null"));
            assert_eq!(rows[1].get_str(6), Some("blob"));
            assert_eq!(rows[1].get_i64(7), Some(0));

            // Row 3 vs Row 4: 'abc' (text) vs X'616263' (blob) + I64 + MIXED text
            assert_eq!(rows[2].get_i64(3), Some(big_i64));
            assert_eq!(rows[2].get_str(6), Some("text"));
            assert_eq!(rows[3].get_str(2), Some("null"));
            assert_eq!(rows[3].get_str(6), Some("blob"));
            assert_eq!(rows[3].get_str(8), Some("text"));
            assert_eq!(rows[3].get_str(9), Some("manifest"));
            assert_eq!(rows[4].get_str(8), Some("blob"));

            // Verify 0x2a covering index range scan + NULL lookup + UPDATE/DELETE
            let idx_sum = conn
                .query_row(
                    "SELECT COUNT(*), SUM(id) FROM pax_t WHERE c_int >= -50;",
                    &[],
                )
                .unwrap()
                .unwrap();
            assert_eq!(idx_sum.get_i64(0), Some(4));
            assert_eq!(idx_sum.get_i64(1), Some(1 + 2 + 3 + 5));

            conn.execute("UPDATE pax_t SET c_int = c_int + 10 WHERE id = 2;")
                .unwrap();
            conn.execute("DELETE FROM pax_t WHERE id = 5;").unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            conn.try_close().unwrap();
        }

        // Inspect raw on-disk header magic (`b"P3v2"` at [72..76]), page 1 (`0x0d`), and PAX leaves (`0x2d` / `0x2a`)
        let raw_db = std::fs::read(&db_path).unwrap();
        assert_eq!(&raw_db[72..76], b"P3v2");
        assert_eq!(raw_db[100], sqliters_btree::BtreePageType::TableLeaf as u8);
        let page_types: Vec<u8> = (1..raw_db.len() / 4096).map(|p| raw_db[p * 4096]).collect();
        assert!(
            page_types.contains(&sqliters_btree::PAX_TABLE_LEAF_BYTE),
            "expected 0x2d PAX table leaf on disk, got {page_types:?}"
        );
        assert!(
            page_types.contains(&sqliters_btree::PAX_INDEX_LEAF_BYTE),
            "expected 0x2a PAX index leaf on disk, got {page_types:?}"
        );

        // Verify automatic 0x2a -> 0x0a fallback when a manifest-typed text value enters c_int!
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute("INSERT INTO pax_t VALUES (6, NULL, 'text_key_in_idx', 1.5, 'ok', 1);")
                .unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            let hit = conn
                .query_row("SELECT id FROM pax_t WHERE c_int = 'text_key_in_idx';", &[])
                .unwrap()
                .unwrap();
            assert_eq!(hit.get_i64(0), Some(6));
            conn.try_close().unwrap();
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_p3v2_pax_splits_merges_wide_2000_cols_and_100kib_overflow_coexistence() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mix_tbl(id INTEGER PRIMARY KEY, cat INT, score REAL, tag TEXT, payload BLOB);
             CREATE INDEX idx_mix_cat ON mix_tbl(cat, id);",
        )
        .unwrap();

        // Sequential & random inserts causing multiple 0x2d & 0x2a leaf splits + 115 KiB overflow rows!
        conn.execute("BEGIN;").unwrap();
        for i in 1..=260i64 {
            let rowid = if i % 2 == 0 { i * 10 } else { 5000 - i * 7 };
            let blob = if i == 50 || i == 180 {
                vec![0x5Au8; 115_000]
            } else {
                vec![(i & 0xff) as u8; 24]
            };
            conn.execute_params(
                "INSERT INTO mix_tbl VALUES (?1, ?2, ?3, ?4, ?5);",
                &[
                    Value::Integer(rowid),
                    Value::Integer(rowid.rem_euclid(17)),
                    Value::Real(rowid as f64 * 0.5),
                    Value::Text(format!("tag_{i:04}")),
                    Value::Blob(blob),
                ],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        let ovfl_len = conn
            .query_row(
                "SELECT SUM(length(payload)) FROM mix_tbl WHERE length(payload) > 100000;",
                &[],
            )
            .unwrap()
            .unwrap()
            .get_i64(0);
        assert_eq!(ovfl_len, Some(230_000));

        // Verify dbstat sees both PAX leaves and overflow pages cleanly
        let dbstat_cnt = conn
            .query_row(
                "SELECT COUNT(*) FROM dbstat WHERE name = 'mix_tbl' AND pagetype = 'leaf';",
                &[],
            )
            .unwrap()
            .unwrap()
            .get_i64(0)
            .unwrap();
        assert!(dbstat_cnt >= 3);

        // Delete 85% of rows to exercise 0x2d & 0x2a merges/rebalancing, then re-verify
        conn.execute("DELETE FROM mix_tbl WHERE cat != 3;").unwrap();
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );

        // 2000-column wide table escaping cleanly to 0x0d + overflow
        let cols_ddl = (0..2000)
            .map(|c| format!("c{c} INT"))
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(&format!("CREATE TABLE wide2000({cols_ddl});"))
            .unwrap();
        let vals_sql = (0..2000)
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        conn.execute(&format!("INSERT INTO wide2000 VALUES ({vals_sql});"))
            .unwrap();
        let wide_row = conn
            .query_row("SELECT c0, c999, c1999 FROM wide2000;", &[])
            .unwrap()
            .unwrap();
        assert_eq!(wide_row.get_i64(0), Some(0));
        assert_eq!(wide_row.get_i64(1), Some(999));
        assert_eq!(wide_row.get_i64(2), Some(1999));
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        conn.try_close().unwrap();
    }

    #[test]
    fn test_p3v2_pax_alter_vacuum_autovacuum_savepoint_wal_backup_blob_and_legacy_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "sqliters_p3v2_acid_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let main_db = dir.join("acid_pax.db");
        let vac_into_db = dir.join("vac_into_pax.db");
        let backup_db = dir.join("backup_pax.db");
        let legacy_db = dir.join("legacy_fmt3.db");

        // 1. WAL + INCREMENTAL autovacuum + SAVEPOINT + ALTER TABLE ADD/DROP COLUMN + incrblob + VACUUM + Backup
        {
            let conn = Connection::open(&main_db).unwrap();
            conn.execute_batch(
                "PRAGMA page_size = 4096;
                 PRAGMA auto_vacuum = INCREMENTAL;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE items(id INTEGER PRIMARY KEY, grp INT, v INT, raw BLOB, obsolete TEXT);
                 CREATE INDEX idx_items_grp ON items(grp, v);",
            )
            .unwrap();

            conn.execute("BEGIN;").unwrap();
            for i in 1..=220i64 {
                conn.execute_params(
                    "INSERT INTO items VALUES (?1, ?2, ?3, ?4, 'drop_me');",
                    &[
                        Value::Integer(i),
                        Value::Integer(i % 10),
                        Value::Integer(i * 3),
                        Value::Blob(vec![b'a'; 32]),
                    ],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();

            // SAVEPOINT rollback across PAX splits, updates, and deletes
            conn.execute_batch(
                "SAVEPOINT sp_pax;
                 UPDATE items SET v = v + 1000 WHERE grp = 2;
                 DELETE FROM items WHERE grp = 5;
                 INSERT INTO items VALUES (9999, 1, 999, X'0102', 'tmp');
                 ROLLBACK TO sp_pax;
                 RELEASE sp_pax;",
            )
            .unwrap();
            assert_eq!(
                conn.query_row("SELECT COUNT(*), SUM(v) FROM items;", &[])
                    .unwrap()
                    .unwrap()
                    .get_i64(0),
                Some(220)
            );

            // ALTER TABLE ADD COLUMN (NULL & DEFAULT) + DROP COLUMN
            conn.execute_batch(
                "ALTER TABLE items ADD COLUMN extra_null INT;
                 ALTER TABLE items ADD COLUMN extra_def TEXT DEFAULT 'varsayilan';
                 ALTER TABLE items DROP COLUMN obsolete;",
            )
            .unwrap();
            let alt_row = conn
                .query_row(
                    "SELECT extra_null, extra_def FROM items WHERE id = 15;",
                    &[],
                )
                .unwrap()
                .unwrap();
            assert_eq!(alt_row.get(0), Some(&Value::Null));
            assert_eq!(alt_row.get_str(1), Some("varsayilan"));

            // Incremental BLOB read/write directly on 0x2d PAX_COL_BYTES!
            {
                let mut bh = conn.blob_open("main", "items", "raw", 10, true).unwrap();
                assert_eq!(bh.len(), 32);
                let mut prefix = [0u8; 4];
                bh.read_at(&mut prefix, 0).unwrap();
                assert_eq!(&prefix, b"aaaa");
                bh.write_at(b"PAX!", 4).unwrap();
                bh.close().unwrap();
            }
            let blob_after = conn
                .query_row("SELECT raw FROM items WHERE id = 10;", &[])
                .unwrap()
                .unwrap();
            assert_eq!(&blob_after.get_blob(0).unwrap()[0..8], b"aaaaPAX!");

            // Delete bulk rows + PRAGMA incremental_vacuum relocating 0x2d/0x2a pages
            conn.execute_batch(
                "DELETE FROM items WHERE id > 40;
                 PRAGMA incremental_vacuum;",
            )
            .unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );

            // Online Backup (`main_db` -> `backup_db`) + `VACUUM INTO` + in-place `VACUUM`
            let dst = Connection::open(&backup_db).unwrap();
            {
                let mut b = Backup::init(&conn, "main", &dst, "main").unwrap();
                assert_eq!(b.step(-1).unwrap(), BackupStepResult::Done);
                b.finish().unwrap();
            }
            dst.try_close().unwrap();

            conn.execute_params(
                "VACUUM INTO ?1;",
                &[Value::Text(vac_into_db.to_string_lossy().into_owned())],
            )
            .unwrap();
            conn.execute("VACUUM;").unwrap();
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
            conn.try_close().unwrap();
        }

        for p in [&main_db, &vac_into_db, &backup_db] {
            let hdr_bytes = std::fs::read(p).unwrap();
            assert_eq!(&hdr_bytes[72..76], b"P3v2");
            let c = Connection::open(p).unwrap();
            assert_eq!(
                c.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            assert_eq!(
                c.query_row("SELECT COUNT(*) FROM items;", &[])
                    .unwrap()
                    .unwrap()
                    .get_i64(0),
                Some(40)
            );
            c.try_close().unwrap();
        }

        // 2. Corrupting a 0x2d PAX leaf header (`n_rows > cap`) must be caught by `PRAGMA integrity_check`!
        let corrupt_db = dir.join("corrupt_pax.db");
        std::fs::copy(&vac_into_db, &corrupt_db).unwrap();
        {
            let mut raw = std::fs::read(&corrupt_db).unwrap();
            let pax_pg = (1..raw.len() / 4096)
                .find(|&pg| raw[pg * 4096] == sqliters_btree::PAX_TABLE_LEAF_BYTE)
                .expect("expected 0x2d page in vac_into_db");
            let off = pax_pg * 4096;
            raw[off + 2..off + 4].copy_from_slice(&9999u16.to_le_bytes()); // n_rows = 9999 > cap
            std::fs::write(&corrupt_db, &raw).unwrap();

            let c = Connection::open(&corrupt_db).unwrap();
            let ic = c.query("PRAGMA integrity_check;", &[]).unwrap();
            assert_ne!(ic.rows[0].get_str(0), Some("ok"));
            c.try_close().unwrap();
        }

        // 3. Transparent Format-3 fallback when reopening a non-P3v2 `.db` WITHOUT LegacyFormat3Guard!
        {
            let _guard = sqliters_types::LegacyFormat3Guard::enter();
            let c = Connection::open(&legacy_db).unwrap();
            c.execute_batch(
                "CREATE TABLE leg(id INTEGER PRIMARY KEY, name TEXT, score INT);
                 CREATE INDEX idx_leg_score ON leg(score);
                 INSERT INTO leg VALUES (1, 'alpha', 10), (2, 'beta', 20);",
            )
            .unwrap();
            c.try_close().unwrap();
        }
        let leg_bytes = std::fs::read(&legacy_db).unwrap();
        assert_eq!(&leg_bytes[72..76], &[0u8; 4]);
        {
            // Reopen in default P3v2 process mode: header lacks b"P3v2", so Format-3 stays active!
            let c = Connection::open(&legacy_db).unwrap();
            c.execute(
                "INSERT INTO leg VALUES (3, 'gamma', 30); UPDATE leg SET score = 25 WHERE id = 2;",
            )
            .unwrap();
            assert_eq!(
                c.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            let sum = c
                .query_row("SELECT SUM(score) FROM leg;", &[])
                .unwrap()
                .unwrap()
                .get_i64(0);
            assert_eq!(sum, Some(65));
            c.try_close().unwrap();
        }
        let leg_bytes_after = std::fs::read(&legacy_db).unwrap();
        assert_eq!(&leg_bytes_after[72..76], &[0u8; 4]);
        assert!(
            !(1..leg_bytes_after.len() / 4096).any(|pg| {
                matches!(
                    leg_bytes_after[pg * 4096],
                    sqliters_btree::PAX_TABLE_LEAF_BYTE | sqliters_btree::PAX_INDEX_LEAF_BYTE
                )
            }),
            "Format-3 database must never emit 0x2d/0x2a pages"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
