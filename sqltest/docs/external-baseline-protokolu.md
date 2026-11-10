# Harici Python / pinned C SQLite baseline protokolü

Kullanıcının son talimatıyla C-only yürütme artık aşağıdaki sırayla yetkilidir.
**Bu adım 0 yalnız protokoldür:** veri/build/SQL/benchmark çalıştırılmadı.
Rust `#[test]` yok; sqliters kaynaklarını değiştirme/derleme/çalıştırma yoktur.
Önceki statik teslimin NOT_RUN kayıtları değişmez. Tek alt ajan; ebeveyn
review/düzeltme/commit sonrasında sonraki adım. Push/history rewrite yoktur.

| Aşama | Somut teslim ve kapı |
| --- | --- |
| 1 — Ön hazırlık/freeze | Standart CSV/TXT + `.read` setlerinin fiziksel üretimi, SHA/count manifesti, dış Python runner ve native pinned C shared+shell build; küçük doğruluk/preflight kontrolü. Performans yok. Kaynak/config/runner review edilip commit edilmeden ölçüm yok. |
| 2 — C-only baseline | Yalnız dondurulmuş dosyalarla gerçek shell `.import`/`.read`, C API per-row/batch; typed doğruluk, mevcut SQL/negatif/limit ve stress baseline'ı, ham süreler ve küçük rapor. sqliters çalıştırılmaz. |

## Referans ve girdilerin dondurulması

C kaynağı `/home/hakanbiris/github/sqlitesimd/sqlite`, 3.54.1 UUID
`555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`.
Kaynak yalnız okunur; SQLite AGENTS kuralları geçerlidir. SQLtest artifact
snapshot/build altında cc O2, shared library ve gerçek CLI aynı amalgamation
ve seçeneklerle üretilir. Her M=128/2000/4096 için ayrı build kimliği:
`SQLITE_MAX_COLUMN=M`, `SQLITE_THREADSAFE=1`, `SQLITE_ENABLE_FTS5`,
`SQLITE_ENABLE_RTREE`, `SQLITE_ENABLE_MATH_FUNCTIONS`; shared için PIC.
JSON omit edilmez; loadable dış extension indirilmez/çalıştırılmaz.
Tam compiler/link argv/env/sürüm ve binary, source-tree, generated source SHA,
source_id/version/compile_options kaydedilir. Cache varlığı yeterli değildir.
ctypes sistem libsqlite'ı bulup kullanamaz; yalnız kaydedilmiş yerel shared
path/hash. CLI da aynı build/config kimliğine bağlı olmalıdır.

Girdiler adım 1'de tamamen üretilir; yeni generator ayrı kod olabilir, mevcut
statik geçmişi sessizce değiştirmez. Ölçümde generator/random yoktur. **Önceden
CSV/TXT'de bulunmayan veri, INSERT SQL'i veya senaryo çalışma anında üretilmez.**
CSV okumak, tür dönüşümü ve frozen template'e bind etmek veri üretimi değildir;
hangi kısmın timed olduğu açıkça etiketlenir.

- Aynı şema/desen için seed 20261120/20261121/20261122: her seed'de 10.000 normal
  kayıt. Virgül, çift tırnak, quoted newline/CRLF, boş string, Unicode ve literal
  `\N` hedeflenir; yeni native import CSV'si standart UTF-8 CSV'dir, eski
  custom-backslash CSV doğrudan shell `.import` girdisi sayılmaz.
- NULL ve empty ayrımı için açık null flag kolonları; BLOB için hex kolon,
  NUL/invalid byte için hex/API edge dosyaları. `.import` sihirli `\N`→NULL
  dönüşümü yapmaz. CSV raw alan roundtrip ve typed tablo iki ayrı case'tir.
- Normal/ilişkili veri, 128/2000/4096×3000 geniş fixture ve mutation/scan SQL
  girdileri önceden üretilir. Geniş satır/INSERT içeriği, query/DDL/TXN template
  ve `.read`/`.import` driver dosyaları da frozen ignored girdidir. Hedef kayıt
  sayısı ayrıca runtime'da doğrulanır; smoke full/stress yerine geçmez.
