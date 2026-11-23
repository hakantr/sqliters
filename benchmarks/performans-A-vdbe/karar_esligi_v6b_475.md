# V6b.475 — Adlandırılmış işçi PMA dosyasında geç eşleme

Kip başına 12 sonuç/reset profili, 50.642 VFS kaydı ve altı kopya EIO geri dönüşü doğrulandı. Sağlam dosyadan tamponlu okuma sonuçları koruyor. Fiziksel açma, unlink, mmap, munmap ve kapanış sayıları eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-named-late-mmap-recovery-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v475-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1345 geçti, 0 başarısız, 325 yok sayıldı. ON 1345 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,71 ms taban, 385,86 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-worker-late-mmap-disk-verify/res/gates-validation.txt).
