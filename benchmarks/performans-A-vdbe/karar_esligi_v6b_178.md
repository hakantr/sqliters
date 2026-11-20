# V6b.178 — V1 ve V2 bağlantı tanısı

Yürütmeden expire olan legacy V1 deyimi step 1 ve reset veya finalize 17 döndürdüğünde bağlantı mevcut tanısını korur. Gerçek şema cookie değişikliği yürütme sırasında saptandığında step tanıyı 1, reset tanıyı 17 olarak aktarır. V2 başarılı yeniden hazırlamada ilk şema kontrolünün sonucuna göre C bağlantı tanısını kurar. [C davranışı](/github/sqliters/target/perf-work/v178-legacy-schema-expected.VkLWGFg8/final/stock.txt) · [Bağımsız doğrulama](/github/sqliters/target/perf-work/v178-legacy-v2-reprepare-independent/sonuc.md)

Önceki 266 C–Tcl ve doğrudan Rust karşılaştırması tekrarlandı. 36 ALTER ve global expiry profilinde ve 21 ayrı V2 korumasında sonuçlar C ile eş. [Kanıt manifesti](/github/sqliters/target/perf-work/v178-legacy-v2-reprepare-independent/proof.sha256) · [Birleşik kaynak ve optimize DML](/github/sqliters/target/perf-work/v178-with-v178-correction.6AwFeUil/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 310,55 ms taban, 387,79 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-connection-diagnostic-disk-verify/res/gates-validation.txt).
