# V6b.349 — WITHOUT ROWID görünüm UPDATE RETURNING için REAL aralık

288 profilde 360 sıra veya dosya farkı kapandı. 864 C yeniden açılışı ve dört odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v349-view-update-wr-real-range-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1225 geçti, 1 başarısız, 329 yok sayıldı. ON 1222 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,65 ms taban, 378,87 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-update-wr-real-range-disk-verify/res/gates-validation.txt).
