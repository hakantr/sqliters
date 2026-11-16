# V6b.7 — cache_spill, busy_timeout ve wal_autocheckpoint

Başlangıç **9a222cd**. C SQLite **3.54.1**, Rust **1.100.0**. ileriPlanlama/ değerlendirilmedi. V6b sürüyor.

| C koşulu | Karar | Sonuç | Rust karşılığı |
|---|---|---|---|
| pragma.c:916–933; pragma.h CACHE_SPILL | NeedSchema bayrağı yok | cache_spill sorgusu şema çerezinden cache_size başlatmaz; gerçek pager cache geometrisi kullanılır. | crates/sqliters-core/src/cgen.rs:125 |
| pragma.c:919–921 | Global CacheSpill kapalı / açık | Kapalıysa sıfır; açıksa max(cache sayfa sayısı, spill eşiği). Sonuç hazırlanan programa dondurulur. | crates/sqliters-codegen/src/pragma.rs:360 |
| pragma.c:923–926 | GetInt32 başarılı / başarısız | Başarılıysa yalnız seçilen Btree eşiği ayarlanır; başarısızsa eşik korunur ve boolean varsayılanı 1 olur. | crates/sqliters-core/src/lib.rs:5759 |
| pcache.c:875–886 | mxPage sıfır, pozitif, negatif; cache/spill maksimumu | Sıfır salt sorgu; negatif KiB sayfa+136 maliyetine bölünür, i32 dönüşümü C gibi; sonuç cache ile maksimumdur. | crates/sqliters-pager/src/pcache.rs:1200 |
| pcache.c:277–291 | cache_size pozitif / negatif; negatif 1e9 sınırı | Pozitif değer doğrudan sayfa sayısıdır; yalnız negatif KiB hesabında 1e9 üst sınırı vardır. | crates/sqliters-pager/src/pcache.rs:1175 |
| pragma.c:927–933; 242–261 | Boolean globaldir; autocommit ise bütün açık pagere uygulanır | Eşik yerel, etkinlik bağlantı genelindedir. İşlem içindeki bayrak değişimi pagerlere hemen uygulanmaz. | crates/sqliters-core/src/lib.rs:5734 |
| attach.c:241–246; build.c:5404–5434 | ATTACH ve yeni TEMP farklı başlatılır | ATTACH global pager bayraklarını devralır; TEMP varsayılan spill davranışıyla açılır. | crates/sqliters-core/src/engine.rs:1202 |
| pragma.c:2656–2663; main.c:1876–1892 | busy_timeout geçerli i32 / geçersiz | Geçerli pozitif süre timeout handler kurar; sıfır/negatif kaldırır; geçersiz metin mevcut handlerı korur. | crates/sqliters-codegen/src/pragma.rs:338 |
| pragma.c:2662; main.c:1831 | Handler süreli / özel / yok | Hazırlanan sonuç o andaki süre veya sıfırdır; sonradan hazırlanan ayarı step yeniden uygulamaz. | crates/sqliters-core/src/lib.rs:5755 |
| pragma.c:2426–2436; main.c:2531–2545 | wal_autocheckpoint RHS / sorgu; nFrame pozitif / diğer | Atoi hatası sıfırdır. Pozitif eşik etkin, diğer değerler kapalı; hazırlanan sonuç donuktur. | crates/sqliters-codegen/src/pragma.rs:528 |
| pragma.c:443–447; EXPLAIN hazırlama | Normal hazırlama ve EXPLAIN | Üç ayarın etkisi hazırlamada uygulanır; step yalnız programı çalıştırır/listeler. Reset gerektiğinde yeniden hazırlar. | crates/sqliters-core/src/lib.rs:5870 |
| btree.c:2729–2755; build.c:5430; pager.c:3857–3869 | Yeni TEMP; nextPagesize varsayılandan farklı | Önce derleme varsayılanıyla açılır, sonra nextPagesize uygulanır; data_version da boyut değişimi için artar. | crates/sqliters-core/src/db_set.rs:1389 |
| pager.c:3861–3869; pcache.c:356–369 | Geçerli yeni sayfa boyutu | Pcache boyutu hemen güncellenir; henüz şema okumayan spill sorgusu yeni geometriyi görür. | crates/sqliters-pager/src/lib.rs:2660 |
| vdbeaux.c:2421–2507 | C tarafından üretilmiş EXPLAIN/EQP | Listeleme Btree okumasını veya komutların Transaction adımlarını çalıştırmaz; gereksiz saat/VFS çağrısı yoktur. | crates/sqliters-core/src/lib.rs:7691 |
| prepare.c:325–330; callback.c:181–188 | Main ilk şema yüklemesi; kayıtlı encoding ve !EncodingFixed | Kodlama aynı UTF-8 kalsa da bekleyen ifadeler advisory expire olur; çalışan ifadeler devam eder. | crates/sqliters-core/src/lib.rs:1864 |
| vdbeaux.c:5290–5295; vdbeapi.c:847–850,991–1023 | Expire edilmiş bekleyen V2 ifadesi | İlk step yeniden hazırlar: spill yeni 17 sonucunu, eski timeout setter yeniden 17 ayarını uygular. | crates/sqliters-core/src/lib.rs:8255 |

