# V6b.233 — ISO son Julian aralığı

Geçerli ayrıştırılmış ISO tarih ve saat, değiştiricilerden sonra Julian aralığı dışındaysa C gibi NULL veriyor. 29 ayrı sınır farkı kapandı. 905 C açılışı ve 12 VDBE testi geçti. [Bağımsız kanıt](/github/sqliters/target/perf-work/v232-iso-final-julian-range-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 309,65 ms taban, 383,65 ms aday. Süre oranı 1,239 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-final-julian-range-disk-verify/res/gates-validation.txt).
