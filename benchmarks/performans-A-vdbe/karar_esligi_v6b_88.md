# V6b.88 — WHERE için TRUE ve FALSE adlı sonuç takma adları

Bilinen tek normal tablo sorgusunda aynı adla gerçek sütun bulunmuyorsa çıplak TRUE/FALSE için ilk uygun skaler sonuç takma adı seçiliyor. C'nin gerçek sütun, takma ad ve boolean literal sırası korunuyor. Tamsayı veya sütun takma adı normal IS karşılaştırmasında kalıyor. Boolean literal takma adı C truth sınıflamasını koruyor. [C kaynak ve bağımsız kanıt](/github/sqliters/target/perf-work/truth-where-alias-independent/sonuc.md)

108 yeni hedef iki kipte ham C eş. 30 koruma, önceki 114 gerçek sütun akışı, 480 sayısal karşılaştırma ve 18 hazırlama tanısı korundu. 1.545 fiziksel dosya C ile salt okunur açıldı ve SHA-256 eş. Birleşik optimize aday 387/400 OFF ve 400/400 ON ile önceki ham DML özetlerini verdi. [Odak doğrulaması](/github/sqliters/target/perf-work/truth-where-alias-independent/res/proof.sha256) · [Tam korpus](/github/sqliters/target/perf-work/truth-alias-final-release-proof/res/proof.sha256)

Birleşik güncel kaynaklarda odak yeniden doğrulandı. 108 hedef ve 30 koruma, 690 C açılışı. [Kesin taban kanıtı](/github/sqliters/target/perf-work/truth-exact-native-focus/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 387/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,03 ms taban, 375,38 ms aday. Süre oranı 1,251 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/truth-alias-disk-verify/res/gates-validation.txt).
