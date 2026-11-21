# V6b.289 — Gün ay ve yıl sonu tarih değiştiricileri

Normal ISO tarih metninde end of day/month/year, C'nin son milisaniye ve civil yıl aralığı kararını kullanır. Dokuz Tcl vakası, kodlama başına 976 hedef ve 30 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v285-date-terminal-end-of-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 540,48 ms taban, 698,57 ms aday. Süre oranı 1,293 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-terminal-end-of-disk-verify/res/gates-validation.txt).
