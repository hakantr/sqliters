# V6b.230 — JULIANDAY REAL çıkışı

Geçerli Julian milisaniyesi C gibi tam sayı toplamından sonra tek bölmeyle REAL oluyor. 171 ayrı bit farkı kapandı. 851 C açılışı ve 12 VDBE testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v230-julianday-real-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 310,33 ms taban, 395,36 ms aday. Süre oranı 1,274 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/julianday-real-value-disk-verify/res/gates-validation.txt).
