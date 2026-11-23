# V6b.446 — Hazırlanmış aggregate ilk adımı ve reset

Yeni uygulama aggregate aritesi eski hazırlanmış seçimi değiştirmez. Taze sorgu yeni tanımı kullanır. 12 C akışı ve 132 kayıt eş. 288 kapsam dışı kontrol ve her kipte odak testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v446-prepared-aggregate-generation-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1326 geçti, 1 başarısız, 325 yok sayıldı. ON 1324 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 305,34 ms taban, 383,80 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-aggregate-generation-disk-verify/res/gates-validation.txt).
