//! Backup API alignment with C SQLite 3.54.1 (step 5j). Every expected value below is what the C
//! library returns for the same call sequence: the scenarios are those of
//! `target/perf-work/step5j/bk/sc/*.txt`, run through the C interpreter `bk/bkx.c` against the
//! 3.54.1 amalgamation (outputs `bk/out/*.C`).

use sqliters_types::{ErrorCode, Value};

use crate::Connection;
use crate::backup::{Backup, BackupStepResult};

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("sqliters-5j-{}-{name}.db", std::process::id()));
        let db = Self(path);
        db.remove_all();
        db
    }
    fn side(&self, suffix: &str) -> std::path::PathBuf {
        let mut s = self.0.clone().into_os_string();
        s.push(suffix);
        s.into()
    }
    fn remove_all(&self) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let _ = std::fs::remove_file(self.side(suffix));
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.remove_all();
    }
}

fn int(conn: &Connection, sql: &str) -> i64 {
    match conn.query(sql, &[]).unwrap().rows[0].values()[0] {
        Value::Integer(n) => n,
        ref other => panic!("expected an integer, got {other:?}"),
    }
}

/// Table `t` with `rows` rows of `zeroblob(1000)`: 60 rows are 17 pages of 4096 bytes.
fn fill(conn: &Connection, wal: bool, rows: usize) {
    conn.execute("PRAGMA page_size=4096").unwrap();
    if wal {
        conn.query("PRAGMA journal_mode=WAL", &[]).unwrap();
    }
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB)")
        .unwrap();
    conn.execute("BEGIN").unwrap();
    for i in 1..=rows {
        conn.execute(&format!("INSERT INTO t VALUES({i}, zeroblob(1000))"))
            .unwrap();
    }
    conn.execute("COMMIT").unwrap();
}

/// (rc, remaining, pagecount) of one step: rc 0 = SQLITE_OK, 101 = SQLITE_DONE, else the error.
fn st(b: &mut Backup, n: i32) -> (i32, i32, i32) {
    let rc = match b.step(n) {
        Ok(BackupStepResult::More) => 0,
        Ok(BackupStepResult::Done) => 101,
        Err(e) => e.code as i32,
    };
    (rc, b.remaining(), b.pagecount())
}

fn fin(b: Backup) -> i32 {
    match b.finish() {
        Ok(()) => 0,
        Err(e) => e.code as i32,
    }
}

/// C: `sqlite3_backup_finish()` returns `p->rc` unless it is SQLITE_DONE — the result of the last
/// step, SQLITE_BUSY included (scenario `err_finish_after_busy`).
#[test]
fn finish_returns_the_result_of_the_last_failed_step() {
    let (s, d) = (TempDb::new("fin-src"), TempDb::new("fin-dst"));
    let src = Connection::open(&s.0).unwrap();
    fill(&src, false, 60);
    let dst = Connection::open(&d.0).unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    src.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES(5000, zeroblob(10))")
        .unwrap();
    assert_eq!(st(&mut b, 5), (5, 12, 17));
    assert_eq!(fin(b), 5);
    src.execute("ROLLBACK").unwrap();
    assert_eq!(int(&dst, "SELECT count(*) FROM sqlite_master"), 0);

    // A step that succeeded resets it: finish after SQLITE_OK is SQLITE_OK.
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(fin(b), 0);
}

/// C: a read-only destination is accepted by `sqlite3_backup_init()`; the first step fails with
/// SQLITE_READONLY (8) when it starts the destination's write transaction, remaining/pagecount
/// stay 0, and finish returns 8 (scenario `err_readonly_dst`).
#[test]
fn a_read_only_destination_fails_at_step_not_at_init() {
    let (s, d) = (TempDb::new("ro-src"), TempDb::new("ro-dst"));
    let src = Connection::open(&s.0).unwrap();
    fill(&src, false, 10);
    {
        let dst = Connection::open(&d.0).unwrap();
        dst.execute("CREATE TABLE d(x)").unwrap();
    }
    let dst = Connection::open_with_flags(&d.0, sqliters_vfs::OpenFlags::READONLY).unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!((b.remaining(), b.pagecount()), (0, 0));
    assert_eq!(st(&mut b, 5), (8, 0, 0));
    assert_eq!(st(&mut b, 5), (8, 0, 0));
    assert_eq!(fin(b), 8);
    assert_eq!(int(&dst, "SELECT count(*) FROM sqlite_master"), 1);
}

