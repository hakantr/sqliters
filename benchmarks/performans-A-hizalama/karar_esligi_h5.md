# H5 — FTS5 decision parity (ext/fts5 3.54.1  <->  crates/sqliters-ext/src/fts5_index.rs, fts5.rs; host: crates/sqliters-core)

Scope: every decision of the FTS5 write path. The Rust functions are ports that keep the C names (`fts5WriteAppendTerm` ->
`Fts5Index::write_append_term`, ...); the decisions below are the branch conditions of the C source, and each one has a trace
point with the same text in the instrumented C amalgamation (`h5/ctrace/patch_h5.py`, class `f5`) and in the Rust source
(`sqliters_types::dtrace!`, compiled only with `--cfg sqliters_dtrace`).

## (a) Decision tables (condition -> outcome -> C source)

### A1 Hash flush triggers
| # | condition | outcome | source |
|---|---|---|---|
| F1 | `iRowid < p->iWriteRowid` | `fts5IndexFlush` before the row is written | fts5_index.c:6812 `sqlite3Fts5IndexBeginWrite` |
| F1 | `iRowid == p->iWriteRowid && p->bDelete==0` (same row again, previous write was not its delete) | flush | same |
| F1 | `p->nPendingData > pConfig->nHashSize` (`hashsize`, default 1 MiB; nPendingData = sum of `Fts5HashEntry.nData` growth, 48-byte entry header included) | flush | same; accounting fts5_hash.c:245 `sqlite3Fts5HashWrite` |
| F1 | otherwise | no flush; `iWriteRowid`, `bDelete` updated, `nPendingRow++` for an insert | same |
| F2 | `fts5IndexFlush`: `nPendingData \|\| nContentlessDelete` | `fts5FlushOneHash`, then hash cleared | fts5_index.c:5836 |
| F2 | `flushRc` set by an earlier failed flush | that error again | same |
| -- | a scan of the hash (`pTerm==0` or `FTS5INDEX_QUERY_SCAN`: prefix query without a matching prefix index, 'integrity-check') | size fields of the visited position lists fixed in place, `bDelete = 0` (the next write to the same row flushes) | fts5_index.c:2873 `fts5SegIterHashInit`, fts5_hash.c:568 |
| -- | a term lookup in the hash (`sqlite3Fts5HashQuery`) | copy of the doclist, hash untouched | fts5_hash.c:493 |

### A2 Hash entry contents (per token)
| condition | outcome | source |
|---|---|---|
| new key (index byte `'0'+iIdx` + token) | entry: rowid varint, 1 size byte reserved (detail!=none), `iCol = 0` (full) / `-1` (column) | fts5_hash.c:245 |
| `iRowid != p->iRowid` | previous list closed (`fts5HashAddPoslistSize`: `nPos*2+bDel`, moved up when > 127; detail=none: `00` if deleted, `00 00` if deleted and re-inserted), rowid delta appended | fts5_hash.c:194 |
| `iCol < 0` | `bDel = 1` (delete marker; kept when the row is inserted again in the same flush) | fts5_hash.c:245 |
| detail=full, new column | `0x01`, column varint, position restarts at 0; every token appends `iPos - prevPos + 2` | same |
| detail=column | one varint `iCol - prevCol + 2` per column the term occurs in | same |
| detail=none | `bContent = 1` only | same |
| prefix index `i` with `nChar` characters, token has >= nChar characters | additional entry under index byte `'0'+i+1` with the first nChar characters | fts5_index.c:7004 `sqlite3Fts5IndexWrite` |
| token longer than 32768 bytes | truncated | fts5_storage.c:447 |

