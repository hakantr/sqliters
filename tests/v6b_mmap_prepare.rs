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

#[cfg(unix)]
#[test]
fn active_mapped_readers_hold_the_limit_until_their_last_cursor_closes() {
    let scratch = Scratch::new();
    let path = scratch.0.join("main.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "PRAGMA page_size=4096;
         CREATE TABLE t(a INTEGER PRIMARY KEY,b);
         INSERT INTO t VALUES (1,zeroblob(3000)),(2,zeroblob(3000)),(3,zeroblob(3000));",
    )
    .unwrap();
    let image = std::fs::read(&path).unwrap();
    let mut first = db.prepare("SELECT a,length(b) FROM t").unwrap();
    let mut second = db.prepare("SELECT a,length(b) FROM t WHERE a>=2 ORDER BY a").unwrap();
    const SET: &str = "PRAGMA mmap_size=1048576";
    const CLEAR: &str = "PRAGMA mmap_size=0";
    const GET: &str = "PRAGMA mmap_size";
    assert_eq!(setting(&db, SET), vec![vec![Value::Integer(1048576)]]);
    assert!(matches!(first.step().unwrap(), StepResult::Row(_)));
    assert!(matches!(second.step().unwrap(), StepResult::Row(_)));
    db.execute_batch("PRAGMA shrink_memory").unwrap();

    let mut rejected = db.prepare(CLEAR).unwrap();
    first.reset().unwrap();
    assert_eq!(setting(&db, CLEAR), vec![vec![Value::Integer(1048576)]]);
    drop(second);
    assert_eq!(setting(&db, GET), vec![vec![Value::Integer(1048576)]]);
    assert_eq!(setting(&db, CLEAR), vec![vec![Value::Integer(0)]]);
    assert_eq!(rows(&mut rejected), vec![vec![Value::Integer(1048576)]]);
    assert_eq!(std::fs::read(path).unwrap(), image);
}

#[cfg(unix)]
#[test]
fn mapped_pager_refs_do_not_belong_to_pcache() {
    use sqliters_pager::{PAGER_STAT_HIT, PAGER_STAT_MISS, PageSource, Pager};
    use sqliters_vfs::OpenFlags;

    let scratch = Scratch::new();
    let path = scratch.0.join("main.db");
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA page_size=4096; CREATE TABLE a(x); CREATE TABLE b(y);")
            .unwrap();
    }
    let mut pager = Pager::open_file(&path, OpenFlags::READWRITE).unwrap();
    pager.set_mmap_limit(8448);
    pager.begin_read().unwrap();
    assert_eq!(pager.page_count(), 3);
    pager.page_ref(2).unwrap();
    let hit = pager.cache_stat(PAGER_STAT_HIT, false);
    let miss = pager.cache_stat(PAGER_STAT_MISS, false);

    let first = pager.pin_btree_page(2, true).unwrap();
    let second = pager.pin_btree_page(2, true).unwrap();
    assert_ne!(first, PageSource::CACHE);
    assert_ne!(first, second);
    assert_eq!(pager.pinned_page(2, first).unwrap()[0], 0x0d);
    assert_eq!(pager.page_refcount(2), 1);
    assert_eq!(pager.cache_stat(PAGER_STAT_HIT, false), hit);
    assert_eq!(pager.cache_stat(PAGER_STAT_MISS, false), miss);

    pager.unpin_page(2, second);
    pager.page_unref(2);
    assert_eq!(pager.page_refcount(2), 0);
    assert_eq!(pager.pin_btree_page(3, true).unwrap(), PageSource::CACHE);
    pager.page_unref(3);
    pager.set_mmap_limit(0);
    assert_eq!(pager.mmap_size().unwrap(), Some(8448));
    pager.unpin_page(2, first);
    pager.set_mmap_limit(0);
    assert_eq!(pager.mmap_size().unwrap(), Some(0));
}

#[cfg(unix)]
#[test]
fn rollback_writes_and_wal_checkpoint_extend_an_existing_mapping() {
    for mode in ["delete", "truncate", "wal"] {
        let scratch = Scratch::new();
        let db = Connection::open(scratch.0.join("main.db")).unwrap();
        db.execute_batch(&format!(
            "PRAGMA journal_mode={mode}; PRAGMA page_size=4096;
             CREATE TABLE t(a INTEGER PRIMARY KEY, b); INSERT INTO t VALUES(1,1);"
        ))
        .unwrap();
        if mode == "wal" {
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        }
        db.execute_batch("PRAGMA mmap_size=8448").unwrap();
        // The first xFetch sees page 2 at EOF and falls back to the cache.
        assert_eq!(setting(&db, "SELECT a FROM t"), vec![vec![Value::Integer(1)]]);

        db.execute_batch(
            "INSERT INTO t VALUES(2,zeroblob(3000)),(3,zeroblob(3000)),(4,zeroblob(3000));",
        )
        .unwrap();
        if mode == "wal" {
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        }
        let mut reader = db.prepare("SELECT a FROM t").unwrap();
        assert!(matches!(reader.step().unwrap(), StepResult::Row(_)));
        assert_eq!(
            setting(&db, "PRAGMA mmap_size=0"),
            vec![vec![Value::Integer(8448)]],
            "{mode}"
        );
        drop(reader);
        assert_eq!(setting(&db, "PRAGMA mmap_size=0"), vec![vec![Value::Integer(0)]]);
    }
}
