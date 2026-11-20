# V6b.202 — DDL geri alımında V1 okuyucu step sonucu

Dış step 1. Canlı okuyucunun 516 ve stale deyimin 17 terminal kodu korunuyor. 1.728 C açılışı ölçüldü. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v200-rollback-cursor-v1-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 308,97 ms taban, 385,87 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-read-rollback-disk-verify/res/gates-validation.txt).
