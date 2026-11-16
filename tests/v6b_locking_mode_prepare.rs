//! SQLite 3.54.1 locking mode changes and results belong to prepare.
use sqliters::{Connection, StepResult, Value};

fn text(conn: &Connection, sql: &str) -> String {
    let out = conn.query(sql, &[]).unwrap();
    let Value::Text(s) = &out.rows[0].values()[0] else {
        panic!("{out:?}")
    };
    s.clone()
}

#[test]
fn memory_pager_stays_exclusive_while_connection_default_changes() {
    let conn = Connection::open_in_memory().unwrap();
    assert_eq!(text(&conn, "PRAGMA locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA main.locking_mode"), "exclusive");
    let mut setter = conn.prepare("PRAGMA locking_mode=EXCLUSIVE").unwrap();
    assert_eq!(text(&conn, "PRAGMA locking_mode"), "exclusive");
    conn.prepare("PRAGMA locking_mode=NORMAL").unwrap();
    assert!(
        matches!(setter.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Text("exclusive".into()))
    );
    assert!(matches!(setter.step().unwrap(), StepResult::Done));
    assert_eq!(text(&conn, "PRAGMA locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA main.locking_mode=NORMAL"), "exclusive");
    assert_eq!(text(&conn, "PRAGMA temp.locking_mode=NORMAL"), "exclusive");
    assert_eq!(text(&conn, "SELECT * FROM pragma_locking_mode"), "normal");
    let qualified = conn
        .query(
            "SELECT locking_mode,schema FROM pragma_locking_mode('main')",
            &[],
        )
        .unwrap();
    assert_eq!(
        qualified.rows[0].values(),
        &[Value::Text("exclusive".into()), Value::Null]
    );
    let unqualified = conn
        .query(
            "SELECT locking_mode,schema FROM pragma_locking_mode(NULL)",
            &[],
        )
        .unwrap();
    assert_eq!(
        unqualified.rows[0].values(),
        &[Value::Text("normal".into()), Value::Null]
    );
}

