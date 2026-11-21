# V6b.309 — Yerleşik likelihood hazırlama tanısı

Normal SELECT için mevcut C tipi hazırlama sonucundaki geçersiz likelihood olasılığı tanısının kodu, özgün metni ve bayt ofseti taşınır. Yerleşik 126 ve yerel scalar 24 native vaka iki kipte eş. 720 C salt okunur açılışı geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v303-likelihood-prepare-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,83 ms taban, 376,09 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/likelihood-prepare-disk-verify/res/gates-validation.txt).
