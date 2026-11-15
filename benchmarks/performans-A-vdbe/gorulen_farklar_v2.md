# Differences from C noticed outside the V2 task (recorded, not fixed unless stated)

Reference: `$S/bin/sqlite3` (3.54.1). Engine: worktree `performans-A-vdbe`.

## D1. Automatic index numbering of a WITHOUT ROWID table whose PRIMARY KEY is one INTEGER column
```
CREATE TABLE t1(a, b, c INTEGER, PRIMARY KEY(c), UNIQUE(a,b)) WITHOUT rowid;
PRAGMA index_list(t1);
SELECT name, rootpage FROM sqlite_schema;
```
C: `0|sqlite_autoindex_t1_2|1|pk|0`, `1|sqlite_autoindex_t1_1|1|u|0`, schema rows `t1|2`, `sqlite_autoindex_t1_1|3`
engine: `0|sqlite_autoindex_t1_1|1|pk|0`, `1|sqlite_autoindex_t1_2|1|u|0`, schema rows `t1|2`, `sqlite_autoindex_t1_2|3`
Cause: C's parser treats `PRIMARY KEY(c)` on an INTEGER column as the rowid alias until it sees WITHOUT ROWID at the
end of the statement; `convertToWithoutRowidTable()` then creates the PRIMARY KEY index last (after the UNIQUE ones).
The engine creates it at the position of the constraint (`schema.rs`, `!ct.without_rowid` in the alias test).
Effect on V2: the generator reports such tables as unsupported ("WITHOUT ROWID table whose automatic index names
differ from C") instead of producing a plan from a differently ordered index list.

## D2. Flag pragmas were ignored by the engine
`PRAGMA automatic_index`, `reverse_unordered_selects`, `short_column_names`, `full_column_names` printed nothing and
kept no state (C prints 1 / 0 / 1 / 0 and keeps the flag); `PRAGMA case_sensitive_like=x` is ignored.
V2 change: the four flag pragmas now keep their value (`DatabaseSet::c_flags_toggled`) and answer the query form as C
does; the value is used only by C's code generator (EXPLAIN / EXPLAIN QUERY PLAN). The old executor still ignores
them (e.g. `reverse_unordered_selects=1` does not reverse the rows it returns; `case_sensitive_like=1` does not
change LIKE). After `PRAGMA case_sensitive_like=x` the generator reports statements that use like() as unsupported.

## D3. sqlite_stat1 rows whose idx names no index were ignored (FIXED in the stat1 loader)
```
CREATE TABLE t1(a, b TEXT); CREATE TABLE t2(c INTEGER PRIMARY KEY, d); CREATE TABLE t3(e INTEGER PRIMARY KEY, f);
ANALYZE sqlite_schema;
INSERT INTO sqlite_stat1 VALUES('t1','idx1','600 6'),('t2','idx1','6 6'),('t3','idx2','6 6');
ANALYZE sqlite_schema;
EXPLAIN QUERY PLAN SELECT 'result' FROM t1, t2, t3 WHERE t2.c=t1.b AND t2.d!='silly' AND t3.e=t1.b AND t3.f!='silly';
```
C (`analysisLoader`): when the idx column names no index (or equals the table name and the table has no PRIMARY KEY
index) the row is taken as the table's own row: `nRowLogEst` = first integer, `TF_HasStat1`. The engine skipped such
rows (`Schema::reload_stat1`). Fixed there (the adapter's stat1 data comes from this loader).

## D4. Functions of the reference shell that the engine's shell does not have
`regexp`, `sha1`, `sha1b`, `sha3`..., `zipfile`, `decimal` collation etc.: `SELECT 'a' REGEXP 'b'` compiles in the C shell
and is "no such function" in the engine's shell (211 statements of the all-kinds corpus; same in V1).

## D5. Virtual table modules of the reference shell missing in the engine (fts3/fts4, echo, wholenumber, ...)
C reports e.g. `no such module: fts3` where the engine reports `no such table` (the CREATE VIRTUAL TABLE failed
earlier in both, with different consequences for later statements).

## D6. DEFAULT of an integer literal that does not fit 64 bits
`CREATE TABLE t300(..., g DEFAULT 9223372036854775808)`: C's `sqlite3ValueFromExpr` gives the REAL 9.223372036854776e+18
(P4 of OP_Column in `EXPLAIN SELECT * FROM t300`); the engine's `default_value_from_expr` gives -9223372036854775808.

## D7. C keeps statistics of rows that were removed from sqlite_stat1 (recorded; the generator reports "unsupported")
`sqlite3AnalysisLoad()` does not reset `Table.nRowLogEst` / `szTabRow` (nor the unordered / noskipscan / sz= state of an
index) when a later ANALYZE finds no row for the object:
```
CREATE TABLE t1(a,b,c); CREATE INDEX t1ab ON t1(a,b); CREATE TABLE t2(x,y);
INSERT INTO t2 VALUES(1,1),(2,2),(3,3); ANALYZE;
DELETE FROM sqlite_stat1; INSERT INTO sqlite_stat1 VALUES('t1','t1ab','10000 5000 2'); ANALYZE sqlite_schema;
EXPLAIN QUERY PLAN SELECT x, y FROM t2 WHERE EXISTS (SELECT 1 FROM t1 WHERE b=x);
```
C still plans with 3 rows for t2 (`SCAN t1 EXISTS USING COVERING INDEX t1ab`); the engine's schema has the default
(200 rows) for t2. The engine now tracks which objects had statistics at C's last read (`DbSlot::c_stat_seen`) and the
generator reports the affected tables as unsupported until C's schema is reset; likewise all tables of a database
whose sqlite_stat1 was written by a statement and not yet re-read by ANALYZE (`c_stat_dirty`), because the engine may
re-read the statistics (its own schema reloads) at moments C does not.

## D8. The engine's parser rejects a parenthesized join in a FROM clause
`CREATE VIEW v3 AS SELECT 0 FROM v2 JOIN (v2 RIGHT JOIN v1);` -> engine: `near "RIGHT": syntax error`; C accepts.

## D9. The engine's shell guesses an error offset when the engine reports none
`guess_error_offset()` (sqliters-cli) marks the first matching identifier for "no such column" / "ambiguous column
name" / ... messages. For errors of C's code generator (EXPLAIN / EXPLAIN QUERY PLAN) the offset is now C's own
(`SqliteError::NO_OFFSET` = "none, do not guess"); other statements keep the guess.

## D10. ANALYZE of a single-column UNIQUE index that contains NULLs
```
CREATE TABLE t1(a0 INTEGER PRIMARY KEY, a1 BLOB NOT NULL, a2 TEXT COLLATE RTRIM, UNIQUE(a2,a0));
CREATE UNIQUE INDEX t1_i0 ON t1(a2 DESC);
INSERT OR IGNORE INTO t1 VALUES(0,1,NULL),(7,8,1),(14,15,2),(21,22,'s0'),(28,29,4),(35,36,'s2'),(42,43,NULL), ... (21 rows, a2 NULL in several)
ANALYZE; SELECT * FROM sqlite_stat1 WHERE idx='t1_i0';
```
C: `t1|t1_i0|21 1`; engine: `t1|t1_i0|21 2`. (C's analyzeOneTable: "for a single-column UNIQUE index, once we have found
a non-NULL row, we know that all the rest will be distinct" - the NULL rows do not count as duplicates.) The different
sqlite_stat1 content gives different costs (4 statements of the plan corpus: #12783, #12795, #12797, #12820).
Full repro: `python3 $S/v2/ctx.py $S/v2/corpus/plan_a.sql 12795 --chunk 100000`.

## D11. CREATE VIRTUAL TABLE with an unknown module fails at prepare in the engine, at run time in C
`CREATE VIRTUAL TABLE ft USING nosuchmod(x);` -> C shell: `Error near line N: no such module: nosuchmod` (run-time
error, after OP_ParseSchema changed the schema); engine shell: `Parse error near line N: no such module: nosuchmod`.
Consequences C has and the engine now mirrors for the generator's state only (`DatabaseSet::c_reset_all_schemas`,
`c_note_cookie_ahead`): in autocommit mode the rollback resets all schemas (iGeneration+1); inside a transaction only
the statement is rolled back, C's in-memory schema cookie stays one ahead of the file (EXPLAIN lists it) until the
next statement that checks the cookie, which then resets the schema (iGeneration+1).

## D12. ALTER TABLE ... DROP CONSTRAINT / ALTER COLUMN ... DROP NOT NULL (3.54 syntax) is not parsed by the engine
`ALTER TABLE t2 ALTER x DROP NOT NULL;`, `ALTER TABLE abc DROP CONSTRAINT one;` -> engine: syntax error; C executes
them (schema cookie and later statements then differ).

## D13. ATTACH with a URI file name and `8_3_names=1`
`ATTACH 'file:./test2.db?8_3_names=1' AS db2;` -> engine: `unable to open database: file:./test2.db?8_3_names=1`; C attaches.
