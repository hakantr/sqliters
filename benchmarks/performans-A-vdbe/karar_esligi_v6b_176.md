# V6b.176 — İki kökte karma anahtar eşitliği

İki tam doğrudan indeksi olan desteklenen MAIN rowid tablolarında düz karma anahtar eşitliği, etkin yazma görüntüsünde doğrulanan karma kökü kullanabilir. Uyumlu C indeks aramasında UPDATE adayları toplar ve artan rowid sırasıyla birer kere yazar. DELETE bir geçişte çalışır. Kolasyon uyuşmazlığı ve eski iç fiziksel düzen için mevcut tablo taraması ile köke uygun karşılaştırıcı kullanılır. [Bağımsız kaynak ve karar](/github/sqliters/target/perf-work/v174-two-root-mixed-equality.bZCEcXLv/sonuc.md)

576 native yazıcı, 576 takip yazıcısı, 1.152 rollback ve 1.728 yeniden açılış doğrulandı. 144 ek çok sayfalı yazıcıda 96 C–Rust tam DB çifti eş. Önceki 196 satırlı BINARY ters tarama tabanında C'nin bulduğu 58 integrity ve bir quick_check tanısı kapandı, takip yazması geçti. [Önceki taban gözlemi](/github/sqliters/target/perf-work/v174-two-root-mixed-equality.bZCEcXLv/res/baseline-observed/summary.json) · [Strict manifest](/github/sqliters/target/perf-work/v174-two-root-mixed-equality.bZCEcXLv/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/two-root-mixed-equality-final-release-proof/res/proof.sha256)

Birleşik CLI, 196 satırlı 48 taze profilde 144 yazıcı ve C yeniden açılışında aynı ham çıktı ve final DB SHA'yı verdi. [Birleşik tekrar](/github/sqliters/target/perf-work/v176-combined-two-index.Lpt10Qs7/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 308,22 ms taban, 388,55 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-root-mixed-equality-disk-verify/res/gates-validation.txt).
