# V6b.326 — Sıralı özyinelemeli CTE LIMIT değerlendirme sırası

VALUES başlangıçlı ve tek özyinelemeli kollu sıralı CTE'lerde dış LIMIT ve OFFSET satır üretiminden önce değerlendirilir. 200 fark kapandı. 18 C salt okunur açılış ve beş odaklı Rust kontrolü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v326-ordered-recursive-cte-limit-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1202 geçti, 1 başarısız, 329 yok sayıldı. ON 1199 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,12 ms taban, 380,45 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ordered-recursive-cte-limit-disk-verify/res/gates-validation.txt).
