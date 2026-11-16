# V6b.8 — locking_mode, pager yaşam döngüsü ve hazırlama hataları

C başvurusu SQLite 3.54.1; Rust araç zinciri 1.100. Bu adım locking_mode üretimini ve hazırlama etkilerini taşır. Bellek/TEMP/immutable korumaları, tutulan kilitte changeCountDone, fiziksel sayfa 1 damgası, PRAGMA sentaksı ve bağlantıdaki hata konumu geçmişi bu taşımanın gerektirdiği doğruluk düzeltmeleridir. Hız ayarı yapılmadı.

**Açık fark:** D103 nedeniyle bu dilim bütün yürütme yollarında C eşliği tamamlandı sayılmaz. Üretici ve anahtar açık makine yolu bu örnekte eşleşir; eski yürütücünün tüketilmeyen TVF argümanı yeniden denetimi eksiktir. Diğer mevcut test/DML/ayrışma kusurları ve E28 de açık.

## Karar tablosu

| C koşulu | Ayrım | Sonuç | Rust karşılığı |
|---|---|---|---|
| `pragma.c:110–117` | RHS yok, normal, exclusive veya başka sözcük | Yok/geçersiz RHS QUERY olur; yalnız iki tam sözcük kabul edilir. Boşluk kırpılmaz. | `crates/sqliters-codegen/src/pragma.rs:424` |
| `pragma.c:691–696` | Şema yok ve QUERY | Bağlantının dfltLockMode değeri döner; main pagerin exclusive olması sonucu değiştirmez. | `crates/sqliters-codegen/src/pragma.rs:426` |
| `pragma.c:699–717` | Şema yok ve geçerli atama | main ve 2..nDb pagerlerine uygulanır; TEMP atlanır; bağlantı varsayılanı yeni ATTACH için saklanır. | `crates/sqliters-core/src/lib.rs:5771` |
| `pragma.c:699–717` | Şema var | Yalnız seçilen pager sorgulanır/değiştirilir; global varsayılan korunur. | `crates/sqliters-core/src/lib.rs:5773` |
| `pragma.c:720–727` | Son pager sonucu exclusive / normal | OP_String8 ve OP_ResultRow sabit metni programa dondurur. | `crates/sqliters-codegen/src/pragma.rs:440` |
| `pragma.c:443–457` | Normal hazırlama / EXPLAIN; TEMP açık / kapalı | EXPLAIN de ayarı hazırlamada değiştirir; kapalı TEMP için C hata davranışı korunur. Normal TEMP açılışı gerçek duruma aktarılır. | `crates/sqliters-core/src/lib.rs:5754` |
| `pager.c:7404–7414` | eMode QUERY / atama; tempFile / WAL heap | QUERY değiştirmez. tempFile veya WAL heap atamayı reddeder; mevcut exclusive/normal döner. | `crates/sqliters-pager/src/lib.rs:1194` |
| `pager.c:5063–5070; 5114–5119` | Bellek, özel TEMP veya immutable | tempFile, noLock, exclusiveMode ve changeCountDone başlangıcı C gibi korunur. | `crates/sqliters-pager/src/lib.rs:599` |
| `pager.c:1147; 1156–1177` | noLock / dosya kilidi | Özel TEMP, bellek ve immutable için fiziksel VFS kilidi alınmaz; normal dosya kilitlenir. | `crates/sqliters-pager/src/lib.rs:684` |
| `pager.c:1133–1148` | xUnlock başarılı / başarısız; tempFile | Kilit bırakma changeCountDone değerini tempFile değerine döndürür. Hata yönünün tamamı bu derlemde enjekte edilmedi. | `crates/sqliters-pager/src/lib.rs:719` |
| `pager.c:6405–6445` | changeCountDone / dbSize>0 | Kilit tutulurken sonraki commit temiz sayfa 1’i zorla kirletmez. Sayfa 1 zaten kirliyse yazılır. | `crates/sqliters-pager/src/lib.rs:4729` |
| `pager.c:3138–3156; 4530` | Fiziksel yazılan sayfa 1 / başka sayfa | Kirli sayfa 1’in sayacı her fiziksel yazımda dbFileVers+1 olur; değişiklik sayacı/valid-for/SQLite sürümü birlikte yazılır. | `crates/sqliters-pager/src/lib.rs:1964` |
| `pager.c:4541–4542` | Sayfa 1 yazıldı | Başarılı fiziksel yazımdan sonra dbFileVers güncellenir. Başarısız yazım ve C ERROR durumunun tamamı E28 kapsamında açık. | `crates/sqliters-pager/src/lib.rs:1999` |
| `pager.c:3029; 6830–6863` | Dolu journal playback / boş WRITER_LOCKED rollback | Playback changeCountDone değerini sıfırlar; yazılmamış boş rollback tutulan kilidin bayrağını korur. | `crates/sqliters-pager/src/lib.rs:5058` |
| `pager.c:3481` | WAL rollback veya geçerli rollback-to-savepoint | Bayrak tempFile değerine döner. Geçersiz savepoint çağrısı durumu değiştirmez. | `crates/sqliters-pager/src/lib.rs:5004; :5381` |
| `backup.c:456; pager.c:6405,4530` | VACUUM/backup ve önceden tamamlanmış sayaç | Kopya başlığına zorla artış uygulanmaz; gerçek sayfa 1 yazımı C gibi damgalar. VACUUM ardından DML dosyası C ile eşleşir. | `crates/sqliters-pager/src/lib.rs:4002` |
| `pragma.c:2838–2875` | Result0, SchemaReq, Result1 bayrakları | pragma_locking_mode bir görünür sütun ve schema adlı gizli sütunla kaydolur; eşitlik kısıtı şemaya bağlanır. | `crates/sqliters-core/src/vtab.rs:3542` |
| `pragma.c:2897–2929` | Gizli argüman var / yalnız gizli şema | Şema sütunu görünür sütun sayısı + has_arg konumundadır; gerçek argüman numarası doğru gönderilir. | `crates/sqliters-core/src/vtab.rs:3682` |
| `pragma.c:2998–3018` | NULL argüman / metin; şema-only pragma | NULL şemasız sorgudur. Metin azArg[1] olarak PRAGMA şemasıdır; sonuç global varsayılandan farklı olabilir. | `crates/sqliters-core/src/vtab.rs:3775` |
| `pragma.c:3048–3051` | Görünür / gizli sütun | C schema-only pragma girdisini azArg[1]’e yazıp gizli ilk sütunda azArg[0] okur: schema çıktısı NULL kalır. Bu gözlenen C davranışı korunur. | `crates/sqliters-core/src/vtab.rs:3864` |
| `parse.y:1724–1741` | İşaretli sayı / işaretli sözcük veya dizge | İşaret yalnız INTEGER/FLOAT öncesinde geçerlidir; erken sentaks hatası TEMP açmaz ve PRAGMA ayarı uygulamaz. | `crates/sqliters-syntax/src/parser.rs:2102` |
| `parse.y:278–294; 334–340; 1732–1736` | nm fallback sözcükleri / NOTHING / CTIME_KW | NOTHING reddedilir; WITH ve CURRENT_DATE/TIME/TIMESTAMP geçerlidir. 147 anahtar sözcük üç işaret biçiminde sınandı. | `crates/sqliters-syntax/src/parser.rs:2116` |
| `parse.y:1724–1730; tokenize.c:648–700` | Geçerli PRAGMA indirgemesi sonrası fazla token | Sonraki token sentaks hatası verse de C’nin daha önce uyguladığı hazırlama etkileri korunur; TEMP açılması ve cache ayarı dahil. | `crates/sqliters-core/src/lib.rs:7458` |
| `prepare.c:821–842; util.c:136–164` | İleti oluşturan hata / iletisiz hata / başarı | İletili hata yeni byte offset yazar; iletisiz hata önceki bağlantı konumunu korur; başarılı prepare konumu -1 yapar. | `crates/sqliters-core/src/lib.rs:7417` |
| `sqliteInt.h:1713; prepare.c:821–842` | Yeni Parse nesnesi | Üretici önceki bağlantı hata konumunu devralır; ilk şema yüklemesi ve ErrorMsg kendi C kurallarıyla temizler. | `crates/sqliters-codegen/src/sqliteint.rs:1569` |
| `pragma.c:2897–2929; wherecode.c:2684–2732 (eski yürütücüde açık)` | TVF argümanı ile ikinci gizli sütun eşitliği | D103 açık: C’nin tüketmediği örtük TVF eşitliği eski yürütücüde tekrar sınanmıyor; anahtar açık VDBE yolu aynı girdiyi doğru çalıştırıyor. | `crates/sqliters-core/src/engine.rs:10518` |

