# V6b.26 — EXPLAIN yazıcı ömrü ve bekleyen işlem kapanışı

C SQLite 3.54.1 ve Rust 1.100. D131’in kaydedilen yazıcı sayımı, EOF/reset/finalize, değişiklik sayacı ve normal/BUSY/FK kapanışları eşitlendi; commit-hook hata metni D132 olarak açık. Genel EXPLAIN kesme/tahsis/callback davranışı veya E28 tamamlandı sayılmaz.

## Karar eşliği

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| insert.c:1011; delete.c:421; update.c:608 | nested==0 / nested!=0 | Yalnız üst DML derlemesinde changeCntOn ayarlanır. C sqlite3VdbeCountChanges vdbeaux.c:5268–5270 alan yazımı Program içinde korunur. | `crates/sqliters-codegen/src/insert.rs:390` |
| vdbeaux.c:887–918; vdbeapi.c:881–886 | readOnly / bIsReader / RUN başlangıcı | EXPLAIN, açıkladığı programın yazıcı ve okuyucu bitlerini kullanır. NoLock sınıfı olması yazıcı sayımını bastırmaz. | `crates/sqliters-core/src/lib.rs:9598` |
| vdbeaux.c:2318–2325,2378–2382; vdbeapi.c:924–931 | Sıradaki opcode var / EOF | EOF yalnız DONE döndürür; VDBE_RUN_STATE ve etkin lease korunur. Tekrar step listeyi başlatmaz. | `crates/sqliters-core/src/lib.rs:11336` |
| vdbeaux.c:2421–2462 | EXPLAIN / EQP ve sıfır satır | Boş EQP de RUN kalır; kayıt oluşturma sayısı sıfır olması Halt nedeni değildir. | `crates/sqliters-core/src/lib.rs:9681` |
| vdbeaux.c:3613,3698 | Reset/finalize: RUN / READY / HALT | Yalnız çalışmış EXPLAIN kapanışta işlem ve sayaç kararına girer. Hazırlanıp hiç step edilmemiş ifade yan etki yapmaz. | `crates/sqliters-core/src/lib.rs:2123` |
| vdbeaux.c:3359 | bIsReader true / false | Veri okumayan EXPLAIN SELECT 1 işlem bitirmez; gerçek tabloya başvuran programın kapanışı ortak Halt işlem kararını izler. | `crates/sqliters-core/src/lib.rs:2130` |
| vdbeaux.c:3416–3419 | autocommit / diğer yazıcı sayısı | Başka EXPLAIN yazıcısı varken DML commit etmez; son yazıcı kapanınca bekleyen işlem tamamlanır. Açık BEGIN ve SAVEPOINT işlem sonu değildir. | `crates/sqliters-core/src/vdbe_host.rs:1166` |
| vdbeaux.c:3421–3427 | Ertelenmiş FK yok / hata | Bekleyen FK ihlali EXPLAIN step EOF anında değil reset/finalize sırasında 787 döndürür ve işlemi geri alır. total_changes önce tamamlanmış DML katkısını korur. | `crates/sqliters-core/src/vdbe_host.rs:1172` |
| vdbeaux.c:3436–3449 | Commit başarılı / BUSY / hook reddi | Yazıcı commit BUSY durumunda C gibi rollback yapılır. Commit hook reddi 531 ve rollback aynı; hata metni D132 olarak açık. Başarılı commit bayrak ve FK sayaçlarını temizler. | `crates/sqliters-core/src/vdbe_host.rs:1190` |
| vdbeaux.c:3496–3504 | changeCntOn açık / kapalı | EXPLAIN DML hiçbir opcode çalıştırmadığından nChange=0 ile changes güncellenir; total_changes sıfır eklenerek korunur. Doğrudan API aynası da yayımlanır. | `crates/sqliters-core/src/lib.rs:2135` |
| vdbeaux.c:3510–3516 | Son kapanış | Lease işlem callbacklerinden sonra kaldırılır; stmt_busy false olur. Row listesinin tükenmesi bu aşama değildir. | `crates/sqliters-core/src/lib.rs:2143` |

## Doğrudan C karşılaştırması

İlk 72 akışın tamamı önce farklıydı; düzeltmeden sonra 72/72 aynı. Otomatik reset yapmayan sürücüyle 408 ayrı akışın 396’sı önce farklıydı; sonra 408/408 aynı. Üç kodlama, iki anahtar, bellek/disk, EXPLAIN INSERT/UPDATE/DELETE/SELECT/PRAGMA, boş EQP, tekrar step, iki açık yazıcı, hiç step edilmemiş ifade, açık işlem, FK çözme/geri alma ve reset/finalize hata kodları kapsandı. Önceki altı D131 örneği de aynı.