### A3 Segment writer (per term / rowid / position list)
| # | condition | outcome | source |
|---|---|---|---|
| F7 | flush, `bSecureDelete==0` and `pgsz >= buf.n + pgidx.n + nDoclist + 1` | term + whole doclist copied to the current leaf | fts5_index.c:5627 `fts5FlushOneHash` |
| F7 | otherwise | doclist written rowid by rowid; a position list larger than the space left is cut at varint boundaries (`fts5PoslistPrefix`), leaf flushed when `buf.n + pgidx.n >= pgsz` | same |
| F9 | `buf.n + pgidx.n + nTerm + 2 >= pgsz` and the leaf holds more than its header | `fts5WriteFlushLeaf` before the term | fts5_index.c:4483 `fts5WriteAppendTerm` |
| F9 | first term on a leaf, `pgno != 1` | `%_idx` row for the previous split key is written (`fts5WriteBtreeTerm` -> `fts5WriteFlushBtree`); new split key = term prefix one byte longer than the common prefix with the previous term | fts5_index.c:4331, 4302 |
| F9 | not the first term on the leaf | prefix-compressed against the previous term (`nPrefix`, suffix) ; pgidx gets the offset delta | fts5_index.c:4483 |
| F10 | leaf flushed without a term | `nEmpty++`; a `0x00` is appended to the doclist index if it has started and the leaf has no rowid | fts5_index.c:4437, 4347 |
| -- | first rowid on a leaf | header bytes 0-1 = offset, full rowid written, `fts5WriteDlidxAppend` | fts5_index.c:4564 `fts5WriteAppendRowid`, 5627 |
| -- | dlidx page `buf.n >= pgsz` | page written with flag 1, parent level started / extended with (pgno, first rowid) | fts5_index.c:4377 |
| F11 | `fts5WriteFlushDlidx`: dlidx started and `nEmpty >= 4` (`FTS5_MIN_DLIDX_SIZE`) | dlidx pages written, bit 0 of `%_idx.pgno` set | fts5_index.c:4278 |
| F11 | otherwise | dlidx buffer dropped | same |
| -- | merge: `buf.n + pgidx.n + n >= pgsz` while appending position-list data | chunk cut at a varint boundary, leaf flushed (also for an empty chunk on a full leaf) | fts5_index.c:4599 `fts5WriteAppendPoslistData` |
| F8 | `pgnoLast > 0` after `fts5WriteFinish` | segment appended to level 0 (`iOrigin1 = iOrigin2 = nOriginCntr++`, `nEntry = nPendingRow` for V2 structures) | fts5_index.c:5627 |

### A4 Segment id and level placement
| # | condition | outcome | source |
|---|---|---|---|
| F12 | always | lowest segment id not used by any segment of the structure; `nSegment >= 2000` -> SQLITE_FULL | fts5_index.c:4143 `fts5AllocateSegid` |
| F13 | (a) nearest non-empty level below `iLvl` has a segment at least as large as the one just written | promotion target = that level, limit = its largest segment | fts5_index.c:1548 `fts5StructurePromote` |
| F13 | otherwise (b) | target = `iLvl`, limit = size of the new segment | same |
| F14 | target level not merging; walking the higher levels, newest segment first: size <= limit and level not merging | segment moved to the front of the target level | fts5_index.c:1505 `fts5StructurePromoteTo` |

### A5 Automerge / crisis merge / merge input selection / work per call
| # | condition | outcome | source |
|---|---|---|---|
| F15 | `nAutomerge > 0` | `nWork = (nWrite+nLeaf)/64 - nWrite/64` (`FTS5_WORK_UNIT`), `nWriteCounter += nLeaf`, `fts5IndexMerge(nRem = 64*nWork*nLevel, nMin = nAutomerge)`; nLeaf = leaves of the new segment + contentless deletes | fts5_index.c:5043 |
| F16 | first level with `nMerge != 0` | that level (ongoing merge) | fts5_index.c:4987 `fts5IndexMerge` |
| F16 | otherwise level with the most segments, if `>= nMin` | that level | same |
| F16 | `nBest < nMin` | `fts5IndexFindDeleteMerge`: level whose `nEntryTombstone*100/nEntry >= nDeleteMerge` is highest (contentless_delete only), else stop | fts5_index.c:4948 |
| F17 | `pLvl->nMerge != 0` | continue: writer on the existing output segment at `pgnoLast+1`, `iBtPage = 0` | fts5_index.c:4786 `fts5IndexMergeLevel` |
| F17 | otherwise | new output segment on level+1 (new level if needed), origins of the inputs | same |
| -- | entry with `nPos==0` and (`bOldest` or no delete flag) | dropped (key annihilation); `bOldest` = output is the only segment of the last level | same |
| -- | tombstoned rowid (contentless_delete) | skipped by the iterator | fts5_index.c:3360 `fts5MultiIterIsDeleted` |
| F18 | new term reached and `nLeafWritten > nRem` (only with a page budget) | stop; `fts5TrimSegments`, `nMerge = nInput` | same |
| F18 | inputs exhausted | input segments removed (`%_data` range, tombstone pages, `%_idx`), `nEntry` summed, empty output dropped | same |
| F20 | input at EOF | `pgnoFirst = pgnoLast = 0` | fts5_index.c:4708 `fts5TrimSegments` |
| F20 | input not at EOF | pages before the current term's leaf deleted, that leaf rewritten to start with the current term | same |
| F19 | level has `>= nCrisisMerge` segments | merged completely into the next level, promote, next level checked | fts5_index.c:5064 |
| F26 | 'optimize': one segment without tombstones, or nothing | nothing | fts5_index.c:5856, 5925 |
| F26 | all segments on one level (or all but one being merged) | merge that level, 1000 leaves per call until empty | same |
| F26 | otherwise | all segments moved to a new last level (oldest first), merged | same |
| F27 | 'merge' N>0 | `fts5IndexMerge(N, nUsermerge)`; structure written only if a merge ran | fts5_index.c:5961 |
| F27 | 'merge' N<0 | optimize structure first, `nMin = 1`, `nMerge = -N` | same |

