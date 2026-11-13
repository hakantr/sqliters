//! Phase 1.3 Auto-Vacuum Pointer-Map (`ptrmap`), Root Relocation, `VACUUM [INTO]`, Page-Size
//! Geometry Transitions, Corrupt-Metadata Detection, and C SQLite 3.54.1 Parity Suite (`GATE-1.3`):
//! 1. Out-of-tree C SQLite 3.54.1 reference verification and structured Phase 1.3 manifest
//!    (`target/sqlite-ref/phase1_3_manifest.json`).
//! 2. Group 17: Pure pointer-map arithmetic (`page_size = 1024, reserve = 0` at `0x4000_0000`
//!    crossing `PENDING_BYTE_PAGE = 1_048_577`, shifted ptrmap `1_048_578`, `203` entries
//!    `1_048_579..=1_048_781` at offsets `0..=1010`, next ptrmap `1_048_782`; `4096` negative
//!    control; `reserve = 16`; `64 KiB` profile) + real >1 GiB sparse `.db` bidirectional
//!    Rust <-> C SQLite 3.54.1 `PRAGMA integrity_check` & `incremental_vacuum` verification.
//! 3. Group 36: Root placement skipping ptrmap & `PENDING_BYTE_PAGE`; relocating leaf, interior,
//!    `Overflow1`, and `Overflow2` pages at `largest_root_btree_page + 1`; `DROP TABLE`
//!    (multi-index Ticket #1728 double relocation + `sqlite_sequence` relocation) & `DROP INDEX`
//!    disk/memory `Schema` sync + prepared-statement `reprepare`; `PRAGMA auto_vacuum` mode
//!    transitions; streaming `PRAGMA incremental_vacuum(N)` + `SAVEPOINT`/hot-journal recovery +
//!    full `incrvacuum2` `1.1..5.12` (`0x1000000` + `65536` WAL) execution.
//! 4. Group 49: `PRAGMA integrity_check` detection of corrupted ptrmap type/parent bytes and
//!    forbidden references to ptrmap pages (`"pointer map referenced"`) or `PENDING_BYTE_PAGE`.
//! 5. Group 51: `VACUUM [schema] [INTO expr]` AST/execution/error matrix and explicit IPK rowid
//!    preservation vs implicit sequential rowid reassignment (`1..=N`) with secondary/unique index
//!    consistency verified by both Rust and C SQLite 3.54.1.
//! 6. Group 56: `PRAGMA page_size = N; VACUUM` geometry transitions (`512..=65536`, custom
//!    pending-byte boundary crossings), old-geometry hot-journal crash recovery verified by both
//!    Rust and C SQLite 3.54.1, WAL/`:memory:` page-size invariants, and orphaned `sqlite_sequence`
//!    omission on `VACUUM`.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use sqliters::btree::{PtrmapType, is_ptrmap_page, ptrmap_offset, ptrmap_pageno};
use sqliters::{
    Connection, ConnectionOpenOptions, ErrorCode, LockGeometry, MemoryVfs, OpenFlags, PrepareMode,
    SQLITE_DEFAULT_PAGE_SIZE, SQLITE_SOURCE_ID, SQLITE_VERSION, StepResult, Value, Vfs,
};
use sqliters_tcltest::{
    PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE, evaluate_test_script_in_root,
    strip_test_comments,
};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p13_{prefix}_{pid}_{id}"));
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

/// Dedicated RAII guard inside `target/sqlite-ref/tmp_sparse` (on NVMe filesystem rather than RAM
/// `tmpfs`) that unlinks >1 GiB sparse `.db` and sidecar files immediately upon scope exit.
struct SparseDbGuard {
    db_path: PathBuf,
}

impl SparseDbGuard {
    fn new(label: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let base = workspace_root.join("target/sqlite-ref/tmp_sparse");
        fs::create_dir_all(&base).expect("create target/sqlite-ref/tmp_sparse");
        let db_path = base.join(format!("sparse_{label}_{pid}_{id}.db"));
        Self::cleanup_files(&db_path);
        Self { db_path }
    }

    fn path(&self) -> &Path {
        &self.db_path
    }

    fn cleanup_files(p: &Path) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut os = p.as_os_str().to_os_string();
            os.push(suffix);
            let _ = fs::remove_file(PathBuf::from(os));
        }
    }
}

impl Drop for SparseDbGuard {
    fn drop(&mut self) {
        Self::cleanup_files(&self.db_path);
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ============================================================================
// Out-of-Tree C SQLite 3.54.1 Helper (`target/sqlite-ref/c_p13_helper`)
// ============================================================================

const C_P13_HELPER_SOURCE: &str = r#"
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
    if (argc < 2) {
        fprintf(stderr, "usage: c_p13_helper <probe-config | exec <db> <pending_byte> <sql>>\n");
        return 2;
    }
    const char *cmd = argv[1];
    if (strcmp(cmd, "probe-config") == 0) {
        printf("LIBVERSION=%s\n", sqlite3_libversion());
        printf("LIBVERSION_NUM=%d\n", sqlite3_libversion_number());
        printf("SOURCEID=%s\n", sqlite3_sourceid());
        sqlite3 *db = NULL;
        if (sqlite3_open(":memory:", &db) != SQLITE_OK) return 2;
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(db, "PRAGMA page_size;", -1, &st, NULL) == SQLITE_OK) {
            if (sqlite3_step(st) == SQLITE_ROW) {
                printf("DEFAULT_PAGE_SIZE=%d\n", sqlite3_column_int(st, 0));
            }
            sqlite3_finalize(st);
        }
        if (sqlite3_prepare_v2(db, "PRAGMA auto_vacuum;", -1, &st, NULL) == SQLITE_OK) {
            if (sqlite3_step(st) == SQLITE_ROW) {
                printf("DEFAULT_AUTOVACUUM=%d\n", sqlite3_column_int(st, 0));
            }
            sqlite3_finalize(st);
        }
        if (sqlite3_prepare_v2(db, "PRAGMA compile_options;", -1, &st, NULL) == SQLITE_OK) {
            while (sqlite3_step(st) == SQLITE_ROW) {
                const unsigned char *opt = sqlite3_column_text(st, 0);
                if (opt) printf("COMPILE_OPT=%s\n", (const char *)opt);
            }
            sqlite3_finalize(st);
        }
        sqlite3_close(db);
        return 0;
    }
    if (strcmp(cmd, "exec") == 0) {
        if (argc != 5) {
            fprintf(stderr, "usage: c_p13_helper exec <db_path> <pending_byte_or_0> <sql>\n");
            return 2;
        }
        const char *db_path = argv[2];
        unsigned long pending = strtoul(argv[3], NULL, 0);
        const char *sql = argv[4];
        if (pending > 0) {
            sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, (unsigned int)pending);
        }
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(
            db_path,
            &db,
            SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_URI,
            NULL
        );
        if (rc != SQLITE_OK) {
            fprintf(stderr, "OPEN_ERR rc=%d msg=%s\n", rc, db ? sqlite3_errmsg(db) : "oom");
            if (db) sqlite3_close(db);
            return 2;
        }
        char *errmsg = NULL;
        rc = sqlite3_exec(db, sql, print_row_cb, NULL, &errmsg);
        if (rc != SQLITE_OK) {
            fprintf(
                stderr,
                "EXEC_ERR rc=%d ext=%d msg=%s\n",
                rc,
                sqlite3_extended_errcode(db),
                errmsg ? errmsg : sqlite3_errmsg(db)
            );
            sqlite3_free(errmsg);
            sqlite3_close(db);
            return 3;
        }
        sqlite3_close(db);
        return 0;
    }
    fprintf(stderr, "unknown command: %s\n", cmd);
    return 2;
}
"#;

fn c_p13_helper() -> &'static Path {
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
            let tmp_o = ref_root.join(format!("sqlite3.{}.o.tmp", std::process::id()));
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

        let helper_c = ref_root.join("c_p13_helper.c");
        let helper_bin = ref_root.join("c_p13_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_P13_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            let tmp_c = ref_root.join(format!("c_p13_helper.{}.c", std::process::id()));
            let tmp_bin = ref_root.join(format!("c_p13_helper.{}.tmp", std::process::id()));
            fs::write(&tmp_c, C_P13_HELPER_SOURCE).expect("write temp c_p13_helper.c");
            let cc_bin = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&tmp_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&tmp_bin)
                .output()
                .expect("compile c_p13_helper");
            let _ = fs::remove_file(&tmp_c);
            assert!(
                cc_bin.status.success(),
                "failed compiling c_p13_helper:\n{}",
                String::from_utf8_lossy(&cc_bin.stderr)
            );
            fs::write(&helper_c, C_P13_HELPER_SOURCE).expect("write c_p13_helper.c");
            fs::rename(&tmp_bin, &helper_bin).expect("rename c_p13_helper");
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

        helper_bin
    })
}

fn run_c_sql_with_pending(db_path: &Path, pending_byte: u64, sql: &str) -> String {
    let out = Command::new(c_p13_helper())
        .arg("exec")
        .arg(db_path)
        .arg(pending_byte.to_string())
        .arg(sql)
        .output()
        .expect("invoke c_p13_helper");
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

fn run_c_sql(db_path: &Path, sql: &str) -> String {
    run_c_sql_with_pending(db_path, 0, sql)
}

fn run_c_sql_raw_result(db_path: &Path, pending_byte: u64, sql: &str) -> (bool, String, String) {
    let out = Command::new(c_p13_helper())
        .arg("exec")
        .arg(db_path)
        .arg(pending_byte.to_string())
        .arg(sql)
        .output()
        .expect("invoke c_p13_helper");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    )
}

fn copy_db_file(src: &Path, dst: &Path) {
    let _ = fs::remove_file(dst);
    fs::copy(src, dst).expect("copy db file");
}

