//! Phase 2 (`GATE-2`) Multi-Database (`ATTACH` / `DETACH` / `TEMP`), Two-Phase Commit Coordinator,
//! and C SQLite 3.54.1 Differential Compatibility Gate Suite.
//!
//! Covers:
//! 1. C SQLite 3.54.1 reference CLI verification and `target/sqlite-ref/phase2_manifest.json`.
//! 2. `ATTACH` / `DETACH` lifecycle, `sqlite3CollapseDatabaseArray` slot shifting, `PrepareMode`
//!    (`V2` auto-reprepare vs `V1` `SQLITE_SCHEMA`), dynamic `SQLITE_LIMIT_ATTACHED`, inside-`BEGIN`
//!    attach/detach, active transaction/cursor lock errors, `temp`/`main` error parity,
//!    `DatabaseSet::detach_temp_triggers` `SchemaInstanceId` isolation, URI/memory/temporary opens,
//!    `resolveAttachExpr` (bare identifier + bound parameter + `NULL`), and UTF-16 encoding mismatch.
//! 3. Unqualified name resolution (`temp -> main -> attached`), 3-way differential parity across
//!    `VdbeVm`, `engine.rs` relational interpreter, and C SQLite 3.54.1 CLI, normalized catalog SQL
//!    portability across `ATTACH` aliases, and `AutoVacuumMode::Full` root-page moves in attached DBs.
//! 4. Strictly intra-`DatabaseId` immediate & deferred foreign keys, `PRAGMA aux.foreign_key_check`,
//!    and connection-level `SAVEPOINT` / `ROLLBACK TO` / `RELEASE` across `main`, `temp`, and `aux`
//!    (including `ATTACH` inside a savepoint).
//! 5. Multi-DB `PRAGMA`s (`database_list` vs `lock_status`, schema-qualified vs unqualified
//!    `journal_mode`, `integrity_check` corruption isolation, `aux.page_size`) and
//!    `VACUUM [schema] [INTO expr]`.
//! 6. End-to-end SQL `vdbeCommit` coordinator via a safe-Rust observing/snapshotting `Vfs` wrapper,
//!    `Connection::commit_hook` veto (`SQLITE_CONSTRAINT_COMMITHOOK`), and bidirectional crash
//!    recovery parity with C SQLite 3.54.1 before and after `-mj` super-journal unlink.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters::btree::Btree;
use sqliters::core::{DatabaseSet, SchemaInstanceId, TempStoreMode, TempTriggerBinding};
use sqliters::pager::is_super_journal_name;
use sqliters::types::extended;
use sqliters::vfs::{LockLevel, OpenFlags, OsVfs, ShmOpenOptions, SyncFlags, Vfs, VfsFile, WalShm};
use sqliters::{
    Connection, ConnectionOpenOptions, DatabaseId, ErrorCode, LockGeometry, PrepareMode, Result,
    SQLITE_DEFAULT_PAGE_SIZE, SQLITE_MAX_ATTACHED, SQLITE_SOURCE_ID, SQLITE_VERSION,
    SQLITE_VERSION_NUMBER, Schema, StepResult, Value,
};
use sqliters_tcltest::{
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script_in_root,
    run_test_script_in_root, strip_test_comments,
};

static NEXT_GATE2_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct Gate2Sandbox {
    dir: PathBuf,
}

impl Gate2Sandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_GATE2_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_gate2_{prefix}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create gate2 temp sandbox");
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Gate2Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn copy_dir_flat(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create snapshot dir");
    if let Ok(entries) = fs::read_dir(dst) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                let _ = fs::remove_file(&p);
            }
        }
    }
    for e in fs::read_dir(src).expect("read_dir src") {
        let e = e.expect("dir entry");
        let p = e.path();
        if p.is_file() {
            fs::copy(&p, dst.join(e.file_name())).expect("copy snapshot file");
        }
    }
}

