# V6b.27 — D132 commit hook hata metni

Commit hook’un true döndürerek reddettiği commit için Rust’ın özel commit hook vetoed commit metni C SQLite 3.54.1’in constraint failed metniyle değiştirildi. 531 genişletilmiş kodu, -1 hata konumu ve mevcut rollback/sayaç davranışı korundu. Rust 1.100; performans ayarı yapılmadı.

## C kararları

| C koşulu | C sonucu | Rust karşılığı |
|---|---|---|
| vdbeaux.c:2997–3002: needXcommit ve kayıtlı callback; callback sonucu sıfır / sıfır dışı | Sıfır dışı sonuç 531; özel p.zErrMsg kurulmaz. Sıfır normal commit’e devam eder. | `crates/sqliters-core/src/execution_context.rs:919`; engine.rs: commit_all_databases |
| vdbeaux.c:3554–3563: p.zErrMsg NULL / dolu | NULL olunca önceki dinamik hata metni temizlenir; normal hata aktarımı kodu korur. Bu dilim genel hata nesnesi tahsis eşliği iddiası değildir. | `crates/sqliters-core/src/execution_context.rs:919`: hata nesnesi doğrudan C varsayılan metnini taşır |
| main.c:2760–2763: dinamik hata metni yok | sqlite3ErrStr(db.errCode) kullanılır. | `crates/sqliters-core/src/execution_context.rs:919` |
| main.c:1732–1734: rc &= 0xff ve geçerli aMsg girdisi; main.c:1702 | 531 & 255 = 19; aMsg[SQLITE_CONSTRAINT] = constraint failed. | `crates/sqliters-core/src/execution_context.rs:919` |

## Doğrulama

V6b.26’nın gerçek EXPLAIN hook kayıtları aynı girdiler ve C gözlemleriyle yeniden çalıştırıldı. Doğrudan işlem fixture’ında D komutu tek ifade hazırladığı için BEGIN;INSERT ve SAVEPOINT;INSERT girdilerinin ikinci SQL’i çalışmamıştı. Bu iki girdi ayrı D satırlarına bölündü; V6b.26 eski ikilisi ve V6b.27 yeni ikilisi aynı düzeltilmiş girdilerle yeniden ölçüldü (hook_correct_before.json / hook_legacy.json). Önceki 36 akışın 24’ü hook’u çağırmıyordu; o sonuçlar gerçek yazmalı hook kanıtı sayılmaz. V6b.26 raporundaki doğrudan COMMIT/RELEASE kapsamı bu kayıtla daraltıldı. Ürün kaynakları fixture düzeltmesi sırasında değişmedi. 192 EXPLAIN kapanışı önce 120/192 aynıydı, 72 hata metni farklıydı; şimdi 192/192 tam aynı. Doğru kurulmuş bağımsız 36 otomatik INSERT/COMMIT/RELEASE akışının 36’sında da önce metin farklıydı; sonra 36’sının metni C ile aynı. 26/36 tamamen aynı; 10 eski legacy SAVEPOINT autocommit farkı D133 olarak açık. Bu 10 başarı sayılmadı. Bütün ana dosya baytları aynı; stdout/stderr/kod/konum/sayaçlar normalize edilmedi.

192 akış; iki anahtar, üç kodlama, DELETE/WAL, başka C bağlantısının açık okuma kilidi, hook kabul/reddi, EXPLAIN’in ilk satırı/EOF sonrası reset/finalize birleşimlerini kapsar. Kilit ve veto birlikteyken C önce kilit hatasını döndürür; bu sıra da eşit. Yeni regresyon testi normal otomatik INSERT ve EXPLAIN kapanışında 531 metnini, rollback’i, changes/total_changes ve veri yokluğunu denetler.

Önceki 72 yazıcı ve 408 raw EXPLAIN akışı, eski altı D131, 3990 compile-options, 438 collation, 636 D128 ve 36 kabuk akışı aynı. Genel programlar 13414 aynı, 82 desteklenmiyor; EXPLAIN 15909, EQP 15909, yürütme 11692 aynı. Üreticiden önceki 64 hazırlama hatası C ile aynı. Defer 1624/1644 tam aynı; 20 eski D52 sayaç farkı kusur olarak korunur, bütün 1644 dosya eşit. Manual API’de tek eski D103 çıktı farkı sürer.

