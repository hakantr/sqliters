# V6b.31 — LIKE UTF-16, NOCASE ve ham aralık baytları

D139’un kalan UTF-16/NOCASE akışları, D140’ın ölçülen NUL karşılaştırma/gruplama farkları ve D141’in ham Unicode aralık farkı giderildi. Açılan UTF-16 yürütme yolunda saptanan TEXT/BLOB CAST ve metinden sayıya dönüşüm farkları da düzeltildi. Bu sonuç bütün V6b, bütün UTF/API davranışları veya yayın eşliği tamamlandı anlamına gelmez. Hız ayarı uygulanmadı.

## C karar eşliği

| C koşulu / kaynak | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| whereexpr.c:1435–1473 | LIKE öneğinin son baytını artırma | C zToken içindeki ham baytları saklar; UTF-8 geçersizleşse de aralık bırakılmaz. Rust Expr ham token alanı, kopyalama, eşitlik ve String8 üretimi bu baytları korur. | `crates/sqliters-codegen/src/sqliteint.rs:174` |
| expr.c:5238–5242; vdbe.c:1475–1506 | Dizgi opcode yüklemesi | String8 UTF-16 dönüşümü Rust Unicode karakterleriyle değil C utf.c bayt kurallarıyla yapılır. D141 örneğindeki C çıktısı korunur. | `crates/sqliters-vdbe/src/vdbe.rs:2255` |
| main.c:3577–3586 | Yerleşik collation kaydı | BINARY istenen kodlamadadır; NOCASE ve RTRIM yalnız UTF-8 kaydı taşır. UTF-16 anahtarları karşılaştırıcıya geçerken UTF-8’e dönüştürülür. | `crates/sqliters-codegen/src/callback.rs:25` |
| vdbeaux.c:4426–4464 | Hücre kodlaması / collation kodlaması | Aynı kodlamada doğrudan karşılaştırma; gerektiğinde iki girdinin baytları collation kodlamasına çevrilir. Geçici kayıt anahtarları da KeyInfo.enc kullanır. | `crates/sqliters-vdbe/src/vdbemem.rs:732` |
| main.c:1116–1128; util.c:442–454 | NOCASE NUL ve uzunluk | ASCII harfleri küçükleştirip ortak uzunluğa kadar karşılaştırır; eşit NUL’da durur; sonra tam UTF-8 bayt uzunluklarına bakar. NUL sonrası bayt içeriği kıyaslanmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:122` |
| main.c:1116–1128 | Eski yürütücü / hızlı karşılaştırma | Aynı NOCASE kararı ortak Value karşılaştırmasına ve hızlı byte karşılaştırıcısına uygulandı. GROUP BY hash anahtarı NUL öneği + tam uzunluk kullanır; eşit değerlerin hash değerleri aynıdır. NUL içermeyen anahtarların eski hash yolu korunur. | `crates/sqliters-core/src/fast_read.rs:911` |
| utf.c:266–283 | UTF16LE ↔ UTF16BE | Tam iki baytlık birimler takas edilir; tek kalan son bayt C gibi bırakılır. | `crates/sqliters-vdbe/src/utf.rs:52` |
| utf.c:287–300,338–399 | UTF16 → UTF8 | Tek son bayt düşürülür. SQLITE_REPLACE_INVALID_UTF kapalı başvuru profilindeki surrogate maskeleri uygulanır; sonraki birim surrogate olmasa da C’nin birleştirmesi korunur. | `crates/sqliters-vdbe/src/utf.rs:84` |
| utf.c:318–336,164–172,84–107 | UTF8 → UTF16 | READ_UTF8 geçersiz/overlong/surrogate kararları ve yazma maskeleri bayt düzeyinde taşındı. Bu yordamın U+FFFE/U+FFFF ve tek 0x80 davranışı sqlite3Utf8Read veya Rust lossy decoding ile karıştırılmaz. | `crates/sqliters-vdbe/src/utf.rs:66` |
| utf.c:268–272,311–315 | Tahsis hatası | Normal çıktı baytları eşit; C malloc/MakeWriteable hata enjeksiyonu ve Rust Vec tahsis sayısı/NOMEM dönüşü eşitliği açık. Bu dal başarılı sayılmaz. | `crates/sqliters-vdbe/src/utf.rs:64` |
| vdbeapi.c:1804–1809 | Parametre bağlama kodlaması | TEXT veritabanı kodlamasına çevrilir. BLOB baytları çevrilmeden enc veritabanı kodlaması olur; sonraki CAST BLOB kararları aynı bağlamı görür. | `crates/sqliters-core/src/vdbe_host.rs:1294` |
| vdbemem.c:926–970 | CAST NULL/BLOB/TEXT/NUMERIC/INTEGER/REAL | CAST için veritabanı kodlaması iletilir. Sayısal TEXT önce bu kodlamada yazılır; TEXT cast UTF-16’da tek baytı kırpar ve sonra kodlama değiştirir. BLOB cast sayısal metnin veritabanı baytlarını korur. | `crates/sqliters-vdbe/src/vdbemem.rs:425` |
| vdbe.c:397–425 | TEXT affinity sayısal yazımı | Yeni sayısal metin veritabanı kodlamasına çevrilir; var olan metnin dönüşümü CAST sonundaki C sırasına bırakılır. MakeRecord girişinde hücrelerin DB kodlaması sözleşmesi korunur. | `crates/sqliters-vdbe/src/vdbemem.rs:493` |
| util.c:1173–1260 | UTF16 tamsayı ayrıştırma | Tam UTF-16 birimlerinin sıfır yüksek baytları taranır; düşük bayt öneki ayrıştırılır. nonNum, basamaksız girdi ve 2/3 taşma dönüş kodlarının önceliği korunur. | `crates/sqliters-vdbe/src/vdbemem.rs:300` |
| vdbemem.c:677–724 | UTF16 kayan nokta ayrıştırma | ASCII birimler kopyalanır; ilk sıfır olmayan yüksek baytta kesilir ve rc=-100 verilir. Önceki NUL bile yüksek-bayt taramasını ortadan kaldırmaz. | `crates/sqliters-vdbe/src/vdbemem.rs:320` |
| vdbemem.c:893–914; vdbe.c:354–372 | NUMERIC tür seçimi / affinity | Kodlama farkındalığı IntValue, Numerify, alsoAnInt ve computeNumericType yollarına taşındı. C’nin integer/real türü ve float bitleri doğrudan karşılaştırıldı. | `crates/sqliters-vdbe/src/vdbemem.rs:402` |

[Ölçülen bütün UTF/NOCASE/sayısal C dalları](karar_esligi_v6b_31_utf_kararlar.md) ayrıca koşul dosya:satırı, gcov sonucu ve Rust konumuyla listelenir. Alınmayan yönler başarı sayılmaz.

## Doğrudan doğrulama

- 48 bağlı LIKE API akışı tamamen aynı; önceki sekiz UTF-16 NOCASE status farkı kapandı. 24 Unicode/tür sınırı akışı da tamamen aynı; önceki 20 fark kapandı. Bu iki matrisin C gözlemleri V6b.30 kayıtlarıyla değişmedi.
- 144 bağlı EXPLAIN/EQP, 48 bind ömrü ve 84 V1/V2-parametre akışı aynı. 528 gerçek yeniden üretilmiş tam programda 14 başlık alanı, bütün opcode/P4 ve altprogramlar aynı. Normal API üzerinden ilk step programı gözlenir; GEN ile sabit program verilmez.
- 12 NUL/NOCASE akışı ile 72 ek SQL akışı aynı: UNIQUE, sıralama, DISTINCT, eşitlik, hızlı GROUP BY, RTRIM, karma metin kodlamaları, sayısal dönüşüm, NULL ve BLOB CAST. Bütün akışlarda veri, hata, sayaç, autocommit ve ana veritabanı SHA-256 değerleri birlikte karşılaştırılır.
- Beş public regresyon testi ve ham dönüşüm sınırları birim testi eklendi. Önceki 1116 LIKE native akışı ve eski PRAGMA/pager/FK/callback kanıtları korundu.
- Ham hücre matrisi **489145** gözlem: 471496 dönüşüm + 17649 karşılaştırma; fark **0**. Sayısal hücre matrisi **1587945** gözlem: IntValue, RealValue dönüş kodu/float bitleri, NUMERIC tür/değer ve numeric affinity; fark **0**. Her bir/tam iki bayt, 65536 UTF16 birimi, bozuk surrogate çiftleri, NUL ve sabit tohumlu rastgele girdiler dahil. Bu sayı tüm olası girdilerin kanıtı olarak sunulmaz.

C oracle 3.54.1 kaynak kimliği `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`. CLI, program sürücüsü ve coverage API’nin 42 compile option satırı aynı; ham hücre dönüştürücüsü de aynı kaynak baytlarını içerir. Rust araç zinciri 1.100.0.

## C’nin beklenmedik sonucu

`߿%` öneğinin son baytı artırılınca `E0 80` ham üst sınırı oluşur. C bunu UTF-16BE’ye dönüştürüp residual LIKE’ı kaldırınca `߿` yanında `ࠀ` satırını da döndürür. Rust aynı aralığı, programı, satırları ve sayaçları üretir; daha sezgisel görünen bir sonuçla değiştirilmedi.

## Dört kapı

Çalışma alanı kapalı **(1129, 1, 337)**, açık **(1115, 15, 337)** (geçti, başarısız, ignored). Başarısız isimler V6b.30 ile aynı: kapalı tek F4, açık eski 15 beklenti. Beklentiler değiştirilmedi. DML **378/400** ve **398/400**, kusur sınıf/örnekleri aynı. Finder stdout **245/245**, stderr **244/245**; iki önceki referans sınıf farkı korunur. Rust 1.100 tabanına göre toplam **1.217x**, W09 **5.052x**, W14 **1.370x**; 14 C digest farkı **0**. Bunlar ölçümdür; performans kabulü veya C’ye göre hız iddiası değildir.

Genel tam programlar **13440 aynı / 72 Unsupported**, EXPLAIN **15938**, EQP **15938**, yürütme **11711** aynı; üretici öncesi 64 hata aynı. Defer matrisinde eski 20 D52, manual API’de eski tek D103 açık; bütün dosyalar aynı.

## Kapsam ve açık yönler

Ana PRAGMA/üretici ölçümünün **766 alınmayan dalı** [ayrı tabloda](karar_esligi_v6b_31_dallar.md), yeni ham hücre ve CAST/affinity profillerinin **33 alınmayan dalı** [ikinci tabloda](karar_esligi_v6b_31_utf_dallar.md) tek tek sınıflandırıldı. İki derlemenin sayaçları birleştirilmez; aynı C yordamı iki profilde yer alabilir. İki sayının toplamı benzersiz kaynak dalı sayısı değildir.

Makineye UTF-16 anahtar kabulü desteklenen LIKE okuma yoluyla sınırlı. Uygulama collation callback’leri ve genel DML/vtab yolları açılmış sayılmaz. Genel geçersiz TEXT baytlarının RuntimeValue/String ve istemci API temsili, allocator/NOMEM, tahsis izi, bağlama limit/hata yolları açık. Ham byte dönüştürücüsünün eşitliği bu üst API temsillerini eşitlemez.

GLOB bağlı yeniden üretimi, kalan PRAGMA işleyicileri, M4 DDL, E28 pager hata durumu ve D52/D103 sıradaki doğruluk işleri arasında. F4/F6–F9 beklentileri eski yol kaldırılana kadar korunur. Tam C eşliği/yayın garantisi verilmedi.

## Tekrarlanabilirlik ve ham kayıtlar

Özel dizin `/home/hakan/github/sqliters/target/perf-work/v6b31`. `res/source_manifest.json`, çalışma alanı logları, DML/finder, `bound_*`, `bind_*`, `nul_*`, `collation_*`, `primitive_*`, `numeric_*`, programlar ve gcov dosyaları saklanır. Özel harness kurulurken eksik C ikilisi/baseline dosyası, yanlış parametre etiketi ve bir C gözlemci zaman aşımı ayrı loglarda tutuldu. Düzeltilmiş son akışlar bütünüyle yeniden doğrulandı; başarısız/eksik koşular son sonuç sayılmadı.

```json
{
  "crates/sqliters-codegen/src/callback.rs": "8ef8ced29a3979b732cbbc5ab0cbfd25221d6cda53da5d78b8f69f1ed154afde",
  "crates/sqliters-codegen/src/expr.rs": "5030a342884549083089e456875965a33e2404031ec3fb5383260dad228a2df9",
  "crates/sqliters-codegen/src/exprcode.rs": "e5daeeb16c6c0ff05e639720b5d716d2b8d5f5889b71b3e43d5e7481905b7e5b",
  "crates/sqliters-codegen/src/sqliteint.rs": "504015de2c43cb720855465454c10888076b0a1be4a951855162e3d00d28d44a",
  "crates/sqliters-codegen/src/whereexpr.rs": "cc8093ccf3a9487b32f28a5a3a79e3632ac6b257854fd55437f6a354c33fc8f4",
  "crates/sqliters-core/src/fast_read.rs": "944d265cb9b770d561276f5cfea6518414f83598133409e5c682ba2edf29e661",
  "crates/sqliters-core/src/vdbe_host.rs": "9d59ecef3e47599d2bc001b11f9b2510c4764a1da5cc244a6c0373c393baaf72",
  "crates/sqliters-types/src/value.rs": "fb441336d87768002c01f0a10b6c08e3796f2508e0cec3c9e616fcc5f57694b5",
  "crates/sqliters-vdbe/src/lib.rs": "a2a3940be2fce4e24139042fdb0a515cc939d95683fd73f43c51ed73bc9d79fa",
  "crates/sqliters-vdbe/src/vdbe.rs": "797a9e57df476ee93b9fcae07364a7a861b2c4b91bae4d2a65206c62f47857af",
  "crates/sqliters-vdbe/src/vdbeaux_rt.rs": "55d5de307e6cc1f5dd44f22d8374aeed6be9b814875bf3dd871a824e390d1670",
  "crates/sqliters-vdbe/src/vdbemem.rs": "519728deaf4a02667a2184432f596602d70bb1283f54965319418daff21c1318",
  "crates/sqliters-vdbe/src/utf.rs": "51b68d4fb0dc49bac773e201a962f504c9df58eb0ff861eaac8a409ea54804de",
  "tests/v6b_utf_collation.rs": "1f33cdda1ad13c331b821c15dae9b4139df4e2ee3cc5fe718d074d88be1e1ff7"
}

