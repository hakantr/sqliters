# V6b.214 — Hazırlanmış count_changes sonucu

Normal yazı hazırlama anındaki sonuç biçimini koruyor. Sayım satırı ve changes() zamanlaması C ile eş. 1.128 ana profil ve 144 INSERT SELECT profil doğrulandı. [Bağımsız sonuç](/github/sqliters/target/perf-work/v214-count-changes-prepared-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 397,75 ms taban, 466,72 ms aday. Süre oranı 1,173 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-count-changes-disk-verify/res/gates-validation.txt).
