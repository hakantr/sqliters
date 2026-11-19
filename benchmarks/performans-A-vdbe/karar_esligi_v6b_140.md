# V6b.140 — Karma indeksli rowid aralık DML'inde ters sıra

Uygun UPDATE ve DELETE deyimleri hazırlanmış deyimin reverse_unordered_selects kararını kullanır. Ters aralıkta C'nin üst sınır seek ve Prev davranışı, fiziksel karşılaştırıcı aynı Btree yazma görüntüsünde seçildikten sonra korunur. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v139-mixed-index-reverse-rowid.X06r44ky/sonuc.md)

İleri ve ters yönün her birinde 216 yazıcı, 1.512 salt okunur açılış, 108 C/Rust ve 27 eski Rust OFF/ON tam DB çifti ile 90 normalize WAL çifti eş. Çok sayfalı, bağlı ve nullable korumaları geçti. [35.737 strict kayıt](/github/sqliters/target/perf-work/v139-mixed-index-reverse-rowid.X06r44ky/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-reverse-rowid-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,87 ms taban, 383,66 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-reverse-rowid-disk-verify/res/gates-validation.txt).
