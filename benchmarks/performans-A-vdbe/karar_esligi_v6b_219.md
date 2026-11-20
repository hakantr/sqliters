# V6b.219 — Aggregate BLOB sayısal öneki

SUM, TOTAL ve AVG, BLOB sayısal önekini C gibi veritabanının UTF-8/UTF-16 metin kodlamasıyla REAL olarak topluyor. Genel aggregate, örtücü raw ve pencere inverse akışları aynı sayısal dönüşümü kullanıyor. 420 profil ve 2.520 C salt okunur açılışı geçti. Her kipte 1.816 çıktı farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v219-covering-aggregate-sql-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,20 ms taban, 373,87 ms aday. Süre oranı 1,254 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/aggregate-blob-numeric-encoding-disk-verify/res/gates-validation.txt).
