# V6b.243 — Negatif ISO yıl ayrıştırması

Canonical negatif ISO tarih ve olağan saatte geçerli yerel gün, dört ASCII yıl hanesi ve erken yıl sınırı mevcut Gregorian hesap ve son Julian denetimiyle çalışır. 54 hedef bağlam ve kip başına 972 ham kayıt C eş. 89 diğer bağlam ve önceki korumalar değişmedi. 54 gerçek saklama, C açılışları, 12 VDBE testi ve 6.803 strict final exact doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v242-iso-signed-year-final-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 300,51 ms taban, 377,91 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/iso-signed-year-disk-verify/res/gates-validation.txt).
