# Step H3 — decision parity of the bulk statements with C SQLite 3.54.1

Scope: `CREATE INDEX` / `REINDEX` (sorter → bulk append), the transfer optimisation of `INSERT INTO d SELECT * FROM s`,
`VACUUM` / `VACUUM INTO`, `ALTER TABLE … RENAME TO / RENAME COLUMN / DROP COLUMN`. Source references are to
`sqlite/src/` of the repository (3.54.1). The Rust side is `crates/sqliters-btree/src/insert.rs` (append / preformat
inserts), `crates/sqliters-core/src/engine.rs` (`refill_index_from_sorter`, `exec_reindex`, `xfer_optimization`,
`xfer_copy`, `build_vacuum_image`, `update_schema_rows`).

## 0. Method

* **C side**: the instrumented amalgamation of step H2 (`h2/ctrace`) copied to `h3/ctrace`, plus `patch_h3.py`:
  * `TR|xf|open|root= bulk=1` — `OP_OpenWrite` with `OPFLAG_BULKCSR` (vdbe.c `open_cursor_set_hints`);
  * `TR|xf|ins|root= app= bulk= pre=` — every `sqlite3BtreeInsert` through a `BTREE_BULKLOAD` cursor or with
    `BTREE_PREFORMAT` (`app` = `seekResult<0`, i.e. `OP_SeekEnd` + `OPFLAG_USESEEKRESULT`);
  * `TR|xf|row|pay= local= o=local|overflow` — `sqlite3BtreeTransferRow` (btree.c:9759);
  * `TR|ridx|sorted|root= n= h=` — FNV-1a hash over the keys in the order `OP_SorterData` hands them to the bulk cursor,
    printed when `OP_SorterNext` reaches the end; `TR|ridx|dup|n=` — `OP_SorterCompare` found a duplicate;
  * `TR|xfd|no|why=N` / `TR|xfd|go|dest= src= empty=` — which `return 0` of `xferOptimization` fired (numbered in source
    order), or that the transfer was coded and whether it is guarded by `emptyDestTest`;
  * `TR|vac|stmt|…` (every statement `execSql` runs on the copy), `TR|vac|meta|idx= val=` (the `aCopy[]` loop),
    `TR|vac|copy|into= pages= pgsz= av=`.
  The classes of H1/H2 stay in force: `ins` (existing cell? gap / freeblock / defragment / overflow cell), `bal` (every
  step of `balance()`, sibling choice with `bulk=`, distribution, dividers, `editPage`/rebuild, pages), `alloc`, `free`.
* **Rust side**: the same events from `sqliters_types::dtrace!` (compiled only with `--cfg sqliters_dtrace`).
* **Differ** `h3/trins.py`: both engines start from an EMPTY file and run the whole script; all classes are compared as
  ONE sequence, event by event, every field — except `xfd`, which C decides while it compiles a statement (or a trigger
  program) and which is compared as a sequence of its own. Also compared: stdout, stderr (error messages), C and Rust
  `PRAGMA integrity_check` on both files, page/freelist counts, the `dbstat` shape, the bytes of the two database files
  and of every file written by `VACUUM INTO` / `ATTACH`.
  As in C (`!pPager->tempFile`) events of temporary databases are not printed: the build phase of a plain `VACUUM` runs
  in a temp database and is therefore compared event by event only in the `VACUUM INTO` scripts (same code), and by
  bytes for plain `VACUUM`.
* **Scenarios**: the 180 scripts of H2 (`h3/sc`, unchanged) + 137 new ones (`h3/gen_sc3.py` → `h3/sc3`): families
  `ci` (CREATE INDEX), `ri` (REINDEX), `xf` (transfer; `xf_c_*` = one script per disabling condition), `va`
  (VACUUM / VACUUM INTO), `rn` (RENAME / ADD / DROP COLUMN). Page sizes 512 … 65536, auto_vacuum 0/1/2.

## 1. `sqlite3CreateIndex` / `sqlite3RefillIndex` (build.c)

