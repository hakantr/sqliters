//! SQLite `testfixture` + `tester.tcl` host environment bound to `sqliters_core::Connection`
//! and real owned `sqliters_core::Statement` handles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters_core::{
    CloseError, Connection, ConnectionOpenOptions, PrepareMode, Row, Statement, StepResult,
    WalCheckpointMode,
};
use sqliters_types::{ErrorCode, LockGeometry, SqliteError, Value, extended, format_sqlite_real};
use sqliters_vfs::OpenFlags;

use crate::expr::{eval_expr, parse_tcl_bool, parse_tcl_float, parse_tcl_int};
use crate::interp::{TclControl, TclInterp, TclResult};
use crate::list::{format_list, fpnum_compare, md5_hex, parse_list};
use crate::regex::{Regex, glob_match};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Default `testfixture` page size (`SQLITE_DEFAULT_PAGE_SIZE=1024` in `sqlite/main.mk`).
pub const TESTFIXTURE_DEFAULT_PAGE_SIZE: u32 = 1024;

#[derive(Debug, Clone)]
pub struct SubtestFailure {
    pub name: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, Default)]
pub struct TestFileReport {
    pub file_name: String,
    pub passed: usize,
    pub failed: Vec<SubtestFailure>,
    pub script_error: Option<String>,
    pub finished: bool,
    pub skipped_zero_subtests: bool,
    pub unknown_capabilities: Vec<String>,
}

impl TestFileReport {
    pub fn is_ok(&self) -> bool {
        self.failed.is_empty() && self.script_error.is_none() && self.passed > 0
    }

    pub fn total_cases(&self) -> usize {
        self.passed + self.failed.len()
    }
}

pub struct TempTestDir {
    pub path: PathBuf,
}

