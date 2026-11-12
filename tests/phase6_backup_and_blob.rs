//! Phase 6 (Step 6-B) integration and differential tests for:
//! - Online Backup Engine (`Backup`, `BackupStepResult`, `Connection::backup_init`, `Backup::init`,
//!   `Backup::step`, `Backup::remaining`, `Backup::pagecount`, `Backup::finish`, `Drop for Backup`)
//! - Incremental BLOB I/O (`Blob`, `Connection::blob_open`, `Blob::open`, `Blob::len`,
//!   `Blob::bytes`, `Blob::read_at`, `Blob::write_at`, `Blob::reopen`, `Blob::close`, `Drop for Blob`)
//!
//! Covers Test Groups 18, 20, 25, 26, 39, 45, 56, 58, 59.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqliters::{
    Backup, BackupStepResult, Blob, Connection, ErrorCode, OpenFlags, StepResult, Value,
};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sqliters_p6b_{prefix}_{pid}_{nanos}"));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn query_i64(conn: &Connection, sql: &str) -> i64 {
    let rows = conn.query_quick(sql);
    match &rows[0][0] {
        Value::Integer(i) => *i,
        other => panic!("expected integer from `{sql}`, got {other:?}"),
    }
}

trait QueryQuickExt {
    fn query_quick(&self, sql: &str) -> Vec<Vec<Value>>;
}

impl QueryQuickExt for Connection {
    fn query_quick(&self, sql: &str) -> Vec<Vec<Value>> {
        self.query(sql, &[])
            .expect(sql)
            .rows
            .into_iter()
            .map(|r| r.values().to_vec())
            .collect()
    }
}

fn check_c_sqlite_integrity_if_available(db_path: &Path) {
    let bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sqlite-ref/sqlite3");
    assert!(
        bin.exists(),
        "C SQLite 3.54.1 reference binary must exist at {}",
        bin.display()
    );
    let out = Command::new(bin)
        .arg(db_path)
        .arg("PRAGMA integrity_check;")
        .output()
        .expect("run C sqlite3 CLI");
    assert!(
        out.status.success(),
        "C sqlite3 integrity_check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "ok",
        "C sqlite3 integrity_check output: {stdout}"
    );
}

// =============================================================================
// PART 1: ONLINE BACKUP ENGINE (`Backup`, Groups 45, 56, 59)
// =============================================================================

#[test]
fn backup_init_guards_distinct_connection_file_unknown_db_readonly_and_in_use() {
    let dir = TempDirGuard::new("backup_guards");
    let src_path = dir.file("src.db");
    let dst_path = dir.file("dst.db");

    let conn = Connection::open(&src_path).unwrap();
    conn.execute("CREATE TABLE t(x INT); INSERT INTO t VALUES (1);")
        .unwrap();

    // 1. Same connection object rejected even across distinct slot names (`main -> main`, `main -> temp`, `main -> aux`).
    let err = conn.backup_init("main", &conn, "main").unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("source and destination must be distinct")
    );

    let err = conn.backup_init("main", &conn, "temp").unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("source and destination must be distinct")
    );

    let aux_path = dir.file("aux.db");
    conn.execute(&format!("ATTACH '{}' AS aux;", aux_path.display()))
        .unwrap();
    let err = conn.backup_init("main", &conn, "aux").unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("source and destination must be distinct")
    );

    // 2. Two distinct connections pointing to the same physical on-disk file rejected.
    let conn_same_file = Connection::open(&src_path).unwrap();
    let err = conn
        .backup_init("main", &conn_same_file, "main")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("source and destination must be distinct")
    );

    // 3. Unknown source or destination database name rejected.
    let dst_conn = Connection::open(&dst_path).unwrap();
    let err = conn
        .backup_init("no_such_src", &dst_conn, "main")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("unknown database no_such_src"));

    let err = conn
        .backup_init("main", &dst_conn, "no_such_dst")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("unknown database no_such_dst"));

    // 4. Read-only destination connection rejected with `ErrorCode::ReadOnly`.
    dst_conn.execute("CREATE TABLE dummy(a INT);").unwrap();
    let ro_dst = Connection::open_with_flags(&dst_path, OpenFlags::READONLY).unwrap();
    let err = conn.backup_init("main", &ro_dst, "main").unwrap_err();
    assert_eq!(err.code, ErrorCode::ReadOnly);

    // 5. Destination connection with an active transaction rejected (`destination database is in use`).
    dst_conn.execute("BEGIN IMMEDIATE;").unwrap();
    let err = conn.backup_init("main", &dst_conn, "main").unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("destination database is in use"));
    dst_conn.execute("ROLLBACK;").unwrap();
}

