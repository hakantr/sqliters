//! Phase 1.1-C (`sqliters-btree` Incremental `BtreeCursor` + `sqliters-vdbe` VM Cursor/Interrupt)
//! Integration & C SQLite 3.54.1 Differential Test Suite:
//! 1. `SELECT ... LIMIT 1` lazy page & overflow guarantee (`SpyVfs` page read counter).
//! 2. Same-connection mid-scan mutations (`Delete`, splits, leaf merges/freelist) across
//!    Table B+Tree & multi-level Index B-Tree (`save_all_cursors` + `RequireSeek` + `SkipNext`)
//!    with C SQLite 3.54.1 differential verification.
//! 3. Multi-level Index B-Tree (`IndexInterior` + `IndexLeaf`, `ASC`/`DESC`, `NULLS`, collations,
//!    overflow keys) forward/reverse/prefix-equal in-order cursor navigation.
//! 4. Rollback `trip_cursors` (`write_only = false` & `write_only = true`) + `SQLITE_ABORT_ROLLBACK`.
//! 5. Atomic `Interrupt` hook (`Arc<AtomicBool>`) mid-scan and mid-overflow-chain.
//! 6. Zero cursor leak across `Done`, `reset_with_btree`, `Halt` error, `UNIQUE` abort, and `Statement` drop/reset.
#![forbid(unsafe_code)]

use std::cmp::Ordering as CmpOrdering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters::btree::{
    Btree, BtreeCursorState, BtreePageType, CursorKind, ParsedPage, SkipNext, raw_page_header,
};
use sqliters::pager::TransactionState;
use sqliters::types::{
    Collation, ErrorCode, KeySortOrder, LockGeometry, Pgno, Result, Value, decode_record,
    encode_record, extended,
};
use sqliters::vdbe::{
    DefaultVmServices, Instruction, Opcode, P4, VdbeProgram, VdbeVm, VmRuntimeContext, VmStep,
};
use sqliters::vfs::{LockLevel, MemoryVfs, OpenFlags, SyncFlags, Vfs, VfsFile};
use sqliters::{Connection, StepResult};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p1c_{prefix}_{pid}_{id}"));
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

// ============================================================================
// SpyVfs: Tracks exact `MAIN_DB` `read_at` calls, read offsets, & optional interrupt trigger
// ============================================================================

#[derive(Default)]
struct SpyReadTracker {
    main_db_read_calls: AtomicUsize,
    read_offsets: Mutex<Vec<(u64, usize)>>,
    interrupt_after_reads: AtomicUsize,
    interrupt_target: Mutex<Option<Arc<AtomicBool>>>,
}

impl SpyReadTracker {
    fn reset(&self) {
        self.main_db_read_calls.store(0, Ordering::SeqCst);
        self.read_offsets.lock().unwrap().clear();
    }

    fn count(&self) -> usize {
        self.main_db_read_calls.load(Ordering::SeqCst)
    }

    fn distinct_pages_read(&self, page_size: u32) -> Vec<Pgno> {
        let ps = u64::from(page_size);
        let mut pages: Vec<Pgno> = self
            .read_offsets
            .lock()
            .unwrap()
            .iter()
            .map(|&(off, _)| (off / ps) as Pgno + 1)
            .collect();
        pages.sort_unstable();
        pages.dedup();
        pages
    }
}

struct SpyVfs {
    inner: Arc<MemoryVfs>,
    tracker: Arc<SpyReadTracker>,
}

impl SpyVfs {
    fn new() -> Self {
        Self {
            inner: Arc::new(MemoryVfs::new()),
            tracker: Arc::new(SpyReadTracker::default()),
        }
    }
}

struct SpyMainDbFile {
    inner: Box<dyn VfsFile>,
    tracker: Arc<SpyReadTracker>,
}

impl VfsFile for SpyMainDbFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        let prev = self
            .tracker
            .main_db_read_calls
            .fetch_add(1, Ordering::SeqCst);
        self.tracker
            .read_offsets
            .lock()
            .unwrap()
            .push((offset, buf.len()));
        let trigger_at = self.tracker.interrupt_after_reads.load(Ordering::SeqCst);
        if trigger_at > 0
            && prev + 1 >= trigger_at
            && let Some(flag) = self.tracker.interrupt_target.lock().unwrap().as_ref()
        {
            flag.store(true, Ordering::Relaxed);
        }
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
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

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

impl Vfs for SpyVfs {
    fn name(&self) -> &str {
        "spy-mem-p1c"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let f = self.inner.open(path, flags)?;
        if flags.contains(OpenFlags::MAIN_DB) {
            Ok(Box::new(SpyMainDbFile {
                inner: f,
                tracker: Arc::clone(&self.tracker),
            }))
        } else {
            Ok(f)
        }
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
// Out-of-Tree C SQLite 3.54.1 Reference Helper (`target/sqlite-ref/`)
// ============================================================================

const C_CURSOR_HELPER_SOURCE: &str = r#"
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

static void exec_sql(sqlite3 *db, const char *sql) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "C helper exec error: rc=%d msg=%s sql=%s\n",
                rc, errmsg ? errmsg : "", sql);
        sqlite3_free(errmsg);
        exit(2);
    }
}

/* Forward scan `SELECT id, length(payload) FROM t ORDER BY id ASC` while mutating on the SAME connection:
 * - delete current row when id % 3 == 0
 * - when id == 25: delete neighbor range 26..40 (merges/frees leaves) and insert rows -20..-1 (behind cursor) + 501..530 (ahead of cursor, splitting pages)
 * - when id == 90: delete 90 and insert 910..925 (ahead of cursor)
 */
