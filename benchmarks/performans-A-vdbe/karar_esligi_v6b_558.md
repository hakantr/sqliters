# V6b.558 — Tam ANALYZE için istatistik B-tree temizliği

ON ve FAST kiplerinde 96/96 ana dosya, çok sayfalı matriste 180/180 SQL, C reopen ve ana dosya eş. MAIN ve ATTACH için 12 işlem, 16/16 C–Rust imaj ve 12 rollback imajı eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/analyze-stat1-clear-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v558-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1432 geçti, 0 başarısız, 319 yok sayıldı. ON 1432 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,05 ms taban, 386,19 ms aday. Süre oranı 1,296 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/analyze-stat1-btree-clear-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
