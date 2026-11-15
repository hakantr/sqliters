# Differences from C in step M3 (machine correctness and the DML route)

C = `$S/bin/sqlite3` / the C library 3.54.1 (drivers `m2/drv/c/rdrv3`); engine = sqliters with the switch off; machine =
`SQLITERS_VDBE_EXEC=1` (since M3 INSERT / UPDATE / DELETE go through C's compile result: C's program on the machine or C's
compile error). Scratch copy with the tools: `target/perf-work/m3/`. Numbering continues D1–D48, E1–E19, F1–F6.

## Closed in this step

| Item | Now | Evidence |
|---|---|---|
| **E16** OP_IdxDelete over an expression index (779) | `sqlite3VdbeFindIndexKey` ported | `m3/fik`: an expression index and a VIRTUAL-column index whose stored keys differ from the recomputed ones (made by C with writable_schema): stdout and file bytes equal to C (pre-M3 machine with C's own program: `779 database disk image is malformed` on every DELETE; engine: file differs); ops_dml_a/b00214 stdout equal; C coverage `sqlite3VdbeFindIndexKey` 27/30 lines; test `m3_idxdelete_searches_an_expression_index` |
| **E17** UNIQUE conflict not detected (WITHOUT ROWID `a UNIQUE, PRIMARY KEY(a DESC)`) | root cause was the engine, not the machine: same cause as **D30** | build.c 4396-4458: equivalent UNIQUE / PRIMARY KEY constraints are merged *keeping the first index and its sort order*; the engine replaced the columns (and sort order) with the PRIMARY KEY's and processed a column's PRIMARY KEY before a UNIQUE written before it. The rows the engine's INSERTs stored were in DESC order while C's program (KeyInfo ASC) searched ASC. Fixed in `schema.rs`; the E17 script and the D30 repro give C's output and C's file bytes, switch on and off (test `m3_unique_before_primary_key_desc_keeps_the_unique_index_order`) |
| **D30** WITHOUT ROWID `UNIQUE PRIMARY KEY DESC` row order | fixed (above) | D30 repro file bytes equal to C, `PRAGMA integrity_check` ok |
| **E19** DML route | INSERT / UPDATE / DELETE (with WITH prefix) through C's compile result in the CLI and the API | corpora below |
| **D43 / D44** engine prepare rejects / accepts / words DML differently | closed on the switch-on path: C's error (message, offset) decides before the engine's prepare; an engine rejection of a statement C compiles is ignored | ops_dml_* through the CLI: scripts with a non-random difference 167/112/102/86/84 (switch off) → 1/2/0/2/2 (all E20); test `m3_routed_dml_reports_c_compile_results` |
| **D45** STRICT type-check order of INSERT | closed on the switch-on path (C's program) | no remaining difference of this class in the corpora |
| **D46** rtree RETURNING value | closed on the switch-on path (OP_VUpdate) | ops_dml_v/b00012 stdout equal |
| **D48** mutually dependent generated columns | closed on the switch-on path: `generated column loop on "e"` at prepare, no crash | rich corpora through the CLI: switch off 7 crashes (stack overflow) in 630 blocks, switch on 0; test `m3_routed_dml_reports_c_compile_results` |
| **E18** virtual-table writes | OP_VBegin, OP_VUpdate, and for fts5 / rtree tables OP_VOpen / OP_VFilter / OP_VColumn / OP_VNext / OP_Rowid in write programs | ops_dml_v API 57/150 (V5 machine) → 138/150; fallbacks 643 → 0 |
| **D47** page bytes (DML corpora) | on the switch-on path every remaining file difference is E20 (below) | `m3/dbbis.sh` on each: the first differing statement writes a value through `lower()` / `upper()` of invalid UTF-8 |
| **E15** (xfer, update hooks, vtab writes not run by the machine) | xfer (`OP_RowCell`, `OPFLAG_PREFORMAT`), update hook, vtab writes now on the machine; BEGIN / COMMIT / SAVEPOINT and DDL still on the engine (E22) | `m3/xfer/run.sh` 9/9 file bytes equal; `m3/hook/hook.c` = test `m3_update_hook_events_of_the_machine` |

Also fixed on the way (engine, found by the switch-on test run): the generator did not see `SQLITE_DBCONFIG_DEFENSIVE`,
`ENABLE_TRIGGER`, `ENABLE_VIEW` or `set_column_limit` / trigger and expression-depth limits of the connection (`cgen.rs`
`make_db`); after `ALTER TABLE ft RENAME TO ft2` of an fts5 table the generator still saw the hidden column `ft`
(`vtab.rs` `rename_connected`, `INSERT INTO ft2(ft2) VALUES('optimize')` failed with "table ft2 has no column named ft2").

## New differences (D49+, E20+, F7+)

- **D49 — engine: text-to-number prefix conversion.** `SELECT '4.24.2'+'4.24.2'` C `8.48`, engine `8` (the machine
  `8.48`). Visible through the engine's `PRAGMA integrity_check` (it stays on the engine): on a file whose expression index
  `t1(c+c)` (c = `'4.24.2'`) C's program wrote, the engine reports `row 1 missing from index t1i1` where C reports `ok`
  (ops_dml_v/b00023 through the CLI; before M3 the engine wrote its own value into the index and the file differed from C's).
- **D50 — engine: `sqlite3_bind_*` does not enforce SQLITE_MAX_LENGTH.** C (`m3/bigtok/big.c` against the amalgamation):
  binding a 1 080 000 000-byte string fails with SQLITE_TOOBIG, the parameter stays NULL and the INSERT succeeds. The engine
  binds the string; with the switch on the machine then stops the statement with "string or blob too big" (Tcl
  `fts5bigtok2-1.1`, expected `0 {}`). Switch off: the engine inserts the whole string (also not C).
- **D51 — engine DDL: an error C reports at prepare is reported at step.** `CREATE TABLE ... PRIMARY KEY` on a generated
  column: C `Parse error near line N: generated columns cannot be part of the PRIMARY KEY`, the engine `Error near line N:`
  (CLI prefix; m2_dml_b dml_00317 / dml_00321). DDL stays on the engine.
- **E20 — values crossing into the engine's function library are Rust `String`s.** Text that is not valid UTF-8 (a BLOB
  cast or concatenated as text) is replaced by U+FFFD when a function of the engine's library (`lower`, `upper`, `hex`, …)
  receives or returns it, and in API `Value::Text`. With the switch on this is the only class of remaining file differences
  in the DML corpora (an index on `lower(b)`: 8 ops_dml + 4 rich scripts) and of the remaining stdout differences that are
  not `random()` (ops_dml_a/b00056, rich v block 79). Known in the report since M1 (`CAST(x'41ABCD42' AS TEXT)`), outside M3.
- **E21 — virtual tables in read programs and other modules.** A SELECT whose program opens a virtual table (OP_VOpen) still
  runs on the engine (the read host has no `ExecContext`); a write program that scans a table of another module than fts5 /
  rtree (generate_series, json_each, dbstat, pragma_*, fts5vocab: D35) stays on the engine. So **D37 is not confirmed
  closed** (it needs the read side). OP_VInitIn, OP_VCheck, OP_VRename are not ported. OP_VColumn with OPFLAG_NOCHANGE follows
  C's fts5 / geopoly / rtree xColumn rules by module (`vtab_column_skips_nochange`), not by a call of the module.
- **E22 — transaction statements and DDL stay on the engine.** `OP_AutoCommit`, `OP_Savepoint`, `OP_SetCookie`, `OP_Expire`,
  `OP_CreateBtree`, `OP_Destroy`, `OP_ParseSchema`, `OP_Drop*` are not ported; BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE
  are not routed. E11 (pager full rollback plays no journal back) is unchanged; it has no effect on results or file bytes in
  any corpus or crash suite (crash suites: 0 failures).
- **E23 — `max_page_count` is checked after the insert, not at the page fetch.** C fails inside `getPageNormal` when a page
  past `mxPgno` is fetched; the machine checks the page count after each OP_Insert / OP_IdxInsert. Same result in
  `c_align_h1b_tests::full_error_…` (SQLITE_FULL, statement rolled back); an insert that allocates past the limit and frees
  pages again inside the same opcode would differ (not seen).
- **F7 — tests that assert non-C behaviour and fail only with the switch on** (not edited; the switch-off engine gives the
  asserted text, as F4 / F6):
  - `tests/phase2_ast_runtime_subtype.rs` `group64_dml_alias_and_returning_wildcard_and_qualifier_c_parity` (line 1014):
    `DELETE FROM main.rt RETURNING main.rt.*` — C: `near "*": syntax error` (`$S/bin/sqlite3`), the test asserts
    `RETURNING may not use "TABLE.*" wildcards`.
  - `tests/phase3_view_upsert_dropcol_cte.rs` `phase3_end_to_end_differential_…` (line 621): `INSERT INTO main.v_items VALUES(…)
    ON CONFLICT DO NOTHING` — C: `cannot modify v_items because it is a view`, the test asserts `cannot UPSERT a view` (D44).
  - `tests/sqlite_test/…` `fts5bigtok2-1.1` (D50).
  - `crates/sqliters-core/src/lib.rs` `tests::point_update_delete_plans_match_vdbe_programs`: inspects the engine's compiled
    program of an UPDATE, which the route drops (engine internals, not C behaviour).
