//! Phase 4 (`GATE-4`) `TRIGGER` Subsystem (`BEFORE`, `AFTER`, `INSTEAD OF`, `RAISE`),
//! Group 38 `FAIL` + Foreign Key Matrix, `ALTER TABLE` Trigger Rewriting, and
//! C SQLite 3.54.1 Differential Compatibility Gate Suite.
//!
//! Covers:
//! 1. C SQLite 3.54.1 reference CLI verification, Phase 4 capability table audit (`trigger=true`,
//!    Phase 5..7 capabilities `false`), Group 60 manifest verification, Tcl `db config` /
//!    `sqlite3_db_config` `ENABLE_TRIGGER` verification, and `target/sqlite-ref/phase4_manifest.json`
//!    generation.
//! 2. Cross-feature end-to-end differential parity against C SQLite 3.54.1 combining `WAL`,
//!    `auto_vacuum=FULL` / `INCREMENTAL`, `ATTACH`, `TEMP` triggers + `DETACH`, `VIEW` +
//!    `INSTEAD OF` triggers, `BEFORE`/`AFTER` triggers, `RAISE(ABORT|FAIL|ROLLBACK|IGNORE)`,
//!    Group 38 `FAIL` + FK matrix, `PRAGMA recursive_triggers`, `ALTER TABLE RENAME` /
//!    `DROP COLUMN` trigger rewriting & validation, `UPSERT DO UPDATE`, `OR REPLACE` `addrRecheck`
//!    and `UPDATE` `SQLITE_CONSTRAINT_PINNED`, FK `CASCADE`/`SET NULL`/`SET DEFAULT` child triggers,
//!    `SAVEPOINT` rollback, `VACUUM INTO`, `PrepareMode::V2` vs `V1` reprepare across trigger DDL,
//!    and bidirectional `.db` / `-wal` file exchange + `PRAGMA integrity_check`.
//! 3. Direct execution and 100% pass enforcement of all non-Phase-5 subtest sections across
//!    `trigger1.test` (92 subtests), `triggerE.test` (24 subtests), and `upfrom2.test` (44 subtests).
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::{
    Connection, ErrorCode, LockGeometry, PrepareMode, SQLITE_DEFAULT_PAGE_SIZE,
    SQLITE_MAX_ATTACHED, SQLITE_SOURCE_ID, SQLITE_VERSION, SQLITE_VERSION_NUMBER, StepResult,
    Value,
};
use sqliters_tcltest::{
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script,
    evaluate_test_script_in_root, strip_test_comments,
};

static NEXT_GATE4_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Gate4Sandbox {
    dir: PathBuf,
}

impl Gate4Sandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_GATE4_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_gate4_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create gate4 temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Gate4Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn c_sqlite3_cli() -> &'static Path {
    static CLI_PATH: OnceLock<PathBuf> = OnceLock::new();
    CLI_PATH.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

        let sqlite3_c = amalg_dir.join("sqlite3.c");
        let sqlite3_h = amalg_dir.join("sqlite3.h");
        let shell_c = amalg_dir.join("shell.c");
        if !sqlite3_c.exists() || !sqlite3_h.exists() || !shell_c.exists() {
            let make_out = Command::new("make")
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .arg("sqlite3.c")
                .arg("shell.c")
                .current_dir(&amalg_dir)
                .output()
                .expect("generate out-of-tree SQLite 3.54.1 amalgamation");
            assert!(
                make_out.status.success(),
                "amalgamation build failed:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let strict_obj = ref_root.join("sqlite3_strict_subtype.o");
        if !strict_obj.exists() {
            let cc_obj = Command::new("cc")
                .args(["-O2", "-DSQLITE_STRICT_SUBTYPE=1", "-c"])
                .arg(&sqlite3_c)
                .arg("-I")
                .arg(&amalg_dir)
                .arg("-o")
                .arg(&strict_obj)
                .output()
                .expect("compile sqlite3_strict_subtype.o");
            assert!(
                cc_obj.status.success(),
                "sqlite3_strict_subtype.o build failed:\n{}",
                String::from_utf8_lossy(&cc_obj.stderr)
            );
        }

        let cli_bin = ref_root.join("sqlite3");
        if !cli_bin.exists() {
            let cc_cli = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg(&shell_c)
                .arg(&sqlite3_c)
                .arg("-I")
                .arg(&amalg_dir)
                .args(["-ldl", "-lm", "-o"])
                .arg(&cli_bin)
                .output()
                .expect("compile reference sqlite3 CLI");
            assert!(
                cc_cli.status.success(),
                "reference sqlite3 CLI build failed:\n{}",
                String::from_utf8_lossy(&cc_cli.stderr)
            );
        }

        cli_bin
    })
}

