# Decision parity of step M3 (machine correctness and the DML route)

C = SQLite 3.54.1 sources (`sqlite/src/*.c`, line numbers of the source tree) and the gcov build of the C driver
(`target/perf-work/m2/covm2/rdrv3_cov`, amalgamation line numbers in the coverage table). Rust = the worktree
`.claude/worktrees/makine` (uncommitted step M3). Every C decision below has a Rust counterpart that takes the same
outcome from the same inputs; "not taken" lists the C branch outcomes no corpus reached, classified.

Corpora behind the coverage (C driver, every statement of every op script, 2 470 scripts): ops_dml_a/t/f/tf/v (V5),
m2_dml_a/b (M2), ops_sc6/7/8 (H6/H6b), ops_dml_r_a/r_f/r_t/r_v (V5 rich corpora as op scripts, new), plus
`m3/fik/run.ops` on `m3/fik/base.db` (an expression index and a VIRTUAL-column index whose stored keys differ from the
recomputed ones: the only input that reaches `sqlite3VdbeFindIndexKey` — none of the generated corpora does).

## 1. Coverage (gcov, `m3/covm3/coverage.md`)

| C | lines executed / total | branches taken / total |
|---|---:|---:|
| `OP_IdxDelete` (sqlite3.c 103070-103119) | 23/28 | 11/18 |
| `OP_RowCell` (102272-102287) | 7/7 | 3/4 |
| `OP_Insert` (102173-102260) | 26/32 | 13/18 |
| `OP_Delete` (102328-102428) | 15/21 | 7/18 |
| `OP_VBegin` (104748-104758) | 6/6 | 3/4 |
| `OP_VUpdate` (105162-105219) | 27/29 | 18/24 |
| `OP_VOpen` (104810-104853) | 19/23 | 8/14 |
| `OP_VFilter` (104947-104993) | 19/19 | 5/6 |
| `OP_VColumn` (105008-105057) | 24/27 | 5/8 |
| `OP_VNext` (105064-105099) | 12/13 | 4/6 |
| `OP_Rowid` (102579-102619) | 19/22 | 8/12 |
| `sqlite3VdbeFindIndexKey` (92978-93051) | 27/30 | 28/38 |
| `vdbeIsMatchingIndexKey` (92880-92951) | 37/42 | 15/24 |
| `vdbeSkipField` (92847-92866) | 3/9 | 4/14 |
| `sqlite3BtreeTransferRow` (82486-82581) | 15/68 | 6/42 |
| `sqlite3VtabBegin` (163650-163693) | 20/21 | 14/20 |
| `sqlite3VdbeHalt` (90800-91002) | 66/90 | 56/90 |

The update hook branches of `OP_Insert` / `OP_Delete` are not reached by the C driver (it registers no hook); they are
checked by `m3/hook/hook.c` (C library, `sqlite3_update_hook`) against the Rust test
`c_align_m3_tests::m3_update_hook_events_of_the_machine` (same 16 statements, same event list). The overflow path of
`sqlite3BtreeTransferRow` is not reached by the C corpora; it is checked by `c_align_m3_tests::m3_xfer_insert_select_on_the_machine`
(3 000-byte row, output equal to the C CLI) and by the file-byte comparison of `m3/res/xfer_overflow` (below).

## 2. Decision tables

### OP_IdxDelete (vdbe.c 6794) — E16

| C decision | outcome | Rust |
|---|---|---|
| vdbe.c 6813 `sqlite3BtreeIndexMoveto(pCrsr,&r,&res)` | `res!=0`: not found exactly | vdbe.rs `OP_IdxDelete` (`bc_index_moveto`) |
| vdbe.c 6816 `sqlite3VdbeFindIndexKey(pCrsr, pOp->p4.pIdx, &r, &res, 0)` | search the neighbours | vdbe.rs `find_index_key(..., false)`; the mask from `VdbeHost::index_expr_mask` (vdbe_host.rs `host_common!`) |
| vdbe.c 6818-6819 `res!=0 && !sqlite3WritableSchema(db)` | SQLITE_CORRUPT_INDEX (779) | same arm, `SqliteError::with_extended(779, …)` |
| vdbe.c 6818 `res!=0`, writable schema | cacheStatus stale, skip | same arm |
| vdbe.c 6830 `pOp->p3 && vdbeIndexKeyCompare(...)` | equal key: P3 = NULL, no delete | same arm (byte comparison) |
| vdbe.c 6836 `sqlite3BtreeDelete(pCrsr, BTREE_AUXDELETE)` | delete | `bc_delete(…, BTREE_AUXDELETE)` |

### sqlite3VdbeFindIndexKey (vdbeaux.c 5508) / vdbeIsMatchingIndexKey (5410) / vdbeSkipField (5377)