## Kapsam

Yeni ölçüm yalnız vdbeCommit’in hook koşulu, VdbeTransferError’ın p.zErrMsg koşulu ve sqlite3_errmsg’in varsayılan metin koşulunu kapsar. Bu işlevlerin tamamı ölçüldü veya taşındı sayılmaz. Birikimli tablolar önceki PRAGMA, pager, FK ve EXPLAIN ölçümlerini de içerir.

# C-side coverage of the functions ported or completed in step V6b.27 (gcov, -O0 build of the reference amalgamation)

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
| parse.y:523 | 111806 | 2/2 |
| parse.y:524 | 60882 | 2/2 |

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

## V6b.27 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeaux.c:2997–3002 | 4/4 | 5/6 |
| vdbeaux.c:3554–3554 | 1/1 | 2/2 |
| main.c:2760–2763 | 2/2 | 2/2 |
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


**666 alınmayan dal** [karar_esligi_v6b_27_dallar.md](karar_esligi_v6b_27_dallar.md) içinde ayrı sınıflandırıldı. Genel NOMEM/pErr tahsisi, callback yeniden girişi/panik ve xSync eşliği bu düzeltmeyle kanıtlanmış sayılmaz.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1120, 0, 337), açık (1105, 15, 337). C PATH’te; açık anahtarın 15 başarısız adı V6b.26 ile aynı. DML kapalı 378/400, açık 398/400 ve sınıf/örnek kümeleri aynı. C dışı test beklentileri değiştirilmedi.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı, anahtar kapalı, CPU6/tmpfs, yedi A/B. 14 özet C ile aynı; ölçüm hız ayarı değildir.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.24      8.86     0.959x     9.19     8.79     9.60    0.92x  OK
W02_create_indexes                5.68      5.67     0.997x     5.63     5.63     8.76    0.65x  OK
W03_pk_point_select              30.03     29.95     0.998x    29.41    29.58    23.30    1.29x  OK
W04_secondary_idx_range           9.48      9.84     1.038x     9.39     9.65     7.46    1.32x  OK
W05_full_scan_filter_agg          4.40      4.40     1.000x     4.38     4.38    16.57    0.27x  OK
W06_join_group_order             10.81     11.04     1.021x    10.77    11.01    28.23    0.39x  OK
W07_window_functions             37.13     37.53     1.011x    36.97    37.19    50.21    0.75x  OK
W08_indexed_update_delete        12.01     11.98     0.998x    11.94    11.84     7.96    1.51x  OK
W09_like_text_scan               12.42     12.56     1.011x    12.41    12.49    18.92    0.66x  OK
W10_jsonb_extract_agg            13.73     13.56     0.987x    13.71    13.48    10.93    1.24x  OK
W11_fts5_index_and_bm25          70.30     69.70     0.992x    68.81    69.04    57.02    1.22x  OK
W12_rtree_insert_and_bbox        40.08     41.12     1.026x    39.42    40.26    24.63    1.67x  OK
W13_autocommit_wal_txns           4.61      4.64     1.008x     4.45     4.45     4.12    1.13x  OK
W14_cli_sql_script               38.86     53.13     1.367x    38.50    52.87    26.19    2.03x  OK
TOTAL                           298.78    313.99     1.051x   geomean cand/base = 1.026x   digest mismatches = 0
```

## Açık işler

D132 kaydedilen geçerli hook reddi girdilerinde kapandı. Sıradaki D133: SAVEPOINT RELEASE hook tarafından reddedilince legacy yürütücü autocommit’i 1 yapıyor; C 0 koruyor. D52, D103, E28, kalan PRAGMA/M4, veri/DDL/JSON5/UTF/CLI eşliği ve eski yürütücünün kaldırılması açık. D133 sonrasında case_sensitive_like’ın hazırlama ve işlev kaydı kararlarına dönülecek. Tam C veya yayın garantisi verilmez. ileriPlanlama değerlendirilmedi.

Ölçüm dizini target/perf-work/v6b27; source_manifest.json kaynak dondurmasını, validated.done bütün sonuçların ve önceki başarısız kümelerinin kontrolünü kaydeder.
