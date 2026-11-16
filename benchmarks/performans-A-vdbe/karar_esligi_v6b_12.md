# V6b.12 — şema başlatma sırası ve ilk hata

C başvurusu SQLite 3.54.1; Rust 1.100. D113 düzeltildi: fiziksel şemalar C sqlite3Init sırasıyla main, sondan başa bağlı veritabanları, TEMP olarak okunur. Açık işlemdeki önbellek yenilemesi de aynı sıradadır. İsim çözümleme önceliği korunur; hız ayarı yapılmadı.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `prepare.c:468–470` | Main yüklenmemiş | Main ilk yüklenir; ilk hata hemen döner. | `crates/sqliters-core/src/lib.rs:1877` |
| `prepare.c:473–477` | Birden fazla bağlı veritabanı | Diğer yuvalar sondan başa yüklenir; en yeni bağlı veritabanı eski bağlı veritabanından önce hata verir. | `crates/sqliters-core/src/lib.rs:1875` |
| `prepare.c:472–478` | TEMP ve bağlı veritabanı birlikte bozuk | TEMP son okunur. aux/b bozukluğu TEMP bozukluğundan önce bildirilir. | `crates/sqliters-core/src/lib.rs:1872` |
| `prepare.c:468–477; 277–288` | Btree işlemi daha önce açılmış | Açık işlemde önbellek yenilemesi de aynı sırayı kullanır; BEGIN, BEGIN IMMEDIATE ve SAVEPOINT için ilk hata eşleşir. | `crates/sqliters-core/src/lib.rs:1958` |
| `prepare.c:470,477` | Her sırada ilk hata | Fiziksel şema yükleme hatası Result ile hemen döner; sonraki şema önceki hatayı değiştirmez. Bozuk SQL metinleriyle hata önceliği sınandı; genel GÇ/NOMEM eşliği bu adımla kanıtlanmaz. | `crates/sqliters-core/src/lib.rs:1910` |
| `prepare.c:468,475` | Şema zaten yüklü veya TEMP açılmamış | Mevcut atlama koşulları korunur; değişen yükleme sırasıdır. Üreticinin build_tab.rs sqlite3_init yordamı bu sırayı zaten kullanıyordu. | `crates/sqliters-codegen/src/build_tab.rs:2061` |
| `build.c:375–387` | İsim çözümleme sırası | Yükleme sırasından bağımsız TEMP, main, ilk bağlanan veritabanı önceliği korunur. Testte u sırasıyla 11,33,44 verir. | `tests/v6b_schema_init_order.rs:73` |

## Karşılaştırmalar

İki bağlanma sırası × 16 bozuk şema birleşimi × iki sorgu biçimi (SELECT ve table_info) × dört işlem durumu (autocommit, BEGIN, BEGIN IMMEDIATE, SAVEPOINT): 256 API girdisi ve geçerli isim çözümleme girdisi. Her girdi anahtar kapalı/açık karşılaştırıldı; şema hata önceliği C ile aynı. Beş hedefli test geçti.

Toplam API akışı 2236: 2235 bütün çıktı aynı; tek fark önceden açık D103. Bütün 2236 akışta main ve bağlı .db dosyalarının bütün baytları C ile aynı. EXPLAIN ve EQP ayrı ayrı 13621/13621, hazırlama/adımlama akışı 9470/9470 aynı.

Tam program karşılaştırması 11.240 aynı, 0 farklı, 148 desteklenmeyen. Desteklenmeyenler önceki dilimle aynı: 135 PRAGMA işleyicisi, 11 defer_foreign_keys yaşam döngüsü, bir optimize ve bir table_list geçmişi. Bu kayıtlar başarı sayılmadı.

896 synchronous, 160 bağlantı/cache GÇ, 16 kalıcı cache yeniden açma ve üç mevcut kodlama dosyasının yeniden açılması eşleşti. Mevcut UTF-8/UTF-16le/UTF-16be dosyalarında toplam 12 tam program da ilk kullanım girdileriyle aynı; dosyalar değişmedi.

## Doğrulama sırasında bulunan ikinci yol ve düzenek hatası

İlk aday yalnız yeni fiziksel yükleyiciyi düzeltmişti. Bağımsız BEGIN / BEGIN IMMEDIATE / SAVEPOINT yoklaması açık işlemdeki yenilemenin hâlâ bad_temp hatasını önce bildirdiğini gösterdi; C bad_b bildiriyordu. İkinci yordam da düzeltildi, yeni hedefli test ve 192 işlem içi girdi eklendi. Dört kapı son kaynak üzerinde yeniden çalıştırıldı. İlk adayın sonuçları res/before_open_tx_fix altında korundu.

