//! Backup API alignment with C SQLite 3.54.1 (step 5i). Every expected value below is what the C
//! library returns for the same call sequence (measured with a C harness against the 3.54.1
//! amalgamation, `target/perf-work/step5i/c/bk.c`).

use sqliters_types::{ErrorCode, Value};

use crate::Connection;
use crate::backup::{Backup, BackupStepResult};

struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("sqliters-5i-{}-{name}.db", std::process::id()));
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

/// `PRAGMA page_size=<pgsz>`, table `t` with `rows` rows of `zeroblob(1000)`: 60 rows are 17
/// pages of 4096 bytes in both engines.
fn fill(conn: &Connection, pgsz: u32, rows: usize) {
    conn.execute(&format!(
        "PRAGMA page_size={pgsz}; CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB);"
    ))
    .unwrap();
    conn.execute("BEGIN").unwrap();
    for i in 1..=rows {
        conn.execute(&format!("INSERT INTO t VALUES({i}, zeroblob(1000))"))
            .unwrap();
    }
    conn.execute("COMMIT").unwrap();
}

/// (rc, remaining, pagecount) of one step: rc 0 = SQLITE_OK, 101 = SQLITE_DONE, else the error.
fn step(b: &mut Backup, n: i32) -> (i32, i32, i32) {
    let rc = match b.step(n) {
        Ok(BackupStepResult::More) => 0,
        Ok(BackupStepResult::Done) => 101,
        Err(e) => e.code as i32,
    };
    (rc, b.remaining(), b.pagecount())
}

/// `sqlite3_backup_remaining()` / `sqlite3_backup_pagecount()` are 0 until a step has counted the
/// source pages, including after a step that failed before copying anything.
#[test]
fn remaining_and_pagecount_are_zero_before_the_first_successful_step() {
    let src = Connection::open_in_memory().unwrap();
    fill(&src, 4096, 60);
    let dst = Connection::open_in_memory().unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!((b.remaining(), b.pagecount()), (0, 0));
    assert_eq!(step(&mut b, 0), (0, 17, 17));
    assert_eq!(step(&mut b, 5), (0, 12, 17));
    b.finish().unwrap();

    // WAL destination with another page size: SQLITE_READONLY, counters still 0.
    let wal = TempDb::new("wal_mismatch");
    let dst = Connection::open(&wal.0).unwrap();
    dst.execute("PRAGMA page_size=1024; PRAGMA journal_mode=WAL; CREATE TABLE d(x);")
        .unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 9), (8, 0, 0));
    assert_eq!(step(&mut b, 9), (8, 0, 0));
    drop(b);
}

/// `sqlite3_close()` on the SOURCE of an unfinished backup is SQLITE_BUSY ("unable to close due
/// to unfinalized statements or unfinished backups", `connectionIsBusy` in `main.c`).
#[test]
fn closing_the_source_of_an_unfinished_backup_is_busy() {
    let src = Connection::open_in_memory().unwrap();
    fill(&src, 4096, 60);
    let dst = Connection::open_in_memory().unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 5), (0, 12, 17));

    let err = src.try_close().unwrap_err();
    assert_eq!(err.error.code, ErrorCode::Busy);
    assert_eq!(
        err.error.message,
        "unable to close due to unfinalized statements or unfinished backups"
    );
    let src = err.connection;

    assert_eq!(step(&mut b, -1), (101, 0, 17));
    b.finish().unwrap();
    assert!(src.try_close().is_ok());
    assert_eq!(int(&dst, "SELECT count(*) FROM t"), 60);
}

/// `backupOnePage`: an in-memory destination that already has another page size cannot be
/// converted: SQLITE_READONLY on the first page copy, and the destination keeps its content.
/// An in-memory destination that is still empty simply takes the source page size.
#[test]
fn in_memory_destination_with_another_page_size_is_readonly() {
    let src = Connection::open_in_memory().unwrap();
    fill(&src, 4096, 60);

    let dst = Connection::open_in_memory().unwrap();
    dst.execute("PRAGMA page_size=1024; CREATE TABLE d(x); INSERT INTO d VALUES(zeroblob(5000));")
        .unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 9), (8, 0, 0));
    assert_eq!(step(&mut b, 9), (8, 0, 0));
    drop(b);
    assert_eq!(int(&dst, "SELECT count(*) FROM d"), 1);
    assert_eq!(int(&dst, "PRAGMA page_size"), 1024);

    let dst = Connection::open_in_memory().unwrap();
    dst.execute("PRAGMA page_size=1024;").unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 9), (0, 8, 17));
    assert_eq!(step(&mut b, 9), (101, 0, 17));
    b.finish().unwrap();
    assert_eq!(int(&dst, "PRAGMA page_size"), 4096);
    assert_eq!(int(&dst, "SELECT count(*) FROM t"), 60);
}

