# V4a decision parity (karar eşliği): window functions in the code generator

Scope: step V4a of the port of C SQLite 3.54.1's code generator: window.c (rewrite, frames, named / chained windows,
EXCLUDE, built-in window functions as the generator sees them, sqlite3WindowCodeInit / CodeStep and all helpers), the
window grammar rules (parse.y 93, 318–345) and the window parts of resolve.c / select.c / expr.c / walker.c. Oracle:
`target/perf-work/bin/sqlite3` and the amalgamation `target/sqlite-ref/amalg` (instrumented copies in `v4a/wdump/`
and `v4a/cov/`, scratch only). Worktree `.claude/worktrees/vdbe` (branch `performans-A-vdbe`, uncommitted on top of
09b2210, Rust 1.100). Binary of every number below: `$S/v4_bin/sqliters.r2` (= the final `$S/v4_bin/sqliters`).
The engine's executor and fast paths still execute every statement; the generator's program is what EXPLAIN / EQP
print, and what the machine runs under the diagnostic switch SQLITERS_VDBE_EXEC=1.

## 0. How a decision is proven: the layers

EXPLAIN / EQP: statements / accepted by the generator / identical / different (`expdiff.py`). Dump layers (`v4a/wdiff.py`):
identical / statements with a dump; `win` = the Window objects at sqlite3WindowCodeStep() (every field of each Window
of the list, plus eDelete and regArg of the WindowCodeArg context: C `v4aDumpWindows()` of `wdump/patch_v4a.py`, port
`window.rs dump_windows`). Schema layer: identical schemas / schemas (`sdiff.py`).

| corpus | EXPLAIN | EQP | terms | loops | paths | agg | win | schema |
|---|---|---|---|---|---|---|---|---|
| win_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 8261 / 8261 | 8261 / 8261 | 8261 / 8261 | 926 / 926 | 8231 / 8231 | 300 / 300 |
| win_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 4147 / 4147 | 4147 / 4147 | 4147 / 4147 | 508 / 508 | 4131 / 4131 | 150 / 150 |
| win_c | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 4526 / 4526 | 4526 / 4526 | 4526 / 4526 | 818 / 818 | 4460 / 4460 | 150 / 150 |
| win_d | 12 / 12 / 12 / 0 | 12 / 12 / 12 / 0 | 11 / 11 | 11 / 11 | 11 / 11 | 6 / 6 | 11 / 11 | 1 / 1 |
| ddl_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10523 / 10523 | 10306 / 10306 | 10323 / 10323 | 2090 / 2090 | 0 / 0 | 400 / 400 |
| plan_a | 26000 / 26000 / 26000 / 0 | 26000 / 26000 / 26000 / 0 | 23583 / 23583 | 23196 / 23196 | 23246 / 23246 | 6054 / 6054 | 0 / 0 | 520 / 520 |
| plan_b | 13000 / 13000 / 13000 / 0 | 13000 / 13000 / 13000 / 0 | 11884 / 11884 | 11698 / 11698 | 11718 / 11718 | 3088 / 3088 | 0 / 0 | 260 / 260 |
| agg_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10531 / 10531 | 10337 / 10337 | 10342 / 10342 | 10048 / 10048 | 0 / 0 | 300 / 300 |
| agg_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 5259 / 5259 | 5168 / 5168 | 5172 / 5172 | 4999 / 4999 | 0 / 0 | 150 / 150 |
| agg_c | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2661 / 2661 | 2613 / 2613 | 2615 / 2615 | 2558 / 2558 | 0 / 0 | 100 / 100 |
| cte_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 9045 / 9045 | 8965 / 8965 | 8977 / 8977 | 1804 / 1804 | 0 / 0 | 300 / 300 |
| cte_b | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2280 / 2280 | 2261 / 2261 | 2264 / 2264 | 395 / 395 | 0 / 0 | 100 / 100 |

Window corpora (reproducible, `v4a/gen_win.py`): win_a = 7101 300 40, win_b = 7202 150 40, win_c = 7401 150 40
(second generation: scalar sub-selects of a window SELECT, IF_NULL_ROW terminals, outer aggregates inside window
sub-selects, WINDOW clauses copied by SelectDup, windows in VALUES, push-down restriction 6c), exec corpora win_x =
7301 100 40 --exec and win_y = 7502 60 40 --exec (tables with rows); win_d = hand-written statements for the branch
outcomes the generated corpora missed (section 3). The V3 corpora are those of `v3/corpus/`.

| other layer | tool | result |
|---|---|---|
| result rows, machine (SQLITERS_VDBE_EXEC=1), win_x | `win_x_rows_r2.txt` | statements 4000 ; machine   identical   2789   different 2 ; fallback  identical      0   different 0 ; none      identical   1175   different 34 |
| result rows, machine, win_y | `win_y_rows_r2.txt` | statements 2400 ; machine   identical   1817   different 0 ; fallback  identical      0   different 0 ; none      identical    566   different 17 |
| all statement kinds, EXPLAIN (statements executed in between) | `all_kinds_r2.txt` | statements 39300 · generator accepted 14532: identical 14177, DIFFERENT 355 · unsupported (fallback) 24366 (of which same as C anyway 4657) · not reached (old parser/other) 402 (same 342) |
| same, EXPLAIN QUERY PLAN | `all_kinds_eqp_r2.txt` | statements 39300 · generator accepted 14532: identical 14260, DIFFERENT 272 · unsupported (fallback) 24366 (of which same as C anyway 21702) · not reached (old parser/other) 402 (same 342) |
| library level, no schema (V1 corpora) | `cgdiff_r2.txt` | total 54613  identical 54612  DIFFERENT 0  unsupported 1 |
| all-kinds statements the generator still reports unsupported that contain a window | `all_kinds_unsup_r2.txt` | 60 statements whose text matches OVER / WINDOW / FILTER (some only in a string literal), all of class "statement kind (grammar rule not ported)": CREATE TRIGGER bodies and DML (V3: 409 "window function"; 399 of them are now accepted and identical) |
| attribution of the row differences | `win_x_rows_r2.json`, `win_y_rows_r2.json` | machine 2 = D30 (engine storage of a WITHOUT ROWID table, pre-existing); engine path 46 = D31 (window name quoting) and 5 = D32 (error precedence); win_d: 2 = D21 (V3) |
| dmlfuzz 6001 400 (--no-api) | `dmlfuzz_r2.txt` | 378 identical / 22 different: the same seed set and classes as V3 r9 and as the Part 0 baseline |
| finder (245 scenarios) | `finder_r2.txt` | levels = h7b; 1 stderr-only difference t/g06_vacuum_into_main_untouched (environmental, also in V3 and Part 0) |
| workspace tests | `test_ws_r2_off.txt` / `_on.txt` | off 1002 passed / 4 failed (the known vtab *_differential_parity) / 337 ignored; on (SQLITERS_VDBE_EXEC=1) 996 / 10 (the 10 of gorulen_farklar_m2.md) / 337 |
| codegen unit tests (C-produced expectations) | `cargo test -p sqliters-codegen` | 32 passed (3 new: window_aggregate_listing, window_objects_dump, window_function_without_from) |

## 1. C function → Rust item (functions ported or completed in V4a)
Paths under `crates/`. cov = C lines executed % / branches taken at least once % (gcov over the accepted statements of
win_a + win_b + win_c + win_x + win_y, each as EXPLAIN and EQP); untaken = source lines with a branch outcome never seen.

