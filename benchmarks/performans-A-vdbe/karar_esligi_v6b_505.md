# V6b.505 — Küçük FILE ve DEFAULT sıralamasında kayıtlı NOCASE ve RTRIM

Kip başına 96 C eş hedef, 384 taban kontrolü ve dokuz snapshot profili. Küçük MAIN görüntüsünde C ve Rust çağıran iş parçacığını kullanır. Program değişiminde ifadeye ait sayaçlar korunur. Group64 36/36, Group65 39/39. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v505-registered-file-sort-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v505-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1375 geçti, 0 başarısız, 324 yok sayıldı. ON 1375 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 301,13 ms taban, 389,43 ms aday. Süre oranı 1,293 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/registered-file-sort-no-spill-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
