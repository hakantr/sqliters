# V6b.19 — D118 kısa okuma ve hata metinleri

C SQLite 3.54.1; Rust 1.100. Veritabanı başlığının SHORT_READ hatası artık doğrudan G/Ç hatasına dönüşmüyor; C gibi sıfırla doldurulan içerik doğrulanıyor. WAL kısa okuma hatası korunuyor. Kısmi veriyle genişletilen deneylerde 640 akışın 624’ü eşleşti; 16 kusur açık. D118 bütünüyle kapanmış sayılmadı.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pager.c:3951–3969` | Açılışta dosya başlığı, SHORT_READ | Başlık sıfırla başlar; SHORT_READ başarı sayılır. Diğer G/Ç hatası korunur. | `crates/sqliters-pager/src/lib.rs:2713` |
| `pager.c:3092–3098` | Veritabanı sayfası kısa okunur | Okunan önek korunur; okunmayan kuyruk VFS sözleşmesi uyarınca sıfırdır. Rust Ok(n) adaptörü de kuyruğu sıfırlar. | `crates/sqliters-pager/src/lib.rs:1631` |
| `pager.c:3089–3091` | Sayfa WAL çerçevesinden gelir | WAL çerçevesindeki SHORT_READ normalleştirilmez; 10/522 hata ve önceki D119 rollback kararı korunur. | `crates/sqliters-pager/src/lib.rs:1599` |
| `btree.c:3339–3346; main.c:1694,1713` | Sihirli önek veya başlık geçersiz | C SQLITE_NOTADB kodunu genel file is not a database metnine dönüştürür. Rust başlık ayrıştırıcısının ayrıntılı farklı metinleri kaldırıldı. | `crates/sqliters-pager/src/header.rs:184` |
| `btree.c:3388–3410; main.c:1713` | Payload kesirleri veya sayfa boyutu geçersiz | Başlık kısa okuma sonrası bu koşulları da geçmelidir. C genel NOTADB metni kullanır; kod/parsing kapsamı bütünüyle yeniden yazılmadı. | `crates/sqliters-pager/src/header.rs:198` |
| `btree.c:2055–2114; main.c:1694` | Btree sayfasının tür baytı geçersiz | Dört geçerli sayfa türü dışındaki bayt CORRUPT üretir. Rust artık aynı genel database disk image is malformed metnini verir. | `crates/sqliters-btree/src/page.rs:25` |
| `pager.c:5469–5483` | Sıcak normal okuyucu sürüm yoklaması | 16 byte sürüm yoklamasındaki SHORT_READ bütün sürüm imgesini sıfırlar. Başlık/sayfa okumasının önek koruma kuralıyla karıştırılmadı. | `crates/sqliters-pager/src/lib.rs:2916` |

Btree tür ayrımı: C btree.c:2062–2110 koşulları Rust from_byte içindeki 0x0d TableLeaf, 0x0a IndexLeaf, 0x02 IndexInterior ve 0x05 TableInterior eşlemelerine karşılık gelir. is_leaf/header_size yöntemleri yaprak ve iç sayfanın başlık ayrımını sürdürür. Bu dilimde yalnız geçersiz tür hata metni değiştirildi; diğer alanların genel eşliği yeniden tamamlanmış sayılmadı.

## Native C ve dosya kanıtı

Önceki D118 örneği res/short_before.json: C 26/26 file is not a database, Rust 10/522 disk I/O error. Aynı özgün girdinin son karşılaştırması res/short_original_after.json içinde: iki anahtarda C/Rust 26/26 ve bütün dosya baytları aynı. Yeni matrisin düzeltme öncesi 640 ham kaydı res/short_matrix_before.json içinde; 200 çıktı/durum farkı vardı. Ara normalleştirme ve hata metni sonuçları da ayrı adlarla saklandı. Tam önceki ham kayıtlar korunmuştur.

Matris: iki anahtar × rollback/WAL × normal/exclusive × on korunan önek uzunluğu (0,8,16,20,24,40,100,108,512,4096) × sekiz bağlam (autocommit, deferred, açık read transaction, immediate, savepoint, canlı imleç, retry, reprepare). VFS aynı dosyadan öneki okuyup kuyruğu sıfırlar ve SHORT_READ döndürür. Ham C/Rust READ/SYNC/DELETE çıktıları tutulur. Karşılaştırılan alanlar hata ve sonuç, autocommit, rollback hook sayısı/hook içindeki autocommit ve changes/total_changes/last_insert_rowid. Bütün 640 akışın .db baytları aynı.

624/640 sonuç/durum/sayaç eşliği sağlandı; eski 200 farkın 184’ü giderildi. Kalan 16 kayıt res/short_matrix_bugs.json içinde başarısız olarak tutulur; başarı sayılmaz. Denetim bu kayıtların tam girdi kümesini de doğrular:

- **D120 — 8 akış:** exclusive retry, önek 0/8/16/20, iki anahtar. C bozuk sayfa1’i önbellekte tutar ve hata sürer; Rust yeniden okuyup başarılı olur. Sonraki öncelik bu önbellek/sayfa1 yükleme ayrımıdır.
- **D121 — 4 akış:** önek 108, reprepare, iki kilit ve iki anahtar. İki motor da CORRUPT verir; Rust invalid cell offset 0 on page 1 metni C’nin genel bozuk görüntü metninden farklıdır.
- **D122 — 4 akış:** önek 512, reprepare, iki kilit ve iki anahtar. C 11/11 malformed database schema (?) verir; Rust 1/1 no such table: t verir. Şema satırının bozuk kayıt denetimi açık.

İki yeni pager testiyle veritabanı kısmi okuması, Ok(n) adaptörü, geçersiz başlıktan sonra yeniden deneme ve WAL çerçevesinde SHORT_READ’in korunması sınandı. Son 17 pager testi geçti. İlk WAL test kurulumu WAL çerçevesi oluşturmuyordu; res/pager_messages.txt başarısız deneyi saklar. Son test WAL içinde gerçek sayfa1 yazımı yapar; res/pager_final.txt başarılıdır.

Normal başlık okuması hâlâ ayrıca 100 byte ön okuma yapar. C’nin bütün ham VFS çağrılarıyla eşlik iddia edilmez; TEMP ve journal zamanlama gözlemleri de açık. Genel başlık kabul kuralları, kodlama bozulmaları ve diğer CORRUPT metinleri tamamlandı sayılmadı.

Tam program 13181 aynı; 126 Unsupported başarı sayılmadı. EXPLAIN/EQP ayrı ayrı 15632 aynı; akış 11459 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103. Bütün API .db baytları aynı. Önceki 896 sync, 160 bağlantı, 16 cold-cache, 12 reopen programı, 18 iki-bağlantı, 6 stmt-status, 30 D117, 56 D119 yürütme, 16 reprepare ve 64 callback/sayaç akışı da eşleşti.

Native seri ilk aktarımda eksik schema_intent_queries.txt girdisi nedeniyle durdu. Girdi tamamlandı; locks_matrix ve sonraki bütün denetimler başarıyla yeniden çalıştı. Önce tamamlanan sonuçlar korundu. İlk hata ve devam günlüğü res/io_final_before_query_fixture_copy.txt ile res/io_resume.txt içinde saklıdır.

## C kapsamı

Kapsam birikimlidir: üreticinin kabul ettiği SQL, şema kurulumları, önceki native senaryolar ve yeni kısa okuma matrisi aynı gcov sayacında birleşir. sqlite3VdbeHalt/sqlite3_step ve yürütme kodları yalnız belirtilen bloklarda sayılır; bütün yordamlar kapsandı sayılmaz. Yeni tam işlev kapsamları sqlite3PagerReadFileheader, decodeFlags ve sqlite3ErrStr’dir.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Derlem: üreticinin kabul ettiği 15.757 ifade EXPLAIN ve EXPLAIN QUERY PLAN olarak, ayrıca şema kurulumları ve native akışlarla çalıştırıldı.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 870 / 1142 (76.2%) | 577 / 794 (72.7%) |
| btree.c | 249 / 276 (90.2%) | 109 / 136 (80.1%) |
| pager.c | 224 / 267 (83.9%) | 151 / 226 (66.8%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 84 / 95 (88.4%) | 40 / 50 (80.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 143 / 184 (77.7%) | 87 / 122 (71.3%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| **total** | 1972 / 2467 (79.9%) | 1161 / 1612 (72.0%) |

| C işlevi | Dosya | Satır | Satır % | Dal | Alınan % | Alınmayan dal içeren satırlar |
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
| lockBtree | btree.c | 73 | 75.3 | 44 | 70.5 | 12 |
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
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 79.3 | 9 |
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
| sqlite3RollbackAll | main.c | 23 | 100.0 | 18 | 100.0 | 0 |
| sqlite3CloseSavepoints | main.c | 9 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerReadFileheader | pager.c | 8 | 100.0 | 4 | 100.0 | 0 |
| decodeFlags | btree.c | 47 | 89.4 | 10 | 90.0 | 1 |
| sqlite3ErrStr | main.c | 18 | 50.0 | 10 | 40.0 | 2 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 92462 | 2/2 |
| parse.y:524 | 46272 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen dört işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_Init | 2/20 |

## V6b.19 — özel hata ve yeniden hazırlama blokları

Bu iki bölge tüm sqlite3VdbeHalt/sqlite3_step yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |


500 alınmayan dal [karar_esligi_v6b_19_dallar.md](karar_esligi_v6b_19_dallar.md) içinde tek tek sınıflandırıldı. Kapsanmayan her yön açık kanıt eksiğidir.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): kapalı (1112, 0, 337), açık (1097, 15, 337). Açık anahtardaki 15 başarısızın adları önceki adımla aynı. DML kapalı 378/400, açık 398/400; sınıf/seed kümeleri öncekiyle aynı. Beklentiler değiştirilmedi. Yeni kısa okuma matrisinin 16 kusuru ayrıca yukarıda listelendi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanına karşı hız: CPU6/tmpfs, yedi A/B, anahtar kapalı. Performans ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.22      8.90     0.965x     9.18     8.84     9.60    0.93x  OK
W02_create_indexes                5.65      5.63     0.996x     5.62     5.60     8.76    0.64x  OK
W03_pk_point_select              29.80     30.97     1.039x    29.51    30.21    23.30    1.33x  OK
W04_secondary_idx_range           9.48      9.91     1.046x     9.37     9.72     7.46    1.33x  OK
W05_full_scan_filter_agg          4.40      4.32     0.981x     4.39     4.31    16.57    0.26x  OK
W06_join_group_order             10.80     11.10     1.027x    10.78    11.05    28.23    0.39x  OK
W07_window_functions             37.00     38.06     1.029x    36.97    37.99    50.21    0.76x  OK
W08_indexed_update_delete        11.95     11.93     0.998x    11.92    11.72     7.96    1.50x  OK
W09_like_text_scan               12.58     12.80     1.018x    12.41    12.55    18.92    0.68x  OK
W10_jsonb_extract_agg            13.73     13.80     1.005x    13.65    13.62    10.93    1.26x  OK
W11_fts5_index_and_bm25          68.99     71.10     1.031x    68.57    69.70    57.02    1.25x  OK
W12_rtree_insert_and_bbox        40.14     41.02     1.022x    39.94    40.46    24.63    1.67x  OK
W13_autocommit_wal_txns           4.50      4.54     1.010x     4.35     4.44     4.12    1.10x  OK
W14_cli_sql_script               38.70     53.35     1.379x    38.30    52.94    26.19    2.04x  OK
TOTAL                           296.94    317.43     1.069x   geomean cand/base = 1.035x   digest mismatches = 0
```

## Sonraki adım ve kayıt

D120 sayfa1 yükleme/önbellek farkı, ardından D121/D122 bozuk kayıt ve şema hataları. E28, M4, kalan PRAGMA, D103, genel UTF-16/geçersiz UTF-8, eski yürütücü ve hızlı yolların denetimi açık. Yayın noktası ve tam C eşliği sağlanmadı.

Kanıt `/home/hakan/github/sqliters/target/perf-work/v6b19/`; res/source_manifest.json üç kaynak dosyasını sabitler. C kaynakları değiştirilmedi; ileriPlanlama dikkate alınmadı.