| C function | file | Rust item | cov (lines / branches) | untaken |
|---|---|---|---|---|
| `windowFind` | window.c | `window_find` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowUpdate` | window.c | `sqlite3_window_update` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `selectWindowRewriteExprCb` | window.c | `select_window_rewrite_expr_cb` (`sqliters-codegen/src/window.rs`) | 100.0 / 87.8 | 5 |
| `selectWindowRewriteSelectCb` | window.c | `select_window_rewrite_select_cb` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `selectWindowRewriteEList` | window.c | `select_window_rewrite_e_list` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `exprListAppendList` | window.c | `expr_list_append_list` (`sqliters-codegen/src/window.rs`) | 88.9 / 85.7 | 2 |
| `sqlite3WindowExtraAggFuncDepth` | window.c | `sqlite3_window_extra_agg_func_depth` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `disallowAggregatesInOrderByCb` | window.c | `disallow_aggregates_in_order_by_cb` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowRewrite` | window.c | `sqlite3_window_rewrite` (`sqliters-codegen/src/window.rs`) | 97.7 / 85.7 | 6 |
| `sqlite3WindowUnlinkFromSelect` | window.c | `sqlite3_window_unlink_from_select` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowOffsetExpr` | window.c | `sqlite3_window_offset_expr` (`sqliters-codegen/src/window.rs`) | 100.0 / 75.0 | 1 |
| `sqlite3WindowAlloc` | window.c | `sqlite3_window_alloc` (`sqliters-codegen/src/window.rs`) | 96.2 / 88.9 | 2 |
| `sqlite3WindowAssemble` | window.c | `sqlite3_window_assemble` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowChain` | window.c | `sqlite3_window_chain` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowAttach` | window.c | `sqlite3_window_attach` (`sqliters-codegen/src/window.rs`) | 88.9 / 83.3 | 1 |
| `sqlite3WindowLink` | window.c | `sqlite3_window_link` (`sqliters-codegen/src/window.rs`) | 100.0 / 90.0 | 1 |
| `sqlite3WindowCompare` | window.c | `sqlite3_window_compare` (`sqliters-codegen/src/window.rs`) | 100.0 / 91.7 | 1 |
| `sqlite3WindowCodeInit` | window.c | `sqlite3_window_code_init` (`sqliters-codegen/src/window.rs`) | 100.0 / 95.5 | 1 |
| `windowCheckValue` | window.c | `window_check_value` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowArgCount` | window.c | `window_arg_count` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowReadPeerValues` | window.c | `window_read_peer_values` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowAggStep` | window.c | `window_agg_step` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowAggFinal` | window.c | `window_agg_final` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowFullScan` | window.c | `window_full_scan` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowReturnOneRow` | window.c | `window_return_one_row` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowInitAccum` | window.c | `window_init_accum` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowCacheFrame` | window.c | `window_cache_frame` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowIfNewPeer` | window.c | `window_if_new_peer` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowCodeRangeTest` | window.c | `window_code_range_test` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `windowCodeOp` | window.c | `window_code_op` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowDup` | window.c | `sqlite3_window_dup` (`sqliters-codegen/src/window.rs`) | 100.0 / 50.0 | 2 |
| `sqlite3WindowListDup` | window.c | `sqlite3_window_list_dup` (`sqliters-codegen/src/window.rs`) | 100.0 / 75.0 | 1 |
| `windowExprGtZero` | window.c | `window_expr_gt_zero` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3WindowCodeStep` | window.c | `sqlite3_window_code_step` (`sqliters-codegen/src/window.rs`) | 100.0 / 100.0 | 0 |
| `agginfoPersistExprCb` | expr.c | `agginfo_persist_expr_cb` (`sqliters-codegen/src/window.rs`) | 100.0 / 63.6 | 6 |
| `gatherSelectWindowsCallback` | expr.c | `gather_select_windows_callback` (`sqliters-codegen/src/expr.rs`) | 100.0 / 100.0 | 0 |
| `gatherSelectWindowsSelectCallback` | expr.c | `gather_select_windows_select_callback` (`sqliters-codegen/src/expr.rs`) | 100.0 / 100.0 | 0 |
| `gatherSelectWindows` | expr.c | `gather_select_windows` (`sqliters-codegen/src/expr.rs`) | 100.0 / 100.0 | 0 |
| `sqlite3ExprAddFunctionOrderBy` | expr.c | `sqlite3_expr_add_function_order_by` (`sqliters-codegen/src/expr.rs`) | 36.0 / 37.5 | 6 |
| `resolveRemoveWindowsCb` | resolve.c | `resolve_remove_windows_cb` (`sqliters-codegen/src/resolve.rs`) | 100.0 / 100.0 | 0 |
| `windowRemoveExprFromSelect` | resolve.c | `window_remove_expr_from_select` (`sqliters-codegen/src/resolve.rs`) | 100.0 / 100.0 | 0 |
| `pushDownWindowCheck` | select.c | `push_down_window_check` (`sqliters-codegen/src/flatten.rs`) | 100.0 / 100.0 | 0 |
| `multiSelectValues` | select.c | `multi_select_values` (`sqliters-codegen/src/select.rs`) | 56.2 / 30.0 | 4 |
| `walkWindowList` | walker.c | `walk_window_list` (`sqliters-codegen/src/walker.rs`) | 100.0 / 78.6 | 3 |

## 2. Decision tables (C source → rule → Rust)

Every rule below is a decision of C's window.c (or of the window parts of parse.y / resolve.c / select.c / expr.c /
walker.c) and the place of its 1:1 counterpart. "Layer" says which differential proves the outcome; "both" in the
coverage column means gcov saw both outcomes of the C condition over the window corpora (section 3 lists every branch
line).

### 2.1 Parser: window definitions, frames, OVER (parse.y rules 93, 318–345) — `parse.rs`, `window.rs`  *(layer: EXPLAIN bytes / error text+offset)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| parse.y:656 rule 93 | `SELECT … window_clause …` → `sqlite3SelectNew`, then `A->pWinDefn = R` | parse.rs rule 93 |
| parse.y:1997 rule 318 | `windowdefn_list , windowdefn` → `sqlite3WindowChain(Z, Y)` **before** `Z->pNextWin = Y` (a definition can only chain to an earlier one; list is newest-first) | parse.rs 318 |
| parse.y:2006 rule 319 | window name = raw token text (`sqlite3DbStrNDup(X.z, X.n)`: quotes kept, not dequoted) | parse.rs 319 (`tok_str`) |
| parse.y:2040–2053 rules 320–324 | `[nm] [PARTITION BY …] [ORDER BY …] frame_opt` → `sqlite3WindowAssemble(Z, part, ob, &W or 0)`; `zBase` = raw token | parse.rs 320–324, window.rs `sqlite3_window_assemble` |
| parse.y:2057 rule 325 | empty frame → `sqlite3WindowAlloc(0, UNBOUNDED, 0, CURRENT, 0, 0)` (implicit frame) | parse.rs 325 |
| parse.y:2060 rule 326 | one bound → end = CURRENT ROW | parse.rs 326 |
| parse.y:2071–2077 rules 330–334 | `UNBOUNDED PRECEDING|FOLLOWING` / `CURRENT ROW` → `{eType=@X, pExpr=0}`; `expr PRECEDING|FOLLOWING` → `{eType=@Y, pExpr=X}` | parse.rs 330–334 |
| parse.y:2084–2086 rules 337–339 | EXCLUDE NO OTHERS / CURRENT ROW / GROUP / TIES → TK_NO / TK_CURRENT / TK_GROUP / TK_TIES | parse.rs 337–339 |
| parse.y:2093 rule 341 | `FILTER (…) OVER …` → `O->pFilter = F` | parse.rs 341 |
| parse.y:2104 rule 343 | FILTER without OVER → Window with `eFrmType = TK_FILTER` (ordinary aggregate) | parse.rs 343 (V3) |
| parse.y:2118 rule 345 | `OVER nm` → Window with only `zName` (eFrmType 0: resolved later by sqlite3WindowUpdate) | parse.rs 345 |
| window.c:1202 | `eType==0` → `bImplicitFrame=1`, `eType=TK_RANGE` | `sqlite3_window_alloc` |
| window.c:1221 | `(CURRENT, PRECEDING)` or `(FOLLOWING, PRECEDING|CURRENT)` → "unsupported frame specification", no Window (both) | `sqlite3_window_alloc` |
| window.c:1233 | `eExclude==0` and optimization SQLITE_WindowFunc disabled → `eExclude=TK_NO` | `sqlite3_window_alloc` (`optimization_disabled`) |
| window.c:1170 | offset not constant (`sqlite3ExprIsConstant(0,…)`, no function table) → replaced by a TK_NULL node (both) | `sqlite3_window_offset_expr` (`sqlite3_expr_is_constant_no_parse`) |
| window.c:1322 | `DISTINCT` and `eFrmType!=TK_FILTER` → "DISTINCT is not supported for window functions" | `sqlite3_window_attach` |
| expr.c:1262 | ORDER BY inside the parentheses of a window function (`IsWindowFunc`) → sqlite3ExprOrderByAggregateError | expr.rs `sqlite3_expr_add_function_order_by` |

### 2.2 Name resolution of window functions (resolve.c 1155–1404, window.c 662–731, 1280–1353) — `resolve.rs`, `window.rs`  *(layer: error text+offset, win dump)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| resolve.c:1162 | `pWin = IsWindowFunc(pExpr) ? y.pWin : 0` (a TK_FILTER window is not a window function) | resolve.rs `p_win` |
| resolve.c:1281 | `pDef && xValue==0 && pWin` → "%T() may not be used as a window function" | resolve.rs |
| resolve.c:1286–1290 | `is_agg && !NC_AllowAgg`, or `is_agg && FUNC_WINDOW && !pWin`, or `is_agg && pWin && !NC_AllowWin` → "misuse of window/aggregate function"; type "window" iff FUNC_WINDOW or pWin | resolve.rs |
| resolve.c:1327 | `!is_agg && EP_WinFunc` (OVER/FILTER on a scalar function) → "FILTER may not be used with non-aggregate %T()" | resolve.rs |
| resolve.c:1345 | `is_agg` → clear NC_AllowWin, and NC_AllowAgg only when `!pWin` (aggregates may be arguments of window functions) | resolve.rs |
| resolve.c:1362 | `pWin && nErr==0` → sqlite3WindowUpdate(pWinSelect->pWinDefn), walk PARTITION / ORDER / FILTER, sqlite3WindowLink, NC_HasWin; else the TK_AGG_FUNCTION path | resolve.rs |
| window.c:668 | `zName && eFrmType==0` (OVER name) → copy partition / order / bounds / frame / exclude of the named window, or "no such window" (window.c:639) | `sqlite3_window_update`, `window_find` |
| window.c:680 | else → sqlite3WindowChain (OVER (base …)) | `sqlite3_window_chain` |
| window.c:1287–1293 | base found: `pPartition` → "PARTITION clause"; base and new both ORDER BY → "ORDER BY clause"; base frame explicit → "frame specification"; else copy partition (and base ORDER BY) and drop zBase | `sqlite3_window_chain` |
| window.c:682 | RANGE with an offset bound and not exactly one ORDER BY term → "RANGE with offset PRECEDING/FOLLOWING requires one ORDER BY expression" (checked after the chain; C keeps the last error message) | `sqlite3_window_update` |
| window.c:690–692 | built-in window function (`FUNC_WINDOW`) with FILTER → "FILTER clause may only be used with aggregate window functions" | `sqlite3_window_update` |
| window.c:702–726 | frame coercion: row_number ROWS(UNB,CUR); dense_rank / rank RANGE(UNB,CUR); percent_rank GROUPS(CUR,UNB); cume_dist GROUPS(1 FOLLOWING,UNB); ntile ROWS(CUR,UNB); lead ROWS(UNB,UNB); lag ROWS(UNB,CUR); exclude reset to 0; identity by `zName==xxxName` | `sqlite3_window_update` (`is_win_func`) |
| window.c:1340 | link into `pSel->pWin` iff the list is empty or `sqlite3WindowCompare(…)==0`; else `SF_MultiPart` when the PARTITION BY lists differ | `sqlite3_window_link` |
| window.c:1367–1384 | compare frame type, bounds, exclude, offsets, partition, order (and FILTER when bFilter) | `sqlite3_window_compare` |
| resolve.c:1877 | ORDER BY term equal to a result column → windowRemoveExprFromSelect (unlink its windows) | resolve.rs `window_remove_expr_from_select` |
| walker.c:141 | pWinDefn walked only for the fixer (`sqlite3WalkWinDefnDummyCallback`), rename, and `sqlite3SelectPopWith` walkers | walker.rs (`walk_win_defn`), select.rs / build_tab.rs |

### 2.3 SELECT rewrite (window.c 964–1120, select.c 7850) — `window.rs sqlite3_window_rewrite`  *(layer: EXPLAIN bytes, win dump nBuf/argCol/eph/accum/result)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| window.c:966 | `p->pWin && p->pPrior==0 && !SF_WinRewrite && !IN_RENAME_OBJECT` → rewrite; else no-op | `sqlite3_window_rewrite` |
| window.c:994 | not an aggregate SELECT → ORDER BY aggregates without pAggInfo are "misuse of aggregate: %s()" | `disallow_aggregates_in_order_by_cb` |
| window.c:1010–1020 | sub-select ORDER BY = PARTITION BY ‖ ORDER BY (integer constants → NULL); the outer ORDER BY is dropped when it equals a prefix of it | `expr_list_append_list`, rewrite |
| window.c:1025 | `iEphCsr = nTab++`, `nTab += 3` (current / write / start / end cursors) | rewrite |
| window.c:762–773 | inside a scalar sub-select only TK_COLUMN of the outer FROM is rewritten | `select_window_rewrite_expr_cb` |
| window.c:777–789 | a window function of this window list → prune; of another list → treated like a terminal | `select_window_rewrite_expr_cb` |
| window.c:796–826 | terminal already in pSub → reuse its column; else append a copy (TK_AGG_FUNCTION copy becomes TK_FUNCTION); replace in place by TK_COLUMN(iEphCsr, col) keeping EP_Collate / EP_OuterON / EP_InnerON (and iJoin) | `select_window_rewrite_expr_cb` (`expr_delete_unlink_windows` for the delete) |
| window.c:1047 | function with SQLITE_SUBTYPE → its arguments are rewritten (bExprArgs=1, evaluated at step time); else the arguments are appended as sub-select columns | rewrite |
| window.c:1055 | FILTER → its copy is one more sub-select column | rewrite |
| window.c:1059–1061 | regAccum / regResult per window, `OP_Null regAccum` | rewrite |
| window.c:1070 | empty sub-select result list → `SELECT 0` | rewrite |
| window.c:1086–1108 | the sub-select becomes the only FROM item (correlated, SF_Expanded|SF_OrderByReqd, aggregate flag moved), its result table copied into the TF_Ephemeral table the rewritten columns point at; outer TK_AGG_FUNCTION depths +1 (`sqlite3WindowExtraAggFuncDepth`) | rewrite |
| expr.c:7366 | an AggInfo entry that points at a node about to be rewritten gets its own copy (agginfoPersistExprCb) | `agginfo_persist_expr_cb` |
| expr.c:2009–2011 | SelectDup: `pWin=0`, `pWinDefn=WindowListDup`, `gatherSelectWindows` relinks the copied window functions | expr.rs `sqlite3_select_dup` |

### 2.4 Code generation (window.c 1394–1471, 2790–3116, select.c 8433–8496) — `window.rs`, `select.rs`  *(layer: EXPLAIN bytes, win dump eDelete/regArg/csrApp/regApp/rs/re)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| select.c:8434 | non-aggregate SELECT with pWin → sqlite3WindowCodeInit before WhereBegin; after it sqlite3WindowCodeStep + the inner-loop subroutine (`OP_Goto iBreak`, addrGosub: selectInnerLoop, `OP_Return regGosub`) | select.rs |
| window.c:1405–1408 | `OpenEphemeral(iEphCsr, nEphExpr)` + 3 × `OpenDup` | `sqlite3_window_code_init` |
| window.c:1412 | PARTITION BY → regPart (NULLed) | `sqlite3_window_code_init` |
| window.c:1422 | EXCLUDE (any non-0) → regStartRowid / regEndRowid / csrApp (full-scan mode), return | `sqlite3_window_code_init` |
| window.c:1434 | MINMAX with start ≠ UNBOUNDED → ephemeral index (DESC for min: `zName[1]=='i'`), regApp+3 | `sqlite3_window_code_init` |
| window.c:1458 / 1466 | nth_value / first_value → regApp+2, csrApp OpenDup; lead / lag → csrApp OpenDup | `sqlite3_window_code_init` |
| window.c:2849–2873 | eDelete: start FOLLOWING (not RANGE, offset > 0 by windowExprGtZero) → RETURN_ROW; start UNBOUNDED, no cached frame (windowCacheFrame) → end PRECEDING (not RANGE, offset > 0) → AGGSTEP, else RETURN_ROW; other starts → AGGINVERSE | `sqlite3_window_code_step` |
| window.c:2886–2904 | regStart / regEnd for offset bounds; peer registers unless ROWS | step |
| window.c:2919 | PARTITION BY → Compare/Jump + `Gosub flush` | step |
| window.c:2945–2952 | offset checks: ROWS/GROUPS `MustBeInt` + `Ge 0` ("…non-negative integer"), RANGE `String8 ''` + `Ge` (P5 NUMERIC|JUMPIFNULL) ("…non-negative number"), nth_value `Gt 0` | `window_check_value` |
| window.c:2954 | not RANGE, start type = end type, offsets → if empty frame (`Ge`/`Le`) return rows with xValue and reset | step |
| window.c:2966 | ROWS/GROUPS FOLLOWING..FOLLOWING → regStart -= regEnd | step |
| window.c:2991–3041 | main loop pattern by (eStart, eEnd, eFrmType) (3 cases: start FOLLOWING; end PRECEDING (bRPS for RANGE PRECEDING..PRECEDING); else, RANGE with regEnd uses windowCodeRangeTest loop, ROWS uses IfPos) | step |
| window.c:3056–3104 | flush pattern by the same cases (RANGE FOLLOWING, FOLLOWING..UNBOUNDED, FOLLOWING..FOLLOWING with regEnd -= regStart) | step |
| window.c:2258 | AGGINVERSE with start UNBOUNDED → nothing | `window_code_op` |
| window.c:2263–2285 | countdown: RANGE → windowCodeRangeTest (Le / Ge / Gt by op and start type), else `IfPos` | `window_code_op` |
| window.c:2299 | RANGE with start type = end type and a countdown: keep start ≤ end (AGGINVERSE) / end ≤ input (AGGSTEP while regRowid) | `window_code_op` |
| window.c:2350 | `op==eDelete` → `Delete` with OPFLAG_SAVEPOSITION | `window_code_op` |
| window.c:2355–2373 | jumpOnEof → Next + Goto returned; peers (not ROWS) → read peers + windowIfNewPeer back to addrContinue | `window_code_op` |
| window.c:2132–2138 | DESC order → op mirrored (Ge→Le, Gt→Lt, Le→Ge), arithmetic Subtract | `window_code_range_test` |
| window.c:2165 | BIGNULL (NULLS FIRST on DESC / NULLS LAST on ASC) → explicit NULL handling (NotNull / IsNull / Goto) | `window_code_range_test` |
| window.c:2207 | (Ge, Add) or (Le, Subtract) → extra compare before the arithmetic | `window_code_range_test` |
| window.c:1706 | inline MINMAX (no full scan, start ≠ UNBOUNDED): IsNull skip; step = AddImm/SCopy/MakeRecord/IdxInsert, inverse = SeekGE/Delete | `window_agg_step` |
| window.c:1724 | regApp (nth/first_value) → `AddImm regApp+1-bInverse` | `window_agg_step` |
| window.c:1731 | not lead/lag (`xSFunc!=noopStepFunc`) → (bExprArgs: code the argument list and point its Column ops at csr) [CollSeq for NEEDCOLL] AggStep / AggInverse | `window_agg_step` |
| window.c:1686 | nth_value's second argument is read from iEphCsr (current row), others from csr | `window_agg_step` |
| window.c:1694 | FILTER → Column + IfNot around the step | `window_agg_step` |
| window.c:1788–1809 | final: inline MINMAX → Null/Last/Column; regApp → nothing; else AggFinal+Copy+Null (bFin) or AggValue | `window_agg_final` |
| window.c:1930 | full-scan mode (EXCLUDE) → windowFullScan (EXCLUDE CURRENT ROW: Eq on rowid; GROUP/TIES: Compare peers (+ Eq for TIES); NO OTHERS: no test) | `window_return_one_row`, `window_full_scan` |
| window.c:1939–1991 | nth_value / first_value: SeekRowid in [regApp, regApp+1]; lead / lag: rowid ± offset (default when 3 args) | `window_return_one_row` |
| window.c:2013–2023 | first row of a partition: Integer 0 into regApp pair; MINMAX with csrApp → ResetSorter | `window_init_accum` |
| window.c:2037–2047 | frame cached when full-scan mode or nth_value / first_value / lead / lag | `window_cache_frame` |
| window.c:2448 | offset > 0 at compile time: sqlite3ValueFromExpr(NUMERIC) then `sqlite3_value_int()>0` (32-bit truncation) | `window_expr_gt_zero` |

### 2.5 Other SELECT decisions that look at windows — `select.rs`, `flatten.rs`  *(layer: EXPLAIN / EQP bytes)*
| C (line) | condition → outcome | Rust |
|---|---|---|
| select.c:2758 | recursive CTE whose recursive part has windows → "cannot use window functions in recursive queries" | select.rs `generate_with_recursive_query` |
| select.c:2943 | VALUES list with a window function → multiSelectValues returns -1 (ordinary compound coding) | select.rs `multi_select_values` |
| select.c:4419 / 4508 | flattening restrictions (25) and (17e) | flatten.rs (V3) |
| select.c:5265 / 5286 / 5380 | push-down: (6b) compound arm with windows → no; (6a) windows without PARTITION BY → no; (6c) term not a function of the PARTITION BY → term not pushed, nChng-- (SF_PushDown stays) | flatten.rs `push_down_where_terms`, `push_down_window_check` |
| select.c:5436 | disableUnusedSubqueryResultColumns: sub-query with windows → no | flatten.rs (V3) |
| select.c:8336 | DISTINCT → GROUP BY conversion only without windows | select.rs (V3) |
| expr.c:5472 | TK_FUNCTION with EP_WinFunc codes as its regResult register | exprcode.rs |
| expr.c:2579 | window functions are never constant | expr.rs (V3) |

## 3. Every branch line of the ported C functions (gcov outcomes)
For each source line of window.c (line number of `sqlite/src/window.c`) and of the other ported functions (amalgamation
line) that contains a branch: the counts of each outcome. `both` = every outcome seen at least once. Lines with an
outcome never seen are discussed below the table.

| C function | line | source | outcome counts | both |
|---|---|---|---|---|
| walkWindowList | sqlite3.c:110259 | `for(pWin=pList; pWin; pWin=pWin->pNextWin){` | 244452,4288 | both |
| walkWindowList | sqlite3.c:110262 | `if( rc ) return WRC_Abort;` | 32,244420 | both |
| walkWindowList | sqlite3.c:110264 | `if( rc ) return WRC_Abort;` | 0,244420 | **no** |
| walkWindowList | sqlite3.c:110266 | `if( rc ) return WRC_Abort;` | 6,244414 | both |
| walkWindowList | sqlite3.c:110268 | `if( rc ) return WRC_Abort;` | 0,244414 | **no** |
| walkWindowList | sqlite3.c:110270 | `if( rc ) return WRC_Abort;` | 0,244414 | **no** |
| walkWindowList | sqlite3.c:110271 | `if( bOneOnly ) break;` | 237974,6440 | both |
| resolveRemoveWindowsCb | sqlite3.c:112280 | `if( ExprHasProperty(pExpr, EP_WinFunc) ){` | 268,53684 | both |
| windowRemoveExprFromSelect | sqlite3.c:112292 | `if( pSelect->pWin ){` | 37032,113538 | both |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114111 | `if( NEVER(pOrderBy==0) ){` | 0,118 | **no** |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114115 | `if( pExpr==0 ){` | 0,118 | **no** |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114123 | `if( pExpr->x.pList==0 \|\| NEVER(pExpr->x.pList->nExpr==0) ){` | 118,0,0,118 | **no** |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114128 | `if( IsWindowFunc(pExpr) ){` | 118,0,118,0 | **no** |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114133 | `if( pOrderBy->nExpr>db->aLimit[SQLITE_LIMIT_COLUMN] ){` | never,never | **no** |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114140 | `if( pOB==0 ){` | never,never | **no** |
| gatherSelectWindowsCallback | sqlite3.c:114675 | `if( pExpr->op==TK_FUNCTION && ExprHasProperty(pExpr, EP_WinFunc) ){` | 656,2596,500,156 | both |
| agginfoPersistExprCb | sqlite3.c:120233 | `if( ALWAYS(!ExprHasProperty(pExpr, EP_TokenOnly\|EP_Reduced))` | 675218,0 | **no** |
| agginfoPersistExprCb | sqlite3.c:120234 | `&& pExpr->pAggInfo!=0` | 748,674470 | both |
| agginfoPersistExprCb | sqlite3.c:120241 | `if( pExpr->op!=TK_AGG_FUNCTION ){` | 446,302 | both |
| agginfoPersistExprCb | sqlite3.c:120242 | `if( iAgg<pAggInfo->nColumn` | 446,0 | **no** |
| agginfoPersistExprCb | sqlite3.c:120243 | `&& pAggInfo->aCol[iAgg].pCExpr==pExpr` | 414,32 | both |
| agginfoPersistExprCb | sqlite3.c:120246 | `if( pExpr && !sqlite3ExprDeferredDelete(pParse, pExpr) ){` | 414,0,414,0 | **no** |
| agginfoPersistExprCb | sqlite3.c:120252 | `if( ALWAYS(iAgg<pAggInfo->nFunc)` | 302,0 | **no** |
| agginfoPersistExprCb | sqlite3.c:120253 | `&& pAggInfo->aFunc[iAgg].pFExpr==pExpr` | 302,0 | **no** |
| agginfoPersistExprCb | sqlite3.c:120256 | `if( pExpr && !sqlite3ExprDeferredDelete(pParse, pExpr) ){` | 302,0,302,0 | **no** |
| multiSelectValues | sqlite3.c:152490 | `if( p->pWin ) return -1;` | 430,264 | both |
| multiSelectValues | sqlite3.c:152492 | `if( p->pPrior==0 ) break;` | 0,264 | **no** |
| multiSelectValues | sqlite3.c:152497 | `ExplainQueryPlan((pParse, 0, "SCAN %d CONSTANT ROW%s", nRow,` | never,never | **no** |
| multiSelectValues | sqlite3.c:152499 | `while( p ){` | never,never | **no** |
| multiSelectValues | sqlite3.c:152501 | `if( !bShowAll ) break;` | never,never | **no** |
| windowFind | window.c:636 | `for(p=pList; p; p=p->pNextWin){` | 5214,212 | both |
| windowFind | window.c:637 | `if( sqlite3StrICmp(p->zName, zName)==0 ) break;` | 4534,680 | both |
| windowFind | window.c:639 | `if( p==0 ){` | 212,4534 | both |
| sqlite3WindowUpdate | window.c:668 | `if( pWin->zName && pWin->eFrmType==0 ){` | 2140,76602,1700,440 | both |
| sqlite3WindowUpdate | window.c:670 | `if( p==0 ) return;` | 106,1594 | both |
| sqlite3WindowUpdate | window.c:682 | `if( (pWin->eFrmType==TK_RANGE)` | 45676,32960 | both |
| sqlite3WindowUpdate | window.c:683 | `&& (pWin->pStart \|\| pWin->pEnd)` | 39302,6374,2232,37070 | both |
| sqlite3WindowUpdate | window.c:684 | `&& (pWin->pOrderBy==0 \|\| pWin->pOrderBy->nExpr!=1)` | 7860,746,1186,6674 | both |
| sqlite3WindowUpdate | window.c:690 | `if( pFunc->funcFlags & SQLITE_FUNC_WINDOW ){` | 41022,35682 | both |
| sqlite3WindowUpdate | window.c:692 | `if( pWin->pFilter ){` | 98,40924 | both |
| sqlite3WindowUpdate | window.c:713 | `for(i=0; i<ArraySize(aUp); i++){` | 230436,8802 | both |
| sqlite3WindowUpdate | window.c:714 | `if( pFunc->zName==aUp[i].zFunc ){` | 32122,198314 | both |
| sqlite3WindowUpdate | window.c:722 | `if( pWin->eStart==TK_FOLLOWING ){` | 2244,29878 | both |
| selectWindowRewriteExprCb | window.c:762 | `if( p->pSubSelect ){` | 1606,227186 | both |
| selectWindowRewriteExprCb | window.c:763 | `if( pExpr->op!=TK_COLUMN ){` | 856,750 | both |
| selectWindowRewriteExprCb | window.c:768 | `for(i=0; i<nSrc; i++){` | 750,388 | both |
| selectWindowRewriteExprCb | window.c:769 | `if( pExpr->iTable==p->pSrc->a[i].iCursor ) break;` | 362,388 | both |
| selectWindowRewriteExprCb | window.c:771 | `if( i==nSrc ) return WRC_Continue;` | 388,362 | both |
| selectWindowRewriteExprCb | window.c:775 | `switch( pExpr->op ){` | 81552,111388,34608 | both |
| selectWindowRewriteExprCb | window.c:778 | `if( !ExprHasProperty(pExpr, EP_WinFunc) ){` | 6962,74590 | both |
| selectWindowRewriteExprCb | window.c:782 | `for(pWin=p->pWin; pWin; pWin=pWin->pNextWin){` | 78000,15920 | both |
| selectWindowRewriteExprCb | window.c:783 | `if( pExpr->y.pWin==pWin ){` | 58670,19330 | both |
| selectWindowRewriteExprCb | window.c:795 | `if( pParse->db->mallocFailed ) return WRC_Abort;` | 0,127308 | **no** |
| selectWindowRewriteExprCb | window.c:796 | `if( p->pSub ){` | 78158,49150 | both |
| selectWindowRewriteExprCb | window.c:798 | `for(i=0; i<p->pSub->nExpr; i++){` | 143472,41692 | both |
| selectWindowRewriteExprCb | window.c:799 | `if( 0==sqlite3ExprCompare(0, p->pSub->a[i].pExpr, pExpr, -1) ){` | 36466,107006 | both |
| selectWindowRewriteExprCb | window.c:805 | `if( iCol<0 ){` | 90842,36466 | both |
| selectWindowRewriteExprCb | window.c:807 | `if( pDup && pDup->op==TK_AGG_FUNCTION ) pDup->op = TK_FUNCTION;` | 90842,0,3400,87442 | **no** |
| selectWindowRewriteExprCb | window.c:810 | `if( p->pSub ){` | 127308,0 | **no** |
| selectWindowRewriteExprCb | window.c:821 | `pExpr->iColumn = (iCol<0 ? p->pSub->nExpr-1: iCol);` | 90842,36466 | both |
| selectWindowRewriteExprCb | window.c:825 | `if( (f & (EP_OuterON\|EP_InnerON)) ) pExpr->w.iJoin = iJoin;` | 0,127308 | **no** |
| selectWindowRewriteExprCb | window.c:827 | `if( pParse->db->mallocFailed ) return WRC_Abort;` | 0,127308 | **no** |
| selectWindowRewriteSelectCb | window.c:840 | `if( pSave==pSelect ){` | 494,494 | both |
| exprListAppendList | window.c:904 | `if( pAppend ){` | 190386,90952 | both |
| exprListAppendList | window.c:906 | `int nInit = pList ? pList->nExpr : 0;` | 132640,57746 | both |
| exprListAppendList | window.c:907 | `for(i=0; i<pAppend->nExpr; i++){` | 234084,190386 | both |
| exprListAppendList | window.c:910 | `if( db->mallocFailed ){` | 0,234084 | **no** |
| exprListAppendList | window.c:914 | `if( bIntToNull ){` | 89752,144332 | both |
| exprListAppendList | window.c:918 | `if( sqlite3ExprIsInteger(pSub, &iDummy, 0, 0) ){` | 2352,87400 | both |
| exprListAppendList | window.c:925 | `if( pList ) pList->a[nInit+i].fg.sortFlags = pAppend->a[i].fg.sortFlags;` | 234084,0 | **no** |
| sqlite3WindowExtraAggFuncDepth | window.c:940 | `if( pExpr->op==TK_AGG_FUNCTION` | 10118,612206 | both |
| sqlite3WindowExtraAggFuncDepth | window.c:941 | `&& pExpr->op2>=pWalker->walkerDepth` | 446,9672 | both |
| disallowAggregatesInOrderByCb | window.c:949 | `if( pExpr->op==TK_AGG_FUNCTION && pExpr->pAggInfo==0 ){` | 92,65316,88,4 | both |
| sqlite3WindowRewrite | window.c:966 | `if( p->pWin` | 56956,72112 | both |
| sqlite3WindowRewrite | window.c:967 | `&& p->pPrior==0` | 56434,522 | both |
| sqlite3WindowRewrite | window.c:968 | `&& ALWAYS((p->selFlags & SF_WinRewrite)==0)` | 56434,0 | **no** |
| sqlite3WindowRewrite | window.c:969 | `&& ALWAYS(!IN_RENAME_OBJECT)` | 56434,0 | **no** |
| sqlite3WindowRewrite | window.c:989 | `if( pTab==0 ){` | 0,56434 | **no** |
| sqlite3WindowRewrite | window.c:994 | `if( (p->selFlags & SF_Aggregate)==0 ){` | 52842,3592 | both |
| sqlite3WindowRewrite | window.c:1012 | `if( pSort && p->pOrderBy && p->pOrderBy->nExpr<=pSort->nExpr ){` | 51196,5238,19222,31974,16666,2556 | both |
| sqlite3WindowRewrite | window.c:1015 | `if( sqlite3ExprListCompare(pSort, p->pOrderBy, -1)==0 ){` | 6066,10600 | both |
| sqlite3WindowRewrite | window.c:1030 | `pMWin->nBufferCol = (pSublist ? pSublist->nExpr : 0);` | 49008,7426 | both |
| sqlite3WindowRewrite | window.c:1042 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 58670,56434 | both |
| sqlite3WindowRewrite | window.c:1047 | `if( pWin->pWFunc->funcFlags & SQLITE_SUBTYPE ){` | 3068,55602 | both |
| sqlite3WindowRewrite | window.c:1049 | `pWin->iArgCol = (pSublist ? pSublist->nExpr : 0);` | 3064,4 | both |
| sqlite3WindowRewrite | window.c:1052 | `pWin->iArgCol = (pSublist ? pSublist->nExpr : 0);` | 53638,1964 | both |
| sqlite3WindowRewrite | window.c:1055 | `if( pWin->pFilter ){` | 3170,55500 | both |
| sqlite3WindowRewrite | window.c:1070 | `if( pSublist==0 ){` | 728,55706 | both |
| sqlite3WindowRewrite | window.c:1084 | `if( p->pSrc==0 ){` | 0,56434 | **no** |
| sqlite3WindowRewrite | window.c:1086 | `}else if( sqlite3SrcItemAttachSubquery(pParse, &p->pSrc->a[0], pSub, 0) ){` | 56434,0 | **no** |
| sqlite3WindowRewrite | window.c:1093 | `if( pTab2==0 ){` | 88,56346 | both |
| sqlite3WindowRewrite | window.c:1110 | `if( db->mallocFailed ) rc = SQLITE_NOMEM;` | 0,56434 | **no** |
| sqlite3WindowUnlinkFromSelect | window.c:1127 | `if( p->ppThis ){` | 63970,43804 | both |
| sqlite3WindowUnlinkFromSelect | window.c:1129 | `if( p->pNextWin ) p->pNextWin->ppThis = p->ppThis;` | 268,63702 | both |
| sqlite3WindowOffsetExpr | window.c:1170 | `if( 0==sqlite3ExprIsConstant(0,pExpr) ){` | 3088,163668 | both |
| sqlite3WindowOffsetExpr | window.c:1171 | `if( IN_RENAME_OBJECT ) sqlite3RenameExprUnmap(pParse, pExpr);` | 0,3088 | **no** |
| sqlite3WindowAlloc | window.c:1202 | `if( eType==0 ){` | 36676,52968 | both |
| sqlite3WindowAlloc | window.c:1221 | `if( (eStart==TK_CURRENT && eEnd==TK_PRECEDING)` | 12444,77200,10398,2046 | both |
| sqlite3WindowAlloc | window.c:1222 | `\|\| (eStart==TK_FOLLOWING && (eEnd==TK_PRECEDING \|\| eEnd==TK_CURRENT))` | 9232,78366,7308,1924,2296,5012 | both |
| sqlite3WindowAlloc | window.c:1229 | `if( pWin==0 ) goto windowAllocErr;` | 0,83378 | **no** |
| sqlite3WindowAlloc | window.c:1233 | `if( eExclude==0 && OptimizationDisabled(pParse->db, SQLITE_WindowFunc) ){` | 70458,12920,0,70458 | **no** |
| sqlite3WindowAssemble | window.c:1260 | `if( pWin ){` | 74782,5678 | both |
| sqlite3WindowAssemble | window.c:1263 | `if( pBase ){` | 3770,71012 | both |
| sqlite3WindowChain | window.c:1281 | `if( pWin->zBase ){` | 3046,78226 | both |
| sqlite3WindowChain | window.c:1284 | `if( pExist ){` | 2940,106 | both |
| sqlite3WindowChain | window.c:1287 | `if( pWin->pPartition ){` | 470,2470 | both |
| sqlite3WindowChain | window.c:1289 | `}else if( pExist->pOrderBy && pWin->pOrderBy ){` | 2138,332,1102,1036 | both |
| sqlite3WindowChain | window.c:1291 | `}else if( pExist->bImplicitFrame==0 ){` | 816,552 | both |
| sqlite3WindowChain | window.c:1294 | `if( zErr ){` | 2388,552 | both |
| sqlite3WindowChain | window.c:1300 | `if( pExist->pOrderBy ){` | 436,116 | both |
| sqlite3WindowAttach | window.c:1315 | `if( p ){` | 75614,0 | **no** |
| sqlite3WindowAttach | window.c:1322 | `if( (p->flags & EP_Distinct) && pWin->eFrmType!=TK_FILTER ){` | 282,75332,132,150 | both |
| sqlite3WindowLink | window.c:1339 | `if( pSel ){` | 79242,0 | **no** |
| sqlite3WindowLink | window.c:1340 | `if( 0==pSel->pWin \|\| 0==sqlite3WindowCompare(0, pSel->pWin, pWin, 0) ){` | 17796,61446,2524,15272 | both |
| sqlite3WindowLink | window.c:1342 | `if( pSel->pWin ){` | 2524,61446 | both |
| sqlite3WindowLink | window.c:1348 | `if( sqlite3ExprListCompare(pWin->pPartition, pSel->pWin->pPartition,-1) ){` | 8734,6538 | both |
| sqlite3WindowCompare | window.c:1367 | `if( NEVER(p1==0) \|\| NEVER(p2==0) ) return 1;` | 18436,0,0,18436 | **no** |
| sqlite3WindowCompare | window.c:1368 | `if( p1->eFrmType!=p2->eFrmType ) return 1;` | 9454,8982 | both |
| sqlite3WindowCompare | window.c:1369 | `if( p1->eStart!=p2->eStart ) return 1;` | 2934,6048 | both |
| sqlite3WindowCompare | window.c:1370 | `if( p1->eEnd!=p2->eEnd ) return 1;` | 1374,4674 | both |
| sqlite3WindowCompare | window.c:1371 | `if( p1->eExclude!=p2->eExclude ) return 1;` | 118,4556 | both |
| sqlite3WindowCompare | window.c:1372 | `if( sqlite3ExprCompare(pParse, p1->pStart, p2->pStart, -1) ) return 1;` | 184,4372 | both |
| sqlite3WindowCompare | window.c:1373 | `if( sqlite3ExprCompare(pParse, p1->pEnd, p2->pEnd, -1) ) return 1;` | 108,4264 | both |
| sqlite3WindowCompare | window.c:1374 | `if( (res = sqlite3ExprListCompare(p1->pPartition, p2->pPartition, -1)) ){` | 1074,3190 | both |
| sqlite3WindowCompare | window.c:1377 | `if( (res = sqlite3ExprListCompare(p1->pOrderBy, p2->pOrderBy, -1)) ){` | 154,3036 | both |
| sqlite3WindowCompare | window.c:1380 | `if( bFilter ){` | 512,2524 | both |
| sqlite3WindowCompare | window.c:1381 | `if( (res = sqlite3ExprCompare(pParse, p1->pFilter, p2->pFilter, -1)) ){` | 4,508 | both |
| sqlite3WindowCodeInit | window.c:1412 | `if( pMWin->pPartition ){` | 26948,29398 | both |
| sqlite3WindowCodeInit | window.c:1422 | `if( pMWin->eExclude ){` | 5034,51312 | both |
| sqlite3WindowCodeInit | window.c:1432 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 53290,51312 | both |
| sqlite3WindowCodeInit | window.c:1434 | `if( (p->funcFlags & SQLITE_FUNC_MINMAX) && pWin->eStart!=TK_UNBOUNDED ){` | 7116,46174,4582,2534 | both |
| sqlite3WindowCodeInit | window.c:1450 | `if( pKeyInfo && pWin->pWFunc->zName[1]=='i' ){` | 4582,0,2192,2390 | **no** |
| sqlite3WindowCodeInit | window.c:1458 | `else if( p->zName==nth_valueName \|\| p->zName==first_valueName ){` | 47426,1282,2050,45376 | both |
| sqlite3WindowCodeInit | window.c:1466 | `else if( p->zName==leadName \|\| p->zName==lagName ){` | 40250,5126,4750,35500 | both |
| windowCheckValue | window.c:1499 | `if( eCond>=WINDOW_STARTING_NUM ){` | 5872,16936 | both |
| windowArgCount | window.c:1537 | `return (pList ? pList->nExpr : 0);` | 252146,93726 | both |
| windowReadPeerValues | window.c:1632 | `if( pOrderBy ){` | 152954,19838 | both |
| windowReadPeerValues | window.c:1635 | `int iColOff = pMWin->nBufferCol + (pPart ? pPart->nExpr : 0);` | 80936,72018 | both |
| windowReadPeerValues | window.c:1637 | `for(i=0; i<pOrderBy->nExpr; i++){` | 181092,152954 | both |
| windowAggStep | window.c:1672 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 144394,138788 | both |
| windowAggStep | window.c:1675 | `int nArg = pWin->bExprArgs ? 0 : windowArgCount(pWin);` | 136894,7500 | both |
| windowAggStep | window.c:1685 | `for(i=0; i<nArg; i++){` | 130884,144394 | both |
| windowAggStep | window.c:1686 | `if( i!=1 \|\| pFunc->zName!=nth_valueName ){` | 21812,109072,17290,4522 | both |
| windowAggStep | window.c:1694 | `if( pWin->pFilter ){` | 7934,136460 | both |
| windowAggStep | window.c:1706 | `if( pMWin->regStartRowid==0` | 134446,9948 | both |
| windowAggStep | window.c:1707 | `&& (pFunc->funcFlags & SQLITE_FUNC_MINMAX)` | 22116,112330 | both |
| windowAggStep | window.c:1708 | `&& (pWin->eStart!=TK_UNBOUNDED)` | 17048,5068 | both |
| windowAggStep | window.c:1712 | `if( bInverse==0 ){` | 9164,7884 | both |
| windowAggStep | window.c:1724 | `}else if( pWin->regApp ){` | 9528,117818 | both |
| windowAggStep | window.c:1731 | `}else if( pFunc->xSFunc!=noopStepFunc ){` | 98066,19752 | both |
| windowAggStep | window.c:1732 | `if( pWin->bExprArgs ){` | 7500,90566 | both |
| windowAggStep | window.c:1741 | `for(iEnd=sqlite3VdbeCurrentAddr(v); iOp<iEnd; iOp++){` | 17283,7500 | both |
| windowAggStep | window.c:1743 | `if( pOp->opcode==OP_Column && pOp->p1==pMWin->iEphCsr ){` | 9332,7951,9104,228 | both |
| windowAggStep | window.c:1748 | `if( pFunc->funcFlags & SQLITE_FUNC_NEEDCOLL ){` | 7178,90888 | both |
| windowAggStep | window.c:1755 | `sqlite3VdbeAddOp3(v, bInverse? OP_AggInverse : OP_AggStep,` | 17118,80948 | both |
| windowAggStep | window.c:1759 | `if( pWin->bExprArgs ){` | 7500,90566 | both |
| windowAggStep | window.c:1764 | `if( addrIf ) sqlite3VdbeJumpHere(v, addrIf);` | 7934,136460 | both |
| windowAggFinal | window.c:1787 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 107514,103066 | both |
| windowAggFinal | window.c:1788 | `if( pMWin->regStartRowid==0` | 97198,10316 | both |
| windowAggFinal | window.c:1789 | `&& (pWin->pWFunc->funcFlags & SQLITE_FUNC_MINMAX)` | 13446,83752 | both |
| windowAggFinal | window.c:1790 | `&& (pWin->eStart!=TK_UNBOUNDED)` | 8458,4988 | both |
| windowAggFinal | window.c:1797 | `}else if( pWin->regApp ){` | 92360,6696 | both |
| windowAggFinal | window.c:1801 | `if( bFin ){` | 9948,82412 | both |
| windowFullScan | window.c:1841 | `nPeer = (pMWin->pOrderBy ? pMWin->pOrderBy->nExpr : 0);` | 8484,938 | both |
| windowFullScan | window.c:1848 | `if( nPeer ){` | 8484,938 | both |
| windowFullScan | window.c:1856 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 9948,9422 | both |
| windowFullScan | window.c:1867 | `if( pMWin->eExclude==TK_CURRENT ){` | 2944,6478 | both |
| windowFullScan | window.c:1870 | `}else if( pMWin->eExclude!=TK_NO ){` | 4520,1958 | both |
| windowFullScan | window.c:1875 | `if( pMWin->pOrderBy ){` | 4034,486 | both |
| windowFullScan | window.c:1878 | `if( pMWin->eExclude==TK_TIES ){` | 2282,2238 | both |
| windowFullScan | window.c:1882 | `if( pKeyInfo ){` | 4034,486 | both |
| windowFullScan | window.c:1892 | `if( addrEq ) sqlite3VdbeJumpHere(v, addrEq);` | 2282,2238 | both |
| windowFullScan | window.c:1904 | `if( nPeer ){` | 8484,938 | both |
| windowReturnOneRow | window.c:1930 | `if( pMWin->regStartRowid ){` | 9422,93312 | both |
| windowReturnOneRow | window.c:1936 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 97198,93312 | both |
| windowReturnOneRow | window.c:1939 | `if( pFunc->zName==nth_valueName` | 94598,2600 | both |
| windowReturnOneRow | window.c:1940 | `\|\| pFunc->zName==first_valueName` | 4096,90502 | both |
| windowReturnOneRow | window.c:1947 | `if( pFunc->zName==nth_valueName ){` | 2600,4096 | both |
| windowReturnOneRow | window.c:1962 | `else if( pFunc->zName==leadName \|\| pFunc->zName==lagName ){` | 85376,5126,9500,75876 | both |
| windowReturnOneRow | window.c:1969 | `if( nArg<3 ){` | 9484,5142 | both |
| windowReturnOneRow | window.c:1975 | `if( nArg<2 ){` | 4926,9700 | both |
| windowReturnOneRow | window.c:1976 | `int val = (pFunc->zName==leadName ? 1 : -1);` | 1538,3388 | both |
| windowReturnOneRow | window.c:1979 | `int op = (pFunc->zName==leadName ? OP_Add : OP_Subtract);` | 3588,6112 | both |
| windowInitAccum | window.c:2008 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 58582,56346 | both |
| windowInitAccum | window.c:2012 | `nArg = MAX(nArg, windowArgCount(pWin));` | 58036,546 | both |
| windowInitAccum | window.c:2013 | `if( pMWin->regStartRowid==0 ){` | 53290,5292 | both |
| windowInitAccum | window.c:2014 | `if( pFunc->zName==nth_valueName \|\| pFunc->zName==first_valueName ){` | 52008,1282,2050,49958 | both |
| windowInitAccum | window.c:2019 | `if( (pFunc->funcFlags & SQLITE_FUNC_MINMAX) && pWin->csrApp ){` | 7116,46174,4582,2534 | both |
| windowCacheFrame | window.c:2037 | `if( pMWin->regStartRowid ) return 1;` | 1884,33930 | both |
| windowCacheFrame | window.c:2038 | `for(pWin=pMWin; pWin; pWin=pWin->pNextWin){` | 35048,22390 | both |
| windowCacheFrame | window.c:2040 | `if( (pFunc->zName==nth_valueName)` | 34226,822 | both |
| windowCacheFrame | window.c:2041 | `\|\| (pFunc->zName==first_valueName)` | 33276,950 | both |
| windowCacheFrame | window.c:2042 | `\|\| (pFunc->zName==leadName)` | 28210,5066 | both |
| windowCacheFrame | window.c:2043 | `\|\| (pFunc->zName==lagName)` | 4702,23508 | both |
| windowIfNewPeer | window.c:2069 | `if( pOrderBy ){` | 149260,23380 | both |
| windowCodeRangeTest | window.c:2132 | `if( pOrderBy->a[0].fg.sortFlags & KEYINFO_ORDER_DESC ){` | 3104,6068 | both |
| windowCodeRangeTest | window.c:2133 | `switch( op ){` | 2116,484,504 | both |
| windowCodeRangeTest | window.c:2165 | `if( pOrderBy->a[0].fg.sortFlags & KEYINFO_ORDER_BIGNULL ){` | 1664,7508 | both |
| windowCodeRangeTest | window.c:2168 | `switch( op ){` | 696,104,724,140 | both |
| windowCodeRangeTest | window.c:2186 | `sqlite3VdbeAddOp2(v, OP_IsNull, reg2,` | 1560,104 | both |
| windowCodeRangeTest | window.c:2187 | `(op==OP_Gt \|\| op==OP_Ge) ? addrDone : lbl);` | 696,864 | both |
| windowCodeRangeTest | window.c:2207 | `if( (op==OP_Ge && arith==OP_Add) \|\| (op==OP_Le && arith==OP_Subtract) ){` | 4746,4426,504,4242,3134,1796,2116,1018 | both |
| windowCodeOp | window.c:2258 | `if( op==WINDOW_AGGINVERSE && pMWin->eStart==TK_UNBOUNDED ){` | 96848,214364,64624,32224 | both |
| windowCodeOp | window.c:2263 | `if( regCountdown>0 ){` | 26400,220188 | both |
| windowCodeOp | window.c:2264 | `if( pMWin->eFrmType==TK_RANGE ){` | 7220,19180 | both |
| windowCodeOp | window.c:2267 | `if( op==WINDOW_AGGINVERSE ){` | 5928,1292 | both |
| windowCodeOp | window.c:2268 | `if( pMWin->eStart==TK_FOLLOWING ){` | 1522,4406 | both |
| windowCodeOp | window.c:2288 | `if( op==WINDOW_RETURN_ROW && pMWin->regStartRowid==0 ){` | 101672,144916,92582,9090 | both |
| windowCodeOp | window.c:2299 | `if( pMWin->eStart==pMWin->eEnd && regCountdown` | 43818,202770,6642,37176 | both |
| windowCodeOp | window.c:2300 | `&& pMWin->eFrmType==TK_RANGE` | 2900,3742 | both |
| windowCodeOp | window.c:2304 | `if( op==WINDOW_AGGINVERSE ){` | 1964,936 | both |
| windowCodeOp | window.c:2309 | `}else if( p->regRowid ){` | 468,468 | both |
| windowCodeOp | window.c:2319 | `switch( op ){` | 101672,32224,112692 | both |
| windowCodeOp | window.c:2329 | `if( pMWin->regStartRowid ){` | 5482,26742 | both |
| windowCodeOp | window.c:2341 | `if( pMWin->regStartRowid ){` | 10068,102624 | both |
| windowCodeOp | window.c:2350 | `if( op==p->eDelete ){` | 76178,170410 | both |
| windowCodeOp | window.c:2355 | `if( jumpOnEof ){` | 62070,184518 | both |
| windowCodeOp | window.c:2362 | `if( bPeer ){` | 104130,80388 | both |
| windowCodeOp | window.c:2367 | `if( bPeer ){` | 140992,105596 | both |
| windowCodeOp | window.c:2368 | `int nReg = (pMWin->pOrderBy ? pMWin->pOrderBy->nExpr : 0);` | 122092,18900 | both |
| windowCodeOp | window.c:2369 | `int regTmp = (nReg ? sqlite3GetTempRange(pParse, nReg) : 0);` | 122092,18900 | both |
| windowCodeOp | window.c:2375 | `if( addrNextRange ){` | 7220,239368 | both |
| sqlite3WindowDup | window.c:2390 | `if( ALWAYS(p) ){` | 21530,0 | **no** |
| sqlite3WindowDup | window.c:2392 | `if( pNew ){` | 21530,0 | **no** |
| sqlite3WindowListDup | window.c:2426 | `for(pWin=p; pWin; pWin=pWin->pNextWin){` | 530,17511 | both |
| sqlite3WindowListDup | window.c:2428 | `if( *pp==0 ) break;` | 0,530 | **no** |
| windowExprGtZero | window.c:2448 | `if( pVal && sqlite3_value_int(pVal)>0 ){` | 2728,222,2366,362 | both |
| sqlite3WindowCodeStep | window.c:2849 | `switch( pMWin->eStart ){` | 3672,35814,16860 | both |
| sqlite3WindowCodeStep | window.c:2851 | `if( pMWin->eFrmType!=TK_RANGE` | 2664,1008 | both |
| sqlite3WindowCodeStep | window.c:2852 | `&& windowExprGtZero(pParse, pMWin->pStart)` | 2224,440 | both |
| sqlite3WindowCodeStep | window.c:2858 | `if( windowCacheFrame(pMWin)==0 ){` | 22390,13424 | both |
| sqlite3WindowCodeStep | window.c:2859 | `if( pMWin->eEnd==TK_PRECEDING ){` | 386,22004 | both |
| sqlite3WindowCodeStep | window.c:2860 | `if( pMWin->eFrmType!=TK_RANGE` | 286,100 | both |
| sqlite3WindowCodeStep | window.c:2861 | `&& windowExprGtZero(pParse, pMWin->pEnd)` | 142,144 | both |
| sqlite3WindowCodeStep | window.c:2886 | `if( pMWin->eStart==TK_PRECEDING \|\| pMWin->eStart==TK_FOLLOWING ){` | 46720,9626,3672,43048 | both |
| sqlite3WindowCodeStep | window.c:2889 | `if( pMWin->eEnd==TK_PRECEDING \|\| pMWin->eEnd==TK_FOLLOWING ){` | 54726,1620,5290,49436 | both |
| sqlite3WindowCodeStep | window.c:2896 | `if( pMWin->eFrmType!=TK_ROWS ){` | 31648,24698 | both |
| sqlite3WindowCodeStep | window.c:2897 | `int nPeer = (pOrderBy ? pOrderBy->nExpr : 0);` | 27168,4480 | both |
| sqlite3WindowCodeStep | window.c:2899 | `if( pMWin->pPartition ) regNewPeer += pMWin->pPartition->nExpr;` | 16008,15640 | both |
| sqlite3WindowCodeStep | window.c:2909 | `for(iInput=0; iInput<nInput; iInput++){` | 238896,56346 | both |
| sqlite3WindowCodeStep | window.c:2919 | `if( pMWin->pPartition ){` | 26948,29398 | both |
| sqlite3WindowCodeStep | window.c:2945 | `if( regStart ){` | 13298,43048 | both |
| sqlite3WindowCodeStep | window.c:2947 | `windowCheckValue(pParse, regStart, 0 + (pMWin->eFrmType==TK_RANGE?3:0));` | 3274,10024 | both |
| sqlite3WindowCodeStep | window.c:2949 | `if( regEnd ){` | 6910,49436 | both |
| sqlite3WindowCodeStep | window.c:2951 | `windowCheckValue(pParse, regEnd, 1 + (pMWin->eFrmType==TK_RANGE?3:0));` | 2598,4312 | both |
| sqlite3WindowCodeStep | window.c:2954 | `if( pMWin->eFrmType!=TK_RANGE && pMWin->eStart==pMWin->eEnd && regStart ){` | 32410,23936,7868,24542,1062,6806 | both |
| sqlite3WindowCodeStep | window.c:2955 | `int op = ((pMWin->eStart==TK_FOLLOWING) ? OP_Ge : OP_Le);` | 556,506 | both |
| sqlite3WindowCodeStep | window.c:2966 | `if( pMWin->eStart==TK_FOLLOWING && pMWin->eFrmType!=TK_RANGE && regEnd ){` | 3672,52674,2664,1008,556,2108 | both |
| sqlite3WindowCodeStep | window.c:2971 | `if( pMWin->eStart!=TK_UNBOUNDED ){` | 20532,35814 | both |
| sqlite3WindowCodeStep | window.c:2976 | `if( regPeer && pOrderBy ){` | 31648,24698,27168,4480 | both |
| sqlite3WindowCodeStep | window.c:2988 | `if( regPeer ){` | 31648,24698 | both |
| sqlite3WindowCodeStep | window.c:2991 | `if( pMWin->eStart==TK_FOLLOWING ){` | 3672,52674 | both |
| sqlite3WindowCodeStep | window.c:2993 | `if( pMWin->eEnd!=TK_UNBOUNDED ){` | 1070,2602 | both |
| sqlite3WindowCodeStep | window.c:2994 | `if( pMWin->eFrmType==TK_RANGE ){` | 514,556 | both |
| sqlite3WindowCodeStep | window.c:3008 | `if( pMWin->eEnd==TK_PRECEDING ){` | 1620,51054 | both |
| sqlite3WindowCodeStep | window.c:3009 | `int bRPS = (pMWin->eStart==TK_PRECEDING && pMWin->eFrmType==TK_RANGE);` | 974,646,468,506 | both |
| sqlite3WindowCodeStep | window.c:3011 | `if( bRPS ) windowCodeOp(&s, WINDOW_AGGINVERSE, regStart, 0);` | 468,1152 | both |
| sqlite3WindowCodeStep | window.c:3013 | `if( !bRPS ) windowCodeOp(&s, WINDOW_AGGINVERSE, regStart, 0);` | 1152,468 | both |
| sqlite3WindowCodeStep | window.c:3017 | `if( pMWin->eEnd!=TK_UNBOUNDED ){` | 38964,12090 | both |
| sqlite3WindowCodeStep | window.c:3018 | `if( pMWin->eFrmType==TK_RANGE ){` | 21074,17890 | both |
| sqlite3WindowCodeStep | window.c:3021 | `if( regEnd ){` | 1438,19636 | both |
| sqlite3WindowCodeStep | window.c:3027 | `if( regEnd ){` | 1438,19636 | both |
| sqlite3WindowCodeStep | window.c:3032 | `if( regEnd ){` | 2782,15108 | both |
| sqlite3WindowCodeStep | window.c:3038 | `if( regEnd ) sqlite3VdbeJumpHere(v, addr);` | 2782,15108 | both |
| sqlite3WindowCodeStep | window.c:3048 | `if( pMWin->pPartition ){` | 26948,29398 | both |
| sqlite3WindowCodeStep | window.c:3056 | `if( pMWin->eEnd==TK_PRECEDING ){` | 1620,54726 | both |
| sqlite3WindowCodeStep | window.c:3057 | `int bRPS = (pMWin->eStart==TK_PRECEDING && pMWin->eFrmType==TK_RANGE);` | 974,646,468,506 | both |
| sqlite3WindowCodeStep | window.c:3059 | `if( bRPS ) windowCodeOp(&s, WINDOW_AGGINVERSE, regStart, 0);` | 468,1152 | both |
| sqlite3WindowCodeStep | window.c:3061 | `}else if( pMWin->eStart==TK_FOLLOWING ){` | 3672,51054 | both |
| sqlite3WindowCodeStep | window.c:3067 | `if( pMWin->eFrmType==TK_RANGE ){` | 1008,2664 | both |
| sqlite3WindowCodeStep | window.c:3072 | `if( pMWin->eEnd==TK_UNBOUNDED ){` | 2108,556 | both |
| sqlite3WindowCodeStep | window.c:3108 | `if( pMWin->pPartition ){` | 26948,29398 | both |
| sqlite3WindowCodeStep | window.c:3109 | `if( pMWin->regStartRowid ){` | 2602,24346 | both |

Branch lines: 273, with every outcome seen: 231, with an outcome never seen: 42.

### 3.1 The lines with an outcome never seen, by reason

| C function | line | source | outcome counts | reason |
|---|---|---|---|---|
| walkWindowList | sqlite3.c:110264 | `if( rc ) return WRC_Abort;` | 0,244420 | a walker callback aborting inside a window's PARTITION / ORDER / FILTER / bounds (no ported walker does) |
| walkWindowList | sqlite3.c:110268 | `if( rc ) return WRC_Abort;` | 0,244414 | a walker callback aborting inside a window's PARTITION / ORDER / FILTER / bounds (no ported walker does) |
| walkWindowList | sqlite3.c:110270 | `if( rc ) return WRC_Abort;` | 0,244414 | a walker callback aborting inside a window's PARTITION / ORDER / FILTER / bounds (no ported walker does) |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114111 | `if( NEVER(pOrderBy==0) ){` | 0,118 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114115 | `if( pExpr==0 ){` | 0,118 | pExpr==0: only after an OOM in sqlite3ExprFunction (the rule's expression is always built) |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114123 | `if( pExpr->x.pList==0 \|\| NEVER(pExpr->x.pList->nExpr==0) ){` | 118,0,0,118 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114128 | `if( IsWindowFunc(pExpr) ){` | 118,0,118,0 | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114133 | `if( pOrderBy->nExpr>db->aLimit[SQLITE_LIMIT_COLUMN] ){` | never,never | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| sqlite3ExprAddFunctionOrderBy | sqlite3.c:114140 | `if( pOB==0 ){` | never,never | OOM only (allocation failure; the port has no allocation failure path) |
| agginfoPersistExprCb | sqlite3.c:120233 | `if( ALWAYS(!ExprHasProperty(pExpr, EP_TokenOnly\|EP_Reduced))` | 675218,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| agginfoPersistExprCb | sqlite3.c:120242 | `if( iAgg<pAggInfo->nColumn` | 446,0 | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| agginfoPersistExprCb | sqlite3.c:120246 | `if( pExpr && !sqlite3ExprDeferredDelete(pParse, pExpr) ){` | 414,0,414,0 | OOM only (allocation failure; the port has no allocation failure path) |
| agginfoPersistExprCb | sqlite3.c:120252 | `if( ALWAYS(iAgg<pAggInfo->nFunc)` | 302,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| agginfoPersistExprCb | sqlite3.c:120253 | `&& pAggInfo->aFunc[iAgg].pFExpr==pExpr` | 302,0 | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| agginfoPersistExprCb | sqlite3.c:120256 | `if( pExpr && !sqlite3ExprDeferredDelete(pParse, pExpr) ){` | 302,0,302,0 | OOM only (allocation failure; the port has no allocation failure path) |
| multiSelectValues | sqlite3.c:152492 | `if( p->pPrior==0 ) break;` | 0,264 | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| multiSelectValues | sqlite3.c:152497 | `ExplainQueryPlan((pParse, 0, "SCAN %d CONSTANT ROW%s", nRow,` | never,never | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| multiSelectValues | sqlite3.c:152499 | `while( p ){` | never,never | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| multiSelectValues | sqlite3.c:152501 | `if( !bShowAll ) break;` | never,never | shared with V1/V3 code paths (VALUES lists, aggregate ORDER BY, AggInfo persistence) whose other outcomes are exercised by the V3 corpora, not by the window corpora measured here |
| selectWindowRewriteExprCb | window.c:795 | `if( pParse->db->mallocFailed ) return WRC_Abort;` | 0,127308 | OOM only (allocation failure; the port has no allocation failure path) |
| selectWindowRewriteExprCb | window.c:807 | `if( pDup && pDup->op==TK_AGG_FUNCTION ) pDup->op = TK_FUNCTION;` | 90842,0,3400,87442 | OOM only (allocation failure; the port has no allocation failure path) |
| selectWindowRewriteExprCb | window.c:810 | `if( p->pSub ){` | 127308,0 | OOM only (allocation failure; the port has no allocation failure path) |
| selectWindowRewriteExprCb | window.c:825 | `if( (f & (EP_OuterON\|EP_InnerON)) ) pExpr->w.iJoin = iJoin;` | 0,127308 | EP_OuterON/EP_InnerON on a result-list / ORDER BY terminal: such flags only come from ON-clause terms; no corpus statement puts one into a window SELECT's result list (the flag copy is the same three statements in Rust) |
| selectWindowRewriteExprCb | window.c:827 | `if( pParse->db->mallocFailed ) return WRC_Abort;` | 0,127308 | OOM only (allocation failure; the port has no allocation failure path) |
| exprListAppendList | window.c:910 | `if( db->mallocFailed ){` | 0,234084 | OOM only (allocation failure; the port has no allocation failure path) |
| exprListAppendList | window.c:925 | `if( pList ) pList->a[nInit+i].fg.sortFlags = pAppend->a[i].fg.sortFlags;` | 234084,0 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowRewrite | window.c:968 | `&& ALWAYS((p->selFlags & SF_WinRewrite)==0)` | 56434,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3WindowRewrite | window.c:969 | `&& ALWAYS(!IN_RENAME_OBJECT)` | 56434,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3WindowRewrite | window.c:989 | `if( pTab==0 ){` | 0,56434 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowRewrite | window.c:1084 | `if( p->pSrc==0 ){` | 0,56434 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowRewrite | window.c:1086 | `}else if( sqlite3SrcItemAttachSubquery(pParse, &p->pSrc->a[0], pSub, 0) ){` | 56434,0 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowRewrite | window.c:1110 | `if( db->mallocFailed ) rc = SQLITE_NOMEM;` | 0,56434 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowOffsetExpr | window.c:1171 | `if( IN_RENAME_OBJECT ) sqlite3RenameExprUnmap(pParse, pExpr);` | 0,3088 | IN_RENAME_OBJECT (ALTER TABLE RENAME is a statement kind the generator does not port) |
| sqlite3WindowAlloc | window.c:1229 | `if( pWin==0 ) goto windowAllocErr;` | 0,83378 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowAlloc | window.c:1233 | `if( eExclude==0 && OptimizationDisabled(pParse->db, SQLITE_WindowFunc) ){` | 70458,12920,0,70458 | SQLITE_WindowFunc optimization disabled (sqlite3_test_control only; not reachable from SQL) |
| sqlite3WindowAttach | window.c:1315 | `if( p ){` | 75614,0 | sqlite3WindowAttach with p==0: only after an OOM in sqlite3ExprFunction |
| sqlite3WindowLink | window.c:1339 | `if( pSel ){` | 79242,0 | pSel==0: a window function outside any SELECT name context (always an error before: NC_AllowWin clear) |
| sqlite3WindowCompare | window.c:1367 | `if( NEVER(p1==0) \|\| NEVER(p2==0) ) return 1;` | 18436,0,0,18436 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3WindowCodeInit | window.c:1450 | `if( pKeyInfo && pWin->pWFunc->zName[1]=='i' ){` | 4582,0,2192,2390 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowDup | window.c:2390 | `if( ALWAYS(p) ){` | 21530,0 | ALWAYS / NEVER invariant (the other outcome is impossible by construction) |
| sqlite3WindowDup | window.c:2392 | `if( pNew ){` | 21530,0 | OOM only (allocation failure; the port has no allocation failure path) |
| sqlite3WindowListDup | window.c:2428 | `if( *pp==0 ) break;` | 0,530 | OOM only (allocation failure; the port has no allocation failure path) |

