# V6b.91 — ORDER BY içinde +TRUE ve +FALSE

Normal tek tablo ORDER BY teriminde unary artı içindeki çıplak TRUE/FALSE, C'nin kaynak sütunu, uygun takma ad ve literal sırasını izliyor. Takma addan gelen tamsayı sıralama değeri olarak kalıyor. Açık veya kaynak sütunundan gelen kolasyon korunuyor. [C kaynak ve karşılaştırma](/github/sqliters/target/perf-work/truth-order-unary-independent/sonuc.md)

132 hedef ve farklı ad bayraklarındaki 48 normal vaka iki kipte C ile ham eş. 30 ayrı çıplak, nitelikli, ordinal veya kapsam dışı koruma aynı. 690 arşiv oturumunda 684 aynı, değişen eski unary sonuçlar C ile eş. 4.500 fiziksel dosya C ile salt okunur açıldı ve SHA-256 eş. [Bağımsız ham kayıt ve kapsam](/github/sqliters/target/perf-work/truth-order-unary-independent/res/proof.sha256)

Birleşik optimize aday, 387/400 OFF ve 400/400 ON ile önceki ham DML özetlerini korudu. [Tam korpus](/github/sqliters/target/perf-work/truth-order-unary-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,67 ms taban, 376,74 ms aday. Süre oranı 1,257 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/truth-order-unary-disk-verify/res/gates-validation.txt).
