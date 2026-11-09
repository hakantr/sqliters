//! `sqliters-core`: High-level SQLite 3.54.1 database engine, schema catalog, compiler,
//! and public `Connection` / `Statement` / `Row` API (`main.c`, `legacy.c`, `vdbeapi.c`).
#![forbid(unsafe_code)]

pub mod compiler;
pub mod db_set;
pub mod engine;
pub mod execution_context;
pub mod schema;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sqliters_btree::{Btree, BtreeCursorState, CursorId};
pub use sqliters_pager::{
    AutoVacuumMode, DatabaseHeader, JournalMode, LockingMode, WalCheckpointMode,
    WalCheckpointResult,
};
use sqliters_pager::{PagerOpenOptions, TransactionState};
use sqliters_syntax::{
    BindParam, Expr, FunctionArgs, InsertSource, ResultColumn, SelectCore, SelectStmt, Stmt,
    TableOrSubquery, parse_one,
};
pub use sqliters_syntax::{ColumnRef, QualifiedName};
pub use sqliters_types::{
    AggregateStepId, DatabaseId, ExecutionServices, FunctionFlags, MAX_DB_SLOTS, RowMutationEvent,
    RowMutationOp, RuntimeValue, SQLITE_MAX_ATTACHED,
};
use sqliters_types::{
    ConflictResolutionEffect, DatabaseKind, ErrorCode, LockGeometry, Pgno, Result,
    SQLITE_DEFAULT_PAGE_SIZE, SqliteError, TextEncoding, Value, extended,
};
pub use sqliters_vdbe::BtreeAccess;
use sqliters_vdbe::{Opcode, TestFunctionsScope, VdbeProgram, VdbeVm, VmRuntimeContext, VmStep};
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
    DatabaseSet, DbSlot, SQLITE_MAX_PAGE_COUNT, SchemaInstanceId, TempStoreMode, TempTriggerBinding,
};
pub use engine::{QueryOutput, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT};
pub use execution_context::{
    CollationCallback, CollationRegistry, CommitCoordinatorHooks, CommitHookCallback,
    ExecutionContext, FunctionRegistry, ProgressCallback, ProgressTracker, ScalarFunctionCallback,
    ScalarFunctionDef,
};
pub use schema::{ColumnSchema, ForeignKeySchema, IndexSchema, Schema, TableSchema};

/// Maximum automatic `PrepareMode::V2` schema-change reprepare retries (`SQLITE_MAX_SCHEMA_RETRY`).
const SQLITE_MAX_SCHEMA_RETRY: usize = 50;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static ACTIVE_CONN_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

struct ActiveConnStackGuard {
    conn_id: u64,
}

impl ActiveConnStackGuard {
    fn push(conn_id: u64) -> Self {
        ACTIVE_CONN_STACK.with(|stack| {
            if let Ok(mut s) = stack.try_borrow_mut() {
                s.push(conn_id);
            }
        });
        Self { conn_id }
    }
}

impl Drop for ActiveConnStackGuard {
    fn drop(&mut self) {
        let cid = self.conn_id;
        let _ = ACTIVE_CONN_STACK.try_with(|stack| {
            if let Ok(mut s) = stack.try_borrow_mut()
                && let Some(pos) = s.iter().rposition(|&id| id == cid)
            {
                s.remove(pos);
            }
        });
    }
}

fn is_conn_active_on_current_thread(conn_id: u64) -> bool {
    ACTIVE_CONN_STACK
        .try_with(|stack| stack.try_borrow().is_ok_and(|s| s.contains(&conn_id)))
        .unwrap_or(false)
}

/// Compile-time `Send` / `Sync` guarantees achieved purely with safe `std` synchronization.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<Connection>();
    assert_sync::<Connection>();
    assert_send::<Statement>();
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
struct HandleId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandleKind {
    Statement,
    TestBlob { writable: bool },
}

#[derive(Debug, Clone, Default)]
struct ActiveLease {
    is_reader: bool,
    is_writer: bool,
    read_instances: Vec<SchemaInstanceId>,
    write_instances: Vec<SchemaInstanceId>,
}

fn push_unique_instance(dst: &mut Vec<SchemaInstanceId>, id: SchemaInstanceId) {
    if !dst.contains(&id) {
        dst.push(id);
    }
}

fn merge_instances(dst: &mut Vec<SchemaInstanceId>, src: &[SchemaInstanceId]) {
    for &id in src {
        push_unique_instance(dst, id);
    }
}

fn mask_to_instances(db_set: &DatabaseSet, mask: u16) -> Vec<SchemaInstanceId> {
    let mut out = Vec::new();
    if mask == 0 {
        return out;
    }
    for (idx, slot) in db_set.slots().iter().enumerate() {
        let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
        if (mask & bit) != 0 {
            push_unique_instance(&mut out, slot.schema_instance_id);
        }
    }
    out
}

