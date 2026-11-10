//! Phase 5 (`GATE-5`) end-to-end verification gate:
//!
//! 1. Audits `PHASE1_CAPABILITY_TABLE` (`windowfunc = true`, `analyze = true`, Phase 6–7 extension
//!    flags `fts3`, `fts4`, `fts5`, `rtree`, `vtab`, `json1` kept `false`), verifies the Group 60
//!    Phase 5 (`window*`, `analyze*`) and promoted Phase 3/4 (`upfrom2`, `trigger1`, `triggerE`)
//!    entries in `tests/sqlite_test_manifest.json`, and writes
//!    `target/sqlite-ref/phase5_manifest.json`.
//! 2. Executes a rich multi-database (`main` + `ATTACH 'aux.db' AS aux`), WAL + `INCREMENTAL`
//!    auto-vacuum workload exercising `ANALYZE`, `sqlite_stat1` manual overrides +
//!    `ANALYZE sqlite_schema`, partial/expression/covering/`WITHOUT ROWID` index planning,
//!    window functions (`ROW_NUMBER`, `RANK`, `DENSE_RANK`, `NTILE`, `LAG`, `LEAD`, `FIRST_VALUE`,
//!    `LAST_VALUE`, `NTH_VALUE`, framed `SUM`/`AVG`/`COUNT`/`GROUP_CONCAT` with `ROWS`, `RANGE`,
//!    `GROUPS`, `EXCLUDE`, `FILTER`, and named `WINDOW` chaining), views and triggers invoking
//!    window functions, `ALTER TABLE RENAME` rewriting both window views/triggers and
//!    `sqlite_stat1`, and `VACUUM INTO` identically on C SQLite 3.54.1 (`target/sqlite-ref/sqlite3`)
//!    and Rust `sqliters`, followed by bidirectional C ↔ Rust on-disk verification and
//!    `PRAGMA integrity_check`.
//! 3. Verifies the pure-SQL sections of the Phase 6/7-gated Tcl window test suites (`window6.test`,
//!    `windowB.test`, and `windowE.test`) pass 100% with zero failures.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters_core::{Connection, Row};
use sqliters_tcltest::{PHASE1_CAPABILITY_TABLE, evaluate_test_script};
use sqliters_types::{LockGeometry, Value, format_sqlite_real};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

const SQLITE_3541_EXPECTED_VERSION: &str = "3.54.1";

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p5gate_{prefix}_{pid}_{id}"));
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

fn assert_tcl_section_ok(
    suite_name: &str,
    section_desc: &str,
    script: &str,
    expected_min_passed: usize,
) {
    let sname = suite_name.to_owned();
    let sc = script.to_owned();
    let report = std::thread::Builder::new()
        .name(format!("p5gate-{suite_name}"))
        .stack_size(32 * 1024 * 1024)
        .spawn(move || evaluate_test_script(&sname, &sc))
        .expect("spawn tcl thread")
        .join()
        .expect("join tcl thread");
    assert!(
        report.is_ok(),
        "{section_desc} failed (passed={}, failed={}, script_error={:?}): {:?}",
        report.passed,
        report.failed.len(),
        report.script_error,
        report.failed.iter().take(10).collect::<Vec<_>>()
    );
    assert!(
        report.passed >= expected_min_passed,
        "{section_desc}: expected at least {expected_min_passed} passed subtests, got {}",
        report.passed
    );
}

