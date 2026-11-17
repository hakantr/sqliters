# V6b.13 — SELECT şema gereksinimi ve Btree kiralamaları

C SQLite 3.54.1, Rust 1.100. D112/D114’ün sınanan UTF-8 akışlarında gereksiz şema okuması ve Btree kiralaması düzeltildi. SELECT kararı üreticinin bağlantıyı değiştirmeyen kopyasından alınır; adımlamadaki Btree gereksinimi C programının maskesinden gelir. Yeni UTF-16 sınırı D115 olarak açıktır.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `parse.y:522–528` | EncodingFixed henüz yok / daha önce sabitlendi | Soğuk SELECT 1 bile şema okuyabilir. Üretici kopyasında gerçek EncodingFixed korunur; sabitlenmiş bağlantıda FROM’suz SELECT’in gereksiz fiziksel şema yüklemesi atlanır. | `crates/sqliters-core/src/cgen.rs:466` |
| `prepare.c:490–503` | Üretici sqlite3ReadSchema çağırdı / çağırmadı | Kopyada schema_not_loaded=true kabul edilerek çağrı gözlenir. Canlı bağlantı, PRAGMA/TEMP/rowest notları ve denetim dökümleri değiştirilmez. Unsupported durumunda eski şema yükleme kararı kullanılır. | `crates/sqliters-core/src/cgen.rs:483` |
| `select.c:6127–6162` | Fiziksel tablo, alt sorgu veya CTE çözümlemesi | Şema kararı C üreticinin çözümlemesinden gelir; yalnız FROM metnine veya Rust AST tahminine dayanmaz. Gerçek hazırlama, yeniden hazırlama ve doğrulama önizlemesi aynı yoklamayı kullanır. | `crates/sqliters-core/src/lib.rs:5303` |
| `expr.c:1164–1181` | ON/işlev koruması yok ve AND tarafı sabit false | C sağdaki tablo alt sorgusunu isim çözümlemeden eler. Üretici programı Btree maskesi 0 verir; eski doğrulayıcının no such table hatasında denetlenmiş bu program makinede çalışır. | `crates/sqliters-core/src/lib.rs:5320` |
| `insert.c:675–680; 711–712` | VALUES coroutine / diğer VALUES biçimleri; SchemaKnownOk | Coroutine başlatırken eksik şema okuma çağrısı eklendi. Bozuk RESET sonrasında VALUES(1),(2) C gibi hata verir; literal CTE biçimi aynı koşullarda satır üretebilir. Bütün UNION/tahsis yönleri kanıtlandı sayılmaz. | `crates/sqliters-codegen/src/select.rs:4967` |
| `vdbeaux.c:2050–2057; vdbe.c:4257–4283` | Üretilen programın Btree maskesi 0 / sıfır değil | Maskesi 0 olan SELECT fiziksel Btree kiralamaz, şema çerezine göre yeniden hazırlanmaz. Eski plan C’nin elediği kaynağı tutuyorsa kullanılmaz. Fiziksel Btree kullanan program mevcut yolu izler. | `crates/sqliters-core/src/lib.rs:5309` |
| `vdbeapi.c:847–858; 978–1001` | İfade expire oldu / yalnız şema temizlendi | Advisory expire denetimi korunur. Bekleyen SELECT ?1 sabit sonucu korur; automatic_index atamasından sonra bağlama korunarak yeniden hazırlanır. Hazırlanmış VALUES şema temizliğinden sonra çalışır, expire sonrası yeni hazırlama C gibi bozuk şemayı okur. | `crates/sqliters-core/src/lib.rs:8453` |
| `vdbe.c:4257–4283` | Yürütmede C’nin Btree kullanımı yok | Başlangıçtaki genel şema yenilemesi ve yorumlayıcının fazladan kök kiralamaları bu SELECT’te atlanır. Kullanılmayan CTE ve false AND için iki anahtar durumunda gereksiz BUSY kalmadı. | `crates/sqliters-core/src/lib.rs:5332` |
| `vacuum.c:173–175` | Btree kullanmayan SELECT hâlâ aktif | Aktif SQL kaydı korunur. İlk satırda bekleyen sabit SELECT sırasında VACUUM C gibi reddedilir; CREATE TABLE ve SELECT’in devamı C ile aynı olur. | `tests/v6b_select_schema_use.rs:79` |

