# V6b.306 — İşaretsiz Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB içindeki işaretsiz ASCII tamsayı ve ondalık kesir mevcut Julian dönüşümünden geçer. 1.320 doğrudan ve 1.320 satır akışı C sonucu eş. 3.636 C salt okunur açılışı ve 36 çıktı imajı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v300-date-blob-julian-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,38 ms taban, 382,59 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-julian-disk-verify/res/gates-validation.txt).