#[test]
fn backup_step_bclosetrans_releases_source_read_lock_unless_user_read_tx_open() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let dir = TempDirGuard::new("backup_bclosetrans");
    let src_path = dir.file("src.db");
    let dst_path = dir.file("dst.db");

    let src = Connection::open(&src_path).unwrap();
    src.execute("PRAGMA page_size = 1024;").unwrap();
    src.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, payload BLOB);")
        .unwrap();
    for i in 1..=20 {
        src.execute(&format!("INSERT INTO t VALUES ({i}, zeroblob(400));"))
            .unwrap();
    }

    let dst = Connection::open(&dst_path).unwrap();
    let mut bkp = src.backup_init("main", &dst, "main").unwrap();

    // Case A: `src` has NO open user read transaction before `step(2)`.
    // After `step(2)` returns `More`, `src` must have released its `Shared` lock (`bCloseTrans == true`),
    // allowing an external connection to acquire an `EXCLUSIVE` write lock and commit on `src`!
    assert_eq!(bkp.step(2).unwrap(), BackupStepResult::More);
    assert!(bkp.remaining() > 0);
    assert!(bkp.pagecount() >= 4);

    let ext_writer = Connection::open(&src_path).unwrap();
    ext_writer
        .execute("BEGIN IMMEDIATE; INSERT INTO t VALUES (21, zeroblob(400)); COMMIT;")
        .expect("external writer must not get SQLITE_BUSY between Backup::step calls when bCloseTrans=true");

    // Case B: `src` DOES have an open user read transaction (`BEGIN; SELECT ...;`) before `step(2)`.
    // After `step(2)` returns `More`, `src`'s read snapshot must remain open (`bCloseTrans == false`),
    // so an external writer attempting `BEGIN IMMEDIATE; COMMIT;` sees `src`'s active `Shared` lock!
    src.execute("BEGIN;").unwrap();
    let cnt_before = query_i64(&src, "SELECT count(*) FROM t;");
    assert_eq!(cnt_before, 21);

    assert_eq!(bkp.step(2).unwrap(), BackupStepResult::More);
    // Because `src` still holds its user read transaction (`Shared` lock), committing a write on `ext_writer`
    // in rollback-journal mode is blocked by `src`'s `Shared` lock.
    ext_writer
        .execute("BEGIN IMMEDIATE; INSERT INTO t VALUES (22, zeroblob(400));")
        .unwrap();
    let commit_err = ext_writer.execute("COMMIT;").unwrap_err();
    assert_eq!(commit_err.code, ErrorCode::Busy);
    ext_writer.execute("ROLLBACK;").unwrap();

    src.execute("COMMIT;").unwrap();

    // Case C: If `src` is inside an uncommitted WRITE transaction, `Backup::step` must return `ErrorCode::Busy`
    // and never copy uncommitted changes!
    src.execute("BEGIN IMMEDIATE; INSERT INTO t VALUES (999, zeroblob(400));")
        .unwrap();
    let busy_err = bkp.step(1).unwrap_err();
    assert_eq!(busy_err.code, ErrorCode::Busy);
    src.execute("ROLLBACK;").unwrap();

    // Finish the backup and verify all 21 committed rows (and NOT rolled-back 999) are in `dst`.
    assert_eq!(bkp.step(-1).unwrap(), BackupStepResult::Done);
    assert_eq!(bkp.remaining(), 0);
    bkp.finish().unwrap();

    assert_eq!(query_i64(&dst, "SELECT count(*) FROM t;"), 21);
    assert_eq!(query_i64(&dst, "SELECT count(*) FROM t WHERE id = 999;"), 0);
    check_c_sqlite_integrity_if_available(&dst_path);
}

#[test]
fn backup_leaf_dirty_page_tracking_and_restart_on_rollback_or_external_write() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let dir = TempDirGuard::new("backup_dirty_and_restart");
    let src_path = dir.file("src.db");
    let dst_path = dir.file("dst.db");

    let src = Connection::open(&src_path).unwrap();
    src.execute("PRAGMA page_size = 1024;").unwrap();
    src.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, tag TEXT, pad BLOB);")
        .unwrap();
    for i in 1..=30 {
        src.execute(&format!(
            "INSERT INTO items VALUES ({i}, 'orig_{i}', zeroblob(300));"
        ))
        .unwrap();
    }

    let dst = Connection::open(&dst_path).unwrap();
    let mut bkp = Backup::init(&src, "main", &dst, "main").unwrap();

    // Copy the first 4 pages so `i_next == 5`.
    assert_eq!(bkp.step(4).unwrap(), BackupStepResult::More);
    let rem_after_4 = bkp.remaining();
    assert!(rem_after_4 > 0);

    // 1. Same-connection committed write to an early row (on a page `< i_next`):
    // `BackupTracker::dirty_pages` records the modified page without requiring a full restart,
    // and the next `step` re-copies the dirty page and advances!
    src.execute("UPDATE items SET tag = 'updated_on_src_1' WHERE id = 1;")
        .unwrap();
    assert_eq!(bkp.step(2).unwrap(), BackupStepResult::More);

    // 2. Same-connection write followed by `ROLLBACK` sets `needs_restart = true`:
    src.execute("BEGIN; UPDATE items SET tag = 'rolled_back' WHERE id = 2; ROLLBACK;")
        .unwrap();
    assert_eq!(bkp.step(2).unwrap(), BackupStepResult::More);

    // 3. Same-connection savepoint rollback also sets `needs_restart = true`:
    src.execute(
        "BEGIN; SAVEPOINT sp; UPDATE items SET tag = 'sp_rolled_back' WHERE id = 3; ROLLBACK TO sp; COMMIT;",
    )
    .unwrap();

    // 4. Run to completion and verify destination matches `src` exactly!
    assert_eq!(bkp.step(-1).unwrap(), BackupStepResult::Done);
    bkp.finish().unwrap();

    let rows = dst.query_quick("SELECT tag FROM items WHERE id IN (1, 2, 3) ORDER BY id;");
    assert_eq!(
        rows,
        vec![
            vec![Value::Text("updated_on_src_1".into())],
            vec![Value::Text("orig_2".into())],
            vec![Value::Text("orig_3".into())],
        ]
    );
    assert_eq!(query_i64(&dst, "SELECT count(*) FROM items;"), 30);
    check_c_sqlite_integrity_if_available(&dst_path);
}

