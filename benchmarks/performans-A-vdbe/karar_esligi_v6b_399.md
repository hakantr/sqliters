# V6b.399 — UTF-16 hesaplanan rowid listeleri

390 gözlemde aritmetik ve yerleşik kolasyonlu rowid listesi için 64 UTF-16 farkı kapandı. Önceki 558 gözlem aynı. C fixture 10/10, Rust rowvalue8 29/29 ve delete2 1/1 iki kipte. [Bağımsız kanıt](/github/sqliters/target/perf-work/v399-row-case-computed-rowid-in-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1279 geçti, 1 başarısız, 325 yok sayıldı. ON 1277 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,26 ms taban, 385,92 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-computed-rowid-disk-verify/res/gates-validation.txt).
