# V6b.385 — Özyinelemeli CTE içinde görünüm CASE

Özyinelemeli CTE tanımı kendi açık başvurusunu tekrar izlemeksizin başlangıç ve özyineleme kollarındaki görünümleri çözüyor. Ölçülen 40 farkın 32’si kapandı. Diğer sonuçlar ve altı C yeniden açılışı eş kaldı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v385-recursive-view-row-case-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1263 geçti, 1 başarısız, 325 yok sayıldı. ON 1260 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 308,24 ms taban, 387,82 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/recursive-view-row-case-disk-verify/res/gates-validation.txt).