#[test]
fn backup_page_size_conversion_and_wal_page_size_mismatch_guard() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let dir = TempDirGuard::new("backup_pagesize");
    let src_1k_path = dir.file("src_1k.db");
    let dst_4k_path = dir.file("dst_4k.db");
    let dst_1k_from_4k_path = dir.file("dst_1k_from_4k.db");
    let dst_wal_mismatch_path = dir.file("dst_wal_mismatch.db");

    // 1. Populate `src_1k` with `page_size = 1024`.
    let src_1k = Connection::open(&src_1k_path).unwrap();
    src_1k
        .execute(
            "PRAGMA page_size = 1024;
             CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT, body BLOB);
             CREATE INDEX idx_docs_title ON docs(title);",
        )
        .unwrap();
    for i in 1..=25 {
        src_1k
            .execute(&format!(
                "INSERT INTO docs VALUES ({i}, 'doc_{i:03}', zeroblob(500));"
            ))
            .unwrap();
    }
    assert_eq!(query_i64(&src_1k, "PRAGMA page_size;"), 1024);

    // 2. Initialize `dst_4k` with `page_size = 4096` and existing data (more bytes or fewer bytes).
    let dst_4k = Connection::open(&dst_4k_path).unwrap();
    dst_4k
        .execute(
            "PRAGMA page_size = 4096;
             CREATE TABLE old_table(x BLOB);
             INSERT INTO old_table VALUES (zeroblob(16384));",
        )
        .unwrap();
    assert_eq!(query_i64(&dst_4k, "PRAGMA page_size;"), 4096);

    // Backup `src_1k` (1024) -> `dst_4k` (4096).
    {
        let mut bkp = src_1k.backup_init("main", &dst_4k, "main").unwrap();
        while bkp.step(3).unwrap() == BackupStepResult::More {}
        bkp.finish().unwrap();
    }

    assert_eq!(query_i64(&dst_4k, "PRAGMA page_size;"), 1024);
    assert_eq!(query_i64(&dst_4k, "SELECT count(*) FROM docs;"), 25);
    assert_eq!(
        dst_4k.query_quick("PRAGMA integrity_check;"),
        vec![vec![Value::Text("ok".into())]]
    );
    check_c_sqlite_integrity_if_available(&dst_4k_path);

    // 3. Now test the reverse direction: `4096 -> 1024`.
    let src_4k_path = dir.file("src_4k.db");
    let src_4k = Connection::open(&src_4k_path).unwrap();
    src_4k
        .execute(
            "PRAGMA page_size = 4096;
             CREATE TABLE big(id INTEGER PRIMARY KEY, data TEXT);
             INSERT INTO big VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma');",
        )
        .unwrap();
    let dst_1k = Connection::open(&dst_1k_from_4k_path).unwrap();
    dst_1k
        .execute("PRAGMA page_size = 1024; CREATE TABLE t0(a INT); INSERT INTO t0 VALUES (1);")
        .unwrap();
    {
        let mut bkp = src_4k.backup_init("main", &dst_1k, "main").unwrap();
        assert_eq!(bkp.step(-1).unwrap(), BackupStepResult::Done);
        bkp.finish().unwrap();
    }
    assert_eq!(query_i64(&dst_1k, "PRAGMA page_size;"), 4096);
    assert_eq!(query_i64(&dst_1k, "SELECT count(*) FROM big;"), 3);
    check_c_sqlite_integrity_if_available(&dst_1k_from_4k_path);

    // 4. Destination in WAL mode with different page size must return `ErrorCode::ReadOnly`.
    let dst_wal = Connection::open(&dst_wal_mismatch_path).unwrap();
    dst_wal
        .execute(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             CREATE TABLE w(x INT);
             INSERT INTO w VALUES (1);",
        )
        .unwrap();
    let mut bad_bkp = src_1k.backup_init("main", &dst_wal, "main").unwrap();
    let err = bad_bkp.step(1).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReadOnly);
    bad_bkp.finish().unwrap();
}

