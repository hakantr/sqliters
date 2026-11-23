# V6b.483 — Hazır V1 olağan yazmada otomatik reset tanısı

Otomatik reset önceki gerçek kısıt mesajını koruyor. Her kipte 42 C profili ve 21 önceki tanı farkı, sayaç, satır, işlem ve beş Group65 testi doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/plain-write-autoreset-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v483-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1352 geçti, 0 başarısız, 325 yok sayıldı. ON 1352 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,50 ms taban, 383,40 ms aday. Süre oranı 1,267 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/legacy-write-autoreset-diagnostic-disk-verify/res/gates-validation.txt).
