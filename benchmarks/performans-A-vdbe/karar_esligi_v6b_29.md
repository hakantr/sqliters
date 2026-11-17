# V6b.29 — case_sensitive_like hazırlama etkisi ve LIKE kayıtları

PRAGMA kod üreticiye taşındı. Bağlantının iki LIKE kaydı, BUSY/expire ayrımı, paylaşılan builtin bayrakları ve C desen eşleştirme kuralları hizalandı. Makineye açılan UTF-16 yolunda Column/Concat/işlev-sonuç kodlaması düzeltildi. Rust 1.100; hız ayarı yapılmadı. V6b veya bütün motor tamamlandı sayılmaz.

## C karar eşliği

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| pragma.c:1664–1667 | zRight var/yok | Getirici satırsızdır. RHS varsa boolean 0 varsayımıyla iki LIKE işlevi hazırlanırken yeniden kaydedilir; step/reset ayarı yeniden uygulayan ayrı bir işlem kodu değildir. | `crates/sqliters-codegen/src/pragma.rs:393` |
| func.c:2367–2377 | caseSensitive ve nArg=2,3 | İki arity bağımsız UTF-8 kayıtlarıdır. ON: LIKE|CASE; OFF: LIKE. Kullanıcı scalar/aggregate tanımı başarılı kayıtta LIKE uygulamasıyla değiştirilir. | `crates/sqliters-core/src/execution_context.rs:325` |
| main.c:2053–2060 | Tam arity/UTF-8 eşleşmesi ve nVdbeActive | Tam kayıt etkin ifadede BUSY olur; kayıt değişmez. Etkin ifade yoksa önce hazırlanmış ifadeler expire olur. Variadic kayıt üzerine yeni tam arity eklemek aynı karar değildir ve expire yapmaz. | `crates/sqliters-core/src/execution_context.rs:332` |
| func.c:2377–2384 | CreateFunc sonucu yok sayılır | BUSY sonrasında bulunan eski kullanıcı işlevi veya paylaşılan builtin üzerinde LIKE/CASE bitleri OR edilir, UNSAFE silinir. Callback ve eşleştirme modu değişmeden metadata değişebilir; CASE geçmişi her arity için ayrıdır. | `crates/sqliters-core/src/execution_context.rs:334` |
| func.c:2379–2384; callback.c:sqlite3FindFunction | Bağlantılar arasında builtin bayrakları | BUSY yerleşik LIKE bayrağını değiştirirse yeni bağlantı da BINARY aralık planını görebilir. Yerleşik eşleştirici case-insensitive kalır. Rust aynı paylaşılan bayrak geçmişini arity başına atomik alanda tutar. | `crates/sqliters-codegen/src/callback.rs:16` |
| callback.c:466–470 | Yeni FuncDef adı / zincir sırası | C yeni ismi ASCII küçük harfe çevirir; yeni overload zincirin başına gelir. LIKE kayıt adı ve arity sırası bu kuralla hizalıdır; genel kullanıcı işlev API eşliği bu dilimin iddiası değildir. | `crates/sqliters-core/src/execution_context.rs:216` |
| func.c:951–959 | nPat sınırı önce | Desen 50000 baytı aşınca escape/NULL denetiminden önce aynı hata döner. BLOB uzunluğu ham bayt sayısıdır. Dinamik limit API ve genel NOMEM ayrıca kanıtlanmadı. | `crates/sqliters-vdbe/src/func_c.rs:235` |
| func.c:961–980 | Üçüncü argüman / escape | NULL escape NULL döndürür. C-string içinde tam tek UTF-8 karakteri gerekir; % veya _ escape ise ilgili joker kapatılır. Escape u32 değer olarak tutulur; Rust char alanına daraltılmaz. | `crates/sqliters-vdbe/src/func_c.rs:249` |
| func.c:981–989 | Desen veya metin NULL | Metne çevrilemeyen NULL değer NULL verir. Desen ve metin ilk NUL baytında biter; veritabanı kodlaması BLOB-metne dönüşümünde korunur. | `crates/sqliters-vdbe/src/func_c.rs:259` |
| func.c:745–819 | % arama ve geri dönüş | Ardışık jokerler, tek karakter tüketimi, escape sonrası çapa ve ASCII bayt/Unicode karakter araması C sırasını izler. En son jokerin araması tükenince C NOWILDCARDMATCH daha eski çağrılara döner; Rust aynı sonucu tek retry noktasıyla yinelemeli verir. | `crates/sqliters-vdbe/src/func_c.rs:161` |
| func.c:821–867 | Literal, escape ve _ | C UTF-8 okuyucusunun bozuk byte tüketimi ve ASCII case-fold kuralı korunur. GLOB karakter sınıfları yeni LIKE yordamının parçası değildir; GLOB eşliği ayrıca açıktır. | `crates/sqliters-vdbe/src/func_c.rs:161` |
| vdbe.c:1626,3337,3353 | BLOB ve Column kodlaması | Literal BLOB ve normal/overflow sütun sonucu veritabanı encoding alanını taşır. UTF-16 satır baytlarını UTF-8 sayma hatası giderildi; uzun metin overflow yolu odaklı testte denendi. | `crates/sqliters-vdbe/src/vdbe.rs:2940` |
| vdbe.c:1870–1909 | Concat kodlaması ve boyut | Sayı girdisi DB kodlamasına stringify edilir; ham bayt birleştirme, uzunluk denetimi ve UTF-16 tek kalan baytı kesme sırası C ile aynı. Sonuç encoding=database olur. | `crates/sqliters-vdbe/src/vdbe.rs:2379` |
| vdbeapi.c:432–434,666–668 | Scalar metin sonucu | sqlite3_result_text/value pCtx->enc dönüşümüne karşılık makine işlev metin sonucunu DB kodlamasına çevirir, ardından boyutu denetler. Genel 1 GiB/NOMEM enjeksiyonu açık. | `crates/sqliters-vdbe/src/vdbe.rs:5075` |
| whereexpr.c:LIKE optimizasyonu; func.c:sqlite3IsLikeFunction | LIKE kalıntısı kaldırılabilir | Aralık planında Function işlem kodu kalmasa da LIKE çözümlemesi references_like ipucunu taşır. Bu Rust yönlendirme bilgisidir, C Vdbe alanı değildir. İki anahtar durumunda uygun okuma makinede yürür. | `crates/sqliters-core/src/vdbe_host.rs:105` |

