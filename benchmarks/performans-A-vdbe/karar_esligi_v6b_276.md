# V6b.276 — Tek işaretli takvim kaydırması

Geçerli ISO başlangıçta dört haneli tek işaretli takvim değiştiricisi yıl ve ayı önce, gün miktarını son uygular. Ortak ISO Julian denetimi son değeri doğrular. 746 hedef, 2.590 ham koruma ve 30 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v275-date-single-packed-calendar-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,63 ms taban, 385,93 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-single-packed-calendar-disk-verify/res/gates-validation.txt).
