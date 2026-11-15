# V3 decision parity (karar eşliği): schema loading as C, GROUP BY / HAVING / FILTER, recursive CTEs, C connection state

Scope: step V3 of the port of C SQLite 3.54.1's code generator. Oracle: `target/perf-work/bin/sqlite3` and the
amalgamation `target/sqlite-ref/amalg` (instrumented copies in `v3/wdump/` and `v3/cov/`, scratch only). Worktree
`/home/hakan/github/sqliters/.claude/worktrees/vdbe` (branch `performans-A-vdbe`, uncommitted on top of 6159c49). The
engine's executor and fast paths still execute every statement; the generator's program is what `EXPLAIN` and
`EXPLAIN QUERY PLAN` print when it supports the statement. Binary of every number below: `$S/v3_bin/sqliters.r9`
(= the final `$S/v3_bin/sqliters`).

## 0. How a decision is proven: the layers

Per corpus: statements / accepted by the generator / identical / different (EXPLAIN and EQP, `expdiff.py`), and the
dump layers (`wdiff.py`: identical / statements with a dump; `sdiff.py`: identical schemas / schemas).

| corpus | EXPLAIN | EQP | terms | loops | paths | agg | schema objects |
|---|---|---|---|---|---|---|---|
| ddl_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10523 / 10523 | 10306 / 10306 | 10323 / 10323 | 2090 / 2090 | 400 / 400 (load failures 0) |
| plan_a | 26000 / 26000 / 26000 / 0 | 26000 / 26000 / 26000 / 0 | 23583 / 23583 | 23196 / 23196 | 23246 / 23246 | 6054 / 6054 | 520 / 520 (load failures 0) |
| plan_b | 13000 / 13000 / 13000 / 0 | 13000 / 13000 / 13000 / 0 | 11884 / 11884 | 11698 / 11698 | 11718 / 11718 | 3088 / 3088 | 260 / 260 (load failures 0) |
| agg_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 10531 / 10531 | 10337 / 10337 | 10342 / 10342 | 10048 / 10048 | 300 / 300 (load failures 0) |
| agg_b | 6000 / 6000 / 6000 / 0 | 6000 / 6000 / 6000 / 0 | 5259 / 5259 | 5168 / 5168 | 5172 / 5172 | 4999 / 4999 | 150 / 150 (load failures 0) |
| agg_c | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2661 / 2661 | 2613 / 2613 | 2615 / 2615 | 2558 / 2558 | 100 / 100 (load failures 0) |
| cte_a | 12000 / 12000 / 12000 / 0 | 12000 / 12000 / 12000 / 0 | 9045 / 9045 | 8965 / 8965 | 8977 / 8977 | 1804 / 1804 | 300 / 300 (load failures 0) |
| cte_b | 3000 / 3000 / 3000 / 0 | 3000 / 3000 / 3000 / 0 | 2280 / 2280 | 2261 / 2261 | 2264 / 2264 | 395 / 395 | 100 / 100 (load failures 0) |

Corpora (reproducible): `ddl_a` = gen_v3.py 3101 400 30 (400 random schemas × 30 queries; C-rejected DDL dropped),
`plan_a` / `plan_b` = V2's planner corpora, `agg_a/b/c` = gen_agg.py (GROUP BY / HAVING / FILTER / DISTINCT and ORDER
BY aggregates / indexed expressions / aggregate sub-selects), `cte_a/b` = gen_cte.py (plain, recursive, UNION /
UNION ALL / ORDER BY / LIMIT recursion, MATERIALIZED hints, error shapes).

| other layer | tool | result |
|---|---|---|
| schema dump, DDL-only corpus (400 schemas) | `sdiff.py corpus/ddl_schemas.sql` | 400 / 400 (load failures 0) |
| DDL acceptance (C-rejected statements kept) | `ddlacc.py corpus/ddl_schemas_raw.sql` | statements 5297 different 55 (all: D16 and its cascades, statements the engine accepts and C rejects) |
| all statement kinds, EXPLAIN, statements executed in between | `expdiff.py --exec` on `v1/corpus/all_kinds.sql` | statements 39300; generator accepted 14133: identical 13778, DIFFERENT 355; unsupported (fallback) 24765 (of which same as C anyway 4709); not reached (old parser/other) 402 (same 342) |
| same, EXPLAIN QUERY PLAN | `expdiff.py --exec --eqp` | statements 39300; generator accepted 14133: identical 13861, DIFFERENT 272; unsupported (fallback) 24765 (of which same as C anyway 21950); not reached (old parser/other) 402 (same 342) |
| prepare errors of all statements run as they are (new) | `perr.py` | statements 39300; with a prepare error in either shell 8858: identical 7501, DIFFERENT 1357 (engine-scope classes, DIFFS_OUTSIDE_TASK.md) |
| C connection-state probes (EncodingFixed, schema-load state, ATTACH) | `probe/encfixed.sh` | probes: 68 identical 66 different 2 |
| library level, no schema (V1 corpora) | `v1/cgdiff.py` | total 54613  identical 54609  DIFFERENT 0  unsupported 4 |
| unit tests with C-produced expectations | `cargo test -p sqliters-codegen` (plan_tests.rs), `c_align_v3_tests.rs` | codegen test result: ok. 29 passed; 0 failed; workspace 979 passed / 4 failed (the known vtab *_differential_parity) / 337 ignored, incl. c_align_v3_tests 6/6 (res/test_ws_r9.txt) |
| C-side coverage | gcov of the functions of section 1 new in V3 over the accepted statements | section 4 |

## 1. C function → Rust item (497 functions, V1 + V2 + V3)
Paths are under `crates/`. "cov" = C lines executed % / branches taken at least once % (gcov, section 4) — measured over the V3 corpora for the functions new or changed in V3, over V2's plan corpus for the others (marked V2); `n/a` = not measured (V1 functions, inlined, or not a separate function in the -O0 build).


### build.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3FinishCoding` | `sqlite3_finish_coding` | `sqliters-codegen/src/build.rs` | 67.3 / 59.5 (V2) | 13 |
| `sqlite3BeginTransaction` | `sqlite3_begin_transaction` | `sqliters-codegen/src/build.rs` | n/a |  |
| `sqlite3EndTransaction` | `sqlite3_end_transaction` | `sqliters-codegen/src/build.rs` | n/a |  |
| `sqlite3Savepoint` | `sqlite3_savepoint` | `sqliters-codegen/src/build.rs` | n/a |  |
| `sqlite3CodeVerifySchema` | `sqlite3_code_verify_schema` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 (V2) | 1 |
| `sqlite3BeginWriteOperation` | `sqlite3_begin_write_operation` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 (V2) | 1 |
| `sqlite3MultiWrite` | `sqlite3_multi_write` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 (V2) | 1 |
| `sqlite3MayAbort` | `sqlite3_may_abort` | `sqliters-codegen/src/build.rs` | 100.0 / 50.0 (V2) | 1 |
| `sqlite3NameFromToken` | `sqlite3_name_from_token` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3IdListAppend` | `sqlite3_id_list_append` | `sqliters-codegen/src/build.rs` | 80.0 / 50.0 (V2) | 3 |
| `sqlite3IdListIndex` | `sqlite3_id_list_index` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3SrcListAppend` | `sqlite3_src_list_append` | `sqliters-codegen/src/build.rs` | 91.3 / 83.3 (V2) | 2 |
| `sqlite3SrcItemAttachSubquery` | `sqlite3_src_item_attach_subquery` | `sqliters-codegen/src/build.rs` | 66.7 / 60.0 (V2) | 4 |
| `sqlite3SrcListAppendFromTerm` | `sqlite3_src_list_append_from_term` | `sqliters-codegen/src/build.rs` | 53.3 / 46.9 (V2) | 9 |
| `sqlite3SrcListIndexedBy` | `sqlite3_src_list_indexed_by` | `sqliters-codegen/src/build.rs` | 100.0 / 75.0 (V2) | 2 |
| `sqlite3SrcListAppendList` | `sqlite3_src_list_append_list` | `sqliters-codegen/src/build.rs` | 90.9 / 50.0 (V2) | 2 |
| `sqlite3SrcListFuncArgs` | `sqlite3_src_list_func_args` | `sqliters-codegen/src/build.rs` | 0.0 / 0.0 (V2) | 1 |
| `sqlite3SrcListShiftJoinType` | `sqlite3_src_list_shift_join_type` | `sqliters-codegen/src/build.rs` | 100.0 / 85.7 (V2) | 2 |
| `sqlite3TwoPartName` | `sqlite3_two_part_name` | `sqliters-codegen/src/build_tab.rs` | 71.4 / 66.7 (V3) | 2 |
| `sqlite3CheckObjectName` | `sqlite3_check_object_name` | `sqliters-codegen/src/build_tab.rs` | 70.6 / 54.5 (V3) | 8 |
| `sqlite3FindIndex` | `sqlite3_find_index` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V3) | 0 |
| `sqlite3StartTable` | `sqlite3_start_table` | `sqliters-codegen/src/build_tab.rs` | 71.8 / 62.5 (V3) | 18 |
| `sqlite3AddColumn` | `sqlite3_add_column` | `sqliters-codegen/src/build_tab.rs` | 88.9 / 80.4 (V3) | 9 |
| `sqlite3AddNotNull` | `sqlite3_add_not_null` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 80.0 (V3) | 1 |
| `sqlite3ColumnSetExpr` | `sqlite3_column_set_expr` | `sqliters-codegen/src/build_tab.rs` | 60.0 / 37.5 (V3) | 3 |
| `sqlite3ColumnExpr` | `sqlite3_column_expr` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 62.5 (V3) | 3 |
| `sqlite3AddDefaultValue` | `sqlite3_add_default_value` | `sqliters-codegen/src/build_tab.rs` | 87.5 / 75.0 (V3) | 4 |
| `makeColumnPartOfPrimaryKey` | `make_column_part_of_primary_key` | `sqliters-codegen/src/build_tab.rs` | 80.0 / 50.0 (V3) | 1 |
| `sqlite3StringToId` | `sqlite3_string_to_id` | `sqliters-codegen/src/build_tab.rs` | 66.7 / 66.7 (V3) | 2 |
| `sqlite3HasExplicitNulls` | `sqlite3_has_explicit_nulls` | `sqliters-codegen/src/build_tab.rs` | 55.6 / 50.0 (V3) | 3 |
| `sqlite3AddPrimaryKey` | `sqlite3_add_primary_key` | `sqliters-codegen/src/build_tab.rs` | 92.9 / 78.6 (V3) | 6 |
| `sqlite3AddCheckConstraint` | `sqlite3_add_check_constraint` | `sqliters-codegen/src/build_tab.rs` | 93.8 / 58.3 (V3) | 4 |
| `sqlite3AddCollateType` | `sqlite3_add_collate_type` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 75.0 (V3) | 3 |
| `sqlite3AddGenerated` | `sqlite3_add_generated` | `sqliters-codegen/src/build_tab.rs` | 77.4 / 64.3 (V3) | 8 |
| `sqlite3CreateForeignKey` | `sqlite3_create_foreign_key` | `sqliters-codegen/src/build_tab.rs` | 86.5 / 78.3 (V3) | 9 |
| `sqlite3DeferForeignKey` | `sqlite3_defer_foreign_key` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 50.0 (V3) | 3 |
| `hasColumn` | `has_column` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V3) | 0 |
| `isDupColumn` | `is_dup_column` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V2) | 0 |
| `recomputeColumnsNotIndexed` | `recompute_columns_not_indexed` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 75.0 (V2) | 2 |
| `estimateTableWidth` | `estimate_table_width` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V3) | 0 |
| `estimateIndexWidth` | `estimate_index_width` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V2) | 0 |
| `resizeIndexObject` | `resize_index_object` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 75.0 (V3) | 1 |
| `convertToWithoutRowidTable` | `convert_to_without_rowid_table` | `sqliters-codegen/src/build_tab.rs` | 89.9 / 82.3 (V2) | 11 |
| `sqlite3EndTable` | `sqlite3_end_table` | `sqliters-codegen/src/build_tab.rs` | 60.6 / 71.0 (V3) | 20 |
| `sqlite3CreateView` | `sqlite3_create_view` | `sqliters-codegen/src/build_tab.rs` | 92.1 / 61.1 (V3) | 6 |
| `sqlite3CreateIndex` | `sqlite3_create_index` | `sqliters-codegen/src/build_tab.rs` | 85.7 / 80.3 (V3) | 35 |
| `sqlite3SubqueryDetach` | `sqlite3_subquery_detach` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3AffinityType` | `sqlite3_affinity_type_sz` | `sqliters-codegen/src/schema.rs` | n/a |  |
| `sqlite3ColumnColl` | `sqlite3_column_coll` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3DefaultRowEst` | `sqlite3_default_row_est` | `sqliters-codegen/src/schema.rs` | 92.3 / 87.5 (V3) | 1 |
| `sqlite3PrimaryKeyIndex` | `sqlite3_primary_key_index` | `sqliters-codegen/src/schema.rs` | 100.0 / 75.0 (V2) | 1 |
| `sqlite3TableColumnToIndex` | `sqlite3_table_column_to_index` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3StorageColumnToTable` | `sqlite3_storage_column_to_table` | `sqliters-codegen/src/schema.rs` | 60.0 / 16.7 (V2) | 3 |
| `sqlite3TableColumnToStorage` | `sqlite3_table_column_to_storage` | `sqliters-codegen/src/schema.rs` | 28.6 / 10.0 (V2) | 4 |
| `sqlite3FindTable` | `sqlite3_find_table` | `sqliters-codegen/src/schema.rs` | 83.9 / 80.0 (V2) | 7 |
| `sqlite3LocateTable` | `sqlite3_locate_table` | `sqliters-codegen/src/schema.rs` | 88.0 / 70.6 (V3) | 7 |
| `sqlite3LocateTableItem` | `sqlite3_locate_table_item` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3KeyInfoOfIndex` | `sqlite3_key_info_of_index` | `sqliters-codegen/src/schema.rs` | 71.4 / 56.2 (V2) | 5 |
| `sqlite3TableLock` | `sqlite3_table_lock` | `sqliters-codegen/src/schema.rs` | 75.0 / 75.0 (V2) | 1 |
| `sqlite3SrcListAssignCursors` | `sqlite3_src_list_assign_cursors` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 (V2) | 1 |
| `sqlite3ViewGetColumnNames` | `sqlite3_view_get_column_names` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V3) | 0 |
| `sqlite3IndexBloomable` | `sqlite3_index_bloomable` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |

