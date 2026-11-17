# V6b.18 — D119 sayfa1 I/O hatası ve okuyucu rollback kararı

C SQLite 3.54.1; Rust 1.100. D119’da sayfa1 okuma hatasının başarıya dönüşmesi giderildi. Ortak adımlama yolunda yürütme IOERR’si işlemi kapatır; reprepare IOERR’si işlemi korur. D118 kısa okuma ve genel E28 pager hata durumu açık.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `btree.c:3320–3323` | SharedLock başarılı, sayfa1 getirme başarısız | C getirme hatasını çağırana döndürür. R Read durumuna geçip sayfa1 pinini yalnız başarıda alır; is_ok ile hata yutulmaz. | `crates/sqliters-pager/src/lib.rs:2848` |
| `pager.c:5691–5699; 5513–5522` | Sayfa1 hatasından sonra temizleme | Başarılı SharedLock sonrasındaki hata okuma snapshot’ını bitirir. Açılmış WAL, başarısız WAL açılışı gibi kapatılmaz; mod korunur. | `crates/sqliters-pager/src/lib.rs:2855` |
| `vdbeaux.c:3357–3401` | bIsReader ve primary IOERR | Salt okuma da bütün işlemi geri alır. Ortak step_in_session sonucu C programının b_is_reader bilgisiyle, eski yürütücüde schema gereksinimiyle sınıflanır. | `crates/sqliters-core/src/lib.rs:9042` |
| `main.c:1518–1557` | Bütün Btree’ler ve rollback | Mevcut rollback_all_databases kullanılır; blob/cursor ve vtab geri alma yolu çalışır. Schema değişimi varsa mevcut reset/yeniden yükleme yolu kullanılır. | `crates/sqliters-core/src/lib.rs:2378` |
| `main.c:1550–1557` | Sayaçlar, defer bayrağı ve callback | Ertelenmiş FK izleri temizlenir. Önceki işlem veya Btree yazısı varsa rollback hook çağrılır; callback eski autocommit değerini görür. | `crates/sqliters-core/src/lib.rs:2395` |
| `main.c:1208–1217; vdbeaux.c:3397–3401` | Savepoint kapatma ve autoCommit | Savepoint listesi ve açık işlem bayrakları kapanır; autocommit yayınlanır. Okuyucu hatasında işlem açık bırakılmaz. | `crates/sqliters-core/src/lib.rs:2400` |
| `vdbeaux.c:3496–3503` | changeCntOn kapalı okuyucu | Bağlantının changes/total_changes/last_insert_rowid değerleri geri sarılmaz. Native STATE çıktısı ve API testiyle karşılaştırıldı. | `crates/sqliters-core/src/lib.rs:2378` |
| `vdbeapi.c:990–1023` | Başarısız otomatik reprepare | Program yürütülmeden hazırlama I/O hatası gelirse runtime rollback uygulanmaz; BEGIN/SAVEPOINT açık kalır. Aşama ayrı izlenir. | `crates/sqliters-core/src/lib.rs:9314` |
| `vdbeaux.c:3370–3393` | Diğer özel hatalar | Yeni yardımcı yalnız IOERR içindir. Readonly INTERRUPT / NOMEM / FULL kuralları bu dilimde genellenmedi; kapsam ve alınmayan dallar ayrı gösterilir. | `crates/sqliters-core/src/lib.rs:9044` |

## Native C kanıtı

Önceki D119 ham kaydı res/page1_error_before.json: shrink_memory prepare sonrasında sayfa1’in 100..4095 byte aralığında IOERR_READ olduğunda C 10/266, Rust başarılı 0. Pager düzeltmesinden sonra iki anahtarda C/R 10/266; bütün .db baytları aynı (res/page1_error_after.json).

İlk ara 56 akışta 16 BEGIN/SAVEPOINT durumunda Rust hata kodunu doğru veriyor fakat işlemi açık bırakıyordu. Bu ara tam JSON, yeniden çalıştırmada üzerine yazıldığı için korunamadı. res/tx_failure_excerpt.json araç çıktısından alınmış açık etiketli özettir; tam eski ham dosya olarak sunulmaz. Son tam matrisler saklandı; sonraki koşularda geçmiş JSON otomatik arşivlenir.

Son 56 sayfa1 akışı: iki anahtar × rollback/WAL × normal/exclusive × yedi bağlam (autocommit, deferred, hazır read snapshot, immediate, savepoint, canlı imleç, hata sonrası retry). Sonuç/hata, AUTOCOMMIT ve bütün .db baytları C ile aynı. Snapshot ve WAL modu korunması 15 pager testi içinde de sınandı.

Ayrı 16 reprepare akışı iki anahtarda BEGIN/SAVEPOINT, normal/exclusive ve rollback/WAL durumlarını kapsar. C başarısız hazırlamada işlemi açık bırakır. İlk geniş rollback kuralının bu ayrımı bozduğu res/reprepare_before_phase_fix.json içinde tam kayıtlıdır. Son aşama ayrımı ile 16/16 eşleşti; failed-prepare ve runtime-error API testleri de geçti.