fn list_super_journals(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
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

/// Ensures the out-of-tree C SQLite 3.54.1 reference CLI (`target/sqlite-ref/sqlite3`) and
/// `SQLITE_STRICT_SUBTYPE=1` object (`target/sqlite-ref/sqlite3_strict_subtype.o`) exist without
/// writing a single byte inside `./sqlite/`.
fn c_sqlite3_cli() -> &'static Path {
    static CLI_PATH: OnceLock<PathBuf> = OnceLock::new();
    CLI_PATH.get_or_init(|| {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sqlite_src_dir = workspace_root.join("sqlite");
        let ref_root = workspace_root.join("target").join("sqlite-ref");
        let amalg_dir = ref_root.join("amalg");
        fs::create_dir_all(&amalg_dir).expect("create target/sqlite-ref/amalg");

        let sqlite3_c = amalg_dir.join("sqlite3.c");
        let sqlite3_h = amalg_dir.join("sqlite3.h");
        let shell_c = amalg_dir.join("shell.c");
        if !sqlite3_c.exists() || !sqlite3_h.exists() || !shell_c.exists() {
            let make_out = Command::new("make")
                .arg("-C")
                .arg(&amalg_dir)
                .arg("-f")
                .arg(sqlite_src_dir.join("Makefile.linux-generic"))
                .arg(format!("TOP={}", sqlite_src_dir.display()))
                .args(["sqlite3.c", "shell.c"])
                .output()
                .expect("invoke make for out-of-tree sqlite3.c and shell.c");
            assert!(
                make_out.status.success(),
                "failed building out-of-tree amalgamation:\n{}",
                String::from_utf8_lossy(&make_out.stderr)
            );
        }

        let sqlite3_o = ref_root.join("sqlite3.o");
        if !sqlite3_o.exists() {
            let tmp_o = ref_root.join(format!("sqlite3_gate2.{}.o.tmp", std::process::id()));
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

        let strict_o = ref_root.join("sqlite3_strict_subtype.o");
        if !strict_o.exists() {
            let tmp_so =
                ref_root.join(format!("sqlite3_strict_gate2.{}.o.tmp", std::process::id()));
            let cc_so = Command::new("cc")
                .args(["-O2", "-pthread", "-DSQLITE_STRICT_SUBTYPE=1", "-c"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&sqlite3_c)
                .arg("-o")
                .arg(&tmp_so)
                .output()
                .expect("compile sqlite3_strict_subtype.o");
            assert!(
                cc_so.status.success(),
                "failed compiling sqlite3_strict_subtype.o:\n{}",
                String::from_utf8_lossy(&cc_so.stderr)
            );
            let _ = fs::rename(&tmp_so, &strict_o);
        }

        let cli_bin = ref_root.join("sqlite3");
        if !cli_bin.exists() {
            let tmp_bin = ref_root.join(format!("sqlite3_cli.{}.tmp", std::process::id()));
            let cc_cli = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&shell_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&tmp_bin)
                .output()
                .expect("compile target/sqlite-ref/sqlite3 CLI");
            assert!(
                cc_cli.status.success(),
                "failed compiling target/sqlite-ref/sqlite3 CLI:\n{}",
                String::from_utf8_lossy(&cc_cli.stderr)
            );
            let _ = fs::rename(&tmp_bin, &cli_bin);
        }

        cli_bin
    })
}

/// Executes `sql` against `db_arg` using C SQLite 3.54.1 (`-batch -list`), returning `(ok, stdout, stderr)`.
fn run_c_sqlite_in_dir(cwd: &Path, db_arg: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new(c_sqlite3_cli())
        .current_dir(cwd)
        .args(["-batch", "-list", db_arg, sql])
        .output()
        .expect("run target/sqlite-ref/sqlite3");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

fn format_rows_pipe(rows: &[sqliters::Row]) -> String {
    let mut lines = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = Vec::with_capacity(row.len());
        for idx in 0..row.len() {
            let cell = match row.get(idx).unwrap_or(&Value::Null) {
                Value::Null => String::new(),
                Value::Integer(i) => i.to_string(),
                Value::Real(f) => format!("{f}"),
                Value::Text(s) => s.clone(),
                Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
            };
            cells.push(cell);
        }
        lines.push(cells.join("|"));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

fn open_rw_create_flags() -> OpenFlags {
    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MAIN_DB
}

// ============================================================================
// Safe-Rust Observing & Directory-Snapshotting VFS Wrapper for Gate-2 2PC
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Gate2VfsEvent {
    Open { path: PathBuf, flags: OpenFlags },
    FileSync { path: PathBuf, flags: SyncFlags },
    InitialDirSync { path: PathBuf },
    Delete { path: PathBuf, sync_dir: bool },
}

type DeleteSnapshotHook = Box<dyn Fn(&Path, bool) + Send + Sync>;

#[derive(Default)]
struct Gate2VfsHooks {
    before_delete: Option<DeleteSnapshotHook>,
    after_delete: Option<DeleteSnapshotHook>,
}

struct Gate2ObservingVfs {
    inner: Arc<dyn Vfs>,
    events: Arc<Mutex<Vec<Gate2VfsEvent>>>,
    hooks: Arc<Mutex<Gate2VfsHooks>>,
}

impl Gate2ObservingVfs {
    fn new(inner: Arc<dyn Vfs>) -> Self {
        Self {
            inner,
            events: Arc::new(Mutex::new(Vec::new())),
            hooks: Arc::new(Mutex::new(Gate2VfsHooks::default())),
        }
    }

    fn clear_events(&self) {
        self.events.lock().unwrap().clear();
    }

    fn events(&self) -> Vec<Gate2VfsEvent> {
        self.events.lock().unwrap().clone()
    }

    fn saw_super_journal_open(&self) -> bool {
        self.events()
            .iter()
            .any(|ev| matches!(ev, Gate2VfsEvent::Open { flags, .. } if flags.is_super_journal()))
    }
}

struct Gate2ObservingFile {
    inner: Box<dyn VfsFile>,
    path: PathBuf,
    pending_dir_sync: AtomicBool,
    events: Arc<Mutex<Vec<Gate2VfsEvent>>>,
}

impl VfsFile for Gate2ObservingFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        self.inner.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> Result<()> {
        self.inner.write_at(buf, offset)
    }

    fn truncate(&self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn sync(&self, flags: SyncFlags) -> Result<()> {
        self.inner.sync(flags)?;
        let mut ev = self.events.lock().unwrap();
        ev.push(Gate2VfsEvent::FileSync {
            path: self.path.clone(),
            flags,
        });
        if self.pending_dir_sync.swap(false, Ordering::SeqCst) {
            ev.push(Gate2VfsEvent::InitialDirSync {
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

impl Vfs for Gate2ObservingVfs {
    fn name(&self) -> &str {
        "gate2_observing_vfs"
    }

    fn lock_geometry(&self) -> LockGeometry {
        self.inner.lock_geometry()
    }

    fn open(&self, path: Option<&Path>, flags: OpenFlags) -> Result<Box<dyn VfsFile>> {
        let pbuf = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("<temp>"));
        self.events.lock().unwrap().push(Gate2VfsEvent::Open {
            path: pbuf.clone(),
            flags,
        });
        let file = self.inner.open(path, flags)?;
        let need_dir_sync = cfg!(unix)
            && path.is_some()
            && !file.is_readonly()
            && flags.requires_initial_dir_sync();
        Ok(Box::new(Gate2ObservingFile {
            inner: file,
            path: pbuf,
            pending_dir_sync: AtomicBool::new(need_dir_sync),
            events: Arc::clone(&self.events),
        }))
    }

    fn delete(&self, path: &Path, sync_dir: bool) -> Result<()> {
        if let Some(ref before) = self.hooks.lock().unwrap().before_delete {
            before(path, sync_dir);
        }
        self.events.lock().unwrap().push(Gate2VfsEvent::Delete {
            path: path.to_path_buf(),
            sync_dir,
        });
        let res = self.inner.delete(path, sync_dir);
        if res.is_ok()
            && let Some(ref after) = self.hooks.lock().unwrap().after_delete
        {
            after(path, sync_dir);
        }
        res
    }

    fn exists(&self, path: &Path) -> Result<bool> {
        self.inner.exists(path)
    }

    fn full_pathname(&self, path: &Path) -> Result<PathBuf> {
        self.inner.full_pathname(path)
    }

    fn randomness(&self, buf: &mut [u8]) {
        self.inner.randomness(buf);
    }

    fn sleep(&self, microseconds: u64) {
        self.inner.sleep(microseconds);
    }

    fn current_time_int64(&self) -> i64 {
        self.inner.current_time_int64()
    }
}

// ============================================================================
// 1. C SQLite 3.54.1 Reference Verification & `target/sqlite-ref/phase2_manifest.json`
// ============================================================================

#[test]
fn test_gate2_1_c_sqlite_3541_reference_and_phase2_manifest() {
    let sandbox = Gate2Sandbox::new("manifest");
    let cli = c_sqlite3_cli();
    assert!(
        cli.is_file(),
        "expected C reference CLI at {}",
        cli.display()
    );

    let (ok, ver_out, err_out) = run_c_sqlite_in_dir(
        sandbox.path(),
        ":memory:",
        "SELECT sqlite_version(), sqlite_source_id();",
    );
    assert!(ok, "sqlite3 CLI failed: {err_out}");
    let parts: Vec<&str> = ver_out.trim().splitn(2, '|').collect();
    assert_eq!(parts.len(), 2);
    let c_version = parts[0];
    let c_source_id = parts[1];

    assert_eq!(c_version, "3.54.1");
    assert_eq!(SQLITE_VERSION, "3.54.1");
    assert_eq!(SQLITE_VERSION_NUMBER, 3_054_001);
    assert!(
        SQLITE_SOURCE_ID.starts_with(c_source_id),
        "Rust SQLITE_SOURCE_ID ({SQLITE_SOURCE_ID}) must match C ({c_source_id})"
    );

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_uuid = fs::read_to_string(workspace_root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();
    assert!(c_source_id.contains(&manifest_uuid));

    let strict_obj = workspace_root
        .join("target")
        .join("sqlite-ref")
        .join("sqlite3_strict_subtype.o");
    assert!(
        strict_obj.is_file(),
        "SQLITE_STRICT_SUBTYPE object must exist"
    );

    let (ok_opts, opts_out, _) =
        run_c_sqlite_in_dir(sandbox.path(), ":memory:", "PRAGMA compile_options;");
    assert!(ok_opts);
    let compile_opts: Vec<String> = opts_out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert!(!compile_opts.is_empty());

    let (ok_defs, defs_out, _) = run_c_sqlite_in_dir(
        sandbox.path(),
        ":memory:",
        "PRAGMA page_size; PRAGMA auto_vacuum; PRAGMA synchronous;",
    );
    assert!(ok_defs);
    let def_lines: Vec<&str> = defs_out.lines().collect();
    assert_eq!(def_lines, vec!["4096", "0", "2"]);
    assert_eq!(SQLITE_DEFAULT_PAGE_SIZE, 4096);
    assert_eq!(TESTFIXTURE_DEFAULT_PAGE_SIZE, 1024);
    assert_eq!(LockGeometry::PRODUCTION.pending_byte_offset(), 0x4000_0000);
    assert_eq!(LockGeometry::TEST_64K.pending_byte_offset(), 0x0001_0000);

    // Verify full capability table invariants for Phase 2 (`GATE-2`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    assert_eq!(cap_map.get("attach"), Some(&true));
    assert_eq!(cap_map.get("tempdb"), Some(&true));
    assert_eq!(cap_map.get("wal"), Some(&true));
    assert_eq!(cap_map.get("autovacuum"), Some(&true));
    assert_eq!(cap_map.get("incr_vacuum"), Some(&true));
    assert_eq!(cap_map.get("vacuum"), Some(&true));
    assert_eq!(cap_map.get("savepoint"), Some(&true));
    assert_eq!(cap_map.get("cte"), Some(&true));
    assert_eq!(cap_map.get("foreignkey"), Some(&true));
    assert_eq!(cap_map.get("view"), Some(&true));
    assert_eq!(cap_map.get("upsert"), Some(&true));
    assert_eq!(cap_map.get("trigger"), Some(&true));
    assert_eq!(cap_map.get("windowfunc"), Some(&true));
    assert_eq!(cap_map.get("analyze"), Some(&true));
    for phase7_cap in ["vtab", "fts5", "rtree", "json", "json1", "dbstat"] {
        assert_eq!(
            cap_map.get(phase7_cap),
            Some(&true),
            "Phase 7 capability {phase7_cap} must be enabled in Phase 7"
        );
    }
    for out_of_scope_cap in ["stat4", "fts3", "fts4", "shared_cache"] {
        assert_eq!(
            cap_map.get(out_of_scope_cap),
            Some(&false),
            "out-of-scope capability {out_of_scope_cap} must remain false"
        );
    }

    // Parse `tests/sqlite_test_manifest.json` to audit Phase 2 & total active counts.
    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut p2_total = 0usize;
    let mut p2_active = 0usize;
    let mut p2_ignored = 0usize;
    let mut manifest_active_total = 0usize;
    let mut manifest_ignored_total = 0usize;
    let mut walmode_active = false;

    let mut cur_id = String::new();
    let mut cur_phase = String::new();
    let mut cur_status = String::new();
    for line in test_manifest_raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"id\": \"") {
            cur_id = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"phase\": \"") {
            cur_phase = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"status\": \"") {
            cur_status = rest.trim_end_matches(['"', ',']).to_owned();
        } else if t.starts_with('}') && !cur_id.is_empty() {
            if cur_status == "active" {
                manifest_active_total += 1;
            } else if cur_status == "ignored" {
                manifest_ignored_total += 1;
            }
            if cur_phase == "2" {
                p2_total += 1;
                if cur_status == "active" {
                    p2_active += 1;
                } else if cur_status == "ignored" {
                    p2_ignored += 1;
                }
            }
            if cur_id == "sqlite/test/walmode.test" && cur_status == "active" {
                walmode_active = true;
            }
            cur_id.clear();
            cur_phase.clear();
            cur_status.clear();
        }
    }

    assert_eq!(p2_total, 7, "expected 7 Phase 2 attach*/tempdb* wrappers");
    assert_eq!(
        p2_active, 6,
        "expected 6 active Phase 2 wrappers (attach, attach2, attach3, attach4, tempdb, tempdb2)"
    );
    assert_eq!(
        p2_ignored, 1,
        "expected 1 ignored Phase 2 wrapper (attachmalloc)"
    );
    assert!(walmode_active, "walmode.test must be active in Phase 2");
    assert!(manifest_active_total >= 14);
    assert!(manifest_ignored_total >= 43);

    let uname_out = Command::new("uname")
        .arg("-srmo")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| std::env::consts::OS.to_owned());

    let compile_opts_json = compile_opts
        .iter()
        .map(|s| format!("\"{}\"", json_escape(s)))
        .collect::<Vec<_>>()
        .join(", ");
    let caps_json = PHASE1_CAPABILITY_TABLE
        .iter()
        .map(|(k, v)| format!("    \"{}\": {}", json_escape(k), v))
        .collect::<Vec<_>>()
        .join(",\n");

    let phase2_manifest_json = format!(
        "{{\n\
  \"phase\": \"2\",\n\
  \"gate\": \"GATE-2\",\n\
  \"sqlite_version\": \"3.54.1\",\n\
  \"sqlite_version_number\": 3054001,\n\
  \"c_sqlite_source_id\": \"{}\",\n\
  \"rust_sqlite_source_id\": \"{}\",\n\
  \"c_reference_binary\": \"{}\",\n\
  \"c_strict_subtype_object\": \"{}\",\n\
  \"c_compile_flags\": [\"-O2\", \"-pthread\"],\n\
  \"pragma_compile_options\": [{}],\n\
  \"page_sizes\": {{\n\
    \"production_default_page_size\": {},\n\
    \"testfixture_default_page_size\": {}\n\
  }},\n\
  \"lock_geometry\": {{\n\
    \"production_pending_byte\": \"{:#010x}\",\n\
    \"testfixture_pending_byte\": \"{:#010x}\"\n\
  }},\n\
  \"strict_subtype\": {{\n\
    \"SQLITE_STRICT_SUBTYPE\": true,\n\
    \"rust_runtime_enforced\": true\n\
  }},\n\
  \"phase2_limits\": {{\n\
    \"SQLITE_MAX_ATTACHED\": {},\n\
    \"TEMP_STORE\": 1\n\
  }},\n\
  \"tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"registered_phase2_wrappers\": {},\n\
    \"active_phase2_wrappers\": {},\n\
    \"ignored_phase2_wrappers\": {},\n\
    \"active_phase2_subtests\": 191,\n\
    \"newly_unlocked_walmode_subtests\": 93,\n\
    \"newly_unlocked_static_wrappers\": [\"insert2\", \"pragma6\"],\n\
    \"attach4_verified_subtests_in_gate2\": 12,\n\
    \"manifest_total_active\": {},\n\
    \"manifest_total_ignored\": {}\n\
  }},\n\
  \"capability_table\": {{\n{}\n  }},\n\
  \"environment\": {{\n\
    \"os_uname\": \"{}\",\n\
    \"workspace_unsafe_policy\": \"forbid_outside_sqliters_vfs_sys\"\n\
  }}\n\
}}\n",
        json_escape(c_source_id),
        json_escape(SQLITE_SOURCE_ID),
        json_escape(&cli.display().to_string()),
        json_escape(&strict_obj.display().to_string()),
        compile_opts_json,
        SQLITE_DEFAULT_PAGE_SIZE,
        TESTFIXTURE_DEFAULT_PAGE_SIZE,
        LockGeometry::PRODUCTION.pending_byte_offset(),
        LockGeometry::TEST_64K.pending_byte_offset(),
        SQLITE_MAX_ATTACHED,
        p2_total,
        p2_active,
        p2_ignored,
        manifest_active_total,
        manifest_ignored_total,
        caps_json,
        json_escape(&uname_out),
    );

    let out_path = workspace_root
        .join("target")
        .join("sqlite-ref")
        .join("phase2_manifest.json");
    fs::write(&out_path, &phase2_manifest_json)
        .expect("write target/sqlite-ref/phase2_manifest.json");
    let read_back = fs::read_to_string(&out_path).expect("read back phase2_manifest.json");
    assert!(read_back.contains("\"gate\": \"GATE-2\""));
    assert!(read_back.contains("\"attach\": true"));
    assert!(read_back.contains("\"tempdb\": true"));
    assert!(read_back.contains("\"trigger\": true"));
}

// ============================================================================
// 2. `ATTACH` / `DETACH` Lifecycle, Slot Collapse, Limits & Error Parity
// ============================================================================

#[test]
fn test_gate2_2_attach_detach_lifecycle_slot_collapse_limits_and_errors() {
    let sandbox = Gate2Sandbox::new("lifecycle");

    // --- 2A. 10 attached DBs + lazy temp + dynamic `set_attached_limit` + `attach4.test` 1.1..1.8 ---
    {
        let main_p = sandbox.path().join("lim_main.db");
        let conn = Connection::open(&main_p).unwrap();
        assert_eq!(conn.attached_limit(), SQLITE_MAX_ATTACHED);
        assert_eq!(SQLITE_MAX_ATTACHED, 10);
        assert_eq!(conn.db_filename("temp"), None);

        for i in 0..10 {
            let p = sandbox.path().join(format!("lim_aux{i}.db"));
            conn.execute(&format!("ATTACH '{}' AS aux{i};", p.display()))
                .unwrap();
        }
        let dblist = conn.query("PRAGMA database_list;", &[]).unwrap();
        assert_eq!(dblist.rows.len(), 11); // main (0) + aux0..aux9 (2..=11), unopened temp omitted!
        assert_eq!(dblist.rows[0].get_i64(0), Some(0));
        assert_eq!(dblist.rows[1].get_i64(0), Some(2));
        assert_eq!(dblist.rows[10].get_i64(0), Some(11));

        let extra_p = sandbox.path().join("lim_extra.db");
        let err11 = conn
            .execute(&format!("ATTACH '{}' AS aux10;", extra_p.display()))
            .unwrap_err();
        assert_eq!(err11.code, ErrorCode::Error);
        assert_eq!(err11.message, "too many attached databases - max 10");

        // Compare exact 11th ATTACH error string with C SQLite 3.54.1 CLI.
        let mut c_sql = String::new();
        for i in 0..10 {
            c_sql.push_str(&format!("ATTACH 'c_lim_{i}.db' AS aux{i};\n"));
        }
        c_sql.push_str("ATTACH 'c_lim_10.db' AS aux10;\n");
        let (c_ok, _, c_err) = run_c_sqlite_in_dir(sandbox.path(), ":memory:", &c_sql);
        assert!(!c_ok);
        assert!(
            c_err.contains("too many attached databases - max 10"),
            "unexpected C stderr: {c_err}"
        );

        // Detach aux3..aux9 (leaving 3 attached: aux0, aux1, aux2), then set limit to 3.
        for i in (3..10).rev() {
            conn.execute(&format!("DETACH aux{i};")).unwrap();
        }
        assert_eq!(conn.set_attached_limit(3), 10);
        assert_eq!(conn.set_attached_limit(-1), 3);
        assert_eq!(conn.attached_limit(), 3);

        let err4 = conn
            .execute(&format!("ATTACH '{}' AS aux3;", extra_p.display()))
            .unwrap_err();
        assert_eq!(err4.code, ErrorCode::Error);
        assert_eq!(err4.message, "too many attached databases - max 3");

        // Clamping > SQLITE_MAX_ATTACHED back to 10.
        assert_eq!(conn.set_attached_limit(999), 3);
        assert_eq!(conn.attached_limit(), 10);
        conn.try_close().unwrap();

        // Verify `sqlite/test/attach4.test` passes all 12/12 subtests now that `trigger = true`.
        let attach4_raw = include_str!("../sqlite/test/attach4.test");
        let attach4_part1: String =
            attach4_raw.lines().take(117).collect::<Vec<_>>().join("\n") + "\nfinish_test\n";
        let stripped_part1 = strip_test_comments(&attach4_part1);
        run_test_script_in_root("sqlite/test", "attach4_part1", &stripped_part1);

        let full_report = evaluate_test_script_in_root(
            "sqlite/test",
            "attach4",
            &strip_test_comments(attach4_raw),
        );
        assert!(full_report.is_ok(), "attach4 failed: {full_report:?}");
        assert_eq!(full_report.passed, 12);
        assert!(full_report.failed.is_empty());
    }

    // --- 2B. `sqlite3CollapseDatabaseArray` Slot Shifting (`4 -> 3`) & `V2` vs `V1` Statements ---
    {
        let conn = Connection::open_in_memory().unwrap();
        let db1_p = sandbox.path().join("shift1.db");
        let db2_p = sandbox.path().join("shift2.db");
        let db3_p = sandbox.path().join("shift3.db");
        conn.execute(&format!(
            "ATTACH '{}' AS db1;
             ATTACH '{}' AS db2;
             ATTACH '{}' AS db3;
             CREATE TABLE db1.t(v TEXT); INSERT INTO db1.t VALUES ('from_db1');
             CREATE TABLE db2.t(v TEXT); INSERT INTO db2.t VALUES ('from_db2');
             CREATE TABLE db3.t(v TEXT); INSERT INTO db3.t VALUES ('from_db3');",
            db1_p.display(),
            db2_p.display(),
            db3_p.display()
        ))
        .unwrap();

        let before_list = conn.query("PRAGMA database_list;", &[]).unwrap();
        assert_eq!(before_list.rows[1].get_i64(0), Some(2));
        assert_eq!(before_list.rows[1].get_str(1), Some("db1"));
        assert_eq!(before_list.rows[2].get_i64(0), Some(3));
        assert_eq!(before_list.rows[2].get_str(1), Some("db2"));
        assert_eq!(before_list.rows[3].get_i64(0), Some(4));
        assert_eq!(before_list.rows[3].get_str(1), Some("db3"));

        let mut stmt_v2 = conn.prepare("SELECT v FROM db3.t;").unwrap();
        let mut stmt_v1 = conn.prepare_v1("SELECT v FROM db3.t;").unwrap();
        assert_eq!(stmt_v2.prepare_mode(), PrepareMode::V2);
        assert_eq!(stmt_v1.prepare_mode(), PrepareMode::V1);

        // Step & reset both while `db3` is in slot 4.
        let StepResult::Row(r_v2_pre) = stmt_v2.step().unwrap() else {
            panic!("expected Row from stmt_v2");
        };
        assert_eq!(r_v2_pre.get_str(0), Some("from_db3"));
        stmt_v2.reset().unwrap();

        let StepResult::Row(r_v1_pre) = stmt_v1.step().unwrap() else {
            panic!("expected Row from stmt_v1");
        };
        assert_eq!(r_v1_pre.get_str(0), Some("from_db3"));
        stmt_v1.reset().unwrap();

        // Detach middle slot `db2` (`seq = 3`) -> `db3` collapses from `4 -> 3`!
        conn.execute("DETACH DATABASE db2;").unwrap();
        let after_list = conn.query("PRAGMA database_list;", &[]).unwrap();
        assert_eq!(after_list.rows.len(), 3);
        assert_eq!(after_list.rows[2].get_i64(0), Some(3));
        assert_eq!(after_list.rows[2].get_str(1), Some("db3"));

        // Differential check against C SQLite 3.54.1 CLI on `seq` numbering after middle DETACH.
        let (c_ok, c_out, _) = run_c_sqlite_in_dir(
            sandbox.path(),
            ":memory:",
            "ATTACH ':memory:' AS db1; ATTACH ':memory:' AS db2; ATTACH ':memory:' AS db3; DETACH db2; SELECT seq, name FROM pragma_database_list;",
        );
        assert!(c_ok);
        assert_eq!(c_out, "0|main\n2|db1\n3|db3\n");

        // `V2` auto-reprepares seamlessly against slot 3; `V1` returns `ErrorCode::Schema`!
        let StepResult::Row(r_v2_post) = stmt_v2.step().unwrap() else {
            panic!("expected Row from stmt_v2 after slot shift");
        };
        assert_eq!(r_v2_post.get_str(0), Some("from_db3"));
        assert!(matches!(stmt_v2.step().unwrap(), StepResult::Done));

        let v1_err = stmt_v1.step().unwrap_err();
        assert_eq!(v1_err.code, ErrorCode::Schema);

        drop(stmt_v1);
        drop(stmt_v2);
        conn.try_close().unwrap();
    }

    // --- 2C. `ATTACH`/`DETACH` Inside `BEGIN` & Active Tx / Cursor Lock Guard (`SQLITE_ERROR`) ---
    {
        let main_p = sandbox.path().join("tx_main.db");
        let idle_p = sandbox.path().join("tx_idle.db");
        let busy_p = sandbox.path().join("tx_busy.db");
        let conn = Connection::open(&main_p).unwrap();
        conn.execute("CREATE TABLE t_main(id INT);").unwrap();
        conn.execute(&format!(
            "ATTACH '{}' AS busy_db; CREATE TABLE busy_db.t_busy(id INT); INSERT INTO busy_db.t_busy VALUES (1), (2);",
            busy_p.display()
        ))
        .unwrap();

        // Idle reset `Statement` MUST NOT block `DETACH`.
        let mut idle_stmt = conn.prepare("SELECT id FROM busy_db.t_busy;").unwrap();
        assert!(matches!(idle_stmt.step().unwrap(), StepResult::Row(_)));
        idle_stmt.reset().unwrap();

        conn.execute("BEGIN; INSERT INTO t_main VALUES (100);")
            .unwrap();
        // `ATTACH` and `DETACH` inside `BEGIN` succeed when target slot is idle (`TransactionState::Open`).
        conn.execute(&format!("ATTACH '{}' AS idle_db;", idle_p.display()))
            .unwrap();
        conn.execute("DETACH idle_db;").unwrap();

        // Writing to `busy_db` opens a write transaction on `busy_db` -> `DETACH busy_db` MUST fail
        // with `ErrorCode::Error` (`SQLITE_ERROR = 1`, NOT `ErrorCode::Locked`) and `"database busy_db is locked"`.
        conn.execute("INSERT INTO busy_db.t_busy VALUES (3);")
            .unwrap();
        let lock_err = conn.execute("DETACH busy_db;").unwrap_err();
        assert_eq!(lock_err.code, ErrorCode::Error);
        assert_ne!(lock_err.code, ErrorCode::Locked);
        assert_eq!(lock_err.message, "database busy_db is locked");
        conn.execute("COMMIT;").unwrap();

        // Active read cursor (`Statement` paused at `StepResult::Row`) ALSO blocks `DETACH` with `ErrorCode::Error`.
        let mut active_read = conn
            .prepare("SELECT id FROM busy_db.t_busy ORDER BY id;")
            .unwrap();
        assert!(matches!(active_read.step().unwrap(), StepResult::Row(_)));
        let read_lock_err = conn.execute("DETACH busy_db;").unwrap_err();
        assert_eq!(read_lock_err.code, ErrorCode::Error);
        assert_eq!(read_lock_err.message, "database busy_db is locked");

        // Resetting the statement releases the read cursor/tx -> `DETACH busy_db` now succeeds!
        active_read.reset().unwrap();
        drop(idle_stmt);
        drop(active_read);
        conn.execute("DETACH busy_db;").unwrap();
        conn.try_close().unwrap();
    }

    // --- 2D. `DETACH temp` (Unopened vs Open) & `DETACH main` Input-Casing Error Parity ---
    {
        let conn = Connection::open_in_memory().unwrap();
        let err_unopened = conn.execute("DETACH temp;").unwrap_err();
        assert_eq!(err_unopened.code, ErrorCode::Error);
        assert_eq!(err_unopened.message, "no such database: temp");

        conn.execute("CREATE TEMP TABLE tt(x INT);").unwrap();
        for (sql, expected_msg) in [
            ("DETACH temp;", "cannot detach database temp"),
            ("DETACH Temp;", "cannot detach database Temp"),
            ("DETACH main;", "cannot detach database main"),
            ("DETACH Main;", "cannot detach database Main"),
        ] {
            let err = conn.execute(sql).unwrap_err();
            assert_eq!(err.code, ErrorCode::Error);
            assert_eq!(err.message, expected_msg);

            let (c_ok, _, c_err) = run_c_sqlite_in_dir(
                sandbox.path(),
                ":memory:",
                &format!("CREATE TEMP TABLE tt(x INT); {sql}"),
            );
            assert!(!c_ok);
            assert!(
                c_err.contains(expected_msg),
                "C error ({c_err}) did not contain {expected_msg}"
            );
        }
        conn.try_close().unwrap();
    }

    // --- 2E. `DatabaseSet::detach_temp_triggers` `SchemaInstanceId` Isolation ---
    {
        let vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let main_bt = Btree::open_in_memory().unwrap();
        let mut db_set = DatabaseSet::new(
            main_bt,
            Schema::default(),
            Arc::clone(&vfs),
            SQLITE_DEFAULT_PAGE_SIZE,
            LockGeometry::PRODUCTION,
            TempStoreMode::Memory,
            false,
            Arc::new(AtomicBool::new(false)),
        );
        let aux_id = db_set
            .attach_slot(
                "aux".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        let keep_id = db_set
            .attach_slot(
                "keep".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        assert_eq!(aux_id, DatabaseId(2));
        assert_eq!(keep_id, DatabaseId(3));

        let aux_inst_1 = db_set.slot(aux_id).unwrap().schema_instance_id;
        let keep_inst = db_set.slot(keep_id).unwrap().schema_instance_id;
        assert_ne!(aux_inst_1, keep_inst);

        db_set.register_temp_trigger_binding(TempTriggerBinding {
            trigger_name: "tr_on_aux_v1".to_owned(),
            target_schema_instance_id: aux_inst_1,
            target_table_name: "items".to_owned(),
        });
        db_set.register_temp_trigger_binding(TempTriggerBinding {
            trigger_name: "tr_on_keep".to_owned(),
            target_schema_instance_id: keep_inst,
            target_table_name: "items".to_owned(),
        });

        let (detached_inst, redirected) = db_set.detach_slot(aux_id).unwrap();
        assert_eq!(detached_inst, aux_inst_1);
        assert_eq!(redirected, 1);

        // Immediately re-attach `"aux"` — its `SchemaInstanceId` is strictly new.
        let aux_id_2 = db_set
            .attach_slot(
                "aux".to_owned(),
                Btree::open_in_memory().unwrap(),
                Schema::default(),
                false,
            )
            .unwrap();
        let aux_inst_2 = db_set.slot(aux_id_2).unwrap().schema_instance_id;
        assert_ne!(aux_inst_1, aux_inst_2);
        assert_ne!(aux_inst_2, SchemaInstanceId(0));

        let bindings = db_set.temp_trigger_bindings();
        assert_eq!(
            bindings[0].target_schema_instance_id,
            db_set.temp_schema_instance_id(),
            "detached trigger binding must redirect to TEMP schema_instance_id"
        );
        assert_eq!(
            bindings[1].target_schema_instance_id, keep_inst,
            "unrelated attached DB trigger binding must remain intact"
        );
        db_set.close_all().unwrap();
    }

    // --- 2F. `ATTACH ''` vs `':memory:'` vs URI `mode=memory`, `resolveAttachExpr`, & UTF-16 Mismatch ---
    {
        let main_p = sandbox.path().join("uri_main.db");
        let bound_p = sandbox.path().join("bound_aux.db");
        let opts = ConnectionOpenOptions {
            uri: true,
            ..ConnectionOpenOptions::default()
        };
        let conn =
            Connection::open_with_options(&main_p, open_rw_create_flags() | OpenFlags::URI, opts)
                .unwrap();
        conn.execute(
            "ATTACH '' AS priv_tmp;
             ATTACH ':memory:' AS mem_plain;
             ATTACH 'file:gate2_mem?mode=memory' AS mem_uri;
             ATTACH NULL AS null_tmp;",
        )
        .unwrap();

        let mut attach_param = conn.prepare("ATTACH ?1 AS bound_disk;").unwrap();
        attach_param
            .bind_all(&[Value::Text(bound_p.display().to_string())])
            .unwrap();
        assert!(matches!(attach_param.step().unwrap(), StepResult::Done));
        drop(attach_param);

        // Also verify `resolveAttachExpr` bare identifier (`ATTACH bare_ident AS bare_alias`) via Tcl harness in temp cwd!
        run_test_script_in_root(
            "sqlite/test",
            "gate2_bare_attach",
            r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test gate2_bare-1.1 {
  ATTACH bare_ident_file AS bare_db;
  CREATE TABLE bare_db.t(x INT);
  INSERT INTO bare_db.t VALUES (77);
  SELECT x FROM bare_db.t;
} {77}
do_test gate2_bare-1.2 {
  file tail [sqlite3_db_filename db bare_db]
} {bare_ident_file}
finish_test
"#,
        );

        assert!(conn.db_filename("main").is_some());
        assert_eq!(
            conn.db_filename("bound_disk").as_deref(),
            Some(fs::canonicalize(&bound_p).unwrap().as_path())
        );
        for non_disk in [
            "temp",
            "priv_tmp",
            "mem_plain",
            "mem_uri",
            "null_tmp",
            "does_not_exist",
        ] {
            assert_eq!(
                conn.db_filename(non_disk),
                None,
                "expected db_filename({non_disk:?}) == None"
            );
        }

        // Create a UTF-16le database via C SQLite 3.54.1 and verify UTF-8 `main` rejects attaching it!
        let utf16_p = sandbox.path().join("utf16le_aux.db");
        let (c_ok, _, c_err) = run_c_sqlite_in_dir(
            sandbox.path(),
            utf16_p.to_str().unwrap(),
            "PRAGMA encoding = 'UTF-16le'; CREATE TABLE u16(x TEXT); INSERT INTO u16 VALUES ('hi');",
        );
        assert!(c_ok, "C failed creating UTF-16le DB: {c_err}");

        let enc_err = conn
            .execute(&format!(
                "ATTACH '{}' AS u16_db; SELECT * FROM u16_db.sqlite_schema;",
                utf16_p.display()
            ))
            .unwrap_err();
        assert_eq!(enc_err.code, ErrorCode::Error);
        assert!(
            enc_err
                .message
                .contains("attached databases must use the same text encoding as main database"),
            "unexpected encoding mismatch error: {}",
            enc_err.message
        );

        conn.try_close().unwrap();
    }
}

// ============================================================================
// 3. Name Resolution (`temp -> main -> attached`), VDBE vs Interpreter vs C Parity & AutoVacuum
// ============================================================================

#[test]
fn test_gate2_3_name_resolution_vdbe_vs_interpreter_c_parity_and_autovacuum() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let sandbox = Gate2Sandbox::new("resolution");

    // --- 3A. Unqualified `temp -> main -> attached` Resolution Across SELECT/INSERT/UPDATE/DELETE/DROP ---
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "ATTACH ':memory:' AS aux;
             CREATE TABLE main.t(id INTEGER PRIMARY KEY, origin TEXT);
             CREATE TABLE aux.t(id INTEGER PRIMARY KEY, origin TEXT);
             CREATE TEMP TABLE t(id INTEGER PRIMARY KEY, origin TEXT);
             INSERT INTO main.t VALUES (1, 'main_v1');
             INSERT INTO aux.t VALUES (1, 'aux_v1');
             INSERT INTO t VALUES (1, 'temp_v1');",
        )
        .unwrap();

        let q1 = conn.query("SELECT origin FROM t;", &[]).unwrap();
        assert_eq!(q1.rows[0].get_str(0), Some("temp_v1"));

        conn.execute("UPDATE t SET origin = 'temp_v2';").unwrap();
        assert_eq!(
            conn.query("SELECT origin FROM temp.t;", &[]).unwrap().rows[0].get_str(0),
            Some("temp_v2")
        );
        assert_eq!(
            conn.query("SELECT origin FROM main.t;", &[]).unwrap().rows[0].get_str(0),
            Some("main_v1")
        );
        assert_eq!(
            conn.query("SELECT origin FROM aux.t;", &[]).unwrap().rows[0].get_str(0),
            Some("aux_v1")
        );

        conn.execute("DELETE FROM t;").unwrap();
        assert_eq!(
            conn.query("SELECT count(*) FROM temp.t;", &[])
                .unwrap()
                .rows[0]
                .get_i64(0),
            Some(0)
        );
        assert_eq!(
            conn.query("SELECT count(*) FROM main.t;", &[])
                .unwrap()
                .rows[0]
                .get_i64(0),
            Some(1)
        );

        // `DROP TABLE t` drops `temp.t` first -> next unqualified `SELECT`/`DROP` hits `main.t` -> then `aux.t`!
        conn.execute("DROP TABLE t;").unwrap();
        assert_eq!(
            conn.query("SELECT origin FROM t;", &[]).unwrap().rows[0].get_str(0),
            Some("main_v1")
        );
        conn.execute("DROP TABLE t;").unwrap();
        assert_eq!(
            conn.query("SELECT origin FROM t;", &[]).unwrap().rows[0].get_str(0),
            Some("aux_v1")
        );
        conn.execute("DROP TABLE t;").unwrap();
        assert!(conn.query("SELECT origin FROM t;", &[]).is_err());
        conn.try_close().unwrap();
    }

    // --- 3B. 3-Way Differential Parity: `VdbeVm` vs `engine.rs` Interpreter vs C SQLite 3.54.1 CLI ---
    {
        let setup_sql = "
            ATTACH 'diff_aux.db' AS aux;
            CREATE TABLE main.orders(oid INTEGER PRIMARY KEY, item_id INT, qty INT);
            INSERT INTO main.orders VALUES (101, 1, 3), (102, 2, 5), (103, 1, 2);
            CREATE TABLE aux.items(id INTEGER PRIMARY KEY, code TEXT, price INT);
            CREATE INDEX aux.idx_items_code ON items(code);
            INSERT INTO aux.items VALUES (1, 'ALPHA', 10), (2, 'BETA', 25), (3, 'GAMMA', 40);
            CREATE TEMP TABLE discounts(code TEXT PRIMARY KEY, pct INT);
            INSERT INTO discounts VALUES ('ALPHA', 10), ('BETA', 20);
        ";

        let r_dir = sandbox.path().join("rust_diff");
        let c_dir = sandbox.path().join("c_diff");
        fs::create_dir_all(&r_dir).unwrap();
        fs::create_dir_all(&c_dir).unwrap();

        let r_conn = Connection::open(r_dir.join("diff_main.db")).unwrap();
        r_conn
            .execute(&setup_sql.replace("diff_aux.db", r_dir.join("diff_aux.db").to_str().unwrap()))
            .unwrap();

        // Confirm `EXPLAIN` for single-table `aux` query emits `VdbeVm` `Transaction` with `P1 == 2`
        // (`DatabaseId(2)` = `aux`) plus `OpenRead` + `OpenReadIdx`.
        let explain = r_conn
            .query(
                "EXPLAIN SELECT aux.items.* FROM aux.items WHERE aux.items.code = 'BETA';",
                &[],
            )
            .unwrap();
        assert!(
            explain
                .rows
                .iter()
                .any(|r| r.get_str(1) == Some("Transaction") && r.get_i64(2) == Some(2)),
            "expected VdbeVm Transaction with P1=2 (aux) for aux.items scan: {:?}",
            explain.rows
        );
        assert!(
            explain
                .rows
                .iter()
                .any(|r| r.get_str(1) == Some("OpenRead")),
            "expected VdbeVm OpenRead instruction: {:?}",
            explain.rows
        );

        // Verify 3-part `SELECT aux.items.*` parity between `VdbeVm` and `engine.rs` interpreter.
        let q_3part_star = "SELECT aux.items.* FROM aux.items WHERE aux.items.code = 'BETA';";
        let vdbe_3part = format_rows_pipe(&r_conn.query(q_3part_star, &[]).unwrap().rows);
        let interp_3part = format_rows_pipe(
            &r_conn
                .query(
                    &format!("WITH _force_interp(z) AS (VALUES(1)) {q_3part_star}"),
                    &[],
                )
                .unwrap()
                .rows,
        );
        assert_eq!(vdbe_3part, "2|BETA|25\n");
        assert_eq!(vdbe_3part, interp_3part);

        let queries = [
            "SELECT items.* FROM aux.items WHERE aux.items.code = 'BETA';",
            "SELECT id, code, price FROM items ORDER BY id;",
            "SELECT code, pct FROM discounts ORDER BY code;",
            "SELECT main.orders.oid, aux.items.code, main.orders.qty * aux.items.price, temp.discounts.pct \
             FROM main.orders \
             JOIN aux.items ON main.orders.item_id = aux.items.id \
             JOIN temp.discounts ON temp.discounts.code = aux.items.code \
             ORDER BY main.orders.oid;",
        ];

        for q in queries {
            let vdbe_out = format_rows_pipe(&r_conn.query(q, &[]).unwrap().rows);
            let interp_sql = format!("WITH _force_interp(z) AS (VALUES(1)) {q}");
            let interp_out = format_rows_pipe(&r_conn.query(&interp_sql, &[]).unwrap().rows);
            assert_eq!(
                vdbe_out, interp_out,
                "VdbeVm vs engine.rs interpreter mismatch on query: {q}"
            );

            let c_script = format!("{setup_sql}\n{q}");
            let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(&c_dir, "c_main.db", &c_script);
            assert!(c_ok, "C query failed ({q}): {c_err}");
            for e in fs::read_dir(&c_dir).unwrap().flatten() {
                let _ = fs::remove_file(e.path());
            }
            assert_eq!(
                vdbe_out, c_out,
                "Rust vs C SQLite 3.54.1 output mismatch on query: {q}"
            );
        }

        // `CREATE TEMP TABLE ... AS SELECT ...` across `main` + `aux`!
        r_conn
            .execute(
                "CREATE TEMP TABLE order_summary AS \
                 SELECT aux.items.code AS item_code, sum(main.orders.qty) AS total_qty \
                 FROM main.orders JOIN aux.items ON main.orders.item_id = aux.items.id \
                 GROUP BY aux.items.code ORDER BY aux.items.code;",
            )
            .unwrap();
        let sum_out = format_rows_pipe(
            &r_conn
                .query(
                    "SELECT item_code, total_qty FROM temp.order_summary ORDER BY item_code;",
                    &[],
                )
                .unwrap()
                .rows,
        );
        assert_eq!(sum_out, "ALPHA|5\nBETA|5\n");

        // --- 3C. Normalized Catalog SQL (`sqlite_schema.sql` stripped of schema prefix & re-attached under new alias) ---
        let cat_rows = r_conn
            .query(
                "SELECT type, name, sql FROM aux.sqlite_schema ORDER BY type, name;",
                &[],
            )
            .unwrap();
        assert_eq!(cat_rows.rows.len(), 2);
        assert_eq!(
            cat_rows.rows[0].get_str(2),
            Some("CREATE INDEX idx_items_code ON items(code)")
        );
        assert_eq!(
            cat_rows.rows[1].get_str(2),
            Some("CREATE TABLE items(id INTEGER PRIMARY KEY, code TEXT, price INT)")
        );

        r_conn.execute("DETACH aux;").unwrap();
        let aux_file = r_dir.join("diff_aux.db");
        r_conn
            .execute(&format!("ATTACH '{}' AS other_alias;", aux_file.display()))
            .unwrap();
        let re_q = r_conn
            .query(
                "SELECT id, price FROM other_alias.items WHERE code = 'GAMMA';",
                &[],
            )
            .unwrap();
        assert_eq!(format_rows_pipe(&re_q.rows), "3|40\n");
        r_conn.try_close().unwrap();

        let (c_ok_re, c_re_out, c_re_err) = run_c_sqlite_in_dir(
            &r_dir,
            ":memory:",
            "ATTACH 'diff_aux.db' AS c_rebound; \
             PRAGMA c_rebound.integrity_check; \
             SELECT id, code, price FROM c_rebound.items ORDER BY id;",
        );
        assert!(
            c_ok_re,
            "C failed reopening Rust-created attached DB: {c_re_err}"
        );
        assert_eq!(c_re_out, "ok\n1|ALPHA|10\n2|BETA|25\n3|GAMMA|40\n");
    }

    // --- 3D. `AutoVacuumMode::Full` Root-Page Relocation on `DROP TABLE` Inside `ATTACH`ed DB ---
    {
        let av_aux_p = sandbox.path().join("autovac_aux.db");
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(&format!(
            "ATTACH '{}' AS av_aux;
             PRAGMA av_aux.page_size = 1024;
             PRAGMA av_aux.auto_vacuum = FULL;
             CREATE TABLE av_aux.t_early(id INTEGER PRIMARY KEY, pad BLOB);
             INSERT INTO av_aux.t_early VALUES (1, zeroblob(600)), (2, zeroblob(600));
             CREATE TABLE av_aux.t_survivor(id INTEGER PRIMARY KEY, k TEXT, pad BLOB);
             CREATE INDEX av_aux.idx_survivor_k ON t_survivor(k);
             INSERT INTO av_aux.t_survivor VALUES (10, 'k10', zeroblob(500)), (20, 'k20', zeroblob(500)), (30, 'k30', zeroblob(500));",
            av_aux_p.display()
        ))
        .unwrap();

        let size_before = fs::metadata(&av_aux_p).unwrap().len();
        conn.execute("DROP TABLE av_aux.t_early;").unwrap();
        let size_after = fs::metadata(&av_aux_p).unwrap().len();
        assert!(
            size_after < size_before,
            "AutoVacuumMode::Full on attached DB must shrink file on DROP TABLE ({size_after} < {size_before})"
        );

        let ic = conn.query("PRAGMA av_aux.integrity_check;", &[]).unwrap();
        assert_eq!(ic.rows[0].get_str(0), Some("ok"));
        let surv = conn
            .query(
                "SELECT id, length(pad) FROM av_aux.t_survivor WHERE k = 'k20';",
                &[],
            )
            .unwrap();
        assert_eq!(format_rows_pipe(&surv.rows), "20|500\n");
        conn.try_close().unwrap();

        let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(
            sandbox.path(),
            av_aux_p.to_str().unwrap(),
            "PRAGMA auto_vacuum; PRAGMA integrity_check; SELECT id, k, length(pad) FROM t_survivor ORDER BY id;",
        );
        assert!(
            c_ok,
            "C integrity check on attached autovacuum DB failed: {c_err}"
        );
        assert_eq!(c_out, "1\nok\n10|k10|500\n20|k20|500\n30|k30|500\n");
    }
}

// ============================================================================
// 4. Intra-`DatabaseId` Foreign Keys & Multi-DB Connection `SAVEPOINT` Stack
// ============================================================================

#[test]
fn test_gate2_4_intra_db_foreign_keys_and_multidb_savepoint_stack() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let sandbox = Gate2Sandbox::new("fk_savepoint");

    // --- 4A. Intra-`DatabaseId` Immediate & Deferred Foreign Keys + `PRAGMA aux.foreign_key_check` ---
    {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "PRAGMA foreign_keys = ON;
             ATTACH ':memory:' AS aux;
             CREATE TABLE main.parent(id INTEGER PRIMARY KEY);
             INSERT INTO main.parent VALUES (1), (2);
             CREATE TABLE aux.parent(id INTEGER PRIMARY KEY);
             INSERT INTO aux.parent VALUES (10);
             CREATE TABLE aux.child_imm(cid INTEGER PRIMARY KEY, pid INT REFERENCES parent(id));
             CREATE TABLE aux.child_def(cid INTEGER PRIMARY KEY, pid INT REFERENCES parent(id) DEFERRABLE INITIALLY DEFERRED);",
        )
        .unwrap();

        // `pid = 1` exists in `main.parent` ONLY -> `aux.child_imm` MUST reject it immediately!
        let imm_err = conn
            .execute("INSERT INTO aux.child_imm VALUES (100, 1);")
            .unwrap_err();
        assert_eq!(imm_err.code, ErrorCode::Constraint);
        assert_eq!(
            imm_err.extended_code,
            extended::SQLITE_CONSTRAINT_FOREIGNKEY
        );

        // `pid = 10` exists in `aux.parent` -> succeeds!
        conn.execute("INSERT INTO aux.child_imm VALUES (100, 10);")
            .unwrap();

        // Deferred FK in `aux.child_def`: allowed inside `BEGIN`, fails at `COMMIT`!
        conn.execute("BEGIN; INSERT INTO aux.child_def VALUES (200, 1);")
            .unwrap();
        let def_err = conn.execute("COMMIT;").unwrap_err();
        assert_eq!(def_err.code, ErrorCode::Constraint);
        assert_eq!(
            def_err.extended_code,
            extended::SQLITE_CONSTRAINT_FOREIGNKEY
        );
        conn.execute("ROLLBACK;").unwrap();

        // Insert an orphan in `aux.child_imm` while `foreign_keys = OFF`, then verify `PRAGMA aux.foreign_key_check` vs `main.foreign_key_check`.
        conn.execute(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO aux.child_imm VALUES (999, 1);
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
        let main_fkc = conn.query("PRAGMA main.foreign_key_check;", &[]).unwrap();
        assert!(main_fkc.rows.is_empty());

        let aux_fkc = conn.query("PRAGMA aux.foreign_key_check;", &[]).unwrap();
        assert_eq!(aux_fkc.rows.len(), 1);
        assert_eq!(aux_fkc.rows[0].get_str(0), Some("child_imm"));
        assert_eq!(aux_fkc.rows[0].get_i64(1), Some(999));
        assert_eq!(aux_fkc.rows[0].get_str(2), Some("parent"));
        assert_eq!(aux_fkc.rows[0].get_i64(3), Some(0));
        conn.try_close().unwrap();
    }

    // --- 4B. Connection-Level `SAVEPOINT` Stack Across `main`, `temp`, `aux` + Dynamic `ATTACH` ---
    {
        let main_p = sandbox.path().join("sp_main.db");
        let aux_p = sandbox.path().join("sp_aux.db");
        let dyn_p = sandbox.path().join("sp_dyn.db");

        let conn = Connection::open(&main_p).unwrap();
        conn.execute(&format!(
            "PRAGMA foreign_keys = ON;
             ATTACH '{}' AS aux;
             CREATE TABLE main.m_log(id INTEGER PRIMARY KEY, tag TEXT);
             CREATE TEMP TABLE t_log(id INTEGER PRIMARY KEY, tag TEXT);
             CREATE TABLE aux.p(id INTEGER PRIMARY KEY);
             INSERT INTO aux.p VALUES (10);
             CREATE TABLE aux.c(id INTEGER PRIMARY KEY, pid INT REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED);",
            aux_p.display()
        ))
        .unwrap();

        conn.execute(
            "SAVEPOINT sp1;
             INSERT INTO main.m_log VALUES (1, 'sp1_main');
             INSERT INTO temp.t_log VALUES (1, 'sp1_temp');
             INSERT INTO aux.c VALUES (1, 10);",
        )
        .unwrap();

        // Nested `sp2`: mutate data + schemas across `main`, `temp`, `aux`, introduce a deferred FK
        // violation in `aux.c`, AND attach a new DB `dyn_aux` inside `sp2` and write to it!
        conn.execute(&format!(
            "SAVEPOINT sp2;
             INSERT INTO main.m_log VALUES (2, 'sp2_main_revert');
             CREATE TEMP TABLE sp2_only_temp(x INT);
             CREATE TABLE aux.sp2_only_aux(y INT);
             INSERT INTO aux.c VALUES (2, 9999);
             ATTACH '{}' AS dyn_aux;
             CREATE TABLE dyn_aux.d_tbl(z INT);
             INSERT INTO dyn_aux.d_tbl VALUES (42);",
            dyn_p.display()
        ))
        .unwrap();

        // Roll back `sp2`: restores data, schemas, `dyn_aux` contents, AND clears `sp2`'s deferred FK suspect!
        conn.execute("ROLLBACK TO sp2;").unwrap();

        assert!(
            conn.query("SELECT * FROM temp.sp2_only_temp;", &[])
                .is_err()
        );
        assert!(conn.query("SELECT * FROM aux.sp2_only_aux;", &[]).is_err());
        assert!(conn.query("SELECT * FROM dyn_aux.d_tbl;", &[]).is_err());

        // `RELEASE sp1` now commits `sp1` cleanly across all DBs without triggering the rolled-back FK suspect!
        conn.execute("RELEASE sp1;").unwrap();

        assert_eq!(
            format_rows_pipe(
                &conn
                    .query("SELECT id, tag FROM main.m_log;", &[])
                    .unwrap()
                    .rows
            ),
            "1|sp1_main\n"
        );
        assert_eq!(
            format_rows_pipe(
                &conn
                    .query("SELECT id, tag FROM temp.t_log;", &[])
                    .unwrap()
                    .rows
            ),
            "1|sp1_temp\n"
        );
        assert_eq!(
            format_rows_pipe(&conn.query("SELECT id, pid FROM aux.c;", &[]).unwrap().rows),
            "1|10\n"
        );
        conn.try_close().unwrap();

        // Verify durable commit in C SQLite 3.54.1!
        let (c_ok, c_out, _) = run_c_sqlite_in_dir(
            sandbox.path(),
            main_p.to_str().unwrap(),
            &format!(
                "ATTACH '{}' AS aux; SELECT id, tag FROM main.m_log; SELECT id, pid FROM aux.c;",
                aux_p.display()
            ),
        );
        assert!(c_ok);
        assert_eq!(c_out, "1|sp1_main\n1|10\n");
    }
}

// ============================================================================
// 5. Multi-DB `PRAGMA`s & `VACUUM [schema] [INTO expr]`
// ============================================================================

#[test]
fn test_gate2_5_multidb_pragmas_and_vacuum_into() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let sandbox = Gate2Sandbox::new("pragmas_vacuum");

    // --- 5A. `PRAGMA database_list` vs `PRAGMA lock_status` (`closed`, `unknown`, `exclusive`, disk lock) ---
    {
        let main_p = sandbox.path().join("ls_main.db");
        let aux_p = sandbox.path().join("ls_aux.db");
        let conn = Connection::open(&main_p).unwrap();

        // Before `temp` is opened: `database_list` omits `temp`, while `lock_status` shows `temp|closed`!
        let dl0 = format_rows_pipe(&conn.query("PRAGMA database_list;", &[]).unwrap().rows);
        assert!(dl0.starts_with("0|main|") && !dl0.contains("|temp|"));

        let ls0 = format_rows_pipe(&conn.query("PRAGMA lock_status;", &[]).unwrap().rows);
        assert_eq!(ls0, "main|unlocked\ntemp|closed\n");

        // Compare `database_list` directly with C SQLite 3.54.1 CLI on fresh disk DB (`temp` omitted until opened)!
        let c_ls_p = sandbox.path().join("c_ls.db");
        let (c_ok, c_dl_out, _) = run_c_sqlite_in_dir(
            sandbox.path(),
            c_ls_p.to_str().unwrap(),
            "SELECT seq, name FROM pragma_database_list;",
        );
        assert!(c_ok);
        assert_eq!(c_dl_out, "0|main\n");

        // `temp_store = MEMORY` -> `temp|unknown`
        conn.execute("PRAGMA temp_store = MEMORY; CREATE TEMP TABLE tm(x INT);")
            .unwrap();
        let ls_mem = format_rows_pipe(&conn.query("PRAGMA lock_status;", &[]).unwrap().rows);
        assert_eq!(ls_mem, "main|unlocked\ntemp|unknown\n");

        // Switch `temp_store = FILE` (invalidates unopened/autocommit `temp`), reopen `temp`, and attach disk `aux`.
        conn.execute(&format!(
            "PRAGMA temp_store = FILE;
             CREATE TEMP TABLE tf(x INT);
             CREATE TABLE main.m(x INT);
             ATTACH '{}' AS aux;
             CREATE TABLE aux.a(x INT);",
            aux_p.display()
        ))
        .unwrap();

        conn.execute("BEGIN; INSERT INTO main.m VALUES (1); INSERT INTO aux.a VALUES (2);")
            .unwrap();
        let ls_tx = conn.query("PRAGMA lock_status;", &[]).unwrap();
        assert_eq!(ls_tx.rows.len(), 3);
        assert_eq!(ls_tx.rows[0].get_str(0), Some("main"));
        assert!(matches!(
            ls_tx.rows[0].get_str(1),
            Some("reserved" | "exclusive")
        ));
        assert_eq!(ls_tx.rows[1].get_str(0), Some("temp"));
        assert_eq!(ls_tx.rows[1].get_str(1), Some("unlocked"));
        assert_eq!(ls_tx.rows[2].get_str(0), Some("aux"));
        assert!(matches!(
            ls_tx.rows[2].get_str(1),
            Some("reserved" | "exclusive")
        ));
        conn.execute("COMMIT;").unwrap();

        // --- 5B. Schema-Qualified vs Unqualified `PRAGMA journal_mode` ---
        let jm_aux = conn.query("PRAGMA aux.journal_mode = WAL;", &[]).unwrap();
        assert_eq!(jm_aux.rows[0].get_str(0), Some("wal"));
        assert_eq!(
            conn.query("PRAGMA main.journal_mode;", &[]).unwrap().rows[0].get_str(0),
            Some("delete")
        );

        let jm_all = conn.query("PRAGMA journal_mode = TRUNCATE;", &[]).unwrap();
        assert_eq!(jm_all.rows[0].get_str(0), Some("truncate"));
        assert_eq!(
            conn.query("PRAGMA main.journal_mode;", &[]).unwrap().rows[0].get_str(0),
            Some("truncate")
        );
        assert_eq!(
            conn.query("PRAGMA aux.journal_mode;", &[]).unwrap().rows[0].get_str(0),
            Some("truncate")
        );
        conn.try_close().unwrap();
    }

    // --- 5C. `PRAGMA aux.integrity_check` Isolates Corruption in Attached DB ---
    {
        let main_ok_p = sandbox.path().join("ic_main.db");
        let aux_bad_p = sandbox.path().join("ic_bad_aux.db");
        {
            let c = Connection::open(&aux_bad_p).unwrap();
            c.execute(
                "PRAGMA page_size = 4096;
                 CREATE TABLE t_corrupt(id INTEGER PRIMARY KEY, v TEXT);
                 INSERT INTO t_corrupt VALUES (1, 'hello'), (2, 'world');",
            )
            .unwrap();
            c.try_close().unwrap();
        }
        // Corrupt page 2 (`t_corrupt` table leaf header byte at offset 4096)
        let mut raw = fs::read(&aux_bad_p).unwrap();
        assert!(raw.len() >= 8192);
        raw[4096] = 0xFF; // invalid B-tree page flag byte
        fs::write(&aux_bad_p, &raw).unwrap();

        let conn = Connection::open(&main_ok_p).unwrap();
        conn.execute(&format!(
            "CREATE TABLE ok_tbl(id INT); INSERT INTO ok_tbl VALUES (1);
             ATTACH '{}' AS bad_aux;",
            aux_bad_p.display()
        ))
        .unwrap();

        let main_ic = conn.query("PRAGMA main.integrity_check;", &[]).unwrap();
        assert_eq!(main_ic.rows[0].get_str(0), Some("ok"));

        let aux_ic = conn.query("PRAGMA bad_aux.integrity_check;", &[]).unwrap();
        let aux_ic_text = format_rows_pipe(&aux_ic.rows);
        assert_ne!(
            aux_ic_text.trim(),
            "ok",
            "corrupted aux DB must fail integrity_check"
        );
        conn.try_close().unwrap();
    }

    // --- 5D. `PRAGMA aux.page_size` + `VACUUM aux` + `VACUUM aux INTO ...` + Unknown Schema Error ---
    {
        let main_p = sandbox.path().join("vac_main.db");
        let aux_p = sandbox.path().join("vac_aux.db");
        let into_p = sandbox.path().join("vac_aux_into.db");

        let conn = Connection::open(&main_p).unwrap();
        conn.execute(&format!(
            "ATTACH '{}' AS aux;
             CREATE TABLE aux.big(id INTEGER PRIMARY KEY, payload BLOB);
             WITH RECURSIVE s(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM s WHERE i<150)
             INSERT INTO aux.big SELECT i, zeroblob(400) FROM s;
             DELETE FROM aux.big WHERE id > 10;",
            aux_p.display()
        ))
        .unwrap();

        conn.execute("PRAGMA aux.page_size = 8192; VACUUM aux;")
            .unwrap();
        assert_eq!(
            conn.query("PRAGMA aux.page_size;", &[]).unwrap().rows[0].get_i64(0),
            Some(8192)
        );
        assert_eq!(
            conn.query("PRAGMA main.page_size;", &[]).unwrap().rows[0].get_i64(0),
            Some(4096)
        );

        let mut vac_into = conn.prepare("VACUUM aux INTO ?1;").unwrap();
        vac_into
            .bind_all(&[Value::Text(into_p.display().to_string())])
            .unwrap();
        assert!(matches!(vac_into.step().unwrap(), StepResult::Done));
        drop(vac_into);

        // Unknown schema error parity (`unknown database nosuch`).
        let unk_vac = conn.execute("VACUUM nosuch;").unwrap_err();
        assert_eq!(unk_vac.code, ErrorCode::Error);
        assert_eq!(unk_vac.message, "unknown database nosuch");

        let unk_pragma = conn.execute("PRAGMA nosuch.integrity_check;").unwrap_err();
        assert_eq!(unk_pragma.code, ErrorCode::Error);
        assert_eq!(unk_pragma.message, "unknown database nosuch");

        conn.try_close().unwrap();

        // Verify `vac_aux_into.db` in C SQLite 3.54.1!
        let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(
            sandbox.path(),
            into_p.to_str().unwrap(),
            "PRAGMA page_size; PRAGMA integrity_check; SELECT count(*), min(id), max(id) FROM big;",
        );
        assert!(c_ok, "C failed reading VACUUM aux INTO output: {c_err}");
        assert_eq!(c_out, "8192\nok\n10|1|10\n");
    }
}

// ============================================================================
// 6. End-to-End SQL `vdbeCommit` Coordinator, Commit Hook Veto & C 2PC Crash Interop
// ============================================================================

#[test]
fn test_gate2_6_sql_vdbe_commit_coordinator_commit_hook_and_c_2pc_interop() {
    let _legacy = sqliters::types::LegacyFormat3Guard::enter();
    let sandbox = Gate2Sandbox::new("2pc_coordinator");

    // --- 6A. `main` (`DELETE`) + `aux` (`DELETE`) -> `-mj` Created + Pre/Post-Unlink Crash Recovery in C & Rust ---
    {
        let work_dir = sandbox.path().join("case_a_work");
        let snap_before = sandbox.path().join("case_a_before_unlink");
        let snap_after = sandbox.path().join("case_a_after_unlink");
        fs::create_dir_all(&work_dir).unwrap();

        let main_p = work_dir.join("main.db");
        let aux_p = work_dir.join("aux.db");

        let os_vfs: Arc<dyn Vfs> = Arc::new(OsVfs::new());
        let obs_vfs = Arc::new(Gate2ObservingVfs::new(os_vfs));

        let conn = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(&main_p),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn.execute(&format!(
            "PRAGMA synchronous = FULL;
             PRAGMA journal_mode = DELETE;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val TEXT);
             INSERT INTO items VALUES (1, 'm_base');
             ATTACH '{}' AS aux;
             PRAGMA aux.synchronous = FULL;
             PRAGMA aux.journal_mode = DELETE;
             CREATE TABLE aux.items(id INTEGER PRIMARY KEY, val TEXT);
             INSERT INTO aux.items VALUES (1, 'a_base');",
            aux_p.display()
        ))
        .unwrap();

        let wd_before = work_dir.clone();
        let sb = snap_before.clone();
        obs_vfs.hooks.lock().unwrap().before_delete = Some(Box::new(move |path, sync_dir| {
            if let Some(s) = path.to_str()
                && is_super_journal_name(s)
            {
                assert!(
                    sync_dir,
                    "SQL vdbeCommit super-journal unlink must pass sync_dir=true"
                );
                copy_dir_flat(&wd_before, &sb);
            }
        }));

        let wd_after = work_dir.clone();
        let sa = snap_after.clone();
        obs_vfs.hooks.lock().unwrap().after_delete = Some(Box::new(move |path, _sync_dir| {
            if let Some(s) = path.to_str()
                && is_super_journal_name(s)
            {
                copy_dir_flat(&wd_after, &sa);
            }
        }));

        obs_vfs.clear_events();
        conn.execute(
            "BEGIN;
             UPDATE main.items SET val = 'm_committed' WHERE id = 1;
             INSERT INTO main.items VALUES (2, 'm_row2');
             UPDATE aux.items SET val = 'a_committed' WHERE id = 1;
             INSERT INTO aux.items VALUES (2, 'a_row2');
             COMMIT;",
        )
        .unwrap();

        assert!(
            obs_vfs.saw_super_journal_open(),
            "2 rollback disk DBs in SQL COMMIT must create `-mj` super-journal"
        );
        assert_eq!(
            list_super_journals(&snap_before).len(),
            1,
            "pre-unlink snapshot must contain `-mj` super-journal"
        );
        assert!(
            list_super_journals(&snap_after).is_empty(),
            "post-unlink snapshot must have unlinked `-mj`"
        );
        assert!(snap_after.join("main.db-journal").is_file());
        assert!(snap_after.join("aux.db-journal").is_file());
        conn.try_close().unwrap();

        // 6A.1: Crash BEFORE `-mj` unlink (restored in-place at `work_dir` so canonical `-mj` and
        //       `-journal` trailer paths match!) -> BOTH C SQLite 3.54.1 AND `sqliters` roll back BOTH DBs!
        {
            copy_dir_flat(&snap_before, &work_dir);
            assert_eq!(list_super_journals(&work_dir).len(), 1);
            let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(
                &work_dir,
                "main.db",
                "ATTACH 'aux.db' AS aux; \
                 PRAGMA main.integrity_check; PRAGMA aux.integrity_check; \
                 SELECT 'M', id, val FROM main.items ORDER BY id; \
                 SELECT 'A', id, val FROM aux.items ORDER BY id;",
            );
            assert!(c_ok, "C pre-unlink recovery failed: {c_err}");
            assert_eq!(c_out, "ok\nok\nM|1|m_base\nA|1|a_base\n");
            assert!(list_super_journals(&work_dir).is_empty());

            copy_dir_flat(&snap_before, &work_dir);
            assert_eq!(list_super_journals(&work_dir).len(), 1);
            let rc = Connection::open(work_dir.join("main.db")).unwrap();
            rc.execute(&format!(
                "ATTACH '{}' AS aux;",
                work_dir.join("aux.db").display()
            ))
            .unwrap();
            let r_out = format_rows_pipe(
                &rc.query(
                    "SELECT 'M', id, val FROM main.items \
                     UNION ALL \
                     SELECT 'A', id, val FROM aux.items;",
                    &[],
                )
                .unwrap()
                .rows,
            );
            assert_eq!(r_out, "M|1|m_base\nA|1|a_base\n");
            rc.try_close().unwrap();
            assert!(list_super_journals(&work_dir).is_empty());
        }

        // 6A.2: Crash AFTER `-mj` unlink (with hot `-journal` files still on disk!) ->
        //       BOTH C SQLite 3.54.1 AND `sqliters` preserve committed rows in BOTH DBs!
        {
            copy_dir_flat(&snap_after, &work_dir);
            assert!(list_super_journals(&work_dir).is_empty());
            assert!(work_dir.join("main.db-journal").is_file());
            assert!(work_dir.join("aux.db-journal").is_file());
            let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(
                &work_dir,
                "main.db",
                "ATTACH 'aux.db' AS aux; \
                 PRAGMA main.integrity_check; PRAGMA aux.integrity_check; \
                 SELECT 'M', id, val FROM main.items ORDER BY id; \
                 SELECT 'A', id, val FROM aux.items ORDER BY id;",
            );
            assert!(c_ok, "C post-unlink recovery failed: {c_err}");
            assert_eq!(
                c_out,
                "ok\nok\nM|1|m_committed\nM|2|m_row2\nA|1|a_committed\nA|2|a_row2\n"
            );

            copy_dir_flat(&snap_after, &work_dir);
            assert!(list_super_journals(&work_dir).is_empty());
            assert!(work_dir.join("main.db-journal").is_file());
            assert!(work_dir.join("aux.db-journal").is_file());
            let rc = Connection::open(work_dir.join("main.db")).unwrap();
            rc.execute(&format!(
                "ATTACH '{}' AS aux;",
                work_dir.join("aux.db").display()
            ))
            .unwrap();
            let r_out = format_rows_pipe(
                &rc.query(
                    "SELECT 'M', id, val FROM main.items \
                     UNION ALL \
                     SELECT 'A', id, val FROM aux.items;",
                    &[],
                )
                .unwrap()
                .rows,
            );
            assert_eq!(
                r_out,
                "M|1|m_committed\nM|2|m_row2\nA|1|a_committed\nA|2|a_row2\n"
            );
            rc.try_close().unwrap();
        }
    }

    // --- 6B. `main` (`DELETE`) + `temp` (`nTrans == 1`) -> NO `-mj` Created! ---
    {
        let main_p = sandbox.path().join("case_b_main.db");
        let obs_vfs = Arc::new(Gate2ObservingVfs::new(Arc::new(OsVfs::new())));
        let conn = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(&main_p),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE main.t(x INT);
             CREATE TEMP TABLE t_tmp(y INT);",
        )
        .unwrap();

        obs_vfs.clear_events();
        conn.execute(
            "BEGIN;
             INSERT INTO main.t VALUES (1);
             INSERT INTO temp.t_tmp VALUES (2);
             COMMIT;",
        )
        .unwrap();
        assert!(
            !obs_vfs.saw_super_journal_open(),
            "`main` + `temp` has nTrans == 1 and must NOT allocate `-mj`"
        );
        conn.try_close().unwrap();
    }

    // --- 6C. `main` (`DELETE`) + `aux1` (`DELETE`) + `aux2` (`WAL`) -> `-mj` Created + WAL Commits! ---
    {
        let main_p = sandbox.path().join("case_c_main.db");
        let aux1_p = sandbox.path().join("case_c_aux1.db");
        let aux2_p = sandbox.path().join("case_c_aux2.db");
        let obs_vfs = Arc::new(Gate2ObservingVfs::new(Arc::new(OsVfs::new())));
        let conn = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(&main_p),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn.execute(&format!(
            "CREATE TABLE main.t(v TEXT);
             ATTACH '{}' AS aux1; CREATE TABLE aux1.t(v TEXT);
             ATTACH '{}' AS aux2; PRAGMA aux2.journal_mode = WAL; CREATE TABLE aux2.t(v TEXT);",
            aux1_p.display(),
            aux2_p.display()
        ))
        .unwrap();

        obs_vfs.clear_events();
        conn.execute(
            "BEGIN;
             INSERT INTO main.t VALUES ('m_ok');
             INSERT INTO aux1.t VALUES ('a1_ok');
             INSERT INTO aux2.t VALUES ('a2_wal_ok');
             COMMIT;",
        )
        .unwrap();
        assert!(
            obs_vfs.saw_super_journal_open(),
            "`main` (DELETE) + `aux1` (DELETE) + `aux2` (WAL) has nTrans == 2 and MUST create `-mj`"
        );
        conn.try_close().unwrap();

        let (c_ok, c_out, c_err) = run_c_sqlite_in_dir(
            sandbox.path(),
            main_p.to_str().unwrap(),
            &format!(
                "ATTACH '{}' AS aux1; ATTACH '{}' AS aux2; \
                 SELECT v FROM main.t; SELECT v FROM aux1.t; SELECT v FROM aux2.t;",
                aux1_p.display(),
                aux2_p.display()
            ),
        );
        assert!(c_ok, "C read after mixed rollback+WAL 2PC failed: {c_err}");
        assert_eq!(c_out, "m_ok\na1_ok\na2_wal_ok\n");
    }

    // --- 6D. `:memory:` `main` + Two Disk `ATTACH` DBs, AND `PRAGMA synchronous = OFF` -> NO `-mj`! ---
    {
        // D1: `:memory:` `main` (`db_path == None`) + two disk DBs
        let d1_p = sandbox.path().join("case_d_mem_aux1.db");
        let d2_p = sandbox.path().join("case_d_mem_aux2.db");
        let obs_vfs = Arc::new(Gate2ObservingVfs::new(Arc::new(OsVfs::new())));
        let conn = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(Path::new(":memory:")),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn.execute(&format!(
            "ATTACH '{}' AS d1; CREATE TABLE d1.t(x INT);
             ATTACH '{}' AS d2; CREATE TABLE d2.t(x INT);",
            d1_p.display(),
            d2_p.display()
        ))
        .unwrap();

        obs_vfs.clear_events();
        conn.execute(
            "BEGIN;
             INSERT INTO d1.t VALUES (11);
             INSERT INTO d2.t VALUES (22);
             COMMIT;",
        )
        .unwrap();
        assert!(
            !obs_vfs.saw_super_journal_open(),
            "`:memory:` main has empty main filename -> simple commit without `-mj`"
        );
        conn.try_close().unwrap();

        // D2: Disk `main` + disk `aux` with `synchronous = OFF` (`nTrans == 0`) -> NO `-mj`!
        let s_main_p = sandbox.path().join("case_d_syncoff_main.db");
        let s_aux_p = sandbox.path().join("case_d_syncoff_aux.db");
        let conn2 = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(&s_main_p),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn2
            .execute(&format!(
                "PRAGMA main.synchronous = OFF;
                 CREATE TABLE main.t(x INT);
                 ATTACH '{}' AS aux;
                 PRAGMA aux.synchronous = OFF;
                 CREATE TABLE aux.t(x INT);",
                s_aux_p.display()
            ))
            .unwrap();
        obs_vfs.clear_events();
        conn2
            .execute("BEGIN; INSERT INTO main.t VALUES (1); INSERT INTO aux.t VALUES (2); COMMIT;")
            .unwrap();
        assert!(
            !obs_vfs.saw_super_journal_open(),
            "`synchronous = OFF` on all rollback DBs yields nTrans == 0 -> NO `-mj`"
        );
        conn2.try_close().unwrap();
    }

    // --- 6E. `Connection::commit_hook` Veto (`SQLITE_CONSTRAINT_COMMITHOOK`) Rolls Back All DBs ---
    {
        let main_p = sandbox.path().join("case_e_main.db");
        let aux_p = sandbox.path().join("case_e_aux.db");
        let obs_vfs = Arc::new(Gate2ObservingVfs::new(Arc::new(OsVfs::new())));
        let conn = Connection::open_with_vfs_and_options(
            obs_vfs.clone(),
            Some(&main_p),
            open_rw_create_flags(),
            ConnectionOpenOptions::default(),
        )
        .unwrap();
        conn.execute(&format!(
            "CREATE TABLE main.t(x INT); INSERT INTO main.t VALUES (1);
             CREATE TEMP TABLE t_tmp(x INT); INSERT INTO temp.t_tmp VALUES (10);
             ATTACH '{}' AS aux;
             CREATE TABLE aux.t(x INT); INSERT INTO aux.t VALUES (100);",
            aux_p.display()
        ))
        .unwrap();

        let hook_calls = Arc::new(AtomicU64::new(0));
        let hc_clone = Arc::clone(&hook_calls);
        conn.commit_hook(Some(Box::new(move || {
            hc_clone.fetch_add(1, Ordering::SeqCst);
            true // veto!
        })))
        .unwrap();

        obs_vfs.clear_events();
        conn.execute(
            "BEGIN;
             INSERT INTO main.t VALUES (2);
             INSERT INTO temp.t_tmp VALUES (20);
             INSERT INTO aux.t VALUES (200);",
        )
        .unwrap();
        let veto_err = conn.execute("COMMIT;").unwrap_err();
        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
        assert_eq!(veto_err.code, ErrorCode::Constraint);
        assert_eq!(
            veto_err.extended_code,
            extended::SQLITE_CONSTRAINT_COMMITHOOK
        );
        assert!(
            !obs_vfs.saw_super_journal_open(),
            "commit_hook veto fires in Step 1 BEFORE super-journal allocation"
        );
        assert!(
            conn.is_autocommit(),
            "commit_hook veto must roll back to autocommit"
        );

        // All 3 databases (`main`, `temp`, `aux`) must have rolled back cleanly!
        assert_eq!(
            format_rows_pipe(&conn.query("SELECT x FROM main.t;", &[]).unwrap().rows),
            "1\n"
        );
        assert_eq!(
            format_rows_pipe(&conn.query("SELECT x FROM temp.t_tmp;", &[]).unwrap().rows),
            "10\n"
        );
        assert_eq!(
            format_rows_pipe(&conn.query("SELECT x FROM aux.t;", &[]).unwrap().rows),
            "100\n"
        );

        // Clearing `commit_hook(None)` allows subsequent multi-DB commits to succeed!
        conn.commit_hook(None).unwrap();
        conn.execute(
            "BEGIN;
             INSERT INTO main.t VALUES (3);
             INSERT INTO temp.t_tmp VALUES (30);
             INSERT INTO aux.t VALUES (300);
             COMMIT;",
        )
        .unwrap();
        assert!(obs_vfs.saw_super_journal_open());
        assert_eq!(
            format_rows_pipe(
                &conn
                    .query("SELECT x FROM main.t ORDER BY x;", &[])
                    .unwrap()
                    .rows
            ),
            "1\n3\n"
        );
        assert_eq!(
            format_rows_pipe(
                &conn
                    .query("SELECT x FROM aux.t ORDER BY x;", &[])
                    .unwrap()
                    .rows
            ),
            "100\n300\n"
        );
        conn.try_close().unwrap();
    }
}
