# V6b.23 — D128: eksik karşılaştırıcı hazırlama kararı ve hata kodu

C SQLite 3.54.1; Rust 1.100. Kaydedilen D128 farkları giderildi: gerekli karşılaştırıcı bulunmayınca hazırlamada 257 kodu korunur; C’nin hiç kullanmadığı karşılaştırıcı yüzünden çalışma anında hata üretilmez. Genel collation-needed/destructor/synthCollSeq, legacy V1 hata maskeleme ve bütün yürütücü yolları tamamlandı sayılmaz.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `callback.c:214–231` | Karşılaştırıcı bulunamadı | C hata metninden sonra Parse.rc=SQLITE_ERROR_MISSING_COLLSEQ (257) kurar. Üretici sonucu artık rc’yi message/offset ile birlikte taşır. | `crates/sqliters-codegen/src/lib.rs:145` |
| `prepare.c:822–835` | Hazırlama hatasının yayımlanması | EXPLAIN, PRAGMA, okuma/yazma üretici yolları SqliteError::with_extended kullanır. Birincil kod, genişletilmiş kod ve hata konumu korunur. | `crates/sqliters-core/src/lib.rs:5374` |
| `callback.c:229–232; expr.c:269–285` | Karşılaştırıcıya ihtiyaç duyulan SELECT | C’nin karşı-olgusal üreticisi hata verirse gerçek bağlantı durumunda yeniden derlenir. Yalnız gerçek C 257 hatası normal/reprepare hattında iki anahtarda da hazırlama hatası olur; başka üretici hataları bu yardımcıyla topluca zorlanmaz. | `crates/sqliters-core/src/lib.rs:5365` |
| `expr.c:1164–1180` | AND’nin sabit yanlış tarafı | ON/HAS_FUNC/rename korumaları altında C sağ karşılaştırmayı atabilir. Önceden taşınan sqlite3_expr_and bu kararı zaten verir; yürütme aynı programı kullanır. | `crates/sqliters-codegen/src/expr.rs:582` |
| `expr.c:2397–2408` | AND/OR sabit sadeleştirme | 1 OR karşılaştırma ve benzeri ifadelerde karşılaştırıcıya ihtiyaç kalmayabilir. Kullanılmayan COLLATE yüzünden hata verilmez. | `crates/sqliters-codegen/src/expr.rs:1110` |
| `vdbe.c:1428–1433,1801–1836; 9224–9293` | Tamsayı sonuç ve boş yol | Gerçek C programı sıfır Btree maskeli, EXPLAIN dışı ve yalnız acyclic Init/Goto/Integer/ResultRow/Halt(OK) yolundaysa makineden yürütülür. Metin, cursor, function, error-halt veya cycle bu dar yola alınmaz. | `crates/sqliters-core/src/vdbe_host.rs:207` |
| `vdbeapi.c:882–886; vdbe.c:1834–1836` | Sonuç, PC ve sayaçlar | Eski yürütücünün gereksiz karşılaştırıcı denetimi atlanır; VM_STEP/RUN/REPREPARE dahil gerçek sayaçlar korunur. İki anahtar ve UTF-16 dahil bu yol aynı C programını kullanır. | `crates/sqliters-core/src/lib.rs:6076` |
| `expr.c:1336–1395; vdbeapi.c:2040–2044; vdbeaux.c:2684,2747–2754` | Parametre metadata | C’nin sadeleştirdiği ama numarasını koruduğu ?1 bağlanabilir kalır. Makine seçiminde nVar legacy metadata ile eşit olmalıdır; parametreyi değerlendiren opcode bu tamsayı yolunda kabul edilmez. | `crates/sqliters-core/src/lib.rs:8015` |

## Önce/sonra ve native API kanıtı

Önceki 12494d3 ikilisi res/collation_before.json içinde 438 akışın 12’sinde farklıydı. SELECT 1 ORDER BY 1 COLLATE missing için C hazırlamada 257 verir; Rust açık UTF-8 yolda 1 verir, diğer örneklerde hazırlamayı kabul ederdi. İlk hata-kodu/hazırlama düzeltmesiyle 438/438 sonuç, hata, durum ve bütün .db baytları eşleşti (res/collation_after_initial.json). C referanslarının tamamı öncekiyle değişmeden kaldı; final collation_matrix aynı 438 gözlemi yeniden doğrular.

