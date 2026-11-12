# Decision parity: sqliters (Rust) vs C SQLite 3.54.1 — step 5g

How this was measured. `ctrace/patch.py` instruments a scratch copy of the amalgamation (`ctrace/sqlite3_tr`, never the
tree under `sqlite/` or `target/sqlite-ref/`) with `TR|class|event|key=value` lines on stderr (enabled by `DTRACE=1`). The
Rust engine has the matching points as `sqliters_types::dtrace!` — compiled in only with `RUSTFLAGS="--cfg sqliters_dtrace"`
(`tracebuild.sh`), absent otherwise. `trdiff.py` runs a script through both, normalises the two logs per class and diffs
them event by event; on a mismatch it bisects the script to the first statement. `trall.sh` does that for every scenario
(192 of step 5c + the 42 added here in `step5g/sc/`) and sums, per decision outcome, how often it was exercised in C and
in Rust. Numbers below are from the final source (`final/tr_*.txt`). "C / Rust" = times the outcome was taken.
A row whose outcome count is 0 is **not verified**, whatever the code says.

Scripts with identical decision logs, per class (234 scripts):

| class | scripts identical / differing | events C | events Rust |
|---|---|---|---|
| cookie | 234 / 0 | 861 | 861 |
| schema | 227 / 7 | 1615 | 1459 |
| stat | 233 / 1 | 50 | 49 |
| real | 234 / 0 | 88 | 88 |
| jsonb | 234 / 0 | 39 | 39 |
| free | 181 / 53 | 3293 | 62771 |
| alloc | 191 / 43 | 19764 | 50328 |
| w | 170 / 64 | 1220296 | 1072541 |
| open / journal / ckpt | 233 / 1 | 1308 | 1354 |

---------------------------------------------------------------------------------------------------------------------

## 1. H1 — file change counter (bytes 24..27, 92..95)

C source: `newDatabase` (btree.c), `pager_write_changecounter` / `pager_incr_changecounter` / `sqlite3PagerCommitPhaseOne`
/ `pagerWalFrames` / `readDbPage` / `pager_playback_one_page` / `pager_write_pagelist` (pager.c).

| # | decision point | C rule | Rust (crates/sqliters-pager/src/lib.rs) |
|---|---|---|---|
| 1 | new page 1 | bytes 24..99 zero: counter 0, version-valid-for 0 | `initialize_empty_database`: 0 / 0, `db_file_vers_cc = 0` |
| 2 | value stamped by a commit | `dbFileVers + 1` (u32 wrap, 0 allowed); also into 92..95; 96..99 = version number | commit paths: `db_file_vers_cc.wrapping_add(1)` |
| 3 | rollback-journal commit | once per transaction that modified the cache (`changeCountDone`); after page 1 reaches the file, `dbFileVers` := page-1 bytes | commit finalize: `db_file_vers_cc = header.change_counter` |
| 4 | WAL commit | page 1 is stamped only when it is in the dirty list; `dbFileVers` is NOT advanced → same value on every later commit while page 1 stays cached | WAL branch: stamp without advancing `db_file_vers_cc` |
| 5 | when `dbFileVers` is re-read | SHARED lock taken in rollback mode (always); page 1 (re)read in WAL mode (cache reset); rollback that restores page 1; savepoint/statement rollback that replays page 1 | header load from disk; WAL header adoption when the cache is invalidated; `rollback` when `tx_preimages` holds page 1; `apply_subsavepoint_undo` when the slot saved page 1 |
| 6 | no-op write transaction (`BEGIN IMMEDIATE; COMMIT`) | no bump | no bump |

Evidence: there is no trace class for H1 (the value is in the file): every scenario that has no other difference is
byte-identical (L0) — 54 of 234 scenarios; `g_h1_basic/modes/wal` reach L0 for every statement before the first statement
of another class. Row 4 and row 5 (rollback / savepoint with page 1) are exercised by `g_h1_wal` (L0 through
`BEGIN; CREATE TABLE; ROLLBACK`, `ROLLBACK TO`, checkpoint, VACUUM in WAL, mode switches).
Not verified: counter wrap at 2^32; page 1 evicted from C's page cache between transactions (database larger than the
cache: C then re-reads page 1 and its WAL stamp advances — the Rust cache keeps the header, so the stamp can differ in
long WAL sessions on databases > `cache_size`); multi-connection WAL sessions.

## 2. H3 — schema cookie (bytes 40..43, `PRAGMA schema_version`)

C source: `sqlite3ChangeCookie` (build.c:2114) codes `OP_SetCookie iDb, BTREE_SCHEMA_VERSION, pSchema->schema_cookie + 1`
— the cookie *as of prepare time* + 1; callers: `sqlite3EndTable` (2996), `sqlite3CreateIndex` (4556, only for an explicit
CREATE INDEX), `sqlite3DropIndex` (4735), `sqlite3CodeDropTable` (3532), `sqlite3FinishTrigger`, `sqlite3DropTriggerPtr`,
`sqlite3VtabFinishParse` (vtab.c:501), `renameReloadSchema` (alter.c:114); `sqlite3RunVacuum` copies meta + 1
(vacuum.c:359); `PRAGMA schema_version=N` (pragma.c:2336). `sqlite3RootPageMoved` does not change it.