static void cmd_forward_scan_mutate(const char *db_path) {
    sqlite3 *db = NULL;
    int rc = sqlite3_open(db_path, &db);
    if (rc != SQLITE_OK) die_rc(db, "open", rc);
    sqlite3_extended_result_codes(db, 1);

    exec_sql(db, "BEGIN;");
    sqlite3_stmt *scan = NULL;
    rc = sqlite3_prepare_v2(db, "SELECT id, length(payload) FROM t ORDER BY id ASC", -1, &scan, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare scan", rc);

    char buf[256];
    while ((rc = sqlite3_step(scan)) == SQLITE_ROW) {
        long long id = sqlite3_column_int64(scan, 0);
        int plen = sqlite3_column_int(scan, 1);
        printf("VISIT:%lld:%d\n", id, plen);

        if (id == 25) {
            exec_sql(db, "DELETE FROM t WHERE id BETWEEN 26 AND 40;");
            for (int k = -20; k <= -1; k++) {
                snprintf(buf, sizeof(buf),
                         "INSERT INTO t VALUES(%d, zeroblob(900));", k);
                exec_sql(db, buf);
            }
            for (int k = 501; k <= 530; k++) {
                snprintf(buf, sizeof(buf),
                         "INSERT INTO t VALUES(%d, zeroblob(900));", k);
                exec_sql(db, buf);
            }
        }
        if (id == 90) {
            for (int k = 910; k <= 925; k++) {
                snprintf(buf, sizeof(buf),
                         "INSERT INTO t VALUES(%d, zeroblob(900));", k);
                exec_sql(db, buf);
            }
        }
        if (id > 0 && (id % 3) == 0) {
            snprintf(buf, sizeof(buf), "DELETE FROM t WHERE id = %lld;", id);
            exec_sql(db, buf);
        }
    }
    if (rc != SQLITE_DONE) die_rc(db, "step scan", rc);
    sqlite3_finalize(scan);
    exec_sql(db, "COMMIT;");

    sqlite3_stmt *fin = NULL;
    rc = sqlite3_prepare_v2(db, "SELECT id, length(payload) FROM t ORDER BY id ASC", -1, &fin, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare final", rc);
    while ((rc = sqlite3_step(fin)) == SQLITE_ROW) {
        printf("FINAL:%lld:%d\n", sqlite3_column_int64(fin, 0), sqlite3_column_int(fin, 1));
    }
    if (rc != SQLITE_DONE) die_rc(db, "step final", rc);
    sqlite3_finalize(fin);
    sqlite3_close(db);
}

/* Reverse scan `SELECT id, length(payload) FROM t ORDER BY id DESC` while mutating on the SAME connection */
static void cmd_reverse_scan_mutate(const char *db_path) {
    sqlite3 *db = NULL;
    int rc = sqlite3_open(db_path, &db);
    if (rc != SQLITE_OK) die_rc(db, "open", rc);
    sqlite3_extended_result_codes(db, 1);

    exec_sql(db, "BEGIN;");
    sqlite3_stmt *scan = NULL;
    rc = sqlite3_prepare_v2(db, "SELECT id, length(payload) FROM t ORDER BY id DESC", -1, &scan, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare desc scan", rc);

    char buf[256];
    while ((rc = sqlite3_step(scan)) == SQLITE_ROW) {
        long long id = sqlite3_column_int64(scan, 0);
        int plen = sqlite3_column_int(scan, 1);
        printf("VISIT:%lld:%d\n", id, plen);

        if (id == 95) {
            exec_sql(db, "DELETE FROM t WHERE id BETWEEN 75 AND 88;");
            for (int k = 600; k <= 620; k++) {
                snprintf(buf, sizeof(buf),
                         "INSERT INTO t VALUES(%d, zeroblob(900));", k);
                exec_sql(db, buf);
            }
            for (int k = -15; k <= -1; k++) {
                snprintf(buf, sizeof(buf),
                         "INSERT INTO t VALUES(%d, zeroblob(900));", k);
                exec_sql(db, buf);
            }
        }
        if (id > 0 && (id % 4) == 0) {
            snprintf(buf, sizeof(buf), "DELETE FROM t WHERE id = %lld;", id);
            exec_sql(db, buf);
        }
    }
    if (rc != SQLITE_DONE) die_rc(db, "step desc scan", rc);
    sqlite3_finalize(scan);
    exec_sql(db, "COMMIT;");

    sqlite3_stmt *fin = NULL;
    rc = sqlite3_prepare_v2(db, "SELECT id, length(payload) FROM t ORDER BY id ASC", -1, &fin, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare final", rc);
    while ((rc = sqlite3_step(fin)) == SQLITE_ROW) {
        printf("FINAL:%lld:%d\n", sqlite3_column_int64(fin, 0), sqlite3_column_int(fin, 1));
    }
    if (rc != SQLITE_DONE) die_rc(db, "step final", rc);
    sqlite3_finalize(fin);
    sqlite3_close(db);
}

/* Verifies C SQLite rollback trip extended error code on an active reader statement:
 * 1) Schema-changing rollback (`writeOnly = 0`): reader gets `SQLITE_ABORT` (4) / `SQLITE_ABORT_ROLLBACK` (516)!
 * 2) Data-only rollback (`writeOnly = 1`): reader cursor is saved (`RequireSeek`) and steps to row 2 (`SQLITE_ROW` = 100)!
 */
static void cmd_rollback_trip_probe(const char *db_path) {
    sqlite3 *db = NULL;
    int rc = sqlite3_open(db_path, &db);
    if (rc != SQLITE_OK) die_rc(db, "open", rc);
    sqlite3_extended_result_codes(db, 1);

    exec_sql(db, "BEGIN; CREATE TABLE ddl_trigger_trip(x INT);");
    sqlite3_stmt *scan = NULL;
    rc = sqlite3_prepare_v2(db, "SELECT id FROM t ORDER BY id ASC", -1, &scan, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare", rc);

    rc = sqlite3_step(scan);
    if (rc != SQLITE_ROW) die_rc(db, "first step", rc);
    long long first_id = sqlite3_column_int64(scan, 0);

    exec_sql(db, "ROLLBACK;");
    int next_rc = sqlite3_step(scan);
    int ext = sqlite3_extended_errcode(db);
    printf("TRIP_DDL:first=%lld:primary=%d:extended=%d\n", first_id, next_rc & 0xff, ext);
    sqlite3_finalize(scan);

    exec_sql(db, "BEGIN; INSERT INTO t VALUES(9999, zeroblob(64));");
    rc = sqlite3_prepare_v2(db, "SELECT id FROM t ORDER BY id ASC", -1, &scan, NULL);
    if (rc != SQLITE_OK) die_rc(db, "prepare2", rc);
    rc = sqlite3_step(scan);
    if (rc != SQLITE_ROW) die_rc(db, "first step2", rc);
    exec_sql(db, "ROLLBACK;");
    next_rc = sqlite3_step(scan);
    long long second_id = (next_rc == SQLITE_ROW) ? sqlite3_column_int64(scan, 0) : -1;
    printf("TRIP_DATA:rc=%d:second=%lld\n", next_rc, second_id);
    sqlite3_finalize(scan);

    sqlite3_close(db);
}

int main(int argc, char **argv) {
    if (argc < 2) return 1;
    if (strcmp(argv[1], "version") == 0) {
        printf("%s\n", sqlite3_libversion());
        return 0;
    }
    if (argc >= 3 && strcmp(argv[1], "forward_scan_mutate") == 0) {
        cmd_forward_scan_mutate(argv[2]);
        return 0;
    }
    if (argc >= 3 && strcmp(argv[1], "reverse_scan_mutate") == 0) {
        cmd_reverse_scan_mutate(argv[2]);
        return 0;
    }
    if (argc >= 3 && strcmp(argv[1], "rollback_trip_probe") == 0) {
        cmd_rollback_trip_probe(argv[2]);
        return 0;
    }
    return 1;
}
"#;

fn c_cursor_helper() -> &'static Path {
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

        let helper_c = ref_root.join("c_btree_cursor_helper.c");
        let helper_bin = ref_root.join("c_btree_cursor_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_CURSOR_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            fs::write(&helper_c, C_CURSOR_HELPER_SOURCE).expect("write c_btree_cursor_helper.c");
            let cc_out = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&helper_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&helper_bin)
                .output()
                .expect("compile c_btree_cursor_helper");
            assert!(
                cc_out.status.success(),
                "failed compiling c_btree_cursor_helper:\n{}",
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
            .expect("run c_btree_cursor_helper version");
        assert!(ver_out.status.success());
        assert_eq!(String::from_utf8_lossy(&ver_out.stdout).trim(), "3.54.1");
        helper_bin
    })
}

fn measure_table_tree_height(bt: &mut Btree, root_pgno: Pgno) -> usize {
    let usable = bt.pager().usable_size();
    let mut pgno = root_pgno;
    let mut height = 0usize;
    loop {
        height += 1;
        let raw = bt.pager_mut().read_page(pgno).expect("read page");
        let (ptype, n_cell, right_child) = raw_page_header(pgno, usable, raw).expect("hdr");
        match ptype {
            BtreePageType::TableLeaf => return height,
            BtreePageType::TableInterior => {
                pgno = if n_cell > 0 {
                    sqliters::btree::read_table_interior_cell(pgno, usable, raw, n_cell, 0)
                        .expect("interior cell")
                        .0
                } else {
                    right_child
                };
            }
            other => panic!("unexpected page type {other:?}"),
        }
    }
}

// ============================================================================
// 1. `SELECT ... LIMIT 1` Lazy Page & Overflow Guarantee (`SpyVfs`)
// ============================================================================

#[test]
fn test_select_limit_1_and_cursor_column_lazy_page_and_overflow_guarantee() {
    let vfs = Arc::new(SpyVfs::new());
    let db_path = Path::new("/lazy_limit1.db");

    // Populate a 240-row table on 512-byte pages where every row carries a 2,400-byte overflow BLOB
    // (5+ overflow pages per row => > 1,200 pages total!).
    let (tbl_root, tree_height, total_pages) = {
        let conn = Connection::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        conn.execute(
            "PRAGMA page_size = 512;
             CREATE TABLE big (
                 id INTEGER PRIMARY KEY,
                 short_col TEXT NOT NULL,
                 mid_blob BLOB NOT NULL,
                 tail_blob BLOB NOT NULL
             );",
        )
        .unwrap();

        conn.execute("BEGIN;").unwrap();
        for id in 1..=240i64 {
            let short = format!("short-{id:04}");
            let mid = vec![(id & 0xff) as u8; 700]; // ~2 overflow pages on 512B pages
            let tail = vec![0xA5u8; 2_200]; // ~5 more overflow pages on 512B pages
            conn.execute_params(
                "INSERT INTO big VALUES (?1, ?2, ?3, ?4)",
                &[
                    Value::Integer(id),
                    Value::Text(short),
                    Value::Blob(mid),
                    Value::Blob(tail),
                ],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        let root = conn
            .schema_snapshot()
            .unwrap()
            .find_table("big")
            .unwrap()
            .root_pgno;
        let pcnt = conn.page_count().unwrap();
        drop(conn);

        let mut bt = Btree::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt.begin_read().unwrap();
        let height = measure_table_tree_height(&mut bt, root);
        bt.commit().unwrap();
        assert!(
            height >= 2,
            "expected multi-level B+Tree, got height={height}"
        );
        assert!(
            pcnt > 1_400,
            "expected >1400 pages with overflow chains, got {pcnt}"
        );
        (root, height, pcnt)
    };

    // Open a completely fresh Connection with a cold Pager cache!
    vfs.tracker.reset();
    {
        let conn = Connection::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READWRITE | OpenFlags::MAIN_DB,
        )
        .unwrap();
        // Warm schema once, then reset SpyVfs counter to measure ONLY
        // `prepare("SELECT id, short_col FROM big ORDER BY id LIMIT 1")` + first `step()`!
        let _ = conn.schema_snapshot().unwrap();
        vfs.tracker.reset();

        let mut stmt = conn
            .prepare("SELECT id, short_col FROM big ORDER BY id LIMIT 1")
            .unwrap();
        let first = stmt.step().unwrap();
        assert_eq!(
            first,
            StepResult::Row(sqliters::Row::new(vec![
                Value::Integer(1),
                Value::Text("short-0001".to_owned()),
            ]))
        );

        let distinct_pages = vfs.tracker.distinct_pages_read(512);
        let read_calls = vfs.tracker.count();
        assert!(
            distinct_pages.len() <= tree_height + 1,
            "SELECT ... ORDER BY id LIMIT 1 must read <= tree_height + 1 pages ({} + 1), \
             but read {} distinct pages ({read_calls} calls) out of {total_pages}: {distinct_pages:?}",
            tree_height,
            distinct_pages.len(),
        );

        // Step to Done and ensure zero cursors remain open!
        assert_eq!(stmt.step().unwrap(), StepResult::Done);
        drop(stmt);
        assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);
    }

    // Also verify cold-cache BtreeCursor `cursor_column` directly:
    // 1. Reading `short_col` (column 1, completely inside `local_payload`) reads ZERO overflow pages!
    // 2. Reading `mid_blob` (column 2, ~700B) reads ONLY its 2 overflow pages and NEVER touches `tail_blob`'s 5 trailing overflow pages!
    {
        let mut bt = Btree::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt.begin_read().unwrap();
        vfs.tracker.reset();

        let cur = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_first(cur).unwrap());
        let reads_after_first = vfs.tracker.distinct_pages_read(512).len();
        assert_eq!(
            reads_after_first, tree_height,
            "cursor_first must read exact root-to-leftmost-leaf path"
        );

        // `cursor_rowid` + `cursor_column(cur, 1)` (`short_col` in local_payload): ZERO additional page reads!
        vfs.tracker.reset();
        assert_eq!(bt.cursor_rowid(cur).unwrap(), 1);
        assert_eq!(
            bt.cursor_column(cur, 1).unwrap(),
            Some(Value::Text("short-0001".to_owned()))
        );
        assert_eq!(
            vfs.tracker.count(),
            0,
            "cursor_column on local_payload column must touch ZERO overflow pages!"
        );

        // `cursor_column(cur, 2)` (`mid_blob`, 700 bytes => 2 overflow pages on 512B pages):
        // must NOT walk the 5 trailing overflow pages belonging to `tail_blob`!
        vfs.tracker.reset();
        let mid_val = bt.cursor_column(cur, 2).unwrap().unwrap();
        let Value::Blob(mid_bytes) = mid_val else {
            panic!("expected Blob for mid_blob");
        };
        assert_eq!(mid_bytes.len(), 700);
        let mid_ovfl_reads = vfs.tracker.distinct_pages_read(512).len();
        assert!(
            (1..=2).contains(&mid_ovfl_reads),
            "expected 1..=2 overflow page reads for mid_blob, got {mid_ovfl_reads}"
        );

        bt.close_cursor(cur);
        assert_eq!(bt.open_cursor_count(), 0);
        bt.commit().unwrap();
    }
}

// ============================================================================
// 2. Same-Connection Mid-Scan Mutations (`Delete` + `split` + `merge`/freelist)
//    & Differential Comparison with C SQLite 3.54.1
// ============================================================================

fn seed_differential_mutation_db(path: &Path) {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "PRAGMA page_size = 512;
         CREATE TABLE t (id INTEGER PRIMARY KEY, payload BLOB NOT NULL);
         BEGIN;",
    )
    .unwrap();
    for id in 1..=120i64 {
        let len = if id % 7 == 0 { 950 } else { 140 };
        conn.execute_params(
            "INSERT INTO t VALUES (?1, ?2)",
            &[Value::Integer(id), Value::Blob(vec![0u8; len])],
        )
        .unwrap();
    }
    conn.execute("COMMIT;").unwrap();
}

type RowLenPairs = Vec<(i64, i64)>;

fn parse_c_helper_lines(stdout: &str) -> (RowLenPairs, RowLenPairs) {
    let mut visited = Vec::new();
    let mut final_rows = Vec::new();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("VISIT:") {
            let mut parts = rest.split(':');
            let id: i64 = parts.next().unwrap().parse().unwrap();
            let plen: i64 = parts.next().unwrap().parse().unwrap();
            visited.push((id, plen));
        } else if let Some(rest) = line.strip_prefix("FINAL:") {
            let mut parts = rest.split(':');
            let id: i64 = parts.next().unwrap().parse().unwrap();
            let plen: i64 = parts.next().unwrap().parse().unwrap();
            final_rows.push((id, plen));
        }
    }
    (visited, final_rows)
}