#[test]
fn backup_drop_before_done_rolls_back_destination_and_guarded_close_detach() {
    let dir = TempDirGuard::new("backup_drop_and_leases");
    let src_path = dir.file("src.db");
    let dst_path = dir.file("dst.db");
    let aux_path = dir.file("aux.db");

    let src = Connection::open(&src_path).unwrap();
    src.execute("PRAGMA page_size = 1024; CREATE TABLE src_t(id INT, b BLOB);")
        .unwrap();
    for i in 1..=20 {
        src.execute(&format!("INSERT INTO src_t VALUES ({i}, zeroblob(400));"))
            .unwrap();
    }

    let dst = Connection::open(&dst_path).unwrap();
    dst.execute(
        "PRAGMA page_size = 1024;
         CREATE TABLE keep_me(v TEXT);
         INSERT INTO keep_me VALUES ('untouched');",
    )
    .unwrap();
    dst.execute(&format!(
        "ATTACH '{}' AS aux; CREATE TABLE aux.a_tbl(x INT);",
        aux_path.display()
    ))
    .unwrap();

    let mut bkp = src.backup_init("main", &dst, "main").unwrap();
    assert_eq!(bkp.step(2).unwrap(), BackupStepResult::More);

    // While `bkp` is open, `dst.try_close()` returns `ErrorCode::Busy`.
    let close_err = dst.try_close().unwrap_err();
    assert_eq!(close_err.error.code, ErrorCode::Busy);
    let dst = close_err.connection;

    // Also test `DETACH` guard on `aux` when a backup targets `aux`:
    {
        let bkp_aux = src.backup_init("main", &dst, "aux").unwrap();
        let detach_err = dst.execute("DETACH aux;").unwrap_err();
        assert_eq!(detach_err.code, ErrorCode::Locked);
        bkp_aux.finish().unwrap();
    }

    // Dropping `bkp` before reaching `Done` must roll back `dst`'s partial write transaction!
    drop(bkp);

    let rows = dst.query_quick("SELECT v FROM keep_me;");
    assert_eq!(rows, vec![vec![Value::Text("untouched".into())]]);
    dst.execute("DETACH aux;").unwrap();
    dst.try_close()
        .expect("dst.try_close() must succeed after Backup is dropped");
}

#[test]
fn backup_concurrent_bidirectional_steps_never_deadlock() {
    let conn1 = Arc::new(Connection::open_in_memory().unwrap());
    let conn2 = Arc::new(Connection::open_in_memory().unwrap());

    conn1
        .execute(
            "CREATE TABLE c1_main(id INTEGER PRIMARY KEY, v BLOB);
             ATTACH ':memory:' AS dst_on_1;",
        )
        .unwrap();
    conn2
        .execute(
            "CREATE TABLE c2_main(id INTEGER PRIMARY KEY, v BLOB);
             ATTACH ':memory:' AS dst_on_2;",
        )
        .unwrap();

    for i in 1..=15 {
        conn1
            .execute(&format!("INSERT INTO c1_main VALUES ({i}, zeroblob(800));"))
            .unwrap();
        conn2
            .execute(&format!("INSERT INTO c2_main VALUES ({i}, zeroblob(800));"))
            .unwrap();
    }

    let barrier = Arc::new(Barrier::new(2));

    let c1_a = Arc::clone(&conn1);
    let c2_a = Arc::clone(&conn2);
    let bar_a = Arc::clone(&barrier);
    let t1 = thread::spawn(move || {
        let mut bkp = Backup::init(&c1_a, "main", &c2_a, "dst_on_2").unwrap();
        bar_a.wait();
        loop {
            match bkp.step(1) {
                Ok(BackupStepResult::More) => thread::yield_now(),
                Ok(BackupStepResult::Done) => break,
                Err(e) if e.code == ErrorCode::Busy => {
                    thread::sleep(Duration::from_micros(50));
                }
                Err(other) => panic!("unexpected error in t1: {other:?}"),
            }
        }
        bkp.finish().unwrap();
    });

    let c1_b = Arc::clone(&conn1);
    let c2_b = Arc::clone(&conn2);
    let bar_b = Arc::clone(&barrier);
    let t2 = thread::spawn(move || {
        let mut bkp = Backup::init(&c2_b, "main", &c1_b, "dst_on_1").unwrap();
        bar_b.wait();
        loop {
            match bkp.step(1) {
                Ok(BackupStepResult::More) => thread::yield_now(),
                Ok(BackupStepResult::Done) => break,
                Err(e) if e.code == ErrorCode::Busy => {
                    thread::sleep(Duration::from_micros(50));
                }
                Err(other) => panic!("unexpected error in t2: {other:?}"),
            }
        }
        bkp.finish().unwrap();
    });

    t1.join().unwrap();
    t2.join().unwrap();

    assert_eq!(
        query_i64(&conn2, "SELECT count(*) FROM dst_on_2.c1_main;"),
        15
    );
    assert_eq!(
        query_i64(&conn1, "SELECT count(*) FROM dst_on_1.c2_main;"),
        15
    );
}

// =============================================================================
// PART 2: INCREMENTAL BLOB I/O (`Blob`, Groups 18, 20, 25, 26, 39, 58)
// =============================================================================