#[derive(Debug, Default)]
struct ActivityTracker {
    leases: BTreeMap<HandleId, ActiveLease>,
}

impl ActivityTracker {
    fn start(&mut self, id: HandleId, is_reader: bool, is_writer: bool) {
        self.leases.insert(
            id,
            ActiveLease {
                is_reader: is_reader || is_writer,
                is_writer,
                read_instances: Vec::new(),
                write_instances: Vec::new(),
            },
        );
    }

    fn start_with_instances(
        &mut self,
        id: HandleId,
        is_reader: bool,
        is_writer: bool,
        read_instances: Vec<SchemaInstanceId>,
        write_instances: Vec<SchemaInstanceId>,
    ) {
        let has_w = is_writer || !write_instances.is_empty();
        let has_r = is_reader || has_w || !read_instances.is_empty();
        self.leases.insert(
            id,
            ActiveLease {
                is_reader: has_r,
                is_writer: has_w,
                read_instances,
                write_instances,
            },
        );
    }

    fn update_instances(
        &mut self,
        id: HandleId,
        read_instances: &[SchemaInstanceId],
        write_instances: &[SchemaInstanceId],
    ) {
        if let Some(lease) = self.leases.get_mut(&id) {
            merge_instances(&mut lease.read_instances, read_instances);
            merge_instances(&mut lease.write_instances, write_instances);
            if !write_instances.is_empty() {
                lease.is_writer = true;
                lease.is_reader = true;
            } else if !read_instances.is_empty() {
                lease.is_reader = true;
            }
        }
    }

    fn remove(&mut self, id: HandleId) -> Option<ActiveLease> {
        self.leases.remove(&id)
    }

