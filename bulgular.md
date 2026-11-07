# SQLite Rust portu inceleme raporu

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
