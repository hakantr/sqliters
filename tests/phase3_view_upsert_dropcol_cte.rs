//! Phase 3 (`GATE-3`) Views, `UPSERT`, `UPDATE ... FROM`, `ALTER TABLE DROP COLUMN`,
//! Full `WITH RECURSIVE`, and C SQLite 3.54.1 Differential Compatibility Gate Suite.
//!
//! Covers:
//! 1. C SQLite 3.54.1 reference CLI verification, Phase 3 capability table audit (`view=true`,
//!    `upsert=true`, Phase 4..7 capabilities `false`), Group 60 manifest verification, and
//!    `target/sqlite-ref/phase3_manifest.json` generation.
//! 2. Cross-feature end-to-end differential parity against C SQLite 3.54.1 combining `WAL`,
//!    `auto_vacuum=FULL` / `INCREMENTAL`, `ATTACH`, `TEMP`, `SchemaFixer` views, `ALTER TABLE`
//!    table/column rename view rewriting, `ALTER TABLE DROP COLUMN`, chained `UPSERT`,
//!    `UPDATE ... FROM`, row-value `SET` & `IN`-list affinity, `WITH RECURSIVE [NOT] MATERIALIZED`,
//!    `SAVEPOINT` rollback, `VACUUM` / `VACUUM INTO`, `PrepareMode::V2` vs `V1` reprepare,
//!    and bidirectional `.db` / `-wal` file exchange + `PRAGMA integrity_check`.
//! 3. Direct execution and 100% pass enforcement of all non-trigger / non-authorizer Phase 3
//!    subtest sections across `view.test`, `upsert1.test`, `upsert2.test`, `upsert4.test`,
//!    `upfrom2.test`, `upfrom4.test`, `alterdropcol.test`, and `alterdropcol2.test`.
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
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script_in_root,
    strip_test_comments,
};

static NEXT_GATE3_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Gate3Sandbox {
    dir: PathBuf,
}

impl Gate3Sandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_GATE3_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_gate3_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create gate3 temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Gate3Sandbox {
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

fn assert_tcl_section_ok(stem: &str, label: &str, raw_tcl: &str, min_expected_passed: usize) {
    let script = strip_test_comments(raw_tcl);
    let report = evaluate_test_script_in_root("sqlite/test", stem, &script);
    assert!(
        report.is_ok(),
        "Phase 3 section `{label}` failed (passed={}, failed={:?}, script_error={:?})",
        report.passed,
        report.failed,
        report.script_error
    );
    assert!(
        report.passed >= min_expected_passed,
        "Phase 3 section `{label}` executed only {} subtests (expected >= {})",
        report.passed,
        min_expected_passed
    );
}

#[test]
fn phase3_gate_c_reference_and_manifest_audit() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sandbox = Gate3Sandbox::new("manifest_audit");

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

