# V6b.367 — Rowid görünüm DELETE karma aralığında tetikleyici sırası

216 görünüm silme profilinde 300 fark kapandı. 648 C yeniden açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v367-rowid-view-delete-mixed-range-corrected-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1243 geçti, 1 başarısız, 329 yok sayıldı. ON 1240 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,51 ms taban, 381,73 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-view-delete-mixed-range-disk-verify/res/gates-validation.txt).
