//! Phase 2-B Integration & C SQLite 3.54.1 Differential Crash/Recovery Suite:
//! Two-Phase Super-Journal / Master-Journal Protocol (Groups 37, 63).
//!
//! Covers:
//! 1. Group 37 & 63 binary trailer layout, `-mj%06X9%02X` & 8.3 `.9XX` naming, `> 100` collision
//!    retry fallback unlink, `is_super_journal_name` arbitrary-file deletion guards, non-ASCII
//!    UTF-8 signed `i8` checksum parity against C SQLite 3.54.1, `synchronous=FULL` sector
//!    alignment, `JournalMode::Persist` tail truncation, and `read_super_journal` corrupt vs I/O
//!    error handling.
//! 2. Exact VFS file-sync / parent-dir-sync / delete ordering instrumentation and post-unlink
//!    benign Phase 2 (`cleanup_benign = true`) fault injection verified by reopening in both
//!    Rust and C SQLite 3.54.1.
//! 3. Full crash/fault matrix before and after super-journal unlink, order-independent recovery
//!    (`db1 -> db2` and `db2 -> db1`) across `Delete` and `Persist` modes, `pager_delsuper`
//!    self-membership (`bSeen`) guard, `vfs.exists(zSuper)` I/O error propagation guard, and
//!    mixed WAL / `:memory:` / `PrivateTemporary` / `synchronous=OFF` coordinator selection.
//! 4. Bidirectional C SQLite 3.54.1 <-> Rust crash interoperability using an out-of-tree C helper
//!    compiled in `target/sqlite-ref/` with a custom `sqlite3_vfs` shim snapshotting disk state
//!    immediately before and immediately after `xDelete` of `-mj...`.
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters::btree::{Btree, commit_btrees_two_phase};
use sqliters::pager::{
    JOURNAL_MAGIC, Pager, PagerOpenOptions, SUPER_JOURNAL_COLLISION_RETRY_LIMIT,
    SUPER_JOURNAL_MAX_PATH_BYTES, TransactionState, apply_8_3_suffix, create_super_journal_file,
    format_super_journal_name, is_super_journal_name, journal_hdr_offset, read_super_journal,
    super_journal_checksum,
};
use sqliters::types::{LockGeometry, Result, SqliteError, Value, encode_record, extended};
use sqliters::vfs::{
    LockLevel, MemoryVfs, OpenFlags, OsVfs, ShmOpenOptions, SyncFlags, Vfs, VfsFile, WalShm,
};
use sqliters::{Connection, JournalMode};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p2b_{prefix}_{pid}_{id}"));
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

fn copy_directory_files(src_dir: &Path, dst_dir: &Path) {
    fs::create_dir_all(dst_dir).expect("create snapshot destination dir");
    if let Ok(existing) = fs::read_dir(dst_dir) {
        for entry in existing.flatten() {
            let p = entry.path();
            if p.is_file() {
                let _ = fs::remove_file(&p);
            }
        }
    }
    for entry in fs::read_dir(src_dir).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_file() {
            let dst = dst_dir.join(entry.file_name());
            fs::copy(&path, &dst).expect("copy snapshot file");
        }
    }
}

fn find_super_journals_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(s) = p.to_str()
                && is_super_journal_name(s)
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

// ============================================================================
// Instrumented & Fault-Injecting VFS Wrapper (`ObservingFaultVfs`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum VfsOpEvent {
    Open {
        path: PathBuf,
        flags: OpenFlags,
    },
    Write {
        path: PathBuf,
        offset: u64,
        len: usize,
    },
    Truncate {
        path: PathBuf,
        size: u64,
    },
    FileSync {
        path: PathBuf,
        flags: SyncFlags,
    },
    InitialDirSync {
        path: PathBuf,
    },
    Delete {
        path: PathBuf,
        sync_dir: bool,
    },
    Exists {
        path: PathBuf,
    },
}

type PathFaultHook = Box<dyn Fn(&Path) -> Option<SqliteError> + Send + Sync>;
type DeleteObserveHook = Box<dyn Fn(&Path, bool) -> Option<SqliteError> + Send + Sync>;

#[derive(Default)]
struct FaultConfig {
    random_queue: Vec<u32>,
    fail_exists: Option<PathFaultHook>,
    fail_read: Option<PathFaultHook>,
    fail_write: Option<PathFaultHook>,
    fail_truncate: Option<PathFaultHook>,
    fail_sync: Option<PathFaultHook>,
    before_delete: Option<DeleteObserveHook>,
    after_delete: Option<DeleteObserveHook>,
}

struct ObservingFaultVfs {
    inner: Arc<dyn Vfs>,
    events: Arc<Mutex<Vec<VfsOpEvent>>>,
    faults: Arc<Mutex<FaultConfig>>,
}

impl ObservingFaultVfs {
    fn new(inner: Arc<dyn Vfs>) -> Self {
        Self {
            inner,
            events: Arc::new(Mutex::new(Vec::new())),
            faults: Arc::new(Mutex::new(FaultConfig::default())),
        }
    }

    fn clear_events(&self) {
        self.events.lock().unwrap().clear();
    }

    fn events_snapshot(&self) -> Vec<VfsOpEvent> {
        self.events.lock().unwrap().clone()
    }

    fn clear_faults(&self) {
        *self.faults.lock().unwrap() = FaultConfig::default();
    }
}

struct ObservingFaultFile {
    inner: Box<dyn VfsFile>,
    path: PathBuf,
    pending_initial_dir_sync: AtomicBool,
    events: Arc<Mutex<Vec<VfsOpEvent>>>,
    faults: Arc<Mutex<FaultConfig>>,
}

impl VfsFile for ObservingFaultFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        if let Some(ref hook) = self.faults.lock().unwrap().fail_read
            && let Some(err) = hook(&self.path)
        {
            return Err(err);
        }
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        if let Some(ref hook) = self.faults.lock().unwrap().fail_write
            && let Some(err) = hook(&self.path)
        {
            return Err(err);
        }
        self.events.lock().unwrap().push(VfsOpEvent::Write {
            path: self.path.clone(),
            offset,
            len: buf.len(),
        });
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> Result<()> {
        if let Some(ref hook) = self.faults.lock().unwrap().fail_truncate
            && let Some(err) = hook(&self.path)
        {
            return Err(err);
        }
        self.events.lock().unwrap().push(VfsOpEvent::Truncate {
            path: self.path.clone(),
            size,
        });
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        if let Some(ref hook) = self.faults.lock().unwrap().fail_sync
            && let Some(err) = hook(&self.path)
        {
            return Err(err);
        }
        self.inner.sync(flags)?;
        let mut ev = self.events.lock().unwrap();
        ev.push(VfsOpEvent::FileSync {
            path: self.path.clone(),
            flags,
        });
        if self.pending_initial_dir_sync.swap(false, Ordering::SeqCst) {
            ev.push(VfsOpEvent::InitialDirSync {
                path: self.path.clone(),
            });
        }
        Ok(())
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

    fn open_wal_shm_with_options(&self, options: ShmOpenOptions) -> Result<WalShm> {
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

impl Vfs for ObservingFaultVfs {
    fn name(&self) -> &str {
        "observing_fault_vfs"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let pbuf = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("<temp>"));
        self.events.lock().unwrap().push(VfsOpEvent::Open {
            path: pbuf.clone(),
            flags,
        });
        let inner_file = self.inner.open(path, flags)?;
        let pending_dir =
            path.is_some() && !inner_file.is_readonly() && flags.requires_initial_dir_sync();
        Ok(Box::new(ObservingFaultFile {
            inner: inner_file,
            path: pbuf,
            pending_initial_dir_sync: AtomicBool::new(pending_dir),
            events: Arc::clone(&self.events),
            faults: Arc::clone(&self.faults),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        if let Some(ref hook) = self.faults.lock().unwrap().before_delete
            && let Some(err) = hook(path, sync_dir)
        {
            return Err(err);
        }
        self.events.lock().unwrap().push(VfsOpEvent::Delete {
            path: path.to_path_buf(),
            sync_dir,
        });
        self.inner.delete(path, sync_dir)?;
        if let Some(ref hook) = self.faults.lock().unwrap().after_delete
            && let Some(err) = hook(path, sync_dir)
        {
            return Err(err);
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        if let Some(ref hook) = self.faults.lock().unwrap().fail_exists
            && let Some(err) = hook(path)
        {
            return Err(err);
        }
        self.events.lock().unwrap().push(VfsOpEvent::Exists {
            path: path.to_path_buf(),
        });
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        self.inner.full_pathname(path)
    }

    fn randomness(&self, buf: &mut [u8]) {
        let mut fg = self.faults.lock().unwrap();
        if !fg.random_queue.is_empty() && buf.len() == 4 {
            let next = fg.random_queue.remove(0);
            buf.copy_from_slice(&next.to_ne_bytes());
            return;
        }
        drop(fg);
        self.inner.randomness(buf);
    }
}

// ============================================================================
// Out-of-Tree C SQLite 3.54.1 Reference Helper (`target/sqlite-ref/`)
// ============================================================================

#[cfg(unix)]
const C_SUPER_JOURNAL_HELPER_SOURCE: &str = r#"
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "sqlite3.h"

static void die_msg(const char *ctx, sqlite3 *db, int rc) {
    fprintf(stderr, "C_2PC_ERR [%s] rc=%d ext=%d msg=%s\n",
            ctx, rc, db ? sqlite3_extended_errcode(db) : rc,
            db ? sqlite3_errmsg(db) : "no-db");
    if (db) sqlite3_close(db);
    exit(2);
}

static void exec_sql(sqlite3 *db, const char *sql) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "C SQL error (%d): %s\nSQL: %s\n",
                rc, errmsg ? errmsg : "", sql);
        sqlite3_free(errmsg);
        sqlite3_close(db);
        exit(2);
    }
}

static void copy_file_raw(const char *src, const char *dst) {
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0) {
        perror("open src");
        _exit(3);
    }
    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (out_fd < 0) {
        perror("open dst");
        close(in_fd);
        _exit(3);
    }
    char buf[8192];
    for (;;) {
        ssize_t n = read(in_fd, buf, sizeof(buf));
        if (n < 0) {
            perror("read copy");
            _exit(3);
        }
        if (n == 0) break;
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(out_fd, buf + off, (size_t)(n - off));
            if (w <= 0) {
                perror("write copy");
                _exit(3);
            }
            off += w;
        }
    }
    close(in_fd);
    close(out_fd);
}