| # | decision | C | Rust |
|---|---|---|---|
| 1 | value written by a DDL statement | (cookie when the statement started) + 1, however many catalogue rows it writes (UNIQUE / PRIMARY KEY auto-indexes, `sqlite_sequence`, `sqlite_stat1`, triggers dropped with a table, root-page moves) | `Btree::change_schema_cookie` with `CookieStmt::Base(start)`; scope opened in `engine::execute_write_body`, base captured in `ensure_write_db` |
| 2 | statement nested in another (`xCreate` / `xDestroy` running `CREATE` / `DROP TABLE`) | prepared when it runs → its own base = current cookie | nested scope (`DatabaseSet::cookie_scope_enter/exit` save and restore the outer base) |
| 3 | CREATE VIRTUAL TABLE | own `OP_SetCookie` before `OP_VCreate`: c+1, then +1 per shadow-table statement | own row + cookie first, then `module.create` |
| 4 | DROP TABLE of a virtual table | shadow drops set c+1..c+n, then the statement's own `OP_SetCookie` sets **c+1** (goes backwards) | own row deleted without cookie change, `destroy`, then `bump` → base+1 |
| 5 | VACUUM | old + 1 | target b-tree pinned to `Base(old)`; header `old+1` |
| 6 | `PRAGMA schema_version=N` | N | N |
| 7 | IF NOT EXISTS / IF EXISTS no-op, REINDEX, second ANALYZE | no change | no change |

Trace class `cookie` (one event per change of the value; C codes one `OP_SetCookie` per `sqlite3ChangeCookie` call, Rust
calls the setter per catalogue row, so repeated sets of the same value are folded):

`scripts 234  identical decision logs 234  differing 0  no-result []  events C 861 Rust 861`

| decision outcome | C | Rust |
|---|---|---|
| `cookie|set|new<old (outer statement after nested ones)` | 6 | 6 |
| `cookie|set|new=old+1` | 827 | 827 |
| `cookie|set|other` | 2 | 2 |
| `cookie|vacuum|` | 26 | 26 |

Not verified: ATTACHed *file* databases with their own DDL (only `:memory:` attach traced), cookie after a failed DDL
statement inside a transaction (covered by finder L0 only for `SAVEPOINT / ROLLBACK TO`).

## 3. D2 / D3 — row order in sqlite_schema and sqlite_stat1 (+ the catalogue write sequence)

C source: `sqlite3StartTable` (placeholder row `OP_Blob 6 nullRow`, build.c), `sqlite3EndTable` (`UPDATE sqlite_schema ...
WHERE rowid=#N`), `sqlite3VtabFinishParse`, `sqlite3CodeDropTable`, `sqlite3RunVacuum`, `analyzeDatabase` /
`analyzeOneTable` (analyze.c), `hash.c` (`sqliteHashFirst` order), `rtreeDestroy`, `sqlite3Fts5DropAll`.

| # | decision | C | Rust |
|---|---|---|---|
| 1 | CREATE TABLE / VIEW / VIRTUAL TABLE / `sqlite_sequence` / `sqlite_stat1` row | placeholder row (5 NULLs) at CREATE start, auto-index rows after it, then the placeholder is rewritten with the real row | `schema::insert_schema_placeholder` + `update_schema_entry` (`exec_create_table`), `insert_schema_entry_over_placeholder` |
| 2 | virtual table vs shadow tables | own row first, then `xCreate` | same (`exec_create_virtual_table`) |
| 3 | DROP TABLE | `sqlite_stat1` rows; triggers; `sqlite_sequence` row; catalogue rows of the table and its indexes in rowid order; then the trees, largest root first; vtab: own row first, then `xDestroy` (rtree: node, rowid, parent; fts5: data, idx, config, docsize, content) | `exec_drop_table` reordered; rtree / fts5 `destroy` orders |
| 4 | VACUUM catalogue | per table in rowid order: table row, its auto-index rows, `sqlite_sequence` right after the first AUTOINCREMENT table; then explicit indexes in rowid order; then views / triggers / vtabs | `build_vacuum_image` |
| 5 | ANALYZE: tables | iteration order of the schema's table hash (`hash.c`): front insertion below 5 entries, bucket-adjacent insertion and full relink on every rehash (count ≥ 5 and > 2×buckets → count×3 buckets, cap 64); filled with `sqlite_master`, then tables / views / vtabs in rowid order | `schema::c_table_hash_order` / `c_hash_iteration_order` (port) |
| 6 | ANALYZE: rows of one table | `pTab->pIndex` order (newest index first, REPLACE ones last, WITHOUT ROWID PRIMARY KEY at its list position, named after the table), then the table-count row when no non-partial index exists | `analyze_one_table` via `ordered_table_indexes` |

Trace classes `schema` (every row written to / deleted from the catalogue b-tree, by rowid) and `stat` (index visited /
table-count row, at code generation):

`scripts 234  identical decision logs 227  differing 7  no-result []  events C 1615 Rust 1459`

| decision outcome | C | Rust |
|---|---|---|
| `schema|del|` | 124 | 124 |
| `schema|ins|` | 1491 | 1335 |

