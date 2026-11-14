# H6 + H6b — karar eşliği (decision parity): the page cache decides like C

Sources: `sqlite/src/pcache.c`, `sqlite/src/pcache1.c`, `sqlite/src/pager.c`, `sqlite/src/btree.c`, `sqlite/src/pragma.c`
(SQLite 3.54.1; line numbers of that tree). Rust: `crates/sqliters-pager/src/pcache.rs` (new), `crates/sqliters-pager/src/lib.rs`,
`crates/sqliters-btree/src/pins.rs` (new), `balance.rs`, `insert.rs`, `lib.rs`, `crates/sqliters-core/src/engine.rs`, `schema.rs`.

## 1. Decision tables (C source: condition -> outcome)

### A. Cache size arithmetic
| # | condition (C) | outcome | file:line | Rust |
|---|---|---|---|---|
| A1 | `szCache >= 0` | cache pages = `szCache` | pcache.c:278-281 | `PageCache::number_of_cache_pages` |
| A2 | `szCache < 0` | `(-1024*szCache)/(szPage+szExtra)`, capped at 1e9; `szExtra` = `ROUND8(sizeof(MemPage))` = **136** in the reference build (measured: `-2000` -> 483 pages of 4096, 1765 of 1024, 123 of 16384, 62 of 32768, 3160 of 512) | pcache.c:288 | same, `PCACHE_SZ_EXTRA = 136` |
| A3 | `pcache1Cachesize(n)`: `nMax = n`, `n90pct = nMax*9/10`, `mxPinned = nMaxPage + 10 - nMinPage` (= `nMax`: private group, `nMin = 10`), then `pcache1EnforceMaxPage` | limits; clean unreferenced pages beyond `nMax` are freed at once | pcache1.c:816-834, 795-797 | `PageCache::pcache1_cachesize` |
| A4 | `sqlite3PcacheSetPageSize`: `xCachesize(pNew, numberOfCachePages(pCache))` runs BEFORE `pCache->szPage = szPage` | the new cache is sized with the OLD page size until the next `sqlite3PcacheSetCachesize` | pcache.c:363-373 | `PageCache::set_page_size` (same order) |
| A5 | `sqlite3BtreeOpen`: `sqlite3BtreeSetCacheSize(p, SQLITE_DEFAULT_CACHE_SIZE)`; `sqlite3InitOne`: first schema read sets `abs(meta[default_cache_size])` or the default | cache size settled at open and again at the first schema load | btree.c:2841, prepare.c | `Pager::open_with_options`, `Btree::init_schema_cache_size` (called by `Schema::load_for_schema`) |
| A6 | `sqlite3PcacheSetSpillsize(mxPage)`: `mxPage != 0` sets `szSpill` (negative: KiB -> pages); result `max(szSpill, numberOfCachePages)` | `PRAGMA cache_spill` value | pcache.c:875-887 | `PageCache::set_spillsize` |

### B. Fetch of a page that is not in the cache (`sqlite3PcacheFetch` / `pcache1FetchStage2`)
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| B1 | `eCreate = createFlag & pCache->eCreate`; `eCreate` is 1 while the dirty list is non-empty and the cache is purgeable, else 2 | "allocate only if easy" vs "allocate always" | pcache.c:423, 228, 242 | `PageCache::e_create`, `dirty_list_add/remove` |
| B2 | `createFlag==1 && (nPinned>=mxPinned || nPinned>=n90pct)` with `nPinned = nPage - nRecyclable` | refuse (`o=refuse`): the pager calls `sqlite3PcacheFetchStress` | pcache1.c:890-895 | `pcache1_fetch_stage2` -> `Stage2::Refused` |
| B3 | `bPurgeable && LRU not empty && nPage+1 >= nMax` | recycle the LRU tail (`o=recycle rp=`) | pcache1.c:903-919 | same function |
| B4 | otherwise | allocate a new page (`o=alloc`) | pcache1.c:924-926 | same function |
| B5 | (`pcache1UnderMemoryPressure`: page-cache slab / soft heap limit) | not modelled: the reference configuration has neither | pcache1.c:520-526 | — (not verified) |

### C. `sqlite3PcacheFetchStress` (victim choice)
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| C1 | `eCreate==2` | no stress (plain fetch) | pcache.c:451 | cannot occur after B2 |
| C2 | `sqlite3PcachePagecount() <= szSpill` | no victim is looked for (`o=below-spill`) | pcache.c:453 | `Pager::pcache_fetch_stress` |
| C3 | walk from `pSynced` towards the head while `nRef!=0` or `PGHDR_NEED_SYNC`; `pSynced` := where it stops | victim = unreferenced dirty page that needs no journal sync | pcache.c:463-467 | `PageCache::stress_victim` |
| C4 | none found: walk from `pDirtyTail` while `nRef!=0` | victim = any unreferenced dirty page | pcache.c:468-470 | same |
| C5 | victim found: `xStress`; `rc!=SQLITE_OK && rc!=SQLITE_BUSY` -> error, BUSY ignored | page written, or cache grows | pcache.c:481-485 | `pcache_fetch_stress` |
| C6 | then `xFetch(.., 2)` | recycle or allocate (B3/B4) | pcache.c:488 | same |

### D. Dirty list / references (`pcache.c`)
| # | event | effect | file:line | Rust |
|---|---|---|---|---|
| D1 | `sqlite3PcacheMakeDirty` on a clean page | `PCACHE_DIRTYLIST_ADD` at the head; `pSynced` := page if `pSynced==0` and the page has no `NEED_SYNC` (the flag is set AFTER this in `pager_write`); clears `DONT_WRITE` | pcache.c:593-607, 232-257 | `make_dirty`, `dirty_list_add` |
| D2 | `sqlite3PcacheRelease`, last reference, dirty page | `PCACHE_DIRTYLIST_FRONT` | pcache.c:554-558 | `release` |
| D3 | last reference, clean page | `pcache1Unpin`: LRU head — or freed at once when `nPurgeable > nMaxPage` (`o=drop`) | pcache.c:555-556, pcache1.c:1102-1111 | `release`, `pcache1_unpin` |
| D4 | `sqlite3PcacheMakeClean` | removed from the dirty list (`pSynced` moves to its predecessor), flags `DIRTY/NEED_SYNC/WRITEABLE` cleared, unpinned if unreferenced | pcache.c:613-625, 201-231 | `make_clean` |
| D5 | `sqlite3PcacheCleanAll` | `MakeClean` from the head of the dirty list (the oldest dirty page ends up most recently used) | pcache.c:630-636 | `clean_all` |
| D6 | `sqlite3PcacheClearSyncFlags` | every dirty page loses `NEED_SYNC`; `pSynced = pDirtyTail` | pcache.c:653-659 | `clear_sync_flags` |
| D7 | `sqlite3PcacheMove` (`sqlite3PagerRekey` in `balance_nonroot`: pairwise swap through `pgnoTemp = PENDING_BYTE/pageSize+1`) | objects swap page numbers; `DIRTY && NEED_SYNC` -> `FRONT` on each rekey; a cursor on the page keeps the object | pcache.c:664-685, btree.c:8747-8770 | `PageCache::rekey_balance`, `Btree::vc_renumber` |
| D8 | `sqlite3PcacheTruncate(pgno)` | dirty pages above are made clean, pages above leave the cache | pcache.c:696-724 | `truncate` |
| D9 | `pcache1Shrink` | every unpinned page is freed | pcache1.c:841-853 | `shrink` (`PRAGMA shrink_memory`) |

