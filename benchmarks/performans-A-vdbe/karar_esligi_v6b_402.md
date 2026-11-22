# V6b.402 — UTF-16 sabit VALUES kümesi aramaları

300 gözlemde sabit VALUES kümesi için 40 UTF-16 sonucu C ile eşleşti. Önceki 558 kayıt ve altı dosya açılışı eş. C fixture 10/10 ve Rust rowvalue8 32/32, delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v402-row-case-constant-values-set-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1282 geçti, 1 başarısız, 325 yok sayıldı. ON 1280 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 576,12 ms taban, 710,17 ms aday. Süre oranı 1,233 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-constant-values-set-disk-verify/res/gates-validation.txt).
