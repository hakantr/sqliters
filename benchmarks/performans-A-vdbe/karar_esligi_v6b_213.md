# V6b.213 — Hazırlanmış trigger yazısı

Önceden hazırlanmış yazı recursion kararını koruyor. Yeniden hazırlanmış ve callback sırasında yeni hazırlanan deyim güncel ayarı kullanıyor. 24 OFF farkı kapandı ve 600 C salt okunur açılışı geçti. [Bağımsız sonuç](/github/sqliters/target/perf-work/v213-prepared-recursive-trigger-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,25 ms taban, 377,42 ms aday. Süre oranı 1,253 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-recursive-trigger-disk-verify/res/gates-validation.txt).
