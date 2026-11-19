# `performans-A-vdbe` — C'nin kod üreticisi ve VDBE'sinin taşınması: ilerleme raporu

> **Bu dal ne için.** Motor bugün C'nin VDBE programlarını üretip çalıştırmıyor; ifadeleri kendi yürütücüsüyle değerlendiriyor. Bunun gözlenebilir sonuçları (`EXPLAIN` çıktısı, planlayıcı / `EXPLAIN QUERY PLAN`, satır sırası, hatadan önce basılan satırlar, önbellek taşmasında yazılan sayfa) C'den farklı. Karar (2026-11-15): C'nin kod üreticisi ve VDBE'si adım adım taşınır; mevcut yürütücü ve hızlı yollar taşıma bitene kadar yürütme yolu olarak kalır.
>
> **Ölçüt.** Kod üretici hattında: `EXPLAIN <ifade>` çıktısının C SQLite 3.54.1 ile bayt bayt aynı olması. Yeni üretici bir ifadeyi tümüyle destekliyorsa `EXPLAIN` yeni listeyi basar; desteklemiyorsa önceki çıktı kalır. Makine hattında: programların çalıştırılıp sonuçların ve işlem kodu izinin C ile karşılaştırılması.

- **Dal tabanı**: `performans-A-hizalama@3b78e5c` (H6b).
- **Ortam**: `Intel Xeon E5-2680 v4`, C referansı SQLite `3.54.1` (`-O3`).

## Özet

