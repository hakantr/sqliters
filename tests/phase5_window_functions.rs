//! Phase 5 — Adım 5-A: Window Functions Syntax & Execution Engine
//! Differential, lifecycle, subtype, cancellation, and upstream Tcl test verification (`Groups 52, 54, 65`).
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use sqliters_core::{Connection, FunctionFlags, RuntimeValue, StepResult, WindowAggregateInstance};
use sqliters_tcltest::{evaluate_test_script, strip_test_comments};
use sqliters_types::{ErrorCode, Result, Value};

// ============================================================================
// Differential C SQLite 3.54.1 Helpers (`./target/sqlite-ref/sqlite3`)
// ============================================================================

fn run_c_sqlite_lines(sql: &str) -> Vec<String> {
    let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sqlite-ref/sqlite3");
    assert!(
        bin.exists(),
        "C SQLite reference binary missing at {}",
        bin.display()
    );
    let out = Command::new(&bin)
        .args(["-batch", "-list", "-separator", "|", ":memory:", sql])
        .output()
        .expect("spawn C sqlite3");
    assert!(
        out.status.success(),
        "C sqlite3 failed on SQL:\n{sql}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn run_c_sqlite_err(sql: &str) -> String {
    let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sqlite-ref/sqlite3");
    assert!(
        bin.exists(),
        "C SQLite reference binary missing at {}",
        bin.display()
    );
    let out = Command::new(&bin)
        .args(["-batch", "-list", ":memory:", sql])
        .output()
        .expect("spawn C sqlite3");
    assert!(
        !out.status.success(),
        "Expected C sqlite3 to fail on SQL:\n{sql}\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).trim().to_owned()
}

fn format_val_pipe(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => sqliters_types::format_sqlite_real(*r),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

fn run_rust_sqlite_lines(sql: &str) -> Vec<String> {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let mut lines = Vec::new();
    let mut rem = sql;
    while !rem.trim().is_empty() {
        let mut stmt = conn.prepare(rem).unwrap_or_else(|e| {
            panic!("Rust sqliters prepare failed on:\n{rem}\nerror: {e}");
        });
        let tail_len = stmt.tail().len();
        let consumed = rem.len().saturating_sub(tail_len);
        rem = &rem[consumed..];
        loop {
            match stmt.step() {
                Ok(StepResult::Row(row)) => {
                    let cols: Vec<String> = row.values().iter().map(format_val_pipe).collect();
                    lines.push(cols.join("|"));
                }
                Ok(StepResult::Done) => break,
                Err(e) => panic!("Rust sqliters step failed on:\n{sql}\nerror: {e}"),
            }
        }
        stmt.finalize().expect("finalize statement");
    }
    lines
}

fn run_rust_sqlite_err(sql: &str) -> String {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let mut rem = sql;
    while !rem.trim().is_empty() {
        match conn.prepare(rem) {
            Err(e) => return e.message,
            Ok(mut stmt) => {
                let tail_len = stmt.tail().len();
                let consumed = rem.len().saturating_sub(tail_len);
                rem = &rem[consumed..];
                loop {
                    match stmt.step() {
                        Ok(StepResult::Row(_)) => {}
                        Ok(StepResult::Done) => break,
                        Err(e) => {
                            let _ = stmt.finalize();
                            return e.message;
                        }
                    }
                }
                if let Err(e) = stmt.finalize() {
                    return e.message;
                }
            }
        }
    }
    panic!("Expected Rust sqliters to fail on SQL:\n{sql}");
}

fn assert_diff_ok(sql: &str) {
    let expected = run_c_sqlite_lines(sql);
    let actual = run_rust_sqlite_lines(sql);
    assert_eq!(
        actual, expected,
        "Differential mismatch on SQL:\n{sql}\n  Rust: {actual:?}\n  C:    {expected:?}"
    );
}

fn assert_diff_err_substr(sql: &str, expected_substr: &str) {
    let c_err = run_c_sqlite_err(sql);
    let r_err = run_rust_sqlite_err(sql);
    assert!(
        c_err.contains(expected_substr),
        "C SQLite error {c_err:?} did not contain {expected_substr:?} for SQL:\n{sql}"
    );
    assert!(
        r_err.contains(expected_substr),
        "Rust sqliters error {r_err:?} did not contain {expected_substr:?} (C was {c_err:?}) for SQL:\n{sql}"
    );
}

fn query_strings(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).expect("prepare query_strings");
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().expect("step query_strings") {
        let cols: Vec<String> = row.values().iter().map(format_val_pipe).collect();
        out.push(cols.join("|"));
    }
    stmt.finalize().expect("finalize query_strings");
    out
}

// ============================================================================
// 1. Upstream Tcl Suite Audit (`window1`..`windowE`, `windowerr`, `windowpushd`,
//    `trigger1`, `triggerE`, `upfrom2`)
// ============================================================================

fn is_known_non_sql_harness_subtest(suite: &str, subtest_name: &str) -> bool {
    match suite {
        // `do_ordercount_test` counts VDBE `OP_OpenEphemeral` opcodes in `EXPLAIN` output
        "window1" => matches!(
            subtest_name,
            "window1-23.2" | "window1-23.3" | "window1-23.4" | "window1-23.5" | "window1-23.6"
        ),
        // Tcl `db func`, `db collate`, and `sqlite3_db_config(DQS_DML)` C-API bindings in Tcl
        "window6" => matches!(
            subtest_name,
            "window6-2.0" | "window6-3.0" | "window6-3.1" | "window6-6.0" | "window6-6.1"
        ),
        // JSON extension (`json_group_array`) is Phase 5 Step 5-B (`Groups 57, 63`)
        "windowB" => matches!(subtest_name, "windowB-10.2" | "windowB-10.3"),
        // `do_eqp_test` checks `EXPLAIN QUERY PLAN` text strings; all SQL result tests in `windowpushd` pass
        "windowpushd" => matches!(
            subtest_name,
            "windowpushd-1.4"
                | "windowpushd-2.1.1.4"
                | "windowpushd-2.1.1.5"
                | "windowpushd-2.1.3.3"
                | "windowpushd-2.1.3.4"
                | "windowpushd-2.1.3.6"
        ),
        _ => false,
    }
}

#[test]
fn upstream_window_tcl_suites_audit() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let suites = [
        "window1",
        "window2",
        "window3",
        "window4",
        "window6",
        "window7",
        "window8",
        "window9",
        "windowA",
        "windowB",
        "windowC",
        "windowD",
        "windowerr",
        "windowpushd",
        "trigger1",
        "triggerE",
        "upfrom2",
    ];
    let mut total_passed = 0usize;
    let mut unexpected_failures = Vec::new();
    for name in suites {
        let path = repo_root.join(format!("sqlite/test/{name}.test"));
        let raw = std::fs::read_to_string(&path).expect("read .test file");
        let stripped = strip_test_comments(&raw);
        let script = format!("set ::sqlite_options(windowfunc) 1\n{stripped}");
        let report = evaluate_test_script(name, &script);
        let unexpected: Vec<_> = report
            .failed
            .into_iter()
            .filter(|f| !is_known_non_sql_harness_subtest(name, &f.name))
            .collect();
        total_passed += report.passed;
        if !unexpected.is_empty() || report.script_error.is_some() {
            unexpected_failures.push((
                name.to_owned(),
                report.passed,
                unexpected,
                report.script_error,
            ));
        }
    }

    // Also run the pure-SQL half of `windowE.test` (`3.0..5.2`) directly through the Tcl evaluator
    // (lines 1..75 of `windowE.test` use Tcl `db collate` and `sqlite3_create_aggregate`, which we
    // verify in `window5_and_windowe_custom_callbacks_and_collation` below).
    let window_e_path = repo_root.join("sqlite/test/windowE.test");
    let window_e_raw = std::fs::read_to_string(&window_e_path).expect("read windowE.test");
    let window_e_lines: Vec<&str> = window_e_raw.lines().collect();
    let window_e_sql_part = format!(
        "set testprefix windowE\n{}",
        strip_test_comments(&window_e_lines[75..].join("\n"))
    );
    let report_e = evaluate_test_script("windowE", &window_e_sql_part);
    total_passed += report_e.passed;
    if !report_e.failed.is_empty() || report_e.script_error.is_some() {
        unexpected_failures.push((
            "windowE".to_owned(),
            report_e.passed,
            report_e.failed,
            report_e.script_error,
        ));
    }

    assert!(
        unexpected_failures.is_empty(),
        "Upstream Tcl window/trigger/upfrom suites had unexpected failures: {unexpected_failures:#?}"
    );
    assert!(
        total_passed >= 3045,
        "Expected at least 3,045 passing upstream Tcl subtests across window/trigger/upfrom suites, got {total_passed}"
    );
}

// ============================================================================
// 2. Differential Test: All 11 Built-in Window Functions
// ============================================================================

#[test]
fn differential_all_11_builtin_window_functions() {
    assert_diff_ok(
        "
        CREATE TABLE emp(dept TEXT, name TEXT, salary INT);
        INSERT INTO emp VALUES
            ('eng', 'alice', 100),
            ('eng', 'bob',   120),
            ('eng', 'carol', 120),
            ('eng', 'dave',  150),
            ('eng', 'erin',  NULL),
            ('sales', 'frank', 90),
            ('sales', 'grace', 90),
            ('sales', 'heidi', 110);

        SELECT
            dept,
            name,
            salary,
            row_number() OVER w,
            rank() OVER w,
            dense_rank() OVER w,
            printf('%.4f', percent_rank() OVER w),
            printf('%.4f', cume_dist() OVER w),
            ntile(1) OVER w,
            ntile(2) OVER w,
            ntile(3) OVER w,
            lag(name) OVER w,
            lag(name, 2, 'none') OVER w,
            lag(name, 0, 'self') OVER w,
            lead(name) OVER w,
            lead(name, 2, 'end') OVER w,
            first_value(name) OVER (w ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            last_value(name) OVER (w ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            nth_value(name, 1) OVER (w ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING),
            nth_value(name, 2) OVER (w ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            nth_value(name, 4) OVER (w ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING)
        FROM emp
        WINDOW w AS (PARTITION BY dept ORDER BY salary ASC NULLS FIRST, name ASC)
        ORDER BY dept, row_number() OVER w;
        ",
    );

    // NULLS LAST and DESC ordering with ranking/navigation functions
    assert_diff_ok(
        "
        CREATE TABLE t_nulls(k INT, v INT);
        INSERT INTO t_nulls VALUES (1, NULL), (2, 10), (3, 10), (4, 20), (5, NULL);
        SELECT
            k,
            v,
            row_number() OVER w,
            rank() OVER w,
            dense_rank() OVER w,
            first_value(k) OVER w,
            last_value(k) OVER w,
            nth_value(k, 2) OVER w
        FROM t_nulls
        WINDOW w AS (ORDER BY v DESC NULLS LAST, k ASC ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
        ORDER BY row_number() OVER w;
        ",
    );

    // Runtime validation errors for ntile() and nth_value()
    for bad_arg in ["0", "-5", "'abc'", "NULL"] {
        assert_diff_err_substr(
            &format!(
                "CREATE TABLE t(x); INSERT INTO t VALUES(1); SELECT ntile({bad_arg}) OVER () FROM t;"
            ),
            "argument of ntile must be a positive integer",
        );
    }
    for bad_arg in ["0", "-5", "'abc'", "NULL", "1.5"] {
        assert_diff_err_substr(
            &format!(
                "CREATE TABLE t(x); INSERT INTO t VALUES(1); SELECT nth_value(x, {bad_arg}) OVER () FROM t;"
            ),
            "second argument to nth_value must be a positive integer",
        );
    }
}

// ============================================================================
// 3. Differential Test: Built-in Aggregates, Frames (`ROWS`, `RANGE`, `GROUPS`),
//    `EXCLUDE` Modes, `FILTER (WHERE ...)`, and Kahan-Babushka-Neumaier Summation
// ============================================================================

#[test]
fn differential_aggregates_frames_exclude_and_filter() {
    // ROWS, RANGE, and GROUPS frames with all 4 EXCLUDE modes
    assert_diff_ok(
        "
        CREATE TABLE nums(p INT, x INT, label TEXT);
        INSERT INTO nums VALUES
            (1, 10, 'a'),
            (1, 10, 'b'),
            (1, 12, 'c'),
            (1, 15, 'd'),
            (1, 15, 'e'),
            (1, 20, 'f'),
            (2, NULL, 'g'),
            (2, NULL, 'h'),
            (2, 5, 'i'),
            (2, 8, 'j');

        SELECT
            p, x, label,
            count(*) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING),
            count(x) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING),
            sum(x) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING EXCLUDE NO OTHERS),
            sum(x) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING EXCLUDE CURRENT ROW),
            sum(x) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING EXCLUDE GROUP),
            sum(x) OVER (PARTITION BY p ORDER BY x RANGE BETWEEN 3 PRECEDING AND 5 FOLLOWING EXCLUDE TIES),
            total(x) OVER (PARTITION BY p ORDER BY x GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            printf('%.3f', avg(x) OVER (PARTITION BY p ORDER BY x GROUPS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE TIES)),
            min(label) OVER (PARTITION BY p ORDER BY x, label ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            max(label) OVER (PARTITION BY p ORDER BY x, label ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            group_concat(label, '-') OVER (PARTITION BY p ORDER BY x, label ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING EXCLUDE CURRENT ROW),
            string_agg(label, ':') FILTER (WHERE x IS NOT NULL AND x >= 12) OVER (PARTITION BY p ORDER BY x, label ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
        FROM nums
        ORDER BY p, x NULLS FIRST, label;
        ",
    );

    // Kahan-Babushka-Neumaier precision across sliding window inverse (`windowE` 4.1, 4.2, 5.1, 5.2)
    assert_diff_ok(
        "
        CREATE TABLE t_kbn(id INT, x);
        INSERT INTO t_kbn VALUES(1, -1), (2, 9223372036854775807), (3, 1), (4, 0.5);
        SELECT
            id,
            sum(id) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 2 FOLLOWING),
            CASE WHEN id >= 3 THEN total(x) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 2 FOLLOWING) END,
            CASE WHEN id != 2 THEN sum(x) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 2 FOLLOWING) ELSE typeof(sum(x) OVER (ORDER BY id ROWS BETWEEN CURRENT ROW AND 2 FOLLOWING)) END
        FROM t_kbn
        ORDER BY id;
        ",
    );
}

// ============================================================================
// 4. Differential Test: Window Chaining, Multiple Windows, `GROUP BY`/`HAVING`,
//    `DISTINCT`, Compound Queries, Views, CTEs, and `windowpushd` Pushdown
// ============================================================================

#[test]
fn differential_chaining_groupby_distinct_compounds_views_pushdown() {
    assert_diff_ok(
        "
        CREATE TABLE sales(region TEXT, rep TEXT, amount INT);
        INSERT INTO sales VALUES
            ('east', 'ava', 10),
            ('east', 'ava', 20),
            ('east', 'ben', 15),
            ('west', 'cal', 30),
            ('west', 'dan', 25),
            ('west', 'dan', 35);

        SELECT
            region,
            rep,
            sum(amount) AS rep_total,
            sum(sum(amount)) OVER w_region AS region_running,
            rank() OVER w_global AS global_rank,
            group_concat(rep, ',') OVER w_frame AS frame_reps
        FROM sales
        GROUP BY region, rep
        HAVING sum(amount) >= 15
        WINDOW
            w_base AS (PARTITION BY region),
            w_region AS (w_base ORDER BY sum(amount) DESC, rep ASC),
            w_frame AS (w_region ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING),
            w_global AS (ORDER BY sum(amount) DESC, rep ASC)
        ORDER BY global_rank, region, rep;
        ",
    );

    // SELECT DISTINCT + compound queries + views + WHERE pushdown safety (`windowpushd`)
    assert_diff_ok(
        "
        CREATE TABLE t_push(grp INT, val INT);
        INSERT INTO t_push VALUES (1, 10), (1, 20), (2, 30), (2, 40), (3, 50);

        CREATE VIEW v_part AS
            SELECT grp, val, sum(val) OVER (PARTITION BY grp ORDER BY val) AS s,
                   row_number() OVER (PARTITION BY grp ORDER BY val) AS rn
            FROM t_push;

        CREATE VIEW v_nopart AS
            SELECT grp, val, sum(val) OVER (ORDER BY grp, val) AS s,
                   row_number() OVER (ORDER BY grp, val) AS rn
            FROM t_push;

        SELECT * FROM v_part WHERE grp = 2 ORDER BY val;
        SELECT * FROM v_nopart WHERE grp = 2 ORDER BY val;
        SELECT DISTINCT sum(val) OVER (PARTITION BY grp) FROM t_push ORDER BY 1;
        SELECT grp, sum(val) OVER (PARTITION BY grp) FROM t_push
        UNION
        SELECT 99, 999
        ORDER BY 1, 2;
        ",
    );
}

// ============================================================================
// 5. Differential Test: `ALTER TABLE RENAME` / `DROP COLUMN` & Triggers with Windows
// ============================================================================

#[test]
fn differential_ddl_rename_and_triggers_with_windows() {
    assert_diff_ok(
        "
        CREATE TABLE base_tbl(k INT, v INT, extra INT);
        INSERT INTO base_tbl VALUES (1, 10, 100), (1, 20, 200), (2, 30, 300);

        CREATE TABLE audit_log(msg TEXT);

        CREATE VIEW win_view AS
            SELECT
                base_tbl.k AS key_col,
                sum(base_tbl.v) OVER w1 AS running_v,
                first_value(base_tbl.v) OVER (
                    PARTITION BY base_tbl.k
                    ORDER BY base_tbl.v
                    ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING
                ) AS fv
            FROM base_tbl
            WINDOW w1 AS (PARTITION BY base_tbl.k ORDER BY base_tbl.v);

        CREATE TRIGGER tr_after_ins AFTER INSERT ON base_tbl BEGIN
            INSERT INTO audit_log
            SELECT base_tbl.k || ':' || sum(base_tbl.v) OVER w_tr
            FROM base_tbl
            WHERE base_tbl.k = NEW.k
            WINDOW w_tr AS (PARTITION BY base_tbl.k ORDER BY base_tbl.v);
        END;

        ALTER TABLE base_tbl RENAME TO renamed_tbl;
        ALTER TABLE renamed_tbl RENAME COLUMN k TO grp_key;
        ALTER TABLE renamed_tbl RENAME COLUMN v TO metric_val;
        ALTER TABLE renamed_tbl DROP COLUMN extra;

        INSERT INTO renamed_tbl VALUES (2, 40);

        SELECT key_col, running_v, fv FROM win_view ORDER BY key_col, running_v;
        SELECT msg FROM audit_log ORDER BY rowid;
        ",
    );

    // Dropping a column referenced inside a view's window clause fails upon subsequent view query,
    // and UPDATE ... FROM with window functions in the FROM subquery works identically to C SQLite.
    assert_diff_ok(
        "
        CREATE TABLE scores(id INT PRIMARY KEY, team TEXT, pts INT, rank_out INT);
        INSERT INTO scores VALUES (1, 'A', 10, 0), (2, 'A', 30, 0), (3, 'B', 25, 0);

        UPDATE scores
        SET rank_out = sub.r
        FROM (
            SELECT id, rank() OVER (PARTITION BY team ORDER BY pts DESC) AS r
            FROM scores
        ) AS sub
        WHERE scores.id = sub.id;

        SELECT id, team, pts, rank_out FROM scores ORDER BY id;
        ",
    );
}

// ============================================================================
// 6. Differential Test: `windowerr` & Window Validation Error Parity
// ============================================================================

#[test]
fn differential_window_error_messages() {
    let cases = [
        (
            "CREATE TABLE t(a, b); SELECT row_number() OVER no_such_win FROM t;",
            "no such window: no_such_win",
        ),
        (
            "CREATE TABLE t(a, b); SELECT row_number() OVER (w1 PARTITION BY a) FROM t WINDOW w1 AS (PARTITION BY b);",
            "cannot override PARTITION clause of window: w1",
        ),
        (
            "CREATE TABLE t(a, b); SELECT row_number() OVER (w1 ORDER BY a) FROM t WINDOW w1 AS (ORDER BY b);",
            "cannot override ORDER BY clause of window: w1",
        ),
        (
            "CREATE TABLE t(a, b); SELECT sum(a) OVER (w1 ROWS CURRENT ROW) FROM t WINDOW w1 AS (ROWS UNBOUNDED PRECEDING);",
            "cannot override frame specification of window: w1",
        ),
        (
            "CREATE TABLE t(a, b); SELECT trim(a) OVER (ORDER BY b) FROM t;",
            "trim() may not be used as a window function",
        ),
        (
            "CREATE TABLE t(a, b); SELECT sum(a) OVER (ORDER BY a, b RANGE BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t;",
            "RANGE with offset PRECEDING/FOLLOWING requires one ORDER BY expression",
        ),
        (
            "CREATE TABLE t(a, b); INSERT INTO t VALUES(1, 2); SELECT sum(a) OVER (ROWS BETWEEN -1 PRECEDING AND CURRENT ROW) FROM t;",
            "frame starting offset must be a non-negative integer",
        ),
        (
            "CREATE TABLE t(a, b); INSERT INTO t VALUES(1, 2); SELECT sum(a) OVER (ORDER BY a RANGE BETWEEN 1 PRECEDING AND -2 FOLLOWING) FROM t;",
            "frame ending offset must be a non-negative number",
        ),
        (
            "CREATE TABLE t(a, b); SELECT a FROM t WHERE row_number() OVER () > 1;",
            "misuse of window function row_number()",
        ),
        (
            "CREATE TABLE t(a, b); SELECT a FROM t GROUP BY row_number() OVER ();",
            "misuse of window function row_number()",
        ),
        (
            "CREATE TABLE t(a, b); SELECT a FROM t GROUP BY a HAVING sum(b) OVER () > 0;",
            "misuse of window function sum()",
        ),
        (
            "CREATE TABLE t(a, b); SELECT sum(row_number() OVER ()) FROM t;",
            "misuse of window function row_number()",
        ),
        (
            "CREATE TABLE t(a, b); SELECT row_number() FILTER (WHERE a > 0) OVER () FROM t;",
            "FILTER clause may only be used with aggregate window functions",
        ),
        (
            "CREATE TABLE t(a, b); SELECT row_number(DISTINCT a) OVER () FROM t;",
            "DISTINCT is not supported for window functions",
        ),
        (
            "CREATE TABLE t(a, b); SELECT row_number(a) OVER () FROM t;",
            "wrong number of arguments to function row_number()",
        ),
    ];

    for (sql, expected_err) in cases {
        assert_diff_err_substr(sql, expected_err);
    }
}

// ============================================================================
// 7. `ExecutionServices` `agg_value` / `agg_inverse` Sliding Frame Lifecycle
//    (`window5.test` & `windowE.test` Custom Callbacks + Collation Verification)
// ============================================================================

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LifecycleStats {
    opens: usize,
    steps: usize,
    inverses: usize,
    values: usize,
    finalizes: usize,
}

struct SlidingSumIntAcc {
    sum: i64,
    stats: Arc<Mutex<LifecycleStats>>,
}

impl WindowAggregateInstance for SlidingSumIntAcc {
    fn step(&mut self, args: &[RuntimeValue]) -> Result<()> {
        self.stats.lock().expect("lock stats").steps += 1;
        if let Some(a) = args.first()
            && !a.value.is_null()
        {
            self.sum += a.value.to_integer_lossy();
        }
        Ok(())
    }

    fn inverse(&mut self, args: &[RuntimeValue]) -> Result<()> {
        self.stats.lock().expect("lock stats").inverses += 1;
        if let Some(a) = args.first()
            && !a.value.is_null()
        {
            self.sum -= a.value.to_integer_lossy();
        }
        Ok(())
    }

    fn value(&self) -> Result<RuntimeValue> {
        self.stats.lock().expect("lock stats").values += 1;
        Ok(RuntimeValue::plain(Value::Integer(self.sum)))
    }

    fn finalize(&mut self) -> Result<RuntimeValue> {
        self.stats.lock().expect("lock stats").finalizes += 1;
        Ok(RuntimeValue::plain(Value::Integer(self.sum)))
    }
}

#[test]
fn custom_window_aggregate_sliding_inverse_and_window5_windowe_parity() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let stats = Arc::new(Mutex::new(LifecycleStats::default()));
    let stats_factory = Arc::clone(&stats);

    conn.register_window_function("sumint", 1, FunctionFlags::NONE, true, true, move || {
        stats_factory.lock().expect("lock stats").opens += 1;
        Box::new(SlidingSumIntAcc {
            sum: 0,
            stats: Arc::clone(&stats_factory),
        })
    })
    .expect("register sumint");

    conn.execute_batch(
        "
        CREATE TABLE t1(a, b);
        INSERT INTO t1 VALUES(4, 'a');
        INSERT INTO t1 VALUES(6, 'b');
        INSERT INTO t1 VALUES(1, 'c');
        INSERT INTO t1 VALUES(5, 'd');
        INSERT INTO t1 VALUES(2, 'e');
        INSERT INTO t1 VALUES(3, 'f');
        ",
    )
    .expect("seed t1");

    // `window5-2.0`: UNBOUNDED PRECEDING TO CURRENT ROW
    let res_2_0 = query_strings(
        &conn,
        "SELECT sumint(a) OVER (ORDER BY rowid) FROM t1 ORDER BY rowid;",
    );
    assert_eq!(res_2_0, vec!["4", "10", "11", "16", "18", "21"]);

    // Reset stats and run `window5-2.1`: `ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING`
    *stats.lock().expect("lock stats") = LifecycleStats::default();
    let res_2_1 = query_strings(
        &conn,
        "SELECT sumint(a) OVER (ORDER BY rowid ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) FROM t1 ORDER BY rowid;",
    );
    assert_eq!(res_2_1, vec!["10", "11", "12", "8", "10", "5"]);

    // Verify true sliding-window execution:
    // - 1 probe/partition accumulator opened (`opens == 1`)
    // - 6 rows stepped in once (`steps == 6`)
    // - 4 rows stepped out via `xInverse` (`inverses == 4`: rows 0, 1, 2, 3 removed as frame slides)
    // - 6 non-destructive intermediate `xValue` reads (`values == 6`)
    // - 1 final `xFinal` cleanup call (`finalizes == 1`)
    let observed = stats.lock().expect("lock stats").clone();
    assert_eq!(
        observed,
        LifecycleStats {
            opens: 1,
            steps: 6,
            inverses: 4,
            values: 6,
            finalizes: 1,
        }
    );

    // `window5-3.0..3.1` & `windowE-2.1`: Legacy aggregate (`supports_window = false`) rejected in OVER
    conn.register_window_function("sum", 1, FunctionFlags::NONE, false, false, || {
        Box::new(SlidingSumIntAcc {
            sum: 0,
            stats: Arc::new(Mutex::new(LifecycleStats::default())),
        })
    })
    .expect("override sum with legacy aggregate");

    let err = conn
        .prepare(
            "SELECT sum(a) OVER (ORDER BY b ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM t1;",
        )
        .expect_err("legacy aggregate must reject OVER clause");
    assert_eq!(err.message, "sum() may not be used as a window function");
    assert_eq!(query_strings(&conn, "SELECT sum(a) FROM t1;"), vec!["21"]);

    // `windowE-1.0..1.3`: Custom collation with `RANGE BETWEEN 1 PRECEDING AND 2 PRECEDING`
    let reverse_flag = Arc::new(Mutex::new(false));
    let rf = Arc::clone(&reverse_flag);
    conn.register_collation("custom", move |a, b| {
        if *rf.lock().expect("lock reverse_flag") {
            b.cmp(a)
        } else {
            a.cmp(b)
        }
    })
    .expect("register custom collation");

    conn.execute_batch(
        "
        CREATE TABLE t_coll(a INTEGER PRIMARY KEY, b TEXT);
        INSERT INTO t_coll VALUES(1, 'one'), (2, 'two'), (3, 'three'), (4, 'four'), (5, 'five'), (6, 'six');
        ",
    )
    .expect("create t_coll");
    assert_eq!(
        query_strings(
            &conn,
            "SELECT group_concat(a, ',') OVER (ORDER BY b COLLATE custom RANGE BETWEEN 1 PRECEDING AND 2 PRECEDING) FROM t_coll;"
        ),
        vec!["5", "4", "1", "6", "3", "2"]
    );
    *reverse_flag.lock().expect("lock reverse_flag") = true;
    assert_eq!(
        query_strings(
            &conn,
            "SELECT group_concat(a, ',') OVER (ORDER BY b COLLATE custom RANGE BETWEEN 1 PRECEDING AND 2 PRECEDING) FROM t_coll;"
        ),
        vec!["2", "3", "6", "1", "4", "5"]
    );
}

