# V6b.387 — UTF-16 CASE gruplamasında yerleşik kolasyonlar

Aynı gruplama anahtarını paylaşan sıralayıcı ve karşılaştırma adımlarında NOCASE ve RTRIM C kodlamasıyla yürütülüyor. 558 gözlemde 24 fark kapandı. Uygulamanın kayıtlı karşılaştırıcıları ve yetkilendirme kontrolleri korunuyor. [Bağımsız kanıt](/github/sqliters/target/perf-work/v387-row-case-builtin-group-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1265 geçti, 1 başarısız, 325 yok sayıldı. ON 1262 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,97 ms taban, 383,15 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-builtin-group-disk-verify/res/gates-validation.txt).
