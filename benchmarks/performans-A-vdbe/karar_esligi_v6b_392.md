# V6b.392 — Özyinelemeli FIFO UNION yerleşik kolasyonları

300 gözlemde NOCASE/RTRIM ve karma büyük kayıtlarla 40 UTF-16 özyinelemeli FIFO sonucu C ile eşleşti. Önceki 558 gözlem ve bağlantı korumaları aynı. C fixture 13/13, Rust rowvalue8 24/24 ve delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v392-row-case-recursive-builtin-fifo-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1273 geçti, 1 başarısız, 325 yok sayıldı. ON 1270 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,43 ms taban, 385,05 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-recursive-builtin-fifo-disk-verify/res/gates-validation.txt).
