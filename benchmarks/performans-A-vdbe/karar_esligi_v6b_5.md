# V6b.5 — page_size hazırlama, açılış ve global VACUUM isteği

Başlangıç **a608629**. C SQLite **3.54.1**, Rust **1.100.0**. ileriPlanlama/ girdi olarak kullanılmadı. V6b sürüyor.

| C koşulu | Karar | Sonuç | Rust karşılığı |
|---|---|---|---|
| pragma.c:519–523; pragma.h PAGE_SIZE bayrağı | NeedSchema yok | Hazırlamada şema yüklenmez, SHARED kilidi alınmaz. | crates/sqliters-codegen/src/pragma.rs:425 |
| btree.c:2716–2767 | Açılışta 100 ham bayt; 16/17 geçerli sayfa boyutu | C gibi imza doğrulamadan boyut, reserve, fixed ve auto-vacuum ön bilgisi alınır. İmza/içerik daha sonraki şema okumasında doğrulanır. | crates/sqliters-pager/src/lib.rs:2625 |
| btree.c:2741–2767 | Boyut <512, >65536 veya ikinin kuvveti değil | Açılış varsayılanı korunur; geçerli 65536 değeri başlıkta 0x0001 olarak çözülür. | crates/sqliters-pager/src/lib.rs:2625 |
| pragma.c:601–603; btree.c:3138–3140 | RHS yok | Canlı Btree boyutu hazırlanmış sorguya gömülür; step anında tekrar okunmaz. | crates/sqliters-core/src/lib.rs:5830 |
| pragma.c:607 | RHS var; sqlite3Atoi | Raw i32 istek, geçersiz olsa da önceki isteğin yerini alır; bağlantının tüm şemaları için ortaktır. | crates/sqliters-core/src/lib.rs:5748 |
| btree.c:3109–3115 | PRAGMA nReserve=0; mevcut reserve üst sınırı korunur | Etkin reserve azaltılmaz. Bu SQL yüzeyinde requested-reserve üst sınırı mevcut reserve ile temsil edilir; ayrı file-control reserve API yüzeyi taşınmış sayılmaz. | crates/sqliters-pager/src/lib.rs:2648 |
| btree.c:3116–3118 | BTS_PAGESIZE_FIXED | Boyut değişmez; global nextPagesize yine de güncellenmiştir. | crates/sqliters-pager/src/lib.rs:2648 |
| btree.c:3120–3125 | 512..65536, ikinin kuvveti; 512 ve reserve>32 | Geçerli yeni boyut uygulanır; gerekli en küçük boyut 1024 olur. | crates/sqliters-pager/src/lib.rs:2648 |
| pager.c:3836–3868 | Yeni boyut farklı ve değiştirmeye uygun | Önbellek geometrisi temizlenir, data_version artar, backup yeniden başlatma bildirimi yapılır. | crates/sqliters-pager/src/lib.rs:2648 |
| pragma.c:608–610 | SetPageSize rc READONLY veya diğer hata; NOMEM özel | Normal geçersiz/fixed istek boş program verir. NOMEM ve genel pager hata yönleri ayrıca kanıtlanmadı; aşağıdaki dal listesi açık tutar. | crates/sqliters-codegen/src/pragma.rs:425 |
| build.c:5406–5434 | TEMP ilk açılışı | Önce önceki global istekle TEMP açılır; yeni TEMP setter daha sonra uygulanır. ATTACH kendi açılış varsayılanını kullanır. | crates/sqliters-core/src/db_set.rs:1375 |
| pragma.c:443–447 | RunOnlyOnce; boş setter ve tek satır sorgu | Normal step sonrasında reset yeniden hazırlar; EXPLAIN listelemesi hazırlama etkisini yapar, programı çalıştırmaz. | crates/sqliters-core/src/lib.rs:5830 |
| vacuum.c:282–284 | VACUUM hedefi; kaynak memdb değil | İstenen sayfa boyutu şemaya bağlı değil; bağlantıdaki son raw istektir. Normal VACUUM ve INTO isteği temizlemez. | crates/sqliters-core/src/engine.rs:41849 |
| vacuum.c:274–278 | Kaynak WAL ve INTO yok | Global nextPagesize sıfırlanır; daha sonraki başka şemanın VACUUM işlemi de bu sıfırı görür. | crates/sqliters-core/src/engine.rs:42038 |
| vacuum.c:282–284; btree.c:3125 | reserve64 + istek512 | VACUUM çıktısı 1024 sayfadır; gerçek dosya baytları C ile karşılaştırıldı. | crates/sqliters-core/src/engine.rs:41383 |

## Doğrulama

```text
statements 5788 | generator accepted 5545: identical 5545, DIFFERENT 0 | unsupported (fallback) 243 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     230  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 5788 | generator accepted 5545: identical 5545, DIFFERENT 0 | unsupported (fallback) 243 (of which same as C anyway 225) | not reached (old parser/other) 0 (same 0)
   unsupported     230  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 1471 | generator accepted 1471: identical 1471, DIFFERENT 0 | unsupported (fallback) 0 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
```

```text
{"same": 5129, "unsupported": 225}
```

