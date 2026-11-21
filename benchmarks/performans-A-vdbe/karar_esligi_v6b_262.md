# V6b.262 — Görünüm UPDATE aynı yönlü iki sınır

Açık MAIN yazma işleminde aynı NUMERIC sütunun aynı yönlü iki koşulundan biri kısmi indeksle yapısal eşse mevcut C uyumlu ilk sınır kullanılır. 216 hedef C ile bayt eş ve 280 önceki sıra/dosya farkı kapandı. 12 ilk yaprak kontrolü ve 4.950 bağımsız C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v260-view-update-same-bound-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 473,38 ms taban, 720,35 ms aday. Süre oranı 1,522 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-same-bound-disk-verify/res/gates-validation.txt).
