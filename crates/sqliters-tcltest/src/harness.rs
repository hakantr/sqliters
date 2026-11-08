//! SQLite `testfixture` + `tester.tcl` host environment bound to `sqliters_core::Connection`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters_core::{Connection, StepResult};
use sqliters_types::{ErrorCode, Value, format_sqlite_real};
use sqliters_vfs::OpenFlags;

use crate::expr::{eval_expr, parse_tcl_float, parse_tcl_int};
use crate::interp::{TclControl, TclInterp, TclResult};
use crate::list::{format_list, fpnum_compare, md5_hex, parse_list};
use crate::regex::{Regex, glob_match};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

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
}

impl TestFileReport {
    pub fn is_ok(&self) -> bool {
        self.failed.is_empty() && self.script_error.is_none() && (self.passed > 0 || self.finished)
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

struct DbHandle {
    conn: Connection,
    nullvalue: String,
    last_errcode: i32,
    last_extended_errcode: i32,
    last_errmsg: String,
}

struct PreparedStmtEntry {
    rows: Vec<Vec<String>>,
    col_names: Vec<String>,
    cursor: Option<usize>,
    last_err: Option<String>,
}

pub struct SqliteTestHost {
    dbs: HashMap<String, DbHandle>,
    stmts: HashMap<String, PreparedStmtEntry>,
    next_stmt_id: usize,
    pub report: TestFileReport,
    pub sqlite_test_dir: PathBuf,
    prng_state: u64,
}

pub type SharedHost = Rc<RefCell<SqliteTestHost>>;

impl SqliteTestHost {
    pub fn new_shared(file_name: &str, sqlite_test_dir: PathBuf) -> SharedHost {
        Rc::new(RefCell::new(Self {
            dbs: HashMap::new(),
            stmts: HashMap::new(),
            next_stmt_id: 1,
            report: TestFileReport {
                file_name: file_name.to_string(),
                passed: 0,
                failed: Vec::new(),
                script_error: None,
                finished: false,
            },
            sqlite_test_dir,
            prng_state: 0x1234_5678_9abc_def0,
        }))
    }

    pub fn has_db(&self, name: &str) -> bool {
        self.dbs.contains_key(name)
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

pub fn bootstrap_tester(
    interp: &mut TclInterp,
    host: &SharedHost,
    temp_dir: &Path,
) -> TclResult<()> {
    let (test_dir_str, file_name) = {
        let h = host.borrow();
        (
            h.sqlite_test_dir.to_string_lossy().into_owned(),
            h.report.file_name.clone(),
        )
    };
    let _ = interp.set_var("testdir", test_dir_str);
    let _ = interp.set_var("argv0", file_name);
    let _ = interp.set_var("argv", String::new());
    let _ = interp.set_var("AUTOVACUUM", "0".to_string());
    let _ = interp.set_var("sqlite_open_file_count", "0".to_string());
    let _ = interp.set_var("sqlite_search_count", "0".to_string());
    let _ = interp.set_var("sqlite_like_count", "0".to_string());
    let _ = interp.set_var("sqlite_sort_count", "0".to_string());
    let _ = interp.set_var("sqlite_xferopt_count", "0".to_string());
    let _ = interp.set_var("sqlite_opentemp_count", "0".to_string());
    let _ = interp.set_var("sqlite_current_time", "0".to_string());
    let _ = interp.set_var("DB", "db".to_string());
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

    let enabled_caps = [
        "altertable",
        "analyze",
        "attach",
        "auth",
        "autoinc",
        "autovacuum",
        "between_opt",
        "bitvec_test",
        "bloblit",
        "bounded_vars",
        "case_sensitive_like",
        "cast",
        "check",
        "columnmetadata",
        "complete",
        "compound",
        "conflict",
        "cte",
        "datetime",
        "default_autovacuum",
        "deprecated",
        "descidx",
        "diskio",
        "empty_result_callbacks",
        "explain",
        "expr_depth",
        "floatingpoint",
        "foreignkey",
        "fts3",
        "fts5",
        "generated",
        "get_table",
        "incrblob",
        "integrityck",
        "json",
        "like_opt",
        "load_ext",
        "localtime",
        "lock_proxy_pragmas",
        "lookaside",
        "memorydb",
        "memorymanage",
        "mergesort",
        "or_opt",
        "offset_sql_func",
        "pager_pragmas",
        "pragma",
        "printf",
        "progress",
        "reindex",
        "returning",
        "rowvalue",
        "rtree",
        "savepoint",
        "schema_pragmas",
        "schema_version",
        "shared_cache",
        "subquery",
        "tclvar",
        "tempdb",
        "threadsafe",
        "trace",
        "trigger",
        "truncate_opt",
        "upsert",
        "utf16",
        "vacuum",
        "view",
        "vtab",
        "wal",
        "windowfunc",
        "xfer_opt",
    ];
    for cap in enabled_caps {
        let val = if cap == "default_autovacuum" || cap == "case_sensitive_like" {
            "0"
        } else {
            "1"
        };
        interp.set_array_entry("sqlite_options", cap, val.to_string());
    }
    for disabled_cap in [
        "icu",
        "stat4",
        "mem5",
        "codec",
        "casesensitivelike",
        "atomicwrite",
        "proxy_locking",
        "unlock_notify",
        "update_delete_limit",
    ] {
        interp.set_array_entry("sqlite_options", disabled_cap, "0".to_string());
    }

    let db_path = temp_dir.join("test.db");
    let _ = fs::remove_file(&db_path);
    let conn = Connection::open(&db_path).map_err(|e| TclControl::Error(sqlite_err_text(&e)))?;
    host.borrow_mut().dbs.insert(
        "db".to_string(),
        DbHandle {
            conn,
            nullvalue: String::new(),
            last_errcode: 0,
            last_extended_errcode: 0,
            last_errmsg: "not an error".to_string(),
        },
    );

    interp.eval(TESTER_TCL_PRELUDE)?;
    Ok(())
}

/// Dispatches SQLite `testfixture` / `tester.tcl` host commands in a fully re-entrant way
/// (never holding a `RefCell` borrow across `interp.eval(...)`).
pub fn invoke_host_cmd(
    host: &SharedHost,
    interp: &mut TclInterp,
    cmd: &str,
    args: &[String],
) -> Option<TclResult<String>> {
    if host.borrow().has_db(cmd) {
        return Some(dispatch_db_method(host, interp, cmd, args));
    }

    match cmd {
        "sqlite3" => Some(open_db_cmd(host, interp, args)),
        "do_test" => Some(cmd_do_test(host, interp, args)),
        "source" => Some(cmd_source(host, interp, args)),
        "ifcapable" => Some(cmd_ifcapable(interp, args)),
        "capable" => Some(
            eval_capable_expr(interp, args.first().map_or("1", String::as_str))
                .map(|b| if b { "1" } else { "0" }.to_string()),
        ),
        "reset_db" => Some(cmd_reset_db(host, interp)),
        "finish_test" => {
            host.borrow_mut().report.finished = true;
            Some(Err(TclControl::FinishTest))
        }
        "sqlite_complete" | "sqlite3_complete" => {
            let sql = args.first().map_or("", String::as_str);
            Some(Ok(if sqliters_syntax::is_complete(sql) {
                "1"
            } else {
                "0"
            }
            .to_string()))
        }
        "working_64bit_int" => Some(Ok("1".to_string())),
        "fpnum_compare" => {
            let a = args.first().map_or("", String::as_str);
            let b = args.get(1).map_or("", String::as_str);
            Some(Ok(if fpnum_compare(a, b) { "1" } else { "0" }.to_string()))
        }
        "randstr" => Some(cmd_randstr(host, args)),
        "hexio_read" => Some(cmd_hexio_read(interp, args)),
        "hexio_write" => Some(cmd_hexio_write(interp, args)),
        "hexio_get_int" => {
            let hex = args.first().map_or("0", String::as_str);
            let val = i64::from_str_radix(hex.trim(), 16).unwrap_or(0);
            Some(Ok(val.to_string()))
        }
        "hexio_render_int32" => {
            let v = args
                .first()
                .and_then(|s| parse_tcl_int(s.trim()))
                .unwrap_or(0) as u32;
            Some(Ok(format!("{v:08X}")))
        }
        "hexio_render_int16" => {
            let v = args
                .first()
                .and_then(|s| parse_tcl_int(s.trim()))
                .unwrap_or(0) as u16;
            Some(Ok(format!("{v:04X}")))
        }
        "md5" => {
            let s = args.first().map_or("", String::as_str);
            Some(Ok(md5_hex(s.as_bytes())))
        }
        "md5sum" => {
            let mut combined = String::new();
            let dbn = args.first().map_or("db", String::as_str);
            for tbl in &args[1..] {
                let sql = format!("SELECT * FROM \"{tbl}\"");
                if let Ok(rows) = exec_sql_rows(host, interp, dbn, &sql) {
                    for (r, _) in rows {
                        for v in r {
                            combined.push_str(&v);
                            combined.push('\n');
                        }
                    }
                }
            }
            Some(Ok(md5_hex(combined.as_bytes())))
        }
        "strftime" => {
            // C test1.c `strftime FMT SEC` via SQLite's own `strftime(FMT, SEC, 'unixepoch')`
            let fmt = args.first().map_or("%Y-%m-%d %H:%M:%S", String::as_str);
            let sec = args.get(1).map_or("0", String::as_str);
            let sql = format!(
                "SELECT strftime('{}', {}, 'unixepoch')",
                fmt.replace('\'', "''"),
                parse_tcl_int(sec).unwrap_or(0)
            );
            if let Ok(mut conn) = Connection::open_in_memory()
                && let Ok(Some(row)) = conn.query_row(&sql, &[])
            {
                return Some(Ok(value_to_tcl_str(&row[0], "")));
            }
            Some(Ok(String::new()))
        }
        "md5file" => {
            let p = interp.resolve_path(args.first().map_or("", String::as_str));
            let data = fs::read(p).unwrap_or_default();
            Some(Ok(md5_hex(&data)))
        }
        "sqlite3_connection_pointer" => Some(Ok(args
            .first()
            .cloned()
            .unwrap_or_else(|| "db".to_string()))),
        "sqlite3_get_autocommit" => {
            let dbn = args.first().map_or("db", String::as_str);
            let ac = host
                .borrow()
                .dbs
                .get(dbn)
                .map(|h| i32::from(h.conn.is_autocommit()))
                .unwrap_or(1);
            Some(Ok(ac.to_string()))
        }
        "sqlite3_txn_state" => {
            let dbn = args.first().map_or("db", String::as_str);
            let st = host
                .borrow()
                .dbs
                .get(dbn)
                .map(|h| if h.conn.is_autocommit() { 0 } else { 2 })
                .unwrap_or(0);
            Some(Ok(st.to_string()))
        }
        "sqlite3_errcode" => {
            let dbn = args.first().map_or("db", String::as_str);
            let code = host.borrow().dbs.get(dbn).map_or(0, |h| h.last_errcode);
            Some(Ok(map_errcode_name(code)))
        }
        "sqlite3_extended_errcode" => {
            let dbn = args.first().map_or("db", String::as_str);
            let code = host
                .borrow()
                .dbs
                .get(dbn)
                .map_or(0, |h| h.last_extended_errcode);
            Some(Ok(map_extended_errcode_name(code)))
        }
        "sqlite3_errmsg" => {
            let dbn = args.first().map_or("db", String::as_str);
            let msg = host
                .borrow()
                .dbs
                .get(dbn)
                .map(|h| h.last_errmsg.clone())
                .unwrap_or_else(|| "not an error".to_string());
            Some(Ok(msg))
        }
        "sqlite3_changes" => {
            let dbn = args.first().map_or("db", String::as_str);
            let n = host.borrow().dbs.get(dbn).map_or(0, |h| h.conn.changes());
            Some(Ok(n.to_string()))
        }
        "sqlite3_total_changes" => {
            let dbn = args.first().map_or("db", String::as_str);
            let n = host
                .borrow()
                .dbs
                .get(dbn)
                .map_or(0, |h| h.conn.total_changes());
            Some(Ok(n.to_string()))
        }
        "sqlite3_last_insert_rowid" => {
            let dbn = args.first().map_or("db", String::as_str);
            let id = host
                .borrow()
                .dbs
                .get(dbn)
                .map_or(0, |h| h.conn.last_insert_rowid());
            Some(Ok(id.to_string()))
        }
        "sqlite3_exec" => {
            let dbn = args.first().map_or("db", String::as_str);
            let sql = args.get(1).map_or("", String::as_str);
            match exec_sql_rows(host, interp, dbn, sql) {
                Ok(rows) => {
                    let mut flat = Vec::new();
                    for (r, _) in rows {
                        flat.extend(r);
                    }
                    Some(Ok(format_list(&["0".to_string(), format_list(&flat)])))
                }
                Err(e) => Some(Ok(format_list(&["1".to_string(), e.err_msg()]))),
            }
        }
        "sqlite3_prepare" | "sqlite3_prepare_v2" => Some(cmd_prepare(host, interp, args)),
        "sqlite3_step" => Some(cmd_step(host, args)),
        "sqlite3_data_count" | "sqlite3_column_count" => Some(cmd_col_count(host, args)),
        "sqlite3_column_text" => Some(cmd_col_text(host, args)),
        "sqlite3_reset" => Some(cmd_stmt_reset(host, args)),
        "sqlite3_finalize" => Some(cmd_stmt_finalize(host, args)),
        "sqlite3_db_config"
        | "sqlite3_db_config_lookaside"
        | "sqlite3_test_control"
        | "sqlite3_test_control_pending_byte"
        | "sqlite3_limit"
        | "sqlite3_soft_heap_limit"
        | "sqlite3_soft_heap_limit64"
        | "sqlite3_hard_heap_limit64"
        | "sqlite3_release_memory"
        | "sqlite3_enable_shared_cache"
        | "sqlite3_config_memstatus"
        | "sqlite3_memdebug_settitle"
        | "sqlite3_shutdown"
        | "sqlite3_initialize"
        | "autoinstall_test_functions"
        | "install_malloc_faultsim"
        | "database_never_corrupt"
        | "database_may_be_corrupt"
        | "extra_schema_checks"
        | "reset_prng_state"
        | "add_alignment_test_functions"
        | "register_dbstat_vtab"
        | "register_echo_module"
        | "register_tcl_module"
        | "load_static_extension"
        | "sqlite3_create_function"
        | "sqlite3_create_function_v2"
        | "sqlite3_create_collation_v2"
        | "sqlite3_rekey"
        | "sqlite3_key"
        | "optimization_control"
        | "breakpoint"
        | "clang_sanitize_address"
        | "atomic_batch_write"
        | "tcl_variable_type"
        | "translate_selftest"
        | "testvfs"
        | "sqlite3_db_status" => Some(Ok("0".to_string())),
        _ => None,
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
    let raw_path = args.get(1).map_or(":memory:", String::as_str);

    let mut readonly = false;
    let mut create = true;
    let mut i = 2;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "-readonly" => readonly = parse_tcl_int(&args[i + 1]).unwrap_or(0) != 0,
            "-create" => create = parse_tcl_int(&args[i + 1]).unwrap_or(1) != 0,
            _ => {}
        }
        i += 2;
    }

    host.borrow_mut().dbs.remove(&handle_name);

    let conn = if raw_path == ":memory:" || raw_path.is_empty() {
        Connection::open_in_memory().map_err(|e| TclControl::Error(sqlite_err_text(&e)))?
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
        Connection::open_with_flags(resolved, flags)
            .map_err(|e| TclControl::Error(sqlite_err_text(&e)))?
    };

    host.borrow_mut().dbs.insert(
        handle_name,
        DbHandle {
            conn,
            nullvalue: String::new(),
            last_errcode: 0,
            last_extended_errcode: 0,
            last_errmsg: "not an error".to_string(),
        },
    );
    Ok(String::new())
}

fn dispatch_db_method(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_name: &str,
    args: &[String],
) -> TclResult<String> {
    if args.is_empty() {
        return Err(TclControl::Error(format!("wrong # args to {db_name}")));
    }
    let sub = args[0].as_str();
    match sub {
        "close" => {
            host.borrow_mut().dbs.remove(db_name);
            Ok(String::new())
        }
        "nullvalue" | "null" => {
            let mut h = host.borrow_mut();
            let db = h
                .dbs
                .get_mut(db_name)
                .ok_or_else(|| TclControl::Error(format!("unknown db {db_name}")))?;
            if let Some(nv) = args.get(1) {
                db.nullvalue = nv.clone();
            }
            Ok(db.nullvalue.clone())
        }
        "changes" => Ok(host.borrow().dbs[db_name].conn.changes().to_string()),
        "total_changes" => Ok(host.borrow().dbs[db_name].conn.total_changes().to_string()),
        "last_insert_rowid" => Ok(host.borrow().dbs[db_name]
            .conn
            .last_insert_rowid()
            .to_string()),
        "errorcode" => Ok(host.borrow().dbs[db_name].last_errcode.to_string()),
        "version" => Ok(sqliters_types::SQLITE_VERSION.to_string()),
        "eval" => db_eval(host, interp, db_name, &args[1..]),
        "one" => {
            let sql = args.get(1).map_or("", String::as_str);
            let rows = exec_sql_rows(host, interp, db_name, sql)?;
            if let Some((first_row, _)) = rows.first() {
                Ok(first_row.first().cloned().unwrap_or_default())
            } else {
                Ok(String::new())
            }
        }
        "exists" => {
            let sql = args.get(1).map_or("", String::as_str);
            let rows = exec_sql_rows(host, interp, db_name, sql)?;
            Ok(if rows.is_empty() { "0" } else { "1" }.to_string())
        }
        "transaction" => {
            let (mode, script) = if args.len() >= 3 {
                (args[1].as_str(), &args[2])
            } else if args.len() == 2 {
                ("deferred", &args[1])
            } else {
                return Err(TclControl::Error(
                    "wrong # args to db transaction".to_string(),
                ));
            };
            let begin_sql = match mode.to_ascii_lowercase().as_str() {
                "immediate" => "BEGIN IMMEDIATE",
                "exclusive" => "BEGIN EXCLUSIVE",
                _ => "BEGIN DEFERRED",
            };
            let need_tx = host
                .borrow()
                .dbs
                .get(db_name)
                .map(|h| h.conn.is_autocommit())
                .unwrap_or(false);
            if need_tx {
                let _ = exec_sql_rows(host, interp, db_name, begin_sql)?;
            }
            match interp.eval(script) {
                Ok(v) => {
                    if need_tx {
                        let _ = exec_sql_rows(host, interp, db_name, "COMMIT")?;
                    }
                    Ok(v)
                }
                Err(e) => {
                    if need_tx {
                        let _ = exec_sql_rows(host, interp, db_name, "ROLLBACK");
                    }
                    Err(e)
                }
            }
        }
        "status" => Ok("0".to_string()),
        "cache"
        | "config"
        | "timeout"
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

fn db_eval(
    host: &SharedHost,
    interp: &mut TclInterp,
    db_name: &str,
    args: &[String],
) -> TclResult<String> {
    let mut idx = 0;
    if args.first().map(String::as_str) == Some("-withoutnulls") {
        idx += 1;
    }
    let Some(sql) = args.get(idx) else {
        return Err(TclControl::Error("wrong # args to db eval".to_string()));
    };
    let rest = &args[idx + 1..];
    let rows = exec_sql_rows(host, interp, db_name, sql)?;

    if rest.is_empty() {
        let mut flat = Vec::new();
        for (vals, _) in rows {
            flat.extend(vals);
        }
        return Ok(format_list(&flat));
    }

    let (arr_name_opt, body) = if rest.len() == 1 {
        (None, &rest[0])
    } else {
        let an = if rest[0].is_empty() {
            None
        } else {
            Some(rest[0].as_str())
        };
        (an, &rest[1])
    };

    // Note: `host` is NOT borrowed during `interp.eval(body)`!
    let start_rb = host
        .borrow()
        .dbs
        .get(db_name)
        .map_or(0, |h| h.conn.rollback_count());
    let total_rows = rows.len();
    for (row_idx, (vals, cols)) in rows.into_iter().enumerate() {
        if let Some(arr) = arr_name_opt {
            interp.set_array_entry(arr, "*", format_list(&cols));
            for (c, v) in cols.iter().zip(vals.iter()) {
                interp.set_array_entry(arr, c, v.clone());
            }
        } else {
            for (c, v) in cols.iter().zip(vals.iter()) {
                let _ = interp.set_var(c, v.clone());
            }
        }
        match interp.eval(body) {
            Ok(_) | Err(TclControl::Continue) => {}
            Err(TclControl::Break) => break,
            Err(other) => return Err(other),
        }
        if row_idx + 1 < total_rows {
            let mut h_guard = host.borrow_mut();
            if let Some(handle) = h_guard.dbs.get_mut(db_name)
                && handle.conn.rollback_count() != start_rb
            {
                handle.last_errcode = 4; // SQLITE_ABORT
                handle.last_extended_errcode = 4;
                handle.last_errmsg = "abort due to ROLLBACK".to_string();
                return Err(TclControl::Error("abort due to ROLLBACK".to_string()));
            }
        }
    }
    Ok(String::new())
}

fn exec_sql_rows(
    host: &SharedHost,
    interp: &TclInterp,
    db_name: &str,
    sql: &str,
) -> TclResult<Vec<(Vec<String>, Vec<String>)>> {
    let mut h_guard = host.borrow_mut();
    let Some(handle) = h_guard.dbs.get_mut(db_name) else {
        return Err(TclControl::Error(format!(
            "invalid command name \"{db_name}\""
        )));
    };
    handle.last_errcode = 0;
    handle.last_extended_errcode = 0;
    handle.last_errmsg = "not an error".to_string();

    let mut rem = sql;
    let mut all_rows: Vec<(Vec<String>, Vec<String>)> = Vec::new();

    while !rem.trim().is_empty() {
        let next_tail = {
            let mut stmt = match handle.conn.prepare(rem) {
                Ok(s) => s,
                Err(e) => {
                    handle.last_errcode = e.code as i32;
                    handle.last_extended_errcode = e.extended_code;
                    handle.last_errmsg = sqlite_err_text(&e);
                    return Err(TclControl::Error(handle.last_errmsg.clone()));
                }
            };

            let named_list: Vec<(String, usize)> = stmt
                .named_parameters()
                .iter()
                .map(|(k, &v)| (k.clone(), v))
                .collect();
            for (raw_param, pidx) in named_list {
                let var_lookup = raw_param
                    .strip_prefix('$')
                    .or_else(|| raw_param.strip_prefix(':'))
                    .or_else(|| raw_param.strip_prefix('@'))
                    .unwrap_or(&raw_param);
                if let Ok(tcl_str) = interp.get_var(var_lookup) {
                    let bound_val = tcl_string_to_sqlite_value(&tcl_str);
                    let _ = stmt.bind(pidx, bound_val);
                }
            }

            let mut step_rows = Vec::new();
            loop {
                match stmt.step() {
                    Ok(StepResult::Row(row)) => {
                        let formatted: Vec<String> = row
                            .values()
                            .iter()
                            .map(|v| value_to_tcl_str(v, &handle.nullvalue))
                            .collect();
                        step_rows.push(formatted);
                    }
                    Ok(StepResult::Done) => break,
                    Err(e) => {
                        handle.last_errcode = e.code as i32;
                        handle.last_extended_errcode = e.extended_code;
                        handle.last_errmsg = sqlite_err_text(&e);
                        return Err(TclControl::Error(handle.last_errmsg.clone()));
                    }
                }
            }
            let cols = stmt.column_names().to_vec();
            for r in step_rows {
                all_rows.push((r, cols.clone()));
            }
            stmt.tail().to_string()
        };

        if next_tail.len() >= rem.len() {
            break;
        }
        rem = &sql[sql.len() - next_tail.len()..];
    }

    Ok(all_rows)
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
            let sqlite_test_dir = host.borrow().sqlite_test_dir.clone();
            let candidate = {
                let p = Path::new(target);
                if p.exists() {
                    p.to_path_buf()
                } else {
                    let in_test = sqlite_test_dir.join(&file_tail);
                    if in_test.exists() {
                        in_test
                    } else {
                        interp.resolve_path(target)
                    }
                }
            };
            let script = fs::read_to_string(&candidate).map_err(|e| {
                TclControl::Error(format!("cannot source {}: {e}", candidate.display()))
            })?;
            match interp.eval(&script) {
                Ok(v) => Ok(v),
                Err(TclControl::Return { value, .. }) => Ok(value),
                Err(other) => Err(other),
            }
        }
    }
}

fn cmd_ifcapable(interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() < 2 {
        return Err(TclControl::Error("wrong # args to ifcapable".to_string()));
    }
    let ok = eval_capable_expr(interp, &args[0])?;
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

fn eval_capable_expr(interp: &mut TclInterp, raw: &str) -> TclResult<bool> {
    let mut rewritten = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_alphanumeric() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let key = format!("::sqlite_options({word})");
            let v = interp.get_var(&key).unwrap_or_else(|_| "1".to_string());
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
    host.borrow_mut().dbs.remove("db");
    for suffix in ["test.db", "test.db-journal", "test.db-wal", "test.db-shm"] {
        let p = interp.resolve_path(suffix);
        let _ = fs::remove_file(p);
    }
    let db_path = interp.resolve_path("test.db");
    let conn = Connection::open(db_path).map_err(|e| TclControl::Error(sqlite_err_text(&e)))?;
    host.borrow_mut().dbs.insert(
        "db".to_string(),
        DbHandle {
            conn,
            nullvalue: String::new(),
            last_errcode: 0,
            last_extended_errcode: 0,
            last_errmsg: "not an error".to_string(),
        },
    );
    if let Ok(setup) = interp.get_var("::SETUP_SQL")
        && !setup.trim().is_empty()
    {
        let _ = exec_sql_rows(host, interp, "db", &setup)?;
    }
    Ok(String::new())
}

fn cmd_prepare(host: &SharedHost, interp: &mut TclInterp, args: &[String]) -> TclResult<String> {
    if args.len() < 4 {
        return Err(TclControl::Error(
            "wrong # args to sqlite3_prepare".to_string(),
        ));
    }
    let dbn = &args[0];
    let sql = &args[1];
    let tail_var = &args[3];

    let (tail, cols, rows, step_err) = {
        let mut h = host.borrow_mut();
        let Some(handle) = h.dbs.get_mut(dbn) else {
            return Err(TclControl::Error(format!("no such db {dbn}")));
        };
        match handle.conn.prepare(sql) {
            Ok(mut stmt) => {
                let tail = stmt.tail().to_string();
                let cols = stmt.column_names().to_vec();
                let mut rows = Vec::new();
                let mut step_err = None;
                loop {
                    match stmt.step() {
                        Ok(StepResult::Row(r)) => {
                            rows.push(
                                r.values()
                                    .iter()
                                    .map(|v| value_to_tcl_str(v, &handle.nullvalue))
                                    .collect(),
                            );
                        }
                        Ok(StepResult::Done) => break,
                        Err(e) => {
                            step_err = Some(sqlite_err_text(&e));
                            break;
                        }
                    }
                }
                (tail, cols, rows, step_err)
            }
            Err(e) => {
                handle.last_errcode = e.code as i32;
                handle.last_extended_errcode = e.extended_code;
                handle.last_errmsg = sqlite_err_text(&e);
                return Err(TclControl::Error(format!(
                    "({}) {}",
                    e.code as i32, handle.last_errmsg
                )));
            }
        }
    };

    let _ = interp.set_var(tail_var, tail);
    let mut h = host.borrow_mut();
    let id = format!("stmt{}", h.next_stmt_id);
    h.next_stmt_id += 1;
    h.stmts.insert(
        id.clone(),
        PreparedStmtEntry {
            rows,
            col_names: cols,
            cursor: None,
            last_err: step_err,
        },
    );
    Ok(id)
}

fn cmd_step(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    let mut h = host.borrow_mut();
    let Some(st) = h.stmts.get_mut(id) else {
        return Ok("SQLITE_MISUSE".to_string());
    };
    let next_idx = st.cursor.map_or(0, |c| c + 1);
    st.cursor = Some(next_idx);
    if next_idx < st.rows.len() {
        Ok("SQLITE_ROW".to_string())
    } else if st.last_err.is_some() {
        Ok("SQLITE_ERROR".to_string())
    } else {
        Ok("SQLITE_DONE".to_string())
    }
}

fn cmd_col_count(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    let h = host.borrow();
    let cnt = h.stmts.get(id).map_or(0, |st| {
        if let Some(c) = st.cursor {
            st.rows.get(c).map_or(st.col_names.len(), Vec::len)
        } else {
            st.col_names.len()
        }
    });
    Ok(cnt.to_string())
}

fn cmd_col_text(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    let col = args
        .get(1)
        .and_then(|s| parse_tcl_int(s))
        .unwrap_or(0)
        .max(0) as usize;
    let h = host.borrow();
    let val = h
        .stmts
        .get(id)
        .and_then(|st| st.cursor.and_then(|c| st.rows.get(c)))
        .and_then(|r| r.get(col))
        .cloned()
        .unwrap_or_default();
    Ok(val)
}

fn cmd_stmt_reset(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    let mut h = host.borrow_mut();
    if let Some(st) = h.stmts.get_mut(id) {
        st.cursor = None;
        if let Some(ref err) = st.last_err {
            return Err(TclControl::Error(err.clone()));
        }
    }
    Ok("SQLITE_OK".to_string())
}

fn cmd_stmt_finalize(host: &SharedHost, args: &[String]) -> TclResult<String> {
    let id = args.first().map_or("", String::as_str);
    let mut h = host.borrow_mut();
    if let Some(st) = h.stmts.remove(id)
        && let Some(err) = st.last_err
    {
        return Err(TclControl::Error(err));
    }
    Ok("SQLITE_OK".to_string())
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

fn sqlite_err_text(e: &sqliters_types::SqliteError) -> String {
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

fn map_errcode_name(code: i32) -> String {
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
        18 => "SQLITE_TOOBIG",
        19 => "SQLITE_CONSTRAINT",
        20 => "SQLITE_MISMATCH",
        21 => "SQLITE_MISUSE",
        25 => "SQLITE_RANGE",
        26 => "SQLITE_NOTADB",
        _ => {
            let _ = ErrorCode::Error;
            "SQLITE_ERROR"
        }
    }
    .to_string()
}

fn map_extended_errcode_name(code: i32) -> String {
    use sqliters_types::extended;
    match code {
        extended::SQLITE_CONSTRAINT_CHECK => "SQLITE_CONSTRAINT_CHECK".to_string(),
        extended::SQLITE_CONSTRAINT_FOREIGNKEY => "SQLITE_CONSTRAINT_FOREIGNKEY".to_string(),
        extended::SQLITE_CONSTRAINT_NOTNULL => "SQLITE_CONSTRAINT_NOTNULL".to_string(),
        extended::SQLITE_CONSTRAINT_PRIMARYKEY => "SQLITE_CONSTRAINT_PRIMARYKEY".to_string(),
        extended::SQLITE_CONSTRAINT_UNIQUE => "SQLITE_CONSTRAINT_UNIQUE".to_string(),
        extended::SQLITE_CONSTRAINT_ROWID => "SQLITE_CONSTRAINT_ROWID".to_string(),
        extended::SQLITE_CONSTRAINT_DATATYPE => "SQLITE_CONSTRAINT_DATATYPE".to_string(),
        extended::SQLITE_READONLY_ROLLBACK => "SQLITE_READONLY_ROLLBACK".to_string(),
        other => map_errcode_name(other & 0xff),
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
  do_test $name [list execsql {PRAGMA integrity_check} $db] {ok}
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
proc wal_is_wal_mode {} { return 0 }
proc wal_set_journal_mode {{db db}} {}
proc wal_check_journal_mode {testname {db db}} {}
proc wal_is_capable {} { return 0 }

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
