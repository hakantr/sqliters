# Differences from C in step M3c (transaction opcodes, virtual-table reads, `max_page_count`)

C = `$S/bin/sqlite3` / the C library 3.54.1 (API driver `m3c/drv/c/rdrv4`, fault VFS `m3c/fault/fault.c`); engine =
sqliters with the switch off; machine = `SQLITERS_VDBE_EXEC=1` (since M3c also BEGIN / COMMIT / END / ROLLBACK / SAVEPOINT /
RELEASE / ROLLBACK TO and every SELECT that reads a virtual table run C's program; a SELECT that C cannot compile fails
with C's compile error). Scratch copy with the tools: `target/perf-work/m3c/`. Numbering continues D1–D51, E1–E23, F1–F7.

## Closed in this step

| Item | Now | Evidence |
|---|---|---|
| **E22** transaction statements on the engine | `OP_AutoCommit`, `OP_Savepoint` ported, the halt of a transaction statement (`sqlite3VdbeHalt` commit / rollback part, BUSY retry with `p->pc`), `OP_Transaction` BUSY (`goto vdbe_return`), QueryOnly / CorruptRdOnly check, TEMP without b-tree skipped, EXCLUSIVE; routed through C's compile result | `ops_tx` API 133/400 (switch on before M3c) → **381/400**, fallbacks 394 → 0; `ops_ddl` 4/4; CLI 380/400 (the 20 left are D52–D57/E24–E27, identical with the switch off); test module `c_align_m3c_tests` (5 tests, C CLI / rdrv4 expectations) |
| **E21** virtual tables in read programs | a SELECT with `OP_VOpen` runs on the machine through the engine's statement context (`ExecContext`); `OP_VInitIn`, `OP_VCheck` ported (`OP_VRename`: only ALTER TABLE, DDL stays on the engine); xFindFunction overloads (FTS5 auxiliary functions) dispatched as `fts5ApiCallback`; subtypes of xColumn results and of xFilter arguments; positioned r-tree cursors lock writes as in C; a SELECT C cannot compile fails with C's error | data output vs C (switch on; M3a binary → M3c): vt_x machine 0 / fallback 2 519 identical + 187 different → machine 2 590 identical + 66 different, fallback 0; vt_e 54/1 → 55/0; vt_d (without its unbounded `generate_series WHERE start=1`, 4 294 967 295 rows) fallback 151/10 → machine 159/2; vt_t (per statement, 10 s budget, 83 statements over the budget in C as well) fallback 1 654/375 → machine 1 984/45; vt_b fallback 4 204/858 → machine 4 964/98; vt_a fallback 8 356/1 796 → machine 9 978/179. Every remaining machine difference is D57 (engine pragma functions). Fallbacks for vtab reads: sc6 / sc7 10 + 10 → 0 |
| **D35** xBestIndex / xFilter contract | `VirtualCursor::filter_c`: the machine hands C's idxNum / idxStr / argv to every module; generate_series (`seriesFilter` ported), dbstat (`statFilter`: schema / name / aggregate / ORDER BY name / b-tree order), pragma_* (`pragmaVtabFilter` argument slots); json_each / json_tree, fts5, fts5vocab, rtree / geopoly already had C's plan | vt corpora above; 22 probe statements (LIMIT / OFFSET, value ranges, NULL arguments, unknown schema) equal |
| **D37** hidden-column constraints of table-valued functions | **closed on the switch-on path**: `SELECT value FROM generate_series(0,3,1) WHERE stop=1` → no row (C), also `start=1`, `pragma_index_list(..) WHERE arg='t1'` | test `m3c_virtual_table_reads_take_c_plans`; rowdiff vt_x: 59 statements different with the M3a binary are identical now |
| **E23** `max_page_count` after the insert | the pager refuses a page past `mxPgno` when it is allocated (`getPageNormal`, pager.c:5653) for every write of a machine statement (`Pager::set_mx_pgno`, set by `run_write`); the post-insert check is gone | 200 scripts (`m3c/mpc`, inserts, overflow rows, updates that grow rows, deletes, savepoints, auto_vacuum, RETURNING): stdout and file 200/200 with the new and with the old check — no visible difference was found, the check is now at C's place |
| **E11** full rollback plays no journal back | stays (pager); not visible with ROLLBACK on the machine either | ops_tx: 400 scripts × (ROLLBACK, ROLLBACK TO, failing COMMIT, BUSY) — every file-byte difference is explained by E24/E25 (switch off identical); crash suites 0 failures; `PRAGMA` outputs in the corpus (schema_version, integrity_check, page_count) equal; the CLI has no page-cache statistics output the engine prints (`.stats` differs as a whole), so the cache content is not observable |
| new, fixed **D52** `changes()` of a RETURNING statement | `sqlite3VdbeHalt` stores `nChange` when the statement halts; the machine stored it before the first row | ops_tx: 247 scripts differed only by this (pending `INSERT … RETURNING` slots); test `m3c_returning_changes_counted_at_halt` |
| new, fixed **D53** OR FAIL error + failed autocommit commit | C: the code is the commit's (SQLITE_BUSY), the message stays the statement's (`p->zErrMsg`); engine reported "database is locked" (both paths) | ops_tx tx_00105 / 00179 / 00200 / 00206 / 00237 / 00315 (rdrv4: `err 5 UNIQUE constraint failed: t.d`) |
| new, fixed **D54** CLI: error of a multi-statement line | the shell re-prepared every statement of a failed batch and reported a later statement's prepare error (`BEGIN; INSERT INTO nosuch …` inside a transaction: C "cannot start a transaction within a transaction"); only a prepare error that is the batch's error counts now (both paths) | CLI ops_tx tx_00192 / 00315 / 00342 |
| new, fixed: CorruptRdOnly after a machine read | "if( rc==SQLITE_CORRUPT && db->autoCommit==0 ) db->flags \|= SQLITE_CorruptRdOnly" also for a read on the machine | test `c_align_h5_tests::corruption_inside_a_transaction_makes_the_connection_refuse_writes` (failed with the switch on once fts5 reads moved to the machine) |

## New differences (D55+, E24+, F8)

- **D55 — engine: `PRAGMA query_only` is not implemented.** `PRAGMA query_only=1; BEGIN IMMEDIATE;` C "attempt to write a
  readonly database", engine nothing (and DML / DDL write). The machine checks the flag in `OP_Transaction` (vdbe.c:4268) but
  the engine has no setter (`VdbeHost::query_only` answers false).
- **D56 — engine DDL: a no-op `CREATE INDEX IF NOT EXISTS` / `DROP TABLE IF EXISTS` takes a write transaction.** C codes
  `Transaction 0 0` (read) + `JournalMode` (`sqlite3ForceNotReadOnly`); with a reader on another connection the engine's
  autocommit statement fails with "database is locked" (ops_tx tx_00380; switch off the same).
- **D57 — engine pragma functions now visible through machine reads** (all exist with the switch off as fallback
  differences): `pragma_collation_list` / `pragma_function_list` / `pragma_module_list` contents (shell collations
  `decimal` / `uint`, shell functions, module registration), `pragma_index_list` order, `pragma_index_info` of a WITHOUT
  ROWID table, the type of a view column over a virtual table in `pragma_table_info`, "unknown database 'x'" quoting
  (vt_x 66 machine differences).
- **E24 — WAL: the change counter of page 1 after savepoint rollbacks.** ops_tx tx_00132 (WAL, cache_size=2): bytes 28 / 96
  of the checkpointed file 2 (C) vs 3; switch off identical.
- **E25 — cache_size=2: free-space bytes of pages written by `CREATE VIRTUAL TABLE … fts5` with auto_vacuum.** 16 ops_tx
  scripts (CREATE through the engine's DDL; switch off identical).
- **E26 — engine DDL with fts5: `DROP TABLE` counts xSync statements in `total_changes()`** (ops_tx tx_00059: 57 vs 53).
- **E27 — cache spill timing with fts5 writes** (ops_tx tx_00383: C holds an EXCLUSIVE lock after a spill, a reader on a
  second connection gets SQLITE_BUSY; engine pager spills later). Switch off identical.
- **E28 — COMMIT that fails with an I/O error.** C (`m3c/fault/fault.c`, third main-db write fails): COMMIT `disk I/O
  error`, autocommit 1, the following ROLLBACK "cannot rollback - no transaction is active"; RELEASE of the transaction
  savepoint: autocommit 0. The engine's pager has no error state (C leaves the journal hot until the next access): the
  machine keeps the engine's behaviour (transaction open after a failed COMMIT) so that the hot journal is not replayed
  at once (test `bulgu_y2`).
- **F8 — tests that assert non-C behaviour and fail only with the switch on** (not edited; the switch-off engine gives the
  asserted result, as F4 / F6 / F7):
  - `tests/phase2_ast_runtime_subtype.rs` `group64_quoted_dots_three_part_refs_in_table_and_catalog_sql_c_parity`
    (line 904) and `tests/phase2_multidb_gate.rs` `test_gate2_3_name_resolution_vdbe_vs_interpreter_c_parity_and_autovacuum`
    (line 1258): `SELECT main."a.b".* FROM main."a.b"`, `SELECT aux.items.* FROM aux.items …` — C `near "*": syntax error`
    (`$S/bin/sqlite3`), the tests expect rows.
  - `tests/phase7_json_fts5_rtree.rs` `test_wal_jsonb_window_interoperability_vacuum_savepoint_and_trusted_schema` (line
    1108): a view with `ft_sp MATCH …` under `PRAGMA trusted_schema = OFF` — C `unsafe use of MATCH()`, the test expects rows.

## D58 — SELECT whose engine parse fails never reaches C's compile result (found while verifying M3c)

`SELECT main."a.b".* FROM (SELECT 1);`, `SELECT aux.items.* FROM (SELECT 1);`, `SELECT x.y.* FROM (SELECT 1) AS y;`:
C reports `near "*": syntax error` (with the caret line). With `SQLITERS_VDBE_EXEC=1` the CLI still prints the engine's
`no such table: main.a.b` (no caret), because the engine's own parse/prepare rejects the statement before the route
asks the generator. The generator itself gives C's error (`EXPLAIN SELECT main."a.b".* …` prints C's text). This is
the reason `group64_quoted_dots…` and `test_gate2_3…` still differ from C on the switch-on path. Fix: on the switch-on
path, let C's compile result decide for SELECT exactly as M3a does for DML (an engine prepare error must not win).
