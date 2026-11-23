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


## V6b.89 — ORDER BY TRUE/FALSE sonuç konumu

C ilk sonuç konumunu izliyor, yıldız açılımı ve explicit alias sırası ölçüldü. 108 hedef ham ve 780 exact odak C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_89.md).


## V6b.90 — Normal SELECT sütun adları

Sütun adı bayrakları ve reprepare metadata'sı C ile hizalandı. 168 ham normal ve 30 yaşam metadata akışı iki kipte C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_90.md).


## V6b.91 — ORDER BY +TRUE ve +FALSE

Unary artı altındaki ad çözümlemesi ve kolasyon C ile hizalandı. 132 hedef ve 48 ad bayrağı ham eş, 4.500 fiziksel C açılışı doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_91.md).


## V6b.92 — EXCLUSIVE DELETE→WAL journal sonlandırması

EXCLUSIVE geçişinde rollback journalı commit sonunda C ile aynı sınırda kaldırıldı. 240 sağlıklı geçiş ve 54 normal RESTART guard doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_92.md).


## V6b.93 — EXCLUSIVE PERSIST ve TRUNCATE→WAL sonlandırması

Geçiş commit'i rollback journalı C ile aynı sınırda kaldırıyor. 240 sağlıklı geçiş, 120 DELETE/WAL koruması ve C yeniden açılışları eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 0,844 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_93.md).


## V6b.94 — EXCLUSIVE MEMORY ve OFF→WAL sonlandırması

Başarılı geçiş commit'i rollback journal ömrünü C ile hizalıyor. 240 sağlıklı geçiş ve 240 önceki kip koruması eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_94.md).


## V6b.95 — EXCLUSIVE WAL→MEMORY/OFF sonlandırması

WAL çıkışında geçici DELETE journal commit sonunda kaldırılıyor. 480 ters geçiş, 360 önceki kip koruması ve 1.440 C/Rust açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 387/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_95.md).


## V6b.96 — Görünüm DELETE INSTEAD OF trigger sırası

Basit sayısal aralık C görünüm materialization sırasıyla hizalandı. OFF DML bir vaka daha eş ve ana tablo içerikleri korundu. OFF 1186/1/337, ON 1177/10/337. DML OFF 388/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_96.md).


## V6b.97 — Görünüm UPDATE INSTEAD OF trigger sırası

Görünüm aralık denetimi ortaktır, ana tablo ve trigger log dosyaları C ile hizalandı. 288 UPDATE ve 282 önceki DELETE grubu doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 388/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_97.md).


## V6b.98 — Yerleşik MIN/MAX ve veritabanı kodlaması

Yerleşik biriktirici UTF-16 BINARY metin sıralamasını C ile hizalıyor. V1/V2 ve her kipte 87/87 ham ve fiziksel eşlik doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 388/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_98.md).


## V6b.99 — Görünüm UPDATE scalar MAX eşitliği

C'nin geçici tarama ve özgün filtre için bağımsız MAX değerlendirme ömrü uygulandı. Ana tablo, yerleşik kodlama, trigger sırası ve iki bağımsız cache sınırı ölçüldü. OFF 1186/1/337, ON 1177/10/337. DML OFF 388/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_99.md).


## V6b.100 — Görünüm MAX eşitliği ve ek indeksler

Tek ilgili bileşik anahtar ve ek bağımsız indeksle C trigger sırası izleniyor. OFF DML bir vaka daha eş, gerçek plan ve cache kapsamı ayrık doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 389/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_100.md).


## V6b.101 — Bağımsız EXISTS covering taraması

Dış covering taramada bağımsız EXISTS kaynak sırası C eş. OFF DML bir vaka daha iyileşti, ham sayaç ve DB dosyası doğrulandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_101.md).


## V6b.102 — Nitelikli korelasyonlu EXISTS sırası

C dış covering taraması, kanıtlanan dış sütun bağımlılığı için uygulanıyor. Kalıcı self-REPLACE çıktısı ve son rowid C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_102.md).


## V6b.103 — WITHOUT ROWID otomatik UNIQUE ve PK eki

Yalın SELECT ve INSERT SELECT planı C maskesine göre otomatik UNIQUE kapsamasını seçiyor. Her API ve kipte 165/165 kalıcı dosya ve C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,246 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_103.md).


## V6b.104 — Zorlanmış indeks ve literal IN sıra anahtarları

Mevcut indeks aramaları, affinity uygulanmış anahtarları C yönünde dolaşıyor. Her API ve kipte 177/177 dosya, kalıcı satır ve C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_104.md).


## V6b.105 — Zorlanmış indeks ve ORDER BY yönü

Filtrelenmemiş tek kaynakta indeksin sağladığı doğal ORDER BY yönü korunuyor. Her API ve kipte 195/195 dosya, kalıcı satır ve C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_105.md).


## V6b.106 — Nitelemesiz WAL checkpoint ve bağlı şemalar

PRAGMA bütün açık şemaları işliyor, busy sonuçları birleşiyor ve sayaçlar MAIN sonucunu izliyor. 756 normal ATTACH ve 72 iki-WAL okur akışı C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_106.md).


## V6b.107 — Zorlanmış sırasız indeks ve reverse PRAGMA

Bağlantı ayarı, filtresiz ve sırasız zorlanmış ikincil indeksin tam tarama yönünü belirliyor. Her API ve kipte 198/198 dosya, kalıcı satır ve C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_107.md).


## V6b.108 — WITHOUT ROWID görünüm UPDATE aralığı

Tek ikincil indeksli sayısal literal aralık C materyalizasyon sırasını izliyor. Her API ve kipte 78/78 kalıcı dosya ve C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_108.md).


## V6b.109 — İlk ATTACH ana şema kodlaması

Ana şema ve kayıtlı kodlama C gibi ATTACH hazırlığında yükleniyor. 48 CLI, 96 native ve 12 EXPLAIN karşılaştırması C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,252 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_109.md).


## V6b.110 — Rowid görünüm UPDATE alt ve üst sınırı

Tek ikincil indeksin iki sayısal literal sınırı C gibi kaynak materyalizasyonuna katılıyor. 36/36 log ve dosya, 24/24 tek sınır koruması C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,243 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_110.md).


## V6b.111 — WITHOUT ROWID görünüm UPDATE iki sınırı

İkinci sayısal sınır da mevcut seçilmiş aralık yürütmesine katılıyor. 72/72 log/dosya ve 60 önceki koruma C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,250 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_111.md).


## V6b.112 — Foreign key PRAGMA id ve sıra

Metadata son bildirilen kısıttan başlayan C sıra numarasını kullanıyor. 72 çıktı farkı kapandı, 27 ek salt okunur kontrol C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_112.md).


## V6b.113 — Tek hedefli journal modu ilk ROW ve commit

Journal geçişi ilk ROW sırasında eski başlığı ve aktif journalı koruyor. 180 native, 48 sahiplik ve 12 doğrudan Pager/Btree kontrolü C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_113.md).


## V6b.114 — İndeks PRAGMA WITHOUT ROWID tablo adı

Gerçek indeks araması sonrasında WITHOUT ROWID tablo birincil indeksi C gibi bulunuyor. 456 metadata bölümü düzeldi, 36 fiziksel dosya eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_114.md).


## V6b.115 — index_xinfo açık collation yazımı

Normal açık built-in COLLATE yazımı C gibi gösteriliyor. 336 metadata farkı kapandı ve 54 dosya byte eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,250 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_115.md).


## V6b.116 — Çoklu hedef journal modu ve ortak commit

İki veya üç sağlıklı DELETE/WAL hedefi nitelemesiz PRAGMA ile ortak sonlandırılıyor. 108 multi, 180 tek/named ve 72 sahiplik C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_116.md).


## V6b.117 — İndeks metadata aynı collation zinciri

Doğrudan sütundaki aynı yerleşik collation zincirinin dış yazımı C eş. 192 metadata farkı kapandı, 72 fiziksel dosya byte eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_117.md).


## V6b.118 — Tek hedefli PERSIST/TRUNCATE WAL yaşamı

İlk ROW eski journal inode ve biçimini tutuyor, normal bitiş WAL'ı tamamlıyor. 288 retained ve 288 önceki DELETE/WAL akışı C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_118.md).


## V6b.119 — Çoklu retained journal WAL giriş sırası

İki veya üç normal disk hedefinde DELETE/PERSIST/TRUNCATE hazırlıkları ortak sonlandırılıyor. 180 yeni akış C eş, önceki sekiz PERSIST fiziksel sıra ayrımı kapandı. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_119.md).


## V6b.120 — Açık SELECT sırası ve fiziksel indeks kolasyonu

Doğrulanan tek kaynakta planner ve fiziksel indeks farklı collation kullanırsa mevcut ilişkisel sıralama çalışıyor. Dört mantıksal sıra farkı kapandı, altı dosya SHA sabit. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_120.md).


## V6b.121 — Çoklu journal ve desteklenen disk slotları

Mevcut ortak commit iki ve üçten fazla normal diske uygulanıyor. 180 dört/altı, 6 üst sınır ve 180 eski native kontrol C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_121.md).


## V6b.122 — İpucusuz SELECT ve seçilen indeks sırası

Tek gerçek OpenRead ve KeyInfo ile seçilen fiziksel indeks güvenli sıralama için doğrulanıyor. Dört yeni ve dört adlandırılmış sıra farkı C eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_122.md).


## V6b.123 — Karma journal hedefi ve aynı kip slotları

Yalnız gerçek geçiş yapan slotlar ortak statement sahipliğine katılıyor. 234 yeni ve 366 eski kontrol C eş, 522 aynı kip slotu fiziksel olarak korunuyor. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_123.md).


## V6b.124 — Tek hedefli WAL→PERSIST/TRUNCATE yaşamı

WAL checkpoint ve ilk ROW aktif journal sınırını C gibi izliyor. 288 yeni akış, 468 önceki kontrol ve 1.944 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_124.md).


## V6b.125 — Çoklu WAL→PERSIST/TRUNCATE ve ortak journal

Koordinatör retained hedeflerde C ile aynı katılımcıları ve bitiş sırasını kullanıyor. 216 yeni ve 414 önceki kontrol, 4.662 salt okunur C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_125.md).


## V6b.126 — Karma rollback kipli disklerde WAL çıkışı

Yalnız WAL sınırını geçen diskler ortak statement bitişine katılıyor. 216 yeni, 396 koruma ve 6.048 salt okunur C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_126.md).


## V6b.127 — Karma indeks kolasyonunun INDEX_XINFO gösterimi

Basit doğrudan anahtarda dış yerleşik kolasyonun SQL yazımı korunuyor. Her kipte 254 metadata ölçümü C ile eş ve 91 veritabanı görüntüsü korunuyor. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 0,937 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_127.md).


## V6b.128 — Açık TEMP ile WAL geçişi ve journal kipi

Yalnız gerçek NamedDisk geçişleri ortak bitişe katılıyor ve TEMP FILE PERSIST/TRUNCATE kipini kullanabiliyor. 1.026 normal akış ve 5.508 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_128.md).


## V6b.129 — Karma indekslerde güvenli salt okunur erişim

Özgün SQL ve bağlamalar korunarak ana tabloya yönleniyor. Her kipte 324/324 C tablo referansı eş ve V127 metadata korunuyor. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_129.md).


## V6b.130 — Bağlantı kapanışında WAL disk sırası

Normal, Drop ve son statement ile tamamlanan kapanışta Btree slotları C sırasında kapanıyor. 846 akış ve 2.376 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_130.md).


## V6b.131 — TEMP FILE TRUNCATE senkronizasyonu

Geçici pager normal COMMIT ve ROLLBACK sonrasında journalı sıfıra indiriyor ve C noSync kararını izliyor. 144 normal akış ve 48 retained journal C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_131.md).


## V6b.132 — Karma indeks bütünlük tanısı

Dar integrity_check kapsamı C dış kolasyonu ve aynı cursor ile üyeliği arıyor. 114 dosyada her kipte 1.710 C grubu eş ve quick_check korunuyor. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_132.md).


## V6b.133 — WAL→MEMORY statement bitişi

Normal WAL çıkışında gerçek MEMORY journal ve C ROW sınırı kullanılıyor. 756 geçiş, 216 retained kontrolü ve 6.318 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_133.md).


## V6b.134 — Karma indeksli noktasal DML fiziksel düzeni

Yazma görüntüsünde gerçek karşılaştırıcı seçiliyor ve önceki nullable kayıtlar korunuyor. 192 rollback-journal ve 72 WAL C/Rust tam dosya çifti eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,280 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_134.md).


## V6b.135 — Karma indeksli noktasal DML kolon yerleşimi

İndeks anahtarı ve rowid konumunda normal kayıt çözümlemesi kullanılıyor. 240 rollback-journal ve 108 WAL C/Rust tam dosya çifti eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,236 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_135.md).


## V6b.136 — MEMORY→WAL statement bitişi

Normal geçişte ilk ROW C gibi fiziksel başlığı koruyor. 756 yeni, 270 koruma ve 7.128 salt okunur C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 0,965 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_136.md).


## V6b.137 — İzinli WAL→OFF statement bitişi

Normal izinli geçişte ilk ROW fiziksel başlığı koruyor ve Defensive kipi mevcut davranışını izliyor. 1.512 akış ve 10.530 salt okunur C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_137.md).


## V6b.138 — Mevcut izinli OFF kipinden WAL geçişi

Bekleyen statement önceki ve hedef kipi ayırarak C ROW ve bitiş sınırını kullanıyor. 1.782 normal ve koruma akışı ile 12.150 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_138.md).


## V6b.139 — Karma indeksli rowid aralık DML

Normal ileri rowid aralığı fiziksel karşılaştırıcıyı aynı aktif yazma görüntüsünden seçiyor. 108 C/Rust tam dosya çifti, 216 rollback ve 300 bağlı sınır eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_139.md).


## V6b.140 — Karma indeksli rowid aralık DML ters sırası

Hazırlanmış yönle ters aralık C Seek ve Prev davranışını kullanıyor. Her yönde 216 yazıcı ve 108 C/Rust tam dosya çifti eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_140.md).


## V6b.141 — Tek hedefli SQL WAL çıkışında PERSIST_WAL

WAL kapanışı VFS kararını ve boyut sınırını C gibi uyguluyor. 1.296 SQL geçişi, 72 bağlantı kapanışı ve 5.130 C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_141.md).


## V6b.142 — Karma indeksli WHERE’siz UPDATE

Normal tablo taraması aynı yazma görüntüsünde fiziksel sıra ve C ileri/ters cursor davranışını kullanıyor. 432 yazıcı, 216 C/Rust tam dosya çifti ve 3.024 salt okunur açılış eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,381 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_142.md).


## V6b.143 — Journalsız SQL geçişinde VFS bitişi

Sayfa 1 sync sonrasında C COMMIT_PHASETWO ve SHARED sırası kullanılıyor. 468 bitiş ve 1.971 salt okunur C açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_143.md).


## V6b.144 — Yabancı anahtar oldmask hazırlama tanısı

Uygun parent UPDATE hazırlığında değişmeyen gelen FK de C oldmask kararına katılıyor. 18 yeni OFF hazırlama farkı kapandı ve 135 dosya SHA korundu. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_144.md).


## V6b.145 — Kalıcı WAL yeniden açılırken SHARED kilidi

Tek hedefli normal D/P/T SQL çıkışı C gibi WAL yeniden açılmadan önce SHARED kilidine iner. 1.296 normal ve 216 canlı C peer akışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,290 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_145.md).


## V6b.146 — İfade indeksi INDEX_XINFO kolasyon adı

C'nin ifade kökündeki açık kolasyon adı doğrudan ve tablo değerli metadata için korunuyor. 18 eski fark kapandı ve her kipte 726 yeni grup eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_146.md).


## V6b.147 — Tek tırnaklı COLLATE adı

C'nin COLLATE adı kabul ettiği dört konumda tek tırnaklı diziler mevcut ad akışından geçer. 12 C dosyası iki kipte açıldı, 84/84 SELECT ve 11 parser kontrolü eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_147.md).


## V6b.148 — Miras INDEX_XINFO kolasyon yazımı

Doğrudan anahtar kolonunda C gibi bildirimdeki kolasyon yazımı korunur. 15 eski dosyada 24 metadata farkı iki kipte kapandı ve iki yeni kapsam eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,274 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_148.md).


