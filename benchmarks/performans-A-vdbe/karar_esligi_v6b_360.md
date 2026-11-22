# V6b.360 — WITHOUT ROWID UPDATE OR ABORT yabancı anahtar hazırlama tanısı

42 profilde 30 salt okunur tanı farkı kapandı. 6088 OFF korpusunda C ile eşleşti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v360-fk-oldmask-update-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1236 geçti, 1 başarısız, 329 yok sayıldı. ON 1233 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 395/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,38 ms taban, 380,63 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-oldmask-update-disk-verify/res/gates-validation.txt).
