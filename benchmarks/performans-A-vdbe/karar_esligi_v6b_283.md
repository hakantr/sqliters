# V6b.283 — timediff scalar işlevi

İki argümanlı timediff, ortak tarih ayrıştırması ve Gregorian zaman hesabıyla C süre metni üretir. Üç kodlamada 62 TEXT, 14 NULL, beş arity tanısı ve iki Tcl vakası eş. 18 C salt okunur açılışı ve DELETE/WAL dosya eşliği doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v280-date-timediff-scalar-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 311,97 ms taban, 392,79 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-timediff-scalar-disk-verify/res/gates-validation.txt).
