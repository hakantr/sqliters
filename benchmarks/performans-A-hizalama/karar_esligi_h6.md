# H6 — karar eşliği (decision parity): the page cache decides like C

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


## 2. Decision-trace differential

Instrumented C: `h6/ctrace` (class `pc` in pcache.c / pcache1.c / pager.c, class `pcx` = every page reference). Rust: the same events through `pctr!` (`--cfg sqliters_dtrace`). Scenario set `h6/sc6` (`h6/gen_pc.py`), runner `h6/trall.py`, result directory `tr_r5`.

Two comparisons per script: **strict** = every `pc` event and every first-reference / last-release of a page other than page 1, in order; **decision stream** = the events that are decisions with observable effect (`miss o=refuse|recycle`, `stress` incl. victim / `pSynced` / dirty-list hash, `pstress`, `wr` with page hash, `walfr`, `syncj`, `jhdr`, `pb`, `subj`, `dontwrite`, `drop`, `purge`, `move`), in order.

- scripts: 145; events compared: C 5138570, Rust 4873675; decision events: C 720211, Rust 846703
- strict trace equal: **16**; decision stream equal: **63**; database file byte-identical: 140; stdout/stderr equal: 145

| family | scripts | strict equal | decisions equal | db identical |
|---|---|---|---|---|
| av | 5 | 0 | 0 | 5 |
| clr | 15 | 0 | 15 | 15 |
| ddl | 5 | 0 | 0 | 5 |
| del | 15 | 0 | 0 | 15 |
| f5 | 5 | 0 | 1 | 4 |
| ins_sel | 15 | 10 | 15 | 15 |
| ins_selidx | 15 | 3 | 15 | 15 |
| ins_stmts | 15 | 3 | 15 | 15 |
| ovf | 5 | 0 | 0 | 5 |
| prag | 5 | 0 | 1 | 5 |
| rt | 5 | 0 | 1 | 5 |
| sel | 15 | 0 | 0 | 15 |
| sp | 5 | 0 | 0 | 5 |
| upd | 15 | 0 | 0 | 15 |
| wr | 5 | 0 | 0 | 1 |

Families: `ins_sel` INSERT..SELECT into a rowid table; `ins_selidx` with two indexes; `ins_stmts` one INSERT statement per row + index; `clr` insert, `DELETE FROM t` (clear), insert again; `upd` / `del` / `sel` UPDATE / DELETE WHERE / SELECT on a filled table; `ovf` overflow rows; `wr` WITHOUT ROWID; `sp` savepoints + ROLLBACK TO + failing statement; `ddl` CREATE INDEX on a big table, DROP, VACUUM; `av` auto_vacuum; `f5` FTS5; `rt` R-Tree; `prag` cache pragmas inside transactions. Each in 15 configurations (page size 512..65536 x cache_size 0, 1, 3..30, default x journal_mode delete / wal / persist / truncate / memory) or 5.

## 3. Coverage (how often each outcome fired; all scripts | scripts whose decision stream is equal)