İlk tam program denemesinde 60 fark test düzeneğinden geldi: C/Rust ayrı ana dosya kullanırken bağlı a.db/b.db dosyaları ortak çalışma dizininde kalıyordu. Her taraf ve her girdi ayrı dizine alındı; 60 fark kayboldu. Ortak dizinli eski çıktılar res/before_open_tx_fix/shared_cwd_* altında saklandı. Bu kayıtlar ürün kusuru veya düzeltmesi olarak sayılmadı. API ve CLI karşılaştırıcıları zaten ayrı dizinler kullanıyordu.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 860 / 1142 (75.3%) | 569 / 794 (71.7%) |
| btree.c | 199 / 229 (86.9%) | 95 / 126 (75.4%) |
| pager.c | 154 / 172 (89.5%) | 102 / 152 (67.1%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 21 / 21 (100.0%) | 4 / 4 (100.0%) |
| vdbeaux.c | 38 / 45 (84.4%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 132 / 173 (76.3%) | 81 / 116 (69.8%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| **total** | 1620 / 2058 (78.7%) | 960 / 1360 (70.6%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 71.6 | 675 | 68.9 | 120 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 100.0 | 18 | 100.0 | 0 |
| sqlite3PagerSetPagesize | pager.c | 31 | 93.5 | 26 | 76.9 | 6 |
| sqlite3BtreeSetPageSize | btree.c | 23 | 100.0 | 22 | 100.0 | 0 |
| sqlite3BtreeGetPageSize | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| getAutoVacuum | pragma.c | 6 | 100.0 | 10 | 100.0 | 0 |
| sqlite3BtreeSetAutoVacuum | btree.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3BtreeGetAutoVacuum | btree.c | 7 | 100.0 | 4 | 100.0 | 0 |
| btreeEndTransaction | btree.c | 15 | 86.7 | 8 | 75.0 | 2 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 78.6 | 3 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 64.3 | 3 |
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
| sqlite3VdbeList | vdbeaux.c | 41 | 82.9 | 16 | 75.0 | 4 |
| sqlite3SetTextEncoding | callback.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3ExpirePreparedStatements | vdbeaux.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 77.6 | 10 |
| getLockingMode | pragma.c | 5 | 100.0 | 6 | 100.0 | 0 |
| returnSingleText | pragma.c | 5 | 100.0 | 2 | 50.0 | 1 |
| sqlite3PagerLockingMode | pager.c | 4 | 100.0 | 6 | 83.3 | 1 |
| pragmaVtabConnect | pragma.c | 34 | 82.3 | 12 | 58.3 | 5 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 78.6 | 3 |
| pragmaVtabFilter | pragma.c | 26 | 84.6 | 14 | 71.4 | 4 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 50.0 | 2 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 83.3 | 3 |
| pagerPlaybackSavepoint | pager.c | 37 | 64.9 | 44 | 36.4 | 15 |
| sqlite3PagerRollback | pager.c | 18 | 83.3 | 16 | 62.5 | 5 |
| sqlite3RunParser | tokenize.c | 75 | 77.3 | 54 | 68.5 | 10 |
| sqlite3Prepare | prepare.c | 68 | 58.8 | 46 | 52.2 | 13 |
| sqlite3Error | util.c | 6 | 100.0 | 4 | 75.0 | 1 |
| sqlite3ErrorClear | util.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3ErrorMsg | util.c | 18 | 77.8 | 6 | 50.0 | 2 |
| getTempStore | pragma.c | 8 | 100.0 | 8 | 100.0 | 0 |
| invalidateTempStorage | pragma.c | 11 | 100.0 | 6 | 100.0 | 0 |
| changeTempStorage | pragma.c | 8 | 100.0 | 4 | 100.0 | 0 |
| sqlite3ResetAllSchemasOfConnection | build.c | 14 | 92.9 | 8 | 75.0 | 2 |
| sqlite3SchemaClear | callback.c | 21 | 95.2 | 6 | 83.3 | 1 |
| sqlite3VtabCallConnect | vtab.c | 18 | 22.2 | 6 | 16.7 | 3 |
| vtabCallConstructor | vtab.c | 70 | 75.7 | 42 | 52.4 | 13 |
| sqlite3VtabEponymousTableInit | vtab.c | 30 | 80.0 | 12 | 50.0 | 4 |
| sqlite3Init | prepare.c | 13 | 100.0 | 12 | 100.0 | 0 |

sqlite3Init içindeki 12 gcov dalının tamamı alındı; bu tek işlevin kapsamı genel C eşliğini kanıtlamaz. İzlenen bütün işlevlerdeki 400 alınmayan dal [karar_esligi_v6b_12_dallar.md](karar_esligi_v6b_12_dallar.md) içinde tek tek sınıflandırıldı. Tahsis, genel GÇ, constructor ve diğer açık işleyici yolları tamamlandı sayılmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1092, 0, 337), açık (1077, 15, 337). Açık 15 başarısızın adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; başarısız seed ve sınıf kümeleri öncekiyle aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı yedi A/B denemesi; anahtar kapalı. Bu ölçüm doğruluk değişikliğinin etkisini izler; performans ayarı yapılmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.24      8.79     0.951x     9.18     8.76     9.60    0.92x  OK
W02_create_indexes                5.70      5.71     1.001x     5.63     5.68     8.76    0.65x  OK
W03_pk_point_select              29.88     29.90     1.001x    29.50    29.58    23.30    1.28x  OK
W04_secondary_idx_range           9.63      9.54     0.991x     9.33     9.38     7.46    1.28x  OK
W05_full_scan_filter_agg          4.40      4.27     0.971x     4.39     4.27    16.57    0.26x  OK
W06_join_group_order             10.80     10.80     1.000x    10.79    10.77    28.23    0.38x  OK
W07_window_functions             37.17     37.43     1.007x    37.07    37.28    50.21    0.75x  OK
W08_indexed_update_delete        12.05     11.74     0.975x    11.97    11.64     7.96    1.48x  OK
W09_like_text_scan               12.61     12.47     0.989x    12.41    12.44    18.92    0.66x  OK
W10_jsonb_extract_agg            13.70     13.40     0.978x    13.64    13.34    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.52     70.06     1.008x    68.57    69.22    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.44     40.24     0.995x    39.45    39.25    24.63    1.63x  OK
W13_autocommit_wal_txns           4.59      4.68     1.019x     4.44     4.60     4.12    1.14x  OK
W14_cli_sql_script               38.88     39.11     1.006x    38.61    38.81    26.19    1.49x  OK
TOTAL                           298.63    298.14     0.998x   geomean cand/base = 0.992x   digest mismatches = 0
```

## Açık sınırlar ve sonraki adım

D112 AÇIK: şema daha önce yüklenmişken writable_schema=RESET ardından FROM’suz SELECT 1 C’de hazırlanır, Rust gereksiz fiziksel şema okumasıyla hata verir. Sıranın düzelmesi bu kusuru kapatmaz. D103 eski yürütücü TVF eşitlik denetimi de açıktır. Yeni D114: başka bağlantı EXCLUSIVE kilit tutarken önceden şeması yüklenmiş bağlantıda kullanılmayan CTE veya sabit false AND içindeki tablo alt sorgusu C’de kilit istemez; Rust iki anahtar durumunda da adımlamada gereksiz okuma kiralamasıyla BUSY verir. Yerel ham iki bağlantı çıktıları ../v6b13/res/schema_locks_* altında; bu yeni yoklama başarı sayılmadı. Mevcut test/DML/ayrışma farkları, E28 ve genel UTF-16/geçersiz UTF-8 iş listesi sürer. V6b veya tam C işlevselliği tamamlandı sayılmaz.

Sonraki adım D112 ve D114: şema gereksinimi üreticinin C koşullarından çıkarılacak. İlk C yoklamaları parse.y:523 EncodingFixed korumasını doğruladı: başka bağlantı EXCLUSIVE kilit tutarken soğuk bağlantıdaki SELECT 1 şema yüklemesi nedeniyle BUSY, önceden şeması yüklenmiş bağlantıda başarılı. Ayrıca insert.c:711–712 çok satırlı VALUES için SchemaKnownOk kararını gerektirir. Bütün sabit SELECT’lerde şema okumayı kapatan bir yaklaşım kullanılmayacak.

Yerel kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b12/`: SQL/API girdileri, tam programlar, satır/hata çıktıları, bütün dosya SHA256 değerleri, gcov ve kapı sonuçları. res/source_manifest.json iki kaynak/test dosyasını sabitler. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
