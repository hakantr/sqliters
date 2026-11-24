# V6b.542 — İki adlandırılmış kolasyonla birlikte yerleşik sıralama anahtarı

Her kipte 1.296 tam sıralama, 1.296 LIMIT/OFFSET, 324 işçi izi tekrarı, 108 fiziksel PMA ve 576 hata kontrolü eş. Yerleşik ve uygulama kolasyonlarının konumları doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/mixed-collation-worker-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v542-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1414 geçti, 0 başarısız, 324 yok sayıldı. ON 1414 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 297,83 ms taban, 382,72 ms aday. Süre oranı 1,285 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-stock-named-collation-sort-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
