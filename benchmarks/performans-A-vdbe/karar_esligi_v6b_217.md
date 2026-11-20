# V6b.217 — Bağlı sabit LIMIT okuyucusu

İlk bağlı dosyanın basit rowid taramasında sabit LIMIT sayacı korunuyor. 42 OFF farkı kapandı. 4.650 C yeniden açılışı ve önceki DDL koruması geçti. [Bağımsız sonuç](/github/sqliters/target/perf-work/v216-aux-limit-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,55 ms taban, 376,42 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/attached-rollback-limit-reader-disk-verify/res/gates-validation.txt).
