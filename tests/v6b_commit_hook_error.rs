use sqliters::{Connection, StepResult};

#[test]
fn commit_hook_rejection_uses_c_error_text() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        for explain in [false, true] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(&format!("PRAGMA encoding='{encoding}'; CREATE TABLE t(a);"))
                .unwrap();
            let mut listing =
                explain.then(|| db.prepare("EXPLAIN INSERT INTO t VALUES(99)").unwrap());
            if let Some(stmt) = &mut listing {
                while let StepResult::Row(_) = stmt.step().unwrap() {}
            }
            db.commit_hook(Some(Box::new(|| true))).unwrap();
            let result = db.execute_batch("INSERT INTO t VALUES(1);");
            let error = if let Some(mut stmt) = listing {
                result.unwrap();
                stmt.reset().unwrap_err()
            } else {
                result.unwrap_err()
            };
            assert_eq!(error.extended_code, 531);
            assert_eq!(error.message, "constraint failed");
            assert!(db.is_autocommit());
            assert_eq!(db.changes(), 0);
            assert_eq!(db.total_changes(), u64::from(explain));
            db.commit_hook(None).unwrap();
            assert!(db.query("SELECT * FROM t", &[]).unwrap().rows.is_empty());
        }
    }
}
