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
