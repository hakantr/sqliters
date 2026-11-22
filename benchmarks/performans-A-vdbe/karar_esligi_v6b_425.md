# V6b.425 — WITHOUT ROWID OR için ek IN ifade biçimleri

Aritmetik filtre, rowid üst sınırı ve LENGTH üreticisi kapsamındaki 128 UTF-16 farkı kapandı. 192 tam C planı ve iki Rust kipinde 43 odak testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v425-without-rowid-or-in-expressions-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1305 geçti, 1 başarısız, 325 yok sayıldı. ON 1303 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,92 ms taban, 380,16 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-in-expressions-disk-verify/res/gates-validation.txt).
