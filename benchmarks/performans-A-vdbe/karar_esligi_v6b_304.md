# V6b.304 — Sabit ISO tarih BLOB girdisi

Değiştiricisiz kanonik BLOB tarihleri mevcut ISO ayrıştırıcısıyla işlenir. 2.448 C typed sonuç, 3.036 C açılışı ve 36 çıktı imajı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v298-date-blob-fixed-iso-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,15 ms taban, 384,14 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-fixed-iso-disk-verify/res/gates-validation.txt).