static void snapshot_dir(const char *src_dir, const char *dst_dir) {
    mkdir(dst_dir, 0755);
    DIR *d = opendir(src_dir);
    if (!d) {
        perror("opendir snapshot");
        _exit(3);
    }
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {
        if (strcmp(ent->d_name, ".") == 0 || strcmp(ent->d_name, "..") == 0) continue;
        char sp[2048], dp[2048];
        snprintf(sp, sizeof(sp), "%s/%s", src_dir, ent->d_name);
        struct stat st;
        if (stat(sp, &st) == 0 && S_ISREG(st.st_mode)) {
            snprintf(dp, sizeof(dp), "%s/%s", dst_dir, ent->d_name);
            copy_file_raw(sp, dp);
        }
    }
    closedir(d);
}

static int is_mj_path(const char *zPath) {
    size_t n = strlen(zPath);
    if (n >= 12 && memcmp(zPath + n - 12, "-mj", 3) == 0 && zPath[n - 3] == '9') {
        return 1;
    }
    if (n >= 4 && zPath[n - 4] == '.' && zPath[n - 3] == '9') {
        return 1;
    }
    return 0;
}

static int dir_has_mj(const char *dir_path) {
    DIR *d = opendir(dir_path);
    if (!d) return 0;
    int found = 0;
    struct dirent *ent;
    while ((ent = readdir(d)) != NULL) {
        if (is_mj_path(ent->d_name)) {
            found = 1;
            break;
        }
    }
    closedir(d);
    return found;
}

typedef struct ShimCtx {
    sqlite3_vfs *pOrig;
    const char *work_dir;
    const char *main_jrnl_path;
    const char *snap_before_dir;
    const char *snap_after_dir;
} ShimCtx;

static ShimCtx g_shim_ctx;
static sqlite3_vfs g_shim_vfs;

static uint32_t read_be32(const unsigned char *p) {
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | (uint32_t)p[3];
}

static void inspect_and_print_trailer(const char *jrnl_path) {
    int fd = open(jrnl_path, O_RDONLY);
    if (fd < 0) return;
    off_t sz = lseek(fd, 0, SEEK_END);
    if (sz >= 20) {
        unsigned char tail16[16];
        if (pread(fd, tail16, 16, sz - 16) == 16) {
            uint32_t len = read_be32(tail16);
            uint32_t cksum = read_be32(tail16 + 4);
            if (len > 0 && (off_t)(len + 20) <= sz) {
                unsigned char pg_bytes[4];
                char *name_buf = (char *)calloc(1, (size_t)len + 1);
                if (pread(fd, pg_bytes, 4, sz - 20 - (off_t)len) == 4 &&
                    pread(fd, name_buf, len, sz - 16 - (off_t)len) == (ssize_t)len) {
                    uint32_t sj_pgno = read_be32(pg_bytes);
                    printf("TRAILER_SJ_PGNO=%u\n", sj_pgno);
                    printf("TRAILER_LEN=%u\n", len);
                    printf("TRAILER_CKSUM=%u\n", cksum);
                    printf("TRAILER_NAME=%s\n", name_buf);
                }
                free(name_buf);
            }
        }
    }
    close(fd);
}

static int shim_xOpen(sqlite3_vfs *pVfs, const char *zName, sqlite3_file *pFile, int flags, int *pOutFlags) {
    ShimCtx *ctx = (ShimCtx *)pVfs->pAppData;
    return ctx->pOrig->xOpen(ctx->pOrig, zName, pFile, flags, pOutFlags);
}

static int shim_xDelete(sqlite3_vfs *pVfs, const char *zPath, int syncDir) {
    ShimCtx *ctx = (ShimCtx *)pVfs->pAppData;
    if (zPath && is_mj_path(zPath)) {
        printf("MJ_UNLINK_PATH=%s\n", zPath);
        printf("MJ_UNLINK_SYNCDIR=%d\n", syncDir);
        if (ctx->main_jrnl_path) {
            inspect_and_print_trailer(ctx->main_jrnl_path);
        }
        if (ctx->snap_before_dir && ctx->snap_before_dir[0]) {
            snapshot_dir(ctx->work_dir, ctx->snap_before_dir);
        }
        int rc = ctx->pOrig->xDelete(ctx->pOrig, zPath, syncDir);
        if (ctx->snap_after_dir && ctx->snap_after_dir[0]) {
            snapshot_dir(ctx->work_dir, ctx->snap_after_dir);
        }
        fflush(stdout);
        /* Simulate immediate hard crash BEFORE Phase 2 cold journal cleanup! */
        _exit(0);
    }
    return ctx->pOrig->xDelete(ctx->pOrig, zPath, syncDir);
}

static void dump_db_items(const char *tag, const char *db_path, const char *watch_dir) {
    sqlite3 *db = NULL;
    int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
    if (rc != SQLITE_OK) die_msg("open_verify", db, rc);

    sqlite3_stmt *st = NULL;
    rc = sqlite3_prepare_v2(db, "PRAGMA integrity_check;", -1, &st, NULL);
    if (rc != SQLITE_OK) die_msg("prep_ic", db, rc);
    if (sqlite3_step(st) == SQLITE_ROW) {
        printf("%s_INTEGRITY=%s\n", tag, (const char *)sqlite3_column_text(st, 0));
    }
    sqlite3_finalize(st);

    rc = sqlite3_prepare_v2(db, "SELECT id, val FROM items ORDER BY id;", -1, &st, NULL);
    if (rc != SQLITE_OK) die_msg("prep_sel", db, rc);
    printf("%s_ROWS=", tag);
    int first = 1;
    while ((rc = sqlite3_step(st)) == SQLITE_ROW) {
        if (!first) printf(",");
        first = 0;
        printf("%lld:%s", sqlite3_column_int64(st, 0), (const char *)sqlite3_column_text(st, 1));
    }
    if (rc != SQLITE_DONE) die_msg("step_sel", db, rc);
    printf("\n");
    sqlite3_finalize(st);

    if (sqlite3_close(db) != SQLITE_OK) die_msg("close_verify", db, 1);
    printf("%s_MJ_PRESENT=%d\n", tag, dir_has_mj(watch_dir));
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    const char *cmd = argv[1];

    /* c-setup-pair <main_db> <aux_db> <DELETE|PERSIST> */
    if (strcmp(cmd, "c-setup-pair") == 0) {
        if (argc < 5) return 2;
        const char *main_db = argv[2];
        const char *aux_db = argv[3];
        const char *jmode = argv[4];

        for (int i = 0; i < 2; i++) {
            const char *p = (i == 0) ? main_db : aux_db;
            sqlite3 *db = NULL;
            if (sqlite3_open(p, &db) != SQLITE_OK) die_msg("open_setup", db, 1);
            char sql[512];
            snprintf(sql, sizeof(sql),
                     "PRAGMA page_size = 4096;"
                     "PRAGMA journal_mode = %s;"
                     "CREATE TABLE items(id INTEGER PRIMARY KEY, val TEXT NOT NULL);"
                     "INSERT INTO items VALUES (1, '%s');",
                     jmode, (i == 0) ? "init_main" : "init_aux");
            exec_sql(db, sql);
            if (sqlite3_close(db) != SQLITE_OK) die_msg("close_setup", db, 1);
        }
        return 0;
    }

    /* c-crash-2pc <work_dir> <main_db> <aux_db> <DELETE|PERSIST> <NORMAL|FULL> <snap_before> <snap_after> */
    if (strcmp(cmd, "c-crash-2pc") == 0) {
        if (argc < 9) return 2;
        const char *work_dir = argv[2];
        const char *main_db = argv[3];
        const char *aux_db = argv[4];
        const char *jmode = argv[5];
        const char *sync_lvl = argv[6];
        const char *snap_before = argv[7];
        const char *snap_after = argv[8];

        char main_jrnl[2048];
        snprintf(main_jrnl, sizeof(main_jrnl), "%s-journal", main_db);

        sqlite3_vfs *orig = sqlite3_vfs_find(NULL);
        if (!orig) return 2;
        g_shim_ctx.pOrig = orig;
        g_shim_ctx.work_dir = work_dir;
        g_shim_ctx.main_jrnl_path = main_jrnl;
        g_shim_ctx.snap_before_dir = snap_before;
        g_shim_ctx.snap_after_dir = snap_after;

        g_shim_vfs = *orig;
        g_shim_vfs.zName = "shim_2pc";
        g_shim_vfs.pAppData = &g_shim_ctx;
        g_shim_vfs.xOpen = shim_xOpen;
        g_shim_vfs.xDelete = shim_xDelete;
        if (sqlite3_vfs_register(&g_shim_vfs, 0) != SQLITE_OK) return 2;

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(main_db, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, "shim_2pc");
        if (rc != SQLITE_OK) die_msg("open_shim", db, rc);

        char sql[4096];
        snprintf(sql, sizeof(sql),
                 "PRAGMA journal_mode = %s;"
                 "PRAGMA synchronous = %s;"
                 "ATTACH DATABASE '%s' AS aux;"
                 "PRAGMA aux.journal_mode = %s;"
                 "PRAGMA aux.synchronous = %s;"
                 "BEGIN;"
                 "UPDATE main.items SET val = 'c_tx_main' WHERE id = 1;"
                 "INSERT INTO main.items VALUES (2, 'c_row2_main');"
                 "UPDATE aux.items SET val = 'c_tx_aux' WHERE id = 1;"
                 "INSERT INTO aux.items VALUES (2, 'c_row2_aux');"
                 "COMMIT;",
                 jmode, sync_lvl, aux_db, jmode, sync_lvl);
        exec_sql(db, sql);
        sqlite3_close(db);
        return 0;
    }

    /* c-recover-and-verify <watch_dir> <first_db> <second_db> */
    if (strcmp(cmd, "c-recover-and-verify") == 0) {
        if (argc < 5) return 2;
        const char *watch_dir = argv[2];
        const char *first_db = argv[3];
        const char *second_db = argv[4];
        dump_db_items("FIRST", first_db, watch_dir);
        dump_db_items("SECOND", second_db, watch_dir);
        fflush(stdout);
        return 0;
    }

    fprintf(stderr, "Unknown command: %s\n", cmd);
    return 2;
}
"#;

