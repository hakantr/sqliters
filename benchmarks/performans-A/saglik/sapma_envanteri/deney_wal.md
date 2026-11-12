# Experiment: WAL frame batching off (`SQLITERS_WAL_NO_BATCH=1`), agent `wal` — patch `patches/wal_no_batch.patch`, binaries `bin_wal/`
Change: `crates/sqliters-pager/src/wal.rs` `Wal::write_frames` + `fn wal_no_batch()` (OnceLock); knob on = C's `walWriteOneFrame` sequence
(24-byte header write, then page write per frame). Knob off == HEAD (instructions:u W13 11,705,200 vs 11,702,924; W01 119,331,640 vs 119,323,583).

ab, tmpfs, cpu 8, base = step5b_bin (other agents were building: absolute ms are higher than on an idle box; ratios are interleaved):
```
== W13 knob OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W13_autocommit_wal_txns           5.32      5.25     0.987x     5.18     5.17     4.12    1.28x  OK
TOTAL                             5.32      5.25     0.987x   geomean cand/base = 0.987x   digest mismatches = 0
== W13 knob ON (SQLITERS_WAL_NO_BATCH=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W13_autocommit_wal_txns           5.30      6.08     1.148x     5.20     6.00     4.12    1.48x  OK
TOTAL                             5.30      6.08     1.148x   geomean cand/base = 1.148x   digest mismatches = 0
== W01 knob OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.45      7.33     0.985x     7.42     7.32     9.60    0.76x  OK
TOTAL                             7.45      7.33     0.985x   geomean cand/base = 0.985x   digest mismatches = 0
== W01 knob ON (SQLITERS_WAL_NO_BATCH=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.45      7.50     1.007x     7.41     7.48     9.60    0.78x  OK
TOTAL                             7.45      7.50     1.007x   geomean cand/base = 1.007x   digest mismatches = 0
```
perf stat (-r 9, cpu 8, tmpfs):
```
W13 step5b_bin(HEAD)  instr:u=11702219  instr:k=11739896 cycles:u=7938393  cycles:k=11045187
W13 knob-build OFF    instr:u=11705074  instr:k=11748722 cycles:u=7747529  cycles:k=10770013
W13 knob-build ON     instr:u=11440717  instr:k=13555882 cycles:u=7462231  cycles:k=12737704
W13 C c_speedbench    instr:u=6459720   instr:k=12269931 cycles:u=4409981  cycles:k=11502073
W01 step5b_bin(HEAD)  instr:u=119323862 instr:k=9362127  cycles:u=43674047 cycles:k=8904297
W01 knob-build OFF    instr:u=119331323 instr:k=9436270  cycles:u=42913560 cycles:k=8860146
W01 knob-build ON     instr:u=119271592 instr:k=9966046  cycles:u=42960297 cycles:k=9539449
W01 C c_speedbench    instr:u=129305264 instr:k=8009101  cycles:u=53082460 cycles:k=8016108
```
(cost is in the kernel: user instructions −2.3% with the knob on, kernel +15% W13 / +5.6% W01)

strace -c -f:
| | pwrite64 | fsync | fcntl | pread64 | total |
|---|---|---|---|---|---|
| W13 Rust OFF | 320 | 8 | 1865 | 12 | 2399 |
| W13 Rust ON | 1236 | 8 | 1865 | 12 | 3315 |
| W13 C | 1236 | 8 | 1853 | 13 | 3231 |
| W01 Rust OFF | 241 | 8 | 83 | 218 | 763 |
| W01 Rust ON | 656 | 8 | 83 | 218 | 1172 |
| W01 C | 638 | 8 | 71 | 213 | 1062 |
(W01: 18 more pwrites than C = 6 more frames + 6 more checkpointed pages because the Rust b-tree has 6 more pages.)

