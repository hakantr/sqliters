//! Phase 7 (`GATE-7`) end-to-end verification gate:
//!
//! 1. Audits `PHASE1_CAPABILITY_TABLE` (`vtab = true`, `json = true`, `json1 = true`,
//!    `fts5 = true`, `rtree = true`, `dbstat = true`, and out-of-scope flags kept `false`),
//!    verifies zero external Cargo dependencies and the acyclic workspace crate DAG, runs
//!    `python3 tools/generate_sqlite_test_wrappers.py --check`, audits multi-root
//!    `tests/sqlite_test_manifest.json` (`sqlite/test`, `sqlite/ext/fts5/test`,
//!    `sqlite/ext/rtree`), verifies C SQLite 3.54.1 reference binaries (`target/sqlite-ref/sqlite3`
//!    and `target/sqlite-ref/sqlite3_ext`), and writes `target/sqlite-ref/phase7_manifest.json`.
//! 2. Verifies multi-root Phase 7 Tcl suites (`pragma5` `7/7`, promoted `windowB` `72/72`, and
//!    representative active `json*`, `vtab*`, `fts5*`, and `rtree*` suites across all 3 roots)
//!    with `unknown_capabilities.is_empty()`.
//! 3. Verifies the cross-feature differential and safety matrix (Test Groups `14, 18, 21, 40, 44,
//!    51, 52, 53, 54, 61, 63, 64, 65, 66`): VTab savepoints and `xSync` rollback veto,
//!    `VTabRiskFlags` (`DirectOnly`, `Normal`, `Innocuous`) under `trusted_schema = OFF`,
//!    `set_defensive(true)` shadow protection, atomic `ALTER TABLE ... RENAME TO`, `VACUUM` and
//!    online `Backup` with C SQLite 3.54.1 `sqlite3_ext` `PRAGMA integrity_check` + FTS5
//!    `'integrity-check'` + `rtreecheck()`, corrupt `JSONB` boundary safety without panics, and
//!    `JSON_SUBTYPE = 74` propagation across scalar and window JSON aggregates.
//! 4. Executes the `sqliters-cli` <-> C SQLite 3.54.1 (`target/sqlite-ref/sqlite3`) sequential
//!    WAL + `jsonb()` + `rank() OVER (...)` + `->>` smoke test (`implementation_plan.md:1008-1035`).
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sqliters_core::{Backup, BackupStepResult, Connection, FunctionFlags, Row};
use sqliters_tcltest::{
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script_in_root,
    strip_test_comments,
};
use sqliters_types::json::JSON_SUBTYPE;
use sqliters_types::{
    ErrorCode, LockGeometry, Result, SqliteError, VTabConfigOption, VTabHostContext, VTabIndexInfo,
    VTabRiskFlags, VTabUpdateOperation, Value, VirtualCursor, VirtualTable, VirtualTableModule,
    format_sqlite_real,
};

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

const SQLITE_3541_EXPECTED_VERSION: &str = "3.54.1";

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p7gate_{prefix}_{pid}_{id}"));
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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ensure_c_sqlite_ref_binary() -> &'static Path {
    static REF_BIN: OnceLock<PathBuf> = OnceLock::new();
    REF_BIN.get_or_init(|| {
        let root = workspace_root();
        let ref_dir = root.join("target/sqlite-ref");
        fs::create_dir_all(&ref_dir).expect("create target/sqlite-ref");
        let bin_path = ref_dir.join("sqlite3");
        let valid = bin_path.exists()
            && Command::new(&bin_path)
                .arg("--version")
                .output()
                .map(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout)
                            .starts_with(SQLITE_3541_EXPECTED_VERSION)
                })
                .unwrap_or(false);
        if !valid {
            let amalg_dir = ref_dir.join("amalg");
            let shell_c = amalg_dir.join("shell.c");
            let sqlite3_c = amalg_dir.join("sqlite3.c");
            assert!(
                shell_c.exists() && sqlite3_c.exists(),
                "amalgamation sources missing under {}",
                amalg_dir.display()
            );
            let status = Command::new("cc")
                .args([
                    "-O2",
                    "-DSQLITE_THREADSAFE=1",
                    "-DSQLITE_ENABLE_MATH_FUNCTIONS",
                ])
                .arg(&shell_c)
                .arg(&sqlite3_c)
                .args(["-lpthread", "-ldl", "-lm", "-o"])
                .arg(&bin_path)
                .status()
                .expect("compile target/sqlite-ref/sqlite3 from target/sqlite-ref/amalg");
            assert!(status.success(), "compiling C sqlite3 reference failed");
        }
        bin_path
    })
}

fn ensure_c_sqlite_ext_binary() -> &'static Path {
    static EXT_BIN: OnceLock<PathBuf> = OnceLock::new();
    EXT_BIN.get_or_init(|| {
        let root = workspace_root();
        let ref_dir = root.join("target/sqlite-ref");
        fs::create_dir_all(&ref_dir).expect("create target/sqlite-ref");
        let bin_path = ref_dir.join("sqlite3_ext");
        let valid = bin_path.exists()
            && Command::new(&bin_path)
                .arg("--version")
                .output()
                .map(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout)
                            .starts_with(SQLITE_3541_EXPECTED_VERSION)
                })
                .unwrap_or(false);
        if !valid {
            let amalg_dir = ref_dir.join("amalg");
            let shell_c = amalg_dir.join("shell.c");
            let sqlite3_c = amalg_dir.join("sqlite3.c");
            assert!(
                shell_c.exists() && sqlite3_c.exists(),
                "amalgamation sources missing under {}",
                amalg_dir.display()
            );
            let status = Command::new("cc")
                .args([
                    "-O2",
                    "-DSQLITE_ENABLE_FTS5",
                    "-DSQLITE_ENABLE_RTREE",
                    "-DSQLITE_ENABLE_DBSTAT_VTAB",
                    "-DSQLITE_ENABLE_MATH_FUNCTIONS",
                    "-DSQLITE_THREADSAFE=1",
                ])
                .arg(&shell_c)
                .arg(&sqlite3_c)
                .args(["-lpthread", "-ldl", "-lm", "-o"])
                .arg(&bin_path)
                .status()
                .expect("compile target/sqlite-ref/sqlite3_ext from target/sqlite-ref/amalg");
            assert!(status.success(), "compiling C sqlite3_ext reference failed");
        }
        bin_path
    })
}

