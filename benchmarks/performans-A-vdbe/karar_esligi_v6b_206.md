# V6b.206 — ORDER BY açık çıktı alias önceliği

Yalın ORDER BY adı ilk açık çıktı alias’ını çözümlüyor. İki kipte 24 profil, 108 C açılışı ve bağlama sonrası sıra C ile eş. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v205-order-alias-resolution-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,78 ms taban, 385,07 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/select-order-alias-disk-verify/res/gates-validation.txt).