#[test]
fn test_table_cursor_mid_scan_mutations_forward_and_reverse_match_c_sqlite() {
    let helper = c_cursor_helper();
    let sandbox = TempSandbox::new("diff_mut");

    // --- Part A: Forward scan with current-row deletes, neighbor leaf merges, and behind/ahead page splits ---
    let c_fwd_db = sandbox.path().join("c_fwd.db");
    let rs_fwd_db = sandbox.path().join("rs_fwd.db");
    seed_differential_mutation_db(&c_fwd_db);
    fs::copy(&c_fwd_db, &rs_fwd_db).unwrap();

    let c_out = Command::new(helper)
        .arg("forward_scan_mutate")
        .arg(&c_fwd_db)
        .output()
        .expect("run C forward_scan_mutate");
    assert!(
        c_out.status.success(),
        "C helper failed:\n{}",
        String::from_utf8_lossy(&c_out.stderr)
    );
    let (c_visited_fwd, c_final_fwd) =
        parse_c_helper_lines(&String::from_utf8(c_out.stdout).unwrap());

    let (rs_visited_fwd, rs_final_fwd) = {
        let tbl_root = {
            let conn = Connection::open(&rs_fwd_db).unwrap();
            conn.schema_snapshot()
                .unwrap()
                .find_table("t")
                .unwrap()
                .root_pgno
        };

        let mut bt =
            Btree::open_file(&rs_fwd_db, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        bt.begin_write().unwrap();

        let mut visited = Vec::new();
        let cur = bt.open_table_cursor(tbl_root, false);
        let mut has_row = bt.cursor_first(cur).unwrap();
        while has_row {
            let id = bt.cursor_rowid(cur).unwrap();
            let Value::Blob(blob_bytes) = bt.cursor_column(cur, 1).unwrap().unwrap() else {
                panic!("expected Blob payload");
            };
            let plen = blob_bytes.len() as i64;
            visited.push((id, plen));

            if id == 25 {
                for del_id in 26..=40i64 {
                    bt.delete_table(tbl_root, del_id).unwrap();
                }
                // Verify mutation invalidated `cur` into `RequireSeek`
                assert!(matches!(
                    bt.cursor_state(cur),
                    Some(BtreeCursorState::RequireSeek { .. })
                ));
                for k in -20..=-1i64 {
                    let rec = encode_record(&[Value::Null, Value::Blob(vec![0u8; 900])]);
                    bt.insert_table(tbl_root, k, &rec).unwrap();
                }
                for k in 501..=530i64 {
                    let rec = encode_record(&[Value::Null, Value::Blob(vec![0u8; 900])]);
                    bt.insert_table(tbl_root, k, &rec).unwrap();
                }
            }
            if id == 90 {
                for k in 910..=925i64 {
                    let rec = encode_record(&[Value::Null, Value::Blob(vec![0u8; 900])]);
                    bt.insert_table(tbl_root, k, &rec).unwrap();
                }
            }
            if id > 0 && id % 3 == 0 {
                assert!(bt.delete_table(tbl_root, id).unwrap());
            }

            has_row = bt.cursor_next(cur).unwrap();
        }
        bt.close_cursor(cur);
        assert_eq!(
            bt.integrity_check(&[1, tbl_root]).unwrap(),
            Vec::<String>::new()
        );
        bt.commit().unwrap();
        drop(bt);

        let conn = Connection::open(&rs_fwd_db).unwrap();
        let q = conn
            .query("SELECT id, length(payload) FROM t ORDER BY id ASC", &[])
            .unwrap();
        let final_rows: Vec<(i64, i64)> = q
            .rows
            .iter()
            .map(|r| (r.get_i64(0).unwrap(), r.get_i64(1).unwrap()))
            .collect();
        (visited, final_rows)
    };

    assert_eq!(
        rs_visited_fwd, c_visited_fwd,
        "forward mid-scan visited row sequence must match C SQLite 3.54.1 100%"
    );
    assert_eq!(
        rs_final_fwd, c_final_fwd,
        "forward mid-scan final table state must match C SQLite 3.54.1 100%"
    );

    // --- Part B: Reverse scan (`cursor_last` -> `cursor_prev`) with deletes, merges, and splits ---
    let c_rev_db = sandbox.path().join("c_rev.db");
    let rs_rev_db = sandbox.path().join("rs_rev.db");
    seed_differential_mutation_db(&c_rev_db);
    fs::copy(&c_rev_db, &rs_rev_db).unwrap();

    let c_rev_out = Command::new(helper)
        .arg("reverse_scan_mutate")
        .arg(&c_rev_db)
        .output()
        .expect("run C reverse_scan_mutate");
    assert!(
        c_rev_out.status.success(),
        "C helper reverse failed:\n{}",
        String::from_utf8_lossy(&c_rev_out.stderr)
    );
    let (c_visited_rev, c_final_rev) =
        parse_c_helper_lines(&String::from_utf8(c_rev_out.stdout).unwrap());

    let (rs_visited_rev, rs_final_rev) = {
        let tbl_root = {
            let conn = Connection::open(&rs_rev_db).unwrap();
            conn.schema_snapshot()
                .unwrap()
                .find_table("t")
                .unwrap()
                .root_pgno
        };

        let mut bt =
            Btree::open_file(&rs_rev_db, OpenFlags::READWRITE | OpenFlags::MAIN_DB).unwrap();
        bt.begin_write().unwrap();

        let mut visited = Vec::new();
        let cur = bt.open_table_cursor(tbl_root, false);
        let mut has_row = bt.cursor_last(cur).unwrap();
        while has_row {
            let id = bt.cursor_rowid(cur).unwrap();
            let Value::Blob(blob_bytes) = bt.cursor_column(cur, 1).unwrap().unwrap() else {
                panic!("expected Blob payload");
            };
            let plen = blob_bytes.len() as i64;
            visited.push((id, plen));

            if id == 95 {
                for del_id in 75..=88i64 {
                    bt.delete_table(tbl_root, del_id).unwrap();
                }
                for k in 600..=620i64 {
                    let rec = encode_record(&[Value::Null, Value::Blob(vec![0u8; 900])]);
                    bt.insert_table(tbl_root, k, &rec).unwrap();
                }
                for k in -15..=-1i64 {
                    let rec = encode_record(&[Value::Null, Value::Blob(vec![0u8; 900])]);
                    bt.insert_table(tbl_root, k, &rec).unwrap();
                }
            }
            if id > 0 && id % 4 == 0 {
                assert!(bt.delete_table(tbl_root, id).unwrap());
            }

            has_row = bt.cursor_prev(cur).unwrap();
        }
        bt.close_cursor(cur);
        assert_eq!(
            bt.integrity_check(&[1, tbl_root]).unwrap(),
            Vec::<String>::new()
        );
        bt.commit().unwrap();
        drop(bt);

        let conn = Connection::open(&rs_rev_db).unwrap();
        let q = conn
            .query("SELECT id, length(payload) FROM t ORDER BY id ASC", &[])
            .unwrap();
        let final_rows: Vec<(i64, i64)> = q
            .rows
            .iter()
            .map(|r| (r.get_i64(0).unwrap(), r.get_i64(1).unwrap()))
            .collect();
        (visited, final_rows)
    };

    assert_eq!(
        rs_visited_rev, c_visited_rev,
        "reverse mid-scan visited row sequence must match C SQLite 3.54.1 100%"
    );
    assert_eq!(
        rs_final_rev, c_final_rev,
        "reverse mid-scan final table state must match C SQLite 3.54.1 100%"
    );
}

#[test]
fn test_index_cursor_mid_scan_delete_interior_and_leaf_cells_and_splits() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.pager_mut().set_page_size(512).unwrap();
    bt.begin_write().unwrap();

    let idx_root = bt.create_index().unwrap();
    let orders = vec![KeySortOrder::default(), KeySortOrder::default()];

    // Insert 120 keys `(key * 10, rowid)` with payloads large enough to force multiple `IndexInterior` levels!
    for i in 1..=120i64 {
        let k = i * 10;
        let pad = format!("idx-payload-{k:05}-{}", "P".repeat(48));
        let rec = encode_record(&[Value::Integer(k), Value::Integer(i), Value::Text(pad)]);
        bt.insert_index(idx_root, &rec, &orders).unwrap();
    }

    // Confirm root is `IndexInterior` (`0x02`)!
    {
        let usable = bt.pager().usable_size();
        let raw = bt.pager_mut().read_page(idx_root).unwrap();
        let (ptype, n_cell, _) = raw_page_header(idx_root, usable, raw).unwrap();
        assert_eq!(ptype, BtreePageType::IndexInterior);
        assert!(n_cell >= 1);
    }

    // Forward scan: delete every entry whose `i % 2 == 0` (which hits BOTH `IndexInterior` and `IndexLeaf` cells!),
    // AND at `k == 350` insert new entries `< 350` (`-50, -40`) and `> 350` (`355, 1305, 1315`) causing splits!
    let mut visited_fwd = Vec::new();
    let mut interior_hits_deleted = 0usize;
    let cur = bt.open_index_cursor(idx_root, orders.clone(), false);
    let mut ok = bt.cursor_first(cur).unwrap();
    while ok {
        let on_interior = {
            let Some(BtreeCursorState::Valid { stack, .. }) = bt.cursor_state(cur) else {
                panic!("expected Valid cursor");
            };
            let top_pgno = stack.last().unwrap().pgno;
            let usable = bt.pager().usable_size();
            let raw = bt.pager_mut().read_page(top_pgno).unwrap();
            raw_page_header(top_pgno, usable, raw).unwrap().0 == BtreePageType::IndexInterior
        };

        let k = bt.cursor_column(cur, 0).unwrap().unwrap().as_i64().unwrap();
        let rid = bt.cursor_column(cur, 1).unwrap().unwrap().as_i64().unwrap();
        visited_fwd.push((k, rid));

        if k == 350 {
            for &(nk, nrid) in &[
                (-50i64, 9001i64),
                (-40, 9002),
                (355, 9003),
                (1305, 9004),
                (1315, 9005),
            ] {
                let pad = format!("ins-{nk}-{}", "Q".repeat(60));
                let rec =
                    encode_record(&[Value::Integer(nk), Value::Integer(nrid), Value::Text(pad)]);
                bt.insert_index(idx_root, &rec, &orders).unwrap();
            }
        }

        if rid <= 120 && rid % 2 == 0 {
            if on_interior {
                interior_hits_deleted += 1;
            }
            assert!(
                bt.delete_index(idx_root, &[Value::Integer(k), Value::Integer(rid)], &orders,)
                    .unwrap()
            );
            // Verify `restore_cursor_position` lands with `SkipForward` when probed via `cursor_is_eof`
            let _ = bt.cursor_is_eof(cur).unwrap();
            if let Some(BtreeCursorState::Valid { skip_next, .. }) = bt.cursor_state(cur) {
                assert_eq!(*skip_next, SkipNext::SkipForward);
            }
        }

        ok = bt.cursor_next(cur).unwrap();
    }
    bt.close_cursor(cur);
    assert!(
        interior_hits_deleted >= 1,
        "expected at least one deletion while positioned directly on an IndexInterior cell"
    );

    // Every original key `10, 20, ..., 1200` plus newly inserted `355, 1305, 1315` (> 350) must be visited
    // in strictly ascending order with ZERO duplicates and ZERO skips; `-50, -40` (< 350) must NOT be visited!
    let mut expected_fwd: Vec<i64> = (1..=120).map(|i| i * 10).collect();
    expected_fwd.extend([355, 1305, 1315]);
    expected_fwd.sort_unstable();
    let actual_keys: Vec<i64> = visited_fwd.iter().map(|&(k, _)| k).collect();
    assert_eq!(actual_keys, expected_fwd);

    // Now reverse scan (`cursor_last` -> `cursor_prev`) while deleting every 3rd surviving entry!
    let surviving_before_rev: Vec<(i64, i64)> = bt
        .scan_index(idx_root)
        .unwrap()
        .into_iter()
        .map(|r| {
            let v = decode_record(&r).unwrap();
            (v[0].as_i64().unwrap(), v[1].as_i64().unwrap())
        })
        .collect();

    let mut visited_rev = Vec::new();
    let rcur = bt.open_index_cursor(idx_root, orders.clone(), false);
    let mut rok = bt.cursor_last(rcur).unwrap();
    let mut step_idx = 0usize;
    while rok {
        let k = bt
            .cursor_column(rcur, 0)
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap();
        let rid = bt
            .cursor_column(rcur, 1)
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap();
        visited_rev.push((k, rid));
        if step_idx.is_multiple_of(3) {
            assert!(
                bt.delete_index(idx_root, &[Value::Integer(k), Value::Integer(rid)], &orders,)
                    .unwrap()
            );
        }
        step_idx += 1;
        rok = bt.cursor_prev(rcur).unwrap();
    }
    bt.close_cursor(rcur);

    let mut expected_rev = surviving_before_rev;
    expected_rev.reverse();
    assert_eq!(visited_rev, expected_rev);
    assert_eq!(
        bt.integrity_check(&[1, idx_root]).unwrap(),
        Vec::<String>::new()
    );
    bt.commit().unwrap();
}

