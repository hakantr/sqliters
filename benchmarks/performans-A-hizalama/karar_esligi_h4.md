# H4 — R-Tree decision parity (rtree.c 3.54.1  <->  crates/sqliters-ext/src/rtree.rs)

Source lines refer to `sqlite/ext/rtree/rtree.c`. "Rust" names are functions of `impl Rtree` in `rtree.rs`.
The reference binary is an amalgamation build: `MIN` / `MAX` are the macros of sqliteInt.h (`(A)<(B)?(A):(B)`,
`(A)>(B)?(A):(B)`), not the ones rtree.c would define for itself (lines 434-439).

## (a) Decision tables

### A1. ChooseLeaf — which child to descend into (lines 2285-2352)  [Rust `choose_leaf`]
| condition (per level, cells 0..nCell-1 in order) | outcome | line |
|---|---|---|
| some cell `cellContains` the new cell | the containing cell with the smallest `cellArea`; of equal areas the FIRST (`area<fMinArea`) | 2309-2320 |
| no cell contains it | cell with the least growth `cellArea(union)-cellArea`; tie -> smaller area; full tie -> the FIRST (`iCell==0 \|\| growth<fMinGrowth \|\| (growth==fMinGrowth && area<fMinArea)`) | 2326-2342 |
| loop bound | `ii < iDepth - iHeight` (iHeight>0 when a sub-tree is re-inserted) | 2296 |
| arithmetic | REAL32: every side is a `float` difference, the product a `double`; INT32: `i64` differences; nDim selects how many factors | 2171-2195 |

### A2. AdjustTree — when an ancestor cell is rewritten (lines 2359-2392)  [Rust `adjust_tree`]
| condition | outcome | line |
|---|---|---|
| parent cell `cellContains` the inserted cell | nothing written, parent stays clean (`isDirty` unchanged) | 2384 |
| otherwise | `cellUnion` + `nodeOverwriteCell` (parent becomes dirty -> rewritten when released) | 2385-2386 |
| walk | from the node up to the root, every level, no early exit | 2367-2390 |

### A3. rtreeInsertCell — fit or split, which mapping row (lines 2896-2924)  [Rust `rtree_insert_cell`]
| condition | outcome | line |
|---|---|---|
| iHeight>0 and the child node is in memory (`nodeHashLookup`) | its `pParent` is re-pointed to the target node first | 2903-2910 |
| `nodeInsertCell` returns full (nCell==nMaxCell) | `SplitNode` | 2911-2912 |
| fits | `AdjustTree`, then `rowidWrite(rowid, node)` (iHeight==0) or `parentWrite(child, node)` (iHeight>0) | 2914-2921 |

### A4. SplitNode — node numbers, order of writes (lines 2610-2736)  [Rust `split_node` / `split_node_tail`]
| condition | outcome | line |
|---|---|---|
| splitting node 1 | `pRight = nodeNew` then `pLeft = nodeNew`; depth+1 written into the root image | 2647-2652 |
| other node | `pLeft` = the node itself, `pRight = nodeNew(parent)` | 2654-2656 |
| node numbers | `nodeWrite(pRight)` FIRST, then `nodeWrite(pLeft)` only if it has no number: on a root split the right half gets the lower number | 2678-2682 |
| parent entries | root: `rtreeInsertCell(parent, leftbbox)`; else overwrite the parent cell of pLeft + `AdjustTree`; then `rtreeInsertCell(parent, rightbbox)` (may split recursively) | 2687-2707 |
| mappings | every cell of pRight: `updateMapping`; cells of pLeft only for a root split; otherwise only the new cell if it stayed left | 2709-2729 |
| release | `nodeRelease(pRight)` then `nodeRelease(pLeft)` (dirty nodes are written here) | 2732-2733 |