    fn summary_excluding(&self, exclude: HandleId, db_set: &DatabaseSet) -> ActivitySummary {
        let mut other_active = 0usize;
        let mut other_readers = 0usize;
        let mut other_writers = 0usize;
        let mut other_read_db_mask = 0u16;
        let mut other_write_db_mask = 0u16;
        for (&hid, lease) in &self.leases {
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

    fn has_no_active_tx(&self) -> bool {
        !self.leases.values().any(|l| l.is_reader || l.is_writer)
    }

    fn has_active_tx_on_instance(&self, inst: SchemaInstanceId) -> bool {
        self.leases
            .values()
            .any(|l| l.read_instances.contains(&inst) || l.write_instances.contains(&inst))
    }
}

#[derive(Debug, Clone)]
enum ConnectionState {
    Open,
    Closing,
    Poisoned(SqliteError),
    Closed,
}

#[derive(Clone)]
struct WriteAbortSnapshot {
    stmt_sp_slot: Option<usize>,
    touched_write_instances: Vec<SchemaInstanceId>,
    started_read_instances: Vec<SchemaInstanceId>,
    fk_before: Vec<(SchemaInstanceId, String, schema::TableRowKey, usize)>,
    rt_before: VmRuntimeContext,
    schemas_before: Vec<(SchemaInstanceId, Schema)>,
    schema_gen_before: u64,
    v1_expire_gen_before: u64,
    schema_changed_before: bool,
}

enum RunningExec {
    Vdbe {
        vm: VdbeVm,
        read_instances: Vec<SchemaInstanceId>,
    },
    MaterializedRead {
        rows: std::vec::IntoIter<Vec<Value>>,
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: Vec<SchemaInstanceId>,
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

enum StatementLifecycle {
    Ready,
    Running(RunningExec),
    Halted { last_error: Option<SqliteError> },
}

enum PendingDrop {
    Statement {
        handle_id: HandleId,
        running: Option<Box<RunningExec>>,
    },
    TestBlob {
        handle_id: HandleId,
        lease_cursor: Option<(SchemaInstanceId, CursorId)>,
        writable: bool,
    },
}

enum ActivityFinish {
    NoLock,
    ReadDone {
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: Vec<SchemaInstanceId>,
    },
    ReadAborted {
        lease_cursors: Vec<(SchemaInstanceId, CursorId)>,
        read_instances: Vec<SchemaInstanceId>,
    },
    WriteCompleted {
        abort_snapshot: WriteAbortSnapshot,
    },
    PendingCommitAborted,
}

struct ConnectionInner {
    db_set: DatabaseSet,
    connection_encoding: TextEncoding,
    state: ConnectionState,
    next_handle_id: u64,
    open_handles: BTreeMap<HandleId, HandleKind>,
    activity: ActivityTracker,
    savepoints: Vec<SqlSavepoint>,
    runtime: VmRuntimeContext,
    active_interrupt_flag: Arc<AtomicBool>,
    functions: FunctionRegistry,
    collations: CollationRegistry,
    progress: ProgressTracker,
    commit_hooks: CommitCoordinatorHooks,
    strict_subtype: bool,
    explicit_tx: bool,
    implicit_savepoint_tx: bool,
    in_user_btree_tx: bool,
    schema_changed_in_tx: bool,
    foreign_keys: bool,
    count_changes: bool,
    wal_autocheckpoint_threshold: i32,
    default_locking_mode: LockingMode,
    writable_schema: bool,
    working_dir: Option<PathBuf>,
    busy_policy: BusyPolicy,
    rollback_count: u64,
    deferred_fk_suspects: Vec<(SchemaInstanceId, String, schema::TableRowKey, usize)>,
    schema_generation: u64,
    v1_expire_generation: u64,
    simulated_cleanup_io_error: Option<SqliteError>,
    panic_on_next_pending_drop: bool,
}

struct ConnectionShared {
    conn_id: u64,
    db_path: Option<PathBuf>,
    database_kind: DatabaseKind,
    is_readonly: bool,
    open_options: ConnectionOpenOptions,
    interrupt_flag: Arc<AtomicBool>,
    gate_busy: AtomicBool,
    mirror_last_insert_rowid: AtomicI64,
    mirror_changes: AtomicU64,
    mirror_total_changes: AtomicU64,
    mirror_autocommit: AtomicBool,
    mirror_rollback_count: AtomicU64,
    mirror_attached_limit: AtomicUsize,
    pending_drops: Mutex<Vec<PendingDrop>>,
    inner: Mutex<ConnectionInner>,
}

impl ConnectionShared {
    fn publish_mirrors(
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

    fn lock_pending_drops(&self) -> MutexGuard<'_, Vec<PendingDrop>> {
        self.pending_drops
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_inner(&self) -> MutexGuard<'_, ConnectionInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self) -> Result<ConnSession<'_>> {
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
        let initial_drops = {
            let mut pq = self.lock_pending_drops();
            std::mem::take(&mut *pq)
        };
        if !initial_drops.is_empty() {
            session
                .inner_mut()
                .process_pending_drops(initial_drops, self);
        }
        Ok(session)
    }

    fn enqueue_or_run_drop(&self, item: PendingDrop) {
        let claimed = {
            let mut pq = self.lock_pending_drops();
            pq.push(item);
            self.gate_busy
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
        };
        if claimed {
            let _stack_guard = ActiveConnStackGuard::push(self.conn_id);
            self.drain_and_release_gate();
        }
    }

    fn drain_and_release_gate(&self) {
        let mut gate_unwind = GateReleaseOnUnwind {
            shared: self,
            armed: true,
        };
        loop {
            let batch = {
                let mut pq = self.lock_pending_drops();
                std::mem::take(&mut *pq)
            };
            if !batch.is_empty() {
                let mut inner = self.lock_inner();
                inner.process_pending_drops(batch, self);
                drop(inner);
                continue;
            }
            let pq = self.lock_pending_drops();
            if pq.is_empty() {
                self.gate_busy.store(false, Ordering::Release);
                gate_unwind.armed = false;
                drop(pq);
                break;
            }
            drop(pq);
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
        let inner = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        let pending = self
            .pending_drops
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        let batch = std::mem::take(pending);
        for item in batch {
            match item {
                PendingDrop::Statement { handle_id, running } => {
                    if let Some(boxed_run) = running {
                        match *boxed_run {
                            RunningExec::Vdbe { mut vm, .. } => {
                                vm.close_cursors(&mut inner.db_set);
                            }
                            RunningExec::MaterializedRead { lease_cursors, .. } => {
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

struct ConnSession<'a> {
    shared: &'a ConnectionShared,
    guard: Option<MutexGuard<'a, ConnectionInner>>,
    _stack_guard: ActiveConnStackGuard,
}

impl ConnSession<'_> {
    #[inline]
    fn inner_mut(&mut self) -> &mut ConnectionInner {
        self.guard
            .as_mut()
            .expect("ConnSession inner guard is live")
    }

    #[inline]
    fn inner(&self) -> &ConnectionInner {
        self.guard
            .as_ref()
            .expect("ConnSession inner guard is live")
    }
}

impl Drop for ConnSession<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.shared.drain_and_release_gate();
    }
}

impl ConnectionInner {
    fn ensure_usable(&self) -> Result<()> {
        match &self.state {
            ConnectionState::Open => Ok(()),
            ConnectionState::Closing | ConnectionState::Closed => {
                Err(SqliteError::misuse("cannot use a closed connection"))
            }
            ConnectionState::Poisoned(err) => Err(err.clone()),
        }
    }

    #[inline]
    fn effective_encoding(&self) -> TextEncoding {
        if self.db_set.main_btree().pager().page_count() > 0 {
            self.db_set.main_btree().encoding()
        } else {
            self.connection_encoding
        }
    }

    #[inline]
    fn check_any_interrupted(&self) -> Result<()> {
        if self.active_interrupt_flag.load(Ordering::SeqCst) {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        self.db_set.main_btree().check_interrupted()
    }

    fn publish(&self, shared: &ConnectionShared) {
        shared.publish_mirrors(
            &self.runtime,
            self.explicit_tx,
            self.implicit_savepoint_tx,
            self.rollback_count,
        );
    }

    fn is_autocommit(&self) -> bool {
        !self.explicit_tx && !self.implicit_savepoint_tx
    }

    fn register_handle(&mut self, kind: HandleKind) -> HandleId {
        let id = HandleId(self.next_handle_id);
        self.next_handle_id += 1;
        self.open_handles.insert(id, kind);
        id
    }

    fn take_simulated_cleanup_error(&mut self) -> Option<SqliteError> {
        self.simulated_cleanup_io_error.take()
    }

    fn poison_with(&mut self, err: SqliteError) -> SqliteError {
        if !matches!(self.state, ConnectionState::Poisoned(_)) {
            self.state = ConnectionState::Poisoned(err.clone());
        }
        err
    }

    fn snapshot_open_schemas(&self) -> Vec<(SchemaInstanceId, Schema)> {
        self.db_set
            .slots()
            .iter()
            .filter(|s| s.btree.is_some())
            .map(|s| (s.schema_instance_id, s.schema.clone()))
            .collect()
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
                && let Ok(fresh) = Schema::load(bt)
            {
                slot.schema = fresh;
            }
        }
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
                let bt = slot.btree.as_mut().unwrap();
                let prev_cookie = slot.schema.schema_cookie;
                let prev_had_pages = slot.schema.had_pages;
                let prev_loaded = slot.schema.loaded;
                let load_res = slot.schema.ensure_fresh_in_tx(bt);
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
                let bt = slot.btree.as_mut().unwrap();
                let prev_cookie = slot.schema.schema_cookie;
                let prev_had_pages = slot.schema.had_pages;
                let prev_loaded = slot.schema.loaded;
                slot.schema.ensure_fresh_in_tx(bt)?;
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
                slot.schema.ensure_fresh_in_tx(bt)?;
                if idx == 0 && has_pages {
                    self.connection_encoding = bt.encoding();
                }
                any_changed = true;
            }
        }
        if any_changed {
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
                    if summary.other_readers == 0 && summary.other_writers == 0 {
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
                        count_changes: &mut self.count_changes,
                        wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut self.default_locking_mode,
                        writable_schema: &mut self.writable_schema,
                        working_dir: &self.working_dir,
                        busy_policy: &mut self.busy_policy,
                        rollback_count: &mut self.rollback_count,
                        deferred_fk_suspects: &mut self.deferred_fk_suspects,
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
                    self.runtime.last_changes = abort_snapshot.rt_before.last_changes;
                    self.runtime.total_changes = abort_snapshot.rt_before.total_changes;
                    self.runtime.last_insert_rowid = abort_snapshot.rt_before.last_insert_rowid;
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

    fn abort_write_statement(
        &mut self,
        hid: HandleId,
        err: &SqliteError,
        snap: WriteAbortSnapshot,
        shared: &ConnectionShared,
    ) -> SqliteError {
        self.activity.remove(hid);
        let summary = self.activity.summary_excluding(hid, &self.db_set);

        if let Some(sim_err) = self.take_simulated_cleanup_error() {
            return self.poison_with(sim_err);
        }

        // `SQLITE_FULL` in SQLite (`vdbeaux.c` / `btree.c`) rolls back the full transaction
        // unless handled inside a savepoint? Wait: in `run_write_autocommit` (`engine.rs:2279`),
        // `ErrorCode::Full` rolls back the full transaction! Check if `err.conflict_effect == Rollback` or `err.code == ErrorCode::Full`!
        let effective_effect = if err.code == ErrorCode::Full {
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
                        count_changes: &mut self.count_changes,
                        wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut self.default_locking_mode,
                        writable_schema: &mut self.writable_schema,
                        working_dir: &self.working_dir,
                        busy_policy: &mut self.busy_policy,
                        rollback_count: &mut self.rollback_count,
                        deferred_fk_suspects: &mut self.deferred_fk_suspects,
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
                self.runtime.last_changes = snap.rt_before.last_changes;
                self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
                if was_autocommit {
                    self.runtime.total_changes = snap.rt_before.total_changes;
                }
                if schema_changed {
                    self.schema_changed_in_tx = false;
                    self.restore_schemas_snapshot(&snap.schemas_before);
                    for slot in self.db_set.slots_mut() {
                        if let Some(ref mut bt) = slot.btree
                            && let Ok(s) = Schema::load(bt)
                        {
                            slot.schema = s;
                        }
                    }
                    self.schema_generation += 1;
                    self.v1_expire_generation += 1;
                }
                self.publish(shared);
                err.clone()
            }

            ConflictResolutionEffect::Fail => {
                let mut committed_autocommit = false;
                if !self.is_autocommit() || summary.other_writers > 0 {
                    if let Some(sp_slot) = snap.stmt_sp_slot {
                        for &inst in &snap.touched_write_instances {
                            if let Some(slot) = self.db_set.slot_by_instance_mut(inst)
                                && let Some(ref mut bt) = slot.btree
                                && bt.transaction_state() == TransactionState::Write
                                && let Err(sp_err) = bt.pager_mut().release_subsavepoint(sp_slot)
                            {
                                return self.poison_with(sp_err);
                            }
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
                            count_changes: &mut self.count_changes,
                            wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                            default_locking_mode: &mut self.default_locking_mode,
                            writable_schema: &mut self.writable_schema,
                            working_dir: &self.working_dir,
                            busy_policy: &mut self.busy_policy,
                            rollback_count: &mut self.rollback_count,
                            deferred_fk_suspects: &mut self.deferred_fk_suspects,
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
                        self.runtime.last_changes = snap.rt_before.last_changes;
                        self.runtime.total_changes = snap.rt_before.total_changes;
                        self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
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
                self.runtime.last_changes = snap.rt_before.last_changes;
                self.runtime.total_changes = snap.rt_before.total_changes;
                self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
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
                            if bt.transaction_state() == TransactionState::Write {
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
                    if stmt_changed_schema {
                        self.restore_schemas_snapshot(&snap.schemas_before);
                        self.schema_generation += 1;
                        self.v1_expire_generation = snap.v1_expire_gen_before + 1;
                    }
                    self.schema_changed_in_tx = snap.schema_changed_before;
                } else {
                    // Autocommit statement abort: roll back write DBs and end reads started here!
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
            RunningExec::MaterializedRead {
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
                PendingDrop::TestBlob {
                    handle_id,
                    lease_cursor,
                    writable,
                } => {
                    let main_inst = self.db_set.main_slot().schema_instance_id;
                    if writable {
                        let dummy_snap = WriteAbortSnapshot {
                            stmt_sp_slot: None,
                            touched_write_instances: vec![main_inst],
                            started_read_instances: Vec::new(),
                            fk_before: self.deferred_fk_suspects.clone(),
                            rt_before: self.runtime,
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
                            Some((inst, _)) => vec![inst],
                            None => vec![main_inst],
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
        let disk_vfs = if inferred_kind == DatabaseKind::Memory && vfs.name() == "memdb" {
            Arc::new(OsVfs::with_lock_geometry(effective_options.lock_geometry))
        } else {
            Arc::clone(&vfs)
        };
        let btree = Btree::open_with_options(
            vfs,
            path,
            OpenFlags(flags.0 & !OpenFlags::URI.0),
            pager_opts,
        )?;
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

        let inner = ConnectionInner {
            db_set,
            connection_encoding,
            state: ConnectionState::Open,
            next_handle_id: 1,
            open_handles: BTreeMap::new(),
            activity: ActivityTracker::default(),
            savepoints: Vec::new(),
            runtime: VmRuntimeContext::default(),
            active_interrupt_flag: Arc::clone(&interrupt_flag),
            functions: FunctionRegistry::default(),
            collations: CollationRegistry::default(),
            progress: ProgressTracker::default(),
            commit_hooks: CommitCoordinatorHooks::default(),
            strict_subtype: true,
            explicit_tx: false,
            implicit_savepoint_tx: false,
            in_user_btree_tx: false,
            schema_changed_in_tx: false,
            foreign_keys: false,
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
            simulated_cleanup_io_error: None,
            panic_on_next_pending_drop: false,
        };

        let shared = Arc::new(ConnectionShared {
            conn_id: NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed),
            db_path,
            database_kind,
            is_readonly,
            open_options,
            interrupt_flag,
            gate_busy: AtomicBool::new(false),
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
        if any_in_tx {
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

    /// Replaces the `main` database with an in-memory SQLite Format 3 image (`sqlite3_deserialize`).
    pub fn deserialize(&self, data: &[u8], readonly: bool) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        if !inner.activity.has_no_active_tx() {
            return Err(SqliteError::new(ErrorCode::Busy, "database is locked"));
        }
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
                slot.schema = Schema::load(bt)?;
            }
        }
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
        InterruptHandle {
            flag: Arc::clone(&self.shared.interrupt_flag),
        }
    }

    /// Returns the default atomic interrupt flag attached to this connection.
    #[inline]
    pub fn interrupt_flag(&self) -> &Arc<AtomicBool> {
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

    /// Installs or clears the connection commit hook (`sqlite3_commit_hook`).
    /// Returning `true` (non-zero) aborts the commit with `SQLITE_CONSTRAINT_COMMITHOOK` and rolls back.
    pub fn commit_hook(&self, hook: Option<CommitHookCallback>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.commit_hooks.set_commit_hook(hook);
        Ok(())
    }

    /// Replaces or restores the `Btree` atomic interrupt flag under the connection gate.
    pub fn set_interrupt_flag(&self, flag: Option<Arc<AtomicBool>>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
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
        inner.schema_generation += 1;
        inner.v1_expire_generation += 1;
        Ok(())
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
                touched_write_instances: vec![main_inst],
                started_read_instances: Vec::new(),
                fk_before: inner.deferred_fk_suspects.clone(),
                rt_before: inner.runtime,
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
                Some((inst, _)) => vec![inst],
                None => vec![main_inst],
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
    compiled_vdbe: Option<VdbeProgram>,
    compiled_schema_generation: u64,
    compiled_v1_expire_generation: u64,
    compiled_slot_cookies: Vec<(SchemaInstanceId, u32, bool)>,
    expired: bool,
    column_names: Vec<String>,
    params: Vec<Value>,
    named_params: HashMap<String, usize>,
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
    compiled_vdbe: Option<VdbeProgram>,
    column_names: Vec<String>,
    max_param: usize,
    named_params: HashMap<String, usize>,
}

#[allow(clippy::too_many_arguments)]
fn compile_first_sql(
    raw_sql: &str,
    db_set: &DatabaseSet,
    writable_schema: bool,
    foreign_keys: bool,
    count_changes: bool,
    enable_test_functions: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Result<CompiledArtifacts> {
    let (stmt_opt, _) = parse_one(raw_sql)?;
    compile_parsed_stmt(
        stmt_opt,
        db_set,
        writable_schema,
        foreign_keys,
        count_changes,
        enable_test_functions,
        functions,
        collations,
    )
}

#[allow(clippy::too_many_arguments)]
fn compile_parsed_stmt(
    stmt_opt: Option<Stmt>,
    db_set: &DatabaseSet,
    writable_schema: bool,
    foreign_keys: bool,
    count_changes: bool,
    enable_test_functions: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Result<CompiledArtifacts> {
    let _tf_guard = TestFunctionsScope::enter(enable_test_functions);
    let Some(stmt) = stmt_opt else {
        return Ok(CompiledArtifacts {
            stmt: None,
            requires_schema: false,
            category: None,
            compiled_vdbe: None,
            column_names: Vec::new(),
            max_param: 0,
            named_params: HashMap::new(),
        });
    };

    let requires_schema = stmt_requires_schema(&stmt);
    let category = Some(classify_stmt_category(&stmt));
    let mut max_param = 0usize;
    let mut named_params = HashMap::new();
    collect_stmt_params(&stmt, &mut max_param, &mut named_params);

    let column_names = validate_and_infer_columns(
        &stmt,
        db_set,
        writable_schema,
        foreign_keys,
        count_changes,
        functions,
        collations,
    )?;
    let mut compiled_vdbe = None;
    // Only use fast-path `VdbeVm` when all `DatabaseId` slots are `0` (`main`) or `1` (`temp`),
    // which never shift on `DETACH`. Attached `slots[2..]` queries run via `MaterializedRead`
    // keyed by `SchemaInstanceId` so `DETACH` slot-collapse never shifts active cursor indices!
    if let Stmt::Select(ref sel) = stmt
        && let Some(prog) = compiler::try_compile_select_in_db_set(sel, db_set, functions)?
        && prog.ops.iter().all(|ins| ins.db.index() < 2)
    {
        compiled_vdbe = Some(prog);
    }

    Ok(CompiledArtifacts {
        stmt: Some(stmt),
        requires_schema,
        category,
        compiled_vdbe,
        column_names,
        max_param,
        named_params,
    })
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
                .find_user_table_in_set(qn)
                .map(|(id, _)| id)
                .or(Some(DatabaseId::MAIN))
        }
    };
    match stmt {
        Stmt::Insert(ins) => resolve_qn_table(&ins.table),
        Stmt::Update(upd) => resolve_qn_table(&upd.table),
        Stmt::Delete(del) => resolve_qn_table(&del.table),
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
        Stmt::DropTable { name, .. } | Stmt::AlterTable { table: name, .. } => {
            resolve_qn_table(name)
        }
        Stmt::CreateIndex(ci) => {
            if let Some(ref s) = ci.name.schema {
                db_set.find_db_id(s)
            } else {
                db_set
                    .find_user_table_in_set(&QualifiedName::unqualified(ci.table_name.clone()))
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
                    .find_user_table_in_set(qn)
                    .map(|(id, _)| id)
                    .or_else(|| db_set.find_index_in_set(qn).map(|(id, _)| id))
            }
        }
        Stmt::Reindex(None) => None,
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
                sl.btree.is_some() && sl.schema.find_user_table(&ct.name.name).is_some()
            }),
        Stmt::CreateTableAs {
            if_not_exists: true,
            name,
            ..
        } => target_write_db_for_stmt(stmt, db_set)
            .and_then(|id| db_set.slot(id))
            .is_some_and(|sl| {
                sl.btree.is_some() && sl.schema.find_user_table(&name.name).is_some()
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
            db_set.find_user_table_in_set(name).is_none()
        }
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
            count_changes: &mut self.count_changes,
            wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
            default_locking_mode: &mut self.default_locking_mode,
            writable_schema: &mut self.writable_schema,
            working_dir: &self.working_dir,
            busy_policy: &mut self.busy_policy,
            rollback_count: &mut self.rollback_count,
            deferred_fk_suspects: &mut self.deferred_fk_suspects,
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

        let mut artifacts = match compile_parsed_stmt(
            stmt_opt.clone(),
            &inner.db_set,
            inner.writable_schema,
            inner.foreign_keys,
            inner.count_changes,
            inner.runtime.enable_test_functions,
            &inner.functions,
            &inner.collations,
        ) {
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
                    compile_parsed_stmt(
                        stmt_opt,
                        &inner.db_set,
                        inner.writable_schema,
                        inner.foreign_keys,
                        inner.count_changes,
                        inner.runtime.enable_test_functions,
                        &inner.functions,
                        &inner.collations,
                    )?
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
            compiled_schema_generation: inner.schema_generation,
            compiled_v1_expire_generation: inner.v1_expire_generation,
            compiled_slot_cookies: inner.snapshot_slot_cookies(),
            expired: false,
            column_names: artifacts.column_names,
            params: vec![Value::Null; artifacts.max_param],
            named_params: artifacts.named_params,
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

    /// Result column names (`sqlite3_column_name`).
    #[inline]
    pub fn column_names(&self) -> &[String] {
        &self.column_names
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

    /// Binds a 1-based parameter (`?1`, `?`, etc.) (`sqlite3_bind_*`).
    /// Requires the statement to be in `Ready` state (`ErrorCode::Misuse` if `Running` or `Halted` without `reset()`).
    pub fn bind(&mut self, index_1_based: usize, value: impl Into<Value>) -> Result<&mut Self> {
        let shared = Arc::clone(&self.shared);
        let session = shared.enter()?;
        self.bind_one_in_session(session.inner(), index_1_based, value.into())?;
        drop(session);
        Ok(self)
    }

    /// Binds a named parameter (`:name`, `@name`, or `$name`).
    /// Requires the statement to be in `Ready` state (`ErrorCode::Misuse` if `Running` or `Halted` without `reset()`).
    pub fn bind_named(&mut self, name: &str, value: impl Into<Value>) -> Result<&mut Self> {
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
    pub fn bind_all(&mut self, values: &[Value]) -> Result<&mut Self> {
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

    fn slot_cookies_changed(&self, inner: &ConnectionInner) -> bool {
        for &(inst, cookie, had_pages) in &self.compiled_slot_cookies {
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

    fn needs_reprepare(&self, inner: &ConnectionInner) -> bool {
        if self.expired {
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
            inner.refresh_open_tx_schemas()?;
            if self.stmt.as_ref().is_some_and(stmt_loads_schema_on_prepare) {
                let any_unloaded = inner
                    .db_set
                    .slots()
                    .iter()
                    .any(|s| s.btree.is_some() && !s.schema.loaded);
                if any_unloaded {
                    let _ = inner.ensure_schemas_loaded_for_prepare(false)?;
                }
            }
            let mut artifacts = match compile_first_sql(
                &self.raw_sql,
                &inner.db_set,
                inner.writable_schema,
                inner.foreign_keys,
                inner.count_changes,
                inner.runtime.enable_test_functions,
                &inner.functions,
                &inner.collations,
            ) {
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
                        compile_first_sql(
                            &self.raw_sql,
                            &inner.db_set,
                            inner.writable_schema,
                            inner.foreign_keys,
                            inner.count_changes,
                            inner.runtime.enable_test_functions,
                            &inner.functions,
                            &inner.collations,
                        )?
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
            self.column_names = artifacts.column_names;
            self.params = transferred;
            self.named_params = artifacts.named_params;
            self.compiled_schema_generation = inner.schema_generation;
            self.compiled_v1_expire_generation = inner.v1_expire_generation;
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

        // Empty / comment-only SQL (`stmt.is_none()`)
        if self.stmt.is_none() {
            self.lifecycle = StatementLifecycle::Halted { last_error: None };
            return Ok(StepResult::Done);
        }

        if matches!(self.lifecycle, StatementLifecycle::Ready) {
            return self.start_execution_in_session(hid, shared, inner);
        }

        // Advance already-running execution (`StatementLifecycle::Running`)
        self.advance_running_in_session(hid, shared, inner)
    }

    fn start_execution_in_session(
        &mut self,
        hid: HandleId,
        shared: &Arc<ConnectionShared>,
        inner: &mut ConnectionInner,
    ) -> Result<StepResult> {
        let initial_cat = self.category.expect("non-empty statement has category");
        if initial_cat != StmtCategory::Control
            && let Err(e) = inner.refresh_open_tx_schemas()
        {
            self.lifecycle = StatementLifecycle::Halted {
                last_error: Some(e.clone()),
            };
            return Err(e);
        }

        if (initial_cat == StmtCategory::Control && self.expired)
            || (initial_cat != StmtCategory::Control && self.needs_reprepare(inner))
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
        }

        let category = self.category.expect("non-empty statement has category");

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
                        self.compiled_schema_generation = inner.schema_generation;
                        self.compiled_v1_expire_generation = inner.v1_expire_generation;
                        self.compiled_slot_cookies = inner.snapshot_slot_cookies();
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
                let mut started_here: Vec<SchemaInstanceId> = Vec::new();
                let mut read_instances: Vec<SchemaInstanceId> = Vec::new();

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

                if let Some(ref prog) = self.compiled_vdbe {
                    let mut tx_dbs: Vec<DatabaseId> = Vec::new();
                    for ins in &prog.ops {
                        if ins.opcode == Opcode::Transaction && !tx_dbs.contains(&ins.db) {
                            tx_dbs.push(ins.db);
                        }
                    }
                    for &db_id in &tx_dbs {
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

                    if !tx_dbs.is_empty() && self.requires_schema {
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
                                for ins in &prog2.ops {
                                    if ins.opcode == Opcode::Transaction {
                                        let db_id = ins.db;
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
                                                inner
                                                    .busy_policy
                                                    .retry_btree(bt, |b| b.begin_read())
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
                            Vec::new(),
                        );
                        let mut vm_rt = inner.runtime;
                        vm_rt.stmt_timestamp_ms = Some(
                            inner
                                .db_set
                                .main_btree()
                                .pager()
                                .vfs()
                                .current_unix_millis(),
                        );
                        let vm = VdbeVm::new(prog_final.clone(), self.params.clone(), vm_rt);
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

                let stmt_clone = self.stmt.clone().expect("non-empty statement");
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
                    |ctx| execute_read_body(ctx, &stmt_clone, &self.params),
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
                    touched_write_instances: vec![db_inst],
                    started_read_instances: Vec::new(),
                    fk_before: inner.deferred_fk_suspects.clone(),
                    rt_before: inner.runtime,
                    schemas_before: inner.snapshot_open_schemas(),
                    schema_gen_before: inner.schema_generation,
                    v1_expire_gen_before: inner.v1_expire_generation,
                    schema_changed_before: inner.schema_changed_in_tx,
                };

                inner
                    .activity
                    .start_with_instances(hid, true, true, vec![db_inst], vec![db_inst]);
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
                let stmt_ref = self.stmt.as_ref().expect("non-empty statement");
                let target_db = target_write_db_for_stmt(stmt_ref, &inner.db_set);
                let skip_prelock = is_noop_if_exists_ddl(stmt_ref, &inner.db_set);

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
                }

                // Open an anonymous statement subsavepoint whenever an outer tx or another writer exists!
                let stmt_sp_slot = (!inner.is_autocommit() || pre_summary.other_writers > 0)
                    .then_some(inner.savepoints.len());
                let mut stmt_tx = StmtTxTracker {
                    stmt_sp_slot,
                    ..StmtTxTracker::default()
                };
                let initial_insts: Vec<SchemaInstanceId> = match prelocked_write {
                    Some((db_id, inst)) => {
                        let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                        stmt_tx.touched_write_mask |= bit;
                        stmt_tx.write_db_mask |= bit;
                        stmt_tx.read_db_mask |= bit;
                        if !inner.is_autocommit() {
                            inner.in_user_btree_tx = true;
                        }
                        vec![inst]
                    }
                    None => Vec::new(),
                };

                let schemas_before = inner.snapshot_open_schemas();
                let fk_before = inner.deferred_fk_suspects.clone();
                let rt_before = inner.runtime;
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
                        started_read_instances: Vec::new(),
                        fk_before,
                        rt_before,
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

                let stmt_clone = self.stmt.clone().expect("non-empty statement");
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
                let exec_res = inner.with_exec_ctx(
                    shared,
                    summary,
                    &mut stmt_tx,
                    &mut expire_self_only,
                    |ctx| execute_write_body(ctx, &stmt_clone, &self.params),
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
                        self.compiled_schema_generation = inner.schema_generation;
                        self.compiled_v1_expire_generation = inner.v1_expire_generation;
                        self.compiled_slot_cookies = inner.snapshot_slot_cookies();

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
                        inner.runtime = vm.runtime_ctx;
                        inner.runtime.stmt_timestamp_ms = None;
                        inner.publish(shared);
                        Ok(StepResult::Row(Row::new(vals)))
                    }
                    Ok(VmStep::Done) => {
                        vm.close_cursors(&mut inner.db_set);
                        let r_insts = std::mem::take(read_instances);
                        inner.runtime = vm.runtime_ctx;
                        inner.runtime.stmt_timestamp_ms = None;
                        inner.publish(shared);
                        let fin_res = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadDone {
                                lease_cursors: Vec::new(),
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
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
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
}
