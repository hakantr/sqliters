# V6b.448 — Uygulama fonksiyonlarının C liste sırası

390 yerel profil, 186.036 sıralı satır ve 3.120 kayıt işlemi C ile eş. Önceki 330 fark kapandı ve 300 yaşam döngüsü korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v448-function-list-application-hash-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1328 geçti, 1 başarısız, 325 yok sayıldı. ON 1326 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 561,03 ms taban, 724,76 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/function-list-application-order-disk-verify/res/gates-validation.txt).
