//! Phase 1.1-D (`sqliters-core` & `sqliters-cli`) Comprehensive Ownership, Concurrency,
//! Lock-Lease Lifecycle, Savepoint, Prepare/Reprepare/Autoreset, & C SQLite 3.54.1 Interop Suite.
#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use sqliters::{
    CloseError, Connection, ConnectionOpenOptions, DatabaseKind, ErrorCode, InterruptHandle,
    LockGeometry, LockLevel, MemoryVfs, OpenFlags, PrepareMode, Result as SqlResult, SqliteError,
    Statement, StepResult, SyncFlags, TestBlobHandle, Value, Vfs, VfsFile, types::extended,
};

// ============================================================================
// Sandbox & Out-of-Tree C SQLite 3.54.1 Reference Helper (`target/sqlite-ref/`)
// ============================================================================

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p1d_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp sandbox dir");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const C_LIFECYCLE_HELPER_SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"

static void die_rc(sqlite3 *db, const char *where, int rc) {
    fprintf(stderr, "C helper error at %s: rc=%d ext=%d msg=%s\n",
            where, rc, db ? sqlite3_extended_errcode(db) : rc,
            db ? sqlite3_errmsg(db) : "no db");
    exit(2);
}

static void exec_must(sqlite3 *db, const char *sql) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "C helper exec error: rc=%d msg=%s sql=%s\n",
                rc, errmsg ? errmsg : "", sql);
        sqlite3_free(errmsg);
        exit(2);
    }
}

static void wait_stdin_line(void) {
    char buf[64];
    (void)fgets(buf, (int)sizeof(buf), stdin);
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s <cmd> <db> [arg]\n", argv[0]);
        return 2;
    }
    const char *cmd = argv[1];
    const char *db_path = argv[2];

    if (strcmp(cmd, "hold_exclusive") == 0) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) die_rc(db, "open", rc);
        exec_must(db, "BEGIN EXCLUSIVE;");
        printf("LOCKED\n");
        fflush(stdout);
        wait_stdin_line();
        exec_must(db, "COMMIT;");
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "hold_reserved") == 0) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) die_rc(db, "open", rc);
        exec_must(db, "BEGIN IMMEDIATE;");
        printf("LOCKED\n");
        fflush(stdout);
        wait_stdin_line();
        exec_must(db, "ROLLBACK;");
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "hold_shared") == 0) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) die_rc(db, "open", rc);
        sqlite3_stmt *stmt = NULL;
        rc = sqlite3_prepare_v2(db, "SELECT * FROM t;", -1, &stmt, NULL);
        if (rc != SQLITE_OK) die_rc(db, "prepare", rc);
        rc = sqlite3_step(stmt);
        if (rc != SQLITE_ROW) die_rc(db, "step_row", rc);
        printf("LOCKED\n");
        fflush(stdout);
        wait_stdin_line();
        sqlite3_finalize(stmt);
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "try_sql") == 0) {
        if (argc < 4) return 2;
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) {
            printf("ERR %d %d\n", rc, sqlite3_extended_errcode(db));
            if (db) sqlite3_close(db);
            return 0;
        }
        sqlite3_busy_timeout(db, 0);
        rc = sqlite3_exec(db, argv[3], NULL, NULL, NULL);
        if (rc == SQLITE_OK) {
            printf("OK\n");
        } else {
            printf("ERR %d %d\n", rc, sqlite3_extended_errcode(db));
        }
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "diff_returning_early_reset") == 0) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) die_rc(db, "open", rc);
        exec_must(db, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);");
        sqlite3_stmt *stmt = NULL;
        rc = sqlite3_prepare_v2(
            db,
            "INSERT INTO t VALUES (10, 'a'), (20, 'b') RETURNING id;",
            -1,
            &stmt,
            NULL
        );
        if (rc != SQLITE_OK) die_rc(db, "prepare_returning", rc);
        rc = sqlite3_step(stmt);
        if (rc != SQLITE_ROW) die_rc(db, "step1", rc);
        int first_id = sqlite3_column_int(stmt, 0);
        int reset_rc = sqlite3_reset(stmt);
        sqlite3_finalize(stmt);
        int autocommit = sqlite3_get_autocommit(db);

        sqlite3_stmt *q = NULL;
        rc = sqlite3_prepare_v2(db, "SELECT id FROM t ORDER BY id;", -1, &q, NULL);
        if (rc != SQLITE_OK) die_rc(db, "prepare_q", rc);
        printf("FIRST=%d RESET_RC=%d AUTOCOMMIT=%d ROWS=", first_id, reset_rc, autocommit);
        int idx = 0;
        while ((rc = sqlite3_step(q)) == SQLITE_ROW) {
            if (idx++ > 0) printf(",");
            printf("%d", sqlite3_column_int(q, 0));
        }
        printf("\n");
        sqlite3_finalize(q);
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "diff_clear_bindings_in_row") == 0) {
        sqlite3 *db = NULL;
        int rc = sqlite3_open(db_path, &db);
        if (rc != SQLITE_OK) die_rc(db, "open", rc);
        exec_must(db, "CREATE TABLE t(x INT); INSERT INTO t VALUES (1), (2);");
        sqlite3_stmt *stmt = NULL;
        rc = sqlite3_prepare_v2(db, "SELECT ?1, x FROM t ORDER BY x;", -1, &stmt, NULL);
        if (rc != SQLITE_OK) die_rc(db, "prepare", rc);
        sqlite3_bind_int64(stmt, 1, 42);

        rc = sqlite3_step(stmt);
        if (rc != SQLITE_ROW) die_rc(db, "step1", rc);
        int r1_p = sqlite3_column_type(stmt, 0) == SQLITE_NULL ? -1 : sqlite3_column_int(stmt, 0);
        int r1_x = sqlite3_column_int(stmt, 1);

        int cb_rc = sqlite3_clear_bindings(stmt);

        rc = sqlite3_step(stmt);
        if (rc != SQLITE_ROW) die_rc(db, "step2", rc);
        int r2_p = sqlite3_column_type(stmt, 0) == SQLITE_NULL ? -1 : sqlite3_column_int(stmt, 0);
        int r2_x = sqlite3_column_int(stmt, 1);

        rc = sqlite3_step(stmt);
        if (rc != SQLITE_DONE) die_rc(db, "done1", rc);

        /* Autoreset step after clear_bindings */
        rc = sqlite3_step(stmt);
        if (rc != SQLITE_ROW) die_rc(db, "step_autoreset", rc);
        int r3_p = sqlite3_column_type(stmt, 0) == SQLITE_NULL ? -1 : sqlite3_column_int(stmt, 0);
        int r3_x = sqlite3_column_int(stmt, 1);

        printf("CB_RC=%d R1=%d:%d R2=%d:%d R3=%d:%d\n",
               cb_rc, r1_p, r1_x, r2_p, r2_x, r3_p, r3_x);
        sqlite3_finalize(stmt);
        sqlite3_close(db);
        return 0;
    }

    return 2;
}
"#;

fn c_lifecycle_helper() -> &'static Path {
    static HELPER_BIN: OnceLock<PathBuf> = OnceLock::new();
    HELPER_BIN.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

        let before_status = Command::new("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["status", "--porcelain", "--", "sqlite"])
            .output()
            .expect("git status sqlite before build");
        assert!(before_status.status.success());

        let sqlite3_c = amalg_dir.join("sqlite3.c");
        let sqlite3_h = amalg_dir.join("sqlite3.h");
        if !sqlite3_c.exists() || !sqlite3_h.exists() {
            let make_out = Command::new("make")
                .arg("-C")
                .arg(&amalg_dir)
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .arg("sqlite3.c")
                .output()
                .expect("invoke make for out-of-tree sqlite3.c");
            assert!(
                make_out.status.success(),
                "failed building sqlite3.c:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let sqlite3_o = ref_root.join("sqlite3.o");
        if !sqlite3_o.exists() {
            let cc_o = Command::new("cc")
                .args(["-O2", "-pthread", "-c"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&sqlite3_c)
                .arg("-o")
                .arg(&sqlite3_o)
                .output()
                .expect("compile sqlite3.o");
            assert!(
                cc_o.status.success(),
                "failed compiling sqlite3.o:\n{}",
                String::from_utf8_lossy(&cc_o.stderr)
            );
        }

        let helper_c = ref_root.join("c_core_lifecycle_helper.c");
        let helper_bin = ref_root.join("c_core_lifecycle_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_LIFECYCLE_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            fs::write(&helper_c, C_LIFECYCLE_HELPER_SOURCE)
                .expect("write c_core_lifecycle_helper.c");
            let cc_out = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&helper_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&helper_bin)
                .output()
                .expect("compile c_core_lifecycle_helper");
            assert!(
                cc_out.status.success(),
                "failed compiling c_core_lifecycle_helper:\n{}",
                String::from_utf8_lossy(&cc_out.stderr)
            );
        }

        let after_status = Command::new("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["status", "--porcelain", "--", "sqlite"])
            .output()
            .expect("git status sqlite after build");
        assert_eq!(
            before_status.stdout, after_status.stdout,
            "building C helper must NEVER write into ./sqlite/"
        );

        helper_bin
    })
}

struct HeldCLock {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl HeldCLock {
    fn spawn(mode: &str, db_path: &Path) -> Self {
        let mut child = Command::new(c_lifecycle_helper())
            .arg(mode)
            .arg(db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn C lock holder");
        let stdout = child.stdout.take().expect("child stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read LOCKED line");
        assert_eq!(
            line.trim(),
            "LOCKED",
            "expected LOCKED from C helper ({mode}), got {line:?}"
        );
        let stdin = child.stdin.take();
        Self { child, stdin }
    }

    fn release(mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = writeln!(s, "GO");
            let _ = s.flush();
        }
        let status = self.child.wait().expect("wait C lock holder");
        assert!(status.success(), "C lock holder exited with {status}");
    }
}

impl Drop for HeldCLock {
    fn drop(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = writeln!(s, "GO");
            let _ = s.flush();
            let _ = self.child.wait();
        }
    }
}

fn c_try_sql(db_path: &Path, sql: &str) -> String {
    let out = Command::new(c_lifecycle_helper())
        .arg("try_sql")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("invoke c_try_sql");
    assert!(
        out.status.success(),
        "c_try_sql failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn pragma_lock_status(conn: &Connection) -> Vec<(String, String)> {
    let q = conn.query("PRAGMA lock_status;", &[]).unwrap();
    assert_eq!(q.column_names, vec!["database", "status"]);
    q.rows
        .iter()
        .map(|r| {
            (
                r.get_str(0).unwrap().to_owned(),
                r.get_str(1).unwrap().to_owned(),
            )
        })
        .collect()
}

// ============================================================================
// Group 1: Compile-Time `Send + Sync` Facade, `Arc<Connection>` Multithreading,
//          and Zero-`'conn`-Lifetime Simultaneous Handles
// ============================================================================

const fn assert_send_sync<T: Send + Sync>() {}
const fn assert_send<T: Send>() {}

#[test]
fn test_group1_send_sync_facade_multithreaded_arc_and_lifetime_free_handles() {
    assert_send_sync::<Connection>();
    assert_send_sync::<InterruptHandle>();
    assert_send::<Statement>();
    assert_send::<TestBlobHandle>();

    let sandbox = TempSandbox::new("g1_facade");
    let db_path = sandbox.path().join("shared.db");
    let conn = Arc::new(Connection::open(&db_path).unwrap());
    conn.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, tag TEXT);")
        .unwrap();

    // Multiple owned `Statement` handles + `TestBlobHandle` created from `&Connection`
    // simultaneously with zero `'conn` lifetime borrow!
    let mut s_ins = conn.prepare("INSERT INTO items VALUES (?1, ?2);").unwrap();
    let mut s_sel = conn
        .prepare("SELECT id, tag FROM items ORDER BY id;")
        .unwrap();
    let blob_rep = conn.open_test_blob_handle(false).unwrap();

    s_ins.bind(1, 1i64).unwrap().bind(2, "alpha").unwrap();
    assert_eq!(s_ins.step().unwrap(), StepResult::Done);
    s_ins.reset().unwrap();
    s_ins.bind(1, 2i64).unwrap().bind(2, "beta").unwrap();
    assert_eq!(s_ins.step().unwrap(), StepResult::Done);

    // Move `s_sel` across a thread boundary (`Statement: Send`) and step it there!
    let rows_from_worker = thread::spawn(move || {
        let mut out = Vec::new();
        while let StepResult::Row(r) = s_sel.step().unwrap() {
            out.push((r.get_i64(0).unwrap(), r.get_str(1).unwrap().to_owned()));
        }
        s_sel.finalize().unwrap();
        out
    })
    .join()
    .unwrap();
    assert_eq!(
        rows_from_worker,
        vec![(1, "alpha".to_owned()), (2, "beta".to_owned())]
    );

    s_ins.finalize().unwrap();
    blob_rep.close().unwrap();

    // Share `Arc<Connection>` across 4 concurrent threads: competing entries never deadlock
    // (returning either `Ok` or `ErrorCode::Busy`), and lock-free atomic mirrors stay readable!
    let barrier = Arc::new(Barrier::new(4));
    let mut handles = Vec::new();
    for tid in 0..4i64 {
        let c = Arc::clone(&conn);
        let b = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            b.wait();
            let mut inserted = 0usize;
            for seq in 0..15i64 {
                let id = 100 + tid * 100 + seq;
                loop {
                    match c.execute_params(
                        "INSERT INTO items VALUES (?1, ?2);",
                        &[Value::Integer(id), Value::Text(format!("t{tid}-{seq}"))],
                    ) {
                        Ok(n) => {
                            assert_eq!(n, 1);
                            inserted += 1;
                            let _ = c.last_insert_rowid();
                            let _ = c.changes();
                            let _ = c.total_changes();
                            let _ = c.is_autocommit();
                            break;
                        }
                        Err(e) if e.code == ErrorCode::Busy => {
                            thread::yield_now();
                        }
                        Err(other) => panic!("unexpected multithreaded error: {other}"),
                    }
                }
            }
            inserted
        }));
    }
    let total_inserted: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total_inserted, 60);
    assert_eq!(conn.total_changes(), 62);

    let count_row = conn
        .query_row("SELECT COUNT(*) FROM items;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(count_row.get_i64(0), Some(62));

    let owned_conn = Arc::try_unwrap(conn).expect("sole Arc owner");
    owned_conn.try_close().unwrap();
}

