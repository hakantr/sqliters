# V6b.438 — Adlandırılmış sorter mmap

Kararlı filtresiz Linux üzerinde adlandırılıp silinen sorter dosyasının eşlemesi aynı süreçten güvenli çekirdek kopyasıyla okunur. C ile 84 VFS izi, 48 önceki VFS izi ve 144 işçi kontrolü eş. Ek pread hatası kapandı. Sınır, mmap ve dosya kapanışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v438-named-sorter-kernel-copy-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1317 geçti, 1 başarısız, 325 yok sayıldı. ON 1315 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,89 ms taban, 381,75 ms aday. Süre oranı 1,282 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-sorter-kernel-copy-disk-verify/res/gates-validation.txt).
