# V6b.272 — Ay ve yıl kaydırması sonrası ceiling

Geçerli ISO başlangıç gününde tam ve işaretli ay veya yıl kaydırmasından sonra tek terminal ceiling, mevcut kaydırmanın C ile eş normalize sonucunu kullanır. Dört Tcl vakası, üç kodlamada 7.852 SQL ölçümü ve 27 C salt okunur açılışı eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v272-date-named-shift-terminal-ceiling-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,93 ms taban, 381,76 ms aday. Süre oranı 1,269 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/date-named-shift-terminal-ceiling-disk-verify/res/gates-validation.txt).
