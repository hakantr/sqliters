# V6b.24 — compile_options ve SQL tanı işlevleri

C SQLite 3.54.1; Rust 1.100. PRAGMA compile_options gerçek üreticiye taşındı. sqlite_compileoption_get/used artık makine işlev kitaplığında çalışır. Hedef, sabit C başvuru derlemesinin 42 satırlık tanı profilidir. COMPILER=gcc-15.2.0 bu uyumluluk hedefini anlatır; Rust derleyicisinin adı değildir. Çalışma anında C ikilisine bağımlılık eklenmedi. Bu metadata eşliği, listelenen bütün özelliklerin motor içinde tamamlandığı anlamına gelmez.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:2379–2387` | COMPILE_OPTIONS; pParse.nMem=1; get(i++) NULL olana kadar | 42 sabit satır aynı sırada String8/ResultRow ile üretilir; RHS değeri yok sayılır. | `crates/sqliters-codegen/src/pragma.rs:463` |
| `vdbeaux.c:674–682` | RunOnlyOnce tarafından eklenen ilk Expire | Reusable, ilk Expire yerine Noop koyar. Reset kendi başına yeniden hazırlama doğurmaz; başka bağlantı etkileri ayrıca expire edebilir. | `crates/sqliters-vdbe/src/vdbeaux.rs:463` |
| `pragma.c:448–530; sqlite/bld/pragma.h:223–228` | İki parçalı şema adı ve VFS ön işlemi | database_list/collation_list ile ortak gerçek üretici yolu; bu liste şema içeriği okumaz. Bilinmeyen şema adı C hata konumuyla döner. Özel VFS cevabı ve yetkilendirici yolu açık Unsupported. | `crates/sqliters-core/src/lib.rs:6055` |
| `main.c:5264–5273` | N>=0 && N<nOpt | Dizi dışı NULL; dizi içi değişmeyen seçenek metni. | `crates/sqliters-types/src/compile_options.rs:50` |
| `func.c:1080–1093; vdbeapi.c:210–212; vdbemem.c:641–657,879–883` | SQL değeri → i64 → C int | NULL sıfıra, sayı/metin/BLOB i64 dönüşümünden sonra 32 bite kesilir. UTF-16 BLOB bağlantı kodlamasıyla yorumlanır. | `crates/sqliters-vdbe/src/func_c.rs:161` |
| `main.c:5231–5257; util.c:99–102` | SQLITE_ öneki ve strlen30 | İsteğe bağlı tek önek ASCII harf duyarsız çıkarılır; ilk NUL ve 0x3fffffff uzunluk maskesi korunur. | `crates/sqliters-types/src/compile_options.rs:57` |
| `main.c:5250–5254; tokenize.c:154–190` | Önek eşit ve sonraki bayt tanımlayıcı karakteri değil | Harf/rakam/_/$/yüksek bit sınırı uygulanır; örneğin COMPILER=gcc kabul edilir, THREADSAFE$ edilmez. | `crates/sqliters-types/src/compile_options.rs:69` |
| `func.c:1056–1071` | sqlite3_value_text sonucu NULL değil | NULL/NaN NULL kalır; diğer değerler C metin biçimi ve bağlantı kodlamasıyla seçenek aramasına gider. | `crates/sqliters-vdbe/src/func_c.rs:171` |
| `func.c:3396–3397` | DFUNCTION, bir argüman, INNOCUOUS; deterministic değil | Her iki işlev gerçek makine kitaplığında bulunur. Eksik/fazla argüman hazırlama hatasıdır. | `crates/sqliters-vdbe/src/functions.rs:190` |
| `resolve.c:1322–1325; util.c:249–272` | wrong_num_args | Eski doğrulamanın -1 yaptığı hata konumu gerçek üreticinin konumundan korunur; SELECT örneklerinde 7. Yardımcı ayrıca önceki 257 collation hatasını taşır. | `crates/sqliters-core/src/lib.rs:5365` |
| `vdbe.c:1471–1503,1801–1836` | String8, UTF kodlama ve ResultRow | Önceki gerçek VM metin yolu kullanılır; aynı 42 metin UTF-8/LE/BE ve reset ile döner. Genel TOOBIG/NOMEM eşliği bu dilimde tamamlanmadı. | `crates/sqliters-vdbe/src/vdbe.rs:2238` |
| `engine.rs COMPILE_OPTIONS` | Eski pragma sanal tablo okuma yolu | Aynı sabit profil kullanılır; pragma_compile_options sorgusu da aynı satırları verir. | `crates/sqliters-core/src/engine.rs:43418` |

## Sabit C derleme profilinin kararları

C CLI, program döküm sürücüsü ve kapsam API sürücüsünün 42 seçeneği birebir aynıdır (res/reference_profiles.json, ikili SHA-256 ve ham çıktı dahil). C’nin üretilmiş ctime.c dosyası sqlite/bld/ctime.c içindedir; aşağıdaki koşullar o dosyadan okunmuştur. Seçenekler Rust tarafında tek sabit dizide tutulur. Bu hedef dışındaki C derleme profilleri ve ön işlemcinin derlemeye alınmayan seçenekleri runtime kapsamı sayılmaz.

| C koşulu / sonuç satırı | Koşul | Gözlenen seçenek | Rust sabit satırı |
|---|---|---|---|
| `sqlite/bld/ctime.c:74`, sonuç `75` | `#ifdef SQLITE_ATOMIC_INTRINSICS` | `ATOMIC_INTRINSICS=1` | `crates/sqliters-types/src/compile_options.rs:5` |
| `sqlite/bld/ctime.c:98`, sonuç `99` | `!(defined(__clang__) && defined(__clang_major__)) && !defined(_MSC_VER) && defined(__GNUC__) && defined(__VERSION__)` | `COMPILER=gcc-15.2.0` | `crates/sqliters-types/src/compile_options.rs:6` |
| `sqlite/bld/ctime.c:110`, sonuç `111` | `#ifdef SQLITE_DEFAULT_AUTOVACUUM` | `DEFAULT_AUTOVACUUM` | `crates/sqliters-types/src/compile_options.rs:7` |
| `sqlite/bld/ctime.c:113`, sonuç `114` | `#ifdef SQLITE_DEFAULT_CACHE_SIZE` | `DEFAULT_CACHE_SIZE=-2000` | `crates/sqliters-types/src/compile_options.rs:8` |
| `sqlite/bld/ctime.c:119`, sonuç `120` | `#ifdef SQLITE_DEFAULT_FILE_FORMAT` | `DEFAULT_FILE_FORMAT=4` | `crates/sqliters-types/src/compile_options.rs:9` |
| `sqlite/bld/ctime.c:128`, sonuç `129` | `#ifdef SQLITE_DEFAULT_JOURNAL_SIZE_LIMIT` | `DEFAULT_JOURNAL_SIZE_LIMIT=-1` | `crates/sqliters-types/src/compile_options.rs:10` |
| `sqlite/bld/ctime.c:142`, sonuç `143` | `#ifdef SQLITE_DEFAULT_MMAP_SIZE` | `DEFAULT_MMAP_SIZE=0` | `crates/sqliters-types/src/compile_options.rs:11` |
| `sqlite/bld/ctime.c:145`, sonuç `146` | `#ifdef SQLITE_DEFAULT_PAGE_SIZE` | `DEFAULT_PAGE_SIZE=4096` | `crates/sqliters-types/src/compile_options.rs:12` |
| `sqlite/bld/ctime.c:148`, sonuç `149` | `#ifdef SQLITE_DEFAULT_PCACHE_INITSZ` | `DEFAULT_PCACHE_INITSZ=20` | `crates/sqliters-types/src/compile_options.rs:13` |
| `sqlite/bld/ctime.c:154`, sonuç `155` | `#ifdef SQLITE_DEFAULT_RECURSIVE_TRIGGERS` | `DEFAULT_RECURSIVE_TRIGGERS` | `crates/sqliters-types/src/compile_options.rs:14` |
| `sqlite/bld/ctime.c:160`, sonuç `161` | `#ifdef SQLITE_DEFAULT_SECTOR_SIZE` | `DEFAULT_SECTOR_SIZE=4096` | `crates/sqliters-types/src/compile_options.rs:15` |
| `sqlite/bld/ctime.c:163`, sonuç `164` | `#ifdef SQLITE_DEFAULT_SYNCHRONOUS` | `DEFAULT_SYNCHRONOUS=2` | `crates/sqliters-types/src/compile_options.rs:16` |
| `sqlite/bld/ctime.c:166`, sonuç `167` | `#ifdef SQLITE_DEFAULT_WAL_AUTOCHECKPOINT` | `DEFAULT_WAL_AUTOCHECKPOINT=1000` | `crates/sqliters-types/src/compile_options.rs:17` |
| `sqlite/bld/ctime.c:169`, sonuç `170` | `#ifdef SQLITE_DEFAULT_WAL_SYNCHRONOUS` | `DEFAULT_WAL_SYNCHRONOUS=2` | `crates/sqliters-types/src/compile_options.rs:18` |
| `sqlite/bld/ctime.c:172`, sonuç `173` | `#ifdef SQLITE_DEFAULT_WORKER_THREADS` | `DEFAULT_WORKER_THREADS=0` | `crates/sqliters-types/src/compile_options.rs:19` |
| `sqlite/bld/ctime.c:175`, sonuç `176` | `#ifdef SQLITE_DIRECT_OVERFLOW_READ` | `DIRECT_OVERFLOW_READ` | `crates/sqliters-types/src/compile_options.rs:20` |
| `sqlite/bld/ctime.c:238`, sonuç `239` | `#ifdef SQLITE_ENABLE_DBSTAT_VTAB` | `ENABLE_DBSTAT_VTAB` | `crates/sqliters-types/src/compile_options.rs:21` |
| `sqlite/bld/ctime.c:259`, sonuç `260` | `#ifdef SQLITE_ENABLE_FTS5` | `ENABLE_FTS5` | `crates/sqliters-types/src/compile_options.rs:22` |
| `sqlite/bld/ctime.c:280`, sonuç `281` | `#ifdef SQLITE_ENABLE_MATH_FUNCTIONS` | `ENABLE_MATH_FUNCTIONS` | `crates/sqliters-types/src/compile_options.rs:23` |
| `sqlite/bld/ctime.c:322`, sonuç `323` | `#ifdef SQLITE_ENABLE_RTREE` | `ENABLE_RTREE` | `crates/sqliters-types/src/compile_options.rs:24` |
| `sqlite/bld/ctime.c:435`, sonuç `436` | `#ifdef SQLITE_MALLOC_SOFT_LIMIT` | `MALLOC_SOFT_LIMIT=1024` | `crates/sqliters-types/src/compile_options.rs:25` |
| `sqlite/bld/ctime.c:438`, sonuç `439` | `#ifdef SQLITE_MAX_ATTACHED` | `MAX_ATTACHED=10` | `crates/sqliters-types/src/compile_options.rs:26` |
| `sqlite/bld/ctime.c:441`, sonuç `442` | `#ifdef SQLITE_MAX_COLUMN` | `MAX_COLUMN=2000` | `crates/sqliters-types/src/compile_options.rs:27` |
| `sqlite/bld/ctime.c:444`, sonuç `445` | `#ifdef SQLITE_MAX_COMPOUND_SELECT` | `MAX_COMPOUND_SELECT=500` | `crates/sqliters-types/src/compile_options.rs:28` |
| `sqlite/bld/ctime.c:447`, sonuç `448` | `#ifdef SQLITE_MAX_DEFAULT_PAGE_SIZE` | `MAX_DEFAULT_PAGE_SIZE=8192` | `crates/sqliters-types/src/compile_options.rs:29` |
| `sqlite/bld/ctime.c:450`, sonuç `451` | `#ifdef SQLITE_MAX_EXPR_DEPTH` | `MAX_EXPR_DEPTH=1000` | `crates/sqliters-types/src/compile_options.rs:30` |
| `sqlite/bld/ctime.c:453`, sonuç `454` | `#ifdef SQLITE_MAX_FUNCTION_ARG` | `MAX_FUNCTION_ARG=1000` | `crates/sqliters-types/src/compile_options.rs:31` |
| `sqlite/bld/ctime.c:456`, sonuç `457` | `#ifdef SQLITE_MAX_LENGTH` | `MAX_LENGTH=1000000000` | `crates/sqliters-types/src/compile_options.rs:32` |
| `sqlite/bld/ctime.c:459`, sonuç `460` | `#ifdef SQLITE_MAX_LIKE_PATTERN_LENGTH` | `MAX_LIKE_PATTERN_LENGTH=50000` | `crates/sqliters-types/src/compile_options.rs:33` |
| `sqlite/bld/ctime.c:465`, sonuç `466` | `#ifdef SQLITE_MAX_MMAP_SIZE` | `MAX_MMAP_SIZE=0x7fff0000` | `crates/sqliters-types/src/compile_options.rs:34` |
| `sqlite/bld/ctime.c:471`, sonuç `472` | `#ifdef SQLITE_MAX_PAGE_COUNT` | `MAX_PAGE_COUNT=0xfffffffe` | `crates/sqliters-types/src/compile_options.rs:35` |
| `sqlite/bld/ctime.c:474`, sonuç `475` | `#ifdef SQLITE_MAX_PAGE_SIZE` | `MAX_PAGE_SIZE=65536` | `crates/sqliters-types/src/compile_options.rs:36` |
| `sqlite/bld/ctime.c:477`, sonuç `478` | `#ifdef SQLITE_MAX_SCHEMA` | `MAX_SCHEMA=10000000` | `crates/sqliters-types/src/compile_options.rs:37` |
| `sqlite/bld/ctime.c:483`, sonuç `484` | `#ifdef SQLITE_MAX_SQL_LENGTH` | `MAX_SQL_LENGTH=1000000000` | `crates/sqliters-types/src/compile_options.rs:38` |
| `sqlite/bld/ctime.c:486`, sonuç `487` | `#ifdef SQLITE_MAX_TRIGGER_DEPTH` | `MAX_TRIGGER_DEPTH=1000` | `crates/sqliters-types/src/compile_options.rs:39` |
| `sqlite/bld/ctime.c:489`, sonuç `490` | `#ifdef SQLITE_MAX_VARIABLE_NUMBER` | `MAX_VARIABLE_NUMBER=32766` | `crates/sqliters-types/src/compile_options.rs:40` |
| `sqlite/bld/ctime.c:492`, sonuç `493` | `#ifdef SQLITE_MAX_VDBE_OP` | `MAX_VDBE_OP=250000000` | `crates/sqliters-types/src/compile_options.rs:41` |
| `sqlite/bld/ctime.c:495`, sonuç `496` | `#ifdef SQLITE_MAX_WORKER_THREADS` | `MAX_WORKER_THREADS=8` | `crates/sqliters-types/src/compile_options.rs:42` |
| `sqlite/bld/ctime.c:513`, sonuç `514` | `#ifdef SQLITE_MUTEX_PTHREADS` | `MUTEX_PTHREADS` | `crates/sqliters-types/src/compile_options.rs:43` |
| `sqlite/bld/ctime.c:763`, sonuç `767` | `#if (!defined(SQLITE_WIN32_MALLOC) \` | `SYSTEM_MALLOC` | `crates/sqliters-types/src/compile_options.rs:44` |
| `sqlite/bld/ctime.c:772`, sonuç `773` | `#ifdef SQLITE_TEMP_STORE` | `TEMP_STORE=1` | `crates/sqliters-types/src/compile_options.rs:45` |
| `sqlite/bld/ctime.c:778`, sonuç `779` | `#if defined(SQLITE_THREADSAFE)` | `THREADSAFE=1` | `crates/sqliters-types/src/compile_options.rs:46` |