| Adım | Commit | Kapsam | `EXPLAIN` C ile aynı (adımın sınıfı) | Tüm ifade türlerinde üreticinin kabul ettiği | Testler |
| :--- | :--- | :--- | ---: | ---: | :--- |
| V1 — temel, ifadeler, `FROM`'suz `SELECT` | `bdffdc1` | program gösterimi ve kurucu, `EXPLAIN` listesi, C'nin ayrıştırma ağacı, `resolve.c`, `expr.c`, `VALUES`, işlem ifadeleri | 54.469 / 54.469 kabul edilen (144 desteklenmiyor) | 3.954 / 39.300 (%10,1) | 907 geçti, bilinen 4 başarısız, 337 yok sayıldı |
| V2 — tablolar: şema bağdaştırıcısı, sıralayıcı, `where.c` ailesi, birleşimler | `c4efe5e` | `whereexpr.c`, `where.c`, `wherecode.c`, `select.c` (GROUP BY hariç), alt sorgu düzleştirme, görünümler, `EXPLAIN QUERY PLAN` | 35.525 / 35.526 kabul edilen (3.474 desteklenmiyor) | 13.659 / 39.300 (%34,8) | 929 geçti, bilinen 4 başarısız, 337 yok sayıldı |
| V3 — şemanın C gibi yüklenmesi, GROUP BY / HAVING / FILTER, özyinelemeli CTE | `a0dfd44` | `build.c` tablo/indeks/görünüm kurucuları, `sqlite3Init`, `sqlite3AnalysisLoad`, GROUP BY sıralayıcı dalı, FILTER, `generateWithRecursiveQuery`, C bağlantı durumu (DBFLAG_EncodingFixed) | 87.000 / 87.000 kabul edilen (8 derlem; 0 farklı) | 14.133 / 39.300 (%36,0) | 979 geçti, bilinen 4 başarısız, 337 yok sayıldı |
| V4a — pencere işlevleri | `f7df02e` | `window.c`'nin üretici tarafı (yeniden yazım, adlandırılmış / zincirlenmiş pencereler, `sqlite3WindowCodeInit` / `CodeStep` ve yardımcıları, ROWS / RANGE / GROUPS, EXCLUDE), ayrıştırıcı kuralları 93, 318–345 | 24.012 / 24.012 (pencere derlemleri) + V3'ün 87.000'i | 14.532 / 39.300 (%37,0) | 1002 geçti, bilinen 4 başarısız, 337 yok sayıldı |
| V4b — FROM içinde sanal tablolar ve tablo değerli işlevler | `628a5f5` | `vtab.c` okuma tarafı, CREATE VIRTUAL TABLE şema yüklemesi, `whereLoopAddVirtual` / `xBestIndex`, sanal tablo döngü kodu ve EQP | 23.640 / 23.642 (vtab derlemleri; 2 = D34) + önceki 111.012 | 14.773 / 39.300 (%37,6) | 1011 geçti, bilinen 4 başarısız, 337 yok sayıldı |
| V5 — DML, UPSERT, RETURNING, tetikleyiciler, yabancı anahtarlar | `4b1ffa5` | `delete.c`, `insert.c`, `update.c`, `upsert.c`, `trigger.c`, `fkey.c` (DML tarafı), RETURNING, ayrıştırıcı kuralları 76, 131, 152, 157–174, 260–277, 309/310 | 65.200 / 65.200 (DML derlemleri; EQP'de 3 = D48) + program düzeyi 62.308 / 62.308 + önceki 134.652 / 134.654 | 26.811 / 39.300 (%68,2) | 1018 geçti, 0 başarısız, 337 yok sayıldı |
| V6 — DDL: CREATE / DROP / ALTER / ANALYZE / VACUUM / ATTACH / REINDEX | `2d1ede0` | `build.c` DDL kodu, `alter.c`, `analyze.c`, CREATE / DROP TRIGGER, `OP_VCreate`, `sqlite3FkDropTable`, ATTACH / DETACH, VACUUM (PRAGMA V6b'ye kaldı) | 28.367 / 28.367 (DDL derlemleri) + program düzeyi 25.107 / 25.107 | 36.329 / 39.300 (%92,4) | 1032 geçti, 0 başarısız, 337 yok sayıldı |
| V6b.1 — PRAGMA ilk dilimi; V6b açık | `84daa83` | 66 adlık kayıt tablosu; bayraklar, analysis_limit, başlıklar, tablo/indeks/FK bilgileri, journal_mode, checkpoint ve optimize'nin geçmiş gerektirmeyen yolları | 3.695 / 3.695 (297 desteklenmiyor); tam program 3.667 / 3.667 | Bu dilimde yeniden ölçülmedi | kapalı 1037/0; açık 1022/15 bilinen; 337 yok sayıldı |
| V6b.2 — PRAGMA önbellek/günlük/güvenli silme; V6b açık | `f1a2e58` | cache_size, journal_size_limit, secure_delete; hazırlama etkileri ve sabit sonuçlar; D83 sıfır önbelleğin yeniden başlatılması | 4.178 / 4.178 (264 desteklenmiyor); tam program 4.176 / 4.176 | Bu dilimde yeniden ölçülmedi | kapalı 1040/0; açık 1025/15 bilinen; 337 yok sayıldı |
| V6b.3 — default_cache_size hazırlama ve çerez yazımı | `556932a` | Hazırlamada önbellek, adımlamada donmuş çerez; reset, TEMP ve i32 sınırları | 4.274/4.274; tam program 4.272/4.272 | Yeniden ölçülmedi | kapalı 1043/0; açık 1028/15 bilinen; 337 yok sayıldı |
| V6b.4 — synchronous hazırlama ve eşitleme bayrakları | `a608629` | C sayısal maske, işlem hatası, TEMP; fullfsync/CKPT ve FULL+DATAONLY | 4.829/4.829; tam program 4.589/4.589; 896 GÇ senaryosu aynı | Yeniden ölçülmedi | kapalı 1045/0; açık 1030/15 bilinen; 337 yok sayıldı |
| V6b.5 — page_size hazırlama ve global VACUUM isteği | `94421ed` | Ham açılış başlığı, donmuş sorgu, TEMP, geçersiz istek, şemalar arası VACUUM ve WAL sıfırı | 5.545/5.545; tam program 5.129/5.129; 176 API akışı aynı | Yeniden ölçülmedi | kapalı 1049/0; açık 1034/15 bilinen; 337 yok sayıldı |
| V6b.6 — auto_vacuum hazırlama ve canlı Btree durumu | `9a222cd` | Global nextAutovac, donmuş meta7, TEMP, commit/rollback/savepoint ve yeniden okuma | 6.516/6.516; tam program 5.860/5.860; 220 API akışı aynı | Yeniden ölçülmedi | kapalı 1057/0; açık 1042/15 bilinen; 337 yok sayıldı |
| V6b.7 — bağlantı/cache PRAGMA hazırlama etkileri | `8fdf60f` | cache_spill, busy_timeout, wal_autocheckpoint; TEMP, EXPLAIN ve ilk kodlama yüklemesi | 7.701 EXPLAIN/EQP, 6.757 tam program, 330+16 API aynı | Yeniden ölçülmedi | kapalı 1063/0; açık 1048/15 bilinen; 337 yok sayıldı |
| V6b.8 — locking_mode ve hazırlama hataları; D103 açık | `d9f13d7` | Global/yerel kip; tempFile, sayaç, sentaks ve hata konumu | 10093 EXPLAIN/EQP; 8249 tam program; API 487/488 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1071/0; açık 1056/15 bilinen; 337 yok sayıldı |
| V6b.9 — temp_store hazırlama ve şema temizleme | `94c2949` | İlk karakter, TEMP işlem koruması, sabit sonuç ve cache_size koruma | 11832 EXPLAIN/EQP; 9216 tam program; API 1047/1048 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1076/0; açık 1061/15 bilinen; 337 yok sayıldı |
| V6b.10 — encoding hazırlama ve advisory expire; D111 açık | `f3762ab` | Aliaslar, şema/kodlama geri yükleme, dosya kilidi, bekleyen/çalışan ifadeler | 13493 EXPLAIN/EQP; 10012 tam program; API 1721/1722 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1083/0; açık 1068/15 bilinen; 337 yok sayıldı |
| V6b.11 — doğrulama hazırlama durumu; D112/D113 açık | `ad50ddd` | D111, ilk sanal tablo ve şema çerezi önizlemesi | 11172 tam program aynı; yeni bozuk şema farkları açık | Yeniden ölçülmedi | kapalı 1087/0; açık 1072/15 bilinen; 337 yok sayıldı |
| V6b.12 — şema başlatma sırası; D112 açık | `694b9c2` | D113: yeni yükleme ve açık işlemde main/bağlı/TEMP hata önceliği | 13621 EXPLAIN/EQP; 11240 tam program; API2235/2236 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1092/0; açık 1077/15 bilinen; 337 yok sayıldı |
| V6b.13 — SELECT şema/Btree kararı; D115 açık | `c56c2cb` | D112/D114 UTF-8, bekleyen ifadeler ve VALUES | 13957 EXPLAIN/EQP; 11534 tam program; API2913/2914 aynı; bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1097/0; açık 1082/15 bilinen; 337 yok sayıldı |
| V6b.14 — UTF-16 boş yürütme yolu ve sayaçlar | `801d13a` | D115/D116; parametre, reset/expire, ilerleme ve sayaçlar | 14077 EXPLAIN/EQP; 11654 tam program; API3075/3076 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1100/0; açık 1085/15 bilinen; 337 yok sayıldı |
| V6b.15 — threads hazırlama ve sınır | `f539575` | C sayı kabulü, maske, 8 sınırı, donmuş satır, pragma_threads | 15456 EXPLAIN/EQP; 13009 tam program; API3995/3996 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1103/0; açık 1088/15 bilinen; 337 yok sayıldı |
| V6b.16 — shrink_memory hazırlama etkisi | `1c0e7e8` | Bütün açık pager’lar, reset/EXPLAIN ayrımı; D117 açık hata farkı | 15632 EXPLAIN/EQP; 13181 program; API 4157/4158 aynı, bütün dosyalar aynı | Yeniden ölçülmedi | kapalı 1104/0; açık 1089/15 bilinen; 337 yok sayıldı |
| V6b.17 — D117 sıcak başlık kontrolü | `565ab23` | 16 byte sürüm, retained reader ve pending WAL; D118 açık | 13181 program; API 4157/4158 aynı, bütün dosyalar aynı; 30 hata yoklaması | Yeniden ölçülmedi | kapalı 1107/0; açık 1092/15 bilinen; 337 yok sayıldı |
| V6b.18 — D119 sayfa1 hata aktarımı | `a7a7cbd` | Okuyucu IOERR rollback, reprepare ayrımı ve callback/sayaçlar | 13181 program; API 4157/4158 aynı, bütün dosyalar aynı; 56/16/64 native | 1,063× taban; W14 1,362× | kapalı 1110/0; açık 1095/15 bilinen; 337 yok sayıldı |
| V6b.19 — D118 kısa okuma | `09bbf9b` | DB/WAL SHORT_READ ayrımı, hata metinleri | 640 akış: 624 aynı, 16 açık; bütün dosyalar aynı | 1,069× taban; W14 1,379× | kapalı 1112/0; açık 1097/15 bilinen; 337 yok sayıldı |
| V6b.20 — sayfa1 cache ve bozuk kayıt | `6a56a70` | D120/D121/D122; SharedLock aşaması | 640+128 native aynı; bütün dosyalar aynı; 13181 program | 1,064× taban; W14 1,361× | kapalı 1113/0; açık 1098/15 bilinen; 337 yok sayıldı |
| V6b.21 — database_list | `711360d` | Hazırlama anı, self/advisory Expire, veri okumayan liste, geçerli UTF-16 metin | 13192 tam program; 12+210+12 native aynı; bütün dosyalar aynı | 1,059× taban; W14 1,363× | kapalı 1114/0; açık 1099/15 bilinen; 337 yok sayıldı |
| V6b.22 — collation_list ve kabuk karşılaştırıcıları | `12494d3` | C Hash geçmişi, UTF-8 BUSY/expire, uint/decimal | 13314 tam program +1362 geçmiş programı; 426/438 native (D128 açık); 7536 comparator; 36 shell; bütün dosyalar aynı | 1,051× taban; W14 1,362× | kapalı 1115/0; açık 1100/15 bilinen; 337 yok sayıldı |
| V6b.23 — D128 hazırlama ve hata kodu | `92b69eb` | Parse.rc, atılan COLLATE, dar tamsayı makine yolu | 13344 tam program; 636+438 native aynı; bütün dosyalar aynı | 1,047× taban; W14 1,364× | kapalı 1116/0; açık 1101/15 bilinen; 337 yok sayıldı |
| V6b.24 — compile_options | `3a17161` | 42 seçenek, get/used, Reusable ve hata konumu | 13372 tam program; 3990 native aynı; bütün dosyalar aynı | 1,052× taban; W14 1,369× | kapalı 1117/0; açık 1102/15 bilinen; 337 yok sayıldı |
| V6b.25 — defer_foreign_keys; D131 açık | bu commit | Prepare/live bayrak, normal FK/işlem sonu, RESTRICT yazım sırası | 13414 tam program; 1624/1644 native aynı (20 D52); bütün dosyalar aynı; D131 6 fark açık | 1,050× taban; W14 1,358× | kapalı 1118/0; açık 1103/15 bilinen; 337 yok sayıldı |
| V6b.26 — EXPLAIN yazıcı ömrü | bu commit | RUN/EOF/reset/finalize, changeCntOn, bekleyen işlem kapanışı | 72+408 native aynı; 120/192 kapanış tam aynı, 72 D132; eski altı D131 aynı | 1.060x taban; W14 1.369x | kapalı 1119/0; açık 1104/15 bilinen; 337 yok sayıldı |
| V6b.27 — commit hook hata metni | bu commit | D132 varsayılan 531 metni | 192/192 EXPLAIN; 26/36 doğrudan aynı, 10 D133; önceki 72+408 aynı | 1.051x taban; W14 1.367x | kapalı 1120/0; açık 1105/15 bilinen; 337 yok sayıldı |
| V6b.28 — savepoint ve callback autocommit | bu commit | D133/D134 kontrol bayrağı ve SQL savepoint ömrü | 144+72+96 ve eski 36 akış tam aynı | 1.058x taban; W14 1.369x | kapalı 1121/0; açık 1106/15 bilinen; 337 yok sayıldı |

## V1 — kod üreticinin temeli

**Taşınan.**
- Program gösterimi (`crates/sqliters-vdbe/src/vdbeaux.rs`, `opcodes.rs`): `VdbeOp`, tüm P4 türleri, 191 işlem kodu (referans kaynaktan üretilmiş), `sqlite3VdbeAddOp*` ailesi, etiketler, `resolveP2Values`, `sqlite3VdbeMakeReady`, `sqlite3VdbeList` / `sqlite3VdbeDisplayP4`. Gelecekteki makine bu gösterimi çalıştıracak; `sqliters-core`a bağımlı değil.
- Yeni paket `crates/sqliters-codegen` (dış bağımlılık yok): `tokenize.c`, `parse.y` (lemon sürücüsü; 348 eylem kuralının 176'sı), `expr.c` (106 işlev), `resolve.c` (18), `walker.c`, `callback.c`, `select.c` (35; `FROM`'suz yol, bileşik seçimler, `VALUES`), `where.c`in sıfır tablolu yolu, `sqlite3FinishCoding`.
- Motor bağlantısı (`crates/sqliters-core/src/cgen.rs`): `EXPLAIN <ifade>` — üretici ifadeyi destekliyorsa C'nin listesi; üretici C'nin derleme hatasını veriyorsa o hata (ileti ve konum); desteklemiyorsa önceki çıktı. `EXPLAIN` olmayan ifadeler üreticiye hiç ulaşmaz; yürütme değişmedi.

**Kanıt.**

| Ölçüm | Sonuç |
| :--- | :--- |
| `EXPLAIN` farksalı, kitaplık düzeyi (54.613 ifade: ifadeler, işlevler, seçimler, işlem ifadeleri, sınırlar, hatalı ifadeler) | kabul edilen 54.469, C ile bayt bayt aynı 54.469, farklı 0, desteklenmiyor 144 |
| `EXPLAIN` farksalı, CLI düzeyi | kabul edilen 53.849, aynı 53.847; farklı 2 (`regexp()` yalnız C kabuğunda kayıtlı) |
| Bağımsız kontrol (ayrı üretilmiş 4.000 rastgele ifade, CLI) | C ile aynı 3.668; önceki çıktıya düşen 332 (`FROM` içinde alt sorgu); yeni ama farklı 0 |
| Ayrıştırma ağacı eşitliği (C'nin ağaç dökümüyle) | `parse.y` sonrası 51.482 / 51.482; `sqlite3SelectPrep` sonrası 41.818 / 41.818 |
| C tarafı kapsamı (gcov, taşınan işlevler) | satır %69,7 (5.466 / 7.845), dal %59,6 (3.194 / 5.360); alınmayan dal taşıyan 1.223 satır "doğrulanmadı" |
| Tüm ifade türleri (39.300 ifade: `sqltest`, C test dosyaları, DML üreteci, depo testleri) | üretici kabul 3.954 (%10,1); aynı 3.798; farklı 156 (150'si yalnız C kabuğunda olan işlevler, 5'i eski motorun hata bağlamı, 1'i aşağıdaki konum farkı) |
| Yürütme değişmedi | `EXPLAIN`'siz 4.000 ifadenin çıktısı önceki ikiliyle aynı; ayrışma bulucu 241 / 245 (H6b ile aynı); DML farksalı 377 / 400; speedbench 14 özet C ile eşit |

Desteklenmeyen 144 ifadenin nedenleri: `ORDER BY`da `COLLATE` içeren bileşik seçim 88, sıralayıcı gerektiren `ORDER BY` 36, pencere işlevi 8, `FROM` terimi 7, `IN tablo` 4, diğer 1.

**Değişen test beklentileri (2).** Çalışma alanı paket listesine yeni paket eklendi; C'nin reddettiği bir ifadenin `EXPLAIN`i (`SELECT aux.items.* …`, C: `near "*": syntax error`) C'nin kabul ettiği biçimle değiştirildi.

**V1'de C ile aynı olmayan.**
- Bağlantının şeması henüz yüklenmemişken ilk ifadenin hata konumu: `EXPLAIN SELECT #0;` C'de konum ve işaret satırları olmadan, motorda işaret satırlarıyla basılıyor (ikinci aynı ifade iki tarafta aynı). C'nin şema yükleme durumunu gerektiriyor (`prepare.c`).
- Geçerli UTF-8 olmayan P4 metni (blob sabiti): varsayılan kiplerde çıktı aynı; `.mode quote` altında C `'A<AB><CD>B'`, motor `x'41abcd42'`.

**Taşınmayan (üretici "desteklenmiyor" der, yaklaşık üretim yok).** Sıralayıcı; tablo içeren her `FROM` terimi ve `where.c` planlaması; pencere işlevleri; CTE'ler; tetikleyiciler; tüm DDL, DML, `PRAGMA`; sonuç sütun adları; şema nesnesi bağdaştırıcısı.

Ayrıntı: `karar_esligi_v1.md` (işlev eşlemesi, karar tabloları, kapsam tabloları).

## V2 — `FROM` içinde tablolar

**Taşınan** (`crates/sqliters-codegen/src/`, 274 C işlevi; ayrıntı `karar_esligi_v2.md`).
- `schema.rs`: şema bağdaştırıcısı — motorun şemasından C'nin `Table` / `Column` / `Index` nesneleri (`build.c` mantığıyla; indeks metni iç içe ayrıştırıcıyla çözülüyor; `sqlite_stat1` yüklemesi `sqlite3AnalysisLoad` gibi).
- `whereexpr.rs` (sanal tablo kısımları hariç tamamı), `where_.rs` (`whereLoopAddBtree`, OR döngüleri, otomatik indeks, Bloom süzgeçleri, `wherePathSolver`, gereksiz birleşimin atılması, yıldız sorgu sezgisi), `wherecode.rs` (sanal tablolar dışındaki tüm plan türleri, OR iyileştirmesi, sağ birleşim, EQP metni).
- `select.rs`: tam `sqlite3Select` (GROUP BY hariç), `selectExpander`, birleşim işleme, sıralayıcı (`pushOntoSorter`, `generateSortTail`), `convertCompoundSelectToSubquery`, eş yordam / somutlaştırma kararı; `flatten.rs`: `flattenSubquery`, `pushDownWhereTerms`, `propagateConstants`, `countOfViewOptimization`, `existsToJoin`.
- Motor tarafı: `EXPLAIN QUERY PLAN` da artık aynı kuralla yeni üreticiden geliyor; bağlantı başına C'nin şema çerezi / nesil / "şema yüklendi" durumu; `PRAGMA automatic_index`, `reverse_unordered_selects`, `short_column_names`, `full_column_names` artık değer tutuyor ve sorgu biçiminde C'nin çıktısını veriyor.

**Kanıt (katmanlı).** Planlayıcı derlemi: 780 üretilmiş şema üzerinde 39.000 ifade.

| Katman | Karşılaştırılan | Aynı | Farklı |
| :--- | ---: | ---: | ---: |
| `sqlite3WhereExprAnalyze` sonrası WHERE terimleri | 32.008 | 32.008 | 0 |
| `whereLoopInsert`e sunulan her `WhereLoop` (maliyetlerle) | 31.443 | 31.435 | 8 |
| Her `wherePathSolver` çağrısının sonucu | 31.513 | 31.509 | 4 |
| `EXPLAIN QUERY PLAN` baytları (CLI) | 35.526 | 35.525 | 1 |
| `EXPLAIN` baytları (CLI) | 35.526 | 35.525 | 1 |

- Tek liste farkı ve döngü/yol farklarının izlenen dördü motorun `ANALYZE` kusurundan (tek sütunlu, NULL içeren `UNIQUE` indeks için `N 2` yazıyor, C `N 1`); kalan döngü farkları aynı imzayı taşıyor ama tek tek izlenmedi.
- Desteklenmeyen 3.474: tablo üzerinde GROUP BY 3.243; WITHOUT ROWID + tek sütunlu INTEGER birincil anahtarda otomatik indeks numaralaması kusuru 189; birincil anahtardan önce yazılmış `UNIQUE` 42.
- **Bağımsız kontrol** (C'nin kurduğu 40 rastgele şema, 2.400 ifade: tek tablo, 2–4 tablolu birleşimler, alt sorgular, bileşik seçimler): `EXPLAIN` C ile aynı 2.390, önceki çıktıya düşen 10, yeni ama farklı 0; `EXPLAIN QUERY PLAN` aynı 2.392, düşen 8, farklı 0.
- **Tüm ifade türleri** (39.300): üretici kabul **%10,1 → %34,8** (13.659). `EXPLAIN` aynı 13.279, farklı 380: 214'ü yalnız C kabuğunda olan işlev/sıralama, 39'u yalnız C kabuğunda olan sanal tablo modülleri, 82'si yalnız `OP_Transaction` çerez/nesil işleneni, kalanı kök sayfa / varsayılan değer gibi eski motor farkları. Bu sınıflar örneklenerek açıklandı, 380'in tamamı tek tek izlenmedi.
- **C tarafı kapsamı** (gcov; 274 işlev): satır %79,8, dal %71,4 (`where.c` %93,2 / %85,8; `wherecode.c` %89,5 / %82,6). Alınmayan dallar "doğrulanmadı" listesinde: sanal tablo dalları, GROUP BY / pencere / özyinelemeli CTE dalları, `UPDATE`/`DELETE` çağıranları, STAT4, hata dalları.
- **Yürütme değişmedi:** ayrışma bulucu 241 / 245; DML farksalı 377 / 400; speedbench özetleri C ile eşit; testler 929 geçti (22 yeni), bilinen 4 başarısız, 337 yok sayıldı.

**Değişen test beklentileri (3; C ile doğrulandı).** WITHOUT ROWID `SELECT`inde `OpenReadIdx` yerine C'nin `OpenRead` + KeyInfo'su; kapsayan indeks planında C'nin işlem kodu listesi; kısmi indeks: `b > 10` ve `b = 7` için C `SCAN ptest` verir (eski beklenti motorun kendi aralık çıkarımıydı).

**C'nin planı ile eski yürütücünün planı.** Kabul edilen 10.680 ifadenin 8.067'sinde plan metni farklı (birleşim sırası; C'nin otomatik indeks + Bloom süzgeci kullandığı yerde eski yürütücü tarıyor; C'nin indeks/satır kimliğiyle aradığı yerde tarama). `EXPLAIN QUERY PLAN` artık C'nin planını basıyor, yürütme taşıma bitene kadar eski planla çalışıyor.

**Bilinen eksikler.** `OP_Transaction` nesil işleneni şu durumlarda izlenmiyor: başka bağlantının şema değişikliği, çerez değiştikten sonra başarısız olan diğer DDL, `ATTACH`/`DETACH` hataları. Üretici "desteklenmiyor" diyor: tablo üzerinde GROUP BY / HAVING, pencere işlevleri, özyinelemeli CTE, sanal tablolar, DML, DDL, `PRAGMA`, VIRTUAL üretilmiş sütunlu tablolar, `COLLATE BINARY` açıkça yazılmış tablolar. Bu adımda görülen eski motor farkları: `gorulen_farklar_v2.md`.

## V3 — şemanın C gibi yüklenmesi, GROUP BY / HAVING / FILTER, özyinelemeli CTE

**Taşınan** (`crates/sqliters-codegen/src/`; 71 yeni/değişen C işlevi, toplam 497; ayrıntı `karar_esligi_v3.md`).
- `build_tab.rs` (yeni): `sqlite3StartTable` … `sqlite3EndTable`, `convertToWithoutRowidTable`, `sqlite3CreateIndex`, `sqlite3CreateView`, `sqlite3Init` / `sqlite3InitOne` / `sqlite3InitCallback`, `sqlite3AnalysisLoad`, `sqlite3ValueFromExpr`. V2'nin `TableDef` bağdaştırıcısı silindi: şema artık `sqlite_schema` satırlarından C'nin ayrıştırıcısıyla kuruluyor. VIRTUAL üretilmiş sütunlar, açık `COLLATE BINARY`, birincil anahtardan önce `UNIQUE` artık destekleniyor.
- `select.rs` / `expr.rs` / `window.rs`: GROUP BY'ın sıralayıcı dalı, `optimizeAggregateUseOfIndexedExpr`, toplama işlevlerinde FILTER, `generateWithRecursiveQuery` (`SRT_Queue` / `SRT_DistQueue`). V2 açığı kapandı: `sqlite3SrcListDup` CTE kullanımını saymıyordu.
- `resolve.rs`: 3.54.1'de olmayan `sNC.pNext = 0` satırı kaldırıldı (alt seçimin ORDER BY'ı dış sütunu adlandırabiliyor).
- Motor tarafı: D1 (WITHOUT ROWID + tek INTEGER birincil anahtarın indeks sırası, veritabanı dosyası C ile bayt bayt aynı), D10 (`sqlite_stat1` metni, `analysis_limit` dahil), D18/D19 hata metinleri; C'nin `DBFLAG_EncodingFixed` / şema yüklendi durumu (şemayı okuyan 21 pragma, `ATTACH`, ilk ifadede hata konumunun kaybolması); `PRAGMA reverse_unordered_selects` altında `sqlite_stat1` ters okunuyor; UTF-16 veritabanında P4_MEM metni C gibi listeleniyor; geçici veritabanı C'nin açtığı yerde açılıyor.

**Kanıt** (ikili `r9`; sonuçlar `target/perf-work/v3/res/*_r9.*`).

| Derlem | İfade | `EXPLAIN` aynı | EQP aynı | Terim / döngü / yol / agg katmanı |
| :--- | ---: | ---: | ---: | :--- |
| ddl_a | 12.000 | 12.000 | 12.000 | 0 fark |
| plan_a | 26.000 | 26.000 | 26.000 | 0 fark |
| plan_b | 13.000 | 13.000 | 13.000 | 0 fark |
| agg_a / agg_b / agg_c | 21.000 | 21.000 | 21.000 | 0 fark |
| cte_a / cte_b | 15.000 | 15.000 | 15.000 | 0 fark |

- Şema katmanı (C `v3DumpSchema` ile): 400 / 400 şema aynı. Kütüphane düzeyi: 54.609 / 54.613 aynı, 0 farklı, 4 desteklenmiyor. Bağlantı durumu yoklamaları: 66 / 68 aynı (kalan 2: üreticinin taşımadığı türden başarısız ilk ifade).
- **Tüm ifade türleri** (39.300): kabul **%34,8 → %36,0** (14.133); `EXPLAIN` aynı 13.778, farklı 355 (V2: 380); EQP aynı 13.861, farklı 272. Üretici tarafında fark kalmadı; 355'in her sınıfı atfedildi: yalnız C kabuğunda olan işlev/sıralama/modül (D4, D5), eski motor farklarının yansımaları (D8, D12, D13, D20, D22, D24–D27, D29), 1 izlenmeyen şema sıfırlaması, 3 rastgele veri. Desteklenmeyen: ifade türü (DML, DDL, PRAGMA), pencere işlevleri 409, sanal tablo / tablo değerli işlev 241.
- **C tarafı kapsamı** (gcov; V3'ün 71 işlevi): satır %84,6, dal %75,9 (`target/perf-work/v3/cov/`).
- **Yürütme:** testler 979 geçti (C beklentili 4 yeni üretici + 6 yeni `c_align_v3` testi), bilinen 4 sanal tablo testi başarısız, 337 yok sayıldı; DML farksalı 378 / 400 (V2: 377); ayrışma bulucu 241 / 2 / 1 / 1 (h7b ile aynı; `g06_vacuum_into_main_untouched` yalnız stderr'de farklı — C ve Rust koşusunun paylaştığı sabit tmpfs yolu, r6'da da aynı).
- **Hız** (speedbench, `vmerge_bin` tabanına göre): toplam 298,6 → 301,4 ms (1,009x), geomean 1,016x, özet uyuşmazlığı 0. W01 +%5,8 ve W03 +%3 9 denemelik tekrarda da kaldı; nedeni bulunamadı (fark satır başına ekleme / nokta okuma yoluna dokunmuyor). Ölçüm, makine hattının ajanı aynı anda derleme yaparken alındı.

**Değişen test beklentileri.** `plan_tests.rs`: `unsupported_over_tables` yerine `unsupported_in_v3`; tablo üzerinde GROUP BY artık destekleniyor (C'nin listesi `group_by_in_index_order` ile sınanıyor). Başka beklenti değişmedi.

**Bilinen eksikler ve sonraki adım.** Pencere işlevleri ve FROM içinde sanal tablolar / tablo değerli işlevler taşınmadı; öneri: V4a (`window.c`) ve V4b (`vtab.c`, `whereLoopAddVirtual`). Bu adımda görülen eski motor farkları `gorulen_farklar_v3.md`'de (D1–D29). **D23 veri farkı:** `SELECT 0.5 IS TRUE` motorda 0, C'de 1 — motor kuyruğunda öne alınmalı.

## V4a — pencere işlevleri (Rust 1.100 ile)

**Önce: araç zinciri.** Ayrı commit (`9f67480`): `rust-toolchain.toml` `channel = "1.100"`, `rust-version = "1.100"`. Aynı kaynakta 1.99.1 → 1.100: speedbench toplam 0,994x, geomean 1,003x; testler, DML farksalı ve ayrışma bulucu aynı. Bu adımdan itibaren hız tabanı 1.100 ile derleniyor (`target/perf-work/v4_bin/base_1100/`).

**Taşınan** (`crates/sqliters-codegen/src/`; ayrıntı `karar_esligi_v4a.md`).
- `window.rs`: `window.c`'nin kod üretici tarafının tamamı — `sqlite3WindowRewrite` ve yürüteçleri, adlandırılmış / zincirlenmiş pencereler, `Alloc` / `Assemble` / `Attach` / `Link` / `Unlink` / `Compare` / `Dup`, `sqlite3WindowCodeInit`, `sqlite3WindowCodeStep` ve tüm yardımcıları (`windowCodeOp`, `windowReturnOneRow`, `windowAggStep` / `Final`, `windowFullScan`, `windowIfNewPeer`, `windowCodeRangeTest`, `windowCheckValue`, `windowCacheFrame`, `windowInitAccum`, `windowExprGtZero`).
- Yerleşik işlev kimliği: C'nin `zName==xxxName` / `xSFunc==noopStepFunc` karşılaştırması yerine `SQLITE_FUNC_BUILTIN|SQLITE_FUNC_WINDOW` bayraklı FuncDef + ad (uygulama işlevi bu bayrağı taşıyamaz). Host arayüzü değişmedi.
- `parse.rs` kuralları 93, 318–345; `resolve.rs` pencere dalları ve `windowRemoveExprFromSelect`; `expr.rs` (`SelectDup` WINDOW listesini kopyalıyor, pencere işlevinde ORDER BY hatası); `select.rs` (özyinelemeli CTE pencere hatası, `multiSelectValues`); `flatten.rs` aşağı itme kısıtları 6a–6c.

**Kanıt** (ikili `r2`; sonuçlar `target/perf-work/v4a/res/*_r2.*`).
- Pencere derlemleri (`gen_win.py`; win_a 12.000, win_b 6.000, win_c 6.000, elle yazılmış win_d 12): `EXPLAIN` ve EQP 24.012 / 24.012 kabul edilen ve aynı; terim / döngü / yol / agg / şema katmanları aynı; yeni **Window nesnesi döküm katmanı** (C `patch_v4a.py` ile) 16.833 / 16.833 aynı.
- V3 derlemleri (87.000 ifade): hâlâ %100 aynı, tüm katmanlar.
- Tüm ifade türleri: kabul **%36,0 → %37,0** (14.532); `EXPLAIN` aynı 14.177, farklı 355 (V3'ün aynı, önceden atfedilmiş 355'i); EQP aynı 14.260, farklı 272 (V3 ile aynı küme). "Pencere işlevi" diye desteklenmeyen 409 ifadenin 399'u artık kabul edilip aynı; 10'u tetikleyici gövdesi / DML içinde (ifade türü olarak bekliyor).
- `SQLITERS_VDBE_EXEC=1` ile sonuç satırları: makinede 4.617 ifadenin 4.615'i C ile aynı; 2 fark D30 (motorun WITHOUT ROWID `UNIQUE PRIMARY KEY DESC` yazım sırası; C'nin `integrity_check`'i dosyayı reddediyor — V4a öncesinde de var). Hata yolunda 51 + 2 fark motor mesajlarından (D31, D32, D21).
- C tarafı kapsamı (gcov): `window.c` satır %99,4 (970 / 976), dal %95,8 (548 / 572); hiç alınmayan 42 dal satırı tek tek sınıflandırıldı (bellek yetmezliği, ALWAYS / NEVER, taşınmamış RENAME, test denetimi).
- Testler: anahtar kapalı 1002 geçti / bilinen 4 / 337 yok sayıldı; açık 996 / 10 (M2'nin 9'u + F6). DML farksalı 378 / 400; ayrışma bulucu h7b ile aynı. Hız (1.100 tabanına göre, boş makinede 7 deneme): toplam 1,001x, geomean 0,998x.

**Değişen test beklentileri (2).** `explain_tests.rs` ve `plan_tests.rs` içindeki "desteklenmiyor" listelerinden birer pencere ifadesi çıkarıldı (C bunları derliyor); C'nin çıktısı 3 yeni testle (`window_function_without_from`, `window_aggregate_listing`, `window_objects_dump`) sınanıyor.

**Kalanlar.** Eski motor farkları `gorulen_farklar_v4a.md`'de (D30–D32). **D30 veri farkıdır** ve D23 ile birlikte motor kuyruğunda öne alınmalı.

## V4b — FROM içinde sanal tablolar ve tablo değerli işlevler

**Taşınan** (ayrıntı `karar_esligi_v4b.md`).
- Yeni `vtab.rs`: CREATE VIRTUAL TABLE'ın şema yüklemesinde C gibi ayrıştırılması (`sqlite3VtabBeginParse` … `FinishParse`), `vtabCallConstructor` (HIDDEN sütun kuralları), `sqlite3VtabCallConnect`, `sqlite3_declare_vtab` (iç içe ayrıştırma), `sqlite3VtabEponymousTableInit`, `sqlite3GetVTable`, `sqlite3VtabOverloadFunction`, `sqlite3VtabMakeWritable`, `sqlite3_index_info`.
- Ayrıştırıcı kuralları 302–308; tablo değerli işlev argümanları (`sqlite3WhereTabFuncArgs`), `isAuxiliaryVtabOperator`, `sqlite3WhereAddLimit`; `allocateIndexInfo`, `vtabBestIndex`, `whereLoopAddVirtualOne` / `Virtual`, `sqlite3_vtab_in` / `rhs_value` / `distinct` / `collation`; VFilter / VNext / VInitIn döngüsü ve "VIRTUAL TABLE INDEX" EQP metni.
- Host arayüzüne 4 yöntem: `vtab_module`, `vtab_connect`, `vtab_best_index`, `vtab_find_function`. Motor tarafında generate_series, json_each / json_tree, dbstat, `pragma_*`, fts5vocab, fts5, rtree için C'den taşınmış `xBestIndex` yöntemleri (yürütücünün kendi `best_index`'i değişmedi). D33 düzeltildi: `pragma_*` sanal tabloları C'deki gibi "innocuous" değil.
- Yeni katmanların bulduğu taşıma hataları düzeltildi (`whereClauseInsert` iParent, çözümsüz sorguda panik, `u.vtab` / `u.btree` birleşim örtüşmesi).

**Kanıt** (ikili `r3`; sonuçlar `target/perf-work/v4b/res/*_r3.*`).
- Sanal tablo derlemleri (`gen_vtab.py`): vt_a 12.000, vt_b 6.000, vt_t 2.400, vt_x 3.000, vt_e 55 — `EXPLAIN` ve EQP tamamı aynı; vt_d 185 / 187 (2 = D34, motorda olmayan `pragma_*` tabloları). Terim / döngü / yol / agg / şema katmanları ve yeni **`xBestIndex` çağrı dizisi katmanı** (her çağrının kısıtları, sıralama, `colUsed` ve çıktıları) 0 fark. Hiçbir yerleşik modülün ulaşmadığı yollar (`OP_VInitIn`, `rhs_value`, `distinct`, `collation`, "xBestIndex malfunction") geçici C modülleriyle (`vtx` / `vtbad`) karşılaştırıldı.
- Önceki derlemler (V3'ün 87.000'i, V4a'nın 24.012'si): %100 aynı.
- Tüm ifade türleri: kabul **%37,0 → %37,6** (14.773); `EXPLAIN` aynı 14.406, farklı 367 (V4a'nın 355'i + 6 yalnız kabukta olan `fsdir` + 6 D27 yansıması); EQP aynı 14.495, farklı 278; sanal tablo nedeniyle desteklenmeyen 0 (V4a: 241).
- C tarafı kapsamı: satır %87,8, dal %76,0; iki yönü de alınmayan 113 dal satırı tek tek sınıflandırıldı.
- Testler: anahtar kapalı 1011 / bilinen 4 / 337; açık 1005 / 10 (aynı 10). DML farksalı 378 / 400; ayrışma bulucu h7b ile aynı. Hız (1.100 tabanı, 7 deneme): toplam 1,005x, geomean 1,006x.
- Makinede sanal tablo işlem kodları yok: anahtar açıkken bu programlar eski motora düşüyor (sonraki makine adımı).

**Değişen test beklentisi (1).** `plan_tests.rs::unsupported_in_v3`: `select * from pragma_table_info('t')` artık destekleniyor (C'nin listesi bayt bayt aynı); listede hâlâ desteklenmeyen bir DML ifadesiyle değiştirildi. 9 yeni test (`vtab_tests.rs`) C'nin listelerini ve `xBestIndex` satırlarını sınıyor.

**Kalanlar** (`gorulen_farklar_v4b.md`). **D37 veri farkı:** yürütücü, tablo değerli işlevin gizli sütunları üzerindeki kısıtları yeniden sınamıyor — `SELECT value FROM generate_series(0,3,1) WHERE stop=1` C'de satır döndürmüyor, motorda `0`, `1` döndürüyor (üreticinin programı C'ninki). D34 eksik `pragma_*` tabloları, D35 motorun `best_index` / filtre sözleşmesi (makine `OP_VFilter`'ı çalıştırmadan önce hizalanmalı), D36 json modüllerinin kayıt zamanı, D38–D41 hata metinleri ve satır sırası. Öne alınacak veri farkları: D23, D30, D37.

## V5 — DML, UPSERT, RETURNING, tetikleyiciler ve yabancı anahtarlar

**Taşınan** (ayrıntı `karar_esligi_v5.md`; adım bölünmeden tek seferde bitti).
- Yeni dosyalar: `delete.rs` (`sqlite3DeleteFrom` — kesme, tek geçiş, görünümler; `GenerateRowDelete`, `GenerateRowIndexDelete`, `GenerateIndexKey`, `OpenTableAndIndices`, `IsReadOnly`, `MaterializeView`), `insert.rs` (`sqlite3Insert`'in bütün biçimleri, `xferOptimization`, AUTOINCREMENT, `GenerateConstraintChecks`, `CompleteInsertion`, üretilmiş sütunlar), `update.rs` (`sqlite3Update`, `updateFromSelect` ile UPDATE…FROM, `updateVirtualTable`), `upsert.rs` (upsert.c'nin tamamı), `trigger.rs` (CREATE TRIGGER'ın şema yüklemesinde C'nin trigHash sırasıyla ayrıştırılması, alt ayrıştırmada tetikleyici alt programları, `TriggersExist`, `TriggerColmask`, RETURNING), `fkey.rs` (fkey.c'nin DML tarafı; `sqlite3FkDropTable` DROP TABLE taşınmadığı için yok).
- Ayrıştırıcı kuralları 76, 131, 152, 157–174, 260–277, 309/310; `resolve.rs`'de `new.` / `old.` / `excluded.` ve RETURNING adları; `exprcode.rs`'de TK_TRIGGER, TK_RAISE ve yapılmakta olan satıra sütun başvurusu; `select.rs`'de SRT_Upfrom.
- Program düzeyi karşılaştırmanın bulduğu taşıma hatası düzeltildi: `exprCompareVariable` `expmask`'ı ayarlamıyordu.
- Motor tarafı: `cgen.rs` foreign_keys / recursive_triggers / count_changes / writable_schema bayraklarını ve yetkilendirici durumunu üreticiye geçiriyor; gizli API `Connection::c_generated_program`; `progdump::dump` M2 döküm biçimini yazıyor. Anahtar kapalıyken yürütme değişmedi.

**Kanıt** (ikili `r7`; sonuçlar `target/perf-work/v5/res/`).
- DML derlemleri (`gen_dml.py`; C'nin reddettiği kurulum DDL'i ayıklanarak): dml_a 12.000, dml_v (sanal tablolar) 6.000, dml_t (tetikleyiciler) 8.000, dml_f (yabancı anahtarlar) 8.000, dml_tf 6.000 — `EXPLAIN` ve EQP tamamı aynı. Zengin derlemler r_a / r_f / r_t / r_v (25.200): `EXPLAIN` tamamı aynı; EQP'de 3 fark, üçü de D48 (motor süreci aynı parçada ölüyor, sonraki ifadeler "ulaşılmadı" sayılıyor). Şema ve wdiff katmanları her derlemde 0 fark.
- **Program düzeyi:** C'nin döktüğü programla üretilen program, ifade ifade: 62.308 / 62.308 aynı (ops_dml_a/t/f/tf/v ve M2'nin dml_a/dml_b'si).
- **Makinede çalıştırma** (API sürücüsü; stdout ve veritabanı dosyası C ile karşılaştırıldı; betik sayısı): ops_dml_a 131/300 (eski yol 112), ops_dml_t 104/200 (89), ops_dml_f 100/200 (90), ops_dml_tf 66/150 (57), ops_dml_v 57/150 (51), M2 dml_a 200/200 (188), M2 dml_b 398/400 (374). Makine yolu her derlemde eski yoldan iyi; kalan farklar motorun kendi prepare'i (D43/D44) ve makine hataları (E16–E19).
- Önceki derlemler (V3, V4a, V4b): %100 aynı; vt_d 185 / 187 (2 = D34, önceki gibi).
- Tüm ifade türleri: kabul **%37,6 → %68,2** (26.811 / 39.300); `EXPLAIN` aynı 26.325, farklı 486 (yeni 119 farkın hepsi daha önceki motor/kabuk ayrışmalarının zinciri: fts3/fts4/zipfile modülleri, yalnız kabukta olan işlevler, DDL biçimleri, çerez ve kök sayfa farkları; hiçbiri üretici farkı değil); EQP aynı 26.464, farklı 347.
- C tarafı kapsamı (9 DML derlemi, gcov): satır %93,9, dal %87,1. Bir yönü hiç görülmeyen dallar: 47 bellek yetmezliği, 41 CREATE TRIGGER hata yolu, 46 başka ifade türü / yetkilendirici / defer_foreign_keys; 132 dal satırı "doğrulanmadı" olarak tek tek listelendi.
- Kendi denetimim: tetikleyici, INSTEAD OF görünüm, UPSERT + RETURNING, UPDATE…FROM, CASCADE, WITHOUT ROWID REPLACE, özyinelemeli CTE ile INSERT içeren 12 elle yazılmış ifadenin `EXPLAIN` çıktısı C ile bayt bayt aynı.
- Testler: anahtar kapalı 1018 geçti / 0 başarısız / 337 yok sayıldı (3 yeni); açık 1012 / 6 (bilinen 6: M1'in 3 bulgusu, kesme davranışını sınayan 2 test, F6). C ile karşılaştıran 26 test `sqlite3`'ü PATH'te arıyor; bu makinede sistemde yok, `target/perf-work/bin` PATH'e eklenerek koşuldu. DML farksalı 378 / 400; ayrışma bulucu h7b ile aynı. Hız (1.100 tabanı): toplam 1,013x, geomean 0,999x; 11 denemelik tekrar ölçümde W03 1,017x, W11 1,020x (sıcak yol değişmedi; kod yerleşimi gürültüsü).

**Yapılmayanlar.** CLI hâlâ yalnız SELECT'i makineye gönderiyor; DML'nin makineye yönlendirilmesi güvenli değil: E16 (ifade indekslerinde `OP_IdxDelete` "database disk image is malformed" veriyor, C başarılı), E17 (`PK(a DESC)` + `UNIQUE(a)` WITHOUT ROWID tablosunda UNIQUE çakışması yakalanmıyor; program C'ninkiyle aynı), E18 (makinede sanal tablo yazmaları yok), D43/D44 (motorun prepare'i C'nin derlediğini reddediyor ya da reddettiğini kabul ediyor). Bu nedenle çökme takımları koşulmadı. DELETE/UPDATE … LIMIT başvuru derlemesinde olmadığı için üretilmedi.

**Değişen test beklentileri (2).** İkisi de taşıma durumu beklentisiydi, C davranışı değil: `explain_tests.rs::unsupported_statements_are_reported_not_approximated` (`insert into t values(1)` yerine `drop table t`; insert için C'nin hatası `no such table: t` ayrıca sınanıyor) ve `plan_tests.rs::unsupported_in_v3` (örnek artık CREATE TRIGGER; `insert into t(b) values(1)` derleniyor). 3 yeni test (`dml_tests.rs`): UPSERT + RETURNING, tetikleyici alt programı, FK CASCADE; beklenen listeler C'den.

**Kalanlar** (`gorulen_farklar_v5.md`). D42 çelişen ON CONFLICT yan tümceleri kabul ediliyor; D43/D44 prepare farkları; D45 STRICT ileti sütun adı; D46 rtree RETURNING tamsayı (C REAL); D47 birkaç sayfa baytı (eski yolda da); **D48** birbirine bağlı üretilmiş sütunlarda INSERT — zengin derlemde motor yığın taşmasıyla çöküyor; basit biçimde (`c AS (e*b) STORED, e AS (b*c) STORED`) çökmüyor ama C'nin `generated column loop on "e"` hatasını vermeden satırı yazıyor. E16–E19 makine adımına kaydedildi. Öne alınacak veri farkları: D23, D30, D37, D48.

## V6 — DDL: CREATE / DROP / ALTER / ANALYZE / VACUUM / ATTACH / REINDEX

**Taşınan** (ayrıntı `karar_esligi_v6.md`).
- Yeni `ddl.rs`: build.c'nin DDL kod üretimi (`sqlite3NestedParse`, `sqlite3ChangeCookie`, StartTable / EndTable kodu, CREATE TABLE … AS SELECT, CreateIndex kodu, `sqlite3RefillIndex`, DROP TABLE / INDEX / VIEW, `destroyRootPage` / `destroyTable` ile auto_vacuum kök taşımaları, `sqlite3ClearStatTables`, REINDEX), DROP TRIGGER, VACUUM, ATTACH / DETACH.
- Yeni `alter.rs`: RENAME TABLE, RENAME COLUMN, ADD COLUMN, DROP COLUMN ve 3.54'ün kısıt biçimleri (DROP CONSTRAINT, SET / DROP NOT NULL, ADD CHECK); iç içe `sqlite_rename_*` SQL'i C'nin yazdığı gibi. Yeni `analyze.rs`: ANALYZE (STAT4'süz derleme).
- `trigger.rs`: CREATE TRIGGER kodlaması ve C'nin hash sırası (silme dahil); `vtab.rs`: `OP_VCreate`; `fkey.rs`: `sqlite3FkDropTable`; `build.rs`: `sqlite3OpenTempDatabase`; ayrıştırıcının DDL kuralları.
- Kanıt yolunda bulunan eski üretici kusurları düzeltildi: `DEFAULT -0.0` işareti (V3'ten), `bQuote` değeri, DbFixer hata konumları.
- **Motora dokunuldu (kurala istisna):** C'nin bağlantı durumu altı yerde motora yansıtıldı (D75–D80: temp veritabanının açılması, tblHash geçmişi, nRowLogEst yükseltmeleri, ALTER sonrası istatistik kaydı, `pragma_quick_check` bağlantısı, `analysis_limit`). Bunlar olmadan bir bloğun sonraki `EXPLAIN`'leri zincirleme farklılaşıyordu; motorun DDL yürütmesi değişmedi.
- **PRAGMA taşınmadı** (V6b): üretici `Unsupported("PRAGMA (step V6b)")` veriyor. **DDL makineye yönlendirilmedi** (M4).

**Kanıt** (ikili `r2`; sonuçlar `target/perf-work/v6/res/`).
- 9 DDL derlemi (a, auto_vacuum, ATTACH + temp, WITHOUT ROWID, FK + tetikleyici, fts5 / rtree, ALTER ağırlıklı, elle yazılmış iki hata derlemi): kabul edilen 28.367 ifadenin `EXPLAIN` ve EQP'si C ile aynı, fark 0. Motorun DDL farkları (D59–D74, D81) yüzünden 1.030 ifade yalnız `EXPLAIN` olarak sınandı (çalıştırılmadı).
- **Program düzeyi:** 25.107 program C'nin döktüğü programla bayt bayt aynı (13 ifadeyi üretici istatistik kuralı gereği desteklemiyor).
- Katmanlar: wdiff'te yalnız fts5 / rtree derleminde loops ve paths katmanında 2'şer fark (D74, rtree `xBestIndex` satır tahmini; listeleme aynı); şema katmanında 3 blok (C'nin eski istatistikleri ve bir araç farkı).
- Önceki derlemler (V3, V4a, V4b, V5, zengin V5): V5 ile aynı (bilinen vt_d 2 = D34, dml_r_* EQP 1'er = D48).
- Tüm ifade türleri: kabul **%68,2 → %92,4** (36.329 / 39.300; kalan 2.929'un hepsi PRAGMA); `EXPLAIN` farkı 486 → 238 (yeni 68'in hepsi motora izlendi: 66 `EXPLAIN PRAGMA` yan etkisi, 1 hata konumu, 1 D58 sınıfı); EQP farkı 170.
- C tarafı kapsamı (gcov): satır %94,6, dal %86,4; bir yönü hiç alınmayan 32 dal satırı "doğrulanmadı" olarak listelendi.
- Kendi denetimim: 24 DDL ifadesinin (STRICT / CHECK / `-0.0` varsayılanlı CREATE TABLE, CREATE TABLE AS, kısmi UNIQUE indeks, TEMP tetikleyici, rtree, FK'lı DROP TABLE, dört ALTER biçimi, ANALYZE, REINDEX, VACUUM, ATTACH / DETACH, olmayan veritabanı) `EXPLAIN` çıktısı C ile bayt bayt aynı. auto_vacuum, ANALYZE, temp, ATTACH, ALTER, DROP ve VACUUM içeren bir yürütme betiğinde V6 öncesi ikili ve V6 ikilisi stdout ve dosya baytında C ile aynı (motor değişiklikleri yürütmeyi bozmadı).
- Testler: anahtar kapalı 1032 geçti / 0 başarısız / 337 yok sayıldı (3 yeni); açık 1017 / 15 (D58 sonrasındaki 15'in aynısı). DML farksalı kapalı 378 / 400, açık 398 / 400; ayrışma bulucu h7b + g06; hız (anahtar kapalı, 1.100 tabanı) toplam 1,004x, geomean 0,996x (r1'deki W02 %3,9 yavaşlaması r2'de giderildi).

**M4 için işlem kodları** (üretilen program sayısı): `SetCookie` 18.292, `ParseSchema` 15.949, `CreateBtree` 10.521, `Expire` 5.166, `DropTable` 2.091, `Destroy` 1.809, `LoadAnalysis` 1.277, `SqlExec` 1.107, `Vacuum` 531, `DropTrigger` 273, `VCreate` 271, `DropIndex` 154, `JournalMode` 146, `VDestroy` 55, `VRename` 2. Makinenin işlev kitaplığına ayrıca `sqlite_attach` / `detach`, `stat_init` / `push` / `get`, `sqlite_rename_table` / `column` / `test` / `quotefix`, `sqlite_drop_column`, `sqlite_add` / `drop` / `find_constraint`, `sqlite_fail` gerekiyor.

**Değişen test beklentileri (2; ikisi de taşıma durumu beklentisiydi).** `explain_tests.rs::unsupported_statements_are_reported_not_approximated`: "create table t(x)" artık program veriyor, "drop table t" C'nin hatasını veriyor (`no such table: t`, konumsuz; `$S/bin/sqlite3` ile doğrulandı), PRAGMA hâlâ desteklenmiyor. `plan_tests.rs::unsupported_in_v3`: CREATE TRIGGER artık program veriyor. 3 yeni test (`v6_drop_table_with_autoindex`, `v6_create_unique_partial_index`, `v6_analyze_one_table`; beklenen listeler C'den).

**Kalanlar** (`gorulen_farklar_v6.md`; hiçbiri üretilen programda değil). Motorun DDL farkları: D59 görünüm / sanal tablo ANALYZE'ı, D60 sistem tablolarının analizi, D61 3.5x ALTER biçimleri motorda yok, D62 VACUUM INTO hata metni, D63 temp'in VACUUM'u, D64 ALTER ve hatadan sonra şema nesli, D65 `ATTACH x'00'`, D66–D68 hata metinleri, D69 ANALYZE satır sırası, D70 "unknown database" hata konumu, D71 `EXPLAIN PRAGMA` yan etkileri (V6b), D72 defer_foreign_keys, D73 NULLS FIRST kabulü, D74 rtree tahmini, D81 nitelenmemiş CREATE INDEX'in başka veritabanındaki tabloyu bulması. E30 / E31 M4 girdisi.

## V6b.1 — PRAGMA üreticisinin ilk dilimi

**Durum: V6b tamamlanmadı.** `e9ba0e2` üzerinden başlandı. `ileriPlanlama/` dosyaları değerlendirilmedi. Çalışma sırası eksik davranışları kapatmaya göre seçiliyor; doğruluğu kanıtlanmamış kodda performans ayarı yapılmıyor.

Beş gramer kuralı üreticiye bağlandı; C başvurusunun `pragma.h` tablosundaki 66 ad ve sütunları üretim aracıyla taşındı. Bayraklar, analysis_limit, başlık değerleri, default_cache_size programı, page_size sorgusu, page_count/max_page_count, journal_mode, incremental_vacuum, tablo/indeks/FK bilgileri, foreign_key_check, pragma_list, wal_checkpoint ve optimize'nin kullanım geçmişi gerektirmeyen yolları destekleniyor. Bayrak/analysis_limit hazırlama etkileri EXPLAIN dahil C zamanında uygulanıyor; sonuçlar hazırlamadaki değerini koruyor, OP_Expire çalıştırmada etkiliyor. Genel PRAGMA yürütmesi eski yürütücüde kalıyor.

**Kanıt:** 3.992 girdiden 3.695 kabul edilen EXPLAIN/EQP çıktısı C ile aynı, 0 farklı, 297 açık desteklenmiyor. Ayrı prepare-only sürücüde tam programlar 3.667/3.667 aynı, 279 desteklenmiyor. Ardışık ayarlarda 153/153 aynı, bir görünüm geçmişi desteklenmiyor. Beş yeni regresyon testi; bunlardan biri gerçek C 3.54.1 ile 270 ifade karşılaştırıyor. C kapsamı 466/990 satır (%47,1), 337/706 dal (%47,7); taşınmayan tüm sqlite3Pragma gövdesi paydada. Hiç alınmayan 369 dal ayrı ayrı sınıflandırıldı. [Karar eşliği](karar_esligi_v6b_1.md), [dal kayıtları](karar_esligi_v6b_1_dallar.md).

**Dört kapı:** anahtar kapalı 1037 geçti/0 başarısız; açık 1022 geçti / 15 önceki başarısız; iki durumda 337 yok sayıldı. DML 378/400 ve 398/400; başarısız seed/sınıf kümeleri V6 ile aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; önceden bilinen g06 stderr farkı sürüyor. Rust1.100 tabanına karşı 7 denemede toplam 0,999x, geomean 0,996x, 14 özet aynı. **W06 önceki V6'ya karşı 15 tekrarda 1,020x yavaş; açık performans borcu.**

**Kalanlar:** 24 pragma henüz taşınmadı; page_size/case_sensitive_like ayarları, defer_foreign_keys yaşam döngüsü (D72), optimize kullanım geçmişi ve özel VFS/yetkilendirici/hata enjeksiyonu açık. D71'in bayrak/analysis_limit kısmı kapandı; pager/encoding hazırlama etkileri bekliyor. Yeni **D82**: başka şemada aynı adla gizlenen görünümün table_list sütun sayısı önceki kullanıma bağlı; eski yürütücü ilk sorguda C'den farklı, üretici bu bağlamı açıkça desteklemiyor. Ayrıntı ve sonraki doğruluk sırası [fark kaydında](gorulen_farklar_v6b_1.md).

## V6b.2 — PRAGMA: cache_size, journal_size_limit, secure_delete

Başlangıç `84daa83`. Üç aile hazırlama etkileriyle üreticiye taşındı. Bayrak/analysis_limit için kurulan sabit sonuç köprüsü genişletildi: ayarlar EXPLAIN dahil hazırlamada uygulanır, daha sonra adımlamak ayarı yeniden yapmaz; sonuçlar hazırlanmış programın değerini korur. Niteliksiz secure_delete bütün açık Btree'lere uygulanır. cache_size şemayı hazırlamada okur. **D83–D85 kapandı:** cache_size taşmasında C'nin 0 sonucu ve sıfır cache_size değerinin şema yeniden yüklemesinde çerezden/varsayılandan başlatılması hizalandı; PRAGMA büyük hex tokenı korunuyor, C derleme hatası normal hazırlamaya da aktarılıyor. Doğrudan TEMP journal_size_limit yanıtı gerçek pager değerini kullanıyor; D57'nin tablo değerli işlev farkları ayrıca açık.

**Kanıt:** genişletilmiş derlemde 4.178 EXPLAIN/EQP, 4.176 tam program, ardışık derlemde 179 kabul edilen ifade C ile aynı; kabul edilenlerde 0 fark. Desteklenmeyen 264 EXPLAIN ve 246 tam program eşliğe katılmadı. Üç yeni regresyon testi, C ile 330 ifadeli program karşılaştırması ve C'nin yazdığı dosyada default_cache_size çerezi sınaması. Elle yazılmış hazırlama/adımlama dizileri iki anahtar durumunda çıktı ve dosya baytlarında C ile aynı. C kapsamı pragma.c için 495/990 satır, 356/706 dal; üç Btree/pager yardımcısında 20/20 satır, 6/6 dal. Önbellek başlatmasının dört yönü ayrıca gcov'da alındı; 350 alınmayan dal ayrı sınıflandırıldı. [Karar eşliği](karar_esligi_v6b_2.md), [dal kayıtları](karar_esligi_v6b_2_dallar.md).

**Dört kapı:** kapalı 1040 geçti/0 başarısız; açık 1025 geçti/15 önceki başarısız; iki durumda 337 yok sayıldı. DML 378/400 ve 398/400, başarısız seed/sınıf kümeleri aynı. Ayrışma bulucu 245 senaryoda 241 L0, 2 L1, 1 L4, 1 L6; bilinen g06 stderr farkı sürüyor. Rust1.100 tabanına karşı toplam 0,997x, geomean 0,991x, 14 özet aynı. W06 tabana göre 1,020x; V6b.1'e göre 15 tekrarda 0,994x. Hız ayarı uygulanmadı.

**V6b açık:** 21 pragma, page_size/case_sensitive_like ayarları, ertelenmiş FK yaşam döngüsü, optimize kullanım ve D82 görünüm geçmişi bekliyor. Sonraki doğruluk adımı default_cache_size dahil kalan hazırlama etkilerini, ardından synchronous, page_size ve auto_vacuum durumunu tamamlamak. [Farklar ve açık işler](gorulen_farklar_v6b_2.md). ileriPlanlama/ yayın sonrasına bırakıldı.

## V6b.3 — default_cache_size hazırlama etkisi

Başlangıç f1a2e58. D71'in bu ailesi kapandı: önbellek prepare/EXPLAIN sırasında ayarlanır, step yalnız hazırlanan çerezi yazar. Başka önbellek ayarının değeri step'te geri alınmaz. RunOnlyOnce reset/yeniden çalıştırmada hazırlama etkisini tekrar uygular. D86: sıfır çerez -2000 döndürür; TEMP gerçek çerezini yazar; i32 dönüşümü C Atoi/AbsInt32 değeridir.

C kanıtı: kabul edilen 4274 EXPLAIN/EQP, 4272 tam program ve 275 ardışık ifade aynı; 0 fark. API lifetime dizisi iki kipte çıktı ve dosya baytlarında C ile aynı. Üç yeni test; kapalı 1043/0, açık 1028/15 bilinen, 337 yok sayıldı. DML 378/400 ve 398/400 hata kümeleri aynı; ayrışma bulucu 241/2/1/1 ve bilinen g06 stderr farkı. Rust1.100 hız kapısı toplam 1,003x, geomean 1,000x; 14 özet aynı. Hız ayarı uygulanmadı.

[Karar eşliği](karar_esligi_v6b_3.md), [alınmayan dallar](karar_esligi_v6b_3_dallar.md), [farklar](gorulen_farklar_v6b_3.md). V6b açık; sıradaki adım synchronous ve pager eşzamanlama bayrakları. ileriPlanlama/ değerlendirilmedi.

## M1 — makine: `SELECT` programlarının C gibi çalıştırılması (dal `performans-A-vdbe-makine`)

**Taşınan** (yaklaşık 7.150 satır; ayrıntı `karar_esligi_m1.md`).
- `crates/sqliters-vdbe/src/vdbe.rs`: `sqlite3VdbeExec` — her `case OP_x` için bir kol; `vdbemem.rs` (`vdbemem.c`: dönüşümler, yakınlık, karşılaştırma); `vdbeaux_rt.rs` (kayıt çözme ve karşılaştırma, imleç geri yükleme, okuma ifadeleri için Halt); `vdbesort.rs` (`vdbesort.c` sıralama düzeni ve `rowset.c`).
- `crates/sqliters-btree/src/btcursor.rs`: `btree.c` imlecinin okuma yarısı (`sqlite3BtreeCursor`, `First/Last/Next/Previous`, `TableMoveto`, `IndexMoveto`, `accessPayload`) — tek iniş hem veriyi buluyor hem C'nin sayfa başvurularını tutuyor.
- İşlem kodu durumu (191): **119 doğrulandı**, 28 taşındı ama derlemlerde hiç çalışmadı, 44 henüz yok (yazma, işlem, tetikleyici, DDL ve sanal tablo kodları).
- Geçici tanılama anahtarı `SQLITERS_VDBE_EXEC=1`: üretici ifadeyi destekliyor ve programın tüm kodları taşınmışsa okuma ifadesi makinede çalışır. Anahtar kapalıyken (varsayılan) hiçbir şey değişmez.

**Kanıt (anahtar açık).**

| Ölçüm | Sonuç |
| :--- | :--- |
| Sonuç farksalı (11 derlem) | makinede çalışan 108.473 ifade; makineye izlenen C farkı yok. Kalan farklar: eski yazma yolu, üretici (2 durum), motorun işlev kitaplığı, saat / `random()` |
| Bağımsız kontrol (C'nin kurduğu 40 veri dolu şema, 2.400 sorgu: tek tablo, LEFT/RIGHT/FULL birleşimler, alt sorgular, bileşik seçimler, karışık tipli ve taşma sayfalı veri) | satırlar ve satır sırası C ile aynı 2.400 / 2.400; stderr 40 / 40 |
| İşlem kodu izi (pc, kod, yazılan yazmaç özeti) | 32.954 ifadenin 30.978'inde akış eşit; eşit olmayanlar saat / `random()` ifadeleri, hata anındaki yazmaç gösterimi, motor işlev sonuçları, 6 plan farkı (üretici) |
| Önbellek karar izi — yazma işlemi içindeki `SELECT`ler | H6b'nin `psx` ailesi (bütünlük kontrolü çıkarılınca) **5/5** (eski yol 0/5); beş yeni aile (birleşim, alt sorgu, sıralayıcı, bileşik, taşma) **25/25** (eski yol 0/25) |
| C tarafı kapsamı (gcov) | satır %78,3, dal %63,4; alınmayan dal taşıyan 426 satır "doğrulanmadı" |
| Anahtar kapalı | testler 940 geçti (11 yeni), bilinen 4 başarısız, 337 yok sayıldı; ayrışma bulucu 241 / 245; DML farksalı 377 / 400; speedbench toplamı 0,997x, özetler eşit |
| Anahtar açık, test paketi | 937 geçti, 7 başarısız: bilinen 4 + 3 bulgu (iki test C'de olmayan davranışı sınıyor, biri hızlı yol sayacını) |

**Makinenin hızı (anahtar açık; geçiş öncesi yapılacak iş).** Aynı programı çalıştırdığı halde makine şimdilik C'den yavaş: `W03` C'nin 2,5 katı, `W04` 2,8 katı, `W09` 5,0 katı, `W14` 2,0 katı; toplam eski yolun 1,50 katı. Henüz profil alınmadı; olası nedenler: her yürütmede yeni `Vdbe`, işlevlerin adla çağrılması ve argüman kopyaları.

**C'den yapı gereği farklı / yapılmayan.** Okuma adımı makineyi sonuna kadar çalıştırıp satırları tutuyor (satır satır adımlama henüz yok); sıralayıcının disk birleştirmesi ve geçici dosyaları yok; geçici B-ağaçları diske taşmıyor; UTF-8 dışı kodlamalar; `OP_AutoCommit` / `OP_Savepoint` ve tüm yazma kodları. Görülen eski motor farkları: `gorulen_farklar_m1.md`.

## M2 — makine: satır satır adımlama, yazma programları, tetikleyici çerçeveleri, yabancı anahtar sayaçları (dal `performans-A-vdbe-makine`)

**Taşınan** (ayrıntı `karar_esligi_m2.md`).
- Adımlama ve ifade ömrü: makine bir satır döndürünce durur, imleçleri B-ağacının imleç listesine girer (`saveAllCursors`, `sqlite3BtreeTripAllCursors`); `sqlite3_reset`, yeniden bağlama, `sqlite3_interrupt`, ilerleme işleyicisi C'nin aralığında, `sqlite3_stmt_status`, şema değişince yeniden hazırlama (`SQLITE_SCHEMA`).
- Yazma kodları gerçek tablolarda: `OP_Transaction` (yazma), `OP_OpenWrite`, `OP_NewRowid`, `OP_Insert`, `OP_Delete`, `OP_IdxInsert`, `OP_IdxDelete`, `OP_Clear`, `OP_ResetCount`, `OP_TypeCheck`, `OP_CursorLock` / `OP_CursorUnlock` (`BTCF_Pinned`); `sqlite3BtreeInsert` / `Delete` / `ClearTable`'ın imleç kısmı (`btcursor.rs`); `OP_Halt` hata eylemi motorun ifade / işlem geri alımına bağlı.
- Tetikleyici alt programları: `OP_Program` (`VdbeFrame`, `pRt` yazmacında tutulan çerçeve, özyineleme belirteci, "triggers nested too deep"), `OP_Param`, çerçevede `OP_Halt` (`sqlite3VdbeFrameRestore`, `OE_Ignore`), çerçevede `OP_Once` / `OP_NewRowid` P3 / `OP_MemMax`, `closeAllCursors`'ın çerçeve sırası.
- Yabancı anahtar: `OP_FkCounter`, `OP_FkIfZero`, `OP_FkCheck`, `nFkConstraint`, `nDeferredCons` / `nDeferredImmCons` (ifade geri alımında, `SAVEPOINT` / `ROLLBACK TO`'da saklanır / geri yüklenir, `COMMIT`'te ve otomatik işlem sonunda denetlenir).
- M1 artıkları: `window.c`'nin yerleşik pencere işlevleri (`row_number` … `nth_value`), `zeroblob()` (`MEM_Zero`), okuma programlarında `OP_TypeCheck`; M1 hatası `OP_NoConflict` NULL alan atlaması düzeltildi.
- Sınama kâhini: C sürücüsü hazırladığı programı kayıpsız döker (`progdump.rs` yükler); makine DML ifadelerini C'nin kendi programıyla çalıştırır. Üretici DML / tetikleyici / pencere kodlamadığı için yazma yolu yalnızca bu kâhin üzerinden çalışıyor; `SQLITERS_VDBE_EXEC=1` CLI'de yalnızca `SELECT`'i makineye götürüyor.

**Kanıt.**

| Ölçüm | Sonuç |
| :--- | :--- |
| DML farksalı (dmlgen 600 betik, H6 sc6/sc7/sc8 betikleri; ifadeler C'nin programıyla makinede) | 600 betikten 596'sı stdout + dosya baytı C ile aynı (eski yol 560); 4 fark motorun DDL iletisi (`no such table: main.t1`); sc6 138/145 (eski yol 134), sc7 75/75 (65), sc8 20/20 (15); makinede 66.700'den fazla ifade, geri düşme yalnız sanal tablo yazmaları (90) |
| İşlem kodu izi (sürücüler üzerinden, ifade ifade) | dml_a 11.275 / 11.275, dml_b 23.605 / 23.605, sc8 12.805 / 12.805, sc6 21.054 / 21.099 (fark: sanal tablo yazmaları geri düşüyor) |
| Önbellek karar izi | eski yol ile aynı ya da daha iyi; kalan farkların tamamına yakını sayfalayıcının tam geri alımı (C günlüğü önbelleğe geri oynatıyor, Rust önbelleği bırakıyor; `gorulen_farklar_m2.md` E11). `pb` olayları çıkarılınca dml_a 197/200, dml_b 398/400 |
| C kapsamı (gcov) | `OP_Program` 17/18 dal, `OP_FkIfZero` 8/10, `OP_FkCheck` 2/2, `closeCursorsInFrame` 4/4, `closeAllCursors` 7/8; alınmayanlar: bellek yetmezliği, `PRAGMA defer_foreign_keys` (motor desteklemiyor) |
| Çökme takımları | sürücü üzerinden (makine yazıyor) 120 A + 648 B kurtarma, 0 hata; CLI `crash_h1` 288 + 2.654, `crash_stmt` 84 + 432, 0 hata (anahtar açık) |
| API farksalı (adımlama, sıfırlama, kesme, ilerleme, şema) | 624 / 624 |
| Anahtar kapalı | testler 948 geçti (12 yeni), bilinen 4 başarısız, 337 yok sayıldı; dmlfuzz 377/400 (M1 ile aynı); ayrışma bulucu 241 / 245, başvurudan farklı 0; speedbench M1'e göre toplam 1,002x (geomean 1,002x; en büyük W10 1,023x, ölçüm gürültüsü düzeyinde), özetler eşit |
| Anahtar açık, test paketi | 943 geçti, 9 başarısız: bilinen 4 + M1'in 3 bulgusu + 2 test C'de olmayan davranışı sınıyor (ifade başlamadan verilen `sqlite3_interrupt` C'de sonraki ifadeyi durdurmuyor: `test_group3_entry_gate_…` ve `group46_…`; değiştirilmedi, anahtar kapalıyken eski yol bu davranışı veriyor) |

**Hız (anahtar açık; ayrı adım olarak önerildi).** C'ye göre: `W03` 2,05x, `W04` 2,56x, `W09` 4,80x, `W10` 2,33x, `W14` 1,99x (M1 makinesine göre W03 0,81x, W04 0,85x). Profil: W09'da süre motorun işlev kitaplığına adla yapılan çağrılarda (`find_scalar`, argüman vektörü, metin dönüşümleri, `LIKE`); W03'te zamanın %30'u her ifadede alınan POSIX kilidinde (`fcntl`).

**C'den yapı gereği farklı / yapılmayan.** `BEGIN` / `COMMIT` / `SAVEPOINT` / DDL programları (`OP_AutoCommit`, `OP_Savepoint`, `OP_SetCookie`, `OP_Expire`, `OP_CreateBtree` …) motorda kalıyor; sanal tablo yazmaları, `xfer` (`OPFLAG_PREFORMAT`), güncelleme kancaları; sıralayıcının disk birleştirmesi, geçici B-ağacının diske taşması, UTF-16. Görülen eski motor farkları: `gorulen_farklar_m2.md`.

## M3a — makine: DML'nin C'nin derleme sonucuyla makineye yönlendirilmesi, E16 / E17, sanal tablo yazmaları, xfer (dal `performans-A-vdbe-makine`, commit `2b0e6a8`)

**Taşınan / düzeltilen** (ayrıntı `karar_esligi_m3.md`).
- **DML yolu (E19):** `SQLITERS_VDBE_EXEC=1` iken INSERT / UPDATE / DELETE, C'nin derleme sonucundan geçiyor: üretici program veriyorsa makine çalıştırıyor; C hata veriyorsa prepare C'nin iletisi ve konumuyla başarısız oluyor; motorun prepare'inin reddi ya da kabulü artık karar vermiyor (bu yolda D43 / D44 kapandı). Motora yalnız üreticinin desteklemediği ifade türleri, yalnız motor kitaplığında olan işlevler (testfixture) ve mutation trace açıkken düşülüyor.
- **E16:** `sqlite3VdbeFindIndexKey`, `vdbeIsMatchingIndexKey`, `vdbeSkipField` taşındı; ifade indeksinde `OP_IdxDelete` C gibi komşu girdiyi buluyor.
- **E17 = D30:** kök neden makine değil motorun şema kurucusu. C eşdeğer UNIQUE / PRIMARY KEY kısıtlarını birleştirirken ilk indeksi ve onun sıralamasını tutuyor (build.c 4396–4458); motor PK'nın DESC sırasını yazıyordu. `schema.rs` düzeltildi; iki yolda da dosya baytları C ile aynı.
- **Sanal tablo yazmaları (E18):** `OP_VBegin`, `OP_VUpdate`, `OP_VOpen`, `OP_VFilter`, `OP_VColumn` (NOCHNG kuralı), `OP_VNext`, sanal tablo imlecinde `OP_Rowid`, ifade günlüğünde `sqlite3VtabSavepoint`. Yazma programlarında çalışıyor; tarama yalnız fts5 / rtree için.
- **xfer:** `OP_RowCell`, `OPFLAG_PREFORMAT` ile `OP_Insert` (B-ağacı aktarım kipi), `sqlite3_xferopt_count` C'nin saydığı yerde. Güncelleme kancası `OP_Insert` / `OP_Delete` içinde C'nin koşullarıyla. `max_page_count` her eklemeden sonra denetleniyor (E23: C'de sayfa alınırken).
- Anahtar açık testlerin bulduğu motor düzeltmeleri: üreticiye `Defensive` / `EnableTrigger` / `EnableView` bayrakları ve sütun / ifade derinliği / tetikleyici sınırları geçiyor; yeniden adlandırılan fts5 tablosunun `declare_sql`'i.

**Kanıt** (ikili `r6`; sonuçlar `target/perf-work/m3/res/`; betik sayısı, stdout ve dosya baytı C ile aynı).

| Derlem | API eski yol | API V5 makinesi | API yeni yol | CLI anahtar kapalı | CLI anahtar açık |
| :--- | ---: | ---: | ---: | ---: | ---: |
| ops_dml_a | 112 / 300 | 130 | **283** | 69 | **288** |
| ops_dml_t | 89 / 200 | 104 | **188** | 46 | **190** |
| ops_dml_f | 90 / 200 | 100 | **193** | 62 | **194** |
| ops_dml_tf | 57 / 150 | 66 | **142** | 29 | **142** |
| ops_dml_v | 51 / 150 | 57 | **138** | 30 | **137** |
| M2 dml_a | 188 / 200 | 200 | 200 | 188 | 200 |
| M2 dml_b | 374 / 400 | 398 | **400** | 373 | 398 |
| sc6 / sc7 / sc8 | 140 / 65 / 15 | 144 / 75 / 20 | 144 / 75 / 20 | 140 / 65 / 15 | 144 / 75 / 20 |

- Zengin derlemler (API): r_a / r_f / r_t / r_v 195 / 193 / 147 / 78 (eski yol 94 / 74 / 59 / 33). CLI'de anahtar kapalıyken 7 çökme (D48), açıkken 0.
- Kalan farklar: `random()` kullanmayan 14 betik; hepsi E20 (geçersiz UTF-8'in motor işlev kitaplığında U+FFFD'ye dönmesi) ya da bilinen sc6 fts5 dosya baytları. Geri düşme yalnız sc6 / sc7'deki 10 + 10 sanal tablo okuyan SELECT.
- C kapsamı (gcov) ve karar tabloları: örn. `sqlite3VdbeFindIndexKey` 27 / 30 satır, `OP_VUpdate` 27 / 29, `OP_VFilter` 19 / 19; alınmayan dallar sınıflandırıldı (G/Ç ve bozuk kayıt, sanal tablo imlecinde `nullRow`, `bIntegrity`). Ek C kanıtları: C'nin yazdığı dosyada FindIndexKey senaryosu, güncelleme kancası olay dizisi (`hook.c`), taşma sayfalı xfer (9 / 9 dosya baytı aynı).
- Çökme takımları (anahtar açık, yazmalar makinede): sürücü 120 + 648, CLI `crash_h1` 288 + 2.654, `crash_stmt` 84 + 432; 0 hata.
- Kendi denetimim: E16 (ifade indeksinde DELETE … RETURNING), E17 / D30, D48 (C'nin `generated column loop on "e"` iletisi), görünüme yazma, bilinmeyen ON CONFLICT hedefi ve işlev, FK CASCADE + tetikleyici + UPSERT + AUTOINCREMENT + xfer içeren betikler anahtar açıkken stdout ve dosya baytında C ile aynı (anahtar kapalı eski yolda E16 dosyası ve D48 farklı).
- Testler: anahtar kapalı 1023 geçti / 0 başarısız / 337 yok sayıldı (5 yeni); açık 1013 / 10 — bilinen 6 + C dışı davranış sınayan 4 yeni (F7): `group64` (`RETURNING main.rt.*` C'de `near "*": syntax error`), `phase3_end_to_end_differential_…` (C: `cannot modify v_items because it is a view`), `fts5bigtok2-1.1` (C'de bağlama SQLITE_TOOBIG ile başarısız, INSERT NULL ile başarılı; D50), `point_update_delete_plans_match_vdbe_programs` (motorun kendi programını inceliyor). Beklentiler değiştirilmedi: anahtar kapalıyken eski yol bu davranışı veriyor. DML farksalı: kapalı 378 / 400, **açık 398 / 400** (kalan 2 = D51). Ayrışma bulucu h7b ile aynı. Hız (anahtar kapalı, 1.100 tabanı): toplam 1,000x, geomean 0,993x.

**Hız (anahtar açık, C'ye göre; M3b konusu).** W03 1,95x, W04 2,60x, W09 4,58x, W10 3,77x, W14 8,30x (DML artık makinede olduğu için W14 belirgin yavaşladı).

**Yapılmayanlar.** İşlem ve DDL kodları (`OP_AutoCommit`, `OP_Savepoint`, `OP_SetCookie`, `OP_Expire`, `OP_CreateBtree`, `OP_Destroy`, `OP_ParseSchema`, `OP_Drop*`) taşınmadı; BEGIN / COMMIT / SAVEPOINT ve DDL motorda (E22). Sanal tablo okuyan SELECT'ler motora düşüyor (okuma host'unda yürütme bağlamı yok; E21), bu yüzden D37'nin kapandığı doğrulanmadı; `OP_VInitIn`, `OP_VCheck`, `OP_VRename` yok. `OPFLAG_PREFORMAT` ile `OP_IdxInsert` (yalnız VACUUM). Ön güncelleme kancası başvuru derlemesinde yok. E11'e dokunulmadı (hiçbir derlemde ve çökme takımında görünür etkisi yok).

**Kalanlar** (`gorulen_farklar_m3.md`). D49 metinden sayıya önek dönüşümü (`'4.24.2'+'4.24.2'` eski yolda 8, C'de 8,48; anahtar açık makine yolu C ile aynı); D50 bağlama SQLITE_MAX_LENGTH sınırını uygulamıyor; D51 bir DDL hatası prepare yerine step'te; E20–E23 yukarıda.

## M3c — makine: işlem ifadeleri, sanal tablo okumaları, `max_page_count` (dal `performans-A-vdbe-makine`, commit `93270fc`)

**Taşınan / düzeltilen** (ayrıntı `karar_esligi_m3c.md`).
- **İşlem ifadeleri (E22):** `OP_AutoCommit`, `OP_Savepoint`; `sqlite3VdbeHalt`'ın commit / geri alma kısmı (`halt_transaction`), C'nin `goto vdbe_return` davranışı (SQLITE_BUSY'de program sayacı korunuyor, ertelenmiş FK hatasında ifade durmuyor), `OP_Transaction`'da QueryOnly / CorruptRdOnly denetimi, BUSY'de devam, açılmamış TEMP'in atlanması. BEGIN / COMMIT / ROLLBACK / SAVEPOINT / RELEASE anahtar açıkken C'nin derleme sonucundan geçip makinede çalışıyor; BUSY'den sonra aynı program yeniden adımlanıyor. DDL motorda kaldı.
- **Sanal tablo okumaları (E21, D35, D37):** okuma host'u yürütme bağlamı içinde; her modül C'nin `xBestIndex` planını alıyor (`VirtualCursor::filter_c`; `seriesFilter`, `statFilter`, `pragmaVtabFilter` satır satır taşındı); FTS5 yardımcı işlevleri, alt tipler, konumlu r-tree imlecinin yazma kilidi. `OP_VInitIn`, `OP_VCheck` taşındı (hiçbir derlemde çalışmadı: modüller `sqlite3_vtab_in(..,1)` istemiyor, `OP_VCheck`'i yalnız üretilmeyen PRAGMA üretiyor). Sanal tablo okuyan SELECT'lerin motora düşmesi bitti.
- **E23:** `max_page_count` C'deki gibi sayfa ayrılırken sayfalayıcıda denetleniyor.
- Yolda kapanan C farkları: D52 RETURNING'li ifadenin `changes()` değeri, D53 OR FAIL sonrası başarısız commit'in kodu / iletisi, D54 CLI'de çok ifadeli satırda hatanın yanlış ifadeye atfedilmesi (D53 ve D54 iki yolda da).

**Kanıt** (anahtar açık; sonuçlar `target/perf-work/m3c/`).
- Yeni işlem derlemi (`gen_tx.py`, 400 betik; savepoint, ertelenmiş FK, tetikleyici, sanal tablo yazması, işlem içinde hata, ikinci bağlantı): API'de C ile aynı **381** (M3a ağacı 133, 394 geri düşme; şimdi 0), CLI'de 383 (M3a 380). Kalan 19 farkın hepsi anahtar kapalıyken de var (E24–E27, D56).
- Sanal tablo derlemlerinin **sorgu sonuçları** C'ye karşı (eşit / farklı ifade; önce motora düşüyordu): vt_a 8.356 / 1.796 → **9.978 / 179**, vt_b 4.204 / 858 → **4.964 / 98**, vt_t 1.654 / 375 → **1.984 / 45**, vt_x 2.519 / 187 → **2.590 / 66**, vt_e 54 / 1 → **55 / 0**, vt_d 151 / 10 → **159 / 2**. Makinede kalan farkların hepsi D57 (motorun pragma işlevlerinin çıktıları). 10 saniyeyi iki ikilide de aşan ifadeler karşılaştırılmadı.
- **D37 kapandı** (anahtar açık yol): `SELECT value FROM generate_series(0,3,1) WHERE stop=1` C'deki gibi satır döndürmüyor.
- M3a'nın derlemleri düşmedi: ops_dml_a 286 / 289 (API / CLI; M3a 283 / 288), ops_dml_v 140 / 139 (138 / 137), diğerleri aynı (ops_dml_f'deki 1 eksik `random()` kullanıyor); sc6 / sc7 geri düşmeleri 10 + 10 → 0; zengin r_a 196.
- C kapsamı (gcov): `OP_AutoCommit` satır 27 / 28, `OP_Savepoint` 83 / 85, `sqlite3VdbeHalt` 69 / 90, `seriesFilter` 104 / 139; alınmayan dallar sınıflandırıldı. vt_a / vt_b kapsamı ölçülmedi (-O0 kabuk çok yavaş).
- Çökme takımları (işlemler de makinede): sürücü 120 + 648 (makinede 1.320 ifade, geri düşme 0), CLI `crash_h1` 288 + 2.654, `crash_stmt` 84 + 432; 0 hata.
- Kendi denetimim: iç içe BEGIN, ertelenmiş FK ile başarısız COMMIT, SAVEPOINT / ROLLBACK TO / bilinmeyen RELEASE, savepoint içinde DDL geri alımı, fts5'e işlem içinde yazma ile; generate_series (D37 dahil), json_each / json_tree, pragma_table_info / index_list, dbstat, rtree, fts5 highlight / bm25 okuyan betikler anahtar açıkken stdout ve dosya baytında C ile aynı (anahtar kapalı eski yolda sanal tablo betiğinin çıktısı farklı).
- Testler: anahtar kapalı 1028 geçti / 0 başarısız / 337 yok sayıldı (5 yeni); açık 1015 / 13 (bilinen 10 + F8'in 3'ü). DML farksalı kapalı 378 / 400, açık 398 / 400. Ayrışma bulucu kapalı h7b + g06; açık tek fark health/s10_without_rowid (makine C ile aynı, başvuru eski yol). Hız (anahtar kapalı, 1.100 tabanı): toplam 1,020x, geomean 1,001x (W11 1,017–1,025x, gürültü sınırında); `Pager`'a eklenen alan yapının başındayken W12 1,056x yavaşlamıştı, sona taşınınca 0,997x.

**Değişen test beklentisi yok.** Anahtar açıkken yeni düşen 3 test C'de olmayan davranışı sınıyor (F8): `group64_quoted_dots…` ve `test_gate2_3…` (`SELECT main."a.b".*`, `SELECT aux.items.*`; C `near "*": syntax error`), `test_wal_jsonb_window…` (trusted_schema=OFF altında MATCH içeren görünüm; C `unsafe use of MATCH()`). 5 yeni test (`c_align_m3c_tests.rs`).

**Yapılmayanlar.** E28: COMMIT'in yazımı G/Ç hatasıyla düşerse C işlemi geri alıyor, Rust açık bırakıyor (Rust sayfalayıcısında PAGER_ERROR durumu yok). D55 `PRAGMA query_only` motorda yok. DDL kodları (`OP_SetCookie`, `OP_CreateBtree`, `OP_ParseSchema`, `OP_VRename` …) taşınmadı. E11 görünür etkisi olmadığı için taşınmadı (M2'nin önbellek izi yeniden koşulmadı).

**Kalanlar** (`gorulen_farklar_m3c.md`). **D58 (doğrularken buldum):** motorun ayrıştırıcısının reddettiği SELECT, C'nin derleme sonucuna hiç ulaşmıyor: `SELECT main."a.b".* FROM (SELECT 1)` anahtar açıkken de motorun `no such table: main.a.b` iletisini veriyor, C ise `near "*": syntax error`. Üretici C'nin hatasını veriyor (`EXPLAIN` önekiyle aynı); F8'deki iki test bu yüzden C'den farklı. D56 etkisiz `CREATE INDEX IF NOT EXISTS` / `DROP TABLE IF EXISTS` yazma kilidi alıyor; D57 motorun pragma işlev çıktıları; E24 WAL değişiklik sayacı; E25 cache_size=2 ile fts5 + auto_vacuum boş alan baytları; E26 fts5 DROP'un `total_changes` sayımı; E27 önbellek taşma zamanlaması (E24–E27 anahtar kapalıyken de var); E28.

**Hız (anahtar açık, C'ye göre; M3b konusu).** W03 2,03x, W04 2,56x, W09 4,66x, W10 3,80x, W14 8,36x.

### D58 düzeltmesi (M3c'den sonra, aynı dal)

- **D58 kapandı:** anahtar açıkken motorun ön derlemesinin reddettiği bir SELECT'te artık C'nin derleme sonucu karar veriyor (`lib.rs` `c_read_decides`): C'nin hatası C'nin konumuyla, ya da C'nin programı makinede. `SELECT main."a.b".* FROM (SELECT 1)` ve benzerleri C gibi `near "*": syntax error`; yeni test `d58_rejected_select_gets_c_compile_error`. Bilinmeyen sütun / tablo, UNION sütun sayısı gibi yaygın hatalar C ile aynı kaldı.
- **E29 (düzeltirken bulundu ve düzeltildi):** kod üreticisine yalnız `sqlite3_vtab_config()` ile verilen risk bayrağı geçiyordu; motorun modül düzeyindeki risk bayrağı geçmiyordu. Bu yüzden görünüm içindeki `DirectOnly` modül üreticide derleniyordu; C ve motor `unsafe use of virtual table` veriyor. Artık `effective_risk_flags()` geçiyor. Yerleşik modüller (generate_series, json_each, json_tree, pragma_table_info, dbstat, fts5, rtree) görünüm içinde `trusted_schema` kapalı ve açıkken iki yolda da C ile aynı.
- Testler: anahtar kapalı 1029 geçti / 0 başarısız / 337 yok sayıldı; açık 1014 / 15 = bilinen 10 + F8'in 3'ü (`group64_quoted_dots…` ve `test_gate2_3…` artık C'nin hatasını aldığı için düşüyor) + **F9'un 2'si**: `SELECT random(*)` C'de hatasız bir satır döndürüyor, `w REGEXP '^alpha'` için C'nin iletisi `no such function: REGEXP` (C kitaplığına karşı bir C programıyla doğrulandı). Beklentiler değiştirilmedi: anahtar kapalıyken eski yol bu davranışı veriyor.

## Bu adımda görülen, eski motora ait C farkları (dokunulmadı)

| İfade | C | Motor |
| :--- | :--- | :--- |
| `SELECT 'a' REGEXP 'b';` (ve `sha1`, `base64`, `decimal`, `ieee754` …) | `0` | `no such function: regexp` (kabuk işlevleri eksik) |
| `SELECT aux.items.* FROM aux.items;` | sözdizimi hatası | kabul ediliyor |
| `SELECT ?0;` | `variable number must be between ?1 and ?32766` | `?0 is not a valid SQLite parameter` |
| `SELECT CASE END;` | `near ";": syntax error` | `CASE requires at least one WHEN branch` |
| `BEGIN TRANSACTION x;` | kabul ediliyor | `near "x": syntax error` |
| `SELECT group_concat(1 ORDER BY 2);` | `1` | `near "ORDER": syntax error` |
| `SELECT CAST(x'41ABCD42' AS TEXT);` | ham baytlar | iki kez U+FFFD |

Bu farklar `EXPLAIN` önekiyle artık C ile aynı; yürütme yolu taşındığında kapanacak.

## V6b.4 — synchronous

Hazırlama etkisi ve sonuç anı C gibi taşındı; 4–6 seviyeleri, TEMP ve işlem içi hatalar doğrulandı. Sayfalayıcı güvenlik seviyesi ile fullfsync bayrakları ayrıldı. Dört kapı ve 896 gerçek GÇ senaryosu [karar_esligi_v6b_4.md](karar_esligi_v6b_4.md) içinde; alınmayan dallar ayrı listede. V6b sürüyor; sonraki dilim page_size ve auto_vacuum.

## V6b.5 — page_size

Hazırlama etkisi ve donmuş sorgu taşındı; nextPagesize bağlantı geneline alındı. Açılıştaki ham başlık okuması, TEMP, farklı şemanın VACUUM işlemi, geçersiz istek ve WAL sıfırlaması C ile doğrulandı. Dört kapı, 176 API akışı ve alınmayan dallar [karar_esligi_v6b_5.md](karar_esligi_v6b_5.md) içinde. Sonraki adım auto_vacuum; V6b sürüyor.

## V6b.6 — auto_vacuum

Üretici, hazırlama etkileri ve meta7 yazımı taşındı. Global istek, donmuş sorgu/operand, TEMP ve canlı Btree bayraklarının commit/rollback sonrası ömrü C ile eşleştirildi. Dört kapı, 220 API akışı ve her alınmayan dal [karar_esligi_v6b_6.md](karar_esligi_v6b_6.md) içinde. Sonraki dilim cache_spill, busy_timeout ve wal_autocheckpoint; V6b sürüyor.

## V6b.7 — bağlantı ve cache ayarları

Üç ayarın hazırlama etkileri üreticiye taşındı. TEMP data_version, EXPLAIN kilidi ve ilk şema/kodlama yüklemesinin bekleyen ifadeleri yeniden hazırlaması C ile eşleştirildi. Dört kapı, program/API/GÇ kanıtları ve her alınmayan dal [karar_esligi_v6b_7.md](karar_esligi_v6b_7.md) içinde. V6b sürüyor; öncelik kalan doğruluk eksikleridir.

## V6b.8 — kilit kipi ve hazırlama doğruluğu

locking_mode üretimi ve hazırlama etkileri taşındı. Tutulan kilitte sayfa 1 sayaç yaşamı, fiziksel damga, erken/geç sentaks etkileri ve hata konumu geçmişi C ile karşılaştırıldı. Eski yürütücüde tüketilmeyen TVF argümanı D103 açık bırakıldı; başarı sayılmadı. Dört kapı, API/dosya/GÇ kanıtı ve alınmayan dallar [karar_esligi_v6b_8.md](karar_esligi_v6b_8.md) içinde. Sonraki uygun dilim temp_store; doğruluk önceliği sürer.

## V6b.9 — geçici saklama hazırlaması

temp_store hazırlama etkileri, TEMP işlem koruması, şema temizleme, cache_size koruma ve donmuş getter C ile karşılaştırıldı. İlk adayın 72 getter farkı kapandı; önceki D103 açık kaldı. Kanıt ve dört kapı [karar_esligi_v6b_9.md](karar_esligi_v6b_9.md) içinde. Sonraki uygun dilim encoding; doğruluk önceliği sürer.

## V6b.10 — kodlama hazırlaması

encoding üretimi ve hazırlama etkileri, aynı atamada advisory expire, sorgunun şema okuması, boş pagerlerin kodlaması ve kilitsiz setter C ile karşılaştırıldı. Üç mevcut dosya kodlaması değişmeden açıldı. Kanıt ve açık sınırlar [karar_esligi_v6b_10.md](karar_esligi_v6b_10.md) içinde. Önce D111 ve sanal tablo bağlama doğrulaması, ardından kalan PRAGMA doğruluk dilimleriyle devam edilecek.

## V6b.11 — üretici doğrulama durumu

D111 ve ilk sanal tablo bağlama sınırı kapandı; 11.172 tam program aynı ilk-kullanım girdisinde C ile eşleşti. Bağımsız bozuk şema yoklaması D112/D113 farklarını gösterdi; bunlar sonraki adıma önceliklendirildi. Kanıt ve dört kapı [karar_esligi_v6b_11.md](karar_esligi_v6b_11.md) içinde.

## V6b.12 — şema başlatma sırası

D113 hem yeni fiziksel şema yükleyicisinde hem açık işlemde önbellek yenileyicisinde düzeltildi. 256 bozuk şema/işlem girdisi ile isim çözümleme girdisi iki anahtar durumunda C ile karşılaştırıldı; bütün dosya baytları aynı. D112 gereksiz şema okuması sonraki adımdır. Kanıt ve dört kapı [karar_esligi_v6b_12.md](karar_esligi_v6b_12.md) içinde.

## V6b.13 — SELECT şema gereksinimi

Üreticinin şema ve Btree kararları hazırlama/yeniden hazırlama/kiralama yoluna bağlandı; D112/D114 UTF-8 yoklamaları C ile aynı. UTF-16 false AND sınırı D115 açık. Ek üretici yoklamasının toplam 1,062x ve W14 1,369x maliyeti kaydedildi; performans ayarı yapılmadı. Kanıt ve dört kapı [karar_esligi_v6b_13.md](karar_esligi_v6b_13.md) içinde.

## V6b.14 — UTF-16 boş yürütme yolu

D115’in kaydedilen satırsız programı ve D116 yeniden hazırlama sayaç kaybı düzeltildi. 3.076 API akışının bütün dosya baytları aynı; tek çıktı farkı D103. Genel UTF-16 ve kalan işler açık. Kanıt ve dört kapı [karar_esligi_v6b_14.md](karar_esligi_v6b_14.md) içinde.

## V6b.15 — threads

threads hazırlama etkisi, bağlantı sınırı ve pragma_threads eklendi; başvuru işçi üst sınırı 8 ile hizalandı. 3.996 API akışının bütün dosyaları aynı; tek çıktı farkı D103. Sonraki adım shrink_memory hazırlamasıdır. Kanıt ve dört kapı [karar_esligi_v6b_15.md](karar_esligi_v6b_15.md) içinde.

## V6b.16 — shrink_memory ve D117

shrink_memory hazırlama etkisi taşındı; bütün 4.158 API akışının dosya baytları aynı. VFS hata enjeksiyonu D117 başlık okuma farkını gösterdi; sonraki adım bu farkın düzeltilmesidir. TEMP/journal gözlemleri de açık. Kanıt ve dört kapı [karar_esligi_v6b_16.md](karar_esligi_v6b_16.md) içinde.

## V6b.17 — D117, D118 ve D119

Sıcak okumanın D117 IOERR_READ farkı yoklanan 30 akışta giderildi; pending WAL guard’ı 896/896 dosyayla doğrulandı. D119 hata aktarımı ve D118 kısa okuma/sayfa1 yükleme farkları sonraki adım. Genel hot-journal/E28 ve TEMP/journal zamanlama gözlemleri açık. Kanıt [karar_esligi_v6b_17.md](karar_esligi_v6b_17.md) içinde.

## V6b.18 — D119

Sayfa1 okuma hatası artık yutulmuyor. Yürütme ve hazırlama hata aşamaları farklı rollback kararlarını izliyor; 56 yürütme, 16 reprepare ve 64 callback/sayaç akışı doğrulandı. D118 kısa okuma sonraki adım; genel E28/M4/legacy hızlı yollar açık. Kanıt [karar_esligi_v6b_18.md](karar_esligi_v6b_18.md) içinde.

## V6b.19 — D118 kısa okuma

Başlık kısa okuma düzeltildi; genişletilmiş matris 640 akışın 624’ünde eşlik gösterdi. D120/D121/D122 toplam 16 kusur açık; bütün dosya baytları aynı. D118 tam kapanmadı; sırada sayfa1 yükleme/önbellek eşliği var. Kanıt [karar_esligi_v6b_19.md](karar_esligi_v6b_19.md) içinde.

## V6b.20 — sayfa1 önbelleği ve bozuk kayıt

D118’in kaydedilen akışları ve D120/D121/D122 giderildi; 640+128 native senaryo eşleşti. Ham G/Ç izinde 52/72 aynı; dirty/TEMP farkları açık. Sırada kalan PRAGMA işleyicileri var. Kanıt [karar_esligi_v6b_20.md](karar_esligi_v6b_20.md) içinde.

## V6b.21 — database_list

Liste hazırlamada sabitlendi; reset/DETACH expire, aktif satırken DDL ve geçerli UTF-16 sonuçlar C ile eşleştirildi. 12 eski/yeni, 210 genişletilmiş ve 12 dosyaya bağlı aux akışı sonuç/hata/dosya baytlarında aynı. 13192 tam program eşleşti; 573 alınmayan dal ayrı sınıflandırıldı. Kalan PRAGMA’lar ve M4 açık; sırada collation_list var. Kanıt [karar_esligi_v6b_21.md](karar_esligi_v6b_21.md) içinde.

## V6b.22 — collation_list ve kabuk kayıtları

C hash sırası/kayıt geçmişi ile UTF-8 BUSY/expire hizalandı. Kabuk uint/decimal karşılaştırıcıları C kaynaklarından taşındı; 7536 girdi ve 36 kabuk/indeks dosyası akışı eşleşti. 438 native akışta 426 aynı; 12 eski SELECT hatası D128 olarak açık ve sonraki doğruluk işidir. 589 alınmayan dal ayrı sınıflandırıldı. Kanıt [karar_esligi_v6b_22.md](karar_esligi_v6b_22.md) içinde.

## V6b.23 — D128

Eksik karşılaştırıcının C 257 hazırlama hatası korundu; C’nin sabit koşulla attığı karşılaştırma çalışma anında denetlenmez. 636 yeni ve 438 önceki akış sonuç/hata/konum/sekiz sayaç/dosya baytlarında aynı. Kalan PRAGMA’lar ve genel collation/legacy yolları açık. Kanıt [karar_esligi_v6b_23.md](karar_esligi_v6b_23.md) içinde.

## V6b.24 — compile_options

Sabit C başvuru derlemesinin 42 seçenekli tanı profili gerçek üreticide; get/used SQL işlevleri makinede çalışır. 3.990 native akış sonuç/hata/konum/sayaç/dosya baytlarında aynı. COMPILER satırı uyumluluk hedefini anlatır; genel özellik eşliği iddiası değildir. Kalan PRAGMA’lar sürer. Kanıt [karar_esligi_v6b_24.md](karar_esligi_v6b_24.md) içinde.

## V6b.25 — defer_foreign_keys ve normal FK kapanışı

Prepare anı ile live bayrak ayrıldı; normal işlem sonu ve şema yükleme etkileri hizalandı. Erken RESTRICT denetimi C’nin parent yazımı sonrası FK eylem sırasına taşındı; D130 başlık bayt farkı kapandı. 1624/1644 native tam aynı; kalan 20 eski D52 ve bağımsız altı D131 açıktır. Bütün dosya baytları eşit. Sonraki doğruluk işi EXPLAIN yazıcı sayacı ve bekleyen işlem kapanışıdır. Kanıt [karar_esligi_v6b_25.md](karar_esligi_v6b_25.md) içinde.

## V6b.26 — EXPLAIN yazıcı ömrü ve D131

EXPLAIN, açıkladığı programın yazıcı sayımına katılır ve EOF sonrasında reset/finalize’a kadar etkin kalır. Kapanıştaki değişiklik sayacı, FK hatası ve BUSY C ile hizalandı; commit-hook hata metni D132 olarak açık. 72+408 native akış ve eski altı D131 aynı; bütün ana dosya baytları eşit. D52/D103/E28 ve kalan PRAGMA/M4 işleri açık. Kanıt [karar_esligi_v6b_26.md](karar_esligi_v6b_26.md) içinde.

## V6b.27 — D132 commit hook hata metni

531 hook reddi C’nin constraint failed iletisini döndürür. 192/192 EXPLAIN ve 26/36 doğru yazmalı doğrudan işlem akışı tamamen aynı; eski 72+36 hata metni farkı giderildi. 10 D133 legacy autocommit farkı sıradaki iş. D52/D103/E28 ve kalan PRAGMA/M4 açıktır. D133 sonrasında case_sensitive_like hazırlama etkisine dönülecek. Kanıt [karar_esligi_v6b_27.md](karar_esligi_v6b_27.md) içinde.

## V6b.28 — D133/D134 savepoint ve callback autocommit

Legacy COMMIT/transaction RELEASE, callback sırasında C gibi autocommit 1 gösterir; reddedilmiş RELEASE savepoint zincirini ve autocommit 0 durumunu korur. 144 callback + 72 kilit + 96 FK ve eski 36 doğrudan hook akışı tamamen aynı. D52/D103/E28 ve kalan PRAGMA/M4 açıktır. Sırada case_sensitive_like var. Kanıt [karar_esligi_v6b_28.md](karar_esligi_v6b_28.md) içinde.

## V6b.29 — case_sensitive_like

Hazırlama etkisi, iki LIKE kaydı, BUSY/expire ve paylaşılan builtin bayrakları C ile hizalandı; açılan makine yolunda UTF-16 Column/Concat/işlev-sonuç kodlaması düzeltildi. 1116 yeni native akış aynı; 48 bağlı desen örneğinde D139 status farkı açık ve sonraki doğruluk işidir. Kapalı testte mevcut F4 beklentisi görünür oldu; C API ile doğrulandı, beklenti korunuyor. D52/D103/E28 ve kalan PRAGMA/M4 açık. Kanıt ve dört kapı [karar_esligi_v6b_29.md](karar_esligi_v6b_29.md).

## V6b.30 — bağlı LIKE yeniden üretimi

Bağlı değer, maskeli bind/clear ve dummy Variable üretimi C ile hizalandı; V1, EXPLAIN ve sayaç aktarımı denetlendi. D139 ana matrisinde 40/48 tam eşit, 8 UTF-16 NOCASE farkı açık. 144 bound açıklama, 48 ömür, 84 API/parametre ve 144 tam program karşılaştırması aynı. D140 NUL/NOCASE ve D141 ham Unicode üst sınırı farkları doğrulandı; sıradaki iş kodlama/collation/ham token baytlarıdır. D52/D103/E28 ve kalan PRAGMA/M4 sürer. Kanıt [karar_esligi_v6b_30.md](karar_esligi_v6b_30.md).

## V6b.31 — UTF-16 LIKE ve ham kodlama kararları

D139 kalan UTF-16 NOCASE, D140 NUL karşılaştırma/gruplama ve D141 ham Unicode aralık örnekleri eşitlendi. Açılan UTF-16 yolda bulunan TEXT/BLOB CAST ve sayısal dönüşüm farkları giderildi. 528 tam program, 72 ek SQL, 489145 ham dönüşüm/karşılaştırma ve 1587945 sayısal hücre gözleminde fark yok. İki anahtar testi ve dört kapı denetlendi; eski kusur/beklenti kümeleri korundu. Genel geçersiz TEXT API temsili, NOMEM, GLOB, kalan PRAGMA/M4 ve E28 açıktır. [Karar eşliği](karar_esligi_v6b_31.md).

## V6b.32 — mmap hazırlama etkisi ve TEMP dosya ömrü

MMAP_SIZE hazırlama etkisi ve VFS sınırı üreticiye taşındı; TEMP dosyası ilk yazma yerine ilk dirty spill’de açılır, küçük commit dirty sayfaları önbellekte bırakır. 270 TEMP akışı, 144 tam program ve 78 ek ifade aynı. Native mmap 240/250 eşit; dört D144 offset ve altı D142 mapped-reader farkı açık. nFetchOut/gerçek harita yaşamı sıradaki doğruluk işidir; mmap tamamlandı sayılmaz. Yeni F10–F12 test beklentileri C VFS gözlemcisiyle sınıflandırıldı, değiştirilmedi. [Kanıt](karar_esligi_v6b_32.md).

## V6b.33 — VFS fetch/unfetch yaşamı

Unix VFS'de gerçek read-only mmap, fetch lease sayısı, EOF256 ve aktif ref sırasında limit guardı eklendi. 10.051 C/model/Rust işlemi ve dosya hashleri aynı. Pager bağlantısı yapılmadığından D142 altı örnekte açık; D144 dört örnek de sürüyor. Native mmap240/250, TEMP270/270, tam program144/144, elle CLI16/16. Dört kapıdaki eski fark kümeleri korundu; OFF(1131, 4, 337), ON(1117, 18, 337); hız Rust1.100 tabanına 1.210x ve14 digest farkı0. Mremap, mmap hata günlüğü ve hata enjeksiyonu tamamlandı sayılmaz. [Kanıt](karar_esligi_v6b_33.md). Kullanıcının sohbet devri için [devam belgesi](devir_v6b_33.md); sıradaki iş V6b.34/D142 sayfalayıcı bağlantısı.


## V6b.34 — D142 sayfalayıcı bağlantısı

B-tree imleçleri Unix xFetch lease'lerini PCache'den ayrı tutuyor. Etkin okuyucunun mmap sınırını koruduğu altı önceki D142 akışı artık eşit. Dosya büyümesinden doğan 36 yeni fark SIZE_HINT ile kapandı ve büyüme/geometri 52/52 oldu. Önceki native mmap 246/250, genişletilmiş native 433/440. Kalıcı dosya hashleri eşit. Kalan çıktılar önceki dört D144 ve yedi hata/bozuk dosya farkıyla aynı. Gcov 150 dal kaydetti ve alınmayan 47 çıkış tek tek sınıflandırıldı. OFF 1135/4/337, ON 1121/18/337. DML ve ayrışma bulucu V6b.33 ile aynı. Rust 1.100 tabanına süre oranı 1,225 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_34.md).

## V6b.35 — D144 DDL adı hazırlama tanısı

Tablo ve görünüm adı çakışmaları ham SQL token'ı ve bayt ofsetiyle hazırlanıyor. V2 ve V1 için 464'er native akış, 14 kabuk denetimi ve önceki 250 mmap akışı C ile eşit. Genişletilmiş mmap 433/440 ve önceki yedi ham fark sürüyor. 64 C dalının alınmayan 18 çıkışı ayrı sınıflandırıldı. OFF 1136/4/337, ON 1122/18/337. DML ve ayrışma bulucu V6b.34 ile aynı. Rust 1.100 tabanına süre oranı 1,235 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_35.md).

## V6b.36 — Üretilen sütun kısıtlarının hazırlama sırası

Üretilen sütunun PK ve DEFAULT tanıları gerçek hazırlamada C'nin hata sırasını kullanıyor. Mevcut tabloya ya da görünüme yönelik IF NOT EXISTS, gövdeyi işlemeden şemayı okuma işlemiyle doğruluyor. 234/244 native ve 22/22 kabuk akışı eşit. Kalan 10 native fark önceki boş dosya V1 yürütme sorunu. DML OFF 380/400, ON 400/400. Çalışma alanı OFF 1137/4/337, ON 1123/18/337. Gcov 64 dal ve alınmayan 8 çıkış sınıflı. Rust 1.100 tabanına süre oranı 1,237 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_36.md).

## V6b.37 — İlk şema sayfası ve V1 yürütme

Boş veritabanında ilk sayfanın tahsisi çerez ve şema biçimi değişmedikçe hazırlanmış ifadeyi eskitmiyor. 468 native akışta 330 yerine 402 aynı. V6b.36 üretilen sütun matrisi 244/244 oldu. Gerçek eskime matrisi önceki sonuçla aynı. Gcov 44 dal ve alınmayan 13 çıkış sınıflı. OFF 1138/4/337, ON 1124/18/337. DML ve ayrışma bulucu aynı. Rust 1.100 tabanına süre oranı 1,228 ve 14 digest eşit. WAL, savepoint dosya baytları, V1 eskime hata maskelemesi ve diğer açık işler sürüyor. [Karar ve doğrulama](karar_esligi_v6b_37.md).

## V6b.38 — D103 gizli şema sütunu

Tek kaynaklı `pragma_locking_mode` sorgusunda C programı kullanılmayan gizli sütun koşulunu denetliyor. 488 eski bağlantı akışındaki kayıtlı D103 kapandı. Yeni native matris 176/184 ve kabuk 18/18. Sekiz V1 iç hata ofseti açık. Gcov 46 dal ve alınmayan 10 çıkış sınıflı. OFF 1139/4/337, ON 1125/18/337. DML ve ayrışma bulucu aynı. Rust 1.100 tabanına süre oranı 1,228 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_38.md).

## V6b.39 — D143 native modül kaydı

Beş yerleşik modül, ilk başvuruda JSON/PRAGMA kaydı, C hash sırası ve hazırlamada donan liste 46/46 native akışta eşleşti. 96 modüllü geçmişte liste sırası eşit. Bağlı modül değiştirme ve kabukta fsdir/completion açık. Kabuk denetimi 4/12. Gcov 64 dal ve alınmayan 13 çıkış sınıflı. OFF 1140/4/337, ON 1126/18/337. DML ve ayrışma bulucu aynı. Rust 1.100 tabanına süre oranı 1,224 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_39.md).

## V6b.40 — POSIX kabuk modülleri

Kabuk gerçek fsdir ve completion hizmetlerini kaydediyor. Normal ve güvenli kip dahil 36 completion, 42 fsdir ve 12 kabuk profili akışı C ile aynı. Native modül, D103, DDL ve mmap kayıtları aynı kaldı. Gcov 214 dal ve alınmayan 43 çıkış sınıflı. OFF 1141/4/337, ON 1127/18/337. DML ve ayrışma bulucu aynı. Rust 1.100 tabanına süre oranı 1,227 ve 14 digest eşit. E28 güncel çekirdekte DELETE ve WAL yazım/senkronizasyon hatalarında yeniden üretildi. [Karar ve doğrulama](karar_esligi_v6b_40.md).

## V6b.41 — E28 ve Pager ERROR

IOERR/FULL spill, faz iki ve rollback hataları pager ERROR durumunu kuruyor. DELETE sıcak journal kurtarması, WAL hata sonrasında kilitlerin bırakılması ve COMMIT rollback akışı doğrulandı. C ile iki Rust kipinde 33/34 native akış ve eski beş E28 COMMIT akışı eşit. Altı DELETE data_version akışı da eşit. EXCLUSIVE DELETE sonlandırması ve sekiz WAL VFS yazım izi farkı açık. Gcov 272 dal, alınmayan 97 çıkış ayrı sınıflı. OFF 1140/7/337, ON 1126/21/337. Önceki başarısızlıklar ile F13–F15 kayıtlı. DML ve ayrışma bulucu V6b.40 ile aynı. Rust 1.100 süre oranı 1,239 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_41.md).

## V6b.42 — D145 EXCLUSIVE DELETE ve WAL yazma sınırları

EXCLUSIVE DELETE journal başlığı ve tanıtıcı ömrü ile WAL header/payload yazma boyutları ve ofsetleri C ile hizalandı. SQL ROLLBACK pager hatası ve sıcak nRec sıfır kurtarma davranışı düzeldi. 88 native akış, sekiz WAL checksum zinciri yeniden yazımı ve 147 kesinti/kurtarma imajı eşit. Gcov 168 dal ve alınmayan 36 çıkış sınıflı. OFF 1142/7/337, ON 1128/21/337. DML ve ayrışma bulucu V6b.41 ile aynı. Rust 1.100 süre oranı 1,247 ve 14 digest eşit. Sıfır baytlık ana DB yanında kalan sıcak journal seçimi D146 olarak açık. [Karar ve doğrulama](karar_esligi_v6b_42.md).

## V6b.43 — D146 sıcak journal seçimi

Sıfır baytlık DB'nin yanındaki eski journal artık RESERVED kilidi altında temizleniyor. İlk CANTOPEN, ilk kısa xRead ve RO açılış hatası C ile aynı. 16 dosya, dört gerçek çöküş imajı ve 20 native akış eşit. V6b.42'nin 88 native ve 147 kurtarma sonucu korundu. Boş WAL ile BUSY kip değişikliğinde alınan geçici SHARED kilidi bırakılıyor. Gcov 88 dal, alınmayan 35 çıkış sınıflı. OFF 1143/7/337, ON 1129/21/337. DML ve ayrışma bulucu V6b.42 ile aynı. Rust 1.100 süre oranı 1,238 ve 14 digest eşit. Sıcak kayıt sırası ve okuma sınırı D147 olarak açık. [Karar ve doğrulama](karar_esligi_v6b_43.md).

## V6b.44 — D147/D148 sıcak journal playback

Sıcak journal kayıtları ayrı numara, sayfa ve checksum okumalarıyla sırayla uygulanıyor. Kısa kayıt okuması playback'i sonlandırıyor ve diğer IOERR kodları korunuyor. Ana dosya uzunluğu eşitse bırakılıyor, küçültme xTruncate, tam sayfalık büyütme sıfır sayfa xWrite ile yapılıyor. 28 native akış, 12 dosya imajı, dört gerçek C çöküşü ve önceki 147 kurtarma vakası eşit. Gcov 56 dal, alınmayan 20 çıkış sınıflı. OFF 1144/7/337, ON 1130/21/337. DML ve ayrışma bulucu V6b.43 ile aynı. Rust 1.100 süre oranı 1,229 ve 14 digest eşit. D149 journal bölüm başlığındaki xRead sınırları açık. [Karar ve doğrulama](karar_esligi_v6b_44.md).

## V6b.45 — D149 sıcak journal bölüm başlığı

Sıcak journal önce super-journal trailer'ını denetliyor, sonra magic ve 32 bit başlık alanlarını C sınırlarında okuyor. Başlık ve kayıt kısa okumaları ayrı sonuç veriyor. 203 native vaka, özgün dört vaka ve sekiz sync sınırı vakası C ile eşit. Önceki 147 kurtarma vakası, 12 dosya imajı ve dört gerçek çöküş imajı eşit. Ana gcov kapsamı 96 dal ve alınmayan 11 çıkış. OFF 1144/7/337, ON 1130/21/337. DML ve ayrışma bulucu V6b.44 ile aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. D150 sıcak DELETE sonlandırma sırası açık. [Karar ve doğrulama](karar_esligi_v6b_45.md).

## V6b.46 — D150 normal sıcak DELETE sonlandırması

Normal sıcak DELETE journal xClose, kipin gerektirdiği xDelete ve SHARED sırasıyla sonlandırılıyor. Birleşik hatada ilk sonlandırma hatası dönüyor. Super-journal temizliği başarılı SHARED sonrasında geliyor. 40 native ve 24 ek sayfa boyutu vakası C ile eşit. D149'un 203 başlık vakası ve önceki 147 kurtarma vakası tekrar eşit. Gcov 18 dal ve alınmayan 7 çıkış. OFF 1145/7/337, ON 1131/21/337. DML ve ayrışma bulucu V6b.45 ile aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. D151 PERSIST/TRUNCATE ve D152 sync file-control açık. [Karar ve doğrulama](karar_esligi_v6b_46.md).

## V6b.47 — D151 sıcak PERSIST ve TRUNCATE journal sonlandırması

Sıcak PERSIST ve TRUNCATE aynı açık tanıtıcıyla sonlandırılıyor. JournalOff sıfırsa mevcut dosya korunuyor. 24 native vaka, sekiz cihaz özelliği ve altı 2PC commit noktası vakasında sorgu izleri ile tüm dosya özetleri C ile eşit. Gerçek C 2PC görüntüsü iki DB açılış sırasında üç motorda eşit. Eski PERSIST sıfır uzunluk beklentisi C'nin koruduğu tüm dosya baytlarını karşılaştıracak şekilde doğrulandı ve düzeltildi. Tekrar sonrası OFF 1145/7/337, ON 1131/21/337, başarısız kümeler aynı. DML ve ayrışma bulucu V6b.46 ile aynı. Rust 1.100 süre oranı 1,238 ve 14 digest eşit. Gcov ana ölçümde 42 dal, dokuz alınmayan çıkış. D152 sync bildirimi ve D153 terminal kilit ve journal sync sırası açık. [Karar ve doğrulama](karar_esligi_v6b_47.md).

## V6b.48 — D152 sıcak journal kurtarmasında DB sync bildirimi

Sıcak playback `SQLITE_FCNTL_SYNC` bildirimini ana DB fiziksel sync seçiminden önce gönderiyor. NOTFOUND kurtarmayı sürdürüyor, G/Ç hatası journal'ı yeniden denemeye bırakıyor. 20 native vaka ve altı C çıkışı doğrulandı. D151'in 24 kurtarma ve sekiz cihaz yeteneği vakası ile önceki 147 kurtarma vakası korundu. OFF 1146/7/337, ON 1132/21/337. DML ve ayrışma bulucu V6b.47 ile aynı. Rust 1.100 süre oranı 1,244 ve 14 digest eşit. D153 terminal bağlantı kapatma sırası açık. [Karar ve doğrulama](karar_esligi_v6b_48.md).

## V6b.49 — D153 olağan rollback-journal bağlantı kapanışı

Olağan disk kapanışı açık journal için sync ve boyut okumasını, ardından kilit ve iki dosyanın kapanış sırasını C ile hizalıyor. 18 native vaka ile D151'in 24 kurtarma ve sekiz cihaz yeteneği vakası kapanış dahil eşit. D152 20/20, önceki 147 kurtarma vakası eşit. Gcov 32 çıkış, alınmayan 13 sınıflı. OFF 1147/7/337, ON 1133/21/337. DML ve ayrışma bulucu V6b.48 ile aynı. Rust 1.100 süre oranı 1,240 ve 14 digest eşit. D154 normal COMMIT SHARED, D155 COMMIT_PHASETWO ve D156 kayıt yazma sınırı açık. [Karar ve doğrulama](karar_esligi_v6b_49.md).

## V6b.50 — D154 normal rollback-journal COMMIT sonu kilit sırası

Normal disk COMMIT journal sonlandırma sonucunu saklayıp SHARED'a geçmeyi deniyor. İlk hata korunuyor ve okuyucusu bulunan bağlantı son okura kadar SHARED'da kalıyor. 24 gerçek kaynak native vakası ve ayrı 36 phase-two ile 18 açık-okur aday vakasında ölçülen terminal izleri, SQL ve DB özetleri eşit. D153 kapanış 18/18 ve önceki 147 kurtarma eşit. Gcov 14 çıkış ve alınmayan sekiz sınıflı. OFF 1147/7/337, ON 1133/21/337. DML ve ayrışma bulucu V6b.49 ile aynı. Rust 1.100 süre oranı 1,247 ve 14 digest eşit. D155 COMMIT_PHASETWO ve D156 kayıt yazım sınırı açık. [Karar ve doğrulama](karar_esligi_v6b_50.md).

## V6b.51 — D155 normal COMMIT phase-two bildirimi

Normal disk rollback-journal COMMIT artık journal sonlandırmasından sonra ve SHARED öncesinde `SQLITE_FCNTL_COMMIT_PHASETWO` gönderiyor. 24 gerçek kaynak vakasında terminal sıra ve DB özetleri eşit. Ayrı 60 callback ve hata vakasında ilk hata ile kalıcı veri korundu. Gcov 14 çıkış ve alınmayan sekiz sınıflı. Önceki 147 kurtarma vakası eşit. OFF 1148/7/337, ON 1134/21/337. DML ve ayrışma bulucu V6b.50 ile aynı. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. D156 kayıt yazım sınırı sırada. [Karar ve doğrulama](karar_esligi_v6b_51.md).

## V6b.52 — D156 disk rollback-journal kayıt yazımı

Ana journal sayfa numarasını, preimage sayfasını ve checksum'u C boyut ve ofsetleriyle ayrı yazıyor. Başarısız alan kaydı ilerletmiyor ve NEED_SYNC korunuyor. 12 sağlıklı ve 144 hata vakasında iki Rust kipi C ile eşit. Ortak nonce altında bütün journal görüntüleri, her koşuda ana DB özetleri eşit. Üç xWrite kararının altı C çıkışı alındı. Önceki 147 kurtarma eşit. OFF 1149/7/337, ON 1135/21/337. DML ve ayrışma bulucu V6b.51 ile aynı. Rust 1.100 süre oranı 1,246 ve 14 digest eşit. D157 cihaz bayrakları sırada. [Karar ve doğrulama](karar_esligi_v6b_52.md).

## V6b.53 — D157 rollback journal cihaz yetenekleri

SAFE_APPEND ve SEQUENTIAL, ana journal başlığını, sync sırasını ve spill bölüm sınırlarını C ile aynı koşullarda belirliyor. 32 cihaz, 32 FULLFSYNC ve dört spill vakası iki Rust kipinde eşit. Üç C kapsamı birlikte 28/40 dal çıkışını aldı. D156'nın 144 yazma hatası ve önceki 147 kurtarma vakası tekrar eşit. OFF 1150/7/337, ON 1136/21/337. DML ve ayrışma bulucu V6b.52 ile aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. D158 hizalı sonraki journal başlığının kısa okuması sırada. [Karar ve doğrulama](karar_esligi_v6b_53.md).

## V6b.54 — D158 journal bölüm başlığında kısa okuma

Sonraki journal başlığı taraması kısa bayt sayısı ve SHORT_READ ile C gibi ilerliyor. Diğer G/Ç hataları korunuyor. Doğal EOF 16/16, önceki journal ve hata 14/14, farklı magic 3/3 eşit. Üç C kapsamı birlikte 8/8 dal çıkışını aldı. Önceki 147 kurtarma vakası tekrar eşit. OFF 1151/7/337, ON 1137/21/337. DML ve ayrışma bulucu V6b.53 ile aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. D159 normal WAL COMMIT bildirimi sırada. [Karar ve doğrulama](karar_esligi_v6b_54.md).

## V6b.55 — D159 normal WAL COMMIT phase-two bildirimi

Normal disk WAL COMMIT yazıcıyı bıraktıktan sonra `SQLITE_FCNTL_COMMIT_PHASETWO` gönderiyor. Bildirim hatası C ile aynı sonucu döndürüyor ve kalıcı veri yeniden açılışta okunuyor. 16 WAL vakası ve 60 rollback regresyonu iki kipte eşit. 48 WAL görüntüsü checksum ve commit sayfası yönünden denetlendi. Önceki 147 kurtarma eşit. OFF 1152/7/337, ON 1138/21/337. DML ve ayrışma bulucu V6b.54 ile aynı. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. D160 EXCLUSIVE rollback bildirimi sırada. [Karar ve doğrulama](karar_esligi_v6b_55.md).

## V6b.56 — D160 EXCLUSIVE rollback COMMIT phase-two bildirimi

EXCLUSIVE disk rollback COMMIT gerçek yazımdan sonra phase-two bildirimi gönderiyor. Boş EXCLUSIVE PERSIST için C'nin erken dönüşü korunuyor. 56 vaka iki Rust kipinde SQL sonucu, bildirim sırası, yeniden açılış ve DB özetlerinde eşit. 120 journal görüntüsü denetlendi. On ham PERSIST farkı bağımsız nonce checksum alanlarıyla sınırlı. Önceki 147 kurtarma eşit. OFF 1153/7/337, ON 1139/21/337. DML ve ayrışma bulucu V6b.55 ile aynı. Rust 1.100 süre oranı 1,243 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_56.md).

## V6b.57 — D52 prepared INSERT RETURNING sayaçları

Prepared INSERT RETURNING üst sayaçları C gibi ifade kapanışında yayımlıyor. İlk satır ve araya giren DML, DONE, reset, finalize ve Drop doğrulandı. 27 bağımsız yaşam döngüsü, 72 geniş vaka, 24 aktif yazıcı ve iki commit hook karşılaştırması eşit. Önceki 147 kurtarma vakası korundu. OFF 1154/7/337, ON 1140/21/337. DML ve ayrışma bulucu V6b.56 ile aynı. Rust 1.100 süre oranı 1,240 ve 14 digest eşit. Sırada 2PC phase-one bildirimi ve aktif rollback. [Karar ve doğrulama](karar_esligi_v6b_57.md).

## V6b.58 — D162 iki dosyalı COMMIT ve aktif rollback

Phase-one SYNC, super-journal yolunu C ile aynı koşulda bildiriyor. Aktif rollback, commit noktasını fiziksel journal ile denetliyor ve katılımcı sonlandırması sonrası super-journal temizliğini C sırasına alıyor. 48 iki dosyalı vaka ve sekiz guard açılış vakası eşit. Tek dosya 27/27 veri ve yeniden açılışta eşit, tam iz 17/27. Gcov 55/76. Önceki 147 kurtarma eşit. OFF 1155/7/337, ON 1141/21/337. DML ve ayrışma bulucu V6b.57 ile aynı. Rust 1.100 süre oranı 1,245 ve 14 digest eşit. D163 tek dosya spill rollback sırası sırada. [Karar ve doğrulama](karar_esligi_v6b_58.md).

## V6b.59 — D163 spill rollback geri yazma sınırı

Aktif rollback sync açıkken yalnız tamamı senkronize ana journal kaydını DB'ye geri yazıyor. Önceki altı ek yazım kalktı ve cache geri yüklemesi korundu. 27/27 tek dosya vakası VFS, SQL ve ana DB özetlerinde eşit. Ham dosya kümesi 21/27 ve altı PERSIST farkı nonce/checksum alanlarında. D162'nin 56 vakası ile önceki 147 kurtarma vakası eşit. Gcov 11/18. OFF 1156/7/337, ON 1142/21/337. DML ve ayrışma bulucu V6b.58 ile aynı. Rust 1.100 süre oranı 1,250 ve 14 digest eşit. Sırada iç PRAGMA V1 ofseti var. [Karar ve doğrulama](karar_esligi_v6b_59.md).

## V6b.60 — D164 V1 iç PRAGMA hata ofseti

Otomatik `pragma_locking_mode` tanısı bilinmeyen şema ofsetini C gibi V1 step sırasında gösteriyor. V2, reset ve finalize ofsetleri ile doğrudan hazırlama tanıları korundu. Eski sekiz fark kapandı. 184 geniş native ve 16 odak vaka ile fiziksel dosya özetleri eşit. Önceki 147 kurtarma eşit. OFF 1157/7/337, ON 1143/21/337. DML ve ayrışma bulucu V6b.59 ile aynı. Rust 1.100 süre oranı 1,235 ve 14 digest eşit. Sırada prepared UPDATE/DELETE RETURNING sayaçları var. [Karar ve doğrulama](karar_esligi_v6b_60.md).

## V6b.61 — D165 prepared UPDATE ve DELETE RETURNING sayaçları

Legacy prepared UPDATE ve DELETE, ilk RETURNING satırında üst sayacı bekletiyor. Trigger katkısı görünür ve üst sayaç ifade kapanışında yayımlanıyor. Gerçek kaynak 36 UPDATE/DELETE, 27 INSERT ve 33 INSERT OR FAIL vakasında C ile eşit. Önceki 147 kurtarma eşit. OFF 1158/7/337, ON 1144/21/337. DML ve ayrışma bulucu V6b.60 ile aynı. Rust 1.100 süre oranı 1,240 ve 14 digest eşit. Sırada normal WAL kapanış VFS sözleşmesi var. [Karar ve doğrulama](karar_esligi_v6b_61.md).

## V6b.62 — D166 normal WAL kapanışında VFS kararları

Normal disk WAL kapanışı checkpoint'ten sonra VFS kalıcılık değerini sorguluyor. BUSY sonrasında terminal kilit bırakma sırası C ile aynı. 6 sağlıklı, 7 VFS yanıtı ve 3 eski adapter vakasında SQL, yeniden açılış ve DB/WAL/SHM özetleri eşit. C dal kapsamı 15/22. Önceki 147 kurtarma eşit. OFF 1159/7/337, ON 1145/21/337. DML ve ayrışma bulucu V6b.61 ile aynı. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. D167 kapanış öncesi dosya kimliği denetimi sırada. [Karar ve doğrulama](karar_esligi_v6b_62.md).

## V6b.63 — D167 WAL kapanışında dosya kimliği

Olağan disk WAL kapanışı checkpoint öncesinde VFS dosya kimliğini sorguluyor. Bağımsız 40/40 C karşılaştırmasında ortak olay sırası, üç dosya özeti ve C yeniden açılışı eşit. Odak Rust testleri 10/10 geçti. C kapsamı 55 kapanışta checkpoint'e 37 kez izin verildiğini ve 18 kez atlandığını gösteriyor. Önceki 147 kurtarma eşit. OFF 1160/7/337, ON 1146/21/337. DML ve ayrışma bulucu V6b.62 ile aynı. Rust 1.100 süre oranı 1,252 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_63.md).

## V6b.64 — F9 REGEXP hazırlama tanısı

SELECT hazırlamasındaki bulunamayan REGEXP ve yanlış arite hataları, C'nin özgün token yazımını ve UTF-8 bayt ofsetini koruyor. 54 hazırlama, 72 kayıtlı fonksiyon ve 6 stored-view vakası iki Rust kipinde eşit. C resolver kapsamı 10/16. Group54 iki kipte geçiyor. OFF 1161/7/337, ON 1148/20/337. DML ve ayrışma bulucu V6b.63 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,232 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_64.md).

## V6b.65 — REGEXP callback değeri ve bellek PRAGMA testi

Pozitif infix REGEXP 192/192 vakada callback değerini ve türünü C gibi döndürüyor. Tam matris OFF 560/576, ON 576/576. Sayısal BLOB'un 16 eski OFF koşulu açık. Bellek içi `mmap_size` için sekiz bağımsız karşılaştırma, sıfır satır ve korunan metadata beklentisini doğruladı. OFF 1163/6/337, ON 1150/19/337. DML ve ayrışma bulucu V6b.64 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,227 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_65.md).

## V6b.66 — Örtük kapanış rollback hook'u ve boş WAL başlığı

Son bağlantı kapanışı yazma veya kapalı autocommit koşulunda rollback callback'ini çağırıyor. Boş WAL, C gibi sıfır sayfa boyutlu SHM başlığını yayımlıyor. Birleşik adayda 10 kapanış, 90 yeniden açılış ve 28 guard sonuçları eşit. Üç gerçek sayfa boyutunda 12 ilk SHM karşılaştırması birebir. Sonraki TRUNCATE'teki iki eski SHM hücresi farkı açık. OFF 1165/6/337, ON 1152/19/337. DML ve ayrışma bulucu V6b.65 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,236 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_66.md).

## V6b.67 — Sayısal BLOB koşulları

Legacy SQL, sayısal BLOB koşulunu veritabanı kodlamasına göre değerlendiriyor. İki Rust kipinde 2160 koşul ve 576 REGEXP vakası C ile eşit. Önceki 16 REGEXP koşul farkı kapandı. Ayrı CHECK ve `IS TRUE/FALSE` kapsamları açık. OFF 1166/6/337, ON 1153/19/337. DML ve ayrışma bulucu V6b.66 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_67.md).

## V6b.68 — WAL restart ve eski indeks hücreleri

WAL restart başlık ve readmark'ı C gibi yayımlıyor. Her indeks bölgesi eski slotlarını kendi ilk append'inde temizliyor. Olağan TRUNCATE ve doğal writer restart 12/12 iki kipte eşit, 16 eski SHM farkı kapandı. OFF 1167/6/337, ON 1154/19/337. DML ve ayrışma bulucu V6b.67 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,232 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_68.md).

## V6b.69 — CHECK için kayıt ve varsayılan BLOB kodlaması

Legacy CHECK, yazma ve doğrudan fiziksel BLOB sütununda veritabanı kodlamasını kullanıyor. Integrity denetiminde kısa fiziksel kaydın literal BLOB varsayılanı UTF-8 olarak değerlendiriliyor. Üç kodlama, her motorda 75 adım ve yedi kısa kayıt C ile eşit. Önceki 147 kurtarma eşit. OFF 1169/6/337, ON 1156/19/337. DML ve ayrışma bulucu V6b.68 ile aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_69.md).

## V6b.70 — Özel geçici DB dosyasının açılış zamanı

C iki özel TEMP DB'de boş okuma, işlem başlangıcı ve küçük COMMIT boyunca dosyayı açmadı. Her birinin ilk cache spill'i tek TEMP_DB xOpen yaptı ve veri izolasyonu korundu. Rust testinin eski dosya açma beklentisi ölçüme göre güncellendi. OFF 1170/5/337, ON 1157/18/337. DML ve ayrışma bulucu V6b.69 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,234 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_70.md).

## V6b.71 — Skaler fonksiyon çağrısında yıldız

Legacy skaler doğrulama yıldızla gelen fonksiyon çağrısını sıfır argümanla çözüyor. 468 native vakada DB özetleri iki kipte eşit. Tam API eşliği OFF 384, ON 408 vaka. Group46/65 C beklentisiyle iki kipte geçti. OFF 1170/5/337, ON 1158/17/337. DML ve ayrışma bulucu V6b.70 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,241 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_71.md).

## V6b.72 — Kesinti yaşamı, token ve sanal tablo sırası

Bağlantı boşken başlayan varsayılan kesinti sonraki hazırlama/step'te temizleniyor. Etkin ifade, V1/V2 hata dönüşü ve prepare token sırası C ile hizalandı. 566 native yeniden koşu, 276 lexical ve 12 vtab örneğini içeriyor. Eski V1 hata metni açık. OFF 1176/4/337, ON 1164/16/337. DML ve ayrışma bulucu V6b.71 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_72.md).


## V6b.73 — CHECK hazırlama ayarı ve TEMP tanısı

Legacy yazma ve bütünlük denetimi, C gibi CHECK bayrağını hazırlamada alıyor. Yalnızca hazırlanan setter önceki ifadeyi süresi dolmuş duruma getirmiyor. İki yeni CHECK testi iki kipte geçti. DEBUG C ile karşılaştırılan 18 TEMP vakasında tanı satırları ve kalıcı DB özetleri iki kipte eşit. OFF 1179/3/337, ON 1167/15/337. DML ve ayrışma bulucu V6b.72 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_73.md).


## V6b.74 — E28 Pager ERROR sözleşmesi

F14 ve F15 testleri C'nin kayıtlı genişletilmiş hata kodlarını, aynı bağlantıdan tekrar okuma ve yazılabilir yeniden açılışta kalıcı değeri denetliyor. Üretim kaynakları aynı. OFF 1181/1/337, ON 1169/13/337. DML ve ayrışma bulucu V6b.73 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_74.md).


## V6b.75 — D50 bağlama boyutu ve SQL source-id

Varsayılan TEXT/BLOB bağlama sınırı C gibi uygulanıyor. Bind rc18 ve NULL parametre bırakıyor. Aynı Tcl testi ve SQL source-id iki kipte C ile aynı. 24 küçük sınır dosyası, 18 source-id yazma ve C yeniden açılışı eş. OFF 1181/1/337, ON 1170/12/337. DML ve ayrışma bulucu V6b.74 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_75.md).


## V6b.76 — CTE aynı FROM ve scalar UPDATE ömrü

Aynı lexical CTE'nin iki alias'ı ölçülen FROM şekillerinde C gibi tek sonucu kullanıyor. Korelasyonsuz scalar UPDATE de statement başına bir kez hesaplanıyor. 153 disk grubu ve C ile açılan 1.530 DB. Önceki 93 grupta iki kipte tam fiziksel eşlik ve iki odak başarılı. OFF 1181/1/337, ON 1171/11/337. DML ve ayrışma bulucu V6b.75 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_76.md).


## V6b.77 — Skaler hata ofseti ve view UPSERT tanısı

Skaler SELECT legacy hazırlamasında C ile aynı tanının eksik bayt ofseti taşınıyor. View UPSERT yazılabilirlik ve kaynak denetimini C sırasıyla bildiriyor. İki kip 468 skaler, 234 view ve altı view yeniden hazırlama vakasında C eş. OFF 1181/1/337, ON 1172/10/337. DML ve ayrışma bulucu V6b.76 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,240 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_77.md).


## V6b.78 — Üretilen sütun kullanım maskesi ve örtücü indeks

Legacy planlayıcı üretilen sütunun C kullanım maskesini ve örtücü ifade kararını izliyor. REPLACE SELECT C ile aynı son satırları ve DB baytlarını yazıyor. 415 dosya grubunda iki kipte C yeniden açılışı ve SHA eş. OFF 1181/1/337, ON 1172/10/337. DML OFF 382/400 ve ON 400/400. Ayrışma bulucu V6b.77 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_78.md).


## V6b.79 — TRUE/FALSE adları ve IS truth testi

C'nin ad çözümü ve truth testi ölçülen SELECT ve DML kapsamlarında hizalandı. 480 değer, 72 DML ve 648 yeniden açılış iki kipte C eş. Dört yeni test iki kipte geçti. OFF 1185/1/337, ON 1176/10/337. DML ve ayrışma bulucu V6b.78 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,247 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_79.md).


## V6b.80 — V1 bağlantı tanısı ve gerçek reset kodu

Varsayılan okuma kesintisinde V1 step, bağlantıda saklanan mesaj ve ofsetle rc1 veriyor. Reset ve finalize gerçek rc9'u koruyor. 324 native vaka iki kipte C eş. Yeni V1 regresyonu iki kipte geçti. OFF 1186/1/337, ON 1177/10/337. DML ve ayrışma bulucu V6b.79 ile aynı. Önceki 147 kurtarma eşit. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_80.md).


## V6b.81 — WITHOUT ROWID STORED tam tarama

Dar aday, ölçülen STORED WITHOUT ROWID taramasında C'nin eş maliyet sırasını izliyor. 712 fiziksel dosya grubu ve 48 INSERT/REPLACE ölçümü iki kipte C eş. Önceki geniş adayın yeni sıra farkı dar kapsamda kapandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 382/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 0,992 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_81.md).


## V6b.82 — DEFAULT VALUES ve tek satırlı FOREIGN KEY

Default değerlerle tek satır yazan INSERT, C'nin immediate FOREIGN KEY kararıyla uyumlu. 216 fiziksel karşılaştırma her kipte eş. 36 işlem kapanışında hatalı kayıt kalıcı dosyaya geçmiyor. OFF DML bir vaka daha eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 383/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_82.md).


## V6b.83 — Sanal tablo SELECT hazırlığı

UTF-8 ON tek sanal tablo SELECT hazırlığında program yeniden kullanılıyor. Çağrı sayısı değişen 32 akışın tamamında callback sırası ve argümanları C ile eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 383/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_83.md).


## V6b.84 — Sanal tablo OR ve IN hazırlığı

Seçilen UTF-8 ON OR ve IN programlarında hazırlama callback tekrarı kalktı. 1.080 odak dosyası ve 792 önceki kapsam dosyası C ile salt okunur açıldı. OFF 1186/1/337, ON 1177/10/337. DML OFF 383/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_84.md).


## V6b.85 — STRICT ve NOT NULL kısıt sırası

Yazma kısıtlarının C sırası ve NOT NULL REPLACE sonrası generated hesaplama uygulandı. Fiziksel dosya ve C açılışı her API ve kipte 162/162 eş. OFF DML dört vaka iyileşti. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_85.md).


## V6b.86 — INDEXED BY ve kalıcı şema indeksleri

V1/V2 ve OFF/ON dört grubun her birinde 93/93 ham ve fiziksel C eşliği. Yazma sırası, reset ve yeniden açılış ölçüldü. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_86.md).


## V6b.87 — TRUE ve FALSE adlı gerçek kaynak sütunları

Bilinen tek normal tabloda gerçek sütun önceliği C ile hizalandı. 114 hedef akış ve 24 yazma dosyası iki kipte C ile eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_87.md).


## V6b.88 — WHERE TRUE/FALSE sonuç takma adları

Tek normal tabloda gerçek sütun bulunmadığında C'nin uygun sonuç takma adı seçimi uygulanıyor. 108 hedef ve 1.545 fiziksel dosyada C kanıtı doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_88.md).
