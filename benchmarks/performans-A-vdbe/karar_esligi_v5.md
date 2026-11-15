# V5 decision parity (karar eşliği): DML, triggers and foreign keys in the code generator

Scope: step V5 of the port of C SQLite 3.54.1's code generator: delete.c, insert.c (incl. INSERT … SELECT,
DEFAULT VALUES, the multi-row VALUES co-routine, xferOptimization, AUTOINCREMENT, GenerateConstraintChecks,
CompleteInsertion, OpenTableAndIndices, ComputeGeneratedColumns), upsert.c, update.c (incl. one-pass, UPDATE … FROM,
updateVirtualTable), RETURNING (build.c sqlite3AddReturning / FinishCoding, trigger.c codeReturningTrigger), trigger.c
(CREATE TRIGGER at schema load, TriggersExist, CodeRowTrigger with trigger sub-programs, TriggerColmask), fkey.c (all of
the DML side; sqlite3FkDropTable belongs to DROP TABLE, not ported), the parse rules of INSERT / UPDATE / DELETE /
UPSERT / RETURNING / CREATE TRIGGER bodies and WITH-prefixed DML, sqlite3MultiWrite / MayAbort (statement journal),
OE_* conflict resolution, change counting (OP_ResetCount, PRAGMA count_changes). Oracle: `target/perf-work/bin/sqlite3`,
the amalgamation `target/sqlite-ref/amalg`, the M2 C driver `$S/m2/drv/c/rdrv3` (lossless dump of C's prepared program).
Worktree `.claude/worktrees/vdbe` (branch `performans-A-vdbe`, uncommitted on top of f8b3ba0, Rust 1.100). Binary of the
numbers below: `$S/v5_bin/sqliters.r7` (= `$S/v5_bin/sqliters`). The engine's executor still executes every statement;
`SQLITERS_VDBE_EXEC=1` still sends only SELECT to the machine (DML on the machine: decision in the report).

## 0. How a decision is proven: the layers

EXPLAIN / EQP: accepted / identical / different (`v5/expdiff.py`, P4_VTAB address normalized). Dump layers (`wdiff.py`:
terms / loops / paths / agg / win / bidx) and the schema layer (`v3/sdiff.py`). **Program layer** (new): every D statement
of the op scripts is prepared by C (rdrv3 dumps the program it will run: every opcode, P1-P5, the full P4 incl. KeyInfo /
FuncDef / Mem / sub-programs, nMem, nCursor, nVar, usesStmtJournal, readOnly, bIsReader, nResColumn, btreeMask,
lockMask, minWriteFileFormat, expmask) and by the generator in the same connection state
(`Connection::c_generated_program`, written by `progdump::dump`); the dumps are compared text for text
(`tools/progcmp.py`). **Run layer**: the generated programs run on the machine (`Statement::set_c_program`) and stdout +
the database file are compared with C (`run5.sh`), the old path for reference (`run5off.sh`).

| corpus | EXPLAIN acc / ident / diff | EQP acc / ident / diff | paths | agg | bidx | schema |
|---|---|---|---|---|---|---|
| dml_a | 12000 / 12000 / 0 | 12000 / 12000 / 0 | 6753/6753 | 750/750 | 0/0 | 300/300 |
| dml_v | 6000 / 6000 / 0 | 6000 / 6000 / 0 | 3523/3523 | 350/350 | 434/434 | 150/150 |
| dml_t | 8000 / 8000 / 0 | 8000 / 8000 / 0 | 5004/5004 | 530/530 | 0/0 | 200/200 |
| dml_f | 8000 / 8000 / 0 | 8000 / 8000 / 0 | 4805/4805 | 504/504 | 0/0 | 200/200 |
| dml_tf | 6000 / 6000 / 0 | 6000 / 6000 / 0 | 3907/3907 | 386/386 | 0/0 | 150/150 |
| dml_r_a | 8000 / 8000 / 0 | 7972 / 7971 / 1 | 4262/4262 | 435/435 | 0/0 | 200/200 |
| dml_r_f | 8000 / 8000 / 0 | 8000 / 8000 / 0 | 3860/3860 | 390/390 | 0/0 | 200/200 |
| dml_r_t | 6000 / 6000 / 0 | 5974 / 5973 / 1 | 2788/2788 | 290/290 | 0/0 | 150/150 |
| dml_r_v | 3200 / 3200 / 0 | 3200 / 3199 / 1 | 1813/1813 | 179/179 | 214/214 | 80/80 |

The 3 EQP differences of dml_r_a / dml_r_t / dml_r_v and their "not reached" statements are the engine crash D48 (the port's shell process dies in the chunk; gorulen_farklar_v5.md).

Earlier corpora (r6): V3's 8 (ddl_a, plan_a, plan_b, agg_a/b/c, cte_a/b), V4a's win_a-d, V4b's vt_a / vt_b / vt_t / vt_x / vt_e: EXPLAIN, EQP, every layer identical; vt_d 185/187 (2 = D34, as in V4b). Files res/<corpus>_{exp,eqp,wdiff,sdiff}_r6.txt.

| op corpus | program layer SAME / C programs | run: equal scripts (machine) | run: equal scripts (old path) |
|---|---|---|---|
| ops_dml_a | 8166 / 8166 | 131/300 (8137 on the machine, 34 fallback) | 112/300 |
| ops_dml_t | 5699 / 5699 | 104/200 (5683 on the machine, 23 fallback) | 89/200 |
| ops_dml_f | 5546 / 5546 | 100/200 (5530 on the machine, 23 fallback) | 90/200 |
| ops_dml_tf | 4330 / 4330 | 66/150 (4314 on the machine, 21 fallback) | 57/150 |
| ops_dml_v | 4208 / 4208 | 57/150 (3574 on the machine, 643 fallback) | 51/150 |
| m2_dml_a | 11159 / 11159 | 200/200 (11159 on the machine, 0 fallback) | 188/200 |
| m2_dml_b | 23200 / 23200 | 398/400 (23200 on the machine, 0 fallback) | 374/400 |

Run-layer differences are classified per script by the first differing output line (`tools/firstdiv5.py`, res/firstdiv5_<dir>_r7.txt): the engine's prepare (it runs before the machine route: D43 / D44), machine defects E16 / E17, vtab writes E18, random(), page bytes D47. None is a program difference.

## 1. C function → Rust item (functions ported or completed in V5)

| C file | C function | Rust file | Rust item |
|---|---|---|---|
| delete.c | sqlite3SrcListLookup | sqliters-codegen/src/delete.rs | sqlite3_src_list_lookup |
| delete.c | sqlite3CodeChangeCount | sqliters-codegen/src/delete.rs | sqlite3_code_change_count |
| delete.c | vtabIsReadOnly | sqliters-codegen/src/delete.rs | vtab_is_read_only |
| delete.c | tabIsReadOnly | sqliters-codegen/src/delete.rs | tab_is_read_only |
| delete.c | sqlite3IsReadOnly | sqliters-codegen/src/delete.rs | sqlite3_is_read_only |
| delete.c | sqlite3MaterializeView | sqliters-codegen/src/delete.rs | sqlite3_materialize_view |
| delete.c | sqlite3DeleteFrom | sqliters-codegen/src/delete.rs | sqlite3_delete_from |
| delete.c | sqlite3GenerateRowDelete | sqliters-codegen/src/delete.rs | sqlite3_generate_row_delete |
| delete.c | sqlite3GenerateRowIndexDelete | sqliters-codegen/src/delete.rs | sqlite3_generate_row_index_delete |
| delete.c | sqlite3GenerateIndexKey | sqliters-codegen/src/delete.rs | sqlite3_generate_index_key_full |
| delete.c | sqlite3ResolvePartIdxLabel | sqliters-codegen/src/delete.rs | sqlite3_resolve_part_idx_label |
| insert.c | sqlite3TableAffinityStr | sqliters-codegen/src/insert.rs | sqlite3_table_affinity_str |
| insert.c | sqlite3TableAffinity | sqliters-codegen/src/insert.rs | sqlite3_table_affinity |
| insert.c | readsTable | sqliters-codegen/src/insert.rs | reads_table |
| insert.c | exprColumnFlagUnion | sqliters-codegen/src/insert.rs | expr_column_flag_union |
| insert.c | sqlite3ComputeGeneratedColumns | sqliters-codegen/src/insert.rs | sqlite3_compute_generated_columns |
| insert.c | autoIncBegin | sqliters-codegen/src/insert.rs | auto_inc_begin |
| insert.c | sqlite3AutoincrementBegin | sqliters-codegen/src/insert.rs | sqlite3_autoincrement_begin |
| insert.c | autoIncStep | sqliters-codegen/src/insert.rs | auto_inc_step |
| insert.c | autoIncrementEnd | sqliters-codegen/src/insert.rs | sqlite3_autoincrement_end |
| insert.c | sqlite3AutoincrementEnd | sqliters-codegen/src/insert.rs | sqlite3_autoincrement_end |
| insert.c | sqlite3Insert | sqliters-codegen/src/insert.rs | sqlite3_insert |
| insert.c | checkConstraintExprNode | sqliters-codegen/src/insert.rs | check_constraint_expr_node |
| insert.c | sqlite3ExprReferencesUpdatedColumn | sqliters-codegen/src/insert.rs | sqlite3_expr_references_updated_column |
| insert.c | indexIteratorFirst | sqliters-codegen/src/insert.rs | sqlite3_generate_constraint_checks(idx_iter) |
| insert.c | indexIteratorNext | sqliters-codegen/src/insert.rs | sqlite3_generate_constraint_checks(idx_iter) |
| insert.c | sqlite3GenerateConstraintChecks | sqliters-codegen/src/insert.rs | sqlite3_generate_constraint_checks |
| insert.c | sqlite3CompleteInsertion | sqliters-codegen/src/insert.rs | sqlite3_complete_insertion |
| insert.c | sqlite3OpenTableAndIndices | sqliters-codegen/src/delete.rs | sqlite3_open_table_and_indices |
| insert.c | xferCompatibleIndex | sqliters-codegen/src/insert.rs | xfer_compatible_index |
| insert.c | xferCheckRowid | sqliters-codegen/src/insert.rs | xfer_check_rowid |
| insert.c | xferCompatibleCheck | sqliters-codegen/src/insert.rs | xfer_compatible_check |
| insert.c | xferOptimization | sqliters-codegen/src/insert.rs | xfer_optimization |
| update.c | indexColumnIsBeingUpdated | sqliters-codegen/src/update.rs | index_column_is_being_updated |
| update.c | indexWhereClauseMightChange | sqliters-codegen/src/update.rs | index_where_clause_might_change |
| update.c | exprRowColumn | sqliters-codegen/src/update.rs | expr_row_column |
| update.c | updateFromSelect | sqliters-codegen/src/update.rs | update_from_select |
| update.c | sqlite3Update | sqliters-codegen/src/update.rs | sqlite3_update |
| update.c | updateVirtualTable | sqliters-codegen/src/update.rs | update_virtual_table |
| upsert.c | sqlite3UpsertNew | sqliters-codegen/src/upsert.rs | sqlite3_upsert_new |
| upsert.c | sqlite3UpsertDup | sqliters-codegen/src/upsert.rs | sqlite3_upsert_dup |
| upsert.c | sqlite3UpsertAnalyzeTarget | sqliters-codegen/src/upsert.rs | sqlite3_upsert_analyze_target |
| upsert.c | sqlite3UpsertNextIsIPK | sqliters-codegen/src/upsert.rs | sqlite3_upsert_next_is_ipk |
| upsert.c | sqlite3UpsertOfIndex | sqliters-codegen/src/upsert.rs | sqlite3_upsert_of_index |
| upsert.c | sqlite3UpsertDoUpdate | sqliters-codegen/src/upsert.rs | sqlite3_upsert_do_update |
| trigger.c | sqlite3TriggerList | sqliters-codegen/src/trigger.rs | sqlite3_trigger_list |
| trigger.c | sqlite3BeginTrigger | sqliters-codegen/src/trigger.rs | sqlite3_begin_trigger |
| trigger.c | sqlite3FinishTrigger | sqliters-codegen/src/trigger.rs | sqlite3_finish_trigger |
| trigger.c | triggerSpanDup | sqliters-codegen/src/trigger.rs | trigger_span_dup |
| trigger.c | sqlite3TriggerSelectStep | sqliters-codegen/src/trigger.rs | sqlite3_trigger_select_step |
| trigger.c | triggerStepAllocate | sqliters-codegen/src/trigger.rs | trigger_step_allocate |
| trigger.c | sqlite3TriggerInsertStep | sqliters-codegen/src/trigger.rs | sqlite3_trigger_insert_step |
| trigger.c | sqlite3TriggerUpdateStep | sqliters-codegen/src/trigger.rs | sqlite3_trigger_update_step |
| trigger.c | sqlite3TriggerDeleteStep | sqliters-codegen/src/trigger.rs | sqlite3_trigger_delete_step |
| trigger.c | checkColumnOverlap | sqliters-codegen/src/trigger.rs | check_column_overlap |
| trigger.c | tempTriggersExist | sqliters-codegen/src/trigger.rs | temp_triggers_exist |
| trigger.c | triggersReallyExist | sqliters-codegen/src/trigger.rs | triggers_really_exist |
| trigger.c | sqlite3TriggersExist | sqliters-codegen/src/trigger.rs | sqlite3_triggers_exist |
| trigger.c | isAsteriskTerm | sqliters-codegen/src/trigger.rs | is_asterisk_term |
| trigger.c | sqlite3ExpandReturning | sqliters-codegen/src/trigger.rs | sqlite3_expand_returning |
| trigger.c | sqlite3ReturningSubqueryVarSelect | sqliters-codegen/src/trigger.rs | returning_subquery_var_select |
| trigger.c | sqlite3ReturningSubqueryCorrelated | sqliters-codegen/src/trigger.rs | returning_subquery_correlated |
| trigger.c | sqlite3ProcessReturningSubqueries | sqliters-codegen/src/trigger.rs | sqlite3_process_returning_subqueries |
| trigger.c | codeReturningTrigger | sqliters-codegen/src/trigger.rs | code_returning_trigger |
| trigger.c | codeTriggerProgram | sqliters-codegen/src/trigger.rs | code_trigger_program |
| trigger.c | transferParseError | sqliters-codegen/src/trigger.rs | code_row_trigger(shared error state) |
| trigger.c | codeRowTrigger | sqliters-codegen/src/trigger.rs | code_row_trigger |
| trigger.c | getRowTrigger | sqliters-codegen/src/trigger.rs | get_row_trigger |
| trigger.c | sqlite3CodeRowTriggerDirect | sqliters-codegen/src/trigger.rs | sqlite3_code_row_trigger_direct |
| trigger.c | sqlite3CodeRowTrigger | sqliters-codegen/src/trigger.rs | sqlite3_code_row_trigger |
| trigger.c | sqlite3TriggerColmask | sqliters-codegen/src/trigger.rs | sqlite3_trigger_colmask |
| fkey.c | sqlite3FkLocateIndex | sqliters-codegen/src/fkey.rs | sqlite3_fk_locate_index |
| fkey.c | fkLookupParent | sqliters-codegen/src/fkey.rs | fk_lookup_parent |
| fkey.c | exprTableRegister | sqliters-codegen/src/fkey.rs | expr_table_register |
| fkey.c | exprTableColumn | sqliters-codegen/src/fkey.rs | expr_table_column |
| fkey.c | fkScanChildren | sqliters-codegen/src/fkey.rs | fk_scan_children |
| fkey.c | sqlite3FkReferences | sqliters-codegen/src/fkey.rs | sqlite3_fk_references |
| fkey.c | fkChildIsModified | sqliters-codegen/src/fkey.rs | fk_child_is_modified |
| fkey.c | fkParentIsModified | sqliters-codegen/src/fkey.rs | fk_parent_is_modified |
| fkey.c | isSetNullAction | sqliters-codegen/src/fkey.rs | is_set_null_action |
| fkey.c | sqlite3FkCheck | sqliters-codegen/src/fkey.rs | sqlite3_fk_check |
| fkey.c | sqlite3FkOldmask | sqliters-codegen/src/fkey.rs | sqlite3_fk_oldmask |
| fkey.c | sqlite3FkRequired | sqliters-codegen/src/fkey.rs | sqlite3_fk_required |
| fkey.c | fkActionTrigger | sqliters-codegen/src/fkey.rs | fk_action_trigger |
| fkey.c | sqlite3FkActions | sqliters-codegen/src/fkey.rs | sqlite3_fk_actions |
| build.c | sqlite3AddReturning | sqliters-codegen/src/trigger.rs | sqlite3_add_returning |
| build.c | sqlite3HaltConstraint | sqliters-codegen/src/build.rs | sqlite3_halt_constraint |
| build.c | sqlite3UniqueConstraint | sqliters-codegen/src/build.rs | sqlite3_unique_constraint |
| build.c | sqlite3RowidConstraint | sqliters-codegen/src/build.rs | sqlite3_rowid_constraint |
| build.c | sqlite3WritableSchema | sqliters-codegen/src/build.rs | sqlite3_writable_schema |
| expr.c | sqlite3ExprListAppendVector | sqliters-codegen/src/expr.rs | sqlite3_expr_list_append_vector |
| select.c | sameSrcAlias | sqliters-codegen/src/select.rs | same_src_alias |
| hash.c | strHash | sqliters-codegen/src/trigger.rs | str_hash |
| hash.c | insertElement | sqliters-codegen/src/trigger.rs | TrigHash::insert_element |
| hash.c | rehash | sqliters-codegen/src/trigger.rs | TrigHash::insert |
| hash.c | sqlite3HashInsert | sqliters-codegen/src/trigger.rs | TrigHash::insert |
| attach.c | sqlite3FixSrcList | sqliters-codegen/src/trigger.rs | fix_src_list |
| attach.c | sqlite3FixTriggerStep | sqliters-codegen/src/trigger.rs | fix_trigger_steps |

Also changed in functions of earlier steps (blocks; their coverage is in section 3): sqlite3FinishCoding (RETURNING
loop, OP_VBegin, AutoincrementBegin, RETURNING ephemeral table), lookupName (new.* / old.* / excluded.* / RETURNING),
resolveExprStep (TK_ROW), sqlite3ExprCodeTarget (TK_COLUMN with iSelfTab<0, TK_TRIGGER, TK_RAISE), selectInnerLoop /
generateSortTail (SRT_Upfrom), sqlite3Select (SF_UFSrcCheck / sameSrcAlias), sqlite3GetVdbe (okConstFactor only at the
top level), sqlite3ExprCompare (exprCompareVariable: expmask), sqlite3InitCallback (trigger rows), sqlite3EndTable
(pSeqTab), the DbFixer callbacks (trigger message).

## 2. Decision tables (C source → rule → Rust)

Every row: the C condition (file:line of `sqlite/src`, 3.54.1), the outcome C produces, the Rust place that takes the same
decision, and the layer that proves the outcome equal (EXPLAIN bytes = the listing incl. sub-programs; EQP; program =
the C driver's lossless dump vs the generated program's dump; run = stdout + database file through the drivers).

### 2.1 DELETE (delete.c 288–693, 745–1034) — `delete.rs`  *(layers: EXPLAIN, EQP, program, run)*

| C condition (file:line) | C outcome | Rust |
|---|---|---|
| `pTab = sqlite3SrcListLookup()` == 0 (delete.c:345) | statement ends, error of LocateTableItem / IndexedByLookup | `sqlite3_delete_from` → `sqlite3_src_list_lookup` (`is_indexed_by` → `sqlite3_indexed_by_lookup`) |
| `pTrigger \|\| sqlite3FkRequired()` (358) | `bComplex`: statement journal (BeginWriteOperation P2) and no ONEPASS_MULTIROW | `b_complex` |
| `sqlite3IsReadOnly()` (388): TF_Readonly without writable_schema / nested; TF_Shadow with defensive; virtual table without xUpdate; view without INSTEAD OF trigger (only RETURNING: `pTrigger->bReturning && pNext==0`) | "table %s may not be modified" / "cannot modify %s because it is a view" | `sqlite3_is_read_only`, `tab_is_read_only`, `vtab_is_read_only` |
| isView (428) | `sqlite3MaterializeView` into the ephemeral table iTabCur (SELECT * FROM db.view WHERE copy, SF_IncludeHidden) | `sqlite3_materialize_view` |
| `db->flags & SQLITE_CountRows && !nested && !pTriggerTab && !bReturning` (450) | `OP_Integer 0 memCnt`, at the end `sqlite3CodeChangeCount` "rows deleted" | same condition; `sqlite3_code_change_count` |
| `rcauth==SQLITE_OK && pWhere==0 && !bComplex && !IsVirtual` (471, no PREUPDATE_HOOK in the build) | truncate: `OP_Clear tnum iDb memCnt?:-1 P4=name` (rowid), per index `OP_Clear` (P3=memCnt for the PK of WITHOUT ROWID) | truncate branch; an authorizer makes the statement unsupported (rcauth not modelled) |
| `sNC.ncFlags & NC_Subquery` (497) | bComplex=1 → no ONEPASS_MULTIROW | `p.nc(snc).nc_flags & NC_Subquery` |
| HasRowid (499) / WITHOUT ROWID (505) | RowSet register / ephemeral PK index (`OP_OpenEphemeral nPk`, KeyInfo of the PK) | same |
| `eOnePass != ONEPASS_SINGLE` (532) | `sqlite3MultiWrite` | same |
| `sqlite3WhereUsesDeferredSeek` (533) | `OP_FinishSeek iTabCur` | `w.b_deferred_seek` |
| eOnePass != OFF (555) | aToOpen[] without the one-pass cursors, OP_OpenEphemeral → Noop, addrBypass | same |
| ONEPASS_OFF + PK (572) | MakeRecord(PK affinity) + IdxInsert into the ephemeral table; rowid: RowSetAdd | same |
| ONEPASS_MULTI (594) | `OP_Once` around OpenTableAndIndices (JumpHereOrPopInst) | same |
| one-pass and data cursor opened here (612) | `OP_NotFound iDataCur addrBypass iKey nKey` | same |
| IsVirtual (633) | VtabMakeWritable, MayAbort, ONEPASS_SINGLE: `OP_Close`, isMultiWrite=0 at toplevel; `OP_VUpdate 0 1 iKey P4_VTAB P5=OE_Abort` | same (`is_toplevel()`) |
| `nested==0 && pTriggerTab==0` (671) | `sqlite3AutoincrementEnd` | same |
| GenerateRowDelete: eMode==ONEPASS_OFF (773) | `OP_NotExists/NotFound iDataCur iLabel iPk nPk` | `sqlite3_generate_row_delete` |
| FkRequired \|\| pTrigger (781) | OLD.* registers (mask = TriggerColmask \| FkOldmask, 0xffffffff = all), BEFORE triggers, reseek if any code was added (821), FkCheck(regOld) | same; `sqlite3_trigger_colmask`, `sqlite3_fk_oldmask` |
| !IsView (845) | GenerateRowIndexDelete, `OP_Delete iDataCur (count?NCHANGE)`, P4_TABLE when !nested or sqlite_stat1, P5 AUXDELETE for one-pass, extra `OP_Delete iIdxNoSeek`, SAVEPOSITION for MULTI | same |
| GenerateRowIndexDelete: aRegIdx[i]==0, pIdx==pPk, iIdxCur+i==iIdxNoSeek (920–922) | index skipped | same |
| GenerateIndexKey: partial index (984) | label + ExprIfFalseDup(iSelfTab=iDataCur+1), pPrior=0 | `sqlite3_generate_index_key_full` |
| pPrior usable (998–1006) | columns shared with the previous key are not reloaded | same |
| column of the table (1008) | `sqlite3VdbeDeletePriorOpcode(OP_RealAffinity)` | same |

### 2.2 INSERT (insert.c 888–1658) — `insert.rs`  *(layers: EXPLAIN, EQP, program, run)*

| C condition | C outcome | Rust |
|---|---|---|
| single-row VALUES select (947) | pList = pEList, no co-routine | `sqlite3_insert` |
| `pColumn==0 && pSelect && !pTrigger && xferOptimization()` (1024) | transfer code, `goto insert_end` | `xfer_optimization` (2.4) |
| autoIncBegin (1038, 409–458): TF_Autoincrement && !DBFLAG_Vacuum; pSeqTab missing / not 2-column rowid table → SQLITE_CORRUPT_SEQUENCE; AutoincInfo per table at the TOPLEVEL Parse (nMem+=1, regCtr=++nMem, nMem+=2) | counter registers | `auto_inc_begin` (`toplevel_n_mem`: a trigger program allocates them in the top-level Parse) |
| IDLIST: column found / generated (1083) / rowid name (1091) / unknown (1095, `%S` = alias if any) | aTabColMap, ipkColumn, errors "cannot INSERT into generated column", "table %S has no column named %s" | same; `src_item_name` for `%S` |
| SELECT is a co-routine over one VALUES (multi-row) (1114) | reuse its registers; EQP "SCAN %S"; regData = regFromSelect when in order | same |
| else (1133) | `OP_InitCoroutine`, SRT_Coroutine select, EndCoroutine (temp-register cache cleared) | same (`sqlite3_clear_temp_reg_cache`) |
| `pTrigger \|\| readsTable()` (1161) | temp table: OpenEphemeral + Yield loop + NewRowid/Insert | `reads_table` (OP_OpenRead p3==iDb on the table or an index; OP_VOpen same vtab) |
| no IDLIST and nColumn>0 (1216): ipkColumn adjusted for generated columns; hidden count; "table %S has %d columns but %d values were supplied" (1243); "%d values for %d columns" (1250) | errors / ipkColumn | same |
| CountRows (1257) | regRowCount | same |
| !isView: OpenTableAndIndices + aRegIdx (1267) | cursors, registers per index | `sqlite3_open_table_and_indices` |
| pUpsert: virtual table / view errors (1285–1293), explicit NULLS (1294), per clause AnalyzeTarget (1305) | errors / Upsert fields (regData, cursors) | same (2.5) |
| column loop (1357–1431): IPK → SoftNull; VIRTUAL skipped; STORED → SoftNull only with BEFORE triggers; hidden / not in IDLIST / DEFAULT VALUES → ExprCodeFactorable(default); temp table → Column; select → SCopy unless in place; VALUES → ExprCodeTarget (+Copy for subqueries) | register values | same |
| BEFORE triggers (1437–1494) | regCols copy with -1 rowid, MustBeInt, generated columns, TableAffinity (not views), CodeRowTrigger BEFORE | same |
| rowid (1501–1537): VALUES NULL → NewRowid + append; NotNull/NewRowid/MustBeInt; WITHOUT ROWID / vtab → Null | rowid code | same |
| generated columns (1545) | ComputeGeneratedColumns | `sqlite3_compute_generated_columns` |
| virtual table (1554) | VtabMakeWritable, `OP_VUpdate 1 nCol+2 regIns P4_VTAB`, P5 OE_Abort for OE_Default | same |
| else: GenerateConstraintChecks, FkCheck when SQLITE_ForeignKeys (1568), `bUseSeek = isReplace==0 \|\| !sqlite3VdbeHasSubProgram(v)` (1580) | completion flags | same; HasSubProgram = toplevel && a sub-program is linked (C links at the start of codeRowTrigger, to the top-level Vdbe) |
| AFTER triggers (1600), loop end (1609–1626), AutoincrementEnd when !nested && !pTriggerTab (1635), CodeChangeCount "rows inserted" (1644) | | same |
| TableAffinity (179–224): STRICT, iReg==0 → TypeCheck before MakeRecord; else isolated TypeCheck; affinity string without trailing BLOB/NONE; iReg==0 → P4 of the MakeRecord | | `sqlite3_table_affinity` |
| ComputeGeneratedColumns (285–381): STORED → '@' in the Affinity P4 / TypeCheck P3=1; NOTAVAIL loop with BUSY; "generated column loop on" | | same |

### 2.3 Constraint checks and completion (insert.c 1891–2739, 2798–2944) — `insert.rs`, `delete.rs`

| C condition | C outcome | Rust |
|---|---|---|
| TF_HasNotNull (1957): per column, generated ones on the 2nd pass, unchanged columns of an UPDATE skipped (1976), overrideError, REPLACE without default → ABORT (1985) | `OP_NotNull`+default copy (REPLACE), `OP_HaltIfNull SQLITE_CONSTRAINT_NOTNULL onError "t.c" P5=1` (ABORT/ROLLBACK/FAIL, MayAbort for ABORT), `OP_IsNull ignoreDest` (IGNORE); 2nd pass recomputes generated columns after REPLACE | `sqlite3_generate_constraint_checks` |
| CHECK (2060): `!SQLITE_IgnoreChecks`; UPDATE skips checks not referencing changed columns (2068, ExprReferencesUpdatedColumn) | TableAffinity once, ExprIfTrue(copy) allOk, IGNORE → Goto, else HaltConstraint CHECK (REPLACE → ABORT) with the constraint name | same; `sqlite3_expr_references_updated_column` |
| upsert without target (2135): DO NOTHING → overrideError=OE_Ignore, pUpsert=0; DO UPDATE → OE_Update | | same |
| upsert with targets and indexes (2147): IndexIterator in ON CONFLICT order, then the rest | index visiting order | `idx_iter` vector |
| `flags & (RecTriggers\|ForeignKeys)` (2210): DELETE triggers (RecTriggers) or FkRequired; only kept when REPLACE may happen (2228–2236) | regTrigCnt counter, recheck labels | same |
| rowid change (2253): keyConf/override/default; UpsertOfIndex(0) (2265); IPK REPLACE deferred after other indexes (2287); UPDATE: `OP_Eq new old` (2304) | `OP_NotExists`, REPLACE: GenerateRowDelete (triggers) or GenerateRowIndexDelete; UPDATE/IGNORE: UpsertDoUpdate + Goto; default RowidConstraint | same |
| per index (2405–2697): aRegIdx==0 skip; partial WHERE (2433); key columns: XN_EXPR copy, rowid/IPK IntCopy, SCopy; PK of WITHOUT ROWID unchanged in UPDATE (2477); OE_None (2484); upsert clause (2495); the single-PK REPLACE shortcut without triggers / FK (2515); `OP_NoConflict`; IdxRowid / PK columns; UPDATE comparisons `OP_Eq`/`OP_Ne` with the PK collations (2567–2586); actions; recheck copy of the conflict code (2660–2681) | | same (collation P4 from `sqlite3_locate_coll_seq`) |
| IPK REPLACE last (2700), recheck after replace triggers (2710), table record `OP_MakeRecord … nNVCol` + affinity when not done (2728) | | same |
| CompleteInsertion (2798): partial / expression index in UPDATE → `OP_IsNull +2`; PK of WITHOUT ROWID: NCHANGE + SAVEPOSITION; `OP_IdxInsert … nKeyCol\|nColumn`; table: `OP_Insert` P5 NCHANGE\|(update_flags or LASTROWID)\|APPEND\|USESEEKRESULT, P4_TABLE unless nested | | `sqlite3_complete_insertion` |
| OpenTableAndIndices (2889): vtab → -999; aToOpen; PK of WITHOUT ROWID is the data cursor (p5=0) | | `sqlite3_open_table_and_indices` |

### 2.4 Transfer optimization (insert.c 2970–3472) — `insert.rs xfer_optimization`

| C condition | C outcome | Rust |
|---|---|---|
| WITH on the parse or the select (3093), virtual dest (3100), onError default from keyConf (3104), select shape (3109–3143: one FROM table, no subquery / WHERE / ORDER BY / GROUP BY / LIMIT / compound / DISTINCT, `*` only) | return 0 | same |
| source table: missing, same table, rowid mismatch, not ordinary, nCol, iPKey, STRICT types (3150–3183), per column generated / expr / affinity / collation / notnull / default text (3184–3254), every dest index has a compatible source index and not the same storage (3255–3272), CHECK compatible (3274, xferCompatibleCheck incl. rowid references), FK on dest with foreign_keys (3290), count_changes (3294) | return 0 | same; `xfer_compatible_index`, `xfer_compatible_check`, `xfer_check_rowid` |
| code (3317–3472): CodeVerifySchema(src db), cursors, autoinc, `OP_Rewind/Goto` empty-dest test when (IPK<0 && indexes) \|\| unique index \|\| onError not ABORT/ROLLBACK; rowid copy loop with RowCell + Insert(PREFORMAT…); per index OpenRead/OpenWrite(BULKCSR) + RowData/IdxInsert; empty dest → AutoincrementEnd + `OP_Halt` + fall back to the normal INSERT | | same (VACUUM branches kept, DBFLAG_Vacuum never set by the engine) |

### 2.5 UPSERT (upsert.c 55–328) — `upsert.rs`

| C condition | C outcome | Rust |
|---|---|---|
| AnalyzeTarget (90–220): resolve target + WHERE; rowid target (129); per UNIQUE index with nKeyCol columns, partial index WHERE must equal (ExprCompare with pParse), each index column (collation via a TK_COLLATE node, expression column) must match some target term (`<2`) | pUpsertIdx, isDup when an earlier clause has the index; "%r ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint" ("" for a single clause) | `sqlite3_upsert_analyze_target` (`ordinal` = `%r`) |
| NextIsIPK (227), OfIndex (247) | clause order | same |
| DoUpdate (267): conflict found through another index: rowid tables `OP_IdxRowid` + `OP_SeekRowid`, WITHOUT ROWID PK columns + `OP_Found` / `OP_Halt SQLITE_CORRUPT "corrupt database"`; REAL columns of excluded.* `OP_RealAffinity`; sqlite3Update(copy of the SrcList) | | `sqlite3_upsert_do_update` |
| lookupName `excluded.*` (resolve.c:547–585): iTable = regData + storage column, TK_REGISTER | | `resolve.rs lookup_name` |

### 2.6 UPDATE (update.c 296–1175, 1208–1373) — `update.rs`

| C condition | C outcome | Rust |
|---|---|---|
| nChangeFrom = nSrc>1 ? nExpr : 0 (406) | UPDATE … FROM path | same |
| cursors (431–447): PK index cursor is the data cursor; upsert reuses INSERT's cursors | | same |
| SET resolution (478–529): IPK → chngRowid; PK column of WITHOUT ROWID → chngPk; generated → error; "rowid" names; unknown → "no such column: %s" | | same |
| generated columns depending on changed ones (543–561) | aXRef=99999 | same |
| hasFK = FkRequired(aXRef, chngKey) (571) | | `sqlite3_fk_required` |
| aRegIdx (578–601): key / FK>1 / PK / partial WHERE might change → registers; else only when a key column changes (OE_Default+REPLACE index → bReplace) | aToOpen | same; `index_where_clause_might_change`, `index_column_is_being_updated` |
| registers (612–630), view materialized (641), WHERE resolved (653), virtual table → updateVirtualTable (659) | | same |
| CountRows without upsert (671) | | same |
| rowid ephemeral `OP_Null` + `OP_OpenEphemeral iEph 0 regRowSet` (681) / PK ephemeral with KeyInfo nAllField=nEphCol (685–703), FROM: updateFromSelect (705) | | same; `update_from_select` (TK_ROW columns, SRT_Upfrom / SRT_Table, SF_UFSrcCheck\|IncludeHidden\|UpdateFrom, SF_OrderByReqd) |
| upsert: ExprIfFalse(WHERE) ONEPASS_SINGLE (721) | | same |
| WhereBegin ONEPASS_DESIRED (+MULTIROW when no trigger / FK / key change / replace / subquery) (743), MULTI → OFF when the index cursor needs updating (770) | | same |
| loop forms (814–891): one-pass NotFound/IsNull; PK/FROM Rewind loops; rowid Rewind + NotExists | | same |
| new rowid (898), OLD.* (911–935), NEW.* (950–982) with newmask, generated (984), BEFORE triggers + reseek + reload (994–1037) | | same |
| constraint checks, reseek, FkCheck old, index delete, `OP_FinishSeek` (bFinishSeek), `OP_Delete` when hasFK>1 \|\| chngKey (no PREUPDATE_HOOK), FkCheck new, CompleteInsertion(ISUPDATE\|SAVEPOSITION for MULTI), FkActions (1040–1121) | | same |
| loop end, AutoincrementEnd, CodeChangeCount "rows updated" (1137–1162) | | same |
| updateVirtualTable (1208–1373): ephemeral of nArg; FROM via updateFromSelect with OPFLAG_NOCHNG columns; else Where ONEPASS, VColumn NOCHNG, rowid / PK; one-pass → Noop + Close; else MakeRecord/NewRowid/Insert; `OP_VUpdate 0 nArg regArg` | | `update_virtual_table` |

### 2.7 RETURNING (build.c 1474–1540, 160–260; trigger.c 897–1109) — `trigger.rs`, `build.rs`

| C condition | C outcome | Rust |
|---|---|---|
| AddReturning: `pParse->pNewTrigger` (1503) | "cannot use RETURNING in a trigger" | `sqlite3_add_returning` |
| retTrig inserted into the TEMP schema's trigHash (1536) | position in the hash list (TriggerList walks it) | `TrigHash::insert` after loading the schema when C has it loaded |
| TriggerList: retTrig with op TK_RETURNING joins any table, then keeps its table (trigger.c:68–79) | | `sqlite3_trigger_list` |
| triggersReallyExist: first use sets op; virtual tables: DELETE/UPDATE error, tr_tm BEFORE; else AFTER (845–860); UPSERT update fires an INSERT RETURNING at toplevel (861) | | `triggers_really_exist` |
| codeReturningTrigger (1026–1109): only at toplevel and for this statement's retTrig; SelectPrep of a one-table select (assigns a cursor), GenerateColumnNames, ExpandReturning (`*` without hidden columns, "RETURNING may not use \"TABLE.*\" wildcards"), nRetCol/iRetCur once, NC_UBaseReg resolution, correlated subqueries → EP_VarSelect, ExprCodeFactorable + RealAffinity, MakeRecord/NewRowid/Insert | | `code_returning_trigger` |
| FinishCoding (172–192, 253–260): `OP_FkCheck`, Rewind/Column/ResultRow/Next before OP_Halt; `OP_OpenEphemeral iRetCur nRetCol` in the init block | | `sqlite3_finish_coding` |
| lookupName in RETURNING (resolve.c:528–536, 591–596): table name / schema-table name / none → TK_REGISTER iBaseReg+(nCol+1)*iTable+storage+1 | | `resolve.rs` |

### 2.8 Triggers (trigger.c 50–894, 1117–1579; parse.y trigger rules) — `trigger.rs`, `parse.rs` 260–277

| C condition | C outcome | Rust |
|---|---|---|
| CREATE TRIGGER at schema load (BeginTrigger 104–318): TEMP qualified name error; TwoPartName; init: drop the table's db name for non-TEMP (155); FixSrcList; table missing / virtual / shadow / system table / view-without-INSTEAD / INSTEAD-on-table errors; INSTEAD → BEFORE; WHEN dup REDUCE | Trigger object | `sqlite3_begin_trigger` (non-init CREATE TRIGGER stays "statement kind") |
| FinishTrigger (324–429): step count limit, FixTriggerStep + FixExpr (fixer: EP_FromDDL unless TEMP, variables → NULL, items fixed to the trigger's schema, "trigger … cannot reference objects in database"), trigHash insert, link to the table when the trigger and table schema are the same (newest first) | | `sqlite3_finish_trigger`, `fix_trigger_steps` |
| steps (435–641): span with spaces normalized; qualified table names error unless TEMP; UPDATE FROM nested-from subquery; upsert explicit NULLS | | `sqlite3_trigger_*_step`, `trigger_span_dup` |
| TriggersExist (875): no table triggers and no TEMP triggers, or disableTriggers → none; EnableTrigger off keeps TEMP ones only (827) | mask of BEFORE/AFTER | `sqlite3_triggers_exist` |
| codeRowTrigger (1237–1359): depth ≥ SQLITE_LIMIT_TRIGGER_DEPTH → "triggers nested too deep"; TriggerPrg linked at the top level first (aColmask all ones while coding); sub-Parse with pTriggerTab, eTriggerOp, no okConstFactor, explain 0; OP_Init P4 "-- TRIGGER name"; WHEN → ExprIfFalse to the end; steps with `OP_Trace "-- span"`, eOrconf, ResetCount after DML steps, SRT_Discard for SELECT; OP_Halt; oldmask/newmask | SubProgram (nMem, nCsr, token) | `code_row_trigger` (Parse fields of the sub-parse swapped: `SubParseState`), `code_trigger_program` |
| getRowTrigger (1367): reuse by (trigger, orconf); errByteOffset=-1 | | `get_row_trigger` |
| CodeRowTriggerDirect (1402): `OP_Program reg ignoreJump ++nMem P4_SUBPROGRAM`, P5 = named trigger && !RecTriggers | | `sqlite3_code_row_trigger_direct` |
| CodeRowTrigger (1474): op match or RETURNING-of-UPSERT, tr_tm, column overlap; RETURNING only at toplevel | | `sqlite3_code_row_trigger` |
| TriggerColmask (1544): views all ones; RETURNING all ones; else program masks | | `sqlite3_trigger_colmask` |
| TK_TRIGGER (expr.c 5676–5733): `OP_Param iTable*(nCol+1)+1+storage`, RealAffinity | | `exprcode.rs` |
| TK_RAISE (expr.c 5866–5891): outside trigger → error; ABORT → MayAbort; IGNORE → `OP_Halt 0 OE_Ignore`; else `OP_Halt CONSTRAINT_TRIGGER/ERROR type r1` | | `exprcode.rs` |
| sub-program objects: pointer identity of the P4 operands | listing order, recursion | `finish_sub_programs` (one Arc per program, stubs for references made while the program was being coded; listing resolves stubs) |

### 2.9 Foreign keys (fkey.c 183–1442) — `fkey.rs`

| C condition | C outcome | Rust |
|---|---|---|
| FkLocateIndex (183–292): single column mapping to the IPK; UNIQUE non-partial index with matching columns and default collations; PK when no column list; mismatch → "foreign key mismatch - \"%w\" referencing \"%w\"" unless disableTriggers | | `sqlite3_fk_locate_index` |
| fkLookupParent (320–460): FkIfZero for nIncr<0; IsNull per child column; IPK: SCopy/MustBeInt, self-reference Eq, OpenRead + NotExists; index: Copy, self-reference Ne chain, Affinity + Found; immediate single-statement case → HaltConstraint FOREIGNKEY; else MayAbort (nIncr>0, immediate) + FkCounter | | `fk_lookup_parent` |
| fkScanChildren (547–660): parent-register = child-column WHERE (collation of the parent column, exprTableRegister), self-reference exclusion (rowid Ne / NOT(… IS …)), WhereBegin + FkCounter, FkIfZero for nIncr<0 | | `fk_scan_children` |
| FkCheck (889–1087): child keys (skip unmodified in UPDATE unless self-reference), LocateTable / FindTable (disableTriggers), nTab++, old/new lookups (no new lookup inside the SET NULL action of the same FK); parent keys: skip unmodified, skip immediate single-statement, child SrcList scans, MayAbort unless deferred or CASCADE/SET NULL | | `sqlite3_fk_check`, `is_set_null_action` |
| FkOldmask (1095), FkRequired (1145): DELETE → child or parent; UPDATE → modified child (2 for self-reference) or parent with an ON UPDATE action (2) | | `sqlite3_fk_oldmask`, `sqlite3_fk_required` |
| fkActionTrigger (1217–1413): FkNoAction; RESTRICT with DeferFKs → none; WHERE old.to = from, WHEN NOT(old IS new) for UPDATE, SET list (CASCADE new.to / SET DEFAULT column default / NULL), RESTRICT → SELECT RAISE(ABORT,'FOREIGN KEY constraint failed'); step op DELETE/UPDATE/SELECT; trigger without name (P5 0, no "-- TRIGGER") | cached per FKey | `fk_action_trigger` (cached per statement) |
| FkActions (1419): for parent keys modified (or DELETE) → CodeRowTriggerDirect(OE_Abort) | | `sqlite3_fk_actions` |
| sqlite3FkReferences (676): the fkeyHash list, newest first | | `sqlite3_fk_references` (tables in schema order, newest first) |

### 2.10 Parse rules (parse.y DELETE / UPDATE / INSERT / UPSERT / RETURNING / WITH / trigger) — `parse.rs`

| rule | C action | Rust |
|---|---|---|
| 152 DELETE (no LIMIT variant: SQLITE_ENABLE_UPDATE_DELETE_LIMIT is not in the build) | IndexedBy + DeleteFrom | 152 |
| 157/158 where_opt_ret RETURNING | AddReturning | 157/158 |
| 159 UPDATE (+FROM: nested-from subquery for nSrc>1, AppendList), set list length check | Update | 159 |
| 160–163 setlist (vector form: ExprListAppendVector, "%d columns assigned %d values") | | 160–163, `sqlite3_expr_list_append_vector` |
| 164/165 INSERT, 166–172 upsert / returning, 173/174 insert_cmd, 76 orconf | | same |
| 309/310 `with` prefix of DML | WithPush | same |
| 260–277 CREATE TRIGGER and trigger commands (tridxby errors) | | same (schema load) |
| any DML when the connection has an authorizer | (sqlite3AuthCheck decisions) | reported unsupported (`dml_supported`) |


## 3. Coverage of the ported C functions (gcov, -O0 build of the reference amalgamation)

Corpus: the 65200 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
|---|---|---|
| delete.c | 297 / 306 (97.1%) | 216 / 236 (91.5%) |
| insert.c | 1117 / 1161 (96.2%) | 827 / 913 (90.6%) |
| update.c | 463 / 475 (97.5%) | 381 / 402 (94.8%) |
| upsert.c | 118 / 129 (91.5%) | 75 / 82 (91.5%) |
| trigger.c | 476 / 561 (84.8%) | 261 / 370 (70.5%) |
| fkey.c | 380 / 400 (95.0%) | 298 / 339 (87.9%) |
| build.c | 59 / 63 (93.7%) | 16 / 20 (80.0%) |
| expr.c | 22 / 24 (91.7%) | 14 / 22 (63.6%) |
| select.c | 9 / 11 (81.8%) | 11 / 14 (78.6%) |
| hash.c | 58 / 59 (98.3%) | 30 / 34 (88.2%) |
| attach.c | 16 / 22 (72.7%) | 9 / 22 (40.9%) |
| **total** | 3015 / 3211 (93.9%) | 2138 / 2454 (87.1%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| sqlite3SrcListLookup | delete.c | 11 | 100.0 | 8 | 100.0 | 0 |
| sqlite3CodeChangeCount | delete.c | 6 | 100.0 | 0 | 100.0 | 0 |
| vtabIsReadOnly | delete.c | 8 | 50.0 | 8 | 37.5 | 3 |
| tabIsReadOnly | delete.c | 8 | 100.0 | 10 | 90.0 | 1 |
| sqlite3IsReadOnly | delete.c | 9 | 100.0 | 10 | 100.0 | 0 |
| sqlite3MaterializeView | delete.c | 13 | 100.0 | 2 | 50.0 | 1 |
| sqlite3DeleteFrom | delete.c | 163 | 96.9 | 118 | 92.4 | 9 |
| sqlite3GenerateRowDelete | delete.c | 40 | 100.0 | 34 | 88.2 | 2 |
| sqlite3GenerateRowIndexDelete | delete.c | 19 | 100.0 | 18 | 100.0 | 0 |
| sqlite3GenerateIndexKey | delete.c | 25 | 100.0 | 26 | 100.0 | 0 |
| sqlite3ResolvePartIdxLabel | delete.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3TableAffinityStr | insert.c | 9 | 100.0 | 10 | 90.0 | 1 |
| sqlite3TableAffinity | insert.c | 24 | 91.7 | 12 | 91.7 | 1 |
| readsTable | insert.c | 16 | 100.0 | 18 | 94.4 | 1 |
| exprColumnFlagUnion | insert.c | 4 | 100.0 | 4 | 100.0 | 0 |
| sqlite3ComputeGeneratedColumns | insert.c | 43 | 100.0 | 28 | 96.4 | 1 |
| autoIncBegin | insert.c | 31 | 90.3 | 24 | 75.0 | 6 |
| sqlite3AutoincrementBegin | insert.c | 24 | 100.0 | 6 | 83.3 | 1 |
| autoIncStep | insert.c | 4 | 100.0 | 2 | 100.0 | 0 |
| autoIncrementEnd | insert.c | 20 | 100.0 | 4 | 75.0 | 1 |
| sqlite3AutoincrementEnd | insert.c | 3 | 100.0 | 2 | 100.0 | 0 |
| sqlite3Insert | insert.c | 315 | 97.5 | 232 | 93.5 | 14 |
| checkConstraintExprNode | insert.c | 7 | 100.0 | 6 | 100.0 | 0 |
| sqlite3ExprReferencesUpdatedColumn | insert.c | 9 | 100.0 | 2 | 100.0 | 0 |
| indexIteratorFirst | insert.c | 6 | 100.0 | 2 | 100.0 | 0 |
| indexIteratorNext | insert.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3GenerateConstraintChecks | insert.c | 358 | 98.9 | 293 | 95.9 | 10 |
| sqlite3CompleteInsertion | insert.c | 27 | 100.0 | 30 | 100.0 | 0 |
| sqlite3OpenTableAndIndices | insert.c | 25 | 96.0 | 24 | 95.8 | 1 |
| xferCompatibleIndex | insert.c | 19 | 94.7 | 20 | 95.0 | 1 |
| xferCheckRowid | insert.c | 5 | 100.0 | 4 | 100.0 | 0 |
| xferCompatibleCheck | insert.c | 9 | 88.9 | 6 | 83.3 | 1 |
| xferOptimization | insert.c | 192 | 87.5 | 180 | 75.6 | 30 |
| indexColumnIsBeingUpdated | update.c | 5 | 100.0 | 2 | 100.0 | 0 |
| indexWhereClauseMightChange | update.c | 3 | 100.0 | 2 | 100.0 | 0 |
| exprRowColumn | update.c | 4 | 100.0 | 2 | 50.0 | 1 |
| updateFromSelect | update.c | 36 | 100.0 | 22 | 81.8 | 4 |
| sqlite3Update | update.c | 353 | 99.4 | 338 | 97.3 | 9 |
| updateVirtualTable | update.c | 74 | 86.5 | 36 | 80.6 | 6 |
| sqlite3UpsertNew | upsert.c | 16 | 62.5 | 2 | 50.0 | 1 |
| sqlite3UpsertDup | upsert.c | 7 | 28.6 | 2 | 50.0 | 1 |
| sqlite3UpsertAnalyzeTarget | upsert.c | 61 | 100.0 | 50 | 92.0 | 4 |
| sqlite3UpsertNextIsIPK | upsert.c | 8 | 100.0 | 10 | 90.0 | 1 |
| sqlite3UpsertOfIndex | upsert.c | 6 | 100.0 | 6 | 100.0 | 0 |
| sqlite3UpsertDoUpdate | upsert.c | 31 | 100.0 | 12 | 100.0 | 0 |
| sqlite3TriggerList | trigger.c | 19 | 100.0 | 14 | 71.4 | 2 |
| sqlite3BeginTrigger | trigger.c | 100 | 63.0 | 84 | 56.0 | 28 |
| sqlite3FinishTrigger | trigger.c | 53 | 77.4 | 32 | 53.1 | 11 |
| triggerSpanDup | trigger.c | 4 | 100.0 | 6 | 83.3 | 1 |
| sqlite3TriggerSelectStep | trigger.c | 10 | 80.0 | 2 | 50.0 | 1 |
| triggerStepAllocate | trigger.c | 20 | 80.0 | 14 | 57.1 | 5 |
| sqlite3TriggerInsertStep | trigger.c | 17 | 70.6 | 6 | 50.0 | 3 |
| sqlite3TriggerUpdateStep | trigger.c | 27 | 63.0 | 12 | 33.3 | 4 |
| sqlite3TriggerDeleteStep | trigger.c | 11 | 81.8 | 4 | 50.0 | 2 |
| checkColumnOverlap | trigger.c | 5 | 100.0 | 8 | 87.5 | 1 |
| tempTriggersExist | trigger.c | 4 | 100.0 | 4 | 75.0 | 1 |
| triggersReallyExist | trigger.c | 34 | 76.5 | 40 | 72.5 | 5 |
| sqlite3TriggersExist | trigger.c | 6 | 100.0 | 8 | 87.5 | 1 |
| isAsteriskTerm | trigger.c | 6 | 100.0 | 6 | 83.3 | 1 |
| sqlite3ExpandReturning | trigger.c | 22 | 100.0 | 16 | 75.0 | 3 |
| sqlite3ReturningSubqueryVarSelect | trigger.c | 5 | 100.0 | 4 | 75.0 | 1 |
| sqlite3ReturningSubqueryCorrelated | trigger.c | 8 | 100.0 | 4 | 100.0 | 0 |
| sqlite3ProcessReturningSubqueries | trigger.c | 11 | 100.0 | 2 | 100.0 | 0 |
| codeReturningTrigger | trigger.c | 50 | 96.0 | 18 | 77.8 | 4 |
| codeTriggerProgram | trigger.c | 38 | 100.0 | 10 | 100.0 | 0 |
| transferParseError | trigger.c | 7 | 100.0 | 2 | 100.0 | 0 |
| codeRowTrigger | trigger.c | 61 | 95.1 | 24 | 75.0 | 6 |
| getRowTrigger | trigger.c | 9 | 100.0 | 10 | 100.0 | 0 |
| sqlite3CodeRowTriggerDirect | trigger.c | 9 | 100.0 | 6 | 83.3 | 1 |
| sqlite3CodeRowTrigger | trigger.c | 10 | 100.0 | 18 | 100.0 | 0 |
| sqlite3TriggerColmask | trigger.c | 15 | 100.0 | 16 | 93.8 | 1 |
| sqlite3FkLocateIndex | fkey.c | 42 | 97.6 | 50 | 90.0 | 4 |
| fkLookupParent | fkey.c | 57 | 98.2 | 34 | 91.2 | 3 |
| exprTableRegister | fkey.c | 14 | 100.0 | 8 | 87.5 | 1 |
| exprTableColumn | fkey.c | 7 | 100.0 | 2 | 50.0 | 1 |
| fkScanChildren | fkey.c | 42 | 100.0 | 22 | 95.5 | 1 |
| sqlite3FkReferences | fkey.c | 2 | 100.0 | 0 | 100.0 | 0 |
| fkChildIsModified | fkey.c | 6 | 100.0 | 8 | 87.5 | 1 |
| fkParentIsModified | fkey.c | 11 | 100.0 | 16 | 100.0 | 0 |
| isSetNullAction | fkey.c | 8 | 100.0 | 12 | 100.0 | 0 |
| sqlite3FkCheck | fkey.c | 76 | 81.6 | 76 | 77.6 | 10 |
| sqlite3FkOldmask | fkey.c | 12 | 100.0 | 18 | 88.9 | 2 |
| sqlite3FkRequired | fkey.c | 17 | 100.0 | 26 | 96.2 | 1 |
| fkActionTrigger | fkey.c | 98 | 95.9 | 57 | 84.2 | 9 |
| sqlite3FkActions | fkey.c | 8 | 100.0 | 10 | 100.0 | 0 |
| sqlite3AddReturning | build.c | 29 | 86.2 | 8 | 50.0 | 4 |
| sqlite3HaltConstraint | build.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3UniqueConstraint | build.c | 16 | 100.0 | 8 | 100.0 | 0 |
| sqlite3RowidConstraint | build.c | 9 | 100.0 | 2 | 100.0 | 0 |
| sqlite3WritableSchema | build.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3ExprListAppendVector | expr.c | 24 | 91.7 | 22 | 63.6 | 7 |
| sameSrcAlias | select.c | 11 | 81.8 | 14 | 78.6 | 3 |
| strHash | hash.c | 6 | 100.0 | 2 | 100.0 | 0 |
| insertElement | hash.c | 17 | 100.0 | 10 | 100.0 | 0 |
| rehash | hash.c | 16 | 93.8 | 8 | 62.5 | 3 |
| sqlite3HashInsert | hash.c | 20 | 100.0 | 14 | 92.9 | 1 |
| sqlite3FixSrcList | attach.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3FixTriggerStep | attach.c | 15 | 60.0 | 20 | 35.0 | 9 |

## DML / trigger blocks inside functions ported by earlier steps

| C file | block | lines | lines covered | branches | taken at least once | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| build.c | sqlite3FinishCoding: RETURNING output loop | 126953-126974 | 11 / 11 | 6 | 5 | 1 |
| build.c | sqlite3FinishCoding: OP_VBegin | 127004-127007 | 3 / 3 | 2 | 2 | 0 |
| build.c | sqlite3FinishCoding: RETURNING ephemeral table | 127034-127041 | 4 / 4 | 4 | 3 | 1 |
| resolve.c | lookupName: new.* / old.* / excluded.* / RETURNING | 111018-111113 | 50 / 54 | 62 | 48 | 9 |
| resolve.c | resolveExprStep: TK_ROW | 111516-111528 | 9 / 9 | 0 | 0 | 0 |
| expr.c | sqlite3ExprCodeTarget: TK_COLUMN with iSelfTab<0 | 118027-118075 | 22 / 22 | 12 | 12 | 0 |
| expr.c | sqlite3ExprCodeTarget: TK_TRIGGER | 118542-118600 | 9 / 9 | 4 | 4 | 0 |
| expr.c | sqlite3ExprCodeTarget: TK_RAISE | 118732-118757 | 11 / 13 | 10 | 6 | 2 |
| select.c | selectInnerLoop: SRT_Upfrom | 150956-150978 | 11 / 12 | 4 | 3 | 1 |
| select.c | generateSortTail: SRT_Upfrom | 151454-151464 | 0 / 8 | 2 | 0 | 1 |
| select.c | sqlite3Select: SF_UFSrcCheck | 157375-157390 | 4 / 7 | 6 | 3 | 2 |


### 3.1 The lines with a branch outcome never seen, by reason (`tools/gapcls.py`, every line in karar_gap_notes_v5.txt)

| reason | lines |
|---|---|
| not verified: decision outcome not met by the corpora (see the line) | 132 |
| memory allocation failure (mallocFailed / NULL from an allocator) | 47 |
| CREATE TRIGGER error / allocation paths: the generator loads only schemas C accepted (schema load) | 41 |
| ALTER TABLE rename (IN_RENAME_OBJECT: not a ported statement kind) | 10 |
| VACUUM only (DBFLAG_Vacuum / VacuumInto: the engine runs VACUUM itself) | 9 |
| nested parse (sqlite3NestedParse: DDL / ANALYZE code, not ported) | 8 |
| authorizer (db->xAuth: DML of such a connection is reported unsupported) | 7 |
| PRAGMA defer_foreign_keys / FkNoAction (SQLITE_DeferFKs, SQLITE_FkNoAction: not settable through the engine) | 6 |
| disableTriggers (sqlite3FkDropTable: DROP TABLE is not ported) | 3 |
| EnableTrigger / QPSG / IgnoreChecks connection flags (defaults only) | 2 |
| CREATE TRIGGER outside schema load (DDL statement: not ported) | 2 |
| shadow tables / defensive mode / writable_schema (not set in the corpora) | 2 |
| limits (trigger depth / steps / expression depth) | 2 |

The lines "not verified" (the corpora never met the other outcome; the decision is ported 1:1 but not proven by a differential):

```
vtabIsReadOnly (delete.c)  sqlite3.c:133305  if( sqlite3GetVTable(pParse->db, pTab)->pMod->pModule->xUpdate==0 ){
vtabIsReadOnly (delete.c)  sqlite3.c:133315  if( (pParse->pToplevel!=0 || (pParse->prepFlags & SQLITE_PREPARE_FROM_DDL))
vtabIsReadOnly (delete.c)  sqlite3.c:133317  ((pParse->db->flags & SQLITE_TrustedSchema)!=0)
sqlite3MaterializeView (delete.c)  sqlite3.c:133383  if( pFrom ){
sqlite3DeleteFrom (delete.c)  sqlite3.c:133622  if( rcauth==SQLITE_DENY ){
sqlite3DeleteFrom (delete.c)  sqlite3.c:133697  if( rcauth==SQLITE_OK
sqlite3DeleteFrom (delete.c)  sqlite3.c:133845  if( IsVirtual(pTab) ){
sqlite3DeleteFrom (delete.c)  sqlite3.c:133866  if( sqlite3IsToplevel(pParse) ){
sqlite3GenerateRowDelete (delete.c)  sqlite3.c:134027  if( mask==0xffffffff || (iCol<=31 && (mask & MASKBIT32(iCol))!=0) ){
sqlite3TableAffinityStr (insert.c)  sqlite3.c:139457  if( zColAff ){
sqlite3TableAffinity (insert.c)  sqlite3.c:139539  if( !zColAff ){
readsTable (insert.c)  sqlite3.c:139575  if( pOp->opcode==OP_OpenRead && pOp->p3==iDb ){
sqlite3ComputeGeneratedColumns (insert.c)  sqlite3.c:139655  }else if( pOp->opcode==OP_TypeCheck ){
autoIncBegin (insert.c)  sqlite3.c:139758  if( pSeqTab==0
autoIncBegin (insert.c)  sqlite3.c:139759  || !HasRowid(pSeqTab)
autoIncBegin (insert.c)  sqlite3.c:139761  || pSeqTab->nCol!=2
sqlite3AutoincrementBegin (insert.c)  sqlite3.c:139833  if( aOp==0 ) break;
autoIncrementEnd (insert.c)  sqlite3.c:139898  if( aOp==0 ) break;
sqlite3Insert (insert.c)  sqlite3.c:140279  if( pSelect && (pSelect->selFlags & SF_Values)!=0 && pSelect->pPrior==0 ){
sqlite3Insert (insert.c)  sqlite3.c:140405  if( aTabColMap==0 ) goto insert_cleanup;
sqlite3Insert (insert.c)  sqlite3.c:140409  if( aTabColMap[j]==0 ) aTabColMap[j] = i+1;
sqlite3Insert (insert.c)  sqlite3.c:140423  if( sqlite3IsRowid(pColumn->a[i].zName) && !withoutRowid ){
sqlite3Insert (insert.c)  sqlite3.c:140448  && pSelect->pPrior==0
sqlite3Insert (insert.c)  sqlite3.c:140476  if( rc || pParse->nErr ) goto insert_cleanup;
sqlite3Insert (insert.c)  sqlite3.c:140536  if( sqlite3ResolveExprListNames(&sNC, pList) ){
sqlite3Insert (insert.c)  sqlite3.c:140626  if( sqlite3HasExplicitNulls(pParse, pUpsert->pUpsertTarget) ){
sqlite3Insert (insert.c)  sqlite3.c:140760  ExprHasProperty(pX, EP_Subquery) ? OP_Copy : OP_SCopy, y, iRegStore);
sqlite3Insert (insert.c)  sqlite3.c:140841  if( pIpk->op==TK_NULL && !IsVirtual(pTab) ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141304  if( isGenerated && !b2ndPass ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141308  if( aiChng && aiChng[i]<0 && !isGenerated ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141328  }else if( b2ndPass && !isGenerated ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141369  if( nGenerated==0 && nSeenReplace==0 ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141378  if( nSeenReplace>0 && (pTab->tabFlags & TF_HasGenerated)!=0 ){
sqlite3GenerateConstraintChecks (insert.c)  sqlite3.c:141623  if( upsertIpkDelay ){
sqlite3OpenTableAndIndices (insert.c)  sqlite3.c:142256  }else if( pParse->db->noSharedCache==0 ){
xferCompatibleIndex (insert.c)  sqlite3.c:142326  if( sqlite3_stricmp(pSrc->azColl[i],pDest->azColl[i])!=0 ){
xferCompatibleCheck (insert.c)  sqlite3.c:142363  if( sqlite3ExprListCompare(pSrc->pCheck,pDest->pCheck,-1) ){
xferOptimization (insert.c)  sqlite3.c:142425  if( pParse->pWith || pSelect->pWith ){
xferOptimization (insert.c)  sqlite3.c:142455  if( pSelect->pGroupBy ){
xferOptimization (insert.c)  sqlite3.c:142458  if( pSelect->pLimit ){
xferOptimization (insert.c)  sqlite3.c:142461  if( pSelect->pPrior ){
xferOptimization (insert.c)  sqlite3.c:142464  if( pSelect->selFlags & SF_Distinct ){
xferOptimization (insert.c)  sqlite3.c:142486  if( pSrc->tnum==pDest->tnum && pSrc->pSchema==pDest->pSchema ){
xferOptimization (insert.c)  sqlite3.c:142511  if( eDestType==COLTYPE_INT && eSrcType==COLTYPE_INTEGER ) continue;
xferOptimization (insert.c)  sqlite3.c:142512  if( eDestType==COLTYPE_INTEGER && eSrcType==COLTYPE_INT ) continue;
xferOptimization (insert.c)  sqlite3.c:142544  (pSrcCol->colFlags & COLFLAG_GENERATED) ){
xferOptimization (insert.c)  sqlite3.c:142552  if( sqlite3ExprCompare(0,
xferOptimization (insert.c)  sqlite3.c:142581  pSrcExpr->u.zToken)!=0)
xferOptimization (insert.c)  sqlite3.c:142597  if( pSrcIdx->tnum==pDestIdx->tnum && pSrc->pSchema==pDest->pSchema
xferOptimization (insert.c)  sqlite3.c:142598  && sqlite3FaultSim(411)==SQLITE_OK ){
xferOptimization (insert.c)  sqlite3.c:142633  for(i=0; i<pSrc->nCol; i++){
xferOptimization (insert.c)  sqlite3.c:142764  for(i=0; i<pSrcIdx->nColumn; i++){
xferOptimization (insert.c)  sqlite3.c:142766  if( sqlite3_stricmp(sqlite3StrBINARY, zColl) ) break;
xferOptimization (insert.c)  sqlite3.c:142768  if( i==pSrcIdx->nColumn ){
xferOptimization (insert.c)  sqlite3.c:142776  if( idxInsFlags!=(OPFLAG_USESEEKRESULT|OPFLAG_PREFORMAT) ){
exprRowColumn (update.c)  sqlite3.c:160618  if( pRet ) pRet->iColumn = iCol+1;
updateFromSelect (update.c)  sqlite3.c:160699  if( pSrc ){
updateFromSelect (update.c)  sqlite3.c:160715  eDest = IsVirtual(pTab) ? SRT_Table : SRT_Upfrom;
updateFromSelect (update.c)  sqlite3.c:160731  if( pChanges ){
updateFromSelect (update.c)  sqlite3.c:160742  if( pSelect ) pSelect->selFlags |= SF_OrderByReqd;
sqlite3Update (update.c)  sqlite3.c:160915  if( aXRef==0 ) goto update_cleanup;
sqlite3Update (update.c)  sqlite3.c:160984  if( rc==SQLITE_DENY ){
sqlite3Update (update.c)  sqlite3.c:160986  }else if( rc==SQLITE_IGNORE ){
sqlite3Update (update.c)  sqlite3.c:161161  if( pKeyInfo ){
sqlite3Update (update.c)  sqlite3.c:161382  || (i<32 && (oldmask & MASKBIT32(i))!=0)
sqlite3Update (update.c)  sqlite3.c:161430  }else if( 0==(tmask&TRIGGER_BEFORE) || i>31 || (newmask & MASKBIT32(i)) ){
updateVirtualTable (update.c)  sqlite3.c:161707  if( HasRowid(pTab) ){
updateVirtualTable (update.c)  sqlite3.c:161708  if( pRowid ){
updateVirtualTable (update.c)  sqlite3.c:161719  if( aXRef[iPk]>=0 ){
updateVirtualTable (update.c)  sqlite3.c:161734  if( pRowExpr ) pRowExpr->op2 = OPFLAG_NOCHNG;
updateVirtualTable (update.c)  sqlite3.c:161762  if( HasRowid(pTab) ){
sqlite3UpsertAnalyzeTarget (upsert.c)  sqlite3.c:161960  if( rc ) return rc;
sqlite3UpsertAnalyzeTarget (upsert.c)  sqlite3.c:161962  if( rc ) return rc;
sqlite3UpsertAnalyzeTarget (upsert.c)  sqlite3.c:161997  if( sqlite3ExprCompare(pParse, pUpsert->pUpsertTargetWhere,
sqlite3UpsertAnalyzeTarget (upsert.c)  sqlite3.c:162011  if( pExpr->op!=TK_COLLATE ){
sqlite3TriggerList (trigger.c)  sqlite3.c:158940  && pTrig->table
sqlite3TriggerList (trigger.c)  sqlite3.c:158942  && (pTrig->pTabSchema!=pTmpSchema || pTrig->bReturning)
triggersReallyExist (trigger.c)  sqlite3.c:159706  && pTab->pTrigger!=0
triggersReallyExist (trigger.c)  sqlite3.c:159707  && sqlite3SchemaToIndex(pParse->db, pTab->pTrigger->pSchema)!=1
triggersReallyExist (trigger.c)  sqlite3.c:159712  if( pList==pTab->pTrigger ){
isAsteriskTerm (trigger.c)  sqlite3.c:159784  if( pTerm->pRight->op!=TK_ASTERISK ) return 0;
sqlite3ReturningSubqueryVarSelect (trigger.c)  sqlite3.c:159840  && (pExpr->x.pSelect->selFlags & SF_Correlated)!=0
codeReturningTrigger (trigger.c)  sqlite3.c:159922  if( !pParse->bReturning ){
codeReturningTrigger (trigger.c)  sqlite3.c:159930  if( pTrigger != &(pReturning->retTrig) ){
codeReturningTrigger (trigger.c)  sqlite3.c:159962  if( sqlite3ResolveExprListNames(&sNC, pNew)==SQLITE_OK
codeRowTrigger (trigger.c)  sqlite3.c:160178  if( v ){
codeRowTrigger (trigger.c)  sqlite3.c:160201  && SQLITE_OK==sqlite3ResolveExprNames(&sNC, pWhen)
sqlite3CodeRowTriggerDirect (trigger.c)  sqlite3.c:160295  if( pPrg ){
sqlite3TriggerColmask (trigger.c)  sqlite3.c:160449  if( pPrg ){
sqlite3FkLocateIndex (fkey.c)  sqlite3.c:138066  if( !aiCol ) return 1;
sqlite3FkLocateIndex (fkey.c)  sqlite3.c:138081  if( aiCol ){
sqlite3FkLocateIndex (fkey.c)  sqlite3.c:138083  for(i=0; i<nCol; i++) aiCol[i] = pFKey->aCol[i].iFrom;
fkLookupParent (fkey.c)  sqlite3.c:138263  if( pIdx->aiColumn[i]==pTab->iPKey ){
exprTableRegister (fkey.c)  sqlite3.c:138324  if( pExpr ){
exprTableColumn (fkey.c)  sqlite3.c:138351  if( pExpr ){
fkScanChildren (fkey.c)  sqlite3.c:138491  if( pWInfo ){
fkChildIsModified (fkey.c)  sqlite3.c:138650  if( iChildKey==pTab->iPKey && bChngRowid ) return 1;
sqlite3FkCheck (fkey.c)  sqlite3.c:138795  for(i=0; i<pFKey->nCol; i++){
sqlite3FkCheck (fkey.c)  sqlite3.c:138824  char *zCol = pTo->aCol[pIdx ? pIdx->aiColumn[i] : pTo->iPKey].zCnName;
sqlite3FkCheck (fkey.c)  sqlite3.c:138887  if( pSrc ){
sqlite3FkOldmask (fkey.c)  sqlite3.c:138945  for(i=0; i<p->nCol; i++) mask |= COLUMN_MASK(p->aCol[i].iFrom);
sqlite3FkOldmask (fkey.c)  sqlite3.c:138953  mask |= COLUMN_MASK(pIdx->aiColumn[i]);
fkActionTrigger (fkey.c)  sqlite3.c:139145  if( pCol->colFlags & COLFLAG_GENERATED ){
fkActionTrigger (fkey.c)  sqlite3.c:139175  if( pRaise ){
fkActionTrigger (fkey.c)  sqlite3.c:139179  if( pSrc ){
fkActionTrigger (fkey.c)  sqlite3.c:139201  if( pTrigger ){
fkActionTrigger (fkey.c)  sqlite3.c:139204  if( pStep->pSrc ){
fkActionTrigger (fkey.c)  sqlite3.c:139220  EnableLookaside;
sqlite3AddReturning (build.c)  sqlite3.c:128284  if( pParse->pNewTrigger ){
sqlite3AddReturning (build.c)  sqlite3.c:128318  ==&pRet->retTrig ){
sqlite3ExprListAppendVector (expr.c)  sqlite3.c:114992  int iFirst = pList ? pList->nExpr : 0;
sqlite3ExprListAppendVector (expr.c)  sqlite3.c:115003  if( pExpr->op!=TK_SELECT && pColumns->nId!=(n=sqlite3ExprVectorSize(pExpr)) ){
sqlite3ExprListAppendVector (expr.c)  sqlite3.c:115014  if( pList ){
sameSrcAlias (select.c)  sqlite3.c:156894  if( p0->pSTab==p1->pSTab && 0==sqlite3_stricmp(p0->zAlias, p1->zAlias) ){
sameSrcAlias (select.c)  sqlite3.c:156898  && (p1->u4.pSubq->pSelect->selFlags & SF_NestedFrom)!=0
sameSrcAlias (select.c)  sqlite3.c:156899  && sameSrcAlias(p0, p1->u4.pSubq->pSelect->pSrc)
rehash (hash.c)  sqlite3.c:39186  if( new_size*sizeof(struct _ht)>SQLITE_MALLOC_SOFT_LIMIT ){
rehash (hash.c)  sqlite3.c:39189  if( new_size==pH->htsize ) return 0;
block sqlite3FinishCoding: RETURNING output loop (build.c)  sqlite3.c:126960  if( pReturning->nRetCol ){
block sqlite3FinishCoding: RETURNING ephemeral table (build.c)  sqlite3.c:127038  if( pRet->nRetCol ){
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111025  if( (pNC->ncFlags & NC_UBaseReg)!=0
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111036  }else if( op!=TK_INSERT && zTab && sqlite3StrICmp("old",zTab)==0 ){
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111045  if( pUpsert && sqlite3StrICmp("excluded",zTab)==0 ){
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111060  if( sqlite3IsRowid(zCol) && VisibleRowid(pTab) ){
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111066  if( iCol<pTab->nCol ){
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111102  pParse->oldmask |= (iCol>=32 ? 0xffffffff : (((u32)1)<<iCol));
block lookupName: new.* / old.* / excluded.* / RETURNING (resolve.c)  sqlite3.c:111106  pParse->newmask |= (iCol>=32 ? 0xffffffff : (((u32)1)<<iCol));
block sqlite3ExprCodeTarget: TK_RAISE (expr.c)  sqlite3.c:118754  pExpr->affExpr, r1);
block selectInnerLoop: SRT_Upfrom (select.c)  sqlite3.c:150957  if( pSort ){
block generateSortTail: SRT_Upfrom (select.c)  sqlite3.c:151458  if( i2<0 ){
block sqlite3Select: SF_UFSrcCheck (select.c)  sqlite3.c:157377  if( sameSrcAlias(p0, p->pSrc) ){
block sqlite3Select: SF_UFSrcCheck (select.c)  sqlite3.c:157380  p0->zAlias ? p0->zAlias : p0->pSTab->zName
```