## Önce/sonra ve native API

92b69eb ikilisi önceki 2.082 API akışının tamamında farklıydı (res/options_before.json). PRAGMA Rust’a özgü üç seçenek veriyor, iki SQL işlevi bulunamıyordu. İlk düzeltme aynı 2.082 akışın tamamını eşitledi (options_initial.json). Derlem 3.990 akışa genişletilince 10 akışta yalnız yanlış argüman sayısının hata konumu farklı kaldı: C 7, eski Rust doğrulaması -1. Tam ham önceki sonuç options_expanded.json ve options_expanded_bugs.json içinde korunur. Hata konumu düzeltmesinden sonra options_offset.json 3.990/3.990 aynı; final kapı bunu options_matrix.json içinde tekrar çalıştırır.

Final 3.990 akış iki anahtar × üç kodlama × bellek/disk bağlamlarını kapsar (bozuk başlık yalnız diskte). PRAGMA RHS ve main/temp/aux/bilinmeyen şema; tamamlanmadan reset, tekrar kullanım, sekiz stmt_status sayacı, çalışan liste yanında DDL, ATTACH/DETACH, pragma TVF, ilerleme kesmesi ve bozuk dosyada şema okumama denendi.

get/used için bütün 42 seçenek, dizi sınırları, i32/i64 ve 2^32 kesilmesi, sonlu/sonsuz/NaN sayılar, kesirler, NULL, NUL, harf boyutu, boş/çift SQLITE_ önekleri, tanımlayıcı sınırları ve UTF-8/LE/BE BLOB girdileri karşılaştırıldı. Bu örneklerde hatalar/konum, kolonlar, satırlar, autocommit/değişiklik durumu, sayaçlar, stdout/stderr/çıkış kodu ve bütün .db baytları aynı. Geçersiz kodlama örnekleri genel UTF-16/geçersiz UTF-8 eşliğini tamamlamaz. 1 GiB üstü uzunluk maskesi kod karşılığı gösterildi; bu büyüklük ve küçük SQLITE_LIMIT_LENGTH/NOMEM enjeksiyonu yapılmadı.

