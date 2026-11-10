# SQLtest — Güncel görev sınırı

Son kullanıcı talimatı önceki yalnız statik/NO-EXEC kapsamını bu görev için
**C SQLite baseline + harici Python kontrolü** olarak değiştirdi. Üst genel
salt okunur kuralın tek yazma istisnası `sqltest/` dizinidir; motor/C kaynakları,
kök Cargo ve mevcut kullanıcı işleri düzenlenmez/stage/restore edilmez.

- Rust `#[test]` yok; sqliters derlenmez/çalıştırılmaz/düzeltilmez. Gelecekte
  kullanıcı binary sağlarsa comparison tasarlanabilir; bu görevde Rust yoktur.
- Adım 0 yalnız izin/protokol revizyonudur; veri/build/SQL/ölçüm yok.
  Adım 1 önceden CSV/TXT üretimi/freeze ve runner/C build/preflight hazırlığı;
  ebeveyn review/commit'ten sonra adım 2 yalnız pinned C doğruluk/stress/süre.
- Kontrol harici Python ile; timed run random/generator veya SQL-data üretmez.
  Native C shell `.import` ve `.read` gerçek shell'de; ctypes API baseline
  ayrı metricfamily. API süresi Rust CLI process süresiyle hız oranı olamaz.
- Test verileri commit edilmez: tüm yeni CSV/JSONL/TXT, geniş SQL/veri `.read`
  setleri, DB/binary/raw çıktılar ignored `artifacts/` altında. Kod/protokol,
  küçük SHA/count manifesti ve sonuç özeti commit içindir. Tracked küçük sample
  veri dosyalarını adım 1 scoped ignore/untrack planında ele alın; history
  rewrite/push yok. Eski illustrative SQL kaynakları benchmark girdisi sayılmaz.
- C kaynakları özgün ağaçta yalnız okunur; snapshot/build/shared library/CLI,
  cache/temp/DB/log yalnız sqltest içinde. Aynı pinned build'den ctypes ve CLI;
  MAX_COLUMN128/2000/4096, sabit O2/config ve hash kontrollü cache.
- Symlink/helper ile dışarı yazma yok. Tek alt ajan, ebeveyn review/düzeltme/
  adım commit kuralı sürer. Alt ajan commit/branch switch yapmaz; port main'de
  yalnız sqltest kapsamının commit'ini ebeveyn yönetir.

Ayrıntı: [External baseline protokolü](docs/external-baseline-protokolu.md).
Önceki statik dosyalar yürütülmüş test sonucu değildir; NOT_RUN geçmişi korunur.