### E. `pagerStress` (pager.c:4676-4735)
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| E1 | `doNotSpill && ((doNotSpill & (ROLLBACK|OFF)) || (pPg->flags & NEED_SYNC))` | return OK without writing (`o=nospill`) | pager.c:4690-4695 | `Pager::pager_stress` |
| E2 | WAL mode | `subjournalPageIfRequired`, then `pagerWalFrames(pPg, 0, 0)` (one uncommitted frame) | pager.c:4699-4704 | `subjournal_page_if_required`, `pager_wal_frames_one` |
| E3 | rollback mode, `NEED_SYNC` or `eState==PAGER_WRITER_CACHEMOD` | `syncJournal(pPager, 1)` first (`o=sync+write`) | pager.c:4715-4719 | `sync_journal(true)` |
| E4 | rollback mode otherwise | `pager_write_pagelist(pPg)` only (`o=write`) | pager.c:4722-4725 | `pager_write_pagelist` |
| E5 | success | `sqlite3PcacheMakeClean`; `aStat[PAGER_STAT_SPILL]++` | pager.c:4697, 4729-4732 | same |
| E6 | `SPILLFLAG_OFF` set/cleared by `sqlite3PagerSetFlags(PAGER_CACHESPILL)`; `SPILLFLAG_ROLLBACK` during `pager_playback_one_page`'s fetch; `SPILLFLAG_NOSYNC` in `pagerWriteLargeSector` | when spilling is suppressed | pager.c:3707-3711, 2506-2509, 6218 | `set_cache_spill`, `apply_subsavepoint_undo`; NOSYNC: sector size is 512 in this build, `pagerWriteLargeSector` never runs (not ported, not verified) |

### F. `syncJournal(newHdr)` (pager.c:4340-4449) and journal headers
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| F1 | always first | `sqlite3PagerExclusiveLock` (RESERVED -> EXCLUSIVE; BUSY returns, leaving PENDING at the OS level) | pager.c:4349 | `sync_journal` -> `lock_db_file(Exclusive)` |
| F2 | `!noSync`, journal file open, not MEMORY | zero a stale following header magic; `fullSync`: sync; write magic + `nRec` at `journalHdr`; sync | pager.c:4352-4428 | same function |
| F3 | then `journalHdr = journalOff`; `newHdr`: `nRec = 0`, `writeJournalHdr` (next sector boundary, magic + nRec ZERO until synced, new random `cksumInit`) | a new journal section after every spill that synced | pager.c:4430-4435, 1500-1580 | `write_journal_hdr` |
| F4 | `noSync` / MEMORY journal | `journalHdr = journalOff` only | pager.c:4436-4438 | same |
| F5 | always last | `sqlite3PcacheClearSyncFlags`; `eState = PAGER_WRITER_DBMOD` | pager.c:4445-4446 | `clear_sync_flags`, `db_file_modified` |

### G. `pager_write` / `sqlite3PagerWrite` / `pager_write_pagelist` / `sqlite3PagerDontWrite`
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| G1 | `PGHDR_WRITEABLE && dbSize >= pgno` | only `subjournalPageIfRequired` | pager.c:6301-6303 | `mark_page_writable` (flag test) |
| G2 | page not in the journal, `pgno <= dbOrigSize` | journal record, `NEED_SYNC` (`o=journal`) | pager.c:6155-6159, 6076 | `append_journal_page` + flag |
| G3 | page not in the journal, `pgno > dbOrigSize` | `NEED_SYNC` only if `eState != DBMOD` (`o=append needsync=`) | pager.c:6160-6167 | same |
| G4 | `pager_write_pagelist`: `pgno <= dbSize && !DONT_WRITE` | write (`o=write`); else skip (`skip-dontwrite` / `skip-beyond`) | pager.c:4525-4558 | `pager_write_pagelist` (`db_size` = `Pager.dbSize`, separate from the b-tree's `nPage`) |
| G5 | `sqlite3PagerDontWrite`: `!tempFile && DIRTY && nSavepoint==0` | `DONT_WRITE` set, `WRITEABLE` cleared (page stays on the dirty list) | pager.c:6345-6355 | `Pager::dont_write` |
| G6 | `getPageNormal` with `PAGER_GET_NOCONTENT` (also for a cached page) | `pgno <= dbOrigSize`: marked in-journal (never journalled); `addToSavepointBitvecs`; page zeroed | pager.c:5630-5680 | `pager_get` / `pager_get_no_content` |
| G7 | `allocateBtreePage` at end of file: `bNoContent = (bDoTruncate==0)` | after an incremental-vacuum step in the transaction the page is fetched with content and journalled | btree.c:6806 | `allocate_raw_page` (`image_truncated`) |

### H. Savepoint playback after spills (`pager_playback_one_page`, pager.c:2380-2560)
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| H1 | main-journal record: `isSynced = noSync || offset <= journalHdr`; sub-journal record: `isSynced = pPg==0 || !NEED_SYNC` | whether the database file may hold a newer version | pager.c:2460-2464 | `apply_subsavepoint_undo` (`tx_journal_end`, `journal_hdr`) |
| H2 | `eState >= DBMOD && isSynced` | image written into the database file (`o=file`) | pager.c:2465-2486 | same |
| H3 | else sub-journal record and page not cached (always in WAL mode) | `sqlite3PagerGet(.., NOCONTENT)` under `SPILLFLAG_ROLLBACK`, `MakeDirty` (`o=load-dirty`) | pager.c:2487-2511 | same |
| H4 | page cached | takes the image, flags unchanged; reference taken and released (dirty-list / LRU position) | pager.c:2512-2545 | same |
| H5 | WAL, `pSavepoint==0` | `pagerRollbackWal`: `sqlite3WalUndo` + `pagerUndoCallback` for every dirty page (unreferenced: dropped; referenced: re-read) | pager.c:3160-3200 | `pager_rollback_wal` |

### I. End of transaction / pragmas
| # | condition | outcome | file:line | Rust |
|---|---|---|---|---|
| I1 | commit, rollback mode | `pager_incr_changecounter`, `writeSuperJournal`, `syncJournal(0)`, `pager_write_pagelist(sorted dirty list)`, `CleanAll`, grow file if `dbSize > dbFileSize`, sync | pager.c:6590-6740 | `commit_phase_one` |
| I2 | `pager_end_transaction`: `bCommit && dbFileSize > dbSize` | file cut back AFTER the journal is finalized | pager.c:2193 | `commit_phase_two` |
| I3 | `PRAGMA cache_size=N` | `sqlite3BtreeSetCacheSize` (A3) — needs the schema | pragma.c:882 | engine `CACHE_SIZE` + `pragma_need_schema` |
| I4 | `PRAGMA cache_spill` / `=N` / `=bool` | query: 0 if off else A6; set: integer -> spill size, boolean -> `SQLITE_CacheSpill`; pagers updated only in autocommit (`setAllPagerFlags`) | pragma.c:916-935, 242 | engine `CACHE_SPILL` |
| I5 | `PRAGMA shrink_memory` | `sqlite3_db_release_memory` -> `sqlite3PagerShrink` | pragma.c:2444 | engine `SHRINK_MEMORY` |
| I6 | in-memory database (`memDb`) | cache not purgeable: nothing recycled, no stress callback | pager.c:5088 | `PageCache::new(.., purgeable = !is_memdb_backing)` |
| I7 | `readDbPage(pgno 1)`: page 1 was not in the cache (cache limit 0: every unpin drops it; `pcache1EnforceMaxPage`; `shrink_memory`) | `Pager.dbFileVers` = bytes 24.. of the page read (WAL snapshot or file), so the next `pager_write_changecounter` stamps that + 1; a page 1 that stayed cached keeps the older `dbFileVers` | pager.c:3103-3119, 3143 | `pager_get_miss` (`db_file_vers_cc`); no trace event — verified by file bytes 24..27 / 92..95 (fz6 seed 286; `PRAGMA journal_mode=wal; PRAGMA cache_size=0;` + 3 x CREATE TABLE: C 4, before 2, now 4) |

### J. Page references (`btree.c`) — the input of B, C, D
C holds a reference on: page 1 for the whole transaction (`pBt->pPage1`); every page of a cursor's stack (`apPage[0..iPage]`);
the siblings and new pages of `balance_nonroot` until `balance_cleanup`; the page returned by `allocateBtreePage` until the
caller releases it; free-list trunks during allocate / free; each overflow page while it is filled or freed; the page being
cleared by `clearDatabasePage`. Rust: `crates/sqliters-btree/src/pins.rs` (`VCursor`, `pin` / `unpin`) and the call sites listed
in the report. Modelled: table / index insert (`sqlite3BtreeInsert`, `sqlite3BtreeLast` + `BTCF_AtLast`, `OP_NotExists` on the
write cursor, `sqlite3BtreeIndexMoveto` bypass cases, `saveAllCursors`), `balance` / `balance_deeper` / `balance_quick` /
`balance_nonroot`, `allocateBtreePage`, `freePage2`, `fillInCell` overflow pages, `clearDatabasePage`, `clearCellOverflow`,
`sqlite3BtreeCount`, full scans. NOT modelled (references differ from C, see section 4): UPDATE / DELETE cursors, index and
rowid lookups of queries, the append cursors of CREATE INDEX / VACUUM / transfer, `sqlite3PagerMovepage` (auto-vacuum),
blob handles and nested statements of FTS5 / R-Tree, `DIRECT_OVERFLOW_READ`.


### K. Page references of statements (step H6b) — the cursor model
The engine evaluates statements with its own machinery (it reads the rows a loop selects before it processes them, looks rows
up by key, walks trees by page number). In H6b those reads no longer touch the cache (`pcache.rs` `Shadow`, `Pager::peek_page_cold`:
a page that is not cached is read from the file / WAL into a side store the cache never sees). The cache is driven only by
references taken where C's cursors take them: `pins.rs` holds one real cursor per C cursor (`VCursor`: `apPage[]`, `aiIdx[]`,
`ix`, `eState` VALID / INVALID / SKIPNEXT / REQUIRESSEEK, `BTCF_AtLast`, `BTCF_ValidNKey`), moved by a port of btree.c.

