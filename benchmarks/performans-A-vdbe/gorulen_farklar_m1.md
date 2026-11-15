# Differences from C noticed during M1 that are not the machine's (old engine / code generator)

All repros: C = $S/bin/sqlite3 (3.54.1), engine = sqliters CLI. "machine on the same file" means: the machine's output on
a database file built by C is byte-identical to C's.

## E1 — expression index on lower(x): index rows missing in the file the engine writes (old write path)
```
CREATE TABLE t3(c0 INT PRIMARY KEY, c2 REAL UNIQUE, c3 INTEGER) WITHOUT ROWID;
CREATE INDEX t3_i1 ON t3(lower(c2), c3);
INSERT INTO t3 VALUES(26,NULL,'x'),(41,x'0102','y'),(60,x'00ff',1),(7,'ab2',3),(8,1.5,4),(9,'A',5);
PRAGMA integrity_check;      -- run by C on the file each engine built
```
C-built file: `ok`. Engine-built file: `row 6 missing from index t3_i1`. The same class shows on a rowid table with
`CREATE UNIQUE INDEX t2_i0 ON t2(lower(b3) DESC, b1 DESC, b0)` (res_exec_a_bugs.json db=110: `row 109 missing from index
t2_i0`). Effect in the differential: 5 of 12 510 machine statements of corpus exec_a differ from C when each engine
builds its own file (the index scan returns the rows in the order of the engine's index); 0 when both read the file C built.

## E2 — sqlite_stat1 row (tbl, NULL, 'N') of a WITHOUT ROWID table on a fresh connection (code generator / stat loader)
```
CREATE TABLE t1(a0 INTEGER DEFAULT 1.5, a1 TEXT, a2, a3 VARCHAR(30), PRIMARY KEY(a0), UNIQUE(a2,a3)) WITHOUT ROWID;
ANALYZE sqlite_schema; INSERT INTO sqlite_stat1 VALUES('t1',NULL,'200'); ANALYZE sqlite_schema;
-- new process:
EXPLAIN QUERY PLAN SELECT a3+1 FROM t1;
```
C: `SCAN t1 USING COVERING INDEX sqlite_autoindex_t1_1`; engine (generator): `SCAN t1`. In the session that wrote the
statistics both agree. (exec_a --cdb: 1 of 6 427 machine statements; row order differs because the plan differs.)

## E3 — sum() / total() of a BLOB (function library)
`SELECT sum(x'3337'), total(x'3337');`  C: `37.0|37.0`  engine: `0.0|0.0` (old path and machine: both call the engine's accumulator).

## E4 — group_concat() of a BLOB that is not valid UTF-8 (engine text is a String)
`SELECT group_concat(x) FROM (SELECT x'c389' AS x UNION ALL SELECT x'89');`  C: bytes `c3 89 2c 89`; engine: `c3 89 2c ef bf bd`.

## E5 — MATCH without a virtual table
`SELECT a, a MATCH 'b' FROM t;`  C: runtime error `unable to use function MATCH in the requested context`; engine: prepare
error `no such function: match` (the statement never reaches the generator).

## E6 — a schema that names a collation that is not registered (code generator)
```
-- connection 1 registers collation REV_ASCII:  CREATE TABLE t_rev(id INTEGER PRIMARY KEY, k TEXT COLLATE REV_ASCII UNIQUE);
-- connection 2 (REV_ASCII not registered):       SELECT k FROM t_rev ORDER BY k;
```
C: `no such collation sequence: REV_ASCII` at prepare. The port of the generator produces a program (index scan with a
KeyInfo whose collation is NULL). The engine's executor reports C's error. M1 keeps every connection whose schema declares
an application-defined collation on the engine's executor (`vdbe_host::machine_program`), so the switch does not expose it;
`EXPLAIN` of such a statement is where it shows. (tests/phase6_driver_callbacks_and_hooks.rs
test_group21_persistent_schema_reload_and_late_collation_rebinding)

## E7 — tests that assert behaviour of the engine's executor that is not C's (seen with the switch on)
- tests/phase3_upsert_updatefrom_cte.rs `with_recursive_materialized_soft_keyword_priority_queue_and_interrupt`: expects a
  CTE declared `AS NOT MATERIALIZED` and joined to itself to evaluate its body once per reference (4 calls of a counting
  function). C's program for `WITH c AS NOT MATERIALIZED (SELECT f(1) AS x UNION ALL SELECT f(2)) SELECT a.x, b.x FROM c AS a
  JOIN c AS b ON a.x = b.x ORDER BY a.x` materializes the body once under `OP_Once` and opens the second reference with
  `OP_OpenDup` (EXPLAIN of C and of the generator are identical): 2 calls. The machine makes 2 calls.
- tests/phase1_core_ownership_and_lifecycle.rs `test_audit4_without_rowid_and_not_indexed_incremental_vdbe_zero_overflow_reads`:
  expects a PRIMARY KEY seek on a WITHOUT ROWID table whose rows spill to overflow pages to read 2..6 pages. C's
  `sqlite3BtreeIndexMoveto` reads the whole key of every probed cell that spills ("The record flows over onto one or more
  overflow pages ... accessPayload()"); the machine does the same (38 pages in the test).

## E8 — function library differences seen in the V1 corpus (same output on the old path and on the machine)
From `res_v1_exec_bugs_classified.json` (class "machine==old") and `vm_v1.json`:
- JSON operators / functions that raise `malformed JSON` or `bad JSON path` in C and return a value (or another error) in the
  engine, e.g. `SELECT 'it''s' ->> NULL;` (C: error `malformed JSON`), `SELECT 'true' -> X'0102ff';` (C: NULL; engine:
  `bad JSON path: '<blob>'`), `SELECT json_error_position(CURRENT_DATE);` (C 1, engine 5).
- `bm25()` / `highlight()` outside an FTS5 query: C `unable to use function bm25 in the requested context`; engine: no error or
  `wrong number of arguments`.
- `SELECT sqlite_source_id();` — the engine's string is shorter than C's (84 vs 95 bytes).
- date / time functions with out-of-range numeric arguments: `SELECT time(0xffffffffffffffff);` C: NULL, engine `12:00:00`;
  `SELECT julianday(2147483648);` C: NULL, engine `2147483648.0`; `SELECT 1 WHERE strftime(4294967296, -26);` C: no row, engine: 1 row.
- text that is not valid UTF-8 reaching the output or a function: `SELECT '1' || X'0102ff';` C prints byte ff, engine U+FFFD
  (also `printf(x'ABCD', ..)`, `concat(..)`, `concat_ws(..)`).
- `rtreecheck()` / `zeroblob()` results differ in representation in the opcode trace only (zeroblob: C keeps `MEM_Zero`).
