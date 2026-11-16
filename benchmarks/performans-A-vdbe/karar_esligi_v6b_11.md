# V6b.11 — üretici doğrulama API’sinin hazırlama durumu

C başvurusu SQLite 3.54.1; Rust 1.100. Gizli c_generated_program API’sinin şema/EncodingFixed ve ilk sanal tablo bağlama durumları gerçek hazırlamayla hizalandı. Üst düzey PRAGMA hazırlama etkileri önizlemede uygulanmaz. Bu adım yeni PRAGMA işleyicisi veya hız ayarı eklemez.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `prepare.c:104; pragma.c:2278–2295` | Şema nesnesi daha önce oluşturuldu; sonraki literal INSERT hazırlamayı atladı | Önizleme gerçek prepare gibi bilinen EncodingFixed durumunu günceller; C’nin kabul ettiği atama artık yanlış encoding hatası vermez. | `crates/sqliters-core/src/lib.rs:3517` |
| `prepare.c:492–506; build.c:210–220` | Şema isteyen ifade; fiziksel önbellek temizlenmiş | Gerçek şema, çerez ve nesil program üretilmeden yüklenir. TEMP kapanışından sonraki OP_Transaction P3 sıfır yerine gerçek çerez 1 olur. | `crates/sqliters-core/src/lib.rs:3526` |
| `prepare.c:321–342; 459–478` | İlk şema yüklemesi; metin kodlaması/istatistikler | Yükleme mevcut hazırlama yordamından geçer; bilinen şema/kodlama ve istatistik durumu üreticiye aktarılır. Genel hata yolları ayrıca kanıtlanmış sayılmaz. | `crates/sqliters-core/src/lib.rs:3527` |
| `vtab.c:1261–1295; pragma.c:3099–3107` | Eponymous pragma tablosu ilk kez kullanılıyor / zaten bağlı | SELECT önizlemesi sanal tablo bağlantısını hazırlar. Ctor tamamlandıktan sonra görünür/gizli sütun ve plan bilgileri C üreticisine verilir. | `crates/sqliters-core/src/lib.rs:3532` |
| `vtab.c:699–732; 559–687` | Genel sanal tablo bağlantısı ve constructor | Mevcut bağlama yolu kullanılır. Kullanıcı modülü hatası, tekrar giriş, tahsis ve bazı HIDDEN sözcük varyantları bu dilimde tamamlandı sayılmaz. | `crates/sqliters-core/src/lib.rs:3537` |
| `pragma.c:883–890; 2284–2287` | Doğrulama önizlemesi / gerçek hazırlama | Önizleme üst düzey PRAGMA etkilerini atar; sonraki gerçek prepare etkileri tek kez uygular. TEMP kapanmaz, encoding değişmez ve busy_timeout setter gereksiz expire olmaz. | `crates/sqliters-core/src/lib.rs:3542` |
| `select.c:6127–6162` | FROM’suz SELECT 1; bozuk şemalar | D112 AÇIK: C burada tablo çözümlemesi/şema okuması istemez; Rust’ın genel SELECT gereksinimi hâlâ fazladan şema okuyor. | `crates/sqliters-core/src/engine.rs:1620` |
| `prepare.c:459–478` | Main, bağlı veritabanları ve TEMP yükleme sırası | D113 AÇIK: C main, sondan başa bağlı veritabanları, TEMP sırasını izler. Fiziksel Rust yükleyicisi henüz 0..slot_cnt sırasındadır; hata önceliği farklıdır. | `crates/sqliters-core/src/lib.rs:1875` |

## Tam program ve diğer kanıtlar

Aynı ilk-kullanım girdilerinde 11172 tam program C ile aynı, program farkı 0. Kalan 148 kayıt: unsupported: PRAGMA handler (V6b): 135; unsupported: PRAGMA defer_foreign_keys transaction lifetime (V6b): 11; unsupported: PRAGMA optimize statistics usage history (V6b): 1; unsupported: PRAGMA table_list hidden view initialization history (V6b): 1. Yanlış encoding hatası veya bağlanmamış sanal tablo kaydı bu derlemde kalmadı.

Önceki S10 aynı derleminde 10.012 program eşleşmişti; 33 yanlış hata ve 1.127 bağlanmamış tablo kaydı vardı. İlk S11 adayında bunlar açılınca 60 çerez farkı çıktı: C OP_Transaction P3=1, önizleme P3=0. Fiziksel şema yüklemesiyle bu 60 fark da kapandı. Önceki aday sonuçları res/before_schema_init/ altında korundu. Bu kez karşılaştırma için ilave P63 EXPLAIN ısınması kullanılmadı.

EXPLAIN/EQP 13493 aynı, hazırlama akışı 9342/9342 aynı. API 1721/1722 bütün çıktı aynı; kalan D103 önceki eski yürütücü kusurudur. Bütün 1722 API akışının bütün .db dosyaları bayt bayt aynı. 16 kalıcı cache yeniden açma, 896 synchronous ve 160 bağlantı/cache GÇ senaryosu aynı.

