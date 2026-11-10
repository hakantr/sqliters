# SQLtest — Statik örnek veri ve SQL komut setleri

**Test çalıştırılmaz.** Bu paket yalnız plan, veri ve SQL metni üretir.
DB motoru/SQL execute/C–Rust derleme/benchmark/selftest/pytest/cargo/sqlite3
çalıştırılmadı. Bütün motor sonuçları **NOT_RUN**. Üretilmiş SQL'in beklenen
başarısı/hatası, motorla doğrulanmış sonuç değildir.

Yazma yetkisi yalnız yeni `sqltest/` içindir; port/C kaynakları, kök Cargo ve
mevcut kullanıcı işleri değiştirilmez. [AGENTS.md](AGENTS.md) bu sınırı kaydeder.
Ana plan: kardeş sqlitesimd deposundaki `docs/sqltest-is-plani.md`.

## Statik üretim

Port kökünden yalnız dosya üretimi:

```bash
python3 sqltest/generate.py
```

Python standart kütüphanesi yeterlidir; SQLite/subprocess/harici paket yoktur.
Seed ve sayılar [profiles.json](profiles.json) içindedir. SplitMix64 sabittir;
aynı kaynak/config aynı SQL/CSV/JSONL baytlarını üretir. Script yalnız sqltest
altına yazar; generator'ı düzenlemeden önce mevcut manifest/girdi kimliğini
koruyun. Üretim hiçbir SQL'i çalıştırmaz.

- [schema.sql](examples/schema.sql), [seed.sql](examples/seed.sql): customers,
  orders, items, employees, duplicate/NULL/unmatched anahtarlar ve FK fixture'ı.
- `examples/scenarios/*.sql`: 50'den fazla bağımsız okunabilir senaryo. Her biri
  gelecekte temiz schema+seed ile ayrı yürütülecek şekilde tasarlanmıştır;
  önceki senaryonun mutation/rollback sonucuna dayanmaz. `negative-*` dosyaları
  kasıtlı hata beklentisidir; sıradan senaryolarla birleştirilmez.
- `examples/normal-sample.csv/jsonl/sql`: ilk 12 normal kaydın okunabilir örneği.
- [large-join-queries.sql](examples/large-join-queries.sql): büyük ilişkili veri
  için 13 açık SELECT; full materyalizasyon, FULL/LEFT JOIN, aggregation,
  semi/anti, recursive çalışan ağacı ve zaman pencereleri.
- `artifacts/generated/full/normal.csv/jsonl/sql`: **10.000 fiziksel kayıt**;
  SQL'de 10.000 açık INSERT ve bütün sütunları okuyan son SELECT vardır.
- `artifacts/generated/full/relational.sql` ve `relational/*.csv/jsonl`:
  **customers 3000, orders 12000, items 24000, employees 1000, measurements
  10000** olmak üzere 50.000 fiziksel kayıt. NULL/duplicate/unmatched ilişki
  anahtarları hedeflidir; son 150 customer id'si sipariş referansından ayrılır.
  Bu fixture küçük schema/seed yerine kendi temiz gelecekteki DB'sini hedefler;
  customers/orders/items/employees adları aynı olduğu için ikisi üst üste
  çalıştırılmaz. Üretilmiş dosyalar DB'ye yüklenmedi.
- `artifacts/generated/full/wide-2000.sql`, `wide-4096.sql`: 2000/4096 explicit
  sütun DDL'i, aynı sayıda INSERT SELECT ifadesi ve bütün sütun SELECT'i.
  Deterministic SQL aritmetiğiyle her tabloda **3000 hedef kayıt** üretir;
  DB oluşturma ve 6.000.000/12.288.000 hücre materyalizasyonu **NOT_RUN**.
- `artifacts/generated/full/boundary/max-{128,2000,4096}/`: sekiz aile × üç
  M−1/M/M+1 genişlik × üç yapılandırma = **72 ayrı SQL dosyası**.

Büyük artifact dosyaları ignored fakat diskte üretilir. Kaynak, seed,
[corpus-manifest.json](corpus-manifest.json), [scope-matrix.json](scope-matrix.json)
ve küçük örnekler review edilebilir. Manifest dosya byte/SHA/physical line
sayısı ile planlanan SQL row/column sayısını ayırır. CSV/JSONL fiziksel kayıt
sayısı üretilmiş veridir; wide hedef satır sayısı yürütülmüş SQL sonucu değildir.

