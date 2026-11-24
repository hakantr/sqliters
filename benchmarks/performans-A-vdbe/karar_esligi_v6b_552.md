# V6b.552 — ORDER BY indeksli JOIN Tcl denetimi

Test tanılama özelliği altında varsayılan, kapalı ve geri yüklenen plan ile altı sonuç satırı C ile eş. Aynı dosyadaki ayrı bağlantı varsayılan planını koruyor. Normal CLI derleme denetimi geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/orderby1-test-control-v547.065ms6bv/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v552-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1426 geçti, 0 başarısız, 320 yok sayıldı. ON 1426 geçti, 0 başarısız, 320 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,40 ms taban, 388,81 ms aday. Süre oranı 1,294 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/orderby-index-join-tcl-control-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
