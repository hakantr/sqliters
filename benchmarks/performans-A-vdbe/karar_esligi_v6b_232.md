# V6b.232 — Normal rollback küçültme sırası

Normal D/P/T disk rollback büyümüş fiziksel dosyayı preimage yazısından önce özgün boyuta getiriyor. 36 sıra farkı kapandı. 324 C açılışı ve 216 fiziksel karşılaştırma doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v231-normal-rollback-order-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,94 ms taban, 380,60 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/normal-rollback-truncate-order-disk-verify/res/gates-validation.txt).