## Veri gösterimi

Normal kolonlar: id, group_id, n, r, name, note, payload. CSV'de NULL `\N`,
payload lowercase hex (boş string boş BLOB); Text'te backslash `\\`, NUL
`\0`, newline `\n`, CR `\r` olarak kaçırılır. CSV alan quoting'i ayrıca
standarttır. JSONL null ve sayıları kayıpsız taşır, BLOB `{type:blob,hex:…}`
nesnesidir; kontrol karakterleri JSON escape kullanır. Ham JSONL integer'larını
gelecek okuyucu float'a çevirmez. SQL Text quote'u çiftlenir; NUL içeren metin
`CAST(X'…' AS TEXT)` ile, BLOB `X'…'` ile yazılır. Bu açıklamadaki üç nokta
gösterimdir; üretilmiş SQL'de placeholder yoktur.

NULL, signed int64 uçları, negatif/pozitif sayılar, sonlu gerçek sayılar,
Türkçe/Japonca/birleşik Unicode, tırnak, boş ve kontrol karakterli Text,
boş/değişken BLOB örnekleri vardır. Özel REAL SQL senaryosu ±0, ±Inf ve NULL
sonucu beklenen ifade içerir; bit eşitliği henüz doğrulanmadı. Geniş kayıt
ifadelerinde tür atanmamış sütunlar Null/Integer/Real/Text/Blob karışımını
korumak için kullanılır. SQL `random()` ve zamana bağlı seed yoktur.

## Kapsam ve sınır beklentileri

DDL/DML/ALTER/conflict/UPSERT/RETURNING, views/triggers/FK, transactions,
INNER/LEFT/RIGHT/FULL/CROSS/NATURAL/self/semi/anti JOIN örnekleri, set işlemleri,
CTE/window/aggregate/index, NULL/Unicode/BLOB/types/overflow ve yönetim komutları
dosya olarak bulunur. Semi/anti örnekleri EXISTS/NOT EXISTS'tir; SQLite için
SEMI/ANTI JOIN keyword'u icat edilmez. Bütün gramer kombinasyonları kapsanmaz.

Boundary ailesi: CREATE TABLE/VIEW, SELECT, CREATE INDEX, UPDATE SET,
GROUP BY/ORDER BY, INSERT values. View dosyası shape çözümü için SELECT de
içerir. Index/GROUP/ORDER/UPDATE listeleri tek mevcut sütunu tekrarlar; böylece
M+1 olmayan sütuna erişimden doğan farklı hata ile karıştırılmaz. INSERT M+1
dosyasında geçerli M kolonlu tablo kullanıldığı için değer/tablo arity farkı
da vardır; tek hata nedeni veya hata code/stage'i kanıtlandığı iddia edilmez.
Her beklenti manifestte **unverified/NOT_RUN** olarak işaretlidir.

Gelecekte C O2 `SQLITE_MAX_COLUMN=128/2000/4096` konfigürasyonları gerekir;
bu görevde compiler çalıştırılmaz. Yerel C 3.54.1 UUID
`555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76` yalnız
referans kimliğidir. Runtime limit PRAGMA değildir; API tarifleri sonraki
adımdadır. Portun mevcut 2000 tavanı bu dosyalarla değiştirilmez; 4096 senaryosu
C özel konfigürasyonunu hedefler. 32767 ağır profil otomatik üretilmez.

Math fonksiyonları ve bazı core özellikler gelecekte capability/compile
option farkı gösterebilir; math-function-probe manifest kaydı gelecekte C
`-DSQLITE_ENABLE_MATH_FUNCTIONS` ve math capability gerektirir. Bu görevde
probe bile çalıştırılmaz. Extension,
WAL çok bağlantı/süreç, crash/recovery ve API uygulama tarifleri adım 2'ye
bırakılmıştır; bu adımda uygulanmış/koşulmuş değillerdir.

## Durum

Yalnız statik üretim ve dosya/içerik/hash/sayım/determinism incelemesi vardır.
SQL doğruluğu, beklenen error, C/Rust eşdeğerliği, runtime limitler, wide DB
kayıt sayısı ve performans **NOT_RUN**. Eksik/unsupported/skip gelecek çalışmada
ayrı raporlanacaktır; burada hiçbir case PASS sertifikası üretilmez.
Adım 1 ebeveyn statik içerik/hash/sayım incelemesi tamamlandı; SQL/test yürütülmedi.