### A5. splitNodeStartree / SortByDimension — axis and distribution (lines 2429-2583)  [Rust `split_node_startree`, `sort_by_dimension`]
| condition | outcome | line |
|---|---|---|
| sort per dimension | merge sort on (min, max) of that dimension; on a full tie the element of the RIGHT run is taken first (`xleft1<xright1 \|\| (xleft1==xright1 && xleft2<xright2)` is false) | 2452-2466 |
| candidate nLeft | `RTREE_MINCELLS .. nCell-RTREE_MINCELLS` | 2530-2534 |
| best distribution of a dimension | least `cellOverlap`; tie -> least total `cellArea`; full tie -> the FIRST (smallest nLeft) | 2554-2561 |
| best dimension | least sum of margins over all its distributions; tie -> the FIRST dimension (`ii==0 \|\| margin<fBestMargin`) | 2564-2568 |
| boxes stored | left box starts as sorted cell 0, right box as sorted cell iBestSplit; unions in sorted order | 2571-2579 |
(The Rust code computes the candidate boxes with running unions instead of rebuilding them; margin, overlap and area
are the same numbers, the stored boxes are built in C's order.)

### A6. deleteCell / removeNode / fixBoundingBox — node removed or kept (lines 2784-2890)  [Rust `delete_cell`, `remove_node`, `fix_bounding_box`]
| condition | outcome | line |
|---|---|---|
| node is the root | cell removed, nothing else | 2881 |
| `NCELL(node) < RTREE_MINCELLS` after the removal | `removeNode`: delete the parent cell (recursively `deleteCell(parent)`), `DELETE` the `_node` row, `DELETE` the `_parent` row, node goes to the head of `pDeleted` with `iNode = height` | 2882-2883, 2784-2832 |
| otherwise | `fixBoundingBox`: parent cell := union of all cells (cell 0 first), up to the root, unconditionally (always dirty) | 2885, 2834-2855 |
| `nodeDeleteCell` | `memmove` of the following cells; the bytes of the former last cell stay in the image | 866-873 |
| `RTREE_MINCELLS` | `((iNodeSize-4)/nBytesPerCell)/3` | 270 |

### A7. rtreeDeleteRowid / reinsertNodeContent — order of re-insertion (lines 2926-3052)  [Rust `rtree_delete_rowid`, `reinsert_node_content`]
| condition | outcome | line |
|---|---|---|
| order of steps | acquire root; find leaf via `_rowid`; `deleteCell`; release leaf; `DELETE` the `_rowid` row; root-with-one-child check; re-insert; release root | 2976-3049 |
| root has exactly one child and depth>0 | `removeNode(child)`, depth-1 written into the root image | 3017-3032 |
| `pDeleted` list | LIFO: the node removed LAST is re-inserted first; inside a node cells 0..n-1 in order; each with `ChooseLeaf(cell, height)` + `rtreeInsertCell(.., height)` | 3035-3042, 2931-2948 |

### A8. Node objects and node numbers (lines 644-963)  [Rust `node_new`, `node_acquire`, `node_write`, `node_release`]
| condition | outcome | line |
|---|---|---|
| `nodeAcquire` of a node in the hash | `nRef++`; a different `pParent` than recorded -> SQLITE_CORRUPT_VTAB | 746-754 |
| not in the hash | blob read of `_node.data`; no row / wrong type / `iNode<=0` / size != iNodeSize / NCELL too large / root depth >= 40 -> SQLITE_CORRUPT_VTAB | 756-831 |
| `nodeRelease` reaching `nRef==0` | release the parent first, then `nodeWrite` if dirty, then drop from the hash; root: `iDepth=-1` | 941-963 |
| `nodeWrite` of a node without number | `INSERT OR REPLACE INTO _node VALUES(NULL, data)`; number = `sqlite3_last_insert_rowid` (max+1) | 905-935 |
| cache | no node survives the call that acquired it (all references are released); nothing is cached between statements | 941-963 |

### A9. Shadow write sites (every statement the module steps)  [Rust `ctx.shadow_*` calls]
| site | statement | line |
|---|---|---|
| `nodeWrite` | `INSERT OR REPLACE INTO '%q'.'%q_node' VALUES(?1, ?2)` | 905-935, 3466 |
| `removeNode` | `DELETE FROM _node WHERE nodeno=?1`, then `DELETE FROM _parent WHERE nodeno=?1` | 2809-2820 |
| `rowidWrite` | `INSERT OR REPLACE INTO _rowid VALUES(?1, ?2)`; with auxiliary columns the UPSERT `INSERT INTO _rowid(rowid,nodeno)VALUES(?1,?2)ON CONFLICT(rowid)DO UPDATE SET nodeno=excluded.nodeno` | 2397-2402, 3471, 3533 |
| `parentWrite` | `INSERT OR REPLACE INTO _parent VALUES(?1, ?2)` | 2407-2412 |
| `rtreeNewRowid` | the `_rowid` statement with (NULL, NULL): placeholder row, rowid = `sqlite3_last_insert_rowid` | 2955-2963 |
| `rtreeDeleteRowid` | `DELETE FROM _rowid WHERE rowid=?1` | 3003-3007 |
| `rtreeUpdate` (aux) | `UPDATE _rowid SET a0=?2,... WHERE rowid=?1` after the insert | 3286-3295 |
| `rtreeSqlInit` (xCreate) | three `CREATE TABLE` + `INSERT INTO _node VALUES(1,zeroblob(N))` | 3485-3513 |
| counters | each of these statements is a top-level statement: its row count is `db->nChange` and is added to `db->nTotalChange` when it halts; its `OP_Insert` sets `db->lastRowid` (vdbeaux.c `sqlite3VdbeHalt`, vdbe.c `OP_Insert`) | — |

### A10. rtreeUpdate — order of checks (lines 3159-3301)  [Rust `rtree_update`]
| condition | outcome | line |
|---|---|---|
| `pRtree->nNodeRef != 0` | SQLITE_LOCKED_VTAB, nothing else | 3170-3175 |
| coordinates | REAL32: `rtreeValueDown` / `rtreeValueUp` of `sqlite3_value_double`; INT32: `rtreeValueInt32`; `lo>hi` -> "rtree constraint failed: T.(c1<=c2)" for the first failing pair | 3206-3227 |
| id supplied and (insert, or id changed) and the id exists | conflict mode REPLACE -> `rtreeDeleteRowid(id)`; otherwise "UNIQUE constraint failed: T.id" | 3231-3250 |
| `aData[0]` not NULL | `rtreeDeleteRowid(old)` | 3257-3259 |
| new row | `rtreeNewRowid` when no id was supplied; `ChooseLeaf(cell, 0)`; `rtreeInsertCell`; release the leaf; aux UPDATE | 3265-3296 |

### A11. rtreeBestIndex — plan selection (lines 2077-2166)  [Rust `best_index`]
| condition (constraints in the order offered) | outcome | line |
|---|---|---|
| no MATCH constraint anywhere, usable `=` on the rowid or column 0 | idxNum=1, argvIndex=1, omit; earlier usages cleared; cost 30, rows 1, SQLITE_INDEX_SCAN_UNIQUE | 2102-2125 |
| usable constraint on a coordinate column with `= > <= < >=` (or MATCH) | appended to idxStr as (`A`..`F`, `'0'+column-1`); omit for `<=`, `>=`, MATCH only | 2127-2148 |
| otherwise | idxNum=2, idxStr of the accepted constraints, rows = nRowEst >> (pairs), cost = 6*rows | 2151-2163 |
| nRowEst | `sqlite_stat1` row of `T_rowid` when the table exists (min 100), else 1048576 | 3378-3405 |

### A12. Search queue — order of the result rows (lines 1468-1756, 1910-2041)  [Rust `search_start`, `step_to_leaf`, `search_point_new`, `enqueue`, `search_point_pop`]
| condition | outcome | line |
|---|---|---|
| comparison of two points | lower `rScore` first; tie -> lower `iLevel` first (depth first) | 1468-1477 |
| new point better than the first (`rScore` lower, or equal and level lower) or queue empty | it becomes `sPoint`; the old `sPoint` is pushed into the heap | 1569-1591 |
| otherwise | binary-heap insert (`rtreeEnqueue`: sift up while strictly smaller than the parent) | 1526-1555 |
| a cell of the first point passes all constraints | level>1: new point for the child node; level 1: new point for the entry (id = node, iCell); the node point is popped first when it has no cell left | 1700-1748 |
| every remaining cell rejected | the node point is popped | 1749-1752 |
| pop | `sPoint` if valid, else heap root: last element moved to the top, sift down preferring the smaller child (`k` over `j` only when strictly smaller) | 1629-1668 |
| idxNum==1 | `_rowid` lookup, one point for the entry; a non-integer argument gives no row | 1928-1955 |
| constraint values | integer -> double (`<`/`>` become `<=`/`>=` beyond 2^48); NULL -> never true; text/blob -> always true for `<` `<=`, never otherwise | 1971-2017 |

### A13. Host rules for virtual tables (C core) implemented in sqliters-core
| rule | C source | engine |
|---|---|---|
| values of INSERT / UPDATE reach xUpdate without column affinity | insert.c 1554-1560, update.c `updateVirtualTable` | `run_vtab_insert_loop`, `try_exec_fast_vtab_insert`, `run_vtab_update_loop` |
| multi-row DELETE: rowids collected in a RowSet, visited ascending, each once; one-row plans (SCAN_UNIQUE, no IN) delete directly | delete.c 532, 598-640 | `run_vtab_delete_loop` |
| UPDATE: rows collected first (ephemeral table), visited in scan order | update.c 1318-1360 | `run_vtab_update_loop` (unchanged) |
| statement journal: `usesStmtJournal = isMultiWrite && mayAbort`; vtab INSERT is multi-write only for SELECT / multi-row VALUES; DELETE / UPDATE unless ONEPASS_SINGLE | insert.c, delete.c 532, update.c 1331 | `vtab_dml_stmt_flags` |
| nested statements of a module have a statement journal when `usesStmtJournal` even in autocommit (`db->nVdbeRead>1`) | vdbe.c OP_Transaction | `nested_stmt_journal_begin` (DROP/ALTER/CREATE TABLE run by xDestroy / xRename / xCreate) |
| nested statements are counted (`changes`, `total_changes`, `last_insert_rowid`) | vdbeaux.c `sqlite3VdbeHalt` | `execute_internal_vtab_sql_counted`, `shadow_*_direct`, `VmRuntimeContext.nested_changes` |
| `x IN (list)`: offered as `=`; when used, one xFilter per distinct non-NULL value in ascending order | where.c `whereLoopAddVirtualOne`, wherecode.c `codeEqualityTerm` | `open_filtered_vtab_cursor_inner` (`BufferedVtabCursor`) |
| `T.C=a OR T.C=b` -> `T.C IN (a,b)` | whereexpr.c `exprAnalyzeOrTerm` case 1 | `rewrite_vtab_or_eq_to_in` |
| MULTI-INDEX OR: cost = LogEst sum of the branch costs + 1 against the single scan; rows branch by branch, duplicates by rowid dropped | where.c `whereLoopAddOr`, wherecode.c | `choose_vtab_or_plan`, `eval_vtab_scan` |
| order of constraints given to xBestIndex: written terms, then virtual terms (BETWEEN halves, OR->IN, commuted copies) of the LAST conjunct first | whereexpr.c `sqlite3WhereExprAnalyze`, where.c `allocateIndexInfo` | `vtab_constraint_order` |
| a column fixed by `col = constant` (propagateConstants, SELECT only; the LAST such conjunct defines it) is no constraint when it is the right operand | select.c `propagateConstants`, whereexpr.c `exprAnalyze` (`EP_FixedCol`) | `vtab_fixed_columns` |
| EXPLAIN QUERY PLAN `SCAN t VIRTUAL TABLE INDEX n:str` (+ ` LEFT-JOIN`, MULTI-INDEX OR tree) | where.c `sqlite3WhereExplainOneScan` | `finish_vtab_eqp_rows` |
| a page taken from the free list is `PAGER_GET_NOCONTENT`: never saved for the open savepoints | pager.c `getPageNormal` / `addToSavepointBitvecs` | `Pager::mark_no_content` |
| the transaction savepoint (`SAVEPOINT` in autocommit mode) is not a pager savepoint (`db->nSavepoint` does not count it): `sqlite3PagerDontWrite` keeps working | vdbe.c OP_Savepoint, btree.c `sqlite3BtreeBeginTrans`, pager.c `sqlite3PagerDontWrite` | `Pager::set_transaction_savepoint`, `Pager::dont_write` |
| ALTER TABLE vtab RENAME TO: nested UPDATE passes on the catalogue first (name written quoted, views / trigger bodies / FK references rewritten), then OP_VRename (xRename: `rtreeRename` renames `_node`, `_parent`, `_rowid` with ALTER TABLE statements), schema cookie = cookie at statement start + 1 | alter.c `sqlite3AlterRenameTable`, `renameReloadSchema`; rtree.c 1042-1069 | `rename_table_in_catalogue` (shared with ordinary tables), `RTreeTable::rename` |
| views and triggers are parsed by every ALTER TABLE; a virtual table they name is connected on demand; `RENAME COLUMN` on a virtual table: `cannot rename columns of virtual table "x"` | alter.c `renameTestSchema`, `isRealTable` | `connect_vtabs_for_alter`, `resolve_trigger_step_target_scope_static`, `view_qn_resolves_to_target_table` |
| `rtreecheck()`: nArg = -1 (count checked at run time); `rtreeCheckTable`: nAux from the column count of `%_rowid` (failure ignored), nDim = (columns - 1 - nAux) / 2, a table that cannot be prepared fails with the bare result code, `nDim < 1` -> "Schema corrupt or not an rtree", bInt = type of column 1 of the first row | rtree.c 4330-4460 | `eval_rtreecheck_in_engine`, `run_rtreecheck` |


## (b) Decision trace: event-by-event comparison from an empty file (h4/trrt.py over h4/sc_rt)

Events compared: every b-tree event of the earlier steps (classes ins / bal / alloc / free ...), every R-Tree decision of (a) interleaved with them (choose / adjust / ins / splitdist / split / depth / delcell / reins / nodeno / newrowid), every shadow write and delete (table, key, value or FNV-1a hash of the 1-node image), and per scan the idxNum / idxStr / argc of xFilter, the search points pushed (level, node, cell, sPoint or heap slot) and the rows returned.

| family | scripts | fully equal (trace + SQL output + integrity + shape) | byte-identical files | events C | events Rust | first mismatch |
|---|---|---|---|---|---|---|
| aux | 4 | 4 | 4 | 23929 | 25214 | - |
| cfl | 12 | 12 | 12 | 17629 | 19442 | - |
| deep | 3 | 3 | 3 | 854935 | 889703 | - |
| del | 14 | 14 | 14 | 83640 | 87568 | - |
| dim | 20 | 20 | 20 | 142833 | 150718 | - |
| mul | 4 | 4 | 4 | 17623 | 18857 | - |
| pgs | 12 | 12 | 12 | 42530 | 46003 | - |
| qry | 8 | 8 | 8 | 65672 | 68800 | - |
| ren | 8 | 8 | 8 | 44244 | 44264 | - |
| shp | 9 | 9 | 9 | 37796 | 39363 | - |
| svp | 16 | 16 | 16 | 50119 | 54318 | - |
| txn | 6 | 6 | 6 | 41890 | 46848 | - |
| upd | 14 | 14 | 14 | 52245 | 57990 | - |
| **total** | 130 | 130 | 130 | 1475085 | 1549088 | |

(The event totals differ because rtreeBestIndex is called several times per statement by C's planner and because C stops a scan at LIMIT / the first row while the engine drains its cursors; those events are compared with the prefix rule or only counted.)

## (c) Coverage: times each decision outcome was exercised (C / Rust)

| decision | outcome | C | Rust | status |
|---|---|---|---|---|
| R1 ChooseLeaf (per level, leaf insert) | contain | 72363 | 72363 | equal |
| R1 ChooseLeaf (per level, leaf insert) | grow | 47765 | 47765 | equal |
| R1 ChooseLeaf (per level, reinsert of a sub-tree) | contain | 1452 | 1452 | equal |
| R1 ChooseLeaf (per level, reinsert of a sub-tree) | grow | 226 | 226 | equal |
| R2 AdjustTree: ancestor cell | keep | 75881 | 75881 | equal |
| R2 AdjustTree: ancestor cell | rewrite | 46110 | 46110 | equal |
| R3 rtreeInsertCell (interior) | fit | 4572 | 4572 | equal |
| R3 rtreeInsertCell (interior) | split | 223 | 223 | equal |
| R3 rtreeInsertCell (leaf) | fit | 66120 | 66120 | equal |
| R3 rtreeInsertCell (leaf) | split | 3326 | 3326 | equal |
| R4 SplitNode (interior) | nonroot | 211 | 211 | equal |
| R4 SplitNode (interior) | root | 12 | 12 | equal |
| R4 SplitNode (leaf) | nonroot | 3173 | 3173 | equal |
| R4 SplitNode (leaf) | root | 153 | 153 | equal |
| R5 splitNodeStartree axis | dim=0 | 2694 | 2694 | equal |
| R5 splitNodeStartree axis | dim=1 | 748 | 748 | equal |
| R5 splitNodeStartree axis | dim=2 | 70 | 70 | equal |
| R5 splitNodeStartree axis | dim=3 | 24 | 24 | equal |
| R5 splitNodeStartree axis | dim=4 | 13 | 13 | equal |
| R6 splitNodeStartree distribution | in between | 3026 | 3026 | equal |
| R6 splitNodeStartree distribution | nLeft==RTREE_MINCELLS | 330 | 330 | equal |
| R6 splitNodeStartree distribution | nLeft==nCell-RTREE_MINCELLS | 193 | 193 | equal |
| R7 tree depth | grow | 165 | 165 | equal |
| R7 tree depth | shrink | 11 | 11 | equal |
| R8 deleteCell (interior) | fix | 1227 | 1227 | equal |
| R8 deleteCell (interior) | remove | 156 | 156 | equal |
| R8 deleteCell (interior) | root | 282 | 282 | equal |
| R8 deleteCell (leaf) | fix | 20391 | 20391 | equal |
| R8 deleteCell (leaf) | remove | 1498 | 1498 | equal |
| R8 deleteCell (leaf) | root | 229 | 229 | equal |
| R9 reinsertNodeContent | entry (h=0) | 10740 | 10740 | equal |
| R9 reinsertNodeContent | sub-tree (h>0) | 1081 | 1081 | equal |
| R10 node number assigned by nodeWrite | new node | 3714 | 3714 | equal |
| R11 rtreeNewRowid | placeholder row | 2763 | 2763 | equal |
| R12 shadow write | aux | 4970 | 4970 | equal |
| R12 shadow write | node | 186291 | 186291 | equal |
| R12 shadow write | parent | 8019 | 8019 | equal |
| R12 shadow write | rowid | 119907 | 119907 | equal |
| R13 shadow delete | node | 1665 | 1665 | equal |
| R13 shadow delete | parent | 1665 | 1665 | equal |
| R13 shadow delete | rowid | 22118 | 22118 | equal |
| R14 rtreeBestIndex | idxNum=1 constraints=0 | 1845 | 3474 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=0 | 1488 | 1160 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=1 | 234 | 460 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=10 | 59 | 72 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=2 | 237 | 820 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=4 | 247 | 300 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=6 | 50 | 59 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R14 rtreeBestIndex | idxNum=2 constraints=8 | 40 | 50 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R15 rtreeFilter | idxNum=1 | 2655 | 2655 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R15 rtreeFilter | idxNum=2 | 1761 | 1761 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R16 search point | entry -> heap | 0 | 0 | **not exercised — not verified** |
| R16 search point | entry -> sPoint | 127870 | 162306 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R16 search point | node -> heap | 0 | 0 | **not exercised — not verified** |
| R16 search point | node -> sPoint | 10834 | 12653 | exercised (scans compared per scan, prefix rule; counts need not be equal) |
| R17 row returned | row | 126753 | 162306 | exercised (scans compared per scan, prefix rule; counts need not be equal) |

B-tree level outcomes exercised by the same scripts (classes of steps H1-H3; all sequences equal):

| decision | outcome | C | Rust |
|---|---|---|---|
| A1 allocateBtreePage | empty-trunk | 24 | 24 |
| A1 allocateBtreePage | extend | 4971 | 4971 |
| A1 allocateBtreePage | leaf | 173 | 173 |
| B1 balance() loop step [del] | nonroot | 2002 | 2002 |
| B1 balance() loop step [del] | root-stop | 1615 | 1615 |
| B1 balance() loop step [del] | stop | 1644 | 1644 |
| B1 balance() loop step [ins] | deeper | 270 | 270 |
| B1 balance() loop step [ins] | nonroot | 410 | 410 |
| B1 balance() loop step [ins] | quick | 4321 | 4321 |
| B1 balance() loop step [ins] | root-stop | 1691 | 1691 |
| B1 balance() loop step [ins] | stop | 2802 | 2802 |
| B2 balance_deeper [ins] | called | 270 | 270 |
| D1 delete trigger nFree*3>usable*2 | balance | 3259 | 3259 |
| D1 delete trigger nFree*3>usable*2 | no-balance | 22205 | 22205 |
| F1 freePage2 | leaf | 1916 | 1916 |
| F1 freePage2 | new-trunk | 101 | 101 |
| I1 sqlite3BtreeInsert: existing cell? | new | 68334 | 68334 |
| I1 sqlite3BtreeInsert: existing cell? | overwrite | 3389 | 3389 |
| I1 sqlite3BtreeInsert: existing cell? | replace-drop | 18264 | 18264 |
| I1 sqlite3BtreeInsert: existing cell? | replace-same | 235571 | 235571 |
| I2 insertCell/allocateSpace [del] | freeblock | 1833 | 1833 |
| I2 insertCell/allocateSpace [del] | gap | 181 | 181 |
| I2 insertCell/allocateSpace [ins] | defrag | 1071 | 1071 |
| I2 insertCell/allocateSpace [ins] | freeblock | 11382 | 11382 |
| I2 insertCell/allocateSpace [ins] | gap | 74527 | 74527 |
| I2 insertCell/allocateSpace [ins] | overflow | 4731 | 4731 |
| N1 sibling selection [del] bBulk=0 | nOld=2 parent has <2 cells: all children | 45 | 45 |
| N1 sibling selection [del] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 632 | 632 |
| N1 sibling selection [del] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 1206 | 1206 |
| N1 sibling selection [del] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 119 | 119 |
| N1 sibling selection [ins] bBulk=0 | nOld=1 parent has <2 cells: all children | 39 | 39 |
| N1 sibling selection [ins] bBulk=0 | nOld=2 parent has <2 cells: all children | 62 | 62 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 4 | 4 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 103 | 103 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 202 | 202 |
| N2 distribution nOld->k [del] | 2->1 leafData=0 | 2 | 2 |
| N2 distribution nOld->k [del] | 2->1 leafData=1 | 30 | 30 |
| N2 distribution nOld->k [del] | 2->2 leafData=0 | 2 | 2 |
| N2 distribution nOld->k [del] | 2->2 leafData=1 | 11 | 11 |
| N2 distribution nOld->k [del] | 3->2 leafData=0 | 24 | 24 |
| N2 distribution nOld->k [del] | 3->2 leafData=1 | 1889 | 1889 |
| N2 distribution nOld->k [del] | 3->3 leafData=1 | 44 | 44 |
| N2 distribution nOld->k [ins] | 1->1 leafData=1 | 7 | 7 |
| N2 distribution nOld->k [ins] | 1->2 leafData=0 | 6 | 6 |
| N2 distribution nOld->k [ins] | 1->2 leafData=1 | 26 | 26 |
| N2 distribution nOld->k [ins] | 2->2 leafData=0 | 30 | 30 |
| N2 distribution nOld->k [ins] | 2->2 leafData=1 | 20 | 20 |
| N2 distribution nOld->k [ins] | 2->3 leafData=0 | 6 | 6 |
| N2 distribution nOld->k [ins] | 2->3 leafData=1 | 6 | 6 |
| N2 distribution nOld->k [ins] | 3->2 leafData=1 | 1 | 1 |
| N2 distribution nOld->k [ins] | 3->3 leafData=0 | 160 | 160 |
| N2 distribution nOld->k [ins] | 3->3 leafData=1 | 79 | 79 |
| N2 distribution nOld->k [ins] | 3->4 leafData=0 | 36 | 36 |
| N2 distribution nOld->k [ins] | 3->4 leafData=1 | 33 | 33 |
| N3 pages [del] | allocated=0 freed=0 | 57 | 57 |
| N3 pages [del] | allocated=0 freed=1 | 1945 | 1945 |
| N3 pages [ins] | allocated=0 freed=0 | 296 | 296 |
| N3 pages [ins] | allocated=0 freed=1 | 1 | 1 |
| N3 pages [ins] | allocated=1 freed=0 | 113 | 113 |
| N4 divider cell [del] (rowid key) | fits | 2014 | 2014 |
| N4 divider cell [ins] (rowid key) | fits | 790 | 790 |
| N4 divider cell [ins] (rowid key) | overflow | 2 | 2 |
| N5 editPage [del] | edit | 4009 | 4009 |
| N5 editPage [del] | rebuild | 7 | 7 |
| N5 editPage [ins] | edit | 1169 | 1169 |
| N5 editPage [ins] | rebuild | 33 | 33 |
| N6 balance-shallower [del] | no | 1970 | 1970 |
| N6 balance-shallower [del] | root-empty-no-room | 1 | 1 |
| N6 balance-shallower [del] | shallower | 31 | 31 |
| N6 balance-shallower [ins] | no | 403 | 403 |
| N6 balance-shallower [ins] | root-empty-no-room | 7 | 7 |
| T1 xferOptimization | return 0 #02 | 34 | 34 |

### Coverage notes
* Tie rules (ChooseLeaf equal area / growth, split equal overlap / area / margin, sort ties) have no outcome of their own in the trace: they are verified through the
  values they decide (`best=`, `dim=`, `nleft=`, node numbers and the hash of every node image written). The scripts `shp_dup_*`, `shp_point_*` and `dim_1*` consist
  largely of identical or one-dimensional boxes, where every comparison is a tie.
* SQLITE_CORRUPT_VTAB paths (wrong node size, cell count, depth >= 40, parent mismatch, missing rows) are ported from the listed lines but are not exercised by a
  trace scenario: **not verified by trace**.
* `rtreeFilter` with a MATCH constraint (geometry / query callbacks) is not implemented in the engine: **not verified**.
* Error paths inside `rtreeUpdate` after a write has started (SQLITE_FULL, I/O errors): **not verified**.

