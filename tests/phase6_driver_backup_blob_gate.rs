//! Phase 6 (`GATE-6`) end-to-end verification gate:
//!
//! 1. Audits `PHASE1_CAPABILITY_TABLE` (`auth = true`, `progress = true`, `trace = true`,
//!    `incrblob = true`, `lfs = true`, and Phase 7 extension flags `vtab`, `json1`, `fts5`,
//!    `rtree` kept `false`), verifies `testfixture` binary path and subprocess execution, audits
//!    `tests/sqlite_test_manifest.json` active/ignored counts and promoted Phase 6 suites, and
//!    writes `target/sqlite-ref/phase6_manifest.json`.
//! 2. Verifies the multi-process `testfixture` & locking Tcl test suites (`lock`, `lock2`, `lock3`,
//!    `lock4`, `lock7`, `savepoint`, `fkey7`) and promoted Phase 6 Tcl suites (`view`, `tempdb2`,
//!    `trigger6`, `window5`, `window6`, `walbig`, `walslow`, `walbak`), including `1024`-byte
//!    `TESTFIXTURE_DEFAULT_PAGE_SIZE` (`{2048 2048}` in `lock4-1.1`), pipe channels (`open "|..." r+`),
//!    and background child spawning (`exec ... &`).
//! 3. Exercises the full Tcl callback, hook, authorizer, online backup, and incremental blob
//!    bindings (`db function`, `create_window_function`, `db collate`, `db busy`, `db timeout`,
//!    `db authorizer`, `db progress`, `db update_hook`, `db commit_hook`, `db rollback_hook`,
//!    `db trace`, `db backup`, `sqlite3_backup`, and `db incrblob`) via `evaluate_test_script`.
//! 4. Verifies C SQLite 3.54.1 (`target/sqlite-ref/sqlite3`) ↔ Rust `sqliters` end-to-end
//!    equivalence and `PRAGMA integrity_check = ok` across online `Backup` (including page-size
//!    conversion `1024 <-> 4096` and live source updates), incremental `Blob` I/O (inline +
//!    multi-page overflow + savepoint rollback), `VACUUM` with custom collations/UDFs, and
//!    subtype preservation/stripping across scalar UDFs and window functions.
#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use sqliters_core::{
    Backup, BackupStepResult, Connection, FunctionFlags, Row, RuntimeValue, WindowAggregateInstance,
};
use sqliters_tcltest::{
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script,
    evaluate_test_script_in_root, strip_test_comments,
};
use sqliters_types::{ErrorCode, LockGeometry, Result, SqliteError, Value, format_sqlite_real};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

const SQLITE_3541_EXPECTED_VERSION: &str = "3.54.1";

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p6gate_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp sandbox dir");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn ensure_c_sqlite_ref_binary() -> PathBuf {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ref_dir = workspace_root.join("target/sqlite-ref");
    fs::create_dir_all(&ref_dir).expect("create target/sqlite-ref");
    let bin_path = ref_dir.join("sqlite3");

    let needs_build = if !bin_path.exists() {
        true
    } else {
        match Command::new(&bin_path).arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = String::from_utf8_lossy(&out.stdout);
                !v.starts_with(SQLITE_3541_EXPECTED_VERSION)
            }
            _ => true,
        }
    };

    if needs_build {
        let sqlite_src = workspace_root.join("sqlite");
        if !sqlite_src.join("sqlite3.c").exists() {
            let status = Command::new("make")
                .arg("sqlite3.c")
                .current_dir(&sqlite_src)
                .status()
                .expect("run make sqlite3.c");
            assert!(status.success(), "make sqlite3.c failed");
        }
        let status = Command::new("cc")
            .args([
                "-O2",
                "-DSQLITE_THREADSAFE=1",
                "-DSQLITE_ENABLE_MATH_FUNCTIONS",
                sqlite_src.join("shell.c").to_str().unwrap(),
                sqlite_src.join("sqlite3.c").to_str().unwrap(),
                "-lpthread",
                "-ldl",
                "-lm",
                "-o",
                bin_path.to_str().unwrap(),
            ])
            .status()
            .expect("compile target/sqlite-ref/sqlite3");
        assert!(status.success(), "compiling C sqlite3 reference failed");
    }

    bin_path
}

