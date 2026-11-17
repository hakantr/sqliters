# V6b.30 — D139 bağlı LIKE yeniden üretimi

Bağlı değer kaynağı, maskeli bind/clear expire kararı, adlı parametrenin dummy Variable komutu ve sayaç aktarımı eklendi. D139 başlangıç matrisindeki 48 farkın 40’ı giderildi. Kalan UTF-16 NOCASE ve yeni sınıflanan Unicode/NUL yolları açık olduğundan D139 veya bütün V6b tamamlandı sayılmaz. Rust 1.100; hız ayarı yapılmadı.

## C karar eşliği

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| whereexpr.c:207–218 | TK_VARIABLE, QPSG kapalı | Eski ifadenin bağlı değerini BLOB affinity ile alır; yalnız TEXT önek sağlar. NULL/BLOB/sayı öneke çevrilmez. İlk prepare eski ifade olmadığı için değer taşımaz. | `crates/sqliters-codegen/src/whereexpr.rs:291` |
| vdbeaux.c:5319–5334 | Eski ifade / NULL / kopya | Host::bound_value 1 tabanlı parametreyi kopyalar; NULL None olur. C LIKE çağrısındaki BLOB affinity sınıfı değiştirmez. Genel affinity ve NOMEM eşliği bu dilimin iddiası değildir. | `crates/sqliters-core/src/cgen.rs:102` |
| whereexpr.c:313–329 | Tam önek ve adlı/numaralı değişken | LIKE kalıntısı kaldırılırsa adı koruyan dummy OP_Variable üretilir; geçici register alınır, P3=0 yapılır ve register bırakılır. ?1/?32/?33/:pat için program ve VM adım farkı kapandı. | `crates/sqliters-codegen/src/whereexpr.rs:379` |
| vdbeapi.c:1751–1768 | expmask ve yeniden bind | Başarılı bind/unbind planı expire eder; aynı değeri veya NULL’ı tekrar bağlamak da buna dahildir. 32 ve daha üstü değişken ortak yüksek biti kullanır. LIKE bSmallint=0’dır; genel smimask davranışı taşındı sayılmaz. | `crates/sqliters-core/src/lib.rs:7374` |
| vdbeapi.c:158–181 | clear_bindings | Değerler NULL olur ve maske varsa expire edilir. ROW sırasında mevcut cursor/yürütme konumu korunur; yeni derleme sonraki yürütmede olur. | `crates/sqliters-core/src/lib.rs:8511` |
| vdbeaux.c:73–75 | V1 hazırlama | C SAVESQL kapalıyken maskeyi temizler. Rust V1 için etkin bind maskesini sıfır kabul eder; V1’de bound-value otomatik yeniden üretimi yapılmaz. Genel V1 ham Program metadata/flags eşliği ayrıca açık. | `crates/sqliters-core/src/lib.rs:7366` |
| vdbeaux.c:146–153 | VdbeSwap sayaçları | Başarılı yeniden derleme sayaçları korur ve REPREPARE bir artırır. Makineye ek olarak EXPLAIN’in sayacı da saklanır. Başarısız reprepare başarı olarak sayılmaz. | `crates/sqliters-core/src/lib.rs:7431` |
| prepare.c:sqlite3Reprepare; whereexpr.c:isLikeOrGlob | Programı bağlı değerle üret | SELECT ve EXPLAIN/EQP programı önceki parametreleri readonly Host üzerinden alır. Yeni program eski parametreleri ve isim bağını korur; yürütme maskeli parametre nedeniyle otomatik legacy fallback yapmaz. | `crates/sqliters-core/src/cgen.rs:479` |
| vdbe.c:sqlite3VdbeList | EXPLAIN/EQP bağlı öneği | İndeks aralığı ve dummy Variable dahil yeniden üretilmiş program listelenir. Liste gerçek yürütmeyi başlatmadan C’nin açıklama ömrünü korur. | `crates/sqliters-core/src/cgen.rs:483` |
| vdbeapi.c:vdbeUnbind / sqlite3_clear_bindings | Bütün Rust bind girişleri | bind, bind_text, bind_named, bind_all ve gate içi/fast yolları aynı maske kararını kullanır. Hazır/busy/range denetimlerinin sırası korunur. Genel bind hata metni/uzunluk kusurları kapanmış sayılmaz. | `crates/sqliters-core/src/lib.rs:8350` |

