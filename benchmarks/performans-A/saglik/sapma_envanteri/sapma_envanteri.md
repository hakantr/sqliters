# Divergence inventory: sqliters (Rust, `performans-A`@1c12e74) vs C SQLite 3.54.1

Step 5c, analysis only (no engine change). Measured 2026-11-12 on the Xeon E5-2680 v4 box, tmpfs unless stated.
Rust = `target/perf-work/step5b_bin/sqliters` (CLI) / `speedbench`; C = `target/perf-work/bin/sqlite3` (3.54.1, file inspection),
`target/sqlite-ref/sqlite3_o3` and `c_speedbench` (timing). Everything below is reproducible with the tools in
`target/perf-work/step5c/` (S5 below).

## 0. How to read this

* **Tool**: `S5/ilk_ayrisma.py SCRIPT.sql` (first-divergence finder; file model in `S5/dbfile.py`). It runs growing prefixes of the
  statement list through both CLIs (fresh database per run, tmpfs, bisection + sampling for non-monotonic cases), compares the two
  files in layers and prints, per layer, the first statement that breaks it, with a description ("leaf split: Rust [166] -> [83, 83],
  C [166] -> [86, 80]", "ib:/000/ is page 9 in Rust (appended), page 8 in C", "freelist: Rust 0, C 2", ...).
  Exit code 0/1 = gate (`--require L2` etc.), `--json` machine output, `--env K=V` passes experimental knobs to the Rust engine,
  `--mask schema-order|stat-order` hides two known row-order classes. Helpers: `icerik_fark.py` (decoded row-level diff of a tree),
  `kabul.py` (cross acceptance: both engines run `integrity_check` on both files), `run_all.sh`, `ozet.py`, `aile_ozeti.py`, `show.py`.
* **Layers** (a file pair agrees at Lk; smaller = closer):

  | layer | meaning |
  |---|---|
  | L0 | raw bytes identical |
  | L1 | L0 ignoring header change counter (24..27) and version-valid-for (92..95) |
  | L2 | L1 ignoring unused bytes inside pages (unallocated gap, freeblock bodies, fragments, reserved bytes, tail of last overflow page, freelist-leaf bodies, freelist-trunk tail, pointer-map entries past EOF) |
  | L3 | same (tree, path) page set, each page byte-identical in live content once page-number fields are blanked (pure renumbering); same page count and freelist length |
  | L4 | same tree shape (paths, page kinds, cells per page => depth, page count) and same logical content; cells at different offsets inside pages |
  | L5 | same logical content per tree only (shape differs) |
  | L6 | logical content of some tree differs (schema text, record encoding, sqlite_stat1, shadow tables, ...) |

* **Scenario sets**: `S5/sc/health` (the 13 health scenarios), `S5/sc/t` (140 targeted, generator `senaryolar.py`), `S5/sc/t2` (39 follow-ups,
  `senaryolar2.py`). Results: `S5/res/{health,t,t2}/*.{txt,json}`, cross acceptance `S5/res/kabul.jsonl`.
* The C 3.54.1 shell opens databases with `SQLITE_DBCONFIG_DEFENSIVE` **on**; sqliters' CLI does not. The finder runs C with
  `-cmd ".dbconfig defensive off"`; without that `PRAGMA journal_mode=OFF` and `PRAGMA schema_version=N` are refused by C only (class S14).

## 1. How far do the files agree today? (final file of each scenario; 192 scenarios = 13 + 140 + 39)

```
family                               n   L0   L1   L2   L3   L4   L5   L6
A inserts                           17    .    .    5    .    9    2    1
B updates                            9    .    .    1    .    7    1    .
C deletes                           11    .    .    1    1    1    8    .
D replace/upsert                     5    .    .    .    .    2    3    .
E overflow                           6    .    .    3    .    2    1    .
F DDL                               14    .    1    2    1    4    5    1
G vacuum                            14    .    .    .    6    .    8    .
H auto_vacuum                       13    .    .    4    1    3    5    .
I page sizes                        11    .    1    3    .    1    6    .
J journal modes                     11    .    .    1    .   10    .    .
K savepoint/rollback                 7    .    1    5    .    .    1    .
L analyze                            4    .    .    .    .    .    3    1
M triggers/FK                        3    .    .    1    .    2    .    .
N R-Tree                             9    .    .    .    1    .    2    6
O FTS5                               8    .    .    .    .    .    2    6
P JSONB                              1    .    .    .    .    .    .    1
Q misc                              10    .    2    1    1    2    1    3
R deep sequential                    3    .    .    .    .    .    3    .
S health (13)                       13    .    1    1    .    1    8    2
T follow-ups (types/alter/cookie)    9    .    1    .    .    1    3    4
U freelist                           4    .    .    .    2    .    2    .
X empty-db/pragmas                  10    .    1    2    .    .    .    7
TOTAL                              192    0    8   30   13   45   64   32
  agree at L0 or better: 0/192 = 0%
  agree at L1 or better: 8/192 = 4%
  agree at L2 or better: 38/192 = 20%
  agree at L3 or better: 51/192 = 27%
  agree at L4 or better: 96/192 = 50%
  agree at L5 or better: 160/192 = 83%
  agree at L6 or better: 192/192 = 100%
```

* **No scenario is byte-identical (L0)**: class H1 (change counter) hits the first statement of every script.
* 20% of the scenarios end at L2 or better (identical modulo counter + unused bytes): sequential inserts without index up to one interior
  page, multi-row/`INSERT..SELECT` inserts, table-only overflow rows, `DELETE FROM t` (truncate), CTAS, savepoint/rollback scenarios,
  collations, views/triggers. 50% end at L4 or better (same tree shape). 83% end at L5 or better (same logical content).
* 17% (32 scenarios) end at L6 = something observable through SQL differs in the file: R-Tree and FTS5 shadow tables (12), empty-database
  header (7), REAL storage / real->text / `ALTER TABLE ADD COLUMN` text / JSONB / `PRAGMA optimize` / random rowid (rest).
* Every Rust-written file of the 192 scenarios passes C `PRAGMA integrity_check` **except `o07_fts5_options`** (`fts5: checksum mismatch
  for table "f"`, class F-family); every C-written file passes the Rust check. The Rust engine was deterministic on all scenarios.

## 2. Class inventory

Columns: **L** = best layer the files can still reach while the class is present; **C↔R** = does C accept the Rust file / Rust the C file
(`integrity_check`, reads); **SQL** = observable through SQL / PRAGMA / CLI; **det** = deterministic; **healthier** = which behaviour is
better for the file/user and why; **align** = effort/risk to make Rust do what C does; **speed** = cost (−) or gain (+) of aligning
(m = measured here, e = estimate). Rust code references are `crate-file:line fn` at HEAD 1c12e74.

### 2.1 Header and file bookkeeping

| id | L | where (minimal SQL) | C (function) | Rust (function) | effect | C↔R | SQL | det | healthier | align / speed |
|---|---|---|---|---|---|---|---|---|---|---|
| **H1** change counter +1 | L1 | any first statement, e.g. `CREATE TABLE t(a)` → counter 2 vs 1 | new page 1 has bytes 24..99 zero (`newDatabase`, btree.c); `pager_incr_changecounter` +1 per commit; **WAL commits never bump** (`sqlite3PagerCommitPhaseOne`) | `pager lib.rs:2636 initialize_empty_database` starts counter and version-valid-for at **1**; commit adds 1 (`commit_phase_one` :3662); WAL branch (:3580, `wal.rs:1694`) also bumps when page 1 is written | 2×4 header bytes, forever +1 (rollback modes). No extra commit or write (strace: one journal+db sequence in both) | yes / yes | no (`PRAGMA data_version` is not implemented in Rust at all, S10) | yes | equal | 2 lines (start at 0); WAL bump removal ~10 lines, low risk. speed 0 (e) |
| **H2** header of a table-less database | L6 (transient; heals at first `CREATE TABLE`) | `PRAGMA journal_mode=WAL;` / `PRAGMA auto_vacuum=FULL;` / `PRAGMA user_version=5;` / `BEGIN IMMEDIATE; COMMIT;` / `VACUUM;` on a new file | schema-format (44) and text-encoding (56) stay 0 until `sqlite3StartTable` emits `OP_SetCookie BTREE_FILE_FORMAT/BTREE_TEXT_ENCODING` | same init function writes format 4 / encoding 1 immediately (`header.rs:85`, `lib.rs:2670`); `Pager::set_encoding` (`lib.rs:1928`) refuses once `page_count != 0` | header bytes; **consequence: `PRAGMA journal_mode=WAL; PRAGMA encoding='UTF-16be'; CREATE TABLE t(a TEXT);` gives a UTF-16be database in C and a UTF-8 one in Rust** (x08); also `VACUUM` of a database whose last table was dropped leaves format 0 in C, 4 in Rust (q07) | yes / yes | **yes** (`PRAGMA encoding`, stored text bytes) | yes | C (encoding stays selectable) | small: write 0/0, set both at first table creation, allow `set_encoding` while encoding field is 0. Risk low-medium (every reader of the header must treat 0 as UTF-8 / format 1..4). speed 0 (e) |
| **H3** schema cookie | L3 | `CREATE TABLE b(x UNIQUE)` (+2 vs +1), `... AUTOINCREMENT` first use (+2 vs +1), `VACUUM` (+2.. vs +1), `DROP TABLE` with indexes or in auto_vacuum, `ALTER TABLE RENAME` (8 vs 5 in t04) | `sqlite3ChangeCookie` once per DDL statement (build.c); vacuum.c meta+1; `sqlite3RootPageMoved` no bump | `schema.rs:2520 bump_schema_cookie` is called by every `insert/update/delete_schema_entry`, i.e. **once per sqlite_schema row touched** (`engine.rs:24596 exec_create_table`, `:25437 exec_drop_table`, `:28224 exec_alter_table`, `:33414 build_vacuum_image`, `:24987 apply_root_page_move`) | header bytes 40..43 only | yes / yes | **yes** (`PRAGMA schema_version`) | yes | equal (any change invalidates other connections' schema either way) | move the bump to one call per DDL statement (~8 call sites); low risk. speed 0 |
| **H4** file creation flags | – (not in file) | any new file | `robust_open(..., 0644)` + `O_NOFOLLOW`; `findCreateFileMode` copies mode/owner of the db to `-journal/-wal/-shm` (os_unix.c) | `vfs lib.rs:3243 OsVfs::open` uses `OpenOptions` without `.mode()` → 0666 & ~umask (0664 under umask 002), no `O_NOFOLLOW`; `-shm` `:2737` same | permission bits of db / journal / wal / shm; symlink-follow behaviour | n/a | via `stat` only | yes | **C** (tighter default; journal inherits db mode; no symlink follow) | ~30 lines (`OpenOptionsExt::mode`, `custom_flags`, stat db for side files); low risk. speed 0 |

### 2.2 Unused bytes and freed pages

| id | L | where | C | Rust | effect | C↔R | SQL | det | healthier | align / speed |
|---|---|---|---|---|---|---|---|---|---|---|
| **U1** unused bytes in rebuilt pages | L2 | first page split, e.g. 86th row of `a01` (C leaves 3 923 stale bytes in the gap of the old root) | `rebuildPage`/`editPage`/`balance_deeper`/`zeroPage` never clear the gap; `defragmentPage` zeroes | `btree page.rs:443 ParsedPage::write` zeroes `raw[hdr..usable]` then lays cells down; every split/rebuild goes through `lib.rs:1022 write_parsed_page` | bytes only | yes / yes | no | yes | Rust slightly (no data remnants in the gap) | cannot be aligned byte-for-byte without reproducing C's exact memcpy history; **mask (L2) instead** |
| **U2** stale bytes after a plain cell delete | L2 | `DELETE FROM t WHERE id=34` (s06 #56) | `freeSpace` writes only the 4-byte freeblock header; `dropCell` leaves the 2-byte pointer slot stale | `page.rs:827 raw_page_free_space` same, but zeroes the vacated pointer slot (:2497, :2634) | bytes only (Rust keeps deleted row content in freeblocks exactly like C: 209 KB in s06) | yes / yes | no | yes | equal | none needed |
| **U3** `PRAGMA secure_delete` ignored | L2 (content) | `PRAGMA secure_delete=ON; ... DELETE ...` (q04: Rust leaves 3 056 bytes of deleted rows in freeblocks, C 0) | `BTS_SECURE_DELETE`/`BTS_FAST_SECURE`: `freeSpace`, `freePage2`, `clearCellOverflow` zero the content; pragma returns `1` | pragma falls into the catch-all `_ =>` arm of `engine.rs:35002 exec_pragma` (:36524): **no row returned, nothing zeroed** | deleted data stays recoverable although the user asked for secure delete | yes / yes | **yes** (`PRAGMA secure_delete` prints nothing instead of 0/1) | yes | **C** – this is a skipped guarantee | medium: flag in the b-tree, zero in `raw_page_free_space` + overflow free (~60 lines) + pragma; low risk. speed 0 when off |
| **U4** freed pages are journalled, zeroed and rewritten | L2 | `DROP TABLE t` / `DELETE FROM t` / any page freed by a merge. 3 509-page table: `DROP TABLE` = **7 022 pwrites in Rust vs 25 in C**; `instructions:u` 112.6 M vs 17.2 M (m); `DELETE FROM t` 472 M vs 17.2 M (m) | `freePage2`: a leaf of the freelist is not journalled and not written (`sqlite3PagerDontWrite`), only the trunk page changes; `allocateBtreePage` does not read a reused leaf (`PAGER_GET_NOCONTENT`) | `btree lib.rs:853 free_page` zero-fills the freed page (:880, :895) → page is journalled + written; `allocate_btree_page` (:603) zero-fills again on reuse | freelist leaves are all-zero in Rust files, stale in C files; **Rust does 2 extra page writes per freed page** | yes / yes | no | yes | Rust for privacy (equals `secure_delete=ON` for whole pages), C for I/O | small-medium: do not dirty freelist leaves unless secure_delete (ties in with U3); risk low (journal must not need the old image: C relies on the same reasoning). **speed +: 6.5× fewer instructions for DROP TABLE, 27× for DELETE-all (m)** |

### 2.3 Record and value encoding (file content differs: L6)

| id | where | C | Rust | effect | C↔R | SQL | det | healthier | align / speed |
|---|---|---|---|---|---|---|---|---|---|
| **V1** REAL-affinity column holding an integral value | `CREATE TABLE a(r REAL); INSERT INTO a VALUES(2);` (also `2.0`, `UPDATE`, `INSERT..SELECT`, CTAS) on a rowid table **without** index / CHECK / BEFORE trigger | `OP_MakeRecord` carries the affinity: `MEM_IntReal` and `sqlite3VdbeIntegerAffinity` store the value as INTEGER serial type (0–6 bytes) when it fits in 48 bits (vdbe.c). With an index/CHECK/trigger `OP_Affinity` runs first and C stores a float too (so those cases already match) | `types record.rs:29 serial_type_for_with_encoding` always encodes `Value::Real` as serial type 7 (8 bytes); `value.rs:91 TypeAffinity::apply` | record bytes; **file size: 100 000 rows × 3 REAL columns with integral values: Rust 3 534 848 bytes, C 1 531 904 (2.3×)** (m) | yes / yes | no (`typeof` = real in both) | yes | **C** (smaller rows, fewer pages) | medium: add the int-for-real rule to the record encoder for the same code paths as C (must NOT apply when an index/CHECK/BEFORE trigger forces `OP_Affinity`, or the files diverge the other way). Risk medium. speed: slight + on such tables (fewer pages), ~0 elsewhere (e) |
| **V2** REAL → TEXT conversion | `CREATE TABLE t(x TEXT); INSERT INTO t VALUES(1e15);` / `CAST(1e15 AS TEXT)` / `1e16 \|\| ''` / CLI output | `sqlite3VdbeMemStringify` → `%!.15g` (`sqlite3FpDecode`, printf.c/util.c): `1000000000000000.0`, `1.0e+16`, `1.0e+100`, `1.0e-05`, `0.66666666666666663`, `4.6116860184273879e+18` | `value.rs:687 format_sqlite_real` (Rust shortest round-trip): `1e15`, `1e16`, `1e100`, `1e-5`, `0.6666666666666666`, `4.611686018427388e18`; used for TEXT affinity, CAST, `\|\|`, Display, CLI, fast_read, JSON | stored TEXT differs; every query printing such a real differs | yes / yes | **yes** (this is the root of known class j "CLI formatting of reals" – it is an engine conversion, not a CLI matter) | yes | C by definition (compat target) | medium: port `sqlite3FpDecode` + the `%!.15g`/`%!.17g` rules into one formatter (~200 lines), high test value; risk low. speed ~0 |
| **V3** JSONB element type of inserted text | `SELECT hex(jsonb_set(jsonb('{"a":1}'),'$.new',5))`, `jsonb_replace(...,'$.a','yy')` | new labels: `JSONB_TEXTRAW` (0xA) (`jsonLookupStep`), SQL text args: always TEXTRAW (`jsonFunctionArgToBlob`, json.c) | `types json.rs:2667 encode_key_jsonb`, `:1730 append_sql_arg_as_jsonb` choose TEXT (0x7) when the string is "plain" | one nibble per inserted label/string in stored JSONB blobs | yes / yes (both readers accept both) | **yes** (`hex()` of the blob) | yes | equal (Rust's is marginally more precise) | ~10 lines; risk low. speed 0 |
| **V4** rowid chosen after `max(rowid)=2^63-1` | `INSERT INTO t(rowid,a) VALUES(9223372036854775807,'m'); INSERT INTO t(a) VALUES('x');` | `OP_NewRowid`: `sqlite3_randomness`, `v &= MAX>>1; v++`, 100 tries | `btree lib.rs:2094 next_rowid`: xorshift with a **fixed seed**, restarted per call (deterministic), fallback scan 1..10000 | different rowid | yes / yes | yes (rowid value) | Rust yes, **C no** | – (C is random by design; cannot be equal) | leave; document |

### 2.4 Schema text, catalogue order, statistics

| id | L | where | C | Rust | effect | C↔R | SQL | det | healthier | align / speed |
|---|---|---|---|---|---|---|---|---|---|---|
| **D1** `ALTER TABLE ADD COLUMN` rewrites the column text | L6 | `CREATE TABLE t(a); ALTER TABLE t ADD COLUMN b DEFAULT 7;` → stored SQL `... b DEFAULT (7))` vs `... b DEFAULT 7)`. Also `CHECK(g >= 0)` → `CHECK (g >= 0)`, `GENERATED ALWAYS AS (a+1)` → `(a + 1) VIRTUAL`, **`DEFAULT x'00'` → `DEFAULT (x'')` (the default value is lost)**, strings are not re-quoted; `ADD COLUMN d DEFAULT (1+2)` / `DEFAULT CURRENT_TIMESTAMP` on a non-empty table accepted (C: `Cannot add a column with non-constant default`) | `sqlite3AlterFinishAddColumn` (alter.c) appends the ORIGINAL token text of the column definition; validates the default | `engine.rs:29230` (in `exec_alter_table`) → `:33239 render_column_def_sql` rebuilds the text from the AST with `compiler.rs:2454 expr_column_label` (`Expr::Blob(_) => "x''"`), `:33317 append_column_to_create_table_sql` | `sqlite_schema.sql` text; for blob defaults the **meaning** changes | yes / yes | **yes** (`SELECT sql FROM sqlite_schema`, `.schema`, default values of old rows) | yes | **C**; the blob case is a data bug | small-medium: splice the original source span (the parser already has token offsets for RENAME/DROP COLUMN) + port the constant-default check; risk low. speed 0 |
| **D2** `CREATE VIRTUAL TABLE`: order of sqlite_schema rows | L5 (with `--mask schema-order`), else L6 | `CREATE VIRTUAL TABLE b USING rtree(id,x0,x1);` → rowid 1 = `b_rowid` (Rust) vs `b` (C); same for FTS5 | `sqlite3VtabFinishParse` inserts the vtab row, then `OP_VCreate` → `xCreate` creates the shadow tables | `engine.rs:24708 exec_create_virtual_table` calls `module.create()` (:24783) first, then `insert_schema_entry` (:24798) | rowids of sqlite_schema rows | yes / yes | **yes** (`SELECT name FROM sqlite_schema` order, `PRAGMA table_list` order) | yes | equal | ~10 lines (insert the row first, roll it back if `create` fails); risk low. speed 0 |
| **D3** `ANALYZE`: order of sqlite_stat1 rows | L5 (with `--mask stat-order`), else L6 | table with ≥2 indexes, `ANALYZE;` → Rust `ia, ib, iac`, C `iac, ib, ia`; WITHOUT ROWID: Rust PK first, C PK last | `analyzeOneTable` walks `pTab->pIndex` (newest index first; PK of WITHOUT ROWID where it sits in that list); tables in schema-hash order | `engine.rs:26952 analyze_one_db`, `:27020 analyze_one_table` (creation order; PK first). The helper `:15733 ordered_table_indexes` already yields C's order | rowids of stat1 rows; stat values are identical in every scenario tested | yes / yes | **yes** (`SELECT * FROM sqlite_stat1` without ORDER BY) | yes | equal | ~15 lines; table order across tables NOT verified against C's hash order. speed 0 |
| **D4** `PRAGMA optimize` is a no-op | L6 | `CREATE INDEX ..; SELECT ... WHERE a=5; PRAGMA optimize;` → C creates/updates sqlite_stat1, Rust nothing | `sqlite3Pragma` `PragTyp_OPTIMIZE` → `sqlite3_analyze` on candidate tables | catch-all arm of `exec_pragma` (`engine.rs:36524`) | sqlite_stat1 absent → later plans may differ | yes / yes | **yes** | yes | C | medium (needs the "table has grown / never analysed" bookkeeping); risk low |
| **D5** AUTOINCREMENT writes sqlite_sequence twice per row | – | `INSERT` into an AUTOINCREMENT table | once at `sqlite3AutoincrementEnd` | `engine.rs:17399 allocate_table_rowid` → `update_sqlite_sequence` before the row and again at :16777 (code reading only, **not measured**) | none in the final file (a12/q05 agree at L3 until other classes) | – | no | yes | C (one write) | small; speed + small on AUTOINCREMENT inserts (e) |

### 2.5 Pager, rollback journal, memory (not visible in the committed file; found with `strace` and hot-journal capture, `S5/jr/`)

| id | where | C | Rust | effect | observable | healthier | align / speed |
|---|---|---|---|---|---|---|---|
| **P1** no page-cache spill in rollback-journal modes | large transaction in `journal_mode=DELETE/TRUNCATE/PERSIST/MEMORY` | `pagerStress` → `syncJournal` + write dirty page to the db when the cache (`cache_size`) is full; journal gets one header segment per spill (hot journal of the test: 12 segments, nRec=5, db already grown) | `spill_if_over_cache_size` (`pager lib.rs:1350`) only knows `spill_dirty_pages_to_wal`; in rollback modes every dirty page stays in memory until commit (hot journal: 1 segment, nRec=0, db untouched) | **memory is O(transaction)**: 1e6 single-row INSERTs in one transaction, `cache_size=100`: Rust **78 MB** RSS (database 72 MB) vs C **5 MB**; WAL mode: Rust 8 MB vs C 4.6 MB (m) | RSS; journal layout; a crash mid-transaction never needs a rollback in Rust (db untouched) | C for memory bound; Rust for I/O (fewer syncs/writes mid-transaction) | medium-large: port `pagerStress` for rollback mode (journal sync + header segment + sub-journal interplay, savepoints); risk medium-high (durability path). speed: − on transactions larger than the cache (extra journal fsync per spill on real disks; ≈0 on tmpfs) (e) |
| **P2** journal header padded to 4096 bytes instead of 512 | every rollback-journal transaction | `setSectorSize`: with `SQLITE_IOCAP_POWERSAFE_OVERWRITE` the sector size is 512 → `JOURNAL_HDR_SZ` 512, first record at offset 512 | `lib.rs:4414 effective_sector_size` takes the VFS value (4096) → records start at 4096; header field says 4096 so it is self-describing | journal bytes/offsets only; hot journals are cross-recoverable both ways (tested: 4 combinations × DELETE/PERSIST/TRUNCATE, `integrity_check` ok, same rows) | no | equal | trivial (return 512 when PSOW); speed 0 |
| **P3** journal record written with one `pwrite` | same | `pager_write` → `write32bits`, `sqlite3OsWrite(page)`, `write32bits` = 3 writes of 4 / pagesize / 4 bytes | 1 write of pagesize+8 | same bytes, 1/3 of the journal syscalls (22 vs 34 pwrites for 4 small transactions) | only via syscall trace; a torn record is rejected by the checksum in both | Rust | – (keep); aligning would cost ≈2 syscalls per journalled page |
| **P4** directory fsync once, not per transaction, in TRUNCATE/PERSIST | `PRAGMA journal_mode=PERSIST` (or TRUNCATE), several transactions | journal is closed in `pager_unlock` and re-opened with `SQLITE_OPEN_CREATE` each transaction → `UNIXFILE_DIRSYNC` → directory fsync at the first journal sync of **every** transaction | journal fd stays open; directory fsync only when the file is first created | 3 fewer fsyncs per 4 transactions (FULL: 17 vs 20; NORMAL: 9/13 vs 12/16) (m). DELETE mode: identical counts (16/16, 12/12), MEMORY identical | syscall trace only. The directory entry already exists and was synced at creation, so no durability is lost | Rust (C's extra sync is redundant) | – (keep, document); aligning costs 1 fsync per transaction on real disks |
| **P5** lock downgrade step | end of every write transaction | `pagerUnlockDb(SHARED_LOCK)` then unlock: `F_RDLCK` on the shared range, then `F_UNLCK` | goes from EXCLUSIVE straight to unlocked (no intermediate `F_RDLCK`) | one fcntl less; no window in which C would see a different lock state than the final one | no | equal | – |
| **P6** order of pages in the journal | multi-page transaction | – | – | same page SET journalled (44/44 in the test), order differs where page 1 / a root is first touched (e.g. `[...,1,5,6,2]` vs `[...,2,5,6,1]`) | no | equal | follows from B-tree write order (B9) |

Durability order itself is the same as C in DELETE mode (journal fsync → directory fsync → nRec header write → journal fsync → database
writes → database fsync → unlink), with the same number of fsyncs for `synchronous=OFF/NORMAL/FULL/EXTRA` (0/12/16/20 for 4 transactions).

### 2.6 WAL (details, raw tables and tools: `S5/deney_wal.md`, `S5/wal/`, `S5/wal_anlik.py`)

The WAL path is the best-aligned part of the engine: for every scenario where the b-tree page sets are equal, the `-wal` file before
checkpoint has the **same frames in the same order** (page number, commit mark), the same header (magic, version, page size, checkpoint
sequence, restart behaviour), the same size, and the `-shm` deterministic fields are identical; salts are random by design in C.

| id | where | C | Rust | effect | observable | healthier | align / speed |
|---|---|---|---|---|---|---|---|
| **W0** (= known class i) frame append batching | every WAL commit | `walWriteOneFrame`: 2 `pwrite` per frame (24-byte header, page) | `pager wal.rs Wal::write_frames` stages consecutive frames, one `pwrite` per run ≤ 64 KiB | same bytes at the same offsets; W13: 320 vs 1 236 `pwrite64` | **nothing** found: `-wal` bytes identical knob on/off (21 scenarios); 367–483 prefix cuts and 157–220 block subsets per variant recover to before/after only, both engines agree on every state; live cross-engine reader/writer pairs agree | Rust | aligning = make the 30-line knob unconditional; **costs +14.8% on W13 (tmpfs, m), +36% on W13 (btrfs, synchronous=NORMAL, m), +0.7% on W01 (m)**; nothing measurable with `synchronous=FULL` on a real disk |
| **W1** checksum of frames appended after an in-place rewrite | transaction that spilled (`cache_size` small), then updates an already-spilled page, then commits | `walEncodeFrame` zeroes salt+checksum (`memset(&aFrame[8],0,16)`) while `iReCksum != 0`; `walRewriteChecksums` makes the tail valid last | `write_frames` always writes the real salt and running checksum | **crash hole**: if the commit frame is durable and the in-place rewrite is not, the chain validates with a stale page → half-applied transaction (reproduced 10/10 with kill + block restore; C in the same state discards the transaction) | after a crash only | **C** – skipped guarantee | ~6 lines, makes the spill path cheaper; risk low. **Do first.** |
| **W2** side-file permissions | db created 0600 | `findCreateFileMode`: `-wal`/`-shm`/journal get the db's mode and owner | default mode (0644 under umask 022) | page images of a private database are group/world-readable in `-wal` | `stat` | **C** | with H4 |
| **W3** spill policy | large statement / transaction in WAL mode | `pagerStress`: one LRU page whenever the cache is full, also mid-statement | `spill_if_over_cache_size` → `spill_dirty_pages_to_wal`: only at statement end inside an explicit transaction, all dirty pages in page order | frame count/order differ in spilling transactions (83 vs 74 frames; mxFrame 53 vs 57); memory not bounded within one statement (`cache_size=100`, one 200k-row INSERT: 138 MB vs 4.6 MB RSS) | `PRAGMA wal_checkpoint` log column, autocheckpoint timing, RSS | C (memory bound) | with P1 (same mechanism) |
| **W4** partial checkpoint under a pinned reader | PASSIVE checkpoint while another connection holds a snapshot | `walCheckpoint`/`walIteratorNext`: newest frame per page over the whole WAL, skipped if above mxSafeFrame | `Wal::backfill_frames_under_read0`: newest frame ≤ mxSafeFrame | Rust writes pages that the next checkpoint writes again (db 73 728 vs 69 632 bytes at that moment) | no | C (less I/O) | few lines; speed + small |
| **W5** page-1 change counter in WAL mode | WAL commits | `pager_write_changecounter`: dbFileVers+1, constant while page 1 stays cached | bumped on every commit that writes page 1 | header bytes (2 vs 63 after 60 commits) | no | equal | with H1 |
| **W6/W7** | TRUNCATE checkpoint; recovery | stale `-shm` hash entries left; 1 `pread` per frame | hash region zeroed; 2 `pread` per frame | none | no | equal | – |

### 2.7 B-tree: splits, merges, page numbering, in-page placement (agent report + finder; raw tables `S5/deney_btree.md`, `S5/btree/`)

Decision points where the Rust b-tree (`crates/sqliters-btree/src/lib.rs`, `page.rs`) does not do what `btree.c` does. "finder" = first
statement reported by `ilk_ayrisma.py` in the named scenario. All are deterministic; C accepts every Rust file and vice versa; none is
visible through SQL except via `dbstat`, `page_count`, `freelist_count` and file size.

| id | L | where (finder evidence) | C | Rust | effect on the file | healthier | align / speed |
|---|---|---|---|---|---|---|---|
| **B1** delete-side rebalance (known f) | L5 | `DELETE FROM t WHERE id=89` (s06 #144): C `[65,3]→[67]`, root collapses, 2 pages freed; Rust keeps 2 leaves. c01, c02, c07, c08, a12, k05 | `sqlite3BtreeDelete` → `balance()` when `nFree*3 > usableSize*2`; `balance_nonroot` merges/redistributes up to 3 siblings, also at interior levels; root collapse | `delete_table_at` / `delete_table_rec`, `delete_index_raw` / `try_fast_leaf_delete_index`: act only when a leaf becomes EMPTY; interior only when down to one child (`rebalance_lone_table_interior`) | table fill 49% vs 64%, files **13–22% larger** on churn (s06 296 vs 249 pages, s07 2456/2173, s08 1182/972, s09 281/244) | **C** | prototype `SQLITERS_BTREE_C_DELETE_BALANCE=1`: s06 251/249, s07 2214/2173, s08 986/972, s09 244/244 pages. **W08 +5.4% (m), +4.6% instructions (m)**, 52% of that is the freeblock walk to learn `nFree` (C caches it); estimate ≤ +2% with a cached `nFree` |
| **B2** delete of an interior index cell | L5 | deletes in indexed tables at small page sizes (s07, s08, s11) | predecessor swap, then `balance()` on leaf and interior | `delete_index_raw` divider arm / `delete_index`: when the predecessor leaf has one cell or the new divider does not fit, the **whole index is rebuilt** (`scan_index` + `bulk_load_sorted_index`) | shape and all page numbers of that index; O(n) for such a delete | C | part of the B1 port |
| **B3** `CREATE INDEX` / `REINDEX` fill (known e) | L5 | `CREATE INDEX ia ON t(a)` (s05 #20004, f02, f11, l01): Rust leaves `[409,379,367,…,27]`, C `[454,415,408,…,281]` | `sqlite3RefillIndex`: sorter → `OP_IdxInsert` append with `BTREE_BULKLOAD` → `balance_nonroot(bBulk=1)` packs left siblings full | `engine.rs populate_index_btree` → `Btree::bulk_load_sorted_index`: bottom-up build with `leaf_budget = (usable-8)*9/10` (90% by construction), leaves allocated before interiors | index fill 87–89% vs 98–99%, s05 490 vs 458 pages and one index a level deeper | **C** | prototype `SQLITERS_INDEX_BULK_FILL=1`: s05 458/458 pages, depth 1, **identical to C including page numbers**; large builds at 512/1024+autovac/4096/65536: every index page shape-identical. **W02 +5.6% (m, +2.3% instructions), W08 +3.6% (m; fuller indexes split sooner — C pays the same)**; estimate +2–3% on W02 when tuned (half of the cost is Vec churn of the prototype) |
| **B4** table leaf overflow in the middle of the tree (known d) | L5 | random-order insert (s03 #171, a03 #400, d02): C `[78,90]→[83,86]` (no new page), Rust `[90]→[46,45]` | `balance_nonroot` redistributes over up to 3 siblings before allocating; `balance_quick` only for an append | `insert_table_rec` → `partition_table_leaf_cells`: best 2-way split of the one page when the target is the right-most child and the insert is not an append | page count/fill (s03 259 vs 260; 1 of 260 pages equal) | mixed (Rust 0.4% denser here; C's rule gives ~86% vs 51% leaf fill for ascending inserts into the middle of the key space) | insert-side port (prototype knob `SQLITERS_BTREE_C_INSERT_BALANCE=1`) |
| **B5** first split of a root leaf / index leaf split point (known c) | L5 → L4 later | `INSERT` #172 of s04: index `ib` `[166]→[83,83]` vs C `[86,80]`; a03 `[84]→[41,44]` vs `[42,43]`; WITHOUT ROWID a10; sqlite_schema itself f09 `[57]→[29,29]` vs `[30,28]` | `balance_deeper` + `balance_nonroot(nOld=1)`: split by bytes (`szNew`/`cntNew`, right-biased evening loop) | tables: minimum byte difference; indexes: count middle (`split_index_leaf`, `grow_index_root`); for an index root split the right page is allocated before the left | cell distribution; **sibling page numbers reversed** (Rust left = higher number) | equal | insert-side port |
| **B6** interior page split (new) | L5 | sequential append past one interior page (r01 #25500: `[488]→[1,244,244]` vs C `[1,256,232]`; i_512_seq #665) | `balance_nonroot` at interior level: 3 siblings, left-packed | `store_table_interior`, `split_index_interior`, `insert_index_divider_parsed`: split at the count middle into two, no redistribution | **table interior pages 50% full vs 84–88%**: 32 vs 19 interior pages at 4096 (120k rows), 225 vs 129 at 512 (30k rows); leaves identical. Every table larger than one interior page (~1.6 MB at 4096) diverges at L5 | **C** | insert-side port |
| **B7** freelist: which page is reused (known g partly) | L3 | `CREATE TABLE n0(a)` after drops (f09 #201: Rust takes page 60, C page 2); u03; overflow chains e05 | `allocateBtreePage`: FIRST leaf of the first trunk (last leaf moved into its slot), or the leaf nearest `nearby` (left sibling in `balance_nonroot`, previous page in `fillInCell`, root in `balance_deeper`) | `btree lib.rs:603 allocate_btree_page(AllocMode::Any)`: LAST leaf, no `nearby` | page numbering whenever the freelist is not empty | equal (C's `nearby` gives slightly better locality) | prototype knob `SQLITERS_BTREE_C_ALLOC=1`; ~100 lines |
| **B8** order in which pages are freed | L3 | `DELETE FROM t WHERE a >= 0` (c04: trunk 3 vs 5); DROP TABLE with overflowing index keys | several siblings freed by one balance: ascending; `clearDatabasePage`: child subtree first, then the cell's overflow | insert side `avail_pgnos.pop()` descending; `free_subtree_children` frees an interior index cell's overflow before the child | freelist order → later numbering | equal | small |
| **B9** order of b-tree writes within one row (new) | L3 | first overflowing row with an index (s11 #4, e03 #4: overflow pages 4,5 → index in C, table in Rust); simultaneous splits (s10 #113, a06 #99) | `sqlite3CompleteInsertion`: secondary indexes first (newest first), table / PK tree last; DELETE: indexes then table; UPDATE: `OPFLAG_ISNOOP`, only changed indexes | general path `engine.rs:16752 validate_and_write_row`: **table first**, then indexes in creation order; compiled path `compiler.rs:3122`: indexes first (PK, unique, rest); UPDATE = remove row + indexes, re-insert | which tree gets the lower page numbers / overflow pages; journal page order (P6) | equal | engine-level reordering (~50 lines over 3 paths), medium risk (constraint-check order must stay); speed 0 |
| **B10** in-page cell placement (new) | L4 | first insert after a delete on the same page (s06 #57: content start 1183 vs 1242, freeblocks 1 vs 0); after any split (s07 #15: Rust page rebuilt in key order, C keeps insertion order + 2 freeblocks); every UPDATE that changes a cell size (b01–b03) | `allocateSpace` (freeblock first-fit, else gap), `defragmentPage` only when needed, `editPage`/`rebuildPage`, `freeSpace` coalescing, `dropCell` | `apply_insert_*_raw` allocate only from the gap and defragment otherwise; `ParsedPage::write` (page.rs:443) lays rebuilt pages out clean in key order | bytes/offsets only (fit decisions are equal). With every other B-tree class aligned, s05 is 458/458 logically identical but only 236 pages byte-identical | Rust (less fragmentation, zeroed gaps) / C for byte comparability | 600–800 lines, constrains every raw in-place path, gives up zero-filled gaps; **this is the price of L2** |
| **B11** `UPDATE` implemented as delete + re-insert | L4/L5 | `UPDATE t SET a=a+1` (b08), `UPDATE t SET b=''` (b07) | `btreeOverwriteCell` when the size is unchanged, else `dropCell`+`insertCell` on the same page; no index touch for unchanged columns | row and index entries are removed and re-inserted (`remove_row_and_indexes`, `validate_and_write_row`); leaf replace paths `apply_replace_table_leaf_raw` are equivalent to C | none by itself (L4); with the B1 knob on it triggers merges C does not do (b07/b08 drop to L5 with the prototype) | C | must be handled together with B1 |
| **B12** `VACUUM` rebuild | L5 for indexes / WITHOUT ROWID | `VACUUM` with an index (v03: 344 064 vs 323 584 bytes), WITHOUT ROWID (v06: 29 vs 27 pages). Plain rowid tables: L3 after VACUUM (v01, v02, v04, v05, v07) | `sqlite3RunVacuum`: `INSERT INTO vacuum_db.x SELECT *` with the xfer optimisation → appends with `BTREE_BULKLOAD` into tables **and** indexes | `engine.rs:33414 build_vacuum_image`: indexes rebuilt with the 90% bulk loader | index / WITHOUT ROWID fill after VACUUM | C | follows from B3 (same builder) — the `SQLITERS_INDEX_BULK_FILL` prototype does not cover the VACUUM path yet (g01–g05, v03, v06 stay L5 with knobs) |
| **B13** auto-vacuum relocation | L4 | `DELETE` in `auto_vacuum=FULL` (h06 #63, h11, h13): different pages moved into the freed slots | `autoVacuumCommit` → `incrVacuumStep` → `relocatePage`, `allocateBtreePage(BTALLOC_LE/EXACT)` | `btree ptrmap.rs:569 auto_vacuum_commit`, `:443 incr_vacuum_step` — same structure; the difference comes from B7 (last leaf vs first/closest) | page numbering | equal | follows from B7 (H family: 8 of 13 at L4, 0 at L5 with all knobs) |
| **B14** `UPDATE` growing every row | L5 | `UPDATE t SET b=b\|\|'…'` (b06): tail `[…,40,25,30]` vs C `[…,44,46,1]` | `balance_quick` when the overflowing cell is the last of the right-most leaf | 2-way split | distribution of the last leaves | equal | insert-side port |

**With the prototypes on** (finder over the same 192 scenarios; `S5/res_btree/` = two requested knobs, `S5/res_btree_x/` = plus insert-side
port and C freelist order):

| configuration | ≥L2 | ≥L3 | ≥L4 | ≥L5 | health s03–s11 |
|---|---|---|---|---|---|
| HEAD | 20% | 27% | 50% | 83% | 1×L4, 8×L5; sizes differ in 7 |
| `C_DELETE_BALANCE` + `INDEX_BULK_FILL` | 20% | 27% | 61% | 83% | 2×L4, 7×L5 |
| + `C_INSERT_BALANCE` + `C_ALLOC` | 22% | 28% | **71%** | 83% | **9×L4, all nine file sizes equal to C** |

What still keeps scenarios below L4 with all four knobs: B10 (caps everything at L4), B9 (numbering in pairs between two trees), B11, B12,
sqlite_schema edits (H3/D-classes), ANALYZE table shape, DROP TABLE/INDEX free order (B8).
C's own work for comparison (instrumented amalgamation): W08 = 16 `balance_nonroot` calls (15 insert-triggered, 1 delete-triggered), s06 = 1 620
(51 merges / 96 splits / 1 473 redistributions), s07 = 8 985; the all-knobs prototype makes exactly the same number in every category.
C profile, W08 only: `freeSpace` 6.7%, `dropCell` 0.7%, `balance_nonroot` 0.5%; Rust HEAD: `raw_page_free_space` 5.5%, no split/balance
function above 0.1% — **rebalancing is not where W08 time goes in either engine**.

### 2.8 FTS5 (agent report; scripts and tools in `S5/fts5/`: `f5.py`, `trace.py`, `gen.py`, `sc/s01..s80`, `repro_bdel.sh`, `tok_cmp.py`)

The Rust FTS5 writer is a **different design**, not a port with small deviations. `f_content`, `f_docsize`, `f_config` and the averages
record match C in the common case; `f_data`/`f_idx` match only for small insert-only loads and then only up to the cookie.
Of 84 FTS5 scripts: `f_data` identical 2, identical except cookie 26, same segments/different bytes 16, different segment structure 40.
C rejects the Rust-written table in 5 scripts (s41, s56, s68, s72, o07); Rust rejects or misreads the C-written table in 14.

| id | minimal SQL | C (fts5_index.c / fts5_storage.c) | Rust (`ext fts5.rs`) | effect | C↔R | SQL-visible | healthier | align (estimate) / speed |
|---|---|---|---|---|---|---|---|---|
| **F1** structure cookie | `CREATE VIRTUAL TABLE f USING fts5(a,b); INSERT INTO f VALUES('x','y');` | `fts5StructureWrite` writes `pConfig->iCookie`, bumped only by config changes | `flush_pending`, `write_c_fts5_index`, `reset_index_storage` do cookie+1 on every flush | first 4 bytes of `f_data` id=10 | ok / ok | `hex(block)` only | equal | ~15 lines, but the cookie is load-bearing for the Rust read cache (`ensure_structure_coherent`): needs F2 or another generation counter first |
| **F2** segment schedule | 16 autocommit inserts | `fts5FlushOneHash` → `fts5IndexAutomerge`/`fts5IndexCrisismerge`/`fts5StructurePromote`; lowest free segid (`fts5AllocateSegid`); monotonic `nWriteCounter` | one level-0 segment per flush, segid = max+1; at ≥15 segments the next flush **rewrites the whole index** into segid 1 (`flush_pending_via_compaction` → `load_merged_index` → `write_c_fts5_index`) | levels/segids/segment count differ from the 16th flush; C files larger (merge leftovers: o05 212 992 vs 159 744 bytes) | ok / ok; results equal | `f_data`/`f_idx` rows | **C**: Rust is O(index) every 15 flushes — 4 000 autocommit inserts **7.26 s vs 0.64 s**; 300 inserts onto a 20 000-doc index 5.99 s vs 0.27 s (m) | port merge machinery, 800–1 200 lines, high risk; **large speed gain** for autocommit/update loads |
| **F3** when a segment is flushed inside a transaction | `BEGIN; ...; DELETE/UPDATE/SELECT on f; ...` | flush at xSync, at (statement) savepoints, nested RELEASE, out-of-order rowid, hash > `hashsize`; reads merge the in-memory hash | flush at sync, explicit SAVEPOINT, **every cursor `filter()`** (so every DELETE/UPDATE/SELECT), any user SQL on a shadow table, ~4 MiB/16 384 ops | segment counts differ (s58 1 vs 2, s62 1 vs 4, s10 57 flushes vs 1 segment) | ok / ok | `SELECT count(*) FROM f_data` inside a txn (5 vs 2) | **C**: 2 000 UPDATEs in one txn on 20 000 docs **35.4 s vs 0.21 s**; 2 000 DELETEs 32.8 s vs 0.19 s; 500×(insert+MATCH) 9.1 s vs 0.22 s (m) | queries must read pending data without flushing + C's four triggers, 300–500 lines, medium-high risk; **large speed gain** |
| **F4** leaf pagination | any segment > ~4 000 bytes | `pgsz` (4050) honoured; posting lists split across leaves (`fts5WriteAppendTerm`, `fts5WriteFlushLeaf`) | `encode_segment_pages_bytes`: constant 4000, ignores `pgsz`, never splits a posting list | every multi-leaf segment differs (leaf count 58 vs 57; `pgsz=512`: 10 vs 77 leaves). Single-leaf segments are byte-identical | ok / ok | `length(block)` | C | 150–250 lines, medium risk, speed neutral |
| **F4b** BUG leaf > 65 535 bytes | one document with a term repeated 70 000 times | splits over 18 leaves | `szLeaf` is `len as u16` and wraps | **corrupt index**: Rust's own `MATCH` → "database disk image is malformed"; C rejects the file | **no** / – | yes | C | fixed by F4 |
| **F5** doclist index | 30 000 docs sharing one term | `fts5WriteDlidxAppend` writes dlidx pages, sets bit 0 of `f_idx.pgno` | never written, ignored on read | 2 dlidx rows missing | ok / ok | no | C (faster seeks) | ~150 lines on top of F4 |
| **F6** BUG delete flag on delete+reinsert of the same rowid in one flush | `INSERT INTO f(rowid,..) VALUES(1,'alpha beta','gamma delta'); UPDATE f SET body='gamma delta updated' WHERE rowid=1;` | posting-list header keeps bDel with positions (`nPos*2+1`) | writer replaces the entry (no bDel); **reader** (`parse_doclist_slice_on_block`, `load_merged_index`) treats bDel=1 as a pure delete regardless of nPos | on **C-written** files after any UPDATE/REPLACE: Rust `MATCH 'alpha'` misses the row, `'integrity-check'` reports a false mismatch, and Rust `'optimize'`/auto-compaction **permanently drops those postings** (C then also misses the row) | ok / **no** | **yes** (wrong MATCH results) | C | reader fix ~10 lines, low risk — do regardless; writer parity ~20 lines |
| **F7** `contentless_delete=1` | `CREATE VIRTUAL TABLE f USING fts5(x, content='', contentless_delete=1)` | v2 structure with 5 varints per segment, origin counters, tombstone hash pages | 3 varints per segment, origin always 1, no tombstones (every delete flush = full rewrite) | **mutually unreadable**: Rust cannot read any C v2 table; C misparses Rust tables with ≥2 segments | **no** / **no** | yes | C | format fix ~20 lines; tombstones ~300 lines. 100 autocommit DELETEs on 20 000 docs: 24 s vs ~4 ms |
| **F8** secure-delete | `INSERT INTO f(f,rank) VALUES('secure-delete',1)` | in-place leaf edits, `f_config.version`=5 | full compaction per delete flush, version stays 4 | config row + structure | ok / ok | `f_config` | C (100 deletes 26 s vs 0.1 s) | with F2 |
| **F9** `'optimize'`, `'merge'` | – | optimize → one segment on a new top level; `'merge',N` incremental | both = full rewrite into level 0 segid 1; N and `usermerge` ignored | structure | ok / ok | shadow rows; different error text for `'delete-all'` on a normal table | C | with F2 |
| **F10** config commands | `INSERT INTO f(f,rank) VALUES('pgsz',100000)` | values validated, all take effect, cookie bumped | `pgsz/automerge/crisismerge/usermerge` stored unvalidated and ignored; `hashsize`/`deletemerge` rejected; `rank='nosuchfn()'` rejected at config time (C stores it) | `f_config` rows, errors | – | yes | C | small |
| **F11** tokenizers | text with combining marks, ø/đ/µ/ſ/İ, words ending `eed`/`ies`/`sses`/`yying` (porter) | `fts5_unicode2.c` tables; porter length guards | 4 169 of 21 229 sampled code points classified differently, 497 fold differently; porter edge cases differ (0 of 73 445 dictionary words) | different terms stored → **C rejects the Rust file ("checksum mismatch", this is the `o07` rejection)**; MATCH results differ for such text | **no** in those cases | yes | C | port the tables + 3 guards, mechanical, low risk |
| **F12** conflict clauses / rowids | `INSERT OR REPLACE INTO f(rowid,..)` existing rowid; `REPLACE`, `INSERT OR IGNORE`, `UPDATE OR ...` | replaces / ignores (`fts5UpdateMethod` + `sqlite3_vtab_on_conflict`) | all raise `UNIQUE constraint failed: f_content.id` | table contents differ | – | **yes** | C | small-medium |
| **F13** `contentless_unindexed=1`, `locale=1` | create options | extra content table / column | parsed, not implemented | **cross-engine failures** ("database disk image is malformed", "missing row") | **no** / **no** | yes | C | medium |
| **F14** integrity check | – | checks `version`, idx rows, dlidx, docsize, averages | terms vs content only; `xIntegrity` not run by `PRAGMA integrity_check` | corruption that C reports is missed | – | yes | C | medium |
| **F16** REAL → TEXT inside FTS5 | `INSERT INTO f VALUES(1e100, ..)` | tokenizes `1.0e+100` | tokenizes `1e100` | C rejects the Rust file ("malformed inverted index") | **no** | yes | C | = V2 |

W11 (FTS5 index + bm25; 47.7 ms vs C 57.0 ms): **none of the advantage comes from skipped index work** — with W11-shaped SQL both engines
write exactly one level-0 segment of 42 leaves, no dlidx, no merge, one flush. The gain is on the query side (200 bm25 queries ≈35 ms vs
≈52 ms, CLI emulation); aligning F1/F4/F5 would cost ≈0–1 ms on W11 (estimate); F2/F3 are not triggered by W11.
Matches C today: averages record, `f_docsize` encoding, `f_content`, v1 structure layout, prefix-index terms, detail=none/column encodings
for plain inserts/deletes, ROLLBACK / ROLLBACK TO / statement abort consistency, fts5vocab rows, MATCH order, `ORDER BY rank`.
Query-side differences seen in passing: `bm25()`/`highlight()` on a zero-row MATCH errors in Rust; detail=none/column bm25 and snippet
differ; `temp.` fts5vocab on a main table fails.

### 2.9 SQL-level behaviour that differs (verified with both CLIs, `S5/../ilk/j.sql` style scripts; statements in the table)

| id | statement | C | Rust | note |
|---|---|---|---|---|
| S1 | `SELECT 1e15, 1e16, 1e100, 1.0e-5, 2.0/3` | `1000000000000000.0\|1.0e+16\|1.0e+100\|1.0e-05\|0.66666666666666663` | `1e15\|1e16\|1e100\|1e-5\|0.6666666666666666` | = V2 |
| S2 | `SELECT count(DISTINCT x) FROM d` with rows 1, 1.0, '1', 2, 2.5, NULL | 4 | 5 (1 and 1.0 counted separately) | known |
| S3 | `SELECT k, sum(v) FROM g GROUP BY k` (no ORDER BY) | sorted (`a, b, c`) | first-seen order (`b, a, c`) | known; legal but different |
| S4 | `SELECT value FROM generate_series(1,5) WHERE value < '2'` | `1` | `1,2,3,4,5` | known |
| S5 | `INSERT OR FAIL INTO rt VALUES(1,1,2),(2,3,4),(1,5,6),(3,7,8)` (rtree) | rows 1, 2 stay | no row stays | known |
| S6 | `INSERT INTO rt(a,b) VALUES(3,4)` after id = 2^63−1 exists | random id | −9223372036854775808 | known |
| S7 | `total_changes()` after writes to a virtual table | counts shadow-table writes (rtree insert: 4, fts5 insert: +10) | counts 1 per row | new |
| S8 | `hex(text)` on a UTF-16 database | UTF-16 bytes | UTF-8 bytes | new |
| S9 | `PRAGMA secure_delete`, `analysis_limit`, `data_version`, `cell_size_check`, `mmap_size` (get or set) | one row | **no row** (silently ignored) | new; U3, D4 |
| S10 | `PRAGMA incremental_vacuum(N)` | one (empty) row per freed page | no rows | new, CLI-visible only |
| S11 | `PRAGMA table_list` | hash order | schema order | new |
| S12 | error text on stderr | `Error near line N: ...` / `Parse error near line N: ...` | `Error: ...`; different parse-error wording for some inputs (`unrecognized token` vs `near "sq": syntax error`) | CLI |
| S13 | `sum()` overflow | `integer overflow` in the three orders tested | same | no divergence seen in the 3 orders tested |
| S14 | CLI default | `SQLITE_DBCONFIG_DEFENSIVE` on (`journal_mode=OFF`, `schema_version=N` refused) | off | CLI |
| S15 | `-0.0` | no difference in the expressions tested (`-0.0=0.0`, `typeof`, `min/max`, CAST, quote) | same | not reproduced |

### 2.10 R-Tree (agent `rtree_wt`; raw numbers `S5/deney_rtree.md`, notes `S5/rtree_wt/PROGRESS.md`)

Finding that changes the picture of known class h: **the shadow-table CONTENT differs because the tree algorithm differs, not because of
write timing.** Writing every dirty node at the end of each statement with the current Rust algorithm (`WRITE_THROUGH=1`) leaves
`b_node`/`b_rowid`/`b_parent` at L6; a C-faithful port of the algorithm alone (`C_ALGO=1`, writes still at commit) makes the three shadow
tables' rows byte-identical to C on s12, s13 and 7 further scenarios (incl. rtree_i32, aux columns, 3-D/5-D, savepoints, statement aborts).

| id | L | where | C (`ext/rtree/rtree.c`) | Rust (`ext rtree.rs`) | effect | C↔R | SQL-visible | healthier | align / speed |
|---|---|---|---|---|---|---|---|---|---|
| **R1** node numbering at a split | L6 | 52nd insert (first root split; n03 #53) | `SplitNode`: `nodeWrite(pRight)` before `nodeWrite(pLeft)` → right half = node 2, left = node 3 | `insert_into_path`: left = 2, right = 3 | `b_node` rows, `b_rowid.nodeno`, `b_parent` | ok / ok (`rtreecheck` ok) | `SELECT nodeno, data FROM b_node`, `rtreenode()` | equal | part of the C-path port (prototype: `c_split_node`, `c_split_startree`, `c_sort_by_dimension`) |
| **R2** reinsertion order after `removeNode` | L6, **node count** | delete/update that underflows a leaf (s12 line 999: `UPDATE id=452`, 16 cells reinserted) | `reinsertNodeContent` walks cells 0..n of each removed node, last-removed node first | `delete_rowid_entry`: `orphaned.into_iter().rev()` reverses the cells inside each node | different tree: s12 **147 nodes vs 152** (this is the known node-count difference; with the port 152/152) | ok / ok | node count, bbox query cost | equal (Rust's tree happened to be 3% smaller here) | ~1 line for the order; full row-identity needs the C path |
| **R3** stale bytes inside node blobs | L6 (row bytes) | `DELETE FROM b WHERE id=20` (s12 #52) | `nodeDeleteCell` leaves the old last cell's bytes after `nCell` | `encode_node_cells` zero-fills | `hex(data)` of `b_node` rows | ok / ok | `SELECT hex(data) FROM b_node` | Rust | reproduced in the prototype with a side map of stale bytes |
| **R4** when nodes are written (known h) | L4–L5 | every statement inside a transaction | `nodeRelease` → `nodeWrite` when the reference count drops to 0 (inside the statement) | `RTreeCache` keeps nodes + undo log, `flush_dirty` writes each dirty node once at xSync in node-number order | page layout of the shadow tables (Rust: sequential, fewer pages: s13 79 vs 81, s12 65 vs 73); **memory unbounded** (below); with `C_ALGO` + commit-time writes `b_node` reaches L3, with eager writes in C order L2 | ok / ok | `dbstat`, RSS | C for memory bound and for "a later statement in the txn sees shadow rows"; Rust for I/O | prototype `WRITE_THROUGH=2`; **W12 +77.5% (`=1`) / +81% (`=2`) (m)** |
| **R5** `flush_dirty` walks the whole cache at every commit | – | autocommit inserts | – | `flush_dirty` iterates all cached nodes | **1e5 autocommit inserts: 47.3 s / 163 G instructions vs C 8.8 s / 5.1 G** (m); with write-through 11.7 s / 9.9 G | – | time | – | a dirty list fixes it without changing behaviour; **speed + 4× on autocommit R-Tree loads** |
| **R6** multi-row `DELETE` order | L6 | `DELETE FROM b WHERE id < 100` | xUpdate called in ascending rowid order (RowSet) | scan order | different tree after multi-row deletes | ok / ok | node rows | equal | engine-level (delete two-pass order) |
| **R7** `-0.0` coordinate | L6 | `INSERT INTO b VALUES(1,-0.0,1,0,1)` | stores 0x80000000 | stores +0.0 (sign lost before xUpdate) | 4 bytes | ok | `rtreenode()` | C | small |
| – | | already known | `INSERT OR FAIL` multi-row (S5), auto rowid after 2^63−1 (S6), `total_changes()` (S7), sqlite_schema row order (D2) | | | | | | |

With `WRITE_THROUGH=2` what remains between the Rust and the C R-Tree file is B-tree level only (B1 delete rebalance, B10 in-page placement)
plus D2: replaying the Rust shadow-table operation trace through the C engine gives shadow pages byte-identical to the real C file
(s12 0/72, s13 0/80 pages differ).

**Addendum from the R-Tree agent's final written report (arrived after the tables above were written; numbers unchanged):**
* further module-level differences: split sort tie order (`SortByDimension` takes the right run on ties → equal keys come out in reverse
  input order; Rust `sort_cell_indexes_by_dim` is stable) — changes the tree in scenarios with ties (a5, a6); `AdjustTree` dirties an
  ancestor only when its cell changes (Rust dirties every ancestor: write counts only); `SplitNode` rewrites mappings of the right node only
  (Rust also the left: redundant); f32 vs f64 `cellArea` arithmetic (code only, no effect observed); corrupt-node error text; a debug build
  panics (and poisons the cache mutex) on a NULL rowid after 2^63−1 where release wraps; `SQLITE_LOCKED_VTAB` not modelled (code only).
* the reinsert-order fix alone (R2, ~10 lines in `delete_rowid_entry`) restores C's node count; the full C tree path is ≈550 lines in / 450 out.
* most of the write-through cost is per-call overhead of `shadow_put_rowid_direct` (schema lookup by name, `Value` clones incl. the 1.2 KB
  blob, record encode) — a dedicated shadow write path should bring it near C's total (C does the same write-through in 154.6 M instructions).
* knob-off residual not resolved: +0.75% instructions on the mixed 1e5 load (8.3066 G vs 8.2445 G) although the knob-off source path is
  unchanged (attributed to LTO inlining, not profiled); every other load within ±0.2%.
* SQLite's own `ext/rtree/*.test` files through the repo's `tclsuite`: identical per-file pass/fail counts in all knob modes.
* not done there: WAL mode / multi-connection behaviour with write-through, crash recovery, 1e6 autocommit.

## 3. Experiments (patches in `S5/patches/`, all behind environment variables, default off = HEAD; `git apply --check` passes for each of the four base patches; `btree_c_insert_alloc_extra.patch` applies on top of the two b-tree patches)

### 3.1 R-Tree write-through (`rtree_write_through.patch`, `bin_rtree_wt/`)

W12 (`ab.py --trials 9 --filter W12`, base = `step5b_bin`, cpu 6), verbatim:
```
### KNOB=off
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.01     24.15     1.006x    23.77    23.58    24.63    0.98x  OK
### SQLITERS_RTREE_WRITE_THROUGH=1
W12_rtree_insert_and_bbox        23.95     42.50     1.775x    23.83    41.51    24.63    1.73x  OK
### SQLITERS_RTREE_WRITE_THROUGH=2
W12_rtree_insert_and_bbox        24.10     43.63     1.810x    23.63    42.63    24.63    1.77x  OK
### SQLITERS_RTREE_C_ALGO=1
W12_rtree_insert_and_bbox        23.85     28.08     1.177x    23.73    27.63    24.63    1.14x  OK
### SQLITERS_RTREE_WRITE_THROUGH=1 SQLITERS_RTREE_CACHE_MAX=0
W12_rtree_insert_and_bbox        24.01     57.94     2.413x    23.85    57.52    24.63    2.35x  OK
### SQLITERS_RTREE_WRITE_THROUGH=2 SQLITERS_RTREE_CACHE_MAX=0
W12_rtree_insert_and_bbox        23.96     67.87     2.832x    23.73    65.63    24.63    2.76x  OK
```
`instructions:u` W12 (final build): HEAD 145.84 M; knob off 145.98 M (+0.09%); `WRITE_THROUGH=1` 229.17 M (+57%); `=2` 229.95 M;
`C_ALGO=1` 164.09 M (+12.5%); `WRITE_THROUGH=1 CACHE_MAX=0` 329.16 M; `=2 CACHE_MAX=0` 375.21 M; C twin 154.65 M.
Knob off: database file and stdout byte-identical to HEAD on 27/27 scripts.

Larger loads through the CLIs (on-disk tmpfs database; wall s / `instructions:u` / peak RSS / pages / nodes):

| load | Rust HEAD | `WRITE_THROUGH=1` | `=1`, `CACHE_MAX=0` | `WRITE_THROUGH=2` | C |
|---|---|---|---|---|---|
| 1e5 inserts, one txn | 0.97 s / 5.02 G / **28.6 MB** / 1275 / 2985 | 1.60 s / 6.99 G / 28.9 MB / 1312 | 2.13 s / 9.97 G / 13.6 MB / 1312 | 1.56 s / 6.82 G / 29.7 MB / 1313 | 0.93 s / 4.01 G / **6.5 MB** / 1312 / 2985 |
| 1e6 inserts, one txn | 10.8 s / 51.2 G / **266 MB** / 12802 / 29932 | 17.8 s / 70.9 G / 308 MB / 13183 | 24.0 s / 107.8 G / 62 MB / 13183 | 18.2 s / 68.2 G / 319 MB / 13191 | 14.9 s (11.9 user) / 45.2 G / **6.9 MB** / 13166 / 29932 |
| 1e6 rows, one `INSERT..SELECT` | 13.9 s / 75.9 G / **1 091 MB** | 17.4 s / 94.1 G / 539 MB | 22.0 s / 130.7 G / 428 MB | 16.5 s / 92.1 G / 539 MB | 11.9 s / 35.4 G / **6.8 MB** |
| mixed ins/del/upd 1e5 ops, one txn | 2.02 s / 8.24 G / 20.7 MB / 398 pages / 933 nodes | 2.80 s / 10.4 G / 17.2 MB / 534 | 3.59 s / 14.6 G / 10.0 MB | 2.92 s / 11.1 G / 18.3 MB / 533 / 931 | 1.24 s / 5.71 G / 6.5 MB / 503 / 931 |
| 1e5 inserts, autocommit | **47.3 s / 163.4 G** / 28.1 MB | 11.7 s / 9.90 G / 28.2 MB | 12.6 s / 12.9 G / 13.6 MB | 11.2 s / 9.57 G | 8.8 s / 5.06 G / 6.9 MB |

* **Memory**: confirmed unbounded at HEAD — the node cache + undo log grow with the transaction (266 MB for 1e6 rows; C 6.9 MB, bounded by
  its page cache). Write-through removes the undo log but keeps the read cache (308 MB); bounding the read cache (`CACHE_MAX`) brings it to
  62–83 MB at a further 14–52% of instructions. The remaining 62 MB at `CACHE_MAX=0` is the pager not spilling in rollback mode (P1:
  the database is 54 MB). The 1 GB for `INSERT..SELECT` is mostly the materialised SELECT (class X2 below), not the R-Tree.
* **Closer to C?** `WRITE_THROUGH=1` alone: no (L6, different node rows). `C_ALGO=1`: shadow rows identical, pages L3–L5.
  `WRITE_THROUGH=2`: `b_node` L2 on insert/update scenarios (s13 62/62 pages, file 81/81 pages), L5 with deletes (B1). Whole-file layer
  stays L5 until D2 is fixed. Per-tree layers: `S5/agac_katman.py`.

### 3.2 Delete-side rebalance and CREATE INDEX fill (`btree_delete_rebalance.patch`, `btree_index_bulk_fill.patch`, optional
`btree_c_insert_alloc_extra.patch`; `bin_btree/`, `bin_btree_extra/`) — full tables in `S5/deney_btree.md`

```
### W08 knobs OFF
W08_indexed_update_delete         9.61      9.66     1.005x     9.56     9.58     7.96    1.21x  OK
### W08 SQLITERS_BTREE_C_DELETE_BALANCE=1
W08_indexed_update_delete         9.62     10.15     1.054x     9.57    10.05     7.96    1.28x  OK
### W08 SQLITERS_INDEX_BULK_FILL=1
W08_indexed_update_delete         9.62      9.97     1.036x     9.57     9.83     7.96    1.25x  OK
### W08 both knobs
W08_indexed_update_delete         9.64     10.33     1.072x     9.60    10.26     7.96    1.30x  OK
### W02 knobs OFF
W02_create_indexes                7.39      7.46     1.010x     7.35     7.41     8.76    0.85x  OK
### W02 SQLITERS_BTREE_C_DELETE_BALANCE=1
W02_create_indexes                7.39      7.42     1.005x     7.33     7.41     8.76    0.85x  OK
### W02 SQLITERS_INDEX_BULK_FILL=1
W02_create_indexes                7.39      7.81     1.056x     7.36     7.73     8.76    0.89x  OK
### W02 both knobs
W02_create_indexes                7.38      7.80     1.057x     7.34     7.74     8.76    0.89x  OK
### W14 knobs OFF
W14_cli_sql_script               34.44     35.28     1.024x    34.28    34.84    26.19    1.35x  OK
### W14 SQLITERS_BTREE_C_DELETE_BALANCE=1
W14_cli_sql_script               34.41     35.01     1.018x    34.29    34.74    26.19    1.34x  OK
### W14 SQLITERS_INDEX_BULK_FILL=1
W14_cli_sql_script               34.37     35.13     1.022x    34.09    34.83    26.19    1.34x  OK
### W14 both knobs
W14_cli_sql_script               34.54     35.07     1.015x    34.25    34.74    26.19    1.34x  OK

### FULL both knobs (SQLITERS_BTREE_C_DELETE_BALANCE=1 SQLITERS_INDEX_BULK_FILL=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.53      7.47     0.992x     7.50     7.43     9.60    0.78x  OK
W02_create_indexes                7.50      7.86     1.048x     7.45     7.79     8.76    0.90x  OK
W03_pk_point_select              28.15     27.85     0.990x    27.89    27.53    23.30    1.20x  OK
W04_secondary_idx_range           7.80      7.76     0.995x     7.74     7.69     7.46    1.04x  OK
W05_full_scan_filter_agg          4.11      4.39     1.068x     4.09     4.37    16.57    0.27x  OK
W06_join_group_order             10.67     10.78     1.010x    10.66    10.73    28.23    0.38x  OK
W07_window_functions             38.98     39.45     1.012x    38.67    39.28    50.21    0.79x  OK
W08_indexed_update_delete         9.45     10.14     1.074x     9.41    10.08     7.96    1.27x  OK
W09_like_text_scan               11.98     12.09     1.009x    11.95    12.07    18.92    0.64x  OK
W10_jsonb_extract_agg            11.12     11.10     0.998x    11.02    11.04    10.93    1.01x  OK
W11_fts5_index_and_bm25          45.49     45.41     0.998x    45.05    45.04    57.02    0.80x  OK
W12_rtree_insert_and_bbox        23.53     23.34     0.992x    23.12    22.85    24.63    0.95x  OK
W13_autocommit_wal_txns           4.87      4.83     0.992x     4.69     4.53     4.12    1.17x  OK
W14_cli_sql_script               35.51     36.04     1.015x    35.22    35.70    26.19    1.38x  OK
TOTAL                           246.69    248.51     1.007x   geomean cand/base = 1.013x   digest mismatches = 0
```
(W05 +7% and W14 +2% appear with the knobs OFF as well: code layout of that build; instructions +0.03% / −0.001%.)

`instructions:u` (whole process incl. W01+W02 setup): W02 HEAD 119.33 M, knobs off 119.37 M, BULK 122.11 M, C 129.30 M;
W08 HEAD 178.34 M, off 178.43 M, DELETE 181.14 M, BULK 183.32 M, both 185.78 M, all four knobs 194.38 M, C 170.19 M;
W14 HEAD 198.32 M, both 198.56 M, all four 222.69 M, C 135.75 M. W08 alone = 59.06 M: DELETE +2.72 M (+4.6%), BULK +2.15 M (+3.6%).

Health scenarios, pages (off / DELETE / BULK / both / all four / C): s03 259/259/259/259/**260**/260; s05 490/490/458/458/458/458 (depth 2→1);
s06 296/251/296/251/**249**/249; s07 2456/2214/2559/2214/**2173**/2173; s08 1182/986/1181/986/**972**/972; s09 281/244/281/244/244/244;
s11 1601/…/**1599**/1599. All knob-on files: C `integrity_check` ok, logical equality, deterministic. Fuzz (step-5b generators, 5 knob sets ×
400 runs): 0 failures, every file C-valid. 2 of 23 b-tree unit tests fail with the delete knob (they assert the step-5b shape on
hand-built trees). The extra patch (insert-side port on parsed pages) is a **shape prototype, not a speed model**: full run TOTAL 1.074×,
W08 1.206×, W10 1.967× (≈68 µs per 3-leaf index rebalance; the existing raw path does the same operation in a few µs).

### 3.3 WAL frame batching (`wal_no_batch.patch`, `bin_wal/`) — full tables in `S5/deney_wal.md`

```
== W13 knob OFF
W13_autocommit_wal_txns           5.32      5.25     0.987x     5.18     5.17     4.12    1.28x  OK
== W13 knob ON (SQLITERS_WAL_NO_BATCH=1)
W13_autocommit_wal_txns           5.30      6.08     1.148x     5.20     6.00     4.12    1.48x  OK
== W01 knob OFF
W01_bulk_insert_prepared          7.45      7.33     0.985x     7.42     7.32     9.60    0.76x  OK
== W01 knob ON (SQLITERS_WAL_NO_BATCH=1)
W01_bulk_insert_prepared          7.45      7.50     1.007x     7.41     7.48     9.60    0.78x  OK
```
W13 `instructions:u` 11.705 M off / 11.441 M on (user goes DOWN 2.3%); kernel instructions 11.75 M off / 13.56 M on (+15%); `pwrite64` 320 / 1 236
(= C's 1 236). Real disk (btrfs, `synchronous=NORMAL`): W13 16.50 ms off / 22.37 ms on / C 19.57 ms. Verdict: nothing observable differs
with batching on (W0 in §2.6); the separate crash hole W1 is not caused by batching.

### 3.4 Other measurements made for this inventory

| what | Rust | C | class |
|---|---|---|---|
| `DROP TABLE` of a 3 509-page table: `pwrite64` / `instructions:u` | 7 022 / 112.6 M | 25 / 17.2 M | U4 |
| `DELETE FROM t` (same table) | 7 022 / 472.2 M | 26 / 17.2 M | U4 |
| `DELETE FROM t WHERE id > 0` (200 000 rows) | 7 022 / **4 598.9 M** | 10 586 / 257.3 M | X1 |
| 1e6 single-row INSERTs in one txn, `cache_size=100`, rollback journal: time / RSS | 1.15 s / **78 MB** | 2.60 s / 5 MB | P1 |
| same, WAL | 1.41 s / 8.2 MB | 2.74 s / 4.6 MB | W3 |
| recursive CTE, 1e6 rows, `SELECT count(*), sum(x)` | **6.07 s / 420 MB** | 0.28 s / 4.7 MB | X2 |
| `INSERT INTO t SELECT … FROM generate_series(1,1e6)` | 2.44 s / 382 MB | 0.48 s / 6.9 MB | X2 |
| 100 000 rows × 3 REAL columns with integral values: file size | 3 534 848 | 1 531 904 | V1 |
| fsync count, 4 small txns, `synchronous=FULL`: DELETE / TRUNCATE / PERSIST | 16 / 17 / 17 | 16 / 20 / 20 | P4 |

**X1/X2 are not file divergences** but structural differences found on the way: X1 a range `DELETE` costs 23 000 instructions per row
(C 1 300); X2 recursive CTEs and the SELECT side of `INSERT..SELECT` are fully materialised (time ×5–21, memory O(rows)) where C streams.
Also seen: `substr('NEW',-6)` returns `''` (C `'NEW'`); `substr(blob,…)` returns lossy TEXT; `SELECT … WHERE id IN (…)` on an rtree
returns rows in a different order; EQP text for vtab scans lacks `VIRTUAL TABLE INDEX n:str`.

## 4. Ranked recommendation

**The oracle.** "Same statement sequence → same file as C" is reachable at **L2** (identical except unused bytes), not at L0: C's unused
bytes are the residue of its exact memcpy history (U1) and its WAL salts / random rowids are random by design. Recommended gate for later
steps: `ilk_ayrisma.py --require L2` on the scenario sets (today 20% pass; 0% at L0 only because of H1).

### 4.1 Fix regardless of the oracle — defects and skipped guarantees found on the way (no speed cost)

| # | class | why | size |
|---|---|---|---|
| 1 | **W1** WAL: frames appended after an in-place rewrite carry a valid checksum | crash hole C does not have (half-applied transaction reproduced) | ~6 lines |
| 2 | **F6** FTS5 reader drops postings flagged bDel | wrong MATCH results on C-written files after any UPDATE; `'optimize'` destroys them | ~10 lines |
| 3 | **F4b, F7, F11, F13, F16** FTS5 files that C rejects (64 KiB leaf, `contentless_delete` format, tokenizer tables, unimplemented options) | cross-engine corruption | 20 – few hundred lines each |
| 4 | **U3** `PRAGMA secure_delete` silently ignored (+ S9: five pragmas return no row) | a requested guarantee is skipped without an error | ~60 lines |
| 5 | **D1** `ALTER TABLE ADD COLUMN` (blob default lost, non-constant default accepted, text rewritten) | schema text / default values differ from C | small-medium |
| 6 | **H2** encoding frozen by `journal_mode`/`auto_vacuum`/`user_version` on an empty database | UTF-8 database where C makes UTF-16 | small |
| 7 | **H4/W2** file modes (0666 & ~umask, side files do not inherit the db mode, no `O_NOFOLLOW`) | `-wal` of a 0600 database is world-readable | ~30 lines |
| 8 | **V2** real → text formatting | stored text and every printed real differ; makes C reject FTS5 files (F16) | ~200 lines |
| 9 | **F12**, **S5–S8**, `substr` negative start, `substr(blob)` | wrong results / errors | small each |

### 4.2 Align with C first — cheap, deterministic, no measurable speed cost (moves scenarios toward L2)

| # | class | effect on the layer reach | speed |
|---|---|---|---|
| 1 | **H1** change counter starts at 1 (+ W5) | makes L0 possible at all; every L1 scenario becomes byte-identical | 0 |
| 2 | **H3** schema cookie once per DDL statement | removes the L3 cap on every script with UNIQUE/PK/AUTOINCREMENT/VACUUM/ALTER | 0 |
| 3 | **D2** vtab row first in sqlite_schema, **D3** stat1 order | removes the L5/L6 cap on every R-Tree/FTS5/ANALYZE script | 0 |
| 4 | **V1** integer storage for REAL-affinity values, **V3** JSONB TEXTRAW | L6 → L2 for those tables; files up to 2.3× smaller (V1) | ≥0 |
| 5 | **U4** do not journal/zero/rewrite freed pages | none on the layer (unused bytes), **6.5–27× fewer instructions for DROP TABLE / DELETE-all (m)** | **gain** |
| 6 | **B7/B8/B13** freelist order (`C_ALLOC` prototype) and **B9** index-before-table write order | L4 → L3 for overflow / freelist / auto_vacuum scenarios | 0 (e) |
| 7 | **B3/B12** bulk-load packing for CREATE INDEX / REINDEX / VACUUM | s05, f02, f11, l01, v03, v06: L5 → L4 (identical to C incl. page numbers where the base is identical); indexes 6.5% smaller, one level shallower | **W02 +5.6% (m)**, estimate +2–3% tuned; still ≈10% faster than C |
| 8 | **B4/B5/B6/B14** insert-side `balance_nonroot` (3-sibling redistribution, byte-based split, interior packing) | with B1+B3+B7: **all nine b-tree health scenarios shape-identical with equal file size; ≥L4 50% → 71%** | raw implementation estimated ≈0 (e; the prototype on parsed pages is not a speed model) |
| 9 | **R1/R2/R3/R6** C-faithful R-Tree algorithm with commit-time writes kept | shadow-table rows byte-identical to C | **W12 +17.7% as prototyped (m, +12.5% instructions)**; tuned cost not measured |
| 10 | **P2** journal sector 512, **W4** partial checkpoint | journal layout; less checkpoint I/O | 0 / small gain |

### 4.3 Needs a decision — alignment costs speed or gives up a property

| class | what C-alignment buys | price (m = measured, e = estimate) |
|---|---|---|
| **B1** delete-side rebalance (+B2, B11) | files 13–22% smaller after churn (s06–s09), table fill 49% → 64%, same shape as C | **W08 +5.4% wall, +4.6% instructions (m)**; ≤ +2% with a cached `nFree` (e). W14 no change (m). Risk medium-high (touches every delete path; 2 step-5b unit tests assert the old shape) |
| **R4** R-Tree write-through | bounded memory (266 MB → ≈62 MB for 1e6 rows with a bounded read cache; C 6.9 MB), page-identical shadow tables, later statements see flushed shadow rows | **W12 +77.5% (`=1`) / +81% (`=2`) (m)**; +141% / +183% with the read cache bounded like C (m). Autocommit loads get 4× FASTER (R5) |
| **P1/W3** page-cache spill like `pagerStress` | memory bounded by `cache_size` (78 MB → ≈5 MB for a 72 MB transaction; 138 MB → 4.6 MB within one statement in WAL) | not prototyped; extra journal fsync + header segment per spill on real disks (e), risk medium-high |
| **B10** in-page cell placement like `allocateSpace`/`defragmentPage` | the only way from L4 to L2 for any page that saw a delete, an update or a split | 600–800 lines constraining every raw in-place path; gives up clean, zero-gap rebuilt pages; speed not measured |
| **W0** WAL batching off | nothing observable | **W13 +14.8% tmpfs, +36% btrfs NORMAL, W01 +0.7% (m)** → recommend keeping the batching |
| **P3/P4** journal record in 3 writes, directory fsync every transaction in TRUNCATE/PERSIST | nothing observable; no durability gain | ≈2 syscalls per journalled page; 1 fsync per transaction (e) → recommend keeping |
| **F2/F3** FTS5 merge schedule and flush triggers | same segment structure as C; removes the O(index) rewrites | 1 100–1 700 lines, high risk; **large gain** on autocommit / update loads (7.3 s → ≈0.6 s class), ≈0 on W11 |
| **S3** GROUP BY output order without ORDER BY | same row order as C | hash aggregation would need an ordered emit; cost not measured |

### 4.4 Keep as documented, deliberate differences

U1 (zero-filled gaps of rebuilt pages; masked by L2), U2, W0, P3, P4, P5, W6/W7, V4 (random rowid: C itself is not deterministic), S12 (CLI error
prefix), S14 (CLI defensive default — or adopt it in the CLI).

## 5. What could not be determined

* **Library API vs CLI**: the finder drives both engines through their CLIs. The Rust engine has at least three insert paths with
  different write orders (compiled VDBE path: indexes first; single-row plan and general path: table first — B9). Whether the same
  statements issued through prepared statements produce the same file as through the CLI was **not** tested.
* B-tree: which single insert-side decision (B4, B5 or B6) accounts for each difference in s03/s04 (only their sum, via the port); the cause
  of every remaining page-number difference with all knobs in s07 (378 pages), s08 (41), s11 (1 185) — attributed to B9 on samples only.
* Cost of B10 (in-page placement), of a tuned C-path R-Tree, of a raw insert-side port, and of rollback-mode spilling: estimates only.
* `ANALYZE`: order of TABLES in sqlite_stat1 versus C's schema-hash order; `sqlite_stat4`. D5 (AUTOINCREMENT double write) is from code reading.
* WAL: no real power-loss test, no concurrent-writer stress, no multi-region `-shm` (> 4 062 frames), `synchronous=EXTRA` and the
  sector-padding path only read, not exercised.
* FTS5: interop with a C merge in progress, Rust reading C tombstone pages, `tokendata=1`, multi-level doclist index, W11 phase split with
  prepared statements (CLI emulation only); all FTS5 alignment sizes are estimates.
* R-Tree: the tuned cost of the C algorithm; a "deferred writes with a bounded cache" middle path was not prototyped.
* Not run at all: page sizes 2048/16384/32768, reserved bytes ≠ 0, databases > 1 GiB (lock-byte page), ATTACH/temp/in-memory files,
  `VACUUM INTO` output file (scenario g06 is a harness artefact), backup API, incremental blob I/O, multi-process access, non-Linux.
* The full workspace test suite was not run on any knob build (crate unit tests and fuzzers only).
