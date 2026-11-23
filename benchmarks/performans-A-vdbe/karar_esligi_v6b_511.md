# V6b.511 — TEXT WITHOUT ROWID parent ve numeric child yabancı anahtar sayacı

Üç encoding, DELETE/WAL, iki sayfa boyutu, tek/bileşik anahtar ve altı UPDATE/DELETE action karşılaştırmasında 2.304/2.304 SQL, C yeniden açılışı, imaj ve bütünlük denetimi eş. Group65 47/47 iki kipte. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-text-parent-counter-v509-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v511-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1385 geçti, 0 başarısız, 324 yok sayıldı. ON 1385 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 299,94 ms taban, 384,60 ms aday. Süre oranı 1,282 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-numeric-child-counter-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