- Küçük committed input manifesti: path, SHA/byte, logical CSV kayıt/alan sayısı,
  delimiter/quote/encoding/line ending, seed, schema, null/blob kuralı,
  expected case/profile/config anahtarları. Tekrarlanabilir üretim kodu kalır;
  verinin kendisi Git'e girmez. Freeze SHA'sı ve reviewed runner/config commit'i
  her koşunun giriş kapısıdır; drift veya eksik dosyada ölçüm başlamaz.

## Ölçüm aileleri ve yöntem

Ana ingestion matrisi max2000, üç seed, aynı frozen 10k input ve canonical tablo.
Tek-row API senaryoları önceden seçilmiş ilk 1000 satırın ayrı frozen dosyasını
kullanır; bu kapsam full 10k/bulk senaryosuyla karıştırılmaz.
max128/max4096 build'leri kendi sınır ve geniş stress correctness matrisinde;
girdi hacmi/config farklıyken birbiriyle performans oranı çıkarılmaz.

| Aile / etiket | Gerçek iş ve timed sınır |
| --- | --- |
| `c-cli-import-e2e` | Yeni native shell process, frozen `.read` driver ve gerçek `.import --csv --skip 1` ile TEXT staging; typed transform ayrı alt etiket. Süre spawn/open/CSV parse/import/commit/process exit içerir. |
| `c-cli-read-insert-e2e` | Frozen per-row INSERT TXT/SQL, gerçek shell `.read`; autocommit ve batch/single transaction driver'ları ayrı. SQL metni runtime'da oluşturulmaz. |
| `c-cli-persistent-sql` | Aynı Python pipe → açık shell connection → önceden yazılmış per-row INSERT akışı; autocommit/batch ayrı. Süre/veri byte sayısı ve completion ACK tarifi config'te sabitlenir; statement gönderilmesi tamamlandığı anlamına gelmez. |
| `c-api-exec-autocommit` | Frozen per-row SQL ifadeleri; ctypes sqlite3_exec/prepare-step-finalize, her row autocommit. Python loop/native call maliyeti dahildir, saf engine CPU değildir. |
| `c-api-prepared-autocommit` | Önceden dosyada bulunan template, CSV'den önceden okunmuş değerler; bind-step-reset döngüsü ve her row commit. |
| `c-api-prepared-batch` | Aynı template/veri; 1000 satırlık dondurulmuş chunk sınırları, BEGIN/COMMIT. Python'daki executemany-benzeri native bind/step/reset döngüsüdür; sqlite3 C API'sinde hayalî executemany fonksiyonu yoktur. |
| `c-api-prepared-one-txn` | Aynı input/template, bütün 10k satır tek transaction. |

API tuple decode/CSV parse, schema kurma ve baseline verification timed kernel
dışında; CSV read/decode wall ayrıca kaydedilir; bind-loop/reset/commit dahildir. Prepared statement'ın prepare süresi
ayrı kaydedilir, loop oranına sessizce katılmaz. API Text/BLOB buffer ömrü ve
sqlite3_column_* verisinin sonraki step/reset'ten önce kopyalanması korunur;
NULL ile zero-length BLOB, column_type ve byte uzunluğuyla ayrılır.

CLI `.import` önce bütün alanları TEXT staging'e yükler; ilk kabul alan sayısı,
logical row count ve bütün raw CSV alanlarının roundtrip eşitliğidir. Sonra
frozen typed-transform SQL explicit null flags/CAST/unhex kullanır. API loaded
canonical tabloyla karşılaştırma, staging ham metinle typed veri birbirine
eşitmiş gibi yapılmaz. API ve CLI typed dönüşüm/affinity politikası eşleşmelidir.

İki oturum × üç tekrar: her seed/case için altı sample; üç seed ayrı
raporlanır. Order seed `20261121`; randomize scenario sırası adım 1'de
dosyaya yazılır. Aynı senaryo için bir untimed warmup, her tekrar fresh DB.
Ana ayar WAL / synchronous=NORMAL / autockpt=0; her bağlantıda geri okunur.
DELETE/FULL ayrı duyarlılık/correctness etiketi, ana oranla karıştırılmaz.
CPU/OS/FS/disk/RAM, governor/boost/SMT/affinity/load, derleyici, runtime limit,
cache/prosedür kaydedilir. OS cache soğukluğu varsayılmaz; varsayılan sıcak
dosya cache'i etiketi. Tüm DB/temp/lock/log/raw/sqlite cache SQLtest altındadır.

