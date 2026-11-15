//! C-alignment regression tests (step M3: INSERT / UPDATE / DELETE through the port of C's code generator and the
//! machine). With the switch on for the test thread, C's compile result decides the statement: C's program runs on
//! the machine, C's compile error is the error of the prepare. Every expected line is what the reference `sqlite3`
//! 3.54.1 prints for the same statements (rows as `a|b`, an error as `err: <message>`).
use crate::{Connection, StepResult};
use sqliters_types::Value;

fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format!("{r:?}"),
        Value::Text(t) => t.to_string(),
        Value::Blob(b) => format!(
            "x'{}'",
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        ),
    }
}

/// Each statement prepared and stepped to its end; rows and errors in order.
fn run(conn: &Connection, sql: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for s in sql {
        match conn.prepare(s) {
            Ok(mut st) => loop {
                match st.step() {
                    Ok(StepResult::Row(r)) => {
                        out.push(r.values().iter().map(show).collect::<Vec<_>>().join("|"))
                    }
                    Ok(StepResult::Done) => break,
                    Err(e) => {
                        out.push(format!("err: {}", e.message));
                        break;
                    }
                }
            },
            Err(e) => out.push(format!("err: {}", e.message)),
        }
    }
    out
}

fn run_on(sql: &[&str]) -> Vec<String> {
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    run(&conn, sql)
}

/// D43 / D44 / D48: C's compile result decides (the engine's prepare would reject the DELETE, accept the
/// generated-column loop, and report its own texts).
#[test]
fn m3_routed_dml_reports_c_compile_results() {
    let out = run_on(&[
        "CREATE TABLE u(x PRIMARY KEY, y)",
        "INSERT INTO u VALUES(1,2) ON CONFLICT(y) DO NOTHING",
        "CREATE TABLE p(a,b)",
        "CREATE INDEX pi ON p(a) WHERE a>0",
        "INSERT INTO p VALUES(1,1),(2,2)",
        "DELETE FROM p INDEXED BY pi",
        "SELECT count(*) FROM p",
        "CREATE VIEW v AS SELECT 1 AS a",
        "INSERT INTO v VALUES(1) ON CONFLICT DO NOTHING",
        "INSERT INTO u AS ins VALUES(1,2,3)",
        "CREATE TABLE g(a INT, b INT, c INT GENERATED ALWAYS AS (e * b) STORED, d INT, e INT GENERATED ALWAYS AS (b * c) STORED)",
        "INSERT INTO g(a,b,d) VALUES(1,2,3)",
        "UPDATE g SET b=5",
        "SELECT count(*) FROM g",
    ]);
    assert_eq!(
        out,
        [
            "err: ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
            "0",
            "err: cannot modify v because it is a view",
            "err: table ins has 2 columns but 3 values were supplied",
            "err: generated column loop on \"e\"",
            "err: generated column loop on \"e\"",
            "0",
        ]
    );
    assert!(crate::vdbe_host::machine_runs() > 0);
}

/// E17 / D30: a PRIMARY KEY equivalent to an earlier UNIQUE constraint is that UNIQUE index, with its sort order
/// (build.c: "considered equivalent ... even if they have different sort orders"); the rows are stored ascending and
/// the UNIQUE conflict of the UPDATE is found. Same with the switch off (engine) and on (machine).
#[test]
fn m3_unique_before_primary_key_desc_keeps_the_unique_index_order() {
    let sql = [
        "CREATE TABLE t0(a TEXT UNIQUE, b INT, PRIMARY KEY(a DESC)) WITHOUT ROWID",
        "INSERT OR IGNORE INTO t0(a, b) VALUES(2, 9)",
        "INSERT OR IGNORE INTO t0(a, b) VALUES(6, 707)",
        "INSERT OR IGNORE INTO t0(a, b) VALUES(-3, 2)",
        "SELECT * FROM t0",
        "UPDATE OR ROLLBACK t0 SET b = a, a = length((SELECT max(a) FROM t0)) WHERE b <= 17",
        "CREATE TABLE t2(b0 INT2 UNIQUE PRIMARY KEY DESC, zz2) WITHOUT ROWID",
        "INSERT INTO t2 VALUES (4,'a'),(5,7),(1,10),(10,3)",
        "SELECT * FROM t2",
        "SELECT zz2 FROM t2 WHERE b0=1",
    ];
    let expected = [
        "-3|2",
        "2|9",
        "6|707",
        "err: UNIQUE constraint failed: t0.a",
        "1|10",
        "4|a",
        "5|7",
        "10|3",
        "10",
    ];
    assert_eq!(run_on(&sql), expected);
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(run(&conn, &sql), expected);
}

/// xferOptimization (OP_RowCell, OPFLAG_PREFORMAT): the copy into a table that is the parent of a foreign key is
/// not checked (C's xfer program has no FK code); overflow rows are transferred.
#[test]
fn m3_xfer_insert_select_on_the_machine() {
    let big = "x".repeat(3000);
    let ins = format!("INSERT INTO s VALUES (1, '{big}', 1), (2, 'b', 2)");
    let out = run_on(&[
        "PRAGMA foreign_keys=1",
        "CREATE TABLE t(a REAL, b TEXT, c ANY) STRICT",
        "CREATE TABLE s(a REAL, b TEXT, c ANY) STRICT",
        "CREATE TABLE ch(d PRIMARY KEY, FOREIGN KEY(d) REFERENCES t(b)) WITHOUT ROWID",
        &ins,
        "INSERT INTO t SELECT * FROM s",
        "SELECT a, length(b), c FROM t",
        "INSERT INTO t SELECT * FROM s",
        "SELECT count(*) FROM t",
    ]);
    assert_eq!(out, ["1.0|3000|1", "2.0|1|2", "4"]);
}

