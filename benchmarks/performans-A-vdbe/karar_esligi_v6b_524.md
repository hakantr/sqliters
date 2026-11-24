# V6b.524 — İki basamaklı FK zincirinde yazma, COMMIT ve rollback

Birleşik kaynakta her kipte 864 açık işlem ve 1.728 autocommit/rollback C eş. Group64 39/39, Group65 57/57 ve ertelenmiş FK 2/2. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v524-fk-two-hop-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v524-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1396 geçti, 0 başarısız, 324 yok sayıldı. ON 1396 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,43 ms taban, 391,02 ms aday. Süre oranı 1,302 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-two-hop-action-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