Birim testi üç kodlamada reset’in yeniden hazırlama üretmemesini, 2^32 indeks dönüşümünü, seçenek sınırını, NULL sonucunu ve yanlış argüman hata konumunu denetler. Eski test beklentileri değiştirilmedi.

Genel derlemde 13372 tam program aynı; 93 Unsupported başarı sayılmadı. EXPLAIN 15862, EQP 15862, yürütme 11656 aynı. Üretici trace’inden önceki 64 hazırlama hatası C ile aynı karşılaştırılır, kabul edilen program sayısına katılmaz. 39 collation geçmişindeki 1.362 ek program da aynı.

Önceki API 4157/4158 aynı; tek çıktı farkı D103 ve bütün dosya baytları aynı. Önceki 438 collation, 636 D128, 36 kabuk/indeks, 7.536 karşılaştırıcı, altı bozuk dosya/no-schema ve IOERR/SHORT_READ/cache/WAL matrisleri korundu. 72 ham G/Ç izinin 52’si aynı; kalan 20 dirty/TEMP farkı önceki açık kapsamdır.

## C kapsamı

Birikimli kapsam sqlite3_compileoption_get/used, iki SQL sarmalayıcısı, sqlite3CompileOptions ve sqlite3VdbeReusable yordamlarıyla genişletildi. Değer-int/metin sarmalayıcıları, Mem int/real dönüşümü, tanımlayıcı karakteri ve strlen30 yardımcıları da ölçüldü. OP_Noop sekizinci seçili opcode bloğudur; tüm sqlite3VdbeExec sayılmadı. C kapsam matrisinde de 3.990 native akış tekrar çalışır.

