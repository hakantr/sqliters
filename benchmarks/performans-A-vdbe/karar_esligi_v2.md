# V2 decision parity (karar eşliği): tables in FROM — schema adapter, sorter, whereexpr.c / where.c / wherecode.c, select.c

Scope: step V2 of the port of C SQLite 3.54.1's code generator. Oracle: `target/perf-work/bin/sqlite3` and the
amalgamation `target/sqlite-ref/amalg`. Worktree `/home/hakan/github/sqliters/.claude/worktrees/vdbe` (branch
`performans-A-vdbe`, uncommitted on top of c2bef41). The engine's executor and fast paths still execute every
statement; the generator's program is what `EXPLAIN` and `EXPLAIN QUERY PLAN` print when it supports the statement.

## 0. How a decision is proven: the layers

| Layer | What is compared | Tool | Corpus | Result |
|---|---|---|---|---|
| terms | WhereClause after `sqlite3WhereExprAnalyze()` (every WhereTerm: operator, cursor, column, prereq masks, flags, truthProb, parent/child, OR/AND sub-clauses): instrumented scratch amalgamation (`wdump/patch_where.py`) vs `where_.rs` dump | `wdiff.py` | plan_a + plan_b (39 000 statements, 780 schemas) | terms statements with a dump 32008 identical 32008 different 0 |
| loops | every WhereLoop offered to `whereLoopInsert()` (add / replace / with / delete / skip / or) with flags, nEq, nBtm, nTop, nSkip, nLTerm, iSortIdx, rSetup, rRun, nOut, prereq | `wdiff.py` | same | loops statements with a dump 31443 identical 31435 different 8 |
| paths | result of each `wherePathSolver()` call (cost, row estimate, isOrdered, revMask, nOBSat, eDistinct, the loops) | `wdiff.py` | same | paths statements with a dump 31513 identical 31509 different 4 |
| plan text | `EXPLAIN QUERY PLAN`, shell level, byte for byte | `expdiff.py --eqp` | same | statements 39000; generator accepted 35526: identical 35525, DIFFERENT 1; unsupported (fallback) 3474 (of which same as C anyway 221); not reached (old parser/other) 0 (same 0) |
| listing | `EXPLAIN`, shell level, byte for byte (also the error text + error position for statements that do not compile) | `expdiff.py` | same | statements 39000; generator accepted 35526: identical 35525, DIFFERENT 1; unsupported (fallback) 3474 (of which same as C anyway 11); not reached (old parser/other) 0 (same 0) |
| listing / plan, all statement kinds | same two comparisons, statements executed between the EXPLAINs | `expdiff.py --exec [--eqp]` | `v1/corpus/all_kinds.sql` (39 300) | EXPLAIN: statements 39300; generator accepted 13659: identical 13279, DIFFERENT 380; unsupported (fallback) 25182 (of which same as C anyway 4601); not reached (old parser/other) 459 (same 42)<br>EQP: statements 39300; generator accepted 13659: identical 13379, DIFFERENT 280; unsupported (fallback) 25182 (of which same as C anyway 21979); not reached (old parser/other) 459 (same 42) |
| library level | port without the engine (`cgexplain`, no schema) vs C library | `v1/cgdiff.py` | V1 corpora (54 613) | 54 604 identical, 0 different, 9 unsupported |
| library level with schema | `plan_tests.rs` (16 tests: listings, EQP rows incl. P3, one planner dump) and `c_align_v2_tests.rs` (5 tests through `Connection`) with C-produced expectations | `cargo test` | — | pass |
| C-side coverage | gcov of the functions of section 1 over the accepted statements | `covrun_v2.py` | plan_a + plan_b | section 4 |

The loop / path differences that remain (and the one listing difference of the plan corpus) all come from one
recorded defect of the engine's ANALYZE (`DIFFS_OUTSIDE_TASK.md` D10: a single-column UNIQUE index with NULLs gets
`N 2` where C writes `N 1`), i.e. the two sides plan with different sqlite_stat1 content. The remaining differences
of the all-kinds corpus are classified in section 5.

A statement is either reported "unsupported" (the engine then keeps its old output) or must be byte-identical;
nothing is approximated.

## 1. C function → Rust item
Paths are under `crates/`. "cov" = C lines executed % / branches taken at least once % by the accepted corpus (gcov, section 4); `n/a` = inlined or not a separate function in the -O0 build.


### build.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3FinishCoding` | `sqlite3_finish_coding` | `sqliters-codegen/src/build.rs` | 67.3 / 59.5 | 13 |
| `sqlite3CodeVerifySchema` | `sqlite3_code_verify_schema` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 | 1 |
| `sqlite3BeginWriteOperation` | `sqlite3_begin_write_operation` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 | 1 |
| `sqlite3MultiWrite` | `sqlite3_multi_write` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 | 1 |
| `sqlite3MayAbort` | `sqlite3_may_abort` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 | 1 |
| `sqlite3NameFromToken` | `sqlite3_name_from_token` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 | 0 |
| `sqlite3IdListAppend` | `sqlite3_id_list_append` | `sqliters-codegen/src/build.rs` | 80.0 / 50.0 | 3 |
| `sqlite3IdListIndex` | `sqlite3_id_list_index` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 | 0 |
| `sqlite3SrcListAppend` | `sqlite3_src_list_append` | `sqliters-codegen/src/build.rs` | 91.3 / 83.3 | 2 |
| `sqlite3SrcItemAttachSubquery` | `sqlite3_src_item_attach_subquery` | `sqliters-codegen/src/build.rs` | 66.7 / 60.0 | 4 |
| `sqlite3SrcListAppendFromTerm` | `sqlite3_src_list_append_from_term` | `sqliters-codegen/src/build.rs` | 53.3 / 46.9 | 9 |
| `sqlite3SrcListIndexedBy` | `sqlite3_src_list_indexed_by` | `sqliters-codegen/src/build.rs` | 100.0 / 75.0 | 2 |
| `sqlite3SrcListAppendList` | `sqlite3_src_list_append_list` | `sqliters-codegen/src/build.rs` | 90.9 / 50.0 | 2 |
| `sqlite3SrcListFuncArgs` | `sqlite3_src_list_func_args` | `sqliters-codegen/src/build.rs` | 0.0 / 0.0 | 1 |
| `sqlite3SrcListShiftJoinType` | `sqlite3_src_list_shift_join_type` | `sqliters-codegen/src/build.rs` | 100.0 / 85.7 | 2 |
| `sqlite3SubqueryDetach` | `sqlite3_subquery_detach` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ColumnColl` | `sqlite3_column_coll` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `sqlite3DefaultRowEst` | `sqlite3_default_row_est` | `sqliters-codegen/src/schema.rs` | 92.3 / 87.5 | 1 |
| `isDupColumn` | `is_dup_column` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `recomputeColumnsNotIndexed` | `recompute_columns_not_indexed` | `sqliters-codegen/src/schema.rs` | 100.0 / 75.0 | 2 |
| `estimateIndexWidth` | `estimate_index_width` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `convertToWithoutRowidTable` | `convert_to_without_rowid_table` | `sqliters-codegen/src/schema.rs` | 89.9 / 82.3 | 11 |
| `sqlite3PrimaryKeyIndex` | `sqlite3_primary_key_index` | `sqliters-codegen/src/schema.rs` | 100.0 / 75.0 | 1 |
| `sqlite3TableColumnToIndex` | `sqlite3_table_column_to_index` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `sqlite3StorageColumnToTable` | `sqlite3_storage_column_to_table` | `sqliters-codegen/src/schema.rs` | 60.0 / 16.7 | 3 |
| `sqlite3TableColumnToStorage` | `sqlite3_table_column_to_storage` | `sqliters-codegen/src/schema.rs` | 28.6 / 10.0 | 4 |
| `sqlite3FindTable` | `sqlite3_find_table` | `sqliters-codegen/src/schema.rs` | 83.9 / 80.0 | 7 |
| `sqlite3LocateTable` | `sqlite3_locate_table` | `sqliters-codegen/src/schema.rs` | 80.0 / 55.9 | 8 |
| `sqlite3LocateTableItem` | `sqlite3_locate_table_item` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `sqlite3KeyInfoOfIndex` | `sqlite3_key_info_of_index` | `sqliters-codegen/src/schema.rs` | 71.4 / 56.2 | 5 |
| `sqlite3TableLock` | `sqlite3_table_lock` | `sqliters-codegen/src/schema.rs` | 75.0 / 75.0 | 1 |
| `sqlite3SrcListAssignCursors` | `sqlite3_src_list_assign_cursors` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 | 1 |
| `sqlite3ViewGetColumnNames` | `sqlite3_view_get_column_names` | `sqliters-codegen/src/select.rs` | 100.0 / 75.0 | 1 |
| `sqlite3IndexBloomable` | `sqlite3_index_bloomable` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |

### attach.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `fixExprCb` | `fix_expr_cb` | `sqliters-codegen/src/schema.rs` | 55.6 / 33.3 | 3 |
| `fixSelectCb` | `fix_select_cb` | `sqliters-codegen/src/schema.rs` | 61.5 / 45.8 | 9 |

### resolve.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3MatchEName` | `sqlite3_match_e_name` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 | 9 |
| `sqlite3ExprColUsed` | `sqlite3_expr_col_used` | `sqliters-codegen/src/resolve.rs` | 75.0 / 25.0 | 4 |
| `extendFJMatch` | `extend_fj_match` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 | 1 |
| `isValidSchemaTableName` | `is_valid_schema_table_name` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 | 7 |
| `sqlite3CreateColumnExpr` | `sqlite3_create_column_expr` | `sqliters-codegen/src/resolve.rs` | 85.7 / 40.0 | 4 |
| `lookupName` | `lookup_name` | `sqliters-codegen/src/resolve.rs` | 45.9 / 43.5 | 70 |
| `sqlite3ResolveSelfReference` | `sqlite3_resolve_self_reference` | `sqliters-codegen/src/resolve.rs` | 100.0 / 75.0 | 2 |

