use sqliters::{Connection, ErrorCode, StepResult, Value};

#[test]
fn missing_order_collation_is_prepare_error_and_expired_statements_recheck_it() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}';")).unwrap();
        // C does not resolve an unused result's COLLATE just to return the value.
        let mut unused = db.prepare("SELECT 1 COLLATE missing").unwrap();
        assert!(matches!(unused.step().unwrap(), StepResult::Row(_)));
        drop(unused);
        let mut pruned = db.prepare("SELECT 1 WHERE 0 AND 'a'='b' COLLATE ghost").unwrap();
        assert!(matches!(pruned.step().unwrap(), StepResult::Done));
        drop(pruned);
        let mut pruned = db.prepare("SELECT 1 WHERE 1 OR (?1='a' COLLATE ghost)").unwrap();
        pruned.bind(1, Value::Text("a".to_owned())).unwrap();
        assert!(matches!(pruned.step().unwrap(), StepResult::Row(_)));
        drop(pruned);
        let err = match db.prepare("SELECT 1 ORDER BY 1 COLLATE missing") {
            Ok(_) => panic!("C rejects this statement during prepare"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::Error);
        assert_eq!(err.extended_code, 257);
        assert_eq!(err.message, "no such collation sequence: missing");
        assert_eq!(err.c_offset(), -1);
        db.register_collation("missing", str::cmp).unwrap();
        let mut prepared = db.prepare("SELECT 1 ORDER BY 1 COLLATE missing").unwrap();
        db.remove_collation("missing").unwrap();
        let err = prepared.step().unwrap_err();
        assert_eq!(err.extended_code, 257);
        assert_eq!(err.message, "no such collation sequence: missing");
    }
}
