# V6b.152 — Karma indeksli sıralı INSERT SELECT

Ayrı normal MAIN rowid tablosundan doğrudan sütun okuyan, açık hedef sütun listeli INSERT SELECT uygun karma indeks yazma yoluna katılır. Kaynak açık rowid veya INTEGER PRIMARY KEY sırasını sağlar. Hedefin fiziksel karşılaştırıcısı ilk hedef değişikliğinden önce mevcut B-tree yazma görüntüsünde seçilir. [C coroutine ve exact kaynak kanıtı](/github/sqliters/target/perf-work/v150-mixed-index-insert-select.OZqdTzGx/sonuc.md)

144 DELETE ve WAL yazıcı, 144 dosya düzeyinde rollback denetimi, 432 salt okunur açılış ve 3.600 gerçek rowid ve kaynak etiket ziyareti doğrulandı. 72 C/Rust ve 18 eski Rust OFF/ON tam DB çifti, 180 normalize WAL çifti ve kaynak kök sayfasının SHA eşliği ölçüldü. [8.036 strict kayıt](/github/sqliters/target/perf-work/v150-mixed-index-insert-select.OZqdTzGx/sha256.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/mixed-index-insert-select-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 304,65 ms taban, 385,61 ms aday. Süre oranı 1,266 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-index-insert-select-disk-verify/res/gates-validation.txt).
