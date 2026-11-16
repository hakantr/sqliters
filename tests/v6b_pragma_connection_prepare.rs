//! SQLite 3.54.1 connection/cache settings that take effect during prepare.
use sqliters::{Connection, ErrorCode, StepResult, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn int(conn: &Connection, sql: &str) -> i64 {
    let rows = conn.query(sql, &[]).unwrap();
    let Value::Integer(n) = rows.rows[0].values()[0] else {
        panic!("{rows:?}");
    };
    n
}

#[test]
fn timeout_and_autocheckpoint_setters_keep_prepared_results() {
    let conn = Connection::open_in_memory().unwrap();
    for name in ["busy_timeout", "wal_autocheckpoint"] {
        let mut setter = conn.prepare(&format!("PRAGMA {name}=17")).unwrap();
        let mut snapshot = conn.prepare(&format!("PRAGMA {name}")).unwrap();
        conn.prepare(&format!("PRAGMA {name}=23")).unwrap();
        assert!(
            matches!(setter.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(17))
        );
        assert!(matches!(setter.step().unwrap(), StepResult::Done));
        assert_eq!(int(&conn, &format!("PRAGMA {name}")), 23);
        assert!(
            matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(17))
        );
        assert!(matches!(snapshot.step().unwrap(), StepResult::Done));
        conn.prepare(&format!("EXPLAIN PRAGMA {name}=31")).unwrap();
        assert_eq!(int(&conn, &format!("PRAGMA {name}")), 31);
        conn.prepare(&format!("PRAGMA {name}='invalid'")).unwrap();
        assert_eq!(
            int(&conn, &format!("PRAGMA {name}")),
            if name == "busy_timeout" { 31 } else { 0 }
        );
    }
}

#[test]
fn spill_threshold_is_local_enable_flag_is_global_and_step_does_not_reapply() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("ATTACH ':memory:' AS aux; PRAGMA cache_size=2; PRAGMA aux.cache_size=3;")
        .unwrap();
    let mut main = conn.prepare("PRAGMA cache_spill=5").unwrap();
    conn.prepare("PRAGMA aux.cache_spill=7").unwrap();
    assert_eq!(int(&conn, "PRAGMA cache_spill"), 5);
    assert_eq!(int(&conn, "PRAGMA aux.cache_spill"), 7);
    conn.prepare("PRAGMA aux.cache_spill=0").unwrap();
    assert!(matches!(main.step().unwrap(), StepResult::Done));
    assert_eq!(int(&conn, "PRAGMA cache_spill"), 0);
    assert_eq!(int(&conn, "PRAGMA aux.cache_spill"), 0);
    conn.prepare("PRAGMA cache_spill='full'").unwrap();
    assert_eq!(int(&conn, "PRAGMA cache_spill"), 5);
    assert_eq!(int(&conn, "PRAGMA aux.cache_spill"), 7);
    conn.prepare("PRAGMA aux.cache_spill=-1").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.cache_spill"), 3);
}

#[test]
fn spill_query_does_not_initialize_schema_cache_from_persistent_cookie() {
    let dir = std::env::temp_dir().join(format!("v6b-spill-cookie-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.db");
    let a = Connection::open(&path).unwrap();
    a.execute_batch("PRAGMA default_cache_size=17; CREATE TABLE t(a);")
        .unwrap();
    a.try_close().unwrap();
    let b = Connection::open(&path).unwrap();
    let mut snapshot = b.prepare("PRAGMA cache_spill").unwrap();
    let mut pending = b.prepare("PRAGMA cache_spill").unwrap();
    let mut timeout = b.prepare("PRAGMA busy_timeout=17").unwrap();
    b.prepare("PRAGMA busy_timeout=23").unwrap();
    assert!(
        matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(483))
    );
    assert_eq!(int(&b, "PRAGMA cache_size"), 17);
    // Loading main's recorded encoding expires pending programs (including
    // connection-only pragmas), so they reprepare before their first step.
    assert!(
        matches!(pending.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(17))
    );
    assert!(
        matches!(timeout.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(17))
    );
    assert_eq!(int(&b, "PRAGMA busy_timeout"), 17);
    assert!(matches!(snapshot.step().unwrap(), StepResult::Done));
    assert_eq!(int(&b, "PRAGMA cache_spill"), 17);
    snapshot.finalize().unwrap();
    pending.finalize().unwrap();
    timeout.finalize().unwrap();
    b.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn temp_uses_default_then_changes_geometry_and_data_version() {
    let conn = Connection::open_in_memory().unwrap();
    conn.prepare("PRAGMA page_size=1024").unwrap();
    conn.c_generated_program("PRAGMA temp.cache_spill").unwrap();
    assert!(conn.prepare("EXPLAIN PRAGMA temp.cache_spill").is_err());
    assert_eq!(int(&conn, "PRAGMA temp.data_version"), 2);
    assert_eq!(int(&conn, "PRAGMA temp.cache_spill"), 1765);
    conn.prepare("PRAGMA temp.page_size=512").unwrap();
    assert_eq!(int(&conn, "PRAGMA temp.data_version"), 3);
    assert_eq!(int(&conn, "PRAGMA temp.cache_spill"), 3160);
}

#[test]
fn invalid_timeout_setter_preserves_custom_busy_handler() {
    let dir = std::env::temp_dir().join(format!("v6b-busy-callback-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.db");
    let a = Connection::open(&path).unwrap();
    a.execute_batch("CREATE TABLE t(a); BEGIN EXCLUSIVE;")
        .unwrap();
    let b = Connection::open(&path).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    b.busy_handler(Some(Box::new(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
        false
    })))
    .unwrap();
    b.prepare("PRAGMA busy_timeout='invalid'").unwrap();
    assert_eq!(int(&b, "PRAGMA busy_timeout"), 0);
    let error = b.query("SELECT * FROM t", &[]).unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    b.try_close().unwrap();
    a.execute("ROLLBACK").unwrap();
    a.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn explain_spill_listing_does_not_read_btree_or_invoke_busy_handler() {
    let dir = std::env::temp_dir().join(format!("v6b-explain-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.db");
    let a = Connection::open(&path).unwrap();
    a.execute_batch("CREATE TABLE t(a); BEGIN EXCLUSIVE;")
        .unwrap();
    let b = Connection::open(&path).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    b.busy_handler(Some(Box::new(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
        false
    })))
    .unwrap();
    b.query("EXPLAIN PRAGMA cache_spill", &[]).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    b.try_close().unwrap();
    a.execute("ROLLBACK").unwrap();
    a.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();

    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA auto_vacuum=1; CREATE TABLE t(a); BEGIN IMMEDIATE;")
        .unwrap();
    conn.prepare("PRAGMA auto_vacuum=2").unwrap();
    conn.execute("COMMIT").unwrap();
    conn.query("EXPLAIN PRAGMA cache_spill", &[]).unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
}