## Sınırlar ve yan etkiler

Yoklama kopyasında schema_not_loaded true ve SchemaKnownOk temiz kabul edilir; EncodingFixed ve diğer gerçek bağlantı değerleri korunur. Bu durumdaki sqlite3ReadSchema çağrısının yokluğu, RESET sonrasında da bu prepare için fiziksel şema okuması gerekmeyeceğini gösterir. Canlı bağlantıdaki şema/istatistik kayıtları bu yoklamayla yüklenmiş sayılmaz.

Yoklama sanal tablo module/connect yoluna girmeden durur; kullanıcı sanal tablo planlayıcısını ilave kez çağırmaz. Toplanan PRAGMA/TEMP/rowest notlarının önceki değerleri geri konur. Gerçek hazırlama ve eski önizlemenin yan etkileri için önceki dört regression testi iki anahtar durumunda da geçti. Genel kullanıcı modülü hata/tekrar giriş eşliği tamamlandı sayılmaz.

Eski doğrulayıcının no such table hatası yalnız C’nin başarıyla ürettiği maskesi 0 programda aşılır. Yeni makine girişi maskeyi tekrar denetler; authorizer, özel collation, UTF-16 ve desteklenmeyen işlem kodu korumaları korunur. UTF-16 korumasının bıraktığı fark D115’tir; makinenin genel UTF-16 eşliği bu adımla kanıtlanmaz.

## Karşılaştırmalar

Yeni derlem: 21 sorgu × 8 bozuk main/aux/TEMP birleşimi × RESET’i hazırlama/adımlama = 336 API girdisi; ek üç girdi bekleyen parametreli SELECT, aktif SELECT/DDL/VACUUM ve hazırlanmış VALUES’in reset/expire davranışını sınar. Bütün yeni girdiler iki anahtar durumunda karşılaştırıldı.

API 2913/2914 bütün çıktı aynı; tek fark önceki D103. Bütün 2914 akışın bütün .db dosyaları aynı. Tam program 11534 aynı, 0 farklı; 148 önceki desteklenmeyen kayıt başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 13957 aynı; hazırlama/adımlama akışı 9806/9806 aynı.

Ayrı iki bağlantı yoklaması: her biri 21 sorgulu soğuk, yüklenmiş ve RESET ile temizlenmiş şema durumu; iki anahtarda toplam 6 akış/126 sorgu. Diğer bağlantı EXCLUSIVE kilit tutarken C/Rust hazırlama, adımlama, satır/hata ve son dosya baytları aynı. Soğuk bağlantıda SELECT 1 BUSY; önceden kodlaması belirlenmiş RESET bağlantısında şema gerektirmeyen SELECT başarılıdır.

