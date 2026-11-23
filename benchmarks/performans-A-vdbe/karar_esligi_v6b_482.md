# V6b.482 — İlk adımdan önce etkin iki işçili PMA mmap

Kip başına 156 API profili, 60 kontrollü iz ve 2.494.892 VFS olayı C ile eş. 24 gerçek iki işçi, 14/778/266, reset ve FK dosya kontrolleri geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/two-worker-initial-mmap-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v482-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1351 geçti, 0 başarısız, 325 yok sayıldı. ON 1351 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 310,29 ms taban, 392,81 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-worker-initial-pma-mmap-disk-verify/res/gates-validation.txt).