## V6b.149 — WAL MEMORY çıkışında PERSIST_WAL kararı

Tek hedefte sağlıklı WAL çıkışının ilk ROW ve retained yan dosya durumu C ile hizalanır. 708 doğrudan, 738 koruma ve 132 canlı C peer akışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_149.md).


## V6b.150 — Karma indeksli doğrudan rowid IN yazımı

Uygun UPDATE ve DELETE hazırlanan rowid IN yönüyle mevcut fiziksel karşılaştırıcıyı korur. 144 normal yazıcı ve rollback, 432 salt okunur açılış ve 4.608 satır ziyareti eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_150.md).


## V6b.151 — MEMORY kapanışında terminal NO_LOCK

Açık WAL veya disk journal olmadan NORMAL NamedDisk kapanışı C gibi terminal NO_LOCK verir. 378 MEMORY ve 648 normal koruma akışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_151.md).


## V6b.152 — Karma indeksli sıralı INSERT SELECT

Açık hedef sütunlarıyla ayrı MAIN tablosundan rowid sıralı normal ekleme mevcut fiziksel düzeni korur. 144 yazıcı rollback ve 432 C/OFF/ON açılışı eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_152.md).


## V6b.153 — OFF kapanışında terminal NO_LOCK

İzinli OFF NamedDisk normal kapanışında C terminal NO_LOCK sırası korunur. 180 OFF, 1.026 koruma ve 4.752 salt okunur açılış eş. OFF 1186/1/337, ON 1177/10/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_153.md).


## V6b.154 — Okuyucunun çağıran kesinti bayrağı

Mapped SELECT ve yorumlanan okuyucu aynı özel AtomicBool kesintisini uygular. Group46, yedi interrupt yaşamı ve iki progress/pencere testi iki kipte eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_154.md).


## V6b.155 — Normal OFF ve MEMORY commit phase-two

Normal OFF ve MEMORY disk commit'i C gibi COMMIT_PHASETWO bildirir ve SHARED'a iner. 612 akış, 948 callback çifti ve 3.168 C açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_155.md).


## V6b.156 — Karma indeksli doğrudan sütun eşitliği

Uygun UPDATE ve DELETE doğrudan indekslenmeyen sütun eşitliğinde aktif fiziksel sırayı korur. 144 yazıcı, 432 rollback ve 9.072 C/Rust satır ziyareti eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_156.md).


## V6b.157 — V1 journal_mode kilit ve yeniden adım

V1 dış SQLITE_ERROR sonucuyla dahili BUSY veya SCHEMA kodunu C sırasıyla taşır. 72 çekişme ve 24 sağlıklı pragma grubunda 192 C/Rust native ve DB çifti eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_157.md).


## V6b.158 — Retained WAL takip yazısında rollback başlığı

Fiziksel WAL ve mantıksal rollback kipinde sayfa 1 biçimi korunur. 96 retained akış, 64 eski format farkı ve 1.008 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,225 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_158.md).


## V6b.159 — WAL OFF PERSIST_WAL kararı

İzinli sağlıklı tek hedef WAL OFF geçişi C PERSIST_WAL kararını uygular. 1.620 akış ve 5.712 C açılışı eş, önceki 60 ilk ROW bildirimi kapandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_159.md).


## V6b.160 — Karma indeks anahtar eşitliği

SQL, C deklarasyonu ve aktif fiziksel sıra uyumunda indeks araması çalışır. 144 yazıcı, 432 rollback ve 7.848 mutasyon ziyareti eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_160.md).


## V6b.161 — Çoklu DELETE ve WAL geçişinde BUSY sonrası kip

Önceden biten slotların hedef kipi gerçek okumaya kadar korunur. 228 doğal çekişme akışı, 456 ana DB ve 684 salt okunur C açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,227 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_161.md).


## V6b.162 — Salt okunur WAL terminal NO_LOCK

Normal salt okunur WAL kapanışında terminal NO_LOCK ana dosya kapanışından önce gelir. 432 salt okunur akışta 288 terminal çağrı ve toplam 960 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_162.md).


## V6b.163 — Karma indeksli açık WHERE 1 DELETE

Açık WHERE 1 DELETE, uygun normal deyimde hazırlanmış yönü ve mevcut fiziksel karşılaştırıcıyı kullanır. 342 yazıcı/rollback ve 1.026 C yeniden açılış eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_163.md).


## V6b.164 — Koordineli PERSIST TRUNCATE ve MEMORY BUSY sonu

Erken başlık okumasından önce C gibi hedef kip ve fiziksel geri alma korunur. 576 akışta 1.152 son ana dosya çifti ve 1.728 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,246 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_164.md).


## V6b.165 — Karma indeksli rowid aralığı ve sütun filtresi

Uygun AND bileşiminde C rowid aralığı ve mevcut fiziksel karşılaştırıcı korunur. 144 yazma, 288 rollback ile 432 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,098 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_165.md).


## V6b.166 — Koordineli OFF ve WAL BUSY sonu

Mevcut izinli native OFF ve WAL geçişi C gibi ara kip, fiziksel geri alma ve normal yeniden okuma verir. 264 native akış, 528 ana dosya slot çifti ve 792 C açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_166.md).


## V6b.167 — NO_CKPT_ON_CLOSE bağlantı ayarı

Bağlantı ve CLI aynı C uyumlu kapanış checkpoint ayarını kullanır. 702 doğal akış, 819 pager sınırı ve 846 C açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,250 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_167.md).


## V6b.168 — Karma ve olağan iki indeksin yazma ve bütünlüğü

Etkin aynı işlem görüntüsünde iki ayrı kök doğrulanıp mevcut fiziksel kurallarla yazılır. Rust 1.100 ile 288 yazıcı, 576 rollback ve 864 açılış eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_168.md).


## V6b.169 — Salt okunur journal_mode V1 dış tanısı

V1 READONLY dış step için genel C tanısını, reset ve finalize için asıl kodu taşır. 24 native grup ve 72 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_169.md).


## V6b.170 — İşlem içi journal_mode V1 dış tanısı

V1 işlem içi journal_mode dış step için genel C tanısını, reset ve finalize için asıl kodu taşır. 24 native grup, 24 V2 kontrolü ve 72 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_170.md).


## V6b.171 — Salt okunur journal_mode sorgusu hazırlığı

Soğuk salt okunur NamedDisk sorgusu prepare sırasında mevcut şema yükleme yolunu kullanır. 148 profil, 444 akış ve 531 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,247 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_171.md).


## V6b.172 — İkinci indeksli eşitlik yazıları

Etkin yazma görüntüsünde doğrulanan ikinci kök, kolasyon uygunluğuna göre normal eşitlik erişimini seçer. 576 native yazıcı, 1.152 rollback, 1.728 açılış ve ayrı 144 çok sayfalı yazıcı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_172.md).


## V6b.173 — Salt okunur WAL çıkışının native ve legacy sonucu

Stock Unix gerçek VFS yolu C kilit sırası ve V2 kodlarıyla eş. V1 ilk step genel tanıyı ve reset asıl kilit kodunu taşır. 714 native C açılışı ile 144 ek V1/V2 akışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_173.md).


## V6b.174 — Eşitlikle aranan ikinci indeks anahtarının güncellenmesi

Uyumlu aramada adaylar toplanır ve artan rowid sırasıyla bir kez yazılır. 576 native, 1.152 rollback, 1.728 açılış ve önceki 20 fiziksel fark doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_174.md).


## V6b.175 — Salt okunur journal_mode setter hazırlığı

Soğuk salt okunur NamedDisk setter, EXPLAIN ve reprepare mevcut şema yolunu C zamanında kullanır. 534 akış, 216 hazırlık sınırı ve 696 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_175.md).


## V6b.176 — İki kökte karma anahtar eşitliği

Uygun eşitlik aktif yazma görüntüsünde karma kökü seçer, diğer durumda tablo taraması korunur. 576 native ve 576 takip yazıcısı, 144 çok sayfalı akış ve eski bütünlük farkının kapanışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_176.md).


## V6b.177 — Tek UPDATE ile iki indeks anahtarı

İki kök aynı etkin yazma görüntüsünde doğrulanır, adaylar C sırasıyla bir kere yazılır. 768 native, 768 takip yazıcısı, 1.536 rollback ve 2.304 açılış doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,284 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_177.md).


## V6b.178 — V1 statement bağlantı tanısı

Çalışmadan expire olan V1 deyimi dönüşünü verir, C uyumlu bağlantı tanısı korunur. 266 C–Tcl çifti, 230 Rust Result koruması ve 845 C açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_178.md).


## V6b.179 — İki kökte sıralı INSERT SELECT

Ayrı MAIN kaynağı rowid sırasıyla okunur ve iki hedef kök aynı yazma görüntüsünde doğrulanır. 396 native, 396 takip yazıcısı, 792 rollback ve 1.188 açılış doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_179.md).


## V6b.180 — Yazılabilir journal_mode sorgusunun hazırlığı

Soğuk yazılabilir ana NamedDisk üzerinde değersiz sorgu ve EXPLAIN şemayı C ile aynı prepare sınırında açar. 672 fiziksel çift, 640 VFS sıra ve 24 doğal BUSY profili doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,230 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_180.md).


## V6b.181 — İki kökte rowid IN yazımı

Düz rowid/IPK listesi uygun iki kökün etkin yazma görüntüsü doğrulamasından geçer. 396 native ve 396 takip yazıcısı, 792 rollback, 1.188 açılış doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_181.md).


## V6b.182 — Yazılabilir journal_mode setter hazırlığı

Soğuk yazılabilir NamedDisk setter ve EXPLAIN şemayı C ile aynı prepare aşamasında hazırlar. 940 fiziksel hazırlık çifti ve 48 doğal prepare çekişmesi profili doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,332 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_182.md).


## V6b.183 — İki kökte rowid aralığı ve sütun filtresi

İki kök etkin yazma görüntüsünde doğrulanır, rowid aralığı ve özgün kalan filtre korunur. 396 native ve 396 takip yazıcısı, 792 rollback ve 1.188 açılış doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_183.md).


## V6b.184 — Boş ana dosyanın tek hedefli WAL girişi

İlk ROW boyunca sıfır bayt DB ve EXCLUSIVE korunur, sayfa 1 sonlandırmada ofset sıfırdan yazılır. 1.308 fiziksel çift, 2.646 C açılışı ve 12 doğal eş bağlantı profili doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_184.md).


## V6b.185 — Açık TEMP ile boş ana dosyanın WAL girişi

Nitelemesiz WAL girişinde boş ana dosya ilk ROW boyunca sıfır kalır ve TEMP kipi korunur. 1.656 fiziksel çift, 2.772 C açılışı ve birleşik 24 canlı eş profili doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,342 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_185.md).


## V6b.186 — İki kökte açık WHERE 1 DELETE

İki kök aynı yazma görüntüsünde doğrulanır ve açık WHERE 1 C'nin satır bazlı sırasını kullanır. 396 native yazıcı ve rollback, 1.188 açılış ve 48 fiziksel Clear ayrımı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,032 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_186.md).


## V6b.187 — Bir bağlı dosyayla boş ana dosyanın WAL girişi

İki katılımcının C kilit ve yazma sırası ile doğal BUSY sonrasındaki geçici kip korunur. 1.188 fiziksel çift ve birleşik 60 BUSY ile 24 canlı C okur profili doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_187.md).


## V6b.188 — Üç kökte rowid nokta yazımları

Rowid noktası ve düz VALUES üç kökün etkin işlem görüntüsü doğrulamasını kullanır. 1.026 yazıcı, eş rollback ve takip yazısı, 540 C fiziksel çift ve bağımsız C yeniden açılış doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_188.md).


## V6b.189 — Tek hedefli WAL çıkışında busy handler

Normal yazılabilir tek dosyada WAL kapatma hatası C gibi doğrudan dönüyor. 498 C–Rust çifti, 48 hazırlık ve 36 WAL giriş koruması ile yeniden deneme ömrü doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_189.md).


## V6b.190 — İki hedefli WAL çıkışında busy handler

Bir ATTACH ile koordineli WAL kapanışı hatayı C gibi doğrudan döndürüyor. 540 C–Rust çifti, aynı statement tekrarları ve OFF ilk satır dosya ömrü doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,246 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_190.md).


## V6b.191 — Tek hedefli SQL WAL temizleme sonucu

Kilit ve checkpoint sonucu C gibi korunuyor. Yan dosya temizliği devam ederken ayrı hata statüsü artık SQL sonucu olarak dönmüyor. 120 dizin izin kontrolü, 120 yazılabilir dizin koruması ve 1.200 fiziksel DB/WAL/SHM fazı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,017 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_191.md).


## V6b.192 — Bağlantı kapanış sonucu

Etkin statement ön koşulu C gibi BUSY. Kabul edilen bağlantı kapanışında pager temizliği tamamlandıktan sonra C gibi başarı dönüyor. 192 C–Rust sonuç ve fiziksel çift, 18 eski dönüş farkı ve 480 C yeniden açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,155 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_192.md).


## V6b.193 — Birden çok ATTACH ile WAL çıkışı

C WAL kapanışının doğrudan hata dönüşü önceki koşullarla tüm koordineli slotlara uygulanıyor. 630 C çift, 946 eski callback ve 56 OFF ilk satır dosya farkı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_193.md).


## V6b.194 — WAL geri almada sıfır canlı frame

Ortak indekste sıfır canlı frame durumunda erişilemeyen kayıtlar C gibi sonraki ilk yazıya kadar kalıyor. 432 doğal spill akışı, 45 bağlantı yaşamı ve 1.728 C yeniden açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,230 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_194.md).


## V6b.195 — Journal mode değerleri ve C önek eşlemesi

Hazırlama ve SQL yürütmesi C kip adlarının aynı ASCII önek kuralını paylaşıyor. 672 tek, 72 bağlı dosya C çifti ve temiz dosya açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_195.md).


## V6b.196 — WAL checkpoint kip adı

Tanınmayan checkpoint kip adı C gibi PASSIVE oluyor. 360 C karşılaştırması, 96 önceki sonuç farkı ve 1.800 fiziksel evre doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,435 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_196.md).


## V6b.197 — Negatif PERSIST_WAL isteğinde VFS sonucu

NamedDisk negatif sorgusu C gibi native VFS handle'ına gidiyor. Callback bulunmadığında mevcut Pager değeri kullanılıyor. 216 native, 144 fallback ve 36 DETACH akışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_197.md).


## V6b.198 — max_page_count değer dönüşümü ve fiziksel sınır

Codegen ve çalıştırma aynı C dönüşümünü kullanıyor. Geçerli hex, decimal ve mevcut sayfa sınırı eş. 4.608 native kontrol ve 72 C yeniden açılışı doğrulandı. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_198.md).


## V6b.199 — TEMP CREATE hazırlığı ve şema cookie ömrü

Hazırlama TEMP Btree'yi açıyor, eski cookie ilk gerçek şema kontrolüne kadar saklanıyor. 1.608 native ve 72 opcode akışı, 1.680 bağımsız C açılışı eş. OFF 1186/1/337, ON 1178/9/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_199.md).


## V6b.200 — DDL geri alımında V2 sorter ve canlı okuyucu

Tamamlanmış sorter satırı verir, canlı Btree okuyucusu 516 ile durur. 72 V2 profil iki kipte eş, Group5 odak testi iki kipte geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_200.md).


## V6b.201 — Kapasite reddinde V1 yazım step sonucu

Dış V1 step 1. Reset ve finalize asıl 13 sonucunu koruyor. 432 C açılışı ve önceki sözdizimi tanısı eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_201.md).


## V6b.202 — DDL geri alımında V1 okuyucu step sonucu

Dış step 1. Canlı okuyucunun 516 ve stale deyimin 17 terminal kodu korunuyor. 1.728 C açılışı ölçüldü. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_202.md).


## V6b.203 — TEMP cache_size şema ve pager ömrü

Şema ayarı korunuyor. Yeni pager varsayılan cache ile açılıyor. 876 native ve C açılışı eş, önceki 72 regresyon kapandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,243 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_203.md).


## V6b.204 — Eksik SELECT sütunu byte ofseti

İki çözümleyici extended kod ve tanı metninde eşleştiğinde eksik sütun ofseti ekleniyor. 384 OFF ofseti ve 216 V1 tanı profili eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_204.md).