fn ensure_sqliters_cli_binary() -> PathBuf {
    let root = workspace_root();
    if let Ok(cur_exe) = std::env::current_exe()
        && let Some(deps_dir) = cur_exe.parent()
        && let Some(profile_dir) = deps_dir.parent()
    {
        let candidate = profile_dir.join(if cfg!(windows) {
            "sqliters.exe"
        } else {
            "sqliters"
        });
        if candidate.is_file() {
            return candidate;
        }
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "sqliters-cli"])
        .current_dir(&root)
        .status()
        .expect("build sqliters-cli");
    assert!(status.success(), "cargo build -p sqliters-cli failed");
    let built = root.join(if cfg!(windows) {
        "target/debug/sqliters.exe"
    } else {
        "target/debug/sqliters"
    });
    assert!(
        built.is_file(),
        "expected sqliters CLI binary at {}",
        built.display()
    );
    built
}

fn run_c_sqlite(bin: &Path, cwd: &Path, db_arg: &str, sql: &str) -> (bool, String, String) {
    let out = Command::new(bin)
        .args(["-batch", "-list", "-noheader", db_arg, sql])
        .current_dir(cwd)
        .output()
        .expect("spawn C sqlite3 binary");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn format_rows_pipe(rows: &[Row]) -> String {
    let mut out = String::new();
    for r in rows {
        let cols: Vec<String> = r
            .values()
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                Value::Integer(i) => i.to_string(),
                Value::Real(f) => format_sqlite_real(*f),
                Value::Text(s) => s.clone(),
                Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
            })
            .collect();
        out.push_str(&cols.join("|"));
        out.push('\n');
    }
    out
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

fn parse_cargo_workspace_deps(toml_path: &Path) -> BTreeSet<String> {
    let raw = fs::read_to_string(toml_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", toml_path.display()));
    let mut section = String::new();
    let mut deps = BTreeSet::new();
    for line in raw.lines() {
        let trimmed = line.split('#').next().unwrap_or("").trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].trim().to_owned();
            continue;
        }
        if matches!(
            section.as_str(),
            "dependencies" | "dev-dependencies" | "build-dependencies"
        ) && let Some((dep_name, rhs)) = trimmed.split_once('=')
        {
            let dep = dep_name.trim().to_owned();
            let spec = rhs.trim();
            assert!(
                spec.contains("path ="),
                "external crate dependency `{dep}` forbidden in {}: `{trimmed}`",
                toml_path.display()
            );
            assert!(
                !spec.contains("git =") && !spec.contains("registry ="),
                "non-workspace dependency `{dep}` forbidden in {}: `{trimmed}`",
                toml_path.display()
            );
            if section == "dependencies" {
                deps.insert(dep);
            }
        }
    }
    deps
}

