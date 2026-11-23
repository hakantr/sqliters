# V6b.488 — Yerleşik NOCASE ve RTRIM ile toplam rowid sıralamasında işçi PMA

Kip başına 180 C API profili, 673.332 kontrollü VFS olayı, 72 gerçek işçi profili. Group64 32/32, 18 FK ve bütünlük kontrolü geçti. Uygulama kolasyonu guard kayıtları korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-stock-collation-rowid-v486-preflight/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v488-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1357 geçti, 0 başarısız, 324 yok sayıldı. ON 1357 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,49 ms taban, 383,84 ms aday. Süre oranı 1,265 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/worker-stock-collation-rowid-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
