# V6b.205 — Belirsiz SELECT sütunu byte ofseti

Aynı genişletilmiş kod ve mesajda C çözümleyicisinin byte ofseti taşınıyor. OFF 240 tanı, 108 C açılışı ve 216 legacy tanı profili eş. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v204-select-ambiguous-offset-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,86 ms taban, 388,96 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/select-ambiguous-column-offset-disk-verify/res/gates-validation.txt).
