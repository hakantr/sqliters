# V6b.160 — Karma indeks anahtarıyla eşitlik yazısı

Tek uygun MAIN rowid indeksinde SQL kolasyonu, C deklarasyonu ve etkin B-tree görüntüsündeki fiziksel sıra eşleşirse UPDATE ve DELETE mevcut indeks aramasını kullanır. Eski iç düzende SQL koşulu tablo taramasında değerlendirilir, doğrulanmış fiziksel sıra yazıda korunur. [C/ON ve ayrı eski düzen kanıtı](/github/sqliters/target/perf-work/v156-mixed-index-key-equality.e0O5uZts/sonuc.md)

144 native yazıcı, 432 rollback ve 432 salt okunur açılış, 7.848 mutasyon ziyareti ve 36 C yeniden hazırlama. 72 C/Rust ve 18 eski OFF/ON tam DB çifti. Ek 36 normal NOCASE/RTRIM yazıcı ve 108 C açılışı eş. [9.747 strict kayıt](/github/sqliters/target/perf-work/v156-mixed-index-key-equality.e0O5uZts/sha256.txt) · [Birleşik kanıt](/github/sqliters/target/perf-work/mixed-index-key-equality-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 306,34 ms taban, 384,83 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-key-equality-disk-verify/res/gates-validation.txt).
