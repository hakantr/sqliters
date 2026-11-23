# V6b.486 — Üç ila sekiz işçide ilk satır sonrası PMA mmap

Kip başına 536 C API profili, 17.970.434 kontrollü VFS olayı ve 968.632 korunan önceki tamponlu işlem eş. 72 doğal işçi, 12 sekiz işçili doluluk, 18 sağlık ve Group64 31/31 geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/multi-worker-late-mmap-exact-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v486-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1354 geçti, 0 başarısız, 325 yok sayıldı. ON 1354 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,14 ms taban, 383,20 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-worker-late-pma-mmap-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
