# V6b.406 — UTF-16 WITHOUT ROWID iki dallı OR

İki fiziksel OR dalı ve bileşik PK için ölçülen 144 UTF-16 sonuç C ile eş. Exact OFF ve ON rowvalue8 34/34, C fixture 16/16. 216 tam C planı ve önceki 558 gözlem eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v406-row-case-without-rowid-or-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1286 geçti, 1 başarısız, 325 yok sayıldı. ON 1284 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,19 ms taban, 376,15 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-without-rowid-or-disk-verify/res/gates-validation.txt).