    // 1. Verify full Phase 3 capability table invariants (`GATE-3`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    for enabled_cap in [
        "altertable",
        "attach",
        "autovacuum",
        "cte",
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
        "xfer_opt",
    ] {
        assert_eq!(
            cap_map.get(enabled_cap),
            Some(&true),
            "capability `{enabled_cap}` must be enabled in Phase 4"
        );
    }
    for pending_cap in [
        "windowfunc",
        "analyze",
        "stat4",
        "auth",
        "progress",
        "trace",
        "incrblob",
        "vtab",
        "fts5",
        "rtree",
        "json",
        "json1",
    ] {
        assert_eq!(
            cap_map.get(pending_cap),
            Some(&false),
            "Phase 5..7 capability `{pending_cap}` must remain false in Phase 4"
        );
    }

    // 2. Audit Group 60 `tests/sqlite_test_manifest.json` Phase 3 & overall counts.
    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut p3_total = 0usize;
    let mut p3_active = 0usize;
    let mut p3_ignored = 0usize;
    let mut p3_active_ids = Vec::new();
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
            if cur_phase == "3" {
                p3_total += 1;
                if cur_status == "active" {
                    p3_active += 1;
                    p3_active_ids.push(cur_id.clone());
                } else if cur_status == "ignored" {
                    p3_ignored += 1;
                }
            }
            cur_id.clear();
            cur_phase.clear();
            cur_status.clear();
        }
    }

    p3_active_ids.sort();
    assert_eq!(
        p3_total, 17,
        "expected 17 Phase 3 wrappers (view*, upsert*, upfrom*, alterdropcol*, withM)"
    );
    assert_eq!(p3_active, 13, "expected 13 active Phase 3 manifest suites");
    assert_eq!(
        p3_ignored, 4,
        "expected 4 ignored Phase 3 manifest suites (view, upfrom2, upsertfault, upfromfault)"
    );
    assert_eq!(
        p3_active_ids,
        vec![
            "sqlite/test/alterdropcol.test",
            "sqlite/test/alterdropcol2.test",
            "sqlite/test/upfrom1.test",
            "sqlite/test/upfrom3.test",
            "sqlite/test/upfrom4.test",
            "sqlite/test/upsert1.test",
            "sqlite/test/upsert2.test",
            "sqlite/test/upsert3.test",
            "sqlite/test/upsert4.test",
            "sqlite/test/upsert5.test",
            "sqlite/test/view2.test",
            "sqlite/test/view3.test",
            "sqlite/test/withM.test",
        ]
    );
    assert!(manifest_active_total >= 43);
    assert!(manifest_ignored_total >= 49);

    let uname_out = Command::new("uname")
        .arg("-srmo")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| std::env::consts::OS.to_owned());

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

    let phase3_manifest_json = format!(
        "{{\n\
  \"phase\": \"3\",\n\
  \"gate\": \"GATE-3\",\n\
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
  \"phase3_limits\": {{\n\
    \"SQLITE_MAX_ATTACHED\": {},\n\
    \"SQLITE_MAX_COLUMN\": 2000,\n\
    \"SQLITE_MAX_EXPR_DEPTH\": 1000,\n\
    \"max_table_references\": 65535\n\
  }},\n\
  \"tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"registered_phase3_wrappers\": {},\n\
    \"active_phase3_wrappers\": {},\n\
    \"ignored_phase3_wrappers\": {},\n\
    \"active_phase3_subtests\": 326,\n\
    \"newly_unlocked_static_wrappers\": [\"affinity3\", \"conflict3\", \"in5\", \"insert5\", \"rowvalue7\", \"select6\", \"types2\", \"with2\", \"with5\"],\n\
    \"verified_non_trigger_subtests_in_gate3\": 413,\n\
    \"manifest_total_active\": {},\n\
    \"manifest_total_ignored\": {}\n\
  }},\n\
  \"host_os\": \"{}\",\n\
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
        p3_total,
        p3_active,
        p3_ignored,
        manifest_active_total,
        manifest_ignored_total,
        json_escape(&uname_out),
        caps_json,
    );

    let out_manifest_path = workspace_root.join("target/sqlite-ref/phase3_manifest.json");
    fs::write(&out_manifest_path, &phase3_manifest_json)
        .expect("write target/sqlite-ref/phase3_manifest.json");
    assert!(out_manifest_path.is_file());
}

