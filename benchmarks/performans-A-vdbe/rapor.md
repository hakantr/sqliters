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
