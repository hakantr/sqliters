# Experiment: R-Tree write-through / C-faithful algorithm (agent rtree_wt) — patch patches/rtree_write_through.patch, binaries bin_rtree_wt/
Knobs: SQLITERS_RTREE_WRITE_THROUGH=1 (flush dirty nodes/_parent/_rowid at the end of every xUpdate, Rust tree algorithm), =2 (C-faithful algorithm + eager structural writes in rtree.c order), SQLITERS_RTREE_C_ALGO=1 (C-faithful algorithm, commit-time writes), SQLITERS_RTREE_CACHE_MAX=n (with write-through: drop the clean read cache above n entries; 0 = like C).
Raw notes: rtree_wt/PROGRESS.md (copied below from its 'loads' section on; build 6 numbers).

### loads (build 5 = before moving stale bytes out of CachedNode; WT numbers valid, knob-off RSS/instr on cte* inflated by the bigger undo entries)
```
ins1e5_tx      head         elapsed=0:00.97  user=0.95    instr=5015018164     rssKB=28616    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      off          elapsed=0:01.06  user=1.04    instr=5014298622     rssKB=28700    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1          elapsed=0:01.60  user=1.57    instr=6992734956     rssKB=28892    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1_c20k     elapsed=0:01.70  user=1.68    instr=7218657485     rssKB=23400    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1_c0       elapsed=0:02.13  user=2.12    instr=9974422628     rssKB=13580    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      algo         elapsed=0:01.02  user=1.00    instr=5348783870     rssKB=29520    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2          elapsed=0:01.56  user=1.53    instr=6816818717     rssKB=29728    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c20k     elapsed=0:01.66  user=1.64    instr=7017554530     rssKB=25884    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c0       elapsed=0:02.24  user=2.23    instr=10607865647    rssKB=13220    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      C            elapsed=0:00.93  user=0.84    instr=4010471142     rssKB=6488     pages=1312    nodes=2985   rows=100000   check=ok 

mixed1e5_tx    head         elapsed=0:02.02  user=2.01    instr=8244526076     rssKB=20740    pages=398     nodes=933    rows=30135    check=ok 
mixed1e5_tx    off          elapsed=0:01.93  user=1.92    instr=8259661509     rssKB=20796    pages=398     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1          elapsed=0:02.80  user=2.79    instr=10423071650    rssKB=17216    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1_c20k     elapsed=0:02.84  user=2.83    instr=10595258395    rssKB=14560    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1_c0       elapsed=0:03.59  user=3.59    instr=14588174615    rssKB=10000    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    algo         elapsed=0:01.98  user=1.97    instr=8347870543     rssKB=22140    pages=398     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2          elapsed=0:02.92  user=2.91    instr=11093051870    rssKB=18280    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2_c20k     elapsed=0:02.88  user=2.87    instr=11105707805    rssKB=15928    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2_c0       elapsed=0:03.83  user=3.82    instr=15998758183    rssKB=9984     pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    C            elapsed=0:01.24  user=1.24    instr=5713288855     rssKB=6492     pages=503     nodes=931    rows=30135    check=ok 

ins1e6_tx      head         elapsed=0:10.77  user=10.51   instr=51151833921    rssKB=266436   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      off          elapsed=0:10.93  user=10.68   instr=51090677850    rssKB=267756   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1          elapsed=0:17.79  user=17.51   instr=70887463083    rssKB=307824   pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1_c20k     elapsed=0:21.65  user=21.54   instr=80688429269    rssKB=83052    pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1_c0       elapsed=0:23.97  user=23.87   instr=107823642888   rssKB=62012    pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      algo         elapsed=0:11.79  user=11.53   instr=54283733007    rssKB=279424   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt2          elapsed=0:18.16  user=17.85   instr=68220738429    rssKB=319120   pages=13191   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt2_c20k     elapsed=0:20.72  user=20.60   instr=80039669647    rssKB=95784    pages=13191   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt2_c0       elapsed=0:24.61  user=24.51   instr=114034505678   rssKB=63828    pages=13191   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      C            elapsed=0:14.94  user=11.85   instr=45168156786    rssKB=6928     pages=13166   nodes=29932  rows=1000000  check=ok 

cte1e5         head         elapsed=0:01.27  user=1.20    instr=7496307811     rssKB=109956   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         off          elapsed=0:01.30  user=1.23    instr=7627849186     rssKB=126044   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         wt1          elapsed=0:01.54  user=1.51    instr=9293589806     rssKB=54864    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt1_c20k     elapsed=0:01.58  user=1.55    instr=9505949132     rssKB=49792    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt1_c0       elapsed=0:01.93  user=1.91    instr=12207542967    rssKB=49796    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         algo         elapsed=0:01.47  user=1.30    instr=7944017804     rssKB=290832   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         wt2          elapsed=0:01.51  user=1.48    instr=9180623094     rssKB=54720    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2_c20k     elapsed=0:01.56  user=1.53    instr=9424301601     rssKB=49840    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2_c0       elapsed=0:02.07  user=2.03    instr=12799514213    rssKB=49820    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         C            elapsed=0:00.68  user=0.61    instr=3056990855     rssKB=6848     pages=1313    nodes=2979   rows=100000   check=ok 

cte1e6         head         elapsed=0:13.92  user=13.27   instr=75890215185    rssKB=1090932  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         off          elapsed=0:13.80  user=13.01   instr=77179922252    rssKB=1252216  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1          elapsed=0:17.41  user=17.03   instr=94050961306    rssKB=539056   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1_c20k     elapsed=0:18.62  user=18.33   instr=103907676728   rssKB=428328   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1_c0       elapsed=0:21.99  user=21.69   instr=130702499798   rssKB=428380   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         algo         elapsed=0:16.02  user=14.31   instr=80232535039    rssKB=2886676  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         wt2          elapsed=0:16.47  user=16.09   instr=92105407532    rssKB=539108   pages=13130   nodes=29802  rows=1000000  check=ok 
cte1e6         wt2_c20k     elapsed=0:19.12  user=18.85   instr=103941240207   rssKB=428724   pages=13130   nodes=29802  rows=1000000  check=ok 
cte1e6         wt2_c0       elapsed=0:22.90  user=22.63   instr=137458373947   rssKB=428316   pages=13130   nodes=29802  rows=1000000  check=ok 
cte1e6         C            elapsed=0:11.89  user=8.95    instr=35415551901    rssKB=6824     pages=13106   nodes=29802  rows=1000000  check=ok 

ins1e5_auto    head         elapsed=0:47.33  user=40.00   instr=163417842307   rssKB=28100    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    off          elapsed=0:47.66  user=40.28   instr=162861395797   rssKB=28624    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt1          elapsed=0:11.68  user=5.59    instr=9895156421     rssKB=28208    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt1_c20k     elapsed=0:12.31  user=5.90    instr=10138510238    rssKB=22944    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt1_c0       elapsed=0:12.62  user=6.20    instr=12876362639    rssKB=13612    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    algo         elapsed=0:46.30  user=39.52   instr=160927712895   rssKB=29728    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2          elapsed=0:11.22  user=5.44    instr=9569910526     rssKB=29564    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2_c20k     elapsed=0:11.88  user=5.75    instr=9837629153     rssKB=26188    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2_c0       elapsed=0:12.59  user=6.27    instr=13401706540    rssKB=13176    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_auto    C            elapsed=0:08.82  user=4.01    instr=5057278253     rssKB=6856     pages=1312    nodes=2985   rows=100000   check=ok 

```
NOTE autocommit: HEAD 163G instr / 47 s for 1e5 autocommit inserts vs 9.9G / 11.7 s with WRITE_THROUGH=1: flush_dirty() walks the whole cache at every commit (quadratic).

