#!/usr/bin/env python3
"""Deterministic Group 60 wrapper & manifest generator/verifier for C SQLite `.test` suites.

Scans configured C SQLite test roots (extensible across Phases 1.2..7 across `sqlite/test`,
`sqlite/ext/fts5/test`, and `sqlite/ext/rtree` using canonical `root + "/" + relative_path`
identity so module names never collide), writes/verifies `tests/sqlite_test_manifest.json`,
generates `#![forbid(unsafe_code)]` Rust wrappers in `tests/sqlite_test/*.rs` invoking the
unmodified C `.test` files via `include_str!` + `strip_test_comments`, and synchronizes
module registrations in `tests/sqlite_test_suite.rs`.

Usage:
  python3 tools/generate_sqlite_test_wrappers.py           # generate / update
  python3 tools/generate_sqlite_test_wrappers.py --check   # verify 100% in sync (no writes)
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
MANIFEST_PATH = REPO_ROOT / "tests" / "sqlite_test_manifest.json"
WRAPPER_DIR = REPO_ROOT / "tests" / "sqlite_test"
SUITE_RS_PATH = REPO_ROOT / "tests" / "sqlite_test_suite.rs"

RUST_KEYWORDS = {
    "as",
    "async",
    "await",
    "box",
    "break",
    "const",
    "continue",
    "crate",
    "default",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "gen",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "macro",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "try",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
    "yield",
}

# Supported multi-root directories (`implementation_plan.md` Group 60).
# Non-default roots get a deterministic prefix so stems from `fts5` or `rtree` never collide
# with `sqlite/test`.
ROOT_PREFIX: dict[str, str] = {
    "sqlite/test": "",
    "sqlite/ext/fts5/test": "fts5_ext_",
    "sqlite/ext/rtree": "rtree_ext_",
}

# Configured scan rules per phase (`(source_root, glob_pattern, default_phase)`).
SCAN_RULES: list[tuple[str, str, str]] = [
    ("sqlite/test", "wal*.test", "1.2"),
    ("sqlite/test", "autovacuum*.test", "1.3"),
    ("sqlite/test", "incrvacuum*.test", "1.3"),
    ("sqlite/test", "attach*.test", "2"),
    ("sqlite/test", "tempdb*.test", "2"),
    ("sqlite/test", "view*.test", "3"),
    ("sqlite/test", "upsert*.test", "3"),
    ("sqlite/test", "upfrom*.test", "3"),
    ("sqlite/test", "alterdropcol*.test", "3"),
    ("sqlite/test", "withM.test", "3"),
    ("sqlite/test", "trigger*.test", "4"),
    ("sqlite/test", "altertrig.test", "4"),
]

# Explicit per-file specification keyed by canonical `id` (`"<source_root>/<relative_path>"`).
WAL_TEST_SPECS: dict[str, dict[str, Any]] = {
    # --- Active Phase 1.2 WAL suites (100% subtests passing) ---
    "sqlite/test/wal7.test": {
        "status": "active",
        "required_capabilities": ["wal"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 6,
        "expected_subtest_count": 6,
    },
    "sqlite/test/wal8.test": {
        "status": "active",
        "required_capabilities": ["wal", "vacuum"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 6,
        "expected_subtest_count": 6,
    },
    "sqlite/test/wal9.test": {
        "status": "active",
        "required_capabilities": ["wal"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 8,
        "expected_subtest_count": 8,
    },
    "sqlite/test/walcksum.test": {
        "status": "active",
        "required_capabilities": ["wal", "savepoint"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 94,
        "expected_subtest_count": 94,
    },
    "sqlite/test/waloverwrite.test": {
        "status": "active",
        "required_capabilities": ["wal", "cte", "savepoint"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 22,
        "expected_subtest_count": 22,
    },
    "sqlite/test/walpersist.test": {
        "status": "active",
        "required_capabilities": ["wal"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 20,
        "expected_subtest_count": 20,
    },
    # --- Ignored WAL suites with explicit phase/harness prerequisites ---
    "sqlite/test/wal.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "attach", "tempdb", "autovacuum"],
        "external_dependencies": ["db func", "launch_testfixture"],
        "ignore_reason": "pending Phase 6 Tcl UDF (db func) and multi-process launch_testfixture",
    },
    "sqlite/test/wal2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs SHM/I/O interception harness",
    },
    "sqlite/test/wal3.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs", "db func"],
        "ignore_reason": "requires C testvfs/tvfs harness and Phase 6 Tcl UDF (db func)",
    },
    "sqlite/test/wal4.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test)",
    },
    "sqlite/test/wal5.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "attach"],
        "external_dependencies": ["testvfs", "db busy", "wal_hook"],
        "ignore_reason": "pending Phase 6 Tcl busy/wal_hook callbacks and C testvfs",
    },
    "sqlite/test/wal6.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "autovacuum"],
        "external_dependencies": ["db func"],
        "ignore_reason": "pending Phase 6 Tcl UDF (db func)",
    },
    "sqlite/test/wal64k.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["test_syscall"],
        "ignore_reason": "requires C test_syscall OS page-size override",
    },
    "sqlite/test/walbak.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["sqlite3_backup"],
        "ignore_reason": "pending Phase 6 Online Backup API (sqlite3_backup)",
    },
    "sqlite/test/walbig.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["db func"],
        "ignore_reason": "pending Phase 6 Tcl UDF (db func)",
    },
    "sqlite/test/walblock.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture"],
        "ignore_reason": "requires C testvfs SHM lock script hook and Phase 6 launch_testfixture",
    },
    "sqlite/test/walckptnoop.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["db func"],
        "ignore_reason": "pending Phase 6 Tcl UDF (db func myrandomblob); all 1.0..1.10 invariants covered in phase1_wal_concurrent_c_compat",
    },
    "sqlite/test/walcrash.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "attach", "autovacuum"],
        "external_dependencies": ["sqlite3_crash_enable", "-vfs crash"],
        "ignore_reason": "requires C crash fault-injection VFS (-vfs crash)",
    },
    "sqlite/test/walcrash2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["sqlite3_crash_enable", "-vfs crash", "launch_testfixture"],
        "ignore_reason": "requires C crash fault-injection VFS (-vfs crash) and Phase 6 launch_testfixture",
    },
    "sqlite/test/walcrash3.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs sector-tear fault injection",
    },
    "sqlite/test/walcrash4.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["sqlite3_crash_enable", "-vfs crash"],
        "ignore_reason": "requires C crash fault-injection VFS (-vfs crash / sqlite3_crash_now)",
    },
    "sqlite/test/walfault.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test + testvfs)",
    },
    "sqlite/test/walfault2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test)",
    },
    "sqlite/test/walhook.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "attach", "autovacuum"],
        "external_dependencies": ["wal_hook"],
        "ignore_reason": "pending Phase 6 user wal_hook Tcl callback",
    },
    "sqlite/test/waliter.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "sqlite3_wal_data"],
        "ignore_reason": "requires C internal wal_iterator / testvfs test command",
    },
    "sqlite/test/walmode.test": {
        "status": "active",
        "required_capabilities": ["wal", "attach", "tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 93,
        "expected_subtest_count": 93,
    },
    "sqlite/test/walnoshm.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "unix-excl"],
        "ignore_reason": "requires C testvfs -noshm and unix-excl alternative VFS",
    },
    "sqlite/test/walprotocol.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs SHM lock callback interception",
    },
    "sqlite/test/walprotocol2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs SHM lock callback interception",
    },
    "sqlite/test/walrestart.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["sqlite3_test_control_fault_install"],
        "ignore_reason": "requires C internal sqlite3FaultSim(660) hook (sqlite3_test_control_fault_install)",
    },
    "sqlite/test/walro.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs read-only SHM simulation",
    },
    "sqlite/test/walro2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs read-only SHM simulation",
    },
    "sqlite/test/walrofault.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test)",
    },
    "sqlite/test/walseh1.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["do_faultsim_test", "windows_seh"],
        "ignore_reason": "requires Windows SEH exception simulation and do_faultsim_test",
    },
    "sqlite/test/walsetlk.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture", "SQLITE_ENABLE_SETLK_TIMEOUT"],
        "ignore_reason": "requires SQLITE_ENABLE_SETLK_TIMEOUT, C testvfs, and launch_testfixture",
    },
    "sqlite/test/walsetlk2.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture", "SQLITE_ENABLE_SETLK_TIMEOUT"],
        "ignore_reason": "requires SQLITE_ENABLE_SETLK_TIMEOUT, C testvfs, and launch_testfixture",
    },
    "sqlite/test/walsetlk3.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture", "SQLITE_ENABLE_SETLK_TIMEOUT"],
        "ignore_reason": "requires SQLITE_ENABLE_SETLK_TIMEOUT, C testvfs, and launch_testfixture",
    },
    "sqlite/test/walsetlk_recover.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture", "SQLITE_ENABLE_SETLK_TIMEOUT"],
        "ignore_reason": "requires SQLITE_ENABLE_SETLK_TIMEOUT, C testvfs, and launch_testfixture",
    },
    "sqlite/test/walsetlk_snapshot.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "launch_testfixture", "SQLITE_ENABLE_SETLK_TIMEOUT"],
        "ignore_reason": "requires SQLITE_ENABLE_SETLK_TIMEOUT, C testvfs, and launch_testfixture",
    },
    "sqlite/test/walshared.test": {
        "status": "ignored",
        "required_capabilities": ["wal", "shared_cache"],
        "external_dependencies": ["sqlite3_enable_shared_cache"],
        "ignore_reason": "out of scope: requires shared_cache=1 (sqlite3_enable_shared_cache)",
    },
    "sqlite/test/walslow.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["db func"],
        "ignore_reason": "pending Phase 6 Tcl UDF (db func)",
    },
    "sqlite/test/walthread.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["sqlthread", "wal_hook"],
        "ignore_reason": "requires C sqlthread multi-interpreter harness and Phase 6 wal_hook",
    },
    "sqlite/test/walvfs.test": {
        "status": "ignored",
        "required_capabilities": ["wal"],
        "external_dependencies": ["testvfs", "tvfs"],
        "ignore_reason": "requires C testvfs/tvfs VFS callback instrumentation",
    },
    # --- Active Phase 1.3 Auto-Vacuum & Incremental-Vacuum suites (100% subtests passing) ---
    "sqlite/test/autovacuum.test": {
        "status": "active",
        "required_capabilities": ["autovacuum", "vacuum"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 338,
        "expected_subtest_count": 338,
    },
    "sqlite/test/incrvacuum.test": {
        "status": "active",
        "required_capabilities": ["autovacuum", "incr_vacuum", "vacuum"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 164,
        "expected_subtest_count": 164,
    },
    "sqlite/test/incrvacuum3.test": {
        "status": "active",
        "required_capabilities": ["autovacuum", "incr_vacuum"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 52,
        "expected_subtest_count": 52,
    },
    # --- Ignored Auto-Vacuum & Incremental-Vacuum suites with explicit prerequisites ---
    "sqlite/test/autovacuum2.test": {
        "status": "ignored",
        "required_capabilities": ["autovacuum"],
        "external_dependencies": ["sqlite3_autovacuum_pages"],
        "ignore_reason": "requires Phase 6 sqlite3_autovacuum_pages Tcl callback",
    },
    "sqlite/test/autovacuum_ioerr2.test": {
        "status": "ignored",
        "required_capabilities": ["autovacuum"],
        "external_dependencies": ["do_ioerr_test"],
        "ignore_reason": "requires C do_ioerr_test I/O fault-simulation harness",
    },
    "sqlite/test/incrvacuum2.test": {
        "status": "ignored",
        "required_capabilities": ["autovacuum", "incr_vacuum", "wal", "attach"],
        "external_dependencies": ["sqlite3_test_control_pending_byte_while_open"],
        "ignore_reason": "incrvacuum2.test:277 calls sqlite3_test_control_pending_byte 0x1000000 before reset_db while db is open; Phase 1.1 LockGeometry forbids changing pending-byte while handles are open (all 30 subtests in 1.1..5.12 verified in phase1_autovacuum_ptrmap)",
        "observed_passed_subtests": 26,
    },
    "sqlite/test/incrvacuum_ioerr.test": {
        "status": "ignored",
        "required_capabilities": ["autovacuum", "incr_vacuum"],
        "external_dependencies": ["do_ioerr_test"],
        "ignore_reason": "requires C do_ioerr_test I/O fault-simulation harness",
    },
    # --- Active Phase 2 Multi-Database (`ATTACH` / `DETACH` / `TEMP`) suites (100% subtests passing) ---
    "sqlite/test/attach.test": {
        "status": "active",
        "required_capabilities": ["attach", "tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 79,
        "expected_subtest_count": 79,
    },
    "sqlite/test/attach2.test": {
        "status": "active",
        "required_capabilities": ["attach", "tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 63,
        "expected_subtest_count": 63,
    },
    "sqlite/test/attach3.test": {
        "status": "active",
        "required_capabilities": ["attach", "tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 32,
        "expected_subtest_count": 32,
    },
    "sqlite/test/tempdb.test": {
        "status": "active",
        "required_capabilities": ["tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 5,
        "expected_subtest_count": 5,
    },
    # --- Ignored Phase 2 suites with explicit Phase 6 or C fault-sim prerequisites ---
    "sqlite/test/attach4.test": {
        "status": "active",
        "required_capabilities": ["attach", "tempdb", "wal", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 12,
        "expected_subtest_count": 12,
    },
    "sqlite/test/attachmalloc.test": {
        "status": "ignored",
        "required_capabilities": ["attach"],
        "external_dependencies": ["do_malloc_test"],
        "ignore_reason": "requires C OOM fault-simulation harness (do_malloc_test)",
        "observed_passed_subtests": 0,
        "expected_subtest_count": 0,
    },
    "sqlite/test/tempdb2.test": {
        "status": "ignored",
        "required_capabilities": ["tempdb"],
        "external_dependencies": ["db func"],
        "ignore_reason": "pending Phase 6 Tcl user-defined SQL function callback (db func int2str)",
        "observed_passed_subtests": 0,
        "expected_subtest_count": 7,
    },
    # --- Active Phase 3 VIEW / UPSERT / UPDATE FROM / CTE suites (100% subtests passing) ---
    "sqlite/test/view2.test": {
        "status": "active",
        "required_capabilities": ["view", "cte"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 4,
        "expected_subtest_count": 4,
    },
    "sqlite/test/view3.test": {
        "status": "active",
        "required_capabilities": ["view", "compound"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 1,
        "expected_subtest_count": 1,
    },
    "sqlite/test/upsert3.test": {
        "status": "active",
        "required_capabilities": ["upsert"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 7,
        "expected_subtest_count": 7,
    },
    "sqlite/test/upsert5.test": {
        "status": "active",
        "required_capabilities": ["upsert"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 249,
        "expected_subtest_count": 249,
    },
    "sqlite/test/upfrom1.test": {
        "status": "active",
        "required_capabilities": ["cte", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 22,
        "expected_subtest_count": 22,
    },
    "sqlite/test/upfrom3.test": {
        "status": "active",
        "required_capabilities": ["cte", "upsert"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 42,
        "expected_subtest_count": 42,
    },
    "sqlite/test/withM.test": {
        "status": "active",
        "required_capabilities": ["cte"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 1,
        "expected_subtest_count": 1,
    },
    # --- Ignored / Promoted Phase 3 suites ---
    "sqlite/test/view.test": {
        "status": "ignored",
        "required_capabilities": ["view", "analyze", "auth"],
        "external_dependencies": ["ANALYZE", "db authorizer"],
        "ignore_reason": "view-25.1..25.2 execute unguarded ANALYZE + db authorizer (pending Phase 5 analyze and Phase 6 auth); all 119 non-auth subtests pass and are verified in phase3_view_upsert_dropcol_cte",
        "observed_passed_subtests": 120,
        "expected_subtest_count": 121,
    },
    "sqlite/test/upsert1.test": {
        "status": "active",
        "required_capabilities": ["upsert", "view", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 35,
        "expected_subtest_count": 35,
    },
    "sqlite/test/upsert2.test": {
        "status": "active",
        "required_capabilities": ["upsert", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 14,
        "expected_subtest_count": 14,
    },
    "sqlite/test/upsert4.test": {
        "status": "active",
        "required_capabilities": ["upsert", "view", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 128,
        "expected_subtest_count": 128,
    },
    "sqlite/test/upsertfault.test": {
        "status": "ignored",
        "required_capabilities": ["upsert"],
        "external_dependencies": ["do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test)",
        "observed_passed_subtests": 0,
        "expected_subtest_count": 0,
    },
    "sqlite/test/upfrom2.test": {
        "status": "ignored",
        "required_capabilities": ["view", "trigger", "windowfunc"],
        "external_dependencies": ["OVER"],
        "ignore_reason": "upfrom2-7.0..7.1 execute unguarded window function COUNT(*) OVER () (pending Phase 5 windowfunc); 1.1..6.2 (44 subtests) pass and are verified in phase4_trigger_gate",
        "observed_passed_subtests": 44,
        "expected_subtest_count": 46,
    },
    "sqlite/test/upfrom4.test": {
        "status": "active",
        "required_capabilities": ["cte", "view", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 8,
        "expected_subtest_count": 8,
    },
    "sqlite/test/upfromfault.test": {
        "status": "ignored",
        "required_capabilities": ["view", "trigger"],
        "external_dependencies": ["do_faultsim_test"],
        "ignore_reason": "requires C OOM/I/O fault-simulation harness (do_faultsim_test)",
        "observed_passed_subtests": 0,
        "expected_subtest_count": 0,
    },
    "sqlite/test/alterdropcol.test": {
        "status": "active",
        "required_capabilities": ["altertable", "view", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 100,
        "expected_subtest_count": 100,
    },
    "sqlite/test/alterdropcol2.test": {
        "status": "active",
        "required_capabilities": ["altertable", "view", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 23,
        "expected_subtest_count": 23,
    },
    # --- Active Phase 4 TRIGGER (`BEFORE` / `AFTER` / `INSTEAD OF` / `RAISE` / `ALTER`) suites ---
    "sqlite/test/altertrig.test": {
        "status": "active",
        "required_capabilities": ["altertable", "trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 36,
        "expected_subtest_count": 36,
    },
    "sqlite/test/trigger2.test": {
        "status": "active",
        "required_capabilities": ["trigger", "tempdb", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 111,
        "expected_subtest_count": 111,
    },
    "sqlite/test/trigger3.test": {
        "status": "active",
        "required_capabilities": ["trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 24,
        "expected_subtest_count": 24,
    },
    "sqlite/test/trigger4.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 24,
        "expected_subtest_count": 24,
    },
    "sqlite/test/trigger5.test": {
        "status": "active",
        "required_capabilities": ["trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 2,
        "expected_subtest_count": 2,
    },
    "sqlite/test/trigger7.test": {
        "status": "active",
        "required_capabilities": ["trigger", "tempdb", "explain"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 10,
        "expected_subtest_count": 10,
    },
    "sqlite/test/trigger8.test": {
        "status": "active",
        "required_capabilities": ["trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 1,
        "expected_subtest_count": 1,
    },
    "sqlite/test/trigger9.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 28,
        "expected_subtest_count": 28,
    },
    "sqlite/test/triggerA.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 17,
        "expected_subtest_count": 17,
    },
    "sqlite/test/triggerB.test": {
        "status": "active",
        "required_capabilities": ["trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 202,
        "expected_subtest_count": 202,
    },
    "sqlite/test/triggerC.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view", "foreignkey"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 121,
        "expected_subtest_count": 121,
    },
    "sqlite/test/triggerD.test": {
        "status": "active",
        "required_capabilities": ["trigger", "attach", "tempdb"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 12,
        "expected_subtest_count": 12,
    },
    "sqlite/test/triggerF.test": {
        "status": "active",
        "required_capabilities": ["trigger"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 12,
        "expected_subtest_count": 12,
    },
    "sqlite/test/triggerG.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 8,
        "expected_subtest_count": 8,
    },
    "sqlite/test/triggerupfrom.test": {
        "status": "active",
        "required_capabilities": ["trigger", "view"],
        "external_dependencies": [],
        "ignore_reason": None,
        "observed_passed_subtests": 14,
        "expected_subtest_count": 14,
    },
    # --- Ignored Phase 4 suites with explicit Phase 5/6 prerequisites ---
    "sqlite/test/trigger1.test": {
        "status": "ignored",
        "required_capabilities": ["trigger", "tempdb", "view", "windowfunc"],
        "external_dependencies": ["OVER"],
        "ignore_reason": "trigger1-22.10 executes unguarded sum(b) OVER (...) window function (pending Phase 5 windowfunc); all 92 non-window subtests pass and are verified in phase4_trigger_gate",
        "observed_passed_subtests": 92,
        "expected_subtest_count": 93,
    },
    "sqlite/test/trigger6.test": {
        "status": "ignored",
        "required_capabilities": ["trigger"],
        "external_dependencies": ["db function"],
        "ignore_reason": "pending Phase 6 Tcl user-defined SQL function callback (db function counter)",
        "observed_passed_subtests": 0,
        "expected_subtest_count": 6,
    },
    "sqlite/test/triggerE.test": {
        "status": "ignored",
        "required_capabilities": ["trigger", "tempdb", "windowfunc"],
        "external_dependencies": ["OVER"],
        "ignore_reason": "triggerE-1.1.11 and 1.2.11 execute unguarded max(b) OVER (...) window function (pending Phase 5 windowfunc); all 24 non-window subtests pass and are verified in phase4_trigger_gate",
        "observed_passed_subtests": 24,
        "expected_subtest_count": 26,
    },
}


def sanitize_ident(raw: str) -> str:
    return re.sub(r"[^A-Za-z0-9_]", "_", raw)


def module_decl_ident(mod_name: str) -> str:
    if mod_name in RUST_KEYWORDS:
        return f"r#{mod_name}"
    return mod_name


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    h.update(path.read_bytes())
    return h.hexdigest()


def count_static_subtest_calls(tcl_text: str) -> int:
    pattern = re.compile(
        r"^\s*(?:do_test|do_execsql_test|do_catchsql_test|do_eqp_test|do_filepath_test)\b",
        re.MULTILINE,
    )
    return len(pattern.findall(tcl_text))


def build_manifest_entries() -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    seen_wrappers: set[str] = set()

    for source_root, glob_pat, default_phase in SCAN_RULES:
        root_dir = REPO_ROOT / source_root
        if not root_dir.is_dir():
            raise RuntimeError(f"Missing C SQLite test root directory: {root_dir}")
        matched = sorted(root_dir.glob(glob_pat))
        if not matched:
            raise RuntimeError(f"Pattern {glob_pat!r} matched 0 files in {root_dir}")

        prefix = ROOT_PREFIX.get(source_root)
        if prefix is None:
            raise RuntimeError(f"Unconfigured source_root {source_root!r} in ROOT_PREFIX")

        for test_file in matched:
            rel_path = test_file.relative_to(root_dir).as_posix()
            canonical_id = f"{source_root}/{rel_path}"
            if canonical_id not in WAL_TEST_SPECS:
                raise RuntimeError(
                    f"Unclassified C SQLite test file {canonical_id!r}; add an explicit spec in tools/generate_sqlite_test_wrappers.py"
                )
            spec = WAL_TEST_SPECS[canonical_id]
            stem = test_file.stem
            base_name = f"{prefix}{stem}"
            mod_name = sanitize_ident(base_name)
            wrapper_rel = f"tests/sqlite_test/{base_name}.rs"
            if wrapper_rel in seen_wrappers:
                raise RuntimeError(f"Wrapper path collision on {wrapper_rel!r}")
            seen_wrappers.add(wrapper_rel)

            status = spec["status"]
            ignore_reason = spec.get("ignore_reason")
            if status == "ignored" and not ignore_reason:
                raise RuntimeError(f"Ignored test {canonical_id!r} must specify non-empty ignore_reason")
            if status == "active" and ignore_reason is not None:
                raise RuntimeError(f"Active test {canonical_id!r} must have ignore_reason=None")

            tcl_text = test_file.read_text(encoding="utf-8", errors="replace")
            static_calls = count_static_subtest_calls(tcl_text)

            entries.append(
                {
                    "id": canonical_id,
                    "source_root": source_root,
                    "relative_path": rel_path,
                    "source_path": canonical_id,
                    "sha256": sha256_file(test_file),
                    "wrapper_path": wrapper_rel,
                    "module_name": mod_name,
                    "test_fn": f"test_{mod_name}",
                    "phase": spec.get("phase", default_phase),
                    "required_capabilities": spec["required_capabilities"],
                    "external_dependencies": spec["external_dependencies"],
                    "status": status,
                    "ignore_reason": ignore_reason,
                    "observed_passed_subtests": spec.get("observed_passed_subtests", 0),
                    "expected_subtest_count": spec.get("expected_subtest_count", static_calls),
                }
            )

    entries.sort(key=lambda e: e["id"])
    return entries


def render_manifest_json(entries: list[dict[str, Any]]) -> str:
    active_count = sum(1 for e in entries if e["status"] == "active")
    ignored_count = sum(1 for e in entries if e["status"] == "ignored")
    if active_count == 0:
        raise RuntimeError("Refusing to generate manifest with 0 active tests")
    doc = {
        "schema_version": 1,
        "generator": "tools/generate_sqlite_test_wrappers.py",
        "supported_roots": list(ROOT_PREFIX.keys()),
        "summary": {
            "total_registered": len(entries),
            "active": active_count,
            "ignored": ignored_count,
        },
        "tests": entries,
    }
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


def format_rust_source(src: str) -> str:
    proc = subprocess.run(
        ["rustfmt", "--edition", "2024"],
        input=src,
        text=True,
        capture_output=True,
        check=True,
    )
    return proc.stdout


def render_wrapper_rs(entry: dict[str, Any]) -> str:
    source_root = entry["source_root"]
    rel_path = entry["relative_path"]
    stem = Path(rel_path).stem
    test_fn = entry["test_fn"]
    include_rel = f"../../{source_root}/{rel_path}"

    lines = [
        "// Generated by `tools/generate_sqlite_test_wrappers.py` — do not edit by hand.",
        "#![forbid(unsafe_code)]",
        "",
        "#[test]",
    ]
    if entry["status"] == "ignored":
        escaped_reason = entry["ignore_reason"].replace("\\", "\\\\").replace('"', '\\"')
        lines.append(f'#[ignore = "{escaped_reason}"]')
    lines.extend(
        [
            f"fn {test_fn}() {{",
            f'    let raw = include_str!("{include_rel}");',
            "    let script = sqliters_tcltest::strip_test_comments(raw);",
            f'    sqliters_tcltest::run_test_script_in_root("{source_root}", "{stem}", &script);',
            "}",
            "",
        ]
    )
    return format_rust_source("\n".join(lines))


def render_suite_rs() -> str:
    wrapper_files = sorted(p.name for p in WRAPPER_DIR.glob("*.rs"))
    lines = ["#![forbid(unsafe_code)]", ""]
    for fname in wrapper_files:
        stem = fname[:-3]
        mod_ident = module_decl_ident(sanitize_ident(stem))
        lines.append("#[allow(non_snake_case)]")
        lines.append(f'#[path = "sqlite_test/{fname}"]')
        lines.append(f"mod {mod_ident};")
    lines.append("")
    return format_rust_source("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify manifest, generated wrappers, and tests/sqlite_test_suite.rs without modifying files.",
    )
    args = parser.parse_args()

    entries = build_manifest_entries()
    expected_manifest = render_manifest_json(entries)

    mismatches: list[str] = []

    if args.check:
        if not MANIFEST_PATH.is_file():
            mismatches.append(f"missing manifest: {MANIFEST_PATH}")
        elif MANIFEST_PATH.read_text(encoding="utf-8") != expected_manifest:
            mismatches.append(f"out-of-sync manifest: {MANIFEST_PATH}")

        for entry in entries:
            wpath = REPO_ROOT / entry["wrapper_path"]
            expected_wrapper = render_wrapper_rs(entry)
            if not wpath.is_file():
                mismatches.append(f"missing wrapper: {wpath}")
            elif wpath.read_text(encoding="utf-8") != expected_wrapper:
                mismatches.append(f"out-of-sync wrapper: {wpath}")

        expected_suite = render_suite_rs()
        if not SUITE_RS_PATH.is_file():
            mismatches.append(f"missing suite file: {SUITE_RS_PATH}")
        elif SUITE_RS_PATH.read_text(encoding="utf-8") != expected_suite:
            mismatches.append(f"out-of-sync suite file: {SUITE_RS_PATH}")

        if mismatches:
            for m in mismatches:
                print(f"ERROR: {m}", file=sys.stderr)
            return 1

        active = sum(1 for e in entries if e["status"] == "active")
        ignored = sum(1 for e in entries if e["status"] == "ignored")
        print(
            f"OK: {len(entries)} manifest entries ({active} active, {ignored} ignored) and wrappers are 100% in sync."
        )
        return 0

    WRAPPER_DIR.mkdir(parents=True, exist_ok=True)
    for entry in entries:
        wpath = REPO_ROOT / entry["wrapper_path"]
        wpath.write_text(render_wrapper_rs(entry), encoding="utf-8")

    MANIFEST_PATH.write_text(expected_manifest, encoding="utf-8")
    SUITE_RS_PATH.write_text(render_suite_rs(), encoding="utf-8")

    active = sum(1 for e in entries if e["status"] == "active")
    ignored = sum(1 for e in entries if e["status"] == "ignored")
    print(
        f"Generated {len(entries)} wrappers ({active} active, {ignored} ignored), {MANIFEST_PATH.relative_to(REPO_ROOT)}, and {SUITE_RS_PATH.relative_to(REPO_ROOT)}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
