# V6b.339 — ANALYZE sonrasında WITHOUT ROWID kapsayan tarama

Normal ANALYZE istatistikleri filtresiz WITHOUT ROWID taramasında C gibi kullanılıyor. 96 profilde 790 fark kapandı ve 288 C yeniden açılışı doğrulandı. Altı odaklı C ve Rust kontrolü geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v339-wr-covering-analyze-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1215 geçti, 1 başarısız, 329 yok sayıldı. ON 1212 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 392/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 304,70 ms taban, 386,10 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wr-covering-analyze-disk-verify/res/gates-validation.txt).
