# V6b.476 — DROP TABLE içinde ertelenmiş FK borcunun sahibini koruma

196 profil iki kipte C ile eş. Önceki 24 ON hatalı reddi giderildi. Başka tablo ve şema örneğinin borcu 787 kaldı. ROLLBACK TO, ardından tekrar DROP ve 588 temiz C yeniden açılışı doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v469-fk-drop-counter-provenance-experiment/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v476-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1346 geçti, 0 başarısız, 325 yok sayıldı. ON 1346 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,04 ms taban, 384,27 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/deferred-fk-drop-ownership-disk-verify/res/gates-validation.txt).
