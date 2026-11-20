# V6b.257 — Ortak tarih değiştiricisi sözdizimi

Tarih değiştiricileri C’nin ASCII ayırıcı ve baştaki sayısal karakter kararlarını kullanır. 212 farklı girdi düzeldi ve önceki 459 eş sonuç korundu. ISO, sayısal TEXT, negatif 24:00, üç kalıcı dosya ve 60 bağımsız C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v257-date-modifier-lexical-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 303,19 ms taban, 378,37 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-modifier-lexical-disk-verify/res/gates-validation.txt).
