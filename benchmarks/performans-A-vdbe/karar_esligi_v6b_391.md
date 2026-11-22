# V6b.391 — UTF-16 yerleşik satır CASE üyeliği

510 gözlemde yerleşik NOCASE ve RTRIM kullanan skaler ve satır IN/NOT IN için 80 fark kapandı. Önceki 558 gözlem ve gerekçeleri aynı. C fixture 13/13 ve dört odak Rust testi iki kipte geçti. Bağlantıya kayıtlı kolasyon ve authorizer kontrolü korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v391-row-case-builtin-in-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1272 geçti, 1 başarısız, 325 yok sayıldı. ON 1269 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 340,82 ms taban, 425,76 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-builtin-in-disk-verify/res/gates-validation.txt).
