# V6b.142 — Karma indeksli WHERE'siz UPDATE

Uygun normal MAIN rowid tablonun WHERE'siz UPDATE deyimi, mevcut ilişkisel mutasyon ve aynı yazma görüntüsündeki fiziksel indeks karşılaştırıcısını kullanır. C gibi hazırlanmış ileri Rewind/Next veya ters Last/Prev yönüyle çalışır. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v140-mixed-index-unfiltered-update.81uPRFBt/sonuc.md)

432 yazıcı ve rollback, 216 C/Rust ve 54 eski Rust OFF/ON tam DB çifti, 180 doğrulanmış nonce ayrımlı WAL çifti ve 3.024 salt okunur açılış eş. Çok sayfalı ve bağlı yazma korumaları geçti. [Strict fiziksel kanıt](/github/sqliters/target/perf-work/v140-mixed-index-unfiltered-update.81uPRFBt/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-unfiltered-update-platform-release-proof/res/proof.sha256)

Birleşik exact release ikilisiyle ana ve çok sayfalı WRITE matrisleri tekrar ölçüldü. [17.389 strict birleşik tekrar](/github/sqliters/target/perf-work/v142-combined-replay.jJBMuvNz/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 516,94 ms taban, 713,83 ms aday. Süre oranı 1,381 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-unfiltered-update-disk-verify/res/gates-validation.txt).
