# V6b.203 — TEMP cache_size şema ve pager ömrü

Şema ayarı korunuyor. Yeni pager varsayılan cache ile açılıyor. 876 native ve C açılışı eş, önceki 72 regresyon kapandı. [Kanıt](/github/sqliters/target/perf-work/v199-temp-cache-cookie-rebase-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,51 ms taban, 384,75 ms aday. Süre oranı 1,243 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-cache-cookie-lifecycle-disk-verify/res/gates-validation.txt).