### A6 Secure delete
| # | condition | outcome | source |
|---|---|---|---|
| F7 | flush with `bSecureDelete`, entry with delete flag (detail=none: `00`) | `fts5FlushSecureDelete(term, rowid)` before anything is written for the entry; a pure delete writes nothing to the new segment, delete+insert is written without the flag | fts5_index.c:5627, 5571 |
| -- | `iVersion != 5` | `%_config` version 5, `iCookie++` | fts5_index.c:5571 |
| F21 | entry not found in any segment | nothing | same |
| F21 | found, entry has the delete flag | replaced by a delete marker (`iDelta`, `0x01`) | fts5_index.c:5262 `fts5DoSecureDelete` |
| F21 | found, not last in doclist | entry removed, next rowid delta fixed | same |
| F21 | last in doclist and only entry of its term on that leaf | term removed; following term re-encoded against the previous one; footer entry dropped | same |
| F23 | entry at offset 4 of a continuation leaf and all leaves back to the term's leaf are empty | term's leaf truncated at the term (footer entry removed; `%_idx` row deleted when the leaf loses its last term) | same |
| F22 | position list continues on following leaves | those leaves emptied (`00 00 00 04`) or shifted (`fts5SecureDeleteOverflow`); detail=none stops at the first leaf with content | fts5_index.c:5164 |
| -- | leaf other than page 1 left without terms | `%_idx` row for it deleted | fts5_index.c:5132 |

### A7 Tombstones (contentless_delete=1)
| # | condition | outcome | source |
|---|---|---|---|
| -- | delete of a row whose `%_docsize.origin != 0` | every segment whose origin range contains it gets the rowid in its tombstone hash; `nEntryTombstone++` on the first (newest level last) | fts5_index.c:8166 |
| F24 | page has room (`nElem < nSlot/2`) and key size fits | rowid added to the page | fts5_index.c:8108, 7873 |
| F25 | no pages yet | 1 page, 32 slots, 4-byte keys (8 if rowid > 0xFFFFFFFF) | fts5_index.c:8006 |
| F25 | 1 page with `nElem <= nSlotPerPage/4` | 1 page with `max(nElem*4, 32)` slots | same |
| F25 | otherwise | `nPg*2+1` pages of `max(32, (pgsz-8)/szKey)` slots; again doubled+1 while a page overflows; structure record written | same |

### A8 Transaction hooks
| # | hook | decision | source |
|---|---|---|---|
| -- | xBegin | `sqlite3Fts5IndexReset` (cached structure dropped if data_version changed) | fts5_main.c:2130, 773 |
| -- | xSync | `sqlite3Fts5FlushToDisk`: totals saved when `bTotalsValid`, then `fts5IndexFlush`; `last_insert_rowid` restored | fts5_main.c:2117, fts5_storage.c:1501 |
| F4 | xSavepoint(i) | flush to disk, `iSavepoint = i+1` | fts5_main.c:3166 |
| F5 | xRelease(i), `i+1 < iSavepoint` | flush to disk, `iSavepoint = i` | fts5_main.c:3183 |
| F5 | xRelease(i) otherwise | nothing | same |
| F6 | xRollbackTo(i), `i+1 <= iSavepoint` | `pgsz = 0`, storage rollback (hash discarded, structure dropped, totals invalid) | fts5_main.c:3201 |
| F3 | xRollback | storage rollback, `pgsz = 0` | fts5_main.c:2153 |
| -- | fts5vocab cursor open | flush of the FTS5 table it reads | fts5_vocab.c:339 |
| -- | queries, 'integrity-check' | no flush (the hash is read as the newest source) | fts5_index.c:3960, 8669 |
| host | statement with a statement journal | `sqlite3VtabSavepoint(BEGIN, nSavepoint+nStatement-1)` before `sqlite3BtreeBeginStmt` | vdbe.c:4306 |
| host | SAVEPOINT name | `sqlite3VtabSavepoint(BEGIN, nStatement+nSavepoint)` before the savepoint exists; a transaction savepoint is not counted (its ROLLBACK TO is number -1) | vdbe.c:4016 |
| host | OP_VBegin | xBegin at statement start (also for zero rows); xSavepoint(iSvpt-1) once if `iSvpt = nStatement+nSavepoint > 0` | vdbe.c:8472, vtab.c:1079 |
| host | DML statement in autocommit mode | commit (xSync) first, then the statement's own row count is stored | vdbeaux.c:3498 |