First mismatching decision per differing script (7; event index, statement):
- family `f` (2): f06_alter_drop_column @#4 [504, 'ALTER TABLE t DROP COLUMN b;']; f07_alter_rename_column @#15 [307, 'ALTER TABLE t RENAME COLUMN a TO alpha;']
- family `g` (3): g_h3_cookie @#24 [29, 'ALTER TABLE a RENAME TO a2;']; g_v1_c24 @#4 [4, 'ALTER TABLE a DROP COLUMN x;']; g_v1_c25 @#5 [5, 'ALTER TABLE a DROP COLUMN x;']
- family `t` (2): t04_alter_rename_only @#3 [3, 'ALTER TABLE t RENAME COLUMN a TO alpha;']; t05_schema_cookie @#25 [13, 'ALTER TABLE a RENAME TO a2;']
`scripts 234  identical decision logs 233  differing 1  no-result []  events C 50 Rust 49`

| decision outcome | C | Rust |
|---|---|---|
| `stat|idx|` | 40 | 39 |
| `stat|tblcnt|` | 10 | 10 |

First mismatching decision per differing script (1; event index, statement):
- family `l` (1): l03_pragma_optimize @#1 [1006, 'PRAGMA optimize;']

Remaining `schema` differences (5 scripts, all ALTER TABLE): C's `UPDATE sqlite_schema SET sql=...` rewrites every row its
WHERE clause matches, also rows whose text does not change; Rust rewrites only rows that change (same final rows).
`stat`: the one differing script is `PRAGMA optimize` (class D4, not implemented, out of scope).
Not verified / known gap: row 5 reproduces a freshly loaded schema. A C connection that DROPped tables since its schema
was last loaded keeps the remaining tables in its hash in their old relative positions with different rehash points;
with ≥ 5 tables that order can differ from a fresh load. No scenario exercises it. The 64-bucket cap (≥ 22 tables) is
exercised by no scenario either.

## 4. V1 — REAL-affinity column holding an integral value; V3 — JSONB element type

C source: `OP_MakeRecord` (vdbe.c:3589: affinity string `P4`; `MEM_Int` under REAL affinity → `MEM_IntReal`; serial-type
choice; an `MEM_IntReal` needing 8 bytes is written as a float), `applyAffinity` / `sqlite3VdbeIntegerAffinity`,
`OP_Affinity` (vdbe.c:3536: REAL + integer → genuine real), `OP_TypeCheck` (STRICT), `OP_RealAffinity` (read side),
`sqlite3GenerateConstraintChecks` (insert.c: `bAffinityDone`, lines 2075 / 2424 / 2732), update.c (`aRegIdx`),
`sqlite3GenerateIndexKey` (delete.c:1011, drops `OP_RealAffinity`), `sqlite3AlterDropColumn`, xfer optimisation.

| # | decision | C | Rust |
|---|---|---|---|
| 1 | which opcode applies the table affinities | `OP_Affinity` (before the index / CHECK code) iff `bAffinityDone`; else the affinity string rides on the table's `OP_MakeRecord` | `engine::real_cols_stored_as_int_on_insert` / `_on_update` |
| 2 | INSERT: `bAffinityDone` | table has a CHECK constraint, or any index (auto or explicit, also partial) | same |
| 3 | UPDATE: `bAffinityDone` | a CHECK mentioning an assigned column, or an index with `aRegIdx != 0`: rowid changes, `hasFK > 1`, an indexed column / expression is assigned (generated columns that depend on assigned ones count), partial-index WHERE mentions an assigned column | `update_rewrites_index`, `modified_cols_with_generated` (`hasFK > 1` is not modelled) |
| 4 | stored form with the affinity on `OP_MakeRecord` ("int form") | a real that is an exact integer becomes an integer (`r == (double)(i64)r`, not ±2^63); stored with the smallest integer serial type if it fits 48 bits (`~i` for negatives → [-2^47, 2^47-1]); larger → 8-byte float. `-0.0` → integer 0 | `types::real_affinity_storage_int`; `encode_table_physical_record(.., real_as_int)`, VDBE `MakeRecordAff`, `SingleRowInsertPlan.real_as_int`, `RowidPointUpdatePlan.real_cols` |
| 5 | stored form after `OP_Affinity` ("float form") | 8-byte float in the table row and in index entries | unchanged path |
| 6 | STRICT table | `OP_TypeCheck`: always int form, also in index entries | `tbl.strict` → int form; `strict_real_index_keys_as_int` |
| 7 | WITHOUT ROWID (not STRICT) | PRIMARY KEY record is built in the index loop → float form | float form |
| 8 | CREATE INDEX / REINDEX | key copied in the form stored in the table (no `OP_RealAffinity`) | `populate_index_btree` keeps the stored form |
| 9 | transfer optimisation (`INSERT INTO t SELECT * FROM s`) | records copied byte for byte | per source row form (`stored_real_form_is_int`), index entries are regenerated (gap) |
| 10 | ALTER TABLE DROP COLUMN | columns read with NUMERIC affinity: stored form kept, integral default → integer | `rewrite_physical_rows_for_drop_column` |
| 11 | read | `OP_RealAffinity` after every read of a REAL-affinity table column (table or covering index) | VDBE `RealAffinity` opcode; `fast_read` `real_cols` / `idx_col_is_real`, integer kernel off for such indexes; general path as before |

