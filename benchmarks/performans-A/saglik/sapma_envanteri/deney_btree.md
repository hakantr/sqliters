# Experiment: B-tree delete rebalance / CREATE INDEX bulk fill / insert-side port + C freelist order (agent btree)
Patches: patches/btree_delete_rebalance.patch (SQLITERS_BTREE_C_DELETE_BALANCE=1), patches/btree_index_bulk_fill.patch (SQLITERS_INDEX_BULK_FILL=1), patches/btree_c_insert_alloc_extra.patch (SQLITERS_BTREE_C_INSERT_BALANCE=1, SQLITERS_BTREE_C_ALLOC=1; applies on top of both). Binaries: bin_btree (two patches), bin_btree_extra (all three). Raw files in btree/.

## ab_final.txt (base = step5b_bin, cand = bin_btree, cpu 10)
```
### W08 knobs OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W08_indexed_update_delete         9.61      9.66     1.005x     9.56     9.58     7.96    1.21x  OK
TOTAL                             9.61      9.66     1.005x   geomean cand/base = 1.005x   digest mismatches = 0

### W08 SQLITERS_BTREE_C_DELETE_BALANCE=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W08_indexed_update_delete         9.62     10.15     1.054x     9.57    10.05     7.96    1.28x  OK
TOTAL                             9.62     10.15     1.054x   geomean cand/base = 1.054x   digest mismatches = 0

### W08 SQLITERS_INDEX_BULK_FILL=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W08_indexed_update_delete         9.62      9.97     1.036x     9.57     9.83     7.96    1.25x  OK
TOTAL                             9.62      9.97     1.036x   geomean cand/base = 1.036x   digest mismatches = 0

### W08 both knobs (SQLITERS_BTREE_C_DELETE_BALANCE=1 SQLITERS_INDEX_BULK_FILL=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W08_indexed_update_delete         9.64     10.33     1.072x     9.60    10.26     7.96    1.30x  OK
TOTAL                             9.64     10.33     1.072x   geomean cand/base = 1.072x   digest mismatches = 0

### W02 knobs OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W02_create_indexes                7.39      7.46     1.010x     7.35     7.41     8.76    0.85x  OK
TOTAL                             7.39      7.46     1.010x   geomean cand/base = 1.010x   digest mismatches = 0

### W02 SQLITERS_BTREE_C_DELETE_BALANCE=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W02_create_indexes                7.39      7.42     1.005x     7.33     7.41     8.76    0.85x  OK
TOTAL                             7.39      7.42     1.005x   geomean cand/base = 1.005x   digest mismatches = 0

### W02 SQLITERS_INDEX_BULK_FILL=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W02_create_indexes                7.39      7.81     1.056x     7.36     7.73     8.76    0.89x  OK
TOTAL                             7.39      7.81     1.056x   geomean cand/base = 1.056x   digest mismatches = 0

### W02 both knobs (SQLITERS_BTREE_C_DELETE_BALANCE=1 SQLITERS_INDEX_BULK_FILL=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W02_create_indexes                7.38      7.80     1.057x     7.34     7.74     8.76    0.89x  OK
TOTAL                             7.38      7.80     1.057x   geomean cand/base = 1.057x   digest mismatches = 0

### W14 knobs OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W14_cli_sql_script               34.44     35.28     1.024x    34.28    34.84    26.19    1.35x  OK
TOTAL                            34.44     35.28     1.024x   geomean cand/base = 1.024x   digest mismatches = 0

### W14 SQLITERS_BTREE_C_DELETE_BALANCE=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W14_cli_sql_script               34.41     35.01     1.018x    34.29    34.74    26.19    1.34x  OK
TOTAL                            34.41     35.01     1.018x   geomean cand/base = 1.018x   digest mismatches = 0

### W14 SQLITERS_INDEX_BULK_FILL=1
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W14_cli_sql_script               34.37     35.13     1.022x    34.09    34.83    26.19    1.34x  OK
TOTAL                            34.37     35.13     1.022x   geomean cand/base = 1.022x   digest mismatches = 0

### W14 both knobs (SQLITERS_BTREE_C_DELETE_BALANCE=1 SQLITERS_INDEX_BULK_FILL=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W14_cli_sql_script               34.54     35.07     1.015x    34.25    34.74    26.19    1.34x  OK
TOTAL                            34.54     35.07     1.015x   geomean cand/base = 1.015x   digest mismatches = 0

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

### FULL knobs OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.49      7.41     0.988x     7.42     7.37     9.60    0.77x  OK
W02_create_indexes                7.46      7.51     1.006x     7.39     7.44     8.76    0.86x  OK
W03_pk_point_select              28.16     27.85     0.989x    27.78    27.69    23.30    1.20x  OK
W04_secondary_idx_range           7.89      7.83     0.992x     7.77     7.70     7.46    1.05x  OK
W05_full_scan_filter_agg          4.12      4.40     1.069x     4.09     4.39    16.57    0.27x  OK
W06_join_group_order             10.65     10.75     1.009x    10.63    10.73    28.23    0.38x  OK
W07_window_functions             38.37     38.39     1.001x    38.25    37.94    50.21    0.76x  OK
W08_indexed_update_delete         9.44      9.49     1.005x     9.40     9.39     7.96    1.19x  OK
W09_like_text_scan               12.02     12.16     1.012x    11.97    12.05    18.92    0.64x  OK
W10_jsonb_extract_agg            11.04     11.07     1.003x    10.99    10.95    10.93    1.01x  OK
W11_fts5_index_and_bm25          45.05     45.10     1.001x    44.52    44.61    57.02    0.79x  OK
W12_rtree_insert_and_bbox        23.39     23.35     0.998x    23.22    23.11    24.63    0.95x  OK
W13_autocommit_wal_txns           4.57      4.66     1.021x     4.44     4.51     4.12    1.13x  OK
W14_cli_sql_script               34.93     35.67     1.021x    34.59    34.95    26.19    1.36x  OK
TOTAL                           244.58    245.62     1.004x   geomean cand/base = 1.008x   digest mismatches = 0

### EXTRA build, FULL, all four knobs (SQLITERS_BTREE_C_DELETE_BALANCE=1 SQLITERS_INDEX_BULK_FILL=1 SQLITERS_BTREE_C_INSERT_BALANCE=1 SQLITERS_BTREE_C_ALLOC=1)
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.44      7.47     1.004x     7.41     7.44     9.60    0.78x  OK
W02_create_indexes                7.43      7.91     1.065x     7.39     7.88     8.76    0.90x  OK
W03_pk_point_select              27.99     28.50     1.018x    27.62    27.87    23.30    1.22x  OK
W04_secondary_idx_range           7.86      7.87     1.001x     7.82     7.71     7.46    1.05x  OK
W05_full_scan_filter_agg          4.11      4.32     1.051x     4.09     4.30    16.57    0.26x  OK
W06_join_group_order             10.66     10.57     0.992x    10.64    10.53    28.23    0.37x  OK
W07_window_functions             38.31     39.30     1.026x    38.24    39.06    50.21    0.78x  OK
W08_indexed_update_delete         9.42     11.37     1.206x     9.42    11.34     7.96    1.43x  OK
W09_like_text_scan               11.98     11.79     0.985x    11.96    11.77    18.92    0.62x  OK
W10_jsonb_extract_agg            11.06     21.76     1.967x    11.00    21.63    10.93    1.99x  OK
W11_fts5_index_and_bm25          45.08     45.06     1.000x    44.63    44.99    57.02    0.79x  OK
W12_rtree_insert_and_bbox        23.50     23.26     0.990x    23.35    23.14    24.63    0.94x  OK
W13_autocommit_wal_txns           4.62      4.49     0.971x     4.45     4.43     4.12    1.09x  OK
W14_cli_sql_script               34.87     38.68     1.109x    34.35    38.31    26.19    1.48x  OK
TOTAL                           244.33    262.34     1.074x   geomean cand/base = 1.079x   digest mismatches = 0

### EXTRA build, FULL, knobs OFF
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          7.59      7.57     0.998x     7.49     7.49     9.60    0.79x  OK
W02_create_indexes                7.58      7.70     1.015x     7.48     7.62     8.76    0.88x  OK
W03_pk_point_select              28.08     28.11     1.001x    27.75    27.68    23.30    1.21x  OK
W04_secondary_idx_range           7.88      7.84     0.995x     7.80     7.81     7.46    1.05x  OK
W05_full_scan_filter_agg          4.10      4.31     1.049x     4.09     4.30    16.57    0.26x  OK
W06_join_group_order             10.66     10.48     0.983x    10.63    10.45    28.23    0.37x  OK
W07_window_functions             39.04     39.21     1.005x    38.58    38.30    50.21    0.78x  OK
W08_indexed_update_delete         9.46      9.46     1.000x     9.43     9.42     7.96    1.19x  OK
W09_like_text_scan               11.97     11.88     0.993x    11.95    11.77    18.92    0.63x  OK
W10_jsonb_extract_agg            11.04     11.06     1.001x    10.99    10.96    10.93    1.01x  OK
W11_fts5_index_and_bm25          45.51     45.72     1.005x    44.92    45.16    57.02    0.80x  OK
W12_rtree_insert_and_bbox        23.44     23.68     1.010x    23.31    23.00    24.63    0.96x  OK
W13_autocommit_wal_txns           4.91      4.94     1.005x     4.59     4.80     4.12    1.20x  OK
W14_cli_sql_script               35.51     35.92     1.012x    35.24    35.65    26.19    1.37x  OK
TOTAL                           246.77    247.88     1.004x   geomean cand/base = 1.005x   digest mismatches = 0

```
## perfstat_final.txt
```
W02 HEAD (step5b_bin)                    instructions:u=119331249 cycles:u=43290215
W02 E+F build, knobs off                 instructions:u=119373265 cycles:u=43369027
W02 E+F build, DELETE_BALANCE            instructions:u=119373820 cycles:u=43924218
W02 E+F build, BULK_FILL                 instructions:u=122110263 cycles:u=44192012
W02 E+F build, both                      instructions:u=122112081 cycles:u=44224911
W02 extra build, knobs off               instructions:u=119318976 cycles:u=43798209
W02 extra build, all four knobs          instructions:u=122187703 cycles:u=44909470
W02 C (c_speedbench)                     instructions:u=129298976 cycles:u=53453385
W08 HEAD (step5b_bin)                    instructions:u=178341928 cycles:u=70043323
W08 E+F build, knobs off                 instructions:u=178428165 cycles:u=70291649
W08 E+F build, DELETE_BALANCE            instructions:u=181143255 cycles:u=71466648
W08 E+F build, BULK_FILL                 instructions:u=183321704 cycles:u=72054086
W08 E+F build, both                      instructions:u=185775648 cycles:u=72942193
W08 extra build, knobs off               instructions:u=178363293 cycles:u=70367618
W08 extra build, all four knobs          instructions:u=194382133 cycles:u=76795373
W08 C (c_speedbench)                     instructions:u=170186718 cycles:u=75317708
W14 HEAD (step5b_bin)                    instructions:u=198317818 cycles:u=90907797
W14 E+F build, knobs off                 instructions:u=198315589 cycles:u=92395761
W14 E+F build, DELETE_BALANCE            instructions:u=198562685 cycles:u=92645046
W14 E+F build, BULK_FILL                 instructions:u=198309734 cycles:u=92762357
W14 E+F build, both                      instructions:u=198559708 cycles:u=92784583
W14 extra build, knobs off               instructions:u=198294840 cycles:u=91896913
W14 extra build, all four knobs          instructions:u=222686843 cycles:u=100825936
W14 C sqlite3_o3                         instructions:u=135750194 cycles:u=69385253
```
## health_table.txt
```
columns: off / DELETE_BALANCE / BULK_FILL / both / both+INSERT_BALANCE+ALLOC (extra) / C
scenario                                                 pages                                  fill %         depth                                 seq %  C-check logical= determ. rust-check errs
s01_tek_sayfa                                      2/2/2/2/2/2           18.8/18.8/18.8/18.8/18.8/18.8   0/0/0/0/0/0   100.0/100.0/100.0/100.0/100.0/100.0  ok True True True 0
s02_sirali_ekleme                      233/233/233/233/233/233           98.5/98.5/98.5/98.5/98.5/98.5   1/1/1/1/1/1   100.0/100.0/100.0/100.0/100.0/100.0  ok True True True 0
s03_rastgele_ekleme                    259/259/259/259/260/260           88.9/88.9/88.9/88.9/88.5/88.5   1/1/1/1/1/1               7.0/7.0/7.0/7.0/5.1/5.1  ok True True True 0
s04_indeksli                           488/488/488/488/488/488           92.6/92.6/92.6/92.6/92.6/92.6   2/2/2/2/2/2               8.6/8.6/8.6/8.6/8.6/8.6  ok True True True 0
s05_sonradan_indeks                    490/490/458/458/458/458           92.1/92.1/98.5/98.5/98.5/98.5   2/2/1/1/1/1   100.0/100.0/100.0/100.0/100.0/100.0  ok True True True 0
s06_karisik_4096                       296/251/296/251/249/249           62.4/73.5/62.4/73.5/74.1/74.1   2/2/2/2/2/2         39.7/26.4/39.7/26.4/27.9/27.9  ok True True True 0
s07_karisik_512                  2456/2214/2559/2214/2173/2173           64.3/71.0/61.8/71.0/72.3/72.3   4/4/4/4/4/4         12.3/10.0/14.4/10.0/12.5/12.8  ok True True True 0
s08_karisik_1024_autovacuum          1182/986/1181/986/972/972           60.6/72.2/60.7/72.2/73.3/73.3   3/3/3/3/3/3         26.0/15.7/29.8/15.7/16.4/16.6  ok True True True 0
s09_karisik_wal                        281/244/281/244/244/244           62.7/72.2/62.7/72.2/72.2/72.2   2/2/2/2/2/2         36.4/19.1/36.4/19.1/19.1/19.6  ok True True True 0
s10_without_rowid                      318/318/318/318/318/318           75.0/75.0/75.0/75.0/75.0/75.0   2/2/2/2/2/2               4.2/4.2/4.2/4.2/4.2/4.2  ok True True True 0
s11_tasma_sayfalari              1601/1601/1601/1601/1599/1599           72.1/72.7/71.1/72.7/72.6/72.6   3/3/3/3/3/3               2.2/2.3/2.2/2.3/2.8/7.1  ok True True True 0
s12_rtree                                    65/65/65/65/65/73           86.5/86.5/86.5/86.5/86.5/79.2   1/1/1/1/1/1    100.0/100.0/100.0/100.0/100.0/58.2  ok True True True 0
s13_rtree_yalniz_ekleme                      79/79/79/79/79/81           88.1/88.1/88.1/88.1/88.1/86.0   1/1/1/1/1/1    100.0/100.0/100.0/100.0/100.0/61.3  ok True True True 0
```
## dbstat_summary.txt
```
########## knob config: off (Rust file vs C file; rows = dbstat pages)
s01_tek_sayfa                 TOTAL                  pages     2/2     same     2  shape-same     2
s02_sirali_ekleme             TOTAL                  pages   233/233   same   233  shape-same   233
s03_rastgele_ekleme           TOTAL                  pages   259/260   same     1  shape-same     1
s04_indeksli                  TOTAL                  pages   488/488   same   408  shape-same   415
s05_sonradan_indeks           TOTAL                  pages   490/458   same   188  shape-same   188
s06_karisik_4096              TOTAL                  pages   296/249   same     4  shape-same    74
s07_karisik_512               TOTAL                  pages  2456/2173  same     3  shape-same    31
s08_karisik_1024_autovacuum   TOTAL                  pages  1176/967   same     2  shape-same    67
s09_karisik_wal               TOTAL                  pages   281/244   same     4  shape-same    54
s10_without_rowid             TOTAL                  pages   318/318   same   306  shape-same   318
s11_tasma_sayfalari           TOTAL                  pages  1518/1509  same     4  shape-same    73
s12_rtree                     TOTAL                  pages    65/73    same     3  shape-same    41
s13_rtree_yalniz_ekleme       TOTAL                  pages    79/81    same     8  shape-same    64
########## knob config: f (Rust file vs C file; rows = dbstat pages)
s01_tek_sayfa                 TOTAL                  pages     2/2     same     2  shape-same     2
s02_sirali_ekleme             TOTAL                  pages   233/233   same   233  shape-same   233
s03_rastgele_ekleme           TOTAL                  pages   259/260   same     1  shape-same     1
s04_indeksli                  TOTAL                  pages   488/488   same   408  shape-same   415
s05_sonradan_indeks           TOTAL                  pages   490/458   same   188  shape-same   188
s06_karisik_4096              TOTAL                  pages   251/249   same   192  shape-same   221
s07_karisik_512               TOTAL                  pages  2214/2173  same     9  shape-same    76
s08_karisik_1024_autovacuum   TOTAL                  pages   981/967   same     7  shape-same    72
s09_karisik_wal               TOTAL                  pages   244/244   same   184  shape-same   188
s10_without_rowid             TOTAL                  pages   318/318   same   306  shape-same   318
s11_tasma_sayfalari           TOTAL                  pages  1508/1509  same    54  shape-same   723
s12_rtree                     TOTAL                  pages    65/73    same     3  shape-same    41
s13_rtree_yalniz_ekleme       TOTAL                  pages    79/81    same     8  shape-same    64
########## knob config: e (Rust file vs C file; rows = dbstat pages)
s01_tek_sayfa                 TOTAL                  pages     2/2     same     2  shape-same     2
s02_sirali_ekleme             TOTAL                  pages   233/233   same   233  shape-same   233
s03_rastgele_ekleme           TOTAL                  pages   259/260   same     1  shape-same     1
s04_indeksli                  TOTAL                  pages   488/488   same   408  shape-same   415
s05_sonradan_indeks           TOTAL                  pages   458/458   same   458  shape-same   458
s06_karisik_4096              TOTAL                  pages   296/249   same     4  shape-same    74
s07_karisik_512               TOTAL                  pages  2559/2173  same     3  shape-same    33
s08_karisik_1024_autovacuum   TOTAL                  pages  1175/967   same     2  shape-same    67
s09_karisik_wal               TOTAL                  pages   281/244   same     4  shape-same    54
s10_without_rowid             TOTAL                  pages   318/318   same   306  shape-same   318
s11_tasma_sayfalari           TOTAL                  pages  1534/1509  same     5  shape-same    96
s12_rtree                     TOTAL                  pages    65/73    same     3  shape-same    41
s13_rtree_yalniz_ekleme       TOTAL                  pages    79/81    same     8  shape-same    64
########## knob config: on (Rust file vs C file; rows = dbstat pages)
s01_tek_sayfa                 TOTAL                  pages     2/2     same     2  shape-same     2
s02_sirali_ekleme             TOTAL                  pages   233/233   same   233  shape-same   233
s03_rastgele_ekleme           TOTAL                  pages   259/260   same     1  shape-same     1
s04_indeksli                  TOTAL                  pages   488/488   same   408  shape-same   415
s05_sonradan_indeks           TOTAL                  pages   458/458   same   458  shape-same   458
s06_karisik_4096              TOTAL                  pages   251/249   same   192  shape-same   221
s07_karisik_512               TOTAL                  pages  2214/2173  same     9  shape-same    76
s08_karisik_1024_autovacuum   TOTAL                  pages   981/967   same     7  shape-same    72
s09_karisik_wal               TOTAL                  pages   244/244   same   184  shape-same   188
s10_without_rowid             TOTAL                  pages   318/318   same   306  shape-same   318
s11_tasma_sayfalari           TOTAL                  pages  1508/1509  same    54  shape-same   723
s12_rtree                     TOTAL                  pages    65/73    same     3  shape-same    41
s13_rtree_yalniz_ekleme       TOTAL                  pages    79/81    same     8  shape-same    64
########## knob config: all (Rust file vs C file; rows = dbstat pages)
s01_tek_sayfa                 TOTAL                  pages     2/2     same     2  shape-same     2
s02_sirali_ekleme             TOTAL                  pages   233/233   same   233  shape-same   233
s03_rastgele_ekleme           TOTAL                  pages   260/260   same   260  shape-same   260
s04_indeksli                  TOTAL                  pages   488/488   same   481  shape-same   488
s05_sonradan_indeks           TOTAL                  pages   458/458   same   458  shape-same   458
s06_karisik_4096              TOTAL                  pages   249/249   same   247  shape-same   249
s07_karisik_512               TOTAL                  pages  2173/2173  same  1795  shape-same  2173
s08_karisik_1024_autovacuum   TOTAL                  pages   967/967   same   926  shape-same   967
s09_karisik_wal               TOTAL                  pages   244/244   same   242  shape-same   244
s10_without_rowid             TOTAL                  pages   318/318   same   310  shape-same   318
s11_tasma_sayfalari           TOTAL                  pages  1509/1509  same   324  shape-same  1509
s12_rtree                     TOTAL                  pages    65/73    same     3  shape-same    41
s13_rtree_yalniz_ekleme       TOTAL                  pages    79/81    same     8  shape-same    64
```