// ============================================================================
// 3. Multi-Level Index B-Tree In-Order Traversal (`IndexInterior` + `IndexLeaf`,
//    `ASC`/`DESC` + `NULLS` + Collations + Overflow Keys)
// ============================================================================

fn compare_tuple_ref(a: &[Value], b: &[Value], orders: &[KeySortOrder]) -> CmpOrdering {
    for (i, spec) in orders.iter().enumerate() {
        let va = &a[i];
        let vb = &b[i];
        let ord = match (va.is_null(), vb.is_null()) {
            (true, true) => CmpOrdering::Equal,
            (true, false) => {
                if spec.nulls_last {
                    CmpOrdering::Greater
                } else {
                    CmpOrdering::Less
                }
            }
            (false, true) => {
                if spec.nulls_last {
                    CmpOrdering::Less
                } else {
                    CmpOrdering::Greater
                }
            }
            (false, false) => {
                let raw = va.cmp_sqlite(vb, spec.collation);
                if spec.desc { raw.reverse() } else { raw }
            }
        };
        if ord != CmpOrdering::Equal {
            return ord;
        }
    }
    CmpOrdering::Equal
}

#[test]
fn test_multilevel_index_btree_inorder_asc_desc_nulls_and_overflow_keys() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.pager_mut().set_page_size(512).unwrap();
    bt.begin_write().unwrap();

    let idx_root = bt.create_index().unwrap();
    let orders = vec![
        KeySortOrder {
            desc: false,
            nulls_last: true, // ASC NULLS LAST
            collation: Collation::NoCase,
            custom_collation: None,
        },
        KeySortOrder {
            desc: true,
            nulls_last: false, // DESC NULLS FIRST
            collation: Collation::Binary,
            custom_collation: None,
        },
        KeySortOrder::default(), // trailing rowid ASC
    ];

    // Build 135 diverse composite records including:
    // - duplicate `col0` prefixes (many rows per bucket so duplicates span `left_child`, `IndexInterior`, and `right_child`!)
    // - `NULL`s in both `col0` and `col1`
    // - mixed-case strings under `NOCASE`
    // - 1,600-byte overflow text keys!
    let buckets = [
        None,
        Some("alpha".to_owned()),
        Some("ALPHA".to_owned()),
        Some("beta".to_owned()),
        Some(format!("ovfl-mid-{}", "M".repeat(1_550))),
        Some("omega".to_owned()),
    ];

    let mut expected_rows: Vec<Vec<Value>> = Vec::new();
    let mut next_rid = 1i64;
    for (bi, bucket) in buckets.iter().enumerate() {
        for sub in 0..24i64 {
            let c0 = match bucket {
                None => Value::Null,
                Some(s) => Value::Text(s.clone()),
            };
            let c1 = if sub % 6 == 0 {
                Value::Null
            } else if sub % 5 == 0 {
                Value::Text(format!("big-c1-{sub:02}-{}", "Z".repeat(1_400)))
            } else {
                Value::Integer((sub * 7) - 30 + bi as i64)
            };
            let row = vec![c0, c1, Value::Integer(next_rid)];
            let rec = encode_record(&row);
            bt.insert_index(idx_root, &rec, &orders).unwrap();
            expected_rows.push(row);
            next_rid += 1;
        }
    }

    expected_rows.sort_by(|a, b| compare_tuple_ref(a, b, &orders));

    // Verify root is `IndexInterior` (`0x02`) and count interior cells visited!
    {
        let usable = bt.pager().usable_size();
        let raw = bt.pager_mut().read_page(idx_root).unwrap();
        let (ptype, _, _) = raw_page_header(idx_root, usable, raw).unwrap();
        assert_eq!(ptype, BtreePageType::IndexInterior);
    }

    // 1. Full forward traversal (`cursor_first` -> `cursor_next`)
    let cur = bt.open_index_cursor(idx_root, orders.clone(), false);
    let mut forward_rows = Vec::new();
    let mut interior_visited = 0usize;
    let mut ok = bt.cursor_first(cur).unwrap();
    while ok {
        if let Some(BtreeCursorState::Valid { stack, .. }) = bt.cursor_state(cur) {
            let pgno = stack.last().unwrap().pgno;
            let usable = bt.pager().usable_size();
            let raw = bt.pager_mut().read_page(pgno).unwrap();
            if raw_page_header(pgno, usable, raw).unwrap().0 == BtreePageType::IndexInterior {
                interior_visited += 1;
            }
        }
        let c0 = bt.cursor_column(cur, 0).unwrap().unwrap();
        let c1 = bt.cursor_column(cur, 1).unwrap().unwrap();
        let c2 = bt.cursor_column(cur, 2).unwrap().unwrap();
        assert_eq!(Value::Integer(bt.cursor_rowid(cur).unwrap()), c2);
        forward_rows.push(vec![c0, c1, c2]);
        ok = bt.cursor_next(cur).unwrap();
    }
    assert!(
        interior_visited >= 3,
        "multi-level index scan must visit IndexInterior cells in-order (visited {interior_visited})"
    );
    assert_eq!(forward_rows.len(), expected_rows.len());
    assert_eq!(forward_rows, expected_rows);

    // 2. Full reverse traversal (`cursor_last` -> `cursor_prev`)
    let mut reverse_rows = Vec::new();
    let mut rok = bt.cursor_last(cur).unwrap();
    while rok {
        let payload = bt.cursor_payload(cur).unwrap();
        reverse_rows.push(decode_record(&payload).unwrap());
        rok = bt.cursor_prev(cur).unwrap();
    }
    reverse_rows.reverse();
    assert_eq!(reverse_rows, expected_rows);

    // 3. Prefix seeks (`cursor_seek_index_equal_first` -> `cursor_next_index_matching`),
    //    specifically testing `"alpha"` (48 NOCASE matches spanning leaves + interior nodes!),
    //    the 1,550-byte overflow key, `NULL`, and a missing key!
    let probe_prefixes: Vec<Vec<Value>> = vec![
        vec![Value::Text("AlPhA".to_owned())],
        vec![Value::Text(format!("ovfl-mid-{}", "M".repeat(1_550)))],
        vec![Value::Null],
        vec![Value::Text("alpha".to_owned()), Value::Null],
        vec![Value::Text("nonexistent-key".to_owned())],
    ];

    for prefix in &probe_prefixes {
        let mut got = Vec::new();
        if bt.cursor_seek_index_equal_first(cur, prefix).unwrap() {
            loop {
                let payload = bt.cursor_payload(cur).unwrap();
                got.push(decode_record(&payload).unwrap());
                if !bt.cursor_next_index_matching(cur, prefix).unwrap() {
                    break;
                }
            }
        }
        let want: Vec<Vec<Value>> = expected_rows
            .iter()
            .filter(|r| {
                compare_tuple_ref(&r[..prefix.len()], prefix, &orders[..prefix.len()])
                    == CmpOrdering::Equal
            })
            .cloned()
            .collect();
        assert_eq!(
            got,
            want,
            "prefix seek mismatch for prefix len {}",
            prefix.len()
        );
    }

    bt.close_cursor(cur);
    assert_eq!(
        bt.integrity_check(&[1, idx_root]).unwrap(),
        Vec::<String>::new()
    );
    bt.commit().unwrap();
}

