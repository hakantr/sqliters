# V6b.269 — Görünüm UPDATE tek bağlı eşitlik

Açık MAIN yazmasında dar V2 görünüm UPDATE yeniden hazırlaması, ilgili INTEGER veya REAL bağlı değeri kısmi indeks uygunluğuna taşır. İlk bağda 8 ve yeniden bağlamada 12 profil C kökü ve dosya baytı eş. 1.440 C salt okunur açılışı ve 376 OFF koruması doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v268-view-update-bound-equality-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 335,30 ms taban, 441,86 ms aday. Süre oranı 1,318 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-bound-equality-disk-verify/res/gates-validation.txt).
