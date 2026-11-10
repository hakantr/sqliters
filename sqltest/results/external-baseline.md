# C SQLite baseline — 2B

**PASS_SCOPED.** 2026-11-10 tarihinde yalnız pinned C SQLite çalıştırıldı.
İncelenmiş kod commit'i `5ef303d`; koşu
`artifacts/external/runs/c-baseline-001` altındadır. Ebeveyn bağımsız audit, metadata SHA ve 222 salt okunur integrity sonucunu doğruladı.
Rust portu derlenmedi/çalıştırılmadı; motor kaynakları düzenlenmedi.

## Gerçekleşen kapsam

- Süre alınmadan önce 168 doğruluk işi: 134 PASS, 34 EXPECTED_ERROR.
  Bunlar 75 core, 72 boundary, üç tam geniş tablo, append, üç UUID fixture,
  dört optional, üç conflict, composite FK, generated column, dört reopen
  ve relational mutation işidir. Beklenen hatalar başarı diye gizlenmedi;
  primary/extended kod, evre, offset ve transaction durumu ham kayıttadır.
- 128/2000/4096 sütun × 3000 satırın sırasıyla 384.000 / 6.000.000 /
  12.288.000 alanı, hem başlangıç hem mutation sonrasında typed karşılaştırıldı.
  Geniş veri yükleme/doğrulama süresi benchmark sonucu değildir.
- Append geri okuması 13.000 kayıt/91.000 alanı kapsar. UUIDv7 için üç seed'in
  her birinde 3000 üst/12.000 detayın tüm alanları, JOIN/aggregate ve
  indeksli/indekssiz sonuçlar doğrulandı. Her seed'de orphan FK (787) ve
  duplicate PK (1555) step hataları da denetlendi.
- 74 süre dışı warmup sonrasında 222 benzersiz örnek: 162 ingestion,
  36 UUID CLI-query, 24 LIVE. Her case/seed için iki oturum × üç tekrar.
  LIVE'da her örnek 1000 üst/3000 detay içerir; autocommit ve batch ayrıdır.
- Runner audit'i ve ayrıca çalıştırılan bağımsız `audit.py` geçti.
  Süreler bittikten sonra 222 sample DB'nin tamamı pinned MAX_COLUMN=2000
  C CLI ile `-readonly ... 'PRAGMA integrity_check;'` sonucunda exit 0,
  stdout `ok`, boş stderr verdi. Komut/PID/CLI SHA ve sonuçlar ignored
  `post-integrity.json` içindedir; bu kontrol süre örneklerini değiştirmedi.

## Sürelerin anlamı

AMD Ryzen 7 8845HS, CPU affinity 2 (SMT kardeşi 3), Linux 7.3.1,
Python 3.14.9, GCC 16.2.1, governor `powersave`; sıcak dosya cache'i.
Başlangıç load-average 2,90/2,14/2,13; bitiş 1,58/1,85/2,00.
WAL/NORMAL/autocheckpoint=0 bağlantı ayarları geri okunmuştur. SQLite 3.54.1,
source-id `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`;
`-O2 -fPIC`, THREADSAFE=1, FTS5/RTREE/math; üç MAX_COLUMN derlemesi.
Tam ortam, derleme kimliği ve örnek başına load/RSS ham kayıttadır.

Aşağıdaki değerler üç seed'deki ayrı altışar örneğin **toplam süre
medyanlarının aralığıdır**; farklı API/CLI yollarının hız oranı değildir.

| Ingestion yolu | Kayıt/örnek | Toplam medyan aralığı (ms) |
|---|---:|---:|
| API execute/autocommit | 1000 | 22,635–23,460 |
| API prepared/autocommit | 1000 | 25,405–26,882 |
| API prepared/1000'lik batch | 10.000 | 111,447–121,170 |
| API prepared/tek transaction | 10.000 | 113,356–121,441 |
| CLI `.import` | 10.000 | 33,520–34,714 |
| CLI `.read`/autocommit | 1000 | 33,654–34,673 |
| CLI `.read`/batch | 10.000 | 44,265–44,459 |
| CLI persistent/autocommit | 1000 | 14,724–16,211 |
| CLI persistent/batch | 10.000 | 21,573–21,855 |

