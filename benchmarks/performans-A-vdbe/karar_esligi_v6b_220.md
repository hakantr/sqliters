# V6b.220 — Bellek hedefli backup geometri kontrolü

Farklı sayfa boyutundaki dolu bellek hedefinde step(0) READONLY 8 ve 0/0 sayaçları döndürüyor. Finish 8. Geometri hedef WRITE açıldıktan sonra ve sayfalar kopyalanmadan önce doğrulanıyor. 876 profil ve 5.004 C salt okunur açılışı ölçüldü. İki kipte 48 API farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v218-backup-memory-step-zero-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,20 ms taban, 379,64 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/backup-memory-step-zero-disk-verify/res/gates-validation.txt).