## Doğrulama

```text
statements 7900 | generator accepted 7701: identical 7701, DIFFERENT 0 | unsupported (fallback) 199 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
   unsupported     186  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 7900 | generator accepted 7701: identical 7701, DIFFERENT 0 | unsupported (fallback) 199 (of which same as C anyway 181) | not reached (old parser/other) 0 (same 0)
   unsupported     186  PRAGMA handler (V6b)
   unsupported      11  PRAGMA defer_foreign_keys transaction lifetime (V6b)
   unsupported       1  PRAGMA optimize statistics usage history (V6b)
   unsupported       1  PRAGMA table_list hidden view initialization history (V6b)
```

```text
statements 3583 | generator accepted 3583: identical 3583, DIFFERENT 0 | unsupported (fallback) 0 (of which same as C anyway 0) | not reached (old parser/other) 0 (same 0)
```

```text
{"same": 6757, "unsupported": 181}
```

```text
330 API lifetime flows, stdout and every .db byte identical in both modes
```

```text
Cold persistent-cache reopening: 16 flows, stdout and database bytes identical
```

```text
IO matrix 896 same 896 different 0
```

```text
Connection IO 160 same 160 different 0
```

Hazırlama, sonraki hazırlama, step/reset, main/TEMP/aux, geçerli/geçersiz/taşan değerler, işlem içi bayraklar ve bütün .db dosyaları iki anahtar durumunda karşılaştırıldı. Ayrıca 8 sayfa boyutunda iki açılışlı cache çerezi denemeleri yapıldı. G/Ç gözlemcileri gerçek işletim sistemi işlemlerini devreder; xSync sırası/bayrakları ve xDelete dizin eşitleme biti ile son .db baytları karşılaştırılır. 160 ek G/Ç senaryosu hazırlanan WAL eşiğinin commit üzerindeki etkisini, sonradan değiştirilen eşikleri, ATTACH ve işlem içi cache_spill ayarlarını kapsar.

Altı test tests/v6b_pragma_connection_prepare.rs içinde. Özel busy callback için native C kanıtı:

```text
invalid prepare 0 callbacks 0
invalid row 100 value 0 callbacks 0
locked select prepare 5 callbacks 1
cleared select prepare 5 callbacks 1
```

İlk şema okumasının bekleyen ifadeyi yeniden hazırlama kanıtı (çalışan sorgu 483, bekleyen sorgu yeniden hazırlanıp 17; eski timeout setter yeniden 17 uygular):

```text
running spill 100 483
pending expired before 0
pending expired after 2
pending spill 100 17 reprepare 1
setter busy 100 17 reprepare 1
```

