# V6b.507 — TEXT ve BLOB bind uzunluğu ve yürütmede Variable sınırı

Üç kodlama ve DELETE/WAL ile 24 V1/V2 native rota, 12 TEXT/BLOB bağlama ve 24 sağlıklı C açılışı eş. Nokta okumada ve kısa DML’de limit korunur. V1 step/reset 1/18, V2 18/18. Group65 her kipte 44/44. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v507-bind-length-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v507-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1380 geçti, 0 başarısız, 324 yok sayıldı. ON 1380 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 305,68 ms taban, 390,56 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/bind-length-and-variable-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
