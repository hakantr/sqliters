//! `sqliters-core`: High-level SQLite 3.54.1 database engine, schema catalog, compiler,
//! and public `Connection` / `Statement` / `Row` API (`main.c`, `legacy.c`, `vdbeapi.c`).
#![forbid(unsafe_code)]

pub mod compiler;
pub mod engine;
pub mod execution_context;
pub mod schema;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
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
    SQLITE_DEFAULT_PAGE_SIZE, SqliteError, Value, extended,
};
pub use sqliters_vdbe::BtreeAccess;
use sqliters_vdbe::{TestFunctionsScope, VdbeProgram, VdbeVm, VmRuntimeContext, VmStep};
use sqliters_vfs::{MemoryVfs, OpenFlags, OsVfs, Vfs};

use crate::engine::{
    ActivitySummary, BusyPolicy, ExecContext, LeaseCursorSpec, SqlSavepoint, StmtCategory,
    check_deferred_fks, classify_stmt_category, collect_select_referenced_roots,
    commit_transaction, execute_control_stmt, execute_nolock_stmt, execute_read_body,
    execute_write_body, parse_auto_vacuum_pragma, parse_incremental_vacuum_limit,
    release_savepoint_op, stmt_loads_schema_on_prepare, stmt_requires_schema,
    validate_and_infer_columns,
};
pub use engine::{QueryOutput, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT};
pub use execution_context::{
    CollationCallback, CollationRegistry, ExecutionContext, FunctionRegistry, ProgressCallback,
    ProgressTracker, ScalarFunctionCallback, ScalarFunctionDef,
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
}

impl Default for ConnectionOpenOptions {
    fn default() -> Self {
        Self {
            default_page_size: SQLITE_DEFAULT_PAGE_SIZE,
            lock_geometry: LockGeometry::default(),
            temp_store_memory: false,
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

#[derive(Debug, Clone, Copy, Default)]
struct ActiveLease {
    is_reader: bool,
    is_writer: bool,
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
            },
        );
    }

    fn remove(&mut self, id: HandleId) -> Option<ActiveLease> {
        self.leases.remove(&id)
    }

    fn summary_excluding(&self, exclude: HandleId) -> ActivitySummary {
        let mut other_active = 0usize;
        let mut other_readers = 0usize;
        let mut other_writers = 0usize;
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
        }
        ActivitySummary {
            other_active,
            other_readers,
            other_writers,
        }
    }

    fn has_no_active_tx(&self) -> bool {
        !self.leases.values().any(|l| l.is_reader || l.is_writer)
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
    fk_before: Vec<(String, schema::TableRowKey, usize)>,
    rt_before: VmRuntimeContext,
    schema_before: Schema,
    schema_gen_before: u64,
    schema_changed_before: bool,
}

enum RunningExec {
    Vdbe {
        vm: VdbeVm,
    },
    MaterializedRead {
        rows: std::vec::IntoIter<Vec<Value>>,
        lease_cursors: Vec<CursorId>,
    },
    MaterializedNoLock {
        rows: std::vec::IntoIter<Vec<Value>>,
    },
    ReturningWrite {
        rows: std::vec::IntoIter<Vec<Value>>,
        abort_snapshot: WriteAbortSnapshot,
    },
    IncrementalVacuum {
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
        running: Option<RunningExec>,
    },
    TestBlob {
        handle_id: HandleId,
        lease_cursor: Option<CursorId>,
        writable: bool,
    },
}

enum ActivityFinish {
    NoLock,
    ReadDone { lease_cursors: Vec<CursorId> },
    ReadAborted { lease_cursors: Vec<CursorId> },
    WriteCompleted { abort_snapshot: WriteAbortSnapshot },
    PendingCommitAborted,
}

