# V6b.274 — Ay ve yıl kaydırması sonrası floor

Geçerli ISO tarihte tam ve işaretli ay veya yıl kaydırmasından sonra tek terminal floor, C günü taşma miktarını düşer. Dört Tcl vakası, üç kodlamada 15.704 SQL ölçümü ve 27 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v273-date-named-shift-terminal-floor-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 313,35 ms taban, 398,01 ms aday. Süre oranı 1,270 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-named-shift-terminal-floor-disk-verify/res/gates-validation.txt).