```

## Ana C kapsam ölçümü

# C-side coverage of the functions ported or completed in step V6b.31 (gcov, -O0 build of the reference amalgamation)

Corpus: the 16063 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C file | lines covered | branches taken at least once |
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
| util.c | 93 / 97 (95.9%) | 77 / 84 (91.7%) |
| build.c | 13 / 14 (92.9%) | 6 / 8 (75.0%) |
| vtab.c | 81 / 118 (68.6%) | 29 / 60 (48.3%) |
| insert.c | 46 / 61 (75.4%) | 17 / 32 (53.1%) |
| vdbeapi.c | 10 / 19 (52.6%) | 3 / 4 (75.0%) |
| pcache1.c | 19 / 19 (100.0%) | 10 / 10 (100.0%) |
| vdbemem.c | 31 / 34 (91.2%) | 26 / 36 (72.2%) |
| utf.c | 51 / 64 (79.7%) | 63 / 74 (85.1%) |
| hash.c | 74 / 74 (100.0%) | 42 / 44 (95.5%) |
| expr.c | 20 / 22 (90.9%) | 16 / 24 (66.7%) |
| ctime.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| func.c | 91 / 121 (75.2%) | 88 / 130 (67.7%) |
| fkey.c | 212 / 273 (77.7%) | 120 / 189 (63.5%) |
| whereexpr.c | 58 / 71 (81.7%) | 51 / 80 (63.8%) |
| **total** | 2883 / 3500 (82.4%) | 1806 / 2459 (73.4%) |

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
| sqlite3Atoi64 | util.c | 51 | 100.0 | 54 | 96.3 | 2 |
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
| sqlite3VdbeMemTranslate | utf.c | 64 | 79.7 | 74 | 85.1 | 8 |
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
| isLikeOrGlob | whereexpr.c | 71 | 81.7 | 80 | 63.8 | 18 |
| sqlite3VdbeGetBoundValue | vdbeaux.c | 10 | 100.0 | 6 | 83.3 | 1 |
| sqlite3VdbeSetSql | vdbeaux.c | 7 | 100.0 | 4 | 100.0 | 0 |

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 205056 | 2/2 |
| parse.y:524 | 147874 | 2/2 |

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

## V6b.31 — özel hata ve yeniden hazırlama blokları

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