## V6b.205 — Belirsiz SELECT sütunu byte ofseti

Aynı genişletilmiş kod ve mesajda C çözümleyicisinin byte ofseti taşınıyor. OFF 240 tanı, 108 C açılışı ve 216 legacy tanı profili eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_205.md).


## V6b.206 — ORDER BY açık çıktı alias önceliği

Yalın ORDER BY adı ilk açık çıktı alias’ını çözümlüyor. İki kipte 24 profil, 108 C açılışı ve bağlama sonrası sıra C ile eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_206.md).


## V6b.207 — TEMP CREATE eski cookie ile V1 step

Eski TEMP cookie ilk yazma denetiminde saptandığında V1 step 1 ve terminal 17 döner. 2.612 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_207.md).


## V6b.208 — TEMP journal OFF savepoint

Açık journal handle olmayan özel TEMP OFF pager savepoint playback yapmıyor. 2.070 C açılışı ve ana dosya sonuçları eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_208.md).


## V6b.209 — foreign_keys setter sonrası legacy V1 erken expiry

Gerçek setter adımından sonra erken V1 step 1, reset ve finalize 17 döndürür. Önceki tanı ve byte ofseti korunur. 804 profil ve 2.412 C açılışı eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_209.md).


## V6b.210 — Yazısız TEMP FILE OFF geri alma

Sayfa yazılmamış ve backing henüz açılmamış özel TEMP FILE OFF işleminde önceki kaydedilmiş sayfalar korunur. 1.296 yeni ve 3.024 koruma C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_210.md).


## V6b.211 — recursive_triggers setter sonrası V1 erken expiry

Fiilen çalışmış recursive_triggers setter adımı sonrası V1 erken step ve terminal C ile eş. 1.104 fark kapandı. 4.644 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_211.md).


## V6b.212 — Açık TEMP FILE OFF yazısız geri alma

Gerçek TEMP dosyası açıkken sıfır yazılı geri alma önceki commit verisini koruyor. 1.608 birleşik C açılışı ve fiziksel TEMP yazma izi eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,239 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_212.md).


## V6b.213 — Hazırlanmış trigger yazısı

24 OFF farkı kapandı. Önceden hazırlanmış yazı recursion kararını koruyor. 600 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_213.md).


## V6b.214 — Hazırlanmış count_changes sonucu

Hazır yazı biçimi, sütun adı ve changes() yaşamı C ile eş. 1.128 ana ve 144 INSERT SELECT profil doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,173 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_214.md).


## V6b.215 — View DELETE hedef sayımı

Sayım INSTEAD OF tetikleyicisinden önce yapılıyor. 94 OFF farkı kapandı ve 1.944 view profili C ile eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,292 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_215.md).


## V6b.216 — Bağlı açık okuyucunun rollback sonucu

İlk bağlı rowid okuyucusu restore edilmiş satırı görüyor. 24 OFF farkı kapandı ve 2.880 C açılışı eş. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_216.md).


## V6b.217 — Bağlı sabit LIMIT okuyucusu

İlk bağlı canlı tarama sabit LIMIT sayacını koruyor. 42 OFF farkı kapandı ve 4.650 C açılışı geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 390/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_217.md).


## V6b.218 — Aynı tablo INSERT SELECT kaynak aralığı

48 OFF profili C ile eş. Seed 6240 kapanırken kalan dokuz ham DML farkı korundu. 8.018 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,323 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_218.md).


## V6b.219 — Aggregate BLOB sayısal öneki

SUM, TOTAL ve AVG üç metin kodlamasında BLOB sayısal önekini C ile aynı kullanıyor. Her kipte 1.816 çıktı farkı kapandı. 2.520 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_219.md).


## V6b.220 — Bellek hedefli backup geometri kontrolü

Farklı sayfa boyutlu dolu bellek hedefi step(0) sırasında READONLY 8 ve 0/0 bildiriyor. 48 API farkı kapandı ve 5.004 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_220.md).


## V6b.221 — Backup finish hedef tanısı

72 hazır V1 akışında eski sözdizimi mesajı taşınmıyor. 400 profil ve 1.644 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,292 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_221.md).


## V6b.222 — Açık sayısal saat dilimi ofseti

18 ana girdi ve kip başına 108 kayıt C ile eşitlendi. Negatif yıl biçimi ve 156 C yeniden açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_222.md).


## V6b.223 — subsec ve subsecond çıktısı

Dört zaman işlevi kesirli SQL tipi ve biçimini C ile eşliyor. Kip başına 2.232 fark kapandı. 246 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_223.md).


## V6b.224 — Açık unixepoch girdisi

İlk explicit unixepoch girdi türü, sıra, yuvarlama ve tarih sınırı C ile eş. 384 ana ve 12 önceki fark kapandı. 423 C açılışı geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_224.md).


## V6b.225 — UTF-16 BLOB CAST metni

Geçerli UTF-16LE/BE gömülü NUL ve devamını koruyor. 54 ayrı sonuç, 24 fiziksel profil C ile eşleşti. 444 C açılışı geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,244 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_225.md).


## V6b.226 — Backup fiziksel sayfa imajı

Önceden kopyalanmış sayfa son fiziksel yayımdan yenileniyor. 48 hedef farkı kapandı. 744 eski ve 144 geniş profil doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_226.md).


## V6b.227 — auto zaman değiştiricisi

İlk auto, C sayısal Julian/Unix ayrımını ve sınırlarını uyguluyor. 687 ana ve 28 önceki fark kapandı. 540 C açılışı geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_227.md).


## V6b.228 — Backup ilk sayfa edinmesi

İlk kopyada kaynak rollback pager sayfasının edinme ömrü C ile eş. 90 hedef dosya farkı kapandı. 840 normal ve 144 geniş profil doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_228.md).


## V6b.229 — Açık julianday girdisi

İlk explicit julianday ham sayısal Julian aralığını C ile eşliyor. 1.035 ana girdide 405 fark kapandı. 684 C açılışı doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_229.md).


## V6b.230 — JULIANDAY REAL çıkışı

Geçerli Julian milisaniyesi C gibi tam sayı toplamından sonra tek bölmeyle REAL oluyor. 171 ayrı bit farkı kapandı. 851 C açılışı ve 12 VDBE testi doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,274 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_230.md).


## V6b.231 — STRFTIME %J biçimi

STRFTIME %J ortak Julian REAL hesabı ve C uyumlu %.16g biçimini kullanıyor. 594 ayrı fark kapandı. 735 C açılışı ve 12 VDBE testi doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,285 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_231.md).


## V6b.232 — Normal rollback küçültme sırası

Normal D/P/T disk rollback büyümüş fiziksel dosyayı preimage yazısından önce özgün boyuta getiriyor. 36 sıra farkı kapandı. 324 C açılışı ve 216 fiziksel karşılaştırma doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_232.md).


## V6b.233 — ISO son Julian aralığı

Geçerli ayrıştırılmış ISO tarih ve saat, değiştiricilerden sonra Julian aralığı dışındaysa C gibi NULL veriyor. 29 ayrı sınır farkı kapandı. 905 C açılışı ve 12 VDBE testi geçti. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,239 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_233.md).


## V6b.234 — Backup hedefi kısmi yazısı

Olağan eş sayfa boyutlu named-disk DELETE, PERSIST ve TRUNCATE yedeğinde ilk ardışık hedef kopyası sayfalayıcı edinme/yazma/bırakma sırasını kullanır. 28 odak profilinde 56 C–Rust geçici yazı ve erken finish çifti eşleşti. Önceki 14 rollback sıra farkı kapandı. 504 backup korumasında 90 ilk kopya ve 48 fiziksel yayın eşliği korundu. 144 geniş profilde 864 son dosya çifti eş. 108 olağan rollback kontrolünde önceki işlem sırası korundu. 32.940 strict kayıt doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_234.md).


## V6b.235 — ISO uzun saat kesri

Canonical ISO uzun saat kesrinde birikim, 0.999 üst sınırı ve tam saniye ile milisaniyeye yuvarlama C binary64 işlem sırasına uyar. 97 kesir ve bir son aralık bağlamında kip başına 3.528 ham kayıt eşleşti. Ek 288 ham %f/iJD bağlamı eş ve önceki C eş 60 %f çıktısı korundu. 73 çok uzun girdi için sonlu, sonsuz ölçek ve NaN davranışı ölçüldü. 1.144 C salt okunur açılışı, 12 VDBE testi ve 10.789 strict kayıt doğrulandı. OFF 1186/1/337, ON 1179/8/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_235.md).


## V6b.236 — SELECT üç parçalı yıldız tanısı

SELECT projeksiyonunda şema ve tablo veya alias ile nitelenmiş yıldız C gibi prepare sırasında rc ve extended rc 1, aynı mesaj ve yıldızın bayt ofsetini döndürür. 64 OFF farkı kapandı. 136 negatif ve 112 geçerli C–Rust karşılaştırması, 372 C açılışı ve 56 gerçek ON izi doğrulandı. İki mevcut runtime testinin beklentisi C tanısıyla hizalandı ve bunların diğer kontrolleri korundu. İki runtime ve yapısal AST testi iki kipte 6/6 geçti. 4.722 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_236.md).


## V6b.237 — Tarihli ISO 24:00

İşaretsiz ISO tarihli 24:00, canonical saat parçaları ve ham kesir sıfır olduğunda yerel ertesi güne taşınır. Yerel yıl sınırı ofsetten ve son Julian denetiminden ayrı uygulanır. 104 bağlam ve kip başına 1.872 ham kayıt C ile eşleşti. 460 diğer ana bağlam değişmedi. 61 gerçek saklama kontrolü, 923 C açılışı, 12 VDBE testi ve 9.102 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_237.md).


## V6b.238 — Yalın ISO 24:00

Yalın 24:00 ayrıştırmasında ilk takvim değiştiricisine kadar C'nin tarih ve ofset durumu izlenir. 55 kabul ve 10 iJD farkında kip başına 1.170 ham kayıt eşleşti. 499 diğer ana bağlam ve 104 tarihli düzeltme korundu. İki TZ'de 54 mevcut host dönüşüm zinciri ham eş kaldı. 72 gerçek saklama, 1.055 C açılışı, 12 VDBE testi ve 10.273 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_238.md).


## V6b.239 — Görünüm UPDATE TEXT aralığı

Doğrudan tek rowid kaynaklı görünüm UPDATE işleminde tek tam BINARY TEXT indeks ve tek kanonik TEXT literal aralığı C'nin tarama ve tetikleyici sıra anahtarını izler. 75 hedef profilde 300 API, sıra ve nihai dosya çifti eş. Önceki sekiz TEXT farkı, geniş matriste 126 OFF ve 84 ON ilişkisel geri dönüş farkı kapandı. 160 ayrı koruma karşılaştırması ve V97/V109 korundu. 14.809 strict kayıt doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_239.md).


## V6b.240 — STRFTIME %k

STRFTIME %k mevcut saat çıktısı ile aynı değeri iki karakter genişlikte boşlukla doldurur. 185 bağımsız bağlam iki kipte ham eş. 0–23 saatler, üç kodlama, UTC/ofset, tarihli ve yalın 24:00, NULL ve karışık biçimler ölçüldü. 198 gerçek saklama satırı, 654 C açılışı, 12 VDBE testi ve 6.895 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_240.md).


## V6b.241 — Görünüm UPDATE tırnaklı NUMERIC aralığı

Tek tam BINARY NUMERIC indeksli doğrudan rowid görünüm UPDATE işleminde mevcut affinity tırnaklı sınırı sayıya dönüştürünce aynı indeksli materyalizasyon kullanılır. 276 OFF ve 184 UTF16 ON eski kalıcı sıra/dosya farkı ile negatif sonsuzluktaki 20 ek fark kapandı. Önceki dört tırnaklı fark C eş. 4.380 native ve 4.380 C açılışı, 31.465 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Ayrışma bulucu ve önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,275 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_241.md).


## V6b.242 — Legacy görünüm UPDATE şema denetimi

Olağan ilişkisel MAIN legacy V1 görünüm UPDATE işlemi gerçek Btree okuma snapshot üstünde hazırlanmış şema kimliğini ve cookie değerini doğrular. 48 OFF ve 32 UTF16 ON geri dönüşünde eski şema yazmadan reddedildi. STEP, RESET, FINALIZE, ana dosya ve log C eş. Ek önceki değişiklik sayacı ve normal takip işlemleri korundu. 576 native ve 576 C açılışı, iki kipte önceki 10/10 test ve 4.735 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,252 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_242.md).


## V6b.243 — Negatif ISO yıl ayrıştırması

Canonical negatif ISO tarih ve olağan saatte geçerli yerel gün, dört ASCII yıl hanesi ve erken yıl sınırı mevcut Gregorian hesap ve son Julian denetimiyle çalışır. 54 hedef bağlam ve kip başına 972 ham kayıt C eş. 89 diğer bağlam ve önceki korumalar değişmedi. 54 gerçek saklama, C açılışları, 12 VDBE testi ve 6.803 strict final exact doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_243.md).


## V6b.244 — Negatif tarihli ISO 24:00

Canonical negatif tarihli 24:00, C'nin önce yıl büyüklüğünü ilerletip işareti uygulama kararını ve ham sıfır kesrini izler. Başlangıç ve değiştiricilerde Julian aralığı korunan 145 hedef, kip başına 2.610 ham kayıt C eş. 59 ana bağlam korundu. 145 gerçek saklama, 288 C açılışı, 12 VDBE ve 4.598 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_244.md).


## V6b.245 — Görünüm UPDATE iki tam indeks

Sabit şemalı doğrudan rowid görünüm UPDATE’inde iki tam nonunique BINARY indeks arasından tek uyumlu literal aralığı mevcut aralık yoluyla izler. 644 sıra ve dosya farkı kapandı. 204 kapsam koruması ve 336 önceki native kontrol eş. 5.100 native ve 5.100 C açılışı, 34.934 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,250 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_245.md).


## V6b.246 — Negatif ISO 24:00 süre dönüşü

Canonical negatif tarihli 24:00 süre değiştiricileri için C sayı sınırı ve yuvarlama, geçici Julian taşması ve son değer denetimi korunur. 20 ölçülen süre zinciri, üç ilk ve üç ara dönüş C eş. 42 ek native aritmetik, 450 C açılışı, 12 VDBE ve 6.305 strict doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_246.md).


## V6b.247 — Görünüm UPDATE eş maliyetli iki tam indeks

Sabit şemada aynı BINARY NUMERIC anahtarını kullanan iki tam aralık indeksi, tablo okuması gerektiğinde ve mevcut MAIN okuma veya yazma snapshot’ı varsa C katalog sırasıyla değerlendirilir. 1.060 kalıcı sıra ve dosya farkı kapandı. Önceki 644 vaka, 48 covering kontrolü ve 6.276 bağımsız C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_247.md).


## V6b.248 — Negatif ISO 24:00 korunmuş takvim

Kanonik negatif tarihli 24:00, C’nin geçerli ham takvim durumunu taşıdığı doğrulanmış geçişleri izliyor. Önceki 24 ve yeni 35 takvim vakası C ile eş. Diske yazılan önceki 24 vaka bayt eş ve 522 bağımsız C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_248.md).


## V6b.249 — Rakamsız saniye kesri

Saat metninde saniyeden sonraki nokta en az bir kesir rakamı gerektirir. Sabit C ile 648 sorgu karşılaştırmasındaki 36 fark ve beş ek odak farkı kapandı. Önceki 612 sonuç bayt eş. UTF8 DELETE ve UTF16LE WAL kalıcı dosyaları C ile bayt eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_249.md).


## V6b.250 — Finder L03 olağan PRAGMA optimize

Normal MAIN kapsamındaki dolu, istatistiksiz tam indeks için varsayılan PRAGMA optimize, C gibi mevcut ANALYZE yoluyla istatistik yazar ve geçici limiti geri yükler. Kanonik senaryonun beş evresindeki C ve Rust dosyaları eş. Bağımsız 588 C salt okunur açılışı ve iki taze tam Finder koşusu geçti. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_250.md).


## V6b.251 — ISO saat alanlarının genişliği