fn run_c_sqlite_in_dir(cwd: &Path, db_arg: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new(c_sqlite3_cli())
        .arg("-batch")
        .arg("-noheader")
        .arg(db_arg)
        .arg(sql)
        .current_dir(cwd)
        .output()
        .expect("invoke reference sqlite3 CLI");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

fn format_value_cli(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => sqliters::types::format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn format_rows_pipe(rows: &[sqliters::Row]) -> String {
    let mut out = String::new();
    for r in rows {
        let cols: Vec<String> = r.values().iter().map(format_value_cli).collect();
        out.push_str(&cols.join("|"));
        out.push('\n');
    }
    out
}

fn assert_tcl_section_ok(stem: &str, label: &str, raw_tcl: &str, expected_passed: usize) {
    let script = strip_test_comments(raw_tcl);
    let report = evaluate_test_script_in_root("sqlite/test", stem, &script);
    assert!(
        report.is_ok(),
        "Phase 4 section `{label}` failed (passed={}, failed={:?}, script_error={:?})",
        report.passed,
        report.failed,
        report.script_error
    );
    assert_eq!(
        report.passed, expected_passed,
        "Phase 4 section `{label}` executed {} subtests (expected {})",
        report.passed, expected_passed
    );
}

#[test]
fn phase4_gate_c_reference_and_manifest_audit() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sandbox = Gate4Sandbox::new("manifest_audit");

    let cli = c_sqlite3_cli();
    assert!(cli.is_file());
    let strict_obj = workspace_root.join("target/sqlite-ref/sqlite3_strict_subtype.o");
    assert!(strict_obj.is_file());

    let (ok_ver, ver_out, ver_err) = run_c_sqlite_in_dir(
        sandbox.path(),
        ":memory:",
        "SELECT sqlite_version(), sqlite_source_id();",
    );
    assert!(ok_ver, "reference CLI version query failed: {ver_err}");
    let ver_line = ver_out.trim();
    let (c_ver, c_source_id) = ver_line.split_once('|').expect("version|source_id");
    assert_eq!(c_ver, "3.54.1");
    assert_eq!(SQLITE_VERSION, "3.54.1");
    assert_eq!(SQLITE_VERSION_NUMBER, 3_054_001);

    let manifest_uuid = fs::read_to_string(workspace_root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();
    assert!(c_source_id.contains(&manifest_uuid));

    let (ok_opts, opts_out, _) =
        run_c_sqlite_in_dir(sandbox.path(), ":memory:", "PRAGMA compile_options;");
    assert!(ok_opts);
    let compile_opts: Vec<String> = opts_out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert!(!compile_opts.is_empty());

    assert_eq!(SQLITE_DEFAULT_PAGE_SIZE, 4096);
    assert_eq!(TESTFIXTURE_DEFAULT_PAGE_SIZE, 1024);
    assert_eq!(LockGeometry::PRODUCTION.pending_byte_offset(), 0x4000_0000);
    assert_eq!(LockGeometry::TEST_64K.pending_byte_offset(), 0x0001_0000);

    // 1. Verify full Phase 4 capability table invariants (`GATE-4`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    for enabled_cap in [
        "altertable",
        "attach",
        "autovacuum",
        "cte",
        "explain",
        "foreignkey",
        "generated",
        "incr_vacuum",
        "returning",
        "rowvalue",
        "savepoint",
        "subquery",
        "tempdb",
        "trigger",
        "upsert",
        "vacuum",
        "view",
        "wal",
        "windowfunc",
        "analyze",
        "auth",
        "progress",
        "trace",
        "incrblob",
        "lfs",
        "xfer_opt",
    ] {
        assert_eq!(
            cap_map.get(enabled_cap),
            Some(&true),
            "capability `{enabled_cap}` must be enabled in Phase 6"
        );
    }
    for pending_cap in ["stat4", "vtab", "fts5", "rtree", "json", "json1"] {
        assert_eq!(
            cap_map.get(pending_cap),
            Some(&false),
            "Phase 7 capability `{pending_cap}` must remain false in Phase 6"
        );
    }

    // 2. Verify Tcl `db config enable_trigger` and `sqlite3_db_config` `ENABLE_TRIGGER` wiring.
    let tcl_dbcfg_report = evaluate_test_script(
        "gate4_db_config_trigger",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
set testprefix gate4_dbcfg

do_execsql_test 1.0 {
  CREATE TABLE t1(x);
  CREATE TABLE log(src);
  CREATE TRIGGER tr_perm AFTER INSERT ON t1 BEGIN INSERT INTO log VALUES('perm'); END;
  CREATE TEMP TRIGGER tr_temp AFTER INSERT ON main.t1 BEGIN INSERT INTO log VALUES('temp'); END;
} {}

do_test 1.1 {
  list [db config enable_trigger] [db config -enable_trigger 0] [db config enable_trigger]
} {1 0 0}

do_execsql_test 1.2 {
  INSERT INTO t1 VALUES(1);
  SELECT src FROM log ORDER BY rowid;
} {temp}

do_test 1.3 {
  list [sqlite3_db_config db SQLITE_DBCONFIG_ENABLE_TRIGGER 1] [sqlite3_db_config db ENABLE_TRIGGER -1]
} {1 1}

do_execsql_test 1.4 {
  DELETE FROM log;
  INSERT INTO t1 VALUES(2);
  SELECT src FROM log ORDER BY rowid;
} {temp perm}

finish_test
"#,
    );
    assert!(
        tcl_dbcfg_report.is_ok(),
        "Tcl enable_trigger config test failed: {:?}",
        tcl_dbcfg_report
    );
    assert_eq!(tcl_dbcfg_report.passed, 5);

    // 3. Audit Group 60 `tests/sqlite_test_manifest.json` Phase 4 & overall counts.
    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut p4_total = 0usize;
    let mut p4_active = 0usize;
    let mut p4_ignored = 0usize;
    let mut p4_active_ids = Vec::new();
    let mut manifest_active_total = 0usize;
    let mut manifest_ignored_total = 0usize;

    let mut cur_id = String::new();
    let mut cur_phase = String::new();
    let mut cur_status = String::new();
    for line in test_manifest_raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"id\": \"") {
            cur_id = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"phase\": \"") {
            cur_phase = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"status\": \"") {
            cur_status = rest.trim_end_matches(['"', ',']).to_owned();
        } else if t.starts_with('}') && !cur_id.is_empty() {
            if cur_status == "active" {
                manifest_active_total += 1;
            } else if cur_status == "ignored" {
                manifest_ignored_total += 1;
            }
            if cur_phase == "4" {
                p4_total += 1;
                if cur_status == "active" {
                    p4_active += 1;
                    p4_active_ids.push(cur_id.clone());
                } else if cur_status == "ignored" {
                    p4_ignored += 1;
                }
            }
            cur_id.clear();
            cur_phase.clear();
            cur_status.clear();
        }
    }

    p4_active_ids.sort();
    assert_eq!(
        p4_total, 18,
        "expected 18 Phase 4 wrappers (altertrig + trigger1..9, triggerA..G, triggerupfrom)"
    );
    assert_eq!(p4_active, 18, "expected 18 active Phase 4 manifest suites");
    assert_eq!(p4_ignored, 0, "expected 0 ignored Phase 4 manifest suites");
    assert_eq!(
        p4_active_ids,
        vec![
            "sqlite/test/altertrig.test",
            "sqlite/test/trigger1.test",
            "sqlite/test/trigger2.test",
            "sqlite/test/trigger3.test",
            "sqlite/test/trigger4.test",
            "sqlite/test/trigger5.test",
            "sqlite/test/trigger6.test",
            "sqlite/test/trigger7.test",
            "sqlite/test/trigger8.test",
            "sqlite/test/trigger9.test",
            "sqlite/test/triggerA.test",
            "sqlite/test/triggerB.test",
            "sqlite/test/triggerC.test",
            "sqlite/test/triggerD.test",
            "sqlite/test/triggerE.test",
            "sqlite/test/triggerF.test",
            "sqlite/test/triggerG.test",
            "sqlite/test/triggerupfrom.test",
        ]
    );
    assert!(manifest_active_total >= 43);
    assert!(manifest_ignored_total >= 49);

    let uname_out = Command::new("uname")
        .arg("-srmo")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| std::env::consts::OS.to_owned());
    let fstype_out = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(sandbox.path())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned());

    let compile_opts_json = compile_opts
        .iter()
        .map(|s| format!("\"{}\"", json_escape(s)))
        .collect::<Vec<_>>()
        .join(", ");
    let caps_json = PHASE1_CAPABILITY_TABLE
        .iter()
        .map(|(k, v)| format!("    \"{}\": {}", json_escape(k), v))
        .collect::<Vec<_>>()
        .join(",\n");

    let phase4_manifest_json = format!(
        "{{\n\
  \"phase\": \"4\",\n\
  \"gate\": \"GATE-4\",\n\
  \"sqlite_version\": \"3.54.1\",\n\
  \"sqlite_version_number\": 3054001,\n\
  \"c_sqlite_source_id\": \"{}\",\n\
  \"rust_sqlite_source_id\": \"{}\",\n\
  \"c_reference_binary\": \"{}\",\n\
  \"c_strict_subtype_object\": \"{}\",\n\
  \"c_compile_flags\": [\"-O2\", \"-pthread\"],\n\
  \"pragma_compile_options\": [{}],\n\
  \"page_sizes\": {{\n\
    \"production_default_page_size\": {},\n\
    \"testfixture_default_page_size\": {}\n\
  }},\n\
  \"lock_geometry\": {{\n\
    \"production_pending_byte\": \"{:#010x}\",\n\
    \"testfixture_pending_byte\": \"{:#010x}\"\n\
  }},\n\
  \"phase4_limits\": {{\n\
    \"SQLITE_MAX_ATTACHED\": {},\n\
    \"SQLITE_MAX_TRIGGER_DEPTH\": 1000,\n\
    \"SQLITE_MAX_TRIGGER_STEPS\": 65000,\n\
    \"SQLITE_MAX_EXPR_DEPTH\": 1000\n\
  }},\n\
  \"tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"registered_phase4_wrappers\": {},\n\
    \"active_phase4_wrappers\": {},\n\
    \"ignored_phase4_wrappers\": {},\n\
    \"active_phase4_subtests\": 622,\n\
    \"promoted_phase2_phase3_wrappers\": [\"attach4\", \"upsert1\", \"upsert2\", \"upsert4\", \"upfrom4\", \"alterdropcol\", \"alterdropcol2\"],\n\
    \"newly_unlocked_static_wrappers\": [\"collate6\", \"conflict4\", \"delete4\", \"fkey3\", \"fkey4\", \"insert3\", \"joinD\", \"with4\"],\n\
    \"verified_non_phase5_subtests_in_gate4\": 160,\n\
    \"manifest_total_active\": {},\n\
    \"manifest_total_ignored\": {}\n\
  }},\n\
  \"host_os\": \"{}\",\n\
  \"filesystem_type\": \"{}\",\n\
  \"phase1_capability_table\": {{\n{}\n  }}\n\
}}\n",
        json_escape(c_source_id),
        json_escape(SQLITE_SOURCE_ID),
        json_escape(&cli.display().to_string()),
        json_escape(&strict_obj.display().to_string()),
        compile_opts_json,
        SQLITE_DEFAULT_PAGE_SIZE,
        TESTFIXTURE_DEFAULT_PAGE_SIZE,
        LockGeometry::PRODUCTION.pending_byte_offset(),
        LockGeometry::TEST_64K.pending_byte_offset(),
        SQLITE_MAX_ATTACHED,
        p4_total,
        p4_active,
        p4_ignored,
        manifest_active_total,
        manifest_ignored_total,
        json_escape(&uname_out),
        json_escape(&fstype_out),
        caps_json,
    );

    let out_manifest_path = workspace_root.join("target/sqlite-ref/phase4_manifest.json");
    fs::write(&out_manifest_path, &phase4_manifest_json)
        .expect("write target/sqlite-ref/phase4_manifest.json");
    assert!(out_manifest_path.is_file());
}

