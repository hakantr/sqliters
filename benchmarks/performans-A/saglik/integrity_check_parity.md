# `PRAGMA integrity_check` parity: C SQLite 3.54.1 vs sqliters (step 5b)

Reference: `sqlite/src/btree.c` (`sqlite3BtreeIntegrityCheck`, `checkTreePage`, `checkList`, `checkRef`, `checkPtrmap`,
`btreeInitPage`, `btreeComputeFreeSpace`) and `sqlite/src/pragma.c` (`PragTyp_INTEGRITY_CHECK`), `vdbe.c`
(`OP_IntegrityCk`, `op_column_corrupt`).
Rust: `crates/sqliters-btree/src/integrity.rs` (new in this step; replaces the old `Btree::integrity_check` body in
`lib.rs`) and `exec_integrity_check` / `integrity_check_table_rows` in `crates/sqliters-core/src/engine.rs`.

Columns: **before** = branch `performans-A` @ 700ded4 (step 5), **now** = working tree after step 5b.
`yes` = same check, same message text; `different` = the condition was (partly) detected but reported with another
text / at another point / with other semantics; `no` = not checked.

## 1. B-tree level (btree.c)

| # | Check | C message format | before | now |
|---|-------|------------------|--------|-----|
| B1 | lock-byte (PENDING_BYTE) page is pre-marked as used, any reference to it is a double reference | `2nd reference to page %u` | different (`<label> references lock-byte page N`) | yes in the PRAGMA; the legacy `Btree::integrity_check()` API appends ` (lock-byte page N)` because `tests/phase1_pager_journal_modes.rs` (not editable) asserts that wording |
| B2 | `checkRef`: page number 0 or beyond the end of the file | `invalid page number %u` | different (`<label> references out-of-range page N`, `overflow page N out of range`) | yes |
| B3 | `checkRef`: page referenced twice (trees, overflow chains, freelist) | `2nd reference to page %u` | different (`2nd reference to page N (<label>)`, `2nd reference to overflow page N`) | yes |
| B4 | `checkPtrmap`: pointer-map entry unreadable | `Failed to read ptrmap key=%u` | yes (not for page numbers < 2) | yes |
| B5 | `checkPtrmap`: entry must be (type, parent) for freelist trunk + leaf pages (FREEPAGE), tree roots (ROOTPAGE), child pages (BTREE), first overflow page (OVERFLOW1), following overflow pages (OVERFLOW2) | `Bad ptr map entry key=%u expected=(%u,%u) got=(%u,%u)` | yes | yes |
| B6 | `checkList`: page of a freelist / overflow list cannot be fetched | `failed to get page %u` | no (I/O error aborted the pragma) | yes |
| B7 | freelist trunk leaf count larger than a page can hold | `Freelist: freelist leaf count too big on page %u` | different (`freelist trunk N leaf count corrupt`, tested per leaf offset) | yes |
| B8 | freelist length vs header count | `Freelist: size is %u but should be %u` | different (`freelist_count mismatch: header says X, found Y`) | yes |
| B9 | overflow chain length vs payload size | `overflow list length is %u but should be %u` | **no** (chain was only walked) | yes |
| B10 | auto-vacuum: largest root page in header vs schema | `max rootpage (%u) disagrees with header (%u)` | yes | yes |
| B11 | incremental-vacuum flag without auto-vacuum | `incremental_vacuum enabled with a max rootpage of zero` | yes | yes |
| B12 | tree page cannot be fetched | `unable to get the page. error code=%d` | no (error aborted the pragma) | yes (prints our extended error code) |
| B13 | tree deeper than `BTCURSOR_MAX_DEPTH` (20) | `btree depth exceeds %d` | **no** | yes |
| B14 | `btreeInitPage`: invalid page-type byte, cell count > `MX_CELL` | `btreeInitPage() returns error code %d` | different (`page N: <parser error text>`) | yes |
| B15 | `btreeComputeFreeSpace`: first freeblock inside the header/pointer area, freeblock past the page, size < 4, chain not ascending, last block past the end, free total out of range | `free space corruption` | **no** | yes |
| B16 | cell pointer outside the content area | `Offset %u out of range %u..%u` | different (parser error, other text; not all cases) | yes |
| B17 | cell runs past the end of the page | `Extends off end of page` | different (parser error) | yes |
| B18 | first payload byte (record header size) < 2 | `Bad cell header size` | **no** | yes |
| B19 | integer keys ascending inside a page **and** across pages (every key must be below the divider / right neighbour's minimum) | `Rowid %lld out of order` | different (`rowids out of order on table leaf N` / `keys out of order on table interior N`; only within one page) | yes |
| B20 | all children of an interior page have the same depth | `Child page depth differs` | **no** (this is the check that hid the defect) | yes |
| B21 | cells and freeblocks must not overlap | `Multiple uses for byte %u of page %u` | **no** | yes |
| B22 | fragmented-byte count in the header vs the gaps actually found | `Fragmentation of %u bytes reported as %u on page %u` | **no** | yes |
| B23 | every page is used by a tree, an overflow chain, the freelist, or is a pointer-map / lock-byte page | `Page %u: never used` | different (`Page N is never used`) | yes |
| B24 | a pointer-map page must not be referenced | `Page %u: pointer map referenced` | yes (reported when met, not in the final pass) | yes |
| B25 | message prefixes | `Freelist: ` / `Tree %u page %u: ` / `Tree %u page %u cell %u: ` / `Tree %u page %u right child: ` | no | yes (including C's quirk that in auto-vacuum files the cells of an interior page keep the `right child` prefix) |
| B26 | stop after `mxErr` errors | — | no | yes |
| B27 | partial check (one table): freelist only when the table is the schema table, no never-used pass, no auto-vacuum root checks | — | no | yes |
| B28 | entry count per tree (`aCnt[]`), used for the index count comparison | — | no | yes |
| B29 | `sqlite3_interrupt` ends the check | (error `SQLITE_INTERRUPT`) | yes | yes |
| B30 | progress handler is invoked every `nProgressOps` steps and may abort | (error `SQLITE_INTERRUPT`) | no | **no** |
| B31 | out-of-memory inside the check (`checkOom`) | (error `SQLITE_NOMEM`) | n/a (allocation failure aborts the process) | n/a |

Order of work and of messages is C's: freelist, auto-vacuum header checks, then per root: right child first, cells from
last to first, coverage check per page after its cells; never-used pass last.

Rust-only diagnostics of the old implementation that C does not have were dropped:
`invalid incremental_vacuum value (N)` (C accepts any non-zero value) and `non-root btree page N has 0 cells`
(C has no b-tree level message; see S23 for what happens instead).

## 2. SQL level (pragma.c)

| # | Check / behaviour | C message format | before | now |
|---|-------------------|------------------|--------|-----|
| S1 | all b-tree findings of one database form ONE result row | `*** in database %s ***\n` + messages joined by `\n` | different (one row per message, no header) | yes |
| S2 | single row `ok` when nothing was found | `ok` | yes | yes |
| S3 | `integrity_check(N)` / `=N`: report at most N errors (default 100, `N<=0` -> 100); every result row uses one, a b-tree row uses one per message | — | no (number ignored) | yes |
| S4 | `integrity_check(table)`: check only that table and its indexes; unknown name is an error; a quoted number is a table name | `no such table: %s` | no (name only filtered the R-Tree check) | yes |
| S5 | `schema.integrity_check` checks one database, the unqualified form all of them | — | yes | yes |
| S6 | index entry count equals table row count (not for partial indexes) | `wrong # of entries in index %s` | **no** | yes |
| S7 | WITHOUT ROWID rows are in PRIMARY KEY order | `row not in PRIMARY KEY order for %s` | **no** | yes |
| S8 | record header of every row is parsed and must be consistent with the payload size (`OP_Column`, `op_column_corrupt`) | final row `database disk image is malformed` | **no** | yes |
| S9 | NOT NULL columns hold no NULL | `NULL value in %s.%s` | **no** | yes |
| S10 | STRICT tables: stored type matches the declared type | `non-%s value in %s.%s` | **no** | yes |
| S11 | TEXT-affinity columns of ordinary tables hold NULL / TEXT / BLOB | `NUMERIC value in %s.%s` | **no** | yes |
| S12 | numeric-affinity columns hold no TEXT that converts to a number | `TEXT value in %s.%s` | **no** | yes |
| S13 | CHECK constraints hold for every row | `CHECK constraint failed in %s` | **no** | yes (`PRAGMA ignore_check_constraints` does not exist in sqliters, so the checks are never skipped) |
| S14 | every row has its entry in every index (partial indexes: when the WHERE clause holds) | `row %d missing from index %s` | **no** | yes (an index record with fewer fields than the key is not accepted as the entry) |
| S15 | index entry differs from the row only by floating-point precision (`OP_IFindKey`) | `index %s stores an imprecise floating-point value for row %d` | no | **no** (reported as S14) |
| S16 | rowid is the last field of the index record | `rowid not at end-of-record for row %d of index %s` | no | **no** |
| S17 | indexed columns with a non-BINARY collation hold the same text as the table | `row %d values differ from index %s` | no | **no** |
| S18 | UNIQUE index holds no second entry with the same key (unless a nullable key column is NULL) | `non-unique entry in index %s` | **no** | yes |
| S19 | virtual tables: `xIntegrity` | R-Tree: `In RTree %s.%s:\n…`; FTS5: its own texts | different (R-Tree only, `integrity_check` only) | different (R-Tree for both pragmas as in C; FTS5 `xIntegrity` still **not** run) |
| S20 | `quick_check` = everything except S7, S14–S18 | — | different (same b-tree pass as `integrity_check`, no R-Tree check) | yes |
| S21 | result column is named after the pragma | `integrity_check` / `quick_check` | different (`integrity_check` for both) | yes |
| S22 | order of the rows: tables in the order of the schema hash table, indexes newest first (REPLACE indexes last) | — | no | yes for a schema as loaded from the file (`c_table_hash_order` ports `hash.c`); a connection that created/dropped tables itself may differ until the schema is reloaded |
| S23 | a tree that cannot be walked (page without cells below the root, unreadable page) fails the statement with `SQLITE_CORRUPT` after the rows already returned | error `database disk image is malformed` | no | different: the rows found so far are returned and the text is the LAST ROW; the statement itself succeeds (our pragma returns all rows at once) |
| S24 | row checks also run on the schema table itself (`sqlite_master`) | (S8–S13 texts with table `sqlite_master`) | no | **no** |
| S25 | imposter tables are skipped | — | n/a (no imposter tables) | n/a |

## 3. Counts

| | C checks | same as C before | same as C now | still missing / different |
|---|---:|---:|---:|---|
| B-tree level (B1–B30; B31 n/a) | 30 | 6 (B4, B5, B10, B11, B24, B29) + 10 different + 14 missing | 29 | B30 (progress handler) |
| SQL level (S1–S24; S25 n/a) | 24 | 2 (S2, S5) + 4 different (S1, S19, S20, S21) + 18 missing | 18 | missing: S15, S16, S17, S24; different: S19 (no FTS5 xIntegrity), S23 (error becomes last row) |

## 4. Evidence

* `step5b/icfuzz.py`: 18 C-written base files (page size 512/1024/4096 x auto_vacuum 0/1/2 x two schemas), 1-3 targeted
  byte mutations per trial, `PRAGMA integrity_check` / `quick_check` / `integrity_check(N)` run by `bin/sqlite3` and by
  the final `target/release/sqliters`, outputs compared. Final binaries, 3000 trials each:

  | run | identical output | b-tree block differs | C flags, sqliters `ok` | C `ok`, sqliters flags | only SQL-level rows differ |
  |---|---:|---:|---:|---:|---:|
  | seed 21 (`icfuzz_final2.log`) | 2124 | 17 | 4 | 3 | 859 |
  | seed 33 (`icfuzz_final3.log`) | 2103 | 15 | 1 | 4 | 882 |

  * b-tree block differences (seed 33, all 15 classified): 13 trials mutated page 1 or the file header - the engines then
    load different schemas (C usually refuses: `malformed database schema`) -, 2 are the "C ok" cases below. None is a
    difference in a b-tree level check of a tree both engines know.
  * "C flags, sqliters ok" (5 in 6000): all are page-1 corruptions of `sqlite_schema` rows that make C fail every statement
    with `malformed database schema (...)` while sqliters still loads a schema and checks what it loaded. This is a gap of
    the schema loader, not of the integrity check, and it is NOT fixed here.
  * "C ok, sqliters flags" (7 in 6000): records whose header size is encoded as zero (C accepts them "for historical
    reasons"), a record with a damaged field behind the last table column (C parses only up to that column, sqliters
    decodes the whole record), one page-1 case.
  * SQL-level rows differ on ~29% of the corrupted files: which key a cursor reads from a garbage cell, and whether it gives
    up, is implementation specific; and S23 applies (C: rows + error, sqliters: rows + final `database disk image is
    malformed` row).
* Hand-built cases with C as the oracle (`step5b/sqlchk.sql`, the `pragma-3.2` scenario of `sqlite/test/pragma.test`, schemas
  of 4..140 tables for the row order): identical output for S1-S14, S18, S20-S22; frozen in
  `crates/sqliters-core/src/lib.rs::tests::integrity_check_reports_what_c_sqlite_reports`.
* The three unbalanced reproducers written by the step-5 build: both engines print the same row,
  e.g. `*** in database main ***` / `Tree 2 page 2 cell 1: Child page depth differs`.
* Health scenarios (13 files written by sqliters, 13 by C): the new check says `ok` for all 26.

## 5. Cost

W14 database (30 pages, 1950 rows, one index), `instructions:u` of the CLI minus process start:
step 5 check 2.0 M, C 5.4 M, new check 14.4 M (20.1 M before the integer-probe / leaf-view lookup was used).
`quick_check`: C 2.3 M, new 8.6 M.
