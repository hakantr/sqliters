//! Regression test suite verifying fixes for all original findings (1..9) and re-review findings (Y1..Y12) in `bulgular.md`.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqliters::types::extended;
use sqliters::{
    Connection, ErrorCode, LockLevel, MemoryVfs, OpenFlags, OsVfs, SqliteError, SyncFlags, Value,
    Vfs, VfsFile,
};

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(prefix: &str) -> Self {
        let id = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sqliters_bulgu_{prefix}_{pid}_{id}.db"));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-journal", path.display()));
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn journal_path(&self) -> PathBuf {
        PathBuf::from(format!("{}-journal", self.path.display()))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(format!("{}-journal", self.path.display()));
        let _ = fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

fn run_c_sqlite3(db_path: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg("-list")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("failed to invoke /usr/bin/sqlite3");
    assert!(
        out.status.success(),
        "C sqlite3 failed on SQL `{sql}`:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("valid utf8 from sqlite3")
        .trim()
        .to_owned()
}

struct InteractiveSqlProc {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl InteractiveSqlProc {
    fn spawn_c_sqlite3(db_path: &Path) -> Self {
        let mut child = Command::new("sqlite3")
            .arg("-batch")
            .arg("-noheader")
            .arg("-list")
            .arg(db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sqlite3");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn spawn_child_test_writer(db_path: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().expect("current test binary"))
            .arg("--exact")
            .arg("test_bulgu_2_active_journal_not_clobbered_and_crashed_hot_journal_recovered")
            .arg("--nocapture")
            .env("SQLITERS_TEST_WRITER_DB", db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn child writer process");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin: Some(stdin),
            stdout,
        }
    }

    fn wait_marker(&mut self, marker: &str) {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(
                n > 0,
                "child process exited before printing marker '{marker}'"
            );
            if line.trim() == marker {
                break;
            }
        }
    }

    fn send_and_wait_marker(&mut self, sql_lines: &str, marker: &str) {
        let sin = self.stdin.as_mut().expect("stdin open");
        writeln!(sin, "{sql_lines}").unwrap();
        writeln!(sin, "SELECT '{marker}';").unwrap();
        sin.flush().unwrap();

        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(
                n > 0,
                "child process exited before printing marker '{marker}'"
            );
            if line.trim() == marker {
                break;
            }
        }
    }

    fn finish(mut self, final_sql: &str) -> String {
        if let Some(mut sin) = self.stdin.take() {
            let _ = writeln!(sin, "{final_sql}");
            let _ = sin.flush();
        }
        let mut rest = String::new();
        let _ = std::io::Read::read_to_string(&mut self.stdout, &mut rest);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "child sql process exited with {status}");
        rest
    }
}

impl Drop for InteractiveSqlProc {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ============================================================================
// Finding 1: POSIX `fcntl` OS locks & multi-handle `MemoryVfs` lock enforcement
// ============================================================================

#[test]
fn test_bulgu_1_cross_process_posix_locks_and_in_process_busy() {
    let tmp = TempDb::new("f1_locks");
    run_c_sqlite3(
        tmp.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT); INSERT INTO t VALUES (1, 'init');",
    );

    // 1. C `sqlite3` holds a RESERVED lock (`BEGIN IMMEDIATE`):
    //    - `sqliters` concurrent write MUST fail with `ErrorCode::Busy` (`SQLITE_BUSY`)
    //    - `sqliters` concurrent read (`SHARED` lock) MUST succeed!
    let mut c_writer = InteractiveSqlProc::spawn_c_sqlite3(tmp.path());
    c_writer.send_and_wait_marker(
        "BEGIN IMMEDIATE;\nINSERT INTO t VALUES (2, 'from_c');",
        "LOCKED_IMMEDIATE",
    );

    {
        let conn = Connection::open(tmp.path()).unwrap();
        let read_res = conn
            .query("SELECT id, val FROM t ORDER BY id;", &[])
            .unwrap();
        assert_eq!(read_res.rows.len(), 1);
        assert_eq!(read_res.rows[0].get_str(1), Some("init"));

        let write_err = conn
            .execute("INSERT INTO t VALUES (3, 'conflict_from_rust');")
            .unwrap_err();
        assert_eq!(write_err.code, ErrorCode::Busy);
    }

    c_writer.finish("COMMIT;");
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(run_c_sqlite3(tmp.path(), "SELECT count(*) FROM t;"), "2");

    // 2. C `sqlite3` holds an EXCLUSIVE lock (`BEGIN EXCLUSIVE`):
    //    - `Connection::open` defers schema load gracefully, and `SELECT` returns `ErrorCode::Busy`!
    let mut c_excl = InteractiveSqlProc::spawn_c_sqlite3(tmp.path());
    c_excl.send_and_wait_marker("BEGIN EXCLUSIVE;", "LOCKED_EXCLUSIVE");

    let conn2 = Connection::open(tmp.path()).unwrap();
    let sel_err = conn2.query("SELECT * FROM t;", &[]).unwrap_err();
    assert_eq!(sel_err.code, ErrorCode::Busy);

    c_excl.finish("COMMIT;");
    let after_unlock = conn2.query("SELECT count(*) FROM t;", &[]).unwrap();
    assert_eq!(after_unlock.rows[0].get_i64(0), Some(2));

    // 3. Multi-handle `MemoryVfs` also enforces `ErrorCode::Busy` on concurrent writers
    let mem_vfs = Arc::new(MemoryVfs::new());
    let db_virtual_path = Path::new("/mem/shared.db");
    let flags = OpenFlags::READWRITE | OpenFlags::CREATE;
    let m1 = Connection::open_with_vfs(mem_vfs.clone(), Some(db_virtual_path), flags).unwrap();
    m1.execute("CREATE TABLE items(x INT); INSERT INTO items VALUES (10);")
        .unwrap();
    let m2 = Connection::open_with_vfs(mem_vfs, Some(db_virtual_path), flags).unwrap();

    m1.execute("BEGIN IMMEDIATE; INSERT INTO items VALUES (20);")
        .unwrap();
    let m2_err = m2.execute("INSERT INTO items VALUES (30);").unwrap_err();
    assert_eq!(m2_err.code, ErrorCode::Busy);
    m1.execute("COMMIT;").unwrap();
    assert_eq!(
        m2.query_row("SELECT count(*) FROM items;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(2)
    );
}

// ============================================================================
// Finding 2: Active `-journal` protected via `RESERVED` lock check vs crashed hot journal
// ============================================================================

#[test]
fn test_bulgu_2_active_journal_not_clobbered_and_crashed_hot_journal_recovered() {
    // Child helper entrypoint for cross-process RESERVED lock + live `-journal` test
    if let Some(db_os) = std::env::var_os("SQLITERS_TEST_WRITER_DB") {
        let conn = Connection::open(Path::new(&db_os)).unwrap();
        conn.execute("BEGIN IMMEDIATE; UPDATE accounts SET balance = 999 WHERE id = 1;")
            .unwrap();
        println!("WRITER_ACTIVE");
        std::io::stdout().flush().unwrap();

        let mut cmd = String::new();
        std::io::stdin().read_line(&mut cmd).unwrap();
        conn.execute(cmd.trim()).unwrap();
        return;
    }

    let tmp = TempDb::new("f2_journal");
    run_c_sqlite3(
        tmp.path(),
        "CREATE TABLE accounts(id INTEGER PRIMARY KEY, balance INT); INSERT INTO accounts VALUES (1, 100);",
    );

    // Spawn a separate OS writer process that enters `BEGIN IMMEDIATE` and modifies a page,
    // creating a real live `<db>-journal` on disk while holding a POSIX `RESERVED` lock!
    let mut writer_proc = InteractiveSqlProc::spawn_child_test_writer(tmp.path());
    writer_proc.wait_marker("WRITER_ACTIVE");
    assert!(
        tmp.journal_path().exists(),
        "active writer must have created -journal on disk"
    );

    // Capture a byte-for-byte snapshot of `(db, -journal)` mid-transaction to simulate a SIGKILL crash
    // on a separate path later (with zero locks held on the copy).
    let crash_tmp = TempDb::new("f2_crashed_copy");
    fs::copy(tmp.path(), crash_tmp.path()).unwrap();
    fs::copy(tmp.journal_path(), crash_tmp.journal_path()).unwrap();

    // Now open `tmp.path()` in a reader connection while `writer_proc` is STILL active:
    // MUST NOT delete or roll back `tmp.journal_path()`!
    {
        let reader = Connection::open(tmp.path()).unwrap();
        let row = reader
            .query_row("SELECT balance FROM accounts WHERE id = 1;", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64(0), Some(100));
    }
    assert!(
        tmp.journal_path().exists(),
        "reader must NEVER delete an active writer's -journal!"
    );

    // Let the active writer ROLLBACK its uncommitted transaction using its intact `-journal`!
    writer_proc.finish("ROLLBACK;");
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT balance FROM accounts WHERE id = 1;"),
        "100"
    );
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");

    // Now test crashed hot-journal recovery on `crash_tmp` where NO process holds a lock:
    // Corrupt page 2 on `crash_tmp` (simulating partial page write before crash; original page 2 is in `-journal`!)
    {
        let mut raw = fs::read(crash_tmp.path()).unwrap();
        let page_size = u16::from_be_bytes([raw[16], raw[17]]) as usize;
        if raw.len() >= page_size * 2 {
            // Overwrite bytes inside page 2 (table leaf page); hot journal replay will restore original page 2!
            for b in &mut raw[page_size + 16..page_size + 64] {
                *b = 0xAA;
            }
            fs::write(crash_tmp.path(), &raw).unwrap();
        }
    }
    assert!(crash_tmp.journal_path().exists());

    let recovered = Connection::open(crash_tmp.path()).unwrap();
    let bal = recovered
        .query_row("SELECT balance FROM accounts WHERE id = 1;", &[])
        .unwrap()
        .unwrap()
        .get_i64(0);
    assert_eq!(bal, Some(100));
    // (C: the journal of a transaction that had not synced it yet starts with a zero byte, so
    // it is not hot — `hasHotJournal` — and is left where it is; the database file was not
    // touched by that transaction.)
    assert!(
        crash_tmp.journal_path().exists(),
        "a journal whose header was never synced is not hot and stays in place"
    );
    drop(recovered);
    assert_eq!(
        run_c_sqlite3(crash_tmp.path(), "PRAGMA integrity_check;"),
        "ok"
    );
}

// ============================================================================
// Finding 3: WAL recovery & visibility (`(2, 2)` + uncheckpointed `-wal` frames)
//            and unknown Format 3 version guards (`btree.c:3356-3386`)
// ============================================================================

#[test]
fn test_bulgu_3_wal_database_recovered_and_unknown_format_versions_guarded() {
    // ------------------------------------------------------------------------
    // Part A: Real WAL visibility (uncheckpointed `-wal` frames, never stale `.db`)
    // ------------------------------------------------------------------------
    let src_tmp = TempDb::new("f3_wal_src");
    let wal_tmp = TempDb::new("f3_wal_snap");

    // Seed `.db` with `(10)` in WAL mode (last-connection close backfills `10` into `.db`).
    run_c_sqlite3(
        src_tmp.path(),
        "PRAGMA journal_mode = WAL; CREATE TABLE t(x INT); INSERT INTO t VALUES (10);",
    );

    // Keep an interactive C SQLite 3.54.1 writer open with `wal_autocheckpoint = 0` so
    // `UPDATE 10 -> 11` and `INSERT 42` commit strictly into `-wal` while `.db` remains at `(10)`!
    let mut c_writer = InteractiveSqlProc::spawn_c_sqlite3(src_tmp.path());
    c_writer.send_and_wait_marker(
        "PRAGMA wal_autocheckpoint = 0; UPDATE t SET x = 11 WHERE x = 10; INSERT INTO t VALUES (42);",
        "F3_WAL_COMMITTED",
    );

    let src_wal = PathBuf::from(format!("{}-wal", src_tmp.path().display()));
    let dst_wal = PathBuf::from(format!("{}-wal", wal_tmp.path().display()));
    assert!(
        src_wal.exists() && fs::metadata(&src_wal).unwrap().len() > 32,
        "expected uncheckpointed committed frames in -wal"
    );
    fs::copy(src_tmp.path(), wal_tmp.path()).unwrap();
    fs::copy(&src_wal, &dst_wal).unwrap();
    let _ = c_writer.finish(".quit");

    // `Connection::open` succeeds lazily; first locked read recovers `-wal` and sees `(11, 42)`, NOT stale `(10)`!
    let conn = Connection::open(wal_tmp.path()).expect("lazy open succeeds without reading header");
    let qres = conn.query("SELECT x FROM t ORDER BY x;", &[]).unwrap();
    assert_eq!(qres.rows.len(), 2);
    assert_eq!(qres.rows[0].values(), &[Value::Integer(11)]);
    assert_eq!(qres.rows[1].values(), &[Value::Integer(42)]);

    let hdr = conn.database_header().unwrap();
    assert_eq!((hdr.write_version, hdr.read_version), (2, 2));

    let jmode = conn
        .query_row("PRAGMA journal_mode;", &[])
        .unwrap()
        .unwrap()
        .get_str(0)
        .unwrap()
        .to_owned();
    assert_eq!(jmode, "wal");

    // Rust `INSERT` commits cleanly into WAL, and C SQLite `PRAGMA integrity_check` verifies `"ok"`!
    conn.execute("INSERT INTO t VALUES (99);").unwrap();
    conn.try_close().unwrap();

    assert_eq!(
        run_c_sqlite3(wal_tmp.path(), "PRAGMA integrity_check;"),
        "ok"
    );
    assert_eq!(
        run_c_sqlite3(wal_tmp.path(), "SELECT x FROM t ORDER BY x;"),
        "11\n42\n99"
    );

    // ------------------------------------------------------------------------
    // Part B: Format 3 version guards (`sqlite/src/btree.c:3356-3386`)
    // ------------------------------------------------------------------------
    // B1) `read_version > 2` (byte `19 = 3`) -> `ErrorCode::NotADb` on first locked read!
    let bad_rv = TempDb::new("f3_bad_rv");
    run_c_sqlite3(
        bad_rv.path(),
        "CREATE TABLE t(x INT); INSERT INTO t VALUES (7);",
    );
    let mut bad_bytes = fs::read(bad_rv.path()).unwrap();
    bad_bytes[19] = 3;
    fs::write(bad_rv.path(), &bad_bytes).unwrap();

    let c_bad = Connection::open(bad_rv.path()).expect("lazy open succeeds");
    assert_eq!(
        c_bad.query("SELECT * FROM t;", &[]).unwrap_err().code,
        ErrorCode::NotADb
    );
    assert_eq!(c_bad.database_header().unwrap_err().code, ErrorCode::NotADb);
    c_bad.try_close().unwrap();

    // B2) `write_version > 2` with `read_version <= 2` (bytes `18..=19 = [3, 2]`)
    //     -> opens read-only and rejects writes with `ErrorCode::ReadOnly`!
    let ro_wv = TempDb::new("f3_ro_wv");
    run_c_sqlite3(
        ro_wv.path(),
        "PRAGMA journal_mode = WAL; CREATE TABLE t(x INT); INSERT INTO t VALUES (77);",
    );
    let mut ro_bytes = fs::read(ro_wv.path()).unwrap();
    ro_bytes[18] = 3;
    ro_bytes[19] = 2;
    fs::write(ro_wv.path(), &ro_bytes).unwrap();

    let c_ro = Connection::open(ro_wv.path()).expect("lazy open succeeds");
    let ro_row = c_ro
        .query_row("SELECT x FROM t;", &[])
        .unwrap()
        .unwrap()
        .get_i64(0);
    assert_eq!(ro_row, Some(77));
    let ro_hdr = c_ro.database_header().unwrap();
    assert_eq!((ro_hdr.write_version, ro_hdr.read_version), (3, 2));
    assert_eq!(
        c_ro.execute("INSERT INTO t VALUES (88);").unwrap_err().code,
        ErrorCode::ReadOnly
    );
    c_ro.try_close().unwrap();
}

// ============================================================================
// Finding 4: `PRAGMA foreign_keys = ON` enforcement, actions, and deferred FKs
// ============================================================================

#[test]
fn test_bulgu_4_foreign_keys_enforcement_actions_and_deferred() {
    let conn = Connection::open_in_memory().unwrap();

    // Exact reproducer from `bulgular.md`
    conn.execute(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE p(id INTEGER PRIMARY KEY);
         CREATE TABLE c(pid INTEGER REFERENCES p(id));",
    )
    .unwrap();

    let fk_err = conn.execute("INSERT INTO c VALUES (99);").unwrap_err();
    assert_eq!(fk_err.code, ErrorCode::Constraint);
    assert!(fk_err.message.contains("FOREIGN KEY constraint failed"));

    // NULL child keys are permitted (`MATCH SIMPLE`), valid parent keys succeed, parent delete blocked
    conn.execute("INSERT INTO p VALUES (1), (2); INSERT INTO c VALUES (1), (NULL);")
        .unwrap();
    let del_parent_err = conn.execute("DELETE FROM p WHERE id = 1;").unwrap_err();
    assert_eq!(del_parent_err.code, ErrorCode::Constraint);
    // Unreferenced parent row can be deleted
    conn.execute("DELETE FROM p WHERE id = 2;").unwrap();

    // Test ON DELETE CASCADE, ON UPDATE CASCADE, ON DELETE SET NULL, ON DELETE SET DEFAULT
    conn.execute(
        "CREATE TABLE orgs(id INTEGER PRIMARY KEY, code TEXT UNIQUE);
         CREATE TABLE teams(
             id INTEGER PRIMARY KEY,
             org_code TEXT REFERENCES orgs(code) ON UPDATE CASCADE ON DELETE CASCADE
         );
         CREATE TABLE members(
             id INTEGER PRIMARY KEY,
             team_id INTEGER REFERENCES teams(id) ON DELETE SET NULL,
             fallback_org INTEGER DEFAULT 1 REFERENCES orgs(id) ON DELETE SET DEFAULT
         );
         INSERT INTO orgs VALUES (1, 'DEFAULT_ORG'), (2, 'ENG');
         INSERT INTO teams VALUES (10, 'ENG');
         INSERT INTO members VALUES (100, 10, 2);",
    )
    .unwrap();

    // ON UPDATE CASCADE propagates `'ENG'` -> `'RND'` to `teams.org_code`
    conn.execute("UPDATE orgs SET code = 'RND' WHERE id = 2;")
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT org_code FROM teams WHERE id = 10;", &[])
            .unwrap()
            .unwrap()
            .get_str(0),
        Some("RND")
    );

    // Deleting org 2:
    //  - cascades delete to `teams(10)`
    //  - `teams(10)` deletion sets `members(100).team_id = NULL` (`ON DELETE SET NULL`)
    //  - `orgs(2)` deletion sets `members(100).fallback_org = 1` (`ON DELETE SET DEFAULT`)
    conn.execute("DELETE FROM orgs WHERE id = 2;").unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM teams;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(0)
    );
    let mrow = conn
        .query_row(
            "SELECT team_id, fallback_org FROM members WHERE id = 100;",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(mrow.get(0), Some(&Value::Null));
    assert_eq!(mrow.get_i64(1), Some(1));

    // Test DEFERRABLE INITIALLY DEFERRED inside explicit transaction
    conn.execute(
        "CREATE TABLE parent_def(id INTEGER PRIMARY KEY);
         CREATE TABLE child_def(pid INTEGER REFERENCES parent_def(id) DEFERRABLE INITIALLY DEFERRED);",
    )
    .unwrap();

    conn.execute("BEGIN; INSERT INTO child_def VALUES (500);")
        .unwrap();
    // Committing while still orphaned must fail with FOREIGN KEY constraint failed AND keep tx active!
    let commit_err = conn.execute("COMMIT;").unwrap_err();
    assert_eq!(commit_err.code, ErrorCode::Constraint);
    assert!(!conn.is_autocommit());

    // Inserting missing parent makes COMMIT succeed!
    conn.execute("INSERT INTO parent_def VALUES (500); COMMIT;")
        .unwrap();
    assert!(conn.is_autocommit());
}

