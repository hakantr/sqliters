# V6b.336 — Tekrarlanan sütun MIN/MAX işleminde grup satırı

Filtresiz tek sütun MIN/MAX tekrarları C gibi ilk akümülatör konumunu koruyor. Üç kodlama ve iki journal kipinde 90 fark kapandı. Ayrı DISTINCT, FILTER ve farklı literal yazımları dahil sekiz odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v336-repeated-minmax-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1212 geçti, 1 başarısız, 329 yok sayıldı. ON 1209 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,32 ms taban, 376,65 ms aday. Süre oranı 1,271 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/repeated-minmax-representative-disk-verify/res/gates-validation.txt).
