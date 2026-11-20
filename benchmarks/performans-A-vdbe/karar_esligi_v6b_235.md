# V6b.235 — ISO uzun saat kesri

Canonical ISO uzun saat kesrinde birikim, 0.999 üst sınırı ve tam saniye ile milisaniyeye yuvarlama C binary64 işlem sırasına uyar. 97 kesir ve bir son aralık bağlamında kip başına 3.528 ham kayıt eşleşti. Ek 288 ham %f/iJD bağlamı eş ve önceki C eş 60 %f çıktısı korundu. 73 çok uzun girdi için sonlu, sonsuz ölçek ve NaN davranışı ölçüldü. 1.144 C salt okunur açılışı, 12 VDBE testi ve 10.789 strict kayıt doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v234-iso-clock-rounding-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,60 ms taban, 377,44 ms aday. Süre oranı 1,256 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-clock-rounding-disk-verify/res/gates-validation.txt).
