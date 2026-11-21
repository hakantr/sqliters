# V6b.312 — Julian BLOB ondalık nokta yazımı

Değiştiricisiz BLOB sayı yazımında ondalık noktanın en az bir yanında rakam olması yeterli. Bağlı ve literal yerleşimde 1.368’er C sonucu, 2.400 C açılışı ve 18 kalıcı imaj eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v307-date-blob-decimal-dot-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 560,41 ms taban, 731,65 ms aday. Süre oranı 1,306 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-decimal-dot-disk-verify/res/gates-validation.txt).
