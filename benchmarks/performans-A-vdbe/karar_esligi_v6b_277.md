# V6b.277 — İşaretli takvim sonrası floor ve ceiling

Geçerli ISO başlangıçta dört haneli işaretli takvim kaydırmasının terminal floor ve ceiling adımları C ile eş. Düzeltme altı Tcl vakasını kapattı ve date-19 adayını 42/42 yaptı. Üç kodlamada 1.490 terminal çıktı ve 30 C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v276-date-packed-terminal-floor-ceiling-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 546,41 ms taban, 705,34 ms aday. Süre oranı 1,291 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-packed-terminal-floor-ceiling-disk-verify/res/gates-validation.txt).