// ============================================================================
// 4. Rollback `trip_cursors` (`write_only = false` & `write_only = true`)
//    + `SQLITE_ABORT_ROLLBACK` (`516`) + C SQLite 3.54.1 Verification
// ============================================================================

#[test]
fn test_rollback_trip_cursors_full_and_write_only_abort_rollback() {
    assert_eq!(extended::SQLITE_ABORT_ROLLBACK, 516);
    assert_eq!(
        extended::SQLITE_ABORT_ROLLBACK,
        (ErrorCode::Abort as i32) | (2 << 8)
    );

    // Confirm C SQLite 3.54.1 emits primary=4 (`SQLITE_ABORT`), extended=516 (`SQLITE_ABORT_ROLLBACK`)!
    {
        let helper = c_cursor_helper();
        let sandbox = TempSandbox::new("trip_c");
        let db_path = sandbox.path().join("trip.db");
        seed_differential_mutation_db(&db_path);
        let out = Command::new(helper)
            .arg("rollback_trip_probe")
            .arg(&db_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(
            text.trim(),
            "TRIP_DDL:first=1:primary=4:extended=516\nTRIP_DATA:rc=100:second=2"
        );
    }

    // --- Part A: `write_only = false` full rollback trips ALL cursors with `SQLITE_ABORT_ROLLBACK` (516) ---
    {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let tbl_root = bt.create_table().unwrap();
        for i in 1..=30i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Text(format!("v-{i}"))]);
            bt.insert_table(tbl_root, i, &rec).unwrap();
        }
        bt.commit().unwrap();

        bt.begin_write().unwrap();
        let ro_cur = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_first(ro_cur).unwrap());
        assert_eq!(bt.cursor_rowid(ro_cur).unwrap(), 1);

        // Also open a `VdbeVm` paused mid-scan at `VmStep::Row`!
        let mut prog = VdbeProgram::new();
        prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
        prog.emit(Instruction::new(Opcode::OpenRead, 0, tbl_root as i32, 2));
        let rew = prog.emit(Instruction::new(Opcode::Rewind, 0, 0, 0));
        prog.emit(Instruction::new(Opcode::Rowid, 0, 1, 0));
        prog.emit(Instruction::new(Opcode::ResultRow, 1, 1, 0));
        prog.emit(Instruction::new(Opcode::Next, 0, (rew + 1) as i32, 0));
        let end = prog.current_addr();
        prog.patch_p2(rew, end);
        prog.emit(Instruction::new(Opcode::Close, 0, 0, 0));
        prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

        let mut vm = VdbeVm::new(prog, Vec::new(), VmRuntimeContext::default());
        assert_eq!(
            vm.step(&mut bt, &mut DefaultVmServices::default()).unwrap(),
            VmStep::Row(vec![Value::Integer(1)])
        );

        let ins_rec = encode_record(&[Value::Integer(999), Value::Text("uncommitted".into())]);
        bt.insert_table(tbl_root, 999, &ins_rec).unwrap();

        // Full rollback (`write_only = false`)!
        bt.rollback().unwrap();

        fn assert_abort_rollback<T: std::fmt::Debug>(res: Result<T>, op_name: &str) {
            let err = res.expect_err(op_name);
            assert_eq!(err.code, ErrorCode::Abort, "{op_name}: primary code");
            assert_eq!(
                err.extended_code,
                extended::SQLITE_ABORT_ROLLBACK,
                "{op_name}: extended code must be 516"
            );
        }

        assert_abort_rollback(bt.cursor_next(ro_cur), "cursor_next");
        assert_abort_rollback(bt.cursor_prev(ro_cur), "cursor_prev");
        assert_abort_rollback(bt.cursor_rowid(ro_cur), "cursor_rowid");
        assert_abort_rollback(bt.cursor_column(ro_cur, 1), "cursor_column");
        assert_abort_rollback(bt.cursor_payload(ro_cur), "cursor_payload");
        assert_abort_rollback(bt.cursor_is_eof(ro_cur), "cursor_is_eof");
        assert_abort_rollback(bt.cursor_first(ro_cur), "cursor_first");
        assert_abort_rollback(bt.cursor_seek_rowid(ro_cur, 1), "cursor_seek_rowid");

        // Stepping the paused VM must ALSO fail with `SQLITE_ABORT_ROLLBACK` (516) and clean up its cursor!
        let vm_err = vm
            .step(&mut bt, &mut DefaultVmServices::default())
            .unwrap_err();
        assert_eq!(vm_err.code, ErrorCode::Abort);
        assert_eq!(vm_err.extended_code, extended::SQLITE_ABORT_ROLLBACK);

        bt.close_cursor(ro_cur);
        assert_eq!(bt.open_cursor_count(), 0);
    }

    // --- Part B: `write_only = true` partial/statement rollback faults write cursors,
    //             while read-only cursors preserve position via `RequireSeek` and keep reading! ---
    {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let tbl_root = bt.create_table().unwrap();
        for i in 1..=60i64 {
            let rec = encode_record(&[
                Value::Integer(i),
                Value::Text(format!("row-{i:03}-{}", "X".repeat(60))),
            ]);
            bt.insert_table(tbl_root, i, &rec).unwrap();
        }
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        let ro_cur = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_seek_rowid(ro_cur, 25).unwrap());
        assert_eq!(bt.cursor_rowid(ro_cur).unwrap(), 25);

        // Upgrade to write transaction + open sub-savepoint + open a writable cursor
        bt.begin_write().unwrap();
        bt.pager_mut().ensure_subsavepoints(1).unwrap();
        assert_eq!(bt.pager().subsavepoint_depth(), 1);

        let wr_cur = bt.open_table_cursor(tbl_root, true);
        assert!(bt.cursor_seek_rowid(wr_cur, 40).unwrap());

        // Mutate & split pages heavily inside the statement savepoint, then roll it back with `write_only = true`!
        for k in 1_000..=1_040i64 {
            let rec = encode_record(&[Value::Integer(k), Value::Text("S".repeat(120))]);
            bt.insert_table(tbl_root, k, &rec).unwrap();
        }
        bt.trip_cursors(extended::SQLITE_ABORT_ROLLBACK, true)
            .unwrap();
        bt.pager_mut().rollback_to_subsavepoint(0).unwrap();
        bt.pager_mut().release_subsavepoint(0).unwrap();

        // Write cursor MUST be faulted with `SQLITE_ABORT_ROLLBACK` (516)!
        let wr_err = bt.cursor_next(wr_cur).unwrap_err();
        assert_eq!(wr_err.code, ErrorCode::Abort);
        assert_eq!(wr_err.extended_code, extended::SQLITE_ABORT_ROLLBACK);
        bt.close_cursor(wr_cur);

        // Read-only cursor MUST be in `RequireSeek` and seamlessly resume from 25 -> 26..=60!
        assert!(matches!(
            bt.cursor_state(ro_cur),
            Some(BtreeCursorState::RequireSeek { .. })
        ));
        assert_eq!(bt.cursor_rowid(ro_cur).unwrap(), 25);
        let mut tail_ids = Vec::new();
        while bt.cursor_next(ro_cur).unwrap() {
            tail_ids.push(bt.cursor_rowid(ro_cur).unwrap());
        }
        let expected_tail: Vec<i64> = (26..=60).collect();
        assert_eq!(tail_ids, expected_tail);

        bt.close_cursor(ro_cur);
        bt.rollback().unwrap();
    }
}

// ============================================================================
// 5. Atomic `Interrupt` Hook (`Arc<AtomicBool>`) on `BtreeCursor` & `VdbeVm::step`
// ============================================================================