### A9 Statements on the shadow tables (each one is counted in `total_changes()` / sets `changes()`)
| operation | statement | source |
|---|---|---|
| leaf / dlidx / tombstone / averages / structure write | `REPLACE INTO %_data(id, block) VALUES(?,?)` | fts5_index.c:972 |
| segment removal, trim | `DELETE FROM %_data WHERE id>=? AND id<=?`, `DELETE FROM %_idx WHERE segid=?` | fts5_index.c:996, 1017 |
| split key | `INSERT INTO %_idx(segid,term,pgno) VALUES(?,?,?)` | fts5_index.c:4661 |
| secure delete | `DELETE FROM %_idx WHERE (segid, (pgno/2)) = (?1, ?2)`, `REPLACE INTO %_config VALUES('version', 5)` | fts5_index.c:5132, 5571 |
| row | `INSERT / REPLACE INTO %_content VALUES(...)`, `REPLACE INTO %_docsize VALUES(?,?[,?])`, `DELETE FROM %_docsize / %_content WHERE id=?` | fts5_storage.c:82 |
| rowid for a contentless row | `REPLACE INTO %_docsize VALUES(NULL,NULL[,NULL])` | fts5_storage.c:967 |
| config change | `REPLACE INTO %_config VALUES(?,?)` + in-place `sqlite3_blob_write` of the cookie (not counted) | fts5_storage.c:1522, fts5_index.c:7818 |
| 'delete-all' / 'rebuild' | `DELETE FROM %_data; DELETE FROM %_idx; [DELETE FROM %_docsize;] [DELETE FROM %_content;]`, reinit, version row | fts5_storage.c:833, 869 |
| placeholder row, contentless_delete=1 | parameter 3 (`origin`) of the cached statement is not rebound by `fts5StorageNewRowid`: the row is `(NULL, NULL, <origin of the previous %_docsize row>)`, NULL before the first one | fts5_storage.c:741 (bind 3), :967 (binds 1, 2 only) |

### A10 Rules outside the trace classes (found by the API fuzz `h5/f5api.py` and the byte fuzz `h5/fz5.py`; verified by output / file comparison, not by trace events)
| condition | outcome | source |
|---|---|---|
| xSavepoint / xRelease / xRollbackTo returns an error | statement fails with `sqlite3ErrStr(rc)`; the module's message is not imported | vtab.c:1136 `sqlite3VtabSavepoint`, vdbe.c `abort_due_to_error` |
| a statement the module steps on a shadow table fails | only its result code comes back (`p->rc = sqlite3_reset(..)`): "constraint failed", not "UNIQUE constraint failed: ft_idx..." | fts5_index.c:972ff, fts5_storage.c |
| the module detects corruption | extended result code SQLITE_CORRUPT_VTAB (267) | fts5Int.h:120 `FTS5_CORRUPT` |
| ORDER BY rank plan (`fts5CursorFirstSorted`), error while the sorter statement is stepped | primary result code only (`sqlite3_step` masks with `db->errMask`), message = `sqlite3ErrStr` | fts5_main.c:1096 `fts5SorterNext` |
| rank function unknown | error when the sorter statement is prepared (ORDER BY rank plan, also with no row) or when the rank column of a row is read; otherwise none | fts5_main.c:1202 `fts5FindRankFunction` |
| bm25 / snippet ask for the size of a row (`xColumnSize`), `%_docsize` row missing or undecodable | FTS5_CORRUPT | fts5_storage.c:1425 `sqlite3Fts5StorageDocsize` |
| bm25: `nTotalRow <= 0` (i64) | FTS5_CORRUPT | fts5_storage.c:1480 `sqlite3Fts5StorageRowCount` |
| statement fails with rc == SQLITE_CORRUPT (no extended bits) and `autoCommit==0` | `SQLITE_CorruptRdOnly`: `OP_Transaction` with a write flag fails with SQLITE_CORRUPT; COMMIT -> SQLITE_CORRUPT + rollback; cleared by rollback | vdbe.c:9507, :4268, vdbeaux.c:3428, main.c:1553 |
| DML statement, commit-time xSync fails | `changes()` 0, the statement's own rows are not added to `total_changes()`; the rows of the nested statements stay | vdbeaux.c `sqlite3VdbeHalt` (`p->nChange = 0`) |
| fts5vocab xBestIndex | idxNum = colUsed \| 0x100 (usable `term =`) \| 0x200 (`term >=`, `>`) \| 0x400 (`term <=`, `<`); cost 100 / 1000000 halved per range end; `ORDER BY term [ASC]` alone consumed; xFilter visits only terms inside the limits (non-TEXT value = no limit) | fts5_vocab.c:275, :640 |
| table-valued-function arguments | `colUsed` gets the hidden columns that received an argument | whereexpr.c:1972 `sqlite3WhereTabFuncArgs` |
| pager, page written while savepoints are open | sub-journal record only if some savepoint lacks the page; a page entering the rollback journal now is added to every savepoint's set without a record; if the outermost savepoint that needs the record is not the innermost one, every savepoint inside it loses `bTruncateOnRelease` | pager.c:1063 `subjRequiresPage`, `pagerAddPageToRollbackJournal`, `pager_write` |
| pager, RELEASE of savepoint n | `bTruncateOnRelease` set: sub-journal cut back to the savepoint's first record; else all records stay | pager.c:7109 |
| pager, ROLLBACK TO savepoint n | main-journal part, then every sub-journal record from the savepoint's first one (first record of a page wins); the savepoint stays open with its page set and the records stay; a page fetched NOCONTENT while it was open has no record of its own | pager.c `pagerPlaybackSavepoint`, :5569 `getPageNormal` (`addToSavepointBitvecs`) |

