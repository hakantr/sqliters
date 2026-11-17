use sqliters::{Connection, Statement, StepResult, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sqliters-mmap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rows(stmt: &mut Statement) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().unwrap() {
        out.push(row.values().to_vec());
    }
    out
}

fn setting(db: &Connection, sql: &str) -> Vec<Vec<Value>> {
    rows(&mut db.prepare(sql).unwrap())
}

#[test]
fn disk_mmap_prepare_effect_and_result_snapshot_follow_c() {
    let scratch = Scratch::new();
    let db = Connection::open(scratch.0.join("main.db")).unwrap();
    let mut first = db.prepare("PRAGMA mmap_size=65536").unwrap();
    assert_eq!(
        setting(&db, "PRAGMA mmap_size"),
        vec![vec![Value::Integer(65536)]]
    );
    let mut snapshot = db.prepare("PRAGMA mmap_size").unwrap();
    db.prepare("EXPLAIN PRAGMA mmap_size=1024").unwrap();
    assert_eq!(
        setting(&db, "PRAGMA mmap_size"),
        vec![vec![Value::Integer(1024)]]
    );
    assert_eq!(rows(&mut first), vec![vec![Value::Integer(65536)]]);
    assert_eq!(rows(&mut snapshot), vec![vec![Value::Integer(65536)]]);
    assert_eq!(
        std::fs::metadata(scratch.0.join("main.db")).unwrap().len(),
        0
    );
    db.execute_batch("PRAGMA mmap_size=9223372036854775808;")
        .unwrap();
    assert_eq!(
        setting(&db, "PRAGMA mmap_size"),
        vec![vec![Value::Integer(0x7fff0000)]]
    );
    db.execute_batch("PRAGMA mmap_size=-1;").unwrap();
    assert_eq!(
        setting(&db, "PRAGMA mmap_size"),
        vec![vec![Value::Integer(0)]]
    );
}

#[test]
fn memory_and_unspilled_temp_have_no_mmap_result_row() {
    let db = Connection::open_in_memory().unwrap();
    assert!(setting(&db, "PRAGMA mmap_size").is_empty());
    assert!(setting(&db, "PRAGMA mmap_size=65536").is_empty());
    db.execute_batch("CREATE TEMP TABLE t(a); INSERT INTO t VALUES(1); BEGIN; INSERT INTO t VALUES(2); ROLLBACK;").unwrap();
    assert!(setting(&db, "PRAGMA temp.mmap_size").is_empty());
    assert_eq!(
        setting(&db, "SELECT a FROM temp.t"),
        vec![vec![Value::Integer(1)]]
    );
    assert!(setting(&db, "PRAGMA temp.mmap_size").is_empty());
    db.execute_batch("PRAGMA temp.cache_size=2; CREATE TEMP TABLE big(a); WITH RECURSIVE c(x) AS(SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<100) INSERT INTO big SELECT zeroblob(8192) FROM c;").unwrap();
    assert_eq!(
        setting(&db, "PRAGMA temp.mmap_size"),
        vec![vec![Value::Integer(0)]]
    );
    assert_eq!(
        setting(&db, "SELECT count(*) FROM temp.big"),
        vec![vec![Value::Integer(100)]]
    );
    assert_eq!(
        setting(&db, "SELECT a FROM temp.t"),
        vec![vec![Value::Integer(1)]]
    );
    db.execute_batch("BEGIN; UPDATE big SET a=zeroblob(3000) WHERE rowid=1; ROLLBACK;")
        .unwrap();
    assert_eq!(
        setting(&db, "PRAGMA temp.mmap_size"),
        vec![vec![Value::Integer(65536)]]
    );
    assert_eq!(
        setting(&db, "SELECT sum(length(a)) FROM temp.big"),
        vec![vec![Value::Integer(819200)]]
    );
}