// ============================================================================
// Group 2: `Connection::try_close` & `Statement::finalize` Contract
// ============================================================================

#[test]
fn test_group2_try_close_busy_recovery_explicit_finalize_and_facade_drop_lifetime() {
    let sandbox = TempSandbox::new("g2_close");
    let db_path = sandbox.path().join("close.db");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute("CREATE TABLE t(x INT PRIMARY KEY); INSERT INTO t VALUES (10), (20);")
        .unwrap();

    // 1. Prepared-but-unstepped `Statement` MUST block `try_close` with `ErrorCode::Busy`
    //    and hand back an intact, usable `Connection` facade!
    let unstepped = conn.prepare("SELECT x FROM t ORDER BY x;").unwrap();
    let CloseError {
        connection: conn,
        error: busy_err,
    } = conn
        .try_close()
        .expect_err("unstepped Statement must block try_close");
    assert_eq!(busy_err.code, ErrorCode::Busy);

    // Recovered facade executes SQL normally!
    conn.execute("INSERT INTO t VALUES (30);").unwrap();

    // 2. Open `TestBlobHandle` representative also blocks `try_close` with `ErrorCode::Busy`!
    unstepped.finalize().unwrap();
    let test_blob = conn.open_test_blob_handle(false).unwrap();
    let CloseError {
        connection: conn,
        error: blob_busy,
    } = conn
        .try_close()
        .expect_err("open TestBlobHandle must block try_close");
    assert_eq!(blob_busy.code, ErrorCode::Busy);
    test_blob.close().unwrap();

    // 3. `Statement::finalize` surfaces prior step error (`sqlite3_finalize` parity) and unregisters once.
    let mut dup_stmt = conn.prepare("INSERT INTO t VALUES (10);").unwrap();
    let step_err = dup_stmt.step().unwrap_err();
    assert_eq!(step_err.code, ErrorCode::Constraint);
    let fin_err = dup_stmt.finalize().unwrap_err();
    assert_eq!(fin_err.code, ErrorCode::Constraint);

    // Now open handle table is empty -> `try_close` succeeds!
    conn.try_close().unwrap();

    // 4. Dropping `Connection` facade while a live `Statement` exists does NOT close `Btree` early!
    let conn2 = Connection::open(&db_path).unwrap();
    let mut survivor = conn2.prepare("SELECT x FROM t ORDER BY x;").unwrap();
    assert_eq!(
        survivor.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(10)]))
    );
    drop(conn2);

    // `survivor` continues stepping cleanly even after `conn2` facade was dropped!
    assert_eq!(
        survivor.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(20)]))
    );
    assert_eq!(
        survivor.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(30)]))
    );
    assert_eq!(survivor.step().unwrap(), StepResult::Done);
    survivor.finalize().unwrap();
}

// ============================================================================
// Group 3: Entry Gate (`Misuse` vs `Busy`), `InterruptHandle`, Scalar Mirrors,
//          `pending_drops` Atomic Gate Handoff, & Mutex / Cleanup Poison Recovery
// ============================================================================

