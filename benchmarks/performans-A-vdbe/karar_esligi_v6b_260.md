# V6b.260 — Görünüm UPDATE karşıt iki sınır ve kısmi indeks

Açık MAIN yazma işleminde aynı NUMERIC BINARY sütununda bir alt ve bir üst sınırdan biri kısmi indeks koşuluyla yapısal eş olduğunda C’nin planı ve kalıcı kayıt sırası izlenir. 90 OFF ve 60 UTF-16 ilişkisel ON farkı kapandı. 888 önceki koruma ve 4.554 bağımsız C salt okunur açılışı geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v258-view-update-two-bound-partial-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 554,16 ms taban, 700,93 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-two-bound-partial-disk-verify/res/gates-validation.txt).
