//! Schema loading and execution locks follow C's compiler decisions.
use sqliters::{Connection, ErrorCode, StepResult, Value};

fn malformed() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(23); PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='broken' WHERE name='t';").unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    c
}

#[test]
fn scalar_prepare_and_preview_do_not_load_a_reset_schema() {
    let c = malformed();
    let p = c.c_generated_program("SELECT 17").unwrap();
    assert_eq!(p.btree_mask, 0);
    assert_eq!(
        c.query("SELECT 17", &[]).unwrap().rows[0].values()[0],
        Value::Integer(17)
    );
    assert_eq!(
        c.prepare("SELECT * FROM t").err().unwrap().code,
        ErrorCode::Corrupt
    );
}

#[test]
fn pending_scalar_survives_reset_and_reprepares_without_schema_io() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a)").unwrap();
    let mut s = c.prepare("SELECT ?1").unwrap();
    s.bind(1, Value::Integer(17)).unwrap();
    c.execute_batch(
        "PRAGMA writable_schema=ON; UPDATE sqlite_schema SET sql='broken' WHERE name='t';",
    )
    .unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    assert!(matches!(s.step().unwrap(), StepResult::Row(r) if r.values()==[Value::Integer(17)]));
    s.reset().unwrap();
    c.execute_batch("PRAGMA automatic_index=OFF").unwrap();
    assert!(matches!(s.step().unwrap(), StepResult::Row(r) if r.values()==[Value::Integer(17)]));
}

#[test]
fn values_coroutine_reads_schema_but_literal_cte_does_not() {
    let c = malformed();
    assert_eq!(
        c.prepare("VALUES(1),(2)").err().unwrap().code,
        ErrorCode::Corrupt
    );
    let c = malformed();
    let rows = c
        .query("WITH q(a) AS (VALUES(1),(2)) SELECT a FROM q", &[])
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values()[0], Value::Integer(1));
    assert_eq!(rows[1].values()[0], Value::Integer(2));
}

#[test]
fn eliminated_table_sources_do_not_load_schemas_or_acquire_read_leases() {
    let c = malformed();
    assert_eq!(
        c.query("WITH q AS (SELECT * FROM t) SELECT 17", &[])
            .unwrap()
            .rows[0]
            .values()[0],
        Value::Integer(17)
    );
    assert!(
        c.query("SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t)", &[])
            .unwrap()
            .rows
            .is_empty()
    );
}

#[test]
fn cold_scalar_reads_schema_but_loaded_and_reset_scalars_do_not() {
    let path = std::env::temp_dir().join(format!(
        "sqliters_v6b_schema_{}_{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let a = Connection::open(&path).unwrap();
    a.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(23)")
        .unwrap();
    let b = Connection::open(&path).unwrap();
    a.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert_eq!(b.prepare("SELECT 17").err().unwrap().code, ErrorCode::Busy);
    a.execute_batch("ROLLBACK").unwrap();
    b.query("SELECT * FROM t", &[]).unwrap();
    a.execute_batch("BEGIN EXCLUSIVE").unwrap();
    for reset in [false, true] {
        if reset {
            b.execute_batch("PRAGMA writable_schema=RESET").unwrap();
        }
        assert_eq!(
            b.query("SELECT 17", &[]).unwrap().rows[0].values()[0],
            Value::Integer(17)
        );
        assert_eq!(
            b.query("WITH q AS (SELECT * FROM t) SELECT 17", &[])
                .unwrap()
                .rows[0]
                .values()[0],
            Value::Integer(17)
        );
        assert!(
            b.query("SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t)", &[])
                .unwrap()
                .rows
                .is_empty()
        );
    }
    a.execute_batch("ROLLBACK").unwrap();
    drop(b);
    drop(a);
    std::fs::remove_file(path).unwrap();
}