Run bitiminde tam typed doğrulama timed bölge dışında; canonical source data,
ingest sonrası row/column/type/tüm içerik, PK/constraint ve integrity_check
eşleşir. C API strict int64 / IEEE bits / Text ve Blob bytes kullanır.
CLI doğrulama sorguları typeof + kayıpsız Text/Blob hex + integer ve sonlu
REAL roundtrip temsilini açıkça taşır; tam REAL bits/NaN/-0 sözleşmesi CLI'de
sağlanamazsa o alt kapsam NOT_RUN/API-only olur, bit eşitliği iddia edilmez.

Kayıtlar: ham tekrar/row/batch süreleri, ns/row, rows/s, toplam wall ve dağılım.
Tek-row API için her 1000 gerçek ayrı işlem latency'si kaydedilir; P50/P95
bunların dağılımıdır. Batch1000 ve tek-transaction10k kendi total/batch latency
metriğidir; bulk P95 sahte per-record P95 değildir. Başarıyla
tamamlanan işlem/satır/alan sayısı, raw input/output SHA ve stderr/exit; timeout/
resource ve failed örnekler korunur. Vaka bütçesi 600 s, full run 3600 s,
önerilen RSS 2 GiB/disk 8 GiB; uygulanamayan limit açıkça kaydedilir.
Bu baseline için bir hız kazanımı eşiği veya Rust kazancı iddiası yoktur.

## Mevcut SQL/stress ve gelecekte candidate contract

128/2000/4096×3000 geniş doğruluk her field tüketilerek en az bir tam koşuda
zorunludur; ağır wide tekrar zamanı varsayılan six-sample ingestion matrisine
katılmaz. Tek correctness koşusunun wall kaydı repeated benchmark değildir.
İleride wide timing açılacaksa tekrar ve kaynak bütçesi ilk süreden önce
ayrı config/commit ile sabitlenir.

Önceki 75 temel, 72 boundary / ek optional ve geniş workload katalogları C-only
doğruluk matrisine alınır; yeni freeze, kaydedilmiş case/config/seed sayısını
tam tanımlar. Beklenen negatif hata primary/extended/message/offset ve
prepare/step/transaction etkisiyle denetlenir. Kontrollü WAL için uygulanmış
C-only orchestrator varsa ayrı gerçek case olarak kaydedilir. Crash/diğer API
tarifi için gerçek orchestrator yoksa NOT_RUN kalır; kaynak dosyası varlığı PASS değildir.
Run başlangıcı RUNNING; final PASS yalnız tamamlanan açık alt kapsam içindir.
DIFF/EXPECTED_ERROR/NOT_SUPPORTED/NOT_RUN/TIMEOUT/RESOURCE/HARNESS_ERROR ayrıdır;
skip veya unsupported başarılı test gibi sayılmaz. Eski PASS hata sonrası kalmaz.

Gelecekte kullanıcı Rust binary verirse baseline yeniden üretilmeden immutable
input+expected bundle okunabilir: engine/version/binary SHA, invocation,
desteklenen CLI/API features, result wire sürümü, case/input/config anahtarları
ve boundary/error contract eşleşmelidir. Aynı dış Python komut hattı gerçek
`.read`/`.import`/persistent SQL workload'unu tekrar edebilir. Binary'nin
`.import`, parameter binding veya başka yüzeyi yoksa NOT_SUPPORTED/DIFF açık
kalır; sqliters çalıştırılarak onarılmaz veya gizlice API yerine CLI konmaz.