| decision outcome | C (all) | Rust (all) | C (equal scripts) | Rust (equal scripts) |
|---|---|---|---|---|
| `pcx|dropref` | 5 | 0 | 0 | 0 |
| `pcx|get` | 2130481 | 1934802 | 846899 | 770275 |
| `pcx|leak` | 0 | 50 | 0 | 0 |
| `pcx|rel` | 2130476 | 1934797 | 846899 | 770275 |
| `pc|cachesize|-` | 509 | 509 | 223 | 223 |
| `pc|dontwrite|set` | 1009 | 1008 | 923 | 923 |
| `pc|dontwrite|skip` | 3312 | 3744 | 1006 | 1006 |
| `pc|drop|reuse=0` | 74133 | 76770 | 35109 | 35109 |
| `pc|drop|reuse=1` | 5 | 0 | 0 | 0 |
| `pc|flags|spilloff=0` | 193 | 193 | 85 | 85 |
| `pc|flags|spilloff=1` | 5 | 5 | 1 | 1 |
| `pc|jhdr|-` | 909 | 933 | 172 | 172 |
| `pc|lock|to=2 rc=0` | 348 | 348 | 125 | 125 |
| `pc|lock|to=4 rc=0` | 348 | 348 | 125 | 125 |
| `pc|miss|alloc` | 84555 | 87581 | 39197 | 39155 |
| `pc|miss|recycle` | 62573 | 118656 | 20895 | 20895 |
| `pc|miss|refuse` | 114704 | 127576 | 55250 | 55250 |
| `pc|move|-` | 47168 | 47163 | 30792 | 30792 |
| `pc|nmax|-` | 509 | 509 | 223 | 223 |
| `pc|pb|file main=0 synced=1` | 1096 | 1096 | 0 | 0 |
| `pc|pb|load-dirty main=0 synced=1` | 157 | 158 | 0 | 0 |
| `pc|pstress|nospill` | 392 | 393 | 0 | 0 |
| `pc|pstress|sync+write` | 580 | 603 | 51 | 51 |
| `pc|pstress|wal` | 25917 | 28440 | 20266 | 20266 |
| `pc|pstress|write` | 67347 | 78594 | 34551 | 34551 |
| `pc|purge|-` | 10 | 10 | 6 | 6 |
| `pc|pw|append` | 65466 | 61369 | 36735 | 36735 |
| `pc|pw|journal` | 4580 | 5403 | 221 | 221 |
| `pc|shrink|-` | 5 | 5 | 1 | 1 |
| `pc|spillsize|-` | 25 | 20 | 4 | 4 |
| `pc|stressed|rc=0` | 94236 | 108030 | 54868 | 54868 |
| `pc|stress|below-spill` | 1685 | 5213 | 16 | 16 |
| `pc|stress|no-victim` | 18783 | 14333 | 366 | 366 |
| `pc|stress|victim` | 94236 | 108030 | 54868 | 54868 |
| `pc|subj|-` | 6285 | 6299 | 1324 | 1324 |
| `pc|syncj|newhdr=0 lockrc=0 nosync=0` | 348 | 348 | 125 | 125 |
| `pc|syncj|newhdr=1 lockrc=0 nosync=0` | 580 | 603 | 51 | 51 |
| `pc|trunc|-` | 854 | 1033 | 357 | 437 |
| `pc|walfr|commit=0` | 25917 | 28440 | 20266 | 20266 |
| `pc|walfr|commit=1` | 146 | 146 | 64 | 64 |
| `pc|wr|skip-beyond` | 364 | 368 | 0 | 0 |
| `pc|wr|skip-dontwrite` | 247 | 246 | 166 | 166 |
| `pc|wr|write` | 78072 | 89503 | 36508 | 36508 |

**Not verified** (the outcome never fired in a script whose decision stream is equal — listed, not passed):

- `pc|drop|reuse=1`
- `pc|pb|file main=0 synced=1`
- `pc|pb|load-dirty main=0 synced=1`
- `pc|pstress|nospill`
- `pc|wr|skip-beyond`
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
clr_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
clr_p2048_c3_persist.sql @2282
   C: TR|pc|pw|c=1 pg=4 o=append needsync=0
   R: TR|pc|subj|c=1 pg=4 nsub=3 h=16185e94
clr_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
clr_p4096_c0_delete.sql @2417
   C: TR|pc|pw|c=1 pg=4 o=append needsync=0
   R: TR|pc|subj|c=1 pg=4 nsub=3 h=df9f8ecd
clr_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p4096_c1_delete.sql @2862
   C: TR|pc|pw|c=1 pg=4 o=append needsync=0
   R: TR|pc|subj|c=1 pg=4 nsub=3 h=49faa797
clr_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
clr_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
clr_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
clr_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
clr_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ddl_p4096_c10_delete.sql @4065
   C: TR|pcx|get|pg=41
   R: TR|pc|miss|c=1 pg=42 cf=1 npage=9 nrec=6 nmax=10 o=recycle rp=35 lru=95cc85d0/6
ddl_p4096_c10_delete.sql decisions @250/3377
   C: TR|pc|stress|c=1 want=48 npage=9 szspill=1 synced=0 victim=42 vfl=e dl=89297868/8
   R: TR|pc|stress|c=1 want=48 npage=9 szspill=1 synced=0 victim=41 vfl=e dl=7e0b3a07/8
ddl_p4096_c12_truncate.sql @4553
   C: TR|pcx|get|pg=45
   R: TR|pc|miss|c=1 pg=46 cf=1 npage=11 nrec=8 nmax=12 o=recycle rp=37 lru=45c76055/8
