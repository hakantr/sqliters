# V6b.16 — shrink_memory hazırlama etkisi

C SQLite 3.54.1; Rust 1.100. shrink_memory kod üreticiye taşındı; cache boşaltımı C gibi hazırlamada yapılır. Bağımsız başlık okuma hata farkı D117 aşağıda açık.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:2444–2446` | Her geçerli shrink_memory biçimi | sqlite3_db_release_memory hazırlamada çağrılır. Sağ değer kullanılmaz; sonuç satırı veya veri opcode’u eklenmez. | `crates/sqliters-codegen/src/pragma.rs:585` |
| `main.c:922–928` | Artan sırada bağlantı Btree yuvaları | Açık her pager boşaltılır. Şema adı kapsamı sınırlamaz; kapalı TEMP atlanır. | `crates/sqliters-core/src/lib.rs:5842` |
| `pragma.c:449–459` | Açık temp şema adı | Genel hazırlama TEMP Btree’yi önce açar; sonra bütün pager’lar boşaltılır. | `crates/sqliters-core/src/lib.rs:5829` |
| `pager.c:3611–3613; pcache.c:892–895` | Pager→pcache→xShrink | Mevcut cache yordamı kullanılır; ayarlar değişmez. | `crates/sqliters-pager/src/lib.rs:1301` |
| `pcache1.c:841–852` | bPurgeable | Bellek cache’i korunur. Purgeable cache temiz/referanssız LRU sayfalarını atar. | `crates/sqliters-pager/src/pcache.rs:595` |
| `pcache1.c:623–640` | nPurgeable>0, LRU sonu anchor değil | LRU sayfaları çıkarılır. C bulk tahsisi özel allocator ayrıntısıdır; Rust aynı blok yerleşimini kullanmaz. | `crates/sqliters-pager/src/pcache.rs:553` |
| `pragma.c:444–446; vdbeaux.c:667–669; vdbe.c:8385–8392` | Normal yürütme ve reset | runOnlyOnce normal yürütmede expire edilir; reset sonrası hazırlama etkisi tekrarlanır. İlk step tekrar boşaltmaz. | `crates/sqliters-core/src/lib.rs:9378; :8441` |
| `vdbeapi.c:924–930; vdbeaux.c:2313–2318` | EXPLAIN/EQP listeleme | Hazırlama etkisi uygulanır. Listeleme OP_Expire yürütmediğinden reset etkiyi tekrarlamaz; C VFS kaydıyla doğrulandı. | `crates/sqliters-core/src/lib.rs:9373` |
| `pragma.c:3099–3107; sqlite3.c:145214–145218` | Sonuç sütunu yok | Sıfır sütun/satır; pragma_shrink_memory eponymous tablosu yok. Geçersiz şema/sözdizimi C ile karşılaştırıldı. | `crates/sqliters-codegen/src/pragma.rs:89` |

## Kanıt

Önceki ham VFS kaydında C, PRAGMA yalnız hazırlanıp saklandığında altı 4096 baytlık sayfayı cache’ten atıyordu; Rust sorguyu cache’ten karşılıyordu. Yeni kayıt iki tarafta aynı altı sayfanın yeniden okunduğunu gösterir. Canlı imleçli örnekte iki taraf da üç sayfayı yeniden okur; imlecin tuttuğu sayfalar korunur.

Dört şema adı × beş sağ değer biçimi × dört bağlam (autocommit, BEGIN, dirty BEGIN IMMEDIATE, SAVEPOINT) = 80 API girdisi; bir ek metadata/hata girdisi. Hepsi iki anahtarda C ile karşılaştırıldı. main/aux/TEMP verisi, prepare/ilk step/reset, açık imleç, rollback, integrity_check ve dosyalar izlendi.

İç test iki dosya cache’inin yalnız hazırlamada boşaldığını, refill sonrası ilk step’in tekrar boşaltmadığını doğrudan doğrular. Normal PRAGMA reset sonrası tekrar boşaltır; EXPLAIN/EQP listelemesi boşaltmaz. İlk yeni test üç biçimin reset davranışını aynı varsaymıştı; bağımsız C read_probe beklentiyi düzeltti (res/reset_C.json). Düzeltilmiş test iki anahtarda geçti; eski test beklentileri değiştirilmedi.

Toplam API 4157/4158 bütün çıktı aynı; tek fark önceki D103. Bütün 4158 akışın bütün .db baytları aynı. Tam program 13181 aynı, 0 farklı; 126 desteklenmeyen kayıt başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 15632 aynı; hazırlama/adımlama 11459/11459 aynı.

Önceki 896 synchronous, 160 bağlantı/cache GÇ, 16 cold-cache, 12 yeniden açma programı, 18 iki-bağlantı ve altı sayaç akışı da eşleşti.

## Ham GÇ sınırları ve D117

72 VFS senaryosunun tamamında ana/bağlı DB sayfalarının okuma dizisi ve bütün .db baytları aynı. Ham izlerin yalnız 10/72’si tamamen aynı; bütün sayfa türlerini birlikte sayınca 52/72 eşleşir. res/read_after.json iki ham izi, ayrı karşılaştırmaları ve dosya SHA256 değerlerini saklar.

C başlık kontrolünde offset 24’ten 16 bayt; Rust offset 0’dan 100 bayt okuyor. Aynı VFS sıcak sorgu aşamasında ilk 24 bayta dokunan okumaları SQLITE_IOERR_READ ile düşürdüğünde C başarılı; Rust iki anahtarda 10/266 disk I/O error verir. Bu gerçek hata davranışı farkı **D117 açık**. Sonraki iş başlık/pager kararını eşitlemektir. Ham kanıt ../v6b17/res/header_fault_before.json ve iki probe kaynağı.

Dirty işlemli on izde DB sayfa okumaları aynı; C rollback journal’ını dosyadan okurken Rust bellekteki preimage yolunu kullanıyor. TEMP’li diğer on izde Rust özel TEMP dosyasından tekrar sayfa okurken C dirty sayfaları cache’te koruyor. Bu iki gözlem ayrıca açık; hata enjeksiyonunda aynı davranış kanıtlanmadı.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 870 / 1142 (76.2%) | 577 / 794 (72.7%) |
| btree.c | 201 / 229 (87.8%) | 96 / 126 (76.2%) |
| pager.c | 157 / 175 (89.7%) | 102 / 152 (67.1%) |
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
| **total** | 1816 / 2278 (79.7%) | 1074 / 1498 (71.7%) |

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
| sqlite3_db_release_memory | main.c | 11 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerShrink | pager.c | 3 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PcacheShrink | pcache.c | 3 | 100.0 | 0 | 100.0 | 0 |
| pcache1Shrink | pcache1.c | 9 | 100.0 | 2 | 100.0 | 0 |
| pcache1EnforceMaxPage | pcache1.c | 10 | 100.0 | 8 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 89281 | 2/2 |
| parse.y:524 | 44900 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

Yeni sqlite3_db_release_memory, sqlite3PagerShrink, sqlite3PcacheShrink, pcache1Shrink, pcache1EnforceMaxPage işlevlerinde satır ve dal kapsamı %100: toplam 36 satır/14 dal. API_ARMOR gibi bu C derlemesinde kapalı koşullar paydaya dahil değil. Rust’ın aynı allocator bloklarını ayırdığı/boşalttığı iddia edilmez.

460 alınmayan dal [karar_esligi_v6b_16_dallar.md](karar_esligi_v6b_16_dallar.md) içinde tek tek sınıflandırıldı. Seçili opcode bölgeleri bütün sqlite3VdbeExec kapsamı olarak sunulmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1104, 0, 337), açık (1089, 15, 337). 15 başarısızın adları aynı. DML kapalı 378/400, açık 398/400; başarısız sınıf/seed kümeleri aynı. Beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU 6/tmpfs; yedi A/B, anahtar kapalı. Sonuç özetleri aynı; performans ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.27      8.79     0.948x     9.19     8.77     9.60    0.92x  OK
W02_create_indexes                5.77      5.76     0.998x     5.63     5.73     8.76    0.66x  OK
W03_pk_point_select              30.18     29.75     0.986x    29.37    29.54    23.30    1.28x  OK
W04_secondary_idx_range           9.47      9.64     1.018x     9.41     9.51     7.46    1.29x  OK
W05_full_scan_filter_agg          4.41      4.31     0.978x     4.40     4.30    16.57    0.26x  OK
W06_join_group_order             10.81     11.08     1.025x    10.77    11.07    28.23    0.39x  OK
W07_window_functions             37.25     38.25     1.027x    37.11    38.06    50.21    0.76x  OK
W08_indexed_update_delete        12.14     11.79     0.971x    12.07    11.61     7.96    1.48x  OK
W09_like_text_scan               12.51     12.61     1.008x    12.43    12.59    18.92    0.67x  OK
W10_jsonb_extract_agg            13.77     13.51     0.981x    13.69    13.46    10.93    1.24x  OK
W11_fts5_index_and_bm25          70.05     70.34     1.004x    69.08    69.67    57.02    1.23x  OK
W12_rtree_insert_and_bbox        41.00     40.62     0.991x    39.73    39.71    24.63    1.65x  OK
W13_autocommit_wal_txns           4.47      4.72     1.055x     4.33     4.47     4.12    1.15x  OK
W14_cli_sql_script               39.18     54.22     1.384x    38.98    53.36    26.19    2.07x  OK
TOTAL                           300.27    315.38     1.050x   geomean cand/base = 1.022x   digest mismatches = 0
```

## Kalan iş ve kayıt

Sonraki adım D117; ardından kalan PRAGMA işleyicileri. D103, TEMP/journal GÇ gözlemleri, mevcut test/DML/ayrışma farkları, E28, M4, genel UTF-16/geçersiz UTF-8 ve diğer iş listesi açık. V6b veya tam C işlevselliği tamamlanmadı.

Kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b16/`. res/source_manifest.json iki kaynak/test dosyasını sabitler. SQL/API girdileri, dosya SHA256 değerleri, tam programlar, gcov ve dört kapı sonuçları saklandı. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