Trace class `real` (per table: the sequence of forms chosen, runs folded — C decides per compiled statement, the Rust
general path per row):

`scripts 234  identical decision logs 234  differing 0  no-result []  events C 88 Rust 88`

| decision outcome | C | Rust |
|---|---|---|
| `real|form|float-form` | 30 | 30 |
| `real|form|int-form` | 58 | 58 |

Further evidence: `v1probe.sh` — 32 statement sequences (`v1cases.txt`), serial types of every record of every tree
compared with C: 0 differ. `v1/run.sh` — 79 query shapes on a C-written and on a Rust-written database: only the CLI's
error-message prefix differs (3 lines, class S12). Before this step 62 of 129 lines differed on the C-written file
(integers returned for REAL columns).
Not verified / gaps: `hasFK > 1` (parent key with ON UPDATE action / self-reference) — C then also rewrites every index;
the transfer optimisation with indexes whose entries were built by CREATE INDEX in int form (C copies them raw); VACUUM
of such an index (C copies the index b-tree, Rust rebuilds it from the table rows); mixed rows after ALTER TABLE.

V3 — C source: `jsonFunctionArgToBlob` (json.c:3606: SQL text that is not JSON → `JSONB_TEXTRAW`), `jsonLookupStep`
(json.c:3260: label created by an edit → `TEXTRAW`, or `TEXT5` with the escaped spelling when the path quotes the key and
it contains a backslash). Functions that build JSON text first (`jsonb_array`, `jsonb_object`, aggregates) let the parser
choose. Rust: `json::append_edit_arg_as_jsonb`, `encode_new_label_jsonb`.

`scripts 234  identical decision logs 234  differing 0  no-result []  events C 39 Rust 39`

| decision outcome | C | Rust |
|---|---|---|
| `jsonb|argtext|TEXTRAW` | 18 | 18 |
| `jsonb|label|TEXTRAW` | 21 | 21 |

Not verified and not aligned: the `TEXT5` label (quoted path key containing a backslash) — 0 times exercised in C by the
scenarios; Rust writes `TEXTRAW` with the unescaped key there.

## 5. U4 — freed pages

C source: `freePage2` (btree.c), `sqlite3PagerDontWrite` (pager.c:65304 in the amalgamation), `clearDatabasePage`,
`clearCellOverflow`, `btreeDropTable`, `sqlite3BtreeClearTable`, `zeroPage`, `btreeSetHasContent`.

| # | decision | C | Rust (crates/sqliters-btree/src/lib.rs) |
|---|---|---|---|
| 1 | freed page becomes | a leaf of the first trunk when the free list is not empty and that trunk holds `< usableSize/4 - 8` leaves; else the new first trunk (8-byte header rewritten, rest untouched) | `free_page` (same threshold) |
| 2 | content of the freed page | zeroed only with `BTS_SECURE_DELETE` (`secure_delete=ON`); `FAST` (`BTS_OVERWRITE`) does not zero freed pages | `secure_delete == 1` only |
| 3 | leaf: journal / write | not journalled, not written; a write the transaction already owed for it is dropped (`sqlite3PagerDontWrite`) when the page is dirty, the pager is not a temp file and no savepoint is open (in WAL mode the frame is still written) | `Pager::dont_write` (rollback-journal file database, no sub-savepoint) |
| 4 | order of frees in a tree | per cell: child subtree (child page last), then the cell's overflow chain; right-most subtree; the page itself | `clear_database_page` |
| 5 | overflow chain | `nOvfl` pages from the payload size; the last page is freed without being read | `free_overflow_chain_n` |
| 6 | DROP TABLE root | cleared to an empty leaf first (`zeroPage`: 8 header bytes, everything else kept unless a secure mode), then freed (non-auto-vacuum) or overwritten by the last root | `drop_tree_relocating` |
| 7 | `DELETE FROM t` (truncate) | `OP_Clear` per tree, table first, then indexes in `pIndex` order; row count from the leaves during the walk | `clear_tree_counting`, engine truncate branch |

Trace class `free`:

`scripts 234  identical decision logs 181  differing 53  no-result []  events C 3293 Rust 62771`

| decision outcome | C | Rust |
|---|---|---|
| `free|dontwrite|write-dropped` | 422 | 30039 |
| `free|page|leaf` | 1891 | 32158 |
| `free|page|new-trunk` | 980 | 574 |

