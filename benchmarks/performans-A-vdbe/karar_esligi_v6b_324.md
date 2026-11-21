# V6b.324 — Özyinelemesiz CTE LIMIT değerlendirme sırası

Özyinelemesiz CTE için LIMIT ve OFFSET satır üretiminden önce birer kez değerlendirilir. 330 fark kapandı. 62 sorgu biçimi, 18 C salt okunur açılış ve altı Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v324-cte-limit-order-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1200 geçti, 1 başarısız, 329 yok sayıldı. ON 1197 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 544,19 ms taban, 705,69 ms aday. Süre oranı 1,297 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/cte-limit-order-disk-verify/res/gates-validation.txt).