#[cfg(unix)]
fn c_2pc_helper_bin() -> &'static Path {
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
                "failed building out-of-tree sqlite3.c:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let sqlite3_o = ref_root.join("sqlite3.o");
        if !sqlite3_o.exists() {
            let tmp_o = ref_root.join(format!("sqlite3_2pc.{}.o.tmp", std::process::id()));
            let cc_o = Command::new("cc")
                .args(["-O2", "-pthread", "-c"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&sqlite3_c)
                .arg("-o")
                .arg(&tmp_o)
                .output()
                .expect("compile sqlite3.o");
            assert!(
                cc_o.status.success(),
                "failed compiling sqlite3.o:\n{}",
                String::from_utf8_lossy(&cc_o.stderr)
            );
            let _ = fs::rename(&tmp_o, &sqlite3_o);
        }

        let helper_c = ref_root.join("c_super_journal_2pc_helper.c");
        let helper_bin = ref_root.join("c_super_journal_2pc_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_SUPER_JOURNAL_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            let tmp_c = ref_root.join(format!(
                "c_super_journal_2pc_helper.{}.c",
                std::process::id()
            ));
            let tmp_bin = ref_root.join(format!(
                "c_super_journal_2pc_helper.{}.tmp",
                std::process::id()
            ));
            fs::write(&tmp_c, C_SUPER_JOURNAL_HELPER_SOURCE).expect("write temp helper.c");
            let cc_bin = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&tmp_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&tmp_bin)
                .output()
                .expect("compile c_super_journal_2pc_helper");
            let _ = fs::remove_file(&tmp_c);
            assert!(
                cc_bin.status.success(),
                "failed compiling c_super_journal_2pc_helper:\n{}",
                String::from_utf8_lossy(&cc_bin.stderr)
            );
            fs::write(&helper_c, C_SUPER_JOURNAL_HELPER_SOURCE)
                .expect("write c_super_journal_2pc_helper.c");
            fs::rename(&tmp_bin, &helper_bin).expect("rename c_super_journal_2pc_helper");
        }

        let git_check = Command::new("git")
            .args(["status", "--short", "--", "sqlite"])
            .current_dir(workspace_root)
            .output()
            .expect("git status check on ./sqlite");
        assert!(
            git_check.stdout.is_empty(),
            "./sqlite must stay completely untouched, found:\n{}",
            String::from_utf8_lossy(&git_check.stdout)
        );

        helper_bin
    })
}

fn parse_kv_stdout(stdout: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_owned(), v.trim().to_owned());
        }
    }
    map
}

// ============================================================================
// 1. Group 37 & 63 Binary Format, Naming, 8.3 Suffix, Collision Retry & Checksum
// ============================================================================

#[test]
fn test_group37_63_naming_8_3_suffix_collision_retry_and_name_guards() {
    // Standard `-mj%06X9%02X` formatting & antipenultimate '9' invariant
    let std_name = format_super_journal_name("/tmp/main.db", 0x7f33_1ffa, false);
    assert_eq!(std_name, "/tmp/main.db-mj7F331F9FA");
    assert_eq!(std_name.as_bytes()[std_name.len() - 3], b'9');
    assert!(is_super_journal_name(&std_name));

    // 8.3 suffix shortening (`sqlite3FileSuffix3` in `util.c:2036-2046`)
    let short_name = format_super_journal_name("/tmp/test.db", 0x7f33_1ffa, true);
    assert_eq!(short_name, "/tmp/test.9FA");
    assert!(is_super_journal_name(&short_name));

    // Dot in parent directory with extensionless basename must NOT truncate at parent directory dot!
    let dir_dot = format_super_journal_name("/tmp/rel.v1/main", 0x0102_03ab, true);
    assert_eq!(dir_dot, "/tmp/rel.v1/main-mj0102039AB");
    assert!(is_super_journal_name(&dir_dot));

    // Windows backslash separator stops backward dot scan just like '/'
    let win_sep = format_super_journal_name(r"C:\dir.ext\main", 0x0102_03cd, true);
    assert_eq!(win_sep, r"C:\dir.ext\main-mj0102039CD");
    assert!(is_super_journal_name(&win_sep));

    let mut suffix_already_short = String::from("foo.abc");
    apply_8_3_suffix(&mut suffix_already_short);
    assert_eq!(suffix_already_short, "foo.abc");

    // Negative controls on `is_super_journal_name` (prevents arbitrary file deletion in `pager_delsuper`!)
    for invalid in [
        "",
        "9AB",
        ".8AB",
        "foo.8AB",
        "foo.9GZ",
        "foo.db",
        "foo.db-journal",
        "foo.db-wal",
        "/etc/passwd",
        "foo-mj1234568AB",       // antipenultimate is '8', not '9'
        "foo-mj12345G9AB",       // non-hex 'G'
        "foo-mj123459AB",        // 1 digit short
        "foo.db-mj1234569AB\0x", // embedded NUL
    ] {
        assert!(
            !is_super_journal_name(invalid),
            "expected {invalid:?} to be rejected by is_super_journal_name"
        );
    }

    // Collision retry (`> 100` collisions unlinks stale candidate and opens fresh exclusive handle)
    let mem_vfs = Arc::new(MemoryVfs::new());
    let obs_vfs = Arc::new(ObservingFaultVfs::new(mem_vfs.clone()));
    let colliding_name = format_super_journal_name("main.db", 0, false);
    assert_eq!(colliding_name, "main.db-mj000000900");

    {
        let mut stale = mem_vfs
            .open(
                Some(Path::new(&colliding_name)),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::SUPER_JOURNAL,
            )
            .unwrap();
        stale.write_at(b"stale_garbage", 0).unwrap();
        stale.close().unwrap();
    }

    // Queue 105 zeros so all 101 collision probes hit `main.db-mj000000900`
    obs_vfs.faults.lock().unwrap().random_queue = vec![0u32; 105];
    obs_vfs.clear_events();

    let participants = [Path::new("main.db-journal"), Path::new("aux.db-journal")];
    let (chosen, mut sj_handle) =
        create_super_journal_file(&*obs_vfs, "main.db", false, &participants).unwrap();
    assert_eq!(chosen, colliding_name);
    let sz = sj_handle.file_size().unwrap() as usize;
    let mut contents = vec![0u8; sz];
    assert_eq!(sj_handle.read_at(&mut contents, 0).unwrap(), sz);
    assert_eq!(&contents, b"main.db-journal\0aux.db-journal\0");
    sj_handle.close().unwrap();

    let events = obs_vfs.events_snapshot();
    let exists_probes = events
        .iter()
        .filter(|e| matches!(e, VfsOpEvent::Exists { path } if path == Path::new(&colliding_name)))
        .count();
    assert_eq!(exists_probes, SUPER_JOURNAL_COLLISION_RETRY_LIMIT + 1);
    assert!(events.contains(&VfsOpEvent::Delete {
        path: PathBuf::from(&colliding_name),
        sync_dir: false,
    }));
}