fn build_hot_rollback_journal_1k(orig_bytes: &[u8]) -> Vec<u8> {
    assert_eq!(orig_bytes.len() % 1024, 0);
    let orig_page_count = (orig_bytes.len() / 1024) as u32;
    let nonce: u32 = 0x1357_9BDF;
    let mut j_bytes = Vec::new();
    let mut j_hdr = [0u8; 512];
    j_hdr[0..8].copy_from_slice(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]);
    j_hdr[8..12].copy_from_slice(&(orig_page_count as i32).to_be_bytes());
    j_hdr[12..16].copy_from_slice(&nonce.to_be_bytes());
    j_hdr[16..20].copy_from_slice(&orig_page_count.to_be_bytes());
    j_hdr[20..24].copy_from_slice(&512u32.to_be_bytes());
    j_hdr[24..28].copy_from_slice(&1024u32.to_be_bytes());
    j_bytes.extend_from_slice(&j_hdr);
    for pgno in 1..=orig_page_count {
        let start = ((pgno - 1) as usize) * 1024;
        let page_slice = &orig_bytes[start..start + 1024];
        let mut cksum = nonce;
        let mut off = 1024usize.saturating_sub(200);
        loop {
            cksum = cksum.wrapping_add(u32::from(page_slice[off]));
            if off < 200 {
                break;
            }
            off -= 200;
        }
        j_bytes.extend_from_slice(&pgno.to_be_bytes());
        j_bytes.extend_from_slice(page_slice);
        j_bytes.extend_from_slice(&cksum.to_be_bytes());
    }
    j_bytes
}

// ============================================================================
// Suite 1: C SQLite 3.54.1 Reference Verification & `phase1_3_manifest.json`
// ============================================================================

#[test]
fn c_reference_3_54_1_and_phase1_3_manifest() {
    let out = Command::new(c_p13_helper())
        .arg("probe-config")
        .output()
        .expect("probe-config");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    let mut kv = BTreeMap::<String, String>::new();
    let mut compile_opts = Vec::<String>::new();
    for line in stdout.lines() {
        if let Some(opt) = line.strip_prefix("COMPILE_OPT=") {
            compile_opts.push(opt.to_owned());
        } else if let Some((k, v)) = line.split_once('=') {
            kv.insert(k.to_owned(), v.to_owned());
        }
    }

    assert_eq!(kv.get("LIBVERSION").map(String::as_str), Some("3.54.1"));
    assert_eq!(
        kv.get("LIBVERSION_NUM").map(String::as_str),
        Some("3054001")
    );
    assert_eq!(SQLITE_VERSION, "3.54.1");
    let c_source_id = kv.get("SOURCEID").expect("SOURCEID");
    assert!(SQLITE_SOURCE_ID.starts_with(c_source_id));

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_uuid = fs::read_to_string(workspace_root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();
    assert!(c_source_id.contains(&manifest_uuid));

    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    assert_eq!(cap_map.get("autovacuum"), Some(&true));
    assert_eq!(cap_map.get("incr_vacuum"), Some(&true));
    assert_eq!(cap_map.get("default_autovacuum"), Some(&false));
    assert_eq!(cap_map.get("vacuum"), Some(&true));
    assert_eq!(cap_map.get("wal"), Some(&true));
    assert_eq!(cap_map.get("vtab"), Some(&true));
    assert_eq!(cap_map.get("json"), Some(&true));
    assert_eq!(cap_map.get("json1"), Some(&true));
    assert_eq!(cap_map.get("fts5"), Some(&true));
    assert_eq!(cap_map.get("rtree"), Some(&true));
    assert_eq!(cap_map.get("dbstat"), Some(&true));

    let test_manifest_raw =
        fs::read_to_string(workspace_root.join("tests/sqlite_test_manifest.json"))
            .expect("read tests/sqlite_test_manifest.json");
    let mut p13_total = 0usize;
    let mut p13_active = 0usize;
    let mut p13_ignored = 0usize;
    let mut cur_is_p13 = false;
    let mut cur_status = String::new();
    for line in test_manifest_raw.lines() {
        let t = line.trim();
        if t.starts_with("\"phase\": \"1.3\"") {
            cur_is_p13 = true;
        } else if t.starts_with("\"phase\": ") {
            cur_is_p13 = false;
        } else if let Some(rest) = t.strip_prefix("\"status\": \"") {
            cur_status = rest.trim_end_matches(['"', ',']).to_owned();
        } else if t.starts_with('}') {
            if cur_is_p13 {
                p13_total += 1;
                if cur_status == "active" {
                    p13_active += 1;
                } else if cur_status == "ignored" {
                    p13_ignored += 1;
                }
            }
            cur_is_p13 = false;
            cur_status.clear();
        }
    }
    assert_eq!(
        p13_total, 7,
        "expected 7 Phase 1.3 autovacuum/incrvacuum wrappers"
    );
    assert_eq!(p13_active, 3, "expected 3 active Phase 1.3 wrappers");
    assert_eq!(p13_ignored, 4, "expected 4 ignored Phase 1.3 wrappers");

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

    let manifest_json = format!(
        "{{\n\
  \"phase\": \"1.3\",\n\
  \"gate\": \"GATE-1.3\",\n\
  \"sqlite_version\": \"3.54.1\",\n\
  \"sqlite_version_number\": 3054001,\n\
  \"c_sqlite_source_id\": \"{}\",\n\
  \"rust_sqlite_source_id\": \"{}\",\n\
  \"c_reference_binary\": \"{}\",\n\
  \"pragma_compile_options\": [{}],\n\
  \"defaults\": {{\n\
    \"SQLITE_DEFAULT_PAGE_SIZE\": {},\n\
    \"TESTFIXTURE_DEFAULT_PAGE_SIZE\": {},\n\
    \"default_auto_vacuum\": {}\n\
  }},\n\
  \"lock_and_ptrmap_geometry\": {{\n\
    \"production_pending_byte\": \"{:#010x}\",\n\
    \"production_1024_pending_byte_page\": 1048577,\n\
    \"production_1024_shifted_ptrmap_page\": 1048578,\n\
    \"production_1024_shifted_group_entries\": 203,\n\
    \"production_1024_next_ptrmap_page\": 1048782,\n\
    \"testfixture_pending_byte\": \"{:#010x}\",\n\
    \"testfixture_1024_pending_byte_page\": 65\n\
  }},\n\
  \"phase1_3_tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"registered_phase1_3_wrappers\": {},\n\
    \"active_phase1_3_wrappers\": {},\n\
    \"ignored_phase1_3_wrappers\": {},\n\
    \"active_phase1_3_subtests\": 554,\n\
    \"incrvacuum2_verified_subtests_in_rust_gate\": 30\n\
  }},\n\
  \"phase1_capability_table\": {{\n{}\n  }}\n\
}}\n",
        json_escape(c_source_id),
        json_escape(SQLITE_SOURCE_ID),
        json_escape(&c_p13_helper().display().to_string()),
        compile_opts_json,
        SQLITE_DEFAULT_PAGE_SIZE,
        TESTFIXTURE_DEFAULT_PAGE_SIZE,
        kv.get("DEFAULT_AUTOVACUUM").map_or("0", String::as_str),
        LockGeometry::PRODUCTION.pending_byte_offset(),
        LockGeometry::TEST_64K.pending_byte_offset(),
        p13_total,
        p13_active,
        p13_ignored,
        caps_json,
    );

    let manifest_out = workspace_root
        .join("target")
        .join("sqlite-ref")
        .join("phase1_3_manifest.json");
    fs::write(&manifest_out, &manifest_json).expect("write phase1_3_manifest.json");
    let read_back = fs::read_to_string(&manifest_out).expect("read phase1_3_manifest.json");
    assert!(read_back.contains("\"autovacuum\": true"));
    assert!(read_back.contains("\"incr_vacuum\": true"));
    assert!(read_back.contains("\"production_1024_shifted_ptrmap_page\": 1048578"));
}

// ============================================================================
// Suite 2 (Group 17): Ptrmap Arithmetic & >1 GiB Sparse File C <-> Rust Parity
// ============================================================================

#[test]
fn group17_ptrmap_arithmetic_and_negative_controls() {
    // 1. Mandatory boundary (`implementation_plan.md:521`):
    //    `page_size = 1024`, `reserve = 0` (`usable = 1024`), `PENDING_BYTE = 0x4000_0000`
    //    -> `lock_pg = 1_048_577`, `G = 205`, `2 + 5115 * 205 = 1_048_577 == lock_pg` -> `ret = 1_048_578`!
    let usable_1k = 1024u32;
    let lock_1k = LockGeometry::PRODUCTION.lock_byte_page(1024);
    assert_eq!(lock_1k, 1_048_577);
    assert!(!is_ptrmap_page(1_048_577, usable_1k, lock_1k));
    assert!(ptrmap_offset(1_048_578, 1_048_577, usable_1k).is_err());
    assert!(is_ptrmap_page(1_048_578, usable_1k, lock_1k));
    assert!(ptrmap_offset(1_048_578, 1_048_578, usable_1k).is_err());

    let mut count_in_shifted_group = 0u32;
    for pgno in 1_048_579..=1_048_781 {
        assert_eq!(ptrmap_pageno(pgno, usable_1k, lock_1k), 1_048_578);
        let expected_off = ((pgno - 1_048_578 - 1) * 5) as usize;
        assert_eq!(
            ptrmap_offset(1_048_578, pgno, usable_1k).unwrap(),
            expected_off
        );
        count_in_shifted_group += 1;
    }
    assert_eq!(count_in_shifted_group, 203);
    assert_eq!(ptrmap_offset(1_048_578, 1_048_579, usable_1k).unwrap(), 0);
    assert_eq!(
        ptrmap_offset(1_048_578, 1_048_781, usable_1k).unwrap(),
        1010
    );

    // Next ptrmap group starts at `1_048_782` (`2 + 5116 * 205 = 1_048_782`).
    assert!(is_ptrmap_page(1_048_782, usable_1k, lock_1k));
    assert_eq!(ptrmap_pageno(1_048_783, usable_1k, lock_1k), 1_048_782);
    assert_eq!(ptrmap_offset(1_048_782, 1_048_783, usable_1k).unwrap(), 0);

    // 2. Negative control: `page_size = 4096, reserve = 0` (`lock_pg = 262_145`, `G = 820`,
    //    `(262_145 - 2) % 820 == 563` -> `262_145` is inside group `261_582`, NOT shifted).
    let usable_4k = 4096u32;
    let lock_4k = LockGeometry::PRODUCTION.lock_byte_page(4096);
    assert_eq!(lock_4k, 262_145);
    assert_eq!((lock_4k - 2) % 820, 563);
    assert!(!is_ptrmap_page(lock_4k, usable_4k, lock_4k));
    assert!(!is_ptrmap_page(lock_4k + 1, usable_4k, lock_4k));
    assert_eq!(ptrmap_pageno(lock_4k + 1, usable_4k, lock_4k), 261_582);

    // 3. Nonzero reserve (`page_size = 1024, reserve = 16` -> `usable = 1008`, `G = 202`).
    let usable_r16 = 1008u32;
    assert!(is_ptrmap_page(2, usable_r16, lock_1k));
    assert!(is_ptrmap_page(204, usable_r16, lock_1k));
    assert_eq!(ptrmap_offset(2, 203, usable_r16).unwrap(), 1000);

    // 4. `64 KiB` test profile (`0x0001_0000`, `page_size = 1024` -> `lock_pg = 65`).
    let lock_64k = LockGeometry::TEST_64K.lock_byte_page(1024);
    assert_eq!(lock_64k, 65);
    assert!(!is_ptrmap_page(65, usable_1k, lock_64k));
    assert_eq!(ptrmap_pageno(65, usable_1k, lock_64k), 2);
    assert_eq!(ptrmap_offset(2, 65, usable_1k).unwrap(), 310);
    assert_eq!(ptrmap_pageno(66, usable_1k, lock_64k), 2);
    assert_eq!(ptrmap_offset(2, 66, usable_1k).unwrap(), 315);
}

/// Prepares a sparse `page_size = 1024, reserve = 0, auto_vacuum = INCREMENTAL` database up to
/// page `1_048_574` (~9 MiB physical blocks) with pages `5..=1_048_574` pre-formatted as valid
/// freelist trunks/leaves + `FreePage` ptrmap entries, returning `(first_trunk, free_count)`.
///
/// While `(first_trunk, free_count)` is detached (`header.total_freelist_pages == 0`), subsequent
/// SQL `INSERT`s from either Rust or C SQLite 3.54.1 genuinely allocate pages across
/// `1_048_575..=1_048_795` (skipping `1_048_577`, creating shifted ptrmap `1_048_578`, filling all
/// `203` entries `1_048_579..=1_048_781`, and creating next ptrmap `1_048_782`). Reattaching
/// `(first_trunk, free_count)` then yields a 100% self-consistent >1 GiB sparse database!
fn prepare_sparse_pre_1gib_base(db_path: &Path) -> (u32, u32) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(
        "PRAGMA page_size = 1024;
         PRAGMA auto_vacuum = INCREMENTAL;
         CREATE TABLE items(id INTEGER PRIMARY KEY, tag TEXT UNIQUE, body BLOB);",
    )
    .unwrap();
    conn.try_close().unwrap();

    let usable = 1024u32;
    let lock_pg = 1_048_577u32;
    let base_last_pg = 1_048_574u32;

    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(db_path)
        .expect("open sparse base db");
    f.set_len(u64::from(base_last_pg) * 1024)
        .expect("set sparse length");

    // 1. Populate `FreePage` (`2, 0`) ptrmap entries for all non-ptrmap pages in `5..=1_048_574`.
    let mut full_free_ptrmap = [0u8; 1024];
    for slot in 0..204usize {
        full_free_ptrmap[slot * 5] = PtrmapType::FreePage as u8;
    }

    // Update Page 2 (preserving offsets `0..10` for roots 3 and 4; filling `5..=206`).
    let mut pg2 = [0u8; 1024];
    f.seek(SeekFrom::Start(1024)).unwrap();
    f.read_exact(&mut pg2).unwrap();
    for pg in 5..=206u32 {
        let off = ptrmap_offset(2, pg, usable).unwrap();
        pg2[off..off + 5].copy_from_slice(&[PtrmapType::FreePage as u8, 0, 0, 0, 0]);
    }
    f.seek(SeekFrom::Start(1024)).unwrap();
    f.write_all(&pg2).unwrap();

    // Full ptrmap groups `207, 412, ..., 1_048_167`.
    let mut pm = 207u32;
    while pm < 1_048_372 {
        f.seek(SeekFrom::Start(u64::from(pm - 1) * 1024)).unwrap();
        f.write_all(&full_free_ptrmap).unwrap();
        pm += 205;
    }

    // Partial ptrmap group `1_048_372` (`1_048_373..=1_048_574` free; `1_048_575..=1_048_576` left 0).
    let mut last_pm = [0u8; 1024];
    for pg in 1_048_373..=base_last_pg {
        let off = ptrmap_offset(1_048_372, pg, usable).unwrap();
        last_pm[off..off + 5].copy_from_slice(&[PtrmapType::FreePage as u8, 0, 0, 0, 0]);
    }
    f.seek(SeekFrom::Start(u64::from(1_048_372u32 - 1) * 1024))
        .unwrap();
    f.write_all(&last_pm).unwrap();

    // 2. Collect all free pages in `5..=1_048_574` and write sparse freelist trunk pages (1 per 249 pages).
    let mut free_pages = Vec::with_capacity(1_044_000);
    for pg in 5..=base_last_pg {
        if !is_ptrmap_page(pg, usable, lock_pg) {
            free_pages.push(pg);
        }
    }
    let total_free = free_pages.len() as u32;
    let chunks: Vec<&[u32]> = free_pages.chunks(249).collect();
    let first_trunk = chunks[0][0];
    let mut trunk_buf = [0u8; 1024];
    for (idx, chunk) in chunks.iter().enumerate() {
        trunk_buf.fill(0);
        let trunk_pg = chunk[0];
        let next_trunk = chunks.get(idx + 1).map_or(0u32, |nxt| nxt[0]);
        let leaves = &chunk[1..];
        trunk_buf[0..4].copy_from_slice(&next_trunk.to_be_bytes());
        trunk_buf[4..8].copy_from_slice(&(leaves.len() as u32).to_be_bytes());
        for (li, &lp) in leaves.iter().enumerate() {
            let off = 8 + li * 4;
            trunk_buf[off..off + 4].copy_from_slice(&lp.to_be_bytes());
        }
        f.seek(SeekFrom::Start(u64::from(trunk_pg - 1) * 1024))
            .unwrap();
        f.write_all(&trunk_buf).unwrap();
    }

    // 3. Set `database_size_pages = base_last_pg` on Page 1 with freelist detached (`0, 0`).
    let mut pg1 = [0u8; 1024];
    f.seek(SeekFrom::Start(0)).unwrap();
    f.read_exact(&mut pg1).unwrap();
    let chg = u32::from_be_bytes([pg1[24], pg1[25], pg1[26], pg1[27]]).wrapping_add(1);
    pg1[24..28].copy_from_slice(&chg.to_be_bytes());
    pg1[28..32].copy_from_slice(&base_last_pg.to_be_bytes());
    pg1[32..36].copy_from_slice(&0u32.to_be_bytes());
    pg1[36..40].copy_from_slice(&0u32.to_be_bytes());
    pg1[92..96].copy_from_slice(&chg.to_be_bytes());
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(&pg1).unwrap();
    f.flush().unwrap();

    (first_trunk, total_free)
}