### expr.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3TableColumnAffinity` | `sqlite3_table_column_affinity` | `sqliters-codegen/src/expr.rs` | 100.0 / 75.0 | 1 |
| `sqlite3ExprCollSeqMatch` | `sqlite3_expr_coll_seq_match` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3IndexAffinityOk` | `sqlite3_index_affinity_ok` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprCompareCollSeq` | `sqlite3_expr_compare_coll_seq` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3SelectExprHeight` | `sqlite3_select_expr_height` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprSetErrorOffset` | `sqlite3_expr_set_error_offset` | `sqliters-codegen/src/expr.rs` | 100.0 / 50.0 | 2 |
| `sqlite3Expr` | `sqlite3_expr` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprOrderByAggregateError` | `sqlite3_expr_order_by_aggregate_error` | `sqliters-codegen/src/expr.rs` | 0.0 / 100.0 | 0 |
| `dupedExprStructSize` | `duped_expr_struct_size` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 | 1 |
| `sqlite3ExprDup` | `sqlite3_expr_dup` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprListCheckLength` | `sqlite3_expr_list_check_length` | `sqliters-codegen/src/expr.rs` | 80.0 / 50.0 | 1 |
| `sqlite3SelectWalkFail` | `sqlite3_select_walk_fail` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprIsConstant` | `sqlite3_expr_is_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprIsConstantNotJoin` | `sqlite3_expr_is_constant_not_join` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprIsTableConstant` | `sqlite3_expr_is_table_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `exprSelectWalkTableConstant` | `expr_select_walk_table_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprIsSingleTableConstraint` | `sqlite3_expr_is_single_table_constraint` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `exprImpliesNotNull` | `expr_implies_not_null` | `sqliters-codegen/src/expr.rs` | 80.8 / 69.2 | 5 |
| `sqlite3ExprIsNotTrue` | `sqlite3_expr_is_not_true` | `sqliters-codegen/src/expr.rs` | 0.0 / 0.0 | 3 |
| `sqlite3ExprIsIIF` | `sqlite3_expr_is_iif` | `sqliters-codegen/src/expr.rs` | 37.5 / 20.8 | 9 |
| `sqlite3ExprImpliesExpr` | `sqlite3_expr_implies_expr` | `sqliters-codegen/src/expr.rs` | 69.2 / 57.1 | 4 |
| `bothImplyNotNullRow` | `both_imply_not_null_row` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `impliesNotNullRow` | `implies_not_null_row` | `sqliters-codegen/src/expr.rs` | 97.1 / 80.7 | 5 |
| `sqlite3ExprImpliesNonNullRow` | `sqlite3_expr_implies_non_null_row` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 | 1 |
| `exprIdxCover` | `expr_idx_cover` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprCoveredByIndex` | `sqlite3_expr_covered_by_index` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprIsConstantOrFunction` | `sqlite3_expr_is_constant_or_function` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprNeedsNoAffinityChange` | `sqlite3_expr_needs_no_affinity_change` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 | 3 |
| `sqlite3IsRowid` | `sqlite3_is_rowid` | `sqliters-codegen/src/expr.rs` | 60.0 / 33.3 | 3 |
| `sqlite3ExprCompareSkip` | `sqlite3_expr_compare_skip` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3TouchRegister` | `sqlite3_touch_register` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3FindInIndex` | `sqlite3_find_in_index` | `sqliters-codegen/src/exprcode.rs` | 98.1 / 93.9 | 6 |
| `sqlite3CodeRhsOfIN` | `sqlite3_code_rhs_of_in` | `sqliters-codegen/src/exprcode.rs` | 93.3 / 82.3 | 11 |
| `sqlite3CodeSubselect` | `sqlite3_code_subselect` | `sqliters-codegen/src/exprcode.rs` | 66.7 / 50.0 | 9 |
| `sqlite3ExprCodeLoadIndexColumn` | `sqlite3_expr_code_load_index_column` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprCodeGetColumnOfTable` | `sqlite3_expr_code_get_column_of_table` | `sqliters-codegen/src/exprcode.rs` | 43.5 / 66.7 | 3 |
| `sqlite3IndexedExprLookup` | `sqlite3_indexed_expr_lookup` | `sqliters-codegen/src/exprcode.rs` | 53.3 / 46.7 | 8 |
| `exprPartidxExprLookup` | `expr_partidx_expr_lookup` | `sqliters-codegen/src/exprcode.rs` | 23.5 / 41.7 | 4 |
| `sqlite3ExprCodeTarget` | `sqlite3_expr_code_target` | `sqliters-codegen/src/exprcode.rs` | 45.3 / 44.0 | 60 |
| `sqlite3ExprIfFalseDup` | `sqlite3_expr_if_false_dup` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 50.0 | 1 |
| `sqlite3RowidAlias` | `sqlite3_rowid_alias` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 | 2 |

### select.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `constInsert` | `const_insert` | `sqliters-codegen/src/flatten.rs` | 95.0 / 90.9 | 2 |
| `findConstInWhere` | `find_const_in_where` | `sqliters-codegen/src/flatten.rs` | 100.0 / 93.8 | 1 |
| `propagateConstantExprRewriteOne` | `propagate_constant_expr_rewrite_one` | `sqliters-codegen/src/flatten.rs` | 100.0 / 90.0 | 2 |
| `propagateConstantExprRewrite` | `propagate_constant_expr_rewrite` | `sqliters-codegen/src/flatten.rs` | 100.0 / 91.7 | 1 |
| `propagateConstants` | `propagate_constants` | `sqliters-codegen/src/flatten.rs` | 100.0 / 80.0 | 2 |
| `selectCheckOnClausesExpr` | `select_check_on_clauses_expr` | `sqliters-codegen/src/flatten.rs` | 80.0 / 71.4 | 6 |
| `selectCheckOnClausesSelect` | `select_check_on_clauses_select` | `sqliters-codegen/src/flatten.rs` | 0.0 / 0.0 | 1 |
| `sqlite3SelectCheckOnClauses` | `sqlite3_select_check_on_clauses` | `sqliters-codegen/src/flatten.rs` | 61.5 / 25.0 | 5 |
| `substExpr` | `subst_expr` | `sqliters-codegen/src/flatten.rs` | 67.8 / 72.9 | 8 |
| `substExprList` | `subst_expr_list` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `substSelect` | `subst_select` | `sqliters-codegen/src/flatten.rs` | 93.8 / 83.3 | 2 |
| `recomputeColumnsUsedExpr` | `recompute_columns_used_expr` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `recomputeColumnsUsed` | `recompute_columns_used` | `sqliters-codegen/src/flatten.rs` | 100.0 / 50.0 | 1 |
| `srclistRenumberCursors` | `srclist_renumber_cursors` | `sqliters-codegen/src/flatten.rs` | 100.0 / 75.0 | 1 |
| `renumberCursorDoMapping` | `renumber_cursor_do_mapping` | `sqliters-codegen/src/flatten.rs` | 100.0 / 75.0 | 1 |
| `renumberCursorsCb` | `renumber_cursors_cb` | `sqliters-codegen/src/flatten.rs` | 100.0 / 83.3 | 1 |
| `renumberCursors` | `renumber_cursors` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `findLeftmostExprlist` | `find_leftmost_exprlist` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `compoundHasDifferentAffinities` | `compound_has_different_affinities` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 | 0 |
| `flattenSubquery` | `flatten_subquery` | `sqliters-codegen/src/flatten.rs` | 91.6 / 77.0 | 31 |
| `pushDownWhereTerms` | `push_down_where_terms` | `sqliters-codegen/src/flatten.rs` | 79.6 / 59.6 | 10 |
| `disableUnusedSubqueryResultColumns` | `disable_unused_subquery_result_columns` | `sqliters-codegen/src/flatten.rs` | 90.9 / 76.7 | 6 |
| `countOfViewOptimization` | `count_of_view_optimization` | `sqliters-codegen/src/flatten.rs` | 94.4 / 78.0 | 11 |
| `existsToJoin` | `exists_to_join` | `sqliters-codegen/src/flatten.rs` | 100.0 / 68.8 | 10 |
| `generateOutputSubroutine` | `generate_output_subroutine` | `sqliters-codegen/src/merge.rs` | 50.0 / 50.0 | 5 |
| `multiSelectByMerge` | `multi_select_by_merge` | `sqliters-codegen/src/merge.rs` | 98.6 / 85.9 | 10 |
| `sqlite3ColumnIndex` | `sqlite3_column_index` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 | 0 |
| `sqlite3SelectDestInit` | `sqlite3_select_dest_init` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `findRightmost` | `find_rightmost` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 | 1 |
| `convertCompoundSelectToSubquery` | `convert_compound_select_to_subquery` | `sqliters-codegen/src/select.rs` | 27.8 / 54.2 | 7 |
| `sqlite3JoinType` | `sqlite3_join_type` | `sqliters-codegen/src/select.rs` | 69.2 / 54.5 | 8 |
| `sqlite3SrcItemColumnUsed` | `sqlite3_src_item_column_used` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 | 1 |
| `tableAndColumnIndex` | `table_and_column_index` | `sqliters-codegen/src/select.rs` | 100.0 / 80.0 | 1 |
| `sqlite3SetJoinExpr` | `sqlite3_set_join_expr` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 | 1 |
| `unsetJoinExpr` | `unset_join_expr` | `sqliters-codegen/src/select.rs` | 100.0 / 95.5 | 1 |
| `sqlite3ProcessJoin` | `sqlite3_process_join` | `sqliters-codegen/src/select.rs` | 92.2 / 63.9 | 14 |
| `sqlite3IndexedByLookup` | `sqlite3_indexed_by_lookup` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `cannotBeFunction` | `cannot_be_function` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 | 1 |
| `searchWith` | `search_with` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 | 4 |
| `resolveFromTermToCte` | `resolve_from_term_to_cte` | `sqliters-codegen/src/select.rs` | 2.9 / 1.3 | 34 |
| `inAnyUsingClause` | `in_any_using_clause` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 | 1 |
| `selectExpander` | `select_expander` | `sqliters-codegen/src/select.rs` | 67.9 / 49.5 | 61 |
| `sqlite3ColumnsFromExprList` | `sqlite3_columns_from_expr_list` | `sqliters-codegen/src/select.rs` | 57.9 / 41.4 | 21 |
| `columnTypeImpl` | `column_type` | `sqliters-codegen/src/select.rs` | 94.7 / 78.3 | 5 |
| `sqlite3SubqueryColumnTypes` | `sqlite3_subquery_column_types` | `sqliters-codegen/src/select.rs` | 92.0 / 86.5 | 6 |
| `checkSubqueryNestingDepth` | `check_subquery_nesting_depth` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 | 1 |
| `sqlite3ResultSetOfSelect` | `sqlite3_result_set_of_select` | `sqliters-codegen/src/select.rs` | 88.0 / 70.0 | 3 |
| `sqlite3GenerateColumnNames` | `sqlite3_generate_column_names` | `sqliters-codegen/src/select.rs` | 90.6 / 73.1 | 5 |
| `sqlite3SelectAddTypeInfo` | `sqlite3_select_add_type_info` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `innerLoopLoadRow` | `inner_loop_load_row` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `makeSorterRecord` | `make_sorter_record` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `pushOntoSorter` | `push_onto_sorter` | `sqliters-codegen/src/select.rs` | 95.2 / 80.0 | 4 |
| `selectInnerLoop` | `select_inner_loop` | `sqliters-codegen/src/select.rs` | 67.1 / 74.0 | 15 |
| `explainTempTable` | `explain_temp_table` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `generateSortTail` | `generate_sort_tail` | `sqliters-codegen/src/select.rs` | 77.2 / 76.4 | 7 |
| `multiSelect` | `multi_select` | `sqliters-codegen/src/select.rs` | 92.3 / 72.5 | 8 |
| `resetAccumulator` | `reset_accumulator` | `sqliters-codegen/src/select.rs` | 48.4 / 41.7 | 7 |
| `isSimpleCount` | `is_simple_count` | `sqliters-codegen/src/select.rs` | 100.0 / 72.7 | 6 |
| `explainSimpleCount` | `explain_simple_count` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `isSelfJoinView` | `is_self_join_view` | `sqliters-codegen/src/select.rs` | 94.1 / 80.0 | 2 |
| `fromClauseTermCanBeCoroutine` | `from_clause_term_can_be_coroutine` | `sqliters-codegen/src/select.rs` | 86.4 / 66.7 | 6 |
| `sqlite3Select` | `sqlite3_select` | `sqliters-codegen/src/select.rs` | 59.7 / 56.4 | 97 |

### whereexpr.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ExprIsLikeOperator` | `sqlite3_expr_is_like_operator` | `sqliters-codegen/src/where_.rs` | 80.0 / 75.0 | 1 |
| `whereClauseInsert` | `where_clause_insert` | `sqliters-codegen/src/whereexpr.rs` | 82.6 / 66.7 | 3 |
| `allowedOp` | `allowed_op` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `exprCommute` | `expr_commute` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 | 2 |
| `operatorMask` | `operator_mask` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `isLikeOrGlob` | `is_like_or_glob` | `sqliters-codegen/src/whereexpr.rs` | 64.8 / 48.8 | 22 |
| `transferJoinMarkings` | `transfer_join_markings` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 | 1 |
| `markTermAsChild` | `mark_term_as_child` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `whereNthSubterm` | `where_nth_subterm` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `whereCombineDisjuncts` | `where_combine_disjuncts` | `sqliters-codegen/src/whereexpr.rs` | 91.7 / 83.3 | 4 |
| `exprAnalyzeOrTerm` | `expr_analyze_or_term` | `sqliters-codegen/src/whereexpr.rs` | 93.3 / 78.6 | 16 |
| `termIsEquivalence` | `term_is_equivalence` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 95.5 | 1 |
| `exprSelectUsage` | `expr_select_usage` | `sqliters-codegen/src/whereexpr.rs` | 94.7 / 75.0 | 3 |
| `exprMightBeIndexed2` | `expr_might_be_indexed` | `sqliters-codegen/src/whereexpr.rs` | 69.2 / 78.6 | 2 |
| `sqlite3WhereExprUsageFull` | `sqlite3_where_expr_usage_nn` | `sqliters-codegen/src/whereexpr.rs` | 80.0 / 88.9 | 2 |
| `sqlite3WhereExprUsage` | `sqlite3_where_expr_usage` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereExprListUsage` | `sqlite3_where_expr_list_usage` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereExprAnalyze` | `sqlite3_where_expr_analyze` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereTabFuncArgs` | `sqlite3_where_tab_func_args` | `sqliters-codegen/src/whereexpr.rs` | 7.4 / 5.6 | 8 |
| `sqlite3WhereLoopBloomable` | `sqlite3_where_loop_bloomable` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 | 1 |

### where.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3IndexColumnAffinity` | `sqlite3_index_column_affinity` | `sqliters-codegen/src/schema.rs` | n/a |  |
| `sqlite3WhereOrderByLimitOptLabel` | `sqlite3_where_order_by_limit_opt_label` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereMinMaxOptEarlyOut` | `sqlite3_where_min_max_opt_early_out` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereOkOnePass` | `sqlite3_where_ok_one_pass` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereOrInsert` | `where_or_insert` | `sqliters-codegen/src/where_.rs` | 100.0 / 95.0 | 1 |
| `sqlite3WhereGetMask` | `sqlite3_where_get_mask` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `createMask` | `create_mask` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereRightSubexprIsColumn` | `where_right_subexpr_is_column` | `sqliters-codegen/src/where_.rs` | 100.0 / 66.7 | 1 |
| `indexInAffinityOk` | `index_in_affinity_ok` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereScanNext` | `where_scan_next` | `sqliters-codegen/src/where_.rs` | 98.1 / 91.4 | 4 |
| `whereScanInit` | `where_scan_init` | `sqliters-codegen/src/where_.rs` | 96.4 / 90.0 | 1 |
| `sqlite3WhereFindTerm` | `sqlite3_where_find_term` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `findIndexCol` | `find_index_col` | `sqliters-codegen/src/where_.rs` | 100.0 / 78.6 | 3 |
| `indexColumnNotNull` | `index_column_not_null` | `sqliters-codegen/src/where_.rs` | 85.7 / 75.0 | 1 |
| `isDistinctRedundant` | `is_distinct_redundant` | `sqliters-codegen/src/where_.rs` | 100.0 / 90.0 | 3 |
| `estLog` | `est_log` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `constraintCompatibleWithOuterJoin` | `constraint_compatible_with_outer_join` | `sqliters-codegen/src/where_.rs` | 87.5 / 87.5 | 1 |
| `columnIsGoodIndexCandidate` | `column_is_good_index_candidate` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `termCanDriveIndex` | `term_can_drive_index` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `constructAutomaticIndex` | `construct_automatic_index` | `sqliters-codegen/src/where_.rs` | 93.6 / 84.6 | 10 |
| `sqlite3ConstructBloomFilter` | `sqlite3_construct_bloom_filter` | `sqliters-codegen/src/where_.rs` | 98.3 / 92.3 | 2 |
| `whereRangeAdjust` | `where_range_adjust` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereRangeScanEst` | `where_range_scan_est` | `sqliters-codegen/src/where_.rs` | 100.0 / 91.7 | 1 |
| `whereLoopCheaperProperSubset` | `where_loop_cheaper_proper_subset` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereLoopAdjustCost` | `where_loop_adjust_cost` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereLoopFindLesser` | `where_loop_find_lesser` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereLoopInsert` | `where_loop_insert` | `sqliters-codegen/src/where_.rs` | 94.4 / 80.8 | 4 |
| `whereLoopOutputAdjust` | `where_loop_output_adjust` | `sqliters-codegen/src/where_.rs` | 100.0 / 94.2 | 3 |
| `whereRangeVectorLen` | `where_range_vector_len` | `sqliters-codegen/src/where_.rs` | 95.5 / 75.0 | 5 |
| `whereLoopAddBtreeIndex` | `where_loop_add_btree_index` | `sqliters-codegen/src/where_.rs` | 97.6 / 90.6 | 10 |
| `indexMightHelpWithOrderBy` | `index_might_help_with_order_by` | `sqliters-codegen/src/where_.rs` | 94.1 / 92.9 | 2 |
| `whereUsablePartialIndex` | `where_usable_partial_index` | `sqliters-codegen/src/where_.rs` | 86.7 / 77.3 | 4 |
| `exprIsCoveredByIndex` | `expr_is_covered_by_index` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereIsCoveringIndexWalkCallback` | `where_is_covering_index_walk_callback` | `sqliters-codegen/src/where_.rs` | 100.0 / 92.9 | 1 |
| `whereIsCoveringIndex` | `where_is_covering_index` | `sqliters-codegen/src/where_.rs` | 82.6 / 50.0 | 4 |
| `wherePartIdxExpr` | `where_part_idx_expr` | `sqliters-codegen/src/where_.rs` | 93.8 / 75.0 | 7 |
| `whereLoopAddBtree` | `where_loop_add_btree` | `sqliters-codegen/src/where_.rs` | 100.0 / 87.3 | 15 |
| `whereLoopAddOr` | `where_loop_add_or` | `sqliters-codegen/src/where_.rs` | 98.4 / 87.5 | 4 |
| `whereLoopAddAll` | `where_loop_add_all` | `sqliters-codegen/src/where_.rs` | 82.6 / 68.8 | 6 |
| `wherePathMatchSubqueryOB` | `where_path_match_subquery_ob` | `sqliters-codegen/src/where_.rs` | 0.0 / 0.0 | 12 |
| `wherePathSatisfiesOrderBy` | `where_path_satisfies_order_by` | `sqliters-codegen/src/where_.rs` | 94.0 / 87.5 | 16 |
| `whereSortingCost` | `where_sorting_cost` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `computeMxChoice` | `compute_mx_choice` | `sqliters-codegen/src/where_.rs` | 100.0 / 94.0 | 3 |
| `whereLoopIsNoBetter` | `where_loop_is_no_better` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `wherePathSolver` | `where_path_solver` | `sqliters-codegen/src/where_.rs` | 94.7 / 90.6 | 9 |
| `whereInterstageHeuristic` | `where_interstage_heuristic` | `sqliters-codegen/src/where_.rs` | 93.3 / 85.7 | 2 |
| `whereShortCut` | `where_short_cut` | `sqliters-codegen/src/where_.rs` | 100.0 / 91.3 | 4 |
| `whereOmitNoopJoin` | `where_omit_noop_join` | `sqliters-codegen/src/where_.rs` | 97.1 / 97.1 | 1 |
| `whereCheckIfBloomFilterIsUseful` | `where_check_if_bloom_filter_is_useful` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 | 0 |
| `whereAddIndexedExpr` | `where_add_indexed_expr` | `sqliters-codegen/src/where_.rs` | 96.0 / 75.0 | 4 |
| `whereReverseScanOrder` | `where_reverse_scan_order` | `sqliters-codegen/src/where_.rs` | 0.0 / 0.0 | 5 |
| `sqlite3WhereBegin` | `sqlite3_where_begin` | `sqliters-codegen/src/where_.rs` | 89.1 / 79.3 | 38 |
| `sqlite3WhereEnd` | `sqlite3_where_end` | `sqliters-codegen/src/where_.rs` | 89.0 / 86.1 | 14 |
| `sqlite3WhereOutputRowCount` | `output_row_count` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereIsDistinct` | `is_distinct` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereIsOrdered` | `is_ordered` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereContinueLabel` | `continue_label` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereBreakLabel` | `break_label` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereIsSorted` | `is_sorted` | `sqliters-codegen/src/whereint.rs` | 0.0 / 100.0 | 0 |
| `sqlite3WhereUsesDeferredSeek` | `uses_deferred_seek` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 | 0 |

### wherecode.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `explainIndexColumnName` | `explain_index_column_name` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `explainAppendTerm` | `explain_append_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `explainIndexRange` | `explain_index_range` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereAddExplainText` | `sqlite3_where_add_explain_text` | `sqliters-codegen/src/wherecode.rs` | 94.3 / 84.0 | 6 |
| `sqlite3WhereExplainOneScan` | `sqlite3_where_explain_one_scan` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 87.5 | 1 |
| `sqlite3WhereExplainBloomFilter` | `sqlite3_where_explain_bloom_filter` | `sqliters-codegen/src/wherecode.rs` | 95.7 / 87.5 | 1 |
| `disableTerm` | `disable_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 93.8 | 1 |
| `codeApplyAffinity` | `code_apply_affinity` | `sqliters-codegen/src/wherecode.rs` | 91.7 / 91.7 | 1 |
| `updateRangeAffinityStr` | `update_range_affinity_str` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `adjustOrderByCol` | `adjust_order_by_col` | `sqliters-codegen/src/wherecode.rs` | 18.2 / 8.3 | 6 |
| `removeUnindexableInClauseTerms` | `remove_unindexable_in_clause_terms` | `sqliters-codegen/src/wherecode.rs` | 97.6 / 71.4 | 8 |
| `codeINTerm` | `code_in_term` | `sqliters-codegen/src/wherecode.rs` | 98.5 / 94.4 | 3 |
| `codeEqualityTerm` | `code_equality_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `codeAllEqualityTerms` | `code_all_equality_terms` | `sqliters-codegen/src/wherecode.rs` | 97.4 / 90.0 | 3 |
| `whereLikeOptimizationStringFixup` | `where_like_optimization_string_fixup` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `codeDeferredSeek` | `code_deferred_seek` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 75.0 | 2 |
| `codeExprOrVector` | `code_expr_or_vector` | `sqliters-codegen/src/wherecode.rs` | 72.7 / 75.0 | 2 |
| `whereApplyPartialIndexConstraints` | `where_apply_partial_index_constraints` | `sqliters-codegen/src/wherecode.rs` | 80.0 / 87.5 | 1 |
| `filterPullDown` | `filter_pull_down` | `sqliters-codegen/src/wherecode.rs` | 78.3 / 70.0 | 3 |
| `whereLoopIsOneRow` | `where_loop_is_one_row` | `sqliters-codegen/src/wherecode.rs` | 88.9 / 80.0 | 2 |
| `whereExprIsReady` | `where_expr_is_ready` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WhereCodeOneLoopStart` | `sqlite3_where_code_one_loop_start` | `sqliters-codegen/src/wherecode.rs` | 85.8 / 79.8 | 49 |
| `sqlite3WhereRightJoinLoop` | `sqlite3_where_right_join_loop` | `sqliters-codegen/src/wherecode.rs` | 93.2 / 87.5 | 4 |

