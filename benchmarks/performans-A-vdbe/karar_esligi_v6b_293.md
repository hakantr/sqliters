# V6b.293 — WITHOUT ROWID görünümünde tam indeks seçimi

Ölçülen yapısal elemede C’nin tam indeks kökü, tetikleyici sırası ve kapanmış dosya baytları eşleşti. 480 önceki fark ve 80 ek üst eşik sonucu kapandı. 8.760 C salt okunur bütünlük denetlendi. [Bağımsız kanıt](/github/sqliters/target/perf-work/v286-view-update-wr-full-with-ineligible-partial-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,62 ms taban, 382,54 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-ineligible-partial-full-disk-verify/res/gates-validation.txt).
