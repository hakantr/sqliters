# V6b.459 — Pencere bölmesi geçişinde xFinal

UTF-8 makine yolunda bölme sonlandırması, ilk satırlar, kısmi reset ve finalize 12 C profili ve 136 olayda eşleşti. İki eski fark kapandı. OFF ve UTF-16 ON yollarında 60 önceki kayıt korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v459-window-partition-cleanup-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v459-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1336 geçti, 0 başarısız, 325 yok sayıldı. ON 1336 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,78 ms taban, 376,81 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/window-partition-accumulator-release-disk-verify/res/gates-validation.txt).
