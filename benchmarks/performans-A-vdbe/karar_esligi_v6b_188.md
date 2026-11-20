# V6b.188 — Üç indeksli tabloda rowid nokta yazmaları

Tek karma ve iki farklı olağan tam doğrudan TEXT indeksi olan desteklenen MAIN rowid tablo, nokta UPDATE ve DELETE ile düz VALUES yazısında üç kökü aynı etkin yazma görüntüsünde doğrular. C'nin son oluşturulan indeks önce fiziksel sırası korunur. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v186-three-root-rowid-point.sGZ9SgoQ/sonuc.md)

1.026 yazıcı, 1.026 rollback ve normal takip yazması, 6.156 yeniden açılış ve 540 C fiziksel dosya çifti doğrulandı. 432 etkin WAL ve 270 çok seviyeli dosya çifti eş. [Strict manifest](/github/sqliters/target/perf-work/v186-three-root-rowid-point.sGZ9SgoQ/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/three-root-rowid-point-final-release-proof/res/proof.sha256)

Birleşik CLI 72 taze profilde 216 yazıcı, ham DB ve quick_check eşliği verdi. C salt okunur yeniden açılışlarında integrity_check ve satır çıktısı eş. [Birleşik tekrar](/github/sqliters/target/perf-work/v188-combined-three-index.jyJ8CqWJ/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 556,65 ms taban, 713,17 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/three-root-rowid-point-disk-verify/res/gates-validation.txt).
