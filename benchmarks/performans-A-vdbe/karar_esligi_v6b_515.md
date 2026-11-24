# V6b.515 — ROWID parent TEXT anahtarı ve numeric child yabancı anahtar action sayacı

Tek ve bileşik TEXT PRIMARY KEY ile ayrı TEXT UNIQUE indekslerde 4.032 native C karşılaştırması ve 4.032 ters yön salt okunur açılış eş. Group64 38/38 ve Group65 50/50 iki kipte. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-rowid-numeric-child-action-v514-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v515-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1388 geçti, 0 başarısız, 324 yok sayıldı. ON 1388 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,82 ms taban, 386,02 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-rowid-parent-counter-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
