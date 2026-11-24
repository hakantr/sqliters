# V6b.512 — Legacy TEXT okumalarında her bağlama yuvasının kodlaması

120/120 native değer, adım ve reset karşılaştırması eş. Önceki native bağlama ve dosya sınır kontrolleri ile Group64 ve Group65 iki kipte geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/legacy-bind-encoding-v508-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v512-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1386 geçti, 0 başarısız, 324 yok sayıldı. ON 1386 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,99 ms taban, 391,93 ms aday. Süre oranı 1,289 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-bind-slot-encoding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