### vdbeaux.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3VdbeExplain` | `sqlite3_vdbe_explain` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 | 0 |
| `sqlite3VdbeExplainPop` | `sqlite3_vdbe_explain_pop` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 | 0 |
| `sqlite3VdbeExplainParent` | `sqlite3_vdbe_explain_parent` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 | 0 |
| `sqlite3VdbeSetP4KeyInfo` | `sqlite3_vdbe_set_p4_key_info` | `sqliters-codegen/src/schema.rs` | 100.0 / 50.0 | 1 |

### callback.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3LocateCollSeq` | `sqlite3_locate_coll_seq` | `sqliters-codegen/src/callback.rs` | 87.5 / 66.7 | 1 |

### delete.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3GenerateIndexKey` | `sqlite3_generate_index_key` | `sqliters-codegen/src/where_.rs` | 88.0 / 46.1 | 6 |

### func.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3IsLikeFunction` | `sqlite3_is_like_function` | `sqliters-codegen/src/whereexpr.rs` | 52.6 / 22.2 | 7 |

### hash.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3HashFind` | `hash_find` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |

### insert.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3OpenTable` | `sqlite3_open_table` | `sqliters-codegen/src/schema.rs` | 81.8 / 75.0 | 1 |
| `sqlite3IndexAffinityStr` | `sqlite3_index_affinity_str` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |

