# V6b.501 — Hazırlanmış TRIM, LTRIM, RTRIM, LOWER ve UPPER

Üç kodlama, iki journal ve V1/V2 ile kip başına 12 API profili C ile eş. Kip başına 120 olağan ve 12 odaklı sonuç eş. Group65 her kipte 37/37. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-trim-case-text-register-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v501-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1371 geçti, 0 başarısız, 324 yok sayıldı. ON 1371 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,85 ms taban, 392,07 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-trim-case-text-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
