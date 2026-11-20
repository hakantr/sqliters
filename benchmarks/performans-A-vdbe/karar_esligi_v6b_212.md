# V6b.212 — Açık TEMP FILE OFF yazısız geri alma

Gerçek TEMP dosyası açıkken sıfır yazılı geri alma önceki commit verisini koruyor. 1.608 birleşik C açılışı ve fiziksel TEMP yazma izi eş. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v210-temp-file-backing-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 332,94 ms taban, 412,67 ms aday. Süre oranı 1,239 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-file-backed-off-zero-write-disk-verify/res/gates-validation.txt).
