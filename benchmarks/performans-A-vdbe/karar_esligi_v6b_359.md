# V6b.359 — Rowid görünüm UPDATE RETURNING sırasında birleşik metin ve tamsayı aralığı

144 profilde 60 sıra veya dosya farkı kapandı. 432 C yeniden açılışı ve dört odaklı kontrol geçti. 6392 OFF korpusunda C ile eşleşti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v359-rowid-view-mixed-range-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1235 geçti, 1 başarısız, 329 yok sayıldı. ON 1232 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 394/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 561,06 ms taban, 700,40 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-view-mixed-range-disk-verify/res/gates-validation.txt).
