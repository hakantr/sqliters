# V6b.457 — Fonksiyon değişikliğinde V1 ilk adım ve reset tanısı

Hazır SELECT fonksiyon değişikliğiyle süre dışı kalınca V1 adımı önceki bağlantı tanısıyla SQLITE_ERROR, reset ve finalize SQLITE_SCHEMA döndürüyor. V2 yeniden hazırlama ve canlı ifade sırasındaki SQLITE_BUSY korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v457-window-function-expiry-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v457-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1335 geçti, 0 başarısız, 325 yok sayıldı. ON 1335 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,49 ms taban, 379,43 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-legacy-function-expiry-disk-verify/res/gates-validation.txt).
