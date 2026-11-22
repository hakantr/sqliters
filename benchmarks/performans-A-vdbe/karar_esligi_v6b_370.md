# V6b.370 — Varsayılan, ABORT ve ROLLBACK UPDATE hazırlama tanısı

Yabancı anahtar eylem önceliği üç ek çatışma kipinde C ile eşleşti. 882 salt okunur kontroldeki 120 tanı farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v370-fk-action-default-conflict-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1247 geçti, 1 başarısız, 328 yok sayıldı. ON 1244 geçti, 4 başarısız, 328 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,84 ms taban, 385,53 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-action-default-conflict-disk-verify/res/gates-validation.txt).
