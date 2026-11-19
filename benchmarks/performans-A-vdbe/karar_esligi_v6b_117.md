# V6b.117 — Aynı collation zincirinde dış yazım

Doğrudan sütunun aynı yerleşik COLLATE adı tekrarlanırsa, `index_xinfo.coll` C gibi dıştaki token yazımını gösteriyor. Katalog terimi sınırları ve mevcut `KeySortOrder` aynı metadata sözleşmesini koruyor. [C ve Rust kaynak/AST kanıtı](/github/sqliters/target/perf-work/index-xinfo-repeated-collation-independent/sonuc.md)

144 birleşik metadata grubu ve 72/72 byte-eş boş dosya C ile tekrar doğrulandı. Önceki 24 ve ek 168 yazım farkı kapandı. Salt okunur C açılışında SHA aynı. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özet birebir. [Exact bağımsız karşılaştırma](/github/sqliters/target/perf-work/index-xinfo-repeated-collation-v117-proof/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/index-xinfo-repeated-collation-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,07 ms taban, 378,52 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/index-xinfo-repeated-collation-disk-verify/res/gates-validation.txt).
