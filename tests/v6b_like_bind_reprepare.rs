use sqliters::{Connection, Statement, StepResult, Value};

fn rows(stmt: &mut Statement) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().unwrap() {
        out.push(row.values().to_vec());
    }
    out
}

#[test]
fn masked_bind_apis_null_and_active_clear_preserve_c_lifetime() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        for sensitive in [true, false] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(&format!("PRAGMA encoding='{enc}'; PRAGMA case_sensitive_like={}; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'),('abc'),('A'),('ABC');", i32::from(sensitive))).unwrap();
            let mut q = db
                .prepare("SELECT a FROM t WHERE a LIKE :pat ORDER BY a COLLATE BINARY")
                .unwrap();
            q.bind_text(1, "a%").unwrap();
            let expected = if sensitive { 2 } else { 4 };
            assert_eq!(rows(&mut q).len(), expected);
            assert_eq!(q.machine_stmt_status(5, false), 1);
            assert_eq!(q.machine_stmt_status(6, false), 1);
            q.reset().unwrap();
            assert_eq!(rows(&mut q).len(), expected);
            assert_eq!(q.machine_stmt_status(5, false), 1);
            assert_eq!(q.machine_stmt_status(6, false), 2);
            q.reset().unwrap();
            q.bind_all(&[Value::Null]).unwrap();
            assert!(rows(&mut q).is_empty());
            assert_eq!(q.machine_stmt_status(5, false), 2);
            q.reset().unwrap();
            q.bind_named(":pat", "a%").unwrap();
            assert!(matches!(q.step().unwrap(), StepResult::Row(_)));
            q.clear_bindings().unwrap();
            assert_eq!(rows(&mut q).len(), expected - 1);
            assert_eq!(q.machine_stmt_status(5, false), 3);
            q.reset().unwrap();
            assert!(rows(&mut q).is_empty());
            assert_eq!(q.machine_stmt_status(5, false), 4);
            q.reset().unwrap();
            q.clear_bindings().unwrap();
            assert!(rows(&mut q).is_empty());
            assert_eq!(q.machine_stmt_status(5, false), 5);
        }
    }
}

#[test]
fn bound_explain_uses_prefix_and_v1_does_not_auto_reprepare() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; PRAGMA case_sensitive_like=ON; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'),('abc'),('A'); CREATE INDEX bx ON t(a);")).unwrap();
        let mut plan = db
            .prepare("EXPLAIN QUERY PLAN SELECT a FROM t WHERE a LIKE ?")
            .unwrap();
        plan.bind(1, "a%").unwrap();
        let detail = rows(&mut plan);
        assert!(
            matches!(&detail[0][3], Value::Text(s) if s.contains("SEARCH t USING COVERING INDEX bx"))
        );
        assert_eq!(plan.machine_stmt_status(5, false), 1);
        plan.reset().unwrap();
        plan.bind(1, Value::Null).unwrap();
        let detail = rows(&mut plan);
        assert!(matches!(&detail[0][3], Value::Text(s) if s == "SCAN t"));
        assert_eq!(plan.machine_stmt_status(5, false), 2);
        let mut old = db.prepare_v1("SELECT a FROM t WHERE a LIKE ?").unwrap();
        old.bind(1, "a%").unwrap();
        assert_eq!(rows(&mut old).len(), 2);
        assert_eq!(old.machine_stmt_status(5, false), 0);
        old.reset().unwrap();
        old.bind(1, Value::Null).unwrap();
        assert!(rows(&mut old).is_empty());
        assert_eq!(old.machine_stmt_status(5, false), 0);
    }
}
