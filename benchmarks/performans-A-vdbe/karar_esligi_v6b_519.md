# V6b.519 — Birden fazla doğrudan FK child action sayaçları

6.912 native C eşliği, farklı action ve farklı parent anahtar seçimi dahil. Her kipte 384 önceki NO ACTION/RESTRICT, Group64 38/38 ve Group65 53/53 geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v519-fk-sibling-action-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v519-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1391 geçti, 0 başarısız, 324 yok sayıldı. ON 1391 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,82 ms taban, 389,66 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-sibling-action-counter-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