## (b) Decision trace: event-by-event comparison from an empty file (h5/trf5.py over h5/sc_f5, generator h5/gen_f5.py)

Compared per script: every event of class f5 (decisions + every shadow write with table, key, length, FNV-1a 64 of the blob) interleaved with the
b-tree classes ins / bal / alloc / free (and xf, ridx, vac, xfd, rt), stdout, error messages, integrity_check by both engines on both files,
the dbstat shape and the raw file bytes.

| family | scripts | trace equal | fully equal | byte-identical files | events C | events Rust | first mismatching decision (script: C / Rust) |
|---|---|---|---|---|---|---|---|
| cfg | 40 | 40 | 40 | 40 | 502379 | 502379 | - |
| cmd | 20 | 19 | 19 | 20 | 586538 | 586511 | cmd_rename_drop: `f5|hook|savepoint i=0` / `ins|cell|root=1 pg=1 idx=0 o=replace-drop sz=61 old=62` |
| del | 22 | 22 | 22 | 22 | 479642 | 479642 | - |
| doc | 10 | 10 | 10 | 10 | 610351 | 610351 | - |
| multi | 2 | 0 | 0 | 2 | 50398 | 50416 | multi_tables_auto: `scan count C 14 R 13` / `` |
| pg | 8 | 8 | 8 | 8 | 592057 | 592057 | - |
| qry | 4 | 4 | 4 | 4 | 31911 | 31911 | - |
| tx | 10 | 10 | 10 | 10 | 189642 | 189642 | - |
| vol | 15 | 15 | 15 | 15 | 1059572 | 1059572 | - |
| **total** | 131 | 128 | 128 | 131 | 4102490 | 4102481 | |

## (c) Coverage: times each decision outcome was exercised (C / Rust)