impl TempTestDir {
    pub fn new(prefix: &str) -> Self {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_tcl_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp test dir");
        Self { path: dir }
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Stable identity for an open database registration in [`SqliteTestHost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCmdTarget {
    Builtin(&'static str),
    Db(DbId),
}

struct DbHandle {
    conn: Rc<Connection>,
    nullvalue: String,
    extended_result_codes: bool,
    last_errcode: i32,
    last_extended_errcode: i32,
    last_errmsg: String,
    cmd_registered: bool,
    eval_refs: usize,
    tx_depth: usize,
}

enum DbSlot {
    Present(DbHandle),
    Closing {
        nullvalue: String,
        extended_result_codes: bool,
        cmd_registered: bool,
        eval_refs: usize,
        tx_depth: usize,
    },
}

struct PreparedStmtRecord {
    stmt: Statement,
    owner_db_id: DbId,
    current_row: Option<Row>,
    column_names: Vec<String>,
}

enum StmtSlot {
    Present(Box<PreparedStmtRecord>),
    CheckedOut { column_names: Vec<String> },
}

const DEFAULT_HOST_BUILTINS: &[(&str, &str)] = &[
    ("sqlite3", "sqlite3"),
    ("do_test", "do_test"),
    ("source", "source"),
    ("ifcapable", "ifcapable"),
    ("capable", "capable"),
    ("reset_db", "reset_db"),
    ("finish_test", "finish_test"),
    ("sqlite_complete", "sqlite_complete"),
    ("sqlite3_complete", "sqlite3_complete"),
    ("working_64bit_int", "working_64bit_int"),
    ("fpnum_compare", "fpnum_compare"),
    ("randstr", "randstr"),
    ("hexio_read", "hexio_read"),
    ("hexio_write", "hexio_write"),
    ("hexio_get_int", "hexio_get_int"),
    ("hexio_render_int32", "hexio_render_int32"),
    ("hexio_render_int16", "hexio_render_int16"),
    ("md5", "md5"),
    ("md5sum", "md5sum"),
    ("strftime", "strftime"),
    ("md5file", "md5file"),
    ("sqlite3_connection_pointer", "sqlite3_connection_pointer"),
    ("sqlite3_get_autocommit", "sqlite3_get_autocommit"),
    ("sqlite3_txn_state", "sqlite3_txn_state"),
    ("sqlite3_errcode", "sqlite3_errcode"),
    ("sqlite3_extended_errcode", "sqlite3_extended_errcode"),
    (
        "sqlite3_extended_result_codes",
        "sqlite3_extended_result_codes",
    ),
    ("sqlite3_errmsg", "sqlite3_errmsg"),
    ("sqlite3_changes", "sqlite3_changes"),
    ("sqlite3_total_changes", "sqlite3_total_changes"),
    ("sqlite3_last_insert_rowid", "sqlite3_last_insert_rowid"),
    ("sqlite3_interrupt", "sqlite3_interrupt"),
    ("sqlite3_is_interrupted", "sqlite3_is_interrupted"),
    ("sqlite3_exec", "sqlite3_exec"),
    ("sqlite3_prepare", "sqlite3_prepare"),
    ("sqlite3_prepare_v2", "sqlite3_prepare_v2"),
    ("sqlite3_step", "sqlite3_step"),
    ("sqlite3_data_count", "sqlite3_data_count"),
    ("sqlite3_column_count", "sqlite3_column_count"),
    ("sqlite3_column_name", "sqlite3_column_name"),
    ("sqlite3_column_type", "sqlite3_column_type"),
    ("sqlite3_column_text", "sqlite3_column_text"),
    ("sqlite3_column_int", "sqlite3_column_int"),
    ("sqlite3_column_int64", "sqlite3_column_int64"),
    ("sqlite3_column_double", "sqlite3_column_double"),
    ("sqlite3_column_bytes", "sqlite3_column_bytes"),
    ("sqlite3_column_blob", "sqlite3_column_blob"),
    ("sqlite3_sql", "sqlite3_sql"),
    ("sqlite3_stmt_busy", "sqlite3_stmt_busy"),
    ("sqlite3_bind_int", "sqlite3_bind_int"),
    ("sqlite3_bind_int64", "sqlite3_bind_int64"),
    ("sqlite3_bind_double", "sqlite3_bind_double"),
    ("sqlite3_bind_null", "sqlite3_bind_null"),
    ("sqlite3_bind_text", "sqlite3_bind_text"),
    ("sqlite3_bind_blob", "sqlite3_bind_blob"),
    ("sqlite3_bind_zeroblob", "sqlite3_bind_zeroblob"),
    (
        "sqlite3_bind_parameter_count",
        "sqlite3_bind_parameter_count",
    ),
    ("sqlite3_bind_parameter_name", "sqlite3_bind_parameter_name"),
    (
        "sqlite3_bind_parameter_index",
        "sqlite3_bind_parameter_index",
    ),
    ("sqlite3_clear_bindings", "sqlite3_clear_bindings"),
    ("sqlite3_reset", "sqlite3_reset"),
    ("sqlite3_finalize", "sqlite3_finalize"),
    (
        "sqlite3_test_control_pending_byte",
        "sqlite3_test_control_pending_byte",
    ),
    ("test_control_pending_byte", "test_control_pending_byte"),
    ("sqlite3_wal_checkpoint", "sqlite3_wal_checkpoint"),
    ("sqlite3_wal_checkpoint_v2", "sqlite3_wal_checkpoint_v2"),
    ("sqlite3_wal_autocheckpoint", "sqlite3_wal_autocheckpoint"),
    ("file_control_persist_wal", "file_control_persist_wal"),
    ("sqlite3_db_config", "noop_zero"),
    ("sqlite3_db_config_lookaside", "noop_zero"),
    ("sqlite3_test_control", "noop_zero"),
    ("sqlite3_limit", "noop_zero"),
    ("sqlite3_soft_heap_limit", "noop_zero"),
    ("sqlite3_soft_heap_limit64", "noop_zero"),
    ("sqlite3_hard_heap_limit64", "noop_zero"),
    ("sqlite3_release_memory", "noop_zero"),
    ("sqlite3_enable_shared_cache", "noop_zero"),
    ("sqlite3_config_memstatus", "noop_zero"),
    ("sqlite3_memdebug_settitle", "noop_zero"),
    ("sqlite3_shutdown", "noop_zero"),
    ("sqlite3_initialize", "noop_zero"),
    ("autoinstall_test_functions", "noop_zero"),
    ("install_malloc_faultsim", "noop_zero"),
    ("database_never_corrupt", "noop_zero"),
    ("database_may_be_corrupt", "noop_zero"),
    ("extra_schema_checks", "noop_zero"),
    ("reset_prng_state", "noop_zero"),
    ("add_alignment_test_functions", "noop_zero"),
    ("register_dbstat_vtab", "noop_zero"),
    ("register_echo_module", "noop_zero"),
    ("register_tcl_module", "noop_zero"),
    ("load_static_extension", "noop_zero"),
    ("sqlite3_create_function", "noop_zero"),
    ("sqlite3_create_function_v2", "noop_zero"),
    ("sqlite3_create_collation_v2", "noop_zero"),
    ("sqlite3_rekey", "noop_zero"),
    ("sqlite3_key", "noop_zero"),
    ("optimization_control", "noop_zero"),
    ("breakpoint", "noop_zero"),
    ("clang_sanitize_address", "noop_zero"),
    ("atomic_batch_write", "noop_zero"),
    ("tcl_variable_type", "noop_zero"),
    ("translate_selftest", "noop_zero"),
    ("testvfs", "noop_zero"),
    ("sqlite3_db_status", "noop_zero"),
];

/// Explicit Phase 1.2 capability table (`sqlite_options` / `ifcapable`).
///
/// Only genuinely implemented and verified Phase 1.1 + Phase 1.2 features are mapped to `true`.
/// Every unverified, future-phase, or out-of-scope capability is explicitly `false`,
/// and any unknown capability name evaluates to `None` (`false` + recorded in diagnostics).
pub const PHASE1_CAPABILITY_TABLE: &[(&str, bool)] = &[
    // Verified Phase 1.1 + Phase 1.2 SQL, B-tree, pager, WAL, VFS, and lifecycle capabilities:
    ("altertable", true),
    ("autoinc", true),
    ("autoreset", true),
    ("between_opt", true),
    ("bloblit", true),
    ("bounded_vars", true),
    ("cast", true),
    ("check", true),
    ("complete", true),
    ("compound", true),
    ("conflict", true),
    ("cte", true),
    ("curdir", true),
    ("datetime", true),
    ("default_cksum", true),
    ("descidx", true),
    ("diskio", true),
    ("explain", true),
    ("expr_depth", true),
    ("floatingpoint", true),
    ("foreignkey", true),
    ("generated", true),
    ("integrityck", true),
    ("like_match_blobs", true),
    ("like_opt", true),
    ("localtime", true),
    ("memorydb", true),
    ("mergesort", true),
    ("or_opt", true),
    ("pager_pragmas", true),
    ("pragma", true),
    ("printf", true),
    ("reindex", true),
    ("returning", true),
    ("rowvalue", true),
    ("savepoint", true),
    ("schema_pragmas", true),
    ("schema_version", true),
    ("subquery", true),
    ("tclvar", true),
    ("threadsafe", true),
    ("truncate_opt", true),
    ("vacuum", true),
    ("wal", true),
    ("xfer_opt", true),
    // Explicitly disabled (future phases or out of scope):
    ("analyze", false),
    ("atomicwrite", false),
    ("attach", false),
    ("auth", false),
    ("autovacuum", false),
    ("bitvec_test", false),
    ("case_sensitive_like", false),
    ("casesensitivelike", false),
    ("codec", false),
    ("columnmetadata", false),
    ("default_autovacuum", false),
    ("deprecated", false),
    ("empty_result_callbacks", false),
    ("fts3", false),
    ("fts5", false),
    ("get_table", false),
    ("icu", false),
    ("incr_vacuum", false),
    ("incrblob", false),
    ("json", false),
    ("json1", false),
    ("load_ext", false),
    ("lock_proxy_pragmas", false),
    ("lookaside", false),
    ("mem3", false),
    ("mem5", false),
    ("memdebug", false),
    ("memorymanage", false),
    ("offset_sql_func", false),
    ("prefer_proxy_locking", false),
    ("progress", false),
    ("proxy_locking", false),
    ("rtree", false),
    ("shared_cache", false),
    ("stat4", false),
    ("tempdb", false),
    ("trace", false),
    ("trigger", false),
    ("unlock_notify", false),
    ("update_delete_limit", false),
    ("upsert", false),
    ("utf16", false),
    ("view", false),
    ("vtab", false),
    ("windowfunc", false),
];

/// Looks up a capability name in the explicit capability table.
pub fn phase1_capability(name: &str) -> Option<bool> {
    PHASE1_CAPABILITY_TABLE
        .iter()
        .find(|&&(k, _)| k == name)
        .map(|&(_, v)| v)
}

pub struct SqliteTestHost {
    cmd_table: HashMap<String, HostCmdTarget>,
    dbs: HashMap<DbId, DbSlot>,
    ptr_to_db: HashMap<String, DbId>,
    next_db_id: u64,
    stmts: HashMap<String, StmtSlot>,
    next_stmt_id: u64,
    active_evals: usize,
    default_page_size: u32,
    lock_geometry: LockGeometry,
    pub report: TestFileReport,
    pub sqlite_test_dir: PathBuf,
    pub source_roots: Vec<PathBuf>,
    prng_state: u64,
}

pub type SharedHost = Rc<RefCell<SqliteTestHost>>;

impl SqliteTestHost {
    pub fn new_shared(file_name: &str, sqlite_test_dir: PathBuf) -> SharedHost {
        let roots = vec![sqlite_test_dir.clone()];
        Self::new_shared_with_roots(file_name, sqlite_test_dir, roots)
    }

    pub fn new_shared_with_roots(
        file_name: &str,
        sqlite_test_dir: PathBuf,
        source_roots: Vec<PathBuf>,
    ) -> SharedHost {
        let mut cmd_table = HashMap::with_capacity(DEFAULT_HOST_BUILTINS.len() + 4);
        for &(name, target) in DEFAULT_HOST_BUILTINS {
            cmd_table.insert(name.to_string(), HostCmdTarget::Builtin(target));
        }

        Rc::new(RefCell::new(Self {
            cmd_table,
            dbs: HashMap::new(),
            ptr_to_db: HashMap::new(),
            next_db_id: 1,
            stmts: HashMap::new(),
            next_stmt_id: 1,
            active_evals: 0,
            default_page_size: TESTFIXTURE_DEFAULT_PAGE_SIZE,
            lock_geometry: LockGeometry::test_64k(),
            report: TestFileReport {
                file_name: file_name.to_string(),
                passed: 0,
                failed: Vec::new(),
                script_error: None,
                finished: false,
                skipped_zero_subtests: false,
                unknown_capabilities: Vec::new(),
            },
            sqlite_test_dir,
            source_roots,
            prng_state: 0x1234_5678_9abc_def0,
        }))
    }

    pub fn has_db(&self, name: &str) -> bool {
        matches!(self.cmd_table.get(name), Some(HostCmdTarget::Db(_)))
    }

    pub fn open_db_count(&self) -> usize {
        self.dbs.len()
    }

    pub fn open_stmt_count(&self) -> usize {
        self.stmts.len()
    }

    pub fn default_page_size(&self) -> u32 {
        self.default_page_size
    }

    pub fn lock_geometry(&self) -> LockGeometry {
        self.lock_geometry
    }

    fn open_options(&self) -> ConnectionOpenOptions {
        ConnectionOpenOptions {
            default_page_size: self.default_page_size,
            lock_geometry: self.lock_geometry,
            temp_store_memory: false,
        }
    }

    fn resolve_db_id(&self, raw_token: &str) -> Option<DbId> {
        let clean = raw_token.strip_prefix("::").unwrap_or(raw_token);
        if let Some(&HostCmdTarget::Db(id)) = self.cmd_table.get(clean) {
            return Some(id);
        }
        self.ptr_to_db.get(clean).copied()
    }

    fn register_new_db(&mut self, cmd_name: String, conn: Connection) -> (DbId, String) {
        let db_id = DbId(self.next_db_id);
        self.next_db_id += 1;
        let ptr_token = format!("0x{:x}", db_id.0);
        self.ptr_to_db.insert(ptr_token.clone(), db_id);
        self.dbs.insert(
            db_id,
            DbSlot::Present(DbHandle {
                conn: Rc::new(conn),
                nullvalue: String::new(),
                extended_result_codes: false,
                last_errcode: 0,
                last_extended_errcode: 0,
                last_errmsg: "not an error".to_string(),
                cmd_registered: true,
                eval_refs: 0,
                tx_depth: 0,
            }),
        );
        self.cmd_table.insert(cmd_name, HostCmdTarget::Db(db_id));
        (db_id, ptr_token)
    }

    fn set_db_error(&mut self, db_id: DbId, code: i32, extended_code: i32, msg: String) -> i32 {
        let mut effective = code;
        if let Some(DbSlot::Present(h)) = self.dbs.get_mut(&db_id) {
            let mask = if h.extended_result_codes {
                0x7fff_ffff
            } else {
                0xff
            };
            h.last_errcode = extended_code & 0xff;
            h.last_extended_errcode = extended_code;
            h.last_errmsg = msg;
            effective = extended_code & mask;
        }
        effective
    }

    fn set_db_step_status(&mut self, db_id: DbId, status_code: i32) {
        if let Some(DbSlot::Present(h)) = self.dbs.get_mut(&db_id) {
            h.last_errcode = status_code;
            h.last_extended_errcode = status_code;
            h.last_errmsg = ErrorCode::from_raw_primary(status_code)
                .as_str()
                .to_string();
        }
    }

    fn clear_db_error(&mut self, db_id: DbId) {
        if let Some(DbSlot::Present(h)) = self.dbs.get_mut(&db_id) {
            h.last_errcode = 0;
            h.last_extended_errcode = 0;
            h.last_errmsg = "not an error".to_string();
        }
    }

    fn record_unknown_capability(&mut self, cap: &str) {
        if !self.report.unknown_capabilities.iter().any(|c| c == cap) {
            self.report.unknown_capabilities.push(cap.to_string());
        }
    }

    fn next_rand(&mut self) -> u64 {
        let mut x = self.prng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.prng_state = x;
        x
    }
}

pub(crate) fn has_host_cmd(host: &SharedHost, name: &str) -> bool {
    host.borrow().cmd_table.contains_key(name)
}

pub(crate) fn list_host_cmds(host: &SharedHost) -> Vec<String> {
    host.borrow().cmd_table.keys().cloned().collect()
}

pub(crate) fn delete_host_cmd_if_exists(host: &SharedHost, name: &str) {
    let target = host.borrow_mut().cmd_table.remove(name);
    if let Some(HostCmdTarget::Db(db_id)) = target {
        invoke_db_delete_hook(host, db_id);
    }
}

pub(crate) fn rename_or_delete_host_cmd(host: &SharedHost, old: &str, new: &str) -> TclResult<()> {
    let Some(target) = host.borrow_mut().cmd_table.remove(old) else {
        return Err(TclControl::Error(format!(
            "can't rename \"{old}\": command doesn't exist"
        )));
    };
    if new.is_empty() {
        if let HostCmdTarget::Db(db_id) = target {
            invoke_db_delete_hook(host, db_id);
        }
    } else {
        host.borrow_mut().cmd_table.insert(new.to_string(), target);
    }
    Ok(())
}

/// Equivalent of `DbDeleteCmd` / `delDatabaseRef` in `tclsqlite.c`:
/// detaches the Tcl command registration once without prematurely destroying
/// an in-flight `db_eval` or externally-owned `Statement` handles.
fn invoke_db_delete_hook(host: &SharedHost, db_id: DbId) {
    let dropped_slot = {
        let mut h = host.borrow_mut();
        let should_drop = match h.dbs.get_mut(&db_id) {
            Some(DbSlot::Present(db)) => {
                db.cmd_registered = false;
                db.eval_refs == 0
            }
            Some(DbSlot::Closing {
                cmd_registered,
                eval_refs,
                ..
            }) => {
                *cmd_registered = false;
                *eval_refs == 0
            }
            None => false,
        };
        if should_drop {
            h.ptr_to_db.retain(|_, &mut id| id != db_id);
            h.dbs.remove(&db_id)
        } else {
            None
        }
    };
    drop(dropped_slot);
}

struct DbEvalScopeGuard {
    host: SharedHost,
    db_id: DbId,
}

impl DbEvalScopeGuard {
    fn enter(host: &SharedHost, db_id: DbId) -> Self {
        let mut h = host.borrow_mut();
        h.active_evals += 1;
        match h.dbs.get_mut(&db_id) {
            Some(DbSlot::Present(db)) => db.eval_refs += 1,
            Some(DbSlot::Closing { eval_refs, .. }) => *eval_refs += 1,
            None => {}
        }
        Self {
            host: Rc::clone(host),
            db_id,
        }
    }
}

impl Drop for DbEvalScopeGuard {
    fn drop(&mut self) {
        let dropped_slot = {
            let mut h = self.host.borrow_mut();
            h.active_evals = h.active_evals.saturating_sub(1);
            let should_remove = match h.dbs.get_mut(&self.db_id) {
                Some(DbSlot::Present(db)) => {
                    db.eval_refs = db.eval_refs.saturating_sub(1);
                    !db.cmd_registered && db.eval_refs == 0
                }
                Some(DbSlot::Closing {
                    cmd_registered,
                    eval_refs,
                    ..
                }) => {
                    *eval_refs = eval_refs.saturating_sub(1);
                    !*cmd_registered && *eval_refs == 0
                }
                None => false,
            };
            if should_remove {
                h.ptr_to_db.retain(|_, &mut id| id != self.db_id);
                h.dbs.remove(&self.db_id)
            } else {
                None
            }
        };
        drop(dropped_slot);
    }
}

pub(crate) fn get_pending_byte_str(host: &SharedHost) -> String {
    host.borrow()
        .lock_geometry
        .pending_byte_offset()
        .to_string()
}

pub(crate) fn set_pending_byte_str(host: &SharedHost, raw: &str) -> Result<u64, String> {
    let val =
        parse_tcl_int(raw.trim()).ok_or_else(|| format!("expected integer but got \"{raw}\""))?;
    if val <= 0 {
        return Err(format!("invalid pending byte offset: {raw}"));
    }
    let new_geom = LockGeometry::try_from_pending_byte(val as u64).map_err(|e| e.to_string())?;

    let mut h = host.borrow_mut();
    let old = h.lock_geometry.pending_byte_offset();
    if new_geom.pending_byte_offset() == old {
        return Ok(old);
    }
    if !h.dbs.is_empty() || !h.stmts.is_empty() || h.active_evals > 0 {
        return Err(
            "cannot change pending byte while database or statement handles are open".to_string(),
        );
    }
    h.lock_geometry = new_geom;
    Ok(old)
}

pub fn bootstrap_tester(
    interp: &mut TclInterp,
    host: &SharedHost,
    temp_dir: &Path,
) -> TclResult<()> {
    let (test_dir_str, argv0_str, open_opts) = {
        let h = host.borrow();
        let script_file = if h.report.file_name.ends_with(".test") {
            h.report.file_name.clone()
        } else {
            format!("{}.test", h.report.file_name)
        };
        (
            h.sqlite_test_dir.to_string_lossy().into_owned(),
            h.sqlite_test_dir
                .join(script_file)
                .to_string_lossy()
                .into_owned(),
            h.open_options(),
        )
    };
    let _ = interp.set_var("testdir", test_dir_str);
    let _ = interp.set_var("argv0", argv0_str);
    let _ = interp.set_var("argv", String::new());
    let _ = interp.set_var("AUTOVACUUM", "0".to_string());
    let _ = interp.set_var("sqlite_open_file_count", "0".to_string());
    let _ = interp.set_var("sqlite_search_count", "0".to_string());
    let _ = interp.set_var("sqlite_like_count", "0".to_string());
    let _ = interp.set_var("sqlite_sort_count", "0".to_string());
    let _ = interp.set_var("sqlite_xferopt_count", "0".to_string());
    let _ = interp.set_var("sqlite_opentemp_count", "0".to_string());
    let _ = interp.set_var("sqlite_current_time", "0".to_string());
    let _ = interp.set_var("tcl_precision", "15".to_string());
    let _ = interp.set_var("bitmask_size", "64".to_string());
    let _ = interp.set_var("SQLITE_MAX_EXPR_DEPTH", "1000".to_string());
    let _ = interp.set_var("SQLITE_MAX_VARIABLE_NUMBER", "999".to_string());
    let _ = interp.set_var("SQLITE_MAX_COLUMN", "2000".to_string());
    let _ = interp.set_var("SQLITE_MAX_COMPOUND_SELECT", "500".to_string());
    let _ = interp.set_var("SQLITE_DEFAULT_FILE_FORMAT", "4".to_string());
    interp.set_array_entry("cmdlinearg", "soft-heap-limit", "0".to_string());
    interp.set_array_entry("cmdlinearg", "hard-heap-limit", "0".to_string());
    interp.set_array_entry("cmdlinearg", "maxerror", "1000".to_string());
    interp.set_array_entry("cmdlinearg", "verbose", "0".to_string());
    interp.set_array_entry("cmdlinearg", "testdir", String::new());

    for &(cap, enabled) in PHASE1_CAPABILITY_TABLE {
        interp.set_array_entry(
            "sqlite_options",
            cap,
            if enabled { "1" } else { "0" }.to_string(),
        );
    }

    let db_path = temp_dir.join("test.db");
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open_with_options(
        &db_path,
        OpenFlags::READWRITE | OpenFlags::CREATE,
        open_opts,
    )
    .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?;

    let (_, ptr_token) = host.borrow_mut().register_new_db("db".to_string(), conn);
    let _ = interp.set_var("DB", ptr_token);

    interp.eval(TESTER_TCL_PRELUDE)?;
    Ok(())
}

/// Dispatches SQLite `testfixture` / `tester.tcl` host commands in a fully re-entrant way
/// (never holding a `RefCell` borrow across `interp.eval(...)` or core engine calls).
pub fn invoke_host_cmd(
    host: &SharedHost,
    interp: &mut TclInterp,
    cmd: &str,
    args: &[String],
) -> Option<TclResult<String>> {
    let target = host.borrow().cmd_table.get(cmd).copied()?;
    match target {
        HostCmdTarget::Db(db_id) => Some(dispatch_db_method(host, interp, cmd, db_id, args)),
        HostCmdTarget::Builtin(builtin) => {
            Some(dispatch_host_builtin(host, interp, cmd, builtin, args))
        }
    }
}

fn dispatch_host_builtin(
    host: &SharedHost,
    interp: &mut TclInterp,
    invoked_name: &str,
    builtin: &'static str,
    args: &[String],
) -> TclResult<String> {
    match builtin {
        "sqlite3" => open_db_cmd(host, interp, args),
        "do_test" => cmd_do_test(host, interp, args),
        "source" => cmd_source(host, interp, args),
        "ifcapable" => cmd_ifcapable(host, interp, args),
        "capable" => eval_capable_expr(host, interp, args.first().map_or("1", String::as_str))
            .map(|b| if b { "1" } else { "0" }.to_string()),
        "reset_db" => cmd_reset_db(host, interp),
        "finish_test" => {
            let mut h = host.borrow_mut();
            h.report.finished = true;
            if h.report.passed == 0 && h.report.failed.is_empty() {
                h.report.skipped_zero_subtests = true;
            }
            Err(TclControl::FinishTest)
        }
        "sqlite_complete" | "sqlite3_complete" => {
            let sql = args.first().map_or("", String::as_str);
            Ok(if sqliters_syntax::is_complete(sql) {
                "1"
            } else {
                "0"
            }
            .to_string())
        }
        "working_64bit_int" => Ok("1".to_string()),
        "fpnum_compare" => {
            let a = args.first().map_or("", String::as_str);
            let b = args.get(1).map_or("", String::as_str);
            Ok(if fpnum_compare(a, b) { "1" } else { "0" }.to_string())
        }
        "randstr" => cmd_randstr(host, args),
        "hexio_read" => cmd_hexio_read(interp, args),
        "hexio_write" => cmd_hexio_write(interp, args),
        "hexio_get_int" => {
            let hex = args.first().map_or("0", String::as_str);
            let val = i64::from_str_radix(hex.trim(), 16).unwrap_or(0);
            Ok(val.to_string())
        }
        "hexio_render_int32" => {
            let v = args
                .first()
                .and_then(|s| parse_tcl_int(s.trim()))
                .unwrap_or(0) as u32;
            Ok(format!("{v:08X}"))
        }
        "hexio_render_int16" => {
            let v = args
                .first()
                .and_then(|s| parse_tcl_int(s.trim()))
                .unwrap_or(0) as u16;
            Ok(format!("{v:04X}"))
        }
        "md5" => {
            let s = args.first().map_or("", String::as_str);
            Ok(md5_hex(s.as_bytes()))
        }
        "md5sum" => {
            let mut combined = String::new();
            let db_tok = args.first().map_or("db", String::as_str);
            if let Some(db_id) = host.borrow().resolve_db_id(db_tok) {
                for tbl in &args[1..] {
                    let sql = format!("SELECT * FROM \"{tbl}\"");
                    if let Ok(vals) = run_sql_flat(host, interp, db_id, &sql) {
                        for v in vals {
                            combined.push_str(&v);
                            combined.push('\n');
                        }
                    }
                }
            }
            Ok(md5_hex(combined.as_bytes()))
        }
        "strftime" => {
            let fmt = args.first().map_or("%Y-%m-%d %H:%M:%S", String::as_str);
            let sec = args.get(1).map_or("0", String::as_str);
            let sql = format!(
                "SELECT strftime('{}', {}, 'unixepoch')",
                fmt.replace('\'', "''"),
                parse_tcl_int(sec).unwrap_or(0)
            );
            let opts = host.borrow().open_options();
            if let Ok(conn) = Connection::open_in_memory_with_options(opts)
                && let Ok(Some(row)) = conn.query_row(&sql, &[])
            {
                return Ok(value_to_tcl_str(&row[0], ""));
            }
            Ok(String::new())
        }
        "md5file" => {
            let p = interp.resolve_path(args.first().map_or("", String::as_str));
            let data = fs::read(p).unwrap_or_default();
            Ok(md5_hex(&data))
        }
        "sqlite3_connection_pointer" => {
            let dbn = args.first().ok_or_else(|| {
                TclControl::Error(
                    "wrong # args: should be \"sqlite3_connection_pointer DB\"".to_string(),
                )
            })?;
            let db_id = host
                .borrow()
                .resolve_db_id(dbn)
                .ok_or_else(|| TclControl::Error(format!("not a database: {dbn}")))?;
            Ok(format!("0x{:x}", db_id.0))
        }
        "sqlite3_get_autocommit" => {
            let dbn = args.first().map_or("db", String::as_str);
            let ac = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(i32::from(db.conn.is_autocommit())),
                        _ => None,
                    })
                    .unwrap_or(1)
            };
            Ok(ac.to_string())
        }
        "sqlite3_txn_state" => {
            let dbn = args.first().map_or("db", String::as_str);
            let st = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => {
                            Some(if db.conn.is_autocommit() { 0 } else { 2 })
                        }
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(st.to_string())
        }
        "sqlite3_errcode" => {
            let dbn = args.first().map_or("db", String::as_str);
            let code = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => {
                            let mask = if db.extended_result_codes {
                                0x7fff_ffff
                            } else {
                                0xff
                            };
                            Some(db.last_extended_errcode & mask)
                        }
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(sqlite_err_name(code).to_string())
        }
        "sqlite3_extended_errcode" => {
            let dbn = args.first().map_or("db", String::as_str);
            let code = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(db.last_extended_errcode),
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(sqlite_err_name(code).to_string())
        }
        "sqlite3_extended_result_codes" => {
            if args.len() != 2 {
                return Err(TclControl::Error(
                    "wrong # args: should be \"sqlite3_extended_result_codes DB ONOFF\""
                        .to_string(),
                ));
            }
            let onoff = parse_tcl_bool(&args[1]).map_err(TclControl::Error)?;
            let mut h = host.borrow_mut();
            let db_id = h
                .resolve_db_id(&args[0])
                .ok_or_else(|| TclControl::Error(format!("not a database: {}", args[0])))?;
            if let Some(DbSlot::Present(db)) = h.dbs.get_mut(&db_id) {
                db.extended_result_codes = onoff;
            }
            Ok(String::new())
        }
        "sqlite3_errmsg" => {
            let dbn = args.first().map_or("db", String::as_str);
            let msg = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(db.last_errmsg.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "not an error".to_string())
            };
            Ok(msg)
        }
        "sqlite3_changes" => {
            let dbn = args.first().map_or("db", String::as_str);
            let n = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(db.conn.changes()),
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(n.to_string())
        }
        "sqlite3_total_changes" => {
            let dbn = args.first().map_or("db", String::as_str);
            let n = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(db.conn.total_changes()),
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(n.to_string())
        }
        "sqlite3_last_insert_rowid" => {
            let dbn = args.first().map_or("db", String::as_str);
            let id_val = {
                let h = host.borrow();
                h.resolve_db_id(dbn)
                    .and_then(|id| match h.dbs.get(&id) {
                        Some(DbSlot::Present(db)) => Some(db.conn.last_insert_rowid()),
                        _ => None,
                    })
                    .unwrap_or(0)
            };
            Ok(id_val.to_string())
        }
        "sqlite3_interrupt" => {
            let dbn = args.first().ok_or_else(|| {
                TclControl::Error("wrong # args: should be \"sqlite3_interrupt DB\"".to_string())
            })?;
            let ih = {
                let h = host.borrow();
                let db_id = h
                    .resolve_db_id(dbn)
                    .ok_or_else(|| TclControl::Error(format!("not a database: {dbn}")))?;
                match h.dbs.get(&db_id) {
                    Some(DbSlot::Present(db)) => db.conn.interrupt_handle(),
                    _ => return Err(TclControl::Error(format!("not a database: {dbn}"))),
                }
            };
            ih.interrupt();
            Ok(String::new())
        }
        "sqlite3_is_interrupted" => {
            let dbn = args.first().ok_or_else(|| {
                TclControl::Error(
                    "wrong # args: should be \"sqlite3_is_interrupted DB\"".to_string(),
                )
            })?;
            let interrupted = {
                let h = host.borrow();
                let db_id = h
                    .resolve_db_id(dbn)
                    .ok_or_else(|| TclControl::Error(format!("not a database: {dbn}")))?;
                match h.dbs.get(&db_id) {
                    Some(DbSlot::Present(db)) => db.conn.interrupt_handle().is_interrupted(),
                    _ => return Err(TclControl::Error(format!("not a database: {dbn}"))),
                }
            };
            Ok(if interrupted { "1" } else { "0" }.to_string())
        }
        "sqlite3_exec" => {
            let dbn = args.first().map_or("db", String::as_str);
            let sql = args.get(1).map_or("", String::as_str);
            let Some(db_id) = host.borrow().resolve_db_id(dbn) else {
                return Err(TclControl::Error(format!("not a database: {dbn}")));
            };
            match run_sql_exec_with_headers(host, interp, db_id, sql) {
                Ok(flat) => Ok(format_list(&["0".to_string(), format_list(&flat)])),
                Err(e) => Ok(format_list(&["1".to_string(), e.err_msg()])),
            }
        }
        "sqlite3_prepare" => cmd_prepare(host, interp, invoked_name, PrepareMode::V1, args),
        "sqlite3_prepare_v2" => cmd_prepare(host, interp, invoked_name, PrepareMode::V2, args),
        "sqlite3_step" => cmd_step(host, args),
        "sqlite3_column_count" => cmd_col_count(host, false, args),
        "sqlite3_data_count" => cmd_col_count(host, true, args),
        "sqlite3_column_name" => cmd_col_name(host, args),
        "sqlite3_column_type" => cmd_col_type(host, args),
        "sqlite3_column_text" => cmd_col_text(host, args),
        "sqlite3_column_int" => cmd_col_int(host, false, args),
        "sqlite3_column_int64" => cmd_col_int(host, true, args),
        "sqlite3_column_double" => cmd_col_double(host, args),
        "sqlite3_column_bytes" => cmd_col_bytes(host, args),
        "sqlite3_column_blob" => cmd_col_blob(host, args),
        "sqlite3_sql" => cmd_stmt_sql(host, args),
        "sqlite3_stmt_busy" => cmd_stmt_busy(host, args),
        "sqlite3_bind_int" => cmd_bind_scalar(host, "int", args),
        "sqlite3_bind_int64" => cmd_bind_scalar(host, "int64", args),
        "sqlite3_bind_double" => cmd_bind_scalar(host, "double", args),
        "sqlite3_bind_null" => cmd_bind_scalar(host, "null", args),
        "sqlite3_bind_text" => cmd_bind_scalar(host, "text", args),
        "sqlite3_bind_blob" => cmd_bind_scalar(host, "blob", args),
        "sqlite3_bind_zeroblob" => cmd_bind_scalar(host, "zeroblob", args),
        "sqlite3_bind_parameter_count" => cmd_bind_param_count(host, args),
        "sqlite3_bind_parameter_name" => cmd_bind_param_name(host, args),
        "sqlite3_bind_parameter_index" => cmd_bind_param_index(host, args),
        "sqlite3_clear_bindings" => cmd_clear_bindings(host, args),
        "sqlite3_reset" => cmd_stmt_reset(host, args),
        "sqlite3_finalize" => cmd_stmt_finalize(host, args),
        "sqlite3_test_control_pending_byte" | "test_control_pending_byte" => {
            cmd_pending_byte(host, invoked_name, args)
        }
        "sqlite3_wal_checkpoint" => cmd_wal_checkpoint(host, args),
        "sqlite3_wal_checkpoint_v2" => cmd_wal_checkpoint_v2(host, args),
        "sqlite3_wal_autocheckpoint" => cmd_wal_autocheckpoint(host, args),
        "file_control_persist_wal" => cmd_file_control_persist_wal(host, invoked_name, args),
        "noop_zero" => Ok("0".to_string()),
        _ => Err(TclControl::Error(format!(
            "invalid command name \"{invoked_name}\""
        ))),
    }
}

fn cmd_pending_byte(host: &SharedHost, cmd_name: &str, args: &[String]) -> TclResult<String> {
    match args.len() {
        0 => Ok(get_pending_byte_str(host)),
        1 => {
            let val = parse_tcl_int(args[0].trim()).ok_or_else(|| {
                TclControl::Error(format!("expected integer but got \"{}\"", args[0]))
            })?;
            if val == 0 {
                return Ok(get_pending_byte_str(host));
            }
            if val < 0 {
                return Err(TclControl::Error(format!(
                    "invalid pending byte offset: {}",
                    args[0]
                )));
            }
            set_pending_byte_str(host, &args[0])
                .map(|old| old.to_string())
                .map_err(TclControl::Error)
        }
        _ => Err(TclControl::Error(format!(
            "wrong # args: should be \"{cmd_name} ?PENDING-BYTE?\""
        ))),
    }
}

fn open_db_cmd(host: &SharedHost, interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    if args.is_empty() {
        return Err(TclControl::Error("wrong # args to sqlite3".to_string()));
    }
    if args[0] == "-version" {
        return Ok(sqliters_types::SQLITE_VERSION.to_string());
    }
    if args[0] == "-has-codec" {
        return Ok("0".to_string());
    }
    let handle_name = args[0].strip_prefix("::").unwrap_or(&args[0]).to_string();
    // Per `tclsqlite.c:DbMain` (`zArg = (objc>=3) ? Tcl_GetString(objv[2]) : ""`),
    // omitting the filename opens an isolated private temporary database (`""`), NOT `:memory:`.
    let raw_path = args.get(1).map_or("", String::as_str);

    let mut readonly = false;
    let mut create = true;
    let mut requested_vfs: Option<&str> = None;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "-readonly" => readonly = parse_tcl_bool(&args[i + 1]).unwrap_or(false),
            "-create" => create = parse_tcl_bool(&args[i + 1]).unwrap_or(true),
            "-vfs" => requested_vfs = Some(args[i + 1].as_str()),
            _ => {}
        }
        i += 2;
    }

    if let Some(vfs_name) = requested_vfs
        && !vfs_name.is_empty()
        && !is_supported_default_vfs_name(vfs_name)
    {
        return Err(TclControl::Error(format!("no such vfs: {vfs_name}")));
    }

    let opts = host.borrow().open_options();
    let conn = if raw_path.is_empty() {
        Connection::open_private_temporary_with_options(opts)
            .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?
    } else if raw_path == ":memory:" {
        Connection::open_in_memory_with_options(opts)
            .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?
    } else {
        let clean_path = raw_path.strip_prefix("file:").unwrap_or(raw_path);
        let clean_path = clean_path.split('?').next().unwrap_or(clean_path);
        let resolved = interp.resolve_path(clean_path);
        let flags = if readonly {
            OpenFlags::READONLY
        } else if create {
            OpenFlags::READWRITE | OpenFlags::CREATE
        } else {
            OpenFlags::READWRITE
        };
        Connection::open_with_options(resolved, flags, opts)
            .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?
    };

    // Per `tclsqlite.c:DbMain`, only after the new connection opens successfully do we
    // replace any existing Tcl command of the same name (running its delete hook once).
    interp.remove_non_host_cmd(&handle_name);
    delete_host_cmd_if_exists(host, &handle_name);

    let _ = host.borrow_mut().register_new_db(handle_name, conn);
    Ok(String::new())
}

fn is_supported_default_vfs_name(name: &str) -> bool {
    #[cfg(unix)]
    {
        name == "unix"
    }
    #[cfg(windows)]
    {
        name == "win32" || name == "win32-longpath"
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = name;
        false
    }
}

fn dispatch_db_method(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_name: &str,
    db_id: DbId,
    args: &[String],
) -> TclResult<String> {
    if args.is_empty() {
        return Err(TclControl::Error(format!("wrong # args to {db_name}")));
    }
    let sub = args[0].as_str();
    match sub {
        "close" => db_close_cmd(host, db_name, db_id),
        "nullvalue" | "null" => {
            let mut h = host.borrow_mut();
            let Some(DbSlot::Present(db)) = h.dbs.get_mut(&db_id) else {
                return Err(TclControl::Error(format!("unknown db {db_name}")));
            };
            if let Some(nv) = args.get(1) {
                db.nullvalue = nv.clone();
            }
            Ok(db.nullvalue.clone())
        }
        "changes" => {
            let h = host.borrow();
            let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                return Err(TclControl::Error(format!("unknown db {db_name}")));
            };
            Ok(db.conn.changes().to_string())
        }
        "total_changes" => {
            let h = host.borrow();
            let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                return Err(TclControl::Error(format!("unknown db {db_name}")));
            };
            Ok(db.conn.total_changes().to_string())
        }
        "last_insert_rowid" => {
            let h = host.borrow();
            let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                return Err(TclControl::Error(format!("unknown db {db_name}")));
            };
            Ok(db.conn.last_insert_rowid().to_string())
        }
        "errorcode" => {
            let h = host.borrow();
            let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                return Err(TclControl::Error(format!("unknown db {db_name}")));
            };
            let mask = if db.extended_result_codes {
                0x7fff_ffff
            } else {
                0xff
            };
            Ok((db.last_extended_errcode & mask).to_string())
        }
        "complete" => {
            let sql = args.get(1).map_or("", String::as_str);
            Ok(if sqliters_syntax::is_complete(sql) {
                "1"
            } else {
                "0"
            }
            .to_string())
        }
        "version" => Ok(sqliters_types::SQLITE_VERSION.to_string()),
        "eval" => db_eval(host, interp, db_id, &args[1..]),
        "one" | "onecolumn" => {
            let sql = args.get(1).map_or("", String::as_str);
            db_onecolumn(host, interp, db_id, sql)
        }
        "exists" => {
            let sql = args.get(1).map_or("", String::as_str);
            db_exists(host, interp, db_id, sql)
        }
        "interrupt" => {
            let ih = {
                let h = host.borrow();
                let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                    return Err(TclControl::Error(format!("unknown db {db_name}")));
                };
                db.conn.interrupt_handle()
            };
            ih.interrupt();
            Ok(String::new())
        }
        "transaction" => db_transaction_cmd(host, interp, db_name, db_id, args),
        "timeout" => {
            if let Some(ms_str) = args.get(1) {
                let ms = parse_tcl_int(ms_str.trim()).ok_or_else(|| {
                    TclControl::Error(format!("expected integer but got \"{ms_str}\""))
                })?;
                let conn = {
                    let h = host.borrow();
                    let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
                        return Err(TclControl::Error(format!("unknown db {db_name}")));
                    };
                    Rc::clone(&db.conn)
                };
                let timeout_ms = if ms <= 0 { 0u64 } else { ms as u64 };
                conn.busy_timeout(timeout_ms)
                    .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?;
            }
            Ok(String::new())
        }
        "status" => Ok("0".to_string()),
        "cache"
        | "config"
        | "busy"
        | "authorizer"
        | "auth"
        | "trace"
        | "profile"
        | "progress"
        | "collate"
        | "collation_needed"
        | "function"
        | "func"
        | "enable_load_extension"
        | "incrblob"
        | "restore"
        | "backup"
        | "wal_hook"
        | "update_hook"
        | "commit_hook"
        | "rollback_hook"
        | "rekey"
        | "key" => Ok(String::new()),
        other => Err(TclControl::Error(format!(
            "bad option \"{other}\" to db handle \"{db_name}\""
        ))),
    }
}

fn db_close_cmd(host: &SharedHost, db_name: &str, db_id: DbId) -> TclResult<String> {
    let conn = {
        let mut h = host.borrow_mut();
        let Some(slot) = h.dbs.get_mut(&db_id) else {
            return Err(TclControl::Error(format!(
                "invalid command name \"{db_name}\""
            )));
        };
        let DbSlot::Present(ref db) = *slot else {
            return Err(TclControl::Error(
                "unable to close due to unfinalized statements or unfinished backups".to_string(),
            ));
        };
        if Rc::strong_count(&db.conn) > 1 {
            let msg =
                "unable to close due to unfinalized statements or unfinished backups".to_string();
            h.set_db_error(
                db_id,
                ErrorCode::Busy as i32,
                ErrorCode::Busy as i32,
                msg.clone(),
            );
            return Err(TclControl::Error(msg));
        }
        let prev = std::mem::replace(
            slot,
            DbSlot::Closing {
                nullvalue: db.nullvalue.clone(),
                extended_result_codes: db.extended_result_codes,
                cmd_registered: db.cmd_registered,
                eval_refs: db.eval_refs,
                tx_depth: db.tx_depth,
            },
        );
        let DbSlot::Present(owned) = prev else {
            unreachable!()
        };
        Rc::try_unwrap(owned.conn).expect("verified unique Rc<Connection>")
    };

    match conn.try_close() {
        Ok(()) => {
            let mut h = host.borrow_mut();
            if matches!(h.cmd_table.get(db_name), Some(&HostCmdTarget::Db(id)) if id == db_id) {
                h.cmd_table.remove(db_name);
            }
            h.ptr_to_db.retain(|_, &mut id| id != db_id);
            h.dbs.remove(&db_id);
            Ok(String::new())
        }
        Err(CloseError { connection, error }) => {
            let errmsg = sqlite_err_text(&error);
            let restored_for_drop = {
                let mut h = host.borrow_mut();
                match h.dbs.remove(&db_id) {
                    Some(DbSlot::Closing {
                        nullvalue,
                        extended_result_codes,
                        cmd_registered,
                        eval_refs,
                        tx_depth,
                    }) if cmd_registered || eval_refs > 0 => {
                        h.dbs.insert(
                            db_id,
                            DbSlot::Present(DbHandle {
                                conn: Rc::new(connection),
                                nullvalue,
                                extended_result_codes,
                                last_errcode: error.code as i32,
                                last_extended_errcode: error.extended_code,
                                last_errmsg: errmsg.clone(),
                                cmd_registered,
                                eval_refs,
                                tx_depth,
                            }),
                        );
                        None
                    }
                    _ => {
                        h.ptr_to_db.retain(|_, &mut id| id != db_id);
                        Some(connection)
                    }
                }
            };
            drop(restored_for_drop);
            Err(TclControl::Error(errmsg))
        }
    }
}

fn db_transaction_cmd(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_name: &str,
    db_id: DbId,
    args: &[String],
) -> TclResult<String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(TclControl::Error(format!(
            "wrong # args: should be \"{db_name} transaction [TYPE] SCRIPT\""
        )));
    }

    let cur_tx_depth = {
        let h = host.borrow();
        let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
            return Err(TclControl::Error(format!(
                "invalid command name \"{db_name}\""
            )));
        };
        db.tx_depth
    };

    let mut z_begin = "SAVEPOINT _tcl_transaction";
    if cur_tx_depth == 0 && args.len() == 3 {
        match args[1].to_ascii_lowercase().as_str() {
            "deferred" | "-deferred" => {}
            "exclusive" | "-exclusive" => z_begin = "BEGIN EXCLUSIVE",
            "immediate" | "-immediate" => z_begin = "BEGIN IMMEDIATE",
            _ => {
                return Err(TclControl::Error(format!(
                    "bad transaction type \"{}\": must be deferred, exclusive, or immediate",
                    args[1]
                )));
            }
        }
    }
    let script = &args[args.len() - 1];

    exec_db_internal_sql(host, db_id, z_begin, true).map_err(TclControl::Error)?;

    {
        let mut h = host.borrow_mut();
        if let Some(DbSlot::Present(db)) = h.dbs.get_mut(&db_id) {
            db.tx_depth += 1;
        }
    }

    let _eval_guard = DbEvalScopeGuard::enter(host, db_id);
    let eval_res = interp.eval(script);

    let maybe_rem_depth = {
        let mut h = host.borrow_mut();
        match h.dbs.get_mut(&db_id) {
            Some(DbSlot::Present(db)) => {
                db.tx_depth = db.tx_depth.saturating_sub(1);
                Some(db.tx_depth)
            }
            Some(DbSlot::Closing { tx_depth, .. }) => {
                *tx_depth = tx_depth.saturating_sub(1);
                None
            }
            None => None,
        }
    };

    let Some(rem_depth) = maybe_rem_depth else {
        return eval_res;
    };

    let is_error = matches!(eval_res, Err(TclControl::Error(_) | TclControl::FinishTest));
    let z_end = match (is_error, rem_depth == 0) {
        (false, false) => "RELEASE _tcl_transaction",
        (false, true) => "COMMIT",
        (true, false) => "ROLLBACK TO _tcl_transaction ; RELEASE _tcl_transaction",
        (true, true) => "ROLLBACK",
    };

    if let Err(end_err_msg) = exec_db_internal_sql(host, db_id, z_end, !is_error) {
        let _ = exec_db_internal_sql(host, db_id, "ROLLBACK", false);
        if !is_error {
            return Err(TclControl::Error(end_err_msg));
        }
    }

    eval_res
}

fn exec_db_internal_sql(
    host: &SharedHost,
    db_id: DbId,
    sql: &str,
    record_err_on_db: bool,
) -> Result<(), String> {
    let (conn, _) = checkout_db_conn(host, db_id).map_err(|c| c.err_msg())?;
    let res = conn.execute_batch(sql);
    drop(conn);
    match res {
        Ok(()) => {
            if record_err_on_db {
                host.borrow_mut().clear_db_error(db_id);
            }
            Ok(())
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            if record_err_on_db {
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            }
            Err(msg)
        }
    }
}

fn checkout_db_conn(host: &SharedHost, db_id: DbId) -> TclResult<(Rc<Connection>, String)> {
    let h = host.borrow();
    match h.dbs.get(&db_id) {
        Some(DbSlot::Present(db)) => Ok((Rc::clone(&db.conn), db.nullvalue.clone())),
        _ => Err(TclControl::Error(
            "database connection is closed".to_string(),
        )),
    }
}

fn current_db_nullvalue(host: &SharedHost, db_id: DbId, fallback: &str) -> String {
    let h = host.borrow();
    match h.dbs.get(&db_id) {
        Some(DbSlot::Present(db)) => db.nullvalue.clone(),
        Some(DbSlot::Closing { nullvalue, .. }) => nullvalue.clone(),
        None => fallback.to_string(),
    }
}

fn bind_tcl_sql_parameters(stmt: &mut Statement, interp: &TclInterp) -> Result<(), SqliteError> {
    let named_list: Vec<(String, usize)> = stmt
        .named_parameters()
        .iter()
        .map(|(k, &v)| (k.clone(), v))
        .collect();
    for (raw_param, pidx) in named_list {
        if let Some(var_lookup) = raw_param
            .strip_prefix('$')
            .or_else(|| raw_param.strip_prefix(':'))
            .or_else(|| raw_param.strip_prefix('@'))
        {
            if let Ok(tcl_str) = interp.get_var(var_lookup) {
                let bound_val = if raw_param.starts_with('@') {
                    Value::Blob(tcl_str.into_bytes())
                } else {
                    tcl_string_to_sqlite_value(&tcl_str)
                };
                stmt.bind(pidx, bound_val)?;
            } else {
                stmt.bind(pidx, Value::Null)?;
            }
        }
    }
    Ok(())
}

/// Streaming `$db eval ?-withoutnulls? $sql ?varName? ?body?` matching `tclsqlite.c`
/// (`dbEvalInit` / `dbEvalStep` / `dbEvalFinalize`) with zero `RefCell` borrows held
/// across `interp.eval(body)` or core `prepare` / `step` / `finalize` calls.
fn db_eval(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    args: &[String],
) -> TclResult<String> {
    let mut without_nulls = false;
    let mut idx = 0;
    while idx + 1 < args.len() && args[idx].starts_with('-') {
        match args[idx].as_str() {
            "-withoutnulls" => {
                without_nulls = true;
                idx += 1;
            }
            other => {
                return Err(TclControl::Error(format!("unknown option: \"{other}\"")));
            }
        }
    }

    let rest_args = &args[idx..];
    if rest_args.is_empty() || rest_args.len() > 3 {
        return Err(TclControl::Error(
            "wrong # args: should be \"db eval ?OPTIONS? SQL ?VAR-NAME? ?SCRIPT?\"".to_string(),
        ));
    }
    let sql = &rest_args[0];
    let body_spec = match rest_args.len() {
        1 => None,
        2 => Some((None, &rest_args[1])),
        3 => {
            let arr = if rest_args[1].is_empty() {
                None
            } else {
                Some(rest_args[1].as_str())
            };
            Some((arr, &rest_args[2]))
        }
        _ => unreachable!(),
    };

    let _eval_guard = DbEvalScopeGuard::enter(host, db_id);
    host.borrow_mut().clear_db_error(db_id);

    let mut rem: &str = sql;
    let mut flat_out: Vec<String> = Vec::new();

    while !rem.is_empty() {
        let is_empty_stmt = match sqliters_syntax::parse_one(rem) {
            Ok((None, _)) => true,
            Ok((Some(_), _)) => false,
            Err(_) => false,
        };
        if is_empty_stmt {
            // Check connection usability on empty/comment SQL, then finish.
            let (conn, _) = checkout_db_conn(host, db_id)?;
            let stmt = match conn.prepare(rem) {
                Ok(s) => s,
                Err(e) => {
                    let msg = sqlite_err_text(&e);
                    host.borrow_mut().set_db_error(
                        db_id,
                        e.code as i32,
                        e.extended_code,
                        msg.clone(),
                    );
                    return Err(TclControl::Error(msg));
                }
            };
            drop(conn);
            if let Err(e) = stmt.finalize() {
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
            break;
        }

        let (conn, initial_nullvalue) = checkout_db_conn(host, db_id)?;
        let mut stmt = match conn.prepare(rem) {
            Ok(s) => s,
            Err(e) => {
                drop(conn);
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
        };
        // Drop `Rc<Connection>` immediately so `db close` inside `interp.eval(body)` sees
        // `Rc::strong_count == 1` and exercises real `Connection::try_close()` -> `SQLITE_BUSY`!
        drop(conn);

        if let Err(e) = bind_tcl_sql_parameters(&mut stmt, interp) {
            let _ = stmt.finalize();
            let msg = sqlite_err_text(&e);
            host.borrow_mut()
                .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            return Err(TclControl::Error(msg));
        }

        let next_tail_len = stmt.tail().len();
        let cur_rem_len = rem.len();
        let mut wrote_array_star = false;

        loop {
            match stmt.step() {
                Ok(StepResult::Row(row)) => {
                    let nv = current_db_nullvalue(host, db_id, &initial_nullvalue);
                    let cols = stmt.column_names();
                    if let Some((arr_opt, body)) = body_spec {
                        if let Some(arr) = arr_opt {
                            if !wrote_array_star {
                                interp.set_array_entry(arr, "*", format_list(cols));
                                wrote_array_star = true;
                            }
                            for (c, v) in cols.iter().zip(row.values().iter()) {
                                if without_nulls && matches!(v, Value::Null) {
                                    let _ = interp.unset_var(&format!("{arr}({c})"), true);
                                } else {
                                    interp.set_array_entry(arr, c, value_to_tcl_str(v, &nv));
                                }
                            }
                        } else {
                            for (c, v) in cols.iter().zip(row.values().iter()) {
                                let _ = interp.set_var(c, value_to_tcl_str(v, &nv));
                            }
                        }

                        // Zero `RefCell` borrow on `host` and zero `ConnSession` guard held here
                        // while `stmt` remains alive in `Row` state keeping its reader/writer lease!
                        match interp.eval(body) {
                            Ok(_) | Err(TclControl::Continue) => {}
                            Err(TclControl::Break) => {
                                if let Err(fin_err) = stmt.finalize() {
                                    let msg = sqlite_err_text(&fin_err);
                                    host.borrow_mut().set_db_error(
                                        db_id,
                                        fin_err.code as i32,
                                        fin_err.extended_code,
                                        msg,
                                    );
                                } else {
                                    host.borrow_mut().clear_db_error(db_id);
                                }
                                return Ok(String::new());
                            }
                            Err(ctrl) => {
                                // Finalize `stmt` deterministically, recording any finalize error
                                // on `db` while preserving Tcl body control/error priority.
                                if let Err(fin_err) = stmt.finalize() {
                                    let msg = sqlite_err_text(&fin_err);
                                    host.borrow_mut().set_db_error(
                                        db_id,
                                        fin_err.code as i32,
                                        fin_err.extended_code,
                                        msg,
                                    );
                                } else if !matches!(ctrl, TclControl::Error(_)) {
                                    host.borrow_mut().clear_db_error(db_id);
                                }
                                return Err(ctrl);
                            }
                        }
                    } else {
                        for v in row.values() {
                            flat_out.push(value_to_tcl_str(v, &nv));
                        }
                    }
                }
                Ok(StepResult::Done) => {
                    if let Some((Some(arr), _)) = body_spec
                        && !wrote_array_star
                    {
                        interp.set_array_entry(arr, "*", format_list(stmt.column_names()));
                    }
                    break;
                }
                Err(step_err) => {
                    let _ = stmt.finalize();
                    let msg = sqlite_err_text(&step_err);
                    host.borrow_mut().set_db_error(
                        db_id,
                        step_err.code as i32,
                        step_err.extended_code,
                        msg.clone(),
                    );
                    return Err(TclControl::Error(msg));
                }
            }
        }

        if let Err(fin_err) = stmt.finalize() {
            let msg = sqlite_err_text(&fin_err);
            host.borrow_mut().set_db_error(
                db_id,
                fin_err.code as i32,
                fin_err.extended_code,
                msg.clone(),
            );
            return Err(TclControl::Error(msg));
        }
        host.borrow_mut().clear_db_error(db_id);

        if next_tail_len >= cur_rem_len {
            break;
        }
        rem = &rem[cur_rem_len - next_tail_len..];
    }

    if body_spec.is_some() {
        Ok(String::new())
    } else {
        Ok(format_list(&flat_out))
    }
}

fn db_onecolumn(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    sql: &str,
) -> TclResult<String> {
    let _eval_guard = DbEvalScopeGuard::enter(host, db_id);
    host.borrow_mut().clear_db_error(db_id);
    let mut rem = sql;
    while !rem.is_empty() {
        if matches!(sqliters_syntax::parse_one(rem), Ok((None, _))) {
            break;
        }
        let (conn, nv) = checkout_db_conn(host, db_id)?;
        let mut stmt = match conn.prepare(rem) {
            Ok(s) => s,
            Err(e) => {
                drop(conn);
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
        };
        drop(conn);
        if let Err(e) = bind_tcl_sql_parameters(&mut stmt, interp) {
            let _ = stmt.finalize();
            let msg = sqlite_err_text(&e);
            host.borrow_mut()
                .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            return Err(TclControl::Error(msg));
        }
        let next_tail_len = stmt.tail().len();
        let cur_len = rem.len();
        match stmt.step() {
            Ok(StepResult::Row(row)) => {
                let first = row
                    .get(0)
                    .map(|v| value_to_tcl_str(v, &nv))
                    .unwrap_or_default();
                let _ = stmt.finalize();
                host.borrow_mut().clear_db_error(db_id);
                return Ok(first);
            }
            Ok(StepResult::Done) => {
                if let Err(e) = stmt.finalize() {
                    let msg = sqlite_err_text(&e);
                    host.borrow_mut().set_db_error(
                        db_id,
                        e.code as i32,
                        e.extended_code,
                        msg.clone(),
                    );
                    return Err(TclControl::Error(msg));
                }
            }
            Err(e) => {
                let _ = stmt.finalize();
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
        }
        if next_tail_len >= cur_len {
            break;
        }
        rem = &rem[cur_len - next_tail_len..];
    }
    Ok(String::new())
}

fn db_exists(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    sql: &str,
) -> TclResult<String> {
    let _eval_guard = DbEvalScopeGuard::enter(host, db_id);
    host.borrow_mut().clear_db_error(db_id);
    let mut rem = sql;
    while !rem.is_empty() {
        if matches!(sqliters_syntax::parse_one(rem), Ok((None, _))) {
            break;
        }
        let (conn, _) = checkout_db_conn(host, db_id)?;
        let mut stmt = match conn.prepare(rem) {
            Ok(s) => s,
            Err(e) => {
                drop(conn);
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
        };
        drop(conn);
        if let Err(e) = bind_tcl_sql_parameters(&mut stmt, interp) {
            let _ = stmt.finalize();
            let msg = sqlite_err_text(&e);
            host.borrow_mut()
                .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            return Err(TclControl::Error(msg));
        }
        let next_tail_len = stmt.tail().len();
        let cur_len = rem.len();
        match stmt.step() {
            Ok(StepResult::Row(_)) => {
                let _ = stmt.finalize();
                host.borrow_mut().clear_db_error(db_id);
                return Ok("1".to_string());
            }
            Ok(StepResult::Done) => {
                if let Err(e) = stmt.finalize() {
                    let msg = sqlite_err_text(&e);
                    host.borrow_mut().set_db_error(
                        db_id,
                        e.code as i32,
                        e.extended_code,
                        msg.clone(),
                    );
                    return Err(TclControl::Error(msg));
                }
            }
            Err(e) => {
                let _ = stmt.finalize();
                let msg = sqlite_err_text(&e);
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
                return Err(TclControl::Error(msg));
            }
        }
        if next_tail_len >= cur_len {
            break;
        }
        rem = &rem[cur_len - next_tail_len..];
    }
    Ok("0".to_string())
}

fn run_sql_headless(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    sql: &str,
) -> TclResult<()> {
    let args = [sql.to_string()];
    let _ = db_eval(host, interp, db_id, &args)?;
    Ok(())
}

fn run_sql_flat(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    sql: &str,
) -> TclResult<Vec<String>> {
    let list_str = db_eval(host, interp, db_id, &[sql.to_string()])?;
    parse_list(&list_str).map_err(TclControl::Error)
}

fn run_sql_exec_with_headers(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_id: DbId,
    sql: &str,
) -> TclResult<Vec<String>> {
    run_sql_flat(host, interp, db_id, sql)
}

fn cmd_do_test(host: &SharedHost, interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() != 3 {
        return Err(TclControl::Error(
            "wrong # args: should be \"do_test name cmd expected\"".to_string(),
        ));
    }
    let mut name = args[0].clone();
    if let Ok(pfx) = interp.get_var("::testprefix")
        && !pfx.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        name = format!("{pfx}-{name}");
    }
    let cmd = &args[1];
    let expected = &args[2];

    let script = format!("{cmd};\n");
    let saved_frame = interp.frame_idx_internal();
    interp.set_frame_idx_internal(0);
    let eval_res = interp.eval(&script);
    interp.set_frame_idx_internal(saved_frame);

    match eval_res {
        Err(TclControl::FinishTest) => return Err(TclControl::FinishTest),
        Err(e) => {
            host.borrow_mut().report.failed.push(SubtestFailure {
                name,
                expected: expected.clone(),
                actual: format!("TCL Error: {}", e.err_msg()),
            });
        }
        Ok(result) => {
            if check_expected(&result, expected) {
                host.borrow_mut().report.passed += 1;
            } else {
                host.borrow_mut().report.failed.push(SubtestFailure {
                    name,
                    expected: expected.clone(),
                    actual: result,
                });
            }
        }
    }
    Ok(String::new())
}

fn cmd_source(host: &SharedHost, interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    let Some(target) = args.first() else {
        return Err(TclControl::Error("wrong # args to source".to_string()));
    };
    let file_tail = Path::new(target)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();

    match file_tail.as_str() {
        "tester.tcl" | "malloc_common.tcl" | "thread_common.tcl" => Ok(String::new()),
        _ => {
            let source_roots = host.borrow().source_roots.clone();
            let candidate = {
                let p = Path::new(target);
                if p.is_absolute() && p.exists() {
                    p.to_path_buf()
                } else {
                    let in_temp = interp.resolve_path(target);
                    if in_temp.exists() {
                        in_temp
                    } else if p.exists() {
                        p.to_path_buf()
                    } else {
                        let mut found = None;
                        for root in &source_roots {
                            let rel_in_root = root.join(target);
                            if rel_in_root.exists() {
                                found = Some(rel_in_root);
                                break;
                            }
                            let tail_in_root = root.join(&file_tail);
                            if tail_in_root.exists() {
                                found = Some(tail_in_root);
                                break;
                            }
                        }
                        found.unwrap_or(in_temp)
                    }
                }
            };
            let script = fs::read_to_string(&candidate).map_err(|e| {
                TclControl::Error(format!("cannot source {}: {e}", candidate.display()))
            })?;
            let prev_script_path = std::mem::replace(
                &mut interp.script_path,
                candidate.to_string_lossy().into_owned(),
            );
            let eval_res = interp.eval(&script);
            interp.script_path = prev_script_path;
            match eval_res {
                Ok(v) => Ok(v),
                Err(TclControl::Return { value, .. }) => Ok(value),
                Err(other) => Err(other),
            }
        }
    }
}

fn cmd_wal_checkpoint(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 && args.len() != 2 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_wal_checkpoint DB ?NAME?\"".to_string(),
        ));
    }
    let (db_id, conn) = {
        let h = host.borrow();
        let db_id = h
            .resolve_db_id(&args[0])
            .ok_or_else(|| TclControl::Error(format!("not a database: {}", args[0])))?;
        let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
            return Err(TclControl::Error(format!("not a database: {}", args[0])));
        };
        (db_id, Rc::clone(&db.conn))
    };
    let db_name = args.get(1).map(String::as_str);
    match conn.wal_checkpoint(db_name) {
        Ok(()) => {
            host.borrow_mut().clear_db_error(db_id);
            Ok("SQLITE_OK".to_string())
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff = host
                .borrow_mut()
                .set_db_error(db_id, e.code as i32, e.extended_code, msg);
            Ok(sqlite_err_name(eff).to_string())
        }
    }
}

fn cmd_wal_checkpoint_v2(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_wal_checkpoint_v2 DB MODE ?NAME?\"".to_string(),
        ));
    }
    let (db_id, conn) = {
        let h = host.borrow();
        let db_id = h
            .resolve_db_id(&args[0])
            .ok_or_else(|| TclControl::Error(format!("not a database: {}", args[0])))?;
        let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
            return Err(TclControl::Error(format!("not a database: {}", args[0])));
        };
        (db_id, Rc::clone(&db.conn))
    };

    let mode_arg = args[1].trim();
    let mode_res: std::result::Result<WalCheckpointMode, SqliteError> =
        if let Some(n) = parse_tcl_int(mode_arg) {
            match n {
                -1 => Ok(WalCheckpointMode::Noop),
                0 => Ok(WalCheckpointMode::Passive),
                1 => Ok(WalCheckpointMode::Full),
                2 => Ok(WalCheckpointMode::Restart),
                3 => Ok(WalCheckpointMode::Truncate),
                _ => Err(SqliteError::misuse("invalid checkpoint mode")),
            }
        } else {
            const MODES: &[(&str, WalCheckpointMode)] = &[
                ("noop", WalCheckpointMode::Noop),
                ("passive", WalCheckpointMode::Passive),
                ("full", WalCheckpointMode::Full),
                ("restart", WalCheckpointMode::Restart),
                ("truncate", WalCheckpointMode::Truncate),
            ];
            let lower = mode_arg.to_ascii_lowercase();
            let mut matched: Option<WalCheckpointMode> = None;
            if !lower.is_empty() {
                for &(name, m) in MODES {
                    if name == lower {
                        matched = Some(m);
                        break;
                    }
                    if name.starts_with(&lower) {
                        if matched.is_some() {
                            matched = None;
                            break;
                        }
                        matched = Some(m);
                    }
                }
            }
            match matched {
                Some(m) => Ok(m),
                None => {
                    return Err(TclControl::Error(format!(
                        "bad mode \"{mode_arg}\": must be noop, passive, full, restart, or truncate"
                    )));
                }
            }
        };

    let db_name = args.get(2).map(String::as_str);
    let cp_res = mode_res.and_then(|m| conn.wal_checkpoint_v2(db_name, m));
    match cp_res {
        Ok(res) => {
            if res.busy {
                let _ = host.borrow_mut().set_db_error(
                    db_id,
                    ErrorCode::Busy as i32,
                    ErrorCode::Busy as i32,
                    "database is locked".to_string(),
                );
            } else {
                host.borrow_mut().clear_db_error(db_id);
            }
            Ok(format_list(&[
                if res.busy { "1" } else { "0" }.to_string(),
                res.n_log.to_string(),
                res.n_ckpt.to_string(),
            ]))
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff =
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            let z_err_code = sqlite_err_name(eff);
            Err(TclControl::Error(format!("{z_err_code} - {msg}")))
        }
    }
}

fn cmd_wal_autocheckpoint(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 2 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_wal_autocheckpoint DB VALUE\"".to_string(),
        ));
    }
    let val = parse_tcl_int(args[1].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[1])))?;
    let (db_id, conn) = {
        let h = host.borrow();
        let db_id = h
            .resolve_db_id(&args[0])
            .ok_or_else(|| TclControl::Error(format!("not a database: {}", args[0])))?;
        let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
            return Err(TclControl::Error(format!("not a database: {}", args[0])));
        };
        (db_id, Rc::clone(&db.conn))
    };
    match conn.wal_autocheckpoint(val as i32) {
        Ok(()) => {
            host.borrow_mut().clear_db_error(db_id);
            Ok("SQLITE_OK".to_string())
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff = host
                .borrow_mut()
                .set_db_error(db_id, e.code as i32, e.extended_code, msg);
            Ok(sqlite_err_name(eff).to_string())
        }
    }
}

fn cmd_file_control_persist_wal(
    host: &SharedHost,
    cmd_name: &str,
    args: &[String],
) -> TclResult<String> {
    if args.len() != 2 {
        return Err(TclControl::Error(format!(
            "wrong # args: should be \"{cmd_name} DB FLAG\""
        )));
    }
    let flag = parse_tcl_int(args[1].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[1])))?
        as i32;
    let conn = {
        let h = host.borrow();
        let db_id = h
            .resolve_db_id(&args[0])
            .ok_or_else(|| TclControl::Error(format!("not a database: {}", args[0])))?;
        let Some(DbSlot::Present(db)) = h.dbs.get(&db_id) else {
            return Err(TclControl::Error(format!("not a database: {}", args[0])));
        };
        Rc::clone(&db.conn)
    };
    match conn.file_control_persist_wal(None, flag) {
        Ok(persisted) => Ok(format!("0 {}", i32::from(persisted))),
        Err(e) => Ok(format!("{} {flag}", e.code as i32)),
    }
}

fn cmd_ifcapable(host: &SharedHost, interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() < 2 {
        return Err(TclControl::Error("wrong # args to ifcapable".to_string()));
    }
    let ok = eval_capable_expr(host, interp, &args[0])?;
    if ok {
        interp.eval(&args[1])
    } else if args.len() >= 4 && args[2] == "else" {
        interp.eval(&args[3])
    } else if args.len() == 3 {
        interp.eval(&args[2])
    } else {
        Ok(String::new())
    }
}

fn eval_capable_expr(host: &SharedHost, interp: &mut TclInterp, raw: &str) -> TclResult<bool> {
    let mut rewritten = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let key = format!("::sqlite_options({word})");
            let v = match interp.get_var(&key) {
                Ok(val) => val,
                Err(_) => match phase1_capability(&word) {
                    Some(b) => if b { "1" } else { "0" }.to_string(),
                    None => {
                        host.borrow_mut().record_unknown_capability(&word);
                        "0".to_string()
                    }
                },
            };
            rewritten.push_str(&v);
        } else {
            rewritten.push(chars[i]);
            i += 1;
        }
    }
    let val = eval_expr(interp, &rewritten)?;
    val.as_bool().map_err(TclControl::Error)
}

fn cmd_reset_db(host: &SharedHost, interp: &mut TclInterp) -> TclResult<String> {
    // 1. Finalize any live statements owned by `"db"` OR orphaned from already-deleted
    //    DB commands, and close `"db"` BEFORE touching disk files!
    let existing_db_id = host.borrow().resolve_db_id("db");
    let stmts_to_finalize: Vec<Statement> = {
        let mut h = host.borrow_mut();
        let ids: Vec<String> = h
            .stmts
            .iter()
            .filter_map(|(sid, slot)| match slot {
                StmtSlot::Present(rec)
                    if Some(rec.owner_db_id) == existing_db_id
                        || !h.dbs.contains_key(&rec.owner_db_id) =>
                {
                    Some(sid.clone())
                }
                _ => None,
            })
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        for sid in ids {
            if let Some(StmtSlot::Present(rec)) = h.stmts.remove(&sid) {
                out.push(rec.stmt);
            }
        }
        out
    };
    for st in stmts_to_finalize {
        let _ = st.finalize();
    }
    if let Some(db_id) = existing_db_id {
        let _ = db_close_cmd(host, "db", db_id);
        delete_host_cmd_if_exists(host, "db");
    }

    // 2. Remove `test.db` and rollback/WAL sidecars now that handles are closed.
    for suffix in ["test.db", "test.db-journal", "test.db-wal", "test.db-shm"] {
        let p = interp.resolve_path(suffix);
        let _ = fs::remove_file(p);
    }

    // 3. Reopen `"db"` with the `testfixture` profile (`1024` page size + domain `LockGeometry`).
    let opts = host.borrow().open_options();
    let db_path = interp.resolve_path("test.db");
    let conn =
        Connection::open_with_options(db_path, OpenFlags::READWRITE | OpenFlags::CREATE, opts)
            .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?;

    interp.remove_non_host_cmd("db");
    let (new_db_id, ptr_token) = host.borrow_mut().register_new_db("db".to_string(), conn);
    let _ = interp.set_var("::DB", ptr_token);

    if let Ok(setup) = interp.get_var("::SETUP_SQL")
        && !setup.trim().is_empty()
    {
        run_sql_headless(host, interp, new_db_id, &setup)?;
    }
    Ok(String::new())
}

fn floor_utf8_boundary(s: &str, max_bytes: usize) -> usize {
    let mut idx = max_bytes.min(s.len());
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn cmd_prepare(
    host: &SharedHost,
    interp: &mut TclInterp,
    cmd_name: &str,
    mode: PrepareMode,
    args: &[String],
) -> TclResult<String> {
    if args.len() != 3 && args.len() != 4 {
        return Err(TclControl::Error(format!(
            "wrong # args: should be \"{cmd_name} DB sql bytes ?tailvar?\""
        )));
    }
    let dbn = &args[0];
    let full_sql = &args[1];
    let n_bytes = parse_tcl_int(args[2].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[2])))?;
    let tail_var = args.get(3);

    let (db_id, conn) = {
        let h = host.borrow();
        let Some(id) = h.resolve_db_id(dbn) else {
            return Err(TclControl::Error(format!("not a database: {dbn}")));
        };
        let Some(DbSlot::Present(db)) = h.dbs.get(&id) else {
            return Err(TclControl::Error(format!("not a database: {dbn}")));
        };
        (id, Rc::clone(&db.conn))
    };

    let (slice_sql, raw_limit) = if n_bytes < 0 {
        (full_sql.as_str(), full_sql.len())
    } else {
        let requested = (n_bytes as usize).min(full_sql.len());
        let valid_end = floor_utf8_boundary(full_sql, requested);
        (&full_sql[..valid_end], requested)
    };

    let parsed_preview = sqliters_syntax::parse_one(slice_sql);
    let prep_res = conn.prepare_with_mode(slice_sql, mode);
    drop(conn);

    match prep_res {
        Ok(stmt) => {
            let unparsed_in_slice = stmt.tail().len();
            let consumed_len = slice_sql.len().saturating_sub(unparsed_in_slice);
            let computed_tail = if raw_limit == slice_sql.len() {
                slice_sql[consumed_len..].to_string()
            } else {
                String::from_utf8_lossy(&full_sql.as_bytes()[consumed_len..raw_limit]).into_owned()
            };

            let is_empty_stmt = matches!(parsed_preview, Ok((None, _)));
            if is_empty_stmt {
                // Empty/comment-only SQL returns `""` (`pStmt == NULL` in `test1.c:5155, 5229`)
                // and must NOT leave an open statement handle registered on `db`.
                let _ = stmt.finalize();
                host.borrow_mut().clear_db_error(db_id);
                if let Some(tv) = tail_var {
                    interp
                        .set_var(tv, computed_tail)
                        .map_err(TclControl::Error)?;
                }
                return Ok(String::new());
            }

            let col_names = stmt.column_names().to_vec();
            let stmt_id = {
                let mut h = host.borrow_mut();
                h.clear_db_error(db_id);
                let id = format!("stmt{}", h.next_stmt_id);
                h.next_stmt_id += 1;
                h.stmts.insert(
                    id.clone(),
                    StmtSlot::Present(Box::new(PreparedStmtRecord {
                        stmt,
                        owner_db_id: db_id,
                        current_row: None,
                        column_names: col_names,
                    })),
                );
                id
            };

            if let Some(tv) = tail_var {
                interp
                    .set_var(tv, computed_tail)
                    .map_err(TclControl::Error)?;
            }
            Ok(stmt_id)
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let effective_rc =
                host.borrow_mut()
                    .set_db_error(db_id, e.code as i32, e.extended_code, msg.clone());
            // Per `test1.c:5139` (`sqlite3_prepare`), `zTail` is written even on semantic
            // compile failure (`no such table`/`column`), whereas `test1.c:5216`
            // (`sqlite3_prepare_v2`) writes `tailvar` only on `SQLITE_OK`.
            if mode == PrepareMode::V1
                && let Some(tv) = tail_var
            {
                let v1_tail = match parsed_preview {
                    Ok((_, rem_tail)) => {
                        let consumed = slice_sql.len().saturating_sub(rem_tail.len());
                        String::from_utf8_lossy(&full_sql.as_bytes()[consumed..raw_limit])
                            .into_owned()
                    }
                    Err(_) => String::new(),
                };
                let _ = interp.set_var(tv, v1_tail);
            }
            Err(TclControl::Error(format!("({effective_rc}) {msg}")))
        }
    }
}

fn cmd_step(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_step STMT\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("SQLITE_MISUSE".to_string());
    }

    let mut rec = {
        let mut h = host.borrow_mut();
        let Some(slot) = h.stmts.get_mut(id) else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let StmtSlot::Present(ref cur) = *slot else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let prev = std::mem::replace(
            slot,
            StmtSlot::CheckedOut {
                column_names: cur.column_names.clone(),
            },
        );
        let StmtSlot::Present(owned) = prev else {
            unreachable!()
        };
        owned
    };

    let owner_db_id = rec.owner_db_id;
    let mode = rec.stmt.prepare_mode();
    let step_res = rec.stmt.step();
    rec.column_names = rec.stmt.column_names().to_vec();

    let ret_code_name = match step_res {
        Ok(StepResult::Row(row)) => {
            rec.current_row = Some(row);
            host.borrow_mut()
                .set_db_step_status(owner_db_id, ErrorCode::Row as i32);
            "SQLITE_ROW".to_string()
        }
        Ok(StepResult::Done) => {
            rec.current_row = None;
            host.borrow_mut()
                .set_db_step_status(owner_db_id, ErrorCode::Done as i32);
            "SQLITE_DONE".to_string()
        }
        Err(e) => {
            rec.current_row = None;
            if mode == PrepareMode::V1 && !matches!(e.code, ErrorCode::Busy | ErrorCode::Misuse) {
                // Legacy `sqlite3_prepare` (`PrepareMode::V1`, `vdbeapi.c:968-972`):
                // `sqlite3_step` returns generic `SQLITE_ERROR` (`db->errCode = SQLITE_ERROR`),
                // while the underlying error (`SQLITE_SCHEMA`, `SQLITE_CONSTRAINT`, etc.) stays
                // latched on `Statement` and surfaces on `sqlite3_reset` / `sqlite3_finalize`!
                host.borrow_mut().set_db_error(
                    owner_db_id,
                    ErrorCode::Error as i32,
                    ErrorCode::Error as i32,
                    ErrorCode::Error.as_str().to_string(),
                );
                "SQLITE_ERROR".to_string()
            } else {
                let msg = sqlite_err_text(&e);
                let eff = host.borrow_mut().set_db_error(
                    owner_db_id,
                    e.code as i32,
                    e.extended_code,
                    msg,
                );
                sqlite_err_name(eff).to_string()
            }
        }
    };

    let mut h = host.borrow_mut();
    if h.stmts.contains_key(id) {
        h.stmts.insert(id.to_string(), StmtSlot::Present(rec));
    }
    Ok(ret_code_name)
}

fn cmd_stmt_reset(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_reset <STMT>\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("SQLITE_OK".to_string());
    }

    let mut rec = {
        let mut h = host.borrow_mut();
        let Some(slot) = h.stmts.get_mut(id) else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let StmtSlot::Present(ref cur) = *slot else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let prev = std::mem::replace(
            slot,
            StmtSlot::CheckedOut {
                column_names: cur.column_names.clone(),
            },
        );
        let StmtSlot::Present(owned) = prev else {
            unreachable!()
        };
        owned
    };

    rec.current_row = None;
    let owner_db_id = rec.owner_db_id;
    let reset_res = rec.stmt.reset();

    let ret_name = match reset_res {
        Ok(()) => {
            host.borrow_mut().clear_db_error(owner_db_id);
            "SQLITE_OK".to_string()
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff =
                host.borrow_mut()
                    .set_db_error(owner_db_id, e.code as i32, e.extended_code, msg);
            sqlite_err_name(eff).to_string()
        }
    };

    let mut h = host.borrow_mut();
    if h.stmts.contains_key(id) {
        h.stmts.insert(id.to_string(), StmtSlot::Present(rec));
    }
    Ok(ret_name)
}

fn cmd_stmt_finalize(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_finalize <STMT>\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("SQLITE_OK".to_string());
    }

    let rec = {
        let mut h = host.borrow_mut();
        match h.stmts.remove(id) {
            Some(StmtSlot::Present(owned)) => owned,
            Some(other @ StmtSlot::CheckedOut { .. }) => {
                h.stmts.insert(id.to_string(), other);
                return Ok("SQLITE_MISUSE".to_string());
            }
            None => return Ok("SQLITE_MISUSE".to_string()),
        }
    };

    let owner_db_id = rec.owner_db_id;
    match rec.stmt.finalize() {
        Ok(()) => {
            host.borrow_mut().clear_db_error(owner_db_id);
            Ok("SQLITE_OK".to_string())
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff =
                host.borrow_mut()
                    .set_db_error(owner_db_id, e.code as i32, e.extended_code, msg);
            Ok(sqlite_err_name(eff).to_string())
        }
    }
}

fn cmd_col_count(host: &SharedHost, data_count_only: bool, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    if id.is_empty() {
        return Ok("0".to_string());
    }
    let h = host.borrow();
    let cnt = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => {
            if data_count_only {
                rec.current_row.as_ref().map_or(0, Row::len)
            } else {
                rec.stmt.column_names().len()
            }
        }
        Some(StmtSlot::CheckedOut { column_names, .. }) => {
            if data_count_only {
                0
            } else {
                column_names.len()
            }
        }
        None => 0,
    };
    Ok(cnt.to_string())
}

fn cmd_col_name(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 2 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_column_name STMT column\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    let col = parse_tcl_int(args[1].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[1])))?;
    if id.is_empty() || col < 0 {
        return Ok(String::new());
    }
    let col_idx = col as usize;
    let h = host.borrow();
    let name = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.stmt.column_names().get(col_idx).cloned(),
        Some(StmtSlot::CheckedOut { column_names, .. }) => column_names.get(col_idx).cloned(),
        None => None,
    }
    .unwrap_or_default();
    Ok(name)
}

fn with_current_col_value<F, R>(host: &SharedHost, args: &[String], cmd: &str, f: F) -> TclResult<R>
where
    F: FnOnce(Option<&Value>) -> R,
{
    if args.len() != 2 {
        return Err(TclControl::Error(format!(
            "wrong # args: should be \"{cmd} STMT column\""
        )));
    }
    let id = args[0].as_str();
    let col = parse_tcl_int(args[1].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[1])))?;
    if id.is_empty() || col < 0 {
        return Ok(f(None));
    }
    let col_idx = col as usize;
    let h = host.borrow();
    let val_opt = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.current_row.as_ref().and_then(|r| r.get(col_idx)),
        _ => None,
    };
    Ok(f(val_opt))
}

fn cmd_col_type(host: &SharedHost, args: &[String]) -> TclResult<String> {
    with_current_col_value(host, args, "sqlite3_column_type", |v| {
        match v {
            Some(Value::Integer(_)) => "INTEGER",
            Some(Value::Real(_)) => "FLOAT",
            Some(Value::Text(_)) => "TEXT",
            Some(Value::Blob(_)) => "BLOB",
            Some(Value::Null) | None => "NULL",
        }
        .to_string()
    })
}

fn cmd_col_text(host: &SharedHost, args: &[String]) -> TclResult<String> {
    with_current_col_value(host, args, "sqlite3_column_text", |v| match v {
        Some(val) => value_to_tcl_str(val, ""),
        None => String::new(),
    })
}

fn cmd_col_int(host: &SharedHost, wide: bool, args: &[String]) -> TclResult<String> {
    let cmd = if wide {
        "sqlite3_column_int64"
    } else {
        "sqlite3_column_int"
    };
    with_current_col_value(host, args, cmd, |v| {
        let i = match v {
            Some(Value::Integer(n)) => *n,
            Some(Value::Real(r)) => *r as i64,
            Some(Value::Text(s)) => parse_tcl_int(s.trim()).unwrap_or(0),
            Some(Value::Blob(b)) => parse_tcl_int(String::from_utf8_lossy(b).trim()).unwrap_or(0),
            Some(Value::Null) | None => 0,
        };
        if wide {
            i.to_string()
        } else {
            (i as i32).to_string()
        }
    })
}

fn cmd_col_double(host: &SharedHost, args: &[String]) -> TclResult<String> {
    with_current_col_value(host, args, "sqlite3_column_double", |v| {
        let r = match v {
            Some(Value::Real(f)) => *f,
            Some(Value::Integer(n)) => *n as f64,
            Some(Value::Text(s)) => parse_tcl_float(s.trim()).unwrap_or(0.0),
            Some(Value::Blob(b)) => {
                parse_tcl_float(String::from_utf8_lossy(b).trim()).unwrap_or(0.0)
            }
            Some(Value::Null) | None => 0.0,
        };
        format_sqlite_real(r)
    })
}

fn cmd_col_bytes(host: &SharedHost, args: &[String]) -> TclResult<String> {
    with_current_col_value(host, args, "sqlite3_column_bytes", |v| {
        let len = match v {
            Some(Value::Null) | None => 0,
            Some(Value::Blob(b)) => b.len(),
            Some(other) => value_to_tcl_str(other, "").len(),
        };
        len.to_string()
    })
}

fn cmd_col_blob(host: &SharedHost, args: &[String]) -> TclResult<String> {
    with_current_col_value(host, args, "sqlite3_column_blob", |v| match v {
        Some(Value::Blob(b)) => b.iter().map(|&byte| byte as char).collect(),
        Some(Value::Text(s)) => s.clone(),
        Some(Value::Integer(_) | Value::Real(_)) => value_to_tcl_str(v.unwrap(), ""),
        Some(Value::Null) | None => String::new(),
    })
}

fn cmd_stmt_sql(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_sql STMT\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok(String::new());
    }
    let h = host.borrow();
    let sql = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.stmt.sql().to_string(),
        _ => String::new(),
    };
    Ok(sql)
}

fn cmd_stmt_busy(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_stmt_busy STMT\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("0".to_string());
    }
    let h = host.borrow();
    let busy = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.stmt.is_busy(),
        Some(StmtSlot::CheckedOut { .. }) => true,
        None => false,
    };
    Ok(if busy { "1" } else { "0" }.to_string())
}

fn cmd_bind_scalar(host: &SharedHost, kind: &str, args: &[String]) -> TclResult<String> {
    let min_args = if kind == "null" { 2 } else { 3 };
    if args.len() < min_args {
        return Err(TclControl::Error(format!(
            "wrong # args to sqlite3_bind_{kind}"
        )));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("SQLITE_MISUSE".to_string());
    }
    let idx_raw = parse_tcl_int(args[1].trim())
        .ok_or_else(|| TclControl::Error(format!("expected integer but got \"{}\"", args[1])))?;

    let val = match kind {
        "null" => Value::Null,
        "int" => {
            let n = parse_tcl_int(args[2].trim()).ok_or_else(|| {
                TclControl::Error(format!("expected integer but got \"{}\"", args[2]))
            })?;
            Value::Integer(i64::from(n as i32))
        }
        "int64" => {
            let n = parse_tcl_int(args[2].trim()).ok_or_else(|| {
                TclControl::Error(format!("expected integer but got \"{}\"", args[2]))
            })?;
            Value::Integer(n)
        }
        "double" => {
            let r = parse_tcl_float(args[2].trim()).ok_or_else(|| {
                TclControl::Error(format!(
                    "expected floating-point number but got \"{}\"",
                    args[2]
                ))
            })?;
            Value::real(r)
        }
        "text" => {
            let raw = &args[2];
            if let Some(nbyte_str) = args.get(3) {
                let nb = parse_tcl_int(nbyte_str.trim()).ok_or_else(|| {
                    TclControl::Error(format!("expected integer but got \"{nbyte_str}\""))
                })?;
                if nb >= 0 {
                    let end = floor_utf8_boundary(raw, nb as usize);
                    Value::Text(raw[..end].to_string())
                } else {
                    Value::Text(raw.clone())
                }
            } else {
                Value::Text(raw.clone())
            }
        }
        "blob" => {
            let raw_bytes: Vec<u8> = args[2].chars().map(|c| (c as u32 & 0xff) as u8).collect();
            if let Some(nbyte_str) = args.get(3) {
                let nb = parse_tcl_int(nbyte_str.trim()).ok_or_else(|| {
                    TclControl::Error(format!("expected integer but got \"{nbyte_str}\""))
                })?;
                if nb >= 0 {
                    let lim = (nb as usize).min(raw_bytes.len());
                    Value::Blob(raw_bytes[..lim].to_vec())
                } else {
                    Value::Blob(raw_bytes)
                }
            } else {
                Value::Blob(raw_bytes)
            }
        }
        "zeroblob" => {
            let n = parse_tcl_int(args[2].trim()).ok_or_else(|| {
                TclControl::Error(format!("expected integer but got \"{}\"", args[2]))
            })?;
            let len = n.max(0) as usize;
            Value::Blob(vec![0u8; len])
        }
        _ => Value::Null,
    };

    let mut rec = {
        let mut h = host.borrow_mut();
        let Some(slot) = h.stmts.get_mut(id) else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let StmtSlot::Present(ref cur) = *slot else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let prev = std::mem::replace(
            slot,
            StmtSlot::CheckedOut {
                column_names: cur.column_names.clone(),
            },
        );
        let StmtSlot::Present(owned) = prev else {
            unreachable!()
        };
        owned
    };

    let owner_db_id = rec.owner_db_id;
    let bind_res = if idx_raw <= 0 {
        Err(SqliteError::new(
            ErrorCode::Range,
            format!("column/bind index {idx_raw} out of range"),
        ))
    } else {
        rec.stmt.bind(idx_raw as usize, val).map(|_| ())
    };

    let ret_name = match bind_res {
        Ok(()) => {
            host.borrow_mut().clear_db_error(owner_db_id);
            "SQLITE_OK".to_string()
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff =
                host.borrow_mut()
                    .set_db_error(owner_db_id, e.code as i32, e.extended_code, msg);
            sqlite_err_name(eff).to_string()
        }
    };

    let mut h = host.borrow_mut();
    if h.stmts.contains_key(id) {
        h.stmts.insert(id.to_string(), StmtSlot::Present(rec));
    }
    Ok(ret_name)
}

fn cmd_bind_param_count(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_bind_parameter_count STMT\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    let h = host.borrow();
    let cnt = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.stmt.parameter_count(),
        _ => 0,
    };
    Ok(cnt.to_string())
}

fn cmd_bind_param_name(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 2 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_bind_parameter_name STMT i\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    let idx = parse_tcl_int(args[1].trim()).unwrap_or(0);
    if idx <= 0 {
        return Ok(String::new());
    }
    let target = idx as usize;
    let h = host.borrow();
    let found = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec
            .stmt
            .named_parameters()
            .iter()
            .find(|&(_, &v)| v == target)
            .map(|(k, _)| k.clone()),
        _ => None,
    }
    .unwrap_or_default();
    Ok(found)
}

fn cmd_bind_param_index(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 2 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_bind_parameter_index STMT name\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    let name = args[1].as_str();
    let h = host.borrow();
    let idx = match h.stmts.get(id) {
        Some(StmtSlot::Present(rec)) => rec.stmt.named_parameters().get(name).copied().unwrap_or(0),
        _ => 0,
    };
    Ok(idx.to_string())
}

fn cmd_clear_bindings(host: &SharedHost, args: &[String]) -> TclResult<String> {
    if args.len() != 1 {
        return Err(TclControl::Error(
            "wrong # args: should be \"sqlite3_clear_bindings STMT\"".to_string(),
        ));
    }
    let id = args[0].as_str();
    if id.is_empty() {
        return Ok("SQLITE_MISUSE".to_string());
    }

    let mut rec = {
        let mut h = host.borrow_mut();
        let Some(slot) = h.stmts.get_mut(id) else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let StmtSlot::Present(ref cur) = *slot else {
            return Ok("SQLITE_MISUSE".to_string());
        };
        let prev = std::mem::replace(
            slot,
            StmtSlot::CheckedOut {
                column_names: cur.column_names.clone(),
            },
        );
        let StmtSlot::Present(owned) = prev else {
            unreachable!()
        };
        owned
    };

    let owner_db_id = rec.owner_db_id;
    let res = rec.stmt.clear_bindings();
    let ret_name = match res {
        Ok(()) => {
            host.borrow_mut().clear_db_error(owner_db_id);
            "SQLITE_OK".to_string()
        }
        Err(e) => {
            let msg = sqlite_err_text(&e);
            let eff =
                host.borrow_mut()
                    .set_db_error(owner_db_id, e.code as i32, e.extended_code, msg);
            sqlite_err_name(eff).to_string()
        }
    };

    let mut h = host.borrow_mut();
    if h.stmts.contains_key(id) {
        h.stmts.insert(id.to_string(), StmtSlot::Present(rec));
    }
    Ok(ret_name)
}

fn cmd_hexio_read(interp: &TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() != 3 {
        return Err(TclControl::Error("wrong # args to hexio_read".to_string()));
    }
    let p = interp.resolve_path(&args[0]);
    let off = parse_tcl_int(&args[1]).unwrap_or(0).max(0) as u64;
    let len = parse_tcl_int(&args[2]).unwrap_or(0).max(0) as usize;
    let mut f = fs::File::open(p).map_err(|e| TclControl::Error(e.to_string()))?;
    let _ = f.seek(SeekFrom::Start(off));
    let mut buf = vec![0u8; len];
    let n = f.read(&mut buf).unwrap_or(0);
    let mut out = String::with_capacity(n * 2);
    for &b in &buf[..n] {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02X}");
    }
    Ok(out)
}

fn cmd_hexio_write(interp: &TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() != 3 {
        return Err(TclControl::Error("wrong # args to hexio_write".to_string()));
    }
    let p = interp.resolve_path(&args[0]);
    let off = parse_tcl_int(&args[1]).unwrap_or(0).max(0) as u64;
    let hex: String = args[2].chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut i = 0;
    while i + 1 < hex.len() {
        if let Ok(b) = u8::from_str_radix(&hex[i..i + 2], 16) {
            bytes.push(b);
        }
        i += 2;
    }
    let mut f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(p)
        .map_err(|e| TclControl::Error(e.to_string()))?;
    f.seek(SeekFrom::Start(off))
        .map_err(|e| TclControl::Error(e.to_string()))?;
    f.write_all(&bytes)
        .map_err(|e| TclControl::Error(e.to_string()))?;
    Ok(bytes.len().to_string())
}

fn cmd_randstr(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let min_len = args
        .first()
        .and_then(|s| parse_tcl_int(s))
        .unwrap_or(10)
        .max(0) as usize;
    let max_len = args
        .get(1)
        .and_then(|s| parse_tcl_int(s))
        .map(|m| m.max(min_len as i64) as usize)
        .unwrap_or(min_len);
    let mut h = host.borrow_mut();
    let len = if max_len > min_len {
        min_len + (h.next_rand() as usize % (max_len - min_len + 1))
    } else {
        min_len
    };
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut s = String::with_capacity(len);
    for _ in 0..len {
        let idx = (h.next_rand() as usize) % ALPHABET.len();
        s.push(ALPHABET[idx] as char);
    }
    Ok(s)
}

fn sqlite_err_text(e: &SqliteError) -> String {
    if e.message.is_empty() {
        e.code.as_str().to_string()
    } else {
        e.message.clone()
    }
}

fn value_to_tcl_str(v: &Value, nullvalue: &str) -> String {
    match v {
        Value::Null => nullvalue.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn tcl_string_to_sqlite_value(s: &str) -> Value {
    if let Some(i) = parse_tcl_int(s)
        && i.to_string() == s
    {
        return Value::Integer(i);
    }
    if (s.contains('.') || s.contains('e') || s.contains('E'))
        && let Some(f) = parse_tcl_float(s)
        && f.is_finite()
    {
        return Value::real(f);
    }
    Value::Text(s.to_string())
}

fn check_expected(result: &str, expected: &str) -> bool {
    let is_slash_form =
        (expected.starts_with('/') && expected.ends_with('/') && expected.len() >= 2)
            || ((expected.starts_with("~/") || expected.starts_with("#/"))
                && expected.ends_with('/')
                && expected.len() >= 3);

    if is_slash_form {
        if let Some(inner) = expected
            .strip_prefix("~/")
            .and_then(|s| s.strip_suffix('/'))
        {
            let matched = if inner.starts_with('*') {
                glob_match(inner, result, false)
            } else {
                let re_pat = inner.replace('#', "[-0-9.]+");
                Regex::compile(&re_pat, false)
                    .map(|r| r.is_match(result))
                    .unwrap_or(false)
            };
            return !matched;
        }
        if let Some(inner) = expected
            .strip_prefix("#/")
            .and_then(|s| s.strip_suffix('/'))
        {
            let Ok(r_items) = parse_list(result) else {
                return false;
            };
            let Ok(e_items) = parse_list(inner) else {
                return false;
            };
            if r_items.len() != e_items.len() {
                return false;
            }
            for (rv, ev) in r_items.iter().zip(e_items.iter()) {
                let Some(rf) = parse_tcl_float(rv.trim()) else {
                    return false;
                };
                if let Some((lo_s, hi_s)) = ev.split_once("..") {
                    let (Some(lo), Some(hi)) = (parse_tcl_float(lo_s), parse_tcl_float(hi_s))
                    else {
                        return false;
                    };
                    if rf < lo || rf > hi {
                        return false;
                    }
                } else if let Some(ef) = parse_tcl_float(ev.trim()) {
                    let lo = (0.9 * ef).min(1.1 * ef);
                    let hi = (0.9 * ef).max(1.1 * ef);
                    if rf < lo || rf > hi {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            return true;
        }
        if let Some(inner) = expected.strip_prefix('/').and_then(|s| s.strip_suffix('/')) {
            if inner.starts_with('*') {
                return glob_match(inner, result, false);
            }
            let re_pat = inner.replace('#', "[-0-9.]+");
            return Regex::compile(&re_pat, false)
                .map(|r| r.is_match(result))
                .unwrap_or(false);
        }
    }

    if expected.ends_with('*') && expected.len() >= 2 {
        if let Some(glob_pat) = expected.strip_prefix("~*") {
            let full = format!("*{glob_pat}");
            return !glob_match(&full, result, false);
        }
        if expected.starts_with('*') {
            return glob_match(expected, result, false);
        }
    }

    result == expected || fpnum_compare(result, expected)
}

/// Symbolic SQLite result/error code name (`sqlite3ErrName` in `main.c` / `t1ErrorName` in `test1.c`).
pub fn sqlite_err_name(code: i32) -> &'static str {
    match code {
        0 => "SQLITE_OK",
        1 => "SQLITE_ERROR",
        2 => "SQLITE_INTERNAL",
        3 => "SQLITE_PERM",
        4 => "SQLITE_ABORT",
        5 => "SQLITE_BUSY",
        6 => "SQLITE_LOCKED",
        7 => "SQLITE_NOMEM",
        8 => "SQLITE_READONLY",
        9 => "SQLITE_INTERRUPT",
        10 => "SQLITE_IOERR",
        11 => "SQLITE_CORRUPT",
        12 => "SQLITE_NOTFOUND",
        13 => "SQLITE_FULL",
        14 => "SQLITE_CANTOPEN",
        15 => "SQLITE_PROTOCOL",
        16 => "SQLITE_EMPTY",
        17 => "SQLITE_SCHEMA",
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        22 => "SQLITE_NOLFS",
        23 => "SQLITE_AUTH",
        24 => "SQLITE_FORMAT",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        27 => "SQLITE_NOTICE",
        28 => "SQLITE_WARNING",
        100 => "SQLITE_ROW",
        101 => "SQLITE_DONE",
        extended::SQLITE_ERROR_MISSING_COLLSEQ => "SQLITE_ERROR_MISSING_COLLSEQ",
        extended::SQLITE_ERROR_RETRY => "SQLITE_ERROR_RETRY",
        extended::SQLITE_ERROR_SNAPSHOT => "SQLITE_ERROR_SNAPSHOT",
        extended::SQLITE_IOERR_READ => "SQLITE_IOERR_READ",
        extended::SQLITE_IOERR_SHORT_READ => "SQLITE_IOERR_SHORT_READ",
        extended::SQLITE_IOERR_WRITE => "SQLITE_IOERR_WRITE",
        extended::SQLITE_IOERR_FSYNC => "SQLITE_IOERR_FSYNC",
        extended::SQLITE_IOERR_DIR_FSYNC => "SQLITE_IOERR_DIR_FSYNC",
        extended::SQLITE_IOERR_TRUNCATE => "SQLITE_IOERR_TRUNCATE",
        extended::SQLITE_IOERR_FSTAT => "SQLITE_IOERR_FSTAT",
        extended::SQLITE_IOERR_UNLOCK => "SQLITE_IOERR_UNLOCK",
        extended::SQLITE_IOERR_RDLOCK => "SQLITE_IOERR_RDLOCK",
        extended::SQLITE_IOERR_DELETE => "SQLITE_IOERR_DELETE",
        extended::SQLITE_IOERR_BLOCKED => "SQLITE_IOERR_BLOCKED",
        extended::SQLITE_IOERR_NOMEM => "SQLITE_IOERR_NOMEM",
        extended::SQLITE_IOERR_ACCESS => "SQLITE_IOERR_ACCESS",
        extended::SQLITE_IOERR_CHECKRESERVEDLOCK => "SQLITE_IOERR_CHECKRESERVEDLOCK",
        extended::SQLITE_IOERR_LOCK => "SQLITE_IOERR_LOCK",
        extended::SQLITE_IOERR_CLOSE => "SQLITE_IOERR_CLOSE",
        extended::SQLITE_IOERR_DIR_CLOSE => "SQLITE_IOERR_DIR_CLOSE",
        extended::SQLITE_IOERR_SHMOPEN => "SQLITE_IOERR_SHMOPEN",
        extended::SQLITE_IOERR_SHMSIZE => "SQLITE_IOERR_SHMSIZE",
        extended::SQLITE_IOERR_SHMLOCK => "SQLITE_IOERR_SHMLOCK",
        extended::SQLITE_IOERR_SHMMAP => "SQLITE_IOERR_SHMMAP",
        extended::SQLITE_IOERR_SEEK => "SQLITE_IOERR_SEEK",
        extended::SQLITE_IOERR_DELETE_NOENT => "SQLITE_IOERR_DELETE_NOENT",
        extended::SQLITE_IOERR_MMAP => "SQLITE_IOERR_MMAP",
        extended::SQLITE_IOERR_GETTEMPPATH => "SQLITE_IOERR_GETTEMPPATH",
        extended::SQLITE_IOERR_CONVPATH => "SQLITE_IOERR_CONVPATH",
        extended::SQLITE_IOERR_IN_PAGE => "SQLITE_IOERR_IN_PAGE",
        extended::SQLITE_BUSY_RECOVERY => "SQLITE_BUSY_RECOVERY",
        extended::SQLITE_BUSY_SNAPSHOT => "SQLITE_BUSY_SNAPSHOT",
        extended::SQLITE_BUSY_TIMEOUT => "SQLITE_BUSY_TIMEOUT",
        extended::SQLITE_CANTOPEN_NOTEMPDIR => "SQLITE_CANTOPEN_NOTEMPDIR",
        extended::SQLITE_CANTOPEN_ISDIR => "SQLITE_CANTOPEN_ISDIR",
        extended::SQLITE_CANTOPEN_FULLPATH => "SQLITE_CANTOPEN_FULLPATH",
        extended::SQLITE_CANTOPEN_CONVPATH => "SQLITE_CANTOPEN_CONVPATH",
        extended::SQLITE_CANTOPEN_SYMLINK => "SQLITE_CANTOPEN_SYMLINK",
        extended::SQLITE_CORRUPT_INDEX => "SQLITE_CORRUPT_INDEX",
        extended::SQLITE_READONLY_RECOVERY => "SQLITE_READONLY_RECOVERY",
        extended::SQLITE_READONLY_CANTLOCK => "SQLITE_READONLY_CANTLOCK",
        extended::SQLITE_READONLY_ROLLBACK => "SQLITE_READONLY_ROLLBACK",
        extended::SQLITE_READONLY_DBMOVED => "SQLITE_READONLY_DBMOVED",
        extended::SQLITE_READONLY_CANTINIT => "SQLITE_READONLY_CANTINIT",
        extended::SQLITE_READONLY_DIRECTORY => "SQLITE_READONLY_DIRECTORY",
        extended::SQLITE_ABORT_ROLLBACK => "SQLITE_ABORT_ROLLBACK",
        extended::SQLITE_CONSTRAINT_CHECK => "SQLITE_CONSTRAINT_CHECK",
        extended::SQLITE_CONSTRAINT_FOREIGNKEY => "SQLITE_CONSTRAINT_FOREIGNKEY",
        extended::SQLITE_CONSTRAINT_NOTNULL => "SQLITE_CONSTRAINT_NOTNULL",
        extended::SQLITE_CONSTRAINT_PRIMARYKEY => "SQLITE_CONSTRAINT_PRIMARYKEY",
        extended::SQLITE_CONSTRAINT_UNIQUE => "SQLITE_CONSTRAINT_UNIQUE",
        extended::SQLITE_CONSTRAINT_ROWID => "SQLITE_CONSTRAINT_ROWID",
        extended::SQLITE_CONSTRAINT_DATATYPE => "SQLITE_CONSTRAINT_DATATYPE",
        extended::SQLITE_NOTICE_RECOVER_WAL => "SQLITE_NOTICE_RECOVER_WAL",
        extended::SQLITE_NOTICE_RECOVER_ROLLBACK => "SQLITE_NOTICE_RECOVER_ROLLBACK",
        other => match other & 0xff {
            1 => "SQLITE_ERROR",
            2 => "SQLITE_INTERNAL",
            3 => "SQLITE_PERM",
            4 => "SQLITE_ABORT",
            5 => "SQLITE_BUSY",
            6 => "SQLITE_LOCKED",
            7 => "SQLITE_NOMEM",
            8 => "SQLITE_READONLY",
            9 => "SQLITE_INTERRUPT",
            10 => "SQLITE_IOERR",
            11 => "SQLITE_CORRUPT",
            12 => "SQLITE_NOTFOUND",
            13 => "SQLITE_FULL",
            14 => "SQLITE_CANTOPEN",
            15 => "SQLITE_PROTOCOL",
            16 => "SQLITE_EMPTY",
            17 => "SQLITE_SCHEMA",
            18 => "SQLITE_TOOBIG",
            19 => "SQLITE_CONSTRAINT",
            20 => "SQLITE_MISMATCH",
            21 => "SQLITE_MISUSE",
            22 => "SQLITE_NOLFS",
            23 => "SQLITE_AUTH",
            24 => "SQLITE_FORMAT",
            25 => "SQLITE_RANGE",
            26 => "SQLITE_NOTADB",
            27 => "SQLITE_NOTICE",
            28 => "SQLITE_WARNING",
            _ => "SQLITE_ERROR",
        },
    }
}

const TESTER_TCL_PRELUDE: &str = r#"
proc execsql {sql {db db}} {
  uplevel [list $db eval $sql]
}

proc execsql_timed {sql {db db}} {
  uplevel [list $db eval $sql]
}

proc catchsql {sql {db db}} {
  set r [catch [list uplevel [list $db eval $sql]] msg]
  lappend r $msg
  return $r
}

proc execsql2 {sql} {
  set result {}
  db eval $sql data {
    foreach f $data(*) {
      lappend result $f $data($f)
    }
  }
  return $result
}

proc catchsql2 {sql {db db}} {
  set r [catch [list uplevel [list execsql2 $sql]] msg]
  lappend r $msg
  return $r
}

proc memdbsql {sql} {
  sqlite3 memdb :memory:
  set result [memdb eval $sql]
  memdb close
  return $result
}

proc fix_testname {varname} {
  upvar $varname testname
  if {[info exists ::testprefix] && [string is digit [string range $testname 0 0]]} {
    set testname "${::testprefix}-$testname"
  }
}

proc normalize_list {L} {
  set L2 [list]
  foreach l $L {lappend L2 $l}
  set L2
}

proc do_execsql_test {args} {
  set db db
  if {[lindex $args 0]=="-db"} {
    set db [lindex $args 1]
    set args [lrange $args 2 end]
  }
  if {[llength $args]==2} {
    foreach {testname sql} $args {}
    set result ""
  } elseif {[llength $args]==3} {
    foreach {testname sql result} $args {}
    if {[llength $result]==0} { set result "" }
  } else {
    error "wrong # args: should be \"do_execsql_test ?-db DB? testname sql ?result?\""
  }
  fix_testname testname
  uplevel do_test [list $testname] [list "execsql {$sql} $db"] [list [list {*}$result]]
}

proc do_catchsql_test {args} {
  set db db
  if {[lindex $args 0]=="-db"} {
    set db [lindex $args 1]
    set args [lrange $args 2 end]
  }
  foreach {testname sql result} $args {}
  fix_testname testname
  uplevel do_test [list $testname] [list "catchsql {$sql} $db"] [list $result]
}

proc do_timed_execsql_test {testname sql {result {}}} {
  uplevel [list do_execsql_test $testname $sql $result]
}

proc realnum_normalize {r} {
  string map {1.#INF inf Inf inf .0e e} [regsub -all {(e[+-])0+} $r {\1}]
}

proc do_realnum_test {name cmd expected} {
  uplevel [list do_test $name [subst -nocommands { realnum_normalize [ $cmd ] }] [realnum_normalize $expected]]
}

proc filepath_normalize {p} {
  set p
}

proc do_filepath_test {name cmd expected} {
  uplevel [list do_test $name $cmd $expected]
}

proc do_eqp_test {name sql res} {
  if {[string index $res 0]!="/"} {
    set res "/*$res*/"
  }
  uplevel do_execsql_test [list $name] [list "EXPLAIN QUERY PLAN $sql"] [list $res]
}

proc do_eqp_execsql_test {name sql res1 res2} {
  do_eqp_test ${name}a $sql $res1
  uplevel do_execsql_test [list ${name}b] [list $sql] [list $res2]
}

proc do_select_tests {prefix args} {
  set testlist [lindex $args end]
  set switches [lrange $args 0 end-1]
  set errfmt ""
  set countonly 0
  set tclquery ""
  set repair ""
  for {set i 0} {$i < [llength $switches]} {incr i} {
    set s [lindex $switches $i]
    if {[string match "-q*" $s]} {
      set tclquery [list execsql [lindex $switches [incr i]]]
    } elseif {[string match "-t*" $s]} {
      set tclquery [lindex $switches [incr i]]
    } elseif {[string match "-e*" $s]} {
      set errfmt [lindex $switches [incr i]]
    } elseif {[string match "-r*" $s]} {
      set repair [lindex $switches [incr i]]
    } elseif {[string match "-c*" $s]} {
      set countonly 1
    }
  }
  eval $repair
  foreach {tn sql res} $testlist {
    if {$tclquery != ""} {
      execsql $sql
      uplevel do_test ${prefix}.$tn [list $tclquery] [list [list {*}$res]]
    } elseif {$countonly} {
      set nRow 0
      db eval $sql {incr nRow}
      uplevel do_test ${prefix}.$tn [list [list set {} $nRow]] [list $res]
    } elseif {$errfmt==""} {
      uplevel do_execsql_test ${prefix}.${tn} [list $sql] [list [list {*}$res]]
    } else {
      set res [list 1 [string trim [format $errfmt {*}$res]]]
      uplevel do_catchsql_test ${prefix}.${tn} [list $sql] [list $res]
    }
    eval $repair
  }
}

proc integrity_check {name {db db}} {
  ifcapable integrityck {
    do_test $name [list execsql {PRAGMA integrity_check} $db] {ok}
  }
}

proc forcedelete {args} {
  foreach f $args {
    file delete -force -- $f
  }
}

proc delete_file {args} {
  foreach f $args {
    file delete -force -- $f
  }
}

proc copy_file {from to} {
  file copy -force -- $from $to
}

proc forcecopy {from to} {
  file copy -force -- $from $to
}

proc db_save {} {
  foreach f [glob -nocomplain sv_test.db*] { forcedelete $f }
  foreach f [glob -nocomplain test.db*] {
    forcecopy $f "sv_$f"
  }
}

proc db_save_and_close {} {
  db_save
  catch { db close }
  return ""
}

proc db_restore {} {
  foreach f [glob -nocomplain test.db*] { forcedelete $f }
  foreach f2 [glob -nocomplain sv_test.db*] {
    set f [string range $f2 3 end]
    forcecopy $f2 $f
  }
}

proc db_restore_and_reopen {{dbfile test.db}} {
  catch { db close }
  db_restore
  sqlite3 db $dbfile
}

proc db_delete_and_reopen {{file test.db}} {
  catch { db close }
  foreach f [glob -nocomplain test.db*] { forcedelete $f }
  sqlite3 db $file
}

proc drop_all_tables {{db db}} {
  set pk [$db one {PRAGMA foreign_keys}]
  $db eval {PRAGMA foreign_keys = OFF}
  foreach tbl [$db eval {SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'}] {
    $db eval "DROP TABLE \"$tbl\""
  }
  $db eval "PRAGMA foreign_keys = $pk"
}

proc delete_all_data {} {
  foreach tbl [db eval {SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'}] {
    db eval "DELETE FROM \"$tbl\""
  }
}

proc stepsql {dbptr sql} {
  set sql [string trim $sql]
  set r 0
  while {[string length $sql]>0} {
    if {[catch {sqlite3_prepare $dbptr $sql -1 sqltail} vm]} {
      return [list 1 $vm]
    }
    set sql [string trim $sqltail]
    while {[sqlite3_step $vm]=="SQLITE_ROW"} {
      for {set i 0} {$i<[sqlite3_data_count $vm]} {incr i} {
        lappend r [sqlite3_column_text $vm $i]
      }
    }
    if {[catch {sqlite3_finalize $vm} errmsg]} {
      return [list 1 $errmsg]
    }
    if {$errmsg ne "SQLITE_OK"} {
      return [list 1 [sqlite3_errmsg $dbptr]]
    }
  }
  return $r
}

proc permutation {} { return "" }
proc presql {} { return "" }
proc isquick {} { return 0 }
proc omit_test {args} {}
proc warning {args} {}
proc verify_ex_errcode {name expected {db db}} {
  do_test $name [list sqlite3_extended_errcode $db] $expected
}

proc do_not_use_codec {} {}
proc wal_is_wal_mode {} {
  expr {[permutation] eq "wal"}
}
proc wal_set_journal_mode {{db db}} {
  if { [wal_is_wal_mode] } {
    $db eval "PRAGMA journal_mode = WAL"
  }
}
proc wal_check_journal_mode {testname {db db}} {
  if { [wal_is_wal_mode] } {
    $db eval { SELECT * FROM sqlite_master }
    do_test $testname [list $db eval "PRAGMA main.journal_mode"] {wal}
  }
}
proc wal_is_capable {} {
  ifcapable !wal { return 0 }
  if {[permutation]=="journaltest"} { return 0 }
  return 1
}

proc explain_no_trace {sql} {
  set tr [db eval "EXPLAIN $sql"]
  return [lrange $tr 7 end]
}

proc do_vmstep_test {tn sql nstep {res {}}} {
  uplevel [list do_execsql_test $tn.0 $sql $res]
}

proc drop_all_indexes {{db db}} {
  foreach idx [$db eval {SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'}] {
    $db eval "DROP INDEX \"$idx\""
  }
}

proc cksum {{db db}} {
  set txt [$db eval {SELECT name, type, sql FROM sqlite_master ORDER BY name}]\n
  foreach tbl [$db eval {SELECT name FROM sqlite_master WHERE type='table' ORDER BY name}] {
    append txt [$db eval "SELECT * FROM \"$tbl\""]\n
  }
  return [md5 $txt]
}

proc allcksum {{db db}} {
  return [cksum $db]
}

proc dbcksum {db dbname} {
  return [cksum $db]
}

proc test_set_config_pagecache {sz nPg} {
  reset_db
}

proc test_restore_config_pagecache {} {
  reset_db
}

proc do_faultsim_test {args} {}
proc do_malloc_test {args} {}
proc do_ioerr_test {args} {}
"#;