#[test]
fn blob_open_schema_table_column_and_cell_type_guards() {
    let dir = TempDirGuard::new("blob_guards");
    let db_path = dir.file("guards.db");

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE parent(
             id INTEGER PRIMARY KEY,
             code TEXT UNIQUE,
             note BLOB
         );
         CREATE TABLE child(
             id INTEGER PRIMARY KEY,
             parent_code TEXT REFERENCES parent(code),
             indexed_blob BLOB,
             plain_blob BLOB,
             num_col INT,
             real_col REAL,
             null_col BLOB
         );
         CREATE INDEX idx_child_blob ON child(indexed_blob);
         CREATE VIEW v_child AS SELECT id, plain_blob FROM child;
         CREATE TABLE without_rowid_tbl(k INT PRIMARY KEY, b BLOB) WITHOUT ROWID;
         CREATE TABLE gen_stored_tbl(
             id INTEGER PRIMARY KEY,
             b BLOB,
             g INT GENERATED ALWAYS AS (length(b)) STORED
         );
         CREATE TABLE gen_virtual_tbl(
             id INTEGER PRIMARY KEY,
             b BLOB,
             g INT GENERATED ALWAYS AS (length(b)) VIRTUAL
         );
         CREATE TABLE expr_idx_tbl(id INTEGER PRIMARY KEY, a BLOB, b BLOB);
         CREATE INDEX idx_expr ON expr_idx_tbl(length(a));

         INSERT INTO parent VALUES (1, 'P1', X'AABB');
         INSERT INTO child VALUES (10, 'P1', X'1122', X'334455', 42, 3.5, NULL);
         INSERT INTO without_rowid_tbl VALUES (1, X'CAFE');
         INSERT INTO gen_stored_tbl(id, b) VALUES (1, X'BEEF');
         INSERT INTO gen_virtual_tbl(id, b) VALUES (1, X'BEEF');
         INSERT INTO expr_idx_tbl VALUES (1, X'0102', X'0304');",
    )
    .unwrap();

    // 1. View rejected (`cannot open view: v_child`).
    let err = conn
        .blob_open("main", "v_child", "plain_blob", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message.contains("cannot open view: v_child"),
        "got: {}",
        err.message
    );

    // 2. Virtual table rejected (`cannot open virtual table: ...`).
    let err = conn
        .blob_open("main", "pragma_table_info", "name", 1, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("cannot open virtual table"));

    // 3. `WITHOUT ROWID` table rejected (`cannot open table without rowid: ...`).
    let err = conn
        .blob_open("main", "without_rowid_tbl", "b", 1, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("cannot open table without rowid: without_rowid_tbl")
    );

    // 4. Table with `GENERATED ALWAYS` (`STORED` or `VIRTUAL`) rejected for BOTH read-only and read-write!
    for tbl in ["gen_stored_tbl", "gen_virtual_tbl"] {
        for rw in [false, true] {
            let err = conn.blob_open("main", tbl, "b", 1, rw).unwrap_err();
            assert_eq!(err.code, ErrorCode::Error);
            assert!(
                err.message
                    .contains(&format!("cannot open table with generated columns: {tbl}")),
                "unexpected message for {tbl} rw={rw}: {}",
                err.message
            );
        }
    }

    // 5. Missing database / table / column / rowid rejected.
    let err = conn
        .blob_open("no_db", "child", "plain_blob", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("no such table: no_db.child"));

    let err = conn
        .blob_open("main", "no_tbl", "plain_blob", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("no such table: main.no_tbl"));

    let err = conn
        .blob_open("main", "child", "no_col", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("no such column: \"no_col\""));

    let err = conn
        .blob_open("main", "child", "plain_blob", 999, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("no such rowid: 999"));

    // 6. Non-BLOB/TEXT cell types (`NULL`, `INTEGER`, `REAL`, `INTEGER PRIMARY KEY`) rejected.
    let err = conn
        .blob_open("main", "child", "null_col", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("cannot open value of type null"));

    let err = conn
        .blob_open("main", "child", "num_col", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("cannot open value of type integer"));

    let err = conn
        .blob_open("main", "child", "id", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("cannot open value of type integer"));

    let err = conn
        .blob_open("main", "child", "real_col", 10, false)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(err.message.contains("cannot open value of type real"));

    // 7. Read-write guards: `INTEGER PRIMARY KEY`, indexed column, expression index on table, and FK columns.
    let err = conn.blob_open("main", "child", "id", 10, true).unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("cannot open indexed column for writing")
    );

    let err = conn
        .blob_open("main", "child", "indexed_blob", 10, true)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("cannot open indexed column for writing")
    );

    // Read-only open on `indexed_blob` MUST succeed!
    let ro_idx_blob = conn
        .blob_open("main", "child", "indexed_blob", 10, false)
        .unwrap();
    assert_eq!(ro_idx_blob.len(), 2);
    ro_idx_blob.close().unwrap();

    // Expression index on `expr_idx_tbl` blocks writing to ANY column of `expr_idx_tbl`.
    let err = conn
        .blob_open("main", "expr_idx_tbl", "b", 1, true)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("cannot open indexed column for writing")
    );

    // Child FK column (`child.parent_code`) and parent FK column (`parent.code`) blocked when `foreign_keys = ON`.
    let err = conn
        .blob_open("main", "child", "parent_code", 10, true)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert!(
        err.message
            .contains("cannot open foreign key column for writing")
    );

    // Read-only open on child FK column succeeds!
    let ro_fk_blob = conn
        .blob_open("main", "child", "parent_code", 10, false)
        .unwrap();
    let mut fk_buf = [0u8; 2];
    ro_fk_blob.read_at(&mut fk_buf, 0).unwrap();
    assert_eq!(&fk_buf, b"P1");
    ro_fk_blob.close().unwrap();

    // 8. Read-only connection rejects `read_write == true` with `ErrorCode::ReadOnly`.
    let ro_conn = Connection::open_with_flags(&db_path, OpenFlags::READONLY).unwrap();
    let err = Blob::open(&ro_conn, "main", "child", "plain_blob", 10, true).unwrap_err();
    assert_eq!(err.code, ErrorCode::ReadOnly);
}

