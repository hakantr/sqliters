# V6b.384 — Skaler WHEN ile satır CASE hazırlama tanısı

Çok sütunlu alt sorgu ile skaler WHEN karşılaştırmasında C hazırlama tanısı korunuyor. Ana 558 gözlemde 10 ve ek 144 gözlemde 30 fark kapandı. Skaler sonuçlar, tanımsız WHEN adı ve 24 API koruma gözlemi aynı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v384-row-case-scalar-when-diagnostic-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1262 geçti, 1 başarısız, 325 yok sayıldı. ON 1259 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,09 ms taban, 377,35 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-scalar-when-diagnostic-disk-verify/res/gates-validation.txt).
