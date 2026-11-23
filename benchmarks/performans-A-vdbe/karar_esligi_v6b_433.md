# V6b.433 — Anonim Linux PMA dosyasında isteğe bağlı eşleme

Seri FILE sıralayıcıda test denetimi ile C eşleme ve kapatma davranışı doğrulandı. Aynı özel tanıtıcının G/Ç ve eşleme erişimi aynı kilitle sıralanır. Varsayılan kip ve ana dosya mmap PRAGMA davranışı korunuyor. [Bağımsız kanıt](/github/sqliters/target/perf-work/v433-sorter-mmap-serialized-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1312 geçti, 1 başarısız, 325 yok sayıldı. ON 1310 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,55 ms taban, 378,70 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/sorter-mmap-test-control-disk-verify/res/gates-validation.txt).
