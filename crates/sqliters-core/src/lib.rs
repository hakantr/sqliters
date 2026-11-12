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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqliters_btree::{Btree, BtreeCursorState, CursorId};
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
    VTabIndexInfo, VTabOrderBy, VTabRiskFlags, VTabUpdateOperation, VirtualCursor, VirtualTable,
    VirtualTableModule,
};
use sqliters_types::{
    ConflictResolutionEffect, DatabaseKind, ErrorCode, LockGeometry, Pgno, RawFieldSpan,
    RecordPart, Result, SQLITE_DEFAULT_PAGE_SIZE, SqliteError, TextEncoding, TypeAffinity, Value,
    encode_record_parts_into, encode_value_refs_record_into, extended,
    locate_roundtrip_stable_fields,
};
pub use sqliters_vdbe::BtreeAccess;
use sqliters_vdbe::{
    InsertValSource, PointDmlIndex, RowidKeySource, RowidPointDeletePlan, RowidPointUpdatePlan,
    SingleRowInsertPlan, TestFunctionsScope, VdbeProgram, VdbeVm, VmRuntimeContext, VmStep,
};
use sqliters_vfs::{MemoryVfs, OpenFlags, OsVfs, Vfs};

use crate::engine::{
    ActivitySummary, BusyPolicy, CommitFailureDisposition, ExecContext, LeaseCursorSpec,
    SqlSavepoint, StmtCategory, StmtTxTracker, classify_stmt_category,
    collect_select_referenced_roots, commit_all_databases, commit_transaction,
    execute_control_stmt, execute_nolock_stmt, execute_read_body, execute_write_body,
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

fn register_builtin_extension_functions(
    functions: &mut FunctionRegistry,
    fts5_aux: Arc<Fts5AuxRegistry>,
) {
    let aux_bm25 = Arc::clone(&fts5_aux);
    functions.register_scalar("bm25", None, FunctionFlags::INNOCUOUS, move |args| {
        if args.len() == 1 {
            return Ok(RuntimeValue::plain(
                aux_bm25.eval_bm25(std::slice::from_ref(&args[0].value))?,
            ));
        }
        let vals: Vec<Value> = args.iter().map(|rv| rv.value.clone()).collect();
        Ok(RuntimeValue::plain(aux_bm25.eval_bm25(&vals)?))
    });

    let aux_hl = Arc::clone(&fts5_aux);
    functions.register_scalar(
        "highlight",
        Some(4),
        FunctionFlags::INNOCUOUS,
        move |args| {
            let vals: Vec<Value> = args.iter().map(|rv| rv.value.clone()).collect();
            Ok(RuntimeValue::plain(aux_hl.eval_highlight(&vals)?))
        },
    );

    let aux_snip = fts5_aux;
    functions.register_scalar("snippet", Some(6), FunctionFlags::INNOCUOUS, move |args| {
        let vals: Vec<Value> = args.iter().map(|rv| rv.value.clone()).collect();
        Ok(RuntimeValue::plain(aux_snip.eval_snippet(&vals)?))
    });

    functions.register_scalar(
        "fts5_source_id",
        Some(0),
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |_args| Ok(RuntimeValue::plain(Fts5AuxRegistry::eval_fts5_source_id())),
    );

    functions.register_scalar("fts5", Some(1), FunctionFlags::INNOCUOUS, |args| {
        Ok(RuntimeValue::plain(Fts5AuxRegistry::eval_fts5_ptr_func(
            &args[0].value,
        )))
    });

    functions.register_scalar(
        "rtreedepth",
        Some(1),
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |args| {
            Ok(RuntimeValue::plain(sqliters_ext::eval_rtreedepth(
                &args[0].value,
            )?))
        },
    );

    functions.register_scalar(
        "rtreenode",
        Some(2),
        FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
        |args| {
            Ok(RuntimeValue::plain(sqliters_ext::eval_rtreenode(
                &args[0].value,
                &args[1].value,
            )?))
        },
    );

    for arity in [1usize, 2usize] {
        functions.register_scalar("rtreecheck", Some(arity), FunctionFlags::INNOCUOUS, |_| {
            Err(SqliteError::new(
                ErrorCode::Internal,
                "__sqliters_builtin_rtreecheck__",
            ))
        });
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
};

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
        Self {
            leases: Vec::with_capacity(4),
        }
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
        read_instances: SmallInstVec,
    },
    MaterializedRead {
        rows: std::vec::IntoIter<Vec<Value>>,
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
}

pub(crate) struct ConnectionShared {
    pub(crate) conn_id: u64,
    pub(crate) db_path: Option<PathBuf>,
    pub(crate) database_kind: DatabaseKind,
    pub(crate) is_readonly: bool,
    pub(crate) open_options: ConnectionOpenOptions,
    pub(crate) interrupt_flag: Arc<AtomicBool>,
    pub(crate) has_external_interrupt: AtomicBool,
    pub(crate) gate_busy: AtomicBool,
    pub(crate) conn_state: AtomicU8,
    pub(crate) has_pending_drops: AtomicBool,
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

    pub(crate) fn enter(&self) -> Result<ConnSession<'_>> {
        if is_conn_active_on_current_thread(self.conn_id) {
            return Err(SqliteError::misuse(
                "reentrant call into active Connection on the same thread",
            ));
        }
        if self
            .gate_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(SqliteError::new(
                ErrorCode::Busy,
                "database connection is busy",
            ));
        }

        let mut gate_unwind = GateReleaseOnUnwind {
            shared: self,
            armed: true,
        };
        let stack_guard = ActiveConnStackGuard::push(self.conn_id);
        let inner_guard = self.lock_inner();
        // Transfer gate release responsibility to `ConnSession` BEFORE running `process_pending_drops`!
        gate_unwind.armed = false;
        let mut session = ConnSession {
            shared: self,
            guard: Some(inner_guard),
            _stack_guard: stack_guard,
        };
        if self.has_pending_drops.load(Ordering::SeqCst) {
            let initial_drops = {
                let mut pq = self.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                self.has_pending_drops.store(false, Ordering::SeqCst);
                batch
            };
            if !initial_drops.is_empty() {
                session
                    .inner_mut()
                    .process_pending_drops(initial_drops, self);
            }
        }
        Ok(session)
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

        if first
            .gate_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(SqliteError::new(
                ErrorCode::Busy,
                "database connection is busy",
            ));
        }
        let mut first_unwind = GateReleaseOnUnwind {
            shared: first,
            armed: true,
        };

        if second
            .gate_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(SqliteError::new(
                ErrorCode::Busy,
                "database connection is busy",
            ));
        }
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

        if first.has_pending_drops.load(Ordering::SeqCst) {
            let first_drops = {
                let mut pq = first.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                first.has_pending_drops.store(false, Ordering::SeqCst);
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

        if second.has_pending_drops.load(Ordering::SeqCst) {
            let second_drops = {
                let mut pq = second.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                second.has_pending_drops.store(false, Ordering::SeqCst);
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
            self.has_pending_drops.store(true, Ordering::SeqCst);
            self.gate_busy
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        };
        if claimed {
            let _stack_guard = ActiveConnStackGuard::push(self.conn_id);
            self.drain_and_release_gate();
        }
    }

    fn drain_and_release_gate(&self) {
        if !self.has_pending_drops.load(Ordering::SeqCst) {
            self.gate_busy.store(false, Ordering::SeqCst);
            if !self.has_pending_drops.load(Ordering::SeqCst)
                || self
                    .gate_busy
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
                    .is_err()
            {
                return;
            }
        }
        let mut gate_unwind = GateReleaseOnUnwind {
            shared: self,
            armed: true,
        };
        loop {
            let batch = {
                let mut pq = self.lock_pending_drops();
                let batch = std::mem::take(&mut *pq);
                if batch.is_empty() {
                    self.has_pending_drops.store(false, Ordering::SeqCst);
                    self.gate_busy.store(false, Ordering::SeqCst);
                    gate_unwind.armed = false;
                    return;
                }
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
            self.shared.gate_busy.store(false, Ordering::Release);
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
        self.has_pending_drops.store(false, Ordering::Relaxed);
        for item in batch {
            match item {
                PendingDrop::Statement { handle_id, running } => {
                    if let Some(boxed_run) = running {
                        match *boxed_run {
                            RunningExec::Vdbe { mut vm, .. } => {
                                vm.close_cursors(&mut inner.db_set);
                            }
                            RunningExec::MaterializedRead { lease_cursors, .. }
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
    fn drop(&mut self) {
        if let Some(ref g) = self.guard {
            self.shared.sync_conn_state(&g.state);
        }
        drop(self.guard.take());
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
        if self.active_interrupt_flag.load(Ordering::SeqCst) {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        self.db_set.main_btree().check_interrupted()
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
                self.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                        read_instances,
                    },
                    shared,
                )
            }
            RunningExec::PointSelectRowYielded { read_instances } => self.finish_activity(
                hid,
                ActivityFinish::ReadDone {
                    lease_cursors: Vec::new(),
                    read_instances,
                },
                shared,
            ),
            RunningExec::MaterializedRead {
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
        let disk_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
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
        let disk_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
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
        let disk_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
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
        let disk_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));

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
                    Arc::new(OsVfs::with_lock_geometry(effective_options.lock_geometry))
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

        let fts5_aux = db_set.vtab_registry().fts5_aux_registry();
        let mut functions = FunctionRegistry::default();
        register_builtin_extension_functions(&mut functions, fts5_aux);
        functions.mark_builtin_init_complete();

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
        };

        let shared = Arc::new(ConnectionShared {
            conn_id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
            db_path,
            database_kind,
            is_readonly,
            open_options,
            interrupt_flag,
            has_external_interrupt: AtomicBool::new(false),
            gate_busy: AtomicBool::new(false),
            conn_state: AtomicU8::new(CONN_STATE_OPEN),
            has_pending_drops: AtomicBool::new(false),
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
        let mut stmt =
            Statement::prepare_in_session(&self.shared, session.inner_mut(), sql, PrepareMode::V2)?;
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
            let mut stmt = Statement::prepare_in_session(
                &self.shared,
                session.inner_mut(),
                sql,
                PrepareMode::V2,
            )?;
            let next_tail_len = stmt.unparsed_tail.len();
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

    /// Executes a query with optional positional parameters and returns all columns & rows.
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        let mut session = self.shared.enter()?;
        session.inner().ensure_usable()?;
        let mut stmt =
            Statement::prepare_in_session(&self.shared, session.inner_mut(), sql, PrepareMode::V2)?;
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
        let mut stmt =
            Statement::prepare_in_session(&self.shared, session.inner_mut(), sql, PrepareMode::V2)?;
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
            // `sqlite3_deserialize` reopens the database as a fresh MemDB b-tree (`attach.c`
            // `REOPEN_AS_MEMDB`), so the secure-delete mode falls back to the build default.
            main_bt.secure_delete(Some(0));
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
            self.shared.has_pending_drops.store(true, Ordering::SeqCst);
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

    let (stmt_opt_auth, ignored_fk_parent_tables) = driver::authorize_stmt_with_slot_and_fks(
        &stmt,
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
        let (stmt_opt, tail) = parse_one(sql)?;
        let consumed_len = sql.len().saturating_sub(tail.len());
        let raw_sql = sql[..consumed_len].to_owned();
        let unparsed_tail = tail.to_owned();

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

        let mut artifacts = match try_compile(inner, stmt_opt.clone()) {
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
                    try_compile(inner, stmt_opt)?
                } else {
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        };

        apply_prepare_auto_vacuum_pragma(inner, &mut artifacts);

        let hid = inner.register_handle(HandleKind::Statement);
        Ok(Self {
            shared: Arc::clone(shared),
            handle_id: Some(hid),
            mode,
            raw_sql,
            stmt: artifacts.stmt,
            requires_schema: artifacts.requires_schema,
            category: artifacts.category,
            compiled_vdbe: artifacts.compiled_vdbe,
            compiled_fast_read: artifacts.compiled_fast_read,
            cached_read_vm: None,
            write_vm_scratch: sqliters_vdbe::VdbeWriteScratch::default(),
            compiled_schema_generation: inner.schema_generation,
            compiled_v1_expire_generation: inner.v1_expire_generation,
            compiled_advisory_expire_generation: inner.advisory_expire_generation,
            compiled_slot_cookies: inner.snapshot_slot_cookies(),
            expired: false,
            column_names: artifacts.column_names,
            params: vec![Value::Null; artifacts.max_param],
            named_params: artifacts.named_params,
            ignored_fk_parent_tables: artifacts.ignored_fk_parent_tables,
            unparsed_tail,
            lifecycle: StatementLifecycle::Ready,
        })
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
            && !self.shared.has_pending_drops.load(Ordering::Acquire)
            && !self.shared.gate_busy.load(Ordering::Acquire)
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
        if let StatementLifecycle::Running(RunningExec::PointSelectRowYielded {
            ref read_instances,
        }) = self.lifecycle
            && let Some(hid) = self.handle_id
        {
            let r_insts = read_instances.clone();
            let fin_res = {
                let mut session = self.shared.enter()?;
                let inner = session.inner_mut();
                inner.ensure_usable()?;
                inner.runtime.stmt_timestamp_ms = None;
                inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                        read_instances: r_insts,
                    },
                    &self.shared,
                )
            };
            self.lifecycle = StatementLifecycle::Halted {
                last_error: fin_res.as_ref().err().cloned(),
            };
            fin_res?;
            return Ok(StepResult::Done);
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
            enum PointOutcome {
                Fallback,
                EarlyErr(SqliteError),
                HaltedErr(SqliteError),
                Hit(Vec<Value>, SmallInstVec),
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
                                            .point_lookup_table_columns(
                                                plan.root_pgno,
                                                rowid,
                                                &plan.cols,
                                            ),
                                    },
                                };

                                match lookup_res {
                                    Ok(Some(vals)) => {
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
                                        PointOutcome::Hit(vals, read_instances)
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
                PointOutcome::Hit(vals, read_instances) => {
                    self.lifecycle =
                        StatementLifecycle::Running(RunningExec::PointSelectRowYielded {
                            read_instances,
                        });
                    return Ok(StepResult::Row(Row::new(vals)));
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
                let exec_res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| {
                        crate::vtab::ensure_stmt_vtabs_connected(ctx, stmt_ref)?;
                        execute_read_body(ctx, stmt_ref, &self.params)
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

    fn advance_running_in_session(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<StepResult> {
        let StatementLifecycle::Running(ref mut running) = self.lifecycle else {
            unreachable!("advance_running_in_session requires Running state");
        };

        match running {
            RunningExec::PointSelectRowYielded { read_instances } => {
                let r_insts = std::mem::take(read_instances);
                inner.runtime.stmt_timestamp_ms = None;
                let fin_res = inner.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                        read_instances: r_insts,
                    },
                    shared,
                );
                self.lifecycle = StatementLifecycle::Halted {
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
                    vm.step(&mut inner.db_set, &mut services)
                };
                match step_res {
                    Ok(VmStep::Row(vals)) => {
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
                    Ok(VmStep::Done) => {
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
                            &mut self.lifecycle,
                            StatementLifecycle::Halted {
                                last_error: fin_res.as_ref().err().cloned(),
                            },
                        );
                        if let StatementLifecycle::Running(RunningExec::Vdbe { vm, .. }) = prev {
                            self.cached_read_vm = Some(vm);
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
                            &mut self.lifecycle,
                            StatementLifecycle::Halted {
                                last_error: Some(e.clone()),
                            },
                        );
                        if let StatementLifecycle::Running(RunningExec::Vdbe { vm, .. }) = prev {
                            self.cached_read_vm = Some(vm);
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
                    self.lifecycle = StatementLifecycle::Halted {
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
                    if let Some(&BtreeCursorState::Fault(code)) = bt.cursor_state(cid) {
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
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
                    self.lifecycle = StatementLifecycle::Halted {
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
                    if let Some(&BtreeCursorState::Fault(code)) = bt.cursor_state(cid) {
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                }

                if *pos < spans.len() {
                    let (off, len) = spans[*pos];
                    *pos += 1;
                    let val = if len == u32::MAX {
                        Value::Null
                    } else {
                        Value::Text(arena[off as usize..(off + len) as usize].to_owned())
                    };
                    Ok(StepResult::Row(Row::new(vec![val])))
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
                    Ok(StepResult::Done)
                }
            }

            RunningExec::MaterializedNoLock { rows } => {
                if let Some(vals) = rows.next() {
                    Ok(StepResult::Row(Row::new(vals)))
                } else {
                    let _ = inner.finish_activity(hid, ActivityFinish::NoLock, shared);
                    inner.maybe_run_wal_autocheckpoint();
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
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
                    self.lifecycle = StatementLifecycle::Halted {
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
                            self.lifecycle = StatementLifecycle::Halted { last_error: None };
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

            RunningExec::IncrementalVacuum {
                db_instance,
                remaining,
                abort_snapshot,
            } => {
                if let Err(e) = inner.check_any_interrupted() {
                    let snap = abort_snapshot.clone();
                    let final_err = inner.abort_write_statement(hid, &e, snap, shared);
                    self.lifecycle = StatementLifecycle::Halted {
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
                                self.lifecycle = StatementLifecycle::Halted { last_error: None };
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
                    Err(err) => {
                        let snap = abort_snapshot.clone();
                        let final_err = inner.abort_write_statement(hid, &err, snap, shared);
                        self.lifecycle = StatementLifecycle::Halted {
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
                        self.lifecycle = StatementLifecycle::Halted { last_error: None };
                        Ok(StepResult::Done)
                    }
                    Err(e) if e.code == ErrorCode::Busy && summary.other_writers == 0 => {
                        *last_busy = e.clone();
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
        InsertValSource::Const(v) => v,
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
    let mut coerced: [Value; 16] = std::array::from_fn(|_| Value::Null);
    let mut is_coerced = [false; 16];

    // 1. Evaluate any built-in `jsonb(?p)` expressions first (matching VDBE step-2 expression order).
    if plan.has_builtin_jsonb {
        for (i, col) in plan.cols.iter().enumerate() {
            if let InsertValSource::JsonbParam1Based(p) = col.src {
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
        }
    }

    let run_res = (|| -> Result<i64> {
        let explicit_rowid = match plan.rowid_src {
            None => None,
            Some(ref src) => {
                let raw_rid = resolve_insert_val_source(src, params);
                if raw_rid.is_null() {
                    None
                } else if let Value::Integer(rid) = *raw_rid {
                    Some(rid)
                } else {
                    match TypeAffinity::Integer.apply(raw_rid.clone()) {
                        Value::Integer(rid) => Some(rid),
                        _ => {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                    }
                }
            }
        };

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

        if plan.real_as_int {
            for (i, col) in plan.cols.iter().enumerate() {
                if col.affinity != TypeAffinity::Real {
                    continue;
                }
                let cur = if is_coerced[i] {
                    &coerced[i]
                } else {
                    resolve_insert_val_source(&col.src, params)
                };
                if let Value::Real(r) = cur
                    && let Some(iv) = sqliters_types::real_affinity_storage_int(*r)
                {
                    coerced[i] = Value::Integer(iv);
                    is_coerced[i] = true;
                }
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

        let (rowid, need_dup_check) = match explicit_rowid {
            Some(rid) => (rid, true),
            None => (bt.next_rowid(plan.root_pgno)?, false),
        };

        let enc = bt.encoding();
        // `sqlite3CompleteInsertion`: the index entries are written before the table row. The
        // rowid uniqueness test therefore comes first when there are indexes (`OP_NotExists`;
        // nothing may be written if it fails: the plan runs without a statement savepoint) and
        // the table insert reuses the leaf it found.
        let probed_leaf = if need_dup_check && !plan.indexes.is_empty() {
            match bt.probe_table_insert_leaf(plan.root_pgno, rowid)? {
                Some(leaf) => Some(leaf),
                None => {
                    return Err(SqliteError::constraint(
                        plan.dup_ext_code,
                        plan.dup_msg.clone(),
                    ));
                }
            }
        } else {
            None
        };

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

        encode_value_refs_record_into(&val_refs[..n_cols], enc, scratch_rec);
        if let Some(leaf) = probed_leaf {
            bt.insert_table_at_leaf(plan.root_pgno, rowid, scratch_rec, leaf)?;
        } else if need_dup_check {
            if !bt.insert_table_unique(plan.root_pgno, rowid, scratch_rec)? {
                return Err(SqliteError::constraint(
                    plan.dup_ext_code,
                    plan.dup_msg.clone(),
                ));
            }
        } else {
            bt.insert_table(plan.root_pgno, rowid, scratch_rec)?;
        }

        Ok(rowid)
    })();

    if plan.has_builtin_jsonb {
        reclaim_jsonb_scratch(&mut coerced[..n_cols], scratch_jsonb);
    }
    run_res
}

/// Result of a direct point `UPDATE` / `DELETE` plan.
enum PointDmlOutcome {
    /// The statement ran to completion: `changes` rows changed, and the VDBE program would have
    /// executed `ticks` instructions.
    Done { changes: u64, ticks: u64 },
    /// Nothing was modified; the VDBE program must run the statement.
    NotHandled,
}

#[cfg(test)]
thread_local! {
    /// Statements completed by a direct point plan on this thread (test instrumentation).
    static POINT_DML_PLAN_RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Columns / key fields a point plan handles (`POINT_DML_MAX_COLS` in the compiler).
const POINT_DML_MAX_FIELDS: usize = 16;

/// Reads the row `rowid` of a point plan: `Ok(Some((leaf_pgno, cell_idx)))` with its record in
/// `row` and its first `n_cols` fields in `spans`, `Ok(None)` when there is no such row, and
/// `Err(())` when the plan has to decline (record on overflow pages, unreadable page, a field
/// the program would read that does not survive `decode -> encode`, or fewer stored fields
/// than columns).
#[allow(clippy::too_many_arguments)]
fn point_dml_read_row(
    bt: &mut Btree,
    root_pgno: Pgno,
    rowid: i64,
    n_cols: usize,
    read_mask: u32,
    parse_record: bool,
    row: &mut Vec<u8>,
    spans: &mut [RawFieldSpan; POINT_DML_MAX_FIELDS],
) -> std::result::Result<Option<(Pgno, usize)>, ()> {
    match bt.seek_table_row_local(root_pgno, rowid, row) {
        Ok(sqliters_btree::TableRowSeek::Missing) => Ok(None),
        Ok(sqliters_btree::TableRowSeek::Local {
            leaf_pgno,
            cell_idx,
        }) => {
            if parse_record
                && !locate_roundtrip_stable_fields(
                    row,
                    bt.encoding(),
                    read_mask,
                    &mut spans[..n_cols],
                )
            {
                return Err(());
            }
            Ok(Some((leaf_pgno, cell_idx)))
        }
        Ok(sqliters_btree::TableRowSeek::Spilled) | Err(_) => Err(()),
    }
}

/// Builds the key record of `idx` for the row `rowid`: stored fields are copied from `row`
/// except the columns overridden by `new_vals` (assigned columns of an `UPDATE`).
fn point_dml_index_key(
    idx: &PointDmlIndex,
    row: &[u8],
    spans: &[RawFieldSpan; POINT_DML_MAX_FIELDS],
    new_vals: Option<&[Option<&Value>; POINT_DML_MAX_FIELDS]>,
    rid_val: &Value,
    enc: TextEncoding,
    out: &mut Vec<u8>,
) {
    let mut parts = [RecordPart::Value(rid_val); POINT_DML_MAX_FIELDS + 1];
    let k_len = idx.col_slots.len();
    for (part, slot) in parts.iter_mut().zip(&idx.col_slots) {
        if let Some(ci) = *slot {
            *part = match new_vals.and_then(|nv| nv[ci]) {
                Some(v) => RecordPart::Value(v),
                None => RecordPart::Raw {
                    serial_type: spans[ci].serial_type,
                    body: &row[spans[ci].start..spans[ci].end],
                },
            };
        }
    }
    encode_record_parts_into(&parts[..=k_len], enc, out);
}

/// Runs `DELETE FROM <tbl> WHERE <rowid> = <param|literal>` directly on the B-tree: the same
/// index and table deletions, with the same key bytes, as the VDBE program compiled for the
/// statement. Declines (`NotHandled`, nothing modified) whenever it cannot prove that.
fn execute_rowid_point_delete_plan(
    plan: &RowidPointDeletePlan,
    params: &[Value],
    row: &mut Vec<u8>,
    rec: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<PointDmlOutcome> {
    let enc = bt.encoding();
    if enc != TextEncoding::Utf8
        || plan.n_cols > POINT_DML_MAX_FIELDS
        || plan
            .indexes
            .iter()
            .any(|i| i.col_slots.len() >= POINT_DML_MAX_FIELDS)
    {
        return Ok(PointDmlOutcome::NotHandled);
    }
    bt.check_interrupted()?;
    if bt.transaction_state() != TransactionState::Write {
        bt.begin_write()?;
    }
    let Some(rowid) = resolve_insert_val_source(&plan.key, params).to_exact_rowid_i64() else {
        return Ok(PointDmlOutcome::Done {
            changes: 0,
            ticks: plan.ticks_miss,
        });
    };
    let mut spans = [RawFieldSpan::default(); POINT_DML_MAX_FIELDS];
    let position = match point_dml_read_row(
        bt,
        plan.root_pgno,
        rowid,
        plan.n_cols,
        plan.read_mask,
        !plan.indexes.is_empty(),
        row,
        &mut spans,
    ) {
        Ok(Some(position)) => position,
        Ok(None) => {
            return Ok(PointDmlOutcome::Done {
                changes: 0,
                ticks: plan.ticks_miss,
            });
        }
        Err(()) => return Ok(PointDmlOutcome::NotHandled),
    };

    let rid_val = Value::Integer(rowid);
    for idx in &plan.indexes {
        point_dml_index_key(idx, row, &spans, None, &rid_val, enc, rec);
        bt.delete_index_raw(idx.root_pgno, rec, &idx.orders)?;
    }

    let deleted = bt.delete_table_at(plan.root_pgno, rowid, Some(position))?;
    Ok(PointDmlOutcome::Done {
        changes: u64::from(deleted),
        ticks: plan.ticks_hit,
    })
}

/// Runs `UPDATE <tbl> SET c = <param|literal>, ... WHERE <rowid> = <param|literal>` directly on
/// the B-tree: the same index deletions / insertions and row replacement, with the same record
/// bytes, as the VDBE program compiled for the statement. Declines (`NotHandled`, nothing
/// modified) whenever it cannot prove that — including a NOT NULL violation, which the program
/// then reports.
fn execute_rowid_point_update_plan(
    plan: &RowidPointUpdatePlan,
    params: &[Value],
    row: &mut Vec<u8>,
    rec: &mut Vec<u8>,
    bt: &mut Btree,
) -> Result<PointDmlOutcome> {
    let enc = bt.encoding();
    if enc != TextEncoding::Utf8
        || plan.n_cols > POINT_DML_MAX_FIELDS
        || plan.sets.len() > POINT_DML_MAX_FIELDS
        || plan
            .indexes
            .iter()
            .any(|i| i.col_slots.len() >= POINT_DML_MAX_FIELDS)
    {
        return Ok(PointDmlOutcome::NotHandled);
    }
    bt.check_interrupted()?;
    if bt.transaction_state() != TransactionState::Write {
        bt.begin_write()?;
    }
    let Some(rowid) = resolve_insert_val_source(&plan.key, params).to_exact_rowid_i64() else {
        return Ok(PointDmlOutcome::Done {
            changes: 0,
            ticks: plan.ticks_miss,
        });
    };
    let mut spans = [RawFieldSpan::default(); POINT_DML_MAX_FIELDS];
    let position = match point_dml_read_row(
        bt,
        plan.root_pgno,
        rowid,
        plan.n_cols,
        plan.read_mask,
        true,
        row,
        &mut spans,
    ) {
        Ok(Some(position)) => position,
        Ok(None) => {
            return Ok(PointDmlOutcome::Done {
                changes: 0,
                ticks: plan.ticks_miss,
            });
        }
        Err(()) => return Ok(PointDmlOutcome::NotHandled),
    };

    // New values: column affinity, then NOT NULL (left to the program to report).
    let mut coerced: [Value; POINT_DML_MAX_FIELDS] = std::array::from_fn(|_| Value::Null);
    let mut is_coerced = [false; POINT_DML_MAX_FIELDS];
    for (i, set) in plan.sets.iter().enumerate() {
        let raw_val = resolve_insert_val_source(&set.src, params);
        let is_null = if set.affinity == TypeAffinity::Blob
            || affinity_is_noop_for_insert(set.affinity, raw_val)
        {
            raw_val.is_null()
        } else {
            coerced[i] = set.affinity.apply(raw_val.clone());
            is_coerced[i] = true;
            coerced[i].is_null()
        };
        if is_null && set.not_null {
            return Ok(PointDmlOutcome::NotHandled);
        }
    }
    let mut new_vals: [Option<&Value>; POINT_DML_MAX_FIELDS] = [None; POINT_DML_MAX_FIELDS];
    for (i, set) in plan.sets.iter().enumerate() {
        new_vals[set.col] = Some(if is_coerced[i] {
            &coerced[i]
        } else {
            resolve_insert_val_source(&set.src, params)
        });
    }

    let rid_val = Value::Integer(rowid);
    // update.c: every old index entry is deleted first (`sqlite3GenerateRowIndexDelete`), then
    // the new entries are inserted and the table row is written last
    // (`sqlite3CompleteInsertion`).
    for idx in &plan.indexes {
        point_dml_index_key(idx, row, &spans, None, &rid_val, enc, rec);
        bt.delete_index_raw(idx.root_pgno, rec, &idx.orders)?;
    }
    // REAL-affinity columns are re-encoded in the statement's storage form, whether assigned
    // or carried over from the old record (see `RowidPointUpdatePlan::real_cols`).
    let mut real_fix: Vec<(usize, Value)> = Vec::new();
    for &ci in &plan.real_cols {
        let fixed = match new_vals[ci] {
            Some(&Value::Real(r)) if plan.real_as_int => {
                sqliters_types::real_affinity_storage_int(r).map(Value::Integer)
            }
            Some(_) => None,
            None => {
                let span = &spans[ci];
                let body = &row[span.start..span.end];
                match span.serial_type {
                    7 if plan.real_as_int && body.len() == 8 => {
                        let mut b = [0u8; 8];
                        b.copy_from_slice(body);
                        sqliters_types::real_affinity_storage_int(f64::from_bits(
                            u64::from_be_bytes(b),
                        ))
                        .map(Value::Integer)
                    }
                    1..=6 | 8 | 9 if !plan.real_as_int => {
                        let iv = match span.serial_type {
                            8 => 0i64,
                            9 => 1i64,
                            _ => body.iter().fold(
                                if body.first().is_some_and(|b| b & 0x80 != 0) {
                                    -1i64
                                } else {
                                    0i64
                                },
                                |acc, &b| (acc << 8) | i64::from(b),
                            ),
                        };
                        Some(Value::real(iv as f64))
                    }
                    _ => None,
                }
            }
        };
        if let Some(v) = fixed {
            real_fix.push((ci, v));
        }
    }
    for (ci, v) in &real_fix {
        new_vals[*ci] = Some(v);
    }
    for idx in &plan.indexes {
        point_dml_index_key(idx, row, &spans, Some(&new_vals), &rid_val, enc, rec);
        bt.insert_index(idx.root_pgno, rec, &idx.orders)?;
    }

    let mut parts = [RecordPart::Value(&Value::Null); POINT_DML_MAX_FIELDS];
    for (ci, part) in parts[..plan.n_cols].iter_mut().enumerate() {
        if plan.rowid_alias_col == Some(ci) {
            continue;
        }
        *part = match new_vals[ci] {
            Some(v) => RecordPart::Value(v),
            None => RecordPart::Raw {
                serial_type: spans[ci].serial_type,
                body: &row[spans[ci].start..spans[ci].end],
            },
        };
    }
    encode_record_parts_into(&parts[..plan.n_cols], enc, rec);
    bt.insert_table_at(plan.root_pgno, rowid, rec, Some(position))?;
    Ok(PointDmlOutcome::Done {
        changes: 1,
        ticks: plan.ticks_hit,
    })
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
        Ok(fast_read::FastReadExecOut::SingleRow(vals)) => {
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
            *lifecycle =
                StatementLifecycle::Running(RunningExec::PointSelectRowYielded { read_instances });
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
                    let first_val = if len0 == u32::MAX {
                        Value::Null
                    } else {
                        Value::Text(arena[off0 as usize..(off0 + len0) as usize].to_owned())
                    };
                    *lifecycle = StatementLifecycle::Running(RunningExec::FastSingleTextArena {
                        arena,
                        spans,
                        pos: 1,
                        lease_cursors,
                        read_instances,
                    });
                    Some(Ok(StepResult::Row(Row::new(vec![first_val]))))
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
    } else if let Some(point_res) = {
        // Direct B-tree plan for single-row `UPDATE` / `DELETE` by rowid, when one is attached
        // and applies; otherwise the VDBE program below runs.
        let point_outcome = if inner.progress.is_enabled() {
            None
        } else if let Some(upd_plan) = prog.rowid_point_update() {
            let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
            let (row, rec) = write_vm_scratch.point_dml_buffers_mut();
            Some(execute_rowid_point_update_plan(
                upd_plan, params, row, rec, bt,
            ))
        } else if let Some(del_plan) = prog.rowid_point_delete() {
            let bt = inner.db_set.slots_mut()[0].btree.as_mut().unwrap();
            let (row, rec) = write_vm_scratch.point_dml_buffers_mut();
            Some(execute_rowid_point_delete_plan(
                del_plan, params, row, rec, bt,
            ))
        } else {
            None
        };
        match point_outcome {
            Some(Ok(PointDmlOutcome::Done { changes, ticks })) => {
                #[cfg(test)]
                POINT_DML_PLAN_RUNS.with(|runs| runs.set(runs.get() + 1));
                inner.progress.add_ticks_without_handler(ticks);
                Some((Ok(()), inner.runtime.last_insert_rowid, changes))
            }
            Some(Err(e)) => Some((Err(e), inner.runtime.last_insert_rowid, 0u64)),
            Some(Ok(PointDmlOutcome::NotHandled)) | None => None,
        }
    } {
        point_res
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

    /// The direct point `UPDATE` / `DELETE` plans must be indistinguishable from the VDBE
    /// programs they shadow: same outcome and error per statement, same `changes()` /
    /// `total_changes()`, same progress-tick count, same table and index contents. One
    /// connection runs the plans; the other has a (never firing) progress handler installed,
    /// which routes every statement through the VDBE program.
    ///
    /// Covers bound parameters and literals of every type (column affinity, NOT NULL, rowid keys
    /// given as text / real / NULL / blob), missing rows, rows on overflow pages (old or new),
    /// rows shorter than the table (`ALTER TABLE ADD COLUMN`), NOCASE / DESC / rowid index
    /// columns, explicit transactions and savepoints with rollbacks, a `SELECT` being stepped
    /// on the same table, and schema changes (index, trigger, dropped index) under statements
    /// that stay prepared.
    #[test]
    fn point_update_delete_plans_match_vdbe_programs() {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Plan {
            Update,
            Delete,
            None,
        }
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        /// A value of any type; text / blob stay short (they may land in indexed columns).
        fn value(state: &mut u64) -> Value {
            let r = next(state);
            let ints: [i64; 10] = [
                0,
                1,
                -1,
                100,
                -129,
                40_000,
                -9_000_000,
                3_000_000_000,
                150_000_000_000_000,
                i64::MIN,
            ];
            match (r >> 8) % 12 {
                0 => Value::Null,
                1..=3 => Value::Integer(ints[((r >> 16) % 10) as usize]),
                4 => Value::Integer(((r >> 16) % 50) as i64),
                5 => Value::Real([1.5, -0.0, 3.0, -2.25, 1e300][((r >> 16) % 5) as usize]),
                6 => Value::Text(
                    [
                        "12", " 7 ", "1e2", "abc", "ABC", "", "h\u{e9}", "0x10", "-3.0",
                    ][((r >> 16) % 9) as usize]
                        .to_owned(),
                ),
                7 => Value::Text(format!("C-{:04}", (r >> 16) % 40)),
                8 => Value::Text("x".repeat(((r >> 16) % 40) as usize)),
                9 => Value::Blob(vec![(r >> 16) as u8; ((r >> 24) % 5) as usize]),
                _ => Value::Integer(((r >> 16) % 10_000) as i64),
            }
        }
        /// A value for non-indexed columns: often long enough to push the row (or, on small
        /// pages, the value alone) onto overflow pages.
        fn wide_value(state: &mut u64, page_size: u32) -> Value {
            let r = next(state);
            match (r >> 8) % 6 {
                0 => Value::Text("w".repeat(((r >> 16) % (2 * page_size as u64)) as usize)),
                1 => Value::Blob(vec![7u8; ((r >> 16) % (page_size as u64 / 2)) as usize]),
                2 => Value::Text("m".repeat(((r >> 16) % 200) as usize)),
                _ => value(state),
            }
        }
        fn key(state: &mut u64, n_rows: i64) -> Value {
            let r = next(state);
            let id = 1 + ((r >> 16) % (n_rows as u64 + 5)) as i64;
            match (r >> 8) % 16 {
                0 => Value::Null,
                1 => Value::Text(id.to_string()),
                2 => Value::Real(id as f64),
                3 => Value::Real(id as f64 + 0.5),
                4 => Value::Blob(vec![1]),
                5 => Value::Text("nope".to_owned()),
                6 => Value::Text(format!(" {id}.0 ")),
                _ => Value::Integer(id),
            }
        }

        const SCHEMA: &str =
            "CREATE TABLE t(id INTEGER PRIMARY KEY, a, b TEXT, c REAL NOT NULL, d INT, e BLOB);
             CREATE INDEX t_ad ON t(a, d);
             CREATE INDEX t_b ON t(b COLLATE NOCASE);
             CREATE INDEX t_c ON t(c DESC, id, a);
             CREATE TABLE u(x NOT NULL, y);
             CREATE TABLE v(id INTEGER PRIMARY KEY, p UNIQUE, q);
             CREATE TABLE w(k INTEGER PRIMARY KEY, a, b);
             CREATE TABLE log(id, a);";
        // Rows of `w` written before this are two fields short of the table.
        const WIDEN_W: &str = "ALTER TABLE w ADD COLUMN z;
             ALTER TABLE w ADD COLUMN y TEXT;
             CREATE INDEX w_za ON w(z, a);";
        // ?1, ?2: short values; ?3: rowid key; ?4: wide value (never indexed).
        let statements: [(&str, Plan); 30] = [
            ("UPDATE t SET d = ?1, a = ?2 WHERE id = ?3", Plan::Update),
            ("UPDATE t SET b = ?1 WHERE id = ?3", Plan::Update),
            ("UPDATE t SET c = ?2 WHERE id = ?3", Plan::Update),
            (
                "UPDATE t SET e = ?4, c = ?2, b = ?1 WHERE rowid = ?3",
                Plan::Update,
            ),
            ("UPDATE t SET a = 7, d = 'seven' WHERE id = 5", Plan::Update),
            (
                "UPDATE t SET a = NULL, b = x'00ff', c = 2.5, d = TRUE WHERE id = '7'",
                Plan::Update,
            ),
            (
                "UPDATE t SET d = 3.0, a = '12', c = 4, b = 5 WHERE id = 9.0",
                Plan::Update,
            ),
            ("UPDATE t SET c = NULL WHERE id = ?3", Plan::Update),
            ("UPDATE t SET e = ?4 WHERE ?3 = id", Plan::Update),
            (
                "UPDATE t SET e = ?4, d = ?1 WHERE _rowid_ = ?3",
                Plan::Update,
            ),
            ("DELETE FROM t WHERE id = ?3", Plan::Delete),
            ("DELETE FROM t WHERE id = 11", Plan::Delete),
            ("DELETE FROM t WHERE rowid = '13'", Plan::Delete),
            ("DELETE FROM t WHERE 17.0 = id", Plan::Delete),
            ("UPDATE u SET x = ?1, y = ?4 WHERE rowid = ?3", Plan::Update),
            ("DELETE FROM u WHERE rowid = ?3", Plan::Delete),
            ("UPDATE v SET q = ?4 WHERE id = ?3", Plan::Update),
            ("DELETE FROM v WHERE id = ?3", Plan::Delete),
            ("UPDATE w SET a = ?1 WHERE k = ?3", Plan::Update),
            ("UPDATE w SET z = ?2, b = ?4 WHERE k = ?3", Plan::Update),
            ("UPDATE w SET y = ?1 WHERE k = ?3", Plan::Update),
            ("DELETE FROM w WHERE k = ?3", Plan::Delete),
            // Shapes a point plan must not be attached to.
            ("UPDATE v SET p = ?1 WHERE id = ?3", Plan::None),
            ("UPDATE t SET d = d + 1 WHERE id = ?3", Plan::None),
            (
                "UPDATE t SET a = ?1 WHERE id = ?3 AND d IS NOT NULL",
                Plan::None,
            ),
            ("UPDATE OR REPLACE t SET a = ?1 WHERE id = ?3", Plan::None),
            ("DELETE FROM t WHERE id = ?3 + 0", Plan::None),
            // Refills, so that the tables do not drain.
            (
                "INSERT OR IGNORE INTO t VALUES (?3, ?1, ?2, 0.5, ?2, ?4)",
                Plan::None,
            ),
            ("INSERT OR IGNORE INTO v VALUES (?3, ?3, ?4)", Plan::None),
            (
                "INSERT OR IGNORE INTO w(k, a, b) VALUES (?3, ?1, ?2)",
                Plan::None,
            ),
        ];
        let dumps = [
            "SELECT * FROM t ORDER BY id",
            "SELECT id FROM t ORDER BY a, d, id",
            "SELECT id FROM t ORDER BY b COLLATE NOCASE, id",
            "SELECT id FROM t ORDER BY c DESC, id, a",
            "SELECT rowid, * FROM u ORDER BY rowid",
            "SELECT * FROM v ORDER BY id",
            "SELECT k FROM v, w WHERE k = id AND p IS q ORDER BY p",
            "SELECT * FROM w ORDER BY k",
            "SELECT k FROM w ORDER BY z, a, k",
            "SELECT count(*), total(id), group_concat(a) FROM log",
            "PRAGMA integrity_check",
        ];
        let tx_controls = [
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT s1",
            "RELEASE s1",
            "ROLLBACK TO s1",
            "COMMIT",
            "SAVEPOINT s2",
        ];
        // Applied one after another while the statements stay prepared.
        let schema_changes = [
            "CREATE INDEX t_d ON t(d, b)",
            "CREATE TRIGGER t_au AFTER UPDATE OF a ON t BEGIN INSERT INTO log VALUES (old.id, new.a); END",
            "DROP INDEX t_ad",
            "DROP TRIGGER t_au",
        ];

        for (page_size, n_rows, auto_vacuum) in [
            (4096u32, 600i64, false),
            (512, 300, false),
            (1024, 40, true),
            (65_536, 200, false),
        ] {
            let plan_conn = Connection::open_in_memory().unwrap();
            let vdbe_conn = Connection::open_in_memory().unwrap();
            let conns = [&plan_conn, &vdbe_conn];
            let mut seed = 0x51_7C_C1_B7_27_22_0A_95u64 ^ u64::from(page_size);
            for conn in conns {
                conn.execute_batch(&format!(
                    "PRAGMA page_size={page_size}; PRAGMA auto_vacuum={};",
                    u8::from(auto_vacuum)
                ))
                .unwrap();
                conn.execute_batch(SCHEMA).unwrap();
            }
            for id in 1..=n_rows {
                let params = [
                    Value::Integer(id),
                    value(&mut seed),
                    value(&mut seed),
                    Value::Real(id as f64 / 4.0),
                    value(&mut seed),
                    wide_value(&mut seed, page_size),
                ];
                for conn in conns {
                    conn.query("INSERT INTO t VALUES (?1, ?2, ?3, ?4, ?5, ?6)", &params)
                        .unwrap();
                    // `u.x` is NOT NULL: a NULL here is simply skipped on both connections.
                    conn.query(
                        "INSERT INTO u VALUES (?1, ?2)",
                        &[params[1].clone(), params[5].clone()],
                    )
                    .ok();
                    conn.query(
                        "INSERT INTO v VALUES (?1, ?1, ?2)",
                        &[params[0].clone(), params[5].clone()],
                    )
                    .unwrap();
                    conn.query("INSERT INTO w VALUES (?1, ?2, ?3)", &params[..3])
                        .unwrap();
                }
            }
            for conn in conns {
                conn.execute_batch(WIDEN_W).unwrap();
            }
            vdbe_conn
                .progress_handler(i32::MAX, Some(Box::new(|| false)))
                .unwrap();

            let mut plan_stmts: Vec<Statement> = statements
                .iter()
                .map(|(sql, _)| plan_conn.prepare(sql).unwrap())
                .collect();
            let mut vdbe_stmts: Vec<Statement> = statements
                .iter()
                .map(|(sql, _)| vdbe_conn.prepare(sql).unwrap())
                .collect();
            for (stmt, (sql, expect)) in plan_stmts.iter().zip(statements) {
                let got = match stmt.compiled_vdbe.as_deref() {
                    Some(prog) if prog.rowid_point_update().is_some() => Plan::Update,
                    Some(prog) if prog.rowid_point_delete().is_some() => Plan::Delete,
                    _ => Plan::None,
                };
                assert_eq!(got, expect, "point plan of {sql}");
            }
            let mut plan_readers: Vec<Statement> = Vec::new();
            let mut vdbe_readers: Vec<Statement> = Vec::new();
            for sql in ["SELECT id, a, d FROM t ORDER BY id", "SELECT k, z FROM w"] {
                plan_readers.push(plan_conn.prepare(sql).unwrap());
                vdbe_readers.push(vdbe_conn.prepare(sql).unwrap());
            }

            let plan_runs_before = POINT_DML_PLAN_RUNS.with(std::cell::Cell::get);
            let mut rows_changed = 0u64;
            let mut readers_open = false;
            let n_ops = 9_000u32;
            for op in 0..n_ops {
                let r = next(&mut seed);
                if r.is_multiple_of(97) {
                    let sql = tx_controls[((r >> 8) % tx_controls.len() as u64) as usize];
                    let a = plan_conn.execute_batch(sql).map_err(|e| e.to_string());
                    let b = vdbe_conn.execute_batch(sql).map_err(|e| e.to_string());
                    assert_eq!(a, b, "op {op}: {sql}");
                    continue;
                }
                if r.is_multiple_of(if readers_open { 151 } else { 1_201 }) {
                    // Start (or finish) scans that stay open across the following statements.
                    for (readers, label) in
                        [(&mut plan_readers, "plan"), (&mut vdbe_readers, "vdbe")]
                    {
                        for reader in readers.iter_mut() {
                            if readers_open {
                                reader.reset().unwrap();
                            } else {
                                assert!(
                                    matches!(reader.step(), Ok(StepResult::Row(_))),
                                    "{label} reader"
                                );
                            }
                        }
                    }
                    readers_open = !readers_open;
                    continue;
                }
                if readers_open && r.is_multiple_of(7) {
                    let mut seen = Vec::new();
                    for readers in [&mut plan_readers, &mut vdbe_readers] {
                        for reader in readers.iter_mut() {
                            seen.push(match reader.step() {
                                Ok(StepResult::Row(row)) => Ok(Some(row.get(0).cloned())),
                                Ok(StepResult::Done) => Ok(None),
                                Err(e) => Err(e.to_string()),
                            });
                        }
                    }
                    let (plan_seen, vdbe_seen) = seen.split_at(plan_readers.len());
                    assert_eq!(plan_seen, vdbe_seen, "op {op}: open scans diverged");
                }
                if op > n_ops / 2 && (op - n_ops / 2).is_multiple_of(n_ops / 10) {
                    let idx = ((op - n_ops / 2) / (n_ops / 10)) as usize - 1;
                    if let Some(sql) = schema_changes.get(idx) {
                        // DDL on a table with a scan in progress is refused (SQLITE_LOCKED).
                        if readers_open {
                            for reader in plan_readers.iter_mut().chain(&mut vdbe_readers) {
                                reader.reset().unwrap();
                            }
                            readers_open = false;
                        }
                        for conn in conns {
                            conn.execute_batch(sql).unwrap();
                        }
                    }
                }
                let which = ((r >> 8) % statements.len() as u64) as usize;
                let sql = statements[which].0;
                let params = [
                    value(&mut seed),
                    value(&mut seed),
                    key(&mut seed, n_rows),
                    wide_value(&mut seed, page_size),
                ];
                let ticks_before = (
                    plan_conn.progress_op_count().unwrap(),
                    vdbe_conn.progress_op_count().unwrap(),
                );
                let mut outcomes = Vec::new();
                for (conn, stmts) in [(&plan_conn, &mut plan_stmts), (&vdbe_conn, &mut vdbe_stmts)]
                {
                    let stmt = &mut stmts[which];
                    for (i, p) in params.iter().enumerate() {
                        if sql.contains(&format!("?{}", i + 1)) {
                            stmt.bind(i + 1, p.clone()).unwrap();
                        }
                    }
                    let res = stmt
                        .step()
                        .map(|r| matches!(r, StepResult::Done))
                        .map_err(|e| (e.extended_code, e.to_string()));
                    stmt.reset().ok();
                    outcomes.push((res, conn.changes(), conn.total_changes()));
                }
                assert_eq!(outcomes[0], outcomes[1], "op {op}: {sql} with {params:?}");
                if statements[which].1 != Plan::None {
                    assert_eq!(
                        plan_conn.progress_op_count().unwrap() - ticks_before.0,
                        vdbe_conn.progress_op_count().unwrap() - ticks_before.1,
                        "op {op}: tick count of {sql} with {params:?}"
                    );
                }
                if outcomes[0].0.is_ok() {
                    rows_changed += outcomes[0].1;
                }
                if op % 500 == 499 || op + 1 == n_ops {
                    for sql in dumps {
                        let a = plan_conn.query(sql, &[]).unwrap().rows;
                        let b = vdbe_conn.query(sql, &[]).unwrap().rows;
                        assert_eq!(a, b, "{sql} after op {op}");
                    }
                }
            }
            assert!(rows_changed > 1_500, "only {rows_changed} rows changed");
            let plan_runs = POINT_DML_PLAN_RUNS.with(std::cell::Cell::get) - plan_runs_before;
            // (On 64 KiB pages only statements that find no row are completed by a plan.)
            assert!(
                plan_runs > 3_000,
                "only {plan_runs} statements took a point plan"
            );
            drop((plan_readers, vdbe_readers));
            for conn in conns {
                while conn.execute_batch("COMMIT").is_ok() {}
                let ok = conn
                    .query_row("PRAGMA integrity_check", &[])
                    .unwrap()
                    .unwrap();
                assert_eq!(ok.get(0), Some(&Value::Text("ok".to_owned())));
            }
        }
    }

    /// The integer row kernel of the covering-index aggregate plans (and the integer range
    /// probe of the index seek under them) must be invisible: for every statement and binding,
    /// the plan with its kernel and the same plan with the kernel removed (every row through
    /// the general row code) return the same rows or the same error. Where the result cannot
    /// depend on the order rows reach the aggregates (no i64 overflow), the generic executor
    /// (a connection with a progress handler never takes a fast-read plan) must agree too.
    ///
    /// Covers statements re-used across bindings of every type (INTEGER, REAL, numeric and
    /// other TEXT, BLOB, NULL, the i64 extremes), `=` / `<` / `<=` / `>` / `>=` / `!=` / `IS` /
    /// `BETWEEN` / `NOT BETWEEN`, `count` / `sum` / `total` / `avg` / `min` / `max` including
    /// integer overflow, rowid columns read from the index, a `DESC` index, group keys mixing
    /// `1`, `1.0`, text, blobs and NULL, rows that leave the kernel mid-scan (NULL / REAL /
    /// TEXT / BLOB values in otherwise integer columns), ranges that are empty, cover
    /// everything or end on duplicate keys, and 512-byte to 64 KiB pages (multi-level indexes).
    #[test]
    fn int_kernel_covering_aggregates_match_general_row_code() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        /// A value for an indexed column: mostly small integers.
        fn cell(state: &mut u64, span: u64, odd_per_64: u64, extremes: bool) -> Value {
            let r = next(state);
            if (r >> 40) % 64 >= odd_per_64 {
                return Value::Integer(((r >> 8) % span) as i64);
            }
            match (r >> 8) % if extremes { 8 } else { 6 } {
                0 | 1 => Value::Null,
                2 => Value::Real(((r >> 16) % span) as f64 + 0.5),
                3 => Value::Real(((r >> 16) % span) as f64),
                4 => Value::Text(["abc", "", "7x", "Z"][((r >> 16) % 4) as usize].to_owned()),
                5 => Value::Blob(vec![1u8; ((r >> 16) % 3) as usize]),
                6 => Value::Integer(
                    [i64::MAX, i64::MIN, i64::MAX / 2, -40_000, 1 << 40][((r >> 16) % 5) as usize],
                ),
                7 => Value::Integer(-(((r >> 16) % span) as i64)),
                _ => Value::Integer(-(((r >> 16) % span) as i64)),
            }
        }
        /// A comparison operand: mostly integers in (and just around) the stored range.
        fn operand(state: &mut u64, span: u64) -> Value {
            let r = next(state);
            let i = ((r >> 8) % (span + 2)) as i64 - 1;
            match (r >> 40) % 24 {
                0 => Value::Null,
                1 => Value::Real(i as f64 + 0.5),
                2 => Value::Real(i as f64),
                3 => Value::Text(i.to_string()),
                4 => Value::Text("abc".to_owned()),
                5 => Value::Blob(vec![1]),
                6 => Value::Integer([i64::MAX, i64::MIN, i64::MAX / 2][((r >> 16) % 3) as usize]),
                _ => Value::Integer(i),
            }
        }

        const SCHEMA: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, g INT, s INT, x, y TEXT);
             CREATE INDEX t_gs ON t(g, s);
             CREATE INDEX t_xg ON t(x, g, id);
             CREATE TABLE u(a INTEGER, b INTEGER, c INTEGER, d);
             CREATE INDEX u_abc ON u(a, b, c);
             CREATE INDEX u_c_desc ON u(c DESC, a);";
        // (statement, must carry an integer kernel)
        let statements: [(&str, bool); 22] = [
            (
                "SELECT count(*), sum(s) FROM t WHERE g = ?1 AND s BETWEEN ?2 AND ?3",
                true,
            ),
            (
                "SELECT g, count(*), sum(s), avg(s) FROM t WHERE s > ?2 GROUP BY g ORDER BY g",
                true,
            ),
            (
                "SELECT g, count(*), total(s), min(s), max(s) FROM t \
                 WHERE s >= ?2 AND s < ?3 GROUP BY g ORDER BY g DESC",
                true,
            ),
            (
                "SELECT count(s), min(s), max(s), total(s), avg(s) FROM t WHERE g = ?1",
                true,
            ),
            (
                "SELECT count(*), sum(s), max(id), min(id) FROM t WHERE g = ?1 AND s > ?2",
                true,
            ),
            (
                "SELECT count(*), sum(s) FROM t WHERE g = ?1 AND s NOT BETWEEN ?2 AND ?3",
                true,
            ),
            (
                "SELECT count(*), count(s), sum(s) FROM t WHERE g IS ?1 AND s != ?2",
                true,
            ),
            (
                "SELECT count(*), sum(s) FROM t WHERE g = ?1 AND s <= ?3 AND ?2 <= s",
                true,
            ),
            (
                "SELECT g, count(*), sum(s) FROM t WHERE s BETWEEN ?2 AND ?3 GROUP BY g",
                true,
            ),
            ("SELECT g, max(s), min(s), count(g) FROM t GROUP BY g", true),
            (
                "SELECT count(*), sum(b), sum(c) FROM u WHERE a = ?1 AND b >= ?2",
                true,
            ),
            (
                "SELECT count(*), total(b), avg(c), min(c), max(b) FROM u WHERE a = ?1 AND b = ?2",
                true,
            ),
            (
                "SELECT a, sum(b), total(b), avg(c), count(c) FROM u WHERE c != ?2 GROUP BY a",
                true,
            ),
            (
                "SELECT a, sum(c), count(*) FROM u WHERE b < ?3 GROUP BY a ORDER BY a DESC",
                true,
            ),
            (
                "SELECT c, count(*), sum(a) FROM u INDEXED BY u_c_desc GROUP BY c ORDER BY c DESC",
                true,
            ),
            (
                "SELECT count(*), sum(a), max(a) FROM u INDEXED BY u_c_desc WHERE c = ?2 AND a > ?1",
                true,
            ),
            (
                "SELECT x, count(*), sum(g), min(id) FROM t INDEXED BY t_xg \
                 WHERE g < ?1 GROUP BY x ORDER BY x",
                true,
            ),
            (
                "SELECT count(*), sum(g), sum(id) FROM t INDEXED BY t_xg WHERE x = ?1 AND g >= ?1",
                true,
            ),
            (
                "SELECT count(*), sum(s), total(g) FROM t INDEXED BY t_gs",
                true,
            ),
            // Shapes outside the kernel: they must simply keep working.
            (
                "SELECT count(*), sum(s) FROM t WHERE g = ?1 AND (s < ?2 OR s > ?3)",
                false,
            ),
            (
                "SELECT count(*), sum(s + 1) FROM t WHERE g = ?1 AND s BETWEEN ?2 AND ?3",
                false,
            ),
            (
                "SELECT count(*), sum(s) FROM t WHERE g = ?1 AND y BETWEEN ?2 AND ?3",
                false,
            ),
        ];

        type Outcome = std::result::Result<Vec<Vec<Value>>, (i32, String)>;
        fn run(stmt: &mut Statement, sql: &str, params: &[Value]) -> Outcome {
            for (i, p) in params.iter().enumerate() {
                if sql.contains(&format!("?{}", i + 1)) {
                    stmt.bind(i + 1, p.clone()).unwrap();
                }
            }
            let mut rows = Vec::new();
            let res = loop {
                match stmt.step() {
                    Ok(StepResult::Row(row)) => rows.push(row.into_values()),
                    Ok(StepResult::Done) => break Ok(rows),
                    Err(e) => break Err((e.extended_code, e.to_string())),
                }
            };
            stmt.reset().ok();
            res
        }

        // `extremes`: values near the i64 limits, so that sums overflow (then the outcome
        // depends on the order rows are aggregated in, which only the two plan variants share).
        for (page_size, n_rows, odd_per_64, extremes) in [
            (4096u32, 5_000u64, 2u64, true),
            (4096, 3_000, 2, false),
            (512, 2_000, 3, true),
            (512, 2_000, 4, false),
            (65_536, 1_500, 0, true),
            (1024, 1_000, 20, false),
            (4096, 0, 0, false),
            (512, 1, 0, false),
        ] {
            let kernel_conn = Connection::open_in_memory().unwrap();
            let general_conn = Connection::open_in_memory().unwrap();
            let generic_conn = Connection::open_in_memory().unwrap();
            let conns = [&kernel_conn, &general_conn, &generic_conn];
            let mut seed = 0x0D15_EA5E_F00D_4B1Du64 ^ (u64::from(page_size) << 20) ^ n_rows;
            for conn in conns {
                conn.execute_batch(&format!("PRAGMA page_size={page_size};"))
                    .unwrap();
                conn.execute_batch(SCHEMA).unwrap();
                conn.execute_batch("BEGIN").unwrap();
            }
            for id in 1..=n_rows {
                let t_row = [
                    Value::Integer(id as i64),
                    cell(&mut seed, 12, odd_per_64, extremes),
                    cell(&mut seed, 10_000, odd_per_64, extremes),
                    cell(&mut seed, 4, 24, extremes),
                    Value::Text(format!("y{}", id % 7)),
                ];
                // `u`: integers only, with sums that overflow i64 for some `a`.
                let r = next(&mut seed);
                let big = [i64::MAX / 2, i64::MAX / 3, i64::MIN / 2, 1 << 50];
                let u_row = [
                    Value::Integer(((r >> 8) % 9) as i64),
                    if extremes && (r >> 16).is_multiple_of(5) {
                        Value::Integer(big[((r >> 24) % 4) as usize])
                    } else {
                        Value::Integer(((r >> 24) % 300) as i64 - 20)
                    },
                    if extremes && (r >> 40).is_multiple_of(40) {
                        Value::Integer(i64::MAX - ((r >> 48) % 3) as i64)
                    } else {
                        Value::Integer(((r >> 48) % 50) as i64)
                    },
                    cell(&mut seed, 5, odd_per_64, extremes),
                ];
                for conn in conns {
                    conn.query("INSERT INTO t VALUES (?1, ?2, ?3, ?4, ?5)", &t_row)
                        .unwrap();
                    conn.query("INSERT INTO u VALUES (?1, ?2, ?3, ?4)", &u_row)
                        .unwrap();
                }
            }
            for conn in conns {
                conn.execute_batch("COMMIT").unwrap();
            }
            generic_conn
                .progress_handler(i32::MAX, Some(Box::new(|| false)))
                .unwrap();

            let mut stmts: Vec<[Statement; 3]> = statements
                .iter()
                .map(|(sql, _)| conns.map(|conn| conn.prepare(sql).unwrap()))
                .collect();
            for (trio, (sql, expect_kernel)) in stmts.iter_mut().zip(statements) {
                let plan = trio[0].compiled_fast_read.as_deref();
                assert_eq!(
                    plan.is_some_and(fast_read::FastReadPlan::has_int_kernel),
                    expect_kernel,
                    "integer kernel of {sql}"
                );
                // Same plan, every row through the general row code.
                let stripped = trio[1]
                    .compiled_fast_read
                    .as_deref()
                    .map(|p| Arc::new(p.without_int_kernel()));
                trio[1].compiled_fast_read = stripped;
            }

            let rows_before = fast_read::INT_KERNEL_ROWS.with(std::cell::Cell::get);
            let mut errors = 0u32;
            let mut nonempty = 0u32;
            for op in 0..700u32 {
                let which = (next(&mut seed) % statements.len() as u64) as usize;
                let sql = statements[which].0;
                let lo = operand(&mut seed, 10_000);
                let mut hi = operand(&mut seed, 10_000);
                if let (Value::Integer(a), Value::Integer(b)) = (&lo, &hi)
                    && !next(&mut seed).is_multiple_of(6)
                {
                    hi = Value::Integer((*a).max(*b));
                }
                let params = [operand(&mut seed, 12), lo, hi];
                let [k, g, v] = &mut stmts[which];
                let with_kernel = run(k, sql, &params);
                let without_kernel = run(g, sql, &params);
                let generic = run(v, sql, &params);
                assert_eq!(
                    with_kernel, without_kernel,
                    "op {op} page {page_size}: kernel vs general row code: {sql} with {params:?}"
                );
                if !extremes {
                    assert_eq!(
                        with_kernel, generic,
                        "op {op} page {page_size}: fast-read plan vs generic executor: {sql} with {params:?}"
                    );
                }
                match &with_kernel {
                    Err(_) => errors += 1,
                    Ok(rows) => {
                        if rows
                            .iter()
                            .any(|r| !matches!(r.first(), Some(Value::Integer(0))))
                        {
                            nonempty += 1;
                        }
                    }
                }
            }
            let (hits, misses) = fast_read::INT_KERNEL_ROWS.with(std::cell::Cell::get);
            let (hits, misses) = (hits - rows_before.0, misses - rows_before.1);
            if n_rows >= 1_000 {
                assert!(hits > 50_000, "page {page_size}: only {hits} kernel rows");
                assert!(
                    nonempty > 250,
                    "page {page_size}: only {nonempty} non-empty results"
                );
                if extremes {
                    assert!(
                        errors > 5,
                        "page {page_size}: only {errors} overflow errors"
                    );
                } else {
                    assert_eq!(errors, 0, "page {page_size}: unexpected errors");
                }
                if odd_per_64 > 0 {
                    assert!(
                        misses > 3_000,
                        "page {page_size}: only {misses} fallback rows"
                    );
                }
            }
        }
    }

    /// Streaming aggregate plans over a non-covering index scan must visit rows in index order
    /// (ascending groups, per-group rowid order here) and group `1` with `1.0`.
    #[test]
    fn fast_aggregate_over_non_covering_index_scan_and_mixed_numeric_group_keys() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE j(id INTEGER PRIMARY KEY, b, p, r REAL);
             CREATE INDEX j_b ON j(b, id);
             INSERT INTO j VALUES
               (1, 1,    '{\"v\":1,\"a\":true}',     0.5),
               (2, 1.0,  '{\"v\":2.5,\"a\":true}',   0.25),
               (3, 0,    '{\"v\":\"7\",\"a\":false}', 1.0),
               (4, NULL, '{\"v\":4,\"a\":true}',     2.0),
               (5, 'x',  '{\"a\":true}',             3.0),
               (6, 1,    jsonb('{\"v\":10,\"a\":1}'), 4.0),
               (7, 0,    '{\"v\":3,\"a\":true}',     5.0);",
        )
        .unwrap();
        let rows = |sql: &str| -> Vec<Vec<Value>> {
            conn.query(sql, &[])
                .unwrap()
                .rows
                .into_iter()
                .map(Row::into_values)
                .collect()
        };
        let text = |s: &str| Value::Text(s.to_owned());

        assert_eq!(
            rows(
                "SELECT b, count(*), sum(p ->> '$.v'), group_concat(id) FROM j \
                  WHERE (p ->> '$.a') = 1 GROUP BY b ORDER BY b"
            ),
            vec![
                vec![Value::Null, Value::Integer(1), Value::Integer(4), text("4")],
                vec![
                    Value::Integer(0),
                    Value::Integer(1),
                    Value::Integer(3),
                    text("7")
                ],
                vec![
                    Value::Integer(1),
                    Value::Integer(3),
                    Value::Real(13.5),
                    text("1,2,6")
                ],
                vec![text("x"), Value::Integer(1), Value::Null, text("5")],
            ]
        );
        let counts = vec![
            vec![Value::Null, Value::Integer(1)],
            vec![Value::Integer(0), Value::Integer(2)],
            vec![Value::Integer(1), Value::Integer(3)],
            vec![text("x"), Value::Integer(1)],
        ];
        assert_eq!(
            rows("SELECT b, count(p) FROM j INDEXED BY j_b GROUP BY b"),
            counts
        );
        assert_eq!(
            rows("SELECT b, count(p) FROM j NOT INDEXED GROUP BY b ORDER BY b"),
            counts
        );
        assert_eq!(
            rows(
                "SELECT count(*), sum(r), group_concat(id) FROM j INDEXED BY j_b \
                  WHERE p IS NOT NULL"
            ),
            vec![vec![
                Value::Integer(7),
                Value::Real(15.75),
                text("4,3,7,1,2,6,5")
            ]]
        );

        conn.execute("INSERT INTO j VALUES (8, 0, '{\"v\":1,', 6.0)")
            .unwrap();
        let err = conn
            .query(
                "SELECT b, sum(p ->> '$.v') FROM j GROUP BY b ORDER BY b",
                &[],
            )
            .unwrap_err();
        assert_eq!(err.message, "malformed JSON");
    }

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

    /// The R-Tree module mutates cached nodes in place, keeps a cached bounding box per node and
    /// undoes failed statements / savepoints from small undo records. The tree that leaves
    /// behind must be the tree a cold cache computes.
    ///
    /// Connection `warm` runs every batch of statements inside one transaction (the cache stays
    /// dirty, so the in-place / incremental paths do all the work). Connection `cold` runs the
    /// same statements but reads a shadow table directly after each one, which flushes the
    /// module and drops its cache: every statement there starts from freshly decoded nodes.
    /// Each statement must have the same outcome on both; after each batch the two trees must
    /// be the same tree (same cells in the same order in every node, walking down from the
    /// root; node numbers are not compared because the number given to a new node depends on
    /// whether a node deleted by the same statement has reached the shadow table yet) and the
    /// rowid table must hold the same rows, `rtreecheck` must pass, a failed statement must
    /// leave the table as it was, and bounding-box aggregates (prepared once, re-bound every
    /// time) must equal a brute-force evaluation over the table's rows.
    #[test]
    fn rtree_warm_cache_tree_matches_cold_cache_tree() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        type Outcome = std::result::Result<Vec<Vec<Value>>, (i32, String)>;
        fn run(stmt: &mut Statement, params: &[Value]) -> Outcome {
            for (i, p) in params.iter().enumerate() {
                stmt.bind(i + 1, p.clone()).unwrap();
            }
            let mut rows = Vec::new();
            let res = loop {
                match stmt.step() {
                    Ok(StepResult::Row(row)) => rows.push(row.into_values()),
                    Ok(StepResult::Done) => break Ok(rows),
                    Err(e) => break Err((e.extended_code, e.to_string())),
                }
            };
            stmt.reset().ok();
            res
        }
        fn num(v: &Value) -> f64 {
            match v {
                Value::Integer(i) => *i as f64,
                Value::Real(r) => *r,
                other => panic!("non-numeric coordinate {other:?}"),
            }
        }
        /// One box: `2 * n_dim` coordinate values (`bad`: first dimension has min > max).
        fn coords(state: &mut u64, n_dim: usize, span: u64, bad: bool) -> Vec<Value> {
            let mut out = Vec::with_capacity(n_dim * 2);
            for d in 0..n_dim {
                let r = next(state);
                let lo = ((r >> 8) % span) as i64;
                let w = match (r >> 40) % 8 {
                    0 | 1 => 0,
                    2 | 3 => 1,
                    4 => 7,
                    5 => (span / 16) as i64,
                    6 => (span / 4) as i64,
                    _ => 3,
                };
                let (lo, hi) = if bad && d == 0 {
                    (lo + w + 5, lo)
                } else {
                    (lo, lo + w)
                };
                if (r >> 52).is_multiple_of(4) {
                    out.push(Value::Real(lo as f64 + 0.25));
                    out.push(Value::Real(hi as f64 + 0.75));
                } else if (r >> 52) % 4 == 1 {
                    out.push(Value::Real(lo as f64));
                    out.push(Value::Integer(hi));
                } else {
                    out.push(Value::Integer(lo));
                    out.push(Value::Integer(hi));
                }
            }
            out
        }

        /// The tree as (level, cell bytes) in depth-first cell order; child pointers left out.
        fn tree_shape(conn: &Connection, n_dim: usize) -> Vec<(u16, Vec<u8>)> {
            fn walk(
                nodes: &HashMap<i64, Vec<u8>>,
                nodeno: i64,
                level: u16,
                cell_len: usize,
                out: &mut Vec<(u16, Vec<u8>)>,
            ) {
                let data = &nodes[&nodeno];
                let n_cell = usize::from(u16::from_be_bytes([data[2], data[3]]));
                for cell in data[4..4 + n_cell * cell_len].chunks_exact(cell_len) {
                    if level == 0 {
                        out.push((0, cell.to_vec()));
                    } else {
                        out.push((level, cell[8..].to_vec()));
                        let child = i64::from_be_bytes(cell[..8].try_into().unwrap());
                        walk(nodes, child, level - 1, cell_len, out);
                    }
                }
            }
            let mut nodes = HashMap::new();
            for row in conn
                .query("SELECT nodeno, data FROM t_node", &[])
                .unwrap()
                .rows
            {
                let mut vals = row.into_values();
                let Some(Value::Blob(data)) = vals.pop() else {
                    panic!("node data is not a blob");
                };
                nodes.insert(vals[0].as_i64().unwrap(), data);
            }
            let depth = u16::from_be_bytes([nodes[&1][0], nodes[&1][1]]);
            let mut out = Vec::new();
            walk(&nodes, 1, depth, 8 + n_dim * 8, &mut out);
            out
        }

        let mut deepest = 0i64;
        for (module, n_dim, n_aux, page_size, n_batches, batch_ops, span) in [
            ("rtree", 2usize, 0usize, 4096u32, 14u64, 260u64, 2000u64),
            ("rtree", 1, 1, 512, 24, 60, 400),
            ("rtree_i32", 2, 0, 512, 24, 70, 300),
            ("rtree", 3, 2, 1024, 16, 80, 100),
            ("rtree_i32", 5, 0, 4096, 6, 120, 50),
            ("rtree", 2, 0, 512, 30, 50, 1_000_000),
        ] {
            let mut seed = 0x5EED_0FA7_7EE5_u64 ^ (u64::from(page_size) << 24) ^ (n_dim as u64);
            let warm = Connection::open_in_memory().unwrap();
            let cold = Connection::open_in_memory().unwrap();
            let conns = [&warm, &cold];
            let coord_cols: Vec<String> = (0..n_dim * 2).map(|i| format!("c{i}")).collect();
            let aux_cols: Vec<String> = (0..n_aux).map(|i| format!("+a{i}")).collect();
            let n_cols = 1 + n_dim * 2 + n_aux;
            for conn in conns {
                conn.execute_batch(&format!(
                    "PRAGMA page_size={page_size};
                     CREATE VIRTUAL TABLE t USING {module}(id, {}{}{});",
                    coord_cols.join(", "),
                    if n_aux > 0 { ", " } else { "" },
                    aux_cols.join(", "),
                ))
                .unwrap();
            }
            let ph = |from: usize| -> String {
                (from..from + n_cols)
                    .map(|i| format!("?{i}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mut bbox_sql = String::from(
                "SELECT count(*), coalesce(sum(id), 0) FROM t WHERE c1 >= ?1 AND c0 <= ?2",
            );
            if n_dim >= 2 {
                bbox_sql.push_str(" AND c3 >= ?3 AND c2 <= ?4");
            }
            let sqls: Vec<String> = vec![
                format!("INSERT INTO t VALUES ({})", ph(1)),
                format!("INSERT OR REPLACE INTO t VALUES ({})", ph(1)),
                format!(
                    "INSERT INTO t VALUES ({}), ({}), ({})",
                    ph(1),
                    ph(1 + n_cols),
                    ph(1 + 2 * n_cols)
                ),
                "UPDATE t SET c0 = ?1, c1 = ?2 WHERE id = ?3".to_owned(),
                "UPDATE t SET id = ?1 WHERE id = ?2".to_owned(),
                "UPDATE OR REPLACE t SET id = ?1 WHERE id = ?2".to_owned(),
                "DELETE FROM t WHERE id = ?1".to_owned(),
                "DELETE FROM t WHERE c0 >= ?1 AND c1 <= ?2".to_owned(),
                bbox_sql,
                "SELECT * FROM t".to_owned(),
                "SELECT count(*) FROM t_node".to_owned(),
            ];
            const BBOX: usize = 8;
            const ALL_ROWS: usize = 9;
            const SHADOW_READ: usize = 10;
            let mut stmts: Vec<[Statement; 2]> = sqls
                .iter()
                .map(|sql| conns.map(|conn| conn.prepare(sql).unwrap()))
                .collect();
            let id_span = (n_batches * batch_ops) as i64;
            let mut next_seq = 0i64;
            let aux_vals = |state: &mut u64| -> Vec<Value> {
                (0..n_aux)
                    .map(|_| match next(state) % 4 {
                        0 => Value::Null,
                        1 => Value::Integer((next(state) % 100) as i64),
                        2 => Value::Text("aux".repeat((next(state) % 40) as usize)),
                        _ => Value::Real(1.5),
                    })
                    .collect()
            };

            for batch in 0..n_batches {
                let rolled_back = batch % 5 == 3;
                let sp_at = (batch % 3 == 1).then_some(batch_ops / 3);
                for conn in conns {
                    conn.execute_batch("BEGIN").unwrap();
                }
                // Shrinking phases delete much more than they insert (condense / re-insert).
                let shrinking = batch % 7 == 5;
                for op in 0..batch_ops {
                    if Some(op) == sp_at {
                        for conn in conns {
                            conn.execute_batch("SAVEPOINT s").unwrap();
                        }
                    }
                    if sp_at.is_some_and(|at| op == at + batch_ops / 3) {
                        let sql = if batch % 2 == 1 {
                            "ROLLBACK TO s; RELEASE s"
                        } else {
                            "RELEASE s"
                        };
                        for conn in conns {
                            conn.execute_batch(sql).unwrap();
                        }
                    }
                    let r = next(&mut seed);
                    let some_id = |state: &mut u64| -> Value {
                        Value::Integer((next(state) % (id_span as u64 + 2)) as i64)
                    };
                    let new_id = |state: &mut u64, seq: &mut i64| -> Value {
                        match next(state) % 8 {
                            0 => Value::Null,
                            1..=3 => {
                                *seq += 1;
                                Value::Integer(id_span + *seq)
                            }
                            _ => Value::Integer((next(state) % (id_span as u64 + 2)) as i64),
                        }
                    };
                    let pick = (r >> 8) % 100;
                    let (which, params): (usize, Vec<Value>) =
                        if pick < if shrinking { 20 } else { 62 } {
                            let mut p = vec![new_id(&mut seed, &mut next_seq)];
                            p.extend(coords(&mut seed, n_dim, span, (r >> 32).is_multiple_of(40)));
                            p.extend(aux_vals(&mut seed));
                            (usize::from((r >> 20).is_multiple_of(6)), p)
                        } else if pick < if shrinking { 24 } else { 68 } {
                            let mut p = Vec::new();
                            for row in 0..3 {
                                p.push(new_id(&mut seed, &mut next_seq));
                                p.extend(coords(
                                    &mut seed,
                                    n_dim,
                                    span,
                                    row == 2 && (r >> 32).is_multiple_of(2),
                                ));
                                p.extend(aux_vals(&mut seed));
                            }
                            (2, p)
                        } else if pick < 76 {
                            let c = coords(&mut seed, 1, span, (r >> 32).is_multiple_of(30));
                            (3, vec![c[0].clone(), c[1].clone(), some_id(&mut seed)])
                        } else if pick < 82 {
                            let target = new_id(&mut seed, &mut next_seq);
                            (
                                4 + usize::from((r >> 20).is_multiple_of(2)),
                                vec![target, some_id(&mut seed)],
                            )
                        } else if pick < if shrinking { 97 } else { 99 } {
                            (6, vec![some_id(&mut seed)])
                        } else {
                            let lo = ((r >> 16) % span) as i64;
                            (
                                7,
                                vec![Value::Integer(lo), Value::Integer(lo + (span / 3) as i64)],
                            )
                        };
                    // Read on the cold connection only: a SELECT inside the transaction would
                    // flush the warm connection's module too (it holds the same rows).
                    let before = run(&mut stmts[ALL_ROWS][1], &[]).unwrap();
                    let [w_stmt, c_stmt] = &mut stmts[which];
                    let w_res = run(w_stmt, &params);
                    let c_res = run(c_stmt, &params);
                    assert_eq!(
                        w_res, c_res,
                        "{module} batch {batch} op {op}: {} {params:?}",
                        sqls[which]
                    );
                    assert_eq!(warm.changes(), cold.changes(), "{} {params:?}", sqls[which]);
                    if w_res.is_err() {
                        let mut after = run(&mut stmts[ALL_ROWS][0], &[]).unwrap();
                        let mut before = before;
                        let key = |row: &Vec<Value>| row[0].as_i64().unwrap();
                        before.sort_by_key(key);
                        after.sort_by_key(key);
                        assert_eq!(
                            before, after,
                            "failed statement changed the table: {}",
                            sqls[which]
                        );
                    }
                    // Flushes the cold connection's tree and drops its node cache.
                    run(&mut stmts[SHADOW_READ][1], &[]).unwrap();
                }
                let end = if rolled_back { "ROLLBACK" } else { "COMMIT" };
                for conn in conns {
                    conn.execute_batch(end).unwrap();
                }

                assert!(
                    tree_shape(&warm, n_dim) == tree_shape(&cold, n_dim),
                    "{module} dim {n_dim} page {page_size} batch {batch}: trees differ"
                );
                let rowid_rows = |conn: &Connection| -> Vec<Vec<Value>> {
                    conn.query("SELECT * FROM t_rowid ORDER BY rowid", &[])
                        .unwrap()
                        .rows
                        .into_iter()
                        .map(|row| {
                            let mut vals = row.into_values();
                            vals.remove(1); // the leaf's node number
                            vals
                        })
                        .collect()
                };
                assert!(
                    rowid_rows(&warm) == rowid_rows(&cold),
                    "{module} dim {n_dim} page {page_size} batch {batch}: rowid tables differ"
                );
                for conn in conns {
                    let chk = conn.query("SELECT rtreecheck('t')", &[]).unwrap();
                    assert_eq!(chk.rows[0].get_str(0), Some("ok"), "{module} batch {batch}");
                }
                let depth = warm
                    .query("SELECT rtreedepth(data) FROM t_node WHERE nodeno = 1", &[])
                    .unwrap()
                    .rows[0]
                    .get_i64(0)
                    .unwrap();
                deepest = deepest.max(depth);

                let all = run(&mut stmts[ALL_ROWS][0], &[]).unwrap();
                for q in 0..12u64 {
                    let r = next(&mut seed);
                    let mut bounds = Vec::new();
                    for d in 0..n_dim.min(2) {
                        let lo = ((r >> (8 + 20 * d)) % span) as i64;
                        let w = ((r >> 50) % (span / 2 + 1)) as i64;
                        if q % 3 == 0 {
                            bounds.push(Value::Integer(lo));
                            bounds.push(Value::Integer(lo + w));
                        } else {
                            bounds.push(Value::Real(lo as f64 + 0.5));
                            bounds.push(Value::Real((lo + w) as f64 + 0.5));
                        }
                    }
                    let mut want_n = 0i64;
                    let mut want_sum = 0i64;
                    for row in &all {
                        let hit = (0..n_dim.min(2)).all(|d| {
                            num(&row[2 + 2 * d]) >= num(&bounds[2 * d])
                                && num(&row[1 + 2 * d]) <= num(&bounds[2 * d + 1])
                        });
                        if hit {
                            want_n += 1;
                            want_sum += row[0].as_i64().unwrap();
                        }
                    }
                    for (ci, stmt) in stmts[BBOX].iter_mut().enumerate() {
                        let got = run(stmt, &bounds).unwrap();
                        assert_eq!(
                            got,
                            vec![vec![Value::Integer(want_n), Value::Integer(want_sum)]],
                            "{module} dim {n_dim} batch {batch} conn {ci} bounds {bounds:?}"
                        );
                    }
                }
            }
        }
        assert!(
            deepest >= 2,
            "no configuration reached a three-level tree (depth {deepest})"
        );
    }

    /// Several virtual tables written by the same transactions must leave the same database
    /// file behind every time. Their dirty state reaches the shadow tables when the transaction
    /// callbacks fan out over the participating tables, so that order decides which b-tree
    /// gets which page; it has to be the order in which the tables joined the transaction, not
    /// the iteration order of a hash map (each connection's registry hashes with its own
    /// random seed, so fresh connections inside one process do exercise that).
    #[test]
    fn multi_vtab_transactions_write_identical_files_on_every_connection() {
        const SCHEMA: &str = "PRAGMA page_size=1024;
             CREATE VIRTUAL TABLE b USING rtree(id, x0, x1, y0, y1, +tag);
             CREATE VIRTUAL TABLE bi USING rtree_i32(id, x0, x1);
             CREATE VIRTUAL TABLE c USING rtree(id, x0, x1, y0, y1, z0, z1);
             CREATE VIRTUAL TABLE d USING rtree_i32(id, x0, x1, y0, y1);
             CREATE VIRTUAL TABLE docs USING fts5(body);
             CREATE TABLE n(id INTEGER PRIMARY KEY, v);";
        fn build_script() -> String {
            let mut state = 0x00C0_FFEE_1234_5678_u64;
            let mut next = || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state
            };
            let mut sql = String::new();
            let mut id = 0u64;
            for block in 0..18u64 {
                let explicit = block % 4 != 3;
                if explicit {
                    sql.push_str("BEGIN;\n");
                }
                for _ in 0..40 {
                    id += 1;
                    let x = next() % 9000;
                    let y = next() % 9000;
                    let w = next() % 300;
                    // The tables join each transaction in an order that changes from block to
                    // block, and not every table takes part in every block.
                    let mut order = [0usize, 1, 2, 3, 4];
                    order.rotate_left((block % 5) as usize);
                    if block % 2 == 1 {
                        order.reverse();
                    }
                    for which in order {
                        if (block + which as u64) % 6 == 5 {
                            continue;
                        }
                        let stmt = match which {
                            0 => format!(
                                "INSERT INTO b VALUES({id},{x}.5,{}.25,{y},{},'t{}');\n",
                                x + w + 1,
                                y + w,
                                id % 7
                            ),
                            1 => format!("INSERT INTO bi VALUES({id},{x},{});\n", x + w),
                            2 => format!(
                                "INSERT INTO c VALUES({id},{x},{},{y},{},{w},{});\n",
                                x + w,
                                y + 3,
                                w + 1
                            ),
                            3 => {
                                format!("INSERT INTO d VALUES({id},{x},{},{y},{});\n", x + w, y + w)
                            }
                            _ => format!(
                                "INSERT INTO docs(rowid, body) VALUES({id}, 'w{} w{} common');\n",
                                x % 50,
                                y % 50
                            ),
                        };
                        sql.push_str(&stmt);
                    }
                    sql.push_str(&format!("INSERT INTO n VALUES({id},{});\n", id % 11));
                    if id.is_multiple_of(9) {
                        let victim = 1 + next() % id;
                        sql.push_str(&format!(
                            "DELETE FROM b WHERE id={victim}; DELETE FROM d WHERE id={victim};\n"
                        ));
                    }
                    if id.is_multiple_of(13) {
                        let target = 1 + next() % id;
                        sql.push_str(&format!(
                            "UPDATE bi SET x0={x}, x1={} WHERE id={target};\n",
                            x + 5
                        ));
                    }
                }
                if explicit {
                    sql.push_str(if block % 6 == 4 {
                        "ROLLBACK;\n"
                    } else {
                        "COMMIT;\n"
                    });
                }
            }
            sql
        }
        fn file_bytes(vfs: &MemoryVfs, path: &Path) -> Vec<u8> {
            let file = vfs
                .open(Some(path), OpenFlags::READONLY | OpenFlags::MAIN_DB)
                .unwrap();
            let mut bytes = vec![0u8; file.file_size().unwrap() as usize];
            assert_eq!(file.read_at(&mut bytes, 0).unwrap(), bytes.len());
            bytes
        }

        let script = build_script();
        let path = Path::new("/multi.db");
        let mut first: Option<Vec<u8>> = None;
        for run in 0..16 {
            let vfs = Arc::new(MemoryVfs::new());
            let conn = Connection::open_with_vfs(
                vfs.clone(),
                Some(path),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.execute_batch(&script).unwrap();
            for table in ["b", "bi", "c", "d"] {
                let chk = conn
                    .query(&format!("SELECT rtreecheck('{table}')"), &[])
                    .unwrap();
                assert_eq!(chk.rows[0].get_str(0), Some("ok"), "{table}");
            }
            let ic = conn.query("PRAGMA integrity_check", &[]).unwrap();
            assert_eq!(ic.rows[0].get_str(0), Some("ok"));
            assert!(conn.try_close().is_ok());
            let bytes = file_bytes(&vfs, path);
            assert!(
                bytes.len() > 100 * 1024,
                "only {} bytes written",
                bytes.len()
            );
            match &first {
                None => first = Some(bytes),
                Some(want) => {
                    let diff = want.iter().zip(&bytes).position(|(a, b)| a != b);
                    assert!(
                        *want == bytes,
                        "run {run}: file differs from run 0 (lengths {} / {}, first difference at {diff:?})",
                        want.len(),
                        bytes.len()
                    );
                }
            }
        }
    }

    /// `PRAGMA integrity_check` / `quick_check` must answer like C SQLite 3.54.1. Every expected
    /// row below is what the reference `sqlite3` prints for the same file.
    #[test]
    fn integrity_check_reports_what_c_sqlite_reports() {
        fn open(vfs: &Arc<MemoryVfs>, path: &Path) -> Connection {
            Connection::open_with_vfs(
                vfs.clone(),
                Some(path),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            )
            .unwrap()
        }
        fn rows(conn: &Connection, sql: &str) -> Vec<String> {
            conn.query(sql, &[])
                .unwrap()
                .rows
                .iter()
                .map(|r| r.get_str(0).unwrap().to_owned())
                .collect()
        }
        fn patch(vfs: &MemoryVfs, path: &Path, offset: u64, bytes: &[u8]) {
            let file = vfs
                .open(Some(path), OpenFlags::READWRITE | OpenFlags::MAIN_DB)
                .unwrap();
            file.write_at(bytes, offset).unwrap();
        }

        // 1. Rows that violate what the (edited) schema declares.
        let vfs = Arc::new(MemoryVfs::new());
        let path = Path::new("/rows.db");
        let conn = open(&vfs, path);
        conn.execute_batch(
            "PRAGMA page_size=1024;
             CREATE TABLE t1(a,b);
             CREATE INDEX t1a ON t1(a);
             INSERT INTO t1 VALUES(1,1),(2,2),(3,3),(2,4),(NULL,5),(NULL,6);
             CREATE TABLE s(id INTEGER PRIMARY KEY, i, t, r, bl, any1);
             INSERT INTO s VALUES(1, 5, 'x', 1.5, x'00', 1),(2, 'five', 7, 'r', 9, 'z'),
                                 (3, 2.5, x'01', 3, 'b', NULL),(4, NULL, NULL, NULL, NULL, NULL);
             CREATE TABLE aff(x, y, z);
             INSERT INTO aff VALUES(1, '12', 'abc'),(2.5, '1e2', 5),('t', ' 7 ', 7.5),(x'00', 'x1', NULL);
             CREATE TABLE ck(p, q);
             INSERT INTO ck VALUES(1, 2),(5, 1),(NULL, 3),(7, NULL),(0, 0);
             CREATE TABLE w(k TEXT PRIMARY KEY, v) WITHOUT ROWID;
             INSERT INTO w VALUES('a',1),('b',2),('c',NULL);",
        )
        .unwrap();
        assert_eq!(rows(&conn, "PRAGMA integrity_check"), ["ok"]);
        assert_eq!(rows(&conn, "PRAGMA quick_check"), ["ok"]);
        conn.execute_batch(
            "PRAGMA writable_schema=ON;
             UPDATE sqlite_master SET sql='CREATE UNIQUE INDEX t1a ON t1(a)' WHERE name='t1a';
             UPDATE sqlite_master SET sql='CREATE TABLE t1(a NOT NULL,b)' WHERE name='t1';
             UPDATE sqlite_master SET sql='CREATE TABLE s(id INTEGER PRIMARY KEY, i INT, t TEXT, r REAL, bl BLOB, any1 ANY) STRICT' WHERE name='s';
             UPDATE sqlite_master SET sql='CREATE TABLE aff(x TEXT, y NUMERIC, z INTEGER)' WHERE name='aff';
             UPDATE sqlite_master SET sql='CREATE TABLE ck(p, q, CHECK(p > q), CHECK(q <> 3))' WHERE name='ck';
             UPDATE sqlite_master SET sql='CREATE TABLE w(k TEXT PRIMARY KEY, v NOT NULL) WITHOUT ROWID' WHERE name='w';
             PRAGMA writable_schema=OFF;",
        )
        .unwrap();
        assert!(conn.try_close().is_ok());
        let conn = open(&vfs, path);
        let full = [
            "NULL value in w.v",
            "CHECK constraint failed in ck",
            "CHECK constraint failed in ck",
            "CHECK constraint failed in ck",
            "non-unique entry in index t1a",
            "NULL value in t1.a",
            "non-unique entry in index t1a",
            "NULL value in t1.a",
            "non-INT value in s.i",
            "non-TEXT value in s.t",
            "non-REAL value in s.r",
            "non-BLOB value in s.bl",
            "non-INT value in s.i",
            "non-TEXT value in s.t",
            "non-BLOB value in s.bl",
            "NUMERIC value in aff.x",
            "TEXT value in aff.y",
            "NUMERIC value in aff.x",
            "TEXT value in aff.y",
            "TEXT value in aff.y",
        ];
        assert_eq!(rows(&conn, "PRAGMA integrity_check"), full);
        // quick_check skips the index checks only.
        let quick: Vec<&str> = full
            .iter()
            .copied()
            .filter(|m| !m.starts_with("non-unique"))
            .collect();
        assert_eq!(rows(&conn, "PRAGMA quick_check"), quick);
        // The argument is an error limit or the one table to check.
        assert_eq!(rows(&conn, "PRAGMA integrity_check(4)"), full[..4]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(1)"), full[..1]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(0)"), full);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(s)"), full[8..15]);
        assert_eq!(rows(&conn, "PRAGMA quick_check(aff)"), full[15..]);
        assert_eq!(rows(&conn, "PRAGMA main.integrity_check(w)"), full[..1]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(sqlite_schema)"), ["ok"]);
        for (sql, missing) in [
            ("PRAGMA integrity_check(xyz)", "xyz"),
            ("PRAGMA integrity_check='4'", "4"),
        ] {
            let err = conn.query(sql, &[]).unwrap_err();
            assert_eq!(err.message, format!("no such table: {missing}"), "{sql}");
        }
        assert_eq!(
            conn.query("PRAGMA quick_check", &[]).unwrap().column_names,
            ["quick_check"]
        );
        assert_eq!(
            conn.query("PRAGMA integrity_check", &[])
                .unwrap()
                .column_names,
            ["integrity_check"]
        );
        assert!(conn.try_close().is_ok());

        // 2. An index that lost its entries (`pragma-3.2` .. `pragma-3.7` of pragma.test) and a
        //    page whose header disagrees with its content.
        let vfs = Arc::new(MemoryVfs::new());
        let path = Path::new("/pages.db");
        let conn = open(&vfs, path);
        conn.execute_batch(
            "PRAGMA page_size=1024; PRAGMA auto_vacuum=OFF;
             BEGIN;
             CREATE TABLE t2(a,b,c);
             CREATE INDEX i2 ON t2(a);
             INSERT INTO t2 VALUES(11,2,3);
             INSERT INTO t2 VALUES(22,3,4);
             COMMIT;",
        )
        .unwrap();
        let roots = conn
            .query("SELECT rootpage FROM sqlite_master ORDER BY name", &[])
            .unwrap();
        let root_of = |i: usize| match roots.rows[i].values()[0] {
            Value::Integer(pg) => pg as u64,
            ref other => panic!("rootpage {other:?}"),
        };
        let (i2_root, t2_root) = (root_of(0), root_of(1));
        assert!(conn.try_close().is_ok());

        // The index root becomes an empty leaf.
        patch(
            &vfs,
            path,
            1024 * (i2_root - 1),
            &[0x0a, 0, 0, 0, 0, 0x04, 0, 0, 0, 0, 0],
        );
        let conn = open(&vfs, path);
        let lost = [
            "wrong # of entries in index i2",
            "row 1 missing from index i2",
            "row 2 missing from index i2",
        ];
        assert_eq!(rows(&conn, "PRAGMA integrity_check"), lost);
        assert_eq!(rows(&conn, "PRAGMA integrity_check=1"), lost[..1]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check=t2"), lost);
        assert_eq!(rows(&conn, "PRAGMA quick_check"), lost[..1]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check=sqlite_schema"), ["ok"]);
        conn.execute_batch("REINDEX t2").unwrap();
        assert_eq!(rows(&conn, "PRAGMA integrity_check"), ["ok"]);
        assert!(conn.try_close().is_ok());

        // The table leaf claims five fragmented bytes it does not have: all b-tree level
        // findings of one database come back as a single row.
        patch(&vfs, path, 1024 * (t2_root - 1) + 7, &[5]);
        let conn = open(&vfs, path);
        let frag = format!(
            "*** in database main ***\nFragmentation of 0 bytes reported as 5 on page {t2_root}"
        );
        assert_eq!(rows(&conn, "PRAGMA integrity_check"), [frag.as_str()]);
        assert_eq!(rows(&conn, "PRAGMA quick_check"), [frag.as_str()]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(t2)"), [frag.as_str()]);
        assert_eq!(rows(&conn, "PRAGMA integrity_check(sqlite_schema)"), ["ok"]);
        assert!(conn.try_close().is_ok());
    }
}

#[cfg(test)]
mod c_align_5g_tests;
/// Regression tests for behaviour that has to match C SQLite 3.54.1 (step 5e): every expected
/// value below is what the reference `sqlite3` produces for the same statements.
#[cfg(test)]
mod fts5_c_compat_tests;

#[cfg(test)]
mod c_parity_tests {
    use super::*;
    use sqliters_vfs::MemoryVfs;

    fn open(vfs: &Arc<MemoryVfs>, path: &Path) -> Connection {
        Connection::open_with_vfs(
            vfs.clone(),
            Some(path),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap()
    }
    fn file_bytes(vfs: &MemoryVfs, path: &Path) -> Vec<u8> {
        let file = vfs
            .open(Some(path), OpenFlags::READONLY | OpenFlags::MAIN_DB)
            .unwrap();
        let mut bytes = vec![0u8; file.file_size().unwrap() as usize];
        assert_eq!(file.read_at(&mut bytes, 0).unwrap(), bytes.len());
        bytes
    }
    fn count(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }
    /// The single integer a statement returns, or `None` when it returns no row.
    fn int(conn: &Connection, sql: &str) -> Option<i64> {
        let res = conn.query(sql, &[]).unwrap();
        res.rows.first().map(|r| r.get_i64(0).unwrap())
    }
    fn texts(conn: &Connection, sql: &str) -> Vec<String> {
        conn.query(sql, &[])
            .unwrap()
            .rows
            .iter()
            .map(|r| r.get_str(0).unwrap().to_owned())
            .collect()
    }

    /// `PRAGMA secure_delete` (`pragma.c` `PragTyp_SECURE_DELETE`): every form returns the
    /// setting, `FAST` is 2, other values are booleans, an unqualified assignment reaches every
    /// open database, `ATTACH` inherits the mode of `main`.
    #[test]
    fn secure_delete_pragma_forms() {
        let conn = Connection::open_in_memory().unwrap();
        let col = conn.query("PRAGMA secure_delete", &[]).unwrap();
        assert_eq!(col.column_names, ["secure_delete"]);
        for (sql, want) in [
            ("PRAGMA secure_delete", 0),
            ("PRAGMA secure_delete=FAST", 2),
            ("PRAGMA secure_delete", 2),
            ("PRAGMA secure_delete=ON", 1),
            ("PRAGMA main.secure_delete=0", 0),
            ("PRAGMA secure_delete", 0),
            ("PRAGMA secure_delete=2", 1),
            ("PRAGMA secure_delete=3", 1),
            ("PRAGMA secure_delete=foo", 0),
            ("PRAGMA secure_delete(1)", 1),
            // `temp` is opened by the qualified query and was not open for the assignments.
            ("PRAGMA temp.secure_delete", 0),
            ("PRAGMA secure_delete='fast'", 2),
            ("PRAGMA temp.secure_delete", 2),
            ("PRAGMA temp.secure_delete=off", 0),
            ("PRAGMA secure_delete", 2),
            ("PRAGMA secure_delete=-1", 0),
            ("PRAGMA secure_delete=256", 1),
            ("PRAGMA secure_delete=1.5", 1),
            ("PRAGMA secure_delete=0x10", 1),
            ("PRAGMA secure_delete=00", 0),
            ("PRAGMA secure_delete=yes", 1),
            ("PRAGMA secure_delete=no", 0),
            ("PRAGMA secure_delete=fAsT", 2),
        ] {
            assert_eq!(int(&conn, sql), Some(want), "{sql}");
        }
        conn.execute_batch("ATTACH ':memory:' AS a2").unwrap();
        assert_eq!(int(&conn, "PRAGMA a2.secure_delete"), Some(2));
        assert_eq!(int(&conn, "PRAGMA a2.secure_delete=1"), Some(1));
        assert_eq!(int(&conn, "PRAGMA secure_delete"), Some(2));
        assert_eq!(int(&conn, "PRAGMA secure_delete=off"), Some(0));
        assert_eq!(int(&conn, "PRAGMA a2.secure_delete"), Some(0));
    }

    /// With `secure_delete` on, nothing of a deleted / replaced / dropped row may stay in the
    /// database file — in rollback and WAL mode, with and without auto-vacuum, for table rows,
    /// index entries, overflow pages, truncated and dropped tables, deleted schema rows and
    /// after `VACUUM`. With the mode off the same script leaves the content behind (which is
    /// what makes the assertion meaningful) and the file is otherwise identical in structure.
    #[test]
    fn secure_delete_leaves_no_deleted_content_in_the_file() {
        fn script(mode: &str, journal: &str, auto_vacuum: &str) -> String {
            let mut sql = format!(
                "PRAGMA auto_vacuum={auto_vacuum};\nPRAGMA journal_mode={journal};\n\
                 PRAGMA secure_delete={mode};\n\
                 CREATE TABLE t(id INTEGER PRIMARY KEY, a INT, b TEXT);\n\
                 CREATE INDEX tb ON t(b);\n\
                 CREATE TABLE big(id INTEGER PRIMARY KEY, b TEXT);\n\
                 CREATE TABLE gone(id INTEGER PRIMARY KEY, b TEXT);\n\
                 CREATE INDEX gone_b ON gone(b);\n\
                 CREATE TABLE emptied(id INTEGER PRIMARY KEY, b TEXT);\n\
                 CREATE TABLE one(id INTEGER PRIMARY KEY, b TEXT UNIQUE);\n\
                 CREATE VIEW v_doomed AS SELECT 'DELME-in-a-view-definition' AS x;\n\
                 BEGIN;\n"
            );
            for i in 1..=400 {
                let tag = if i % 3 == 0 { "DELME" } else { "KEEPME" };
                let pad = "p".repeat(5 + (i * 7) % 60);
                sql.push_str(&format!(
                    "INSERT INTO t VALUES({i},{i},'{tag}-{i}-{pad}');\n"
                ));
                sql.push_str(&format!(
                    "INSERT INTO gone VALUES({i},'DELME-{i}-{pad}');\n"
                ));
                sql.push_str(&format!(
                    "INSERT INTO emptied VALUES({i},'DELME-{i}-{pad}');\n"
                ));
            }
            for i in 1..=6 {
                let wide = "DELME".repeat(1500);
                sql.push_str(&format!("INSERT INTO big VALUES({i},'{wide}{i}');\n"));
            }
            sql.push_str("INSERT INTO one VALUES(1,'DELME-only-row');\nCOMMIT;\n");
            sql.push_str(
                "DELETE FROM t WHERE id%3=0 AND id%2=0;\n\
                 UPDATE t SET b='KEEPME-'||id WHERE id%3=0 AND id%2=1 AND id<200;\n\
                 UPDATE t SET b='KEEPME-'||id||'-grown-well-beyond-the-length-of-the-old-value-\
                 so-that-the-cell-has-to-move-somewhere-else' WHERE id%3=0 AND id%2=1;\n\
                 DELETE FROM big WHERE id<>3;\n\
                 UPDATE big SET b='KEEPME';\n\
                 DELETE FROM emptied;\n\
                 DROP TABLE gone;\n\
                 DELETE FROM one;\n\
                 DROP VIEW v_doomed;\n",
            );
            sql
        }
        for journal in ["delete", "wal"] {
            for auto_vacuum in ["none", "full"] {
                let mut residue = Vec::new();
                for mode in ["OFF", "ON", "FAST"] {
                    let vfs = Arc::new(MemoryVfs::new());
                    let path = Path::new("/sd.db");
                    let conn = open(&vfs, path);
                    conn.execute_batch(&script(mode, journal, auto_vacuum))
                        .unwrap();
                    assert_eq!(texts(&conn, "PRAGMA integrity_check"), ["ok"]);
                    assert_eq!(int(&conn, "SELECT count(*) FROM t"), Some(334));
                    if journal == "wal" {
                        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                            .unwrap();
                    }
                    let before_vacuum = count(&file_bytes(&vfs, path), b"DELME");
                    conn.execute_batch("VACUUM").unwrap();
                    if journal == "wal" {
                        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                            .unwrap();
                    }
                    assert!(conn.try_close().is_ok());
                    let bytes = file_bytes(&vfs, path);
                    assert_eq!(
                        count(&bytes, b"DELME"),
                        0,
                        "{journal}/{auto_vacuum}/{mode}: content survived VACUUM"
                    );
                    assert!(count(&bytes, b"KEEPME") >= 334);
                    residue.push((mode, before_vacuum));
                }
                assert!(
                    residue[0].1 > 0,
                    "{journal}/{auto_vacuum}: with secure_delete=OFF deleted cells stay in place"
                );
                assert_eq!(residue[1].1, 0, "{journal}/{auto_vacuum}: secure_delete=ON");
                // FAST zeroes freed cells but, as in C (`freePage2` tests `BTS_SECURE_DELETE`
                // only), not the pages that go to the free list.
                assert!(
                    residue[2].1 <= residue[0].1,
                    "{journal}/{auto_vacuum}: secure_delete=FAST leaves {} markers, OFF {}",
                    residue[2].1,
                    residue[0].1
                );
            }
        }
    }

    /// `analysis_limit`, `cell_size_check`, `mmap_size` and `data_version` return what C
    /// returns: a row with the setting (no row for the `cell_size_check` assignment).
    #[test]
    fn pragmas_that_report_a_setting_return_one_row() {
        let conn = Connection::open_in_memory().unwrap();
        for (sql, want) in [
            ("PRAGMA analysis_limit", Some(0)),
            ("PRAGMA analysis_limit=100", Some(100)),
            ("PRAGMA analysis_limit=-5", Some(100)),
            ("PRAGMA analysis_limit=abc", Some(100)),
            ("PRAGMA analysis_limit=0x10", Some(16)),
            ("PRAGMA analysis_limit=1.5", Some(16)),
            ("PRAGMA analysis_limit='20'", Some(20)),
            ("PRAGMA analysis_limit=4294967297", Some(1)),
            ("PRAGMA analysis_limit=2147483648", Some(0)),
            (
                "PRAGMA analysis_limit=9223372036854775807",
                Some(2_147_483_647),
            ),
            ("PRAGMA main.analysis_limit=3", Some(3)),
            ("PRAGMA temp.analysis_limit", Some(3)),
            ("PRAGMA analysis_limit(44)", Some(44)),
            ("PRAGMA analysis_limit=+7", Some(7)),
            ("PRAGMA analysis_limit=on", Some(7)),
            ("PRAGMA analysis_limit=' 12 '", Some(12)),
            ("PRAGMA analysis_limit='0X1f'", Some(31)),
            ("PRAGMA analysis_limit='12abc'", Some(31)),
            ("PRAGMA analysis_limit=0", Some(0)),
            ("PRAGMA cell_size_check", Some(0)),
            ("PRAGMA cell_size_check=1", None),
            ("PRAGMA cell_size_check", Some(1)),
            ("PRAGMA cell_size_check=off", None),
            ("PRAGMA cell_size_check", Some(0)),
            ("PRAGMA main.cell_size_check=yes", None),
            ("PRAGMA temp.cell_size_check", Some(1)),
            ("PRAGMA cell_size_check=foo", None),
            ("PRAGMA cell_size_check", Some(0)),
            ("PRAGMA cell_size_check(true)", None),
            ("PRAGMA cell_size_check", Some(1)),
            ("PRAGMA cell_size_check=-1", None),
            ("PRAGMA cell_size_check", Some(0)),
            // No memory mapping in this engine: the effective limit is always 0, as in a C
            // build with `SQLITE_MAX_MMAP_SIZE=0`.
            ("PRAGMA mmap_size", Some(0)),
            ("PRAGMA mmap_size=1048576", Some(0)),
            ("PRAGMA main.mmap_size", Some(0)),
            ("PRAGMA data_version", Some(1)),
            ("PRAGMA data_version=7", Some(1)),
            ("PRAGMA temp.data_version", Some(1)),
        ] {
            assert_eq!(int(&conn, sql), want, "{sql}");
        }
        for (sql, name) in [
            ("PRAGMA analysis_limit", "analysis_limit"),
            ("PRAGMA cell_size_check", "cell_size_check"),
            ("PRAGMA mmap_size", "mmap_size"),
            ("PRAGMA data_version", "data_version"),
        ] {
            assert_eq!(conn.query(sql, &[]).unwrap().column_names, [name], "{sql}");
        }
    }

    /// `ANALYZE` under `PRAGMA analysis_limit` produces the `sqlite_stat1` rows C produces:
    /// the scan of an index stops (or skips ahead) once the limit is exceeded, the first number
    /// is still the exact entry count, the averages come from the rows visited.
    #[test]
    fn analyze_honours_analysis_limit() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t(id INTEGER PRIMARY KEY, a INT, b TEXT, c INT);
             CREATE INDEX ta ON t(a);
             CREATE INDEX tab ON t(a,b);
             CREATE INDEX tc ON t(c DESC, a);
             CREATE TABLE same(a INT, b INT);
             CREATE INDEX sab ON same(a,b);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<2000)
               INSERT INTO t SELECT i, CASE WHEN i%40=0 THEN NULL ELSE i%7 END, 'v'||(i%13), i/50 FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<900)
               INSERT INTO same SELECT 5, i%3 FROM n;",
        )
        .unwrap();
        let unlimited = [
            "sab=900 900 300",
            "ta=2000 250",
            "tab=2000 250 20",
            "tc=2000 49 7",
        ];
        for (limit, want) in [
            (0, unlimited),
            (
                1,
                ["sab=900 2 2", "ta=2000 2", "tab=2000 2 2", "tc=2000 1 1"],
            ),
            (
                10,
                [
                    "sab=900 11 11",
                    "ta=2000 11",
                    "tab=2000 11 6",
                    "tc=2000 6 3",
                ],
            ),
            (
                300,
                [
                    "sab=900 301 151",
                    "ta=2000 151",
                    "tab=2000 151 13",
                    "tc=2000 43 7",
                ],
            ),
            (5000, unlimited),
        ] {
            assert_eq!(
                int(&conn, &format!("PRAGMA analysis_limit={limit}")),
                Some(limit)
            );
            conn.execute_batch("ANALYZE").unwrap();
            assert_eq!(
                texts(
                    &conn,
                    "SELECT idx||'='||stat FROM sqlite_stat1 ORDER BY idx"
                ),
                want,
                "analysis_limit={limit}"
            );
        }
    }

    /// `PRAGMA data_version` moves when another connection commits and stays put for commits,
    /// rollbacks, `VACUUM` and checkpoints of the connection itself. The absolute values are
    /// C's (1 after open; +1 for the first snapshot through a WAL).
    #[test]
    fn data_version_changes_only_for_other_connections() {
        for (journal, start) in [("delete", 1i64), ("wal", 2), ("truncate", 1)] {
            let vfs = Arc::new(MemoryVfs::new());
            let path = Path::new("/dv.db");
            let a = open(&vfs, path);
            let b = open(&vfs, path);
            let dv = |c: &Connection| int(c, "PRAGMA data_version").unwrap();
            a.execute_batch(&format!(
                "PRAGMA journal_mode={journal}; CREATE TABLE t(a);"
            ))
            .unwrap();
            assert_eq!(int(&b, "SELECT count(*) FROM t"), Some(0));
            assert_eq!(dv(&a), start, "{journal}");
            a.execute_batch(
                "INSERT INTO t VALUES(1);
                 BEGIN; INSERT INTO t VALUES(2); COMMIT;
                 BEGIN; INSERT INTO t VALUES(3); ROLLBACK;
                 CREATE INDEX ta ON t(a); DROP INDEX ta;
                 PRAGMA user_version=3;
                 VACUUM;",
            )
            .unwrap();
            assert_eq!(dv(&a), start, "{journal}: own commits");

            b.execute_batch("INSERT INTO t VALUES(10)").unwrap();
            assert_eq!(dv(&a), start + 1, "{journal}: commit by another connection");
            assert_eq!(dv(&a), start + 1, "{journal}: stable between changes");
            b.execute_batch("BEGIN; INSERT INTO t VALUES(11); ROLLBACK;")
                .unwrap();
            assert_eq!(int(&b, "SELECT count(*) FROM t"), Some(3));
            assert_eq!(dv(&a), start + 1, "{journal}: rollback / read elsewhere");
            b.execute_batch("BEGIN; INSERT INTO t VALUES(12); INSERT INTO t VALUES(13); COMMIT;")
                .unwrap();
            assert_eq!(dv(&a), start + 2, "{journal}: one transaction, one step");
            a.execute_batch("INSERT INTO t VALUES(4)").unwrap();
            assert_eq!(
                dv(&a),
                start + 2,
                "{journal}: own commit after a foreign one"
            );

            // Inside a read transaction the snapshot, and with it the value, is frozen.
            a.execute_batch("BEGIN; SELECT count(*) FROM t;").unwrap();
            if journal == "wal" {
                b.execute_batch("INSERT INTO t VALUES(30)").unwrap();
            }
            assert_eq!(dv(&a), start + 2, "{journal}: inside own read transaction");
            a.execute_batch("COMMIT").unwrap();
            let expect = if journal == "wal" {
                start + 3
            } else {
                start + 2
            };
            assert_eq!(dv(&a), expect, "{journal}: after the read transaction");

            // The other connection kept its own count.
            assert!(dv(&b) > 1);
            assert!(a.try_close().is_ok());
            assert!(b.try_close().is_ok());
        }
    }

    /// `PRAGMA cell_size_check=ON` (`btreeCellSizeCheck`): a b-tree page with a cell pointer
    /// outside the cell content area, or a cell that runs past the end of the page, is refused
    /// with `SQLITE_CORRUPT` as soon as the page is loaded — also for statements that never
    /// touch the damaged cell, which is all the lazy validation without the pragma notices.
    #[test]
    fn cell_size_check_validates_pages_on_load() {
        let vfs = Arc::new(MemoryVfs::new());
        let path = Path::new("/csc.db");
        let conn = open(&vfs, path);
        conn.execute_batch(
            "PRAGMA page_size=1024;
             CREATE TABLE t(id INTEGER PRIMARY KEY, b TEXT);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<600)
               INSERT INTO t SELECT i, 'row-'||i||'-'||substr('xxxxxxxxxxxxxxxxxxxxxxxx',1,i%23) FROM n;
             CREATE TABLE healthy(id INTEGER PRIMARY KEY, b TEXT);
             INSERT INTO healthy VALUES(1,'one');",
        )
        .unwrap();
        let root = int(&conn, "SELECT rootpage FROM sqlite_schema WHERE name='t'").unwrap();
        assert!(conn.try_close().is_ok());

        // The root is an interior page; damage its second leaf: point the last cell at
        // `usable - 2`, beyond the last possible cell offset (`usable - 4`).
        let bytes = file_bytes(&vfs, path);
        let page = |n: usize| &bytes[(n - 1) * 1024..n * 1024];
        let root_page = page(root as usize);
        assert_eq!(root_page[0], 0x05, "table root must be an interior page");
        let cell1 = u16::from_be_bytes([root_page[14], root_page[15]]) as usize;
        let leaf = u32::from_be_bytes(root_page[cell1..cell1 + 4].try_into().unwrap()) as usize;
        let leaf_page = page(leaf);
        assert_eq!(leaf_page[0], 0x0d);
        let n_cell = u16::from_be_bytes([leaf_page[3], leaf_page[4]]) as usize;
        let first_ptr = u16::from_be_bytes([leaf_page[8], leaf_page[9]]) as usize;
        // Rowid of the first cell of that leaf: payload-size varint, then the rowid varint.
        let (_, n1) = sqliters_types::get_varint(&leaf_page[first_ptr..]).unwrap();
        let (first_rowid, _) =
            sqliters_types::get_varint_i64(&leaf_page[first_ptr + n1..]).unwrap();
        {
            let file = vfs
                .open(Some(path), OpenFlags::READWRITE | OpenFlags::MAIN_DB)
                .unwrap();
            let slot = (leaf - 1) * 1024 + 8 + 2 * (n_cell - 1);
            file.write_at(&1022u16.to_be_bytes(), slot as u64).unwrap();
        }

        let probe = format!("SELECT id FROM t WHERE id={first_rowid}");
        // Without the check the undamaged first cell of the page is still served.
        let conn = open(&vfs, path);
        assert_eq!(int(&conn, "PRAGMA cell_size_check"), Some(0));
        assert_eq!(int(&conn, &probe), Some(first_rowid));
        assert!(conn.try_close().is_ok());

        let conn = open(&vfs, path);
        conn.execute_batch("PRAGMA cell_size_check=ON").unwrap();
        for sql in [
            probe.clone(),
            format!("UPDATE t SET b='z' WHERE id={first_rowid}"),
            format!("DELETE FROM t WHERE id={}", first_rowid + 1),
            "SELECT sum(length(b)) FROM t".to_owned(),
        ] {
            let err = conn.query(&sql, &[]).unwrap_err();
            assert_eq!(err.code, ErrorCode::Corrupt, "{sql}");
            assert_eq!(err.message, "database disk image is malformed", "{sql}");
        }
        // Pages without a bad cell are not affected, in this and in other tables.
        assert_eq!(int(&conn, "SELECT id FROM t WHERE id=1"), Some(1));
        assert_eq!(texts(&conn, "SELECT b FROM healthy"), ["one"]);
        assert!(conn.try_close().is_ok());

        // The flag belongs to the connection: a database attached later is checked as well.
        let other = open(&vfs, Path::new("/other.db"));
        other
            .execute_batch("PRAGMA cell_size_check=ON; ATTACH '/csc.db' AS x;")
            .unwrap();
        let attached_probe = format!("SELECT id FROM x.t WHERE id={first_rowid}");
        let err = other.query(&attached_probe, &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::Corrupt);
        other.execute_batch("PRAGMA cell_size_check=OFF").unwrap();
        assert_eq!(int(&other, &attached_probe), Some(first_rowid));
        assert!(other.try_close().is_ok());
    }

    /// `ALTER TABLE ... ADD COLUMN` (`sqlite3AlterFinishAddColumn`, `alter.c`): the column
    /// definition is spliced into the stored `CREATE TABLE` text exactly as written (C's
    /// `addColOffset` rule), rows that predate the column read the default the way
    /// `sqlite3ValueFromExpr` folds it, and every refusal carries C's message and
    /// `SQLITE_ERROR` — the checks that only apply to a table with rows included. Each expected
    /// value is what the reference `sqlite3` 3.54.1 produces: `Ok((stored sql,
    /// typeof|quote of the new column in the first existing row))` or `Err(message)`.
    #[test]
    fn alter_table_add_column_matches_c() {
        type Want = std::result::Result<(&'static str, &'static str), &'static str>;
        #[rustfmt::skip]
        let cases: &[(&str, &str, &str, usize, bool, Want)] = &[
            ("CREATE TABLE t(a);", "t", "b", 2, false, Ok(("CREATE TABLE t(a, b)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a);", "t", "c TEXT DEFAULT 'x y'", 2, false, Ok(("CREATE TABLE t(a, c TEXT DEFAULT 'x y')", "text|'x y'"))),
            ("CREATE TABLE t(a);", "t", "e DEFAULT -5", 2, false, Ok(("CREATE TABLE t(a, e DEFAULT -5)", "integer|-5"))),
            ("CREATE TABLE t(a);", "t", "f DEFAULT NULL", 2, false, Ok(("CREATE TABLE t(a, f DEFAULT NULL)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "g INT NOT NULL DEFAULT 0 CHECK(g >= 0)", 2, false, Ok(("CREATE TABLE t(a, g INT NOT NULL DEFAULT 0 CHECK(g >= 0))", "integer|0"))),
            ("CREATE TABLE t(a);", "t", "h REFERENCES t(a)", 2, false, Ok(("CREATE TABLE t(a, h REFERENCES t(a))", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "i GENERATED ALWAYS AS (a+1)", 2, false, Ok(("CREATE TABLE t(a, i GENERATED ALWAYS AS (a+1))", "integer|11"))),
            ("CREATE TABLE t(a);", "t", "i2 AS (a+1) VIRTUAL", 2, false, Ok(("CREATE TABLE t(a, i2 AS (a+1) VIRTUAL)", "integer|11"))),
            ("CREATE TABLE t(a);", "t", "j DEFAULT 1.5 COLLATE nocase", 2, false, Ok(("CREATE TABLE t(a, j DEFAULT 1.5 COLLATE nocase)", "real|1.5"))),
            ("CREATE TABLE t(a);", "t", "\"k k\" DEFAULT x'00'", 2, false, Ok(("CREATE TABLE t(a, \"k k\" DEFAULT x'00')", "blob|X'00'"))),
            ("CREATE TABLE t(a);", "t", "k2 BLOB DEFAULT X'0102ff'", 2, false, Ok(("CREATE TABLE t(a, k2 BLOB DEFAULT X'0102ff')", "blob|X'0102FF'"))),
            ("CREATE TABLE t(a);", "t", "m VARCHAR(10, 2) DEFAULT 'a''b'", 2, false, Ok(("CREATE TABLE t(a, m VARCHAR(10, 2) DEFAULT 'a''b')", "text|'a''b'"))),
            ("CREATE TABLE t(a);", "t", "[n n] INT", 2, false, Ok(("CREATE TABLE t(a, [n n] INT)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "`o` REAL", 2, false, Ok(("CREATE TABLE t(a, `o` REAL)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "p DEFAULT \"str\"", 2, false, Ok(("CREATE TABLE t(a, p DEFAULT \"str\")", "text|'str'"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT a", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT a)", "text|'a'"))),
            ("CREATE TABLE t(a);", "t", "s DEFAULT TRUE", 2, false, Ok(("CREATE TABLE t(a, s DEFAULT TRUE)", "integer|1"))),
            ("CREATE TABLE t(a);", "t", "b /* c */ INT /* d */ ", 2, false, Ok(("CREATE TABLE t(a, b /* c */ INT /* d */)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "b\n  INT\n  DEFAULT 5\n", 2, false, Ok(("CREATE TABLE t(a, b\n  INT\n  DEFAULT 5)", "integer|5"))),
            ("CREATE TABLE t(a);", "t", "b INT   ;; ", 2, false, Ok(("CREATE TABLE t(a, b INT)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "b CONSTRAINT nn NOT NULL DEFAULT 5", 2, false, Ok(("CREATE TABLE t(a, b CONSTRAINT nn NOT NULL DEFAULT 5)", "integer|5"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (CAST(5 AS TEXT))", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT (CAST(5 AS TEXT)))", "text|'5'"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (-(5))", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT (-(5)))", "integer|-5"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT ((2))", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT ((2)))", "integer|2"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT -9223372036854775808", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT -9223372036854775808)", "integer|-9223372036854775808"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (-x'31')", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT (-x'31'))", "integer|-1"))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (CAST(x'31' AS INT))", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT (CAST(x'31' AS INT)))", "integer|1"))),
            ("CREATE TABLE t(a);", "t", "b REAL DEFAULT 3", 2, false, Ok(("CREATE TABLE t(a, b REAL DEFAULT 3)", "real|3.0"))),
            ("CREATE TABLE t(a);", "t", "b INT DEFAULT '12'", 2, false, Ok(("CREATE TABLE t(a, b INT DEFAULT '12')", "integer|12"))),
            ("CREATE TABLE t(a);", "t", "b REFERENCES t(a) DEFAULT 5", 2, false, Ok(("CREATE TABLE t(a, b REFERENCES t(a) DEFAULT 5)", "integer|5"))),
            ("CREATE TABLE t(a);", "t", "b REFERENCES t(a) DEFAULT 5", 0, true, Ok(("CREATE TABLE t(a, b REFERENCES t(a) DEFAULT 5)", ""))),
            ("CREATE TABLE t(a);", "t", "b REFERENCES t(a) DEFAULT 5", 2, true, Err("Cannot add a REFERENCES column with non-NULL default value")),
            ("CREATE TABLE t(a);", "t", "b REFERENCES t(a) DEFAULT NULL", 2, true, Ok(("CREATE TABLE t(a, b REFERENCES t(a) DEFAULT NULL)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "b INT REFERENCES t(a) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED", 2, true, Ok(("CREATE TABLE t(a, b INT REFERENCES t(a) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED)", "null|NULL"))),
            ("CREATE TABLE t(a);", "t", "b CHECK(b>0) DEFAULT 1", 2, false, Ok(("CREATE TABLE t(a, b CHECK(b>0) DEFAULT 1)", "integer|1"))),
            ("CREATE TABLE t(a);", "t", "b CHECK(b>0) DEFAULT -1", 0, false, Ok(("CREATE TABLE t(a, b CHECK(b>0) DEFAULT -1)", ""))),
            ("CREATE TABLE t(a);", "t", "b CHECK(b>0) DEFAULT -1", 2, false, Err("CHECK constraint failed")),
            ("CREATE TABLE t(a);", "t", "b INT NOT NULL AS (NULL)", 0, false, Ok(("CREATE TABLE t(a, b INT NOT NULL AS (NULL))", ""))),
            ("CREATE TABLE t(a);", "t", "b INT NOT NULL AS (NULL)", 2, false, Err("NOT NULL constraint failed")),
            ("CREATE TABLE t(a);", "t", "b INT NOT NULL AS (a) VIRTUAL", 2, false, Ok(("CREATE TABLE t(a, b INT NOT NULL AS (a) VIRTUAL)", "integer|10"))),
            ("CREATE TABLE t(a);", "t", "d DEFAULT (1+2)", 0, false, Ok(("CREATE TABLE t(a, d DEFAULT (1+2))", ""))),
            ("CREATE TABLE t(a);", "t", "d DEFAULT (1+2)", 2, false, Err("Cannot add a column with non-constant default")),
            ("CREATE TABLE t(a);", "t", "l DEFAULT CURRENT_TIMESTAMP", 0, false, Ok(("CREATE TABLE t(a, l DEFAULT CURRENT_TIMESTAMP)", ""))),
            ("CREATE TABLE t(a);", "t", "l DEFAULT CURRENT_TIMESTAMP", 2, false, Err("Cannot add a column with non-constant default")),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (abs(-3))", 0, false, Ok(("CREATE TABLE t(a, b DEFAULT (abs(-3)))", ""))),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (abs(-3))", 2, false, Err("Cannot add a column with non-constant default")),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (a)", 2, false, Err("default value of column [b] is not constant")),
            ("CREATE TABLE t(a);", "t", "b DEFAULT (?)", 2, false, Err("default value of column [b] is not constant")),
            ("CREATE TABLE t(a);", "t", "i3 INT AS (a) STORED", 0, false, Ok(("CREATE TABLE t(a, i3 INT AS (a) STORED)", ""))),
            ("CREATE TABLE t(a);", "t", "i3 INT AS (a) STORED", 2, false, Err("cannot add a STORED column")),
            ("CREATE TABLE t(a);", "t", "v NOT NULL", 0, false, Ok(("CREATE TABLE t(a, v NOT NULL)", ""))),
            ("CREATE TABLE t(a);", "t", "v NOT NULL", 2, false, Err("Cannot add a NOT NULL column with default value NULL")),
            ("CREATE TABLE t(a);", "t", "w NOT NULL DEFAULT NULL", 0, false, Ok(("CREATE TABLE t(a, w NOT NULL DEFAULT NULL)", ""))),
            ("CREATE TABLE t(a);", "t", "w NOT NULL DEFAULT NULL", 2, false, Err("Cannot add a NOT NULL column with default value NULL")),
            ("CREATE TABLE t(a);", "t", "x PRIMARY KEY", 2, false, Err("Cannot add a PRIMARY KEY column")),
            ("CREATE TABLE t(a);", "t", "x2 INTEGER PRIMARY KEY AUTOINCREMENT", 2, false, Err("Cannot add a PRIMARY KEY column")),
            ("CREATE TABLE t(a);", "t", "y UNIQUE", 2, false, Err("Cannot add a UNIQUE column")),
            ("CREATE TABLE t(a);", "t", "rowid INT", 2, false, Err("cannot add rowid alias: rowid")),
            ("CREATE TABLE t(a);", "t", "A INT", 2, false, Err("duplicate column name: A")),
            ("CREATE TABLE t(a);", "t", "b COLLATE nosuchcoll", 2, false, Err("no such collation sequence: nosuchcoll")),
            ("CREATE TABLE t(a);", "t", "b AS (a) DEFAULT 5", 2, false, Err("cannot use DEFAULT on a generated column")),
            ("CREATE TABLE t(a);", "t", "b AS (a) PRIMARY KEY", 2, false, Err("generated columns cannot be part of the PRIMARY KEY")),
            ("CREATE TABLE t(a);", "t", "b AS (c)", 2, false, Err("error in table t after add column: no such column: c")),
            ("CREATE TABLE t(a INT, b0 TEXT, PRIMARY KEY(a));", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a INT, b0 TEXT, b DEFAULT 7, PRIMARY KEY(a))", "integer|7"))),
            ("CREATE TABLE t(a , CONSTRAINT c1 CHECK(a>0) );", "t", "c TEXT DEFAULT 'x y'", 2, false, Ok(("CREATE TABLE t(a , c TEXT DEFAULT 'x y', CONSTRAINT c1 CHECK(a>0) )", "text|'x y'"))),
            ("CREATE TABLE t(a INT,b0,UNIQUE(b0),FOREIGN KEY(a) REFERENCES t(b0));", "t", "b", 2, false, Ok(("CREATE TABLE t(a INT,b0, b,UNIQUE(b0),FOREIGN KEY(a) REFERENCES t(b0))", "null|NULL"))),
            ("CREATE TABLE \"t t\"(a)  ;", "\"t t\"", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE \"t t\"(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a /* c */ )   ;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a /* c */ , b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(\n  a INT, -- first\n  b0 TEXT -- second\n);", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(\n  a INT, -- first\n  b0 TEXT -- second\n, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a PRIMARY KEY) WITHOUT ROWID;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a PRIMARY KEY, b DEFAULT 7) WITHOUT ROWID", "integer|7"))),
            ("CREATE TABLE t(a PRIMARY KEY) WITHOUT ROWID;", "t", "rowid INT", 2, false, Ok(("CREATE TABLE t(a PRIMARY KEY, rowid INT) WITHOUT ROWID", "null|NULL"))),
            ("CREATE TABLE t(a INT) STRICT;", "t", "b ANY DEFAULT 'x'", 2, false, Ok(("CREATE TABLE t(a INT, b ANY DEFAULT 'x') STRICT", "text|'x'"))),
            ("CREATE TABLE t(a INT) STRICT;", "t", "b INT DEFAULT 'x'", 0, false, Ok(("CREATE TABLE t(a INT, b INT DEFAULT 'x') STRICT", ""))),
            ("CREATE TABLE t(a INT) STRICT;", "t", "b INT DEFAULT 'x'", 2, false, Err("type mismatch on DEFAULT")),
            ("CREATE TABLE t(a INT) STRICT;", "t", "b BADTYPE", 2, false, Err("error in table t after add column: unknown datatype for t.b: \"BADTYPE\"")),
            ("CREATE TABLE t(a INT) STRICT;", "t", "b", 2, false, Err("error in table t after add column: missing datatype for t.b")),
            ("create table t(a);", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE IF NOT EXISTS main.t(a);", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE old(a); ALTER TABLE old RENAME TO t;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE \"t\"(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a); ALTER TABLE t ADD COLUMN z0 INT DEFAULT 3;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, z0 INT DEFAULT 3, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a, z0, z1); ALTER TABLE t DROP COLUMN z1;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, z0, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE \"tü\"(a TEXT);", "\"tü\"", "\"ö\" DEFAULT 'ß'", 2, false, Ok(("CREATE TABLE \"tü\"(a TEXT, \"ö\" DEFAULT 'ß')", "text|'ß'"))),
            ("CREATE TABLE\tt\t(\ta\t)\t;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t\t(\ta\t, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a,PRIMARY KEY(a));", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT 7,PRIMARY KEY(a))", "integer|7"))),
            ("CREATE TABLE src(a); CREATE TABLE t AS SELECT a FROM src;", "t", "b DEFAULT 7", 2, false, Ok(("CREATE TABLE t(a, b DEFAULT 7)", "integer|7"))),
            ("CREATE TABLE t(a); CREATE VIEW v AS SELECT a FROM t;", "v", "b", 0, false, Err("Cannot add a column to a view")),
            ("CREATE TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT);", "sqlite_sequence", "b", 0, false, Err("table sqlite_sequence may not be altered")),
            ("CREATE TABLE t(a);", "sqlite_schema", "b", 0, false, Err("table sqlite_master may not be altered")),
            ("CREATE TABLE t(a);", "nosuch", "b", 0, false, Err("no such table: nosuch")),
        ];
        let stored = |conn: &Connection| -> Vec<String> {
            texts(
                conn,
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name IN ('t','t t','tü')",
            )
        };
        for &(setup, table, col, rows, fk, want) in cases {
            let what = format!("{setup} / ADD COLUMN {col} / rows={rows} fk={fk}");
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(&format!("PRAGMA foreign_keys={}", i32::from(fk)))
                .unwrap();
            conn.execute_batch(setup).unwrap();
            for k in 0..rows {
                conn.execute_batch(&format!("INSERT INTO {table}(a) VALUES({})", 10 * (k + 1)))
                    .unwrap();
            }
            let before = stored(&conn);
            let res = conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col};"));
            match want {
                Ok((sql, val)) => {
                    res.unwrap_or_else(|e| panic!("{what}: {}", e.message));
                    assert_eq!(stored(&conn), [sql], "{what}");
                    if rows > 0 {
                        let name = sqliters_syntax::parse_one(&format!("ALTER TABLE x ADD {col}"))
                            .ok()
                            .and_then(|(stmt, _)| match stmt {
                                Some(Stmt::AlterTable {
                                    action:
                                        sqliters_syntax::AlterTableAction::AddColumn { column, .. },
                                    ..
                                }) => Some(column.name),
                                _ => None,
                            })
                            .unwrap();
                        let q = format!("\"{}\"", name.replace('"', "\"\""));
                        assert_eq!(
                            texts(
                                &conn,
                                &format!(
                                    "SELECT typeof({q})||'|'||quote({q}) FROM {table} LIMIT 1"
                                )
                            ),
                            [val],
                            "{what}"
                        );
                    }
                    assert_eq!(texts(&conn, "PRAGMA integrity_check"), ["ok"], "{what}");
                }
                Err(msg) => {
                    let err = res.expect_err(&what);
                    assert_eq!(err.message, msg, "{what}");
                    // C returns SQLITE_ERROR for every one of these. The two failures of the
                    // post-ALTER row check keep SQLITE_CONSTRAINT, which the integration test
                    // for finding Y8 (`tests/bulgular_regressions.rs`) requires.
                    let want_code = match msg {
                        "CHECK constraint failed" | "NOT NULL constraint failed" => {
                            ErrorCode::Constraint
                        }
                        _ => ErrorCode::Error,
                    };
                    assert_eq!(err.code, want_code, "{what}");
                    assert_eq!(stored(&conn), before, "{what}: schema must be unchanged");
                }
            }
        }

        // A rejected ALTER inside a transaction leaves the table usable and unchanged, and the
        // text survives a round trip through another connection's schema load.
        let vfs = Arc::new(MemoryVfs::new());
        let path = Path::new("/alter.db");
        let conn = open(&vfs, path);
        conn.execute_batch(
            "CREATE TABLE t(a, PRIMARY KEY(a)); INSERT INTO t VALUES(1),(2);
             BEGIN;
             ALTER TABLE t ADD COLUMN k DEFAULT x'00ff10';",
        )
        .unwrap();
        assert!(
            conn.execute_batch("ALTER TABLE t ADD COLUMN c CHECK(c>0) DEFAULT -1")
                .is_err()
        );
        conn.execute_batch("ALTER TABLE t ADD COLUMN c CHECK(c>0) DEFAULT 1; COMMIT;")
            .unwrap();
        let want_sql =
            "CREATE TABLE t(a, k DEFAULT x'00ff10', c CHECK(c>0) DEFAULT 1, PRIMARY KEY(a))";
        assert_eq!(stored(&conn), [want_sql]);
        assert!(conn.try_close().is_ok());
        let conn = open(&vfs, path);
        assert_eq!(stored(&conn), [want_sql]);
        conn.execute_batch("INSERT INTO t(a) VALUES(3)").unwrap();
        assert_eq!(
            texts(&conn, "SELECT a||'|'||quote(k)||'|'||c FROM t ORDER BY a"),
            ["1|X'00FF10'|1", "2|X'00FF10'|1", "3|X'00FF10'|1"]
        );
    }

    /// Header of a database that has no table yet (inventory class H2): after a first
    /// statement such as `PRAGMA journal_mode=WAL`, `auto_vacuum`, `user_version`, an empty
    /// write transaction or `VACUUM`, the schema format (offset 44) and the text encoding
    /// (offset 56) are still 0 as in C, and `PRAGMA encoding` is honoured until the connection
    /// has seen a schema object (`DBFLAG_EncodingFixed`).
    #[test]
    fn table_less_database_header_and_pragma_encoding_rule() {
        fn fields(vfs: &MemoryVfs, path: &Path) -> (u32, u32) {
            let b = file_bytes(vfs, path);
            let u = |off: usize| u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
            (u(44), u(56))
        }
        let path = Path::new("/h2.db");
        for (first, encoding, want_code) in [
            ("PRAGMA journal_mode=WAL", "UTF-16be", 3u32),
            ("PRAGMA auto_vacuum=FULL", "UTF-16le", 2),
            ("PRAGMA user_version=5", "UTF-16be", 3),
            ("PRAGMA application_id=7", "UTF-16le", 2),
            ("BEGIN IMMEDIATE; COMMIT", "UTF-16be", 3),
            ("VACUUM", "UTF-16le", 2),
            ("PRAGMA page_size=8192; VACUUM", "UTF-16be", 3),
            ("PRAGMA schema_version=9", "UTF-8", 1),
        ] {
            let vfs = Arc::new(MemoryVfs::new());
            let conn = open(&vfs, path);
            conn.execute_batch(first).unwrap();
            if first.contains("WAL") {
                conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .unwrap();
            }
            assert_eq!(fields(&vfs, path), (0, 0), "{first}");
            assert_eq!(texts(&conn, "PRAGMA encoding"), ["UTF-8"], "{first}");
            assert!(conn.try_close().is_ok());

            // A second connection finds page 1 on disk and may still choose the encoding.
            let conn = open(&vfs, path);
            conn.execute_batch(&format!("PRAGMA encoding='{encoding}'"))
                .unwrap();
            assert_eq!(texts(&conn, "PRAGMA encoding"), [encoding], "{first}");
            conn.execute_batch("PRAGMA user_version=11").unwrap();
            assert_eq!(texts(&conn, "PRAGMA encoding"), [encoding], "{first}");
            conn.execute_batch(
                "CREATE TABLE t(a TEXT); INSERT INTO t VALUES('\u{e9}xyz');                  PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
            assert_eq!(fields(&vfs, path), (4, want_code), "{first}");
            // Once a table exists the encoding is final.
            conn.execute_batch("PRAGMA encoding='UTF-8'; PRAGMA encoding='latin1';")
                .unwrap();
            assert_eq!(texts(&conn, "PRAGMA encoding"), [encoding], "{first}");
            assert!(conn.try_close().is_ok());
            let stored: &[u8] = match want_code {
                1 => b"\xc3\xa9xyz",
                2 => b"\xe9\0x\0y\0z\0",
                _ => b"\0\xe9\0x\0y\0z",
            };
            assert_eq!(count(&file_bytes(&vfs, path), stored), 1, "{first}");
            let conn = open(&vfs, path);
            assert_eq!(texts(&conn, "PRAGMA encoding"), [encoding], "{first}");
            assert_eq!(texts(&conn, "SELECT a FROM t"), ["\u{e9}xyz"], "{first}");
            assert_eq!(texts(&conn, "PRAGMA integrity_check"), ["ok"]);
            assert!(conn.try_close().is_ok());
        }

        // `DBFLAG_EncodingFixed`: a schema object anywhere in the connection (a TEMP table is
        // enough) ends the choice; an unknown name is only an error while it could apply.
        let vfs = Arc::new(MemoryVfs::new());
        let conn = open(&vfs, path);
        conn.execute_batch("PRAGMA user_version=5").unwrap();
        let err = conn.execute_batch("PRAGMA encoding='latin1'").unwrap_err();
        assert_eq!(err.message, "unsupported encoding: latin1");
        assert_eq!(err.code, ErrorCode::Error);
        conn.execute_batch("CREATE TEMP TABLE tt(a); PRAGMA encoding='UTF-16be';")
            .unwrap();
        assert_eq!(texts(&conn, "PRAGMA encoding"), ["UTF-8"]);
        conn.execute_batch("PRAGMA encoding='latin1'; CREATE TABLE t(a TEXT);")
            .unwrap();
        assert_eq!(fields(&vfs, path), (4, 1));
        assert!(conn.try_close().is_ok());

        // VACUUM copies the header fields as stored: a file whose only table was dropped keeps
        // its recorded encoding and loses the schema format, as in C.
        let vfs = Arc::new(MemoryVfs::new());
        let conn = open(&vfs, path);
        conn.execute_batch("CREATE TABLE z(a); DROP TABLE z;")
            .unwrap();
        assert_eq!(fields(&vfs, path), (4, 1));
        conn.execute_batch("VACUUM").unwrap();
        assert_eq!(fields(&vfs, path), (0, 1));
        assert!(conn.try_close().is_ok());
        let conn = open(&vfs, path);
        conn.execute_batch("PRAGMA encoding='UTF-16be'; CREATE TABLE t(a TEXT);")
            .unwrap();
        assert_eq!(texts(&conn, "PRAGMA encoding"), ["UTF-8"]);
        assert_eq!(fields(&vfs, path), (4, 1));
        assert!(conn.try_close().is_ok());

        // An attached database that has no table yet follows the connection encoding.
        let vfs = Arc::new(MemoryVfs::new());
        let conn = open(&vfs, path);
        conn.execute_batch(
            "PRAGMA user_version=5; ATTACH '/h2aux.db' AS aux; PRAGMA aux.user_version=6;
             PRAGMA encoding='UTF-16be';
             CREATE TABLE aux.t(a TEXT); INSERT INTO aux.t VALUES('q');",
        )
        .unwrap();
        assert_eq!(texts(&conn, "PRAGMA encoding"), ["UTF-16be"]);
        assert_eq!(fields(&vfs, Path::new("/h2aux.db")), (4, 3));
        assert_eq!(fields(&vfs, path), (0, 0));
        assert!(conn.try_close().is_ok());
    }
}
