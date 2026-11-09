//! Phase 1.2-A WAL Protocol, Pager Integration & C SQLite 3.54.1 Interoperability Suite:
//! 1. Format 3 header version rules & two-phase locked WAL discovery (Groups 35 & 62)
//! 2. `READ(0) -> useWal=1 -> READ(1..=4)` all 3 `walRestartLog` paths, `Write -> Read` slot
//!    retention, and `SQLITE_BUSY_SNAPSHOT` (Group 24)
//! 3. 4-word WAL savepoints, mid-tx spill, `iReCksum` frame overwrite, and `ROLLBACK TO` across
//!    `nCkpt` log wrap (Groups 55 & 61)
//! 4. WAL-index growth across block boundaries (`4062/4063`, `8158/8159`, `12254/12255`) and
//!    bidirectional C SQLite 3.54.1 <-> Rust interop across `512`, `1024`, `4096`, `65536` (Groups 5 & 48)
//! 5. `walIndexRecover` lock discipline (`WRITE + CKPT + RECOVER`, never `READ(0)`), read-mark
//!    preservation on `Busy` slots, and uncommitted/torn tail recovery (Groups 6 & 15)
//! 6. Checkpoint modes (`NOOP`, `PASSIVE`, `FULL`, `RESTART`, `TRUNCATE`) and deterministic
//!    March 2026 WAL-reset salt race regression guard (Groups 7 & 8)
//! 7. Read-only WAL (`WAL_SHM_RDONLY` / `bShmUnreliable` + writer re-attach), `immutable`,
//!    `locking_mode = EXCLUSIVE`, rollback-journaled `DELETE <-> WAL` Page 1 `(1,1)<->(2,2)`
//!    transitions, and last-connection `close()` checkpoint + unlink (Groups 9, 10, 11)
#![forbid(unsafe_code)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use sqliters::btree::Btree;
use sqliters::pager::{
    DatabaseHeader, JournalMode, LockingMode, Pager, PagerOpenOptions, TransactionState,
    WAL_FRAME_HDRSIZE, WAL_HDRSIZE, WAL_MAGIC_LE, Wal, WalCheckpointMode, WalExclusiveMode,
    WalFileHeader, wal_encode_frame_header, wal_frame_offset,
};
use sqliters::types::{
    ErrorCode, LockGeometry, Pgno, Result, SqliteError, Value, decode_record, encode_record,
    extended,
};
use sqliters::vfs::{
    CheckpointMode, LockLevel, MemoryVfs, OpenFlags, OsVfs, READMARK_NOT_USED, ShmOpenOptions,
    SyncFlags, Vfs, VfsFile, WalIndexHeaderData, WalShmOps,
};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p12a_{prefix}_{pid}_{id}"));
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
        // Restore directory & file permissions if a read-only test tightened them before cleanup.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for entry in entries.flatten() {
                    let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o644));
                }
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Initializes an SQLite schema with `CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB)` inside
/// `bt` (must be in an active write transaction on a newly initialized database) and returns the
/// root page of `items`.
fn init_items_schema_in_btree(bt: &mut Btree) -> Pgno {
    let tbl_root = bt.create_table().expect("create items table btree");
    let schema_rec = encode_record(&[
        Value::Text("table".to_owned()),
        Value::Text("items".to_owned()),
        Value::Text("items".to_owned()),
        Value::Integer(i64::from(tbl_root)),
        Value::Text("CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB)".to_owned()),
    ]);
    bt.insert_table(1, 1, &schema_rec)
        .expect("insert sqlite_schema row");
    bt.pager_mut()
        .header_mut()
        .expect("header_mut")
        .schema_cookie = 1;
    tbl_root
}

fn find_items_root_in_btree(bt: &mut Btree) -> Pgno {
    let rows = bt.scan_table(1).expect("scan sqlite_schema");
    let mut decoded_debug = Vec::new();
    for (rowid, payload) in rows {
        let cols = decode_record(&payload).expect("decode sqlite_schema row");
        if cols.get(1).and_then(Value::as_str) == Some("items") {
            return cols[3].as_i64().expect("rootpage int") as Pgno;
        }
        decoded_debug.push((rowid, cols));
    }
    panic!("items table not found in sqlite_schema; rows={decoded_debug:?}");
}

// ============================================================================
// Out-of-Tree C SQLite 3.54.1 Reference Helper (`target/sqlite-ref/c_wal_protocol_helper`)
// ============================================================================

#[cfg(unix)]
const C_WAL_HELPER_SOURCE: &str = r#"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "sqlite3.h"

static void die_sql(const char *ctx, sqlite3 *db, int rc) {
    fprintf(stderr, "C_HELPER_ERR [%s] rc=%d ext=%d msg=%s\n",
            ctx, rc, db ? sqlite3_extended_errcode(db) : rc,
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
    if (argc < 2) return 2;
    const char *cmd = argv[1];

    if (strcmp(cmd, "version") == 0) {
        printf("VERSION=%s\n", sqlite3_libversion());
        printf("SOURCEID=%s\n", sqlite3_sourceid());
        fflush(stdout);
        return 0;
    }

    /* create-wal-clean <db_path> <page_size> <rows> */
    if (strcmp(cmd, "create-wal-clean") == 0) {
        if (argc < 5) return 2;
        const char *db_path = argv[2];
        int page_size = atoi(argv[3]);
        int rows = atoi(argv[4]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);

        char sql[256];
        snprintf(sql, sizeof(sql), "PRAGMA page_size = %d; PRAGMA journal_mode = WAL;", page_size);
        exec_checked(db, sql);
        exec_checked(db, "CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);");

        exec_checked(db, "BEGIN;");
        sqlite3_stmt *ins = NULL;
        rc = sqlite3_prepare_v2(db, "INSERT INTO items(id, val) VALUES (?1, ?2);", -1, &ins, NULL);
        if (rc != SQLITE_OK) die_sql("prepare", db, rc);
        for (int i = 1; i <= rows; i++) {
            char buf[64];
            int n = snprintf(buf, sizeof(buf), "c-row-%04d", i);
            sqlite3_bind_int(ins, 1, i);
            sqlite3_bind_blob(ins, 2, buf, n, SQLITE_TRANSIENT);
            rc = sqlite3_step(ins);
            if (rc != SQLITE_DONE) die_sql("step", db, rc);
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_checked(db, "COMMIT;");

        rc = sqlite3_close(db);
        if (rc != SQLITE_OK) die_sql("close", NULL, rc);
        printf("OK\n");
        fflush(stdout);
        return 0;
    }

    /* append-wal-crash <db_path> <page_size_if_new> <start_id> <count> <tag>
     * Commits rows into WAL with wal_autocheckpoint=0, then calls _exit(0) so -wal/-shm
     * remain uncheckpointed on disk! */
    if (strcmp(cmd, "append-wal-crash") == 0) {
        if (argc < 7) return 2;
        const char *db_path = argv[2];
        int page_size = atoi(argv[3]);
        int start_id = atoi(argv[4]);
        int count = atoi(argv[5]);
        const char *tag = argv[6];

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, 0);

        if (page_size > 0) {
            char sql[256];
            snprintf(sql, sizeof(sql), "PRAGMA page_size = %d; PRAGMA journal_mode = WAL;", page_size);
            exec_checked(db, sql);
        }
        exec_checked(db, "PRAGMA wal_autocheckpoint = 0;");
        exec_checked(db, "CREATE TABLE IF NOT EXISTS items(id INTEGER PRIMARY KEY, val BLOB);");

        exec_checked(db, "BEGIN;");
        sqlite3_stmt *ins = NULL;
        rc = sqlite3_prepare_v2(db, "INSERT OR REPLACE INTO items(id, val) VALUES (?1, ?2);", -1, &ins, NULL);
        if (rc != SQLITE_OK) die_sql("prepare", db, rc);
        for (int i = 0; i < count; i++) {
            char buf[128];
            int id = start_id + i;
            int n = snprintf(buf, sizeof(buf), "%s-%05d", tag, id);
            sqlite3_bind_int(ins, 1, id);
            sqlite3_bind_blob(ins, 2, buf, n, SQLITE_TRANSIENT);
            rc = sqlite3_step(ins);
            if (rc != SQLITE_DONE) die_sql("step", db, rc);
            sqlite3_reset(ins);
        }
        sqlite3_finalize(ins);
        exec_checked(db, "COMMIT;");
        printf("COMMITTED\n");
        fflush(stdout);
        _exit(0);
    }

    /* verify-integrity-and-count <db_path> <leave_wal_alive:0|1> */
    if (strcmp(cmd, "verify-integrity-and-count") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        int leave_alive = atoi(argv[3]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, 0);
        exec_checked(db, "PRAGMA wal_autocheckpoint = 0;");

        sqlite3_stmt *ic = NULL;
        rc = sqlite3_prepare_v2(db, "PRAGMA integrity_check;", -1, &ic, NULL);
        if (rc != SQLITE_OK) die_sql("prepare ic", db, rc);
        if (sqlite3_step(ic) != SQLITE_ROW) die_sql("step ic", db, rc);
        const unsigned char *ic_txt = sqlite3_column_text(ic, 0);
        printf("INTEGRITY=%s\n", ic_txt ? (const char *)ic_txt : "NULL");
        sqlite3_finalize(ic);

        sqlite3_stmt *cnt = NULL;
        rc = sqlite3_prepare_v2(db, "SELECT count(*) FROM items;", -1, &cnt, NULL);
        if (rc == SQLITE_OK) {
            if (sqlite3_step(cnt) == SQLITE_ROW) {
                printf("COUNT=%d\n", sqlite3_column_int(cnt, 0));
            }
            sqlite3_finalize(cnt);
        } else {
            printf("COUNT=-1\n");
        }
        fflush(stdout);

        if (leave_alive) {
            _exit(0);
        }
        sqlite3_close(db);
        return 0;
    }

    /* checkpoint <db_path> <passive|full|restart|truncate> */
    if (strcmp(cmd, "checkpoint") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        const char *mode_str = argv[3];

        int mode = SQLITE_CHECKPOINT_PASSIVE;
        if (strcmp(mode_str, "full") == 0) mode = SQLITE_CHECKPOINT_FULL;
        else if (strcmp(mode_str, "restart") == 0) mode = SQLITE_CHECKPOINT_RESTART;
        else if (strcmp(mode_str, "truncate") == 0) mode = SQLITE_CHECKPOINT_TRUNCATE;

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, 0);
        sqlite3_extended_result_codes(db, 1);

        /* Trigger WAL open via schema read first */
        exec_checked(db, "SELECT count(*) FROM sqlite_master;");

        int n_log = -99, n_ckpt = -99;
        rc = sqlite3_wal_checkpoint_v2(db, "main", mode, &n_log, &n_ckpt);
        int ext = (rc == SQLITE_OK) ? SQLITE_OK : sqlite3_extended_errcode(db);
        printf("RC=%d EXT=%d LOG=%d CKPT=%d\n", rc, ext, n_log, n_ckpt);
        fflush(stdout);
        _exit(0);
    }

    /* hold-wal-reader <db_path> */
    if (strcmp(cmd, "hold-wal-reader") == 0) {
        if (argc < 3) return 2;
        const char *db_path = argv[2];
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, 0);
        exec_checked(db, "BEGIN;");
        sqlite3_stmt *st = NULL;
        rc = sqlite3_prepare_v2(db, "SELECT count(*) FROM sqlite_master;", -1, &st, NULL);
        if (rc != SQLITE_OK) die_sql("prepare", db, rc);
        if (sqlite3_step(st) != SQLITE_ROW) die_sql("step", db, rc);
        sqlite3_finalize(st);
        printf("READY\n");
        fflush(stdout);
        while (fgetc(stdin) != EOF) {}
        sqlite3_exec(db, "COMMIT;", NULL, NULL, NULL);
        sqlite3_close(db);
        return 0;
    }

    fprintf(stderr, "unknown subcommand: %s\n", cmd);
    return 2;
}
"#;

#[cfg(unix)]
fn c_wal_helper() -> &'static Path {
    static HELPER_BIN: OnceLock<PathBuf> = OnceLock::new();
    HELPER_BIN.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

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

        let helper_c = ref_root.join("c_wal_protocol_helper.c");
        let helper_bin = ref_root.join("c_wal_protocol_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_WAL_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            fs::write(&helper_c, C_WAL_HELPER_SOURCE).expect("write c_wal_protocol_helper.c");
            let cc_bin = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&helper_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&helper_bin)
                .output()
                .expect("compile c_wal_protocol_helper");
            assert!(
                cc_bin.status.success(),
                "failed compiling c_wal_protocol_helper:\n{}",
                String::from_utf8_lossy(&cc_bin.stderr)
            );
        }

        let after_status = Command::new("git")
            .arg("-C")
            .arg(workspace_root)
            .args(["status", "--porcelain", "--", "sqlite"])
            .output()
            .expect("git status sqlite after build");
        assert!(
            after_status.status.success() && after_status.stdout.is_empty(),
            "./sqlite must remain untouched!"
        );

        let ver_out = Command::new(&helper_bin)
            .arg("version")
            .output()
            .expect("run c_wal_protocol_helper version");
        assert!(ver_out.status.success());
        let ver_str = String::from_utf8_lossy(&ver_out.stdout);
        assert!(
            ver_str.contains("VERSION=3.54.1"),
            "unexpected C SQLite version: {ver_str}"
        );

        helper_bin
    })
}

