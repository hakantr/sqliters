# V6b.6 — auto_vacuum hazırlama, meta7 ve canlı Btree durumu

Başlangıç **94421ed**. C SQLite **3.54.1**, Rust **1.100.0**. ileriPlanlama/ değerlendirilmedi. V6b sürüyor.

| C koşulu | Karar | Sonuç | Rust karşılığı |
|---|---|---|---|
| pragma.c:519–523; AUTO_VACUUM NeedSchema | Şema gerekli | Hazırlamada şema okunur; C hatası/Unsupported ayrımı korunur. | crates/sqliters-core/src/engine.rs:1545 |
| pragma.c:805–806; btree.c:3258–3270 | RHS yok | Canlı auto/incr bayraklarından 0/1/2 sonucu alınır ve hazırlanan programa dondurulur. | crates/sqliters-core/src/lib.rs:5849 |
| pragma.c:125–132 | none/full/incremental tam eşleşme; diğerleri Atoi 0..2 | Başta boşluklu sözcükler ve sayılar 0; 2junk 2; taşma 0; geçerli hex ve işaret C gibi. | crates/sqliters-codegen/src/pragma.rs:178 |
| pragma.c:808–810 | RHS var; SetAutoVacuum başarısından önce | nextAutovac her istek için bağlantı genelinde saklanır; reddedilen istek de sonraki VACUUM işlemini etkiler. | crates/sqliters-core/src/lib.rs:5748 |
| btree.c:3232–3252 | fixed ve sıfır/nonzero yeteneği farklı | READONLY ayar programı yazmasızdır; Full↔Incremental aynı yetenekte hemen uygulanır. | crates/sqliters-pager/src/lib.rs:977 |
| pragma.c:816–817 | SetAutoVacuum OK ve eAuto 1/2 | Transaction, ReadCookie(meta4), If, erken Halt ve SetCookie(meta7=eAuto-1) aynen üretilir. | crates/sqliters-codegen/src/pragma.rs:283 |
| pragma.c:827–843 | iDb, If hedefi, donmuş meta7 operandı | Hazırlanan yazma/boş program kararı ve operand Statement içinde saklanır; step RHS veya global isteği yeniden uygulamaz. | crates/sqliters-core/src/lib.rs:5839 |
| pragma.c:827–831; vdbe.c OP_Transaction | Önce write transaction; meta4 sıfır olsa da | Hazırlama sonrası None ile iptal edilmiş yazma yine sayfa1 yaratır; erken Halt meta7 yazımını atlar. | crates/sqliters-core/src/engine.rs:4018 |
| btree.c:10521–10542 | SetCookie meta7; PagerWrite başarılı | Disk çerezi ve canlı incrVacuum bayrağı hazırlanmış değerle birlikte güncellenir. | crates/sqliters-core/src/engine.rs:4018 |
| btree.c:3565–3576 | Yeni sayfa1 başlığı | 24..99 aralığı önce sıfırlanır; auto/incr ve sayfa sayısı sonra yazılır. SQLite sürüm damgası commit yazımında konur. | crates/sqliters-pager/src/lib.rs:3364 |
| pragma.c:443–447 | RunOnlyOnce | Boş setter de yazmalı setter de step sonrasında kendini geçersiz kılar; reset yeniden hazırlar. | crates/sqliters-core/src/lib.rs:10383 |
| build.c:5404–5434; btree.c:2729–2755 | Yeni TEMP Btree; normal/EXPLAIN hazırlama | Normal TEMP gerçek durumu açıp değiştirir. Salt okunur üretici incelemesi yeni TEMP için flags=0/fixed=0 hesaplar, bağlantıyı değiştirmez. | crates/sqliters-core/src/cgen.rs:101 |
| vacuum.c:289–291 | nextAutovac>=0 | Hedefin yeniden kurulacak modu global istekten, henüz istek yoksa kaynak Btree modundan seçilir. | crates/sqliters-core/src/engine.rs:41828 |
| btree.c:4370–4409,4432–4459,4552–4599 | Commit/rollback; incrVacuum için yeni atama yok | Canlı bayrak disk sayfa1 geri yüklemesinden ayrı tutulur; commit/rollback onu kendiliğinden geri almaz. | crates/sqliters-pager/src/lib.rs:4934 |
| btree.c:4648–4688 | Savepoint rollback | Geri alınan sayfa1, canlı incrVacuum bayrağını değiştirmez. | crates/sqliters-pager/src/lib.rs:5538 |
| btree.c:4648–4676; newDatabase | Boş savepoint geri alması | Btree katmanında boş sayfa1 yeniden oluşturulur; genel pager geri almasıyla yetinilmez. | crates/sqliters-pager/src/lib.rs:5556 |
| btree.c:3451–3454 | lockBtree yeni okuma işlemi | Gerçek tablo okuması başladığında canlı bayraklar başlıktan yenilenir; bellek ve henüz diske çıkmamış TEMP de buna dahildir. | crates/sqliters-pager/src/lib.rs:2713 |

## Doğrulama

```text
statements 6748 | generator accepted 6516: identical 6516, DIFFERENT 0 | unsupported (fallback) 232 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     219  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 6748 | generator accepted 6516: identical 6516, DIFFERENT 0 | unsupported (fallback) 232 (of which same as C anyway 214) | not reached (old parser/other) 0 (same 0)
   unsupported     219  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 2431 | generator accepted 2431: identical 2431, DIFFERENT 0 | unsupported (fallback) 0 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
```

