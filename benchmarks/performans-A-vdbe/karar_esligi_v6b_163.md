# V6b.163 — Karma indeksli açık WHERE 1 DELETE

Açık WHERE 1, uygun tek karma indeksli MAIN rowid tablo için mevcut satır bazlı fiziksel karşılaştırıcı ve hazırlanmış ileri/ters tarama kararını kullanır. Bu uygunluk, authorizer kullanılmayan açık SQL hazırlamasına aittir. [Bağımsız C ve Rust kaydı](/github/sqliters/target/perf-work/v160-mixed-index-delete-clear.UzNSTeO9/sonuc.md)

342 yazıcı, 342 rollback ve 1.026 salt okunur yeniden açılış. 180 C/Rust ve 36 eski OFF/ON dosya ve WAL karşılaştırması eş. Küçük, taşmalı ve çok sayfalı DB ile iki boş kök ve freelist denetlendi. [37.271 strict kayıt](/github/sqliters/target/perf-work/v160-mixed-index-delete-clear.UzNSTeO9/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-delete-where-one-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 318,56 ms taban, 399,37 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-delete-where-one-disk-verify/res/gates-validation.txt).
