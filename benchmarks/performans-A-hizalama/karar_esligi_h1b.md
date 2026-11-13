# H1b — decision parity: how DML statements are driven (C SQLite 3.54.1 vs the engine)

Sources: `sqlite/src/{delete,update,insert,fkey,build,where,vdbe,vdbeaux,expr,trigger,upsert}.c` (line numbers of the tree in this
repo). Engine: `crates/sqliters-core/src/dml_c.rs` (new: the decisions C takes at code generation), `engine.rs`, `lib.rs`,
`compiler.rs`. Trace: C `h1b/ctrace/patch_h1b.py`, Rust `dtrace!` (`--cfg sqliters_dtrace`), differ `h1b/trstmt.py`.

## a. Decision tables (from the C source)

### A1. DELETE — pass strategy (`sqlite3DeleteFrom`, delete.c)

| # | condition | outcome | source | engine |
|---|---|---|---|---|
| D1 | `bComplex = pTrigger (incl. RETURNING) \|\| sqlite3FkRequired(pTab,0,0)`; later `\|\| NC_Subquery` in WHERE | disables truncate and `WHERE_ONEPASS_MULTIROW` | delete.c:358, 497-498 | `prepare_delete_exec`: `b_complex`; `dml_c::fk_required_delete` (was: "foreign_keys pragma on") |
| D2 | `rcauth==SQLITE_OK && pWhere==0 && !bComplex && !IsVirtual` | truncate: `OP_Clear` table then each index in `pIndex` order | delete.c:471-492 | same condition (INDEXED BY no longer disables it; FK pragma alone no longer disables it) |
| D3 | loop is `WHERE_ONEROW` (rowid `=`, or every column of a UNIQUE index `=`; `whereShortCut` where.c:6362-6417, `whereLoopAddBtreeIndex` where.c:3424) | `ONEPASS_SINGLE`; `isMultiWrite` not set by delete.c:532 | delete.c:528, where.c (`sqlite3WhereOkOnePass`) | `dml_c::scan_info` on the access plan + `where_names_one_rowid` (rowid / single-column UNIQUE `=` sub-select, single-value IN) |
| D4 | not one row, `!bComplex`, not virtual | `ONEPASS_MULTI`: rows deleted in scan order while the cursor is on them | delete.c:498, 528 | rows collected in scan order, then deleted in that order (no sub-query can observe the difference) |
| D5 | otherwise, rowid table | two-pass: `OP_RowSetAdd`, `OP_RowSetRead` returns the rowids ascending, duplicates dropped | delete.c:582, 626 | `sort_table_keys_in_tree_order` (was: scan order) |
| D6 | otherwise, WITHOUT ROWID | two-pass: ephemeral index with the PRIMARY KEY's KeyInfo → PK order | delete.c:508-515, 573-577, 617-624 | same sort with the key's collations / DESC |
| D7 | virtual table | never MULTI; `sqlite3MayAbort`; one-row top-level delete clears `isMultiWrite` | delete.c:633-643 | vtab loop unchanged; journal kept (analysis reports both flags) — not aligned |

### A2. UPDATE — pass strategy (`sqlite3Update`, update.c)