Genişletilmiş ilk **564 API akışı** 90 farklı gözlem gösterdi: 30’u yalnız eski yürütücünün sıfır sayaçlarıydı, 60’ı C’nin sildiği karşılaştırmanın Rust’ta çalışma anında denetlenmesiydi. İlk JSON dosyasının üzerine final canonical sonuç yazıldı; ilk sayı/sınıf özeti ve ilk 12 farkın ham çıktısı res/missing_initial.txt içinde kaldı. Bu dosya 90 satırın tam JSON kanıtı olarak sunulmaz. Tamsayı/boş C programı için dar yürütme yolu eklendikten sonra 564/564 eşleşti; bu ara sürümün tam sonucu res/missing_pruned.json içinde korunur.

Final **636 akış**, iki anahtar × üç kodlama × 18 sorgu × cold/warm/deferred/savepoint/reprepare/restore bağlamlarıdır; cold durumda olmayan tabloyu kullanan sorgular çıkarılır. Normal ve EXPLAIN/EQP hazırlama, DISTINCT, min/max, bileşik SELECT, kaynak tablosu, Unicode ad, geçerli kayıt sonrası silme, hatadan sonra kaydı geri getirme ve kullanılmayan ?1 kapsanır. Hata kodu/metni/konumu, kolon bilgisi, satırlar, autocommit/değişiklik bilgileri, sekiz stmt_status sayacı (REPREPARE dahil), stderr ve bütün .db baytları aynı (res/missing_matrix.json; missing_matrix_bugs.json boştur).

REPREPARE sayacı eklenince C kapsam sürücüsünün eski gcda checksum’ı bir kez stderr tanısı üretti; sonuç ve dosya farkı yoktu. Bu ham kayıt res/missing_with_reprepare_counter_bugs.json içinde saklandı. Güncel checksum ile tam tekrar res/missing_counter_retry.json içinde 636/636 eşleşti; tanı normalize edilmedi veya görmezden gelinmedi. Final kapı/kapsam ayrı temiz profil hedefini kullanır.

Unit testi, kullanılmayan sonuç COLLATE’inin kabulünü, 0 AND / 1 OR içinde atılan karşılaştırmayı, ?1 bağlamasını, zorunlu ORDER BY hatasını ve kayıt silinince yeniden hazırlama hatasını üç kodlamada denetler. Üretici testleri ve cgexplain örneğinde yalnız yeni Error.code alanını dışarıda bırakan pattern güncellemeleri vardır; mevcut beklenen sonuçlar değiştirilmedi.

Genel program derlemi 13344 aynı; 104 Unsupported başarı sayılmadı. EXPLAIN 15834, EQP 15834, yürütme 11639 aynı. Ek 60 hazırlama hatası üretici trace’inden önce döner ve C ile aynı karşılaştırılır; kabul edilen program sayısına katılmadı. 39 kayıt geçmişindeki 1362 program da yeniden eşleşti.

Önceki API 4157/4158 aynı; tek çıktı farkı D103 ve bütün dosya baytları aynı. 438 collation, 36 kabuk/indeks dosyası, 7536 karşılaştırıcı, altı bozuk dosya/no-schema ve önceki IOERR/SHORT_READ/cache/WAL deneyleri tekrar doğrulandı. 72 ham G/Ç izinin 52’si aynı; kalan 20 dirty/TEMP farkı açıktır.

## C kapsamı