#[test]
fn test_group3_entry_gate_reentrancy_interrupt_mirrors_pending_drops_and_poison_recovery() {
    let sandbox = TempSandbox::new("g3_gate");
    let db_path = sandbox.path().join("gate.db");
    let conn = Arc::new(Connection::open(&db_path).unwrap());
    conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT);")
        .unwrap();

    // Prepare statements and handles BEFORE entering the active execution callback hook
    let mut other_stmt = conn.prepare("SELECT id FROM t;").unwrap();
    let mut drop_inside_hook_stmt = conn.prepare("SELECT id, val FROM t;").unwrap();
    let drop_inside_hook_blob = conn.open_test_blob_handle(false).unwrap();

    // 1. Same-thread callback hook (`execute_and_invoke_hook_for_testing`):
    conn.execute_and_invoke_hook_for_testing(
        "INSERT INTO t VALUES (10, 'ten'), (20, 'twenty');",
        |c| {
            // Lock-free scalar mirrors & immutable metadata succeed and reflect freshly published values!
            assert_eq!(c.last_insert_rowid(), 20);
            assert_eq!(c.changes(), 2);
            assert_eq!(c.total_changes(), 2);
            assert!(c.is_autocommit());
            assert_eq!(c.rollback_count(), 0);
            assert!(!c.is_readonly());
            assert!(c.db_path().is_some());
            assert_eq!(c.database_kind(), DatabaseKind::NamedDisk);

            // `InterruptHandle` operates strictly on the atomic flag without entering the gate!
            let ih = c.interrupt_handle();
            assert!(!ih.is_interrupted());
            ih.interrupt();
            assert!(ih.is_interrupted());
            ih.reset();
            assert!(!ih.is_interrupted());

            // Same-thread re-entry into ANY gate-protected API returns `ErrorCode::Misuse` immediately!
            assert_eq!(c.execute("SELECT 1;").unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(c.prepare("SELECT 1;").unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(c.schema_snapshot().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(c.database_header().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(c.page_size().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(c.page_count().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(other_stmt.step().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(other_stmt.reset().unwrap_err().code, ErrorCode::Misuse);
            assert_eq!(
                other_stmt.bind(1, 1i64).unwrap_err().code,
                ErrorCode::Misuse
            );
            assert_eq!(
                other_stmt.clear_bindings().unwrap_err().code,
                ErrorCode::Misuse
            );

            // Dropping owned `Statement` and `TestBlobHandle` inside the callback enqueues onto
            // `pending_drops` WITHOUT deadlocking on `ConnectionInner`!
            let _ = &mut drop_inside_hook_stmt;
            drop(drop_inside_hook_stmt);
            drop(drop_inside_hook_blob);
        },
    )
    .unwrap();

    other_stmt.finalize().unwrap();

    // 2. Cross-thread contention (`ErrorCode::Busy`) & cross-thread `pending_drops` handoff:
    let mut stmt_dropped_cross_thread = conn.prepare("SELECT id FROM t ORDER BY id;").unwrap();
    assert!(matches!(
        stmt_dropped_cross_thread.step().unwrap(),
        StepResult::Row(_)
    ));
    let blob_dropped_cross_thread = conn.open_test_blob_handle(false).unwrap();

    let entered_gate = Arc::new(Barrier::new(2));
    let release_gate = Arc::new(Barrier::new(2));
    let conn_bg = Arc::clone(&conn);
    let entered_bg = Arc::clone(&entered_gate);
    let release_bg = Arc::clone(&release_gate);

    let bg = thread::spawn(move || {
        entered_bg.wait();
        // While main thread holds the execution gate, another thread gets `ErrorCode::Busy`!
        assert_eq!(
            conn_bg
                .execute("INSERT INTO t VALUES (30, 'thirty');")
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        assert_eq!(conn_bg.page_count().unwrap_err().code, ErrorCode::Busy);
        assert_eq!(
            conn_bg.prepare("SELECT 1;").unwrap_err().code,
            ErrorCode::Busy
        );

        // Dropping handles from Thread 2 while Thread 1 holds the gate enqueues onto `pending_drops`
        // without blocking and MUST be drained before Thread 1 releases the gate!
        drop(stmt_dropped_cross_thread);
        drop(blob_dropped_cross_thread);
        release_bg.wait();
    });

    conn.with_active_execution_hook_for_testing(|_c| {
        entered_gate.wait();
        release_gate.wait();
    })
    .unwrap();
    bg.join().unwrap();

    // Immediately calling `try_close()` (with ZERO extra dummy SQL calls!) MUST succeed because
    // both same-thread and cross-thread `pending_drops` were drained before gate release!
    assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);
    let conn = Arc::try_unwrap(conn).expect("sole Arc<Connection> owner");
    conn.try_close().unwrap();

    // 3. Asynchronous `InterruptHandle` aborts both VDBE and interpreter (`GROUP BY` / `CTE`) queries!
    let conn_int = Connection::open(&db_path).unwrap();
    conn_int.execute("BEGIN;").unwrap();
    for i in 100..=600i64 {
        conn_int
            .execute_params(
                "INSERT INTO t VALUES (?1, ?2);",
                &[Value::Integer(i), Value::Text(format!("val-{i}"))],
            )
            .unwrap();
    }
    conn_int.execute("COMMIT;").unwrap();

    let ih = conn_int.interrupt_handle();
    ih.interrupt();
    assert_eq!(
        conn_int
            .query("SELECT id, val FROM t WHERE val LIKE '%999%';", &[])
            .unwrap_err()
            .code,
        ErrorCode::Interrupt
    );
    // Interpreter CTE + GROUP BY path also honors `InterruptHandle`!
    assert_eq!(
        conn_int
            .query(
                "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 10000)
                 SELECT val, COUNT(*) FROM t GROUP BY val;",
                &[],
            )
            .unwrap_err()
            .code,
        ErrorCode::Interrupt
    );
    ih.reset();
    assert_eq!(
        conn_int
            .query_row("SELECT COUNT(*) FROM t;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(503)
    );

    // 4. Panic inside execution lease (`catch_unwind`) recovers cleanly without panicking in `Drop`!
    let mut panic_survivor_stmt = conn_int.prepare("SELECT COUNT(*) FROM t;").unwrap();
    let unwind_res = catch_unwind(AssertUnwindSafe(|| {
        let _ = conn_int.with_active_execution_hook_for_testing(|_c| {
            panic!("simulated panic while holding ConnectionInner guard and gate");
        });
    }));
    assert!(unwind_res.is_err());
    // Gate was released by RAII `ConnSession::drop`, and `Drop` never panics!
    assert!(matches!(
        panic_survivor_stmt.step().unwrap(),
        StepResult::Row(_)
    ));
    drop(panic_survivor_stmt);
    conn_int.try_close().unwrap();

    // 5. Simulated cleanup I/O error transitions connection to `Poisoned(SqliteError)` preserving root error!
    let conn_poison = Connection::open_in_memory().unwrap();
    conn_poison
        .execute("CREATE TABLE p(x INT); INSERT INTO p VALUES (1), (2);")
        .unwrap();
    let mut poison_stmt = conn_poison.prepare("SELECT x FROM p;").unwrap();
    assert!(matches!(poison_stmt.step().unwrap(), StepResult::Row(_)));
    conn_poison
        .set_simulated_cleanup_io_error_for_testing(Some(SqliteError::new(
            ErrorCode::IoErr,
            "simulated disk I/O error during cursor/tx cleanup",
        )))
        .unwrap();

    let rst_err = poison_stmt.reset().unwrap_err();
    assert_eq!(rst_err.code, ErrorCode::IoErr);
    assert!(rst_err.message.contains("simulated disk I/O error"));

    // Subsequent SQL is rejected with the preserved root I/O error, while scalar mirrors stay readable!
    let sub_err = conn_poison.execute("SELECT 1;").unwrap_err();
    assert_eq!(sub_err.code, ErrorCode::IoErr);
    assert!(sub_err.message.contains("simulated disk I/O error"));
    assert_eq!(conn_poison.total_changes(), 2);
    drop(poison_stmt);
}

// ============================================================================
// Group 4: Lazy Open under External C `EXCLUSIVE`, `DatabaseKind` (`""` vs `":memory:"`),
//          `ConnectionOpenOptions`, & `PRAGMA lock_status`
// ============================================================================

#[test]
fn test_group4_lazy_open_exclusive_lock_database_kind_and_open_options() {
    let sandbox = TempSandbox::new("g4_lazy_open");
    let db_path = sandbox.path().join("lazy.db");

    // Seed database via C SQLite 3.54.1, then hold `BEGIN EXCLUSIVE` in an external C subprocess!
    assert_eq!(
        c_try_sql(
            &db_path,
            "CREATE TABLE t(id INT, name TEXT); INSERT INTO t VALUES (7, 'from_c');"
        ),
        "OK"
    );
    let c_excl = HeldCLock::spawn("hold_exclusive", &db_path);

    // 1. Rust `Connection::open` succeeds lazily (`Open/NoLock`, `schema_loaded == false`)!
    let conn = Connection::open(&db_path).expect("lazy open must succeed under external EXCLUSIVE");
    assert_eq!(conn.database_kind(), DatabaseKind::NamedDisk);
    assert_eq!(
        pragma_lock_status(&conn),
        vec![
            ("main".to_owned(), "unlocked".to_owned()),
            ("temp".to_owned(), "closed".to_owned()),
        ]
    );

    // `BEGIN DEFERRED` and `SAVEPOINT` prepare + step succeed with zero schema/OS lock reads!
    let mut begin_stmt = conn.prepare("BEGIN DEFERRED;").unwrap();
    assert_eq!(begin_stmt.step().unwrap(), StepResult::Done);
    begin_stmt.finalize().unwrap();
    assert!(!conn.is_autocommit());

    let mut sp_stmt = conn.prepare("SAVEPOINT sp_under_excl;").unwrap();
    assert_eq!(sp_stmt.step().unwrap(), StepResult::Done);
    sp_stmt.finalize().unwrap();
    assert_eq!(
        pragma_lock_status(&conn),
        vec![
            ("main".to_owned(), "unlocked".to_owned()),
            ("temp".to_owned(), "closed".to_owned()),
        ]
    );

    // Schema-requiring `SELECT * FROM t` returns `ErrorCode::Busy` while C holds `EXCLUSIVE`!
    assert_eq!(
        conn.prepare("SELECT * FROM t;").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        pragma_lock_status(&conn),
        vec![
            ("main".to_owned(), "unlocked".to_owned()),
            ("temp".to_owned(), "closed".to_owned()),
        ]
    );

    // Once external C releases `EXCLUSIVE`, Rust `SELECT * FROM t` succeeds and reads C's data!
    c_excl.release();
    let row = conn
        .query_row("SELECT id, name FROM t;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row.get_i64(0), Some(7));
    assert_eq!(row.get_str(1), Some("from_c"));
    conn.execute("COMMIT;").unwrap();
    conn.try_close().unwrap();

    // 2. `ConnectionOpenOptions`: 4096 production vs 1024 testfixture, and candidate override by external 4096 DB!
    let db_4096 = sandbox.path().join("p4096.db");
    let c4096 = Connection::open(&db_4096).unwrap();
    c4096.execute("CREATE TABLE a(x);").unwrap();
    assert_eq!(c4096.page_size().unwrap(), 4096);
    c4096.try_close().unwrap();

    let db_1024 = sandbox.path().join("p1024.db");
    let opts_1024 = ConnectionOpenOptions {
        default_page_size: 1024,
        lock_geometry: LockGeometry::default(),
        temp_store_memory: false,
        ..ConnectionOpenOptions::default()
    };
    let c1024 = Connection::open_with_options(
        &db_1024,
        OpenFlags::READWRITE | OpenFlags::CREATE,
        opts_1024,
    )
    .unwrap();
    c1024.execute("CREATE TABLE a(x);").unwrap();
    assert_eq!(c1024.page_size().unwrap(), 1024);
    c1024.try_close().unwrap();
    assert_eq!(c_try_sql(&db_1024, "PRAGMA integrity_check;"), "OK");

    // Open an empty file lazily with `default_page_size: 1024`, then populate externally with 4096 BEFORE first read!
    let db_override = sandbox.path().join("override.db");
    fs::write(&db_override, b"").unwrap();
    let lazy_1024 = Connection::open_with_options(
        &db_override,
        OpenFlags::READWRITE | OpenFlags::CREATE,
        opts_1024,
    )
    .unwrap();
    assert_eq!(
        c_try_sql(
            &db_override,
            "PRAGMA page_size = 4096; CREATE TABLE ext(v INT); INSERT INTO ext VALUES (99);"
        ),
        "OK"
    );
    assert_eq!(lazy_1024.page_size().unwrap(), 4096);
    assert_eq!(
        lazy_1024
            .query_row("SELECT v FROM ext;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(99)
    );
    lazy_1024.try_close().unwrap();

    // 3. `Connection::open("")` (`PrivateTemporary`) vs `Connection::open(":memory:")` (`Memory`)
    let mem_conn = Connection::open(":memory:").unwrap();
    assert_eq!(mem_conn.database_kind(), DatabaseKind::Memory);
    assert_eq!(mem_conn.db_path(), None);
    assert_eq!(
        pragma_lock_status(&mem_conn),
        vec![
            ("main".to_owned(), "unknown".to_owned()),
            ("temp".to_owned(), "closed".to_owned()),
        ]
    );

    let tmp1 = Connection::open("").unwrap();
    let tmp2 = Connection::open("").unwrap();
    assert_eq!(tmp1.database_kind(), DatabaseKind::PrivateTemporary);
    assert_eq!(tmp2.database_kind(), DatabaseKind::PrivateTemporary);
    assert_eq!(tmp1.db_path(), None);
    assert_eq!(tmp2.db_path(), None);

    tmp1.execute("CREATE TABLE only_in_1(x INT); INSERT INTO only_in_1 VALUES (42);")
        .unwrap();
    assert!(tmp2.prepare("SELECT * FROM only_in_1;").is_err());
    assert!(mem_conn.prepare("SELECT * FROM only_in_1;").is_err());
    tmp1.try_close().unwrap();
    tmp2.try_close().unwrap();
    mem_conn.try_close().unwrap();
}

// ============================================================================
// Group 5: `ActivityTracker` & Lock Transitions (`Write -> Read`, `RETURNING`,
//          Retryable `COMMIT`, `lock7-1.1..1.8`, `DROP`/`VACUUM`, & Rollback Trip)
// ============================================================================

#[test]
fn test_group5_activity_tracker_lock_transitions_returning_lock7_drop_vacuum_and_rollback_trip() {
    let sandbox = TempSandbox::new("g5_activity");
    let db_path = sandbox.path().join("activity.db");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, grp TEXT, v INT);
         INSERT INTO t VALUES (1, 'a', 10), (2, 'a', 20), (3, 'b', 30);",
    )
    .unwrap();
    assert_eq!(
        pragma_lock_status(&conn),
        vec![
            ("main".to_owned(), "unlocked".to_owned()),
            ("temp".to_owned(), "closed".to_owned()),
        ]
    );

    // 1. Two paused `SELECT`s (one VDBE `SELECT`, one interpreter `GROUP BY` `SELECT`) hold `SHARED`!
    let mut sel_vdbe = conn.prepare("SELECT id, v FROM t ORDER BY id;").unwrap();
    let mut sel_interp = conn
        .prepare("SELECT grp, SUM(v) FROM t GROUP BY grp ORDER BY grp;")
        .unwrap();
    // Prepared-but-unstepped handles are NOT active readers (`unlocked` -> external C writer succeeds!)
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    assert_eq!(
        c_try_sql(&db_path, "INSERT INTO t VALUES (4, 'b', 40);"),
        "OK"
    );

    assert!(matches!(sel_vdbe.step().unwrap(), StepResult::Row(_)));
    assert!(matches!(sel_interp.step().unwrap(), StepResult::Row(_)));
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );
    assert!(
        c_try_sql(&db_path, "INSERT INTO t VALUES (5, 'c', 50);").starts_with("ERR 5 "),
        "external C writer must get SQLITE_BUSY (5) while Rust holds SHARED"
    );

    // Finishing the FIRST `SELECT` (`sel_vdbe.reset()`) MUST keep `main | shared` for `sel_interp`!
    sel_vdbe.reset().unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );
    assert!(c_try_sql(&db_path, "INSERT INTO t VALUES (5, 'c', 50);").starts_with("ERR 5 "));

    // Autocommit `UPDATE` on the same `Connection` while `sel_interp` is paused at `Row`
    // MUST commit via **Write -> Read** downgrade (`main | shared` preserved)!
    conn.execute("UPDATE t SET v = 11 WHERE id = 1;").unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );
    assert!(c_try_sql(&db_path, "INSERT INTO t VALUES (5, 'c', 50);").starts_with("ERR 5 "));

    // Stepping `sel_interp` past its last row to `Done` releases the last reader -> `main | unlocked`!
    assert!(matches!(sel_interp.step().unwrap(), StepResult::Row(_)));
    assert_eq!(sel_interp.step().unwrap(), StepResult::Done);
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    assert_eq!(
        c_try_sql(&db_path, "INSERT INTO t VALUES (5, 'c', 50);"),
        "OK"
    );
    sel_vdbe.finalize().unwrap();
    sel_interp.finalize().unwrap();

    // 2. Active writable `TestBlobHandle` prevents premature autocommit commit!
    let wblob = conn.open_test_blob_handle(true).unwrap();
    conn.execute("INSERT INTO t VALUES (6, 'c', 60);").unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "reserved".to_owned())
    );
    // Closing `wblob` completes the write lease and commits cleanly -> `unlocked`!
    wblob.close().unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    // 3. Paused `RETURNING` DML keeps write lease active, blocks `COMMIT`/`SAVEPOINT`/`RELEASE` with `Busy`,
    //    and matches C SQLite 3.54.1 on both `Done` and early `reset()`!
    let mut ret_stmt = conn
        .prepare("INSERT INTO t VALUES (7, 'd', 70), (8, 'd', 80) RETURNING id;")
        .unwrap();
    assert_eq!(
        ret_stmt.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(7)]))
    );
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "reserved".to_owned())
    );
    assert!(c_try_sql(&db_path, "INSERT INTO t VALUES (99, 'z', 0);").starts_with("ERR 5 "));
    assert_eq!(
        conn.execute("SAVEPOINT sp_busy;").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        ret_stmt.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Integer(8)]))
    );
    assert_eq!(ret_stmt.step().unwrap(), StepResult::Done);
    ret_stmt.finalize().unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    // Differential check against C SQLite 3.54.1 for early `reset()` on `INSERT ... RETURNING`!
    let c_ret_db = sandbox.path().join("c_ret.db");
    let c_ret_out = Command::new(c_lifecycle_helper())
        .arg("diff_returning_early_reset")
        .arg(&c_ret_db)
        .output()
        .unwrap();
    assert!(c_ret_out.status.success());
    let c_ret_str = String::from_utf8(c_ret_out.stdout).unwrap();

    let rs_ret_conn = Connection::open_in_memory().unwrap();
    rs_ret_conn
        .execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT);")
        .unwrap();
    let mut rs_ret_stmt = rs_ret_conn
        .prepare("INSERT INTO t VALUES (10, 'a'), (20, 'b') RETURNING id;")
        .unwrap();
    let StepResult::Row(first_r) = rs_ret_stmt.step().unwrap() else {
        panic!("expected first RETURNING row");
    };
    let rs_first_id = first_r.get_i64(0).unwrap();
    let rs_reset_rc = match rs_ret_stmt.reset() {
        Ok(()) => 0,
        Err(e) => e.code as i32,
    };
    rs_ret_stmt.finalize().unwrap();
    let rs_autocommit = i32::from(rs_ret_conn.is_autocommit());
    let rs_rows: Vec<String> = rs_ret_conn
        .query("SELECT id FROM t ORDER BY id;", &[])
        .unwrap()
        .rows
        .iter()
        .map(|r| r.get_i64(0).unwrap().to_string())
        .collect();
    let rs_ret_str = format!(
        "FIRST={rs_first_id} RESET_RC={rs_reset_rc} AUTOCOMMIT={rs_autocommit} ROWS={}\n",
        rs_rows.join(",")
    );
    assert_eq!(
        rs_ret_str, c_ret_str,
        "early reset on RETURNING DML must match C SQLite 3.54.1 100%"
    );

    // 4. Explicit `BEGIN; INSERT ...;`:
    //    - Active read-only `SELECT` does NOT block `COMMIT`, and `COMMIT` downgrades `Write -> Read` (`shared`)!
    //    - Active writer (`RETURNING`) DOES block `COMMIT` and inner/outer `RELEASE` with `ErrorCode::Busy`!
    conn.execute("SAVEPOINT outer_sp; SAVEPOINT inner_sp; INSERT INTO t VALUES (9, 'e', 90);")
        .unwrap();
    let mut paused_reader = conn.prepare("SELECT id FROM t ORDER BY id;").unwrap();
    assert!(matches!(paused_reader.step().unwrap(), StepResult::Row(_)));

    let mut paused_writer = conn
        .prepare("INSERT INTO t VALUES (10, 'e', 100) RETURNING id;")
        .unwrap();
    assert!(matches!(paused_writer.step().unwrap(), StepResult::Row(_)));
    assert_eq!(
        conn.execute("RELEASE inner_sp;").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        conn.execute("RELEASE outer_sp;").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(conn.execute("COMMIT;").unwrap_err().code, ErrorCode::Busy);

    // Finish `paused_writer` -> now `RELEASE inner_sp` AND outermost `RELEASE outer_sp` succeed,
    // downgrading `Write -> Read` (`main | shared`) because `paused_reader` is still at `Row`!
    assert_eq!(paused_writer.step().unwrap(), StepResult::Done);
    paused_writer.finalize().unwrap();
    conn.execute("RELEASE inner_sp;").unwrap();
    conn.execute("RELEASE outer_sp;").unwrap();
    assert!(conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );
    paused_reader.finalize().unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    // 5. Retryable `COMMIT` when external C reader holds `SHARED`:
    conn.execute("BEGIN; INSERT INTO t VALUES (11, 'f', 110);")
        .unwrap();
    let c_shared = HeldCLock::spawn("hold_shared", &db_path);
    let mut commit_stmt = conn.prepare("COMMIT;").unwrap();
    assert_eq!(commit_stmt.step().unwrap_err().code, ErrorCode::Busy);
    assert!(
        !conn.is_autocommit(),
        "tx must stay open on retryable COMMIT Busy"
    );
    c_shared.release();
    assert_eq!(commit_stmt.step().unwrap(), StepResult::Done);
    assert!(conn.is_autocommit());
    commit_stmt.finalize().unwrap();

    // 6. `lock7-1.1 .. lock7-1.8` sequence:
    let lock7_db = sandbox.path().join("lock7.db");
    {
        let seed = Connection::open(&lock7_db).unwrap();
        seed.execute("CREATE TABLE t1(a, b);").unwrap();
        seed.try_close().unwrap();
    }
    let db1 = Connection::open(&lock7_db).unwrap();
    let db2 = Connection::open(&lock7_db).unwrap();
    db1.execute("BEGIN;").unwrap();
    db2.execute("BEGIN;").unwrap();
    assert_eq!(
        pragma_lock_status(&db1)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    assert_eq!(
        pragma_lock_status(&db2)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    db1.execute("INSERT INTO t1 VALUES (1, 1);").unwrap();
    assert_eq!(
        db2.execute("INSERT INTO t1 VALUES (2, 2);")
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert!(!db2.is_autocommit());
    assert_eq!(
        pragma_lock_status(&db1)[0],
        ("main".to_owned(), "reserved".to_owned())
    );
    assert_eq!(
        pragma_lock_status(&db2)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    db1.execute("COMMIT;").unwrap();
    db2.execute("ROLLBACK;").unwrap();
    db1.try_close().unwrap();
    db2.try_close().unwrap();

    // 7. `DROP TABLE` -> `ErrorCode::Locked` vs `VACUUM` -> `ErrorCode::Error`!
    conn.execute("CREATE TABLE drop_me(x INT); INSERT INTO drop_me VALUES (1);")
        .unwrap();
    let mut active_sel = conn.prepare("SELECT id FROM t ORDER BY id;").unwrap();
    assert!(matches!(active_sel.step().unwrap(), StepResult::Row(_)));

    assert_eq!(
        conn.execute("DROP TABLE drop_me;").unwrap_err().code,
        ErrorCode::Locked
    );
    assert_eq!(conn.execute("VACUUM;").unwrap_err().code, ErrorCode::Error);
    active_sel.finalize().unwrap();

    conn.execute("BEGIN;").unwrap();
    assert_eq!(conn.execute("VACUUM;").unwrap_err().code, ErrorCode::Error);
    conn.execute("ROLLBACK;").unwrap();
    conn.execute("DROP TABLE drop_me;").unwrap();

    // 8. Rollback cursor tripping (`write_only == true` continues; `write_only == false` -> `SQLITE_ABORT_ROLLBACK`)
    //    verified for BOTH VDBE `SELECT` and interpreter (`GROUP BY`) `SELECT`!
    conn.execute("BEGIN;").unwrap();
    let mut trip_vdbe = conn.prepare("SELECT id FROM t ORDER BY id;").unwrap();
    let mut trip_interp = conn
        .prepare("SELECT grp, COUNT(*) FROM t GROUP BY grp ORDER BY grp;")
        .unwrap();
    assert!(matches!(trip_vdbe.step().unwrap(), StepResult::Row(_)));
    assert!(matches!(trip_interp.step().unwrap(), StepResult::Row(_)));

    // Pure DML rollback (`write_only == true`): both read cursors reseek/continue without `Abort`!
    conn.execute("INSERT INTO t VALUES (500, 'z', 500);")
        .unwrap();
    conn.execute("ROLLBACK;").unwrap();
    assert!(matches!(trip_vdbe.step().unwrap(), StepResult::Row(_)));
    assert!(matches!(trip_interp.step().unwrap(), StepResult::Row(_)));
    trip_vdbe.finalize().unwrap();
    trip_interp.finalize().unwrap();

    // DDL rollback (`write_only == false`): both VDBE and interpreter `SELECT` return `SQLITE_ABORT_ROLLBACK`!
    conn.execute("BEGIN;").unwrap();
    let mut abort_vdbe = conn.prepare("SELECT id FROM t ORDER BY id;").unwrap();
    let mut abort_interp = conn
        .prepare("SELECT grp, COUNT(*) FROM t GROUP BY grp ORDER BY grp;")
        .unwrap();
    assert!(matches!(abort_vdbe.step().unwrap(), StepResult::Row(_)));
    assert!(matches!(abort_interp.step().unwrap(), StepResult::Row(_)));

    conn.execute("CREATE TABLE transient_ddl(k INT);").unwrap();
    conn.execute("ROLLBACK;").unwrap();

    let err_v = abort_vdbe.step().unwrap_err();
    assert_eq!(err_v.code, ErrorCode::Abort);
    assert_eq!(err_v.extended_code, extended::SQLITE_ABORT_ROLLBACK);

    let err_i = abort_interp.step().unwrap_err();
    assert_eq!(err_i.code, ErrorCode::Abort);
    assert_eq!(err_i.extended_code, extended::SQLITE_ABORT_ROLLBACK);

    // `sqlite3_finalize` without an intervening `reset()` surfaces the prior `step()` error
    // while still unregistering the handle cleanly so `conn.try_close()` succeeds:
    assert_eq!(
        abort_vdbe.finalize().unwrap_err().extended_code,
        extended::SQLITE_ABORT_ROLLBACK
    );
    assert_eq!(
        abort_interp.finalize().unwrap_err().extended_code,
        extended::SQLITE_ABORT_ROLLBACK
    );
    conn.try_close().unwrap();
}

// ============================================================================
// Group 6: Connection-Level Named SQL `SAVEPOINT` Stack & Deferred FK Restoration
// ============================================================================

#[test]
fn test_group6_connection_level_savepoint_stack_duplicate_names_and_deferred_fk_restoration() {
    let sandbox = TempSandbox::new("g6_savepoint");
    let db_path = sandbox.path().join("sp.db");

    {
        let seed = Connection::open(&db_path).unwrap();
        seed.execute("CREATE TABLE log(x INT);").unwrap();
        seed.try_close().unwrap();
    }

    // 1. Zero-lock `SAVEPOINT` in autocommit works on a `READONLY` connection!
    let ro_conn = Connection::open_with_flags(&db_path, OpenFlags::READONLY).unwrap();
    assert!(ro_conn.is_readonly());
    assert!(ro_conn.is_autocommit());
    ro_conn.execute("SAVEPOINT ro_sp;").unwrap();
    assert!(!ro_conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&ro_conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    ro_conn
        .execute("ROLLBACK TO ro_sp; RELEASE ro_sp;")
        .unwrap();
    assert!(ro_conn.is_autocommit());
    ro_conn.try_close().unwrap();

    // 2. Duplicate savepoint names resolve innermost-first (case-insensitively), plus statement-level ABORT!
    let conn = Connection::open(&db_path).unwrap();
    conn.execute("CREATE TABLE uniq_tbl(k INT PRIMARY KEY);")
        .unwrap();
    conn.execute(
        "SAVEPOINT a;
         INSERT INTO uniq_tbl VALUES (1);
         SAVEPOINT A;
         INSERT INTO uniq_tbl VALUES (2);",
    )
    .unwrap();
    // Statement-level ABORT inside inner `SAVEPOINT A`: multi-row insert failing on 2nd row rolls back
    // ONLY `(10)` while keeping `(1)` and `(2)`!
    assert_eq!(
        conn.execute("INSERT INTO uniq_tbl VALUES (10), (2);")
            .unwrap_err()
            .code,
        ErrorCode::Constraint
    );
    // `ROLLBACK TO a` resolves to innermost `A`, rolling back `(2)` while preserving `(1)`!
    conn.execute("ROLLBACK TO a; RELEASE A; RELEASE a;")
        .unwrap();
    assert!(conn.is_autocommit());
    let rows = conn
        .query("SELECT k FROM uniq_tbl ORDER BY k;", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get_i64(0), Some(1));

    // 3. Full deferred FK suspect restoration across `SAVEPOINT` + `ROLLBACK TO` + `COMMIT`!
    conn.execute(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE parent(id INTEGER PRIMARY KEY);
         CREATE TABLE child(
             id INTEGER PRIMARY KEY,
             pid INT REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED
         );
         INSERT INTO parent VALUES (1);",
    )
    .unwrap();

    // Case 3A: Orphan added inside `sp1`, then `ROLLBACK TO sp1` -> `COMMIT` succeeds!
    conn.execute(
        "BEGIN;
         INSERT INTO child VALUES (10, 1);
         SAVEPOINT sp1;
         INSERT INTO child VALUES (11, 999);
         ROLLBACK TO sp1;
         RELEASE sp1;
         COMMIT;",
    )
    .unwrap();

    // Case 3B: Orphan created BEFORE `sp2`, repaired INSIDE `sp2`, then `ROLLBACK TO sp2` ->
    // `COMMIT` MUST fail with `ErrorCode::Constraint` because the pre-savepoint suspect state was restored!
    conn.execute(
        "BEGIN;
         INSERT INTO child VALUES (20, 888);
         SAVEPOINT sp2;
         INSERT INTO parent VALUES (888);
         ROLLBACK TO sp2;
         RELEASE sp2;",
    )
    .unwrap();
    let fk_err = conn.execute("COMMIT;").unwrap_err();
    assert_eq!(fk_err.code, ErrorCode::Constraint);
    conn.execute("ROLLBACK;").unwrap();
    conn.try_close().unwrap();
}

// ============================================================================
// Group 7: `prepare` (`V2` vs `V1`), Semantic Validation, `reprepare`, `autoreset`,
//          `reset`, `bind` READY Check, & `clear_bindings` (with C Differential Check)
// ============================================================================

#[test]
fn test_group7_prepare_v2_vs_v1_validation_reprepare_autoreset_reset_bind_and_clear_bindings() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT UNIQUE);")
        .unwrap();

    // 1. Prepare-time semantic validation with ZERO side effects for both VDBE and interpreter queries:
    assert!(conn.prepare("SELECT * FROM missing_tbl;").is_err());
    assert!(conn.prepare("SELECT missing_col FROM users;").is_err());
    assert!(conn.prepare("SELECT no_such_fn(1) FROM users;").is_err());
    assert!(conn.prepare("SELECT coalesce(1) FROM users;").is_err());
    // Interpreter CTE / GROUP BY also validates at `prepare` time!
    assert!(
        conn.prepare("WITH c AS (SELECT 1 AS a) SELECT bogus_col FROM c GROUP BY a;")
            .is_err()
    );
    assert!(
        conn.prepare("INSERT INTO users VALUES (1, 'a', 'extra');")
            .is_err()
    );
    assert!(conn.prepare("CREATE TABLE users(x INT);").is_err());

    // Valid DML `prepare` has ZERO side effects before `step()`!
    let prep_ins = conn
        .prepare("INSERT INTO users VALUES (1, 'Ada');")
        .unwrap();
    assert_eq!(prep_ins.column_names().len(), 0);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM users;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(0)
    );
    assert_eq!(conn.total_changes(), 0);
    prep_ins.finalize().unwrap();

    // Column metadata at `prepare` time:
    let s_ret = conn
        .prepare("INSERT INTO users VALUES (1, 'Ada') RETURNING id, name;")
        .unwrap();
    assert_eq!(s_ret.column_names(), &["id", "name"]);
    s_ret.finalize().unwrap();

    let s_exp = conn.prepare("EXPLAIN SELECT * FROM users;").unwrap();
    assert_eq!(s_exp.column_names().len(), 8);
    s_exp.finalize().unwrap();

    let s_eqp = conn
        .prepare("EXPLAIN QUERY PLAN SELECT * FROM users WHERE id = 1;")
        .unwrap();
    assert_eq!(s_eqp.column_names().len(), 4);
    s_eqp.finalize().unwrap();

    // 2. `PrepareMode::V2` automatic `reprepare` vs `PrepareMode::V1` `ErrorCode::Schema` on `ALTER TABLE`!
    conn.execute("INSERT INTO users VALUES (1, 'Ada');")
        .unwrap();
    let mut v2_sel = conn.prepare("SELECT * FROM users WHERE id = ?1;").unwrap();
    let mut v1_sel = conn
        .prepare_v1("SELECT * FROM users WHERE id = ?1;")
        .unwrap();
    assert_eq!(v1_sel.prepare_mode(), PrepareMode::V1);
    v2_sel.bind(1, 1i64).unwrap();
    v1_sel.bind(1, 1i64).unwrap();

    conn.execute("ALTER TABLE users ADD COLUMN role TEXT DEFAULT 'admin';")
        .unwrap();

    // `prepare_v1` does NOT auto-reprepare -> returns `ErrorCode::Schema`, and `finalize()`
    // without prior `reset()` surfaces that recorded `Schema` error (`sqlite3_finalize`):
    assert_eq!(v1_sel.step().unwrap_err().code, ErrorCode::Schema);
    assert_eq!(v1_sel.finalize().unwrap_err().code, ErrorCode::Schema);

    // `prepare` (`V2`) auto-reprepares, transfers bound `?1 = 1`, and refreshes `column_names`!
    let StepResult::Row(r_v2) = v2_sel.step().unwrap() else {
        panic!("expected row after V2 reprepare");
    };
    assert_eq!(v2_sel.column_names(), &["id", "name", "role"]);
    assert_eq!(
        r_v2.values(),
        &[
            Value::Integer(1),
            Value::Text("Ada".into()),
            Value::Text("admin".into())
        ]
    );
    assert_eq!(v2_sel.step().unwrap(), StepResult::Done);
    v2_sel.finalize().unwrap();

    // 3. C `autoreset` on `SELECT`, `INSERT`, and post-constraint-error `step()`!
    let mut auto_sel = conn.prepare("SELECT name FROM users ORDER BY id;").unwrap();
    assert_eq!(
        auto_sel.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Text("Ada".into())]))
    );
    assert_eq!(auto_sel.step().unwrap(), StepResult::Done);
    // Calling `step()` again after `Done` (without `reset()`) rewinds and yields the first row again!
    assert_eq!(
        auto_sel.step().unwrap(),
        StepResult::Row(sqliters::Row::new(vec![Value::Text("Ada".into())]))
    );
    assert_eq!(auto_sel.step().unwrap(), StepResult::Done);
    auto_sel.finalize().unwrap();

    // `INSERT` autoreset executes a second real `INSERT`!
    conn.execute("CREATE TABLE seq_log(id INTEGER PRIMARY KEY, tag TEXT);")
        .unwrap();
    let mut auto_ins = conn
        .prepare("INSERT INTO seq_log(tag) VALUES (?1);")
        .unwrap();
    auto_ins.bind(1, "dup-ok").unwrap();
    assert_eq!(auto_ins.step().unwrap(), StepResult::Done);
    assert_eq!(conn.last_insert_rowid(), 1);
    assert_eq!(auto_ins.step().unwrap(), StepResult::Done);
    assert_eq!(conn.last_insert_rowid(), 2);
    auto_ins.finalize().unwrap();

    // Constraint `Err` -> `step()` autoresets and re-executes!
    let mut uniq_ins = conn
        .prepare("INSERT INTO users(id, name) VALUES (2, 'Ada');")
        .unwrap();
    assert_eq!(uniq_ins.step().unwrap_err().code, ErrorCode::Constraint);
    // Delete conflicting row; next `step()` autoresets and succeeds!
    conn.execute("UPDATE users SET name = 'Ada_old' WHERE id = 1;")
        .unwrap();
    assert_eq!(uniq_ins.step().unwrap(), StepResult::Done);
    uniq_ins.finalize().unwrap();

    // 4. `bind` / `bind_named` / `bind_all` READY check:
    let mut bstmt = conn
        .prepare("SELECT id, :label FROM users WHERE id >= ?2 ORDER BY id;")
        .unwrap();
    assert_eq!(bstmt.parameter_count(), 2);
    bstmt.bind_named(":label", "first").unwrap();
    bstmt.bind(2, 1i64).unwrap();
    assert_eq!(
        bstmt
            .bind_all(&[
                Value::Text("first".into()),
                Value::Integer(1),
                Value::Integer(99),
            ])
            .unwrap_err()
            .code,
        ErrorCode::Range
    );

    let StepResult::Row(r1) = bstmt.step().unwrap() else {
        panic!("expected first row");
    };
    assert_eq!(r1.get_str(1), Some("first"));

    // While paused at `Row`, `bind` / `bind_named` / `bind_all` MUST fail with `ErrorCode::Misuse`
    // and leave existing bindings unchanged!
    assert_eq!(bstmt.bind(2, 999i64).unwrap_err().code, ErrorCode::Misuse);
    assert_eq!(
        bstmt.bind_named(":label", "mutated").unwrap_err().code,
        ErrorCode::Misuse
    );
    assert_eq!(
        bstmt
            .bind_all(&[Value::Text("mutated".into()), Value::Integer(999)])
            .unwrap_err()
            .code,
        ErrorCode::Misuse
    );

    let StepResult::Row(r2) = bstmt.step().unwrap() else {
        panic!("expected second row with unchanged bindings");
    };
    assert_eq!(r2.get_str(1), Some("first"));
    assert_eq!(bstmt.step().unwrap(), StepResult::Done);

    // In `Done` WITHOUT `reset()`, `bind` STILL returns `ErrorCode::Misuse`!
    assert_eq!(bstmt.bind(2, 2i64).unwrap_err().code, ErrorCode::Misuse);

    // After `reset()`, bindings were preserved (`:label == "first"`), and `bind` succeeds!
    bstmt.reset().unwrap();
    bstmt.bind(2, 2i64).unwrap();
    let StepResult::Row(r_after_reset) = bstmt.step().unwrap() else {
        panic!("expected row 2 after reset");
    };
    assert_eq!(r_after_reset.get_i64(0), Some(2));
    assert_eq!(r_after_reset.get_str(1), Some("first"));
    bstmt.finalize().unwrap();

    // 5. `clear_bindings()` in `Row` AND in `Done` (`vdbeapi.c:sqlite3_clear_bindings`) + C differential check!
    let sandbox = TempSandbox::new("g7_clear_bindings");
    let c_cb_db = sandbox.path().join("c_cb.db");
    let c_cb_out = Command::new(c_lifecycle_helper())
        .arg("diff_clear_bindings_in_row")
        .arg(&c_cb_db)
        .output()
        .unwrap();
    assert!(c_cb_out.status.success());
    let c_cb_str = String::from_utf8(c_cb_out.stdout).unwrap();

    let rs_cb_db = sandbox.path().join("rs_cb.db");
    let rs_cb = Connection::open(&rs_cb_db).unwrap();
    rs_cb
        .execute("CREATE TABLE t(x INT); INSERT INTO t VALUES (1), (2);")
        .unwrap();
    let mut cb_stmt = rs_cb.prepare("SELECT ?1, x FROM t ORDER BY x;").unwrap();
    cb_stmt.bind(1, 42i64).unwrap();

    let StepResult::Row(cr1) = cb_stmt.step().unwrap() else {
        panic!("step1");
    };
    let r1_p = cr1.get_i64(0).unwrap_or(-1);
    let r1_x = cr1.get_i64(1).unwrap();

    // `clear_bindings()` while paused at `Row` succeeds AND keeps `SHARED` lock alive!
    cb_stmt.clear_bindings().unwrap();
    assert_eq!(
        pragma_lock_status(&rs_cb)[0],
        ("main".to_owned(), "shared".to_owned())
    );

    let StepResult::Row(cr2) = cb_stmt.step().unwrap() else {
        panic!("step2");
    };
    let r2_p = cr2.get_i64(0).unwrap_or(-1);
    let r2_x = cr2.get_i64(1).unwrap();
    assert_eq!(cb_stmt.step().unwrap(), StepResult::Done);

    // `clear_bindings()` in `Done` also succeeds!
    cb_stmt.clear_bindings().unwrap();

    // Autoreset step sees cleared `NULL` (`-1`) parameter!
    let StepResult::Row(cr3) = cb_stmt.step().unwrap() else {
        panic!("step_autoreset");
    };
    let r3_p = cr3.get_i64(0).unwrap_or(-1);
    let r3_x = cr3.get_i64(1).unwrap();

    let rs_cb_str = format!("CB_RC=0 R1={r1_p}:{r1_x} R2={r2_p}:{r2_x} R3={r3_p}:{r3_x}\n");
    assert_eq!(
        rs_cb_str, c_cb_str,
        "clear_bindings in Row + autoreset must match C SQLite 3.54.1 100%"
    );
    cb_stmt.finalize().unwrap();
    rs_cb.try_close().unwrap();
    conn.try_close().unwrap();
}

