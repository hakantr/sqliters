# V6b.314 — Sabit saat metni içeren tarih BLOB girdisi

Değiştiricisiz sabit saat metni mevcut C uyumlu saat ayrıştırmasına bağlanır. Üç kodlamada bağlı ve literal yerleşimde 1.800’er tipli sonuç C ile eş. 2.340 salt okunur denetim ve 18 dosya imajı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v310-date-blob-fixed-clock-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 340,00 ms taban, 418,74 ms aday. Süre oranı 1,232 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-fixed-clock-disk-verify/res/gates-validation.txt).
