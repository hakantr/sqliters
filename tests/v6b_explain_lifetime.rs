use sqliters::{Connection, StepResult};

fn scalar(db: &Connection, sql: &str) -> i64 {
    db.query(sql, &[]).unwrap().rows[0].get_i64(0).unwrap()
}

#[test]
fn explain_eof_keeps_writer_until_reset_or_finalize() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        for sql in [
            "EXPLAIN INSERT INTO t VALUES(2)",
            "EXPLAIN QUERY PLAN INSERT INTO t VALUES(2)",
        ] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(&format!(
                "PRAGMA encoding='{encoding}'; CREATE TABLE t(a); INSERT INTO t VALUES(1);"
            ))
            .unwrap();
            let mut explain = db.prepare(sql).unwrap();
            assert!(!explain.is_busy());
            while let StepResult::Row(_) = explain.step().unwrap() {}
            assert!(explain.is_busy());
            db.execute_batch("PRAGMA defer_foreign_keys=ON; INSERT INTO t VALUES(3);")
                .unwrap();
            assert_eq!(scalar(&db, "PRAGMA defer_foreign_keys"), 1);
            assert!(matches!(explain.step().unwrap(), StepResult::Done));
            assert!(explain.is_busy());
            assert_eq!(scalar(&db, "SELECT changes()"), 1);
            explain.reset().unwrap();
            assert!(!explain.is_busy());
            assert_eq!(db.changes(), 0);
            assert_eq!(scalar(&db, "SELECT changes()"), 0);
            assert_eq!(scalar(&db, "SELECT total_changes()"), 2);
            assert_eq!(scalar(&db, "PRAGMA defer_foreign_keys"), 0);
            assert_eq!(scalar(&db, "SELECT sum(a) FROM t"), 4);
        }

        for finalize in [false, true] {
            let db = Connection::open_in_memory().unwrap();
            db.execute_batch(&format!(
                "PRAGMA encoding='{encoding}'; PRAGMA foreign_keys=ON;
                 CREATE TABLE p(a PRIMARY KEY); CREATE TABLE c(b REFERENCES p(a));"
            ))
            .unwrap();
            let mut explain = db.prepare("EXPLAIN INSERT INTO p VALUES(1)").unwrap();
            while let StepResult::Row(_) = explain.step().unwrap() {}
            db.execute_batch("PRAGMA defer_foreign_keys=ON; INSERT INTO c VALUES(1);")
                .unwrap();
            assert_eq!(scalar(&db, "PRAGMA defer_foreign_keys"), 1);
            let error = if finalize {
                explain.finalize().unwrap_err()
            } else {
                let error = explain.reset().unwrap_err();
                assert!(!explain.is_busy());
                error
            };
            assert_eq!(error.extended_code, 787);
            assert_eq!(error.message, "FOREIGN KEY constraint failed");
            assert_eq!(db.changes(), 0);
            assert_eq!(scalar(&db, "SELECT changes()"), 0);
            assert_eq!(scalar(&db, "SELECT total_changes()"), 1);
            assert_eq!(scalar(&db, "PRAGMA defer_foreign_keys"), 0);
            assert_eq!(scalar(&db, "SELECT count(*) FROM c"), 0);
        }
    }
}
