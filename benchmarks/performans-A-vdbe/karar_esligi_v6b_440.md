# V6b.440 — Kaldırılan fonksiyon aritesi ve yeniden kayıt

Üç kodlama, V1/V2 ve OFF/ON üzerinde 36 akış ve 468 API kaydı eş. 24 önceki fark kapandı. Kaldırılan arite maskesi ve diğer ariteler, gömülü liste satırı, yeniden kayıt, reset ve Rust bool dönüşü doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v440-like-mask-corrected-candidate/sonuc.md)

İki argümanlı LIKE kaldırılırken C ile aynı özgün SQL tanısı ve ofset geliyor. ESCAPE aritesi çalışıyor. case_sensitive_like PRAGMA’sı kaldırılan ariteyi yeniden kaydediyor. [Yerel C kaydı](/github/sqliters/target/perf-work/v440-like-mask-corrected-candidate/res/infix-c.txt)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1319 geçti, 1 başarısız, 325 yok sayıldı. ON 1317 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 299,72 ms taban, 382,48 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/removed-function-arity-mask-disk-verify/res/gates-validation.txt).