İlk adayda cold_cache.json farkı bu sayede bulundu; eski adayın kanıtları res/before_encoding_expire/ altında korundu. Nihai kaynak üzerinde DML/ayrışma/program ve G/Ç kontrolleri tekrarlandı.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 571 / 1006 (56.8%) | 415 / 722 (57.5%) |
| btree.c | 198 / 229 (86.5%) | 91 / 126 (72.2%) |
| pager.c | 60 / 63 (95.2%) | 38 / 46 (82.6%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 21 / 21 (100.0%) | 4 / 4 (100.0%) |
| vdbeaux.c | 34 / 45 (75.6%) | 13 / 18 (72.2%) |
| callback.c | 5 / 5 (100.0%) | 0 / 0 (0.0%) |
| prepare.c | 78 / 92 (84.8%) | 38 / 58 (65.5%) |
| **total** | 981 / 1475 (66.5%) | 609 / 984 (61.9%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 54.0 | 675 | 54.5 | 141 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 100.0 | 18 | 100.0 | 0 |
| sqlite3PagerSetPagesize | pager.c | 31 | 90.3 | 26 | 69.2 | 8 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| getAutoVacuum | pragma.c | 6 | 100.0 | 10 | 100.0 | 0 |
| sqlite3BtreeSetAutoVacuum | btree.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3BtreeGetAutoVacuum | btree.c | 7 | 100.0 | 4 | 100.0 | 0 |
| btreeEndTransaction | btree.c | 15 | 86.7 | 8 | 75.0 | 2 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 71.4 | 4 |
| sqlite3BtreeSavepoint | btree.c | 16 | 87.5 | 14 | 42.9 | 5 |
| lockBtree | btree.c | 73 | 67.1 | 44 | 61.4 | 16 |
| newDatabase | btree.c | 24 | 100.0 | 4 | 75.0 | 1 |
| setAllPagerFlags | pragma.c | 10 | 100.0 | 6 | 100.0 | 0 |
| sqlite3BtreeSetSpillSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PagerSetSpillsize | pager.c | 2 | 100.0 | 0 | 100.0 | 0 |
| numberOfCachePages | pcache.c | 6 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PcacheSetSpillsize | pcache.c | 8 | 100.0 | 6 | 100.0 | 0 |
| sqlite3_busy_timeout | main.c | 8 | 100.0 | 2 | 100.0 | 0 |
| sqlite3_busy_handler | main.c | 8 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_wal_autocheckpoint | main.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeList | vdbeaux.c | 41 | 73.2 | 16 | 68.8 | 5 |
| sqlite3SetTextEncoding | callback.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3ExpirePreparedStatements | vdbeaux.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3InitOne | prepare.c | 92 | 84.8 | 58 | 65.5 | 13 |

Bu toplam taşınmayan pragma işleyicilerini ve genel Btree/şema yollarını da içerir; tamamlanma yüzdesi değildir. Her alınmayan dal ayrı [karar_esligi_v6b_7_dallar.md](karar_esligi_v6b_7_dallar.md) içinde sınıflandırıldı. Özel WAL hook değiştirme API’si, timeout beklemesinin tüm zamanlama/VFS hata yolları, authorizer, özel VFS PRAGMA override ve tahsis/GÇ hata enjeksiyonu bu kanıtla kapanmaz. E28 ve diğer iş listesi açık.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1063, 0, 337), açık (1048, 15, 337). Açık 15 hata adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu sonuçları res/finder_last.txt içinde; önceki adıma göre değerlendirme aşağıda. Beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
scenarios differing from the reference: 1
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.29      8.84     0.951x     9.26     8.80     9.60    0.92x  OK
W02_create_indexes                5.74      5.77     1.004x     5.68     5.70     8.76    0.66x  OK
W03_pk_point_select              30.08     29.94     0.996x    29.43    29.56    23.30    1.28x  OK
W04_secondary_idx_range           9.51      9.70     1.020x     9.44     9.57     7.46    1.30x  OK
W05_full_scan_filter_agg          4.41      4.36     0.989x     4.40     4.35    16.57    0.26x  OK
W06_join_group_order             10.85     11.13     1.026x    10.81    11.13    28.23    0.39x  OK
W07_window_functions             37.36     37.43     1.002x    37.32    37.23    50.21    0.75x  OK
W08_indexed_update_delete        12.11     11.93     0.985x    12.05    11.80     7.96    1.50x  OK
W09_like_text_scan               12.56     12.31     0.980x    12.42    12.29    18.92    0.65x  OK
W10_jsonb_extract_agg            13.69     13.40     0.979x    13.65    13.37    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.20     70.23     1.015x    69.01    69.19    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.29     40.19     0.998x    39.79    39.32    24.63    1.63x  OK
W13_autocommit_wal_txns           4.56      4.70     1.032x     4.37     4.58     4.12    1.14x  OK
W14_cli_sql_script               39.27     39.44     1.004x    38.84    39.23    26.19    1.51x  OK
TOTAL                           298.92    299.38     1.002x   geomean cand/base = 0.998x   digest mismatches = 0
```

## Yerel kanıt

Kök /home/hakan/github/sqliters/target/perf-work/v6b7/. connection.sql, connection*.ops ve cold_cache.py girdileri; yerel program/API/GÇ sürücüleri bin/ altında, ham sonuçlar res/ içinde. res/source_manifest.json test edilen kaynak SHA256 değerlerini tutar. C 3.54.1 PATH başında cargo test --workspace --no-fail-fast iki anahtar durumunda çalıştırıldı. C kaynak ağacı değiştirilmedi; gcov ayrı dizine yönlendirildi. Anahtar açık son kaynak üzerinde çalıştı; kapalı ilk aday koşusu saklanıp son kaynak üzerinde tekrarlandı.

Sıradaki uygun dilim kalan PRAGMA işleyicilerinin taşınmasıdır. V6b, M4 ve diğer C eşliği eksikleri giderilmeden performans ayarı yapılmayacak.
