# V6b.498 — Hazır CONCAT ve CONCAT_WS metin baytları

Üç kodlama ve iki kipte 12 C V1/V2 API profili, kodlama başına 96 normal kontrol ve hazırlama/reset regresyonu eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-concat-text-register-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v498-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1368 geçti, 0 başarısız, 324 yok sayıldı. ON 1368 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,94 ms taban, 391,60 ms aday. Süre oranı 1,288 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-concat-text-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
