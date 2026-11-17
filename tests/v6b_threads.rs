use sqliters::{Connection, StepResult, Value};

fn integer(c: &Connection, sql: &str) -> i64 {
    match &c.query(sql, &[]).unwrap().rows[0].values()[0] {
        Value::Integer(n) => *n,
        other => panic!("{sql}: {other:?}"),
    }
}

#[test]
fn threads_setting_uses_c_integer_mask_and_hard_limit() {
    let c = Connection::open_in_memory().unwrap();
    assert_eq!(integer(&c, "PRAGMA threads"), 0);
    for (setting, expected) in [
        ("3", 3),
        ("99", 8),
        ("2147483648", 0),
        ("0x80000007", 7),
        ("-1", 7),
        ("'4junk'", 7),
        ("'9223372036854775808'", 7),
    ] {
        assert_eq!(integer(&c, &format!("PRAGMA threads={setting}")), expected);
        assert_eq!(integer(&c, "PRAGMA threads"), expected);
    }
}

#[test]
fn threads_prepare_freezes_rows_and_reset_reprepares_self() {
    let c = Connection::open_in_memory().unwrap();
    let mut old = c.prepare("PRAGMA threads").unwrap();
    let mut setter = c.prepare("PRAGMA threads=7").unwrap();
    assert_eq!(integer(&c, "SELECT * FROM pragma_threads"), 7);
    c.prepare("EXPLAIN PRAGMA threads=2").unwrap();
    assert_eq!(integer(&c, "PRAGMA threads"), 2);
    assert!(matches!(old.step().unwrap(), StepResult::Row(r) if r.values()==[Value::Integer(0)]));
    assert!(
        matches!(setter.step().unwrap(), StepResult::Row(r) if r.values()==[Value::Integer(7)])
    );
    assert_eq!(integer(&c, "PRAGMA threads"), 2);
    setter.reset().unwrap();
    assert!(
        matches!(setter.step().unwrap(), StepResult::Row(r) if r.values()==[Value::Integer(7)])
    );
    assert_eq!(integer(&c, "PRAGMA threads"), 7);
}

#[test]
fn threads_is_per_connection_global_across_schemas_and_transactional_control() {
    let a = Connection::open_in_memory().unwrap();
    let b = Connection::open_in_memory().unwrap();
    a.execute_batch("ATTACH ':memory:' AS aux; BEGIN").unwrap();
    assert_eq!(integer(&a, "PRAGMA aux.threads=5"), 5);
    a.execute_batch("ROLLBACK").unwrap();
    assert_eq!(integer(&a, "PRAGMA main.threads"), 5);
    assert_eq!(integer(&a, "PRAGMA temp.threads"), 5);
    assert_eq!(integer(&b, "PRAGMA threads"), 0);
}