// ============================================================================
// Group 8: `PRAGMA journal_mode`, `lock-7.3` Zero-Write Truncate Downgrade,
//          & `sqliters-cli` Dot-Command Smoke Test
// ============================================================================

#[test]
fn test_group8_pragma_journal_mode_transitions_lock_7_3_and_cli_smoke_test() {
    let sandbox = TempSandbox::new("g8_jmode_cli");
    let db_path = sandbox.path().join("jmode.db");
    let conn = Connection::open(&db_path).unwrap();
    conn.execute("CREATE TABLE items(id INTEGER PRIMARY KEY, code TEXT); CREATE INDEX idx_code ON items(code); INSERT INTO items VALUES (1, 'A1');")
        .unwrap();

    let get_mode = |c: &Connection, sql: &str| -> String {
        c.query_row(sql, &[])
            .unwrap()
            .unwrap()
            .get_str(0)
            .unwrap()
            .to_owned()
    };

    assert_eq!(get_mode(&conn, "PRAGMA journal_mode;"), "delete");
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = TRUNCATE;"),
        "truncate"
    );
    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = persist;"), "persist");
    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = MEMORY;"), "memory");
    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = OFF;"), "off");
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = TRUNCATE;"),
        "truncate"
    );
    // `WAL` is supported in Phase 1.2 (and transitions cleanly back to `truncate`), while unknown modes keep current mode!
    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = wal;"), "wal");
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = TRUNCATE;"),
        "truncate"
    );
    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = bogus;"), "truncate");

    // `:memory:` supports only `memory` and `off` (including no-op `WAL` inside an active `BEGIN`!):
    let mem = Connection::open_in_memory().unwrap();
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode;"), "memory");
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode = delete;"), "memory");
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode = wal;"), "memory");
    mem.execute("BEGIN;").unwrap();
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode = wal;"), "memory");
    mem.execute("COMMIT;").unwrap();
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode = off;"), "off");
    assert_eq!(get_mode(&mem, "PRAGMA journal_mode = memory;"), "memory");
    mem.try_close().unwrap();

    // Audit Item 5:
    // (a) `!is_in_autocommit` (`BEGIN DEFERRED` with `Pager` still in `Open`!) blocks changing
    //     INTO or OUT OF `wal`, while no-op `PRAGMA journal_mode = wal;` on an already-`wal` DB succeeds!
    conn.execute("BEGIN DEFERRED;").unwrap();
    let into_err = conn
        .query_row("PRAGMA journal_mode = wal;", &[])
        .unwrap_err();
    assert_eq!(into_err.code, ErrorCode::Error);
    assert_eq!(
        into_err.message,
        "cannot change into wal mode from within a transaction"
    );
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = truncate;"),
        "truncate"
    );
    conn.execute("COMMIT;").unwrap();

    assert_eq!(get_mode(&conn, "PRAGMA journal_mode = wal;"), "wal");
    conn.execute("BEGIN DEFERRED;").unwrap();
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = wal;"),
        "wal",
        "no-op PRAGMA journal_mode = wal inside BEGIN DEFERRED must succeed"
    );
    let out_err = conn
        .query_row("PRAGMA journal_mode = delete;", &[])
        .unwrap_err();
    assert_eq!(out_err.code, ErrorCode::Error);
    assert_eq!(
        out_err.message,
        "cannot change out of wal mode from within a transaction"
    );
    conn.execute("COMMIT;").unwrap();

    // (b) Fresh `Connection` opened on an existing `(2, 2)` WAL DB (after clean close unlinked `-wal`/`-shm`):
    //     - `PRAGMA journal_mode;` immediately discovers `"wal"`!
    //     - `PRAGMA journal_mode = bogus;` immediately discovers `"wal"`!
    //     - `PRAGMA journal_mode = truncate;` properly rewrites Page 1 `(2, 2) -> (1, 1)`!
    let wal_disc_path = sandbox.path().join("wal_lazy_disc.db");
    {
        let c_setup = Connection::open(&wal_disc_path).unwrap();
        c_setup
            .execute("CREATE TABLE w(x INTEGER PRIMARY KEY); INSERT INTO w VALUES (42);")
            .unwrap();
        assert_eq!(get_mode(&c_setup, "PRAGMA journal_mode = wal;"), "wal");
        c_setup.try_close().unwrap();
    }
    assert_eq!(&fs::read(&wal_disc_path).unwrap()[18..20], &[2, 2]);
    {
        let c_q = Connection::open(&wal_disc_path).unwrap();
        assert_eq!(get_mode(&c_q, "PRAGMA journal_mode;"), "wal");
        c_q.try_close().unwrap();
    }
    {
        let c_bogus = Connection::open(&wal_disc_path).unwrap();
        assert_eq!(get_mode(&c_bogus, "PRAGMA journal_mode = bogus;"), "wal");
        c_bogus.try_close().unwrap();
    }
    {
        let c_exit = Connection::open(&wal_disc_path).unwrap();
        assert_eq!(get_mode(&c_exit, "PRAGMA journal_mode = delete;"), "delete");
        c_exit.try_close().unwrap();
    }
    assert_eq!(
        &fs::read(&wal_disc_path).unwrap()[18..20],
        &[1, 1],
        "fresh Connection PRAGMA journal_mode = delete on existing (2, 2) DB must rewrite Page 1 to (1, 1)"
    );

    // Restore `conn` back to `truncate` for `lock-7.3` below:
    assert_eq!(
        get_mode(&conn, "PRAGMA journal_mode = TRUNCATE;"),
        "truncate"
    );

    // `lock-7.3`: `journal_mode = truncate` + paused `SELECT` at `Row` +
    // `BEGIN; UPDATE items SET code = 'Z' WHERE 0; COMMIT;` (zero dirty pages!) -> `main | shared`!
    let mut paused = conn.prepare("SELECT id, code FROM items;").unwrap();
    assert!(matches!(paused.step().unwrap(), StepResult::Row(_)));
    conn.execute("BEGIN; UPDATE items SET code = 'Z' WHERE 0; COMMIT;")
        .unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );
    paused.finalize().unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    conn.try_close().unwrap();

    // CLI smoke test: build & invoke `sqliters` binary testing `.tables`, `.schema`, `.indexes`, `.dbinfo`, `.dump`!
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let build_cli = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["build", "--quiet", "-p", "sqliters-cli"])
        .output()
        .expect("cargo build -p sqliters-cli");
    assert!(
        build_cli.status.success(),
        "failed building sqliters-cli:\n{}",
        String::from_utf8_lossy(&build_cli.stderr)
    );
    let cli_bin = workspace_root.join("target").join("debug").join("sqliters");

    let cli_out = Command::new(&cli_bin)
        .arg(&db_path)
        .args([".tables", ".schema", ".indexes", ".dbinfo", ".dump"])
        .output()
        .expect("run sqliters CLI dot commands");
    assert!(
        cli_out.status.success(),
        "sqliters CLI exited non-zero:\n{}",
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let stdout = String::from_utf8(cli_out.stdout).unwrap();
    assert!(
        stdout.contains("items"),
        "expected .tables output in:\n{stdout}"
    );
    assert!(
        stdout.contains("CREATE TABLE items"),
        "expected .schema output in:\n{stdout}"
    );
    assert!(
        stdout.contains("idx_code"),
        "expected .indexes output in:\n{stdout}"
    );
    assert!(
        stdout.contains("database page size:  4096") && stdout.contains("number of tables:    1"),
        "expected .dbinfo output in:\n{stdout}"
    );
    assert!(
        stdout.contains("INSERT INTO items VALUES(1,'A1');") && stdout.contains("COMMIT;"),
        "expected .dump output in:\n{stdout}"
    );

    // Wait briefly if needed to ensure zero background child lingers
    let _ = Duration::from_millis(1);
}

