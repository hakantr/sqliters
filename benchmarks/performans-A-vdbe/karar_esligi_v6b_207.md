# V6b.207 — TEMP CREATE eski cookie ile V1 step

Eski TEMP cookie ilk yazma denetiminde saptandığında V1 step 1 ve terminal 17 döner. 2.612 C açılışı doğrulandı. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v207-temp-create-v1-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,04 ms taban, 381,31 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-create-legacy-cookie-disk-verify/res/gates-validation.txt).
