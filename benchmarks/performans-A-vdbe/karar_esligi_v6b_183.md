# V6b.183 — İki kökte rowid aralığı ve sütun filtresi

Desteklenen MAIN rowid tablolarında rowid aralığı ve indekslenmeyen doğrudan sütun eşitliği, iki fiziksel kökü ilk mutasyondan önce aynı aktif yazma görüntüsünde doğrular. C'nin rowid aralığı ziyaret yönü ve kalan eşitlik filtresi korunur. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v181-two-root-range-filter.5EylSipu/sonuc.md)

396 native, 396 takip yazıcısı, 792 rollback ve 1.188 yeniden açılış eş. 120 çok seviyeli yazıcıda iki kökün tam sayfa SHA ve DB imajı doğrulandı. Önceki C bütünlük denetimi tanı sınırına ulaşıp takip yazması dururken aday aynı akışta bayt eş dosya ve başarılı takip yazması verdi. [Strict manifest](/github/sqliters/target/perf-work/v181-two-root-range-filter.5EylSipu/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/two-root-range-filter-final-release-proof/res/proof.sha256)

Birleşik CLI, 196 satırlı 48 taze profilde 144 C/OFF/ON yazıcı ve C yeniden açılışında ham çıktı ve DB SHA'yı eşledi. [Birleşik tekrar](/github/sqliters/target/perf-work/v183-combined-two-index.BfXv5l1O/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 566,40 ms taban, 711,07 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/two-root-range-filter-disk-verify/res/gates-validation.txt).