struct ConnectionInner {
    btree: Btree,
    schema: Schema,
    schema_loaded: bool,
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
    strict_subtype: bool,
    explicit_tx: bool,
    implicit_savepoint_tx: bool,
    in_user_btree_tx: bool,
    schema_changed_in_tx: bool,
    foreign_keys: bool,
    count_changes: bool,
    wal_autocheckpoint_threshold: i32,
    default_locking_mode: LockingMode,
    next_auto_vacuum: Option<AutoVacuumMode>,
    next_page_size: Option<u32>,
    writable_schema: bool,
    working_dir: Option<PathBuf>,
    busy_policy: BusyPolicy,
    rollback_count: u64,
    deferred_fk_suspects: Vec<(String, schema::TableRowKey, usize)>,
    schema_generation: u64,
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
                    if let Some(RunningExec::Vdbe { mut vm }) = running {
                        vm.close_cursors(&mut inner.btree);
                    } else if let Some(RunningExec::MaterializedRead { lease_cursors, .. }) =
                        running
                    {
                        for cid in lease_cursors {
                            inner.btree.close_cursor(cid);
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
                    if let Some(cid) = lease_cursor {
                        inner.btree.close_cursor(cid);
                    }
                    inner.activity.remove(handle_id);
                    inner.open_handles.remove(&handle_id);
                }
            }
        }
        if !matches!(inner.state, ConnectionState::Closed) {
            inner.btree.close_all_cursors();
            if inner.btree.transaction_state() != TransactionState::Open {
                let _ =
                    inner
                        .btree
                        .rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
            }
            let _ = inner.btree.close();
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

    /// Ensures `sqlite_schema` is loaded for `prepare`, releasing any temporary `Read` transaction
    /// immediately if no user/statement B-tree transaction was active (`sqlite3InitOne.openedTransaction`).
    fn ensure_schema_loaded_for_prepare(&mut self) -> Result<bool> {
        if self.btree.transaction_state() == TransactionState::Open {
            self.busy_policy
                .retry_btree(&mut self.btree, |bt| bt.begin_read())?;
            let prev_cookie = self.schema.schema_cookie;
            let prev_had_pages = self.schema.had_pages;
            let prev_loaded = self.schema.loaded;
            let load_res = self.schema.ensure_fresh_in_tx(&mut self.btree);
            let end_res = if !self.in_user_btree_tx && self.activity.has_no_active_tx() {
                self.btree.pager_mut().end_read_if_unused()
            } else {
                Ok(())
            };
            load_res?;
            end_res?;
            let changed = !self.schema_loaded
                || !prev_loaded
                || prev_cookie != self.schema.schema_cookie
                || prev_had_pages != self.schema.had_pages;
            self.schema_loaded = true;
            if changed {
                self.schema_generation += 1;
            }
            Ok(changed)
        } else {
            let prev_cookie = self.schema.schema_cookie;
            let prev_had_pages = self.schema.had_pages;
            let prev_loaded = self.schema.loaded;
            self.schema.ensure_fresh_in_tx(&mut self.btree)?;
            let changed = !self.schema_loaded
                || !prev_loaded
                || prev_cookie != self.schema.schema_cookie
                || prev_had_pages != self.schema.had_pages;
            self.schema_loaded = true;
            if changed {
                self.schema_generation += 1;
            }
            Ok(changed)
        }
    }

    fn refresh_schema_in_open_tx(&mut self) -> Result<()> {
        let has_pages = self.btree.pager().page_count() > 0;
        let cur_cookie = self.btree.pager().header().schema_cookie;
        if !self.schema_loaded
            || !self.schema.loaded
            || self.schema.had_pages != has_pages
            || self.schema.schema_cookie != cur_cookie
        {
            self.schema.ensure_fresh_in_tx(&mut self.btree)?;
            self.schema_loaded = true;
            self.schema_generation += 1;
        }
        Ok(())
    }

    fn maybe_run_wal_autocheckpoint(&mut self) {
        if !self.is_autocommit() {
            return;
        }
        let n_entry = self.btree.pager_mut().take_wal_callback_frame_count();
        let threshold = self.wal_autocheckpoint_threshold;
        if n_entry > 0
            && threshold > 0
            && i64::from(n_entry) >= i64::from(threshold)
            && self.btree.transaction_state() == TransactionState::Open
            && self.activity.has_no_active_tx()
        {
            let _ = self.btree.wal_checkpoint(WalCheckpointMode::Passive, None);
        }
    }

    fn wal_checkpoint_internal(
        &mut self,
        db_name: Option<&str>,
        mode: WalCheckpointMode,
    ) -> Result<WalCheckpointResult> {
        let target_is_temp = match db_name {
            None | Some("") => false,
            Some(s) if s.eq_ignore_ascii_case("main") => false,
            Some(s) if s.eq_ignore_ascii_case("temp") => true,
            Some(s) => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database: {s}"),
                ));
            }
        };
        if target_is_temp {
            return Ok(WalCheckpointResult::not_wal());
        }
        if self.btree.transaction_state() != TransactionState::Open
            || !self.activity.has_no_active_tx()
        {
            return Err(SqliteError::new(
                ErrorCode::Locked,
                "database table is locked",
            ));
        }
        if self.btree.pager().db_kind() == DatabaseKind::NamedDisk
            && (!self.btree.pager().is_header_loaded()
                || (self.btree.pager().journal_mode() == JournalMode::Wal
                    && self.btree.pager().wal().is_none()))
        {
            self.busy_policy
                .retry_btree(&mut self.btree, |bt| bt.begin_read())?;
            self.btree.pager_mut().end_read_if_unused()?;
        }
        if self.btree.pager().wal().is_none() {
            return Ok(WalCheckpointResult::not_wal());
        }
        self.busy_policy.checkpoint_btree(&mut self.btree, mode)
    }

    fn finish_activity(
        &mut self,
        hid: HandleId,
        outcome: ActivityFinish,
        shared: &ConnectionShared,
    ) -> Result<()> {
        let _removed = self.activity.remove(hid);
        let summary = self.activity.summary_excluding(hid);

        if let Some(sim_err) = self.take_simulated_cleanup_error() {
            match outcome {
                ActivityFinish::ReadDone { lease_cursors }
                | ActivityFinish::ReadAborted { lease_cursors } => {
                    for cid in lease_cursors {
                        self.btree.close_cursor(cid);
                    }
                }
                _ => {}
            }
            return Err(self.poison_with(sim_err));
        }

        match outcome {
            ActivityFinish::NoLock | ActivityFinish::PendingCommitAborted => Ok(()),

            ActivityFinish::ReadDone { lease_cursors }
            | ActivityFinish::ReadAborted { lease_cursors } => {
                for cid in lease_cursors {
                    self.btree.close_cursor(cid);
                }
                if summary.other_readers == 0
                    && summary.other_writers == 0
                    && !self.in_user_btree_tx
                    && self.btree.transaction_state() == TransactionState::Read
                    && let Err(e) = self.btree.pager_mut().end_read_if_unused()
                {
                    return Err(self.poison_with(e));
                }
                Ok(())
            }

            ActivityFinish::WriteCompleted { abort_snapshot } => {
                if !self.is_autocommit() {
                    if let Some(slot) = abort_snapshot.stmt_sp_slot
                        && self.btree.transaction_state() == TransactionState::Write
                        && let Err(e) = self.btree.pager_mut().release_subsavepoint(slot)
                    {
                        return Err(self.poison_with(e));
                    }
                    if summary.other_writers == 0
                        && summary.other_readers == 0
                        && let Err(e) = self.btree.spill_if_over_cache_size()
                    {
                        return Err(self.poison_with(e));
                    }
                    self.publish(shared);
                    return Ok(());
                }

                if summary.other_writers > 0 {
                    if let Some(slot) = abort_snapshot.stmt_sp_slot
                        && self.btree.transaction_state() == TransactionState::Write
                        && let Err(e) = self.btree.pager_mut().release_subsavepoint(slot)
                    {
                        return Err(self.poison_with(e));
                    }
                    self.publish(shared);
                    return Ok(());
                }

                // Autocommit write completion with no other active writers: verify deferred FKs first!
                let fk_res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut self.btree,
                        schema: &mut self.schema,
                        runtime: &mut self.runtime,
                        interrupt_flag: &self.active_interrupt_flag,
                        functions: &self.functions,
                        collations: &self.collations,
                        progress: &mut self.progress,
                        strict_subtype: self.strict_subtype,
                        explicit_tx: &mut self.explicit_tx,
                        implicit_savepoint_tx: &mut self.implicit_savepoint_tx,
                        in_user_btree_tx: &mut self.in_user_btree_tx,
                        savepoints: &mut self.savepoints,
                        schema_generation: &mut self.schema_generation,
                        schema_changed_in_tx: &mut self.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut self.foreign_keys,
                        count_changes: &mut self.count_changes,
                        wal_autocheckpoint_threshold: &mut self.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut self.default_locking_mode,
                        next_auto_vacuum: &mut self.next_auto_vacuum,
                        next_page_size: &mut self.next_page_size,
                        writable_schema: &mut self.writable_schema,
                        working_dir: &self.working_dir,
                        busy_policy: &mut self.busy_policy,
                        rollback_count: &mut self.rollback_count,
                        deferred_fk_suspects: &mut self.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    check_deferred_fks(&mut ctx)
                };
                if let Err(fk_err) = fk_res {
                    let write_only = !self.schema_changed_in_tx;
                    let retain_read = summary.other_readers > 0 && write_only;
                    if let Err(rb_err) = self.btree.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        write_only,
                        retain_read,
                    ) {
                        return Err(self.poison_with(rb_err));
                    }
                    self.runtime.last_changes = abort_snapshot.rt_before.last_changes;
                    self.runtime.total_changes = abort_snapshot.rt_before.total_changes;
                    self.runtime.last_insert_rowid = abort_snapshot.rt_before.last_insert_rowid;
                    self.deferred_fk_suspects.clear();
                    if self.schema_changed_in_tx {
                        self.schema = abort_snapshot.schema_before;
                        self.schema_generation += 1;
                        self.schema_changed_in_tx = false;
                    }
                    self.publish(shared);
                    return Err(fk_err);
                }

                let retain_read = summary.other_readers > 0;
                if self.btree.transaction_state() == TransactionState::Write
                    && let Err(cm_err) = self.btree.commit_retaining_read(retain_read)
                {
                    let write_only = !self.schema_changed_in_tx;
                    if let Err(rb_err) = self.btree.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        write_only,
                        retain_read && write_only,
                    ) {
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
                    if self.schema_changed_in_tx {
                        self.schema = abort_snapshot.schema_before;
                        self.schema_generation += 1;
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
        let summary = self.activity.summary_excluding(hid);

        if let Some(sim_err) = self.take_simulated_cleanup_error() {
            return self.poison_with(sim_err);
        }

        match err.conflict_effect {
            ConflictResolutionEffect::Rollback => {
                let schema_changed =
                    self.schema_changed_in_tx || self.schema_generation != snap.schema_gen_before;
                let write_only = !schema_changed;
                let retain_read = summary.other_readers > 0 && write_only;
                if self.btree.transaction_state() == TransactionState::Write
                    && let Err(rb_err) = self.btree.rollback_with_trip(
                        extended::SQLITE_ABORT_ROLLBACK,
                        write_only,
                        retain_read,
                    )
                {
                    return self.poison_with(rb_err);
                }
                self.explicit_tx = false;
                self.implicit_savepoint_tx = false;
                self.in_user_btree_tx = false;
                self.savepoints.clear();
                self.deferred_fk_suspects.clear();
                self.rollback_count += 1;
                self.runtime.last_changes = snap.rt_before.last_changes;
                self.runtime.last_insert_rowid = snap.rt_before.last_insert_rowid;
                if schema_changed {
                    self.schema_changed_in_tx = false;
                    if let Ok(s) = Schema::load(&mut self.btree) {
                        self.schema = s;
                    } else {
                        self.schema = snap.schema_before;
                    }
                    self.schema_generation += 1;
                }
                self.publish(shared);
                err.clone()
            }

            ConflictResolutionEffect::Fail => {
                let mut committed_autocommit = false;
                if !self.is_autocommit() || summary.other_writers > 0 {
                    if let Some(slot) = snap.stmt_sp_slot
                        && self.btree.transaction_state() == TransactionState::Write
                        && let Err(sp_err) = self.btree.pager_mut().release_subsavepoint(slot)
                    {
                        return self.poison_with(sp_err);
                    }
                    if summary.other_writers == 0
                        && summary.other_readers == 0
                        && let Err(sp_err) = self.btree.spill_if_over_cache_size()
                    {
                        return self.poison_with(sp_err);
                    }
                } else if self.btree.transaction_state() == TransactionState::Write {
                    let retain_read = summary.other_readers > 0;
                    if let Err(cm_err) = self.btree.commit_retaining_read(retain_read) {
                        let write_only = !self.schema_changed_in_tx;
                        if let Err(rb_err) = self.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            write_only,
                            retain_read && write_only,
                        ) {
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
                        if self.schema_changed_in_tx {
                            self.schema = snap.schema_before;
                            self.schema_generation += 1;
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

                if let Some(slot) = snap.stmt_sp_slot {
                    let _ = self
                        .btree
                        .trip_cursors(extended::SQLITE_ABORT_ROLLBACK, !stmt_changed_schema);
                    if self.btree.transaction_state() == TransactionState::Write {
                        if let Err(rb_err) = self.btree.pager_mut().rollback_to_subsavepoint(slot) {
                            return self.poison_with(rb_err);
                        }
                        if let Err(rel_err) = self.btree.pager_mut().release_subsavepoint(slot) {
                            return self.poison_with(rel_err);
                        }
                    }
                    if stmt_changed_schema {
                        self.schema = snap.schema_before;
                        self.schema_generation += 1;
                    }
                    self.schema_changed_in_tx = snap.schema_changed_before;
                } else {
                    let write_only = !stmt_changed_schema && !self.schema_changed_in_tx;
                    let retain_read = summary.other_readers > 0 && write_only;
                    if self.btree.transaction_state() == TransactionState::Write
                        && let Err(rb_err) = self.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            write_only,
                            retain_read,
                        )
                    {
                        return self.poison_with(rb_err);
                    }
                    if stmt_changed_schema || self.schema_changed_in_tx {
                        self.schema = snap.schema_before;
                        self.schema_generation += 1;
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
            RunningExec::Vdbe { mut vm } => {
                vm.close_cursors(&mut self.btree);
                self.finish_activity(
                    hid,
                    ActivityFinish::ReadDone {
                        lease_cursors: Vec::new(),
                    },
                    shared,
                )
            }
            RunningExec::MaterializedRead { lease_cursors, .. } => {
                self.finish_activity(hid, ActivityFinish::ReadDone { lease_cursors }, shared)
            }
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
                        let _ = self.finish_running_exec(handle_id, run, shared);
                    }
                    self.open_handles.remove(&handle_id);
                }
                PendingDrop::TestBlob {
                    handle_id,
                    lease_cursor,
                    writable,
                } => {
                    if writable {
                        let dummy_snap = WriteAbortSnapshot {
                            stmt_sp_slot: None,
                            fk_before: self.deferred_fk_suspects.clone(),
                            rt_before: self.runtime,
                            schema_before: self.schema.clone(),
                            schema_gen_before: self.schema_generation,
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
                        let cursors = lease_cursor.into_iter().collect();
                        let _ = self.finish_activity(
                            handle_id,
                            ActivityFinish::ReadDone {
                                lease_cursors: cursors,
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
        let vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(options.lock_geometry));
        let btree = Btree::open_with_options(
            vfs,
            Some(Path::new(":memory:")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
            pager_opts,
        )?;
        Self::from_btree(btree, options)
    }

    /// Opens an isolated private temporary database (`""`, [`DatabaseKind::PrivateTemporary`]).
    pub fn open_private_temporary() -> Result<Self> {
        Self::open_private_temporary_with_options(ConnectionOpenOptions::default())
    }

    /// Opens an isolated private temporary database (`""`) with explicit [`ConnectionOpenOptions`].
    pub fn open_private_temporary_with_options(options: ConnectionOpenOptions) -> Result<Self> {
        let pager_opts = options.to_pager_options(DatabaseKind::PrivateTemporary);
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
        let btree = Btree::open_private_temporary(vfs, pager_opts)?;
        Self::from_btree(btree, options)
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
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
        let mut pager_opts = options.to_pager_options(DatabaseKind::NamedDisk);
        pager_opts.immutable = true;
        let btree = Btree::open_with_options(vfs, Some(p), OpenFlags::READONLY, pager_opts)?;
        Self::from_btree(btree, options)
    }

    /// Opens a database with explicit [`OpenFlags`] and [`ConnectionOpenOptions`].
    pub fn open_with_options(
        path: impl AsRef<Path>,
        flags: OpenFlags,
        options: ConnectionOpenOptions,
    ) -> Result<Self> {
        let p = path.as_ref();
        if flags.contains(OpenFlags::URI)
            && let Some(s) = p.to_str()
            && let Some(rest) = s.strip_prefix("file:")
        {
            let (raw_path, query) = match rest.split_once('?') {
                Some((rp, q)) => (rp, Some(q)),
                None => (rest, None),
            };
            let mut is_immutable = false;
            let mut mode_ro = false;
            let mut mode_memory = false;
            if let Some(q) = query {
                for pair in q.split('&') {
                    if let Some((k, v)) = pair.split_once('=') {
                        if k == "immutable" && (v == "1" || v.eq_ignore_ascii_case("true")) {
                            is_immutable = true;
                        } else if k == "mode" {
                            if v.eq_ignore_ascii_case("ro") {
                                mode_ro = true;
                            } else if v.eq_ignore_ascii_case("memory") {
                                mode_memory = true;
                            }
                        }
                    }
                }
            }
            if is_immutable {
                return Self::open_immutable_with_options(Path::new(raw_path), options);
            }
            if mode_memory {
                return Self::open_in_memory_with_options(options);
            }
            let effective_flags = if mode_ro {
                OpenFlags::READONLY
            } else {
                OpenFlags(flags.0 & !OpenFlags::URI.0)
            };
            let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(options.lock_geometry));
            return Self::open_with_vfs_and_options(
                vfs,
                Some(Path::new(raw_path)),
                effective_flags,
                options,
            );
        }
        let vfs: Arc<dyn Vfs> = if p == Path::new(":memory:") || flags.contains(OpenFlags::MEMORY) {
            Arc::new(MemoryVfs::with_lock_geometry(options.lock_geometry))
        } else {
            Arc::new(OsVfs::with_lock_geometry(options.lock_geometry))
        };
        Self::open_with_vfs_and_options(vfs, Some(p), flags, options)
    }

    /// Opens a database backed by a custom [`Vfs`].
    pub fn open_with_vfs(vfs: Arc<dyn Vfs>, path: Option<&Path>, flags: OpenFlags) -> Result<Self> {
        let options = ConnectionOpenOptions {
            lock_geometry: vfs.lock_geometry(),
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
        let pager_opts = options.to_pager_options(inferred_kind);
        let btree = Btree::open_with_options(vfs, path, flags, pager_opts)?;
        Self::from_btree(btree, options)
    }

    fn from_btree(mut btree: Btree, open_options: ConnectionOpenOptions) -> Result<Self> {
        let interrupt_flag = Arc::new(AtomicBool::new(false));
        btree.set_interrupt_flag(Some(Arc::clone(&interrupt_flag)));

        let db_path = btree.pager().db_path().map(Path::to_path_buf);
        let database_kind = btree.pager().db_kind();
        let is_readonly = btree.pager().is_readonly();

        let inner = ConnectionInner {
            btree,
            schema: Schema::default(),
            schema_loaded: false,
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
            strict_subtype: true,
            explicit_tx: false,
            implicit_savepoint_tx: false,
            in_user_btree_tx: false,
            schema_changed_in_tx: false,
            foreign_keys: false,
            count_changes: false,
            wal_autocheckpoint_threshold: SQLITE_DEFAULT_WAL_AUTOCHECKPOINT,
            default_locking_mode: LockingMode::Normal,
            next_auto_vacuum: None,
            next_page_size: None,
            writable_schema: false,
            working_dir: None,
            busy_policy: BusyPolicy::default(),
            rollback_count: 0,
            deferred_fk_suspects: Vec::new(),
            schema_generation: 0,
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
        inner.btree.close_all_cursors();
        if inner.btree.transaction_state() != TransactionState::Open {
            let _ = inner
                .btree
                .rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
            inner.rollback_count += 1;
        }
        inner.explicit_tx = false;
        inner.implicit_savepoint_tx = false;
        inner.in_user_btree_tx = false;
        inner.savepoints.clear();
        inner.deferred_fk_suspects.clear();
        inner.publish(&self.shared);

        if let Err(close_err) = inner.btree.close() {
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
        if inner.btree.transaction_state() != TransactionState::Open {
            let _ = inner
                .btree
                .rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false);
        }
        inner
            .btree
            .pager_mut()
            .load_deserialized_image(data, readonly)?;
        inner.btree.set_writable_schema(inner.writable_schema);
        inner.schema = Schema::default();
        inner.schema_loaded = false;
        inner.schema_generation += 1;
        inner.next_auto_vacuum = None;
        inner.next_page_size = None;
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
        inner.schema = Schema::load(&mut inner.btree)?;
        inner.schema_loaded = true;
        inner.schema_generation += 1;
        Ok(())
    }

    /// Returns an owned snapshot of the current in-memory [`Schema`] catalog, loading or
    /// refreshing it from disk if needed.
    pub fn schema_snapshot(&self) -> Result<Schema> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let _ = inner.ensure_schema_loaded_for_prepare()?;
        Ok(inner.schema.clone())
    }

    /// Returns an owned copy of the authoritative 100-byte [`DatabaseHeader`], performing a brief
    /// locked header read if the connection is currently in `Open` (`NoLock`) state.
    pub fn database_header(&self) -> Result<DatabaseHeader> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        if inner.btree.transaction_state() == TransactionState::Open {
            inner
                .busy_policy
                .retry_btree(&mut inner.btree, |bt| bt.begin_read())?;
            let hdr = inner.btree.pager().header().clone();
            if !inner.in_user_btree_tx && inner.activity.has_no_active_tx() {
                inner.btree.pager_mut().end_read_if_unused()?;
            }
            Ok(hdr)
        } else {
            Ok(inner.btree.pager().header().clone())
        }
    }

    /// Returns the authoritative database page size (`512..=65536`), performing a brief locked
    /// header read if needed.
    pub fn page_size(&self) -> Result<u32> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        inner.btree.pager_mut().effective_page_size()
    }

    /// Returns the authoritative database page count, performing a brief locked header read if
    /// the connection is currently in `Open` (`NoLock`) state.
    pub fn page_count(&self) -> Result<Pgno> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        if inner.btree.transaction_state() == TransactionState::Open {
            inner
                .busy_policy
                .retry_btree(&mut inner.btree, |bt| bt.begin_read())?;
            let cnt = inner.btree.pager().page_count();
            if !inner.in_user_btree_tx && inner.activity.has_no_active_tx() {
                inner.btree.pager_mut().end_read_if_unused()?;
            }
            Ok(cnt)
        } else {
            Ok(inner.btree.pager().page_count())
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

    /// Replaces or restores the `Btree` atomic interrupt flag under the connection gate.
    pub fn set_interrupt_flag(&self, flag: Option<Arc<AtomicBool>>) -> Result<()> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        let eff = flag.unwrap_or_else(|| Arc::clone(&self.shared.interrupt_flag));
        inner.active_interrupt_flag = Arc::clone(&eff);
        inner.btree.set_interrupt_flag(Some(eff));
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
    /// `Some("main")`).
    ///
    /// Passing `mode < 0` queries the current flag without modifying it; `mode == 0` disables
    /// persistent `-wal`/`-shm` across last-connection close; `mode > 0` enables it.
    pub fn file_control_persist_wal(&self, db_name: Option<&str>, mode: i32) -> Result<bool> {
        let mut session = self.shared.enter()?;
        let inner = session.inner_mut();
        inner.ensure_usable()?;
        match db_name {
            None | Some("") => {}
            Some(s) if s.eq_ignore_ascii_case("main") => {}
            Some(_) => {
                return Err(SqliteError::new(ErrorCode::NotFound, "not found"));
            }
        }
        if mode >= 0 {
            inner.btree.pager_mut().set_persist_wal(mode != 0);
        }
        Ok(inner.btree.pager().persist_wal())
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
        if writable {
            if inner.btree.transaction_state() != TransactionState::Write {
                inner
                    .btree
                    .pager_mut()
                    .begin_write_with_savepoint_depth(inner.savepoints.len())?;
            }
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            let hid = inner.register_handle(HandleKind::TestBlob { writable: true });
            inner.activity.start(hid, true, true);
            Ok(TestBlobHandle {
                shared: Arc::clone(&self.shared),
                handle_id: Some(hid),
                lease_cursor: None,
                writable: true,
            })
        } else {
            if inner.btree.transaction_state() == TransactionState::Open {
                inner.btree.begin_read()?;
            }
            if !inner.is_autocommit() {
                inner.in_user_btree_tx = true;
            }
            let cid = inner.btree.open_table_cursor(1, false);
            if inner.btree.pager().page_count() >= 1 {
                let _ = inner.btree.cursor_first(cid);
            }
            let hid = inner.register_handle(HandleKind::TestBlob { writable: false });
            inner.activity.start(hid, true, false);
            Ok(TestBlobHandle {
                shared: Arc::clone(&self.shared),
                handle_id: Some(hid),
                lease_cursor: Some(cid),
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
        Ok(session.inner().btree.open_cursor_count())
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
                StatementLifecycle::Running(r) => Some(r),
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
    lease_cursor: Option<CursorId>,
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
        let res = if self.writable {
            let snap = WriteAbortSnapshot {
                stmt_sp_slot: None,
                fk_before: inner.deferred_fk_suspects.clone(),
                rt_before: inner.runtime,
                schema_before: inner.schema.clone(),
                schema_gen_before: inner.schema_generation,
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
            inner.finish_activity(
                hid,
                ActivityFinish::ReadDone {
                    lease_cursors: lc.into_iter().collect(),
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
    compiled_cookie: u32,
    compiled_had_pages: bool,
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

fn compile_first_sql(
    raw_sql: &str,
    schema: &Schema,
    foreign_keys: bool,
    count_changes: bool,
    enable_test_functions: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Result<CompiledArtifacts> {
    let (stmt_opt, _) = parse_one(raw_sql)?;
    compile_parsed_stmt(
        stmt_opt,
        schema,
        foreign_keys,
        count_changes,
        enable_test_functions,
        functions,
        collations,
    )
}

fn compile_parsed_stmt(
    stmt_opt: Option<Stmt>,
    schema: &Schema,
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
        schema,
        foreign_keys,
        count_changes,
        functions,
        collations,
    )?;
    let mut compiled_vdbe = None;
    if let Stmt::Select(ref sel) = stmt
        && let Some(prog) = compiler::try_compile_select_with_functions(sel, schema, functions)?
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
        if schema
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
        {
            artifacts.category = Some(StmtCategory::NoLockPragma);
        } else {
            let e_auto = parse_auto_vacuum_pragma(pv);
            inner.next_auto_vacuum = Some(e_auto);
            let ok = inner.btree.set_configured_auto_vacuum(e_auto);
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
            if !inner.schema_loaded {
                let _ = inner.ensure_schema_loaded_for_prepare()?;
            } else if inner.btree.transaction_state() != TransactionState::Open {
                inner.refresh_schema_in_open_tx()?;
            }
        }

        let mut artifacts = match compile_parsed_stmt(
            stmt_opt.clone(),
            &inner.schema,
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
                    && inner.btree.transaction_state() == TransactionState::Open =>
            {
                // Cached schema might be stale due to an external writer: check on-disk cookie once!
                if inner.ensure_schema_loaded_for_prepare()? {
                    compile_parsed_stmt(
                        stmt_opt,
                        &inner.schema,
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
            compiled_cookie: inner.schema.schema_cookie,
            compiled_had_pages: inner.schema.had_pages,
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

    fn needs_reprepare(&self, inner: &ConnectionInner) -> bool {
        if self.expired {
            return true;
        }
        if !self.requires_schema {
            return false;
        }
        self.compiled_schema_generation != inner.schema_generation
            || self.compiled_cookie != inner.schema.schema_cookie
            || self.compiled_had_pages != inner.schema.had_pages
    }

    fn reprepare_in_open_tx(&mut self, inner: &mut ConnectionInner) -> Result<()> {
        for _ in 0..SQLITE_MAX_SCHEMA_RETRY {
            if inner.btree.transaction_state() != TransactionState::Open {
                inner.refresh_schema_in_open_tx()?;
            } else if !inner.schema_loaded
                && self.stmt.as_ref().is_some_and(stmt_loads_schema_on_prepare)
            {
                let _ = inner.ensure_schema_loaded_for_prepare()?;
            }
            let mut artifacts = compile_first_sql(
                &self.raw_sql,
                &inner.schema,
                inner.foreign_keys,
                inner.count_changes,
                inner.runtime.enable_test_functions,
                &inner.functions,
                &inner.collations,
            )?;
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
            self.compiled_cookie = inner.schema.schema_cookie;
            self.compiled_had_pages = inner.schema.had_pages;
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
        if matches!(
            self.category,
            Some(
                StmtCategory::NoLockPragma | StmtCategory::Vacuum | StmtCategory::IncrementalVacuum
            )
        ) && self.needs_reprepare(inner)
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
                let summary = inner.activity.summary_excluding(hid);
                let res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    execute_control_stmt(&mut ctx, &stmt_clone)
                };
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
                let summary = inner.activity.summary_excluding(hid);
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                if prev_ts.is_none() {
                    inner.runtime.stmt_timestamp_ms =
                        Some(inner.btree.pager().vfs().current_unix_millis());
                }
                let res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
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
                        execute_nolock_stmt(&mut ctx, &stmt_clone)
                    }
                };
                inner.runtime.stmt_timestamp_ms = prev_ts;
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
                let summary = inner.activity.summary_excluding(hid);
                let res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    engine::exec_vacuum_checked(
                        &mut ctx,
                        vac_schema.as_deref(),
                        vac_into.as_ref(),
                        &self.params,
                    )
                };
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
                        inner.schema_loaded = true;
                        self.compiled_schema_generation = inner.schema_generation;
                        self.compiled_cookie = inner.schema.schema_cookie;
                        self.compiled_had_pages = inner.schema.had_pages;
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
                let opened_read_here = inner.btree.transaction_state() == TransactionState::Open;
                if opened_read_here
                    && let Err(e) = inner
                        .busy_policy
                        .retry_btree(&mut inner.btree, |bt| bt.begin_read())
                {
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                if self.requires_schema
                    && let Err(e) = inner.refresh_schema_in_open_tx()
                {
                    if opened_read_here
                        && !inner.in_user_btree_tx
                        && inner.activity.has_no_active_tx()
                    {
                        let _ = inner.btree.pager_mut().end_read_if_unused();
                    }
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                if self.needs_reprepare(inner) {
                    if self.mode == PrepareMode::V1 {
                        if opened_read_here
                            && !inner.in_user_btree_tx
                            && inner.activity.has_no_active_tx()
                        {
                            let _ = inner.btree.pager_mut().end_read_if_unused();
                        }
                        let err =
                            SqliteError::new(ErrorCode::Schema, "database schema has changed");
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
                    if let Err(e) = self.reprepare_in_open_tx(inner) {
                        if opened_read_here
                            && !inner.in_user_btree_tx
                            && inner.activity.has_no_active_tx()
                        {
                            let _ = inner.btree.pager_mut().end_read_if_unused();
                        }
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                }

                if !inner.is_autocommit() {
                    inner.in_user_btree_tx = true;
                }
                inner.activity.start(hid, true, false);

                if let Some(ref prog) = self.compiled_vdbe {
                    let mut vm_rt = inner.runtime;
                    vm_rt.stmt_timestamp_ms = Some(inner.btree.pager().vfs().current_unix_millis());
                    let vm = VdbeVm::new(prog.clone(), self.params.clone(), vm_rt);
                    self.lifecycle = StatementLifecycle::Running(RunningExec::Vdbe { vm });
                    return self.advance_running_in_session(hid, shared, inner);
                }

                // Interpreter `Read` path (`SELECT`, `EXPLAIN`, or read `PRAGMA`):
                if let Err(e) = inner.btree.check_interrupted() {
                    let _ = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadAborted {
                            lease_cursors: Vec::new(),
                        },
                        shared,
                    );
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                let stmt_clone = self.stmt.clone().expect("non-empty statement");
                let specs = collect_select_referenced_roots(&stmt_clone, &inner.schema);
                let mut lease_cursors = Vec::with_capacity(specs.len());
                let page_cnt = inner.btree.pager().page_count();
                for spec in specs {
                    match spec {
                        LeaseCursorSpec::Table(root) => {
                            let cid = inner.btree.open_table_cursor(root, false);
                            if page_cnt >= root
                                && let Err(e) = inner.btree.cursor_first(cid)
                            {
                                lease_cursors.push(cid);
                                let _ = inner.finish_activity(
                                    hid,
                                    ActivityFinish::ReadAborted { lease_cursors },
                                    shared,
                                );
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            lease_cursors.push(cid);
                        }
                        LeaseCursorSpec::Index(root, orders) => {
                            let cid = inner.btree.open_index_cursor(root, orders, false);
                            if page_cnt >= root
                                && let Err(e) = inner.btree.cursor_first(cid)
                            {
                                lease_cursors.push(cid);
                                let _ = inner.finish_activity(
                                    hid,
                                    ActivityFinish::ReadAborted { lease_cursors },
                                    shared,
                                );
                                self.lifecycle = StatementLifecycle::Halted {
                                    last_error: Some(e.clone()),
                                };
                                return Err(e);
                            }
                            lease_cursors.push(cid);
                        }
                    }
                }

                let summary = inner.activity.summary_excluding(hid);
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                if prev_ts.is_none() {
                    inner.runtime.stmt_timestamp_ms =
                        Some(inner.btree.pager().vfs().current_unix_millis());
                }
                let exec_res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    execute_read_body(&mut ctx, &stmt_clone, &self.params)
                };
                inner.runtime.stmt_timestamp_ms = prev_ts;

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
                                });
                            Ok(StepResult::Row(Row::new(first_row)))
                        } else {
                            let fin_res = inner.finish_activity(
                                hid,
                                ActivityFinish::ReadDone { lease_cursors },
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
                            ActivityFinish::ReadAborted { lease_cursors },
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
                if schema
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("temp"))
                {
                    self.lifecycle = StatementLifecycle::Halted { last_error: None };
                    return Ok(StepResult::Done);
                }
                let mut remaining = parse_incremental_vacuum_limit(value.as_ref());

                let prev_tx_state = inner.btree.transaction_state();
                let opened_write_here = prev_tx_state != TransactionState::Write;
                let base_sp_depth = inner.savepoints.len();
                if opened_write_here {
                    if let Err(e) = inner.busy_policy.retry_btree(&mut inner.btree, |bt| {
                        bt.pager_mut()
                            .begin_write_with_savepoint_depth(base_sp_depth)
                    }) {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                } else if base_sp_depth > 0
                    && let Err(e) = inner.btree.pager_mut().ensure_subsavepoints(base_sp_depth)
                {
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                let pre_summary = inner.activity.summary_excluding(hid);
                let cleanup_unstarted_write = |inner: &mut ConnectionInner| {
                    if opened_write_here && pre_summary.other_writers == 0 {
                        let retain_read = prev_tx_state == TransactionState::Read
                            || pre_summary.other_readers > 0;
                        if let Err(rb_err) = inner.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            true,
                            retain_read,
                        ) {
                            let _ = inner.poison_with(rb_err);
                        }
                    }
                };

                let stmt_sp_slot = if !inner.is_autocommit() || pre_summary.other_writers > 0 {
                    let slot = inner.savepoints.len();
                    if let Err(e) = inner.btree.pager_mut().ensure_subsavepoints(slot + 1) {
                        cleanup_unstarted_write(inner);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    Some(slot)
                } else {
                    None
                };

                if !inner.is_autocommit() {
                    inner.in_user_btree_tx = true;
                }

                let abort_snapshot = WriteAbortSnapshot {
                    stmt_sp_slot,
                    fk_before: inner.deferred_fk_suspects.clone(),
                    rt_before: inner.runtime,
                    schema_before: inner.schema.clone(),
                    schema_gen_before: inner.schema_generation,
                    schema_changed_before: inner.schema_changed_in_tx,
                };

                inner.activity.start(hid, true, true);
                if let Err(e) = inner.btree.check_interrupted() {
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
                match inner.btree.incremental_vacuum_step() {
                    Ok(true) => {
                        self.lifecycle =
                            StatementLifecycle::Running(RunningExec::IncrementalVacuum {
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
                let prev_tx_state = inner.btree.transaction_state();
                let opened_write_here = prev_tx_state != TransactionState::Write;
                let base_sp_depth = inner.savepoints.len();
                if opened_write_here {
                    if let Err(e) = inner.busy_policy.retry_btree(&mut inner.btree, |bt| {
                        bt.pager_mut()
                            .begin_write_with_savepoint_depth(base_sp_depth)
                    }) {
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                } else if base_sp_depth > 0
                    && let Err(e) = inner.btree.pager_mut().ensure_subsavepoints(base_sp_depth)
                {
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                let pre_summary = inner.activity.summary_excluding(hid);
                let cleanup_unstarted_write = |inner: &mut ConnectionInner| {
                    if opened_write_here && pre_summary.other_writers == 0 {
                        let retain_read = prev_tx_state == TransactionState::Read
                            || pre_summary.other_readers > 0;
                        if let Err(rb_err) = inner.btree.rollback_with_trip(
                            extended::SQLITE_ABORT_ROLLBACK,
                            true,
                            retain_read,
                        ) {
                            let _ = inner.poison_with(rb_err);
                        }
                    }
                };

                if self.requires_schema
                    && let Err(e) = inner.refresh_schema_in_open_tx()
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
                }

                // Open an anonymous statement subsavepoint whenever an outer tx or another writer exists!
                let stmt_sp_slot = if !inner.is_autocommit() || pre_summary.other_writers > 0 {
                    let slot = inner.savepoints.len();
                    if let Err(e) = inner.btree.pager_mut().ensure_subsavepoints(slot + 1) {
                        cleanup_unstarted_write(inner);
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(e.clone()),
                        };
                        return Err(e);
                    }
                    Some(slot)
                } else {
                    None
                };

                if !inner.is_autocommit() {
                    inner.in_user_btree_tx = true;
                }

                let abort_snapshot = WriteAbortSnapshot {
                    stmt_sp_slot,
                    fk_before: inner.deferred_fk_suspects.clone(),
                    rt_before: inner.runtime,
                    schema_before: inner.schema.clone(),
                    schema_gen_before: inner.schema_generation,
                    schema_changed_before: inner.schema_changed_in_tx,
                };

                inner.activity.start(hid, true, true);
                let summary = inner.activity.summary_excluding(hid);

                if let Err(e) = inner.btree.check_interrupted() {
                    let final_err = inner.abort_write_statement(hid, &e, abort_snapshot, shared);
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(final_err.clone()),
                    };
                    return Err(final_err);
                }

                let stmt_clone = self.stmt.clone().expect("non-empty statement");
                let prev_ts = inner.runtime.stmt_timestamp_ms;
                if prev_ts.is_none() {
                    inner.runtime.stmt_timestamp_ms =
                        Some(inner.btree.pager().vfs().current_unix_millis());
                }
                let exec_res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    execute_write_body(&mut ctx, &stmt_clone, &self.params)
                };
                inner.runtime.stmt_timestamp_ms = prev_ts;

                match exec_res {
                    Ok(out) => {
                        if !out.column_names.is_empty() {
                            self.column_names = out.column_names;
                        }
                        self.compiled_schema_generation = inner.schema_generation;
                        self.compiled_cookie = inner.schema.schema_cookie;
                        self.compiled_had_pages = inner.schema.had_pages;

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
            RunningExec::Vdbe { vm } => {
                let step_res = {
                    let encoding = inner.btree.encoding();
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
                    vm.step(&mut inner.btree, &mut services)
                };
                match step_res {
                    Ok(VmStep::Row(vals)) => {
                        inner.runtime = vm.runtime_ctx;
                        inner.runtime.stmt_timestamp_ms = None;
                        inner.publish(shared);
                        Ok(StepResult::Row(Row::new(vals)))
                    }
                    Ok(VmStep::Done) => {
                        vm.close_cursors(&mut inner.btree);
                        inner.runtime = vm.runtime_ctx;
                        inner.runtime.stmt_timestamp_ms = None;
                        inner.publish(shared);
                        let fin_res = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadDone {
                                lease_cursors: Vec::new(),
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
                        vm.close_cursors(&mut inner.btree);
                        inner.runtime.stmt_timestamp_ms = None;
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: Vec::new(),
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
            } => {
                if let Err(e) = inner.btree.check_interrupted() {
                    let cursors = std::mem::take(lease_cursors);
                    let _ = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadAborted {
                            lease_cursors: cursors,
                        },
                        shared,
                    );
                    self.lifecycle = StatementLifecycle::Halted {
                        last_error: Some(e.clone()),
                    };
                    return Err(e);
                }

                for &cid in lease_cursors.iter() {
                    if let Some(&BtreeCursorState::Fault(code)) = inner.btree.cursor_state(cid) {
                        let msg = if code == extended::SQLITE_ABORT_ROLLBACK {
                            "abort due to ROLLBACK"
                        } else {
                            ErrorCode::from_raw_primary(code).as_str()
                        };
                        let err = SqliteError::with_extended(code, msg);
                        let cursors = std::mem::take(lease_cursors);
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: cursors,
                            },
                            shared,
                        );
                        self.lifecycle = StatementLifecycle::Halted {
                            last_error: Some(err.clone()),
                        };
                        return Err(err);
                    }
                    if let Err(e) = inner.btree.restore_cursor_position(cid) {
                        let cursors = std::mem::take(lease_cursors);
                        let _ = inner.finish_activity(
                            hid,
                            ActivityFinish::ReadAborted {
                                lease_cursors: cursors,
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
                    let fin_res = inner.finish_activity(
                        hid,
                        ActivityFinish::ReadDone {
                            lease_cursors: cursors,
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
                if let Err(e) = inner.btree.check_interrupted() {
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
                remaining,
                abort_snapshot,
            } => {
                if let Err(e) = inner.btree.check_interrupted() {
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
                match inner.btree.incremental_vacuum_step() {
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
                let summary = inner.activity.summary_excluding(hid);
                let rel_opt = release_savepoint.clone();
                let res = {
                    let mut publish_fn = |rt: &VmRuntimeContext, exp: bool, imp: bool, rb: u64| {
                        shared.publish_mirrors(rt, exp, imp, rb);
                    };
                    let mut ctx = ExecContext {
                        btree: &mut inner.btree,
                        schema: &mut inner.schema,
                        runtime: &mut inner.runtime,
                        interrupt_flag: &inner.active_interrupt_flag,
                        functions: &inner.functions,
                        collations: &inner.collations,
                        progress: &mut inner.progress,
                        strict_subtype: inner.strict_subtype,
                        explicit_tx: &mut inner.explicit_tx,
                        implicit_savepoint_tx: &mut inner.implicit_savepoint_tx,
                        in_user_btree_tx: &mut inner.in_user_btree_tx,
                        savepoints: &mut inner.savepoints,
                        schema_generation: &mut inner.schema_generation,
                        schema_changed_in_tx: &mut inner.schema_changed_in_tx,
                        activity: summary,
                        foreign_keys: &mut inner.foreign_keys,
                        count_changes: &mut inner.count_changes,
                        wal_autocheckpoint_threshold: &mut inner.wal_autocheckpoint_threshold,
                        default_locking_mode: &mut inner.default_locking_mode,
                        next_auto_vacuum: &mut inner.next_auto_vacuum,
                        next_page_size: &mut inner.next_page_size,
                        writable_schema: &mut inner.writable_schema,
                        working_dir: &inner.working_dir,
                        busy_policy: &mut inner.busy_policy,
                        rollback_count: &mut inner.rollback_count,
                        deferred_fk_suspects: &mut inner.deferred_fk_suspects,
                        publish_mirrors: &mut publish_fn,
                    };
                    match rel_opt {
                        Some(ref sp) => release_savepoint_op(&mut ctx, sp),
                        None => commit_transaction(&mut ctx),
                    }
                };

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
            StatementLifecycle::Running(run) => Some(run),
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
