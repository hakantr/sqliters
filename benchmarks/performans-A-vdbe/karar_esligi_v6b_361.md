# V6b.361 — WITHOUT ROWID UPDATE yabancı anahtar hazırlama tanısı

42 profilde 120 salt okunur tanı farkı kapandı. 6184 OFF korpusunda C ile eşleşti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v361-fk-oldmask-conflict-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1237 geçti, 1 başarısız, 329 yok sayıldı. ON 1234 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 396/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 546,88 ms taban, 713,90 ms aday. Süre oranı 1,305 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-oldmask-conflict-disk-verify/res/gates-validation.txt).