#[cfg(unix)]
fn spawn_c_wal_reader(db_path: &Path) -> Child {
    let mut child = Command::new(c_wal_helper())
        .arg("hold-wal-reader")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn hold-wal-reader");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read READY");
    assert_eq!(line.trim(), "READY");
    child.stdout = Some(reader.into_inner());
    child
}

#[cfg(unix)]
fn release_c_child(mut child: Child) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"\n");
    }
    let st = child.wait().expect("wait c child");
    assert!(st.success());
}

// ============================================================================
// Fault-Injecting / Spy VFS Wrappers (`#![forbid(unsafe_code)]`)
// ============================================================================

struct FaultWalVfs {
    inner: OsVfs,
    fail_wal_exists: AtomicBool,
    fail_wal_delete: AtomicBool,
    fail_wal_truncate: AtomicBool,
    journal_writes: AtomicUsize,
    journal_syncs: AtomicUsize,
}

impl FaultWalVfs {
    fn new() -> Self {
        Self {
            inner: OsVfs::new(),
            fail_wal_exists: AtomicBool::new(false),
            fail_wal_delete: AtomicBool::new(false),
            fail_wal_truncate: AtomicBool::new(false),
            journal_writes: AtomicUsize::new(0),
            journal_syncs: AtomicUsize::new(0),
        }
    }
}

struct FaultWalFile {
    inner: Box<dyn VfsFile>,
    is_wal: bool,
    is_journal: bool,
    fail_wal_truncate: Arc<FaultWalVfs>,
}

impl VfsFile for FaultWalFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if self.is_journal {
            self.fail_wal_truncate
                .journal_writes
                .fetch_add(1, Ordering::SeqCst);
        }
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if self.is_wal
            && size == 0
            && self
                .fail_wal_truncate
                .fail_wal_truncate
                .load(Ordering::SeqCst)
        {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_TRUNCATE,
                "injected WAL truncate(0) failure",
            ));
        }
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        if self.is_journal {
            self.fail_wal_truncate
                .journal_syncs
                .fetch_add(1, Ordering::SeqCst);
        }
        self.inner.sync(flags)
    }

    fn file_size(&self) -> Result<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&self) -> Result<bool> {
        self.inner.check_reserved_lock()
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        self.inner.lock_state_for_diagnostics()
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }

    fn open_wal_shm_with_options(&self, options: ShmOpenOptions) -> Result<sqliters::vfs::WalShm> {
        self.inner.open_wal_shm_with_options(options)
    }

    fn supports_wal_shm(&self) -> bool {
        self.inner.supports_wal_shm()
    }

    fn device_characteristics(&self) -> u32 {
        self.inner.device_characteristics()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }
}

impl Vfs for FaultWalVfs {
    fn name(&self) -> &str {
        "fault-wal-os"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let f = self.inner.open(path, flags)?;
        Ok(Box::new(FaultWalFile {
            inner: f,
            is_wal: flags.contains(OpenFlags::WAL),
            is_journal: flags.contains(OpenFlags::MAIN_JOURNAL),
            fail_wal_truncate: Arc::new(Self {
                inner: OsVfs::new(),
                fail_wal_exists: AtomicBool::new(false),
                fail_wal_delete: AtomicBool::new(false),
                fail_wal_truncate: AtomicBool::new(self.fail_wal_truncate.load(Ordering::SeqCst)),
                journal_writes: AtomicUsize::new(0),
                journal_syncs: AtomicUsize::new(0),
            }),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        if path.to_string_lossy().ends_with("-wal") && self.fail_wal_delete.load(Ordering::SeqCst) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                "injected orphan WAL delete failure",
            ));
        }
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        if path.to_string_lossy().ends_with("-wal") && self.fail_wal_exists.load(Ordering::SeqCst) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_ACCESS,
                "injected WAL xAccess failure",
            ));
        }
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        self.inner.full_pathname(path)
    }
}

/// Shared-counter VFS wrapper so `FaultWalFile` increments the parent `Arc<SharedSpyState>`.
#[derive(Default)]
struct SharedSpyState {
    fail_wal_truncate: AtomicBool,
    journal_writes: AtomicUsize,
    journal_syncs: AtomicUsize,
}

struct SpyWalVfs {
    inner: OsVfs,
    state: Arc<SharedSpyState>,
}

impl SpyWalVfs {
    fn new() -> (Arc<Self>, Arc<SharedSpyState>) {
        let state = Arc::new(SharedSpyState::default());
        let vfs = Arc::new(Self {
            inner: OsVfs::new(),
            state: Arc::clone(&state),
        });
        (vfs, state)
    }
}

struct SpyWalFile {
    inner: Box<dyn VfsFile>,
    is_wal: bool,
    is_journal: bool,
    state: Arc<SharedSpyState>,
}

impl VfsFile for SpyWalFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if self.is_journal {
            self.state.journal_writes.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if self.is_wal && size == 0 && self.state.fail_wal_truncate.load(Ordering::SeqCst) {
            return Err(SqliteError::with_extended(
                extended::SQLITE_IOERR_TRUNCATE,
                "injected WAL truncate(0) error",
            ));
        }
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        if self.is_journal {
            self.state.journal_syncs.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.sync(flags)
    }

