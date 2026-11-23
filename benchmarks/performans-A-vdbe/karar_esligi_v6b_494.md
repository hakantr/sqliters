# V6b.494 — Seri DEFAULT sıralaması ve aynı ifadeyle kip değişimi

Kip başına 252 DEFAULT ve 144 aynı handle kip geçişi C ile eş. 228 guard, Group64 33/33, Unicode 5/5, 12 fiziksel ve 18 FK kontrolü geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/registered-nocase-rtrim-default-v493-rebased-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v494-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1362 geçti, 0 başarısız, 324 yok sayıldı. ON 1362 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,03 ms taban, 385,37 ms aday. Süre oranı 1,289 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-nocase-rtrim-default-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