Beş yeni ve önceki dokuz hedefli test iki anahtarda da geçti. 896 synchronous, 160 bağlantı/cache GÇ, 16 kalıcı cache yeniden açma ve mevcut üç kodlama dosyasındaki 12 tam program karşılaştırması da eşleşti.

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
| prepare.c | 143 / 184 (77.7%) | 86 / 122 (70.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| **total** | 1675 / 2130 (78.6%) | 980 / 1398 (70.1%) |

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
| sqlite3ReadSchema | prepare.c | 11 | 100.0 | 6 | 83.3 | 1 |
| sqlite3MultiValues | insert.c | 61 | 72.1 | 32 | 46.9 | 13 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 79246 | 2/2 |
| parse.y:524 | 39432 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

SELECT üst kuralının EncodingFixed ve şema okuma sonucu için dört dalı da alındı. 418 alınmayan dal [karar_esligi_v6b_13_dallar.md](karar_esligi_v6b_13_dallar.md) içinde tek tek sınıflandırıldı. VALUES geri düşümü, NOMEM, shared-cache ve diğer açık yollar tamamlandı sayılmaz.

C kapsamı düzeneği aktif SELECT altında beklenen VACUUM hatasını açıkça tanır. P parametre bağlama, B/C ve X yaşam döngüsü işlemleri eklendi; böylece bağlamalar ve finalize durumları gerçek API girdilerini izler. Bu düzeltmeden sonra C sayaçları sıfırlanıp kapsam yeniden çalıştırıldı. Kaynak C ağacı değiştirilmedi.

## D115 — UTF-16’da elenmiş alt sorgu AÇIK

Bağımsız 12 yoklama UTF-8/UTF-16le/UTF-16be, kullanılmayan CTE / false AND ve iki anahtarı kapsar. Sekiz çıktı eşleşir; dört UTF-16 false AND çıktısı farklıdır. Main şema writable_schema ile bozulup RESET yalnız hazırlanır; SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t) C’de satırsız tamamlanır. Rust no such table: t hatası verir; mevcut makine UTF-16 koruması, eski doğrulayıcıyı aşan programın kullanılmasını engeller. Bütün 12 yoklamada dosya baytları aynı.

Ham karşılaştırma ve gerçek dosya kodlama alanı res/utf16_schema_free_limits.json içinde; bu dört kayıt başarı sayılmadı. Sıradaki adım D115’tir. D103, önceki test/DML/ayrışma kusurları, E28, kalan PRAGMA ve M4 işleri de açıktır. V6b veya tam C garantisi verilmez.

## Dört kapı ve maliyet

Testler (geçti/başarısız/yok sayıldı): kapalı (1097, 0, 337), açık (1082, 15, 337); 15 başarısızın adları öncekiyle aynı. Beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; sınıf/seed kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanına karşı CPU 6/tmpfs üzerinde yedi A/B denemesi; anahtar kapalı. Ek üretici yoklaması maliyet getirdi: toplam süre 1,062x, W14 1,369x. Bütün 14 sonuç özeti aynı. Hız ayarı uygulanmadı; ilave derleme maliyeti M3b/makineye geçiş sırasında azaltılacak. Doğruluk kanıtı tamamlanmadan performans için davranış değiştirilmeyecek.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.26      9.11     0.984x     9.22     8.87     9.60    0.95x  OK
W02_create_indexes                5.69      5.69     1.001x     5.61     5.67     8.76    0.65x  OK
W03_pk_point_select              29.85     29.76     0.997x    29.44    29.50    23.30    1.28x  OK
W04_secondary_idx_range           9.49      9.82     1.035x     9.39     9.66     7.46    1.31x  OK
W05_full_scan_filter_agg          4.41      4.31     0.977x     4.39     4.29    16.57    0.26x  OK
W06_join_group_order             10.81     11.32     1.047x    10.80    11.30    28.23    0.40x  OK
W07_window_functions             37.15     38.08     1.025x    37.05    37.95    50.21    0.76x  OK
W08_indexed_update_delete        12.11     11.98     0.989x    11.95    11.92     7.96    1.51x  OK
W09_like_text_scan               12.46     12.83     1.030x    12.43    12.68    18.92    0.68x  OK
W10_jsonb_extract_agg            13.74     14.03     1.021x    13.69    13.82    10.93    1.28x  OK
W11_fts5_index_and_bm25          69.20     70.42     1.018x    68.60    70.13    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.17     40.79     1.016x    39.75    40.72    24.63    1.66x  OK
W13_autocommit_wal_txns           4.56      4.79     1.051x     4.45     4.47     4.12    1.16x  OK
W14_cli_sql_script               38.90     53.25     1.369x    38.55    52.91    26.19    2.03x  OK
TOTAL                           297.79    316.19     1.062x   geomean cand/base = 1.036x   digest mismatches = 0
```

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b13/`. res/source_manifest.json beş kaynak/test dosyasını sabitler. SQL/API girdileri, bütün dosya SHA256 değerleri, tam programlar, gcov, alınmayan dallar ve dört kapı sonuçları saklandı. ileriPlanlama değerlendirilmedi.
