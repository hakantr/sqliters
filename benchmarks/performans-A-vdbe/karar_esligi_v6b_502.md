# V6b.502 — user_version ve application_id sayısal değerleri

512/4096 sayfa, DELETE/PERSIST/WAL ve 17 sayı profili. 204/204 commit, rollback, C yeniden açılışı ve ana dosya baytları eş. FK boş ve integrity_check ok. Sayısal literal hazırlama ve reset testi her iki kipte geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/header-metadata-lifecycle-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v502-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1372 geçti, 0 başarısız, 324 yok sayıldı. ON 1372 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 556,91 ms taban, 717,32 ms aday. Süre oranı 1,288 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/application-header-numeric-values-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
