# V6b.22 — collation_list ve kabuk karşılaştırıcıları

C SQLite 3.54.1; Rust 1.100. collation_list gerçek bağlantı kayıt geçmişinden üretilir ve iki anahtarda makineden yürür. Kabukta eksik uint/decimal karşılaştırıcıları da C kaynaklarından taşındı. Decimal SQL işlevleri, genel collation API/UTF sentezleme, OOM ve kalan V6b/M4 tamamlanmış sayılmaz.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:444–457,1456–1463` | runOnlyOnce, şema adı, nMem=2 | RHS listeyi filtrelemez; TEMP adı önce TEMP Btree açar. C hash dolaşım sırasındaki seq/name hazırlamada sabitlenir. | `crates/sqliters-codegen/src/pragma.rs:451` |
| `main.c:3579–3586; hash.c:83–109` | Varsayılan kayıt sırası | BINARY, NOCASE, RTRIM bu sırada eklenir. Bucket dizisi olmayan üç girişin liste sırası RTRIM, NOCASE, BINARY olur. | `crates/sqliters-core/src/execution_context.rs:515` |
| `hash.c:56–73,83–109,118–152,267–270` | Hash, bucket ve yeniden boyutlandırma | Daha önce taşınan c_hash_iteration_order aynı ekleme geçmişi üzerinde kullanılır; adlar alfabetik sıralanmaz. Beş girişte ve sonraki büyüme eşiklerinde yeniden bağlama uygulanır. | `crates/sqliters-core/src/schema.rs:2648` |
| `callback.c:113–127` | Adın ilk yazımı | ASCII eşdeğer ad yeni hash girişi oluşturmaz. İlk zName korunur; sonraki MIXED/mixed kaydı sıra veya yazımı değiştirmez. | `crates/sqliters-core/src/execution_context.rs:534` |
| `main.c:2938–2945` | Var olan UTF-8 xCmp ve etkin ifade | Değiştirme/silme SQLITE_BUSY ve C metnini verir. Etkin ifade yoksa c_expire_all çağrılır; yeni ad eklemek eski listeyi expire etmez. | `crates/sqliters-core/src/lib.rs:1718` |
| `main.c:2968–2973; callback.c:115–127` | NULL xCmp ile kayıt | Silinen karşılaştırıcının adı hash’te kalır. Daha önce hiç bulunmayan adı silme çağrısı bile giriş oluşturur. | `crates/sqliters-core/src/execution_context.rs:577` |
| `callback.c:256–267; build.c:2017` | Şema yüklemesinde karşılaştırıcısız ad | init.busy sırasında LocateCollSeq placeholder döndürür. Başarılı katalog yüklemesi column COLLATE adlarını catalog/token sırasıyla bağlantıya aktarır. | `crates/sqliters-codegen/src/callback.rs:56` |
| `prepare.c:459–478; attach.c:271` | Ana/bağlı/TEMP yükleme ve DETACH | Katalogdan gelen adlar gerçek yükleme sırasında kaydedilir; ATTACH sonrası korunur ve DETACH ile silinmez. Başarısız kısmi yükleme yaşamı ayrıca kanıtlanmadı. | `crates/sqliters-core/src/schema.rs:864` |
| `pragma.c:1456–1463; vdbe.c:8386–8393` | Makine, reset, expire ve okuyucu bayrağı | Sıfır Btree maskeli program iki anahtarda gerçek makineden yürür. Yeni kayıt ilk çalıştırmanın satırlarını değiştirmez; reset güncel listeyi derler. | `crates/sqliters-core/src/lib.rs:6036` |
| `shell.c.in:5236–5238` | Kabuk kayıtları | Her normal, salt okunur, yeniden açılan veya yedek bellek kabuk DB’sine uint, ardından decimal gerçek karşılaştırıcıları kaydedilir. Kitaplığın varsayılan üç girişi değiştirilmez. | `crates/sqliters-cli/src/main.rs:634` |
| `ext/misc/uint.c:50–82` | Basamak dizileri ve sıfırlar | Yalnız iki taraf da basamakla başlarsa baştaki sıfırlar atlanır. Basamak adedi, aynı uzunlukta byte karşılaştırması, ardından kalan metin uygulanır; işaret, nokta ve üs sayısal işlem görmez. | `crates/sqliters-cli/src/collations.rs:5` |
| `ext/misc/decimal.c:90–123` | Ondalık metnin okunması | C ASCII boşluğu, ilk işaret/sıfırlar, tüm metindeki basamaklar, son nokta ve e/E kuyruğu aynen işlenir. Genel matematiksel sayı ayrıştırıcısı kullanılmaz. | `crates/sqliters-cli/src/collations.rs:58` |
| `ext/misc/decimal.c:105–121,127–173` | Üs sınırı ve basamak/frac düzeni | Üs döngüsü 1.000.000 eşiğinde durur; işaret ve nExtra kararları, pozitif/negatif üs için sıfır ekleme veya kaydırma C sırasındadır. | `crates/sqliters-cli/src/collations.rs:110` |
| `ext/misc/decimal.c:170–180,766–773` | Negatif sıfır, 10.000.000 basamak sınırı, hata | Tamamı sıfır olan negatif sayı işaretini kaybeder. C basamak sınırını aşınca karşılaştırıcı eşit döndürür. Rust fallible Vec kullanır; tahsis sayısı ve OOM enjeksiyonu eşliği açık. | `crates/sqliters-cli/src/collations.rs:145` |
| `ext/misc/decimal.c:415–443` | Ondalık karşılaştırma | Kesir sonundaki sıfırlar kırpılır; işaret, negatiflerde operand değişimi, anlamlı tamsayı adedi, ortak basamaklar ve kalan uzunluk sırasıyla karşılaştırılır. | `crates/sqliters-cli/src/collations.rs:167` |

## Native karşılaştırma ve açık kusur

**D126 — liste sırası, ilk yazım ve kayıt geçmişi.** Önceki 711360d ikilisiyle 384/384 akış farklıydı; C/R referansları res/collation_before.json içinde. Varsayılan sıra ve silinmiş/boş karşılaştırıcı adlarının kaybı, alfabetik özel ad sırası ve hazırlama sonrası yeni kayıtların ilk listeye sızması vardı. İlk üretici/registry düzeltmesi sonrası 60 fark; expire/BUSY düzeltmesi sonrası 12 şema yükleme farkı kaldı. Gerçek katalog yükleme adları aktarıldıktan sonra 384/384 eşleşti (collation_first_port; collation_lifecycle; collation_schema).

**D127 — değiştirme/silme BUSY ve expire.** Hazırlanmış ama başlamamış liste, yeni ad eklenince C’de sabit kalır; var olan xCmp değiştirilince güncel listeyle yeniden hazırlanır. Satır beklerken yeni ad/boş kayıt eklenebilir; etkin xCmp değiştirme/silme BUSY verir. Silinmiş yerleşik UTF-8 xCmp tekrar silinince BUSY verilmez. UTF-8 kayıt API kapsamındaki bu kararlar karşılaştırıldı; diğer kodlamalardan yeniden sentezleme, destructor ve genel builtin kullanım davranışı ayrıca açık.

Genişletilmiş **438 akışın 426’sı tam eşleşir**. İki anahtar, üç kodlama, bellek/disk, 0–129 özel ad, hash büyüme eşikleri, ilk yazım, boş/Unicode ad, NULL karşılaştırıcı, pending/reset/expire, callback/sayaçlar, sanal tablo ve bilinmeyen şema; ayrıca dosyadan/ATTACH ile yüklenen karşılaştırıcısız adlar sınanır. Bütün 438 akışın dosya baytları ve C referans gözlemleri önceki ikili deneyiyle de aynıdır (res/collation_matrix.json; collation_before_expanded.json; validation_extra.py).

**D128 açık — SELECT eksik karşılaştırıcı hatası.** Kalan 12 satır aynı eski kusurdur: SELECT 1 ORDER BY 1 COLLATE missing. C hazırlamada 257/no such collation sequence: missing verir. Açık UTF-8 Rust yolunda kod 1’e iner; diğer örneklerde hazırlama kabul edilir. Sonraki liste çıktısı ve bütün durum/dosya bilgileri aynı. Yalnız bu kesin hata satırı önceden kaydedilen şekliyle kabul edilir; başka fark izinli değildir. D128 sonraki doğruluk adımıdır.

**39 kayıt geçmişindeki 1.362 tam program** C özel program dökümüyle eşleşir; P1–P5, P4 metinleri ve program metadata birlikte karşılaştırıldı. Hash eşikleri ve adlar yalnız sonuç satırıyla kanıtlanmış sayılmadı (res/collation_programs.json). Bozuk ana dosyada şema okumadan listeleme için ek altı akış da sonuç/durum ve .db baytlarında aynıdır (res/collation_noschema.json).

**D129 — kabuk kayıtları.** İlk tam kapı 83 EXPLAIN ve 93 yürütme farkı yakaladı: C CLI uint/decimal kaydederken Rust CLI kaydetmiyordu. Ham ilk kaynak manifesti, tüm sonuçlar ve kapsam res_before_shell_collations/ içinde korundu; bu sürüm commit edilmedi. Gerçek karşılaştırıcılar taşındıktan sonra final dört kapı yeniden çalıştırıldı; listeye sahte ad eklenmedi.

**7.536 karşılaştırıcı girdisi** aynı C kaynaklarıyla doğrudan karşılaştırıldı. Sabit çift çarpımı ve sabit seedli dizgiler; boş metin, NUL, ASCII boşluğu, artı/eksi, sıfırlar, 200+ basamak, birden fazla nokta, geçersiz sayı/artık karakter, Unicode, üs döngüsü ve basamak sınırının iki yanı içerir. SQLite karşılaştırıcı sözleşmesindeki küçük/eşit/büyük kararı aynıdır; ham C callback dönüş sayısının büyüklüğü kullanıcı sonucu sayılmadı (res/ext_inputs.json; ext_C.txt; ext_Rust.txt; ext_summary.json).

**36 kabuk akışı** iki anahtar × üç kodlama × rollback/WAL × normal/salt okunur/yeniden açmayı kapsar. Her iki karşılaştırıcıyla oluşturulan indeksler, sıralı/range sonuçlar, açılıştaki liste ve bütün .db baytları C ile aynıdır (res/shell_matrix.json). İlk hızlı kabuk denemesi derleme bitmeden eski ikiliyle çalışmıştı; before_cli_build_* kayıtları bu doğrulanmamış deneyi ayırır. Final G/Ç çalışmasında yeni CLI kapısının binaries.done bağımlılığı eksikti; yalnız o eski CLI denemesi res_before_cli_binary_ready/ içinde saklandı, bağımlılık düzeltildi ve shell_matrix yeni ikiliyle yeniden eşleşti. Ürün kaynağı bu düzeltmede değişmedi.

Tam program derlemi 13314 aynı; 104 Unsupported başarı sayılmadı. EXPLAIN 15804, EQP 15804, yürütme 11609 aynı. API 4157/4158 aynı; tek çıktı farkı eski D103, bütün dosya baytları aynı. Önceki sync/IOERR/SHORT_READ/cache/WAL matrisleri yeniden eşleşir. 72 ham G/Ç izinin 52’si aynı; 20 dirty/TEMP farkı açık kalır.

## C kapsamı

Ölçüm birikimlidir: 15.929 kabul edilen SQL, şema kurulumları, önceki native matrisler ve collation API akışları. Yeni seçili tam yordamlar Hash, CollSeq arama/oluşturma ve createCollation’dır. Bir tam C yordamını ölçmek bütün Rust karşılığını taşımış olmak anlamına gelmez. VDBE bütünü sayılmaz; önceki seçili opcode/özel blok kapsamları korunur.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 15929 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 887 / 1142 (77.7%) | 587 / 794 (73.9%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 319 / 380 (83.9%) | 211 / 306 (69.0%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 109 / 123 (88.6%) | 57 / 74 (77.0%) |
| vdbeaux.c | 57 / 64 (89.1%) | 14 / 18 (77.8%) |
| callback.c | 55 / 60 (91.7%) | 19 / 24 (79.2%) |
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
| **total** | 2339 / 2878 (81.3%) | 1416 / 1946 (72.8%) |

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

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 93942 | 2/2 |
| parse.y:524 | 47288 | 2/2 |

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

## V6b.22 — özel hata ve yeniden hazırlama blokları

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


**589 alınmayan dal** [karar_esligi_v6b_22_dallar.md](karar_esligi_v6b_22_dallar.md) içinde ayrı satırlarda sınıflandırıldı; yedisi decimal yardımcılarındadır. uint karşılaştırıcısında 36/36 dal; decimal parse 79/86, cmp 20/20 ve callback 4/4 dal alınmıştır. NULL/tahsis hata dalları kanıtlandı sayılmaz; C’nin ayrı Decimal heap nesnesiyle Rust stack yapısının tahsis izi aynı değildir. Geçersiz UTF-8/UTF-16, kısmi başarısız şema yükleme, diğer encoding/destructor/synthCollSeq kararları ayrıca açıktır.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1115, 0, 337); açık (1100, 15, 337). C ikilisi PATH’te. Açık anahtardaki 15 başarısızın adları öncekiyle aynı; beklentiler değiştirilmedi. DML kapalı 378/400, açık 398/400; sınıf/örnek kümeleri aynı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs; yedi A/B; anahtar kapalı. Karşılaştırıcı kayıtlarının ve yeni durum takibinin maliyeti ölçüme dahildir. Performans ayarı yapılmadı; açık anahtarın hız garantisi verilmez.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.24      8.83     0.956x     9.22     8.78     9.60    0.92x  OK
W02_create_indexes                5.64      5.64     0.999x     5.63     5.62     8.76    0.64x  OK
W03_pk_point_select              29.94     30.22     1.009x    29.61    29.63    23.30    1.30x  OK
W04_secondary_idx_range           9.54      9.71     1.018x     9.46     9.55     7.46    1.30x  OK
W05_full_scan_filter_agg          4.40      4.39     0.998x     4.38     4.38    16.57    0.27x  OK
W06_join_group_order             10.81     10.90     1.008x    10.77    10.86    28.23    0.39x  OK
W07_window_functions             37.01     37.65     1.017x    36.92    37.26    50.21    0.75x  OK
W08_indexed_update_delete        11.95     11.91     0.996x    11.92    11.88     7.96    1.50x  OK
W09_like_text_scan               12.43     12.47     1.003x    12.41    12.43    18.92    0.66x  OK
W10_jsonb_extract_agg            13.70     13.61     0.993x    13.62    13.46    10.93    1.24x  OK
W11_fts5_index_and_bm25          69.77     69.92     1.002x    68.71    69.49    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.98     41.21     1.006x    39.87    40.55    24.63    1.67x  OK
W13_autocommit_wal_txns           4.44      4.78     1.077x     4.35     4.55     4.12    1.16x  OK
W14_cli_sql_script               38.62     52.58     1.362x    38.52    52.50    26.19    2.01x  OK
TOTAL                           298.48    313.81     1.051x   geomean cand/base = 1.028x   digest mismatches = 0
```

## Kayıt ve sıradaki adım

D128 hazırlama hatası ve genişletilmiş kod korunması sıradaki doğruluk işidir. Sonrasında kalan PRAGMA işleyicileri sürdürülür. function_list/module_list, decimal SQL işlevleri ve diğer kabuk işlevleri, D103, E28, M4, bozuk UTF girdileri, eski yürütücü ve hızlı yollar açıktır. Bu dilim tam C garantisi veya yayın tamamlanması iddiası değildir.

Kanıt /home/hakan/github/sqliters/target/perf-work/v6b22/; on bir kaynak/test dosyası res/source_manifest.json, C extension kopyaları res/ext_source_manifest.json ile sabittir. C kaynakları değiştirilmedi. ileriPlanlama değerlendirilmedi.
