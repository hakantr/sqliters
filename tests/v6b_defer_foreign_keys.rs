use sqliters::{Connection, StepResult};

fn flag(db: &Connection) -> i64 {
    let rows = db.query("PRAGMA defer_foreign_keys", &[]).unwrap().rows;
    rows[0].get_i64(0).unwrap()
}

#[test]
fn deferred_flag_prepare_schema_load_and_transaction_lifetime() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}';"))
            .unwrap();
        let set = db.prepare("PRAGMA defer_foreign_keys=ON").unwrap();
        assert_eq!(flag(&db), 1);
        drop(set);
        assert_eq!(flag(&db), 1);
        // C's first schema initialization runs a reader while SELECT is prepared.
        let first = db.prepare("SELECT 1").unwrap();
        assert_eq!(flag(&db), 0);
        drop(first);
        db.execute_batch("PRAGMA defer_foreign_keys=ON;").unwrap();
        assert_eq!(db.query("SELECT 1", &[]).unwrap().rows.len(), 1);
        assert_eq!(flag(&db), 1);
        db.execute_batch("ROLLBACK;").unwrap_err();
        assert_eq!(flag(&db), 0);

        db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE p(a PRIMARY KEY); CREATE TABLE c(b REFERENCES p(a)); BEGIN; PRAGMA defer_foreign_keys=ON; INSERT INTO c VALUES(1);").unwrap();
        assert!(db.execute_batch("COMMIT;").is_err());
        assert!(!db.is_autocommit());
        assert_eq!(flag(&db), 1);
        db.execute_batch("INSERT INTO p VALUES(1); COMMIT;")
            .unwrap();
        assert_eq!(flag(&db), 0);
        db.execute_batch("PRAGMA defer_foreign_keys=ON;").unwrap();
        let mut read = db.prepare("SELECT b FROM c").unwrap();
        assert!(matches!(read.step().unwrap(), StepResult::Row(_)));
        assert_eq!(flag(&db), 1);
        read.reset().unwrap();
        assert_eq!(flag(&db), 0);
        db.execute_batch("CREATE TABLE pr(a PRIMARY KEY); CREATE TABLE cr(b REFERENCES pr(a) ON DELETE RESTRICT); INSERT INTO pr VALUES(1); INSERT INTO cr VALUES(1); BEGIN; PRAGMA defer_foreign_keys=ON; DELETE FROM pr; INSERT INTO pr VALUES(1); COMMIT;").unwrap();
        assert_eq!(flag(&db), 0);
        db.execute_batch("BEGIN;").unwrap();
        let mut old = db.prepare("INSERT INTO c VALUES(2)").unwrap();
        drop(db.prepare("PRAGMA defer_foreign_keys=ON").unwrap());
        assert!(old.step().is_err());
        assert_eq!(flag(&db), 1);
        drop(old);
        db.execute_batch("ROLLBACK; BEGIN; PRAGMA defer_foreign_keys=ON;")
            .unwrap();
        let mut old = db.prepare("INSERT INTO c VALUES(2)").unwrap();
        drop(db.prepare("PRAGMA defer_foreign_keys=OFF").unwrap());
        assert!(old.step().is_err());
        assert_eq!(db.last_insert_rowid(), 2);
        drop(old);
        db.execute_batch("ROLLBACK;").unwrap();
    }
}