ddl_p4096_c12_truncate.sql decisions @273/3959
   C: TR|pc|stress|c=1 want=53 npage=11 szspill=1 synced=0 victim=46 vfl=e dl=a13d9e56/9
   R: TR|pc|stress|c=1 want=53 npage=11 szspill=1 synced=0 victim=45 vfl=e dl=961f5ff5/9
ddl_p4096_c1_delete.sql @1945
   C: TR|pc|miss|c=1 pg=22 cf=1 npage=3 nrec=0 nmax=1 o=refuse
   R: TR|pcx|rel|pg=21
ddl_p4096_c1_delete.sql decisions @186/1544
   C: TR|pc|stress|c=1 want=22 npage=3 szspill=1 synced=0 victim=0 vfl=0 dl=d760e545/2
   R: TR|pc|stress|c=1 want=22 npage=3 szspill=1 synced=21 victim=21 vfl=6 dl=e27f23a6/2
ddl_p4096_cd_delete.sql @41534
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ddl_p4096_cd_delete.sql decisions @1128/18380
   C: TR|pc|miss|c=1 pg=801 cf=1 npage=482 nrec=481 nmax=483 o=recycle rp=321 lru=d698b9ab/481
   R: TR|pc|miss|c=1 pg=801 cf=1 npage=482 nrec=481 nmax=483 o=recycle rp=321 lru=38b60789/481
ddl_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ddl_p512_c30_wal.sql decisions @407/3238
   C: TR|pc|stress|c=2 want=105 npage=29 szspill=1 synced=81 victim=81 vfl=6 dl=c0345a3b/26
   R: TR|pc|stress|c=2 want=105 npage=29 szspill=1 synced=80 victim=80 vfl=6 dl=b6751b4f/26
del_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p1024_c5_delete.sql decisions @243/947
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=5 nrec=3 nmax=5 o=recycle rp=35 lru=86011ac8/3
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=35 lru=7f0eb948/4
del_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p16384_c6_wal.sql decisions @389/2372
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=6 nrec=4 nmax=6 o=recycle rp=40 lru=147f7652/4
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=6 nrec=5 nmax=6 o=recycle rp=40 lru=c869290d/5
del_p2048_c3_persist.sql @1621
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=1 nmax=3 o=recycle rp=28 lru=190c7c9b/1
   R: TR|pcx|rel|pg=2
del_p2048_c3_persist.sql decisions @220/868
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=1 nmax=3 o=recycle rp=28 lru=190c7c9b/1
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=2 nmax=3 o=recycle rp=28 lru=717bac1b/2
del_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p32768_c5_wal.sql decisions @335/2949
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=5 nrec=3 nmax=5 o=recycle rp=36 lru=57412c7a/3
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=36 lru=bbdbc10d/4
del_p4096_c0_delete.sql @1954
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=2 nrec=0 nmax=0 o=alloc
   R: TR|pcx|rel|pg=2
del_p4096_c0_delete.sql decisions @172/668
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=22
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=0
del_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p4096_c10_delete.sql decisions @339/1625
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=7 nmax=10 o=recycle rp=16 lru=0560534f/7
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=8 nmax=10 o=recycle rp=16 lru=fc39ade9/8
del_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p4096_c12_truncate.sql decisions @298/1649
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=9 nmax=12 o=recycle rp=27 lru=187d0770/9
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=10 nmax=12 o=recycle rp=27 lru=8ae7348e/10
del_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p4096_c15_memory.sql decisions @332/1753
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=12 nmax=15 o=recycle rp=60 lru=dedc6f01/12
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=13 nmax=15 o=recycle rp=60 lru=71021da7/13
del_p4096_c1_delete.sql @2330
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=2 nrec=0 nmax=1 o=alloc
   R: TR|pcx|rel|pg=2
