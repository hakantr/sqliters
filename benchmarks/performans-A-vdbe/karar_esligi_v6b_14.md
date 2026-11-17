# V6b.14 — UTF-16 boş yürütme yolu ve yeniden hazırlama sayaçları

C SQLite 3.54.1; Rust 1.100. D115’in kaydedilen UTF-16 satırsız alt sorgu farkı kapandı. Kodlamadan bağımsız boş yürütme yolu için makineye dar geçiş eklendi. Doğrulamada görülen D116 (yeniden hazırlamada kaybolan sayaçlar) da düzeltildi. Genel UTF-16 makine koruması ve diğer açık işler devam eder.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `vdbe.c:9224–9294` | Başlangıç komutu ve ilk atlama | Yalnız PC 0’daki Init kabul edilir. P2 hedefine gidilir; sonraki komutlar aynı denetimden geçer. İzleme/kesme davranışını gerçek makine yürütür. | `crates/sqliters-core/src/vdbe_host.rs:201` |
| `vdbe.c:1120–1168` | Koşulsuz Goto | P5=0 ve geçerli P2 izlenir. Negatif/sınır dışı hedef, döngü veya farklı komut bu dar geçişi reddeder. İlerleme ve kesme makinenin mevcut yolundadır. | `crates/sqliters-core/src/vdbe_host.rs:210` |
| `vdbe.c:1428–1434` | Başlangıçta sabit tamsayı argümanı | OP_Integer kodlama dönüşümü yapmaz; tamsayı yüklenip sonraki komuta geçilir. randomblob(1) gövdesi çalışmasa da 1’in başlangıçta yüklenmesi C ile aynı kalır. | `crates/sqliters-core/src/vdbe_host.rs:214` |
| `vdbe.c:1350–1418` | Başarılı Halt / hata veya sonuç satırı | Yalnız Halt P1=0 ile biten yol kabul edilir. ResultRow, metin/işlev/imleç komutu veya hata duruşu görülen yol genel makine korumalarına bırakılır. | `crates/sqliters-core/src/vdbe_host.rs:213` |
| `vdbeaux.c:2050–2057` | Btree maskesi ve EXPLAIN | Maske 0 ve normal yürütme gerekir; EXPLAIN veya Btree kullanan program bu boş yol geçişine alınmaz. Böylece UTF-16’da satırsız biten elenmiş alt sorgu çalışırken şema okunmaz. | `crates/sqliters-core/src/vdbe_host.rs:203` |
| `vdbeaux.c:152–153` | Başarılı yeniden hazırlama | Önceki aCounter kopyalanır; REPREPARE sayacı sarmalı artırılır. VM_STEP ve RUN artık eski çalışmaları kaybetmez. Aynı aktarım makinenin SQLITE_SCHEMA yeniden denemesinde de kullanılır. | `crates/sqliters-core/src/lib.rs:7230` |
| `vdbeapi.c:2238–2239` | Sayaç okuma / resetFlag | Seçili sayaç okunur; istenirse yalnız o sayaç sıfırlanır. Sonraki yeniden hazırlama kalan sayaçları ve sıfırlanmış değeri korur. | `crates/sqliters-vdbe/src/vdbe.rs:1890` |

## Yürütme kanıtı ve sınır

C’nin SELECT 17 WHERE 0 AND EXISTS(SELECT * FROM t) programı 6 komutludur. Normal PC yolu 0 → 5 → 1 → 4’tür: Init, Goto, Goto, başarılı Halt. Integer/ResultRow gövdesi çalışmaz. randomblob(1), length(İ) örneğinde PC yolu 0 → 9 → 10 → 1 → 8’dir; başlangıçta tamsayı argümanı yüklenir, işlevler çalışmaz. Ham C programları res/empty_program_C.txt ve res/empty_variants_C.txt içinde.

Yol çözümlemesi bağlanmış değerleri veya SQL metnini tahmin ederek değerlendirmez; C üreticinin komutlarını izler. En çok komut sayısı kadar ilerleyerek döngüyü reddeder. Yalnız kontrol akışı, sabit tamsayı hazırlaması ve başarılı Halt kabul edilir. Metin işleme, sonuç satırı, imleç ve işlev yürütmesine bu adımla UTF-16 izni verilmez. Authorizer ve özel collation korumaları korunur; genel UTF-16/invalid UTF-8 iş listesi kapanmış sayılmaz.

## Karşılaştırmalar

Yeni API derlemi: üç kodlama × dört main/TEMP bozuk şema birleşimi × beş ifade = 60 girdi. Unicode sütun adı, çalıştırılmayan işlevler, atılan parametreler, reset, advisory expire, sütun metadata ve VM_STEP/RUN sayaçları sınanır. Ek 21 girdi ilerleme handlerinin devam/kesme eşiklerini kapsar. Bunların tamamı iki anahtar durumunda karşılaştırıldı.

Toplam API 3075/3076 bütün çıktı aynı; tek fark önceki D103. Bütün 3076 akışta bütün .db dosyaları C ile aynı. 11654 tam program aynı, 0 farklı; 148 önceki desteklenmeyen kayıt başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 14077 aynı; hazırlama/adımlama akışı 9926/9926 aynı.

İki bağlantı: UTF-8/UTF-16le/UTF-16be × soğuk/yüklü/RESET şema × iki anahtar = 18 akış ve 378 sorgu. Diğer bağlantı EXCLUSIVE kilit tutarken bütün hazırlama/adımlama, satır/hata ve dosya baytları aynı. Şema isteyen sorgular kilidi yine C gibi ister. Ayrı altı sayaç akışında VM_STEP, RUN, REPREPARE ve sayaç sıfırlaması C ile aynı; dosyalar da aynı.

