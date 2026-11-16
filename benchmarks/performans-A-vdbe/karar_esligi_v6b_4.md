# V6b.4 — synchronous hazırlama ve eşitleme bayrakları

Başlangıç **556932a**. C SQLite **3.54.1**, Rust **1.100.0**. ileriPlanlama/ girdi olarak kullanılmadı. V6b sürüyor.

| C koşulu | Karar | Sonuç | Rust karşılığı |
|---|---|---|---|
| pragma.c:519–523 | PragFlg_NeedSchema | Hazırlamada şema okunur. | crates/sqliters-core/src/engine.rs:1544 |
| pragma.c:1133–1134 | RHS yok | safety_level-1 sonucu hazırlamada dondurulur; TEMP 0. | crates/sqliters-core/src/lib.rs:5825 |
| pragma.c:1136–1139 | !autoCommit; TEMP dahil | Hazırlama hatası: Safety level may not be changed inside a transaction. | crates/sqliters-codegen/src/pragma.rs:458 |
| pragma.c:1140 | iDb==1 ve autocommit | TEMP setter etkisiz; boş program. | crates/sqliters-codegen/src/pragma.rs:458 |
| pragma.c:1142–1149 | İlk ASCII rakam; Atoi / getSafetyLevel; (iLevel+1)&7; sıfır→1 | i32 taşması, önek dönüşümü ve 4–6 korunur; negatif ve tanınmayan sözcük 1. | crates/sqliters-codegen/src/pragma.rs:458 |
| pragma.c:73–93 | omitFull; on/no/off/false/yes/true/extra/full | Boolean ve synchronous aynı C yardımcısını kullanır. | crates/sqliters-codegen/src/pragma.rs:160 |
| pragma.c:1148–1150 | safety_level, bSyncSet, setAllPagerFlags | Hazırlamada kullanıcı ayarı işaretlenir; step ayarı tekrar uygulamaz. EXPLAIN aynı etkiyi yapar. | crates/sqliters-core/src/lib.rs:5749 |
| pragma.c:241–260 | autoCommit; her açık Btree | Bayraklar ve cache spill her açık sayfalayıcıya uygulanır. İşlem içinde bayrak hazırlama uygulamayı erteler. | crates/sqliters-core/src/lib.rs:5762 |
| attach.c:243–248 | Yeni bağlı veritabanı | fullfsync/checkpoint_fullfsync bağlantı bayraklarını devralır. | crates/sqliters-core/src/engine.rs:1201 |
| pager.c:3671–3691 | tempFile veya OFF; level>=FULL; level==EXTRA | 0 noSync; 2–6 fullSync; yalnız 3 dizin eşitlemesi. Seviye 4–6 EXTRA değildir. | crates/sqliters-pager/src/lib.rs:762 |
| pager.c:3693–3707 | FULLFSYNC ve CKPT_FULLFSYNC | Dosya eşitleme türü güvenlik seviyesinden ayrıdır; checkpoint FULL bayrağı ayrıca hesaplanır. | crates/sqliters-pager/src/lib.rs:1160 |
| pager.c:4410–4427 | Journal başlığı öncesi FULL, sonrası FULL\|DATAONLY | Birleşik 0x13 bayrağı yeni VFS varyantıyla korunur. | crates/sqliters-pager/src/lib.rs:1850 |
| pager.c:1449–1451,2134–2135,6462–6469 | Journal sonlandırma / veritabanı sync | PERSIST data-only, TRUNCATE ve db çağrıları C syncFlags kullanır. | crates/sqliters-pager/src/lib.rs:1165 |
| os_unix.c:unixSync/full_fsync | Normal / full / data-only / full+data-only | İki bağımsız bayrak gerçek VFS alt yordamına aktarılır; Tcl sayaçları FULL birleşimini sayar. | crates/sqliters-vfs/src/lib.rs:158 |

## Doğrulama

```text
statements 5082 | generator accepted 4829: identical 4829, DIFFERENT 0 | unsupported (fallback) 253 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     230  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 5082 | generator accepted 4829: identical 4829, DIFFERENT 0 | unsupported (fallback) 253 (of which same as C anyway 235) | not reached (old parser/other) 0 (same 0)
   unsupported     230  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported      10  PRAGMA page_size prepare effect (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 765 | generator accepted 765: identical 765, DIFFERENT 0 | unsupported (fallback) 0 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
```

```text
{"same": 4589, "unsupported": 235}
```

```text
API lifetime, stdout and file bytes identical in both modes
```

```text
IO matrix 896 same 896 different 0
```

