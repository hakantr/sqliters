# V6b.307 — İşaretli Julian tarih BLOB girdisi

Değiştiricisiz tam kod birimi BLOB’da tek ASCII işareti mevcut sayı ayrıştırmasına taşınır. Bağlı ve literal yerleşimde 1.152’şer C typed sonuç, 2.916 C açılışı ve 36 çıktı imajı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v302-date-blob-signed-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,31 ms taban, 385,18 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-signed-disk-verify/res/gates-validation.txt).
