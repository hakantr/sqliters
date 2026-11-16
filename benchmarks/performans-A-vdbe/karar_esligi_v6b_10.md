# V6b.10 — encoding hazırlama, şema yükleme ve advisory expire

Başvuru C SQLite 3.54.1; araç zinciri Rust 1.100. encoding üretimi, hazırlama etkileri, şema okuması ve bekleyen ifadelerin yeniden hazırlanması taşındı. Bu dilim genel UTF-16 ve geçersiz UTF-8 iş listesinin bütünü değildir. Hız ayarı yapılmadı.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:2255–2264` | Sekiz tam kodlama adı / diğer sözcük | UTF8 ve UTF-8 → 1; iki LE ve iki BE adı → 2/3; UTF16 ve UTF-16 → yerel endian. Harf boyutu duyarsız; boşluk kırpılmaz. | `crates/sqliters-codegen/src/pragma.rs:423` |
| `pragma.c:2278–2295` | DBFLAG_EncodingFixed açık / kapalı | Sabitse atama ve geçersiz ad sessizce yok sayılır. Kapalıysa geçerli ad ayarı değiştirir, geçersiz ad tam unsupported encoding iletisidir. | `crates/sqliters-codegen/src/pragma.rs:421` |
| `pragma.c:2280–2286` | pEnc.enc sıfır / 1,2,3 | UTF16 yerel kodlamayı seçer; diğer adlar belirli kodlamayı seçer. C tablo sırası ve kabul edilen ad kümesi korunur. | `crates/sqliters-codegen/src/pragma.rs:426` |
| `pragma.c:2284–2287; callback.c:181–188` | Her geçerli atama; önceki değer aynı / farklı | SCHEMA_ENC ve bağlantı kodlaması hazırlamada değişir; aynı değerde bile bekleyen ifadeler advisory expire olur. | `crates/sqliters-core/src/lib.rs:5783` |
| `pragma.c:2278–2295` | Autocommit açık / kapalı | Atama için autocommit koruması yoktur. BEGIN içinde de geçerli atama uygulanır, geçersiz ad hata verir. | `crates/sqliters-codegen/src/pragma.rs:417` |
| `pragma.c:2250–2297; 443–447` | Normal hazırlama / EXPLAIN | EXPLAIN hazırlaması da atamayı ve advisory expire etkisini uygular; step eski programı gereksiz yeniden uygulamaz. | `crates/sqliters-core/src/lib.rs:5888` |
| `pragma.c:2268–2274` | Sorgu; sqlite3ReadSchema başarılı / hata | Sorgu hazırlamada şemayı yükler; hata varsa sonuç programı üretilmez. Başarıda güncel enc adı String8/ResultRow olur. | `crates/sqliters-codegen/src/pragma.rs:437` |
| `prepare.c:320–342; callback.c:181–188` | İlk main şema yüklemesi, kayıtlı encoding ve !EncodingFixed | Dosyadaki kodlama bağlantıya geri yüklenir; bekleyen ifadeler aynı UTF-8 değeri için de advisory expire olur. Boş pagerlerin bekleyen kodlaması bu değerle eşitlenir. | `crates/sqliters-core/src/lib.rs:1717` |
| `pragma.c:2274–2277; prepare.c:320–342` | Var olan dosya, ilk şema okumasından önce atama | Atama bağlantıyı geçici değiştirir; sonraki şema okuması gerçek kayıtlı kodlamayı geri getirir. Dosyanın baytları değişmez. | `crates/sqliters-core/src/lib.rs:1710` |
| `pragma.c:2278–2295; build.c:1400–1415` | Dosyada kodlama kaydı yok / var | Boş veritabanının ilk tablosu seçilen kodlamayla yazılır; kayıtlı fiziksel kodlama üstüne atama yazılmaz. user_version ile sayfa 1 oluşmuş olması tek başına engel değildir. | `crates/sqliters-pager/src/lib.rs:2579` |
| `pragma.c:2278–2295` | Setter / getter için dosya okuma | Setter şema okumaz ve dosya kilidi istemez. Getter şemayı okuyacağı için başka bağlantının EXCLUSIVE kilidinde BUSY verebilir. | `crates/sqliters-pager/src/lib.rs:2576` |
| `callback.c:188; vdbeaux.c:5290–5297` | Bekleyen / çalışan ifade | Bekleyen getter ve setter sonraki step öncesi yeniden hazırlanır. Çalışan ifade mevcut programıyla tamamlanabilir. | `crates/sqliters-core/src/lib.rs:8344` |
| `pragma.c:2268–2274` | Hazırlama şema gereksinimi / adımlama kontrolü | encoding sorgusu şemayı hazırlarken okur; sonuç programına OP_Transaction eklenmez. Sabit metin genel PRAGMA köprüsünden döner. | `crates/sqliters-core/src/engine.rs:1553` |
| `pragma.c:2838–2875; 2897–2910` | Result0; gizli argüman yok | pragma_encoding tek encoding sütunuyla kaydolur. Normal SELECT yolu bağlantıyı kurar; gizli doğrulama API’sinin ilk bağlantı sınırı ayrıca kaydedildi. | `crates/sqliters-core/src/vtab.rs:3543` |
| `parse.y:1724–1730; tokenize.c:648–700` | Geçerli atamadan sonra fazla token | Geç sentaks hatasından önce yapılan kodlama ve advisory expire etkileri korunur. | `crates/sqliters-core/src/lib.rs:7485` |

## Program, API, dosya ve kilit kanıtı

EXPLAIN 13493/13493 aynı (13659 toplam); EQP 13493 aynı; hazırlama/durum akışı 9342/9342 aynı. Tam program 10012 aynı; 1308 kayıt dökülemedi. Bunların 33’ü D111’deki yanlış hata dönüşüdür; eşleşme sayılmadı.

Tam program dökülemeyen kayıtların sınıfları: unsupported: PRAGMA handler (V6b): 135; unsupported: PRAGMA defer_foreign_keys transaction lifetime (V6b): 11; unsupported: PRAGMA optimize statistics usage history (V6b): 1; unsupported: PRAGMA table_list hidden view initialization history (V6b): 1; unsupported: virtual table the engine cannot connect: 1127; error: unsupported encoding: unknown: 3; error: unsupported encoding: : 3; error: unsupported encoding: UTF8 : 3; error: unsupported encoding:  UTF8: 3; error: unsupported encoding: 0: 3; error: unsupported encoding: 1: 3; error: unsupported encoding: -1: 3; error: unsupported encoding: 2: 3; error: unsupported encoding: 0x2: 3; error: unsupported encoding: WITH: 3; error: unsupported encoding: CURRENT_DATE: 3. Bunlar başarı sayılmadı. İlk sanal tablo bağlamasını gizli c_generated_program API’si yapmıyor; normal prepare/EXPLAIN yolu yapıyor. C ve Rust’a aynı P63 EXPLAIN SELECT bağlama hazırlığı eklenen ayrı programs_warm.py derleminde 550 pragma_encoding SELECT programının bütün alanları aynı. İlk bağlama öncesi program kanıtı hâlâ bu doğrulama API’sinin sınırıdır; yerine başarı yazılmadı.

D111 AÇIK: c_generated_program doğrulama API’si, gerçek prepare girişindeki c_note_encoding_fixed güncellemesini yapmadığından 33 durumda C’nin kabul ettiği encoding atamasına yanlış unsupported encoding hatası döndürüyor. Normal prepare aynı girdi için bu güncellemeyi yapıyor ve API/EXPLAIN karşılaştırmalarında eşleşiyor. Bu hata gerçek program eşliği yerine Unsupported olarak kapatılmadı; ham kayıtlar program JSONL dosyasında. İlk sanal tablo bağlantısı sınırıyla birlikte sıradaki doğrulama düzeltmesidir.

İki anahtarda 1722 API akışının 1721 tanesinde bütün çıktı aynı; önceki D103 tek açık çıktı farkı. Bütün 1722 akışın oluşturduğu bütün .db dosyaları bayt bayt aynı. Mevcut UTF-8/UTF-16le/UTF-16be dosyalarını yeniden açan üç karşılaştırmada çıktı ve dosyalar aynı, dosyalar değişmedi. Kalıcı cache ile 16 yeniden açma ve 896 synchronous + 160 bağlantı/cache GÇ senaryosu aynı.

Yeni matris: beş durum × beş şema biçimi × 22 RHS; setter, getter ve pragma_encoding SELECT. API akışları bekleyen getter/setter, eski busy_timeout setter, aynı kodlama ile expiry, reset, EXPLAIN, erken/geç hata, BEGIN, sayfa 1, TEMP ve ilk tablo Unicode verisini kapsar. 7 hedefli test ve önceki 19 PRAGMA hedefli testi geçti. CLI akışı program/durum kontrolü, bütün satırlar ve bütün .db baytları ayrıca API sürücüsüyle karşılaştırmadır.

İki bağlantılı doğrudan C kilit yoklaması:

```text
PRAGMA encoding='UTF-16be' => 0 not an error
PRAGMA encoding => 5 database is locked
PRAGMA encoding => 0 not an error
 row UTF-8
