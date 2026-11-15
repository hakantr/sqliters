# Differences from C noticed outside the V5 task

D42-D48 (engine) and E16-E19 (machine) are new in step V5 (DML, triggers and foreign keys in the code generator). None is a difference of a generated program: every statement involved has an EXPLAIN / EXPLAIN QUERY PLAN listing identical to C's and, where the op scripts reach it, a program identical to the one C prepares (program layer). Reference: `$S/bin/sqlite3` (3.54.1). Scratch copy with the tools: `target/perf-work/v5/DIFFS_OUTSIDE_TASK.md`.

Numbering continues the earlier steps (D1–D41 engine, E1–E15 machine). Each item: what C does, what the engine /
machine does, where it was seen, why it is outside V5.

## Engine (the old executor / the engine's own prepare) — D42–D47

- **D42 — conflicting ON CONFLICT clauses.** `CREATE TABLE t(a,b, PRIMARY KEY(b,a) ON CONFLICT ROLLBACK, UNIQUE(b,a) ON
  CONFLICT ABORT)`: C rejects ("conflicting ON CONFLICT clauses specified"), the engine creates the table (its root
  pages then differ from C's for everything created after it). Seen in the raw DML corpora (61 EXPLAIN differences of
  dml_a before tools/cfilter.py drops setup DDL that C rejects).
- **D43 — the engine's prepare rejects DML that C compiles.** `DELETE FROM t INDEXED BY <partial index>` without
  WHERE: engine "no query solution", C truncates (OP_Clear); a duplicate `ON CONFLICT(x)` clause that C never resolves
  (isDup) with a bad function call (`length(1.6, 4)`, `coalesce(x)`) gives the engine's "wrong number of arguments to
  function …()"; seen through the API driver (res/firstdiv5_*_r7.txt: 11+2+4+1+2 scripts, plus singles).
- **D44 — the engine accepts DML C rejects at compile time, or reports another text.** ON CONFLICT targets that match
  no PRIMARY KEY / UNIQUE constraint; `table %S has N columns …` where C prints the alias of `INSERT INTO t AS ins`
  (`%S`); "cannot UPSERT a view" vs C's "cannot modify v because it is a view"; ambiguous / unknown column texts of
  UPDATE … FROM. The generator gives C's error (EXPLAIN shows it); the statement itself still runs through the engine's
  prepare. (res/firstdiv5_*_r7.txt class "C compile error, the engine's prepare reports its own error/none": 134 / 76 /
  80 / 68 / 68 scripts of ops_dml_a / t / f / tf / v.)
- **D45 — STRICT type-check order of an INSERT.** `INSERT OR IGNORE INTO t0(a,b,d) VALUES(444,'x','zz')` on a STRICT
  table: C reports the first failing column in storage order ("cannot store TEXT value in BLOB column t0.d"), the
  engine another column ("… INT column t0.c"); also "datatype mismatch" of C where the engine stores the value
  (setup inserts of the DML corpora, class "E op" of firstdiv5).
- **D46 — rtree RETURNING value.** `INSERT INTO rt VALUES(15,-3,4.7) RETURNING *` (vtab write, engine path): C returns
  the stored REAL `-3.0` (rtree coordinates are REAL), the engine returns the INTEGER `-3`.
- **D47 — database page bytes.** 3–8 scripts per DML corpus end with the same stdout but a database file that differs
  in a few bytes (e.g. ops_dml_t/b00002: 33 bytes); the old path gives the same difference (btree / pager layer).

## Machine (port of the VDBE; switch SQLITERS_VDBE_EXEC) — E16–E19, for the machine step

- **E16 — OP_IdxDelete over an expression index** (M2 noted it): without `sqlite3VdbeFindIndexKey` the machine
  reports SQLITE_CORRUPT_INDEX (779, "database disk image is malformed") where C deletes the neighbouring entry
  (ops_dml_a/b00214 line 53: `DELETE FROM t1 RETURNING …`).
- **E17 — a UNIQUE conflict not detected.** WITHOUT ROWID table `t0(a TEXT UNIQUE, b INT, PRIMARY KEY(a DESC))`,
  `UPDATE OR ROLLBACK t0 SET b = a, a = length((SELECT max(a) FROM t0)) WHERE b <= 17`: C fails with "UNIQUE constraint
  failed: t0.a", the machine updates one row (the program is C's, byte for byte: ops_dml_t/b00154 line 19).
- **E18 — virtual-table writes** (`OP_VOpen` in write programs, `OP_VUpdate`, `OP_VBegin`) are not implemented: 643
  statements of ops_dml_v fall back to the engine.
- **E19 — the CLI route.** `SQLITERS_VDBE_EXEC=1` sends only SELECT to the machine. A route for DML has to use C's
  compile result (program or error) before the engine's prepare (D43, D44).

## all-kinds (cascades of earlier divergences, new in V5 because DML is now compiled)
119 new EXPLAIN differences, each attributed to the first earlier statement whose output differs (res/
all_kinds_firstdiv_r6.txt) or to the first statement after which root pages / the schema cookie differ
(tools/rootdiv.py, $S/v3/gendiv.py): fts3 / fts4 / zipfile modules missing (D5), shell functions readfile, REGEXP,
trigfunc, decimal (D4), CREATE TABLE … CONSTRAINT forms and ALTER TABLE DROP CONSTRAINT (D14/D16 family), echo module,
PRAGMA temp.page_count / defer_foreign_keys / query_only, VACUUM temp and ALTER RENAME cookies (D25/D27 family), CREATE
INDEX root page after VACUUM INTO (#13585), temp root page (#24132), ATTACH with URI options; 2 (#8075, #28592) are not
reproducible when their chunk runs alone (identical output: harness file state).
- **D48 — engine stack overflow (crash) on mutually dependent generated columns.** `CREATE TABLE t0(…, c INT
  GENERATED ALWAYS AS (e * b) STORED, …, e INT GENERATED ALWAYS AS (b * c) STORED)` is accepted by C and the engine;
  `INSERT … INTO t0` makes C report "generated column loop on \"e\"", the engine process aborts with a stack overflow
  (same with the V4b binary $S/v5_bin/sqliters.r0). Seen in the rich DML corpora (dml_r_a, dml_r_t, dml_r_v: the 3 EQP
  "differences" and the "not reached" counts are statements after the crash in the same chunk).