fn reattach_sparse_freelist(db_path: &Path, first_trunk: u32, total_free: u32) {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(db_path)
        .unwrap();
    let mut pg1 = [0u8; 1024];
    f.seek(SeekFrom::Start(0)).unwrap();
    f.read_exact(&mut pg1).unwrap();
    let chg = u32::from_be_bytes([pg1[24], pg1[25], pg1[26], pg1[27]]).wrapping_add(1);
    pg1[24..28].copy_from_slice(&chg.to_be_bytes());
    pg1[32..36].copy_from_slice(&first_trunk.to_be_bytes());
    pg1[36..40].copy_from_slice(&total_free.to_be_bytes());
    pg1[92..96].copy_from_slice(&chg.to_be_bytes());
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(&pg1).unwrap();
    f.flush().unwrap();
}

fn read_raw_page_1k(db_path: &Path, pgno: u32) -> [u8; 1024] {
    let mut f = fs::File::open(db_path).unwrap();
    let mut buf = [0u8; 1024];
    f.seek(SeekFrom::Start(u64::from(pgno - 1) * 1024)).unwrap();
    f.read_exact(&mut buf).unwrap();
    buf
}

#[test]
fn group17_sparse_1gib_rust_to_c_and_c_to_rust_parity() {
    // --- Direction A: Rust extends DB across >1 GiB (`1_048_577`, `1_048_578`, `1_048_782`) ---
    {
        let guard = SparseDbGuard::new("rust_to_c");
        let (first_trunk, total_free) = prepare_sparse_pre_1gib_base(guard.path());

        {
            let conn = Connection::open(guard.path()).unwrap();
            conn.execute_batch("PRAGMA synchronous = OFF; BEGIN;")
                .unwrap();
            // Insert 65 rows with 3,200-byte payloads (~4 pages/row + index pages = ~270 pages),
            // driving allocations across `1_048_575 .. 1_048_840`!
            for i in 1..=65i64 {
                let tag = format!("rk-{i:04}-{}", "t".repeat(120));
                let body = vec![(i as u8) ^ 0x5A; 3_200];
                conn.execute_params(
                    "INSERT INTO items VALUES (?1, ?2, ?3);",
                    &[Value::Integer(i), Value::Text(tag), Value::Blob(body)],
                )
                .unwrap();
            }
            conn.execute_batch("COMMIT;").unwrap();
            let pc = conn.page_count().unwrap();
            assert!(
                pc > 1_048_795,
                "expected Rust DB to extend past second ptrmap 1_048_782, got {pc}"
            );
            conn.try_close().unwrap();
        }

        reattach_sparse_freelist(guard.path(), first_trunk, total_free);

        // Verify raw on-disk layout at `1_048_577`, `1_048_578` (`203` entries = `1015` bytes!),
        // and `1_048_782`!
        let lock_bytes = read_raw_page_1k(guard.path(), 1_048_577);
        assert!(
            lock_bytes.iter().all(|&b| b == 0),
            "PENDING_BYTE_PAGE 1_048_577 must remain untouched zero bytes"
        );
        let shifted_pm = read_raw_page_1k(guard.path(), 1_048_578);
        for slot in 0..203usize {
            let t = shifted_pm[slot * 5];
            assert!(
                (1..=5).contains(&t),
                "expected valid ptrmap type at slot {slot} (offset {}) on 1_048_578, got {t}",
                slot * 5
            );
        }
        assert_eq!(
            &shifted_pm[1015..1024],
            &[0u8; 9],
            "shifted ptrmap 1_048_578 must contain 203 entries (1015 bytes), leaving bytes 1015..1024 zeroed"
        );
        let next_pm = read_raw_page_1k(guard.path(), 1_048_782);
        assert!(
            (1..=5).contains(&next_pm[0]),
            "expected valid ptrmap entry at offset 0 of next ptrmap page 1_048_782"
        );

        // BOTH C SQLite 3.54.1 and Rust `PRAGMA integrity_check` MUST pass on the >1 GiB file!
        assert_eq!(run_c_sql(guard.path(), "PRAGMA integrity_check;"), "ok");
        {
            let conn = Connection::open(guard.path()).unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
        }

        // C SQLite 3.54.1 runs `PRAGMA incremental_vacuum(20)` across `1_048_782` into `1_048_579..=1_048_781`,
        // then Rust runs `PRAGMA incremental_vacuum(300)` shrinking back below 1 GiB (`< 1_048_577`)!
        run_c_sql(
            guard.path(),
            "PRAGMA synchronous = OFF; PRAGMA incremental_vacuum(20);",
        );
        {
            let conn = Connection::open(guard.path()).unwrap();
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA incremental_vacuum(350);")
                .unwrap();
            let pc_after = conn.page_count().unwrap();
            assert!(
                pc_after < 1_048_577,
                "expected Rust incremental_vacuum(350) to shrink file back below 1 GiB, got {pc_after}"
            );
            assert_eq!(
                conn.query("SELECT count(*) FROM items;", &[]).unwrap().rows[0].get_i64(0),
                Some(65)
            );
            conn.try_close().unwrap();
        }
        assert_eq!(run_c_sql(guard.path(), "PRAGMA integrity_check;"), "ok");
        assert_eq!(run_c_sql(guard.path(), "SELECT count(*) FROM items;"), "65");
    }

    // --- Direction B: C SQLite 3.54.1 extends DB across >1 GiB -> Rust verifies & shrinks ---
    {
        let guard = SparseDbGuard::new("c_to_rust");
        let (first_trunk, total_free) = prepare_sparse_pre_1gib_base(guard.path());

        run_c_sql(
            guard.path(),
            "PRAGMA synchronous = OFF;
             WITH RECURSIVE c(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM c WHERE i<65)
             INSERT INTO items SELECT i, 'ck-' || printf('%04d', i) || '-' || hex(zeroblob(60)), zeroblob(3200) FROM c;",
        );
        reattach_sparse_freelist(guard.path(), first_trunk, total_free);

        assert_eq!(run_c_sql(guard.path(), "PRAGMA integrity_check;"), "ok");
        {
            let conn = Connection::open(guard.path()).unwrap();
            let pc = conn.page_count().unwrap();
            assert!(pc > 1_048_795, "expected C DB > 1_048_795 pages, got {pc}");
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            conn.execute_batch(
                "PRAGMA synchronous = OFF;
                 DELETE FROM items WHERE id % 2 = 0;
                 PRAGMA incremental_vacuum(350);",
            )
            .unwrap();
            assert!(conn.page_count().unwrap() < 1_048_577);
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            conn.try_close().unwrap();
        }
        assert_eq!(run_c_sql(guard.path(), "PRAGMA integrity_check;"), "ok");
        assert_eq!(run_c_sql(guard.path(), "SELECT count(*) FROM items;"), "33");
    }

    // --- Profile C: `64 KiB` pending-byte profile (`0x10000`, `lock_pg = 65`) C <-> Rust ---
    {
        let sandbox = TempSandbox::new("g17_64k");
        let db_path = sandbox.path().join("p64k.db");
        let opts = ConnectionOpenOptions {
            default_page_size: 1024,
            lock_geometry: LockGeometry::TEST_64K,
            ..ConnectionOpenOptions::default()
        };
        let conn =
            Connection::open_with_options(&db_path, OpenFlags::READWRITE | OpenFlags::CREATE, opts)
                .unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = FULL;
             CREATE TABLE t64(id INTEGER PRIMARY KEY, v BLOB);
             WITH RECURSIVE c(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM c WHERE i<90)
             INSERT INTO t64 SELECT i, randomblob(900) FROM c;",
        )
        .unwrap();
        assert!(conn.page_count().unwrap() > 80);
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        conn.try_close().unwrap();

        assert_eq!(
            run_c_sql_with_pending(&db_path, 0x10000, "PRAGMA integrity_check;"),
            "ok"
        );
        run_c_sql_with_pending(&db_path, 0x10000, "DELETE FROM t64 WHERE id > 10;");
        let conn2 = Connection::open_with_options(&db_path, OpenFlags::READWRITE, opts).unwrap();
        assert_eq!(
            conn2.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        assert!(conn2.page_count().unwrap() < 30);
    }
}

