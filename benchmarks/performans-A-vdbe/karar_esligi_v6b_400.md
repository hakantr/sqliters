# V6b.400 — UTF-16 satır CASE tamsayı kümesi aramaları

390 gözlemde sabit SELECT kümesi ve ikincil indeks aramaları için 56 sonuç C ile eşleşti. Önceki 558 gözlem aynı. C fixture 13/13, Rust rowvalue8 30/30 ve delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v400-row-case-integer-set-probe-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1280 geçti, 1 başarısız, 325 yok sayıldı. ON 1278 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 563,60 ms taban, 719,81 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-integer-set-probe-disk-verify/res/gates-validation.txt).