del_p4096_c1_delete.sql decisions @198/790
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=25
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=1
del_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
del_p4096_c20_wal.sql decisions @371/1376
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=17 nmax=20 o=recycle rp=70 lru=5dc043a9/17
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=18 nmax=20 o=recycle rp=70 lru=48937214/18
del_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p4096_cd_delete.sql decisions @1757/4839
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=480 nmax=483 o=recycle rp=32 lru=e052ea42/480
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=32 lru=a04f13da/481
del_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
del_p4096_cd_wal.sql decisions @1314/4045
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=480 nmax=483 o=recycle rp=33 lru=44c4f3b4/480
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=33 lru=e7f90467/481
del_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
del_p512_c30_wal.sql decisions @463/1786
   C: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=27 nmax=30 o=recycle rp=100 lru=34ddb8e7/27
   R: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=28 nmax=30 o=recycle rp=100 lru=4d520026/28
del_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p65536_c4_delete.sql decisions @318/4492
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=4 nrec=2 nmax=4 o=recycle rp=15 lru=c2b4842f/2
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=4 nrec=3 nmax=4 o=recycle rp=2 lru=af2c12d7/3
del_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
del_p8192_c8_delete.sql decisions @460/1963
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=6 nmax=8 o=recycle rp=15 lru=7ea19dee/6
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=7 nmax=8 o=recycle rp=15 lru=57d784a8/7
f5_p4096_c10_delete.sql @22
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c10_delete.sql decisions @14/2467
   C: TR|pc|miss|c=1 pg=10 cf=1 npage=9 nrec=3 nmax=10 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=10 cf=1 npage=9 nrec=3 nmax=10 o=recycle rp=6 lru=4392bfa2/3
f5_p4096_c12_truncate.sql @22
   C: TR|pcx|rel|pg=2
   R: TR|pc|subj|c=1 pg=1 nsub=0
f5_p4096_c12_truncate.sql decisions @14/1599
   C: TR|pc|miss|c=1 pg=12 cf=1 npage=11 nrec=3 nmax=12 o=recycle rp=6 lru=f080dbdc/3
   R: TR|pc|miss|c=1 pg=12 cf=1 npage=11 nrec=3 nmax=12 o=recycle rp=6 lru=4392bfa2/3
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
   R: TR|pc|miss|c=2 pg=31 cf=1 npage=29 nrec=5 nmax=30 o=recycle rp=8 lru=43ad73b1/5
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
ins_selidx_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_selidx_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_selidx_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_selidx_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_selidx_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_stmts_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
ins_stmts_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ins_stmts_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ins_stmts_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
ovf_p4096_c10_delete.sql @2752
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=30 cf=1 npage=9 nrec=0 nmax=10 o=refuse
ovf_p4096_c10_delete.sql decisions @1694/2188
   C: TR|pc|stress|c=1 want=30 npage=9 szspill=1 synced=285 victim=285 vfl=6 dl=f31af82c/9
   R: TR|pc|stress|c=1 want=30 npage=9 szspill=1 synced=285 victim=285 vfl=6 dl=6d08d3cb/9
ovf_p4096_c12_truncate.sql @3075
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=30 cf=1 npage=11 nrec=1 nmax=12 o=refuse
ovf_p4096_c12_truncate.sql decisions @1891/2419
   C: TR|pc|stress|c=1 want=30 npage=11 szspill=1 synced=318 victim=318 vfl=6 dl=dae283a9/10
   R: TR|pc|stress|c=1 want=30 npage=11 szspill=1 synced=318 victim=318 vfl=6 dl=f27aaa7e/10
ovf_p4096_c1_delete.sql @2003
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=30 cf=1 npage=5 nrec=0 nmax=1 o=refuse
ovf_p4096_c1_delete.sql decisions @1127/1881
   C: TR|pc|stress|c=1 want=30 npage=5 szspill=1 synced=189 victim=189 vfl=6 dl=1a51985a/5
   R: TR|pc|stress|c=1 want=30 npage=5 szspill=1 synced=189 victim=189 vfl=6 dl=3d61a1e9/5
ovf_p4096_cd_delete.sql @27983
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=30 cf=1 npage=482 nrec=48 nmax=483 o=refuse
ovf_p4096_cd_delete.sql decisions @16124/20985
   C: TR|pc|stress|c=1 want=30 npage=482 szspill=1 synced=2698 victim=2698 vfl=6 dl=1c1f1d9a/434
   R: TR|pc|stress|c=1 want=30 npage=482 szspill=1 synced=2698 victim=2698 vfl=6 dl=9b0c23a1/434
