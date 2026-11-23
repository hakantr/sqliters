# V6b.493 — Hazır SUBSTR ve SUBSTRING metin sınırları

Üç kodlamada kip başına 12 C API profili, her kodlama ve kipte 726 aralık satırı ve Group65 30/30 eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-substr-text-register-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v493-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1362 geçti, 0 başarısız, 324 yok sayıldı. ON 1362 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,07 ms taban, 383,64 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-substr-text-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