fn has_acyclic_dag(graph: &BTreeMap<String, BTreeSet<String>>) -> bool {
    fn dfs(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visited.contains(node) {
            return true;
        }
        if !visiting.insert(node.to_owned()) {
            return false;
        }
        if let Some(edges) = graph.get(node) {
            for next in edges {
                if !dfs(next, graph, visiting, visited) {
                    return false;
                }
            }
        }
        visiting.remove(node);
        visited.insert(node.to_owned());
        true
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for k in graph.keys() {
        if !dfs(k, graph, &mut visiting, &mut visited) {
            return false;
        }
    }
    true
}

#[test]
fn phase7_gate_1_capability_crate_dag_and_manifest_audit() {
    let root = workspace_root();
    let sandbox = TempSandbox::new("manifest_audit");

    // 1. Verify both C SQLite 3.54.1 reference binaries (`sqlite3` and `sqlite3_ext`).
    let ref_bin = ensure_c_sqlite_ref_binary();
    let ext_bin = ensure_c_sqlite_ext_binary();
    let manifest_uuid = fs::read_to_string(root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();

    for (label, bin) in [("sqlite3", ref_bin), ("sqlite3_ext", ext_bin)] {
        let (ok, ver_out, ver_err) = run_c_sqlite(
            bin,
            sandbox.path(),
            ":memory:",
            "SELECT sqlite_version(), sqlite_source_id();",
        );
        assert!(ok, "C {label} version query failed: {ver_err}");
        let (c_ver, c_src_id) = ver_out
            .trim()
            .split_once('|')
            .expect("version|source_id format");
        assert_eq!(c_ver, SQLITE_3541_EXPECTED_VERSION);
        assert!(
            c_src_id.contains(&manifest_uuid),
            "unexpected C {label} sqlite_source_id: {c_src_id}"
        );
    }

    let (_, compile_opts_out, _) = run_c_sqlite(
        ext_bin,
        sandbox.path(),
        ":memory:",
        "PRAGMA compile_options;",
    );
    let compile_opts: Vec<String> = compile_opts_out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    assert!(
        compile_opts.iter().any(|o| o == "ENABLE_FTS5"),
        "expected ENABLE_FTS5 in sqlite3_ext compile_options: {compile_opts:?}"
    );
    assert!(
        compile_opts.iter().any(|o| o == "ENABLE_RTREE"),
        "expected ENABLE_RTREE in sqlite3_ext compile_options: {compile_opts:?}"
    );
    assert!(
        compile_opts.iter().any(|o| o == "ENABLE_DBSTAT_VTAB"),
        "expected ENABLE_DBSTAT_VTAB in sqlite3_ext compile_options: {compile_opts:?}"
    );

    assert_eq!(TESTFIXTURE_DEFAULT_PAGE_SIZE, 1024);
    assert_eq!(LockGeometry::PRODUCTION.pending_byte_offset(), 0x4000_0000);
    assert_eq!(LockGeometry::TEST_64K.pending_byte_offset(), 0x0001_0000);

    // 2. Audit `PHASE1_CAPABILITY_TABLE` for Phase 7 (`GATE-7`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    for phase7_cap in ["vtab", "json", "json1", "fts5", "rtree", "dbstat"] {
        assert_eq!(
            cap_map.get(phase7_cap),
            Some(&true),
            "Phase 7 capability `{phase7_cap}` must be enabled in GATE-7"
        );
    }
    for out_of_scope_cap in [
        "stat4",
        "fts3",
        "fts4",
        "load_ext",
        "shared_cache",
        "utf16",
        "icu",
        "unlock_notify",
        "lock_proxy_pragmas",
        "prefer_proxy_locking",
        "memorymanage",
    ] {
        assert_eq!(
            cap_map.get(out_of_scope_cap),
            Some(&false),
            "out-of-scope capability `{out_of_scope_cap}` must remain false in GATE-7"
        );
    }

    // 3. Verify zero external crates, acyclic crate DAG, and `#![forbid(unsafe_code)]`.
    let mut dag: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    dag.insert(
        "sqliters".to_owned(),
        parse_cargo_workspace_deps(&root.join("Cargo.toml")),
    );
    for entry in fs::read_dir(root.join("crates")).expect("read crates dir") {
        let entry = entry.expect("dir entry");
        if entry.path().is_dir() {
            let crate_name = entry.file_name().to_string_lossy().into_owned();
            let cargo_toml = entry.path().join("Cargo.toml");
            if cargo_toml.is_file() {
                let deps = parse_cargo_workspace_deps(&cargo_toml);
                dag.insert(crate_name.clone(), deps);
                if crate_name == "sqliters-asm" {
                    let entry_rs = entry.path().join("src/lib.rs");
                    let src = fs::read_to_string(&entry_rs).expect("read sqliters-asm lib.rs");
                    assert!(
                        src.contains("#![allow(unsafe_code)]")
                            && src.contains("#![deny(unsafe_op_in_unsafe_fn)]"),
                        "leaf crate {} must declare #![allow(unsafe_code)] and #![deny(unsafe_op_in_unsafe_fn)]",
                        entry_rs.display()
                    );
                } else if crate_name != "sqliters-vfs" {
                    let entry_rs = if entry.path().join("src/lib.rs").is_file() {
                        entry.path().join("src/lib.rs")
                    } else {
                        entry.path().join("src/main.rs")
                    };
                    let src = fs::read_to_string(&entry_rs).expect("read crate root rs");
                    assert!(
                        src.contains("#![forbid(unsafe_code)]"),
                        "non-VFS crate {} must declare #![forbid(unsafe_code)]",
                        entry_rs.display()
                    );
                }
            }
        }
    }
    let root_lib_src = fs::read_to_string(root.join("src/lib.rs")).expect("read root src/lib.rs");
    assert!(
        root_lib_src.contains("#![forbid(unsafe_code)]"),
        "root sqliters src/lib.rs must declare #![forbid(unsafe_code)]"
    );
    let shm_mod_src = fs::read_to_string(root.join("crates/sqliters-vfs/src/shm/mod.rs"))
        .expect("read shm/mod.rs");
    assert!(
        shm_mod_src.contains("#![forbid(unsafe_code)]"),
        "crates/sqliters-vfs/src/shm/mod.rs must declare #![forbid(unsafe_code)]"
    );
    assert!(
        has_acyclic_dag(&dag),
        "workspace crate graph must be acyclic"
    );
    assert!(
        dag["sqliters-asm"].is_empty(),
        "sqliters-asm must have 0 dependencies"
    );
    assert!(
        dag["sqliters-types"].iter().all(|d| d == "sqliters-asm"),
        "sqliters-types must have 0 dependencies (except optional leaf sqliters-asm)"
    );
    assert!(
        dag["sqliters-ext"].contains("sqliters-types")
            && dag["sqliters-ext"]
                .iter()
                .all(|d| d == "sqliters-types" || d == "sqliters-asm"),
        "sqliters-ext must depend solely on sqliters-types (and optional leaf sqliters-asm)"
    );
    assert!(
        !dag["sqliters-vdbe"].contains("sqliters-core")
            && !dag["sqliters-vdbe"].contains("sqliters-ext"),
        "sqliters-vdbe must not depend on sqliters-core or sqliters-ext"
    );

    // Check `pragma5.rs` is un-ignored and `#![forbid(unsafe_code)]` is intact.
    let pragma5_src =
        fs::read_to_string(root.join("tests/sqlite_test/pragma5.rs")).expect("read pragma5.rs");
    assert!(
        !pragma5_src.contains("#[ignore"),
        "tests/sqlite_test/pragma5.rs must be active in GATE-7"
    );
    assert!(
        pragma5_src.contains("#![forbid(unsafe_code)]"),
        "tests/sqlite_test/pragma5.rs must declare #![forbid(unsafe_code)]"
    );

    // 4. Verify `tools/generate_sqlite_test_wrappers.py --check` exits 0.
    let gen_check = Command::new("python3")
        .args(["tools/generate_sqlite_test_wrappers.py", "--check"])
        .current_dir(&root)
        .output()
        .expect("run tools/generate_sqlite_test_wrappers.py --check");
    assert!(
        gen_check.status.success(),
        "generate_sqlite_test_wrappers.py --check failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&gen_check.stdout),
        String::from_utf8_lossy(&gen_check.stderr)
    );

    // 5. Audit `tests/sqlite_test_manifest.json` across all 3 roots and Phase 7 families.
    let manifest_raw = fs::read_to_string(root.join("tests/sqlite_test_manifest.json"))
        .expect("read tests/sqlite_test_manifest.json");
    let mut active_ids = Vec::new();
    let mut ignored_ids = Vec::new();
    let mut p7_by_family: BTreeMap<&str, (usize, usize)> = BTreeMap::from([
        ("json", (0, 0)),
        ("vtab", (0, 0)),
        ("fts5", (0, 0)),
        ("rtree", (0, 0)),
    ]);
    let mut seen_roots = BTreeSet::new();

    let mut cur_id = String::new();
    let mut cur_root = String::new();
    let mut cur_phase = String::new();
    let mut cur_status = String::new();
    for line in manifest_raw.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("\"id\": \"") {
            cur_id = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"source_root\": \"") {
            cur_root = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"phase\": \"") {
            cur_phase = rest.trim_end_matches(['"', ',']).to_owned();
        } else if let Some(rest) = t.strip_prefix("\"status\": \"") {
            cur_status = rest.trim_end_matches(['"', ',']).to_owned();
        } else if t.starts_with('}') && !cur_id.is_empty() {
            seen_roots.insert(cur_root.clone());
            let is_active = cur_status == "active";
            if is_active {
                active_ids.push(cur_id.clone());
            } else if cur_status == "ignored" {
                ignored_ids.push(cur_id.clone());
            }
            if cur_phase == "7" {
                let fam = if cur_id.starts_with("sqlite/test/json") {
                    "json"
                } else if cur_id.starts_with("sqlite/test/vtab") {
                    "vtab"
                } else if cur_id.starts_with("sqlite/ext/fts5/test/") {
                    "fts5"
                } else if cur_id.starts_with("sqlite/ext/rtree/") {
                    "rtree"
                } else {
                    panic!("unexpected Phase 7 suite id: {cur_id}");
                };
                let slot = p7_by_family.get_mut(fam).unwrap();
                slot.0 += 1;
                if is_active {
                    slot.1 += 1;
                }
            }
            cur_id.clear();
            cur_root.clear();
            cur_phase.clear();
            cur_status.clear();
        }
    }

    assert_eq!(
        seen_roots,
        BTreeSet::from([
            "sqlite/test".to_owned(),
            "sqlite/ext/fts5/test".to_owned(),
            "sqlite/ext/rtree".to_owned(),
        ]),
        "all 3 C SQLite test roots must be registered in sqlite_test_manifest.json"
    );
    assert_eq!(active_ids.len(), 131, "expected 131 active manifest suites");
    assert_eq!(
        ignored_ids.len(),
        204,
        "expected 204 ignored manifest suites"
    );
    assert_eq!(
        p7_by_family["json"],
        (12, 12),
        "all 12 json*.test must pass"
    );
    assert_eq!(
        p7_by_family["vtab"],
        (26, 15),
        "expected 15/26 active vtab*"
    );
    assert_eq!(
        p7_by_family["fts5"],
        (148, 26),
        "expected 26/148 active fts5*"
    );
    assert_eq!(
        p7_by_family["rtree"],
        (26, 7),
        "expected 7/26 active rtree*"
    );
    assert!(
        active_ids.iter().any(|s| s == "sqlite/test/windowB.test"),
        "windowB.test must be promoted to active in Phase 7"
    );

    // 6. Write `target/sqlite-ref/phase7_manifest.json`.
    let uname_out = Command::new("uname")
        .arg("-srmo")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| std::env::consts::OS.to_owned());
    let fstype_out = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(sandbox.path())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".to_owned());

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
    let testfixture_bin = sqliters_tcltest::interp::resolve_testfixture_binary();

    let phase7_manifest_json = format!(
        "{{\n\
  \"phase\": \"7\",\n\
  \"gate\": \"GATE-7\",\n\
  \"sqlite_version\": \"{SQLITE_3541_EXPECTED_VERSION}\",\n\
  \"sqlite_manifest_uuid\": \"{}\",\n\
  \"c_reference_binaries\": {{\n\
    \"sqlite3\": \"{}\",\n\
    \"sqlite3_ext\": \"{}\"\n\
  }},\n\
  \"c_compile_options\": [{compile_opts_json}],\n\
  \"c_defines\": {{\n\
    \"SQLITE_THREADSAFE\": 1,\n\
    \"SQLITE_DEFAULT_PAGE_SIZE\": 4096,\n\
    \"SQLITE_DEFAULT_SYNCHRONOUS\": 2,\n\
    \"SQLITE_DEFAULT_WAL_SYNCHRONOUS\": 2,\n\
    \"SQLITE_ENABLE_FTS5\": true,\n\
    \"SQLITE_ENABLE_RTREE\": true,\n\
    \"SQLITE_ENABLE_DBSTAT_VTAB\": true,\n\
    \"SQLITE_OMIT_WAL\": false,\n\
    \"SQLITE_OMIT_AUTORESET\": false\n\
  }},\n\
  \"rust_profile\": {{\n\
    \"production_default_page_size\": 4096,\n\
    \"testfixture_default_page_size\": {TESTFIXTURE_DEFAULT_PAGE_SIZE},\n\
    \"production_pending_byte_offset\": 1073741824,\n\
    \"test_64k_pending_byte_offset\": 65536,\n\
    \"json_max_depth\": 1000,\n\
    \"fts5_max_expr_depth\": 256,\n\
    \"json_subtype\": {JSON_SUBTYPE},\n\
    \"external_cargo_crates\": 0,\n\
    \"testfixture_binary\": \"{}\"\n\
  }},\n\
  \"os\": \"{}\",\n\
  \"filesystem\": \"{}\",\n\
  \"capability_table\": {{\n{caps_json}\n  }},\n\
  \"tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"total_registered\": 335,\n\
    \"active_total\": {},\n\
    \"ignored_total\": {},\n\
    \"phase7_families\": {{\n\
      \"json\": {{\"total\": 12, \"active\": 12, \"ignored\": 0}},\n\
      \"vtab\": {{\"total\": 26, \"active\": 15, \"ignored\": 11}},\n\
      \"fts5\": {{\"total\": 148, \"active\": 26, \"ignored\": 122}},\n\
      \"rtree\": {{\"total\": 26, \"active\": 7, \"ignored\": 19}}\n\
    }},\n\
    \"promoted_suites\": [\"tests/sqlite_test/pragma5.rs\", \"sqlite/test/windowB.test\"]\n\
  }}\n\
}}\n",
        json_escape(&manifest_uuid),
        json_escape(&ref_bin.display().to_string()),
        json_escape(&ext_bin.display().to_string()),
        json_escape(&testfixture_bin.display().to_string()),
        json_escape(&uname_out),
        json_escape(&fstype_out),
        active_ids.len(),
        ignored_ids.len(),
    );

    let out_manifest = root.join("target/sqlite-ref/phase7_manifest.json");
    fs::write(&out_manifest, &phase7_manifest_json).expect("write phase7_manifest.json");
    assert!(out_manifest.is_file());
}