    fn file_size(&self) -> Result<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> Result<()> {
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> Result<()> {
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&self) -> Result<bool> {
        self.inner.check_reserved_lock()
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn lock_state_for_diagnostics(&self) -> Result<Option<LockLevel>> {
        self.inner.lock_state_for_diagnostics()
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }

    fn open_wal_shm_with_options(&self, options: ShmOpenOptions) -> Result<sqliters::vfs::WalShm> {
        self.inner.open_wal_shm_with_options(options)
    }

    fn supports_wal_shm(&self) -> bool {
        self.inner.supports_wal_shm()
    }

    fn device_characteristics(&self) -> u32 {
        self.inner.device_characteristics()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }
}

impl Vfs for SpyWalVfs {
    fn name(&self) -> &str {
        "spy-wal-os"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let f = self.inner.open(path, flags)?;
        Ok(Box::new(SpyWalFile {
            inner: f,
            is_wal: flags.contains(OpenFlags::WAL),
            is_journal: flags.contains(OpenFlags::MAIN_JOURNAL),
            state: Arc::clone(&self.state),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        self.inner.full_pathname(path)
    }
}

// ============================================================================
// Suite 1: Format 3 Header & Two-Phase WAL Discovery (Groups 35 & 62)
// ============================================================================

#[test]
fn test_suite1_format3_header_versions_and_two_phase_wal_discovery() {
    let sandbox = TempSandbox::new("suite1_discovery");

    // 1A. Format 3 version matrix: `(1,1)`, `(2,2)`, `read_version > 2 -> NotADb`, `write_version > 2 -> readonly`!
    let base_db = sandbox.path().join("ver_matrix.db");
    {
        let mut p = Pager::open_file(
            &base_db,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        p.begin_write().unwrap();
        let p2 = p.allocate_raw_page().unwrap();
        p.write_page(p2).unwrap()[0..8].copy_from_slice(b"VER_DATA");
        p.commit().unwrap();
        p.close().unwrap();
    }

    // Patch bytes 18..19 to `(0, 1)` and `(1, 3)` -> `NotADb`!
    for &(wv, rv) in &[(0u8, 1u8), (1, 0), (1, 3), (3, 3)] {
        let p_bad_path = sandbox.path().join(format!("bad_{wv}_{rv}.db"));
        let mut raw = fs::read(&base_db).unwrap();
        raw[18] = wv;
        raw[19] = rv;
        fs::write(&p_bad_path, raw).unwrap();

        let mut p =
            Pager::open_file(&p_bad_path, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        assert_eq!(p.begin_read().unwrap_err().code, ErrorCode::NotADb);
        p.close().unwrap();
    }

    // Patch bytes 18..19 to `(3, 1)` -> `write_version > 2` with `read_version == 1` parses validly,
    // reads succeed, `is_readonly() == true`, and `begin_write()` returns `ErrorCode::ReadOnly`!
    let ro_ver_db = sandbox.path().join("future_write_ver.db");
    {
        let mut raw = fs::read(&base_db).unwrap();
        raw[18] = 3;
        raw[19] = 1;
        fs::write(&ro_ver_db, raw).unwrap();

        let mut p =
            Pager::open_file(&ro_ver_db, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        assert!(!p.is_readonly());
        p.begin_read().unwrap();
        assert!(p.is_readonly());
        assert_eq!(&p.read_page(2).unwrap()[0..8], b"VER_DATA");
        assert_eq!(p.begin_write().unwrap_err().code, ErrorCode::ReadOnly);
        p.close().unwrap();
    }

    // 1B. Empty 0-byte `.db` + orphan `<db>-wal`: deletes orphan `<db>-wal` on `begin_read()`,
    // AND propagates I/O errors on `exists` / `delete` while unlocking `.db` back to `NoLock`!
    let orphan_db = sandbox.path().join("orphan.db");
    let orphan_wal = sandbox.path().join("orphan.db-wal");
    fs::write(&orphan_db, b"").unwrap();
    fs::write(&orphan_wal, b"stale-orphan-wal-bytes").unwrap();

    let fault_vfs = Arc::new(FaultWalVfs::new());
    fault_vfs.fail_wal_exists.store(true, Ordering::SeqCst);
    {
        let mut p = Pager::open_with_options(
            fault_vfs.clone(),
            Some(&orphan_db),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions::default(),
        )
        .unwrap();
        let err = p.begin_read().unwrap_err();
        assert_eq!(err.extended_code, extended::SQLITE_IOERR_ACCESS);
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );
        p.close().unwrap();
    }
    fault_vfs.fail_wal_exists.store(false, Ordering::SeqCst);
    fault_vfs.fail_wal_delete.store(true, Ordering::SeqCst);
    {
        let mut p = Pager::open_with_options(
            fault_vfs.clone(),
            Some(&orphan_db),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions::default(),
        )
        .unwrap();
        let err = p.begin_read().unwrap_err();
        assert_eq!(err.extended_code, extended::SQLITE_IOERR_DELETE);
        assert!(orphan_wal.exists());
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );
        p.close().unwrap();
    }
    fault_vfs.fail_wal_delete.store(false, Ordering::SeqCst);
    {
        let mut p = Pager::open_with_options(
            fault_vfs,
            Some(&orphan_db),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions::default(),
        )
        .unwrap();
        p.begin_read().unwrap();
        assert!(
            !orphan_wal.exists(),
            "orphan -wal must be deleted on 0-byte DB"
        );
        assert_eq!(p.journal_mode(), JournalMode::Delete);
        assert_eq!(p.page_count(), 0);
        p.close().unwrap();
    }

    // 1C. Pass 1 discovery: non-empty `.db` (with older Page 1 on `.db` disk) + live `<db>-wal`
    // containing updated Page 1 (`user_version = 77`, `page_count = 3`) -> fresh `Pager` opens WAL
    // BEFORE reading Page 1 and immediately sees `user_version == 77` and `page_count == 3`!
    let pass1_db = sandbox.path().join("pass1.db");
    let pass1_wal = sandbox.path().join("pass1.db-wal");
    let mut keep_alive = Pager::open_file(
        &pass1_db,
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    keep_alive.set_journal_mode(JournalMode::Wal).unwrap();
    keep_alive.begin_write().unwrap();
    keep_alive.header_mut().unwrap().user_version = 10;
    keep_alive.commit().unwrap();
    keep_alive
        .wal_checkpoint(WalCheckpointMode::Full, None, None)
        .unwrap();

    // Now `.db` on disk has `user_version = 10` and `1` page. Write a WAL commit updating Page 1
    // to `user_version = 77` and `3` pages WITHOUT checkpointing!
    keep_alive.begin_write().unwrap();
    keep_alive.header_mut().unwrap().user_version = 77;
    let _ = keep_alive.allocate_raw_page().unwrap();
    let p3 = keep_alive.allocate_raw_page().unwrap();
    keep_alive.write_page(p3).unwrap()[0..9].copy_from_slice(b"PASS1_PG3");
    keep_alive.commit().unwrap();
    assert!(pass1_wal.exists());

    // Open a brand-new `Pager` and verify Pass 1 sees `user_version == 77` & `page_count == 3`!
    let mut reader =
        Pager::open_file(&pass1_db, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
    reader.begin_read().unwrap();
    assert_eq!(reader.journal_mode(), JournalMode::Wal);
    assert_eq!(reader.header().user_version, 77);
    assert_eq!(reader.page_count(), 3);
    assert_eq!(&reader.read_page(3).unwrap()[0..9], b"PASS1_PG3");
    reader.close().unwrap();
    keep_alive.close().unwrap();
    assert!(
        !pass1_wal.exists(),
        "last connection close must unlink -wal"
    );

    // 1D. Pass 2 discovery after clean C SQLite 3.54.1 close (`(2, 2)` on `.db`, no `-wal`/`-shm` on disk)
    // + `default_wal_synchronous` vs explicit `set_synchronous` override!
    #[cfg(unix)]
    {
        let c_db = sandbox.path().join("c_clean_wal.db");
        let c_wal = sandbox.path().join("c_clean_wal.db-wal");
        let c_shm = sandbox.path().join("c_clean_wal.db-shm");
        let c_jrn = sandbox.path().join("c_clean_wal.db-journal");

        let out = Command::new(c_wal_helper())
            .arg("create-wal-clean")
            .arg(&c_db)
            .arg("4096")
            .arg("25")
            .output()
            .expect("run create-wal-clean");
        assert!(out.status.success());
        assert!(!c_wal.exists() && !c_shm.exists());

        // Case i: default `PagerOpenOptions::default()` has `default_wal_synchronous == 2`,
        // AND custom `default_wal_synchronous: 1` switches `synchronous()` from `2 -> 1` when
        // user did NOT call `set_synchronous`!
        assert_eq!(PagerOpenOptions::default().default_wal_synchronous, 2);
        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let mut p_auto_sync = Pager::open_with_options(
            os_vfs.clone(),
            Some(&c_db),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions {
                default_wal_synchronous: 1,
                ..PagerOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(p_auto_sync.synchronous(), 2);
        p_auto_sync.begin_read().unwrap();
        assert_eq!(p_auto_sync.journal_mode(), JournalMode::Wal);
        assert_eq!(p_auto_sync.synchronous(), 1);
        p_auto_sync.close().unwrap();

        // Case ii: user called `set_synchronous(3)` BEFORE `begin_read()` -> user setting `3` is preserved!
        let mut bt = Btree::open_with_options(
            os_vfs,
            Some(&c_db),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions {
                default_wal_synchronous: 1,
                ..PagerOpenOptions::default()
            },
        )
        .unwrap();
        bt.set_synchronous(3);
        bt.begin_write().unwrap();
        assert_eq!(bt.journal_mode(), JournalMode::Wal);
        assert_eq!(bt.synchronous(), 3);
        let root = find_items_root_in_btree(&mut bt);
        let rec = encode_record(&[Value::Integer(26), Value::Blob(b"rust-26".to_vec())]);
        bt.insert_table(root, 26, &rec).unwrap();
        bt.commit().unwrap();
        assert!(c_wal.exists());
        assert!(
            !c_jrn.exists(),
            "reopened (2,2) DB must NEVER fall back to rollback journal"
        );
        bt.close().unwrap();

        let ver = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&c_db)
            .arg("0")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&ver.stdout);
        assert!(txt.contains("INTEGRITY=ok"), "got: {txt}");
        assert!(txt.contains("COUNT=26"), "got: {txt}");
    }
}

// ============================================================================
// Suite 2: `READ(0) -> useWal=1 -> READ(1..=4)`, All 3 `walRestartLog` Paths,
//          `Write -> Read` Positive Slot Retention & `SQLITE_BUSY_SNAPSHOT` (Group 24)
// ============================================================================

#[test]
fn test_suite2_read0_to_positive_slot_all_three_restart_paths_and_busy_snapshot() {
    let sandbox = TempSandbox::new("suite2_group24");
    let db_path = sandbox.path().join("g24.db");

    // ---- Path C (`nBackfill == 0`, empty WAL): first transaction after `set_journal_mode(Wal)` ----
    let mut writer = Pager::open_file(
        &db_path,
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    writer.begin_write().unwrap();
    let p2 = writer.allocate_raw_page().unwrap();
    writer.write_page(p2).unwrap()[0..8].copy_from_slice(b"INIT_DB_");
    writer.commit().unwrap();
    writer.set_journal_mode(JournalMode::Wal).unwrap();

    // Empty WAL (`mxFrame == 0, nBackfill == 0`) -> `begin_read()` takes `READ(0)`!
    writer.begin_read().unwrap();
    assert_eq!(writer.wal().unwrap().read_lock_slot(), Some(0));
    assert_eq!(writer.wal().unwrap().n_ckpt(), 0);

    // Upgrade to write, dirty Page 2 BEFORE any WAL frames are written, then `commit_retaining_read(true)`!
    writer.begin_write().unwrap();
    writer.write_page(2).unwrap()[0..8].copy_from_slice(b"PATH_C_1");
    writer.commit_retaining_read(true).unwrap();

    assert_eq!(writer.transaction_state(), TransactionState::Read);
    let slot_c = writer.wal().unwrap().read_lock_slot().unwrap();
    assert!(
        (1..=4).contains(&slot_c),
        "expected positive READ(1..=4) slot after Path C commit_retaining_read(true), got {slot_c}"
    );
    assert_eq!(writer.wal().unwrap().n_ckpt(), 0);
    assert_eq!(&writer.read_page(2).unwrap()[0..8], b"PATH_C_1");
    writer.end_read_if_unused().unwrap();

    // ---- Path A (`nBackfill > 0`, reset SUCCEEDS): full checkpoint -> `READ(0)` -> wrap WAL ----
    let ckpt_full = writer
        .wal_checkpoint(WalCheckpointMode::Full, None, None)
        .unwrap();
    assert!(!ckpt_full.busy && ckpt_full.n_log > 0 && ckpt_full.n_ckpt == ckpt_full.n_log);

    writer.begin_read().unwrap();
    assert_eq!(writer.wal().unwrap().read_lock_slot(), Some(0));

    writer.begin_write().unwrap();
    // Dirty Page 2 in cache before `walRestartLog` runs (`use_wal = true` must NOT wipe dirty cache!).
    writer.write_page(2).unwrap()[0..8].copy_from_slice(b"PATH_A_2");
    writer.commit_retaining_read(true).unwrap();

    let slot_a = writer.wal().unwrap().read_lock_slot().unwrap();
    assert!((1..=4).contains(&slot_a));
    assert_eq!(
        writer.wal().unwrap().n_ckpt(),
        1,
        "Path A must wrap WAL and increment nCkpt to 1"
    );
    assert_eq!(writer.wal().unwrap().header().mx_frame, 2); // Page 1 + Page 2
    assert_eq!(&writer.read_page(2).unwrap()[0..8], b"PATH_A_2");

    // Verify retained positive `READ(1..=4)` slot blocks external C SQLite `RESTART` / `TRUNCATE`!
    #[cfg(unix)]
    {
        let c_out = Command::new(c_wal_helper())
            .arg("checkpoint")
            .arg(&db_path)
            .arg("restart")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&c_out.stdout);
        assert!(
            txt.contains("RC=5 "),
            "retained positive reader slot must block C SQLite RESTART with SQLITE_BUSY (5), got: {txt}"
        );
    }

    // Also verify another Rust reader sees `PATH_A_2` from the wrapped WAL!
    let mut reader_r1 =
        Pager::open_file(&db_path, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
    reader_r1.begin_read().unwrap();
    assert_eq!(&reader_r1.read_page(2).unwrap()[0..8], b"PATH_A_2");
    let r1_slot = reader_r1.wal().unwrap().read_lock_slot().unwrap();
    assert!((1..=4).contains(&r1_slot));

    // Release `writer`'s read snapshot while `reader_r1` STILL holds `SHARED WAL_READ_LOCK(r1_slot)`
    // with `aReadMark[r1_slot] == 2 == mxFrame`!
    writer.end_read_if_unused().unwrap();

    // ---- Path B (`nBackfill > 0`, reset hits `Busy` from `reader_r1` holding `READ(1..=4)`) ----
    // Run `PASSIVE` checkpoint: because `aReadMark[r1_slot] == 2 == mxFrame`, `PASSIVE` backfills
    // all 2 frames (`nBackfill == 2 == mxFrame`), while `reader_r1` STILL holds `READ(r1_slot)`!
    let ckpt_p = writer
        .wal_checkpoint(WalCheckpointMode::Passive, None, None)
        .unwrap();
    assert!(!ckpt_p.busy && ckpt_p.n_ckpt == 2 && ckpt_p.n_log == 2);

    // `writer` begins read: `nBackfill == mxFrame == 2`, so `writer` takes `READ(0)`!
    writer.begin_read().unwrap();
    assert_eq!(writer.wal().unwrap().read_lock_slot(), Some(0));

    // `writer` upgrades to write, dirties Page 2 (`PATH_B_3`), and commits with `retain_read = true`:
    // `walRestartLog` attempts `EXCLUSIVE WAL_READ_LOCK(1..=4)`, hits `Busy` on `r1_slot`, skips reset
    // (`n_ckpt` stays `1`, frames append to `3..=4`), preserves dirty Page 2, and acquires a positive slot!
    writer.begin_write().unwrap();
    writer.write_page(2).unwrap()[0..8].copy_from_slice(b"PATH_B_3");
    writer.commit_retaining_read(true).unwrap();

    let slot_b = writer.wal().unwrap().read_lock_slot().unwrap();
    assert!((1..=4).contains(&slot_b));
    assert_eq!(
        writer.wal().unwrap().n_ckpt(),
        1,
        "Path B must NOT reset nCkpt while reader_r1 holds READ(1..=4)"
    );
    assert_eq!(writer.wal().unwrap().header().mx_frame, 4);
    assert_eq!(&writer.read_page(2).unwrap()[0..8], b"PATH_B_3");

    // Meanwhile `reader_r1` is isolated on its snapshot (`mx_frame == 2`) and still sees `PATH_A_2`,
    // and attempting to upgrade `reader_r1` to a writer MUST fail with `SQLITE_BUSY_SNAPSHOT`!
    reader_r1.invalidate_clean_cache_for_test_or_recheck();
    assert_eq!(&reader_r1.read_page(2).unwrap()[0..8], b"PATH_A_2");
    let snap_err = reader_r1.begin_write().unwrap_err();
    assert_eq!(snap_err.code, ErrorCode::Busy);
    assert_eq!(snap_err.extended_code, extended::SQLITE_BUSY_SNAPSHOT);
    assert_eq!(reader_r1.transaction_state(), TransactionState::Read);
    assert_eq!(&reader_r1.read_page(2).unwrap()[0..8], b"PATH_A_2");

    reader_r1.close().unwrap();
    writer.close().unwrap();
}

trait CacheTestExt {
    fn invalidate_clean_cache_for_test_or_recheck(&mut self);
}
impl CacheTestExt for Pager {
    fn invalidate_clean_cache_for_test_or_recheck(&mut self) {
        // `read_page` serves from snapshot; `begin_write` checks `WalIndexHdr` directly.
    }
}

// ============================================================================
// Suite 3: 4-Word WAL Savepoints, Spill, `iReCksum` Overwrite & Log Wrap (Groups 55 & 61)
// ============================================================================

#[test]
fn test_suite3_wal_savepoints_spill_irecksum_overwrite_and_log_wrap() {
    let sandbox = TempSandbox::new("suite3_savepoints");
    let db_path = sandbox.path().join("sp_wal.db");

    let mut bt = Btree::open_file(
        &db_path,
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    bt.pager_mut().set_page_size(1024).unwrap();
    bt.set_journal_mode(JournalMode::Wal).unwrap();

    bt.begin_write().unwrap();
    let tbl_root = init_items_schema_in_btree(&mut bt);
    for i in 1..=10i64 {
        let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![b'A'; 120])]);
        bt.insert_table(tbl_root, i, &rec).unwrap();
    }
    bt.commit().unwrap();

    // 3A. Mid-tx spill + overwriting an already-spilled page (`iReCksum` checksum rewrite) + nested savepoint undo!
    bt.begin_write().unwrap();
    // Spill initial modification to row 1 (`V1`)
    bt.delete_table(tbl_root, 1).unwrap();
    bt.insert_table(
        tbl_root,
        1,
        &encode_record(&[Value::Integer(1), Value::Blob(b"ROW1_V1".to_vec())]),
    )
    .unwrap();
    let spilled1 = bt.spill_dirty_pages_to_wal().unwrap();
    assert!(spilled1 > 0);

    // Overwrite row 1 AGAIN (`V2`) on the same leaf page and spill -> triggers in-place frame overwrite (`i_re_cksum > 0`)!
    bt.delete_table(tbl_root, 1).unwrap();
    bt.insert_table(
        tbl_root,
        1,
        &encode_record(&[Value::Integer(1), Value::Blob(b"ROW1_V2_FINAL".to_vec())]),
    )
    .unwrap();
    let spilled2 = bt.spill_dirty_pages_to_wal().unwrap();
    assert!(spilled2 > 0);

    // Open nested savepoint 0 & savepoint 1, insert rows, spill to WAL, then `ROLLBACK TO` savepoint 0!
    bt.pager_mut().ensure_subsavepoints(1).unwrap();
    for i in 11..=30i64 {
        let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![b'X'; 200])]);
        bt.insert_table(tbl_root, i, &rec).unwrap();
    }
    assert!(bt.spill_dirty_pages_to_wal().unwrap() > 0);

    bt.pager_mut().ensure_subsavepoints(2).unwrap();
    for i in 31..=50i64 {
        let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![b'Y'; 200])]);
        bt.insert_table(tbl_root, i, &rec).unwrap();
    }
    assert!(bt.spill_dirty_pages_to_wal().unwrap() > 0);

    // Roll back inner savepoint 1, then outer savepoint 0: rows 11..=50 (and their spilled WAL frames!) must vanish!
    bt.pager_mut().rollback_to_subsavepoint(1).unwrap();
    assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 30);
    bt.pager_mut().rollback_to_subsavepoint(0).unwrap();
    assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 10);
    bt.pager_mut().release_subsavepoint(0).unwrap();

    // Commit (`walRewriteChecksums` rewrites cumulative checksums from `i_re_cksum` through commit frame!).
    bt.commit().unwrap();

    // Verify C SQLite 3.54.1 recovers/verifies every checksum in `-wal` (by zeroing `-shm` first!)
    // and passes `PRAGMA integrity_check`!
    #[cfg(unix)]
    {
        let shm_path = sandbox.path().join("sp_wal.db-shm");
        if shm_path.exists() {
            fs::write(&shm_path, vec![0u8; 32768]).unwrap();
        }
        let ver = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&db_path)
            .arg("1")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&ver.stdout);
        assert!(
            txt.contains("INTEGRITY=ok"),
            "iReCksum WAL failed C recovery: {txt}"
        );
        assert!(txt.contains("COUNT=10"), "got: {txt}");
    }

    // 3B. `ROLLBACK TO` across `walRestartLog` log wrap (`token[3] != n_ckpt` normalizing `token[0] = 0`)!
    let ckpt = bt.wal_checkpoint(WalCheckpointMode::Full, None).unwrap();
    assert!(!ckpt.busy && ckpt.n_ckpt == ckpt.n_log);

    bt.begin_write().unwrap();
    assert_eq!(bt.pager().wal().unwrap().read_lock_slot(), Some(0));
    let n_ckpt_before = bt.pager().wal().unwrap().n_ckpt();

    // Modify row 2 BEFORE opening savepoint 0 (kept in cache, not spilled yet!)
    bt.delete_table(tbl_root, 2).unwrap();
    bt.insert_table(
        tbl_root,
        2,
        &encode_record(&[Value::Integer(2), Value::Blob(b"PRE_WRAP_SP".to_vec())]),
    )
    .unwrap();

    // Record savepoint 0 BEFORE log wrap (`token[3] == n_ckpt_before`, `token[0] == old mxFrame > 0`)!
    bt.pager_mut().ensure_subsavepoints(1).unwrap();

    // Modify rows inside savepoint 0 and SPILL -> triggers `walRestartLog` (`n_ckpt` increments!)
    for i in 100..=115i64 {
        bt.insert_table(
            tbl_root,
            i,
            &encode_record(&[Value::Integer(i), Value::Blob(vec![b'W'; 180])]),
        )
        .unwrap();
    }
    assert!(bt.spill_dirty_pages_to_wal().unwrap() > 0);
    assert_eq!(bt.pager().wal().unwrap().n_ckpt(), n_ckpt_before + 1);

    // `ROLLBACK TO` savepoint 0 across the log wrap!
    bt.pager_mut().rollback_to_subsavepoint(0).unwrap();
    bt.pager_mut().release_subsavepoint(0).unwrap();
    bt.commit().unwrap();

    bt.begin_read().unwrap();
    assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 10);
    let r2 = decode_record(&bt.seek_table_rowid(tbl_root, 2).unwrap().unwrap()).unwrap();
    assert_eq!(r2[1], Value::Blob(b"PRE_WRAP_SP".to_vec()));
    assert_eq!(
        bt.integrity_check(&[1, tbl_root]).unwrap(),
        Vec::<String>::new()
    );

    // Also verify full transaction `rollback()` (`wal.undo`) discards spilled frames while keeping committed rows!
    bt.end_read_if_unused_via_pager();
    bt.begin_write().unwrap();
    for i in 200..=220i64 {
        bt.insert_table(
            tbl_root,
            i,
            &encode_record(&[Value::Integer(i), Value::Blob(vec![b'Z'; 180])]),
        )
        .unwrap();
    }
    assert!(bt.spill_dirty_pages_to_wal().unwrap() > 0);
    bt.rollback().unwrap();

    bt.begin_read().unwrap();
    assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 10);
    bt.close().unwrap();

    #[cfg(unix)]
    {
        let ver = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&db_path)
            .arg("0")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&ver.stdout);
        assert!(
            txt.contains("INTEGRITY=ok") && txt.contains("COUNT=10"),
            "{txt}"
        );
    }
}

