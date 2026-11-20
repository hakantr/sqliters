# V6b.210 — Yazısız TEMP FILE OFF geri alma

Sayfa yazılmamış ve backing henüz açılmamış özel TEMP FILE OFF işleminde önceki kaydedilmiş sayfalar korunur. 1.296 yeni ve 3.024 koruma C açılışı doğrulandı. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v208-temp-file-off-lifecycle-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 301,57 ms taban, 379,75 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-file-off-zero-write-disk-verify/res/gates-validation.txt).