### analyze.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `decodeIntArray` | `decode_int_array` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 80.8 (V3) | 5 |
| `analysisLoader` | `analysis_loader` | `sqliters-codegen/src/build_tab.rs` | 92.6 / 75.0 (V3) | 2 |
| `sqlite3AnalysisLoad` | `sqlite3_analysis_load` | `sqliters-codegen/src/build_tab.rs` | 91.7 / 81.2 (V3) | 3 |

### attach.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `fixExprCb` | `fix_expr_cb` | `sqliters-codegen/src/build_tab.rs` | 55.6 / 33.3 (V2) | 3 |
| `fixSelectCb` | `fix_select_cb` | `sqliters-codegen/src/build_tab.rs` | 61.5 / 45.8 (V2) | 9 |
| `sqlite3FixSelect` | `fix_select` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V3) | 0 |

### resolve.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `incrAggFunctionDepth` | `incr_agg_function_depth` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveAlias` | `resolve_alias` | `sqliters-codegen/src/resolve.rs` | 88.9 / 70.0 (V3) | 3 |
| `areDoubleQuotedStringsEnabled` | `are_double_quoted_strings_enabled` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `sqlite3MatchEName` | `sqlite3_match_e_name` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 (V2) | 9 |
| `sqlite3ExprColUsed` | `sqlite3_expr_col_used` | `sqliters-codegen/src/resolve.rs` | 75.0 / 25.0 (V2) | 4 |
| `extendFJMatch` | `extend_fj_match` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 (V2) | 1 |
| `isValidSchemaTableName` | `is_valid_schema_table_name` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 (V2) | 7 |
| `sqlite3CreateColumnExpr` | `sqlite3_create_column_expr` | `sqliters-codegen/src/resolve.rs` | 85.7 / 40.0 (V2) | 4 |
| `lookupName` | `lookup_name` | `sqliters-codegen/src/resolve.rs` | 45.9 / 43.5 (V2) | 70 |
| `notValidImpl` | `not_valid_impl` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `exprProbability` | `expr_probability` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveSetExprSubtypeArg` | `resolve_set_expr_subtype_arg` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveExprStep` | `resolve_expr_step` | `sqliters-codegen/src/resolve.rs` | 74.6 / 65.4 (V3) | 41 |
| `resolveAsName` | `resolve_as_name` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveOrderByTermToExprList` | `resolve_order_by_term_to_expr_list` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveOutOfRangeError` | `resolve_out_of_range_error` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveCompoundOrderBy` | `resolve_compound_order_by` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `sqlite3ResolveOrderGroupBy` | `sqlite3_resolve_order_group_by` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveOrderGroupBy` | `resolve_order_group_by` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `resolveSelectStep` | `resolve_select_step` | `sqliters-codegen/src/resolve.rs` | 82.5 / 78.3 (V3) | 16 |
| `sqlite3ResolveExprNames` | `sqlite3_resolve_expr_names` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `sqlite3ResolveExprListNames` | `sqlite3_resolve_expr_list_names` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `sqlite3ResolveSelectNames` | `sqlite3_resolve_select_names` | `sqliters-codegen/src/resolve.rs` | n/a |  |
| `sqlite3ResolveSelfReference` | `sqlite3_resolve_self_reference` | `sqliters-codegen/src/resolve.rs` | 100.0 / 75.0 (V2) | 2 |

### expr.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3TableColumnAffinity` | `sqlite3_table_column_affinity` | `sqliters-codegen/src/expr.rs` | 100.0 / 75.0 (V2) | 1 |
| `sqlite3ExprAffinity` | `sqlite3_expr_affinity` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprDataType` | `sqlite3_expr_data_type` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAddCollateToken` | `sqlite3_expr_add_collate_token` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAddCollateString` | `sqlite3_expr_add_collate_string` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprSkipCollate` | `sqlite3_expr_skip_collate` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprSkipCollateAndLikely` | `sqlite3_expr_skip_collate_and_likely` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCollSeq` | `sqlite3_expr_coll_seq` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprNNCollSeq` | `sqlite3_expr_nn_coll_seq` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCollSeqMatch` | `sqlite3_expr_coll_seq_match` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3CompareAffinity` | `sqlite3_compare_affinity` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `comparisonAffinity` | `comparison_affinity` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3IndexAffinityOk` | `sqlite3_index_affinity_ok` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `binaryCompareP5` | `binary_compare_p5` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3BinaryCompareCollSeq` | `sqlite3_binary_compare_coll_seq` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCompareCollSeq` | `sqlite3_expr_compare_coll_seq` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprIsVector` | `sqlite3_expr_is_vector` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprVectorSize` | `sqlite3_expr_vector_size` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3VectorFieldSubexpr` | `sqlite3_vector_field_subexpr` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprForVectorField` | `sqlite3_expr_for_vector_field` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCheckHeight` | `sqlite3_expr_check_height` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `exprSetHeight` | `expr_set_height` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprSetHeightAndFlags` | `sqlite3_expr_set_height_and_flags` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3SelectExprHeight` | `sqlite3_select_expr_height` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprSetErrorOffset` | `sqlite3_expr_set_error_offset` | `sqliters-codegen/src/expr.rs` | 100.0 / 50.0 (V2) | 2 |
| `sqlite3ExprAlloc` | `sqlite3_expr_alloc` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3Expr` | `sqlite3_expr` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprInt32` | `sqlite3_expr_int32` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAttachSubtrees` | `sqlite3_expr_attach_subtrees` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3PExpr` | `sqlite3_p_expr` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3PExprAddSelect` | `sqlite3_p_expr_add_select` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListToValues` | `sqlite3_expr_list_to_values` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAnd` | `sqlite3_expr_and` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprFunction` | `sqlite3_expr_function` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprOrderByAggregateError` | `sqlite3_expr_order_by_aggregate_error` | `sqliters-codegen/src/expr.rs` | 0.0 / 100.0 (V2) | 0 |
| `sqlite3ExprAddFunctionOrderBy` | `sqlite3_expr_add_function_order_by` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprFunctionUsable` | `sqlite3_expr_function_usable` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAssignVarNumber` | `sqlite3_expr_assign_var_number` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `dupedExprStructSize` | `duped_expr_struct_size` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 (V2) | 1 |
| `exprDup` | `expr_dup` | `sqliters-codegen/src/expr.rs` | 96.5 / 92.9 (V3) | 3 |
| `sqlite3ExprDup` | `sqlite3_expr_dup` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprListDup` | `sqlite3_expr_list_dup` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3SrcListDup` | `sqlite3_src_list_dup` | `sqliters-codegen/src/expr.rs` | 83.7 / 75.0 (V3) | 6 |
| `sqlite3SelectDup` | `sqlite3_select_dup` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3WithDup` | `sqlite3_with_dup` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListAppend` | `sqlite3_expr_list_append` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListSetSortOrder` | `sqlite3_expr_list_set_sort_order` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListSetName` | `sqlite3_expr_list_set_name` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListSetSpan` | `sqlite3_expr_list_set_span` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprListCheckLength` | `sqlite3_expr_list_check_length` | `sqliters-codegen/src/expr.rs` | 80.0 / 50.0 (V2) | 1 |
| `sqlite3ExprListFlags` | `sqlite3_expr_list_flags` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3SelectWalkFail` | `sqlite3_select_walk_fail` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3IsTrueOrFalse` | `sqlite3_is_true_or_false` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprIdToTrueFalse` | `sqlite3_expr_id_to_true_false` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprTruthValue` | `sqlite3_expr_truth_value` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprSimplifiedAndOr` | `sqlite3_expr_simplified_and_or` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `exprNodeIsConstantFunction` | `expr_node_is_constant_function` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `exprNodeIsConstant` | `expr_node_is_constant` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `exprIsConst` | `expr_is_const` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprIsConstant` | `sqlite3_expr_is_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprListIsConstant` | `sqlite3_expr_list_is_constant` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprIsConstantNotJoin` | `sqlite3_expr_is_constant_not_join` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprIsTableConstant` | `sqlite3_expr_is_table_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `exprSelectWalkTableConstant` | `expr_select_walk_table_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprIsSingleTableConstraint` | `sqlite3_expr_is_single_table_constraint` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `exprImpliesNotNull` | `expr_implies_not_null` | `sqliters-codegen/src/expr.rs` | 80.8 / 69.2 (V2) | 5 |
| `sqlite3ExprIsNotTrue` | `sqlite3_expr_is_not_true` | `sqliters-codegen/src/expr.rs` | 0.0 / 0.0 (V2) | 3 |
| `sqlite3ExprIsIIF` | `sqlite3_expr_is_iif` | `sqliters-codegen/src/expr.rs` | 37.5 / 20.8 (V2) | 9 |
| `sqlite3ExprImpliesExpr` | `sqlite3_expr_implies_expr` | `sqliters-codegen/src/expr.rs` | 69.2 / 57.1 (V2) | 4 |
| `bothImplyNotNullRow` | `both_imply_not_null_row` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `impliesNotNullRow` | `implies_not_null_row` | `sqliters-codegen/src/expr.rs` | 97.1 / 80.7 (V2) | 5 |
| `sqlite3ExprImpliesNonNullRow` | `sqlite3_expr_implies_non_null_row` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 (V2) | 1 |
| `exprIdxCover` | `expr_idx_cover` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprCoveredByIndex` | `sqlite3_expr_covered_by_index` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `exprNodeIsConstantOrGroupBy` | `expr_node_is_constant_or_group_by` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprIsConstantOrGroupBy` | `sqlite3_expr_is_constant_or_group_by` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprIsConstantOrFunction` | `sqlite3_expr_is_constant_or_function` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprIsInteger` | `sqlite3_expr_is_integer` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCanBeNull` | `sqlite3_expr_can_be_null` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprNeedsNoAffinityChange` | `sqlite3_expr_needs_no_affinity_change` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 (V2) | 3 |
| `sqlite3IsRowid` | `sqlite3_is_rowid` | `sqliters-codegen/src/expr.rs` | 60.0 / 33.3 (V2) | 3 |
| `sqlite3ExprCompare` | `sqlite3_expr_compare` | `sqliters-codegen/src/expr.rs` | 100.0 / 91.7 (V3) | 8 |
| `sqlite3ExprListCompare` | `sqlite3_expr_list_compare` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprCompareSkip` | `sqlite3_expr_compare_skip` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ReferencesSrcList` | `sqlite3_references_src_list` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `findOrCreateAggInfoColumn` | `find_or_create_agg_info_column` | `sqliters-codegen/src/expr.rs` | 92.3 / 83.3 (V3) | 5 |
| `analyzeAggregate` | `analyze_aggregate` | `sqliters-codegen/src/expr.rs` | 97.6 / 81.2 (V3) | 13 |
| `sqlite3ExprAnalyzeAggregates` | `sqlite3_expr_analyze_aggregates` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ExprAnalyzeAggList` | `sqlite3_expr_analyze_agg_list` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3GetTempReg` | `sqlite3_get_temp_reg` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ReleaseTempReg` | `sqlite3_release_temp_reg` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3GetTempRange` | `sqlite3_get_temp_range` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ReleaseTempRange` | `sqlite3_release_temp_range` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ClearTempRegCache` | `sqlite3_clear_temp_reg_cache` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3TouchRegister` | `sqlite3_touch_register` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 (V2) | 0 |
| `codeCompare` | `code_compare` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprCodeSubselect` | `expr_code_subselect` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprVectorRegister` | `expr_vector_register` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `codeVectorCompare` | `code_vector_compare` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprEvalRhsFirst` | `expr_eval_rhs_first` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprComputeOperands` | `expr_compute_operands` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `isCandidateForInOpt` | `is_candidate_for_in_opt` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3SetHasNullFlag` | `sqlite3_set_has_null_flag` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3InRhsIsConstant` | `sqlite3_in_rhs_is_constant` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3FindInIndex` | `sqlite3_find_in_index` | `sqliters-codegen/src/exprcode.rs` | 98.1 / 93.9 (V2) | 6 |
| `exprINAffinity` | `expr_in_affinity` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3SubselectError` | `sqlite3_subselect_error` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3VectorErrorMsg` | `sqlite3_vector_error_msg` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `findCompatibleInRhsSubrtn` | `find_compatible_in_rhs_subrtn` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3CodeRhsOfIN` | `sqlite3_code_rhs_of_in` | `sqliters-codegen/src/exprcode.rs` | 93.3 / 82.3 (V2) | 11 |
| `sqlite3CodeSubselect` | `sqlite3_code_subselect` | `sqliters-codegen/src/exprcode.rs` | 66.7 / 50.0 (V2) | 9 |
| `sqlite3ExprCheckIN` | `sqlite3_expr_check_in` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeIN` | `sqlite3_expr_code_in` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `codeReal` | `code_real` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `codeInteger` | `code_integer` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeLoadIndexColumn` | `sqlite3_expr_code_load_index_column` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprCodeGeneratedColumn` | `sqlite3_expr_code_generated_column` | `sqliters-codegen/src/exprcode.rs` | 76.5 / 58.3 (V3) | 5 |
| `sqlite3ExprCodeGetColumnOfTable` | `sqlite3_expr_code_get_column_of_table` | `sqliters-codegen/src/exprcode.rs` | 95.7 / 91.7 (V3) | 1 |
| `sqlite3ExprCodeGetColumn` | `sqlite3_expr_code_get_column` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeMove` | `sqlite3_expr_code_move` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprToRegister` | `sqlite3_expr_to_register` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprCodeVector` | `expr_code_vector` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `setDoNotMergeFlagOnCopy` | `set_do_not_merge_flag_on_copy` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprCodeInlineFunction` | `expr_code_inline_function` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprNodeCanReturnSubtype` | `expr_node_can_return_subtype` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCanReturnSubtype` | `sqlite3_expr_can_return_subtype` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprCodeTargetAndOr` | `expr_code_target_and_or` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3IndexedExprLookup` | `sqlite3_indexed_expr_lookup` | `sqliters-codegen/src/exprcode.rs` | 53.3 / 46.7 (V2) | 8 |
| `exprPartidxExprLookup` | `expr_partidx_expr_lookup` | `sqliters-codegen/src/exprcode.rs` | 23.5 / 41.7 (V2) | 4 |
| `sqlite3ExprCodeTarget` | `sqlite3_expr_code_target` | `sqliters-codegen/src/exprcode.rs` | 64.1 / 60.9 (V3) | 47 |
| `sqlite3ExprCodeRunJustOnce` | `sqlite3_expr_code_run_just_once` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprNullRegisterRange` | `sqlite3_expr_null_register_range` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeTemp` | `sqlite3_expr_code_temp` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCode` | `sqlite3_expr_code` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeCopy` | `sqlite3_expr_code_copy` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeFactorable` | `sqlite3_expr_code_factorable` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprCodeExprList` | `sqlite3_expr_code_expr_list` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `exprCodeBetween` | `expr_code_between` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprIfTrue` | `sqlite3_expr_if_true` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprIfFalse` | `sqlite3_expr_if_false` | `sqliters-codegen/src/exprcode.rs` | n/a |  |
| `sqlite3ExprIfFalseDup` | `sqlite3_expr_if_false_dup` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 50.0 (V2) | 1 |
| `sqlite3RowidAlias` | `sqlite3_rowid_alias` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 (V2) | 2 |