#[test]
fn blob_read_write_inline_and_multipage_overflow_savepoint_rollback_and_wal() {
    let dir = TempDirGuard::new("blob_overflow_wal");
    let db_path = dir.file("overflow_wal.db");

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "PRAGMA page_size = 1024;
         PRAGMA journal_mode = WAL;
         CREATE TABLE blobs(id INTEGER PRIMARY KEY, small BLOB, large BLOB);
         INSERT INTO blobs VALUES (1, X'0102030405060708', zeroblob(12000));",
    )
    .unwrap();

    // 1. Bounds and read-only enforcement.
    {
        let mut ro = conn.blob_open("main", "blobs", "small", 1, false).unwrap();
        assert_eq!(ro.len(), 8);
        assert_eq!(ro.bytes(), 8);
        assert!(!ro.is_empty());

        let mut buf = [0u8; 4];
        ro.read_at(&mut buf, 2).unwrap();
        assert_eq!(buf, [3, 4, 5, 6]);

        // Out-of-bounds read rejected (`offset + len > blob.len()`).
        let err = ro.read_at(&mut buf, 6).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);

        // Write on read-only handle rejected with `ErrorCode::ReadOnly`.
        let err = ro.write_at(&[9, 9], 0).unwrap_err();
        assert_eq!(err.code, ErrorCode::ReadOnly);
        ro.close().unwrap();
    }

    // 2. Write across a multi-page overflow chain (12,000 bytes on 1024-byte pages = 12 pages)
    // inside a transaction with nested `SAVEPOINT` and `ROLLBACK TO`!
    conn.execute("BEGIN;").unwrap();
    let mut pattern = vec![0u8; 12_000];
    for (idx, b) in pattern.iter_mut().enumerate() {
        *b = ((idx * 7 + 13) & 0xFF) as u8;
    }

    {
        let mut rw = conn.blob_open("main", "blobs", "large", 1, true).unwrap();
        assert_eq!(rw.len(), 12_000);

        // Write in chunks crossing leaf-local and multiple overflow page boundaries.
        rw.write_at(&pattern[0..3500], 0).unwrap();
        rw.write_at(&pattern[3500..8900], 3500).unwrap();
        rw.write_at(&pattern[8900..12_000], 8900).unwrap();

        // Out-of-bounds write rejected.
        let err = rw.write_at(&[1, 2], 11_999).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);

        // Per `sqlite/src/vdbe.c:4001-4006` (`savepoint-5.1.1`), opening a new `SAVEPOINT` while a
        // read-write blob handle is open returns `ErrorCode::Busy` (`cannot open savepoint - SQL statements in progress`).
        let sp_busy = conn.execute("SAVEPOINT sp_blob;").unwrap_err();
        assert_eq!(sp_busy.code, ErrorCode::Busy);
        rw.close().unwrap();
    }

    // Open `SAVEPOINT sp_blob`, then open `rw`, overwrite a slice spanning local + overflow pages,
    // and `ROLLBACK TO sp_blob`!
    conn.execute("SAVEPOINT sp_blob;").unwrap();
    {
        let mut rw = conn.blob_open("main", "blobs", "large", 1, true).unwrap();
        let overwrite = vec![0xEEu8; 3000];
        rw.write_at(&overwrite, 500).unwrap();

        let mut verify_overwritten = vec![0u8; 3000];
        rw.read_at(&mut verify_overwritten, 500).unwrap();
        assert_eq!(verify_overwritten, overwrite);

        // `RELEASE sp_blob` while `rw` is open returns `ErrorCode::Busy` (`savepoint-5.3.4`),
        // whereas `ROLLBACK TO sp_blob` succeeds and expires `rw`!
        let rel_busy = conn.execute("RELEASE sp_blob;").unwrap_err();
        assert_eq!(rel_busy.code, ErrorCode::Busy);

        conn.execute("ROLLBACK TO sp_blob;").unwrap();

        let abort_err = rw.read_at(&mut verify_overwritten, 0).unwrap_err();
        assert_eq!(abort_err.code, ErrorCode::Abort);

        // `rw.reopen(1)` re-binds to row 1 and sees the pre-savepoint `pattern`!
        rw.reopen(1).unwrap();
        let mut full_readback = vec![0u8; 12_000];
        rw.read_at(&mut full_readback, 0).unwrap();
        assert_eq!(full_readback, pattern);

        // While `rw` is an open unexpired writable blob, `COMMIT` must return `ErrorCode::Busy`!
        let commit_busy = conn.execute("COMMIT;").unwrap_err();
        assert_eq!(commit_busy.code, ErrorCode::Busy);

        rw.close().unwrap();
    }

    conn.execute("RELEASE sp_blob; COMMIT;").unwrap();

    // Verify from a fresh connection and via C `sqlite3` `PRAGMA integrity_check`.
    let reader = Connection::open(&db_path).unwrap();
    let rows = reader.query_quick("SELECT large FROM blobs WHERE id = 1;");
    assert_eq!(rows, vec![vec![Value::Blob(pattern)]]);
    check_c_sqlite_integrity_if_available(&db_path);
}