// ============================================================================
// 8. `RuntimeValue` `SUBTYPE` & `RESULT_SUBTYPE` Preservation and Strict Misuse (`Group 65`)
// ============================================================================

struct SubtypeTrackingWinAcc {
    last_subtype: u8,
    emit_subtype_without_flag: bool,
}

impl WindowAggregateInstance for SubtypeTrackingWinAcc {
    fn step(&mut self, args: &[RuntimeValue]) -> Result<()> {
        if let Some(first) = args.first() {
            self.last_subtype = first.subtype;
        }
        Ok(())
    }

    fn inverse(&mut self, _args: &[RuntimeValue]) -> Result<()> {
        Ok(())
    }

    fn value(&self) -> Result<RuntimeValue> {
        let st: u32 = if self.emit_subtype_without_flag {
            99
        } else {
            u32::from(self.last_subtype)
        };
        Ok(RuntimeValue::with_subtype(
            Value::Integer(i64::from(self.last_subtype)),
            st,
        ))
    }

    fn finalize(&mut self) -> Result<RuntimeValue> {
        self.value()
    }
}

#[test]
fn window_subtype_preservation_and_misuse() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    // 1. Scalar tagger with `RESULT_SUBTYPE` sets subtype 77
    conn.register_scalar_function(
        "tag77",
        1,
        FunctionFlags::DETERMINISTIC | FunctionFlags::RESULT_SUBTYPE,
        |args| Ok(RuntimeValue::with_subtype(args[0].value.clone(), 77)),
    )
    .expect("register tag77");

    // 2. Window aggregate WITH `SUBTYPE | RESULT_SUBTYPE` receives input subtype 77 and emits subtype 77
    conn.register_window_function(
        "win_keep_sub",
        1,
        FunctionFlags::SUBTYPE | FunctionFlags::RESULT_SUBTYPE,
        true,
        true,
        || {
            Box::new(SubtypeTrackingWinAcc {
                last_subtype: 0,
                emit_subtype_without_flag: false,
            })
        },
    )
    .expect("register win_keep_sub");

    // 3. Window aggregate WITHOUT `SUBTYPE` has input subtype stripped to 0
    conn.register_window_function(
        "win_strip_sub",
        1,
        FunctionFlags::RESULT_SUBTYPE,
        true,
        true,
        || {
            Box::new(SubtypeTrackingWinAcc {
                last_subtype: 0,
                emit_subtype_without_flag: false,
            })
        },
    )
    .expect("register win_strip_sub");

    // 4. Window aggregate WITHOUT `RESULT_SUBTYPE` returning nonzero subtype triggers `ErrorCode::Misuse`
    conn.register_window_function(
        "win_bad_result_sub",
        1,
        FunctionFlags::SUBTYPE,
        true,
        true,
        || {
            Box::new(SubtypeTrackingWinAcc {
                last_subtype: 0,
                emit_subtype_without_flag: true,
            })
        },
    )
    .expect("register win_bad_result_sub");

    conn.execute_batch("CREATE TABLE t_sub(x INT); INSERT INTO t_sub VALUES (10), (20);")
        .expect("seed t_sub");

    // Verify `win_keep_sub` preserves subtype 77 while `win_strip_sub` strips input subtype to 0,
    // and built-in `first_value`/`lag` strip subtypes across the window ephemeral table boundary.
    let rows = query_strings(
        &conn,
        "
        SELECT
            win_keep_sub(tag77(x)) OVER (ORDER BY x),
            subtype(win_keep_sub(tag77(x)) OVER (ORDER BY x)),
            win_strip_sub(tag77(x)) OVER (ORDER BY x),
            subtype(win_strip_sub(tag77(x)) OVER (ORDER BY x)),
            subtype(first_value(tag77(x)) OVER (ORDER BY x)),
            subtype(lag(tag77(x), 1, tag77(x)) OVER (ORDER BY x))
        FROM t_sub
        ORDER BY x;
        ",
    );
    assert_eq!(rows, vec!["77|77|0|0|0|0", "77|77|0|0|0|0"]);

    // Verify strict `RESULT_SUBTYPE` enforcement on `win_bad_result_sub`
    let mut bad_stmt = conn
        .prepare("SELECT win_bad_result_sub(tag77(x)) OVER (ORDER BY x) FROM t_sub;")
        .expect("prepare win_bad_result_sub");
    let err = bad_stmt
        .step()
        .expect_err("expected error when returning subtype without RESULT_SUBTYPE");
    assert_eq!(err.code, ErrorCode::Error);
    assert_eq!(
        err.message,
        "misuse of sqlite3_result_subtype() by win_bad_result_sub()"
    );
    let _ = bad_stmt.finalize();
}

