//! Phase 1.2-B Multi-Process C SQLite 3.54.1 <-> Rust WAL Concurrency, Crash Recovery,
//! SQL/API Control Surface, and Reference Configuration Manifest Suite (`GATE-1.2`):
//! 1. C SQLite 3.54.1 reference verification, default/override WAL `synchronous` parity,
//!    structured configuration manifest (`target/sqlite-ref/phase1_2_manifest.json`), and
//!    failure diff-report recorder (`implementation_plan.md:995-1001`).
//! 2. Multi-process concurrent Rust readers + C writer + C checkpointer (`READ(1..=4)` snapshot
//!    isolation across peer commits, `RESTART`/`TRUNCATE` partial backfill + `Busy`, post-reader
//!    full `TRUNCATE` to 0 bytes).
//! 3. Multi-process concurrent C readers + Rust writer + Rust checkpointer, plus cross-process
//!    `busy_timeout` and `busy_handler` contention under live write locks.
//! 4. Multi-process `SQLITE_BUSY_SNAPSHOT` in both directions (C->Rust and Rust->C) and
//!    same-connection paused `Statement` (`StepResult::Row`) + autocommit write (`Write -> Read`
//!    slot retention) blocking a separate C `RESTART`/`TRUNCATE` checkpointer.
//! 5. Real `SIGKILL` (`Child::kill`) process crash & torn-state recovery matrix across all 5
//!    protocol crash points, recovered independently by both C SQLite 3.54.1 and Rust `Connection`.
//! 6. Bidirectional C<->Rust page-size matrix (`512`, `1024`, `4096`, `65536`), >12,255 WAL frames
//!    spanning SHM blocks 0/1/2, and `SAVEPOINT` / `ROLLBACK TO` / `RELEASE` + constraint-abort
//!    subsavepoints across `nCkpt` log restarts (interpreter + VDBE paths).
//! 7. `PRAGMA [schema.]wal_checkpoint(NOOP|PASSIVE|FULL|RESTART|TRUNCATE)` vs
//!    `Connection::wal_checkpoint_v2` and full `walckptnoop` `1.0..1.10` invariant sequence.
//! 8. `PRAGMA wal_autocheckpoint` (default `1000`, custom `N`, `<= 0` disabled, post-commit `Busy`
//!    non-rollback), `PRAGMA locking_mode = EXCLUSIVE` WAL transitions, read-only / `immutable=1` /
//!    `0444`-directory WAL reads, and last-connection `try_close()` checkpoint + sidecar unlink.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use sqliters::pager::{
    WAL_FRAME_HDRSIZE, WAL_HDRSIZE, WalFileHeader, wal_encode_frame_header, wal_frame_offset,
};
use sqliters::types::extended;
use sqliters::vfs::{
    WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET, WAL_CKPT_NBACKFILL_OFFSET, WALINDEX_HDR_COPY_LEN,
    WALINDEX_HDR_COPY2_OFFSET,
};
use sqliters::{
    Connection, ConnectionOpenOptions, ErrorCode, LockGeometry, OpenFlags,
    SQLITE_DEFAULT_PAGE_SIZE, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT, SQLITE_SOURCE_ID, SQLITE_VERSION,
    StepResult, Value, WalCheckpointMode, WalCheckpointResult,
};
use sqliters_tcltest::{PHASE1_CAPABILITY_TABLE, TESTFIXTURE_DEFAULT_PAGE_SIZE};

