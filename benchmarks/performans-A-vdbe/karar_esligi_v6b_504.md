# V6b.504 — Metin uzunluğu sınırını yürütmede uygulama

Üç kodlamada CLI çıktısı ve hata kodu, DELETE/WAL'da kip başına altı hazır V2 ilk adım/reset izi C ile eş. Bağlantı sınırı VDBE denetimlerine ve TRIM/LOWER/UPPER fonksiyonlarına yansıyor. [Bağımsız ölçüm](/github/sqliters/target/perf-work/text-length-limit-v501-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v504-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1374 geçti, 0 başarısız, 324 yok sayıldı. ON 1374 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 306,30 ms taban, 389,52 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/text-length-limit-runtime-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
