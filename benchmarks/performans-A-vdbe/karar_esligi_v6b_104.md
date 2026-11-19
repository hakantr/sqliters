# V6b.104 — Zorlanmış indeks sabit IN sırası

`INDEXED BY` ve tek baş anahtarın sabit IN listesi, affinity uygulanmış arama anahtarlarını mevcut indeks yönünde dolaşıyor. Mevcut karşılaştırıcı veritabanı kodlamasını ve collation sırasını kullanıyor. SQLite C referansında aynı yön ephemeral B-tree üzerinden `Rewind/Next` veya `Last/Prev` ile doğrulandı. Kaynak sırası sağlıklı INSERT SELECT'in kalıcı hedef satırlarını da belirliyor. [C ve Rust karar kanıtı](/github/sqliters/target/perf-work/forced-literal-in-order-independent/sonuc.md)

Her V1/V2 API ve kipte 177/177 fiziksel dosya, kalıcı satır ve C salt okunur açılışı eş. Ham eşlik OFF 56/177'den 156/177'ye, ON 96/177'den 163/177'ye çıktı. Önceki 219 dosya ve açılış her kipte korundu. Birleşik optimize DML ham özetleri öncekiyle birebir ve OFF 390/400, ON 400/400. [Odak manifesti](/github/sqliters/target/perf-work/forced-literal-in-order-independent/sha256.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/forced-literal-in-order-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,37 ms taban, 373,77 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/forced-literal-in-order-disk-verify/res/gates-validation.txt).
