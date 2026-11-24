# V6b.567 — Özgün collate8 testinin çalışma alanı kapısına dönüşü

Özgün Tcl testinin 23 alt testi sabit C ve iki Rust kipinde geçti. Çalışma alanı kapısında collate8 etkinleştirildi. [Bağımsız ölçüm](/github/sqliters/target/perf-work/collate8-enable-v567.oW79qbah/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v567-with-v548-correction-proof/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1441 geçti, 0 başarısız, 318 yok sayıldı. ON 1441 geçti, 0 başarısız, 318 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 300,96 ms taban, 390,68 ms aday. Süre oranı 1,298 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/upstream-collate8-default-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