#[test]
fn phase5_gate_c_reference_and_manifest_audit() {
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

    assert_eq!(LockGeometry::PRODUCTION.pending_byte_offset(), 0x4000_0000);
    assert_eq!(LockGeometry::TEST_64K.pending_byte_offset(), 0x0001_0000);

    // 1. Verify full capability table invariants for Phase 5 (`GATE-5`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    for enabled_cap in [
        "altertable",
        "analyze",
        "attach",
        "autovacuum",
        "between_opt",
        "cte",
        "explain",
        "foreignkey",
        "generated",
        "incr_vacuum",
        "or_opt",
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
        "xfer_opt",
    ] {
        assert_eq!(
            cap_map.get(enabled_cap),
            Some(&true),
            "capability `{enabled_cap}` must be enabled in Phase 5"
        );
    }
    for pending_cap in [
        "stat4", "auth", "progress", "trace", "incrblob", "vtab", "fts3", "fts4", "fts5", "rtree",
        "json", "json1",
    ] {
        assert_eq!(
            cap_map.get(pending_cap),
            Some(&false),
            "Phase 6..7 capability `{pending_cap}` must remain false in Phase 5"
        );
    }

    // 2. Audit Group 60 `tests/sqlite_test_manifest.json` Phase 3, 4, 5 & overall counts.
    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut p3_active = 0usize;
    let mut p3_ignored = 0usize;
    let mut p4_active = 0usize;
    let mut p4_ignored = 0usize;
    let mut p5_total = 0usize;
    let mut p5_active = 0usize;
    let mut p5_ignored = 0usize;
    let mut p5_active_ids = Vec::new();
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
            match cur_phase.as_str() {
                "3" => {
                    if cur_status == "active" {
                        p3_active += 1;
                    } else if cur_status == "ignored" {
                        p3_ignored += 1;
                    }
                }
                "4" => {
                    if cur_status == "active" {
                        p4_active += 1;
                    } else if cur_status == "ignored" {
                        p4_ignored += 1;
                    }
                }
                "5" => {
                    p5_total += 1;
                    if cur_status == "active" {
                        p5_active += 1;
                        p5_active_ids.push(cur_id.clone());
                    } else if cur_status == "ignored" {
                        p5_ignored += 1;
                    }
                }
                _ => {}
            }
            cur_id.clear();
            cur_phase.clear();
            cur_status.clear();
        }
    }

    p5_active_ids.sort();
    assert_eq!(
        p3_active, 14,
        "expected 14 active Phase 3 manifest suites (including promoted upfrom2)"
    );
    assert_eq!(p3_ignored, 3, "expected 3 ignored Phase 3 manifest suites");
    assert_eq!(
        p4_active, 17,
        "expected 17 active Phase 4 manifest suites (including promoted trigger1 & triggerE)"
    );
    assert_eq!(
        p4_ignored, 1,
        "expected 1 ignored Phase 4 manifest suite (trigger6)"
    );
    assert_eq!(
        p5_total, 31,
        "expected 31 Phase 5 wrappers (17 window*.test + 14 analyze*.test/analyzer1.test)"
    );
    assert_eq!(p5_active, 16, "expected 16 active Phase 5 manifest suites");
    assert_eq!(
        p5_ignored, 15,
        "expected 15 ignored Phase 5 manifest suites"
    );
    assert_eq!(
        p5_active_ids,
        vec![
            "sqlite/test/analyze.test",
            "sqlite/test/analyze4.test",
            "sqlite/test/analyze7.test",
            "sqlite/test/analyzeC.test",
            "sqlite/test/window1.test",
            "sqlite/test/window2.test",
            "sqlite/test/window3.test",
            "sqlite/test/window4.test",
            "sqlite/test/window7.test",
            "sqlite/test/window8.test",
            "sqlite/test/window9.test",
            "sqlite/test/windowA.test",
            "sqlite/test/windowC.test",
            "sqlite/test/windowD.test",
            "sqlite/test/windowerr.test",
            "sqlite/test/windowpushd.test",
        ]
    );
    assert_eq!(manifest_active_total, 62);
    assert_eq!(manifest_ignored_total, 61);

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

    let mut caps_json = String::new();
    for (idx, (k, v)) in PHASE1_CAPABILITY_TABLE.iter().enumerate() {
        let comma = if idx + 1 < PHASE1_CAPABILITY_TABLE.len() {
            ","
        } else {
            ""
        };
        caps_json.push_str(&format!("    \"{k}\": {v}{comma}\n"));
    }

    let active_p5_json = p5_active_ids
        .iter()
        .map(|id| format!("      \"{id}\""))
        .collect::<Vec<_>>()
        .join(",\n");

    let phase5_manifest_json = format!(
        "{{\n\
  \"phase\": \"5\",\n\
  \"gate\": \"GATE-5\",\n\
  \"c_reference_sqlite_version\": \"{c_ver}\",\n\
  \"c_reference_sqlite_source_id\": \"{c_src_id}\",\n\
  \"os_kernel\": \"{uname_out}\",\n\
  \"filesystem_type\": \"{fstype_out}\",\n\
  \"lock_geometry\": {{\n\
    \"production_pending_byte\": \"0x40000000\",\n\
    \"test_64k_pending_byte\": \"0x00010000\"\n\
  }},\n\
  \"sqlite_test_manifest_summary\": {{\n\
    \"phase3_active\": {p3_active},\n\
    \"phase3_ignored\": {p3_ignored},\n\
    \"phase4_active\": {p4_active},\n\
    \"phase4_ignored\": {p4_ignored},\n\
    \"phase5_total\": {p5_total},\n\
    \"phase5_active\": {p5_active},\n\
    \"phase5_ignored\": {p5_ignored},\n\
    \"phase5_active_suites\": [\n{active_p5_json}\n    ],\n\
    \"overall_active_wrappers\": {manifest_active_total},\n\
    \"overall_ignored_wrappers\": {manifest_ignored_total}\n\
  }},\n\
  \"capabilities\": {{\n{caps_json}  }}\n\
}}\n"
    );

    let out_path = workspace_root.join("target/sqlite-ref/phase5_manifest.json");
    fs::write(&out_path, &phase5_manifest_json)
        .expect("write target/sqlite-ref/phase5_manifest.json");
    assert!(out_path.exists());
}