| # | condition | outcome | source | engine |
|---|---|---|---|---|
| U1 | `chngRowid` / `chngPk`: SET names the rowid / a PRIMARY KEY column of a WITHOUT ROWID table | `chngKey`; row deleted and re-inserted, all indexes rewritten; `OP_MustBeInt` → mayAbort | update.c:480-538, 906, 1088-1100 | static for an INTEGER PRIMARY KEY column; an assignment to the implicit `rowid` name is seen only when the value changes |
| U2 | `hasFK = sqlite3FkRequired(pTab,aXRef,chngKey)`: 0 / 1 / 2 (2: self-referencing child key, or parent key with an ON UPDATE action) | 2 forces delete + insert and all indexes | update.c:569, fkey.c `sqlite3FkRequired` | `dml_c::fk_required_update`, wired into `enter_update_row_storage_rules` (was: always "not 2") |
| U3 | `bReplace`: OR REPLACE, or (no OR clause and) a rewritten index declared ON CONFLICT REPLACE | no MULTIROW; all index cursors opened | update.c:577-592 | `prepare_update_exec` |
| U4 | `!nested && !pTrigger && !hasFK && !chngKey && !bReplace && no sub-query in WHERE` | `WHERE_ONEPASS_MULTIROW` | update.c:744-752 | same |
| U5 | one-row loop | `ONEPASS_SINGLE` (no `sqlite3MultiWrite` at update.c:769) | update.c:766-769 | as D3 |
| U6 | MULTI but the scan index is rewritten (`aToOpen[iCur]`), or `WHERE_MULTI_OR` | `ONEPASS_OFF` | update.c:770-775, where.c (`WHERE_DUPLICATES_OK` not passed) | `dml_c::update_one_pass` |
| U7 | `ONEPASS_OFF`, rowid table | ephemeral table keyed by rowid, read back in rowid order | update.c:698-701, 783-785, 845-851 | keys sorted |
| U8 | `ONEPASS_OFF`, WITHOUT ROWID | ephemeral index, PK order | update.c:703-714, 800-803 | keys sorted |
| U9 | `UPDATE … FROM` | `sqlite3MultiWrite`, `ONEPASS_OFF`, ephemeral keyed by rowid/PK (last matching source row wins) | update.c:715-719, `updateFromSelect` | pre-existing ordered ephemeral; which source row is last depends on the join plan (see remaining differences) |
| U10 | sub-select in a SET expression that does not refer to the row | coded with `OP_Once`: evaluated once per statement execution | expr.c `sqlite3CodeSubselect`, vdbe.c:2778 | `stmt_tx.once_subqueries` for provably self-contained scalar sub-selects (table and view UPDATE loops) |
| U11 | UPDATE of a view | NEW row gets the view's column affinities before the INSTEAD OF triggers (`tmask&TRIGGER_BEFORE`) | update.c:991-993 | `apply_view_col_affinities` |

### A3. INSERT — source handling and rowid (`sqlite3Insert`, insert.c)

| # | condition | outcome | source | engine |
|---|---|---|---|---|
| I1 | one-row VALUES | not a SELECT: `pSelect = 0` | insert.c (`SF_Values`, `pPrior==0`) | analysis: `is_select = false` |
| I2 | `pColumn==0 && xferOptimization(..)` | transfer optimisation | insert.c:1024-1027 | pre-existing `check_insert_xfer_eligible` (conditions not re-audited in this step) |
| I3 | SELECT source and (`pTrigger` or `readsTable`) | `useTempTable = 1`: all source rows first | insert.c:1161-1162 | the engine always materialises the source first |
| I4 | SELECT source otherwise | co-routine: a row is inserted before the next is computed | insert.c:1147-1215 | NOT aligned: materialised first (only visible through `last_insert_rowid()` after an error raised while a later row is computed) |
| I5 | rowid supplied (`ipkColumn>=0`) | `OP_MustBeInt` → mayAbort; literal NULL of a one-row VALUES → `OP_NewRowid` directly (`appendFlag`) unless a BEFORE trigger exists | insert.c:1451-1461, 1498-1536 | analysis `ipk_given` / `literal_null` |
| I6 | AUTOINCREMENT | counter per table and top-level statement, read at statement start; `OP_MemMax` right after the rowid is known (before any constraint); `OP_NewRowid` P3: `max(table max, counter)+1`, FULL at MAX; `sqlite3AutoincrementEnd` at the end of the top-level statement writes when the counter grew — or when the table has no `sqlite_sequence` row yet (NULL comparison), even if nothing was inserted | insert.c `autoIncBegin` / `autoIncStep` (1538) / `sqlite3AutoincrementEnd`, vdbe.c:5821 | `stmt_tx.autoinc`, `autoinc_counter` / `autoinc_note_rowid` / `autoinc_end`, `stmt_tx.autoinc_coded` (was: `sqlite_sequence` rewritten after every written row, rejected rows not counted) |
| I7 | `last_insert_rowid` | set by `OP_Insert` before `sqlite3BtreeInsert`; never restored on failure; saved/restored around trigger programs | vdbe.c:5952 | set before the table insert; not restored in `abort_write_statement` |
| I8 | INSERT on a view | no affinity on NEW (`!isView`), but `old.x`/`new.x` of a REAL column get `OP_RealAffinity`; `old.x`/`new.x` have no affinity in comparisons (TK_TRIGGER) | insert.c (BEFORE block), expr.c:49, 5730 | `make_runtime_*_pseudo_scope` |
| I9 | one-row top-level INSERT, `isMultiWrite==0`, immediate FK whose parent is missing | `sqlite3HaltConstraint(OE_Abort)` before the row is written | fkey.c:442-451 | `stmt_tx.insert_fk_halts_immediately`, `check_insert_fk_immediately` |