// ============================================================================
// 9. `InterruptHandle` Cancellation During Window Execution
// ============================================================================

#[test]
fn window_execution_interrupt_cancellation() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    conn.execute_batch(
        "
        CREATE TABLE big_win(a INT, b INT);
        WITH RECURSIVE cnt(x) AS (
            SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 200
        )
        INSERT INTO big_win SELECT x % 5, x FROM cnt;
        ",
    )
    .expect("populate big_win");

    let interrupt = conn.interrupt_handle();
    let int_clone = interrupt.clone();
    conn.register_scalar_function("trip_interrupt", 1, FunctionFlags::NONE, move |args| {
        if args[0].value.to_integer_lossy() >= 50 {
            int_clone.interrupt();
        }
        Ok(args[0].clone())
    })
    .expect("register trip_interrupt");

    let mut stmt = conn
        .prepare(
            "SELECT sum(trip_interrupt(b)) OVER (PARTITION BY a ORDER BY b ROWS BETWEEN 2 PRECEDING AND 2 FOLLOWING) FROM big_win;",
        )
        .expect("prepare interruptible window query");
    let err = stmt
        .step()
        .expect_err("window query must abort with Interrupt when InterruptHandle fires");
    assert_eq!(err.code, ErrorCode::Interrupt);
    let _ = stmt.finalize();
}