trait BtreeTestExt {
    fn end_read_if_unused_via_pager(&mut self);
}
impl BtreeTestExt for Btree {
    fn end_read_if_unused_via_pager(&mut self) {
        self.pager_mut().end_read_if_unused().unwrap();
    }
}

// ============================================================================
// Suite 4: WAL-Index Growth Across Block Boundaries (`4062/4063`, `8158/8159`, `12254/12255`)
//          & Bidirectional C SQLite 3.54.1 <-> Rust Across Page Sizes Including 64 KiB (Groups 5 & 48)
// ============================================================================

#[test]
fn test_suite4_wal_index_block_boundaries_12255_frames_and_64k_bidirectional_c_interop() {
    let sandbox = TempSandbox::new("suite4_blocks_64k");

    // 4A. Commit across `4062/4063`, `8158/8159`, and `12254/12255` WAL frames (4 SHM 32 KiB regions = 131072 bytes)!
    let big_db = sandbox.path().join("blocks_12k.db");
    let big_shm = sandbox.path().join("blocks_12k.db-shm");
    let mut bt_big = Btree::open_file(
        &big_db,
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    bt_big.pager_mut().set_page_size(512).unwrap();
    bt_big.set_synchronous(1); // NORMAL in WAL -> fast commits without per-commit fsync
    bt_big.set_journal_mode(JournalMode::Wal).unwrap();

    bt_big.begin_write().unwrap();
    let big_root = init_items_schema_in_btree(&mut bt_big);
    for id in 1..=20i64 {
        let val = format!("INIT-{id:02}-{}", "x".repeat(96)).into_bytes();
        bt_big
            .insert_table(
                big_root,
                id,
                &encode_record(&[Value::Integer(id), Value::Blob(val)]),
            )
            .unwrap();
    }
    bt_big.commit().unwrap();

    // Append B-tree transactions until `mx_frame >= 12_260`, crossing 4062->4063, 8158->8159, and 12254->12255!
    let mut seq: u32 = 1;
    while bt_big.pager().wal().map_or(0, |w| w.header().mx_frame) < 12_260 {
        bt_big.begin_write().unwrap();
        for id in [1i64, 5, 10, 15, 20] {
            bt_big.delete_table(big_root, id).unwrap();
            let val = format!("SEQ-{seq:06}-ID-{id:02}-{}", "y".repeat(80)).into_bytes();
            bt_big
                .insert_table(
                    big_root,
                    id,
                    &encode_record(&[Value::Integer(id), Value::Blob(val)]),
                )
                .unwrap();
        }
        bt_big.commit().unwrap();
        seq += 1;
    }
    let final_mx = bt_big.pager().wal().unwrap().header().mx_frame;
    assert!(
        final_mx >= 12_260,
        "expected >= 12260 frames across 4 WAL-index regions, got {final_mx}"
    );
    assert_eq!(fs::metadata(&big_shm).unwrap().len(), 4 * 32_768);

    // Separate Rust B-tree reader verifies latest values & `integrity_check` across all 4 SHM blocks:
    let expected_seq = seq - 1;
    let mut r_bt = Btree::open_file(&big_db, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    r_bt.begin_read().unwrap();
    assert_eq!(
        r_bt.integrity_check(&[1, big_root]).unwrap(),
        Vec::<String>::new()
    );
    let scanned = r_bt.scan_table(big_root).unwrap();
    assert_eq!(scanned.len(), 20);
    for (rowid, payload) in &scanned {
        if [1i64, 5, 10, 15, 20].contains(rowid) {
            let cols = decode_record(payload).unwrap();
            let prefix = format!("SEQ-{expected_seq:06}-ID-{rowid:02}-");
            let Value::Blob(ref b) = cols[1] else {
                panic!("expected Blob payload, got {:?}", cols[1]);
            };
            assert!(b.starts_with(prefix.as_bytes()));
        }
    }
    r_bt.close().unwrap();

    #[cfg(unix)]
    {
        // C SQLite 3.54.1 opens the live 12,260+ frame 4-region WAL/SHM and passes `integrity_check` + `COUNT=20`!
        let ver = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&big_db)
            .arg("1")
            .output()
            .unwrap();
        let txt = String::from_utf8_lossy(&ver.stdout);
        assert!(
            txt.contains("INTEGRITY=ok") && txt.contains("COUNT=20"),
            "12260-frame WAL failed C check: {txt}"
        );
    }
    bt_big.close().unwrap();

    // 4B. Bidirectional C SQLite 3.54.1 <-> Rust WAL write, recovery, and checkpoint at `512`, `1024`, `4096`, `65536`!
    #[cfg(unix)]
    for &ps in &[512u32, 1024, 4096, 65_536] {
        let db_p = sandbox.path().join(format!("interop_{ps}.db"));
        let wal_p = sandbox.path().join(format!("interop_{ps}.db-wal"));
        let shm_p = sandbox.path().join(format!("interop_{ps}.db-shm"));

        // Step 1: Rust creates WAL DB at `page_size = ps`, checkpoints Page 1 to `.db`, then appends
        // uncheckpointed WAL rows `1..=12`!
        let mut bt = Btree::open_file(
            &db_p,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt.pager_mut().set_page_size(ps).unwrap();
        bt.set_journal_mode(JournalMode::Wal).unwrap();

        bt.begin_write().unwrap();
        let root = init_items_schema_in_btree(&mut bt);
        for id in 1..=4i64 {
            let payload = format!("rust-init-{ps}-{id:03}").into_bytes();
            bt.insert_table(
                root,
                id,
                &encode_record(&[Value::Integer(id), Value::Blob(payload)]),
            )
            .unwrap();
        }
        bt.commit().unwrap();
        bt.wal_checkpoint(WalCheckpointMode::Full, None).unwrap();

        bt.begin_write().unwrap();
        for id in 5..=12i64 {
            let payload = vec![b'R'; (ps as usize / 3).clamp(64, 12_000)];
            bt.insert_table(
                root,
                id,
                &encode_record(&[Value::Integer(id), Value::Blob(payload)]),
            )
            .unwrap();
        }
        bt.commit().unwrap();

        if ps == 65_536 {
            // Verify exact 64 KiB page-size encodings across `.db`, `-wal`, and `-shm`!
            let db_hdr = fs::read(&db_p).unwrap();
            assert_eq!(
                &db_hdr[16..18],
                &[0x00, 0x01],
                ".db @16..18 must be be16(1)"
            );

            let wal_hdr = fs::read(&wal_p).unwrap();
            assert_eq!(
                u32::from_be_bytes([wal_hdr[8], wal_hdr[9], wal_hdr[10], wal_hdr[11]]),
                65_536,
                "-wal @8..12 must be be32(65536), NOT 1"
            );

            let shm_bytes = fs::read(&shm_p).unwrap();
            assert_eq!(
                u16::from_ne_bytes([shm_bytes[14], shm_bytes[15]]),
                1,
                "-shm WalIndexHdr copy1 @14..16 must be ne16(1)"
            );
            assert_eq!(
                u16::from_ne_bytes([shm_bytes[48 + 14], shm_bytes[48 + 15]]),
                1,
                "-shm WalIndexHdr copy2 @62..64 must be ne16(1)"
            );
        }

        // Step 2: Keep a dummy `.db` `SHARED` lock open so `bt.close()` leaves `-wal`/`-shm` on disk,
        // then zero `-shm` to force C SQLite 3.54.1 to recover Rust's `-wal`, verify 12 rows, and
        // have C SQLite append rows `13..=20` into `-wal`!
        let hold_shared = OsVfs::new()
            .open(Some(&db_p), OpenFlags::READONLY | OpenFlags::MAIN_DB)
            .unwrap();
        hold_shared.lock(LockLevel::Shared).unwrap();
        bt.close().unwrap();
        drop(hold_shared);
        assert!(wal_p.exists());

        fs::write(&shm_p, vec![0u8; 32_768]).unwrap();
        let ver1 = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&db_p)
            .arg("1")
            .output()
            .unwrap();
        let txt1 = String::from_utf8_lossy(&ver1.stdout);
        assert!(
            txt1.contains("INTEGRITY=ok") && txt1.contains("COUNT=12"),
            "ps={ps} C recovery of Rust WAL failed: {txt1}"
        );

        let app = Command::new(c_wal_helper())
            .arg("append-wal-crash")
            .arg(&db_p)
            .arg("0")
            .arg("13")
            .arg("8")
            .arg("from-c")
            .output()
            .unwrap();
        assert!(app.status.success());

        // Step 3: Zero `-shm` to force Rust `walIndexRecover` to recover C SQLite's `-wal` at `ps`,
        // verify all 20 rows & `integrity_check`, run `TRUNCATE` checkpoint, and verify in C SQLite!
        fs::write(&shm_p, vec![0u8; 32_768]).unwrap();
        let mut bt2 = Btree::open_file(&db_p, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        bt2.begin_read().unwrap();
        assert_eq!(bt2.pager().page_size(), ps);
        let root2 = find_items_root_in_btree(&mut bt2);
        assert_eq!(bt2.scan_table(root2).unwrap().len(), 20);
        assert_eq!(
            bt2.integrity_check(&[1, root2]).unwrap(),
            Vec::<String>::new()
        );
        bt2.pager_mut().end_read_if_unused().unwrap();

        let trunc = bt2
            .wal_checkpoint(WalCheckpointMode::Truncate, None)
            .unwrap();
        assert!(!trunc.busy && trunc.n_log == 0 && trunc.n_ckpt == 0);
        assert_eq!(fs::metadata(&wal_p).unwrap().len(), 0);
        bt2.close().unwrap();

        let ver2 = Command::new(c_wal_helper())
            .arg("verify-integrity-and-count")
            .arg(&db_p)
            .arg("0")
            .output()
            .unwrap();
        let txt2 = String::from_utf8_lossy(&ver2.stdout);
        assert!(
            txt2.contains("INTEGRITY=ok") && txt2.contains("COUNT=20"),
            "ps={ps} post-Rust-checkpoint C verify failed: {txt2}"
        );
    }
}

// ============================================================================
// Suite 5: `walIndexRecover` Lock Discipline (`WRITE + CKPT + RECOVER`, Never `READ(0)`),
//          Busy Read-Mark Preservation & Torn/Uncommitted Tail Recovery (Groups 6 & 15)
// ============================================================================

#[test]
fn test_suite5_wal_index_recover_lock_discipline_marks_and_torn_tail() {
    let sandbox = TempSandbox::new("suite5_recovery");
    let db_path = sandbox.path().join("rec.db");
    let wal_path = sandbox.path().join("rec.db-wal");
    let shm_path = sandbox.path().join("rec.db-shm");

    let mut p = Pager::open_file(
        &db_path,
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    p.set_journal_mode(JournalMode::Wal).unwrap();
    p.begin_write().unwrap();
    let p2 = p.allocate_raw_page().unwrap();
    p.write_page(p2).unwrap()[0..10].copy_from_slice(b"COMMITTED!");
    p.commit().unwrap();
    let committed_mx = p.wal().unwrap().header().mx_frame;
    assert_eq!(committed_mx, 2);

    // Hold `.db` `SHARED` AND keep `WalShm` (`UNIX_SHM_DMS`) alive before closing `p`, so closing
    // `p` neither unlinks `-wal`/`-shm` nor resets DMS to 3 bytes (`os_unix.c:4976`)!
    let os_vfs = OsVfs::new();
    let probe_db = os_vfs
        .open(Some(&db_path), OpenFlags::READWRITE | OpenFlags::MAIN_DB)
        .unwrap();
    probe_db.lock(LockLevel::Shared).unwrap();
    let shm_setup = probe_db.open_wal_shm().unwrap();
    let shm_r0 = probe_db.open_wal_shm().unwrap();
    let shm_r2 = probe_db.open_wal_shm().unwrap();
    let _ = shm_setup.try_read_header_and_ckpt_info().unwrap();
    p.close().unwrap();

    // Append an uncommitted frame (`nTruncate == 0`) AND a half-written torn frame to `<db>-wal`!
    {
        let mut wal_bytes = fs::read(&wal_path).unwrap();
        let file_hdr =
            WalFileHeader::decode_for_recovery(wal_bytes[0..WAL_HDRSIZE].try_into().unwrap())
                .unwrap()
                .unwrap();
        let fake_page = vec![0xEEu8; 4096];
        let (uncommitted_fhdr, _) = wal_encode_frame_header(
            2,
            0, // nTruncate = 0 (uncommitted!)
            file_hdr.salt,
            file_hdr.big_end_cksum,
            [0x1111_2222, 0x3333_4444],
            &fake_page,
        )
        .unwrap();
        wal_bytes.extend_from_slice(&uncommitted_fhdr);
        wal_bytes.extend_from_slice(&fake_page);
        wal_bytes.extend_from_slice(&[0xAAu8; 500]); // torn tail!
        fs::write(&wal_path, wal_bytes).unwrap();
    }

    // Set sentinel `aReadMark[2] = 0x1234` and hold `SHARED WAL_READ_LOCK(2)`
    // (proving recovery leaves `Busy` `READ(1..=4)` marks untouched!):
    let held_r2 = shm_r2.acquire_read_slot_with_mark(2, Some(0x1234)).unwrap();
    // Corrupt/zero the dual `WalIndexHdr` (`0..96`) on disk/mmap to force `walIndexRecover`!
    {
        let f = fs::OpenOptions::new().write(true).open(&shm_path).unwrap();
        use std::os::unix::fs::FileExt;
        f.write_all_at(&[0u8; 96], 0).unwrap();
    }
    let held_r0 = shm_r0.acquire_read_slot(0).unwrap();

    // Trigger recovery via `Pager::begin_read()` while `READ(0)` and `READ(2)` are held!
    let mut rec_pager =
        Pager::open_file(&db_path, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
    rec_pager.begin_read().unwrap();
    assert_eq!(&rec_pager.read_page(2).unwrap()[0..10], b"COMMITTED!");

    // Inspect post-recovery `WalCkptInfo`:
    let (_, info) = shm_setup.try_read_header_and_ckpt_info().unwrap().unwrap();
    assert_eq!(info.n_backfill, 0);
    assert_eq!(info.n_backfill_attempted, committed_mx);
    assert_eq!(info.read_marks[0], 0);
    assert_eq!(info.read_marks[1], committed_mx);
    assert_eq!(
        info.read_marks[2], 0x1234,
        "Busy slot 2 mark MUST remain untouched by walIndexRecover"
    );
    assert_eq!(info.read_marks[3], READMARK_NOT_USED);
    assert_eq!(info.read_marks[4], READMARK_NOT_USED);

    drop(held_r0);
    drop(held_r2);
    rec_pager.end_read_if_unused().unwrap();

    // Also test recovery when `mxFrame == 0` (only WAL header + uncommitted frame -> `aReadMark[1] == READMARK_NOT_USED`)!
    {
        let (hdr_bytes, fhdr) = WalFileHeader::encode(4096, 0, [11, 22], false).unwrap();
        let page = vec![0u8; 4096];
        let (fr_hdr, _) =
            wal_encode_frame_header(1, 0, fhdr.salt, false, fhdr.cksum, &page).unwrap();
        let mut zero_commit_wal = Vec::new();
        zero_commit_wal.extend_from_slice(&hdr_bytes);
        zero_commit_wal.extend_from_slice(&fr_hdr);
        zero_commit_wal.extend_from_slice(&page);
        fs::write(&wal_path, zero_commit_wal).unwrap();

        let f = fs::OpenOptions::new().write(true).open(&shm_path).unwrap();
        use std::os::unix::fs::FileExt;
        f.write_all_at(&[0u8; 96], 0).unwrap();

        rec_pager.begin_read().unwrap();
        let (h0, info0) = shm_setup.try_read_header_and_ckpt_info().unwrap().unwrap();
        assert_eq!(h0.mx_frame, 0);
        assert_eq!(info0.read_marks[1], READMARK_NOT_USED);
        rec_pager.end_read_if_unused().unwrap();
    }

    drop(shm_setup);
    drop(shm_r0);
    drop(shm_r2);
    drop(probe_db);
    rec_pager.close().unwrap();
}

// ============================================================================
// Suite 6: Checkpoint Modes (`NOOP`, `PASSIVE`, `FULL`, `RESTART`, `TRUNCATE`) &
//          Deterministic March 2026 WAL-Reset Salt Race Guard (Groups 7 & 8)
// ============================================================================

#[test]
fn test_suite6_checkpoint_modes_and_march_2026_salt_race_guard() {
    let sandbox = TempSandbox::new("suite6_ckpt");
    let db_path = sandbox.path().join("ckpt.db");
    let wal_path = sandbox.path().join("ckpt.db-wal");
    let shm_path = sandbox.path().join("ckpt.db-shm");

    let (spy_vfs, spy_state) = SpyWalVfs::new();
    let mut p = Pager::open_with_options(
        spy_vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    p.set_journal_mode(JournalMode::Wal).unwrap();

    // Commit tx1 (frames 1..=2)
    p.begin_write().unwrap();
    let p2 = p.allocate_raw_page().unwrap();
    p.write_page(p2).unwrap()[0..6].copy_from_slice(b"SNAP_1");
    p.commit().unwrap();

    // Hold `reader1` on snapshot 1 (`mxFrame == 2`)
    let mut reader1 = Pager::open_file(&db_path, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    reader1.begin_read().unwrap();
    assert_eq!(&reader1.read_page(2).unwrap()[0..6], b"SNAP_1");

    // Commit tx2 (frames 3..=4, growing DB to 3 pages!)
    p.begin_write().unwrap();
    let p3 = p.allocate_raw_page().unwrap();
    p.write_page(2).unwrap()[0..6].copy_from_slice(b"SNAP_2");
    p.write_page(p3).unwrap()[0..6].copy_from_slice(b"PAGE_3");
    p.commit().unwrap();
    let total_frames = p.wal().unwrap().header().mx_frame as i32;
    assert!(total_frames >= 4);

    // 6A. `NOOP` on read-only connection -> `ErrorCode::ReadOnly`!
    let mut ro_ckpt = Pager::open_file(&db_path, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    assert_eq!(
        ro_ckpt
            .wal_checkpoint(WalCheckpointMode::Noop, None, None)
            .unwrap_err()
            .code,
        ErrorCode::ReadOnly
    );
    ro_ckpt.close().unwrap();

    // `NOOP` on writable connection while ANOTHER handle holds BOTH `WAL_CKPT_LOCK` AND `WAL_WRITE_LOCK`:
    // proves `NOOP` acquires neither `CKPT` nor `WRITE` and reports `(mxFrame, 0)`!
    {
        let os_f = OsVfs::new()
            .open(Some(&db_path), OpenFlags::READWRITE | OpenFlags::MAIN_DB)
            .unwrap();
        os_f.lock(LockLevel::Shared).unwrap();
        let ext_shm = os_f.open_wal_shm().unwrap();
        let _held_ckpt_and_write = ext_shm.acquire_checkpoint(CheckpointMode::Full).unwrap();

        let noop = p
            .wal_checkpoint(WalCheckpointMode::Noop, None, None)
            .unwrap();
        assert!(!noop.busy);
        assert_eq!(noop.n_log, total_frames);
        assert_eq!(noop.n_ckpt, 0);
    }

    // `NOOP` when `-shm` header is zeroed (with no active readers/writers) recovers `-shm` without backfilling!
    reader1.end_read_if_unused().unwrap();
    {
        let f = fs::OpenOptions::new().write(true).open(&shm_path).unwrap();
        use std::os::unix::fs::FileExt;
        f.write_all_at(&[0u8; 96], 0).unwrap();

        let noop_rec = p
            .wal_checkpoint(WalCheckpointMode::Noop, None, None)
            .unwrap();
        assert_eq!(
            (noop_rec.busy, noop_rec.n_log, noop_rec.n_ckpt),
            (false, total_frames, 0)
        );
    }

    // 6B. Partial `PASSIVE` backfill + `FULL` downgrade under active writer + `RESTART` + `TRUNCATE`!
    // Re-create partial snapshot: commit tx3 after `reader1` re-enters read at `total_frames`.
    reader1.begin_read().unwrap();
    p.begin_write().unwrap();
    p.write_page(2).unwrap()[0..6].copy_from_slice(b"SNAP_3");
    p.commit().unwrap();
    let total_after_tx3 = p.wal().unwrap().header().mx_frame as i32;

    // Active writer holds `WAL_WRITE_LOCK`: `FULL` downgrades to passive backfill up to `total_frames`
    // and returns `busy == true`!
    let mut active_writer =
        Pager::open_file(&db_path, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
    active_writer.begin_write().unwrap();

    let full_contended = p
        .wal_checkpoint(WalCheckpointMode::Full, None, None)
        .unwrap();
    assert!(full_contended.busy);
    assert_eq!(full_contended.n_log, total_after_tx3);
    assert_eq!(full_contended.n_ckpt, total_frames);
    active_writer.rollback().unwrap();
    active_writer.close().unwrap();

    // `RESTART` while `reader1` holds `READ(1..=4)` -> `busy == true`; after `reader1` ends -> `busy == false`!
    let restart_busy = p
        .wal_checkpoint(WalCheckpointMode::Restart, None, None)
        .unwrap();
    assert!(restart_busy.busy);
    reader1.close().unwrap();

    let restart_ok = p
        .wal_checkpoint(WalCheckpointMode::Restart, None, None)
        .unwrap();
    assert!(!restart_ok.busy && restart_ok.n_ckpt == restart_ok.n_log);

    // `TRUNCATE` header-before-truncate order proof: inject `truncate(0)` I/O error -> `-shm` header
    // MUST already have `mx_frame == 0`, `aReadMark[1] == 0`, `aReadMark[2..=4] == READMARK_NOT_USED`!
    p.begin_write().unwrap();
    p.write_page(2).unwrap()[0..6].copy_from_slice(b"SNAP_4");
    p.commit().unwrap();
    assert!(fs::metadata(&wal_path).unwrap().len() > 0);

    spy_state.fail_wal_truncate.store(true, Ordering::SeqCst);
    let trunc_err = p
        .wal_checkpoint(WalCheckpointMode::Truncate, None, None)
        .unwrap_err();
    assert_eq!(trunc_err.extended_code, extended::SQLITE_IOERR_TRUNCATE);
    assert!(
        fs::metadata(&wal_path).unwrap().len() > 0,
        "-wal still has old bytes after injected truncate(0) failure"
    );
    assert_eq!(
        p.wal().unwrap().header().mx_frame,
        0,
        "WalIndexHdr must be reset BEFORE wal_file.truncate(0)"
    );
    {
        let os_f = OsVfs::new()
            .open(Some(&db_path), OpenFlags::READONLY | OpenFlags::MAIN_DB)
            .unwrap();
        let shm_chk = os_f.open_wal_shm().unwrap();
        let (h, info) = shm_chk.try_read_header_and_ckpt_info().unwrap().unwrap();
        assert_eq!(h.mx_frame, 0);
        assert_eq!(info.read_marks[1], 0);
        assert_eq!(info.read_marks[2], READMARK_NOT_USED);
        assert_eq!(info.read_marks[3], READMARK_NOT_USED);
        assert_eq!(info.read_marks[4], READMARK_NOT_USED);
    }

    spy_state.fail_wal_truncate.store(false, Ordering::SeqCst);
    let trunc_clean = p
        .wal_checkpoint(WalCheckpointMode::Truncate, None, None)
        .unwrap();
    assert!(!trunc_clean.busy && trunc_clean.n_log == 0 && trunc_clean.n_ckpt == 0);
    assert_eq!(fs::metadata(&wal_path).unwrap().len(), 0);

    // 6C. Deterministic March 2026 WAL-Reset Salt Race Regression Test (`wal.c:2354-2355`, Group 7)!
    // Setup:
    // 1) Commit 3 frames (`mxFrame = 3`, `salt = S_A`, Page 2 = `"SALT_OLD"`), then `FULL` checkpoint
    //    so `nBackfill == 3 == mxFrame`!
    p.begin_write().unwrap();
    p.write_page(2).unwrap()[0..8].copy_from_slice(b"SALT_OLD");
    p.write_page(3).unwrap()[0..8].copy_from_slice(b"P3_CLEAN");
    p.commit().unwrap();
    let full_pre = p
        .wal_checkpoint(WalCheckpointMode::Full, None, None)
        .unwrap();
    assert!(!full_pre.busy && full_pre.n_ckpt == full_pre.n_log && full_pre.n_log >= 2);
    let salt_before = p.wal().unwrap().header().salt;

    // 2) Run `PASSIVE` checkpoint with `after_read_hdr_hook`:
    //    - Before hook: checkpointer reads `self.hdr` (`mx_frame >= 2, salt = salt_before`) while
    //      holding ONLY `WAL_CKPT_LOCK`!
    //    - Inside hook: concurrent `writer_race` (on `READ(0)` because `nBackfill == mxFrame`!)
    //      acquires `WAL_WRITE_LOCK`, wraps the WAL via `walRestartLog` (`salt = salt_after != salt_before`,
    //      `nBackfill = 0, nBackfillAttempted = 0`), writes an UNCOMMITTED spilled frame (`"UNCOMMIT"`)
    //      to Page 2 at frame 1, and rolls back / commits a 1-page header-only tx!
    //    - After hook: checkpointer acquires `EXCLUSIVE WAL_READ_LOCK(0)`, detects `live_salt != salt_before`,
    //      and MUST NOT touch `nBackfillAttempted`, `nBackfill`, or overwrite `.db` with `"UNCOMMIT"`!
    let db_path_hook = db_path.clone();
    let mut writer_ran = false;
    let mut hook = || {
        let mut w_race =
            Pager::open_file(&db_path_hook, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        w_race.begin_write().unwrap();
        assert_eq!(w_race.wal().unwrap().read_lock_slot(), Some(0));
        // Spill uncommitted `"BAD_DATA"` into wrapped frame 1 (changing `WalIndexHdr.salt` in `-shm`
        // AND resetting `nBackfill = 0, nBackfillAttempted = 0`), then roll back!
        w_race.write_page(2).unwrap()[0..8].copy_from_slice(b"BAD_DATA");
        w_race.spill_dirty_pages_to_wal().unwrap();
        w_race.rollback().unwrap();
        w_race.close().unwrap();
        writer_ran = true;
    };

    let race_res = p
        .wal_checkpoint_with_hook(WalCheckpointMode::Passive, None, None, Some(&mut hook))
        .unwrap();
    assert!(writer_ran);
    assert!(!race_res.busy);
    assert_eq!(
        race_res.n_ckpt, 0,
        "March 2026 salt guard must abort backfill when salt changed before READ(0) lock!"
    );

    // Verify `nBackfillAttempted == 0`, `salt != salt_before`, AND `.db` Page 2 on disk is still `"SALT_OLD"`!
    {
        let os_f = OsVfs::new()
            .open(Some(&db_path), OpenFlags::READONLY | OpenFlags::MAIN_DB)
            .unwrap();
        let shm_chk = os_f.open_wal_shm().unwrap();
        let (live_h, live_info) = shm_chk.try_read_header_and_ckpt_info().unwrap().unwrap();
        assert_ne!(live_h.salt, salt_before);
        assert_eq!(live_info.n_backfill, 0);
        assert_eq!(live_info.n_backfill_attempted, 0);

        let mut disk_p2 = [0u8; 8];
        os_f.read_at(&mut disk_p2, 4096).unwrap();
        assert_eq!(&disk_p2, b"SALT_OLD");
    }

    p.close().unwrap();
}

// ============================================================================
// Suite 7: Read-Only WAL (`WAL_SHM_RDONLY` / `bShmUnreliable`), `immutable`,
//          `locking_mode = EXCLUSIVE`, Journaled `DELETE <-> WAL` Transitions &
//          Last-Connection Close (Groups 9, 10, 11)
// ============================================================================

#[test]
fn test_suite7_readonly_unreliable_shm_immutable_exclusive_transitions_and_close() {
    let sandbox = TempSandbox::new("suite7_modes");

    // 7A. `WAL_SHM_RDONLY` / `bShmUnreliable` (`ReadonlyCantInit` -> heap index under `READ(0)`)
    //     + switching back to live shared memory when a writer attaches!
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let ro_db = sandbox.path().join("ro_unreliable.db");
        let ro_wal = sandbox.path().join("ro_unreliable.db-wal");
        let ro_shm = sandbox.path().join("ro_unreliable.db-shm");

        // C SQLite writes WAL rows and exits via `_exit(0)`, leaving `-wal` and `-shm` on disk
        // with NO active DMS holder!
        let app = Command::new(c_wal_helper())
            .arg("append-wal-crash")
            .arg(&ro_db)
            .arg("4096")
            .arg("1")
            .arg("6")
            .arg("ro-init")
            .output()
            .unwrap();
        assert!(app.status.success());
        assert!(ro_wal.exists() && ro_shm.exists());

        // Make `.db`, `-wal`, and `-shm` `0444` so read-only open hits `ReadonlyCantInit` on DMS!
        fs::set_permissions(&ro_db, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&ro_wal, fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&ro_shm, fs::Permissions::from_mode(0o444)).unwrap();

        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let mut bt_ro = Btree::open_with_options(
            os_vfs.clone(),
            Some(&ro_db),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
            PagerOpenOptions {
                readonly_shm: true,
                ..PagerOpenOptions::default()
            },
        )
        .unwrap();
        bt_ro.begin_read().unwrap();
        assert!(bt_ro.pager().wal().unwrap().is_shm_unreliable());
        assert_eq!(bt_ro.pager().wal().unwrap().read_lock_slot(), Some(0));
        let root = find_items_root_in_btree(&mut bt_ro);
        assert_eq!(bt_ro.scan_table(root).unwrap().len(), 6);
        bt_ro.end_read_if_unused_via_pager();

        // Restore `0644` permissions:
        fs::set_permissions(&ro_db, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&ro_wal, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&ro_shm, fs::Permissions::from_mode(0o644)).unwrap();

        // First: external C SQLite commits row 7 and exits via `_exit(0)` (leaving DMS unlocked!).
        // `bt_ro` (`b_shm_unreliable == true`) scans frames past `mxFrame`, sees `nTruncate != 0`,
        // rebuilds its heap index, and reads all 7 rows while staying `b_shm_unreliable == true`!
        let app2 = Command::new(c_wal_helper())
            .arg("append-wal-crash")
            .arg(&ro_db)
            .arg("0")
            .arg("7")
            .arg("1")
            .arg("writer-7")
            .output()
            .unwrap();
        assert!(app2.status.success());

        bt_ro.begin_read().unwrap();
        assert!(bt_ro.pager().wal().unwrap().is_shm_unreliable());
        assert_eq!(bt_ro.scan_table(root).unwrap().len(), 7);
        bt_ro.end_read_if_unused_via_pager();

        // Second: live C SQLite process attaches to `ro_db` (initializing `-shm` and holding
        // `UNIX_SHM_DMS` `F_RDLCK`, making DMS `ActiveShared`!).
        // `bt_ro` (still open with `readonly_shm: true`!) re-enters `begin_read()`:
        // `walBeginShmUnreliable` detects DMS is now `ActiveShared`, discards its private heap index,
        // switches `b_shm_unreliable -> false`, and reads all 7 rows from shared `-shm`!
        let live_c_holder = spawn_c_wal_reader(&ro_db);
        bt_ro.begin_read().unwrap();
        assert!(!bt_ro.pager().wal().unwrap().is_shm_unreliable());
        assert!((1..=4).contains(&bt_ro.pager().wal().unwrap().read_lock_slot().unwrap()));
        assert_eq!(bt_ro.scan_table(root).unwrap().len(), 7);
        bt_ro.close().unwrap();
        release_c_child(live_c_holder);
    }

    // 7B. `immutable = true` reads WAL without creating `-shm` or taking OS locks, and rejects writes!
    #[cfg(unix)]
    {
        let imm_db = sandbox.path().join("imm.db");
        let imm_shm = sandbox.path().join("imm.db-shm");
        let app = Command::new(c_wal_helper())
            .arg("append-wal-crash")
            .arg(&imm_db)
            .arg("4096")
            .arg("1")
            .arg("5")
            .arg("imm")
            .output()
            .unwrap();
        assert!(app.status.success());
        fs::remove_file(&imm_shm).unwrap();

        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let mut bt_imm = Btree::open_with_options(
            os_vfs,
            Some(&imm_db),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
            PagerOpenOptions {
                immutable: true,
                ..PagerOpenOptions::default()
            },
        )
        .unwrap();
        bt_imm.begin_read().unwrap();
        assert!(!imm_shm.exists(), "immutable mode must NOT create -shm");
        let root = find_items_root_in_btree(&mut bt_imm);
        assert_eq!(bt_imm.scan_table(root).unwrap().len(), 5);
        assert_eq!(bt_imm.begin_write().unwrap_err().code, ErrorCode::ReadOnly);
        bt_imm.close().unwrap();
    }

    // 7C. `locking_mode = EXCLUSIVE`:
    //   i) Set BEFORE WAL open -> `WalExclusiveMode::HeapMemory` (never creates `-shm`)!
    //  ii) Set AFTER WAL open -> `Normal -> Exclusive` on write, then `Exclusive -> Normal`!
    let ex1_db = sandbox.path().join("ex_heap.db");
    let ex1_shm = sandbox.path().join("ex_heap.db-shm");
    {
        let mut p = Pager::open_file(
            &ex1_db,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        assert_eq!(
            p.set_locking_mode(LockingMode::Exclusive),
            LockingMode::Exclusive
        );
        p.set_journal_mode(JournalMode::Wal).unwrap();
        p.begin_write().unwrap();
        assert!(p.wal().unwrap().is_heap_memory());
        let p2 = p.allocate_raw_page().unwrap();
        p.write_page(p2).unwrap()[0..7].copy_from_slice(b"EX_HEAP");
        p.commit().unwrap();
        assert!(
            !ex1_shm.exists(),
            "HeapMemory EXCLUSIVE WAL must not create -shm"
        );
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Exclusive)
        );
        p.close().unwrap();
    }

    let ex2_db = sandbox.path().join("ex_shm.db");
    {
        let mut p = Pager::open_file(
            &ex2_db,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        p.set_journal_mode(JournalMode::Wal).unwrap();
        p.begin_write().unwrap();
        assert_eq!(p.wal().unwrap().exclusive_mode(), WalExclusiveMode::Normal);
        p.commit().unwrap();

        assert_eq!(
            p.set_locking_mode(LockingMode::Exclusive),
            LockingMode::Exclusive
        );
        p.begin_write().unwrap();
        let p2 = p.allocate_raw_page().unwrap();
        p.write_page(p2).unwrap()[0..6].copy_from_slice(b"EX_SHM");
        assert_eq!(
            p.wal().unwrap().exclusive_mode(),
            WalExclusiveMode::Exclusive
        );
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Exclusive)
        );
        p.commit().unwrap();
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Exclusive)
        );

        // Switch back to `LockingMode::Normal`: next transaction commit releases `.db` `Exclusive -> Shared`!
        assert_eq!(p.set_locking_mode(LockingMode::Normal), LockingMode::Normal);
        p.begin_write().unwrap();
        p.write_page(2).unwrap()[0..6].copy_from_slice(b"NORM!!");
        p.commit().unwrap();
        assert_eq!(p.wal().unwrap().exclusive_mode(), WalExclusiveMode::Normal);
        assert_eq!(
            p.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Shared)
        );
        p.close().unwrap();
    }

    // 7D. `DELETE <-> WAL` Rollback-Journaled Page 1 `(1,1) <-> (2,2)` Transitions, `Busy` under
    //     active reader, `page_size` immutability in WAL, and Last-Connection Close!
    let tr_db = sandbox.path().join("transitions.db");
    let tr_wal = sandbox.path().join("transitions.db-wal");
    let tr_shm = sandbox.path().join("transitions.db-shm");
    let (spy_vfs, spy_state) = SpyWalVfs::new();

    let mut p1 = Pager::open_with_options(
        spy_vfs.clone(),
        Some(&tr_db),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    p1.begin_write().unwrap();
    let pg2 = p1.allocate_raw_page().unwrap();
    p1.write_page(pg2).unwrap()[0..8].copy_from_slice(b"TRANS_OK");
    p1.commit().unwrap();
    assert_eq!(&fs::read(&tr_db).unwrap()[18..20], &[1, 1]);

    // Cannot enter WAL inside an active transaction:
    p1.begin_read().unwrap();
    assert_eq!(
        p1.set_journal_mode(JournalMode::Wal).unwrap_err().code,
        ErrorCode::Error
    );
    p1.end_read_if_unused().unwrap();

    // Active reader on `.db` blocks `DELETE -> WAL` with `ErrorCode::Busy` and leaves mode `Delete`!
    let mut blocker = Pager::open_file(&tr_db, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    blocker.begin_read().unwrap();
    assert_eq!(
        p1.set_journal_mode(JournalMode::Wal).unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(p1.journal_mode(), JournalMode::Delete);
    assert_eq!(&fs::read(&tr_db).unwrap()[18..20], &[1, 1]);
    blocker.close().unwrap();

    // `DELETE -> WAL` writes & syncs rollback journal BEFORE updating Page 1 bytes `18..19` to `[2, 2]`!
    let j_writes_before = spy_state.journal_writes.load(Ordering::SeqCst);
    let j_syncs_before = spy_state.journal_syncs.load(Ordering::SeqCst);
    assert_eq!(
        p1.set_journal_mode(JournalMode::Wal).unwrap(),
        JournalMode::Wal
    );
    assert!(spy_state.journal_writes.load(Ordering::SeqCst) > j_writes_before);
    assert!(spy_state.journal_syncs.load(Ordering::SeqCst) > j_syncs_before);
    assert_eq!(&fs::read(&tr_db).unwrap()[18..20], &[2, 2]);

    // `set_page_size` is locked (no-op) in WAL mode!
    let orig_ps = p1.page_size();
    p1.set_page_size(8192).unwrap();
    assert_eq!(p1.page_size(), orig_ps);

    // Commit a WAL write on `p1`, and open a second WAL connection `p2_conn` (idle in `Open` after
    // `begin_read` + `end_read_if_unused`, holding `SHARED` on `.db`!).
    p1.begin_write().unwrap();
    p1.write_page(2).unwrap()[0..8].copy_from_slice(b"WAL_DATA");
    p1.commit().unwrap();
    assert!(tr_wal.exists() && tr_shm.exists());

    let mut p2_conn = Pager::open_with_vfs(
        spy_vfs.clone(),
        Some(&tr_db),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    p2_conn.begin_read().unwrap();
    assert_eq!(&p2_conn.read_page(2).unwrap()[0..8], b"WAL_DATA");
    p2_conn.end_read_if_unused().unwrap();
    assert_eq!(p2_conn.transaction_state(), TransactionState::Open);
    assert_eq!(
        p2_conn.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Shared)
    );

    // Because `p2_conn` holds `SHARED` on `.db`, `p1.set_journal_mode(Delete)` MUST fail with `Busy`
    // WITHOUT closing `p1`'s WAL or unlinking `-wal`/`-shm`!
    assert_eq!(
        p1.set_journal_mode(JournalMode::Delete).unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(p1.journal_mode(), JournalMode::Wal);
    assert!(tr_wal.exists() && tr_shm.exists());

    // Closing `p1` while `p2_conn` still holds `SHARED` on `.db` MUST keep `-wal` and `-shm` alive!
    p1.close().unwrap();
    assert!(
        tr_wal.exists() && tr_shm.exists(),
        "non-last connection close must NOT unlink -wal or -shm"
    );

    // `p2_conn` can still read and transition `WAL -> DELETE` once it is the sole connection!
    #[cfg(unix)]
    {
        // Also verify an external C SQLite WAL reader blocks `WAL -> DELETE` with `Busy`!
        let c_reader = spawn_c_wal_reader(&tr_db);
        assert_eq!(
            p2_conn
                .set_journal_mode(JournalMode::Delete)
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        release_c_child(c_reader);
    }

    let j_writes_mid = spy_state.journal_writes.load(Ordering::SeqCst);
    let j_syncs_mid = spy_state.journal_syncs.load(Ordering::SeqCst);
    assert_eq!(
        p2_conn.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Delete
    );
    assert!(
        spy_state.journal_writes.load(Ordering::SeqCst) > j_writes_mid,
        "WAL -> DELETE must write rollback journal before modifying Page 1 bytes 18..19"
    );
    assert!(
        spy_state.journal_syncs.load(Ordering::SeqCst) > j_syncs_mid,
        "WAL -> DELETE must sync rollback journal before modifying Page 1 bytes 18..19"
    );
    assert!(!tr_wal.exists() && !tr_shm.exists());
    assert_eq!(&fs::read(&tr_db).unwrap()[18..20], &[1, 1]);

    // Verify `"WAL_DATA"` was checkpointed into `.db` before `-wal` was unlinked!
    p2_conn.begin_read().unwrap();
    assert_eq!(&p2_conn.read_page(2).unwrap()[0..8], b"WAL_DATA");

    // Switch back to `WAL`, write `"FINAL_CL"`, and verify LAST-connection `close()` checkpoints & unlinks!
    p2_conn.end_read_if_unused().unwrap();
    p2_conn.set_journal_mode(JournalMode::Wal).unwrap();
    p2_conn.begin_write().unwrap();
    p2_conn.write_page(2).unwrap()[0..8].copy_from_slice(b"FINAL_CL");
    p2_conn.commit().unwrap();
    assert!(tr_wal.exists() && tr_shm.exists());

    p2_conn.close().unwrap();
    assert!(
        !tr_wal.exists() && !tr_shm.exists(),
        "last-connection close must checkpoint and unlink -wal and -shm"
    );

    let mut check_after =
        Pager::open_file(&tr_db, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    check_after.begin_read().unwrap();
    assert_eq!(&check_after.read_page(2).unwrap()[0..8], b"FINAL_CL");
    check_after.close().unwrap();

    // Also verify `MemoryVfs` rejects `JournalMode::Wal` on `PrivateTemporary` / `Memory` cleanly:
    let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());
    let mut temp_p = Pager::open_private_temporary(mem_vfs, PagerOpenOptions::default()).unwrap();
    assert_eq!(
        temp_p.set_journal_mode(JournalMode::Wal).unwrap(),
        JournalMode::Delete
    );
    temp_p.close().unwrap();
    let _ = (
        WAL_FRAME_HDRSIZE,
        WAL_MAGIC_LE,
        DatabaseHeader::default(),
        wal_frame_offset(1, 4096),
    );
}

// ============================================================================
// Suite 8: Orchestrator Audit Regressions (Items 1..4)
//   1a) `wal_try_begin_read` sentinel check (`mx_frame == u32::MAX` never selects `READMARK_NOT_USED`)
//   1b) `try_claim_read_mark` / `try_claim_positive_read_mark_under_write` separated from `SHARED` lock
//   2)  `run_checkpoint_under_guard` `mx_frame == 0` `TRUNCATE` (`Busy -> busy=true`, `walRestartHdr` + `truncate(0)`)
//   3)  Checkpoint corrupt `n_page` size sanity check (`wal.c:2365-2376` -> `ErrorCode::Corrupt`)
//   4)  Read-only `transition_journal_mode` WAL discovery & no-op `set_journal_mode` inside transactions
// ============================================================================

#[test]
fn test_suite8_audit_edge_case_regressions() {
    // ------------------------------------------------------------------------
    // 1a) Corrupt/fuzzed `mx_frame == u32::MAX` (`0xffff_ffff` == `READMARK_NOT_USED`)
    //     MUST NOT panic or select `READMARK_NOT_USED` slots!
    // ------------------------------------------------------------------------
    let mvfs = Arc::new(MemoryVfs::new());
    let vfs: Arc<dyn Vfs> = mvfs;
    let s1_db = Path::new("audit_s1.db");

    let mut p_init = Pager::open_with_options(
        vfs.clone(),
        Some(s1_db),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    p_init.begin_write().unwrap();
    let p2 = p_init.allocate_raw_page().unwrap();
    p_init.write_page(p2).unwrap()[0..4].copy_from_slice(b"S1A!");
    p_init.commit().unwrap();
    p_init.set_journal_mode(JournalMode::Wal).unwrap();

    p_init.begin_write().unwrap();
    p_init.write_page(2).unwrap()[0..4].copy_from_slice(b"WAL1");
    p_init.commit().unwrap();

    // Keep an explicit `WalShm` handle open and publish `mx_frame = u32::MAX` (`0xffff_ffff`),
    // while `aReadMark[1] == 1` and `aReadMark[2..=4] == READMARK_NOT_USED`!
    let mut s1_db_fd = vfs.open(Some(s1_db), OpenFlags::READWRITE).unwrap();
    let mut shm_admin = Arc::new(s1_db_fd.open_wal_shm().unwrap());
    let orig_s1_hdr = {
        let mut wg = shm_admin.acquire_writer().unwrap();
        let orig = wg.read_header_verified().unwrap();
        let fuzzed = WalIndexHeaderData::new(
            orig.page_size,
            u32::MAX,
            orig.n_page,
            orig.change_counter.wrapping_add(1),
            orig.big_end_cksum != 0,
            orig.salt,
            orig.frame_cksum,
        )
        .unwrap();
        wg.publish_header(&fuzzed).unwrap();
        wg.release().unwrap();
        orig
    };

    let mut s1_ro_fd = vfs.open(Some(s1_db), OpenFlags::READONLY).unwrap();
    let mut direct_wal = Wal::open(
        vfs.clone(),
        &*s1_ro_fd,
        Path::new("audit_s1.db-wal"),
        false,
        -1,
        ShmOpenOptions {
            readonly_shm: true,
            ..ShmOpenOptions::default()
        },
        false,
    )
    .unwrap();
    assert!(
        direct_wal
            .begin_read_transaction(&*s1_ro_fd)
            .expect("wal_try_begin_read with mx_frame == u32::MAX must not panic")
    );
    assert_eq!(
        direct_wal.read_lock_slot(),
        Some(1),
        "must select slot 1 (mark=1), NEVER slots 2..=4 (READMARK_NOT_USED)"
    );
    direct_wal.end_read_transaction().unwrap();
    direct_wal.abort_open();
    s1_ro_fd.close().unwrap();

    let mut ro_shm_p = Pager::open_with_options(
        vfs.clone(),
        Some(s1_db),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        PagerOpenOptions {
            readonly_shm: true,
            ..PagerOpenOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        ro_shm_p.begin_read().unwrap_err().code,
        ErrorCode::Corrupt,
        "Pager::begin_read on fuzzed mx_frame == u32::MAX must return Corrupt without panicking"
    );
    ro_shm_p.close().unwrap();

    // ------------------------------------------------------------------------
    // 1b) `try_claim_read_mark` & `try_claim_positive_read_mark_under_write`
    //     update `aReadMark[slot]` and release `EXCLUSIVE` cleanly without touching later slots!
    // ------------------------------------------------------------------------
    let mut s1_r1_fd = vfs.open(Some(s1_db), OpenFlags::READWRITE).unwrap();
    let mut shm_r1 = s1_r1_fd.open_wal_shm().unwrap();
    let r1_guard = shm_r1.acquire_read_slot(1).unwrap();

    // Slot 1 is held SHARED -> `try_claim_read_mark(1, ...)` returns `Ok(false)` (`Busy`),
    // while `try_claim_read_mark(2, 1, false)` returns `Ok(true)`, leaves `shm_admin` idle,
    // and leaves `aReadMark[3..=4] == READMARK_NOT_USED`!
    assert!(!shm_admin.try_claim_read_mark(1, 1, false).unwrap());
    assert!(shm_admin.try_claim_read_mark(2, 1, false).unwrap());
    assert_eq!(shm_admin.lock_state().unwrap().held_mask(), 0);

    // Same separation under `OwnedWriterTxnGuard` (`walRestartLog` `READ(0) -> WRITE -> release READ(0)`):
    let mut owned_wg =
        sqliters::vfs::WalShm::acquire_owned_read_slot_with_mark(&shm_admin, 0, None, false)
            .unwrap()
            .upgrade_to_writer()
            .unwrap();
    owned_wg.release_read_slot_zero_under_write().unwrap();
    assert!(
        !owned_wg
            .try_claim_positive_read_mark_under_write(1, 1)
            .unwrap()
    );
    assert!(
        owned_wg
            .try_claim_positive_read_mark_under_write(2, 1)
            .unwrap()
    );
    assert_eq!(
        owned_wg.reader_slot(),
        None,
        "mark claim must NOT acquire SHARED reader slot yet"
    );
    owned_wg
        .acquire_positive_read_slot_under_write(2, None)
        .unwrap();
    assert_eq!(owned_wg.reader_slot(), Some(2));
    let ckpt_after_claim = owned_wg.read_ckpt_info().unwrap();
    assert_eq!(ckpt_after_claim.read_marks[2], 1);
    assert_eq!(ckpt_after_claim.read_marks[3], READMARK_NOT_USED);
    assert_eq!(ckpt_after_claim.read_marks[4], READMARK_NOT_USED);
    owned_wg.unlock_all().unwrap();
    r1_guard.release().unwrap();
    shm_r1.close(false).unwrap();
    s1_r1_fd.close().unwrap();
    {
        let mut wg = shm_admin.acquire_writer().unwrap();
        wg.publish_header(&orig_s1_hdr).unwrap();
        wg.release().unwrap();
    }
    Arc::get_mut(&mut shm_admin).unwrap().close(false).unwrap();
    s1_db_fd.close().unwrap();
    p_init.close().unwrap();

    // ------------------------------------------------------------------------
    // 2) `run_checkpoint_under_guard` when `mx_frame == 0` with rolled-back frames in `-wal`:
    //    - Contended `READ(1..=4)` -> `Ok(WalCheckpointResult { busy: true, n_log: 0, n_ckpt: 0 })`
    //    - Uncontended `TRUNCATE` -> runs `walRestartHdr` FIRST (`nCkpt++`, `salt[0]++`), `truncate(0)` SECOND!
    // ------------------------------------------------------------------------
    let s2_db = Path::new("audit_s2.db");
    let s2_wal = Path::new("audit_s2.db-wal");
    let mut p_s2 = Pager::open_with_options(
        vfs.clone(),
        Some(s2_db),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    p_s2.begin_write().unwrap();
    for _ in 0..8 {
        let pg = p_s2.allocate_raw_page().unwrap();
        p_s2.write_page(pg).unwrap()[0..4].copy_from_slice(b"INIT");
    }
    p_s2.commit().unwrap();
    p_s2.set_journal_mode(JournalMode::Wal).unwrap();

    // Open WAL via a write that spills dirty frames to `-wal`, then `rollback()`
    // so `mx_frame == 0` while `-wal` on disk has `> 0` rolled-back bytes!
    p_s2.begin_write().unwrap();
    for pg in 2..=6 {
        p_s2.write_page(pg).unwrap()[0..4].copy_from_slice(b"SPIL");
    }
    assert!(p_s2.spill_dirty_pages_to_wal().unwrap() > 0);
    p_s2.rollback().unwrap();

    {
        let mut wfd = vfs.open(Some(s2_wal), OpenFlags::READONLY).unwrap();
        assert!(
            wfd.file_size().unwrap() > 0,
            "spilled then rolled-back WAL must have >0 bytes on disk while mx_frame == 0"
        );
        wfd.close().unwrap();
    }
    assert_eq!(p_s2.wal().unwrap().header().mx_frame, 0);
    let salt0_before = p_s2.wal().unwrap().header().salt[0];
    let n_ckpt_before = p_s2.wal().unwrap().n_ckpt();

    // Hold `SHARED WAL_READ_LOCK(1)` on a second SHM handle: `TRUNCATE` MUST return `Ok(busy=true)`, NOT `Err(Busy)`!
    let mut s2_r_fd = vfs.open(Some(s2_db), OpenFlags::READWRITE).unwrap();
    let mut s2_r_shm = s2_r_fd.open_wal_shm().unwrap();
    let s2_r1 = s2_r_shm.acquire_read_slot(1).unwrap();

    let busy_res = p_s2
        .wal_checkpoint(WalCheckpointMode::Truncate, None, None)
        .expect("contended TRUNCATE at mx_frame==0 must return Ok(busy=true), not Err(Busy)");
    assert!(busy_res.busy);
    assert_eq!((busy_res.n_log, busy_res.n_ckpt), (0, 0));
    {
        let mut wfd = vfs.open(Some(s2_wal), OpenFlags::READONLY).unwrap();
        assert!(wfd.file_size().unwrap() > 0);
        wfd.close().unwrap();
    }

    // Release `READ(1)`: uncontended `TRUNCATE` runs `walRestartHdr` AND truncates `-wal` to 0 bytes!
    s2_r1.release().unwrap();
    let ok_res = p_s2
        .wal_checkpoint(WalCheckpointMode::Truncate, None, None)
        .unwrap();
    assert!(!ok_res.busy);
    assert_eq!((ok_res.n_log, ok_res.n_ckpt), (0, 0));
    {
        let mut wfd = vfs.open(Some(s2_wal), OpenFlags::READONLY).unwrap();
        assert_eq!(wfd.file_size().unwrap(), 0);
        wfd.close().unwrap();
    }
    assert_eq!(
        p_s2.wal().unwrap().header().salt[0],
        salt0_before.wrapping_add(1),
        "TRUNCATE at mx_frame==0 must execute walRestartHdr (incrementing salt[0])"
    );
    assert_eq!(
        p_s2.wal().unwrap().n_ckpt(),
        n_ckpt_before.wrapping_add(1),
        "TRUNCATE at mx_frame==0 must increment nCkpt"
    );
    s2_r_shm.close(false).unwrap();
    s2_r_fd.close().unwrap();
    p_s2.close().unwrap();

    // ------------------------------------------------------------------------
    // 3) Checkpoint corrupt `n_page` size sanity check (`wal.c:2365-2376`):
    //    `cur_db_size + 65536 + mx_frame * sz_page < n_page * sz_page` -> `ErrorCode::Corrupt`!
    // ------------------------------------------------------------------------
    let s3_db = Path::new("audit_s3.db");
    let mut p_s3 = Pager::open_with_options(
        vfs.clone(),
        Some(s3_db),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    p_s3.begin_write().unwrap();
    let p2_s3 = p_s3.allocate_raw_page().unwrap();
    p_s3.write_page(p2_s3).unwrap()[0..4].copy_from_slice(b"PG02");
    p_s3.commit().unwrap();
    p_s3.set_journal_mode(JournalMode::Wal).unwrap();

    p_s3.begin_write().unwrap();
    p_s3.write_page(2).unwrap()[0..4].copy_from_slice(b"PG2W");
    p_s3.commit().unwrap();

    // `.db` is 2 * 4096 = 8192 bytes, `mx_frame = 1`. Forge `n_page = 100` (`n_req = 409_600 > 77_824`)!
    let mut s3_fd = vfs.open(Some(s3_db), OpenFlags::READWRITE).unwrap();
    let mut s3_shm = s3_fd.open_wal_shm().unwrap();
    {
        let mut wg = s3_shm.acquire_writer().unwrap();
        let orig = wg.read_header_verified().unwrap();
        let forged = WalIndexHeaderData::new(
            orig.page_size,
            orig.mx_frame,
            100,
            orig.change_counter.wrapping_add(1),
            orig.big_end_cksum != 0,
            orig.salt,
            orig.frame_cksum,
        )
        .unwrap();
        wg.publish_header(&forged).unwrap();
        wg.release().unwrap();
    }
    let ckpt_err = p_s3
        .wal_checkpoint(WalCheckpointMode::Full, None, None)
        .unwrap_err();
    assert_eq!(ckpt_err.code, ErrorCode::Corrupt);
    assert_eq!(
        s3_fd.file_size().unwrap(),
        8192,
        "corrupt n_page must abort before writing or truncating .db"
    );
    s3_shm.close(false).unwrap();
    s3_fd.close().unwrap();
    let _ = p_s3.close();

    // ------------------------------------------------------------------------
    // 4) Read-only `transition_journal_mode` WAL discovery & no-op `set_journal_mode` in tx
    // ------------------------------------------------------------------------
    let sandbox = TempSandbox::new("audit_s4_ro_jmode");
    let s4_db = sandbox.path().join("s4.db");
    {
        let mut rw = Pager::open_file(
            &s4_db,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        rw.begin_write().unwrap();
        let pg = rw.allocate_raw_page().unwrap();
        rw.write_page(pg).unwrap()[0..4].copy_from_slice(b"DATA");
        rw.commit().unwrap();
        assert_eq!(
            rw.transition_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );

        // No-op `set_journal_mode(Wal)` inside active `Read` and `Write` transactions succeeds!
        rw.begin_read().unwrap();
        assert_eq!(rw.transaction_state(), TransactionState::Read);
        assert_eq!(
            rw.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        assert_eq!(
            rw.set_journal_mode(JournalMode::Delete).unwrap_err().code,
            ErrorCode::Error
        );
        rw.end_read_if_unused().unwrap();

        rw.begin_write().unwrap();
        assert_eq!(rw.transaction_state(), TransactionState::Write);
        assert_eq!(
            rw.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        assert_eq!(
            rw.set_journal_mode(JournalMode::Delete).unwrap_err().code,
            ErrorCode::Error
        );
        rw.commit().unwrap();
        rw.close().unwrap();
    }
    assert_eq!(&fs::read(&s4_db).unwrap()[18..20], &[2, 2]);

    // Open `s4.db` READONLY (lazy open, `!is_header_loaded()`): `transition_journal_mode(Wal)`
    // loads Page 1 under `SHARED` lock, discovers `(2, 2)`, and returns `JournalMode::Wal`!
    let mut ro = Pager::open_file(&s4_db, OpenFlags::READONLY | OpenFlags::MAIN_DB).unwrap();
    assert!(!ro.is_header_loaded());
    assert_eq!(
        ro.transition_journal_mode(JournalMode::Wal).unwrap(),
        JournalMode::Wal
    );
    assert!(ro.is_header_loaded());
    assert_eq!(ro.journal_mode(), JournalMode::Wal);
    assert_eq!(
        ro.transition_journal_mode(JournalMode::Delete)
            .unwrap_err()
            .code,
        ErrorCode::ReadOnly
    );
    ro.close().unwrap();

    // `:memory:` pager inside `Write` transaction treats `set_journal_mode(Wal)` as a no-op returning `Memory`!
    let mut mem_p = Pager::open_in_memory().unwrap();
    mem_p.begin_write().unwrap();
    assert_eq!(
        mem_p.set_journal_mode(JournalMode::Wal).unwrap(),
        JournalMode::Memory
    );
    mem_p.rollback().unwrap();
    mem_p.close().unwrap();
}