Başka C bağlantısının açık okuma kilidi ve commit hook kabul/reddiyle 192 kapanış akışının 120’si tam aynı. Kalan 72 örnekte yalnız 531 hata metni farklı (D132): C constraint failed, Rust commit hook vetoed commit. Bütün sayaçlar, veriler ve ana dosya baytları aynı; bu 72 başarı sayılmadı. Hem ilk satırda hem EOF sonrasında kapanış, DELETE/WAL journal ve iki anahtar denendi. Ana dosyaların bütün baytları SHA-256 ile aynı; bu ölçüm journal/SHM anlık baytlarının eşliği iddiası değildir.

Özel sürücü T raw step, H sonucu denetlenen reset, F sonucu denetlenen finalize ve Z stmt_busy gözlemi ekler; SQL çıktısı normalize edilmez. İlk yardımcı taslağında Q zaten sorgu için ayrılmıştı; H ile değiştirilip bütün başlangıç ölçümü yeniden alındı. İlk raw fixture bazı akışlara boş son satır ekliyordu: C sürücüsü o satırda eski SQL alanını yeniden okuyordu. Girdi tek satır sonuna düzeltilip eski ikiliyle raw_before_valid.json ve yeni ikiliyle raw_after_valid.json yeniden üretildi. İlk sonuçlar saklandı, başarı kanıtı sayılmadı. Ek commit hook sürücüsünde K mevcut collation komutuyla çakışmıştı; J ile ayrılıp yeniden ölçüldü. İlk 192/192 iddiası geçersizdir: gerçek hook derlemi 120/192 tam aynı, 72 D132. Kilit ve hook birlikteyken C önce BUSY döndürür; fixture beklentisi bu kaynak/gözlem sırasına düzeltildi. EXPLAIN içermeyen 36 doğrudan INSERT/COMMIT/RELEASE akışının 24’ü aynı; 12 otomatik INSERT kapanışında da aynı eski D132 hata metni farkı bulundu.

Program’a change_cnt_on eklendi. Tarihsel 14 alanlı PROG dökümü bu alanı serileştirmez; yükleyici varsayılan false bırakır ve EXPLAIN enjeksiyonu için kullanılmaz. Mevcut tam program karşılaştırmaları opcode/operandlar ve eski protokoldeki alanları karşılaştırır; yeni alanın kanıtı C kaynak koşulu ve gözlenen reset/finalize sayaçlarıdır.

Genel derlem: 13414 program aynı, 82 desteklenmiyor. EXPLAIN 15909, EQP 15909, yürütme 11692 aynı; üreticiden önceki 64 hazırlama hatası ayrıca aynı. Önceki compile-options 3990, collation 438, D128 636 ve kabuk 36 akış korundu. Defer matrisi 1624/1644 tam aynı; 20 eski D52 sayaç farkı ham olarak korunur ve kusur sayılır. Bütün 1644 dosya eşit. Manual API 4157/4158 aynı; tek eski D103 çıktı farkı ve bütün dosyaların eşliği ayrı kayıtlı.

## C kapsamı

Halt ölçüm dilimi 3330–3516’ya genişletildi. NextOpcode, List ve CountChanges; üç DML üreticisindeki yalnız nested==0 satırları ve Reset’te yalnız RUN→Halt koşulu eklendi. Tüm Reset veya tüm Insert/Update/Delete işlevi ölçülmüş sayılmaz. Kurulum ifadelerinin aldığı C dalları da kapsamda sayılır. İlk kapsam devamı, shell karşılaştırıcılarının gcda girdileri henüz üretilmediği için durdu. 7536 bağımsız uint/decimal çifti yeni Rust 1.100/edition2024 yardımcı ikilisiyle yeniden çalıştırılıp kapsam ve dal listesi yeniden üretildi. Ürün kodu bu yardımcı düzeltmeleri boyunca değişmedi. Hiç alınmayan her gcov kısa devre çıkışı ayrı sınıflandırılır; alınan dal tek başına Rust eşliği kanıtı değildir.

# C-side coverage of the functions ported or completed in step V6b.26 (gcov, -O0 build of the reference amalgamation)

