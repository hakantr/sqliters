# V6b.264 — Görünüm UPDATE farklı sütun aralığı

Olağan görünüm UPDATE taramasında iki doğrudan sütun izdüşümü, kısmi indeksin aralık sütunu ile korunmuş plan sınırlarına göre eşlenir. Sabit C ve iki Rust kipi 252 hedef, 420 korunmuş fiziksel kapanış ve 9.414 C salt okunur açılışında eşleşti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v262-view-update-distinct-range-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,98 ms taban, 378,90 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-distinct-range-disk-verify/res/gates-validation.txt).