#[test]
fn test_trailer_binary_format_non_ascii_i8_checksum_sector_align_persist_truncate_and_corrupt_guards()
 {
    // 1. Verify sector alignment helper (`journalHdrOffset` in `pager.c:1403-1413`)
    assert_eq!(journal_hdr_offset(0, 512), 0);
    assert_eq!(journal_hdr_offset(1, 512), 512);
    assert_eq!(journal_hdr_offset(100, 512), 512);
    assert_eq!(journal_hdr_offset(512, 512), 512);
    assert_eq!(journal_hdr_offset(513, 512), 1024);

    // 2. Differential non-ASCII UTF-8 signed `i8` checksum parity against C SQLite 3.54.1!
    #[cfg(unix)]
    {
        let sandbox = TempSandbox::new("utf8_cksum");
        let main_db = sandbox.path().join("münchen_ğüşiöç.db");
        let aux_db = sandbox.path().join("aux_éàü.db");
        let snap_before = sandbox.path().join("snap_before");
        let snap_after = sandbox.path().join("snap_after");

        let helper = c_2pc_helper_bin();
        let setup_out = Command::new(helper)
            .arg("c-setup-pair")
            .arg(&main_db)
            .arg(&aux_db)
            .arg("DELETE")
            .output()
            .expect("run c-setup-pair");
        assert!(
            setup_out.status.success(),
            "c-setup-pair stderr: {}",
            String::from_utf8_lossy(&setup_out.stderr)
        );

        let crash_out = Command::new(helper)
            .arg("c-crash-2pc")
            .arg(sandbox.path())
            .arg(&main_db)
            .arg(&aux_db)
            .arg("DELETE")
            .arg("FULL")
            .arg(&snap_before)
            .arg(&snap_after)
            .output()
            .expect("run c-crash-2pc");
        assert!(
            crash_out.status.success(),
            "c-crash-2pc stderr: {}",
            String::from_utf8_lossy(&crash_out.stderr)
        );
        let kv = parse_kv_stdout(&String::from_utf8_lossy(&crash_out.stdout));
        let c_trailer_name = kv.get("TRAILER_NAME").expect("TRAILER_NAME from C");
        let c_cksum: u32 = kv
            .get("TRAILER_CKSUM")
            .expect("TRAILER_CKSUM from C")
            .parse()
            .unwrap();
        let c_sj_pgno: u32 = kv
            .get("TRAILER_SJ_PGNO")
            .expect("TRAILER_SJ_PGNO from C")
            .parse()
            .unwrap();
        assert_eq!(kv.get("MJ_UNLINK_SYNCDIR").map(String::as_str), Some("1"));
        assert!(
            c_trailer_name.as_bytes().iter().any(|&b| b >= 0x80),
            "expected non-ASCII UTF-8 bytes in super-journal name: {c_trailer_name}"
        );
        assert_eq!(
            super_journal_checksum(c_trailer_name.as_bytes()),
            c_cksum,
            "signed i8 checksum must match C SQLite 3.54.1 bit-for-bit on non-ASCII UTF-8"
        );
        assert_eq!(c_sj_pgno, LockGeometry::PRODUCTION.sj_pgno(4096));

        // Verify Rust `read_super_journal` decodes C's non-ASCII journal trailer directly!
        let os_vfs = OsVfs::new();
        let snapped_jrnl = snap_before.join("münchen_ğüşiöç.db-journal");
        let mut jf = os_vfs
            .open(
                Some(&snapped_jrnl),
                OpenFlags::READONLY | OpenFlags::SUPER_JOURNAL,
            )
            .unwrap();
        let decoded = read_super_journal(&*jf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap();
        jf.close().unwrap();
        assert_eq!(decoded.as_deref(), Some(c_trailer_name.as_str()));
    }

    // 3. Verify exact byte layout, `synchronous=FULL` sector alignment, `TEST_64K` `PAGER_SJ_PGNO`,
    //    and `JournalMode::Persist` truncation of stale trailing journal bytes!
    for (geom, page_sz, sync_level) in [
        (LockGeometry::PRODUCTION, 4096u32, 2u8), // FULL: sector-aligned trailer
        (LockGeometry::PRODUCTION, 4096u32, 1u8), // NORMAL: unaligned trailer
        (LockGeometry::TEST_64K, 1024u32, 2u8),   // TEST_64K: sj_pgno == 65
    ] {
        let mem_vfs = Arc::new(MemoryVfs::with_lock_geometry(geom));
        let opts = PagerOpenOptions {
            default_page_size: page_sz,
            lock_geometry: Some(geom),
            ..Default::default()
        };

        let mut pager = Pager::open_with_options(
            mem_vfs.clone(),
            Some(Path::new("fmt.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            opts,
        )
        .unwrap();
        pager.transition_journal_mode(JournalMode::Persist).unwrap();
        pager.set_synchronous(sync_level);

        // Transaction 1: allocate 6 pages and modify all of them so `fmt.db-journal` is large!
        pager.begin_write().unwrap();
        for _ in 2..=6 {
            let pg = pager.allocate_raw_page().unwrap();
            pager.write_page(pg).unwrap()[0..4].copy_from_slice(b"INIT");
        }
        pager.commit().unwrap();

        let jpath = Path::new("fmt.db-journal");
        // Mutate all 6 pages in Tx 2 so `fmt.db-journal` grows to 6 page records:
        pager.begin_write().unwrap();
        for pg in 1..=6u32 {
            if pg != pager.lock_byte_page() {
                pager.write_page(pg).unwrap()[100..104].copy_from_slice(b"BIG!");
            }
        }
        pager.commit().unwrap();
        let big_jrnl_size = {
            let mut jf = mem_vfs.open(Some(jpath), OpenFlags::READONLY).unwrap();
            let sz = jf.file_size().unwrap();
            jf.close().unwrap();
            sz
        };
        assert!(big_jrnl_size > u64::from(page_sz) * 4);

        // Transaction 3: touch ONLY Page 2 and run `commit_phase_one(Some(z_super))`!
        // `write_super_journal` MUST truncate the stale `Persist` tail so `read_super_journal`
        // finds the trailer at EOF!
        let z_super = "fmt_ümlaut.db-mjABCDEF901";
        pager.begin_write().unwrap();
        pager.write_page(2).unwrap()[0..4].copy_from_slice(b"TX3!");
        pager.commit_phase_one(Some(z_super)).unwrap();

        let raw_jrnl = {
            let mut jf = mem_vfs.open(Some(jpath), OpenFlags::READONLY).unwrap();
            let sz = jf.file_size().unwrap() as usize;
            let mut b = vec![0u8; sz];
            assert_eq!(jf.read_at(&mut b, 0).unwrap(), sz);
            let parsed = read_super_journal(&*jf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap();
            assert_eq!(parsed.as_deref(), Some(z_super));
            jf.close().unwrap();
            b
        };
        assert!((raw_jrnl.len() as u64) < big_jrnl_size);

        let name_bytes = z_super.as_bytes();
        let trailer_len = name_bytes.len() + 20;
        let trailer_start = raw_jrnl.len() - trailer_len;
        if sync_level >= 2 {
            // MemoryVfs default sector_size is 4096
            assert_eq!(trailer_start % 4096, 0);
        } else {
            // 1 header (4096) + 2 records (Page 2 and Page 1, each `4 + page_sz + 4`)
            let expected_unaligned = 4096 + 2 * (4 + page_sz as usize + 4);
            assert_eq!(trailer_start, expected_unaligned);
        }

        let trailer = &raw_jrnl[trailer_start..];
        let expected_sj_pgno = geom.sj_pgno(page_sz);
        assert_eq!(
            u32::from_be_bytes(trailer[0..4].try_into().unwrap()),
            expected_sj_pgno
        );
        assert_eq!(&trailer[4..4 + name_bytes.len()], name_bytes);
        let n = name_bytes.len();
        assert_eq!(
            u32::from_be_bytes(trailer[4 + n..8 + n].try_into().unwrap()),
            n as u32
        );
        assert_eq!(
            u32::from_be_bytes(trailer[8 + n..12 + n].try_into().unwrap()),
            super_journal_checksum(name_bytes)
        );
        assert_eq!(&trailer[12 + n..20 + n], &JOURNAL_MAGIC);

        // Phase 2 in `Persist` mode with `has_super == true` MUST truncate `fmt.db-journal` to 0!
        pager.commit_phase_two(false, false).unwrap();
        let post_commit_sz = {
            let mut jf = mem_vfs.open(Some(jpath), OpenFlags::READONLY).unwrap();
            let sz = jf.file_size().unwrap();
            jf.close().unwrap();
            sz
        };
        assert_eq!(
            post_commit_sz, 0,
            "Persist mode MUST truncate journal to 0 bytes when has_super is true"
        );
        pager.close().unwrap();
    }

    // 4. `read_super_journal` corrupt trailer (`Ok(None)`) vs real I/O error (`Err`) matrix
    let base_vfs = Arc::new(MemoryVfs::new());
    let fault_vfs = Arc::new(ObservingFaultVfs::new(base_vfs));
    let test_jrnl = Path::new("corrupt_check-journal");

    let write_trailer_bytes = |name: &[u8], len_field: u32, cksum: u32, magic: [u8; 8]| {
        let mut f = fault_vfs
            .open(
                Some(test_jrnl),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_JOURNAL,
            )
            .unwrap();
        f.truncate(0).unwrap();
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0u8; 512]);
        buf.extend_from_slice(&262_145u32.to_be_bytes());
        buf.extend_from_slice(name);
        buf.extend_from_slice(&len_field.to_be_bytes());
        buf.extend_from_slice(&cksum.to_be_bytes());
        buf.extend_from_slice(&magic);
        f.write_at(&buf, 0).unwrap();
        f.close().unwrap();
    };

    let valid_name = b"ok.db-mj1122339AA";
    let valid_cksum = super_journal_checksum(valid_name);

    // Bad magic -> Ok(None)
    let mut bad_magic = JOURNAL_MAGIC;
    bad_magic[7] ^= 0xff;
    write_trailer_bytes(valid_name, valid_name.len() as u32, valid_cksum, bad_magic);
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        assert_eq!(
            read_super_journal(&*rf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap(),
            None
        );
        rf.close().unwrap();
    }

    // Bad checksum -> Ok(None)
    write_trailer_bytes(
        valid_name,
        valid_name.len() as u32,
        valid_cksum.wrapping_add(1),
        JOURNAL_MAGIC,
    );
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        assert_eq!(
            read_super_journal(&*rf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap(),
            None
        );
        rf.close().unwrap();
    }

    // Zero length & oversize length -> Ok(None)
    write_trailer_bytes(valid_name, 0, valid_cksum, JOURNAL_MAGIC);
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        assert_eq!(
            read_super_journal(&*rf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap(),
            None
        );
        rf.close().unwrap();
    }
    write_trailer_bytes(
        valid_name,
        valid_name.len() as u32,
        valid_cksum,
        JOURNAL_MAGIC,
    );
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        assert_eq!(read_super_journal(&*rf, 10).unwrap(), None);
        rf.close().unwrap();
    }

    // Valid magic & checksum, but NOT a super-journal name -> Ok(None)
    let non_sj = b"/tmp/important_user_file.txt";
    write_trailer_bytes(
        non_sj,
        non_sj.len() as u32,
        super_journal_checksum(non_sj),
        JOURNAL_MAGIC,
    );
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        assert_eq!(
            read_super_journal(&*rf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap(),
            None
        );
        rf.close().unwrap();
    }

    // Real VFS read I/O error MUST propagate `Err(...)` (never `Ok(None)`)!
    write_trailer_bytes(
        valid_name,
        valid_name.len() as u32,
        valid_cksum,
        JOURNAL_MAGIC,
    );
    fault_vfs.faults.lock().unwrap().fail_read = Some(Box::new(|_| {
        Some(SqliteError::with_extended(
            extended::SQLITE_IOERR_READ,
            "injected trailer read error",
        ))
    }));
    {
        let mut rf = fault_vfs
            .open(Some(test_jrnl), OpenFlags::READONLY)
            .unwrap();
        let err = read_super_journal(&*rf, SUPER_JOURNAL_MAX_PATH_BYTES).unwrap_err();
        assert_eq!(err.extended_code, extended::SQLITE_IOERR_READ);
        rf.close().unwrap();
    }
}

// ============================================================================
// 2. Sync Ordering & Post-Unlink Benign Phase 2 Fault Injection
// ============================================================================

#[test]
fn test_sync_ordering_and_post_unlink_benign_phase2_fault_c_and_rust_reopen() {
    let sandbox = TempSandbox::new("sync_order");
    let db1_path = sandbox.path().join("db1.db");
    let db2_path = sandbox.path().join("db2.db");

    // Initialize `db1` and `db2` with SQL tables via `Connection` so C SQLite can query them too!
    for (p, label) in [(&db1_path, "init1"), (&db2_path, "init2")] {
        let conn = Connection::open(p).unwrap();
        conn.execute(&format!(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, val TEXT NOT NULL);
             INSERT INTO items VALUES (1, '{label}');"
        ))
        .unwrap();
        conn.try_close().unwrap();
    }

    let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
    let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs));

    let mut bt1 = Btree::open_with_vfs(
        obs_vfs.clone(),
        Some(&db1_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    let mut bt2 = Btree::open_with_vfs(
        obs_vfs.clone(),
        Some(&db2_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
    )
    .unwrap();

    obs_vfs.clear_events();

    bt1.begin_write().unwrap();
    bt2.begin_write().unwrap();
    bt1.insert_table(
        2,
        2,
        &encode_record(&[Value::Integer(2), Value::Text("tx1_db1".into())]),
    )
    .unwrap();
    bt2.insert_table(
        2,
        2,
        &encode_record(&[Value::Integer(2), Value::Text("tx1_db2".into())]),
    )
    .unwrap();

    let main_canon = bt1.db_path().unwrap().to_path_buf();
    let j1_canon = bt1.journal_path().unwrap().to_path_buf();
    let j2_canon = bt2.journal_path().unwrap().to_path_buf();
    let db2_canon = bt2.db_path().unwrap().to_path_buf();

    commit_btrees_two_phase(
        &*obs_vfs,
        Some(&main_canon),
        false,
        &mut [&mut bt1, &mut bt2],
        &[false, false],
    )
    .unwrap();

    let events = obs_vfs.events_snapshot();
    let find_pos = |pred: &dyn Fn(&VfsOpEvent) -> bool| -> usize {
        events
            .iter()
            .position(pred)
            .expect("expected VFS event in commit sequence")
    };

    let idx_j1_first_write =
        find_pos(&|e| matches!(e, VfsOpEvent::Write { path, .. } if path == &j1_canon));
    let idx_j2_first_write =
        find_pos(&|e| matches!(e, VfsOpEvent::Write { path, .. } if path == &j2_canon));
    let idx_sj_open = find_pos(&|e| {
        matches!(e, VfsOpEvent::Open { flags, .. }
            if flags.contains(OpenFlags::SUPER_JOURNAL)
                && flags.contains(OpenFlags::CREATE)
                && flags.contains(OpenFlags::EXCLUSIVE))
    });
    let sj_path = match &events[idx_sj_open] {
        VfsOpEvent::Open { path, .. } => path.clone(),
        _ => unreachable!(),
    };
    let idx_sj_write =
        find_pos(&|e| matches!(e, VfsOpEvent::Write { path, .. } if path == &sj_path));
    let idx_sj_fsync =
        find_pos(&|e| matches!(e, VfsOpEvent::FileSync { path, .. } if path == &sj_path));
    let idx_sj_dirsync =
        find_pos(&|e| matches!(e, VfsOpEvent::InitialDirSync { path } if path == &sj_path));
    let idx_j1_sync =
        find_pos(&|e| matches!(e, VfsOpEvent::FileSync { path, .. } if path == &j1_canon));
    let idx_j1_dirsync =
        find_pos(&|e| matches!(e, VfsOpEvent::InitialDirSync { path } if path == &j1_canon));
    let idx_db1_write =
        find_pos(&|e| matches!(e, VfsOpEvent::Write { path, .. } if path == &main_canon));
    let idx_db1_sync =
        find_pos(&|e| matches!(e, VfsOpEvent::FileSync { path, .. } if path == &main_canon));
    let idx_j2_sync =
        find_pos(&|e| matches!(e, VfsOpEvent::FileSync { path, .. } if path == &j2_canon));
    let idx_db2_write =
        find_pos(&|e| matches!(e, VfsOpEvent::Write { path, .. } if path == &db2_canon));
    let idx_db2_sync =
        find_pos(&|e| matches!(e, VfsOpEvent::FileSync { path, .. } if path == &db2_canon));
    let idx_sj_unlink =
        find_pos(&|e| matches!(e, VfsOpEvent::Delete { path, sync_dir: true } if path == &sj_path));
    let idx_j1_unlink =
        find_pos(&|e| matches!(e, VfsOpEvent::Delete { path, .. } if path == &j1_canon));
    let idx_j2_unlink =
        find_pos(&|e| matches!(e, VfsOpEvent::Delete { path, .. } if path == &j2_canon));

    // Assert exact durability ordering!
    assert!(idx_j1_first_write < idx_sj_open);
    assert!(idx_j2_first_write < idx_sj_open);
    assert!(idx_sj_open < idx_sj_write);
    assert!(idx_sj_write < idx_sj_fsync);
    assert!(idx_sj_fsync < idx_sj_dirsync);
    assert!(idx_sj_dirsync < idx_j1_sync);
    assert!(idx_j1_sync < idx_j1_dirsync);
    assert!(idx_j1_dirsync < idx_db1_write);
    assert!(idx_db1_write < idx_db1_sync);
    assert!(idx_sj_dirsync < idx_j2_sync);
    assert!(idx_j2_sync < idx_db2_write);
    assert!(idx_db2_write < idx_db2_sync);
    assert!(idx_db1_sync < idx_sj_unlink);
    assert!(idx_db2_sync < idx_sj_unlink);
    assert!(idx_sj_unlink < idx_j1_unlink);
    assert!(idx_sj_unlink < idx_j2_unlink);

    // Now inject an I/O fault during post-unlink cold journal cleanup (`cleanup_benign = true`)!
    // Unlinking `-mj` succeeds, then deleting `db1-journal` and `db2-journal` fails with `IOERR_DELETE`.
    obs_vfs.faults.lock().unwrap().before_delete = Some(Box::new(|path, _sync_dir| {
        let s = path.to_str().unwrap_or("");
        if s.ends_with("-journal") {
            Some(SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                "injected cold journal delete failure after super-journal unlink",
            ))
        } else {
            None
        }
    }));

    bt1.begin_write().unwrap();
    bt2.begin_write().unwrap();
    bt1.insert_table(
        2,
        3,
        &encode_record(&[Value::Integer(3), Value::Text("tx2_db1".into())]),
    )
    .unwrap();
    bt2.insert_table(
        2,
        3,
        &encode_record(&[Value::Integer(3), Value::Text("tx2_db2".into())]),
    )
    .unwrap();

    // Commit MUST still return `Ok(())` and transition both Btrees cleanly out of `Write`!
    commit_btrees_two_phase(
        &*obs_vfs,
        Some(&main_canon),
        false,
        &mut [&mut bt1, &mut bt2],
        &[false, false],
    )
    .unwrap();
    assert_eq!(bt1.transaction_state(), TransactionState::Open);
    assert_eq!(bt2.transaction_state(), TransactionState::Open);
    obs_vfs.clear_faults();
    bt1.close().unwrap();
    bt2.close().unwrap();

    // Both cold journals (`db1-journal`, `db2-journal`) remain on disk, while `-mj` is gone!
    assert!(j1_canon.exists());
    assert!(j2_canon.exists());
    assert!(find_super_journals_in_dir(sandbox.path()).is_empty());

    // Copy to two sandboxes and verify NEITHER Rust NOR C SQLite 3.54.1 rolls back the committed tx!
    let rust_reopen_dir = sandbox.path().join("reopen_rust");
    let c_reopen_dir = sandbox.path().join("reopen_c");
    copy_directory_files(sandbox.path(), &rust_reopen_dir);
    copy_directory_files(sandbox.path(), &c_reopen_dir);

    // Rust reopen check
    for (fname, expected_row3) in [("db1.db", "tx2_db1"), ("db2.db", "tx2_db2")] {
        let p = rust_reopen_dir.join(fname);
        let conn = Connection::open(&p).unwrap();
        let res = conn
            .query("SELECT id, val FROM items ORDER BY id;", &[])
            .unwrap();
        assert_eq!(res.rows.len(), 3);
        assert_eq!(res.rows[2].get_str(1), Some(expected_row3));
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(ic.rows[0].get_str(0), Some("ok"));
        conn.try_close().unwrap();
        assert!(!rust_reopen_dir.join(format!("{fname}-journal")).exists());
    }

    // C SQLite 3.54.1 reopen check
    #[cfg(unix)]
    {
        let out = Command::new(c_2pc_helper_bin())
            .arg("c-recover-and-verify")
            .arg(&c_reopen_dir)
            .arg(c_reopen_dir.join("db2.db"))
            .arg(c_reopen_dir.join("db1.db"))
            .output()
            .expect("run c-recover-and-verify");
        assert!(
            out.status.success(),
            "C verify stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let kv = parse_kv_stdout(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(kv.get("FIRST_INTEGRITY").map(String::as_str), Some("ok"));
        assert_eq!(
            kv.get("FIRST_ROWS").map(String::as_str),
            Some("1:init2,2:tx1_db2,3:tx2_db2")
        );
        assert_eq!(kv.get("SECOND_INTEGRITY").map(String::as_str), Some("ok"));
        assert_eq!(
            kv.get("SECOND_ROWS").map(String::as_str),
            Some("1:init1,2:tx1_db1,3:tx2_db1")
        );
        assert!(!c_reopen_dir.join("db1.db-journal").exists());
        assert!(!c_reopen_dir.join("db2.db-journal").exists());
    }
}

// ============================================================================
// 3. Crash / Fault Matrix, Order-Independent `pager_delsuper`, `bSeen` & `exists` I/O Guard
// ============================================================================

#[test]
fn test_crash_matrix_pager_delsuper_order_independence_bseen_and_exists_ioerr_guard() {
    // --- Subtest 3A: Crash BEFORE `-mj` is synced ---
    {
        let mem_vfs = Arc::new(MemoryVfs::new());
        let obs_vfs = Arc::new(ObservingFaultVfs::new(mem_vfs.clone()));
        let mut bt1 = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(Path::new("a1.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        let mut bt2 = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(Path::new("a2.db")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();

        bt1.begin_write().unwrap();
        let r1 = bt1.create_table().unwrap();
        bt1.insert_table(r1, 1, &encode_record(&[Value::Text("v1".into())]))
            .unwrap();
        bt1.commit().unwrap();

        bt2.begin_write().unwrap();
        let r2 = bt2.create_table().unwrap();
        bt2.insert_table(r2, 1, &encode_record(&[Value::Text("v2".into())]))
            .unwrap();
        bt2.commit().unwrap();

        // Inject sync failure specifically on the `-mj` super-journal file
        obs_vfs.faults.lock().unwrap().fail_sync = Some(Box::new(|path| {
            let s = path.to_str().unwrap_or("");
            if is_super_journal_name(s) {
                Some(SqliteError::with_extended(
                    extended::SQLITE_IOERR_FSYNC,
                    "injected super-journal fsync failure",
                ))
            } else {
                None
            }
        }));

        bt1.begin_write().unwrap();
        bt2.begin_write().unwrap();
        bt1.insert_table(r1, 2, &encode_record(&[Value::Text("uncommitted1".into())]))
            .unwrap();
        bt2.insert_table(r2, 2, &encode_record(&[Value::Text("uncommitted2".into())]))
            .unwrap();

        let err = commit_btrees_two_phase(
            &*obs_vfs,
            Some(Path::new("a1.db")),
            false,
            &mut [&mut bt1, &mut bt2],
            &[false, false],
        )
        .unwrap_err();
        assert_eq!(err.extended_code, extended::SQLITE_IOERR_FSYNC);
        obs_vfs.clear_faults();

        // Drop without calling rollback to simulate hard crash; reopen and verify independent rollback!
        drop(bt1);
        drop(bt2);

        let mut check1 = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(Path::new("a1.db")),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        let mut check2 = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(Path::new("a2.db")),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        check1.begin_read().unwrap();
        check2.begin_read().unwrap();
        assert_eq!(check1.scan_table(r1).unwrap().len(), 1);
        assert_eq!(check2.scan_table(r2).unwrap().len(), 1);
        check1.close().unwrap();
        check2.close().unwrap();
    }

    // --- Subtest 3B: Crash AFTER participant 1 completed `commit_phase_one(Some(zSuper))`
    //     and BEFORE participant 2 completed `commit_phase_one` (`-mj` still on disk) ---
    //     Verified across (Delete, Persist) x (open db1->db2, open db2->db1)!
    for jmode in [JournalMode::Delete, JournalMode::Persist] {
        for db1_first in [true, false] {
            let sandbox = TempSandbox::new("mid_phase1_crash");
            let p1 = sandbox.path().join("p1.db");
            let p2 = sandbox.path().join("p2.db");
            let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
            let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs.clone()));

            let (r1, r2, p1_canon, p2_canon) = {
                let mut bt1 = Btree::open_with_vfs(
                    obs_vfs.clone(),
                    Some(&p1),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
                )
                .unwrap();
                let mut bt2 = Btree::open_with_vfs(
                    obs_vfs.clone(),
                    Some(&p2),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
                )
                .unwrap();
                bt1.set_journal_mode(jmode).unwrap();
                bt2.set_journal_mode(jmode).unwrap();

                bt1.begin_write().unwrap();
                let r1 = bt1.create_table().unwrap();
                bt1.insert_table(r1, 1, &encode_record(&[Value::Text("orig_p1".into())]))
                    .unwrap();
                bt1.commit().unwrap();

                bt2.begin_write().unwrap();
                let r2 = bt2.create_table().unwrap();
                bt2.insert_table(r2, 1, &encode_record(&[Value::Text("orig_p2".into())]))
                    .unwrap();
                bt2.commit().unwrap();

                let c1 = bt1.db_path().unwrap().to_path_buf();
                let c2 = bt2.db_path().unwrap().to_path_buf();

                // Start multi-DB write tx
                bt1.begin_write().unwrap();
                bt2.begin_write().unwrap();
                bt1.insert_table(r1, 2, &encode_record(&[Value::Text("abort_p1".into())]))
                    .unwrap();
                bt2.insert_table(r2, 2, &encode_record(&[Value::Text("abort_p2".into())]))
                    .unwrap();

                // Fail `p2.db` sync in `commit_phase_one` AFTER `p1.db` already finished Phase 1
                // AND AFTER `p2.db-journal` already wrote its `-mj` trailer & synced!
                let c2_match = c2.clone();
                obs_vfs.faults.lock().unwrap().fail_sync = Some(Box::new(move |path| {
                    if path == c2_match {
                        Some(SqliteError::with_extended(
                            extended::SQLITE_IOERR_FSYNC,
                            "injected participant 2 db sync crash",
                        ))
                    } else {
                        None
                    }
                }));

                let err = commit_btrees_two_phase(
                    &*obs_vfs,
                    Some(&c1),
                    false,
                    &mut [&mut bt1, &mut bt2],
                    &[false, false],
                )
                .unwrap_err();
                assert_eq!(err.extended_code, extended::SQLITE_IOERR_FSYNC);

                // Snapshot the exact mid-crash disk state BEFORE `Drop` could roll back in-process!
                let snap = sandbox.path().join("crash_snap");
                copy_directory_files(sandbox.path(), &snap);
                obs_vfs.clear_faults();
                let _ = bt1.close();
                let _ = bt2.close();
                (r1, r2, snap.join("p1.db"), snap.join("p2.db"))
            };

            let snap_dir = p1_canon.parent().unwrap();
            let mjs_before = find_super_journals_in_dir(snap_dir);
            assert_eq!(
                mjs_before.len(),
                1,
                "super-journal MUST remain on disk when Phase 1 fails"
            );
            // Note: the `-mj` file inside `crash_snap` stores paths from `sandbox.path()`, so
            // restore the snapshot files back into `sandbox.path()` so journal paths in `-mj` match!
            copy_directory_files(snap_dir, sandbox.path());
            assert_eq!(find_super_journals_in_dir(sandbox.path()).len(), 1);

            let (first_path, first_root, first_orig, second_path, second_root, second_orig) =
                if db1_first {
                    (&p1, r1, "orig_p1", &p2, r2, "orig_p2")
                } else {
                    (&p2, r2, "orig_p2", &p1, r1, "orig_p1")
                };

            // 1. Open FIRST DB: must roll back to `first_orig` AND preserve `-mj` for the sibling!
            let mut rec1 = Btree::open_with_vfs(
                os_vfs.clone(),
                Some(first_path),
                OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            rec1.pager_mut().set_journal_mode(jmode).unwrap();
            rec1.begin_read().unwrap();
            let rows1 = rec1.scan_table(first_root).unwrap();
            assert_eq!(rows1.len(), 1);
            assert_eq!(
                sqliters::types::decode_record(&rows1[0].1).unwrap()[0],
                Value::Text(first_orig.into())
            );
            assert_eq!(
                rec1.integrity_check(&[1, first_root]).unwrap(),
                Vec::<String>::new()
            );
            rec1.close().unwrap();

            assert_eq!(
                find_super_journals_in_dir(sandbox.path()).len(),
                1,
                "pager_delsuper MUST preserve `-mj` while sibling journal still points to it (mode={jmode:?}, db1_first={db1_first})"
            );
            if jmode == JournalMode::Persist {
                let j1_file = PathBuf::from(format!("{}-journal", first_path.display()));
                assert!(j1_file.exists());
                assert_eq!(fs::metadata(&j1_file).unwrap().len(), 0);
            }

            // 2. Open SECOND DB: must STILL see `-mj`, roll back to `second_orig`, AND delete `-mj`!
            let mut rec2 = Btree::open_with_vfs(
                os_vfs.clone(),
                Some(second_path),
                OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            rec2.pager_mut().set_journal_mode(jmode).unwrap();
            rec2.begin_read().unwrap();
            let rows2 = rec2.scan_table(second_root).unwrap();
            assert_eq!(rows2.len(), 1);
            assert_eq!(
                sqliters::types::decode_record(&rows2[0].1).unwrap()[0],
                Value::Text(second_orig.into())
            );
            assert_eq!(
                rec2.integrity_check(&[1, second_root]).unwrap(),
                Vec::<String>::new()
            );
            rec2.close().unwrap();

            assert!(
                find_super_journals_in_dir(sandbox.path()).is_empty(),
                "pager_delsuper MUST delete `-mj` after the final sibling recovers (mode={jmode:?}, db1_first={db1_first})"
            );
            let _ = p2_canon;
        }
    }

    // --- Subtest 3C & 3D: Self-membership (`bSeen == false`) guard in `pager_delsuper`
    //     AND `vfs.exists(zSuper)` I/O error guard during hot-journal recovery ---
    {
        let sandbox = TempSandbox::new("bseen_and_exists_guard");
        let db_path = sandbox.path().join("guard.db");
        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs.clone()));

        let root = {
            let mut bt = Btree::open_with_vfs(
                obs_vfs.clone(),
                Some(&db_path),
                OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            bt.begin_write().unwrap();
            let r = bt.create_table().unwrap();
            bt.insert_table(r, 1, &encode_record(&[Value::Text("safe_orig".into())]))
                .unwrap();
            bt.commit().unwrap();

            // Create a super-journal file that does NOT list `guard.db-journal` (`bSeen == false`)!
            let foreign_j = sandbox.path().join("foreign.db-journal");
            let (z_super, mut sj) = create_super_journal_file(
                &*obs_vfs,
                bt.db_path().unwrap().to_str().unwrap(),
                false,
                &[foreign_j.as_path()],
            )
            .unwrap();
            sj.close().unwrap();

            // Run Phase 1 on `guard.db` pointing at `z_super`, then snapshot before Phase 2!
            bt.begin_write().unwrap();
            bt.insert_table(r, 2, &encode_record(&[Value::Text("dirty_row".into())]))
                .unwrap();
            bt.commit_phase_one(Some(&z_super)).unwrap();
            let snap = sandbox.path().join("snap_guard");
            copy_directory_files(sandbox.path(), &snap);
            let _ = bt.close();
            copy_directory_files(&snap, sandbox.path());
            let _ = fs::remove_dir_all(&snap);
            r
        };

        let mjs = find_super_journals_in_dir(sandbox.path());
        assert_eq!(mjs.len(), 1);
        let mj_path = mjs[0].clone();
        let jpath = PathBuf::from(format!("{}-journal", db_path.display()));
        assert!(jpath.exists());

        // 3D: Inject `SQLITE_IOERR_ACCESS` on `vfs.exists(&mj_path)` during hot-journal recovery!
        let mj_for_hook = mj_path.clone();
        obs_vfs.faults.lock().unwrap().fail_exists = Some(Box::new(move |path| {
            if path == mj_for_hook {
                Some(SqliteError::with_extended(
                    extended::SQLITE_IOERR_ACCESS,
                    "injected super-journal exists() I/O error",
                ))
            } else {
                None
            }
        }));

        let mut bt_guard = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(&db_path),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        let err = bt_guard.begin_read().unwrap_err();
        assert_eq!(err.extended_code, extended::SQLITE_IOERR_ACCESS);
        // Hot journal MUST still be intact on disk (never treated as committed)!
        assert!(jpath.exists());
        assert!(mj_path.exists());

        // Clear `exists()` fault and retry `begin_read()`:
        // 3C: `guard.db` rolls back to `safe_orig`, and because `mj_path` does NOT list
        // `guard.db-journal` (`bSeen == false`), `pager_delsuper` MUST NOT delete `mj_path`!
        obs_vfs.clear_faults();
        bt_guard.begin_read().unwrap();
        let rows = bt_guard.scan_table(root).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            sqliters::types::decode_record(&rows[0].1).unwrap()[0],
            Value::Text("safe_orig".into())
        );
        bt_guard.close().unwrap();

        assert!(
            !jpath.exists(),
            "own hot journal should be cleaned after rollback"
        );
        assert!(
            mj_path.exists(),
            "pager_delsuper MUST NOT delete super-journal when bSeen == false"
        );
    }

    // --- Subtest 3E: Mixed WAL + Rollback, `:memory:`, `PrivateTemporary`, and `synchronous=OFF` ---
    {
        let sandbox = TempSandbox::new("mixed_coordinator");
        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs));

        let main_p = sandbox.path().join("m.db");
        let aux_rb_p = sandbox.path().join("aux_rb.db");
        let aux_wal_p = sandbox.path().join("aux_wal.db");

        let mut bt_main = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(&main_p),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        let mut bt_temp =
            Btree::open_private_temporary(obs_vfs.clone(), PagerOpenOptions::default()).unwrap();

        // Case 1: `main (Delete, sync=FULL)` + `temp (PrivateTemporary)` -> `nTrans == 1` -> NO `-mj`!
        obs_vfs.clear_events();
        bt_main.begin_write().unwrap();
        bt_temp.begin_write().unwrap();
        let rm = bt_main.create_table().unwrap();
        let rt = bt_temp.create_table().unwrap();
        bt_main
            .insert_table(rm, 1, &encode_record(&[Value::Integer(10)]))
            .unwrap();
        bt_temp
            .insert_table(rt, 1, &encode_record(&[Value::Integer(20)]))
            .unwrap();
        assert!(bt_main.needs_super_journal_in_write_tx());
        assert!(!bt_temp.needs_super_journal_in_write_tx());

        let main_canon = bt_main.db_path().unwrap().to_path_buf();
        commit_btrees_two_phase(
            &*obs_vfs,
            Some(&main_canon),
            false,
            &mut [&mut bt_main, &mut bt_temp],
            &[false, false],
        )
        .unwrap();
        assert!(
            !obs_vfs
                .events_snapshot()
                .iter()
                .any(|e| matches!(e, VfsOpEvent::Open { flags, .. } if flags.is_super_journal())),
            "main + TEMP must NOT allocate a super-journal"
        );

        // Case 2: `main (Delete, sync=FULL)` + `aux_rb (Persist, sync=NORMAL)` + `aux_wal (WAL)`
        // `nTrans == 2 > 1` -> creates `-mj`, commits BOTH rollback DBs AND the WAL DB!
        let mut bt_rb = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(&aux_rb_p),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt_rb.set_journal_mode(JournalMode::Persist).unwrap();
        bt_rb.set_synchronous(1);

        let mut bt_wal = Btree::open_with_vfs(
            obs_vfs.clone(),
            Some(&aux_wal_p),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt_wal.set_journal_mode(JournalMode::Wal).unwrap();

        obs_vfs.clear_events();
        bt_main.begin_write().unwrap();
        bt_rb.begin_write().unwrap();
        bt_wal.begin_write().unwrap();
        let r_rb = bt_rb.create_table().unwrap();
        let r_wal = bt_wal.create_table().unwrap();
        bt_main
            .insert_table(rm, 2, &encode_record(&[Value::Integer(11)]))
            .unwrap();
        bt_rb
            .insert_table(r_rb, 1, &encode_record(&[Value::Integer(22)]))
            .unwrap();
        bt_wal
            .insert_table(r_wal, 1, &encode_record(&[Value::Integer(33)]))
            .unwrap();

        assert!(bt_main.needs_super_journal_in_write_tx());
        assert!(bt_rb.needs_super_journal_in_write_tx());
        assert!(!bt_wal.needs_super_journal_in_write_tx());

        commit_btrees_two_phase(
            &*obs_vfs,
            Some(&main_canon),
            false,
            &mut [&mut bt_main, &mut bt_rb, &mut bt_wal],
            &[false, false, false],
        )
        .unwrap();
        assert!(
            obs_vfs
                .events_snapshot()
                .iter()
                .any(|e| matches!(e, VfsOpEvent::Open { flags, .. } if flags.is_super_journal())),
            "2 durable rollback DBs + 1 WAL DB MUST coordinate via super-journal"
        );

        bt_main.begin_read().unwrap();
        bt_rb.begin_read().unwrap();
        bt_wal.begin_read().unwrap();
        assert_eq!(bt_main.scan_table(rm).unwrap().len(), 2);
        assert_eq!(bt_rb.scan_table(r_rb).unwrap().len(), 1);
        assert_eq!(bt_wal.scan_table(r_wal).unwrap().len(), 1);

        bt_main.close().unwrap();
        bt_rb.close().unwrap();
        bt_wal.close().unwrap();
        bt_temp.close().unwrap();
    }

    // --- Subtest 3F: Crash AFTER `-mj` is unlinked (`sync_dir = true`), BEFORE cold journals
    //     (`db1-journal`, `db2-journal`) are cleaned up ---
    for jmode in [JournalMode::Delete, JournalMode::Persist] {
        for db1_first in [true, false] {
            let sandbox = TempSandbox::new("post_unlink_crash");
            let work_dir = sandbox.path().join("work");
            let snap_dir = sandbox.path().join("snap_after_unlink");
            fs::create_dir_all(&work_dir).unwrap();
            let p1 = work_dir.join("u1.db");
            let p2 = work_dir.join("u2.db");
            let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
            let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs.clone()));

            let (r1, r2) = {
                let mut bt1 = Btree::open_with_vfs(
                    obs_vfs.clone(),
                    Some(&p1),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
                )
                .unwrap();
                let mut bt2 = Btree::open_with_vfs(
                    obs_vfs.clone(),
                    Some(&p2),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
                )
                .unwrap();
                bt1.set_journal_mode(jmode).unwrap();
                bt2.set_journal_mode(jmode).unwrap();

                bt1.begin_write().unwrap();
                let r1 = bt1.create_table().unwrap();
                bt1.insert_table(r1, 1, &encode_record(&[Value::Text("u1_v1".into())]))
                    .unwrap();
                bt1.commit().unwrap();

                bt2.begin_write().unwrap();
                let r2 = bt2.create_table().unwrap();
                bt2.insert_table(r2, 1, &encode_record(&[Value::Text("u2_v1".into())]))
                    .unwrap();
                bt2.commit().unwrap();

                bt1.begin_write().unwrap();
                bt2.begin_write().unwrap();
                bt1.insert_table(r1, 2, &encode_record(&[Value::Text("u1_committed".into())]))
                    .unwrap();
                bt2.insert_table(r2, 2, &encode_record(&[Value::Text("u2_committed".into())]))
                    .unwrap();

                let wd = work_dir.clone();
                let sd = snap_dir.clone();
                obs_vfs.faults.lock().unwrap().after_delete =
                    Some(Box::new(move |path, sync_dir| {
                        if let Some(s) = path.to_str()
                            && is_super_journal_name(s)
                        {
                            assert!(sync_dir);
                            copy_directory_files(&wd, &sd);
                        }
                        None
                    }));

                let c1 = bt1.db_path().unwrap().to_path_buf();
                commit_btrees_two_phase(
                    &*obs_vfs,
                    Some(&c1),
                    false,
                    &mut [&mut bt1, &mut bt2],
                    &[false, false],
                )
                .unwrap();
                obs_vfs.clear_faults();
                bt1.close().unwrap();
                bt2.close().unwrap();
                (r1, r2)
            };

            copy_directory_files(&snap_dir, &work_dir);
            assert!(find_super_journals_in_dir(&work_dir).is_empty());
            let j1 = work_dir.join("u1.db-journal");
            let j2 = work_dir.join("u2.db-journal");
            assert!(j1.exists() && fs::metadata(&j1).unwrap().len() > 0);
            assert!(j2.exists() && fs::metadata(&j2).unwrap().len() > 0);

            let order = if db1_first {
                [
                    (&p1, &j1, r1, "u1_committed"),
                    (&p2, &j2, r2, "u2_committed"),
                ]
            } else {
                [
                    (&p2, &j2, r2, "u2_committed"),
                    (&p1, &j1, r1, "u1_committed"),
                ]
            };

            for (db_path, jrnl_path, root, expected_second_row) in order {
                let mut rec = Btree::open_with_vfs(
                    os_vfs.clone(),
                    Some(db_path),
                    OpenFlags::READWRITE | OpenFlags::MAIN_DB,
                )
                .unwrap();
                rec.pager_mut().set_journal_mode(jmode).unwrap();
                rec.begin_read().unwrap();
                let rows = rec.scan_table(root).unwrap();
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    sqliters::types::decode_record(&rows[1].1).unwrap()[0],
                    Value::Text(expected_second_row.into())
                );
                assert_eq!(
                    rec.integrity_check(&[1, root]).unwrap(),
                    Vec::<String>::new()
                );
                rec.close().unwrap();

                match jmode {
                    JournalMode::Delete => assert!(!jrnl_path.exists()),
                    JournalMode::Persist => assert_eq!(fs::metadata(jrnl_path).unwrap().len(), 0),
                    _ => unreachable!(),
                }
            }
        }
    }
}

