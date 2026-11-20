# V6b.216 — Bağlı okuyucunun rollback sonucu

İlk bağlı DELETE/WAL dosyasındaki basit açık rowid okuyucusu geri alınmış değeri C gibi görüyor. 24 OFF farkı kapandı. 480 profil ve 2.880 C yeniden açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v213-aux-rollback-reader-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,34 ms taban, 380,32 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/attached-rollback-live-reader-disk-verify/res/gates-validation.txt).
