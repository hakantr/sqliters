# V6b.531 — INTEGER WHERE ile adlandırılmış FILE ve DEFAULT sıralaması

Her kipte 2.304 profil, satırlar, SORT, reset ve gerçek işçiler eş. Koşu ataması eş ölçülen iki-PMA kayıtları ile geri kontroller geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/named-where-worker-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v531-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1403 geçti, 0 başarısız, 324 yok sayıldı. ON 1403 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 561,38 ms taban, 740,02 ms aday. Süre oranı 1,318 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-worker-where-sort-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
