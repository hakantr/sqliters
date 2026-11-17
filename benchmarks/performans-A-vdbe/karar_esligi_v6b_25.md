# V6b.25 — defer_foreign_keys hazırlama, FK ve işlem sonu; D131 açık

C SQLite 3.54.1; Rust 1.100. FLAG kod üretimi, prepare anındaki etki, normal FK akışları ve işlem sonu temizliği taşındı. **V6b ve genel kapanış eşliği açık kalır:** eski RETURNING yürütücüsündeki D52 ve EXPLAIN yazıcı sayacındaki yeni D131 aşağıda başarısız örnekleriyle tutulur. Bu rapor bütün motor veya bütün defer_foreign_keys durumları için C garantisi değildir.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:1158–1201` | RHS yok / bool ayarı / OFF sayaçları | Mevcut FLAG üreticisinin defer_foreign_keys engeli kaldırıldı. Değişiklik prepare sırasında uygulanır; OFF iki FK sayacını ve legacy kuşkularını sıfırlar. İfade step edilirse OP_Expire bütün ifadeleri iptal eder. | `crates/sqliters-codegen/src/pragma.rs:340` |
| `pragma.c:1159–1161; vdbe.c:8386–8395` | Sorgu sonucu hazırlamada donar; kendi Expire/advisory reset | Sorgu gerçek C programını iki anahtarda çalıştırır; reset/reprepare sayaçları eski sıfır sayaçlı yoldan gelmez. | `crates/sqliters-core/src/lib.rs:6083` |
| `prepare.c:381–400; vdbeaux.c:3359,3416–3449` | İlk şema yüklemesinin iç SELECT’i | Başarılı yükleme veya corruptSchema callback hatası, autocommit ve yazıcı yoksa ertelemeyi iç sorgunun kapanışında temizler. SELECT henüz step edilmeden prepare sırasında görülebilir. Başlık hatası ve genel NOMEM/GÇ aşamaları topluca aynı kabul edilmez. | `crates/sqliters-core/src/lib.rs:1936` |
| `fkey.c:440–455` | Derleme kararı / çalışan bayrak | Tek satırlı erken Halt ile FkCounter ayrımı prepare anındaki bayrağı kullanır; gerçek FK sayaç yerleşimi çalışan bayrağı kullanır. Step edilmemiş PRAGMA yüzünden eski programın kararları değişmez. | `crates/sqliters-core/src/engine.rs:481` |
| `fkey.c:1230–1232` | RESTRICT ve prepare anında DeferFKs | Genel erteleme açıksa RESTRICT eylem programı üretilmez. Declared deferred niteliği ile genel bayrak aynı şey değildir. Eski hazırlanmış program, PRAGMA step edilene kadar eski kararını korur. | `crates/sqliters-core/src/engine.rs:23081` |
| `fkey.c:452–455,1078–1080; vdbeaux.c:2708–2709` | MayAbort / usesStmtJournal | Bu koşullar FK’nin kendi isDeferred alanını kullanır; genel DeferFKs açık diye MayAbort kaldırılmaz. Analyzer ve statement-journal kararı kaydedilen prepare bayrağıyla uyumludur. | `crates/sqliters-core/src/dml_c.rs:1299` |
| `delete.c:842–865; update.c:1105–1120` | Satır yazımı, ardından FK eylemi | RESTRICT RAISE(ABORT) 1811 hatası satır değişikliğinden sonra gelir. Erken denetimler kaldırıldı; hata eylem döngüsünde üretilir. Statement rollback veri satırını geri alsa da C’nin dirty/change-counter davranışı korunur. | `crates/sqliters-core/src/engine.rs:23082` |
| `vdbe.c:7809–7843` | FkCounter/FkIfZero: canlı DeferFKs | Makine artık bağlantının gerçek bayrağını okur; nDeferredCons/nDeferredImmCons yolları ayrılır. Legacy normal FK doğrulamasında live flag ile prepare kararı karıştırılmaz. | `crates/sqliters-core/src/vdbe_host.rs:747` |
| `vdbeaux.c:3306–3313,3447–3449` | FK hata kontrolü / başarılı commit | Başarısız ertelenmiş COMMIT işlemi açık ve bayrağı korur. Başarılı commit tüm sayaçları ve bayrağı temizler. | `crates/sqliters-core/src/engine.rs:1535` |
| `main.c:1541–1558` | Rollback; xRollback sonrası ve rollback hook öncesi | Bütün veritabanları geri alınınca sayaçlar/bayrak temizlenir. ROLLBACK TO ise savepoint sayaçlarını geri yükler ve mevcut bayrağı geri almaz. | `crates/sqliters-core/src/engine.rs:1363` |
| `vdbeaux.c:3359,3416–3449; vdbeapi.c:881–886` | BIsReader / autocommit / başka yazıcı | C okuyucu biti seçilen yürütücüden bağımsız taşınır. Normal read, reset ve denetim hatası kapanışları bayrağı temizler; veri okumayan sıcak SELECT 1 korur. EXPLAIN yazıcı sayacı D131 olarak ayrıca açıktır. | `crates/sqliters-core/src/lib.rs:2110` |
| `vdbeaux.c:887–918; vdbeapi.c:881–886` | EXPLAIN INSERT readOnly=0, bIsReader=1 | AÇIK D131: NoLockPragma bu programı yazıcı olarak saymıyor. Yanındaki okuyucu bayrağı C’de 1 bırakırken Rust 0 yapıyor. Genel bekleyen işlem kapanışı bu dilimde tamamlandı sayılmaz. | `crates/sqliters-core/src/lib.rs:9563` |

## Native önce/sonra

Başlangıçtaki 180 akışın tamamı farklıydı: PRAGMA kod üreticide Unsupported, legacy sorgusu da boştu (res/defer_before.json). 492 akışta yalnız bayrak/host taşıması 260 fark bıraktı (defer_initial.json). FK ve sayaç yolu eklendikten sonra 564 akışta 192 fark yalnız okuyucu/şema/kapanış tarafındaydı (defer_fk.json).

İlk şema yüklemesi, gerçek bIsReader, reset sayaçları ve kapanış kuralları eklenince 876 akışta 40 RESTRICT farkı kaldı (defer_reader.json). Erken RESTRICT ve DML bayrakları hizalanınca 876/876 eşleşti (defer_restrict.json). WAL, aktif ifadeler ve bozuk şema eklenince 1500 akışta 26 fark görüldü: 20 eski D52 sayaç farkı ve altı corruptSchema kapanışı (defer_expanded.json). Sonraki düzeltme altı şema farkını kapattı (defer_schema_error.json).

Prepare sırası ayrıca sınandı: eski INSERT programı hazırlanıp PRAGMA ON/OFF yalnız hazırlanarak bırakıldığında kodlanmış karar ile live flag ayrılmalıdır. 1548 akışta bu ayrım eksikliği 20 ek fark gösterdi (defer_prepare_order.json). Prepare bayrağı ayrı tutuldu; tek satırlı erken Halt, RESTRICT ve statement-journal kararları bu kayıtla uygulanır. Yeni statement oluşmadan çalışan bayrağın değişmesi eski programı yeniden yazmış sayılmaz.

## D130 — başarısız RESTRICT ve veritabanı baytları

Sonuç/hata aynı olsa da diskteki iki başlık baytı farklıydı. Satır önce değiştirilip RESTRICT eyleminde hata veren C, statement rollback sonrasında commit’te change_counter/version_valid_for değerini artırır; erken hata veren Rust aynı dirty yoluna girmezdi. Ayrı 12 örnekte 3a17161 ikilisinin 10’u farklıydı (iki anahtar × üç kodlama × DELETE/UPDATE; açık UTF-8 makine yolu zaten aynı). Yeni kaynakla 12/12 çıktı, hata ve bütün .db baytları aynı (res/restrict_legacy.json). Başlıklar da ham hex olarak kaydedildi.

Prepare geçmişi genişletilince 1644 akışta statement-journal gerekliliğinin yanlışlıkla kaldırıldığı beş dosya farkı daha ortaya çıktı: DELETE bayrak açıkken hazırlanıp OFF ifadesi step edilmeden bırakılıyordu. C fkey.c:453 ve :1079 genel bayrak açık olsa da declared-immediate FK için MayAbort ister; C programında usesStmtJournal=1. Analyzer bu ayrımı artık korur. Bu beş dosya farkı da kapandı; 1644 akışın tamamının .db baytları eşit (defer_may_abort.json, final defer_matrix.json).

RESTRICT denetimi, parent satır yazımından sonraki FK eylem sırasına taşındı. CASCADE/SET NULL ile RESTRICT’in farklı sıraları ve DELETE/UPDATE için ek **96 akış** tam eşit (mixed_restrict.json); veri, hata, durum ve .db baytları aynı. Bu kaldırma bağımsız boyut temizliği değildir: erken denetimler C’nin hata/yazım sırasını değiştiriyordu.

## Final kapsamı ve açık farklar

**1644 akışın 1624’ü tam eşit; 20’si D52 olarak başarısız tutuldu.** İki anahtar, üç kodlama, bellek/disk, rollback/WAL; bool sınırları, query freeze/reset/reprepare, first-schema prepare etkisi, explicit/implicit savepoint, başarılı/başarısız COMMIT, ROLLBACK/ROLLBACK TO, FK çözme/OFF sıfırlama, beş FK eylemi, prepared INSERT/DELETE/UPDATE ve çalıştırılmadan bırakılan setter kapsanır. Bozuk dosya örnekleri yalnız diskte; WAL bellek örneği C gibi memory kipine döner.

Ek **60 akış** autocommit FK ihlali, WAL, temp/aux global bayrağı ve bilinmeyen şema için tam eşit (defer_extra.json). Bu örnekler C gcov profiline de eklendi. Ölçülen stdout/stderr, çıkış kodu, satırlar, hata kodu/metni/konumu, durumlar ve kaydedilen sayaç yanıtları ham olarak karşılaştırıldı. Bütün final .db SHA-256 değerleri aynı. Bu sayılar bütün public API sayaçları veya tüm NOMEM/uzunluk/kilit kombinasyonları anlamına gelmez.

**D52 — AÇIK, önceki legacy davranış:** çalışan INSERT RETURNING ilk satırını verdikten sonra C changes/total_changes bilgilerini Halt’a bırakır; legacy yol erken yayımlar. İki active_writer ailesindeki 20 fark yalnız bu yedi ham çıktı satırıdır. defer_known.py tam fark çiftlerini ve bütün diğer alanları doğrular; çıktı normalize edilmez, bugs JSON’daki 20 kayıt silinmez ve tam eşit sayılmaz. PRAGMA içermeyen bağımsız altı örnekte önceki/sonraki Rust çıktıları birebir aynı; beşi eski D52 farkını korur, açık UTF-8 C makinesi eşittir (returning_legacy.json). Eski yürütücü ve C dışı test beklentileri bu dilimde kaldırılmadı/değiştirilmedi.

**D131 — AÇIK, sonraki öncelik:** EXPLAIN INSERT C’de readOnly=0 ile nVdbeWrite sayacına katılır. NoLockPragma bunu yazıcı saymıyor. Çalışan EXPLAIN yanında PRAGMA ON ve bir okuyucu yürütüldüğünde C bayrağı 1 bırakır; Rust 0 yapar. Altı örneğin altısı farklı, dosyalar aynı (explain_writer.json ve explain_writer_bugs.json). Bu bulgu mevcut 1644 örneğin başarı oranına gizlice eklenmedi; genel metadata yazıcı sayacı ve bekleyen işlem kapanışı tamamlanmadı sayılır.

Genel program derlemi 13414 aynı; 82 Unsupported başarı sayılmadı. EXPLAIN 15909, EQP 15909, yürütme 11692 aynı. Üretici trace’inden önceki 64 hazırlama hatası C ile aynı karşılaştırılır. 39 collation geçmişindeki 1362 ek program, 3990 compile-options akışı, 438 collation/636 D128 ve 36 kabuk akışı korundu. Önceki manual API 4157/4158 aynı (tek D103 çıktı farkı); bütün dosyalar aynı.

## Kapsam ve ölçüm kaydı

FK yardımcıları ve immediate/deferred FK denetimleri ölçüme eklendi. Halt’ın 3405–3468 commit bloğu ayrı bölge olarak sayılır; tüm VdbeHalt veya VdbeExec tamamlandı sayılmaz. FK ve Halt kaynak konumları kendi işlev gövdeleri içinde eşleştirilir. Amalgamın kaldırdığı sondaki kaynak boşlukları yalnız kaynak-satır eşlemesinde rstrip edilir; SQL çıktıları değiştirilmez. İlk eşleme, bu boşluk yüzünden durdu; ölçüm verisi korunarak eşleme düzeltildi.

V6b.24’te Noop ölçüm listesine eklenmiş fakat C’nin ortak default bloğu adlandırılmış opcode regex’ine girmemişti; tabloda yedi bölge vardı. Önceki rapordaki sekizinci bölge iddiası açıkça düzeltildi. Bu dilimde vdbe.c:9423–9427 ortak Noop/Explain default bloğu ayrıca ölçüldü; sıfır dalı olan bir bloktur.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 16034 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 900 / 1142 (78.8%) | 593 / 794 (74.7%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 324 / 380 (85.3%) | 219 / 306 (71.6%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 123 / 137 (89.8%) | 69 / 86 (80.2%) |
| vdbeaux.c | 70 / 77 (90.9%) | 20 / 26 (76.9%) |
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
| **total** | 2677 / 3246 (82.5%) | 1628 / 2209 (73.7%) |

| C işlevi | file | lines | line % | branches | taken % | source lines with a branch never taken |
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

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 109310 | 2/2 |
| parse.y:524 | 59346 | 2/2 |

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

## V6b.25 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeaux.c:3405–3468 | 31/42 | 28/44 |
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


**647 alınmayan dal** [karar_esligi_v6b_25_dallar.md](karar_esligi_v6b_25_dallar.md) içinde tek tek sınıflandırıldı. VtabInSync, ROW trace, read-only Halt BUSY, NOMEM/FULL ve genel callback/özel hata durumları ayrıca kanıtlanmadı. Branch’in gerçek veri yazıcısıyla alınmış olması EXPLAIN metadata sayacının doğru olduğunu kanıtlamaz (D131).

IO devamında eski D118 C gözlemini karşılaştıran res/short_before.json girdisi ilk kopyada eksikti. Native iki sonuç ve dosya eşitliği geçmişti; yalnız tarihsel karşılaştırma okunamadı. Eski veri asıl haliyle kopyalanıp o adım ve sonraki IO adımları tekrar çalıştırıldı (short_original_input_missing.txt; io_resume.txt). Bozuk şema fixture’ını oluştururken C kabuğunun defensive ayarı ilk kurulum yazımını reddetti; yalnız fixture oluşturma bağlantısında açıkça kapatılarak bütün matris yeniden çalıştı (defer_expanded_fixture_failure.txt). Bu ara hatalar başarı sayılmadı.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1118, 0, 337); açık (1103, 15, 337). C PATH’te; açık anahtardaki 15 başarısızın adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; sınıf ve örnek kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs, yedi A/B, anahtar kapalı. 14 özet C ile aynı. Erteleme açıkken ortak kapanışı atlayan hızlı okuma yollarından normal yola dönülmesi semantik doğruluk içindir; hız ayarı yapılmadı. Bu ölçüm M3b açık anahtar hızının bittiğini göstermez.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.31      8.81     0.946x     9.25     8.72     9.60    0.92x  OK
W02_create_indexes                5.66      5.66     1.000x     5.62     5.63     8.76    0.65x  OK
W03_pk_point_select              30.20     30.15     0.998x    29.54    29.52    23.30    1.29x  OK
W04_secondary_idx_range           9.47      9.75     1.029x     9.42     9.63     7.46    1.31x  OK
W05_full_scan_filter_agg          4.39      4.35     0.989x     4.38     4.34    16.57    0.26x  OK
W06_join_group_order             10.81     10.95     1.013x    10.77    10.94    28.23    0.39x  OK
W07_window_functions             37.34     38.14     1.021x    36.96    37.35    50.21    0.76x  OK
W08_indexed_update_delete        12.09     11.95     0.988x    11.91    11.88     7.96    1.50x  OK
W09_like_text_scan               12.61     12.64     1.003x    12.43    12.48    18.92    0.67x  OK
W10_jsonb_extract_agg            13.74     13.72     0.999x    13.64    13.59    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.47     70.01     1.008x    68.57    68.92    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.86     40.85     1.000x    40.06    39.61    24.63    1.66x  OK
W13_autocommit_wal_txns           4.57      4.64     1.015x     4.45     4.51     4.12    1.13x  OK
W14_cli_sql_script               39.13     53.15     1.358x    38.62    52.67    26.19    2.03x  OK
TOTAL                           299.67    314.76     1.050x   geomean cand/base = 1.023x   digest mismatches = 0
```

## Açık işler

D131 yazıcı sayacı ve genel son ifade kapanışı sıradaki doğruluk işidir. D52 legacy RETURNING, D103, E28, kalan PRAGMA/M4, veri/DDL/JSON5/UTF/CLI farkları ve eski yürütücünün kaldırılması açıktır. Genel legacy opcode/ilerleme izleri, bütün callback’ler, negatif/karma FK sayaç durumları ve bellek hata enjeksiyonu kanıtlanmış sayılmaz. Tam C veya yayın garantisi verilmez.

Kanıt /home/hakan/github/sqliters/target/perf-work/v6b25/. Sekiz kaynak/test dosyası res/source_manifest.json ile kayıtlı; son C satır numarası yorum düzeltmesinin önceki hash’i ve yalnız yorum diff’i res/source_manifest_before_comment_fix.json ile res/comment_reference_correction.diff içindedir. Kod gövdesi veya satır sayısı değişmedi; önceki rapordaki ölçüm açıklamasının düzeltmesi ürün kaynağı değişikliği değildir. C kaynakları değiştirilmedi; ileriPlanlama değerlendirilmedi.
