# SQLite Rust portu — yeniden inceleme raporu

İncelenen commit: `9ae8d0374dbc0326faa1052b71f39063eeca2914`  
Tarih: 8 Kasım 2026  
Referans: yerel `sqlite/`, `/usr/bin/sqlite3` **3.54.1**, Python `sqlite3` **3.54.1**.

## Güncel sonuç

**İlk rapordaki dokuz özgün hata örneği artık geçiyor.** Bunlar için dokuz regresyon testi eklenmiş ve bu incelemede tamamı yeniden çalıştırılmıştır. WAL için tam destek yerine açık reddetme uygulanmış; önceki sessiz eksik okuma giderilmiş. README de kapsamı daha doğru tarif edecek şekilde değiştirilmiş.

Ancak bu, motorun genel olarak doğru olduğu anlamına gelmiyor. Genişletilmiş incelemede aşağıda **12 ek/açık sorun** doğrulandı. Özellikle hatalı commit sonrası geri alma, gerçek C SQLite journal'ının eksik kurtarılması, eski şema ile başka tabloya yazma ve auto-vacuum dosyalarının bozulması üretim kullanımı açısından engeldir.

Bu incelemede uygulama kaynakları değiştirilmedi. Yalnızca bu rapor güncellendi; düzeltme veya yeni commit yapılmadı. Önceki inceleme, tarihsel kayıt olarak dosyanın sonunda korunmuştur.

## Önceki dokuz bulgunun durumu

| Önceki no | Konu | Yeniden kontrol sonucu |
| --- | --- | --- |
| 1 | İşletim sistemi kilitlerinin uygulanmaması | Özgün örnek düzeldi. POSIX `fcntl` ve süreç içi inode takibi var; MemoryVfs çakışmaları da denetleniyor. Bununla birlikte SQL `BEGIN EXCLUSIVE` hâlâ yanlış: yeni bulgu Y9. |
| 2 | Aktif journal'a müdahale | Özgün örnek düzeldi. SHARED kilidi altında RESERVED kontrolü yapılıyor; aktif yazıcının journal'ı korunuyor. Çöken işlemin kurtarılmasında ise ayrı hatalar var: Y1–Y2. |
| 3 | WAL'ın sessizce göz ardı edilmesi | Güvenli reddetme ile giderildi. WAL başlığı/dolu `-wal` için `CantOpen` dönüyor. WAL desteği uygulanmış değil. |
| 4 | Yabancı anahtarların uygulanmaması | Özgün ihlal artık hata veriyor. Eklenen regresyon testi CASCADE, SET NULL, SET DEFAULT ve deferred commit kontrolünü de geçiyor. Tüm FK davranışlarının doğrulandığı anlamına gelmez. |
| 5 | FAIL/ROLLBACK'ın ABORT gibi davranması | Özgün örnekler düzeldi. FAIL sonrası `1,2,3` ve açık işlem; ROLLBACK sonrası `1` ve autocommit doğrulandı. |
| 6 | İndeksli sütunun yeniden adlandırılması | Özgün örnek düzeldi. Tablo, açık indeks ve FK referanslarında token tabanlı yeniden yazım var. |
| 7 | Sabit tarih/saat | Düzeldi. Derleyici/yorumlayıcı gerçek saat yolunu kullanıyor; zaman regresyonu geçiyor. |
| 8 | UTF-16 dosyaları | Özgün örnekler düzeldi. Encoding kayıt okuma/yazma ve karşılaştırmalara aktarılıyor; ek dosya testleri de geçti. |
| 9 | Boş sütun metadata'sı | Düzeldi. `query()` adları yürütmeden sonra alıyor; materialize sorgular ve prepare metadata regresyonları geçiyor. |

Kaynak doğrulama noktaları: `sqliters-vfs/src/lib.rs` içindeki `sys_unix`, inode takibi ve `OsFile::lock`; pager `begin_read_under_shared_lock` ve `recover_hot_journal_under_shared_lock`; core `run_write_autocommit`, FK denetim işlevleri, ALTER yeniden yazım işlevleri; `TextEncoding`; `Connection::query`. Tam yollar aşağıdaki bulgularda ve ilk raporda bulunur.

## Çalıştırılan kontroller