// ============================================================================
// Finding 5: `OR FAIL` and `OR ROLLBACK` conflict semantics
// ============================================================================

#[test]
fn test_bulgu_5_or_fail_and_or_rollback_semantics() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. Exact `OR FAIL` reproducer inside explicit transaction: keeps `(1), (2), (3)` and stays in tx!
    conn.execute("CREATE TABLE t(x INT UNIQUE); INSERT INTO t VALUES (1);")
        .unwrap();
    conn.execute("BEGIN; INSERT INTO t VALUES (2);").unwrap();

    let fail_err = conn
        .execute("INSERT OR FAIL INTO t VALUES (3), (1), (4);")
        .unwrap_err();
    assert_eq!(fail_err.code, ErrorCode::Constraint);
    assert!(
        !conn.is_autocommit(),
        "OR FAIL must keep enclosing explicit transaction open"
    );
    assert_eq!(
        conn.changes(),
        1,
        "row (3) written before conflict must count"
    );

    conn.execute("COMMIT;").unwrap();
    let vals: Vec<i64> = conn
        .query("SELECT x FROM t ORDER BY x;", &[])
        .unwrap()
        .rows
        .iter()
        .map(|r| r.get_i64(0).unwrap())
        .collect();
    assert_eq!(vals, vec![1, 2, 3]);

    // 2. `OR FAIL` in autocommit mode also persists rows written prior to the failing row!
    let ac_fail = conn
        .execute("INSERT OR FAIL INTO t VALUES (10), (2), (20);")
        .unwrap_err();
    assert_eq!(ac_fail.code, ErrorCode::Constraint);
    assert!(conn.is_autocommit());
    let vals_after_ac: Vec<i64> = conn
        .query("SELECT x FROM t ORDER BY x;", &[])
        .unwrap()
        .rows
        .iter()
        .map(|r| r.get_i64(0).unwrap())
        .collect();
    assert_eq!(vals_after_ac, vec![1, 2, 3, 10]);

    // 3. Exact `OR ROLLBACK` reproducer inside explicit transaction: rolls back ENTIRE transaction!
    conn.execute("BEGIN; INSERT INTO t VALUES (50);").unwrap();
    assert!(!conn.is_autocommit());

    let rb_err = conn
        .execute("INSERT OR ROLLBACK INTO t VALUES (60), (1);")
        .unwrap_err();
    assert_eq!(rb_err.code, ErrorCode::Constraint);
    assert!(
        conn.is_autocommit(),
        "OR ROLLBACK must abort the entire enclosing transaction and return to autocommit"
    );

    let vals_after_rb: Vec<i64> = conn
        .query("SELECT x FROM t ORDER BY x;", &[])
        .unwrap()
        .rows
        .iter()
        .map(|r| r.get_i64(0).unwrap())
        .collect();
    assert_eq!(vals_after_rb, vec![1, 2, 3, 10]);
}

