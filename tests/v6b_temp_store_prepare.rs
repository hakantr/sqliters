//! SQLite 3.54.1 temp_store preparation and TEMP invalidation.
use sqliters::{Connection, StepResult, Value};

fn integer(c: &Connection, sql: &str) -> i64 {
    let rows = c.query(sql, &[]).unwrap();
    let Value::Integer(value) = rows.rows[0].values()[0] else { panic!("{rows:?}") };
    value
}

#[test]
fn temp_store_uses_the_first_numeric_character_and_exact_words() {
    let c = Connection::open_in_memory().unwrap();
    for (value, expected) in [("20",2),("'2junk'",2),("10",1),("'1junk'",1),
        ("2.5",2),("' file'",0),("'memory '",0),("-2",0),("+2",2),("'2 '",2),
        ("0x2",0),("FILE",1),("mEmOrY",2)] {
        c.prepare(&format!("PRAGMA temp_store={value}")).unwrap();
        assert_eq!(integer(&c,"PRAGMA temp_store"),expected,"{value}");
    }
}

#[test]
fn preparing_changes_the_mode_while_existing_results_stay_frozen() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TEMP TABLE q(a)").unwrap();
    let mut getter = c.prepare("PRAGMA temp_store").unwrap();
    let mut setter = c.prepare("PRAGMA temp_store=MEMORY").unwrap();
    assert_eq!(integer(&c,"PRAGMA temp_store"),2);
    c.prepare("PRAGMA temp_store=FILE").unwrap();
    assert!(matches!(getter.step().unwrap(),StepResult::Row(ref r) if r.values()[0]==Value::Integer(0)));
    assert!(matches!(getter.step().unwrap(),StepResult::Done));
    assert!(matches!(setter.step().unwrap(),StepResult::Done));
    assert_eq!(integer(&c,"PRAGMA temp_store"),1);
    setter.reset().unwrap();
    assert!(matches!(setter.step().unwrap(),StepResult::Done));
    assert_eq!(integer(&c,"PRAGMA temp_store"),2);
}

#[test]
fn transaction_guard_depends_on_open_temp_and_unchanged_mode_is_allowed() {
    let cold = Connection::open_in_memory().unwrap();
    cold.execute_batch("BEGIN").unwrap();
    cold.prepare("PRAGMA temp_store=MEMORY").unwrap();
    assert_eq!(integer(&cold,"PRAGMA temp_store"),2);
    let error = cold.prepare("PRAGMA temp.temp_store=FILE").err().unwrap();
    assert_eq!(error.message,"temporary storage cannot be changed from within a transaction");
    assert_eq!(integer(&cold,"PRAGMA temp_store"),2);
    cold.execute_batch("ROLLBACK").unwrap();

    let open = Connection::open_in_memory().unwrap();
    open.execute_batch("CREATE TEMP TABLE q(a); BEGIN").unwrap();
    open.prepare("PRAGMA temp_store=DEFAULT").unwrap();
    assert_eq!(open.prepare("PRAGMA temp_store=MEMORY").err().unwrap().message,
        "temporary storage cannot be changed from within a transaction");
    open.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn active_temp_cursor_blocks_change_until_finished() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TEMP TABLE q(a); INSERT INTO q VALUES(1),(2)").unwrap();
    let mut reader = c.prepare("SELECT * FROM q").unwrap();
    assert!(matches!(reader.step().unwrap(),StepResult::Row(_)));
    assert_eq!(c.prepare("PRAGMA temp_store=FILE").err().unwrap().message,
        "temporary storage cannot be changed from within a transaction");
    assert_eq!(integer(&c,"PRAGMA temp_store"),0);
    assert!(matches!(reader.step().unwrap(),StepResult::Row(_)));
    assert!(matches!(reader.step().unwrap(),StepResult::Done));
    c.prepare("PRAGMA temp_store=FILE").unwrap();
    assert_eq!(integer(&c,"PRAGMA temp_store"),1);
    assert_eq!(c.prepare("SELECT * FROM q").err().unwrap().message,"no such table: q");
}

#[test]
fn explain_closes_temp_and_pending_temp_reader_is_reprepared() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TEMP TABLE q(a); INSERT INTO q VALUES(17); PRAGMA main.cache_size=17").unwrap();
    let mut pending = c.prepare("SELECT * FROM q").unwrap();
    c.prepare("EXPLAIN PRAGMA temp_store=FILE").unwrap();
    assert_eq!(integer(&c,"PRAGMA temp_store"),1);
    assert_eq!(pending.step().unwrap_err().message,"no such table: q");
    assert_eq!(integer(&c,"PRAGMA main.cache_size"),17);
}
