# Differences from C noticed outside the V2 / V3 tasks (recorded, not fixed unless stated)

D1-D13 were recorded in step V2; D14-D19 are new in step V3. D1 and D10 were fixed in the engine in V3 (item 2 of
the step), D18 and D19 as well (message text only).

Reference: `$S/bin/sqlite3` (3.54.1). Engine: worktree `performans-A-vdbe`.

## D1. Automatic index numbering of a WITHOUT ROWID table whose PRIMARY KEY is one INTEGER column (FIXED in V3)
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
V3 fix (`crates/sqliters-core/src/schema.rs`, `build_table_schema_from_ast`): what C writes into the file was
confirmed with the reference shell (`SELECT type, name, tbl_name, rootpage, sql FROM sqlite_schema`,
`PRAGMA index_list`): the PRIMARY KEY index of such a table gets the *last* automatic index number and the *last* root page, because
`sqlite3AddPrimaryKey()` makes the column the rowid alias (`iPKey`) without creating an index and
`convertToWithoutRowidTable()` creates the index when the statement ends. The same holds for a table-level
`PRIMARY KEY(c DESC)` on one INTEGER column (the alias test of the table-constraint form ignores the sort order; the
column-constraint form `c INTEGER PRIMARY KEY DESC` is *not* the alias - legacy behaviour of `sqlite3AddPrimaryKey`),
which the engine also had wrong for rowid tables in the WITHOUT ROWID case only. The engine now defers that index to
the end of the constraint list. Regression test: `c_align_v3_tests::automatic_index_names_and_schema_rows_follow_c`
(expectations produced by the reference shell).

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