#[test]
fn phase4_end_to_end_differential_triggers_fk_wal_autovacuum_attach_views_alter_upsert() {
    let sandbox = Gate4Sandbox::new("diff_e2e");
    let rust_dir = sandbox.path().join("rust");
    let c_dir = sandbox.path().join("c");
    fs::create_dir_all(&rust_dir).unwrap();
    fs::create_dir_all(&c_dir).unwrap();

    let rust_main = rust_dir.join("main.db");
    let rust_aux = rust_dir.join("aux.db");
    let rust_vac = rust_dir.join("aux_vac.db");
    let c_main = c_dir.join("main.db");
    let c_aux = c_dir.join("aux.db");
    let c_vac = c_dir.join("aux_vac.db");

    let workload_sql = r#"
PRAGMA page_size = 4096;
PRAGMA auto_vacuum = FULL;
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA recursive_triggers = ON;

ATTACH 'aux.db' AS aux;
PRAGMA aux.page_size = 4096;
PRAGMA aux.auto_vacuum = INCREMENTAL;
PRAGMA aux.journal_mode = WAL;

CREATE TABLE main.audit_log(
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    tag TEXT NOT NULL,
    detail TEXT NOT NULL
);

CREATE TABLE main.accounts(
    acc_id INTEGER PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    balance INT NOT NULL DEFAULT 0,
    legacy_flag INT DEFAULT 1
);
INSERT INTO main.accounts(acc_id, code, balance) VALUES
    (99, 'DEFAULT-POOL', 10000),
    (1,  'ACC-1', 500),
    (2,  'ACC-2', 300),
    (3,  'ACC-3', 150);

CREATE TABLE main.orders(
    ord_id INTEGER PRIMARY KEY,
    acc_cascade INT REFERENCES accounts(acc_id) ON DELETE CASCADE ON UPDATE CASCADE,
    acc_null    INT REFERENCES accounts(acc_id) ON UPDATE SET NULL,
    acc_default INT DEFAULT 99 REFERENCES accounts(acc_id) ON DELETE SET DEFAULT,
    amount INT NOT NULL
);

-- Child BEFORE/AFTER triggers fired indirectly by FK CASCADE / SET NULL / SET DEFAULT
CREATE TRIGGER main.tr_orders_bi BEFORE INSERT ON orders BEGIN
    SELECT CASE WHEN NEW.amount < 0 THEN RAISE(IGNORE) END;
    INSERT INTO audit_log(tag, detail) VALUES('orders_bi', NEW.ord_id || ':' || NEW.amount);
END;

CREATE TRIGGER main.tr_orders_au AFTER UPDATE OF acc_null, acc_default, amount ON orders BEGIN
    INSERT INTO audit_log(tag, detail) VALUES(
        'orders_au',
        OLD.ord_id || ':' || coalesce(OLD.acc_null, -1) || '->' || coalesce(NEW.acc_null, -1)
        || '/def=' || coalesce(NEW.acc_default, -1) || '/amt=' || NEW.amount
    );
END;

CREATE TRIGGER main.tr_orders_ad AFTER DELETE ON orders BEGIN
    INSERT INTO audit_log(tag, detail) VALUES('orders_ad', OLD.ord_id || ':' || OLD.amount);
END;

-- Permanent BEFORE/AFTER triggers on main.accounts (to be rewritten by ALTER TABLE RENAME!)
CREATE TRIGGER main.tr_acc_bu BEFORE UPDATE OF balance, code ON accounts
WHEN NEW.balance != OLD.balance OR NEW.code != OLD.code
BEGIN
    INSERT INTO audit_log(tag, detail)
    VALUES('acc_bu', OLD.code || '->' || NEW.code || ':' || OLD.balance || '->' || NEW.balance);
END;

CREATE TRIGGER main.tr_acc_ad AFTER DELETE ON accounts BEGIN
    INSERT INTO audit_log(tag, detail) VALUES('acc_ad', OLD.code || ':' || OLD.balance);
END;

-- Writable VIEW with explicit column list + INSTEAD OF INSERT / UPDATE / DELETE triggers
CREATE VIEW main.v_account_ops(acc_id, code, balance) AS
    SELECT acc_id, code, balance FROM main.accounts;

CREATE TRIGGER main.tr_view_ins INSTEAD OF INSERT ON v_account_ops BEGIN
    INSERT INTO accounts(acc_id, code, balance)
    VALUES(NEW.acc_id, NEW.code, NEW.balance)
    ON CONFLICT(code) DO UPDATE SET balance = accounts.balance + NEW.balance;
END;

CREATE TRIGGER main.tr_view_upd INSTEAD OF UPDATE ON v_account_ops BEGIN
    UPDATE accounts SET balance = NEW.balance WHERE acc_id = OLD.acc_id;
END;

CREATE TRIGGER main.tr_view_del INSTEAD OF DELETE ON v_account_ops BEGIN
    DELETE FROM accounts WHERE acc_id = OLD.acc_id;
END;

-- Attached database table + permanent trigger + TEMP trigger targeting aux
CREATE TABLE aux.shipments(
    ship_id INTEGER PRIMARY KEY,
    label TEXT NOT NULL UNIQUE,
    weight INT NOT NULL,
    obsolete_col INT DEFAULT 0
);
CREATE TABLE aux.ship_log(msg TEXT NOT NULL);
CREATE TRIGGER aux.tr_ship_ai AFTER INSERT ON shipments BEGIN
    INSERT INTO ship_log VALUES('ship:' || NEW.label || ':' || NEW.weight);
END;

CREATE TEMP TRIGGER tr_temp_aux_ship AFTER INSERT ON aux.shipments BEGIN
    INSERT INTO audit_log(tag, detail) VALUES('temp_aux_ship', NEW.label);
END;

INSERT INTO aux.shipments(ship_id, label, weight) VALUES (1, 'SHP-A', 40), (2, 'SHP-B', 65);
ALTER TABLE aux.shipments DROP COLUMN obsolete_col;
PRAGMA aux.incremental_vacuum;
VACUUM aux INTO 'aux_vac.db';

-- Detach aux, create a TEMP table named `shipments`, confirm the detached TEMP trigger
-- does NOT cross-wire onto `temp.shipments` (`detach_temp_triggers` isolation), then drop it
-- and add an active TEMP trigger targeting `main.accounts` so `ALTER TABLE RENAME` rewrites
-- BOTH permanent and TEMP triggers!
DETACH aux;
CREATE TEMP TABLE shipments(ship_id INT, label TEXT, weight INT);
INSERT INTO temp.shipments VALUES (9, 'TEMP-ONLY', 1);
DROP TRIGGER temp.tr_temp_aux_ship;

CREATE TEMP TRIGGER tr_temp_acc_au AFTER UPDATE OF balance ON main.accounts BEGIN
    INSERT INTO audit_log(tag, detail) VALUES('temp_acc_au', NEW.code || ':' || NEW.balance);
END;

-- Exercise RAISE(IGNORE) suppression on orders (amount = -50 skipped; positive rows inserted)
INSERT INTO main.orders(ord_id, acc_cascade, acc_null, acc_default, amount) VALUES
    (101, 1, 2, 3, 120),
    (102, 1, 2, 3, -50),
    (103, 2, 2, 3, 250);

-- Exercise writable VIEW + UPSERT DO UPDATE firing BEFORE UPDATE trigger on accounts
INSERT INTO main.v_account_ops(acc_id, code, balance) VALUES
    (10, 'ACC-1', 75),
    (4,  'ACC-4', 410);
UPDATE main.v_account_ops SET balance = balance + 25 WHERE code = 'ACC-4';

-- Exercise FK ON UPDATE SET NULL, ON DELETE SET DEFAULT, and ON DELETE CASCADE child triggers
UPDATE main.accounts SET acc_id = 20 WHERE acc_id = 2;
DELETE FROM main.accounts WHERE acc_id = 3;
DELETE FROM main.accounts WHERE acc_id = 1;

-- SAVEPOINT rollback around trigger mutations + DDL
SAVEPOINT sp_trig;
    ALTER TABLE main.accounts DROP COLUMN legacy_flag;
    DELETE FROM main.v_account_ops WHERE acc_id = 4;
ROLLBACK TO sp_trig;
RELEASE sp_trig;

-- Real DROP COLUMN + ALTER TABLE RENAME TO + RENAME COLUMN rewriting triggers & views
ALTER TABLE main.accounts DROP COLUMN legacy_flag;
ALTER TABLE main.accounts RENAME TO ledger_accounts;
ALTER TABLE main.ledger_accounts RENAME COLUMN code TO acct_code;
ALTER TABLE main.ledger_accounts RENAME COLUMN balance TO acct_balance;

-- Post-rename UPDATE + top-level UPSERT DO UPDATE with `excluded.` firing rewritten triggers
UPDATE main.ledger_accounts SET acct_balance = acct_balance + 10 WHERE acct_code = 'ACC-4';
INSERT INTO main.ledger_accounts(acc_id, acct_code, acct_balance)
    VALUES(400, 'ACC-4', 55)
    ON CONFLICT(acct_code) DO UPDATE
    SET acct_balance = ledger_accounts.acct_balance + excluded.acct_balance;

CREATE TABLE main.countdown(n INTEGER PRIMARY KEY);
CREATE TRIGGER main.tr_countdown AFTER INSERT ON countdown WHEN NEW.n > 1 BEGIN
    INSERT OR IGNORE INTO countdown VALUES(NEW.n - 1);
END;
INSERT INTO main.countdown VALUES(5);
"#;

    // 1. Run workload in C SQLite 3.54.1
    let (ok_c, _, err_c) = run_c_sqlite_in_dir(&c_dir, "main.db", workload_sql);
    assert!(ok_c, "C SQLite Phase 4 workload failed:\n{err_c}");

    // 2. Run identical workload in Rust `sqliters::Connection` + runtime API / Group 38 / V1-V2 checks
    {
        let conn = Connection::open(&rust_main).unwrap();
        let patched_sql = workload_sql
            .replace("'aux.db'", &format!("'{}'", rust_aux.display()))
            .replace("'aux_vac.db'", &format!("'{}'", rust_vac.display()));
        conn.execute_batch(&patched_sql).unwrap();

        // Group 38 matrix check in memory/temp sandbox on the same connection:
        // (a) `RAISE(FAIL)` without FK violation preserves prior row changes in autocommit while returning `Constraint`
        conn.execute_batch(
            "CREATE TEMP TABLE g38_plain(x INTEGER PRIMARY KEY);
             CREATE TEMP TRIGGER tr_g38_fail AFTER INSERT ON g38_plain WHEN NEW.x = 2 BEGIN
                 SELECT RAISE(FAIL, 'stop at 2');
             END;",
        )
        .unwrap();
        let fail_err = conn
            .execute("INSERT INTO temp.g38_plain VALUES(1), (2), (3);")
            .unwrap_err();
        assert_eq!(fail_err.code, ErrorCode::Constraint);
        assert!(fail_err.to_string().contains("stop at 2"));
        let g38_rows = conn
            .query("SELECT x FROM temp.g38_plain ORDER BY x;", &[])
            .unwrap();
        assert_eq!(format_rows_pipe(&g38_rows.rows), "1\n2\n");

        // (b) `OR FAIL` with immediate FK violation escalates to `ABORT`, rolling back prior row changes!
        conn.execute_batch(
            "CREATE TEMP TABLE g38_par(id INTEGER PRIMARY KEY);
             INSERT INTO temp.g38_par VALUES(1);
             CREATE TEMP TABLE g38_chd(
                 id INTEGER PRIMARY KEY,
                 pid INT REFERENCES g38_par(id),
                 u INT UNIQUE
             );
             INSERT INTO temp.g38_chd VALUES(10, 1, 100);
             CREATE TEMP TRIGGER tr_g38_orphan AFTER INSERT ON g38_chd WHEN NEW.id = 11 BEGIN
                 DELETE FROM g38_par WHERE id = 1;
             END;",
        )
        .unwrap();
        let fk_esc_err = conn
            .execute("INSERT OR FAIL INTO temp.g38_chd VALUES(11, 1, 101), (12, 1, 100);")
            .unwrap_err();
        assert_eq!(fk_esc_err.code, ErrorCode::Constraint);
        assert!(
            fk_esc_err
                .to_string()
                .contains("FOREIGN KEY constraint failed"),
            "expected immediate FK escalation on FAIL, got: {fk_esc_err}"
        );
        let chd_after_imm = conn
            .query("SELECT id FROM temp.g38_chd ORDER BY id;", &[])
            .unwrap();
        assert_eq!(format_rows_pipe(&chd_after_imm.rows), "10\n");

        // (c) `OR FAIL` with deferred FK violation in autocommit rolls back the entire transaction at commit!
        conn.execute_batch(
            "CREATE TEMP TABLE g38_dpar(id INTEGER PRIMARY KEY);
             CREATE TEMP TABLE g38_dchd(
                 id INTEGER PRIMARY KEY,
                 pid INT REFERENCES g38_dpar(id) DEFERRABLE INITIALLY DEFERRED,
                 u INT UNIQUE
             );
             INSERT INTO temp.g38_dpar VALUES(1);
             INSERT INTO temp.g38_dchd VALUES(1, 1, 50);",
        )
        .unwrap();
        let def_fk_err = conn
            .execute("INSERT OR FAIL INTO temp.g38_dchd VALUES(2, 999, 51), (3, 1, 50);")
            .unwrap_err();
        assert_eq!(def_fk_err.code, ErrorCode::Constraint);
        let dchd_after = conn
            .query("SELECT id FROM temp.g38_dchd ORDER BY id;", &[])
            .unwrap();
        assert_eq!(format_rows_pipe(&dchd_after.rows), "1\n");

        // (d) `UPDATE OR REPLACE` cursor pinning (`SQLITE_CONSTRAINT_PINNED`) when `DELETE` trigger mutates target table
        conn.execute_batch(
            "CREATE TEMP TABLE g38_pin(a UNIQUE, b UNIQUE);
             INSERT INTO temp.g38_pin VALUES(1, NULL), (0, NULL);
             CREATE TEMP TRIGGER tr_g38_pin AFTER DELETE ON g38_pin BEGIN
                 DELETE FROM g38_pin;
             END;",
        )
        .unwrap();
        let pin_err = conn
            .execute("UPDATE OR REPLACE temp.g38_pin SET b = 1;")
            .unwrap_err();
        assert_eq!(pin_err.code, ErrorCode::Constraint);
        assert_eq!(pin_err.message, "constraint failed");
        let pin_rows = conn
            .query("SELECT a, b FROM temp.g38_pin ORDER BY rowid;", &[])
            .unwrap();
        assert_eq!(format_rows_pipe(&pin_rows.rows), "1|\n0|\n");

        // (e) `PrepareMode::V2` auto-reprepare vs `PrepareMode::V1` `SQLITE_SCHEMA` across `CREATE TRIGGER` & `DROP TRIGGER`
        let mut ins_v2 = conn
            .prepare_with_mode("INSERT INTO main.countdown VALUES(10);", PrepareMode::V2)
            .unwrap();
        let mut ins_v1 = conn
            .prepare_with_mode("INSERT INTO main.countdown VALUES(20);", PrepareMode::V1)
            .unwrap();
        conn.execute(
            "CREATE TRIGGER main.tr_reprep_probe AFTER INSERT ON countdown BEGIN
                 SELECT 1;
             END;",
        )
        .unwrap();

        assert_eq!(ins_v2.step().unwrap(), StepResult::Done);
        ins_v2.finalize().unwrap();

        let v1_err = ins_v1.step().unwrap_err();
        assert_eq!(v1_err.code, ErrorCode::Schema);
        let v1_fin_err = ins_v1.finalize().unwrap_err();
        assert_eq!(v1_fin_err.code, ErrorCode::Schema);

        // Clean up the probe row and probe trigger so on-disk `main.db` matches C SQLite 1-to-1!
        conn.execute_batch(
            "DROP TRIGGER main.tr_reprep_probe;
             DELETE FROM main.countdown WHERE n > 5;",
        )
        .unwrap();

        // (f) `db_config_enable_trigger(Some(false))` suppresses permanent triggers while keeping TEMP triggers active
        assert!(!conn.db_config_enable_trigger(Some(false)));
        conn.execute("INSERT INTO main.countdown VALUES(100);")
            .unwrap();
        let max_n = conn
            .query("SELECT count(*) FROM main.countdown WHERE n >= 90;", &[])
            .unwrap();
        assert_eq!(
            max_n.rows[0].values(),
            &[Value::Integer(1)],
            "permanent countdown trigger must not fire while enable_trigger=false"
        );
        conn.execute("DELETE FROM main.countdown WHERE n = 100;")
            .unwrap();
        assert!(conn.db_config_enable_trigger(Some(true)));

        conn.try_close().unwrap();
    }

    let inspect_sql = "\
ATTACH 'aux.db' AS aux;
ATTACH 'aux_vac.db' AS av;
PRAGMA main.integrity_check;
PRAGMA aux.integrity_check;
PRAGMA av.integrity_check;
SELECT type, name, tbl_name, sql FROM main.sqlite_schema WHERE name != 'sqlite_sequence' ORDER BY type, name;
SELECT acc_id, acct_code, acct_balance FROM main.ledger_accounts ORDER BY acc_id;
SELECT ord_id, acc_cascade, acc_null, acc_default, amount FROM main.orders ORDER BY ord_id;
SELECT seq, tag, detail FROM main.audit_log ORDER BY seq;
SELECT n FROM main.countdown ORDER BY n;
SELECT ship_id, label, weight FROM aux.shipments ORDER BY ship_id;
SELECT msg FROM aux.ship_log ORDER BY rowid;
SELECT ship_id, label, weight FROM av.shipments ORDER BY ship_id;
SELECT msg FROM av.ship_log ORDER BY rowid;
";

    // 3. C SQLite 3.54.1 inspects both C-produced and Rust-produced DBs -> identical schema, rows, & integrity!
    let (ok_cc, out_cc, err_cc) = run_c_sqlite_in_dir(&c_dir, "main.db", inspect_sql);
    assert!(ok_cc, "C inspect on C DB failed: {err_cc}");
    let (ok_cr, out_cr, err_cr) = run_c_sqlite_in_dir(&rust_dir, "main.db", inspect_sql);
    assert!(ok_cr, "C inspect on Rust DB failed: {err_cr}");
    assert_eq!(
        out_cr, out_cc,
        "Rust Phase 4 DB state diverged from C SQLite 3.54.1"
    );

    // 4. Rust `Connection` opens C-SQLite-produced WAL `.db` files, runs triggers on them, and verifies integrity!
    {
        let conn_on_c = Connection::open(&c_main).unwrap();
        conn_on_c
            .execute(&format!(
                "ATTACH '{}' AS aux; ATTACH '{}' AS av;",
                c_aux.display(),
                c_vac.display()
            ))
            .unwrap();
        for pragma in [
            "PRAGMA main.integrity_check;",
            "PRAGMA aux.integrity_check;",
            "PRAGMA av.integrity_check;",
        ] {
            let ic = conn_on_c.query(pragma, &[]).unwrap();
            assert_eq!(format_rows_pipe(&ic.rows), "ok\n");
        }
        let r_audit = conn_on_c
            .query(
                "SELECT seq, tag, detail FROM main.audit_log ORDER BY seq;",
                &[],
            )
            .unwrap();
        let (_, c_audit, _) = run_c_sqlite_in_dir(
            &c_dir,
            "main.db",
            "SELECT seq, tag, detail FROM main.audit_log ORDER BY seq;",
        );
        assert_eq!(format_rows_pipe(&r_audit.rows), c_audit);
        conn_on_c.try_close().unwrap();
    }
}

