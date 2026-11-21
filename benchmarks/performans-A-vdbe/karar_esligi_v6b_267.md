# V6b.267 — Point UPDATE ve DELETE plan testi

Mevcut test doğrudan rowid planı ile eski Rust VDBE’yi iki workspace ortamında aynı tam gövdeyle karşılaştırır. Test iş parçacığı geçici kipi güvenli geri yükler. Odaklı ve paralel ON/OFF testleri, 72 normal C/Rust DML tekrarı ve dosya baytları doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v267-point-plan-test-routing-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1182 geçti, 5 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,27 ms taban, 375,49 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/point-plan-test-routing-disk-verify/res/gates-validation.txt).