## Doğrudan C karşılaştırması

48 başlangıç native akışı: C gözlemleri önceki ham kayıtla aynı; 40 tam eşit, 8 UTF-16le/be + case_sensitive_like=OFF + NOCASE indeksinde legacy status farkı sürer. 48 akışın tümünde satır/hata/değişiklik/autocommit ve bütün ana dosya baytları aynıdır. Sekiz fark normalize edilmedi veya başarı sayılmadı.

144 parametreli EXPLAIN/EQP akışı önceden 48 TEXT deseninde farklıydı; şimdi 144/144 aynı. NULL ve BLOB öneke çevrilmez. 48 bind ömrü akışındaki 36 eski fark kapandı: NULL tekrar bind, hazırken clear ve ROW sırasında clear; hiç bind edilmeden tekrar yürütme korunur.

84 V1/V2 ve parametre 1/32/33/EQP status akışı tamamen aynı. V1 maskesi, yüksek bit paylaşımı, tekrar TEXT/NULL/BLOB ve EXPLAIN REPREPARE denetlendi. Adlı/numaralı değişkende eklenen Variable, 24 akışta bulunan VM_STEP farkını giderdi. İki public regression testi bind_text/named/all, ROW sırasında clear, tekrar yürütme, bound EQP ve V1 sınırını korur.

144 **tam program** akışında ilk yürütme sırasında yeniden üretilmiş gerçek program C’nin 14 başlık alanı, bütün opcode/P4 ve altprogram biçimiyle aynı. Özel gözlemci standart `GEN=1` yerine normal API üzerinde gerçek statement programını alır; test için verilen sabit programla yürütme kullanılmaz. C başvuru gözlemcisinin 42 compile option satırı önceki oracle profiliyle birebir doğrulandı. `c_program_for_testing` yalnız immutable tanı erişimidir. Bu full-dump matrisi ?1/:pat/?33 ve TEXT/leading-wildcard/NULL/BLOB içerir; V1 ham metadata eşliği olarak sunulmaz.

Bu yeni karşılaştırmalar iki anahtar, üç kodlama ve bellek/disk üzerinde çalışır. Önceki 1116 LIKE native akışı ve önceki PRAGMA/pager/DDL/FK/callback sonuçları korunur.

## Açık farklar ve sıradaki doğruluk işi

**D139:** ana 48’lik matriste kalan 8 NOCASE UTF-16 yolu makine tarafından reddedilir. Kodlama/collation geçişi sonraki adımdır. Ek sınır-türleri matrisinde 24 akışın 4’ü tam eşittir; UTF-8’de geçersiz UTF-8 üst sınırı üretici Unsupported yapabilir ve legacy sayaçları C’den ayrılır. GLOB bound üretimi, genel DML/vtab reprepare ve bütün API limit/hata yolları bu dilimde kapandı sayılmaz.

**D140:** ayrı başlangıç yoklamasında 12/12 NUL/NOCASE akışında sıra/eşitlik/DISTINCT farkı doğrulandı. C sqlite3StrNICmp NUL’da durup sonra byte uzunluğuna bakar; Rust’ın eski yolu sonrasını karşılaştırır. V6b.29 ve bu adımın Rust çıktıları aynı; yeni regresyon değildir. Bütün dosyalar eşit. Ham kayıtlar `../v6b31/res/nul_before30.json` ve `nul_current.json` içindedir.

**D141:** sınır-türleri matrisinde UTF-16BE’nin 8 akışında veri farkı vardır. Örnek `߿%` için C’nin byte artırımı geçersiz UTF-8 üst sınırı oluşturur; UTF-16BE dönüşümü ve LIKE kalıntısının kaldırılması sonucu C `ࠀ` satırını da döndürür. Rust bu sınırı tutamayıp legacy’ye döner. V6b.29 ve bu adımda veri gözlemleri birebir aynı; C’nin bu çıktısı da eşlik hedefidir. `bound_edge_before30.json` ve `bound_edge_types.json` normalleştirilmeden saklanır. Yeni kodlama düzeltmesi ham token baytlarını ve C dönüşümünü kapsamalı.

D52/D103/E28, F4/F6–F9 beklentileri ve kalan PRAGMA/M4 işleri sürer. Geçersiz UTF-8 TEXT’in genel değer temsili, NOMEM/tahsis izi, UTF-16 karşılaştırma ve bütün callback/thread koşulları ayrıca açıktır.

