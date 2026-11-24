# V6b.534 — INTEGER WHERE ile LIMIT/OFFSET geçici top-N indeksi

Her kipte 2.808 yeni ve 972 önceki LIMIT ham izi C eş. SORT, seri callback sırası, reset ve NULL sınırları doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/where-topn-v510-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v534-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1406 geçti, 0 başarısız, 324 yok sayıldı. ON 1406 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,61 ms taban, 393,43 ms aday. Süre oranı 1,296 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/named-where-topn-sort-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