| # | condition (C) | outcome | file:line | Rust |
|---|---|---|---|---|
| K1 | `moveToRoot`: cursor holds pages | all pages but the root are released, the page the cursor is on first; else the root is fetched | btree.c:5586 | `vc_move_to_root`, `vcr_root` |
| K2 | `moveToChild` / `moveToParent` | child referenced, `aiIdx[iPage] = ix`; parent: page released, `ix = aiIdx[iPage-1]` | btree.c:5486, 5545 | `vcr_to_child`, `vcr_to_parent` |
| K3 | `sqlite3BtreeNext`: `eState != VALID` | `restoreCursorPosition` (seek of the saved key), `CURSOR_SKIPNEXT` with `skipNext > 0` returns without moving | btree.c:6362-6384, 906 | `vc_next`, `vcr_restore` |
| K4 | `btreeNext`: `++ix >= nCell` on a leaf | up while `ix >= nCell` (`iPage == 0` → `CURSOR_INVALID`, the root stays referenced); table: down the next child; index: stop on the divider | btree.c:6385-6420 | `vc_next` |
| K5 | `sqlite3BtreeTableMoveto`: VALID and `BTCF_ValidNKey` | `nKey == key` → no move; `nKey < key` and AtLast → -1; `nKey+1 == key` → `sqlite3BtreeNext` first | btree.c:5837-5870 | `vc_table_moveto` (remembered cell is verified first) |
| K6 | `sqlite3BtreeTableMoveto`: binary search | `idx = upr>>(1-biasRight)`, halving; leaf: `ix = idx`, `res = c`; interior: child `lwr` | btree.c:5880-5960 | `vc_table_moveto` |
| K7 | `sqlite3BtreeIndexMoveto`: VALID, on the last page | last cell `<=` key → no move; first cell `<=` key → search starts on the page; else `moveToRoot` | btree.c:6068-6110 | `index_moveto_start`, `vc_index_moveto` |
| K8 | `sqlite3BtreeDelete` with `BTREE_SAVEPOSITION` | `bPreserve = 2` unless the leaf will be rebalanced / is interior / has one cell (then the key is saved); after: `CURSOR_SKIPNEXT` (`skipNext` 1, or -1 on the last cell) or `moveToRoot` + release all + REQUIRESSEEK | btree.c:9873-10040 | `delete_table_at`, `delete_index_impl` + `idx_after_delete` |
| K9 | `sqlite3BtreeDelete` without it | `moveToRoot` after the delete (and after `balance`) | btree.c:10030 | same |
| K10 | `sqlite3BtreeInsert` table: `BTCF_ValidNKey && nKey == pX->nKey` | no seek (overwrite / replace through the cursor); with `BTREE_SAVEPOSITION` after a balance: release all + REQUIRESSEEK | btree.c:9441-9520, 9690 | `insert_table_c` (`nkey`, `vc_resume`) |
| K11 | `saveAllCursors(pBt, iRoot, pExcept)` | every other cursor on the tree gives its pages back | btree.c:816 | `vc_save_others` (H6) |
| K12 | DELETE / UPDATE one pass (`eOnePass != ONEPASS_OFF`) | the loop cursor is the write cursor: `OP_Rewind` / seek, one `OP_Next` per row visited, the row is changed in between; index loop: the index cursor steps, its entry is deleted with `OP_Delete` on it (`iIdxNoSeek`) | delete.c:300-560, 745-870; update.c:296-1100 | `cscan.rs` `CScan` (`Kind::Table`, `Index`, `RowidList`), `delete_index_at_cursor` |
| K13 | DELETE / UPDATE two passes | pass 1 through read cursors to the end; `sqlite3OpenTableAndIndices(OP_OpenWrite)` reuses the cursor numbers (read cursors closed); `OP_NotExists` per row | delete.c:520-580; update.c:690-760 | `CScan::begin`, `c_write_cursor_to_row` |
| K14 | `OPFLAG_SAVEPOSITION` | DELETE: on the delete through the loop cursor (table, or index `iIdxNoSeek`); UPDATE: on the table `OP_Insert` | delete.c:855-865; update.c:1020 | `cc_save_position(root)` |
| K15 | `OP_NoConflict` (UNIQUE index; INSERT: every one, UPDATE: `aRegIdx[i] != 0`) + `OP_IdxInsert` `OPFLAG_USESEEKRESULT` | the write cursor of the index is placed by the check; the insert does not search again | vdbe.c:5499; insert.c `sqlite3GenerateConstraintChecks` / `sqlite3CompleteInsertion` | `seek_index_no_conflict`, `idx_seek_hint` (stack and child slots validated) |
| K16 | `sqlite3VdbeHalt` → `closeAllCursors` | cursors closed by cursor number: tables of the statement, then index cursors in `pTab->pIndex` order (WITHOUT ROWID: PRIMARY KEY last) | vdbeaux.c:2853 | `vc_close_all` / `vc_close_from` (`is_index`, `idx_order`, `wr_roots`), `c_statement_halt`, `finish_activity` |
| K17 | `OP_Program` / `sqlite3VdbeFrameRestore` | a trigger / FK-action program has its own cursors, closed when it returns | vdbeaux.c:2827 | `c_program_frame` → `vc_frame_push` / `vc_frame_pop` |
| K18 | `clearCellOverflow` | every overflow page but the last is fetched (`getOverflowPage`) and freed while referenced; the last one only if cached (`btreePageLookup`) | btree.c:7011-7060 | `free_overflow_chain(_n)`, `free_last_overflow_page` |
| K19 | `btreeDropTable` (no auto-vacuum) | `btreeGetPage(root)`, `freePage`, `releasePage` | btree.c:10374-10400 | `drop_tree_relocating` |
| K20 | `sqlite3RefillIndex` | table read cursor to the end (keys to the sorter), then `OP_SeekEnd` + append through the index write cursor; `OP_Close iTab`, `OP_Close iIdx` | build.c:3852-3950 | `AppendCursor` (`append_seek_end` on the write cursor), `cc_close_reads`, `cc_close` |
| K21 | `xferOptimization` (INSERT..SELECT * / VACUUM) | source cursor `OP_Rewind`..`OP_Next`, destination appended; `OP_Close` of both after the table and after each index | insert.c:3397, 3457 | `xfer_copy_vacuum` (`cc_close`), registry cursors marked `c_cursor` |
| K22 | VACUUM copy back (`sqlite3BtreeCopyFile` → `backupOnePage`) | per page: `sqlite3PagerGet(dest)`, `sqlite3PagerWrite`, copy, unref; `sqlite3PagerTruncateImage` (pages past the new end are not journalled); `sqlite3BtreeSetSpillSize(pTemp, sqlite3BtreeSetSpillSize(pMain,0))` | backup.c:237-300; vacuum.c:261 | `Pager::copy_pages_from`, `build_vacuum_image` |
| K23 | `fkLookupParent` (INSERT into a child, parent key = rowid) | `OP_OpenRead` parent, `OP_NotExists`, `OP_Close`, between the constraint checks and the writes | fkey.c:320-460 | `c_fk_parent_lookups` |
| K24 | `OP_Once` around the sub-select of `x IN (SELECT ...)` | the sub-select runs once per run of the statement | vdbe.c:2778 | `stmt_tx.once_in_subqueries` (top-level statement, self-contained sub-select) |
| K25 | `attachFunc`: `sqlite3BtreeSetPagerFlags(.., db->flags & PAGER_FLAGS_MASK)` | an attached database takes the connection's `cache_spill` flag | attach.c:244 | `DatabaseSet::attach_slot` |
| K26 | reads of the engine that C does not make at that moment | no cache event | — | `Shadow`, `cc_silent` (DML statements), registry lease cursors |
| K27 | loop of a DELETE / UPDATE that visits no row: `OP_Rewind` on an empty table | the root page is fetched and held until the statement halts | vdbe.c OP_Rewind; btree.c:5586 | `cscan.rs` `Start::Rewind` (also when `n_scanned == 0`) |
| K28 | rowid range: `OP_SeekGE` / `OP_SeekGT` with the lower bound; "if( res<0 \|\| (res==0 && oc==OP_SeekGT) ) sqlite3BtreeNext" | the search for the bound itself (a bound that is not a row, an empty range), then at most one step | vdbe.c OP_SeekGE | `Start::Seek`, `Btree::cc_seek_rowid_ge`, `stmt_tx.c_rowid_lo` |
| K29 | `OP_SeekGE` on a cursor with `BTREE_SEEK_EQ` (`OPFLAG_SEEKEQ`: every constraint is `==`) and no equal entry: "if( eqOnly && r.eqSeen==0 ) goto seek_not_found" | no `sqlite3BtreeNext` after the search | vdbe.c OP_SeekGE; where.c:6530 | `IdxScan.eq_only`, `cc_idx_finish` |
| K30 | `rowid = X` DELETE / UPDATE and no such row | the one `OP_NotExists` on the write cursor | delete.c / update.c ONEPASS_SINGLE | `ScanInfo.rowid_seek`, `stmt_tx.c_rowid_seek`, `CScan::new` |
| K31 | insert.c `case OE_Replace` of a UNIQUE index: `sqlite3GenerateRowDelete(.., ONEPASS_OFF, iThisCur)` | `OP_NotExists` on the table cursor for the row in the way, then its index entries, the table row, the entry under the index cursor | insert.c:2330-2390; delete.c:740 | `delete_row…` with `MutationOrigin::ReplaceConflictDelete` → `c_write_cursor_to_row` |
| K32 | update.c: "if the PK of the row is changing, then the GenerateConstraintChecks() above may have moved cursor iDataCur. Reseek it." | `OP_NotExists` / `OP_NotFound` with the old key before the index entries are deleted | update.c:1050-1057 | `validate_and_write_row_inner` (`key_changes`) |
| K33 | `sqlite3VdbeHalt` → `closeAllCursors` of a statement that fails, and of a `rowid = ?` SELECT that finds nothing | every cursor releases its pages | vdbeaux.c:2843 | `abort_write_statement`, rowid point SELECT `Miss` |
| K34 | one-row / multi-row VALUES INSERT with indexes: all constraint checks (`OP_NotExists`, `OP_NoConflict` per UNIQUE index), then `OP_IdxInsert` per index, then `OP_Insert` | (program order) | insert.c `sqlite3GenerateConstraintChecks`, `sqlite3CompleteInsertion` | the one-row plan (`execute_single_row_insert_plan`: rowid test, `seek_index_no_conflict` per UNIQUE index in `pTab->pIndex` order, then the index entries, then the row); the compiled insert program only for tables without indexes; multi-row VALUES with indexes: the general executor |
| K35 | `UPDATE .. WHERE rowid = ?` with expressions / a rowid change, `DELETE .. WHERE rowid = ?` that is not a point plan | (program order) | update.c, delete.c | the point plan, or the compiled program with `OP_SeekRowid` moving C's write cursor (`cc_seek_rowid`); an UPDATE that rewrites a UNIQUE index or changes the rowid: the general executor (C tests `OP_NoConflict` before it deletes the old entries) |
| K36 | `OP_OpenRead` cursors of a SELECT program; `OP_SeekGE` on an index whose first entry >= key is a divider on an interior page | the search goes down to the leaf below the divider's left child and steps back up | btree.c:6050-6200, vdbe.c OP_SeekGE | interpreter cursors `cursor_mark_c`; `rc_sync` (`vc_prev` + `vc_next`) |
| K37 | `WHERE_ONEROW` (every column of a UNIQUE index `==`) in a SELECT | no `OP_Next`: the cursor stays on the row | where.c `whereShortCut` / `WHERE_ONEROW` | compiled SELECT: no `IndexMatchNext` |
| K38 | WAL: `pagerRollbackWal` → `pagerUndoCallback` → `readDbPage(page 1)`: "if( pgno==1 ) memcpy(&pPager->dbFileVers, ..)" | the change counter the next commit stamps counts on from the last committed page 1 | pager.c:3100-3160, 2990 | `Pager::pager_rollback_wal` (`db_file_vers_cc`) |
| K39 | `sqlite3BtreeIndexMoveto`: a probed cell whose record continues on overflow pages → `accessPayload(pCur, 0, nKey, buf, 0)`; `sqlite3PagerDirectReadOk` (no dirty page in the cache, no WAL frame of the page, file-backed) | each overflow page of the cell is fetched through the cache and released (`sqlite3PagerGet(.., PAGER_GET_READONLY)`, `sqlite3PagerUnref`), or read from the file past the cache; `indexCellCompare`: "If the record extends into overflow pages, do not attempt the optimization" (neither shortcut); shortcut 1 compares no other cell | btree.c:6093-6127, 6192-6230, 5293-5330; pager.c:805 | `Btree::c_overflow_refs`, `Pager::direct_overflow_read_ok`, `index_page_search_full(.., refs)`, `index_cell_overflows`, `c_idx_search_quiet`; the general index delete descent (`Step::Parsed`) |
| K40 | `OP_NotExists` of a statement belongs to that statement | a statement that fails after its seek does not spare the next statement the seek of the same rowid | vdbe.c OP_NotExists | `Btree::new_rowid_hint` cleared in `vc_close_all` |