## Dört kapı

Çalışma alanı kapalı **(1123, 1, 337)**, açık **(1109, 15, 337)** (geçti, başarısız, ignored). Başarısız isim kümeleri V6b.29 ile aynı; F4 kapalıdaki tek başarısızlık olarak korunur, beklenti değiştirilmedi. DML **378/400** ve **398/400**, sınıf/örnekler aynı. Finder stdout **245/245**, stderr **244/245**; iki eski referans sınıf farkı korundu. Rust 1.100 tabanına göre toplam **1.227x**, W09 **5.009x**, W14 **1.372x**; 14 C digest farkı sıfır. W09’un önceki adımda görülen makine maliyeti sürer; performans düzeltmesi yapılmadı.

Genel tam programlar **13440 aynı / 72 Unsupported**; EXPLAIN **15938**, EQP **15938**, yürütme **11711** aynı. Üretici öncesi 64 hata aynı. Önceki defer akışlarında 20 D52 ve manual API’de tek D103 açık; bütün ilgili dosyalar eşittir.

## Kapsam

GetBoundValue 10/10 satır, 5/6 dal; kalan dal tahsis hatasıdır. SetSql 7/7 satır ve 4/4 dal; bu sayı genel SQL sahipliği/NOMEM kanıtı değildir. isLikeOrGlob 71 satırın %71.8’i ve 80 dalın %53.8’i; genel sayı/bozuk literal/allocator yönleri açık. **776 alınmayan dal** [ayrı tabloda](karar_esligi_v6b_30_dallar.md) tek tek sınıflandırıldı. Yeni API ölçümleri vdbeapi.c:1751–1768,170–181 ve vdbeaux.c:146–153 ile sınırlıdır; bütün bind/swap yordamları kabul edildi sayılmaz.

# C-side coverage of the functions ported or completed in step V6b.28 (gcov, -O0 build of the reference amalgamation)

