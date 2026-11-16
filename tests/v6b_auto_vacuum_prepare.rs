//! SQLite 3.54.1 live auto-vacuum flags and prepared meta-cookie writes.
use sqliters::{Connection, StepResult, Value};

fn int(conn: &Connection, sql: &str) -> i64 {
    let rows = conn.query(sql, &[]).unwrap();
    let Value::Integer(n) = rows.rows[0].values()[0] else {
        panic!("{rows:?}");
    };
    n
}

#[test]
fn auto_vacuum_freezes_query_and_cookie_but_keeps_last_global_request() {
    let conn = Connection::open_in_memory().unwrap();
    let mut incr = conn.prepare("PRAGMA auto_vacuum=2").unwrap();
    let mut snapshot = conn.prepare("PRAGMA auto_vacuum").unwrap();
    conn.prepare("PRAGMA auto_vacuum=1").unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 1);
    incr.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
    assert!(
        matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Integer(2))
    );
    assert!(matches!(snapshot.step().unwrap(), StepResult::Done));
    conn.execute("VACUUM").unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 1);
    incr.reset().unwrap();
    incr.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
}

#[test]
fn cancelled_auto_vacuum_write_still_creates_page_one() {
    let conn = Connection::open_in_memory().unwrap();
    let mut incr = conn.prepare("PRAGMA auto_vacuum=2").unwrap();
    conn.prepare("PRAGMA auto_vacuum=0").unwrap();
    assert_eq!(int(&conn, "PRAGMA page_count"), 0);
    incr.step().unwrap();
    assert_eq!(int(&conn, "PRAGMA page_count"), 1);
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 0);
}

#[test]
fn live_auto_vacuum_flag_survives_commit_and_rollback_until_next_read() {
    for end in ["COMMIT", "ROLLBACK"] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA auto_vacuum=1; CREATE TABLE t(a); BEGIN IMMEDIATE;")
            .unwrap();
        let _setter = conn.prepare("PRAGMA auto_vacuum=2").unwrap();
        conn.execute("INSERT INTO t VALUES(1)").unwrap();
        conn.execute(end).unwrap();
        assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2, "{end}");
        conn.query("SELECT * FROM t", &[]).unwrap();
        assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 1, "{end}");
    }
}

#[test]
fn savepoint_restores_cookie_and_preserves_live_auto_vacuum_flag() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA auto_vacuum=1; CREATE TABLE t(a); BEGIN IMMEDIATE; SAVEPOINT s;")
        .unwrap();
    conn.execute("PRAGMA auto_vacuum=2").unwrap();
    conn.execute("ROLLBACK TO s").unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
    conn.execute("COMMIT").unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
    conn.query("SELECT * FROM t", &[]).unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 1);
}

#[test]
fn temp_and_rejected_aux_requests_follow_c_prepare_decisions() {
    let conn = Connection::open_in_memory().unwrap();
    conn.prepare("PRAGMA temp.auto_vacuum=2").unwrap();
    assert_eq!(int(&conn, "PRAGMA temp.auto_vacuum"), 2);
    conn.execute_batch("CREATE TEMP TABLE t(a); CREATE TABLE t(a); ATTACH ':memory:' AS aux; CREATE TABLE aux.t(a);").unwrap();
    conn.prepare("PRAGMA aux.auto_vacuum=2").unwrap();
    assert_eq!(int(&conn, "PRAGMA aux.auto_vacuum"), 0);
    conn.execute("VACUUM").unwrap();
    assert_eq!(int(&conn, "PRAGMA auto_vacuum"), 2);
    assert_eq!(int(&conn, "PRAGMA aux.auto_vacuum"), 0);
}

#[test]
fn auto_vacuum_text_and_numeric_conversion_match_c() {
    let conn = Connection::open_in_memory().unwrap();
    for (value, mode) in [
        ("'2junk'", 2),
        ("' FULL '", 0),
        ("' +2'", 0),
        ("-1", 0),
        ("0x2", 2),
        ("2147483648", 0),
        ("FULL", 1),
        ("incremental", 2),
    ] {
        conn.prepare(&format!("PRAGMA auto_vacuum={value}"))
            .unwrap();
        assert_eq!(int(&conn, "PRAGMA auto_vacuum"), mode, "{value}");
    }
}

#[test]
fn inspecting_temp_auto_vacuum_program_does_not_open_or_change_temp() {
    let conn = Connection::open_in_memory().unwrap();
    conn.c_generated_program("PRAGMA temp.auto_vacuum=2")
        .unwrap();
    assert!(conn.prepare("EXPLAIN PRAGMA temp.auto_vacuum").is_err());
    assert_eq!(int(&conn, "PRAGMA temp.auto_vacuum"), 0);
}

#[test]
fn rollback_to_empty_savepoint_recreates_page_one_with_live_mode() {
    for mode in [0, 1, 2] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("BEGIN; SAVEPOINT s;").unwrap();
        let sql = if mode == 0 {
            "PRAGMA default_cache_size=17".to_owned()
        } else {
            format!("PRAGMA auto_vacuum={mode}")
        };
        conn.execute(&sql).unwrap();
        conn.execute("ROLLBACK TO s").unwrap();
        assert_eq!(int(&conn, "PRAGMA page_count"), 1);
        assert_eq!(int(&conn, "PRAGMA auto_vacuum"), mode);
        conn.execute_batch("RELEASE s; COMMIT;").unwrap();
        assert_eq!(int(&conn, "PRAGMA page_count"), 1);
        assert_eq!(int(&conn, "PRAGMA auto_vacuum"), mode);
    }
}