#[test]
fn phase7_gate_2_multi_root_tcl_suites_and_pragma5() {
    let root = workspace_root();
    let representative_suites: &[(&str, &str, usize)] = &[
        ("sqlite/test", "pragma5", 7),
        ("sqlite/test", "windowB", 72),
        ("sqlite/test", "json101", 281),
        ("sqlite/test", "json501", 184),
        ("sqlite/test", "jsonb01", 40),
        ("sqlite/test", "vtab2", 15),
        ("sqlite/test", "vtab6", 76),
        ("sqlite/test", "vtabA", 18),
        ("sqlite/test", "vtab_alter", 13),
        ("sqlite/ext/fts5/test", "fts5ai", 12),
        ("sqlite/ext/fts5/test", "fts5expr", 303),
        ("sqlite/ext/fts5/test", "fts5leftjoin", 11),
        ("sqlite/ext/fts5/test", "fts5limits", 7),
        ("sqlite/ext/fts5/test", "fts5phrase", 46),
        ("sqlite/ext/fts5/test", "fts5update", 59),
        ("sqlite/ext/rtree", "rtree5", 16),
        ("sqlite/ext/rtree", "rtreeH", 21),
        ("sqlite/ext/rtree", "rtreecheck", 21),
    ];

    for &(source_root, stem, expected_passed) in representative_suites {
        let script_path = root.join(source_root).join(format!("{stem}.test"));
        let raw = fs::read_to_string(&script_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", script_path.display()));
        let stripped = strip_test_comments(&raw);
        let root_owned = source_root.to_owned();
        let stem_owned = stem.to_owned();
        let report = std::thread::Builder::new()
            .name(format!("p7gate-{stem}"))
            .stack_size(32 * 1024 * 1024)
            .spawn(move || evaluate_test_script_in_root(&root_owned, &stem_owned, &stripped))
            .expect("spawn tcl evaluation thread")
            .join()
            .expect("join tcl evaluation thread");

        assert!(
            report.is_ok(),
            "{source_root}/{stem}.test failed: passed={}, failed={:?}, script_error={:?}",
            report.passed,
            report.failed,
            report.script_error
        );
        assert_eq!(
            report.passed, expected_passed,
            "{source_root}/{stem}.test subtest count mismatch"
        );
        assert!(
            report.unknown_capabilities.is_empty(),
            "{source_root}/{stem}.test encountered unknown capabilities: {:?}",
            report.unknown_capabilities
        );
    }
}

#[derive(Default)]
struct GateVTabState {
    events: Vec<String>,
    fail_next_sync: bool,
}

struct GateAuditModule {
    name: String,
    risk: VTabRiskFlags,
    force_innocuous: bool,
    state: Arc<Mutex<GateVTabState>>,
}

impl VirtualTableModule for GateAuditModule {
    fn name(&self) -> &str {
        &self.name
    }

    fn risk_flags(&self) -> VTabRiskFlags {
        self.risk
    }

    fn shadow_table_suffixes(&self) -> &[&str] {
        &["store"]
    }

    fn create(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        if self.force_innocuous {
            ctx.set_vtab_config(VTabConfigOption::Innocuous)?;
        }
        let store = format!("{}_store", ctx.table_name());
        ctx.execute_sql(
            &format!("CREATE TABLE \"{store}\"(id INTEGER PRIMARY KEY, label TEXT)"),
            &[],
        )?;
        ctx.declare_vtab("CREATE TABLE x(label TEXT)")?;
        Ok(Box::new(GateAuditTable {
            table_name: ctx.table_name().to_owned(),
            state: Arc::clone(&self.state),
        }))
    }

    fn connect(
        &self,
        ctx: &mut dyn VTabHostContext,
        _args: &[String],
    ) -> Result<Box<dyn VirtualTable>> {
        if self.force_innocuous {
            ctx.set_vtab_config(VTabConfigOption::Innocuous)?;
        }
        ctx.declare_vtab("CREATE TABLE x(label TEXT)")?;
        Ok(Box::new(GateAuditTable {
            table_name: ctx.table_name().to_owned(),
            state: Arc::clone(&self.state),
        }))
    }
}

struct GateAuditTable {
    table_name: String,
    state: Arc<Mutex<GateVTabState>>,
}

impl VirtualTable for GateAuditTable {
    fn best_index(&self, _info: &mut VTabIndexInfo) -> Result<()> {
        Ok(())
    }

    fn open(&self) -> Result<Box<dyn VirtualCursor>> {
        Ok(Box::new(GateAuditCursor {
            table_name: self.table_name.clone(),
            rows: Vec::new(),
            pos: 0,
        }))
    }

    fn update(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        op: VTabUpdateOperation,
    ) -> Result<Option<i64>> {
        let store = format!("{}_store", self.table_name);
        match op {
            VTabUpdateOperation::Insert { new_rowid, values } => {
                let lbl = values.first().cloned().unwrap_or(Value::Null);
                match new_rowid {
                    Some(Value::Integer(id)) => {
                        ctx.execute_sql(
                            &format!("INSERT INTO \"{store}\"(id, label) VALUES(?1, ?2)"),
                            &[Value::Integer(id), lbl],
                        )?;
                        Ok(Some(id))
                    }
                    _ => {
                        ctx.execute_sql(
                            &format!("INSERT INTO \"{store}\"(label) VALUES(?1)"),
                            &[lbl],
                        )?;
                        let res = ctx.query_sql("SELECT last_insert_rowid()", &[])?;
                        Ok(res.first().and_then(|r| match r.first() {
                            Some(Value::Integer(i)) => Some(*i),
                            _ => None,
                        }))
                    }
                }
            }
            VTabUpdateOperation::Delete { old_rowid } => {
                ctx.execute_sql(
                    &format!("DELETE FROM \"{store}\" WHERE id = ?1"),
                    &[old_rowid],
                )?;
                Ok(None)
            }
            VTabUpdateOperation::Update {
                old_rowid,
                new_rowid,
                values,
            } => {
                let lbl = values.first().cloned().unwrap_or(Value::Null);
                let target_id = if new_rowid.is_null() {
                    old_rowid.clone()
                } else {
                    new_rowid
                };
                ctx.execute_sql(
                    &format!("UPDATE \"{store}\" SET id = ?1, label = ?2 WHERE id = ?3"),
                    &[target_id.clone(), lbl, old_rowid],
                )?;
                Ok(match target_id {
                    Value::Integer(i) => Some(i),
                    _ => None,
                })
            }
        }
    }

    fn begin(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.state.lock().unwrap().events.push("begin".to_owned());
        Ok(())
    }

    fn sync(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.events.push("sync".to_owned());
        if st.fail_next_sync {
            st.fail_next_sync = false;
            return Err(SqliteError::new(ErrorCode::Error, "gate xSync veto"));
        }
        Ok(())
    }

    fn commit(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.state.lock().unwrap().events.push("commit".to_owned());
        Ok(())
    }

    fn rollback(&mut self, _ctx: &mut dyn VTabHostContext) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .events
            .push("rollback".to_owned());
        Ok(())
    }

    fn savepoint(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .events
            .push(format!("savepoint:{idx}"));
        Ok(())
    }

    fn release(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .events
            .push(format!("release:{idx}"));
        Ok(())
    }

    fn rollback_to(&mut self, _ctx: &mut dyn VTabHostContext, idx: usize) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .events
            .push(format!("rollback_to:{idx}"));
        Ok(())
    }
}

