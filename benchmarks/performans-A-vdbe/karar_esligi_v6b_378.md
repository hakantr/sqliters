# V6b.378 — UTF-16 BINARY satır CASE tekilleştirmesi

Sırasız geçici BINARY anahtar kullanan salt okunur CASE sorgularında 720 gözlem ve 80 kapanan fark doğrulandı. Sonradan eklenmiş TEXT DEFAULT değerlerini okuyan eski fiziksel satırlar için 24 ek C eşleşmesi ve 12 C açılışı geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v378-row-case-binary-distinct-revised-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1256 geçti, 1 başarısız, 325 yok sayıldı. ON 1253 geçti, 4 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,33 ms taban, 377,02 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/row-case-binary-distinct-disk-verify/res/gates-validation.txt).