## Doğrudan C karşılaştırması

Başlangıçtaki 288 PRAGMA akışının 96 farkı giderildi. Hazırlama/step, tekrar ON/OFF, boolean sözcük ve sınırlar, etkin okuma ve EXPLAIN ömrü bu derlemdedir. Etkin okuyucunun indeks seçimi için 12 ayrı akış; scalar exact/variadic kayıt için 144; kayıt adı/EXPLAIN/function_list için 288; aggregate ve variadic-scalar + exact-aggregate için 192 akış tamamen aynıdır.

24 desen akışı her biri 3468 sorgu yürütür: toplam 83232 sorgu gözlemi. NULL, NUL, escape %, _, çok baytlı escape, geçerli Unicode, ham geçersiz UTF-8 BLOB ve 49999/50000/50001 sınırları karşılaştırıldı. Ek 12 Unicode arama/kısa devre akışı aynıdır. 72 UPDATE/DELETE/INSERT SELECT/CHECK/Concat akışı ve 48 geç hazırlama hatası/yanlış schema/EQP akışı da aynıdır. Native matrislerin her biri iki anahtar, üç kodlama ve bellek/disk üzerinde çalışır.

F4 için 36 ek API akışı C ile tam aynıdır. Toplam yeni **1116 katı native akış** aynı sonuç, hata/konum, izlenen sayaçlar ve bütün ana veritabanı dosya baytlarını verir. 1020 temel + 12 ek akış ayrıca C gcov sayacını besledi. Hazırlanan ifade ömrü ve global builtin bayrağının yeni bağlantıya taşınması anlamlı public regression testinde de doğrulandı. C global gözlemi `res/like_global.txt` içindedir; eşzamanlı çok bağlantı/thread yarışlarının tamamı kanıtlanmış sayılmaz.

Tam program karşılaştırma formatı önceki 14 C başlık alanını ve bütün işlem kodu/P4/altprogramı korur. references_like C alanı olmadığı için bu dump başlığına katılmaz; LIKE çözümleme ve kalıntısı olmayan indeks planı native gözlemlerle ayrıca denetlendi.