Corpus: the 16034 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 900 / 1142 (78.8%) | 593 / 794 (74.7%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 324 / 380 (85.3%) | 219 / 306 (71.6%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 123 / 137 (89.8%) | 69 / 86 (80.2%) |
| vdbeaux.c | 98 / 125 (78.4%) | 37 / 58 (63.8%) |
| callback.c | 69 / 74 (93.2%) | 32 / 38 (84.2%) |
| prepare.c | 206 / 259 (79.5%) | 131 / 192 (68.2%) |
| tokenize.c | 59 / 76 (77.6%) | 37 / 54 (68.5%) |
| util.c | 92 / 97 (94.8%) | 76 / 84 (90.5%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 10 / 19 (52.6%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 31 / 34 (91.2%) | 26 / 36 (72.2%) |
| utf.c | 51 / 64 (79.7%) | 62 / 74 (83.8%) |
| hash.c | 74 / 74 (100.0%) | 42 / 44 (95.5%) |
| expr.c | 20 / 22 (90.9%) | 16 / 24 (66.7%) |
| ctime.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| func.c | 8 / 8 (100.0%) | 2 / 2 (100.0%) |
| fkey.c | 212 / 273 (77.7%) | 120 / 189 (63.5%) |
| **total** | 2705 / 3294 (82.1%) | 1645 / 2241 (73.4%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 75.3 | 675 | 71.8 | 112 |
| sqlite3BtreeSetCacheSize | btree.c | 6 | 100.0 | 0 | 100.0 | 0 |
| sqlite3BtreeSecureDelete | btree.c | 9 | 100.0 | 4 | 100.0 | 0 |
| sqlite3PagerJournalSizeLimit | pager.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PagerSetFlags | pager.c | 25 | 100.0 | 18 | 100.0 | 0 |
| sqlite3PagerSetPagesize | pager.c | 31 | 93.5 | 26 | 80.8 | 5 |
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
| pragmaVtabConnect | pragma.c | 34 | 94.1 | 12 | 83.3 | 2 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 78.6 | 3 |
| pragmaVtabFilter | pragma.c | 26 | 88.5 | 14 | 78.6 | 3 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 50.0 | 2 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 87.5 | 3 |
| pagerPlaybackSavepoint | pager.c | 37 | 78.4 | 44 | 54.5 | 12 |
| sqlite3PagerRollback | pager.c | 18 | 100.0 | 16 | 75.0 | 4 |
| sqlite3RunParser | tokenize.c | 75 | 77.3 | 54 | 68.5 | 10 |
| sqlite3Prepare | prepare.c | 68 | 63.2 | 46 | 54.4 | 12 |
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
| sqlite3Atoi64 | util.c | 51 | 98.0 | 54 | 94.4 | 3 |
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
| getPageNormal | pager.c | 45 | 80.0 | 32 | 75.0 | 7 |
| sqlite3PagerUnrefPageOne | pager.c | 5 | 100.0 | 0 | 100.0 | 0 |
| pagerUnlockIfUnused | pager.c | 4 | 100.0 | 2 | 100.0 | 0 |
| pagerUnlockAndRollback | pager.c | 21 | 42.9 | 14 | 71.4 | 3 |
| sqlite3InitCallback | prepare.c | 57 | 79.0 | 56 | 58.9 | 19 |
| corruptSchema | prepare.c | 18 | 66.7 | 14 | 64.3 | 5 |
| sqlite3PagerCloseWal | pager.c | 19 | 63.2 | 18 | 27.8 | 6 |
| sqlite3BtreeGetFilename | btree.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3PagerFilename | pager.c | 4 | 100.0 | 6 | 66.7 | 1 |
| sqlite3ValueText | vdbemem.c | 7 | 100.0 | 8 | 100.0 | 0 |
| valueToText | vdbemem.c | 14 | 78.6 | 16 | 43.8 | 4 |
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
| sqlite3ExprAnd | expr.c | 13 | 92.3 | 8 | 75.0 | 2 |
| sqlite3ExprSimplifiedAndOr | expr.c | 9 | 88.9 | 16 | 62.5 | 4 |
| sqlite3_compileoption_get | main.c | 5 | 100.0 | 4 | 100.0 | 0 |
| sqlite3_compileoption_used | main.c | 9 | 100.0 | 8 | 100.0 | 0 |
| sqlite3CompileOptions | ctime.c | 3 | 100.0 | 0 | 100.0 | 0 |
| compileoptiongetFunc | func.c | 4 | 100.0 | 0 | 100.0 | 0 |
| compileoptionusedFunc | func.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeReusable | vdbeaux.c | 6 | 100.0 | 4 | 50.0 | 2 |
| sqlite3_value_int | vdbeapi.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3_value_text | vdbeapi.c | 2 | 100.0 | 0 | 100.0 | 0 |
| sqlite3VdbeIntValue | vdbemem.c | 9 | 100.0 | 8 | 87.5 | 1 |
| sqlite3RealToI64 | vdbemem.c | 4 | 100.0 | 4 | 100.0 | 0 |
| sqlite3IsIdChar | tokenize.c | 1 | 100.0 | 0 | 100.0 | 0 |
| sqlite3Strlen30 | util.c | 3 | 100.0 | 2 | 100.0 | 0 |
| fkLookupParent | fkey.c | 57 | 56.1 | 34 | 55.9 | 8 |
| fkScanChildren | fkey.c | 42 | 69.0 | 22 | 50.0 | 7 |
| fkActionTrigger | fkey.c | 98 | 95.9 | 57 | 77.2 | 13 |
| sqlite3FkCheck | fkey.c | 76 | 75.0 | 76 | 60.5 | 20 |
| sqlite3VdbeCheckFkDeferred | vdbeaux.c | 4 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeCheckFkImmediate | vdbeaux.c | 3 | 100.0 | 2 | 100.0 | 0 |
| sqlite3VdbeNextOpcode | vdbeaux.c | 45 | 55.6 | 32 | 53.1 | 10 |
| sqlite3VdbeCountChanges | vdbeaux.c | 3 | 100.0 | 0 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 112574 | 2/2 |
| parse.y:524 | 61458 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yedi adlandırılmış opcode ve Noop/Explain ortak default bloğu (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 15/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_ResultRow | 2/4 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |
| OP_Noop_OP_Explain_default | 0/0 |

## V6b.26 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| insert.c:1011–1011 | 1/1 | 2/2 |
| delete.c:421–421 | 1/1 | 2/2 |
| update.c:608–608 | 1/1 | 2/2 |
| vdbeaux.c:3613–3613 | 1/1 | 2/2 |
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeaux.c:3405–3516 | 44/63 | 41/60 |
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


**665 alınmayan dal** [karar_esligi_v6b_26_dallar.md](karar_esligi_v6b_26_dallar.md) içinde. readOnly Halt BUSY, vtab xSync içinde yeniden giriş, NOMEM/FULL, genel interrupt/tahsis, bozuk UTF ve E28 pager ERROR yolları ayrıca kanıtlanmadı.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1119, 0, 337); açık (1104, 15, 337). C ikilisi PATH’te; açık anahtarın 15 başarısız adı V6b.25 ile aynı. C dışı eski beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; sınıf/örnek kümeleri öncekiyle aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanına karşı CPU6/tmpfs ve yedi A/B, anahtar kapalı. Hız ayarı yapılmadı; 14 sonuç özeti C ile aynı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.22      8.82     0.956x     9.18     8.79     9.60    0.92x  OK
W02_create_indexes                5.71      5.71     1.001x     5.65     5.64     8.76    0.65x  OK
W03_pk_point_select              29.75     30.57     1.028x    29.34    29.53    23.30    1.31x  OK
W04_secondary_idx_range           9.49      9.91     1.044x     9.28     9.66     7.46    1.33x  OK
W05_full_scan_filter_agg          4.40      4.39     0.998x     4.40     4.38    16.57    0.27x  OK
W06_join_group_order             10.79     10.99     1.018x    10.77    10.96    28.23    0.39x  OK
W07_window_functions             37.32     37.46     1.004x    37.12    37.24    50.21    0.75x  OK
W08_indexed_update_delete        12.00     11.91     0.993x    11.91    11.76     7.96    1.50x  OK
W09_like_text_scan               12.43     12.52     1.007x    12.42    12.46    18.92    0.66x  OK
W10_jsonb_extract_agg            13.71     13.59     0.992x    13.61    13.47    10.93    1.24x  OK
W11_fts5_index_and_bm25          69.41     70.69     1.018x    68.96    68.97    57.02    1.24x  OK
W12_rtree_insert_and_bbox        40.40     41.26     1.021x    39.47    39.74    24.63    1.68x  OK
W13_autocommit_wal_txns           4.49      4.82     1.075x     4.42     4.54     4.12    1.17x  OK
W14_cli_sql_script               38.80     53.12     1.369x    38.58    52.71    26.19    2.03x  OK
TOTAL                           297.92    315.76     1.060x   geomean cand/base = 1.034x   digest mismatches = 0
```

## Açık kalan işler

D132 commit hook hata metni sıradaki doğruluk işidir. D52 eski RETURNING sayaçları, D103, E28, kalan PRAGMA’lar ve M4; veri/DDL/JSON5/UTF/CLI eşliği ile eski yürütücünün kaldırılması açık. Yayın veya tam C garantisi verilmez. Sonraki adım kalan PRAGMA üreticileri ve bunların canlı bağlantı kayıtlarıdır. ileriPlanlama dizini değerlendirilmedi.

Ölçüm: target/perf-work/v6b26/. source_manifest.json doğrulanan ürün dosyalarını sabitler; validated.done test/sonuç/sınıf kümesi ve kaynak hash denetiminden sonra yazılır. Ham başlangıç, ara ve son ölçümler korunur.
