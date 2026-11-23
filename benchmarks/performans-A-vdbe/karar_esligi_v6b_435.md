# V6b.435 — Tek işçiyle PMA ve kapanış

Mevcut VDBE ON girişinde UTF-8 ana rowid tablosu ve tam BINARY sıralama için bir yardımcı görev çalışır. Çağıran iş parçacığındaki 32 koşuda 63.158 VFS kaydı, 14 gerçek işçi koşusunda satır ve step/reset sonuçları C ile eş. Rust 80 işçi başlattı ve hepsini birleştirdi. PMA testleri 4/4, önceki iki sorter testi her kipte geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v435-one-worker-pma-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1314 geçti, 1 başarısız, 325 yok sayıldı. ON 1312 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 417,17 ms taban, 565,12 ms aday. Süre oranı 1,355 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/single-worker-pma-disk-verify/res/gates-validation.txt).