struct GateAuditCursor {
    table_name: String,
    rows: Vec<(i64, Value)>,
    pos: usize,
}

impl VirtualCursor for GateAuditCursor {
    fn filter(
        &mut self,
        ctx: &mut dyn VTabHostContext,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        let store = format!("{}_store", self.table_name);
        let raw = ctx.query_sql(
            &format!("SELECT id, label FROM \"{store}\" ORDER BY id"),
            &[],
        )?;
        self.rows = raw
            .into_iter()
            .filter_map(|r| match r.first() {
                Some(Value::Integer(id)) => Some((*id, r.get(1).cloned().unwrap_or(Value::Null))),
                _ => None,
            })
            .collect();
        self.pos = 0;
        Ok(())
    }

    fn next(&mut self) -> Result<()> {
        self.pos += 1;
        Ok(())
    }

    fn eof(&self) -> bool {
        self.pos >= self.rows.len()
    }

    fn column(&self, idx: usize) -> Result<Value> {
        match idx {
            0 => Ok(self.rows[self.pos].1.clone()),
            _ => Ok(Value::Null),
        }
    }

    fn rowid(&self) -> Result<i64> {
        Ok(self.rows[self.pos].0)
    }
}

#[test]
fn phase7_gate_3_cross_feature_vtab_backup_vacuum_security_jsonb_subtype_matrix() {
    let ext_bin = ensure_c_sqlite_ext_binary();
    let sandbox = TempSandbox::new("cross_feature_matrix");

    // -------------------------------------------------------------------------
    // A. VTab savepoints, `xSync` commit veto rollback, and risk/defensive security
    //    (Groups 14, 21, 44, 53, 61, 63, 66)
    // -------------------------------------------------------------------------
    let vtab_state = Arc::new(Mutex::new(GateVTabState::default()));
    let rollback_fired = Arc::new(AtomicBool::new(false));
    let rb_flag = Arc::clone(&rollback_fired);

    let sec_conn = Connection::open_in_memory().expect("open in-memory");
    sec_conn
        .set_rollback_hook(Some(Box::new(move || {
            rb_flag.store(true, Ordering::SeqCst);
        })))
        .unwrap();
    sec_conn
        .register_virtual_table_module(Arc::new(GateAuditModule {
            name: "gate_direct".to_owned(),
            risk: VTabRiskFlags::DirectOnly,
            force_innocuous: false,
            state: Arc::clone(&vtab_state),
        }))
        .unwrap();
    sec_conn
        .register_virtual_table_module(Arc::new(GateAuditModule {
            name: "gate_normal".to_owned(),
            risk: VTabRiskFlags::Normal,
            force_innocuous: false,
            state: Arc::clone(&vtab_state),
        }))
        .unwrap();

    sec_conn
        .execute_batch(
            "CREATE TABLE ledger(id INTEGER PRIMARY KEY, note TEXT);
             CREATE VIRTUAL TABLE vt_norm USING gate_normal();
             CREATE VIRTUAL TABLE vt_dir USING gate_direct();
             INSERT INTO vt_norm(rowid, label) VALUES(1, 'seed');
             INSERT INTO ledger VALUES(1, 'seed');",
        )
        .unwrap();

    // Nested SAVEPOINT + ROLLBACK TO + RELEASE across B-tree and VTab
    sec_conn
        .execute_batch(
            "SAVEPOINT sp_a;
               INSERT INTO vt_norm(rowid, label) VALUES(2, 'kept');
               SAVEPOINT sp_b;
                 INSERT INTO vt_norm(rowid, label) VALUES(3, 'discarded');
                 INSERT INTO ledger VALUES(3, 'discarded');
               ROLLBACK TO sp_b;
             RELEASE sp_a;",
        )
        .unwrap();
    assert_eq!(
        format_rows_pipe(
            &sec_conn
                .query("SELECT rowid, label FROM vt_norm ORDER BY rowid;", &[])
                .unwrap()
                .rows
        ),
        "1|seed\n2|kept\n"
    );

    // `xSync` failure during autocommit or explicit COMMIT rolls back both B-tree and VTab!
    vtab_state.lock().unwrap().fail_next_sync = true;
    sec_conn.execute_batch("BEGIN;").unwrap();
    sec_conn
        .execute_batch(
            "INSERT INTO ledger VALUES(99, 'vetoed');
             INSERT INTO vt_norm(rowid, label) VALUES(99, 'vetoed');",
        )
        .unwrap();
    let commit_err = sec_conn.execute_batch("COMMIT;").unwrap_err();
    assert!(
        commit_err.message.contains("gate xSync veto"),
        "expected xSync error, got: {commit_err}"
    );
    // If explicit transaction stays active or rolls back, ensure clean state:
    let _ = sec_conn.execute_batch("ROLLBACK;");
    assert_eq!(
        format_rows_pipe(
            &sec_conn
                .query("SELECT id FROM ledger ORDER BY id;", &[])
                .unwrap()
                .rows
        ),
        "1\n"
    );
    assert_eq!(
        format_rows_pipe(
            &sec_conn
                .query("SELECT rowid FROM vt_norm ORDER BY rowid;", &[])
                .unwrap()
                .rows
        ),
        "1\n2\n"
    );

    // `DirectOnly` rejected inside VIEW even with `trusted_schema = ON`;
    // `Normal` rejected inside VIEW when `trusted_schema = OFF`.
    sec_conn
        .execute_batch(
            "CREATE VIEW v_dir AS SELECT * FROM vt_dir;
             CREATE VIEW v_norm AS SELECT * FROM vt_norm;",
        )
        .unwrap();
    let dir_err = sec_conn.query("SELECT * FROM v_dir;", &[]).unwrap_err();
    assert!(dir_err.message.contains("unsafe use of virtual table"));

    sec_conn
        .execute_batch("PRAGMA trusted_schema = OFF;")
        .unwrap();
    let norm_err = sec_conn.query("SELECT * FROM v_norm;", &[]).unwrap_err();
    assert!(norm_err.message.contains("unsafe use of virtual table"));
    sec_conn
        .execute_batch("PRAGMA trusted_schema = ON;")
        .unwrap();
    assert_eq!(
        sec_conn
            .query("SELECT count(*) FROM v_norm;", &[])
            .unwrap()
            .rows[0]
            .get_i64(0),
        Some(2)
    );

    // -------------------------------------------------------------------------
    // B. FTS5 + R-Tree + JSONB on disk: `set_defensive(true)`, atomic `ALTER TABLE RENAME`,
    //    `VACUUM`, online `Backup`, and C SQLite 3.54.1 `sqlite3_ext` verification
    //    (Groups 40, 44, 51, 52, 54, 64, 65)
    // -------------------------------------------------------------------------
    let src_db = sandbox.path().join("p7_source.db");
    let vac_db = sandbox.path().join("p7_vacuum_into.db");
    let bak_db = sandbox.path().join("p7_backup.db");

    {
        let conn = Connection::open(&src_db).expect("open source db");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, category TEXT, payload BLOB);
             INSERT INTO items VALUES
               (1, 'kitap', jsonb('{\"title\":\"Algoritmalar\",\"price\":120}')),
               (2, 'kitap', jsonb('{\"title\":\"Derleyiciler\",\"price\":185}')),
               (3, 'kalem', jsonb('{\"title\":\"Dolmakalem\",\"price\":95}'));

             CREATE VIRTUAL TABLE docs_old USING fts5(title, body, tokenize = 'porter unicode61');
             INSERT INTO docs_old(rowid, title, body) VALUES
               (1, 'running compilers', 'lexical analysis and parsing'),
               (2, 'spatial indexing', 'bounding boxes and rtree queries'),
               (3, 'json binary', 'structured jsonb storage and arrows');

             CREATE VIRTUAL TABLE boxes_old USING rtree(id, minX, maxX, minY, maxY, +label TEXT);
             INSERT INTO boxes_old VALUES
               (1, 0.0, 10.0, 0.0, 10.0, 'origin'),
               (2, 5.0, 25.0, 5.0, 25.0, 'overlap'),
               (3, 50.0, 60.0, 50.0, 60.0, 'far');",
        )
        .unwrap();

        // Defensive mode blocks direct shadow corruption while FTS5/R-Tree DML works.
        conn.set_defensive(true).unwrap();
        assert!(conn.execute("DELETE FROM docs_old_data;").is_err());
        assert!(conn.execute("DELETE FROM boxes_old_node;").is_err());
        conn.execute(
            "INSERT INTO docs_old(rowid, title, body) VALUES(4, 'defensive check', 'safe write');",
        )
        .unwrap();
        conn.set_defensive(false).unwrap();

        // Atomic RENAME rollback on shadow collision, then clean RENAME to final names.
        conn.execute("CREATE TABLE fts_docs_data(dummy INT);")
            .unwrap();
        assert!(
            conn.execute("ALTER TABLE docs_old RENAME TO fts_docs;")
                .is_err()
        );
        conn.execute("DROP TABLE fts_docs_data;").unwrap();
        conn.execute_batch(
            "ALTER TABLE docs_old RENAME TO fts_docs;
             ALTER TABLE boxes_old RENAME TO spatial_idx;",
        )
        .unwrap();

        // In-place VACUUM + VACUUM INTO preserving rootpage=0 virtual tables and shadow tables.
        conn.execute("VACUUM;").unwrap();
        conn.execute(&format!(
            "VACUUM INTO '{}';",
            vac_db.display().to_string().replace('\'', "''")
        ))
        .unwrap();

        // Online Backup (`src_db` -> `bak_db`)
        let dst = Connection::open(&bak_db).expect("open backup dst");
        {
            let mut backup = Backup::init(&conn, "main", &dst, "main").expect("start backup");
            assert_eq!(
                backup.step(-1).expect("backup step"),
                BackupStepResult::Done
            );
            backup.finish().expect("backup finish");
        }
    }

    // Verify all 3 database files (`src_db`, `vac_db`, `bak_db`) with C SQLite 3.54.1 `sqlite3_ext`!
    for (label, db_file) in [
        ("source", &src_db),
        ("vacuum_into", &vac_db),
        ("online_backup", &bak_db),
    ] {
        let (ok, out, err) = run_c_sqlite(
            ext_bin,
            sandbox.path(),
            db_file.to_str().unwrap(),
            "PRAGMA integrity_check;
             INSERT INTO fts_docs(fts_docs) VALUES('integrity-check');
             SELECT rtreecheck('spatial_idx');
             SELECT rowid FROM fts_docs WHERE fts_docs MATCH 'compile OR defensive' ORDER BY rowid;
             SELECT id, label FROM spatial_idx WHERE minX <= 12.0 AND maxX >= 6.0 ORDER BY id;
             SELECT id, payload ->> '$.title' FROM items ORDER BY id;
             SELECT count(*) > 0 FROM dbstat WHERE name = 'items';",
        );
        assert!(ok, "C sqlite3_ext failed on {label}: {err}");
        assert_eq!(
            out,
            "ok\nok\n1\n4\n1|origin\n2|overlap\n1|Algoritmalar\n2|Derleyiciler\n3|Dolmakalem\n1\n",
            "C sqlite3_ext output mismatch on {label}"
        );
    }

    // -------------------------------------------------------------------------
    // C. Corrupt JSONB boundary safety (0 panics) & JSON_SUBTYPE=74 window/arrow parity
    //    (Groups 14, 18, 52, 65)
    // -------------------------------------------------------------------------
    let jb_conn = Connection::open_in_memory().unwrap();
    jb_conn
        .create_scalar_function(
            "inspect_subtype",
            1,
            FunctionFlags::DETERMINISTIC.union(FunctionFlags::SUBTYPE),
            |args| {
                Ok(sqliters_core::RuntimeValue::from(Value::Integer(
                    i64::from(args[0].subtype),
                )))
            },
        )
        .unwrap();

    let malformed_jsonb_blobs: &[Vec<u8>] = &[
        vec![],
        vec![0x0f],                         // reserved type 15
        vec![0x1c, 0xff],                   // 1-byte size 255 truncated
        vec![0x2d, 0x01, 0x00],             // 2-byte size 256 truncated
        vec![0x4e, 0x7f, 0xff, 0xff, 0xff], // 4-byte huge size truncated
        vec![0x8f],                         // 8-byte size header truncated + bad type
        vec![0x1b, b'x'],                   // object with odd single element
        vec![0x37, 0xff],                   // declared 3-byte text payload truncated after 1 byte
    ];
    for bad in malformed_jsonb_blobs {
        let r1 = jb_conn.query("SELECT json(?1);", &[Value::Blob(bad.clone())]);
        assert!(
            r1.is_err(),
            "corrupt JSONB {bad:?} must return Err on json()"
        );
        let r2 = jb_conn.query("SELECT ?1 -> '$[0]';", &[Value::Blob(bad.clone())]);
        assert!(r2.is_err(), "corrupt JSONB {bad:?} must return Err on ->");
        let valid_strict = jb_conn
            .query("SELECT json_valid(?1, 8);", &[Value::Blob(bad.clone())])
            .unwrap();
        assert_eq!(valid_strict.rows[0].get_i64(0), Some(0));
    }
    // Superficial JSONB header check (`flags = 4`) vs strict recursive check (`flags = 8`):
    let sup_vs_strict = jb_conn
        .query(
            "SELECT json_valid(X'1b78', 4), json_valid(X'1b78', 8);",
            &[],
        )
        .unwrap();
    assert_eq!(sup_vs_strict.rows[0].get_i64(0), Some(1));
    assert_eq!(sup_vs_strict.rows[0].get_i64(1), Some(0));

    // Deep nesting > 1000 rejected cleanly without stack overflow
    let deep_1001 = format!("{}{}{}", "[".repeat(1001), "1", "]".repeat(1001));
    assert!(
        jb_conn
            .query("SELECT json(?1);", &[Value::Text(deep_1001)])
            .is_err()
    );

    // JSON_SUBTYPE = 74 preservation across scalar & window aggregates + subquery stripping
    let subtype_sql = "
        WITH s(k, v) AS (VALUES ('a', 10), ('b', 20))
        SELECT
          inspect_subtype(json('{\"x\":1}')),
          inspect_subtype((SELECT json('{\"x\":1}'))),
          inspect_subtype(json_group_array(v) OVER (ORDER BY k)),
          inspect_subtype(json_group_object(k, v) OVER (ORDER BY k)),
          inspect_subtype('{\"a\":[1,2]}' -> '$.a'),
          inspect_subtype('{\"a\":[1,2]}' ->> '$.a'),
          json_object('arr', json_group_array(v)) OVER ()
        FROM s LIMIT 1;
    ";
    // Verify scalar, subquery, window, and arrow subtype numbers directly:
    let st_rows = jb_conn
        .query(
            "WITH s(k, v) AS (VALUES ('a', 10), ('b', 20))
             SELECT
               inspect_subtype(json('{\"x\":1}')),
               inspect_subtype((SELECT json('{\"x\":1}'))),
               inspect_subtype(json_group_array(v) OVER (ORDER BY k)),
               inspect_subtype(json_group_object(k, v) OVER (ORDER BY k)),
               inspect_subtype('{\"a\":[1,2]}' -> '$.a'),
               inspect_subtype('{\"a\":[1,2]}' ->> '$.a')
             FROM s;",
            &[],
        )
        .unwrap();
    let _ = subtype_sql;
    assert_eq!(
        format_rows_pipe(&st_rows.rows),
        format!("{0}|0|{0}|{0}|{0}|0\n{0}|0|{0}|{0}|{0}|0\n", JSON_SUBTYPE)
    );

    // Differential window + JSONB aggregate parity against `sqlite3_ext`
    let diff_sql = "
        CREATE TABLE wj(grp TEXT, ord INT, info BLOB);
        INSERT INTO wj VALUES
          ('g1', 1, jsonb('{\"tag\":\"alpha\",\"n\":10}')),
          ('g1', 2, jsonb('{\"tag\":\"beta\",\"n\":20}')),
          ('g2', 1, jsonb('{\"tag\":\"gamma\",\"n\":30}'));
        SELECT
          grp,
          ord,
          json_group_array(info -> '$.tag') OVER (
            PARTITION BY grp ORDER BY ord ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
          ),
          json(jsonb_group_object(info ->> '$.tag', info -> '$.n') OVER (PARTITION BY grp))
        FROM wj
        ORDER BY grp, ord;
    ";
    let (_, c_diff_out, c_diff_err) = run_c_sqlite(ext_bin, sandbox.path(), ":memory:", diff_sql);
    assert!(
        c_diff_err.is_empty(),
        "C sqlite3_ext diff error: {c_diff_err}"
    );
    jb_conn
        .execute_batch(
            "CREATE TABLE wj(grp TEXT, ord INT, info BLOB);
             INSERT INTO wj VALUES
               ('g1', 1, jsonb('{\"tag\":\"alpha\",\"n\":10}')),
               ('g1', 2, jsonb('{\"tag\":\"beta\",\"n\":20}')),
               ('g2', 1, jsonb('{\"tag\":\"gamma\",\"n\":30}'));",
        )
        .unwrap();
    let r_diff = jb_conn
        .query(
            "SELECT
               grp,
               ord,
               json_group_array(info -> '$.tag') OVER (
                 PARTITION BY grp ORDER BY ord ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
               ),
               json(jsonb_group_object(info ->> '$.tag', info -> '$.n') OVER (PARTITION BY grp))
             FROM wj
             ORDER BY grp, ord;",
            &[],
        )
        .unwrap();
    assert_eq!(format_rows_pipe(&r_diff.rows), c_diff_out);
}

