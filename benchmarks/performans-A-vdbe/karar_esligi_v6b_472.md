# V6b.472 — VACUUM sonrası belirsiz kilidin güvenli doğrulanması

Başarılı EXCLUSIVE öncesi journal, WAL ve ana dosya erişimi yapılmıyor. 16 hedef C kilit ve API gözlemi, gerçek peer çekişmesi, C readback, 156 dosya görüntüsü ve Y1 doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v470-vacuum-unknown-lock-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v472-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1344 geçti, 0 başarısız, 325 yok sayıldı. ON 1344 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,80 ms taban, 385,60 ms aday. Süre oranı 1,282 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-pending-lock-disk-verify/res/gates-validation.txt).
