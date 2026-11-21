# V6b.310 — Üslü Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB içindeki üslü ASCII sayı mevcut iJD dönüşümünü kullanır. Bağlı ve literal yerleşimde 1.440’ar C sonucu eş, 3.376 salt okunur C açılışı ve 36 çıktı imajı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v305-date-blob-exponent-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 305,97 ms taban, 390,92 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-exponent-disk-verify/res/gates-validation.txt).
