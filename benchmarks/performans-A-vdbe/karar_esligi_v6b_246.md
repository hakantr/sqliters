# V6b.246 — Negatif ISO 24:00 süre dönüşü

Canonical negatif tarihli 24:00 süre değiştiricileri için C sayı sınırı ve yuvarlama, geçici Julian taşması ve son değer denetimi korunur. 20 ölçülen süre zinciri, üç ilk ve üç ara dönüş C eş. 42 ek native aritmetik, 450 C açılışı, 12 VDBE ve 6.305 strict doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v245-negative-dated-hour24-elapsed-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 303,45 ms taban, 383,69 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/negative-hour24-elapsed-disk-verify/res/gates-validation.txt).