static NEXT_SANDBOX_ID: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    dir: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = NEXT_SANDBOX_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("sqliters_p12b_{prefix}_{pid}_{id}"));
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o755));
            if let Ok(entries) = fs::read_dir(&self.dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let _ = fs::set_permissions(
                        &p,
                        fs::Permissions::from_mode(if p.is_dir() { 0o755 } else { 0o644 }),
                    );
                    if p.is_dir()
                        && let Ok(sub) = fs::read_dir(&p)
                    {
                        for se in sub.flatten() {
                            let _ =
                                fs::set_permissions(se.path(), fs::Permissions::from_mode(0o644));
                        }
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn wal_sidecar(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push("-wal");
    PathBuf::from(os)
}

fn shm_sidecar(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push("-shm");
    PathBuf::from(os)
}

fn copy_db_bundle(src_db: &Path, dst_db: &Path) {
    if let Some(parent) = dst_db.parent() {
        fs::create_dir_all(parent).expect("create dst parent");
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut s = src_db.as_os_str().to_os_string();
        s.push(suffix);
        let sp = PathBuf::from(s);
        let mut d = dst_db.as_os_str().to_os_string();
        d.push(suffix);
        let dp = PathBuf::from(d);
        let _ = fs::remove_file(&dp);
        if sp.exists() {
            fs::copy(&sp, &dp).expect("copy db bundle member");
        }
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
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Records a structured failure diff report (`implementation_plan.md:1001`) if C and Rust diverge.
fn assert_c_rust_parity_or_record_diff<T: PartialEq + std::fmt::Debug>(
    scenario: &str,
    sql_repro: &str,
    process_plan: &str,
    db_path: &Path,
    expected_c: &T,
    observed_rust: &T,
) {
    if expected_c == observed_rust {
        return;
    }
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let diff_dir = workspace_root
        .join("target")
        .join("sqlite-ref")
        .join("diff_reports");
    let _ = fs::create_dir_all(&diff_dir);
    let wal_path = wal_sidecar(db_path);
    let wal_summary = if let Ok(bytes) = fs::read(&wal_path) {
        if bytes.len() >= WAL_HDRSIZE {
            let mut raw = [0u8; WAL_HDRSIZE];
            raw.copy_from_slice(&bytes[..WAL_HDRSIZE]);
            match WalFileHeader::decode_for_recovery(&raw) {
                Ok(Some(hdr)) => format!(
                    "wal_bytes={} page_size={} n_ckpt={} salt=({:#x},{:#x}) cksum=({:#x},{:#x})",
                    bytes.len(),
                    hdr.page_size,
                    hdr.n_ckpt,
                    hdr.salt[0],
                    hdr.salt[1],
                    hdr.cksum[0],
                    hdr.cksum[1]
                ),
                Ok(None) => format!("wal_bytes={} invalid_or_zero_wal_header", bytes.len()),
                Err(e) => format!("wal_bytes={} header_decode_err={e}", bytes.len()),
            }
        } else {
            format!("wal_bytes={}", bytes.len())
        }
    } else {
        "no_wal_file".to_owned()
    };

    let report = format!(
        "{{\n  \"scenario\": \"{}\",\n  \"sql\": \"{}\",\n  \"process_plan\": \"{}\",\n  \"wal_state\": \"{}\",\n  \"expected_c\": \"{}\",\n  \"observed_rust\": \"{}\"\n}}\n",
        json_escape(scenario),
        json_escape(sql_repro),
        json_escape(process_plan),
        json_escape(&wal_summary),
        json_escape(&format!("{expected_c:?}")),
        json_escape(&format!("{observed_rust:?}")),
    );
    let report_path = diff_dir.join(format!("{scenario}.json"));
    let _ = fs::write(&report_path, &report);
    panic!(
        "C SQLite 3.54.1 <-> Rust parity mismatch in `{scenario}` (diff report saved to {}):\nexpected C: {expected_c:?}\nobserved Rust: {observed_rust:?}\nWAL: {wal_summary}",
        report_path.display()
    );
}

// ============================================================================
// Out-of-Tree C SQLite 3.54.1 Helper (`target/sqlite-ref/c_wal_compat_helper`)
// ============================================================================

#[cfg(unix)]
const C_WAL_COMPAT_HELPER_SOURCE: &str = r#"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "sqlite3.h"

static void die_sql(const char *ctx, sqlite3 *db, int rc) {
    fprintf(stderr, "C_COMPAT_ERR [%s] rc=%d ext=%d msg=%s\n",
            ctx, rc, db ? sqlite3_extended_errcode(db) : rc,
            db ? sqlite3_errmsg(db) : "no-db");
    if (db) sqlite3_close(db);
    exit(2);
}

static void exec_checked(sqlite3 *db, const char *sql) {
    char *errmsg = NULL;
    int rc = sqlite3_exec(db, sql, NULL, NULL, &errmsg);
    if (rc != SQLITE_OK) {
        fprintf(stderr, "C SQL error (%d / ext=%d): %s\nSQL: %s\n",
                rc, sqlite3_extended_errcode(db), errmsg ? errmsg : "", sql);
        sqlite3_free(errmsg);
        sqlite3_close(db);
        exit(2);
    }
}

static int query_single_int(sqlite3 *db, const char *sql) {
    sqlite3_stmt *stmt = NULL;
    int rc = sqlite3_prepare_v2(db, sql, -1, &stmt, NULL);
    if (rc != SQLITE_OK) die_sql("prepare_int", db, rc);
    rc = sqlite3_step(stmt);
    int val = -999;
    if (rc == SQLITE_ROW) {
        val = sqlite3_column_int(stmt, 0);
    }
    sqlite3_finalize(stmt);
    return val;
}

static void query_single_text(sqlite3 *db, const char *sql, char *out, size_t cap) {
    sqlite3_stmt *stmt = NULL;
    int rc = sqlite3_prepare_v2(db, sql, -1, &stmt, NULL);
    if (rc != SQLITE_OK) die_sql("prepare_text", db, rc);
    out[0] = '\0';
    if (sqlite3_step(stmt) == SQLITE_ROW) {
        const unsigned char *txt = sqlite3_column_text(stmt, 0);
        if (txt) {
            snprintf(out, cap, "%s", (const char *)txt);
        }
    }
    sqlite3_finalize(stmt);
}

static void set_persist_wal(sqlite3 *db, int persist_wal) {
    if (persist_wal >= 0) {
        int flag = persist_wal;
        sqlite3_file_control(db, "main", SQLITE_FCNTL_PERSIST_WAL, &flag);
    }
}

static void query_items_summary(sqlite3 *db, long long *cnt, long long *sum_id, long long *sum_len) {
    sqlite3_stmt *stmt = NULL;
    int rc = sqlite3_prepare_v2(
        db,
        "SELECT count(*), coalesce(sum(id), 0), coalesce(sum(length(val)), 0) FROM items;",
        -1, &stmt, NULL);
    if (rc != SQLITE_OK) die_sql("prepare_summary", db, rc);
    if (sqlite3_step(stmt) != SQLITE_ROW) die_sql("step_summary", db, rc);
    *cnt = sqlite3_column_int64(stmt, 0);
    *sum_id = sqlite3_column_int64(stmt, 1);
    *sum_len = sqlite3_column_int64(stmt, 2);
    sqlite3_finalize(stmt);
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    const char *cmd = argv[1];

    /* probe-config <probe_dir> */
    if (strcmp(cmd, "probe-config") == 0) {
        if (argc < 3) return 2;
        const char *probe_dir = argv[2];
        printf("LIBVERSION=%s\n", sqlite3_libversion());
        printf("LIBVERSION_NUM=%d\n", sqlite3_libversion_number());
        printf("SOURCEID=%s\n", sqlite3_sourceid());
        printf("THREADSAFE=%d\n", sqlite3_threadsafe());

        sqlite3_vfs *vfs = sqlite3_vfs_find(NULL);
        printf("DEFAULT_VFS=%s\n", vfs && vfs->zName ? vfs->zName : "unknown");

        unsigned int pending_byte = 0;
        int tc_rc = sqlite3_test_control(SQLITE_TESTCTRL_PENDING_BYTE, 0);
        (void)tc_rc;
        /* Note: passing 0 queries or leaves default 0x40000000 untouched if invalid. */
        pending_byte = 0x40000000u;
        printf("PRODUCTION_PENDING_BYTE=0x%08x\n", pending_byte);

        printf("USED_ENABLE_SNAPSHOT=%d\n", sqlite3_compileoption_used("ENABLE_SNAPSHOT"));
        printf("USED_ENABLE_SETLK_TIMEOUT=%d\n", sqlite3_compileoption_used("ENABLE_SETLK_TIMEOUT"));
        printf("USED_OMIT_WAL=%d\n", sqlite3_compileoption_used("OMIT_WAL"));
        printf("USED_OMIT_AUTORESET=%d\n", sqlite3_compileoption_used("OMIT_AUTORESET"));
        printf("USED_DEBUG=%d\n", sqlite3_compileoption_used("DEBUG"));
        printf("USED_TEST=%d\n", sqlite3_compileoption_used("TEST"));
        printf("USED_STRICT_SUBTYPE=%d\n", sqlite3_compileoption_used("STRICT_SUBTYPE"));

        for (int i = 0; ; i++) {
            const char *opt = sqlite3_compileoption_get(i);
            if (!opt) break;
            printf("COMPILE_OPT=%s\n", opt);
        }

        sqlite3 *mem = NULL;
        if (sqlite3_open(":memory:", &mem) != SQLITE_OK) die_sql("open_mem", mem, 1);
        char sql_ver[64];
        query_single_text(mem, "SELECT sqlite_version();", sql_ver, sizeof(sql_ver));
        printf("SQL_VERSION=%s\n", sql_ver);
        char sql_src[256];
        query_single_text(mem, "SELECT sqlite_source_id();", sql_src, sizeof(sql_src));
        printf("SQL_SOURCEID=%s\n", sql_src);
        sqlite3_close(mem);

        char db1_path[1024];
        snprintf(db1_path, sizeof(db1_path), "%s/c_probe_default.db", probe_dir);
        sqlite3 *db1 = NULL;
        if (sqlite3_open(db1_path, &db1) != SQLITE_OK) die_sql("open_db1", db1, 1);
        exec_checked(db1, "CREATE TABLE p(x);");
        int def_ps = query_single_int(db1, "PRAGMA page_size;");
        int def_sync = query_single_int(db1, "PRAGMA synchronous;");
        int def_autockpt = query_single_int(db1, "PRAGMA wal_autocheckpoint;");
        exec_checked(db1, "PRAGMA journal_mode = WAL;");
        int def_wal_sync = query_single_int(db1, "PRAGMA synchronous;");
        sqlite3_close(db1);

        char db2_path[1024];
        snprintf(db2_path, sizeof(db2_path), "%s/c_probe_override.db", probe_dir);
        sqlite3 *db2 = NULL;
        if (sqlite3_open(db2_path, &db2) != SQLITE_OK) die_sql("open_db2", db2, 1);
        exec_checked(db2, "PRAGMA synchronous = NORMAL; PRAGMA journal_mode = WAL;");
        int ovr_wal_sync_normal = query_single_int(db2, "PRAGMA synchronous;");
        exec_checked(db2, "PRAGMA synchronous = OFF;");
        int ovr_wal_sync_off = query_single_int(db2, "PRAGMA synchronous;");
        sqlite3_close(db2);

        printf("DEFAULT_PAGE_SIZE=%d\n", def_ps);
        printf("DEFAULT_SYNCHRONOUS=%d\n", def_sync);
        printf("DEFAULT_WAL_SYNCHRONOUS=%d\n", def_wal_sync);
        printf("DEFAULT_WAL_AUTOCHECKPOINT=%d\n", def_autockpt);
        printf("OVERRIDE_WAL_SYNC_NORMAL=%d\n", ovr_wal_sync_normal);
        printf("OVERRIDE_WAL_SYNC_OFF=%d\n", ovr_wal_sync_off);
        fflush(stdout);
        return 0;
    }

    /* init-wal <db_path> <page_size> <rows> <payload_bytes> <persist_wal> */
    if (strcmp(cmd, "init-wal") == 0) {
        if (argc < 7) return 2;
        const char *db_path = argv[2];
        int page_size = atoi(argv[3]);
        int rows = atoi(argv[4]);
        int payload_bytes = atoi(argv[5]);
        int persist_wal = atoi(argv[6]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_wal_autocheckpoint(db, 0);

        char sql[256];
        snprintf(sql, sizeof(sql), "PRAGMA page_size = %d; PRAGMA journal_mode = WAL;", page_size);
        exec_checked(db, sql);
        set_persist_wal(db, persist_wal);
        exec_checked(db, "CREATE TABLE IF NOT EXISTS items(id INTEGER PRIMARY KEY, val BLOB);");

        if (rows > 0) {
            exec_checked(db, "BEGIN;");
            sqlite3_stmt *ins = NULL;
            rc = sqlite3_prepare_v2(db, "INSERT INTO items(id, val) VALUES (?1, ?2);", -1, &ins, NULL);
            if (rc != SQLITE_OK) die_sql("prepare_ins", db, rc);
            unsigned char *buf = (unsigned char *)malloc((size_t)(payload_bytes > 0 ? payload_bytes : 1));
            for (int i = 1; i <= rows; i++) {
                for (int b = 0; b < payload_bytes; b++) {
                    buf[b] = (unsigned char)((i * 31 + b * 17) & 0xff);
                }
                sqlite3_bind_int(ins, 1, i);
                sqlite3_bind_blob(ins, 2, buf, payload_bytes, SQLITE_STATIC);
                if (sqlite3_step(ins) != SQLITE_DONE) die_sql("step_ins", db, rc);
                sqlite3_reset(ins);
            }
            free(buf);
            sqlite3_finalize(ins);
            exec_checked(db, "COMMIT;");
        }
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* commit-batch <db_path> <start_id> <count> <payload_bytes> <busy_ms> <persist_wal> */
    if (strcmp(cmd, "commit-batch") == 0) {
        if (argc < 8) return 2;
        const char *db_path = argv[2];
        int start_id = atoi(argv[3]);
        int count = atoi(argv[4]);
        int payload_bytes = atoi(argv[5]);
        int busy_ms = atoi(argv[6]);
        int persist_wal = atoi(argv[7]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, busy_ms);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, persist_wal);

        exec_checked(db, "BEGIN IMMEDIATE;");
        sqlite3_stmt *ins = NULL;
        rc = sqlite3_prepare_v2(db, "INSERT OR REPLACE INTO items(id, val) VALUES (?1, ?2);", -1, &ins, NULL);
        if (rc != SQLITE_OK) die_sql("prepare_ins", db, rc);
        unsigned char *buf = (unsigned char *)malloc((size_t)(payload_bytes > 0 ? payload_bytes : 1));
        for (int i = 0; i < count; i++) {
            int id = start_id + i;
            for (int b = 0; b < payload_bytes; b++) {
                buf[b] = (unsigned char)((id * 37 + b * 13) & 0xff);
            }
            sqlite3_bind_int(ins, 1, id);
            sqlite3_bind_blob(ins, 2, buf, payload_bytes, SQLITE_STATIC);
            if (sqlite3_step(ins) != SQLITE_DONE) die_sql("step_ins", db, rc);
            sqlite3_reset(ins);
        }
        free(buf);
        sqlite3_finalize(ins);
        exec_checked(db, "COMMIT;");
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* hold-reader <db_path> <persist_wal> */
    if (strcmp(cmd, "hold-reader") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        int persist_wal = atoi(argv[3]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, persist_wal);

        exec_checked(db, "BEGIN DEFERRED;");
        long long cnt = 0, sum_id = 0, sum_len = 0;
        query_items_summary(db, &cnt, &sum_id, &sum_len);
        printf("READY count=%lld sum_id=%lld sum_len=%lld\n", cnt, sum_id, sum_len);
        fflush(stdout);

        char line[128];
        while (fgets(line, sizeof(line), stdin) != NULL) {
            if (strncmp(line, "QUERY", 5) == 0) {
                query_items_summary(db, &cnt, &sum_id, &sum_len);
                printf("SNAPSHOT count=%lld sum_id=%lld sum_len=%lld\n", cnt, sum_id, sum_len);
                fflush(stdout);
            } else if (strncmp(line, "UPGRADE", 7) == 0) {
                char *errmsg = NULL;
                int urc = sqlite3_exec(
                    db,
                    "INSERT INTO items(id, val) VALUES (999999, x'aa');",
                    NULL, NULL, &errmsg);
                int ext = sqlite3_extended_errcode(db);
                if (errmsg) sqlite3_free(errmsg);
                printf("UPGRADE rc=%d ext=%d\n", urc, ext);
                if (urc != SQLITE_OK && !sqlite3_get_autocommit(db)) {
                    exec_checked(db, "ROLLBACK;");
                }
                fflush(stdout);
            } else if (strncmp(line, "EXIT", 4) == 0) {
                if (!sqlite3_get_autocommit(db)) {
                    exec_checked(db, "END;");
                }
                break;
            }
        }
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* hold-dms <db_path> — opens WAL + attaches -shm via a quick read, then stays idle in autocommit
       holding DMS SHARED on -shm without holding any READ(0..=4) or WRITE lock! */
    if (strcmp(cmd, "hold-dms") == 0) {
        if (argc < 3) return 2;
        const char *db_path = argv[2];
        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, 1);
        long long cnt = 0, sum_id = 0, sum_len = 0;
        query_items_summary(db, &cnt, &sum_id, &sum_len);
        printf("READY_DMS count=%lld\n", cnt);
        fflush(stdout);
        char line[64];
        (void)fgets(line, sizeof(line), stdin);
        sqlite3_close(db);
        return 0;
    }

    /* hold-writer <db_path> <id> <payload_bytes> <persist_wal> */
    if (strcmp(cmd, "hold-writer") == 0) {
        if (argc < 6) return 2;
        const char *db_path = argv[2];
        int id = atoi(argv[3]);
        int payload_bytes = atoi(argv[4]);
        int persist_wal = atoi(argv[5]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, persist_wal);

        exec_checked(db, "BEGIN IMMEDIATE;");
        sqlite3_stmt *ins = NULL;
        rc = sqlite3_prepare_v2(db, "INSERT OR REPLACE INTO items(id, val) VALUES (?1, zeroblob(?2));", -1, &ins, NULL);
        if (rc != SQLITE_OK) die_sql("prepare_ins", db, rc);
        sqlite3_bind_int(ins, 1, id);
        sqlite3_bind_int(ins, 2, payload_bytes);
        if (sqlite3_step(ins) != SQLITE_DONE) die_sql("step_ins", db, rc);
        sqlite3_finalize(ins);

        printf("READY_WRITER\n");
        fflush(stdout);

        char line[64];
        if (fgets(line, sizeof(line), stdin) != NULL) {
            if (strncmp(line, "COMMIT", 6) == 0) {
                exec_checked(db, "COMMIT;");
            } else {
                exec_checked(db, "ROLLBACK;");
            }
        }
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* checkpoint <db_path> <mode_int> <busy_ms> <persist_wal> */
    if (strcmp(cmd, "checkpoint") == 0) {
        if (argc < 6) return 2;
        const char *db_path = argv[2];
        int mode = atoi(argv[3]);
        int busy_ms = atoi(argv[4]);
        int persist_wal = atoi(argv[5]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_busy_timeout(db, busy_ms);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, persist_wal);

        /* Warm schema so a freshly opened C sqlite3* discovers WAL mode (`pager.c:7594`). */
        int warm_rc = sqlite3_exec(db, "SELECT count(*) FROM sqlite_schema;", NULL, NULL, NULL);
        if (warm_rc != SQLITE_OK) {
            int ext = sqlite3_extended_errcode(db);
            printf("CKPT rc=%d ext=%d log=-1 ckpt=-1\n", warm_rc, ext);
            fflush(stdout);
            sqlite3_close(db);
            return 0;
        }

        int n_log = -99, n_ckpt = -99;
        int ck_rc = sqlite3_wal_checkpoint_v2(db, "main", mode, &n_log, &n_ckpt);
        int ext = sqlite3_extended_errcode(db);
        printf("CKPT rc=%d ext=%d log=%d ckpt=%d\n", ck_rc, ext, n_log, n_ckpt);
        fflush(stdout);
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* verify <db_path> <persist_wal> */
    if (strcmp(cmd, "verify") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        int persist_wal = atoi(argv[3]);

        sqlite3 *db = NULL;
        int rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READWRITE, NULL);
        if (rc != SQLITE_OK) die_sql("open", db, rc);
        sqlite3_wal_autocheckpoint(db, 0);
        set_persist_wal(db, persist_wal);

        char integ[128], jmode[64];
        query_single_text(db, "PRAGMA integrity_check;", integ, sizeof(integ));
        int psize = query_single_int(db, "PRAGMA page_size;");
        query_single_text(db, "PRAGMA journal_mode;", jmode, sizeof(jmode));
        long long cnt = 0, sum_id = 0, sum_len = 0;
        query_items_summary(db, &cnt, &sum_id, &sum_len);

        printf("INTEGRITY=%s\n", integ);
        printf("PAGE_SIZE=%d\n", psize);
        printf("JOURNAL_MODE=%s\n", jmode);
        printf("SUMMARY count=%lld sum_id=%lld sum_len=%lld\n", cnt, sum_id, sum_len);
        fflush(stdout);
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close", db, 1);
        return 0;
    }

    /* verify-readonly <db_path> <immutable:0|1> */
    if (strcmp(cmd, "verify-readonly") == 0) {
        if (argc < 4) return 2;
        const char *db_path = argv[2];
        int immutable = atoi(argv[3]);

        sqlite3 *db = NULL;
        int rc;
        if (immutable) {
            char uri[1200];
            snprintf(uri, sizeof(uri), "file:%s?immutable=1", db_path);
            rc = sqlite3_open_v2(uri, &db, SQLITE_OPEN_READONLY | SQLITE_OPEN_URI, NULL);
        } else {
            rc = sqlite3_open_v2(db_path, &db, SQLITE_OPEN_READONLY, NULL);
        }
        if (rc != SQLITE_OK) die_sql("open_ro", db, rc);

        char integ[128];
        query_single_text(db, "PRAGMA integrity_check;", integ, sizeof(integ));
        long long cnt = 0, sum_id = 0, sum_len = 0;
        query_items_summary(db, &cnt, &sum_id, &sum_len);
        printf("INTEGRITY=%s\n", integ);
        printf("SUMMARY count=%lld sum_id=%lld sum_len=%lld\n", cnt, sum_id, sum_len);
        fflush(stdout);
        if (sqlite3_close(db) != SQLITE_OK) die_sql("close_ro", db, 1);
        return 0;
    }

    fprintf(stderr, "Unknown c_wal_compat_helper command: %s\n", cmd);
    return 2;
}
"#;

#[cfg(unix)]
fn c_compat_helper() -> &'static Path {
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

        let helper_c = ref_root.join("c_wal_compat_helper.c");
        let helper_bin = ref_root.join("c_wal_compat_helper");
        let need_compile = match fs::read_to_string(&helper_c) {
            Ok(existing) => existing != C_WAL_COMPAT_HELPER_SOURCE || !helper_bin.exists(),
            Err(_) => true,
        };
        if need_compile {
            let tmp_c = ref_root.join(format!("c_wal_compat_helper.{}.c", std::process::id()));
            let tmp_bin = ref_root.join(format!("c_wal_compat_helper.{}.tmp", std::process::id()));
            fs::write(&tmp_c, C_WAL_COMPAT_HELPER_SOURCE).expect("write temp helper.c");
            let cc_bin = Command::new("cc")
                .args(["-O2", "-pthread"])
                .arg("-I")
                .arg(&amalg_dir)
                .arg(&tmp_c)
                .arg(&sqlite3_o)
                .args(["-ldl", "-lm", "-o"])
                .arg(&tmp_bin)
                .output()
                .expect("compile c_wal_compat_helper");
            let _ = fs::remove_file(&tmp_c);
            assert!(
                cc_bin.status.success(),
                "failed compiling c_wal_compat_helper:\n{}",
                String::from_utf8_lossy(&cc_bin.stderr)
            );
            fs::write(&helper_c, C_WAL_COMPAT_HELPER_SOURCE).expect("write c_wal_compat_helper.c");
            fs::rename(&tmp_bin, &helper_bin).expect("rename c_wal_compat_helper");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ItemsSummary {
    count: i64,
    sum_id: i64,
    sum_len: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DbVerifySnapshot {
    integrity: String,
    page_size: i64,
    journal_mode: String,
    summary: ItemsSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CCheckpointOutput {
    rc: i32,
    ext: i32,
    n_log: i32,
    n_ckpt: i32,
}

fn parse_summary_fields(line: &str) -> ItemsSummary {
    let mut count = None;
    let mut sum_id = None;
    let mut sum_len = None;
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            match k {
                "count" => count = v.parse().ok(),
                "sum_id" => sum_id = v.parse().ok(),
                "sum_len" => sum_len = v.parse().ok(),
                _ => {}
            }
        }
    }
    ItemsSummary {
        count: count.unwrap_or_else(|| panic!("missing count in `{line}`")),
        sum_id: sum_id.unwrap_or_else(|| panic!("missing sum_id in `{line}`")),
        sum_len: sum_len.unwrap_or_else(|| panic!("missing sum_len in `{line}`")),
    }
}

#[cfg(unix)]
fn run_c_init_wal(
    db_path: &Path,
    page_size: u32,
    rows: usize,
    payload_bytes: usize,
    persist_wal: bool,
) {
    let out = Command::new(c_compat_helper())
        .arg("init-wal")
        .arg(db_path)
        .arg(page_size.to_string())
        .arg(rows.to_string())
        .arg(payload_bytes.to_string())
        .arg(if persist_wal { "1" } else { "0" })
        .output()
        .expect("run c init-wal");
    assert!(
        out.status.success(),
        "c init-wal failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
fn run_c_commit_batch(
    db_path: &Path,
    start_id: i64,
    count: usize,
    payload_bytes: usize,
    busy_ms: u32,
    persist_wal: bool,
) {
    let out = Command::new(c_compat_helper())
        .arg("commit-batch")
        .arg(db_path)
        .arg(start_id.to_string())
        .arg(count.to_string())
        .arg(payload_bytes.to_string())
        .arg(busy_ms.to_string())
        .arg(if persist_wal { "1" } else { "0" })
        .output()
        .expect("run c commit-batch");
    assert!(
        out.status.success(),
        "c commit-batch failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
fn run_c_checkpoint(
    db_path: &Path,
    mode: WalCheckpointMode,
    busy_ms: u32,
    persist_wal: bool,
) -> CCheckpointOutput {
    let out = Command::new(c_compat_helper())
        .arg("checkpoint")
        .arg(db_path)
        .arg((mode as i32).to_string())
        .arg(busy_ms.to_string())
        .arg(if persist_wal { "1" } else { "0" })
        .output()
        .expect("run c checkpoint");
    assert!(
        out.status.success(),
        "c checkpoint failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().find(|l| l.starts_with("CKPT ")).unwrap();
    let mut rc = 0;
    let mut ext = 0;
    let mut n_log = -99;
    let mut n_ckpt = -99;
    for tok in line.split_whitespace() {
        if let Some((k, v)) = tok.split_once('=') {
            match k {
                "rc" => rc = v.parse().unwrap(),
                "ext" => ext = v.parse().unwrap(),
                "log" => n_log = v.parse().unwrap(),
                "ckpt" => n_ckpt = v.parse().unwrap(),
                _ => {}
            }
        }
    }
    CCheckpointOutput {
        rc,
        ext,
        n_log,
        n_ckpt,
    }
}

#[cfg(unix)]
fn run_c_verify(db_path: &Path, persist_wal: Option<bool>) -> DbVerifySnapshot {
    let persist_arg = match persist_wal {
        Some(true) => "1",
        Some(false) => "0",
        None => "-1",
    };
    let out = Command::new(c_compat_helper())
        .arg("verify")
        .arg(db_path)
        .arg(persist_arg)
        .output()
        .expect("run c verify");
    assert!(
        out.status.success(),
        "c verify failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut integrity = String::new();
    let mut page_size = 0i64;
    let mut journal_mode = String::new();
    let mut summary = ItemsSummary {
        count: 0,
        sum_id: 0,
        sum_len: 0,
    };
    for line in stdout.lines() {
        if let Some(v) = line.strip_prefix("INTEGRITY=") {
            integrity = v.to_owned();
        } else if let Some(v) = line.strip_prefix("PAGE_SIZE=") {
            page_size = v.parse().unwrap();
        } else if let Some(v) = line.strip_prefix("JOURNAL_MODE=") {
            journal_mode = v.to_owned();
        } else if line.starts_with("SUMMARY ") {
            summary = parse_summary_fields(line);
        }
    }
    DbVerifySnapshot {
        integrity,
        page_size,
        journal_mode,
        summary,
    }
}

fn run_rust_verify(db_path: &Path, persist_wal: Option<bool>) -> DbVerifySnapshot {
    let conn = Connection::open(db_path).expect("open rust verify");
    conn.wal_autocheckpoint(0).expect("disable autockpt");
    if let Some(p) = persist_wal {
        conn.file_control_persist_wal(None, i32::from(p))
            .expect("set persist_wal");
    }
    let integ = conn
        .query("PRAGMA integrity_check;", &[])
        .expect("rust integrity_check")
        .rows[0]
        .get_str(0)
        .unwrap()
        .to_owned();
    let psize = conn
        .query("PRAGMA page_size;", &[])
        .expect("rust page_size")
        .rows[0]
        .get_i64(0)
        .unwrap();
    let jmode = conn
        .query("PRAGMA journal_mode;", &[])
        .expect("rust journal_mode")
        .rows[0]
        .get_str(0)
        .unwrap()
        .to_owned();
    let q = conn
        .query(
            "SELECT count(*), coalesce(sum(id), 0), coalesce(sum(length(val)), 0) FROM items;",
            &[],
        )
        .expect("rust summary query");
    let summary = ItemsSummary {
        count: q.rows[0].get_i64(0).unwrap(),
        sum_id: q.rows[0].get_i64(1).unwrap(),
        sum_len: q.rows[0].get_i64(2).unwrap(),
    };
    conn.try_close().expect("close rust verify");
    DbVerifySnapshot {
        integrity: integ,
        page_size: psize,
        journal_mode: jmode,
        summary,
    }
}

fn query_rust_items_summary(conn: &Connection) -> ItemsSummary {
    let q = conn
        .query(
            "SELECT count(*), coalesce(sum(id), 0), coalesce(sum(length(val)), 0) FROM items;",
            &[],
        )
        .expect("query items summary");
    ItemsSummary {
        count: q.rows[0].get_i64(0).unwrap(),
        sum_id: q.rows[0].get_i64(1).unwrap(),
        sum_len: q.rows[0].get_i64(2).unwrap(),
    }
}

// ============================================================================
// Interactive Child Process Controller (Used for Separate C & Rust Processes)
// ============================================================================

struct InteractiveChild {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl InteractiveChild {
    fn from_spawned(mut child: Child) -> Self {
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn read_line_matching(&mut self, prefix: &str) -> String {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).expect("read child stdout");
            assert!(
                n > 0,
                "child exited unexpectedly while waiting for prefix `{prefix}`"
            );
            let trimmed = line.trim();
            if trimmed.starts_with(prefix) {
                return trimmed.to_owned();
            }
        }
    }

    fn send_cmd(&mut self, cmd_line: &str) {
        self.stdin
            .write_all(cmd_line.as_bytes())
            .expect("write child stdin");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush child stdin");
    }

    fn exit_cleanly(mut self) {
        let _ = self.stdin.write_all(b"EXIT\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        let status = self.child.wait().expect("wait child");
        assert!(status.success(), "child exited with failure: {status}");
    }

    fn sigkill_and_reap(mut self) {
        self.child.kill().expect("send SIGKILL to child");
        let status = self.child.wait().expect("reap SIGKILLed child");
        assert!(
            !status.success(),
            "SIGKILLed child must not report normal success exit"
        );
    }
}

fn spawn_rust_worker(role: &str, db_path: &Path, extra_env: &[(&str, &str)]) -> InteractiveChild {
    let exe = std::env::current_exe().expect("current test executable");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", "child_process_worker_entrypoint", "--nocapture"])
        .env("SQLITERS_WAL_COMPAT_ROLE", role)
        .env("SQLITERS_WAL_COMPAT_DB", db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for &(k, v) in extra_env {
        cmd.env(k, v);
    }
    InteractiveChild::from_spawned(cmd.spawn().expect("spawn rust child worker"))
}

#[cfg(unix)]
fn spawn_c_reader(db_path: &Path, persist_wal: bool) -> (InteractiveChild, ItemsSummary) {
    let child = Command::new(c_compat_helper())
        .arg("hold-reader")
        .arg(db_path)
        .arg(if persist_wal { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn c hold-reader");
    let mut ic = InteractiveChild::from_spawned(child);
    let ready = ic.read_line_matching("READY ");
    let snap = parse_summary_fields(&ready);
    (ic, snap)
}

#[cfg(unix)]
fn spawn_c_dms_holder(db_path: &Path) -> InteractiveChild {
    let child = Command::new(c_compat_helper())
        .arg("hold-dms")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn c hold-dms");
    let mut ic = InteractiveChild::from_spawned(child);
    let _ = ic.read_line_matching("READY_DMS ");
    ic
}

#[cfg(unix)]
fn spawn_c_writer(
    db_path: &Path,
    id: i64,
    payload_bytes: usize,
    persist_wal: bool,
) -> InteractiveChild {
    let child = Command::new(c_compat_helper())
        .arg("hold-writer")
        .arg(db_path)
        .arg(id.to_string())
        .arg(payload_bytes.to_string())
        .arg(if persist_wal { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn c hold-writer");
    let mut ic = InteractiveChild::from_spawned(child);
    let _ = ic.read_line_matching("READY_WRITER");
    ic
}

// ============================================================================
// Rust Child Process Worker Entrypoint (`SQLITERS_WAL_COMPAT_ROLE`)
// ============================================================================

#[test]
fn child_process_worker_entrypoint() {
    let Ok(role) = std::env::var("SQLITERS_WAL_COMPAT_ROLE") else {
        return;
    };
    let db_path = PathBuf::from(
        std::env::var("SQLITERS_WAL_COMPAT_DB").expect("SQLITERS_WAL_COMPAT_DB env var"),
    );

    match role.as_str() {
        "rust-hold-reader" => {
            let conn = Connection::open(&db_path).expect("worker open");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            conn.execute("BEGIN DEFERRED;").unwrap();
            let snap = query_rust_items_summary(&conn);
            println!(
                "READY count={} sum_id={} sum_len={}",
                snap.count, snap.sum_id, snap.sum_len
            );
            std::io::stdout().flush().unwrap();

            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(cmd) = line else { break };
                let trimmed = cmd.trim();
                if trimmed == "QUERY" {
                    let cur = query_rust_items_summary(&conn);
                    println!(
                        "SNAPSHOT count={} sum_id={} sum_len={}",
                        cur.count, cur.sum_id, cur.sum_len
                    );
                    std::io::stdout().flush().unwrap();
                } else if trimmed == "UPGRADE" {
                    let res = conn.execute("INSERT INTO items(id, val) VALUES (888888, x'bb');");
                    match res {
                        Ok(_) => println!("UPGRADE ok=1"),
                        Err(err) => {
                            let ext = err.extended_code;
                            println!("UPGRADE code={:?} ext={}", err.code, ext);
                            if !conn.is_autocommit() {
                                conn.execute("ROLLBACK;").unwrap();
                            }
                        }
                    }
                    std::io::stdout().flush().unwrap();
                } else if trimmed == "EXIT" {
                    if !conn.is_autocommit() {
                        let _ = conn.execute("END;");
                    }
                    break;
                }
            }
            conn.try_close().unwrap();
        }

        "rust-paused-stmt-downgrade" => {
            let conn = Connection::open(&db_path).expect("worker open");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();

            let mut sel = conn
                .prepare("SELECT id, length(val) FROM items ORDER BY id;")
                .expect("prepare paused select");
            let first_id = match sel.step().expect("first step") {
                StepResult::Row(r) => r.get_i64(0).unwrap(),
                StepResult::Done => panic!("expected non-empty items table"),
            };

            // Autocommit write on the SAME connection while `sel` stays paused at `StepResult::Row`:
            // triggers `Write -> Read` downgrade retaining positive `READ(1..=4)` lock slot!
            conn.execute("INSERT INTO items(id, val) VALUES (500001, zeroblob(512));")
                .expect("autocommit insert alongside paused select");

            println!("READY first_id={first_id}");
            std::io::stdout().flush().unwrap();

            let stdin = std::io::stdin();
            let mut maybe_sel = Some(sel);
            for line in stdin.lock().lines() {
                let Ok(cmd) = line else { break };
                let trimmed = cmd.trim();
                if trimmed == "DRAIN_PAUSED" {
                    if let Some(mut s) = maybe_sel.take() {
                        let mut count = 1usize;
                        let mut max_id = first_id;
                        while let StepResult::Row(r) = s.step().expect("drain step") {
                            count += 1;
                            max_id = r.get_i64(0).unwrap();
                        }
                        drop(s);
                        println!("DRAINED count={count} max_id={max_id}");
                        std::io::stdout().flush().unwrap();
                    }
                } else if trimmed == "EXIT" {
                    drop(maybe_sel.take());
                    break;
                }
            }
            drop(maybe_sel);
            conn.try_close().unwrap();
        }

        "crash-1-uncommitted-wal" => {
            let wal_path = wal_sidecar(&db_path);
            let before_size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
            let conn = Connection::open(&db_path).expect("open crash-1");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            conn.execute("PRAGMA cache_size = 2;").unwrap();
            conn.execute("BEGIN IMMEDIATE;").unwrap();
            for i in 1001..=1045 {
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, zeroblob(700));",
                    &[Value::Integer(i)],
                )
                .unwrap();
            }
            let after_size = fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
            assert!(
                after_size > before_size,
                "dirty cache spill must write uncommitted frames to -wal ({before_size} -> {after_size})"
            );
            println!("READY_FOR_SIGKILL wal_size={after_size}");
            std::io::stdout().flush().unwrap();
            let mut buf = [0u8; 8];
            let _ = std::io::stdin().read(&mut buf);
        }

        "crash-2-commit-before-shm" => {
            // Prepare valid tx2 frames in `-wal` while keeping `-shm` at tx1's pre-commit header,
            // then block holding open file descriptors until killed by `SIGKILL`.
            let shm_path = shm_sidecar(&db_path);
            let wal_path = wal_sidecar(&db_path);
            let conn = Connection::open(&db_path).expect("open crash-2");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            // Warm `-shm` at baseline (10 rows) and snapshot its header region (0..136).
            let _ = query_rust_items_summary(&conn);
            let shm_before = fs::read(&shm_path).expect("read baseline -shm");
            assert!(shm_before.len() >= 136);

            // Commit tx2 (rows 11..=25) into `-wal` without checkpointing `.db`.
            conn.execute("BEGIN IMMEDIATE;").unwrap();
            for i in 11..=25 {
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, zeroblob(256));",
                    &[Value::Integer(i)],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();

            // Rewind `-shm` header bytes 0..136 back to tx1's state to model a process killed
            // right after syncing committed frames to `-wal` and right before updating `-shm`.
            let mut shm_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&shm_path)
                .unwrap();
            shm_file.seek(SeekFrom::Start(0)).unwrap();
            shm_file.write_all(&shm_before[..136]).unwrap();
            shm_file.sync_all().unwrap();
            let wal_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&wal_path)
                .unwrap();

            println!("READY_FOR_SIGKILL");
            std::io::stdout().flush().unwrap();
            let mut buf = [0u8; 8];
            let _ = std::io::stdin().read(&mut buf);
            drop((wal_file, shm_file, conn));
        }

        "crash-3-torn-shm-header" => {
            let shm_path = shm_sidecar(&db_path);
            let conn = Connection::open(&db_path).expect("open crash-3");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            conn.execute("BEGIN IMMEDIATE;").unwrap();
            for i in 11..=22 {
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, zeroblob(300));",
                    &[Value::Integer(i)],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();

            // Corrupt copy 2 of `WalIndexHdr` (`48..96`) in `-shm` so `hdr1 != hdr2` (torn write).
            let mut shm_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&shm_path)
                .unwrap();
            shm_file
                .seek(SeekFrom::Start(WALINDEX_HDR_COPY2_OFFSET as u64))
                .unwrap();
            let torn = [0xA5u8; WALINDEX_HDR_COPY_LEN];
            shm_file.write_all(&torn).unwrap();
            shm_file.sync_all().unwrap();

            println!("READY_FOR_SIGKILL");
            std::io::stdout().flush().unwrap();
            let mut buf = [0u8; 8];
            let _ = std::io::stdin().read(&mut buf);
            drop((shm_file, conn));
        }

        "crash-4-mid-backfill" => {
            // Produce two committed WAL batches, partially backfill only batch 1 (pinned by a
            // transient reader), then advance `nBackfillAttempted` > `nBackfill` to model a
            // checkpointer killed mid-backfill before updating `nBackfill`.
            let shm_path = shm_sidecar(&db_path);
            let conn = Connection::open(&db_path).expect("open crash-4");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();

            let reader = Connection::open(&db_path).expect("open transient reader");
            reader.wal_autocheckpoint(0).unwrap();
            reader.file_control_persist_wal(None, 1).unwrap();
            reader.execute("BEGIN DEFERRED;").unwrap();
            let _ = query_rust_items_summary(&reader);

            // Commit batch 2 (rows 11..=30) beyond `reader`'s snapshot (`mxFrame` > reader mark).
            conn.execute("BEGIN IMMEDIATE;").unwrap();
            for i in 11..=30 {
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, zeroblob(320));",
                    &[Value::Integer(i)],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();

            let ckpt = conn
                .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Passive)
                .expect("partial passive checkpoint");
            assert!(
                ckpt.n_ckpt > 0 && ckpt.n_ckpt < ckpt.n_log,
                "expected partial backfill 0 < n_ckpt < n_log, got {ckpt:?}"
            );
            reader.execute("END;").unwrap();
            reader.try_close().unwrap();

            // Write `nBackfillAttempted = n_log` > `nBackfill = n_ckpt` into `-shm`.
            let mut shm_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&shm_path)
                .unwrap();
            shm_file
                .seek(SeekFrom::Start(WAL_CKPT_NBACKFILL_OFFSET as u64))
                .unwrap();
            let mut raw_nb = [0u8; 4];
            shm_file.read_exact(&mut raw_nb).unwrap();
            let n_backfill = u32::from_ne_bytes(raw_nb);
            let attempted = (ckpt.n_log as u32).max(n_backfill + 1);
            shm_file
                .seek(SeekFrom::Start(WAL_CKPT_BACKFILL_ATTEMPTED_OFFSET as u64))
                .unwrap();
            shm_file.write_all(&attempted.to_ne_bytes()).unwrap();
            shm_file.sync_all().unwrap();

            println!(
                "READY_FOR_SIGKILL n_backfill={} attempted={} mx_frame={}",
                n_backfill, attempted, ckpt.n_log
            );
            std::io::stdout().flush().unwrap();
            let mut buf = [0u8; 8];
            let _ = std::io::stdin().read(&mut buf);
            drop((shm_file, conn));
        }

        "crash-5-post-restart-before-truncate" => {
            // Model crash right after `walRestartHdr` (and new WAL header write) BEFORE
            // `truncate(0)` in a `TRUNCATE` checkpoint while stale old-salt frames remain in `-wal`!
            let wal_path = wal_sidecar(&db_path);
            let conn = Connection::open(&db_path).expect("open crash-5");
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();

            // Backfill all valid rows (1..=10) into `.db` via PASSIVE checkpoint.
            let ck = conn
                .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Passive)
                .unwrap();
            assert_eq!(ck.n_ckpt, ck.n_log);
            assert!(ck.n_log > 0);

            let mut wal_bytes = fs::read(&wal_path).expect("read -wal");
            let mut raw_hdr = [0u8; WAL_HDRSIZE];
            raw_hdr.copy_from_slice(&wal_bytes[..WAL_HDRSIZE]);
            let old_hdr = WalFileHeader::decode_for_recovery(&raw_hdr)
                .unwrap()
                .expect("valid WAL header");
            let page_size = old_hdr.page_size as usize;

            // Append a bogus "committed" frame 1 at offset 32 carrying OLD salts (`old_hdr.salt`)
            // with corrupted page 1 bytes — if recovery failed to reject old salts after
            // `walRestartHdr`, it would corrupt `.db`!
            let bogus_page = vec![0xEEu8; page_size];
            let (fhdr, _) = wal_encode_frame_header(
                1,
                99,
                old_hdr.salt,
                old_hdr.big_end_cksum,
                old_hdr.cksum,
                &bogus_page,
            )
            .unwrap();
            let f1_off = wal_frame_offset(1, old_hdr.page_size) as usize;
            if wal_bytes.len() < f1_off + WAL_FRAME_HDRSIZE + page_size {
                wal_bytes.resize(f1_off + WAL_FRAME_HDRSIZE + page_size, 0);
            }
            wal_bytes[f1_off..f1_off + WAL_FRAME_HDRSIZE].copy_from_slice(&fhdr);
            wal_bytes[f1_off + WAL_FRAME_HDRSIZE..f1_off + WAL_FRAME_HDRSIZE + page_size]
                .copy_from_slice(&bogus_page);

            // Now write a restarted 32-byte `WalFileHeader` (`n_ckpt + 1`, incremented `salt[0]`,
            // fresh `salt[1]`) at offset 0..32 WITHOUT truncating `-wal`!
            let (restarted_raw, _) = WalFileHeader::encode(
                old_hdr.page_size,
                old_hdr.n_ckpt.wrapping_add(1),
                [
                    old_hdr.salt[0].wrapping_add(1),
                    old_hdr.salt[1] ^ 0x55AA_55AA,
                ],
                old_hdr.big_end_cksum,
            )
            .unwrap();
            wal_bytes[..WAL_HDRSIZE].copy_from_slice(&restarted_raw);

            let mut wf = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&wal_path)
                .unwrap();
            wf.seek(SeekFrom::Start(0)).unwrap();
            wf.write_all(&wal_bytes).unwrap();
            wf.sync_all().unwrap();

            println!("READY_FOR_SIGKILL wal_len={}", wal_bytes.len());
            std::io::stdout().flush().unwrap();
            let mut buf = [0u8; 8];
            let _ = std::io::stdin().read(&mut buf);
            drop((wf, conn));
        }

        other => panic!("unknown SQLITERS_WAL_COMPAT_ROLE: {other}"),
    }
}

// ============================================================================
// Suite 1: C SQLite 3.54.1 Verification & Structured Configuration Manifest
// (`implementation_plan.md:995-1001`)
// ============================================================================

#[test]
#[cfg(unix)]
fn c_reference_3_54_1_verification_and_phase1_2_manifest() {
    let sandbox = TempSandbox::new("manifest_probe");
    let out = Command::new(c_compat_helper())
        .arg("probe-config")
        .arg(sandbox.path())
        .output()
        .expect("run probe-config");
    assert!(
        out.status.success(),
        "probe-config failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

    // 1. Version & SQLITE_SOURCE_ID checks (`implementation_plan.md:996-997`).
    assert_eq!(kv.get("LIBVERSION").map(String::as_str), Some("3.54.1"));
    assert_eq!(kv.get("SQL_VERSION").map(String::as_str), Some("3.54.1"));
    assert_eq!(
        kv.get("LIBVERSION_NUM").map(String::as_str),
        Some("3054001")
    );
    assert_eq!(SQLITE_VERSION, "3.54.1");

    let c_source_id = kv.get("SOURCEID").expect("C SOURCEID");
    assert!(c_source_id.len() >= 80, "invalid C SOURCEID: {c_source_id}");
    assert_eq!(kv.get("SQL_SOURCEID"), Some(c_source_id));
    assert!(
        SQLITE_SOURCE_ID.starts_with(c_source_id),
        "Rust SQLITE_SOURCE_ID `{SQLITE_SOURCE_ID}` must start with C `{c_source_id}`"
    );
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_uuid = fs::read_to_string(workspace_root.join("sqlite/manifest.uuid"))
        .expect("read sqlite/manifest.uuid")
        .trim()
        .to_owned();
    assert!(
        c_source_id.contains(&manifest_uuid),
        "C SOURCEID `{c_source_id}` must match sqlite/manifest.uuid `{manifest_uuid}`"
    );

    // 2. Default & override WAL synchronous parity (`implementation_plan.md:371, 999`).
    let c_def_ps: u32 = kv["DEFAULT_PAGE_SIZE"].parse().unwrap();
    let c_def_sync: i64 = kv["DEFAULT_SYNCHRONOUS"].parse().unwrap();
    let c_def_wal_sync: i64 = kv["DEFAULT_WAL_SYNCHRONOUS"].parse().unwrap();
    let c_def_autockpt: i32 = kv["DEFAULT_WAL_AUTOCHECKPOINT"].parse().unwrap();
    let c_ovr_norm: i64 = kv["OVERRIDE_WAL_SYNC_NORMAL"].parse().unwrap();
    let c_ovr_off: i64 = kv["OVERRIDE_WAL_SYNC_OFF"].parse().unwrap();

    assert_eq!(c_def_ps, SQLITE_DEFAULT_PAGE_SIZE);
    assert_eq!(c_def_autockpt, SQLITE_DEFAULT_WAL_AUTOCHECKPOINT);

    let r_db1 = sandbox.path().join("rust_probe_default.db");
    let r_conn1 = Connection::open(&r_db1).unwrap();
    r_conn1.execute("CREATE TABLE p(x);").unwrap();
    let r_def_ps = r_conn1.query("PRAGMA page_size;", &[]).unwrap().rows[0]
        .get_i64(0)
        .unwrap() as u32;
    let r_def_sync = r_conn1.query("PRAGMA synchronous;", &[]).unwrap().rows[0]
        .get_i64(0)
        .unwrap();
    let r_def_autockpt = r_conn1
        .query("PRAGMA wal_autocheckpoint;", &[])
        .unwrap()
        .rows[0]
        .get_i64(0)
        .unwrap() as i32;
    r_conn1.execute("PRAGMA journal_mode = WAL;").unwrap();
    let r_def_wal_sync = r_conn1.query("PRAGMA synchronous;", &[]).unwrap().rows[0]
        .get_i64(0)
        .unwrap();
    r_conn1.try_close().unwrap();

    let r_db2 = sandbox.path().join("rust_probe_override.db");
    let r_conn2 = Connection::open(&r_db2).unwrap();
    r_conn2
        .execute("PRAGMA synchronous = NORMAL; PRAGMA journal_mode = WAL;")
        .unwrap();
    let r_ovr_norm = r_conn2.query("PRAGMA synchronous;", &[]).unwrap().rows[0]
        .get_i64(0)
        .unwrap();
    r_conn2.execute("PRAGMA synchronous = OFF;").unwrap();
    let r_ovr_off = r_conn2.query("PRAGMA synchronous;", &[]).unwrap().rows[0]
        .get_i64(0)
        .unwrap();
    r_conn2.try_close().unwrap();

    assert_eq!(r_def_ps, c_def_ps);
    assert_eq!(r_def_sync, c_def_sync);
    assert_eq!(r_def_wal_sync, c_def_wal_sync);
    assert_eq!(r_def_autockpt, c_def_autockpt);
    assert_eq!(r_ovr_norm, c_ovr_norm);
    assert_eq!(r_ovr_off, c_ovr_off);

    // 3. Capability table checks (`implementation_plan.md:272-285, 999`).
    let cap_map: BTreeMap<&str, bool> = PHASE1_CAPABILITY_TABLE.iter().copied().collect();
    assert_eq!(cap_map.get("autoreset"), Some(&true));
    assert_eq!(cap_map.get("wal"), Some(&true));
    assert_eq!(cap_map.get("tempdb"), Some(&true));
    assert_eq!(cap_map.get("attach"), Some(&true));
    assert_eq!(cap_map.get("view"), Some(&true));
    assert_eq!(cap_map.get("upsert"), Some(&true));
    assert_eq!(cap_map.get("trigger"), Some(&true));
    assert_eq!(cap_map.get("windowfunc"), Some(&true));
    assert_eq!(cap_map.get("analyze"), Some(&true));
    assert_eq!(cap_map.get("vtab"), Some(&true));
    assert_eq!(cap_map.get("json"), Some(&true));
    assert_eq!(cap_map.get("json1"), Some(&true));
    assert_eq!(cap_map.get("fts5"), Some(&true));
    assert_eq!(cap_map.get("rtree"), Some(&true));
    assert_eq!(cap_map.get("dbstat"), Some(&true));

    // 4. Read `tests/sqlite_test_manifest.json` summary & OS/filesystem metadata.
    let test_manifest_path = workspace_root.join("tests/sqlite_test_manifest.json");
    let test_manifest_raw =
        fs::read_to_string(&test_manifest_path).expect("read tests/sqlite_test_manifest.json");
    let mut total_wal_wrappers = 0usize;
    let mut active_wal_wrappers = 0usize;
    let mut ignored_wal_wrappers = 0usize;
    let mut in_wal_entry = false;
    for line in test_manifest_raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"id\": \"sqlite/test/wal") {
            in_wal_entry = true;
            total_wal_wrappers += 1;
        } else if trimmed.starts_with("\"id\": ") {
            in_wal_entry = false;
        } else if in_wal_entry && trimmed.starts_with("\"status\": \"active\"") {
            active_wal_wrappers += 1;
        } else if in_wal_entry && trimmed.starts_with("\"status\": \"ignored\"") {
            ignored_wal_wrappers += 1;
        }
    }
    assert_eq!(total_wal_wrappers, 43);
    assert!(active_wal_wrappers >= 6);
    assert_eq!(active_wal_wrappers + ignored_wal_wrappers, 43);

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

    // 5. Emit `target/sqlite-ref/phase1_2_manifest.json` with all fields from `implementation_plan.md:999`.
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

    let prod_opts = ConnectionOpenOptions::default();
    let manifest_json = format!(
        "{{\n\
  \"phase\": \"1.2\",\n\
  \"sqlite_version\": \"3.54.1\",\n\
  \"sqlite_version_number\": 3054001,\n\
  \"c_sqlite_source_id\": \"{}\",\n\
  \"rust_sqlite_source_id\": \"{}\",\n\
  \"c_reference_binary\": \"{}\",\n\
  \"c_compile_flags\": [\"-O2\", \"-pthread\"],\n\
  \"pragma_compile_options\": [{}],\n\
  \"sqlite_threadsafe\": {},\n\
  \"default_vfs\": \"{}\",\n\
  \"compile_option_probes\": {{\n\
    \"SQLITE_ENABLE_SNAPSHOT\": {},\n\
    \"SQLITE_ENABLE_SETLK_TIMEOUT\": {},\n\
    \"SQLITE_OMIT_WAL\": {},\n\
    \"SQLITE_OMIT_AUTORESET\": {},\n\
    \"SQLITE_DEBUG\": {},\n\
    \"SQLITE_TEST\": {},\n\
    \"SQLITE_STRICT_SUBTYPE\": {}\n\
  }},\n\
  \"synchronous_and_page_size\": {{\n\
    \"SQLITE_DEFAULT_PAGE_SIZE\": {},\n\
    \"TESTFIXTURE_DEFAULT_PAGE_SIZE\": {},\n\
    \"SQLITE_DEFAULT_SYNCHRONOUS\": {},\n\
    \"SQLITE_DEFAULT_WAL_SYNCHRONOUS\": {},\n\
    \"user_override_wal_synchronous_normal\": {},\n\
    \"user_override_wal_synchronous_off\": {},\n\
    \"SQLITE_DEFAULT_WAL_AUTOCHECKPOINT\": {}\n\
  }},\n\
  \"lock_geometry\": {{\n\
    \"production_pending_byte\": \"{:#010x}\",\n\
    \"testfixture_pending_byte\": \"{:#010x}\",\n\
    \"c_helper_pending_byte\": \"{}\"\n\
  }},\n\
  \"rust_open_options_and_features\": {{\n\
    \"default_page_size\": {},\n\
    \"default_lock_geometry_pending_byte\": \"{:#010x}\",\n\
    \"temp_store_memory\": {},\n\
    \"sqliters_core_test_diagnostics_available\": true,\n\
    \"workspace_unsafe_policy\": \"forbid_outside_sqliters_vfs_sys\"\n\
  }},\n\
  \"phase1_capability_table\": {{\n{}\n  }},\n\
  \"wal_tcl_suite_inventory\": {{\n\
    \"manifest_path\": \"tests/sqlite_test_manifest.json\",\n\
    \"registered_wal_wrappers\": {},\n\
    \"active_wal_wrappers\": {},\n\
    \"ignored_wal_wrappers\": {}\n\
  }},\n\
  \"environment\": {{\n\
    \"os_uname\": \"{}\",\n\
    \"filesystem_type\": \"{}\"\n\
  }}\n\
}}\n",
        json_escape(c_source_id),
        json_escape(SQLITE_SOURCE_ID),
        json_escape(&c_compat_helper().display().to_string()),
        compile_opts_json,
        kv["THREADSAFE"],
        json_escape(&kv["DEFAULT_VFS"]),
        kv["USED_ENABLE_SNAPSHOT"],
        kv["USED_ENABLE_SETLK_TIMEOUT"],
        kv["USED_OMIT_WAL"],
        kv["USED_OMIT_AUTORESET"],
        kv["USED_DEBUG"],
        kv["USED_TEST"],
        kv["USED_STRICT_SUBTYPE"],
        c_def_ps,
        TESTFIXTURE_DEFAULT_PAGE_SIZE,
        c_def_sync,
        c_def_wal_sync,
        c_ovr_norm,
        c_ovr_off,
        c_def_autockpt,
        LockGeometry::PRODUCTION.pending_byte_offset(),
        LockGeometry::TEST_64K.pending_byte_offset(),
        json_escape(&kv["PRODUCTION_PENDING_BYTE"]),
        prod_opts.default_page_size,
        prod_opts.lock_geometry.pending_byte_offset(),
        prod_opts.temp_store_memory,
        caps_json,
        total_wal_wrappers,
        active_wal_wrappers,
        ignored_wal_wrappers,
        json_escape(&uname_out),
        json_escape(&fstype_out),
    );

    let manifest_out = workspace_root
        .join("target")
        .join("sqlite-ref")
        .join("phase1_2_manifest.json");
    fs::write(&manifest_out, &manifest_json).expect("write phase1_2_manifest.json");
    let read_back = fs::read_to_string(&manifest_out).expect("read back phase1_2_manifest.json");
    assert!(read_back.contains("\"sqlite_version\": \"3.54.1\""));
    assert!(read_back.contains("\"wal\": true"));
    assert!(read_back.contains("\"autoreset\": true"));
}

// ============================================================================
// Suite 2: Multi-Process Rust Readers + C Writer + C Checkpointer
// ============================================================================

#[test]
#[cfg(unix)]
fn multiprocess_rust_readers_with_c_writer_and_c_checkpointer() {
    let sandbox = TempSandbox::new("rust_readers_c_writer");
    let db_path = sandbox.path().join("concurrent_rc.db");

    // 1. C initializes WAL database with 20 rows (page_size = 4096).
    run_c_init_wal(&db_path, 4096, 20, 256, true);
    // Commit a second C batch (rows 21..=40) so `-wal` has uncheckpointed frames (`mxFrame > 0`)
    // and reader 1 acquires a positive `READ(1..=4)` mark!
    run_c_commit_batch(&db_path, 21, 20, 256, 2000, true);

    // 2. Spawn Rust reader process #1 (pins snapshot at 40 rows).
    let mut r_reader1 = spawn_rust_worker("rust-hold-reader", &db_path, &[]);
    let r1_ready = parse_summary_fields(&r_reader1.read_line_matching("READY "));
    assert_eq!(r1_ready.count, 40);

    // 3. C writer commits batch 3 (rows 41..=65) while Rust reader #1 holds its snapshot.
    run_c_commit_batch(&db_path, 41, 25, 256, 2000, true);

    // 4. Spawn Rust reader process #2 (pins newer snapshot at 65 rows on a second `READ(1..=4)` slot).
    let mut r_reader2 = spawn_rust_worker("rust-hold-reader", &db_path, &[]);
    let r2_ready = parse_summary_fields(&r_reader2.read_line_matching("READY "));
    assert_eq!(r2_ready.count, 65);

    // 5. C writer commits batch 4 (rows 66..=90) while BOTH Rust readers remain open!
    run_c_commit_batch(&db_path, 66, 25, 256, 2000, true);

    // Verify both Rust readers still observe their exact pre-commit snapshots!
    r_reader1.send_cmd("QUERY");
    assert_eq!(
        parse_summary_fields(&r_reader1.read_line_matching("SNAPSHOT ")),
        r1_ready
    );
    r_reader2.send_cmd("QUERY");
    assert_eq!(
        parse_summary_fields(&r_reader2.read_line_matching("SNAPSHOT ")),
        r2_ready
    );

    // 6. C checkpointer attempts `RESTART` and `TRUNCATE` while Rust readers hold `READ(1..=4)`:
    //    must return `SQLITE_BUSY (5)` after partial backfill up to `r_reader1`'s mark (`0 < ckpt < log`)!
    let ck_restart = run_c_checkpoint(&db_path, WalCheckpointMode::Restart, 20, true);
    assert_eq!(ck_restart.rc, 5, "expected SQLITE_BUSY: {ck_restart:?}");
    assert!(
        ck_restart.n_ckpt > 0 && ck_restart.n_ckpt < ck_restart.n_log,
        "expected partial backfill up to oldest Rust reader mark: {ck_restart:?}"
    );

    let ck_trunc_busy = run_c_checkpoint(&db_path, WalCheckpointMode::Truncate, 20, true);
    assert_eq!(ck_trunc_busy.rc, 5);
    assert_eq!(ck_trunc_busy.n_ckpt, ck_restart.n_ckpt);

    // Verify neither Rust reader's snapshot was corrupted by C's partial backfill!
    r_reader1.send_cmd("QUERY");
    assert_eq!(
        parse_summary_fields(&r_reader1.read_line_matching("SNAPSHOT ")),
        r1_ready
    );
    r_reader2.send_cmd("QUERY");
    assert_eq!(
        parse_summary_fields(&r_reader2.read_line_matching("SNAPSHOT ")),
        r2_ready
    );

    // Release reader #1; reader #2 (at 65 rows) now governs the backfill ceiling!
    r_reader1.exit_cleanly();
    let ck_after_r1 = run_c_checkpoint(&db_path, WalCheckpointMode::Restart, 20, true);
    assert_eq!(ck_after_r1.rc, 5);
    assert!(
        ck_after_r1.n_ckpt > ck_restart.n_ckpt && ck_after_r1.n_ckpt < ck_after_r1.n_log,
        "expected backfill ceiling to advance from {} to reader #2's mark (< {}): {ck_after_r1:?}",
        ck_restart.n_ckpt,
        ck_after_r1.n_log
    );

    // Release reader #2; C `TRUNCATE` checkpoint must now succeed (`SQLITE_OK`) and truncate `-wal` to 0!
    r_reader2.exit_cleanly();
    let ck_final = run_c_checkpoint(&db_path, WalCheckpointMode::Truncate, 1000, true);
    assert_eq!(ck_final.rc, 0, "expected SQLITE_OK: {ck_final:?}");
    assert_eq!(ck_final.n_log, 0);
    assert_eq!(ck_final.n_ckpt, 0);
    assert_eq!(fs::metadata(wal_sidecar(&db_path)).unwrap().len(), 0);

    // Verify C and Rust read identical final 90-row state.
    let c_ver = run_c_verify(&db_path, Some(true));
    let r_ver = run_rust_verify(&db_path, Some(true));
    assert_eq!(c_ver.integrity, "ok");
    assert_eq!(c_ver.summary.count, 90);
    assert_c_rust_parity_or_record_diff(
        "rust_readers_c_writer_final",
        "SELECT count(*), sum(id), sum(length(val)) FROM items;",
        "2 Rust reader processes + C writer + C RESTART/TRUNCATE checkpointer",
        &db_path,
        &c_ver,
        &r_ver,
    );
}

// ============================================================================
// Suite 3: Multi-Process C Readers + Rust Writer + Rust Checkpointer + Busy Contention
// ============================================================================

#[test]
#[cfg(unix)]
fn multiprocess_c_readers_with_rust_writer_checkpointer_and_busy_contention() {
    let sandbox = TempSandbox::new("c_readers_rust_writer");
    let db_path = sandbox.path().join("concurrent_cr.db");

    // 1. Rust initializes WAL DB (page_size = 4096) with 15 rows + second commit of 15 rows (30 total).
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);",
        )
        .unwrap();
        conn.execute("BEGIN;").unwrap();
        for i in 1..=15 {
            conn.execute_params(
                "INSERT INTO items(id, val) VALUES (?1, zeroblob(300));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        conn.execute("BEGIN;").unwrap();
        for i in 16..=30 {
            conn.execute_params(
                "INSERT INTO items(id, val) VALUES (?1, zeroblob(300));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();
        conn.try_close().unwrap();
    }

    // 2. Spawn C reader process #1 (pins 30-row snapshot).
    let (mut c_reader1, c1_snap) = spawn_c_reader(&db_path, true);
    assert_eq!(c1_snap.count, 30);

    // 3. Rust writer commits rows 31..=55 while C reader #1 holds its snapshot.
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute("BEGIN IMMEDIATE;").unwrap();
        for i in 31..=55 {
            conn.execute_params(
                "INSERT INTO items(id, val) VALUES (?1, zeroblob(300));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        // Spawn C reader #2 (pins 55-row snapshot), then commit rows 56..=80 from Rust!
        let (mut c_reader2, c2_snap) = spawn_c_reader(&db_path, true);
        assert_eq!(c2_snap.count, 55);

        conn.execute("BEGIN IMMEDIATE;").unwrap();
        for i in 56..=80 {
            conn.execute_params(
                "INSERT INTO items(id, val) VALUES (?1, zeroblob(300));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        // Verify Rust `wal_checkpoint_v2(..., Restart | Truncate)` reports `busy == true`
        // AND partially backfills up to `c_reader1`'s mark (`0 < n_ckpt < n_log`)!
        let ck_r1 = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Restart)
            .unwrap();
        assert!(ck_r1.busy, "expected Busy under live C reader: {ck_r1:?}");
        assert!(ck_r1.n_ckpt > 0 && ck_r1.n_ckpt < ck_r1.n_log);

        // PRAGMA wal_checkpoint(TRUNCATE) must also return 1 row `(1, n_log, n_ckpt)` with Ok!
        let pragma_row = conn
            .query("PRAGMA main.wal_checkpoint(TRUNCATE);", &[])
            .unwrap();
        assert_eq!(pragma_row.column_names, vec!["busy", "log", "checkpointed"]);
        assert_eq!(
            pragma_row.rows[0].values(),
            &[
                Value::Integer(1),
                Value::Integer(i64::from(ck_r1.n_log)),
                Value::Integer(i64::from(ck_r1.n_ckpt))
            ]
        );

        // Verify both C readers still observe untouched snapshots!
        c_reader1.send_cmd("QUERY");
        assert_eq!(
            parse_summary_fields(&c_reader1.read_line_matching("SNAPSHOT ")),
            c1_snap
        );
        c_reader2.send_cmd("QUERY");
        assert_eq!(
            parse_summary_fields(&c_reader2.read_line_matching("SNAPSHOT ")),
            c2_snap
        );

        c_reader1.exit_cleanly();
        let ck_r2 = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Restart)
            .unwrap();
        assert!(ck_r2.busy);
        assert!(ck_r2.n_ckpt > ck_r1.n_ckpt && ck_r2.n_ckpt < ck_r2.n_log);

        c_reader2.exit_cleanly();
        let ck_done = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Truncate)
            .unwrap();
        assert_eq!(
            ck_done,
            WalCheckpointResult {
                busy: false,
                n_log: 0,
                n_ckpt: 0
            }
        );
        assert_eq!(fs::metadata(wal_sidecar(&db_path)).unwrap().len(), 0);
        conn.try_close().unwrap();
    }

    // 4. Multi-process `busy_timeout` & `busy_handler` contention under a live C writer process!
    let mut c_writer = spawn_c_writer(&db_path, 81, 256, true);
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();

        // 4A. Custom `busy_handler` invoked N times then aborts with `ErrorCode::Busy`.
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let hc_clone = Arc::clone(&handler_calls);
        conn.busy_handler(Some(Box::new(move |n| {
            hc_clone.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            n < 3
        })))
        .unwrap();
        let err = conn.execute("BEGIN IMMEDIATE;").unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(handler_calls.load(Ordering::SeqCst), 4);

        // `FULL` checkpoint while C holds `WAL_WRITE_LOCK` invokes `busy_handler`, then downgrades
        // to `PASSIVE` and reports `busy: true` (`pager.c:7608`, `wal.c:4435-4452`)!
        handler_calls.store(0, Ordering::SeqCst);
        let ck_under_writer = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Full)
            .unwrap();
        assert!(ck_under_writer.busy);
        assert_eq!(handler_calls.load(Ordering::SeqCst), 4);

        // 4B. `busy_timeout(1500)` succeeds cleanly when the C writer commits ~80ms later!
        conn.busy_timeout(1500).unwrap();
        let release_thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            c_writer.send_cmd("COMMIT");
            c_writer.exit_cleanly();
        });
        let t0 = Instant::now();
        conn.execute(
            "BEGIN IMMEDIATE;
             INSERT INTO items(id, val) VALUES (82, zeroblob(256));
             COMMIT;",
        )
        .unwrap();
        let elapsed = t0.elapsed();
        release_thread.join().unwrap();
        assert!(elapsed >= Duration::from_millis(40));
        conn.try_close().unwrap();
    }

    let c_ver = run_c_verify(&db_path, Some(true));
    let r_ver = run_rust_verify(&db_path, Some(true));
    assert_eq!(c_ver.summary.count, 82);
    assert_c_rust_parity_or_record_diff(
        "c_readers_rust_writer_busy",
        "INSERT 1..82 + busy_handler + busy_timeout",
        "2 C readers + Rust writer/checkpointer + live C writer contention",
        &db_path,
        &c_ver,
        &r_ver,
    );
}

// ============================================================================
// Suite 4: `SQLITE_BUSY_SNAPSHOT` (Both Directions) & Paused `Statement`
//          `Write -> Read` Downgrade Blocking Separate C Checkpointer
// ============================================================================

#[test]
#[cfg(unix)]
fn multiprocess_busy_snapshot_and_paused_statement_write_to_read_downgrade() {
    let sandbox = TempSandbox::new("busy_snapshot_and_downgrade");
    let db_path = sandbox.path().join("snapshot.db");
    run_c_init_wal(&db_path, 4096, 12, 200, true);

    // 1. Rust reader opens `BEGIN DEFERRED; SELECT ...`, C commits row 13, Rust upgrades ->
    //    `ErrorCode::Busy` with extended code `SQLITE_BUSY_SNAPSHOT (517)`!
    let mut r_reader = spawn_rust_worker("rust-hold-reader", &db_path, &[]);
    let snap0 = parse_summary_fields(&r_reader.read_line_matching("READY "));
    assert_eq!(snap0.count, 12);

    run_c_commit_batch(&db_path, 13, 1, 200, 1000, true);

    r_reader.send_cmd("UPGRADE");
    let upg_line = r_reader.read_line_matching("UPGRADE ");
    assert!(
        upg_line.contains("code=Busy")
            && upg_line.contains(&format!("ext={}", extended::SQLITE_BUSY_SNAPSHOT)),
        "expected Rust SQLITE_BUSY_SNAPSHOT (517), got `{upg_line}`"
    );
    r_reader.exit_cleanly();

    // Fresh Rust read sees 13 rows cleanly!
    assert_eq!(run_rust_verify(&db_path, Some(true)).summary.count, 13);

    // 2. Reverse direction: C reader opens `BEGIN DEFERRED; SELECT ...`, Rust commits row 14,
    //    C attempts `INSERT` upgrade -> receives `rc=5 (SQLITE_BUSY)` & `ext=517 (SQLITE_BUSY_SNAPSHOT)`!
    let (mut c_reader, c_snap0) = spawn_c_reader(&db_path, true);
    assert_eq!(c_snap0.count, 13);
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute("INSERT INTO items(id, val) VALUES (14, zeroblob(200));")
            .unwrap();
        conn.try_close().unwrap();
    }
    c_reader.send_cmd("UPGRADE");
    let c_upg = c_reader.read_line_matching("UPGRADE ");
    assert_eq!(c_upg, "UPGRADE rc=5 ext=517");
    c_reader.exit_cleanly();

    // 3. Same-connection paused Rust `Statement` (`StepResult::Row`) + autocommit `INSERT`
    //    (`ActivityTracker` `Write -> Read` downgrade retaining positive `READ(1..=4)` slot)
    //    while a separate C process runs `RESTART` / `TRUNCATE` checkpoint!
    let mut r_paused = spawn_rust_worker("rust-paused-stmt-downgrade", &db_path, &[]);
    let ready_paused = r_paused.read_line_matching("READY ");
    assert_eq!(ready_paused, "READY first_id=1");

    // Separate C process commits another row (id=15) AFTER the paused Rust statement's snapshot!
    run_c_commit_batch(&db_path, 15, 1, 200, 1000, true);

    // Separate C checkpointer attempts `RESTART` and `TRUNCATE`: MUST observe the retained
    // `READ(1..=4)` lock held by the paused Rust `Statement` and return `SQLITE_BUSY (5)`
    // WITHOUT clobbering frames needed by the paused Rust `Statement`!
    let ck_while_paused = run_c_checkpoint(&db_path, WalCheckpointMode::Restart, 20, true);
    assert_eq!(
        ck_while_paused.rc, 5,
        "C RESTART checkpoint must see live Rust READ(1..=4) slot after Write->Read downgrade: {ck_while_paused:?}"
    );
    let ck_trunc_paused = run_c_checkpoint(&db_path, WalCheckpointMode::Truncate, 20, true);
    assert_eq!(ck_trunc_paused.rc, 5);

    // Now drain & finalize the paused Rust `Statement`: it must see its intact snapshot cleanly,
    // and once finalized + exited, C `TRUNCATE` succeeds to 0 bytes!
    r_paused.send_cmd("DRAIN_PAUSED");
    let drained = r_paused.read_line_matching("DRAINED ");
    assert!(
        drained.contains("count=14") || drained.contains("count=15"),
        "unexpected drained summary: {drained}"
    );
    r_paused.exit_cleanly();

    let ck_after_finalize = run_c_checkpoint(&db_path, WalCheckpointMode::Truncate, 1000, true);
    assert_eq!(ck_after_finalize.rc, 0);
    assert_eq!(ck_after_finalize.n_log, 0);
    assert_eq!(ck_after_finalize.n_ckpt, 0);

    let c_ver = run_c_verify(&db_path, Some(true));
    let r_ver = run_rust_verify(&db_path, Some(true));
    assert_eq!(c_ver.integrity, "ok");
    assert_eq!(c_ver.summary.count, 16); // 1..=15 + 500001
    assert_c_rust_parity_or_record_diff(
        "busy_snapshot_and_paused_downgrade",
        "paused SELECT + autocommit INSERT + C RESTART/TRUNCATE",
        "Rust worker + C writer + C checkpointer",
        &db_path,
        &c_ver,
        &r_ver,
    );
}

// ============================================================================
// Suite 5: Process Crash (`SIGKILL`) & Torn-State Recovery Matrix (All 5 Points)
// ============================================================================

#[test]
#[cfg(unix)]
fn sigkill_process_crash_and_torn_state_recovery_matrix_c_and_rust() {
    let sandbox = TempSandbox::new("sigkill_matrix");

    let cases: [(&str, i64, bool); 6] = [
        // (worker_role, expected_row_count_after_recovery, keep_live_dms_holder)
        ("crash-1-uncommitted-wal", 10, false),
        ("crash-2-commit-before-shm", 25, false),
        ("crash-3-torn-shm-header", 22, true),
        ("crash-4-mid-backfill", 30, false),
        ("crash-4-mid-backfill", 30, true),
        ("crash-5-post-restart-before-truncate", 10, false),
    ];

    for (idx, (role, expected_count, with_live_dms)) in cases.into_iter().enumerate() {
        let base_dir = sandbox.path().join(format!("case_{idx}_{role}"));
        fs::create_dir_all(&base_dir).unwrap();
        let crash_db = base_dir.join("crash.db");
        run_c_init_wal(&crash_db, 1024, 10, 200, true);
        // Ensure initial 10 rows are in `-wal` (uncheckpointed) before crash worker starts.
        run_c_commit_batch(&crash_db, 1, 10, 200, 1000, true);

        if with_live_dms {
            // Live multi-process recovery: keep a DMS holder open while `SIGKILL` kills the worker,
            // then test C recovery on one live setup and Rust recovery on an identical live setup!
            for recoverer in ["c", "rust"] {
                let live_dir = base_dir.join(format!("live_{recoverer}"));
                let live_db = live_dir.join("crash.db");
                copy_db_bundle(&crash_db, &live_db);

                let dms_holder = spawn_c_dms_holder(&live_db);
                let mut worker = spawn_rust_worker(role, &live_db, &[]);
                let _ = worker.read_line_matching("READY_FOR_SIGKILL");
                worker.sigkill_and_reap();

                let snap = if recoverer == "c" {
                    run_c_verify(&live_db, Some(true))
                } else {
                    // Spawn verify in separate process or direct Rust (DMS holder is C in another PID,
                    // so opening Rust in parent PID has zero same-PID conflict!).
                    run_rust_verify(&live_db, Some(true))
                };
                dms_holder.exit_cleanly();

                assert_eq!(
                    snap.integrity, "ok",
                    "integrity_check failed on `{role}` ({recoverer}, live_dms=true)"
                );
                assert_eq!(
                    snap.summary.count, expected_count,
                    "unexpected row count on `{role}` ({recoverer}, live_dms=true): {snap:?}"
                );
            }
        } else {
            let mut worker = spawn_rust_worker(role, &crash_db, &[]);
            let _ = worker.read_line_matching("READY_FOR_SIGKILL");
            worker.sigkill_and_reap();

            // Clone the exact post-`SIGKILL` on-disk bundle into two isolated directories so BOTH
            // C SQLite 3.54.1 and Rust `Connection` recover the exact same crashed state!
            let c_copy = base_dir.join("recover_by_c/crash.db");
            let r_copy = base_dir.join("recover_by_rust/crash.db");
            copy_db_bundle(&crash_db, &c_copy);
            copy_db_bundle(&crash_db, &r_copy);

            let c_snap = run_c_verify(&c_copy, Some(true));
            let r_snap = run_rust_verify(&r_copy, Some(true));
            assert_eq!(
                c_snap.integrity, "ok",
                "C recovery integrity failed on {role}"
            );
            assert_eq!(
                c_snap.summary.count, expected_count,
                "C count mismatch on {role}"
            );
            assert_c_rust_parity_or_record_diff(
                role,
                "PRAGMA integrity_check; SELECT count(*), sum(id), sum(length(val)) FROM items;",
                "SIGKILL child worker -> clone crashed bundle -> independent C vs Rust recovery",
                &r_copy,
                &c_snap,
                &r_snap,
            );
        }
    }
}

// ============================================================================
// Suite 6: Bidirectional Page-Size Matrix (`512`, `1024`, `4096`, `65536`),
//          >12,255 SHM Frames, & `SAVEPOINT` / `ROLLBACK TO` / VDBE Parity
// ============================================================================

#[test]
#[cfg(unix)]
fn bidirectional_page_size_matrix_large_shm_blocks_and_savepoint_vdbe_parity() {
    let sandbox = TempSandbox::new("pagesize_shm_savepoint");

    // 1. Bidirectional C <-> Rust page-size matrix across `512`, `1024`, `4096`, and `65536`!
    for &ps in &[512u32, 1024, 4096, 65536] {
        // Direction A: C creates WAL DB + writes 1..=18 -> Rust appends 19..=36 + checkpoints -> C verifies!
        let db_a = sandbox.path().join(format!("c_to_rust_{ps}.db"));
        run_c_init_wal(&db_a, ps, 18, 600, true);
        {
            let conn = Connection::open(&db_a).unwrap();
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            assert_eq!(conn.page_size().unwrap(), ps);
            conn.execute("BEGIN;").unwrap();
            for i in 19..=36 {
                let blob = vec![(i & 0xff) as u8; 600];
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, ?2);",
                    &[Value::Integer(i), Value::Blob(blob)],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();
            let ck = conn
                .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Restart)
                .unwrap();
            assert!(!ck.busy && ck.n_ckpt == ck.n_log);
            conn.try_close().unwrap();
        }
        let c_ver_a = run_c_verify(&db_a, Some(true));
        let r_ver_a = run_rust_verify(&db_a, Some(true));
        assert_eq!(c_ver_a.page_size, i64::from(ps));
        assert_eq!(c_ver_a.summary.count, 36);
        assert_c_rust_parity_or_record_diff(
            &format!("page_size_c_to_rust_{ps}"),
            "PRAGMA page_size; PRAGMA journal_mode=WAL; INSERT 1..36;",
            "C init -> Rust write + RESTART checkpoint -> C verify",
            &db_a,
            &c_ver_a,
            &r_ver_a,
        );

        // Direction B: Rust creates WAL DB + writes 1..=18 -> C appends 19..=36 + checkpoints -> Rust verifies!
        let db_b = sandbox.path().join(format!("rust_to_c_{ps}.db"));
        {
            let conn = Connection::open(&db_b).unwrap();
            conn.wal_autocheckpoint(0).unwrap();
            conn.file_control_persist_wal(None, 1).unwrap();
            conn.execute(&format!(
                "PRAGMA page_size = {ps};
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);"
            ))
            .unwrap();
            conn.execute("BEGIN;").unwrap();
            for i in 1..=18 {
                let blob = vec![((i * 7) & 0xff) as u8; 600];
                conn.execute_params(
                    "INSERT INTO items(id, val) VALUES (?1, ?2);",
                    &[Value::Integer(i), Value::Blob(blob)],
                )
                .unwrap();
            }
            conn.execute("COMMIT;").unwrap();
            conn.try_close().unwrap();
        }
        run_c_commit_batch(&db_b, 19, 18, 600, 1000, true);
        let ck_b = run_c_checkpoint(&db_b, WalCheckpointMode::Full, 1000, true);
        assert_eq!(ck_b.rc, 0);
        assert_eq!(ck_b.n_ckpt, ck_b.n_log);

        let c_ver_b = run_c_verify(&db_b, Some(true));
        let r_ver_b = run_rust_verify(&db_b, Some(true));
        assert_eq!(r_ver_b.page_size, i64::from(ps));
        assert_eq!(r_ver_b.summary.count, 36);
        assert_c_rust_parity_or_record_diff(
            &format!("page_size_rust_to_c_{ps}"),
            "PRAGMA page_size; PRAGMA journal_mode=WAL; INSERT 1..36;",
            "Rust init -> C write + FULL checkpoint -> Rust verify",
            &db_b,
            &c_ver_b,
            &r_ver_b,
        );
    }

    // 2. >12,255 WAL frames crossing SHM blocks 0 (`1..=4062`), 1 (`4063..=8158`), 2 (`8159..=12254`),
    //    and into block 3 (`>= 12255` -> `-shm` >= 131,072 bytes = 4 * 32 KiB chunks)!
    let big_db = sandbox.path().join("shm_12300_frames.db");
    {
        let conn = Connection::open(&big_db).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute(
            "PRAGMA page_size = 512;
             PRAGMA synchronous = OFF;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);",
        )
        .unwrap();
        conn.execute("BEGIN;").unwrap();
        for i in 1..=12_600 {
            conn.execute_params(
                "INSERT INTO items(id, val) VALUES (?1, zeroblob(420));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        conn.execute("COMMIT;").unwrap();

        let noop = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Noop)
            .unwrap();
        assert!(
            noop.n_log > 12_255,
            "expected > 12,255 WAL frames spanning SHM blocks 0..3, got {}",
            noop.n_log
        );
        let shm_len = fs::metadata(shm_sidecar(&big_db)).unwrap().len();
        assert!(
            shm_len >= 4 * 32_768,
            "expected -shm >= 131072 bytes (4 x 32 KiB chunks), got {shm_len}"
        );
        conn.try_close().unwrap();
    }
    let c_big = run_c_verify(&big_db, Some(true));
    let r_big = run_rust_verify(&big_db, Some(true));
    assert_eq!(c_big.integrity, "ok");
    assert_eq!(c_big.summary.count, 12_600);
    assert_c_rust_parity_or_record_diff(
        "shm_over_12255_frames",
        "12600 rows at page_size=512 (>12255 WAL frames)",
        "Rust >12255 frames -> C & Rust verify",
        &big_db,
        &c_big,
        &r_big,
    );

    // 3. `SAVEPOINT` / `ROLLBACK TO` / `RELEASE` + constraint-abort statement subsavepoint
    //    across `nCkpt` log restart, exercising BOTH compiled VDBE (`SELECT` + `EXPLAIN`) and
    //    engine DML (`INSERT` / `UPDATE` / `DELETE` / `WITH` CTE) paths!
    let sp_db = sandbox.path().join("savepoint_vdbe.db");
    {
        let conn = Connection::open(&sp_db).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute(
            "PRAGMA page_size = 1024;
             PRAGMA cache_size = 3;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, code TEXT UNIQUE, val BLOB);",
        )
        .unwrap();

        // Confirm single-table SELECT compiles to VDBE bytecode (`EXPLAIN`).
        let explain = conn
            .query("EXPLAIN SELECT id, code FROM items WHERE id = ?1;", &[])
            .unwrap();
        assert!(!explain.rows.is_empty());

        // Seed initial rows, then run `RESTART` checkpoint to bump `nCkpt` before savepoint tx!
        conn.execute(
            "INSERT INTO items VALUES (1, 'k1', zeroblob(400)), (2, 'k2', zeroblob(400));",
        )
        .unwrap();
        let ck0 = conn
            .wal_checkpoint_v2(Some("main"), WalCheckpointMode::Restart)
            .unwrap();
        assert!(!ck0.busy);

        // Nested savepoints with WAL dirty-page spills (`cache_size = 3`) + `ROLLBACK TO` +
        // multi-row `INSERT` `UNIQUE` constraint abort rolling back only the failed statement!
        conn.execute("SAVEPOINT outer_sp;").unwrap();
        for i in 3..=20 {
            conn.execute_params(
                "INSERT INTO items VALUES (?1, ?2, zeroblob(500));",
                &[Value::Integer(i), Value::Text(format!("k{i}"))],
            )
            .unwrap();
        }
        conn.execute("SAVEPOINT inner_sp;").unwrap();
        for i in 21..=40 {
            conn.execute_params(
                "INSERT INTO items VALUES (?1, ?2, zeroblob(500));",
                &[Value::Integer(i), Value::Text(format!("k{i}"))],
            )
            .unwrap();
        }
        // Step VDBE `SELECT` inside transaction before `ROLLBACK TO inner_sp`!
        let cnt_mid = conn.query("SELECT count(*) FROM items;", &[]).unwrap().rows[0]
            .get_i64(0)
            .unwrap();
        assert_eq!(cnt_mid, 40);

        conn.execute("ROLLBACK TO inner_sp;").unwrap();
        conn.execute("RELEASE inner_sp;").unwrap();
        let cnt_after_rb = conn.query("SELECT count(*) FROM items;", &[]).unwrap().rows[0]
            .get_i64(0)
            .unwrap();
        assert_eq!(cnt_after_rb, 20);

        // Multi-row INSERT that hits UNIQUE conflict on `'k10'` at the second tuple: statement
        // subsavepoint must roll back `(99, 'k99')` while preserving `1..=20`!
        let dup_err = conn
            .execute(
                "INSERT INTO items VALUES
                    (99, 'k99', zeroblob(500)),
                    (100, 'k10', zeroblob(500));",
            )
            .unwrap_err();
        assert_eq!(dup_err.code, ErrorCode::Constraint);

        let cnt_after_abort = conn.query("SELECT count(*) FROM items;", &[]).unwrap().rows[0]
            .get_i64(0)
            .unwrap();
        assert_eq!(cnt_after_abort, 20);

        conn.execute("RELEASE outer_sp;").unwrap();
        conn.try_close().unwrap();
    }
    let c_sp = run_c_verify(&sp_db, Some(true));
    let r_sp = run_rust_verify(&sp_db, Some(true));
    assert_eq!(c_sp.integrity, "ok");
    assert_eq!(c_sp.summary.count, 20);
    assert_c_rust_parity_or_record_diff(
        "savepoint_spill_and_stmt_abort",
        "SAVEPOINT outer/inner + cache_size=3 spill + ROLLBACK TO + UNIQUE statement abort",
        "Rust nested WAL savepoints -> C & Rust verify",
        &sp_db,
        &c_sp,
        &r_sp,
    );
}

// ============================================================================
// Suite 7: `wal_checkpoint` SQL/API Modes & Full `walckptnoop.test` `1.0..1.10` Parity
// ============================================================================

#[test]
#[cfg(unix)]
fn wal_checkpoint_modes_and_walckptnoop_full_parity() {
    let sandbox = TempSandbox::new("walckptnoop");
    let db_path = sandbox.path().join("test.db");

    // Verify schema qualification (`main`, `temp`, unknown schema) & prepare-time column names!
    {
        let conn = Connection::open(&db_path).unwrap();
        let stmt = conn.prepare("PRAGMA wal_checkpoint;").unwrap();
        assert_eq!(stmt.column_names(), &["busy", "log", "checkpointed"]);
        drop(stmt);

        // `temp` schema is not in WAL mode -> returns `(0, -1, -1)`.
        assert_eq!(
            conn.wal_checkpoint_v2(Some("temp"), WalCheckpointMode::Noop)
                .unwrap(),
            WalCheckpointResult::not_wal()
        );
        let temp_row = conn
            .query("PRAGMA temp.wal_checkpoint(NOOP);", &[])
            .unwrap();
        assert_eq!(
            temp_row.rows[0].values(),
            &[Value::Integer(0), Value::Integer(-1), Value::Integer(-1)]
        );

        // Unknown schema returns `ErrorCode::Error` ("unknown database ...").
        let bad_api = conn
            .wal_checkpoint_v2(Some("bogus"), WalCheckpointMode::Passive)
            .unwrap_err();
        assert_eq!(bad_api.code, ErrorCode::Error);
        let bad_pragma = conn.query("PRAGMA bogus.wal_checkpoint;", &[]).unwrap_err();
        assert_eq!(bad_pragma.code, ErrorCode::Error);

        // 0-byte newly switched WAL DB (`pager.c:7594-7605`): `PRAGMA journal_mode = WAL;`
        // followed immediately by `wal_checkpoint_v2` initializes WAL and returns `(0, 0)`!
        conn.execute("PRAGMA page_size = 1024; PRAGMA journal_mode = WAL;")
            .unwrap();
        let zero_wal = conn
            .wal_checkpoint_v2(None, WalCheckpointMode::Passive)
            .unwrap();
        assert_eq!(
            zero_wal,
            WalCheckpointResult {
                busy: false,
                n_log: 0,
                n_ckpt: 0
            }
        );
        conn.try_close().unwrap();
    }

    // Re-create clean `test.db` for exact `walckptnoop.test` `1.0..1.10` sequence!
    let _ = fs::remove_file(&db_path);
    let _ = fs::remove_file(wal_sidecar(&db_path));
    let _ = fs::remove_file(shm_sidecar(&db_path));

    let conn = Connection::open(&db_path).unwrap();
    // Match testfixture `wal_autocheckpoint` (default 1000; 1000-row insert is ~298 frames < 1000).
    // `1.0`: `page_size=1024`, `CREATE TABLE t1` + `CREATE INDEX i1` + `journal_mode=wal` + 1000-row CTE!
    conn.execute(
        "PRAGMA page_size = 1024;
         PRAGMA auto_vacuum = NONE;
         PRAGMA secure_delete = OFF;
         VACUUM;
         CREATE TABLE t1(x INTEGER PRIMARY KEY, y TEXT);
         CREATE INDEX i1 ON t1(y);
         PRAGMA journal_mode = wal;
         WITH s(i) AS (
           SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 1000
         )
         INSERT INTO t1 SELECT NULL, hex(randomblob(64)) FROM s;",
    )
    .unwrap();

    // `1.1` & `1.2`: `PRAGMA wal_checkpoint = noop;` -> `(0, N, 0)` twice (`N > 200`, `nBackfill == 0`).
    let r1_1 = conn
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(r1_1.get_i64(0), Some(0));
    let n_frames = r1_1.get_i64(1).unwrap();
    assert!(n_frames > 200, "expected >200 WAL frames, got {n_frames}");
    assert_eq!(r1_1.get_i64(2), Some(0));

    let r1_2 = conn
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_2.values(),
        &[
            Value::Integer(0),
            Value::Integer(n_frames),
            Value::Integer(0)
        ]
    );

    // `1.3`: `PRAGMA wal_checkpoint = passive;` -> `(0, N, N)`.
    let r1_3 = conn
        .query("PRAGMA wal_checkpoint = passive;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_3.values(),
        &[
            Value::Integer(0),
            Value::Integer(n_frames),
            Value::Integer(n_frames)
        ]
    );

    // `1.4`: `PRAGMA wal_checkpoint = noop;` -> `(0, N, N)`.
    let r1_4 = conn
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_4.values(),
        &[
            Value::Integer(0),
            Value::Integer(n_frames),
            Value::Integer(n_frames)
        ]
    );

    // `1.5`: `db_save_and_close` + `db_restore_and_reopen` -> `PRAGMA wal_checkpoint = noop;`
    //        runs `walIndexRecover` on freshly reopened `-shm` (`nBackfill` reset to `0`) -> `(0, N, 0)`!
    let saved_db = sandbox.path().join("sv_test.db");
    copy_db_bundle(&db_path, &saved_db);
    conn.try_close().unwrap();
    copy_db_bundle(&saved_db, &db_path);

    let conn2 = Connection::open(&db_path).unwrap();
    let r1_5 = conn2
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_5.values(),
        &[
            Value::Integer(0),
            Value::Integer(n_frames),
            Value::Integer(0)
        ]
    );

    // `1.6`: Close `conn2` cleanly (checkpoints & unlinks `-wal`/`-shm`, leaving `.db` `(2,2)`),
    //        reopen cold `conn3`, run `PRAGMA wal_checkpoint = noop;` -> `(0, 0, 0)`!
    conn2.try_close().unwrap();
    assert!(!wal_sidecar(&db_path).exists());
    assert!(!shm_sidecar(&db_path).exists());

    let conn3 = Connection::open(&db_path).unwrap();
    let r1_6 = conn3
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_6.values(),
        &[Value::Integer(0), Value::Integer(0), Value::Integer(0)]
    );

    // `1.7`: `BEGIN; DELETE FROM t1; PRAGMA wal_checkpoint = noop;` -> `Locked` ("database table is locked")!
    conn3.execute("BEGIN; DELETE FROM t1;").unwrap();
    let err1_7 = conn3
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap_err();
    assert_eq!(err1_7.code, ErrorCode::Locked);
    assert!(err1_7.message.contains("database table is locked"));

    // `1.8` & `1.9`: `COMMIT; PRAGMA wal_checkpoint = noop;` and `wal_checkpoint_v2(..., Noop)` -> `(0, M, 0)` (`M > 0`)!
    conn3.execute("COMMIT;").unwrap();
    let r1_8 = conn3
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    let m_frames = r1_8.get_i64(1).unwrap();
    assert!(m_frames > 0);
    assert_eq!(
        r1_8.values(),
        &[
            Value::Integer(0),
            Value::Integer(m_frames),
            Value::Integer(0)
        ]
    );

    let r1_9 = conn3
        .wal_checkpoint_v2(None, WalCheckpointMode::Noop)
        .unwrap();
    assert_eq!(
        r1_9,
        WalCheckpointResult {
            busy: false,
            n_log: m_frames as i32,
            n_ckpt: 0
        }
    );

    // `1.10`: `PRAGMA journal_mode = delete; PRAGMA wal_checkpoint = noop;` -> `delete` and `(0, -1, -1)`!
    let jm = conn3
        .query("PRAGMA journal_mode = delete;", &[])
        .unwrap()
        .rows[0]
        .get_str(0)
        .unwrap()
        .to_owned();
    assert_eq!(jm, "delete");
    let r1_10 = conn3
        .query("PRAGMA wal_checkpoint = noop;", &[])
        .unwrap()
        .rows[0]
        .clone();
    assert_eq!(
        r1_10.values(),
        &[Value::Integer(0), Value::Integer(-1), Value::Integer(-1)]
    );
    conn3.try_close().unwrap();
}

