# V6b.69 — Açık CHECK eşlik maddeleri

Doğrudan BLOB sütununda fiziksel kayıt ve literal schema default ayrımı ölçüldü. `CHECK(+v)` ve doğrudan VIRTUAL BLOB örnekleri önceki Rust davranışını koruyor ve C'den ayrışıyor. Bu ifade türlerinde sonuç değerinin C hücre kodlaması ayrı inceleme gerektiriyor. [Ham sınır kayıtları](/home/hakan/github/sqliters/target/perf-work/check-blob-origin-proof/res/native/boundaries.json).

`ALTER TABLE ADD COLUMN DEFAULT` yürütmesi ve `ignore_check_constraints` legacy yazma kararı açık. C, ALTER sırasında eklenen fiziksel alan yoksa varsayılanı UTF-8 ile değerlendiriyor. Yeni INSERT'in fiziksel değer kararı veritabanı kodlamasına bağlı. [ALTER ve flag karşılaştırması](/home/hakan/github/sqliters/target/perf-work/check-blob-independent/res/sonuc.md).

İlk kesinti adayı, aktif hazırlamada dört sözdizimi vakasının hata önceliğini değiştirdi ve Group46'nın eski idle kesinti beklentisi tam koşuda ayrıştı. Kesinti yamaları bu adımdan ayrı tutuldu. [Bekletme kaydı](/home/hakan/github/sqliters/target/perf-work/interrupt-lifecycle-verify/withheld/durum.txt).
