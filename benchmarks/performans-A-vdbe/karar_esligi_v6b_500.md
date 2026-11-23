# V6b.500 — Sade UPDATE eşitliğinde kayıtlı NOCASE ve RTRIM

Dört COLLATE yerleşimi, iki journal ve üç kodlamada ilk adım, reset, callback argüman sırası ve dosyalar C ile eş. Geniş kanıt 1.536 karşılaştırma. Birleşik V500 kaynağında 192 karşılaştırma, 288 sağlıklı C açılışı ve Group65 her kipte 36/36. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v500-update-collation-rebase/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v500-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1370 geçti, 0 başarısız, 324 yok sayıldı. ON 1370 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,75 ms taban, 387,72 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-update-where-collation-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