// ============================================================================
// Group 9: Audit Regressions (Unwind Gate Safety, BEGIN DEFERRED Unstarted-Write
//          Cleanup, Autocommit Commit-IoErr Clean Rollback + VACUUM Lock Safety,
//          & WITHOUT ROWID / NOT INDEXED Incremental VDBE Zero-Overflow Reads)
// ============================================================================

#[derive(Default)]
struct SpyFaultState {
    read_offsets: Mutex<Vec<(u64, usize)>>,
    lock_events: Mutex<Vec<(bool, LockLevel)>>,
    fail_next_main_db_write: AtomicBool,
    fail_all_main_db_writes: AtomicBool,
}

impl SpyFaultState {
    fn reset_reads(&self) {
        self.read_offsets.lock().unwrap().clear();
    }

    fn reset_locks(&self) {
        self.lock_events.lock().unwrap().clear();
    }

    fn distinct_pages_read(&self, page_size: u32) -> Vec<u32> {
        let mut pages: Vec<u32> = self
            .read_offsets
            .lock()
            .unwrap()
            .iter()
            .map(|&(off, _)| (off / u64::from(page_size)) as u32 + 1)
            .collect();
        pages.sort_unstable();
        pages.dedup();
        pages
    }
}

struct SpyFaultVfs {
    inner: Arc<MemoryVfs>,
    state: Arc<SpyFaultState>,
}

