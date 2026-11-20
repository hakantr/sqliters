# V6b.238 — Yalın ISO 24:00

Yalın 24:00 ayrıştırmasında ilk takvim değiştiricisine kadar C'nin tarih ve ofset durumu izlenir. 55 kabul ve 10 iJD farkında kip başına 1.170 ham kayıt eşleşti. 499 diğer ana bağlam ve 104 tarihli düzeltme korundu. İki TZ'de 54 mevcut host dönüşüm zinciri ham eş kaldı. 72 gerçek saklama, 1.055 C açılışı, 12 VDBE testi ve 10.273 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v238-iso-bare-hour24-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 314,92 ms taban, 395,11 ms aday. Süre oranı 1,255 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-bare-hour24-disk-verify/res/gates-validation.txt).
