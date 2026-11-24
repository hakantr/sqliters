# V6b.547 — Uygulamanın NOCASE karşılaştırıcısında hazırlık ve sıralama

Kip başına 1.728 tam sıralama, 1.728 top-N, 486 seri sıra, 54 fiziksel PMA ve 324 hata sonrası temizleme eş. V1 reset/finalize tanısı ve V2 yeni callback kullanımı C ile eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/nocase-override-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v547-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1419 geçti, 0 başarısız, 323 yok sayıldı. ON 1419 geçti, 0 başarısız, 323 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,73 ms taban, 388,92 ms aday. Süre oranı 1,293 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/nocase-application-sort-replacement-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