#[test]
fn phase4_non_phase5_sections_of_ignored_trigger_tcl_suites() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read_lines = |rel: &str| -> Vec<String> {
        fs::read_to_string(workspace_root.join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"))
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    };

    // 1. `trigger1.test`: all 92 non-window subtests (omitting only `trigger1-22.10` `sum(b) OVER(...)`)
    {
        let lines = read_lines("sqlite/test/trigger1.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..806]);
        sliced.extend_from_slice(&lines[829..]);
        assert_tcl_section_ok(
            "trigger1",
            "trigger1.test (all 92 non-window subtests)",
            &sliced.join("\n"),
            92,
        );
    }

    // 2. `triggerE.test`: all 24 non-window subtests (omitting only loop item `11` `max(b) OVER(ORDER BY $1)`)
    {
        let lines = read_lines("sqlite/test/triggerE.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..60]);
        sliced.extend_from_slice(&lines[62..]);
        assert_tcl_section_ok(
            "triggerE",
            "triggerE.test (all 24 non-window subtests)",
            &sliced.join("\n"),
            24,
        );
    }

    // 3. `upfrom2.test`: all 44 `BEFORE`/`AFTER`/`INSTEAD OF` trigger + non-window `UPDATE ... FROM` subtests (`1.1..6.2`)
    {
        let lines = read_lines("sqlite/test/upfrom2.test");
        let mut sliced = lines[..384].to_vec();
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok(
            "upfrom2",
            "upfrom2.test (1.1..6.2 all 44 non-window subtests)",
            &sliced.join("\n"),
            44,
        );
    }
}
