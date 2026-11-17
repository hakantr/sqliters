# V6b.21 — database_list hazırlama anı ve makine yürütmesi

C SQLite 3.54.1; Rust 1.100. database_list kod üreticiye taşındı. Normal PRAGMA iki anahtar durumunda aynı makine programından çalışır; donmuş satırlar eski tek hücreli PRAGMA çözücüsüne eklenmedi. EXPLAIN/EQP ortak üreticiden gelir. V6b bütünü, genel UTF kodlama eşliği ve M4 henüz tamamlanmadı.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:444–457` | Bir kez çalışma, şema seçimi ve açıkça TEMP | Ortak üretici runOnlyOnce işaretini kurar; bilinmeyen şema hazırlanırken hata verir. temp.database_list önce TEMP Btree açar. | `crates/sqliters-codegen/src/pragma.rs:232` |
| `pragma.c:1442–1446` | nMem=3; sayısal slot sırası; kapalı Btree atlanır | İsimler, sıra ve üyelik hazırlama anında sabitlenir. Sağ taraf ve seçilen şema listeyi filtrelemez. | `crates/sqliters-codegen/src/pragma.rs:451` |
| `pragma.c:1448–1451` | Üç hücre: sıra, isim, dosya | Integer/String8/ResultRow dizisi üretilir; sonraki ATTACH ilk çalıştırmanın satırlarını değiştirmez. | `crates/sqliters-codegen/src/pragma.rs:461` |
| `btree.c:11367–11369; pager.c:7164–7170` | Disk dosyası ile bellek/TEMP ayrımı | Host gerçek Btree yolunu veya boş metni verir. Geçersiz UTF-8 yol kayıplı çevrilmez; üretici Unsupported döner. Özel memdb VFS ayrıca kanıtlanmadı. | `crates/sqliters-core/src/cgen.rs:101` |
| `vdbe.c:8386–8393` | Expire P1!=0, P2=1 | Yalnız self/advisory biçimi makinede desteklenir. Çalışan ifade devam eder; reset veya yeniden çalıştırma öncesi derlenir. Global/immediate biçimler M4 kapsamında açıktır. | `crates/sqliters-vdbe/src/vdbe.rs:2103` |
| `attach.c:423–427; vdbeaux.c:5290–5294` | ATTACH kendisini, DETACH bütün ifadeleri expire eder | Önceden var olan c_expire_seq artık Statement tarafından da izlenir. Böylece şema gerektirmeyen ve hiç adımlanmamış database_list, DETACH sonrası yeniden hazırlanır. | `crates/sqliters-core/src/lib.rs:5186` |
| `vdbeapi.c:882–886` | Aktif ifade ve aktif okuyucu sayaçları ayrıdır | Program.b_is_reader=0 olan bu liste okuyucu sayılmaz. Satırlar beklerken CREATE/INSERT/DROP davranışı C ile eşleşir. | `crates/sqliters-core/src/lib.rs:9985` |
| `vdbe.c:1475–1501,1519–1525; utf.c:318–334` | String8: UTF-8 veya bağlantının UTF-16 kodlaması | Host kodlamasıyla geçerli UTF-8 program dizgileri çevrilir; dönüşüm öncesi ve sonrası uzunluk sınırı denetlenir. Genel geçersiz UTF-8 ve tahsis hatası eşliği açık. | `crates/sqliters-vdbe/src/vdbe.rs:2250` |
| `vdbemem.c:1565–1577,1525–1548; utf.c:336–397` | UTF-16 sonuç metni UTF-8 API sonucuna çevrilir | Mem.enc=2/3 sonuçları uygun LE/BE çözücüyle alınır; ASCII içindeki NUL baytları kullanıcı metnine sızmaz. Bozuk surrogate davranışı tamamlanmış sayılmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:521` |
| `pragma.c:1442–1454` | Şema okuyucusu ve veri işlemi gerektirmez | Sıfır Btree maskeli program iki anahtarda gerçek makineden yürür. Bozuk ana dosyada BEGIN/liste/COMMIT akışı gereksiz şema okuması yapmaz. | `crates/sqliters-core/src/lib.rs:6015` |

## Native kanıt ve düzeltilen farklar

**D123 — hazırlama anı ve expire.** Önceki committen alınan 12 akışın tamamı farklıydı: iki anahtar × üç kodlama × bellek/disk. C hazırlanan listeye daha sonra ATTACH edilen aux satırını katmaz; eski Rust katardı. Yeni üreticiden sonra ATTACH düzeldi, fakat ilk step öncesi DETACH edilmiş aux eski programda kaldı. Statement.c_expire_seq izlemesi sonrasında 12/12 sonuç, hata ve bütün .db baytları eşleşti (res/database_list_before.json; database_list_initial.json; database_list_expire_seq.json). Geçmiş *_with_driver_dump kayıtları, Rust sürücüsüne yanlışlıkla C dump argümanı verilmiş ilk deneyi içerir; final ölçümde Rust kendi üreticisini kullanır ve bu argüman verilmez.