## Program ve sonuç kanıtı

EXPLAIN 10093/10281 kabul edilen, 10093 aynı, 0 farklı; EQP 10093 aynı, 0 farklı. Desteklenmeyen ifadeler başarı sayısına katılmadı. Hazırlama/durum akışında 5964/5964 aynı. Tam VDBE programında 8249 aynı; 171 açık Unsupported; yeni program farkı yok.

İki anahtar durumunda 488 API yaşam döngüsünün 487 tanesinde bütün çıktı aynı; bir D103 farkı aşağıda. 488 akışın tamamında main/aux dahil oluşan bütün .db dosyaları bayt bayt aynı. Kalıcı cache ile 16 yeniden açma akışında çıktı ve dosyalar aynı. GÇ gözlemcisiyle 896 synchronous + 160 bağlantı/cache senaryosu aynı.

Girdiler normal hazırlama, EXPLAIN/EQP, yalnız hazırlayıp çalıştırmama, sonra başka ayar hazırlama, eski ifadeyi step/reset etme, global/main/aux/TEMP/bellek, 14 RHS biçimi, 147 sözcük × üç işaret, geç ve erken sentaks hataları, rollback/savepoint, altı journal kipinde VACUUM ve bağlı dosyaları kapsar. CLI akışları program/durum kontrolüdür; bütün satır çıktısı ve dosya karşılaştırması ayrıca API sürücüsüyle yapılır.

