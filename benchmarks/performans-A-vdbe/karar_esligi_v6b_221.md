# V6b.221 — Backup finish ve hedefin saklı tanısı

Açık backup finish, hedefin önceki saklı sözdizimi metnini temizliyor. 72 hazır V1 akışında eski mesajın taşınması önlendi. 400 profil ve 1.644 C salt okunur açılışı ölçüldü. [Bağımsız kanıt](/github/sqliters/target/perf-work/v220-backup-explicit-finish-retained-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 552,95 ms taban, 714,40 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/backup-finish-retained-message-disk-verify/res/gates-validation.txt).
