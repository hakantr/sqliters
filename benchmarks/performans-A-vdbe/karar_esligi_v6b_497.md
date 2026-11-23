# V6b.497 — Kayıtlı NOCASE ve RTRIM ile hazır LIMIT/OFFSET sıralaması

Kip başına 558 C API, 44.874 exact karşılaştırıcı çağrısı ve 54 mevcut kontrol. Group64 34/34 ve 1.674 sağlıklı koşu. Birleşik kaynak odak doğrulaması geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/registered-nocase-rtrim-limit-v496-final-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v497-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1367 geçti, 0 başarısız, 324 yok sayıldı. ON 1367 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,35 ms taban, 386,53 ms aday. Süre oranı 1,283 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-nocase-rtrim-limit-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
