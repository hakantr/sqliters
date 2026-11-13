# H2 — decision parity of the insert side (`sqlite3BtreeInsert` → `insertCell` → `balance()`)

C reference: SQLite 3.54.1, `sqlite/src/btree.c` (line numbers below). Rust: `crates/sqliters-btree/src/insert.rs` (new, I),
`balance.rs` (B, ported in H1, now also driven by inserts), `page.rs` (P), `lib.rs` (L), engine: `crates/sqliters-core/src/engine.rs` (E).

## Method

* **C trace**: instrumented amalgamation `h2/ctrace` (`patch.py` + `patch_extra.py` (H1) + `patch_h1b.py` + `patch_h2.py`), shell `sqlite3_tr`, events on stderr
  when `DTRACE` is set, never for temporary files. New in H2 (class `ins`): `TR|ins|cell|root= pg= idx= o=new|overwrite|replace-same|replace-drop sz=`
  (in `sqlite3BtreeInsert`) and `TR|ins|slot|ctx=ins|del pg= i= sz= o=gap|freeblock|defrag|overflow off=` (in `insertCell` and `insertCellFast`, outcome of
  `allocateSpace` recorded through a flag). Existing classes used: `bal` (every `balance()` step, sibling set, distribution, dividers, `editPage`/`rebuildPage`,
  pages allocated/freed, `balance_deeper`, `balance_quick`; `ctx=ins|del`), `alloc` (`allocateBtreePage`: page, branch, `nearby`), `free` (`freePage2`).
* **Rust trace**: `sqliters_types::dtrace!` at the same points (compiled only with `--cfg sqliters_dtrace`; trace build `step5g/tracebuild.sh`). The context
  is no longer hard-coded: `Btree.tr_ins` is set by the insert entry points and cleared by the delete entry points (`Btree::tr_ctx()`).
* **Differ** `h2/trins.py`: both engines START FROM AN EMPTY FILE and run the whole script. Compared event by event, all fields, in one sequence:
  classes `ins`, `bal` (both contexts), `alloc`, `free|page`. Also compared: stdout, C and Rust `PRAGMA integrity_check` on both files, `page_count`,
  `freelist_count`, C's `dbstat` rows (shape), and the raw bytes of the two files. `h2/trall.sh OUT` runs all scenarios, `h2/trsum.py OUT --cov` summarises.
