# V6b.364 — Rowid görünüm UPDATE karma aralığında tek indeks

Tam INTEGER ve uygun kısmi TEXT indeksinin ayrı kullanıldığı 240 profilde 100 fark kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v364-rowid-view-single-index-mixed-range-corrected-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1240 geçti, 1 başarısız, 329 yok sayıldı. ON 1237 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,36 ms taban, 378,36 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-view-single-index-mixed-range-disk-verify/res/gates-validation.txt).
