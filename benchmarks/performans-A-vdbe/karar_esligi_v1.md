# V1 decision parity (karar eşliği): C code generator → Rust port

Scope: step V1 — program representation, EXPLAIN listing, parse tree, name resolution, expression code generation,
statement shell, FROM-less SELECT (incl. compound, VALUES, sub-selects, simple aggregates) and transaction control.
Oracle: C SQLite 3.54.1 (`target/perf-work/bin/sqlite3`, amalgamation `target/sqlite-ref/amalg`).
Worktree: `/home/hakan/github/sqliters/.claude/worktrees/vdbe` (branch `performans-A-vdbe`, uncommitted).

How a decision is proven here. The program C generates *is* the trace of its code-generation decisions (which opcode,
which register, which jump target, which P4/P5). For every statement of the corpus the port either reports
"unsupported" (first missing piece; the engine then keeps its previous EXPLAIN output) or must produce the byte-identical
listing or the identical error message + error offset. Three independent comparisons are run:

| Comparison | Tool | Corpus | Result |
|---|---|---|---|
| library level, no engine: port (`cgexplain`) vs C library (`cdump/cexplain`), listing bytes and `ERR <offset> <msg>` | `cgdiff.py` | 54 613 statements (`corpus/g*_*.sql`, `limits.sql`, `smoke1.sql`) | 54 469 accepted, 54 469 identical, 0 different, 144 unsupported |
| CLI level: `sqliters` shell vs `sqlite3` shell, stdout segment + "Parse error" text | `expdiff.py` | same 54 613 | 53 849 accepted (634 swallowed by the shells' statement splitter, same in both), 53 847 identical, 2 different (both: `regexp()` exists only in the C shell) |
| parse tree: dump after parse.y (P) and after `sqlite3SelectPrep` (R), instrumented scratch amalgamation vs `treedump.rs` | `treediff.py` | accepted statements with a SELECT | P 51 482 / 51 482 equal, R 41 818 / 41 818 equal |
| C-side branch coverage of the ported functions by the accepted statements (gcov) | `covrun.py` | 54 469 accepted | lines 5466 / 7845 (69.7 %), branches taken 3194 / 5360 (59.6 %) — section 4 |

A branch of a ported C function that the corpus never takes is **not verified**: section 4 lists them per function
(`cov/untaken.txt` has every source line). Whole functions that are deliberately outside V1 report "unsupported" in the
port instead of approximating; their branches are in the same list and are marked in section 5.

## 1. C function → Rust item
Paths are under `crates/`. "cov" = C lines executed % / branches taken at least once % by the accepted corpus.


### vdbeaux.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3VdbeAddOp0` | `Vdbe::add_op0` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddOp1` | `Vdbe::add_op1` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddOp2` | `Vdbe::add_op2` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddOp3` | `Vdbe::add_op3` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddOp4Int` | `Vdbe::add_op4_int` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddInt64` | `Vdbe::add_int64` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddDouble` | `Vdbe::add_double` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeGoto` | `Vdbe::goto` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeLoadString` | `Vdbe::load_string` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddOp4` | `Vdbe::add_op4` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeAddFunctionCall` | `Vdbe::add_function_call` | `sqliters-vdbe/src/vdbeaux.rs` | 87.5 / 50.0 |
| `sqlite3VdbeEndCoroutine` | `Vdbe::end_coroutine` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeMakeLabel` | `Vdbe::make_label` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeResolveLabel` | `Vdbe::resolve_label` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `resolveP2Values` | `Vdbe::resolve_p2_values` | `sqliters-vdbe/src/vdbeaux.rs` | 72.2 / 63.2 |
| `sqlite3VdbeCurrentAddr` | `Vdbe::current_addr` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeChangeOpcode` | `Vdbe::change_opcode` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeChangeP2` | `Vdbe::change_p2` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeChangeP5` | `Vdbe::change_p5` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 50.0 |
| `sqlite3VdbeJumpHere` | `Vdbe::jump_here` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeJumpHereOrPopInst` | `Vdbe::jump_here_or_pop_inst` | `sqliters-vdbe/src/vdbeaux.rs` | 0.0 / 0.0 |
| `sqlite3VdbeChangeToNoop` | `Vdbe::change_to_noop` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 50.0 |
| `sqlite3VdbeChangeP4` | `Vdbe::change_p4` | `sqliters-vdbe/src/vdbeaux.rs` | 88.9 / 75.0 |
| `sqlite3VdbeAppendP4` | `Vdbe::append_p4` | `sqliters-vdbe/src/vdbeaux.rs` | 85.7 / 50.0 |
| `sqlite3VdbeGetOp` | `Vdbe::get_op` | `sqliters-vdbe/src/vdbeaux.rs` | 75.0 / 50.0 |
| `sqlite3VdbeGetLastOp` | `Vdbe::get_last_op` | `sqliters-vdbe/src/vdbeaux.rs` | 100.0 / 100.0 |
| `sqlite3VdbeDisplayP4` | `sqlite3_vdbe_display_p4` | `sqliters-vdbe/src/vdbeaux.rs` | 64.9 / 61.5 |
| `sqlite3VdbeUsesBtree` | `Vdbe::uses_btree` | `sqliters-vdbe/src/vdbeaux.rs` | 80.0 / 75.0 |
| `sqlite3VdbeNextOpcode` | `Program::list` | `sqliters-vdbe/src/vdbeaux.rs` | 51.1 / 28.1 |
| `sqlite3VdbeList` | `Program::list` | `sqliters-vdbe/src/vdbeaux.rs` | 70.7 / 50.0 |
| `sqlite3VdbeMakeReady` | `Vdbe::make_ready` | `sqliters-vdbe/src/vdbeaux.rs` | 93.3 / 72.2 |

### tokenize.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3GetToken` | `sqlite3_get_token` | `sqliters-codegen/src/tokenize.rs` | 94.9 / 87.8 |
| `getToken` | `get_token` | `sqliters-codegen/src/tokenize.rs` | 0.0 / 0.0 |
| `analyzeWindowKeyword` | `analyze_window_keyword` | `sqliters-codegen/src/tokenize.rs` | 0.0 / 0.0 |
| `analyzeOverKeyword` | `analyze_over_keyword` | `sqliters-codegen/src/tokenize.rs` | 0.0 / 0.0 |
| `analyzeFilterKeyword` | `analyze_filter_keyword` | `sqliters-codegen/src/tokenize.rs` | 0.0 / 0.0 |
| `sqlite3RunParser` | `sqlite3_run_parser` | `sqliters-codegen/src/tokenize.rs` | 80.0 / 61.1 |

### parse.y
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `yy_find_shift_action` | `yy_find_shift_action` | `sqliters-codegen/src/parse.rs` | 92.9 / 70.0 |
| `yy_find_reduce_action` | `yy_find_reduce_action` | `sqliters-codegen/src/parse.rs` | 100.0 / 100.0 |
| `yy_shift` | `YyParser::yy_shift` | `sqliters-codegen/src/parse.rs` | 100.0 / 100.0 |
| `yy_reduce` | `YyParser::yy_reduce` | `sqliters-codegen/src/parse.rs` | 40.9 / 39.0 |
| `sqlite3Parser` | `YyParser::sqlite3_parser` | `sqliters-codegen/src/parse.rs` | 76.9 / 66.7 |
| `tokenExpr` | `token_expr` | `sqliters-codegen/src/parse.rs` | 95.7 / 66.7 |
| `sqlite3PExprIsNull` | `sqlite3_p_expr_is_null` | `sqliters-codegen/src/parse.rs` | 100.0 / 100.0 |
| `sqlite3PExprIs` | `sqlite3_p_expr_is` | `sqliters-codegen/src/parse.rs` | 100.0 / 83.3 |
| `parserDoubleLinkSelect` | `parser_double_link_select` | `sqliters-codegen/src/parse.rs` | 100.0 / 93.8 |
| `attachWithToSelect` | `attach_with_to_select` | `sqliters-codegen/src/parse.rs` | 83.3 / 50.0 |
| `parserAddExprIdListTerm` | `parser_add_expr_id_list_term` | `sqliters-codegen/src/parse.rs` | 71.4 / 33.3 |

### util.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3ErrorMsg` | `Parse::error_msg` | `sqliters-codegen/src/sqliteint.rs` | 88.9 / 83.3 |
| `sqlite3Dequote` | `sqlite3_dequote` | `sqliters-codegen/src/util.rs` | 100.0 / 90.0 |
| `sqlite3DequoteExpr` | `token_expr` | `sqliters-codegen/src/parse.rs` | 100.0 / 100.0 |
| `sqlite3DequoteNumber` | `sqlite3_dequote_number` | `sqliters-codegen/src/expr.rs` | 100.0 / 80.6 |
| `sqlite3GetInt32` | `sqlite3_get_int32` | `sqliters-codegen/src/util.rs` | 87.5 / 81.6 |
| `sqlite3DecOrHexToI64` | `sqlite3_dec_or_hex_to_i64` | `sqliters-codegen/src/util.rs` | 100.0 / 62.5 |
| `sqlite3HexToBlob` | `sqlite3_hex_to_blob` | `sqliters-codegen/src/util.rs` | 100.0 / 75.0 |

### build.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3AffinityType` | `sqlite3_affinity_type` | `sqliters-codegen/src/util.rs` | 73.3 / 57.1 |
| `sqlite3FinishCoding` | `sqlite3_finish_coding` | `sqliters-codegen/src/build.rs` | 69.1 / 57.1 |
| `sqlite3BeginTransaction` | `sqlite3_begin_transaction` | `sqliters-codegen/src/build.rs` | 88.2 / 78.6 |
| `sqlite3EndTransaction` | `sqlite3_end_transaction` | `sqliters-codegen/src/build.rs` | 85.7 / 66.7 |
| `sqlite3Savepoint` | `sqlite3_savepoint` | `sqliters-codegen/src/build.rs` | 75.0 / 50.0 |
| `sqlite3CteNew` | `yy_reduce(rule 314)` | `sqliters-codegen/src/parse.rs` | 81.8 / 50.0 |
| `sqlite3WithAdd` | `sqlite3_with_add` | `sqliters-codegen/src/select.rs` | 83.3 / 71.4 |

### printf.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3RecordErrorOffsetOfExpr` | `Parse::record_error_offset_of_expr` | `sqliters-codegen/src/sqliteint.rs` | 100.0 / 75.0 |

### walker.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3WalkExprNN` | `sqlite3_walk_expr_nn` | `sqliters-codegen/src/walker.rs` | 94.1 / 86.4 |
| `sqlite3WalkExpr` | `sqlite3_walk_expr` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 |
| `sqlite3WalkExprList` | `sqlite3_walk_expr_list` | `sqliters-codegen/src/walker.rs` | 100.0 / 100.0 |
| `sqlite3WalkSelectExpr` | `sqlite3_walk_select_expr` | `sqliters-codegen/src/walker.rs` | 64.3 / 31.8 |
| `sqlite3WalkSelectFrom` | `sqlite3_walk_select_from` | `sqliters-codegen/src/walker.rs` | 72.7 / 58.3 |
| `sqlite3WalkSelect` | `sqlite3_walk_select` | `sqliters-codegen/src/walker.rs` | 92.3 / 71.4 |

### callback.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3GetCollSeq` | `sqlite3_get_coll_seq` | `sqliters-codegen/src/callback.rs` | 92.9 / 64.3 |
| `sqlite3FindCollSeq` | `sqlite3_find_coll_seq` | `sqliters-codegen/src/callback.rs` | 100.0 / 100.0 |
| `matchQuality` | `match_quality` | `sqliters-codegen/src/callback.rs` | 84.6 / 77.8 |
| `sqlite3FindFunction` | `sqlite3_find_function` | `sqliters-codegen/src/callback.rs` | 91.9 / 83.3 |

### resolve.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `incrAggFunctionDepth` | `incr_agg_function_depth` | `sqliters-codegen/src/resolve.rs` | 42.9 / 50.0 |
| `resolveAlias` | `resolve_alias` | `sqliters-codegen/src/resolve.rs` | 77.8 / 50.0 |
| `areDoubleQuotedStringsEnabled` | `are_double_quoted_strings_enabled` | `sqliters-codegen/src/resolve.rs` | 57.1 / 25.0 |
| `lookupName` | `lookup_name` | `sqliters-codegen/src/resolve.rs` | 44.7 / 36.7 |
| `notValidImpl` | `not_valid_impl` | `sqliters-codegen/src/resolve.rs` | 0.0 / 0.0 |
| `exprProbability` | `expr_probability` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 |
| `resolveSetExprSubtypeArg` | `resolve_set_expr_subtype_arg` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 |
| `resolveExprStep` | `resolve_expr_step` | `sqliters-codegen/src/resolve.rs` | 79.7 / 69.7 |
| `resolveAsName` | `resolve_as_name` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 |
| `resolveOrderByTermToExprList` | `resolve_order_by_term_to_expr_list` | `sqliters-codegen/src/resolve.rs` | 77.8 / 16.7 |
| `resolveOutOfRangeError` | `resolve_out_of_range_error` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 |
| `resolveCompoundOrderBy` | `resolve_compound_order_by` | `sqliters-codegen/src/resolve.rs` | 94.3 / 73.9 |
| `sqlite3ResolveOrderGroupBy` | `sqlite3_resolve_order_group_by` | `sqliters-codegen/src/resolve.rs` | 100.0 / 78.6 |
| `resolveOrderGroupBy` | `resolve_order_group_by` | `sqliters-codegen/src/resolve.rs` | 100.0 / 95.0 |
| `resolveSelectStep` | `resolve_select_step` | `sqliters-codegen/src/resolve.rs` | 74.2 / 67.4 |
| `sqlite3ResolveExprNames` | `sqlite3_resolve_expr_names` | `sqliters-codegen/src/resolve.rs` | 94.1 / 90.0 |
| `sqlite3ResolveExprListNames` | `sqlite3_resolve_expr_list_names` | `sqliters-codegen/src/resolve.rs` | 96.0 / 75.0 |
| `sqlite3ResolveSelectNames` | `sqlite3_resolve_select_names` | `sqliters-codegen/src/resolve.rs` | 100.0 / 100.0 |

### expr.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3ExprAffinity` | `sqlite3_expr_affinity` | `sqliters-codegen/src/expr.rs` | 100.0 / 79.2 |
| `sqlite3ExprDataType` | `sqlite3_expr_data_type` | `sqliters-codegen/src/expr.rs` | 0.0 / 0.0 |
| `sqlite3ExprAddCollateToken` | `sqlite3_expr_add_collate_token` | `sqliters-codegen/src/expr.rs` | 100.0 / 50.0 |
| `sqlite3ExprAddCollateString` | `sqlite3_expr_add_collate_string` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprSkipCollate` | `sqlite3_expr_skip_collate` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprSkipCollateAndLikely` | `sqlite3_expr_skip_collate_and_likely` | `sqliters-codegen/src/expr.rs` | 87.5 / 87.5 |
| `sqlite3ExprCollSeq` | `sqlite3_expr_coll_seq` | `sqliters-codegen/src/expr.rs` | 97.3 / 77.3 |
| `sqlite3ExprNNCollSeq` | `sqlite3_expr_nn_coll_seq` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3CompareAffinity` | `sqlite3_compare_affinity` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `comparisonAffinity` | `comparison_affinity` | `sqliters-codegen/src/expr.rs` | 0.0 / 0.0 |
| `binaryCompareP5` | `binary_compare_p5` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3BinaryCompareCollSeq` | `sqlite3_binary_compare_coll_seq` | `sqliters-codegen/src/expr.rs` | 100.0 / 75.0 |
| `codeCompare` | `code_compare` | `sqliters-codegen/src/exprcode.rs` | 88.9 / 75.0 |
| `sqlite3ExprIsVector` | `sqlite3_expr_is_vector` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprVectorSize` | `sqlite3_expr_vector_size` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3VectorFieldSubexpr` | `sqlite3_vector_field_subexpr` | `sqliters-codegen/src/expr.rs` | 83.3 / 66.7 |
| `sqlite3ExprForVectorField` | `sqlite3_expr_for_vector_field` | `sqliters-codegen/src/expr.rs` | 87.5 / 62.5 |
| `exprCodeSubselect` | `expr_code_subselect` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `exprVectorRegister` | `expr_vector_register` | `sqliters-codegen/src/exprcode.rs` | 82.3 / 66.7 |
| `codeVectorCompare` | `code_vector_compare` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprCheckHeight` | `sqlite3_expr_check_height` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `heightOfSelect` | `height_of_select` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `exprSetHeight` | `expr_set_height` | `sqliters-codegen/src/expr.rs` | 90.9 / 70.0 |
| `sqlite3ExprSetHeightAndFlags` | `sqlite3_expr_set_height_and_flags` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprAlloc` | `sqlite3_expr_alloc` | `sqliters-codegen/src/expr.rs` | 93.3 / 83.3 |
| `sqlite3ExprInt32` | `sqlite3_expr_int32` | `sqliters-codegen/src/expr.rs` | 100.0 / 75.0 |
| `sqlite3ExprAttachSubtrees` | `sqlite3_expr_attach_subtrees` | `sqliters-codegen/src/expr.rs` | 86.7 / 87.5 |
| `sqlite3PExpr` | `sqlite3_p_expr` | `sqliters-codegen/src/expr.rs` | 81.8 / 50.0 |
| `sqlite3PExprAddSelect` | `sqlite3_p_expr_add_select` | `sqliters-codegen/src/expr.rs` | 85.7 / 50.0 |
| `sqlite3ExprListToValues` | `sqlite3_expr_list_to_values` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 |
| `sqlite3ExprAnd` | `sqlite3_expr_and` | `sqliters-codegen/src/expr.rs` | 92.3 / 75.0 |
| `sqlite3ExprFunction` | `sqlite3_expr_function` | `sqliters-codegen/src/expr.rs` | 87.5 / 80.0 |
| `sqlite3ExprAddFunctionOrderBy` | `sqlite3_expr_add_function_order_by` | `sqliters-codegen/src/expr.rs` | 48.0 / 43.8 |
| `sqlite3ExprFunctionUsable` | `sqlite3_expr_function_usable` | `sqliters-codegen/src/expr.rs` | 57.1 / 25.0 |
| `sqlite3ExprAssignVarNumber` | `sqlite3_expr_assign_var_number` | `sqliters-codegen/src/expr.rs` | 100.0 / 86.7 |
| `exprDup` | `expr_dup` | `sqliters-codegen/src/expr.rs` | 73.7 / 54.8 |
| `sqlite3ExprListDup` | `sqlite3_expr_list_dup` | `sqliters-codegen/src/expr.rs` | 65.5 / 43.8 |
| `sqlite3SrcListDup` | `sqlite3_src_list_dup` | `sqliters-codegen/src/expr.rs` | 72.1 / 50.0 |
| `sqlite3SelectDup` | `sqlite3_select_dup` | `sqliters-codegen/src/expr.rs` | 91.2 / 50.0 |
| `sqlite3WithDup` | `sqlite3_with_dup` | `sqliters-codegen/src/expr.rs` | 100.0 / 83.3 |
| `sqlite3ExprListAppend` | `sqlite3_expr_list_append` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprListSetSortOrder` | `sqlite3_expr_list_set_sort_order` | `sqliters-codegen/src/expr.rs` | 100.0 / 87.5 |
| `sqlite3ExprListSetName` | `sqlite3_expr_list_set_name` | `sqliters-codegen/src/expr.rs` | 88.9 / 50.0 |
| `sqlite3ExprListSetSpan` | `sqlite3_expr_list_set_span` | `sqliters-codegen/src/expr.rs` | 100.0 / 75.0 |
| `sqlite3ExprListFlags` | `sqlite3_expr_list_flags` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3IsTrueOrFalse` | `sqlite3_is_true_or_false` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprIdToTrueFalse` | `sqlite3_expr_id_to_true_false` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprTruthValue` | `sqlite3_expr_truth_value` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprSimplifiedAndOr` | `sqlite3_expr_simplified_and_or` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `exprEvalRhsFirst` | `expr_eval_rhs_first` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `exprComputeOperands` | `expr_compute_operands` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `exprNodeIsConstantFunction` | `expr_node_is_constant_function` | `sqliters-codegen/src/expr.rs` | 100.0 / 78.6 |
| `exprNodeIsConstant` | `expr_node_is_constant` | `sqliters-codegen/src/expr.rs` | 71.8 / 64.1 |
| `exprIsConst` | `expr_is_const` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprListIsConstant` | `sqlite3_expr_list_is_constant` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `exprNodeIsConstantOrGroupBy` | `expr_node_is_constant_or_group_by` | `sqliters-codegen/src/expr.rs` | 100.0 / 91.7 |
| `sqlite3ExprIsConstantOrGroupBy` | `sqlite3_expr_is_constant_or_group_by` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprIsInteger` | `sqlite3_expr_is_integer` | `sqliters-codegen/src/expr.rs` | 80.7 / 58.3 |
| `sqlite3ExprCanBeNull` | `sqlite3_expr_can_be_null` | `sqliters-codegen/src/expr.rs` | 100.0 / 66.7 |
| `isCandidateForInOpt` | `is_candidate_for_in_opt` | `sqliters-codegen/src/exprcode.rs` | 63.2 / 63.6 |
| `sqlite3SetHasNullFlag` | `sqlite3_set_has_null_flag` | `sqliters-codegen/src/exprcode.rs` | 90.0 / 50.0 |
| `sqlite3InRhsIsConstant` | `sqlite3_in_rhs_is_constant` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3FindInIndex` | `sqlite3_find_in_index` | `sqliters-codegen/src/exprcode.rs` | 35.9 / 34.3 |
| `exprINAffinity` | `expr_in_affinity` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 87.5 |
| `sqlite3SubselectError` | `sqlite3_subselect_error` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 50.0 |
| `sqlite3VectorErrorMsg` | `sqlite3_vector_error_msg` | `sqliters-codegen/src/exprcode.rs` | 0.0 / 0.0 |
| `findCompatibleInRhsSubrtn` | `find_compatible_in_rhs_subrtn` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 92.9 |
| `sqlite3CodeRhsOfIN` | `sqlite3_code_rhs_of_in` | `sqliters-codegen/src/exprcode.rs` | 96.2 / 73.5 |
| `sqlite3CodeSubselect` | `sqlite3_code_subselect` | `sqliters-codegen/src/exprcode.rs` | 89.5 / 75.0 |
| `sqlite3ExprCheckIN` | `sqlite3_expr_check_in` | `sqliters-codegen/src/exprcode.rs` | 80.0 / 75.0 |
| `sqlite3ExprCodeIN` | `sqlite3_expr_code_in` | `sqliters-codegen/src/exprcode.rs` | 93.5 / 82.9 |
| `codeReal` | `code_real` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 75.0 |
| `codeInteger` | `code_integer` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprCodeGetColumnOfTable` | `sqlite3_expr_code_get_column_of_table` | `sqliters-codegen/src/exprcode.rs` | 34.8 / 41.7 |
| `sqlite3ExprCodeGetColumn` | `sqlite3_expr_code_get_column` | `sqliters-codegen/src/exprcode.rs` | 57.1 / 16.7 |
| `sqlite3ExprCodeMove` | `sqlite3_expr_code_move` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprToRegister` | `sqlite3_expr_to_register` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 50.0 |
| `exprCodeVector` | `expr_code_vector` | `sqliters-codegen/src/exprcode.rs` | 91.7 / 83.3 |
| `setDoNotMergeFlagOnCopy` | `set_do_not_merge_flag_on_copy` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `exprCodeInlineFunction` | `expr_code_inline_function` | `sqliters-codegen/src/exprcode.rs` | 52.4 / 38.5 |
| `exprNodeCanReturnSubtype` | `expr_node_can_return_subtype` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprCanReturnSubtype` | `sqlite3_expr_can_return_subtype` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `exprCodeTargetAndOr` | `expr_code_target_and_or` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprCodeTarget` | `sqlite3_expr_code_target` | `sqliters-codegen/src/exprcode.rs` | 71.8 / 61.8 |
| `sqlite3ExprCodeRunJustOnce` | `sqlite3_expr_code_run_just_once` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 86.4 |
| `sqlite3ExprNullRegisterRange` | `sqlite3_expr_null_register_range` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 100.0 |
| `sqlite3ExprCodeTemp` | `sqlite3_expr_code_temp` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 90.0 |
| `sqlite3ExprCode` | `sqlite3_expr_code` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 70.0 |
| `sqlite3ExprCodeCopy` | `sqlite3_expr_code_copy` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 50.0 |
| `sqlite3ExprCodeFactorable` | `sqlite3_expr_code_factorable` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 75.0 |
| `sqlite3ExprCodeExprList` | `sqlite3_expr_code_expr_list` | `sqliters-codegen/src/exprcode.rs` | 84.0 / 75.0 |
| `exprCodeBetween` | `expr_code_between` | `sqliters-codegen/src/exprcode.rs` | 100.0 / 75.0 |
| `sqlite3ExprIfTrue` | `sqlite3_expr_if_true` | `sqliters-codegen/src/exprcode.rs` | 96.1 / 80.0 |
| `sqlite3ExprIfFalse` | `sqlite3_expr_if_false` | `sqliters-codegen/src/exprcode.rs` | 94.8 / 93.3 |
| `sqlite3ExprCompare` | `sqlite3_expr_compare` | `sqliters-codegen/src/expr.rs` | 86.3 / 71.9 |
| `sqlite3ExprListCompare` | `sqlite3_expr_list_compare` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ReferencesSrcList` | `sqlite3_references_src_list` | `sqliters-codegen/src/expr.rs` | 85.0 / 70.0 |
| `findOrCreateAggInfoColumn` | `find_or_create_agg_info_column` | `sqliters-codegen/src/expr.rs` | 0.0 / 0.0 |
| `analyzeAggregate` | `analyze_aggregate` | `sqliters-codegen/src/expr.rs` | 65.5 / 50.7 |
| `sqlite3ExprAnalyzeAggregates` | `sqlite3_expr_analyze_aggregates` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ExprAnalyzeAggList` | `sqlite3_expr_analyze_agg_list` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3GetTempReg` | `sqlite3_get_temp_reg` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ReleaseTempReg` | `sqlite3_release_temp_reg` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3GetTempRange` | `sqlite3_get_temp_range` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ReleaseTempRange` | `sqlite3_release_temp_range` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |
| `sqlite3ClearTempRegCache` | `sqlite3_clear_temp_reg_cache` | `sqliters-codegen/src/expr.rs` | 100.0 / 100.0 |

### select.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3SelectNew` | `sqlite3_select_new` | `sqliters-codegen/src/select.rs` | 89.7 / 75.0 |
| `sqlite3KeyInfoFromExprList` | `sqlite3_key_info_from_expr_list` | `sqliters-codegen/src/select.rs` | 100.0 / 75.0 |
| `sqlite3SelectOpName` | `sqlite3_select_op_name` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `sqlite3GetVdbe` | `sqlite3_get_vdbe` | `sqliters-codegen/src/select.rs` | 100.0 / 75.0 |
| `codeOffset` | `code_offset` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `codeDistinct` | `code_distinct` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `fixDistinctOpenEph` | `fix_distinct_open_eph` | `sqliters-codegen/src/select.rs` | 92.3 / 80.0 |
| `selectInnerLoop` | `select_inner_loop` | `sqliters-codegen/src/select.rs` | 46.4 / 46.9 |
| `computeLimitRegisters` | `compute_limit_registers` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `multiSelectCollSeq` | `multi_select_coll_seq` | `sqliters-codegen/src/merge.rs` | 100.0 / 83.3 |
| `multiSelectByMergeKeyInfo` | `multi_select_by_merge_key_info` | `sqliters-codegen/src/merge.rs` | 100.0 / 80.0 |
| `multiSelectValues` | `multi_select_values` | `sqliters-codegen/src/select.rs` | 100.0 / 90.0 |
| `unionWithLimitOne` | `union_with_limit_one` | `sqliters-codegen/src/select.rs` | 100.0 / 91.7 |
| `multiSelect` | `multi_select` | `sqliters-codegen/src/select.rs` | 92.3 / 82.5 |
| `sqlite3SelectWrongNumTermsError` | `sqlite3_select_wrong_num_terms_error` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `generateOutputSubroutine` | `generate_output_subroutine` | `sqliters-codegen/src/merge.rs` | 53.8 / 50.0 |
| `multiSelectByMerge` | `multi_select_by_merge` | `sqliters-codegen/src/merge.rs` | 99.3 / 89.7 |
| `minMaxQuery` | `min_max_query` | `sqliters-codegen/src/select.rs` | 100.0 / 81.2 |
| `convertCompoundSelectToSubquery` | `convert_compound_select_to_subquery` | `sqliters-codegen/src/select.rs` | 27.8 / 58.3 |
| `sqlite3WithPush` | `sqlite3_with_push` | `sqliters-codegen/src/select.rs` | 77.8 / 50.0 |
| `sqlite3SelectPopWith` | `sqlite3_select_pop_with` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `sqlite3ExpandSubquery` | `sqlite3_expand_subquery` | `sqliters-codegen/src/select.rs` | 93.3 / 50.0 |
| `selectExpander` | `select_expander` | `sqliters-codegen/src/select.rs` | 52.5 / 37.5 |
| `sqlite3SelectExpand` | `sqlite3_select_expand` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `selectAddSubqueryTypeInfo` | `sqlite3_select_add_type_info` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 |
| `sqlite3SelectPrep` | `sqlite3_select_prep` | `sqliters-codegen/src/select.rs` | 100.0 / 87.5 |
| `analyzeAggFuncArgs` | `analyze_agg_func_args` | `sqliters-codegen/src/select.rs` | 90.9 / 83.3 |
| `assignAggregateRegisters` | `assign_aggregate_registers` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `resetAccumulator` | `reset_accumulator` | `sqliters-codegen/src/select.rs` | 93.5 / 70.8 |
| `finalizeAggFunctions` | `finalize_agg_functions` | `sqliters-codegen/src/select.rs` | 96.7 / 72.7 |
| `updateAccumulator` | `update_accumulator` | `sqliters-codegen/src/select.rs` | 85.1 / 61.4 |
| `havingToWhereExprCb` | `having_to_where` | `sqliters-codegen/src/select.rs` | 100.0 / 70.0 |
| `havingToWhere` | `having_to_where` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `sqlite3CopySortOrder` | `sqlite3_copy_sort_order` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `sqlite3Select` | `sqlite3_select` | `sqliters-codegen/src/select.rs` | 54.1 / 42.1 |

### insert.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3MultiValuesEnd` | `sqlite3_multi_values_end` | `sqliters-codegen/src/select.rs` | 100.0 / 66.7 |
| `exprListIsNoAffinity` | `expr_list_is_no_affinity` | `sqliters-codegen/src/select.rs` | 100.0 / 100.0 |
| `sqlite3MultiValues` | `sqlite3_multi_values` | `sqliters-codegen/src/select.rs` | 98.4 / 81.2 |

### where.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3WhereBegin` | `sqlite3_where_begin (zero tables / one co-routine only)` | `sqliters-codegen/src/where_.rs` | 54.0 / 38.3 |
| `sqlite3WhereEnd` | `sqlite3_where_end (zero tables / one co-routine only)` | `sqliters-codegen/src/where_.rs` | 27.4 / 18.0 |
| `translateColumnToCopy` | `sqlite3_where_end` | `sqliters-codegen/src/where_.rs` | 87.5 / 80.0 |

### whereexpr.c
| C function | Rust item | file | cov (lines / branches) |
|---|---|---|---|
| `sqlite3WhereSplit` | `sqlite3_where_split` | `sqliters-codegen/src/where_.rs` | 100.0 / 100.0 |
| `exprAnalyze` | `expr_analyze (zero-table effects only)` | `sqliters-codegen/src/where_.rs` | 64.3 / 64.7 |

## 2. Decision tables (C source line → rule → Rust)

All line numbers are `sqlite/src/<file>` of 3.54.1. "Verified" = exercised by the corpus with byte-identical EXPLAIN
(cgdiff/expdiff) — the coverage table in section 4 lists, per C function, the branches the corpus never took.

### 2.1 Register / cursor / label allocation order (visible as register numbers, cursor numbers, jump targets)
| Decision | C | Rule | Rust |
|---|---|---|---|
| temp register reuse | expr.c:7719 `sqlite3GetTempReg`, 7730 `sqlite3ReleaseTempReg` | stack `aTempReg[8]`; pop the last released, else `++nMem`; release pushes if fewer than 8 | `expr.rs: sqlite3_get_temp_reg / sqlite3_release_temp_reg` |
| temp range | expr.c:7749/7763 | `nReg==1` → single-reg pool; reuse `iRangeReg` block if `nReg<=nRangeReg` else `nMem+1..`; release keeps the largest block | `sqlite3_get_temp_range / sqlite3_release_temp_range` |
| pool reset | expr.c:7783, vdbeaux.c:580 | after `OP_Return` of a subroutine and after `OP_EndCoroutine` both pools are emptied | `sqlite3_clear_temp_reg_cache`, callers of `Vdbe::end_coroutine` |
| labels | vdbeaux.c:618/645/880 | labels are negative numbers; P2 of jump opcodes ≤ `SQLITE_MX_JUMP_OPCODE` resolved from the end towards `OP_Init`; `OP_Init.p2` is set by `sqlite3VdbeJumpHere(v,0)` | `Vdbe::make_label / resolve_label / resolve_p2_values` |
| cursors | `pParse->nTab++` at: IN RHS (`sqlite3FindInIndex` first thing, given back for IN_INDEX_NOOP), sorter index, DISTINCT table, aggregate DISTINCT / ORDER BY tables, GROUP BY sorter, FROM items (`sqlite3SrcListAssignCursors`) | in exactly that source order | same call sites |
| nMem for program | vdbeaux.c:2662 | `nMem += nCursor; if nCursor==0 && nMem>0 nMem++; EXPLAIN: at least 10` | `Vdbe::make_ready` |

### 2.2 Constant factoring (`OP_Once` vs init section)
| Decision | C | Rule | Rust |
|---|---|---|---|
| is the expression constant | expr.c:2562 `exprNodeIsConstant`, 2503 `exprNodeIsConstantFunction` | eCode 1..6; TK_FUNCTION constant iff EP_ConstFunc or (with pParse) FuncDef has CONSTANT/SLOCHNG and all args constant; TK_ID true/false is converted to TK_TRUEFALSE as a side effect; columns / registers / RAISE never; variables constant except in DDL modes | `expr.rs: expr_node_is_constant(_function)`, `sqlite3_expr_is_constant*` |
| where a constant is coded | expr.c:5915 `sqlite3ExprCodeRunJustOnce` | `regDest<0`: reuse a reusable `pConstExpr` entry that compares equal (`sqlite3ExprCompare==0`); expression with EP_HasFunc → inline `OP_Once` block (okConstFactor off inside); otherwise appended to `Parse.pConstExpr`, coded after `OP_Halt` by `sqlite3FinishCoding` | `exprcode.rs: sqlite3_expr_code_run_just_once`, `build.rs: sqlite3_finish_coding` |
| who factors | `sqlite3ExprCodeTemp` (6000), `sqlite3ExprCodeFactorable` (6064), `sqlite3ExprCodeExprList` with SQLITE_ECEL_FACTOR (6123), TK_FUNCTION in `sqlite3ExprCodeTarget` (5477) | only when `pParse->okConstFactor` (set by `sqlite3GetVdbe` for a top-level parse; cleared around the LHS of IN, inside OP_Once blocks, while the init section is coded) and `sqlite3ExprIsConstantNotJoin` | same functions |
| function argument registers | expr.c:5512-5530 | `constMask` bit i (i<32) if argument i is constant; `constMask!=0` → fresh registers `nMem+1..` (never released), else temp range | `sqlite3_expr_code_target` (TK_FUNCTION) |

### 2.3 Comparison: affinity, collation, P5
| Decision | C | Rule | Rust |
|---|---|---|---|
| expression affinity | expr.c:45 `sqlite3ExprAffinity` | column → column affinity; TK_SELECT → first result column; CAST → `sqlite3AffinityType`; vector / deferred function → first element; skip COLLATE / IF_NULL_ROW; REGISTER → op2 | `expr.rs: sqlite3_expr_affinity` |
| comparison affinity | expr.c:351 `sqlite3CompareAffinity` | both sides have affinity: NUMERIC if either numeric else BLOB; else the one that has it `| SQLITE_AFF_NONE` | `sqlite3_compare_affinity` |
| P5 of the comparison opcode | expr.c:410 `binaryCompareP5` | `sqlite3CompareAffinity(left, aff(right)) | jumpIfNull` (SQLITE_JUMPIFNULL 0x10, SQLITE_NULLEQ 0x80 for IS / IS NOT) | `binary_compare_p5` |
| collation of a comparison | expr.c:430 `sqlite3BinaryCompareCollSeq` | left has EP_Collate → left's; else right has EP_Collate → right's; else left's, else right's; commuted comparisons swap the operands | `sqlite3_binary_compare_coll_seq`, `code_compare` |
| collation of an expression | expr.c:254 `sqlite3ExprCollSeq` | TK_COLUMN → declared collation or the default (BINARY, not NULL); CAST / UPLUS → operand; vector → first element; TK_COLLATE → named; EP_Collate propagates through left, else right / first list item with EP_Collate | `sqlite3_expr_coll_seq` |
| operand evaluation order | expr.c:2416 `exprEvalRhsFirst`, 2441 `exprComputeOperands` | RHS first iff LHS has a subquery and RHS has none; `OP_IsNull` short-circuit around the subquery operand when the other operand can be NULL | `exprcode.rs: expr_eval_rhs_first / expr_compute_operands` |
| vector comparison | expr.c:715 `codeVectorCompare` | size mismatch → "row value misused"; LE/GE use LT/GT + `OP_ElseEq` on all but the last field; NE codes EQ then `OP_Not` | `code_vector_compare` |

### 2.4 IN operator strategy
| Decision | C | Rule | Rust |
|---|---|---|---|
| strategy | expr.c:3293 `sqlite3FindInIndex` | (1) candidate table/index (`isCandidateForInOpt`: single table, no WHERE/LIMIT/aggregate…) — **not ported: reported unsupported**; (2) IN_INDEX_NOOP when a list RHS is not constant or has ≤2 terms (and the caller allows it): chain of `OP_Eq`/`OP_Ne`; (3) else ephemeral table (IN_INDEX_EPH) | `sqlite3_find_in_index`, `is_candidate_for_in_opt` |
| RHS-has-NULL register | expr.c:3316-3327, 3493 | wanted only when `destIfFalse!=destIfNull`; dropped if no RHS column can be NULL; allocated before the RHS is coded | same |
| materialization once / per use | expr.c:3660 `sqlite3CodeRhsOfIN` | no EP_VarSelect and `iSelfTab==0` → `OP_BeginSubrtn`+`OP_Once` subroutine; a later identical subquery (same selId and affinity string, `SubrtnSig`, bloom-filter bit `mSubrtnSig`) or the same Expr (EP_Subrtn) reuses it with `OP_Gosub`+`OP_OpenDup`; a non-constant list term cancels the Once (`OP_Noop`×2) | `sqlite3_code_rhs_of_in`, `find_compatible_in_rhs_subrtn` |
| Bloom filter | expr.c:3799 | once-materialized SELECT RHS, all collations BINARY, optimization enabled → `OP_Blob 10000` + `OP_FilterAdd`; `OP_Filter` before `OP_NotFound` when the subroutine's `OP_Once.p3>0` | `sqlite3_code_rhs_of_in`, `sqlite3_expr_code_in` |
| affinity string | expr.c:3531 `exprINAffinity` | per LHS field: `sqlite3CompareAffinity(RHS column, aff(LHS))` for a SELECT RHS, `aff(LHS)` for a list | `expr_in_affinity` |
| list RHS key affinity | expr.c:3841 | LHS affinity ≤NONE → BLOB, REAL → NUMERIC | `sqlite3_code_rhs_of_in` |
| single-element constant list | parse.y `expr ::= expr in_op LP exprlist RP` | `x IN (c)` → `x = +c` (unless c is a COLLATE or x a vector); `IN ()` → constant 0/1 unless x contains a function | `parse.rs` rule 223 |

### 2.5 Short-circuit and jump rules
| Decision | C | Rule | Rust |
|---|---|---|---|
| AND/OR value | expr.c:4988 `exprCodeTargetAndOr` | simplified by `sqlite3ExprSimplifiedAndOr` (EP_IsTrue/EP_IsFalse operands) → `OP_And r,r`; subquery operand → `OP_If`/`OP_IfNot` skip + `OP_Or` fix-up | `expr_code_target_and_or` |
| conditional jump | expr.c:6240 `sqlite3ExprIfTrue`, 6400 `sqlite3ExprIfFalse` | AND/OR recurse with a local label; NOT flips; TK_TRUTH maps to IfTrue/IfFalse with jumpIfNull; comparisons code the (inverted: `((op+(TK_ISNULL&1))^1)-(TK_ISNULL&1)`) opcode directly; BETWEEN via `exprCodeBetween`; IN via `sqlite3ExprCodeIN`; constants with EP_IsTrue/EP_IsFalse emit `OP_Goto` or nothing | `sqlite3_expr_if_true / sqlite3_expr_if_false` |
| coalesce / iif / likely | expr.c:4680 `exprCodeInlineFunction` | coalesce: `OP_NotNull` chain; iif/if: coded as CASE; unlikely/likely/likelihood: the first argument | `expr_code_inline_function` |
| CASE | expr.c:5800 | `CASE x`: x evaluated once into a register (TK_REGISTER copy), each WHEN `x=e` via IfFalse(JUMPIFNULL) | `sqlite3_expr_code_target` (TK_CASE) |
| NULL tests | parse.y `sqlite3PExprIsNull`, resolve.c:1049 | literal operand → integer constant at parse time; `X IS [NOT] NULL` with X that cannot be NULL inside a WHERE clause of every enclosing context → constant | `parse.rs: sqlite3_p_expr_is_null`, `resolve.rs` (TK_ISNULL/TK_NOTNULL) |

### 2.6 SELECT without FROM, compound, VALUES, aggregates
| Decision | C | Rule | Rust |
|---|---|---|---|
| zero-table WHERE | where.c:6954, 7030 | terms split at AND (`sqlite3WhereSplit`, term = expr without COLLATE/likely), analyzed last-to-first, each non-virtual term coded `sqlite3ExprIfFalse(…, iBreak, JUMPIFNULL)`; vector `=`/`IS` replaced by per-field terms appended to the term list; OR term replaced by its simplification | `where_.rs: sqlite3_where_begin (nTabList==0), expr_analyze` |
| ORDER BY of a FROM-less SELECT | where.c:6955 + path solver | always satisfied (`nOBSat==nExpr`) → the sorter `OP_OpenEphemeral`/`OP_SorterOpen` becomes `OP_Noop` | `sqlite3_select` |
| DISTINCT of a FROM-less SELECT | where.c:6956-6960, 7053-7060, 6209-6216 | with ORDER BY: WHERE_DISTINCT_UNIQUE (ephemeral table → `OP_Noop`); without: DISTINCTBY + path solver second pass → WHERE_DISTINCT_ORDERED (`OP_Null 1,reg` + `OP_Ne`/`OP_Eq` p5=0x80 + `OP_Copy`) | `where_.rs`, `select.rs: code_distinct / fix_distinct_open_eph` |
| DISTINCT + identical ORDER BY | select.c:8331 | `sqlite3CopySortOrder` copies the DESC flags onto the result list (always, as a side effect), lists equal → rewritten as GROUP BY (`isTnct=2`) | `sqlite3_select`, `sqlite3_copy_sort_order` |
| LIMIT / OFFSET | select.c:2582 `computeLimitRegisters` | integer constant → `OP_Integer` (+`OP_Goto` if 0); else `OP_MustBeInt`+`OP_IfNot`; OFFSET → `OP_MustBeInt`+`OP_OffsetLimit`, registers `iLimit, iOffset, iOffset+1` | `compute_limit_registers` |
| compound operator | select.c:3024 `multiSelect` | SF_MultiValue → `multiSelectValues`; ORDER BY → merge; UNION / INTERSECT / EXCEPT without ORDER BY → merge with a synthesized `ORDER BY 1` (unless UNION … LIMIT 1); UNION ALL → the arms in sequence sharing the limit registers | `select.rs: multi_select`, `merge.rs` |
| merge | select.c:3490 `multiSelectByMerge` | ORDER BY extended to every result column for non-ALL; `OP_Permutation` only if the key order is not the identity; split point: balanced for >3 arms of the same UNION [ALL]; two co-routines + output subroutines + the 7 merge labels | `merge.rs: multi_select_by_merge` |
| VALUES rows | insert.c:673 `sqlite3MultiValues` | co-routine unless: WITH clause seen, schema parse, non-constant row (IS-operators count as non-constant), first row has an affinity, special parse; then `UNION ALL` chain with SF_MultiValue | `select.rs: sqlite3_multi_values` |
| scalar / EXISTS subquery | expr.c:3908 `sqlite3CodeSubselect` | `OP_BeginSubrtn` + `OP_Once` unless correlated; LIMIT: none → `LIMIT 1`; existing non-0/1 limit X → `X<>0`; SRT_Mem with separate result registers when DISTINCT+OFFSET; `OP_ClrSubtype` if the result can carry a subtype | `sqlite3_code_subselect` |
| aggregate accumulators | select.c:6784-7085, 8990-9060 | `regAcc` flag only when there are accumulator columns and no min()/max(); min/max `OP_CollSeq` with `regHit`; DISTINCT aggregate argument through `codeDistinct`; ORDER BY aggregate through an ephemeral table | `reset_accumulator / update_accumulator / finalize_agg_functions` |
| GROUP BY without tables | select.c:8612-8915 | rows arrive "ordered" (`nOBSat==nGroupBy`) → no sorter (`OP_SorterOpen` → `OP_Noop`), group-change test `OP_Compare`+`OP_Jump`, output / reset subroutines; HAVING terms that are constant or GROUP BY terms move to WHERE (`havingToWhere`) | `sqlite3_select` (GROUP BY branch), `having_to_where` |

### 2.7 Function lookup and flags
| Decision | C | Rule | Rust |
|---|---|---|---|
| best match | callback.c:296 `matchQuality`, 402 `sqlite3FindFunction` | application functions first (db->aFunc), built-ins only if none matched (or DBFLAG_PreferBuiltin); score: exact nArg 4, variadic 1 (−3: ≥1 arg, −4: ≥2 args), +2 same encoding; first best wins in chain order | `callback.rs`; tables generated from the reference library (`builtin_funcs.rs`) |
| errors | resolve.c:1150-1345 | unknown → "no such function"; wrong arity → "wrong number of arguments to function X()"; aggregate where not allowed / window-only function without OVER → "misuse of aggregate|window function X()"; ORDER BY in a non-aggregate; likelihood() second argument | `resolve.rs: resolve_expr_step` (TK_FUNCTION) |
| flags used by the generator | sqliteInt.h | INLINE (+iArg), UNLIKELY, CONSTANT/SLOCHNG (EP_ConstFunc), NEEDCOLL (`OP_CollSeq`), LENGTH/TYPEOF/BYTELEN (OP_Column p5), INTERNAL, DIRECT/UNSAFE, SUBTYPE/RESULT_SUBTYPE, MINMAX/ANYORDER/WINDOW, COUNT | `consts.rs`, `builtin_funcs.rs` |

## 3. What "supported" means (statement classes of the corpus)
Generator `gen_corpus.py <seed> <count> <class>` (deterministic; 12 rounds of 600-1500 statements per class):

| Class | Content | Statements |
|---|---|---|
| expr | random expression trees over literals (integer / hex / float / string / blob / NULL / TRUE / FALSE, digit separators), parameters (`?`, `?N`, `:a`, `@a`, `$a`, `$a::b(c)`), every unary / binary / comparison / LIKE-GLOB-MATCH / IS / BETWEEN / IN (list and SELECT) / CASE / CAST / COLLATE / EXISTS / scalar subquery / row values operator | 17 100 |
| func | every built-in function of the reference library (`cdump/funcs.tsv`, 186 entries) x argument shapes: right and wrong arity, constant / variable / NULL arguments, aggregates in and out of aggregate context, DISTINCT, ORDER BY in aggregates, FILTER / OVER (rejected) | 17 100 |
| select | FROM-less SELECT forms: WHERE, DISTINCT, LIMIT / OFFSET, ORDER BY (terms by number / alias / expression, ASC / DESC / NULLS), GROUP BY / HAVING, aggregates, compound (UNION [ALL] / INTERSECT / EXCEPT chains, with ORDER BY / LIMIT), VALUES (one row, multi-row co-routine, compound of VALUES), WITH (non-recursive, column lists), sub-selects in every position | 17 100 |
| err | statements that fail to compile (syntax errors at every token position, unknown functions / collations, misuse of aggregates, limits) | 428 |
| txn | BEGIN [DEFERRED / IMMEDIATE / EXCLUSIVE] [TRANSACTION [name]], COMMIT / END, ROLLBACK [TO [SAVEPOINT] x], SAVEPOINT, RELEASE (quoted names, keywords as names) | 2 100 |
| limits | SQLITE_LIMIT boundaries: expression depth, compound terms, function arguments, variable numbers, column counts, LIKE pattern length, vector sizes | 695 |
| smoke | hand-written statements used while porting | 90 |

Reasons the port answers "unsupported" on this corpus (144 of 54 613, library level):

| Count | Reason (first missing piece) | C function not ported |
|---|---|---|
| 88 | compound SELECT with COLLATE in ORDER BY | `convertCompoundSelectToSubquery` (creates a FROM subquery) |
| 36 | ORDER BY that needs a sorter (GROUP BY + ORDER BY, DISTINCT + ORDER BY rewritten to GROUP BY) | `pushOntoSorter`, `generateSortTail` |
| 8 | window function | window.c |
| 7 | FROM clause term | where.c planning |
| 4 | IN table (`x IN tbl`) | FROM clause |
| 1 | statement kind (grammar rule not ported) | — |

## 4. C-side coverage of the ported functions (gcov, accepted corpus = 54 469 statements)
Build: `cov/cexplain_cov` = reference amalgamation at -O0 with `--coverage` (scratch copy; the reference tree is not
touched). Only the functions listed in section 1 are counted. Per file:


| C file | lines covered | branches taken at least once |
|---|---|---|
| vdbeaux.c | 296 / 381 (77.7%) | 93 / 164 (56.7%) |
| tokenize.c | 247 / 300 (82.3%) | 192 / 265 (72.5%) |
| parse.y | 580 / 1256 (46.2%) | 231 / 524 (44.1%) |
| util.c | 102 / 108 (94.4%) | 89 / 112 (79.5%) |
| build.c | 122 / 161 (75.8%) | 77 / 126 (61.1%) |
| printf.c | 8 / 8 (100.0%) | 9 / 12 (75.0%) |
| walker.c | 52 / 62 (83.9%) | 51 / 78 (65.4%) |
| callback.c | 64 / 70 (91.4%) | 52 / 66 (78.8%) |
| resolve.c | 546 / 781 (69.9%) | 393 / 681 (57.7%) |
| expr.c | 1911 / 2374 (80.5%) | 1126 / 1647 (68.4%) |
| select.c | 1125 / 1618 (69.5%) | 629 / 1133 (55.5%) |
| insert.c | 73 / 74 (98.6%) | 36 / 44 (81.8%) |
| where.c | 193 / 428 (45.1%) | 115 / 354 (32.5%) |
| whereexpr.c | 147 / 224 (65.6%) | 101 / 154 (65.6%) |
| **total** | 5466 / 7845 (69.7%) | 3194 / 5360 (59.6%) |

Reading the numbers: gcov counts whole C functions. Functions that V1 ports only for the FROM-less class
(`sqlite3Select`, `sqlite3WhereBegin`, `sqlite3WhereEnd`, `lookupName`, `selectExpander`, `yy_reduce`, `exprAnalyze`,
`sqlite3FindInIndex`, `selectInnerLoop`) contain the table paths too; those branches are untaken *and* not ported (the
port reports unsupported there). They account for 547 of the 1223 source lines with a never-taken branch.

### Not verified: functions with never-taken branches (sorted by count)
"lines" = executable C lines of the function; "untaken" = source lines carrying a branch the corpus never took
(every such line is in `cov/untaken.txt`).

| C function | file | lines | line % | branches | taken % | untaken | what the untaken part is (where classified) |
|---|---|---|---|---|---|---|---|
| `sqlite3Select` | select.c | 506 | 54.1 | 378 | 42.1 | 123 | FROM-clause work is outside V1 and reports unsupported: subquery flattening / co-routines / materialization of FROM items, sorter (`pushOntoSorter`), window rewrite, min/max and count(*) table optimizations, GROUP BY with a sorter; taken: FROM-less paths, DISTINCT, aggregates, GROUP BY without sorter, compound dispatch |
| `sqlite3WhereBegin` | where.c | 248 | 54.0 | 222 | 38.3 | 80 | only the zero-table path and the single VALUES co-routine scan are ported; everything from `whereLoopAddAll` on reports unsupported |
| `lookupName` | resolve.c | 255 | 44.7 | 248 | 36.7 | 77 | column lookup in FROM tables, triggers (NEW/OLD), upsert EXCLUDED, RETURNING, generated columns, rowid aliases: unsupported; taken: result-alias matching, double-quoted string fallback, no-such-column errors, co-routine columns |
| `selectExpander` | select.c | 196 | 52.5 | 208 | 37.5 | 69 | FROM items other than the multi-row VALUES co-routine (tables, views, CTE references, table-valued functions, JOIN ... USING / NATURAL) report unsupported; `*` expansion only over the co-routine |
| `sqlite3WhereEnd` | where.c | 164 | 27.4 | 122 | 18.0 | 54 | same: only loop-less and co-routine `OP_Goto`/`InitCoroutine` ends |
| `yy_reduce` | parse.y | 1126 | 40.9 | 454 | 39.0 | 52 | 348 action rules; 176 ported (expressions, SELECT core, compound, VALUES, WITH, CAST types, transactions); the rest (DDL, DML, FROM terms, window, trigger, PRAGMA, ATTACH, VACUUM ...) set unsupported |
| `sqlite3ExprCodeTarget` | expr.c | 362 | 71.8 | 207 | 61.8 | 47 | TK_COLUMN on real tables, TK_AGG_COLUMN from tables, TK_IF_NULL_ROW, TK_RAISE, TK_TRIGGER, window functions, TK_SELECT_COLUMN of table subqueries, generated columns, `OP_Affinity` for indexed expressions |
| `resolveExprStep` | resolve.c | 197 | 79.7 | 185 | 69.7 | 38 | TK_ROW / TK_DOT (table.column, schema.table.column) against FROM tables, window functions and FILTER, RAISE, NC_IdxExpr / NC_GenCol / NC_PartIdx / NC_IsCheck contexts (DDL) |
| `exprAnalyze` | whereexpr.c | 216 | 64.3 | 150 | 64.7 | 36 | terms with table masks: OR-term analysis, LIKE / GLOB range terms, virtual terms for BETWEEN / IS NULL on columns, transitive constraints, vtab MATCH |
| `sqlite3FindInIndex` | expr.c | 103 | 35.9 | 99 | 34.3 | 29 | IN against a table or index (`isCandidateForInOpt` succeeded): unsupported; taken: IN_INDEX_NOOP, IN_INDEX_EPH |
| `selectInnerLoop` | select.c | 140 | 46.4 | 96 | 46.9 | 27 | SRT_* destinations of INSERT ... SELECT, CTE queues, sorter push, `SRT_Upfrom`, `SRT_DistQueue`; taken: Output, Mem, Set, Exists, Coroutine, EphemTab, Union/Except via merge |
| `analyzeAggregate` | expr.c | 84 | 65.5 | 69 | 50.7 | 22 | aggregate column references to table columns (`findOrCreateAggInfoColumn`), indexed expressions; taken: TK_AGG_FUNCTION bookkeeping |
| `resolveSelectStep` | resolve.c | 97 | 74.2 | 92 | 67.4 | 20 | FROM subqueries, correlated FROM, window definitions, `IS_VIEW`/trigger contexts |
| `updateAccumulator` | select.c | 87 | 85.1 | 70 | 61.4 | 20 | `regAcc` with table columns, FILTER clause, DISTINCT aggregate over an index, min/max over indexed columns with `OP_CollSeq` variations |
| `sqlite3GetToken` | tokenize.c | 197 | 94.9 | 181 | 87.8 | 16 |  |
| `sqlite3CodeRhsOfIN` | expr.c | 105 | 96.2 | 68 | 73.5 | 16 |  |
| `sqlite3RunParser` | tokenize.c | 75 | 80.0 | 54 | 61.1 | 15 | OOM, interrupt, `SQLITE_TOOBIG`, parser stack overflow paths |
| `sqlite3ExprCompare` | expr.c | 51 | 86.3 | 96 | 71.9 | 15 |  |
| `findOrCreateAggInfoColumn` | expr.c | 39 | 0.0 | 30 | 0.0 | 14 | needs a column of a FROM table (not reachable without FROM) |
| `sqlite3FinishCoding` | build.c | 55 | 69.1 | 42 | 57.1 | 14 | `OP_Transaction` / `OP_VerifyCookie` loop for statements that open a database (none in V1 except BEGIN's own opcodes), `OP_TableLock`, vtab locks, autoincrement, RETURNING; the `pConstExpr` part is taken |
| `sqlite3VdbeNextOpcode` | vdbeaux.c | 45 | 51.1 | 32 | 28.1 | 13 | trigger sub-programs and `EXPLAIN QUERY PLAN` (no `OP_Explain` / `OP_Program` in V1 programs) |
| `exprDup` | expr.c | 57 | 73.7 | 42 | 54.8 | 13 | EXPRDUP_REDUCE with window / `EP_TokenOnly` sizes of nodes V1 never duplicates |
| `sqlite3ExprCodeIN` | expr.c | 108 | 93.5 | 76 | 82.9 | 12 |  |
| `sqlite3AffinityType` | build.c | 45 | 73.3 | 42 | 57.1 | 11 |  |
| `sqlite3SrcListDup` | expr.c | 43 | 72.1 | 24 | 50.0 | 11 | FROM items with tables / ON / USING |
| `sqlite3WalkSelectExpr` | walker.c | 14 | 64.3 | 22 | 31.8 | 10 |  |
| `resolveCompoundOrderBy` | resolve.c | 53 | 94.3 | 46 | 73.9 | 10 |  |
| `exprNodeIsConstant` | expr.c | 39 | 71.8 | 39 | 64.1 | 9 |  |
| `sqlite3CodeSubselect` | expr.c | 57 | 89.5 | 36 | 75.0 | 8 |  |
| `sqlite3ExprIfTrue` | expr.c | 77 | 96.1 | 45 | 80.0 | 8 |  |
| `multiSelectByMerge` | select.c | 140 | 99.3 | 78 | 89.7 | 8 |  |
| `sqlite3VdbeDisplayP4` | vdbeaux.c | 74 | 64.9 | 39 | 61.5 | 7 | P4 kinds no V1 program contains: P4_TABLE, P4_INDEX, P4_VTAB, P4_INTARRAY (only in the Rust unit test), P4_SUBPROGRAM, P4_MEM, P4_INT64-free build |
| `getToken` | tokenize.c | 13 | 0.0 | 16 | 0.0 | 7 | only used after WINDOW / OVER / FILTER keywords: reached only by statements V1 then rejects (window functions) |
| `sqlite3GetInt32` | util.c | 32 | 87.5 | 38 | 81.6 | 7 |  |
| `sqlite3ExprCollSeq` | expr.c | 37 | 97.3 | 44 | 77.3 | 7 |  |
| `sqlite3ExprIsInteger` | expr.c | 31 | 80.7 | 24 | 58.3 | 7 |  |
| `convertCompoundSelectToSubquery` | select.c | 36 | 27.8 | 24 | 58.3 | 7 | ported only as the test that decides it is needed; the rewrite itself reports unsupported (it creates a FROM subquery) |
| `sqlite3VdbeList` | vdbeaux.c | 41 | 70.7 | 16 | 50.0 | 6 | `EXPLAIN QUERY PLAN` rows, interrupt / OOM |
| `sqlite3ExprDataType` | expr.c | 33 | 0.0 | 19 | 0.0 | 6 | only called for indexed expressions / subtype checks on columns (FROM tables) |
| `sqlite3ExprAddFunctionOrderBy` | expr.c | 25 | 48.0 | 16 | 43.8 | 6 |  |
| `sqlite3ExprListDup` | expr.c | 29 | 65.5 | 16 | 43.8 | 6 |  |
| `sqlite3ExprCanBeNull` | expr.c | 17 | 100.0 | 21 | 66.7 | 6 |  |
| `generateOutputSubroutine` | select.c | 80 | 53.8 | 24 | 50.0 | 6 | destinations other than SRT_Output / SRT_Mem / SRT_Set / SRT_Coroutine / SRT_EphemTab of a merge |
| `sqlite3MultiValues` | insert.c | 61 | 98.4 | 32 | 81.2 | 6 |  |
| `isCandidateForInOpt` | expr.c | 19 | 63.2 | 22 | 63.6 | 5 |  |
| `sqlite3ExprCodeGetColumnOfTable` | expr.c | 23 | 34.8 | 12 | 41.7 | 5 |  |
| `multiSelect` | select.c | 65 | 92.3 | 40 | 82.5 | 5 |  |
| `resetAccumulator` | select.c | 31 | 93.5 | 24 | 70.8 | 5 |  |
| `sqlite3DequoteNumber` | util.c | 18 | 100.0 | 36 | 80.6 | 4 |  |
| `sqlite3DecOrHexToI64` | util.c | 14 | 100.0 | 16 | 62.5 | 4 |  |
| `sqlite3WalkSelectFrom` | walker.c | 11 | 72.7 | 12 | 58.3 | 4 |  |
| `sqlite3WalkSelect` | walker.c | 13 | 92.3 | 14 | 71.4 | 4 |  |
| `sqlite3FindFunction` | callback.c | 37 | 91.9 | 30 | 83.3 | 4 |  |
| `resolveAlias` | resolve.c | 18 | 77.8 | 10 | 50.0 | 4 |  |
| `notValidImpl` | resolve.c | 9 | 0.0 | 8 | 0.0 | 4 |  |
| `sqlite3ExprFunctionUsable` | expr.c | 7 | 57.1 | 8 | 25.0 | 4 |  |
| `sqlite3ExprAssignVarNumber` | expr.c | 36 | 100.0 | 30 | 86.7 | 4 |  |
| `sqlite3ExprCodeExprList` | expr.c | 25 | 84.0 | 28 | 75.0 | 4 |  |
| `finalizeAggFunctions` | select.c | 30 | 96.7 | 22 | 72.7 | 4 |  |
| `sqlite3WithAdd` | build.c | 18 | 83.3 | 14 | 71.4 | 4 |  |
| `resolveP2Values` | vdbeaux.c | 36 | 72.2 | 19 | 63.2 | 3 |  |
| `sqlite3VdbeChangeP4` | vdbeaux.c | 18 | 88.9 | 16 | 75.0 | 3 |  |
| `sqlite3VdbeMakeReady` | vdbeaux.c | 45 | 93.3 | 18 | 72.2 | 3 |  |
| `sqlite3Parser` | parse.y | 26 | 76.9 | 12 | 66.7 | 3 |  |
| `sqlite3RecordErrorOffsetOfExpr` | printf.c | 8 | 100.0 | 12 | 75.0 | 3 |  |
| `sqlite3GetCollSeq` | callback.c | 14 | 92.9 | 14 | 64.3 | 3 |  |
| `matchQuality` | callback.c | 13 | 84.6 | 18 | 77.8 | 3 |  |
| `areDoubleQuotedStringsEnabled` | resolve.c | 7 | 57.1 | 8 | 25.0 | 3 |  |
| `resolveOrderByTermToExprList` | resolve.c | 18 | 77.8 | 6 | 16.7 | 3 |  |
| `sqlite3ResolveExprListNames` | resolve.c | 25 | 96.0 | 12 | 75.0 | 3 |  |
| `sqlite3ExprAffinity` | expr.c | 22 | 100.0 | 24 | 79.2 | 3 |  |
| `comparisonAffinity` | expr.c | 9 | 0.0 | 6 | 0.0 | 3 |  |
| `sqlite3ExprForVectorField` | expr.c | 16 | 87.5 | 8 | 62.5 | 3 |  |
| `sqlite3SelectDup` | expr.c | 34 | 91.2 | 10 | 50.0 | 3 |  |
| `sqlite3ExprListSetName` | expr.c | 9 | 88.9 | 6 | 50.0 | 3 |  |
| `exprNodeIsConstantFunction` | expr.c | 16 | 100.0 | 14 | 78.6 | 3 |  |
| `sqlite3ExprCodeGetColumn` | expr.c | 7 | 57.1 | 6 | 16.7 | 3 |  |
| `exprCodeInlineFunction` | expr.c | 42 | 52.4 | 13 | 38.5 | 3 |  |
| `sqlite3ExprCodeRunJustOnce` | expr.c | 26 | 100.0 | 22 | 86.4 | 3 |  |
| `sqlite3ExprCode` | expr.c | 10 | 100.0 | 10 | 70.0 | 3 |  |
| `sqlite3ExprIfFalse` | expr.c | 77 | 94.8 | 45 | 93.3 | 3 |  |
| `sqlite3ReferencesSrcList` | expr.c | 20 | 85.0 | 10 | 70.0 | 3 |  |
| `minMaxQuery` | select.c | 21 | 100.0 | 16 | 81.2 | 3 |  |
| `sqlite3WithPush` | select.c | 9 | 77.8 | 8 | 50.0 | 3 |  |
| `sqlite3ExpandSubquery` | select.c | 15 | 93.3 | 6 | 50.0 | 3 |  |
| `havingToWhereExprCb` | select.c | 16 | 100.0 | 10 | 70.0 | 3 |  |
| `sqlite3BeginTransaction` | build.c | 17 | 88.2 | 14 | 78.6 | 3 |  |
| `sqlite3VdbeAddFunctionCall` | vdbeaux.c | 16 | 87.5 | 4 | 50.0 | 2 |  |
| `analyzeWindowKeyword` | tokenize.c | 6 | 0.0 | 4 | 0.0 | 2 | same |
| `analyzeOverKeyword` | tokenize.c | 5 | 0.0 | 6 | 0.0 | 2 | same |
| `tokenExpr` | parse.y | 23 | 95.7 | 6 | 66.7 | 2 |  |
| `parserAddExprIdListTerm` | parse.y | 7 | 71.4 | 6 | 33.3 | 2 |  |
| `sqlite3WalkExprNN` | walker.c | 17 | 94.1 | 22 | 86.4 | 2 |  |
| `sqlite3ExprAddCollateToken` | expr.c | 10 | 100.0 | 6 | 50.0 | 2 |  |
| `sqlite3BinaryCompareCollSeq` | expr.c | 9 | 100.0 | 8 | 75.0 | 2 |  |
| `exprVectorRegister` | expr.c | 17 | 82.3 | 6 | 66.7 | 2 |  |
| `sqlite3ExprAlloc` | expr.c | 15 | 93.3 | 12 | 83.3 | 2 |  |
| `sqlite3ExprListToValues` | expr.c | 21 | 100.0 | 16 | 87.5 | 2 |  |
| `sqlite3ExprAnd` | expr.c | 13 | 92.3 | 8 | 75.0 | 2 |  |
| `sqlite3ExprFunction` | expr.c | 16 | 87.5 | 10 | 80.0 | 2 |  |
| `sqlite3ExprCheckIN` | expr.c | 10 | 80.0 | 8 | 75.0 | 2 |  |
| `sqlite3ExprToRegister` | expr.c | 8 | 100.0 | 4 | 50.0 | 2 |  |
| `sqlite3SelectNew` | select.c | 29 | 89.7 | 8 | 75.0 | 2 |  |
| `fixDistinctOpenEph` | select.c | 13 | 92.3 | 10 | 80.0 | 2 |  |
| `multiSelectByMergeKeyInfo` | select.c | 18 | 100.0 | 10 | 80.0 | 2 |  |
| `sqlite3MultiValuesEnd` | insert.c | 7 | 100.0 | 6 | 66.7 | 2 |  |
| `sqlite3EndTransaction` | build.c | 7 | 85.7 | 6 | 66.7 | 2 |  |
| `sqlite3Savepoint` | build.c | 8 | 75.0 | 6 | 50.0 | 2 |  |
| `translateColumnToCopy` | where.c | 16 | 87.5 | 10 | 80.0 | 2 |  |
| `sqlite3VdbeChangeP5` | vdbeaux.c | 3 | 100.0 | 2 | 50.0 | 1 |  |
| `sqlite3VdbeJumpHereOrPopInst` | vdbeaux.c | 5 | 0.0 | 2 | 0.0 | 1 |  |
| `sqlite3VdbeChangeToNoop` | vdbeaux.c | 8 | 100.0 | 2 | 50.0 | 1 |  |
| `sqlite3VdbeAppendP4` | vdbeaux.c | 7 | 85.7 | 2 | 50.0 | 1 |  |
| `sqlite3VdbeGetOp` | vdbeaux.c | 4 | 75.0 | 2 | 50.0 | 1 |  |
| `sqlite3VdbeUsesBtree` | vdbeaux.c | 5 | 80.0 | 4 | 75.0 | 1 |  |
| `analyzeFilterKeyword` | tokenize.c | 4 | 0.0 | 4 | 0.0 | 1 | same |
| `yy_find_shift_action` | parse.y | 14 | 92.9 | 10 | 70.0 | 1 |  |
| `sqlite3PExprIs` | parse.y | 5 | 100.0 | 6 | 83.3 | 1 |  |
| `parserDoubleLinkSelect` | parse.y | 20 | 100.0 | 16 | 93.8 | 1 |  |
| `attachWithToSelect` | parse.y | 6 | 83.3 | 2 | 50.0 | 1 |  |
| `sqlite3ErrorMsg` | util.c | 18 | 88.9 | 6 | 83.3 | 1 |  |
| `sqlite3Dequote` | util.c | 14 | 100.0 | 10 | 90.0 | 1 |  |
| `sqlite3HexToBlob` | util.c | 8 | 100.0 | 4 | 75.0 | 1 |  |
| `incrAggFunctionDepth` | resolve.c | 7 | 42.9 | 2 | 50.0 | 1 |  |
| `sqlite3ResolveOrderGroupBy` | resolve.c | 14 | 100.0 | 14 | 78.6 | 1 |  |
| `resolveOrderGroupBy` | resolve.c | 26 | 100.0 | 20 | 95.0 | 1 |  |
| `sqlite3ResolveExprNames` | resolve.c | 17 | 94.1 | 10 | 90.0 | 1 |  |
| `sqlite3ExprSkipCollateAndLikely` | expr.c | 8 | 87.5 | 8 | 87.5 | 1 |  |
| `codeCompare` | expr.c | 9 | 88.9 | 4 | 75.0 | 1 |  |
| `sqlite3VectorFieldSubexpr` | expr.c | 6 | 83.3 | 6 | 66.7 | 1 |  |
| `exprSetHeight` | expr.c | 11 | 90.9 | 10 | 70.0 | 1 |  |
| `sqlite3ExprInt32` | expr.c | 10 | 100.0 | 4 | 75.0 | 1 |  |
| `sqlite3ExprAttachSubtrees` | expr.c | 15 | 86.7 | 8 | 87.5 | 1 |  |
| `sqlite3PExpr` | expr.c | 11 | 81.8 | 2 | 50.0 | 1 |  |
| `sqlite3PExprAddSelect` | expr.c | 7 | 85.7 | 2 | 50.0 | 1 |  |
| `sqlite3WithDup` | expr.c | 13 | 100.0 | 6 | 83.3 | 1 |  |
| `sqlite3ExprListSetSortOrder` | expr.c | 10 | 100.0 | 8 | 87.5 | 1 |  |
| `sqlite3ExprListSetSpan` | expr.c | 8 | 100.0 | 4 | 75.0 | 1 |  |
| `sqlite3SetHasNullFlag` | expr.c | 10 | 90.0 | 2 | 50.0 | 1 |  |
| `exprINAffinity` | expr.c | 14 | 100.0 | 8 | 87.5 | 1 |  |
| `sqlite3SubselectError` | expr.c | 5 | 100.0 | 2 | 50.0 | 1 |  |
| `sqlite3VectorErrorMsg` | expr.c | 5 | 0.0 | 2 | 0.0 | 1 |  |
| `findCompatibleInRhsSubrtn` | expr.c | 18 | 100.0 | 14 | 92.9 | 1 |  |
| `codeReal` | expr.c | 6 | 100.0 | 4 | 75.0 | 1 |  |
| `exprCodeVector` | expr.c | 12 | 91.7 | 6 | 83.3 | 1 |  |
| `sqlite3ExprCodeTemp` | expr.c | 15 | 100.0 | 10 | 90.0 | 1 |  |
| `sqlite3ExprCodeCopy` | expr.c | 6 | 100.0 | 2 | 50.0 | 1 |  |
| `sqlite3ExprCodeFactorable` | expr.c | 5 | 100.0 | 4 | 75.0 | 1 |  |
| `exprCodeBetween` | expr.c | 26 | 100.0 | 4 | 75.0 | 1 |  |
| `sqlite3KeyInfoFromExprList` | select.c | 9 | 100.0 | 4 | 75.0 | 1 |  |
| `sqlite3GetVdbe` | select.c | 6 | 100.0 | 4 | 75.0 | 1 |  |
| `multiSelectCollSeq` | select.c | 7 | 100.0 | 6 | 83.3 | 1 |  |
| `multiSelectValues` | select.c | 16 | 100.0 | 10 | 90.0 | 1 |  |
| `unionWithLimitOne` | select.c | 9 | 100.0 | 12 | 91.7 | 1 |  |
| `selectAddSubqueryTypeInfo` | select.c | 10 | 100.0 | 8 | 87.5 | 1 |  |
| `sqlite3SelectPrep` | select.c | 8 | 100.0 | 8 | 87.5 | 1 |  |
| `analyzeAggFuncArgs` | select.c | 11 | 90.9 | 6 | 83.3 | 1 |  |
| `sqlite3CteNew` | build.c | 11 | 81.8 | 2 | 50.0 | 1 |  |

Sum of untaken source lines: 1223. Functions with every branch taken at least once (82): `sqlite3VdbeAddOp0`, `sqlite3VdbeAddOp1`, `sqlite3VdbeAddOp2`, `sqlite3VdbeAddOp3`, `sqlite3VdbeAddOp4Int`, `sqlite3VdbeAddInt64`, `sqlite3VdbeAddDouble`, `sqlite3VdbeGoto`, `sqlite3VdbeLoadString`, `sqlite3VdbeAddOp4`, `sqlite3VdbeEndCoroutine`, `sqlite3VdbeMakeLabel`, `sqlite3VdbeResolveLabel`, `sqlite3VdbeCurrentAddr`, `sqlite3VdbeChangeOpcode`, `sqlite3VdbeChangeP2`, `sqlite3VdbeJumpHere`, `sqlite3VdbeGetLastOp`, `yy_find_reduce_action`, `yy_shift`, `sqlite3PExprIsNull`, `sqlite3DequoteExpr`, `sqlite3WalkExpr`, `sqlite3WalkExprList`, `sqlite3FindCollSeq`, `exprProbability`, `resolveSetExprSubtypeArg`, `resolveAsName`, `resolveOutOfRangeError`, `sqlite3ResolveSelectNames`, `sqlite3ExprAddCollateString`, `sqlite3ExprSkipCollate`, `sqlite3ExprNNCollSeq`, `sqlite3CompareAffinity`, `binaryCompareP5`, `sqlite3ExprIsVector`, `sqlite3ExprVectorSize`, `exprCodeSubselect`, `codeVectorCompare`, `sqlite3ExprCheckHeight`, `heightOfSelect`, `sqlite3ExprSetHeightAndFlags`, `sqlite3ExprListAppend`, `sqlite3ExprListFlags`, `sqlite3IsTrueOrFalse`, `sqlite3ExprIdToTrueFalse`, `sqlite3ExprTruthValue`, `sqlite3ExprSimplifiedAndOr`, `exprEvalRhsFirst`, `exprComputeOperands`, `exprIsConst`, `sqlite3ExprListIsConstant`, `exprNodeIsConstantOrGroupBy`, `sqlite3ExprIsConstantOrGroupBy`, `sqlite3InRhsIsConstant`, `codeInteger`, `sqlite3ExprCodeMove`, `setDoNotMergeFlagOnCopy`, `exprNodeCanReturnSubtype`, `sqlite3ExprCanReturnSubtype`, `exprCodeTargetAndOr`, `sqlite3ExprNullRegisterRange`, `sqlite3ExprListCompare`, `sqlite3ExprAnalyzeAggregates`, `sqlite3ExprAnalyzeAggList`, `sqlite3GetTempReg`, `sqlite3ReleaseTempReg`, `sqlite3GetTempRange`, `sqlite3ReleaseTempRange`, `sqlite3ClearTempRegCache`, `sqlite3SelectOpName`, `codeOffset`, `codeDistinct`, `computeLimitRegisters`, `sqlite3SelectWrongNumTermsError`, `sqlite3SelectPopWith`, `sqlite3SelectExpand`, `assignAggregateRegisters`, `havingToWhere`, `sqlite3CopySortOrder`, `exprListIsNoAffinity`, `sqlite3WhereSplit`.

## 5. Decisions of C that V1 does not reproduce (reported, not approximated)
| C decision | Where in C | State in the port |
|---|---|---|
| sorter for ORDER BY / GROUP BY (`pushOntoSorter`, `generateSortTail`, `OP_SorterOpen` family) | select.c | unsupported ("ORDER BY that needs a sorter") |
| compound + ORDER BY with COLLATE → FROM-subquery rewrite | select.c `convertCompoundSelectToSubquery` | unsupported |
| every FROM term except the multi-row VALUES co-routine scanned plainly (no WHERE / ORDER BY / DISTINCT / aggregate over it) | select.c, where*.c | unsupported ("FROM clause term", "sqlite3WhereBegin: query planner") |
| IN against a table / index, `x IN tbl` | expr.c `sqlite3FindInIndex` step 1 | unsupported ("IN table") |
| window functions, FILTER, named windows | window.c, resolve.c | unsupported |
| recursive CTE, CTE references | select.c | unsupported (a WITH clause is parsed and resolved; any reference is a FROM term) |
| RAISE, triggers, NEW / OLD | expr.c, trigger.c | unsupported |
| result column names (`sqlite3GenerateColumnNames`) | select.c | not filled (EXPLAIN uses its own 8 names; left to the machine step) |
| authorizer callbacks during code generation | auth.c | not issued by the generator (the engine authorizes as before) |
| schema objects (`Table`, `Index`, `Column`) | build.c | no adapter yet: nothing in V1 reads a schema object; `DbInfo` carries only names and flags |
| error offset on a connection whose schema is not loaded yet | prepare.c / parse.y `cmd ::= select` → `sqlite3ReadSchema` → nested prepare resets `db->errByteOffset` | NOT reproduced: the port always reports the offset. Repro: first statement of a connection `EXPLAIN SELECT #0;` — C: no error offset (shell prints no "^--- error here"), port: offset 15. Any later statement: identical. Affects only errors raised by reduce actions while the last token is processed (register token `#N`, variable number checks on the last token, "ORDER BY clause should come after ...") |
| P4 text that is not valid UTF-8 (blob literal bytes) | vdbeaux.c `sqlite3VdbeDisplayP4` | library level exact (`sqlite3_vdbe_display_p4_bytes`); through the engine the cell is a BLOB value with the same bytes (C: TEXT), because the engine's TEXT is a `String`. Shell output identical in the default, list, column, box modes; differs in quote / insert modes (`x'41abcd42'` instead of `'A..B'`) |
