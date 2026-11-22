# V6b.422 — WITHOUT ROWID OR için filtreli IN üreticileri

Tek karşılaştırmalı filtre, indeks aralığı ve tek yerleşik ABS üreticisi 112 farkı kapattı. 168 tam C planı ve 84 Rust odak testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v422-without-rowid-or-filtered-in-producers-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1302 geçti, 1 başarısız, 325 yok sayıldı. ON 1300 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 298,18 ms taban, 381,39 ms aday. Süre oranı 1,279 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-filtered-in-producers-disk-verify/res/gates-validation.txt).
