//! Regression test suite verifying fixes for all 9 findings in `bulgular.md`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqliters::{Connection, ErrorCode, MemoryVfs, OpenFlags, Value};

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
            assert!(n > 0, "child process exited before printing marker '{marker}'");
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
            assert!(n > 0, "child process exited before printing marker '{marker}'");
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
    c_writer.send_and_wait_marker("BEGIN IMMEDIATE;\nINSERT INTO t VALUES (2, 'from_c');", "LOCKED_IMMEDIATE");

    {
        let mut conn = Connection::open(tmp.path()).unwrap();
        let read_res = conn.query("SELECT id, val FROM t ORDER BY id;", &[]).unwrap();
        assert_eq!(read_res.rows.len(), 1);
        assert_eq!(read_res.rows[0].get_str(1), Some("init"));

        let write_err = conn
            .execute("INSERT INTO t VALUES (3, 'conflict_from_rust');")
            .unwrap_err();
        assert_eq!(write_err.code, ErrorCode::Busy);
    }

    c_writer.finish("COMMIT;");
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT count(*) FROM t;"),
        "2"
    );

    // 2. C `sqlite3` holds an EXCLUSIVE lock (`BEGIN EXCLUSIVE`):
    //    - `Connection::open` defers schema load gracefully, and `SELECT` returns `ErrorCode::Busy`!
    let mut c_excl = InteractiveSqlProc::spawn_c_sqlite3(tmp.path());
    c_excl.send_and_wait_marker("BEGIN EXCLUSIVE;", "LOCKED_EXCLUSIVE");

    let mut conn2 = Connection::open(tmp.path()).unwrap();
    let sel_err = conn2.query("SELECT * FROM t;", &[]).unwrap_err();
    assert_eq!(sel_err.code, ErrorCode::Busy);

    c_excl.finish("COMMIT;");
    let after_unlock = conn2.query("SELECT count(*) FROM t;", &[]).unwrap();
    assert_eq!(after_unlock.rows[0].get_i64(0), Some(2));

    // 3. Multi-handle `MemoryVfs` also enforces `ErrorCode::Busy` on concurrent writers
    let mem_vfs = Arc::new(MemoryVfs::new());
    let db_virtual_path = Path::new("/mem/shared.db");
    let flags = OpenFlags::READWRITE | OpenFlags::CREATE;
    let mut m1 = Connection::open_with_vfs(mem_vfs.clone(), Some(db_virtual_path), flags).unwrap();
    m1.execute("CREATE TABLE items(x INT); INSERT INTO items VALUES (10);")
        .unwrap();
    let mut m2 = Connection::open_with_vfs(mem_vfs, Some(db_virtual_path), flags).unwrap();

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
        let mut conn = Connection::open(Path::new(&db_os)).unwrap();
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
        let mut reader = Connection::open(tmp.path()).unwrap();
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

    let mut recovered = Connection::open(crash_tmp.path()).unwrap();
    let bal = recovered
        .query_row("SELECT balance FROM accounts WHERE id = 1;", &[])
        .unwrap()
        .unwrap()
        .get_i64(0);
    assert_eq!(bal, Some(100));
    assert!(
        !crash_tmp.journal_path().exists(),
        "crashed hot journal should be replayed and unlinked once no writer holds a lock"
    );
    drop(recovered);
    assert_eq!(
        run_c_sqlite3(crash_tmp.path(), "PRAGMA integrity_check;"),
        "ok"
    );
}

// ============================================================================
// Finding 3: Explicit WAL mode rejection (`read_version == 2` / `-wal`)
// ============================================================================

#[test]
fn test_bulgu_3_wal_database_explicitly_rejected() {
    let tmp = TempDb::new("f3_wal");
    run_c_sqlite3(
        tmp.path(),
        "PRAGMA journal_mode = WAL; CREATE TABLE t(x INT); INSERT INTO t VALUES (42);",
    );

    let err = match Connection::open(tmp.path()) {
        Ok(_) => panic!("opening WAL database must fail explicitly rather than returning stale data"),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::CantOpen);
    assert!(
        err.message.contains("WAL"),
        "expected WAL mention in error message, got: {}",
        err.message
    );
}

// ============================================================================
// Finding 4: `PRAGMA foreign_keys = ON` enforcement, actions, and deferred FKs
// ============================================================================

#[test]
fn test_bulgu_4_foreign_keys_enforcement_actions_and_deferred() {
    let mut conn = Connection::open_in_memory().unwrap();

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
        .query_row("SELECT team_id, fallback_org FROM members WHERE id = 100;", &[])
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

    conn.execute("BEGIN; INSERT INTO child_def VALUES (500);").unwrap();
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
    let mut conn = Connection::open_in_memory().unwrap();

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
    assert_eq!(conn.changes(), 1, "row (3) written before conflict must count");

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
        let mut conn = Connection::open(tmp.path()).unwrap();

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

        let chk_err = conn.execute("INSERT INTO t VALUES (-3, 'neg');").unwrap_err();
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
    let mut conn = Connection::open_in_memory().unwrap();
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
            let mut conn = Connection::open(tmp_c.path()).unwrap();
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
                .query("SELECT id FROM t WHERE s = ?1;", &[Value::Text("Türkçe".into())])
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
            let mut conn = Connection::open(tmp_r.path()).unwrap();
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
    let mut conn = Connection::open_in_memory().unwrap();
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
