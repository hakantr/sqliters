# V6b.156 — Karma indeksli doğrudan sütun eşitliği

Tek karma indeksli MAIN rowid tabloda indekslenmeyen gerçek bir sütunun literal veya bağ değerle eşitliği, mevcut fiziksel karşılaştırıcı doğrulamasından geçen UPDATE ve DELETE kapsamına girer. Hazırlama ve ters FullTable taraması aynı sütun koşulunu kullanır. SQL karşılaştırması mevcut sütun affinity ve kolasyonuyla değerlendirilir. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v152-mixed-index-direct-filter.upWAhU4f/sonuc.md)

144 yazıcı, 432 rollback ve 432 salt okunur açılış, 9.072 mutasyon ziyareti ve 36 C yeniden hazırlama sayacı doğrulandı. 72 C/Rust ve 18 eski Rust OFF/ON tam DB çifti ile 120 normalize WAL çifti eş. [8.554 strict kayıt](/github/sqliters/target/perf-work/v152-mixed-index-direct-filter.upWAhU4f/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-direct-filter-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,78 ms taban, 380,61 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-direct-filter-disk-verify/res/gates-validation.txt).