| Kontrol | Sonuç |
| --- | --- |
| `cargo build -p sqliters-cli` | Başarılı; karşılaştırmalarda güncel binary kullanıldı. |
| `cargo test --workspace` | **81 geçti, 0 başarısız, 168 atlandı.** Sayılar Rust test/harness sayılarıdır; Tcl alt test sayısı değildir. |
| `tests/bulgular_regressions.rs` | Dokuz testin tamamı geçti; workspace toplamına dahildir. |
| SQLite uyarlama modülleri, varsayılan koşu | **53/221 modül çalıştı ve geçti**, **168/221 ignore edildi**. Bu bölüm yaklaşık 264 saniye sürdü. |
| `cargo test --test sqlite_test_suite -- --ignored --skip joinD::test_joinD --test-threads=8` | **167 modül başarısız**. Raporlanan alt test toplamları: 51.365 başarılı, 7.974 başarısız. Script hataları nedeniyle bunlar tüm özgün SQLite alt testlerinin toplamı değildir. |
| Atlananların tümünü çalıştırma | 240 saniyelik sınırla denendi; `joinD` tamamlanmadı, süreç timeout ile sonlandırıldı. Bu modül için tamamlanmış sonuç yok. Diğer 167 modülün sonucu ayrı koşuda kesinleştirildi. |
| Depolama matrisi | 512, 1024, 4096, 65536 sayfa boyutları × UTF-8/UTF-16le/UTF-16be: **12 senaryo geçti**. Her birinde 400 karışık sırada insert, indeks, delete ve update sonrası C SQLite `integrity_check=ok`, kalan satır sayısı 267. |
| SQL diferansiyel kontrolü | 20 özellik senaryosu + 64 sorgu aynı SQL ile Rust ve C CLI'da karşılaştırıldı. Anlamlı farklar aşağıda raporlandı; yalnızca float gösterimi veya UNION'da temsilci seçimi farkları ayrı hata sayılmadı. |
| Hata/çökme denemeleri | Özel VFS ile main DB yazma hatası; gerçek C sürecini öldürerek journal kurtarma; iki Rust bağlantısıyla şema değişimi; C auto-vacuum dosyalarına Rust yazması. |

**Test kapsamının yorumu:** 221 dosyanın test ağacına eklenmesi 221 dosyanın geçtiği anlamına gelmiyor. Atlanan testlerin başarısızlıkları; motor davranışları, kapsam dışı özellikler ve özel Rust Tcl/testfixture çalıştırıcısının eksiklerini birlikte içeriyor. 7.974 alt test hatasını 7.974 bağımsız motor hatası olarak yorumlamak doğru olmaz. Resmî C `testfixture` ile tam SQLite paketi çalıştırılmadı; güç kesintisinin bütün olası noktaları veya diğer işletim sistemleri doğrulanmadı.

## Yeni veya açık kalan bulgular

Öncelik: **P1** veri bütünlüğü, yanlış dosyaya/tabloya yazma veya normal girdide süreç çökmesi; **P2** belirli SQL/işlem semantiğinde doğrulanmış uyumsuzluk.

### Y1. [P1] Commit I/O hatasından sonra ROLLBACK diskteki kısmi yazmaları geri almıyor

