# V6b.517 — İşçili yerleşik sıralamada reset ve SORT sayacı

Her kipte 576/576 stock ve 528/528 kayıtlı C karşılaştırması. READY ve BUSY reset=1 davranışı, kümülatif SORT, satırlar, işçiler ve callback kontrolleri eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/stock-worker-sort-counter-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v517-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1389 geçti, 0 başarısız, 324 yok sayıldı. ON 1389 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,08 ms taban, 385,73 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/stock-worker-sort-counter-reset-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