C’nin yerel API yoklamaları:

```text
CREATE TABLE t(a) => 1
PRAGMA locking_mode=EXCLUSIVE; INSERT INTO t VALUES(1) => 2
INSERT INTO t VALUES(2) => 2
BEGIN IMMEDIATE; ROLLBACK; INSERT INTO t VALUES(3) => 2
BEGIN; INSERT INTO t VALUES(4); ROLLBACK; INSERT INTO t VALUES(5) => 3
BEGIN IMMEDIATE; SAVEPOINT s; UPDATE t SET a=a; ROLLBACK TO s; RELEASE s; COMMIT => 3
INSERT INTO t VALUES(6) => 4
PRAGMA locking_mode=NORMAL => 4
SELECT * FROM t => 4
INSERT INTO t VALUES(7) => 5
PRAGMA locking_mode=EXCLUSIVE; INSERT INTO t VALUES(8) => 6
VACUUM; INSERT INTO t VALUES(9) => 7
PRAGMA locking_mode=NORMAL; SELECT * FROM t; INSERT INTO t VALUES(10) => 8
opened rc=12 lock=-1
begin rc=12 lock=-1
spilled write rc=0 lock=0
commit rc=0 lock=0
PRAGMA temp.locking_mode=NULL => 1 offset 25 near "NULL": syntax error
EXPLAIN PRAGMA temp.locking_mode => 1 offset 25 SQL logic error
SELECT 1 => 0 offset -1 not an error
EXPLAIN PRAGMA temp.locking_mode => 1 offset -1 SQL logic error
PRAGMA temp.locking_mode=NULL => 1 offset 25 near "NULL": syntax error
EXPLAIN PRAGMA temp.locking_mode => 1 offset -1 SQL logic error
```

Kilit yoklaması: EXCLUSIVE yalnız hazırlandığında diğer bağlantı yazabilir; ilk gerçek yazımdan sonra diğer bağlantı BUSY alır. NORMAL yalnız hazırlanınca BUSY sürer; aynı bağlantıda sonraki SELECT tamamlanınca kilit bırakılır. TEMP’in fiziksel dosya oluşturma zamanının bütün yönleri bu yoklamayla kanıtlanmış sayılmaz.

D103 açık örneği: `SELECT locking_mode,schema FROM pragma_locking_mode('aux') WHERE schema='main'`. C ve anahtar açık Rust sıfır satır; kapalı Rust bir `exclusive|NULL` satırı verir. Ham fark `res/manual_known.json` içinde; matris girdisi korunmuştur. Bu genel eski yürütücü eksikliği makineye geçişte giderilecek; C’den farklı beklenti eklenmedi.

## C kapsamı

