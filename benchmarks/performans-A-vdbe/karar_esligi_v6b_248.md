# V6b.248 — Negatif ISO 24:00 korunmuş takvim

Kanonik negatif tarihli 24:00, C’nin geçerli ham takvim durumunu taşıdığı doğrulanmış geçişleri izliyor. Önceki 24 ve yeni 35 takvim vakası C ile eş. Diske yazılan önceki 24 vaka bayt eş ve 522 bağımsız C salt okunur açılışı doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v247-negative-hour24-retained-calendar-candidate-independent/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1181 geçti, 6 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 391/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 242/245, determinism 244/245. Açık L1, L4 ve L6 kayıtları doğrulandı. |
| Performans tabanı | 300,15 ms taban, 384,48 ms aday. Süre oranı 1,281 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/negative-hour24-retained-calendar-disk-verify/res/gates-validation.txt).