ovf_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
ovf_p512_c30_wal.sql decisions @980/1267
   C: TR|pc|stress|c=2 want=37 npage=29 szspill=1 synced=168 victim=168 vfl=6 dl=8056606c/27
   R: TR|pc|stress|c=2 want=37 npage=29 szspill=1 synced=168 victim=168 vfl=6 dl=1d7c8c57/27
prag_p4096_c10_delete.sql @13452
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=70 nmax=72 o=recycle rp=135 lru=522cfeee/70
prag_p4096_c10_delete.sql decisions @535/704
   C: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=69 nmax=72 o=recycle rp=135 lru=eefafbf8/69
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=70 nmax=72 o=recycle rp=135 lru=522cfeee/70
prag_p4096_c12_truncate.sql @15065
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=70 nmax=72 o=recycle rp=143 lru=e0b1f5e2/70
prag_p4096_c12_truncate.sql decisions @603/792
   C: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=69 nmax=72 o=recycle rp=143 lru=3f201a5c/69
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=71 nrec=70 nmax=72 o=recycle rp=143 lru=e0b1f5e2/70
prag_p4096_c1_delete.sql @6217
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=3 cf=2 npage=51 nrec=50 nmax=72 o=alloc
prag_p4096_c1_delete.sql decisions @256/292
   C: TR|pc|miss|c=1 pg=23 cf=2 npage=71 nrec=69 nmax=72 o=recycle rp=47 lru=4540063b/69
   R: TR|pc|miss|c=1 pg=23 cf=2 npage=71 nrec=70 nmax=72 o=recycle rp=47 lru=d604dbf3/70
prag_p4096_cd_delete.sql @245991
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=499 cf=2 npage=72 nrec=71 nmax=72 o=recycle rp=1763 lru=cfdb7cde/71
prag_p4096_cd_delete.sql decisions @9030/10857
   C: TR|pc|miss|c=1 pg=499 cf=2 npage=72 nrec=70 nmax=72 o=recycle rp=1763 lru=21ae2bd6/70
   R: TR|pc|miss|c=1 pg=499 cf=2 npage=72 nrec=71 nmax=72 o=recycle rp=1763 lru=cfdb7cde/71
prag_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
rt_p4096_c10_delete.sql @27
   C: TR|pcx|rel|pg=3
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
rt_p4096_c10_delete.sql decisions @11/10999
   C: TR|pc|stress|c=1 want=10 npage=9 szspill=1 synced=0 victim=6 vfl=e dl=9f435eb9/9
   R: TR|pc|stress|c=1 want=10 npage=9 szspill=1 synced=0 victim=6 vfl=e dl=f20e40e9/9
rt_p4096_c12_truncate.sql @27
   C: TR|pcx|rel|pg=3
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
rt_p4096_c12_truncate.sql decisions @11/8024
   C: TR|pc|stress|c=1 want=11 npage=10 szspill=1 synced=0 victim=6 vfl=e dl=2253ceba/10
   R: TR|pc|stress|c=1 want=11 npage=10 szspill=1 synced=0 victim=6 vfl=e dl=4b72ef41/10
rt_p4096_c1_delete.sql @65
   C: TR|pc|miss|c=1 pg=2 cf=2 npage=2 nrec=0 nmax=1 o=alloc
   R: TR|pcx|rel|pg=3
rt_p4096_c1_delete.sql decisions @31/90968
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=4
   R: TR|pc|drop|c=1 pg=3 reuse=0 npage=2 nmax=1
rt_p4096_cd_delete.sql @25
   C: TR|pcx|rel|pg=3
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
rt_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
rt_p512_c30_wal.sql decisions @9/2309
   C: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=7 victim=7 vfl=6 dl=d5f89540/27
   R: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=7 victim=7 vfl=6 dl=8de63d4b/27
sel_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p1024_c5_delete.sql decisions @246/379
   C: TR|pc|miss|c=1 pg=2 cf=2 npage=5 nrec=2 nmax=5 o=recycle rp=17 lru=f8a80d3e/2
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=16 lru=3651a123/4
sel_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sel_p16384_c6_wal.sql decisions @392/530
   C: TR|pc|miss|c=2 pg=3 cf=2 npage=6 nrec=3 nmax=6 o=recycle rp=17 lru=7404f22d/3
   R: TR|pc|miss|c=2 pg=3 cf=2 npage=6 nrec=5 nmax=6 o=recycle rp=16 lru=02167029/5
