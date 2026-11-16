//! SQLite 3.54.1 encoding changes, schema initialization and advisory expiry.
use sqliters::{Connection, StepResult, Value};

fn encoding(c: &Connection) -> String {
    let result = c.query("PRAGMA encoding", &[]).unwrap();
    let Value::Text(value) = &result.rows[0].values()[0] else { panic!("{result:?}") };
    value.clone()
}

#[test]
fn aliases_apply_on_prepare_and_invalid_words_error_until_fixed() {
    let c = Connection::open_in_memory().unwrap();
    for (value, expected) in [("UTF8","UTF-8"),("UTF-8","UTF-8"),
        ("UTF16le","UTF-16le"),("UTF-16le","UTF-16le"),
        ("uTf16bE","UTF-16be"),("UTF-16be","UTF-16be")] {
        c.prepare(&format!("PRAGMA encoding='{value}'")).unwrap();
        assert_eq!(encoding(&c),expected);
    }
    for value in ["UTF16", "UTF-16"] {
        c.prepare(&format!("PRAGMA encoding='{value}'")).unwrap();
        assert_eq!(encoding(&c),if cfg!(target_endian="little") {"UTF-16le"} else {"UTF-16be"});
    }
    for value in [" UTF8", "UTF8 ", "unknown", ""] {
        let error = c.prepare(&format!("PRAGMA encoding='{value}'")).err().unwrap();
        assert_eq!(error.message,format!("unsupported encoding: {value}"));
    }
    c.execute_batch("CREATE TABLE t(a)").unwrap();
    let fixed=encoding(&c);
    c.prepare("PRAGMA encoding='unknown'").unwrap();
    c.prepare("PRAGMA encoding='UTF8'").unwrap();
    assert_eq!(encoding(&c),fixed);
}

#[test]
fn pending_getter_and_setter_are_reprepared_after_encoding_changes() {
    let c = Connection::open_in_memory().unwrap();
    let mut getter=c.prepare("PRAGMA encoding").unwrap();
    let mut little=c.prepare("PRAGMA encoding='UTF-16le'").unwrap();
    assert_eq!(encoding(&c),"UTF-16le");
    c.prepare("PRAGMA encoding='UTF-16be'").unwrap();
    assert!(matches!(getter.step().unwrap(),StepResult::Row(ref r) if r.values()[0]==Value::Text("UTF-16be".into())));
    assert!(matches!(getter.step().unwrap(),StepResult::Done));
    assert!(matches!(little.step().unwrap(),StepResult::Done));
    assert_eq!(encoding(&c),"UTF-16le");
    c.prepare("EXPLAIN PRAGMA encoding='UTF8'").unwrap();
    assert_eq!(encoding(&c),"UTF-8");
}

#[test]
fn same_encoding_assignment_still_expires_pending_statements() {
    let c=Connection::open_in_memory().unwrap();
    let mut old=c.prepare("PRAGMA busy_timeout=17").unwrap();
    c.prepare("PRAGMA busy_timeout=31").unwrap();
    c.prepare("PRAGMA encoding='UTF8'").unwrap();
    assert!(matches!(old.step().unwrap(),StepResult::Row(_)));
    assert!(matches!(old.step().unwrap(),StepResult::Done));
    assert_eq!(c.query("PRAGMA busy_timeout",&[]).unwrap().rows[0].values()[0],Value::Integer(17));
}

#[test]
fn transaction_and_late_error_keep_prepare_effects() {
    let c=Connection::open_in_memory().unwrap();
    c.execute_batch("BEGIN").unwrap();
    c.prepare("PRAGMA encoding='UTF-16be'").unwrap();
    assert_eq!(encoding(&c),"UTF-16be");
    assert_eq!(c.prepare("PRAGMA encoding='unknown'").err().unwrap().message,"unsupported encoding: unknown");
    c.execute_batch("ROLLBACK").unwrap();
    assert!(c.prepare("PRAGMA encoding='UTF-16le' junk").is_err());
    assert_eq!(encoding(&c),"UTF-16le");
}

#[test]
fn active_statement_continues_after_advisory_encoding_expiry() {
    let c=Connection::open_in_memory().unwrap();
    let mut active=c.prepare("SELECT 'one' UNION ALL SELECT 'İstanbul'").unwrap();
    assert!(matches!(active.step().unwrap(),StepResult::Row(ref r) if r.values()[0]==Value::Text("one".into())));
    c.prepare("PRAGMA encoding='UTF-16be'").unwrap();
    assert!(matches!(active.step().unwrap(),StepResult::Row(ref r) if r.values()[0]==Value::Text("İstanbul".into())));
    assert!(matches!(active.step().unwrap(),StepResult::Done));
    assert_eq!(encoding(&c),"UTF-16be");
}

#[test]
fn existing_empty_page_one_uses_the_new_encoding_for_the_first_table() {
    let dir=std::env::temp_dir().join(format!("v6b-encoding-page1-{}",std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();let path=dir.join("test.db");
    let c=Connection::open(&path).unwrap();
    c.execute_batch("PRAGMA user_version=7").unwrap();
    c.prepare("PRAGMA encoding='UTF-16le'").unwrap();
    assert_eq!(encoding(&c),"UTF-16le");
    c.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES('İstanbul')").unwrap();
    assert_eq!(encoding(&c),"UTF-16le");
    c.try_close().unwrap();
    let bytes=std::fs::read(&path).unwrap();
    assert_eq!(u32::from_be_bytes(bytes[56..60].try_into().unwrap()),2);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn encoding_setter_needs_no_file_read_lock() {
    let dir=std::env::temp_dir().join(format!("v6b-encoding-lock-{}",std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();let path=dir.join("test.db");
    let a=Connection::open(&path).unwrap();a.execute_batch("CREATE TABLE t(a)").unwrap();
    let b=Connection::open(&path).unwrap();a.execute_batch("BEGIN EXCLUSIVE").unwrap();
    b.prepare("PRAGMA encoding='UTF-16be'").unwrap();
    assert_eq!(b.prepare("PRAGMA encoding").err().unwrap().code,sqliters::ErrorCode::Busy);
    a.execute_batch("ROLLBACK").unwrap();
    assert_eq!(encoding(&b),"UTF-8");
    b.try_close().unwrap();a.try_close().unwrap();std::fs::remove_dir_all(dir).unwrap();
}
