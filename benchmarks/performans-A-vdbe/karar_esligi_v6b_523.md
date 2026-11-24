# V6b.523 — Niteliksiz MAIN ve AUX hazır okumalarında başlangıç tablosu

192 hedef bağlama farkı kapandı. 336/336 C çağrı davranışı ve 168/168 Tcl V1. Gerçek AUX DDL, TEMP gölgelemesi ve DETACH sonrasında yeniden bağlanma C ile eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/unqualified-main-aux-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v523-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1395 geçti, 0 başarısız, 324 yok sayıldı. ON 1395 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,61 ms taban, 392,42 ms aday. Süre oranı 1,292 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/unqualified-main-aux-prepared-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