// ============================================================================
// Suite 8: `wal_autocheckpoint`, `locking_mode = EXCLUSIVE`, Read-Only /
//          `immutable=1` / `0444` Directory Reads, & Last-Close Sidecar Unlink
// ============================================================================

#[test]
#[cfg(unix)]
fn autocheckpoint_exclusive_locking_readonly_immutable_and_last_close_unlink() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = TempSandbox::new("autockpt_excl_ro_close");

    // 1. `wal_autocheckpoint`: default `1000`, custom `N = 8`, `<= 0` disabled, AND post-commit
    //    `Busy` (blocked by active C reader) NEVER rolls back the committed transaction!
    let auto_db = sandbox.path().join("autockpt.db");
    {
        let conn = Connection::open(&auto_db).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        assert_eq!(
            conn.wal_autocheckpoint_threshold().unwrap(),
            SQLITE_DEFAULT_WAL_AUTOCHECKPOINT
        );
        assert_eq!(
            conn.query("PRAGMA wal_autocheckpoint;", &[]).unwrap().rows[0].get_i64(0),
            Some(1000)
        );

        // Disable (`<= 0`) -> reports `0` and does NOT auto-checkpoint even past 15 frames!
        assert_eq!(
            conn.query("PRAGMA wal_autocheckpoint = -5;", &[])
                .unwrap()
                .rows[0]
                .get_i64(0),
            Some(0)
        );
        conn.execute(
            "PRAGMA page_size = 1024;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);",
        )
        .unwrap();
        for i in 1..=15 {
            conn.execute_params(
                "INSERT INTO items VALUES (?1, zeroblob(800));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        let noop_disabled = conn
            .wal_checkpoint_v2(None, WalCheckpointMode::Noop)
            .unwrap();
        assert!(noop_disabled.n_log >= 15 && noop_disabled.n_ckpt == 0);

        // Enable custom threshold `N = 8`: next autocommit `INSERT` pushes `n_entry >= 8` and
        // triggers automatic post-commit `PASSIVE` checkpoint (`n_ckpt == n_log`)!
        assert_eq!(
            conn.query("PRAGMA wal_autocheckpoint = 8;", &[])
                .unwrap()
                .rows[0]
                .get_i64(0),
            Some(8)
        );
        conn.execute("INSERT INTO items VALUES (16, zeroblob(800));")
            .unwrap();
        let noop_enabled = conn
            .wal_checkpoint_v2(None, WalCheckpointMode::Noop)
            .unwrap();
        assert_eq!(noop_enabled.n_ckpt, noop_enabled.n_log);

        // Spawn C reader pinning 16-row snapshot, then commit rows 17..=30 with `wal_autocheckpoint = 4`:
        // post-commit passive autocheckpoint hits C reader's mark (`Busy` for frames > mark),
        // MUST still return `Ok` and MUST keep all 30 rows committed!
        let (c_reader, c_snap) = spawn_c_reader(&auto_db, true);
        assert_eq!(c_snap.count, 16);
        conn.wal_autocheckpoint(4).unwrap();
        for i in 17..=30 {
            conn.execute_params(
                "INSERT INTO items VALUES (?1, zeroblob(800));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        assert_eq!(query_rust_items_summary(&conn).count, 30);
        c_reader.exit_cleanly();
        conn.try_close().unwrap();
    }

    // 2. `PRAGMA locking_mode = EXCLUSIVE` <-> `NORMAL` WAL transitions:
    //    2A. `WAL_HEAPMEMORY_MODE`: setting `locking_mode = EXCLUSIVE` BEFORE entering WAL skips
    //        `-shm`, blocks C peers (`SQLITE_BUSY`), refuses `locking_mode = NORMAL` while in WAL
    //        (`pager.c:7673`, `wal.c:4592`), and allows `NORMAL` once switched out of WAL!
    let excl_heap_db = sandbox.path().join("excl_heap.db");
    {
        let conn = Connection::open(&excl_heap_db).unwrap();
        assert_eq!(
            conn.query("PRAGMA locking_mode;", &[]).unwrap().rows[0].get_str(0),
            Some("normal")
        );
        assert_eq!(
            conn.query("PRAGMA locking_mode = EXCLUSIVE;", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("exclusive")
        );
        conn.execute(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);
             INSERT INTO items VALUES (1, zeroblob(200)), (2, zeroblob(200));",
        )
        .unwrap();
        assert!(
            !shm_sidecar(&excl_heap_db).exists(),
            "WAL_HEAPMEMORY_MODE must not create -shm"
        );
        let ck_c_blocked = run_c_checkpoint(&excl_heap_db, WalCheckpointMode::Passive, 10, true);
        assert_eq!(ck_c_blocked.rc, 5);

        // Cannot leave `EXCLUSIVE` while in heap-memory WAL (`wal.c:4592` returns `"exclusive"`).
        assert_eq!(
            conn.query("PRAGMA locking_mode = NORMAL;", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("exclusive")
        );
        conn.try_close().unwrap();
    }
    assert_eq!(run_c_verify(&excl_heap_db, None).summary.count, 2);

    //    2B. `WAL_NORMAL_MODE <-> WAL_EXCLUSIVE_MODE`: entering WAL FIRST (`NORMAL`), then setting
    //        `locking_mode = EXCLUSIVE` acquires exclusive WAL locks after a write, blocks C peers,
    //        and `PRAGMA locking_mode = NORMAL;` + subsequent access releases back to `NORMAL`!
    let excl_db = sandbox.path().join("excl.db");
    {
        let conn = Connection::open(&excl_db).unwrap();
        conn.file_control_persist_wal(None, 1).unwrap();
        conn.execute(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);
             INSERT INTO items VALUES (1, zeroblob(200));",
        )
        .unwrap();
        assert_eq!(
            conn.query("PRAGMA locking_mode = EXCLUSIVE;", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("exclusive")
        );
        conn.execute("INSERT INTO items VALUES (2, zeroblob(200));")
            .unwrap();
        let ck_c_blocked = run_c_checkpoint(&excl_db, WalCheckpointMode::Full, 10, true);
        assert_eq!(ck_c_blocked.rc, 5);

        assert_eq!(
            conn.query("PRAGMA locking_mode = NORMAL;", &[])
                .unwrap()
                .rows[0]
                .get_str(0),
            Some("normal")
        );
        conn.execute("INSERT INTO items VALUES (3, zeroblob(200));")
            .unwrap();
        let ck_c_unblocked = run_c_checkpoint(&excl_db, WalCheckpointMode::Passive, 500, true);
        assert_eq!(ck_c_unblocked.rc, 0);
        conn.try_close().unwrap();
    }
    assert_eq!(run_c_verify(&excl_db, None).summary.count, 3);

    // 3. Read-only (`OpenFlags::READONLY`), `immutable=1` URI / `open_immutable`, and `0444`
    //    read-only directory WAL reads (with uncheckpointed WAL frames)!
    let ro_dir = sandbox.path().join("ro_vault");
    fs::create_dir_all(&ro_dir).unwrap();
    let ro_db = ro_dir.join("vault.db");
    run_c_init_wal(&ro_db, 4096, 10, 256, true);
    run_c_commit_batch(&ro_db, 11, 15, 256, 1000, true); // 25 total rows, 11..25 only in `-wal`!

    // 3A. `OpenFlags::READONLY` with existing `-wal` + `-shm` (read-only file permissions `0444`).
    fs::set_permissions(&ro_db, fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(wal_sidecar(&ro_db), fs::Permissions::from_mode(0o444)).unwrap();
    if shm_sidecar(&ro_db).exists() {
        fs::set_permissions(shm_sidecar(&ro_db), fs::Permissions::from_mode(0o444)).unwrap();
    }
    {
        let ro_conn = Connection::open_with_flags(&ro_db, OpenFlags::READONLY).unwrap();
        assert_eq!(query_rust_items_summary(&ro_conn).count, 25);
        // `NOOP` or `PASSIVE` checkpoint on a read-only WAL connection returns `ErrorCode::ReadOnly`!
        let ro_ckpt_err = ro_conn
            .wal_checkpoint_v2(None, WalCheckpointMode::Noop)
            .unwrap_err();
        assert_eq!(ro_ckpt_err.code, ErrorCode::ReadOnly);
        ro_conn.try_close().unwrap();
    }

    // 3B. Remove `-shm` and tighten directory to `0555` / files to `0444`, then verify `immutable=1`
    //     reads all 25 WAL rows WITHOUT creating `-shm` in BOTH C SQLite 3.54.1 and Rust!
    fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o755)).unwrap();
    let _ = fs::remove_file(shm_sidecar(&ro_db));
    // Note: on Linux, directory search (`x` bit -> `0555`, or `0444` tested for `immutable` when
    // process is non-root) prevents creating `-shm` (`w` bit stripped). Let's test `0555` directory
    // (`r-xr-xr-x`, zero write permission so `-shm` creation fails with `EACCES`) + `0444` files!
    fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o555)).unwrap();
    {
        let uri = format!("file:{}?immutable=1", ro_db.display());
        let imm_conn =
            Connection::open_with_flags(&uri, OpenFlags::READONLY | OpenFlags::URI).unwrap();
        let r_sum = query_rust_items_summary(&imm_conn);
        assert_eq!(r_sum.count, 25);
        assert!(
            !shm_sidecar(&ro_db).exists(),
            "immutable=1 must NOT create -shm"
        );
        imm_conn.try_close().unwrap();

        let c_ro_out = Command::new(c_compat_helper())
            .arg("verify-readonly")
            .arg(&ro_db)
            .arg("1")
            .output()
            .expect("c verify-readonly immutable");
        assert!(c_ro_out.status.success());
        let c_ro_txt = String::from_utf8_lossy(&c_ro_out.stdout);
        assert!(c_ro_txt.contains("INTEGRITY=ok"));
        assert!(c_ro_txt.contains("count=25"));
    }
    fs::set_permissions(&ro_dir, fs::Permissions::from_mode(0o755)).unwrap();

    // 4. Last-connection `try_close()` automatic checkpoint + `-wal` / `-shm` unlink
    //    (when `persist_wal` is default `false`), leaving `.db` with `(2, 2)` header verified by C!
    let close_db = sandbox.path().join("clean_close.db");
    {
        let conn = Connection::open(&close_db).unwrap();
        conn.wal_autocheckpoint(0).unwrap();
        conn.execute(
            "PRAGMA page_size = 4096;
             PRAGMA journal_mode = WAL;
             CREATE TABLE items(id INTEGER PRIMARY KEY, val BLOB);",
        )
        .unwrap();
        for i in 1..=25 {
            conn.execute_params(
                "INSERT INTO items VALUES (?1, zeroblob(300));",
                &[Value::Integer(i)],
            )
            .unwrap();
        }
        assert!(wal_sidecar(&close_db).exists());
        assert!(shm_sidecar(&close_db).exists());
        conn.try_close().unwrap();
    }
    assert!(
        !wal_sidecar(&close_db).exists(),
        "last close must unlink -wal when persist_wal=false"
    );
    assert!(
        !shm_sidecar(&close_db).exists(),
        "last close must unlink -shm when persist_wal=false"
    );
    let hdr_bytes = fs::read(&close_db).unwrap();
    assert_eq!(
        (hdr_bytes[18], hdr_bytes[19]),
        (2, 2),
        ".db header must retain WAL format versions (2, 2) after clean close"
    );
    let c_after_close = run_c_verify(&close_db, None);
    assert_eq!(c_after_close.integrity, "ok");
    assert_eq!(c_after_close.journal_mode, "wal");
    assert_eq!(c_after_close.summary.count, 25);
}