### main.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3IsBinary` | `sqlite3_is_binary` | `sqliters-codegen/src/exprcode.rs` | n/a |  |

### prepare.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3Prepare` | `prepare` | `sqliters-codegen/src/lib.rs` | 52.9 / 45.6 | 16 |
| `sqlite3ReadSchema` | `sqlite3_read_schema` | `sqliters-codegen/src/schema.rs` | 81.8 / 66.7 | 2 |

### update.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ColumnDefault` | `sqlite3_column_default` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 | 0 |

### utf.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3Utf8Read` | `utf8_read` | `sqliters-codegen/src/whereexpr.rs` | 50.0 / 12.5 | 3 |

### util.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ColumnType` | `sqlite3_column_type` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 | 0 |
| `sqlite3LogEst` | `sqlite3_log_est` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `sqlite3LogEstAdd` | `sqlite3_log_est_add` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 | 0 |
| `sqlite3HexToInt` | `hex_to_int` | `sqliters-codegen/src/util.rs` | 100.0 / 100.0 | 0 |
| `sqlite3StrICmp` | `str_ieq` | `sqliters-codegen/src/util.rs` | 100.0 / 100.0 | 0 |
| `sqlite3LogEstToInt` | `sqlite3_log_est_to_int` | `sqliters-codegen/src/where_.rs` | 100.0 / 87.5 | 1 |

