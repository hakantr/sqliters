//! Phase 1.1-B Pager (`sqliters-pager`) Integration & C SQLite 3.54.1 Interoperability Suite:
//! - `PagerOpenOptions`, `DatabaseKind::{NamedDisk, PrivateTemporary, Memory}`, canonical sidecar derivation
//! - Lazy `Open/NoLock` open & first locked authoritative header read (`begin_read` / `effective_*`)
//! - `Open -> Write` failed lock rollback to `NoLock` vs `Read -> Write` preserving `Shared` (`lock7-1.5..1.7`)
//! - `commit_retaining_read(true)` / `downgrade_to_read()` including zero-write `Write` (`lock-7.3`)
//! - `JournalMode::{Delete, Truncate, Persist, Memory, Off}` full matrix & 28-byte `zeroJournalHdr`
//! - `LockGeometry::TEST_64K` (`0x10000`) + `page_size = 1024` skipping `lock_byte_page = 65`
//! - Indexed anonymous physical sub-savepoint slots (`ensure_subsavepoints`, 0-page DB rollback)
#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sqliters::btree::Btree;
use sqliters::pager::{
    DB_HEADER_SIZE, DatabaseHeader, JOURNAL_HEADER_CORE_BYTES, JOURNAL_MAGIC, JournalMode, Pager,
    PagerOpenOptions, TransactionState,
};
use sqliters::types::{
    DatabaseKind, ErrorCode, KeySortOrder, LockGeometry, TextEncoding, Value, encode_record,
    extended,
};
use sqliters::vfs::{LockLevel, MemoryVfs, OpenFlags, OsVfs, SyncFlags, Vfs, VfsFile};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p1b_{prefix}_{pid}_{id}"));
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

#[derive(Default)]
struct SpyCounters {
    full_pathname_calls: AtomicUsize,
    temp_db_opens: AtomicUsize,
    journal_truncates: AtomicUsize,
    journal_syncs: AtomicUsize,
}

/// Forwarding VFS wrapper around `OsVfs` that tracks:
/// - calls to `full_pathname` (asserting `""` and `":memory:"` are never passed),
/// - materialized anonymous temporary `.db` paths (`open(None, TEMP_DB | ...)`),
/// - journal `truncate` and `sync` counts for verifying zero-write `Truncate`/`Persist` behavior.
struct SpyOsVfs {
    inner: OsVfs,
    sandbox_dir: PathBuf,
    next_temp_id: AtomicU64,
    last_temp_db_path: Mutex<Option<PathBuf>>,
    counters: Arc<SpyCounters>,
}

impl SpyOsVfs {
    fn new(sandbox_dir: &Path) -> Self {
        Self {
            inner: OsVfs::new(),
            sandbox_dir: sandbox_dir.to_path_buf(),
            next_temp_id: AtomicU64::new(1),
            last_temp_db_path: Mutex::new(None),
            counters: Arc::new(SpyCounters::default()),
        }
    }

    fn last_temp_db_path(&self) -> Option<PathBuf> {
        self.last_temp_db_path.lock().unwrap().clone()
    }
}

struct SpyJournalFile {
    inner: Box<dyn VfsFile>,
    counters: Arc<SpyCounters>,
}

impl VfsFile for SpyJournalFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> sqliters::types::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> sqliters::types::Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> sqliters::types::Result<()> {
        self.counters
            .journal_truncates
            .fetch_add(1, Ordering::Relaxed);
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> sqliters::types::Result<()> {
        self.counters.journal_syncs.fetch_add(1, Ordering::Relaxed);
        self.inner.sync(flags)
    }

