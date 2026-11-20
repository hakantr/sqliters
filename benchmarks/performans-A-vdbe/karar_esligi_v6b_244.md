# V6b.244 — Negatif tarihli ISO 24:00

Canonical negatif tarihli 24:00, C'nin önce yıl büyüklüğünü ilerletip işareti uygulama kararını ve ham sıfır kesrini izler. Başlangıç ve değiştiricilerde Julian aralığı korunan 145 hedef, kip başına 2.610 ham kayıt C eş. 59 ana bağlam korundu. 145 gerçek saklama, 288 C açılışı, 12 VDBE ve 4.598 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v244-negative-dated-hour24-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 305,18 ms taban, 380,78 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/negative-dated-hour24-disk-verify/res/gates-validation.txt).
