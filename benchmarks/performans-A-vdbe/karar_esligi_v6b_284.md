# V6b.284 — Saatli işaretli tarih değiştiricisi

Tek normal signed tarih ve saat değiştiricisi C'nin yıl, ay, gün ve saat sırasını uygular. 62 geçerli timediff çiftinin iç içe tarih, Julian ve milisaniye çıktısı eş. İki Tcl vakası ve 18 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v281-date-packed-hms-modifier-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,08 ms taban, 381,00 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-packed-hms-modifier-disk-verify/res/gates-validation.txt).
