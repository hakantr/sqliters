# V6b.287 — Typed Julian tam ay ve yıl

Tek typed Julian ve tam ay veya yıl değiştiricisi, C miktar ve Julian aralık denetimlerini kullanır. 388 hedefte 208 önceki fark ve dört Tcl vakası kapandı. Üç kodlamada 30 C salt okunur açılışı ve dosya baytları eş. [Bağımsız kanıt](/github/sqliters/target/perf-work/v284-typed-julian-whole-calendar-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1183 geçti, 4 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,68 ms taban, 379,98 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/typed-julian-whole-calendar-disk-verify/res/gates-validation.txt).