/// E16: OP_IdxDelete of an entry whose stored key differs from the recomputed one in an expression / virtual
/// column field (here: stored by the engine, which keeps invalid UTF-8 text lossily) is found among its neighbours
/// (sqlite3VdbeFindIndexKey) instead of reporting SQLITE_CORRUPT_INDEX.
#[test]
fn m3_idxdelete_searches_an_expression_index() {
    let conn = Connection::open_in_memory().unwrap();
    run(
        &conn,
        &[
            "CREATE TABLE t1(a BLOB, b TEXT, c INTEGER GENERATED ALWAYS AS (a || a) VIRTUAL, PRIMARY KEY(b DESC, a)) WITHOUT ROWID",
            "CREATE INDEX t1i1 ON t1(c)",
            "INSERT INTO t1(a, b) VALUES(x'6699', 'zz')",
            "INSERT INTO t1(a, b) VALUES('b', 695)",
        ],
    );
    let _on = crate::vdbe_host::ForceMachine::on();
    let out = run(
        &conn,
        &["DELETE FROM t1 RETURNING b", "SELECT count(*) FROM t1"],
    );
    assert_eq!(out, ["zz", "695", "0"]);
}

/// `sqlite3_update_hook` events of OP_Insert / OP_Delete (statements on the machine), as the C library reports
/// them (target/perf-work/m3/hook/hook.c against the amalgamation): `-- <sql>` before the events of a statement.
#[test]
fn m3_update_hook_events_of_the_machine() {
    use std::sync::{Arc, Mutex};
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let l2 = Arc::clone(&log);
    conn.update_hook(Some(Box::new(move |op, db, tb, rowid| {
        let op = match op {
            sqliters_types::RowMutationOp::Insert => "INSERT",
            sqliters_types::RowMutationOp::Update => "UPDATE",
            sqliters_types::RowMutationOp::Delete => "DELETE",
        };
        l2.lock()
            .unwrap()
            .push(format!("hook {op} {db}.{tb} {rowid}"));
    })))
    .unwrap();
    let sql = [
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b UNIQUE, c)",
        "CREATE TABLE w(k PRIMARY KEY, v) WITHOUT ROWID",
        "CREATE TABLE s(x, y)",
        "INSERT INTO t VALUES(1,'a',1),(2,'b',2),(3,'c',3)",
        "INSERT INTO w VALUES('p',1),('q',2)",
        "UPDATE t SET c=c+1 WHERE a>=2",
        "UPDATE t SET a=10 WHERE a=1",
        "REPLACE INTO t VALUES(2,'c',9)",
        "INSERT INTO t VALUES(20,'b',0) ON CONFLICT(b) DO UPDATE SET c=99",
        "UPDATE w SET v=v+1",
        "DELETE FROM w WHERE k='p'",
        "INSERT INTO s SELECT a, b FROM t",
        "INSERT INTO s SELECT * FROM s",
        "DELETE FROM t WHERE a=3",
        "DELETE FROM s",
        "DELETE FROM t",
    ];
    for s in sql {
        log.lock().unwrap().push(format!("-- {s}"));
        if let Err(e) = conn.execute_batch(s) {
            log.lock().unwrap().push(format!("err: {}", e.message));
        }
    }
    let expected = [
        "-- CREATE TABLE t(a INTEGER PRIMARY KEY, b UNIQUE, c)",
        "-- CREATE TABLE w(k PRIMARY KEY, v) WITHOUT ROWID",
        "-- CREATE TABLE s(x, y)",
        "-- INSERT INTO t VALUES(1,'a',1),(2,'b',2),(3,'c',3)",
        "hook INSERT main.t 1",
        "hook INSERT main.t 2",
        "hook INSERT main.t 3",
        "-- INSERT INTO w VALUES('p',1),('q',2)",
        "-- UPDATE t SET c=c+1 WHERE a>=2",
        "hook UPDATE main.t 2",
        "hook UPDATE main.t 3",
        "-- UPDATE t SET a=10 WHERE a=1",
        "hook UPDATE main.t 10",
        "-- REPLACE INTO t VALUES(2,'c',9)",
        "hook INSERT main.t 2",
        "-- INSERT INTO t VALUES(20,'b',0) ON CONFLICT(b) DO UPDATE SET c=99",
        "hook INSERT main.t 20",
        "-- UPDATE w SET v=v+1",
        "-- DELETE FROM w WHERE k='p'",
        "-- INSERT INTO s SELECT a, b FROM t",
        "hook INSERT main.s 1",
        "hook INSERT main.s 2",
        "hook INSERT main.s 3",
        "-- INSERT INTO s SELECT * FROM s",
        "hook INSERT main.s 4",
        "hook INSERT main.s 5",
        "hook INSERT main.s 6",
        "-- DELETE FROM t WHERE a=3",
        "-- DELETE FROM s",
        "-- DELETE FROM t",
    ];
    assert_eq!(*log.lock().unwrap(), expected);
}