// ============================================================================
// Finding 6: `ALTER TABLE RENAME COLUMN` on indexed, constrained & FK columns
// ============================================================================

#[test]
fn test_bulgu_6_alter_table_rename_column_updates_indexes_constraints_and_fks() {
    let tmp = TempDb::new("f6_alter");
    {
        let conn = Connection::open(tmp.path()).unwrap();

        // Exact reproducer from `bulgular.md` + partial index + CHECK + child FK!
        conn.execute(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE t(a INT CHECK(a > 0), label TEXT, UNIQUE(a));
             CREATE INDEX ix ON t(a);
             CREATE INDEX ix_partial ON t(label) WHERE a >= 10;
             CREATE TABLE child(id INTEGER PRIMARY KEY, ref_a INT REFERENCES t(a));
             INSERT INTO t VALUES (5, 'bes'), (15, 'onbes');
             INSERT INTO child VALUES (1, 15);",
        )
        .unwrap();

        conn.execute("ALTER TABLE t RENAME COLUMN a TO b;").unwrap();

        // Verify queries, index seek, CHECK constraint, and FK still work on renamed column `b`
        let res = conn
            .query("SELECT b, label FROM t WHERE b = 15;", &[])
            .unwrap();
        assert_eq!(res.column_names, vec!["b", "label"]);
        assert_eq!(res.rows.len(), 1);
        assert_eq!(res.rows[0].get_str(1), Some("onbes"));

        let chk_err = conn
            .execute("INSERT INTO t VALUES (-3, 'neg');")
            .unwrap_err();
        assert_eq!(chk_err.code, ErrorCode::Constraint);

        let fk_err = conn
            .execute("INSERT INTO child VALUES (2, 999);")
            .unwrap_err();
        assert_eq!(fk_err.code, ErrorCode::Constraint);
    }

    // Verify C `sqlite3` reads the rewritten schema and passes `PRAGMA integrity_check`!
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT b FROM t ORDER BY b;"),
        "5\n15"
    );
}

// ============================================================================
// Finding 7: Real UTC `CURRENT_DATE`, `CURRENT_TIME`, `CURRENT_TIMESTAMP`
// ============================================================================

#[test]
fn test_bulgu_7_current_timestamp_uses_real_utc_clock() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE dual_rows(id INT); INSERT INTO dual_rows VALUES (1), (2);")
        .unwrap();

    let before_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Test both VDBE path (table scan) and materialized path (no-FROM / aggregate)
    let vdbe_res = conn
        .query(
            "SELECT CURRENT_DATE, CURRENT_TIME, CURRENT_TIMESTAMP FROM dual_rows;",
            &[],
        )
        .unwrap();
    assert_eq!(vdbe_res.rows.len(), 2);
    // Within a single statement, both rows must observe the exact same statement timestamp
    assert_eq!(vdbe_res.rows[0], vdbe_res.rows[1]);

    let mat_row = conn
        .query_row(
            "SELECT CURRENT_DATE, CURRENT_TIME, CURRENT_TIMESTAMP, unixepoch();",
            &[],
        )
        .unwrap()
        .unwrap();

    let after_sec = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let cdate = mat_row.get_str(0).unwrap();
    let ctime = mat_row.get_str(1).unwrap();
    let cts = mat_row.get_str(2).unwrap();
    let uepoch = mat_row.get_i64(3).unwrap();

    assert!(
        uepoch >= before_sec - 2 && uepoch <= after_sec + 2,
        "unixepoch() {uepoch} outside system UTC window [{before_sec}, {after_sec}]"
    );
    assert_eq!(cts, format!("{cdate} {ctime}"));
    assert_ne!(
        cts, "2026-11-08 00:00:00",
        "CURRENT_TIMESTAMP must not be hardcoded"
    );
    assert_eq!(vdbe_res.rows[0].get_str(0), Some(cdate));
}

// ============================================================================
// Finding 8: `UTF-16le` & `UTF-16be` SQLite file interoperability
// ============================================================================

