# V6b.470 — Hazır V1 yazma kısıtlarında adım ve reset tanısı

Yedi DML kısıtı, üç kodlama, V1/V2 ve RETURNING ile iki kipte 168 C profili eşleşti. İlk genel kod, reset ve finalize için özgün kod ve mesaj, OR FAIL geçerli satırları ve FK denetimi doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v470-prepared-v1-constraint-diagnostic-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v470-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1343 geçti, 0 başarısız, 325 yok sayıldı. ON 1343 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 597,37 ms taban, 747,43 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-constraint-diagnostic-disk-verify/res/gates-validation.txt).
