//! Phase 5 — Step 5-B integration & differential test suite:
//! `ANALYZE` / `sqlite_stat1` full lifecycle, cost-based query planner, `WITHOUT ROWID`,
//! `IndexRangeScan`, `IN (...)`, covering/partial/expression indexes, skip-scan, and
//! `EXPLAIN QUERY PLAN`.
#![forbid(unsafe_code)]

use std::fs;
use std::path::Path;
use std::process::Command;

use sqliters_core::{Connection, StepResult};
use sqliters_tcltest::evaluate_test_script;
use sqliters_types::{ErrorCode, Value};

fn eval_rows(conn: &mut Connection, sql: &str) -> Vec<Vec<String>> {
    let q = conn.query(sql, &[]).expect(sql);
    q.rows
        .into_iter()
        .map(|r| {
            r.values()
                .iter()
                .map(|v| match v {
                    Value::Null => String::new(),
                    Value::Integer(i) => i.to_string(),
                    Value::Real(f) => format!("{f}"),
                    Value::Text(s) => s.clone(),
                    Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                })
                .collect()
        })
        .collect()
}

fn eval_flat(conn: &mut Connection, sql: &str) -> Vec<String> {
    eval_rows(conn, sql).into_iter().flatten().collect()
}

fn eqp_details(conn: &mut Connection, sql: &str) -> Vec<String> {
    let rows = eval_rows(conn, &format!("EXPLAIN QUERY PLAN {sql}"));
    rows.into_iter()
        .map(|mut r| r.pop().unwrap_or_default())
        .collect()
}

