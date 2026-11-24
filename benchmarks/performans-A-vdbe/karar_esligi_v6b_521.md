# V6b.521 — Uygulamanın kendi adıyla kaydettiği sıralamada gerçek işçi

Her kipte 384/384 C satır, SORT, reset ve callback. İki PMA için 120/120, stock 576/576, önceki override 528/528 ve Rust fallible sözleşmesi 72/72 eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/named-collation-worker-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v521-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1393 geçti, 0 başarısız, 324 yok sayıldı. ON 1393 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,10 ms taban, 387,03 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-registered-collation-worker-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
