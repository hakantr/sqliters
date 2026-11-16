//! SQLite 3.54.1 page-size decisions at prepare, TEMP open and VACUUM.
use sqliters::{Connection, StepResult, Value};

fn int(conn: &Connection, sql: &str) -> i64 {
    let rows = conn.query(sql, &[]).unwrap();
    let Value::Integer(n) = rows.rows[0].values()[0] else {
        panic!("{rows:?}");
    };
    n
}

#[test]
fn page_size_applies_at_prepare_and_query_keeps_its_prepared_value() {
    let conn = Connection::open_in_memory().unwrap();
    let mut setter = conn.prepare("PRAGMA page_size=8192").unwrap();
    let mut snapshot = conn.prepare("PRAGMA page_size").unwrap();
    conn.prepare("PRAGMA page_size=1024").unwrap();
    setter.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 1024);
    assert!(
        matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(8192))
    );
    conn.prepare("EXPLAIN PRAGMA page_size=2048").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 2048);
    conn.execute_batch("ATTACH ':memory:' AS aux;").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.page_size"), 4096);
    assert_eq!(int(&conn, "PRAGMA temp.page_size"), 2048);
}

#[test]
fn page_size_inside_empty_transaction_survives_rollback_and_invalid_request() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("BEGIN").unwrap();
    conn.prepare("PRAGMA page_size=8192").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 8192);
    conn.execute("ROLLBACK").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 8192);
    conn.prepare("PRAGMA page_size='not a size'").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 8192);
    assert_eq!(int(&conn, "PRAGMA temp.page_size"), 4096);
}

#[test]
fn page_size_request_is_shared_by_schemas_and_preserved_by_vacuum() {
    let dir = std::env::temp_dir().join(format!("v6b-page-global-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = Connection::open(dir.join("main.db")).unwrap();
    let aux = dir.join("aux.db").to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "CREATE TABLE t(a); ATTACH '{aux}' AS aux; CREATE TABLE aux.t(a);"
    ))
    .unwrap();
    conn.prepare("PRAGMA aux.page_size=8192").unwrap();
    conn.execute("VACUUM").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_size"), 8192);
    conn.execute("VACUUM aux").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.page_size"), 8192);
    conn.prepare("PRAGMA page_size=1024").unwrap();
    conn.prepare("PRAGMA page_size=123").unwrap();
    conn.execute("VACUUM aux").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.page_size"), 8192);
    conn.try_close().unwrap();
    let reopened = Connection::open(dir.join("main.db")).unwrap();
    assert_eq!(int(&reopened, "PRAGMA page_size"), 8192);
    assert_eq!(int(&reopened, "PRAGMA data_version"), 2);
    reopened.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn page_size_prepare_does_not_acquire_a_shared_lock() {
    let dir = std::env::temp_dir().join(format!("v6b-page-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = Connection::open(dir.join("main.db")).unwrap();
    a.execute_batch("CREATE TABLE t(a); BEGIN EXCLUSIVE; INSERT INTO t VALUES(1);")
        .unwrap();
    let b = Connection::open(dir.join("main.db")).unwrap();
    b.prepare("PRAGMA page_size=8192").unwrap();
    assert_eq!(int(&b, "PRAGMA page_size"), 4096);
    b.try_close().unwrap();
    a.execute("ROLLBACK").unwrap();
    a.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