* **Scenarios** `h2/gen_sc.py` → `h2/sc/*.sql` (177): families `s` plain tables sequential / reverse / random / row by row / auto rowid; `p` payload sizes 0 bytes … several
  overflow pages, rows around `maxLocal`; `x` index keys int / text / large / overflowing / duplicates / NULL, sequential, reverse, random, several indexes;
  `w` WITHOUT ROWID; `v` auto_vacuum 1 and 2; `m` inserts interleaved with deletes, same-size / growing / shrinking UPDATEs and REPLACE (plain, indexed,
  WITHOUT ROWID, autocommit, auto_vacuum); `e` exact-fit sweep (one table per row size) and churn; `o` overwrite / replace of rows with overflow chains (scan,
  rowid lookup, partial, grow, shrink, REPLACE, UPSERT); `r` root on page 1 (hundreds of schema rows, drops, auto_vacuum); `t` savepoints / rollbacks;
  `g` several trees growing in turns + drop + free-page reuse; `h3` (xfer `INSERT INTO d SELECT * FROM s` with indexes, `CREATE INDEX` on a filled table — the
  subject of H3, kept to show C's `bBulk=1` numbers). Page sizes 512, 1024, 4096, 8192, 65536.

## Decision table

`cov key` names the line of the coverage table (below) that counts the outcome in C and in Rust.

### S — where the entry goes: seek, cursor reuse, overwrite or not
| # | C condition (line) | outcome | Rust | trace / cov key | status |
|---|---|---|---|---|---|
| S1 | `pCur->curFlags & BTCF_Multiple` (9470) | `saveAllCursors` | `save_cursors_for_mutation` (I, as before) | — | no effect on pages; not traced |
| S2 | `pCur->eState>=CURSOR_REQUIRESEEK` (9486) | `moveToRoot` | the engine holds no C cursor: descent from the root, or a validated `(leaf, idx)` position | — | no effect on pages; position checked through `pg=`/`idx=` of every `ins|cell` event |
| S3 | table: `BTCF_ValidNKey && pX->nKey==info.nKey` (9529) and `info.nSize!=0 && info.nPayload==nData+nZero` (9533–9534) | `btreeOverwriteCell` | `insert_table_c(.., Upsert{overwrite:true})` → `replace_cell` → `overwrite_cell`; callers: UPDATE (`insert_table_at` with the cursor position, `insert_table`), REPLACE on a table with an index | `ins|cell o=overwrite` / I1 | verified where exercised; **engine gap**: a two-pass UPDATE that reads no column of the old row (see "not verified") |
| S3b | same, but `info.nSize==0` (cursor only positioned by `OP_NotExists`) | no overwrite: replace path (R) | `insert_table_replace_unparsed` (`Upsert{overwrite:false}`), used by REPLACE that keeps the table row of a table without index (E: `stmt_tx.replace_unparsed_row`) | `ins|cell o=replace-same` / `replace-drop` | verified (families m, o) |
| S4 | table, `loc==0` (9540) | `sqlite3BtreeTableMoveto(nKey, flags&BTREE_APPEND)` | `find_table_interior_child_append_hint` per level + `table_leaf_search` (last cell first = `biasRight`) | `pg=`, `idx=` of `ins|cell` | verified (same page and index in every event) |
| S5 | index, `loc==0 && !(flags&BTREE_SAVEPOSITION)` (9562) | `sqlite3BtreeIndexMoveto` / `btreeMoveto` | `index_page_search_raw` (C's probe sequence `(lwr+upr)>>1`), `index_page_search_full` for cells with overflow | `pg=`, `idx=` | verified |
| S6 | index, `loc==0`: `info.nKey==pX->nKey` (9584) | `btreeOverwriteCell`, else replace path | `replace_cell(overwrite=true)`; also on an interior page (divider hit) | `ins|cell o=overwrite` / `replace-drop` | verified (WITHOUT ROWID UPDATE / REPLACE: family m_wr) |
| S7 | `flags & BTREE_PREFORMAT` (9616) | cell prepared by `sqlite3BtreeTransferRow` | — (the engine copies rows of the xfer optimisation through the normal insert) | — | **not verified / not implemented: H3** |
| S8 | `flags & BTREE_SAVEPOSITION` after `balance()` (9722) | cursor → `CURSOR_REQUIRESEEK` | callers re-seek | — | no effect on pages |

### C — `fillInCell`
| # | C condition (line) | outcome | Rust | trace / cov key | status |
|---|---|---|---|---|---|
| C1 | `nPayload<=pPage->maxLocal` (7148) | whole payload local, cell padded to 4 bytes | `fill_in_cell` / `local_len`; fast paths compute the same size | `sz=` of `ins|cell` | verified (p family: 0-byte … multi-page payloads, rows at `maxLocal`, `maxLocal+1`) |
| C2 | else `n = minLocal + (nPayload-minLocal)%(usable-4)`; `n>maxLocal` → `minLocal` (7171–7173) | local part + 4-byte overflow pointer | same | `sz=` | verified |
| C3 | overflow page allocation `allocateBtreePage(.., nearby=pgnoOvfl, 0)` (7244); auto-vacuum: `nearby` = next non-pointer-map page (7234–7240) | chain in forward order | `fill_overflow_chain` | `alloc|page pg= o= near=` / A1 | verified |
| C4 | `ptrmapPut(pgnoOvfl, pgnoPtrmap ? OVERFLOW2 : OVERFLOW1, pgnoPtrmap)` (7257) | entry at allocation time, first page fixed by `insertCell` | same order | bytes | verified through byte equality (v family L0) |

### R — entry exists (`loc==0`)
| # | C condition (line) | outcome | Rust | trace / cov key | status |
|---|---|---|---|---|---|
| R1 | `BTREE_CLEAR_CELL` (9655): old cell has overflow | chain freed page by page (`clearCellOverflow`) after the new cell (and its chain) is built | `replace_cell` → `free_overflow_chain_n` | `free|page` / F1 | verified (o family) |
| R2 | `info.nSize==szNew && info.nLocal==info.nPayload && (!autoVacuum || szNew<minLocal)` (9657–9659) | `memcpy(oldCell, newCell, szNew)` (page written even when identical) | `replace_cell` | `ins|cell o=replace-same` / I1 | verified |
| R3 | else | `dropCell` (9678) then `insertCellFast` at the same index | `balance::drop_cell` + `page_insert_cell` | `ins|cell o=replace-drop` + `ins|slot` | verified |
| R4 | `loc<0 && pPage->nCell>0` (9681) | new cell after the cursor's cell | lower-bound index | `idx=` of `o=new` | verified |

### I — `insertCellFast` / `allocateSpace` / `pageFindSlot` / `defragmentPage`
| # | C condition (line) | outcome | Rust | trace / cov key | status |
|---|---|---|---|---|---|
| I1 | `sz+2>pPage->nFree` (7480; `insertCell` 7385 also `nOverflow`) | overflow cell (`apOvfl[]`) → `balance()` | `page_insert_cell` returns false (the gap is tested first; `nFree` is computed only when the gap is too small) → `OvflCells` → `Btree::balance`; `balance::insert_cell` for dividers | `ins|slot o=overflow` / I2 | verified |
| I2 | freeblock exists and `gap+2<=top` (1889) → `pageFindSlot`: first block with `size>=nByte` (1791); remainder `<4` → whole block, fragment count += remainder (1794–1803) unless `nFrag>57` (1797 → no slot); else the block is shortened and its tail used | slot inside a freeblock | `balance::allocate_space` → `page_find_slot` | `ins|slot o=freeblock off=` / I2 | verified (same offset in every event) |
| I3 | `gap+2+nByte>top` (1909) | `defragmentPage(min(4, nFree-(2+nByte)))`; fast path for ≤2 freeblocks when `nFrag<=nMaxFrag` (1674); gap zeroed | `balance::defragment_page` | `ins|slot o=defrag off=` / I2 | verified |
| I4 | else | `top -= nByte` | fast paths (`insert_table_c`, `insert_table_on_leaf_c`, `insert_index_c`: no freeblock and `gap+2+sz<=top`) and `allocate_space` | `ins|slot o=gap off=` / I2 | verified |
| I5 | `autoVacuum` (7497) | `ptrmapPutOvflPtr` | `ptrmap_put(ovfl, Overflow1, pgno)` | bytes | verified through byte equality |
| I6 | `dropCell` (7299): `freeSpace` coalescing with the next / previous freeblock (1990, 2005), fragment bookkeeping, content-start move (2024); `nCell==0` → header reset (7330) | — | `balance::drop_cell` → `raw_page_free_space`; the in-place delete paths of P now call it too | bytes | verified through byte equality |
| I7 | `btreeOverwriteContent`: `memcmp` (9322) → `sqlite3PagerWrite` only when the bytes differ; zero tail (9304) | — | `overwrite_cell` (no `nZero` payloads in the engine) | bytes | verified through byte equality; "page not dirtied" itself is not traced |

### B / N — `balance()` and `balance_nonroot()` on the insert path
| # | C condition (line) | outcome | Rust | trace / cov key | status |
|---|---|---|---|---|---|
| B1 | `nOverflow==0 && nFree*3<=usable*2` (9175) | stop | `Btree::balance` (B) | `bal|step o=stop` / B1 [ins] | verified |
| B2 | `iPage==0` (9181): `nOverflow` → `balance_deeper` (9188), else stop | root split | `balance_deeper` | `o=deeper`, `bal|deeper` / B1, B2 | verified |
| B3 | `intKeyLeaf && nOverflow==1 && aiOvfl[0]==nCell && pParent->pgno!=1 && pParent->nCell==iIdx` (9217–9221) | `balance_quick` | `balance_quick` | `o=quick`, `bal|quick` / B1, B3 | **verified now** (H1: not exercised) |
| B4 | else `balance_nonroot(pParent, iIdx, .., iPage==1, hints&BTREE_BULKLOAD)` (9260) | — | `balance_nonroot` | `o=nonroot` / B1 | verified, `bBulk=0` |
| N1 | sibling choice (8343–8356): `nxDiv`, `nOld`; with `bBulk` `nxDiv=i-2+bBulk`, `i=2-bBulk` | 1–3 old siblings | same | `bal|sib` / N1 | verified for `bBulk=0`; `bBulk=1` **not verified** (no Rust caller before H3) |
| N2 | distribution (8560–8683): left packing, then the right-to-left evening loop; `bBulk` stops it (8663) | `cntNew[]`, `szNew[]` | same | `bal|dist` / N2 | verified for `bBulk=0` |
| N3 | new pages `allocateBtreePage(.., bBulk ? 1 : pgno, 0)` (8713), renumbering in ascending order (8740–8760), old pages freed | — | same | `bal|pages`, `alloc|page`, `free|page` / N3 | verified for `bBulk=0` |
| N4 | divider cells into the parent (8810–8870) | fits / overflow cell of the parent | `insert_cell` | `bal|div` / N4 | verified |
| N5 | `editPage` vs `rebuildPage` (8900–8940) | — | `NonRoot::edit_page` | `bal|edit` / N5 | verified |
| N6 | `isRoot && pParent->nCell==0 && hdrOffset<=apNew[0]->nFree` (8944) | balance-shallower | same | `bal|shallow` / N6 | verified (`no` on the insert path; `shallower` in H1) |


## Results (final build)

180 scripts, every one started from an empty file by both engines; 3092603 C events, 3075416 Rust events. **176 scripts have identical event sequences** (every `ins` / `bal` / `alloc` / `free` decision, all fields, in order); 176 are equal in trace + SQL output + integrity (both engines on both files) + dbstat shape; 175 produce byte-identical files.

| family | what | scripts | trace identical | + sql, integrity, shape | byte-identical files | C events |
|---|---|---|---|---|---|---|
| e | exact-fit sweep and churn | 6 | 6 | 6 | 6 | 42190 |
| g | several trees growing in turns, drop, free-page reuse | 2 | 2 | 2 | 2 | 80750 |
| h3 | xfer copy with indexes, CREATE INDEX (subject of H3) | 4 | 0 | 0 | 0 | 117662 |
| m | mixed insert / delete / same-size, growing, shrinking UPDATE / REPLACE | 24 | 24 | 24 | 23 | 429937 |
| o | overwrite / replace of rows with overflow chains, UPSERT, two-pass UPDATE | 21 | 21 | 21 | 21 | 28194 |
| p | payload sizes (0 bytes … several overflow pages, maxLocal edges) | 15 | 15 | 15 | 15 | 167732 |
| r | root on page 1 (sqlite_schema) | 6 | 6 | 6 | 6 | 13435 |
| s | plain tables: sequential / reverse / random / row by row / auto rowid | 25 | 25 | 25 | 25 | 524988 |
| t | savepoints / rollbacks | 2 | 2 | 2 | 2 | 64199 |
| v | auto_vacuum 1 / 2 | 18 | 18 | 18 | 18 | 353614 |
| w | WITHOUT ROWID | 15 | 15 | 15 | 15 | 135962 |
| x | index keys (int, text, large, overflowing, duplicates, NULL; several indexes) | 42 | 42 | 42 | 42 | 1133940 |

First mismatching decision of every script that is not fully equal:

* `h3_cidx_1024`: trace differs at event #5579, sql differs, shape differs, bytes differ, pages C 344/0 Rust 367/0<br>C: `ins|cell|root=1 pg=1 idx=1 o=new sz=40`<br>Rust: `alloc|page|pg=119 o=extend`
* `h3_cidx_4096`: trace differs at event #5144, sql differs, shape differs, bytes differ, pages C 86/0 Rust 92/0<br>C: `ins|cell|root=1 pg=1 idx=1 o=new sz=40`<br>Rust: `alloc|page|pg=32 o=extend`
* `h3_xfer_1024`: trace differs at event #26355, sql differs, shape differs, bytes differ, pages C 517/0 Rust 529/0<br>C: `ins|cell|root=266 pg=266 idx=0 o=new sz=37`<br>Rust: `ins|cell|root=268 pg=268 idx=0 o=new sz=6`
* `h3_xfer_4096`: trace differs at event #18384, sql differs, shape differs, bytes differ, pages C 131/0 Rust 133/0<br>C: `ins|cell|root=68 pg=68 idx=0 o=new sz=37`<br>Rust: `ins|cell|root=70 pg=70 idx=0 o=new sz=6`
* `m_wr_64k`: trace identical, sql =, shape =, bytes differ, pages C 396/2 Rust 396/2

## Coverage: times every outcome was exercised (C / Rust)

Columns 3–4: the 176 scripts of the H2 families (everything except `h3_*`). Columns 5–6: the 4 `h3_*` scripts (xfer copy with indexes, CREATE INDEX), whose index builds are the subject of H3 — they are listed because they are the only place where C takes the `bBulk=1` branches.

| decision | outcome | C | Rust | h3: C | h3: Rust |
|---|---|---|---|---|---|
| A1 allocateBtreePage | empty-trunk | 2724 | 2724 | 0 | 0 |
| A1 allocateBtreePage | extend | 66093 | 66093 | 1074 | 1117 |
| A1 allocateBtreePage | leaf | 3341 | 3341 | 0 | 0 |
| B1 balance() loop step [del] | nonroot | 2370 | 2370 | 0 | 0 |
| B1 balance() loop step [del] | root-stop | 619 | 619 | 0 | 0 |
| B1 balance() loop step [del] | stop | 2647 | 2647 | 0 | 0 |
| B1 balance() loop step [ins] | deeper | 685 | 685 | 22 | 16 |
| B1 balance() loop step [ins] | nonroot | 79125 | 79125 | 2018 | 2052 |
| B1 balance() loop step [ins] | quick | 14097 | 14097 | 423 | 423 |
| B1 balance() loop step [ins] | root-stop | 33698 | 33698 | 1053 | 1094 |
| B1 balance() loop step [ins] | stop | 55409 | 55409 | 1188 | 1343 |
| B2 balance_deeper [ins] | called | 685 | 685 | 22 | 16 |
| B3 balance_quick [ins] | called | 14097 | 14097 | 423 | 423 |
| D1 delete trigger nFree*3>usable*2 | balance | 2356 | 2356 | 0 | 0 |
| D1 delete trigger nFree*3>usable*2 | no-balance | 36468 | 36468 | 0 | 0 |
| D2 interior cell replaced by predecessor | fits | 945 | 945 | 0 | 0 |
| D2 interior cell replaced by predecessor | overflow | 5 | 5 | 0 | 0 |
| D3 second balance at the interior page | called | 910 | 910 | 0 | 0 |
| F1 freePage2 | leaf | 4723 | 4723 | 0 | 0 |
| F1 freePage2 | new-trunk | 2757 | 2757 | 0 | 0 |
| I1 sqlite3BtreeInsert: existing cell? | new | 806357 | 806357 | 45018 | 35018 |
| I1 sqlite3BtreeInsert: existing cell? | overwrite | 12269 | 12269 | 0 | 0 |
| I1 sqlite3BtreeInsert: existing cell? | replace-drop | 26345 | 26345 | 6 | 6 |
| I1 sqlite3BtreeInsert: existing cell? | replace-same | 66 | 66 | 0 | 0 |
| I2 insertCell/allocateSpace [del] | defrag | 67 | 67 | 0 | 0 |
| I2 insertCell/allocateSpace [del] | freeblock | 3279 | 3279 | 0 | 0 |
| I2 insertCell/allocateSpace [del] | gap | 367 | 367 | 0 | 0 |
| I2 insertCell/allocateSpace [del] | overflow | 7 | 7 | 0 | 0 |
| I2 insertCell/allocateSpace [ins] | defrag | 20007 | 20007 | 406 | 758 |
| I2 insertCell/allocateSpace [ins] | freeblock | 373725 | 373725 | 6741 | 13466 |
| I2 insertCell/allocateSpace [ins] | gap | 540710 | 540710 | 39651 | 23168 |
| I2 insertCell/allocateSpace [ins] | overflow | 93229 | 93229 | 2275 | 2475 |
| N1 sibling selection [del] bBulk=0 | nOld=2 parent has <2 cells: all children | 13 | 13 | 0 | 0 |
| N1 sibling selection [del] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 75 | 75 | 0 | 0 |
| N1 sibling selection [del] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 2170 | 2170 | 0 | 0 |
| N1 sibling selection [del] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 112 | 112 | 0 | 0 |
| N1 sibling selection [ins] bBulk=0 | nOld=1 parent has <2 cells: all children | 350 | 350 | 5 | 10 |
| N1 sibling selection [ins] bBulk=0 | nOld=2 parent has <2 cells: all children | 845 | 845 | 8 | 16 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 20528 | 20528 | 178 | 356 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 51653 | 51653 | 794 | 1588 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 5749 | 5749 | 41 | 82 |
| N1 sibling selection [ins] bBulk=1 | nOld=1 parent has <2 cells: all children | 0 | 0 | 11 | 0 |
| N1 sibling selection [ins] bBulk=1 | nOld=2 bulk: right-most child (nxDiv=i-1) | 0 | 0 | 981 | 0 |
| N2 distribution nOld->k [del] | 2->1 leafData=0 | 5 | 5 | 0 | 0 |
| N2 distribution nOld->k [del] | 2->2 leafData=0 | 8 | 8 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->1 leafData=1 | 63 | 63 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->2 leafData=0 | 168 | 168 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->2 leafData=1 | 1660 | 1660 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->3 leafData=0 | 88 | 88 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->3 leafData=1 | 376 | 376 | 0 | 0 |
| N2 distribution nOld->k [del] | 3->4 leafData=0 | 2 | 2 | 0 | 0 |
| N2 distribution nOld->k [ins] | 1->1 leafData=0 | 1 | 1 | 0 | 0 |
| N2 distribution nOld->k [ins] | 1->1 leafData=1 | 9 | 9 | 0 | 0 |
| N2 distribution nOld->k [ins] | 1->2 leafData=0 | 270 | 270 | 16 | 10 |
| N2 distribution nOld->k [ins] | 1->2 leafData=1 | 70 | 70 | 0 | 0 |
| N2 distribution nOld->k [ins] | 2->1 leafData=0 | 12 | 12 | 0 | 0 |
| N2 distribution nOld->k [ins] | 2->2 leafData=0 | 433 | 433 | 560 | 6 |
| N2 distribution nOld->k [ins] | 2->2 leafData=1 | 132 | 132 | 0 | 0 |
| N2 distribution nOld->k [ins] | 2->3 leafData=0 | 201 | 201 | 429 | 10 |
| N2 distribution nOld->k [ins] | 2->3 leafData=1 | 67 | 67 | 0 | 0 |
| N2 distribution nOld->k [ins] | 3->2 leafData=0 | 21 | 21 | 0 | 0 |
| N2 distribution nOld->k [ins] | 3->2 leafData=1 | 65 | 65 | 0 | 0 |
| N2 distribution nOld->k [ins] | 3->3 leafData=0 | 27690 | 27690 | 847 | 1694 |
| N2 distribution nOld->k [ins] | 3->3 leafData=1 | 26734 | 26734 | 0 | 0 |
| N2 distribution nOld->k [ins] | 3->4 leafData=0 | 8226 | 8226 | 166 | 332 |
| N2 distribution nOld->k [ins] | 3->4 leafData=1 | 14957 | 14957 | 0 | 0 |
| N2 distribution nOld->k [ins] | 3->5 leafData=1 | 237 | 237 | 0 | 0 |
| N3 pages [del] | allocated=0 freed=0 | 472 | 472 | 0 | 0 |
| N3 pages [del] | allocated=0 freed=1 | 1833 | 1833 | 0 | 0 |
| N3 pages [del] | allocated=0 freed=2 | 63 | 63 | 0 | 0 |
| N3 pages [del] | allocated=1 freed=0 | 2 | 2 | 0 | 0 |
| N3 pages [ins] | allocated=0 freed=0 | 54999 | 54999 | 1407 | 1700 |
| N3 pages [ins] | allocated=0 freed=1 | 98 | 98 | 0 | 0 |
| N3 pages [ins] | allocated=1 freed=0 | 23791 | 23791 | 611 | 352 |
| N3 pages [ins] | allocated=2 freed=0 | 237 | 237 | 0 | 0 |
| N4 divider cell [del] (index cell) | fits | 356 | 356 | 0 | 0 |
| N4 divider cell [del] (index cell) | overflow | 2 | 2 | 0 | 0 |
| N4 divider cell [del] (rowid key) | fits | 2412 | 2412 | 0 | 0 |
| N4 divider cell [ins] (index cell) | fits | 75974 | 75974 | 3592 | 4382 |
| N4 divider cell [ins] (index cell) | overflow | 2975 | 2975 | 34 | 38 |
| N4 divider cell [ins] (rowid key) | fits | 101198 | 101198 | 0 | 0 |
| N4 divider cell [ins] (rowid key) | overflow | 725 | 725 | 0 | 0 |
| N5 editPage [del] | edit | 4682 | 4682 | 0 | 0 |
| N5 editPage [del] | rebuild | 458 | 458 | 0 | 0 |
| N5 editPage [ins] | edit | 233775 | 233775 | 5348 | 5986 |
| N5 editPage [ins] | rebuild | 26222 | 26222 | 296 | 486 |
| N6 balance-shallower [del] | no | 2365 | 2365 | 0 | 0 |
| N6 balance-shallower [del] | shallower | 5 | 5 | 0 | 0 |
| N6 balance-shallower [ins] | no | 79103 | 79103 | 2018 | 2052 |
| N6 balance-shallower [ins] | root-empty-no-room | 10 | 10 | 0 | 0 |
| N6 balance-shallower [ins] | shallower | 12 | 12 | 0 | 0 |

Outcomes whose count differs between C and Rust in the H2 families: **0**. Outcomes exercised by C only in `h3_*` (not exercised by Rust through SQL): `N1 sibling selection [ins] bBulk=1|nOld=1 parent has <2 cells: all children`, `N1 sibling selection [ins] bBulk=1|nOld=2 bulk: right-most child (nxDiv=i-1)`.


## Coverage gaps — outcomes that are "not verified"

* **`bBulk=1`** (N1 `nxDiv=i-2+bBulk` / `i=2-bBulk`, N2 the evening loop stopped by `bBulk`, N3 `nearby=1`): C exercises them in the `h3_*` scripts (CREATE INDEX,
  xfer copy of indexes — lines `bBulk=1` of the coverage table), Rust never through SQL: no engine path opens a `BTREE_BULKLOAD` cursor before step H3.
  They are verified one level lower, against C-derived expectations: `c_align_h2_tests::bulk_append_gives_the_index_shape_of_c_create_index` appends sorted keys
  through `Btree::insert_index_bulk` (`BalCursor.bulk`) and gets, page for page, the cell counts of the index C builds with `CREATE INDEX` (512 / 1024 / 4096-byte
  pages: 52 / 41 / 31 pages). Not compared event by event.
* **S7 `BTREE_PREFORMAT`** (`sqlite3BtreeTransferRow`, used by the xfer optimisation and VACUUM): not implemented; rows are copied through the normal insert (same
  cell bytes; the overflow pages of a transferred row are allocated with `nearby=0` in C). H3.
* **S3 / S3b, the `info.nSize` state of C's cursor**: the b-tree takes it as an argument; the engine derives it for REPLACE (table without index → not parsed) and for
  a two-pass UPDATE that reads no column of the old row (→ not parsed). Every other UPDATE shape is treated as parsed (one-pass: `OP_Rowid` parses the cell; UPSERT
  DO UPDATE: `OP_Rowid`; an unassigned column or a rewritten index: `OP_Column`). UPDATE … FROM and UPDATEs inside trigger programs were not examined separately.
* **I7 "the page is not made writable when the bytes are equal"** (`btreeOverwriteContent`): implemented, not traced (visible only in the journal / WAL frames).
* **Corrupt-input branches** (`SQLITE_CORRUPT` returns of `allocateSpace`, `pageFindSlot`, `defragmentPage`, `freeSpace`, `clearCellOverflow`, `btreeOverwriteCell`):
  ported where they guard memory safety, never exercised by the differ (both engines only see valid files here).
* **Seek avoidance** (`BTCF_AtLast`, cursor already on the right leaf, `BTREE_APPEND` → `biasRight`): no effect on pages; the engine's positioned paths are checked
  only through the page and cell index of every `ins|cell` event.
* **64 KiB pages, unused bytes**: `m_wr_64k` is identical in every decision and in shape, but one page differs in unused bytes — the stale tail of an overflow page
  taken from the free list. With 65536-byte pages the default cache holds ~30 pages; which image of a freed page survives (the one in the cache, whose write was
  dropped by `sqlite3PagerDontWrite`, or the older one on disk) depends on page-cache eviction, which is not modelled.