### select.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `constInsert` | `const_insert` | `sqliters-codegen/src/flatten.rs` | 95.0 / 90.9 (V2) | 2 |
| `findConstInWhere` | `find_const_in_where` | `sqliters-codegen/src/flatten.rs` | 100.0 / 93.8 (V2) | 1 |
| `propagateConstantExprRewriteOne` | `propagate_constant_expr_rewrite_one` | `sqliters-codegen/src/flatten.rs` | 100.0 / 90.0 (V2) | 2 |
| `propagateConstantExprRewrite` | `propagate_constant_expr_rewrite` | `sqliters-codegen/src/flatten.rs` | 100.0 / 91.7 (V2) | 1 |
| `propagateConstants` | `propagate_constants` | `sqliters-codegen/src/flatten.rs` | 100.0 / 80.0 (V2) | 2 |
| `selectCheckOnClausesExpr` | `select_check_on_clauses_expr` | `sqliters-codegen/src/flatten.rs` | 80.0 / 71.4 (V2) | 6 |
| `selectCheckOnClausesSelect` | `select_check_on_clauses_select` | `sqliters-codegen/src/flatten.rs` | 0.0 / 0.0 (V2) | 1 |
| `sqlite3SelectCheckOnClauses` | `sqlite3_select_check_on_clauses` | `sqliters-codegen/src/flatten.rs` | 61.5 / 25.0 (V2) | 5 |
| `substExpr` | `subst_expr` | `sqliters-codegen/src/flatten.rs` | 74.6 / 79.2 (V3) | 7 |
| `substExprList` | `subst_expr_list` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `substSelect` | `subst_select` | `sqliters-codegen/src/flatten.rs` | 93.8 / 83.3 (V2) | 2 |
| `recomputeColumnsUsedExpr` | `recompute_columns_used_expr` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `recomputeColumnsUsed` | `recompute_columns_used` | `sqliters-codegen/src/flatten.rs` | 100.0 / 50.0 (V2) | 1 |
| `srclistRenumberCursors` | `srclist_renumber_cursors` | `sqliters-codegen/src/flatten.rs` | 100.0 / 75.0 (V2) | 1 |
| `renumberCursorDoMapping` | `renumber_cursor_do_mapping` | `sqliters-codegen/src/flatten.rs` | 100.0 / 75.0 (V2) | 1 |
| `renumberCursorsCb` | `renumber_cursors_cb` | `sqliters-codegen/src/flatten.rs` | 100.0 / 83.3 (V2) | 1 |
| `renumberCursors` | `renumber_cursors` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `findLeftmostExprlist` | `find_leftmost_exprlist` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `compoundHasDifferentAffinities` | `compound_has_different_affinities` | `sqliters-codegen/src/flatten.rs` | 100.0 / 100.0 (V2) | 0 |
| `flattenSubquery` | `flatten_subquery` | `sqliters-codegen/src/flatten.rs` | 91.6 / 77.0 (V2) | 31 |
| `pushDownWhereTerms` | `push_down_where_terms` | `sqliters-codegen/src/flatten.rs` | 79.6 / 59.6 (V2) | 10 |
| `disableUnusedSubqueryResultColumns` | `disable_unused_subquery_result_columns` | `sqliters-codegen/src/flatten.rs` | 90.9 / 76.7 (V2) | 6 |
| `countOfViewOptimization` | `count_of_view_optimization` | `sqliters-codegen/src/flatten.rs` | 94.4 / 78.0 (V2) | 11 |
| `existsToJoin` | `exists_to_join` | `sqliters-codegen/src/flatten.rs` | 100.0 / 68.8 (V2) | 10 |
| `multiSelectCollSeq` | `multi_select_coll_seq` | `sqliters-codegen/src/merge.rs` | n/a |  |
| `multiSelectByMergeKeyInfo` | `multi_select_by_merge_key_info` | `sqliters-codegen/src/merge.rs` | n/a |  |
| `generateOutputSubroutine` | `generate_output_subroutine` | `sqliters-codegen/src/merge.rs` | 88.8 / 79.2 (V3) | 3 |
| `multiSelectByMerge` | `multi_select_by_merge` | `sqliters-codegen/src/merge.rs` | 98.6 / 85.9 (V2) | 10 |
| `sqlite3ColumnIndex` | `sqlite3_column_index` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3SelectDestInit` | `sqlite3_select_dest_init` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3SelectNew` | `sqlite3_select_new` | `sqliters-codegen/src/select.rs` | n/a |  |
| `findRightmost` | `find_rightmost` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 (V2) | 1 |
| `sqlite3KeyInfoFromExprList` | `sqlite3_key_info_from_expr_list` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3SelectOpName` | `sqlite3_select_op_name` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3GetVdbe` | `sqlite3_get_vdbe` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3WithPush` | `sqlite3_with_push` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3SelectPopWith` | `sqlite3_select_pop_with` | `sqliters-codegen/src/select.rs` | n/a |  |
| `convertCompoundSelectToSubquery` | `convert_compound_select_to_subquery` | `sqliters-codegen/src/select.rs` | 27.8 / 54.2 (V2) | 7 |
| `sqlite3JoinType` | `sqlite3_join_type` | `sqliters-codegen/src/select.rs` | 69.2 / 54.5 (V2) | 8 |
| `sqlite3SrcItemColumnUsed` | `sqlite3_src_item_column_used` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 (V2) | 1 |
| `tableAndColumnIndex` | `table_and_column_index` | `sqliters-codegen/src/select.rs` | 100.0 / 80.0 (V2) | 1 |
| `sqlite3SetJoinExpr` | `sqlite3_set_join_expr` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 (V2) | 1 |
| `unsetJoinExpr` | `unset_join_expr` | `sqliters-codegen/src/select.rs` | 100.0 / 95.5 (V2) | 1 |
| `sqlite3ProcessJoin` | `sqlite3_process_join` | `sqliters-codegen/src/select.rs` | 92.2 / 63.9 (V2) | 14 |
| `sqlite3IndexedByLookup` | `sqlite3_indexed_by_lookup` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `cannotBeFunction` | `cannot_be_function` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 (V2) | 1 |
| `searchWith` | `search_with` | `sqliters-codegen/src/select.rs` | 0.0 / 0.0 (V2) | 4 |
| `resolveFromTermToCte` | `resolve_from_term_to_cte` | `sqliters-codegen/src/select.rs` | 2.9 / 1.3 (V2) | 34 |
| `sqlite3ExpandSubquery` | `sqlite3_expand_subquery` | `sqliters-codegen/src/select.rs` | n/a |  |
| `inAnyUsingClause` | `in_any_using_clause` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 (V2) | 1 |
| `selectExpander` | `select_expander` | `sqliters-codegen/src/select.rs` | 67.9 / 49.5 (V2) | 61 |
| `sqlite3ColumnsFromExprList` | `sqlite3_columns_from_expr_list` | `sqliters-codegen/src/select.rs` | 57.9 / 41.4 (V2) | 21 |
| `columnTypeImpl` | `column_type` | `sqliters-codegen/src/select.rs` | 94.7 / 78.3 (V2) | 5 |
| `sqlite3SubqueryColumnTypes` | `sqlite3_subquery_column_types` | `sqliters-codegen/src/select.rs` | 92.0 / 86.5 (V2) | 6 |
| `checkSubqueryNestingDepth` | `check_subquery_nesting_depth` | `sqliters-codegen/src/select.rs` | 60.0 / 50.0 (V2) | 1 |
| `sqlite3ResultSetOfSelect` | `sqlite3_result_set_of_select` | `sqliters-codegen/src/select.rs` | 88.0 / 70.0 (V2) | 3 |
| `sqlite3GenerateColumnNames` | `sqlite3_generate_column_names` | `sqliters-codegen/src/select.rs` | 90.6 / 73.1 (V2) | 5 |
| `sqlite3SelectExpand` | `sqlite3_select_expand` | `sqliters-codegen/src/select.rs` | n/a |  |
| `selectAddSubqueryTypeInfo` | `select_add_subquery_type_info` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3SelectAddTypeInfo` | `sqlite3_select_add_type_info` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3SelectPrep` | `sqlite3_select_prep` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3SelectWrongNumTermsError` | `sqlite3_select_wrong_num_terms_error` | `sqliters-codegen/src/select.rs` | n/a |  |
| `codeOffset` | `code_offset` | `sqliters-codegen/src/select.rs` | n/a |  |
| `codeDistinct` | `code_distinct` | `sqliters-codegen/src/select.rs` | n/a |  |
| `fixDistinctOpenEph` | `fix_distinct_open_eph` | `sqliters-codegen/src/select.rs` | n/a |  |
| `innerLoopLoadRow` | `inner_loop_load_row` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `makeSorterRecord` | `make_sorter_record` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `pushOntoSorter` | `push_onto_sorter` | `sqliters-codegen/src/select.rs` | 95.2 / 80.0 (V2) | 4 |
| `selectInnerLoop` | `select_inner_loop` | `sqliters-codegen/src/select.rs` | 87.9 / 89.6 (V3) | 7 |
| `explainTempTable` | `explain_temp_table` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `generateSortTail` | `generate_sort_tail` | `sqliters-codegen/src/select.rs` | 77.2 / 76.4 (V2) | 7 |
| `computeLimitRegisters` | `compute_limit_registers` | `sqliters-codegen/src/select.rs` | n/a |  |
| `multiSelectValues` | `multi_select_values` | `sqliters-codegen/src/select.rs` | n/a |  |
| `unionWithLimitOne` | `union_with_limit_one` | `sqliters-codegen/src/select.rs` | n/a |  |
| `hasAnchor` | `has_anchor` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V3) | 0 |
| `generateWithRecursiveQuery` | `generate_with_recursive_query` | `sqliters-codegen/src/select.rs` | 97.6 / 83.3 (V3) | 6 |
| `multiSelect` | `multi_select` | `sqliters-codegen/src/select.rs` | 96.9 / 82.5 (V3) | 6 |
| `sqlite3CopySortOrder` | `sqlite3_copy_sort_order` | `sqliters-codegen/src/select.rs` | n/a |  |
| `minMaxQuery` | `min_max_query` | `sqliters-codegen/src/select.rs` | n/a |  |
| `analyzeAggFuncArgs` | `analyze_agg_func_args` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V3) | 0 |
| `optimizeAggregateUseOfIndexedExpr` | `optimize_aggregate_use_of_indexed_expr` | `sqliters-codegen/src/select.rs` | 100.0 / 83.3 (V3) | 1 |
| `aggregateIdxEprRefToColCallback` | `aggregate_idx_epr_ref_to_col_callback` | `sqliters-codegen/src/select.rs` | 100.0 / 80.0 (V3) | 2 |
| `aggregateConvertIndexedExprRefToColumn` | `aggregate_convert_indexed_expr_ref_to_column` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V3) | 0 |
| `assignAggregateRegisters` | `assign_aggregate_registers` | `sqliters-codegen/src/select.rs` | n/a |  |
| `resetAccumulator` | `reset_accumulator` | `sqliters-codegen/src/select.rs` | 48.4 / 41.7 (V2) | 7 |
| `finalizeAggFunctions` | `finalize_agg_functions` | `sqliters-codegen/src/select.rs` | n/a |  |
| `updateAccumulator` | `update_accumulator` | `sqliters-codegen/src/select.rs` | 100.0 / 91.4 (V3) | 6 |
| `havingToWhere` | `having_to_where` | `sqliters-codegen/src/select.rs` | n/a |  |
| `isSimpleCount` | `is_simple_count` | `sqliters-codegen/src/select.rs` | 100.0 / 72.7 (V2) | 6 |
| `explainSimpleCount` | `explain_simple_count` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `isSelfJoinView` | `is_self_join_view` | `sqliters-codegen/src/select.rs` | 94.1 / 80.0 (V2) | 2 |
| `fromClauseTermCanBeCoroutine` | `from_clause_term_can_be_coroutine` | `sqliters-codegen/src/select.rs` | 86.4 / 66.7 (V2) | 6 |
| `sqlite3Select` | `sqlite3_select` | `sqliters-codegen/src/select.rs` | 92.5 / 84.1 (V3) | 50 |

### whereexpr.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ExprIsLikeOperator` | `sqlite3_expr_is_like_operator` | `sqliters-codegen/src/where_.rs` | 80.0 / 75.0 (V2) | 1 |
| `whereClauseInsert` | `where_clause_insert` | `sqliters-codegen/src/whereexpr.rs` | 82.6 / 66.7 (V2) | 3 |
| `allowedOp` | `allowed_op` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `exprCommute` | `expr_commute` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 (V2) | 2 |
| `operatorMask` | `operator_mask` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `isLikeOrGlob` | `is_like_or_glob` | `sqliters-codegen/src/whereexpr.rs` | 64.8 / 48.8 (V2) | 22 |
| `transferJoinMarkings` | `transfer_join_markings` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 (V2) | 1 |
| `markTermAsChild` | `mark_term_as_child` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereNthSubterm` | `where_nth_subterm` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereCombineDisjuncts` | `where_combine_disjuncts` | `sqliters-codegen/src/whereexpr.rs` | 91.7 / 83.3 (V2) | 4 |
| `exprAnalyzeOrTerm` | `expr_analyze_or_term` | `sqliters-codegen/src/whereexpr.rs` | 93.3 / 78.6 (V2) | 16 |
| `termIsEquivalence` | `term_is_equivalence` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 95.5 (V2) | 1 |
| `exprSelectUsage` | `expr_select_usage` | `sqliters-codegen/src/whereexpr.rs` | 94.7 / 75.0 (V2) | 3 |
| `exprMightBeIndexed2` | `expr_might_be_indexed` | `sqliters-codegen/src/whereexpr.rs` | 69.2 / 78.6 (V2) | 2 |
| `exprAnalyze` | `expr_analyze` | `sqliters-codegen/src/whereexpr.rs` | n/a |  |
| `sqlite3WhereSplit` | `sqlite3_where_split` | `sqliters-codegen/src/whereexpr.rs` | n/a |  |
| `sqlite3WhereExprUsageFull` | `sqlite3_where_expr_usage_nn` | `sqliters-codegen/src/whereexpr.rs` | 80.0 / 88.9 (V2) | 2 |
| `sqlite3WhereExprUsage` | `sqlite3_where_expr_usage` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereExprListUsage` | `sqlite3_where_expr_list_usage` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereExprAnalyze` | `sqlite3_where_expr_analyze` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereTabFuncArgs` | `sqlite3_where_tab_func_args` | `sqliters-codegen/src/whereexpr.rs` | 7.4 / 5.6 (V2) | 8 |
| `sqlite3WhereLoopBloomable` | `sqlite3_where_loop_bloomable` | `sqliters-codegen/src/whereexpr.rs` | 100.0 / 75.0 (V2) | 1 |

### where.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3IndexColumnAffinity` | `sqlite3_index_column_affinity` | `sqliters-codegen/src/schema.rs` | n/a |  |
| `sqlite3WhereOrderByLimitOptLabel` | `sqlite3_where_order_by_limit_opt_label` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereMinMaxOptEarlyOut` | `sqlite3_where_min_max_opt_early_out` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereOkOnePass` | `sqlite3_where_ok_one_pass` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereOrInsert` | `where_or_insert` | `sqliters-codegen/src/where_.rs` | 100.0 / 95.0 (V2) | 1 |
| `sqlite3WhereGetMask` | `sqlite3_where_get_mask` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `createMask` | `create_mask` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereRightSubexprIsColumn` | `where_right_subexpr_is_column` | `sqliters-codegen/src/where_.rs` | 100.0 / 66.7 (V2) | 1 |
| `indexInAffinityOk` | `index_in_affinity_ok` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereScanNext` | `where_scan_next` | `sqliters-codegen/src/where_.rs` | 98.1 / 91.4 (V2) | 4 |
| `whereScanInit` | `where_scan_init` | `sqliters-codegen/src/where_.rs` | 96.4 / 90.0 (V2) | 1 |
| `sqlite3WhereFindTerm` | `sqlite3_where_find_term` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `findIndexCol` | `find_index_col` | `sqliters-codegen/src/where_.rs` | 100.0 / 78.6 (V2) | 3 |
| `indexColumnNotNull` | `index_column_not_null` | `sqliters-codegen/src/where_.rs` | 85.7 / 75.0 (V2) | 1 |
| `isDistinctRedundant` | `is_distinct_redundant` | `sqliters-codegen/src/where_.rs` | 100.0 / 90.0 (V2) | 3 |
| `estLog` | `est_log` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `translateColumnToCopy` | `translate_column_to_copy` | `sqliters-codegen/src/where_.rs` | n/a |  |
| `constraintCompatibleWithOuterJoin` | `constraint_compatible_with_outer_join` | `sqliters-codegen/src/where_.rs` | 87.5 / 87.5 (V2) | 1 |
| `columnIsGoodIndexCandidate` | `column_is_good_index_candidate` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `termCanDriveIndex` | `term_can_drive_index` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `constructAutomaticIndex` | `construct_automatic_index` | `sqliters-codegen/src/where_.rs` | 93.6 / 84.6 (V2) | 10 |
| `sqlite3ConstructBloomFilter` | `sqlite3_construct_bloom_filter` | `sqliters-codegen/src/where_.rs` | 98.3 / 92.3 (V2) | 2 |
| `whereRangeAdjust` | `where_range_adjust` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereRangeScanEst` | `where_range_scan_est` | `sqliters-codegen/src/where_.rs` | 100.0 / 91.7 (V2) | 1 |
| `whereLoopCheaperProperSubset` | `where_loop_cheaper_proper_subset` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereLoopAdjustCost` | `where_loop_adjust_cost` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereLoopFindLesser` | `where_loop_find_lesser` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereLoopInsert` | `where_loop_insert` | `sqliters-codegen/src/where_.rs` | 94.4 / 80.8 (V2) | 4 |
| `whereLoopOutputAdjust` | `where_loop_output_adjust` | `sqliters-codegen/src/where_.rs` | 100.0 / 94.2 (V2) | 3 |
| `whereRangeVectorLen` | `where_range_vector_len` | `sqliters-codegen/src/where_.rs` | 95.5 / 75.0 (V2) | 5 |
| `whereLoopAddBtreeIndex` | `where_loop_add_btree_index` | `sqliters-codegen/src/where_.rs` | 97.6 / 90.6 (V2) | 10 |
| `indexMightHelpWithOrderBy` | `index_might_help_with_order_by` | `sqliters-codegen/src/where_.rs` | 94.1 / 92.9 (V2) | 2 |
| `whereUsablePartialIndex` | `where_usable_partial_index` | `sqliters-codegen/src/where_.rs` | 86.7 / 77.3 (V2) | 4 |
| `exprIsCoveredByIndex` | `expr_is_covered_by_index` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereIsCoveringIndexWalkCallback` | `where_is_covering_index_walk_callback` | `sqliters-codegen/src/where_.rs` | 100.0 / 92.9 (V2) | 1 |
| `whereIsCoveringIndex` | `where_is_covering_index` | `sqliters-codegen/src/where_.rs` | 82.6 / 50.0 (V2) | 4 |
| `wherePartIdxExpr` | `where_part_idx_expr` | `sqliters-codegen/src/where_.rs` | 93.8 / 75.0 (V2) | 7 |
| `whereLoopAddBtree` | `where_loop_add_btree` | `sqliters-codegen/src/where_.rs` | 100.0 / 87.3 (V2) | 15 |
| `whereLoopAddOr` | `where_loop_add_or` | `sqliters-codegen/src/where_.rs` | 98.4 / 87.5 (V2) | 4 |
| `whereLoopAddAll` | `where_loop_add_all` | `sqliters-codegen/src/where_.rs` | 82.6 / 68.8 (V2) | 6 |
| `wherePathMatchSubqueryOB` | `where_path_match_subquery_ob` | `sqliters-codegen/src/where_.rs` | 0.0 / 0.0 (V2) | 12 |
| `wherePathSatisfiesOrderBy` | `where_path_satisfies_order_by` | `sqliters-codegen/src/where_.rs` | 94.0 / 87.5 (V2) | 16 |
| `whereSortingCost` | `where_sorting_cost` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `computeMxChoice` | `compute_mx_choice` | `sqliters-codegen/src/where_.rs` | 100.0 / 94.0 (V2) | 3 |
| `whereLoopIsNoBetter` | `where_loop_is_no_better` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `wherePathSolver` | `where_path_solver` | `sqliters-codegen/src/where_.rs` | 94.7 / 90.6 (V2) | 9 |
| `whereInterstageHeuristic` | `where_interstage_heuristic` | `sqliters-codegen/src/where_.rs` | 93.3 / 85.7 (V2) | 2 |
| `whereShortCut` | `where_short_cut` | `sqliters-codegen/src/where_.rs` | 100.0 / 91.3 (V2) | 4 |
| `whereOmitNoopJoin` | `where_omit_noop_join` | `sqliters-codegen/src/where_.rs` | 97.1 / 97.1 (V2) | 1 |
| `whereCheckIfBloomFilterIsUseful` | `where_check_if_bloom_filter_is_useful` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 (V2) | 0 |
| `whereAddIndexedExpr` | `where_add_indexed_expr` | `sqliters-codegen/src/where_.rs` | 100.0 / 81.2 (V3) | 3 |
| `whereReverseScanOrder` | `where_reverse_scan_order` | `sqliters-codegen/src/where_.rs` | 0.0 / 0.0 (V2) | 5 |
| `sqlite3WhereBegin` | `sqlite3_where_begin` | `sqliters-codegen/src/where_.rs` | 89.1 / 79.3 (V2) | 38 |
| `sqlite3WhereEnd` | `sqlite3_where_end` | `sqliters-codegen/src/where_.rs` | 89.0 / 86.1 (V2) | 14 |
| `sqlite3WhereOutputRowCount` | `output_row_count` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereIsDistinct` | `is_distinct` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereIsOrdered` | `is_ordered` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereContinueLabel` | `continue_label` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereBreakLabel` | `break_label` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereIsSorted` | `is_sorted` | `sqliters-codegen/src/whereint.rs` | 0.0 / 100.0 (V2) | 0 |
| `sqlite3WhereUsesDeferredSeek` | `uses_deferred_seek` | `sqliters-codegen/src/whereint.rs` | 100.0 / 100.0 (V2) | 0 |

### wherecode.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `explainIndexColumnName` | `explain_index_column_name` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `explainAppendTerm` | `explain_append_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `explainIndexRange` | `explain_index_range` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereAddExplainText` | `sqlite3_where_add_explain_text` | `sqliters-codegen/src/wherecode.rs` | 94.3 / 84.0 (V2) | 6 |
| `sqlite3WhereExplainOneScan` | `sqlite3_where_explain_one_scan` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 87.5 (V2) | 1 |
| `sqlite3WhereExplainBloomFilter` | `sqlite3_where_explain_bloom_filter` | `sqliters-codegen/src/wherecode.rs` | 95.7 / 87.5 (V2) | 1 |
| `disableTerm` | `disable_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 93.8 (V2) | 1 |
| `codeApplyAffinity` | `code_apply_affinity` | `sqliters-codegen/src/wherecode.rs` | 91.7 / 91.7 (V2) | 1 |
| `updateRangeAffinityStr` | `update_range_affinity_str` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `adjustOrderByCol` | `adjust_order_by_col` | `sqliters-codegen/src/wherecode.rs` | 18.2 / 8.3 (V2) | 6 |
| `removeUnindexableInClauseTerms` | `remove_unindexable_in_clause_terms` | `sqliters-codegen/src/wherecode.rs` | 97.6 / 71.4 (V2) | 8 |
| `codeINTerm` | `code_in_term` | `sqliters-codegen/src/wherecode.rs` | 98.5 / 94.4 (V2) | 3 |
| `codeEqualityTerm` | `code_equality_term` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `codeAllEqualityTerms` | `code_all_equality_terms` | `sqliters-codegen/src/wherecode.rs` | 97.4 / 90.0 (V2) | 3 |
| `whereLikeOptimizationStringFixup` | `where_like_optimization_string_fixup` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `codeDeferredSeek` | `code_deferred_seek` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 75.0 (V2) | 2 |
| `codeExprOrVector` | `code_expr_or_vector` | `sqliters-codegen/src/wherecode.rs` | 72.7 / 75.0 (V2) | 2 |
| `whereApplyPartialIndexConstraints` | `where_apply_partial_index_constraints` | `sqliters-codegen/src/wherecode.rs` | 80.0 / 87.5 (V2) | 1 |
| `filterPullDown` | `filter_pull_down` | `sqliters-codegen/src/wherecode.rs` | 78.3 / 70.0 (V2) | 3 |
| `whereLoopIsOneRow` | `where_loop_is_one_row` | `sqliters-codegen/src/wherecode.rs` | 88.9 / 80.0 (V2) | 2 |
| `whereExprIsReady` | `where_expr_is_ready` | `sqliters-codegen/src/wherecode.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WhereCodeOneLoopStart` | `sqlite3_where_code_one_loop_start` | `sqliters-codegen/src/wherecode.rs` | 85.8 / 79.8 (V2) | 49 |
| `sqlite3WhereRightJoinLoop` | `sqlite3_where_right_join_loop` | `sqliters-codegen/src/wherecode.rs` | 93.2 / 87.5 (V2) | 4 |

