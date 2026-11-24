# V6b.563 — Hazırlanmış rowid sıralamasında SORT sayacı ve işçi geçişi

Kip başına 28 önceki SORT farkı kapandı. 66 stok, 72 dinamik işçi geçişi ve 774 BINARY karşılaştırması C ile eş. Fiziksel PMA ve önceki kolasyon kontrolleri geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/binary-rowid-stock-sort-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v563-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1437 geçti, 0 başarısız, 319 yok sayıldı. ON 1437 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,72 ms taban, 387,93 ms aday. Süre oranı 1,299 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/stock-rowid-sort-statement-status-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
