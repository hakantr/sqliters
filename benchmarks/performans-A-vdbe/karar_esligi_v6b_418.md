# V6b.418 — IN listeli WITHOUT ROWID OR

V6b.417 tabanındaki 21 hedef biçimde 336 UTF-16 farkı kapandı. 504 tam EXPLAIN planı, 24 deyim yaşam döngüsü kaydı ve 144 API davranışı kaydı doğrulandı. Birleşik ikilinin 528 eski kontrol planı C ve V6b.417 ile eşleşti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v416-without-rowid-or-literal-in-independent-c-proof/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1298 geçti, 1 başarısız, 325 yok sayıldı. ON 1296 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,15 ms taban, 376,06 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-literal-in-or-disk-verify/res/gates-validation.txt).