#[test]
fn test_atomic_interrupt_hook_aborts_btree_cursor_overflow_and_vdbe_step() {
    let vfs = Arc::new(SpyVfs::new());
    let db_path = Path::new("/interrupt_test.db");

    let conn = Connection::open_with_vfs(
        Arc::clone(&vfs) as Arc<dyn Vfs>,
        Some(db_path),
        OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB,
    )
    .unwrap();
    conn.execute(
        "PRAGMA page_size = 512;
         CREATE TABLE heavy (id INTEGER PRIMARY KEY, big BLOB NOT NULL);
         BEGIN;",
    )
    .unwrap();
    for id in 1..=120i64 {
        conn.execute_params(
            "INSERT INTO heavy VALUES (?1, ?2)",
            &[Value::Integer(id), Value::Blob(vec![0x5Au8; 3_000])],
        )
        .unwrap();
    }
    conn.execute("COMMIT;").unwrap();
    let tbl_root = conn
        .schema_snapshot()
        .unwrap()
        .find_table("heavy")
        .unwrap()
        .root_pgno;
    drop(conn);

    // 1. Interrupt a cold `BtreeCursor` right in the middle of reassembling a 6-page overflow BLOB!
    {
        let flag = Arc::new(AtomicBool::new(false));
        let mut bt = Btree::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
        )
        .unwrap();
        bt.set_interrupt_flag(Some(Arc::clone(&flag)));
        bt.begin_read().unwrap();

        let cur = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_first(cur).unwrap());

        // Arm SpyVfs to flip `flag = true` after 2 overflow page reads inside `cursor_payload`!
        vfs.tracker.reset();
        *vfs.tracker.interrupt_target.lock().unwrap() = Some(Arc::clone(&flag));
        vfs.tracker.interrupt_after_reads.store(2, Ordering::SeqCst);

        let err = bt.cursor_payload(cur).unwrap_err();
        assert_eq!(err.code, ErrorCode::Interrupt);
        assert!(
            vfs.tracker.count() <= 3,
            "overflow walk must abort immediately upon interrupt, read {} pages",
            vfs.tracker.count()
        );

        // `cursor_next` also immediately returns `ErrorCode::Interrupt` while flag remains set
        assert_eq!(bt.cursor_next(cur).unwrap_err().code, ErrorCode::Interrupt);

        // Clearing the flag restores normal cursor operation
        vfs.tracker.interrupt_after_reads.store(0, Ordering::SeqCst);
        *vfs.tracker.interrupt_target.lock().unwrap() = None;
        flag.store(false, Ordering::Relaxed);
        assert!(bt.cursor_next(cur).unwrap());
        assert_eq!(bt.cursor_rowid(cur).unwrap(), 2);

        bt.close_cursor(cur);
        bt.commit().unwrap();
    }

    // 2. Interrupt `VdbeVm::step` / `Statement::step` mid-table-scan (e.g. `WHERE id = 999999` scanning rows)
    {
        let flag = Arc::new(AtomicBool::new(false));
        let conn = Connection::open_with_vfs(
            Arc::clone(&vfs) as Arc<dyn Vfs>,
            Some(db_path),
            OpenFlags::READONLY | OpenFlags::MAIN_DB,
        )
        .unwrap();
        let _ = conn.schema_snapshot().unwrap();
        conn.set_interrupt_flag(Some(Arc::clone(&flag))).unwrap();

        vfs.tracker.reset();
        *vfs.tracker.interrupt_target.lock().unwrap() = Some(Arc::clone(&flag));
        vfs.tracker.interrupt_after_reads.store(4, Ordering::SeqCst);

        let mut stmt = conn
            .prepare("SELECT id, big FROM heavy WHERE length(big) = 1")
            .unwrap();
        let err = stmt.step().unwrap_err();
        assert_eq!(err.code, ErrorCode::Interrupt);
        drop(stmt);

        // Zero leaked cursors after interrupted `VdbeVm::step`!
        assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);

        // Reset flag and confirm subsequent query succeeds
        vfs.tracker.interrupt_after_reads.store(0, Ordering::SeqCst);
        *vfs.tracker.interrupt_target.lock().unwrap() = None;
        flag.store(false, Ordering::Relaxed);
        let row = conn
            .query_row("SELECT id, length(big) FROM heavy WHERE id = 1", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64(0), Some(1));
        assert_eq!(row.get_i64(1), Some(3_000));
        assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);
    }
}

// ============================================================================
// 6. Zero Cursor Leak (`Done`, `reset_with_btree`, `Halt` error, `UNIQUE` error,
//    slot reopen, and `Statement` reset/drop)
// ============================================================================

#[test]
fn test_zero_cursor_leak_across_done_reset_halt_error_and_statement_drop() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.begin_write().unwrap();
    let tbl_root = bt.create_table().unwrap();
    let idx_root = bt.create_index().unwrap();
    let orders = vec![KeySortOrder::default(), KeySortOrder::default()];

    for i in 1..=10i64 {
        let rec = encode_record(&[Value::Integer(i), Value::Text(format!("u-{i}"))]);
        bt.insert_table(tbl_root, i, &rec).unwrap();
        let irec = encode_record(&[Value::Text(format!("u-{i}")), Value::Integer(i)]);
        bt.insert_index(idx_root, &irec, &orders).unwrap();
    }
    bt.commit().unwrap();

    // Build a program that opens BOTH a Table cursor (slot 0, re-opened twice to test slot replacement!)
    // and an Index cursor (slot 1), then yields rows.
    let mut scan_prog = VdbeProgram::new();
    scan_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    scan_prog.emit(Instruction::new(Opcode::OpenRead, 0, tbl_root as i32, 2));
    // Reopen on slot 0: must close the previous CursorId!
    scan_prog.emit(Instruction::new(Opcode::OpenRead, 0, tbl_root as i32, 2));
    scan_prog.emit(
        Instruction::new(Opcode::OpenReadIndex, 1, idx_root as i32, 0)
            .with_p4(P4::key_info(orders.clone())),
    );
    let rew = scan_prog.emit(Instruction::new(Opcode::Rewind, 0, 0, 0));
    scan_prog.emit(Instruction::new(Opcode::Rowid, 0, 1, 0));
    scan_prog.emit(Instruction::new(Opcode::ResultRow, 1, 1, 0));
    scan_prog.emit(Instruction::new(Opcode::Next, 0, (rew + 1) as i32, 0));
    let end = scan_prog.current_addr();
    scan_prog.patch_p2(rew, end);
    scan_prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0)); // intentionally omit explicit OP_Close to verify Halt closes cursors!

    bt.begin_read().unwrap();
    let mut vm = VdbeVm::new(scan_prog, Vec::new(), VmRuntimeContext::default());

    // Case 1: Paused at `Row` -> exactly 2 cursors open (slot 0 reopen did NOT leak); early `reset_with_btree` -> 0 cursors!
    assert_eq!(
        vm.step(&mut bt, &mut DefaultVmServices::default()).unwrap(),
        VmStep::Row(vec![Value::Integer(1)])
    );
    assert_eq!(bt.open_cursor_count(), 2);
    vm.reset_with_btree(&mut bt);
    assert_eq!(bt.open_cursor_count(), 0);

    // Case 2: Run all the way to `VmStep::Done` -> `Halt` closes all cursors (`open_cursor_count() == 0`)!
    let mut count = 0usize;
    while let VmStep::Row(_) = vm.step(&mut bt, &mut DefaultVmServices::default()).unwrap() {
        count += 1;
    }
    assert_eq!(count, 10);
    assert_eq!(bt.open_cursor_count(), 0);
    bt.commit().unwrap();

    // Case 3: `Opcode::Halt` with non-zero error code (`ins.p1 != 0`) while cursors are open!
    let mut err_prog = VdbeProgram::new();
    err_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    err_prog.emit(Instruction::new(Opcode::OpenRead, 0, tbl_root as i32, 2));
    err_prog.emit(
        Instruction::new(Opcode::OpenReadIndex, 1, idx_root as i32, 0)
            .with_p4(P4::key_info(orders.clone())),
    );
    err_prog.emit(
        Instruction::new(Opcode::Halt, extended::SQLITE_CONSTRAINT_CHECK, 0, 0)
            .with_p4(P4::Str("CHECK constraint failed".to_owned())),
    );

    bt.begin_read().unwrap();
    let mut err_vm = VdbeVm::new(err_prog, Vec::new(), VmRuntimeContext::default());
    let halt_err = err_vm
        .step(&mut bt, &mut DefaultVmServices::default())
        .unwrap_err();
    assert_eq!(halt_err.code, ErrorCode::Constraint);
    assert_eq!(halt_err.extended_code, extended::SQLITE_CONSTRAINT_CHECK);
    assert_eq!(bt.open_cursor_count(), 0);
    bt.commit().unwrap();

    // Case 4: `Opcode::IdxInsert` incremental `UNIQUE` check failure closes both temporary probe cursor AND VM cursors!
    let mut uniq_prog = VdbeProgram::new();
    uniq_prog.read_only = false;
    uniq_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    uniq_prog.emit(Instruction::new(Opcode::OpenWrite, 0, tbl_root as i32, 2));
    uniq_prog.emit(
        Instruction::new(Opcode::OpenWriteIndex, 1, idx_root as i32, 0)
            .with_p4(P4::key_info(orders.clone())),
    );
    uniq_prog.emit(Instruction::new(Opcode::String8, 0, 1, 0).with_p4(P4::Str("u-5".to_owned())));
    uniq_prog.emit(Instruction::new(Opcode::Integer, 999, 2, 0)); // conflicting rowid 999 != existing rowid 5!
    uniq_prog.emit(Instruction::new(Opcode::MakeRecord, 1, 2, 3));
    uniq_prog.emit(
        Instruction::new(Opcode::IdxInsert, 1, 3, 0)
            .with_p5(1)
            .with_p4(P4::Str("UNIQUE constraint failed: idx".to_owned())),
    );
    uniq_prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    bt.begin_write().unwrap();
    let mut uniq_vm = VdbeVm::new(uniq_prog, Vec::new(), VmRuntimeContext::default());
    let uerr = uniq_vm
        .step(&mut bt, &mut DefaultVmServices::default())
        .unwrap_err();
    assert_eq!(uerr.code, ErrorCode::Constraint);
    assert_eq!(uerr.extended_code, extended::SQLITE_CONSTRAINT_UNIQUE);
    assert_eq!(bt.open_cursor_count(), 0);
    bt.rollback().unwrap();

    // Case 5: High-level `Connection` / `Statement` early `reset()` and `drop()` mid-scan!
    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT UNIQUE);
         INSERT INTO items VALUES (1, 'a'), (2, 'b'), (3, 'c');",
    )
    .unwrap();
    {
        let mut stmt = conn
            .prepare("SELECT id, name FROM items WHERE name = 'b'")
            .unwrap();
        assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
        stmt.reset().unwrap();
        // Re-step after reset: if `stmt.reset()` leaked old cursors, we would see >0 after dropping!
        assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
        stmt.reset().unwrap();
    }
    assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);
    {
        let mut stmt = conn
            .prepare("SELECT id, name FROM items WHERE name = 'b'")
            .unwrap();
        assert!(matches!(stmt.step().unwrap(), StepResult::Row(_)));
        // Drop `stmt` while paused at `StepResult::Row`!
    }
    assert_eq!(conn.open_cursor_count_for_testing().unwrap(), 0);
}

