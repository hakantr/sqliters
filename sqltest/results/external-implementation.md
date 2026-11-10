# Adım 2A — Kod ve untimed preflight teslimi

Ebeveyn kaynak/Git kapsamı incelemesi ve bağımsız 24 Python kontrolü +
45 küçük C preflight yeniden çalıştırması geçti. **Benchmark, latency veya kalibrasyon çalıştırılmadı.**
Rust derleme/yürütme yoktur. Tam C baseline ancak reviewed2A commit ve ayrı
2B talimatından sonra çalıştırılabilir.

- Frozen input542 dosya /1.773.183.640 byte. Manifest SHA-256:
  `69e61e7d30667d6170493fb4103646792c0808494b69712bcc16179831aa5137`.
- 24 harici Python kontrolü PASS; genişletilmiş45 küçük C preflight kaydı
  PASS. Bunlar23'ten fazla kontrol içeren yeni actualsource/freeze'e bağlıdır;
  eski13/32 kayıtları bu sürümün kontrol sayısı değildir.
- Preflight: 32-row ingestion backend'leri, birer wide row, frozen UUID8/24,
  autocommit ve batch LIVE smoke8/24, C CLI ile candidate-own-SELECT yolu,
  bounded untimed UUID worker. Perf clock çağrısını yasaklayan kontrolle
  untimed yollar sınandı. Gerçek benchmark sample sayısı **0**.
- Kod planı: önce168 full untimed correctness işi, sonra222 sample
  (ingestion162 +UUID36 +LIVE24), per-session fresh warmup. Full işler bu
  adımda çalıştırılmadı; hepsi NOT_RUN.
- Eski üç legacy wide CTE case'i explicit replacement map ile precomputed
  CSV + full content + typed mutation worker'a bağlanır. WAL/crash ve
  advanced API recipe'leri uygulanmadığı için NOT_RUN; genel PASS sayılmaz.
- Candidate CLI performance126 sample + own SELECT/golden/audit/JSON/CSV;
  C baseline yeniden üretilmez ve candidate DB C API ile okunmaz. API/LIVE
  matching backend yoksa NOT_RUN. Farklı tarihli oranlar descriptive/diagnostic,
  contemporaneous A/B değildir. Bu candidate komutu burada çalıştırılmadı.

## Kod haritası

- `benchmark.py`, `external_worker.py`, `gates.py`: reviewed commit/input gate,
  source/build identity, fresh process-group worker, actual resource/deadline,
  RUNNING→PASS_SCOPED/FAIL ve current summary invalidation.
- `correctness.py`:75 core +72 boundary +3 wide +append1 +UUID3 +optional4
  +conflict3 +FK1 +generated1 +reopen4 +relational mutation1 =168 işler.
- `analysis.py`, `wireio.py`, `audit.py`: strict typed/row/batch/raw matrix,
  independent raw statistics and cached golden integrity; LIVE request-based
  same-run validation, no cross-run value SHA oracle.
- `candidate_benchmark.py`, `candidate_worker.py`, `compare_cli.py`: actual
  external candidate CLI pipeline, historical126 sample comparison, own SELECT.
- `external.py`, `live.py`, `freeze_inputs.py`, `support.py`, config/protocol:
  native API timing scope, LIVE generation/db/e2e distinction, operator files,
  per-connection config readback, all-used-Python source hashes.

Stage1'deki522 input path'i korundu. CSV/literal row input/canonical value
files byte-identical; yalnız84 `.read`/`.import`/persistent-start/UUID query
operator dosyası readback/marker için değişti.20 ek dosya: mapped wide mutation
operators+goldens, own raw verification/transform, UUID CLI load, LIVE operator
SQL ve frozen LIVE order. Yeni veri/golden/raw/DB/binary ignored kalır.

## Bu adımda çalıştırılanlar

```sh
python3 sqltest/freeze_inputs.py
python3 sqltest/harness_controls.py
python3 sqltest/external.py preflight
```

Bütün source/config/protocol/preflight/input SHA'ları aynı freeze'e bağlı;
sonrasında bunlar değiştirilmedi. Yalnız README ve bu SHA-bağsız teslim kaydı
son durum için güncellenebilir. Sonraki gerçek komutlar README'dedir ve şimdi
çalıştırılmadı. Parent review/commit beklenir; alt ajan commit/push yapmadı.
