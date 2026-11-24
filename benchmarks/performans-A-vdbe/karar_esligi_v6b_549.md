# V6b.549 — Üç ORDER BY Tcl kontrolünü varsayılan kapıya ekleme

orderby2, orderby5 ve orderbyA iki kipte önce seçili, ardından varsayılan çalışma ile geçti. İki kipte beklenen üç ek başarılı ve üç eksik ignore kaydı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/orderby-coverage-tcl-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v549-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1423 geçti, 0 başarısız, 320 yok sayıldı. ON 1423 geçti, 0 başarısız, 320 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,15 ms taban, 385,88 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/orderby-tcl-coverage-restoration-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