## Açık kusur ve test sınırı

**D139 açık:** 48 bağlı desen akışının tamamında sqlite3_stmt_status farklıdır. Satırlar, hatalar, değişiklik/autocommit gözlemleri ve bütün dosyalar aynıdır; sayaç farkı başarı sayılmaz. Üreticide bound-value kaynağı yoktur; expmask parametresi bağlıysa mevcut c_machine_reprepared_by_bind yolu eski yürütücüye düşer. Ham `res/like_bound.json` saklanır; `like_bound_classified.json` yalnız farkın status satırlarında olduğunu doğrular, bunları eşit diye yeniden etiketlemez. Sıradaki doğruluk işi bound-value ile C gibi yeniden üretme ve sayaç eşliğidir.

**F4 korunur:** kapalı koşumdaki ek başarısızlık test_group3_entry_gate_reentrancy_interrupt_mirrors_pending_drops_and_poison_recovery testidir. Etkin ifade yokken interrupt bayrağını sonraki sorguya yapışık bekler. Aynı test önceki açık koşumun bilinen 15 kaydı içindeydi; C sqlite3Step etkin Vdbe yokken interrupt durumunu temizler. 36 API karşılaştırması C davranışını doğrular. İstenildiği gibi legacy kaldırılmadan beklenti değiştirilmedi; test geçiyor diye sunulmadı.

D52 (20/1644 defer sayaç farkı), D103 (manual akışta tek legacy çıktı farkı), E28 ve kalan PRAGMA/M4 işleri açıktır. UTF-16 NOCASE/RTRIM anahtarları, özel collation/authorizer yönlendirmesi, genel geçersiz UTF-8 TEXT/UTF-16 sınırları, yüklenebilir extension ve genel function kayıt API hataları tamamlandı sayılmaz. Genel LIKE/GLOB dinamik limit API, tahsis hata izi ve bütün eşzamanlı callback yaşamı ayrıca kanıtlanmadı.

## Dört kapı

Çalışma alanı kapalı: **(1121, 1, 337)**; açık: **(1107, 15, 337)** (geçti, başarısız, ignored). Açık başarısızlık kümesi önceki 15 kayıtla aynı; kapalıdaki tek fark yukarıdaki F4’tür. Test beklentileri değiştirilmedi. DML **378/400** ve **398/400**, sınıflar ve örnekler V6b.28 ile aynı. Finder stdout **245/245**, stderr **244/245**; iki eski temel sınıflandırma farkı korundu. Rust 1.100 tabanına göre toplam **1.207x**, W14 **1.368x**; 14 iş yükünde C digest farkı sıfır. W09 **4.773x**: önceki kapalı yürütücünün hızlı LIKE yolu yerine makineye yönlendirme önemli gerileme getirdi. Önceki toplam 1.058x idi; bu ölçümde gerileme saklanmadı. Doğruluk işleri önceliklidir; makine maliyeti M3b aşamasında ele alınacak. Bu ölçüm hız düzeltmesi veya resmi son ölçüm değildir.

Tam program: **13440 aynı / 72 açık Unsupported**. EXPLAIN **15938**, EQP **15938**, yürütme **11711** aynı; üretici öncesi 64 hazırlama hatası aynı. Önceki PRAGMA/pager/FK/callback dosya ve hata matrisleri korundu.

## Kapsam ve alınmayan dallar

LIKE kayıt döngüsü 12/12 satır ve 4/4 dal; likeFunc 25/25 satır ve 20/20 dal. Ortak patternCompare 60.5% satır, 61.5% dal: GLOB karakter sınıfları da bu C yordamında olduğu için oran LIKE tamamlandı anlamına gelmez. Bütün birikimli kapsamda **729 alınmayan dal** [ayrı tabloda](karar_esligi_v6b_29_dallar.md) tek tek kaydedilir. LIKE profilinde girilemeyen GLOB yönleri, açık işleyiciler ve enjekte edilmemiş hata yolları ayrıdır. Kapsamdan çıkarılıp yüzde yükseltilmedi.

CreateFunc, Column, Blob, Concat ve result encoding yalnız belirtilen C satır bölgeleriyle ölçülür; bütün API/Vdbe/Halt tamamlandı sayılmaz. C kaynak eşlemesi fonksiyon/case gövdesinde yapılır; rstrip yalnız kaynak konum eşlemesine uygulanır, karşılaştırılan sonuçlara uygulanmaz.

