# V6b.421 — Adım adım PMA birleştirmesi

PMA okuyucu ağacı ve 16 üstündeki ara gruplar C ile aynı adım düzeninde ilerliyor. 36 önceki PMA farkı kapandı. Bellek ve LIMIT kontrolleriyle 60 doğrulama, 8.988 adım ve 68.068 karşılaştırma kaydı eş. Her kipte odak testi geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v421-pma-merge-schedule-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1301 geçti, 1 başarısız, 325 yok sayıldı. ON 1299 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,47 ms taban, 379,83 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/pma-merge-reader-schedule-disk-verify/res/gates-validation.txt).