Ortak saat ayrıştırması saat, dakika ve saniye alanlarında C gibi iki ASCII rakam ister. 824 sorguluk matriste 84 fark kapandı, 740 eş kontrol ve önceki kesir sonuçları korundu. Üç kalıcı dosya ve 15 bağımsız C salt okunur açılışı eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_251.md).


## V6b.252 — STRFTIME biçim sonunda eşleşmemiş yüzde

STRFTIME biçimi tek eşleşmemiş yüzde ile sona erdiğinde C gibi SQL NULL döner. 1.944 biçim girdisinde 399 fark kapandı. Kaçırılmış yüzde çiftleri, geçerli 741 önceki çıktı ve 1.536 tarih kontrolü eş. Üç fiziksel dosya ve 15 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_252.md).


## V6b.253 — Varsayılan sayısal tarih metni

Normal sayısal TEXT, değiştiricisiz veya yalnız çıktı hassasiyeti bayraklı durumda C’nin Julian gün dönüşümünü kullanır. 46 eski C farkı kapandı. STRFTIME terminal yüzde önkoşulu ile 1.944 biçim ve 1.480 native matrisin taze birleşik doğrulaması geçti. Üç dosya profili ve 60 C salt okunur açılışı eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 243/245, determinism 244/245. Açık L4 ve L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_253.md).


## V6b.254 — Finder S10 WITHOUT ROWID DELETE

Sağlıklı MAIN WITHOUT ROWID tablosunda örtük tam taramalı DELETE için kanıtlanan tek tam covering indeks, mevcut C uyumlu tarama ve silme sırasını kullanır. Kanonik ve ortak 15.000 kayıtlı başlangıç dosyasında Rust OFF/ON ile C L0. 165 bağımsız C salt okunur açılışı, 12 hedef ve 21 koruma profili ile iki tam yeni Finder koşusu geçti. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,514 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_254.md).


## V6b.255 — Süre biriminde tek çoğul ek

Ortak süre değiştiricisi C gibi en fazla bir çoğul s ekini kabul eder. 2.925 fark kapandı ve 4.347 geçerli sonuç C ile eş kaldı. Numeric TEXT, negatif 24:00 ve 60 bağımsız C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_255.md).


## V6b.256 — Ortak ISO tarih sözdizimi

Ortak tarih ayrıştırıcı C’nin ASCII yıl, ay ve gün alanlarını, büyük T ve ASCII boşluk ayırıcısını ve saatli Z kuralını kullanır. 92 girdi üç kodlamada C ile hizalandı, 530 eş sonuç ve önceki numeric, kesir ve korunmuş takvim kontrolleri korundu. Üç dosya profili ile 60 C salt okunur açılışı eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_256.md).


## V6b.257 — Ortak tarih değiştiricisi sözdizimi

Tarih değiştiricileri C’nin ASCII ayırıcı ve baştaki sayısal karakter kararlarını kullanır. 212 farklı girdi düzeldi ve önceki 459 eş sonuç korundu. ISO, sayısal TEXT, negatif 24:00, üç kalıcı dosya ve 60 bağımsız C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_257.md).


## V6b.258 — Görünüm UPDATE tek koşullu tam ve kısmi indeks

Sabit MAIN şemasında ve mevcut açık yazma snapshot’ında tek tam ve tek kısmi indeks, tek basit aralık koşulu için C’nin yapısal uygunluk ayrımını kullanır. Kalıcı sıra ve dosya farkları OFF’ta 108, UTF-16 ilişkisel ON’da 72 kapandı. 330 koruma ve 2.628 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,457 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_258.md).


## V6b.259 — Doğrudan INTEGER/REAL Julian ve hassasiyet

Değiştiricisiz veya yalnız alt saniye hassasiyeti bayraklı INTEGER ve REAL Julian girdileri ortak C uyumlu dönüşümü ve son aralık kontrolünü kullanır. 111 sorguda bit eş, 45 fark kapandı. Üç kalıcı dosya ve 105 bağımsız C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,253 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_259.md).


## V6b.260 — Görünüm UPDATE karşıt iki sınır ve kısmi indeks

Açık MAIN yazma işleminde aynı NUMERIC BINARY sütununda bir alt ve bir üst sınırdan biri kısmi indeks koşuluyla yapısal eş olduğunda C’nin planı ve kalıcı kayıt sırası izlenir. 90 OFF ve 60 UTF-16 ilişkisel ON farkı kapandı. 888 önceki koruma ve 4.554 bağımsız C salt okunur açılışı geçti. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_260.md).


## V6b.261 — Typed Julian tek süre değiştiricisi

Tek olağan süre değiştiricili INTEGER ve REAL için C uyumlu ham dönüşüm, birim sınırı, yarım ms yuvarlaması ve son aralık denetimi kullanılır. Üç ölçüm kümesinde sırasıyla 87, 78 ve 441 C farkı kapandı. Üç kalıcı dosya profili ve 105 C salt okunur açılışı eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_261.md).


## V6b.262 — Görünüm UPDATE aynı yönlü iki sınır

Açık MAIN yazma işleminde aynı NUMERIC sütunun aynı yönlü iki koşulundan biri kısmi indeksle yapısal eşse mevcut C uyumlu ilk sınır kullanılır. 216 hedef C ile bayt eş ve 280 önceki sıra/dosya farkı kapandı. 12 ilk yaprak kontrolü ve 4.950 bağımsız C salt okunur açılışı eş. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,522 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_262.md).


## V6b.263 — Typed Julian çoklu süre değiştiricileri

Yalnız süre ve hassasiyet bayrakları içeren typed INTEGER/REAL zincirleri, bir ham Julian dönüşümü, C birim sınırı, her adımın signed yuvarlaması ve tek final aralık kontrolünü kullanır. 1.573/1.573 C eş ve 1.122 fark kapandı. Üç kalıcı kodlama profili ile 120 C salt okunur açılışı geçti. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_263.md).


## V6b.264 — Görünüm UPDATE farklı sütun aralığı

Olağan görünüm UPDATE taramasında iki doğrudan sütun izdüşümü, kısmi indeksin aralık sütunu ile korunmuş plan sınırlarına göre eşlenir. Sabit C ve iki Rust kipi 252 hedef, 420 korunmuş fiziksel kapanış ve 9.414 C salt okunur açılışında eşleşti. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_264.md).


## V6b.265 — Typed Julian tek başlangıç değiştiricisi

Doğrudan INTEGER ve REAL Julian girdilerinde tek exact start of day/month/year takvim okumasından önce C yuvarlaması ve aralık kontrolü kullanır. Ana 225 hedefin ve ek 408 sınır hedefinin tamamı iki kipte C ile eş. İki matriste 129 ve 208 fark kapandı, 45 C ve 90 Rust salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_265.md).


## V6b.266 — Görünüm UPDATE sabit eşitlik ve aralık

Açık MAIN yazma işleminde ayrı INTEGER eşitlik ve NUMERIC aralık sütunlarını C yapısal uygunluğuna göre mevcut planlayıcıya verir. 1.152 DELETE/WAL hedefi, 168 ek sözdizimi hedefi ve 14.256 salt okunur C açılışı eş. 192 unary artı komşusu eski davranışı korudu. OFF 1186/1/337, ON 1181/6/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_266.md).


## V6b.267 — Point UPDATE ve DELETE plan testi

Mevcut test doğrudan rowid planı ile eski Rust VDBE’yi iki workspace ortamında aynı tam gövdeyle karşılaştırır. Test iş parçacığı geçici kipi güvenli geri yükler. Odaklı ve paralel ON/OFF testleri, 72 normal C/Rust DML tekrarı ve dosya baytları doğrulandı. OFF 1186/1/337, ON 1182/5/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_267.md).


## V6b.268 — Tamsayı aggregate plan testi

Mevcut testin sekiz profili, profil başına 700 işlemi ve tüm kıyasları iki workspace ortamında aynı gövdeyle çalışır. Üretim yolu için 48 normal COUNT ve SUM okuması, 48 C salt okunur açılışı ve ham tekrar eşliği doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,251 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_268.md).


## V6b.269 — Görünüm UPDATE tek bağlı eşitlik

Açık MAIN yazmasında dar V2 görünüm UPDATE yeniden hazırlaması, ilgili INTEGER veya REAL bağlı değeri kısmi indeks uygunluğuna taşır. İlk bağda 8 ve yeniden bağlamada 12 profil C kökü ve dosya baytı eş. 1.440 C salt okunur açılışı ve 376 OFF koruması doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,318 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_269.md).


## V6b.270 — Tek ISO tarih ceiling değiştiricisi

Dört haneli işaretsiz ISO tarihindeki tek terminal ceiling C normalizasyonunu kullanır. 14 Tcl vakası, üç kodlamada 706 hedef ve 36 salt okunur C açılışı doğrulandı. Diğer 28 Tcl vakasının ham davranışı korundu. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,303 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_270.md).


## V6b.271 — Tek ISO tarih floor değiştiricisi

Dört haneli işaretsiz ISO tarihin tek terminal floor adımı C'nin başlangıç günü taşmasını düzeltiyor. 14 Tcl vakası ve üç kodlamada 800 hedef C eş, önceki 14 ceiling vakası başarılı. 36 salt okunur C açılışı ve DELETE/WAL dosya baytları eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_271.md).


## V6b.272 — Ay ve yıl kaydırması sonrası ceiling

Geçerli ISO başlangıç gününde tam ve işaretli ay veya yıl kaydırmasından sonra tek terminal ceiling, mevcut kaydırmanın C ile eş normalize sonucunu kullanır. Dört Tcl vakası, üç kodlamada 7.852 SQL ölçümü ve 27 C salt okunur açılışı eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_272.md).


## V6b.273 — Aynı tamsayı yeniden bağlama

Maskeli yuvada önceki ve yeni typed INTEGER aynıysa C gibi gereksiz yeniden hazırlama oluşmaz. 116 generated ON profilinin sayacı C eş, önceki Rust satırları, kökleri ve dosya baytları korundu. 6.340 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_273.md).


## V6b.274 — Ay ve yıl kaydırması sonrası floor

Geçerli ISO tarihte tam ve işaretli ay veya yıl kaydırmasından sonra tek terminal floor, C günü taşma miktarını düşer. Dört Tcl vakası, üç kodlamada 15.704 SQL ölçümü ve 27 C salt okunur açılışı eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_274.md).


## V6b.275 — Görünüm UPDATE iki bağlı koşul

İki kompakt yuvada, açık MAIN V2 görünüm UPDATE yeniden hazırlaması gerçek equality yuvasını C ile aynı kısmi indeks uygunluğuna taşır. 412 STEP kökü ve 208 son dosya C eş. 4.384 C salt okunur açılışı ve eski tek bağ/tamsayı korumaları doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_275.md).


## V6b.276 — Tek işaretli takvim kaydırması

Geçerli ISO başlangıçta dört haneli tek işaretli takvim değiştiricisi yıl ve ayı önce, gün miktarını son uygular. Ortak ISO Julian denetimi son değeri doğrular. 746 hedef, 2.590 ham koruma ve 30 C salt okunur açılışı eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_276.md).


## V6b.277 — İşaretli takvim sonrası floor ve ceiling

Geçerli ISO başlangıçta dört haneli işaretli takvim kaydırmasının terminal floor ve ceiling adımları C ile eş. Düzeltme altı Tcl vakasını kapattı ve date-19 adayını 42/42 yaptı. Üç kodlamada 1.490 terminal çıktı ve 30 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,291 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_277.md).


## V6b.278 — Görünüm UPDATE bağlı aralık

Açık MAIN V2 görünüm UPDATE yeniden hazırlamasında dar bağlı NUMERIC aralık ve plain INTEGER kısmi predikat C yapısal karşılaştırmasını kullanır. 456 STEP kökü, 256 son dosya ve 5.476 C açılışı eş. Önceki eşitlik ve aynı tamsayı vakaları korundu. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_278.md).


## V6b.279 — strftime Pazartesi başlangıçlı hafta

Açık TEXT biçimi ve tek sabit tarih girdisinde strftime yüzde W, ilk Pazartesiye göre C hafta numarasını üretir. 27 Tcl vakası, üç kodlamada 1.786 hedef ve 18 C salt okunur açılışı eş. Önceki date-19 sonuçları korundu. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_279.md).


## V6b.280 — strftime on iki saat alanı

Açık TEXT biçimi ve tek sabit tarih girdisinde yüzde l, C'nin boşluk dolgulu on iki saat alanını üretir. Tcl date-3.29 kapandı. Üç kodlamada 2.034 sonuç, 18 C salt okunur açılışı ve önceki 27 W ile 42 date-19 koruması doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,274 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_280.md).


## V6b.281 — Görünüm UPDATE düşük boşluklu yuvalar

İki gerçek V2 bağ yuvası 1–31 içindeyse kısmi equality kökü ve maskesi C ile eş. Önceki 68 dosya farkı kapandı. 16 normal okumadan yazmaya geçiş örneğinde üçüncü STEP kök ve log eş, ilk fark açık. 9.384 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,285 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_281.md).


## V6b.282 — Görünüm UPDATE yüksek bağ yuvaları

C'nin paylaşılan yüksek bağ biti gerçek equality yuvasından ayrı kullanılır. Önceki matrisin 68 son dosya farkı kapandı. 12.436 C salt okunur açılışı ve sonraki STEP kök/log kararları doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_282.md).


## V6b.283 — timediff scalar işlevi

İki argümanlı timediff, ortak tarih ayrıştırması ve Gregorian zaman hesabıyla C süre metni üretir. Üç kodlamada 62 TEXT, 14 NULL, beş arity tanısı ve iki Tcl vakası eş. 18 C salt okunur açılışı ve DELETE/WAL dosya eşliği doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_283.md).


## V6b.284 — Saatli işaretli tarih değiştiricisi

Tek normal signed tarih ve saat değiştiricisi C'nin yıl, ay, gün ve saat sırasını uygular. 62 geçerli timediff çiftinin iç içe tarih, Julian ve milisaniye çıktısı eş. İki Tcl vakası ve 18 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_284.md).


## V6b.285 — strftime Pazar başlangıçlı hafta

Açık TEXT biçimi ve tek sabit tarihle yüzde U, C'nin Pazar başlangıçlı haftasını üretir. İki Tcl vakası kapandı ve date-23 adayı 22/22 oldu. Üç kodlamada 2.433 sonuç ve 18 C salt okunur açılışı eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_285.md).


## V6b.286 — WITHOUT ROWID görünümü adlandırılmış RETURNING

Tek sıradan BINARY ikincil indeksli WITHOUT ROWID görünümünde nitelemesiz RETURNING ile sayı aralığı C materyalizasyon sırasını kullanır. Önceki 120 fark kapandı ve 480 DELETE veya WAL hedefi C eş. 2.592 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_286.md).


## V6b.287 — Typed Julian tam ay ve yıl

Tek typed Julian ve tam ay veya yıl değiştiricisi, C miktar ve Julian aralık denetimlerini kullanır. 388 hedefte 208 önceki fark ve dört Tcl vakası kapandı. Üç kodlamada 30 C salt okunur açılışı ve dosya baytları eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_287.md).


## V6b.288 — WITHOUT ROWID görünümü iki tam indeks

Kapsayıcı olmayan iki sıradan tam BINARY ikincil indeksli WR görünüm aralığı, C ile eş kökü seçer. 360 eski log veya dosya farkı kapandı. 1.632 iki indeks profili ve 7.728 C salt okunur açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_288.md).


## V6b.289 — Gün ay ve yıl sonu tarih değiştiricileri

Normal ISO tarih metninde end of day/month/year, C'nin son milisaniye ve civil yıl aralığı kararını kullanır. Dokuz Tcl vakası, kodlama başına 976 hedef ve 30 C salt okunur açılışı eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,293 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_289.md).


## V6b.290 — İşaretli hafta günü kaydırması

Geçerli ISO tarih metninde tek weekday sayısındaki gerçek eksi, C'nin geriye kaydırma yönünü kullanır. 13 Tcl vakası ve 932 önceki fark kapandı. 1.680 ileri yön kontrolü ve 30 salt okunur C açılışı doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_290.md).


## V6b.291 — WITHOUT ROWID görünümü aynı anahtarlı kısmi indeks