Corpus: the 16063 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Çalışan satır | En az bir kez alınan dal |
|---|---|---|
| pragma.c | 911 / 1142 (79.8%) | 601 / 794 (75.7%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 324 / 380 (85.3%) | 220 / 306 (71.9%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 128 / 142 (90.1%) | 69 / 86 (80.2%) |
| vdbeaux.c | 115 / 142 (81.0%) | 46 / 68 (67.6%) |
| callback.c | 70 / 74 (94.6%) | 34 / 38 (89.5%) |
| prepare.c | 206 / 259 (79.5%) | 131 / 192 (68.2%) |
| tokenize.c | 59 / 76 (77.6%) | 37 / 54 (68.5%) |
| util.c | 92 / 97 (94.8%) | 76 / 84 (90.5%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 46 / 61 (75.4%) | 17 / 32 (53.1%) |
| vdbeapi.c | 10 / 19 (52.6%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 31 / 34 (91.2%) | 26 / 36 (72.2%) |
| utf.c | 51 / 64 (79.7%) | 62 / 74 (83.8%) |
| hash.c | 74 / 74 (100.0%) | 42 / 44 (95.5%) |
| expr.c | 20 / 22 (90.9%) | 16 / 24 (66.7%) |
| ctime.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| func.c | 91 / 121 (75.2%) | 88 / 130 (67.7%) |
| fkey.c | 212 / 273 (77.7%) | 120 / 189 (63.5%) |
| whereexpr.c | 51 / 71 (71.8%) | 43 / 80 (53.8%) |
| **total** | 2875 / 3500 (82.1%) | 1796 / 2459 (73.0%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 76.4 | 675 | 73.0 | 108 |
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
| sqlite3PagerRollback | pager.c | 18 | 100.0 | 16 | 81.2 | 3 |
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
| sqlite3MultiValues | insert.c | 61 | 75.4 | 32 | 53.1 | 12 |
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
| sqlite3LocateCollSeq | callback.c | 8 | 100.0 | 6 | 83.3 | 1 |
| createCollation | main.c | 28 | 89.3 | 24 | 70.8 | 5 |
| sqlite3GetCollSeq | callback.c | 14 | 100.0 | 14 | 100.0 | 0 |
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
| sqlite3_get_autocommit | main.c | 5 | 100.0 | 0 | 100.0 | 0 |
| sqlite3RegisterLikeFunctions | func.c | 12 | 100.0 | 4 | 100.0 | 0 |
| likeFunc | func.c | 25 | 100.0 | 20 | 100.0 | 0 |
| patternCompare | func.c | 76 | 60.5 | 104 | 59.6 | 16 |
| isLikeOrGlob | whereexpr.c | 71 | 71.8 | 80 | 53.8 | 20 |
| sqlite3VdbeGetBoundValue | vdbeaux.c | 10 | 100.0 | 6 | 83.3 | 1 |
| sqlite3VdbeSetSql | vdbeaux.c | 7 | 100.0 | 4 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 203868 | 2/2 |
| parse.y:524 | 147562 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yedi adlandırılmış opcode ve Noop/Explain ortak default bloğu (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 16/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_ResultRow | 2/4 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |
| OP_Noop_OP_Explain_default | 0/0 |

## V6b.30 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
| vdbeapi.c:1751–1768 | 6/7 | 6/12 |
| vdbeapi.c:170–181 | 8/8 | 3/4 |
| vdbeaux.c:146–153 | 5/5 | 0/0 |
| vdbeapi.c:432–434 | 2/3 | 1/2 |
| vdbeapi.c:666–668 | 2/3 | 1/2 |
| main.c:2047–2069 | 8/9 | 10/12 |
| vdbe.c:1852–1912 | 32/40 | 14/26 |
| vdbe.c:1617–1628 | 6/8 | 1/4 |
| vdbe.c:3328–3382 | 12/23 | 4/24 |
| vdbe.c:4074–4090 | 12/12 | 6/6 |
| vdbe.c:4186–4204 | 10/13 | 6/6 |
| vdbeaux.c:2997–3002 | 4/4 | 6/6 |
| vdbeaux.c:3554–3554 | 1/1 | 2/2 |
| main.c:2760–2763 | 2/2 | 2/2 |
| insert.c:1011–1011 | 1/1 | 2/2 |
| delete.c:421–421 | 1/1 | 2/2 |
| update.c:608–608 | 1/1 | 2/2 |
| vdbeaux.c:3613–3613 | 1/1 | 2/2 |
| vdbeapi.c:881–886 | 5/5 | 4/4 |
| vdbeaux.c:3330–3404 | 20/22 | 16/26 |
| vdbeaux.c:3405–3516 | 46/63 | 42/60 |
| vdbeapi.c:990–1023 | 14/17 | 7/10 |
| vdbe.c:3136–3147 | 0/2 | 0/4 |
| vdbe.c:3210–3224 | 7/7 | 10/12 |
| vdbe.c:3393–3402 | 4/6 | 1/2 |

## Sonuç dosyaları

Özel doğrulama dizini `/home/hakan/github/sqliters/target/perf-work/v6b30`. `res/source_manifest.json`, `validation.txt`, `tests_off/on.txt`, `bound_*`, `bind_*`, `full_C_profile.txt`, `programs.jsonl`, `explain/eqp/flow*.json`, DML/finder/speed/kapsam ham kayıtları saklanır. İlk özel gözlemci denemelerindeki dosya açma ve C başlık/derleme ayarı hataları ayrı loglarda korunur; son sonuç sayılmaz.

```json
{
  "crates/sqliters-codegen/src/sqliteint.rs": "0a41652f8bb16ed32195898c2ddc5807dcd34ea7b3ca674720f420e08826c216",
  "crates/sqliters-codegen/src/whereexpr.rs": "3ff0f60bb4093281a239b9f4f8a3fff5d2ad4dec78daeb23689e7a16f96903c3",
  "crates/sqliters-core/src/cgen.rs": "bc85282da4485c63363ff2f033cd444ff3e956112cbae087af995ee228684d99",
  "crates/sqliters-core/src/lib.rs": "29ed94cbd4ea164b0b0d1740a10d824d34a54ceacc6b9923782704cff7d5d8a8",
  "crates/sqliters-core/src/vdbe_host.rs": "25dd9e0ee25f80f290aa1bbb74768beb555e2e29ecc3ee596d236e5a10af9857",
  "tests/v6b_like_bind_reprepare.rs": "9ce2192049fbb8e63da4c5d195adf9e52d96d3dedcd7015cb7ffe17f10d7caa8"
}
```
