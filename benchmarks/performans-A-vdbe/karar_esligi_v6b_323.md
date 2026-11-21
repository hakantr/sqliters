# V6b.323 — FROM alt sorgularında LIMIT değerlendirme sırası

Olağan FROM alt sorgularında LIMIT ve OFFSET satır üretiminden önce değerlendirilir. 360 fark kapandı. 93 sorgu, 18 C salt okunur açılış ve beş odaklı Rust karşılaştırması doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v323-subquery-limit-order-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1199 geçti, 1 başarısız, 329 yok sayıldı. ON 1196 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 550,65 ms taban, 719,37 ms aday. Süre oranı 1,306 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/subquery-limit-order-disk-verify/res/gates-validation.txt).