sel_p2048_c3_persist.sql @1630
   C: TR|pcx|get|pg=3
   R: TR|pcx|leak|c=1 pages=1
sel_p2048_c3_persist.sql decisions @223/357
   C: TR|pc|miss|c=1 pg=15 cf=2 npage=3 nrec=1 nmax=3 o=recycle rp=28 lru=190c7c9b/1
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=3 nrec=2 nmax=3 o=recycle rp=28 lru=33790bea/2
sel_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sel_p32768_c5_wal.sql decisions @337/481
   C: TR|pc|miss|c=2 pg=3 cf=2 npage=5 nrec=2 nmax=5 o=recycle rp=17 lru=f8a80d3e/2
   R: TR|pc|miss|c=2 pg=3 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=16 lru=992023dc/4
sel_p4096_c0_delete.sql @1961
   C: TR|pcx|rel|pg=3
   R: TR|pc|drop|c=1 pg=1 reuse=0 npage=2 nmax=0
sel_p4096_c0_delete.sql decisions @174/282
   C: TR|pc|drop|c=1 pg=3 reuse=0 npage=2 nmax=0
   R: TR|pc|drop|c=1 pg=1 reuse=0 npage=2 nmax=0
sel_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p4096_c10_delete.sql decisions @340/476
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=5 nmax=10 o=recycle rp=49 lru=1ecfdaee/5
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=8 nmax=10 o=recycle rp=2 lru=baed2d2a/8
sel_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p4096_c12_truncate.sql decisions @298/433
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=7 nmax=12 o=recycle rp=55 lru=7512c50e/7
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=10 nmax=12 o=recycle rp=2 lru=79e908ca/10
sel_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p4096_c15_memory.sql decisions @332/476
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=10 nmax=15 o=recycle rp=60 lru=49551917/10
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=13 nmax=15 o=recycle rp=60 lru=63ef3c3f/13
sel_p4096_c1_delete.sql @2337
   C: TR|pcx|rel|pg=3
   R: TR|pc|drop|c=1 pg=1 reuse=0 npage=2 nmax=1
sel_p4096_c1_delete.sql decisions @200/302
   C: TR|pc|drop|c=1 pg=3 reuse=0 npage=2 nmax=1
   R: TR|pc|drop|c=1 pg=1 reuse=0 npage=2 nmax=1
sel_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
sel_p4096_c20_wal.sql decisions @371/512
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=15 nmax=20 o=recycle rp=70 lru=c6ed0291/15
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=18 nmax=20 o=recycle rp=70 lru=c82b4686/18
sel_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p4096_cd_delete.sql decisions @1757/2331
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=478 nmax=483 o=recycle rp=32 lru=508f8905/478
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=32 lru=1220ebf0/481
sel_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
sel_p4096_cd_wal.sql decisions @1314/1727
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=478 nmax=483 o=recycle rp=33 lru=317e65ab/478
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=33 lru=862ea6fd/481
sel_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sel_p512_c30_wal.sql decisions @463/573
   C: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=25 nmax=30 o=recycle rp=103 lru=b844244c/25
   R: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=28 nmax=30 o=recycle rp=103 lru=3d661670/28
sel_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p65536_c4_delete.sql decisions @321/642
   C: TR|pc|miss|c=1 pg=15 cf=2 npage=4 nrec=2 nmax=4 o=recycle rp=16 lru=b7b472de/2
   R: TR|pc|miss|c=1 pg=2 cf=2 npage=4 nrec=3 nmax=4 o=recycle rp=16 lru=43817681/3
sel_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
sel_p8192_c8_delete.sql decisions @460/617
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=4 nmax=8 o=recycle rp=44 lru=9633292a/4
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=7 nmax=8 o=recycle rp=44 lru=dff8841e/7
sp_p4096_c10_delete.sql @2306
   C: TR|pc|pw|c=1 pg=15 o=append needsync=0
   R: TR|pc|subj|c=1 pg=15 nsub=5 h=26afb866
sp_p4096_c10_delete.sql decisions @340/1279
   C: TR|pc|stress|c=1 want=4 npage=9 szspill=1 synced=28 victim=28 vfl=6 dl=f989df98/9
   R: TR|pc|stress|c=1 want=4 npage=9 szspill=1 synced=28 victim=28 vfl=6 dl=ef009053/9