    fn file_size(&self) -> sqliters::types::Result<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> sqliters::types::Result<()> {
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> sqliters::types::Result<()> {
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&self) -> sqliters::types::Result<bool> {
        self.inner.check_reserved_lock()
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn lock_state_for_diagnostics(&self) -> sqliters::types::Result<Option<LockLevel>> {
        self.inner.lock_state_for_diagnostics()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }

    fn close(&mut self) -> sqliters::types::Result<()> {
        self.inner.close()
    }
}

impl Vfs for SpyOsVfs {
    fn name(&self) -> &str {
        "spy-os"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(
        &self,
        path: Option<&Path>,
        flags: OpenFlags,
    ) -> sqliters::types::Result<Box<dyn VfsFile>> {
        if path.is_none() && flags.contains(OpenFlags::TEMP_DB) {
            self.counters.temp_db_opens.fetch_add(1, Ordering::Relaxed);
            let id = self.next_temp_id.fetch_add(1, Ordering::Relaxed);
            let p = self.sandbox_dir.join(format!("spy_private_temp_{id}.db"));
            *self.last_temp_db_path.lock().unwrap() = Some(p.clone());
            return self.inner.open(Some(&p), flags | OpenFlags::DELETEONCLOSE);
        }
        let opened = self.inner.open(path, flags)?;
        if flags.contains(OpenFlags::MAIN_JOURNAL) || flags.contains(OpenFlags::TEMP_JOURNAL) {
            return Ok(Box::new(SpyJournalFile {
                inner: opened,
                counters: Arc::clone(&self.counters),
            }));
        }
        Ok(opened)
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> sqliters::types::Result<()> {
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> sqliters::types::Result<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> sqliters::types::Result<PathBuf> {
        assert!(
            !path.as_os_str().is_empty() && path != Path::new(":memory:"),
            "empty or :memory: path must NEVER be passed to Vfs::full_pathname: {path:?}"
        );
        self.counters
            .full_pathname_calls
            .fetch_add(1, Ordering::Relaxed);
        self.inner.full_pathname(path)
    }
}

fn journal_checksum_ref(nonce: u32, page_data: &[u8]) -> u32 {
    let mut cksum = nonce;
    let mut i = page_data.len();
    while i >= 200 {
        i -= 200;
        cksum = cksum.wrapping_add(u32::from(page_data[i]));
    }
    cksum
}

/// Crafts a valid single-section SQLite hot rollback journal file containing the given pre-images.
fn write_synthetic_hot_journal(
    journal_path: &Path,
    orig_pages: u32,
    sector_size: u32,
    page_size: u32,
    nonce: u32,
    records: &[(u32, &[u8])],
) {
    let hdr_sz = sector_size as usize;
    let mut buf = vec![0u8; hdr_sz];
    buf[0..8].copy_from_slice(&JOURNAL_MAGIC);
    buf[8..12].copy_from_slice(&(records.len() as u32).to_be_bytes());
    buf[12..16].copy_from_slice(&nonce.to_be_bytes());
    buf[16..20].copy_from_slice(&orig_pages.to_be_bytes());
    buf[20..24].copy_from_slice(&sector_size.to_be_bytes());
    buf[24..28].copy_from_slice(&page_size.to_be_bytes());

    for &(pgno, page_bytes) in records {
        assert_eq!(page_bytes.len(), page_size as usize);
        buf.extend_from_slice(&pgno.to_be_bytes());
        buf.extend_from_slice(page_bytes);
        let cksum = journal_checksum_ref(nonce, page_bytes);
        buf.extend_from_slice(&cksum.to_be_bytes());
    }

    fs::write(journal_path, &buf).expect("write synthetic hot journal");
}

#[cfg(unix)]
mod c_ref {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    const C_PAGER_HELPER_SOURCE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sqlite3.h"

static int print_row_cb(void *unused, int argc, char **argv, char **col_names) {
    (void)unused;
    (void)col_names;
    for (int i = 0; i < argc; i++) {
        if (i > 0) fputc('|', stdout);
        fputs(argv[i] ? argv[i] : "", stdout);
    }
    fputc('\n', stdout);
    return 0;
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

    if (strcmp(cmd, "exec") == 0) {
        if (argc < 5) return 2;
        const char *db_path = argv[2];
        unsigned int pbyte = (unsigned int)strtoul(argv[3], NULL, 0);
        const char *sql = argv[4];

        sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, pbyte);
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(
            db_path,
            &db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE,
            NULL
        );
        if (rc != SQLITE_OK) {
            fprintf(stderr, "open failed (%d): %s\n", rc, db ? sqlite3_errmsg(db) : "no-db");
            if (db) sqlite3_close(db);
            return 2;
        }
        sqlite3_busy_timeout(db, 0);

        char *errmsg = NULL;
        rc = sqlite3_exec(db, sql, print_row_cb, NULL, &errmsg);
        if (rc != SQLITE_OK) {
            fprintf(stderr, "SQL failed (%d): %s\n", rc, errmsg ? errmsg : "");
            sqlite3_free(errmsg);
            sqlite3_close(db);
            return 2;
        }
        sqlite3_close(db);
        fflush(stdout);
        return 0;
    }

    return 2;
}
"#;

    /// Builds `c_pager_helper` out-of-tree inside `<workspace>/target/sqlite-ref/` against the
    /// workspace's pinned C SQLite 3.54.1 amalgamation without writing a single byte into `./sqlite/`.
    pub fn c_pager_helper() -> &'static Path {
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
                    .expect("invoke make for out-of-tree sqlite3.c amalgamation");
                assert!(
                    make_out.status.success(),
                    "failed to build sqlite3.c:\n{}",
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

            let helper_c = ref_root.join("c_pager_helper.c");
            let helper_bin = ref_root.join("c_pager_helper");
            let need_compile = match fs::read_to_string(&helper_c) {
                Ok(existing) => existing != C_PAGER_HELPER_SOURCE || !helper_bin.exists(),
                Err(_) => true,
            };
            if need_compile {
                fs::write(&helper_c, C_PAGER_HELPER_SOURCE).expect("write c_pager_helper.c");
                let cc_out = Command::new("cc")
                    .args(["-O2", "-pthread"])
                    .arg("-I")
                    .arg(&amalg_dir)
                    .arg(&helper_c)
                    .arg(&sqlite3_o)
                    .args(["-ldl", "-lm", "-o"])
                    .arg(&helper_bin)
                    .output()
                    .expect("compile c_pager_helper");
                assert!(
                    cc_out.status.success(),
                    "failed compiling c_pager_helper:\n{}",
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

            let ver_out = Command::new(&helper_bin)
                .arg("version")
                .output()
                .expect("run c_pager_helper version");
            assert!(ver_out.status.success());
            let ver_str = String::from_utf8(ver_out.stdout).expect("utf8 version");
            let expected_ver = fs::read_to_string(sqlite_src_dir.join("VERSION"))
                .expect("read sqlite/VERSION")
                .trim()
                .to_owned();
            let expected_uuid = fs::read_to_string(sqlite_src_dir.join("manifest.uuid"))
                .expect("read sqlite/manifest.uuid")
                .trim()
                .to_owned();
            assert!(
                ver_str.contains(&format!("VERSION={expected_ver}")),
                "unexpected C SQLite version: {ver_str}"
            );
            assert!(
                ver_str.contains(&expected_uuid[..24]),
                "unexpected C SQLite sourceid: {ver_str} vs {expected_uuid}"
            );

            helper_bin
        })
    }

    pub fn run_c_sql(db_path: &Path, pending_byte: u64, sql: &str) -> String {
        let out = Command::new(c_pager_helper())
            .arg("exec")
            .arg(db_path)
            .arg(format!("0x{pending_byte:x}"))
            .arg(sql)
            .output()
            .expect("invoke c_pager_helper exec");
        assert!(
            out.status.success(),
            "C SQLite 3.54.1 failed on `{sql}`:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .expect("utf8 stdout")
            .trim()
            .to_owned()
    }
}

// ============================================================================
// 1. Lazy `Open/NoLock` Open, Concurrent `EXCLUSIVE`, Error Rollback, & Deferred Hot-Journal Recovery
// ============================================================================

#[test]
fn test_lazy_open_nolock_concurrent_exclusive_and_deferred_hot_journal_recovery() {
    let sandbox = TempSandbox::new("lazy_open");
    let db_path = sandbox.path().join("lazy.db");
    let j_path = sandbox.path().join("lazy.db-journal");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    // Initialize a 2-page DB via Connection A
    let mut conn_a = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(conn_a.transaction_state(), TransactionState::Open);
    assert!(!conn_a.is_header_loaded());
    assert_eq!(conn_a.page_count(), 0);
    assert_eq!(
        conn_a.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    conn_a.begin_write().unwrap();
    let p2 = conn_a.allocate_raw_page().unwrap();
    assert_eq!(p2, 2);
    conn_a.write_page(2).unwrap()[0..8].copy_from_slice(b"CLEAN_P2");
    conn_a.commit().unwrap();

    // Connection A now acquires `EXCLUSIVE`!
    conn_a.begin_exclusive().unwrap();
    assert_eq!(
        conn_a.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Exclusive)
    );

    // While A holds `EXCLUSIVE`, Connection B opens the same `.db`: MUST succeed in `Open/NoLock`!
    let mut conn_b = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(conn_b.transaction_state(), TransactionState::Open);
    assert!(!conn_b.is_header_loaded());
    assert_eq!(conn_b.page_count(), 0);
    assert_eq!(
        conn_b.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    // `begin_read()` while A holds `EXCLUSIVE` fails with `Busy` and leaves B in `Open/NoLock`
    let err = conn_b.begin_read().unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(conn_b.transaction_state(), TransactionState::Open);
    assert!(!conn_b.is_header_loaded());
    assert_eq!(
        conn_b.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    // Release A's `EXCLUSIVE` lock; now B's first `begin_read()` loads the authoritative header!
    conn_a.rollback().unwrap();
    conn_a.close().unwrap();

    conn_b.begin_read().unwrap();
    assert!(conn_b.is_header_loaded());
    assert_eq!(conn_b.page_count(), 2);
    let clean_page2 = conn_b.read_page(2).unwrap().to_vec();
    assert_eq!(&clean_page2[0..8], b"CLEAN_P2");
    conn_b.close().unwrap();

    // Simulate a crashed writer: corrupt page 2 in `.db` and write a valid hot `<db>-journal`!
    let mut raw_db = fs::read(&db_path).unwrap();
    raw_db[4096..4104].copy_from_slice(b"CRASHED!");
    fs::write(&db_path, &raw_db).unwrap();
    write_synthetic_hot_journal(&j_path, 2, 512, 4096, 0x1234_5678, &[(2, &clean_page2)]);
    assert!(j_path.exists());

    // Opening readonly and readwrite pagers MUST NOT touch or delete the hot journal during `open_*`!
    let mut p_ro = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let mut p_rw = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert!(
        j_path.exists(),
        "hot journal must NOT be replayed or unlinked during Pager::open_*"
    );

    // `p_ro.begin_read()` fails with `SQLITE_READONLY_ROLLBACK` and unlocks `.db` back to `NoLock`!
    let ro_err = p_ro.begin_read().unwrap_err();
    assert_eq!(ro_err.code, ErrorCode::ReadOnly);
    assert_eq!(ro_err.extended_code, extended::SQLITE_READONLY_ROLLBACK);
    assert!(!p_ro.is_header_loaded());
    assert_eq!(p_ro.transaction_state(), TransactionState::Open);
    assert_eq!(
        p_ro.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    assert!(j_path.exists());

    // `p_rw.begin_read()` replays the hot journal, unlinks `<db>-journal`, and downgrades to `Shared`!
    p_rw.begin_read().unwrap();
    assert!(
        !j_path.exists(),
        "hot journal must be deleted on first locked read"
    );
    assert_eq!(
        p_rw.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Shared)
    );
    assert_eq!(&p_rw.read_page(2).unwrap()[0..8], b"CLEAN_P2");
    p_rw.close().unwrap();
    p_ro.close().unwrap();

    // Also verify `NotADb` and `-wal` (`CantOpen`) errors defer to `begin_read()` and unlock back to `NoLock`!
    let bad_path = sandbox.path().join("notadb.db");
    fs::write(&bad_path, b"not a valid sqlite file").unwrap();
    let mut p_bad = Pager::open_with_options(
        vfs.clone(),
        Some(&bad_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(
        p_bad.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    let bad_err = p_bad.begin_read().unwrap_err();
    assert_eq!(bad_err.code, ErrorCode::NotADb);
    assert!(!p_bad.is_header_loaded());
    assert_eq!(
        p_bad.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    p_bad.close().unwrap();

    let wal_file = sandbox.path().join("lazy.db-wal");
    let mut unsupported_wal = [0u8; 64];
    unsupported_wal[0..4].copy_from_slice(&0x377f_0682u32.to_be_bytes());
    unsupported_wal[4..8].copy_from_slice(&9_999_999u32.to_be_bytes());
    unsupported_wal[8..12].copy_from_slice(&4096u32.to_be_bytes());
    let [s0, s1] = sqliters_vfs::wal_checksum_with_endian(
        sqliters_vfs::WalChecksumEndian::Little,
        &unsupported_wal[0..24],
        None,
    )
    .unwrap();
    unsupported_wal[24..28].copy_from_slice(&s0.to_be_bytes());
    unsupported_wal[28..32].copy_from_slice(&s1.to_be_bytes());
    fs::write(&wal_file, unsupported_wal).unwrap();
    let mut p_wal = Pager::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let wal_err = p_wal.begin_read().unwrap_err();
    assert_eq!(wal_err.code, ErrorCode::CantOpen);
    assert!(!p_wal.is_header_loaded());
    assert_eq!(
        p_wal.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    p_wal.close().unwrap();
}

// ============================================================================
// 2. Candidate `default_page_size = 1024` Overridden by External `4096` Writer & `effective_*`
// ============================================================================

#[test]
fn test_default_page_size_candidate_overridden_by_external_writer_and_effective_accessors() {
    let sandbox = TempSandbox::new("page_size_override");
    let db_path = sandbox.path().join("race.db");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    // Create an initially 0-byte file on disk
    fs::write(&db_path, []).unwrap();

    let opts_1024 = PagerOpenOptions {
        default_page_size: 1024,
        ..PagerOpenOptions::default()
    };

    // Connection B1 and B2 open the empty file with `default_page_size = 1024` candidate
    let mut b1 = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        opts_1024,
    )
    .unwrap();
    let mut b2 = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        opts_1024,
    )
    .unwrap();
    assert_eq!(b1.page_size(), 1024);
    assert_eq!(b1.page_count(), 0);
    assert!(!b1.is_header_loaded());

    // Before B1/B2 read, Connection A populates `race.db` with `4096`-byte pages (3 pages)!
    let mut writer_4k = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(), // 4096
    )
    .unwrap();
    writer_4k.begin_write().unwrap();
    assert_eq!(writer_4k.allocate_raw_page().unwrap(), 2);
    assert_eq!(writer_4k.allocate_raw_page().unwrap(), 3);
    writer_4k.write_page(3).unwrap()[0..6].copy_from_slice(b"PAGE4K");
    writer_4k.commit().unwrap();
    writer_4k.close().unwrap();

    // `b1.effective_page_size()` & `b1.effective_page_count()` briefly acquire SHARED, override `1024` -> `4096`,
    // and return to `Open/NoLock`!
    assert_eq!(b1.effective_page_size().unwrap(), 4096);
    assert_eq!(b1.effective_page_count().unwrap(), 3);
    assert!(b1.is_header_loaded());
    assert_eq!(b1.transaction_state(), TransactionState::Open);
    assert_eq!(
        b1.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    b1.close().unwrap();

    // `b2.begin_read()` directly overrides the `1024` candidate with the on-disk `4096` header!
    b2.begin_read().unwrap();
    assert_eq!(b2.page_size(), 4096);
    assert_eq!(b2.page_count(), 3);
    assert_eq!(&b2.read_page(3).unwrap()[0..6], b"PAGE4K");
    b2.close().unwrap();

    // Conversely, a brand-new empty DB written with `opts_1024` produces a genuine 1024-byte page DB
    // (`2` pages = `2048` bytes on disk, matching `lock4-1.1`).
    let new_1k_path = sandbox.path().join("fresh1k.db");
    let mut p_1k = Pager::open_with_options(
        vfs,
        Some(&new_1k_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        opts_1024,
    )
    .unwrap();
    p_1k.begin_write().unwrap();
    assert_eq!(p_1k.allocate_raw_page().unwrap(), 2);
    p_1k.commit().unwrap();
    p_1k.close().unwrap();
    assert_eq!(fs::metadata(&new_1k_path).unwrap().len(), 2048);
}

// ============================================================================
// 3. `PagerOpenOptions` Validation & Symlink Canonical Sidecar Derivation
// ============================================================================

#[test]
fn test_pager_open_options_validation_and_misuse_guards() {
    let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::new());

    for bad_ps in [0, 256, 511, 1000, 3000, 65_535, 131_072] {
        let res = Pager::open_with_options(
            mem_vfs.clone(),
            None,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
            PagerOpenOptions {
                default_page_size: bad_ps,
                ..PagerOpenOptions::default()
            },
        );
        assert_eq!(res.err().unwrap().code, ErrorCode::Misuse);
    }

    // Mismatched `lock_geometry` between `PagerOpenOptions` and `Vfs` fails closed with `Misuse`!
    let mismatch = Pager::open_with_options(
        mem_vfs,
        Some(Path::new("geom.db")),
        OpenFlags::READWRITE | OpenFlags::CREATE,
        PagerOpenOptions {
            lock_geometry: Some(LockGeometry::TEST_64K),
            ..PagerOpenOptions::default()
        },
    );
    assert_eq!(mismatch.err().unwrap().code, ErrorCode::Misuse);
}

#[cfg(unix)]
#[test]
fn test_symlink_db_derives_canonical_journal_wal_and_shm_sidecars() {
    use std::os::unix::fs::symlink;

    let sandbox = TempSandbox::new("symlink_sidecar");
    let real_dir = sandbox.path().join("real_dir");
    let link_dir = sandbox.path().join("link_dir");
    fs::create_dir_all(&real_dir).unwrap();
    fs::create_dir_all(&link_dir).unwrap();

    let real_db = real_dir.join("target.db");
    let link_db = link_dir.join("alias.db");
    symlink(Path::new("../real_dir/target.db"), &link_db).unwrap();

    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
    let canonical_real = vfs.full_pathname(&real_db).unwrap();
    let expected_journal = PathBuf::from(format!("{}-journal", canonical_real.display()));
    let expected_wal = PathBuf::from(format!("{}-wal", canonical_real.display()));
    let expected_shm = PathBuf::from(format!("{}-shm", canonical_real.display()));
    let wrong_link_journal = link_dir.join("alias.db-journal");

    let mut pager = Pager::open_with_options(
        vfs,
        Some(&link_db),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();

    assert_eq!(pager.db_path(), Some(canonical_real.as_path()));
    assert_eq!(pager.journal_path(), Some(expected_journal.as_path()));
    assert_eq!(pager.wal_path(), Some(expected_wal.as_path()));
    assert_eq!(pager.shm_path(), Some(expected_shm.as_path()));

    // During an active `Delete`-mode write transaction, `<target.db>-journal` MUST appear in `real_dir`,
    // never `link_dir/alias.db-journal`!
    pager.begin_write().unwrap();
    let p2 = pager.allocate_raw_page().unwrap();
    pager.write_page(p2).unwrap()[0..4].copy_from_slice(b"LINK");
    assert!(expected_journal.exists());
    assert!(!wrong_link_journal.exists());
    pager.commit().unwrap();
    assert!(!expected_journal.exists());
    assert!(!wrong_link_journal.exists());

    // Switch to `Persist` mode and verify the persistent journal lives in `real_dir`!
    assert_eq!(
        pager.set_journal_mode(JournalMode::Persist).unwrap(),
        JournalMode::Persist
    );
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"PERS");
    pager.commit().unwrap();
    assert!(expected_journal.exists());
    assert!(!wrong_link_journal.exists());
    pager.close().unwrap();
}

// ============================================================================
// 4. `DatabaseKind::{NamedDisk, PrivateTemporary, Memory}` & Lazy Temporary File Lifecycle
// ============================================================================

#[test]
fn test_database_kind_named_private_temporary_and_memory_lifecycle() {
    let sandbox = TempSandbox::new("db_kinds");
    let spy = Arc::new(SpyOsVfs::new(sandbox.path()));
    let vfs: Arc<dyn Vfs> = spy.clone();

    // 1) `:memory:` and `OpenFlags::MEMORY` -> `DatabaseKind::Memory`, `is_memdb_backing == true`
    let mut mem1 = Pager::open_with_options(
        vfs.clone(),
        Some(Path::new(":memory:")),
        OpenFlags::READWRITE | OpenFlags::CREATE,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(mem1.db_kind(), DatabaseKind::Memory);
    assert!(mem1.is_memdb_backing());
    assert_eq!(mem1.journal_mode(), JournalMode::Memory);
    assert_eq!(mem1.lock_state_for_diagnostics().unwrap(), None);
    assert_eq!(spy.counters.full_pathname_calls.load(Ordering::Relaxed), 0);
    mem1.close().unwrap();

    // 2) `PrivateTemporary` with `temp_in_memory = true` -> logical `PrivateTemporary`, backing `memdb`
    let mut temp_mem = Pager::open_with_options(
        vfs.clone(),
        Some(Path::new("")),
        OpenFlags::READWRITE | OpenFlags::CREATE,
        PagerOpenOptions {
            temp_in_memory: true,
            ..PagerOpenOptions::default()
        },
    )
    .unwrap();
    assert_eq!(temp_mem.db_kind(), DatabaseKind::PrivateTemporary);
    assert!(temp_mem.is_memdb_backing());
    assert_eq!(temp_mem.journal_mode(), JournalMode::Memory);
    assert_eq!(temp_mem.lock_state_for_diagnostics().unwrap(), None);
    assert_eq!(spy.counters.full_pathname_calls.load(Ordering::Relaxed), 0);
    temp_mem.close().unwrap();

    // 3) Two file-backed `PrivateTemporary` pagers (`""` and `open_private_temporary`) have isolated
    // identities, open their backing OS file lazily on first write, report `None` -> `Some(...)`,
    // and delete the temporary OS file on `close()`!
    let mut tmp_a = Pager::open_with_options(
        vfs.clone(),
        Some(Path::new("")),
        OpenFlags::READWRITE | OpenFlags::CREATE,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let mut tmp_b =
        Pager::open_private_temporary(vfs.clone(), PagerOpenOptions::default()).unwrap();

    assert_eq!(tmp_a.db_kind(), DatabaseKind::PrivateTemporary);
    assert!(!tmp_a.is_memdb_backing());
    assert_eq!(tmp_a.journal_mode(), JournalMode::Delete);
    assert_eq!(spy.counters.full_pathname_calls.load(Ordering::Relaxed), 0);

    // Before any write (even across `begin_read` + `end_read_if_unused`), backing file is NOT opened!
    assert_eq!(tmp_a.lock_state_for_diagnostics().unwrap(), None);
    assert_eq!(spy.counters.temp_db_opens.load(Ordering::Relaxed), 0);
    tmp_a.begin_read().unwrap();
    assert_eq!(tmp_a.page_count(), 0);
    assert_eq!(tmp_a.lock_state_for_diagnostics().unwrap(), None);
    tmp_a.end_read_if_unused().unwrap();
    assert_eq!(spy.counters.temp_db_opens.load(Ordering::Relaxed), 0);

    // First write on `tmp_a` materializes its private temporary OS file!
    tmp_a.begin_write().unwrap();
    assert_eq!(spy.counters.temp_db_opens.load(Ordering::Relaxed), 1);
    let tmp_a_path = spy.last_temp_db_path().expect("temp_a path recorded");
    assert!(tmp_a_path.exists());
    assert_eq!(
        tmp_a.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Reserved)
    );
    let pa2 = tmp_a.allocate_raw_page().unwrap();
    tmp_a.write_page(pa2).unwrap()[0..6].copy_from_slice(b"TEMP_A");
    tmp_a.commit().unwrap();
    assert_eq!(
        tmp_a.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    // `tmp_b` is completely isolated from `tmp_a`: still 0 pages until its own write!
    tmp_b.begin_read().unwrap();
    assert_eq!(tmp_b.page_count(), 0);
    tmp_b.end_read_if_unused().unwrap();

    tmp_b.begin_write().unwrap();
    assert_eq!(spy.counters.temp_db_opens.load(Ordering::Relaxed), 2);
    let tmp_b_path = spy.last_temp_db_path().expect("temp_b path recorded");
    assert_ne!(tmp_a_path, tmp_b_path);
    assert!(tmp_b_path.exists());
    let pb2 = tmp_b.allocate_raw_page().unwrap();
    tmp_b.write_page(pb2).unwrap()[0..6].copy_from_slice(b"TEMP_B");
    tmp_b.commit().unwrap();

    tmp_a.begin_read().unwrap();
    assert_eq!(&tmp_a.read_page(2).unwrap()[0..6], b"TEMP_A");
    tmp_b.begin_read().unwrap();
    assert_eq!(&tmp_b.read_page(2).unwrap()[0..6], b"TEMP_B");

    // Closing `tmp_a` and `tmp_b` unlinks both temporary OS files from disk!
    tmp_a.close().unwrap();
    assert!(
        !tmp_a_path.exists(),
        "PrivateTemporary file must be deleted from disk on close()"
    );
    tmp_b.close().unwrap();
    assert!(
        !tmp_b_path.exists(),
        "PrivateTemporary file must be deleted from disk on close()"
    );

    // 4) Direct `OsVfs` (`open(None, ...)`) lazy `PrivateTemporary` verification (`None` -> `Some(...)`)
    let direct_os: Arc<dyn Vfs> = Arc::new(OsVfs::new());
    let mut direct_tmp =
        Pager::open_private_temporary(direct_os, PagerOpenOptions::default()).unwrap();
    assert_eq!(direct_tmp.db_kind(), DatabaseKind::PrivateTemporary);
    assert!(!direct_tmp.is_memdb_backing());
    assert_eq!(direct_tmp.lock_state_for_diagnostics().unwrap(), None);
    direct_tmp.begin_write().unwrap();
    assert_eq!(
        direct_tmp.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Reserved)
    );
    let dp2 = direct_tmp.allocate_raw_page().unwrap();
    direct_tmp.write_page(dp2).unwrap()[0..4].copy_from_slice(b"OS_T");
    direct_tmp.commit().unwrap();
    assert_eq!(
        direct_tmp.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    direct_tmp.close().unwrap();
}

// ============================================================================
// 5. `Open -> Write` `RESERVED` `Busy` Unlocks to `NoLock` vs `Read -> Write` Preserves `Shared`
// ============================================================================

#[test]
fn test_open_to_write_busy_unlocks_to_nolock_vs_read_to_write_preserves_shared_and_commit_busy() {
    let sandbox = TempSandbox::new("lock_rollback");
    let db_path = sandbox.path().join("locks.db");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    // Seed initial 2-page DB
    let mut seed = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    seed.begin_write().unwrap();
    let p2 = seed.allocate_raw_page().unwrap();
    seed.write_page(p2).unwrap()[0..4].copy_from_slice(b"V001");
    let initial_change_counter = seed.header().change_counter;
    seed.commit().unwrap();
    seed.close().unwrap();

    let mut writer = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let mut conn_open = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let mut conn_reader = Pager::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();

    // `conn_reader` acquires `SHARED` first
    conn_reader.begin_read().unwrap();
    assert_eq!(
        conn_reader.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Shared)
    );

    // `writer` acquires `RESERVED` (`SHARED` + `RESERVED` coexist in SQLite!)
    writer.begin_write().unwrap();
    assert_eq!(
        writer.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Reserved)
    );

    // `conn_open` starts from `Open/NoLock`: `begin_write*` acquires `SHARED`, hits `Busy` on `RESERVED`,
    // and MUST drop back to `NoLock` + `Open` (`lock7-1.5 -> lock7-1.7`)!
    assert_eq!(conn_open.begin_write().unwrap_err().code, ErrorCode::Busy);
    assert_eq!(conn_open.transaction_state(), TransactionState::Open);
    assert_eq!(
        conn_open.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    assert_eq!(
        conn_open
            .begin_write_with_savepoint_depth(2)
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(conn_open.subsavepoint_depth(), 0);
    assert_eq!(conn_open.transaction_state(), TransactionState::Open);
    assert_eq!(
        conn_open.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    // `conn_reader` already held `Read` (`SHARED`): `begin_write()` hits `Busy` on `RESERVED`
    // and MUST preserve `TransactionState::Read` and `LockLevel::Shared`!
    assert_eq!(conn_reader.begin_write().unwrap_err().code, ErrorCode::Busy);
    assert_eq!(conn_reader.transaction_state(), TransactionState::Read);
    assert_eq!(
        conn_reader.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::Shared)
    );
    assert_eq!(&conn_reader.read_page(2).unwrap()[0..4], b"V001");

    // `writer` dirties page 2 and attempts `commit()` while `conn_reader` still holds `SHARED`:
    // `lock(Exclusive)` returns `Busy` BEFORE `.db` is modified, leaving `writer` in `Write` with
    // dirty cache intact for retry!
    writer.write_page(2).unwrap()[0..4].copy_from_slice(b"V002");
    assert_eq!(writer.commit().unwrap_err().code, ErrorCode::Busy);
    assert_eq!(writer.transaction_state(), TransactionState::Write);
    assert_eq!(&writer.read_page(2).unwrap()[0..4], b"V002");

    // Release `conn_reader`'s `SHARED` lock; now `writer.commit()` succeeds and bumps `change_counter` once!
    conn_reader.end_read_if_unused().unwrap();
    assert_eq!(
        conn_reader.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    writer.commit().unwrap();
    assert_eq!(writer.header().change_counter, initial_change_counter + 2);

    // Verify `rollback()` also works cleanly after `commit()` hits `Busy`!
    conn_reader.begin_read().unwrap();
    writer.begin_write().unwrap();
    writer.write_page(2).unwrap()[0..5].copy_from_slice(b"ABORT");
    assert_eq!(writer.commit().unwrap_err().code, ErrorCode::Busy);
    writer.rollback().unwrap();
    assert_eq!(writer.transaction_state(), TransactionState::Open);
    assert_eq!(&conn_reader.read_page(2).unwrap()[0..4], b"V002");

    conn_reader.close().unwrap();
    conn_open.close().unwrap();
    writer.close().unwrap();
}

// ============================================================================
// 6. `commit_retaining_read(true)` / `downgrade_to_read()` on Dirty & Zero-Write (`lock-7.3`)
// ============================================================================

#[test]
fn test_commit_retaining_read_and_zero_write_downgrade_across_modes() {
    let sandbox = TempSandbox::new("downgrade_lock73");
    let spy = Arc::new(SpyOsVfs::new(sandbox.path()));
    let vfs: Arc<dyn Vfs> = spy.clone();
    let db_path = sandbox.path().join("lock73.db");
    let j_path = sandbox.path().join("lock73.db-journal");

    let mut p1 = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    let mut p2 = Pager::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();

    // Initialize 2-page DB
    p1.begin_write().unwrap();
    let pg = p1.allocate_raw_page().unwrap();
    p1.write_page(pg).unwrap()[0..4].copy_from_slice(b"INIT");
    p1.commit().unwrap();

    for mode in [
        JournalMode::Delete,
        JournalMode::Truncate,
        JournalMode::Persist,
    ] {
        assert_eq!(p1.set_journal_mode(mode).unwrap(), mode);

        // (A) Dirty `Write` -> `commit_retaining_read(true)` finalizes journal and downgrades to `Shared`!
        p1.begin_write().unwrap();
        let tag = mode.as_str().as_bytes();
        p1.write_page(2).unwrap()[0..tag.len()].copy_from_slice(tag);
        p1.commit_retaining_read(true).unwrap();

        assert_eq!(p1.transaction_state(), TransactionState::Read);
        assert_eq!(
            p1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Shared)
        );
        match mode {
            JournalMode::Delete => assert!(!j_path.exists()),
            JournalMode::Truncate => assert_eq!(fs::metadata(&j_path).unwrap().len(), 0),
            JournalMode::Persist => {
                let jb = fs::read(&j_path).unwrap();
                assert_eq!(&jb[0..JOURNAL_HEADER_CORE_BYTES], &[0u8; 28]);
            }
            _ => unreachable!(),
        }

        // Because `p1` downgraded from `Exclusive`/`Reserved` to `Shared`, `p2` CAN acquire `Shared` AND `Reserved`!
        p2.begin_read().unwrap();
        p2.begin_write().unwrap();
        p2.rollback().unwrap();

        p1.end_read_if_unused().unwrap();
        assert_eq!(p1.transaction_state(), TransactionState::Open);
        assert_eq!(
            p1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );

        // (B) Zero-write `Write` (`lock-7.3`): `BEGIN; UPDATE ... WHERE 0; COMMIT` with active reader!
        let trunc_before = spy.counters.journal_truncates.load(Ordering::Relaxed);
        let sync_before = spy.counters.journal_syncs.load(Ordering::Relaxed);

        p1.begin_write().unwrap();
        assert_eq!(
            p1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Reserved)
        );
        assert_eq!(p1.journal_offset(), 0);

        p1.downgrade_to_read().unwrap();
        assert_eq!(p1.transaction_state(), TransactionState::Read);
        assert_eq!(
            p1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::Shared)
        );

        // Zero-write transaction (`journal_off == 0`) must NOT perform unnecessary journal truncate/sync!
        assert_eq!(
            spy.counters.journal_truncates.load(Ordering::Relaxed),
            trunc_before
        );
        assert_eq!(
            spy.counters.journal_syncs.load(Ordering::Relaxed),
            sync_before
        );

        // Another writer can acquire `Reserved` now that `p1` dropped `Reserved` -> `Shared`!
        p2.begin_write().unwrap();
        p2.end_write(false).unwrap();

        p1.end_read_if_unused().unwrap();
        assert_eq!(
            p1.lock_state_for_diagnostics().unwrap(),
            Some(LockLevel::NoLock)
        );
    }

    p2.close().unwrap();
    p1.close().unwrap();
}

// ============================================================================
// 7. `JournalMode::Persist` 28-Byte Zeroing (`zeroJournalHdr`), `journal_size_limit`, & C SQLite 3.54.1
// ============================================================================

#[test]
fn test_journal_mode_persist_28_bytes_only_c_sqlite_interop_and_size_limit() {
    let sandbox = TempSandbox::new("persist_28b");
    let db_path = sandbox.path().join("persist.db");
    let j_path = sandbox.path().join("persist.db-journal");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    // Create a genuine SQLite schema table via `Btree` so C SQLite 3.54.1 can also query rows & `integrity_check`!
    let mut bt = Btree::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();

    assert_eq!(bt.pager().journal_size_limit(), -1);
    assert_eq!(bt.pager_mut().set_journal_size_limit(-42), -1); // `< -1` is a no-op!
    assert_eq!(
        bt.pager_mut()
            .set_journal_mode(JournalMode::Persist)
            .unwrap(),
        JournalMode::Persist
    );

    bt.begin_write().unwrap();
    let tbl_root = bt.create_table().unwrap();
    // Register `CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)` in `sqlite_schema` (root page 1)
    let schema_rec = encode_record(&[
        Value::Text("table".into()),
        Value::Text("items".into()),
        Value::Text("items".into()),
        Value::Integer(i64::from(tbl_root)),
        Value::Text("CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)".into()),
    ]);
    bt.insert_table(1, 1, &schema_rec).unwrap();
    bt.pager_mut().header_mut().unwrap().schema_cookie = 1;
    for i in 1..=20i64 {
        let row = encode_record(&[Value::Null, Value::Text(format!("row-{i:03}"))]);
        bt.insert_table(tbl_root, i, &row).unwrap();
    }
    bt.commit().unwrap();
    assert!(j_path.exists());

    // Start a second `Persist` write transaction so `<db>-journal` has a live header + page records
    bt.begin_write().unwrap();
    let updated_row = encode_record(&[Value::Null, Value::Text("committed-persist-row".into())]);
    assert!(bt.delete_table(tbl_root, 1).unwrap());
    bt.insert_table(tbl_root, 1, &updated_row).unwrap();

    // Stamp bytes `28..512` (1-based bytes 29..512) of `<db>-journal` with `0xA5` BEFORE `commit()`!
    // `syncJournal` only updates `0..12`, and `zeroJournalHdr` MUST only zero `0..28`, leaving `28..512` untouched!
    {
        let mut jbytes = fs::read(&j_path).unwrap();
        assert!(jbytes.len() >= 512);
        assert_eq!(&jbytes[0..8], &JOURNAL_MAGIC);
        jbytes[28..512].fill(0xA5);
        fs::write(&j_path, &jbytes).unwrap();
    }

    bt.commit().unwrap();

    let after_commit_jbytes = fs::read(&j_path).unwrap();
    assert!(after_commit_jbytes.len() > 4096);
    assert_eq!(
        &after_commit_jbytes[0..JOURNAL_HEADER_CORE_BYTES],
        &[0u8; 28],
        "Persist mode must zero the first 28 bytes of <db>-journal"
    );
    assert!(
        after_commit_jbytes[28..512].iter().all(|&b| b == 0xA5),
        "Persist mode must NOT zero bytes 28..512 of the journal header sector"
    );

    // Verify `journal_size_limit > 0` truncates oversized persistent journal down to `limit`!
    assert_eq!(bt.pager_mut().set_journal_size_limit(1024), 1024);
    bt.begin_write().unwrap();
    for i in 2..=5i64 {
        let r = encode_record(&[Value::Null, Value::Text(format!("updated-{i}"))]);
        assert!(bt.delete_table(tbl_root, i).unwrap());
        bt.insert_table(tbl_root, i, &r).unwrap();
    }
    bt.commit().unwrap();
    let limited_len = fs::metadata(&j_path).unwrap().len();
    assert_eq!(limited_len, 1024);
    assert_eq!(
        &fs::read(&j_path).unwrap()[0..JOURNAL_HEADER_CORE_BYTES],
        &[0u8; 28]
    );

    // Restore unlimited `-1`, stamp `28..512` again, commit, close Rust handle, and verify C SQLite 3.54.1!
    assert_eq!(bt.pager_mut().set_journal_size_limit(-1), -1);
    bt.begin_write().unwrap();
    assert!(bt.delete_table(tbl_root, 6).unwrap());
    bt.insert_table(
        tbl_root,
        6,
        &encode_record(&[Value::Null, Value::Text("for-c-sqlite".into())]),
    )
    .unwrap();
    {
        let mut jbytes = fs::read(&j_path).unwrap();
        jbytes[28..512].fill(0x5A);
        fs::write(&j_path, &jbytes).unwrap();
    }
    bt.commit().unwrap();
    bt.close().unwrap();

    #[cfg(unix)]
    {
        let out = c_ref::run_c_sql(
            &db_path,
            LockGeometry::PRODUCTION.pending_byte_offset(),
            "PRAGMA integrity_check; SELECT name FROM items WHERE id IN (1, 6) ORDER BY id;",
        );
        assert_eq!(out, "ok\ncommitted-persist-row\nfor-c-sqlite");
    }

    // Finally verify `journal_size_limit = 0` truncates `<db>-journal` to 0 bytes in `Persist` mode!
    let mut p_zero = Pager::open_file(&db_path, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
    assert_eq!(
        p_zero.set_journal_mode(JournalMode::Persist).unwrap(),
        JournalMode::Persist
    );
    assert_eq!(p_zero.set_journal_size_limit(0), 0);
    p_zero.begin_write().unwrap();
    p_zero.write_page(2).unwrap()[200] ^= 0x00;
    p_zero.rollback().unwrap();
    assert_eq!(fs::metadata(&j_path).unwrap().len(), 0);
    p_zero.close().unwrap();
}

// ============================================================================
// 8. Full `JournalMode` Matrix (`Memory`, `Off`, `Truncate`, `Delete`), Recovery, & Mode Transitions
// ============================================================================

#[test]
fn test_journal_mode_matrix_memory_off_savepoint134_and_reserved_delete_guard() {
    assert_eq!(
        JournalMode::from_pragma_str("DELETE"),
        Some(JournalMode::Delete)
    );
    assert_eq!(
        JournalMode::from_pragma_str("TrUnCaTe"),
        Some(JournalMode::Truncate)
    );
    assert_eq!(
        JournalMode::from_pragma_str("persist"),
        Some(JournalMode::Persist)
    );
    assert_eq!(
        JournalMode::from_pragma_str("MEMORY"),
        Some(JournalMode::Memory)
    );
    assert_eq!(JournalMode::from_pragma_str("off"), Some(JournalMode::Off));
    assert_eq!(JournalMode::from_pragma_str("wal"), Some(JournalMode::Wal));
    assert_eq!(JournalMode::from_pragma_str("bogus"), None);

    // 1) In-memory DB mode restrictions (`Memory` & `Off` only) + `Off` rollback keeping pages (`pager.c:6850`)
    let mut mem_pager = Pager::open_in_memory().unwrap();
    assert_eq!(mem_pager.journal_mode(), JournalMode::Memory);
    for disallowed in [
        JournalMode::Delete,
        JournalMode::Truncate,
        JournalMode::Persist,
        JournalMode::Wal,
    ] {
        assert_eq!(
            mem_pager.set_journal_mode(disallowed).unwrap(),
            JournalMode::Memory
        );
    }
    assert_eq!(
        mem_pager.set_journal_mode(JournalMode::Off).unwrap(),
        JournalMode::Off
    );
    mem_pager.begin_write().unwrap();
    let mp2 = mem_pager.allocate_raw_page().unwrap();
    mem_pager.write_page(mp2).unwrap()[0..7].copy_from_slice(b"MEM_OFF");
    // In `MEMDB` + `JournalMode::Off`, `rollback()` clears dirty flags while keeping modified pages in memory!
    mem_pager.rollback().unwrap();
    mem_pager.begin_read().unwrap();
    assert_eq!(mem_pager.page_count(), 2);
    assert_eq!(&mem_pager.read_page(2).unwrap()[0..7], b"MEM_OFF");
    mem_pager.close().unwrap();

    // 2) Disk DB `Memory` mode: no `<db>-journal` on disk, full rollback works, AND pre-existing hot journal is recovered!
    let sandbox = TempSandbox::new("jmode_matrix");
    let db_path = sandbox.path().join("matrix.db");
    let j_path = sandbox.path().join("matrix.db-journal");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    let mut pager = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(
        pager.set_journal_mode(JournalMode::Memory).unwrap(),
        JournalMode::Memory
    );
    pager.begin_write().unwrap();
    let p2 = pager.allocate_raw_page().unwrap();
    pager.write_page(p2).unwrap()[0..6].copy_from_slice(b"DISK_1");
    assert!(!j_path.exists());
    pager.commit().unwrap();
    assert!(!j_path.exists());

    // Modify & rollback in `Memory` mode
    pager.begin_write().unwrap();
    assert!(pager.ok_to_change_journal_mode()); // zero-write so far!
    pager.write_page(2).unwrap()[0..6].copy_from_slice(b"REVERT");
    assert!(!pager.ok_to_change_journal_mode()); // dirty page blocks mode change!
    assert_eq!(
        pager.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Memory
    );
    assert!(!j_path.exists());
    pager.rollback().unwrap();

    pager.begin_read().unwrap();
    let clean_p2 = pager.read_page(2).unwrap().to_vec();
    assert_eq!(&clean_p2[0..6], b"DISK_1");
    pager.close().unwrap();

    // Plant a crashed hot journal on disk, open a fresh pager, switch to `Memory` BEFORE first read,
    // and verify `begin_read()` still recovers & deletes the hot journal!
    let mut corrupted = fs::read(&db_path).unwrap();
    corrupted[4096..4102].copy_from_slice(b"BROKEN");
    fs::write(&db_path, &corrupted).unwrap();
    write_synthetic_hot_journal(&j_path, 2, 512, 4096, 0x9988_7766, &[(2, &clean_p2)]);

    let mut mem_rec = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(
        mem_rec.set_journal_mode(JournalMode::Memory).unwrap(),
        JournalMode::Memory
    );
    assert!(j_path.exists()); // still untouched before `begin_read()`
    mem_rec.begin_read().unwrap();
    assert!(
        !j_path.exists(),
        "Memory mode must still recover pre-existing hot journal"
    );
    assert_eq!(&mem_rec.read_page(2).unwrap()[0..6], b"DISK_1");

    // 3) Disk DB `Off` mode (`savepoint-13.4`): sub-savepoint `ROLLBACK TO` works in cache, AND
    // full `rollback()` when `db_file_modified == false` discards dirty cache to reload untouched disk pages!
    mem_rec.end_read_if_unused().unwrap();
    assert_eq!(
        mem_rec.set_journal_mode(JournalMode::Off).unwrap(),
        JournalMode::Off
    );
    mem_rec.begin_write().unwrap();
    mem_rec.write_page(2).unwrap()[0..6].copy_from_slice(b"OUTER!");
    mem_rec.ensure_subsavepoints(1).unwrap();
    let p3 = mem_rec.allocate_raw_page().unwrap();
    assert_eq!(p3, 3);
    mem_rec.write_page(2).unwrap()[0..6].copy_from_slice(b"INNER!");
    assert!(!j_path.exists());

    // `ROLLBACK TO` sub-savepoint 0 restores `"OUTER!"` and `page_count == 2` even in `JournalMode::Off`!
    mem_rec.rollback_to_subsavepoint(0).unwrap();
    assert_eq!(mem_rec.page_count(), 2);
    assert_eq!(&mem_rec.read_page(2).unwrap()[0..6], b"OUTER!");

    // Full `rollback()` on disk DB in `Off` mode discards dirty cache (`header_loaded = false`)
    // so the next `begin_read()` reloads `"DISK_1"` from untouched disk!
    mem_rec.rollback().unwrap();
    assert!(!mem_rec.is_header_loaded());
    mem_rec.begin_read().unwrap();
    assert_eq!(mem_rec.page_count(), 2);
    assert_eq!(&mem_rec.read_page(2).unwrap()[0..6], b"DISK_1");
    mem_rec.close().unwrap();

    // 4) Leaving `Truncate`/`Persist` for `Delete`: unlinks `<db>-journal` under `RESERVED` lock,
    // and does NOT unlink `<db>-journal` if another connection holds `RESERVED`!
    let mut p_pers = Pager::open_with_options(
        vfs.clone(),
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert_eq!(
        p_pers.set_journal_mode(JournalMode::Truncate).unwrap(),
        JournalMode::Truncate
    );
    p_pers.begin_write().unwrap();
    p_pers.write_page(2).unwrap()[0..6].copy_from_slice(b"TRUNC1");
    p_pers.commit().unwrap();
    assert!(j_path.exists());
    assert_eq!(fs::metadata(&j_path).unwrap().len(), 0);

    // Another connection holds `RESERVED` with an active journal: `p_pers.set_journal_mode(Delete)` from `Open`
    // MUST NOT delete the active writer's `<db>-journal`, and must return `p_pers` to `Open/NoLock`!
    let mut active_w = Pager::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    active_w.begin_write().unwrap();
    active_w.write_page(2).unwrap()[0..6].copy_from_slice(b"ACTIVE");
    assert!(fs::metadata(&j_path).unwrap().len() > 0);

    assert_eq!(
        p_pers.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Delete
    );
    assert!(
        j_path.exists(),
        "set_journal_mode(Delete) must NOT unlink <db>-journal owned by another RESERVED writer"
    );
    assert_eq!(p_pers.transaction_state(), TransactionState::Open);
    assert_eq!(
        p_pers.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    active_w.rollback().unwrap();
    active_w.close().unwrap();

    // Now without any concurrent writer, `Persist -> Delete` unlinks `<db>-journal` cleanly!
    assert_eq!(
        p_pers.set_journal_mode(JournalMode::Persist).unwrap(),
        JournalMode::Persist
    );
    p_pers.begin_write().unwrap();
    p_pers.write_page(2).unwrap()[0..6].copy_from_slice(b"PERS_2");
    p_pers.commit().unwrap();
    assert!(j_path.exists());

    assert_eq!(
        p_pers.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Delete
    );
    assert!(!j_path.exists());
    assert_eq!(p_pers.transaction_state(), TransactionState::Open);
    assert_eq!(
        p_pers.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    p_pers.close().unwrap();
}

// ============================================================================
// 9. `LockGeometry::TEST_64K` (`0x10000`) + `page_size = 1024` Skips Page 65 (`lock_byte_page = 65`)
// ============================================================================

#[test]
fn test_lock_geometry_test_64k_skips_page_65_btree_integrity_and_c_sqlite_interop() {
    let sandbox = TempSandbox::new("geom_64k");
    let raw_db_path = sandbox.path().join("raw64k.db");
    let bt_db_path = sandbox.path().join("btree64k.db");

    let vfs_64k: Arc<dyn Vfs> = Arc::new(OsVfs::with_lock_geometry(LockGeometry::TEST_64K));
    let opts_64k_1k = PagerOpenOptions {
        default_page_size: 1024,
        lock_geometry: Some(LockGeometry::TEST_64K),
        db_kind: None,
        temp_in_memory: false,
        ..PagerOpenOptions::default()
    };

    // (A) Direct `Pager::allocate_raw_page()` test: 64 -> 66 (page 65 skipped!)
    let mut pager = Pager::open_with_options(
        vfs_64k.clone(),
        Some(&raw_db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        opts_64k_1k,
    )
    .unwrap();
    assert_eq!(pager.lock_geometry(), LockGeometry::TEST_64K);
    assert_eq!(pager.lock_byte_page(), 65);
    assert_eq!(pager.lock_geometry().sj_pgno(1024), 65);

    pager.begin_write().unwrap(); // initializes page 1
    assert_eq!(pager.page_count(), 1);
    for expected in 2..=64 {
        let got = pager.allocate_raw_page().unwrap();
        assert_eq!(got, expected);
    }
    // Next allocation after page 64 MUST skip lock-byte page 65 and return 66!
    let after_64 = pager.allocate_raw_page().unwrap();
    assert_eq!(after_64, 66);
    assert_eq!(pager.page_count(), 66);

    // Direct `read_page(65)` and `write_page(65)` must fail closed with `Corrupt`!
    assert_eq!(pager.read_page(65).unwrap_err().code, ErrorCode::Corrupt);
    assert_eq!(pager.write_page(65).unwrap_err().code, ErrorCode::Corrupt);

    pager.write_page(64).unwrap()[0..4].copy_from_slice(b"PG64");
    pager.write_page(66).unwrap()[0..4].copy_from_slice(b"PG66");
    pager.commit().unwrap();
    pager.close().unwrap();

    // Reopen and verify pages 64 and 66 persisted cleanly
    let mut check_p = Pager::open_with_options(
        vfs_64k.clone(),
        Some(&raw_db_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        opts_64k_1k,
    )
    .unwrap();
    check_p.begin_read().unwrap();
    assert_eq!(check_p.page_count(), 66);
    assert_eq!(&check_p.read_page(64).unwrap()[0..4], b"PG64");
    let clean_pg66 = check_p.read_page(66).unwrap().to_vec();
    assert_eq!(&clean_pg66[0..4], b"PG66");
    check_p.close().unwrap();

    // Verify hot-journal recovery under `LockGeometry::TEST_64K` (`sj_pgno == 65` sentinel honored,
    // page 66 restored cleanly)!
    {
        let raw_j_path = sandbox.path().join("raw64k.db-journal");
        let mut raw_bytes = fs::read(&raw_db_path).unwrap();
        let pg66_off = 65 * 1024;
        raw_bytes[pg66_off..pg66_off + 4].copy_from_slice(b"BAD!");
        fs::write(&raw_db_path, &raw_bytes).unwrap();

        let dummy_sj = vec![0xEEu8; 1024];
        let dummy_after_sj = vec![0xFFu8; 1024];
        // Record 1: valid page 66 pre-image; Record 2: `pgno == sj_pgno == 65` stops playback before Record 3 (page 64)!
        write_synthetic_hot_journal(
            &raw_j_path,
            66,
            512,
            1024,
            0xCAFE_BABE,
            &[(66, &clean_pg66), (65, &dummy_sj), (64, &dummy_after_sj)],
        );

        let mut rec_p = Pager::open_with_options(
            vfs_64k.clone(),
            Some(&raw_db_path),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            opts_64k_1k,
        )
        .unwrap();
        rec_p.begin_read().unwrap();
        assert!(!raw_j_path.exists());
        assert_eq!(&rec_p.read_page(66).unwrap()[0..4], b"PG66");
        assert_eq!(&rec_p.read_page(64).unwrap()[0..4], b"PG64"); // stopped at `sj_pgno == 65`!
        rec_p.close().unwrap();
    }

    // (B) Full `Btree` + `integrity_check` + C SQLite 3.54.1 (`SQLITE_TESTCTRL_PENDING_BYTE = 0x10000`) across > 80 pages!
    let mut bt = Btree::open_with_options(
        vfs_64k.clone(),
        Some(&bt_db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        opts_64k_1k,
    )
    .unwrap();
    assert_eq!(bt.lock_byte_page(), 65);

    bt.begin_write().unwrap();
    let tbl_root = bt.create_table().unwrap();
    let idx_root = bt.create_index().unwrap();
    let orders = [KeySortOrder::default(), KeySortOrder::default()];

    let s_tbl = encode_record(&[
        Value::Text("table".into()),
        Value::Text("big".into()),
        Value::Text("big".into()),
        Value::Integer(i64::from(tbl_root)),
        Value::Text("CREATE TABLE big(id INTEGER PRIMARY KEY, body TEXT)".into()),
    ]);
    let s_idx = encode_record(&[
        Value::Text("index".into()),
        Value::Text("idx_big_body".into()),
        Value::Text("big".into()),
        Value::Integer(i64::from(idx_root)),
        Value::Text("CREATE INDEX idx_big_body ON big(body)".into()),
    ]);
    bt.insert_table(1, 1, &s_tbl).unwrap();
    bt.insert_table(1, 2, &s_idx).unwrap();
    bt.pager_mut().header_mut().unwrap().schema_cookie = 1;

    for i in 1..=120i64 {
        let payload = if i % 5 == 0 {
            format!("ovfl-{i:04}-{}", "Z".repeat(1400))
        } else {
            format!("row-{i:04}-{}", "A".repeat(320))
        };
        let rec = encode_record(&[Value::Null, Value::Text(payload.clone())]);
        bt.insert_table(tbl_root, i, &rec).unwrap();
        let irec = encode_record(&[Value::Text(payload), Value::Integer(i)]);
        bt.insert_index(idx_root, &irec, &orders).unwrap();
    }

    assert!(
        bt.pager().page_count() > 80,
        "expected > 80 pages across lock_byte_page 65, got {}",
        bt.pager().page_count()
    );
    assert_eq!(
        bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(),
        Vec::<String>::new(),
        "Btree::integrity_check must skip unused lock_byte_page 65 cleanly"
    );

    // Verify `integrity_check` catches forged freelist reference to `lock_byte_page = 65`!
    bt.pager_mut().ensure_subsavepoints(1).unwrap();
    {
        let hdr = bt.pager_mut().header_mut().unwrap();
        hdr.first_freelist_trunk_page = 65;
        hdr.total_freelist_pages = 1;
    }
    let errs = bt.integrity_check(&[1, tbl_root, idx_root]).unwrap();
    assert!(
        errs.iter().any(|e| e.contains("lock-byte page 65")),
        "expected lock-byte page 65 corruption diagnostic, got: {errs:?}"
    );
    bt.pager_mut().rollback_to_subsavepoint(0).unwrap();
    bt.pager_mut().release_subsavepoint(0).unwrap();
    assert_eq!(
        bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(),
        Vec::<String>::new()
    );

    bt.commit().unwrap();
    bt.close().unwrap();

    #[cfg(unix)]
    {
        let out = c_ref::run_c_sql(
            &bt_db_path,
            LockGeometry::TEST_64K.pending_byte_offset(),
            "PRAGMA integrity_check; SELECT count(*) FROM big;",
        );
        assert_eq!(out, "ok\n120");
    }
}

// ============================================================================
// 10. Indexed Anonymous Physical Sub-Savepoint Slots (`Open` No-Op & 0-Page Rollback)
// ============================================================================

#[test]
fn test_indexed_anonymous_subsavepoints_open_noop_and_empty_db_zero_page_rollback() {
    let sandbox = TempSandbox::new("subsavepoints");
    let db_path = sandbox.path().join("sp.db");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    let mut pager = Pager::open_with_options(
        vfs,
        Some(&db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();

    // `ensure_subsavepoints` in `Open` or `Read` MUST NEVER force `begin_write()`!
    pager.ensure_subsavepoints(3).unwrap();
    assert_eq!(pager.transaction_state(), TransactionState::Open);
    assert_eq!(pager.subsavepoint_depth(), 0);
    assert_eq!(
        pager.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );

    pager.begin_read().unwrap();
    pager.ensure_subsavepoints(2).unwrap();
    assert_eq!(pager.transaction_state(), TransactionState::Read);
    assert_eq!(pager.subsavepoint_depth(), 0);
    pager.end_read_if_unused().unwrap();

    // `begin_write_with_savepoint_depth(1)` on a 0-page empty DB opens slot 0 BEFORE Page 1 is initialized,
    // enabling `SAVEPOINT s1; CREATE TABLE ...; ROLLBACK TO s1;` to roll back all the way to 0 pages!
    pager.begin_write_with_savepoint_depth(1).unwrap();
    assert_eq!(pager.subsavepoint_depth(), 1);
    assert_eq!(pager.page_count(), 1); // Page 1 initialized AFTER slot 0 captured `orig_page_count = 0`!

    let p2 = pager.allocate_raw_page().unwrap();
    assert_eq!(p2, 2);
    pager.write_page(2).unwrap()[0..5].copy_from_slice(b"OUTER");

    // Nested slot 1
    pager.ensure_subsavepoints(2).unwrap();
    assert_eq!(pager.subsavepoint_depth(), 2);
    let p3 = pager.allocate_raw_page().unwrap();
    assert_eq!(p3, 3);
    pager.write_page(2).unwrap()[0..5].copy_from_slice(b"INNER");

    // Roll back to slot 1: keeps slot 1 open (`depth == 2`), restores `page_count == 2` and `"OUTER"`
    pager.rollback_to_subsavepoint(1).unwrap();
    assert_eq!(pager.subsavepoint_depth(), 2);
    assert_eq!(pager.page_count(), 2);
    assert_eq!(&pager.read_page(2).unwrap()[0..5], b"OUTER");

    // Modify again inside slot 1, then `release_subsavepoint(1)` merges pre-images into slot 0
    pager.write_page(2).unwrap()[0..5].copy_from_slice(b"MERGD");
    pager.release_subsavepoint(1).unwrap();
    assert_eq!(pager.subsavepoint_depth(), 1);
    assert_eq!(&pager.read_page(2).unwrap()[0..5], b"MERGD");

    // Now `rollback_to_subsavepoint(0)` reverts ALL the way back to the 0-page empty DB!
    pager.rollback_to_subsavepoint(0).unwrap();
    assert_eq!(pager.subsavepoint_depth(), 1);
    assert_eq!(pager.page_count(), 0);
    assert_eq!(pager.header().database_size_pages, 0);

    // Committing after rolling back to 0 pages leaves a 0-byte empty file on disk!
    pager.commit().unwrap();
    assert_eq!(pager.transaction_state(), TransactionState::Open);
    assert_eq!(fs::metadata(&db_path).unwrap().len(), 0);

    // verify `DB_HEADER_SIZE` constant re-export
    assert_eq!(DB_HEADER_SIZE, 100);
    pager.close().unwrap();
}

// ============================================================================
// 11. Journal Finalization Fault Injection (`truncate` / `zero` / `sync` / `delete`) & Readonly Mode Switch
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum JournalFault {
    None,
    Truncate,
    ZeroHeaderWrite,
    Sync,
    Delete,
}

struct FaultVfs {
    inner: MemoryVfs,
    fault: Arc<Mutex<JournalFault>>,
}

impl FaultVfs {
    fn new() -> Self {
        Self {
            inner: MemoryVfs::new(),
            fault: Arc::new(Mutex::new(JournalFault::None)),
        }
    }

    fn set_fault(&self, f: JournalFault) {
        *self.fault.lock().unwrap() = f;
    }
}

struct FaultJournalFile {
    inner: Box<dyn VfsFile>,
    fault: Arc<Mutex<JournalFault>>,
}

impl VfsFile for FaultJournalFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> sqliters::types::Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> sqliters::types::Result<()> {
        if offset == 0
            && buf.len() == JOURNAL_HEADER_CORE_BYTES
            && buf.iter().all(|&b| b == 0)
            && *self.fault.lock().unwrap() == JournalFault::ZeroHeaderWrite
        {
            return Err(sqliters::types::SqliteError::with_extended(
                extended::SQLITE_IOERR_WRITE,
                "injected zeroJournalHdr 28-byte write fault",
            ));
        }
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> sqliters::types::Result<()> {
        if size == 0 && *self.fault.lock().unwrap() == JournalFault::Truncate {
            return Err(sqliters::types::SqliteError::with_extended(
                extended::SQLITE_IOERR_TRUNCATE,
                "injected journal truncate(0) fault",
            ));
        }
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> sqliters::types::Result<()> {
        if flags == SyncFlags::DataOnly && *self.fault.lock().unwrap() == JournalFault::Sync {
            return Err(sqliters::types::SqliteError::with_extended(
                extended::SQLITE_IOERR_FSYNC,
                "injected zeroJournalHdr DataOnly sync fault",
            ));
        }
        self.inner.sync(flags)
    }

    fn file_size(&self) -> sqliters::types::Result<u64> {
        self.inner.file_size()
    }

    fn lock(&self, level: LockLevel) -> sqliters::types::Result<()> {
        self.inner.lock(level)
    }

    fn unlock(&self, level: LockLevel) -> sqliters::types::Result<()> {
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&self) -> sqliters::types::Result<bool> {
        self.inner.check_reserved_lock()
    }

    fn lock_level(&self) -> LockLevel {
        self.inner.lock_level()
    }

    fn lock_state_for_diagnostics(&self) -> sqliters::types::Result<Option<LockLevel>> {
        self.inner.lock_state_for_diagnostics()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }

    fn close(&mut self) -> sqliters::types::Result<()> {
        self.inner.close()
    }
}

impl Vfs for FaultVfs {
    fn name(&self) -> &str {
        "fault-vfs"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(
        &self,
        path: Option<&Path>,
        flags: OpenFlags,
    ) -> sqliters::types::Result<Box<dyn VfsFile>> {
        let f = self.inner.open(path, flags)?;
        if flags.contains(OpenFlags::MAIN_JOURNAL) {
            return Ok(Box::new(FaultJournalFile {
                inner: f,
                fault: Arc::clone(&self.fault),
            }));
        }
        Ok(f)
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> sqliters::types::Result<()> {
        if *self.fault.lock().unwrap() == JournalFault::Delete {
            return Err(sqliters::types::SqliteError::with_extended(
                extended::SQLITE_IOERR_DELETE,
                "injected journal delete fault",
            ));
        }
        self.inner.delete(path, sync_dir)
    }

    fn exists(&self, path: &Path) -> sqliters::types::Result<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> sqliters::types::Result<PathBuf> {
        self.inner.full_pathname(path)
    }
}

#[test]
fn test_journal_finalization_fault_injection_and_readonly_mode_transitions() {
    let fvfs = Arc::new(FaultVfs::new());
    let vfs: Arc<dyn Vfs> = fvfs.clone();
    let db_path = Path::new("fault.db");
    let j_path = Path::new("fault.db-journal");

    let mut pager = Pager::open_with_options(
        vfs.clone(),
        Some(db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    pager.begin_write().unwrap();
    let p2 = pager.allocate_raw_page().unwrap();
    pager.write_page(p2).unwrap()[0..4].copy_from_slice(b"BASE");
    pager.commit().unwrap();

    let assert_ro_and_rw_see = |expected: &[u8; 4]| {
        let mut ro = Pager::open_with_options(
            vfs.clone(),
            Some(db_path),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
            PagerOpenOptions::default(),
        )
        .unwrap();
        ro.begin_read()
            .expect("READONLY begin_read must NOT fail with SQLITE_READONLY_ROLLBACK");
        assert_eq!(&ro.read_page(2).unwrap()[0..4], expected);
        ro.close().unwrap();

        let mut rw = Pager::open_with_options(
            vfs.clone(),
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
            PagerOpenOptions::default(),
        )
        .unwrap();
        rw.begin_read().unwrap();
        assert_eq!(&rw.read_page(2).unwrap()[0..4], expected);
        rw.close().unwrap();
    };

    // ------------------------------------------------------------------------
    // Part A: Fault during `commit()` followed by `rollback()`
    // ------------------------------------------------------------------------

    // A1) `Truncate` mode `truncate(0)` fault -> `rollback()` restores `BASE` & truncates journal!
    pager.set_journal_mode(JournalMode::Truncate).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"MOD1");
    fvfs.set_fault(JournalFault::Truncate);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.rollback().unwrap();
    assert_ro_and_rw_see(b"BASE");

    // A2) `Persist` mode 28-byte `zeroJournalHdr` write fault -> `rollback()` restores `BASE` & zeroes header!
    pager.set_journal_mode(JournalMode::Persist).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"MOD2");
    fvfs.set_fault(JournalFault::ZeroHeaderWrite);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.rollback().unwrap();
    assert_ro_and_rw_see(b"BASE");

    // A3) `Persist` mode `DataOnly` sync fault -> `rollback()` restores `BASE` & zeroes header!
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"SYNC");
    fvfs.set_fault(JournalFault::Sync);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.rollback().unwrap();
    assert_ro_and_rw_see(b"BASE");

    // A4) `Delete` mode `vfs.delete` fault -> `journal_file` was already closed, yet `rollback()`
    // MUST still unlink `<db>-journal` so a subsequent `READONLY` reader never hits `SQLITE_READONLY_ROLLBACK`!
    pager.set_journal_mode(JournalMode::Delete).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"MOD3");
    fvfs.set_fault(JournalFault::Delete);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    assert!(
        vfs.exists(j_path).unwrap(),
        "hot journal remains while delete is faulted"
    );
    fvfs.set_fault(JournalFault::None);
    pager.rollback().unwrap();
    assert!(
        !vfs.exists(j_path).unwrap(),
        "rollback() after failed Delete commit MUST unlink <db>-journal"
    );
    assert_ro_and_rw_see(b"BASE");

    // ------------------------------------------------------------------------
    // Part B: Fault during `commit()` Phase 2 followed by direct `commit()` RETRY (`commit_phase_one_done`)
    // ------------------------------------------------------------------------

    // B1) `Truncate` fault -> direct `commit()` retry preserves `"RT_T"` and truncates journal to 0!
    pager.set_journal_mode(JournalMode::Truncate).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"RT_T");
    fvfs.set_fault(JournalFault::Truncate);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.commit().unwrap();
    assert_eq!(pager.transaction_state(), TransactionState::Open);
    assert_ro_and_rw_see(b"RT_T");

    // B2) `Persist` `ZeroHeaderWrite` fault -> direct `commit()` retry zeroes 28B & preserves `"RT_Z"`!
    pager.set_journal_mode(JournalMode::Persist).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"RT_Z");
    fvfs.set_fault(JournalFault::ZeroHeaderWrite);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.commit().unwrap();
    assert_ro_and_rw_see(b"RT_Z");

    // B3) `Persist` `Sync` (`DataOnly`) fault (after 28B were zeroed!) -> direct `commit()` retry
    // MUST NOT re-enter Phase 1 (`sync_journal_before_db_write`) or rewrite `JOURNAL_MAGIC` at offset 0!
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"RT_S");
    fvfs.set_fault(JournalFault::Sync);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    fvfs.set_fault(JournalFault::None);
    pager.commit().unwrap();
    {
        let mut jf = vfs.open(Some(j_path), OpenFlags::READONLY).unwrap();
        let mut hdr28 = [0xFFu8; JOURNAL_HEADER_CORE_BYTES];
        jf.read_at(&mut hdr28, 0).unwrap();
        jf.close().unwrap();
        assert_eq!(hdr28, [0u8; JOURNAL_HEADER_CORE_BYTES]);
    }
    assert_ro_and_rw_see(b"RT_S");

    // B4) `Delete` fault -> direct `commit()` retry unlinks `<db>-journal` even though `journal_file`
    // was already closed on the first attempt, preventing the next reader from rolling back `"RT_D"`!
    pager.set_journal_mode(JournalMode::Delete).unwrap();
    pager.begin_write().unwrap();
    pager.write_page(2).unwrap()[0..4].copy_from_slice(b"RT_D");
    fvfs.set_fault(JournalFault::Delete);
    assert_eq!(pager.commit().unwrap_err().code, ErrorCode::IoErr);
    assert_eq!(pager.transaction_state(), TransactionState::Write);
    assert!(vfs.exists(j_path).unwrap());
    fvfs.set_fault(JournalFault::None);
    pager.commit().unwrap();
    assert!(
        !vfs.exists(j_path).unwrap(),
        "commit() retry after failed Delete MUST unlink <db>-journal"
    );
    assert_ro_and_rw_see(b"RT_D");

    // ------------------------------------------------------------------------
    // Part C: Readonly handle mode transitions (`Persist -> Delete` in both `Open` and `Read`)
    // ------------------------------------------------------------------------
    let mut ro_pager = Pager::open_with_options(
        vfs,
        Some(db_path),
        OpenFlags::READONLY | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert!(ro_pager.is_readonly());
    assert_eq!(
        ro_pager.set_journal_mode(JournalMode::Persist).unwrap(),
        JournalMode::Persist
    );
    assert_eq!(
        ro_pager.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Delete
    );
    ro_pager.begin_read().unwrap();
    assert_eq!(
        ro_pager.set_journal_mode(JournalMode::Persist).unwrap(),
        JournalMode::Persist
    );
    assert_eq!(
        ro_pager.set_journal_mode(JournalMode::Delete).unwrap(),
        JournalMode::Delete
    );
    assert_eq!(ro_pager.transaction_state(), TransactionState::Read);
    ro_pager.close().unwrap();
    pager.close().unwrap();
}

// ============================================================================
// 12. Lazy-Open `set_page_size` / `set_encoding` Guards & Subsavepoint Page 1 Header Sync
// ============================================================================

#[test]
fn test_lazy_open_set_page_size_and_set_encoding_preserve_populated_db_and_subsavepoint_p1_sync() {
    let sandbox = TempSandbox::new("lazy_setters");
    let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());

    // (A) 0-byte empty file: `set_page_size(8192)` and `set_encoding(Utf16le)` right after lazy open
    // apply cleanly while keeping the pager in `Open/NoLock`!
    let empty_path = sandbox.path().join("empty_cfg.db");
    let mut p_empty = Pager::open_with_options(
        vfs.clone(),
        Some(&empty_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert!(!p_empty.is_header_loaded());
    p_empty.set_page_size(8192).unwrap();
    p_empty.set_encoding(TextEncoding::Utf16Le).unwrap();
    assert_eq!(p_empty.transaction_state(), TransactionState::Open);
    assert_eq!(
        p_empty.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    assert_eq!(p_empty.page_size(), 8192);
    assert_eq!(p_empty.encoding(), TextEncoding::Utf16Le);

    p_empty.begin_write().unwrap();
    assert_eq!(p_empty.allocate_raw_page().unwrap(), 2);
    p_empty.commit().unwrap();
    p_empty.close().unwrap();
    assert_eq!(fs::metadata(&empty_path).unwrap().len(), 16_384);

    // Reopen lazily and verify `effective_page_size` & `effective_encoding`!
    let mut p_verify = Pager::open_with_options(
        vfs.clone(),
        Some(&empty_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert!(!p_verify.is_header_loaded());
    assert_eq!(p_verify.effective_page_size().unwrap(), 8192);
    assert_eq!(
        p_verify.effective_encoding().unwrap(),
        TextEncoding::Utf16Le
    );
    assert_eq!(p_verify.transaction_state(), TransactionState::Open);
    assert_eq!(
        p_verify.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    p_verify.close().unwrap();

    // (B) Populated `4096` / `Utf8` DB opened lazily (`!is_header_loaded()`):
    // calling `set_page_size(8192)` and `set_encoding(Utf16le)` MUST load the real header, act as
    // safe no-ops preserving `4096` / `Utf8`, and leave the pager in `Open/NoLock`!
    let pop_path = sandbox.path().join("populated_4k.db");
    let mut seed = Pager::open_with_options(
        vfs.clone(),
        Some(&pop_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    seed.begin_write().unwrap();
    let sp2 = seed.allocate_raw_page().unwrap();
    seed.write_page(sp2).unwrap()[0..4].copy_from_slice(b"UTF8");
    seed.commit().unwrap();
    seed.close().unwrap();

    let mut reopened = Pager::open_with_options(
        vfs,
        Some(&pop_path),
        OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        PagerOpenOptions::default(),
    )
    .unwrap();
    assert!(!reopened.is_header_loaded());

    reopened.set_page_size(8192).unwrap();
    reopened.set_encoding(TextEncoding::Utf16Le).unwrap();

    assert!(reopened.is_header_loaded());
    assert_eq!(reopened.transaction_state(), TransactionState::Open);
    assert_eq!(
        reopened.lock_state_for_diagnostics().unwrap(),
        Some(LockLevel::NoLock)
    );
    assert_eq!(reopened.page_size(), 4096);
    assert_eq!(reopened.encoding(), TextEncoding::Utf8);
    assert_eq!(reopened.effective_encoding().unwrap(), TextEncoding::Utf8);

    // Subsequent write + commit continues using `4096` / `Utf8` without corruption!
    reopened.begin_write().unwrap();
    reopened.write_page(2).unwrap()[4..8].copy_from_slice(b"_OK!");
    reopened.commit().unwrap();
    assert_eq!(fs::metadata(&pop_path).unwrap().len(), 8192); // 2 * 4096

    // (C) `apply_subsavepoint_undo` keeps cached Page 1 bytes `0..100` synchronized with `orig_header`!
    reopened.begin_write().unwrap();
    reopened.header_mut().unwrap().user_version = 777;
    reopened.ensure_subsavepoints(1).unwrap();
    reopened.header_mut().unwrap().user_version = 999;
    {
        let hdr_999 = reopened.header().clone();
        let p1_bytes = reopened.write_page(1).unwrap();
        hdr_999.write_into(&mut p1_bytes[0..DB_HEADER_SIZE]);
    }
    assert_eq!(
        DatabaseHeader::parse(&reopened.read_page(1).unwrap()[0..DB_HEADER_SIZE])
            .unwrap()
            .user_version,
        999
    );

    reopened.rollback_to_subsavepoint(0).unwrap();
    assert_eq!(reopened.header().user_version, 777);
    assert_eq!(
        DatabaseHeader::parse(&reopened.read_page(1).unwrap()[0..DB_HEADER_SIZE])
            .unwrap()
            .user_version,
        777,
        "apply_subsavepoint_undo must synchronize Page 1 header bytes 0..100 with restored header"
    );
    reopened.commit().unwrap();
    reopened.close().unwrap();
}
