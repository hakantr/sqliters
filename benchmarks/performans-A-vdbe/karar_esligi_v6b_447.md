# V6b.447 — İki yardımcı işçiyle PMA

İki kipte 468 zorlanmış koşu ve 3.438.644 VFS kaydı eş. 54 bariyerli koşu çağıran iş parçacığının PMA üretmesini, ters hata önceliğini ve bütün işçilerin birleştirilmesini doğruladı. 18 okuyucu kilidi, 180 olağan FK ve son dosya görüntüleri eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v447-two-worker-pma-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1327 geçti, 1 başarısız, 325 yok sayıldı. ON 1325 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,59 ms taban, 385,10 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-worker-pma-disk-verify/res/gates-validation.txt).