Üç yeni ve önceki on dört hedefli test iki anahtarda da geçti. 896 synchronous, 160 bağlantı/cache GÇ, 16 kalıcı cache yeniden açma ve mevcut üç kodlama dosyasında 12 tam program karşılaştırması da eşleşti.

## D116 — sayaç kaybı

İlk API denemesi 120 akışta yalnız stmt_status farkı gösterdi; satırlar ve bütün dosya baytları aynıydı. İki çalışmadan sonraki expire/yeniden hazırlamada C VM_STEP=12, RUN=3; Rust VM_STEP=4, RUN=1 veriyordu. Eski VM atılırken sayaçlar kayboluyordu. vdbeaux.c:152–153 ile aynı sayaç aktarımı ve REPREPARE artışı eklendi. C/Rust ayrı yerel yoklama, sayaçların ayrı ayrı sıfırlanmasını da karşılaştırır. İlk karşılaştırmanın araç kaydından alınan özet/örnek res/counter_failure_excerpt.json içinde; eski tam API dosyası son başarılı koşuda üzerine yazıldı. Bu özet eski ham dosyanın kopyası olarak sunulmaz. Nihai API, sayaç ve dosya sonuçları tam olarak saklandı.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 860 / 1142 (75.3%) | 569 / 794 (71.7%) |
| btree.c | 199 / 229 (86.9%) | 95 / 126 (75.4%) |
| pager.c | 154 / 172 (89.5%) | 102 / 152 (67.1%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 21 / 21 (100.0%) | 4 / 4 (100.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 143 / 184 (77.7%) | 86 / 122 (70.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| **total** | 1700 / 2164 (78.6%) | 983 / 1402 (70.1%) |

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
| sqlite3VdbeSwap | vdbeaux.c | 19 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_stmt_status | vdbeapi.c | 15 | 40.0 | 4 | 75.0 | 1 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 80344 | 2/2 |
| parse.y:524 | 39840 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

Seçilen opcode blokları sqlite3VdbeExec bütününün kapsandığı anlamına gelmez. Toplam 455 alınmayan dal [karar_esligi_v6b_14_dallar.md](karar_esligi_v6b_14_dallar.md) içinde ayrı sınıflandırıldı: genel trace, dış interrupt, Once-reset eşiği, altprogram/hata Halt, MEMUSED, tahsis, shared-cache ve diğer açık yollar ayrıca belirtilir. Sabit ilerleme handleri ile kesme/devam akışları yerel C kapsamına dahil edildi.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1100, 0, 337), açık (1085, 15, 337); 15 başarısızın adları öncekiyle aynı. F4/F6–F9 beklentileri değiştirilmedi. DML kapalı 378/400, açık 398/400; başarısız sınıf ve seed kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı, CPU 6, tmpfs, yedi A/B denemesi; anahtar kapalı. Bütün sonuç özetleri aynı. S13’teki ek üretici maliyeti bu adımın performans hedefi değildir; hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.24      9.32     1.009x     9.19     9.24     9.60    0.97x  OK
W02_create_indexes                5.77      5.77     1.000x     5.70     5.70     8.76    0.66x  OK
W03_pk_point_select              30.08     30.26     1.006x    29.40    29.84    23.30    1.30x  OK
W04_secondary_idx_range           9.53      9.74     1.023x     9.40     9.60     7.46    1.30x  OK
W05_full_scan_filter_agg          4.40      4.39     0.999x     4.39     4.38    16.57    0.27x  OK
W06_join_group_order             10.79     10.88     1.009x    10.76    10.86    28.23    0.39x  OK
W07_window_functions             37.24     38.19     1.025x    37.21    38.03    50.21    0.76x  OK
W08_indexed_update_delete        12.15     11.92     0.981x    12.09    11.89     7.96    1.50x  OK
W09_like_text_scan               12.46     12.57     1.009x    12.42    12.47    18.92    0.66x  OK
W10_jsonb_extract_agg            13.76     13.90     1.010x    13.70    13.85    10.93    1.27x  OK
W11_fts5_index_and_bm25          69.55     70.33     1.011x    68.67    70.13    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.65     40.55     0.998x    39.48    39.82    24.63    1.65x  OK
W13_autocommit_wal_txns           4.55      4.74     1.043x     4.50     4.66     4.12    1.15x  OK
W14_cli_sql_script               38.96     53.95     1.385x    38.75    53.55    26.19    2.06x  OK
TOTAL                           299.12    316.50     1.058x   geomean cand/base = 1.032x   digest mismatches = 0
```

## Kalan iş ve kanıt

D115 kaydedilen satırsız programlar için, D116 sayaç aktarımı için düzeltildi. D103, mevcut 15 test başarısızlığı, DML/ayrışma farkları, E28, genel UTF-16/geçersiz UTF-8 ve diğer iş listesi açıktır. Sonraki adım kalan PRAGMA işleyicileri; önce threads ve shrink_memory hazırlama etkileri, ardından M4. V6b veya tam C işlevselliği tamamlandı sayılmaz.

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b14/`. res/source_manifest.json dört kaynak/test dosyasını sabitler. SQL/API girdileri, dosya SHA256 değerleri, programlar, gcov ve bütün kapı sonuçları saklandı. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