| C decision | outcome | Rust (vdbe.rs) |
|---|---|---|
| 5527-5534 mask over `MIN(nColumn,BMS)`: `aiColumn==XN_EXPR` or `COLFLAG_VIRTUAL` | bit set | `index_expr_mask`: `col_idx==None` (expression) or a generated VIRTUAL column; trailing rowid / PK columns never set |
| 5538 `mask!=0` | false: `*pRes=1` (caller reports 779) | `find_index_key` returns 1 |
| 5545 Previous up to BTREE_FDK_RANGE (10) while not EOF | | `while !cur.eof() && ii < 10 { bc_previous }` |
| 5548 `rc==SQLITE_DONE` | First, nStep=-1 (whole index) | `done` → `bc_first`, `n_step=-1` |
| 5548 else | nStep=20 | `BTREE_FDK_RANGE * 2` |
| 5559-5563 loop `ii<nStep || nStep<0`, match → break, Next | | inner `while` |
| 5565 `rc==SQLITE_DONE` → OK | | `bc_next` false → break |
| 5569 `nStep<0 || rc || res==0 || bIntegrity` | stop | same condition |
| 5575 second pass | First, nStep=-1 | same |
| 5426 `nRec>0x7fffffff` | SQLITE_CORRUPT | `corrupt()` |
| 5445 `szHdr>98307` | SQLITE_CORRUPT | `corrupt()` |
| 5457 `idxHdr>=szHdr` | CORRUPT_BKPT | `corrupt()` |
| 5463 `idxRec+nSerial>nRec` | CORRUPT_BKPT | `corrupt()` |
| 5467 `vdbeSkipField(...)==0` | compare with `sqlite3MemCompare(&m, &aMem[ii], aColl[ii])`, `res!=0` break | `vdbe_skip_field` / `sqlite3_mem_compare` |
| 5389 `iCol>=BMS || !(mask & MASKBIT(iCol))` | not skipped | `vdbe_skip_field` |
| 5390 `bIntegrity==0` | skipped | same |
| 5391-5396 both REAL and ≤ 2 ULP apart | skipped | same (`to_bits` distance) |

### OP_RowCell (vdbe.c 5996) and OPFLAG_PREFORMAT (btree.c 9616) — xfer