**Not modelled** (the engine's reads there are silent, i.e. the references C takes are missing): `fkScanChildren` and the
order of FK actions on the parent side; sub-selects evaluated per row inside DML; the loop of a DELETE / UPDATE on a WITHOUT
ROWID table when C picks a covering secondary index; `sqlite3PagerMovepage` / pointer-map pages of auto-vacuum (`relocatePage`
unpins at once, H6); the nested statements and the blob reader of FTS5 / R-Tree; joins (the engine's join order / row order
differs from C's in the tested case, a difference that exists before this step); reads of payload that continues on overflow
pages other than the key comparison of a seek (`OP_Column` of a spilled column, `OP_IdxGT` / `OP_IdxRowid` on an index entry
with overflow pages: C goes through `accessPayload` there too); SELECT
shapes run by the general executor instead of a cursor program — ORDER BY .. DESC with LIMIT (reverse scans that stop early),
an index range with LIMIT, IN on an index, `min()` / `max()` through an index, EXISTS and scalar sub-selects, joins (family
`psx`); `PRAGMA integrity_check` (page walk order); a rowid-range bound that is not an integer or a simple fraction.

**What this means structurally.** Statements this engine runs as a cursor program (the one-row plans, the compiled SELECT
programs of the interpreter, the ported write paths) hold page references where C's `BtCursor.apPage[]` does, by
construction. Statements the general executor evaluates set-at-a-time (it reads the rows of a loop first, then processes
them) are replayed: the loop of C's program is described (`CScan`, `IdxScan`, `ScanSync`) and moved next to the engine's own
evaluation. Every statement shape needs its own description, and each new shape examined in this step (empty loops, range
bounds, one-row lookups, REPLACE, rowid changes, failing statements) needed one more rule. The families still open are the
ones for which no description exists yet. The alternative that ends this is to run those statements as cursor programs too
(joins, sub-selects, ordered / limited scans, the nested statements of virtual tables), so that the references follow from
the execution instead of from a replay of it.

## 2. Decision-trace differential

Instrumented C: `h6/ctrace` (unchanged in H6b) (class `pc` in pcache.c / pcache1.c / pager.c, class `pcx` = every page reference). Rust: the same events through `pctr!` (`--cfg sqliters_dtrace`). Scenario set `h6/sc6` (`h6/gen_pc.py`), runner `h6/trall.py`, result directory `tr_f5`.

Two comparisons per script: **strict** = every `pc` event and every first-reference / last-release of a page other than page 1, in order; **decision stream** = the events that are decisions with observable effect (`miss o=refuse|recycle`, `stress` incl. victim / `pSynced` / dirty-list hash, `pstress`, `wr` with page hash, `walfr`, `syncj`, `jhdr`, `pb`, `subj`, `dontwrite`, `drop`, `purge`, `move`), in order.

- scripts: 145; events compared: C 5138570, Rust 4577191; decision events: C 720211, Rust 625151
- strict trace equal: **82**; decision stream equal: **127**; database file byte-identical: 140; stdout/stderr equal: 145

| family | scripts | strict equal | decisions equal | db identical |
|---|---|---|---|---|
| av | 5 | 0 | 0 | 5 |
| clr | 15 | 10 | 15 | 15 |
| ddl | 5 | 0 | 5 | 5 |
| del | 15 | 10 | 15 | 15 |
| f5 | 5 | 0 | 1 | 4 |
| ins_sel | 15 | 10 | 15 | 15 |
| ins_selidx | 15 | 10 | 15 | 15 |
| ins_stmts | 15 | 10 | 15 | 15 |
| ovf | 5 | 4 | 5 | 5 |
| prag | 5 | 4 | 5 | 5 |
| rt | 5 | 0 | 1 | 5 |
| sel | 15 | 10 | 15 | 15 |
| sp | 5 | 4 | 5 | 5 |
| upd | 15 | 10 | 15 | 15 |
| wr | 5 | 0 | 0 | 1 |

Families: `ins_sel` INSERT..SELECT into a rowid table; `ins_selidx` with two indexes; `ins_stmts` one INSERT statement per row + index; `clr` insert, `DELETE FROM t` (clear), insert again; `upd` / `del` / `sel` UPDATE / DELETE WHERE / SELECT on a filled table; `ovf` overflow rows; `wr` WITHOUT ROWID; `sp` savepoints + ROLLBACK TO + failing statement; `ddl` CREATE INDEX on a big table, DROP, VACUUM; `av` auto_vacuum; `f5` FTS5; `rt` R-Tree; `prag` cache pragmas inside transactions. Each in 15 configurations (page size 512..65536 x cache_size 0, 1, 3..30, default x journal_mode delete / wal / persist / truncate / memory) or 5.

### Second scenario set (step H6b): `h6b/sc7` (`h6b/gen_pc2.py`), result directory `tr7_f5`

Five more configurations of `ovf`, `sp`, `av`, `ddl`, `prag`, `rt`, `f5`, `wr`, and the new families `trig` (row triggers writing a log table), `fk` (child inserts, parent DELETE / UPDATE with CASCADE / SET NULL), `ups` (UPSERT, OR REPLACE, OR IGNORE), `join` (INSERT..SELECT from a join, sub-selects, joins inside a transaction), `ixs` (UPDATE / DELETE whose loop runs on an index, a rowid range, an IN list), `reidx` (REINDEX), `alt` (ALTER TABLE).

- scripts: 75; strict trace equal: **32**; decision stream equal: **46**; database file byte-identical: 65; stdout/stderr equal: 75

| family | scripts | strict equal | decisions equal | db identical |
|---|---|---|---|---|
| alt | 5 | 4 | 5 | 5 |
| av | 5 | 0 | 0 | 5 |
| ddl | 5 | 0 | 5 | 5 |
| f5 | 5 | 0 | 0 | 5 |
| fk | 5 | 0 | 1 | 5 |
| ixs | 5 | 4 | 5 | 5 |
| join | 5 | 0 | 0 | 0 |
| ovf | 5 | 4 | 5 | 5 |
| prag | 5 | 4 | 5 | 5 |
| reidx | 5 | 4 | 5 | 5 |
| rt | 5 | 0 | 0 | 5 |
| sp | 5 | 4 | 5 | 5 |
| trig | 5 | 4 | 5 | 5 |
| ups | 5 | 4 | 5 | 5 |
| wr | 5 | 0 | 0 | 0 |

### Third scenario set (step H6b): `h6b/sc8` (`h6b/gen_pc3.py`), result directory `tr8_f5`

The one-statement shapes applications run most, five configurations each: `uq` one-row and multi-row VALUES INSERTs into a table with a UNIQUE and a plain index (explicit / automatic rowid; conflicts under ABORT, OR IGNORE, OR REPLACE; inside a transaction and in autocommit mode); `pt` UPDATE / DELETE of one row by rowid (literal SET, SET with an expression, a growing value, a rowid change, a column of a UNIQUE index, a row that does not exist); `ps` SELECTs between the writes of a transaction: rowid lookup (hit / miss), equality on a secondary index (covering and not), UNIQUE lookup, rowid range, rowid IN list, full scans; `psx` the SELECT shapes that are **not** equal yet (ORDER BY .. DESC LIMIT, index range with LIMIT, IN on an index, min() / max() through an index, EXISTS / scalar sub-selects, a join, PRAGMA integrity_check). `psx` also prints rows in another order than C for `ORDER BY c DESC` (ties; a difference that exists before this step).

- scripts: 20; strict trace equal: **6**; decision stream equal: **15**; database file byte-identical: 20; stdout/stderr equal: 15

| family | scripts | strict equal | decisions equal | db identical |
|---|---|---|---|---|
| ps | 5 | 3 | 5 | 5 |
| psx | 5 | 0 | 0 | 5 |
| pt | 5 | 0 | 5 | 5 |
| uq | 5 | 3 | 5 | 5 |

## 3. Coverage (how often each outcome fired; all scripts | scripts whose decision stream is equal)

| decision outcome | C (all) | Rust (all) | C (equal scripts) | Rust (equal scripts) |
|---|---|---|---|---|
| `pcx|dropref` | 5 | 0 | 0 | 0 |
| `pcx|get` | 2130481 | 1911871 | 1745955 | 1685657 |
| `pcx|rel` | 2130476 | 1911871 | 1745955 | 1685657 |
| `pc|cachesize|-` | 509 | 509 | 449 | 449 |
| `pc|dontwrite|set` | 1009 | 1011 | 986 | 986 |
| `pc|dontwrite|skip` | 3312 | 3310 | 2635 | 2635 |
| `pc|drop|reuse=0` | 74133 | 50413 | 41702 | 41702 |
| `pc|drop|reuse=1` | 5 | 0 | 0 | 0 |
| `pc|flags|spilloff=0` | 193 | 193 | 171 | 171 |
| `pc|flags|spilloff=1` | 5 | 5 | 5 | 5 |
| `pc|jhdr|-` | 909 | 910 | 778 | 778 |
| `pc|lock|to=2 rc=0` | 348 | 348 | 301 | 301 |
| `pc|lock|to=4 rc=0` | 348 | 348 | 301 | 301 |
| `pc|miss|alloc` | 84555 | 60747 | 50983 | 50905 |
| `pc|miss|recycle` | 62573 | 59761 | 46004 | 46004 |
| `pc|miss|refuse` | 114704 | 91239 | 77632 | 77632 |
| `pc|move|-` | 47168 | 47163 | 37248 | 37248 |
| `pc|nmax|-` | 509 | 509 | 449 | 449 |
| `pc|pb|file main=0 synced=1` | 1096 | 1096 | 1096 | 1096 |
| `pc|pb|load-dirty main=0 synced=1` | 157 | 157 | 157 | 157 |
| `pc|pstress|nospill` | 392 | 392 | 392 | 392 |
| `pc|pstress|sync+write` | 580 | 581 | 496 | 496 |
| `pc|pstress|wal` | 25917 | 25923 | 24862 | 24862 |
| `pc|pstress|write` | 67347 | 60138 | 51236 | 51236 |
| `pc|purge|-` | 10 | 10 | 10 | 10 |
| `pc|pw|append` | 65466 | 59996 | 50514 | 50514 |
| `pc|pw|journal` | 4580 | 4580 | 3907 | 3907 |
| `pc|shrink|-` | 5 | 5 | 5 | 5 |
| `pc|spillsize|-` | 25 | 25 | 25 | 25 |
| `pc|stressed|rc=0` | 94236 | 87034 | 76986 | 76986 |
| `pc|stress|below-spill` | 1685 | 41 | 35 | 35 |
| `pc|stress|no-victim` | 18783 | 4164 | 611 | 611 |
| `pc|stress|victim` | 94236 | 87034 | 76986 | 76986 |
| `pc|subj|-` | 6285 | 6286 | 5428 | 5428 |
| `pc|syncj|newhdr=0 lockrc=0 nosync=0` | 348 | 348 | 301 | 301 |
| `pc|syncj|newhdr=1 lockrc=0 nosync=0` | 580 | 581 | 496 | 496 |
| `pc|trunc|-` | 854 | 1033 | 757 | 924 |
| `pc|walfr|commit=0` | 25917 | 25923 | 24862 | 24862 |
| `pc|walfr|commit=1` | 146 | 146 | 134 | 134 |
| `pc|wr|skip-beyond` | 364 | 367 | 358 | 358 |
| `pc|wr|skip-dontwrite` | 247 | 249 | 226 | 226 |
| `pc|wr|write` | 78072 | 70874 | 60473 | 60473 |

**Not verified** (the outcome never fired in a script whose decision stream is equal — listed, not passed):

- `pc|drop|reuse=1`
- B5 memory pressure, E6 `SPILLFLAG_NOSYNC` / `pagerWriteLargeSector`, temp-file and in-memory caches (not traced), `sqlite3PagerFlush` (`sqlite3_db_cacheflush`)

## 4. First divergence of every script that is not equal

```
av_p4096_c10_delete.sql @25
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p4096_c10_delete.sql decisions @16/719
   C: TR|pc|stress|c=1 want=11 npage=9 szspill=1 synced=0 victim=5 vfl=e dl=681e6f43/9
   R: TR|pc|stress|c=1 want=11 npage=9 szspill=1 synced=0 victim=2 vfl=e dl=ec16d8a3/9
av_p4096_c12_truncate.sql @25
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p4096_c12_truncate.sql decisions @15/792
   C: TR|pc|stress|c=1 want=12 npage=11 szspill=1 synced=0 victim=5 vfl=e dl=7947264e/10
   R: TR|pc|stress|c=1 want=12 npage=11 szspill=1 synced=0 victim=2 vfl=e dl=781342fa/10
av_p4096_c1_delete.sql @36
   C: TR|pc|miss|c=1 pg=2 cf=1 npage=2 nrec=0 nmax=1 o=refuse
   R: TR|pcx|rel|pg=3
av_p4096_c1_delete.sql decisions @15/929
   C: TR|pc|stress|c=1 want=2 npage=2 szspill=1 synced=0 victim=0 vfl=0 dl=b5a66e9b/2
   R: TR|pc|stress|c=1 want=2 npage=2 szspill=1 synced=3 victim=3 vfl=6 dl=c0c4acfc/2
av_p4096_cd_delete.sql @23
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p4096_cd_delete.sql decisions @17/3430
   C: TR|pc|stress|c=1 want=435 npage=434 szspill=1 synced=0 victim=5 vfl=e dl=cf5eb7a1/434
   R: TR|pc|stress|c=1 want=435 npage=434 szspill=1 synced=0 victim=2 vfl=e dl=7aea5a21/434
av_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
av_p512_c30_wal.sql decisions @6/1401
   C: TR|pc|stress|c=2 want=30 npage=28 szspill=1 synced=6 victim=6 vfl=6 dl=017d8e51/27
   R: TR|pc|stress|c=2 want=30 npage=28 szspill=1 synced=2 victim=2 vfl=6 dl=87ae04c9/27
clr_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
clr_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
clr_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
clr_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
clr_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ddl_p4096_c10_delete.sql @9224
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=10
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p4096_c12_truncate.sql @10492
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=12
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p4096_c1_delete.sql @4368
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=1
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p4096_cd_delete.sql @101715
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=483
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
del_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
del_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
f5_p4096_c10_delete.sql @22
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c10_delete.sql decisions @14/2467
   C: TR|pc|miss|c=1 pg=10 cf=1 npage=9 nrec=3 nmax=10 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=10 cf=1 npage=9 nrec=2 nmax=10 o=recycle rp=6 lru=3979155c/2
f5_p4096_c12_truncate.sql @22
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c12_truncate.sql decisions @14/1599
   C: TR|pc|miss|c=1 pg=12 cf=1 npage=11 nrec=3 nmax=12 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=12 cf=1 npage=11 nrec=2 nmax=12 o=recycle rp=6 lru=3979155c/2
f5_p4096_c1_delete.sql @41
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c1_delete.sql decisions @21/33673
   C: TR|pc|stress|c=1 want=4 npage=2 szspill=1 synced=2 victim=2 vfl=6 dl=c6ebfa61/2
   R: TR|pc|stress|c=1 want=4 npage=2 szspill=1 synced=0 victim=0 vfl=0 dl=d20a38c2/2
f5_p4096_cd_delete.sql @20
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
f5_p512_c30_wal.sql decisions @11/2590
   C: TR|pc|miss|c=2 pg=31 cf=1 npage=29 nrec=5 nmax=30 o=recycle rp=8 lru=05251535/5
   R: TR|pc|miss|c=2 pg=31 cf=1 npage=29 nrec=4 nmax=30 o=recycle rp=8 lru=3a9c1e4e/4
ins_sel_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_sel_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_sel_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_sel_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_sel_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_selidx_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_selidx_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_stmts_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_stmts_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ovf_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
prag_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
rt_p4096_c10_delete.sql @37
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p4096_c10_delete.sql decisions @11/10999
   C: TR|pc|stress|c=1 want=10 npage=9 szspill=1 synced=0 victim=6 vfl=e dl=9f435eb9/9
   R: TR|pc|stress|c=1 want=10 npage=9 szspill=1 synced=0 victim=6 vfl=e dl=07f83445/9
rt_p4096_c12_truncate.sql @37
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p4096_c12_truncate.sql decisions @11/8024
   C: TR|pc|stress|c=1 want=11 npage=10 szspill=1 synced=0 victim=6 vfl=e dl=2253ceba/10
   R: TR|pc|stress|c=1 want=11 npage=10 szspill=1 synced=0 victim=6 vfl=e dl=4b72ef41/10
rt_p4096_c1_delete.sql @61
   C: TR|pcx|rel|pg=2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=2 nrec=0 nmax=1 o=alloc
rt_p4096_c1_delete.sql decisions @30/90968
   C: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=1
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=3 nmax=1
rt_p4096_cd_delete.sql @35
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
rt_p512_c30_wal.sql decisions @9/2309
   C: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=7 victim=7 vfl=6 dl=d5f89540/27
   R: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=7 victim=7 vfl=6 dl=8de63d4b/27
sel_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sel_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sel_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
sel_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
sel_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sp_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
upd_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
upd_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
wr_p4096_c10_delete.sql @6998
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p4096_c10_delete.sql decisions @1727/5570
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=9 nrec=0 nmax=10 o=refuse
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=9 nrec=0 nmax=10 o=refuse
wr_p4096_c12_truncate.sql @7932
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p4096_c12_truncate.sql decisions @1924/6227
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=11 nrec=0 nmax=12 o=refuse
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=11 nrec=0 nmax=12 o=refuse
wr_p4096_c1_delete.sql @2635
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p4096_c1_delete.sql decisions @559/4727
   C: TR|pc|miss|c=1 pg=5 cf=1 npage=8 nrec=0 nmax=1 o=refuse
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=8 nrec=0 nmax=1 o=refuse
wr_p4096_cd_delete.sql @103440
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p4096_cd_delete.sql decisions @9201/9661
   C: TR|pc|move|c=1 from=430 to=262145 fl=e
   R: TR|pc|dontwrite|c=1 pg=143 fl=e nsp=0 o=set
wr_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
wr_p512_c30_wal.sql decisions @864/1996
   C: TR|pc|miss|c=2 pg=7 cf=1 npage=29 nrec=2 nmax=30 o=refuse
   R: TR|pc|miss|c=2 pg=5 cf=1 npage=29 nrec=2 nmax=30 o=refuse
```

Second set (`sc7`):

```
alt_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
av_p1024_c7_delete.sql @26
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p1024_c7_delete.sql decisions @16/3169
   C: TR|pc|stress|c=1 want=8 npage=6 szspill=1 synced=0 victim=5 vfl=e dl=75eb9d86/6
   R: TR|pc|stress|c=1 want=8 npage=6 szspill=1 synced=0 victim=2 vfl=e dl=4c7c8132/6
av_p16384_c5_delete.sql @26
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p16384_c5_delete.sql decisions @14/396
   C: TR|pc|miss|c=1 pg=5 cf=1 npage=4 nrec=2 nmax=5 o=recycle rp=4 lru=597b8653/2
   R: TR|pc|miss|c=1 pg=5 cf=1 npage=4 nrec=2 nmax=5 o=recycle rp=2 lru=776cab03/2
av_p2048_c4_truncate.sql @26
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p2048_c4_truncate.sql decisions @14/3445
   C: TR|pc|miss|c=1 pg=3 cf=2 npage=3 nrec=2 nmax=4 o=recycle rp=4 lru=597b8653/2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=3 nrec=2 nmax=4 o=recycle rp=2 lru=776cab03/2
av_p4096_c25_persist.sql @25
   C: TR|pcx|get|pg=2
   R: TR|pcx|rel|pg=3
av_p4096_c25_persist.sql decisions @15/585
   C: TR|pc|stress|c=1 want=24 npage=23 szspill=1 synced=0 victim=5 vfl=e dl=143e5cc6/22
   R: TR|pc|stress|c=1 want=24 npage=23 szspill=1 synced=0 victim=2 vfl=e dl=3088f972/22
av_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
av_p8192_c9_wal.sql decisions @7/344
   C: TR|pc|stress|c=2 want=11 npage=8 szspill=1 synced=6 victim=6 vfl=6 dl=f3de22d3/8
   R: TR|pc|stress|c=2 want=11 npage=8 szspill=1 synced=2 victim=2 vfl=6 dl=fae3f4eb/8
ddl_p1024_c7_delete.sql @23749
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=7
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p16384_c5_delete.sql @5900
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=5
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p2048_c4_truncate.sql @19286
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=4
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p4096_c25_persist.sql @8921
   C: TR|pc|spillsize|c=1 arg=0 sz=1 res=25
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
ddl_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
f5_p1024_c7_delete.sql @23
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p1024_c7_delete.sql decisions @14/8769
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=6 nrec=3 nmax=7 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=7 cf=1 npage=6 nrec=2 nmax=7 o=recycle rp=6 lru=3979155c/2
f5_p16384_c5_delete.sql @23
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p16384_c5_delete.sql decisions @6/5384
   C: TR|pc|stress|c=1 want=5 npage=4 szspill=1 synced=0 victim=3 vfl=e dl=ec01e0d2/4
   R: TR|pc|stress|c=1 want=5 npage=4 szspill=1 synced=0 victim=3 vfl=e dl=4f570895/4
f5_p2048_c4_truncate.sql @23
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p2048_c4_truncate.sql decisions @5/65756
   C: TR|pc|stress|c=1 want=4 npage=3 szspill=1 synced=0 victim=3 vfl=e dl=fad35540/3
   R: TR|pc|stress|c=1 want=4 npage=3 szspill=1 synced=0 victim=3 vfl=e dl=28ee3eeb/3
f5_p4096_c25_persist.sql @22
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c25_persist.sql decisions @14/1276
   C: TR|pc|miss|c=1 pg=25 cf=1 npage=24 nrec=3 nmax=25 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=25 cf=1 npage=24 nrec=2 nmax=25 o=refuse
f5_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
f5_p8192_c9_wal.sql decisions @9/1623
   C: TR|pc|miss|c=2 pg=10 cf=1 npage=8 nrec=3 nmax=9 o=recycle rp=7 lru=24a1995f/3
   R: TR|pc|miss|c=2 pg=10 cf=1 npage=8 nrec=2 nmax=9 o=recycle rp=7 lru=7a6cafbc/2
fk_p1024_c6_delete.sql @4
   C: TR|pc|flags|c=1 spilloff=0
   R: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
fk_p1024_c6_delete.sql decisions @1086/2786
   C: TR|pc|miss|c=1 pg=4 cf=1 npage=6 nrec=1 nmax=6 o=refuse
   R: TR|pc|miss|c=1 pg=32 cf=1 npage=6 nrec=1 nmax=6 o=refuse
fk_p4096_c10_delete.sql @3
   C: TR|pc|flags|c=1 spilloff=0
   R: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
fk_p4096_c10_delete.sql decisions @487/6713
   C: TR|pc|miss|c=1 pg=22 cf=1 npage=9 nrec=0 nmax=10 o=refuse
   R: TR|pc|miss|c=1 pg=15 cf=1 npage=9 nrec=0 nmax=10 o=refuse
fk_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
fk_p4096_c20_wal.sql decisions @552/2765
   C: TR|pc|miss|c=2 pg=5 cf=1 npage=19 nrec=1 nmax=20 o=refuse
   R: TR|pc|miss|c=2 pg=59 cf=1 npage=19 nrec=1 nmax=20 o=refuse
fk_p4096_cd_delete.sql @3
   C: TR|pc|flags|c=1 spilloff=0
   R: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
fk_p8192_c8_truncate.sql @4
   C: TR|pc|flags|c=1 spilloff=0
   R: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
fk_p8192_c8_truncate.sql decisions @545/13913
   C: TR|pc|miss|c=1 pg=4 cf=1 npage=7 nrec=0 nmax=8 o=refuse
   R: TR|pc|miss|c=1 pg=38 cf=1 npage=7 nrec=0 nmax=8 o=refuse
ixs_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
join_p1024_c6_delete.sql @1218
   C: TR|pcx|get|pg=3
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=6 nrec=5 nmax=6 o=recycle rp=28 lru=27ad8771/5
join_p1024_c6_delete.sql decisions @212/1218
   C: TR|pc|miss|c=1 pg=21 cf=2 npage=6 nrec=4 nmax=6 o=recycle rp=28 lru=05788ff6/4
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=6 nrec=5 nmax=6 o=recycle rp=28 lru=27ad8771/5
join_p4096_c10_delete.sql @5469
   C: TR|pcx|get|pg=3
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=9 nrec=8 nmax=10 o=recycle rp=31 lru=b291e773/8
join_p4096_c10_delete.sql decisions @203/954
   C: TR|pc|miss|c=1 pg=25 cf=2 npage=9 nrec=7 nmax=10 o=recycle rp=31 lru=352aeda8/7
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=9 nrec=8 nmax=10 o=recycle rp=31 lru=b291e773/8
join_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
join_p4096_c20_wal.sql decisions @208/1251
   C: TR|pc|miss|c=2 pg=37 cf=2 npage=19 nrec=17 nmax=20 o=recycle rp=55 lru=624cfc2c/17
   R: TR|pc|miss|c=2 pg=3 cf=2 npage=19 nrec=18 nmax=20 o=recycle rp=55 lru=6538f1c0/18
join_p4096_cd_delete.sql @54803
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
join_p4096_cd_delete.sql decisions @570/905
   C: TR|pc|wr|c=1 pg=5 o=write h=16a2bb22
   R: TR|pc|wr|c=1 pg=5 o=write h=5de18671
join_p8192_c8_truncate.sql @9062
   C: TR|pcx|get|pg=3
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=7 nrec=6 nmax=8 o=recycle rp=29 lru=9d28e9be/6
join_p8192_c8_truncate.sql decisions @233/1129
   C: TR|pc|miss|c=1 pg=23 cf=2 npage=7 nrec=5 nmax=8 o=recycle rp=29 lru=828e937b/5
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=7 nrec=6 nmax=8 o=recycle rp=29 lru=9d28e9be/6
ovf_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
prag_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
reidx_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
rt_p1024_c7_delete.sql @38
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p1024_c7_delete.sql decisions @12/54050
   C: TR|pc|stress|c=1 want=4 npage=6 szspill=1 synced=0 victim=2 vfl=e dl=9406f116/6
   R: TR|pc|stress|c=1 want=4 npage=6 szspill=1 synced=0 victim=5 vfl=e dl=c10dd176/6
rt_p16384_c5_delete.sql @38
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p16384_c5_delete.sql decisions @11/72529
   C: TR|pc|stress|c=1 want=5 npage=4 szspill=1 synced=0 victim=4 vfl=e dl=0cca213d/4
   R: TR|pc|stress|c=1 want=5 npage=4 szspill=1 synced=0 victim=0 vfl=0 dl=fc98f26d/4
rt_p2048_c4_truncate.sql @45
   C: TR|pcx|rel|pg=2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=3 nrec=1 nmax=4 o=recycle rp=4 lru=010c56d3/1
rt_p2048_c4_truncate.sql decisions @17/134670
   C: TR|pc|miss|c=1 pg=3 cf=2 npage=3 nrec=2 nmax=4 o=recycle rp=4 lru=597b8653/2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=3 nrec=1 nmax=4 o=recycle rp=4 lru=010c56d3/1
rt_p4096_c25_persist.sql @37
   C: TR|pcx|rel|pg=2
   R: TR|pcx|get|pg=3
rt_p4096_c25_persist.sql decisions @11/2268
   C: TR|pc|stress|c=1 want=23 npage=22 szspill=1 synced=0 victim=9 vfl=e dl=5bd3a915/22
   R: TR|pc|stress|c=1 want=23 npage=22 szspill=1 synced=0 victim=9 vfl=e dl=1cb951de/22
rt_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
rt_p8192_c9_wal.sql decisions @9/17084
   C: TR|pc|stress|c=2 want=10 npage=8 szspill=1 synced=7 victim=7 vfl=6 dl=a2f5e2c9/8
   R: TR|pc|stress|c=2 want=10 npage=8 szspill=1 synced=7 victim=7 vfl=6 dl=a531667d/8
sp_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
trig_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ups_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
wr_p1024_c7_delete.sql @25210
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p1024_c7_delete.sql decisions @11152/23497
   C: TR|pc|miss|c=1 pg=85 cf=1 npage=10 nrec=0 nmax=7 o=refuse
   R: TR|pc|miss|c=1 pg=25 cf=1 npage=10 nrec=0 nmax=7 o=refuse
wr_p16384_c5_delete.sql @3243
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p16384_c5_delete.sql decisions @355/4450
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=8 nrec=0 nmax=5 o=refuse
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=8 nrec=0 nmax=5 o=refuse
wr_p2048_c4_truncate.sql @13911
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p2048_c4_truncate.sql decisions @5042/22490
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=9 nrec=0 nmax=4 o=refuse
   R: TR|pc|miss|c=1 pg=43 cf=1 npage=9 nrec=0 nmax=4 o=refuse
wr_p4096_c25_persist.sql @6277
   C: TR|pcx|get|pg=3
   R: TR|pcx|get|pg=2
wr_p4096_c25_persist.sql decisions @1083/2729
   C: TR|pc|miss|c=1 pg=7 cf=1 npage=24 nrec=0 nmax=25 o=refuse
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=24 nrec=0 nmax=25 o=refuse
wr_p8192_c9_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
wr_p8192_c9_wal.sql decisions @1046/3741
   C: TR|pc|miss|c=2 pg=8 cf=1 npage=8 nrec=0 nmax=9 o=refuse
   R: TR|pc|miss|c=2 pg=5 cf=1 npage=8 nrec=0 nmax=9 o=refuse
```

Third set (`sc8`):

```
ps_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ps_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
psx_p1024_c6_delete.sql @47751
   C: TR|pc|miss|c=1 pg=139 cf=1 npage=6 nrec=1 nmax=6 o=refuse
   R: TR|pc|miss|c=1 pg=20 cf=1 npage=6 nrec=1 nmax=6 o=refuse
psx_p1024_c6_delete.sql decisions @24536/43221
   C: TR|pc|miss|c=1 pg=139 cf=1 npage=6 nrec=1 nmax=6 o=refuse
   R: TR|pc|miss|c=1 pg=20 cf=1 npage=6 nrec=1 nmax=6 o=refuse
psx_p4096_c10_delete.sql @19064
   C: TR|pc|miss|c=1 pg=43 cf=1 npage=10 nrec=3 nmax=10 o=recycle rp=41 lru=1662757b/3
   R: TR|pc|miss|c=1 pg=19 cf=1 npage=10 nrec=3 nmax=10 o=recycle rp=41 lru=1662757b/3
psx_p4096_c10_delete.sql decisions @7501/9999
   C: TR|pc|miss|c=1 pg=43 cf=1 npage=10 nrec=3 nmax=10 o=recycle rp=41 lru=1662757b/3
   R: TR|pc|miss|c=1 pg=19 cf=1 npage=10 nrec=3 nmax=10 o=recycle rp=41 lru=1662757b/3
psx_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
psx_p4096_c20_wal.sql decisions @4399/6787
   C: TR|pc|miss|c=2 pg=44 cf=1 npage=19 nrec=12 nmax=20 o=recycle rp=46 lru=c4292ef8/12
   R: TR|pc|miss|c=2 pg=20 cf=1 npage=19 nrec=12 nmax=20 o=recycle rp=46 lru=c4292ef8/12
psx_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
psx_p512_c30_wal.sql decisions @29263/35689
   C: TR|pc|miss|c=2 pg=296 cf=1 npage=29 nrec=2 nmax=30 o=refuse
   R: TR|pc|miss|c=2 pg=21 cf=1 npage=29 nrec=2 nmax=30 o=refuse
psx_p8192_c8_truncate.sql @26856
   C: TR|pc|miss|c=1 pg=32 cf=1 npage=8 nrec=3 nmax=8 o=recycle rp=15 lru=bbd89d62/3
   R: TR|pc|miss|c=1 pg=19 cf=1 npage=8 nrec=3 nmax=8 o=recycle rp=15 lru=bbd89d62/3
psx_p8192_c8_truncate.sql decisions @9821/29680
   C: TR|pc|miss|c=1 pg=32 cf=1 npage=8 nrec=3 nmax=8 o=recycle rp=15 lru=bbd89d62/3
   R: TR|pc|miss|c=1 pg=19 cf=1 npage=8 nrec=3 nmax=8 o=recycle rp=15 lru=bbd89d62/3
pt_p1024_c6_delete.sql @44436
   C: TR|pc|trunc|c=1 pgno=155
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
pt_p4096_c10_delete.sql @21865
   C: TR|pc|trunc|c=1 pgno=40
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
pt_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
pt_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
pt_p8192_c8_truncate.sql @29147
   C: TR|pc|trunc|c=1 pgno=35
   R: TR|pc|lock|c=1 from=1 to=2 rc=0
uq_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
uq_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
```
