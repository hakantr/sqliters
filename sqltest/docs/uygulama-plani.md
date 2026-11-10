# Gelecek test uygulama sırası — yalnız tarif

**Bu dosya uygulanmış test değildir.** Bütün SQL/API/WAL/crash, derleme,
uyumluluk ve performans sonuçları **NOT_RUN**. Kullanıcının mevcut talimatı
plan + statik dosya üretimidir; bu tarifleri yürütme yetkisi vermez. Native
C runner veya Rust adapter yoktur. Mevcut port kaynakları değiştirilmez.

## Sıra ve giriş kapıları

1. Gelecekte ayrı yürütme yetkisi alındığında kaynak/config/seed kimlikleri,
   bütün dosya SHA'ları ve desteklenen özellikler dondurulur. Kullanıcı dirty
   kaynakları geri alınmaz; gerçek anlık revizyon ve dosya SHA'ları kaydedilir.
2. C referans 3.54.1 UUID
   `555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`
   yalnız SQLtest içine kopyalanıp orada out-of-source derlenebilir. Bu görevde
   kaynak kopyası, compiler veya build çalıştırılmaz. Gelecek bayrak matrisi:

   | C yapılandırması | Gelecek compiler seçenekleri | Kapsam |
   | --- | --- | --- |
   | max128 | O2, `SQLITE_MAX_COLUMN=128` | 127/128/129 sınırları |
   | max2000 | O2, `SQLITE_MAX_COLUMN=2000` | 1999/2000/2001, ortak Rust karşılaştırması |
   | max4096 | O2, `SQLITE_MAX_COLUMN=4096` | 4095/4096/4097 ve geniş özel C fixture |
   | optional | Yukarıdakilere ayrı `SQLITE_ENABLE_FTS5`, `SQLITE_ENABLE_RTREE`, gerekirse `SQLITE_ENABLE_MATH_FUNCTIONS` | Capability ve config kimliği ayrı |

   Compiler yolu/hash/sürüm, tam argv/env/link seçenekleri, bütün source,
   amalgamation ve binary hash'leri; source_id/version/compile_options ve
   etkin runtime limit geri okumaları gerekir. Cache'in yalnız var olması
   yeterli değildir. Python sistem sqlite modülü pinned C yerine geçmez.
   JSON/JSONB için JSON capability'si ve `SQLITE_OMIT_JSON` durumu ayrıca
   kaydedilir. Dış extension indirme veya load-extension çalıştırma yoktur.
3. Aynı girdiyle C smoke/negatif/boundary referans sonuçları gelecekte alınır.
   Kasıtlı `negative-*` vaka beklenen evre/kod ve son durum tutarsa beklenen
   hata sayılabilir; her SQL dosyasının başarılı olması beklenmez.
4. Gelecekte ayrı standalone Rust adapter ancak ayrıca yetkili görevde
   hazırlanır. Bu teslimde yoktur. Public prepare/bind/step/reset/finalize ve
   limit API'si kaynakla eşlenir; sahte engine yazılmaz. Motorun mevcut
   compile-time 2000 sütun sınırı değiştirilmez. >2000 C özel konfigürasyon
   sonuçları Rust'ın aynı konfigürasyonu çalıştırabildiği anlamına gelmez.
5. Önce küçük fixture, sonra 10k normal + 50k ilişkili veri, 128×3000 ve 2000×3000
   hücre, C özel 4096×3000 hücre ve indeks/mutasyon workload'ları. Her dosya
   aşağıdaki prerequisite/sandbox ayrımıyla ayrı DB kopyasında yürütülür.
6. Optional capability fixture'ları ve gerçek ayrı WAL oturumları, sonra API
   ve bounded crash tarifleri. Bütün gerekli aşamalar için ham artifact ve
   bağımsız audit olmadan genel başarı/uyumluluk iddiası yoktur.

## Profile, maliyet ve kaynak bütçesi

[profiles.json](../profiles.json) içindeki `future_resource_limits` yalnız
önerilen uygulama bütçesidir. Smoke/full/stress için vaka başı 30/180/600 saniye;
toplam 1800 saniye, 2 GiB RSS ve 8 GiB sandbox disk üst sınırı. Bunlar henüz
ölçülmüş/uygulanmış sınırlar değildir. Aşımlar sessiz küçültme yerine açık
TIMEOUT/RESOURCE kaydıdır. Ana/alt seed'ler ve veri hacmi değiştirilirse yeni
run/config kimliği gerekir; 32767 ağır profil ayrıca opt-in, bu teslimde yoktur.

Gelecek maliyet kaydı: parse/prepare, insert, index kurma, update/delete,
rollback, tüm içerik scan/tüketim, vacuum/reopen ayrı; duvar süresi, satır/alan
sayısı, max RSS, DB/WAL/SHM byte'ları ve sync/I/O sayıları. Sıcak/soğuk cache
etiketi prosedürle doğrulanmalı; bağlantıyı kapatmak OS cache'i soğutmaz.
Bu statik teslimde zaman/throughput veya hız oranı hesaplanmadı.

## Runtime sütun limiti — SQL değil API action

M compile tavanı; C için 128/2000/4096, mevcut Rust için 2000. Aşağıdaki
satırlar aynı bağlantıda sırayla gelecek API action'ıdır:

