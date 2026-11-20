# V6b.208 — TEMP journal OFF savepoint

Açık journal handle olmayan özel TEMP OFF pager savepoint playback yapmıyor. 2.070 C açılışı ve ana dosya sonuçları eş. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v205-temp-journal-savepoint-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,87 ms taban, 382,23 ms aday. Süre oranı 1,262 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-off-savepoint-disk-verify/res/gates-validation.txt).
