# V6b.313 — Julian BLOB sayısında çevre boşluğu

Mevcut SQLite ASCII boşluk denetimi sayısal BLOB yazımının başına ve sonuna uygulanır. Bağlı ve literal yerleşimde 1.296’şar C sonucu, 1.800 C açılışı ve 18 kalıcı dosya eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v308-date-blob-numeric-space-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 581,95 ms taban, 745,50 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-blob-numeric-space-disk-verify/res/gates-validation.txt).