#[test]
fn test_vdbe_delete_under_scan_cursor_and_activity_query_helpers() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.pager_mut().set_page_size(512).unwrap();
    bt.begin_write().unwrap();
    let r1 = bt.create_table().unwrap();
    let r2 = bt.create_table().unwrap();

    for i in 1..=50i64 {
        let rec = encode_record(&[
            Value::Integer(i),
            Value::Text(format!("row-{i:02}-{}", "D".repeat(80))),
        ]);
        bt.insert_table(r1, i, &rec).unwrap();
    }

    // Verify `has_open_cursors`, `has_other_cursors_on_root`, and `has_any_other_open_cursor`
    assert!(!bt.has_open_cursors());
    let c1 = bt.open_cursor(r1, CursorKind::Table, false);
    assert!(bt.has_open_cursors());
    assert!(!bt.has_other_cursors_on_root(r1, Some(c1)));
    assert!(bt.has_other_cursors_on_root(r1, None));
    assert!(!bt.has_any_other_open_cursor(Some(c1)));

    let c2 = bt.open_table_cursor(r2, false);
    assert!(bt.has_any_other_open_cursor(Some(c1)));
    assert!(!bt.has_other_cursors_on_root(r1, Some(c1)));
    assert!(bt.has_other_cursors_on_root(r2, Some(c1)));
    bt.close_cursor(c1);
    bt.close_cursor(c2);
    assert!(!bt.has_open_cursors());

    // Execute a `VdbeVm` program that scans `r1` and deletes EVERY row via `Opcode::Delete` (`p2 = 0`) + `Opcode::Next`!
    // Without `SkipForward`, deleting row 1 would cause `OP_Next` to skip row 2, leaving half the rows behind!
    let mut del_prog = VdbeProgram::new();
    del_prog.read_only = false;
    del_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    del_prog.emit(Instruction::new(Opcode::OpenWrite, 0, r1 as i32, 2));
    let rew = del_prog.emit(Instruction::new(Opcode::Rewind, 0, 0, 0));
    del_prog.emit(Instruction::new(Opcode::Delete, 0, 0, 0).with_p5(0x02));
    del_prog.emit(Instruction::new(Opcode::Next, 0, (rew + 1) as i32, 0));
    let after = del_prog.current_addr();
    del_prog.patch_p2(rew, after);
    del_prog.emit(Instruction::new(Opcode::Close, 0, 0, 0));
    del_prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

    let mut vm = VdbeVm::new(del_prog, Vec::new(), VmRuntimeContext::default());
    assert_eq!(
        vm.step(&mut bt, &mut DefaultVmServices::default()).unwrap(),
        VmStep::Done
    );
    assert_eq!(vm.runtime_ctx.last_changes, 50);
    assert_eq!(bt.scan_table(r1).unwrap().len(), 0);
    assert_eq!(
        bt.integrity_check(&[1, r1, r2]).unwrap(),
        Vec::<String>::new()
    );
    bt.commit().unwrap();

    // Verify `Btree::close` clears any open cursor registrations
    bt.begin_read().unwrap();
    let _leaked = bt.open_table_cursor(r1, false);
    assert_eq!(bt.open_cursor_count(), 1);
    bt.commit().unwrap();
    bt.close().unwrap();
    assert_eq!(bt.open_cursor_count(), 0);
}

// ============================================================================
// 7. Regression: `trip_cursors(..., true)` Error Propagation & Full-Trip Escalation
// ============================================================================

#[test]
fn test_trip_cursors_write_only_save_error_propagates_and_faults_all_cursors() {
    // Subcase A: `save_cursor_position` fails via atomic `Interrupt` on an overflowed read-only index cursor.
    {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let tbl_root = bt.create_table().unwrap();
        let idx_root = bt.create_index().unwrap();

        for id in 1..=10i64 {
            bt.insert_table(
                tbl_root,
                id,
                &encode_record(&[Value::Integer(id), Value::Text(format!("row-{id}"))]),
            )
            .unwrap();
        }
        // Multi-page overflow index record so `cursor_first` stays local, while `save_cursor_position`
        // must walk the overflow chain via `read_full_payload` (which checks `interrupt_flag`)!
        let ovfl_rec = encode_record(&[Value::Text("Z".repeat(1600)), Value::Integer(1)]);
        bt.insert_index(idx_root, &ovfl_rec, &[]).unwrap();
        bt.commit().unwrap();

        bt.begin_write().unwrap();
        let ro_already_saved = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_first(ro_already_saved).unwrap());
        assert!(bt.save_cursor_position(ro_already_saved).unwrap());
        assert!(matches!(
            bt.cursor_state(ro_already_saved),
            Some(BtreeCursorState::RequireSeek { .. })
        ));

        let ro_unpositioned = bt.open_table_cursor(tbl_root, false);
        assert_eq!(
            bt.cursor_state(ro_unpositioned),
            Some(&BtreeCursorState::Invalid)
        );

        let ro_overflow = bt.open_index_cursor(idx_root, Vec::new(), false);
        assert!(bt.cursor_first(ro_overflow).unwrap());
        assert!(matches!(
            bt.cursor_state(ro_overflow),
            Some(BtreeCursorState::Valid { .. })
        ));

        let wr_cur = bt.open_table_cursor(tbl_root, true);
        assert!(bt.cursor_last(wr_cur).unwrap());

        let flag = Arc::new(AtomicBool::new(true));
        bt.set_interrupt_flag(Some(Arc::clone(&flag)));

        let err = bt
            .trip_cursors(extended::SQLITE_ABORT_ROLLBACK, true)
            .expect_err("trip_cursors(write_only=true) must propagate save_cursor_position error");
        assert_eq!(err.code, ErrorCode::Interrupt);
        assert_eq!(err.extended_code, ErrorCode::Interrupt as i32);

        // Disarm the live interrupt flag so subsequent calls verify sticky `Fault(err.extended_code)`!
        flag.store(false, Ordering::Relaxed);

        for cid in [ro_already_saved, ro_unpositioned, ro_overflow, wr_cur] {
            assert_eq!(
                bt.cursor_state(cid),
                Some(&BtreeCursorState::Fault(ErrorCode::Interrupt as i32)),
                "every open cursor must escalate to Fault(err.extended_code)"
            );
            let step_err = bt.cursor_next(cid).unwrap_err();
            assert_eq!(step_err.code, ErrorCode::Interrupt);
            assert_eq!(step_err.extended_code, ErrorCode::Interrupt as i32);
        }
    }

    // Subcase B: `rollback_with_trip(..., write_only=true, retain_read=true)` when `save_cursor_position`
    // hits a truncated overflow link (`ErrorCode::Corrupt`): pager STILL rolls back, `Err(Corrupt)` is returned,
    // and ALL open cursors become `Fault(ErrorCode::Corrupt as i32)`.
    {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let tbl_root = bt.create_table().unwrap();
        let idx_root = bt.create_index().unwrap();
        bt.insert_table(tbl_root, 1, &encode_record(&[Value::Integer(1)]))
            .unwrap();
        bt.insert_index(
            idx_root,
            &encode_record(&[Value::Blob(vec![0xAB; 1800]), Value::Integer(1)]),
            &[],
        )
        .unwrap();
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        bt.begin_write().unwrap();

        // Corrupt the first overflow page's `next_pgno` pointer inside the uncommitted write tx
        let usable = bt.pager().usable_size();
        let first_ovfl = {
            let raw_idx = bt.pager_mut().read_page(idx_root).unwrap();
            match ParsedPage::parse(idx_root, usable, raw_idx).unwrap() {
                ParsedPage::IndexLeaf { cells } => cells[0].overflow_page.expect("has overflow"),
                other => panic!("expected IndexLeaf, got {other:?}"),
            }
        };
        {
            let ovfl_buf = bt.pager_mut().write_page(first_ovfl).unwrap();
            ovfl_buf[0..4].fill(0); // Premature end of 4-hop overflow chain -> Corrupt!
        }

        let ro_saved = bt.open_table_cursor(tbl_root, false);
        assert!(bt.cursor_first(ro_saved).unwrap());
        assert!(bt.save_cursor_position(ro_saved).unwrap());

        let ro_corrupt = bt.open_index_cursor(idx_root, Vec::new(), false);
        assert!(bt.cursor_first(ro_corrupt).unwrap());

        let wr_cur = bt.open_table_cursor(tbl_root, true);
        assert!(bt.cursor_first(wr_cur).unwrap());

        let rb_err = bt
            .rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, true, true)
            .expect_err("rollback_with_trip must propagate save_cursor_position Corrupt error");
        assert_eq!(rb_err.code, ErrorCode::Corrupt);
        assert_eq!(rb_err.extended_code, ErrorCode::Corrupt as i32);
        // Pager must still have rolled back cleanly to Read despite the cursor-save failure!
        assert_eq!(bt.transaction_state(), TransactionState::Read);

        for cid in [ro_saved, ro_corrupt, wr_cur] {
            assert_eq!(
                bt.cursor_state(cid),
                Some(&BtreeCursorState::Fault(ErrorCode::Corrupt as i32))
            );
        }
    }
}

