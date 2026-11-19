# V6b.174 — Eşitlikle aranan ikinci indeks anahtarının güncellenmesi

Desteklenen iki köklü MAIN rowid tablosunda `SET tag = literal/bind WHERE tag = literal/bind`, mevcut yazma görüntüsünde doğrulanan iki fiziksel sıra ile çalışır. RTRIM indeks aramasında C aday rowid'leri toplar ve artan sırada bir kez yazar. BINARY tablo taramasında yazma hazırlanan tarama yönünü izler. Rust mevcut iki geçişli ve tek geçişli yürütmeyi kullanır. [Bağımsız kaynak ve karar](/github/sqliters/target/perf-work/v172-ordinary-index-self-update.0v1tC8sg/sonuc.md)

576 native yazıcı, 1.152 rollback ve 1.728 yeniden açılış doğrulandı. İki çok sayfalı kökte 144 ek yazıcı ve 432 yeniden açılış eş. Aynı büyük BINARY ters tarama vakalarında önceki tabanın 20 fiziksel farkı kapandı. Karma indeksin tüm alt ağaç sayfaları yerinde kaldı. [Strict manifest](/github/sqliters/target/perf-work/v172-ordinary-index-self-update.0v1tC8sg/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/ordinary-index-self-update-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,04 ms taban, 383,11 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ordinary-index-self-update-disk-verify/res/gates-validation.txt).