fn run_ref_sqlite(sql: &str) -> Option<String> {
    let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sqlite-ref/sqlite3");
    if !bin.exists() {
        return None;
    }
    let out = Command::new(bin).arg(":memory:").arg(sql).output().ok()?;
    if !out.status.success() {
        return Some(format!(
            "ERR:{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[test]
fn analyze_stat1_permissions_and_lifecycle() {
    let mut conn = Connection::open_in_memory().unwrap();

    // Direct user CREATE TABLE sqlite_stat1 is rejected with exact C SQLite message
    let err = conn
        .execute_batch("CREATE TABLE sqlite_stat1(tbl, idx, stat);")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("object name reserved for internal use: sqlite_stat1"),
        "unexpected error: {}",
        err.message
    );

    // Create user tables and indexes
    conn.execute_batch(
        "CREATE TABLE t1(a INT, b TEXT, c INT);
         CREATE INDEX t1_a ON t1(a);
         CREATE INDEX t1_bc ON t1(b COLLATE NOCASE, c);
         INSERT INTO t1 VALUES (1, 'x', 10), (1, 'X', 10), (1, 'y', 20), (2, 'z', 30);
         CREATE TABLE t_unindexed(x INT, y INT);
         INSERT INTO t_unindexed VALUES (10, 20), (30, 40), (50, 60);
         CREATE TABLE t_empty(a INT PRIMARY KEY, b INT);",
    )
    .unwrap();

    // ANALYZE non-existent table fails with "no such table: ..."
    let err = conn.execute_batch("ANALYZE no_such_table;").unwrap_err();
    assert!(
        err.message.contains("no such table: no_such_table"),
        "unexpected error: {}",
        err.message
    );

    // Run ANALYZE
    conn.execute_batch("ANALYZE;").unwrap();

    // Verify exact sqlite_stat1 contents
    let stat_rows = eval_rows(
        &mut conn,
        "SELECT tbl, COALESCE(idx, '<NULL>'), stat FROM sqlite_stat1 ORDER BY tbl, idx;",
    );
    assert_eq!(
        stat_rows,
        vec![
            vec!["t1".to_owned(), "t1_a".to_owned(), "4 2".to_owned()],
            vec!["t1".to_owned(), "t1_bc".to_owned(), "4 2 2".to_owned()],
            vec![
                "t_unindexed".to_owned(),
                "<NULL>".to_owned(),
                "3".to_owned()
            ],
        ]
    );

    // System table protections on sqlite_stat1 after creation
    let err = conn
        .execute_batch("ALTER TABLE sqlite_stat1 RENAME TO bad_stat;")
        .unwrap_err();
    assert!(
        err.message
            .contains("table sqlite_stat1 may not be altered")
    );

    let err = conn
        .execute_batch("CREATE INDEX bad_idx ON sqlite_stat1(tbl);")
        .unwrap_err();
    assert!(
        err.message
            .contains("table sqlite_stat1 may not be indexed")
    );

    let err = conn
        .execute_batch("CREATE TRIGGER bad_trig AFTER INSERT ON sqlite_stat1 BEGIN SELECT 1; END;")
        .unwrap_err();
    assert!(
        err.message
            .contains("cannot create trigger on system table"),
        "unexpected error: {}",
        err.message
    );

    // User DML (UPDATE/INSERT/DELETE) and DROP TABLE on sqlite_stat1 ARE allowed
    conn.execute_batch(
        "UPDATE sqlite_stat1 SET stat = '10000 5000' WHERE idx = 't1_a';
         ANALYZE sqlite_schema;",
    )
    .unwrap();
    assert_eq!(
        eval_flat(
            &mut conn,
            "SELECT stat FROM sqlite_stat1 WHERE idx = 't1_a';"
        ),
        vec!["10000 5000".to_owned()]
    );

    // Atomic DDL synchronization: DROP INDEX, ALTER TABLE RENAME, DROP TABLE
    conn.execute_batch("DROP INDEX t1_a;").unwrap();
    assert!(
        eval_flat(
            &mut conn,
            "SELECT idx FROM sqlite_stat1 WHERE idx = 't1_a';"
        )
        .is_empty()
    );

    conn.execute_batch("ALTER TABLE t1 RENAME TO t1_renamed;")
        .unwrap();
    assert_eq!(
        eval_flat(
            &mut conn,
            "SELECT tbl FROM sqlite_stat1 WHERE idx = 't1_bc';"
        ),
        vec!["t1_renamed".to_owned()]
    );

    conn.execute_batch("DROP TABLE t1_renamed;").unwrap();
    assert!(
        eval_flat(
            &mut conn,
            "SELECT tbl FROM sqlite_stat1 WHERE tbl = 't1_renamed';"
        )
        .is_empty()
    );

    // VACUUM preserves sqlite_stat1
    conn.execute_batch("VACUUM;").unwrap();
    assert_eq!(
        eval_flat(&mut conn, "SELECT tbl, stat FROM sqlite_stat1;"),
        vec!["t_unindexed".to_owned(), "3".to_owned()]
    );

    // User DROP TABLE sqlite_stat1 is allowed
    conn.execute_batch("DROP TABLE sqlite_stat1;").unwrap();
}

#[test]
fn cost_based_planner_stat1_selectivity_and_replan() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE items(a INT, b INT, payload TEXT);
         CREATE INDEX idx_a ON items(a);
         CREATE INDEX idx_b ON items(b);
         INSERT INTO items VALUES
           (1, 10, 'p1'), (1, 20, 'p2'), (1, 30, 'p3'), (1, 40, 'p4'),
           (1, 50, 'p5'), (1, 60, 'p6'), (1, 70, 'p7'), (1, 80, 'p8');",
    )
    .unwrap();

    // Prepare a statement before ANALYZE
    let mut stmt = conn
        .prepare("SELECT payload FROM items WHERE a = 1 AND b = 50;")
        .unwrap();
    match stmt.step().unwrap() {
        StepResult::Row(r) => assert_eq!(r.values(), &[Value::Text("p5".into())]),
        other => panic!("expected row, got {other:?}"),
    }
    stmt.reset().unwrap();

    // Run ANALYZE: idx_a has stat "8 8" (low selectivity), idx_b has stat "8 1" (high selectivity)
    conn.execute_batch("ANALYZE;").unwrap();
    let eqp = eqp_details(
        &mut conn,
        "SELECT payload FROM items WHERE a = 1 AND b = 50;",
    );
    assert_eq!(eqp, vec!["SEARCH items USING INDEX idx_b (b=?)"]);

    // Flip statistics manually in sqlite_stat1 and reload via ANALYZE sqlite_master
    conn.execute_batch(
        "UPDATE sqlite_stat1 SET stat = '1000 1' WHERE idx = 'idx_a';
         UPDATE sqlite_stat1 SET stat = '1000 500' WHERE idx = 'idx_b';
         ANALYZE sqlite_master;",
    )
    .unwrap();
    let eqp_after = eqp_details(
        &mut conn,
        "SELECT payload FROM items WHERE a = 1 AND b = 50;",
    );
    assert_eq!(eqp_after, vec!["SEARCH items USING INDEX idx_a (a=?)"]);

    // Prepared statement still executes accurately after schema/stat generation bump
    match stmt.step().unwrap() {
        StepResult::Row(r) => assert_eq!(r.values(), &[Value::Text("p5".into())]),
        other => panic!("expected row, got {other:?}"),
    }
}

#[test]
fn covering_index_vdbe_no_table_cursor_and_execution() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE cov_test(id INT PRIMARY KEY, a INT, b TEXT, c INT, d TEXT);
         CREATE INDEX cov_ab ON cov_test(a, b);
         INSERT INTO cov_test VALUES (1, 10, 'alpha', 100, 'x'), (2, 10, 'beta', 200, 'y'), (3, 20, 'gamma', 300, 'z');",
    )
    .unwrap();

    // 1. Covering index equality seek: only `a` and `b` are referenced
    let eqp = eqp_details(&mut conn, "SELECT b FROM cov_test WHERE a = 10 ORDER BY b;");
    assert_eq!(eqp[0], "SEARCH cov_test USING COVERING INDEX cov_ab (a=?)");

    // Inspect VDBE EXPLAIN opcodes: must use OpenReadIdx on `cov_ab` and MUST NOT emit OpenRead or SeekRowid on `cov_test`!
    let explain_rows = eval_rows(&mut conn, "EXPLAIN SELECT b FROM cov_test WHERE a = 10;");
    let opcodes: Vec<&str> = explain_rows.iter().map(|r| r[1].as_str()).collect();
    assert!(
        opcodes.contains(&"OpenReadIdx"),
        "expected OpenReadIdx in covering index plan: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&"OpenRead"),
        "covering index VDBE program must not open table cursor: {opcodes:?}"
    );
    assert!(
        !opcodes.contains(&"SeekRowid"),
        "covering index VDBE program must not seek table rowid: {opcodes:?}"
    );

    assert_eq!(
        eval_flat(&mut conn, "SELECT b FROM cov_test WHERE a = 10 ORDER BY b;"),
        vec!["alpha".to_owned(), "beta".to_owned()]
    );
}

