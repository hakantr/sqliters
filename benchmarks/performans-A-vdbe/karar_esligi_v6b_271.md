# V6b.271 — Tek ISO tarih floor değiştiricisi

Dört haneli işaretsiz ISO tarihin tek terminal floor adımı C'nin başlangıç günü taşmasını düzeltiyor. 14 Tcl vakası ve üç kodlamada 800 hedef C eş, önceki 14 ceiling vakası başarılı. 36 salt okunur C açılışı ve DELETE/WAL dosya baytları eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v271-date-terminal-floor-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 565,34 ms taban, 724,23 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-terminal-floor-disk-verify/res/gates-validation.txt).
