# Differences from C noticed outside the V4a task (recorded, not fixed)

D30-D32 are new in step V4a (window functions in the code generator). None is a difference of a generated program:
every statement involved has an EXPLAIN / EXPLAIN QUERY PLAN listing identical to C's. Reference: `$S/bin/sqlite3` (3.54.1).
Scratch copy with the tools: `target/perf-work/v4a/DIFFS_OUTSIDE_TASK.md`.

## D30. WITHOUT ROWID table with a column-level `UNIQUE PRIMARY KEY DESC`: the engine writes the rows in ascending order
- Found by: rowdiff.py on win_x.sql (#587, #597; machine path, SQLITERS_VDBE_EXEC=1), then minimized (ctxw.py, bis).
- Repro (file written by the engine, checked by C):
  `CREATE TABLE t2(b0 INT2 UNIQUE PRIMARY KEY DESC, zz2) WITHOUT ROWID; INSERT INTO t2 VALUES (4,'a'),(5,7),(1,10),(10,3);`
  with `target/release/sqliters e.db`, then `sqlite3 e.db "PRAGMA integrity_check"` -> 3 x
  `row not in PRIMARY KEY order for t2` (the file C writes for the same script differs from byte 4106 on).
  `b0 INT PRIMARY KEY DESC` without the UNIQUE is correct; the column-level UNIQUE before PRIMARY KEY makes the engine
  lose the DESC of the PRIMARY KEY index.
- Effect: the engine's executor reads its own order and returns C's rows (no difference on the default path); the
  machine reads the b-tree with C's key order (from C's schema) and misses rows: `SELECT zz2 FROM t2 WHERE b0=1` -> no
  row (C: 10); a scan in another index's order stops after the first row. The file is not C-compatible.
- Not caused by V4a: the same with the Part 0 baseline binary `$S/v4_bin/base_1100/sqliters` (pre-V4a tree).
- Scope: engine (build / storage of WITHOUT ROWID tables). Not fixed.

## D31. Window-chain error messages print the window name dequoted
- Found by: rowdiff.py on win_x.sql / win_y.sql (engine path; the statements fail in the engine's prepare): 31 + 15
  statements (res/win_x_rows_r2.json, res/win_y_rows_r2.json).
- C (window.c sqlite3WindowChain): `cannot override %s of window: %s` with `pWin->zBase`, which is the token text as
  written (parse.y: `sqlite3DbStrNDup(pParse->db, W.z, W.n)`, not dequoted), e.g.
  `cannot override ORDER BY clause of window: "W0"`; the engine (crates/sqliters-core/src/window.rs
  `chain_against_base`) prints `W0`. Same for `no such window: %s`.
- The generator's message is C's (EXPLAIN of these statements: identical). Scope: engine. Not fixed.

## D32. Error precedence of window errors: the engine stops at its first error, C reports its last one
- Found by: rowdiff.py on win_x.sql (#1904, #5129, #6354) and win_y.sql (2 statements); engine path.
- C's sqlite3ErrorMsg() keeps the LAST message of the statement: e.g. sqlite3WindowUpdate() reports `cannot override
  frame specification of window: w1` from sqlite3WindowChain() and then, for the same window,
  `RANGE with offset PRECEDING/FOLLOWING requires one ORDER BY expression`, which is the message C prints. The engine
  prints the first one. Also: a `rowid` of a view in WHERE (`no such column: rowid` in the engine) where C first
  reports the window error of the select list.
- The generator's message is C's (EXPLAIN identical). Scope: engine (its resolver order). Not fixed.

## Not V4a differences, seen again
- D21 (V3): the engine rejects an aggregate of an outer query in the ORDER BY of a sub-select (`misuse of aggregate:
  count()`), C accepts: seen again with window sub-selects, win_d.sql #4 and #6 (`SELECT (SELECT row_number() OVER ()
  FROM t2 ORDER BY count(t1.a)) FROM t1`); the generator's program is C's (EXPLAIN identical).
- dmlfuzz 22 seeds (V3: same seed set), finder t/g06_vacuum_into_main_untouched (environmental), the 4 known
  `*_differential_parity` vtab test failures.
