# V6b.132 — Karma indeks bütünlük tanısı

Tek MAIN rowid tablonun uygun tek karma yerleşik indeksinde, integrity_check C'nin dış kolasyonunu, kayıt anahtarını ve tablo boyunca aynı indeks cursorunu kullanır. Normal C dosyasında bütünlük sonucu ok. Eski iç sırada tanı satırları, sıraları ve hata bütçesi C ile eş. [Bağımsız C/Rust kanıtı](/github/sqliters/target/perf-work/v131-mixed-index-integrity-independent.jTkUGlZg/sonuc.md)

114 görüntüde, her iki Rust kipinde 1.710/1.710 grup C ile eş ve 504 önceki tanı grubu düzeldi. Her kipte 456 quick_check grubu, 324 önceki SELECT, 254 metadata grubu ve 36 arşiv açılışı korundu. 186 DB görüntüsünün SHA değeri sabit. [Strict SHA kanıtı](/github/sqliters/target/perf-work/v131-mixed-index-integrity-independent.jTkUGlZg/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-integrity-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,41 ms taban, 381,36 ms aday. Süre oranı 1,261 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-integrity-disk-verify/res/gates-validation.txt).