Aynı BINARY anahtardaki yapısal olarak eş tamsayı kısmi koşulu, C ile eş materyalizasyon kökünü kullanır. 384 hedefte 640 önceki ilişkisel fark kapandı. 8.040 C salt okunur açılışı ve kapsayıcı durumlar doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,330 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_291.md).


## V6b.292 — Tek saat kaydırması

Geçerli ISO tarih-saat girdisi ve tek saat kaydırmasında yerel SQL sonucu C ile eş. Dokuz Tcl vakası ve üç metin kodlamasında DELETE/WAL dosya kontrolleri doğrulandı. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,291 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_292.md).


## V6b.293 — WITHOUT ROWID görünümünde tam indeks seçimi

Ölçülen yapısal elemede C’nin tam indeks kökü, tetikleyici sırası ve kapanmış dosya baytları eşleşti. 480 önceki fark ve 80 ek üst eşik sonucu kapandı. 8.760 C salt okunur bütünlük denetlendi. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_293.md).


## V6b.294 — UTC değiştiricisinin tam yazımı

C’nin reddettiği boşluk çevreli utc değerleri artık SQL NULL. Dört saat dilimi ve üç kodlamada 432 önceki fark kapandı. 24 DELETE/WAL imajı ve 120 C salt okunur bütünlük kontrolü eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_294.md).


## V6b.295 — WITHOUT ROWID farklı anahtarlı kısmi indeks

C’nin farklı eşikte elediği kısmi indeks yerine tam indeksin aynı aralığı kullanılıyor. Önceki 800 dosya farkının 160’ı kapandı. Taze kontrolle 40 ek sonuç ve 15.672 C salt okunur açılış eş. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_295.md).


## V6b.296 — Tcl test kabuğunda sabit saat

Ayrı test host’ları için Tcl bağlı tamsayısı disk VFS saatine yansıtıldı. 77 Tcl linked-int ve 43 harness kontrolü geçti. İki kipte 21 tarih testi daha aynı beklentiyi verdi. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,289 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_296.md).


## V6b.297 — WITHOUT ROWID farklı üst eşik

C’nin farklı işlemciyle elediği aynı anahtarlı kısmi indeks yerine tam indeksin doğru üst sınırı kullanılacak. 80 eski tarama kökü ve 320 taze ilişki sonucu C ile eş. 18.552 salt okunur C bütünlük kontrolü geçti. OFF 1186/1/337, ON 1183/4/337. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,283 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_297.md).


## V6b.298 — Date3 ve func2 normal workspace

İki testin eski ignore satırı kaldırıldı. Dört normal odak koşusu geçti. Ayrı Tcl doğrulamasında her kipte date3 126/126 ve func2 131/131. Tam workspace için iki ek geçen ve iki eksik ignore denetlenir. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_298.md).


## V6b.299 — WITHOUT ROWID INTEGER ve REAL indeks koşulu

INTEGER sorgu ve çıplak REAL kısmi koşulunda C’nin tam indeks seçimi uygulanır. 160 eski, 320 taze ve 80 ek token sonucu eş. 21.768 C salt okunur bütünlük kontrolü geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,227 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_299.md).


## V6b.300 — WITHOUT ROWID farklı anahtarlı REAL koşulu

Farklı ilk sütunlu kısmi indeksin çıplak REAL koşulu C gibi maliyet öncesinde elenir. 160 eski ve 200 ek sonuç C ile aynı. 23.928 C salt okunur bütünlük kontrolü geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_300.md).


## V6b.301 — Doğal SELECT akışında adım saati

Exact birleşik tabanda ölçülen doğal legacy projeksiyonlar zamanı ilk ihtiyaçta ve aynı adım boyunca tek değer olarak alır. Beş sorgu biçiminde 100 C eş sonuç, 209 C salt okunur açılış ve 840 native matris kaydı yeniden doğrulandı. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_301.md).


## V6b.302 — Tarih girdisinde now yazımı

Ortak now girdi kararı çevre boşluğunu kabul etmez. 2.880 satır sonucu ve 900 C salt okunur dosya doğrulandı. Doğal adım saati, date3 ve func2 korumaları geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_302.md).


## V6b.303 — WITHOUT ROWID görünümünde çift eksi koşulu

Aynı anahtardaki iki unary eksi koşulunda C’nin tam aralık kökü kullanılır. 160 eski ve 520 ek sonuç C ile eş. 29.352 C salt okunur bütünlük kontrolü tamamlandı. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_303.md).


## V6b.304 — Sabit ISO tarih BLOB girdisi

Değiştiricisiz kanonik BLOB tarihleri mevcut ISO ayrıştırıcısıyla işlenir. 2.448 C typed sonuç, 3.036 C açılışı ve 36 çıktı imajı eş. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_304.md).


## V6b.305 — VDBE saf tarih çağrısı koruması

OP_PureFunc rolü ve P5 korunur. C’nin reddettiği beş yerleşik zaman çağrısının ölçülen biçimleri değerlendirme öncesi durur. İki rol birim kanıtı, 592 normal ham karşılaştırma ve 209 C açılışı geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_305.md).


## V6b.306 — İşaretsiz Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB içindeki işaretsiz ASCII tamsayı ve ondalık kesir mevcut Julian dönüşümünden geçer. 1.320 doğrudan ve 1.320 satır akışı C sonucu eş. 3.636 C salt okunur açılışı ve 36 çıktı imajı doğrulandı. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_306.md).


## V6b.307 — İşaretli Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB’da tek ASCII işareti mevcut sayı ayrıştırmasına taşınır. Bağlı ve literal yerleşimde 1.152’şer C typed sonuç, 2.916 C açılışı ve 36 çıktı imajı eş. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_307.md).


## V6b.308 — Bileşik CAST görünümü tür bildirimi

Görünüm PRAGMA sütun meta verisinde sayısal baş CAST için C’nin UNION ALL ve çok satırlı VALUES tür kararı taşınır. 336 metadata farkı kapandı, 288 satır ve typeof sonucu korundu. 30 C salt okunur açılışı ve 6 kodlama/günlük imaj grubu eş. Tcl 128/4’ten 130/2’ye geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_308.md).


## V6b.309 — Yerleşik likelihood hazırlama tanısı

Normal SELECT için mevcut C tipi hazırlama sonucundaki geçersiz likelihood olasılığı tanısının kodu, özgün metni ve bayt ofseti taşınır. Yerleşik 126 ve yerel scalar 24 native vaka iki kipte eş. 720 C salt okunur açılışı geçti. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_309.md).


## V6b.310 — Üslü Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB içindeki üslü ASCII sayı mevcut iJD dönüşümünü kullanır. Bağlı ve literal yerleşimde 1.440’ar C sonucu eş, 3.376 salt okunur C açılışı ve 36 çıktı imajı doğrulandı. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_310.md).


## V6b.311 — Bileşik CAST ile CREATE TABLE AS

CTAS sayısal baş CAST için katalogda NUM ve ilk yazımda FLEXNUM kararını taşır. Çok satırlı VALUES için BLOB türü ayrı. 108 fiziksel kayıt farkı kapandı, 36 aday dosyası C ile bayt eş ve 18 native callback profili eş. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_311.md).


## V6b.312 — Julian BLOB ondalık nokta yazımı

Değiştiricisiz BLOB sayı yazımında ondalık noktanın en az bir yanında rakam olması yeterli. Bağlı ve literal yerleşimde 1.368’er C sonucu, 2.400 C açılışı ve 18 kalıcı imaj eş. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,306 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_312.md).


## V6b.313 — Julian BLOB sayısında çevre boşluğu

Mevcut SQLite ASCII boşluk denetimi sayısal BLOB yazımının başına ve sonuna uygulanır. Bağlı ve literal yerleşimde 1.296’şar C sonucu, 1.800 C açılışı ve 18 kalıcı dosya eş. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_313.md).


## V6b.314 — Sabit saat metni içeren tarih BLOB girdisi

Değiştiricisiz sabit saat metni mevcut C uyumlu saat ayrıştırmasına bağlanır. Üç kodlamada bağlı ve literal yerleşimde 1.800’er tipli sonuç C ile eş. 2.340 salt okunur denetim ve 18 dosya imajı doğrulandı. OFF 1188/1/335, ON 1185/4/335. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,232 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_314.md).


## V6b.315 — CAST Tcl paketinin etkinleştirilmesi

Belgelendirilmiş Tcl BLOB bağlama seçenek dizilimi tanındı. CAST iki Rust kipinde 130/132’den 132/132’ye geçti. Var olan CAST paketi normal workspace doğrulamasına alındı. OFF 1189/1/334, ON 1186/4/334. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_315.md).


## V6b.316 — date5, func7 ve func8 test paketleri

Mevcut Tcl paketleri iki kipte 874/874, 73/73 ve 8/8 ile normal koşuya alındı. Takvimde kip başına 2.622 ayrı C sonucu, matematikte 73 ham C bölüm sonucu ve 7 C kullanıcı işlevi sonucu eş. OFF 1192/1/331, ON 1189/4/331. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_316.md).


## V6b.317 — Tek tırnaklı nitelikli sütun adları

C’nin iki ve üç parçalı sütun adı söz dizimi mevcut nitelikli ayrıştırma yoluna bağlandı. 168 ham olay, 18 C salt okunur açılış ve dosya özetleri eş. Bir ayrıştırma regresyon testi eklendi. OFF 1193/1/331, ON 1190/4/331. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_317.md).


## V6b.318 — NOT NULL test paketini etkinleştir

Mevcut NOT NULL paketi iki çalışma kipinde etkin. 106/106 ham Tcl kontrolü, 12 olağan kısıt profili ve 18 C salt okunur dosya kontrolü eş. OFF 1194/1/330, ON 1191/4/330. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_318.md).


## V6b.319 — LIMIT ve OFFSET tam sayı kurallarını C ile eşle

Tek tablo ve tablosuz SELECT yürütmesinde C kayıpsız tam sayı dönüşümü, negatif LIMIT ve sıfır LIMIT sırası uygulandı. 31 geçerli sorgu, kip başına 180 hata girdisi ve 18 C salt okunur açılış eş. LIMIT Tcl paketi iki kipte 122/122 ile etkin. OFF 1195/1/329, ON 1192/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_319.md).


## V6b.320 — Materyalize SELECT LIMIT ve OFFSET değerlerini C ile eşle

Materyalize SELECT ve CTE sınırları kayıpsız tam sayı dönüşümünü kullanır. Üç kodlama ve iki günlük kipinde 510 başlangıç farkı kapandı. Kip başına 540 C karşılaştırması ve 18 C salt okunur açılış eş. Beş davranışı kapsayan yeni test iki çalışma kipinde geçti. OFF 1196/1/329, ON 1193/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,279 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_320.md).


## V6b.321 — Olağan SELECT LIMIT ve OFFSET değerlendirme sırası

Olağan tablolardaki DISTINCT GROUP BY ve JOIN sorguları LIMIT ve OFFSET değerlerini satır hazırlamadan önce değerlendirir. 180 başlangıç farkı kapandı. 54 sorgu, 18 C salt okunur açılış ve beş odaklı Rust karşılaştırması doğrulandı. OFF 1197/1/329, ON 1194/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_321.md).


## V6b.322 — Bileşik SELECT LIMIT ve OFFSET değerlendirme sırası

Olağan tablo kollarındaki UNION ALL UNION INTERSECT ve EXCEPT için 800 ifade sırası farkı kapandı. 88 sorgu biçimi, 18 C salt okunur açılış ve 24 Rust okuyucu/yazıcı çifti C ile eş. Yeni odaklı test iki kipte geçti. OFF 1198/1/329, ON 1195/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,055 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_322.md).


## V6b.323 — FROM alt sorgularında LIMIT değerlendirme sırası

Olağan FROM alt sorgularında LIMIT ve OFFSET satır üretiminden önce değerlendirilir. 360 fark kapandı. 93 sorgu, 18 C salt okunur açılış ve beş odaklı Rust karşılaştırması doğrulandı. OFF 1199/1/329, ON 1196/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,306 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_323.md).


## V6b.324 — Özyinelemesiz CTE LIMIT değerlendirme sırası

Özyinelemesiz CTE için LIMIT ve OFFSET satır üretiminden önce birer kez değerlendirilir. 330 fark kapandı. 62 sorgu biçimi, 18 C salt okunur açılış ve altı Rust kontrolü doğrulandı. OFF 1200/1/329, ON 1197/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,297 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_324.md).


## V6b.325 — UNION ALL ve dolan LIMIT

Pozitif LIMIT ve OFFSET için yeterli satır oluştuğunda olağan UNION ALL kolları için gereksiz satır değerlendirmesi yapılmaz. 50 fark kapandı. 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. OFF 1201/1/329, ON 1198/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_325.md).


## V6b.326 — Sıralı özyinelemeli CTE LIMIT değerlendirme sırası

VALUES başlangıçlı ve tek özyinelemeli kollu sıralı CTE'lerde dış LIMIT ve OFFSET satır üretiminden önce değerlendirilir. 200 fark kapandı. 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. OFF 1202/1/329, ON 1199/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_326.md).


## V6b.327 — Özyinelemeli CTE akışında LIMIT ve OFFSET

Özyinelemeli akış LIMIT ve OFFSET değerlerini birer kez değerlendirir. 160 sıralamasız fark kapandı. 56 sorgu biçimi, 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. OFF 1203/1/329, ON 1200/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_327.md).


## V6b.328 — Özyinelemeli CTE akışında WHERE çağrı sayısı

Sınırlı olağan özyinelemeli CTE akışında WHERE aynı satır için bir kez çalışır. Beş çağrı sayısı farkı kapandı. 62 sorgu biçimi ve 18 C salt okunur açılış aynı, altı Rust kontrolü iki kipte geçti. OFF 1204/1/329, ON 1201/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,252 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_328.md).


## V6b.329 — UNION eşdeğer satır temsili

Materyalize UNION aynı kolda ilk temsilciyi korur ve eşitlikte sağ kolu seçer. 120 değer ve tür farkı kapandı. 27 sorgu biçimi, 18 C salt okunur açılış ve altı Rust kontrolü doğrulandı. OFF 1205/1/329, ON 1202/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_329.md).


## V6b.330 — Sıralamasız UNION ve sabit LIMIT 1

İki olağan koldaki sıralamasız UNION sabit tamsayı LIMIT 1 ile kolları kaynak sırasıyla değerlendirir. 120 fark kapandı. 25 sorgu biçimi ve 18 C salt okunur açılış doğrulandı. OFF 1206/1/329, ON 1203/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_330.md).


## V6b.331 — GROUP BY anahtarının sonuç sütununda kullanımı

GROUP BY anahtarını sonuç sütununda kullanma, takma ada bağlı doğrudan COLLATE ifadesinin ayrı değerlendirilmesini koruyacak biçimde doğrulandı. 18 C ve Rust kontrolü, 17 olağan SQL biçimi ve 18 C salt okunur açılışı doğrulandı. OFF 1207/1/329, ON 1204/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_331.md).


## V6b.332 — MIN/MAX ile gruplama anahtarının kullanımı

MIN/MAX ile hesaplanmış GROUP BY anahtarının sonuçta kullanımı C ile eş. 20 yerel fark kapandı. On C ve Rust kontrolü, 24 olağan SQL biçimi ve 18 C salt okunur açılışı doğrulandı. OFF 1208/1/329, ON 1205/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_332.md).


## V6b.333 — GROUP BY anahtarında collation aktarımı

Tekli artı ve CAST ile sarılmış sütunlar ve açık COLLATE ifadeleri, gruplamada C ile aynı karşılaştırma kuralını taşır. Üç kodlama, DELETE ve WAL içinde 120 fark kapandı. C ve iki Rust kipi yedi odaklı kontrolde eş. OFF 1209/1/329, ON 1206/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,275 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_333.md).


## V6b.334 — GROUP BY sonucunda MIN/MAX girdisinin temsili

MIN/MAX ile döndürülen grup sütunu C’nin seçtiği girdi değerini taşıyor. Önceki 130 düzeltme korundu, 30 INTEGER/REAL sınır farkı kapandı. İlk adayda bozulan integer kernel testi düzeltilen adayda geçti. On bir odaklı C ve Rust kontrolü doğrulandı. OFF 1210/1/329, ON 1207/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_334.md).


## V6b.335 — Skaler işlev içindeki MIN/MAX ve grup satırı

