# V6b.290 — İşaretli hafta günü kaydırması

Geçerli ISO tarih metninde tek weekday sayısındaki gerçek eksi, C'nin geriye kaydırma yönünü kullanır. 13 Tcl vakası ve 932 önceki fark kapandı. 1.680 ileri yön kontrolü ve 30 salt okunur C açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v286-date-signed-weekday-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,40 ms taban, 382,25 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-signed-weekday-disk-verify/res/gates-validation.txt).
