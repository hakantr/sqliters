# V6b.240 — STRFTIME %k

STRFTIME %k mevcut saat çıktısı ile aynı değeri iki karakter genişlikte boşlukla doldurur. 185 bağımsız bağlam iki kipte ham eş. 0–23 saatler, üç kodlama, UTC/ofset, tarihli ve yalın 24:00, NULL ve karışık biçimler ölçüldü. 198 gerçek saklama satırı, 654 C açılışı, 12 VDBE testi ve 6.895 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v239-strftime-space-hour-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 313,45 ms taban, 395,92 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/strftime-space-hour-disk-verify/res/gates-validation.txt).