### vdbeaux.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3VdbeExplain` | `sqlite3_vdbe_explain` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3VdbeExplainPop` | `sqlite3_vdbe_explain_pop` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3VdbeExplainParent` | `sqlite3_vdbe_explain_parent` | `sqliters-codegen/src/build.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3VdbeSetP4KeyInfo` | `sqlite3_vdbe_set_p4_key_info` | `sqliters-codegen/src/schema.rs` | 100.0 / 50.0 (V2) | 1 |

### callback.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3FindCollSeq` | `sqlite3_find_coll_seq` | `sqliters-codegen/src/callback.rs` | n/a |  |
| `sqlite3GetCollSeq` | `sqlite3_get_coll_seq` | `sqliters-codegen/src/callback.rs` | n/a |  |
| `sqlite3LocateCollSeq` | `sqlite3_locate_coll_seq` | `sqliters-codegen/src/callback.rs` | 87.5 / 66.7 (V2) | 1 |
| `matchQuality` | `match_quality` | `sqliters-codegen/src/callback.rs` | n/a |  |
| `sqlite3FindFunction` | `sqlite3_find_function` | `sqliters-codegen/src/callback.rs` | n/a |  |

### delete.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3GenerateIndexKey` | `sqlite3_generate_index_key` | `sqliters-codegen/src/where_.rs` | 88.0 / 46.1 (V2) | 6 |