| Action | Beklenen old value | Beklenen effective | Durum |
| --- | --- | --- | --- |
| query(-1) | M | M | NOT_RUN |
| set(128) | M | 128 | NOT_RUN |
| set(64) | 128 | 64 | NOT_RUN |
| set(128) | 64 | 128 | NOT_RUN |
| set(M+100) | 128 | M (clamp) | NOT_RUN |
| query(-1) | M | M | NOT_RUN |
| set(0) | M | 0 | NOT_RUN |
| restore(M) | 0 | M | NOT_RUN |

C `sqlite3_limit(SQLITE_LIMIT_COLUMN,…)`, Rust `set_column_limit(isize)`;
PRAGMA ile değiştirme varsayılmaz. 64'e düşürülmüş limiti 128'e yeniden
yükseltmek serbesttir; compile tavanını artırmak değildir. Her pozitif etkin
limit için N−1/N/N+1 boundary SQL'i ayrı temiz bağlantı/şema durumunda denenir.
Sıfır limitte işlem hatası ayrıca tariflenir; sıfırdan sonra tablo metadata'sı
çözülememesini sütun grammar sonucuyla karıştırma. Restore her senaryo sonunda
zorunludur; bir vakadaki limit başka vakaya sızmamalıdır.

## Strict C/Rust karşılaştırma ve kabul

- Null/Integer(int64)/Real(IEEE bit)/Text(ham byte)/Blob(ham byte) tag'leri,
  bütün sütunlar, her satır ve multiplicity korunur. SQL Value::PartialEq,
  JSON float'a dönüştürme, string yuvarlama veya rowcount tek başına oracle
  olamaz. API'nin NaN/-0 normalizasyonu kaynak/evreyle belgelenir; fark sonradan
  keyfi normalize edilmez. Invalid UTF-8 erişimi ve encoding yöntemi ayrı kayıttır.
- ORDER BY ve deterministik tie-breaker bulunan sonuçlar sırayla; sırasız
  sonuçlar typed bag + multiplicity ile karşılaştırılır. Eşit anahtarların
  sırasını açıkça belirtilmediği yerde oracle yapma. Bütün wide içerik stream
  edilip tüketilir; field count + typed content hash ve ilk farkın tam değeri
  tutulur. Hash üretildi etiketi, eksiksiz stream tüketiminin kanıtı değildir.
- Prepare/step/reset/finalize evresi, primary/extended hata, message/offset,
  değişiklik sayısı/last-rowid/autocommit ve hata sonrası tablo durumu ayrı
  taşınır. Plan/EXPLAIN farkları kaydedilir, planner opcodes'u diller arasında
  birebir zorlanmaz. Aynı semantiği çalıştırmadan performans farkı yorumlanmaz.
- Bütün vaka/profil/config anahtarları, SQL/source/binary/seed hash'leri,
  argv/env, ham sonuç ve exit/signal tek run_id'ye bağlanır. Boş/duplicate/
  yarım wire, eksik fixture, stale cache ve eski PASS reddedilir; hata sonrası
  önceki PASS kalmaz. Ancak gelecekte gerçek runner/audit ile doğrulanabilir.
- Durumlar: PASS, EXPECTED_ERROR, DIFF, NOT_SUPPORTED, NOT_RUN,
  SKIP_RESOURCE, TIMEOUT, HARNESS_ERROR. Şu an **hepsi NOT_RUN**; üretilmiş dosya
  PASS değildir. C'de başarılı çekirdek vaka Rust'ta reddedilirse ham DIFF
  korunur; unsupported açıklaması farkı silmez. Faz 6/7/optional yokluğu genel
  geçişe çevrilmez. Desteklenmeyen konfigürasyon karşılaştırması ayrı etiketlenir.

## Dosya grupları

- [extra-scope.json](../extra-scope.json) yeni SQL/recipe/schedule yollarını,
  prerequisites, capabilities, beklenen durumları ve NOT_RUN'u bağlar.
- `examples/optional/`: JSON/JSONB, FTS5, R-Tree küçük veri ve komutları.
- `examples/additional/`: FAIL/ABORT/ROLLBACK ayrı transaction, composite FK/
  WITHOUT ROWID, generated column ve page reopen phase dosyaları.
- `examples/stress/relational-workload.sql`: büyük ilişkili fixture üstünde
  index/bulk update/delete/savepoint rollback/reinsert/vacuum/tam scan.
- `artifacts/generated/full/stress/wide-{128,2000,4096}-workload.sql`: geniş
  fixture üstünde ilk/orta/son alan update ve aggregate + bütün kolon scan.
- [WAL schedule](../schedules/wal.json) / [açıklama](../schedules/wal.md):
  reader/writer/checkpointer SQL'leri ayrı oturum/faz dosyalarıdır. Tek script
  diye concatenate edilmez; gerçek ACK/barrier orchestrator henüz yoktur.
- [API tarifi](../recipes/api.md), [crash/recovery](../recipes/crash-recovery.md):
  yalnız uygulama sözleşmesi, runnable adapter/orchestrator değildir.

Dosya üretimi ve statik hash/sayı incelemesi dışında motor veya süreç
yürütülmedi. Son teslim için ebeveyn incelemesi gerekir.
