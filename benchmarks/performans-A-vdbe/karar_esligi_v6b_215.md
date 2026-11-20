# V6b.215 — View DELETE hedef sayımı

Rowid tabanlı view DELETE hedefi INSTEAD OF trigger çalışmadan önce sayılıyor. 94 OFF farkı kapandı. 1.944 view profili ve 1.328 önceki kontrol eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v215-view-trigger-count-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1179 geçti, 8 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 549,61 ms taban, 709,93 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/view-delete-count-ignore-disk-verify/res/gates-validation.txt).
