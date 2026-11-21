# V6b.311 — Bileşik CAST ile CREATE TABLE AS

CTAS sayısal baş CAST için katalogda NUM ve ilk yazımda FLEXNUM kararını taşır. Çok satırlı VALUES için BLOB türü ayrı. 108 fiziksel kayıt farkı kapandı, 36 aday dosyası C ile bayt eş ve 18 native callback profili eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v305-ctas-cast-flexnum-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,17 ms taban, 384,18 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ctas-cast-flexnum-disk-verify/res/gates-validation.txt).