/// `sqlite3BtreeBeginTrans(pDest, 2)`: the destination is locked EXCLUSIVE from the first step,
/// so another connection can neither read nor write the destination file until the backup is
/// finished.
#[test]
fn destination_file_is_exclusively_locked_during_the_backup() {
    let src = Connection::open_in_memory().unwrap();
    fill(&src, 4096, 60);
    let file = TempDb::new("excl_dst");
    let dst = Connection::open(&file.0).unwrap();
    dst.execute("CREATE TABLE d(x); INSERT INTO d VALUES(1),(2);")
        .unwrap();
    let other = Connection::open(&file.0).unwrap();
    assert_eq!(int(&other, "SELECT count(*) FROM d"), 2);

    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 5), (0, 12, 17));
    assert_eq!(
        other.query("SELECT count(*) FROM d", &[]).unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        other.execute("INSERT INTO d VALUES(3)").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(step(&mut b, -1), (101, 0, 17));
    b.finish().unwrap();
    assert_eq!(int(&other, "SELECT count(*) FROM t"), 60);
}

/// `sqlite3PagerCommitPhaseOne`: a commit on an in-memory source restarts its backups (C does not
/// feed the changed pages of an in-memory database to the backup). Trace of scenario T.
#[test]
fn commit_on_an_in_memory_source_restarts_the_backup() {
    let src = Connection::open_in_memory().unwrap();
    fill(&src, 4096, 60);
    let dst = Connection::open_in_memory().unwrap();
    dst.execute("ATTACH ':memory:' AS aux").unwrap();
    let mut b = Backup::init(&src, "main", &dst, "aux").unwrap();
    assert_eq!(step(&mut b, 5), (0, 12, 17));
    src.execute("INSERT INTO t VALUES(9000, zeroblob(3000))")
        .unwrap();
    assert_eq!(step(&mut b, 6), (0, 12, 18)); // restarted at page 1: 18 + 1 - 7
    assert_eq!(step(&mut b, 6), (0, 6, 18));
    assert_eq!(step(&mut b, 6), (101, 0, 18));
    b.finish().unwrap();
    assert_eq!(int(&dst, "SELECT count(*) FROM aux.t"), 61);
}

/// A commit through the source handle of a FILE database does not restart the backup: the pages
/// it changed are brought up to date and the copy goes on (scenario C).
#[test]
fn commit_on_a_file_source_does_not_restart_the_backup() {
    let file = TempDb::new("file_src");
    let src = Connection::open(&file.0).unwrap();
    fill(&src, 4096, 60);
    let dst = Connection::open_in_memory().unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(step(&mut b, 5), (0, 12, 17));
    src.execute(
        "BEGIN; INSERT INTO t SELECT id+1000, zeroblob(1000) FROM t WHERE id<=20; \
         UPDATE t SET v=zeroblob(999) WHERE id<=3; COMMIT",
    )
    .unwrap();
    assert_eq!(step(&mut b, 5), (0, 12, 22));
    src.execute("UPDATE t SET v=zeroblob(998) WHERE id=1")
        .unwrap();
    assert_eq!(step(&mut b, 7), (0, 5, 22));
    assert_eq!(step(&mut b, 7), (101, 0, 22));
    b.finish().unwrap();
    assert_eq!(int(&dst, "SELECT count(*) FROM t"), 80);
    assert_eq!(int(&dst, "SELECT sum(length(v)) FROM t"), 79996);
}

/// `sqlite3BtreeUpdateMeta(pDest, 1, iDestSchema+1)`: the copy's schema cookie is the
/// destination's previous cookie plus one, not the source's cookie.
#[test]
fn destination_schema_cookie_is_its_previous_cookie_plus_one() {
    let src = Connection::open_in_memory().unwrap();
    src.execute(
        "CREATE TABLE a(x); CREATE TABLE b(x); CREATE TABLE c(x); CREATE INDEX ci ON c(x);",
    )
    .unwrap();
    assert_eq!(int(&src, "PRAGMA schema_version"), 4);

    let dst = Connection::open_in_memory().unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(b.step(-1).unwrap(), BackupStepResult::Done);
    b.finish().unwrap();
    assert_eq!(int(&dst, "PRAGMA schema_version"), 1);
    assert_eq!(int(&dst, "SELECT count(*) FROM sqlite_master"), 4);

    let dst = Connection::open_in_memory().unwrap();
    dst.execute("CREATE TABLE d(x); CREATE TABLE e(x);")
        .unwrap();
    let mut b = Backup::init(&src, "main", &dst, "main").unwrap();
    assert_eq!(b.step(-1).unwrap(), BackupStepResult::Done);
    b.finish().unwrap();
    assert_eq!(int(&dst, "PRAGMA schema_version"), 3);
    assert_eq!(int(&dst, "SELECT count(*) FROM sqlite_master"), 4);
}
