# V6b.532 — Niteliksiz INNER JOIN kaynaklarının hazırlanma anındaki tablosu

204 hedef fark kapandı. 348/348 C çağrı sözleşmesi, 174/174 Tcl V1 ve 3.132 salt okunur C açılışı geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/unqualified-join-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v532-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1404 geçti, 0 başarısız, 324 yok sayıldı. ON 1404 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 304,27 ms taban, 394,15 ms aday. Süre oranı 1,295 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/unqualified-join-prepared-binding-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
