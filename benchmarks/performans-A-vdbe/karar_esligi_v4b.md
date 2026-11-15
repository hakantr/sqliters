# V4b decision parity (karar eşliği): virtual tables and table-valued functions in the code generator

Scope: step V4b of the port of C SQLite 3.54.1's code generator: the read side of vtab.c the generator needs (CREATE
VIRTUAL TABLE at schema load, sqlite3VtabCallConnect / vtabCallConstructor / sqlite3_declare_vtab with HIDDEN columns,
eponymous tables, sqlite3GetVTable, sqlite3VtabOverloadFunction, sqlite3VtabMakeWritable), table-valued-function
syntax (sqlite3SrcListFuncArgs, sqlite3WhereTabFuncArgs, ON-clause rule for function arguments), whereexpr.c's
virtual-table parts (isAuxiliaryVtabOperator, WO_AUX terms, sqlite3WhereAddLimit), where.c's allocateIndexInfo /
whereLoopAddVirtualOne / whereLoopAddVirtual / sqlite3_vtab_in / _rhs_value / _distinct / _collation, wherecode.c's
virtual-table loop (OP_VOpen / VFilter / VColumn / VNext / VInitIn) and its EQP text. Modules are reached through the
Host interface (PROGRAM_API_CHANGES.md); the engine answers xBestIndex with ports of C's methods. Oracle:
`target/perf-work/bin/sqlite3` and the amalgamation `target/sqlite-ref/amalg` (instrumented copies in `v4b/wdump/`
and `v4b/cov/`, scratch only; the shell's generate_series = ext/misc/series.c registered in both). Worktree
`.claude/worktrees/vdbe` (branch `performans-A-vdbe`, uncommitted on top of ba2a3d9, Rust 1.100). Binary of every
number below: `$S/v4b_bin/sqliters.r3` (= the final `$S/v4b_bin/sqliters`).
The engine's executor and fast paths still execute every statement; the generator's program is what EXPLAIN / EQP
print. The VDBE machine has no virtual-table opcodes yet: under SQLITERS_VDBE_EXEC=1 such programs fall back to the
engine (M-step work, recorded).

## 0. How a decision is proven: the layers

EXPLAIN / EQP: statements / accepted by the generator / identical / different (`v4b/expdiff.py`: the P4_VTAB address
`vtab:%p` is replaced by `vtab:P` on both sides, nothing else). Dump layers (`v4b/wdiff.py`): identical / statements with a
dump; `bidx` = every xBestIndex call of the statement, inputs (`BI`: aConstraint iColumn:op:usable:iTermOffset, aOrderBy,
colUsed, eDistinct, mIn) and outputs (`BO`: rc, idxNum, idxStr, estimatedCost bits, estimatedRows, orderByConsumed,
idxFlags, mHandleIn, aConstraintUsage argvIndex:omit) - C `v4bDumpBI/BO` of `wdump/patch_v4b.py` in vtabBestIndex(), port
`where_.rs dump_index_info_inputs/outputs`; the loops layer prints the u.vtab fields of a vtab loop and the terms layer
the eMatchOp of WO_AUX terms (TERMAUX). Schema layer: identical schemas / schemas (`sdiff.py`).

| corpus | EXPLAIN | EQP | terms | loops | paths | agg | win | bidx | schema |
|---|---|---|---|---|---|---|---|---|---|
| vt_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 9857 / 9857 | 9857 / 9857 | 9857 / 9857 | 2074 / 2074 | 0 / 0 | 9857 / 9857 | 300 / 300 |
| vt_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 4939 / 4939 | 4939 / 4939 | 4939 / 4939 | 965 / 965 | 0 / 0 | 4939 / 4939 | 150 / 150 |
| vt_t | 2400 / 2400 / 2400 / 0 | 2400 / 2400 / 2400 / 0 | 1978 / 1978 | 1978 / 1978 | 1978 / 1978 | 401 / 401 | 0 / 0 | 1978 / 1978 | 60 / 60 |
| vt_x | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2659 / 2659 | 2659 / 2659 | 2659 / 2659 | 519 / 519 | 0 / 0 | 2659 / 2659 | 100 / 100 |
| vt_e | 55 / 55 / 55 / 0 | 55 / 55 / 55 / 0 | 55 / 55 | 55 / 55 | 55 / 55 | 6 / 6 | 0 / 0 | 55 / 55 | 1 / 1 |
| vt_d | 187 / 187 / 185 / 2 | 187 / 187 / 185 / 2 | 164 / 164 | 164 / 164 | 164 / 164 | 8 / 8 | 0 / 0 | 161 / 161 | 2 / 2 |
| win_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 8261 / 8261 | 8261 / 8261 | 8261 / 8261 | 926 / 926 | 8231 / 8231 | 0 / 0 | 300 / 300 |
| win_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 4147 / 4147 | 4147 / 4147 | 4147 / 4147 | 508 / 508 | 4131 / 4131 | 0 / 0 | 150 / 150 |
| win_c | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 4526 / 4526 | 4526 / 4526 | 4526 / 4526 | 818 / 818 | 4460 / 4460 | 0 / 0 | 150 / 150 |
| win_d | 12 / 12 / 12 / 0 | 12 / 12 / 12 / 0 | 11 / 11 | 11 / 11 | 11 / 11 | 6 / 6 | 11 / 11 | 0 / 0 | 1 / 1 |
| ddl_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10523 / 10523 | 10306 / 10306 | 10323 / 10323 | 2090 / 2090 | 0 / 0 | 0 / 0 | 400 / 400 |
| plan_a | 26000 / 26000 / 26000 / 0 | 26000 / 26000 / 26000 / 0 | 23583 / 23583 | 23196 / 23196 | 23246 / 23246 | 6054 / 6054 | 0 / 0 | 0 / 0 | 520 / 520 |
| plan_b | 13000 / 13000 / 13000 / 0 | 13000 / 13000 / 13000 / 0 | 11884 / 11884 | 11698 / 11698 | 11718 / 11718 | 3088 / 3088 | 0 / 0 | 0 / 0 | 260 / 260 |
| agg_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10531 / 10531 | 10337 / 10337 | 10342 / 10342 | 10048 / 10048 | 0 / 0 | 0 / 0 | 300 / 300 |
| agg_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 5259 / 5259 | 5168 / 5168 | 5172 / 5172 | 4999 / 4999 | 0 / 0 | 0 / 0 | 150 / 150 |
| agg_c | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2661 / 2661 | 2613 / 2613 | 2615 / 2615 | 2558 / 2558 | 0 / 0 | 0 / 0 | 100 / 100 |
| cte_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 9045 / 9045 | 8965 / 8965 | 8977 / 8977 | 1804 / 1804 | 0 / 0 | 0 / 0 | 300 / 300 |
| cte_b | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2280 / 2280 | 2261 / 2261 | 2264 / 2264 | 395 / 395 | 0 / 0 | 0 / 0 | 100 / 100 |

Virtual-table corpora (reproducible, `v4b/gen_vtab.py <seed> <blocks> <stmts>`): vt_a = 8301 300 40, vt_b = 8402 150 40,
vt_t = 8201 60 40, exec corpus vt_x = 8503 100 30 --exec (bounded series, LIMIT 300; also run for result rows); hand-written
statements (vt_e: targeted statements for branch outcomes the generated corpora missed - ORDER BY with COLLATE /
constants, vector IN and vector ranges, LIMIT/OFFSET in compounds and correlated ORDER BY, like/glob/match in function
form, overloaded fts5 functions, WO_AUX terms of outer joins, three-table plans with IN; vt_d: eponymous names in other case / with a schema prefix, views over vtabs and trusted_schema, OR terms, ORDER
BY / LIMIT / DISTINCT / GROUP BY consumption, joins of every kind, subqueries, CTEs, compound selects, fts5 / rtree /
fts5vocab / json / pragma / dbstat constraints, errors, temp and attached virtual tables, a re-attached database whose
virtual tables are connected by the statement). Each block of the generated corpora: ordinary tables with rows and
indexes, an fts5 table (unicode61 with random options) and a trigram one, fts5vocab row / col / instance tables, rtree
and rtree_i32 tables with rows, two views over virtual tables, sometimes ANALYZE. Win / V3 corpora as in V4a / V3.

| other layer | tool | result |
|---|---|---|
| result rows (SQLITERS_VDBE_EXEC=1), vt_x | `vt_x_rows_r3.txt` | statements 3000 ; machine   identical      0   different 0 ; fallback  identical   2519   different 187 ; none      identical    253   different 41 |
| all statement kinds, EXPLAIN (statements executed in between) | `all_kinds_r3.txt` | statements 39300 · generator accepted 14773: identical 14406, DIFFERENT 367 · unsupported (fallback) 24125 (of which same as C anyway 4588) · not reached (old parser/other) 402 (same 342) |
| same, EXPLAIN QUERY PLAN | `all_kinds_eqp_r3.txt` | statements 39300 · generator accepted 14773: identical 14495, DIFFERENT 278 · unsupported (fallback) 24125 (of which same as C anyway 21571) · not reached (old parser/other) 402 (same 342) |
| attribution of the vt_x row differences | `vt_x_rows_r3.json`, `rowcls.py` | 228 statements, all on the engine's executor (the machine has no virtual-table opcodes: every vtab program falls back); the same 228 statements with byte-identical port output with the V4a binary (`vt_x_rows_v4a.json`, same as r2): pre-existing engine differences D37 (59 rows: hidden-column constraints not re-tested), D38 (22 error texts), D39 (45 "no query solution" phase), D40 (41 name-resolution precedence), D41 (14 row order), 47 pragma lists (D4/D5/D36) |
| all-kinds new differences | `all_kinds_bugs_r3.json` vs V4a's | 12 statements that were "unsupported" in V4a: 6 `fsdir` (shell-only module, D5/D36), 6 OP_Transaction generation cascades of D27 (gendiv.py: after #37420 / #38864); V4a's 355 unchanged (352 byte-identical outputs, 3 random-data root pages) |
| xBestIndex call semantics over modules no module of the build exercises | scratch C modules `vtx` / `vtbad` (`wdump/vtx.c`, `V4BVTX=1`) vs `vtab_tests.rs` hosts | sqlite3_vtab_in(…,1) + OP_VInitIn, sqlite3_vtab_rhs_value, sqlite3_vtab_distinct (1 / 2), sqlite3_vtab_collation (BINARY / NOCASE), argv-gap "vtbad.xBestIndex malfunction": listings and BI/BO lines equal to C's (4 unit tests) |
| library level, no schema (V1 corpora) | `cgdiff_r3.txt` | total 54 613, identical 54 612, different 0, unsupported 1 (statement kind) = V4a |
| dmlfuzz 6001 400 (--no-api) | `dmlfuzz_r3.txt` | 378 identical / 22 different: the same seed set as V4a r2 and the Part 0 baseline |
| finder (245 scenarios) | `finder_r3.txt` | levels = h7b; 1 stderr-only difference t/g06_vacuum_into_main_untouched (environmental, as in V3 / V4a) |
| workspace tests (final tree) | `test_ws_r3_off.txt` / `_on.txt` | off 1011 passed / 4 failed (the known *_differential_parity, see below) / 337 ignored; on (SQLITERS_VDBE_EXEC=1) 1005 / 10 (the same 10 as V4a: M2's 9 + F6) / 337 |
| codegen unit tests (C-produced expectations) | `cargo test -p sqliters-codegen` | 41 passed (9 new in vtab_tests.rs; plan_tests::unsupported_in_v3: the pragma_table_info statement replaced by a DML statement, see the report) |
| the 4 known failing workspace tests | tests/phase7_vtab_framework_and_pragma_tables.rs | they fail because the reference binary path is hard-coded to `/home/hakanbiris/github/sqliters/target/sqlite-ref/sqlite3` (absent here); with the path pointed at `$S/bin/sqlite3` (temporary, reverted) all 8 tests of the file pass. Not an EXPLAIN / EQP matter |
| speedbench vs the 1.100 baseline | `speedbench_ab_r3.txt` (7 trials), `speedbench_ab_r3_w13*.txt` | TOTAL 301.07 -> 302.52 ms (1.005x), geomean 1.006x, digests OK; W13 1.074x / 1.034x (15 trials) vs base_1100 but 0.992x vs the V4a final binary (15 trials): not caused by V4b |
| all statement kinds, EQP | `all_kinds_eqp_r3.txt` | 14 773 accepted / 14 495 identical / 278 different = V4a's 272 + the 6 fsdir statements (D5/D36) |

## 1. C function → Rust item (functions ported or completed in V4b)
Paths under `crates/`. cov = C lines executed % / branches taken at least once % (gcov over the accepted statements of
vt_a + vt_b + vt_t + vt_d + vt_e + vt_x, each as EXPLAIN and EQP, with their setup statements, plus vt_m.txt over the
scratch modules vtx / vtbad); untaken = source lines with a
branch outcome never seen. The virtual-table blocks inside functions earlier steps ported are in the second table
(from cov/coverage.md).

| C function | file | Rust item | cov (lines / branches) | untaken |
|---|---|---|---|---|
| `addModuleArgument` | vtab.c | `add_module_argument` (`sqliters-codegen/src/vtab.rs`) | 84.6 / 50.0 | 2 |
| `sqlite3VtabBeginParse` | vtab.c | `sqlite3_vtab_begin_parse` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 50.0 | 2 |
| `addArgumentToVtab` | vtab.c | `add_argument_to_vtab` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 75.0 | 1 |
| `sqlite3VtabFinishParse` | vtab.c | `sqlite3_vtab_finish_parse` (`sqliters-codegen/src/vtab.rs`) | 93.8 / 60.0 | 4 |
| `sqlite3VtabArgInit` | vtab.c | `sqlite3_vtab_arg_init` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3VtabArgExtend` | vtab.c | `sqlite3_vtab_arg_extend` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 100.0 | 0 |
| `vtabCallConstructor` | vtab.c | `vtab_call_constructor` (`sqliters-codegen/src/vtab.rs`) | 77.1 / 59.5 | 13 |
| `sqlite3VtabCallConnect` | vtab.c | `sqlite3_vtab_call_connect` (`sqliters-codegen/src/vtab.rs`) | 72.2 / 66.7 | 2 |
| `sqlite3_declare_vtab` | vtab.c | `sqlite3_declare_vtab+declare_vtab_parse` (`sqliters-codegen/src/vtab.rs`) | 72.7 / 46.4 | 11 |
| `sqlite3GetVTable` | vtab.c | `sqlite3_get_vtable` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 75.0 | 1 |
| `sqlite3VtabEponymousTableInit` | vtab.c | `sqlite3_vtab_eponymous_table_init` (`sqliters-codegen/src/vtab.rs`) | 80.0 / 75.0 | 3 |
| `sqlite3VtabOverloadFunction` | vtab.c | `sqlite3_vtab_overload_function` (`sqliters-codegen/src/vtab.rs`) | 96.2 / 78.6 | 3 |
| `sqlite3VtabMakeWritable` | vtab.c | `sqlite3_vtab_make_writable` (`sqliters-codegen/src/vtab.rs`) | 80.0 / 37.5 | 4 |
| `sqlite3IsShadowTableOf` | build.c | `sqlite3_is_shadow_table_of` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 50.0 | 6 |
| `sqlite3MarkAllShadowTablesOf` | build.c | `sqlite3_mark_all_shadow_tables_of` (`sqliters-codegen/src/vtab.rs`) | 93.3 / 75.0 | 5 |
| `sqlite3ShadowTableName` | build.c | `sqlite3_shadow_table_name` (`sqliters-codegen/src/vtab.rs`) | 100.0 / 75.0 | 2 |
| `isAuxiliaryVtabOperator` | whereexpr.c | `is_auxiliary_vtab_operator` (`sqliters-codegen/src/whereexpr.rs`) | 85.0 / 81.8 | 5 |
| `whereAddLimitExpr` | whereexpr.c | `where_add_limit_expr` (`sqliters-codegen/src/whereexpr.rs`) | 100.0 / 70.0 | 3 |
| `sqlite3WhereAddLimit` | whereexpr.c | `sqlite3_where_add_limit` (`sqliters-codegen/src/whereexpr.rs`) | 82.8 / 81.8 | 5 |
| `sqlite3WhereTabFuncArgs` | whereexpr.c | `sqlite3_where_tab_func_args` (`sqliters-codegen/src/whereexpr.rs`) | 92.6 / 88.9 | 2 |
| `whereClauseInsert` | whereexpr.c | `where_clause_insert` (`sqliters-codegen/src/whereexpr.rs`) | 78.3 / 58.3 | 3 |
| `termFromWhereClause` | where.c | `term_from_where_clause` (`sqliters-codegen/src/where_.rs`) | 80.0 / 75.0 | 1 |
| `allocateIndexInfo` | where.c | `allocate_index_info` (`sqliters-codegen/src/where_.rs`) | 93.3 / 88.9 | 8 |
| `freeIdxStr` | where.c | `(dropped_with_IndexInfo)` (`sqliters-codegen/src/where_.rs`) | 100.0 / 100.0 | 0 |
| `freeIndexInfo` | where.c | `(dropped_with_IndexInfo)` (`sqliters-codegen/src/where_.rs`) | 100.0 / 100.0 | 0 |
| `vtabBestIndex` | where.c | `vtab_best_index` (`sqliters-codegen/src/where_.rs`) | 81.2 / 70.0 | 3 |
| `isLimitTerm` | where.c | `is_limit_term` (`sqliters-codegen/src/where_.rs`) | 100.0 / 75.0 | 1 |
| `allConstraintsUsed` | where.c | `all_constraints_used` (`sqliters-codegen/src/where_.rs`) | 100.0 / 100.0 | 0 |
| `whereLoopAddVirtualOne` | where.c | `where_loop_add_virtual_one` (`sqliters-codegen/src/where_.rs`) | 96.7 / 88.3 | 7 |
| `sqlite3_vtab_collation` | where.c | `HiddenCalls::collation` (`sqliters-codegen/src/where_.rs`) | 100.0 / 75.0 | 1 |
| `sqlite3_vtab_in` | where.c | `HiddenCalls::vtab_in` (`sqliters-codegen/src/where_.rs`) | 90.0 / 75.0 | 2 |
| `sqlite3_vtab_rhs_value` | where.c | `HiddenCalls::rhs_value` (`sqliters-codegen/src/where_.rs`) | 93.8 / 60.0 | 3 |
| `sqlite3_vtab_distinct` | where.c | `HiddenCalls::distinct` (`sqliters-codegen/src/where_.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3VtabUsesAllSchemas` | where.c | `vtab_best_index` (`sqliters-codegen/src/where_.rs`) | 0.0 / 0.0 | 3 |
| `whereLoopAddVirtual` | where.c | `where_loop_add_virtual` (`sqliters-codegen/src/where_.rs`) | 96.2 / 95.5 | 2 |
| `whereLoopXfer` | where.c | `where_loop_insert` (`sqliters-codegen/src/where_.rs`) | 84.6 / 87.5 | 1 |
| `whereInterstageHeuristic` | where.c | `where_interstage_heuristic` (`sqliters-codegen/src/where_.rs`) | 60.0 / 50.0 | 4 |
| `sqlite3LogEstFromDouble` | util.c | `sqlite3_log_est_from_double` (`sqliters-codegen/src/where_.rs`) | 100.0 / 100.0 | 0 |

### 1.1 Virtual-table blocks inside functions ported by earlier steps

| C file | block | lines | lines covered | branches | taken at least once | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| wherecode.c | sqlite3WhereCodeOneLoopStart: Case 1 virtual table | 166289-166412 | 56 / 58 | 52 | 33 | 14 |
| wherecode.c | sqlite3WhereAddExplainText: VIRTUAL TABLE INDEX | 164875-164880 | 4 / 4 | 4 | 4 | 0 |
| whereexpr.c | exprAnalyze: WO_AUX terms | 169275-169308 | 23 / 23 | 10 | 9 | 1 |
| where.c | whereLoopAddAll: virtual table | 174716-174724 | 5 / 5 | 6 | 6 | 0 |
| where.c | whereLoopAddOr: virtual table | 174583-174585 | 2 / 2 | 2 | 2 | 0 |
| where.c | wherePathSatisfiesOrderBy: virtual table | 174934-174943 | 5 / 5 | 6 | 6 | 0 |
| where.c | sqlite3WhereBegin: OP_VOpen | 176979-176985 | 5 / 5 | 4 | 4 | 0 |
| where.c | whereInterstageHeuristic: vtab break | 176027-176030 | 2 / 2 | 2 | 2 | 0 |
| select.c | selectExpander: unsafe use of virtual table | 155727-155734 | 5 / 5 | 10 | 7 | 3 |
| select.c | sqlite3SelectCheckOnClauses: table-function arguments | 157194-157214 | 13 / 13 | 12 | 12 | 0 |
| expr.c | sqlite3ExprCodeTarget: sqlite3VtabOverloadFunction | 118436-118440 | 4 / 4 | 6 | 5 | 1 |
| expr.c | sqlite3ExprCodeGetColumnOfTable: OP_VColumn | 117408-117434 | 8 / 18 | 8 | 5 | 2 |
| build.c | sqlite3LocateTable: eponymous virtual table | 127213-127232 | 8 / 8 | 16 | 14 | 1 |
| build.c | viewGetColumnNames: sqlite3VtabCallConnect | 129963-129968 | 5 / 5 | 2 | 2 | 0 |
| build.c | sqlite3EndTable: TF_Shadow | 129516-129518 | 2 / 2 | 4 | 3 | 1 |

## 2. Decision tables (C source → rule → Rust)

Every rule below is a decision of C's vtab.c, of the virtual-table parts of where.c / wherecode.c / whereexpr.c /
select.c / expr.c / build.c / parse.y, or of the engine bridge that stands in for C's `sqlite3_module` calls, and the
place of its 1:1 counterpart. "Layer" says which differential proves the outcome (EXPLAIN bytes, EQP text, error
text+offset, the planner dump layers terms / loops / paths, the new xBestIndex layer `bidx` = every BI/BO pair, the
schema layer). Section 3 lists the gcov outcome of every branch line of these functions and blocks.

### 2.1 CREATE VIRTUAL TABLE at schema load (parse.y 1939–1956, vtab.c 360–540) — `parse.rs` 302–308, `vtab.rs`  *(layer: schema `T … eTabType=1`, then everything that uses the table)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| parse.y:1941 rule 304 | `createkw VIRTUAL TABLE ifnotexists nm dbnm USING nm` → sqlite3VtabBeginParse(X, Y, Z, E) | parse.rs 304 |
| parse.y:1940/1942 rules 302/303 | end of statement with / without argument list → sqlite3VtabFinishParse(pParse, 0 / &RP) | parse.rs 302, 303 |
| parse.y:1947–1951 rules 305–308 | `vtabarg ::= .` → sqlite3VtabArgInit; `ANY` / `lp anylist RP` / `LP` → sqlite3VtabArgExtend (the parser's wildcard ANY takes every other token) | parse.rs 305–308 (YYWILDCARD in the driver) |
| vtab.c:397 | sqlite3StartTable(…, isVirtual=1, noErr) (the init special case `init.busy && newTnum==1` only for sqlite_schema itself) | `sqlite3_vtab_begin_parse`, build_tab.rs `sqlite3_start_table` |
| vtab.c:403–407 | azArg = [module name, NULL (database, set by the constructor), table name] | `add_module_argument` ×3 |
| vtab.c:368 | `nArg+3 >= SQLITE_LIMIT_COLUMN` → "too many columns on %s" (argument still added) | `add_module_argument` |
| vtab.c:438 | `sArg.z && pNewTable` → append the accumulated argument text verbatim (from the first token to the end of the last) | `add_argument_to_vtab`, `sqlite3_vtab_arg_extend` |
| vtab.c:457 | `nArg<1` → nothing | `sqlite3_vtab_finish_parse` |
| vtab.c:465 | `!db->init.busy` (a real CREATE VIRTUAL TABLE) → code generation (not ported: reported unsupported, statement kind) | `sqlite3_vtab_finish_parse` |
| vtab.c:514–525 | init: sqlite3MarkAllShadowTablesOf, insert into tblHash, pNewTable=0 | `sqlite3_vtab_finish_parse` |
| build.c:2627–2645 | module unknown / no xShadowName (iVersion<3) → nothing; else every ordinary table `<vtab>_<suffix>` with xShadowName(suffix) true gets TF_Shadow | `sqlite3_mark_all_shadow_tables_of` (Host::vtab_module().shadow_names) |
| build.c:2766 (EndTable) | `pSelect==0 && sqlite3ShadowTableName(zName)` → TF_Shadow (a shadow table loaded after its virtual table) | build_tab.rs `sqlite3_end_table`, `sqlite3_shadow_table_name` |

### 2.2 Constructor and declared schema (vtab.c 560–735, 822–915) — `vtab.rs`, build_tab.rs `declare_vtab_parse`  *(layer: EXPLAIN bytes of every VColumn / affinity, error text)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| vtab.c:707 | sqlite3GetVTable(db, pTab) already set → OK, no second xConnect | `sqlite3_vtab_call_connect` (Table.vtab) |
| vtab.c:713–717 | module azArg[0] not registered (`sqlite3HashFind(&db->aModule)`, no lazy registration) → "no such module: %s" | `sqlite3_vtab_call_connect` (Host::vtab_module(lazy=false)) |
| vtab.c:579 | the same table's constructor already running (pVtabCtx chain) → SQLITE_LOCKED "vtable constructor called recursively: %s" | `vtab_call_constructor` (Parse.vtab_ctx) |
| vtab.c:601 | azArg[1] = name of the table's database (eponymous: main) | `vtab_call_constructor` |
| vtab.c:624–629 | constructor failed: zErr==0 → "vtable constructor failed: %s", else zErr | `vtab_call_constructor` (VtabConnect::Err) |
| vtab.c:639 | constructor did not call sqlite3_declare_vtab → "vtable constructor did not declare schema: %s" | `vtab_call_constructor` (declare None) |
| vtab.c:839–846 | first two tokens (spaces/comments skipped) must be CREATE, TABLE → "syntax error" (MISUSE) | `sqlite3_declare_vtab` |
| vtab.c:858–867 | nested parse with PARSE_MODE_DECLARE_VTAB, `init.busy=0`, nQueryLoop=1, own Parse (its VDBE is finalized unused) | build_tab.rs `declare_vtab_parse` (state saved/restored), build.rs FinishCoding emits nothing in that mode |
| build.c:1337 | IN_SPECIAL_PARSE: no ReadSchema / name collision check in StartTable | build_tab.rs `sqlite3_start_table` (declare mode skips codegen) |
| build.c:3186 sqlite3CheckObjectName | not init: name starting "sqlite_" (nested==0) → "object name reserved for internal use: %s" | build_tab.rs `sqlite3_check_object_name` |
| build.c:1978 / 2048 / 3715 / 4056 | declare mode: CHECK ignored; generated column → "virtual tables cannot use computed columns"; FOREIGN KEY ignored; index other than the PRIMARY KEY skipped | build_tab.rs (in_declare_vtab) |
| vtab.c:873 | `!pTab->aCol` → take the declared columns (nCol, nNVCol), TF_WithoutRowid / TF_NoVisibleRowid; WITHOUT ROWID with xUpdate and a PK of more than one column → SQLITE_ERROR | `sqlite3_declare_vtab` (WITHOUT ROWID declared tables: unsupported, none of the engine's modules declares one) |
| vtab.c:663–668 | a column type containing the word "hidden" (case-insensitive, at the start or after a space, followed by end or space) → removed (with the following space, or the preceding one at the end), COLFLAG_HIDDEN, TF_HasHidden; affinity stays the one computed from the full type text (e.g. "hidden" → NUMERIC) | `vtab_call_constructor` |
| vtab.c:682 | a visible column after a hidden one → TF_OOOHidden | `vtab_call_constructor` |
| vtab.c:726 | connected → sqlite3MarkAllShadowTablesOf | `sqlite3_vtab_call_connect` |
| build.c:3182 viewGetColumnNames | virtual table → sqlite3VtabCallConnect (nSchemaLock++/--) | select.rs `sqlite3_view_get_column_names` |

### 2.3 Locating the table: eponymous tables (build.c 409–470, vtab.c 1262–1295) — `schema.rs`, `vtab.rs`  *(layer: EXPLAIN bytes / error text)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| build.c:432 | not found in the schema, `!(prepFlags & NO_VTAB) && !init.busy` → module named like the table (`sqlite3HashFind(aModule, zName)`), else `pragma_*` → sqlite3PragmaVtabRegister (only pragmas with Result0/Result1), else `json*` → sqlite3JsonVtabRegister | schema.rs `sqlite3_locate_table` (Host::vtab_module(lazy=true)) |
| build.c:446 | module found and sqlite3VtabEponymousTableInit returns 1 → return pMod->pEpoTab (NULL after a failed constructor: the error message stays) | schema.rs |
| vtab.c:1267 | pEpoTab exists → 1 | `sqlite3_vtab_eponymous_table_init` (Parse.epo_tab) |
| vtab.c:1268 | `xCreate!=0 && xCreate!=xConnect` (fts5, fts5vocab, rtree) → 0: "no such table" | `sqlite3_vtab_eponymous_table_init` (VtabModule.eponymous) |
| vtab.c:1276–1284 | Table: zName = module name as registered, TF_Eponymous, schema main, iPKey -1, azArg [name, NULL, name]; xConnect | `sqlite3_vtab_eponymous_table_init` |
| vtab.c:1287–1292 | constructor failed → error message, pParse->rc, pEpoTab cleared, return 1 | `sqlite3_vtab_eponymous_table_init` |
| build.c:455 | virtual table found but SQLITE_PREPARE_NO_VTAB → NULL ("no such table") | schema.rs |
| select.c:6161 | `!IsVirtual(pTab) && fg.isTabFunc` → "'%s' is not a function" | select.rs `cannot_be_function` (V2) |
| select.c:6180–6185 | virtual table from DDL (view / trigger, or SQLITE_PREPARE_FROM_DDL) and `eVtabRisk > (TrustedSchema!=0)` → "unsafe use of virtual table \"%s\"" | select.rs (VTable.e_vtab_risk) |

### 2.4 WHERE terms of virtual tables (whereexpr.c 402–498, 1559–1603, 1649–1780, 1940–1986) — `whereexpr.rs`  *(layer: terms incl. TERMAUX, EXPLAIN bytes, error text)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| whereexpr.c:1948–1953 | item without isTabFunc → nothing; function syntax on a non-virtual table → "'%s' is not a function" | `sqlite3_where_tab_func_args` |
| whereexpr.c:1958–1963 | j-th argument → the j-th HIDDEN column (in column order); none left → "too many arguments on %s() - max %d" | `sqlite3_where_tab_func_args` |
| whereexpr.c:1965–1985 | term `hiddencol = +(argument)` (TK_UPLUS over a copy), colUsed bit, EP_OuterON when the item is the right side of LEFT/RIGHT JOIN else EP_InnerON (w.iJoin = cursor), whereClauseInsert(TERM_DYNAMIC) | `sqlite3_where_tab_func_args` |
| whereexpr.c:418 | TK_FUNCTION with exactly two arguments, second argument a vtab column and the function is like/glob/match/regexp (any case) → (1, SQLITE_INDEX_CONSTRAINT_LIKE/GLOB/MATCH/REGEXP, col=arg2, expr=arg1) | `is_auxiliary_vtab_operator` |
| whereexpr.c:450–466 | else first argument a vtab column whose module has xFindFunction returning ≥ SQLITE_INDEX_CONSTRAINT_FUNCTION (150) → that operator (engine: fts5's xFindFunction returns 1 for its auxiliary functions: never a constraint) | `is_auxiliary_vtab_operator` (Host::vtab_find_function) |
| whereexpr.c:469 | `op >= TK_EQ` → 0 (early exit) | `is_auxiliary_vtab_operator` |
| whereexpr.c:475–495 | TK_NE / TK_ISNOT / TK_NOTNULL: count = left is vtab column + right is vtab column (then swapped), operator NE / ISNOT / ISNOTNULL | `is_auxiliary_vtab_operator` |
| whereexpr.c:1570 | `pWC->op==TK_AND` and not the vector-IN case → for each match: if the expr does not use the column's table (`(prereqExpr & prereqColumn)==0`), add `TK_MATCH(NULL, copy of expr)` as TERM_VIRTUAL|TERM_DYNAMIC, EP_OuterON+iJoin copied, prereqRight = prereqExpr|extraRight, leftCursor/leftColumn of the column, eOperator WO_AUX, eMatchOp, child of the term, parent TERM_COPIED, prereqAll copied; then left/right swapped for the second match | expr_analyze (WO_AUX block) |
| whereexpr.c:88–90 (whereClauseInsert) | `iParent=-1` then `memset(&eOperator…)` zeroes it: an inserted term that exprAnalyze never sees (LIMIT/OFFSET terms) keeps iParent 0 | `where_clause_insert` (i_parent: 0) |
| whereexpr.c:1712–1714 | AddLimit only for: no GROUP BY, not SF_Distinct/SF_Aggregate, exactly one FROM item that is a virtual table | `sqlite3_where_add_limit` |
| whereexpr.c:1722–1752 | every WHERE term must reach xBestIndex: TERM_CODED skipped, terms with children skipped, `leftCursor==iCsr && prereqRight==0` ok, parent passed through with exactly one child ok, else no LIMIT | `sqlite3_where_add_limit` |
| whereexpr.c:1755–1760 | ORDER BY terms all TK_COLUMN of the table and not NULLS FIRST-type BIGNULL, else no LIMIT | `sqlite3_where_add_limit` |
| whereexpr.c:1766–1771 | OFFSET term when `iOffset!=0` and not compound; LIMIT term when `iOffset==0` or not compound | `sqlite3_where_add_limit` |
| whereexpr.c:1671 | limit/offset expression an integer ≥0 (sqlite3ExprIsInteger, bound parameter looked at) → TK_MATCH(NULL, integer) (rhs_value works); else TK_MATCH(NULL, TK_REGISTER(iReg)) | `where_add_limit_expr` |
| where.c:176705 (sqlite3WhereBegin) | AddLimit after sqlite3WhereExprAnalyze when the SELECT has a LIMIT | where_.rs `sqlite3_where_begin` |

### 2.5 The xBestIndex calls (where.c 1405–1697, 4315–4808, 4996–5019) — `where_.rs`  *(layer: bidx (BI/BO), loops, paths, error text)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| where.c:1446–1460 | constraint = term (over pWC and its pOuter chain) with leftCursor of the table, `prereqRight & mUnusable == 0`, eOperator other than WO_EQUIV alone, not TERM_VNULL, and for LEFT/LTORJ/RIGHT items constraintCompatibleWithOuterJoin → TERM_OK | `allocate_index_info` |
| where.c:1477–1508 | ORDER BY passed only if every non-constant term is a column of the table (or COLLATE equal to the column's collation, iColumn copied into the COLLATE node) and none is NULLS FIRST/LAST-BIGNULL; constant terms skipped | `allocate_index_info` |
| where.c:1513–1520 | eDistinct: DISTINCTBY and rowid not used → 2+SortByGroup; GROUPBY → 1-SortByGroup; WANT_DISTINCT → 3; else 0 | `allocate_index_info` |
| where.c:1541–1550 | colUsed = item colUsed, plus the PK columns of a WITHOUT ROWID table (≥63 → bit 63) | `allocate_index_info` |
| where.c:1565–1600 | op: WO_IN → EQ (mIn bit unless TERM_SLICE); WO_AUX → eMatchOp; WO_ISNULL / WO_IS → ISNULL / IS; else the WO_ value; LT/LE/GT/GE with a vector RHS → mNoOmit bit (j<16) and LT→LE, GT→GE | `allocate_index_info` |
| where.c:4393 | usable = prereqs inside mUsable, operator not in mExclude, and (retry allowed or not a LIMIT/OFFSET term) | `where_loop_add_virtual_one` |
| where.c:4402–4411 | outputs reset: usage 0, idxStr 0, idxNum 0, orderByConsumed 0, estimatedCost = SQLITE_BIG_DBL/2, estimatedRows 25, idxFlags 0, mHandleIn 0 | `where_loop_add_virtual_one` (engine bridge copies them in) |
| where.c:1681–1688 | rc not OK and not CONSTRAINT: NOMEM → OOM; no zErrMsg → sqlite3ErrStr(rc); else zErrMsg | `vtab_best_index`, `sqlite3_err_str` |
| where.c:1690 | bAllSchemas → sqlite3VtabUsesAllSchemas (verify every schema; begin writes if writeMask) | `vtab_best_index` (no engine module sets it) |
| where.c:4415 | SQLITE_CONSTRAINT → no loop, OK | `where_loop_add_virtual_one` |
| where.c:4439–4446 | argvIndex out of range, term missing, slot already used, or constraint not usable → "%s.xBestIndex malfunction" | `where_loop_add_virtual_one` |
| where.c:4456–4465 | omit: `i<16 && !(mNoOmit bit)` → omitMask bit of argv slot; omitted OFFSET term → bOmitOffset | `where_loop_add_virtual_one` |
| where.c:4467–4478 | `sqlite3_vtab_in(…,1)` was called for the constraint → mHandleIn bit; else an IN term → orderByConsumed=0, idxFlags &= ~UNIQUE, *pbIn | `where_loop_add_virtual_one` |
| where.c:4486 | a used LIMIT/OFFSET term with *pbIn or an earlier unused constraint → *pbRetryLimit=1, no loop | `where_loop_add_virtual_one` |
| where.c:4502–4508 | argv slots not contiguous → "%s.xBestIndex malfunction" | `where_loop_add_virtual_one` |
| where.c:4512–4530 | idxNum, needFree, idxStr, isOrdered = orderByConsumed ? nOrderBy : 0, bIdxNumHex = idxFlags & SCAN_HEX, rRun = sqlite3LogEstFromDouble(cost), nOut = sqlite3LogEst(rows), ONEROW iff SCAN_UNIQUE | `where_loop_add_virtual_one`, `sqlite3_log_est_from_double` |
| where.h union | u.vtab and u.btree share memory: after a vtab table, the next table's automatic-index loop finds idxNum / bit-fields / isOrdered / omitMask in nEq/nBtm/nTop/nDistinctCol | `alias_vtab_into_btree` (loops layer) |
| where.c:4728 | first call with everything usable; retry without LIMIT/OFFSET when asked | `where_loop_add_virtual` |
| where.c:4740 | further calls only if the plan needs other tables or uses IN | `where_loop_add_virtual` |
| where.c:4746–4756 | plan used IN → call with IN excluded; mBestNoIn | `where_loop_add_virtual` |
| where.c:4762–4783 | one call per distinct `prereqRight & ~mPrereq` value of the constraints in increasing order, skipping mBest / mBestNoIn | `where_loop_add_virtual` |
| where.c:4787 / 4797 | no zero-prereq plan seen → call with only mPrereq usable; none without IN → that with IN excluded | `where_loop_add_virtual` |
| where.c:5010–5016 | virtual table: mUnusable = later FROM items after an OUTER/CROSS join | `where_loop_add_all` |
| where.c:4876 (whereLoopAddOr) | OR sub-clauses of a virtual table → whereLoopAddVirtual on the sub-clause | `where_loop_add_or` |
| where.c:5228–5236 | path order: vtab loop with isOrdered and the ORDER BY being pWInfo's → all terms satisfied; else stop matching (isOrderDistinct=0) | `where_path_satisfies_order_by` |
| where.c:6321 | whereInterstageHeuristic stops at a vtab loop (pWLoop NULL until the solver succeeds) | `where_interstage_heuristic` |
| where.c:4568–4588 | sqlite3_vtab_in: constraint in mIn → bHandle 0 clears / >0 sets mHandleIn, true; else false | `HiddenCalls::vtab_in` |
| where.c:4598–4620 | sqlite3_vtab_rhs_value: out of range → MISUSE; value of the RHS (sqlite3ValueFromExpr, BLOB affinity), computed once; none → NOTFOUND | `HiddenCalls::rhs_value` |
| where.c:4556–4566 | sqlite3_vtab_collation: term with a left operand → sqlite3ExprCompareCollSeq, else / none → "BINARY" | `HiddenCalls::collation` |

### 2.6 Code for the loop (wherecode.c 203–209, 1617–1742; where.c 7272–7279) — `wherecode.rs`, `where_.rs`  *(layer: EXPLAIN bytes, EQP text)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| where.c:7273–7276 | vtab loop → OP_VOpen cursor, P4_VTAB (the instance); a virtual table without a vtab loop → nothing | `sqlite3_where_begin` |
| wherecode.c:1624 | registers iReg..iReg+nConstraint+1 (temp range, never released) | `sqlite3_where_code_one_loop_start` |
| wherecode.c:1633–1641 | IN term handled by the module (mHandleIn) → sqlite3CodeRhsOfIN into a new cursor + OP_VInitIn; other IN → codeEqualityTerm (IN loop), addrNotFound = addrNxt | same |
| wherecode.c:1643–1652 | other term → codeExprOrVector(RHS); omitted OFFSET (bOmitOffset) → OP_Integer 0 into the OFFSET counter | same |
| wherecode.c:1656–1664 | OP_Integer idxNum, OP_Integer nConstraint (argc, right before VFilter), OP_VFilter P4 = idxStr (P4_DYNAMIC/STATIC, none if NULL) | same |
| wherecode.c:1670 | loop end op: OP_VNext, or OP_Noop with one-pass | same |
| wherecode.c:1676 | omitted constraint (j<16 and omitMask bit) → disableTerm (walks iParent; a LIMIT term's iParent 0 decrements term 0's nChild) | same, `disable_term` |
| wherecode.c:1680–1735 | IN term not handled by the module → reload the value (OP_Column / OP_Rowid of the IN loop whose target is the register), then `col = reg` false → addrCont (JUMPIFNULL); vector IN: the iField-th left column | same |
| wherecode.c:204–208 | EQP " VIRTUAL TABLE INDEX " + idxNum as "0x%x" (bIdxNumHex) or "%d", ":" + idxStr ("" if NULL) | `sqlite3_where_add_explain_text` |
| expr.c:4542 | column of a virtual table → OP_VColumn (rowid: OP_Rowid); P5 only OPFLAG_NOCHNG | exprcode.rs (V1) |
| expr.c:5570–5574 | function whose first argument (second for infix LIKE/GLOB/REGEXP/MATCH) is a vtab column whose module has xFindFunction → sqlite3VtabOverloadFunction: overloaded → copy with SQLITE_FUNC_EPHEM | exprcode.rs, `sqlite3_vtab_overload_function` |
| select.c:7646–7666 | TVF arguments of an item left of a RIGHT JOIN or the RHS of one (iJoin = item cursor), or left of a LEFT JOIN (iJoin = cursor before it) → walked like ON clauses: "table-function argument references tables to its right" | flatten.rs `sqlite3_select_check_on_clauses` |

### 2.7 The engine bridge (C's sqlite3_module calls) — `crates/sqliters-core/src/cgen.rs`, module crates  *(layer: bidx)*
| C | rule → outcome | Rust |
|---|---|---|
| vtab.c:713 / build.c:433 | module lookup by name, case-insensitive; `pragma_*` registered lazily with the spelling of the first use (zName of the module and of the eponymous table) | `EngineHost::vtab_module`, vtab.rs `VTabRegistry::get_or_register_module` / `registered_module_name` |
| vtab.c:617 xConnect | the engine connects the virtual tables an EXPLAIN names before the generator runs (`ensure_stmt_vtabs_connected`); the generator gets that instance (id = registry entry address), its declare_vtab text, constraint support, risk (INNOCUOUS → Low, DIRECTONLY → High, none → Normal), xFindFunction | `EngineHost::vtab_connect`, lib.rs `prepare_in_session` |
| sqlite3_index_info | constraint op codes = SQLITE_INDEX_CONSTRAINT_*, usable, iColumn, ORDER BY, colUsed and C's initial outputs in; argvIndex/omit, idxNum, idxStr, needToFreeIdxStr, orderByConsumed, cost, rows, idxFlags out; error SQLITE_CONSTRAINT → 19 without message, other → code + message | `EngineHost::vtab_best_index`, `VTabConstraintOp::c_code` |
| series.c seriesBestIndex (shell 3.54.1) | START/STOP/STEP EQ, value =/IS/>=/>/<=/< bits, LIMIT/OFFSET bits (and the `aIdx[3]==0` quirk), first-argument check, unusable mask → CONSTRAINT, cost/rows, ORDER BY value consumed with ASC/DESC bit, SQLITE_INDEX_SCAN_HEX | sqliters-ext generate_series.rs `best_index_calls` |
| json.c jsonEachBestIndex | JSON/ROOT EQ (any operator on them unusable → CONSTRAINT), rowid ASC consumed, idxNum 0/1/3, cost 1 | sqliters-ext json_vtab.rs `best_index_calls` |
| dbstat.c statBestIndex | any unusable EQ → CONSTRAINT; schema 0x01 (omit), name 0x02, aggregate 0x04; cost 1; ORDER BY name[,path] consumed 0x08; SCAN_HEX | sqliters-core vtab.rs `DbStatTable::best_index_calls` |
| pragma.c pragmaVtabBestIndex | cost 1; no hidden columns → OK; hidden EQ unusable → CONSTRAINT; arg/schema slots; no arg → cost/rows 2147483647; else 20/20 | sqliters-core vtab.rs `PragmaVTabTable::best_index_calls` |
| fts5_vocab.c fts5VocabBestIndexMethod | term constraints only with BINARY collation (sqlite3_vtab_collation); idxNum = colUsed + EQ/GE/LE bits; costs 100 / 1000000 halved; ORDER BY term ASC consumed | sqliters-ext fts5.rs `Fts5VocabTable::best_index_calls` |
| fts5_main.c fts5BestIndexMethod / rtree.c rtreeBestIndex | (already C ports in the engine) + needToFreeIdxStr (fts5: always; rtree: on the idxNum-2 plan) and fts5's idxStr also on the CONSTRAINT return | fts5.rs / rtree.rs `best_index_calls` |

## 3. Every branch line of the ported C functions (gcov outcomes)
For each source line (line of the amalgamation `target/sqlite-ref/amalg/sqlite3.c`) of the functions of section 1
that contains a branch: the counts of each outcome. `both` = every outcome seen at least once. Lines with an outcome
never seen are discussed below the table; the blocks of section 1.1 are in `cov/untaken.txt`.

| C function | line | source | outcome counts | both |
|---|---|---|---|---|
| sqlite3LogEstFromDouble | sqlite3.c:38944 | `if( x<=1 ) return 0;` | 18100,30055 | both |
| sqlite3LogEstFromDouble | sqlite3.c:38945 | `if( x<=2000000000 ) return sqlite3LogEst((u64)x);` | 25815,4240 | both |
| sqlite3IsShadowTableOf | sqlite3.c:129384 | `if( !IsVirtual(pTab) ) return 0;` | 0,19195 | **no** |
| sqlite3IsShadowTableOf | sqlite3.c:129386 | `if( sqlite3_strnicmp(zName, pTab->zName, nName)!=0 ) return 0;` | 0,19195 | **no** |
| sqlite3IsShadowTableOf | sqlite3.c:129387 | `if( zName[nName]!='_' ) return 0;` | 0,19195 | **no** |
| sqlite3IsShadowTableOf | sqlite3.c:129389 | `if( pMod==0 ) return 0;` | 0,19195 | **no** |
| sqlite3IsShadowTableOf | sqlite3.c:129390 | `if( pMod->pModule->iVersion<3 ) return 0;` | 0,19195 | **no** |
| sqlite3IsShadowTableOf | sqlite3.c:129391 | `if( pMod->pModule->xShadowName==0 ) return 0;` | 0,19195 | **no** |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129410 | `if( pMod==0 ) return;` | 0,4285 | **no** |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129411 | `if( NEVER(pMod->pModule==0) ) return;` | 0,4285 | **no** |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129412 | `if( pMod->pModule->iVersion<3 ) return;` | 1833,2452 | both |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129413 | `if( pMod->pModule->xShadowName==0 ) return;` | 0,2452 | **no** |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129416 | `for(k=sqliteHashFirst(&pTab->pSchema->tblHash); k; k=sqliteHashNext(k)){` | 33625,2452 | both |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129419 | `if( !IsOrdinaryTable(pOther) ) continue;` | 7330,26295 | both |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129420 | `if( pOther->tabFlags & TF_Shadow ) continue;` | 16510,9785 | both |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129421 | `if( sqlite3StrNICmp(pOther->zName, pTab->zName, nName)==0` | 1,9784 | both |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129422 | `&& pOther->zName[nName]=='_'` | 1,0 | **no** |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129423 | `&& pMod->pModule->xShadowName(pOther->zName+nName+1)` | 0,1 | **no** |
| sqlite3ShadowTableName | sqlite3.c:129441 | `if( zTail==0 ) return 0;` | 13278,23212 | both |
| sqlite3ShadowTableName | sqlite3.c:129443 | `pTab = zCopy ? sqlite3FindTable(db, zCopy, 0) : 0;` | 23212,0 | **no** |
| sqlite3ShadowTableName | sqlite3.c:129445 | `if( pTab==0 ) return 0;` | 4017,19195 | both |
| sqlite3ShadowTableName | sqlite3.c:129446 | `if( !IsVirtual(pTab) ) return 0;` | 0,19195 | **no** |
| sqlite3GetVTable | sqlite3.c:162801 | `for(pVtab=pTab->u.vtab.p; pVtab && pVtab->db!=db; pVtab=pVtab->pNext);` | 191413,3,0,191413 | **no** |
| addModuleArgument | sqlite3.c:162972 | `if( pTable->u.vtab.nArg+3>=db->aLimit[SQLITE_LIMIT_COLUMN] ){` | 0,65840 | **no** |
| addModuleArgument | sqlite3.c:162976 | `if( azModuleArg==0 ){` | 0,65840 | **no** |
| sqlite3VtabBeginParse | sqlite3.c:163003 | `if( pTable==0 ) return;` | 0,8561 | **no** |
| sqlite3VtabBeginParse | sqlite3.c:163026 | `if( pTable->u.vtab.azArg ){` | 8561,0 | **no** |
| addArgumentToVtab | sqlite3.c:163041 | `if( pParse->sArg.z && pParse->pNewTable ){` | 24221,8561,24221,0 | **no** |
| sqlite3VtabFinishParse | sqlite3.c:163057 | `if( pTab==0 ) return;` | 0,8561 | **no** |
| sqlite3VtabFinishParse | sqlite3.c:163061 | `if( pTab->u.vtab.nArg<1 ) return;` | 0,8561 | **no** |
| sqlite3VtabFinishParse | sqlite3.c:163069 | `if( !db->init.busy ){` | 4279,4282 | both |
| sqlite3VtabFinishParse | sqlite3.c:163079 | `if( pEnd ){` | 4279,0 | **no** |
| sqlite3VtabFinishParse | sqlite3.c:163124 | `if( pOld ){` | 0,4282 | **no** |
| sqlite3VtabArgExtend | sqlite3.c:163149 | `if( pArg->z==0 ){` | 24221,4424 | both |
| vtabCallConstructor | sqlite3.c:163184 | `for(pCtx=db->pVtabCtx; pCtx; pCtx=pCtx->pPrior){` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163185 | `if( pCtx->pTab==pTab ){` | never,never | **no** |
| vtabCallConstructor | sqlite3.c:163194 | `if( !zModuleName ){` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163199 | `if( !pVTable ){` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163225 | `if( rc==SQLITE_NOMEM ) sqlite3OomFault(db);` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163228 | `if( SQLITE_OK!=rc ){` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163229 | `if( zErr==0 ){` | never,never | **no** |
| vtabCallConstructor | sqlite3.c:163236 | `}else if( ALWAYS(pVTable->pVtab) ){` | 9594,0 | **no** |
| vtabCallConstructor | sqlite3.c:163243 | `if( sCtx.bDeclared==0 ){` | 0,9594 | **no** |
| vtabCallConstructor | sqlite3.c:163259 | `for(iCol=0; iCol<pTab->nCol; iCol++){` | 57423,9594 | both |
| vtabCallConstructor | sqlite3.c:163264 | `for(i=0; i<nType; i++){` | 71001,45660 | both |
| vtabCallConstructor | sqlite3.c:163265 | `if( 0==sqlite3StrNICmp("hidden", &zType[i], 6)` | 11763,59238 | both |
| vtabCallConstructor | sqlite3.c:163266 | `&& (i==0 \|\| zType[i-1]==' ')` | 1138,10625,1138,0 | **no** |
| vtabCallConstructor | sqlite3.c:163267 | `&& (zType[i+6]=='\0' \|\| zType[i+6]==' ')` | 0,11763,never,never | **no** |
| vtabCallConstructor | sqlite3.c:163272 | `if( i<nType ){` | 11763,45660 | both |
| vtabCallConstructor | sqlite3.c:163274 | `int nDel = 6 + (zType[i+6] ? 1 : 0);` | 0,11763 | **no** |
| vtabCallConstructor | sqlite3.c:163275 | `for(j=i; (j+nDel)<=nType; j++){` | 11763,11763 | both |
| vtabCallConstructor | sqlite3.c:163278 | `if( zType[i]=='\0' && i>0 ){` | 11763,0,1138,10625 | **no** |
| sqlite3VtabCallConnect | sqlite3.c:163311 | `if( sqlite3GetVTable(db, pTab) ){` | 57052,3 | both |
| sqlite3VtabCallConnect | sqlite3.c:163319 | `if( !pMod ){` | 0,3 | **no** |
| sqlite3VtabCallConnect | sqlite3.c:163326 | `if( rc!=SQLITE_OK ){` | 0,3 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163440 | `for(i=0; aKeyword[i]; i++){` | 19188,9594 | both |
| sqlite3_declare_vtab | sqlite3.c:163444 | `}while( tokenType==TK_SPACE \|\| tokenType==TK_COMMENT );` | 9594,19188,0,19188 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163445 | `if( tokenType!=aKeyword[i] ){` | 0,19188 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163453 | `if( !pCtx \|\| pCtx->bDeclared ){` | 9594,0,0,9594 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163472 | `if( SQLITE_OK==sqlite3RunParser(&sParse, zCreateTable) ){` | 9594,0 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163477 | `if( !pTab->aCol ){` | 9594,0 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163489 | `if( !HasRowid(pNew)` | 0,9594 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163490 | `&& pCtx->pVTable->pMod->pModule->xUpdate!=0` | never,never | **no** |
| sqlite3_declare_vtab | sqlite3.c:163491 | `&& sqlite3PrimaryKeyIndex(pNew)->nKeyCol!=1` | never,never | **no** |
| sqlite3_declare_vtab | sqlite3.c:163498 | `if( pIdx ){` | 0,9594 | **no** |
| sqlite3_declare_vtab | sqlite3.c:163508 | `(sParse.zErrMsg ? "%s" : 0), sParse.zErrMsg);` | never,never | **no** |
| sqlite3_declare_vtab | sqlite3.c:163514 | `if( sParse.pVdbe ){` | 9594,0 | **no** |
| sqlite3VtabOverloadFunction | sqlite3.c:163776 | `if( NEVER(pExpr==0) ) return pDef;` | 0,10040 | **no** |
| sqlite3VtabOverloadFunction | sqlite3.c:163777 | `if( pExpr->op!=TK_COLUMN ) return pDef;` | 3209,6831 | both |
| sqlite3VtabOverloadFunction | sqlite3.c:163780 | `if( NEVER(pTab==0) ) return pDef;` | 0,6831 | **no** |
| sqlite3VtabOverloadFunction | sqlite3.c:163781 | `if( !IsVirtual(pTab) ) return pDef;` | 1,6830 | both |
| sqlite3VtabOverloadFunction | sqlite3.c:163786 | `if( pMod->xFindFunction==0 ) return pDef;` | 3280,3550 | both |
| sqlite3VtabOverloadFunction | sqlite3.c:163805 | `if( rc==0 ){` | 1162,2388 | both |
| sqlite3VtabOverloadFunction | sqlite3.c:163813 | `if( pNew==0 ){` | 0,2388 | **no** |
| sqlite3VtabMakeWritable | sqlite3.c:163832 | `Parse *pToplevel = sqlite3ParseToplevel(pParse);` | 0,7341 | **no** |
| sqlite3VtabMakeWritable | sqlite3.c:163837 | `for(i=0; i<pToplevel->nVtabLock; i++){` | 0,7341 | **no** |
| sqlite3VtabMakeWritable | sqlite3.c:163838 | `if( pTab==pToplevel->apVtabLock[i] ) return;` | never,never | **no** |
| sqlite3VtabMakeWritable | sqlite3.c:163842 | `if( apVtabLock ){` | 7341,0 | **no** |
| sqlite3VtabEponymousTableInit | sqlite3.c:163871 | `if( pMod->pEpoTab ) return 1;` | 28843,5450 | both |
| sqlite3VtabEponymousTableInit | sqlite3.c:163872 | `if( pModule->xCreate!=0 && pModule->xCreate!=pModule->xConnect ) return 0;` | 707,4743,138,569 | both |
| sqlite3VtabEponymousTableInit | sqlite3.c:163874 | `if( pTab==0 ) return 0;` | 0,5312 | **no** |
| sqlite3VtabEponymousTableInit | sqlite3.c:163876 | `if( pTab->zName==0 ){` | 0,5312 | **no** |
| sqlite3VtabEponymousTableInit | sqlite3.c:163893 | `if( rc ){` | 0,5312 | **no** |
| whereClauseInsert | sqlite3.c:167770 | `if( pWC->nTerm>=pWC->nSlot ){` | 840,205421 | both |
| whereClauseInsert | sqlite3.c:167774 | `if( pWC->a==0 ){` | 0,840 | **no** |
| whereClauseInsert | sqlite3.c:167775 | `if( wtFlags & TERM_DYNAMIC ){` | never,never | **no** |
| whereClauseInsert | sqlite3.c:167785 | `if( (wtFlags & TERM_VIRTUAL)==0 ) pWC->nBase = pWC->nTerm;` | 162481,43780 | both |
| whereClauseInsert | sqlite3.c:167786 | `if( p && ExprHasProperty(p, EP_Unlikely) ){` | 206261,0,0,206261 | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168115 | `if( pExpr->op==TK_FUNCTION ){` | 9174,160795 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168122 | `if( pList==0 \|\| pList->nExpr!=2 ){` | 9174,0,2,9172 | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168135 | `if( ExprIsVtab(pCol) && (i = sqlite3ExprIsLikeOperator(pExpr))!=0 ){` | 9168,4,9168,0,9168,0 | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168155 | `if( ExprIsVtab(pCol) ){` | 4,0,4,0 | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168165 | `if( pMod->xFindFunction!=0 ){` | 0,4 | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168167 | `if( i>=SQLITE_INDEX_CONSTRAINT_FUNCTION ){` | never,never | **no** |
| isAuxiliaryVtabOperator | sqlite3.c:168175 | `}else if( pExpr->op>=TK_EQ ){` | 128099,32696 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168182 | `}else if( pExpr->op==TK_NE \|\| pExpr->op==TK_ISNOT \|\| pExpr->op==TK_NOTNULL ){` | 16076,16620,14512,1564,1626,12886 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168187 | `if( ExprIsVtab(pLeft) ){` | 19804,6,6976,12828 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168192 | `if( pRight && ExprIsVtab(pRight) ){` | 18184,1626,454,17730,6,448 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168198 | `if( pExpr->op==TK_NE ) *peOp2 = SQLITE_INDEX_CONSTRAINT_NE;` | 16620,3190 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168199 | `if( pExpr->op==TK_ISNOT ) *peOp2 = SQLITE_INDEX_CONSTRAINT_ISNOT;` | 1564,18246 | both |
| isAuxiliaryVtabOperator | sqlite3.c:168200 | `if( pExpr->op==TK_NOTNULL ) *peOp2 = SQLITE_INDEX_CONSTRAINT_ISNOTNULL;` | 1626,18184 | both |
| whereAddLimitExpr | sqlite3.c:169377 | `if( sqlite3ExprIsInteger(pExpr, &iVal, pParse, 0) && iVal>=0 ){` | 6253,3938,4929,1324 | both |
| whereAddLimitExpr | sqlite3.c:169379 | `if( pVal==0 ) return;` | 0,4929 | **no** |
| whereAddLimitExpr | sqlite3.c:169383 | `if( pVal==0 ) return;` | 0,5262 | **no** |
| whereAddLimitExpr | sqlite3.c:169387 | `if( pNew ){` | 10191,0 | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169418 | `if( p->pGroupBy==0` | 15751,172 | both |
| sqlite3WhereAddLimit | sqlite3.c:169419 | `&& (p->selFlags & (SF_Distinct\|SF_Aggregate))==0             /* 2 */` | 13747,2004 | both |
| sqlite3WhereAddLimit | sqlite3.c:169420 | `&& (p->pSrc->nSrc==1 && IsVirtual(p->pSrc->a[0].pSTab))      /* 3 */` | 9733,4014,9619,114 | both |
| sqlite3WhereAddLimit | sqlite3.c:169427 | `for(ii=0; ii<pWC->nTerm; ii++){` | 26559,9037 | both |
| sqlite3WhereAddLimit | sqlite3.c:169428 | `if( pWC->a[ii].wtFlags & TERM_CODED ){` | 2,26557 | both |
| sqlite3WhereAddLimit | sqlite3.c:169435 | `if( pWC->a[ii].nChild ){` | 4854,21703 | both |
| sqlite3WhereAddLimit | sqlite3.c:169442 | `if( pWC->a[ii].leftCursor==iCsr && pWC->a[ii].prereqRight==0 ) continue;` | 21121,582,21121,0 | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169446 | `if( pWC->a[ii].iParent>=0 ){` | 0,582 | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169448 | `if( pParent->leftCursor==iCsr` | never,never | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169449 | `&& pParent->prereqRight==0` | never,never | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169450 | `&& pParent->nChild==1` | never,never | **no** |
| sqlite3WhereAddLimit | sqlite3.c:169461 | `if( pOrderBy ){` | 2976,6061 | both |
| sqlite3WhereAddLimit | sqlite3.c:169462 | `for(ii=0; ii<pOrderBy->nExpr; ii++){` | 4070,1816 | both |
| sqlite3WhereAddLimit | sqlite3.c:169464 | `if( pExpr->op!=TK_COLUMN ) return;` | 254,3816 | both |
| sqlite3WhereAddLimit | sqlite3.c:169465 | `if( pExpr->iTable!=iCsr ) return;` | 2,3814 | both |
| sqlite3WhereAddLimit | sqlite3.c:169466 | `if( pOrderBy->a[ii].fg.sortFlags & KEYINFO_ORDER_BIGNULL ) return;` | 904,2910 | both |
| sqlite3WhereAddLimit | sqlite3.c:169472 | `if( p->iOffset!=0 && (p->selFlags & SF_Compound)==0 ){` | 2318,5559,2316,2 | both |
| sqlite3WhereAddLimit | sqlite3.c:169476 | `if( p->iOffset==0 \|\| (p->selFlags & SF_Compound)==0 ){` | 2318,5559,2316,2 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169654 | `if( pItem->fg.isTabFunc==0 ) return;` | 73322,27601 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169657 | `if( !IsVirtual(pTab) ){` | 0,27601 | **no** |
| sqlite3WhereTabFuncArgs | sqlite3.c:169662 | `if( pArgs==0 ) return;` | 1414,26187 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169663 | `for(j=k=0; j<pArgs->nExpr; j++){` | 46145,24703 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169666 | `while( k<pTab->nCol && (pTab->aCol[k].colFlags & COLFLAG_HIDDEN)==0 ){k++;}` | 157889,1484,113228,44661 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169667 | `if( k>=pTab->nCol ){` | 1484,44661 | both |
| sqlite3WhereTabFuncArgs | sqlite3.c:169673 | `if( pColRef==0 ) return;` | 0,44661 | **no** |
| sqlite3WhereTabFuncArgs | sqlite3.c:169682 | `if( pItem->fg.jointype & (JT_LEFT\|JT_RIGHT) ){` | 1600,43061 | both |
| termFromWhereClause | sqlite3.c:171102 | `for(p=pWC; p; p=p->pOuter){` | 255352,0 | **no** |
| termFromWhereClause | sqlite3.c:171103 | `if( iTerm<p->nTerm ) return &p->a[iTerm];` | 241104,14248 | both |
| allocateIndexInfo | sqlite3.c:171146 | `for(p=pWC, nTerm=0; p; p=p->pOuter){` | 49169,46949 | both |
| allocateIndexInfo | sqlite3.c:171147 | `for(i=0, pTerm=p->a; i<p->nTerm; i++, pTerm++){` | 153659,49169 | both |
| allocateIndexInfo | sqlite3.c:171149 | `if( pTerm->leftCursor != pSrc->iCursor ) continue;` | 37631,116028 | both |
| allocateIndexInfo | sqlite3.c:171150 | `if( pTerm->prereqRight & mUnusable ) continue;` | 1292,114736 | both |
| allocateIndexInfo | sqlite3.c:171156 | `if( (pTerm->eOperator & ~(WO_EQUIV))==0 ) continue;` | 4,114732 | both |
| allocateIndexInfo | sqlite3.c:171157 | `if( pTerm->wtFlags & TERM_VNULL ) continue;` | 1616,113116 | both |
| allocateIndexInfo | sqlite3.c:171162 | `if( (pSrc->fg.jointype & (JT_LEFT\|JT_LTORJ\|JT_RIGHT))!=0` | 4136,108980 | both |
| allocateIndexInfo | sqlite3.c:171163 | `&& !constraintCompatibleWithOuterJoin(pTerm,pSrc)` | 1478,2658 | both |
| allocateIndexInfo | sqlite3.c:171177 | `if( pOrderBy ){` | 14225,32724 | both |
| allocateIndexInfo | sqlite3.c:171179 | `for(i=0; i<n; i++){` | 17999,7633 | both |
| allocateIndexInfo | sqlite3.c:171184 | `if( sqlite3ExprIsConstant(0, pExpr) ){` | 2,17997 | both |
| allocateIndexInfo | sqlite3.c:171189 | `if( pOrderBy->a[i].fg.sortFlags & KEYINFO_ORDER_BIGNULL ) break;` | 3496,14501 | both |
| allocateIndexInfo | sqlite3.c:171192 | `if( pExpr->op==TK_COLUMN && pExpr->iTable==pSrc->iCursor ){` | 12201,2300,11185,1016 | both |
| allocateIndexInfo | sqlite3.c:171199 | `if( pExpr->op==TK_COLLATE` | 228,3088 | both |
| allocateIndexInfo | sqlite3.c:171200 | `&& (pE2 = pExpr->pLeft)->op==TK_COLUMN` | 226,2 | both |
| allocateIndexInfo | sqlite3.c:171201 | `&& pE2->iTable==pSrc->iCursor` | 226,0 | **no** |
| allocateIndexInfo | sqlite3.c:171208 | `if( pE2->iColumn<0 ) continue;  /* Collseq does not matter for rowid */` | 2,224 | both |
| allocateIndexInfo | sqlite3.c:171210 | `if( zColl==0 ) zColl = sqlite3StrBINARY;` | 224,0 | **no** |
| allocateIndexInfo | sqlite3.c:171211 | `if( sqlite3_stricmp(pExpr->u.zToken, zColl)==0 ) continue;` | 218,6 | both |
| allocateIndexInfo | sqlite3.c:171217 | `if( i==n ){` | 7633,6592 | both |
| allocateIndexInfo | sqlite3.c:171220 | `if( (pWInfo->wctrlFlags & WHERE_DISTINCTBY) && !pSrc->fg.rowidUsed ){` | 673,6960,607,66 | both |
| allocateIndexInfo | sqlite3.c:171222 | `}else if( pWInfo->wctrlFlags & WHERE_GROUPBY ){` | 453,6573 | both |
| allocateIndexInfo | sqlite3.c:171224 | `}else if( pWInfo->wctrlFlags & WHERE_WANT_DISTINCT ){` | 438,6135 | both |
| allocateIndexInfo | sqlite3.c:171236 | `if( pIdxInfo==0 ){` | 0,46949 | **no** |
| allocateIndexInfo | sqlite3.c:171248 | `if( HasRowid(pTab)==0 ){` | 0,46949 | **no** |
| allocateIndexInfo | sqlite3.c:171253 | `for(i=0; i<pPk->nKeyCol; i++){` | never,never | **no** |
| allocateIndexInfo | sqlite3.c:171256 | `if( iCol>=BMS-1 ) iCol = BMS-1;` | never,never | **no** |
| allocateIndexInfo | sqlite3.c:171264 | `for(p=pWC, i=j=0; p; p=p->pOuter){` | 49169,46949 | both |
| allocateIndexInfo | sqlite3.c:171266 | `for(pTerm=p->a; i<nLast; i++, pTerm++){` | 153659,49169 | both |
| allocateIndexInfo | sqlite3.c:171268 | `if( (pTerm->wtFlags & TERM_OK)==0 ) continue;` | 42021,111638 | both |
| allocateIndexInfo | sqlite3.c:171272 | `if( op==WO_IN ){` | 4702,106936 | both |
| allocateIndexInfo | sqlite3.c:171273 | `if( (pTerm->wtFlags & TERM_SLICE)==0 ){` | 4698,4 | both |
| allocateIndexInfo | sqlite3.c:171274 | `pHidden->mIn \|= SMASKBIT32(j);` | 4698,0 | **no** |
| allocateIndexInfo | sqlite3.c:171278 | `if( op==WO_AUX ){` | 25981,85657 | both |
| allocateIndexInfo | sqlite3.c:171280 | `}else if( op & (WO_ISNULL\|WO_IS) ){` | 3638,82019 | both |
| allocateIndexInfo | sqlite3.c:171281 | `if( op==WO_ISNULL ){` | 1426,2212 | both |
| allocateIndexInfo | sqlite3.c:171298 | `if( op & (WO_LT\|WO_LE\|WO_GT\|WO_GE)` | 21618,60401 | both |
| allocateIndexInfo | sqlite3.c:171299 | `&& sqlite3ExprIsVector(pTerm->pExpr->pRight)` | 10,21608 | both |
| allocateIndexInfo | sqlite3.c:171302 | `if( j<16 ) mNoOmit \|= (1 << j);` | 10,0 | **no** |
| allocateIndexInfo | sqlite3.c:171303 | `if( op==WO_LT ) pIdxCons[j].op = WO_LE;` | 2,8 | both |
| allocateIndexInfo | sqlite3.c:171304 | `if( op==WO_GT ) pIdxCons[j].op = WO_GE;` | 4,6 | both |
| allocateIndexInfo | sqlite3.c:171313 | `for(i=j=0; i<nOrderBy; i++){` | 10205,46949 | both |
| allocateIndexInfo | sqlite3.c:171315 | `if( sqlite3ExprIsConstant(0, pExpr) ) continue;` | 2,10203 | both |
| freeIdxStr | sqlite3.c:171333 | `if( pIdxInfo->needToFreeIdxStr ){` | 608,51258 | both |
| freeIndexInfo | sqlite3.c:171351 | `for(i=0; i<pIdxInfo->nConstraint; i++){` | 111638,46949 | both |
| vtabBestIndex | sqlite3.c:171387 | `if( rc!=SQLITE_OK && rc!=SQLITE_CONSTRAINT ){` | 5392,49352,1672,3720 | both |
| vtabBestIndex | sqlite3.c:171388 | `if( rc==SQLITE_NOMEM ){` | 0,1672 | **no** |
| vtabBestIndex | sqlite3.c:171390 | `}else if( !pVtab->zErrMsg ){` | 0,1672 | **no** |
| vtabBestIndex | sqlite3.c:171396 | `if( pTab->u.vtab.p->bAllSchemas ){` | 0,54744 | **no** |
| whereLoopXfer | sqlite3.c:172288 | `if( pFrom->nLTerm > pTo->nLSlot` | 1992,95007 | both |
| whereLoopXfer | sqlite3.c:172289 | `&& whereLoopResize(db, pTo, pFrom->nLTerm)` | 0,1992 | **no** |
| whereLoopXfer | sqlite3.c:172296 | `if( pFrom->wsFlags & WHERE_VIRTUALTABLE ){` | 44275,52724 | both |
| whereLoopXfer | sqlite3.c:172298 | `}else if( (pFrom->wsFlags & WHERE_AUTO_INDEX)!=0 ){` | 20012,32712 | both |
| isLimitTerm | sqlite3.c:174029 | `&& pTerm->eMatchOp<=SQLITE_INDEX_CONSTRAINT_OFFSET;` | 4163,86202,4163,0 | **no** |
| allConstraintsUsed | sqlite3.c:174041 | `for(ii=0; ii<nCons; ii++){` | 4421,965 | both |
| allConstraintsUsed | sqlite3.c:174042 | `if( aUsage[ii].argvIndex<=0 ) return 0;` | 948,3473 | both |
| whereLoopAddVirtualOne | sqlite3.c:174096 | `for(i=0; i<nConstraint; i++, pIdxCons++){` | 136526,54744 | both |
| whereLoopAddVirtualOne | sqlite3.c:174099 | `if( (pTerm->prereqRight & mUsable)==pTerm->prereqRight` | 130879,5647 | both |
| whereLoopAddVirtualOne | sqlite3.c:174100 | `&& (pTerm->eOperator & mExclude)==0` | 128603,2276 | both |
| whereLoopAddVirtualOne | sqlite3.c:174101 | `&& (pbRetryLimit \|\| !isLimitTerm(pTerm))` | 16965,111638,14963,2002 | both |
| whereLoopAddVirtualOne | sqlite3.c:174120 | `if( rc ){` | 5392,49352 | both |
| whereLoopAddVirtualOne | sqlite3.c:174121 | `if( rc==SQLITE_CONSTRAINT ){` | 3720,1672 | both |
| whereLoopAddVirtualOne | sqlite3.c:174138 | `for(i=0; i<nConstraint; i++, pIdxCons++){` | 125864,48155 | both |
| whereLoopAddVirtualOne | sqlite3.c:174140 | `if( (iTerm = pUsage[i].argvIndex - 1)>=0 ){` | 73401,52463 | both |
| whereLoopAddVirtualOne | sqlite3.c:174143 | `if( iTerm>=nConstraint` | 73400,1 | both |
| whereLoopAddVirtualOne | sqlite3.c:174144 | `\|\| j<0` | 73400,0 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174145 | `\|\| (pTerm = termFromWhereClause(pWC, j))==0` | 73400,0 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174146 | `\|\| pNew->aLTerm[iTerm]!=0` | 73400,0 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174147 | `\|\| pIdxCons->usable==0` | 0,73400 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174159 | `if( iTerm>mxTerm ) mxTerm = iTerm;` | 61572,11828 | both |
| whereLoopAddVirtualOne | sqlite3.c:174162 | `if( pUsage[i].omit ){` | 66898,6502 | both |
| whereLoopAddVirtualOne | sqlite3.c:174163 | `if( i<16 && ((1<<i)&mNoOmit)==0 ){` | 66898,0,66890,8 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174169 | `if( pTerm->eMatchOp==SQLITE_INDEX_CONSTRAINT_OFFSET ){` | 508,66390 | both |
| whereLoopAddVirtualOne | sqlite3.c:174173 | `if( SMASKBIT32(i) & pHidden->mHandleIn ){` | 73400,0,3,73397 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174175 | `}else if( (pTerm->eOperator & WO_IN)!=0 ){` | 2434,70963 | both |
| whereLoopAddVirtualOne | sqlite3.c:174192 | `if( isLimitTerm(pTerm) && (*pbIn \|\| !allConstraintsUsed(pUsage, i)) ){` | 2161,71239,1913,248,948,965 | both |
| whereLoopAddVirtualOne | sqlite3.c:174208 | `for(i=0; i<=mxTerm; i++){` | 68916,48155 | both |
| whereLoopAddVirtualOne | sqlite3.c:174209 | `if( pNew->aLTerm[i]==0 ){` | 0,68916 | **no** |
| whereLoopAddVirtualOne | sqlite3.c:174222 | `pNew->u.vtab.isOrdered = (i8)(pIdxInfo->orderByConsumed ?` | 1867,46288 | both |
| whereLoopAddVirtualOne | sqlite3.c:174231 | `if( pIdxInfo->idxFlags & SQLITE_INDEX_SCAN_UNIQUE ){` | 870,47285 | both |
| whereLoopAddVirtualOne | sqlite3.c:174237 | `if( pNew->u.vtab.needFree ){` | 650,47505 | both |
| sqlite3_vtab_collation | sqlite3.c:174266 | `if( iCons>=0 && iCons<pIdxInfo->nConstraint ){` | 4123,0,4123,0 | **no** |
| sqlite3_vtab_collation | sqlite3.c:174270 | `if( pX->pLeft ){` | 2893,1230 | both |
| sqlite3_vtab_collation | sqlite3.c:174273 | `zRet = (pC ? pC->zName : sqlite3StrBINARY);` | 2893,1230 | both |
| sqlite3_vtab_in | sqlite3.c:174285 | `u32 m = SMASKBIT32(iCons);` | 13,0 | **no** |
| sqlite3_vtab_in | sqlite3.c:174286 | `if( m & pHidden->mIn ){` | 6,7 | both |
| sqlite3_vtab_in | sqlite3.c:174287 | `if( bHandle==0 ){` | 0,6 | **no** |
| sqlite3_vtab_in | sqlite3.c:174289 | `}else if( bHandle>0 ){` | 3,3 | both |
| sqlite3_vtab_rhs_value | sqlite3.c:174311 | `if( iCons<0 \|\| iCons>=pIdxInfo->nConstraint ){` | 7,0,0,7 | **no** |
| sqlite3_vtab_rhs_value | sqlite3.c:174314 | `if( pH->aRhs[iCons]==0 ){` | 7,0 | **no** |
| sqlite3_vtab_rhs_value | sqlite3.c:174328 | `if( rc==SQLITE_OK && pVal==0 ){  /* IMP: R-19933-32160 */` | 7,0,4,3 | **no** |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174356 | `for(i=0; i<nDb; i++){` | never,never | **no** |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174359 | `if( DbMaskNonZero(pParse->writeMask) ){` | never,never | **no** |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174360 | `for(i=0; i<nDb; i++){` | never,never | **no** |
| whereLoopAddVirtual | sqlite3.c:174417 | `if( p==0 ) return SQLITE_NOMEM_BKPT;` | 0,46949 | **no** |
| whereLoopAddVirtual | sqlite3.c:174423 | `if( whereLoopResize(pParse->db, pNew, nConstraint) ){` | 0,46949 | **no** |
| whereLoopAddVirtual | sqlite3.c:174434 | `if( bRetry ){` | 1196,45753 | both |
| whereLoopAddVirtual | sqlite3.c:174446 | `if( rc==SQLITE_OK && ((mBest = (pNew->prereq & ~mPrereq))!=0 \|\| bIn) ){` | 45528,1421,41063,4465,2016,39047 | both |
| whereLoopAddVirtual | sqlite3.c:174454 | `if( bIn ){` | 2030,4451 | both |
| whereLoopAddVirtual | sqlite3.c:174460 | `if( mBestNoIn==0 ){` | 2006,24 | both |
| whereLoopAddVirtual | sqlite3.c:174468 | `while( rc==SQLITE_OK ){` | 10884,120 | both |
| whereLoopAddVirtual | sqlite3.c:174472 | `for(i=0; i<nConstraint; i++){` | 27048,10884 | both |
| whereLoopAddVirtual | sqlite3.c:174475 | `if( mThis>mPrev && mThis<mNext ) mNext = mThis;` | 5637,21411,4529,1108 | both |
| whereLoopAddVirtual | sqlite3.c:174478 | `if( mNext==ALLBITS ) break;` | 6361,4523 | both |
| whereLoopAddVirtual | sqlite3.c:174479 | `if( mNext==mBest \|\| mNext==mBestNoIn ) continue;` | 62,4461,12,50 | both |
| whereLoopAddVirtual | sqlite3.c:174484 | `if( pNew->prereq==mPrereq ){` | 46,4 | both |
| whereLoopAddVirtual | sqlite3.c:174486 | `if( bIn==0 ) seenZeroNoIN = 1;` | 8,38 | both |
| whereLoopAddVirtual | sqlite3.c:174493 | `if( rc==SQLITE_OK && seenZero==0 ){` | 6361,120,4467,1894 | both |
| whereLoopAddVirtual | sqlite3.c:174497 | `if( bIn==0 ) seenZeroNoIN = 1;` | 4417,50 | both |
| whereLoopAddVirtual | sqlite3.c:174503 | `if( rc==SQLITE_OK && seenZeroNoIN==0 ){` | 6231,250,52,6179 | both |
| whereInterstageHeuristic | sqlite3.c:176024 | `for(i=0; i<pWInfo->nLevel; i++){` | 31089,2 | both |
| whereInterstageHeuristic | sqlite3.c:176026 | `if( p==0 ) break;` | 180,30909 | both |
| whereInterstageHeuristic | sqlite3.c:176027 | `if( (p->wsFlags & WHERE_VIRTUALTABLE)!=0 ){` | 10903,20006 | both |
| whereInterstageHeuristic | sqlite3.c:176031 | `if( (p->wsFlags & (WHERE_COLUMN_EQ\|WHERE_COLUMN_NULL\|WHERE_COLUMN_IN))!=0 ){` | 0,20006 | **no** |
| whereInterstageHeuristic | sqlite3.c:176034 | `for(pLoop=pWInfo->pLoops; pLoop; pLoop=pLoop->pNextLoop){` | never,never | **no** |
| whereInterstageHeuristic | sqlite3.c:176035 | `if( pLoop->iTab!=iTab ) continue;` | never,never | **no** |
| whereInterstageHeuristic | sqlite3.c:176036 | `if( (pLoop->wsFlags & (WHERE_CONSTRAINT\|WHERE_AUTO_INDEX))!=0 ){` | never,never | **no** |

Branch lines: 250, with every outcome seen: 137, with an outcome never seen: 113.

### 3.1 The lines with an outcome never seen, by reason

| C function | line | source | outcome counts | reason |
|---|---|---|---|---|
| sqlite3IsShadowTableOf | sqlite3.c:129384 | `if( !IsVirtual(pTab) ) return 0;` | 0,19195 | sqlite3IsShadowTableOf is reached through sqlite3ShadowTableName only with a virtual table found (the other outcome in sqlite3ShadowTableName) |
| sqlite3IsShadowTableOf | sqlite3.c:129386 | `if( sqlite3_strnicmp(zName, pTab->zName, nName)!=0 ) return 0;` | 0,19195 | a table named <vtab>_<x> where the prefix comparison fails: not in the corpora |
| sqlite3IsShadowTableOf | sqlite3.c:129387 | `if( zName[nName]!='_' ) return 0;` | 0,19195 | see 129386 |
| sqlite3IsShadowTableOf | sqlite3.c:129389 | `if( pMod==0 ) return 0;` | 0,19195 | a persistent vtab whose module is not registered (see 163319) |
| sqlite3IsShadowTableOf | sqlite3.c:129390 | `if( pMod->pModule->iVersion<3 ) return 0;` | 0,19195 | a module with iVersion<3: fts5 and rtree are version 3+ (none of the build's table modules is older) |
| sqlite3IsShadowTableOf | sqlite3.c:129391 | `if( pMod->pModule->xShadowName==0 ) return 0;` | 0,19195 | a module without xShadowName: the build's non-eponymous modules (fts5, fts5vocab, rtree) - fts5vocab has none; covered for MarkAll only |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129410 | `if( pMod==0 ) return;` | 0,4285 | see 163319 |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129411 | `if( NEVER(pMod->pModule==0) ) return;` | 0,4285 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129413 | `if( pMod->pModule->xShadowName==0 ) return;` | 0,2452 | fts5vocab (no xShadowName) is the only such table module: the true outcome only in some runs |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129422 | `&& pOther->zName[nName]=='_'` | 1,0 | an ordinary table <vtab>X... without the '_' separator: not in the corpora |
| sqlite3MarkAllShadowTablesOf | sqlite3.c:129423 | `&& pMod->pModule->xShadowName(pOther->zName+nName+1)` | 0,1 | an ordinary table <vtab>_<suffix> whose suffix is not a shadow name: f_extra in vt_d is in another schema at MarkAll time |
| sqlite3ShadowTableName | sqlite3.c:129443 | `pTab = zCopy ? sqlite3FindTable(db, zCopy, 0) : 0;` | 23212,0 | OOM only (zCopy==0) |
| sqlite3ShadowTableName | sqlite3.c:129446 | `if( !IsVirtual(pTab) ) return 0;` | 0,19195 | a name <table>_x whose <table> is an ordinary table: not in the corpora |
| sqlite3GetVTable | sqlite3.c:162801 | `for(pVtab=pTab->u.vtab.p; pVtab && pVtab->db!=db; pVtab=pVtab->pNext);` | 191413,3,0,191413 | a VTable of another connection in the list (shared cache): not built |
| addModuleArgument | sqlite3.c:162972 | `if( pTable->u.vtab.nArg+3>=db->aLimit[SQLITE_LIMIT_COLUMN] ){` | 0,65840 | a CREATE VIRTUAL TABLE with ≥ SQLITE_LIMIT_COLUMN-3 module arguments: not in the corpora (the Rust branch is the same comparison) |
| addModuleArgument | sqlite3.c:162976 | `if( azModuleArg==0 ){` | 0,65840 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3VtabBeginParse | sqlite3.c:163003 | `if( pTable==0 ) return;` | 0,8561 | sqlite3StartTable failed (name error) for a CREATE VIRTUAL TABLE row: a schema row C loaded without error never fails |
| sqlite3VtabBeginParse | sqlite3.c:163026 | `if( pTable->u.vtab.azArg ){` | 8561,0 | the authorizer block: azArg is always set (the true outcome only) |
| addArgumentToVtab | sqlite3.c:163041 | `if( pParse->sArg.z && pParse->pNewTable ){` | 24221,8561,24221,0 | a CREATE VIRTUAL TABLE without an argument list ends with sArg.z==0: every corpus vtab has arguments |
| sqlite3VtabFinishParse | sqlite3.c:163057 | `if( pTab==0 ) return;` | 0,8561 | pNewTable==0 after a failed BeginParse (see 163003) |
| sqlite3VtabFinishParse | sqlite3.c:163061 | `if( pTab->u.vtab.nArg<1 ) return;` | 0,8561 | nArg<1: impossible after BeginParse added three arguments (ALWAYS-like) |
| sqlite3VtabFinishParse | sqlite3.c:163079 | `if( pEnd ){` | 4279,0 | the non-init branch (a real CREATE VIRTUAL TABLE: code generation, a statement kind the generator does not port; C runs it for the setup statements only, with pEnd always set) |
| sqlite3VtabFinishParse | sqlite3.c:163124 | `if( pOld ){` | 0,4282 | pOld!=0: only after an OOM in sqlite3HashInsert |
| vtabCallConstructor | sqlite3.c:163184 | `for(pCtx=db->pVtabCtx; pCtx; pCtx=pCtx->pPrior){` | 0,9594 | the pVtabCtx chain has more than one entry only when a constructor connects another virtual table (no module of the build does) |
| vtabCallConstructor | sqlite3.c:163185 | `if( pCtx->pTab==pTab ){` | never,never | recursive constructor call: no module of the build does it (Rust: Parse.vtab_ctx, same message) |
| vtabCallConstructor | sqlite3.c:163194 | `if( !zModuleName ){` | 0,9594 | OOM only (zModuleName allocation) |
| vtabCallConstructor | sqlite3.c:163199 | `if( !pVTable ){` | 0,9594 | OOM only (VTable allocation) |
| vtabCallConstructor | sqlite3.c:163225 | `if( rc==SQLITE_NOMEM ) sqlite3OomFault(db);` | 0,9594 | OOM only |
| vtabCallConstructor | sqlite3.c:163228 | `if( SQLITE_OK!=rc ){` | 0,9594 | a failing xConnect: the build's modules connect without error in the corpora (Rust: VtabConnect::Err) |
| vtabCallConstructor | sqlite3.c:163229 | `if( zErr==0 ){` | never,never | a failing xConnect without a message (see 163228) |
| vtabCallConstructor | sqlite3.c:163236 | `}else if( ALWAYS(pVTable->pVtab) ){` | 9594,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| vtabCallConstructor | sqlite3.c:163243 | `if( sCtx.bDeclared==0 ){` | 0,9594 | a constructor that does not call sqlite3_declare_vtab: no module of the build |
| vtabCallConstructor | sqlite3.c:163266 | `&& (i==0 \|\| zType[i-1]==' ')` | 1138,10625,1138,0 | "hidden" not preceded by a space ("xhidden"): no declared type of the build's modules |
| vtabCallConstructor | sqlite3.c:163267 | `&& (zType[i+6]=='\0' \|\| zType[i+6]==' ')` | 0,11763,never,never | "hidden" followed by other text ("hiddenx"): no declared type of the build's modules |
| vtabCallConstructor | sqlite3.c:163274 | `int nDel = 6 + (zType[i+6] ? 1 : 0);` | 0,11763 | "hidden" at the end only (all build modules write it last, e.g. "TEXT HIDDEN", "start hidden"): nDel 6 only |
| vtabCallConstructor | sqlite3.c:163278 | `if( zType[i]=='\0' && i>0 ){` | 11763,0,1138,10625 | "hidden" alone at i==0 never ends the type text with i>0 ("hidden" alone) - declared types of the build |
| sqlite3VtabCallConnect | sqlite3.c:163319 | `if( !pMod ){` | 0,3 | a persistent virtual table whose module is not registered: C can not create one (writable_schema is refused by the shell); Rust: "no such module: %s" from Host::vtab_module |
| sqlite3VtabCallConnect | sqlite3.c:163326 | `if( rc!=SQLITE_OK ){` | 0,3 | a failing xConnect at first use (see 163228) |
| sqlite3_declare_vtab | sqlite3.c:163444 | `}while( tokenType==TK_SPACE \|\| tokenType==TK_COMMENT );` | 9594,19188,0,19188 | comments / leading white space before CREATE in a declare text: none in the build's modules |
| sqlite3_declare_vtab | sqlite3.c:163445 | `if( tokenType!=aKeyword[i] ){` | 0,19188 | a declare text that does not start CREATE TABLE: no module of the build |
| sqlite3_declare_vtab | sqlite3.c:163453 | `if( !pCtx \|\| pCtx->bDeclared ){` | 9594,0,0,9594 | sqlite3_declare_vtab called twice / outside a constructor: no module of the build |
| sqlite3_declare_vtab | sqlite3.c:163472 | `if( SQLITE_OK==sqlite3RunParser(&sParse, zCreateTable) ){` | 9594,0 | a declare text that does not parse: no module of the build (Rust: unsupported, never happens with the engine's modules) |
| sqlite3_declare_vtab | sqlite3.c:163477 | `if( !pTab->aCol ){` | 9594,0 | a second declaration for a table with columns: only on a reconnect after a schema reset (C keeps the table; the generator rebuilds it per statement) |
| sqlite3_declare_vtab | sqlite3.c:163489 | `if( !HasRowid(pNew)` | 0,9594 | WITHOUT ROWID declared tables: no module of the build declares one (Rust: unsupported) |
| sqlite3_declare_vtab | sqlite3.c:163490 | `&& pCtx->pVTable->pMod->pModule->xUpdate!=0` | never,never | see 163489 |
| sqlite3_declare_vtab | sqlite3.c:163491 | `&& sqlite3PrimaryKeyIndex(pNew)->nKeyCol!=1` | never,never | see 163489 |
| sqlite3_declare_vtab | sqlite3.c:163498 | `if( pIdx ){` | 0,9594 | see 163489 (the PRIMARY KEY index of a WITHOUT ROWID declaration) |
| sqlite3_declare_vtab | sqlite3.c:163508 | `(sParse.zErrMsg ? "%s" : 0), sParse.zErrMsg);` | never,never | a declare text that does not parse (see 163472) |
| sqlite3_declare_vtab | sqlite3.c:163514 | `if( sParse.pVdbe ){` | 9594,0 | the nested parse never creates a VDBE for the build's declare texts? (it does in C: sParse.pVdbe is set by sqlite3StartTable's code generation; the false outcome is a declare text without CREATE TABLE code) |
| sqlite3VtabOverloadFunction | sqlite3.c:163776 | `if( NEVER(pExpr==0) ) return pDef;` | 0,10040 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3VtabOverloadFunction | sqlite3.c:163780 | `if( NEVER(pTab==0) ) return pDef;` | 0,6831 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3VtabOverloadFunction | sqlite3.c:163813 | `if( pNew==0 ){` | 0,2388 | OOM only |
| sqlite3VtabMakeWritable | sqlite3.c:163832 | `Parse *pToplevel = sqlite3ParseToplevel(pParse);` | 0,7341 | sqlite3VtabMakeWritable: called only by INSERT / UPDATE / DELETE code generation (statement kinds the generator does not port yet); ported for them |
| sqlite3VtabMakeWritable | sqlite3.c:163837 | `for(i=0; i<pToplevel->nVtabLock; i++){` | 0,7341 | see 163832 |
| sqlite3VtabMakeWritable | sqlite3.c:163838 | `if( pTab==pToplevel->apVtabLock[i] ) return;` | never,never | see 163832 |
| sqlite3VtabMakeWritable | sqlite3.c:163842 | `if( apVtabLock ){` | 7341,0 | see 163832 |
| sqlite3VtabEponymousTableInit | sqlite3.c:163874 | `if( pTab==0 ) return 0;` | 0,5312 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3VtabEponymousTableInit | sqlite3.c:163876 | `if( pTab->zName==0 ){` | 0,5312 | OOM only (zName allocation) |
| sqlite3VtabEponymousTableInit | sqlite3.c:163893 | `if( rc ){` | 0,5312 | a failing eponymous constructor: the build's eponymous modules (series, json, dbstat, pragma) connect without error |
| whereClauseInsert | sqlite3.c:167774 | `if( pWC->a==0 ){` | 0,840 | OOM only (whereClauseInsert reallocation failure) |
| whereClauseInsert | sqlite3.c:167775 | `if( wtFlags & TERM_DYNAMIC ){` | never,never | OOM only |
| whereClauseInsert | sqlite3.c:167786 | `if( p && ExprHasProperty(p, EP_Unlikely) ){` | 206261,0,0,206261 | likelihood()/unlikely() over a vtab term: shared with V2 (exercised by the V2/V3 corpora) |
| isAuxiliaryVtabOperator | sqlite3.c:168122 | `if( pList==0 \|\| pList->nExpr!=2 ){` | 9174,0,2,9172 | a TK_FUNCTION term with other than two arguments over a vtab column (vt_e has like(…,3 args)); the other outcome is the 2-argument functions |
| isAuxiliaryVtabOperator | sqlite3.c:168135 | `if( ExprIsVtab(pCol) && (i = sqlite3ExprIsLikeOperator(pExpr))!=0 ){` | 9168,4,9168,0,9168,0 | second argument a vtab column but not like/glob/match/regexp (e.g. instr(x, col)): not in the corpora |
| isAuxiliaryVtabOperator | sqlite3.c:168155 | `if( ExprIsVtab(pCol) ){` | 4,0,4,0 | see 168135 |
| isAuxiliaryVtabOperator | sqlite3.c:168165 | `if( pMod->xFindFunction!=0 ){` | 0,4 | a vtab whose module has no xFindFunction as first argument of a 2-argument function in WHERE: not in the corpora |
| isAuxiliaryVtabOperator | sqlite3.c:168167 | `if( i>=SQLITE_INDEX_CONSTRAINT_FUNCTION ){` | never,never | xFindFunction returning ≥ SQLITE_INDEX_CONSTRAINT_FUNCTION: only geopoly does; not in either build |
| whereAddLimitExpr | sqlite3.c:169379 | `if( pVal==0 ) return;` | 0,4929 | OOM only (allocation failure; the port has no allocation failure path) |
| whereAddLimitExpr | sqlite3.c:169383 | `if( pVal==0 ) return;` | 0,5262 | OOM only (allocation failure; the port has no allocation failure path) |
| whereAddLimitExpr | sqlite3.c:169387 | `if( pNew ){` | 10191,0 | OOM only |
| sqlite3WhereAddLimit | sqlite3.c:169442 | `if( pWC->a[ii].leftCursor==iCsr && pWC->a[ii].prereqRight==0 ) continue;` | 21121,582,21121,0 | a term on the single vtab with prereqRight!=0: impossible with one FROM item except a correlated outer column (not in the corpora) |
| sqlite3WhereAddLimit | sqlite3.c:169446 | `if( pWC->a[ii].iParent>=0 ){` | 0,582 | a WHERE term with a parent that is not passed through to xBestIndex: not in the corpora |
| sqlite3WhereAddLimit | sqlite3.c:169448 | `if( pParent->leftCursor==iCsr` | never,never | see 169446 |
| sqlite3WhereAddLimit | sqlite3.c:169449 | `&& pParent->prereqRight==0` | never,never | see 169446 |
| sqlite3WhereAddLimit | sqlite3.c:169450 | `&& pParent->nChild==1` | never,never | a parent with more than one child (BETWEEN) is caught earlier by nChild |
| sqlite3WhereTabFuncArgs | sqlite3.c:169657 | `if( !IsVirtual(pTab) ){` | 0,27601 | function syntax on an ordinary table fails in selectExpander (cannotBeFunction) before the WHERE analysis |
| sqlite3WhereTabFuncArgs | sqlite3.c:169673 | `if( pColRef==0 ) return;` | 0,44661 | OOM only (allocation failure; the port has no allocation failure path) |
| termFromWhereClause | sqlite3.c:171102 | `for(p=pWC; p; p=p->pOuter){` | 255352,0 | termFromWhereClause past the end of the chain (returns 0): never by construction |
| allocateIndexInfo | sqlite3.c:171201 | `&& pE2->iTable==pSrc->iCursor` | 226,0 | ORDER BY x COLLATE c where x is a column of another table: not in the corpora |
| allocateIndexInfo | sqlite3.c:171210 | `if( zColl==0 ) zColl = sqlite3StrBINARY;` | 224,0 | zColl==0: sqlite3ColumnColl returns 0 only for a column without COLLATE; the build's modules never declare a COLLATE, so the true outcome only |
| allocateIndexInfo | sqlite3.c:171236 | `if( pIdxInfo==0 ){` | 0,46949 | OOM only (allocation failure; the port has no allocation failure path) |
| allocateIndexInfo | sqlite3.c:171248 | `if( HasRowid(pTab)==0 ){` | 0,46949 | WITHOUT ROWID virtual tables: no module of the build declares one |
| allocateIndexInfo | sqlite3.c:171253 | `for(i=0; i<pPk->nKeyCol; i++){` | never,never | see 171248 |
| allocateIndexInfo | sqlite3.c:171256 | `if( iCol>=BMS-1 ) iCol = BMS-1;` | never,never | see 171248 |
| allocateIndexInfo | sqlite3.c:171274 | `pHidden->mIn \|= SMASKBIT32(j);` | 4698,0 | SMASKBIT32(n) macro (n<=31 ? 1<<n : 0): the n>31 outcome (a 33rd constraint) is never seen; the other condition of the line is covered (vt_m for mHandleIn) |
| allocateIndexInfo | sqlite3.c:171302 | `if( j<16 ) mNoOmit \|= (1 << j);` | 10,0 | a vector range constraint at constraint index ≥16: not in the corpora |
| vtabBestIndex | sqlite3.c:171388 | `if( rc==SQLITE_NOMEM ){` | 0,1672 | OOM only (SQLITE_NOMEM from xBestIndex) |
| vtabBestIndex | sqlite3.c:171390 | `}else if( !pVtab->zErrMsg ){` | 0,1672 | a failing xBestIndex without zErrMsg: the build's modules always set a message (Rust: sqlite3_err_str) |
| vtabBestIndex | sqlite3.c:171396 | `if( pTab->u.vtab.p->bAllSchemas ){` | 0,54744 | SQLITE_VTAB_USES_ALL_SCHEMAS: only sqlite_dbpage sets it; not in either build |
| whereLoopXfer | sqlite3.c:172289 | `&& whereLoopResize(db, pTo, pFrom->nLTerm)` | 0,1992 | OOM only (whereLoopResize) |
| isLimitTerm | sqlite3.c:174029 | `&& pTerm->eMatchOp<=SQLITE_INDEX_CONSTRAINT_OFFSET;` | 4163,86202,4163,0 | isLimitTerm on a WO_AUX term with eMatchOp > OFFSET (a FUNCTION constraint): see 168167 |
| whereLoopAddVirtualOne | sqlite3.c:174144 | `\|\| j<0` | 73400,0 | xBestIndex malfunction variants (negative term offset, unknown term, used slot, unusable constraint): vt_m covers the argv gap only |
| whereLoopAddVirtualOne | sqlite3.c:174145 | `\|\| (pTerm = termFromWhereClause(pWC, j))==0` | 73400,0 | see 174144 |
| whereLoopAddVirtualOne | sqlite3.c:174146 | `\|\| pNew->aLTerm[iTerm]!=0` | 73400,0 | see 174144 |
| whereLoopAddVirtualOne | sqlite3.c:174147 | `\|\| pIdxCons->usable==0` | 0,73400 | see 174144 |
| whereLoopAddVirtualOne | sqlite3.c:174163 | `if( i<16 && ((1<<i)&mNoOmit)==0 ){` | 66898,0,66890,8 | an omitted constraint in mNoOmit (vector range) or at index ≥16: not in the corpora |
| whereLoopAddVirtualOne | sqlite3.c:174173 | `if( SMASKBIT32(i) & pHidden->mHandleIn ){` | 73400,0,3,73397 | SMASKBIT32(n) macro (n<=31 ? 1<<n : 0): the n>31 outcome (a 33rd constraint) is never seen; the other condition of the line is covered (vt_m for mHandleIn) |
| whereLoopAddVirtualOne | sqlite3.c:174209 | `if( pNew->aLTerm[i]==0 ){` | 0,68916 | a gap in the argv slots: vt_m (vtbad) covers it |
| sqlite3_vtab_collation | sqlite3.c:174266 | `if( iCons>=0 && iCons<pIdxInfo->nConstraint ){` | 4123,0,4123,0 | sqlite3_vtab_collation with an out-of-range constraint index: no module does |
| sqlite3_vtab_in | sqlite3.c:174285 | `u32 m = SMASKBIT32(iCons);` | 13,0 | SMASKBIT32(n) macro (n<=31 ? 1<<n : 0): the n>31 outcome (a 33rd constraint) is never seen; the other condition of the line is covered (vt_m for mHandleIn) |
| sqlite3_vtab_in | sqlite3.c:174287 | `if( bHandle==0 ){` | 0,6 | sqlite3_vtab_in(…,0): no module of the build or of vt_m calls it |
| sqlite3_vtab_rhs_value | sqlite3.c:174311 | `if( iCons<0 \|\| iCons>=pIdxInfo->nConstraint ){` | 7,0,0,7 | sqlite3_vtab_rhs_value with an out-of-range index: no module does |
| sqlite3_vtab_rhs_value | sqlite3.c:174314 | `if( pH->aRhs[iCons]==0 ){` | 7,0 | a second rhs_value call for the same constraint: no module does |
| sqlite3_vtab_rhs_value | sqlite3.c:174328 | `if( rc==SQLITE_OK && pVal==0 ){  /* IMP: R-19933-32160 */` | 7,0,4,3 | rhs_value on a constraint whose RHS is not a constant: vt_m calls it on constants only |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174356 | `for(i=0; i<nDb; i++){` | never,never | sqlite3VtabUsesAllSchemas: see 171396 |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174359 | `if( DbMaskNonZero(pParse->writeMask) ){` | never,never | see 171396 |
| sqlite3VtabUsesAllSchemas | sqlite3.c:174360 | `for(i=0; i<nDb; i++){` | never,never | see 171396 |
| whereLoopAddVirtual | sqlite3.c:174417 | `if( p==0 ) return SQLITE_NOMEM_BKPT;` | 0,46949 | OOM only (allocation failure; the port has no allocation failure path) |
| whereLoopAddVirtual | sqlite3.c:174423 | `if( whereLoopResize(pParse->db, pNew, nConstraint) ){` | 0,46949 | OOM only (whereLoopResize) |
| whereInterstageHeuristic | sqlite3.c:176031 | `if( (p->wsFlags & (WHERE_COLUMN_EQ\|WHERE_COLUMN_NULL\|WHERE_COLUMN_IN))!=0 ){` | 0,20006 | whereInterstageHeuristic: shared with V2 (btree loops); the vtab break is in section 1.1 |
| whereInterstageHeuristic | sqlite3.c:176034 | `for(pLoop=pWInfo->pLoops; pLoop; pLoop=pLoop->pNextLoop){` | never,never | see 176031 |
| whereInterstageHeuristic | sqlite3.c:176035 | `if( pLoop->iTab!=iTab ) continue;` | never,never | see 176031 |
| whereInterstageHeuristic | sqlite3.c:176036 | `if( (pLoop->wsFlags & (WHERE_CONSTRAINT\|WHERE_AUTO_INDEX))!=0 ){` | never,never | see 176031 |

