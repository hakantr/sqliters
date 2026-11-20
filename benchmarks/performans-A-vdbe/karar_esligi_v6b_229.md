# V6b.229 — Açık julianday girdisi

İlk explicit julianday ham sayısal Julian aralığını C ile eşliyor. 1.035 ana girdide 405 fark kapandı. 684 C açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v228-explicit-julianday-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,16 ms taban, 380,99 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/explicit-julianday-input-disk-verify/res/gates-validation.txt).
