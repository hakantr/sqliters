# Differences from C noticed in step V6 (schema statements in the code generator)

D59–D81 (engine; D75–D80 fixed here) and E30–E31 (machine) are new in step V6. **None is a difference of a generated program**: every
statement of the V6 corpora that the generator accepts has an EXPLAIN / EXPLAIN QUERY PLAN listing identical to C's
(28 367 statements, each as EXPLAIN and as EQP: 56 734 listings, 0 different) and, where the op scripts reach it, a
program identical to the one C prepares (25 107 programs, 0 different; 13 statements the generator reported unsupported). Reference: `$S/bin/sqlite3` (3.54.1) and the C driver `$S/v6/drv/c/rdrv6`. Numbering continues
D1–D58, E1–E29, F1–F9 (gorulen_farklar_m3c.md). Each item: what C does, what the engine does, where it was seen
(`$S/v6/res/xf_<corpus>_r1.log` lists every statement whose execution differs; `res/all_kinds_cls_r1.txt` the
all-kinds classes), and whether it was fixed here.

The DDL corpora mark every statement whose execution differs (`xfilter.py`) as "EXPLAIN only", so that both shells
keep the same schema; the marked statements are this list. Per corpus: a 130, av 103, att 147, wr 122, fk 150, vt 136,
alt 211, edge 25, edge2 6 (of 4 000 / 4 000 / … / 293 / 79 statements).

## Engine DDL and connection state, not fixed (D59–D74, D81)

- **D59 — ANALYZE of a view or a virtual table.** `CREATE VIEW v AS SELECT 1; ANALYZE v;` C runs openStatTable() before
  analyzeOneTable() sees the view: `sqlite_stat1` is created and the schema cookie changes; the engine does nothing.
  Seen in every random corpus (`ANALYZE <view>`; the class counts per corpus are in PROGRESS.md).
- **D60 — ANALYZE of a system table.** `ANALYZE sqlite_sequence`: C writes no statistics for `sqlite\_%` tables
  (analyzeOneTable, analyze.c:1021), the engine writes `sqlite_sequence||<n>`.
- **D61 — ALTER TABLE forms of 3.5x missing in the engine's parser.** `ALTER TABLE t DROP CONSTRAINT c`,
  `ALTER TABLE t ALTER COLUMN c SET NOT NULL [ON CONFLICT …]`, `… DROP NOT NULL`, `ALTER TABLE t ADD [CONSTRAINT c]
  CHECK(…)`: C executes them (or reports "no such constraint: c"), the engine reports `near "CONSTRAINT": syntax error`
  / `near "CHECK": syntax error` / "expected RENAME, ADD, or DROP in ALTER TABLE". EXPLAIN of these statements is now
  C's listing (the generator ports them).
- **D62 — VACUUM INTO an unopenable path.** C "unable to open database: nodir/x.db", the engine the OS error with the
  absolute path ("/tmp/…/nodir/x.db: No such file or directory (os error 2)").
- **D63 — `VACUUM temp`.** C codes nothing for the temp database (vacuum.c:126); the engine resets the schemas, so C's
  schema generation (P4 of OP_Transaction of every later program) is one lower than the engine's.