#[test]
fn without_rowid_composite_pk_and_secondary_index_range_scan() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE wr(k1 TEXT, k2 INT, val TEXT, extra INT, PRIMARY KEY(k1, k2)) WITHOUT ROWID;
         CREATE INDEX wr_val ON wr(val, k1);
         INSERT INTO wr VALUES
           ('a', 1, 'v1', 10),
           ('a', 2, 'v2', 20),
           ('a', 3, 'v1', 30),
           ('b', 1, 'v1', 40);",
    )
    .unwrap();

    // Composite PK range scan (`k1 = 'a' AND k2 >= 2`)
    let eqp_pk = eqp_details(
        &mut conn,
        "SELECT k2, val FROM wr WHERE k1 = 'a' AND k2 >= 2 ORDER BY k2;",
    );
    assert_eq!(eqp_pk[0], "SEARCH wr USING PRIMARY KEY (k1=? AND k2>?)");
    assert_eq!(
        eval_flat(
            &mut conn,
            "SELECT k2, val FROM wr WHERE k1 = 'a' AND k2 >= 2 ORDER BY k2;"
        ),
        vec!["2", "v2", "3", "v1"]
    );

    // Secondary index on WITHOUT ROWID table with overlapping PK column `k1`
    let eqp_sec = eqp_details(
        &mut conn,
        "SELECT k1, k2, extra FROM wr WHERE val = 'v1' ORDER BY k1, k2;",
    );
    assert_eq!(eqp_sec[0], "SEARCH wr USING INDEX wr_val (val=?)");
    assert_eq!(
        eval_flat(
            &mut conn,
            "SELECT k1, k2, extra FROM wr WHERE val = 'v1' ORDER BY k1, k2;"
        ),
        vec!["a", "1", "10", "a", "3", "30", "b", "1", "40"]
    );
}