### B1. Constraint check order (`sqlite3GenerateConstraintChecks`, insert.c:1886-2767)

| # | step | rule | source | engine |
|---|---|---|---|---|
| C1 | NOT NULL | column order; UPDATE: changed columns only; REPLACE with a DEFAULT substitutes it and a 2nd pass then checks with ABORT | 1941-2054 | `validate_and_write_row` step 3 (all columns checked on UPDATE: not aligned for unchanged columns) |
| C2 | STRICT `OP_TypeCheck` | not a constraint (always ABORT, no OR clause); runs where `sqlite3TableAffinity` is first coded: with generated columns before C1, else before the first CHECK, else before the first index of the index loop (after the rowid check), else when the record is built | insert.c:170-200, 2075-2078, 2424-2427, 2745-2752 | `strict_pending` in `validate_and_write_row` (was: before C1 and ignorable by OR IGNORE) |
| C3 | CHECK | declaration order; UPDATE: only CHECKs that mention a changed column; REPLACE → ABORT | 2059-2099 | step 4 (all CHECKs on UPDATE: not aligned) |
| C4 | rowid | before the indexes, except: REPLACE from the column's own clause with other indexes present → deferred to the end (`ipkTop`) | 2238-2362 | `defer_ipk_replace` (pre-existing) |
| C5 | UNIQUE / PRIMARY KEY indexes | `pTab->pIndex` order: newest first, ON CONFLICT REPLACE ones last; the PK of a WITHOUT ROWID table at its own position; UPSERT targets first | 2104-2160, 2366-2702 | `ordered_table_indexes` (fixed: input order independent, WR PK numbered like C — the order was reversed for ≥2 CREATE INDEX indexes and the WR PK was checked first) |
| C6 | conflict test | after `OP_NoConflict` only an UPDATE exempts the row being updated; a row that sits at the new key is a conflict | 2528-2572 | exemption "same target key" removed |
| C7 | REPLACE delete | rowid conflict: `sqlite3GenerateRowDelete` only with DELETE triggers (recursive) or FK, else index entries only; index conflict: `sqlite3GenerateRowDelete(.., iThisCur)` — conflicting index entry last | 2322-2345, 2613-2640 | `replace_conflict_delete_row` (`conflict_index_root`) |
| C8 | recheck | after REPLACE with triggers / FK actions: every uniqueness test again with ABORT | 2641-2731 | `recheck_uniqueness_abort` (pre-existing) |
| C9 | generated columns | computed in dependency order, repeated passes in column order | `sqlite3ComputeGeneratedColumns` | `generated_col_eval_order` (was: column order — index entries of forward-referencing generated columns were wrong, C integrity_check failed) |

### B2. Per-row write order

| # | statement | order | source | engine |
|---|---|---|---|---|
| W1 | DELETE | index entries in `pIndex` order except `iIdxNoSeek`, table row, then the `iIdxNoSeek` entry (the scan index of a one-pass delete; the conflicting index of a REPLACE) | delete.c:840-860 | `remove_row_and_indexes_opt` + `stmt_tx.delete_idx_no_seek` |
| W2 | INSERT | index entries in `pIndex` order, table row last (`sqlite3CompleteInsertion`); WR PK at its list position | insert.c `sqlite3CompleteInsertion` | pre-existing (5g) |
| W3 | UPDATE | old entries of the rewritten indexes deleted (`pIndex` order), table row deleted only if `hasFK>1 \|\| chngKey`, new entries inserted, table row written | update.c:1065, 1088-1100, 1109 | rowid tables: as C (now also `hasFK>1`); WITHOUT ROWID tables: every index entry deleted and re-inserted — not aligned |
| W4 | REPLACE on rowid | old row overwritten (index entries only deleted) unless triggers / FK | insert.c:2333-2345 | pre-existing (H1) |
| W5 | AUTOINCREMENT | `sqlite_sequence` written once, at the end of the top-level statement | insert.c `autoIncrementEnd` | `autoinc_end` |

### C. Statement journal (`usesStmtJournal = isMultiWrite && mayAbort`, vdbeaux.c:2709; opened in `OP_Transaction` when `autoCommit==0 || nVdbeRead>1`, vdbe.c:4295)