// ============================================================================
// 4. Bidirectional C SQLite 3.54.1 <-> Rust Crash Interoperability
// ============================================================================

#[cfg(unix)]
#[test]
fn test_bidirectional_c_sqlite_3541_and_rust_2pc_crash_interoperability() {
    let helper = c_2pc_helper_bin();

    // ------------------------------------------------------------------------
    // Part A: C SQLite 3.54.1 Crash (before & after `-mj` unlink) -> Rust Recovery
    // ------------------------------------------------------------------------
    for jmode_str in ["DELETE", "PERSIST"] {
        let sandbox = TempSandbox::new(&format!("c_to_rust_{jmode_str}"));
        let work_dir = sandbox.path().join("work");
        fs::create_dir_all(&work_dir).unwrap();
        let main_db = work_dir.join("main.db");
        let aux_db = work_dir.join("aux.db");
        let snap_before = sandbox.path().join("snap_before");
        let snap_after = sandbox.path().join("snap_after");

        let setup = Command::new(helper)
            .arg("c-setup-pair")
            .arg(&main_db)
            .arg(&aux_db)
            .arg(jmode_str)
            .output()
            .unwrap();
        assert!(setup.status.success());

        let crash = Command::new(helper)
            .arg("c-crash-2pc")
            .arg(&work_dir)
            .arg(&main_db)
            .arg(&aux_db)
            .arg(jmode_str)
            .arg("FULL")
            .arg(&snap_before)
            .arg(&snap_after)
            .output()
            .unwrap();
        assert!(crash.status.success());

        // A1: Before `-mj` unlink -> Rust MUST roll back BOTH DBs regardless of open order,
        //     preserving `-mj` after the 1st DB and deleting `-mj` after the 2nd DB!
        for main_first in [true, false] {
            copy_directory_files(&snap_before, &work_dir);
            assert_eq!(find_super_journals_in_dir(&work_dir).len(), 1);

            let order = if main_first {
                [(&main_db, "init_main"), (&aux_db, "init_aux")]
            } else {
                [(&aux_db, "init_aux"), (&main_db, "init_main")]
            };

            // Open 1st DB with Rust `Connection`
            {
                let conn1 = Connection::open(order[0].0).unwrap();
                conn1
                    .execute(&format!("PRAGMA journal_mode = {jmode_str};"))
                    .unwrap();
                let q1 = conn1
                    .query("SELECT id, val FROM items ORDER BY id;", &[])
                    .unwrap();
                assert_eq!(q1.rows.len(), 1);
                assert_eq!(q1.rows[0].get_str(1), Some(order[0].1));
                let ic1 = conn1.query("PRAGMA integrity_check;", &[]).unwrap();
                assert_eq!(ic1.rows[0].get_str(0), Some("ok"));
                conn1.try_close().unwrap();
            }
            assert_eq!(
                find_super_journals_in_dir(&work_dir).len(),
                1,
                "Rust recovery of 1st C-crashed DB must preserve `-mj` (mode={jmode_str}, main_first={main_first})"
            );

            // Open 2nd DB with Rust `Connection`
            {
                let conn2 = Connection::open(order[1].0).unwrap();
                conn2
                    .execute(&format!("PRAGMA journal_mode = {jmode_str};"))
                    .unwrap();
                let q2 = conn2
                    .query("SELECT id, val FROM items ORDER BY id;", &[])
                    .unwrap();
                assert_eq!(q2.rows.len(), 1);
                assert_eq!(q2.rows[0].get_str(1), Some(order[1].1));
                let ic2 = conn2.query("PRAGMA integrity_check;", &[]).unwrap();
                assert_eq!(ic2.rows[0].get_str(0), Some("ok"));
                conn2.try_close().unwrap();
            }
            assert!(
                find_super_journals_in_dir(&work_dir).is_empty(),
                "Rust recovery of 2nd C-crashed DB must unlink `-mj` (mode={jmode_str}, main_first={main_first})"
            );
        }

        // A2: After `-mj` unlink (with cold journals still on disk!) -> Rust MUST preserve committed
        //     data in BOTH DBs regardless of open order!
        for main_first in [true, false] {
            copy_directory_files(&snap_after, &work_dir);
            assert!(find_super_journals_in_dir(&work_dir).is_empty());
            assert!(work_dir.join("main.db-journal").exists());
            assert!(work_dir.join("aux.db-journal").exists());

            let order = if main_first {
                [
                    (&main_db, "c_tx_main", "c_row2_main"),
                    (&aux_db, "c_tx_aux", "c_row2_aux"),
                ]
            } else {
                [
                    (&aux_db, "c_tx_aux", "c_row2_aux"),
                    (&main_db, "c_tx_main", "c_row2_main"),
                ]
            };

            for (dbp, expected_r1, expected_r2) in order {
                let conn = Connection::open(dbp).unwrap();
                conn.execute(&format!("PRAGMA journal_mode = {jmode_str};"))
                    .unwrap();
                let q = conn
                    .query("SELECT id, val FROM items ORDER BY id;", &[])
                    .unwrap();
                assert_eq!(q.rows.len(), 2);
                assert_eq!(q.rows[0].get_str(1), Some(expected_r1));
                assert_eq!(q.rows[1].get_str(1), Some(expected_r2));
                let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
                assert_eq!(ic.rows[0].get_str(0), Some("ok"));
                conn.try_close().unwrap();
            }
        }
    }

    // ------------------------------------------------------------------------
    // Part B: Rust 2PC Crash (before & after `-mj` unlink) -> C SQLite 3.54.1 Recovery
    // ------------------------------------------------------------------------
    for (jmode_str, jmode_enum) in [
        ("DELETE", JournalMode::Delete),
        ("PERSIST", JournalMode::Persist),
    ] {
        let sandbox = TempSandbox::new(&format!("rust_to_c_{jmode_str}"));
        let work_dir = sandbox.path().join("work");
        fs::create_dir_all(&work_dir).unwrap();
        let main_db = work_dir.join("main.db");
        let aux_db = work_dir.join("aux.db");
        let snap_before = sandbox.path().join("rust_snap_before");
        let snap_after = sandbox.path().join("rust_snap_after");

        let setup = Command::new(helper)
            .arg("c-setup-pair")
            .arg(&main_db)
            .arg(&aux_db)
            .arg(jmode_str)
            .output()
            .unwrap();
        assert!(setup.status.success());

        {
            let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
            let obs_vfs = Arc::new(ObservingFaultVfs::new(os_vfs));

            let mut bt_main = Btree::open_with_vfs(
                obs_vfs.clone(),
                Some(&main_db),
                OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            let mut bt_aux = Btree::open_with_vfs(
                obs_vfs.clone(),
                Some(&aux_db),
                OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            )
            .unwrap();
            bt_main.set_journal_mode(jmode_enum).unwrap();
            bt_aux.set_journal_mode(jmode_enum).unwrap();

            bt_main.begin_write().unwrap();
            bt_aux.begin_write().unwrap();
            // Root page 2 is `items` created by `c-setup-pair`
            bt_main
                .insert_table(
                    2,
                    2,
                    &encode_record(&[Value::Integer(2), Value::Text("rust_row2_main".into())]),
                )
                .unwrap();
            bt_aux
                .insert_table(
                    2,
                    2,
                    &encode_record(&[Value::Integer(2), Value::Text("rust_row2_aux".into())]),
                )
                .unwrap();

            let work_before = work_dir.clone();
            let sb = snap_before.clone();
            obs_vfs.faults.lock().unwrap().before_delete = Some(Box::new(move |path, sync_dir| {
                if let Some(s) = path.to_str()
                    && is_super_journal_name(s)
                {
                    assert!(sync_dir, "super-journal delete MUST pass sync_dir = true");
                    copy_directory_files(&work_before, &sb);
                }
                None
            }));

            let work_after = work_dir.clone();
            let sa = snap_after.clone();
            obs_vfs.faults.lock().unwrap().after_delete = Some(Box::new(move |path, _sync_dir| {
                if let Some(s) = path.to_str()
                    && is_super_journal_name(s)
                {
                    copy_directory_files(&work_after, &sa);
                    // Abort right after `-mj` unlink so cold journals stay untouched in `snap_after`!
                    return Some(SqliteError::with_extended(
                        extended::SQLITE_IOERR_DELETE,
                        "stop after snapshotting post-unlink state",
                    ));
                }
                None
            }));

            let main_canon = bt_main.db_path().unwrap().to_path_buf();
            let _ = commit_btrees_two_phase(
                &*obs_vfs,
                Some(&main_canon),
                false,
                &mut [&mut bt_main, &mut bt_aux],
                &[false, false],
            );
            obs_vfs.clear_faults();
            let _ = bt_main.close();
            let _ = bt_aux.close();
        }

        // B1: C SQLite 3.54.1 recovers `rust_snap_before` (in BOTH `main->aux` and `aux->main` orders):
        //     rolls back both DBs to `(1, init_...)`, keeps `-mj` after 1st DB, deletes `-mj` after 2nd DB!
        for main_first in [true, false] {
            copy_directory_files(&snap_before, &work_dir);
            assert_eq!(find_super_journals_in_dir(&work_dir).len(), 1);

            let (first_db, expected_first, second_db, expected_second) = if main_first {
                (&main_db, "1:init_main", &aux_db, "1:init_aux")
            } else {
                (&aux_db, "1:init_aux", &main_db, "1:init_main")
            };

            let out = Command::new(helper)
                .arg("c-recover-and-verify")
                .arg(&work_dir)
                .arg(first_db)
                .arg(second_db)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "C recover stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let kv = parse_kv_stdout(&String::from_utf8_lossy(&out.stdout));
            assert_eq!(kv.get("FIRST_INTEGRITY").map(String::as_str), Some("ok"));
            assert_eq!(
                kv.get("FIRST_ROWS").map(String::as_str),
                Some(expected_first)
            );
            assert_eq!(
                kv.get("FIRST_MJ_PRESENT").map(String::as_str),
                Some("1"),
                "C SQLite 3.54.1 must preserve Rust's `-mj` after 1st DB recovery (mode={jmode_str}, main_first={main_first})"
            );
            assert_eq!(kv.get("SECOND_INTEGRITY").map(String::as_str), Some("ok"));
            assert_eq!(
                kv.get("SECOND_ROWS").map(String::as_str),
                Some(expected_second)
            );
            assert_eq!(
                kv.get("SECOND_MJ_PRESENT").map(String::as_str),
                Some("0"),
                "C SQLite 3.54.1 must delete Rust's `-mj` after 2nd DB recovery (mode={jmode_str}, main_first={main_first})"
            );
        }

        // B2: C SQLite 3.54.1 opens `rust_snap_after` (in BOTH `main->aux` and `aux->main` orders):
        //     preserves committed rows in both DBs and cleans up Rust's cold journals!
        for main_first in [true, false] {
            copy_directory_files(&snap_after, &work_dir);
            assert!(find_super_journals_in_dir(&work_dir).is_empty());
            assert!(work_dir.join("main.db-journal").exists());
            assert!(work_dir.join("aux.db-journal").exists());

            let (first_db, expected_first, second_db, expected_second) = if main_first {
                (
                    &main_db,
                    "1:init_main,2:rust_row2_main",
                    &aux_db,
                    "1:init_aux,2:rust_row2_aux",
                )
            } else {
                (
                    &aux_db,
                    "1:init_aux,2:rust_row2_aux",
                    &main_db,
                    "1:init_main,2:rust_row2_main",
                )
            };

            let out = Command::new(helper)
                .arg("c-recover-and-verify")
                .arg(&work_dir)
                .arg(first_db)
                .arg(second_db)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "C recover post-unlink stderr: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let kv = parse_kv_stdout(&String::from_utf8_lossy(&out.stdout));
            assert_eq!(kv.get("FIRST_INTEGRITY").map(String::as_str), Some("ok"));
            assert_eq!(
                kv.get("FIRST_ROWS").map(String::as_str),
                Some(expected_first)
            );
            assert_eq!(kv.get("SECOND_INTEGRITY").map(String::as_str), Some("ok"));
            assert_eq!(
                kv.get("SECOND_ROWS").map(String::as_str),
                Some(expected_second)
            );
            assert!(!work_dir.join("main.db-journal").exists());
            assert!(!work_dir.join("aux.db-journal").exists());
        }
    }
}
