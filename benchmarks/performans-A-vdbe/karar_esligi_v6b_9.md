# V6b.9 — temp_store hazırlama ve TEMP geçersizleştirme

Başvuru C SQLite 3.54.1; Rust 1.100. temp_store üretimi ve hazırlama etkisi taşındı. Kip ayrıştırma, açık TEMP işlem koruması, TEMP kapatma, şema temizleme, cache_size koruma ve donmuş sonuç davranışı aynı C girdileriyle karşılaştırıldı. Performans ayarı yapılmadı.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:141–143` | İlk karakter 0,1,2 / diğer | Sayı bütünü ayrıştırılmaz; 20 ve 2junk MEMORY, 10 FILE, 0x2 DEFAULT olur. | `crates/sqliters-codegen/src/pragma.rs:196` |
| `pragma.c:144–150` | Tam FILE / MEMORY / diğer sözcük | Büyük-küçük harf duyarsız iki tam sözcük kabul edilir; boşluk kırpılmaz; diğer girdi DEFAULT olur. | `crates/sqliters-codegen/src/pragma.rs:199` |
| `pragma.c:185–186` | İstenen kip mevcut kip ile aynı | TEMP açık ve işlem içinde olsa da değişiklik, kapatma ve hata yoktur. | `crates/sqliters-codegen/src/pragma.rs:570` |
| `pragma.c:161–165` | TEMP kapalı / açık | Kapalı TEMP varken açık ana işlem ayarı engellemez. Açık TEMP varken autocommit ve TEMP Btree işlem durumu denetlenir. | `crates/sqliters-codegen/src/pragma.rs:572` |
| `pragma.c:162–168` | !autoCommit veya TEMP txn != NONE | Tam hata temporary storage cannot be changed from within a transaction; eski ayar ve TEMP içeriği korunur. | `crates/sqliters-codegen/src/pragma.rs:573` |
| `pragma.c:443–457; 992–997` | Şema adı TEMP / diğer | Önce normal şema çözme ve TEMP açma kuralı uygulanır; ayarın kendisi bağlantı genelindedir. TEMP açılmışsa işlem koruması artık geçerlidir. | `crates/sqliters-core/src/lib.rs:5754` |
| `pragma.c:170–172` | Açık TEMP ve izin verilen değişiklik | TEMP Btree kapanır; TEMP nesneleri ve bağları kaldırılır; diğer şemalar yeniden yüklenmek üzere temizlenir. | `crates/sqliters-core/src/db_set.rs:1406` |
| `pragma.c:183–192` | Kapatma başarılı / koruma hatası | Başarıda yeni kip yazılır; hata durumunda ayar değiştirilmez. | `crates/sqliters-core/src/lib.rs:5771` |
| `build.c:652–673; callback.c:495–529` | Yüklü / yüklenmemiş şema; temizleme | Yüklü şema nesli artar, şema yükleme bayrağı temizlenir. Genel nSchemaLock erteleme yönü ayrıca kanıtlanmadı. | `crates/sqliters-core/src/db_set.rs:971` |
| `callback.c:493; 495–529` | Schema.cache_size ve şema nesneleri | Şema nesneleri temizlenirken main/aux cache_size korunur; pager cache başlatma durumu gereksiz sıfırlanmaz. | `crates/sqliters-core/src/lib.rs:5776` |
| `build.c:652–673; vdbeapi.c:845–858; vdbe.c:4320–4337` | Sabit program / OP_Transaction şema kontrolü | Şema kullanmayan hazırlanmış sorgu TEMP kapatıldı diye yeniden hazırlanmaz; donmuş sonuç korunur. Bekleyen TEMP SELECT şema kontrolüyle yeniden hazırlanıp tablo yok hatası verir. | `crates/sqliters-core/src/lib.rs:8333` |
| `build.c:652–673` | Şema temizleme / expire işlemi | Salt şema temizleme aktif VDBE’leri expire etmez; devam eden main SELECT TEMP kapanışından sonra sonraki satırlarını verir. | `crates/sqliters-core/src/db_set.rs:971` |
| `pragma.c:992–999; 443–447` | Sorgu / atama; normal / EXPLAIN | Sorgu sabit integer/ResultRow üretir; atama sıfır sütun/satırdır. Etki hazırlamada gerçekleşir, step eski ayarı tekrar uygulamaz. | `crates/sqliters-codegen/src/pragma.rs:565` |
| `vdbeaux.c:667–669; vdbe.c:8386–8396` | İlk step / reset sonrası tekrar step | İlk step donmuş programı çalıştırır; reset sonrası yeniden hazırlama gerektiğinde kendi ayarı C gibi tekrar uygulanır. | `crates/sqliters-core/src/lib.rs:5906` |
| `pragma.c:2838–2875; 2897–2910` | Result0; gizli argüman yok | pragma_temp_store tek görünür temp_store sütunu ile kaydolur. Sonuç mevcut global kiptir. | `crates/sqliters-core/src/vtab.rs:3542` |
| `parse.y:1724–1730; tokenize.c:648–700` | Geçerli PRAGMA ardından fazla token | Geç sentaks hatasından önce yapılan TEMP kapatma ve ayar değişikliği geri alınmaz. | `crates/sqliters-core/src/lib.rs:7468` |

## Program, API ve dosya kanıtı

EXPLAIN 11832/11832 aynı (12009 toplam, 177 desteklenmeyen); EQP 11832 aynı; hazırlama/durum akışı 7692/7692 aynı. Tam program 9216 aynı, 736 Unsupported. Yeni program farkı yok.

İki anahtarda 1048 API akışı: 1047 çıktı tamamen aynı, önceki D103 tek açık çıktı farkı. Bütün 1048 akışın oluşturduğu bütün .db dosyaları bayt bayt aynı. Kalıcı cache ile 16 yeniden açma akışında bütün çıktılar ve dosyalar aynı. 896 synchronous + 160 bağlantı/cache GÇ senaryosu da aynı.

Tam program çıktısındaki 736 Unsupported kaydının sınıfları: 577 sanal tablo bağlantısı doğrulama API’sinde hazır değil; 146 kalan PRAGMA işleyicisi; 11 defer_foreign_keys; birer optimize geçmişi ve table_list görünüm geçmişi. Bunlar başarı sayılmadı. İlk bağlantıdaki sanal tablo bağlama eksiği gizli c_generated_program doğrulama API’sindedir; normal prepare/EXPLAIN yolu bu bağlantıyı yapar. İki sürücüye de aynı P63 EXPLAIN SELECT hazırlığı eklenen ayrı programs_warm.py derleminde 576 pragma_temp_store SELECT programının tamamı bütün alanlarıyla aynı. Bu, ilk bağlama öncesi program kanıtının yerine sayılmadı; ilk kullanım EXPLAIN ve API çıktıları ayrıca karşılaştırıldı.

Yeni derlem: dört TEMP/işlem durumu × altı şema biçimi × 24 RHS; her blokta setter, getter ve pragma_temp_store SELECT. API matrisinde önceden hazırlanan getter, step edilmemiş setter, EXPLAIN, aktif TEMP/main okuyucu, reset, şema değişimi, geç hata, main/aux cache değerleri ve son DML birlikte denendi. 5 hedefli Rust testi bu yaşam döngülerini korur. CLI akışı program/durum kontrolüdür; satır ve dosya karşılaştırması ayrıca API sürücüsündedir.

Kritik C sonuçları: 20 ve 2junk → 2; 10 ve 1junk → 1; 0x2 ve boşluklu file/memory → 0. TEMP kapalı BEGIN içinde değişiklik kabul edilir; TEMP açıkken BEGIN veya aktif TEMP okuyucu değişikliği tam hata metniyle reddeder. Aynı kip işlem içinde de kabul edilir. EXPLAIN setter yalnız hazırlanırken TEMP nesnelerini kaldırır. Değişiklikten önce hazırlanmış temp_store getter eski sıfırını döndürür; pending TEMP SELECT artık bulunmayan tablo hatası verir. Aktif main SELECT devam eder. main.cache_size=17 ve aux.cache_size=23 korunur.

İlk adayda 72 akışta donmuş getter değişmişti: has_detached_slot kontrolü şema kullanmayan sabit ifadeyi de yeniden hazırlıyordu. Şema gereksinimi kontrolü öne alındı; aynı matris yeniden çalıştırıldı. Bu fark giderildi; D103 bu düzeltmenin kapsamına alınmadı.

## C kapsamı

getTempStore (8/8 dal), invalidateTempStorage (6/6) ve changeTempStorage (4/4) bütün yönleriyle örneklendi. Genel şema temizleme yardımcılarında alınmayan yönler aşağıda ve ayrı dal dosyasında açık bırakıldı.

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 846 / 1142 (74.1%) | 553 / 794 (69.6%) |
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
| **total** | 1512 / 1927 (78.5%) | 898 / 1288 (69.7%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 70.1 | 675 | 66.5 | 128 |
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

Alınmayan her dal [karar_esligi_v6b_9_dallar.md](karar_esligi_v6b_9_dallar.md) içinde ayrı sınıflandırıldı. Toplam genel parser/pager/şema yollarını ve taşınmayan işleyicileri de içerir; eşlik tamamlanma yüzdesi değildir. NOMEM/GÇ enjeksiyonu, authorizer, özel VFS PRAGMA override, nSchemaLock ile ertelenmiş genel şema temizleme ve bütün UTF-16/geçersiz UTF-8 yolları bu kanıtla kapanmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1076, 0, 337), açık (1061, 15, 337). Açık 15 başarısız testin adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri öncekiyle aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi, anahtar kapalı; bütün sonuç özetleri aynı. Hız ayarı yok.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.32      8.88     0.953x     9.24     8.76     9.60    0.93x  OK
W02_create_indexes                5.75      5.80     1.009x     5.68     5.76     8.76    0.66x  OK
W03_pk_point_select              29.87     30.24     1.013x    29.33    29.84    23.30    1.30x  OK
W04_secondary_idx_range           9.52      9.57     1.005x     9.34     9.42     7.46    1.28x  OK
W05_full_scan_filter_agg          4.40      4.18     0.950x     4.39     4.16    16.57    0.25x  OK
W06_join_group_order             10.83     10.66     0.985x    10.76    10.64    28.23    0.38x  OK
W07_window_functions             37.33     37.67     1.009x    37.27    37.55    50.21    0.75x  OK
W08_indexed_update_delete        12.06     11.86     0.983x    12.01    11.81     7.96    1.49x  OK
W09_like_text_scan               12.63     12.45     0.986x    12.41    12.42    18.92    0.66x  OK
W10_jsonb_extract_agg            13.74     13.48     0.981x    13.65    13.42    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.77     73.42     1.052x    68.89    69.27    57.02    1.29x  OK
W12_rtree_insert_and_bbox        40.34     42.64     1.057x    40.14    41.32    24.63    1.73x  OK
W13_autocommit_wal_txns           4.72      4.76     1.008x     4.47     4.55     4.12    1.16x  OK
W14_cli_sql_script               39.42     39.34     0.998x    38.68    39.08    26.19    1.50x  OK
TOTAL                           299.69    304.97     1.018x   geomean cand/base = 0.999x   digest mismatches = 0
```

## Yerel kanıt ve açık işler

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b9/`: temp_store.sql, temp*.ops ve önceki regresyon girdileri; bin altında C/Rust sürücüleri; res altında program, API, bütün dosyaların SHA256, GÇ ve test sonuçları; cov altında ayrı instrument edilmiş C kopyası. res/source_manifest.json son doğrulanan yedi kaynak/test dosyasının SHA256 değerlerini içerir. C kaynak ağacına dokunulmadı; ileriPlanlama değerlendirilmedi.

D103 (eski yürütücüde tüketilmeyen örtük TVF argümanı), bilinen test/DML/ayrışma farkları ve E28 açık kalır. V6b tamamlandı veya tam C garantisi verildi sayılmaz. Sonraki uygun doğruluk dilimi encoding hazırlama ve sabit metin/şema yükleme kurallarıdır; ardından diğer PRAGMA işleyicileri ve M4.