### Seçili C işlevlerinin birikimli kapsamı (gcov, -O0 başvuru derlemi)

Corpus: the 15987 statements the port accepts, each run as EXPLAIN and as EXPLAIN QUERY PLAN, plus the setup statements of their schemas.

| C dosyası | Kapsanan satırlar | En az bir kez alınan dallar |
|---|---|---|
| pragma.c | 895 / 1142 (78.4%) | 590 / 794 (74.3%) |
| btree.c | 251 / 278 (90.3%) | 110 / 136 (80.9%) |
| pager.c | 319 / 380 (83.9%) | 211 / 306 (69.0%) |
| pcache.c | 17 / 17 (100.0%) | 10 / 10 (100.0%) |
| main.c | 123 / 137 (89.8%) | 69 / 86 (80.2%) |
| vdbeaux.c | 63 / 70 (90.0%) | 16 / 22 (72.7%) |
| callback.c | 69 / 74 (93.2%) | 32 / 38 (84.2%) |
| prepare.c | 198 / 259 (76.4%) | 122 / 192 (63.5%) |
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
| expr.c | 19 / 22 (86.4%) | 15 / 24 (62.5%) |
| ctime.c | 3 / 3 (100.0%) | 0 / 0 (0.0%) |
| func.c | 8 / 8 (100.0%) | 2 / 2 (100.0%) |
| **total** | 2439 / 2966 (82.2%) | 1483 / 2016 (73.6%) |

