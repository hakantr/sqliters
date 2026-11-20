# V6b.204 — Eksik SELECT sütunu byte ofseti

İki çözümleyici extended kod ve tanı metninde eşleştiğinde eksik sütun ofseti ekleniyor. 384 OFF ofseti ve 216 V1 tanı profili eş. [Kanıt](/github/sqliters/target/perf-work/v202-select-missing-column-offset-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,84 ms taban, 379,75 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/select-missing-column-offset-disk-verify/res/gates-validation.txt).