Skaler işlev içinde aynı SELECT kapsamında çağrılan MIN/MAX, grup satırını C ile aynı şekilde seçiyor. Üç kodlama ve iki journal kipinde 140 fark kapandı. Bağlantıya özel MAX ve ayrı alt sorgu dahil dokuz odaklı kontrol geçti. OFF 1211/1/329, ON 1208/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_335.md).


## V6b.336 — Tekrarlanan sütun MIN/MAX işleminde grup satırı

Filtresiz tek sütun MIN/MAX tekrarları C gibi ilk akümülatör konumunu koruyor. Üç kodlama ve iki journal kipinde 90 fark kapandı. Ayrı DISTINCT, FILTER ve farklı literal yazımları dahil sekiz odaklı kontrol geçti. OFF 1212/1/329, ON 1209/4/329. DML OFF 391/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_336.md).


## V6b.337 — Yabancı anahtarlı WITHOUT ROWID kaynağında kapsayan indeks

Kapsama hesabı yabancı anahtar tanımlı WITHOUT ROWID kaynaklarında C ile aynı taramayı seçiyor. 54 profilde 420 fark kapandı ve 162 C salt okunur yeniden açılışı doğrulandı. DML OFF 6189 kapandı, kalan sekiz ham kayıt aynı. OFF 1213/1/329, ON 1210/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_337.md).


## V6b.338 — LIMIT ile WITHOUT ROWID kapsayan tam tarama

Filtresiz ve sıralamasız tek tablo okumalarında LIMIT ve OFFSET C ile aynı kapsayan taramayı seçiyor. 54 profilde 590 fark kapandı ve 162 C yeniden açılışı doğrulandı. Sekiz odaklı C ve Rust kontrolü geçti. OFF 1214/1/329, ON 1211/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,292 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_338.md).


## V6b.339 — ANALYZE sonrasında WITHOUT ROWID kapsayan tarama

Normal ANALYZE istatistikleri filtresiz WITHOUT ROWID taramasında C gibi kullanılıyor. 96 profilde 790 fark kapandı ve 288 C yeniden açılışı doğrulandı. Altı odaklı C ve Rust kontrolü geçti. OFF 1215/1/329, ON 1212/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_339.md).


## V6b.340 — Ayrı indeks ANALYZE istatistiklerinin kullanımı

Tek indeks ANALYZE sonrasında C’nin varsayılan satır tahmini uygulanıyor. Ölçülen 120 profildeki 590 fark kapandı. Yazılan dosya, kalıcı sıra ve 360 C yeniden açılışı eş. OFF 1216/1/329, ON 1213/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_340.md).


## V6b.341 — reverse_unordered_selects ile tam rowid taraması

Filtresiz ve sıralamasız olağan MAIN rowid taramalarında reverse_unordered_selects imleci ters yürütüyor. Üç kodlama ve iki journal kipinde 20 fark kapandı. 18 C yeniden açılışı ve altı odaklı kontrol doğrulandı. OFF 1217/1/329, ON 1214/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_341.md).


## V6b.342 — reverse_unordered_selects ile olağan CROSS JOIN

İki indekssiz MAIN rowid kaynağı arasındaki filtresiz CROSS JOIN, reverse_unordered_selects açıkken kaynakları C sırasıyla okuyor. 20 fark kapandı. 18 C yeniden açılışı ve altı odaklı kontrol doğrulandı. OFF 1218/1/329, ON 1215/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_342.md).


## V6b.343 — reverse_unordered_selects ile yıldızlı SELECT

Yıldız ve nitelikli yıldız ifadeleri olağan tek tablo ve CROSS JOIN okumalarında C ters sırasını koruyor. 70 fark kapandı. 18 C yeniden açılışı ve altı odaklı kontrol doğrulandı. OFF 1219/1/329, ON 1216/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,195 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_343.md).


## V6b.344 — INTEGER PRIMARY KEY eşitliğinde ters JOIN taraması

Nitelikli INTEGER PRIMARY KEY eşitliğinde olağan INNER ve LEFT JOIN, reverse_unordered_selects açıkken C ile aynı satır sırasını veriyor. 60 fark kapandı. 18 C yeniden açılışı ve yedi odaklı kontrol doğrulandı. OFF 1220/1/329, ON 1217/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_344.md).


## V6b.345 — INTEGER PRIMARY KEY aralığında ters LEFT JOIN taraması

34 SELECT biçimi ve altı profilde 60 aralık sırası farkı kapandı. 18 C yeniden açılışı ve yedi odaklı kontrol geçti. OFF 1221/1/329, ON 1218/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_345.md).


## V6b.346 — INTEGER PRIMARY KEY için ters USING JOIN taraması

50 SELECT biçimi ve altı profilde 70 sıra farkı kapandı. 18 C yeniden açılışı ve yedi odaklı kontrol geçti. OFF 1222/1/329, ON 1219/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,259 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_346.md).


## V6b.347 — Sol INTEGER PRIMARY KEY filtresiyle ters LEFT JOIN taraması

72 SELECT biçimi ve altı profilde 70 sıra farkı kapandı. 18 C yeniden açılışı ve sekiz odaklı kontrol geçti. OFF 1223/1/329, ON 1220/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_347.md).


## V6b.348 — WITHOUT ROWID görünüm UPDATE ve bağımsız tam indeksler

96 profilde 100 sıra veya ana dosya farkı kapandı. İki uygun indeksin bulunduğu 24 profil öncekiyle eş. 360 C yeniden açılışı ile altı odaklı kontrol geçti. OFF 1224/1/329, ON 1221/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_348.md).


## V6b.349 — WITHOUT ROWID görünüm UPDATE RETURNING için REAL aralık

288 profilde 360 sıra veya dosya farkı kapandı. 864 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1225/1/329, ON 1222/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_349.md).


## V6b.350 — WITHOUT ROWID görünüm UPDATE RETURNING sütun projeksiyonları

168 profilde 150 sıra veya dosya farkı kapandı. 504 C yeniden açılışı ve beş odaklı kontrol geçti. OFF 1226/1/329, ON 1223/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_350.md).


## V6b.351 — WITHOUT ROWID görünüm UPDATE ve tek uygun UNIQUE aralık indeksi

120 profilde 100 sıra veya dosya farkı kapandı. 360 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1227/1/329, ON 1224/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_351.md).


## V6b.352 — WITHOUT ROWID görünüm UPDATE RETURNING için iki taraflı aralık

180 profilde 120 sıra veya dosya farkı kapandı. 540 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1228/1/329, ON 1225/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_352.md).


## V6b.353 — WITHOUT ROWID görünüm UPDATE ve aynı anahtara uygun iki tam indeks

180 profilde 120 sıra veya dosya farkı kapandı. 540 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1229/1/329, ON 1226/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_353.md).


## V6b.354 — WITHOUT ROWID görünüm UPDATE RETURNING için BETWEEN aralığı

240 profilde 80 sıra veya dosya farkı kapandı. 720 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1230/1/329, ON 1227/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_354.md).


## V6b.355 — WITHOUT ROWID görünüm UPDATE RETURNING için aynı yönlü sınırlar

420 profilde 280 sıra veya dosya farkı kapandı. 1260 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1231/1/329, ON 1228/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,274 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_355.md).


## V6b.356 — WITHOUT ROWID görünüm UPDATE RETURNING için metin biçimli sayısal sınır

450 profilde 480 sıra veya ana dosya farkı kapandı. 1350 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1232/1/329, ON 1229/4/329. DML OFF 392/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,319 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_356.md).


## V6b.357 — WITHOUT ROWID görünüm UPDATE ve UNIQUE kısıtının otomatik indeksi

120 profilde 140 sıra veya ana dosya farkı kapandı. 360 C yeniden açılışı ve dört odaklı kontrol geçti. 6138 OFF korpusunda C ile eşleşti. OFF 1233/1/329, ON 1230/4/329. DML OFF 393/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_357.md).


## V6b.358 — WITHOUT ROWID görünüm UPDATE RETURNING sırasında birincil anahtar filtresi

330 profilde 90 sıra veya dosya farkı kapandı. 990 C yeniden açılışı ve dört odaklı kontrol geçti. OFF 1234/1/329, ON 1231/4/329. DML OFF 393/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_358.md).


## V6b.359 — Rowid görünüm UPDATE RETURNING sırasında birleşik metin ve tamsayı aralığı

144 profilde 60 sıra veya dosya farkı kapandı. 432 C yeniden açılışı ve dört odaklı kontrol geçti. 6392 OFF korpusunda C ile eşleşti. OFF 1235/1/329, ON 1232/4/329. DML OFF 394/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,248 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_359.md).


## V6b.360 — WITHOUT ROWID UPDATE OR ABORT yabancı anahtar hazırlama tanısı

42 profilde 30 salt okunur tanı farkı kapandı. 6088 OFF korpusunda C ile eşleşti. OFF 1236/1/329, ON 1233/4/329. DML OFF 395/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,280 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_360.md).


## V6b.361 — WITHOUT ROWID UPDATE yabancı anahtar hazırlama tanısı

42 profilde 120 salt okunur tanı farkı kapandı. 6184 OFF korpusunda C ile eşleşti. OFF 1237/1/329, ON 1234/4/329. DML OFF 396/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,305 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_361.md).


## V6b.362 — WITHOUT ROWID UPDATE yabancı anahtar eylem tanısı

Üst tablo UPDATE OR IGNORE ve UPDATE OR FAIL için hazırlama tanı önceliği C ile eşleşti. 42 profil ve 504 salt okunur kontrolde 80 fark kapandı. OFF 1238/1/329, ON 1235/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,280 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_362.md).


## V6b.363 — Rowid görünüm UPDATE karma aralıkta tetikleyici sırası

240 profil ve 720 C yeniden açılışında 90 sıra veya dosya farkı kapandı. OFF 1239/1/329, ON 1236/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_363.md).


## V6b.364 — Rowid görünüm UPDATE karma aralığında tek indeks

Tam INTEGER ve uygun kısmi TEXT indeksinin ayrı kullanıldığı 240 profilde 100 fark kapandı. OFF 1240/1/329, ON 1237/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_364.md).


## V6b.365 — Rowid görünüm UPDATE kısmi indeks koşulu

Literal koşul eşliği ve IS NOT NULL çıkarımı C ile eşleşti. 360 profil ve 250 kapanış doğrulandı. OFF 1241/1/329, ON 1238/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_365.md).


## V6b.366 — Kaldırılan sütun karşılaştırma kuralının hazırlama tanısı

72 hazırlama karşılaştırması C ile eşleşti. 36 C yeniden açılışı ve DB dosyası doğrulandı. OFF 1242/1/329, ON 1239/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,275 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_366.md).


## V6b.367 — Rowid görünüm DELETE karma aralığında tetikleyici sırası

216 görünüm silme profilinde 300 fark kapandı. 648 C yeniden açılışı doğrulandı. OFF 1243/1/329, ON 1240/4/329. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_367.md).


## V6b.368 — Tcl karşılaştırma işlevi ve sonlandırma geri çağrısı

C ve iki Rust kipinde eski collate7 kapsamı ve üç kodlamadaki 24 ek yaşam döngüsü kontrolü geçti. OFF 1245/1/328, ON 1242/4/328. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_368.md).


## V6b.369 — Satır değerli CASE karşılaştırması

İki ve üç öğeli CASE karşılaştırmasında 100 fark kapandı. Adım, NULL, karşılaştırma sırası ve geçersiz genişlik tanıları C ile eş. OFF 1246/1/328, ON 1243/4/328. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_369.md).


## V6b.370 — Varsayılan, ABORT ve ROLLBACK UPDATE hazırlama tanısı

Yabancı anahtar eylem önceliği üç ek çatışma kipinde C ile eşleşti. 882 salt okunur kontroldeki 120 tanı farkı kapandı. OFF 1247/1/328, ON 1244/4/328. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_370.md).


## V6b.371 — REPLACE UPDATE hazırlama tanısı

REPLACE’in erken DELETE denetiminden sonra C’nin son tanı önceliği eşleşti. 1.134 salt okunur kontrolde 40 fark kapandı. OFF 1248/1/328, ON 1245/4/328. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,300 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_371.md).


## V6b.372 — UPDATE test paketinin etkinleştirilmesi

C ve iki Rust kipinde 141 UPDATE kontrolü geçti. Pencere sorgusunun planı ve 72 C yeniden açılışı eş. OFF 1249/1/327, ON 1246/4/327. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_372.md).


## V6b.373 — Alt sorgulu satır CASE adım davranışı

İki Rust kipinde salt okunur satır CASE alt sorguları gerçek adım sınırlarında yürütülüyor. Altı disk profilinde 144 ifade sonucu C ile karşılaştırıldı. Önceki 60 fark kapandı. Odaklı C ve Rust adım kontrolleri geçti. Özgün rowvalue8 paketi workspace koşusuna alındı. OFF 1251/1/326, ON 1248/4/326. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_373.md).


## V6b.374 — UTF-16 alt sorgulu satır CASE sıralayıcısı

Yerleşik BINARY sıralayıcısı kullanan UTF-16 CASE alt sorguları iki Rust kipinde yürütülüyor. 12 disk profilinde 720 gözlem C ile karşılaştırıldı. Önceki 80 fark kapandı. C ve Rust adım, sıralama ve yeniden hazırlama kontrolleri geçti. OFF 1252/1/326, ON 1249/4/326. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_374.md).


## V6b.375 — Canlı iki tablo SELECT imleçleri

Aynı bağlantıda olağan UPDATE veya DELETE sonrası doğrudan iki rowid tablosunun SELECT adımları C ile eşleşiyor. Exact API gözleminde iki kipte 72/72, 48 bayt eş ana dosya grubu ve 144 başarılı C yeniden açılışı. Özgün delete2 paketi çalışma alanı koşusuna alındı. OFF 1253/1/325, ON 1250/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_375.md).


## V6b.376 — Kalıcı görünümde satır CASE

Kalıcı görünümden gelen alt sorgulu satır CASE adım davranışı iki Rust kipinde C ile eşleşiyor. Exact 600 gözlemde 110 fark kapandı. 12 C yeniden açılışı ve görünüm, derinlik, yetkilendirme ve kolasyon korumaları doğrulandı. OFF 1254/1/325, ON 1251/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_376.md).


## V6b.377 — UTF-16 BINARY satır CASE gruplaması

Salt okunur satır CASE sorgularında BINARY grup karşılaştırmaları kendi UTF-16 kodlamasında yürütülüyor. 12 profilde 432 gözlem ve 80 kapanan fark doğrulandı. Diğer sonuçlar ve dosya özetleri aynı. İki Rust kipi ve C odak testleri geçti. OFF 1255/1/325, ON 1252/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,254 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_377.md).


## V6b.378 — UTF-16 BINARY satır CASE tekilleştirmesi

Sırasız geçici BINARY anahtar kullanan salt okunur CASE sorgularında 720 gözlem ve 80 kapanan fark doğrulandı. Sonradan eklenmiş TEXT DEFAULT değerlerini okuyan eski fiziksel satırlar için 24 ek C eşleşmesi ve 12 C açılışı geçti. OFF 1256/1/325, ON 1253/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_378.md).


## V6b.379 — CTE ve ifade alt sorgusu üzerinden görünüm CASE eşliği

Gerçekten başvurulan özyinelemesiz CTE ve ifade alt sorgularında kalıcı görünüm çözümü korunuyor. C ve iki Rust kipinde 660 gözlemde 130 fark kapandı. Altı C açılışı ve önceki DEFAULT testi geçti. OFF 1257/1/325, ON 1254/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,283 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_379.md).


## V6b.380 — Eski satırda sütun varsayılanının metin kodlaması

Sonradan eklenmiş TEXT DEFAULT sütununu eski fiziksel kayıttan okuyan VDBE, metni bağlantının veritabanı kodlamasına getiriyor. 324 gözlemde 16 C farkı kapandı. Önceki DISTINCT geçişinin 24 Rust sonucu korundu. OFF 1258/1/325, ON 1255/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_380.md).


## V6b.381 — TEMP görünümünde satır CASE

TEMP görünümü, ana ve bağlı şemalarla C ad çözümlemesini koruyor. 510 gözlemde 80 fark kapandı. 12 C açılışı, görünüm devre dışı bırakma ve yetkilendirme korumaları, bekleyen sorgunun ana görünüme yeniden hazırlanması geçti. OFF 1259/1/325, ON 1256/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_381.md).