### func.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3_strglob` | `sqlite3_strglob_prefix` | `sqliters-codegen/src/build_tab.rs` | 66.7 / 50.0 (V3) | 2 |
| `sqlite3IsLikeFunction` | `sqlite3_is_like_function` | `sqliters-codegen/src/whereexpr.rs` | 52.6 / 22.2 (V2) | 7 |

### hash.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3HashFind` | `hash_find` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |

### insert.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3OpenTable` | `sqlite3_open_table` | `sqliters-codegen/src/schema.rs` | 81.8 / 75.0 (V2) | 1 |
| `sqlite3IndexAffinityStr` | `sqlite3_index_affinity_str` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3MultiValuesEnd` | `sqlite3_multi_values_end` | `sqliters-codegen/src/select.rs` | n/a |  |
| `exprListIsNoAffinity` | `expr_list_is_no_affinity` | `sqliters-codegen/src/select.rs` | n/a |  |
| `sqlite3MultiValues` | `sqlite3_multi_values` | `sqliters-codegen/src/select.rs` | n/a |  |

### main.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3IsBinary` | `sqlite3_is_binary` | `sqliters-codegen/src/exprcode.rs` | n/a |  |

### malloc.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3DbSpanDup` | `span_dup` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 75.0 (V3) | 1 |

### prepare.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3InitCallback` | `sqlite3_init_callback` | `sqliters-codegen/src/build_tab.rs` | 66.7 / 39.3 (V3) | 23 |
| `sqlite3InitOne` | `sqlite3_init_one` | `sqliters-codegen/src/build_tab.rs` | 78.3 / 62.1 (V3) | 14 |
| `sqlite3Init` | `sqlite3_init` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 83.3 (V3) | 2 |
| `sqlite3Prepare` | `prepare` | `sqliters-codegen/src/lib.rs` | 52.9 / 45.6 (V2) | 16 |
| `sqlite3ReadSchema` | `sqlite3_read_schema` | `sqliters-codegen/src/schema.rs` | 81.8 / 66.7 (V3) | 2 |

### printf.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3RecordErrorOffsetOfExpr` | `record_error_offset_of_expr` | `sqliters-codegen/src/sqliteint.rs` | n/a |  |

### tokenize.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3GetToken` | `sqlite3_get_token` | `sqliters-codegen/src/tokenize.rs` | n/a |  |
| `getToken` | `get_token` | `sqliters-codegen/src/tokenize.rs` | n/a |  |
| `analyzeWindowKeyword` | `analyze_window_keyword` | `sqliters-codegen/src/tokenize.rs` | n/a |  |
| `analyzeOverKeyword` | `analyze_over_keyword` | `sqliters-codegen/src/tokenize.rs` | n/a |  |
| `analyzeFilterKeyword` | `analyze_filter_keyword` | `sqliters-codegen/src/tokenize.rs` | n/a |  |
| `sqlite3RunParser` | `sqlite3_run_parser` | `sqliters-codegen/src/tokenize.rs` | n/a |  |

### update.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ColumnDefault` | `sqlite3_column_default` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 (V3) | 0 |

### utf.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3Utf8Read` | `utf8_read` | `sqliters-codegen/src/whereexpr.rs` | 50.0 / 12.5 (V2) | 3 |

### util.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3DequoteToken` | `sqlite3_dequote_token` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 87.5 (V3) | 1 |
| `sqlite3DequoteNumber` | `sqlite3_dequote_number` | `sqliters-codegen/src/expr.rs` | n/a |  |
| `sqlite3ColumnType` | `sqlite3_column_type` | `sqliters-codegen/src/schema.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3LogEst` | `sqlite3_log_est` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3LogEstAdd` | `sqlite3_log_est_add` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ErrorMsg` | `error_msg` | `sqliters-codegen/src/sqliteint.rs` | n/a |  |
| `sqlite3HexToInt` | `hex_to_int` | `sqliters-codegen/src/util.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3Dequote` | `sqlite3_dequote` | `sqliters-codegen/src/util.rs` | n/a |  |
| `sqlite3StrICmp` | `str_ieq` | `sqliters-codegen/src/util.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3GetInt32` | `sqlite3_get_int32` | `sqliters-codegen/src/util.rs` | n/a |  |
| `sqlite3DecOrHexToI64` | `sqlite3_dec_or_hex_to_i64` | `sqliters-codegen/src/util.rs` | n/a |  |
| `sqlite3HexToBlob` | `sqlite3_hex_to_blob` | `sqliters-codegen/src/util.rs` | n/a |  |
| `sqlite3LogEstToInt` | `sqlite3_log_est_to_int` | `sqliters-codegen/src/where_.rs` | 100.0 / 87.5 (V2) | 1 |

### vdbe.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ValueApplyAffinity` | `apply_affinity` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 100.0 (V3) | 0 |