First mismatching decision per differing script (53; event index, statement):
- family `c` (8): c01_delete_every_3rd @#0 [2001, 'DELETE FROM t WHERE id=1492;']; c02_delete_range @#0 [1504, 'DELETE FROM t WHERE id BETWEEN 200 AND 900;']; c04_delete_all_where @#0 [1504, 'DELETE FROM t WHERE a >= 0;'] …
- family `e` (1): e05_overflow_delete_reuse @#7 [208, 'DELETE FROM t WHERE id=17;']
- family `f` (2): f11_reindex @#0 [1506, 'REINDEX;']; f14_drop_table_autovac_off_then_create @#4 [1505, 'DROP TABLE t;']
- family `g` (6): g01_vacuum_after_churn @#0 [1505, 'DELETE FROM t WHERE id % 3 = 0;']; g03_vacuum_page_size @#0 [1505, 'DELETE FROM t WHERE id % 3 = 0;']; g04_vacuum_to_autovac @#0 [1505, 'DELETE FROM t WHERE id % 3 = 0;'] …
- family `h` (9): h02_autovac_full_delete @#0 [1506, 'DELETE FROM t WHERE id % 3 = 0;']; h03_autovac_incr_delete @#0 [1506, 'DELETE FROM t WHERE id % 3 = 0;']; h04_autovac_incr_all @#0 [1506, 'DELETE FROM t WHERE id > 300;'] …
- family `i` (3): i_512_overflow @#10 [63, 'DELETE FROM t WHERE id % 4 = 0;']; i_512_random_indexed @#0 [25, "INSERT INTO t VALUES(1075,35984,'gyamhdufe8b0ksy');"]; i_65536_overflow @#1 [23, 'DELETE FROM t WHERE id IN (3,4,9);']
- family `k` (1): k02_nested_savepoints @#0 [806, 'DELETE FROM t WHERE id < 300;']
- family `l` (1): l02_analyze_twice_and_table @#0 [1009, 'DELETE FROM t WHERE id > 500;']
- family `n` (2): n05_rtree_delete @#0 [824, 'DELETE FROM b WHERE id=441;']; n08_rtree_drop @#3 [102, 'DROP TABLE b;']
- family `o` (5): o03_fts5_autocommit @#0 [17, "INSERT INTO f VALUES('fawlwv caiacbhs pgnjzgozr','ihvykm qns ewo; o05_fts5_delete_update @#0 [418, 'DELETE FROM f WHERE rowid=99;']; o06_fts5_optimize_merge @#0 [17, "INSERT INTO f VALUES('ofmytmv prkga yersiz imzjamep hmlmwc','boy …
- family `q` (1): q04_secure_delete @#3 [806, 'DELETE FROM t WHERE id > 300;']
- family `s` (7): s06_karisik_4096 @#0 [144, 'DELETE FROM t WHERE id=89;']; s07_karisik_512 @#0 [75, "UPDATE t SET a=837, b='uylwuoxi9xqpdcg zdn515ktfjoki2zfc24mnxac6; s08_karisik_1024_autovacuum @#1 [152, "UPDATE t SET a=3525, b='pmdnzhr4hw9tfxkok6rgdl33r5qi250mou3bcg7 …
- family `t` (2): t05_schema_cookie @#9 [19, 'DROP TABLE r;']; t06_stat1_order @#0 [13, 'DROP INDEX a2;']
- family `u` (3): u01_freelist_reuse_order @#0 [42, 'DELETE FROM t WHERE id BETWEEN 5 AND 25;']; u02_freelist_trunk_overflow @#0 [403, 'DELETE FROM t WHERE id > 20;']; u04_delete_all_truncate_freelist @#22 [1505, 'DELETE FROM t;']
- family `v` (2): v01_vacuum_after_deletes @#0 [1504, 'DELETE FROM t WHERE id % 3 = 0;']; v07_vacuum_overflow @#4 [104, 'DELETE FROM t WHERE id % 4 = 0;']

The class shares its log with B8: scripts whose trees have the same shape in both engines match event for event
(e.g. `f04_drop_table`: 37 events); the others differ because different pages exist to be freed (B1–B6), see section 6.
`secure_delete` differential (`step5e/sd/run_sd.py`, 34 scenarios × OFF/ON/FAST, `final/sd.txt`): DELME markers left: OFF new/C 167738 170895 | ON new/C 99 224 | FAST new/C 151119 144066
Not done: the allocation side of C's rule (`PAGER_GET_NOCONTENT`: a reused free leaf is neither read nor journalled).
The Rust engine still reads, journals and zero-fills a page it takes from the free list. Not verified: row 3 with an
open savepoint in C terms (`nSavepoint` counts statement journals; the Rust proxy is its sub-savepoint stack).

## 6. B7 / B8 / B13 — free-list order; B9 — order of b-tree writes per row

C source: `allocateBtreePage` (btree.c), callers `btreeCreateTable` (nearby 1), `fillInCell` (previous overflow page;
+1 skipping pointer-map / pending-byte pages in auto-vacuum), `balance_quick` (0), `balance_deeper` (root),
`balance_nonroot` (previous sibling; 1 when bulk loading), `incrVacuumStep` (EXACT / LE); `sqlite3CompleteInsertion`
(insert.c), `sqlite3GenerateRowIndexDelete` / `sqlite3GenerateRowDelete` (delete.c), update.c, `btreeOverwriteCell`.