#[test]
fn disk_modes_apply_at_prepare_and_results_keep_their_snapshot() {
    let dir = std::env::temp_dir().join(format!("v6b-locking-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = Connection::open(dir.join("test.db")).unwrap();
    conn.execute_batch(&format!(
        "ATTACH '{}' AS aux; ATTACH ':memory:' AS mem; CREATE TABLE t(a);",
        dir.join("aux.db").display()
    ))
    .unwrap();
    let mut main = conn.prepare("PRAGMA main.locking_mode=EXCLUSIVE").unwrap();
    assert_eq!(text(&conn, "PRAGMA main.locking_mode"), "exclusive");
    assert_eq!(text(&conn, "PRAGMA locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA locking_mode='invalid'"), "normal");
    assert_eq!(text(&conn, "PRAGMA locking_mode=' exclusive'"), "normal");
    conn.prepare("PRAGMA locking_mode=EXCLUSIVE").unwrap();
    conn.prepare("PRAGMA aux.locking_mode=NORMAL").unwrap();
    assert_eq!(text(&conn, "PRAGMA aux.locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA main.locking_mode"), "exclusive");
    let mut snapshot = conn.prepare("PRAGMA locking_mode").unwrap();
    conn.prepare("EXPLAIN PRAGMA locking_mode=NORMAL").unwrap();
    assert!(
        matches!(snapshot.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Text("exclusive".into()))
    );
    assert!(matches!(snapshot.step().unwrap(), StepResult::Done));
    assert!(
        matches!(main.step().unwrap(), StepResult::Row(ref r) if r.values()[0] == Value::Text("exclusive".into()))
    );
    assert!(matches!(main.step().unwrap(), StepResult::Done));
    assert_eq!(text(&conn, "PRAGMA main.locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA aux.locking_mode"), "normal");
    assert_eq!(text(&conn, "PRAGMA mem.locking_mode"), "exclusive");
    assert_eq!(text(&conn, "SELECT * FROM pragma_locking_mode"), "normal");
    main.finalize().unwrap();
    snapshot.finalize().unwrap();
    conn.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn normal_preparation_releases_retained_lock_only_after_next_read_finishes() {
    let dir = std::env::temp_dir().join(format!("v6b-locking-retained-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.db");
    let a = Connection::open(&path).unwrap();
    a.execute("CREATE TABLE t(a)").unwrap();
    let b = Connection::open(&path).unwrap();
    b.query("SELECT * FROM t", &[]).unwrap();
    let exclusive = a.prepare("PRAGMA locking_mode=EXCLUSIVE").unwrap();
    b.execute("INSERT INTO t VALUES(1)").unwrap();
    a.execute("INSERT INTO t VALUES(2)").unwrap();
    assert_eq!(
        b.query("SELECT * FROM t", &[]).unwrap_err().code,
        sqliters::ErrorCode::Busy
    );
    let normal = a.prepare("PRAGMA locking_mode=NORMAL").unwrap();
    assert_eq!(
        b.query("SELECT * FROM t", &[]).unwrap_err().code,
        sqliters::ErrorCode::Busy
    );
    a.query("SELECT * FROM t", &[]).unwrap();
    b.query("SELECT * FROM t", &[]).unwrap();
    normal.finalize().unwrap();
    exclusive.finalize().unwrap();
    b.try_close().unwrap();
    a.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn private_temp_and_immutable_pagers_keep_exclusive_mode() {
    let temp = Connection::open_private_temporary().unwrap();
    assert_eq!(text(&temp, "PRAGMA main.locking_mode=NORMAL"), "exclusive");
    assert_eq!(text(&temp, "PRAGMA locking_mode"), "normal");
    temp.execute_batch("PRAGMA temp_store=MEMORY;").unwrap();
    assert_eq!(text(&temp, "PRAGMA temp.locking_mode=NORMAL"), "exclusive");
    temp.try_close().unwrap();

    let dir = std::env::temp_dir().join(format!("v6b-locking-immutable-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.db");
    let writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES(17);")
        .unwrap();
    writer.try_close().unwrap();
    let before = std::fs::read(&path).unwrap();
    let reader = Connection::open_immutable(&path).unwrap();
    assert_eq!(text(&reader, "PRAGMA locking_mode"), "normal");
    assert_eq!(
        text(&reader, "PRAGMA main.locking_mode=NORMAL"),
        "exclusive"
    );
    reader.query("SELECT * FROM t", &[]).unwrap();
    reader.try_close().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn header_change_count_follows_retained_locks_and_rollback_lifecycle() {
    let dir = std::env::temp_dir().join(format!("v6b-locking-counter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.db");
    let conn = Connection::open(&path).unwrap();
    let cases = [
        ("CREATE TABLE t(a)", 1),
        ("PRAGMA locking_mode=EXCLUSIVE; INSERT INTO t VALUES(1)", 2),
        ("INSERT INTO t VALUES(2)", 2),
        ("BEGIN IMMEDIATE; ROLLBACK; INSERT INTO t VALUES(3)", 2),
        (
            "BEGIN; INSERT INTO t VALUES(4); ROLLBACK; INSERT INTO t VALUES(5)",
            3,
        ),
        (
            "BEGIN IMMEDIATE; SAVEPOINT s; UPDATE t SET a=a; ROLLBACK TO s; RELEASE s; COMMIT",
            3,
        ),
        ("INSERT INTO t VALUES(6)", 4),
        ("PRAGMA locking_mode=NORMAL", 4),
        ("SELECT * FROM t", 4),
        ("INSERT INTO t VALUES(7)", 5),
        ("PRAGMA locking_mode=EXCLUSIVE; INSERT INTO t VALUES(8)", 6),
        ("VACUUM; INSERT INTO t VALUES(9)", 7),
        (
            "PRAGMA locking_mode=NORMAL; SELECT * FROM t; INSERT INTO t VALUES(10)",
            8,
        ),
    ];
    for (sql, expected) in cases {
        conn.execute_batch(sql).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            u32::from_be_bytes(bytes[24..28].try_into().unwrap()),
            expected,
            "{sql}"
        );
        assert_eq!(
            u32::from_be_bytes(bytes[92..96].try_into().unwrap()),
            expected,
            "{sql}"
        );
    }
    conn.try_close().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pragma_syntax_error_does_not_open_temp() {
    let conn = Connection::open_in_memory().unwrap();
    for value in ["NULL", "SELECT", "NOTHING", "+'exclusive'", "-OFF"] {
        assert!(
            conn.prepare(&format!("PRAGMA temp.locking_mode={value}"))
                .is_err()
        );
        assert!(
            conn.prepare("EXPLAIN PRAGMA temp.locking_mode").is_err(),
            "{value}"
        );
    }
    for value in ["CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "WITH"] {
        let valid = Connection::open_in_memory().unwrap();
        valid
            .prepare(&format!("PRAGMA temp.locking_mode={value}"))
            .unwrap();
        valid.prepare("EXPLAIN PRAGMA temp.locking_mode").unwrap();
    }
    assert_eq!(text(&conn, "PRAGMA temp.locking_mode"), "exclusive");
}

#[test]
fn error_without_message_keeps_the_previous_prepare_error_offset() {
    let conn = Connection::open_in_memory().unwrap();
    let sql = "PRAGMA temp.locking_mode=NULL";
    let syntax = conn.prepare(sql).err().unwrap();
    assert_eq!(syntax.offset, sql.find("NULL"));
    let generic = conn
        .prepare("EXPLAIN PRAGMA temp.locking_mode")
        .err()
        .unwrap();
    assert_eq!(generic.message, "SQL logic error");
    assert_eq!(generic.offset, syntax.offset);
    conn.prepare("SELECT 1").unwrap();
    let generic = conn
        .prepare("EXPLAIN PRAGMA temp.locking_mode")
        .err()
        .unwrap();
    assert_eq!(generic.c_offset(), -1);
}

#[test]
fn late_syntax_error_keeps_pragma_effects_already_performed_by_c_parser() {
    let conn = Connection::open_in_memory().unwrap();
    for (name, value, expected) in [
        ("busy_timeout", "17", "17"),
        ("wal_autocheckpoint", "23", "23"),
        ("page_size", "1024", "1024"),
        ("locking_mode", "EXCLUSIVE", "exclusive"),
    ] {
        assert!(
            conn.prepare(&format!("PRAGMA {name}={value} junk"))
                .is_err()
        );
        let result = conn.query(&format!("PRAGMA {name}"), &[]).unwrap();
        assert_eq!(result.rows[0].values()[0].to_text_lossy(), expected);
    }
    assert!(conn.prepare("PRAGMA temp.cache_size=17 junk").is_err());
    conn.prepare("EXPLAIN PRAGMA temp.locking_mode").unwrap();
    let rows = conn.query("PRAGMA temp.cache_size", &[]).unwrap();
    assert_eq!(rows.rows[0].values()[0], Value::Integer(17));
}