#[test]
fn test_bulgu_8_utf16le_and_utf16be_bidirectional_interoperability() {
    for (enc_name, label) in [("UTF-16le", "le"), ("UTF-16be", "be")] {
        // 1. C `sqlite3` creates a UTF-16 database with Turkish & emoji text + UNIQUE index
        let tmp_c = TempDb::new(&format!("f8_c_{label}"));
        run_c_sqlite3(
            tmp_c.path(),
            &format!(
                "PRAGMA encoding = '{enc_name}';
                 CREATE TABLE t(id INTEGER PRIMARY KEY, s TEXT UNIQUE);
                 INSERT INTO t VALUES (1, 'Türkçe'), (2, 'Çağdaş Şehir 🦀');"
            ),
        );

        {
            let conn = Connection::open(tmp_c.path()).unwrap();
            let enc_pragma = conn
                .query_row("PRAGMA encoding;", &[])
                .unwrap()
                .unwrap()
                .get_str(0)
                .unwrap()
                .to_owned();
            assert_eq!(enc_pragma, enc_name);

            // Read via table scan (VDBE) and via index seek (`WHERE s = ...`)
            let all = conn.query("SELECT id, s FROM t ORDER BY id;", &[]).unwrap();
            assert_eq!(all.rows.len(), 2);
            assert_eq!(all.rows[0].get_str(1), Some("Türkçe"));
            assert_eq!(all.rows[1].get_str(1), Some("Çağdaş Şehir 🦀"));

            let seek = conn
                .query(
                    "SELECT id FROM t WHERE s = ?1;",
                    &[Value::Text("Türkçe".into())],
                )
                .unwrap();
            assert_eq!(seek.rows.len(), 1);
            assert_eq!(seek.rows[0].get_i64(0), Some(1));

            // Mutate UTF-16 database from `sqliters` and run `VACUUM`!
            conn.execute("INSERT INTO t VALUES (3, 'Ğüşiöç İı'); VACUUM;")
                .unwrap();
        }

        // Verify C `sqlite3` passes `PRAGMA integrity_check` and reads all UTF-16 strings back!
        assert_eq!(run_c_sqlite3(tmp_c.path(), "PRAGMA integrity_check;"), "ok");
        assert_eq!(run_c_sqlite3(tmp_c.path(), "PRAGMA encoding;"), enc_name);
        assert_eq!(
            run_c_sqlite3(tmp_c.path(), "SELECT s FROM t WHERE id = 3;"),
            "Ğüşiöç İı"
        );

        // 2. `sqliters` creates a brand-new UTF-16 database from scratch; C `sqlite3` verifies it!
        let tmp_r = TempDb::new(&format!("f8_r_{label}"));
        {
            let conn = Connection::open(tmp_r.path()).unwrap();
            conn.execute(&format!(
                "PRAGMA encoding = '{enc_name}';
                 CREATE TABLE cities(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 CREATE INDEX idx_cities_name ON cities(name);
                 INSERT INTO cities VALUES (1, 'İstanbul'), (2, 'Çanakkale'), (3, 'Şanlıurfa');"
            ))
            .unwrap();
        }
        assert_eq!(run_c_sqlite3(tmp_r.path(), "PRAGMA integrity_check;"), "ok");
        assert_eq!(run_c_sqlite3(tmp_r.path(), "PRAGMA encoding;"), enc_name);
        assert_eq!(
            run_c_sqlite3(tmp_r.path(), "SELECT name FROM cities ORDER BY id;"),
            "İstanbul\nÇanakkale\nŞanlıurfa"
        );
    }
}

// ============================================================================
// Finding 9: `Connection::query()` & `Statement::column_names()` on materialized queries
// ============================================================================

#[test]
fn test_bulgu_9_query_and_statement_column_names_populated_everywhere() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a INT, b TEXT); INSERT INTO t VALUES (1, 'x');")
        .unwrap();

    // 1. VDBE simple scan
    let r1 = conn.query("SELECT a FROM t;", &[]).unwrap();
    assert_eq!(r1.column_names, vec!["a"]);

    // 2. Aggregate query (materialized engine path) — exact reproducer from `bulgular.md`
    let r2 = conn.query("SELECT count(*) FROM t;", &[]).unwrap();
    assert_eq!(r2.column_names, vec!["count(*)"]);

    // 3. Compound `UNION ALL` (materialized engine path) — exact reproducer from `bulgular.md`
    let r3 = conn
        .query("SELECT a FROM t UNION ALL SELECT a FROM t;", &[])
        .unwrap();
    assert_eq!(r3.column_names, vec!["a"]);
    assert_eq!(r3.rows.len(), 2);

    // 4. `Statement::column_names()` available BEFORE `step()` on materialized queries & 0-row queries!
    {
        let stmt_agg = conn
            .prepare("SELECT count(*) AS total, max(b) FROM t WHERE a > 999;")
            .unwrap();
        assert_eq!(stmt_agg.column_names(), &["total", "max(b)"]);
    }
    {
        let stmt_join = conn
            .prepare("SELECT l.a, r.b AS rb FROM t l JOIN t r ON l.a = r.a WHERE 1 = 0;")
            .unwrap();
        assert_eq!(stmt_join.column_names(), &["a", "rb"]);
    }
    {
        let stmt_ret = conn
            .prepare("INSERT INTO t VALUES (2, 'y') RETURNING a, b AS label;")
            .unwrap();
        assert_eq!(stmt_ret.column_names(), &["a", "label"]);
    }
    {
        let r_vals = conn.query("VALUES (10, 20), (30, 40);", &[]).unwrap();
        assert_eq!(r_vals.column_names, vec!["column1", "column2"]);
    }
}

// ============================================================================
// Finding Y1: Failed `COMMIT` I/O (`write_at`, `truncate`, `sync`, `delete`) + `ROLLBACK` atomicity
// ============================================================================

#[derive(Default)]
struct FaultState {
    main_writes_seen: usize,
    fail_on_main_write_nth: Option<usize>,
    fail_all_main_writes: bool,
    fail_next_truncate: bool,
    fail_next_main_sync: bool,
    fail_next_journal_delete: bool,
}

struct FaultVfs {
    inner: OsVfs,
    state: Arc<std::sync::Mutex<FaultState>>,
}

impl FaultVfs {
    fn new() -> (Arc<Self>, Arc<std::sync::Mutex<FaultState>>) {
        let state = Arc::new(std::sync::Mutex::new(FaultState::default()));
        (
            Arc::new(Self {
                inner: OsVfs::new(),
                state: Arc::clone(&state),
            }),
            state,
        )
    }
}

struct FaultFile {
    inner: Box<dyn VfsFile>,
    is_main_db: bool,
    state: Arc<std::sync::Mutex<FaultState>>,
}

