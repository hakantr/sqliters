# V6b.372 — UPDATE test paketinin etkinleştirilmesi

C ve iki Rust kipinde 141 UPDATE kontrolü geçti. Pencere sorgusunun planı ve 72 C yeniden açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v372-enable-update-suite-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1249 geçti, 1 başarısız, 327 yok sayıldı. ON 1246 geçti, 4 başarısız, 327 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,24 ms taban, 376,27 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/enable-update-suite-disk-verify/res/gates-validation.txt).