| C işlevi | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 75.0 | 675 | 71.7 | 113 |
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
| sqlite3ExprAnd | expr.c | 13 | 84.6 | 8 | 62.5 | 3 |
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

## SELECT üst kuralı: parse.y:522–528 (yy_reduce tamamı sayılmadı)

| C koşulu | Çalışma sayacı | Alınan dallar |
|---|---|---|
| parse.y:523 | 104228 | 2/2 |
| parse.y:524 | 56572 | 2/2 |

Kuralda toplam 4/4 dal alındı. Üstteki işlev toplamına eklenmedi.

## Yürütülen sekiz işlem kodunun C blokları (sqlite3VdbeExec bütünü sayılmadı)

| İşlem kodu | Alınan dallar |
|---|---|
| OP_Goto | 6/8 |
| OP_Halt | 4/20 |
| OP_Integer | 0/0 |
| OP_String8 | 6/10 |
| OP_ResultRow | 2/4 |
| OP_Expire | 2/2 |
| OP_Init | 10/20 |

## V6b.24 — özel hata ve yeniden hazırlama blokları

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


**594 alınmayan dal** [karar_esligi_v6b_24_dallar.md](karar_esligi_v6b_24_dallar.md) içinde tek tek sınıflandırıldı. Sabit programdaki ALWAYS önkoşulları, NOMEM, genel UTF kodlama/dönüşüm ve önceki açık PRAGMA/pager yolları tamamlandı sayılmaz.