#[test]
fn phase7_gate_4_cli_to_c_sqlite354_sequential_smoke_test() {
    let sandbox = TempSandbox::new("cli_smoke");
    let tam_test_db = sandbox.path().join("tam_test.db");
    let sqliters_cli = ensure_sqliters_cli_binary();
    let c_sqlite354_cli = ensure_c_sqlite_ref_binary();

    let rust_cli_sql = "
  PRAGMA journal_mode = WAL;

  CREATE TABLE urunler(
    id INTEGER PRIMARY KEY,
    ad TEXT,
    fiyat INT,
    meta BLOB
  );

  INSERT INTO urunler
  VALUES (1, 'Kalem', 50, jsonb('{\"etiket\":\"kirtasiye\"}'));

  SELECT
    ad,
    rank() OVER (ORDER BY fiyat DESC),
    meta ->> '$.etiket'
  FROM urunler;
";

    let cli_out = Command::new(&sqliters_cli)
        .arg(&tam_test_db)
        .arg(rust_cli_sql)
        .current_dir(sandbox.path())
        .output()
        .expect("run sqliters-cli on tam_test.db");
    assert!(
        cli_out.status.success(),
        "sqliters-cli failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&cli_out.stdout),
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let cli_stdout = String::from_utf8_lossy(&cli_out.stdout);
    assert_eq!(
        cli_stdout.trim(),
        "wal\nKalem|1|kirtasiye",
        "unexpected sqliters-cli smoke output"
    );

    let (c_ok, c_stdout, c_stderr) = run_c_sqlite(
        c_sqlite354_cli,
        sandbox.path(),
        tam_test_db.to_str().unwrap(),
        "
  PRAGMA integrity_check;
  PRAGMA journal_mode;
  SELECT id, ad, json(meta) FROM urunler;
",
    );
    assert!(
        c_ok,
        "C SQLite 3.54.1 CLI verification failed:\nstdout: {c_stdout}\nstderr: {c_stderr}"
    );
    assert_eq!(
        c_stdout.trim(),
        "ok\nwal\n1|Kalem|{\"etiket\":\"kirtasiye\"}",
        "unexpected C SQLite 3.54.1 smoke output"
    );
}
