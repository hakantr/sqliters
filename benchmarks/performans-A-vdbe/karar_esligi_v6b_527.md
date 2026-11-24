# V6b.527 — Üç ve dört basamaklı düz FK action zinciri

Birleşik kaynakta her kipte 15.552/15.552 C karşılaştırması, SQL, iki yönlü dosya açılışı, byte ve bütünlük eş. Group64 40/40, Group65 59/59 ve deferred FK 2/2. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v527-fk-linear-chain-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v527-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1399 geçti, 0 başarısız, 324 yok sayıldı. ON 1399 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,83 ms taban, 388,90 ms aday. Süre oranı 1,284 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-linear-action-chain-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