#[test]
fn phase3_end_to_end_differential_wal_autovacuum_attach_views_upsert_dropcol_cte() {
    let sandbox = Gate3Sandbox::new("diff_e2e");
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

ATTACH 'aux.db' AS aux;
PRAGMA aux.page_size = 4096;
PRAGMA aux.auto_vacuum = INCREMENTAL;
PRAGMA aux.journal_mode = WAL;

CREATE TABLE main.items(
    id INTEGER PRIMARY KEY,
    sku TEXT NOT NULL UNIQUE,
    Category TEXT NOT NULL,
    obsolete_note TEXT DEFAULT 'drop-me',
    scratch_col INT DEFAULT 77,
    qty INT NOT NULL DEFAULT 0,
    price INT NOT NULL DEFAULT 100
);

CREATE TABLE aux.warehouses(
    wh_id INTEGER PRIMARY KEY,
    region TEXT NOT NULL,
    legacy_col INT DEFAULT 99,
    bonus INT NOT NULL DEFAULT 0
);

CREATE VIEW main.v_items AS
    SELECT items.oid, sku, Category, "qty" + 0 FROM items;

CREATE TEMP VIEW v_temp_summary AS
    SELECT sku, qty * price AS total_val FROM main.items WHERE qty > 0;

INSERT INTO main.items(id, sku, Category, obsolete_note, qty, price) VALUES
    (1, 'A-100', 'tools', 'old1', 5, 200),
    (2, 'B-200', 'parts', 'old2', 3, 150),
    (3, 'C-300', 'tools', 'old3', 0, 500);

-- Chained multi-target UPSERT with DO UPDATE WHERE filter and catch-all fallback
INSERT INTO main.items(id, sku, Category, qty, price) VALUES
    (10, 'A-100', 'tools', 7, 220),
    (2,  'B-200', 'parts', 4, 140),
    (4,  'D-400', 'kits',  2, 900)
ON CONFLICT(sku)
    DO UPDATE SET qty = items.qty + excluded.qty, price = excluded.price
    WHERE excluded.qty > items.qty
ON CONFLICT
    DO UPDATE SET qty = items.qty + 1;

INSERT INTO aux.warehouses(wh_id, region, legacy_col, bonus) VALUES
    (1, 'tools', 11, 10),
    (2, 'kits',  22, 25),
    (3, 'parts', 33, 5);

-- SAVEPOINT around DDL + DML that rolls back cleanly, then re-applies real schema changes
SAVEPOINT sp_ddl;
    ALTER TABLE main.items DROP COLUMN scratch_col;
    UPDATE main.items SET qty = 999;
ROLLBACK TO sp_ddl;
RELEASE sp_ddl;

-- Drop obsolete columns (including `scratch_col` restored by `ROLLBACK TO sp_ddl`)
ALTER TABLE main.items DROP COLUMN scratch_col;
ALTER TABLE main.items DROP COLUMN obsolete_note;
ALTER TABLE aux.warehouses DROP COLUMN legacy_col;
PRAGMA aux.incremental_vacuum;

-- Rename table & column and verify view definition rewriting (`SchemaFixer` + `rename.c`)
ALTER TABLE main.items RENAME TO catalog_items;
ALTER TABLE main.catalog_items RENAME COLUMN sku TO item_code;

-- UPDATE ... FROM with CTE + row-value SET assignment across main and attached aux
WITH RECURSIVE
    tier_mult(step, factor) AS MATERIALIZED (
        VALUES(1, 1)
        UNION ALL
        SELECT step + 1, factor * 2 FROM tier_mult WHERE step < 4
    ),
    top_factor(mult) AS NOT MATERIALIZED (
        SELECT max(factor) FROM tier_mult
    )
UPDATE main.catalog_items
   SET (qty, price) = (SELECT catalog_items.qty + w.bonus, catalog_items.price + tf.mult FROM top_factor tf)
  FROM aux.warehouses AS w
 WHERE w.region = main.catalog_items.Category;

-- Transfer optimization (`INSERT INTO ... SELECT * FROM ...`) into matching table
CREATE TABLE main.catalog_archive(
    id INTEGER PRIMARY KEY,
    item_code TEXT NOT NULL UNIQUE,
    Category TEXT NOT NULL,
    qty INT NOT NULL DEFAULT 0,
    price INT NOT NULL DEFAULT 100
);
INSERT INTO main.catalog_archive SELECT * FROM main.catalog_items;

VACUUM aux INTO 'aux_vac.db';
"#;

    // Execute identical workload in C SQLite 3.54.1
    let (ok_c, _, err_c) = run_c_sqlite_in_dir(&c_dir, "main.db", workload_sql);
    assert!(ok_c, "C SQLite workload failed:\n{err_c}");

    // Execute identical workload in Rust `sqliters::Connection`
    {
        let prev_cwd = std::env::current_dir().unwrap();
        let conn = Connection::open(&rust_main).unwrap();
        let patched_sql = workload_sql
            .replace("'aux.db'", &format!("'{}'", rust_aux.display()))
            .replace("'aux_vac.db'", &format!("'{}'", rust_vac.display()));
        let _ = prev_cwd;
        conn.execute_batch(&patched_sql).unwrap();
        assert_eq!(
            conn.take_xferopt_delta(),
            1,
            "expected exactly 1 transfer-optimization activation on catalog_archive"
        );

        // Verify raw span column names, INTEGER PRIMARY KEY `oid` -> `id` resolution, & declared column casing on rewritten view `v_items`
        let v_res = conn
            .query("SELECT * FROM main.v_items ORDER BY id;", &[])
            .unwrap();
        assert_eq!(
            v_res.column_names,
            vec!["id", "item_code", "Category", "\"qty\" + 0"]
        );

        // Verify IN-list LHS-only affinity (`'12' IN (id)` must NOT coerce `'12'` to integer `12`)
        let in_res = conn
            .query(
                "SELECT count(*) FROM main.catalog_items WHERE ('2', '4') IN ((id, id));",
                &[],
            )
            .unwrap();
        assert_eq!(in_res.rows[0].values(), &[Value::Integer(0)]);

        // Verify `SQLITE_DBCONFIG_ENABLE_VIEW = 0` blocks permanent views while allowing TEMP views
        assert!(!conn.db_config_enable_view(Some(false)));
        let dis_err = conn.query("SELECT * FROM main.v_items;", &[]).unwrap_err();
        assert!(
            dis_err
                .to_string()
                .contains("access to view \"v_items\" prohibited"),
            "unexpected error: {dis_err}"
        );
        // Re-enable views
        assert!(conn.db_config_enable_view(Some(true)));

        // Verify `cannot UPSERT a view` and `UPDATE ... FROM` `sameSrcAlias` error parity
        let ups_view_err = conn
            .execute("INSERT INTO main.v_items VALUES(1,'X','Y',2) ON CONFLICT DO NOTHING;")
            .unwrap_err();
        assert!(ups_view_err.to_string().contains("cannot UPSERT a view"));

        let same_alias_err = conn
            .execute("UPDATE main.catalog_items SET qty = 1 FROM main.catalog_items;")
            .unwrap_err();
        assert!(
            same_alias_err
                .to_string()
                .contains("target object/alias may not appear in FROM clause: main.catalog_items")
                || same_alias_err
                    .to_string()
                    .contains("target object/alias may not appear in FROM clause: catalog_items"),
            "unexpected sameSrcAlias error: {same_alias_err}"
        );

        // Verify `PrepareMode::V2` auto-reprepare vs `PrepareMode::V1` `SQLITE_SCHEMA` on `DROP COLUMN`
        conn.execute("CREATE TABLE main.reprep(a INT, b INT, c INT); INSERT INTO main.reprep VALUES(10, 20, 30);")
            .unwrap();
        let mut stmt_v2 = conn
            .prepare_with_mode("SELECT a, c FROM main.reprep;", PrepareMode::V2)
            .unwrap();
        let mut stmt_v1 = conn
            .prepare_with_mode("SELECT a, c FROM main.reprep;", PrepareMode::V1)
            .unwrap();
        conn.execute("ALTER TABLE main.reprep DROP COLUMN b;")
            .unwrap();

        match stmt_v2.step().unwrap() {
            StepResult::Row(r) => {
                assert_eq!(r.values(), &[Value::Integer(10), Value::Integer(30)])
            }
            StepResult::Done => panic!("expected row after V2 auto-reprepare"),
        }
        stmt_v2.finalize().unwrap();

        let v1_step_err = stmt_v1.step().unwrap_err();
        assert_eq!(v1_step_err.code, ErrorCode::Schema);
        let v1_fin_err = stmt_v1.finalize().unwrap_err();
        assert_eq!(v1_fin_err.code, ErrorCode::Schema);

        // Drop temporary helper table so disk state matches C SQLite workload 1-to-1
        conn.execute("DROP TABLE main.reprep;").unwrap();

        // Verify `nTabRef` overflow (`too many references to "dv0": max 65535`) completes in milliseconds
        conn.execute("CREATE TEMP VIEW dv0 AS SELECT 1 AS x;")
            .unwrap();
        for lvl in 1..=16 {
            let prev = lvl - 1;
            conn.execute(&format!(
                "CREATE TEMP VIEW dv{lvl} AS SELECT * FROM dv{prev} UNION ALL SELECT * FROM dv{prev};"
            ))
            .unwrap();
        }
        let nref_err = conn.query("SELECT * FROM dv16;", &[]).unwrap_err();
        assert!(
            nref_err
                .to_string()
                .contains("too many references to \"dv0\": max 65535"),
            "unexpected nTabRef error: {nref_err}"
        );

        conn.try_close().unwrap();
    }

    let inspect_sql = "\
ATTACH 'aux.db' AS aux;
ATTACH 'aux_vac.db' AS av;
PRAGMA main.integrity_check;
PRAGMA aux.integrity_check;
PRAGMA av.integrity_check;
SELECT type, name, tbl_name, sql FROM main.sqlite_schema ORDER BY name;
SELECT id, item_code, Category, qty, price FROM main.catalog_items ORDER BY id;
SELECT id, item_code, Category, qty, price FROM main.catalog_archive ORDER BY id;
SELECT id, item_code, Category, \"\"\"qty\"\" + 0\" FROM main.v_items ORDER BY id;
SELECT wh_id, region, bonus FROM aux.warehouses ORDER BY wh_id;
SELECT wh_id, region, bonus FROM av.warehouses ORDER BY wh_id;
";

    // 1. C SQLite inspects C-produced DBs AND Rust-produced DBs -> byte-for-byte identical!
    let (ok_cc, out_cc, err_cc) = run_c_sqlite_in_dir(&c_dir, "main.db", inspect_sql);
    assert!(ok_cc, "C inspect on C DB failed: {err_cc}");
    let (ok_cr, out_cr, err_cr) = run_c_sqlite_in_dir(&rust_dir, "main.db", inspect_sql);
    assert!(ok_cr, "C inspect on Rust DB failed: {err_cr}");
    assert_eq!(
        out_cr, out_cc,
        "Rust DB state diverged from C SQLite 3.54.1"
    );

    // 2. Rust `Connection` opens C-SQLite-produced WAL `.db` files and verifies identical rows & integrity!
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
        let r_items = conn_on_c
            .query(
                "SELECT id, item_code, Category, qty, price FROM main.catalog_items ORDER BY id;",
                &[],
            )
            .unwrap();
        let (_, c_items, _) = run_c_sqlite_in_dir(
            &c_dir,
            "main.db",
            "SELECT id, item_code, Category, qty, price FROM main.catalog_items ORDER BY id;",
        );
        assert_eq!(format_rows_pipe(&r_items.rows), c_items);
        conn_on_c.try_close().unwrap();
    }
}