# C kapsamı — V6b.29, başvuru amalgamı, -O0 ve gcov

Derlem: 16063 kabul edilen ifade, EXPLAIN ve EXPLAIN QUERY PLAN olarak, şema kurulumlarıyla çalıştırıldı.

| C dosyası | Çalışan satır | En az bir kez alınan dal |
|---|---|---|
| pragma.c | 911 / 1142 (79.8%) | 601 / 794 (75.7%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 324 / 380 (85.3%) | 220 / 306 (71.9%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 128 / 142 (90.1%) | 69 / 86 (80.2%) |
| vdbeaux.c | 98 / 125 (78.4%) | 37 / 58 (63.8%) |
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
| func.c | 91 / 121 (75.2%) | 90 / 130 (69.2%) |
| fkey.c | 212 / 273 (77.7%) | 120 / 189 (63.5%) |
| **total** | 2807 / 3412 (82.3%) | 1746 / 2369 (73.7%) |

| C yordamı | Dosya | Satır | Satır % | Dal | Alınan % | Alınmayan dallı kaynak satırı |
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
| patternCompare | func.c | 76 | 60.5 | 104 | 61.5 | 14 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 202320 | 2/2 |
| parse.y:524 | 147034 | 2/2 |

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

## V6b.29 — özel hata ve yeniden hazırlama blokları

Bu bölgeler tüm sqlite3VdbeHalt/sqlite3_step veya OP_Column yordamları olarak sayılmadı.

| C bölgesi | Satır | Alınan dal |
|---|---|---|
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

Özel doğrulama dizini: `/home/hakan/github/sqliters/target/perf-work/v6b29`. `res/source_manifest.json` sınanan ürün dosyalarını SHA-256 ile sabitler. `res/tests_off.txt`, `tests_on.txt`, `validation.txt`, `programs.jsonl`, `explain/eqp/flow*.json`, `dmlfuzz_last_*.json`, `finder_last.txt`, `speedbench.txt`, `partial_coverage.json` ve bütün `like_*.json` ham sonuçları saklanır. Önceki yanlış deneme/harness profil çakışması çıktıları silinmedi. Eski isimli kapsam raporunu yazan özel betik düzeltildi; V6b.28 dal raporu doğrulanmış HEAD’den bayt bayt geri kondu.

```json
{
  "crates/sqliters-codegen/src/callback.rs": "340c750845e0719be788bfac076e9a51acf16a5640fe1f6c298d992f0a59d7cf",
  "crates/sqliters-codegen/src/pragma.rs": "804ca40b41d6aba53f5c519cade618fa9490f87b4ab19e39164488512e5e55ac",
  "crates/sqliters-core/src/cgen.rs": "bc7437d71808321cdcb853befe6f1d87c2eddc0df85e95675da11795c2540160",
  "crates/sqliters-core/src/execution_context.rs": "31e54348abd1bdb0e5ea45da85703370fc5838ae786d2fb0ee7e9d5187bee08e",
  "crates/sqliters-core/src/lib.rs": "13152999ac15379b01143e173e0ca4a8c3099911d34ed89ff130180011df12c7",
  "crates/sqliters-core/src/vdbe_host.rs": "ef463a8eb32d90453d889b2da7e081626e80dbdc397ccf6aa8c89104aa441984",
  "crates/sqliters-vdbe/src/func_c.rs": "f61f0e2117ce74f391da1778dd24458ddc29f0d71077cc714974c7b5ca56d1cf",
  "crates/sqliters-vdbe/src/lib.rs": "96511e40f3b3e803269d6555a7e93fe1e065225bda5bdec02495ff4c92357863",
  "crates/sqliters-vdbe/src/vdbe.rs": "e9a475e2bc6d4de1ab119a6445fbdcb1815cd4211fecd14ad29227fb7781d02c",
  "crates/sqliters-vdbe/src/vdbeaux.rs": "828348a8b081552cd38b0bdd65b54000d78dd1325937beed4ed3e75165370d0a",
  "tests/v6b_case_sensitive_like.rs": "ad183766d509781befaac8f1a421c2e28fc7ec17575af0323194ed1b5aef7788"
}
```