/// C: `sqlite3_backup_init()` only looks at the transaction state of the destination b-tree, so
/// a second backup object for the same destination can be created while the first has not
/// stepped; used one after the other both complete (scenarios `two_backups_*`).
#[test]
fn a_second_backup_into_the_same_destination_can_be_initialised() {
    let (s, d) = (TempDb::new("two-src"), TempDb::new("two-dst"));
    let src = Connection::open(&s.0).unwrap();
    fill(&src, false, 10);
    let dst = Connection::open(&d.0).unwrap();
    dst.execute_batch("CREATE TABLE d(x); INSERT INTO d VALUES(1),(2)")
        .unwrap();
    let mut b1 = Backup::init(&src, "main", &dst, "main").unwrap();
    let mut b2 = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b1, -1), (101, 0, 5));
    assert_eq!(fin(b1), 0);
    assert_eq!(st(&mut b2, -1), (101, 0, 5));
    assert_eq!(fin(b2), 0);
    assert_eq!(int(&dst, "SELECT count(*) FROM t"), 10);

    // Once the first backup holds the destination's transaction a new init is refused
    // (`checkReadTransaction`: "destination database is in use").
    let mut b1 = Backup::init(&src, "main", &dst, "main").unwrap();
    src.execute("INSERT INTO t VALUES(100, zeroblob(3000))")
        .unwrap();
    assert_eq!(st(&mut b1, 2).0, 0);
    let err = Backup::init(&src, "main", &dst, "main").unwrap_err();
    assert_eq!(err.code, ErrorCode::Error);
    assert_eq!(err.message, "destination database is in use");
    assert_eq!(st(&mut b1, -1).0, 101);
    assert_eq!(fin(b1), 0);
}

/// C: a source that shrinks below the backup's position (DELETE + VACUUM through the source
/// connection) does not restart the backup: the next step is SQLITE_DONE and
/// `sqlite3_backup_remaining()` is negative (scenario `mod_same_delete_vacuum_j_ff_n5`).
#[test]
fn a_shrinking_source_does_not_restart_the_backup() {
    let (s, d) = (TempDb::new("shr-src"), TempDb::new("shr-dst"));
    let src = Connection::open(&s.0).unwrap();
    fill(&src, false, 60);
    let dst = Connection::open(&d.0).unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(st(&mut b, 5), (0, 7, 17));
    src.execute_batch("DELETE FROM t WHERE id>5; VACUUM")
        .unwrap();
    assert_eq!(st(&mut b, 5), (101, -6, 4));
    assert_eq!(st(&mut b, 5), (101, -6, 4));
    assert_eq!(fin(b), 0);
    assert_eq!(int(&dst, "SELECT count(*) FROM t"), 5);
    assert_eq!(int(&dst, "PRAGMA page_count"), 4);
    let ok = dst.query("PRAGMA integrity_check", &[]).unwrap();
    assert_eq!(ok.rows[0].values()[0], Value::Text("ok".to_owned()));
}

/// C: in rollback-journal mode neither ROLLBACK nor ROLLBACK TO restarts a backup (the journal
/// playback feeds the restored pages to the backup) — scenarios `mod_same_rollback_j_ff_n5` and
/// `mod_same_savepoint_rb_then_write_j_ff_n5`.
#[test]
fn journal_rollback_and_savepoint_rollback_do_not_restart() {
    for sql in [
        "BEGIN; UPDATE t SET v=zeroblob(900) WHERE id=1; ROLLBACK",
        "BEGIN; SAVEPOINT s; UPDATE t SET v=zeroblob(902) WHERE id=3; ROLLBACK TO s; \
         UPDATE t SET v=zeroblob(903) WHERE id=59; COMMIT",
    ] {
        let (s, d) = (TempDb::new("rb-src"), TempDb::new("rb-dst"));
        let src = Connection::open(&s.0).unwrap();
        fill(&src, false, 60);
        let dst = Connection::open(&d.0).unwrap();
        let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
        assert_eq!(st(&mut b, 5), (0, 12, 17));
        assert_eq!(st(&mut b, 5), (0, 7, 17));
        src.execute_batch(sql).unwrap();
        assert_eq!(st(&mut b, 5), (0, 2, 17), "{sql}");
        src.execute_batch(sql).unwrap();
        assert_eq!(st(&mut b, 5), (101, 0, 17), "{sql}");
        assert_eq!(fin(b), 0);
        assert_eq!(
            int(&dst, "SELECT sum(length(v)) FROM t"),
            int(&src, "SELECT sum(length(v)) FROM t")
        );
    }
}

