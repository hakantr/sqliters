# V6b.460 — V1 aggregate callback adım, reset ve finalize hataları

Pinli C ile 84 ON profili eşleşti. xStep, xInverse, xValue ve xFinal hatasının ilk veya sonraki adımda aktarılması, alttaki hata kodunun reset ve finalize için korunması, gerçek VFS BUSY ve callback SCHEMA deneme sayısı doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/window-v1-on-error-v457-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v460-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1337 geçti, 0 başarısız, 325 yok sayıldı. ON 1337 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,53 ms taban, 383,37 ms aday. Süre oranı 1,284 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-aggregate-callback-error-disk-verify/res/gates-validation.txt).