## V6b.382 — UTF-16 BINARY fiziksel indeks araması

Ana veritabanında BINARY anahtarlı CASE okumalarında karma kodlamalı arama anahtarları C ile eşleşiyor. 756 fiziksel gözlemde 240 fark kapandı. 360 karma anahtar karşılaştırması, 60 olağan yazma ve 24 bayt eş C dosya imajı doğrulandı. OFF 1260/1/325, ON 1257/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_382.md).


## V6b.383 — Toplama ve pencere sonucu metin kodlaması

VDBE toplama ve pencere metin sonuçlarını veritabanının kodlamasına getiriyor. 504 gözlemde 112 C bayt farkı kapandı ve 392 önceki eşleşme korundu. Fiziksel indeks ve DISTINCT kontrolleri eş. OFF 1261/1/325, ON 1258/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_383.md).


## V6b.384 — Skaler WHEN ile satır CASE hazırlama tanısı

Çok sütunlu alt sorgu ile skaler WHEN karşılaştırmasında C hazırlama tanısı korunuyor. Ana 558 gözlemde 10 ve ek 144 gözlemde 30 fark kapandı. Skaler sonuçlar, tanımsız WHEN adı ve 24 API koruma gözlemi aynı. OFF 1262/1/325, ON 1259/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_384.md).


## V6b.385 — Özyinelemeli CTE içinde görünüm CASE

Özyinelemeli CTE tanımı kendi açık başvurusunu tekrar izlemeksizin başlangıç ve özyineleme kollarındaki görünümleri çözüyor. Ölçülen 40 farkın 32’si kapandı. Diğer sonuçlar ve altı C yeniden açılışı eş kaldı. OFF 1263/1/325, ON 1260/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_385.md).


## V6b.386 — UTF-16 BINARY CASE üyeliği

Salt okunur CASE alt sorgusunda BINARY IN ve NOT IN anahtarları aynı kodlamayla karşılaştırılıyor. 558 gözlemde 32 fark kapandı. Altı C yeniden açılışı ve NULL, boş sonuç, satır üyeliği ve yeniden hazırlama kontrolleri geçti. OFF 1264/1/325, ON 1261/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_386.md).


## V6b.387 — UTF-16 CASE gruplamasında yerleşik kolasyonlar

Aynı gruplama anahtarını paylaşan sıralayıcı ve karşılaştırma adımlarında NOCASE ve RTRIM C kodlamasıyla yürütülüyor. 558 gözlemde 24 fark kapandı. Uygulamanın kayıtlı karşılaştırıcıları ve yetkilendirme kontrolleri korunuyor. OFF 1265/1/325, ON 1262/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_387.md).


## V6b.388 — UTF-16 satır CASE anahtarları

558 gözlemli exact karşılaştırmada son 48 fark kapandı. FIFO UNION, BINARY TOP-N LIMIT/OFFSET, NOCASE fiziksel arama ve NOCASE/RTRIM DISTINCT düzeltildi. İki kipte 372/372 Rust sonucu ve altı C salt okunur dosya açılışı eş. Daha geniş 1.188 gözlemde önceki eş sonuçlar korundu. OFF 1269/1/325, ON 1266/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,209 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_388.md).


## V6b.389 — UTF-16 yerleşik satır CASE sıralaması

Tam ORDER BY ve LIMIT/OFFSET NOCASE/RTRIM sonuçları C ile eş. 756, 576 ve 612 gözlemli üç matris 320 farkı kapattı. Eşit anahtarların sıralama kontrolünde 384 Rust sonucu C ile eş ve 256 ek fark kapandı. Önceki 558 sonuç ve yol izi aynı. Pinned C 28/28, iki Rust kipi rowvalue8 21/21 ve delete2 1/1. OFF 1270/1/325, ON 1267/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_389.md).


## V6b.390 — Fiziksel indeks üzerinden satır CASE gruplaması

Exact V6b.389 tabanıyla 684 gözlemde fiziksel NOCASE indeksinin beslediği 32 gruplanmış sonuç C ile eşleşti. Önceki 558 CASE kaydı ve bağlantı kolasyonu korumaları eş. C fixture 10/10, Rust rowvalue8 22/22 ve delete2 1/1 iki kipte. OFF 1271/1/325, ON 1268/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_390.md).


## V6b.391 — UTF-16 yerleşik satır CASE üyeliği

510 gözlemde yerleşik NOCASE ve RTRIM kullanan skaler ve satır IN/NOT IN için 80 fark kapandı. Önceki 558 gözlem ve gerekçeleri aynı. C fixture 13/13 ve dört odak Rust testi iki kipte geçti. Bağlantıya kayıtlı kolasyon ve authorizer kontrolü korundu. OFF 1272/1/325, ON 1269/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_391.md).


## V6b.392 — Özyinelemeli FIFO UNION yerleşik kolasyonları

300 gözlemde NOCASE/RTRIM ve karma büyük kayıtlarla 40 UTF-16 özyinelemeli FIFO sonucu C ile eşleşti. Önceki 558 gözlem ve bağlantı korumaları aynı. C fixture 13/13, Rust rowvalue8 24/24 ve delete2 1/1 iki kipte. OFF 1273/1/325, ON 1270/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_392.md).


## V6b.393 — UTF-16 satır CASE kısmi sıralaması

Kısmi ORDER BY ve LIMIT/OFFSET iki matriste 288 UTF-16 sonucunu C ile eşitledi. Önceki 558 gözlem, C planları ve koruma yolları aynı. C 13/13, Rust rowvalue8 25/25 ve delete2 1/1 iki kipte. OFF 1274/1/325, ON 1271/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_393.md).


## V6b.394 — UTF-16 sabit rowid listesi taraması

660 gözlemde tamsayı eğilimli tek alanlı sabit rowid listeleri için 48 UTF-16 sonucu C ile eşleşti. Önceki 558 envanter gözlemi ve üyelik kontrolleri aynı. C 22/22, beş odak Rust testi iki kipte ve bağlantı korumaları geçti. OFF 1275/1/325, ON 1272/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,242 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_394.md).


## V6b.395 — Özyinelemeli satır CASE öncelik kuyruğu

360 gözlemde özyinelemeli öncelik kuyruğu için 56 UTF-16 sonucu C ile eşleşti. 558 seçili gözlem aynı. V6b.388 tabanına karşı 2.052 geniş gözlemde kalan 432 fark kapandı. C 13/13 ve Rust rowvalue8 27/27, delete2 1/1 iki kipte. OFF 1276/1/325, ON 1273/4/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,262 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_395.md).


## V6b.396 — Üç parçalı RETURNING joker tanısı

144 Rust native sonucu içinde 20 wildcard tanısı C ile aynı hata kodu, mesaj ve byte ofsetine geldi. Ek 42 vaka iki kipte eş. Mevcut Group64 testi C referansıyla iki kipte geçti, tüm dosya SHA ve yeniden açılışlar eş. OFF 1276/1/325, ON 1274/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,256 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_396.md).


## V6b.397 — UTF-16 satır CASE fiziksel RTRIM indeksleri

480 gözlemde fiziksel RTRIM aramaları ve indeks üzerinden gruplama 104 UTF-16 sonucunu eşitledi. C ile 312 plan eş, önceki 558 gözlem korundu. C fixture 19/19, ilişkili dört Rust testi iki kipte geçti. OFF 1277/1/325, ON 1275/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,260 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_397.md).


## V6b.398 — RETURNING nitelikli ad hata ofsetleri

Önceki 72 native vaka iki kipte 72/72. Ek 198 vakada 108 OFF ve 72 ON UTF-8 bayt ofseti C ile eşleşti. 594 ana dosya ve C yeniden açılışı eş. Joker, authorizer ve önceki RTRIM akışları korundu. OFF 1278/1/325, ON 1276/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_398.md).


## V6b.399 — UTF-16 hesaplanan rowid listeleri

390 gözlemde aritmetik ve yerleşik kolasyonlu rowid listesi için 64 UTF-16 farkı kapandı. Önceki 558 gözlem aynı. C fixture 10/10, Rust rowvalue8 29/29 ve delete2 1/1 iki kipte. OFF 1279/1/325, ON 1277/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_399.md).


## V6b.400 — UTF-16 satır CASE tamsayı kümesi aramaları

390 gözlemde sabit SELECT kümesi ve ikincil indeks aramaları için 56 sonuç C ile eşleşti. Önceki 558 gözlem aynı. C fixture 13/13, Rust rowvalue8 30/30 ve delete2 1/1 iki kipte. OFF 1280/1/325, ON 1278/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_400.md).


## V6b.401 — UTF-16 çok indeksli OR kursörü

420 gözlemde çok indeksli OR için 72 UTF-16 sonucu ve 108 C planı eşleşti. Önceki 558 gözlem ve fiziksel korumalar aynı. C fixture 22/22 ve dört Rust odak testi iki kipte geçti. OFF 1281/1/325, ON 1279/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,293 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_401.md).


## V6b.402 — UTF-16 sabit VALUES kümesi aramaları

300 gözlemde sabit VALUES kümesi için 40 UTF-16 sonucu C ile eşleşti. Önceki 558 kayıt ve altı dosya açılışı eş. C fixture 10/10 ve Rust rowvalue8 32/32, delete2 1/1 iki kipte. OFF 1282/1/325, ON 1280/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,233 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_402.md).


## V6b.403 — Uygulama kayıtlı skaler kolasyonları

Tek satırlı kaynak tablosuz karşılaştırmalarda kayıtlı NOCASE ve RTRIM çağrıları iki kipte ayrı ayrı 696/696 C eş. Group64 5/5, hata ve reset davranışı geçti. Altı sağlıklı DELETE/WAL imajı ve C yeniden açılışı eş. OFF 1283/1/325, ON 1281/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_403.md).


## V6b.404 — UTF-16 karma rowid ve indeks OR

840 gözlemde karma ana rowid ve ikincil indeks OR için 128 sonuç C ile eş. 192 tam C planı ve önceki 558 gözlem aynı. C fixture 19/19 ve dört Rust odak testi her kipte geçti. OFF 1284/1/325, ON 1282/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_404.md).


## V6b.405 — Skaler doğruluk projeksiyonunda kayıtlı kolasyon

Karma IS TRUE projeksiyonlarında önceki altı fark kapandı. Her Rust kipinde 1.374 C kayıt ve 33 projeksiyon kontrolü eş. Önceki 696 geri çağrı kaydı korundu. Group64 6/6, hata/reset ve sağlıklı dosya açılışları geçti. OFF 1285/1/325, ON 1283/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,258 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_405.md).


## V6b.406 — UTF-16 WITHOUT ROWID iki dallı OR

İki fiziksel OR dalı ve bileşik PK için ölçülen 144 UTF-16 sonuç C ile eş. Exact OFF ve ON rowvalue8 34/34, C fixture 16/16. 216 tam C planı ve önceki 558 gözlem eş. OFF 1286/1/325, ON 1284/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_406.md).


## V6b.407 — Özel adlı kolasyonun tam bellek sırası

Her Rust kipinde 180 dar bellek sıralama profili ve 12.708 callback/adım optimize C ile eş. İlk callback hatası özgün kodla döndü. Group64 7/7, altı DELETE/WAL imajı ve C yeniden açılışları korundu. OFF 1287/1/325, ON 1285/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_407.md).


## V6b.408 — Üç ikincil indeksli WITHOUT ROWID OR

Tam bileşik birincil anahtar, üç kolda aynı geçici indeksle yönetiliyor. Ortadaki kol önce arıyor ve yalnız yeni anahtarı mevcut arama konumuna ekliyor. 176 UTF-16 sonucu ve 264 tam EXPLAIN planı C ile eşleşti. Önceki iki kollu sonuçlar, C yeniden açma kontrolleri ve uygulama karşılaştırıcısı sınırları korundu. OFF 1288/1/325, ON 1286/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_408.md).


## V6b.409 — Kayıtlı NOCASE ve RTRIM bellek sıralaması

Açık NOCASE ve RTRIM ile tam bellek sıralamasında uygulama karşılaştırıcıları her Rust kipinde 360 profil ve 25.416 adım boyunca C ile eşleşti. Aynı deyimde FILE ve MEMORY geçişi, callback argüman sırası ve özgün hata aktarımı doğrulandı. 72 kapsam dışı sıralama profili önceki davranışını korudu. OFF 1289/1/325, ON 1287/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,255 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_409.md).


## V6b.410 — Birincil ve ikincil WITHOUT ROWID OR kolları

Birincil B-tree kolu bileşik anahtarı tablo imlecinden alıyor. İkincil kol aynı anahtar alanlarıyla tabloda konumu doğruluyor. 160 UTF-16 sonucu ve 240 tam EXPLAIN planı C ile eşleşti. İki kolun sırası, aralık genişlikleri, NULL bağlama ve C yeniden açma denetlendi. OFF 1290/1/325, ON 1288/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,261 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_410.md).


## V6b.411 — Çok kollu ikincil indeks OR

Tüm ikincil kollar aynı tam bileşik anahtarı ve ilk, orta, son akışını kullanıyor. Dört, beş, altı ve sekiz kollu C biçimlerinde 208 UTF-16 sonucu ve 312 tam EXPLAIN planı eşleşti. Önceki iki ve üç kollu sonuçlar ile birincil ve ikincil iki kollu kontrol korundu. OFF 1291/1/325, ON 1289/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_411.md).


## V6b.412 — Çok kollu birincil ve karışık OR

Birincil ve ikincil kollar C planındaki adım sınırlarıyla aynı geçici anahtar kümesini kullanıyor. Dokuz biçim 144 UTF-16 sonucu ve 216 tam EXPLAIN planını eşledi. Bağımsız birincil artı üç ikincil tanığı ek 16 sonucu ve 24 planı kapattı. Önceki 312 geniş ikincil plan korunuyor. OFF 1292/1/325, ON 1290/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_412.md).


## V6b.413 — Açık UTF-8 BINARY bellek sıralaması

Kayıtlı UTF-8 BINARY karşılaştırıcısı C ile her Rust kipinde 4.236 adım ve 36 Unicode/NUL gözleminde eş. Native UTF-16 sıraları C ile karşılaştırıcısız eşleşti. Karşılaştırma hatasının özgün kodu ve ofseti, aynı deyim FILE/MEMORY geçişi ve altı dosyanın baytları doğrulandı. OFF 1293/1/325, ON 1291/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_413.md).


## V6b.414 — Örtük UTF-8 BINARY bellek sıralaması

Varsayılan veya BINARY tanımlı düz sütunda 132 profil ve 9.264 geri çağrı adımı her Rust kipinde C ile eş. Doğal UTF-16 ve önceki 864 açık sıralama profili korunuyor. Hata sırası, yeniden hazırlama ve 12 C dosyası doğrulandı. OFF 1294/1/325, ON 1292/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,249 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_414.md).


## V6b.415 — Koşullu WITHOUT ROWID OR kolları

Yerel sayısal ve sıralı bileşik filtreler anahtar kopyasından önce çalışıyor. Dört biçimde 64 UTF-16 sonucu ve 96 tam EXPLAIN planı eşleşti. Önceki 14 biçim, uygulama karşılaştırıcısı ve C yeniden açışı korundu. OFF 1295/1/325, ON 1293/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,263 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_415.md).


## V6b.416 — Örtük şema kolasyonu bellek sıralaması

Şemadan gelen NOCASE, RTRIM ve uygulama kolasyonu tam bellek sıralamasında her Rust kipinde 222 profil ve 15.648 adım boyunca C ile eşleşti. Unicode ve NUL için 216 ek kayıt, özgün hata ve 18 DELETE/WAL dosyası doğrulandı. Aynı kaynakla 180 olağan ertelenmiş yabancı anahtar çalıştırması da C ile eş. OFF 1296/1/325, ON 1294/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_416.md).


## V6b.417 — PMA açmayan özel adlı FILE sıralaması

Sabit C varsayılan PMA tabanı altında kalacağı kayıt baytlarından doğrulanan FILE sıralaması aynı karşılaştırıcı sırasını kullanıyor. Her Rust kipinde 136 profil C ile eşleşti ve 112 karşılaştırıcı dizisi kapandı. Tam callback argümanları, zorlanan tahsis ve gerçek C PMA açma denetimleri ile 180 olağan ertelenmiş yabancı anahtar çalıştırması doğrulandı. OFF 1297/1/325, ON 1295/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,269 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_417.md).