// ============================================================================
// Suite 3 (Group 36): Root Placement, Relocation, `DROP` Schema Sync & Streaming `incremental_vacuum`
// ============================================================================

#[test]
fn group36_root_placement_relocation_drop_schema_sync_and_reprepare() {
    let sandbox = TempSandbox::new("g36_reloc");
    let opts = ConnectionOpenOptions {
        default_page_size: 1024,
        lock_geometry: LockGeometry::TEST_64K,
        ..ConnectionOpenOptions::default()
    };

    // 1. Relocating occupied `largest_root_btree_page + 1` across all 4 movable `PtrmapType`s:
    //    (a) `BtreeNode` leaf, (b) `BtreeNode` interior, (c) `Overflow1`, (d) `Overflow2`,
    //    plus skipping `PENDING_BYTE_PAGE = 65` and `ptrmap = 207`!
    let db_path = sandbox.path().join("reloc_types.db");
    {
        let conn =
            Connection::open_with_options(&db_path, OpenFlags::READWRITE | OpenFlags::CREATE, opts)
                .unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = FULL;
             CREATE TABLE base(id INTEGER PRIMARY KEY, payload BLOB);",
        )
        .unwrap();
        // `base` root is Page 3 (`Page 2` = ptrmap).
        // Case (c) & (d): Insert a 2,500-byte overflow row so Page 4 = `Overflow1` and Page 5 = `Overflow2`!
        conn.execute_params(
            "INSERT INTO base VALUES (1, ?1);",
            &[Value::Blob(vec![0xAB; 2_500])],
        )
        .unwrap();
        // Creating `t_ovfl1` at Page 4 relocates `Overflow1`; creating `t_ovfl2` at Page 5 relocates `Overflow2`!
        conn.execute_batch(
            "CREATE TABLE t_ovfl1(x);
             CREATE TABLE t_ovfl2(x);",
        )
        .unwrap();

        // Case (a) & (b): Insert 120 medium rows into `base` to create `BtreeNode` leaf & interior pages,
        // then create 70 tables (crossing `PENDING_BYTE_PAGE = 65`!).
        conn.execute_batch(
            "WITH RECURSIVE c(i) AS (VALUES(2) UNION ALL SELECT i+1 FROM c WHERE i<120)
             INSERT INTO base SELECT i, randomblob(220) FROM c;",
        )
        .unwrap();
        for k in 1..=70 {
            conn.execute(&format!("CREATE TABLE dyn_{k}(a PRIMARY KEY, b);"))
                .unwrap();
        }

        let snap = conn.schema_snapshot().unwrap();
        for tbl in &snap.tables {
            assert_ne!(tbl.root_pgno, 2, "root must never land on ptrmap page 2");
            assert_ne!(
                tbl.root_pgno, 65,
                "root must never land on PENDING_BYTE_PAGE 65"
            );
        }
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );

        // 2. Multi-index `DROP TABLE` (Ticket #1728 double root relocation) + `sqlite_sequence`
        //    relocation + `DROP INDEX` + prepared statement automatic `reprepare`!
        let db2_path = sandbox.path().join("drop_sync.db");
        let conn2 = Connection::open_with_options(
            &db2_path,
            OpenFlags::READWRITE | OpenFlags::CREATE,
            opts,
        )
        .unwrap();
        conn2
            .execute_batch(
                "PRAGMA auto_vacuum = FULL;
                 CREATE TABLE victim(a PRIMARY KEY, b UNIQUE, c);
                 CREATE INDEX victim_c ON victim(c);
                 INSERT INTO victim VALUES ('a1', 'b1', 'c1'), ('a2', 'b2', 'c2');
                 CREATE TABLE seq_owner(id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);
                 INSERT INTO seq_owner(name) VALUES ('first'), ('second');
                 CREATE TABLE tail_tbl(k INTEGER PRIMARY KEY, v TEXT);
                 CREATE INDEX tail_idx ON tail_tbl(v);
                 INSERT INTO tail_tbl VALUES (10, 'ten'), (20, 'twenty');",
            )
            .unwrap();

        // Prepare a V2 statement on `tail_tbl` BEFORE `DROP TABLE victim` relocates `tail_tbl` & `tail_idx`
        // and `sqlite_sequence`!
        let mut prepared_tail = conn2
            .prepare_with_mode("SELECT k, v FROM tail_tbl WHERE v = ?1;", PrepareMode::V2)
            .unwrap();

        let root_before = conn2
            .schema_snapshot()
            .unwrap()
            .tables
            .iter()
            .find(|t| t.name == "tail_tbl")
            .unwrap()
            .root_pgno;
        conn2.execute("DROP TABLE victim;").unwrap();
        let snap_after = conn2.schema_snapshot().unwrap();
        let mem_root_after = snap_after
            .tables
            .iter()
            .find(|t| t.name == "tail_tbl")
            .unwrap()
            .root_pgno;
        let disk_root_after = conn2
            .query(
                "SELECT rootpage FROM sqlite_schema WHERE type='table' AND name='tail_tbl';",
                &[],
            )
            .unwrap()
            .rows[0]
            .get_i64(0)
            .unwrap() as u32;
        assert_eq!(
            mem_root_after, disk_root_after,
            "in-memory Schema and on-disk sqlite_schema rootpage must stay identical after DROP TABLE"
        );
        assert!(
            mem_root_after < root_before,
            "expected tail_tbl root to relocate downward ({root_before} -> {mem_root_after})"
        );

        // Verify `sqlite_sequence` remained intact after relocation and `AUTOINCREMENT` continues at 3!
        conn2
            .execute("INSERT INTO seq_owner(name) VALUES ('third');")
            .unwrap();
        assert_eq!(
            conn2
                .query("SELECT max(id) FROM seq_owner;", &[])
                .unwrap()
                .rows[0]
                .get_i64(0),
            Some(3)
        );

        // Verify `prepared_tail` automatically reprepares against relocated `tail_tbl`/`tail_idx`!
        prepared_tail
            .bind(1, Value::Text("twenty".to_owned()))
            .unwrap();
        match prepared_tail.step().unwrap() {
            StepResult::Row(r) => {
                assert_eq!(r.get_i64(0), Some(20));
                assert_eq!(r.get_str(1), Some("twenty"));
            }
            StepResult::Done => panic!("expected row from reprepared statement"),
        }
        assert!(matches!(prepared_tail.step().unwrap(), StepResult::Done));
        prepared_tail.finalize().unwrap();

        // Also test `DROP INDEX` relocating `seq_owner` / `tail_tbl`!
        conn2.execute("DROP INDEX tail_idx;").unwrap();
        let snap_after_idx = conn2.schema_snapshot().unwrap();
        for tbl in &snap_after_idx.tables {
            let disk_rp = conn2
                .query(
                    "SELECT rootpage FROM sqlite_schema WHERE type='table' AND name=?1;",
                    &[Value::Text(tbl.name.clone())],
                )
                .unwrap()
                .rows[0]
                .get_i64(0)
                .unwrap() as u32;
            assert_eq!(tbl.root_pgno, disk_rp, "mismatch for table {}", tbl.name);
        }
        assert_eq!(
            conn2.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        conn2.try_close().unwrap();
        conn.try_close().unwrap();

        assert_eq!(
            run_c_sql_with_pending(&db_path, 0x10000, "PRAGMA integrity_check;"),
            "ok"
        );
        assert_eq!(
            run_c_sql_with_pending(&db2_path, 0x10000, "PRAGMA integrity_check;"),
            "ok"
        );
    }
}