| C file | lines covered | branches taken at least once |
|---|---|---|
| pragma.c | 818 / 1115 (73.4%) | 532 / 776 (68.6%) |
| btree.c | 199 / 229 (86.9%) | 95 / 126 (75.4%) |
| pager.c | 154 / 172 (89.5%) | 102 / 152 (67.1%) |
| pcache.c | 14 / 14 (100.0%) | 10 / 10 (100.0%) |
| main.c | 21 / 21 (100.0%) | 4 / 4 (100.0%) |
| vdbeaux.c | 38 / 45 (84.4%) | 14 / 18 (77.8%) |
| callback.c | 5 / 5 (100.0%) | 0 / 0 (0.0%) |
| prepare.c | 119 / 160 (74.4%) | 64 / 104 (61.5%) |
| tokenize.c | 58 / 75 (77.3%) | 37 / 54 (68.5%) |
| util.c | 25 / 29 (86.2%) | 8 / 12 (66.7%) |
| **total** | 1451 / 1865 (77.8%) | 866 / 1256 (68.9%) |

| C function | file | lines | line % | branches | taken % | source lines with a branch never taken |
|---|---|---|---|---|---|---|
| getSafetyLevel | pragma.c | 9 | 100.0 | 12 | 100.0 | 0 |
| sqlite3GetBoolean | pragma.c | 2 | 100.0 | 0 | 100.0 | 0 |
| setPragmaResultColumnNames | pragma.c | 8 | 100.0 | 6 | 100.0 | 0 |
| returnSingleInt | pragma.c | 4 | 100.0 | 0 | 100.0 | 0 |
| pragmaLocate | pragma.c | 12 | 100.0 | 8 | 100.0 | 0 |
| actionName | pragma.c | 9 | 100.0 | 5 | 100.0 | 0 |
| sqlite3Pragma | pragma.c | 946 | 70.0 | 675 | 66.4 | 129 |
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
| btreeEndTransaction | btree.c | 15 | 86.7 | 8 | 75.0 | 2 |
| sqlite3BtreeCommitPhaseTwo | btree.c | 15 | 86.7 | 8 | 62.5 | 1 |
| sqlite3BtreeRollback | btree.c | 22 | 95.5 | 14 | 78.6 | 3 |
| sqlite3BtreeSavepoint | btree.c | 16 | 93.8 | 14 | 64.3 | 3 |
| lockBtree | btree.c | 73 | 67.1 | 44 | 61.4 | 16 |
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
| sqlite3InitOne | prepare.c | 92 | 85.9 | 58 | 69.0 | 13 |
| getLockingMode | pragma.c | 5 | 100.0 | 6 | 100.0 | 0 |
| returnSingleText | pragma.c | 5 | 100.0 | 2 | 50.0 | 1 |
| sqlite3PagerLockingMode | pager.c | 4 | 100.0 | 6 | 83.3 | 1 |
| pragmaVtabConnect | pragma.c | 34 | 82.3 | 12 | 50.0 | 6 |
| pragmaVtabBestIndex | pragma.c | 27 | 88.9 | 14 | 71.4 | 4 |
| pragmaVtabFilter | pragma.c | 26 | 84.6 | 14 | 71.4 | 4 |
| pragmaVtabColumn | pragma.c | 7 | 100.0 | 2 | 100.0 | 0 |
| sqlite3PragmaVtabRegister | pragma.c | 5 | 100.0 | 4 | 50.0 | 2 |
| pagerUnlockDb | pager.c | 8 | 100.0 | 6 | 83.3 | 1 |
| pager_incr_changecounter | pager.c | 11 | 100.0 | 8 | 62.5 | 3 |
| pager_write_changecounter | pager.c | 6 | 100.0 | 2 | 50.0 | 1 |
| pager_write_pagelist | pager.c | 25 | 100.0 | 24 | 83.3 | 3 |
| pagerPlaybackSavepoint | pager.c | 37 | 64.9 | 44 | 36.4 | 15 |
| sqlite3PagerRollback | pager.c | 18 | 83.3 | 16 | 62.5 | 5 |
| sqlite3RunParser | tokenize.c | 75 | 77.3 | 54 | 68.5 | 10 |
| sqlite3Prepare | prepare.c | 68 | 58.8 | 46 | 52.2 | 13 |
| sqlite3Error | util.c | 6 | 100.0 | 4 | 75.0 | 1 |
| sqlite3ErrorClear | util.c | 5 | 100.0 | 2 | 100.0 | 0 |
| sqlite3ErrorMsg | util.c | 18 | 77.8 | 6 | 50.0 | 2 |