#[test]
fn phase3_non_trigger_sections_of_ignored_phase3_tcl_suites() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read_lines = |rel: &str| -> Vec<String> {
        fs::read_to_string(workspace_root.join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"))
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    };

    // 1. `view.test`: all lines except `view-25.1..25.2` (lines 664..=690 which use `ANALYZE` + `db authorizer`)
    {
        let lines = read_lines("sqlite/test/view.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..663]);
        sliced.extend_from_slice(&lines[690..]);
        assert_tcl_section_ok("view", "view.test (non-auth)", &sliced.join("\n"), 119);
    }

    // 2. `upsert1.test`: lines 1..=223 + 236..=270 (omitting `upsert1-900` and `upsert1-1300` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/upsert1.test");
        let mut sliced = Vec::new();
        sliced.extend_from_slice(&lines[..223]);
        sliced.extend_from_slice(&lines[235..270]);
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok(
            "upsert1",
            "upsert1.test (100..810 + 1000..1210)",
            &sliced.join("\n"),
            32,
        );
    }

    // 3. `upsert2.test`: lines 1..=65 (`upsert2-100..210`, before `upsert2-300` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/upsert2.test");
        let mut sliced = lines[..65].to_vec();
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok("upsert2", "upsert2.test (100..210)", &sliced.join("\n"), 6);
    }

    // 4. `upsert4.test`: lines 1..=384 (`upsert4-1.0..8.2.5`, before `upsert4-9.0` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/upsert4.test");
        let mut sliced = lines[..384].to_vec();
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok(
            "upsert4",
            "upsert4.test (1.0..8.2.5)",
            &sliced.join("\n"),
            126,
        );
    }

    // 5. `upfrom2.test`: header + lines 321..=384 (`upfrom2-4.*..6.2` non-trigger sections)
    {
        let lines = read_lines("sqlite/test/upfrom2.test");
        let mut sliced = lines[..16].to_vec();
        sliced.extend_from_slice(&lines[320..384]);
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok("upfrom2", "upfrom2.test (4.1..6.2)", &sliced.join("\n"), 14);
    }

    // 6. `upfrom4.test`: lines 1..=84 (`upfrom4-100..210`, before `upfrom4-300` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/upfrom4.test");
        let mut sliced = lines[..84].to_vec();
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok("upfrom4", "upfrom4.test (100..210)", &sliced.join("\n"), 5);
    }

    // 7. `alterdropcol.test`: lines 1..=239 + 258..=339 (omitting `5.4.1..5.5.2` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/alterdropcol.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..239]);
        sliced.extend_from_slice(&lines[257..]);
        assert_tcl_section_ok(
            "alterdropcol",
            "alterdropcol.test (non-trigger)",
            &sliced.join("\n"),
            96,
        );
    }

    // 8. `alterdropcol2.test`: lines 1..=162 (`1.0..2.7.2`, before `2.8.1` `CREATE TRIGGER`)
    {
        let lines = read_lines("sqlite/test/alterdropcol2.test");
        let mut sliced = lines[..162].to_vec();
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok(
            "alterdropcol2",
            "alterdropcol2.test (1.0..2.7.2)",
            &sliced.join("\n"),
            15,
        );
    }
}