Mevcut UTF-8, UTF-16le ve UTF-16be dosyaları ilk kez açıldığında SELECT/encoding/pragma_encoding/table_info için toplam 12 tam program da eşleşti; ek bağlama hazırlığı yok. Mevcut dosya yeniden açma satır/hata çıktıları da aynı ve dosyalar değişmedi. Dört hedefli test; hızlı literal INSERT sonrası sabit encoding, yeni pragma sanal tabloları, temizlenmiş şema çerezi ve önizlemenin hazırlama etkilerini uygulamamasını doğrular.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 860 / 1142 (75.3%) | 568 / 794 (71.5%) |
| btree.c | 199 / 229 (86.9%) | 95 / 126 (75.4%) |
| pager.c | 154 / 172 (89.5%) | 102 / 152 (67.1%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 21 / 21 (100.0%) | 4 / 4 (100.0%) |
| vdbeaux.c | 38 / 45 (84.4%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 132 / 173 (76.3%) | 75 / 116 (64.7%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| **total** | 1620 / 2058 (78.7%) | 953 / 1360 (70.1%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 71.6 | 675 | 68.7 | 121 |
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
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 69.0 | 13 |
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
| sqlite3Init | prepare.c | 13 | 100.0 | 12 | 91.7 | 1 |

Her alınmayan dal [karar_esligi_v6b_11_dallar.md](karar_esligi_v6b_11_dallar.md) içinde ayrı sınıflandırıldı. Constructor bellek hatası, tekrar giriş, schema bildirmeme, özel hata metni, HIDDEN tip sözcüğünün bazı sınır varyantları ve genel modül hata yolları açık. Eponymous pragma yolu ile genel sqlite3VtabCallConnect yolu ayrı gösterilir; birinin kapsanması diğerinin soğuk bağlama yolunu kanıtlamaz. Kapsam toplamı bir tamamlanma yüzdesi değildir.

## Ek bağımsız yoklamalar: D112 ve D113 açık

Girdi, aux.u ve TEMP q tablolarını oluşturup writable_schema ile iki CREATE metnini sırasıyla broken aux / broken temp yapar; writable_schema=RESET yalnız hazırlanır. C SELECT 1 hazırlamasını kabul eder, hem S10 hem S11 Rust malformed database schema (q) hatası verir (D112). SELECT * FROM aux.u hazırlamasında C malformed database schema (u), iki Rust adayı (q) verir (D113). Bu yeni farklar eşleşme sayılmadı ve sonraki adıma önceliklendirildi.

Girdiler ve ham C/S10/S11 sonuçları `/home/hakan/github/sqliters/target/perf-work/v6b12/schema_order*.ops` ve `res/schema_order*_before.json` içindedir. Bozuk şema, FROM’suz SELECT ve şema yükleme sırasının genel C eşliği bu adımla kapanmış sayılmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1087, 0, 337), açık (1072, 15, 337); açık 15 başarısızın adları öncekiyle aynı. Beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi, anahtar kapalı. Bütün sonuç özetleri aynı; hız ayarı yok.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.27      8.74     0.943x     9.17     8.72     9.60    0.91x  OK
W02_create_indexes                5.68      5.69     1.001x     5.63     5.67     8.76    0.65x  OK
W03_pk_point_select              29.75     30.32     1.019x    29.41    29.65    23.30    1.30x  OK
W04_secondary_idx_range           9.43      9.75     1.035x     9.40     9.52     7.46    1.31x  OK
W05_full_scan_filter_agg          4.42      4.37     0.989x     4.39     4.35    16.57    0.26x  OK
W06_join_group_order             10.79     10.86     1.006x    10.77    10.83    28.23    0.38x  OK
W07_window_functions             37.21     37.22     1.000x    37.03    37.11    50.21    0.74x  OK
W08_indexed_update_delete        11.99     11.67     0.974x    11.95    11.60     7.96    1.47x  OK
W09_like_text_scan               12.45     12.36     0.993x    12.42    12.31    18.92    0.65x  OK
W10_jsonb_extract_agg            13.70     13.45     0.982x    13.65    13.36    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.04     70.24     1.017x    68.67    69.12    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.31     40.45     1.003x    40.00    39.48    24.63    1.64x  OK
W13_autocommit_wal_txns           4.64      4.76     1.026x     4.39     4.45     4.12    1.16x  OK
W14_cli_sql_script               39.14     39.35     1.006x    38.61    39.03    26.19    1.50x  OK
TOTAL                           297.81    299.23     1.005x   geomean cand/base = 0.999x   digest mismatches = 0
```

## Yerel kanıt ve sıradaki adım

Kök `/home/hakan/github/sqliters/target/perf-work/v6b11/`: önceki regresyon SQL/API girdileri; res altında tam programlar, satır/hata, bütün .db SHA256, kapsam ve kapı çıktıları; cov altında ayrı C kopyası. res/source_manifest.json son doğrulanan iki kaynak/test dosyasını sabitler. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.

D111 ve bu derlemdeki ilk sanal tablo bağlama sınırı kapandı. D103, D112, D113, mevcut test/DML/ayrışma kusurları, E28 ve genel UTF-16/geçersiz UTF-8 iş listesi açık. Sonraki uygun adım D112/D113 şema okuma kararlarıdır; ardından kalan PRAGMA işleyicileri ve M4. V6b tamamlanmış veya tam C garantisi verilmiş sayılmaz.