```text
176 API lifetime flows, stdout and every .db byte identical in both modes
```

```text
12 raw-header cases have identical output and database bytes
```

```text
64 reserved bytes: request 512 -> VACUUM 1024, both modes output and bytes identical
```

```text
IO matrix 896 same 896 different 0
```

```text
open 0
setter 0
query 0
row 4096
```

176 API akışında hazırlama/step, donmuş sorgu, EXPLAIN, boş işlem, bağlı şema, TEMP, geçersiz/taşan değerler, VACUUM isteğinin korunması ve WAL sıfırlaması karşılaştırıldı. Her akışın stdout ve bütün .db dosyalarının baytları aynı. 12 geçersiz imzalı ham başlık dosyası C gibi şema okumadan page_size verir. Reserve64 dosyası C file-control ile oluşturuldu; iki kipte aynı VACUUM dosyası üretildi.

Dört yeni test tests/v6b_page_size_prepare.rs içinde. Özel kilit testi iki bağlantı kullanır: biri EXCLUSIVE kilit tutarken diğeri açılır ve page_size setter/query hazırlaması başarıyla tamamlanır. Aynı sıra C 3.54.1 kitaplığı ile lock_probe.c içinde sınandı. .db açılış başlığı ham okunur; bu aşamada SHARED kilidi veya şema doğrulaması eklenmez.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 512 / 990 (51.7%) | 371 / 706 (52.5%) |
| btree.c | 40 / 40 (100.0%) | 26 / 26 (100.0%) |
| pager.c | 57 / 61 (93.4%) | 37 / 46 (80.4%) |
| **total** | 609 / 1091 (55.8%) | 434 / 778 (55.8%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 49.5 | 675 | 50.4 | 150 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 96.0 | 18 | 94.4 | 1 |
| sqlite3PagerSetPagesize | pager.c | 31 | 90.3 | 26 | 69.2 | 8 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |

Kapsam sqlite3Pragma içindeki taşınmayan işleyicileri de içerir; tamamlanma yüzdesi değildir. BtreeSetPageSize kararları C reserve fixture + VACUUM akışıyla da sayıldı. Her alınmayan dal ayrı: [karar_esligi_v6b_5_dallar.md](karar_esligi_v6b_5_dallar.md). Genel pager boyutu yordamının canlı referans, dolu memdb ve hata yönleri; OOM; authorizer ve özel VFS PRAGMA override için açık sınırlar listede tutulur. Bu adım bu yolların tam eşlik kanıtı değildir.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1049, 0, 337), açık (1034, 15, 337). Açık 15 hatanın adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; g06 stderr farkı sürüyor. Beklentiler değiştirilmedi.

CPU 6, tmpfs; Rust 1.100 tabanına karşı 7 A/B denemesi, anahtar kapalı; hız ayarı yok.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.29      8.90     0.958x     9.24     8.83     9.60    0.93x  OK
W02_create_indexes                5.71      5.74     1.005x     5.68     5.69     8.76    0.66x  OK
W03_pk_point_select              30.24     30.34     1.003x    29.65    29.79    23.30    1.30x  OK
W04_secondary_idx_range           9.54      9.68     1.015x     9.40     9.52     7.46    1.30x  OK
W05_full_scan_filter_agg          4.42      4.20     0.950x     4.41     4.18    16.57    0.25x  OK
W06_join_group_order             10.80     10.96     1.015x    10.78    10.94    28.23    0.39x  OK
W07_window_functions             37.30     37.45     1.004x    37.07    37.28    50.21    0.75x  OK
W08_indexed_update_delete        12.08     11.95     0.989x    11.99    11.70     7.96    1.50x  OK
W09_like_text_scan               12.48     12.51     1.002x    12.42    12.50    18.92    0.66x  OK
W10_jsonb_extract_agg            13.69     13.47     0.984x    13.65    13.36    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.64     69.59     0.999x    68.79    69.19    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.24     41.53     1.032x    40.19    40.90    24.63    1.69x  OK
W13_autocommit_wal_txns           4.64      4.74     1.022x     4.49     4.59     4.12    1.15x  OK
W14_cli_sql_script               39.11     39.62     1.013x    38.84    39.12    26.19    1.51x  OK
TOTAL                           299.18    300.67     1.005x   geomean cand/base = 0.999x   digest mismatches = 0
```

## Yerel kanıt

Kök /home/hakan/github/sqliters/target/perf-work/v6b5/. page_size.sql, page_*.ops, lock_probe.c, reserve_probe.c ve Python sürücüleri burada; res/source_manifest.json test edilen kaynak SHA256 değerlerini tutar. İkililer bin/ altında donduruldu. C 3.54.1 PATH başında cargo test --workspace --no-fail-fast iki anahtar durumunda çalıştırıldı. C kaynak ağacı değiştirilmedi; gcov ayrı hedef dizinine yönlendirildi.

Sonraki adım auto_vacuum: global nextAutovac, hazırlama etkisi, dondurulmuş meta7 yazımı, TEMP ve rollback sonrası Btree durumu. V6b ve M4 henüz tamamlanmadı.