// ============================================================================
// 8. Regression: Empty `orders` (`Vec::new()` / `&[]`) Index Insert & Reseek
// ============================================================================

#[test]
fn test_index_empty_orders_insert_and_mid_scan_mutation_reseek() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.pager_mut().set_page_size(512).unwrap();
    bt.begin_write().unwrap();
    let idx_root = bt.create_index().unwrap();

    // Insert 70 multi-column entries with `orders = &[]` (default ASC BINARY across all columns),
    // forcing leaf and interior splits!
    for i in 1..=70i64 {
        let rec = encode_record(&[
            Value::Integer((i + 1) / 2),
            Value::Integer(i),
            Value::Text(format!("pad-{i:03}-{}", "E".repeat(48))),
        ]);
        bt.insert_index(idx_root, &rec, &[]).unwrap();
    }
    assert_eq!(
        bt.scan_index(idx_root).unwrap().len(),
        70,
        "insert_index with empty orders must preserve all distinct multi-column entries"
    );

    // Confirm multi-level `IndexInterior` root was formed
    {
        let usable = bt.pager().usable_size();
        let raw_root = bt.pager_mut().read_page(idx_root).unwrap();
        let (ptype, _, _) = raw_page_header(idx_root, usable, raw_root).unwrap();
        assert_eq!(ptype, BtreePageType::IndexInterior);
    }

    // Open an index cursor with `orders = Vec::new()` (matching `VdbeVm` `OpenReadIndex` with `P4::None`)
    // and mutate the index on EVERY step (deleting even `i` and inserting high keys `1000 + i`)!
    let cur = bt.open_index_cursor(idx_root, Vec::new(), false);
    let mut visited = Vec::new();
    let mut ok = bt.cursor_first(cur).unwrap();
    while ok {
        let col0 = bt.cursor_column(cur, 0).unwrap().unwrap().as_i64().unwrap();
        let rowid = bt.cursor_column(cur, 1).unwrap().unwrap().as_i64().unwrap();
        let pad = match bt.cursor_column(cur, 2).unwrap().unwrap() {
            Value::Text(s) => s,
            other => panic!("expected Text pad, got {other:?}"),
        };
        assert!(
            visited.len() <= 200,
            "cursor looped back to start on empty orders reseek!"
        );
        visited.push((col0, rowid));

        if rowid <= 70 && rowid % 2 == 0 {
            // Delete the current entry with `orders = &[]` -> triggers `save_cursor_position` + `SkipForward`!
            assert!(
                bt.delete_index(
                    idx_root,
                    &[
                        Value::Integer(col0),
                        Value::Integer(rowid),
                        Value::Text(pad)
                    ],
                    &[],
                )
                .unwrap()
            );
        } else if rowid <= 70 && rowid % 5 == 0 {
            // Insert a new entry ahead of the cursor -> triggers `save_cursor_position` + `Exact` reseek!
            let high_id = 1000 + rowid;
            let ins_rec = encode_record(&[
                Value::Integer(500 + rowid),
                Value::Integer(high_id),
                Value::Text(format!("pad-{high_id:04}-{}", "N".repeat(48))),
            ]);
            bt.insert_index(idx_root, &ins_rec, &[]).unwrap();
        }

        ok = bt.cursor_next(cur).unwrap();
    }
    bt.close_cursor(cur);

    // Every initial row 1..=70 plus the 7 inserted future rows (rowid in {5,15,25,35,45,55,65}) must be visited strictly once!
    let mut expected: Vec<(i64, i64)> = (1..=70i64).map(|i| ((i + 1) / 2, i)).collect();
    for r in (5..=65i64).step_by(10) {
        expected.push((500 + r, 1000 + r));
    }
    assert_eq!(visited, expected);

    // Also verify `VdbeVm` `OpenWriteIndex` + `OpenReadIndex` with `P4::None` (`orders = Vec::new()`)
    let v_idx = bt.create_index().unwrap();
    let mut wprog = VdbeProgram::new();
    wprog.read_only = false;
    wprog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
    wprog.emit(Instruction::new(Opcode::OpenWriteIndex, 0, v_idx as i32, 0));
    for id in 1..=5i64 {
        wprog.emit(Instruction::new(Opcode::Integer, 42, 1, 0));
        wprog.emit(Instruction::new(Opcode::Integer, id as i32, 2, 0));
        wprog.emit(Instruction::new(Opcode::MakeRecord, 1, 2, 3));
        wprog.emit(Instruction::new(Opcode::IdxInsert, 0, 3, 0));
    }
    wprog.emit(Instruction::new(Opcode::Close, 0, 0, 0));
    wprog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
    let mut wvm = VdbeVm::new(wprog, Vec::new(), VmRuntimeContext::default());
    assert_eq!(
        wvm.step(&mut bt, &mut DefaultVmServices::default())
            .unwrap(),
        VmStep::Done
    );
    assert_eq!(bt.scan_index(v_idx).unwrap().len(), 5);
    assert_eq!(
        bt.integrity_check(&[1, idx_root, v_idx]).unwrap(),
        Vec::<String>::new()
    );
    bt.commit().unwrap();
}

// ============================================================================
// 9. Regression: Non-Unique Multi-Level Index Prefix `orders` Spanning Interior
// ============================================================================

#[test]
fn test_nonunique_index_prefix_orders_does_not_skip_left_child_of_interior_match() {
    let mut bt = Btree::open_in_memory().unwrap();
    bt.pager_mut().set_page_size(512).unwrap();
    bt.begin_write().unwrap();
    let idx_root = bt.create_index().unwrap();
    let full_orders = [KeySortOrder::default(), KeySortOrder::default()];

    // Populate `[col0, rowid, pad]` with 45 duplicate `col0 == 20` entries flanked by `10` and `30`
    // so `col0 == 20` spans `left_child`, `IndexInterior` divider cells, and `right_child`!
    for rowid in 1..=5i64 {
        let rec = encode_record(&[
            Value::Integer(10),
            Value::Integer(rowid),
            Value::Blob(vec![0x10; 72]),
        ]);
        bt.insert_index(idx_root, &rec, &full_orders).unwrap();
    }
    for rowid in 1..=45i64 {
        let rec = encode_record(&[
            Value::Integer(20),
            Value::Integer(rowid),
            Value::Blob(vec![0x20; 72]),
        ]);
        bt.insert_index(idx_root, &rec, &full_orders).unwrap();
    }
    for rowid in 1..=5i64 {
        let rec = encode_record(&[
            Value::Integer(30),
            Value::Integer(rowid),
            Value::Blob(vec![0x30; 72]),
        ]);
        bt.insert_index(idx_root, &rec, &full_orders).unwrap();
    }

    // Prove that the root is `IndexInterior` and at least one interior divider cell has `col0 == 20`
    // with `rowid > 1` (meaning earlier `(20, 1..interior_rowid)` entries live in its `left_child`!).
    let interior_rowid_for_20 = {
        let usable = bt.pager().usable_size();
        let raw_root = bt.pager_mut().read_page(idx_root).unwrap();
        match ParsedPage::parse(idx_root, usable, raw_root).unwrap() {
            ParsedPage::IndexInterior { cells, .. } => {
                let mut hit = None;
                for c in &cells {
                    let vals = decode_record(&c.local_payload).unwrap();
                    if vals[0] == Value::Integer(20) {
                        let rid = vals[1].as_i64().unwrap();
                        if rid > 1 {
                            hit = Some(rid);
                            break;
                        }
                    }
                }
                hit.expect(
                    "root IndexInterior must contain a divider cell with col0 == 20 and rowid > 1",
                )
            }
            other => panic!("expected multi-level IndexInterior root, got {other:?}"),
        }
    };
    assert!(interior_rowid_for_20 > 1);

    // Open cursor with ONLY 1-element `orders` (`vec![KeySortOrder::default()]` for `col0`),
    // so `target_values.len() == 1 >= orders.len() == 1`!
    let cur = bt.open_index_cursor(idx_root, vec![KeySortOrder::default()], false);

    // 1. `cursor_seek_index_equal_first` + `cursor_next_index_matching` must visit ALL 45 rows `1..=45`!
    let mut matched_rowids = Vec::new();
    let mut ok = bt
        .cursor_seek_index_equal_first(cur, &[Value::Integer(20)])
        .unwrap();
    while ok {
        assert_eq!(bt.cursor_column(cur, 0).unwrap(), Some(Value::Integer(20)));
        matched_rowids.push(bt.cursor_column(cur, 1).unwrap().unwrap().as_i64().unwrap());
        ok = bt
            .cursor_next_index_matching(cur, &[Value::Integer(20)])
            .unwrap();
    }
    let expected_rowids: Vec<i64> = (1..=45).collect();
    assert_eq!(
        matched_rowids, expected_rowids,
        "must not skip matching col0 == 20 entries in left_child of IndexInterior cell (interior divider rowid={interior_rowid_for_20})"
    );

    // 2. `cursor_seek_index_ge` with 1-column prefix must also land on `(20, 1)`!
    assert!(bt.cursor_seek_index_ge(cur, &[Value::Integer(20)]).unwrap());
    assert_eq!(bt.cursor_column(cur, 1).unwrap(), Some(Value::Integer(1)));

    // 3. `cursor_seek_index_ge` with 2-column exact key targeting the `IndexInterior` divider cell
    //    must still step onto `(20, interior_rowid_for_20)` accurately!
    assert!(
        bt.cursor_seek_index_ge(
            cur,
            &[Value::Integer(20), Value::Integer(interior_rowid_for_20)],
        )
        .unwrap()
    );
    assert_eq!(bt.cursor_column(cur, 0).unwrap(), Some(Value::Integer(20)));
    assert_eq!(
        bt.cursor_column(cur, 1).unwrap(),
        Some(Value::Integer(interior_rowid_for_20))
    );

    bt.close_cursor(cur);
    bt.commit().unwrap();
}
