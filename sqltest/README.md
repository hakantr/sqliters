# SQLtest — Harici Python doğruluk ve performans paketi

Kontrol harici Python ile yapılır; Rust `#[test]` yazılmaz. Bu görevde yalnız
pinned C SQLite çalıştırılır; sqliters derlenmez veya çalıştırılmaz.

Güncel durum: **C-only 2B baseline PASS_SCOPED.** İncelenmiş `5ef303d`
koduyla 168 doğruluk işi, 74 warmup ve 222 süre örneği tamamlandı;
bağımsız audit ve 222 salt okunur DB integrity kontrolü geçti.
[Ölçülmüş sonuç ve sınırlar](results/external-baseline.md).
Sonuç dosyaları ebeveyn incelemesi bekliyor. Rust çalıştırılmadı;
WAL çoklu süreç/crash/ileri API tarifleri NOT_RUN kalır.

## Veri ve oracle ayrımı

- Sabit CSV/TXT: üç seed, 10.000 kayıtlık normal dosyalar, 1000 kayıtlık tek
  işlem/append alt kümeleri; virgül, tırnak, newline/CRLF, Unicode, NULL bayrağı
  ve hex BLOB. Gerçek `.import` raw TEXT staging ve typed dönüşüm ayrı kontrol edilir.
- Geniş girdiler: 128/2000/4096 sütun × 3000 satırın bütün değerleri fiziksel
  dosyadadır. Ölçüm sırasında CTE/random ile kayıt üretilmez.
- Sabit UUIDv7: her seed'de 3000 üst/12000 detay, PK/FK, indeksli/indekssiz
  JOIN ve aggregate. Bu sabit ailede tam typed içerik ve SHA karşılaştırılır.
- Ayrı LIVE: Python UUIDv4/UTC ve DB DEFAULT UUIDv4/UTC; 1000 üst/3000 detay,
  autocommit/batch. Değer/dosya hash'i farklı koşuların oracle'ı **değildir**.
  İstenen payload, aynı koşunun geri okuması, UUID/zaman/FK/JOIN kuralları denetlenir.

**Test verileri Git'e girmez.** 542 frozen input/operator/golden dosyası
(1.773.183.640 byte), DB/binary, canlı olaylar ve büyük ham sonuçlar ignored
`artifacts/` altındadır. Kod, SHA/count manifestleri ve küçük sonuç özetleri
commit edilir; push yapılmaz. Önceki küçük sample dosyaları izlemden çıkarıldı;
eski local/remote commit'lerdeki örnekler history rewrite yapılmadan kalır.

## Komutlar

Port kökünden hazırlık (mevcut baseline saklanacaksa girdileri yeniden üretmeyin):

```sh
python3 sqltest/generate.py
python3 sqltest/sqlitebuild.py
python3 sqltest/freeze_inputs.py
python3 sqltest/harness_controls.py
python3 sqltest/external.py preflight
```

Tamamlanan tarihsel C-only baseline ve mevcut kanıtın denetimi:

```sh
python3 sqltest/external.py measure --reviewed-commit 5ef303d --output sqltest/artifacts/external/runs/c-baseline-001
python3 sqltest/audit.py sqltest/artifacts/external/runs/c-baseline-001
```

İlk komutun mevcut çıktı dizini doludur; tekrar ölçüm için ayrı, gözden
geçirilmiş bir koşu gerekir. Kanıtı incelemek için ikinci komut yeterlidir.

Koşu önce 168 tam correctness işini geçirir; sonra 162 ingestion +36 UUID
CLI-query +24 LIVE örneğini alır. İki oturum × üç tekrar ve oturum başına
untimed warmup; tek-row latency, batch/tek transaction, üretim/DB/e2e ayrı
metriklerdir. CLI e2e ile native API süreleri birbirine oranlanmaz.

Rust portu tamamlanıp kullanıcı ikiliyi sağladığında **yalnız aday CLI**:

```sh
python3 sqltest/external.py compare --engine sqliters --binary /path/to/sqliters --baseline sqltest/artifacts/external/runs/c-baseline-001 --output sqltest/artifacts/external/runs/sqliters-001
```

Aday kendi `.import`/`.read`/persistent SQL ve SELECT sonuçlarını üretir;
C kütüphanesi açılmaz, C baseline yeniden çalıştırılmaz. 126 CLI örneği
kaydedilmiş C sonuçlarıyla karşılaştırılır; farklı tarihlerdeki süre oranları
historical/diagnostic'tir. API/LIVE ve core API catalog'u için uyumlu native
backend gerektiğinde açık NOT_RUN kalır. Eksik CLI özelliği PASS sayılmaz.
Yerel frozen input ve C golden bundle korunmalıdır; hash tek başına onun
yerini tutmaz. Eksik bundle'da karşılaştırma başlamaz.

## Harita ve kapsam

- [Protokol](docs/external-baseline-protokolu.md), [config](external-config.json),
  [input manifesti](external-input-manifest.json).
- [Hazırlık](results/external-preparation.md), [2A uygulama kaydı](results/external-implementation.md),
  [kontroller](results/external-controls.json), [preflight](results/external-preflight.json).
- [C baseline raporu](results/external-baseline.md),
  [kanıt indeksi](results/external-baseline-index.json),
  [aggregate süreler](results/external-timing-summary.csv).
- `examples/schema.sql`, `seed.sql`, `scenarios/*.sql`, `large-join-queries.sql`:
  küçük kaynak örnekleri ve SQL aileleri; [eski scope](scope-matrix.json).
- `examples/optional/`, `additional/`, `stress/`: JSON/JSONB/FTS5/R-Tree,
  conflict, generated columns, page-size/reopen ve mutation örnekleri.
- [WAL schedule](schedules/wal.json), [API](recipes/api.md),
  [crash/recovery](recipes/crash-recovery.md): gerçek backend'i olmayan
  tarifler NOT_RUN olarak kalır; genel sistem/port uyumluluğu iddiası yoktur.

Özgün C kaynakları ve port motor dosyaları değiştirilmez; bütün çalışma
çıktıları sqltest içinde kalır. [Görev sınırı](AGENTS.md).