Birikimli kapsama sqlite3GetCollSeq, sqlite3ExprAnd, sqlite3ExprSimplifiedAndOr ve OP_ResultRow eklendi. Expr/collation kaynak konumları kendi işlev gövdelerinde eşleştirildi; tekrar eden kaynak satırlarının yanlış işleve bağlanması önlendi. VDBE bütünü sayılmaz.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 15959 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 887 / 1142 (77.7%) | 587 / 794 (73.9%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 319 / 380 (83.9%) | 211 / 306 (69.0%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 109 / 123 (88.6%) | 57 / 74 (77.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 69 / 74 (93.2%) | 32 / 38 (84.2%) |
| prepare.c | 198 / 259 (76.4%) | 122 / 192 (63.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 18 / 21 (85.7%) | 14 / 24 (58.3%) |
| utf.c | 51 / 64 (79.7%) | 62 / 74 (83.8%) |
| hash.c | 74 / 74 (100.0%) | 42 / 44 (95.5%) |
| expr.c | 19 / 22 (86.4%) | 15 / 24 (62.5%) |
| **total** | 2372 / 2914 (81.4%) | 1444 / 1984 (72.8%) |

| C işlevi | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 74.2 | 675 | 71.3 | 114 |
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
| lockBtree | btree.c | 73 | 75.3 | 44 | 72.7 | 11 |
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
| sqlite3InitOne | prepare.c | 92 | 89.1 | 58 | 81.0 | 8 |
| getLockingMode | pragma.c | 5 | 100.0 | 6 | 100.0 | 0 |
| returnSingleText | pragma.c | 5 | 100.0 | 2 | 50.0 | 1 |
| sqlite3PagerLockingMode | pager.c | 4 | 100.0 | 6 | 83.3 | 1 |
| pragmaVtabConnect | pragma.c | 34 | 88.2 | 12 | 75.0 | 3 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 78.6 | 3 |
| pragmaVtabFilter | pragma.c | 26 | 84.6 | 14 | 71.4 | 4 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 50.0 | 2 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 87.5 | 3 |
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
| sqlite3PagerSharedLock | pager.c | 60 | 71.7 | 58 | 67.2 | 13 |
| readDbPage | pager.c | 19 | 100.0 | 12 | 91.7 | 1 |
| pager_reset | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3RollbackAll | main.c | 23 | 100.0 | 18 | 100.0 | 0 |
| sqlite3CloseSavepoints | main.c | 9 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerReadFileheader | pager.c | 8 | 100.0 | 4 | 100.0 | 0 |
| decodeFlags | btree.c | 47 | 89.4 | 10 | 90.0 | 1 |
| sqlite3ErrStr | main.c | 18 | 50.0 | 10 | 40.0 | 2 |
| pagerPagecount | pager.c | 12 | 91.7 | 8 | 75.0 | 2 |
| sqlite3PagerPagecount | pager.c | 3 | 100.0 | 0 | 100.0 | 0 |
| getPageNormal | pager.c | 45 | 77.8 | 32 | 65.6 | 10 |
| sqlite3PagerUnrefPageOne | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| pagerUnlockIfUnused | pager.c | 4 | 100.0 | 2 | 100.0 | 0 |
| pagerUnlockAndRollback | pager.c | 21 | 42.9 | 14 | 71.4 | 3 |
| sqlite3InitCallback | prepare.c | 57 | 70.2 | 56 | 48.2 | 20 |
| corruptSchema | prepare.c | 18 | 66.7 | 14 | 50.0 | 5 |
| sqlite3PagerCloseWal | pager.c | 19 | 63.2 | 18 | 27.8 | 6 |
| sqlite3BtreeGetFilename | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PagerFilename | pager.c | 4 | 100.0 | 6 | 66.7 | 1 |
| sqlite3ValueText | vdbemem.c | 7 | 100.0 | 8 | 100.0 | 0 |
| valueToText | vdbemem.c | 14 | 78.6 | 16 | 37.5 | 5 |
| sqlite3VdbeMemTranslate | utf.c | 64 | 79.7 | 74 | 83.8 | 8 |
| strHash | hash.c | 6 | 100.0 | 2 | 100.0 | 0 |
| insertElement | hash.c | 17 | 100.0 | 10 | 100.0 | 0 |
| rehash | hash.c | 16 | 100.0 | 8 | 87.5 | 1 |
| findElementWithHash | hash.c | 15 | 100.0 | 10 | 100.0 | 0 |
| sqlite3HashInsert | hash.c | 20 | 100.0 | 14 | 92.9 | 1 |
| findCollSeqEntry | callback.c | 20 | 85.0 | 8 | 75.0 | 2 |
| sqlite3FindCollSeq | callback.c | 6 | 100.0 | 4 | 100.0 | 0 |
| sqlite3LocateCollSeq | callback.c | 8 | 87.5 | 6 | 66.7 | 1 |
| createCollation | main.c | 28 | 89.3 | 24 | 70.8 | 5 |
| sqlite3GetCollSeq | callback.c | 14 | 100.0 | 14 | 92.9 | 1 |
| sqlite3ExprAnd | expr.c | 13 | 84.6 | 8 | 62.5 | 3 |
| sqlite3ExprSimplifiedAndOr | expr.c | 9 | 88.9 | 16 | 62.5 | 4 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 96324 | 2/2 |
| parse.y:524 | 48668 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen yedi işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_ResultRow | 2/4 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |

## V6b.23 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |
| vdbe.c:3136–3147 | 0/2 | 0/4 |
| vdbe.c:3210–3224 | 7/7 | 10/12 |
| vdbe.c:3393–3402 | 4/6 | 1/2 |


## Kabuk karşılaştırıcılarının C kapsamı

Aynı 3.54.1 kaynaklarının kopyası `-O0 --coverage`, aynı SQLite nesnesiyle bağlandı. Aşağıdaki yordamlar ölçülür; decimal SQL işlevleri ve tüm kabuk kapsamı sayılmaz. uint/decimal kayıt çağrıları C yardımcısında da çalıştırılır.

| C yordamı | Satır | Alınan dal |
|---|---|---|
| ext/misc/uint.c / uintCollFunc | 27/27 | 36/36 |
| ext/misc/decimal.c / decimalNewFromText | 87/87 | 79/86 |
| ext/misc/decimal.c / decimal_cmp | 23/23 | 20/20 |
| ext/misc/decimal.c / decimalCollFunc | 11/11 | 4/4 |
| ext/misc/decimal.c / decimal_free | 5/5 | 2/2 |
| ext/misc/decimal.c / decimal_clear | 3/3 | 0/0 |

7 alınmayan dal, ana dal tablosunda ayrı sınıflandırılır. Rust/C tahsis sayısı veya OOM davranışı kanıtlanmış sayılmadı.


**601 alınmayan dal** [karar_esligi_v6b_23_dallar.md](karar_esligi_v6b_23_dallar.md) içinde tek tek sınıflandırıldı. Genel callback/synthCollSeq, rename/ON/HAS_FUNC kısa devreleri, ROW trace ve NOMEM/tahsis izleri ayrıca kanıtlanmadı. Önceki yedi decimal tahsis/NULL dalı da korunur.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1116, 0, 337); açık (1101, 15, 337). C ikilisi PATH’teydi. Açık anahtardaki 15 başarısızın adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; sınıf/örnek kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs; yedi A/B; anahtar kapalı. Dar makine yolu yanlış doğrulama ve sayaç farkını gidermek için eklendi; performans ayarı yapılmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.20      8.75     0.952x     9.17     8.71     9.60    0.91x  OK
W02_create_indexes                5.70      5.67     0.995x     5.64     5.65     8.76    0.65x  OK
W03_pk_point_select              30.43     29.75     0.978x    29.64    29.52    23.30    1.28x  OK
W04_secondary_idx_range           9.57      9.82     1.026x     9.30     9.47     7.46    1.32x  OK
W05_full_scan_filter_agg          4.41      4.32     0.979x     4.39     4.30    16.57    0.26x  OK
W06_join_group_order             10.80     10.90     1.009x    10.79    10.87    28.23    0.39x  OK
W07_window_functions             37.14     37.31     1.005x    37.00    37.25    50.21    0.74x  OK
W08_indexed_update_delete        11.98     11.92     0.995x    11.92    11.82     7.96    1.50x  OK
W09_like_text_scan               12.62     12.65     1.002x    12.41    12.54    18.92    0.67x  OK
W10_jsonb_extract_agg            13.68     13.71     1.002x    13.62    13.56    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.22     70.10     1.013x    68.97    69.62    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.62     40.25     0.991x    39.48    39.92    24.63    1.63x  OK
W13_autocommit_wal_txns           4.53      4.64     1.026x     4.39     4.54     4.12    1.13x  OK
W14_cli_sql_script               38.77     52.88     1.364x    38.50    52.58    26.19    2.02x  OK
TOTAL                           298.68    312.67     1.047x   geomean cand/base = 1.020x   digest mismatches = 0
```

## Açık işler ve kayıt

V6b’nin kalan PRAGMA işleyicileri sürer. compile_options ilk doğrudan incelemede C’nin 42 seçeneği yerine Rust’a özgü üç satır veriyor; bu metadata yolu sıradaki adaydır. function_list/module_list, genel collation API ve legacy yürütme yolları, D103, E28, M4, UTF-16/geçersiz UTF-8 ve kalan C farkları açıktır. Tam C garantisi veya yayın tamamlanması iddiası yok.

Kanıt /home/hakan/github/sqliters/target/perf-work/v6b23/; sekiz kaynak/test dosyası res/source_manifest.json ile sabittir. C kaynakları ve ileriPlanlama değiştirilmedi/değerlendirilmedi.
