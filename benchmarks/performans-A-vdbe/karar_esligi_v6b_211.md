# V6b.211 — recursive_triggers setter sonrası V1 erken expiry

Fiilen çalışmış recursive_triggers setter adımı sonrası V1 erken step ve terminal C ile eş. 1.104 fark kapandı. 4.644 C açılışı doğrulandı. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v211-recursive-triggers-legacy-early-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,10 ms taban, 386,16 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-recursive-triggers-expiry-disk-verify/res/gates-validation.txt).
