# V6b.485 — Üç ila sekiz işçide başlangıç PMA mmap

Kip başına 604 C API profili ve 11.067.994 kontrollü VFS olayı eş. Birleşik kaynakta WITHOUT ROWID PK ve BINARY UNIQUE/NOT NULL kontrolleri kip başına 90.034 VFS olayı ile eş. Grup64 OFF/ON 30/30. [Bağımsız ölçüm](/github/sqliters/target/perf-work/multi-worker-initial-mmap-exact-verify/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v485-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1353 geçti, 0 başarısız, 325 yok sayıldı. ON 1353 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,82 ms taban, 378,32 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/multi-worker-initial-pma-mmap-disk-verify/res/gates-validation.txt).
