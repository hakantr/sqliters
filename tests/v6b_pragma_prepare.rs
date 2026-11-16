//! PRAGMA prepare/step boundaries, compared with SQLite 3.54.1.
use sqliters::types::Value;
use sqliters::{Connection, StepResult};

fn int(conn: &Connection, sql: &str) -> i64 {
    let rows = conn.query(sql, &[]).unwrap();
    let Value::Integer(n) = rows.rows[0].values()[0] else {
        panic!("{sql}: {rows:?}")
    };
    n
}

#[test]
fn default_cache_size_changes_cache_at_prepare_and_cookie_at_step() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(int(&conn, "PRAGMA default_cache_size"), -2000);
    let mut setter = conn.prepare("PRAGMA default_cache_size=17").unwrap();
    assert_eq!(int(&conn, "PRAGMA cache_size"), 17);
    assert_eq!(int(&conn, "PRAGMA default_cache_size"), -2000);
    conn.prepare("PRAGMA cache_size=23").unwrap();
    assert!(matches!(setter.step().unwrap(), StepResult::Done));
    assert_eq!(int(&conn, "PRAGMA default_cache_size"), 17);
    assert_eq!(int(&conn, "PRAGMA cache_size"), 23);
    setter.reset().unwrap();
    assert!(matches!(setter.step().unwrap(), StepResult::Done));
    assert_eq!(int(&conn, "PRAGMA cache_size"), 17);
    conn.prepare("EXPLAIN PRAGMA default_cache_size=83")
        .unwrap();
    assert_eq!(int(&conn, "PRAGMA cache_size"), 83);
    assert_eq!(int(&conn, "PRAGMA default_cache_size"), 17);
}

#[test]
fn default_cache_size_uses_c_integer_conversion_and_temp_cookie() {
    let conn = Connection::open_in_memory().unwrap();
    for (literal, n) in [
        ("-2147483648", i32::MAX as i64),
        ("2147483648", 0),
        ("0x10000000000000001", 0),
        ("'17junk'", 17),
        ("-12", 12),
    ] {
        let mut setter = conn
            .prepare(&format!("PRAGMA default_cache_size={literal}"))
            .unwrap();
        assert_eq!(int(&conn, "PRAGMA cache_size"), n);
        setter.step().unwrap();
        assert_eq!(
            int(&conn, "PRAGMA default_cache_size"),
            if n == 0 { -2000 } else { n }
        );
    }
    let mut setter = conn.prepare("PRAGMA temp.default_cache_size=29").unwrap();
    assert_eq!(int(&conn, "PRAGMA temp.cache_size"), 29);
    assert_eq!(int(&conn, "PRAGMA temp.default_cache_size"), -2000);
    setter.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA temp.default_cache_size"), 29);
}

#[test]
fn default_cache_size_is_per_schema_and_survives_cookie_rollback() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("ATTACH ':memory:' AS aux; CREATE TABLE t(a); BEGIN;")
        .unwrap();
    conn.execute("PRAGMA aux.default_cache_size=43").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.cache_size"), 43);
    assert_eq!(int(&conn, "PRAGMA cache_size"), -2000);
    conn.execute("ROLLBACK").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.cache_size"), 43);
    assert_eq!(int(&conn, "PRAGMA aux.default_cache_size"), -2000);
}

#[test]
fn synchronous_changes_at_prepare_without_reapplying_at_step() {
    let conn = Connection::open_in_memory().unwrap();
    let mut setter = conn.prepare("PRAGMA synchronous=EXTRA").unwrap();
    let mut snapshot = conn.prepare("PRAGMA synchronous").unwrap();
    conn.prepare("PRAGMA synchronous=4").unwrap();
    assert_eq!(int(&conn, "PRAGMA synchronous"), 4);
    setter.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA synchronous"), 4);
    assert!(matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(3)));
    conn.prepare("EXPLAIN PRAGMA synchronous=6").unwrap();
    assert_eq!(int(&conn, "PRAGMA synchronous"), 6);
    conn.prepare("PRAGMA temp.synchronous=3").unwrap();
    assert_eq!(int(&conn, "PRAGMA temp.synchronous"), 0);
    conn.execute("BEGIN").unwrap();
    for schema in ["main", "temp"] {
        let sql = format!("PRAGMA {schema}.synchronous=2");
        let err = conn.prepare(&sql).err().expect("transaction must reject setter");
        assert_eq!(err.message, "Safety level may not be changed inside a transaction");
    }
    conn.execute("ROLLBACK").unwrap();
    assert_eq!(int(&conn, "PRAGMA synchronous"), 6);
}

#[test]
fn synchronous_preserves_c_numeric_mask_and_text_defaults() {
    let conn = Connection::open_in_memory().unwrap();
    for (literal, n) in [
        ("0", 0), ("1", 1), ("2", 2), ("3", 3), ("4", 4), ("5", 5),
        ("6", 6), ("7", 0), ("8", 0), ("9", 1), ("15", 0),
        ("2147483647", 0), ("2147483648", 0), ("-1", 1),
        ("'3junk'", 3), ("'unknown'", 1), ("FULL", 2), ("EXTRA", 3),
    ] {
        conn.prepare(&format!("PRAGMA synchronous={literal}")).unwrap();
        assert_eq!(int(&conn, "PRAGMA synchronous"), n, "{literal}");
    }
}
