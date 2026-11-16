//! The generator snapshot uses the metadata C sees at actual prepare.
use sqliters::{Connection, StepResult, Value};

#[test]
fn snapshot_sees_fixed_encoding_after_literal_insert() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(17)").unwrap();
    c.c_generated_program("PRAGMA encoding='unknown'").unwrap();
    c.prepare("PRAGMA encoding='unknown'").unwrap();
    assert_eq!(c.query("PRAGMA encoding",&[]).unwrap().rows[0].values()[0],Value::Text("UTF-8".into()));
}

#[test]
fn snapshot_connects_fresh_pragma_virtual_tables() {
    for name in ["pragma_temp_store","pragma_encoding","pragma_locking_mode"] {
        let c=Connection::open_in_memory().unwrap();
        c.c_generated_program(&format!("SELECT * FROM {name}")).unwrap();
        assert_eq!(c.query(&format!("SELECT * FROM {name}"),&[]).unwrap().rows.len(),1);
    }
}

#[test]
fn cleared_schema_is_loaded_before_snapshot() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE t(a); CREATE TEMP TABLE q(a)").unwrap();
    c.prepare("PRAGMA temp_store=FILE").unwrap();
    let sql="SELECT * FROM pragma_temp_store";
    let before=c.c_generated_program(sql).unwrap();
    assert_eq!(c.query(sql,&[]).unwrap().rows[0].values()[0],Value::Integer(1));
    let after=c.c_generated_program(sql).unwrap();
    assert_eq!(sqliters_vdbe::progdump::dump(&before,0),sqliters_vdbe::progdump::dump(&after,0));
}

#[test]
fn snapshot_discards_top_level_prepare_effects() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TEMP TABLE q(a); INSERT INTO q VALUES(17)").unwrap();
    c.c_generated_program("PRAGMA temp_store=MEMORY").unwrap();
    assert_eq!(c.query("PRAGMA temp_store",&[]).unwrap().rows[0].values()[0],Value::Integer(0));
    assert_eq!(c.query("SELECT * FROM q",&[]).unwrap().rows[0].values()[0],Value::Integer(17));
    let empty=Connection::open_in_memory().unwrap();
    let mut old=empty.prepare("PRAGMA busy_timeout=17").unwrap();
    empty.prepare("PRAGMA busy_timeout=31").unwrap();
    empty.c_generated_program("PRAGMA encoding='UTF-16be'").unwrap();
    assert_eq!(empty.query("PRAGMA encoding",&[]).unwrap().rows[0].values()[0],Value::Text("UTF-8".into()));
    assert!(matches!(old.step().unwrap(),StepResult::Row(_)));
    assert!(matches!(old.step().unwrap(),StepResult::Done));
    assert_eq!(empty.query("PRAGMA busy_timeout",&[]).unwrap().rows[0].values()[0],Value::Integer(31));
}