Kapsam toplamı genel parser/pager/şema ve hâlâ taşınmayan işleyicileri de içerir; C eşliği tamamlanma yüzdesi değildir. Hiç alınmayan her dal [karar_esligi_v6b_8_dallar.md](karar_esligi_v6b_8_dallar.md) içinde ayrı sınıflandırıldı. NOMEM, authorizer, özel VFS PRAGMA override, WAL heap kip değişimi ve genel GÇ hata durumlarının tamamı kanıtlanmadı. Özellikle E28 pager ERROR durumu bu değişiklikle kapanmaz.

## Dört kapı

Testler (geçti/başarısız/yok sayıldı): anahtar kapalı (1071, 0, 337); açık (1056, 15, 337). Açık 15 başarısız testin adları önceki adımla aynı. DML kapalı 378/400, açık 398/400; başarısız seed/sınıf kümeleri öncekiyle aynı. Beklentisi değişen iki düşük seviye TEMP kilit doğrulaması C’nin noLock=1 davranışına bağlandı; F4, F6–F9 beklentilerine dokunulmadı.

```text
245 scenarios {'L0': 242, 'L1': 1, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 244
reference: 245 {'L0': 241, 'L1': 2, 'L4': 1, 'L6(none)': 1} stdout_equal 245 stderr_equal 245
DIFF t/g06_vacuum_into_main_untouched.json ('L0', True, False, {}) ('L0', True, True, {})
DIFF t/j_exclusive_locking.json ('L0', True, True, {}) ('L1', True, True, {'L0': ['header change_counter: Rust 204, C 5', 'header version_valid_for: Rust 204, C 5']})
scenarios differing from the reference: 2
```

CPU 6, tmpfs, Rust 1.100 tabanına karşı 7 A/B denemesi; anahtar kapalı. Bütün sonuç özetleri eşleşir. Hız ayarı uygulanmadı.

```text
workload                       base ms   cand ms  cand/base    min b    min c     C ms   cand/C  digest
W01_bulk_insert_prepared          9.34      8.96     0.960x     9.28     8.82     9.60    0.93x  OK
W02_create_indexes                5.71      5.78     1.013x     5.67     5.69     8.76    0.66x  OK
W03_pk_point_select              30.25     30.09     0.995x    29.43    29.44    23.30    1.29x  OK
W04_secondary_idx_range           9.50      9.62     1.013x     9.40     9.35     7.46    1.29x  OK
W05_full_scan_filter_agg          4.40      4.37     0.993x     4.39     4.35    16.57    0.26x  OK
W06_join_group_order             10.79     10.93     1.013x    10.78    10.91    28.23    0.39x  OK
W07_window_functions             37.28     37.78     1.013x    37.15    37.45    50.21    0.75x  OK
W08_indexed_update_delete        12.05     12.07     1.002x    11.95    11.92     7.96    1.52x  OK
W09_like_text_scan               12.45     12.41     0.997x    12.40    12.36    18.92    0.66x  OK
W10_jsonb_extract_agg            13.77     13.49     0.980x    13.73    13.41    10.93    1.23x  OK
W11_fts5_index_and_bm25          69.46     71.17     1.025x    69.14    70.33    57.02    1.25x  OK
W12_rtree_insert_and_bbox        40.40     40.50     1.003x    39.90    40.23    24.63    1.64x  OK
W13_autocommit_wal_txns           4.65      4.66     1.003x     4.52     4.44     4.12    1.13x  OK
W14_cli_sql_script               39.21     39.45     1.006x    38.84    39.07    26.19    1.51x  OK
TOTAL                           299.24    301.30     1.007x   geomean cand/base = 1.001x   digest mismatches = 0
```

## Yeniden üretme ve sınırlar

Yerel kanıt kökü `/home/hakan/github/sqliters/target/perf-work/v6b8/`: SQL ve connection/locking *.ops girdileri, C ve Rust API sürücüleri, özel VFS gözlemcileri, gcov kopyası, res altındaki ham çıktılar ve bütün .db dosyalarının SHA256 özetleri. `res/source_manifest.json` son test edilen on kaynak/test dosyasının SHA256 değerlerini tutar. C 3.54.1 PATH başında, workspace testleri iki anahtarda son kaynakla çalıştırıldı. Kaynak C ağacı değiştirilmedi; ileriPlanlama değerlendirilmedi.

V6b devam ediyor. Sonraki doğruluk dilimi temp_store hazırlama etkisidir; ardından kalan PRAGMA işleyicileri ve M4. Performans çalışması ancak ilgili kodun doğruluğu kanıtlandıktan sonra yapılacak.