**D124 — UTF-16 sabit metin sonuçları.** Yeni gerçek makine yolu UTF-16 bağlantılarında metinleri UTF-8 gibi işliyordu; ilk birim/native deneyi bunu yakaladı. OP_String8 bağlantı kodlamasını kullanır, Mem::to_value enc alanını çözer. ASCII, ü, CJK, emoji ve U+FFFE isimleri LE/BE dahil eşleşir. Geçersiz UTF-8, bozuk surrogate ve genel metin işlevleri bu kanıtın dışındadır (res/database_list_unit_initial.txt; database_list_unit_encoding_fixed.txt; row_matrix.json).

**D125 — aktif liste satırı gereksiz okuyucu sayılıyordu.** database_list satırı beklerken C DROP TABLE çalıştırır; Rust tablo kilitli hatası verirdi. Genel Read başlangıcı artık mevcut C programının b_is_reader bayrağını kullanır. İlk ham 204 deney 128 fark içerir: gerçek UTF-16/okuyucu kusurlarına ek olarak script içindeki boş satır sürücü protokolünü bozuyordu. Boş satır hatası ölçüm aracında düzeltildi; bu 128 sayısı ürün kusuru sayısı olarak sunulmaz (res/row_matrix_before_activity_and_encoding_fix*.json).

**210 genişletilmiş akış** iki anahtar, üç kodlama, bellek/disk üzerinde 17 bağlamı; ayrıca bozuk ana dosyada altı akışı kapsar. RHS/şema adı, TEMP açılması, ATTACH/DETACH öncesi hazırlama, bekleyen satırla ATTACH/DETACH, reset, DDL/DML/DROP, Unicode isimler, pragma_database_list sanal tablosu ve yanlış argüman hatası, ilerleme callback ile durdurma/devam, statement sayaçları karşılaştırılır. Sonuç/stdout/stderr ve bütün .db baytları eşleşir. Ek **12 dosyaya bağlı aux akışı** göreli aux yolunun tam dosya adına çevrilmesini ve dosya baytlarını doğrular (res/row_matrix.json; row_paths.json). Native matris V2 prepare API kullanır; eski V1 hata kodu davranışı ayrıca açık kalır.

Program karşılaştırması ilk turda database_list için 11 fark gösterdi: ölçüm aracı C ve Rust girdisinde farklı mutlak test.db yolları kullanıyordu. Diğer blokların ayrı çalışma dizinleri korunarak bu path içeren bloklar aynı temiz dizin/dosya adı üzerinde sırayla çalıştırıldı; metin veya yol normalleştirilmedi. Ham önceki sonuçlar res/before_path_isolation_fix_* dosyalarındadır. Yeni tam programların tamamı eşleşir.

Kapsam sürücüsü ilk denemede stdout üretmeyen lifecycle_cov idi; 210 çıktının bu nedenle karşılaştırılamadığı kayıtlar res/before_row_driver_fix_* içinde korundu. Public API sürücüsü aynı instrumented sqlite3.o dosyasına bağlandı; 210/210 çıktı ve dosya eşliğinden sonra mevcut sayaçlardan gcov raporu üretildi. Kaynak kod SHA’sı değişmedi. G/Ç kapısı ilk çalışmada eksik short_before.json fixture’ında durdu; kalan original SHORT_READ/WAL/liste deneyleri fixture kopyalandıktan sonra tamamlandı, ilk hata günlüğü saklandı.

Tam program 13192 aynı; 115 Unsupported başarı sayılmadı. EXPLAIN 15643, EQP 15643, yürütme 11459 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103. Bütün API dosya baytları aynı.

Önceki G/Ç kanıtları yeniden denetlendi: 896 sync, 160 bağlantı/cache, 72 okuma, 30 hata, 56 page1, 16 reprepare, 64 rollback/counter, 640 SHORT_READ, 128 cache toparlanma ve dört WAL reader çıkışı. 72 okumada DB sayfa okumaları ve .db baytları aynı; bütün ham G/Ç izleri 52/72 aynı, kalan 20 dirty/TEMP farkı açık. Bu adım bunları çözmüş sayılmaz.

## Kapsam ve hiç alınmayan dallar