impl SpyFaultVfs {
    fn new() -> Self {
        Self {
            inner: Arc::new(MemoryVfs::new()),
            state: Arc::new(SpyFaultState::default()),
        }
    }
}

struct SpyFaultMainFile {
    inner: Box<dyn VfsFile>,
    state: Arc<SpyFaultState>,
}

impl VfsFile for SpyFaultMainFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> SqlResult<usize> {
        self.state
            .read_offsets
            .lock()
            .unwrap()
            .push((offset, buf.len()));
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> SqlResult<()> {
        if self.state.fail_all_main_db_writes.load(Ordering::SeqCst)
            || self
                .state
                .fail_next_main_db_write
                .swap(false, Ordering::SeqCst)
        {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_WRITE,
                "injected MAIN_DB write_at failure",
            ));
        }
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> SqlResult<()> {
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> SqlResult<()> {
        self.inner.sync(flags)
    }

    fn file_size(&self) -> SqlResult<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> SqlResult<()> {
        self.state.lock_events.lock().unwrap().push((true, level));
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> SqlResult<()> {
        self.state.lock_events.lock().unwrap().push((false, level));
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&self) -> SqlResult<bool> {
        self.inner.check_reserved_lock()
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn lock_state_for_diagnostics(&self) -> SqlResult<Option<LockLevel>> {
        Ok(Some(self.inner.lock_level()))
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }

    fn close(&mut self) -> SqlResult<()> {
        self.inner.close()
    }
}

impl Vfs for SpyFaultVfs {
    fn name(&self) -> &str {
        "spy-fault-p1d"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> SqlResult<Box<dyn VfsFile>> {
        let f = self.inner.open(path, flags)?;
        let is_main_db = flags.contains(OpenFlags::MAIN_DB)
            || (!flags.contains(OpenFlags::MAIN_JOURNAL)
                && !flags.contains(OpenFlags::TEMP_JOURNAL)
                && !flags.contains(OpenFlags::TEMP_DB)
                && !flags.contains(OpenFlags::WAL)
                && path.is_some_and(|p| {
                    let s = p.to_string_lossy();
                    !s.ends_with("-journal") && !s.ends_with("-wal") && !s.ends_with("-shm")
                }));
        if is_main_db {
            Ok(Box::new(SpyFaultMainFile {
                inner: f,
                state: Arc::clone(&self.state),
            }))
        } else {
            Ok(f)
        }
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> SqlResult<()> {
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> SqlResult<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> SqlResult<PathBuf> {
        self.inner.full_pathname(path)
    }
}

#[test]
fn test_audit1_gate_busy_released_across_unwind_in_enter_and_drain_and_release_gate() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE u(x INT); INSERT INTO u VALUES (10);")
        .unwrap();

    // 1. Unwind inside `ConnSession::drop -> drain_and_release_gate -> process_pending_drops`:
    let doomed_stmt = conn.prepare("SELECT x FROM u;").unwrap();
    conn.arm_panic_on_next_pending_drop_for_testing().unwrap();
    let unwind_drain = catch_unwind(AssertUnwindSafe(|| {
        let _ = conn.execute_and_invoke_hook_for_testing("", move |_c| {
            // Dropping `doomed_stmt` while the gate is held enqueues onto `pending_drops`;
            // when `execute_and_invoke_hook_for_testing` exits, `drain_and_release_gate` runs and panics!
            drop(doomed_stmt);
        });
    }));
    assert!(
        unwind_drain.is_err(),
        "expected simulated panic in drain_and_release_gate"
    );

    // Gate MUST be cleanly released (neither `Busy` nor `Misuse`) and connection remains usable!
    conn.execute("INSERT INTO u VALUES (20);").unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM u;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(2)
    );

    // 2. Unwind inside `ConnectionShared::enter -> process_pending_drops(initial_drops)`:
    let doomed_enter_stmt = conn.prepare("SELECT x FROM u;").unwrap();
    conn.enqueue_pending_drop_for_enter_unwind_testing(doomed_enter_stmt)
        .unwrap();
    let unwind_enter = catch_unwind(AssertUnwindSafe(|| {
        let _ = conn.execute("INSERT INTO u VALUES (999);");
    }));
    assert!(unwind_enter.is_err(), "expected simulated panic in enter()");

    // Again, `gate_busy` and `ACTIVE_CONN_STACK` MUST be cleanly unwound!
    conn.execute("INSERT INTO u VALUES (30);").unwrap();
    assert_eq!(
        conn.query_row("SELECT SUM(x) FROM u;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(60)
    );
    conn.try_close().unwrap();
}

#[test]
fn test_audit2_begin_deferred_cleanup_unstarted_write_preserves_open_and_read_states() {
    let sandbox = TempSandbox::new("audit2_begin_def");
    let db_path = sandbox.path().join("def.db");

    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "CREATE TABLE d_tbl(a INT);
         CREATE TABLE victim_tbl(b INT);
         INSERT INTO d_tbl VALUES (1);",
    )
    .unwrap();

    // Case A: `BEGIN DEFERRED;` (`Open` / `unlocked`) -> pre-execution V1 `Schema` & V2 `reprepare` failure
    // MUST roll back the unstarted `Write` back to `Open` (`unlocked`) while keeping `!is_autocommit()`!
    let mut v1_ins = conn.prepare_v1("INSERT INTO d_tbl VALUES (10);").unwrap();
    let mut v2_ins = conn.prepare("INSERT INTO victim_tbl VALUES (20);").unwrap();

    conn.execute("BEGIN DEFERRED;").unwrap();
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    {
        let ext = Connection::open(&db_path).unwrap();
        ext.execute(
            "ALTER TABLE d_tbl ADD COLUMN extra INT DEFAULT 0;
             DROP TABLE victim_tbl;",
        )
        .unwrap();
        ext.try_close().unwrap();
    }

    assert_eq!(v1_ins.step().unwrap_err().code, ErrorCode::Schema);
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    assert_eq!(v2_ins.step().unwrap_err().code, ErrorCode::Error);
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    // Confirm zero leaked `RESERVED` lock on disk: an external writer can still `BEGIN IMMEDIATE`!
    {
        let ext_check = Connection::open(&db_path).unwrap();
        ext_check.execute("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
        ext_check.try_close().unwrap();
    }
    conn.execute("ROLLBACK;").unwrap();
    assert!(conn.is_autocommit());
    let _ = v1_ins.finalize();
    let _ = v2_ins.finalize();

    // Case B: `BEGIN DEFERRED; SELECT ...;` (`Read` / `shared`) -> failed write reprepare
    // MUST downgrade the unstarted `Write` back to `Read` (`shared`), NOT `Write` (`reserved`) or `Open`!
    conn.execute("CREATE TABLE victim_tbl2(b INT);").unwrap();
    let mut v1_ins_read = conn
        .prepare_v1("INSERT INTO d_tbl(a, extra) VALUES (11, 2);")
        .unwrap();
    let mut v2_ins_read = conn
        .prepare("INSERT INTO victim_tbl2 VALUES (22);")
        .unwrap();

    {
        let ext2 = Connection::open(&db_path).unwrap();
        ext2.execute(
            "ALTER TABLE d_tbl ADD COLUMN extra2 INT DEFAULT 7;
             DROP TABLE victim_tbl2;",
        )
        .unwrap();
        ext2.try_close().unwrap();
    }

    conn.execute("BEGIN DEFERRED;").unwrap();
    let _ = conn.query("SELECT COUNT(*) FROM d_tbl;", &[]).unwrap();
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );

    assert_eq!(v1_ins_read.step().unwrap_err().code, ErrorCode::Schema);
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );

    assert_eq!(v2_ins_read.step().unwrap_err().code, ErrorCode::Error);
    assert!(!conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "shared".to_owned())
    );

    conn.execute("ROLLBACK;").unwrap();
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    let _ = v1_ins_read.finalize();
    let _ = v2_ins_read.finalize();
    conn.try_close().unwrap();
}

#[test]
fn test_audit3_autocommit_commit_ioerr_clean_rollback_no_false_poison_and_vacuum_schema_reload() {
    let vfs = Arc::new(SpyFaultVfs::new());
    let db_path = Path::new("/audit3_fault.db");
    let conn = Connection::open_with_vfs(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();

    conn.execute("CREATE TABLE ft(id INTEGER PRIMARY KEY, val TEXT);")
        .unwrap();
    conn.execute("INSERT INTO ft VALUES (1, 'seed');").unwrap();
    let base_total = conn.total_changes();
    let base_rowid = conn.last_insert_rowid();

    // 1. Autocommit `INSERT` commit Phase-1 `MAIN_DB` `write_at` `IoErr`:
    //    `rollback_with_trip` succeeds -> returns `IoErr` WITHOUT poisoning `conn`!
    vfs.state
        .fail_next_main_db_write
        .store(true, Ordering::SeqCst);
    let err = conn
        .execute("INSERT INTO ft VALUES (2, 'transient_fail');")
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::IoErr);
    assert!(conn.is_autocommit());
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );
    assert_eq!(conn.total_changes(), base_total);
    assert_eq!(conn.last_insert_rowid(), base_rowid);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ft;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(1)
    );

    // Connection is completely healthy & unpoisoned: subsequent DML succeeds!
    conn.execute("INSERT INTO ft VALUES (3, 'ok_after_ioerr');")
        .unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ft;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(2)
    );

    // 2. Autocommit `INSERT OR FAIL` commit `MAIN_DB` `write_at` `IoErr`:
    //    Row `4` wrote before row `1` hit `UNIQUE`; commit fails with `IoErr`, rolls back row `4`,
    //    and leaves `conn` healthy & unpoisoned!
    let pre_fail_total = conn.total_changes();
    vfs.state
        .fail_next_main_db_write
        .store(true, Ordering::SeqCst);
    let fail_err = conn
        .execute("INSERT OR FAIL INTO ft VALUES (4, 'partial'), (1, 'dup');")
        .unwrap_err();
    assert_eq!(fail_err.code, ErrorCode::IoErr);
    assert_eq!(conn.total_changes(), pre_fail_total);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM ft;", &[])
            .unwrap()
            .unwrap()
            .get_i64(0),
        Some(2)
    );

    // 3. `VACUUM` commit `IoErr` rolls back cleanly without poisoning, AND healthy `VACUUM`
    //    reloads `Schema` while holding `Write` (zero post-commit `SHARED` lock roundtrip!):
    conn.execute("CREATE TABLE trash(x TEXT); DROP TABLE trash;")
        .unwrap();
    vfs.state
        .fail_next_main_db_write
        .store(true, Ordering::SeqCst);
    let vac_err = conn.execute("VACUUM;").unwrap_err();
    assert_eq!(vac_err.code, ErrorCode::IoErr);
    assert_eq!(
        pragma_lock_status(&conn)[0],
        ("main".to_owned(), "unlocked".to_owned())
    );

    vfs.state.reset_locks();
    conn.execute("VACUUM;").unwrap();
    let lock_events = vfs.state.lock_events.lock().unwrap().clone();
    let last_none_pos = lock_events
        .iter()
        .rposition(|&(is_lock, lvl)| !is_lock && lvl == LockLevel::NoLock)
        .expect("VACUUM commit unlocks MAIN_DB to NoLock");
    // No `lock(Shared)` may occur AFTER the final `unlock(None)` of `VACUUM` commit!
    assert_eq!(
        last_none_pos,
        lock_events.len() - 1,
        "VACUUM must reload schema before unlocking MAIN_DB, got lock events: {lock_events:?}"
    );
    assert_eq!(
        conn.query_row("SELECT val FROM ft WHERE id = 3;", &[])
            .unwrap()
            .unwrap()
            .get_str(0),
        Some("ok_after_ioerr")
    );

    // 4. Conversely, when BOTH `commit_retaining_read` AND `rollback_with_trip` fail with `IoErr`,
    //    safe state cannot be restored -> `Connection` transitions to `ConnectionState::Poisoned(IoErr)`!
    vfs.state
        .fail_all_main_db_writes
        .store(true, Ordering::SeqCst);
    let poison_err = conn
        .execute("INSERT INTO ft VALUES (99, 'double_fault');")
        .unwrap_err();
    assert_eq!(poison_err.code, ErrorCode::IoErr);
    vfs.state
        .fail_all_main_db_writes
        .store(false, Ordering::SeqCst);
    assert_eq!(
        conn.query("SELECT 1;", &[]).unwrap_err().code,
        ErrorCode::IoErr
    );
    conn.try_close().unwrap();
}

