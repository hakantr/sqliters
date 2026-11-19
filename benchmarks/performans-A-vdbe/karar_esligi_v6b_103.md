# V6b.103 — WITHOUT ROWID otomatik UNIQUE kapsama kararı

C, otomatik UNIQUE indeksinin kapsama maskesini `WITHOUT ROWID` dönüşümünden önce hesaplıyor. Fiziksel kayda sonradan eklenen PRIMARY KEY sütunları otomatik UNIQUE maskesinde yer almıyor. Açık ikincil indeks bu sütunları kapsayabiliyor. MAIN üzerindeki yalın ve filtresiz projeksiyonda derlenmiş SELECT ile ilişkisel SELECT kaynağı aynı plan kararını kullanıyor. Mevcut B-tree erişimi fiziksel indeks sütunlarını koruyor. [C kaynak ve karşılaştırma](/github/sqliters/target/perf-work/wr-unique-cover-independent/sonuc.md)

Her V1/V2 ve kip grubunda 165/165 disk dosyası ve C salt okunur açılışı eş. Ham eşlik OFF 84/165'ten 138/165'e, ON 111/165'ten 147/165'e çıktı. Değişen bütün ham sonuçlar C ile eş. Önceki 703 pozitif grubun çıktısı, dosyası ve C açılışı korundu. Birleşik optimize DML, önceki ham özetle birebir ve OFF 390/400, ON 400/400. [Odak kanıtı](/github/sqliters/target/perf-work/wr-unique-cover-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/wr-autoindex-cover-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 297,92 ms taban, 371,20 ms aday. Süre oranı 1,246 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wr-autoindex-cover-disk-verify/res/gates-validation.txt).
