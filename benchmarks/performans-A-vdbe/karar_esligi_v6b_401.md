# V6b.401 — UTF-16 çok indeksli OR kursörü

420 gözlemde çok indeksli OR için 72 UTF-16 sonucu ve 108 C planı eşleşti. Önceki 558 gözlem ve fiziksel korumalar aynı. C fixture 22/22 ve dört Rust odak testi iki kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v401-row-case-reopened-or-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1281 geçti, 1 başarısız, 325 yok sayıldı. ON 1279 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 558,63 ms taban, 722,17 ms aday. Süre oranı 1,293 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-reopened-or-disk-verify/res/gates-validation.txt).