## Dört kapı

Çalışma alanı geçti/başarısız/yok sayıldı: kapalı (1117, 0, 337); açık (1102, 15, 337). C ikilisi PATH’teydi. Açık anahtardaki 15 başarısızın adları öncekiyle aynı. DML kapalı 378/400, açık 398/400; sınıf ve örnek kümeleri önceki adımla aynı. Bunlar bütün C farklarının kapandığı anlamına gelmez.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

Rust 1.100 tabanı; CPU6/tmpfs; yedi A/B; anahtar kapalı. 14 özet C ile aynı; hız ayarı yapılmadı. Bu ölçüm makinenin açık anahtardaki hızını veya M3b’nin bittiğini göstermez.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.30      8.93     0.959x     9.20     8.85     9.60    0.93x  OK
W02_create_indexes                5.75      5.68     0.988x     5.66     5.64     8.76    0.65x  OK
W03_pk_point_select              29.94     29.78     0.995x    29.62    29.46    23.30    1.28x  OK
W04_secondary_idx_range           9.45      9.63     1.019x     9.33     9.53     7.46    1.29x  OK
W05_full_scan_filter_agg          4.40      4.35     0.989x     4.39     4.34    16.57    0.26x  OK
W06_join_group_order             10.81     11.02     1.019x    10.77    10.99    28.23    0.39x  OK
W07_window_functions             37.09     37.46     1.010x    37.01    37.33    50.21    0.75x  OK
W08_indexed_update_delete        11.98     11.74     0.980x    11.91    11.72     7.96    1.48x  OK
W09_like_text_scan               12.48     12.41     0.994x    12.40    12.34    18.92    0.66x  OK
W10_jsonb_extract_agg            13.72     13.71     0.999x    13.64    13.67    10.93    1.25x  OK
W11_fts5_index_and_bm25          69.13     69.95     1.012x    69.07    69.25    57.02    1.23x  OK
W12_rtree_insert_and_bbox        40.15     40.75     1.015x    39.71    40.12    24.63    1.65x  OK
W13_autocommit_wal_txns           4.47      4.61     1.031x     4.43     4.45     4.12    1.12x  OK
W14_cli_sql_script               38.75     53.03     1.369x    38.38    52.52    26.19    2.03x  OK
TOTAL                           297.42    313.03     1.052x   geomean cand/base = 1.023x   digest mismatches = 0
```

## Açık işler ve kayıt

V6b açık: function_list/module_list, defer_foreign_keys yaşam döngüsü, case_sensitive_like, mmap/bellek sınırları, integrity/optimize gibi kalan yollar sürüyor. Genel işlev kaydı/override yaşam döngüsü, küçük uzunluk sınırı ve tahsis hatası yolları ayrıca kanıtlanmadı. D103, E28, M4, kalan veri/DDL/Unicode/CLI farkları ve eski yürütücünün kaldırılması açık. Tam C garantisi veya yayın tamamlanması iddiası yok.

Kanıt /home/hakan/github/sqliters/target/perf-work/v6b24/; sekiz kaynak/test dosyası res/source_manifest.json ile sabit. C kaynakları değiştirilmedi; ileriPlanlama değerlendirilmedi.