`sqlite3BeginWriteOperation(pParse, setStatement, iDb)` / `sqlite3MultiWrite` — sets `isMultiWrite`:

| site | condition | engine (`dml_c::Analyzer`) |
|---|---|---|
| delete.c:422 | `bComplex` (triggers incl. RETURNING, FK) | `delete()` |
| delete.c:532 | `eOnePass != ONEPASS_SINGLE` | `delete()` |
| delete.c:641 | virtual table, one-row, top-level → `isMultiWrite = 0` | not modelled (flags forced on) |
| insert.c:1012 | `pSelect \|\| pTrigger` (multi-row VALUES is a SELECT) | `insert()` |
| insert.c:2354 | rowid REPLACE with `regTrigCnt` | `constraint_checks()` |
| insert.c:2370 | rowid REPLACE, table has indexes | `constraint_checks()` |
| insert.c:2623 | index REPLACE with `regTrigCnt` | `constraint_checks()` |
| update.c:609 | `pTrigger \|\| hasFK` | `update()` |
| update.c:716 | `UPDATE … FROM` | `update()` |
| update.c:769 | `eOnePass != ONEPASS_SINGLE` | `update()` |
| update.c:1331 | virtual table | not modelled (flags forced on) |
| build.c:1393, 3475, 3669, 3897, 4512, 4729; analyze.c:1402/1433 (0); pragma.c:578/859/2596 (0); trigger.c:394 (0); build.c:5714 (0); where.c:4655 (0) | DDL / ANALYZE / PRAGMA / CREATE TRIGGER / REINDEX / automatic index | NOT aligned: every non-DML write statement inside a transaction still opens a statement savepoint |

`sqlite3MayAbort` — sets `mayAbort`:

