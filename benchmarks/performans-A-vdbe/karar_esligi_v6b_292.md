# V6b.292 — Tek saat kaydırması

Geçerli ISO tarih-saat girdisi ve tek saat kaydırmasında yerel SQL sonucu C ile eş. Dokuz Tcl vakası ve üç metin kodlamasında DELETE/WAL dosya kontrolleri doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v287-date-terminal-clock-shift-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 336,75 ms taban, 434,81 ms aday. Süre oranı 1,291 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-terminal-clock-shift-disk-verify/res/gates-validation.txt).
