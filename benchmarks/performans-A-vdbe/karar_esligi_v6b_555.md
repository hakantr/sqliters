# V6b.555 — orderby1 Tcl kontrolünün etkinleştirilmesi

Birleşik kaynakta orderby1 iki kipte önce seçili, ardından varsayılan çalışmada geçti. Beklenen bir ek başarılı ve bir eksik ignore kaydı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/orderby1-test-enable-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v555-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1429 geçti, 0 başarısız, 319 yok sayıldı. ON 1429 geçti, 0 başarısız, 319 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 298,70 ms taban, 388,81 ms aday. Süre oranı 1,302 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/orderby1-tcl-coverage-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