VFS gözlemcileri aynı gerçek Unix dosya işlemlerini devreder; SQL çıktısına ek olarak xSync/xDelete sırası, tam bayrak değeri, sync-dir ve kapanmış .db baytları karşılaştırılır. 896 senaryo: iki makine anahtarı × dört journal kipi × 28 değer × dört fullfsync/checkpoint_fullfsync birleşimi. Çıktı, olaylar ve .db baytları tümünde aynı. C tarafında değişiklik yok; gözlemci target/perf-work/v6b4 içinde derlendi.

Hazırlama API sırası ayrıca canlı setter, dondurulmuş sorgu, başka setter sonrasında eski setter step, EXPLAIN, TEMP ve işlem içi hatayı kapsar. İki anahtarda stdout ve .db baytları aynı (res/manual_api.json). tests/v6b_pragma_prepare.rs iki yeni regresyon testi içerir.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 510 / 990 (51.5%) | 368 / 706 (52.1%) |
| btree.c | 15 / 15 (100.0%) | 4 / 4 (100.0%) |
| pager.c | 29 / 30 (96.7%) | 19 / 20 (95.0%) |
| **total** | 554 / 1035 (53.5%) | 391 / 730 (53.6%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 49.3 | 675 | 49.9 | 152 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 96.0 | 18 | 94.4 | 1 |

Ölçüm sqlite3Pragma içindeki taşınmayan yolları da içerir; tamamlanma yüzdesi değildir. C VFS gözlemcisinin gerçek GÇ matrisi de bu adımın gcov sayaçlarına eklendi. Her alınmayan dal ayrı satırda [karar_esligi_v6b_4_dallar.md](karar_esligi_v6b_4_dallar.md). sqlite3PagerSetFlags içinde yalnız PAGER_CACHESPILL=0 yönü alınmadı; cache_spill işleyicisi ayrı açık eksik. Derlemede kapalı BATCH_ATOMIC_WRITE koşulu çalışan referansta yok.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1045, 0, 337), açık (1030, 15, 337). Açık 15 hata adı önceki adımla aynı. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; bilinen g06 stderr farkı sürüyor. Test beklentileri değiştirilmedi.

CPU 6 ve tmpfs üzerinde Rust 1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.26      8.82     0.953x     9.19     8.75     9.60    0.92x  OK
W02_create_indexes                5.70      5.65     0.991x     5.63     5.64     8.76    0.65x  OK
W03_pk_point_select              30.53     29.87     0.979x    29.85    29.49    23.30    1.28x  OK
W04_secondary_idx_range           9.46      9.88     1.044x     9.33     9.61     7.46    1.32x  OK
W05_full_scan_filter_agg          4.39      4.25     0.966x     4.39     4.23    16.57    0.26x  OK
W06_join_group_order             10.78     10.88     1.010x    10.77    10.87    28.23    0.39x  OK
W07_window_functions             37.07     37.68     1.017x    37.02    37.59    50.21    0.75x  OK
W08_indexed_update_delete        11.96     11.94     0.999x    11.87    11.88     7.96    1.50x  OK
W09_like_text_scan               12.43     12.38     0.997x    12.41    12.25    18.92    0.65x  OK
W10_jsonb_extract_agg            13.81     13.70     0.992x    13.66    13.68    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.27     69.44     1.003x    68.73    69.23    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.46     40.50     1.001x    39.57    39.68    24.63    1.64x  OK
W13_autocommit_wal_txns           4.64      4.65     1.002x     4.45     4.57     4.12    1.13x  OK
W14_cli_sql_script               38.73     39.22     1.013x    38.41    38.78    26.19    1.50x  OK
TOTAL                           298.49    298.88     1.001x   geomean cand/base = 0.997x   digest mismatches = 0
```

## Sınırlar ve yeniden çalıştırma

Yetkilendirici ve özel VFS PRAGMA override hâlâ üreticide Unsupported. Bellek yetmezliği ve her GÇ hatası enjeksiyonu bu adımın kanıtı değildir. E28 COMMIT hata durumu açık kalır. Synchronous dışındaki yanlış eşitlemelerin kapandığına dair genel iddia yok; gözlem matrisi yukarıdaki normal işlem akışını kapsar.

Yerel kök /home/hakan/github/sqliters/target/perf-work/v6b4/. Girdiler synchronous.sql, synchronous.ops; gerçek GÇ kaynakları sync_probe.c ve sync-r/src/main.rs; ham sonuçlar res/ altında. res/source_manifest.json test edilen kaynak SHA256 değerlerini içerir. bin/sqliters ve bin/rdrv6 dondurulmuş ikililerdir. PATH başında C 3.54.1 ile cargo test --workspace --no-fail-fast iki anahtar durumunda çalıştırıldı.

Sonraki işlem page_size: C nextPagesize bağlantı geneli hazırlama durumu, TEMP açılması ve VACUUM kullanımı. Ardından auto_vacuum; doğruluk tamamlanmadan hız ayarı yapılmayacak.