#[test]
fn group36_pragma_auto_vacuum_streaming_incrvacuum_recovery_and_incrvacuum2() {
    let sandbox = TempSandbox::new("g36_incrvac");
    let db_path = sandbox.path().join("modes.db");

    // 1. Mode transitions: `0` -> `1` via `VACUUM` -> `2` in-place toggle -> `1` in-place toggle -> `0` via `VACUUM`!
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 1024;
             CREATE TABLE m(id INTEGER PRIMARY KEY, body BLOB);
             INSERT INTO m VALUES (1, randomblob(3000)), (2, randomblob(3000));",
        )
        .unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(0)
        );
        // Setting `auto_vacuum = FULL` on an existing non-autovacuum DB requires `VACUUM`.
        conn.execute("PRAGMA auto_vacuum = FULL;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(0)
        );
        conn.execute("VACUUM;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(1)
        );
        assert_eq!(conn.database_header().unwrap().incremental_vacuum_mode, 0);

        // Toggling `FULL (1) <-> INCREMENTAL (2)` on an existing auto-vacuum DB applies immediately!
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        assert_eq!(conn.database_header().unwrap().incremental_vacuum_mode, 1);

        // Populate extra rows & delete them so we can stream `PRAGMA incremental_vacuum(3)`!
        conn.execute_batch(
            "WITH RECURSIVE c(i) AS (VALUES(3) UNION ALL SELECT i+1 FROM c WHERE i<25)
             INSERT INTO m SELECT i, randomblob(1500) FROM c;
             DELETE FROM m WHERE id >= 5;",
        )
        .unwrap();
        let pc_before = conn.page_count().unwrap();

        // Stream 3 zero-column rows from `PRAGMA incremental_vacuum(3)`, running a nested `SELECT`
        // while paused on `StepResult::Row`!
        let mut iv_stmt = conn.prepare("PRAGMA incremental_vacuum(3);").unwrap();
        assert_eq!(iv_stmt.column_names().len(), 0);
        let mut yielded = 0usize;
        while let StepResult::Row(r) = iv_stmt.step().unwrap() {
            assert!(
                r.values().is_empty(),
                "incremental_vacuum row must have 0 columns"
            );
            yielded += 1;
            let cnt = conn.query("SELECT count(*) FROM m;", &[]).unwrap().rows[0].get_i64(0);
            assert_eq!(cnt, Some(4));
        }
        assert_eq!(yielded, 3);
        iv_stmt.finalize().unwrap();
        let pc_mid = conn.page_count().unwrap();
        assert!(
            pc_mid < pc_before,
            "expected 3 tail pages freed ({pc_before} -> {pc_mid})"
        );

        // Verify `SAVEPOINT` + `PRAGMA incremental_vacuum(5)` + `ROLLBACK TO` restores page_count & data!
        conn.execute_batch(
            "SAVEPOINT sp_iv;
             PRAGMA incremental_vacuum(5);
             ROLLBACK TO sp_iv;
             RELEASE sp_iv;",
        )
        .unwrap();
        assert_eq!(conn.page_count().unwrap(), pc_mid);
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );

        // Snapshot pre-compaction 1024-byte DB image for hot-journal crash recovery verification:
        conn.try_close().unwrap();
        let pre_iv_bytes = fs::read(&db_path).unwrap();
        let pre_iv_journal = build_hot_rollback_journal_1k(&pre_iv_bytes);

        let conn = Connection::open(&db_path).unwrap();
        conn.execute("PRAGMA incremental_vacuum;").unwrap();
        let post_iv_pc = conn.page_count().unwrap();
        assert!(post_iv_pc < pc_mid);

        // Simulate crash mid-`incremental_vacuum` leaving truncated/compacted DB + hot journal:
        conn.try_close().unwrap();
        let compacted_bytes = fs::read(&db_path).unwrap();

        let iv_rust_rec = sandbox.path().join("iv_crash_rust.db");
        fs::write(&iv_rust_rec, &compacted_bytes).unwrap();
        fs::write(
            format!("{}-journal", iv_rust_rec.display()),
            &pre_iv_journal,
        )
        .unwrap();
        {
            let rconn = Connection::open(&iv_rust_rec).unwrap();
            assert_eq!(rconn.page_count().unwrap(), pc_mid);
            assert_eq!(
                rconn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
        }

        let iv_c_rec = sandbox.path().join("iv_crash_c.db");
        fs::write(&iv_c_rec, &compacted_bytes).unwrap();
        fs::write(format!("{}-journal", iv_c_rec.display()), &pre_iv_journal).unwrap();
        assert_eq!(run_c_sql(&iv_c_rec, "PRAGMA integrity_check;"), "ok");
        assert_eq!(
            run_c_sql(&iv_c_rec, "PRAGMA page_count;"),
            pc_mid.to_string()
        );

        // Convert back to `NONE (0)` via `VACUUM`!
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA auto_vacuum = NONE; VACUUM;")
            .unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(0)
        );
        assert_eq!(conn.database_header().unwrap().largest_root_btree_page, 0);
        conn.try_close().unwrap();
        assert_eq!(run_c_sql(&db_path, "PRAGMA integrity_check;"), "ok");
    }

    // 1b. Same-handle `BUSY` on `PRAGMA auto_vacuum = 2` -> `PRAGMA auto_vacuum` returns `2` ->
    //     `SELECT * FROM sqlite_schema` refreshes Page 1 even when `change_counter` didn't change
    //     (`btree.c:3453-3454`), reverting `PRAGMA auto_vacuum` to `1` WITHOUT closing `db`, while
    //     `VACUUM` still honors `next_auto_vacuum == 2`!
    {
        let busy_db = sandbox.path().join("same_handle_busy_autovac.db");
        let db1 = Connection::open(&busy_db).unwrap();
        db1.execute_batch(
            "PRAGMA auto_vacuum = FULL;
             CREATE TABLE t1(x);
             INSERT INTO t1 VALUES (1);",
        )
        .unwrap();
        assert_eq!(
            db1.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(1)
        );

        let db2 = Connection::open(&busy_db).unwrap();
        db2.execute("BEGIN EXCLUSIVE;").unwrap();

        let err = db1.execute("PRAGMA auto_vacuum = 2;").unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        db2.execute("ROLLBACK;").unwrap();

        // Before reading Page 1 again, in-memory `configured_auto_vacuum` is `2`:
        assert_eq!(
            db1.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        // Reading Page 1 via `SELECT * FROM sqlite_schema` (with unchanged `change_counter`)
        // refreshes `configured_auto_vacuum` from on-disk Page 1 (`Full = 1`) without closing `db1`:
        let _ = db1.query("SELECT * FROM sqlite_schema;", &[]).unwrap();
        assert_eq!(
            db1.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(1)
        );
        // `VACUUM` still uses `db1.next_auto_vacuum == Some(Incremental)` (`2`):
        db1.execute("VACUUM;").unwrap();
        assert_eq!(
            db1.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        db2.try_close().unwrap();
        db1.try_close().unwrap();
        assert_eq!(run_c_sql(&busy_db, "PRAGMA auto_vacuum;"), "2");
    }

    // 1c. `prepare("PRAGMA auto_vacuum = 2")` -> `expire()` -> `step()` & reset+re-step:
    {
        let exp_db = sandbox.path().join("expire_reprepare_autovac.db");
        let conn = Connection::open(&exp_db).unwrap();
        let mut st = conn.prepare("PRAGMA auto_vacuum = 2;").unwrap();
        // Overwrite `next_auto_vacuum` & `configured_auto_vacuum` to `1` before stepping `st`:
        conn.execute("PRAGMA auto_vacuum = 1;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(1)
        );
        // Expiring `st` forces `reprepare_in_open_tx` on `step()`, re-running prepare-time
        // `apply_prepare_auto_vacuum_pragma` AND `execute_write_body` to apply `Incremental (2)`!
        st.expire();
        assert!(matches!(st.step().unwrap(), StepResult::Done));
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        // Resetting `st`, switching DB to `1`, and re-stepping `st` without `expire()` also sets `2`
        // AND updates `next_auto_vacuum = 2`!
        st.reset().unwrap();
        conn.execute("PRAGMA auto_vacuum = 1;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(1)
        );
        assert!(matches!(st.step().unwrap(), StepResult::Done));
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        st.finalize().unwrap();
        conn.execute("VACUUM;").unwrap();
        assert_eq!(
            conn.query("PRAGMA auto_vacuum;", &[]).unwrap().rows[0].get_i64(0),
            Some(2)
        );
        conn.try_close().unwrap();
    }

    // 2. Execute the complete `incrvacuum2.test` (`1.1..5.12`, 30 subtests including `ifcapable attach` 2.1..2.6 + `0x1000000` + `65536` WAL)
    //    closing `db` immediately before `sqlite3_test_control_pending_byte 0x1000000` per Phase 1.1 `LockGeometry`!
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw_incrvac2 = fs::read_to_string(workspace_root.join("sqlite/test/incrvacuum2.test"))
        .expect("read incrvacuum2.test");
    let patched_incrvac2 = strip_test_comments(&raw_incrvac2).replace(
        "sqlite3_test_control_pending_byte 0x1000000\n  reset_db",
        "db close\n  sqlite3_test_control_pending_byte 0x1000000\n  reset_db",
    );
    let report = evaluate_test_script_in_root("sqlite/test", "incrvacuum2_full", &patched_incrvac2);
    assert!(
        report.is_ok(),
        "incrvacuum2 1.1..5.12 failed: failed={:?} script_error={:?}",
        report.failed,
        report.script_error
    );
    assert_eq!(report.passed, 30);
}

// ============================================================================
// Suite 4 (Group 49): `integrity_check` Ptrmap & Pending-Byte Corruption Detection
// ============================================================================

#[test]
fn group49_integrity_check_ptrmap_and_pending_byte_corruption_detection() {
    let sandbox = TempSandbox::new("g49_corrupt");
    let base_db = sandbox.path().join("clean.db");
    {
        let conn = Connection::open(&base_db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 1024;
             PRAGMA auto_vacuum = FULL;
             CREATE TABLE t(id INTEGER PRIMARY KEY, v BLOB);
             INSERT INTO t VALUES (1, randomblob(2500)), (2, randomblob(2500));",
        )
        .unwrap();
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
    }

    // 1. Corrupt ptrmap `eType` byte for Page 3 (`RootPage = 1` -> `FreePage = 2` at offset 0 of Page 2).
    let bad_type_db = sandbox.path().join("bad_ptrmap_type.db");
    copy_db_file(&base_db, &bad_type_db);
    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&bad_type_db)
            .unwrap();
        f.seek(SeekFrom::Start(1024)).unwrap(); // Page 2 offset 0 = ptrmap entry for Page 3
        f.write_all(&[PtrmapType::FreePage as u8]).unwrap();
    }
    {
        let conn = Connection::open(&bad_type_db).unwrap();
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        let msg = ic.rows[0].get_str(0).unwrap_or("");
        assert_ne!(
            msg, "ok",
            "Rust integrity_check must detect corrupt ptrmap type"
        );
    }
    assert_ne!(
        run_c_sql(&bad_type_db, "PRAGMA integrity_check;"),
        "ok",
        "C SQLite integrity_check must also detect corrupt ptrmap type"
    );

    // 2. Corrupt ptrmap `parent_pgno` for Page 4 (`Overflow1` parent `3` -> `99` at offset 5 of Page 2).
    let bad_parent_db = sandbox.path().join("bad_ptrmap_parent.db");
    copy_db_file(&base_db, &bad_parent_db);
    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&bad_parent_db)
            .unwrap();
        f.seek(SeekFrom::Start(1024 + 5 + 1)).unwrap(); // Page 2 offset 5 + 1 = parent u32 of Page 4
        f.write_all(&99u32.to_be_bytes()).unwrap();
    }
    {
        let conn = Connection::open(&bad_parent_db).unwrap();
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_ne!(ic.rows[0].get_str(0), Some("ok"));
    }
    assert_ne!(run_c_sql(&bad_parent_db, "PRAGMA integrity_check;"), "ok");

    // 3. Forbidden reference to ptrmap page (`Page 2` placed on freelist header -> `"pointer map referenced"`).
    let ref_ptrmap_db = sandbox.path().join("ref_ptrmap.db");
    copy_db_file(&base_db, &ref_ptrmap_db);
    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&ref_ptrmap_db)
            .unwrap();
        let mut pg1 = [0u8; 1024];
        f.read_exact(&mut pg1).unwrap();
        pg1[32..36].copy_from_slice(&2u32.to_be_bytes()); // first_freelist_trunk_page = Page 2 (ptrmap!)
        pg1[36..40].copy_from_slice(&1u32.to_be_bytes());
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&pg1).unwrap();
    }
    {
        let conn = Connection::open(&ref_ptrmap_db).unwrap();
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        let joined = ic
            .rows
            .iter()
            .filter_map(|r| r.get_str(0))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("pointer map referenced"),
            "expected 'pointer map referenced' diagnostic, got: {joined}"
        );
    }
    assert_ne!(run_c_sql(&ref_ptrmap_db, "PRAGMA integrity_check;"), "ok");

    // 4. Forbidden reference to `PENDING_BYTE_PAGE` (`Page 65` under `TEST_64K` and `Page 1_048_577` under `PRODUCTION`).
    let pending64_db = sandbox.path().join("ref_pending64.db");
    let opts64 = ConnectionOpenOptions {
        default_page_size: 1024,
        lock_geometry: LockGeometry::TEST_64K,
        ..ConnectionOpenOptions::default()
    };
    {
        let conn = Connection::open_with_options(
            &pending64_db,
            OpenFlags::READWRITE | OpenFlags::CREATE,
            opts64,
        )
        .unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = INCREMENTAL;
             CREATE TABLE p(id INTEGER PRIMARY KEY, b BLOB);
             WITH RECURSIVE c(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM c WHERE i<80)
             INSERT INTO p SELECT i, randomblob(900) FROM c;",
        )
        .unwrap();
        conn.try_close().unwrap();
    }
    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pending64_db)
            .unwrap();
        let mut pg1 = [0u8; 1024];
        f.read_exact(&mut pg1).unwrap();
        pg1[32..36].copy_from_slice(&65u32.to_be_bytes()); // point freelist trunk at PENDING_BYTE_PAGE = 65!
        pg1[36..40].copy_from_slice(&1u32.to_be_bytes());
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&pg1).unwrap();
    }
    {
        let conn =
            Connection::open_with_options(&pending64_db, OpenFlags::READONLY, opts64).unwrap();
        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_ne!(
            ic.rows[0].get_str(0),
            Some("ok"),
            "reference to 64 KiB PENDING_BYTE_PAGE (65) must fail integrity_check"
        );
    }
    assert_ne!(
        run_c_sql_with_pending(&pending64_db, 0x10000, "PRAGMA integrity_check;"),
        "ok"
    );

    // Also verify forbidden reference to `PENDING_BYTE_PAGE = 1_048_577` under `1 GiB` (`PRODUCTION`)!
    {
        let guard = SparseDbGuard::new("ref_pending_1gib");
        let (first_trunk, total_free) = prepare_sparse_pre_1gib_base(guard.path());
        {
            let conn = Connection::open(guard.path()).unwrap();
            conn.execute_batch(
                "PRAGMA synchronous = OFF;
                 INSERT INTO items VALUES (1, 'p1', zeroblob(3200)), (2, 'p2', zeroblob(3200));",
            )
            .unwrap();
            conn.try_close().unwrap();
        }
        reattach_sparse_freelist(guard.path(), first_trunk, total_free);
        assert_eq!(run_c_sql(guard.path(), "PRAGMA integrity_check;"), "ok");

        // Corrupt first freelist trunk leaf entry (`offset 8` of `first_trunk`) to point at `1_048_577`!
        {
            let mut f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(guard.path())
                .unwrap();
            f.seek(SeekFrom::Start(u64::from(first_trunk - 1) * 1024 + 8))
                .unwrap();
            f.write_all(&1_048_577u32.to_be_bytes()).unwrap();
        }
        {
            let conn = Connection::open_with_flags(guard.path(), OpenFlags::READONLY).unwrap();
            let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
            assert_ne!(
                ic.rows[0].get_str(0),
                Some("ok"),
                "reference to 1 GiB PENDING_BYTE_PAGE (1_048_577) must fail Rust integrity_check"
            );
        }
        assert_ne!(
            run_c_sql(guard.path(), "PRAGMA integrity_check;"),
            "ok",
            "reference to 1 GiB PENDING_BYTE_PAGE (1_048_577) must fail C SQLite integrity_check"
        );
    }
}

