# V6b.365 — Rowid görünüm UPDATE kısmi indeks koşulu

Literal koşul eşliği ve IS NOT NULL çıkarımı C ile eşleşti. 360 profil ve 250 kapanış doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v365-rowid-view-partial-index-eligibility-corrected-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1241 geçti, 1 başarısız, 329 yok sayıldı. ON 1238 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,73 ms taban, 380,83 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-view-partial-index-eligibility-disk-verify/res/gates-validation.txt).