| site | condition | engine |
|---|---|---|
| vdbeaux.c:482 | every coded scalar SQL function (`sqlite3VdbeAddFunctionCall`); inline functions (coalesce, ifnull, iif, if, likely, unlikely, likelihood, sqlite_offset) and aggregates are not calls; LIKE/GLOB/REGEXP/MATCH, `->`/`->>`, CURRENT_* are | `collect_node` |
| build.c:5542 | `sqlite3HaltConstraint(.., OE_Abort, ..)`: NOT NULL, CHECK (REPLACE → ABORT), rowid, UNIQUE, the recheck after REPLACE triggers | `constraint_checks()` |
| insert.c:2015 | NOT NULL with ABORT | same |
| insert.c:1461, 1530 | `OP_MustBeInt` on a supplied rowid | `insert()` |
| insert.c:1559; delete.c:637; update.c:1362; vtab.c:472 | virtual table writes | flags forced on |
| update.c:906 | `chngRowid` | `update()` |
| upsert.c:311 | UPSERT DO UPDATE on a WITHOUT ROWID table | `insert()` |
| fkey.c:442-455 | immediate FK, new child row (halt or `OP_FkCounter`) | `fk_check_insert` / `fk_check_new` (skipped for the SET NULL action's own UPDATE: `isSetNullAction`, fkey.c:855) |
| fkey.c:1078-1080 | parent row removed / changed, immediate FK whose action is not CASCADE / SET NULL | `fk_check_old` |
| expr.c:5878 | `RAISE(ABORT, ..)` (also the RESTRICT action program) | `collect_node`, `fk_actions` |
| select.c:2616, 2624 | LIMIT / OFFSET that is not an integer constant | `select_limits` |
| window.c:1523 | window frame bound check | any window function (conservative) |
| alter.c:208/540/678; build.c:2935/3370/3529/3926; vdbeaux.c:575 (`OP_ParseSchema`) | DDL | not aligned (see above) |
| — | `OP_TypeCheck` (STRICT), `OP_Concat` / `OP_MakeRecord` TOOBIG | no `sqlite3MayAbort` in C: such a statement aborts without a statement journal |

Halt (`sqlite3VdbeHalt`, vdbeaux.c:3344-3510):

| # | condition | outcome | engine (`abort_write_statement`) |
|---|---|---|---|
| H1 | IOERR, INTERRUPT (write statement); NOMEM, FULL without statement journal | whole transaction rolled back, autocommit on | `effective_effect = Rollback` (was: FULL always, INTERRUPT statement-only) |
| H2 | NOMEM, FULL with statement journal | statement rolled back | `Abort` with savepoint |
| H3 | autocommit, only writer: OK or OE_Fail | commit (deferred FK check first) | pre-existing |
| H4 | autocommit, only writer: other error | rollback | pre-existing |
| H5 | in transaction: OE_Fail | statement released: rows so far stay, `changes()` = rows so far | pre-existing |
| H6 | in transaction: OE_Abort, statement journal open | statement rolled back, `changes()` = 0, deferred-FK counter restored | pre-existing |
| H7 | in transaction: OE_Abort, no statement journal (`p->iStatement==0`) | nothing rolled back: what the statement wrote stays; `changes()` = 0; deferred-FK counter not restored | `ELIDED_STMT_SP` (new for the general path) |
| H8 | in transaction: OE_Rollback | transaction rolled back | pre-existing |
| H9 | counters | `changes()` as above; `total_changes()` keeps the changes of completed trigger steps even when the statement fails; `last_insert_rowid()` never restored | fixed |

## b. Decision trace (same starting file, statement by statement)

Tool: `h1b/trstmt.py FIRST N`. Per seed of the DML fuzz corpus (`h1b/dmlgen.py`) the schema and seed rows are written by the C
reference shell; the file is copied and the remaining statements run on one copy by the instrumented C shell
(`h1b/ctrace/sqlite3_tr`) and on the other by the trace build of the Rust CLI. A statement is compared until the first statement
whose SQL output or error differs (after that the engines work on different data).

Final build, seeds 1..600: **6663 top-level DML statements in C, 6487 compared**, 176 not compared.

| decision | compared | mismatches |
|---|---|---|
| S1 `usesStmtJournal` (and `isMultiWrite` / `mayAbort` where the Rust path reports them) | 6487 | 9 |
| S2 statement journal opened | 6487 | 0 |
| S3 pass strategy of DELETE / UPDATE (general path; the compiled point plans do not report it) | 6487 | 28 |
| S4 halt outcome of a failed statement | 6487 | 0 |
| W b-tree write sequence of the statement (tree root, insert/delete, rowid or record length) | 6487 | 303 (equal: 6184) |

Remaining decision mismatches: 37 statements (S3 28, S1 9) in 29 seeds. By the first mismatch of each seed:

* S3, 22 seeds "C two-pass, engine one-pass multi" (+ 1 the other way, + 1 DELETE "C two-pass rowset, engine one-pass single"):
  the scan differs. C runs the loop of a WITHOUT ROWID table through the cheapest index when the statement has no usable WHERE
  term (`colUsed == 0`, where.c:4236-4253) and uses an index range for `x IS NOT NULL`; when that index is rewritten the UPDATE
  falls back to two passes (update.c:770-775). The engine's access planner scans the table instead. Cause: scan choice
  (planner), not the strategy rule. SQL-visible when the row order decides which UNIQUE conflict is hit first.
* S1, 5 seeds: analysis gaps — 4× the engine computes `mayAbort = 0` where C has 1 (e.g. `DELETE FROM t2 WHERE id>5` with a
  BEFORE DELETE trigger tree; cause not identified), 1× one-row detection (`WHERE a=NULL`). Where C has a journal and the
  engine does not, an abort would leave rows C rolls back — not observed in the fuzz, not excluded.
* S2 and S4: 0.

Statements whose b-tree write sequence differs: 303 (of 6487) in 179 seeds. By the first differing statement of each seed
(`h1b/tr_final.json`):

| statement | table | kind of difference | seeds | cause |
|---|---|---|---|---|
| UPDATE | WITHOUT ROWID | different events | 83 | pre-existing: the engine deletes and re-inserts the row and every index entry; C rewrites only the indexes whose columns change and overwrites the PRIMARY KEY record (update.c:1065-1109). Not done in this step |
| UPDATE | rowid table | different events | 22 | not classified individually (sample: an index C does not rewrite — `aRegIdx` — is rewritten, or the reverse, for expression / partial indexes and generated columns) |
| UPDATE / DELETE / INSERT / REPLACE | through a view | — | 22 | INSTEAD OF trigger bodies; row order of the materialised view (planner / flattening) |
| DELETE | WITHOUT ROWID / rowid | same events, other order | 20 | one-pass scan through a different index than C (planner) |
| UPSERT / REPLACE / INSERT | any | — | 25 | UPSERT executor (`execute_upsert_table_insert_row`) does not yet delete the conflicting index entry last; REPLACE with triggers; not classified individually |
| other | | | 7 | follows a decision mismatch in the same script |

None of the first differing events is a page split or allocation: the `w` class records row-level inserts and deletes only, so
nothing here is attributed to H2 (insert-side splitting); H2 shows in the tree-shape gates (finder layers, `recover.py` mode own).
The per-row classification above is by statement kind; only the first group was traced to its cause in the source.

Older traces (must not regress): step 5g class `w` over 234 scripts: identical **181** (5g: 170); H1 delete-rebalance trace over
95 scenarios: **95 fully equal** (H1: 93 — the two open ones were the scan-index delete order and the two-pass row order, both
closed here).

Final source (`h1b/bin_r9`, re-run after the last performance edits): the same numbers — 6663 / 6487 statements, S1 9, S2 0,
S3 28, S4 0, write sequence equal 6184 / different 303 (`h1b/tr_r9.{txt,json}`); 5g class `w` 181 / 234; H1 trace 95 / 95.
The compiled point programs (compiler.rs) no longer run the analysis where `isMultiWrite` is 0 by construction (one-row VALUES
INSERT with no REPLACE site; rowid-seek DELETE; rowid-seek UPDATE unless the rowid is declared ON CONFLICT REPLACE); a
`debug_assert!` compares the shortcut with the full analysis in every debug build (cargo test, the API fuzz driver).

## c. Coverage (decision | outcome : times taken in C / in Rust, seeds 1..600)

| decision | outcome | C | Rust | |
|---|---|---|---|---|
| S1 isMultiWrite,mayAbort | 0,0 | 284 | 283 | counts differ: see b |
| S1 isMultiWrite,mayAbort | 0,1 | 515 | 511 | counts differ: see b |
| S1 isMultiWrite,mayAbort | 1,0 | 2056 | 2063 | counts differ: see b |
| S1 isMultiWrite,mayAbort | 1,1 | 3808 | 3795 | counts differ: see b |
| S1 usesStmtJournal | 0 | 2855 | 2862 | counts differ: see b |
| S1 usesStmtJournal | 1 | 3808 | 3801 | counts differ: see b |
| S2 statement journal opened | no | 4858 | 4864 | counts differ: see b |
| S2 statement journal opened | yes | 1805 | 1799 | counts differ: see b |
| S3 strategy | delete onepass-multi | 411 | 411 |  |
| S3 strategy | delete onepass-single | 133 | 133 |  |
| S3 strategy | delete truncate | 88 | 88 |  |
| S3 strategy | delete two-pass-ephemeral | 210 | 208 | counts differ: see b |
| S3 strategy | delete two-pass-rowset | 744 | 739 | counts differ: see b |
| S3 strategy | update from-ephemeral | 129 | 129 |  |
| S3 strategy | update onepass-multi | 486 | 512 | counts differ: see b |
| S3 strategy | update onepass-single | 125 | 124 | counts differ: see b |
| S3 strategy | update two-pass | 1259 | 1230 | counts differ: see b |
| S4 halt | auto-commit-fail rc=19 | 84 | 83 | counts differ: see b |
| S4 halt | auto-rollback rc=1 | 10 | 10 |  |
| S4 halt | auto-rollback rc=19 | 732 | 730 | counts differ: see b |
| S4 halt | auto-rollback rc=20 | 8 | 8 |  |
| S4 halt | fail-keep rc=19 | 79 | 79 |  |
| S4 halt | nothing rc=1 | 1 | 1 |  |
| S4 halt | nothing rc=19 | 89 | 89 |  |
| S4 halt | stmt-rollback rc=1 | 3 | 3 |  |
| S4 halt | stmt-rollback rc=19 | 492 | 493 | counts differ: see b |
| S4 halt | stmt-rollback rc=20 | 10 | 10 |  |
| S4 halt | txn-rollback rc=19 | 67 | 67 |  |

Not exercised by the corpus (not verified): virtual-table DML (D7, update.c:1331/1362), `SQLITE_NOMEM` / `SQLITE_IOERR` /
`SQLITE_INTERRUPT` halts (H1), a statement journal opened because of another active statement in autocommit (`nVdbeRead>1`;
the engine still keys this on other *writers*), `PRAGMA defer_foreign_keys`, the transfer optimisation conditions (I2),
`RAISE(FAIL)` / `RAISE(ROLLBACK)` inside nested trigger levels beyond two, TEMP triggers on main tables, attached databases.
Statement-journal flags of DDL / ANALYZE / PRAGMA statements are not traced and not aligned.