impl VfsFile for FaultFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> sqliters::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> sqliters::Result<()> {
        if self.is_main_db {
            let mut st = self.state.lock().unwrap();
            if st.fail_all_main_writes {
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected persistent main DB write error",
                ));
            }
            st.main_writes_seen += 1;
            if st.fail_on_main_write_nth == Some(st.main_writes_seen) {
                st.fail_on_main_write_nth = None;
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected one-shot main DB write_at error",
                ));
            }
        }
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> sqliters::Result<()> {
        if self.is_main_db {
            let mut st = self.state.lock().unwrap();
            if st.fail_next_truncate {
                st.fail_next_truncate = false;
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected main DB truncate error",
                ));
            }
        }
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> sqliters::Result<()> {
        if self.is_main_db {
            let mut st = self.state.lock().unwrap();
            if st.fail_next_main_sync {
                st.fail_next_main_sync = false;
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected main DB sync error",
                ));
            }
        }
        self.inner.sync(flags)
    }

    fn file_size(&self) -> sqliters::Result<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> sqliters::Result<()> {
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> sqliters::Result<()> {
        self.inner.unlock(level)
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn check_reserved_lock(&self) -> sqliters::Result<bool> {
        self.inner.check_reserved_lock()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }
}

impl Vfs for FaultVfs {
    fn name(&self) -> &'static str {
        "fault-vfs"
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> sqliters::Result<Box<dyn VfsFile>> {
        let is_main_db = path.is_some_and(|p| !p.to_string_lossy().ends_with("-journal"))
            && !flags.contains(OpenFlags::MAIN_JOURNAL);
        let inner = self.inner.open(path, flags)?;
        Ok(Box::new(FaultFile {
            inner,
            is_main_db,
            state: Arc::clone(&self.state),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> sqliters::Result<()> {
        if path.to_string_lossy().ends_with("-journal") {
            let mut st = self.state.lock().unwrap();
            if st.fail_next_journal_delete {
                st.fail_next_journal_delete = false;
                return Err(SqliteError::new(
                    ErrorCode::IoErr,
                    "injected journal delete error",
                ));
            }
        }
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> sqliters::Result<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> sqliters::Result<PathBuf> {
        self.inner.full_pathname(path)
    }
}

#[test]
fn test_bulgu_y1_failed_commit_partial_writes_rolled_back_atomically() {
    let tmp = TempDb::new("y1_commit_ioerr");
    let (vfs, fault) = FaultVfs::new();
    let rw_flags = OpenFlags::READWRITE | OpenFlags::CREATE;

    // 1. Exact scenario from `bulgular.md` Y1: fail on the 3rd main-DB `write_at` during COMMIT, then ROLLBACK!
    {
        let conn = Connection::open_with_vfs(vfs.clone(), Some(tmp.path()), rw_flags).unwrap();
        conn.execute(
            "CREATE TABLE t1(x); CREATE TABLE t2(y);
             INSERT INTO t1 VALUES (10); INSERT INTO t2 VALUES (20);",
        )
        .unwrap();

        conn.execute("BEGIN; UPDATE t1 SET x = 11; UPDATE t2 SET y = 21;")
            .unwrap();
        {
            let mut st = fault.lock().unwrap();
            st.main_writes_seen = 0;
            st.fail_on_main_write_nth = Some(3);
        }

        let commit_err = conn.execute("COMMIT;").unwrap_err();
        assert_eq!(commit_err.code, ErrorCode::IoErr);
        assert!(
            !conn.is_autocommit(),
            "tx must remain active after failed COMMIT"
        );

        conn.execute("ROLLBACK;").unwrap();
        assert!(conn.is_autocommit());
    }

    // Reopen fresh from disk and verify BOTH Rust and C `sqlite3` see `(10, 20)` and `integrity_check = ok`!
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT (SELECT x FROM t1), (SELECT y FROM t2);"),
        "10|20"
    );
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");

    // 2. Test failure on `truncate`, `sync`, and `journal delete` during COMMIT followed by ROLLBACK!
    for mode in ["truncate", "sync", "delete"] {
        let conn = Connection::open_with_vfs(vfs.clone(), Some(tmp.path()), rw_flags).unwrap();
        conn.execute("BEGIN; UPDATE t1 SET x = 99; CREATE TABLE extra_tbl(z); INSERT INTO extra_tbl VALUES ('grow');")
            .unwrap();
        {
            let mut st = fault.lock().unwrap();
            match mode {
                "truncate" => {
                    // Drop or grow to trigger truncate on commit or rollback
                    st.fail_next_truncate = true;
                }
                "sync" => st.fail_next_main_sync = true,
                "delete" => st.fail_next_journal_delete = true,
                _ => unreachable!(),
            }
        }
        if mode == "truncate" {
            // Commit the growth first, then drop `extra_tbl` so `VACUUM` shrinks `page_count` and
            // triggers `file.truncate` inside `Pager::commit()`, and ALSO test `BEGIN ... COMMIT`
            // hitting `file.truncate` when trailing bytes exist on disk!
            fault.lock().unwrap().fail_next_truncate = false;
            conn.execute("COMMIT;").unwrap();
            conn.execute("DROP TABLE extra_tbl;").unwrap();

            // (a) `VACUUM` commit hitting `file.truncate` I/O error -> rolls back cleanly!
            fault.lock().unwrap().fail_next_truncate = true;
            let vac_err = conn.execute("VACUUM;").unwrap_err();
            assert_eq!(vac_err.code, ErrorCode::IoErr);
            assert!(conn.is_autocommit());

            // (b) Explicit `BEGIN ... COMMIT` hitting `file.truncate` I/O error -> `ROLLBACK` restores `(99, 20)`!
            let cur_len = fs::metadata(tmp.path()).unwrap().len();
            fs::OpenOptions::new()
                .write(true)
                .open(tmp.path())
                .unwrap()
                .set_len(cur_len + 4096)
                .unwrap();
            conn.execute("BEGIN; UPDATE t1 SET x = 77;").unwrap();
            fault.lock().unwrap().fail_next_truncate = true;
            let err = conn.execute("COMMIT;").unwrap_err();
            assert_eq!(err.code, ErrorCode::IoErr);
            // `ROLLBACK;` restores `t1 = 99` and truncates trailing bytes back to `cur_len`!
            conn.execute("ROLLBACK;").unwrap();
            drop(conn);
            assert_eq!(run_c_sqlite3(tmp.path(), "SELECT x FROM t1;"), "99");
            assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
            // Reset t1 back to 10 and compact cleanly
            let clean = Connection::open(tmp.path()).unwrap();
            clean.execute("UPDATE t1 SET x = 10; VACUUM;").unwrap();
        } else {
            let err = conn.execute("COMMIT;").unwrap_err();
            assert_eq!(err.code, ErrorCode::IoErr);
            conn.execute("ROLLBACK;").unwrap();
            drop(conn);
            assert_eq!(
                run_c_sqlite3(tmp.path(), "SELECT (SELECT x FROM t1), (SELECT y FROM t2);"),
                "10|20"
            );
            assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
        }
    }

    // 3. Test persistent disk write failure DURING `ROLLBACK` after partial `COMMIT`:
    //    `<db>-journal` MUST be preserved on disk so the next reopen recovers `(10, 20)`!
    {
        let conn = Connection::open_with_vfs(vfs.clone(), Some(tmp.path()), rw_flags).unwrap();
        conn.execute("BEGIN; UPDATE t1 SET x = 555; UPDATE t2 SET y = 666;")
            .unwrap();
        {
            let mut st = fault.lock().unwrap();
            st.main_writes_seen = 0;
            st.fail_on_main_write_nth = Some(2);
        }
        assert_eq!(conn.execute("COMMIT;").unwrap_err().code, ErrorCode::IoErr);

        // Inject persistent write failure so `ROLLBACK;` itself fails to restore `.db`
        fault.lock().unwrap().fail_all_main_writes = true;
        assert_eq!(
            conn.execute("ROLLBACK;").unwrap_err().code,
            ErrorCode::IoErr
        );
        drop(conn);
    }
    assert!(
        tmp.journal_path().exists(),
        "hot journal MUST be preserved when rollback disk writes fail!"
    );
    fault.lock().unwrap().fail_all_main_writes = false;

    // Reopening now recovers from the preserved hot journal!
    {
        let recovered = Connection::open(tmp.path()).unwrap();
        let row = recovered
            .query_row("SELECT (SELECT x FROM t1), (SELECT y FROM t2);", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64(0), Some(10));
        assert_eq!(row.get_i64(1), Some(20));
    }
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
}

// ============================================================================
// Finding Y2: Multi-header C SQLite spilled journal playback & sector-aligned Rust journal
// ============================================================================

#[test]
fn test_bulgu_y2_multi_header_spilled_journal_and_bidirectional_c_recovery() {
    // 1. Exact C SQLite crash reproducer from `bulgular.md` Y2:
    //    `page_size=1024`, 200 rows with 400-char padding, `cache_size=5; BEGIN IMMEDIATE; UPDATE t SET x=1`,
    //    kill C `sqlite3` before COMMIT so a multi-header spilled rollback journal is left on disk!
    let tmp_base = TempDb::new("y2_c_spill_base");
    run_c_sqlite3(
        tmp_base.path(),
        "PRAGMA page_size = 1024;
         CREATE TABLE t(id INTEGER PRIMARY KEY, x INT, pad TEXT);
         WITH RECURSIVE cnt(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM cnt WHERE i < 200)
         INSERT INTO t SELECT i, 0, zeroblob(0) || replace(hex(zeroblob(200)), '00', 'ab') FROM cnt;",
    );

    let mut c_proc = InteractiveSqlProc::spawn_c_sqlite3(tmp_base.path());
    c_proc.send_and_wait_marker(
        "PRAGMA cache_size = 5;\nBEGIN IMMEDIATE;\nUPDATE t SET x = 1;",
        "SPILLED_UNCOMMITTED",
    );
    // Kill C `sqlite3` abruptly (SIGKILL via `Child::kill`) before COMMIT!
    let _ = c_proc.child.kill();
    let _ = c_proc.child.wait();
    assert!(tmp_base.journal_path().exists());

    // Copy crashed `(db, -journal)` to two separate paths and recover one with C `sqlite3` and one with Rust!
    let copy_for_c = TempDb::new("y2_copy_c");
    let copy_for_r = TempDb::new("y2_copy_r");
    fs::copy(tmp_base.path(), copy_for_c.path()).unwrap();
    fs::copy(tmp_base.journal_path(), copy_for_c.journal_path()).unwrap();
    fs::copy(tmp_base.path(), copy_for_r.path()).unwrap();
    fs::copy(tmp_base.journal_path(), copy_for_r.journal_path()).unwrap();

    assert_eq!(
        run_c_sqlite3(copy_for_c.path(), "SELECT count(*), sum(x) FROM t;"),
        "200|0"
    );
    assert_eq!(
        run_c_sqlite3(copy_for_c.path(), "PRAGMA integrity_check;"),
        "ok"
    );

    {
        let r_conn = Connection::open(copy_for_r.path()).unwrap();
        let row = r_conn
            .query_row("SELECT count(*), sum(x) FROM t;", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64(0), Some(200));
        assert_eq!(
            row.get_i64(1),
            Some(0),
            "Rust MUST replay ALL spilled journal headers back to sum(x)=0!"
        );
    }
    assert_eq!(
        run_c_sqlite3(copy_for_r.path(), "PRAGMA integrity_check;"),
        "ok"
    );
    assert_eq!(
        run_c_sqlite3(copy_for_r.path(), "SELECT count(*), sum(x) FROM t;"),
        "200|0"
    );

    // 2. Reverse direction: crash/fail Rust mid-COMMIT (with finalized journal header & default 4096 sector size),
    //    and let C `/usr/bin/sqlite3` recover the Rust-written hot journal!
    let tmp_r_crash = TempDb::new("y2_rust_journal_for_c");
    let (vfs, fault) = FaultVfs::new();
    let rw_flags = OpenFlags::READWRITE | OpenFlags::CREATE;
    {
        let conn =
            Connection::open_with_vfs(vfs.clone(), Some(tmp_r_crash.path()), rw_flags).unwrap();
        conn.execute("CREATE TABLE t(x INT); INSERT INTO t VALUES (10);")
            .unwrap();
        conn.execute("BEGIN; UPDATE t SET x = 999;").unwrap();
        {
            let mut st = fault.lock().unwrap();
            st.main_writes_seen = 0;
            // Let Page 1 write succeed, fail on Page 2 write AND block rollback writes so finalized Rust journal stays on disk!
            st.fail_on_main_write_nth = Some(2);
        }
        assert!(conn.execute("COMMIT;").is_err());
        fault.lock().unwrap().fail_all_main_writes = true;
        drop(conn);
    }
    assert!(tmp_r_crash.journal_path().exists());

    // C `sqlite3` opens and recovers the Rust hot journal cleanly!
    assert_eq!(run_c_sqlite3(tmp_r_crash.path(), "SELECT x FROM t;"), "10");
    assert_eq!(
        run_c_sqlite3(tmp_r_crash.path(), "PRAGMA integrity_check;"),
        "ok"
    );
}

// ============================================================================
// Finding Y3: External DDL across connections invalidates cached schema & prepared statements
// ============================================================================

#[test]
fn test_bulgu_y3_cross_connection_ddl_refreshes_schema_and_prevents_wrong_table_write() {
    let tmp = TempDb::new("y3_schema_cookie");
    let conn_a = Connection::open(tmp.path()).unwrap();
    let conn_b = Connection::open(tmp.path()).unwrap();

    // Exact reproduction from `bulgular.md` Y3:
    conn_a
        .execute("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .unwrap();
    let b_initial = conn_b.query("SELECT * FROM t;", &[]).unwrap();
    assert_eq!(b_initial.rows.len(), 1);
    assert_eq!(b_initial.rows[0].get_i64(0), Some(1));

    // Also prepare a SELECT statement on B BEFORE A drops `t`!
    let mut b_prepared_sel = conn_b.prepare("SELECT * FROM t;").unwrap();
    assert!(matches!(
        b_prepared_sel.step().unwrap(),
        sqliters::StepResult::Row(_)
    ));
    b_prepared_sel.reset().unwrap();

    conn_a
        .execute("DROP TABLE t; CREATE TABLE u(x); INSERT INTO u VALUES (99);")
        .unwrap();

    // Prepared SELECT on B must detect schema change on next `step()` and fail with `no such table: t`!
    let prep_err = b_prepared_sel.step().unwrap_err();
    assert!(
        prep_err.message.contains("no such table: t"),
        "expected no such table: t, got: {}",
        prep_err.message
    );
    drop(b_prepared_sel);

    // `INSERT INTO t` on B MUST fail with `no such table: t` and NEVER write into `u`!
    let ins_err = conn_b.execute("INSERT INTO t VALUES (2);").unwrap_err();
    assert!(
        ins_err.message.contains("no such table: t"),
        "expected no such table: t, got: {}",
        ins_err.message
    );

    // Verify `u` only contains `99` and `integrity_check` is `ok`!
    let u_rows = conn_a.query("SELECT x FROM u;", &[]).unwrap();
    assert_eq!(u_rows.rows.len(), 1);
    assert_eq!(u_rows.rows[0].get_i64(0), Some(99));
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
}

// ============================================================================
// Finding Y4: `auto_vacuum` (`FULL` & `INCREMENTAL`) bidirectional C parity & ptrmap maintenance
// ============================================================================

#[test]
fn test_bulgu_y4_auto_vacuum_databases_write_shrink_and_pass_c_integrity_check() {
    for (mode_num, label) in [(1, "full"), (2, "incremental")] {
        let tmp = TempDb::new(&format!("y4_autovac_{label}"));
        run_c_sqlite3(
            tmp.path(),
            &format!(
                "PRAGMA page_size = 512;
                 PRAGMA auto_vacuum = {mode_num};
                 CREATE TABLE t(id INTEGER PRIMARY KEY, x TEXT);
                 CREATE INDEX idx_t_x ON t(x);
                 INSERT INTO t VALUES (1, 'initial');"
            ),
        );

        let grown_size: u64;
        let shrunk_size: u64;
        {
            let conn = Connection::open(tmp.path()).unwrap();
            let r = conn.query("SELECT x FROM t WHERE id = 1;", &[]).unwrap();
            assert_eq!(r.rows.len(), 1);
            assert_eq!(r.rows[0].get_str(0), Some("initial"));

            // Mutate C-created auto-vacuum DB from Rust: trigger B-tree leaf/interior splits
            // AND multi-page overflow chains (`1200` bytes on `512`-byte pages)!
            conn.execute_batch("BEGIN;").unwrap();
            for i in 2..=60i64 {
                let payload = if i % 5 == 0 {
                    format!("ovfl-{i:03}-{}", "z".repeat(1200))
                } else {
                    format!("row-{i:03}-{}", "y".repeat(180))
                };
                conn.execute_params(
                    "INSERT INTO t VALUES (?1, ?2);",
                    &[Value::Integer(i), Value::Text(payload)],
                )
                .unwrap();
            }
            conn.execute_batch("COMMIT;").unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            grown_size = std::fs::metadata(tmp.path()).unwrap().len();
            assert!(
                grown_size > 16 * 512,
                "expected multi-page growth, got {grown_size}"
            );

            // Delete most rows and verify FULL shrinks on commit while INCREMENTAL shrinks on
            // `PRAGMA incremental_vacuum`!
            conn.execute_batch("DELETE FROM t WHERE id > 3;").unwrap();
            if mode_num == 2 {
                let before_incrvac = std::fs::metadata(tmp.path()).unwrap().len();
                assert_eq!(
                    before_incrvac, grown_size,
                    "INCREMENTAL mode must retain freelist pages until PRAGMA incremental_vacuum"
                );
                let freed = conn.query("PRAGMA incremental_vacuum;", &[]).unwrap();
                assert!(
                    !freed.rows.is_empty(),
                    "expected PRAGMA incremental_vacuum to yield freed-page rows"
                );
            }
            shrunk_size = std::fs::metadata(tmp.path()).unwrap().len();
            assert!(
                shrunk_size < grown_size / 2,
                "expected {label} auto-vacuum file to shrink ({grown_size} -> {shrunk_size})"
            );
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
        }

        // Verify C SQLite 3.54.1 `PRAGMA integrity_check` == `ok` and exact row/index contents!
        assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
        assert_eq!(run_c_sqlite3(tmp.path(), "SELECT count(*) FROM t;"), "3");
        assert_eq!(
            run_c_sqlite3(tmp.path(), "SELECT x FROM t WHERE id = 1;"),
            "initial"
        );
    }
}

// ============================================================================
// Finding Y5: Genuine Index-B-Tree `WITHOUT ROWID` storage & bidirectional C compatibility
// ============================================================================

#[test]
fn test_bulgu_y5_without_rowid_index_btree_bidirectional_c_compatibility() {
    // 1. Exact reproduction from `bulgular.md` Y5:
    //    `CREATE TABLE t(a TEXT PRIMARY KEY) WITHOUT ROWID; INSERT INTO t VALUES('a');`
    let tmp_exact = TempDb::new("y5_exact_repro");
    {
        let conn = Connection::open(tmp_exact.path()).unwrap();
        conn.execute(
            "CREATE TABLE t(a TEXT PRIMARY KEY) WITHOUT ROWID;
             INSERT INTO t VALUES ('a');",
        )
        .unwrap();

        // Implicit `rowid` does not exist on WITHOUT ROWID tables, and PK is implicitly NOT NULL
        assert!(conn.query("SELECT rowid FROM t;", &[]).is_err());
        let nn_err = conn.execute("INSERT INTO t VALUES (NULL);").unwrap_err();
        assert_eq!(nn_err.extended_code, extended::SQLITE_CONSTRAINT_NOTNULL);
        let pk_err = conn.execute("INSERT INTO t VALUES ('a');").unwrap_err();
        assert_eq!(pk_err.extended_code, extended::SQLITE_CONSTRAINT_PRIMARYKEY);

        // Error checks for invalid WITHOUT ROWID definitions
        let missing_pk = conn
            .execute("CREATE TABLE no_pk(x INT, y TEXT) WITHOUT ROWID;")
            .unwrap_err();
        assert!(missing_pk.message.contains("PRIMARY KEY missing"));
        let bad_autoinc = conn
            .execute("CREATE TABLE bad_ai(x INTEGER PRIMARY KEY AUTOINCREMENT) WITHOUT ROWID;")
            .unwrap_err();
        assert!(bad_autoinc.message.contains("AUTOINCREMENT"));
    }
    assert_eq!(
        run_c_sqlite3(tmp_exact.path(), "PRAGMA integrity_check;"),
        "ok"
    );
    assert_eq!(run_c_sqlite3(tmp_exact.path(), "SELECT a FROM t;"), "a");
    assert_eq!(
        run_c_sqlite3(tmp_exact.path(), "SELECT count(*) FROM sqlite_schema;"),
        "1"
    );

    // 2. Split composite PK out of column declaration order + secondary UNIQUE & non-unique indexes,
    //    multi-page splits, OR REPLACE / OR IGNORE, UPDATE, DELETE, ALTER TABLE ADD COLUMN, and VACUUM!
    let tmp_complex = TempDb::new("y5_rust_to_c_complex");
    {
        let conn = Connection::open(tmp_complex.path()).unwrap();
        conn.execute(
            "PRAGMA page_size = 1024;
             CREATE TABLE comp(
                 v1 TEXT,
                 k1 INT,
                 u_tag TEXT UNIQUE,
                 k2 TEXT COLLATE NOCASE,
                 PRIMARY KEY(k1, k2 DESC)
             ) WITHOUT ROWID;
             CREATE INDEX idx_comp_v1 ON comp(v1);",
        )
        .unwrap();

        for i in 1..=80 {
            conn.execute_params(
                "INSERT INTO comp VALUES (?1, ?2, ?3, ?4);",
                &[
                    Value::Text(format!("val_{}", i % 10)),
                    Value::Integer(i % 8),
                    Value::Text(format!("tag_{i}")),
                    Value::Text(format!("key_{i:03}_") + &"x".repeat(40)),
                ],
            )
            .unwrap();
        }

        // Case-insensitive PK collision (`k2 COLLATE NOCASE`) resolved via OR REPLACE
        conn.execute(
            "INSERT OR REPLACE INTO comp VALUES ('replaced', 1, 'tag_rep', 'KEY_001_' || replace(hex(zeroblob(20)), '00', 'xx'));",
        )
        .unwrap();
        // Secondary UNIQUE conflict resolved via OR IGNORE
        conn.execute("INSERT OR IGNORE INTO comp VALUES ('ignored', 99, 'tag_2', 'unique_k2');")
            .unwrap();
        // UPDATE both PK and non-PK columns, DELETE a subset, ADD COLUMN with DEFAULT, and VACUUM
        conn.execute(
            "UPDATE comp SET k1 = 100, v1 = 'moved' WHERE u_tag = 'tag_3';
             DELETE FROM comp WHERE k1 = 0;
             ALTER TABLE comp ADD COLUMN extra_num INT DEFAULT 77;
             VACUUM;",
        )
        .unwrap();

        let chk = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(chk.rows[0].get_str(0), Some("ok"));
    }
    assert_eq!(
        run_c_sqlite3(tmp_complex.path(), "PRAGMA integrity_check;"),
        "ok"
    );
    assert_eq!(
        run_c_sqlite3(
            tmp_complex.path(),
            "SELECT k1, v1, extra_num FROM comp WHERE u_tag = 'tag_3';"
        ),
        "100|moved|77"
    );
    assert_eq!(
        run_c_sqlite3(tmp_complex.path(), "SELECT count(*) FROM comp;"),
        "70"
    );

    // 3. Reverse direction: C `sqlite3` creates and populates a `WITHOUT ROWID` database,
    //    Rust reads/updates/deletes/inserts on it, and C `PRAGMA integrity_check` verifies `ok`!
    let tmp_c_to_r = TempDb::new("y5_c_to_rust");
    run_c_sqlite3(
        tmp_c_to_r.path(),
        "CREATE TABLE wr(payload TEXT, p1 INT, p2 TEXT, uniq_code INT UNIQUE, PRIMARY KEY(p1, p2)) WITHOUT ROWID;
         CREATE INDEX idx_wr_payload ON wr(payload);
         INSERT INTO wr VALUES ('alpha', 1, 'one', 100), ('beta', 1, 'two', 200), ('gamma', 2, 'one', 300);",
    );
    {
        let conn = Connection::open(tmp_c_to_r.path()).unwrap();
        // Point PK seek
        let r = conn
            .query_row(
                "SELECT payload, uniq_code FROM wr WHERE p1 = 1 AND p2 = 'two';",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(r.get_str(0), Some("beta"));
        assert_eq!(r.get_i64(1), Some(200));

        // Secondary index seek
        let r2 = conn
            .query_row("SELECT p1, p2 FROM wr WHERE payload = 'gamma';", &[])
            .unwrap()
            .unwrap();
        assert_eq!(r2.get_i64(0), Some(2));
        assert_eq!(r2.get_str(1), Some("one"));

        // Mutate in Rust
        conn.execute(
            "UPDATE wr SET p2 = 'TWO_UPDATED', payload = 'beta2' WHERE uniq_code = 200;
             DELETE FROM wr WHERE p1 = 1 AND p2 = 'one';
             INSERT INTO wr VALUES ('delta', 3, 'three', 400);",
        )
        .unwrap();
    }
    assert_eq!(
        run_c_sqlite3(tmp_c_to_r.path(), "PRAGMA integrity_check;"),
        "ok"
    );
    assert_eq!(
        run_c_sqlite3(
            tmp_c_to_r.path(),
            "SELECT p1, p2, payload, uniq_code FROM wr ORDER BY p1, p2;"
        ),
        "1|TWO_UPDATED|beta2|200\n2|one|gamma|300\n3|three|delta|400"
    );
}

// ============================================================================
// Finding Y6: Turkish / Unicode identifiers around 7-byte UTF-8 boundaries never panic
// ============================================================================

#[test]
fn test_bulgu_y6_unicode_identifiers_no_utf8_slice_panic() {
    let tmp = TempDb::new("y6_unicode_ident");
    {
        let conn = Connection::open(tmp.path()).unwrap();
        // `"çççç"` is 8 bytes (four 2-byte UTF-8 chars: byte 7 is inside the 4th `'ç'`)!
        conn.execute(
            r#"
            CREATE TABLE "çççç"(x INT);
            INSERT INTO "çççç" VALUES (1);
            CREATE INDEX "ğğğğ" ON "çççç"(x);
            CREATE TABLE "öööö" AS SELECT x FROM "çççç";
            ALTER TABLE "öööö" RENAME TO "şşşş";
            "#,
        )
        .unwrap();

        let r1 = conn.query(r#"SELECT x FROM "çççç";"#, &[]).unwrap();
        assert_eq!(r1.rows[0].get_i64(0), Some(1));
        let r2 = conn.query(r#"SELECT x FROM "şşşş";"#, &[]).unwrap();
        assert_eq!(r2.rows[0].get_i64(0), Some(1));

        // Reserved `sqlite_` prefix check still works properly
        assert!(conn.execute("CREATE TABLE sqlite_bad(x);").is_err());
        assert!(
            conn.execute(r#"ALTER TABLE "çççç" RENAME TO sqlite_bad;"#)
                .is_err()
        );

        // Also test date/time functions with multi-byte strings around byte 10 / fractional seconds
        let dt = conn
            .query_row("SELECT date('2026-01-çç'), time('12:30:00.1çç');", &[])
            .unwrap()
            .unwrap();
        assert_eq!(dt.get(0), Some(&Value::Null));
        assert_eq!(dt.get(1), Some(&Value::Null));
    }
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
}

// ============================================================================
// Finding Y7: `STRICT` table `ANY` column preserves storage class & text leading zeros
// ============================================================================

#[test]
fn test_bulgu_y7_strict_any_preserves_storage_class_and_leading_zeros() {
    let tmp = TempDb::new("y7_strict_any");
    {
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute(
            "CREATE TABLE t(
                id INTEGER PRIMARY KEY,
                x ANY,
                g ANY GENERATED ALWAYS AS (x) VIRTUAL
             ) STRICT;
             INSERT INTO t(id, x) VALUES
                (1, '000123'),
                (2, 456),
                (3, 3.25),
                (4, X'CAFE'),
                (5, NULL);",
        )
        .unwrap();

        let rows = conn
            .query("SELECT typeof(x), x, typeof(g), g FROM t ORDER BY id;", &[])
            .unwrap()
            .rows;
        assert_eq!(rows[0].get_str(0), Some("text"));
        assert_eq!(rows[0].get_str(1), Some("000123"));
        assert_eq!(rows[0].get_str(2), Some("text"));
        assert_eq!(rows[0].get_str(3), Some("000123"));

        assert_eq!(rows[1].get_str(0), Some("integer"));
        assert_eq!(rows[1].get_i64(1), Some(456));
        assert_eq!(rows[2].get_str(0), Some("real"));
        assert_eq!(rows[3].get_str(0), Some("blob"));
        assert_eq!(rows[4].get_str(0), Some("null"));

        // UPDATE on STRICT ANY also preserves `'007'` as `text`!
        conn.execute("UPDATE t SET x = '007' WHERE id = 2;")
            .unwrap();
        let u = conn
            .query_row("SELECT typeof(x), x FROM t WHERE id = 2;", &[])
            .unwrap()
            .unwrap();
        assert_eq!(u.get_str(0), Some("text"));
        assert_eq!(u.get_str(1), Some("007"));

        // Invalid datatype in STRICT table is rejected
        assert!(
            conn.execute("CREATE TABLE bad_strict(a VARCHAR(10)) STRICT;")
                .is_err()
        );
        assert!(conn.execute("CREATE TABLE bad_strict2(a) STRICT;").is_err());
    }
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT typeof(x), x FROM t WHERE id = 1;"),
        "text|000123"
    );
}

// ============================================================================
// Finding Y8: `ALTER TABLE ADD COLUMN` validates `CHECK`, virtual `NOT NULL`, and `STRICT` on existing rows
// ============================================================================

#[test]
fn test_bulgu_y8_alter_table_add_column_validates_existing_rows() {
    let tmp = TempDb::new("y8_add_col_check");
    {
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();

        // Exact reproducer from `bulgular.md` Y8:
        let err = conn
            .execute("ALTER TABLE t ADD COLUMN y INT CHECK(y > 0) DEFAULT -1;")
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);
        assert!(err.message.contains("CHECK constraint failed"));

        // Schema must remain unchanged (`t` still has only column `x`)!
        let r = conn.query("SELECT * FROM t;", &[]).unwrap();
        assert_eq!(r.column_names, vec!["x"]);
        assert_eq!(r.rows[0].get_i64(0), Some(1));

        // Virtual generated NOT NULL column evaluating to NULL on existing row must also fail!
        let g_err = conn
            .execute("ALTER TABLE t ADD COLUMN z INT GENERATED ALWAYS AS (NULL) VIRTUAL NOT NULL;")
            .unwrap_err();
        assert_eq!(g_err.code, ErrorCode::Error);

        // Valid `ADD COLUMN` with satisfied `CHECK` succeeds!
        conn.execute("ALTER TABLE t ADD COLUMN y INT CHECK(y > 0) DEFAULT 5;")
            .unwrap();
        let r2 = conn.query("SELECT x, y FROM t;", &[]).unwrap();
        assert_eq!(r2.rows[0].get_i64(1), Some(5));
    }
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
}

// ============================================================================
// Finding Y9: `BEGIN EXCLUSIVE` acquires `EXCLUSIVE` lock immediately (blocks readers & fails on active reader)
// ============================================================================

#[test]
fn test_bulgu_y9_begin_exclusive_blocks_external_readers_and_conflicts_with_shared() {
    let tmp = TempDb::new("y9_begin_excl");
    let conn1 = Connection::open(tmp.path()).unwrap();
    conn1
        .execute("CREATE TABLE t(x INT); INSERT INTO t VALUES (1);")
        .unwrap();

    // 1. While `BEGIN EXCLUSIVE` is held in Rust, an external `/usr/bin/sqlite3` `SELECT` MUST fail with `database is locked`!
    conn1.execute("BEGIN EXCLUSIVE;").unwrap();
    let c_out = Command::new("sqlite3")
        .arg("-batch")
        .arg(tmp.path())
        .arg("SELECT * FROM t;")
        .output()
        .expect("spawn sqlite3");
    assert!(
        !c_out.status.success(),
        "external C sqlite3 SELECT must fail while Rust holds BEGIN EXCLUSIVE"
    );
    let stderr = String::from_utf8_lossy(&c_out.stderr);
    assert!(
        stderr.contains("database is locked"),
        "expected 'database is locked', got: {stderr}"
    );
    conn1.execute("COMMIT;").unwrap();

    // After COMMIT, external C `sqlite3` `SELECT` succeeds!
    assert_eq!(run_c_sqlite3(tmp.path(), "SELECT * FROM t;"), "1");

    // 2. Conversely, if another connection holds a SHARED read lock (both in-process and cross-process),
    // `BEGIN EXCLUSIVE` MUST fail immediately with `Busy`!
    let conn2 = Connection::open(tmp.path()).unwrap();
    conn2.execute("INSERT INTO t VALUES (2);").unwrap();
    let mut active_scan = conn2.prepare("SELECT x FROM t;").unwrap();
    assert!(matches!(
        active_scan.step().unwrap(),
        sqliters::StepResult::Row(_)
    ));
    // `active_scan` currently holds a SHARED read lock on `conn2`!
    let excl_err = conn1.execute("BEGIN EXCLUSIVE;").unwrap_err();
    assert_eq!(excl_err.code, ErrorCode::Busy);
    assert!(conn1.is_autocommit());
    drop(active_scan);

    // Cross-process C/Python `sqlite3` holding a `SHARED` read lock via unclosed cursor inside `BEGIN`:
    let mut py_reader = Command::new("python3")
        .arg("-c")
        .arg(
            "import sqlite3, sys\n\
             conn = sqlite3.connect(sys.argv[1], isolation_level=None)\n\
             conn.execute('BEGIN')\n\
             cur = conn.execute('SELECT x FROM t')\n\
             cur.fetchone()\n\
             print('HOLDING_SHARED', flush=True)\n\
             sys.stdin.readline()\n\
             cur.close()\n\
             conn.execute('ROLLBACK')\n\
             conn.close()\n",
        )
        .arg(tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn python3 sqlite3 reader");
    {
        let mut reader_out = BufReader::new(py_reader.stdout.take().unwrap());
        let mut line = String::new();
        reader_out.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "HOLDING_SHARED");
    }
    let excl_err_cross = conn1.execute("BEGIN EXCLUSIVE;").unwrap_err();
    assert_eq!(excl_err_cross.code, ErrorCode::Busy);
    assert!(conn1.is_autocommit());
    drop(py_reader.stdin.take());
    assert!(py_reader.wait().unwrap().success());

    // Once reader finishes, `BEGIN EXCLUSIVE` succeeds!
    conn1.execute("BEGIN EXCLUSIVE; COMMIT;").unwrap();
}

// ============================================================================
// Finding Y10: Bare column with `MIN(x)` / `MAX(x)` comes from the extremum row
// ============================================================================

#[test]
fn test_bulgu_y10_min_max_bare_column_selected_from_extremum_row() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE t(grp TEXT, a INT, b TEXT);
         INSERT INTO t VALUES
            ('g1', 1, 'one'),
            ('g1', 3, 'three'),
            ('g1', 2, 'two'),
            ('g2', 10, 'ten'),
            ('g2', 5, 'five');",
    )
    .unwrap();

    // Exact ungrouped reproducer from `bulgular.md` Y10
    let r_max = conn
        .query_row("SELECT max(a), b FROM t;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(r_max.get_i64(0), Some(10));
    assert_eq!(r_max.get_str(1), Some("ten"));

    let r_min = conn
        .query_row("SELECT min(a), b FROM t;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(r_min.get_i64(0), Some(1));
    assert_eq!(r_min.get_str(1), Some("one"));

    // Grouped `GROUP BY` with `MAX` and `MIN`
    let g_max = conn
        .query(
            "SELECT grp, max(a), b FROM t GROUP BY grp ORDER BY grp;",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(g_max[0].get_i64(1), Some(3));
    assert_eq!(g_max[0].get_str(2), Some("three"));
    assert_eq!(g_max[1].get_i64(1), Some(10));
    assert_eq!(g_max[1].get_str(2), Some("ten"));

    let g_min = conn
        .query(
            "SELECT grp, min(a), b FROM t GROUP BY grp ORDER BY grp;",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(g_min[0].get_i64(1), Some(1));
    assert_eq!(g_min[0].get_str(2), Some("one"));
    assert_eq!(g_min[1].get_i64(1), Some(5));
    assert_eq!(g_min[1].get_str(2), Some("five"));

    // With `FILTER (WHERE ...)`
    let r_filt = conn
        .query_row("SELECT max(a) FILTER (WHERE a < 10), b FROM t;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(r_filt.get_i64(0), Some(5));
    assert_eq!(r_filt.get_str(1), Some("five"));
}

// ============================================================================
// Finding Y11: `BEGIN` rejected inside an implicit `SAVEPOINT` transaction
// ============================================================================

#[test]
fn test_bulgu_y11_begin_rejected_inside_savepoint_transaction() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(x INT);").unwrap();

    conn.execute("SAVEPOINT x;").unwrap();
    assert!(!conn.is_autocommit());

    let err = conn.execute("BEGIN;").unwrap_err();
    assert!(
        err.message
            .contains("cannot start a transaction within a transaction"),
        "unexpected error: {}",
        err.message
    );
    assert!(!conn.is_autocommit());

    // Even after `ROLLBACK TO x`, the implicit savepoint transaction is still open!
    conn.execute("INSERT INTO t VALUES (1); ROLLBACK TO x;")
        .unwrap();
    assert!(conn.execute("BEGIN IMMEDIATE;").is_err());

    // Releasing top-level savepoint returns to autocommit, after which `BEGIN` succeeds!
    conn.execute("RELEASE x;").unwrap();
    assert!(conn.is_autocommit());
    conn.execute("BEGIN; INSERT INTO t VALUES (42); COMMIT;")
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT x FROM t;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(42)
    );
}

// ============================================================================
// Finding Y12: Automatic rowid after negative maximum rowid (`-5` -> `-4` vs `AUTOINCREMENT` -> `1`)
// ============================================================================

#[test]
fn test_bulgu_y12_negative_max_rowid_increment_vs_autoincrement() {
    let tmp = TempDb::new("y12_neg_rowid");
    {
        let conn = Connection::open(tmp.path()).unwrap();

        // 1. Normal rowid table: `-5` -> `-4` -> `-3`, `-1` -> `0` -> `1`
        conn.execute(
            "CREATE TABLE t(a TEXT);
             INSERT INTO t(rowid, a) VALUES (-5, 'old');
             INSERT INTO t(a) VALUES ('new');",
        )
        .unwrap();
        let rows = conn
            .query("SELECT rowid, a FROM t ORDER BY rowid;", &[])
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_i64(0), Some(-5));
        assert_eq!(rows[0].get_str(1), Some("old"));
        assert_eq!(rows[1].get_i64(0), Some(-4));
        assert_eq!(rows[1].get_str(1), Some("new"));

        conn.execute("INSERT INTO t(rowid, a) VALUES (-1, 'minus_one'); INSERT INTO t(a) VALUES ('zero'); INSERT INTO t(a) VALUES ('one');")
            .unwrap();
        let rids: Vec<i64> = conn
            .query("SELECT rowid FROM t ORDER BY rowid;", &[])
            .unwrap()
            .rows
            .iter()
            .map(|r| r.get_i64(0).unwrap())
            .collect();
        assert_eq!(rids, vec![-5, -4, -1, 0, 1]);

        // 2. `AUTOINCREMENT` table: `sqlite_sequence` starts at `0`, so after `-5` next rowid is `1`!
        conn.execute(
            "CREATE TABLE t_ai(id INTEGER PRIMARY KEY AUTOINCREMENT, a TEXT);
             INSERT INTO t_ai VALUES (-5, 'old');
             INSERT INTO t_ai(a) VALUES ('new');",
        )
        .unwrap();
        let ai_rows = conn
            .query("SELECT id, a FROM t_ai ORDER BY id;", &[])
            .unwrap()
            .rows;
        assert_eq!(ai_rows[0].get_i64(0), Some(-5));
        assert_eq!(ai_rows[1].get_i64(0), Some(1));
    }

    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
}
