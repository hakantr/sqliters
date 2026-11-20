# V6b.222 — Açık sayısal saat dilimi ofseti

Geçerli ±HH:MM ofseti UTC’ye dönüştürülüyor. 18 ayrı ana girdi ve 108 sonuç kaydı her kipte C ile eş. Negatif yıl DATE ve DATETIME biçiminde işaret ve dört rakam ayrı. STRFTIME C biçimini koruyor. 156 C salt okunur açılışı ve 12 VDBE testi doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v221-explicit-utc-offset-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,64 ms taban, 383,82 ms aday. Süre oranı 1,268 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/explicit-utc-offset-disk-verify/res/gates-validation.txt).
