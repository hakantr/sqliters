# V6b.335 — Skaler işlev içindeki MIN/MAX ve grup satırı

Skaler işlev içinde aynı SELECT kapsamında çağrılan MIN/MAX, grup satırını C ile aynı şekilde seçiyor. Üç kodlama ve iki journal kipinde 140 fark kapandı. Bağlantıya özel MAX ve ayrı alt sorgu dahil dokuz odaklı kontrol geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v335-nested-minmax-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1211 geçti, 1 başarısız, 329 yok sayıldı. ON 1208 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 296,42 ms taban, 378,39 ms aday. Süre oranı 1,277 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/nested-minmax-representative-disk-verify/res/gates-validation.txt).
