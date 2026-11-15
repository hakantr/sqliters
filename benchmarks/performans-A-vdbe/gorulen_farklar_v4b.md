# Differences from C noticed outside the V4b task

D33-D41 are new in step V4b (virtual tables and table-valued functions in the code generator). D33 was fixed (engine,
a one-line risk flag); the others are recorded. None is a difference of a generated program: every statement involved
has an EXPLAIN / EXPLAIN QUERY PLAN listing identical to C's (the `vtab:%p` address of P4_VTAB aside, which is a
pointer in C too). Reference: `$S/bin/sqlite3` (3.54.1). Scratch copy with the tools: `target/perf-work/v4b/DIFFS_OUTSIDE_TASK.md`.

## D33 (FIXED). pragma_* table-valued functions were "innocuous" in the engine
- C: `pragmaVtabConnect()` (pragma.c:2821) does not call `sqlite3_vtab_config()`: eVtabRisk = SQLITE_VTABRISK_Normal, so
  with `PRAGMA trusted_schema=0` (the reference shell's default) a view that reads a pragma table-valued function fails:
  `CREATE TABLE t1(a); CREATE VIEW vp AS SELECT name FROM pragma_table_info('t1'); SELECT * FROM vp;` ->
  `Parse error near line 3: unsafe use of virtual table "pragma_table_info"`. The engine ran the view.
- Found by: vt_d.sql (EXPLAIN of `SELECT * FROM vp`: the generator follows the engine's risk flag).
- Fix: `crates/sqliters-core/src/vtab.rs` PragmaVTabModule: risk_flags Normal, no SQLITE_VTAB_INNOCUOUS in xConnect.
  Execution now gives C's error; EXPLAIN too.

## D34. pragma_* eponymous tables the engine does not have
- C registers `pragma_<name>` for every pragma with PragFlg_Result0|Result1 (sqlite3PragmaVtabRegister); the engine
  knows 16 (table_info, table_xinfo, index_list, index_info, index_xinfo, foreign_key_list, function_list,
  module_list, pragma_list, collation_list, database_list, table_list, integrity_check, quick_check,
  foreign_key_check, compile_options). `SELECT * FROM pragma_journal_mode` (also pragma_foreign_keys, page_size,
  user_version, cache_size, … about 45 names) -> engine `no such table`, with and without EXPLAIN (vt_d #173, #174).
- Scope: engine feature. Not fixed.

## D35. The engine executor's xBestIndex / xFilter contract differs from C's (invisible today)
- The generator asks the modules with ports of C's xBestIndex (`best_index_calls`: series.c of the shell, json.c,
  dbstat.c, pragma.c, fts5_vocab.c; fts5 and rtree already were ports). The engine's executor still plans with its
  own `best_index()` and its cursors' `filter()` expect that plan (generate_series: older bit layout, no LIMIT /
  OFFSET; dbstat: aggregate 0x02 instead of 0x04, name not an argument; json_each: a root without json).
- Invisible while the executor runs its own plan; must be aligned before the VDBE machine runs C's OP_VFilter
  (M-step).

## D36. Module registration
- json_each / json_tree / jsonb_each / jsonb_tree: C registers them at their first eponymous use
  (sqlite3JsonVtabRegister); the engine has them from the start: `PRAGMA module_list` / `pragma_module_list` lists
  them before any use; `CREATE VIRTUAL TABLE x USING json_each` before any use: C `no such module`.
- Shell-only modules (D5): `fsdir`, `completion` (6 all-kinds statements use fsdir: `no such table: fsdir` in the
  engine; with V4a they were "unsupported").

## D37. The engine executor ignores constraints on table-valued-function hidden columns (data difference)
- `SELECT value, start, stop, step FROM generate_series(0,3,1) WHERE stop = 1` C: no row; engine: `0|0|1|1`, `1|0|1|1`.
  `SELECT value FROM generate_series(0,5) WHERE start = 1` C: none; engine 1..5. `SELECT seq,name FROM
  pragma_index_list('t2') WHERE arg='t1'` C: none; engine the indexes of t1. `SELECT count(*) FROM
  generate_series(3,20,-1) AS v, t1 WHERE t1.a > 1 AND v.start IN (0,100,20,7)` C 0, engine 246. 59 statements of
  vt_x (rowdiff, res/vt_x_rows_r2.json), the same with the V4a binary.
- C's program re-tests every constraint the module did not omit (the second `stop=` term); the engine trusts the
  module. The generator's program is C's (EXPLAIN identical): fixed when the machine runs virtual-table programs.

## D38. Error texts and phases around virtual tables (engine)
- `unknown database 'nosuch'` (C quotes) vs `unknown database nosuch`; `dbstat('nosuch')` C: no rows, engine error;
  `ft MATCH 'one' OR rowid=1` C runs it (multi-index OR over the fts5 table), engine "unable to use function MATCH in
  the requested context"; `json_tree(...) WHERE root IN (10,20,100)` C no rows, engine "bad JSON path: '10'"; a dbstat
  self-join: C "no query solution", engine runs it. 22 statements of vt_x.

## D39. "no query solution" is a run-time error in the engine
- C fails such statements at prepare (`Parse error near line N: no query solution`), the engine when it steps
  (`Error near line N: ...`). 45 statements of vt_x; EXPLAIN gives C's error (generator).

## D40. Name-resolution error precedence with table-valued functions in RIGHT / FULL joins
- `SELECT count(*) FROM t1 RIGHT JOIN json_each(t1.a) AS v ON 1 WHERE ...` C accepts (0), engine `no such column:
  t1.a`; other statements: C reports another unknown column first. 41 statements of vt_x.

## D41. Row order without ORDER BY
- 14 statements of vt_x return the same rows in another order (C's plan vs the engine executor's plan; the V2 class).

## All-kinds (39 300 statements, EXPLAIN): what V4b changed
- accepted 14 532 -> 14 773, identical 14 177 -> 14 406, different 355 -> 367 (final binary r3). The 355 are V4a's statements (352 with
  byte-identical outputs, 3 = the random-data root pages #24135 / #24137 / #24170). The 12 new: 6 `fsdir` (D5/D36:
  shell-only module), 6 OP_Transaction generation cascades of D27 (gendiv.py: after #37420
  `ALTER TABLE t1 ADD COLUMN d INT CHECK(RAISE(ABORT, 'add_chk'))` and #38864 `CREATE TABLE t_gen(x INT, y AS
  (RAISE(IGNORE)))`), statements that were "unsupported" before. No statement is unsupported for a virtual-table
  reason any more (r2 still had 2: `temp.pragma_table_info(...)`, fixed in r3). EXPLAIN QUERY PLAN: 14 773 accepted,
  differences = V4a's 272 + the 6 fsdir statements.