API ölçümü Python/ctypes bind/step/reset/commit maliyetini kapsar; engine-only
değildir. CSV okuma/decode ve reusable prepare ayrı kaydedilmiştir.
Execute/autocommit'te her satırın prepare/finalize maliyeti döngüye dahildir.
CLI `.read`/`.import` startup ve gerçek dosya I/O'sunu; persistent yol
pipe/SQL/ACK maliyetini kapsar. Import typed dönüşümü süre dışındadır.

UUID JOIN CLI-query medyanları indeks zorlanınca 31,833–33,016 ms,
indekssiz 26,968–28,252 ms oldu. Bu sorgu/fixture'da indeksli yol daha
yavaştır; genel indeks veya Rust performansı sonucu çıkarılmaz. DB yükleme
süre dışı; CLI başlangıcı, query ve tüm çıktı süre içindedir.

LIVE toplam medyanları client UUID4 autocommit/batch için
275,248/123,988 ms; DB-default UUID4 için 239,974/90,708 ms'dir.
Gerçek autocommit satır P50/P95 değerleri sırasıyla client'ta
43,893/106,303 µs, DB-default'ta 41,007/101,083 µs'dir. Batch toplamından
bölünmüş ns/kayıt gerçek satır P95 değildir. Client UUID/zaman üretimi,
DB yazma ve e2e süreleri ayrı; DB-default üretimi SQLite step'inden ayrılamaz.
LIVE doğruluğu istenen payload + aynı koşu geri okuması/UUID/UTC/FK/JOIN
invariantlarıdır; canlı değerlerin hash'i farklı koşulara oracle yapılmaz.

37 case/seed grubunun tüm medyan/P95 metrikleri ve gerçek satır örnek
sayıları [aggregate CSV](external-timing-summary.csv) içindedir.
[İndeks](external-baseline-index.json), input manifesti, kod/derleyici kimlikleri
ve ham metadata SHA'larını bağlar; LIVE veri/dosya hash oracle'ı içermez.
Yalnız altı örnekli bu tarihsel ölçüm bir hızlanma veya contemporaneous A/B
kanıtı değildir; gelecekteki CLI oranları ortam eşleşmesine göre tanısaldır.

## Denetim ve gelecekteki karşılaştırma

Port kökünde kullanılan komutlar:

```sh
python3 sqltest/external.py measure --reviewed-commit 5ef303d --output sqltest/artifacts/external/runs/c-baseline-001
python3 sqltest/audit.py sqltest/artifacts/external/runs/c-baseline-001
python3 sqltest/artifacts/external/runs/c-baseline-001/post-integrity.py
```

İlk komut tamamlanmış tarihsel koşudur; aynı çıktı dizinine tekrar yazılmaz.
Saklanan girdiler/golden bundle korunarak, kullanıcı Rust ikilisini daha sonra
verdiğinde şu komut kullanılabilir; bu teslimde çalıştırılmadı:

```sh
python3 sqltest/external.py compare --engine sqliters --binary /path/to/sqliters --baseline sqltest/artifacts/external/runs/c-baseline-001 --output sqltest/artifacts/external/runs/sqliters-001
```

Adayın kendi CLI SELECT/readback yolu kullanılır; C baseline yeniden
çalıştırılmaz ve C kütüphanesi aday DB'sini okumaz. Eksik özellik/bundle
PASS sayılmaz. API/LIVE için uyumlu backend gerekir. WAL çoklu süreç,
contention, crash/recovery ve ileri API/uygulama tarifleri **NOT_RUN**;
WAL journal ayarını kullanmış olmak bu sistem tariflerinin geçtiği anlamına
gelmez. Genel port/SQL eşdeğerliği veya tüm olası kombinasyonlar iddiası yoktur.

Ham sonuçlar, CSV/TXT/SQL girdileri, canlı olaylar, golden'lar, DB ve binary
ignored `artifacts/` altındadır. Yeni commit'e yalnız kod/metadata/aggregate
özetler girer; eski veri içeren Git geçmişi değiştirilmedi, push yapılmadı.