| C decision | outcome | Rust |
|---|---|---|
| vdbe.c 5996 `iKey = pOp->p3 ? aMem[p3].u.i : 0`; `sqlite3BtreeTransferRow(pDest, pSrc, iKey)` | the source cell (local part + overflow chain) prepared | vdbe.rs `OP_RowCell`: the source payload is read (`bc_payload_size` / `bc_payload`) into `Vdbe.preformat_cell` |
| btree.c 9779 `nIn==nRem && nIn<maxLocal` | local copy | the cell is built by `fill_in_cell` in the insert: same bytes |
| btree.c 9794 `nOut<nPayload` / 9831 overflow pages allocated (`allocateBtreePage(...,0,0)`, ptrmap OVERFLOW2) | new overflow chain, `nearby` 0 | `Btree.preformat` (`bc_set_preformat(true)`) → `fill_overflow_chain` with `nearby = 0` (the engine's existing transfer path) |
| btree.c 9616 `flags & BTREE_PREFORMAT` | insert the prepared cell | vdbe.rs `OP_Insert`: `data = preformat_cell` when `OPFLAG_PREFORMAT` |
| `OP_IdxInsert` with `OPFLAG_PREFORMAT` (VACUUM only) | — | refused by `unsupported()` ("OPFLAG_PREFORMAT (index)"); the generator emits it only for VACUUM, which is not generated |

### OP_Insert / OP_Delete update hook (vdbe.c 5925, 6086, 6091, 6144)

| C decision | outcome | Rust |
|---|---|---|
| 5925 `p4type==P4_TABLE && HAS_UPDATE_HOOK(db)` | pTab, zDb set | `hook_tab` (`P4::Table` and `host.has_update_hook()`) |
| 5950 `p5 & OPFLAG_NCHANGE` (nChange, LASTROWID) | as before | unchanged |
| after insert, `rc==0 && pTab` | `xUpdateCallback(ISUPDATE?SQLITE_UPDATE:SQLITE_INSERT, zDb, zName, nKey)` | `host.update_hook(23/18, i_db, name, n_key)` → `MutationPipeline::invoke_update_hook` |
| 6086 `p4type==P4_TABLE && HAS_UPDATE_HOOK` | pTab, zDb | `hook_tab` in `OP_Delete` |
| 6091 `(p5 & OPFLAG_SAVEPOSITION) && isTable` | `movetoTarget = sqlite3BtreeIntegerKey` | `c.moveto_target = bc_integer_key` |
| 6142-6144 `opflags & OPFLAG_NCHANGE` and `xUpdateCallback && pTab && HasRowid(pTab)` | `xUpdateCallback(SQLITE_DELETE, zDb, zName, movetoTarget)` | `host.update_hook(9, …)`; `WriteHost::update_hook` returns early for a WITHOUT ROWID table (HasRowid) |

### Virtual tables in write programs (vdbe.c 8472-8940; vtab.c 1046)

| C decision | outcome | Rust |
|---|---|---|
| vdbe.c 8472 OP_VBegin: `sqlite3VtabBegin(db, pVTab)`, import errmsg | xBegin (+ xSavepoint when `iSvpt`) once per transaction | `WriteHost::vtab_begin` → `vtab::vtab_begin_if_needed` (C's `iSvpt = nStatement + nSavepoint` rule is the engine's) |
| vtab.c 1056 `sqlite3VtabInSync(db)` | SQLITE_LOCKED | not reachable from a statement (no xSync in progress) |
| vdbe.c 8534 OP_VOpen: cursor already open on the same `pVtab` | reuse | `VCursor.vt.id == vt.id` |
| `pVtab==0` | SQLITE_LOCKED | `WriteHost::vtab_db`: no connected entry → `ErrorCode::Locked` |
| `xOpen`, `allocateCursor(CURTYPE_VTAB)` | | `WriteHost::vtab_open` (`VirtualTable::open`, fetch mask all columns, row-by-row cursor) |
| vdbe.c 8671 OP_VFilter: `iQuery=aMem[p3]`, `nArg=aMem[p3+1]`, `xFilter(pVCur, iQuery, p4.z, nArg, apArg)`; `xEof` → jump P2; `nullRow=0` | | vdbe.rs `OP_VFilter`; `WriteHost::vtab_filter` (`VirtualCursor::filter` with C's idxNum / idxStr / argv) |
| vdbe.c 8732 OP_VColumn: `nullRow` → NULL | | same |
| `p5 & OPFLAG_NOCHANGE` → `MEM_Null|MEM_Zero`, then `xColumn` (which may leave it) | | `vtab_column_skips_nochange`: fts5 (non-hidden column: `fts5ColumnMethod` 3088 returns without a result) and geopoly column 0 keep `MEM_Null|MEM_Zero`; others call `column_ctx` |
| vdbe.c 8788 OP_VNext: `nullRow` → fall through; `xNext`; `!xEof` → jump | | vdbe.rs `OP_VNext` |
| vdbe.c 6320 OP_Rowid on CURTYPE_VTAB: `xRowid` | | vdbe.rs `OP_Rowid` `vcur` branch |
| vdbe.c 8886 OP_VUpdate: `vtabOnConflict = p5`, `xUpdate(nArg, apArg, &rowid)` | | `WriteHost::vtab_update`: argv → `VTabUpdateOperation` (1 arg DELETE; argv[0] NULL INSERT; else UPDATE), `sqlite3_value_nochange` = `MEM_Null|MEM_Zero` |
| 8924 `rc==OK && p1` | `db->lastRowid = rowid` | `host.set_last_rowid` |
| 8928 `(rc&0xff)==SQLITE_CONSTRAINT && bConstraint`: `p5==OE_Ignore` → OK; else `errorAction = p5==OE_Replace ? OE_Abort : p5` | | `vtab_constraint_support`; `conflict_effect` Rollback / Fail / Abort |
| 8934 else `nChange++` (also before an error) | | same |
| OP_Transaction `sqlite3VtabSavepoint(db, SAVEPOINT_BEGIN, iStatement-1)` | | `run_write`: `vtab_savepoint_before_stmt` when the statement journal opens and a virtual table is in the transaction |

Modules: OP_VFilter needs a module whose `xFilter` takes C's idxNum / idxStr (D35). The route accepts a program that
scans (OP_VOpen) only fts5 / rtree / rtree_i32 tables (vdbe_host.rs `machine_write_program`); a program scanning any
other module stays on the engine. Read programs (SELECT) with OP_VOpen still fall back (no ExecContext in the read host).

### The DML route (E19) — vdbe_host.rs `machine_write_program`, lib.rs `c_write_of` / `apply_c_write` / `write_artifacts`

| C step | Rust |
|---|---|
| `sqlite3_prepare`: parse + code generation; an error ends the prepare (`sqlite3ErrorWithMsg`, error offset) | `c_write_of` runs before the engine's `compile_parsed_stmt`; `WriteRoute::Error` → `Err` with C's message and offset (`with_offset`); C's `sqlite3Init()` loaded flag → `c_mark_schemas_loaded` |
| the program | `WriteRoute::Program` → `Statement.c_machine_write`; an engine compile error is ignored (`write_artifacts`); result column names (RETURNING) and parameters from C's program (`apply_c_write`) |
| `sqlite3Reprepare` on a schema change | `reprepare_in_open_tx` calls `c_write_of` again |
| (not C) fall back to the engine | `PrepareResult::Unsupported`, an opcode the machine lacks, authorizer, application-defined collation, UTF-16, an open incremental blob handle |
| the fast literal INSERT path of `execute_batch` | off while the switch is on (`try_fast_exec_literal_insert_step`) |

### Engine schema (E17 / D30): build.c 4396-4458 (`sqlite3CreateIndex`, `pTab==pParse->pNewTable`)

| C decision | outcome | Rust (schema.rs) |
|---|---|---|
| constraints create their automatic indexes in textual order | a column-level UNIQUE written before the PRIMARY KEY of the same column comes first | `unique_constraints` keep `before_pk`; those are pushed before the PK |
| 4425-4436 same columns and collations, sort order not compared | the existing index stays, with its sort orders | `push_inline_autoindex` no longer replaces the existing columns |
| 4452 `idxType==SQLITE_IDXTYPE_PRIMARYKEY` | `pIdx->idxType = PRIMARYKEY` | `existing.origin = "pk"` |

## 3. Branch outcomes never taken (C side, `m3/covm3/untaken.txt`), classified

- **I/O / allocation errors** (`if( rc ) goto abort_due_to_error` after a b-tree call, `mallocFailed`, `aRec==0`):
  OP_IdxDelete 103090/103093/103107/103113, OP_RowCell 102284, OP_Insert 102250, OP_Delete 102415, OP_VBegin 104753,
  OP_VUpdate 105175, OP_VOpen 104834, OP_VFilter 104984, OP_VColumn 105052, OP_VNext 105088, OP_Rowid 102603/102609,
  vdbeIsMatchingIndexKey 92904/92910, sqlite3VtabBegin 163680/163682. Rust: the same `?` propagation; not verified.
- **Corrupt records** (vdbeIsMatchingIndexKey 92896, 92915, 92927, 92933; TransferRow 82502, 82527): Rust returns
  `corrupt()` at the same tests; not verified (no corrupt-file corpus in this step).
- **Writable schema** (OP_IdxDelete 103095 `!sqlite3WritableSchema` false outcome): Rust skips as C; not verified.
- **Update hook** (OP_Insert 102201/102251/102256, OP_Delete 102362/102367/102420): not reached by the C driver; verified
  with `m3/hook/hook.c` vs `m3_update_hook_events_of_the_machine` (inserts, updates incl. rowid change, REPLACE, UPSERT,
  WITHOUT ROWID (no events), INSERT…SELECT into the same table, truncation). OPFLAG_SAVEPOSITION + hook (6091): not verified.
- **NEVER / ALWAYS** (OP_VUpdate 105178/105185, OP_VOpen 104819/104820/104827/104841): structural.
- **Virtual-table constraint support** (OP_VUpdate 105204/105205: `bConstraint` false outcome after a constraint error
  and the `OE_Ignore` outcome): the corpora's rtree/fts5 constraint errors all came with `bConstraint` set and a non-Ignore
  conflict mode; not verified.
- **nullRow** of a virtual-table cursor (OP_VColumn 105020, OP_VNext 105073, OP_Rowid 102590/102610): not reached
  (a LEFT JOIN on a virtual table inside DML); not verified. `xColumn` error (`sContext.isError`, 105044): not verified.
- **sqlite3VdbeFindIndexKey**: the EOF-at-start outcome of the Previous loop (93035 `rc==SQLITE_DONE` true is taken, the
  `res!=0` with cursor invalid at 93029, the exhaustive second pass with `bIntegrity`): partly; `vdbeSkipField`
  `bIntegrity` outcomes (92856-92861) belong to `PRAGMA integrity_check` (`OP_IFindKey`, not ported: integrity_check
  stays on the engine).
- **sqlite3BtreeTransferRow overflow branches** (82521-82575): not reached in C corpora; Rust covered by the unit test and
  the overflow comparison script (file bytes equal to C, auto_vacuum 0 and 1).
- **sqlite3VtabBegin** `xBegin==0` (163667), `iSvpt && xSavepoint` false outcomes: module-dependent; not verified.
- **sqlite3VdbeHalt** branches (special errors, SQLITE_BUSY at commit, CorruptRdOnly, schema with other VMs, FK at
  commit): the halt of a write statement is the engine's write-statement end (`c_statement_halt`, `abort_write_statement`),
  unchanged by M3; listed for completeness.
