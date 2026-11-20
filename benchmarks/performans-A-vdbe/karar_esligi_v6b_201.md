# V6b.201 — Kapasite reddinde V1 yazım step sonucu

Dış V1 step 1. Reset ve finalize asıl 13 sonucunu koruyor. 432 C açılışı ve önceki sözdizimi tanısı eş. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v198-native-capacity-v1-candidate.lIGjTubV/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,30 ms taban, 376,77 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-write-capacity-disk-verify/res/gates-validation.txt).