#[test]
fn phase5_end_to_end_differential_window_analyze_planner_c_compat() {
    let sandbox = TempSandbox::new("diff_e2e");
    let c_dir = sandbox.path().join("c_run");
    let rust_dir = sandbox.path().join("rust_run");
    fs::create_dir_all(&c_dir).unwrap();
    fs::create_dir_all(&rust_dir).unwrap();

    let workload_sql = "\
PRAGMA page_size = 1024;
PRAGMA auto_vacuum = INCREMENTAL;
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

ATTACH 'aux.db' AS aux;
PRAGMA aux.page_size = 1024;
PRAGMA aux.auto_vacuum = FULL;
PRAGMA aux.journal_mode = WAL;

CREATE TABLE main.sales(
    sale_id INTEGER PRIMARY KEY,
    region TEXT NOT NULL,
    rep TEXT NOT NULL,
    quarter INT NOT NULL,
    amount INT NOT NULL,
    bonus INT GENERATED ALWAYS AS (amount / 10) STORED
);
CREATE INDEX main.idx_sales_reg_q_amt ON sales(region, quarter, amount);
CREATE INDEX main.idx_sales_high ON sales(rep, amount) WHERE amount >= 200;
CREATE INDEX main.idx_sales_lower_rep ON sales(lower(rep));

CREATE TABLE main.tier_rules(
    tier_code TEXT PRIMARY KEY,
    min_amt INT NOT NULL,
    max_amt INT NOT NULL
) WITHOUT ROWID;
INSERT INTO main.tier_rules VALUES
    ('BRONZE', 0, 149),
    ('SILVER', 150, 299),
    ('GOLD', 300, 9999);

INSERT INTO main.sales(sale_id, region, rep, quarter, amount) VALUES
    (1, 'EU', 'Alice', 1, 120),
    (2, 'EU', 'Bob',   1, 240),
    (3, 'EU', 'Alice', 2, 300),
    (4, 'EU', 'Cara',  2, 300),
    (5, 'US', 'Dan',   1, 180),
    (6, 'US', 'Erin',  1, 420),
    (7, 'US', 'Dan',   2, 260),
    (8, 'US', 'Frank', 2, 90),
    (9, 'AP', 'Gina',  1, 350),
    (10, 'AP', 'Hiro', 2, 500);

CREATE TABLE main.rank_audit(
    audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
    tag TEXT NOT NULL,
    summary TEXT NOT NULL
);

CREATE VIEW main.v_region_window AS
SELECT
    sale_id,
    region,
    rep,
    quarter,
    amount,
    row_number() OVER w_reg AS rn,
    rank() OVER w_reg AS rnk,
    dense_rank() OVER w_reg AS drnk,
    sum(amount) OVER (w_reg ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS moving_sum,
    first_value(rep) OVER w_reg AS top_rep,
    lag(amount, 1, 0) OVER w_reg AS prev_amt,
    lead(amount, 1, -1) OVER w_reg AS next_amt
FROM main.sales
WINDOW w_reg AS (PARTITION BY region ORDER BY amount DESC, sale_id ASC);

CREATE TRIGGER main.tr_after_sale_win AFTER INSERT ON sales BEGIN
    INSERT INTO rank_audit(tag, summary)
    SELECT
        'INS:' || NEW.rep,
        group_concat(rep || '=' || rnk, ',')
    FROM (
        SELECT rep, rank() OVER (PARTITION BY region ORDER BY amount DESC, sale_id ASC) AS rnk
        FROM sales
        WHERE region = NEW.region
        ORDER BY amount DESC, sale_id ASC
    );
END;

INSERT INTO main.sales(sale_id, region, rep, quarter, amount)
VALUES (11, 'EU', 'Zoe', 3, 360);

UPDATE main.sales AS s
SET amount = s.amount + w.delta
FROM (
    SELECT
        sale_id,
        ntile(2) OVER (PARTITION BY region ORDER BY amount ASC, sale_id ASC) * 10 AS delta
    FROM main.sales
) AS w
WHERE s.sale_id = w.sale_id AND s.region = 'US';

ALTER TABLE main.sales RENAME TO revenue_entries;
ALTER TABLE main.revenue_entries RENAME COLUMN amount TO gross_amount;

ANALYZE;

UPDATE main.sqlite_stat1
SET stat = '5000 10 1 1'
WHERE tbl = 'revenue_entries' AND idx = 'idx_sales_reg_q_amt';
ANALYZE sqlite_schema;

CREATE TABLE aux.archive_summary AS
SELECT
    region,
    rep,
    gross_amount,
    rn,
    rnk,
    drnk,
    moving_sum,
    top_rep,
    prev_amt,
    next_amt
FROM main.v_region_window
WHERE region IN ('EU', 'US')
ORDER BY region, rn;

PRAGMA main.incremental_vacuum(2);
PRAGMA main.wal_checkpoint(TRUNCATE);
PRAGMA aux.wal_checkpoint(TRUNCATE);
VACUUM aux INTO 'aux_vacuumed.db';
";

    let query_check_sql = "\
ATTACH 'aux.db' AS aux;
ATTACH 'aux_vacuumed.db' AS av;
PRAGMA main.integrity_check;
PRAGMA aux.integrity_check;
PRAGMA av.integrity_check;
SELECT type, name, tbl_name, sql FROM main.sqlite_schema WHERE name NOT LIKE 'sqlite_autoindex%' ORDER BY type, name;
SELECT tbl, idx, stat FROM main.sqlite_stat1 ORDER BY tbl, idx;
SELECT tbl, idx, stat FROM aux.sqlite_stat1 ORDER BY tbl, idx;
SELECT sale_id, region, rep, quarter, gross_amount, bonus FROM main.revenue_entries ORDER BY sale_id;
SELECT sale_id, region, rep, gross_amount, rn, rnk, drnk, moving_sum, top_rep, prev_amt, next_amt FROM main.v_region_window ORDER BY region, rn;
SELECT audit_id, tag, summary FROM main.rank_audit ORDER BY audit_id;
SELECT region, rep, gross_amount, rn, rnk, drnk, moving_sum, top_rep, prev_amt, next_amt FROM aux.archive_summary ORDER BY region, rn;
SELECT region, rep, gross_amount, rn, rnk, drnk, moving_sum, top_rep, prev_amt, next_amt FROM av.archive_summary ORDER BY region, rn;
SELECT
    region,
    sum(gross_amount) AS reg_total,
    sum(sum(gross_amount)) OVER (ORDER BY sum(gross_amount) ASC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS running_reg_total,
    rank() OVER (ORDER BY sum(gross_amount) DESC) AS reg_rank
FROM main.revenue_entries
GROUP BY region
ORDER BY region;
SELECT
    sale_id,
    region,
    gross_amount,
    sum(gross_amount) FILTER (WHERE quarter >= 2) OVER (
        PARTITION BY region
        ORDER BY gross_amount ASC, sale_id ASC
        GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE CURRENT ROW
    ) AS grp_excl_sum,
    nth_value(rep, 2) OVER (
        PARTITION BY region
        ORDER BY gross_amount DESC, sale_id ASC
        ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
    ) AS second_rep
FROM main.revenue_entries
ORDER BY sale_id;
";

    // 1. Run workload on C SQLite 3.54.1
    let (ok_c, _, err_c) = run_c_sqlite_in_dir(&c_dir, "main.db", workload_sql);
    assert!(ok_c, "C SQLite workload failed: {err_c}");

    // 2. Run identical workload on Rust `sqliters`
    let rust_main = rust_dir.join("main.db");
    {
        let conn = Connection::open(&rust_main).expect("open rust main.db");
        conn.set_working_dir(Some(rust_dir.clone())).unwrap();
        conn.execute_batch(workload_sql)
            .expect("execute workload_sql on Rust sqliters");

        // Verify planner EQP chooses covering/partial/expression indexes after ANALYZE + RENAME
        let eqp_cov = conn
            .query(
                "EXPLAIN QUERY PLAN SELECT region, quarter, gross_amount FROM revenue_entries WHERE region = 'EU' AND quarter = 2;",
                &[],
            )
            .unwrap();
        let eqp_cov_text = format_rows_pipe(&eqp_cov.rows);
        assert!(
            eqp_cov_text.contains("USING COVERING INDEX idx_sales_reg_q_amt"),
            "expected covering index idx_sales_reg_q_amt in EQP, got: {eqp_cov_text}"
        );

        let eqp_part = conn
            .query(
                "EXPLAIN QUERY PLAN SELECT rep, gross_amount FROM revenue_entries WHERE rep = 'Alice' AND gross_amount >= 200;",
                &[],
            )
            .unwrap();
        let eqp_part_text = format_rows_pipe(&eqp_part.rows);
        assert!(
            eqp_part_text.contains("idx_sales_high"),
            "expected partial index idx_sales_high in EQP, got: {eqp_part_text}"
        );

        let eqp_expr = conn
            .query(
                "EXPLAIN QUERY PLAN SELECT sale_id FROM revenue_entries WHERE lower(rep) = 'alice';",
                &[],
            )
            .unwrap();
        let eqp_expr_text = format_rows_pipe(&eqp_expr.rows);
        assert!(
            eqp_expr_text.contains("idx_sales_lower_rep"),
            "expected expression index idx_sales_lower_rep in EQP, got: {eqp_expr_text}"
        );

        conn.try_close().unwrap();
    }

    // 3. C SQLite 3.54.1 inspects both C-produced and Rust-produced DBs -> identical schema, stat1, rows, & integrity!
    let (ok_cc, out_cc, err_cc) = run_c_sqlite_in_dir(&c_dir, "main.db", query_check_sql);
    assert!(ok_cc, "C inspect on C DB failed: {err_cc}");
    let (ok_cr, out_cr, err_cr) = run_c_sqlite_in_dir(&rust_dir, "main.db", query_check_sql);
    assert!(ok_cr, "C inspect on Rust DB failed: {err_cr}");
    assert_eq!(
        out_cr, out_cc,
        "Rust Phase 5 on-disk DB state diverged from C SQLite 3.54.1"
    );

    // 4. Rust `Connection` opens C-SQLite-produced DB files and runs the verification queries identically!
    {
        let c_main = c_dir.join("main.db");
        let conn_on_c = Connection::open(&c_main).expect("open C-created main.db in Rust");
        conn_on_c.set_working_dir(Some(c_dir.clone())).unwrap();
        conn_on_c
            .execute_batch("ATTACH 'aux.db' AS aux; ATTACH 'aux_vacuumed.db' AS av;")
            .unwrap();
        for pragma in [
            "PRAGMA main.integrity_check;",
            "PRAGMA aux.integrity_check;",
            "PRAGMA av.integrity_check;",
        ] {
            let ic = conn_on_c.query(pragma, &[]).unwrap();
            assert_eq!(format_rows_pipe(&ic.rows), "ok\n");
        }
        let r_view = conn_on_c
            .query(
                "SELECT sale_id, region, rep, gross_amount, rn, rnk, drnk, moving_sum, top_rep, prev_amt, next_amt FROM main.v_region_window ORDER BY region, rn;",
                &[],
            )
            .unwrap();
        let (_, c_view, _) = run_c_sqlite_in_dir(
            &c_dir,
            "main.db",
            "SELECT sale_id, region, rep, gross_amount, rn, rnk, drnk, moving_sum, top_rep, prev_amt, next_amt FROM main.v_region_window ORDER BY region, rn;",
        );
        assert_eq!(format_rows_pipe(&r_view.rows), c_view);
        conn_on_c.try_close().unwrap();
    }
}

#[test]
fn phase5_pure_sql_sections_of_ignored_window_suites() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read_lines = |rel: &str| -> Vec<String> {
        fs::read_to_string(workspace_root.join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"))
            .lines()
            .map(ToOwned::to_owned)
            .collect()
    };

    // 1. `window6.test`: all 64 pure-SQL subtests (`1.1.1..1.6.4`, `4.0..5.5`, `7.0..11.4.1`)
    {
        let lines = read_lines("sqlite/test/window6.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..84]);
        sliced.extend_from_slice(&lines[107..146]);
        sliced.extend_from_slice(&lines[162..]);
        assert_tcl_section_ok(
            "window6",
            "window6.test (all 64 pure-SQL subtests)",
            &sliced.join("\n"),
            64,
        );
    }

    // 2. `windowB.test`: all 36 pure-SQL subtests (`1.0..2.1.4`, `4.0..9.0`)
    {
        let lines = read_lines("sqlite/test/windowB.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..83]);
        sliced.extend_from_slice(&lines[328..490]);
        sliced.push("finish_test".to_owned());
        assert_tcl_section_ok(
            "windowB",
            "windowB.test (all 36 pure-SQL subtests)",
            &sliced.join("\n"),
            36,
        );
    }

    // 3. `windowE.test`: all 8 pure-SQL subtests (`3.0..5.2`)
    {
        let lines = read_lines("sqlite/test/windowE.test");
        let mut sliced = Vec::with_capacity(lines.len());
        sliced.extend_from_slice(&lines[..16]);
        sliced.extend_from_slice(&lines[75..]);
        assert_tcl_section_ok(
            "windowE",
            "windowE.test (all 8 pure-SQL subtests)",
            &sliced.join("\n"),
            8,
        );
    }
}
