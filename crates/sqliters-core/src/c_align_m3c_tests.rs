//! C-alignment regression tests (step M3c: transaction statements and virtual-table reads on the port of C's VDBE).
//! With the switch on for the test thread, BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE run C's program
//! (`OP_AutoCommit`, `OP_Savepoint`, `OP_Transaction`) on the machine, and a SELECT that reads a virtual table runs on
//! the machine with C's xBestIndex plan handed to xFilter (`VirtualCursor::filter_c`). Every expected line is what
//! the reference `sqlite3` 3.54.1 (or the C API driver `m3c/drv/c/rdrv4`) gives for the same statements.
use crate::{Connection, StepResult};
use sqliters_types::Value;

fn show(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(r) => format!("{r:?}"),
        Value::Text(t) => t.to_string(),
        Value::Blob(b) => format!("x'{}'", b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
    }
}

fn run(conn: &Connection, sql: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for s in sql {
        match conn.prepare(s) {
            Ok(mut st) => loop {
                match st.step() {
                    Ok(StepResult::Row(r)) => out.push(r.values().iter().map(show).collect::<Vec<_>>().join("|")),
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

/// OP_AutoCommit / OP_Savepoint: C's errors and the transaction state they leave.
#[test]
fn m3c_transaction_statements_run_on_the_machine() {
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    let before = crate::vdbe_host::machine_runs();
    let out = run(
        &conn,
        &[
            "CREATE TABLE t(a)",
            "COMMIT",
            "ROLLBACK",
            "BEGIN",
            "BEGIN",
            "SAVEPOINT s",
            "INSERT INTO t VALUES(1)",
            "RELEASE nosuch",
            "ROLLBACK TO s",
            "SELECT count(*) FROM t",
            "RELEASE s",
            "COMMIT",
            "SAVEPOINT a",
            "INSERT INTO t VALUES(2)",
            "SAVEPOINT b",
            "INSERT INTO t VALUES(3)",
            "ROLLBACK TO a",
            "RELEASE a",
            "SELECT count(*) FROM t",
        ],
    );
    assert_eq!(
        out,
        vec![
            "err: cannot commit - no transaction is active",
            "err: cannot rollback - no transaction is active",
            "err: cannot start a transaction within a transaction",
            "err: no such savepoint: nosuch",
            "0",
            "0",
        ]
    );
    assert!(conn.is_autocommit());
    assert!(crate::vdbe_host::machine_runs() - before >= 15);
}

/// Deferred foreign keys at COMMIT and at the RELEASE of the transaction savepoint: SQLITE_CONSTRAINT_FOREIGNKEY,
/// the transaction stays open (sqlite3VdbeCheckFkDeferred, `goto vdbe_return`).
#[test]
fn m3c_deferred_foreign_keys_hold_commit_and_release() {
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    let out = run(
        &conn,
        &[
            "PRAGMA foreign_keys=1",
            "CREATE TABLE p(id INTEGER PRIMARY KEY)",
            "CREATE TABLE c(x REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
            "BEGIN",
            "INSERT INTO c VALUES(1)",
            "COMMIT",
        ],
    );
    assert_eq!(out, vec!["err: FOREIGN KEY constraint failed"]);
    assert!(!conn.is_autocommit());
    let out = run(&conn, &["INSERT INTO p VALUES(1)", "COMMIT", "SAVEPOINT a", "INSERT INTO c VALUES(2)", "RELEASE a"]);
    assert_eq!(out, vec!["err: FOREIGN KEY constraint failed"]);
    assert!(!conn.is_autocommit());
    let out = run(&conn, &["ROLLBACK", "SELECT count(*) FROM c"]);
    assert_eq!(out, vec!["1"]);
}

/// SQLITE_BUSY of COMMIT (a reader on another connection): the transaction stays open and the same statement
/// commits once the reader is gone (`p->pc` stays at OP_AutoCommit).
#[test]
fn m3c_busy_commit_leaves_the_transaction_open() {
    let dir = std::env::temp_dir().join(format!("sqliters_m3c_busy_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("b.db");
    let _ = std::fs::remove_file(&path);
    let c1 = Connection::open(&path).unwrap();
    let c2 = Connection::open(&path).unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    c1.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(1);").unwrap();
    assert!(run(&c1, &["BEGIN", "INSERT INTO t VALUES(2)"]).is_empty());
    c2.execute_batch("BEGIN; SELECT count(*) FROM t;").unwrap();
    let mut commit = c1.prepare("COMMIT").unwrap();
    let e = commit.step().unwrap_err();
    assert_eq!(e.message, "database is locked");
    assert!(!c1.is_autocommit());
    c2.execute_batch("COMMIT").unwrap();
    assert!(matches!(commit.step(), Ok(StepResult::Done)));
    drop(commit);
    assert!(c1.is_autocommit());
    assert_eq!(run(&c2, &["SELECT count(*) FROM t"]), vec!["2"]);
    drop(c1);
    drop(c2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// sqlite3VdbeHalt stores `changes()` when a RETURNING statement halts, not when it returns its first row
/// (C API driver: `changes()` stays 0 after the first row of a two-row INSERT ... RETURNING).
#[test]
fn m3c_returning_changes_counted_at_halt() {
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    conn.execute_batch("CREATE TABLE t(a INTEGER PRIMARY KEY, b)").unwrap();
    let mut st = conn.prepare("INSERT INTO t(b) VALUES(1),(2) RETURNING a").unwrap();
    assert!(matches!(st.step(), Ok(StepResult::Row(_))));
    assert_eq!((conn.changes(), conn.total_changes()), (0, 0));
    assert!(matches!(st.step(), Ok(StepResult::Row(_))));
    assert!(matches!(st.step(), Ok(StepResult::Done)));
    assert_eq!((conn.changes(), conn.total_changes()), (2, 2));
}

/// D37 / D35: virtual tables read on the machine get C's xBestIndex plan (constraints on hidden columns are
/// checked by C's program; LIMIT / OFFSET reach generate_series; dbstat's ORDER BY name / name= / aggregate).
#[test]
fn m3c_virtual_table_reads_take_c_plans() {
    let conn = Connection::open_in_memory().unwrap();
    let _on = crate::vdbe_host::ForceMachine::on();
    let out = run(
        &conn,
        &[
            "SELECT value FROM generate_series(0,3,1) WHERE stop=1",
            "SELECT value FROM generate_series(1,20,3) LIMIT 3 OFFSET 2",
            "SELECT value FROM generate_series(10,1,-3)",
            "CREATE TABLE t2(a, b UNIQUE)",
            "CREATE INDEX t2a ON t2(a)",
            "SELECT name FROM pragma_table_info('t2')",
            "SELECT seq FROM pragma_index_list('t2') WHERE arg='t1'",
            "SELECT name FROM dbstat('main',1)",
            "SELECT name FROM dbstat WHERE name='t2a'",
        ],
    );
    assert_eq!(
        out,
        vec!["7", "10", "13", "10", "7", "4", "1", "a", "b", "sqlite_schema", "t2", "sqlite_autoindex_t2_1", "t2a", "t2a"]
    );
}
