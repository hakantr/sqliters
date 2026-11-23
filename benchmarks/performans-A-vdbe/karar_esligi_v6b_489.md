# V6b.489 — Hazır metin uzunluğu ve Unicode kod noktası

Üç kodlama ve iki kipte 6 önceki fark giderildi. Her kipte 12 C hazır ifade izi, 6 veritabanındaki sütun kontrolleri ve Group65 27/27 eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/builtin-unistr-text-observers-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v489-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1358 geçti, 0 başarısız, 324 yok sayıldı. ON 1358 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 554,17 ms taban, 705,64 ms aday. Süre oranı 1,273 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/prepared-unistr-text-observers-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