// ============================================================================
// Suite 5 (Group 51): `VACUUM [schema] [INTO expr]` & Rowid / Index Semantics
// ============================================================================

#[test]
fn group51_vacuum_ast_into_modes_errors_and_rowid_semantics() {
    let sandbox = TempSandbox::new("g51_vacuum");
    let src_db = sandbox.path().join("src.db");

    // 1. Create source DB with:
    //    - `ipk_tbl` (explicit `INTEGER PRIMARY KEY` with gapped/negative rowids `-50, 10, 999999` + secondary index)
    //    - `imp_tbl` (implicit rowid table with gaps after `DELETE` + `UNIQUE` & composite secondary indexes)
    //    - `wr_tbl` (`WITHOUT ROWID` table + secondary index)
    //    - `cfg` table storing output path for scalar-subquery `VACUUM INTO (SELECT ...)`
    let out_subquery_db = sandbox.path().join("out_subquery.db");
    {
        let conn = Connection::open(&src_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE ipk_tbl(id INTEGER PRIMARY KEY, code TEXT);
             CREATE INDEX idx_ipk_code ON ipk_tbl(code);
             INSERT INTO ipk_tbl VALUES (-50, 'neg'), (10, 'ten'), (999999, 'big');

             CREATE TABLE imp_tbl(k TEXT UNIQUE, n INT);
             CREATE INDEX idx_imp_n_k ON imp_tbl(n, k);
             INSERT INTO imp_tbl VALUES ('a', 100), ('b', 200), ('c', 300), ('d', 400), ('e', 500);
             DELETE FROM imp_tbl WHERE k IN ('b', 'd');

             CREATE TABLE wr_tbl(k1 TEXT, k2 INT, val TEXT, PRIMARY KEY(k1, k2)) WITHOUT ROWID;
             CREATE INDEX idx_wr_val ON wr_tbl(val);
             INSERT INTO wr_tbl VALUES ('x', 1, 'one'), ('y', 2, 'two');

             CREATE TABLE cfg(out_path TEXT);",
        )
        .unwrap();
        conn.execute_params(
            "INSERT INTO cfg VALUES (?1);",
            &[Value::Text(out_subquery_db.to_string_lossy().into_owned())],
        )
        .unwrap();

        // Schema variants & prepare/runtime error checks:
        conn.execute("VACUUM temp;").unwrap(); // valid no-op
        let err_nosuch = conn.execute("VACUUM nosuchdb;").unwrap_err();
        assert!(err_nosuch.message.contains("unknown database nosuchdb"));
        let err_into_as_schema = conn.execute("VACUUM into;").unwrap_err();
        assert!(err_into_as_schema.message.contains("unknown database into"));

        // Prepare-time validation of `INTO <expr>`:
        let err_col = conn
            .prepare("VACUUM INTO nosuchcol;")
            .expect_err("expected prepare err");
        assert!(err_col.message.contains("no such column"));
        let err_fn = conn
            .prepare("VACUUM INTO nosuchfunc();")
            .expect_err("expected prepare err");
        assert!(err_fn.message.contains("no such function"));

        // Runtime non-text filename errors:
        for bad_sql in [
            "VACUUM INTO NULL;",
            "VACUUM INTO 12345;",
            "VACUUM INTO 3.25;",
            "VACUUM INTO X'CAFE';",
        ] {
            let e = conn.execute(bad_sql).unwrap_err();
            assert_eq!(e.code, ErrorCode::Error);
            assert!(
                e.message.contains("non-text filename"),
                "expected non-text filename for `{bad_sql}`, got: {}",
                e.message
            );
        }

        // `"output file already exists"` without overwriting existing bytes!
        let pre_existing = sandbox.path().join("already_there.db");
        fs::write(&pre_existing, b"DO_NOT_TOUCH_ME").unwrap();
        let e_exists = conn
            .execute_params(
                "VACUUM main INTO ?1;",
                &[Value::Text(pre_existing.to_string_lossy().into_owned())],
            )
            .unwrap_err();
        assert!(e_exists.message.contains("output file already exists"));
        assert_eq!(fs::read(&pre_existing).unwrap(), b"DO_NOT_TOUCH_ME");

        // `"cannot VACUUM from within a transaction"` & `"cannot VACUUM - SQL statements in progress"`:
        conn.execute("BEGIN;").unwrap();
        let e_tx = conn.execute("VACUUM;").unwrap_err();
        assert!(
            e_tx.message
                .contains("cannot VACUUM from within a transaction")
        );
        conn.execute("ROLLBACK;").unwrap();

        let mut active_sel = conn.prepare("SELECT id FROM ipk_tbl;").unwrap();
        assert!(matches!(active_sel.step().unwrap(), StepResult::Row(_)));
        let e_in_prog = conn.execute("VACUUM;").unwrap_err();
        assert!(e_in_prog.message.contains("SQL statements in progress"));
        active_sel.finalize().unwrap();

        // Scalar subquery `VACUUM main INTO (SELECT out_path FROM cfg)`:
        conn.execute("VACUUM main INTO (SELECT out_path FROM cfg);")
            .unwrap();
        assert_eq!(run_c_sql(&out_subquery_db, "PRAGMA integrity_check;"), "ok");

        // In-place `VACUUM main`: verify explicit IPK rowids preserved (`-50, 10, 999999`) AND the
        // implicit rowids of a table WITH indexes kept as they are (`1, 3, 5`): C SQLite renumbers
        // the rows only of a table that has neither an INTEGER PRIMARY KEY nor an index
        // (`xferOptimization`: `OP_NewRowid` when `pDest->pIndex==0`).
        conn.execute("VACUUM main;").unwrap();
        let ipk_rids: Vec<i64> = conn
            .query("SELECT rowid FROM ipk_tbl ORDER BY rowid;", &[])
            .unwrap()
            .rows
            .iter()
            .map(|r| r.get_i64(0).unwrap())
            .collect();
        assert_eq!(ipk_rids, vec![-50, 10, 999999]);

        let imp_rids: Vec<(i64, String)> = conn
            .query("SELECT rowid, k FROM imp_tbl ORDER BY rowid;", &[])
            .unwrap()
            .rows
            .iter()
            .map(|r| (r.get_i64(0).unwrap(), r.get_str(1).unwrap().to_owned()))
            .collect();
        assert_eq!(
            imp_rids,
            vec![
                (1, "a".to_owned()),
                (3, "c".to_owned()),
                (5, "e".to_owned())
            ]
        );
        // Index-ordered lookup on the kept implicit rowids:
        let idx_hit = conn
            .query(
                "SELECT rowid, k FROM imp_tbl WHERE n = 500 AND k = 'e';",
                &[],
            )
            .unwrap();
        assert_eq!(idx_hit.rows[0].get_i64(0), Some(5));
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        conn.try_close().unwrap();
    }
    assert_eq!(run_c_sql(&src_db, "PRAGMA integrity_check;"), "ok");

    // 2. `VACUUM INTO` from a `READONLY` connection:
    let out_from_ro = sandbox.path().join("out_from_ro.db");
    {
        let ro_conn = Connection::open_with_flags(&src_db, OpenFlags::READONLY).unwrap();
        ro_conn
            .execute_params(
                "VACUUM INTO ?1;",
                &[Value::Text(out_from_ro.to_string_lossy().into_owned())],
            )
            .unwrap();
        ro_conn.try_close().unwrap();
    }
    assert_eq!(run_c_sql(&out_from_ro, "PRAGMA integrity_check;"), "ok");

    // 3. `VACUUM INTO` from `:memory:`:
    let out_from_mem = sandbox.path().join("out_from_mem.db");
    {
        let mem_conn = Connection::open_in_memory().unwrap();
        mem_conn
            .execute_batch("CREATE TABLE mem_t(a, b); INSERT INTO mem_t VALUES (7, 'seven');")
            .unwrap();
        mem_conn
            .execute_params(
                "VACUUM INTO ?1;",
                &[Value::Text(out_from_mem.to_string_lossy().into_owned())],
            )
            .unwrap();
    }
    assert_eq!(run_c_sql(&out_from_mem, "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sql(&out_from_mem, "SELECT a, b FROM mem_t;"),
        "7|seven"
    );

    // 4. `VACUUM INTO` from a named database opened on a custom `MemoryVfs` routes target creation
    //    into that `MemoryVfs` without touching the OS filesystem!
    {
        let mem_vfs = Arc::new(MemoryVfs::new());
        let vfs_dyn: Arc<dyn Vfs> = mem_vfs.clone();
        let src_vpath = sandbox.path().join("custom_vfs_src.db");
        let dst_vpath = sandbox.path().join("custom_vfs_dst.db");
        let vconn = Connection::open_with_vfs(
            Arc::clone(&vfs_dyn),
            Some(&src_vpath),
            OpenFlags::READWRITE | OpenFlags::CREATE,
        )
        .unwrap();
        vconn
            .execute_batch(
                "PRAGMA auto_vacuum = FULL;
                 CREATE TABLE vt(k INTEGER PRIMARY KEY, val TEXT);
                 INSERT INTO vt VALUES (1, 'in-mem-vfs'), (2, 'second');",
            )
            .unwrap();
        vconn
            .execute_params(
                "VACUUM INTO ?1;",
                &[Value::Text(dst_vpath.to_string_lossy().into_owned())],
            )
            .unwrap();

        // OS filesystem must NOT have either file; `mem_vfs` MUST have `dst_vpath`!
        assert!(
            !src_vpath.exists() && !dst_vpath.exists(),
            "custom MemoryVfs VACUUM INTO must not touch OS filesystem"
        );
        assert!(mem_vfs.exists(&dst_vpath).unwrap());

        // Second `VACUUM INTO` targeting non-empty `dst_vpath` in `mem_vfs` must fail cleanly:
        let dup_err = vconn
            .execute_params(
                "VACUUM INTO ?1;",
                &[Value::Text(dst_vpath.to_string_lossy().into_owned())],
            )
            .unwrap_err();
        assert!(dup_err.message.contains("output file already exists"));

        let dst_conn =
            Connection::open_with_vfs(vfs_dyn, Some(&dst_vpath), OpenFlags::READONLY).unwrap();
        assert_eq!(
            dst_conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        assert_eq!(
            dst_conn
                .query("SELECT val FROM vt WHERE k = 1;", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("in-mem-vfs")
        );
    }
}

// ============================================================================
// Suite 6 (Group 56): `PRAGMA page_size = N; VACUUM` Geometry Transitions,
// Old-Geometry Hot-Journal Crash Recovery, WAL/`:memory:`, & Orphaned `sqlite_sequence`
// ============================================================================

#[test]
fn group56_page_size_transitions_hot_journal_recovery_and_orphaned_sequence() {
    let sandbox = TempSandbox::new("g56_pagesize");

    // 1. Page-size transitions `512 -> 1024 -> 4096 -> 8192 -> 65536 -> 512` across `TEST_64K`
    //    pending-byte boundary (`PENDING_BYTE_PAGE` shifts `129 -> 65 -> 17 -> 9 -> 2 -> 129`!)
    //    with `auto_vacuum = FULL`, verified at EVERY step by C SQLite 3.54.1!
    let ps_db = sandbox.path().join("ps_matrix.db");
    let opts64 = ConnectionOpenOptions {
        default_page_size: 512,
        lock_geometry: LockGeometry::TEST_64K,
        ..ConnectionOpenOptions::default()
    };
    {
        let conn =
            Connection::open_with_options(&ps_db, OpenFlags::READWRITE | OpenFlags::CREATE, opts64)
                .unwrap();
        conn.execute_batch(
            "PRAGMA auto_vacuum = FULL;
             CREATE TABLE docs(id INTEGER PRIMARY KEY, title TEXT UNIQUE, body BLOB);
             WITH RECURSIVE c(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM c WHERE i<140)
             INSERT INTO docs SELECT i, 'doc-' || i, zeroblob(600) FROM c;",
        )
        .unwrap();
        assert_eq!(conn.page_size().unwrap(), 512);

        for &target_ps in &[1024u32, 4096, 8192, 65536, 512] {
            conn.execute_batch(&format!("PRAGMA page_size = {target_ps}; VACUUM;"))
                .unwrap();
            assert_eq!(conn.page_size().unwrap(), target_ps);
            assert_eq!(
                conn.query("PRAGMA page_size;", &[]).unwrap().rows[0].get_i64(0),
                Some(i64::from(target_ps))
            );
            assert_eq!(
                conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
                Some("ok")
            );
            assert_eq!(
                conn.query("SELECT count(*) FROM docs;", &[]).unwrap().rows[0].get_i64(0),
                Some(140)
            );
            assert_eq!(
                run_c_sql_with_pending(&ps_db, 0x10000, "PRAGMA integrity_check;"),
                "ok"
            );
        }
        conn.try_close().unwrap();
    }

    // 2. Simulated mid-commit crash during `PRAGMA page_size = 4096; VACUUM;` leaving a hot
    //    rollback journal in the OLD `1024`-byte geometry while `.db` already contains partial/new
    //    `4096`-byte pages -> BOTH Rust AND C SQLite 3.54.1 must recover the intact `1024`-byte DB!
    let orig_1k_db = sandbox.path().join("crash_orig_1k.db");
    {
        let conn = Connection::open(&orig_1k_db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 1024;
             PRAGMA auto_vacuum = FULL;
             CREATE TABLE crash_t(id INTEGER PRIMARY KEY, msg TEXT);
             INSERT INTO crash_t VALUES (1, 'alpha'), (2, 'bravo'), (3, 'charlie');",
        )
        .unwrap();
        conn.try_close().unwrap();
    }
    let orig_bytes = fs::read(&orig_1k_db).unwrap();
    let orig_page_count = (orig_bytes.len() / 1024) as u32;
    assert!(orig_page_count >= 3);
    let j_bytes = build_hot_rollback_journal_1k(&orig_bytes);

    // Produce a 4096-byte vacuumed image to simulate crashed `.db` overwritten mid-commit:
    let crashed_4k_template = sandbox.path().join("template_4k.db");
    copy_db_file(&orig_1k_db, &crashed_4k_template);
    {
        let conn = Connection::open(&crashed_4k_template).unwrap();
        conn.execute_batch(
            "DELETE FROM crash_t WHERE id = 2;
             PRAGMA page_size = 4096;
             VACUUM;",
        )
        .unwrap();
        conn.try_close().unwrap();
    }
    let overwritten_4k_bytes = fs::read(&crashed_4k_template).unwrap();

    // Recover #1 via Rust `Connection`:
    let rust_rec_db = sandbox.path().join("rust_rec.db");
    fs::write(&rust_rec_db, &overwritten_4k_bytes).unwrap();
    fs::write(format!("{}-journal", rust_rec_db.display()), &j_bytes).unwrap();
    {
        let conn = Connection::open(&rust_rec_db).unwrap();
        assert_eq!(conn.page_size().unwrap(), 1024);
        assert_eq!(
            conn.query("PRAGMA integrity_check;", &[]).unwrap().rows[0].get_str(0),
            Some("ok")
        );
        let rows = conn
            .query("SELECT id, msg FROM crash_t ORDER BY id;", &[])
            .unwrap();
        assert_eq!(rows.rows.len(), 3);
        assert_eq!(rows.rows[1].get_str(1), Some("bravo"));
    }

    // Recover #2 via C SQLite 3.54.1:
    let c_rec_db = sandbox.path().join("c_rec.db");
    fs::write(&c_rec_db, &overwritten_4k_bytes).unwrap();
    fs::write(format!("{}-journal", c_rec_db.display()), &j_bytes).unwrap();
    assert_eq!(run_c_sql(&c_rec_db, "PRAGMA integrity_check;"), "ok");
    assert_eq!(run_c_sql(&c_rec_db, "PRAGMA page_size;"), "1024");
    assert_eq!(run_c_sql(&c_rec_db, "SELECT count(*) FROM crash_t;"), "3");

    // 3. WAL in-place `VACUUM` keeps `page_size`, whereas `VACUUM INTO` from WAL honors `PRAGMA page_size`!
    let wal_db = sandbox.path().join("wal_ps.db");
    let wal_into_db = sandbox.path().join("wal_into_8k.db");
    {
        let conn = Connection::open(&wal_db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 1024;
             PRAGMA journal_mode = WAL;
             CREATE TABLE w(x);
             INSERT INTO w VALUES ('wal-row');
             PRAGMA page_size = 8192;
             VACUUM;",
        )
        .unwrap();
        assert_eq!(
            conn.page_size().unwrap(),
            1024,
            "in-place VACUUM in WAL mode must preserve current page_size"
        );
        conn.execute("PRAGMA page_size = 8192;").unwrap();
        conn.execute_params(
            "VACUUM INTO ?1;",
            &[Value::Text(wal_into_db.to_string_lossy().into_owned())],
        )
        .unwrap();
        conn.try_close().unwrap();
    }
    assert_eq!(run_c_sql(&wal_into_db, "PRAGMA page_size;"), "8192");
    assert_eq!(run_c_sql(&wal_into_db, "PRAGMA journal_mode;"), "delete");
    assert_eq!(run_c_sql(&wal_into_db, "PRAGMA integrity_check;"), "ok");

    // 4. `:memory:` keeps `page_size` across `VACUUM` once initialized:
    {
        let mem = Connection::open_in_memory().unwrap();
        mem.execute_batch(
            "PRAGMA page_size = 2048;
             CREATE TABLE mt(x);
             INSERT INTO mt VALUES (1);
             PRAGMA page_size = 8192;
             VACUUM;",
        )
        .unwrap();
        assert_eq!(mem.page_size().unwrap(), 2048);
    }

    // 5. Orphaned `sqlite_sequence` omitted after dropping last `AUTOINCREMENT` table + `VACUUM` (`vacuum2-1.1, 3.1`):
    let seq_db = sandbox.path().join("orphan_seq.db");
    {
        let conn = Connection::open(&seq_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE a1(id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT);
             INSERT INTO a1(v) VALUES ('one');
             CREATE TABLE keep_t(x TEXT);
             INSERT INTO keep_t VALUES ('stay');
             DROP TABLE a1;",
        )
        .unwrap();
        // Before `VACUUM`, `sqlite_sequence` still exists in `sqlite_schema`:
        assert_eq!(
            conn.query(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'sqlite_sequence';",
                &[]
            )
            .unwrap()
            .rows[0]
                .get_i64(0),
            Some(1)
        );
        conn.execute("VACUUM;").unwrap();
        assert_eq!(
            conn.query(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'sqlite_sequence';",
                &[]
            )
            .unwrap()
            .rows[0]
                .get_i64(0),
            Some(0),
            "VACUUM must omit orphaned sqlite_sequence when no AUTOINCREMENT table remains"
        );
        conn.try_close().unwrap();
    }
    assert_eq!(run_c_sql(&seq_db, "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sql(
            &seq_db,
            "SELECT count(*) FROM sqlite_schema WHERE name = 'sqlite_sequence';",
        ),
        "0"
    );

    // Also confirm `run_c_sql_raw_result` rejects corrupt page cleanly (used to keep helper dead-code-free):
    let (ok_clean, out_clean, _) = run_c_sql_raw_result(&seq_db, 0, "SELECT x FROM keep_t;");
    assert!(ok_clean);
    assert_eq!(out_clean, "stay");
}
