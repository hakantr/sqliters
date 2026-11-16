//! prepare.c initializes main, reverse attached databases, then TEMP.
use sqliters::{Connection, ErrorCode, Value};

fn setup() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE m(x); ATTACH ':memory:' AS a; CREATE TABLE a.bad_a(x); ATTACH ':memory:' AS b; CREATE TABLE b.bad_b(x); CREATE TEMP TABLE bad_temp(x); PRAGMA writable_schema=ON;").unwrap();
    c
}

fn poison(c: &Connection, schema: &str, name: &str) {
    c.execute_batch(&format!(
        "UPDATE {schema}.sqlite_schema SET sql='broken' WHERE name='{name}'"
    ))
    .unwrap();
}

fn schema_error(c: &Connection) -> String {
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    let error = c.prepare("SELECT * FROM m").err().unwrap();
    assert_eq!(error.code, ErrorCode::Corrupt);
    error.message
}

#[test]
fn main_schema_error_precedes_every_other_schema() {
    let c = setup();
    poison(&c, "main", "m");
    poison(&c, "a", "bad_a");
    poison(&c, "b", "bad_b");
    poison(&c, "temp", "bad_temp");
    assert_eq!(schema_error(&c), "malformed database schema (m)");
}

#[test]
fn newest_attached_schema_error_precedes_older_and_temp() {
    let c = setup();
    poison(&c, "a", "bad_a");
    poison(&c, "b", "bad_b");
    poison(&c, "temp", "bad_temp");
    assert_eq!(schema_error(&c), "malformed database schema (bad_b)");
}

#[test]
fn temp_schema_is_checked_after_valid_attached_schemas() {
    let c = setup();
    poison(&c, "temp", "bad_temp");
    assert_eq!(schema_error(&c), "malformed database schema (bad_temp)");
}

#[test]
fn open_transaction_schema_refresh_uses_the_same_initialization_order() {
    for begin in ["BEGIN", "BEGIN IMMEDIATE", "SAVEPOINT s"] {
        for corrupt_main in [false, true] {
            let c = setup();
            c.execute_batch(begin).unwrap();
            if corrupt_main {
                poison(&c, "main", "m");
            }
            poison(&c, "a", "bad_a");
            poison(&c, "b", "bad_b");
            poison(&c, "temp", "bad_temp");
            let name = if corrupt_main { "m" } else { "bad_b" };
            assert_eq!(
                schema_error(&c),
                format!("malformed database schema ({name})"),
                "{begin}"
            );
        }
    }
}

#[test]
fn lookup_order_stays_temp_main_then_older_attached_database() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("ATTACH ':memory:' AS a; ATTACH ':memory:' AS b; CREATE TABLE a.u(x); CREATE TABLE b.u(x); INSERT INTO a.u VALUES(11); INSERT INTO b.u VALUES(23);").unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    assert_eq!(
        c.query("SELECT * FROM u", &[]).unwrap().rows[0].values()[0],
        Value::Integer(11)
    );
    c.execute_batch("CREATE TABLE main.u(x); INSERT INTO main.u VALUES(33);")
        .unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    assert_eq!(
        c.query("SELECT * FROM u", &[]).unwrap().rows[0].values()[0],
        Value::Integer(33)
    );
    c.execute_batch("CREATE TEMP TABLE u(x); INSERT INTO temp.u VALUES(44);")
        .unwrap();
    c.prepare("PRAGMA writable_schema=RESET").unwrap();
    assert_eq!(
        c.query("SELECT * FROM u", &[]).unwrap().rows[0].values()[0],
        Value::Integer(44)
    );
}
