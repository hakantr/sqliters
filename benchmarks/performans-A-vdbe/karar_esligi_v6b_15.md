# V6b.15 — threads hazırlama ve bağlantı sınırı

C SQLite 3.54.1; Rust 1.100. PRAGMA threads kod üreticiye taşındı; C’nin sayı kabulü, 31 bit maskesi, üst sınırı, donmuş satırı ve bağlantı kapsamı uygulanır. pragma_threads eponymous tablo yolu eklendi. Bu adım işçi sınırını taşır; paralel sıralayıcı uygulaması eklemez.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:2716–2725` | Sorgu veya geçersiz atama | Mevcut worker-thread sınırı döner. Eksik değer, bozuk sayı, fazla metin, i64 taşması ve negatif değer atama yapmaz. | `crates/sqliters-codegen/src/pragma.rs:101` |
| `util.c:1276–1297; 1173–1260` | Ondalık/onaltılık parse sonucu | Yalnız SQLITE_OK ve N>=0 kabul edilir. Mevcut C tamsayı yordamı paylaşılır; Rust sayısal dönüştürmesine farklı tolerans eklenmez. | `crates/sqliters-codegen/src/pragma.rs:102` |
| `pragma.c:2718–2722` | Geçerli N | Önce 31 bit maske, sonra sqlite3_limit üst sınırı uygulanır. 2147483648 değeri 0; 0x80000007 değeri 7 olur. | `crates/sqliters-codegen/src/pragma.rs:103` |
| `sqliteInt.h:744–760; main.c:3084,3095–3103` | Derlemenin worker üst sınırı | Başvuru THREADSAFE derlemesinde üst sınır 8, varsayılan 0. Ham sabit üreticisi kapalı THREADSAFE==0 kolundaki 0’ı seçmişti; kullanılmayan eski sabit 8 ile düzeltildi. | `crates/sqliters-codegen/src/consts.rs:14` |
| `pragma.c:2722–2725` | Hazırlama ve EXPLAIN hazırlama | Atama hazırlamada bağlantı sınırını değiştirir; sonuç satırı o andaki değerden donar. Getter ve setter ilk adımlamada önceki donmuş değerini verir. | `crates/sqliters-core/src/lib.rs:5841` |
| `pragma.c:2714–2726; 444–446` | Expire ve reset | threads ataması diğer bekleyen ifadeleri expire etmez. PRAGMA’nın runOnlyOnce davranışı korunur; reset sonrası setter yeniden hazırlanıp atamasını tekrar uygular. | `crates/sqliters-codegen/src/pragma.rs:614` |
| `pragma.c:449–459` | Şema adı ve TEMP | Ayar bağlantı genelidir; main/temp/aux aynı sınırı görür. Açıkça temp adı hazırlamada mevcut genel TEMP açma yolunu izler. İşlem rollback’i ayarı geri almaz. | `crates/sqliters-core/src/db_set.rs:289` |
| `pragma.c:3099–3107` | pragma_threads sanal tablosu | Tek threads sütunu, argümansız eponymous tablo. Filtre adımlamasındaki canlı ayarı görür; fazla argüman ve filtre sonuçları C ile karşılaştırıldı. | `crates/sqliters-core/src/vtab.rs:3543` |

## Kanıt

Başvuru C ikilisinde PRAGMA threads varsayılan 0; threads=2147483647 sonucu 8. compile_options DEFAULT_WORKER_THREADS=0 ve MAX_WORKER_THREADS=8 bildirir. sqliteInt.h’de THREADSAFE==0 için 0, diğer derleme için 8 seçilir. Raw gen_consts.py koşullu derlemeyi değerlendirmeden ilk define’ı seçtiğinden eski sabit yanlıştı. Düzeltilen sabitin diğer üretici yollarında kullanımı yoktu; yeni API sınır testi yeniden üretimdeki yanlış 0’ı da yakalar.

38 değer × dört şema adı × üç bağlam (boş, tablolu, işlem içi) = 456 API girdisi. Ek dört girdi reset, expire, sanal tablo ve büyük sıralamayı kapsar. Her girdi iki anahtar durumunda karşılaştırıldı. Sınırlar, hex işareti, boşluk/artık metin, i64 taşması, temp/aux ve rollback davranışı C ile aynı.

İlk yeni testte reset sonrası setter’ın uygulanmaması beklenmişti; C’nin ayrı hazırlanmış ifadelerle yoklaması bu beklentiyi düzeltti: setter resetlenip yeniden adımlanınca yeniden hazırlanır ve 7’yi tekrar uygular. Bu yeni test C sonucuna göre düzenlendi; mevcut F4/F6–F9 testlerine dokunulmadı. Yerel threads_reset.ops ve res/reset_C.txt ham kanıttır.

Toplam API 3995/3996 bütün çıktı aynı; tek fark önceki D103. Bütün 3996 akışın bütün .db dosyaları aynı. Tam program 13009 aynı, 0 farklı; 137 desteklenmeyen kayıt başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 15456 aynı; hazırlama/adımlama akışı 11294/11294 aynı.

threads=8 ve cache_size=16 altında 40.000 satırlı, uzun metinli veri üzerinde indeks oluşturma, özet sorgu ve integrity_check sonuçları C ile aynı; main dosyasının bütün baytları da aynı. Bu test worker sınırının sonuç/dosya eşliğini sınar; Rust’ın C ile aynı sayıda işletim sistemi işçisi açtığını kanıtlamaz.

Üç yeni ve önceki on yedi hedefli test iki anahtarda da geçti. 896 synchronous, 160 bağlantı/cache GÇ, 16 kalıcı cache yeniden açma, mevcut üç kodlamada 12 tam program, 18 iki-bağlantı akışı ve altı sayaç akışı da eşleşti.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 867 / 1142 (75.9%) | 576 / 794 (72.5%) |
| btree.c | 199 / 229 (86.9%) | 95 / 126 (75.4%) |
| pager.c | 154 / 172 (89.5%) | 102 / 152 (67.1%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 32 / 34 (94.1%) | 12 / 16 (75.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 143 / 184 (77.7%) | 86 / 122 (70.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| **total** | 1775 / 2242 (79.2%) | 1058 / 1484 (71.3%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 72.3 | 675 | 69.9 | 117 |
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
| sqlite3_limit | main.c | 13 | 84.6 | 12 | 66.7 | 2 |
| sqlite3DecOrHexToI64 | util.c | 14 | 100.0 | 16 | 93.8 | 1 |
| sqlite3Atoi64 | util.c | 51 | 84.3 | 54 | 83.3 | 6 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 87647 | 2/2 |
| parse.y:524 | 44406 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

462 alınmayan dal [karar_esligi_v6b_15_dallar.md](karar_esligi_v6b_15_dallar.md) içinde ayrı sınıflandırıldı. Özellikle genel atoi64 UTF-16 tampon yolu, büyük X öneki, metnin içindeki artı işareti, uzun ondalık sınırı ve diğer genel API yönleri bu derlemle tamamlandı sayılmaz. Seçili opcode blokları tüm sqlite3VdbeExec kapsamı olarak sunulmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1103, 0, 337), açık (1088, 15, 337); 15 başarısızın adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; başarısız sınıf/seed kümeleri aynı. Beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı, CPU 6/tmpfs, yedi A/B denemesi; anahtar kapalı. Bütün sonuç özetleri aynı. Performans ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.28      8.82     0.950x     9.18     8.78     9.60    0.92x  OK
W02_create_indexes                5.70      5.74     1.008x     5.63     5.67     8.76    0.66x  OK
W03_pk_point_select              30.11     29.51     0.980x    29.49    29.38    23.30    1.27x  OK
W04_secondary_idx_range           9.64      9.76     1.012x     9.50     9.57     7.46    1.31x  OK
W05_full_scan_filter_agg          4.41      4.31     0.978x     4.40     4.30    16.57    0.26x  OK
W06_join_group_order             10.82     11.09     1.024x    10.79    11.07    28.23    0.39x  OK
W07_window_functions             37.23     38.38     1.031x    37.10    38.21    50.21    0.76x  OK
W08_indexed_update_delete        12.14     11.81     0.973x    12.08    11.71     7.96    1.48x  OK
W09_like_text_scan               12.44     12.61     1.014x    12.40    12.58    18.92    0.67x  OK
W10_jsonb_extract_agg            13.77     13.63     0.990x    13.70    13.51    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.87     70.93     1.015x    69.51    70.34    57.02    1.24x  OK
W12_rtree_insert_and_bbox        40.41     41.03     1.015x    39.74    40.40    24.63    1.67x  OK
W13_autocommit_wal_txns           4.58      4.73     1.033x     4.33     4.58     4.12    1.15x  OK
W14_cli_sql_script               39.11     53.32     1.363x    38.77    52.65    26.19    2.04x  OK
TOTAL                           299.52    315.68     1.054x   geomean cand/base = 1.024x   digest mismatches = 0
```

## Kalan iş

Sıradaki adım shrink_memory’nin hazırlama etkisidir. C’nin hazırlamadaki cache boşaltımı, ayrı VFS okuma gözlemcisiyle gösterildi; eski Rust yolu yalnız adımlamada uygular. Ham önceki okuma kayıtları ../v6b16/res/read_before.json içinde. Başlık kontrolü okumalarında da mevcut kapsam farkı vardır (C offset24/16 byte; Rust offset0/100 byte); bu gözlem ayrı tutulur, shrink eşliği diye gizlenmez.

D103, mevcut test/DML/ayrışma farkları, E28, kalan PRAGMA, M4, genel UTF-16/geçersiz UTF-8 ve diğer iş listesi açıktır. V6b veya tam C işlevselliği tamamlandı sayılmaz.

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b15/`. res/source_manifest.json sekiz kaynak/test dosyasını sabitler. SQL/API girdileri, bütün dosya SHA256 değerleri, tam programlar, gcov ve dört kapı sonuçları saklandı. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
