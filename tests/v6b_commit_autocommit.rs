use sqliters::Connection;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[test]
fn commit_callback_and_failed_release_keep_c_transaction_state() {
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        for savepoint in [false, true] {
            for reject in [false, true] {
                let db = Arc::new(Connection::open_in_memory().unwrap());
                db.execute_batch(&format!("PRAGMA encoding='{encoding}'; CREATE TABLE t(a);"))
                    .unwrap();
                db.execute_batch(if savepoint { "SAVEPOINT s;" } else { "BEGIN;" })
                    .unwrap();
                db.execute_batch("INSERT INTO t VALUES(1);").unwrap();
                let auto = Arc::new(AtomicBool::new(false));
                let observed = Arc::clone(&auto);
                let weak = Arc::downgrade(&db);
                db.commit_hook(Some(Box::new(move || {
                    observed.store(weak.upgrade().unwrap().is_autocommit(), Ordering::SeqCst);
                    reject
                })))
                .unwrap();
                let result = db.execute_batch(if savepoint { "RELEASE s;" } else { "COMMIT;" });
                assert!(auto.load(Ordering::SeqCst));
                if reject {
                    let error = result.unwrap_err();
                    assert_eq!(error.extended_code, 531);
                    assert_eq!(error.message, "constraint failed");
                } else {
                    result.unwrap();
                }
                assert_eq!(db.is_autocommit(), !(savepoint && reject));
                db.commit_hook(None).unwrap();
                assert_eq!(
                    db.query("SELECT a FROM t", &[]).unwrap().rows.len(),
                    usize::from(!reject)
                );
                if savepoint && reject {
                    db.execute_batch("ROLLBACK TO s; RELEASE s;").unwrap();
                    assert!(db.is_autocommit());
                }
            }
        }
    }
}
