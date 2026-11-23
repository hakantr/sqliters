# V6b.484 — İki işçide ilk satırdan sonra PMA mmap

Sıfır başlangıçta iki işçi güncel limit ile sonraki PMA aralığını açıyor. Kip başına 144 C API profili, 96 kontrollü iz ve 3.830.224 VFS olayı eş. İlk satır öncesi iz, gerçek aktif işçi, FK ve 266 fallback denetimleri geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/two-worker-late-mmap-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v484-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1353 geçti, 0 başarısız, 325 yok sayıldı. ON 1353 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,66 ms taban, 387,64 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-worker-late-pma-mmap-disk-verify/res/gates-validation.txt).
