# V6b.533 — NO ACTION ve diğer doğrudan action için ortak FK sayacı

864/864 native C sonuç, iki yön açılış, dosya byte ve FK bütünlüğü eş. Önceden hazırlanan parent onarımı C gibi sayacı görür. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-sibling-mixed-counter-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v533-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1405 geçti, 0 başarısız, 324 yok sayıldı. ON 1405 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,90 ms taban, 390,87 ms aday. Süre oranı 1,295 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-sibling-no-action-repair-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
