# V6b.301 — Doğal SELECT akışında adım saati

Exact birleşik tabanda ölçülen doğal legacy projeksiyonlar zamanı ilk ihtiyaçta ve aynı adım boyunca tek değer olarak alır. Beş sorgu biçiminde 100 C eş sonuç, 209 C salt okunur açılış ve 840 native matris kaydı yeniden doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v301-step-clock-reconciled-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,22 ms taban, 382,42 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-step-clock-streaming-disk-verify/res/gates-validation.txt).
