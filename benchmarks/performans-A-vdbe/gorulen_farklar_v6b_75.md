# V6b.75 — Açık bind ve skaler API sınırları

Dinamik `SQLITE_LIMIT_LENGTH` API'si ve farklı giriş kodlamalarıyla C bind ayrıntıları ayrı açık kapsam. D50, varsayılan Rust TEXT/BLOB sınırını ve Tcl değerlendirme davranışını hizalar. SQL source-id düzeltmesi dışında V6b.71 skaler arite tanıları ve bazı legacy hata ofsetleri açık. [Bind veri sınırı](/github/sqliters/target/perf-work/bind-size-proof/sonuc.md) · [Skaler izler](/github/sqliters/target/perf-work/sql-source-id-proof/sonuc.md).
