# V6b.177 — Tek UPDATE ile iki indeks anahtarı

İki doğrulanmış doğrudan kökü olan desteklenen MAIN rowid tablolarında tek UPDATE iki anahtarı değiştirebilir. Düz eşitlik indeksle çalışıyorsa aday rowid'ler toplanır ve artan sırada birer kere yazılır. Kolasyon uyuşmazlığı tablo taramasının yönünü korur. C'nin iki köke son oluşturulan indeks önce yazma sırası mevcut DML yordamıyla korunur. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v176-dual-key-update.qS8EPinQ/sonuc.md)

768 native yazıcı, 768 takip yazıcısı, 1.536 rollback ve 2.304 yeniden açılış doğrulandı. 144 ek çok sayfalı akışta 96 C–Rust fiziksel çift eş. Önceki 196 satırlı tabanla C'nin bulduğu 43 integrity ve bir count tanısı kapandı, takip yazması tamamlandı. [Bağımsız exact kayıt](/github/sqliters/target/perf-work/v176-dual-key-update.qS8EPinQ/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/dual-index-key-update-final-release-proof/res/proof.sha256)

Birleşik CLI ayrı 196 satırlı 48 profilde 144 C/OFF/ON yazıcıda ve sonraki C yeniden açılışında ham DB SHA ve çıktıyı eşledi. [Birleşik tekrar](/github/sqliters/target/perf-work/v177-combined-two-index.TA5cIXbi/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 331,85 ms taban, 426,16 ms aday. Süre oranı 1,284 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/dual-index-key-update-disk-verify/res/gates-validation.txt).
