# V6b.320 — Materyalize SELECT LIMIT ve OFFSET değerlerini C ile eşle

Materyalize SELECT ve CTE sınırları kayıpsız tam sayı dönüşümünü kullanır. Üç kodlama ve iki günlük kipinde 510 başlangıç farkı kapandı. Kip başına 540 C karşılaştırması ve 18 C salt okunur açılış eş. Beş davranışı kapsayan yeni test iki çalışma kipinde geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v320-materialized-limit-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1196 geçti, 1 başarısız, 329 yok sayıldı. ON 1193 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 561,53 ms taban, 718,40 ms aday. Süre oranı 1,279 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/materialized-limit-disk-verify/res/gates-validation.txt).
