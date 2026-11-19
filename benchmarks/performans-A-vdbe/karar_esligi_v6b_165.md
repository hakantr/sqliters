# V6b.165 — Karma indeksli rowid aralığı ve sütun filtresi

Tek uygun MAIN rowid tabloda rowid aralığının indekslenmeyen doğrudan sütun eşitliğiyle AND birleşimi, mevcut fiziksel indeks yazma kararını kullanır. Hazırlanan C rowid aralığı yönü ve özgün WHERE filtresi korunur. [Bağımsız C ve Rust kaydı](/github/sqliters/target/perf-work/v164-mixed-index-range-filter.CRXiBmj3/sonuc.md)

144 native yazıcı, 288 rollback, 432 C/OFF/ON açılış ve 5.184 mutasyon ziyareti eş. 72 C/Rust ve 18 eski OFF/ON tam DB çifti, 120 normalize WAL. Önceki ters büyüme dosya farkı kapandı. [9.000 strict kayıt](/github/sqliters/target/perf-work/v164-mixed-index-range-filter.CRXiBmj3/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-range-filter-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 380,25 ms taban, 417,65 ms aday. Süre oranı 1,098 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-range-filter-disk-verify/res/gates-validation.txt).