## V6b.418 — IN listeli WITHOUT ROWID OR

V6b.417 tabanındaki 21 hedef biçimde 336 UTF-16 farkı kapandı. 504 tam EXPLAIN planı, 24 deyim yaşam döngüsü kaydı ve 144 API davranışı kaydı doğrulandı. Birleşik ikilinin 528 eski kontrol planı C ve V6b.417 ile eşleşti. OFF 1298/1/325, ON 1296/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_418.md).


## V6b.419 — Alt sorgu ve birden çok IN listeli WITHOUT ROWID OR

İki sabitlenmiş alt sorgu biçimi, tam sayı aritmetiği ve bağımsız liste kollarında eski 64 UTF-16 farkı ile 48 komşu fark kapandı. 168 tam hedef planı C ile eşleşti. Önceki IN, filtre, yetkilendirici, kolasyon ve dosya denetimleri korundu. OFF 1299/1/325, ON 1297/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_419.md).


## V6b.420 — Uygulama kolasyonlu MEMORY LIMIT ve OFFSET

Altı sağlıklı DELETE/WAL dosyasında 372 profil ve iki Rust kipinde toplam 21.732 adım C ile eşleşti. 300 önceki karşılaştırıcı farkı kapandı. Özgün geri çağrı hatası, genişletilmiş kod, ofset, reset, yeniden hazırlama ve yetkilendirici denetlendi. OFF 1300/1/325, ON 1298/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,275 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_420.md).


## V6b.421 — Adım adım PMA birleştirmesi

PMA okuyucu ağacı ve 16 üstündeki ara gruplar C ile aynı adım düzeninde ilerliyor. 36 önceki PMA farkı kapandı. Bellek ve LIMIT kontrolleriyle 60 doğrulama, 8.988 adım ve 68.068 karşılaştırma kaydı eş. Her kipte odak testi geçti. OFF 1301/1/325, ON 1299/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,264 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_421.md).


## V6b.422 — WITHOUT ROWID OR için filtreli IN üreticileri

Tek karşılaştırmalı filtre, indeks aralığı ve tek yerleşik ABS üreticisi 112 farkı kapattı. 168 tam C planı ve 84 Rust odak testi doğrulandı. OFF 1302/1/325, ON 1300/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,279 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_422.md).


## V6b.423 — Tamponlu seviye 0 PMA dosyası

Bağlantı VFS dosyasındaki seviye 0 PMA biçimi, 8.530 C yazma ve okuma ile eş ofset ve boyut gösterdi. İki Rust kipinde açma hatası karşılaştırmadan önce döndü. OFF 1303/1/325, ON 1301/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,267 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_423.md).


## V6b.424 — Kullanıcı API üzerinden FILE sıralama geri çağrıları

108 C profili, 15.156 adım, 98.652 tam argüman çifti eş. 84 eski fark kapandı. 60 callback ve 12 VFS açma hatası özgün kodla döndü. OFF 1304/1/325, ON 1302/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,278 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_424.md).


## V6b.425 — WITHOUT ROWID OR için ek IN ifade biçimleri

Aritmetik filtre, rowid üst sınırı ve LENGTH üreticisi kapsamındaki 128 UTF-16 farkı kapandı. 192 tam C planı ve iki Rust kipinde 43 odak testi doğrulandı. OFF 1305/1/325, ON 1303/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_425.md).


## V6b.426 — İkinci PMA dosyasının tamponlu birleştirme I/O sırası

1.577.890 okuma ve yazmanın ofsetleri ile boyutları ve 24 açma ve kapama tam eşleşti. 2.052 adım ile 16.033 karşılaştırma eş, 12 ara PMA callback hatasında I/O durdu. OFF 1306/1/325, ON 1304/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_426.md).


## V6b.427 — FILE ve DEFAULT LIMIT/OFFSET geçici B-tree akışı

Literal ve bağlı parametreli 576 C profilinde 1.440 adım ve 461.760 VFS okuma ve yazması eşleşti. Açma, okuma ve yazma hataları ile işlem durumu doğrulandı. 48 uygulama hatasından sonra ek G/Ç olmadı. OFF 1307/1/325, ON 1305/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_427.md).


## V6b.428 — Anonim sıralama ve geçici indeks dosyalarının ömrü

İki dosya türü ve iki umask altında C ile aynı 0600, sıfır dizin bağı ve temiz süreç sonlandırması doğrulandı. 576 LIMIT/OFFSET profili yeniden eşleşti; MMAP_SIZE bildirimi hatası sıralamayı kesmedi. OFF 1307/1/325, ON 1305/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_428.md).


## V6b.429 — Unix PMA ve geçici indeks dizini seçimi

64 doğrudan VFS ve 72 gerçek diske taşan SQL profili her Rust kipinde C ile eşleşti. 40 VFS ve 48 SQL farkı kapandı. OFF 1307/1/325, ON 1305/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,266 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_429.md).


## V6b.430 — Unix adlandırılmış geçici dosya yolu ve denemeleri

3.252 sınır ve 278 önceki VFS gözlemi doğrulandı. Her Rust kipindeki 36 SQL profili C ile eşleşti. Önceki 12 SQL farkı kapandı. OFF 1307/1/325, ON 1305/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,268 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_430.md).


## V6b.431 — Çalışan deyim sırasında uygulama fonksiyonunu değiştirme ve kaldırma

48 etkin kayıt vakası ve 912 API kaydı C ile eşleşti. Önceki 336 kontrol korundu. Yeni sabit ve değişken arite testi OFF ve ON koşularında geçti. OFF 1308/1/325, ON 1306/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_431.md).


## V6b.432 — Çalışan deyimde yeni aritenin görünürlüğü

Çalışan deyim mevcut fonksiyonu kullanır. Yeni bir hazırlama eklenen ariteyi görür. Her Rust kipinde 36 C vakası eşleşti ve önceki BUSY ile IN kanıtları korundu. OFF 1309/1/325, ON 1307/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,265 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_432.md).


## V6b.433 — Anonim Linux PMA dosyasında isteğe bağlı eşleme

Seri FILE sıralayıcıda test denetimi ile C eşleme ve kapatma davranışı doğrulandı. Aynı özel tanıtıcının G/Ç ve eşleme erişimi aynı kilitle sıralanır. Varsayılan kip ve ana dosya mmap PRAGMA davranışı korunuyor. OFF 1312/1/325, ON 1310/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,277 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_433.md).


## V6b.434 — Doğrudan function_list hazırlama ve reset

Pinli C SQLite üç kodlamada V1/V2 ve Rust OFF/ON ile karşılaştırıldı. 48 akış ve 390 API kaydında ilk adım, reset, aynı aritede değiştirme ve kaldırma, yeniden hazırlama ve finalize eş. 168 kapsam dışı kontrol ve her kipte 27 odak testi doğrulandı. OFF 1313/1/325, ON 1311/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,286 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_434.md).


## V6b.435 — Tek işçiyle PMA ve kapanış

Mevcut VDBE ON girişinde UTF-8 ana rowid tablosu ve tam BINARY sıralama için bir yardımcı görev çalışır. Çağıran iş parçacığındaki 32 koşuda 63.158 VFS kaydı, 14 gerçek işçi koşusunda satır ve step/reset sonuçları C ile eş. Rust 80 işçi başlattı ve hepsini birleştirdi. PMA testleri 4/4, önceki iki sorter testi her kipte geçti. OFF 1314/1/325, ON 1312/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,355 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_435.md).


## V6b.436 — Etkin ifadede fonksiyon kayıt aritesi

Pinli C ile 132 akış ve 972 API kaydı eş. Mevcut doğru ariteyi değiştirme etkin ifadede SQLITE_BUSY verir. Uyumlu uygulama overloadı, yerleşik tanımdan önce seçilir. Hazır function_list, boşta yapılan gerçek yerleşik tanım değişikliğinde yeniden hazırlanır. Önceki 72 fark kapandı. OFF 1315/1/325, ON 1313/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,274 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_436.md).


## V6b.437 — Hazırlanmış scalar seçimi ve ilk adım/reset

Pinli C ile 48 akış ve 852 API kaydında hazırlanmış view/trigger, tablo biçiminde function_list, reset ve reprepare davranışı sınandı. Taze AST view callback zamanlamasındaki 20 önceki kayıt farkı kaydedildi. Diğer ölçümler eş. 12 arite geri yükleme ve 180 değişmeyen kapsam dışı kontrol doğrulandı. OFF 1316/1/325, ON 1314/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,270 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_437.md).


## V6b.438 — Adlandırılmış sorter mmap

Kararlı filtresiz Linux üzerinde adlandırılıp silinen sorter dosyasının eşlemesi aynı süreçten güvenli çekirdek kopyasıyla okunur. C ile 84 VFS izi, 48 önceki VFS izi ve 144 işçi kontrolü eş. Ek pread hatası kapandı. Sınır, mmap ve dosya kapanışı doğrulandı. OFF 1317/1/325, ON 1315/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,282 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_438.md).


## V6b.439 — UTF-16LE ve UTF-16BE tek işçi PMA

64 zorlanmış UTF-16 profili ve 305.818 VFS kaydı C ile eş. Gerçek işçide satır ve reset, açma, okuma ve yazma hata aşamaları doğrulandı. Tüm Rust işçileri birleştirildi. UTF-8 ve named mmap etkileşimi ile 180 olağan FK koşusu korundu. OFF 1318/1/325, ON 1316/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,275 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_439.md).


## V6b.440 — Kaldırılan fonksiyon aritesi ve yeniden kayıt

Üç kodlama, V1/V2 ve OFF/ON üzerinde 36 akış ve 468 API kaydı eş. 24 önceki fark kapandı. Kaldırılan arite maskesi ve diğer ariteler, gömülü liste satırı, yeniden kayıt, reset ve Rust bool dönüşü doğrulandı. LIKE operatörünün iki aritesi ve PRAGMA sonrası geri kayıt iki yürütme kipinde pinli C ile doğrulandı. OFF 1319/1/325, ON 1317/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_440.md).


## V6b.441 — Scalar ve aggregate tanımı değiştirme

Etkin ifadede aynı fonksiyon tanımı üzerine kayıt SQLITE_BUSY verir. Boştayken uygulama callback türü tek liste girdisi bırakacak şekilde değişir. 36 akış, 540 kayıt ve 252 kapsam dışı kontrol doğrulandı. OFF 1320/1/325, ON 1318/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_441.md).


## V6b.442 — Varsayılan function_list sırası ve sütun metadatası

36 yerel C karşılaştırması eş. Önceki 24 fark kapandı. 3.198 satır, 204 kolon kaydı, 108 yaşam döngüsü ve 288 önceki kontrol doğrulandı. OFF 1321/1/325, ON 1319/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,271 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_442.md).


## V6b.443 — OFF ve ON tek işçi PMA

Üç kodlama ve DELETE/WAL için zorlanmış koşulda 890.408 VFS kaydı eş. Gerçek işçilerde satır, adım, dosya ve hata sonucu eş. 18 okuyucu kilidi ve 180 olağan FK koşusu ile son imajlar doğrulandı. OFF 1322/1/325, ON 1320/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_443.md).


## V6b.444 — Doğrudan function_list yetkilendirme ve şema tanısı

Altı şema biçimi, dört yetkilendirme seçimi, üç kodlama ve V1/V2 üzerinden 288 akış ve 1.248 API kaydı C ile eş. Geçersiz şema, callback öncesi SQL ofsetiyle reddedilir. IGNORE sıfır satır ve sütunla reset yaşam döngüsünü korur. OFF 1323/1/325, ON 1321/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,272 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_444.md).


## V6b.445 — Doğrudan function_list EXPLAIN ve ilerleme

Yetkilendirici kurulu olmayan doğrudan kullanımda 24 C ve EXPLAIN izi, 156 adım zamanlaması ve 52.740 API kaydı eş. İptal, reset ve farklı ilerleme periyotları ölçüldü. Hazırlanabilir olmayan Rust-only adlar önceki satır yaşam döngüsünü korur. OFF 1325/1/325, ON 1323/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,281 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_445.md).


## V6b.446 — Hazırlanmış aggregate ilk adımı ve reset

Yeni uygulama aggregate aritesi eski hazırlanmış seçimi değiştirmez. Taze sorgu yeni tanımı kullanır. 12 C akışı ve 132 kayıt eş. 288 kapsam dışı kontrol ve her kipte odak testi doğrulandı. OFF 1326/1/325, ON 1324/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,257 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_446.md).


## V6b.447 — İki yardımcı işçiyle PMA

İki kipte 468 zorlanmış koşu ve 3.438.644 VFS kaydı eş. 54 bariyerli koşu çağıran iş parçacığının PMA üretmesini, ters hata önceliğini ve bütün işçilerin birleştirilmesini doğruladı. 18 okuyucu kilidi, 180 olağan FK ve son dosya görüntüleri eş. OFF 1327/1/325, ON 1325/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_447.md).


## V6b.448 — Uygulama fonksiyonlarının C liste sırası

390 yerel profil, 186.036 sıralı satır ve 3.120 kayıt işlemi C ile eş. Önceki 330 fark kapandı ve 300 yaşam döngüsü korundu. OFF 1328/1/325, ON 1326/3/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,292 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_448.md).


## V6b.449 — Kayıtlı görünüm MATCH güvenilir şema kararı

Güvenilmeyen şemadaki kayıtlı görünüm MATCH ifadesi C gibi reddedildi. Doğrudan okuma, güvenilir görünüm, VACUUM sonrası yeniden açılış ve bütünlük kontrolleri korundu. FTS dosyasındaki 14 test iki kipte geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v449-trusted-match-defense-candidate/sonuc.md) OFF 1328/1/325, ON 1327/2/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_449.md).


## V6b.450 — Derleme sınırına kadar FILE PMA işçileri

Üç ila sekiz işçi, sekiz üzerindeki isteklerin sınırı, BINARY toplam sırası, üç kodlama ve iki yürütme kipinde 10.111.938 VFS olayı ile değerlendirildi. Gerçek işçi kapanışı ve FK açık 180 sağlıklı koşu doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v450-worker-limit-rebased-candidate/sonuc.md) OFF 1329/1/325, ON 1328/2/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,276 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_450.md).


## V6b.451 — Yetkilendiricili doğrudan function_list yürütmesi

Geçerli izin, yoksayma ve ret kararı korunarak doğrudan PRAGMA programı C ile 312 yaşam döngüsü profili ve 35.424 olayda eşleşti. İlk adım, reset, V1/V2, tekrar yetkilendirme ve iptal doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v451-function-list-authorized-vdbe-rebased-candidate/sonuc.md) OFF 1330/1/325, ON 1329/2/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,273 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_451.md).


## V6b.452 — WITHOUT ROWID indeks aramalarında fiziksel okuma

C kaynaklı dosyada dört bulunan ve bulunamayan anahtar yolunun satırları, okuma sırası, boyutu ve ofseti iki Rust kipinde pinli C ile eşleşti. NOT INDEXED yolunda 32 okuma ve sayfa sırası eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v452-without-rowid-seek-io-rebased-candidate/sonuc.md) OFF 1330/1/325, ON 1330/1/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,285 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_452.md).


## V6b.453 — Taşma sayfasında doğrudan kısmi okuma

C kaynaklı dosyada dört indeks araması ve NOT INDEXED taramasında iki Rust kipi C ile aynı fiziksel boyut ve ofseti kullandı. Son 511 baytlık okuma eşleşti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v452-direct-overflow-read-experiment/sonuc.md) OFF 1330/1/325, ON 1330/1/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 1,283 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_453.md).


## V6b.454 — pragma_function_list iç yetkilendirmesi

840 iç çağrı ve yaşam döngüsü profili iki kipte C ile eşleşti. Reddedilen veya yoksayılan 864 tarama fonksiyon kaydı döndürmedi. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v454-function-list-table-inner-authorizer-rebased-candidate/sonuc.md) OFF 1331/1/325, ON 1331/1/325. DML OFF 397/400 ve ON 400/400. Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. Önceki 147 kurtarma aynı. Rust 1.100 süre oranı 0,861 ve 14 digest eşit. [Karar ve doğrulama](karar_esligi_v6b_454.md).
