# V6b.529 — İki doğrudan FK alt tablosundan birinin devam ettiği action ağacı

Birleşik kaynakta 31.104/31.104 C SQL, iki yönlü açılış, imaj ve bütünlük. Group64 40/40, Group65 61/61 ve ertelenmiş FK 2/2 iki kipte. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v529-fk-branch-chain-exact/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v529-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1401 geçti, 0 başarısız, 324 yok sayıldı. ON 1401 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,50 ms taban, 389,78 ms aday. Süre oranı 1,284 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-single-continuing-branch-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
