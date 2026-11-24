# V6b.518 — NO ACTION ve RESTRICT sayacı ile işlem içi onarım

Birleşik kaynakta her kipte 384 açık işlem, 384 autocommit ve 288 onarım C eş. UTF-16 dinamik hata metni, rollback, Group64 38/38, Group65 52/52 ve ertelenmiş FK paketi 2/2. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v518-fk-counter-control-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v518-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1390 geçti, 0 başarısız, 324 yok sayıldı. ON 1390 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 304,66 ms taban, 391,49 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-counter-repair-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