Real disk (btrfs) speedbench W13 (synchronous=NORMAL), 21 interleaved runs, median ms: Rust OFF 16.50, Rust ON 22.37, C 19.57 (tmpfs same harness 5.20 / 6.14 / 4.85).
CLI 300 autocommit inserts, median ms: btrfs NORMAL 260.5 / 264.2 / C 250.9; btrfs FULL 10449 / 10329 / C 9944; tmpfs NORMAL 8.15 / 8.23 / 8.26; tmpfs FULL 6.68 / 6.74 / 7.00.

Observable differences with batching ON: none found.
* knob ON: strace of `-wal` writes line-for-line identical to C (single/multi-page commit, restart, NORMAL/FULL, all checkpoint modes, journal_size_limit, EXCLUSIVE, autocheckpoint).
* `-wal` bytes knob ON vs OFF: identical in every snapshot of 21 scenarios. Rust vs C: frame count/order/pgno/commit marks/nCkpt/file size identical in 11 scenario groups; remaining page-image differences = change counter (W5), unused bytes, in-page placement. `-shm` deterministic fields identical (except W6).
* crash states: prefix model (16..483 cut offsets per variant) and block-subset model (157..220 subsets): 0 outcomes other than before/after, 0 reader disagreements between engines, for Rust-written and C-written WALs.
* concurrent readers: live Rust-writer/C-reader and C-writer/Rust-reader pairs gave identical outputs (2 scripts). No concurrent-writer stress, no real power loss, no multi-region -shm (>4062 frames).

Other WAL-path differences found (independent of batching):
* W1 (=D1) CRASH HOLE: C `walEncodeFrame` zeroes salt+checksum of frames written while `iReCksum != 0` (after an in-place rewrite of a spilled frame) so the commit frame is invalid until `walRewriteChecksums` finishes; Rust `Wal::write_frames` always writes real salt + running checksum. If the appended commit frame is durable but the in-place rewrite is not, the chain validates with a stale page -> half-applied transaction (repro wal/hole/hole.py, 10/10: a=40 'NEW', a=1 old, integrity_check ok; C in the same state recovers 0 rows of the txn). Fix ~6 lines.
* W2 (=D2) permissions: -wal/-shm/journal created 0666&~umask, C copies the db's mode (db 0600 -> Rust -wal 0644, C 0600) and uses O_NOFOLLOW.
* W3 (=D3) spill policy: C `pagerStress` writes one LRU page whenever the cache is full (also mid-statement); Rust `spill_if_over_cache_size` -> `spill_dirty_pages_to_wal` only at statement end inside an explicit transaction, all dirty pages in pgno order. Different frame count/order (83 vs 74; mxFrame 53 vs 57), `PRAGMA wal_checkpoint` log column and autocheckpoint timing can differ; cache_size does not bound memory within one statement (cache_size=100 + one 200k-row INSERT: RSS 138,104 KiB vs 4,608 KiB).
* W4 (=D4) partial checkpoint with a pinned reader backfills the newest frame <= mxSafeFrame (C skips pages whose newest frame is above mxSafeFrame): db 73728 vs 69632 bytes after PASSIVE, extra I/O, not SQL-visible. `Wal::backfill_frames_under_read0` vs `walCheckpoint`/`walIteratorNext`.
* W5 (=D5) page-1 change counter in WAL mode: C writes dbFileVers+1 (constant while page 1 is cached: 2 after 300 commits), Rust increments per commit that writes page 1 (63 after 60 growing commits).
* W6 -shm hash region zeroed after TRUNCATE (C leaves stale entries). W7 recovery/rewrite use 2 preads per frame vs 1.
* no difference: header write+sync, restart (salt1+1, nCkpt+1), journal_size_limit, TRUNCATE, close (checkpoint, unlink), EXCLUSIVE heap index, checkpoint result rows, autocheckpoint threshold.
* passing: `substr('NEW',-6)` returns '' in Rust, 'NEW' in C (`substr('NEW',-4)`: 'NE' vs 'NEW'); ascending inserts into the middle of the key space: Rust leaves 51% full (10 cells) vs C 86% (17 cells).
Tools: wal/walcmp.py (use `--ignore-change-counter --logical`), wal/filecmp.py, wal/drive.py, wal/scen.py, wal/crash.py, wal/hole/hole.py.
