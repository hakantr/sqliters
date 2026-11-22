# V6b.383 — Toplama ve pencere sonucu metin kodlaması

VDBE toplama ve pencere metin sonuçlarını veritabanının kodlamasına getiriyor. 504 gözlemde 112 C bayt farkı kapandı ve 392 önceki eşleşme korundu. Fiziksel indeks ve DISTINCT kontrolleri eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v383-aggregate-result-encoding-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1261 geçti, 1 başarısız, 325 yok sayıldı. ON 1258 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,10 ms taban, 374,90 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/aggregate-result-encoding-disk-verify/res/gates-validation.txt).