## D10. ANALYZE of a single-column UNIQUE index that contains NULLs (FIXED in V3)
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
V3 fix (`crates/sqliters-core/src/engine.rs`, `compute_index_stat1_string[_limited]`, new argument `unique_single`):
C's `analyzeOneTable()` codes `OP_NotNull regPrev -> endDistinctTest` in front of the distinctness test of an index
with `nColTest==1 && pIdx->nKeyCol==1 && IsUniqueIndex(pIdx)`: once the previous key is non-NULL the comparison is
skipped and `regChng` keeps the value of the last test that ran, so the run of NULL keys at the start of the index
is the only place where "equal to the previous row" is counted. The engine now does the same for such an index (for
a WITHOUT ROWID table the condition is the index's own key count). Regression test:
`c_align_v3_tests::analyze_single_column_unique_index_with_nulls_follows_c` (expectations from the reference shell).

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

## D14. The engine's parser accepts a column definition after a table constraint
`CREATE TABLE t(a, UNIQUE(a), b);` -> C: `near "b": syntax error`; engine: creates the table.
(C's grammar: `columnlist conslist_opt` - constraints only after the last column.) Not fixed (engine parser).

## D15. PRAGMA index_list order of a WITHOUT ROWID table
```
CREATE TABLE t(a, b, c INTEGER, UNIQUE(a), PRIMARY KEY(c), UNIQUE(b)) WITHOUT ROWID; PRAGMA index_list(t);
```
C: `0|sqlite_autoindex_t_3|1|pk|0`, `1|sqlite_autoindex_t_2|1|u|0`, `2|sqlite_autoindex_t_1|1|u|0` (the order of
`Table.pIndex`: the PRIMARY KEY index was created last and is therefore first, the others newest first);
engine: `..._3 pk`, `..._1 u`, `..._2 u` (PRIMARY KEY first, then creation order). Names and root pages are equal
since the D1 fix; only the pragma's row order differs. Not fixed (pragma; the generator uses its own index list).

## D16. Conflicting ON CONFLICT clauses when one of them is an explicit ABORT
`CREATE TABLE t(a PRIMARY KEY ON CONFLICT ABORT, b, UNIQUE(a) ON CONFLICT REPLACE);`,
`CREATE TABLE t(a UNIQUE ON CONFLICT ABORT, b, UNIQUE(a) ON CONFLICT FAIL);`
-> C: `conflicting ON CONFLICT clauses specified`; engine: creates the table.
(C distinguishes "no ON CONFLICT clause" = OE_Default from an explicit ABORT; the engine's AST has `ConflictAction::Abort`
for both, see `schema.rs` `push_inline_auto`.) Not fixed (needs the parser to keep "explicit"). The generator's own
loader does report the error for such a stored CREATE TABLE text (the table is then "unsupported": C could never have
it in a schema). `gen_v3.py` drops statements the reference shell rejects from its corpora for this reason.

## D17. ALTER TABLE RENAME COLUMN that C refuses
`CREATE TABLE t1(a0, a4 INTEGER, UNIQUE(a0,a4), PRIMARY KEY(a4)); ALTER TABLE t1 RENAME COLUMN a4 TO rn1;`
-> C: `error in table t1 after rename: no such column: a4` (statement fails, nothing renamed); engine: renames.
(In C the UNIQUE index was built before `PRIMARY KEY(a4)` made a4 the rowid alias; the rename walker does not find the
token in that index's column list, the rewritten text then fails the `sqlite_rename_test` re-parse. With the PRIMARY
KEY first the rename works in both.) Not fixed (engine ALTER TABLE; arguably a C bug, but the C-identical rule applies).

## D18. STRICT type error message (FIXED in V3)
`CREATE TABLE t(a blob) STRICT; INSERT INTO t VALUES(1);` -> C: `cannot store INT value in BLOB column t.a`;
engine: `cannot store INTEGER value in blob column t.a` (the column type as typed; value type name: C's `vdbeMemTypeName()` says INT; column type:
C prints `sqlite3StdType[eCType-1]`, i.e. upper case). Fixed in `engine.rs` `check_strict_column_type`.

## D19. "no such table" of CREATE INDEX (FIXED in V3)
`CREATE INDEX i ON t1(a);` on a missing table -> C: `no such table: main.t1`; engine: `no such table: t1`.
`CREATE INDEX temp.i ON t1(a);` -> C: `no such table: t1`; engine: `no such table: temp.t1`.
`CREATE INDEX MAIN.i ON t1(a);` -> C: `no such table: main.t1` (the schema's own name, `aDb[iDb].zDbSName`).
(C: `sqlite3FixSrcList()` binds the table name of a non-TEMP index to the index's schema before
`sqlite3LocateTableItem()`.) Fixed in `engine.rs` `exec_create_index`.

## D20. The engine rejects an ON clause that names a table appearing later in the FROM clause (new in V3 session 4)
```
CREATE TABLE t102(i0); INSERT INTO t102 VALUES(1);
SELECT * FROM t102 AS a JOIN t102 AS b ON (c.i0=1) JOIN t102 AS c;
```
C: `1|1|1`; engine: `no such column: c.i0`. (C resolves ON clauses of inner joins against the whole FROM list; the
generator port does the same for EXPLAIN / EXPLAIN QUERY PLAN.) Cascade in all-kinds #7336: a CREATE TABLE AS SELECT
with such an ON clause fails in the engine, so later statements see a different schema. Not fixed (engine resolver).

## D21. The engine rejects an aggregate of an outer column in a sub-select's ORDER BY
`CREATE TABLE t1(a INT); CREATE TABLE t2(c INT, d INT); INSERT INTO t1 VALUES(1),(2); INSERT INTO t2 VALUES(3,4),(10,NULL);
SELECT (SELECT c FROM t2 ORDER BY sum(a)) FROM t1;` -> C: `3`; engine: `misuse of aggregate function sum()`.
(EXPLAIN / EXPLAIN QUERY PLAN of the statement are C's since the resolve.rs fix of all-kinds #14288.) Not fixed (engine).

## D22. The engine's parser does not take TRUE / FALSE as identifiers
`CREATE TABLE false(a);`, `CREATE TABLE t(true INT, false INT);` -> C creates the tables; engine: `near "false":
syntax error` / `near "true": syntax error`. (C's grammar: TRUE/FALSE are ordinary identifiers, `sqlite3ExprIdToTrueFalse`
turns an unresolved one into the boolean.) Not fixed (engine parser).

## Prepare-error differential of the all-kinds corpus (statements run as they are)
New tool `perr.py` (session 4): every all-kinds statement executed (no EXPLAIN prefix), the "Parse error" message and
its context lines compared byte-exactly. Final tree: see PROGRESS.md (r7a: 8 858 statements with an error in either
shell, 7 421 identical, 1 437 different). The differences are engine-scope classes, largest first (`perrsum.py`):
485 C accepts, engine "no such module" (D5 modules: fts3/fts4/echo/... - C has them), 246 C accepts, engine "no such
function" (D4), 168 same message but different / missing "error here" context (engine offset guess, D9), 131 C
accepts, engine "syntax error" (engine parser gaps: D8, D12, D14, D22, ...), 86 "no such module" vs "no such table"
(D5), 52 C "no such collation sequence" where the engine accepts, 42 + 38 "no such table" cascades, ...

## D23. `x IS TRUE` with a non-integer true value — DATA difference (engine executor)
`SELECT 0.5 IS TRUE, 0.5 IS NOT FALSE, 2 IS TRUE, 0.5 IS FALSE;` -> C: `1|1|1|0`; engine: `0|1|0|0`.
(C: `x IS TRUE` is TK_ISTRUE, `OP_IsTrue` = `sqlite3VdbeBooleanValue()`: any non-zero number is true; the engine
compares with the integer 1.) Found by firstdiv.py on all-kinds #14743-#14747. Not fixed (engine expression
evaluation; outside the generator) — a wrong-result defect, should be first in the engine queue.

## D24. PRAGMA query_only is not implemented by the engine
`CREATE TABLE t1(a); PRAGMA query_only; PRAGMA query_only=ON; PRAGMA query_only; DROP TABLE t1;` -> C: `0`, `1`,
`Error near line 4: attempt to write a readonly database`; engine: prints nothing for the two queries and drops the
table. Cascade: all-kinds #19656, #19661, #19663 ("no such table: t1" where C lists). Not fixed (engine pragma).

## D25. The engine's parser requires commas between table constraints
`CREATE TABLE t1(a,b,c, PRIMARY KEY(a) UNIQUE (a) CONSTRAINT one);` -> C creates t1 (+ sqlite_autoindex_t1_1)
(grammar `conslist ::= conslist tconscomma tcons`, `tconscomma ::= .`); engine: `near "UNIQUE": syntax error`.
Cascade: all-kinds #21023, #21028. Not fixed (engine parser).

## D26. Function checks of generated columns and DEFAULT at CREATE TABLE
`CREATE TABLE t1(a, b AS (f1(a+1)), c AS (f2(a+2)));` -> C: `no such function: f2` (error offset at f2); engine:
`no such function: f1`. `CREATE TEMP TABLE temp3(a, b DEFAULT(f3(31)));` -> C creates the table (the INSERT then fails
with `unknown function: f3()`); engine: `no such function: f3` at CREATE. Cascade: all-kinds #27533. Not fixed
(engine CREATE TABLE).

## Cascade attribution of the all-kinds "different" classes (session 4, firstdiv.py)
Tool `firstdiv.py <corpus> <bin> <k>...` (env NDIV=n): runs the statements of k's chunk before k in both shells and
lists the first n statements whose output differs. Attributions: #912 -> D13 (ATTACH with 8_3_names, then
`CREATE TABLE db2.t2`); #6573, #6584 -> D8; #6812, #6813, #6852 -> D4 (`decimal` collation of the reference shell);
#7289, #7336 -> D20; #14754, #14757, #14758 -> D22 (`CREATE TABLE t8(a INT, true INT, …)`); #19656-#19663 -> D24;
#21023, #21028 -> D25; #27533 -> D26; #1462 … (altertab chunks) -> D12; #1536 -> D12.

## D27. CHECK / generated-column expressions that C rejects only when the schema is re-parsed
`CREATE TABLE t_gen(x INT, y AS (RAISE(IGNORE)));` -> C: `Error near line 1: RAISE() may only be used within a
trigger-program` (a run-time error: the statement fails in OP_ParseSchema, is rolled back, and the rollback resets the
schemas: iGeneration+1); engine: `Parse error near line 1: …` (prepare error, no reset). Same for
`ALTER TABLE t1 ADD COLUMN d INT CHECK(RAISE(ABORT, 'add_chk'))` (C: iGeneration +2 after it). Cascade: the P4
(generation) of OP_Transaction in later listings: all-kinds #38895 (6 statements), #37422 … (12 statements). Not
fixed (engine CREATE TABLE / ALTER TABLE; the generator mirrors only resets the engine reports).

## D28. `PRAGMA temp_store=…` inside a transaction with an open temp database: error phase
`CREATE TEMP TABLE x(a); BEGIN; PRAGMA temp_store=2;` -> C: `Parse error near line 1: temporary storage cannot be
changed from within a transaction` (pragma.c: compile time); engine: `Error near line 1: …` (run time). Not fixed.
(Session 4 made the engine open the temp database where C does for `PRAGMA temp.*` and for `PRAGMA integrity_check` /
`quick_check` without a schema — pragma.c:457, build.c:5447 — so the reset of `PRAGMA temp_store` that follows them
is now C's: all-kinds #14659.)

## D29. Schema cookie of the temp database after ALTER TABLE RENAME / RENAME COLUMN
`… ALTER TABLE x1 RENAME two TO 'four';` and `ALTER TABLE txx RENAME TO "t xx";` (with a temp schema): the engine
increments the temp database's schema cookie where C does not (C writes temp only when a temp trigger / view refers
to the renamed object). `CREATE TRIGGER tr1 AFTER DELETE ON t4 BEGIN UPDATE main.t1 SET a=1, b=2; END;` on a temp
table t4: C accepts (cookie +1), engine: "qualified table names are not allowed on INSERT, UPDATE, and DELETE
statements within triggers". Cascade: P3 (cookie) of `OP_Transaction 1` in later listings: all-kinds #1751, #1781,
#8045, #8054, #21016. Not fixed (engine ALTER TABLE / CREATE TRIGGER).

## Not a difference: random data
all-kinds #24135, #24137, #24170 (root pages of a temp table filled with 100 000 `randomblob()` rows; three runs of
the reference shell give root pages 11339, 11320, 11336), #37448 / #37559 prefixes (`randomblob` / `sha3` output).