## BUILD 6 (stale bytes moved from CachedNode to RTreeCache.stale side map => CachedNode/RTreeUndo sizes == HEAD)
### perf stat
```
W12 HEAD  X=0                                                            instructions:u=145842980 cycles:u=67625569
W12 cand  X=0                                                            instructions:u=145963701 cycles:u=68521978
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=1                                 instructions:u=228896406 cycles:u=120472341
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=1 SQLITERS_RTREE_CACHE_MAX=0      instructions:u=329068605 cycles:u=165654032
W12 cand  SQLITERS_RTREE_C_ALGO=1                                        instructions:u=164254599 cycles:u=78986360
W12 cand  SQLITERS_RTREE_C_ALGO=1 SQLITERS_RTREE_WRITE_THROUGH=1         instructions:u=233203808 cycles:u=125958459
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=2                                 instructions:u=229968215 cycles:u=124097760
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=2 SQLITERS_RTREE_CACHE_MAX=0      instructions:u=375241671 cycles:u=188033635
W12 C twin (c_speedbench): instructions:u=154647311 cycles:u=71772785
W13 step5b_bin/speedbench                                        instructions:u=11708902
W13 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=11707800
W10 step5b_bin/speedbench                                        instructions:u=85013960
W10 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=84997618
W03 step5b_bin/speedbench                                        instructions:u=191521313
W03 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=191525963
CLI step5b_bin/sqliters ins1e5_tx(100k single-row INSERTs)=5014107140 cte50k(one INSERT..SELECT)=3732253906 select3000=647171246
CLI step5c/src_rtree_wt/target/release/sqliters ins1e5_tx(100k single-row INSERTs)=5013823369 cte50k(one INSERT..SELECT)=3732220290 select3000=642070812
```
### ab.py
```
### KNOB=off
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        23.82     23.80     0.999x    23.63    23.56    24.63    0.97x  OK
TOTAL                            23.82     23.80     0.999x   geomean cand/base = 0.999x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.04     42.53     1.770x    23.65    41.79    24.63    1.73x  OK
TOTAL                            24.04     42.53     1.770x   geomean cand/base = 1.770x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=2
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        23.90     43.37     1.815x    23.59    42.44    24.63    1.76x  OK
TOTAL                            23.90     43.37     1.815x   geomean cand/base = 1.815x   digest mismatches = 0
### SQLITERS_RTREE_C_ALGO=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.08     28.48     1.183x    23.95    27.76    24.63    1.16x  OK
TOTAL                            24.08     28.48     1.183x   geomean cand/base = 1.183x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=1 SQLITERS_RTREE_CACHE_MAX=0
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.14     58.05     2.405x    23.85    55.84    24.63    2.36x  OK
TOTAL                            24.14     58.05     2.405x   geomean cand/base = 2.405x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=2 SQLITERS_RTREE_CACHE_MAX=0
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.39     65.17     2.671x    23.92    63.74    24.63    2.65x  OK
TOTAL                            24.39     65.17     2.671x   geomean cand/base = 2.671x   digest mismatches = 0
### C twin (3 runs)
W12_rtree_insert_and_bbox    memory              5500       26.615 ms       206653.8 ops/s     4256 KiB  b123c883941db5cf
W12_rtree_insert_and_bbox    memory              5500       24.730 ms       222406.4 ops/s     4320 KiB  b123c883941db5cf
W12_rtree_insert_and_bbox    memory              5500       24.551 ms       224026.8 ops/s     4272 KiB  b123c883941db5cf
```
knob-off parity: W12 +0.08%, CLI inserts -0.006%, cte1e5 +0.008% (RSS 110.2 vs 109.7 MB), ins1e6 -0.13% (RSS equal), BUT mixed1e5_tx +0.78% (8.309G vs 8.245G)
-> build 7 adds #[inline(always)] on write_rowid_mapping / delete_rowid_mapping / build_leaf_path / allocate_new_rowid (single call site in HEAD).
C path also verified byte-identical to C on 5-D, 3-D float and 1-D rtree_i32 tables at page_size 512 and 4096 (div8).
Multi-row DELETE on vtab: C calls xUpdate in ascending rowid order, Rust in scan order -> different tree (div5).