#[test]
fn blob_expiry_on_row_mutation_reopen_and_schema_invalidation() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE p(id INTEGER PRIMARY KEY, b BLOB);
         CREATE TABLE c(
             id INTEGER PRIMARY KEY,
             pid INT REFERENCES p(id) ON DELETE CASCADE,
             data BLOB
         );
         CREATE TABLE audit(msg TEXT);
         INSERT INTO p VALUES (1, X'AAAA'), (2, X'BBBB');
         INSERT INTO c VALUES (10, 1, X'101010'), (20, 2, X'202020'), (30, 2, X'303030');",
    )
    .unwrap();

    // 1. Open blobs on row 10 and row 20 of `c`.
    let mut blob_10 = conn.blob_open("main", "c", "data", 10, false).unwrap();
    let mut blob_20 = conn.blob_open("main", "c", "data", 20, true).unwrap();

    // Unrelated `CREATE TABLE` must NOT expire existing open `Blob` handles!
    conn.execute("CREATE TABLE unrelated(x INT);").unwrap();
    let mut buf3 = [0u8; 3];
    blob_10.read_at(&mut buf3, 0).unwrap();
    assert_eq!(buf3, [0x10, 0x10, 0x10]);

    // 2. Updating row 20 via SQL (`UPDATE`) expires `blob_20` ONLY; `blob_10` remains valid!
    conn.execute("UPDATE c SET data = X'99887766' WHERE id = 20;")
        .unwrap();
    blob_10.read_at(&mut buf3, 0).unwrap();
    assert_eq!(buf3, [0x10, 0x10, 0x10]);

    let err = blob_20.read_at(&mut buf3, 0).unwrap_err();
    assert_eq!(err.code, ErrorCode::Abort);
    let err = blob_20.write_at(&[1, 2, 3], 0).unwrap_err();
    assert_eq!(err.code, ErrorCode::Abort);

    // 3. `blob_20.reopen(20)` recovers from row-level expiry and picks up the new 4-byte length!
    blob_20.reopen(20).unwrap();
    assert_eq!(blob_20.len(), 4);
    let mut buf4 = [0u8; 4];
    blob_20.read_at(&mut buf4, 0).unwrap();
    assert_eq!(buf4, [0x99, 0x88, 0x77, 0x66]);

    // 4. `FOREIGN KEY ON DELETE CASCADE`: deleting `p.id = 1` cascades to `c.id = 10` and expires `blob_10`,
    // while `blob_20` (on `c.id = 20`, `pid = 2`) remains valid!
    conn.execute("DELETE FROM p WHERE id = 1;").unwrap();
    let err = blob_10.read_at(&mut buf3, 0).unwrap_err();
    assert_eq!(err.code, ErrorCode::Abort);
    blob_20.read_at(&mut buf4, 0).unwrap();
    assert_eq!(buf4, [0x99, 0x88, 0x77, 0x66]);

    // `blob_10.reopen(30)` switches `blob_10` to row 30!
    blob_10.reopen(30).unwrap();
    blob_10.read_at(&mut buf3, 0).unwrap();
    assert_eq!(buf3, [0x30, 0x30, 0x30]);

    // 5. `INSERT OR REPLACE` and `ON CONFLICT DO UPDATE` also expire open `Blob` handles on the target row!
    conn.execute("INSERT OR REPLACE INTO c VALUES (30, 2, X'AABBCC');")
        .unwrap();
    assert_eq!(
        blob_10.read_at(&mut buf3, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    blob_10.reopen(30).unwrap();

    conn.execute(
        "INSERT INTO c VALUES (30, 2, X'DDEEFF')
         ON CONFLICT(id) DO UPDATE SET data = excluded.data;",
    )
    .unwrap();
    assert_eq!(
        blob_10.read_at(&mut buf3, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    blob_10.reopen(30).unwrap();
    blob_10.read_at(&mut buf3, 0).unwrap();
    assert_eq!(buf3, [0xDD, 0xEE, 0xFF]);

    // 6. `ALTER TABLE` invalidates the table for open blobs so even `reopen` returns `ErrorCode::Abort`!
    conn.execute("ALTER TABLE c ADD COLUMN extra INT DEFAULT 0;")
        .unwrap();
    assert_eq!(
        blob_10.read_at(&mut buf3, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    assert_eq!(blob_10.reopen(30).unwrap_err().code, ErrorCode::Abort);

    // While `blob_10` and `blob_20` are still open (even expired), `conn.try_close()` returns `ErrorCode::Busy`!
    let close_err = conn.try_close().unwrap_err();
    assert_eq!(close_err.error.code, ErrorCode::Busy);
    let conn = close_err.connection;
    blob_10.close().unwrap();
    blob_20.close().unwrap();
    conn.try_close().unwrap();
}

#[test]
fn blob_coexistence_with_statements_and_commit_with_readonly_blob() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE items(id INTEGER PRIMARY KEY, payload TEXT);
         INSERT INTO items VALUES (1, 'hello_world'), (2, 'second_row_payload');",
    )
    .unwrap();

    // Open a read-only blob on row 1 and a prepared `Statement` scanning `items`.
    let blob_ro = conn
        .blob_open("main", "items", "payload", 1, false)
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT id, payload FROM items ORDER BY id;")
        .unwrap();

    assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
    let mut buf5 = [0u8; 5];
    blob_ro.read_at(&mut buf5, 0).unwrap();
    assert_eq!(&buf5, b"hello");

    assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
    assert_eq!(stmt.step().unwrap(), StepResult::Done);
    stmt.reset().unwrap();
    drop(stmt);

    // Starting and committing an explicit transaction that inserts row 3 while `blob_ro` (read-only) is open:
    // `COMMIT` must succeed (unlike a writable blob which returns `Busy`), and `blob_ro` retains its read lease!
    conn.execute("BEGIN; INSERT INTO items VALUES (3, 'third'); COMMIT;")
        .unwrap();
    blob_ro.read_at(&mut buf5, 0).unwrap();
    assert_eq!(&buf5, b"hello");

    // Modifying row 1 itself expires `blob_ro`.
    conn.execute("UPDATE items SET payload = 'mutated_row_1' WHERE id = 1;")
        .unwrap();
    assert_eq!(
        blob_ro.read_at(&mut buf5, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    blob_ro.close().unwrap();
}

#[test]
fn blob_trigger_expiry_table_clear_vacuum_detach_and_drop_table() {
    let dir = TempDirGuard::new("blob_trig_vac_detach");
    let aux_path = dir.file("aux.db");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(&format!(
        "CREATE TABLE main_t(id INTEGER PRIMARY KEY, b BLOB);
         CREATE TABLE trig_src(x INT);
         CREATE TRIGGER tr_upd AFTER INSERT ON trig_src BEGIN
             UPDATE main_t SET b = X'FFFF' WHERE id = new.x;
         END;
         INSERT INTO main_t VALUES (1, X'0102'), (2, X'0304');
         ATTACH '{}' AS aux;
         CREATE TABLE aux.aux_t(id INTEGER PRIMARY KEY, b BLOB);
         INSERT INTO aux.aux_t VALUES (1, X'AABB');",
        aux_path.display()
    ))
    .unwrap();

    // 1. Trigger-driven update on `main_t` row 1 expires `blob_1` while `blob_2` stays valid.
    let mut blob_1 = conn.blob_open("main", "main_t", "b", 1, false).unwrap();
    let blob_2 = conn.blob_open("main", "main_t", "b", 2, false).unwrap();
    conn.execute("INSERT INTO trig_src VALUES (1);").unwrap();

    let mut buf2 = [0u8; 2];
    assert_eq!(
        blob_1.read_at(&mut buf2, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    blob_2.read_at(&mut buf2, 0).unwrap();
    assert_eq!(buf2, [0x03, 0x04]);

    // 2. `DELETE FROM main_t;` (`OP_Clear`) expires all blobs on `main_t`, but `reopen` works after inserting a new row.
    conn.execute("DELETE FROM main_t; INSERT INTO main_t VALUES (5, X'5566');")
        .unwrap();
    assert_eq!(
        blob_2.read_at(&mut buf2, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    blob_1.reopen(5).unwrap();
    blob_1.read_at(&mut buf2, 0).unwrap();
    assert_eq!(buf2, [0x55, 0x66]);
    blob_1.close().unwrap();
    blob_2.close().unwrap();

    // 3. While `blob_drop` is actively open on `drop_me`, `DROP TABLE drop_me;` returns `ErrorCode::Locked`,
    // and `ROLLBACK` invalidates open blobs permanently (`read_at` and `reopen` both return `Abort`).
    conn.execute("CREATE TABLE drop_me(id INTEGER PRIMARY KEY, b BLOB); INSERT INTO drop_me VALUES (1, X'11');")
        .unwrap();
    let mut blob_drop = conn.blob_open("main", "drop_me", "b", 1, false).unwrap();
    assert_eq!(
        conn.execute("DROP TABLE drop_me;").unwrap_err().code,
        ErrorCode::Locked
    );

    conn.execute("BEGIN; INSERT INTO drop_me VALUES (2, X'22'); ROLLBACK;")
        .unwrap();
    let mut buf1 = [0u8; 1];
    assert_eq!(
        blob_drop.read_at(&mut buf1, 0).unwrap_err().code,
        ErrorCode::Abort
    );
    assert_eq!(blob_drop.reopen(1).unwrap_err().code, ErrorCode::Abort);
    // Now that `blob_drop` has been invalidated and released its lease, `DROP TABLE drop_me;` succeeds!
    conn.execute("DROP TABLE drop_me;").unwrap();
    blob_drop.close().unwrap();
}

#[test]
fn backup_and_blob_drop_inside_udf_callback_via_pending_drop() {
    use std::sync::Mutex;

    let conn = Connection::open_in_memory().unwrap();
    let dst = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, b BLOB); INSERT INTO t VALUES (1, X'CAFE');",
    )
    .unwrap();

    let blob = conn.blob_open("main", "t", "b", 1, false).unwrap();
    let bkp = conn.backup_init("main", &dst, "main").unwrap();

    let holder: Arc<Mutex<(Option<Blob>, Option<Backup>)>> =
        Arc::new(Mutex::new((Some(blob), Some(bkp))));
    let holder_cb = Arc::clone(&holder);

    conn.create_scalar_function(
        "drop_handles_now",
        0,
        sqliters::FunctionFlags::default(),
        move |_args| {
            let mut guard = holder_cb.lock().unwrap();
            // Dropping both `Blob` and `Backup` while `conn`'s execution gate is active on this thread!
            guard.0.take();
            guard.1.take();
            Ok(Value::Integer(1).into())
        },
    )
    .unwrap();

    assert_eq!(query_i64(&conn, "SELECT drop_handles_now();"), 1);

    // After the statement finishes, `PendingDrop` items have been drained, so both `conn` and `dst` close cleanly!
    conn.try_close().unwrap();
    dst.try_close().unwrap();
}
