# Decision parity, step M3c (transaction opcodes, virtual-table reads, `max_page_count`)

C = SQLite 3.54.1 (`sqlite/src/*.c`, line numbers of the source tree; the gcov runs use the amalgamation).
Rust = the worktree `performans-A-vdbe-makine` (uncommitted), switch `SQLITERS_VDBE_EXEC=1`.
`vdbe.rs` = `crates/sqliters-vdbe/src/vdbe.rs`, `host` = `crates/sqliters-core/src/vdbe_host.rs`.

Evidence per row: the corpora of `PROGRESS.md` (tx = `m3c/ops_tx` 400 API scripts + CLI form, ddl = `m3c/ops_ddl`,
vt = V4b vtab corpora through `rowdiff.py`, mpc = 200 `max_page_count` CLI scripts) and the gcov counts of
`covm3c/coverage.md` (section "Coverage" at the end). "taken" = the C outcome occurs in the corpora; "test" =
covered by a unit test of `c_align_m3c_tests.rs`.

## 1. `OP_Savepoint` (vdbe.c 3978-4157) — Rust `vdbe.rs` arm `OP_Savepoint`

| # | C condition (file:line) | outcome | Rust place | evidence |
|---|---|---|---|---|
| S1 | `p1==SAVEPOINT_BEGIN` (vdbe.c:4000) | open a savepoint | `if p1 == SAVEPOINT_BEGIN` | tx (taken) |
| S2 | `db->nVdbeWrite>0` (4001) | SQLITE_BUSY "cannot open savepoint - SQL statements in progress" | `host.n_vdbe_write() > 0` (= `activity.other_writers`) | tx: pending RETURNING writer + SAVEPOINT (taken, stdout equal) |
| S3 | `sqlite3VtabSavepoint(db, SAVEPOINT_BEGIN, nStatement+nSavepoint)` != OK (4016) | abort | `host.vtab_savepoint(SAVEPOINT_BEGIN, ..)?` → `vtab::vtab_savepoint_all(ctx, savepoints.len())` (the engine's vtab layer subtracts the transaction savepoint, `vtab_tx_savepoint_offset`) | tx with fts5/rtree in a transaction (call taken; error outcome not taken: no module fails xSavepoint) |
| S4 | `db->autoCommit` (4029) | transaction savepoint (`autoCommit=0`, `isTransactionSavepoint=1`) / `nSavepoint++` | `host.auto_commit()` → `savepoint_open(name, transaction)` (`implicit_savepoint_tx`, `SqlSavepoint.is_transaction_savepoint`) | tx (both) |
| S5 | `pNew->nDeferredCons = db->nDeferredCons` (4038) | saved counters | `SqlSavepoint.deferred_fk_suspects` (counters + engine suspects) | ddl/d4, tx FK scripts |
| S6 | `!pSavepoint` (4056) | "no such savepoint: %s" | `host.savepoint_find(name)` (newest first, `sqlite3StrICmp` = `eq_ignore_ascii_case`) | tx (taken), test |
| S7 | `db->nVdbeWrite>0 && p1==SAVEPOINT_RELEASE` (4059) | SQLITE_BUSY "cannot release savepoint - SQL statements in progress" | same condition | tx (taken) |
| S8 | `isTransaction = pSavepoint->pNext==0 && db->isTransactionSavepoint` (4072) | | `host.savepoint_is_transaction(k)` | tx |
| S9 | `isTransaction && p1==RELEASE` (4073) | commit | same | tx (taken) |
| S10 | `sqlite3VdbeCheckFkDeferred(p)!=OK` (4074) | `goto vdbe_return` (not halted, transaction open) | `host.fk_deferred_pending()` → `Err(fk_error())` with `vm.not_halted` | tx, ddl/d4, test `m3c_deferred_foreign_keys_hold_commit_and_release` |
| S11 | `sqlite3VdbeHalt(p)==SQLITE_BUSY` (4078) | `p->pc` kept, `autoCommit=0`, BUSY | `host.halt_transaction(true)` → `Err` → `set_auto_commit(false)`, `self.pc = pc`, `not_halted` | tx K scripts (RELEASE of a transaction savepoint while another connection reads: taken) |
| S12 | `rc = p->rc; if( rc ) autoCommit=0 else isTransactionSavepoint=0` (4084-4089) | | `Ok(Some(e))` → `set_auto_commit(false)` + `halt_rc`; `Ok(None)` → `set_transaction_savepoint(false)` | error outcome not taken (needs a failing commit hook / xSync / I/O error at RELEASE) |
| S13 | `p1==SAVEPOINT_ROLLBACK` (4093) | `isSchemaChange = DBFLAG_SchemaChange` (4094) | `host.savepoint_schema_change(k)` = `schema_changed_in_tx` (fixed in M3c: no longer the engine's schema generation, which also moves without DDL) | tx_00264 (pending reader survives ROLLBACK TO), ddl/d1-d2 |
| S14 | `sqlite3BtreeTripAllCursors(.., isSchemaChange==0)` != OK (4096) | abort | `host.trip_all_cursors(ii, !is_schema_change)?` | tx (call taken; error not taken) |
| S15 | `sqlite3BtreeSavepoint(pBt, p1, iSavepoint)` != OK (4106) | abort | `host.btree_savepoint(ii, p1, k)?` (`subsavepoint_depth` of savepoint k; `inTrans==TRANS_WRITE` check of btree.c `sqlite3BtreeSavepoint`) | tx (call taken) |
| S16 | `isSchemaChange` (4111) | expire + reset schemas + flag | `host.savepoint_schema_reset(k)` (engine: schema of the savepoint back, generations bumped) | ddl/d1, d2 (taken) |
| S17 | `while( db->pSavepoint!=pSavepoint )` (4121) / `p1==RELEASE` (4132) | nested destroyed; RELEASE destroys it, ROLLBACK restores `nDeferredCons` | `host.savepoint_close(k, release)` | tx (both) |
| S18 | `!isTransaction || p1==SAVEPOINT_ROLLBACK` (4145) | `sqlite3VtabSavepoint(db, p1, iSavepoint)` | same condition → `host.vtab_savepoint(p1, k)` (`vtab_release_all` / `vtab_rollback_to_all` with the place `savepoint_close` recorded) | tx with fts5 (taken) |
| S19 | `p->eVdbeState==VDBE_HALT_STATE` (4152) | SQLITE_DONE | `if self.halted { return Ok(false) }` | tx (RELEASE of the transaction savepoint) |

## 2. `OP_AutoCommit` (vdbe.c 4168-4234) — Rust `vdbe.rs` arm `OP_AutoCommit`

| # | C condition | outcome | Rust place | evidence |
|---|---|---|---|---|
| A1 | `desiredAutoCommit!=db->autoCommit` (4179) | else: "cannot start a transaction within a transaction" / "cannot rollback - no transaction is active" / "cannot commit - no transaction is active" (4212) | `desired_auto_commit != host.auto_commit()` | tx (all three taken), test |
| A2 | `iRollback` (4180) | `sqlite3RollbackAll(db, SQLITE_ABORT_ROLLBACK)`, `autoCommit=1` | `host.rollback_all()` (engine `rollback_all_databases` + hook; btree errors ignored as in C) | tx (taken; pending readers tripped as in C) |
| A3 | `desiredAutoCommit && db->nVdbeWrite>0` (4184) | SQLITE_BUSY "cannot commit transaction - SQL statements in progress" | same | tx (taken) |
| A4 | `sqlite3VdbeCheckFkDeferred(p)!=OK` (4192) | `goto vdbe_return` | `fk_deferred_pending()` + `not_halted` | tx, test |
| A5 | `sqlite3VdbeHalt(p)==SQLITE_BUSY` (4197) | `pc` kept, `autoCommit = 1-desired`, BUSY | `halt_transaction` `Err` → `set_auto_commit(!desired)`, `RunningExec::MachineControl` keeps the VM, the next step repeats | tx K scripts (taken), test `m3c_busy_commit_leaves_the_transaction_open` |
| A6 | `sqlite3CloseSavepoints(db)` (4203) | | `host.close_savepoints()` | tx |
| A7 | `p->rc==SQLITE_OK` (4204) | DONE / ERROR | `Ok(None)` → `Ok(false)`; `Ok(Some(e))` → `Err(e)` (`halt_rc`) | error outcome not taken |

## 3. `sqlite3VdbeHalt` for a transaction statement (vdbeaux.c 3330-3520) — Rust `host::halt_transaction`, `host::halt_control`

| # | C condition | outcome | Rust place | evidence |
|---|---|---|---|---|
| H1 | `isSpecialError` (3377) | statement / transaction rollback | not reachable for OP_AutoCommit / OP_Savepoint (they halt with `p->rc==OK`); BUSY of OP_Transaction is not special | classified: unreachable for these programs |
| H2 | `!sqlite3VtabInSync(db) && db->autoCommit && db->nVdbeWrite==(p->readOnly==0)` (3416) | commit or rollback | `auto_commit && n_vdbe_write == !read_only` (no transaction statement runs inside xSync) | tx (both outcomes) |
| H3 | `sqlite3VdbeCheckFkDeferred(p)` (3418) / `NEVER(p->readOnly)` | FK error | `check_deferred_fks` → `fk_error()` | NEVER branch: unreachable (A4/S10 checked before) |
| H4 | `db->flags & SQLITE_CorruptRdOnly` (3428) | SQLITE_CORRUPT, flag cleared, rollback | `ctx.db_set.corrupt_rdonly()` | not taken (needs a corrupt file inside a transaction) |
| H5 | `rc = vdbeCommit(db, p)` (3436) | | `engine::commit_all_databases` (vtab xSync, EXCLUSIVE locks, commit hook, two-phase commit, xCommit: the engine's port of vdbeCommit, unchanged) | tx (taken) |
| H6 | `rc==SQLITE_BUSY && p->readOnly` (3438) | return BUSY, transaction open | `Err((e, LeaveOpen)) if e.code==Busy && read_only` → `Err(e)` | tx K scripts (taken) |
| H7 | `rc!=SQLITE_OK` (3442) | `p->rc=rc; sqlite3RollbackAll(db, SQLITE_OK)` | FK / CORRUPT / xSync / commit-hook failures: rollback + rollback hook as in C (`Ok(Some(e))`); an I/O error of the two-phase commit: the transaction stays open (**E28**: the engine's pager has no PAGER_ERROR state that keeps the journal hot; C evidence `m3c/fault/fault.c`) | not taken in the corpora; E28 by `fault.c` and the `bulgu_y1` / `bulgu_y2` tests |
| H8 | success (3447) | `nDeferredCons=0`, `sqlite3CommitInternalChanges` | `deferred_fk_suspects.clear()`, `schema_changed_in_tx=false` | tx (taken) |
| H9 | statement BUSY at OP_Transaction (`BEGIN IMMEDIATE`), reset (`p->rc==BUSY`, not read-only, autocommit) | `sqlite3RollbackAll(db, SQLITE_OK)` (3455) | `host::halt_control` | not taken in the corpora (needs a busy second database after the first one started) |

## 4. `OP_Transaction` additions (vdbe.c 4257-4345) — Rust `vdbe.rs` arm `OP_Transaction`, `WriteHost::begin_write_trans`

| # | C condition | outcome | Rust place | evidence |
|---|---|---|---|---|
| T1 | `pOp->p2 && (db->flags & (QueryOnly|CorruptRdOnly))` (4268) | READONLY / CORRUPT | `host.query_only() || host.corrupt_rdonly()` | QueryOnly: the engine has no `PRAGMA query_only` (D55, the host answers false); CorruptRdOnly not taken |
| T2 | `pBt` (4282) | a database without a b-tree is skipped (TEMP before use) | `WriteHost.control`: unopened slot → no transaction | tx (`BEGIN IMMEDIATE` with and without TEMP) |
| T3 | `(rc&0xff)==SQLITE_BUSY` (4287) | `p->pc` kept, `goto vdbe_return` | `not_halted` + `self.pc` (a DML statement of the engine's write path halts at once: `run_write`) | tx: taken 20 times in C (statements while the second connection holds its lock), the 400 scripts equal / classified |
| T4 | `p2==2` | EXCLUSIVE lock | `begin_exclusive()` for `p2 >= 2` | tx (`BEGIN EXCLUSIVE`) |

## 5. Virtual tables in read programs (vdbe.c 8534-8830)

| # | C condition | outcome | Rust place | evidence |
|---|---|---|---|---|
| V1 | `OP_VInitIn` (8634) | `ValueList` pointer in r[P2] (`MEM_Null`, subtype 'p') | arm `OP_VInitIn`: `MEM_NULL|MEM_SUBTYPE`, `e_subtype=b'p'`, `value_lists` | no module of the engine calls `sqlite3_vtab_in(..,1)` (C: only test_bestindex.c / vec1): not generated by any corpus; `OP_VFilter` hands the cursor's values to `VirtualCursor::filter_c` (`in_lists`) |
| V2 | `OP_VCheck` (8587): `pTab->u.vtab.p==0` (8600) | NULL | `host.vtab_integrity` (`None` if not connected) | only `PRAGMA integrity_check` codes it (pragma.c:2190); the generator does not code PRAGMA: unreachable |
| V3 | `xIntegrity` rc (8612) / `zErr` (8616) | abort / result text | `?` / `set_str` | unreachable (V2) |
| V4 | `OP_VFilter` (8671-8730) | `xFilter(pVCur, iQuery, p4.z, nArg, apArg)` (8706) | `host.vtab_filter_in` → `VirtualCursor::filter_c` (C's plan) | vt (taken for all modules) |
| V5 | `OP_VRename` (8830) | | not ported: coded only by `ALTER TABLE RENAME` of a virtual table (alter.c:277), DDL stays on the engine | unreachable |
| V6 | xFindFunction overload (`SQLITE_FUNC_EPHEM`, vtab.c `sqlite3VtabOverloadFunction`) | the module's function | `host_common!` `call_function`: FUNC_EPHEM + an FTS5 auxiliary name → `engine::call_fts5_aux` (fts5ApiCallback) | vt_x highlight / snippet / bm25 (58 statements, equal) |

### Module xFilter with C's plan (D35)

| module | C xFilter | Rust | decisions |
|---|---|---|---|
| generate_series | `seriesFilter` (ext/misc/series.c 440-686) | `SeriesC::filter` (port, `filter_c`) + `seriesNext` / `seriesColumn` / `seriesRowid` / `seriesEof` | every branch ported in order: NULL argument (453), start / stop / step defaults (464-480), value-only ranges (488-493), step sign (496-509), LIMIT / OFFSET (513-519), value=, >=, >, <=, < with float/int arms (524-597), range reduction (602-629), iTerm adjustment (633-645), order swap (650-657), LIMIT/OFFSET apply (661-673) |
| dbstat | `statFilter` (dbstat.c) | `DbStatCursor::filter_c` | schema= (`sqlite3FindDbName` < 0 → EOF; NULL → EOF), name= (NULL: no WHERE), aggregate= (`sqlite3_value_double!=0`), 0x08 ORDER BY name, b-tree order of C's UNION ALL query |
| pragma_* | `pragmaVtabFilter` (pragma.c) | `PragmaVTabCursor::filter_c` | `j = PragFlg_Result1 ? 0 : 1`, NULL argument leaves its slot empty |
| json_each / json_tree, fts5, fts5vocab, rtree / geopoly | (already the same plan) | default `filter_c` = `filter` | vt corpora equal on the machine |

## 6. `max_page_count` (E23)

| # | C condition | outcome | Rust place | evidence |
|---|---|---|---|---|
| P1 | `pgno>pPager->mxPgno` (pager.c:5653, `getPageNormal` of a page past the end) | SQLITE_FULL at the page fetch | `Pager::allocate_raw_page`: `allocated_pgno > mx_pgno` → FULL before the page count moves; `run_write` sets `mx_pgno` of every pager from the slot's `max_page_count` for the statement and clears it after | mpc 200/200 scripts equal (stdout + file) on the new and on the old check; removed: `check_max_page_count` after OP_Insert / OP_IdxInsert |

## Coverage (gcov; `cov_m3c.py`, `covm3c/coverage.md`, `covm3c/untaken.txt`)

Driver part: ops_tx (400) + ops_ddl (4) through `covm3c/rdrv4_cov`. Shell part: vt_d, vt_e and the first 15 `--reset`
chunks of vt_t and vt_x (the -O0 gcov shell runs out of the 60 s budget on 8 of them; vt_a / vt_b skipped for the same
reason — their statements are covered by the rowdiff runs of the release binaries) and the 200 mpc scripts.

| C | lines | branches |
|---|---:|---:|
| `OP_AutoCommit` | 27/28 | 17/18 |
| `OP_Savepoint` | 83/85 | 57/64 |
| `OP_Transaction` | 36/40 | 27/36 |
| `sqlite3VdbeHalt` | 69/90 | 62/90 |
| `vdbeCommit` | 35/104 | 32/82 |
| `sqlite3VdbeCheckFkDeferred` | 4/4 | 2/2 |
| `sqlite3RollbackAll` | 22/23 | 12/18 |
| `sqlite3CloseSavepoints` | 9/9 | 2/2 |
| `sqlite3BtreeSavepoint` | 16/16 | 12/14 |
| `sqlite3PagerSavepoint` | 18/18 | 18/20 |
| `sqlite3BtreeTripAllCursors` | 15/17 | 12/14 |
| `sqlite3VtabSync` / `Commit` / `Rollback` / `Savepoint` | 11/11, 3/3, 3/3, 26/26 | 5/8, –, –, 13/17 |
| `OP_VOpen` / `OP_VFilter` / `OP_VColumn` / `OP_VNext` / `OP_Rowid` | 19/23, 19/19, 22/27, 13/13, 19/22 | 8/14, 6/6, 5/8, 5/6, 8/12 |
| `OP_VInitIn` / `OP_VCheck` | 0/10, 0/17 | 0/2, 0/6 |
| `seriesFilter` / `seriesNext` / `seriesColumn` | 104/139, 10/10, 10/10 | 87/132, 6/8, 4/4 |
| `statFilter` / `pragmaVtabFilter` / `jsonEachFilter` / `fts5VocabFilterMethod` | 37/38, 25/26, 61/66, 46/47 | 14/16, 11/14, 26/34, 29/38 |
| `getPageNormal` / `allocateBtreePage` / `sqlite3PagerMaxPageCount` | 34/45, 85/147, 4/4 | 22/32, 45/112, 1/2 |

### Not-taken branch outcomes of the ported code, classified

| C line (amalgamation) | outcome never taken | class |
|---|---|---|
| OP_AutoCommit `if( p->rc==SQLITE_OK )` | p->rc != OK after the halt (A7) | needs a failing commit hook / xSync / I/O error at COMMIT: E28 (fault.c evidence) |
| OP_Savepoint `sqlite3VtabSavepoint(BEGIN)` rc (S3), `if( pNew )` | error / OOM | no module fails xSavepoint; malloc failure (not modelled) |
| OP_Savepoint `if( rc )` after the commit of RELEASE (S12) | commit error | E28 (fault.c: RELEASE leaves autocommit 0, ported) |
| OP_Savepoint trip / btree savepoint / vtab savepoint rc (S14, S15, S18) | I/O error | I/O errors (crash suites run them for DML; not reachable by a scripted corpus) |
| OP_Transaction QueryOnly arm (T1) | QueryOnly | D55 (the engine has no `PRAGMA query_only`) |
| OP_Transaction `if( p->iStatement==0 )`, `if( rc==SQLITE_OK )` statement journal arms | already open / error | statement-journal of DML (M2/M3a tables) |
| OP_Transaction schema check outcomes | schema cookie changed | ddl/d1 (SQLITE_SCHEMA re-prepare of a slot) covers it on the Rust side; the C driver re-prepares before the step |
| sqlite3RollbackAll `xRollbackCallback` | rollback hook set | the drivers set no hook (test `h2`/`h5` suites cover the hook on the engine) |
| sqlite3VtabSavepoint `pVTab->pVtab && iVersion>=2` false, `iSavepoint>iSavepoint` false | module without xSavepoint | every module of the build has iVersion >= 2 |
| OP_VOpen ALWAYS / NEVER arms, rc, `if( pCur )` | defensive / OOM | unreachable |
| OP_VColumn `OPFLAG_NOCHNG`, `isError`, rc | NOCHNG in a read program; module error | NOCHNG only in UPDATE programs (M3a table); no module errors on these corpora |
| OP_VNext rc | module error | idem |
| OP_VInitIn, OP_VCheck (every line) | — | not generated: no module calls `sqlite3_vtab_in(..,1)`; OP_VCheck only in PRAGMA integrity_check (pragma.c:2190), which the generator does not code |
| statFilter `zSql==0`, `rc==SQLITE_OK` false | OOM / prepare error | unreachable on these corpora |
| pragmaVtabFilter `zText` NULL, `zSql==0` | NULL argument / OOM | NULL argument covered on the Rust side by the probe set (`pragma_table_info(NULL)` equal) |
| sqlite3PagerMaxPageCount `mxPage>0` false | `PRAGMA max_page_count` read / non-positive | engine handles the pragma (unchanged) |
| seriesFilter float arms of value constraints, extreme bounds (8629-8724), `iBase<iTerm` desc, order swap with desc, OFFSET beyond the end | REAL bounds, ±2^63 limits, DESC series with value constraints, OFFSET past the series | not in the vtab corpora; the Rust port is line-by-line (`SeriesC::filter`), checked with the probe set (value>3.5, value<=7, LIMIT/OFFSET, negative step, ORDER BY DESC) — equal |
| seriesNext asserts | — | assert() (debug) |
