# V6b.455 — Bir anonim işçide ilk satırdan sonra mmap

Kip başına 48 zorlanmış çalıştırmada 714.154 VFS kaydı ve 484 lease eşleşti. Altı gerçek işçi akışı, reset ve kapanış, 132 varsayılan kontrol ve FK açık 180 dosya sonucu doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/pma-worker-mmap-v451-audit/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v455-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1333 geçti, 1 başarısız, 325 yok sayıldı. ON 1333 geçti, 1 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,83 ms taban, 384,45 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/anonymous-worker-pma-late-mmap-disk-verify/res/gates-validation.txt).
