# V6b.294 — UTC değiştiricisinin tam yazımı

C’nin reddettiği boşluk çevreli utc değerleri artık SQL NULL. Dört saat dilimi ve üç kodlamada 432 önceki fark kapandı. 24 DELETE/WAL imajı ve 120 C salt okunur bütünlük kontrolü eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v289-date-utc-exact-lexical-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,92 ms taban, 382,44 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-utc-exact-lexical-disk-verify/res/gates-validation.txt).
