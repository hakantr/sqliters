# V6b.270 — Tek ISO tarih ceiling değiştiricisi

Dört haneli işaretsiz ISO tarihindeki tek terminal ceiling C normalizasyonunu kullanır. 14 Tcl vakası, üç kodlamada 706 hedef ve 36 salt okunur C açılışı doğrulandı. Diğer 28 Tcl vakasının ham davranışı korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v270-date-terminal-ceiling-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 558,04 ms taban, 726,86 ms aday. Süre oranı 1,303 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-terminal-ceiling-disk-verify/res/gates-validation.txt).
