# V6b.255 — Süre biriminde tek çoğul ek

Ortak süre değiştiricisi C gibi en fazla bir çoğul s ekini kabul eder. 2.925 fark kapandı ve 4.347 geçerli sonuç C ile eş kaldı. Numeric TEXT, negatif 24:00 ve 60 bağımsız C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v253-duration-unit-single-plural-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,12 ms taban, 378,76 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/duration-unit-single-plural-disk-verify/res/gates-validation.txt).
