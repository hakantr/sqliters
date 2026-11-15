# Differences from C noticed during M2 that are not the machine's (old engine / pager / code generator)

E1..E8 of step M1 still hold (`$S/m1/DIFFS_OUTSIDE_TASK.md`), except the opcode-trace remark on `zeroblob()` in E8: the
machine now evaluates the built-in `zeroblob(N)` itself and keeps C's `MEM_Zero` blob.
C = `$S/bin/sqlite3` / the C library 3.54.1 (drivers `drv/c/rdrv2`, `drv/c/rdrv3`); engine = sqliters.

## E9 — CREATE INDEX on a table that does not exist (engine DDL)
`CREATE INDEX i ON nosuch(a);`  C: `no such table: main.nosuch`; engine: `no such table: nosuch`.
All remaining stdout differences of the DML differential are this message (dml_b 4 scripts, ops_sc6 6 scripts); the
statement is DDL (E op), run by the engine with the switch on and off alike.

## E10 — PRAGMA defer_foreign_keys is not implemented by the engine
`PRAGMA defer_foreign_keys; PRAGMA defer_foreign_keys=1; PRAGMA defer_foreign_keys;`  C: `0`, `1`; engine: no rows (the pragma
is ignored). C's `SQLITE_DeferFKs` flag decides `OP_FkCounter` (immediate violations counted in `db->nDeferredImmCons`) and
`OP_FkIfZero`; the machine asks `VdbeHost::defer_fks()`, which is `false` while the engine has no such flag. (C coverage: the
`SQLITE_DeferFKs` branch of `OP_FkCounter` and the `nDeferredImmCons != 0` outcomes of `OP_FkIfZero` were never taken.)

## E11 — the pager's full rollback plays no journal back (pager)
When a write transaction is rolled back as a whole (an autocommit statement that fails, `ROLLBACK`, a failing `COMMIT` of a
deferred FK violation), C's `pager_playback` replays every journal record (`TR|pc|pb ... incache=1 o=cache` events: the page
image goes back into the cached page). The Rust pager drops the cache instead: no `pb` events. File bytes and results are the
same. This is the first page-cache decision difference in 121 of the 122 dml_a scripts whose decision streams differ, with the
switch on and off alike (trdml.py: `pc_decisions_equal_without_pb`).

## E12 — pager flags of PRAGMA statements
C calls `setAllPagerFlags` (`TR|pc|flags` events) for pragmas the engine handles without touching the pagers
(`PRAGMA foreign_keys`, `PRAGMA recursive_triggers`): two extra events at the start of the trace of tst/frames_fk.ops.
No effect on results or file bytes.

## E13 — sqlite3_interrupt() before a statement starts (old path, finding F4 of the M2 item-1 gate)
C's `sqlite3Step` clears `db->u1.isInterrupted` when no statement is active (`nVdbeActive==0`): an interrupt that arrives while
nothing runs does not stop the next statement. The engine's executor never clears the flag for reads, so the next SELECT
fails with `interrupted`. The machine does what C does. (tests/phase1 `test_group3_entry_gate_...` asserts the engine's
behaviour; not edited.)

## E14 — old write path through the API driver (db bytes)
With the switch off, `run_dml.sh ops_sc6 off` differs from C in the file bytes of `wr_*` scripts that the machine (C's program)
gets right; numbers are in the gate log (`ops_sc6.off_<tag>.res`). Old path, not touched.

## E15 — what the machine does not run in this step (not differences: the engine runs these statements)
Writes to virtual tables (FTS5, R-Tree: the program dump has P4 kind `V`; 45 statements in ops_sc6, 45 in ops_sc7), DDL,
`BEGIN` / `COMMIT` / `ROLLBACK` / `SAVEPOINT` / `RELEASE` (opcodes `OP_AutoCommit`, `OP_Savepoint`, `OP_SetCookie`,
`OP_Expire`, `OP_CreateBtree`, `OP_Destroy`, `OP_ParseSchema` … are not ported), `xfer` INSERT ... SELECT
(`OPFLAG_PREFORMAT`, `OP_RowCell`), update / pre-update hooks (`OP_Insert` / `OP_Delete` do not call them).

## F6. Rollback trip of a GROUP BY statement (found when merging V3 into the machine branch)

`tests/phase1_core_ownership_and_lifecycle.rs` `test_group5_activity_tracker_…` (line 1246) expects that after
`BEGIN; <step a GROUP BY … ORDER BY grp statement once>; CREATE TABLE …; ROLLBACK;` the next step of the GROUP BY
statement fails with SQLITE_ABORT_ROLLBACK. C 3.54.1 (checked with a program against the amalgamation) returns the
next row (`b`): C's GROUP BY program has already read the table into its sorter, and sqlite3BtreeTripAllCursors trips
only B-tree cursors (the plain `SELECT id FROM t ORDER BY id` does fail with 516 in C). Since V3 the generator supports
GROUP BY over tables, so with `SQLITERS_VDBE_EXEC=1` the machine runs C's program and returns the row as C does; the
old path aborts. Assertion left unchanged because it would break the switch-off run (same handling as F4); it becomes
C's when the old path is removed.
