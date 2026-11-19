# V6b.172 — İkinci indeks eşitliğiyle UPDATE ve DELETE

Tek karma ve bir olağan doğrudan indeksi olan desteklenen MAIN rowid tablolarında C eşitlik filtresi için kolasyonu uygun ikinci kökü arar. Sütun ve kök kolasyonu ayrıldığında ileri veya ters tablo taraması kullanır. Rust filtre kökünü aktif yazma görüntüsünde doğrulanan fiziksel kök numarasıyla seçer. UPDATE karma kökü, DELETE iki kökün üyeliğini mevcut fiziksel yazma kurallarıyla günceller. [Bağımsız C/Rust kanıtı](/github/sqliters/target/perf-work/v168-ordinary-index-equality.dzsFEWHv/sonuc.md)

576 native yazıcı, 1.152 rollback ve 1.728 yeniden açılış doğrulandı. 288 C–Rust ve 72 eski iç düzen OFF–ON tam DB çifti eş. İki iç düğüm köklü 196 satırlı ek ölçümde 144 yazıcı, 432 yeniden açılış ve 96 tam C–Rust çifti eş. [Strict manifest](/github/sqliters/target/perf-work/v168-ordinary-index-equality.dzsFEWHv/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/ordinary-index-equality-final-release-proof/res/proof.sha256)

Birleşik CLI'de 48 taze profil ve 144 yazıcı doğrulandı. Her çıktı, C salt okunur yeniden açılışı ve ana DB SHA eşti. [Birleşik tekrar](/github/sqliters/target/perf-work/v172-combined-two-index.m1lWRqrB/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,63 ms taban, 381,63 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/ordinary-index-equality-disk-verify/res/gates-validation.txt).
