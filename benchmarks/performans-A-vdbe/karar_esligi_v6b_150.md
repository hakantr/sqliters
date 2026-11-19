# V6b.150 — Karma indeksli doğrudan rowid IN yazımı

Uygun UPDATE ve DELETE deyimi, boş olmayan doğrudan rowid veya INTEGER PRIMARY KEY alias literal ve bağlı IN listesini C gibi tekilleştirir ve hazırlanmış yönde ziyaret eder. Fiziksel karşılaştırıcı aynı yazma görüntüsünde seçilir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v142-mixed-index-rowid-in.XaH50HzC/sonuc.md)

144 normal yazıcı ve rollback, 432 C/OFF/ON salt okunur açılış, 4.608 gerçek rowid ziyareti eş. 72 C/Rust ve 18 eski Rust OFF/ON tam DB çifti, 180 nonce ayrımlı WAL çifti doğrulandı. [8.150 strict kayıt](/github/sqliters/target/perf-work/v142-mixed-index-rowid-in.XaH50HzC/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-rowid-in-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,03 ms taban, 388,79 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-rowid-in-disk-verify/res/gates-validation.txt).
