# V6b.371 — REPLACE UPDATE hazırlama tanısı

REPLACE’in erken DELETE denetiminden sonra C’nin son tanı önceliği eşleşti. 1.134 salt okunur kontrolde 40 fark kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v371-fk-action-replace-prepare-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1248 geçti, 1 başarısız, 328 yok sayıldı. ON 1245 geçti, 4 başarısız, 328 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 306,48 ms taban, 398,33 ms aday. Süre oranı 1,300 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-action-replace-prepare-disk-verify/res/gates-validation.txt).