**C ctypes API latency ile Rust CLI process/stdout süresi oranlanamaz.** CLI
ile API ayrı comparison family'dir. API karşılaştırması için ileride ayrı
yetkili native API backend/RPC/standalone adapter, aynı batching/prepare/
transaction ve canonical typed sözleşme gerekir (Rust #[test] değil).
“Bir binary gelince bütün API ölçümleri otomatik karşılaştırılır” iddiası yoktur.
C baseline, source/input/config hash'leri ve küçük summary sayesinde tekrar
harcamadan candidate sonuçlarına bağlanır; uyuşmayan pipeline/config ayrı kalır.
Ignored input/canonical baseline bundle yerelde korunur; yalnız hash/summary
asıl sonuçların yerine geçmez. Bundle eksikse karşılaştırma NOT_RUN olur;
Git'e alınmaması artifact'in otomatik silinmesi anlamına gelmez.

## Git dışı veri ve yayın sınırı

Yeni tüm CSV/JSONL/TXT ve geniş/veri INSERT/.read SQL, DB/WAL/SHM/journal,
snapshot/shared/CLI binary, ham output ve rapor ayrıntıları ignored artifacts
altındadır. Kod, bu protokol, küçük SHA/count manifesti ve küçük result summary
commit edilir. Ignore tek başına tracked dosyayı çıkarmaz: adım 1'de mevcut
`examples/normal-sample.csv`, `.jsonl` ve veri taşıyan `.sql`
ignored alana taşınır ve yalnız bu scoped yollar parent tarafından index'ten
çıkarılır. Illustrative eski SQL kaynakları etiketli kalabilir; yeni timed
benchmark girdisi olarak committed veri kullanılmaz.

Adım 0'da taşıma/untrack/index işlemi yapılmadı. Geçmiş commit/history rewrite
ve push yok. Eski küçük örnekler Git geçmişinde ve mevcut remote geçmişinde
kalabilir; unpublished varsayılmaz, “GitHub'dan temizlendi” denmez. Yeni
girdilerin Git'e alınmaması geçmiş purge işlemi değildir ve işi durdurmaz.
Mevcut 13 dirty port dosyası ve root R-0007 dahil başka kullanıcı işleri kapsam
dışıdır. Ebeveyn her adımı inceleyip yalnız sqltest'i port main'e commit eder;
root yönerge değişikliği ayrıca kendi scoped commit'ine aittir.


## Ölçüm öncesi UUID / LIVE eki

Frozen UUIDv7: RFC9562 timestamp48/version7/rand12/variant10/rand62 alanları;
ana seed'ler 20261120/21/22, sabit base_ms=1735689600000. Her seed'de üst3000,
detay12000 fiziksel CSV/TXT; TEXT canonical36 PK/FK ve
(detail.parent_id,created_at) indeksi; literal INSERT, JOIN/aggregate ve
index/no-index query dosyaları ölçümden önce üretilir. Küçük smoke8/24 ayrı
frozen fixture'dır. Python file generator iki tabloyu da üretir.

Ayrı LIVE iki case: Python uuid.uuid4()+UTCnow client ve SQLite DEFAULT
RFC9562 UUIDv4+strftime UTC timestamp, prepared INSERT RETURNING id/timestamp.
Her koşuda üst1000/detay3000, cents integer payload ve ilişkiler; aynı
2oturum×3 tekrar planı. Live API-row/batch ayrı family, üretim latency'si,
DB write ve uçtan uca süre ayrı olacak. Ön hazırlık smoke8/24'tür; süre alınmaz.

Bu LIVE ailesi frozen-input genel kuralının açık istisnasıdır. Nondeterministic
veri/olay çıktıları ignored; **dosya/sonuç hash'i cross-run veya cross-engine
oracle yapılmaz**. Count, UUID unique/canonical/version4/variant10, UTC zaman
aralığı(start/end+tolerance2s), FK/no-orphan, JOIN count, index/no-index aynı
koşuda aynı seçili sonuç ve cents toplamı kayıtlı eventlog ile denetlenir.
Kod/source/binary SHA yalnız provenance içindir. Hiçbir Rust çalıştırması yok.


Frozen UUIDv7 JOIN performance planı: join-index ve join-noindex, her seed'de
2oturum×3 tekrar; aynı dış Python/CLI process+query+bütün çıktı family'si.
Input DB yükleme timer dışındadır. Dondurulmuş CLI-own typed SELECT dosyaları
ve order dosyası kullanılır; index/no-index bütün sonuçları aynı golden'a
uymalı. Native API query latency Rust CLI process süresine oranlanmaz.
Actual runner 2A review/commit sonrasıdır; bu hazırlıkta zaman yoktur. Index'in
her zaman hızlandırdığı varsayılmaz. RFC9562 A.6 v7 vector'ü ayrı Python
kontrolüdür; fixed epoch milisaniyesi integer+timedelta ile üretilir.


## 2A runner / audit sözleşmesi (ilk duration öncesi)

Bu kod adımında benchmark/latency/calibration çalıştırılmaz. 2A kaynak/config/
freeze/preflight ebeveynce commit edildikten sonra ayrı 2B yetkisiyle C-only
baseline başlatılır. Admission, reviewed commit'teki bütün kullanılan Python
kaynakları/config/protokol/manifest baytlarını karşılaştırır; input/source/
binary drift, başarısız freeze/preflight ve eksik golden kapıyı kapatır.

Önce tam **168 untimed correctness işi** geçmelidir: core75, boundary72,
wide3, append1, frozenUUID3, optional4, conflict3, compositeFK1, generated1,
reopen4, relational-stress1. Legacy üç wide workload aynı full-wide +
precomputed typed mutation case'ine açıkça eşlenir; eski CTE veri yükleyicisi
benchmark'a sokulmaz. WAL/crash/UDF/hooks/backup/blob tarifleri gerçek backend
olmadığından NOT_RUN; 168 sayısına sahte PASS olarak eklenmez.

Sonra her session/case/seed için bir untimed fresh-DB warmup ve **222 timed
sample**: ingestion9×3seed×2session×3repeat=162; frozenUUID forced-index/noindex
CLI query2×3×2×3=36; LIVE ikiUUIDmode×autocommit/batch×2×3=24. LIVE batch1000,
parent1000/detail3000; batch row-step commit latency değildir. Client generation,
DB bind/step/reset+commit ve e2e ayrı; DB DEFAULT UUID/time üretimi SQLite step
maliyetinin içindedir, izole generation zamanı diye uydurulmaz. API CSV read/
decode ve reused prepared-prepare loop dışında; exec-auto'da her row prepare/
finalize gerçek row süresindedir. Bulk ns/row sample ortalamasının dağılımı,
gerçek row P50/P95 olarak etiketlenmez.

Worker kendi process group'unda; 2 GiB hard RLIMIT_AS, 8 GiB file upper bound;
parent ayrıca **yalnız yeni run output sandbox'ı** toplamını 8 GiB sınırlar
(immutable input/build bundle ayrı). Case600s/global3600s bütçesi setup,
correctness ve warmup dahil gerçek deadline'dır; aşımda owned group kill/reap,
TIMEOUT/RESOURCE ve RUNNING→FAIL kaydı. PID/argv/exit/CPU affinity/load/RSS,
source/build/config/protocol/commit ve ham event/row/batch dosyaları saklanır.
Yeni run global summary'yi RUNNING/FAIL ile günceller; eski bundle korunur.

Bağımsız read-only audit ham result dosyalarından benzersiz222 anahtarı,
168 correctness matrix'i, tam type/field/hash/golden, error stage/extended/
offset/transaction durumunu ve percentile/istatistiği yeniden hesaplar. LIVE
aynı-run requested events + readback/UTC/FK/JOIN/cents ile denetlenir; live
value/file SHA hiçbir cross-run oracle değildir.

Gelecek candidate-only CLI performance **126 sample**: aynı frozen order'ın
CLI5×3×2×3=90 alt kümesi + UUID36; aynı dış Python pipeline, own SELECT ve
actual C baseline golden'ı gerekir. API/LIVE native backend yoksa NOT_RUN.
C library candidate validation yolunda açılmaz, baseline yeniden üretilmez.
C baseline ile sonraki candidate tarihi farklıdır: oran historical/descriptive;
contemporaneous A/B veya kanıtlanmış genel kazanç değildir. CPU/OS/governor/
affinity/cache ve load1 farkı (ön sınır1.0) kaydedilir; koşullar uyuşmazsa
comparable_environment=false, oran diagnostic kalır. Candidate compiler/flags
kullanıcı sağlamadıysa bilinmiyor olarak işaretlenir. Audit candidate JSON/CSV
sürelerini de raw126 dosyadan yeniden hesaplar.


2A resource sınırları yalnız owned run output için uygulanır; immutable input/
build cache 8GiB output kotasına dahil değildir. Full correctness kaynak/input
SHA'ları timingden hemen önce yeniden denetlenir. Candidate eskiCbaseline'ın
library'sini açmadan pure file audit yapar; cachedC/API/LIVE oranı uydurulmaz.

UUID CLI-query e2e her yeni query bağlantısında frozen settings/readback ve
marker'ı da içerir: WAL/NORMAL/autockpt0 doğrulanmadan typed sonuç kabul edilmez.
Bu sabit startup+configuration+query+bütün çıktı pipeline'ı C ve candidate'da
aynıdır; DB fixture yüklemesi timer dışındadır.