sp_p4096_c12_truncate.sql @2334
   C: TR|pc|pw|c=1 pg=15 o=append needsync=0
   R: TR|pc|subj|c=1 pg=15 nsub=5 h=26afb866
sp_p4096_c12_truncate.sql decisions @339/1387
   C: TR|pc|stress|c=1 want=4 npage=11 szspill=1 synced=58 victim=58 vfl=6 dl=e44ec195/11
   R: TR|pc|stress|c=1 want=4 npage=11 szspill=1 synced=58 victim=58 vfl=6 dl=4ed7efaa/11
sp_p4096_c1_delete.sql @40
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=3
   R: TR|pcx|rel|pg=3
sp_p4096_c1_delete.sql decisions @15/674
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=3
   R: TR|pc|drop|c=1 pg=3 reuse=0 npage=3 nmax=1
sp_p4096_cd_delete.sql @47666
   C: TR|pcx|get|pg=2
   R: TR|pc|miss|c=1 pg=4 cf=1 npage=482 nrec=48 nmax=483 o=refuse
sp_p4096_cd_delete.sql decisions @801/9600
   C: TR|pc|stress|c=1 want=4 npage=482 szspill=1 synced=146 victim=146 vfl=6 dl=93466f12/434
   R: TR|pc|stress|c=1 want=4 npage=482 szspill=1 synced=146 victim=146 vfl=6 dl=a7e433de/434
sp_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
sp_p512_c30_wal.sql decisions @588/2384
   C: TR|pc|stress|c=2 want=5 npage=29 szspill=1 synced=69 victim=69 vfl=6 dl=3aff4dea/27
   R: TR|pc|stress|c=2 want=5 npage=29 szspill=1 synced=69 victim=69 vfl=6 dl=a679fff6/27
upd_p1024_c5_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p1024_c5_delete.sql decisions @243/865
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=5 nrec=3 nmax=5 o=recycle rp=35 lru=86011ac8/3
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=35 lru=7f0eb948/4
upd_p16384_c6_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p16384_c6_wal.sql decisions @389/2079
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=6 nrec=4 nmax=6 o=recycle rp=40 lru=147f7652/4
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=6 nrec=5 nmax=6 o=recycle rp=40 lru=c869290d/5
upd_p2048_c3_persist.sql @1621
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=1 nmax=3 o=recycle rp=28 lru=190c7c9b/1
   R: TR|pcx|rel|pg=2
upd_p2048_c3_persist.sql decisions @220/1503
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=1 nmax=3 o=recycle rp=28 lru=190c7c9b/1
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=3 nrec=2 nmax=3 o=recycle rp=28 lru=717bac1b/2
upd_p32768_c5_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p32768_c5_wal.sql decisions @335/1376
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=5 nrec=3 nmax=5 o=recycle rp=36 lru=57412c7a/3
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=5 nrec=4 nmax=5 o=recycle rp=36 lru=bbdbc10d/4
upd_p4096_c0_delete.sql @1954
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=2 nrec=0 nmax=0 o=alloc
   R: TR|pcx|rel|pg=2
upd_p4096_c0_delete.sql decisions @172/1312
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=22
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=0
upd_p4096_c10_delete.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p4096_c10_delete.sql decisions @339/916
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=7 nmax=10 o=recycle rp=16 lru=0560534f/7
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=9 nrec=8 nmax=10 o=recycle rp=16 lru=fc39ade9/8
upd_p4096_c12_truncate.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p4096_c12_truncate.sql decisions @298/950
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=9 nmax=12 o=recycle rp=27 lru=187d0770/9
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=11 nrec=10 nmax=12 o=recycle rp=27 lru=8ae7348e/10
upd_p4096_c15_memory.sql @28
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p4096_c15_memory.sql decisions @332/1064
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=12 nmax=15 o=recycle rp=60 lru=dedc6f01/12
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=14 nrec=13 nmax=15 o=recycle rp=60 lru=71021da7/13
upd_p4096_c1_delete.sql @2330
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=2 nrec=0 nmax=1 o=alloc
   R: TR|pcx|rel|pg=2