Kapsam birikimlidir: kabul edilen 15.768 SQL ifadesi (EXPLAIN ve EQP), şema kurulumları, önceki native hata/kilit akışları ve yeni 210 akış. Yeni tam ölçülen yordamlar Btree/Pager dosya adı, expire ve metin kodlama yardımcılarıdır. Bir yordamın ölçülmesi bütün Rust karşılığının taşındığı anlamına gelmez. SQLite VDBE bütünü sayılmaz; yalnız altı opcode bloğu, SELECT üst kuralı ve belirtilen özel bloklar ölçülür. OP_Expire global dalının C’de kurulum DDL’si tarafından alınması, Rust makinesinin o biçimi desteklediği anlamına gelmez.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 15768 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 880 / 1142 (77.1%) | 584 / 794 (73.6%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 319 / 380 (83.9%) | 211 / 306 (69.0%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 84 / 95 (88.4%) | 40 / 50 (80.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 25 / 26 (96.2%) | 5 / 6 (83.3%) |
| prepare.c | 195 / 259 (75.3%) | 121 / 192 (63.0%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 82 / 94 (87.2%) | 68 / 82 (82.9%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 44 / 61 (72.1%) | 15 / 32 (46.9%) |
| vdbeapi.c | 6 / 15 (40.0%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 18 / 21 (85.7%) | 14 / 24 (58.3%) |
| utf.c | 51 / 64 (79.7%) | 62 / 74 (83.8%) |
| **total** | 2200 / 2742 (80.2%) | 1339 / 1860 (72.0%) |

| C işlevi | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 73.5 | 675 | 70.8 | 115 |
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
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 79.3 | 9 |
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

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 94026 | 2/2 |
| parse.y:524 | 47360 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen altı işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |

## V6b.21 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |
| vdbe.c:3136–3147 | 0/2 | 0/4 |
| vdbe.c:3210–3224 | 7/7 | 10/12 |
| vdbe.c:3393–3402 | 4/6 | 1/2 |


**573 hiç alınmayan dal** [karar_esligi_v6b_21_dallar.md](karar_esligi_v6b_21_dallar.md) içinde ayrı satırlarda sınıflandırıldı. Yeni dilimde küçük SQLITE_LIMIT_LENGTH, NOMEM, UTF-16→UTF-16 doğrudan takas, bozuk UTF-8/surrogate ve özel memdb VFS yönleri ayrıca kanıtlanmadı. Authorizer/VFS özel PRAGMA cevapları ve genel M4 expire biçimleri açık.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1114, 0, 337); açık (1099, 15, 337). C ikilisi PATH’teydi. Açık anahtardaki 15 başarısızın adları önceki commit ile aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; önceki sınıf/seed kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs; yedi A/B; anahtar kapalı. Performans ayarı yapılmadı; bu ölçüm açık anahtarın hız garantisi değildir.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.24      8.87     0.960x     9.16     8.78     9.60    0.92x  OK
W02_create_indexes                5.65      5.67     1.002x     5.64     5.65     8.76    0.65x  OK
W03_pk_point_select              30.04     30.72     1.023x    29.65    29.90    23.30    1.32x  OK
W04_secondary_idx_range           9.61      9.83     1.023x     9.39     9.76     7.46    1.32x  OK
W05_full_scan_filter_agg          4.40      4.36     0.991x     4.39     4.35    16.57    0.26x  OK
W06_join_group_order             10.80     11.33     1.050x    10.76    11.30    28.23    0.40x  OK
W07_window_functions             37.05     38.07     1.027x    37.01    37.98    50.21    0.76x  OK
W08_indexed_update_delete        12.01     11.79     0.982x    11.94    11.70     7.96    1.48x  OK
W09_like_text_scan               12.42     12.38     0.997x    12.41    12.34    18.92    0.65x  OK
W10_jsonb_extract_agg            13.72     13.77     1.003x    13.67    13.61    10.93    1.26x  OK
W11_fts5_index_and_bm25          69.22     70.07     1.012x    68.85    69.50    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.30     40.82     1.013x    39.51    39.91    24.63    1.66x  OK
W13_autocommit_wal_txns           4.46      4.63     1.040x     4.39     4.52     4.12    1.13x  OK
W14_cli_sql_script               38.79     52.87     1.363x    38.63    52.60    26.19    2.02x  OK
TOTAL                           297.71    315.17     1.059x   geomean cand/base = 1.031x   digest mismatches = 0
```

## Açık işler ve kayıt

Sıradaki uygun V6b işi collation_list; Hash dolaşım sırası ve dinamik kayıtlar C ile eşleştirilecek. function_list/module_list, diğer PRAGMA’lar, D103, E28 pager ERROR/COMMIT, M4, UTF-16/geçersiz UTF-8 ve eski yürütücü hâlâ açık. Tam C garantisi veya yayın tamamlanması iddiası yok.

Kanıt /home/hakan/github/sqliters/target/perf-work/v6b21/; sekiz kaynak/test dosyası res/source_manifest.json ile sabittir. C kaynakları değiştirilmedi. ileriPlanlama değerlendirilmedi.
