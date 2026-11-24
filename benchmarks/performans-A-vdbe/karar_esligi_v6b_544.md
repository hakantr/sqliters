# V6b.544 — Ayrı ertelenmiş FK başvurusunun işlem sayacı

3.648 yeni karşılaştırma ve 5.184 önceki kontrol C SQL sonucu, ana dosya, C yeniden açılışı ve FK denetiminde eş. Açık işlemde COMMIT ve ROLLBACK C ile aynı. Group64 40/40, Group65 64/64 iki kipte. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-child-deferred-reference-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v544-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1416 geçti, 0 başarısız, 324 yok sayıldı. ON 1416 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 297,38 ms taban, 381,63 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-separate-deferred-owner-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