upd_p4096_c1_delete.sql decisions @198/937
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=25
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=1
upd_p4096_c20_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
upd_p4096_c20_wal.sql decisions @371/1306
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=17 nmax=20 o=recycle rp=70 lru=5dc043a9/17
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=19 nrec=18 nmax=20 o=recycle rp=70 lru=48937214/18
upd_p4096_cd_delete.sql @26
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p4096_cd_delete.sql decisions @1757/5423
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=480 nmax=483 o=recycle rp=32 lru=e052ea42/480
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=32 lru=a04f13da/481
upd_p4096_cd_wal.sql @3
   C: TR|pc|miss|c=1 pg=1 cf=2 npage=0 nrec=0 nmax=483 o=alloc
   R: TR|pc|trunc|c=1 pgno=0
upd_p4096_cd_wal.sql decisions @1314/4976
   C: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=480 nmax=483 o=recycle rp=33 lru=44c4f3b4/480
   R: TR|pc|miss|c=2 pg=5 cf=2 npage=482 nrec=481 nmax=483 o=recycle rp=33 lru=e7f90467/481
upd_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
upd_p512_c30_wal.sql decisions @463/1798
   C: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=27 nmax=30 o=recycle rp=100 lru=34ddb8e7/27
   R: TR|pc|miss|c=2 pg=79 cf=2 npage=29 nrec=28 nmax=30 o=recycle rp=100 lru=4d520026/28
upd_p65536_c4_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p65536_c4_delete.sql decisions @318/9030
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=4 nrec=2 nmax=4 o=recycle rp=15 lru=c2b4842f/2
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=4 nrec=3 nmax=4 o=recycle rp=2 lru=af2c12d7/3
upd_p8192_c8_delete.sql @29
   C: TR|pcx|rel|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
upd_p8192_c8_delete.sql decisions @460/1505
   C: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=6 nmax=8 o=recycle rp=15 lru=7ea19dee/6
   R: TR|pc|miss|c=1 pg=4 cf=2 npage=8 nrec=7 nmax=8 o=recycle rp=15 lru=57d784a8/7
wr_p4096_c10_delete.sql @27
   C: TR|pcx|get|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
wr_p4096_c10_delete.sql decisions @18/5570
   C: TR|pc|stress|c=1 want=8 npage=9 szspill=1 synced=4 victim=4 vfl=6 dl=cdec54a6/9
   R: TR|pc|stress|c=1 want=8 npage=9 szspill=1 synced=4 victim=4 vfl=6 dl=57befc8d/9
wr_p4096_c12_truncate.sql @27
   C: TR|pcx|get|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
wr_p4096_c12_truncate.sql decisions @41/6227
   C: TR|pc|stress|c=1 want=9 npage=11 szspill=1 synced=4 victim=4 vfl=6 dl=571d8e28/11
   R: TR|pc|stress|c=1 want=9 npage=11 szspill=1 synced=4 victim=4 vfl=6 dl=0ac27ab7/11
wr_p4096_c1_delete.sql @51
   C: TR|pc|miss|c=1 pg=3 cf=2 npage=2 nrec=0 nmax=1 o=alloc
   R: TR|pcx|rel|pg=2
wr_p4096_c1_delete.sql decisions @21/4727
   C: TR|pc|jhdr|c=1 off=0 nrec=0 orig=3
   R: TR|pc|drop|c=1 pg=2 reuse=0 npage=2 nmax=1
wr_p4096_cd_delete.sql @25
   C: TR|pcx|get|pg=2
   R: TR|pc|lock|c=1 from=2 to=4 rc=0
wr_p4096_cd_delete.sql decisions @9201/9661
   C: TR|pc|move|c=1 from=430 to=262145 fl=e
   R: TR|pc|dontwrite|c=1 pg=143 fl=e nsp=0 o=set
wr_p512_c30_wal.sql @1
   C: TR|pc|trunc|c=1 pgno=0
   R: TR|pc|nmax|c=1 nmax=483 n90=434 mxpin=483 npage=0
wr_p512_c30_wal.sql decisions @6/1996
   C: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=5 victim=5 vfl=6 dl=4bdbe998/27
   R: TR|pc|stress|c=2 want=29 npage=27 szspill=1 synced=5 victim=5 vfl=6 dl=896e6750/27
```