| # | decision | C | Rust |
|---|---|---|---|
| 1 | free list empty | extend the file (skip a pointer-map page and the pending-byte page) | same |
| 2 | first trunk has no leaf, list not searched | the trunk itself is taken | same (`empty-trunk`) |
| 3 | EXACT with `nearby` on the free list, or LE | whole list searched; a matching trunk is taken (its first leaf becomes the trunk) | same (`searched-trunk`) |
| 4 | leaf choice, list not searched | `nearby == 0`: leaf 0; else the leaf with the smallest `|leaf - nearby|` (first of equals) | `AllocMode::Any` = leaf 0 (was: last leaf), `AllocMode::Near` |
| 5 | removal from the trunk | last leaf moved into the hole; vacated slot left as is | same (was: zeroed) |
| 6 | `nearby` per caller | see sources above | new root: 1; overflow chain: previous page (+1 rule in auto-vacuum); root split child: root; append split: 0; other new siblings: previous sibling; index bulk load: 1 |
| 7 | INSERT write order | index entries in `pTab->pIndex` order (PRIMARY KEY b-tree of a WITHOUT ROWID table at its list position), table row last | general path, second insert path, compiled program, `SingleRowInsertPlan` (rowid probe first, `probe_table_insert_leaf` / `insert_table_at_leaf`) |
| 8 | UPDATE write order | delete the entries of the rewritten indexes (list order); delete the row only if the rowid changes or `hasFK > 1`; insert the new entries (list order); write the row (overwrite in place) | general path (`update_index_rewrite` mask, in-place row), compiled program, `RowidPointUpdatePlan` |
| 9 | UPDATE of a row with overflow, same record size | `btreeOverwriteCell`: local part and overflow pages overwritten in place, unchanged pages not dirtied | `try_overwrite_spilled_table_row` |
| 10 | DELETE write order | index entries in list order, then the row; in one-pass mode the index driving the scan is deleted after the row | indexes (list order) then row; the one-pass exception is NOT modelled |
| 11 | order of UNIQUE checks | `pTab->pIndex` order | same list (was: PRIMARY KEY / UNIQUE first) — changes which constraint is reported |

Trace classes `alloc` (every allocation: outcome, leaf index, leaf count, trunk, hint) and `w` (every b-tree insert /
delete: root page and rowid or record length):

`scripts 234  identical decision logs 191  differing 43  no-result []  events C 19764 Rust 50328`

| decision outcome | C | Rust |
|---|---|---|
| `alloc|page|empty-trunk` | 915 | 522 |
| `alloc|page|extend` | 17732 | 18355 |
| `alloc|page|leaf` | 1109 | 31441 |
| `alloc|page|searched-trunk` | 8 | 10 |

First mismatching decision per differing script (43; event index, statement):
- family `c` (2): c05_delete_reinsert @#19 [1581, "INSERT INTO t VALUES(2075,61645,'ma2m gasub7lv439zeoqtt6w5bet2; c11_delete_range_indexed @#25 [1506, 'DELETE FROM t WHERE a < 30000;']
- family `d` (1): d02_upsert_update @#14 [1125, "INSERT INTO t(id,a,b) VALUES(1170,384,'2kr0rmmmst3ok5nz2ye0of 
- family `e` (1): e05_overflow_delete_reuse @#339 [254, "INSERT INTO t VALUES(300,'584abjjpy58lf275ijjsr1en951vjfspnb7fy
- family `f` (4): f02_create_index_large @#84 [3004, 'CREATE INDEX ia ON t(a);']; f11_reindex @#31 [1504, 'CREATE INDEX ia ON t(a);']; f12_expr_partial_desc_index @#42 [1504, 'CREATE INDEX ie ON t(a+1, lower(b));'] …
- family `h` (8): h02_autovac_full_delete @#25 [1506, 'DELETE FROM t WHERE id % 3 = 0;']; h03_autovac_incr_delete @#25 [1508, 'PRAGMA incremental_vacuum(5);']; h04_autovac_incr_all @#25 [1507, 'PRAGMA incremental_vacuum;'] …
- family `i` (3): i_1024_random_indexed @#205 [355, "INSERT INTO t VALUES(1437,84510,'yu99al67g7u6ytf9miai 2d2yzro8e; i_512_random_indexed @#7 [21, "INSERT INTO t VALUES(1394,94744,'i1wvvtl6on64fg8e0rj5ozl1xujxbho; i_512_seq @#299 [985, "INSERT INTO t VALUES(982,18797,'pgrnd kp99v2n vcc9kycqclzkzh8md
- family `k` (1): k02_nested_savepoints @#11 [808, 'UPDATE t SET b = b || b;']
- family `l` (1): l01_analyze @#74 [3004, 'CREATE INDEX ia ON t(a);']
- family `o` (6): o03_fts5_autocommit @#7 [17, "INSERT INTO f VALUES('fawlwv caiacbhs pgnjzgozr','ihvykm qns ewo; o04_fts5_txn @#120 [1137, "INSERT INTO f VALUES('psngbcpht zngzke akc hskbg','ig jm mpfju; o05_fts5_delete_update @#38 [25, "INSERT INTO f(rowid,title,body) VALUES(23,'nnjdjfyeo tyjxkfg ahz …
- family `r` (3): r01_seq_depth2_4096 @#1161 [36788, "INSERT INTO t VALUES(36786,51596,'v9 jryxtans ygqsrk9yef6iiwf; r02_seq_depth2_1024 @#381 [4078, "INSERT INTO t VALUES(4075,77834,'ar0ovwckdhcye4');"]; r03_seq_depth2_insert_select @#2048 [2, 'WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHER
- family `s` (9): s03_rastgele_ekleme @#258 [1833, "INSERT INTO t VALUES(1556,37781,'nibhl92d  tz7avp15wr2q9 hwhu3; s05_sonradan_indeks @#457 [20004, 'CREATE INDEX ia ON t(a);']; s06_karisik_4096 @#5 [151, "INSERT INTO t VALUES(94,1737,'2299djo33m7e2a5ps37tw11dgan12zw9k …
- family `u` (3): u01_freelist_reuse_order @#41 [43, 'INSERT INTO t VALUES(100, zeroblob(4000));']; u02_freelist_trunk_overflow @#408 [112, 'INSERT INTO t VALUES(110, zeroblob(450));']; u04_delete_all_truncate_freelist @#24 [1504, 'CREATE INDEX ia ON t(a);']
- family `v` (1): v03_vacuum_with_index @#78 [3004, 'CREATE INDEX ia ON t(a);']
`scripts 234  identical decision logs 170  differing 64  no-result []  events C 1220296 Rust 1072541`