### vdbemem.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3ValueFromExpr` | `sqlite3_value_from_expr` | `sqliters-codegen/src/build_tab.rs` | 100.0 / 85.7 (V3) | 2 |
| `valueFromExpr` | `value_from_expr` | `sqliters-codegen/src/build_tab.rs` | 88.6 / 78.6 (V3) | 13 |

### walker.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3WalkExprNN` | `sqlite3_walk_expr_nn` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 (V3) | 0 |
| `walkWindowList` | `walk_window_list` | `sqliters-codegen/src/walker.rs` | 100.0 / 57.1 (V3) | 6 |
| `sqlite3WalkExpr` | `sqlite3_walk_expr` | `sqliters-codegen/src/walker.rs` | n/a |  |
| `sqlite3WalkExprList` | `sqlite3_walk_expr_list` | `sqliters-codegen/src/walker.rs` | n/a |  |
| `sqlite3WalkSelectExpr` | `sqlite3_walk_select_expr` | `sqliters-codegen/src/walker.rs` | n/a |  |
| `sqlite3WalkSelectFrom` | `sqlite3_walk_select_from` | `sqliters-codegen/src/walker.rs` | n/a |  |
| `sqlite3WalkSelect` | `sqlite3_walk_select` | `sqliters-codegen/src/walker.rs` | n/a |  |
| `sqlite3WalkerDepthIncrease` | `sqlite3_walker_depth_increase` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3WalkerDepthDecrease` | `sqlite3_walker_depth_decrease` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3ExprWalkNoop` | `sqlite3_expr_walk_noop` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 (V2) | 0 |
| `sqlite3SelectWalkNoop` | `sqlite3_select_walk_noop` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 (V2) | 0 |

### window.c
| C function | Rust item | file | cov (lines / branches) | untaken-branch lines |
|---|---|---|---|---|
| `sqlite3WindowAttach` | `sqlite3_window_attach` | `sqliters-codegen/src/window.rs` | 77.8 / 33.3 (V3) | 2 |
| `sqlite3WindowDup` | `sqlite3_window_dup` | `sqliters-codegen/src/window.rs` | 100.0 / 50.0 (V3) | 2 |
| `sqlite3WindowCompare` | `sqlite3_window_compare` | `sqliters-codegen/src/window.rs` | 87.5 / 54.2 (V3) | 10 |

## 2. Decision tables (C source → rule → Rust)

Sections 2.2–2.8 are V2's tables (unchanged code, re-verified by the V3 runs of plan_a / plan_b); 2.1 replaces V2's schema-adapter table; 2.9–2.13 are new in V3.

### 2.1 Schema objects as C builds them: schema loading (prepare.c, build.c, analyze.c, vdbemem.c) — `build_tab.rs`  *(layer: schema dump)*

