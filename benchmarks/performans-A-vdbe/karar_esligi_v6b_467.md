# V6b.467 — Üç sıralama anahtarında işçi PMA

Tek ana rowid tablosu, üç yerel kodlama, FILE ve DEFAULT, son benzersiz rowid ile işçi PMA sonucu, reset, işçi birleştirme ve geçici dosya kapanışı C ile doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-three-key-v464-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v467-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1341 geçti, 0 başarısız, 325 yok sayıldı. ON 1341 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 332,10 ms taban, 429,65 ms aday. Süre oranı 1,294 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/three-key-worker-pma-disk-verify/res/gates-validation.txt).
