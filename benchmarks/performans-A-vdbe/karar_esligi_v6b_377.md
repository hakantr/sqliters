# V6b.377 — UTF-16 BINARY satır CASE gruplaması

Salt okunur satır CASE sorgularında BINARY grup karşılaştırmaları kendi UTF-16 kodlamasında yürütülüyor. 12 profilde 432 gözlem ve 80 kapanan fark doğrulandı. Diğer sonuçlar ve dosya özetleri aynı. İki Rust kipi ve C odak testleri geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v377-row-case-binary-group-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1255 geçti, 1 başarısız, 325 yok sayıldı. ON 1252 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 306,33 ms taban, 384,00 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-binary-group-disk-verify/res/gates-validation.txt).