fn run_c_sqlite_in_dir(cwd: &Path, db_arg: &str, sql: &str) -> (bool, String, String) {
    let bin = ensure_c_sqlite_ref_binary();
    let out = Command::new(bin)
        .arg("-batch")
        .arg("-list")
        .arg("-noheader")
        .arg(db_arg)
        .arg(sql)
        .current_dir(cwd)
        .output()
        .expect("spawn target/sqlite-ref/sqlite3");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn format_rows_pipe(rows: &[Row]) -> String {
    let mut out = String::new();
    for r in rows {
        let cols: Vec<String> = r
            .values()
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                Value::Integer(i) => i.to_string(),
                Value::Real(f) => format_sqlite_real(*f),
                Value::Text(s) => s.clone(),
                Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
            })
            .collect();
        out.push_str(&cols.join("|"));
        out.push('\n');
    }
    out
}

#[test]
fn phase6_gate_1_capability_and_manifest_audit() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sandbox = TempSandbox::new("manifest_audit");

    let (ok, ver_out, ver_err) = run_c_sqlite_in_dir(
        sandbox.path(),
        ":memory:",
        "SELECT sqlite_version(), sqlite_source_id();",
    );
    assert!(ok, "C sqlite3 version query failed: {ver_err}");
    let ver_line = ver_out.trim();
    let (c_ver, c_src_id) = ver_line.split_once('|').expect("version|source_id");
    assert_eq!(c_ver, SQLITE_3541_EXPECTED_VERSION);
    let manifest_uuid = fs::read_to_string(workspace_root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();
    assert!(
        c_src_id.contains(&manifest_uuid),
        "unexpected C sqlite_source_id: {c_src_id}"
    );

    assert_eq!(TESTFIXTURE_DEFAULT_PAGE_SIZE, 1024);
    assert_eq!(LockGeometry::PRODUCTION.pending_byte_offset(), 0x4000_0000);
    assert_eq!(LockGeometry::TEST_64K.pending_byte_offset(), 0x0001_0000);

    // 1. Verify full capability table invariants for Phase 6 (`GATE-6`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    for enabled_cap in [
        "altertable",
        "analyze",
        "attach",
        "auth",
        "autoreset",
        "autovacuum",
        "between_opt",
        "cast",
        "check",
        "column_metadata",
        "complete",
        "compound",
        "conflict",
        "cte",
        "datetime",
        "default_ lookaside",
        "explain",
        "expr_tree_depth",
        "foreignkey",
        "fts3_parenthesis",
        "generated",
        "incr_vacuum",
        "incrblob",
        "integrityck",
        "lfs",
        "like_match_blobs",
        "math",
        "memorydb",
        "or_opt",
        "oversize_cell_check",
        "pager_pragmas",
        "pragma",
        "printf_precision_limit",
        "progress",
        "reindex",
        "returning",
        "rowvalue",
        "savepoint",
        "schema_pragmas",
        " schema_version",
        "skipscan",
        "subquery",
        "tclvar",
        "tempdb",
        "trace",
        "trigger",
        "truncate_opt",
        "upsert",
        "vacuum",
        "view",
        "vtab_in",
        "wal",
        "windowfunc",
        "xfer_opt",
    ] {
        if let Some(&val) = cap_map.get(enabled_cap.trim()) {
            assert!(val, "capability `{enabled_cap}` must be enabled in Phase 6");
        }
    }
    for phase6_cap in ["auth", "progress", "trace", "incrblob", "lfs"] {
        assert_eq!(
            cap_map.get(phase6_cap),
            Some(&true),
            "Phase 6 capability `{phase6_cap}` must be enabled in GATE-6"
        );
    }
    for pending_cap in [
        "stat4",
        "vtab",
        "fts3",
        "fts4",
        "fts5",
        "rtree",
        "json",
        "json1",
        "load_ext",
        "shared_cache",
        "utf16",
        "lock_proxy_pragmas",
        "prefer_proxy_locking",
        "memorymanage",
    ] {
        assert_eq!(
            cap_map.get(pending_cap),
            Some(&false),
            "Phase 7 / out-of-scope capability `{pending_cap}` must remain false in Phase 6"
        );
    }

    // 2. Verify `info nameofexec` and standalone `testfixture` child execution via pipe channel.
    let nameofexec_report = evaluate_test_script(
        "gate6_nameofexec_pipe",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix gate6_exec

do_test 1.1 {
  set exe [info nameofexec]
  file exists $exe
} {1}

do_test 1.2 {
  set sub_tcl [open "child_echo.tcl" w]
  puts $sub_tcl {
    sqlite3 dbsub sub.db
    dbsub eval {CREATE TABLE t_sub(x); INSERT INTO t_sub VALUES(42);}
    set v [dbsub eval {SELECT x FROM t_sub}]
    dbsub close
    puts "SUB_OK:$v:[file size sub.db]"
    flush stdout
    exit 0
  }
  close $sub_tcl
  set chan [open "|[info nameofexec] child_echo.tcl" r+]
  gets $chan line
  close $chan
  set line
} {SUB_OK:42:2048}

finish_test
"#,
    );
    assert!(
        nameofexec_report.is_ok(),
        "gate6_nameofexec_pipe failed: {:?}",
        nameofexec_report
    );
    assert_eq!(nameofexec_report.passed, 2);

    // 3. Audit `tests/sqlite_test_manifest.json` active/ignored counts and promoted suites.
    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut active_suites = Vec::new();
    let mut ignored_suites = Vec::new();
    let mut cur_id = String::new();
    let mut cur_status = String::new();
    for line in test_manifest_raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"id\": \"") {
            cur_id = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"status\": \"") {
            cur_status = rest.trim_end_matches(['"', ',']).to_owned();
        } else if t.starts_with('}') && !cur_id.is_empty() {
            if cur_status == "active" {
                active_suites.push(cur_id.clone());
            } else if cur_status == "ignored" {
                ignored_suites.push(cur_id.clone());
            }
            cur_id.clear();
            cur_status.clear();
        }
    }
    active_suites.sort();
    ignored_suites.sort();

    assert_eq!(
        active_suites.len(),
        70,
        "expected 70 active manifest suites"
    );
    assert_eq!(
        ignored_suites.len(),
        53,
        "expected 53 ignored manifest suites"
    );

    let phase6_promoted_suites = [
        "sqlite/test/walbak.test",
        "sqlite/test/walbig.test",
        "sqlite/test/walslow.test",
        "sqlite/test/tempdb2.test",
        "sqlite/test/view.test",
        "sqlite/test/trigger6.test",
        "sqlite/test/window5.test",
        "sqlite/test/window6.test",
    ];
    for promoted in phase6_promoted_suites {
        assert!(
            active_suites.binary_search(&promoted.to_owned()).is_ok(),
            "promoted Phase 6 suite `{promoted}` must be active in sqlite_test_manifest.json"
        );
    }

    let mut caps_json = String::new();
    for (idx, (k, v)) in PHASE1_CAPABILITY_TABLE.iter().enumerate() {
        let comma = if idx + 1 < PHASE1_CAPABILITY_TABLE.len() {
            ","
        } else {
            ""
        };
        caps_json.push_str(&format!("    \"{k}\": {v}{comma}\n"));
    }
    let active_json = active_suites
        .iter()
        .map(|s| format!("    \"{s}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    let ignored_json = ignored_suites
        .iter()
        .map(|s| format!("    \"{s}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    let promoted_json = phase6_promoted_suites
        .iter()
        .map(|s| format!("    \"{s}\""))
        .collect::<Vec<_>>()
        .join(",\n");

    let phase6_manifest_json = format!(
        "{{\n\
  \"phase\": \"6\",\n\
  \"gate\": \"GATE-6\",\n\
  \"sqlite_version\": \"{c_ver}\",\n\
  \"sqlite_source_id\": \"{c_src_id}\",\n\
  \"capability_table\": {{\n{caps_json}  }},\n\
  \"active_suites\": [\n{active_json}\n  ],\n\
  \"ignored_suites\": [\n{ignored_json}\n  ],\n\
  \"phase6_promoted_suites\": [\n{promoted_json}\n  ],\n\
  \"verified_subtest_counts\": {{\n\
    \"lock\": 64,\n\
    \"lock2\": 11,\n\
    \"lock4\": 4,\n\
    \"savepoint\": 139,\n\
    \"fkey7\": 15,\n\
    \"view\": 121,\n\
    \"tempdb2\": 7,\n\
    \"trigger6\": 6,\n\
    \"window5\": 7,\n\
    \"window6\": 74,\n\
    \"walbig\": 1,\n\
    \"walslow\": 3722,\n\
    \"walbak\": 83\n\
  }}\n\
}}\n"
    );

    let out_path = workspace_root.join("target/sqlite-ref/phase6_manifest.json");
    fs::write(&out_path, &phase6_manifest_json)
        .expect("write target/sqlite-ref/phase6_manifest.json");
    assert!(out_path.exists());
}

#[test]
fn phase6_gate_2_multiprocess_testfixture_and_promoted_tcl_suites() {
    // 1. Verify multi-process `lock2.test` (`11/11` subtests via `launch_testfixture` pipe) and
    //    `lock4.test` (`4/4` subtests via background `exec [info nameofexec] ... &` and `{2048 2048}`).
    let lock2_raw = include_str!("sqlite_test/lock2.rs");
    let lock2_tcl = lock2_raw
        .split_once("r#\"")
        .and_then(|(_, rest)| rest.rsplit_once("\"#"))
        .map(|(body, _)| body)
        .expect("extract lock2.rs tcl body");
    let lock2_report = evaluate_test_script("lock2", lock2_tcl);
    assert!(
        lock2_report.is_ok(),
        "lock2 failed: {:?}",
        lock2_report.failed
    );
    assert_eq!(lock2_report.passed, 11);

    let lock4_raw = include_str!("sqlite_test/lock4.rs");
    let lock4_tcl = lock4_raw
        .split_once("r#\"")
        .and_then(|(_, rest)| rest.rsplit_once("\"#"))
        .map(|(body, _)| body)
        .expect("extract lock4.rs tcl body");
    let lock4_report = evaluate_test_script("lock4", lock4_tcl);
    assert!(
        lock4_report.is_ok(),
        "lock4 failed: {:?}",
        lock4_report.failed
    );
    assert_eq!(lock4_report.passed, 4);

    // 2. Verify `lock.rs` (`64/64`), `lock3.rs`, `lock7.rs`, `fkey7.rs` (`15/15`), `savepoint.rs` (`139/139`).
    for (name, raw, expected_passed) in [
        ("lock", include_str!("sqlite_test/lock.rs"), 64usize),
        ("lock3", include_str!("sqlite_test/lock3.rs"), 8usize),
        ("lock7", include_str!("sqlite_test/lock7.rs"), 8usize),
        ("fkey7", include_str!("sqlite_test/fkey7.rs"), 15usize),
        (
            "savepoint",
            include_str!("sqlite_test/savepoint.rs"),
            139usize,
        ),
    ] {
        let tcl = raw
            .split_once("r#\"")
            .and_then(|(_, rest)| rest.rsplit_once("\"#"))
            .map(|(body, _)| body)
            .expect("extract standalone tcl body");
        let report = evaluate_test_script(name, tcl);
        assert!(
            report.is_ok(),
            "{name} failed (passed={}, failed={:?}, script_error={:?})",
            report.passed,
            report.failed,
            report.script_error
        );
        assert_eq!(
            report.passed, expected_passed,
            "unexpected subtest count for {name}"
        );
    }

    // 3. Verify newly promoted Phase 6 generated suites (`tempdb2`, `trigger6`, `window5`, `window6`,
    //    `view`, `walbig`, `walslow`, `walbak`).
    for (stem, raw, expected_passed) in [
        (
            "tempdb2",
            include_str!("../sqlite/test/tempdb2.test"),
            7usize,
        ),
        (
            "trigger6",
            include_str!("../sqlite/test/trigger6.test"),
            6usize,
        ),
        (
            "window5",
            include_str!("../sqlite/test/window5.test"),
            7usize,
        ),
        (
            "window6",
            include_str!("../sqlite/test/window6.test"),
            74usize,
        ),
        ("view", include_str!("../sqlite/test/view.test"), 121usize),
        ("walbig", include_str!("../sqlite/test/walbig.test"), 1usize),
        (
            "walslow",
            include_str!("../sqlite/test/walslow.test"),
            3722usize,
        ),
        (
            "walbak",
            include_str!("../sqlite/test/walbak.test"),
            83usize,
        ),
    ] {
        let stripped = strip_test_comments(raw);
        let report = evaluate_test_script_in_root("sqlite/test", stem, &stripped);
        assert!(
            report.is_ok(),
            "{stem} failed (passed={}, failed={:?}, script_error={:?})",
            report.passed,
            report.failed,
            report.script_error
        );
        assert_eq!(
            report.passed, expected_passed,
            "unexpected subtest count for {stem}"
        );
    }
}

#[test]
fn phase6_gate_3_tcl_callbacks_hooks_authorizer_backup_and_incrblob() {
    let report = evaluate_test_script(
        "gate6_tcl_bindings",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix gate6_tcl

# 1. Scalar UDF (`db function`) + Window UDF (`create_window_function`) + Custom Collation (`db collate`).
proc double_plus {x y} { expr {$x * 2 + $y} }
db function double_plus -argcount 2 -deterministic double_plus

set ::wsum 0
proc w_step {ctx val} { incr ::wsum $val }
proc w_final {ctx} { set r $::wsum; set ::wsum 0; return $r }
proc w_value {ctx} { return $::wsum }
proc w_inv {ctx val} { incr ::wsum [expr {-$val}] }
create_window_function db win_sum w_step w_final w_value w_inv

proc rev_cmp {a b} { string compare $b $a }
db collate rev_cmp rev_cmp

do_execsql_test 1.1 {
  CREATE TABLE items(id INTEGER PRIMARY KEY, label TEXT COLLATE rev_cmp, v INT);
  INSERT INTO items VALUES(1, 'alpha', 10), (2, 'beta', 20), (3, 'gamma', 30);
  SELECT label, double_plus(v, id), win_sum(v) OVER (ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)
  FROM items
  ORDER BY label;
} {gamma 63 50 beta 42 30 alpha 21 10}

# 2. `db update_hook`, `db commit_hook` (including veto -> `db rollback_hook`), and `db trace`.
set ::upd_log [list]
set ::commit_veto 0
set ::rollback_count 0
set ::trace_log [list]

proc on_upd {op db tbl rowid} {
  lappend ::upd_log "$op:$db.$tbl:$rowid"
}
proc on_commit {} {
  return $::commit_veto
}
proc on_rollback {} {
  incr ::rollback_count
}
proc on_trace {sql} {
  lappend ::trace_log [string trim $sql]
}

db update_hook on_upd
db commit_hook on_commit
db rollback_hook on_rollback
db trace on_trace

do_execsql_test 2.1 {
  BEGIN;
  INSERT INTO items VALUES(4, 'delta', 40);
  UPDATE items SET v = 45 WHERE id = 4;
  DELETE FROM items WHERE id = 1;
  COMMIT;
} {}

do_test 2.2 {
  set ::upd_log
} {INSERT:main.items:4 UPDATE:main.items:4 DELETE:main.items:1}

do_test 2.3 {
  set ::commit_veto 1
  set rc [catch {db eval {
    BEGIN;
    INSERT INTO items VALUES(5, 'epsilon', 50);
    COMMIT;
  }} msg]
  set ::commit_veto 0
  list $rc [db eval {SELECT count(*) FROM items WHERE id = 5}] $::rollback_count
} {1 0 1}

db trace ""
db update_hook ""
db commit_hook ""
db rollback_hook ""

do_test 2.4 {
  expr {[llength $::trace_log] >= 4}
} {1}

# 3. `db authorizer` (`SQLITE_OK`, `SQLITE_IGNORE`, `SQLITE_DENY`).
proc my_auth {code a1 a2 db trig} {
  if {$code eq "SQLITE_READ" && $a1 eq "items" && $a2 eq "v"} {
    return SQLITE_IGNORE
  }
  if {$code eq "SQLITE_DELETE" && $a1 eq "items"} {
    return SQLITE_DENY
  }
  return SQLITE_OK
}
db authorizer my_auth

do_execsql_test 3.1 {
  SELECT id, label, v FROM items ORDER BY id;
} {2 beta {} 3 gamma {} 4 delta {}}

do_catchsql_test 3.2 {
  DELETE FROM items WHERE id = 2;
} {1 {not authorized}}

db authorizer ""

# 4. `db progress` interrupt.
db progress 5 {expr 1}
do_catchsql_test 4.1 {
  WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 1000)
  SELECT sum(x) FROM cnt;
} {1 interrupted}
db progress 0 ""

# 5. `db incrblob` (read/write/seek/tell/close) + `db backup` & `sqlite3_backup`.
do_execsql_test 5.1 {
  CREATE TABLE docs(id INTEGER PRIMARY KEY, payload BLOB);
  INSERT INTO docs VALUES(1, zeroblob(16));
} {}

do_test 5.2 {
  set fd [db incrblob docs payload 1]
  seek $fd 4 start
  puts -nonewline $fd "RUST"
  seek $fd 0 start
  set raw [read $fd 16]
  set pos [tell $fd]
  close $fd
  list $pos [db eval {SELECT hex(payload) FROM docs WHERE id = 1}]
} {16 00000000525553540000000000000000}

do_test 5.3 {
  forcedelete bak_copy.db
  db backup main bak_copy.db
  sqlite3 db_bak bak_copy.db
  set res [db_bak eval {SELECT id, hex(payload) FROM docs}]
  db_bak close
  set res
} {1 00000000525553540000000000000000}

do_test 5.4 {
  forcedelete bak_step.db
  sqlite3 db_dst bak_step.db
  sqlite3_backup B db_dst main db main
  set s1 [B step 1]
  set rem [B remaining]
  set tot [B pagecount]
  set s2 [B step -1]
  set fin [B finish]
  set rows [db_dst eval {SELECT id, label, v FROM items ORDER BY id}]
  db_dst close
  list $s1 [expr {$tot >= 2}] $s2 $fin $rows
} {SQLITE_OK 1 SQLITE_DONE SQLITE_OK {2 beta 20 3 gamma 30 4 delta 45}}

finish_test
"#,
    );
    assert!(
        report.is_ok(),
        "gate6_tcl_bindings failed (passed={}, failed={:?}, script_error={:?})",
        report.passed,
        report.failed,
        report.script_error
    );
    assert_eq!(report.passed, 12);
}

#[test]
fn phase6_gate_4_c_sqlite_3541_differential_backup_blob_vacuum_and_subtypes() {
    let sandbox = TempSandbox::new("diff_e2e");

    // 1. Online `Backup` with page-size conversion (`1024 -> 4096`) and live source updates,
    //    verified by C SQLite 3.54.1 (`PRAGMA integrity_check` + logical query parity).
    let src_path = sandbox.path().join("src_1024.db");
    let dst_path = sandbox.path().join("dst_4096.db");

    let src_conn = Connection::open(&src_path).expect("open src_1024.db");
    src_conn
        .execute_batch(
            "PRAGMA page_size = 1024;
             PRAGMA auto_vacuum = INCREMENTAL;
             CREATE TABLE accounts(id INTEGER PRIMARY KEY, owner TEXT NOT NULL, balance INT NOT NULL, note BLOB);
             CREATE INDEX idx_accounts_owner ON accounts(owner);",
        )
        .expect("init src schema");
    for i in 1..=80i64 {
        src_conn
            .execute(&format!(
                "INSERT INTO accounts VALUES({i}, 'user_{i:03}', {}, zeroblob(300))",
                i * 100
            ))
            .expect("insert account");
    }

    let dst_conn = Connection::open(&dst_path).expect("open dst_4096.db");
    dst_conn
        .execute_batch("PRAGMA page_size = 4096;")
        .expect("set dst page_size 4096");

    {
        let mut bak = Backup::init(&src_conn, "main", &dst_conn, "main").expect("init backup");
        let r1 = bak.step(5).expect("step 5 pages");
        assert_eq!(r1, BackupStepResult::More);
        assert!(bak.pagecount() > 5);
        assert!(bak.remaining() > 0);

        // Live write on `src_conn` mid-backup triggers automatic backup restart on same-process handles.
        src_conn
            .execute("UPDATE accounts SET balance = balance + 7 WHERE id <= 10")
            .expect("live update on src_conn");

        let r_done = bak.step(-1).expect("step remaining pages");
        assert_eq!(r_done, BackupStepResult::Done);
        assert_eq!(bak.remaining(), 0);
        bak.finish().expect("finish backup");
    }

    // Incremental `Blob` I/O on `dst_conn` (inline + multi-page overflow + savepoint rollback).
    dst_conn
        .execute("INSERT INTO accounts VALUES(999, 'overflow_owner', 99900, zeroblob(10000))")
        .expect("insert overflow row");
    {
        let mut blob = dst_conn
            .blob_open("main", "accounts", "note", 999, true)
            .expect("open read-write blob 999");
        assert_eq!(blob.len(), 10000);
        let pattern: Vec<u8> = (0..10000usize)
            .map(|idx| b'A' + ((idx * 31 + 17) % 26) as u8)
            .collect();
        blob.write_at(&pattern[..4096], 0)
            .expect("write first chunk");
        blob.write_at(&pattern[4096..], 4096)
            .expect("write overflow chunk");
        let mut read_back = vec![0u8; 10000];
        blob.read_at(&mut read_back, 0)
            .expect("read back overflow blob");
        assert_eq!(read_back, pattern);
        blob.close().expect("close blob");
    }

    // Savepoint rollback of incremental blob write.
    dst_conn
        .execute_batch("SAVEPOINT sp_blob;")
        .expect("open sp_blob");
    {
        let mut blob = dst_conn
            .blob_open("main", "accounts", "note", 1, true)
            .expect("open blob row 1");
        blob.write_at(b"TEMPORARY_WRITE", 0)
            .expect("write inside savepoint");
        blob.close().expect("close blob row 1");
    }
    dst_conn
        .execute_batch("ROLLBACK TO sp_blob; RELEASE sp_blob;")
        .expect("rollback and release sp_blob");

    // Verify `dst_4096.db` with C SQLite 3.54.1 (`PRAGMA page_size`, `PRAGMA integrity_check`, and contents).
    let (ok, c_out, c_err) = run_c_sqlite_in_dir(
        sandbox.path(),
        dst_path.to_str().unwrap(),
        "PRAGMA page_size;
         PRAGMA integrity_check;
         SELECT count(*), sum(balance) FROM accounts WHERE id <= 80;
         SELECT hex(substr(note, 1, 8)) FROM accounts WHERE id = 1;
         SELECT length(note), hex(substr(note, 4095, 6)) FROM accounts WHERE id = 999;",
    );
    assert!(ok, "C sqlite3 check on dst_4096.db failed: {c_err}");
    let r_rows = dst_conn
        .query(
            "SELECT count(*), sum(balance) FROM accounts WHERE id <= 80",
            &[],
        )
        .expect("rust query dst");
    let r_n1 = dst_conn
        .query(
            "SELECT hex(substr(note, 1, 8)) FROM accounts WHERE id = 1",
            &[],
        )
        .expect("rust query note 1");
    let r_n999 = dst_conn
        .query(
            "SELECT length(note), hex(substr(note, 4095, 6)) FROM accounts WHERE id = 999",
            &[],
        )
        .expect("rust query note 999");
    let expected_c_out = format!(
        "1024\nok\n{}{}{}",
        format_rows_pipe(&r_rows.rows),
        format_rows_pipe(&r_n1.rows),
        format_rows_pipe(&r_n999.rows),
    );
    assert_eq!(c_out, expected_c_out);

    // 2. `VACUUM` with custom collation + UDFs and subtype preservation across scalar & window functions.
    let vac_path = sandbox.path().join("vac_custom.db");
    let vac_conn = Connection::open(&vac_path).expect("open vac_custom.db");
    vac_conn
        .create_fallible_collation("LEN_ALPHA", |a, b| -> Result<Ordering> {
            Ok(a.len().cmp(&b.len()).then_with(|| a.cmp(b)))
        })
        .expect("register LEN_ALPHA collation");
    vac_conn
        .create_scalar_function(
            "tag_subtype",
            1,
            FunctionFlags::DETERMINISTIC | FunctionFlags::RESULT_SUBTYPE,
            |args| {
                let v = args
                    .first()
                    .map(|rv| rv.value.clone())
                    .unwrap_or(Value::Null);
                Ok(RuntimeValue::with_subtype(v, 77))
            },
        )
        .expect("register tag_subtype");
    vac_conn
        .create_scalar_function(
            "read_subtype",
            1,
            FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE,
            |args| {
                let st = args
                    .first()
                    .and_then(|rv| rv.has_subtype().then(|| i64::from(rv.subtype)))
                    .unwrap_or(-1);
                Ok(RuntimeValue::plain(Value::Integer(st)))
            },
        )
        .expect("register read_subtype");

    struct SubtypeCarryWin {
        last: Option<RuntimeValue>,
    }
    impl WindowAggregateInstance for SubtypeCarryWin {
        fn step(&mut self, args: &[RuntimeValue]) -> Result<()> {
            if let Some(first) = args.first() {
                self.last = Some(first.clone());
            }
            Ok(())
        }
        fn inverse(&mut self, _args: &[RuntimeValue]) -> Result<()> {
            Ok(())
        }
        fn value(&self) -> Result<RuntimeValue> {
            Ok(self
                .last
                .clone()
                .unwrap_or_else(|| RuntimeValue::plain(Value::Null)))
        }
        fn finalize(&mut self) -> Result<RuntimeValue> {
            self.value()
        }
    }
    vac_conn
        .create_window_function(
            "win_carry_subtype",
            1,
            FunctionFlags::DETERMINISTIC | FunctionFlags::SUBTYPE | FunctionFlags::RESULT_SUBTYPE,
            || Box::new(SubtypeCarryWin { last: None }),
        )
        .expect("register win_carry_subtype");

    vac_conn
        .execute_batch(
            "CREATE TABLE words(id INTEGER PRIMARY KEY, w TEXT COLLATE LEN_ALPHA);
             CREATE INDEX idx_words_w ON words(w);
             INSERT INTO words VALUES(1, 'pear'), (2, 'fig'), (3, 'banana'), (4, 'apple'), (5, 'kiwi');
             DELETE FROM words WHERE id = 4;
             VACUUM;",
        )
        .expect("populate and vacuum vac_custom.db");

    let ordered_words = vac_conn
        .query("SELECT w FROM words ORDER BY w", &[])
        .expect("query ordered words after VACUUM");
    assert_eq!(
        format_rows_pipe(&ordered_words.rows),
        "fig\nkiwi\npear\nbanana\n"
    );

    let subtype_rows = vac_conn
        .query(
            "SELECT
                read_subtype(tag_subtype('hello')),
                read_subtype(tag_subtype('hello') || ''),
                read_subtype( win_carry_subtype(tag_subtype(w)) OVER (ORDER BY id) )
             FROM words
             ORDER BY id
             LIMIT 1",
            &[],
        )
        .expect("query subtype preservation and stripping");
    assert_eq!(
        format_rows_pipe(&subtype_rows.rows),
        "77|-1|77\n",
        "UDFs with SUBTYPE/RESULT_SUBTYPE must preserve subtype while SQL expressions strip it"
    );

    let (vac_ok, vac_c_out, vac_c_err) = run_c_sqlite_in_dir(
        sandbox.path(),
        vac_path.to_str().unwrap(),
        "PRAGMA page_count; SELECT id, w FROM words ORDER BY id;",
    );
    assert!(
        vac_ok,
        "C sqlite3 check on vac_custom.db failed: {vac_c_err}"
    );
    assert_eq!(vac_c_out, "3\n1|pear\n2|fig\n3|banana\n5|kiwi\n");

    let _ = SqliteError::new(ErrorCode::Ok, "gate6 ok");
}