#[test]
fn test_audit4_without_rowid_and_not_indexed_incremental_vdbe_zero_overflow_reads() {
    let vfs = Arc::new(SpyFaultVfs::new());
    let db_path = Path::new("/audit4_wr.db");
    let opts = ConnectionOpenOptions {
        default_page_size: 512,
        ..ConnectionOpenOptions::default()
    };
    let conn = Connection::open_with_vfs_and_options(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        opts,
    )
    .unwrap();

    conn.execute(
        "CREATE TABLE wr_big(
             k TEXT NOT NULL,
             short_col TEXT NOT NULL,
             big_blob BLOB NOT NULL,
             sec INT NOT NULL,
             PRIMARY KEY(k)
         ) WITHOUT ROWID;
         CREATE INDEX idx_wr_sec ON wr_big(sec);",
    )
    .unwrap();

    conn.execute("BEGIN;").unwrap();
    let mut ins = conn
        .prepare("INSERT INTO wr_big VALUES (?1, ?2, ?3, ?4);")
        .unwrap();
    // Each 2048-byte BLOB spans 4+ overflow pages on 512-byte pages (>1,000 pages total)!
    let big_payload = vec![0xABu8; 2048];
    for i in 1..=250i64 {
        ins.bind(1, format!("k-{i:04}")).unwrap();
        ins.bind(2, format!("short-{i:04}")).unwrap();
        ins.bind(3, big_payload.clone()).unwrap();
        ins.bind(4, i).unwrap();
        assert_eq!(ins.step().unwrap(), StepResult::Done);
        ins.reset().unwrap();
    }
    ins.finalize().unwrap();
    conn.execute("COMMIT;").unwrap();
    let total_pages = conn.page_count().unwrap();
    assert!(
        total_pages > 1000,
        "expected >1000 pages on 512B DB, got {total_pages}"
    );
    conn.try_close().unwrap();

    // Reopen fresh connection so Pager cache starts completely cold!
    let reader = Connection::open_with_vfs_and_options(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        opts,
    )
    .unwrap();
    // Warm schema catalog once and verify `EXPLAIN` compiles to VDBE `OpenReadIdx`:
    let explain_rows = reader
        .query("EXPLAIN SELECT k, short_col FROM wr_big LIMIT 1;", &[])
        .unwrap();
    assert!(
        explain_rows
            .rows
            .iter()
            .any(|r| r.get_str(1) == Some("OpenReadIdx")),
        "WITHOUT ROWID SELECT must compile to VDBE OpenReadIdx, got: {:?}",
        explain_rows.rows
    );

    // 1. `SELECT k, short_col FROM wr_big LIMIT 1;` -> incremental VDBE `OpenReadIdx` + `Rewind`,
    //    reading ONLY root-to-first-leaf path and ZERO overflow pages!
    vfs.state.reset_reads();
    let row_lim1 = reader
        .query_row("SELECT k, short_col FROM wr_big LIMIT 1;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row_lim1.get_str(0), Some("k-0001"));
    assert_eq!(row_lim1.get_str(1), Some("short-0001"));
    let pages_lim1 = vfs.state.distinct_pages_read(512);
    assert!(
        (2..=6).contains(&pages_lim1.len()),
        "expected only root-to-first-leaf path (2..=6 pages out of {total_pages}), read {}: {pages_lim1:?}",
        pages_lim1.len()
    );

    // 2. Primary PK `IndexSeek` (`WHERE k = 'k-0175'`) on fresh cold reader:
    reader.try_close().unwrap();
    let reader2 = Connection::open_with_vfs_and_options(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        opts,
    )
    .unwrap();
    let _ = reader2.schema_snapshot().unwrap();
    vfs.state.reset_reads();
    let row_pk = reader2
        .query_row("SELECT k, short_col FROM wr_big WHERE k = 'k-0175';", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row_pk.get_str(0), Some("k-0175"));
    assert_eq!(row_pk.get_str(1), Some("short-0175"));
    let pages_pk = vfs.state.distinct_pages_read(512);
    assert!(
        (2..=6).contains(&pages_pk.len()),
        "expected O(log N) PK seek (2..=6 pages) without overflow reads, read {}: {pages_pk:?}",
        pages_pk.len()
    );

    // 3. Secondary `IndexSeek` (`WHERE sec = 175`) -> extracts `WITHOUT ROWID` PK & seeks `wr_big`:
    reader2.try_close().unwrap();
    let reader3 = Connection::open_with_vfs_and_options(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        opts,
    )
    .unwrap();
    let _ = reader3.schema_snapshot().unwrap();
    vfs.state.reset_reads();
    let row_sec = reader3
        .query_row("SELECT k, short_col FROM wr_big WHERE sec = 175;", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row_sec.get_str(0), Some("k-0175"));
    assert_eq!(row_sec.get_str(1), Some("short-0175"));
    let pages_sec = vfs.state.distinct_pages_read(512);
    assert!(
        (4..=12).contains(&pages_sec.len()),
        "expected O(log N) secondary index + PK seek (4..=12 pages) without overflow reads, read {}: {pages_sec:?}",
        pages_sec.len()
    );

    // 4. `NOT INDEXED` (`SELECT k, short_col FROM wr_big NOT INDEXED WHERE k = 'k-0002' LIMIT 1;`):
    reader3.try_close().unwrap();
    let reader4 = Connection::open_with_vfs_and_options(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        opts,
    )
    .unwrap();
    let _ = reader4.schema_snapshot().unwrap();
    vfs.state.reset_reads();
    let row_ni = reader4
        .query_row(
            "SELECT k, short_col FROM wr_big NOT INDEXED WHERE k = 'k-0002' LIMIT 1;",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(row_ni.get_str(0), Some("k-0002"));
    assert_eq!(row_ni.get_str(1), Some("short-0002"));
    let pages_ni = vfs.state.distinct_pages_read(512);
    assert!(
        (2..=8).contains(&pages_ni.len()),
        "expected streaming NOT INDEXED scan (2..=8 pages) stopping at k-0002 without overflow reads, read {}: {pages_ni:?}",
        pages_ni.len()
    );

    // 5. `ALTER TABLE wr_big ADD COLUMN tag TEXT DEFAULT 'v1'` preserves default fallback on VDBE `Index` cursor:
    reader4
        .execute("ALTER TABLE wr_big ADD COLUMN tag TEXT DEFAULT 'v1';")
        .unwrap();
    let row_alt = reader4
        .query_row("SELECT k, tag FROM wr_big WHERE k = 'k-0175';", &[])
        .unwrap()
        .unwrap();
    assert_eq!(row_alt.get_str(0), Some("k-0175"));
    assert_eq!(row_alt.get_str(1), Some("v1"));
    reader4.try_close().unwrap();
}