- **D64 — C's schema generation after ALTER TABLE and after failing schema statements.** When a statement that changed
  the schema fails at run time, C rolls back and resets every schema (`sqlite3ResetAllSchemasOfConnection`, generation
  +1); ALTER TABLE reloads its database and temp (`OP_ParseSchema` with no WHERE). The engine's mirror misses: ALTER
  TABLE of a table of an attached database (`ALTER TABLE aux.t1 ADD COLUMN …`: C's generation 2, the engine 1),
  `CREATE VIRTUAL TABLE r USING rtree` (rtree's "Wrong number of columns for an rtree table"), `ALTER TABLE … RENAME
  COLUMN a TO b` / `DROP COLUMN` failing inside `sqlite_rename_*`, a CREATE TABLE whose generated column fails in
  OP_SqlExec (also D51: C reports it as a prepare error). Every later OP_Transaction P4 differs.
- **D65 — `ATTACH x'00' AS a`.** C attaches a temporary database (empty file name), the engine fails ("unable to open
  database:").
- **D66 — `DROP TABLE nosuchdb.t`.** C "no such table: nosuchdb.t", the engine "unknown database nosuchdb".
- **D67 — DROP / ALTER of an eponymous virtual table.** `DROP TABLE generate_series` / `json_each` /
  `pragma_table_info`, `ALTER TABLE generate_series RENAME TO g`: C "table generate_series may not be dropped" /
  "… may not be altered", the engine "no such table: generate_series". (The generator reports these statements
  unsupported: the engine does not connect an eponymous table a DDL statement names.)
- **D68 — CREATE VIRTUAL TABLE fts5 over an existing shadow-table name.** C "fts5: error creating shadow table f3_data:
  table 'f3_data' already exists", the engine "table f3_data already exists".
- **D69 — ANALYZE writes the statistics rows in another order.** C analyzes the tables in its `tblHash` order; the
  engine in its own order, so the rowids of `sqlite_stat1` rows (and the database bytes) differ (edge block 6; the
  probes `SELECT * FROM sqlite_stat1` show the same rows in another order).
- **D70 — error offset of "unknown database".** `CREATE TABLE aux.t(v)` without `aux`, executed by the engine: C
  prints the error position (sqlite3TwoPartName, `%T`), the engine none (all-kinds #579).
- **D71 — side effects of `EXPLAIN PRAGMA`.** C's sqlite3Pragma acts while the statement is prepared, so
  `EXPLAIN PRAGMA encoding='utf16'` sets the connection's text encoding, `EXPLAIN PRAGMA foreign_keys=ON` /
  `recursive_triggers=ON` / `writable_schema=RESET` / `page_size` / `journal_mode` change the connection; the engine's
  EXPLAIN of a PRAGMA changes nothing. All-kinds: 56 differences (`OP_SetCookie 0 5 <enc>` of a later CREATE, BINARY-16
  collations), 3 FK / recursive-trigger programs, 4 after `writable_schema=RESET`, 3 after `page_size` / `journal_mode`.
  To be fixed with the PRAGMA port (V6b).
- **D72 — `PRAGMA defer_foreign_keys`** is not implemented by the engine (no row; `SQLITE_DeferFKs` is not given to the
  generator). Not visible in the V6 corpora's listings (sqlite3FkDropTable and fkLookupParent read it); V6b.
- **D73 — `CREATE INDEX i ON t(a NULLS FIRST)`.** C "unsupported use of NULLS FIRST" (sqlite3HasExplicitNulls), the
  engine creates the index (edge2).
- **D74 — rtree xBestIndex row estimate.** For a DELETE / SELECT on an rtree table C's module estimates 50 rows
  (from the sqlite_stat1 row of `%_rowid`), the engine's module 524 288; the plan and the listing are the same in the
  corpus (ddl_vt: 2 loop / path dumps of wdiff6).

- **D81 — unqualified CREATE INDEX on a table of another database.** `CREATE INDEX i ON r10(c1)` where `r10` exists only
  in `aux` (or `temp` without a qualified index name in some forms): C ties the table name to the index's database
  (main; sqlite3FixSrcList) and fails "no such table: main.r10"; the engine finds the table in the attached database and
  creates the index there (ddl_att / ddl_alt: 14 + 7 statements, also "no such column: rowid" where C reports the
  missing table first).

The engine's errors for `INSERT OR REPLACE` into a STRICT table (NOT NULL vs datatype, ddl corpora) are D45; the
engine's "table t11 has no column named n75" where C names the table as written (`aux.t11`, `%S`) is the D44 family.
The CREATE TABLE statements C rejects for "conflicting ON CONFLICT clauses specified" and the engine creates are D42.

Counts of the marked statements by class over the nine corpora (`tools/xfcls.py`, res/xf_classes_r1.txt, 1 030 in
all): ALTER TABLE 3.5x forms (D61) 426, VACUUM INTO (D62) 195, ALTER TABLE generation (D64) 147, VACUUM temp (D63) 113,
ANALYZE (D59 / D60 / D69) 91, DML error texts (D45 / D44) 23, unqualified CREATE INDEX (D81) 21, CREATE TABLE (D42 /
D64) 4, DROP of an eponymous table (D67) 3, CREATE VIRTUAL TABLE (D64 / D68) 2, DROP TABLE sqlite_stat1 (the
generator's V3 rule for rewritten statistics) 2, D73 1, D66 1, D65 1.

## Engine connection state the generator needed — fixed in this step because they blocked the evidence (D75–D80)

The rule of the task: engine DDL behaviour is not changed in this step unless it blocks the generator's evidence. These
six are C connection state that C's prepare reads or changes and the engine did not mirror; without them every later
EXPLAIN of the block differs (cascade). The engine's DDL execution itself is unchanged.

- **D75 (fixed) — C opens the temp database while it prepares ALTER TABLE.** RENAME TO / RENAME COLUMN / DROP COLUMN of
  a non-TEMP table code nested statements that read `temp.sqlite_schema`; `sqlite3CodeVerifySchema(1)` →
  `sqlite3OpenTempDatabase` opens the temp b-tree at prepare time, also under EXPLAIN (a nested parse clears `explain`).
  Every later program then has `OP_Transaction 1 …` where the engine had none (DROP … IF EXISTS of a missing object:
  sqlite3CodeVerifyNamedSchema). Fix: the generator reports the open (`Host::note_temp_opened`); the engine opens its
  temp database (`lib.rs` prepare; ALTER statements are prepared by the port for this side effect).
- **D76 (fixed) — the iteration order of C's tblHash.** ANALYZE and REINDEX walk `Schema.tblHash`; after this
  connection created and dropped objects, C's hash is not a fresh load of the current objects (removals keep the bucket
  count; nested CREATE of sqlite_sequence is inserted before its table; fts5 / rtree shadow tables after their virtual
  table). Fix: the engine records the history since C's last load (`DbSlot::c_tbl_hash`: names of the load, then
  inserts and removals; reset when C reloads), the generator replays it into C's hash (`trigger::CHash`, incl.
  `removeElementGivenHash`).
- **D77 (fixed) — row estimates C raises at prepare.** `sqlite3CreateIndex()` calls `sqlite3DefaultRowEst()` with the
  live table, so the prepare of a CREATE INDEX (also EXPLAIN) raises a table's `nRowLogEst` below 99 to 99 until the
  statistics are read again; ALTER TABLE's `renameTestSchema()` parses every CREATE INDEX of the schema and does the same
  for every indexed table (also when the ALTER fails afterwards). Seen as different plans after `ANALYZE` of small
  tables. Fix: `DbSlot::c_rowest_tables` (from the generator's prepare, `Host::note_rowest_clamped`) and
  `c_rowest_clamped`; the generator applies them after its statistics load.
- **D78 (fixed) — stale statistics after ALTER TABLE's reload were not detected.** The engine cleared its record of the
  statistics C had read (`c_stat_seen`) when ALTER TABLE reloads the schema, so statistics removed later (a table
  emptied and analyzed again keeps C's old `nRowLogEst`) were not reported unsupported. Fix: the record is recomputed
  from the statistics the reload reads (`c_stat_names`).
- **D79 (fixed) — `pragma_quick_check` of ALTER TABLE ADD COLUMN.** C's ADD COLUMN of a CHECK / NOT NULL generated /
  STRICT column runs `SELECT … FROM pragma_quick_check(%Q,%Q)`; the engine did not connect that eponymous table for the
  generator (the statement was unsupported). Fix: `vtab.rs` connects it for ADD COLUMN (also `c_generated_program`).
- **D80 (fixed) — `PRAGMA analysis_limit` was not given to the generator** (`Db.n_analysis_limit`; ANALYZE codes it).

## Generator defects found by the V6 evidence and fixed (earlier steps' code)

- A column DEFAULT `-0.0` (BLOB affinity) was loaded as `+0.0` (`value_from_expr`, V3): C keeps the sign (sqlite3AtoF);
  seen only in the program layer (OP_Column P4 `R8000000000000000`), the EXPLAIN text is `0` for both.
- Error offsets of the DbFixer messages ("view %T cannot reference objects in database %s", trigger form): C records
  the offset at the name token (FinishTrigger: no offset, its token is not in the statement text).

## Machine (port of the VDBE) — E30–E31, input of step M4

- **E30 — DDL programs need opcodes the machine does not run:** OP_SetCookie, OP_ParseSchema, OP_CreateBtree,
  OP_Expire, OP_DropTable, OP_Destroy, OP_LoadAnalysis, OP_SqlExec, OP_Vacuum, OP_DropTrigger, OP_VCreate, OP_DropIndex,
  OP_JournalMode, OP_VDestroy, OP_VRename (counts per statement kind: karar_esligi_v6.md §4, res/opcount_r1.txt). DDL
  stays on the engine (no route in V6).
- **E31 — SQL functions of DDL programs the machine's function library must provide:** `sqlite_attach(3)`,
  `sqlite_detach(1)`, `stat_init(4)`, `stat_push(2)`, `stat_get(1)`, `sqlite_rename_table(7)`, `sqlite_rename_column(9)`,
  `sqlite_rename_test(7)`, `sqlite_rename_quotefix(2)`, `sqlite_drop_column(3)`, `sqlite_drop_constraint(2)`,
  `sqlite_add_constraint(3)`, `sqlite_find_constraint(2)`, `sqlite_fail(2)` (internal functions; C runs them with
  `nested` set).

## all-kinds (39 300 statements): new EXPLAIN / EQP differences, all engine state

EXPLAIN: 238 differences of 36 329 accepted (V5: 486 of 26 811); 316 of V5's differences are gone, **68 are new**, each
traced (`tools/akdiv.py`, res/all_kinds_firstdiv_r1.txt, classes res/all_kinds_cls_r1.txt): 56 text encoding after an
`EXPLAIN PRAGMA encoding` (D71), 4 after `PRAGMA writable_schema=RESET`, 3 after `page_size` / `journal_mode` (D71),
3 FK / trigger programs after an EXPLAIN-only `PRAGMA foreign_keys` / `recursive_triggers` (D71), 1 error offset of
the engine's "unknown database" (D70), 1 `RETURNING main.rt.*` of a statement the engine parses (C: syntax error; the
D58 / F8 class, switch off). EQP: 170 differences (V5 347); 2 new (#579 D70, #37522 the D58 class).
No new difference is a generator difference.

## Tool notes

- sdiff6's C side (`$S/v4b/wdump/cexplain_v4b`, a library driver) runs without SQLITE_DBCONFIG_DEFENSIVE while both
  shells run with it: `ALTER TABLE f9_data RENAME TO x` (an fts5 shadow table) succeeds only in the driver (edge,
  schema of block 4).
- A statement line ending in a `--` comment cannot be run by the line tools (the `;` they append is inside the comment);
  the hand-written corpora use `/* */`.