V3 replaces the V2 adapter (objects rebuilt from the engine's schema plus scans of the CREATE text) by C's own path:
the generator's parser runs over the `sql` column of every `sqlite_schema` row in init mode, exactly as
`sqlite3InitCallback()` does, and the same grammar actions build the Table / Column / Index / FKey objects.

| C | Rule | Rust | Verified by |
|---|---|---|---|
| `sqlite3Init` prepare.c | main (0) first, then `nDb-1 … 1` (TEMP last); a database is loaded once per statement that needs it (`sqlite3ReadSchema`) | `sqlite3_init` | schema dump of TEMP / attached schemas |
| `sqlite3InitOne` | the rows of `sqlite_schema` in rowid order through `sqlite3InitCallback`, then `sqlite3AnalysisLoad` | `sqlite3_init_one` (rows from `Host::schema_rows`) | schema dump |
| `sqlite3InitCallback` | a row whose `sql` starts with "cr": nested parse with `init.busy=1`, `init.iDb`, `init.newTnum = rootpage`, `init.azInit = {type,name,tbl_name}`; a row with NULL `sql`: an automatic index, found by name and given its root page; a parse error leaves the object out (C: corrupt schema) | `sqlite3_init_callback`, `init_parse` (saves and restores the parser state of the statement being compiled) — a failed row marks `name` / `tbl_name` "bad": a statement that uses it is reported unsupported | `LOADERR` lines of the schema dump (0 on the corpora) |
| `cmdx ::= cmd` in init mode | no code is generated for the nested statement | rule 2 skips `sqlite3_finish_coding` when `init_busy` | listings contain no stray `Halt` |
| `sqlite3StartTable` | name and database from the token(s) or, for root page 1, the schema table's own name; duplicate names / reserved names as C | `sqlite3_start_table` | T lines |
| `sqlite3AddColumn` | the type token decides `eCType` (ANY BLOB INT INTEGER REAL TEXT → standard type, affinity from the table, `szEst=5` when the affinity is ≤ TEXT), otherwise `sqlite3AffinityType` gives affinity and `szEst`; `COLFLAG_HASTYPE`; duplicate column names are an error; `GENERATED ALWAYS` at the end of the type is cut off | `sqlite3_add_column` | C lines (`aff`, `szEst`, `eCType`, `flags`) |
| `sqlite3AddNotNull` | `notNull = onError`, `COLFLAG_HASTYPE` unaffected; every index already built on the column that is UNIQUE gets `uniqNotNull=1`; `TF_HasNotNull` | `sqlite3_add_not_null` | I lines (`fl`), KeyInfo of listings |
| `sqlite3AddDefaultValue` | the expression is stored as `TK_SPAN` over an `EXPRDUP_REDUCE` copy in `pTab->u.tab.pDfltList` (`iDflt`); a non-constant default is an error ("default value of column [x] is not constant"); a generated column cannot have one | `sqlite3_add_default_value`, `sqlite3_column_set_expr` | C lines (`iDflt`, `dflt`) |
| `sqlite3ValueFromExpr` / `valueFromExpr` vdbemem.c | the P4 of `OP_Column` for a column added by ALTER TABLE: the literal becomes a Mem (`EP_IntValue` or `sqlite3DecOrHexToI64` → integer, otherwise the text sign+token); BLOB affinity: FLOAT → real (`sqlite3AtoF`), an INTEGER token that did not fit → NUMERIC affinity; other affinities through `applyAffinity` (vdbe.c:397): under NUMERIC / INTEGER / REAL an integer stays an integer, a real goes through `sqlite3VdbeIntegerAffinity` (exact and not the extreme values → integer), text through `applyNumericAffinity(p,1)` (`alsoAnInt`: `sqlite3RealSameAsInt` — equal bits and within ±2^51 — or a valid 64-bit integer text); TEXT affinity stringifies numbers; numbers drop `MEM_Str`; `-literal` in one step (not for hex), `-(-x)` by `sqlite3VdbeMemNumerify` + negation; TRUE / FALSE; hex blob; finally `sqlite3VdbeChangeEncoding` to the database encoding (the UTF-16 listing of such a P4 is `pMem->z` as a C string: lib.rs `c_list_rows`, all-kinds #26467) | `sqlite3_value_from_expr`, `value_from_expr`, `apply_affinity` | C lines `dflt=<type>:<text>` (found 3 rule gaps: integer under REAL, TRUE/FALSE, the 48-bit rule) |
| `sqlite3AddPrimaryKey` | one column, declared type exactly INTEGER, and — for the *column* constraint only — not DESC: the column is the rowid alias (`iPKey`, `keyConf`, AUTOINCREMENT), no index; the *table* constraint form passes `sortOrder=0`, so `PRIMARY KEY(c DESC)` on an INTEGER column is the alias too; otherwise `sqlite3CreateIndex(…, SQLITE_IDXTYPE_PRIMARYKEY)`; a second PRIMARY KEY is an error; `COLFLAG_PRIMKEY` on each named column | `sqlite3_add_primary_key` | T lines (`iPKey`), I lines; engine side of the same rule: D1 |
| `sqlite3AddCheckConstraint`, `sqlite3AddCollateType`, `sqlite3AddGenerated` | CHECK list kept (`nCheck`); COLLATE sets the column collation and re-labels single-column indexes already built on it; generated columns: `COLFLAG_VIRTUAL` / `COLFLAG_STORED`, `TF_HasVirtual` / `TF_HasStored`, `nNVCol` decremented for VIRTUAL, PRIMARY KEY on a generated column is an error | same names in snake case | C lines, T lines (`nNVCol`) |
| `sqlite3CreateForeignKey`, `sqlite3DeferForeignKey` | FKey objects (parent, columns, actions, deferred); each new FKey is put at the head of `pTab->u.tab.pFKey` (so the list is in reverse declaration order); `sqlite3DeferForeignKey` changes the head (the FKey just created) | same names | F lines (994 foreign keys of the DDL corpus) |
| `sqlite3CreateIndex` | column loop as in V2's table (rowid alias, `uniqNotNull`, `XN_EXPR` / `bHasExpr`, collation: explicit, the column's, or the static BINARY); **an identical automatic index is not created twice** (same columns and collations: the constraints merge, the explicit ON CONFLICT wins, two different explicit ones are the error "conflicting ON CONFLICT clauses specified"); a new index goes to the head of `pTab->pIndex`, `OE_Replace` ones behind the others (`reorder_replace_indexes`); automatic index names `sqlite_autoindex_<tbl>_<n>` count the indexes existing at that moment; in init mode the root page is `init.newTnum`; a VIRTUAL column in the index sets `bHasVCol` / `bHasExpr` | `sqlite3_create_index` | I lines (order, name, `tnum`, `onError`, `cols`, `sort`, `coll`, `fl`) |
| `sqlite3EndTable` | `TF_Strict` checks and NOT NULL on PRIMARY KEY columns of STRICT tables, `TF_WithoutRowid` → `convertToWithoutRowidTable`, generated-column resolution, `estimateTableWidth` / `estimateIndexWidth`, CHECK and generated expressions resolved with `NC_IsCheck` / `NC_GenCol`, the object enters the schema hash | `sqlite3_end_table` | T / C / I lines |
| `convertToWithoutRowidTable` | NOT NULL on the PRIMARY KEY columns; a rowid-alias PRIMARY KEY becomes a real index **now** (so it is the last automatic index); the PRIMARY KEY loses repeated columns (`nKeyCol` shrinks), gets every other column appended; each secondary index gets the PRIMARY KEY columns it lacks (`isDupColumn`), `bAscKeyBug` | `convert_to_without_rowid_table` | I lines of WITHOUT ROWID tables |
| `resizeIndexObject` | arrays re-allocated with N entries and only `nColumn` entries copied; `aiRowLogEst[nColumn]` is afterwards the memory of `aiColumn[0]` (V2 finding) | `resize_index_object` (truncates to `n_column` first), `Index.is_resized` | I lines (`sort` after PK de-duplication), LOOP `nOut` |
| `sqlite3DefaultRowEst` at creation vs at load | `sqlite3CreateIndex` calls it with the `nKeyCol` of that moment; `sqlite3AnalysisLoad` calls it again for every index without statistics. A table created by the running connection (OP_ParseSchema) is *not* seen by that second pass until the next schema load or ANALYZE, so a WITHOUT ROWID PRIMARY KEY with repeated columns keeps the estimate computed before the de-duplication | `sqlite3_analysis_load` skips the pass for `Host::created_since_load` tables (engine: `DbSlot.c_incremental`, set by CREATE TABLE, cleared by schema resets and ANALYZE of that database) | I lines `est=[200,33,32]` vs `[200,33,0]` |
| `sqlite3AnalysisLoad` / `analysisLoader` / `decodeIntArray` | as in V2 (index row, table row, `unordered`, `sz=`, `noskipscan`), now run by the generator on the raw rows of `sqlite_stat1` (`Host::stat1_rows`) | `sqlite3_analysis_load`, `analysis_loader`, `decode_int_array` | plans of the corpora with hand-written statistics |
| `sqlite3CreateView` / `sqlite3FixSelect` | as in V2; the view's SELECT comes from the nested parse instead of a re-parse on demand | `sqlite3_create_view`, `fix_select` | view statements |
| triggers, virtual tables | a trigger row is only noted (`has_trigger`); a `CREATE VIRTUAL TABLE` row becomes a `TABTYP_VTAB` stub that statements report as unsupported | `sqlite3_init_callback`, `is_create_virtual_table` | — |


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

### 2.8 Connection state the listing shows (V2; V3 additions in 2.13)
| C | Rule | Engine |
|---|---|---|
| `sqlite3FinishCoding` | `OP_Transaction iDb, write, schema_cookie, iGeneration` with P5=1 for every database the statement reads | `DbInfo.schema_cookie` (engine schema), `DbSlot::c_generation` |
| `sqlite3SchemaClear` | `iGeneration++` when a loaded schema is cleared: `sqlite3RollbackAll` / `OP_Savepoint ROLLBACK` with `DBFLAG_SchemaChange`, `OP_ParseSchema` with P4==0 (ALTER TABLE: the database and temp), end of `sqlite3RunVacuum`, failed `OP_VCreate` in autocommit mode | `DatabaseSet::c_reset_all_schemas`, `c_reparse_schema`, hooks in `engine.rs` / `lib.rs` |
| `sqlite3ReadSchema` → `sqlite3Init` | a prepare that loads a schema resets the error offset | `DbSlot::c_loaded`, `Db.schema_not_loaded` |
| `db->flags` | `SQLITE_AutoIndex`, `SQLITE_ReverseOrder`, `SQLITE_ShortColNames`, `SQLITE_FullColNames` from their pragmas | `DatabaseSet::c_flags_toggled` |



### 2.9 Aggregates and GROUP BY (select.c tag-select-0800…0822, expr.c) — `select.rs`, `expr.rs`  *(layer: aggregate-info dump + EXPLAIN bytes)*

| C | Decision | Rust | Verified by |
|---|---|---|---|
| `sqlite3Select` tag-select-0500 | a DISTINCT query without aggregates whose ORDER BY equals its result list is rewritten as GROUP BY (`SF_Distinct` cleared, `isTnct=2`, `WHERE_DISTINCTBY`) | `sqlite3_select_body` | AGG line `nGroupBy`, EQP "USE TEMP B-TREE FOR DISTINCT" |
| ORDER BY = GROUP BY | `orderByGrp` when both lists compare equal after `sqlite3CopySortOrder`: the GROUP BY takes the ORDER BY's directions (`WHERE_SORTBYGROUP`), and the ORDER BY sorter is cancelled (`OP_Noop`) if the rows already come out sorted (`groupBySort` or `sqlite3WhereIsSorted`) | `sqlite3_copy_sort_order`, GROUP BY block | AGG `x[1]`, listing |
| `sqlite3ExprAnalyzeAggregates` / `analyzeAggregate` | every column of this query level becomes an `aCol[]` entry (`findOrCreateAggInfoColumn`: same table+column reuses the entry; `iSorterColumn` = position in the GROUP BY when the column is a GROUP BY term, otherwise the next sorter column); every aggregate call at this level (`op2` = depth) becomes an `aFunc[]` entry unless an identical call exists (`sqlite3ExprCompare`); ORDER BY aggregate: `iOBTab` cursor, `bOBPayload` unless the single ORDER BY term equals the single argument, `bOBUnique` for DISTINCT in that case, `bUseSubtype` for SQLITE_SUBTYPE functions; DISTINCT: `iDistinct` cursor unless `bOBUnique` | `analyze_aggregate`, `find_or_create_agg_info_column` | AGGCOL / AGGFUNC lines (every field) |
| `analyzeAggregate` default branch | inside aggregate arguments (`NC_InAggFunc`), an expression equal to an indexed expression of an index in use (`pParse->pIdxEpr`, newest first) becomes an `aCol[]` entry on the *index* cursor | `analyze_aggregate` (`_` arm) | AGGCOL `iTable` = index cursor, `tab=-` |
| `havingToWhere` / `havingToWhereExprCb` (select.c:7120) | with GROUP BY, for each top-level AND term of HAVING: when `sqlite3ExprIsConstantOrGroupBy` accepts it, it is not always-false (`ExprAlwaysFalse`) and has no `pAggInfo` (a correlated reference into an outer aggregate), the term is *moved*: AND-ed to the end of the WHERE clause, and replaced in HAVING by the integer 1 | `having_to_where` | AGG `nWhere` / `nHaving` |
| `minMaxQuery` | a single `min(x)` / `max(x)` without FILTER, GROUP BY, HAVING becomes an ORDER BY for the planner (`WHERE_ORDERBY_MIN/MAX`) with early termination | `min_max_query` | AGG `x[0]` of kind `plain` |
| `isSimpleCount` | `SELECT count(*) FROM tbl` alone: `OP_Count` on the table or its smallest suitable index (not unordered, not partial, narrower than the table; `NOT INDEXED` keeps the table) | `is_simple_count` | AGG kind `count`, `x[0]` = root page |
| tag-select-0810: sorting index | `sortingIdx` cursor and `OP_SorterOpen` are always allocated; `nSortingColumn` = GROUP BY terms plus non-GROUP-BY columns | GROUP BY block | AGG `nSortCol`, `sortIdx` |
| tag-select-0810: one DISTINCT aggregate | `nFunc==1` with `iDistinct>=0`: the DISTINCT is offered to the planner (`pDistinct` = GROUP BY + argument, `WHERE_WANT_DISTINCT|WHERE_AGG_DISTINCT`); `fixDistinctOpenEph` afterwards | GROUP BY block, `fix_distinct_open_eph` | AGG `x[2]` (`eDist`), `x[3]` (`distFlag`) |
| tag-select-0810: order from the planner | `sqlite3WhereIsOrdered()==nGroupBy`: no sorter (`OP_SorterOpen` → `OP_Noop`), groups detected on the fly; otherwise `groupBySort`: EQP "USE TEMP B-TREE FOR GROUP BY" (or "DISTINCT" for the rewritten form), rows pushed as GROUP BY terms + the columns with `iSorterColumn>=nGroupBy` (`OP_MakeRecord` / `OP_SorterInsert`), second loop over `OP_SorterSort` … `OP_SorterNext` reading through the pseudo cursor (`useSortingIdx`) | GROUP BY block | AGG `x[0]` (`groupBySort`), `sortPTab`, `useSortIdx` |
| `optimizeAggregateUseOfIndexedExpr` / `aggregateConvertIndexedExprRefToColumn` | when `pParse->pIdxEpr` is set after `sqlite3WhereBegin`: `nColumn` is cut back to `nAccumulator`, `nSortingColumn` recomputed, the aggregate arguments analyzed again (now seeing the indexed expressions), and the tagged sub-expressions turned into `TK_AGG_COLUMN` on the index cursor | same names in snake case | AGG `nCol` / `nSortCol`, AGGCOL rows, `OP_Column` on the index cursor |
| group change | `OP_Compare` of the current GROUP BY values with the previous ones under the GROUP BY KeyInfo, `OP_Jump`; on a change: output subroutine, `OP_Move`, abort flag test, reset subroutine | GROUP BY block | listing |
| `iOrderByCol` terms | a GROUP BY term that names a result column makes that result expression read the group register (`sqlite3ExprToRegister`) unless it is an aggregate column or already a register | GROUP BY block | listing |
| `updateAccumulator` | FILTER: jump over the step when the filter is false / NULL; for a FILTERed min()/max() with accumulator columns and `regAcc`: `OP_Copy regAcc → regHit` first; ORDER BY aggregate: arguments (+ `OP_Sequence` unless unique, + payload, + subtype flags) into the `iOBTab` ephemeral index instead of `OP_AggStep`; DISTINCT: `codeDistinct` on `iDistinct`; `OP_CollSeq` for SQLITE_FUNC_NEEDCOLL; accumulator columns loaded once per group (`regHit` / `regAcc` test, `OP_If`) | `update_accumulator` | listing |
| `regAcc` | allocated only when there are accumulator columns and no min()/max() without FILTER | non-GROUP-BY block | AGG `x[1]` of kind `plain` |
| `finalizeAggFunctions` | ORDER BY aggregates: loop over `iOBTab` (`OP_Rewind` … `OP_Next`) calling `OP_AggStep`, then `OP_AggFinal`; others `OP_AggFinal` only | `finalize_agg_functions` | listing |
| aggregate sub-selects | an aggregate that belongs to an outer query level (`op2` depth) is an entry of the *outer* AggInfo; inside the sub-select it is read as a register | `analyze_aggregate` (`walker_depth`), resolver (`sqlite3ReferencesSrcList`) | corpus class "aggregate sub-selects" |
| errors | "misuse of aggregate function f()", "aggregate functions are not allowed in the GROUP BY clause", "HAVING clause on a non-aggregate query", "N-th GROUP BY term out of range", "DISTINCT aggregates must have exactly one argument", "ORDER BY may not be used with non-aggregate f()", "FILTER may not be used with non-aggregate f()" — same text, same error offset | resolver | error statements of the corpora |

### 2.10 Common table expressions (select.c: withExpand, generateWithRecursiveQuery, fromClauseTermCanBeCoroutine) — `select.rs`  *(layer: EXPLAIN / EQP bytes)*

| C | Decision | Rust | Verified by |
|---|---|---|---|
| `withExpand` / `searchWith` | a FROM name without schema is looked up in the WITH clauses from the innermost outward; the CTE body is copied (`sqlite3SelectDup`) for each reference; `CteUse` is shared by all references (`nUse++`) | `with_expand`, `search_with` | cte corpus |
| **`sqlite3SrcListDup`** | copying a FROM item that is a CTE counts as one more use (`pCteUse->nUse++`) — this is why a CTE referenced once inside an `IN (SELECT …)` (whose SELECT is copied) is materialized | `sqlite3_src_list_dup` (**fixed in V3**; V2 produced "CO-ROUTINE c" where C prints "MATERIALIZE c") | cte corpus classes "IN c", "IN (SELECT … FROM c)" |
| recursion detection | the right-most terms of a UNION / UNION ALL that reference the CTE itself are the recursive part (`SF_Recursive`, the FROM item `isRecursive`, cursor of the "current row" table); a second reference in one term: "multiple references to recursive table: %s"; reference from a sub-select: "recursive reference in a subquery: %s"; while the body is being expanded: "circular reference: %s" | `with_expand` | error classes of the cte corpus (273 circular, 70 multiple references per 2 400 statements) |
| column count | "table %s has %d values for %d columns" | `with_expand` | cte corpus |
| `fromClauseTermCanBeCoroutine` (2a)(2b) | `AS MATERIALIZED`: never a co-routine; used twice or more and not `AS NOT MATERIALIZED`: materialized once: the first use codes the subroutine with `OP_BeginSubrtn` and an `OP_Once` guard (no `OP_Once` when the CTE is correlated), later uses enter it with `OP_Gosub` and read the result through `OP_OpenDup` (tag-select-0484 / 0488) | `from_clause_term_can_be_coroutine`, `CteUse.addr_m9e/reg_rtn/i_cur` | EQP "MATERIALIZE" / "CO-ROUTINE", listings |
| flattening | a CTE with `eM10d==M10d_Yes` is not flattened; for recursive queries the port implements restrictions (22) (the subquery is part of a recursive CTE: `SF_Recursive` on the sub-select) and (23) (the outer query is recursive and the subquery is a compound); the other restrictions are those of V2's table | `flatten_subquery` | cte corpus |
| `multiSelect` | `SF_Recursive` with an anchor (`hasAnchor`) → `generateWithRecursiveQuery`; the recursive step itself (anchor detached) is coded as a UNION ALL of the recursive terms | `multi_select`, `has_anchor` | listings |
| `generateWithRecursiveQuery` | LIMIT / OFFSET registers taken from the compound and applied to the *output*; Queue cursor, plus a Distinct cursor (= Queue+1) for UNION; destination `SRT_Fifo` / `SRT_DistFifo` without ORDER BY, `SRT_Queue` / `SRT_DistQueue` with ORDER BY (queue key = ORDER BY terms + sequence, KeyInfo of `multiSelectByMergeKeyInfo(…,1)`); every recursive term is forced to `TK_ALL`; "recursive aggregate queries not supported"; EQP "SETUP" and "RECURSIVE STEP"; loop: `OP_Rewind` queue → `OP_NullRow` current → `OP_RowData` (or `OP_Column` nKey+1) → `OP_Delete` → output through `selectInnerLoop` → `OP_DecrJumpZero` limit → recursive step into the queue → `OP_Goto` | `generate_with_recursive_query` | recursive classes of the cte corpus (listing + EQP) |
| `selectInnerLoop` / `generateOutputSubroutine`: `SRT_Queue`, `SRT_DistQueue` | record of the row; DistQueue: `OP_Found` on the distinct index first and `OP_IdxInsert` with `OPFLAG_USESEEKRESULT`; key registers copied by `iOrderByCol`, `OP_Sequence`, `OP_MakeRecord`, `OP_IdxInsert` | `select_inner_loop`, `generate_output_subroutine` | cte_b corpus (UNION recursion with ORDER BY) |
| wherecode.c / where.c for the recursive table | the "current row" table is a pseudo cursor with one row: no `OP_Rewind` / `OP_Next` (`OP_Noop` loop), no automatic index | (V2 code) `wherecode.rs`, `where_.rs` | listings |

### 2.11 FILTER clause; window functions (window.c) — `window.rs`

| C | Decision | Rust | Verified by |
|---|---|---|---|
| `filter_over ::= filter_clause` | an aggregate with only a FILTER clause gets a `Window` object with `eFrmType=TK_FILTER`; `sqlite3WindowAttach` sets `EP_WinFunc`, `y.pWin`, `pOwner` | grammar rules 343, 346, 192–194; `sqlite3_window_attach` | AGGFUNC `filter=1` |
| `IsWindowFunc` | such a function is *not* a window function: the resolver treats it as an aggregate, walks the filter expression; "FILTER may not be used with non-aggregate f()" otherwise | `resolve_expr_step` | error statements |
| `walkWindowList`, `sqlite3WindowDup`, `sqlite3WindowCompare`, `substExpr` | the filter expression takes part in every tree walk, is copied with the expression, distinguishes two otherwise equal aggregate calls, and is substituted when a subquery is flattened | `walk_window_list`, `sqlite3_window_dup`, `sqlite3_window_compare`, `subst_expr` | agg corpus classes with FILTER in views / subqueries, AGGFUNC count |
| `OVER …`, `WINDOW` clause, window-only functions | **not ported**: grammar rules 318–342, 344, 345 and rule 93 report `window function` / `WINDOW clause` as unsupported; nothing of `sqlite3WindowRewrite` / `sqlite3WindowCodeStep` exists | `parse.rs` | 429 statements of all-kinds stay unsupported |

### 2.12 Virtual tables and table-valued functions in FROM (vtab.c, where.c `whereLoopAddVirtual`) — not ported

A `CREATE VIRTUAL TABLE` row of the schema loads as a `TABTYP_VTAB` stub; a statement that names it, an eponymous
virtual table or a table-valued function reports `virtual table` / `eponymous virtual table / table-valued function`
as unsupported. No `xBestIndex` host interface exists yet, so there is no xBestIndex call-sequence comparison in V3.

### 2.13 Connection state the listing shows — V3 additions to V2's table 2.8

| C | Rule | Engine / port | Verified by |
|---|---|---|---|
| `DBFLAG_EncodingFixed` (prepare.c:104, :225, :261) | set by the first `sqlite3InitCallback()` call that reads a real `sqlite_schema` row (the synthetic call of `sqlite3InitOne` is masked back), never cleared | `DatabaseSet::c_note_encoding_fixed` (a schema C has loaded — `c_loaded` — and that has an object), `Db.encoding_fixed` | `probe/encfixed.sh` 54/54 (r6: 36/54) |
| `cmd ::= select` (parse.y:519) | while the flag is clear, `sqlite3ReadSchema()` runs before `sqlite3Select()`; when it has to load a schema, `sqlite3Init()` clears the error offset of an error already raised in the statement (e.g. a syntax error inside the SELECT: no "error here" for the first SELECT of a connection) | rule 84 of `parse.rs`; `lib.rs prepare_in_session`: a statement the engine's parser rejects gets C's message and offset when the port's parser rejects it too | probes; `perr.py` all-kinds: 70 statements became identical, 0 different |
| `sqlite3Pragma` (pragma.c:521, :2267) | the pragmas with `PragFlg_NeedSchema` (21 names of `aPragmaName[]`) and the query form of `PRAGMA encoding` call `sqlite3ReadSchema()` | `engine.rs c_pragma_reads_schema`, `lib.rs c_reads_schema` → `c_mark_schemas_loaded` | probes `PRAGMA encoding;` / `cache_size` / `table_list` + `EXPLAIN SELECT #0` |
| `attachFunc` (attach.c) | ends with `sqlite3Init()`: every schema, the new one included, is loaded | `exec_attach_database` → `c_mark_schemas_loaded` | probe `ATTACH …; EXPLAIN SELECT #0` |
| `analysisLoader` under `SQLITE_ReverseOrder` | `sqlite3AnalysisLoad()` reads `sqlite_stat1` with an ORDER-BY-less SELECT, which `PRAGMA reverse_unordered_selects` reverses; the last row of a table sets `nRowLogEst` | `DbSlot::c_stat_reverse` (flag at C's last load: `c_mark_schemas_loaded`, ANALYZE, `c_reparse_schema`), `Host::stat1_rows` reverses | all-kinds #31205; probe z2.sql |
| `sqlite3VdbeDisplayP4` P4_MEM (vdbeaux.c:1979) | a text Mem is listed as `pMem->z`, a C string in the database encoding | `lib.rs c_list_rows` (UTF-16: bytes up to the first zero byte) | all-kinds #26467 |
| `resolveSelectStep` (resolve.c:1996–2056) | `sNC.pNext = pOuterNC` stays set for ORDER BY and GROUP BY: their terms may name columns of outer queries (an older release cleared it) | `resolve.rs` (line removed) | all-kinds #14288; 27 probes (EXPLAIN, EQP, run) |


## 3. What "supported" means in V3

Accepted in V3 in addition to V2 (compiled by the generator, listing and plan byte-identical to C): every schema the
reference shell can create from CREATE TABLE / INDEX / VIEW text (the schema is now built by C's own grammar actions
from `sqlite_schema`, so V2's unsupported schema classes are gone: VIRTUAL generated columns, explicit COLLATE BINARY,
UNIQUE before INTEGER PRIMARY KEY, D1 tables); GROUP BY / HAVING over tables (sorter and ordered-scan forms, indexed
expressions, DISTINCT and ORDER BY aggregates, FILTER); recursive common table expressions (queue / distinct queue,
ORDER BY / LIMIT / OFFSET of the recursion, MATERIALIZED hints).

Still reported as unsupported (`SQLITERS_CG_TRACE=1` prints the reason; the engine's old output is then printed):

| Reason reported | Why |
|---|---|
| `statement kind (grammar rule not ported)` | DML, DDL outside schema loading, PRAGMA, ATTACH, triggers, … (later steps) |
| `window function`, `WINDOW clause` | item 5 of V3, not ported (proposal: V4a) |
| `eponymous virtual table / table-valued function`, `virtual table` | item 6 of V3, not ported (proposal: V4b) |
| `WITH prefix of DML` | DML is not ported |
| `table whose removed sqlite_stat1 rows C still remembers`, `sqlite_stat1 was written and not re-read by ANALYZE` | C's in-memory statistics are not the engine's (D7) |
| `like() after PRAGMA case_sensitive_like` | the pragma re-registers like(); the engine does not implement it |
| `schema object that could not be loaded` | a sqlite_schema row whose text the port's grammar rejects (C would report a corrupt schema) |

## 4. C-side coverage of the functions new or changed in V3


Corpus: the 61000 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
|---|---|---|
| build.c | 771 / 944 (81.7%) | 531 / 724 (73.3%) |
| analyze.c | 70 / 74 (94.6%) | 46 / 58 (79.3%) |
| attach.c | 2 / 2 (100.0%) | 0 / 0 (0.0%) |
| resolve.c | 243 / 312 (77.9%) | 200 / 287 (69.7%) |
| expr.c | 527 / 676 (78.0%) | 370 / 492 (75.2%) |
| select.c | 979 / 1062 (92.2%) | 613 / 720 (85.1%) |
| where.c | 25 / 25 (100.0%) | 13 / 16 (81.2%) |
| func.c | 4 / 6 (66.7%) | 2 / 4 (50.0%) |
| malloc.c | 5 / 5 (100.0%) | 3 / 4 (75.0%) |
| prepare.c | 132 / 173 (76.3%) | 72 / 132 (54.5%) |
| update.c | 7 / 7 (100.0%) | 4 / 4 (100.0%) |
| util.c | 7 / 7 (100.0%) | 7 / 8 (87.5%) |
| vdbe.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| vdbemem.c | 82 / 91 (90.1%) | 67 / 84 (79.8%) |
| walker.c | 31 / 31 (100.0%) | 30 / 36 (83.3%) |
| window.c | 46 / 50 (92.0%) | 17 / 34 (50.0%) |
| **total** | 2934 / 3468 (84.6%) | 1975 / 2603 (75.9%) |


A branch of a ported C function that no accepted statement takes is **not verified**. `cov/untaken.txt` lists every
such source line (function by function); the per-function counts are in the last column of section 1. The largest
groups:


| C function | source lines with a never-taken branch |
|---|---|
| `sqlite3Select` | 50 |
| `sqlite3ExprCodeTarget` | 47 |
| `resolveExprStep` | 41 |
| `sqlite3CreateIndex` | 35 |
| `sqlite3InitCallback` | 23 |
| `sqlite3EndTable` | 20 |
| `sqlite3StartTable` | 18 |
| `resolveSelectStep` | 16 |
| `sqlite3InitOne` | 14 |
| `analyzeAggregate` | 13 |
| `valueFromExpr` | 13 |
| `sqlite3WindowCompare` | 10 |
| `sqlite3AddColumn` | 9 |
| `sqlite3CreateForeignKey` | 9 |
| `sqlite3CheckObjectName` | 8 |
| `sqlite3AddGenerated` | 8 |
| `sqlite3ExprCompare` | 8 |
| `sqlite3LocateTable` | 7 |
| `substExpr` | 7 |
| `selectInnerLoop` | 7 |
| `sqlite3AddPrimaryKey` | 6 |
| `sqlite3CreateView` | 6 |
| `sqlite3SrcListDup` | 6 |
| `generateWithRecursiveQuery` | 6 |
| `multiSelect` | 6 |
| `updateAccumulator` | 6 |
| `walkWindowList` | 6 |
| `decodeIntArray` | 5 |
| `findOrCreateAggInfoColumn` | 5 |
| `sqlite3ExprCodeGeneratedColumn` | 5 |

Known reasons for the never-taken branches: window-function branches (`pWin`, `IsWindowFunc`, `NC_AllowWin` errors:
not ported); virtual-table branches (`IsVirtual`: unsupported); OOM / `db->mallocFailed`; `SQLITE_ENABLE_STAT4` and
other compile options the reference build does not have; `init.busy==2` (ALTER TABLE ADD COLUMN re-parse); corrupt
schema branches of `sqlite3InitCallback` (the corpora load only valid schemas); `IN_RENAME_OBJECT` branches (ALTER
TABLE RENAME is not ported); error branches for limits (`SQLITE_MAX_COLUMN`, too many terms).

## 5. Decisions of C that V3 does not reproduce, and remaining differences

1. Everything in the table of section 3 (reported as unsupported).
2. V2's items 2 and 4 (schema-reset sources not tracked for `iGeneration`; `nTabRef`). V2's item 3 (schema-loaded
   state) is now C's: `DBFLAG_EncodingFixed`, the pragmas that read the schema and ATTACH are mirrored (table 2.13).
3. Remaining differences of the all-kinds corpus: `res/all_kinds_bugs_<tag>.json`, classified in `PROGRESS.md`; each class
   is either a function / module / collation only the reference shell has (D4, D5) or a cascade of an engine
   difference recorded in `DIFFS_OUTSIDE_TASK.md` (attributed statement by statement with `firstdiv.py`).
4. Engine differences found during V3 and not fixed (engine scope): D14–D17, D20–D26 in `DIFFS_OUTSIDE_TASK.md`;
   **D23 is a wrong-result defect** (`0.5 IS TRUE`).
5. C's schema-loaded state after a failing *first* statement of a kind the generator does not port (e.g.
   `CREATE TABLE x(;`): whether C's parse reached `sqlite3ReadSchema()` is not known, so the error offset of a
   following first-SELECT syntax error can differ (2 of the 68 probes of `probe/encfixed.sh`).
6. Not tracked schema-reset sources of V2 item 2 remain, plus D27 (statements C rejects only in OP_ParseSchema) and
   D29 (temp schema cookie after ALTER TABLE RENAME): they change P3 / P4 of `OP_Transaction` in later listings.

