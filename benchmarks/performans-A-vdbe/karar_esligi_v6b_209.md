# V6b.209 — foreign_keys setter sonrası legacy V1 erken expiry

Gerçek setter adımından sonra erken V1 step 1, reset ve finalize 17 döndürür. Önceki tanı ve byte ofseti korunur. 804 profil ve 2.412 C açılışı eş. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v209-foreign-keys-legacy-early-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,74 ms taban, 382,78 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-foreign-keys-expiry-disk-verify/res/gates-validation.txt).
