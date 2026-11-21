# V6b.275 — Görünüm UPDATE iki bağlı koşul

İki kompakt yuvada, açık MAIN V2 görünüm UPDATE yeniden hazırlaması gerçek equality yuvasını C ile aynı kısmi indeks uygunluğuna taşır. 412 STEP kökü ve 208 son dosya C eş. 4.384 C salt okunur açılışı ve eski tek bağ/tamsayı korumaları doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v274-view-update-two-bound-equality-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 338,29 ms taban, 429,62 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-two-bound-equality-disk-verify/res/gates-validation.txt).