/// C: a WAL rollback restarts the backups once per page the transaction changed
/// (`pagerUndoCallback`); a transaction that changed nothing — a statement that fails on its
/// first row — restarts nothing (scenarios `mod_same_rollback_w_ff_n5`, `mod_same_insert_w_ff_n5`).
#[test]
fn a_wal_rollback_restarts_only_when_pages_were_changed() {
    let (s, d) = (TempDb::new("walrb-src"), TempDb::new("walrb-dst"));
    let src = Connection::open(&s.0).unwrap();
    fill(&src, true, 60);
    let dst = Connection::open(&d.0).unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(st(&mut b, 5), (0, 7, 17));
    src.execute_batch("BEGIN; UPDATE t SET v=zeroblob(900) WHERE id=1; ROLLBACK")
        .unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(fin(b), 0);

    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(st(&mut b, 5), (0, 7, 17));
    src.execute("INSERT INTO t SELECT id+1000, zeroblob(1000) FROM t WHERE id<=20")
        .unwrap();
    assert_eq!(st(&mut b, 5), (0, 7, 22));
    // The same statement again fails on its first row (UNIQUE): nothing was written.
    assert_eq!(
        src.execute("INSERT INTO t SELECT id+1000, zeroblob(1000) FROM t WHERE id<=20")
            .unwrap_err()
            .code,
        ErrorCode::Constraint
    );
    assert_eq!(st(&mut b, 5), (0, 2, 22));
    assert_eq!(st(&mut b, 5), (101, 0, 22));
    assert_eq!(fin(b), 0);
}

/// C (`btreeOverwriteContent`): replacing a row by a record with the same bytes does not make
/// the page writable, so the transaction commits nothing — the file's change counter stays and
/// a backup of an in-memory source is not restarted (scenario `mod_same_update_copied_j_mf_n5`).
#[test]
fn a_byte_identical_update_writes_nothing() {
    let s = TempDb::new("noop-src");
    let src = Connection::open(&s.0).unwrap();
    src.execute_batch(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB); \
         INSERT INTO t VALUES(1,zeroblob(999)),(2,'abc')",
    )
    .unwrap();
    let cc = |c: &Connection| c.database_header().unwrap().change_counter;
    // C: 2, 2, 2, 3 after each of these statements.
    assert_eq!(cc(&src), 2);
    src.execute("UPDATE t SET v=zeroblob(999) WHERE id=1")
        .unwrap();
    assert_eq!(cc(&src), 2);
    src.execute("UPDATE t SET v='abc' WHERE id=2").unwrap();
    assert_eq!(cc(&src), 2);
    src.execute("UPDATE t SET v='abd' WHERE id=2").unwrap();
    assert_eq!(cc(&src), 3);

    let d = TempDb::new("noop-dst");
    let mem = Connection::open_in_memory().unwrap();
    fill(&mem, false, 60);
    let dst = Connection::open(&d.0).unwrap();
    let mut b = Backup::init(&mem, "main", &dst, "main").unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    assert_eq!(st(&mut b, 5), (0, 7, 17));
    // A commit of an in-memory source that changed pages restarts the backup ...
    mem.execute("UPDATE t SET v=zeroblob(999) WHERE id<=3")
        .unwrap();
    assert_eq!(st(&mut b, 5), (0, 12, 17));
    // ... the same statement again changes no byte: no restart.
    mem.execute("UPDATE t SET v=zeroblob(999) WHERE id<=3")
        .unwrap();
    assert_eq!(st(&mut b, 5), (0, 7, 17));
    assert_eq!(fin(b), 0);
}
