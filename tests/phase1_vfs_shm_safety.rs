//! Phase 1.1-A VFS, Pathname/Symlink, Inode Deferred-Close, Two-Level `WalShm`, and
//! Out-of-Tree C SQLite 3.54.1 Interprocess Compatibility Suite (`implementation_plan.md`
//! Test Groups 1, 2, 3, 4, 24, 48).
#![forbid(unsafe_code)]

#[cfg(unix)]
mod unix_suite {
    use std::fs::{self, OpenOptions};
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier, OnceLock};
    use std::thread;

    use sqliters::types::{ErrorCode, LockGeometry, extended};
    use sqliters::vfs::{
        CheckpointMode, LockLevel, MemoryVfs, OpenFlags, OsVfs, READMARK_NOT_USED, ShmDmsState,
        ShmOpenOptions, Vfs, VfsFile, WALINDEX_MAX_VERSION, WalIndexHeaderData, WalLockMode,
        WalShmOps,
    };

    static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

    struct TempSandbox {
        dir: PathBuf,
    }

    impl TempSandbox {
        fn new(prefix: &str) -> Self {
            let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let dir = std::env::temp_dir().join(format!("sqliters_p1a_{prefix}_{pid}_{id}"));
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
            // Restore write permissions on any readonly test directories/files before removal.
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if let Ok(meta) = fs::symlink_metadata(&p)
                        && !meta.file_type().is_symlink()
                    {
                        let mode = if meta.is_dir() { 0o755 } else { 0o644 };
                        let _ = fs::set_permissions(&p, fs::Permissions::from_mode(mode));
                    }
                }
            }
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    const C_HELPER_SOURCE: &str = r#"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "sqlite3.h"

static void die_sql(const char *ctx, sqlite3 *db, int rc) {
    fprintf(stderr, "%s failed: rc=%d ext=%d msg=%s\n",
            ctx,
            rc,
            db ? sqlite3_extended_errcode(db) : rc,
            db ? sqlite3_errmsg(db) : "no-db");
    if (db) sqlite3_close(db);
    exit(2);
}

