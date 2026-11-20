# V6b.186 — İki kökte açık WHERE 1 DELETE

Desteklenen MAIN rowid tablosundaki açık `DELETE … WHERE 1`, iki kökü aynı yazma görüntüsünde doğrulayıp C'nin satır bazlı silme sırasını izler. WHERE'siz DELETE, C'nin ayrı Clear işlemini sürdürür. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v183-two-root-where-one.yotmeU2n/sonuc.md)

396 yazıcı ve geri alma akışı, 1.188 yeniden açılış, 48 çok sayfalı Clear ve WHERE 1 ayrımı ve ayrı canlı WAL doğrulandı. Önceki 196 satırlı örneğin giriş sayısı ve boş sayfa farkı kapandı. Normal takip INSERT'i C dosyasıyla eş. [Strict manifest](/github/sqliters/target/perf-work/v183-two-root-where-one.yotmeU2n/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/two-root-where-one-final-release-proof/res/proof.sha256)

Birleşik CLI 96 taze profilde 288 yazıcı, C açılışı ve final DB SHA eşliği verdi. Clear ve WHERE 1, 48/48 profilde ayrı fiziksel imaj verdi. [Birleşik tekrar](/github/sqliters/target/perf-work/v186-combined-two-index.z276h6im/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 555,88 ms taban, 573,84 ms aday. Süre oranı 1,032 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-root-where-one-disk-verify/res/gates-validation.txt).