#[test]
fn partial_and_expression_indexes_and_skip_scan() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE ptest(a INT, b INT, c TEXT);
         CREATE INDEX p_gt5 ON ptest(a) WHERE b > 5;
         CREATE INDEX p_notnull ON ptest(c) WHERE c IS NOT NULL;
         CREATE INDEX expr_ab ON ptest(a + b);
         INSERT INTO ptest VALUES (1, 2, NULL), (2, 10, 'hello'), (3, 20, 'world');",
    )
    .unwrap();

    // Partial index usable when implied (`b > 10` implies `b > 5`; `b = 7` implies `b > 5`)
    assert_eq!(
        eqp_details(&mut conn, "SELECT a, c FROM ptest WHERE a = 2 AND b > 10;"),
        vec!["SEARCH ptest USING INDEX p_gt5 (a=?)"]
    );
    assert_eq!(
        eqp_details(&mut conn, "SELECT a, c FROM ptest WHERE a = 2 AND b = 7;"),
        vec!["SEARCH ptest USING INDEX p_gt5 (a=?)"]
    );
    // Not usable when `b > 3` does not imply `b > 5`
    assert_eq!(
        eqp_details(&mut conn, "SELECT a, c FROM ptest WHERE a = 2 AND b > 3;"),
        vec!["SCAN ptest"]
    );
    // INDEXED BY partial index when not implied must return "no query solution"
    let err = conn
        .execute_batch("SELECT a FROM ptest INDEXED BY p_gt5 WHERE a = 2 AND b > 3;")
        .unwrap_err();
    assert!(
        err.message.contains("no query solution"),
        "unexpected error: {}",
        err.message
    );

    // `c = 'hello'` implies `c IS NOT NULL`
    assert_eq!(
        eqp_details(&mut conn, "SELECT a FROM ptest WHERE c = 'hello';"),
        vec!["SEARCH ptest USING INDEX p_notnull (c=?)"]
    );

    // Expression index (`a + b = 12`)
    assert_eq!(
        eqp_details(&mut conn, "SELECT c FROM ptest WHERE a + b = 12;"),
        vec!["SEARCH ptest USING INDEX expr_ab (<expr>=?)"]
    );
    assert_eq!(
        eval_flat(&mut conn, "SELECT c FROM ptest WHERE a + b = 12;"),
        vec!["hello".to_owned()]
    );

    // Skip-scan when leading column has high repetition (`d1 >= 18`)
    conn.execute_batch(
        "CREATE TABLE skip_t(flag INT, code INT, info TEXT);
         CREATE INDEX skip_idx ON skip_t(flag, code);
         ANALYZE;
         INSERT INTO sqlite_stat1 VALUES ('skip_t', 'skip_idx', '1000 500 1');
         ANALYZE sqlite_master;
         INSERT INTO skip_t VALUES (0, 42, 'zero-42'), (1, 42, 'one-42'), (1, 99, 'one-99');",
    )
    .unwrap();
    assert_eq!(
        eqp_details(&mut conn, "SELECT info FROM skip_t WHERE code = 42;"),
        vec!["SEARCH skip_t USING INDEX skip_idx (ANY(flag) AND code=?)"]
    );
    assert_eq!(
        eval_flat(
            &mut conn,
            "SELECT info FROM skip_t WHERE code = 42 ORDER BY info;"
        ),
        vec!["one-42".to_owned(), "zero-42".to_owned()]
    );

    // `noskipscan` disables skip-scan
    conn.execute_batch(
        "UPDATE sqlite_stat1 SET stat = '1000 500 1 noskipscan' WHERE idx = 'skip_idx';
         ANALYZE sqlite_master;",
    )
    .unwrap();
    assert_eq!(
        eqp_details(&mut conn, "SELECT info FROM skip_t WHERE code = 42;"),
        vec!["SCAN skip_t"]
    );
}