```

Aynı ayrım Rust testinde de doğrulandı. Ayrıca C’nin aynı UTF8 atamasında pending busy_timeout=17 setterini yeniden hazırladığı ve ayarı 31’den 17’ye getirdiği native API yoklaması res/expire_same_C.txt içinde. Çalışan UNION ALL okuyucusu kodlama değişiminden sonra satırlarını tamamlar.

UTF-8/UTF-16le/UTF-16be içinde A, Ā, B, é, Ω, 中, emoji ve özel kullanım karakteriyle ORDER BY, indeks kurma ve integrity_check karşılaştırması hem önceki S9 ikilisi hem son S10 ikilisiyle C’ye eşitti; dosya baytları da eşitti. Bu küçük ek kontrol, genel UTF-16/bozuk UTF-8 kapsamı yerine geçmez.

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
| prepare.c | 119 / 160 (74.4%) | 64 / 104 (61.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| **total** | 1526 / 1927 (79.2%) | 913 / 1288 (70.9%) |

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

Her alınmayan dal [karar_esligi_v6b_10_dallar.md](karar_esligi_v6b_10_dallar.md) içinde ayrı sınıflandırıldı. Genel pager/şema/parser yolları ve henüz taşınmayan işleyiciler de toplamda vardır; bu bir eşlik tamamlanma yüzdesi değildir. NOMEM/GÇ enjeksiyonu, authorizer, özel VFS PRAGMA override ve bütün bozuk veritabanı/UTF-16/geçersiz UTF-8 yolları bu kanıtla kapanmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1083, 0, 337), açık (1068, 15, 337). Açık başarısızların adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri öncekiyle aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Bütün sonuç özetleri aynı; hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.36      8.93     0.955x     9.25     8.86     9.60    0.93x  OK
W02_create_indexes                5.80      5.83     1.005x     5.69     5.73     8.76    0.67x  OK
W03_pk_point_select              29.61     30.55     1.032x    29.45    29.47    23.30    1.31x  OK
W04_secondary_idx_range           9.58     10.12     1.057x     9.38     9.67     7.46    1.36x  OK
W05_full_scan_filter_agg          4.41      4.37     0.992x     4.40     4.35    16.57    0.26x  OK
W06_join_group_order             10.82     11.04     1.019x    10.80    11.02    28.23    0.39x  OK
W07_window_functions             37.46     37.62     1.004x    37.26    37.49    50.21    0.75x  OK
W08_indexed_update_delete        12.09     11.95     0.988x    12.06    11.79     7.96    1.50x  OK
W09_like_text_scan               12.64     12.57     0.995x    12.43    12.48    18.92    0.66x  OK
W10_jsonb_extract_agg            13.70     13.52     0.987x    13.69    13.49    10.93    1.24x  OK
W11_fts5_index_and_bm25          69.54     69.48     0.999x    68.76    69.02    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.16     39.75     0.990x    39.91    39.29    24.63    1.61x  OK
W13_autocommit_wal_txns           4.66      4.77     1.024x     4.46     4.52     4.12    1.16x  OK
W14_cli_sql_script               39.53     39.31     0.994x    38.89    38.98    26.19    1.50x  OK
TOTAL                           299.36    299.80     1.001x   geomean cand/base = 1.003x   digest mismatches = 0
```

## Yerel kanıt ve açık işler

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b10/`: encoding.sql, encoding*.ops, yeniden açma ve native kilit yoklamaları; bin altında sürücüler; res altında ham sonuçlar ve bütün API .db SHA256 özetleri; cov altında ayrı instrument edilmiş C kopyası. res/source_manifest.json son doğrulanan altı kaynak/test dosyasının SHA256 değerlerini tutar. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.

D103, mevcut test/DML/ayrışma farkları, genel UTF-16/geçersiz UTF-8 farkları ve E28 açık kalır. V6b tamamlandı veya tam C garantisi verildi sayılmaz. Sonraki uygun dilim D111 doğrulama API’si ve ilk sanal tablo bağlama sınırıdır; ardından kalan PRAGMA işleyicileri; önce doğruluk, ardından M4 ve performans iş listesi.