### walker.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3WalkerDepthIncrease` | `sqlite3_walker_depth_increase` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 | 0 |
| `sqlite3WalkerDepthDecrease` | `sqlite3_walker_depth_decrease` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 | 0 |
| `sqlite3ExprWalkNoop` | `sqlite3_expr_walk_noop` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 | 0 |
| `sqlite3SelectWalkNoop` | `sqlite3_select_walk_noop` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 | 0 |

## 2. Decision tables (C source → rule → Rust)

Line numbers are of `sqlite/src/*.c` (3.54.1). "Verified by" names the layer of section 0 that compares the decision.

### 2.1 Schema objects as C builds them (build.c, analyze.c) — `schema.rs`
| C | Rule | Rust | Verified by |
|---|---|---|---|
| `sqlite3AddColumn` build.c | standard type names (ANY BLOB INT INTEGER REAL TEXT) set `eCType`, affinity from the table, `szEst=5` when affinity ≤ TEXT; any other type goes through `sqlite3AffinityType` (affinity + `szEst` from the first number in `CHAR(n)`-like types, `v/4+1`, max 255); no type: BLOB, `szEst=1` | `add_column`, `sqlite3_affinity_type_sz` | loop costs (`rRun` depends on `szTabRow` / `szIdxRow`) |
| `estimateTableWidth` / `estimateIndexWidth` | `szTabRow = LogEst(4*(Σ szEst + (iPKey<0)))`, `szIdxRow = LogEst(4*Σ szEst of index columns, 1 for rowid/expr)` | `build_table`, `estimate_index_width` | loop costs |
| `sqlite3CreateIndex` linking, build.c | a new index goes to the head of `pTab->pIndex`; `OE_Replace` indexes are moved behind the others | `link_index` | order of LOOP lines; ties in `whereLoopFindLesser` |
| order of creation | automatic indexes in the order of their constraints while CREATE TABLE is parsed (rowid form, `pPk==0`), then `convertToWithoutRowidTable`, then the CREATE INDEX statements in `sqlite_schema` order | `build_table` (adapter sorts by the number in `sqlite_autoindex_<t>_<n>` and by `sqlite_schema` rowid) | LOOP order |
| `sqlite3CreateIndex` column loop | `iColumn<0` (rowid or INTEGER PRIMARY KEY column) is stored as `iPKey` without touching `uniqNotNull`; a nullable column clears `uniqNotNull`; expression → `XN_EXPR`, `bHasExpr`, `uniqNotNull=0`; explicit `COLLATE` kept as written, otherwise the column's, otherwise the static BINARY (shown as empty in a KeyInfo; explicit BINARY shows `B`) | `create_index`, `index_decl_from_sql` (CREATE INDEX text is parsed and resolved with `sqlite3ResolveSelfReference`) | KeyInfo P4 (`k(n,…)`), `WHERE_ONEROW` in LOOP `ws` |
| `sqlite3AddNotNull` / `convertToWithoutRowidTable` / `sqlite3EndTable` (STRICT) | NOT NULL added to PRIMARY KEY columns *after* the automatic indexes exist, so their `uniqNotNull` reflects the declared NOT NULL only | `declared_not_null` (token scan of the CREATE TABLE text) + the two later steps in `build_table` | KeyInfo P4 `k(1,)` vs `k(2,,)` |
| `convertToWithoutRowidTable` | PK: duplicate columns removed, `uniqNotNull=1`, root = table root, all other columns appended with the column's collation; secondary indexes: rowid slot replaced by the PK columns not already present (`isDupColumn`), `bAscKeyBug` | `convert_to_without_rowid_table` | listings of WITHOUT ROWID tables |
| `resizeIndexObject` build.c:2261 | the resized `aiRowLogEst[]` has N entries, so `aiRowLogEst[nColumn]` (read by `whereLoopAddBtreeIndex` when `nEq==nColumn`) is the memory of `aiColumn[0]` | `Index.is_resized` + the read in `where_loop_add_btree_index` | LOOP `nOut` (found by the loop dump: 17 statements) |
| `sqlite3DefaultRowEst` | `a[0]=max(table nRowLogEst,99)` (raises the table's value too), −10 for a partial index; then 33,32,30,28,26,23…; last key column 0 if unique; entries past `nKeyCol` are 0 | `sqlite3_default_row_est`, array sized `nColumn+1` | LOOP `nOut` / `rRun` |
| `analysisLoader` analyze.c | index row: `aiRowLogEst` from the numbers, `unordered` / `sz=` / `noskipscan`, table `nRowLogEst = a[0]` unless partial; `idx` NULL **or naming no index** (or the table name without a PK index): the table's own row | engine `Schema::reload_stat1` (fallback added in V2) → adapter → `build_table` | plans with hand-written sqlite_stat1 |
| `sqlite3AnalysisLoad` | statistics of objects whose row disappeared are *kept* (only `TF_HasStat1` / `hasStat1` are cleared) | not reproduced: tracked (`DbSlot::c_stat_seen/stale/dirty`) and reported as unsupported | section 5 |
| `sqlite3CreateView` | `sqlite3FixSelect` (table references tied to the view's database unless TEMP, `EP_FromDDL`, `fromDDL`), `SF_View`, `EXPRDUP_REDUCE` copy (offsets and column fields zero), `TF_NoVisibleRowid` | `ensure_view_select`, `fix_select_cb`, `fix_expr_cb` | view statements of all three corpora, error offset −1 inside views |
| `sqlite3ViewGetColumnNames` | columns from a copy of the SELECT; `nTab` / `nSelect` restored; the copy's table references released (`nTabRef--`) | `sqlite3_view_get_column_names` | "too many references" message names the view |
| `sqlite3LocateTable` / `sqlite3FindTable` | TEMP, main, attached in order; `sqlite_schema` ↔ `sqlite_master` / `sqlite_temp_master` aliases; `fixedSchema` items only in their database | `sqlite3_find_table`, `sqlite3_locate_table(_item)`, `hash_find` | attached / temp tables of the corpora |

### 2.2 WHERE-clause terms (whereexpr.c) — `whereexpr.rs`  *(layer: term dump, 32 008 statements identical)*
| C | Rule | Rust |
|---|---|---|
| `sqlite3WhereSplit` | split on AND (or OR inside an OR term) into `WhereTerm`s, `truthProb` 1 or from `likelihood()` | `sqlite3_where_split`, `where_clause_insert` |
| `exprAnalyze` | prereq masks; commuted copy when both sides are columns (`TERM_VIRTUAL` copy, `iParent`), `TERM_IS` / `WO_IS`, `WO_ISNULL`, BETWEEN → two virtual `>=` `<=` terms, vector comparisons → one term per field + `TERM_SLICE`, `x IS NOT NULL` → virtual `x>NULL` (`TERM_VNULL`), ON-clause terms of outer joins keep their join mask | `expr_analyze` |
| `isLikeOrGlob` | LIKE / GLOB with a constant prefix → two range terms (`TERM_LIKEOPT`, upper bound = prefix with last byte +1, NOCASE upper/lower pair when the column is not BINARY-comparable), numeric-looking prefixes excluded (`sqlite3AtoF` check), a bound parameter marks the statement for re-prepare | `is_like_or_glob`, `atof_is_num` |
| `exprAnalyzeOrTerm` | OR of indexable terms on one table → `WO_OR` with a sub-WhereClause (`indexable` mask); all `col=const` on the same column → virtual `col IN (...)` | `expr_analyze_or_term`, `where_combine_disjuncts` |
| `termIsEquivalence` / `exprMightBeIndexed` | `WO_EQUIV` for transitive equalities; an expression that matches an indexed expression is indexable | `term_is_equivalence`, `expr_might_be_indexed` |

### 2.3 Loop candidates and costs (where.c) — `where_.rs`  *(layer: loop dump, every `whereLoopInsert` call)*
| C | Rule | Rust |
|---|---|---|
| `whereLoopAddBtree` | fake IPK index first for a rowid table, then `pTab->pIndex`; automatic index when no usable index (`rSetup = rLogSize + rSize`, +28 without real stats, −25 for a view/subquery; `nOut=43`); full scan `rRun = rSize+16`; covering-index scan `rSize + 1 + 15*szIdxRow/szTabRow`; partial index only when `whereUsablePartialIndex` | `where_loop_add_btree` |
| `whereLoopAddBtreeIndex` | per added term: `=` / `IS` / `IN` (`nIn`, cost of IN vs scan), `ISNULL`, ranges (`whereRangeScanEst` without STAT4: 1/4 per bound, 1/64 both), `WHERE_ONEROW` / `WHERE_UNQ_WANTED`, lookup cost `rRun = LogEstAdd(...)` with `szIdxRow/szTabRow` scaling, skip-scan when `aiRowLogEst[i+1] ≥ 42` and stat1 present | `where_loop_add_btree_index`, `where_range_scan_est`, `where_loop_output_adjust` |
| `whereLoopInsert` / `whereLoopFindLesser` / `whereLoopAdjustCost` / `whereLoopCheaperProperSubset` | keep a loop unless an existing one has the same or fewer prerequisites and lower cost; replace and delete dominated ones; proper-subset adjustment | `where_loop_insert`, `where_loop_find_lesser`, `where_loop_adjust_cost` |
| `whereLoopAddOr` | OR-optimization candidates from the sub-clauses (`WhereOrSet`, at most 3 costs per prerequisite set) | `where_loop_add_or`, `where_or_insert` |
| `whereLoopAddAll` | prerequisites that pin outer / CROSS / RIGHT joins and EXISTS-to-JOIN items | `where_loop_add_all` |
| `sqlite3WhereBegin` | Bloom filter decision (`whereCheckIfBloomFilterIsUseful`), omit-noop-join, star-query heuristic (`computeMxChoice`), covering-index detection (`whereIsCoveringIndex`), one-pass flags | `sqlite3_where_begin`, `where_omit_noop_join`, `where_check_if_bloom_filter_is_useful`, `compute_mx_choice` |

### 2.4 Path solver (where.c) — *(layer: path dump)*
| C | Rule | Rust |
|---|---|---|
| `wherePathSolver` | N-best search (`mxChoice` 1 / 5 / 12 / 18 by join size), cost = `LogEstAdd(rSetup, rRun + nRow)`, sorting cost added when the path does not satisfy ORDER BY (`whereSortingCost`), first-found wins ties; C's quirk `mxUnsort = aTo[0].nRow` kept | `where_path_solver`, `where_sorting_cost` |
| `wherePathSatisfiesOrderBy` | ORDER BY / GROUP BY / DISTINCT satisfied by index order, including reverse scans (`revMask`), `isOrderDistinct`, equality-constrained columns skipped | `where_path_satisfies_order_by` |
| `whereShortCut` | one-table query with a rowid or unique-index equality: no loop search (no LOOP / PATH lines on either side) | `where_short_cut` |
| `whereInterstageHeuristic` | between the two solver passes: disable loops that the first pass shows useless | `where_interstage_heuristic` |

### 2.5 Loop code (wherecode.c) — `wherecode.rs`  *(layer: EXPLAIN bytes)*
| C | Rule | Rust |
|---|---|---|
| `sqlite3WhereCodeOneLoopStart` | case per plan: co-routine subquery, rowid equality / IN (`SeekRowid`), rowid range, index range / equality (`SeekGE/GT/LE/LT`, `IdxGT…`, `SeekScan`, NULL-skipping, `DeferredSeek`), OR-optimization (`RowSetTest`, sub-`sqlite3WhereBegin` per disjunct, `OP_Gosub` body), full scan (`Rewind`/`Last`); then residual terms, transitive constraints, LEFT JOIN match flag, RIGHT JOIN bookkeeping | `sqlite3_where_code_one_loop_start` |
| `codeAllEqualityTerms` / `codeEqualityTerm` / `codeINTerm` | registers of the key prefix, affinity string trimmed by `sqlite3CompareAffinity`, IN loops (`sqlite3FindInIndex` with `IN_INDEX_LOOP`) | `code_all_equality_terms`, `code_equality_term`, `code_in_term` |
| `sqlite3WhereRightJoinLoop` | second pass over the unmatched rows of a RIGHT / FULL JOIN | `sqlite3_where_right_join_loop` |
| `filterPullDown` / `sqlite3ConstructBloomFilter` | Bloom filter construction and early tests | `filter_pull_down`, `sqlite3_construct_bloom_filter` |

### 2.6 SELECT (select.c) — `select.rs`, `flatten.rs`
| C | Rule | Rust |
|---|---|---|
| `sqlite3Select` tag-select-0220 | LEFT → inner, FULL → RIGHT/LEFT when the WHERE clause rejects the NULL row (`sqlite3ExprImpliesNonNullRow`) | `sqlite3_select_body` |
| `flattenSubquery` restrictions (1)–(28) | every restriction as a separate early return in C's order; compound subqueries duplicated per arm (`sqlite3SelectDup`, new `selId`, cursor renumbering) | `flatten_subquery`, `subst_expr`, `renumber_cursors` |
| tag-select-0230 | useless ORDER BY of a FROM subquery dropped before flattening | `sqlite3_select_body` |
| `fromClauseTermCanBeCoroutine` | co-routine when the subquery is the outer loop candidate; otherwise materialize once (`OP_Once`) with reuse for self-joined views and CTEs | `from_clause_term_can_be_coroutine`, `is_self_join_view` |
| `pushDownWhereTerms`, `disableUnusedSubqueryResultColumns`, `propagateConstants`, `countOfViewOptimization`, `existsToJoin` | as in C, each behind its optimization flag | `flatten.rs` |
| `selectInnerLoop` / `pushOntoSorter` / `generateSortTail` | sorter vs ephemeral index, ORDER BY + LIMIT (`OP_IfNotZero` / `Last` / `IdxLE` top-N), partial sort (`nOBSat`), deferred row load, DISTINCT kinds | same names |
| `isSimpleCount` / `minMaxQuery` | `count(*)` on the smallest suitable index (`OP_Count`); min/max as an ORDER BY for the planner with early out | `is_simple_count`, `min_max_query`, `sqlite3_where_min_max_opt_early_out` |
| `sqlite3FindInIndex` expr.c | `IN (SELECT col FROM tbl)` uses the table (rowid) or an index when affinities and collations allow (`aiMap` for vectors) | `sqlite3_find_in_index`, `is_candidate_for_in_opt` |

### 2.7 EXPLAIN QUERY PLAN rows
`OP_Explain` is added only for `explain==2` (`sqlite3VdbeExplain`), except `sqlite3WhereExplainBloomFilter`, which adds
it unconditionally in 3.54.1. P1 = own address, P2 = parent (`addrExplain` stack, popped at `select_end`), P3 = `rRun`
of the loop for SCAN / SEARCH rows. Texts: `sqlite3WhereAddExplainText` (SCAN / SEARCH … USING [COVERING] INDEX … (col=? AND col>? …),
`MULTI-INDEX OR`, `INDEX n`, `RIGHT-JOIN`, `LEFT-JOIN`, `EXISTS`), select.c (`CO-ROUTINE`, `MATERIALIZE`, `COMPOUND QUERY`,
`LEFT-MOST SUBQUERY`, `UNION ALL`, `MERGE (…)`, `LEFT`, `RIGHT`, `USE TEMP B-TREE FOR …`, `SCAN n CONSTANT ROW`),
expr.c (`[CORRELATED ]SCALAR SUBQUERY n`, `[CORRELATED ]LIST SUBQUERY n`, `REUSE …`, `USING INDEX … FOR IN-OPERATOR`,
`USING ROWID SEARCH ON TABLE …`), `CREATE BLOOM FILTER`.

### 2.8 Connection state the listing shows
| C | Rule | Engine |
|---|---|---|
| `sqlite3FinishCoding` | `OP_Transaction iDb, write, schema_cookie, iGeneration` with P5=1 for every database the statement reads | `DbInfo.schema_cookie` (engine schema), `DbSlot::c_generation` |
| `sqlite3SchemaClear` | `iGeneration++` when a loaded schema is cleared: `sqlite3RollbackAll` / `OP_Savepoint ROLLBACK` with `DBFLAG_SchemaChange`, `OP_ParseSchema` with P4==0 (ALTER TABLE: the database and temp), end of `sqlite3RunVacuum`, failed `OP_VCreate` in autocommit mode | `DatabaseSet::c_reset_all_schemas`, `c_reparse_schema`, hooks in `engine.rs` / `lib.rs` |
| `sqlite3ReadSchema` → `sqlite3Init` | a prepare that loads a schema resets the error offset | `DbSlot::c_loaded`, `Db.schema_not_loaded` |
| `db->flags` | `SQLITE_AutoIndex`, `SQLITE_ReverseOrder`, `SQLITE_ShortColNames`, `SQLITE_FullColNames` from their pragmas | `DatabaseSet::c_flags_toggled` |


## 3. What "supported" means in V2

Accepted (compiled by the generator, listing and plan byte-identical to C): SELECT over rowid and WITHOUT ROWID
tables, views, FROM-clause subqueries, temp and attached databases; every WHERE constraint shape; ORDER BY / LIMIT /
OFFSET / DISTINCT; joins of any number of tables of all types (inner, LEFT, RIGHT, FULL, CROSS, NATURAL, USING);
aggregates without GROUP BY (incl. `count(*)`, min / max, DISTINCT aggregates); correlated and uncorrelated
sub-selects (scalar, EXISTS, IN); compound selects; plain (non-recursive) common table expressions — these came for
free with the FROM-clause machinery and are compared like any other statement, but no CTE-specific corpus was built.

Reported as unsupported (first missing piece is named; `SQLITERS_CG_TRACE=1` prints it):

| Reason reported | Why |
|---|---|
| `statement kind (grammar rule not ported)` | DML, DDL, PRAGMA, ATTACH, … (later steps) |
| `GROUP BY over a FROM clause` (also DISTINCT + ORDER BY that C rewrites to GROUP BY) | GROUP BY / HAVING aggregation code is left for the next step |
| `window function` | later step |
| `recursive common table expression` | later step |
| `eponymous virtual table / table-valued function`, `virtual table` | later step |
| `table with a VIRTUAL generated column` | needs the column expressions of the schema (CREATE TABLE is not parsed by the generator) |
| `CREATE TABLE with an explicit COLLATE BINARY` | C keeps the explicit collation (KeyInfo shows `B`); the engine's schema does not record it |
| `UNIQUE constraint written before the INTEGER PRIMARY KEY it covers` | `uniqNotNull` / `aiColumn` of that automatic index depend on the order of the constraints in the text |
| `WITHOUT ROWID table whose automatic index names differ from C` | engine defect D1 |
| `table whose removed sqlite_stat1 rows C still remembers`, `sqlite_stat1 was written and not re-read by ANALYZE` | C's in-memory statistics are not the engine's (D7) |
| `like() after PRAGMA case_sensitive_like` | the pragma re-registers like(); the engine does not implement it |
| `view … could not be parsed`, `index whose CREATE INDEX text could not be parsed`, `CREATE TABLE text whose column definitions could not be scanned` | the schema text scan failed (not seen in the corpora) |
| `LIKE optimization with a range bound that is not valid UTF-8` | the program representation holds text as `String` |

## 4. C-side coverage of the ported functions


Corpus: the 35526 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
|---|---|---|
| build.c | 366 / 449 (81.5%) | 261 / 380 (68.7%) |
| attach.c | 21 / 35 (60.0%) | 13 / 30 (43.3%) |
| resolve.c | 152 / 332 (45.8%) | 120 / 328 (36.6%) |
| expr.c | 668 / 971 (68.8%) | 432 / 679 (63.6%) |
| select.c | 1893 / 2570 (73.7%) | 1289 / 2002 (64.4%) |
| whereexpr.c | 315 / 388 (81.2%) | 236 / 334 (70.7%) |
| where.c | 1938 / 2079 (93.2%) | 1581 / 1842 (85.8%) |
| wherecode.c | 885 / 989 (89.5%) | 631 / 764 (82.6%) |
| vdbeaux.c | 24 / 24 (100.0%) | 7 / 8 (87.5%) |
| callback.c | 7 / 8 (87.5%) | 4 / 6 (66.7%) |
| delete.c | 22 / 25 (88.0%) | 12 / 26 (46.2%) |
| func.c | 10 / 19 (52.6%) | 4 / 18 (22.2%) |
| hash.c | 2 / 2 (100.0%) | 0 / 0 (0.0%) |
| insert.c | 12 / 14 (85.7%) | 5 / 6 (83.3%) |
| prepare.c | 45 / 79 (57.0%) | 25 / 52 (48.1%) |
| update.c | 7 / 7 (100.0%) | 4 / 4 (100.0%) |
| utf.c | 4 / 8 (50.0%) | 1 / 8 (12.5%) |
| util.c | 45 / 45 (100.0%) | 33 / 34 (97.1%) |
| walker.c | 10 / 10 (100.0%) | 0 / 0 (0.0%) |
| **total** | 6426 / 8054 (79.8%) | 4658 / 6521 (71.4%) |


A branch of a ported C function that no accepted statement takes is **not verified**. `cov/untaken.txt` lists every
such source line (function by function); the per-function counts are in the last column of section 1. The largest
groups:


| C function | source lines with a never-taken branch |
|---|---|
| `sqlite3Select` | 97 |
| `lookupName` | 70 |
| `selectExpander` | 61 |
| `sqlite3ExprCodeTarget` | 60 |
| `sqlite3WhereCodeOneLoopStart` | 49 |
| `sqlite3WhereBegin` | 38 |
| `resolveFromTermToCte` | 34 |
| `flattenSubquery` | 31 |
| `isLikeOrGlob` | 22 |
| `sqlite3ColumnsFromExprList` | 21 |
| `exprAnalyzeOrTerm` | 16 |
| `wherePathSatisfiesOrderBy` | 16 |
| `sqlite3Prepare` | 16 |
| `selectInnerLoop` | 15 |
| `whereLoopAddBtree` | 15 |
| `sqlite3ProcessJoin` | 14 |
| `sqlite3WhereEnd` | 14 |
| `sqlite3FinishCoding` | 13 |
| `wherePathMatchSubqueryOB` | 12 |
| `convertToWithoutRowidTable` | 11 |
| `sqlite3CodeRhsOfIN` | 11 |
| `countOfViewOptimization` | 11 |
| `pushDownWhereTerms` | 10 |
| `existsToJoin` | 10 |
| `multiSelectByMerge` | 10 |
| `constructAutomaticIndex` | 10 |
| `whereLoopAddBtreeIndex` | 10 |
| `sqlite3SrcListAppendFromTerm` | 9 |
| `fixSelectCb` | 9 |
| `sqlite3MatchEName` | 9 |
| `sqlite3ExprIsIIF` | 9 |
| `sqlite3CodeSubselect` | 9 |
| `wherePathSolver` | 9 |
| `sqlite3LocateTable` | 8 |
| `sqlite3IndexedExprLookup` | 8 |
| `substExpr` | 8 |
| `sqlite3JoinType` | 8 |
| `multiSelect` | 8 |
| `sqlite3WhereTabFuncArgs` | 8 |
| `removeUnindexableInClauseTerms` | 8 |
| `sqlite3FindTable` | 7 |
| `isValidSchemaTableName` | 7 |
| `convertCompoundSelectToSubquery` | 7 |
| `generateSortTail` | 7 |
| `resetAccumulator` | 7 |

Known reasons for the never-taken branches (so they are not mistaken for verified code): virtual-table branches
(`IsVirtual`, `WHERE_VIRTUALTABLE`: unsupported, never accepted); window / GROUP BY / recursive-CTE branches of
`sqlite3Select`, `selectInnerLoop`, `selectExpander`, `resolveFromTermToCte`; OOM and `db->mallocFailed` branches;
`SQLITE_ENABLE_STAT4` / `SORTER_REFERENCES` branches that the reference build does not compile; UPDATE / DELETE
callers of `sqlite3WhereBegin` (`WHERE_ONEPASS_*`, `WHERE_OR_SUBCLAUSE` with `OP_RowSet` for rowid sets of DML);
`SQLITE_TESTCTRL` optimization switches (every `OptimizationDisabled` test is taken one way only); error branches
(too many FROM terms, expression depth, collation lookup failures); rare planner shapes that the generator's corpus
does not produce (see `cov/untaken.txt` for `whereLoopAddBtreeIndex`, `wherePathSatisfiesOrderBy`,
`sqlite3WhereCodeOneLoopStart`, `flattenSubquery`).

## 5. Decisions of C that V2 does not reproduce, and remaining differences

1. Everything in the table of section 3 (reported as unsupported).
2. C's schema-reset sources that are tracked for `iGeneration` (P4 of `OP_Transaction`): ROLLBACK / ROLLBACK TO of a
   transaction that changed the schema, ALTER TABLE, VACUUM, CREATE VIRTUAL TABLE failing in autocommit mode and inside
   a transaction, `PRAGMA temp_store` change with an open temp database, `PRAGMA writable_schema=RESET`. Not tracked:
   a schema change made by another connection (`SQLITE_SCHEMA` on the cookie check), other DDL statements that fail
   after `OP_SetCookie`, ALTER TABLE failing in its `sqlite_rename_test` phase, DETACH / ATTACH failures. In those
   cases the listing differs from C in the P4 operand of `OP_Transaction` only.
3. "Schema loaded" is taken from the engine's rule (every statement except transaction control, ATTACH / DETACH and
   most PRAGMAs reads the schema when prepared); C skips the load for a table-less SELECT once the encoding is fixed.
   It matters only for the error offset of the first failing statement after a schema reset.
4. `Table.nTabRef` is decremented only where `sqlite3ViewGetColumnNames()` deletes its copy of the view; the other
   places where C deletes a SrcList are not mirrored (only the "too many references" limit of 65 535 depends on it).
5. Remaining differences of the all-kinds corpus (`all_kinds_bugs_final.json`, classified with `bugsum.py`): functions
   and collations that only the C shell has (D4), virtual table modules that only the C shell has (D5), scripts whose
   state diverges because of an engine difference recorded in `DIFFS_OUTSIDE_TASK.md` (D6, D8, D10, D11, D12, D13 and
   the untracked reset sources of item 2), and 5 multi-statement lines that the shells split differently (same as V1).