| # | C condition (line) | outcome in C | Rust | trace |
| :-- | :-- | :-- | :-- | :-- |
| C1 | `sqlite3CreateIndex` order (4521, 4541, 4555) | `OP_CreateBtree`, then `INSERT INTO sqlite_schema VALUES('index',…)`, then `sqlite3RefillIndex`, then cookie | `exec_create_index`: `create_index()`, `insert_schema_entry`, `populate_index_btree` (was: fill first, row last) | `alloc`, `ins` (root=1), then the fill |
| R1 | sorter key (3889; vdbesort.c:1134) `OP_SorterOpen … P3=nKeyCol`: with one worker the sorter compares the first `nKeyCol` fields only and is stable | equal keys leave the sorter in table-scan order (rowid / PRIMARY KEY order) | `refill_index_from_sorter`: unstable sort on (digest, first `n_key` fields, arena offset) — the offset is the scan order, so the result is the stable order | `ridx|sorted` hash |
| R2 | `memRootPage<0` (3904) | `OP_Clear` after the table scan, before the index is opened (REINDEX) | `clear_first` → `Btree::clear_tree` after the keys are collected | `free` events |
| R3 | (3905–3907) `OP_OpenWrite` + `OPFLAG_BULKCSR` | cursor hint `BTREE_BULKLOAD` → `balance_nonroot(…, bBulk=1)` (btree.c:8351, 8355, 8663, 8713) | `AppendCursor::new(root, true)` → `BalCursor.bulk` | `xf|open`, `bal|sib … bulk=1` |
| R4 | `IsUniqueIndex` (3910–3917) | from the second key on `OP_SorterCompare` against the previous key on `nKeyCol` fields; a NULL among them never matches (vdbesort.c:2939); match → `sqlite3UniqueConstraint(OE_Abort)` with the keys before it inserted | `next` closure in `refill_index_from_sorter` (same test, same message / extended code via `format_unique_index_failure`) | `ridx|dup` |
| R5 | `!pIndex->bAscKeyBug` (3930) | `OP_SeekEnd` + `OP_IdxInsert` with `OPFLAG_USESEEKRESULT`: the key becomes the last entry without any comparison | `Btree::append_index_run` (`append_index_c`) | `xf|ins app=1 bulk=1 pre=0` |
| R6 | `bAscKeyBug` (build.c:2560: UNIQUE-constraint index of a WITHOUT ROWID table to which a DESC PRIMARY KEY column was appended) | no `OP_SeekEnd`: `seekResult` is 0, the key is placed by `sqlite3BtreeIndexMoveto` (cursor still `BTREE_BULKLOAD`) | `index_has_asc_key_bug` → `insert_index_bulk` | `xf|ins app=0 bulk=1 pre=0` |
| R7 | `sqlite3GenerateIndexKey` (called at 3899): column read + `OP_RealAffinity` deleted | the key of a REAL column is the stored form (an integer when the table holds one) | fast scan path (already) and now the general path (`index_keys_take_stored_form`) | bytes |
| X1 | `sqlite3Reindex` (5645–5725): one loop over `db->aDb[]` (or the named database), `tblHash` order, `pTab->pIndex` order | index rebuilt when: no argument, or `pTab==pReTab`, or `pIdx==pReIndex`, or `expressions` and `pIdx->bHasExpr`, or `collationMatch` (5610: any of the `nColumn` columns, the appended rowid — BINARY — included); a collation name wins over a table/index of the same name; nothing matched → "unable to identify the object to be reindexed" | `exec_reindex` (rewritten); PRIMARY KEY of a WITHOUT ROWID table rebuilt at its place in the index list (`refill_without_rowid_primary_key`) | order of `free` / `xf|open` / `ridx|sorted` |

## 2. `xferOptimization` (insert.c:3069)

Called only when `pColumn==0 && pSelect!=0 && pTrigger==0` (insert.c:1024–1027; RETURNING is a trigger). The `why` number
is the ordinal of the `return 0` in the source (the one inside `SQLITE_ENABLE_HIDDEN_COLUMNS`, line 3191, is not compiled
and not counted).

| why | line | condition that disables the transfer |
| ---: | ---: | :-- |
| 1 | 3097 | a WITH clause on the INSERT or the SELECT |
| 2 | 3101 | destination is a virtual table |
| 3 | 3110 | FROM has not exactly one term |
| 4 | 3113 | FROM term is a subquery |
| 5 | 3116 | WHERE |
| 6 | 3119 | ORDER BY |
| 7 | 3124 | GROUP BY |
| 8 | 3127 | LIMIT |
| 9 | 3130 | compound SELECT (tests 3–8 look at its right-most term) |
| 10 | 3133 | DISTINCT |
| 11 | 3138 | result set has not exactly one column |
| 12 | 3142 | result column is not `*` (`t.*` is not) |
| 13 | 3152 | source table not found |
| 14 | 3156 | source is the destination |
| 15 | 3159 | one is WITHOUT ROWID, the other is not |
| 16 | 3162 | source is a view or virtual table |
| 17 | 3165 | different number of columns |
| 18 | 3168 | different INTEGER PRIMARY KEY column |
| 19 | 3172 | destination STRICT, source not |
| 20 | 3181 | STRICT column types incompatible |
| 21 | 3213 | generated-column kind differs |
| 22 | 3225 | generated-column expressions differ |
| 23 | 3230 | affinity differs |
| 24 | 3234 | collation differs |
| 25 | 3237 | destination NOT NULL, source not |
| 26 | 3251 | DEFAULT of a column other than the first differs (text comparison) |
| 27 | 3263 | a destination index has no `xferCompatibleIndex` in the source |
| 28 | 3270 | compatible index is the same b-tree (corrupt schema) |
| 29 | 3278 | CHECK constraints differ (not under `DBFLAG_Vacuum`) |
| 30 | 3291 | `PRAGMA foreign_keys` on and the destination has foreign keys |
| 31 | 3295 | `PRAGMA count_changes` |
| (32, 33) | 3300, 3304 | authorizer denies — no authorizer in the shell |

Run-time decisions once the transfer is coded:

| # | C condition (line) | outcome | Rust (`xfer_copy`, `exec_insert_xfer`) | trace |
| :-- | :-- | :-- | :-- | :-- |
| E1 | not VACUUM and ((1) no INTEGER PRIMARY KEY but indexes, or (2) a UNIQUE index, or (3) `onError` not ABORT/ROLLBACK) (3328–3352) | `emptyDestTest`: transfer only into an empty table; otherwise the ordinary insert that was coded behind it runs (`xferOptimization` returns 0 — with generated columns that second half does not compile: "table d has N columns but M values were supplied") | `XferPlan.needs_empty_dest`; `exec_insert_xfer` returns `false` → ordinary insert | `xfd|go empty=1`, then `xf` events or ordinary `ins` events |
| E2 | `emptySrcTest` (3357, 3460) | rowid source table empty → index loops skipped | `n_rows == 0` → return | no `xf|open` |
| K1 | `pDest->iPKey>=0` (3359–3367) | `OP_Rowid`; not VACUUM: `OP_NotExists` → `sqlite3RowidConstraint(onError)`; `autoIncStep` | lookup (skipped for a rowid above the destination's largest), `autoinc_note_rowid` | error text / code |
| K2 | no IPK, `pDest->pIndex==0`, not VACUUM INTO (3368–3369) | `OP_NewRowid` on the destination: rows are renumbered | `next_rowid` | SQL result (`rowid`) |
| K3 | otherwise (3371) | source rowid kept | same | |
| T1 | VACUUM (3375–3377) | `OP_SeekEnd`; `OP_Insert` `APPEND\|USESEEKRESULT\|PREFORMAT` | `append_table_transfer` | `xf|ins app=1 bulk=0 pre=1` |
| T2 | not VACUUM (3379) | `OP_Insert` `NCHANGE\|LASTROWID\|APPEND\|PREFORMAT` (place found by `sqlite3BtreeTableMoveto`) | `insert_table_transfer_sorted` | `xf|ins app=0 bulk=0 pre=1` |
| T3 | `sqlite3BtreeTransferRow` (btree.c:9759): `nIn==nRem && nIn<maxLocal` → copy; else `btreePayloadToLocal` of the DESTINATION page geometry, overflow pages by `allocateBtreePage(…, nearby=0, 0)` (9834), allocated before the insert | local / overflow | `fill_in_cell` with `Btree.preformat` (`fill_overflow_chain`: `nearby = 0`), built before the seek | `xf|row o=local|overflow`, `alloc` |
| I1 | every destination index in `pDest->pIndex` order, source = first compatible index of `pSrc->pIndex` (3403–3414), cursor `OPFLAG_BULKCSR` | | `xfer_index_pairs` / `c_index_list_order` | `xf|open` order |
| I2 | VACUUM and every column of the source index BINARY (3432–3439) | `OP_SeekEnd` + `OP_RowCell` + `OP_IdxInsert` `USESEEKRESULT\|PREFORMAT\|APPEND` | `append_index(.., preformat)` | `xf|ins app=1 bulk=1 pre=1` |
| I3 | otherwise (3444–3454) | `OP_RowData` + `OP_IdxInsert` `APPEND` (+`NCHANGE` for the PRIMARY KEY of a WITHOUT ROWID table): placed by key | `insert_index_sorted` (key above the last entry → behind it, as `sqlite3BtreeIndexMoveto` does; else full search) | `xf|ins app=0 bulk=1 pre=0` |

## 3. `sqlite3RunVacuum` (vacuum.c)

| # | statement / step (line) | effect on the new file | Rust (`build_vacuum_image`) | trace |
| :-- | :-- | :-- | :-- | :-- |
| V1 | `SELECT sql FROM main.sqlite_schema WHERE type='table' AND name<>'sqlite_sequence' AND coalesce(rootpage,1)>0` (298) — each `sql` starting with `CRE` is executed | CREATE TABLE as on any database: root, placeholder row, roots + rows of the automatic indexes, final row; `sqlite_sequence` after the first AUTOINCREMENT table | phase (1): `parse_one` + the write sequence of `exec_create_table_started` | `vac|stmt|CRE …`, `alloc`, `ins` |
| V2 | `SELECT sql FROM main.sqlite_schema WHERE type='index'` (305) | CREATE INDEX: root, catalogue row, `sqlite3RefillIndex` on the empty table | phase (2) | `vac|stmt|CRE …`, `xf|open` |
| V3 | `SELECT 'INSERT INTO vacuum_db.'||quote(name)||' SELECT*FROM"main".'||quote(name) FROM vacuum_db.sqlite_schema WHERE type='table' AND coalesce(rootpage,1)>0` (317) | one transfer per table of the NEW catalogue, in its rowid order, under `DBFLAG_Vacuum` (rows T1, K1–K3, I1–I3 above; no `emptyDestTest`, no `OP_NotExists`) | phase (3): `xfer_copy(XferMode::Vacuum)` | `vac|stmt|INS …`, `xfd|go empty=0`, `xf` |
| V4 | `INSERT INTO vacuum_db.sqlite_schema SELECT * FROM main.sqlite_schema WHERE type IN('view','trigger') OR (type='table' AND rootpage=0)` (333) | ordinary inserts with new rowids; no file-format change: when no CREATE TABLE ran, the new schema still has file format 1 and the rootpage 0 is stored as a one-byte integer (vdbe.c:3730, 4567) | phase (4), `record_with_one_byte_small_ints` | `xfd|no|why=5`, `ins` |
| V5 | `aCopy[]` (358–375): schema cookie +1, default cache size, text encoding, user version, application id | `sqlite3BtreeUpdateMeta` | phase (5) | `vac|meta` ×5 |
| V6 | `sqlite3BtreeCopyFile` (379) — not for `VACUUM INTO` | pages copied over the main file | `Pager::copy_pages_from` (unchanged) | `vac|copy` |
| V7 | page size / auto_vacuum of the copy (281–291): main's page size, then `db->nextPagesize`; `db->nextAutovac` or main's | geometry of the new file | `compute_effective_vacuum_page_size`, `set_configured_auto_vacuum` (unchanged) | `vac|copy pgsz= av=` |
| V8 | `btreeCreateTable` in auto-vacuum mode (btree.c:10086–10225): the page at the new root's place is moved away (`relocatePage` → `sqlite3PagerMovepage`), the root is fetched again from the file and only its header is rewritten (`zeroPage`) | the rest of the new root page is the STORED image (zeroes for a page the transaction added) | `Pager::restore_stored_image` in `create_root_page` | bytes |

## 4. `ALTER TABLE` (alter.c)

Every nested `UPDATE sqlite_schema` is a two-pass UPDATE (`sqlite3Update` with `pParse->nested`: no ONEPASS_MULTI): the
rowids the WHERE accepts are collected, then every row is rewritten in rowid order through a cursor that has read it —
`sqlite3BtreeInsert` writes a record of the same size over the old one (`o=overwrite`), any other replaces the cell
(`o=replace-drop`). Rows whose text does not change are written too.

| # | statement (line) | rows written | Rust | trace |
| :-- | :-- | :-- | :-- | :-- |
| A1 | RENAME TO (216): `UPDATE sqlite_schema SET sql = sqlite_rename_table(…) WHERE (type!='index' OR tbl_name=<old> COLLATE nocase) AND name NOT LIKE 'sqlite\_%'` | every such row, new or unchanged `sql`, old names | `update_schema_rows` pass 1 | `ins|cell root=1 …` |
| A2 | RENAME TO (226): `UPDATE sqlite_schema SET tbl_name=<new>, name=CASE … END WHERE tbl_name=<old> COLLATE nocase AND type IN ('table','index','trigger')` | the table, its indexes (automatic ones renamed), its triggers — a second write of those rows | pass 2 | `ins|cell` |
| A3 | RENAME TO (247): `UPDATE sqlite_sequence SET name=<new> WHERE name=<old>` when the table exists | | unchanged | |
| A4 | RENAME TO (257): `UPDATE sqlite_temp_schema …` (not for a temp table); `sqlite_stat1` is NOT touched | temp database (not traced) | temp edits as before; the rewrite of `sqlite_stat1` rows was removed | SQL result |
| A5 | RENAME COLUMN (671–672): `renameTestSchema`, `renameFixQuotes` = `UPDATE sqlite_schema SET sql = sqlite_rename_quotefix(…) WHERE name NOT LIKE 'sqlite\_%' AND sql NOT LIKE 'create virtual%'`, then the same on temp | every such row rewritten (`o=overwrite` when nothing changes) | pass 1 (text unchanged: `sqlite_rename_quotefix` itself is not implemented — see "still different") | `ins|cell … o=overwrite` |
| A6 | RENAME COLUMN (685): `UPDATE sqlite_schema SET sql = sqlite_rename_column(…) WHERE name NOT LIKE 'sqlite\_%' AND (type != 'index' OR tbl_name = <table>)` | every such row; this UPDATE runs AFTER a write cursor was opened on the temp schema, whose file format is 1 until a temp table exists: `Vdbe.minWriteFileFormat` (vdbe.c:4567) drops below 4 and `OP_MakeRecord` (3730) stores integers 0/1 as one-byte integers — rows with rootpage 0 (views, triggers) grow by one byte | pass 2 with `legacy_ints` (`rename_column_writes_legacy_ints`) | `ins|cell … sz=` |
| A7 | RENAME COLUMN to a name another column has | no check up front: the catalogue is rewritten, `renameTestSchema` (704) fails with "error in table T after rename: duplicate column name: X", statement rolled back | the error is raised after the two passes | `ins` events, stderr |
| A8 | DROP COLUMN (2337–2348): `renameFixQuotes`, `UPDATE sqlite_schema SET sql = sqlite_drop_column(…) WHERE (type=='table' AND tbl_name=<table> COLLATE nocase)` | all rows, then the table's row | two `update_schema_rows` passes (was: one row) | `ins|cell` |
| A9 | DROP COLUMN (2350–2416): every row of the table rebuilt by `OP_MakeRecord` and stored at the cursor (`OP_Insert` / `OP_IdxInsert` with `OPFLAG_SAVEPOSITION`); same `minWriteFileFormat` effect | cells replaced in place; integers 0/1 as one-byte integers | `rewrite_physical_rows_for_drop_column` (`legacy_ints`; the WITHOUT ROWID tree is no longer cleared and rebuilt) | `ins|cell`, bytes |
| A10 | ADD COLUMN (442): one `UPDATE … WHERE type='table' AND name=<table>` | the table's row | unchanged | |
| D1 | DROP INDEX (build.c `sqlite3DropIndex`): `DELETE FROM sqlite_schema …`, `sqlite3ClearStatTables`, cookie, `destroyRootPage` | catalogue row first, b-tree last | `exec_drop_index` reordered | `bal|del`, `free` order |


## 4b. Rule found by the fuzz (insert side, all statements): the "next key" shortcut of `sqlite3BtreeTableMoveto`

| # | C condition (line) | outcome in C | Rust | trace |
| :-- | :-- | :-- | :-- | :-- |
| M1 | `sqlite3BtreeTableMoveto` (btree.c:5903–5927): cursor `CURSOR_VALID` with `BTCF_ValidNKey` and `pCur->info.nKey+1==intKey` | the row is reached with `sqlite3BtreeNext` + `getCellInfo` — the cell IS parsed (`info.nSize!=0`), so the `sqlite3BtreeInsert` that follows writes a record of the same size over the old one (`btreeOverwriteCell`). The write cursor is in that state after it replaced a row without `balance()` (btree.c:9615–9665: `ValidNKey` is cleared only on the way into `balance()`); any other cursor writing to the tree (`saveAllCursors`), a rebalance, or the end of the statement (trigger step) ends it | `Btree::in_place_row` (set by the three no-balance exits of `replace_cell`, dropped by `save_cursors_for_mutation`) + `StmtTxTracker::in_place_cursors` (one per sub-program depth, closed at every trigger step / foreign-key action): the "cursor not parsed" cases of H1b/H2 (`replace_unparsed_row` for `INSERT OR REPLACE` into a table without index, `update_unparsed_cursor` for the two-pass UPDATE that reads no old column) no longer apply to a row whose rowid follows the row just replaced in place | `ins|cell … o=overwrite` instead of `o=replace-same` (scripts `rp_*`) |

## 5. Results (final build)

322 scripts, every one started from an empty file by both engines; 6514115 C events, 6514076 Rust events. **321 scripts have identical event sequences**; 320 are equal in trace + SQL output + error messages + integrity (both engines on both files) + dbstat shape; 320 produce byte-identical files (main file and every `VACUUM INTO` / `ATTACH` file).

| family | what | scripts | trace identical | + sql, errors, integrity, shape | byte-identical files | C events |
|---|---|---|---|---|---|---|
| ci | CREATE INDEX: 0/1/many rows, duplicates ± UNIQUE, NULLs, BINARY/NOCASE/RTRIM, DESC, expression, partial, overflow keys, WITHOUT ROWID sources, REAL / generated columns, in transactions, 512…65536, auto_vacuum | 26 | 26 | 26 | 26 | 793120 |
| h3 | H2 leftovers: xfer copy with indexes, CREATE INDEX | 4 | 4 | 4 | 4 | 147688 |
| ri | REINDEX: all / table / index / collation, in transactions, auto_vacuum, 14 tables (hash order) | 8 | 8 | 8 | 8 | 485904 |
| rn | ALTER TABLE RENAME TO / RENAME COLUMN / ADD / DROP COLUMN with views, triggers, foreign keys, automatic indexes, WITHOUT ROWID, in transactions, error cases, auto_vacuum | 10 | 10 | 10 | 10 | 19234 |
| va | VACUUM / VACUUM INTO: every object kind, free pages, 512…65536, auto_vacuum 0/1/2 and changes of it, page-size changes, empty / view-only / sqlite_sequence / sqlite_stat1 / WAL / rowid gaps / virtual tables | 33 | 32 | 32 | 32 | 1508745 |
| xf | transfer optimisation: page sizes, overflow rows, auto_vacuum, non-empty destinations, new rowids, UNIQUE, WITHOUT ROWID, AUTOINCREMENT, STRICT / generated / CHECK, and one script per disabling condition (`xf_c_*`) | 60 | 60 | 59 | 60 | 551596 |
| e | H2: exact-fit sweep and churn | 6 | 6 | 6 | 6 | 42447 |
| g | H2: several trees in turns | 2 | 2 | 2 | 2 | 80828 |
| m | H2: mixed DML | 24 | 24 | 24 | 23 | 429951 |
| o | H2: overwrite / replace / UPSERT / two-pass UPDATE | 21 | 21 | 21 | 21 | 28215 |
| p | H2: payload sizes | 15 | 15 | 15 | 15 | 167762 |
| r | H2: root on page 1 | 6 | 6 | 6 | 6 | 13435 |
| rp | added in H3 (rule M1): INSERT OR REPLACE … SELECT / two-pass UPDATE over consecutive rowids, trigger steps, triggers writing the same or another tree, gaps, DESC order; fuzz seed 3271 | 5 | 5 | 5 | 5 | 32323 |
| s | H2: plain tables | 25 | 25 | 25 | 25 | 525003 |
| t | H2: savepoints / rollbacks | 2 | 2 | 2 | 2 | 64209 |
| v | H2: auto_vacuum 1 / 2 | 18 | 18 | 18 | 18 | 353650 |
| w | H2: WITHOUT ROWID | 15 | 15 | 15 | 15 | 135977 |
| x | H2: index keys | 42 | 42 | 42 | 42 | 1134028 |

First mismatching decision of every script that is not fully equal:

* `m_wr_64k`: trace identical, sql =, shape =, bytes differ, pages C 396/2 Rust 396/2<br>Cause: H2 leftover: unused bytes of one page (which image of a freed page C still has depends on its page cache).
* `va_vtab`: trace differs at event #692, sql =, shape =, bytes differ, pages C 26/0 Rust 26/0<br>C: `ins|cell|root=18 pg=18 idx=0 o=new sz=5`<br>Rust: `alloc|page|pg=20 o=extend`<br>Cause: FTS5 / R-Tree shadow tables are created and filled in a different order than C does (known blocker of the virtual-table families, not part of this step); the divergence is in the setup, before VACUUM.
* `xf_c_vtab`: trace identical, sql differs, shape =, bytes =, pages C 25/0 Rust 25/0<br>Cause: `total_changes()` after `CREATE VIRTUAL TABLE … USING rtree`: C counts the row the module inserts into its node table (1), the engine does not (0) — pre-existing, outside this step; every decision is identical.

## 6. Coverage: times every outcome was exercised (C / Rust)

Columns 3–4: the 141 scripts of the H3 families (`ci`, `ri`, `xf`, `va`, `rn`, `h3`). Columns 5–6: the 181 scripts of the H2 families.

| decision | outcome | H3: C | H3: Rust | H2 set: C | H2 set: Rust |
|---|---|---|---|---|---|
| A1 allocateBtreePage | empty-trunk | 46 | 46 | 2733 | 2733 |
| A1 allocateBtreePage | extend | 36131 | 36131 | 66602 | 66602 |
| A1 allocateBtreePage | leaf | 4299 | 4299 | 3386 | 3386 |
| B1 balance() loop step [del] | nonroot | 862 | 862 | 2409 | 2409 |
| B1 balance() loop step [del] | root-stop | 3213 | 3213 | 655 | 655 |
| B1 balance() loop step [del] | stop | 1574 | 1574 | 2710 | 2710 |
| B1 balance() loop step [ins] | deeper | 1335 | 1335 | 718 | 718 |
| B1 balance() loop step [ins] | nonroot | 42854 | 42854 | 79691 | 79691 |
| B1 balance() loop step [ins] | quick | 9046 | 9046 | 14226 | 14226 |
| B1 balance() loop step [ins] | root-stop | 27805 | 27805 | 34002 | 34002 |
| B1 balance() loop step [ins] | stop | 21826 | 21826 | 55706 | 55706 |
| B2 balance_deeper [ins] | called | 1335 | 1335 | 718 | 718 |
| D1 delete trigger nFree*3>usable*2 | balance | 3314 | 3314 | 2405 | 2405 |
| D1 delete trigger nFree*3>usable*2 | no-balance | 122582 | 122582 | 37378 | 37378 |
| D2 interior cell replaced by predecessor | fits | 1500 | 1500 | 995 | 995 |
| D2 interior cell replaced by predecessor | overflow | 1 | 1 | 9 | 9 |
| D3 second balance at the interior page | called | 1473 | 1473 | 960 | 960 |
| F1 freePage2 | leaf | 6474 | 6474 | 4769 | 4769 |
| F1 freePage2 | new-trunk | 83 | 83 | 2767 | 2767 |
| I1 sqlite3BtreeInsert: existing cell? | new | 1067158 | 1067158 | 812960 | 812960 |
| I1 sqlite3BtreeInsert: existing cell? | overwrite | 741 | 742 **≠** | 12513 | 12513 |
| I1 sqlite3BtreeInsert: existing cell? | replace-drop | 876 | 876 | 26879 | 26879 |
| I1 sqlite3BtreeInsert: existing cell? | replace-same | 42 | 2 **≠** | 148 | 148 |
| I2 insertCell/allocateSpace [del] | defrag | 60 | 60 | 72 | 72 |
| I2 insertCell/allocateSpace [del] | freeblock | 2043 | 2043 | 3336 | 3336 |
| I2 insertCell/allocateSpace [del] | gap | 321 | 321 | 410 | 410 |
| I2 insertCell/allocateSpace [del] | overflow | 1 | 1 | 11 | 11 |
| I2 insertCell/allocateSpace [ins] | defrag | 10775 | 10775 | 20234 | 20234 |
| I2 insertCell/allocateSpace [ins] | freeblock | 168271 | 168271 | 374423 | 374423 |
| I2 insertCell/allocateSpace [ins] | gap | 929843 | 929843 | 547387 | 547387 |
| I2 insertCell/allocateSpace [ins] | overflow | 50176 | 50176 | 93869 | 93869 |
| N1 sibling selection [del] bBulk=0 | nOld=2 parent has <2 cells: all children | 19 | 19 | 15 | 15 |
| N1 sibling selection [del] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 76 | 76 | 81 | 81 |
| N1 sibling selection [del] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 675 | 675 | 2191 | 2191 |
| N1 sibling selection [del] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 92 | 92 | 122 | 122 |
| N1 sibling selection [ins] bBulk=0 | nOld=1 parent has <2 cells: all children | 635 | 635 | 351 | 351 |
| N1 sibling selection [ins] bBulk=0 | nOld=2 parent has <2 cells: all children | 1216 | 1216 | 855 | 855 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 iParentIdx==0 (nxDiv=0) | 7113 | 7113 | 20550 | 20550 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 middle (nxDiv=iParentIdx-1) | 17828 | 17828 | 51816 | 51816 |
| N1 sibling selection [ins] bBulk=0 | nOld=3 right-most child (nxDiv=i-2) | 3046 | 3046 | 5758 | 5758 |
| N1 sibling selection [ins] bBulk=1 | nOld=1 parent has <2 cells: all children | 380 | 380 | 24 | 24 |
| N1 sibling selection [ins] bBulk=1 | nOld=2 bulk: iParentIdx==0 | 3 | 3 | 0 | 0 |
| N1 sibling selection [ins] bBulk=1 | nOld=2 bulk: right-most child (nxDiv=i-1) | 12633 | 12633 | 337 | 337 |
| N2 distribution nOld->k [del] | 2->1 leafData=0 | 3 | 3 | 5 | 5 |
| N2 distribution nOld->k [del] | 2->1 leafData=1 | 1 | 1 | 0 | 0 |
| N2 distribution nOld->k [del] | 2->2 leafData=0 | 12 | 12 | 9 | 9 |
| N2 distribution nOld->k [del] | 2->2 leafData=1 | 3 | 3 | 0 | 0 |
| N2 distribution nOld->k [del] | 2->3 leafData=0 | 0 | 0 | 1 | 1 |
| N2 distribution nOld->k [del] | 3->1 leafData=1 | 34 | 34 | 63 | 63 |
| N2 distribution nOld->k [del] | 3->2 leafData=0 | 272 | 272 | 183 | 183 |
| N2 distribution nOld->k [del] | 3->2 leafData=1 | 437 | 437 | 1667 | 1667 |
| N2 distribution nOld->k [del] | 3->3 leafData=0 | 50 | 50 | 101 | 101 |
| N2 distribution nOld->k [del] | 3->3 leafData=1 | 50 | 50 | 378 | 378 |
| N2 distribution nOld->k [del] | 3->4 leafData=0 | 0 | 0 | 2 | 2 |
| N2 distribution nOld->k [ins] | 1->1 leafData=0 | 0 | 0 | 1 | 1 |
| N2 distribution nOld->k [ins] | 1->1 leafData=1 | 27 | 27 | 10 | 10 |
| N2 distribution nOld->k [ins] | 1->2 leafData=0 | 966 | 966 | 294 | 294 |
| N2 distribution nOld->k [ins] | 1->2 leafData=1 | 22 | 22 | 70 | 70 |
| N2 distribution nOld->k [ins] | 2->1 leafData=0 | 5 | 5 | 12 | 12 |
| N2 distribution nOld->k [ins] | 2->2 leafData=0 | 7792 | 7792 | 626 | 626 |
| N2 distribution nOld->k [ins] | 2->2 leafData=1 | 11 | 11 | 132 | 132 |
| N2 distribution nOld->k [ins] | 2->3 leafData=0 | 6039 | 6039 | 355 | 355 |
| N2 distribution nOld->k [ins] | 2->3 leafData=1 | 5 | 5 | 67 | 67 |
| N2 distribution nOld->k [ins] | 3->2 leafData=0 | 0 | 0 | 23 | 23 |
| N2 distribution nOld->k [ins] | 3->2 leafData=1 | 0 | 0 | 65 | 65 |
| N2 distribution nOld->k [ins] | 3->3 leafData=0 | 22440 | 22440 | 27784 | 27784 |
| N2 distribution nOld->k [ins] | 3->3 leafData=1 | 415 | 415 | 26767 | 26767 |
| N2 distribution nOld->k [ins] | 3->4 leafData=0 | 5012 | 5012 | 8269 | 8269 |
| N2 distribution nOld->k [ins] | 3->4 leafData=1 | 120 | 120 | 14979 | 14979 |
| N2 distribution nOld->k [ins] | 3->5 leafData=1 | 0 | 0 | 237 | 237 |
| N3 pages [del] | allocated=0 freed=0 | 115 | 115 | 488 | 488 |
| N3 pages [del] | allocated=0 freed=1 | 713 | 713 | 1855 | 1855 |
| N3 pages [del] | allocated=0 freed=2 | 34 | 34 | 63 | 63 |
| N3 pages [del] | allocated=1 freed=0 | 0 | 0 | 3 | 3 |
| N3 pages [ins] | allocated=0 freed=0 | 30685 | 30685 | 55320 | 55320 |
| N3 pages [ins] | allocated=0 freed=1 | 5 | 5 | 100 | 100 |
| N3 pages [ins] | allocated=1 freed=0 | 12164 | 12164 | 24034 | 24034 |
| N3 pages [ins] | allocated=2 freed=0 | 0 | 0 | 237 | 237 |
| N4 divider cell [del] (index cell) | fits | 384 | 384 | 400 | 400 |
| N4 divider cell [del] (index cell) | overflow | 0 | 0 | 2 | 2 |
| N4 divider cell [del] (rowid key) | fits | 540 | 540 | 2423 | 2423 |
| N4 divider cell [ins] (index cell) | fits | 80204 | 80204 | 76779 | 76779 |
| N4 divider cell [ins] (index cell) | overflow | 516 | 516 | 3014 | 3014 |
| N4 divider cell [ins] (rowid key) | fits | 1265 | 1265 | 101330 | 101330 |
| N4 divider cell [ins] (rowid key) | overflow | 0 | 0 | 725 | 725 |
| N5 editPage [del] | edit | 1680 | 1680 | 4772 | 4772 |
| N5 editPage [del] | rebuild | 106 | 106 | 462 | 462 |
| N5 editPage [ins] | edit | 115994 | 115994 | 235197 | 235197 |
| N5 editPage [ins] | rebuild | 8845 | 8845 | 26342 | 26342 |
| N6 balance-shallower [del] | no | 858 | 858 | 2404 | 2404 |
| N6 balance-shallower [del] | shallower | 4 | 4 | 5 | 5 |
| N6 balance-shallower [ins] | no | 42824 | 42824 | 79668 | 79668 |
| N6 balance-shallower [ins] | root-empty-no-room | 27 | 27 | 11 | 11 |
| N6 balance-shallower [ins] | shallower | 3 | 3 | 12 | 12 |
| R1 sorter output of sqlite3RefillIndex | run completed (hash compared) | 176 | 176 | 6 | 6 |
| R2 OP_SorterCompare (UNIQUE) | duplicate -> abort | 8 | 8 | 0 | 0 |
| T1 xferOptimization | coded, emptyDestTest=0 | 282 | 282 | 20 | 20 |
| T1 xferOptimization | coded, emptyDestTest=1 | 16 | 16 | 2 | 2 |
| T1 xferOptimization | return 0 #01 | 205 | 205 | 493 | 493 |
| T1 xferOptimization | return 0 #02 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #03 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #04 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #05 | 43 | 43 | 6 | 6 |
| T1 xferOptimization | return 0 #06 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #07 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #08 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #09 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #10 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #11 | 1 | 1 | 1 | 1 |
| T1 xferOptimization | return 0 #12 | 2 | 2 | 0 | 0 |
| T1 xferOptimization | return 0 #13 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #14 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #15 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #16 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #17 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #18 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #19 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #20 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #21 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #22 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #23 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #24 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #25 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #26 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #27 | 2 | 2 | 0 | 0 |
| T1 xferOptimization | return 0 #29 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #30 | 1 | 1 | 0 | 0 |
| T1 xferOptimization | return 0 #31 | 1 | 1 | 0 | 0 |
| V1 VACUUM statement | CRE | 476 | 476 | 24 | 24 |
| V1 VACUUM statement | INS | 248 | 248 | 12 | 12 |
| V2 VACUUM meta copy | idx=1 | 41 | 41 | 2 | 2 |
| V2 VACUUM meta copy | idx=3 | 41 | 41 | 2 | 2 |
| V2 VACUUM meta copy | idx=5 | 41 | 41 | 2 | 2 |
| V2 VACUUM meta copy | idx=6 | 41 | 41 | 2 | 2 |
| V2 VACUUM meta copy | idx=8 | 41 | 41 | 2 | 2 |
| V3 VACUUM copy | into=0 | 30 | 30 | 0 | 0 |
| V3 VACUUM copy | into=1 | 11 | 11 | 2 | 2 |
| X1 OP_OpenWrite OPFLAG_BULKCSR | bulk cursor opened | 957 | 957 | 105 | 105 |
| X2 sqlite3BtreeInsert via bulk / preformat | append(seekResult<0)=0 bulk=0 preformat=1 | 30761 | 30761 | 716 | 716 |
| X2 sqlite3BtreeInsert via bulk / preformat | append(seekResult<0)=0 bulk=1 preformat=0 | 67925 | 67925 | 0 | 0 |
| X2 sqlite3BtreeInsert via bulk / preformat | append(seekResult<0)=1 bulk=0 preformat=1 | 24311 | 24311 | 827 | 827 |
| X2 sqlite3BtreeInsert via bulk / preformat | append(seekResult<0)=1 bulk=1 preformat=0 | 263825 | 263825 | 1198 | 1198 |
| X2 sqlite3BtreeInsert via bulk / preformat | append(seekResult<0)=1 bulk=1 preformat=1 | 76101 | 76101 | 1855 | 1855 |
| X3 sqlite3BtreeTransferRow | local | 129309 | 129309 | 3353 | 3353 |
| X3 sqlite3BtreeTransferRow | overflow | 1864 | 1864 | 45 | 45 |

`balance_nonroot(bBulk=1)` through SQL: C 13377 sibling selections, Rust 13377 (step H2: C 992, Rust 0). Outcomes whose count differs between C and Rust: **2**: `I1 sqlite3BtreeInsert: existing cell?|overwrite` (C 13254, Rust 13255), `I1 sqlite3BtreeInsert: existing cell?|replace-same` (C 190, Rust 150).


## 7. Coverage gaps — outcomes that are "not verified"

* **`xferOptimization` return #28** (compatible index is the same b-tree: corrupt schema) and **#32/#33** (authorizer): not reachable from a script
  on a valid database without an authorizer callback. Never exercised in either engine.
* **`xferOptimization` return #2** (virtual-table destination) is exercised (`xf_c_vtab`), but that script's SQL output differs for an unrelated,
  older reason (`total_changes()` after `CREATE VIRTUAL TABLE … USING rtree`).
* **`UNIQUE` constraint of the table (`sqlite_autoindex`) vs `CREATE UNIQUE INDEX`**: C distinguishes `OE_Default` from `OE_Abort` in
  `xferCompatibleIndex` (insert.c:2977); the schema model has one "abort" value for both, so a table with `UNIQUE(a)` and one with
  `CREATE UNIQUE INDEX … (a)` are treated as transfer-compatible where C says no (#27). Not exercised.
* **DEFAULT comparison (#26)** uses the source text when both sides have one; expressions that differ only in a way the stored text
  normalises away were not examined. **Expression / partial-index comparison** in `xferCompatibleIndex` is AST equality, C's is
  `sqlite3ExprCompare`.
* **Sorter spill**: C's sorter writes sorted runs to temp files (PMAs) and merges them when the keys exceed the cache size; the engine sorts
  in memory. The output ORDER is the same by construction (stable order on the key columns) and is compared by hash on every run, including
  builds over 223,000 rows (two-level merges in C); the spill decisions themselves are not traced.
* **The build phase of a plain `VACUUM`** runs in a temporary database, which neither engine traces: it is compared event by event only through
  `VACUUM INTO` (same code path, `DBFLAG_VacuumInto` differs in K2 only) and by the bytes of the vacuumed file.
* **`sqlite3BtreeCopyFile`** (V6) was not changed and has no events of its own; verified by bytes only.
* **`sqlite_rename_quotefix`** (A5) is not implemented: the pass exists (every row is rewritten) but double-quoted string literals in stored
  SQL are not turned into single-quoted ones. See "still different".
* **Temp-schema statements of ALTER TABLE** (A4 and the temp halves of A5/A6): the temp database is not traced and not compared by bytes; only
  their effect on `Vdbe.minWriteFileFormat` (A6, A9) is modelled, from the temp file's header (file format < 4 ⇔ no table was ever created in it).
* **ALTER TABLE on virtual tables** (`OP_VRename`) and **`ALTER TABLE … ADD/DROP CONSTRAINT`, `SET NOT NULL`** (alter.c:2831–3100): not examined.
* **Page-cache dependent bytes**: a page that is freed while dirty (its write is dropped, `sqlite3PagerDontWrite`) and allocated again in the
  same transaction keeps its cached image in the engine; C has that image only as long as its page cache (2000 KiB by default) has not evicted
  the page, afterwards it reads the older image from the file. Seen once: `REINDEX` of a 223,000-key index in an 80 MB file — 1 byte of 1 page
  (an unused gap byte) differs, every decision (744,578 events) identical. Needs a model of C's pcache; not attempted.
* **`REINDEX` without argument / with a collation, table order**: the order is that of C's table hash for a freshly loaded schema
  (`c_table_hash_order`); a connection that created and dropped tables since it loaded the schema has a different hash history in C.
* **The two outcome counts that differ in section 6** (`I1 … overwrite`, `I1 … replace-same`) come from one script, `va_vtab`: C's FTS5 and R-Tree
  modules write their shadow tables through SQL statements (REPLACE of same-size rows → `replace-same`), the engine's modules write them
  differently. Virtual-table modules are outside this step; every other script has identical counts.
* **Bytes of free pages** (fuzz, 95 of 1124 rowid-table scripts): in every script examined the decision trace is identical to C's and the
  differing bytes lie on freelist pages only (no page of any b-tree). Two sources: (1) C's page cache spilling during a transaction — a page taken
  from the freelist is fetched with `PAGER_GET_NOCONTENT`, hence not journalled, so a `ROLLBACK` leaves whatever a cache spill wrote there (two
  of five scripts become identical to the engine's file when C runs with `PRAGMA cache_size=200000`); (2) the content of pages freed and
  reused across `ROLLBACK TO` / `PRAGMA incremental_vacuum` (first differing statement `COMMIT` or the pragma; older class, not part of this step).