**Konum:** [commit](crates/sqliters-pager/src/lib.rs#L600), [rollback](crates/sqliters-pager/src/lib.rs#L680).

Commit dirty sayfaları ana dosyaya sırayla yazıyor. Sonraki bir yazma hata verirse `?` ile çıkıyor. `rollback()` yalnızca ön görüntüleri cache'e koyuyor; ana dosyaya geri yazmadan journal'ı siliyor. “Main DB henüz değişmedi” varsayımı, başarısız commit sonrası geçerli değil.

**Doğrulama:** `OsVfs`'yi saran bir test VFS'si kullanıldı. Yalnızca `.db` dosyasındaki üçüncü `write_at` çağrısına tek seferlik `IoErr` enjekte edildi; diğer bütün çağrılar gerçek VFS'ye aktarıldı.

```sql
CREATE TABLE t1(x); CREATE TABLE t2(y);
INSERT INTO t1 VALUES(10); INSERT INTO t2 VALUES(20);
BEGIN;
UPDATE t1 SET x=11; UPDATE t2 SET y=21;
COMMIT;   -- Üçüncü main DB yazmasında yapay I/O hatası.
ROLLBACK;
```

Bağlantı kapatılıp dosya tekrar açıldı. Beklenen `(10,20)` yerine **`(11,20)`** okundu. ROLLBACK başarılı dönmesine rağmen atomiklik bozuldu; journal da kaldırıldı.

**Gerekli değişiklik:** Diskin commit sırasında kısmen değişmiş olabileceği durumu izlenmeli; rollback journal ana dosyaya geri oynatılmalı. Geri alma başarısızsa kurtarma verileri korunmalı. Yazma, truncate, sync ve journal silme hata noktaları ayrıca test edilmeli.

### Y2. [P1] Journal okuyucu tek başlıkla sınırlı; yazıcı sektör bilgisini yanlış kaydediyor

**Konum:** [512 bayt sabiti](crates/sqliters-pager/src/lib.rs#L21), [başlık yazımı](crates/sqliters-pager/src/lib.rs#L819), [kurtarma](crates/sqliters-pager/src/lib.rs#L863).  
**C karşılığı:** `sqlite/src/pager.c`, `readJournalHdr` ve `pager_playback` içindeki çoklu başlık döngüsü.

İki yönde ayrı sorun var:

1. Rust kurtarma kodu yalnızca ilk journal başlığındaki kayıt sayısını okuyor. Cache spill nedeniyle sonraki başlıklara yazılmış kayıtları gezmiyor. Sektör boyutuna göre başlık hizalamasını da kullanmıyor.
2. Rust yazıcısı başlık ve ilk kayıt konumu için 512 bayt kullanırken journal başlığının sektör alanına `file.sector_size()` değerini yazıyor. Varsayılan değer 4096. Dosyada ilan edilen düzen ile gerçek kayıt konumu farklı.

**Gerçek C çökmesiyle yeniden üretim:** Python/C SQLite'da `page_size=1024`, 200 satırlı `t(id INTEGER PRIMARY KEY,x INT,pad TEXT)` oluşturuldu; `pad` 400 karakter, `x=0`. Commit sonrası `cache_size=5; BEGIN IMMEDIATE; UPDATE t SET x=1` çalıştırıldı. İşlem commit edilmeden süreç öldürüldü. DB ve journal iki ayrı yola kopyalanıp C ve Rust ile açıldı:

| Kurtaran | `SELECT count(*),sum(x) FROM t` | integrity_check |
| --- | --- | --- |
| C SQLite | `200|0` | `ok` |
| Rust | **`200|188`** | **`ok`** |

Bu gerçek journal'ın ilk başlığında 3 kayıt vardı. Rust sonraki journal bölümlerini atlayıp dosyayı sildi; 188 satırlık commit edilmemiş değişiklik kaldı. Rust'ın `integrity_check=ok` demesi işlem atomikliğini doğrulamıyor.

**Ters yön kontrolü:** Rust journal kopyasında record_count, mevcut kayıtlara göre final commit başlığı gibi ayarlanıp ana sayfa bozulması simüle edildi. Kayıtlar 512'de, sektör alanı 4096 iken C kurtaramadı (`database disk image is malformed`). Yalnızca sektör alanı 512 yapıldığında aynı kayıt ve sayfalar C ile `ok` ve eski değer `10` sonucuna döndü. Bu ikinci deney kontrollü dosya simülasyonudur; gerçek güç kesintisi değildir.

**Gerekli değişiklik:** Sektöre hizalı yazım/okuma ve journal'ın tüm başlık bölümleri C formatına uygun uygulanmalı. İki yönlü kurtarma testleri eklenmeli.

### Y3. [P1] Başka bağlantının DDL değişiminden sonra eski schema ile farklı tabloya yazılabiliyor

**Konum:** [ensure_schema_loaded](crates/sqliters-core/src/lib.rs#L174), [prepare](crates/sqliters-core/src/lib.rs#L238), [statement dispatch](crates/sqliters-core/src/engine.rs#L206), [begin_read sırasında başlık yenileme](crates/sqliters-pager/src/lib.rs#L330).

Schema cookie kontrolü diskten yeni başlık okunmadan yapılıyor. `begin_write()` daha sonra pager başlığını yenilese bile aynı statement için schema yeniden yüklenmiyor. Önceden yüklenmiş tablo adı/root page eşlemesi kullanılabiliyor.

**Aynı dosyadaki iki Rust bağlantısıyla doğrulama:**

```text
A: CREATE TABLE t(x); INSERT INTO t VALUES(1);
B: SELECT * FROM t;                 -> 1 (schema cache yüklendi)
A: DROP TABLE t;
A: CREATE TABLE u(x); INSERT INTO u VALUES(99);
B: INSERT INTO t VALUES(2);          -> başarı, 1 değişiklik
A: SELECT * FROM u;                 -> 99 ve 2
```

Beklenen B'de `no such table: t` hatasıdır. Gerçekte silinmiş tablonun root page'i yeni `u` tablosunda kullanılmış ve **yazma yanlış tabloya gitmiştir**.

**Gerekli değişiklik:** Okuma/yazma transaction'ı ve schema yenilemesi aynı snapshot altında, derleme ve tablo/index çözümlemesinden önce tamamlanmalı; hazırlanmış planlar schema değişiminde geçersizleşmeli.

### Y4. [P1] Auto-vacuum dosyalarına yazmak pointer-map bütünlüğünü bozuyor

**Konum:** [sayfa ayırma](crates/sqliters-btree/src/lib.rs#L95), [raw sayfa ayırma](crates/sqliters-pager/src/lib.rs#L553), [auto-vacuum başlık alanları](crates/sqliters-pager/src/header.rs#L25).  
**C karşılığı:** `sqlite/src/btree.c`, `ptrmapPut`, `PTRMAP_PAGENO` ve auto-vacuum sayfa yönetimi.

Başlıktaki auto-vacuum alanları okunuyor ama tahsis/yazma yolunda pointer-map sayfaları yönetilmiyor; bu dosyalar yazmaya karşı reddedilmiyor.

**Yeniden üretim:** C SQLite ile `page_size=512; auto_vacuum=FULL` ayarlanıp `CREATE TABLE t(x TEXT)` ve bir satır commit edildi. Rust ile tek transaction'da 150'den uzun karakterli 60 ek satır yazıldı. Aynı deney `INCREMENTAL` için tekrarlandı.

Her iki durumda Rust başarı döndürdü; C `PRAGMA integrity_check` çok sayıda **`Failed to read ptrmap key=...`** hatası döndürdü.

**Gerekli değişiklik:** Pointer-map desteklenene kadar auto-vacuum dosyaları yazmaya açılmamalı. Yalnızca PRAGMA'yı yok saymak mevcut C dosyalarını korumuyor.

### Y5. [P1] Kapsam dışı WITHOUT ROWID kabul edilip uyumsuz dosya oluşturuluyor

**Konum:** [CREATE TABLE](crates/sqliters-core/src/engine.rs#L6225), [koşulsuz tablo B-tree tahsisi](crates/sqliters-core/src/engine.rs#L6244), `crates/sqliters-core/src/schema.rs` içindeki `without_rowid` alanı.

```sql
CREATE TABLE t(a TEXT PRIMARY KEY) WITHOUT ROWID;
INSERT INTO t VALUES('a');
```

Rust dosyasında bu SQL başarılı oluyor. Dosya C ile açıldığında `PRAGMA integrity_check` **`Page 2: never used`** döndürüyor. Katalog `WITHOUT ROWID` derken fiziksel oluşturma yolu normal rowid tablo ve ek indeks üretiyor.

README bu özelliği kapsam dışında sayıyor; buna rağmen SQL'in başarıyla kabul edilmesi ve uyumsuz dosya yazılması açık hatadır.

**Gerekli değişiklik:** Desteklenmeyen biçim tahsis/katalog yazımından önce reddedilmeli veya gerçek indeks tabanlı WITHOUT ROWID depolaması uygulanmalı. Mevcut C WITHOUT ROWID dosyaları için de güvenli açılış politikası gerekli.

### Y6. [P1] Geçerli Türkçe/Unicode nesne adı panic oluşturuyor

**Konum:** [CREATE TABLE ad kontrolü](crates/sqliters-core/src/engine.rs#L6226); aynı kalıp `6283`, `6456`, `6608` satırlarında da var.

```sql
CREATE TABLE "çççç"(x);
INSERT INTO "çççç" VALUES(1);
SELECT * FROM "çççç";
```

- C: başarı, `1`.
- Rust: **exit 101**, `end byte index 7 is not a char boundary` panic'i.

`name.len() >= 7` sonrasında UTF-8 string üzerinde `name[..7]` kullanılıyor. Yedinci bayt karakterin ortasına denk gelebiliyor. Bu, SQL hatası yerine uygulamayı çökertebilir.

**Gerekli değişiklik:** `sqlite_` öneki bayt veya güvenli `get(..7)` üzerinden kontrol edilmeli; CREATE TABLE/AS, CREATE INDEX ve RENAME yollarının tamamı test edilmeli.

### Y7. [P2] STRICT ANY özgün değeri korumuyor

**Konum:** [affinity belirleme](crates/sqliters-core/src/schema.rs#L406), [insert sırasında affinity uygulama](crates/sqliters-core/src/engine.rs#L4732).

```sql
CREATE TABLE t(x ANY) STRICT;
INSERT INTO t VALUES('000123');
SELECT typeof(x),x FROM t;
```

- C: **`text|000123`**.
- Rust: **`integer|123`**.

STRICT ANY, normal tablodaki numeric affinity davranışını almamalı. Başındaki sıfırlar ve storage class kayboluyor. STRICT ANY için affinity/coercion yolu ayrılmalı; update ve generated-column yolları da denetlenmeli.

### Y8. [P2] ADD COLUMN CHECK mevcut satırlar üzerinde doğrulanmıyor

**Konum:** [AddColumn](crates/sqliters-core/src/engine.rs#L6792).

```sql
CREATE TABLE t(x);
INSERT INTO t VALUES(1);
ALTER TABLE t ADD COLUMN y INT CHECK(y>0) DEFAULT -1;
```

- C: **`CHECK constraint failed`**, ALTER reddediliyor.
- Rust: başarı; `SELECT * FROM t` → **`1|-1`**.
- Rust'ın yazdığı dosyada C `integrity_check`: **`CHECK constraint failed in t`**.

Yeni CREATE SQL'i kataloğa yazılmadan önce mevcut kayıtlar yeni CHECK/generated NOT NULL koşullarıyla doğrulanmalı. Başarısız doğrulamada schema değişmemeli.

### Y9. [P2] BEGIN EXCLUSIVE yalnızca RESERVED kilidi alıyor

**Konum:** [BEGIN dispatcher](crates/sqliters-core/src/engine.rs#L259), [begin_write](crates/sqliters-pager/src/lib.rs#L368).

Immediate ve Exclusive aynı `begin_write()` çağrısına yönlendiriliyor. EXCLUSIVE kilidi ancak commit'te isteniyor.

**Doğrulama:** Rust bağlantısında tablo ve satır oluşturulup `BEGIN EXCLUSIVE` açık tutuldu. Ayrı C `sqlite3` sürecinin `SELECT * FROM t` çağrısı **başarıyla `1`** döndürdü. Rollback-journal kipinde beklenen `database is locked` sonucudur.

Gerçek VFS kilitleri eklenmiş olsa da SQL işlem modunun gerektirdiği kilit başlangıçta alınmalı; mevcut okuyucu varsa BEGIN EXCLUSIVE hemen BUSY verebilmeli.

### Y10. [P2] Tek MIN/MAX ile seçilen bare column yanlış satırdan geliyor

**Konum:** [aggregate temsilci satırı](crates/sqliters-core/src/engine.rs#L1408).

```sql
CREATE TABLE t(a,b);
INSERT INTO t VALUES(1,'one'),(2,'two');
SELECT max(a),b FROM t;
```

- C: **`2|two`**.
- Rust: **`2|one`**.

Tek yerleşik MIN/MAX içeren sorguda SQLite bare column değerini extremum satırından alır. Motor `group_rows.first()` seçtiği için farklı satırdaki alanı döndürüyor. Örnekte eşit extremum yok; sonuç farkı belirsiz tie davranışından kaynaklanmıyor. Aynı sorun MIN ve GROUP BY örneklerinde de görüldü.

### Y11. [P2] SAVEPOINT ile açılmış işlem içinde BEGIN kabul ediliyor

**Konum:** [BEGIN kontrolü](crates/sqliters-core/src/engine.rs#L254).

```sql
SAVEPOINT x;
BEGIN;
```

- C: `cannot start a transaction within a transaction`.
- Rust: başarı.

Kontrol yalnızca `explicit_tx` bayrağına bakıyor; `implicit_savepoint_tx` göz ardı ediliyor ve ardından sıfırlanıyor. BEGIN, autocommit kapalı olduğu tüm durumlarda transaction nesting kuralını uygulamalı.

### Y12. [P2] Negatif maksimum rowid sonrası otomatik rowid farklı üretiliyor

**Konum:** [next_rowid](crates/sqliters-btree/src/lib.rs#L495).

```sql
CREATE TABLE t(a);
INSERT INTO t(rowid,a) VALUES(-5,'old');
INSERT INTO t(a) VALUES('new');
SELECT rowid,a FROM t ORDER BY rowid;
```

- C: `-5|old`, **`-4|new`**.
- Rust: `-5|old`, **`1|new`**.

`(max_id + 1).max(1)` normal rowid tablosunda gereksiz pozitif sınır uyguluyor. AUTOINCREMENT olmayan tablolarda SQLite'ın maksimum rowid + 1 kuralı izlenmeli; AUTOINCREMENT ayrı ele alınmalı.

## Düzeltme ve doğrulama sırası

1. **Y1–Y4:** Transaction atomikliği, journal formatı/kurtarma, schema snapshot ve auto-vacuum dosya güvenliği.
2. **Y5–Y6:** Desteklenmeyen biçimin güvenli reddi ve Unicode panic'lerinin kaldırılması.
3. **Y7–Y12:** Kısıt, işlem ve sorgu semantiği farkları.
4. Her hata için C referansıyla bağımsız regresyon; özellikle disk yeniden açılışı ve C `integrity_check` doğrulaması.
5. Atlanan SQLite modüllerinde SQL motoru hatalarıyla Tcl/testfixture eksiklerini ayırma. Varsayılan test sonucu ile tam uyumluluk iddiasını birbirinden ayrı tutma.

**Nihai değerlendirme:** Önceki rapor üzerine somut ilerleme var; dokuz özgün örnek düzeltilmiş. Buna rağmen bu yeniden inceleme veri atomikliği ve dosya bütünlüğünde yeni doğrulanmış sorunlar ortaya çıkardı. Mevcut sürüm için tam SQLite uyumluluğu veya üretim verisi güvenilirliği onayı verilemez.

---

# İlk inceleme — tarihsel kayıt

Aşağıdaki bölüm `814e41e` commitindeki durumu anlatır. Güncel durum yukarıdadır; eski bulguların hâlâ aynen sürdüğü şeklinde okunmamalıdır.

İncelenen commit: `814e41e` — `Initial Rust SQLite port`  
Tarih: 8 Kasım 2026

## Sonuç

Mevcut uygulama, SQLite dosya biçiminin ve SQL özelliklerinin bir bölümünü çalıştırıyor; ancak doğru ve tam uyumlu bir SQLite portu olarak değerlendirilemez. Eşzamanlı erişim, WAL, kısıtlar ve işlem semantiğinde doğrulanmış hatalar var. README'deki “birebir”, “%100” ve “tam SQL AST” ifadeleri mevcut doğrulamalarla desteklenmiyor.

Bu incelemede kaynak kod değiştirilmedi. Mevcut kod önce yukarıdaki commite kaydedildi; rapor daha sonra oluşturuldu.

## Yöntem ve kapsam

- `cargo test --workspace` çalıştırıldı: **16 test geçti**, başarısız test yok. Doc-testlerde test bulunmuyor.
- Rust kaynakları, `sqlite/src/` altındaki ilgili C uygulamalarıyla karşılaştırıldı.
- SQL örnekleri `target/debug/sqliters` ve sistemdeki `/usr/bin/sqlite3` ile çalıştırıldı. Referans CLI sürümü **3.54.1**; `sqlite/VERSION` da **3.54.1**.
- Eşzamanlı erişim, WAL ve UTF-16 örneklerinde Python `sqlite3` bağlantısı ve Rust CLI birlikte kullanıldı. Python modülünün SQLite sürümü ayrıca kaydedilmedi.
- Rust API davranışı, mevcut derleme çıktısına bağlanan geçici bir Rust programıyla kontrol edildi.
- Deneme veritabanları geçici dizinlerde oluşturuldu. Tam SQLite test paketi, hata enjeksiyonu ve güç kesintisi testleri çalıştırılmadı. Bulgular tüm hataların listesi değildir.

Öncelikler: **P1** veri güvenliği/doğruluğu veya temel uyumluluk sorunu; **P2** belirli özellik/API davranışında hata.

## 1. [P1] Dosya kilitleri işletim sistemine uygulanmıyor

**Konum:** [VFS lock](crates/sqliters-vfs/src/lib.rs#L495), [unlock](crates/sqliters-vfs/src/lib.rs#L506).  
**C karşılığı:** `sqlite/src/os_unix.c:1866`, `unixLock`.

`OsFile::lock()` yalnızca dosya nesnesinin kendi `Mutex<LockLevel>` alanını değiştiriyor. POSIX bayt aralığı kilidi almıyor; diğer bağlantı ve süreçlerle çakışma denetlenmiyor. Bellek VFS'sinin kilit uygulaması da çakışmada hata döndürmüyor.

**Yeniden üretim:** C/Python bağlantısında `CREATE TABLE t(x)` commit edildikten sonra `BEGIN IMMEDIATE; INSERT INTO t VALUES(1)` açık tutuldu. Ayrı Rust CLI sürecinde aynı dosyada şu SQL çalıştırıldı:

```sql
INSERT INTO t VALUES(2);
SELECT * FROM t;
```

Rust işlemi `SQLITE_BUSY` döndürmesi gerekirken başarıyla tamamlandı ve `2` döndürdü. C bağlantısı rollback yaptıktan sonra da tabloda `2` kaldı.

**Etki:** Yazıcıların birbirini dışlaması sağlanmıyor. Eşzamanlı kullanımda değişiklik kaybı ve dosya bozulması riski var. SQLite ile uyumlu gerçek kilitleme uygulanmalı.

## 2. [P1] Aktif rollback journal, çökmüş işlemin journal'ı gibi ele alınıyor

**Konum:** [açılışta kurtarma](crates/sqliters-pager/src/lib.rs#L133), [journal kurtarma](crates/sqliters-pager/src/lib.rs#L750).  
**C karşılığı:** `sqlite/src/pager.c:5196`, `hasHotJournal`.

Kurtarma akışı journal dosyasının varlığını kontrol ediyor; başka bir yazıcının RESERVED kilidini tutup tutmadığını kontrol etmiyor. Geçersiz sihirli başlıkta journal dosyasını siliyor; geçerli başlıkta geri oynatma ve truncation yapabiliyor. C uygulaması, RESERVED kilidi bulunan aktif yazıcının journal'ını hot journal kabul etmiyor.

**Tetikleme:** Önceki bulgudaki açık C yazma işlemi sırasında Rust bağlantısının açılması bu akışa giriyor. Kilit sorunu düzeltilse bile kurtarma öncesindeki hot-journal tespiti ayrıca düzeltilmeli.

**Etki:** Devam eden işlemin kurtarma verileri silinebilir veya aktif veritabanına yanlış kurtarma uygulanabilir. Bu bulgunun güç kesintisindeki sonuçları ayrıca test edilmedi.

## 3. [P1] WAL veritabanları kabul ediliyor ama WAL okunmuyor

**Konum:** [başlık sürümü kontrolü](crates/sqliters-pager/src/header.rs#L106), `crates/sqliters-pager/src/lib.rs` içindeki ana dosyadan okuma/yazma akışı.  
**C karşılığı:** `sqlite/src/pager.c:3314`, `sqlite3WalBeginReadTransaction`; `sqlite/src/pager.c:7676`, `sqlite3WalOpen`.

Başlık ayrıştırıcısı `read_version=2` değerini kabul ediyor. Buna karşılık pager, WAL frame'lerini ve WAL snapshot'ını işlemiyor.

**Yeniden üretim:** C/Python bağlantısında tablo oluşturulup commit edildi. Ardından `PRAGMA journal_mode=WAL; INSERT INTO t VALUES(42)` commit edildi ve bağlantı açık tutuldu. Aynı dosyada Rust ile `SELECT * FROM t` çalıştırıldı.

- C bağlantısı: `[(42,)]`.
- Rust: hata vermeden boş sonuç.

**Etki:** Commit edilmiş veriler sessizce eksik okunuyor. WAL uygulanana kadar bu dosyalar açık hatayla reddedilmeli; WAL modundaki dosyaya mevcut pager ile yazılmamalı.

## 4. [P1] `foreign_keys=ON` olmasına rağmen yabancı anahtarlar uygulanmıyor

**Konum:** [sütun FK'sinin atlanması](crates/sqliters-core/src/schema.rs#L427), [tablo FK'sinin atlanması](crates/sqliters-core/src/schema.rs#L556), [PRAGMA](crates/sqliters-core/src/engine.rs#L3269).  
**C karşılığı:** `sqlite/src/fkey.c`, `sqlite3FkCheck`.

```sql
PRAGMA foreign_keys=ON;
CREATE TABLE p(id INTEGER PRIMARY KEY);
CREATE TABLE c(pid INTEGER REFERENCES p(id));
INSERT INTO c VALUES(99);
SELECT * FROM c;
```

- SQLite: `FOREIGN KEY constraint failed`.
- Rust: başarı; `99` kaydediliyor.

PRAGMA yalnızca bir boolean değiştiriyor; schema oluşturulurken FK bilgileri atlanıyor.

**Etki:** Uygulama kısıtların etkin olduğunu düşünürken ilişkisel bütünlük bozuluyor. FK denetimi ve eylemleri uygulanmalı veya desteklenmediği açıkça belirtilmeli.

## 5. [P1] `OR FAIL` ve `OR ROLLBACK`, `ABORT` gibi uygulanıyor

**Konum:** [hata halinde geri alma](crates/sqliters-core/src/engine.rs#L328), [UNIQUE çatışması](crates/sqliters-core/src/engine.rs#L2180).  
**C karşılığı:** `sqlite/src/vdbe.c:1325` çevresindeki `OE_Fail`, `OE_Rollback`, `OE_Abort` ayrımı.

Rust API ile aşağıdaki başlangıçtan sonra iki çatışma türü ayrı ayrı denendi:

```sql
CREATE TABLE t(x UNIQUE);
INSERT INTO t VALUES(1);
BEGIN;
INSERT INTO t VALUES(2);
INSERT OR FAIL INTO t VALUES(3),(1);
-- İkinci denemede son komutta OR ROLLBACK kullanıldı.
```

Hata yakalanıp aynı bağlantıda sonuç sorgulandı:

| Çatışma türü | SQLite semantiği | Rust sonucu |
| --- | --- | --- |
| FAIL | İşlem açık kalır; önceki başarılı `3` satırı korunur: `1,2,3` | İşlem açık; `1,2` |
| ROLLBACK | Tüm işlem geri alınır; autocommit açılır: `1` | İşlem açık; `1,2` |

**Etki:** Hata sonrasında hangi değişikliklerin kalacağı ve işlemin durumu yanlış. Çatışma eylemi hata yönetimine taşınmalı; tüm constraint hataları aynı statement rollback yoluna yönlendirilmemeli.

## 6. [P2] İndeksli sütunu yeniden adlandırmak başarısız oluyor

**Konum:** [RenameColumn](crates/sqliters-core/src/engine.rs#L2939), [metinsel değiştirme](crates/sqliters-core/src/engine.rs#L3019).  
**C karşılığı:** `sqlite/src/alter.c:625`, `sqlite3AlterRenameColumn`.

```sql
CREATE TABLE t(a INT);
CREATE INDEX ix ON t(a);
ALTER TABLE t RENAME COLUMN a TO b;
```

- SQLite: başarılı; sonraki insert/select çalışıyor.
- Rust: `no such column: a`.

Yalnızca tablonun CREATE SQL'i değiştiriliyor; indeks SQL'indeki sütun referansı güncellenmiyor. Yardımcı işlev ayrıca yalnızca ilk metinsel eşleşmeyi değiştiriyor.

**Etki:** README'de desteklendiği belirtilen ALTER işlemi yaygın bir şemada çalışmıyor. Disk denemesinde hata sonrası eski şemanın rollback ile korunduğu doğrulandı; bu örnekte kalıcı schema bozulması görülmedi.

## 7. [P2] Güncel tarih ve saat sabit kodlanmış

**Konum:** [yorumlayıcı](crates/sqliters-core/src/engine.rs#L1371), [derleyici](crates/sqliters-core/src/compiler.rs#L1040).  
**C karşılığı:** `sqlite/src/date.c`, güncel zaman fonksiyonları.

```sql
SELECT CURRENT_DATE, CURRENT_TIME, CURRENT_TIMESTAMP;
```

Rust daima `2026-11-08|00:00:00|2026-11-08 00:00:00` döndürüyor. Aynı karşılaştırmada SQLite gerçek UTC zamanı olan `2026-11-07|21:57:57|2026-11-07 21:57:57` döndürdü.

**Etki:** Zaman damgaları ve bunlara dayanan DEFAULT/sorgu sonuçları yanlış. Gerçek saat kaynağı ve SQLite'ın statement içindeki zaman tutarlılığı uygulanmalı.

## 8. [P1] Geçerli UTF-16 SQLite dosyaları doğru okunmuyor

**Konum:** [encoding başlığı](crates/sqliters-pager/src/header.rs#L139), [kayıt metni çözümleme](crates/sqliters-types/src/record.rs#L186).

Başlık UTF-16 encoding değerlerini kabul ediyor; kayıt çözümleyicisi ise metni koşulsuz UTF-8 olarak çözüyor ve geçersiz baytları kayıplı dönüştürüyor. Encoding, kayıt okuma/yazmaya aktarılmıyor.

**Yeniden üretim:** C/Python ile `PRAGMA encoding='UTF-16le'` ve ayrı denemede `'UTF-16be'` ayarlanıp `CREATE TABLE t(x TEXT); INSERT INTO t VALUES('Türkçe')` commit edildi. Rust iki dosyada da `SELECT * FROM t` için `no such table: t` döndürdü.

**Etki:** Geçerli SQLite dosyalarının schema/metin içeriği yanlış yorumlanıyor. Encoding desteği eklenmeli veya desteklenmeyen encoding açılışta reddedilmeli. `%100` dosya uyumluluğu iddiasıyla çelişiyor.

## 9. [P2] Materialize edilen sorgularda API sütun adları boş dönüyor

**Konum:** [query](crates/sqliters-core/src/lib.rs#L190), [step sırasında metadata güncellemesi](crates/sqliters-core/src/lib.rs#L463).

`Connection::query()` sütun adlarını `collect_rows()` çağrısından önce kopyalıyor. VDBE'ye derlenmeyen sorgularda adlar ancak ilk `step()` sırasında hesaplanıyor.

Rust API denemesinde:

- `SELECT a FROM t`: sütun adları `["a"]`.
- `SELECT count(*) FROM t`: bir satır dönmesine rağmen sütun adları `[]`.
- `SELECT a FROM t UNION ALL SELECT a FROM t`: iki satır dönmesine rağmen sütun adları `[]`.

**Etki:** `QueryResult` metadata'sı sorgu yürütme yoluna göre değişiyor. Sütun adları yürütme sonrası alınmalı; prepared-statement metadata sözleşmesi de tutarlı hale getirilmeli.

## Önerilen düzeltme sırası

1. Gerçek dosya kilitleri ve güvenli hot-journal tespiti.
2. WAL ve UTF-16 için doğru destek veya açık reddetme.
3. FK denetimi ve çatışma eylemlerinin işlem semantiği.
4. ALTER referans güncellemesi, gerçek saat ve API metadata'sı.
5. Her doğrulanmış örneği regresyon testine dönüştürme; ardından C SQLite ile daha geniş diferansiyel testler, I/O hata enjeksiyonu ve çökme kurtarma testleri.

Mevcut testler temel senaryoları doğruluyor; eşzamanlı bağlantı, aktif journal, checkpoint edilmemiş WAL, UTF-16 ve hata sonrası işlem durumunu güvence altına almıyor. Bu sorunlar giderilmeden üretim verisi için güvenilir SQLite eşdeğeri olduğu söylenemez.
