# V6b.327 — Özyinelemeli CTE akışında LIMIT ve OFFSET

Özyinelemeli akış LIMIT ve OFFSET değerlerini birer kez değerlendirir. 160 sıralamasız fark kapandı. 56 sorgu biçimi, 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v327-recursive-cte-stream-limit-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1203 geçti, 1 başarısız, 329 yok sayıldı. ON 1200 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,66 ms taban, 391,10 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/recursive-cte-stream-limit-disk-verify/res/gates-validation.txt).
