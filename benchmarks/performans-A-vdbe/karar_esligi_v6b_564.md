# V6b.564 — İç içe ifadelerde ilk açık COLLATE

Üç kodlamadaki 84 C sonucu her kipte eş. Önceki 54 sütun/alias ve 48 unary artı kontrolü korundu. Özgün collate8 Tcl testinin 23 alt testi geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/collate8-first-explicit-v560.DTJ4V4Gx/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v564-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1438 geçti, 0 başarısız, 319 yok sayıldı. ON 1438 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 555,82 ms taban, 730,76 ms aday. Süre oranı 1,315 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/nested-first-explicit-collation-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
