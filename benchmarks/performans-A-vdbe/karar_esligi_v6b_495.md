# V6b.495 — Hazır REPLACE baytları ve sonuç sınırı

Üç kodlama ve kip başına 12 C API profili, 210 kontrol ve Group65 31/31 eş. Kalıcı yazma 0/12 tabandan 2/12 bayt eş; 18 C açılışında FK ve bütünlük temiz. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-replace-text-register-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v495-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1363 geçti, 0 başarısız, 324 yok sayıldı. ON 1363 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 558,62 ms taban, 727,98 ms aday. Süre oranı 1,303 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-replace-text-register-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