```text
{"same": 5860, "unsupported": 214}
```

```text
220 API lifetime flows, stdout and every .db byte identical in both modes
```

```text
IO matrix 896 same 896 different 0
```

220 API akışında stdout ve bütün .db dosyalarının baytları aynı: üç başlangıç modu × main/TEMP/aux × geçerli, geçersiz ve taşan değerler; hazırlama/step/reset; eski setter operandı; gerçek yazma ve VACUUM; iptal edilmiş yazmanın boş sayfa yaratması; COMMIT, ROLLBACK, savepoint ve sonraki okuma. Dondurulmuş sorgu ve global istek ayrı doğrulandı. Makine anahtarı iki durumda da sınandı. İlk TEMP programı üretici incelemesinde de kabul edilir; salt okunur inceleme gerçek TEMP durumunu açmaz/değiştirmez.

Sekiz yeni test tests/v6b_auto_vacuum_prepare.rs içinde. C dönüşüm beklentileri ayrıca res/conversion_reference.json ile doğrulandı: tırnak içinde başta boşluk olan sayı 0, 2junk ise 2. Önceki ikiliye karşı canlı durum farkları state_probe.json içinde saklandı.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 535 / 996 (53.7%) | 390 / 716 (54.5%) |
| btree.c | 192 / 223 (86.1%) | 92 / 126 (73.0%) |
| pager.c | 57 / 61 (93.4%) | 38 / 46 (82.6%) |
| **total** | 784 / 1280 (61.2%) | 520 / 888 (58.6%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 51.3 | 675 | 51.7 | 148 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 96.0 | 18 | 94.4 | 1 |
| sqlite3PagerSetPagesize | pager.c | 31 | 90.3 | 26 | 73.1 | 7 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| getAutoVacuum | pragma.c | 6 | 100.0 | 10 | 100.0 | 0 |
| sqlite3BtreeSetAutoVacuum | btree.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3BtreeGetAutoVacuum | btree.c | 7 | 100.0 | 4 | 100.0 | 0 |
| btreeEndTransaction | btree.c | 15 | 86.7 | 8 | 75.0 | 2 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 71.4 | 4 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 64.3 | 3 |
| lockBtree | btree.c | 73 | 65.8 | 44 | 56.8 | 18 |
| newDatabase | btree.c | 24 | 100.0 | 4 | 75.0 | 1 |

Bu ölçüm taşınmayan işleyicileri de içerir; tamamlanma yüzdesi değildir. getAutoVacuum, sqlite3BtreeSetAutoVacuum ve sqlite3BtreeGetAutoVacuum dalları %100 alındı. Her alınmayan dal ayrı [karar_esligi_v6b_6_dallar.md](karar_esligi_v6b_6_dallar.md). Tahsis ve her GÇ hatası, authorizer, özel VFS PRAGMA override, tüm hata sonrası pager durumları bu kanıtla kapanmaz. E28 ayrı açık eksiktir.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1057, 0, 337), açık (1042, 15, 337). Açık 15 hata adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; g06 stderr farkı sürüyor. Beklentiler değiştirilmedi.

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.33      8.85     0.949x     9.19     8.74     9.60    0.92x  OK
W02_create_indexes                5.76      5.80     1.007x     5.72     5.69     8.76    0.66x  OK
W03_pk_point_select              30.24     30.20     0.999x    29.38    29.50    23.30    1.30x  OK
W04_secondary_idx_range           9.46      9.78     1.034x     9.38     9.64     7.46    1.31x  OK
W05_full_scan_filter_agg          4.40      4.24     0.963x     4.39     4.23    16.57    0.26x  OK
W06_join_group_order             10.81     11.15     1.032x    10.78    11.14    28.23    0.40x  OK
W07_window_functions             37.44     37.37     0.998x    37.22    37.18    50.21    0.74x  OK
W08_indexed_update_delete        12.10     12.06     0.997x    12.07    11.84     7.96    1.51x  OK
W09_like_text_scan               12.45     12.70     1.020x    12.42    12.38    18.92    0.67x  OK
W10_jsonb_extract_agg            13.72     13.44     0.980x    13.66    13.39    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.35     69.75     1.006x    69.21    69.02    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.51     39.28     0.970x    39.71    39.14    24.63    1.59x  OK
W13_autocommit_wal_txns           4.58      4.70     1.026x     4.44     4.44     4.12    1.14x  OK
W14_cli_sql_script               39.04     39.65     1.016x    38.69    39.21    26.19    1.51x  OK
TOTAL                           299.19    298.95     0.999x   geomean cand/base = 0.999x   digest mismatches = 0
```

## Yerel kanıt

Kök /home/hakan/github/sqliters/target/perf-work/v6b6/. auto_vacuum.sql ve auto_*.ops girdiler; program/API sürücüleri bin/ altında; ham sonuçlar res/ içinde. res/source_manifest.json test edilen kaynak SHA256 değerlerini tutar. C 3.54.1 PATH başında cargo test --workspace --no-fail-fast iki anahtar durumunda çalıştırıldı. C kaynak ağacı değiştirilmedi; gcov ayrı dizine yönlendirildi.

Sıradaki uygun dilim cache_spill, busy_timeout ve wal_autocheckpoint hazırlama etkileri ve sonuç anıdır. V6b, M4 ve diğer C eşliği borçları bitmeden performans ayarı yapılmayacak.
