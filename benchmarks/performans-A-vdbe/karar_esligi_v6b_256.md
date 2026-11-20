# V6b.256 — Ortak ISO tarih sözdizimi

Ortak tarih ayrıştırıcı C’nin ASCII yıl, ay ve gün alanlarını, büyük T ve ASCII boşluk ayırıcısını ve saatli Z kuralını kullanır. 92 girdi üç kodlamada C ile hizalandı, 530 eş sonuç ve önceki numeric, kesir ve korunmuş takvim kontrolleri korundu. Üç dosya profili ile 60 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v256-iso-date-lexical-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,48 ms taban, 382,03 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-date-lexical-disk-verify/res/gates-validation.txt).
