# V6b.450 — Derleme sınırına kadar FILE PMA işçileri

Üç ila sekiz işçi, sekiz üzerindeki isteklerin sınırı, BINARY toplam sırası, üç kodlama ve iki yürütme kipinde 10.111.938 VFS olayı ile değerlendirildi. Gerçek işçi kapanışı ve FK açık 180 sağlıklı koşu doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v450-worker-limit-rebased-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v450-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1329 geçti, 1 başarısız, 325 yok sayıldı. ON 1328 geçti, 2 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,23 ms taban, 385,69 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/maximum-worker-pma-disk-verify/res/gates-validation.txt).
