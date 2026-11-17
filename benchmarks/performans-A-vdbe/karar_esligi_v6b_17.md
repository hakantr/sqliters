# V6b.17 — D117 sıcak başlık kontrolü

C SQLite 3.54.1; Rust 1.100. Sıcak rollback okuması C’nin 16 baytlık sürüm karşılaştırmasına taşındı. D117’nin yoklanan IOERR_READ durumları eşitlendi; kısa sayfa okumasındaki D118 ayrı ve açık.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pager.c:5316–5334` | OPEN ve önceki retained READER | Korunmuş fiziksel SHARED/EXCLUSIVE kilit varsa sürüm yoklaması tekrarlanmaz. locking_mode=normal yalnız prepare edildiğinde de eski okuma kilidi korunur. | `crates/sqliters-pager/src/lib.rs:2831` |
| `pager.c:5452–5472` | !tempFile ve hasHeldSharedLock | İlk alımda atlanan; sonraki normal alımda offset24/16 byte okunan sürüm kontrolü. Memory/TEMP özel semantiği korunur. | `crates/sqliters-pager/src/lib.rs:2893` |
| `pager.c:5473–5479` | Sürüm kontrolü okuma sonucu | SHORT_READ tüm 16 byte’ı sıfır sayar; diğer hata doğrudan döner. Sonraki sayfa1 yüklemesinin D118 farkı aşağıda açık. | `crates/sqliters-pager/src/lib.rs:2897` |
| `pager.c:5480–5488; 1821–1825` | 16 byte farklı | Cache sıfırlanır, backup restart edilir, data_version bir kez artar. Yalnız change counter karşılaştırması yapılmaz. | `crates/sqliters-pager/src/lib.rs:2906` |
| `pager.c:5509–5517` | Normal alımda pagerPagecount | Cache varken de xFileSize hatası korunur; retained reader bu metadata yoklamasını atlar. VFS FSTAT hata deneyi C ile karşılaştırıldı. | `crates/sqliters-pager/src/lib.rs:2922` |
| `pager.c:3087–3118; 4537–4543` | Sayfa1 depodan okundu veya DB dosyasına yazıldı | Ham [24..40] saklanır; mutasyona uğramış mantıksal başlıktan türetilmez. WAL commit tek başına depodan okunan sürümü ilerletmez. | `crates/sqliters-pager/src/lib.rs:384` |
| `pager.c:3102–3118` | Sayfa1 okuma hatası | Başarısız gerçek sayfa okumada FF sürümü; başarılı okumada ham sürüm. FF kopyasının karşılığı var; üst katmanın hatayı yutması D119 olarak açık. | `crates/sqliters-pager/src/lib.rs:1606` |
| `pager.c:2530–2534; 6441` | Rollback/savepoint/hot journal sayfa1 geri yükleme | Geri yüklenen ham sürüm saklanır; sonraki normal alım doğru görüntüyle karşılaştırılır. | `crates/sqliters-pager/src/lib.rs:5176` |
| `btree.c:3320–3323` | Sayfa1 getirme hatası | C hatayı döndürür; Rust set_state içindeki is_ok hatayı yutar. Bu karşılık eksik: D119 açık; sonraki adımda düzeltilecek. | `crates/sqliters-pager/src/lib.rs:1401` |
| `btree.c:3451–3454` | Cache’teki değişmemiş sayfa1 | Okuma yeniden açıldığında live auto_vacuum bayrakları cached başlıktan yenilenir. BUSY sonrası prepare etkisi böyle geri okunur. | `crates/sqliters-pager/src/lib.rs:2917` |
| `btree.c:3330–3336; pager.c:5480` | Sıfır başlık sayfa sayısı | Fiziksel dosya boyutuna normalize edilir; karşılaştırma sürümünde ham sıfır korunur. Aynı dosyayı tekrar okumak data_version’ı tekrar artırmaz. | `crates/sqliters-pager/src/lib.rs:2998` |
| `pager.c:5497–5506; btree.c:3371–3386` | Bekleyen WAL keşfi/açılışı | Cache’te başlık olsa da WAL açılışı atlanmaz. İlk denemedeki +1 sayaç regresyonu bu korumayla giderildi. | `crates/sqliters-pager/src/lib.rs:2913` |

## Native C kanıtı ve sınırlar

D117 önceki kayıtta aynı VFS ilk 24 bayta dokunan okumaları düşürünce C başarılı, Rust 10/266 disk I/O error veriyordu. Son native dört temel akışın sonucu, ham izi ve .db SHA256 değeri aynı (res/header_fault_after.json).

İki anahtar × üç kilit durumu × beş hata senaryosu = 30 akış. Kilit durumları normal, exclusive ve yalnız hazırlanmış exclusive→normal geçişidir. Dört okuma hata bölgesi [0,24), [24,40), [40,100), [0,100); beşinci senaryo xFileSize hatasıdır. Hepsinde sonuç, hata kodu/ileti ve dosya baytları aynı. Normal/exclusive durumlarının 20 ham izi tamamen aynı. Geçişteki 10 izde C eski journal’ı sorgu sonunda silerken Rust kapanışa kadar tutuyor; bu zamanlama gözlemi açık. res/error_matrix.json her iki ham izi saklar.

C kilitleme modunun prepare değişimi mevcut okuma kilidini hemen bırakmıyor. İlk Rust guard’ı yalnız geçerli mode değerini kullanmıştı; native yoklama fiziksel kilidin esas alınmasını gösterdi. Son kaynak bu ayrımı uygular. Önceki doğrulama res/before_retained_reader_fix altında saklandı.

İlk guard bekleyen WAL açılışını atlıyordu: 896 sync örneğinin 224 WAL dosyasında sayaç 2 yerine 3 oldu. Bu regresyon kabul edilmedi. Guard WAL başlık/modunu da gözetir; son 896 örneğin tamamı hem izde hem dosya baytlarında aynı. Hatalı ham C/R dosyaları res/wal_bytes_regression altında; ilk sonuçlar res/before_pending_wal_guard altında.

Ham sayfa sayısını sıfırlayan ayrı native yoklama: C ve Rust data_version 1→2→2, count/sum 2/3 ve integrity_check=ok verir; dosyalar aynı. Bu, counter dışındaki 12 byte’ın izlendiğini ve normalize edilmiş başlıktan sahte bir sürüm türetilmediğini doğrular. res/raw_version.json; raw_version.c ve raw-version-r kaynakları.

Tam testte yakalanan cached auto_vacuum yenileme eksikliği kabul edilmedi. Ek native iki bağlantı yoklaması iki anahtarda C ile aynı: initial 1, BUSY 5/5, before_read 2, after_read 1, VACUUM sonrası 2; bütün dosya baytları aynı (res/auto_refresh.json). Önceki başarısız test/kayıt res/before_cached_av_refresh altında saklandı.

72 VFS örneğinde bütün DB sayfa okuma dizileri ve .db baytları aynı; ham izlerin 14’ü tamamen aynı. Cache-miss sayfa1 yolunda Rust hâlâ 100 byte başlık yükleyebilir, C tam sayfa okur. Önceki TEMP dirty-cache ve rollback-journal okuma gözlemleri de açık.

**D119 açık:** cache boşaltıldıktan sonra yalnız sayfa 1’in 100..4095 byte aralığındaki okumalar IOERR_READ ile düşürülünce C 10/266 disk I/O error; Rust başarılı RESULT 0 döndürür. set_state içindeki is_ok okuma hatasını yutar. İki anahtarın ham kayıtları ../v6b18/res/page1_error_before.json içinde; .db baytları aynı. Öncelikli sonraki düzeltme bu hata aktarımı ve D118 sayfa1 yüklemesidir.

**D118 açık:** sıfır doldurulmuş SHORT_READ veren aynı VFS’de C 26/26 file is not a database, Rust 10/522 disk I/O error döndürür (iki anahtar). Genel read_db_page kısa okumayı normalleştirse de ayrı 100 byte başlık okuyucusu aynı yolu izlemez. Sonraki iş sayfa1 yükleme/kısa okuma kararını eşitlemektir. Kanıt ../v6b18/res/short_before.json; bütün .db baytları bu yoklamada aynı.

Üç yeni pager testi dahil 14 pager testi geçti: sıcak normal/retained/mod geçişi, tüm 16 ham byte ve pending WAL kontrolü. Bu testler C native yoklamalarının yerine sunulmaz.

Tam program 13181 aynı; 126 Unsupported başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 15632 aynı; hazırlama/adımlama 11459/11459 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103. Bütün 4158 akışın .db baytları aynı. 896 sync, 160 bağlantı/cache, 16 cold-cache, 12 yeniden açma programı, 18 iki-bağlantı ve 6 sayaç akışı aynı.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 870 / 1142 (76.2%) | 577 / 794 (72.7%) |
| btree.c | 206 / 229 (90.0%) | 98 / 126 (77.8%) |
| pager.c | 211 / 259 (81.5%) | 140 / 222 (63.1%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 43 / 45 (95.6%) | 16 / 20 (80.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 143 / 184 (77.7%) | 86 / 122 (70.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| **total** | 1875 / 2362 (79.4%) | 1114 / 1568 (71.0%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 72.6 | 675 | 70.1 | 117 |
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
| btreeEndTransaction | btree.c | 15 | 100.0 | 8 | 87.5 | 1 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 78.6 | 3 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 64.3 | 3 |
| lockBtree | btree.c | 73 | 74.0 | 44 | 65.9 | 14 |
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
| sqlite3VdbeSwap | vdbeaux.c | 19 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_stmt_status | vdbeapi.c | 15 | 40.0 | 4 | 75.0 | 1 |
| sqlite3_limit | main.c | 13 | 84.6 | 12 | 66.7 | 2 |
| sqlite3DecOrHexToI64 | util.c | 14 | 100.0 | 16 | 93.8 | 1 |
| sqlite3Atoi64 | util.c | 51 | 84.3 | 54 | 83.3 | 6 |
| sqlite3_db_release_memory | main.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerShrink | pager.c | 3 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PcacheShrink | pcache.c | 3 | 100.0 | 0 | 100.0 | 0 |
| pcache1Shrink | pcache1.c | 9 | 100.0 | 2 | 100.0 | 0 |
| pcache1EnforceMaxPage | pcache1.c | 10 | 100.0 | 8 | 100.0 | 0 |
| sqlite3PagerSharedLock | pager.c | 60 | 51.7 | 58 | 48.3 | 15 |
| readDbPage | pager.c | 19 | 94.7 | 12 | 83.3 | 2 |
| pager_reset | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 89342 | 2/2 |
| parse.y:524 | 44928 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

sqlite3PagerSharedLock/readDbPage genel yordamları ve pager_reset bu dilimde kapsama eklendi; yüzdeler bütün yordamların genel hot-journal/WAL/hata yönlerini de paydaya katar. Kapsam C eşliği iddiası değildir. Alınmayan 490 dalın her biri [karar_esligi_v6b_17_dallar.md](karar_esligi_v6b_17_dallar.md) içinde ayrı listelendi; yeni yordamların koşulları ayrıca tek tek sınıflandırıldı.

Özellikle hot-journal READWRITE/READONLY bayrak ayrımı, journal sync sırası ve hata durumunun PAGER_ERROR üzerinden açılması Rust’ta ayrıca tamamlanmadı; E28/genel pager doğruluğu açık. Seçili opcode bölgeleri bütün sqlite3VdbeExec sayılmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1107, 0, 337), açık (1092, 15, 337). Başarısız adları ve DML sınıf/seed kümeleri öncekiyle aynı. DML kapalı 378/400, açık 398/400; beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı, CPU 6/tmpfs, yedi A/B, anahtar kapalı. Sonuç özetleri aynı. Değişiklik GÇ hata davranışını düzeltir; performans ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.20      8.80     0.957x     9.12     8.73     9.60    0.92x  OK
W02_create_indexes                5.66      5.71     1.009x     5.62     5.63     8.76    0.65x  OK
W03_pk_point_select              29.60     30.61     1.034x    29.41    29.97    23.30    1.31x  OK
W04_secondary_idx_range           9.60      9.85     1.026x     9.43     9.77     7.46    1.32x  OK
W05_full_scan_filter_agg          4.39      4.36     0.993x     4.39     4.35    16.57    0.26x  OK
W06_join_group_order             10.78     11.02     1.022x    10.74    11.00    28.23    0.39x  OK
W07_window_functions             36.98     37.71     1.020x    36.93    37.62    50.21    0.75x  OK
W08_indexed_update_delete        12.01     11.94     0.994x    11.92    11.93     7.96    1.50x  OK
W09_like_text_scan               12.60     12.40     0.984x    12.51    12.36    18.92    0.66x  OK
W10_jsonb_extract_agg            13.75     13.79     1.003x    13.63    13.68    10.93    1.26x  OK
W11_fts5_index_and_bm25          70.10     70.18     1.001x    68.63    69.16    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.66     40.87     1.005x    40.08    40.22    24.63    1.66x  OK
W13_autocommit_wal_txns           4.54      4.51     0.994x     4.45     4.50     4.12    1.10x  OK
W14_cli_sql_script               38.50     53.22     1.383x    38.34    52.93    26.19    2.03x  OK
TOTAL                           298.36    314.97     1.056x   geomean cand/base = 1.026x   digest mismatches = 0
```

## Kalan iş ve kayıt

Sıradaki adım D119 hata aktarımı ve D118 sayfa1 yükleme yolu; sonra kalan PRAGMA. D103, TEMP/journal gözlemleri, E28, M4, genel UTF-16/geçersiz UTF-8 ve önceki test/DML/ayrışma farkları açık. Tam C eşliği ve yayın doğrulaması henüz tamamlanmadı.

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b17/`; res/source_manifest.json pager kaynağını ve üç yeni iç testi sabitler. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