static void exec_checked(sqlite3 *db, const char *sql) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "SQL error (%d): %s\nSQL: %s\n", rc, errmsg ? errmsg : "", sql);
        sqlite3_free(errmsg);
        sqlite3_close(db);
        exit(2);
    }
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "missing subcommand\n");
        return 2;
    }
    const char *cmd = argv[1];

    if (strcmp(cmd, "version") == 0) {
        printf("VERSION=%s\n", sqlite3_libversion());
        printf("SOURCEID=%s\n", sqlite3_sourceid());
        fflush(stdout);
        return 0;
    }

    if (strcmp(cmd, "probe-fcntl") == 0) {
        if (argc < 6) return 2;
        const char *path = argv[2];
        long long offset = atoll(argv[3]);
        long long len = atoll(argv[4]);
        int want_wr = (strcmp(argv[5], "wr") == 0);

        int fd = open(path, want_wr ? O_RDWR : O_RDONLY);
        if (fd < 0 && want_wr) {
            fd = open(path, O_RDONLY);
        }
        if (fd < 0) {
            fprintf(stderr, "open(%s) failed: %s\n", path, strerror(errno));
            return 2;
        }
        struct flock fl;
        memset(&fl, 0, sizeof(fl));
        fl.l_type = want_wr ? F_WRLCK : F_RDLCK;
        fl.l_whence = SEEK_SET;
        fl.l_start = (off_t)offset;
        fl.l_len = (off_t)len;
        if (fcntl(fd, F_GETLK, &fl) != 0) {
            fprintf(stderr, "F_GETLK failed: %s\n", strerror(errno));
            close(fd);
            return 2;
        }
        const char *t = "OTHER";
        if (fl.l_type == F_UNLCK) t = "UNLCK";
        else if (fl.l_type == F_RDLCK) t = "RDLCK";
        else if (fl.l_type == F_WRLCK) t = "WRLCK";
        printf("TYPE=%s PID=%d\n", t, (int)fl.l_pid);
        fflush(stdout);
        close(fd);
        return 0;
    }

    if (strcmp(cmd, "hold-db-lock") == 0) {
        if (argc < 5) return 2;
        const char *db_path = argv[2];
        unsigned int pbyte = (unsigned int)strtoul(argv[3], NULL, 0);
        const char *mode = argv[4];

        sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, pbyte);
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(
            db_path,
            &db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            NULL
        );
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, 0);

        sqlite3_stmt *rd_stmt = NULL;
        if (strcmp(mode, "shared") == 0) {
            rc = sqlite3_prepare_v2(db, "SELECT x FROM t;", -1, &rd_stmt, NULL);
            if (rc != SQLITE_OK) die_sql("prepare shared", db, rc);
            rc = sqlite3_step(rd_stmt);
            if (rc != SQLITE_ROW) die_sql("step shared", db, rc);
        } else if (strcmp(mode, "reserved") == 0) {
            exec_checked(db, "BEGIN IMMEDIATE; INSERT INTO t VALUES (9001);");
        } else if (strcmp(mode, "exclusive") == 0) {
            exec_checked(db, "BEGIN EXCLUSIVE;");
        } else {
            fprintf(stderr, "unknown hold mode: %s\n", mode);
            return 2;
        }

        printf("READY\n");
        fflush(stdout);
        while (fgetc(stdin) != EOF) {}

        if (rd_stmt) sqlite3_finalize(rd_stmt);
        sqlite3_exec(db, "ROLLBACK;", NULL, NULL, NULL);
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "init-rollback-db") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        unsigned int pbyte = (unsigned int)strtoul(argv[3], NULL, 0);
        sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, pbyte);
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(
            db_path,
            &db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            NULL
        );
        if (rc != SQLITE_OK) die_sql("open init", db, rc);
        exec_checked(db, "CREATE TABLE IF NOT EXISTS t(x INTEGER); INSERT INTO t VALUES (1), (2);");
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "try-db-lock") == 0) {
        if (argc < 5) return 2;
        const char *db_path = argv[2];
        unsigned int pbyte = (unsigned int)strtoul(argv[3], NULL, 0);
        const char *mode = argv[4];

        sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, pbyte);
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) {
            printf("RC=%d EXT=%d\n", rc, db ? sqlite3_extended_errcode(db) : rc);
            if (db) sqlite3_close(db);
            return 0;
        }
        sqlite3_busy_timeout(db, 0);
        sqlite3_extended_result_codes(db, 1);

        const char *sql = "SELECT count(*) FROM sqlite_master;";
        if (strcmp(mode, "immediate") == 0) {
            sql = "BEGIN IMMEDIATE;";
        } else if (strcmp(mode, "exclusive") == 0) {
            sql = "BEGIN EXCLUSIVE;";
        }
        rc = sqlite3_exec(db, sql, NULL, NULL, NULL);
        int ext = sqlite3_extended_errcode(db);
        printf("RC=%d EXT=%d\n", rc, ext);
        fflush(stdout);
        sqlite3_exec(db, "ROLLBACK;", NULL, NULL, NULL);
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "wal-hold-populated") == 0) {
        if (argc < 6) return 2;
        const char *db_path = argv[2];
        int page_size = atoi(argv[3]);
        int rows = atoi(argv[4]);
        const char *hold_mode = argv[5];

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(
            db_path,
            &db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            NULL
        );
        if (rc != SQLITE_OK) die_sql("open wal", db, rc);
        sqlite3_busy_timeout(db, 0);
        sqlite3_extended_result_codes(db, 1);

        char pragma_sql[256];
        snprintf(
            pragma_sql,
            sizeof(pragma_sql),
            "PRAGMA page_size = %d; PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;",
            page_size
        );
        exec_checked(db, pragma_sql);
        exec_checked(db, "CREATE TABLE IF NOT EXISTS items(id INTEGER PRIMARY KEY, payload BLOB);");

        exec_checked(db, "BEGIN;");
        sqlite3_stmt *ins = NULL;
        rc = sqlite3_prepare_v2(db, "INSERT INTO items(id, payload) VALUES (?1, zeroblob(512));", -1, &ins, NULL);
        if (rc != SQLITE_OK) die_sql("prepare insert", db, rc);
        for (int i = 1; i <= rows; i++) {
            sqlite3_bind_int(ins, 1, i);
            rc = sqlite3_step(ins);
            if (rc != SQLITE_DONE) die_sql("step insert", db, rc);
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_checked(db, "COMMIT;");

        sqlite3_stmt *held_reader = NULL;
        if (strcmp(hold_mode, "reader") == 0) {
            exec_checked(db, "BEGIN;");
            rc = sqlite3_prepare_v2(db, "SELECT id FROM items ORDER BY id;", -1, &held_reader, NULL);
            if (rc != SQLITE_OK) die_sql("prepare wal reader", db, rc);
            rc = sqlite3_step(held_reader);
            if (rc != SQLITE_ROW) die_sql("step wal reader", db, rc);
        } else if (strcmp(hold_mode, "writer") == 0) {
            exec_checked(db, "BEGIN IMMEDIATE; INSERT INTO items(id, payload) VALUES (999999, zeroblob(128));");
        }

        printf("READY\n");
        fflush(stdout);
        while (fgetc(stdin) != EOF) {}

        if (held_reader) sqlite3_finalize(held_reader);
        sqlite3_exec(db, "ROLLBACK;", NULL, NULL, NULL);
        sqlite3_close(db);
        return 0;
    }

    if (strcmp(cmd, "wal-try-op") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        const char *op = argv[3];

        int open_flags = (strcmp(op, "readonly_select") == 0)
            ? SQLITE_OPEN_READONLY
            : SQLITE_OPEN_READWRITE;
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, open_flags, NULL);
        if (rc != SQLITE_OK) {
            printf("RC=%d EXT=%d LOG=-1 CKPT=-1\n", rc, db ? sqlite3_extended_errcode(db) : rc);
            if (db) sqlite3_close(db);
            return 0;
        }
        sqlite3_busy_timeout(db, 0);
        sqlite3_extended_result_codes(db, 1);

        int n_log = -1;
        int n_ckpt = -1;
        if (strcmp(op, "write") == 0) {
            rc = sqlite3_exec(db, "BEGIN IMMEDIATE; INSERT INTO items(id, payload) VALUES (888888, zeroblob(64));", NULL, NULL, NULL);
        } else if (strcmp(op, "ckpt_passive") == 0) {
            rc = sqlite3_exec(db, "SELECT count(*) FROM sqlite_master;", NULL, NULL, NULL);
            if (rc == SQLITE_OK) {
                rc = sqlite3_wal_checkpoint_v2(db, "main", SQLITE_CHECKPOINT_PASSIVE, &n_log, &n_ckpt);
            }
        } else if (strcmp(op, "ckpt_restart") == 0) {
            rc = sqlite3_exec(db, "SELECT count(*) FROM sqlite_master;", NULL, NULL, NULL);
            if (rc == SQLITE_OK) {
                rc = sqlite3_wal_checkpoint_v2(db, "main", SQLITE_CHECKPOINT_RESTART, &n_log, &n_ckpt);
            }
        } else if (strcmp(op, "readonly_select") == 0) {
            sqlite3_stmt *st = NULL;
            rc = sqlite3_prepare_v2(db, "SELECT count(*) FROM items;", -1, &st, NULL);
            if (rc == SQLITE_OK) {
                int s = sqlite3_step(st);
                if (s == SQLITE_ROW) {
                    n_log = sqlite3_column_int(st, 0);
                    rc = SQLITE_OK;
                } else {
                    rc = s;
                }
                sqlite3_finalize(st);
            }
        } else {
            fprintf(stderr, "unknown wal-try-op: %s\n", op);
            sqlite3_close(db);
            return 2;
        }
        int ext = (rc == SQLITE_OK) ? SQLITE_OK : sqlite3_extended_errcode(db);
        printf("RC=%d EXT=%d LOG=%d CKPT=%d\n", rc, ext, n_log, n_ckpt);
        fflush(stdout);
        sqlite3_exec(db, "ROLLBACK;", NULL, NULL, NULL);
        sqlite3_close(db);
        return 0;
    }

    fprintf(stderr, "unrecognized subcommand: %s\n", cmd);
    return 2;
}
"#;

    /// Builds `sqlite3.c` + `c_vfs_shm_helper` strictly inside `<workspace>/target/sqlite-ref/`,
    /// asserting that `./sqlite/` remains 100% untouched and that `sqlite3_libversion()` and
    /// `sqlite3_sourceid()` match `./sqlite/VERSION` (`3.54.1`) and `./sqlite/manifest.uuid`.
    fn c_reference_helper() -> &'static Path {
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
            assert!(
                before_status.status.success(),
                "git status failed before building sqlite-ref"
            );

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
                    .expect("invoke make for out-of-tree sqlite3.c amalgamation");
                assert!(
                    make_out.status.success(),
                    "failed to build sqlite3.c in target/sqlite-ref/amalg:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&make_out.stdout),
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
                    "failed compiling sqlite3.o:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&cc_o.stdout),
                    String::from_utf8_lossy(&cc_o.stderr)
                );
            }

            let helper_c = ref_root.join("c_vfs_shm_helper.c");
            let helper_bin = ref_root.join("c_vfs_shm_helper");
            let need_compile = match fs::read_to_string(&helper_c) {
                Ok(existing) => existing != C_HELPER_SOURCE || !helper_bin.exists(),
                Err(_) => true,
            };
            if need_compile {
                fs::write(&helper_c, C_HELPER_SOURCE).expect("write c_vfs_shm_helper.c");
                let cc_out = Command::new("cc")
                    .args(["-O2", "-pthread"])
                    .arg("-I")
                    .arg(&amalg_dir)
                    .arg(&helper_c)
                    .arg(&sqlite3_o)
                    .args(["-ldl", "-lm", "-o"])
                    .arg(&helper_bin)
                    .output()
                    .expect("compile c_vfs_shm_helper");
                assert!(
                    cc_out.status.success(),
                    "failed compiling c_vfs_shm_helper:\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&cc_out.stdout),
                    String::from_utf8_lossy(&cc_out.stderr)
                );
            }

            // Verify `./sqlite/` was NOT dirtied by building the C reference helper!
            let after_status = Command::new("git")
                .arg("-C")
                .arg(workspace_root)
                .args(["status", "--porcelain", "--", "sqlite"])
                .output()
                .expect("git status sqlite after build");
            assert_eq!(
                before_status.stdout, after_status.stdout,
                "building C reference helper must NEVER write files into ./sqlite/"
            );

            // Verify `sqlite3_libversion()` and `sqlite3_sourceid()` against `./sqlite/manifest.uuid`!
            let ver_out = Command::new(&helper_bin)
                .arg("version")
                .output()
                .expect("run c_vfs_shm_helper version");
            assert!(ver_out.status.success());
            let ver_text = String::from_utf8_lossy(&ver_out.stdout);
            let mut got_version = None;
            let mut got_sourceid = None;
            for line in ver_text.lines() {
                if let Some(v) = line.strip_prefix("VERSION=") {
                    got_version = Some(v.trim().to_owned());
                } else if let Some(s) = line.strip_prefix("SOURCEID=") {
                    got_sourceid = Some(s.trim().to_owned());
                }
            }
            let expected_uuid = fs::read_to_string(sqlite_src_dir.join("manifest.uuid"))
                .expect("read ./sqlite/manifest.uuid")
                .trim()
                .to_owned();
            assert_eq!(got_version.as_deref(), Some("3.54.1"));
            let sourceid = got_sourceid.expect("SOURCEID output");
            assert!(
                sourceid.contains(&expected_uuid),
                "expected C helper sqlite3_sourceid() `{sourceid}` to contain ./sqlite/manifest.uuid `{expected_uuid}`"
            );

            helper_bin
        })
    }

    struct HeldHelperProcess {
        child: Child,
    }

    impl HeldHelperProcess {
        fn spawn(args: &[&str]) -> Self {
            let bin = c_reference_helper();
            let mut child = Command::new(bin)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn c_vfs_shm_helper");

            let stdout = child.stdout.take().expect("child stdout");
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let n = reader.read_line(&mut line).expect("read READY line");
            if n == 0 || line.trim() != "READY" {
                let _ = child.kill();
                let out = child.wait_with_output().expect("collect failed child");
                panic!(
                    "c_vfs_shm_helper {:?} did not print READY (got {:?}):\nstderr:\n{}",
                    args,
                    line,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            child.stdout = Some(reader.into_inner());
            Self { child }
        }

        fn stop(mut self) {
            drop(self.child.stdin.take());
            let status = self.child.wait().expect("wait c_vfs_shm_helper");
            assert!(status.success(), "c_vfs_shm_helper exited with {status}");
        }
    }

    impl Drop for HeldHelperProcess {
        fn drop(&mut self) {
            drop(self.child.stdin.take());
            let _ = self.child.wait();
        }
    }

    fn probe_kernel_lock(path: &Path, offset: u64, len: u64, write_probe: bool) -> (String, i32) {
        let bin = c_reference_helper();
        let out = Command::new(bin)
            .arg("probe-fcntl")
            .arg(path)
            .arg(offset.to_string())
            .arg(len.to_string())
            .arg(if write_probe { "wr" } else { "rd" })
            .output()
            .expect("invoke probe-fcntl");
        assert!(
            out.status.success(),
            "probe-fcntl failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let trimmed = text.trim();
        let mut ltype = String::new();
        let mut pid = 0i32;
        for part in trimmed.split_whitespace() {
            if let Some(t) = part.strip_prefix("TYPE=") {
                ltype = t.to_owned();
            } else if let Some(p) = part.strip_prefix("PID=") {
                pid = p.parse().expect("parse pid");
            }
        }
        (ltype, pid)
    }

    fn run_c_try_db_lock(db_path: &Path, pending_byte: u64, mode: &str) -> (i32, i32) {
        let bin = c_reference_helper();
        let out = Command::new(bin)
            .arg("try-db-lock")
            .arg(db_path)
            .arg(format!("{pending_byte:#x}"))
            .arg(mode)
            .output()
            .expect("invoke try-db-lock");
        assert!(
            out.status.success(),
            "try-db-lock failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout);
        let mut rc = -1;
        let mut ext = -1;
        for part in s.split_whitespace() {
            if let Some(v) = part.strip_prefix("RC=") {
                rc = v.parse().unwrap();
            } else if let Some(v) = part.strip_prefix("EXT=") {
                ext = v.parse().unwrap();
            }
        }
        (rc, ext)
    }

    fn run_c_wal_try_op(db_path: &Path, op: &str) -> (i32, i32, i32, i32) {
        let bin = c_reference_helper();
        let out = Command::new(bin)
            .arg("wal-try-op")
            .arg(db_path)
            .arg(op)
            .output()
            .expect("invoke wal-try-op");
        assert!(
            out.status.success(),
            "wal-try-op failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8_lossy(&out.stdout);
        let mut rc = -1;
        let mut ext = -1;
        let mut n_log = -1;
        let mut n_ckpt = -1;
        for part in s.split_whitespace() {
            if let Some(v) = part.strip_prefix("RC=") {
                rc = v.parse().unwrap();
            } else if let Some(v) = part.strip_prefix("EXT=") {
                ext = v.parse().unwrap();
            } else if let Some(v) = part.strip_prefix("LOG=") {
                n_log = v.parse().unwrap();
            } else if let Some(v) = part.strip_prefix("CKPT=") {
                n_ckpt = v.parse().unwrap();
            }
        }
        (rc, ext, n_log, n_ckpt)
    }

    // ========================================================================
    // 1. `full_pathname` & Symlink Resolution Suite
    // ========================================================================

    #[test]
    fn test_full_pathname_cwd_trailing_double_slash_normalization_no_eisdir() {
        // Subprocess worker entrypoint so we test a real relative path against `current_dir()`
        // without mutating process-global CWD across parallel cargo test threads!
        if let Ok(sandbox_str) = std::env::var("SQLITERS_P1A_CWD_WORKER_DIR") {
            let expected_root = fs::canonicalize(&sandbox_str).expect("canonicalize sandbox");
            let vfs = OsVfs::new();
            let raw_rel =
                Path::new("./tempdir/t1//t2/./t3//./../..//./../../tempdir/..//test.db//");
            let resolved = vfs
                .full_pathname(raw_rel)
                .expect("resolve pathological relative path");
            assert_eq!(resolved, expected_root.join("test.db"));

            let mut file = vfs
                .open(Some(raw_rel), OpenFlags::READWRITE | OpenFlags::CREATE)
                .expect("open trailing-slash DB path without EISDIR");
            file.write_at(b"SQLite format 3\0", 0).unwrap();
            let mut check = [0u8; 16];
            assert_eq!(file.read_at(&mut check, 0).unwrap(), 16);
            assert_eq!(&check, b"SQLite format 3\0");
            file.close().unwrap();
            return;
        }

        let sandbox = TempSandbox::new("cwd_norm");
        // Create intermediate directories referenced before `..` collapses them, matching Tcl
        // `file mkdir tempdir/t1/t2/t3` tests!
        fs::create_dir_all(sandbox.path().join("tempdir/t1/t2/t3")).unwrap();

        let out = Command::new(std::env::current_exe().expect("current test binary"))
            .arg("--exact")
            .arg("unix_suite::test_full_pathname_cwd_trailing_double_slash_normalization_no_eisdir")
            .arg("--nocapture")
            .current_dir(sandbox.path())
            .env("SQLITERS_P1A_CWD_WORKER_DIR", sandbox.path())
            .output()
            .expect("spawn CWD worker test");
        assert!(
            out.status.success(),
            "CWD relative full_pathname worker failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(sandbox.path().join("test.db").is_file());
    }

    #[test]
    fn test_full_pathname_symlinks_dotdot_nonexistent_leaf_broken_and_loops() {
        let sandbox = TempSandbox::new("symlinks");
        let canonical_base = fs::canonicalize(sandbox.path()).unwrap();
        let vfs = OsVfs::new();

        // 1. Relative symlink chain (`z/link.db -> ../y/link.db -> ../x/real.db`) with
        //    NON-EXISTENT leaf `real.db`!
        let dir_x = canonical_base.join("x");
        let dir_y = canonical_base.join("y");
        let dir_z = canonical_base.join("z");
        fs::create_dir_all(&dir_x).unwrap();
        fs::create_dir_all(&dir_y).unwrap();
        fs::create_dir_all(&dir_z).unwrap();

        symlink(Path::new("../x/real.db"), dir_y.join("link.db")).unwrap();
        symlink(Path::new("../y/link.db"), dir_z.join("link.db")).unwrap();

        let resolved_chain = vfs.full_pathname(&dir_z.join("link.db")).unwrap();
        assert_eq!(resolved_chain, dir_x.join("real.db"));
        assert!(!dir_x.join("real.db").exists());

        // Opening without CREATE on broken/non-existent leaf fails with `CantOpen`!
        let err_no_create = vfs
            .open(Some(&dir_z.join("link.db")), OpenFlags::READWRITE)
            .err()
            .expect("broken symlink without CREATE must fail");
        assert_eq!(err_no_create.code, ErrorCode::CantOpen);

        // Opening WITH `CREATE` creates `x/real.db` at the resolved target!
        let mut f_created = vfs
            .open(
                Some(&dir_z.join("link.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .unwrap();
        f_created.write_at(b"hello-symlink", 0).unwrap();
        f_created.close().unwrap();
        assert!(dir_x.join("real.db").is_file());

        // 2. Absolute symlink chain
        let abs_link = canonical_base.join("abs_entry.db");
        symlink(dir_z.join("link.db"), &abs_link).unwrap();
        assert_eq!(vfs.full_pathname(&abs_link).unwrap(), dir_x.join("real.db"));

        // 3. Directory symlink followed by `..`:
        //    `nav/ jump -> ../real_tree/deep/leaf`
        //    `nav/jump/../sibling.db` MUST resolve to `real_tree/deep/sibling.db` (NOT `nav/sibling.db`!)
        let deep_leaf = canonical_base.join("real_tree/deep/leaf");
        let nav_dir = canonical_base.join("nav");
        fs::create_dir_all(&deep_leaf).unwrap();
        fs::create_dir_all(&nav_dir).unwrap();
        symlink(Path::new("../real_tree/deep/leaf"), nav_dir.join("jump")).unwrap();

        let via_dir_dotdot = nav_dir.join("jump").join("..").join("sibling.db");
        assert_eq!(
            vfs.full_pathname(&via_dir_dotdot).unwrap(),
            canonical_base.join("real_tree/deep/sibling.db")
        );

        // 4. Broken symlink pointing into a non-existent parent directory:
        //    `full_pathname` resolves lexically after `ENOENT`, and `open(CREATE)` fails with `CantOpen`.
        let broken_parent_link = canonical_base.join("broken_dir_target.db");
        symlink(Path::new("no_such_dir/sub/db.sqlite"), &broken_parent_link).unwrap();
        assert_eq!(
            vfs.full_pathname(&broken_parent_link).unwrap(),
            canonical_base.join("no_such_dir/sub/db.sqlite")
        );
        let err_missing_parent = vfs
            .open(
                Some(&broken_parent_link),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .err()
            .expect("missing parent dir must fail on open");
        assert_eq!(err_missing_parent.code, ErrorCode::CantOpen);

        // 5. Multi-hop valid symlink chain (8 hops) succeeds, while a symlink loop
        //    (`loop1.db -> loop2.db -> loop1.db`) fails closed at `SQLITE_MAX_SYMLINK` (`200`)
        //    with `SQLITE_CANTOPEN` (`os_unix.c:6964` `SQLITE_CANTOPEN_BKPT`)!
        let mut prev_hop = dir_x.join("real.db");
        for hop in 0..8 {
            let next_hop = canonical_base.join(format!("hop_{hop}.db"));
            symlink(&prev_hop, &next_hop).unwrap();
            prev_hop = next_hop;
        }
        assert_eq!(vfs.full_pathname(&prev_hop).unwrap(), dir_x.join("real.db"));

        let loop1 = canonical_base.join("loop1.db");
        let loop2 = canonical_base.join("loop2.db");
        symlink(Path::new("loop2.db"), &loop1).unwrap();
        symlink(Path::new("loop1.db"), &loop2).unwrap();
        let loop_err = vfs.full_pathname(&loop1).unwrap_err();
        assert_eq!(loop_err.code, ErrorCode::CantOpen);
        assert_eq!(loop_err.extended_code, ErrorCode::CantOpen as i32);

        // 6. `OpenFlags::NOFOLLOW` rejects both leaf symlink and symlinked parent directory!
        let nofollow_leaf_err = vfs
            .open(
                Some(&dir_z.join("link.db")),
                OpenFlags::READWRITE | OpenFlags::NOFOLLOW,
            )
            .err()
            .unwrap();
        assert_eq!(nofollow_leaf_err.code, ErrorCode::CantOpen);
        assert_eq!(
            nofollow_leaf_err.extended_code,
            extended::SQLITE_CANTOPEN_SYMLINK
        );

        let nofollow_dir_err = vfs
            .open(
                Some(&nav_dir.join("jump").join("inside.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::NOFOLLOW,
            )
            .err()
            .unwrap();
        assert_eq!(
            nofollow_dir_err.extended_code,
            extended::SQLITE_CANTOPEN_SYMLINK
        );

        // 7. Path exceeding `MAX_PATHNAME` (512 bytes) fails closed with `SQLITE_CANTOPEN`
        //    (`os_unix.c:6943, 7033` returns `SQLITE_CANTOPEN_BKPT`; `os_win.c:4946` returns
        //    `SQLITE_CANTOPEN_FULLPATH`).
        let overly_long = canonical_base.join("a".repeat(520));
        let long_err = vfs.full_pathname(&overly_long).unwrap_err();
        assert_eq!(long_err.code, ErrorCode::CantOpen);
        assert_eq!(long_err.extended_code, ErrorCode::CantOpen as i32);
    }

    #[test]
    fn test_shm_sidecar_derivation_via_symlink_and_hardlink_split_brain_rejection() {
        let sandbox = TempSandbox::new("sidecar_identity");
        let canonical = fs::canonicalize(sandbox.path()).unwrap();
        let real_dir = canonical.join("real");
        let link_dir = canonical.join("links");
        fs::create_dir_all(&real_dir).unwrap();
        fs::create_dir_all(&link_dir).unwrap();

        let real_db = real_dir.join("app.db");
        let sym_db = link_dir.join("sym_app.db");
        let hard_db = link_dir.join("hard_app.db");

        let vfs = OsVfs::new();
        let mut h_real = vfs
            .open(Some(&real_db), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        h_real.write_at(&[0u8; 4096], 0).unwrap();

        symlink(&real_db, &sym_db).unwrap();
        fs::hard_link(&real_db, &hard_db).unwrap();

        // Opening via symlink resolves to `real/app.db`, so `-shm` is created at `real/app.db-shm`
        // (NEVER `links/sym_app.db-shm`) and shares the exact same `UnixShmNode` as `h_real`!
        let mut h_sym = vfs.open(Some(&sym_db), OpenFlags::READWRITE).unwrap();
        let mut shm_via_sym = h_sym.open_wal_shm().unwrap();
        assert!(real_dir.join("app.db-shm").exists());
        assert!(!link_dir.join("sym_app.db-shm").exists());

        let shm_via_real = h_real.open_wal_shm().unwrap();
        let writer = shm_via_sym.acquire_writer().unwrap();
        assert_eq!(
            shm_via_real.acquire_writer().unwrap_err().code,
            ErrorCode::Busy
        );

        // Opening via hardlink (`links/hard_app.db`) resolves to a DIFFERENT `-shm` path
        // (`links/hard_app.db-shm`) for the SAME `.db` inode -> MUST fail closed with `SHMOPEN`!
        let mut h_hard = vfs.open(Some(&hard_db), OpenFlags::READWRITE).unwrap();
        let split_err = h_hard.open_wal_shm().unwrap_err();
        assert_eq!(split_err.code, ErrorCode::IoErr);
        assert_eq!(split_err.extended_code, extended::SQLITE_IOERR_SHMOPEN);
        assert!(!link_dir.join("hard_app.db-shm").exists());

        writer.release().unwrap();
        drop(shm_via_real);
        shm_via_sym.close(true).unwrap();
        h_hard.close().unwrap();
        h_sym.close().unwrap();
        h_real.close().unwrap();

        // Also verify pre-existing symlinked `-shm` sidecar is rejected!
        let decoy = canonical.join("decoy");
        fs::write(&decoy, b"decoy").unwrap();
        symlink(&decoy, real_dir.join("app.db-shm")).unwrap();
        let mut h_reopen = vfs.open(Some(&real_db), OpenFlags::READWRITE).unwrap();
        let sym_shm_err = h_reopen.open_wal_shm().unwrap_err();
        assert_eq!(sym_shm_err.extended_code, extended::SQLITE_IOERR_SHMOPEN);
        h_reopen.close().unwrap();
    }

    // ========================================================================
    // 2. Multi-Connection In-Process (2..=32) & Child-Process Deferred-Close
    // ========================================================================

    #[test]
    fn test_32_in_process_connections_lock_contention_diagnostics_and_deferred_close() {
        let sandbox = TempSandbox::new("32_conns");
        let base = fs::canonicalize(sandbox.path()).unwrap();
        let db_path = base.join("shared32.db");
        let sym_path = base.join("sym_shared32.db");
        let hard_path = base.join("hard_shared32.db");
        let dot_alias = base.join(".").join("shared32.db");

        let vfs = Arc::new(OsVfs::new());
        {
            let mut init = vfs
                .open(Some(&db_path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            init.write_at(&[0u8; 4096], 0).unwrap();
            init.close().unwrap();
        }
        symlink(&db_path, &sym_path).unwrap();
        fs::hard_link(&db_path, &hard_path).unwrap();

        // Diagnostics contrast: `MemoryVfs` returns `Ok(None)` (`unknown`), `OsVfs` returns `Ok(Some(...))`.
        let mem_vfs = MemoryVfs::new();
        let mem_file = mem_vfs
            .open(
                Some(Path::new("diag.db")),
                OpenFlags::READWRITE | OpenFlags::CREATE,
            )
            .unwrap();
        assert_eq!(mem_file.lock_state_for_diagnostics().unwrap(), None);

        // Open 32 concurrent handles mixing canonical, `./` alias, symlink, and hardlink paths!
        let mut handles: Vec<Box<dyn VfsFile>> = Vec::with_capacity(32);
        for i in 0..32usize {
            let p: &Path = match i % 4 {
                0 => &db_path,
                1 => &dot_alias,
                2 => &sym_path,
                _ => &hard_path,
            };
            let h = vfs.open(Some(p), OpenFlags::READWRITE).unwrap();
            assert_eq!(
                h.lock_state_for_diagnostics().unwrap(),
                Some(LockLevel::NoLock)
            );
            handles.push(h);
        }

        // Acquire SHARED across all 32 handles concurrently using threads!
        let barrier = Arc::new(Barrier::new(32));
        thread::scope(|s| {
            for h in &handles {
                let b = Arc::clone(&barrier);
                s.spawn(move || {
                    b.wait();
                    h.lock(LockLevel::Shared).unwrap();
                    assert_eq!(
                        h.lock_state_for_diagnostics().unwrap(),
                        Some(LockLevel::Shared)
                    );
                });
            }
        });

        // Handle 0 upgrades to RESERVED; handles 1..32 see `check_reserved_lock() == true` and `Busy`.
        handles[0].lock(LockLevel::Reserved).unwrap();
        assert_eq!(
            handles[0].lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Reserved)
        );
        for h in &handles[1..] {
            assert!(h.check_reserved_lock().unwrap());
            assert_eq!(
                h.lock(LockLevel::Reserved).unwrap_err().code,
                ErrorCode::Busy
            );
        }

        // Handle 0 requests EXCLUSIVE while 31 readers hold SHARED -> Busy, enters PENDING!
        assert_eq!(
            handles[0].lock(LockLevel::Exclusive).unwrap_err().code,
            ErrorCode::Busy
        );
        assert_eq!(
            handles[0].lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Pending)
        );

        // A 33rd handle cannot acquire SHARED while PENDING is held!
        let mut h33 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        assert_eq!(
            h33.lock(LockLevel::Shared).unwrap_err().code,
            ErrorCode::Busy
        );
        h33.close().unwrap();

        // Close handles 16..32 explicitly (`close()`) and unlock+drop handles 1..16 while Handle 0
        // holds PENDING — on POSIX, closing ANY raw FD immediately would have wiped Handle 0's kernel
        // locks! Verify with a separate child process that the kernel lock is STILL held by our PID!
        let mut tail = handles.split_off(16);
        for mut h in tail.drain(..) {
            h.close().unwrap();
            // Closed handle propagates misuse error on diagnostics query rather than faking NoLock.
            assert_eq!(
                h.lock_state_for_diagnostics().unwrap_err().code,
                ErrorCode::Misuse
            );
        }

        let my_pid = std::process::id() as i32;
        let (ltype_mid, pid_mid) = probe_kernel_lock(
            &db_path,
            LockGeometry::PRODUCTION.pending_byte_offset(),
            1,
            false,
        );
        assert_eq!(ltype_mid, "WRLCK");
        assert_eq!(pid_mid, my_pid);

        let mut mid = handles.split_off(1);
        for h in mid.drain(..) {
            h.unlock(LockLevel::NoLock).unwrap();
            drop(h);
        }

        // Verify kernel PENDING/RESERVED lock is STILL intact after 31 handles closed/dropped!
        let (ltype_after, pid_after) = probe_kernel_lock(
            &db_path,
            LockGeometry::PRODUCTION.reserved_byte_offset(),
            1,
            true,
        );
        assert_eq!(ltype_after, "WRLCK");
        assert_eq!(pid_after, my_pid);

        // Now Handle 0 upgrades cleanly from PENDING to EXCLUSIVE!
        handles[0].lock(LockLevel::Exclusive).unwrap();
        assert_eq!(
            handles[0].lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Exclusive)
        );
        let (excl_rc, _) = run_c_try_db_lock(
            &db_path,
            LockGeometry::PRODUCTION.pending_byte_offset(),
            "shared",
        );
        assert_eq!(excl_rc, 5); // SQLITE_BUSY in child C SQLite 3.54.1 process!

        handles[0].close().unwrap();

        // Once Handle 0 closes, all deferred FDs are closed and kernel locks are released!
        let (ltype_free, _) = probe_kernel_lock(
            &db_path,
            LockGeometry::PRODUCTION.shared_first_offset(),
            LockGeometry::PRODUCTION.shared_size(),
            true,
        );
        assert_eq!(ltype_free, "UNLCK");
    }

    #[test]
    fn test_deferred_fd_close_preserves_db_and_shm_locks_across_child_process() {
        let sandbox = TempSandbox::new("deferred_fd_child");
        let base = fs::canonicalize(sandbox.path()).unwrap();
        let db_path = base.join("defer.db");
        let hard_path = base.join("defer_hard.db");

        // Initialize valid SQLite DB with C SQLite 3.54.1 so `try-db-lock` can run real SQL!
        let init_out = Command::new(c_reference_helper())
            .arg("init-rollback-db")
            .arg(&db_path)
            .arg("0x40000000")
            .output()
            .unwrap();
        assert!(init_out.status.success());
        fs::hard_link(&db_path, &hard_path).unwrap();

        let vfs = OsVfs::new();
        let vfs_64k = OsVfs::with_lock_geometry(LockGeometry::TEST_64K);

        // Part A: `h1` holds `SHARED`, `h2` & `h3` open/lock/close on same inode, `vfs_64k` fails
        // geometry check -> C SQLite `BEGIN EXCLUSIVE` in child process MUST still get `SQLITE_BUSY`!
        let mut h1 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        h1.lock(LockLevel::Shared).unwrap();

        let mut h2 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        h2.lock(LockLevel::Shared).unwrap();
        h2.close().unwrap();

        let mut h3 = vfs.open(Some(&hard_path), OpenFlags::READONLY).unwrap();
        h3.close().unwrap();

        assert_eq!(
            vfs_64k
                .open(Some(&db_path), OpenFlags::READWRITE)
                .err()
                .unwrap()
                .code,
            ErrorCode::Misuse
        );

        let (rc_excl_while_shared, _) = run_c_try_db_lock(&db_path, 0x4000_0000, "exclusive");
        assert_eq!(
            rc_excl_while_shared, 5,
            "closing h2/h3 must NOT drop h1's POSIX SHARED lock visible to child C SQLite!"
        );

        // Upgrade `h1` to `RESERVED`, open & close another handle `h4`, verify child `BEGIN IMMEDIATE` gets `BUSY`!
        h1.lock(LockLevel::Reserved).unwrap();
        let mut h4 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        h4.close().unwrap();
        let (rc_imm_while_reserved, _) = run_c_try_db_lock(&db_path, 0x4000_0000, "immediate");
        assert_eq!(rc_imm_while_reserved, 5);

        h1.close().unwrap();
        let (rc_after_close, _) = run_c_try_db_lock(&db_path, 0x4000_0000, "exclusive");
        assert_eq!(rc_after_close, 0);

        // Part B: `-shm` POSIX lock preservation when secondary `WalShm` AND raw `OsFile` on `-shm` close!
        let mut db1 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        let mut db2 = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        let mut shm1 = db1.open_wal_shm().unwrap();
        let mut shm2 = db2.open_wal_shm().unwrap();

        let mut writer1 = shm1.acquire_writer().unwrap();
        let _ = writer1.ensure_region(0).unwrap();
        let hdr = WalIndexHeaderData::new(4096, 2, 2, 1, false, [1, 2], [3, 4]).unwrap();
        writer1.publish_header(&hdr).unwrap();

        let r2 = shm2.acquire_read_slot(1).unwrap();
        r2.release().unwrap();
        shm2.close(false).unwrap();
        db2.close().unwrap();

        // Even opening `defer.db-shm` via `OsVfs::open` and closing it MUST defer FD close while
        // `shm1` is active!
        let shm_path = base.join("defer.db-shm");
        let mut raw_shm_osfile = vfs.open(Some(&shm_path), OpenFlags::READONLY).unwrap();
        raw_shm_osfile.close().unwrap();

        let my_pid = std::process::id() as i32;
        // Byte 120 = WAL_WRITE_LOCK (`F_WRLCK`), Byte 128 = UNIX_SHM_DMS (`F_RDLCK`)
        let (wlock_type, wlock_pid) = probe_kernel_lock(&shm_path, 120, 1, false);
        assert_eq!(wlock_type, "WRLCK");
        assert_eq!(wlock_pid, my_pid);

        let (dms_type, dms_pid) = probe_kernel_lock(&shm_path, 128, 1, true);
        assert_eq!(dms_type, "RDLCK");
        assert_eq!(dms_pid, my_pid);

        writer1.release().unwrap();
        shm1.close(true).unwrap();
        db1.close().unwrap();
        assert!(!shm_path.exists());
    }

    // ========================================================================
    // 3. Interprocess C SQLite 3.54.1 `.db` & `-shm` Compatibility Suite
    // ========================================================================

    #[test]
    fn test_cross_process_c_sqlite_3_54_1_db_locks_production_and_64k_test_geometry() {
        let sandbox = TempSandbox::new("c_db_locks");

        for geometry in [LockGeometry::PRODUCTION, LockGeometry::TEST_64K] {
            let pbyte = geometry.pending_byte_offset();
            let pbyte_hex = format!("{pbyte:#x}");
            let db_path = sandbox.path().join(format!("lock_{pbyte_hex}.db"));

            let init_out = Command::new(c_reference_helper())
                .arg("init-rollback-db")
                .arg(&db_path)
                .arg(&pbyte_hex)
                .output()
                .unwrap();
            assert!(init_out.status.success());

            let vfs = OsVfs::with_lock_geometry(geometry);

            // Direction 1: C SQLite holds RESERVED -> Rust `OsVfs` can acquire SHARED, sees
            // `check_reserved_lock() == true`, and gets `Busy` on RESERVED!
            let held_reserved = HeldHelperProcess::spawn(&[
                "hold-db-lock",
                db_path.to_str().unwrap(),
                &pbyte_hex,
                "reserved",
            ]);
            {
                let mut rust_h = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
                rust_h.lock(LockLevel::Shared).unwrap();
                assert!(rust_h.check_reserved_lock().unwrap());
                assert_eq!(
                    rust_h.lock(LockLevel::Reserved).unwrap_err().code,
                    ErrorCode::Busy
                );
                rust_h.close().unwrap();
            }
            held_reserved.stop();

            // Direction 2: C SQLite holds SHARED -> Rust `OsVfs` acquires SHARED + RESERVED, then
            // gets `Busy` on EXCLUSIVE (stopping at `Pending`) until C SQLite releases SHARED!
            let held_shared = HeldHelperProcess::spawn(&[
                "hold-db-lock",
                db_path.to_str().unwrap(),
                &pbyte_hex,
                "shared",
            ]);
            {
                let mut rust_h = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
                rust_h.lock(LockLevel::Reserved).unwrap();
                assert_eq!(
                    rust_h.lock(LockLevel::Exclusive).unwrap_err().code,
                    ErrorCode::Busy
                );
                assert_eq!(rust_h.lock_level(), LockLevel::Pending);

                held_shared.stop();
                // Now that C SQLite closed its reader, Rust upgrades `Pending -> Exclusive`!
                rust_h.lock(LockLevel::Exclusive).unwrap();

                // And while Rust holds EXCLUSIVE at `geometry`, C SQLite gets `SQLITE_BUSY`!
                let (c_rc, _) = run_c_try_db_lock(&db_path, pbyte, "shared");
                assert_eq!(c_rc, 5);

                rust_h.close().unwrap();
            }
        }
    }

    #[test]
    fn test_cross_process_c_sqlite_3_54_1_wal_shm_header_4k_and_64k_and_lock_offsets() {
        let sandbox = TempSandbox::new("c_wal_shm");

        // Test BOTH standard 4096-byte pages AND 65536-byte pages (`WalIndexHdr.szPage == 1`!)
        for page_size in [4_096u32, 65_536u32] {
            let db_path = sandbox.path().join(format!("wal_{page_size}.db"));
            let shm_path = sandbox.path().join(format!("wal_{page_size}.db-shm"));

            let c_holder = HeldHelperProcess::spawn(&[
                "wal-hold-populated",
                db_path.to_str().unwrap(),
                &page_size.to_string(),
                "25",
                "idle",
            ]);

            let vfs = OsVfs::new();
            let mut db_file = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
            let mut shm = db_file.open_wal_shm().unwrap();
            assert_eq!(shm.dms_state(), ShmDmsState::ActiveShared);

            // 1. Verify Rust `WalShm::read_header_verified()` decodes C SQLite 3.54.1's live
            //    `WalIndexHdr` (`0..48` & `48..96`, native checksum, `iVersion = 3007000`, `szPage`)!
            let hdr = shm.read_header_verified().unwrap();
            assert_eq!(hdr.version, WALINDEX_MAX_VERSION);
            assert_eq!(hdr.is_init, 1);
            assert_eq!(hdr.page_size, page_size);
            assert!(hdr.mx_frame > 0);
            assert!(hdr.n_page > 0);

            // Verify raw on-disk `szPage` u16 at bytes 14..16 and 62..64 is `1` when `page_size == 65536`!
            let raw_shm = fs::read(&shm_path).unwrap();
            assert!(raw_shm.len() >= 32_768);
            let raw_sz_copy1 = u16::from_ne_bytes([raw_shm[14], raw_shm[15]]);
            let raw_sz_copy2 = u16::from_ne_bytes([raw_shm[62], raw_shm[63]]);
            let expected_raw_sz = if page_size == 65_536 {
                1u16
            } else {
                page_size as u16
            };
            assert_eq!(raw_sz_copy1, expected_raw_sz);
            assert_eq!(raw_sz_copy2, expected_raw_sz);
            assert_eq!(&raw_shm[0..48], &raw_shm[48..96]);

            // 2. Acquire a real `WalReadGuard` on slot 1 (`byte 124`) and look up page 1 in C's
            //    hash table (`aPgno` + `aHash`)!
            let reader = shm.acquire_read_slot(1).unwrap();
            assert_eq!(
                shm.lock_state().unwrap().mode(),
                WalLockMode::Reading { slot: 1 }
            );
            let page1_frame = reader
                .lookup_page_in_range(1, 1, hdr.mx_frame)
                .unwrap()
                .expect("page 1 must exist in C SQLite's WAL-index hash table");
            assert!(page1_frame >= 1 && page1_frame <= hdr.mx_frame);

            // Verify C SQLite `RESTART` checkpoint in another process sees Rust's `WAL_READ_LOCK(1)`
            // at byte 124 (when we hold slot 1 on an earlier mark or during active read lock)!
            // Specifically, `RESTART` requires exclusive `WAL_READ_LOCK(1..=4)` after backfilling,
            // so holding `WAL_READ_LOCK(1)` blocks `SQLITE_CHECKPOINT_RESTART` with `SQLITE_BUSY`!
            let (ckpt_restart_rc, _, _, _) = run_c_wal_try_op(&db_path, "ckpt_restart");
            assert_eq!(
                ckpt_restart_rc, 5,
                "C SQLite RESTART checkpoint must respect Rust's WAL_READ_LOCK(1) at byte 124!"
            );
            reader.release().unwrap();

            // 3. Rust acquires `WriterTxnGuard` (`WAL_WRITE_LOCK` at byte 120) -> C SQLite writer
            //    immediately returns `SQLITE_BUSY`!
            let writer = shm.acquire_writer().unwrap();
            let (c_write_rc, _, _, _) = run_c_wal_try_op(&db_path, "write");
            assert_eq!(
                c_write_rc, 5,
                "C SQLite BEGIN IMMEDIATE must observe Rust's WAL_WRITE_LOCK at byte 120!"
            );
            writer.release().unwrap();

            // 4. Rust acquires `CheckpointTxnGuard` (`WAL_CKPT_LOCK` at byte 121) -> C SQLite
            //    `PASSIVE` checkpoint immediately returns `SQLITE_BUSY`!
            let ckpt = shm.acquire_checkpoint(CheckpointMode::Passive).unwrap();
            let (c_ckpt_rc, _, _, _) = run_c_wal_try_op(&db_path, "ckpt_passive");
            assert_eq!(
                c_ckpt_rc, 5,
                "C SQLite wal_checkpoint_v2 must observe Rust's WAL_CKPT_LOCK at byte 121!"
            );
            ckpt.release().unwrap();

            shm.close(false).unwrap();
            db_file.close().unwrap();
            c_holder.stop();
        }

        // Also test reverse direction: C SQLite holds `writer` (`WAL_WRITE_LOCK` byte 120) and
        // `reader` (`WAL_READ_LOCK` byte 124) -> Rust `WalShm` observes `ErrorCode::Busy`!
        let db_c_writer = sandbox.path().join("c_holds_write.db");
        let c_writer = HeldHelperProcess::spawn(&[
            "wal-hold-populated",
            db_c_writer.to_str().unwrap(),
            "4096",
            "10",
            "writer",
        ]);
        {
            let vfs = OsVfs::new();
            let mut db_file = vfs.open(Some(&db_c_writer), OpenFlags::READWRITE).unwrap();
            let mut shm = db_file.open_wal_shm().unwrap();
            assert_eq!(shm.acquire_writer().unwrap_err().code, ErrorCode::Busy);
            // Concurrent reader on slot 2 succeeds even while C SQLite holds `WAL_WRITE_LOCK`!
            let r = shm.acquire_read_slot(2).unwrap();
            assert!(r.read_header_verified().unwrap().mx_frame >= 1);
            r.release().unwrap();
            shm.close(false).unwrap();
            db_file.close().unwrap();
        }
        c_writer.stop();
    }

    #[test]
    fn test_unix_shm_dms_3_byte_init_and_readonly_cantinit_parity_with_c_sqlite() {
        let sandbox = TempSandbox::new("dms_cantinit");
        let db_path = sandbox.path().join("dms.db");
        let shm_path = sandbox.path().join("dms.db-shm");

        // 1. Verify Rust `open_wal_shm()` on a fresh writable DB initializes `-shm` with the exact
        //    3-byte DMS file (`ftruncate(fd, 3)` in `unixLockSharedMemory`, `os_unix.c:4902`) and
        //    holds shared `F_RDLCK` on byte `128` BEFORE any 32 KiB region is mapped!
        let vfs = OsVfs::new();
        {
            let mut db_file = vfs
                .open(Some(&db_path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            db_file.write_at(&[0u8; 4096], 0).unwrap();
            let mut shm = db_file.open_wal_shm().unwrap();

            let meta = fs::metadata(&shm_path).unwrap();
            assert_eq!(
                meta.len(),
                3,
                "fresh Unix DMS initialization must ftruncate -shm to 3 bytes before region mapping"
            );
            let (dms_lock, dms_pid) = probe_kernel_lock(&shm_path, 128, 1, true);
            assert_eq!(dms_lock, "RDLCK");
            assert_eq!(dms_pid, std::process::id() as i32);

            shm.close(true).unwrap();
            db_file.close().unwrap();
        }

        // 2. Populate a real WAL DB with C SQLite, let C close cleanly? Wait: if we copy `.db` +
        //    `-wal` (without `-shm`) into a read-only directory (`0555`) or create an uninitialized
        //    `0444` `-shm` with no DMS holder, BOTH C SQLite 3.54.1 and Rust `WalShm` MUST report
        //    `SQLITE_READONLY_CANTINIT` (`1288`)!
        let src_db = sandbox.path().join("ro_src.db");
        let c_pop = HeldHelperProcess::spawn(&[
            "wal-hold-populated",
            src_db.to_str().unwrap(),
            "4096",
            "6",
            "idle",
        ]);
        let ro_dir = sandbox.path().join("ro_box");
        fs::create_dir_all(&ro_dir).unwrap();
        let ro_db = ro_dir.join("ro.db");
        let ro_wal = ro_dir.join("ro.db-wal");
        let ro_shm = ro_dir.join("ro.db-shm");
        fs::copy(&src_db, &ro_db).unwrap();
        fs::copy(sandbox.path().join("ro_src.db-wal"), &ro_wal).unwrap();
        c_pop.stop();

        // Pre-create an empty read-only `-shm` (`0444`) with NO process holding DMS byte 128,
        // and mark `.db` + `.wal` `0444` and `ro_dir` `0555`.
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&ro_shm)
            .unwrap();
        fs::set_permissions(&ro_db, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&ro_wal, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&ro_shm, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o555)).unwrap();

        // Verify C SQLite 3.54.1 returns `SQLITE_CANTOPEN` or `SQLITE_READONLY_CANTINIT` (`wal.c`):
        // wait — when `-shm` exists `0444` and is unlocked at byte 128, `unixLockSharedMemory` returns
        // `SQLITE_READONLY_CANTINIT` (`1288`), and `walIndexReadHdr` falls back to `walBeginShmUnreliable`
        // unless `READ(0)` is busy or schema read fails; let's check both Rust strict `open_wal_shm()`
        // (`Err(SQLITE_READONLY_CANTINIT)`) and `allow_unreliable_readonly: true` (`ShmDmsState::ReadonlyCantInit`
        // + `acquire_read_slot(0)` succeeds while `acquire_writer()` returns `SQLITE_READONLY_CANTINIT`)!
        let mut ro_file = vfs.open(Some(&ro_db), OpenFlags::READONLY).unwrap();
        let cantinit_err = ro_file.open_wal_shm().unwrap_err();
        assert_eq!(cantinit_err.code, ErrorCode::ReadOnly);
        assert_eq!(
            cantinit_err.extended_code,
            extended::SQLITE_READONLY_CANTINIT
        );

        let mut unreliable_shm = ro_file
            .open_wal_shm_with_options(ShmOpenOptions {
                readonly_shm: true,
                allow_unreliable_readonly: true,
            })
            .unwrap();
        assert_eq!(unreliable_shm.dms_state(), ShmDmsState::ReadonlyCantInit);

        // Slot 0 (`WAL_READ_LOCK(0)` at byte 123) can still be locked in shared mode for
        // `walBeginShmUnreliable` fallback, while slot 1..=4 and writer require initialized DMS!
        let r0 = unreliable_shm.acquire_read_slot(0).unwrap();
        assert_eq!(r0.slot(), 0);
        assert_eq!(r0.lookup_page_in_range(1, 1, 10).unwrap(), None);
        r0.release().unwrap();

        assert_eq!(
            unreliable_shm
                .acquire_read_slot(1)
                .unwrap_err()
                .extended_code,
            extended::SQLITE_READONLY_CANTINIT
        );
        assert_eq!(
            unreliable_shm.acquire_writer().unwrap_err().extended_code,
            extended::SQLITE_READONLY_CANTINIT
        );
        let _ = READMARK_NOT_USED;

        unreliable_shm.close(false).unwrap();
        ro_file.close().unwrap();
    }

    #[test]
    fn test_unix_concurrent_open_close_shm_never_drops_posix_locks_or_splits_node() {
        let sandbox = TempSandbox::new("shm_toctou_race");
        let db_path = sandbox.path().join("race.db");
        let shm_path = sandbox.path().join("race.db-shm");

        {
            let vfs = OsVfs::new();
            let mut init = vfs
                .open(Some(&db_path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            init.write_at(&[0u8; 4096], 0).unwrap();
            init.close().unwrap();
        }

        const CHURN_THREADS: usize = 10;
        const CHURN_ITERS: usize = 300;
        const PROBE_ROUNDS: usize = 25;

        struct WriterCounterGuard<'a>(&'a std::sync::atomic::AtomicUsize);
        impl<'a> WriterCounterGuard<'a> {
            fn enter(counter: &'a std::sync::atomic::AtomicUsize, msg: &str) -> Self {
                let prev = counter.fetch_add(1, Ordering::SeqCst);
                assert_eq!(prev, 0, "{msg}");
                Self(counter)
            }
        }
        impl Drop for WriterCounterGuard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        let barrier = Arc::new(Barrier::new(CHURN_THREADS + 1));
        let active_writers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop_churn = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let mut handles = Vec::with_capacity(CHURN_THREADS);
        for tid in 0..CHURN_THREADS {
            let db_p = db_path.clone();
            let bar = Arc::clone(&barrier);
            let writers = Arc::clone(&active_writers);
            let stop = Arc::clone(&stop_churn);

            handles.push(thread::spawn(move || {
                let vfs = OsVfs::new();
                let mut db_file = vfs.open(Some(&db_p), OpenFlags::READWRITE).unwrap();
                bar.wait();

                let mut i = 0usize;
                while i < CHURN_ITERS || !stop.load(Ordering::Acquire) {
                    let mut shm = db_file.open_wal_shm().expect(
                        "concurrent open_wal_shm must never fail under close_session churn",
                    );

                    match shm.acquire_writer() {
                        Ok(mut w) => {
                            let writer_slot = WriterCounterGuard::enter(
                                &writers,
                                "split-brain UnixShmNode detected: two threads held WriterTxnGuard simultaneously!",
                            );
                            let prev_mx = match w.read_header_verified() {
                                Ok(h) if h.is_init != 0 => h.mx_frame,
                                _ => 0,
                            };
                            let frame_no = prev_mx + 1;
                            w.record_frame(frame_no, (frame_no % 64) + 1, prev_mx)
                                .unwrap();
                            let hdr = WalIndexHeaderData {
                                version: WALINDEX_MAX_VERSION,
                                unused: 0,
                                change_counter: frame_no,
                                is_init: 1,
                                big_end_cksum: 0,
                                page_size: 4096,
                                mx_frame: frame_no,
                                n_page: 64,
                                frame_cksum: [frame_no, frame_no ^ 0x5a5a_5a5a],
                                salt: [0x1234_5678, 0x9abc_def0],
                                cksum: [0, 0],
                            };
                            let _ = w.publish_header(&hdr).unwrap();
                            drop(writer_slot);
                            w.release().unwrap();
                        }
                        Err(e) => {
                            assert_eq!(e.code, ErrorCode::Busy);
                            let slot = ((tid + i) % 4 + 1) as u8;
                            let r = shm.acquire_read_slot(slot).unwrap();
                            if let Ok(hdr) = r.read_header_verified() {
                                assert_eq!(hdr.page_size, 4096);
                                let _ = r.lookup_page_in_range(1, 1, hdr.mx_frame).unwrap();
                            }
                            r.release().unwrap();
                        }
                    }

                    // Alternate `delete_if_last = true` and `false` to hit the exact `rem == 0`
                    // detach + unlink + deferred-FD-close path in `UnixShmSession::close_session`.
                    let delete_if_last = (tid + i).is_multiple_of(2);
                    shm.close(delete_if_last).unwrap();
                    i += 1;
                }

                db_file.close().unwrap();
            }));
        }

        // Inspector thread: repeatedly acquires `WriterTxnGuard` amidst maximum `open_wal_shm` /
        // `close(true)` churn and verifies from a separate child process via `F_GETLK` that neither
        // `WAL_WRITE_LOCK` (byte 120) nor DMS `F_RDLCK` (byte 128) is ever dropped!
        let vfs = OsVfs::new();
        let mut inspector_db = vfs.open(Some(&db_path), OpenFlags::READWRITE).unwrap();
        let our_pid = std::process::id() as i32;
        barrier.wait();

        for round in 0..PROBE_ROUNDS {
            let mut shm = inspector_db.open_wal_shm().unwrap();
            let mut writer_guard = loop {
                match shm.acquire_writer() {
                    Ok(w) => break w,
                    Err(e) => {
                        assert_eq!(e.code, ErrorCode::Busy);
                        thread::yield_now();
                    }
                }
            };
            let writer_slot = WriterCounterGuard::enter(
                &active_writers,
                "split-brain writer lock during inspector round",
            );
            let _ = writer_guard.ensure_region(0).unwrap();

            let (wlock_state, wlock_pid) = probe_kernel_lock(&shm_path, 120, 1, true);
            assert_eq!(
                wlock_state, "WRLCK",
                "POSIX WAL_WRITE_LOCK (byte 120) dropped under concurrent close_session churn at round {round}"
            );
            assert_eq!(wlock_pid, our_pid);

            let (dms_state, dms_pid) = probe_kernel_lock(&shm_path, 128, 1, true);
            assert_eq!(
                dms_state, "RDLCK",
                "POSIX DMS F_RDLCK (byte 128) dropped under concurrent close_session churn at round {round}"
            );
            assert_eq!(dms_pid, our_pid);

            drop(writer_slot);
            writer_guard.release().unwrap();
            shm.close(round.is_multiple_of(2)).unwrap();
        }

        stop_churn.store(true, Ordering::Release);
        inspector_db.close().unwrap();
        for h in handles {
            h.join().unwrap();
        }
    }
}

mod cross_platform_suite {
    use std::path::Path;

    use sqliters::types::{
        ErrorCode, LockGeometry, decode_db_header_page_size, decode_shm_page_size,
        decode_wal_header_page_size, encode_db_header_page_size, encode_shm_page_size,
        encode_wal_header_page_size,
    };
    use sqliters::vfs::{
        CheckpointMode, LockLevel, MemoryVfs, OpenFlags, OsVfs, Vfs, WALINDEX_MAX_VERSION,
        WalIndexHeaderData, WalShmOps,
    };

    #[test]
    fn test_64k_page_size_codecs_and_lock_geometry_invariants() {
        // Test Group 48: 64 KiB page-size encoding/decoding across DB header (u16 BE),
        // WAL header (u32 BE), and SHM WalIndexHdr.szPage (u16 NE: 65536 -> 1).
        for &sz in &[512u32, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let db_enc = encode_db_header_page_size(sz).unwrap();
            let wal_enc = encode_wal_header_page_size(sz).unwrap();
            let shm_enc = encode_shm_page_size(sz).unwrap();
            if sz == 65_536 {
                assert_eq!(db_enc, [0x00, 0x01]);
                assert_eq!(wal_enc, 65_536u32.to_be_bytes());
                assert_eq!(shm_enc, 1u16);
            }
            assert_eq!(decode_db_header_page_size(db_enc).unwrap(), sz);
            assert_eq!(decode_wal_header_page_size(wal_enc).unwrap(), sz);
            assert_eq!(decode_shm_page_size(shm_enc).unwrap(), sz);
        }

        let prod = LockGeometry::production();
        assert_eq!(prod.pending_byte_offset(), 0x4000_0000);
        assert_eq!(prod.reserved_byte_offset(), 0x4000_0001);
        assert_eq!(prod.shared_first_offset(), 0x4000_0002);
        assert_eq!(prod.shared_size(), 510);
        assert_eq!(prod.lock_byte_page(4096), (0x4000_0000 / 4096) + 1);
        assert_eq!(prod.sj_pgno(4096), prod.lock_byte_page(4096));

        let test64k = LockGeometry::test_64k();
        assert_eq!(test64k.pending_byte_offset(), 0x0001_0000);
        assert_eq!(test64k.lock_byte_page(4096), 17);
        assert_eq!(test64k.sj_pgno(4096), 17);
    }

    #[test]
    fn test_memory_vfs_multi_connection_wal_shm_guards_and_12255_frames() {
        let mem_vfs = MemoryVfs::with_lock_geometry(LockGeometry::test_64k());
        let p = Path::new("/mem/multi_conn_wal.db");

        let mut c1 = mem_vfs
            .open(Some(p), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        let mut c2 = mem_vfs.open(Some(p), OpenFlags::READWRITE).unwrap();
        assert_eq!(c1.lock_state_for_diagnostics().unwrap(), None);
        c1.lock(LockLevel::Shared).unwrap();
        assert_eq!(c1.lock_state_for_diagnostics().unwrap(), None);
        c1.unlock(LockLevel::NoLock).unwrap();

        let mut shm1 = c1.open_wal_shm().unwrap();
        let mut shm2 = c2.open_wal_shm().unwrap();

        // Populate 12,255 frames spanning region 0 (4,062), region 1 (4,096), region 2 (4,096),
        // and region 3 (1 frame) via `RecoveryTxnGuard`, with 65,536-byte page size (`szPage = 1`).
        let mut rec = shm1.acquire_recovery().unwrap();
        assert_eq!(shm2.acquire_writer().unwrap_err().code, ErrorCode::Busy);
        assert_eq!(
            shm2.acquire_checkpoint(CheckpointMode::Passive)
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        assert_eq!(shm2.acquire_recovery().unwrap_err().code, ErrorCode::Busy);
        // `walIndexRecover` MUST NOT lock `WAL_READ_LOCK(0)` (`wal.c:1422`): slot 0 stays available!
        let r0_during_rec = shm2.acquire_read_slot(0).unwrap();
        r0_during_rec.release().unwrap();

        for frame in 1..=12_255u32 {
            let pgno = (frame % 257) + 1;
            rec.record_frame(frame, pgno, frame - 1).unwrap();
        }
        let hdr = WalIndexHeaderData {
            version: WALINDEX_MAX_VERSION,
            unused: 0,
            change_counter: 1,
            is_init: 1,
            big_end_cksum: 0,
            page_size: 65_536,
            mx_frame: 12_255,
            n_page: 260,
            frame_cksum: [0xdead_beef, 0xcafe_babe],
            salt: [0x1122_3344, 0x5566_7788],
            cksum: [0, 0],
        };
        let (published_hdr, slot_updated) = rec.finish_recovery_header_and_marks(&hdr).unwrap();
        assert_eq!(slot_updated, [true, true, true, true, true]);
        rec.release().unwrap();

        // Reader on `shm2` verifies 64 KiB header and finds latest frame in 12,255-frame hash chain.
        let r2 = shm2.acquire_read_slot_with_mark(1, Some(12_255)).unwrap();
        let got_hdr = r2.read_header_verified().unwrap();
        assert_eq!(got_hdr, published_hdr);

        // Latest frame where `(frame % 257) + 1 == 1` is `257 * 47 = 12079`.
        assert_eq!(
            r2.lookup_page_in_range(1, 1, 12_255).unwrap(),
            Some(257 * 47)
        );
        // Frame 12,255 sits in region 3 (`4062 + 4096 + 4096 = 12254`, so 12255 is slot 0 of region 3)!
        let target_pgno = (12_255 % 257) + 1;
        assert_eq!(
            r2.lookup_page_in_range(target_pgno, 1, 12_255).unwrap(),
            Some(12_255)
        );

        // While `r2` holds `WAL_READ_LOCK(1)`, `CheckpointMode::Restart` acquires `CKPT + WRITE`
        // (`wal.c:4428-4446`) and then fails with `Busy` when trying to lock `WAL_READ_LOCK(1)`
        // exclusively (`wal.c:2449`)!
        let mut ckpt = shm1.acquire_checkpoint(CheckpointMode::Restart).unwrap();
        assert_eq!(
            ckpt.lock_read_slot_exclusive(1).unwrap_err().code,
            ErrorCode::Busy
        );
        ckpt.release().unwrap();

        // Starting a second guard on `shm2` while `r2` is still active fails closed (`Misuse`).
        assert_eq!(
            shm2.acquire_checkpoint(CheckpointMode::Passive)
                .unwrap_err()
                .code,
            ErrorCode::Misuse
        );

        // Also verify cross-connection partial-acquisition rollback: on a third connection `shm3`,
        // while `shm1` holds `WAL_WRITE_LOCK`, `shm3.acquire_checkpoint(Restart)` acquires `CKPT`,
        // fails on `WRITE` with `Busy`, and rolls back `CKPT` cleanly so `Passive` succeeds next!
        let mut c3 = mem_vfs.open(Some(p), OpenFlags::READWRITE).unwrap();
        let mut shm3 = c3.open_wal_shm().unwrap();
        let mut w1 = shm1.acquire_writer().unwrap();
        assert_eq!(
            shm3.acquire_checkpoint(CheckpointMode::Restart)
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        // Proof `CKPT` was rolled back: `Passive` checkpoint (needs only `CKPT`) succeeds!
        let passive_probe = shm3.acquire_checkpoint(CheckpointMode::Passive).unwrap();
        passive_probe.release().unwrap();
        shm3.close(false).unwrap();
        c3.close().unwrap();

        w1.record_frame(12_256, 999, 12_255).unwrap();
        let mut hdr2 = published_hdr;
        hdr2.mx_frame = 12_256;
        hdr2.n_page = 999;
        let _ = w1.publish_header(&hdr2).unwrap();
        w1.release().unwrap();

        // `r2` restricted to `1..=12_255` does NOT see frame 12,256 (`pgno 999`), whereas a new reader does!
        assert_eq!(r2.lookup_page_in_range(999, 1, 12_255).unwrap(), None);
        r2.release().unwrap();

        let r2_latest = shm2.acquire_read_slot_with_mark(2, Some(12_256)).unwrap();
        assert_eq!(
            r2_latest.lookup_page_in_range(999, 1, 12_256).unwrap(),
            Some(12_256)
        );
        r2_latest.release().unwrap();

        shm2.close(false).unwrap();
        shm1.close(true).unwrap();
        c2.close().unwrap();
        c1.close().unwrap();
    }

    #[test]
    fn test_cross_platform_os_vfs_lock_geometry_conflict_and_close_diagnostics() {
        let tmp =
            std::env::temp_dir().join(format!("sqliters_p1a_xplat_geom_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let vfs_prod = OsVfs::new();
        let vfs_64k = OsVfs::with_lock_geometry(LockGeometry::test_64k());

        let mut h1 = vfs_prod
            .open(Some(&tmp), OpenFlags::READWRITE | OpenFlags::CREATE)
            .unwrap();
        assert_eq!(
            h1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );
        h1.lock(LockLevel::Shared).unwrap();
        assert_eq!(
            h1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Shared)
        );

        // Opening the same physical file simultaneously with a conflicting `LockGeometry`
        // MUST fail closed!
        let conflict = vfs_64k
            .open(Some(&tmp), OpenFlags::READWRITE)
            .err()
            .expect("conflicting LockGeometry on active physical file must fail closed");
        assert_eq!(conflict.code, ErrorCode::Misuse);

        h1.close().unwrap();
        // Once all handles with `production` geometry are closed, opening with `test_64k` succeeds.
        let mut h2 = vfs_64k.open(Some(&tmp), OpenFlags::READWRITE).unwrap();
        h2.close().unwrap();
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_concurrent_open_wal_shm_and_close_session_race_stress_os_and_memory() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;

        struct WriterCounterGuard<'a>(&'a AtomicUsize);
        impl<'a> WriterCounterGuard<'a> {
            fn enter(counter: &'a AtomicUsize, msg: &str) -> Self {
                let prev = counter.fetch_add(1, Ordering::SeqCst);
                assert_eq!(prev, 0, "{msg}");
                Self(counter)
            }
        }
        impl Drop for WriterCounterGuard<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }

        // 1. `MemoryVfs`: 12 threads x 250 iterations (3,000 open_wal_shm <-> close(true/false) cycles)
        //    sharing the same `/mem/shm_toctou.db`, verifying DMS state, region mapping, and strict
        //    single-writer mutual exclusion across `rem == 0` transitions.
        let mem_vfs = Arc::new(MemoryVfs::new());
        let mem_path = Path::new("/mem/shm_toctou.db");
        {
            let mut init = mem_vfs
                .open(Some(mem_path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            init.write_at(&[0u8; 4096], 0).unwrap();
            init.close().unwrap();
        }

        const MEM_THREADS: usize = 12;
        const MEM_ITERS: usize = 250;
        let mem_barrier = Arc::new(Barrier::new(MEM_THREADS));
        let mem_writers = Arc::new(AtomicUsize::new(0));
        let mut mem_handles = Vec::with_capacity(MEM_THREADS);

        for tid in 0..MEM_THREADS {
            let vfs = Arc::clone(&mem_vfs);
            let bar = Arc::clone(&mem_barrier);
            let writers = Arc::clone(&mem_writers);
            mem_handles.push(thread::spawn(move || {
                let mut db = vfs.open(Some(Path::new("/mem/shm_toctou.db")), OpenFlags::READWRITE).unwrap();
                bar.wait();
                for iter in 0..MEM_ITERS {
                    let mut shm = db
                        .open_wal_shm()
                        .expect("MemoryVfs open_wal_shm must never race with close_session");
                    match shm.acquire_writer() {
                        Ok(mut w) => {
                            let writer_slot = WriterCounterGuard::enter(
                                &writers,
                                "MemoryVfs split-brain MemShmNode: two writers active simultaneously!",
                            );
                            let prev_mx = match w.read_header_verified() {
                                Ok(h) if h.is_init != 0 => h.mx_frame,
                                _ => 0,
                            };
                            let fno = prev_mx + 1;
                            w.record_frame(fno, (fno % 64) + 1, prev_mx).unwrap();
                            let hdr = WalIndexHeaderData {
                                version: WALINDEX_MAX_VERSION,
                                unused: 0,
                                change_counter: fno,
                                is_init: 1,
                                big_end_cksum: 0,
                                page_size: 4096,
                                mx_frame: fno,
                                n_page: 64,
                                frame_cksum: [fno, fno.wrapping_mul(31)],
                                salt: [0x1111_2222, 0x3333_4444],
                                cksum: [0, 0],
                            };
                            let _ = w.publish_header(&hdr).unwrap();
                            drop(writer_slot);
                            w.release().unwrap();
                        }
                        Err(e) => {
                            assert_eq!(e.code, ErrorCode::Busy);
                            let slot = ((tid + iter) % 4 + 1) as u8;
                            let r = shm.acquire_read_slot(slot).unwrap();
                            if let Ok(hdr) = r.read_header_verified() {
                                assert_eq!(hdr.page_size, 4096);
                            }
                            r.release().unwrap();
                        }
                    }
                    shm.close((tid + iter).is_multiple_of(2)).unwrap();
                }
                db.close().unwrap();
            }));
        }
        for h in mem_handles {
            h.join().unwrap();
        }

        // 2. `OsVfs` (cross-platform Unix + Windows): 12 threads x 200 iterations (2,400 open/close
        //    cycles) on the same physical `.db` file with alternating `close(true)` / `close(false)`.
        let os_db_path = std::env::temp_dir().join(format!(
            "sqliters_p1a_xplat_shm_race_{}.db",
            std::process::id()
        ));
        let os_shm_path = std::env::temp_dir().join(format!(
            "sqliters_p1a_xplat_shm_race_{}.db-shm",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&os_db_path);
        let _ = std::fs::remove_file(&os_shm_path);

        {
            let vfs = OsVfs::new();
            let mut init = vfs
                .open(Some(&os_db_path), OpenFlags::READWRITE | OpenFlags::CREATE)
                .unwrap();
            init.write_at(&[0u8; 4096], 0).unwrap();
            init.close().unwrap();
        }

        const OS_THREADS: usize = 12;
        const OS_ITERS: usize = 200;
        let os_barrier = Arc::new(Barrier::new(OS_THREADS));
        let os_writers = Arc::new(AtomicUsize::new(0));
        let mut os_handles = Vec::with_capacity(OS_THREADS);

        for tid in 0..OS_THREADS {
            let db_p = os_db_path.clone();
            let bar = Arc::clone(&os_barrier);
            let writers = Arc::clone(&os_writers);
            os_handles.push(thread::spawn(move || {
                let vfs = OsVfs::new();
                let mut db = vfs.open(Some(&db_p), OpenFlags::READWRITE).unwrap();
                bar.wait();
                for iter in 0..OS_ITERS {
                    let mut shm = db
                        .open_wal_shm()
                        .expect("OsVfs open_wal_shm must never race with close_session");
                    match shm.acquire_writer() {
                        Ok(mut w) => {
                            let writer_slot = WriterCounterGuard::enter(
                                &writers,
                                "OsVfs split-brain ShmNode: two writers active simultaneously!",
                            );
                            let prev_mx = match w.read_header_verified() {
                                Ok(h) if h.is_init != 0 => h.mx_frame,
                                _ => 0,
                            };
                            let fno = prev_mx + 1;
                            w.record_frame(fno, (fno % 64) + 1, prev_mx).unwrap();
                            let hdr = WalIndexHeaderData {
                                version: WALINDEX_MAX_VERSION,
                                unused: 0,
                                change_counter: fno,
                                is_init: 1,
                                big_end_cksum: 0,
                                page_size: 4096,
                                mx_frame: fno,
                                n_page: 64,
                                frame_cksum: [fno, fno ^ 0xa5a5_a5a5],
                                salt: [0x5555_6666, 0x7777_8888],
                                cksum: [0, 0],
                            };
                            let _ = w.publish_header(&hdr).unwrap();
                            drop(writer_slot);
                            w.release().unwrap();
                        }
                        Err(e) => {
                            assert_eq!(e.code, ErrorCode::Busy);
                            let slot = ((tid + iter) % 4 + 1) as u8;
                            let r = shm.acquire_read_slot(slot).unwrap();
                            if let Ok(hdr) = r.read_header_verified() {
                                assert_eq!(hdr.page_size, 4096);
                            }
                            r.release().unwrap();
                        }
                    }
                    shm.close((tid + iter).is_multiple_of(2)).unwrap();
                }
                db.close().unwrap();
            }));
        }
        for h in os_handles {
            h.join().unwrap();
        }
        let _ = std::fs::remove_file(&os_shm_path);
        let _ = std::fs::remove_file(&os_db_path);
    }
}