64 hook/sayaç akışında hata ve autocommit’e ek olarak rollback hook sayısı, hook içindeki autocommit, changes(), total_changes() ve last_insert_rowid() C ile aynı. Callback autocommit açılmadan çağrılır; okuma hatası önceki değişiklik sayaçlarını sıfırlamaz. res/hook_matrix.json bütün ham çıktıları saklar.

C ve Rust ham VFS izleri tam eşit sayılmadı: 100 byte başlık ön okuması, TEMP cache ömrü ve journal okuma/silme zamanlamaları açık. 56/16/64 sayıları sonuç/durum/sayaç ve .db eşliğidir; VFS işlemlerinin tümünün aynı olduğu iddiası değildir.

Tam program 13181 aynı; 126 Unsupported başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 15632 aynı; akış 11459/11459 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103. Bütün 4158 akışın .db baytları aynı. Önceki 896 sync, 160 bağlantı/cache, 16 cold-cache, 12 yeniden açma programı, 18 iki-bağlantı, 6 stmt-status akışı ve 30 D117 hata senaryosu da eşleşti.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 870 / 1142 (76.2%) | 577 / 794 (72.7%) |
| btree.c | 206 / 229 (90.0%) | 98 / 126 (77.8%) |
| pager.c | 216 / 259 (83.4%) | 147 / 222 (66.2%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 75 / 77 (97.4%) | 35 / 40 (87.5%) |
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
| **total** | 1912 / 2394 (79.9%) | 1140 / 1588 (71.8%) |

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
| pagerPlaybackSavepoint | pager.c | 37 | 67.6 | 44 | 45.5 | 13 |
| sqlite3PagerRollback | pager.c | 18 | 100.0 | 16 | 75.0 | 4 |
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
| readDbPage | pager.c | 19 | 100.0 | 12 | 91.7 | 1 |
| pager_reset | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3RollbackAll | main.c | 23 | 100.0 | 18 | 94.4 | 1 |
| sqlite3CloseSavepoints | main.c | 9 | 100.0 | 2 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 89494 | 2/2 |
| parse.y:524 | 44996 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

## V6b.18 — özel hata ve yeniden hazırlama blokları

Bu iki bölge tüm sqlite3VdbeHalt/sqlite3_step yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |

497 alınmayan dal [karar_esligi_v6b_18_dallar.md](karar_esligi_v6b_18_dallar.md) içinde ayrı sınıflandırıldı. sqlite3VdbeHalt/sqlite3_step için yalnız tablodaki satır aralıkları sayıldı; bütün yordamlar sayılmadı. Diğer NOMEM/FULL/INTERRUPT, savedPc>=0 reprepare ve init.busy yönleri açık.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1110, 0, 337), açık (1095, 15, 337). 15 başarısızın adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; başarısız sınıf/seed kümeleri aynı. Beklentiler değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU 6/tmpfs; yedi A/B; anahtar kapalı. Bütün sonuç özetleri aynı. Performans ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.43      9.41     0.998x     9.37     9.35     9.60    0.98x  OK
W02_create_indexes                5.88      5.86     0.997x     5.83     5.81     8.76    0.67x  OK
W03_pk_point_select              30.41     30.92     1.017x    29.44    30.54    23.30    1.33x  OK
W04_secondary_idx_range           9.58     10.05     1.049x     9.45     9.78     7.46    1.35x  OK
W05_full_scan_filter_agg          4.42      4.50     1.019x     4.39     4.48    16.57    0.27x  OK
W06_join_group_order             10.81     11.32     1.047x    10.78    11.30    28.23    0.40x  OK
W07_window_functions             37.95     38.83     1.023x    37.56    38.59    50.21    0.77x  OK
W08_indexed_update_delete        12.40     12.13     0.978x    12.33    11.91     7.96    1.52x  OK
W09_like_text_scan               12.43     12.70     1.022x    12.41    12.59    18.92    0.67x  OK
W10_jsonb_extract_agg            13.86     14.03     1.012x    13.73    13.97    10.93    1.28x  OK
W11_fts5_index_and_bm25          69.52     70.49     1.014x    68.67    69.93    57.02    1.24x  OK
W12_rtree_insert_and_bbox        40.70     41.87     1.029x    40.01    41.46    24.63    1.70x  OK
W13_autocommit_wal_txns           4.98      5.04     1.013x     4.87     4.98     4.12    1.23x  OK
W14_cli_sql_script               39.72     54.08     1.362x    39.27    53.72    26.19    2.07x  OK
TOTAL                           302.09    321.23     1.063x   geomean cand/base = 1.038x   digest mismatches = 0
```

## Kalan iş ve kayıt

Sıradaki adım D118: kısa okuma ve sayfa1 başlık yüklemesi. Yeni üst katman rollback yardımı ortak step_in_session okuyucularını kapsar; bütün özel pubstep hızlı yolları veya diğer özel hata türleri tamamlandı sayılmaz. Eski yürütücü/hızlı yol denetimi, E28 COMMIT/pager ERROR, M4, kalan PRAGMA, D103, genel UTF-16/geçersiz UTF-8 ve diğer C farkları açık. Yayın noktası ve tam C eşliği henüz sağlanmadı.

Kanıt `/home/hakan/github/sqliters/target/perf-work/v6b18/`; res/source_manifest.json pager, core ve API test dosyasını sabitler. C kaynak ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.
