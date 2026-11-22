# V6b.398 — RETURNING nitelikli ad hata ofsetleri

Önceki 72 native vaka iki kipte 72/72. Ek 198 vakada 108 OFF ve 72 ON UTF-8 bayt ofseti C ile eşleşti. 594 ana dosya ve C yeniden açılışı eş. Joker, authorizer ve önceki RTRIM akışları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v398-returning-qualified-offset-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1278 geçti, 1 başarısız, 325 yok sayıldı. ON 1276 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,57 ms taban, 382,47 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/returning-qualified-offset-disk-verify/res/gates-validation.txt).
