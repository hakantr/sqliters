# V6b.491 — Hazır quote ve unistr_quote metin baytları

Kip başına 12 C V1/V2 API profili ve üç kodlama eş. Group65 28/28 ve printf C regresyonu geçti. Altı format profili eski geçerli davranışı doğruladı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-unistr-quote-register-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v491-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1360 geçti, 0 başarısız, 324 yok sayıldı. ON 1360 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,34 ms taban, 387,35 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-quote-text-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
