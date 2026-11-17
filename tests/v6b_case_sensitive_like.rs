use sqliters::{Connection, FunctionFlags, RuntimeValue, StepResult, Value};

fn scalar(db: &Connection, sql: &str) -> Value {
    db.query(sql, &[]).unwrap().rows[0].values()[0].clone()
}

#[test]
fn like_prepare_effect_binding_and_busy_builtin_flags_follow_c() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "PRAGMA encoding='{encoding}'; CREATE TABLE t(a TEXT);"
        ))
        .unwrap();
        let mut cached = db.prepare("SELECT 'a' LIKE 'A'").unwrap();
        let pragma = db.prepare("PRAGMA case_sensitive_like=ON").unwrap();
        assert_eq!(scalar(&db, "SELECT 'a' LIKE 'A'"), Value::Integer(0));
        assert!(matches!(cached.step().unwrap(), StepResult::Row(ref row)
            if row.values()[0] == Value::Integer(0)));
        cached.reset().unwrap();
        drop(pragma);
        db.prepare("EXPLAIN PRAGMA case_sensitive_like=OFF")
            .unwrap();
        assert_eq!(scalar(&db, "SELECT 'a' LIKE 'A'"), Value::Integer(1));
        assert!(matches!(cached.step().unwrap(), StepResult::Row(ref row)
            if row.values()[0] == Value::Integer(1)));
        cached.reset().unwrap();

        db.register_scalar_function("LiKe", 2, FunctionFlags::default(), |_| {
            Ok(RuntimeValue::plain(Value::Integer(7)))
        })
        .unwrap();
        assert_eq!(scalar(&db, "SELECT like('a','A')"), Value::Integer(7));
        assert_eq!(
            scalar(
                &db,
                "SELECT name FROM pragma_function_list WHERE builtin=0 AND lower(name)='like' AND narg=2"
            ),
            Value::Text("like".into())
        );
        db.prepare("PRAGMA case_sensitive_like=ON").unwrap();
        assert_eq!(scalar(&db, "SELECT like('a','A')"), Value::Integer(0));
        assert_eq!(scalar(&db, "SELECT like('_',x'80')"), Value::Integer(i64::from(encoding == "UTF-8")));
        assert_eq!(
            scalar(&db, "SELECT like('%%a','%a','%')"),
            Value::Integer(1)
        );
        assert_eq!(scalar(&db, "SELECT like('%','abc',NULL)"), Value::Null);
        let error = db
            .query("SELECT like(printf('%0*d',50001,0),NULL,'')", &[])
            .unwrap_err();
        assert_eq!(error.message, "LIKE or GLOB pattern too complex");

        let long = format!("a{}", "é".repeat(5000));
        db.execute_batch(&format!("INSERT INTO t VALUES('{long}');"))
            .unwrap();
        let row = db
            .query("SELECT a FROM t NOT INDEXED WHERE a LIKE 'a%'", &[])
            .unwrap();
        assert_eq!(row.rows[0].values()[0], Value::Text(long));
    }

    // func.c mutates the shared built-in flags even when CreateFunc returns
    // BUSY. Its matcher remains case-insensitive, while planning uses BINARY.
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'),('A'); CREATE INDEX bx ON t(a); CREATE INDEX nx ON t(a COLLATE NOCASE);").unwrap();
    let mut active = db.prepare("SELECT a FROM t").unwrap();
    assert!(matches!(active.step().unwrap(), StepResult::Row(_)));
    db.prepare("PRAGMA case_sensitive_like=ON").unwrap();
    assert_eq!(scalar(&db, "SELECT 'a' LIKE 'A'"), Value::Integer(1));
    let rows = db.query("SELECT a FROM t WHERE a LIKE 'a%'", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].values()[0], Value::Text("a".into()));
    active.reset().unwrap();
    drop(active);
    drop(db);
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'),('A'); CREATE INDEX bx ON t(a); CREATE INDEX nx ON t(a COLLATE NOCASE);").unwrap();
    assert_eq!(scalar(&db, "SELECT 'a' LIKE 'A'"), Value::Integer(1));
    assert_eq!(
        db.query("SELECT a FROM t WHERE a LIKE 'a%'", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}
