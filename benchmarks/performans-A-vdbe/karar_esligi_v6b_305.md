# V6b.305 — VDBE saf tarih çağrısı koruması

OP_PureFunc rolü ve P5 korunur. C’nin reddettiği beş yerleşik zaman çağrısının ölçülen biçimleri değerlendirme öncesi durur. İki rol birim kanıtı, 592 normal ham karşılaştırma ve 209 C açılışı geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v299-temporal-pure-machine-guard-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1188 geçti, 1 başarısız, 335 yok sayıldı. ON 1185 geçti, 4 başarısız, 335 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,23 ms taban, 382,02 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temporal-pure-machine-guard-disk-verify/res/gates-validation.txt).