#[test]
fn differential_against_c_sqlite_reference() {
    let setup_sql = "
        CREATE TABLE t1(a INT, b INT, c TEXT);
        CREATE INDEX i1 ON t1(a, b);
        CREATE INDEX i2 ON t1(c) WHERE a > 0;
        INSERT INTO t1 VALUES (1, 10, 'x'), (1, 20, 'y'), (2, 30, 'z'), (-1, 40, 'x');
        ANALYZE;
    ";
    let queries = [
        "SELECT tbl, idx, stat FROM sqlite_stat1 ORDER BY tbl, idx;",
        "SELECT a, b, c FROM t1 WHERE a = 1 AND b > 10;",
        "SELECT a, b FROM t1 WHERE c = 'x' AND a > 0;",
        "SELECT a, b, c FROM t1 WHERE a IN (1, 2) AND b IN (10, 30) ORDER BY a, b;",
    ];
    let full_ref_sql = format!("{setup_sql}\n{}", queries.join("\n"));
    if let Some(expected) = run_ref_sqlite(&full_ref_sql) {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(setup_sql).unwrap();
        let mut out_lines = Vec::new();
        for q in queries {
            for r in eval_rows(&mut conn, q) {
                out_lines.push(r.join("|"));
            }
        }
        assert_eq!(out_lines.join("\n"), expected);
    }
}

#[test]
fn targeted_tcl_suite_audit_phase5b() {
    let test_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("sqlite/test");
    if !test_dir.exists() {
        return;
    }
    let strict_phase5b_suites = [
        "analyze", "analyze4", "analyze7", "analyzeC", "index6", "index7", "in4", "count",
        "between", "whereE", "whereF", "whereH", "whereI", "whereK", "whereM",
    ];
    let files = [
        "analyze", "analyze4", "analyze7", "analyzeC", "index6", "index7", "in4", "count", "fkey1",
        "between", "where2", "where3", "where7", "where9", "whereA", "whereE", "whereF", "whereG",
        "whereH", "whereI", "whereK", "whereL", "whereM", "whereN", "with1", "with3",
    ];
    let mut total_passed = 0usize;
    let mut total_failed = 0usize;
    for name in files {
        let path = test_dir.join(format!("{name}.test"));
        if !path.exists() {
            continue;
        }
        let raw_script = fs::read_to_string(&path).unwrap();
        // Enable `ifcapable analyze` in-memory for the Step 5-B targeted audit while keeping
        // `("analyze", false)` in `PHASE1_CAPABILITY_TABLE` until Step 5-C GATE-5.
        let script = raw_script
            .replace("ifcapable {!analyze}", "if {0}")
            .replace("ifcapable !analyze", "if {0}")
            .replace("ifcapable {!analyze||!vtab}", "if {0}")
            .replace("ifcapable !vtab", "if {0}")
            .replace("load_static_extension db wholenumber;", "")
            .replace("load_static_extension db wholenumber", "")
            .replace(
                "CREATE VIRTUAL TABLE nums USING wholenumber;",
                "CREATE TEMP VIEW nums(value) AS WITH RECURSIVE c(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM c WHERE x<2000) SELECT x FROM c;",
            );
        let name_owned = name.to_owned();
        let report = std::thread::Builder::new()
            .name(format!("tcl-{name}"))
            .stack_size(32 * 1024 * 1024)
            .spawn(move || evaluate_test_script(&name_owned, &script))
            .unwrap()
            .join()
            .unwrap();
        println!(
            "[TCL-AUDIT] {name}.test: passed={}, failed={}, script_err={:?}",
            report.passed,
            report.failed.len(),
            report.script_error
        );
        for f in report.failed.iter().take(5) {
            println!(
                "   FAIL [{}]: expected=[{}] actual=[{}]",
                f.name, f.expected, f.actual
            );
        }
        if strict_phase5b_suites.contains(&name) {
            assert!(
                report.script_error.is_none(),
                "{name}.test had script error: {:?}",
                report.script_error
            );
            assert!(
                report.failed.is_empty(),
                "{name}.test had {} failed subtests: {:?}",
                report.failed.len(),
                report.failed
            );
            assert!(report.passed > 0, "{name}.test passed 0 subtests");
        }
        total_passed += report.passed;
        total_failed += report.failed.len();
    }
    println!("[TCL-AUDIT-TOTAL] passed={total_passed}, failed={total_failed}");
    assert!(total_passed > 0, "expected Tcl tests to run and pass");
}