| decision outcome | C | Rust |
|---|---|---|
| `w|del|` | 167182 | 165553 |
| `w|ins|` | 1053114 | 906988 |

First mismatching decision per differing script (64; event index, statement):
- family `a` (2): a12_autoincrement @#4 [3, "INSERT INTO t(a,b) VALUES(485,'7wjyax4r3o9aj2xkvnd me6vzfb');"]; a15_insert_select @#6005 [5, 'CREATE TABLE v AS SELECT b, a FROM t WHERE id % 3 = 0;']
- family `c` (1): c11_delete_range_indexed @#4405 [1506, 'DELETE FROM t WHERE a < 30000;']
- family `d` (3): d01_replace_existing @#1502 [1504, "INSERT OR REPLACE INTO t VALUES(1,552,'bnqdzyif3a7pbek7zw8ezbp; d03_replace_unique_index @#2003 [1004, "REPLACE INTO t VALUES(2000,432,'ckoneh1pw25ir3xracpwopqpiunzq2; d05_upsert_nothing_wr @#36 [36, "INSERT INTO t VALUES('k0022',1) ON CONFLICT(k) DO UPDATE SET v=v
- family `f` (8): f01_create_index_small @#43 [44, 'CREATE INDEX ia ON t(a);']; f02_create_index_large @#3003 [3004, 'CREATE INDEX ia ON t(a);']; f06_alter_drop_column @#503 [504, 'ALTER TABLE t DROP COLUMN b;'] …
- family `g` (14): g_b9_order @#125 [15, 'DELETE FROM t WHERE a > 30;']; g_h3_cookie @#28 [26, 'INSERT INTO c(x,y) VALUES(1,2);']; g_v1_battery @#12 [3, 'CREATE INDEX ar ON a(r);'] …
- family `h` (1): h05_autovac_full_drop @#4510 [1509, 'CREATE INDEX iu ON u(x);']
- family `k` (2): k04_rollback_ddl @#305 [306, 'CREATE INDEX ia ON t(a);']; k05_stmt_abort @#604 [306, 'UPDATE OR REPLACE t SET a = 10 WHERE id > 290;']
- family `l` (3): l01_analyze @#3003 [3004, 'CREATE INDEX ia ON t(a);']; l02_analyze_twice_and_table @#1003 [1004, 'CREATE INDEX ia ON t(a);']; l03_pragma_optimize @#1003 [1004, 'CREATE INDEX ia ON t(a);']
- family `m` (1): m03_recursive_trigger_instead_of @#62 [11, 'REPLACE INTO t VALUES(500, 9);']
- family `n` (8): n02_rtree_one_insert @#9 [2, 'INSERT INTO b VALUES(1,7330.114,7436.864,902.618,1115.304);']; n03_rtree_autocommit @#9 [2, 'INSERT INTO b VALUES(1,4914.549,5228.192,286.782,357.728);']; n04_rtree_txn @#9 [3, 'INSERT INTO b VALUES(1,5534.590,5729.412,6461.806,6656.220);'] …
- family `o` (8): o01_fts5_create @#6 [1, 'CREATE VIRTUAL TABLE f USING fts5(title, body);']; o02_fts5_one_insert @#6 [1, 'CREATE VIRTUAL TABLE f USING fts5(title, body);']; o03_fts5_autocommit @#6 [1, 'CREATE VIRTUAL TABLE f USING fts5(title, body);'] …
- family `q` (2): q05_sqlite_sequence @#6 [3, 'INSERT INTO b(x) VALUES(1);']; q09_rowid_extremes @#3 [3, "INSERT INTO t(a) VALUES('random rowid chosen by engine?');"]
- family `s` (4): s05_sonradan_indeks @#20003 [20004, 'CREATE INDEX ia ON t(a);']; s10_without_rowid @#30003 [15004, 'DELETE FROM t WHERE n % 7 = 0;']; s12_rtree @#9 [3, 'INSERT INTO b VALUES(1,6245.989,6303.029,6330.899,6335.244);'] …
- family `t` (4): t01_real_affinity_int_values @#30 [19, 'CREATE TABLE e AS SELECT r FROM a;']; t04_alter_rename_only @#4 [3, 'ALTER TABLE t RENAME COLUMN a TO alpha;']; t05_schema_cookie @#25 [13, 'ALTER TABLE a RENAME TO a2;'] …
- family `u` (2): u01_freelist_reuse_order @#75 [54, 'INSERT INTO u VALUES(zeroblob(9000));']; u04_delete_all_truncate_freelist @#1503 [1504, 'CREATE INDEX ia ON t(a);']
- family `v` (1): v03_vacuum_with_index @#3003 [3004, 'CREATE INDEX ia ON t(a);']

