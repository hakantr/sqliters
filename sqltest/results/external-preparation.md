# Adım 1 — Frozen input / pinned C / küçük preflight

Hazırlık tamamlandı; ebeveyn kaynak/veri/Git kapsamı incelemesi ve bağımsız
13 Python kontrolü + 32 küçük C preflight yeniden çalıştırması geçti. Benchmark/latency/calibration
**NOT_RUN**; sqliters derlenmedi veya çalıştırılmadı. Sonraki 2A actual runner
review/commit, ardından ayrı 2B C-only ölçüm adımı gerekir.

- Pinned C3.54.1 O2 shared+CLI MAX_COLUMN128/2000/4096, aynı kaynak/object/config.
  Hash/env/compiler/make/tcl/source snapshot/generation identity kayıtlıdır;
  özgün C kaynağına yazılmadı. Native Python process TMPDIR/SQLITE_TMPDIR
  CDLL öncesinde artifact içine ayarlanır.
- `external-input-manifest.json`: 522 fiziksel ignored input; toplam
  1.436.732.280 byte. Manifest SHA-256:
  `d5b9012b7ee65e8902957f27e1948642f593bc71174e15f79b9bd40c5005a9b1`.
  Üretici kaynakları başta/sonda pinli, state FROZEN ve manifest SHA kapılıdır.
- Üç seed×10k normal +1000 tek-row subset +ayrı unique-id append ve32 smoke;
  standard CSV/null flags/hex BLOB, native shell .import/.read sürücüleri,
  literal per-row/batch/transaction TXT ve expected canonical dosyaları.
- Wide128/2000/4096×3000 CSV, literal bütün alan INSERT ve canonical expected
  fiziksel hazırlanmıştır; eski CTE wide dosyası benchmark loader değildir.
- Frozen UUIDv7 üst3000/detay12000 her seed; PK NOT NULL, FK ON, index/JOIN ve
  no-index/aggregate expected, üç seed×8/24 smoke; RFC9562 A.6 bağımsız vector.
- Frozen ana timing order162 kayıt; UUID CLI-query order36 kayıt. Bu kayıtlar
  ölçülmüş sample değildir. Measurement implementation ready=false kapısı
  ölçümü engeller; 2A'da tam runner/case coverage incelemesi gereklidir.
- 13 harici Python harness kontrolü geçti; 32 küçük C preflight kaydı PASS:
  gerçek shell CSV raw roundtrip + typed transform/.read, native bind/limit/
  error stage ve cleanup diagnostics, üç tek-wide-row, UUID FK/duplicate ve
  küçük LIVE client-UUID4 / DB-default UUID4 UTC invariants.

LIVE smoke her modda8 üst/24 detay; requested payload ve doğrulanmış server
id/time üzerinden aynı-run event oracle kullanır. Cross-run data/result SHA
oracle yoktur; eventloglar ignored. Source/binary SHA yalnız provenance'dır.

Full75 core/72 boundary/optional/mutation/wide3000/UUIDfull ve gerçek LIVE1000/
3000, controlled WAL/crash/API tarifleri henüz koşulmadı: NOT_RUN. Özellikle
küçük preflight, tam SQL/stress/uyumluluk veya performans PASS'ı değildir.

## Çalıştırılan hazırlık komutları

```sh
python3 sqltest/sqlitebuild.py
python3 sqltest/generate.py
python3 sqltest/freeze_inputs.py
python3 sqltest/harness_controls.py
python3 sqltest/external.py preflight
```

Hiçbir `measure`/candidate/sqliters komutu çalıştırılmadı. Data/goldens/raw/
DB/binary ignored; yalnız kod/protokol/manifest/küçük özet commit içindir.
Üç eski tracked sample'ın scoped silinmesi ebeveyn index işlemidir:
`examples/normal-sample.csv`, `.jsonl`, `.sql`. Kopyalar ignored artifact'tadır;
üretici yeni örnekleri ignored konuma yazar. History rewrite/push yoktur;
eski remote/local commit'lerdeki örnekler silinmiş sayılmaz.
