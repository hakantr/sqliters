# V6b.499 — Hazır MAIN rowid okumalarında metin baytları

UTF-8 OFF dosya farkı kapandı. Üç kodlama ile 12 yeniden açılış ve kip başına 12 V1/V2 izi C ile eş. İlk şema yüklemesi, reset/rebind ve FK korundu. Birleşik Group65 36/36, LIMIT 1/1. [Bağımsız ölçüm](/github/sqliters/target/perf-work/table-text-register-read-exact-verify/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v499-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1369 geçti, 0 başarısız, 324 yok sayıldı. ON 1369 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,46 ms taban, 385,68 ms aday. Süre oranı 1,279 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/main-rowid-text-register-read-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