Both classes are sensitive to everything that shapes the trees. Scripts that differ do so at statements of classes that
are out of scope here: splits (B4–B6: other sibling counts, so other allocations), delete-side rebalance (B1: C frees
pages Rust keeps), CREATE INDEX fill (B3: Rust allocates leaves before interiors), FTS5 / R-Tree writers. Where the trees
agree the logs agree: `f04_drop_table` (48 allocations incl. 13 from the free list), `g_b9_order` (all INSERT and UPDATE
statements; first difference = row 10, an index-driven DELETE).
Differences in `w` that are NOT caused by tree shapes (found by this trace, not aligned): (a) `REPLACE` / `INSERT OR REPLACE`
hitting an existing rowid — C deletes the old row's index entries and lets `OP_Insert` overwrite the row, Rust deletes
the row and inserts it again; (b) `CREATE TABLE ... AS SELECT` — C inserts the rows before it rewrites the catalogue
placeholder, Rust rewrites the catalogue row first (same final file in the scenarios); (c) `CREATE INDEX` — C inserts
entry by entry through `sqlite3BtreeInsert`, the Rust bulk loader writes whole pages (class B3), so no `w` events;
(d) row 10 (index-driven one-pass DELETE); (e) ALTER TABLE catalogue rewrites (section 3).
Not verified: row 3 for LE (only `incremental_vacuum` scenarios reach it), row 6 for `balance_nonroot` siblings (the Rust
split code is not a port, the hint is the closest equivalent), row 8 with `hasFK > 1`, B13 beyond what follows from B7.

## 7. P2, W4, H4 / W2

C source: `setSectorSize` (pager.c), `walCheckpoint` / `walIteratorInit` (wal.c), `findCreateFileMode`, `robust_open`,
`unixOpen`, `unixOpenSharedMemory` (os_unix.c).

| # | decision | C | Rust |
|---|---|---|---|
| 1 | journal header segment size | 512 for a temp file or a device with power-safe overwrite (default); else the VFS sector size (32..65536) | `Pager::effective_sector_size` |
| 2 | checkpoint: frames considered | newest frame per page over `(nBackfill, mxFrame]` | `Wal::backfill_frames_under_read0` |
| 3 | checkpoint: copy or skip | skip when that newest frame is `> mxSafeFrame` or the page is beyond the database size; else copy | same |
| 4 | mode of a new file | database: 0644 & ~umask; journal / WAL: mode and owner of the database file (chmod after open if the umask removed bits, chown only succeeds as root); delete-on-close: 0600; `-shm`: mode and owner of the database file | `vfs::sys::unix::find_create_file_mode`, `apply_created_file_mode`, `open_wal_shm_with_options` |
| 5 | symlinks | `O_NOFOLLOW` on every open (the path is resolved first) | `O_NOFOLLOW` constant per target (x86, x86_64, riscv64, s390x, loongarch64, aarch64, arm, powerpc64 Linux; macOS); not applied on the read-only fallback opens |

Trace classes `journal`, `ckpt`, `open` (`open` and `journal` are compared as sets of outcomes — the engines open files a
different number of times):

`scripts 234  identical decision logs 233  differing 1  no-result []  events C 1308 Rust 1354`

| decision outcome | C | Rust |
|---|---|---|
| `ckpt|page|copy` | 599 | 645 |
| `journal|sector|psow-512` | 234 | 234 |
| `open|mode|db-mode` | 232 | 232 |
| `open|mode|default-0644` | 234 | 234 |
| `open|mode|shm-db-mode` | 9 | 9 |

First mismatching decision per differing script (1; event index, statement):
- family `s` (1): s09_karisik_wal @#0 [792, "INSERT INTO t VALUES(401,4219,'4d04pur ',848.571591);"]

W4 with a pinned reader (`w4test.py`, two reader/writer processes per engine, 3 variants): same+new pages             MATCH result row C ['0 — decisions C {'copy': 7, 'skip-newest-frame-beyond-safe': 8, 'skip-page-beyond-db-size': 0} R {'copy': 7, 'skip-newest-frame-beyond-safe': 8, 'skip-page-beyond-db-size': 0} / only new pages             MATCH result row C ['0 — decisions C {'copy': 10, 'skip-newest-frame-beyond-safe': 22, 'skip-page-beyond-db-size': 0} R {'copy': 10, 'skip-newest-frame-beyond-safe': 22, 'skip-page-beyond-db-size': 0} / nothing after the reader   MATCH result row C ['0 — decisions C {'copy': 11, 'skip-newest-frame-beyond-safe': 0, 'skip-page-beyond-db-size': 0} R {'copy': 11, 'skip-newest-frame-beyond-safe': 0, 'skip-page-beyond-db-size': 0}
Not verified: row 1 without power-safe overwrite (no such VFS here); row 3 "page beyond the database size" (0 times);
row 4 owner change (needs root); row 5 on targets other than x86_64 Linux; FULL / RESTART / TRUNCATE checkpoints under a
pinned reader.