| decision | outcome | C | Rust |
|---|---|---|---|
| A1 allocateBtreePage | empty-trunk | 1050 | 1050 |
| A1 allocateBtreePage | extend | 5270 | 5270 |
| A1 allocateBtreePage | leaf | 5096 | 5096 |
| B1 balance() loop step [del] | nonroot | 8988 | 8988 |
| B1 balance() loop step [del] | root-stop | 49051 | 49051 |
| B1 balance() loop step [del] | stop | 847 | 847 |
| B1 balance() loop step [ins] | deeper | 703 | 703 |
| B1 balance() loop step [ins] | nonroot | 14224 | 14224 |
| B1 balance() loop step [ins] | quick | 3757 | 3757 |
| B1 balance() loop step [ins] | root-stop | 17022 | 17022 |
| B1 balance() loop step [ins] | stop | 932 | 932 |
| B2 balance_deeper [ins] | called | 703 | 703 |
| D1 delete trigger nFree*3>usable*2 | balance | 49877 | 49877 |
| D1 delete trigger nFree*3>usable*2 | no-balance | 147312 | 147312 |
| D2 interior cell replaced by predecessor | fits | 21 | 21 |
| D3 second balance at the interior page | called | 21 | 21 |
| F1 freePage2 | leaf | 5642 | 5642 |
| F1 freePage2 | new-trunk | 1151 | 1151 |
| F1 sqlite3Fts5IndexBeginWrite (delete) | flush: nPendingData>nHashSize | 132 | 132 |
| F1 sqlite3Fts5IndexBeginWrite (delete) | flush: rowid<iWriteRowid | 8664 | 8664 |
| F1 sqlite3Fts5IndexBeginWrite (delete) | flush: same rowid, bDelete==0 | 125 | 125 |
| F1 sqlite3Fts5IndexBeginWrite (delete) | no flush | 10956 | 10956 |
| F1 sqlite3Fts5IndexBeginWrite (insert) | flush: nPendingData>nHashSize | 822 | 822 |
| F1 sqlite3Fts5IndexBeginWrite (insert) | flush: rowid<iWriteRowid | 249 | 249 |
| F1 sqlite3Fts5IndexBeginWrite (insert) | flush: same rowid, bDelete==0 | 72 | 72 |
| F1 sqlite3Fts5IndexBeginWrite (insert) | no flush | 115997 | 115997 |
| F10 fts5WriteFlushLeaf | leaf with terms | 95398 | 95398 |
| F10 fts5WriteFlushLeaf | term-less leaf (doclist continues) | 11941 | 11941 |
| F11 fts5WriteFlushDlidx | doclist index dropped | 13161 | 13161 |
| F11 fts5WriteFlushDlidx | doclist index written (>=4 term-less leaves) | 436 | 436 |
| F11 fts5WriteFlushDlidx | no doclist index | 81801 | 81801 |
| F12 fts5AllocateSegid | lowest free id | 41204 | 41204 |
| F13 fts5StructurePromote | condition (a): promote into a lower level | 5 | 5 |
| F13 fts5StructurePromote | condition (b): this level | 40412 | 40412 |
| F14 fts5StructurePromoteTo | segment promoted | 1084 | 1084 |
| F15 fts5IndexAutomerge | no work quantum | 37320 | 37320 |
| F15 fts5IndexAutomerge | work quanta | 716 | 716 |
| F16 fts5IndexMerge level choice | delete-merge level (fts5IndexFindDeleteMerge) | 23 | 23 |
| F16 fts5IndexMerge level choice | level with >= nMin segments / ongoing merge | 972 | 972 |
| F16 fts5IndexMerge level choice | no level (stop) | 704 | 704 |
| F17 fts5IndexMergeLevel start | continue ongoing merge | 43 | 43 |
| F17 fts5IndexMergeLevel start | new output segment | 3407 | 3407 |
| F18 fts5IndexMergeLevel end | inputs exhausted (segments removed) | 3391 | 3391 |
| F18 fts5IndexMergeLevel end | stopped at nRem (inputs trimmed) | 59 | 59 |
| F19 fts5IndexCrisismerge | level has >= nCrisisMerge segments | 2425 | 2425 |
| F2 sqlite3Fts5IndexSync -> fts5IndexFlush | flush (contentless deletes only) | 1719 | 1719 |
| F2 sqlite3Fts5IndexSync -> fts5IndexFlush | flush (pending data) | 32575 | 32575 |
| F2 sqlite3Fts5IndexSync -> fts5IndexFlush | nothing pending | 1074 | 1062 |
| F20 fts5TrimSegments | first leaf rewritten | 285 | 285 |
| F20 fts5TrimSegments | input fully consumed | 2 | 2 |
| F21 fts5DoSecureDelete | entry removed, next rowid delta fixed | 13207 | 13207 |
| F21 fts5DoSecureDelete | entry removed, next rowid delta fixed (poslist overflows page) | 415 | 415 |
| F21 fts5DoSecureDelete | last in doclist | 1801 | 1801 |
| F21 fts5DoSecureDelete | last in doclist (poslist overflows page) | 629 | 629 |
| F21 fts5FlushSecureDelete | entry not found | 6 | 6 |
| F22 fts5SecureDeleteOverflow | empty | 373 | 373 |
| F22 fts5SecureDeleteOverflow | none-stop | 287 | 287 |
| F22 fts5SecureDeleteOverflow | shift | 676 | 676 |
| F23 secure delete: term removed from earlier leaf | leaf keeps other terms | 1 | 1 |
| F23 secure delete: term removed from earlier leaf | leaf loses last term (-> %_idx row deleted) | 164 | 164 |
| F24 fts5IndexTombstoneAdd | added to existing page | 3773 | 3773 |
| F25 fts5IndexTombstoneRebuild | rehash failed -> more pages (several pages, key 4) | 1 | 1 |
| F25 fts5IndexTombstoneRebuild | rehash failed -> more pages (several pages, key 8) | 1 | 1 |
| F25 fts5IndexTombstoneRebuild | rehash ok (1 page, key 4) | 344 | 344 |
| F25 fts5IndexTombstoneRebuild | rehash ok (1 page, key 8) | 4 | 4 |
| F25 fts5IndexTombstoneRebuild | rehash ok (several pages, key 4) | 13 | 13 |
| F25 fts5IndexTombstoneRebuild | rehash ok (several pages, key 8) | 2 | 2 |
| F26 sqlite3Fts5IndexOptimize | merge | 30 | 30 |
| F26 sqlite3Fts5IndexOptimize | nothing to do | 7 | 7 |
| F27 sqlite3Fts5IndexMerge | merge attempt | 51 | 51 |
| F27 sqlite3Fts5IndexMerge | no structure to merge | 2 | 2 |
| F28 sqlite3Fts5IndexSetCookie | config change | 149 | 149 |
| F29 sqlite3Fts5StorageDeleteAll | reinit | 9 | 9 |
| F3 sqlite3Fts5IndexRollback | discard hash + structure | 754 | 755 |
| F30 shadow write | config | 31 | 31 |
| F30 shadow write | content | 100207 | 100207 |
| F30 shadow write | data | 204389 | 204389 |
| F30 shadow write | docsize | 116714 | 116714 |
| F30 shadow write | idx | 95398 | 95398 |
| F31 shadow delete | content | 15668 | 15668 |
| F31 shadow delete | data | 40128 | 40128 |
| F31 shadow delete | docsize | 19696 | 19696 |
| F31 shadow delete | idx | 39997 | 39997 |
| F4 xSavepoint | flush to disk | 392 | 381 |
| F5 xRelease | flush to disk (iSavepoint+1<pTab->iSavepoint) | 9 | 8 |
| F5 xRelease | no-op | 337 | 333 |
| F6 xRollbackTo | storage rollback (iSavepoint+1<=pTab->iSavepoint) | 114 | 114 |
| F7 fts5FlushOneHash | plain path | 35770 | 35770 |
| F7 fts5FlushOneHash | secure-delete path | 2027 | 2027 |
| F8 fts5FlushOneHash result | new level-0 segment | 37056 | 37056 |
| F8 fts5FlushOneHash result | no segment written | 741 | 741 |
| F9 fts5WriteAppendTerm | first term of the segment | 40463 | 40463 |
| F9 fts5WriteAppendTerm | first term on a later leaf (-> %_idx term) | 35029 | 35029 |
| F9 fts5WriteAppendTerm | no common prefix | 23030 | 23030 |
| F9 fts5WriteAppendTerm | prefix-compressed | 1202120 | 1202120 |
| I1 sqlite3BtreeInsert: existing cell? | new | 423238 | 423238 |
| I1 sqlite3BtreeInsert: existing cell? | overwrite | 113 | 113 |
| I1 sqlite3BtreeInsert: existing cell? | replace-drop | 55325 | 55325 |
| I1 sqlite3BtreeInsert: existing cell? | replace-same | 41501 | 41501 |
| I2 insertCell/allocateSpace [del] | freeblock | 7091 | 7091 |
| I2 insertCell/allocateSpace [del] | gap | 5447 | 5447 |
| I2 insertCell/allocateSpace [ins] | defrag | 4053 | 4053 |
| I2 insertCell/allocateSpace [ins] | freeblock | 182029 | 182029 |
| I2 insertCell/allocateSpace [ins] | gap | 308547 | 308547 |
| I2 insertCell/allocateSpace [ins] | overflow | 17981 | 17981 |
| N1 sibling selection [del] bBulk=0 | nOld=2 parent has <2 cells: all children | 1235 | 1235 |
| N1 sibling selection [del] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 3803 | 3803 |
| N1 sibling selection [del] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 3009 | 3009 |
| N1 sibling selection [del] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 941 | 941 |
| N1 sibling selection [ins] bBulk=0 | nOld=1 parent has <2 cells: all children | 378 | 378 |
| N1 sibling selection [ins] bBulk=0 | nOld=2 parent has <2 cells: all children | 675 | 675 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 888 | 888 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 10404 | 10404 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 1879 | 1879 |
| N2 distribution nOld->k [del] | 2->1 leafData=0 | 40 | 40 |
| N2 distribution nOld->k [del] | 2->1 leafData=1 | 377 | 377 |
| N2 distribution nOld->k [del] | 2->2 leafData=0 | 17 | 17 |
| N2 distribution nOld->k [del] | 2->2 leafData=1 | 801 | 801 |
| N2 distribution nOld->k [del] | 3->1 leafData=1 | 202 | 202 |
| N2 distribution nOld->k [del] | 3->2 leafData=0 | 12 | 12 |
| N2 distribution nOld->k [del] | 3->2 leafData=1 | 3391 | 3391 |
| N2 distribution nOld->k [del] | 3->3 leafData=0 | 2 | 2 |
| N2 distribution nOld->k [del] | 3->3 leafData=1 | 4146 | 4146 |
| N2 distribution nOld->k [ins] | 1->1 leafData=1 | 3 | 3 |
| N2 distribution nOld->k [ins] | 1->2 leafData=0 | 55 | 55 |
| N2 distribution nOld->k [ins] | 1->2 leafData=1 | 308 | 308 |
| N2 distribution nOld->k [ins] | 1->3 leafData=1 | 12 | 12 |
| N2 distribution nOld->k [ins] | 2->2 leafData=0 | 77 | 77 |
| N2 distribution nOld->k [ins] | 2->2 leafData=1 | 397 | 397 |
| N2 distribution nOld->k [ins] | 2->3 leafData=0 | 11 | 11 |
| N2 distribution nOld->k [ins] | 2->3 leafData=1 | 186 | 186 |
| N2 distribution nOld->k [ins] | 2->4 leafData=1 | 4 | 4 |
| N2 distribution nOld->k [ins] | 3->2 leafData=1 | 70 | 70 |
| N2 distribution nOld->k [ins] | 3->3 leafData=0 | 248 | 248 |
| N2 distribution nOld->k [ins] | 3->3 leafData=1 | 10175 | 10175 |
| N2 distribution nOld->k [ins] | 3->4 leafData=0 | 33 | 33 |
| N2 distribution nOld->k [ins] | 3->4 leafData=1 | 2572 | 2572 |
| N2 distribution nOld->k [ins] | 3->5 leafData=1 | 73 | 73 |
| N3 pages [del] | allocated=0 freed=0 | 4966 | 4966 |
| N3 pages [del] | allocated=0 freed=1 | 3820 | 3820 |
| N3 pages [del] | allocated=0 freed=2 | 202 | 202 |
| N3 pages [ins] | allocated=0 freed=0 | 10900 | 10900 |
| N3 pages [ins] | allocated=0 freed=1 | 70 | 70 |
| N3 pages [ins] | allocated=1 freed=0 | 3165 | 3165 |
| N3 pages [ins] | allocated=2 freed=0 | 89 | 89 |
| N4 divider cell [del] (index cell) | fits | 33 | 33 |
| N4 divider cell [del] (rowid key) | fits | 12484 | 12484 |
| N4 divider cell [ins] (index cell) | fits | 702 | 702 |
| N4 divider cell [ins] (rowid key) | fits | 29588 | 29588 |
| N5 editPage [del] | edit | 20975 | 20975 |
| N5 editPage [del] | rebuild | 530 | 530 |
| N5 editPage [ins] | edit | 43178 | 43178 |
| N5 editPage [ins] | rebuild | 1336 | 1336 |
| N6 balance-shallower [del] | no | 8569 | 8569 |
| N6 balance-shallower [del] | shallower | 419 | 419 |
| N6 balance-shallower [ins] | no | 14221 | 14221 |
| N6 balance-shallower [ins] | root-empty-no-room | 3 | 3 |
| R1 ChooseLeaf (per level, leaf insert) | contain | 53 | 53 |
| R10 node number assigned by nodeWrite | new node | 7 | 7 |
| R12 shadow write | node | 174 | 174 |
| R12 shadow write | parent | 7 | 7 |
| R12 shadow write | rowid | 333 | 333 |
| R13 shadow delete | rowid | 2 | 2 |
| R14 rtreeBestIndex | idxNum=1 constraints=0 | 26 | 52 |
| R14 rtreeBestIndex | idxNum=2 constraints=0 | 2 | 0 |
| R15 rtreeFilter | idxNum=1 | 26 | 26 |
| R15 rtreeFilter | idxNum=2 | 2 | 0 |
| R16 search point | entry -> sPoint | 4 | 2 |
| R16 search point | node -> sPoint | 4 | 0 |
| R17 row returned | row | 0 | 2 |
| R2 AdjustTree: ancestor cell | keep | 52 | 52 |
| R3 rtreeInsertCell (interior) | fit | 7 | 7 |
| R3 rtreeInsertCell (leaf) | fit | 160 | 160 |
| R3 rtreeInsertCell (leaf) | split | 4 | 4 |
| R4 SplitNode (leaf) | nonroot | 1 | 1 |
| R4 SplitNode (leaf) | root | 3 | 3 |
| R5 splitNodeStartree axis | dim=0 | 3 | 3 |
| R5 splitNodeStartree axis | dim=1 | 1 | 1 |
| R6 splitNodeStartree distribution | in between | 2 | 2 |
| R6 splitNodeStartree distribution | nLeft==nCell-RTREE_MINCELLS | 2 | 2 |
| R7 tree depth | grow | 3 | 3 |
| R8 deleteCell (leaf) | root | 2 | 2 |
| X1 OP_OpenWrite OPFLAG_BULKCSR | bulk cursor opened | 2 | 2 |

### Outcomes not exercised by the scenario set ("not verified" by trace)

- none of the listed outcomes
- F6 xRollbackTo -> no-op (`iSavepoint+1 > pTab->iSavepoint`): not reachable through SQL - the host only calls xRollbackTo for a table whose `VTable.iSavepoint` is above the savepoint (vtab.c `sqlite3VtabSavepoint`), and every such table has had xSavepoint called for it.
- not in the table because they have no trace point of their own: the per-token hash decisions of A2 (verified through the FNV-1a hash of every leaf written), `fts5AllocateSegid` with >= 2000 segments (SQLITE_FULL), dlidx trees of more than 2 levels beyond what `doc_dlidx_pgsz32_deep` builds, 8-byte tombstone keys beyond `vol_tomb64`.