## FINAL build section of rtree_wt/PROGRESS.md (build 8; these ab / perf numbers supersede the build-6 block above)

## FINAL BUILD (build 8 source; patch + bin_rtree_wt refreshed; apply --check OK; fmt OK; clippy clean; no unsafe)
Changes after build 6: RTreeUndo has NO new variants (C-path stale undo = Node(-nodeno, ..), wt=2 touch marker = NodePush);
flush_dirty = dispatcher + flush_dirty_deferred::<const C_STALE> (HEAD body for C_STALE=false); inline(always) on the four
single-call-site helpers the C path also calls.
### perf stat
```
W12 HEAD  X=0                                                            instructions:u=145842694 cycles:u=70304715
W12 cand  X=0                                                            instructions:u=145975153 cycles:u=68515559
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=1                                 instructions:u=229168364 cycles:u=122437559
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=1 SQLITERS_RTREE_CACHE_MAX=0      instructions:u=329161992 cycles:u=167427167
W12 cand  SQLITERS_RTREE_C_ALGO=1                                        instructions:u=164087831 cycles:u=79662391
W12 cand  SQLITERS_RTREE_C_ALGO=1 SQLITERS_RTREE_WRITE_THROUGH=1         instructions:u=233170196 cycles:u=125513787
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=2                                 instructions:u=229952891 cycles:u=123576132
W12 cand  SQLITERS_RTREE_WRITE_THROUGH=2 SQLITERS_RTREE_CACHE_MAX=0      instructions:u=375207830 cycles:u=191039651
W12 C twin (c_speedbench): instructions:u=154647314 cycles:u=71501163
W13 step5b_bin/speedbench                                        instructions:u=11707946
W13 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=11709517
W10 step5b_bin/speedbench                                        instructions:u=85012978
W10 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=84998235
W03 step5b_bin/speedbench                                        instructions:u=191525460
W03 step5c/src_rtree_wt/target/release/examples/speedbench       instructions:u=191529221
CLI step5b_bin/sqliters ins1e5_tx(100k single-row INSERTs)=5014137940 cte50k(one INSERT..SELECT)=3732200351 select3000=647225018
CLI step5c/src_rtree_wt/target/release/sqliters ins1e5_tx(100k single-row INSERTs)=5014061870 cte50k(one INSERT..SELECT)=3731722967 select3000=642008083
```
### ab.py (verbatim)
```
### KNOB=off
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.01     24.15     1.006x    23.77    23.58    24.63    0.98x  OK
TOTAL                            24.01     24.15     1.006x   geomean cand/base = 1.006x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        23.95     42.50     1.775x    23.83    41.51    24.63    1.73x  OK
TOTAL                            23.95     42.50     1.775x   geomean cand/base = 1.775x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=2
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.10     43.63     1.810x    23.63    42.63    24.63    1.77x  OK
TOTAL                            24.10     43.63     1.810x   geomean cand/base = 1.810x   digest mismatches = 0
### SQLITERS_RTREE_C_ALGO=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        23.85     28.08     1.177x    23.73    27.63    24.63    1.14x  OK
TOTAL                            23.85     28.08     1.177x   geomean cand/base = 1.177x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=1 SQLITERS_RTREE_CACHE_MAX=0
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        24.01     57.94     2.413x    23.85    57.52    24.63    2.35x  OK
TOTAL                            24.01     57.94     2.413x   geomean cand/base = 2.413x   digest mismatches = 0
### SQLITERS_RTREE_WRITE_THROUGH=2 SQLITERS_RTREE_CACHE_MAX=0
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W12_rtree_insert_and_bbox        23.96     67.87     2.832x    23.73    65.63    24.63    2.76x  OK
TOTAL                            23.96     67.87     2.832x   geomean cand/base = 2.832x   digest mismatches = 0
### C twin (3 runs)
W12_rtree_insert_and_bbox    memory              5500       27.121 ms       202796.5 ops/s     4316 KiB  b123c883941db5cf
W12_rtree_insert_and_bbox    memory              5500       24.541 ms       224118.1 ops/s     4288 KiB  b123c883941db5cf
W12_rtree_insert_and_bbox    memory              5500       24.472 ms       224747.4 ops/s     4364 KiB  b123c883941db5cf
```
### knob-off parity + spot checks (load.sh rows)
```
ins1e5_tx      head         elapsed=0:01.00  user=0.98    instr=5015068311     rssKB=28580    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      off          elapsed=0:01.01  user=0.99    instr=5014454778     rssKB=28392    pages=1275    nodes=2985   rows=100000   check=ok 
mixed1e5_tx    head         elapsed=0:01.90  user=1.89    instr=8244479915     rssKB=20352    pages=398     nodes=933    rows=30135    check=ok 
mixed1e5_tx    off          elapsed=0:02.05  user=2.03    instr=8306620177     rssKB=20684    pages=398     nodes=933    rows=30135    check=ok 
cte1e5         head         elapsed=0:01.28  user=1.22    instr=7496498844     rssKB=109968   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         off          elapsed=0:01.28  user=1.23    instr=7497040193     rssKB=109776   pages=1273    nodes=2979   rows=100000   check=ok 
p_del          head         elapsed=0:00.94  user=0.93    instr=4510079502     rssKB=15448    pages=331     nodes=818    rows=20000    check=ok 
p_del          off          elapsed=0:00.98  user=0.97    instr=4511301142     rssKB=15476    pages=331     nodes=818    rows=20000    check=ok 
p_upd          head         elapsed=0:01.23  user=1.21    instr=5704614538     rssKB=17344    pages=678     nodes=1612   rows=50000    check=ok 
p_upd          off          elapsed=0:01.22  user=1.20    instr=5702352714     rssKB=17760    pages=678     nodes=1612   rows=50000    check=ok 
ins1e6_tx      head         elapsed=0:10.58  user=10.32   instr=51151937411    rssKB=266492   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      off          elapsed=0:10.47  user=10.22   instr=51084048320    rssKB=266516   pages=12802   nodes=29932  rows=1000000  check=ok 
cte1e6         head         elapsed=0:13.80  user=13.08   instr=75878278388    rssKB=1090904  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         off          elapsed=0:13.95  user=13.21   instr=75888375714    rssKB=1090876  pages=12758   nodes=29802  rows=1000000  check=ok 
ins1e5_auto    head         elapsed=0:47.17  user=39.92   instr=163417953465   rssKB=28116    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    off          elapsed=0:47.17  user=39.90   instr=163121203744   rssKB=28132    pages=1312    nodes=2985   rows=100000   check=ok 
mixed1e5_tx    wt1          elapsed=0:02.86  user=2.85    instr=10438971276    rssKB=17144    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt2          elapsed=0:03.03  user=3.02    instr=11110341101    rssKB=17780    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    algo         elapsed=0:02.02  user=2.00    instr=8502302484     rssKB=22124    pages=398     nodes=931    rows=30135    check=ok 
ins1e5_tx      wt1          elapsed=0:01.57  user=1.55    instr=7006220628     rssKB=28872    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2          elapsed=0:01.60  user=1.58    instr=6882029472     rssKB=29740    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c0       elapsed=0:02.29  user=2.28    instr=10941145745    rssKB=13640    pages=1313    nodes=2985   rows=100000   check=ok 
cte1e5         wt1          elapsed=0:01.51  user=1.48    instr=9176223016     rssKB=54848    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2          elapsed=0:01.52  user=1.47    instr=9134535542     rssKB=54804    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         algo         elapsed=0:01.47  user=1.31    instr=7860997333     rssKB=281976   pages=1273    nodes=2979   rows=100000   check=ok 
ins1e5_auto    wt1          elapsed=0:12.46  user=5.71    instr=9902384064     rssKB=28088    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2          elapsed=0:11.41  user=5.56    instr=9627185785     rssKB=29592    pages=1313    nodes=2985   rows=100000   check=ok 
DONE
```
### loads, full matrix (build 6 binaries; same knob-mode code)
```
ins1e5_tx      head         elapsed=0:01.00  user=0.98    instr=5015024428     rssKB=29032    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      off          elapsed=0:01.00  user=0.99    instr=5014302089     rssKB=29060    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1          elapsed=0:01.71  user=1.69    instr=7000578379     rssKB=28180    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1_c20k     elapsed=0:01.66  user=1.64    instr=7215600491     rssKB=23000    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt1_c0       elapsed=0:02.15  user=2.14    instr=10019150900    rssKB=13620    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      algo         elapsed=0:01.08  user=1.05    instr=5389521309     rssKB=30220    pages=1275    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2          elapsed=0:01.56  user=1.53    instr=6882464189     rssKB=29692    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c20k     elapsed=0:01.68  user=1.67    instr=7118797030     rssKB=25804    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c0       elapsed=0:02.34  user=2.32    instr=10941889975    rssKB=13208    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      C            elapsed=0:00.94  user=0.85    instr=4010471649     rssKB=6912     pages=1312    nodes=2985   rows=100000   check=ok 

mixed1e5_tx    head         elapsed=0:01.91  user=1.90    instr=8244501755     rssKB=20748    pages=398     nodes=933    rows=30135    check=ok 
mixed1e5_tx    off          elapsed=0:01.97  user=1.95    instr=8308648838     rssKB=20752    pages=398     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1          elapsed=0:02.82  user=2.81    instr=10435821471    rssKB=17232    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1_c20k     elapsed=0:03.10  user=3.09    instr=10603277406    rssKB=14020    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt1_c0       elapsed=0:03.68  user=3.68    instr=14544891952    rssKB=10020    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    algo         elapsed=0:02.04  user=2.03    instr=8503831785     rssKB=22128    pages=398     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2          elapsed=0:03.00  user=2.98    instr=11108874150    rssKB=17756    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2_c20k     elapsed=0:03.10  user=3.09    instr=11298374427    rssKB=15536    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    wt2_c0       elapsed=0:03.97  user=3.96    instr=16778936253    rssKB=10060    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    C            elapsed=0:01.19  user=1.18    instr=5713289276     rssKB=6776     pages=503     nodes=931    rows=30135    check=ok 

cte1e5         head         elapsed=0:01.30  user=1.24    instr=7497453365     rssKB=109716   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         off          elapsed=0:01.31  user=1.24    instr=7498075097     rssKB=110232   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         wt1          elapsed=0:01.59  user=1.55    instr=9169352648     rssKB=54796    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt1_c20k     elapsed=0:01.62  user=1.58    instr=9399441555     rssKB=49812    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt1_c0       elapsed=0:01.95  user=1.92    instr=12200027693    rssKB=49860    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         algo         elapsed=0:01.51  user=1.34    instr=7863074961     rssKB=281944   pages=1273    nodes=2979   rows=100000   check=ok 
cte1e5         wt2          elapsed=0:01.54  user=1.50    instr=9135012825     rssKB=54832    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2_c20k     elapsed=0:01.57  user=1.54    instr=9402487158     rssKB=49812    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2_c0       elapsed=0:02.16  user=2.13    instr=13127989027    rssKB=49816    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         C            elapsed=0:00.69  user=0.61    instr=3056991317     rssKB=6488     pages=1313    nodes=2979   rows=100000   check=ok 

ins1e6_tx      head         elapsed=0:10.80  user=10.53   instr=51151925420    rssKB=266468   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      off          elapsed=0:10.67  user=10.40   instr=51083454938    rssKB=266500   pages=12802   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1          elapsed=0:18.45  user=18.17   instr=70922181589    rssKB=307228   pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1_c20k     elapsed=0:20.07  user=19.97   instr=80700569522    rssKB=82572    pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt1_c0       elapsed=0:23.84  user=23.73   instr=107885968783   rssKB=62384    pages=13183   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt2          elapsed=0:17.56  user=17.26   instr=68860135205    rssKB=319392   pages=13191   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      wt2_c0       elapsed=0:27.58  user=27.49   instr=118505825100   rssKB=63780    pages=13191   nodes=29932  rows=1000000  check=ok 
ins1e6_tx      C            elapsed=0:15.65  user=12.28   instr=45168157638    rssKB=6848     pages=13166   nodes=29932  rows=1000000  check=ok 

cte1e6         head         elapsed=0:13.76  user=13.01   instr=75890199531    rssKB=1090912  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         off          elapsed=0:13.63  user=12.90   instr=75887485079    rssKB=1090840  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1          elapsed=0:17.19  user=16.78   instr=92830188519    rssKB=539036   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1_c20k     elapsed=0:18.04  user=17.74   instr=102824361983   rssKB=428356   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         wt1_c0       elapsed=0:21.42  user=21.14   instr=129280100119   rssKB=428388   pages=13126   nodes=29802  rows=1000000  check=ok 
cte1e6         wt2          elapsed=0:16.56  user=16.14   instr=91421251043    rssKB=539100   pages=13130   nodes=29802  rows=1000000  check=ok 
cte1e6         wt2_c0       elapsed=0:23.56  user=23.26   instr=140935015695   rssKB=428272   pages=13130   nodes=29802  rows=1000000  check=ok 
cte1e6         C            elapsed=0:11.76  user=8.88    instr=35415552288    rssKB=6400     pages=13106   nodes=29802  rows=1000000  check=ok 

ins1e5_auto    off          elapsed=0:48.23  user=40.48   instr=168286729455   rssKB=28184    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt1          elapsed=0:11.56  user=5.53    instr=9897014427     rssKB=28364    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt1_c0       elapsed=0:12.72  user=6.47    instr=12927894890    rssKB=13592    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2          elapsed=0:11.74  user=5.65    instr=9627835273     rssKB=29640    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_auto    C            elapsed=0:09.72  user=4.19    instr=5057278886     rssKB=6956     pages=1312    nodes=2985   rows=100000   check=ok 

ins1e5_tx      mem_off      elapsed=0:01.07  user=1.05    instr=5014209541     rssKB=35824    pages=-       nodes=-      rows=-        check=- 
ins1e5_tx      mem_wt1      elapsed=0:01.75  user=1.73    instr=6997818427     rssKB=35400    pages=-       nodes=-      rows=-        check=- 
ins1e5_tx      mem_wt2      elapsed=0:01.75  user=1.73    instr=6878011789     rssKB=36892    pages=-       nodes=-      rows=-        check=- 
ins1e5_tx      mem_C        elapsed=0:00.81  user=0.80    instr=3930663786     rssKB=10072    pages=-       nodes=-      rows=-        check=- 
DONE
```
### verification on final build
- knob off: db file + stdout byte-identical to HEAD on 27/27 scripts (13 health, a1..a8, loads, probes)
- closeness table + replay identical to build 7 (tmp/b8_closeness.txt, tmp/b8_replay.txt)
- C path == C shadow tables for 5-D/3-D float and 1-D i32 at page_size 512 and 4096 (wt2+cache0)
- a7/a8 savepoint scenarios: rows == C in all modes; shadow tables == C for algo/wt2/wt2c0; wt1 == off
- sqlite/ext/rtree/*.test via tclsuite (debug): per-file pass/fail identical for off/wt1/wt1c0/algo/wt2/wt2c0 (rtree2, rtree4 time out in all)
- knob-off instruction residuals that did not go away: mixed1e5_tx +0.75% (varies -0.16..+0.75% over builds 5-9 for the same
  knob-off source path; cte1e5 flipped 0.00% <-> +1.75% between builds) => LTO/inlining side effects, not knob checks.
### parity tail (final build)
```
cte1e6         head         elapsed=0:13.80  user=13.08   instr=75878278388    rssKB=1090904  pages=12758   nodes=29802  rows=1000000  check=ok 
cte1e6         off          elapsed=0:13.95  user=13.21   instr=75888375714    rssKB=1090876  pages=12758   nodes=29802  rows=1000000  check=ok 
ins1e5_auto    head         elapsed=0:47.17  user=39.92   instr=163417953465   rssKB=28116    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    off          elapsed=0:47.17  user=39.90   instr=163121203744   rssKB=28132    pages=1312    nodes=2985   rows=100000   check=ok 
mixed1e5_tx    wt1          elapsed=0:02.86  user=2.85    instr=10438971276    rssKB=17144    pages=534     nodes=933    rows=30135    check=ok 
mixed1e5_tx    wt2          elapsed=0:03.03  user=3.02    instr=11110341101    rssKB=17780    pages=533     nodes=931    rows=30135    check=ok 
mixed1e5_tx    algo         elapsed=0:02.02  user=2.00    instr=8502302484     rssKB=22124    pages=398     nodes=931    rows=30135    check=ok 
ins1e5_tx      wt1          elapsed=0:01.57  user=1.55    instr=7006220628     rssKB=28872    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2          elapsed=0:01.60  user=1.58    instr=6882029472     rssKB=29740    pages=1313    nodes=2985   rows=100000   check=ok 
ins1e5_tx      wt2_c0       elapsed=0:02.29  user=2.28    instr=10941145745    rssKB=13640    pages=1313    nodes=2985   rows=100000   check=ok 
cte1e5         wt1          elapsed=0:01.51  user=1.48    instr=9176223016     rssKB=54848    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         wt2          elapsed=0:01.52  user=1.47    instr=9134535542     rssKB=54804    pages=1314    nodes=2979   rows=100000   check=ok 
cte1e5         algo         elapsed=0:01.47  user=1.31    instr=7860997333     rssKB=281976   pages=1273    nodes=2979   rows=100000   check=ok 
ins1e5_auto    wt1          elapsed=0:12.46  user=5.71    instr=9902384064     rssKB=28088    pages=1312    nodes=2985   rows=100000   check=ok 
ins1e5_auto    wt2          elapsed=0:11.41  user=5.56    instr=9627185785     rssKB=29592    pages=1313    nodes=2985   rows=100000   check=ok 
DONE
```
final knob-off deltas vs HEAD: W12 +0.091%, ins1e5_tx -0.012%, cte1e5 +0.007%, cte1e6 +0.013%, ins1e6_tx -0.13%, p_del +0.027%, p_upd -0.040%, ins1e5_auto -0.18%, mixed1e5_tx +0.754%.
## DONE — report handed back.
