# V6b.530 — Karışık referential action ayarlarında önceden hazırlanmış onarım

864/864 C işlemi, hata, iki yön dosya açılışı, byte ve FK bütünlüğü eş. Group64 40/40, Group65 60/60 iki kipte. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-mixed-action-repairs-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v530-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1402 geçti, 0 başarısız, 324 yok sayıldı. ON 1402 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 302,73 ms taban, 390,62 ms aday. Süre oranı 1,290 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-mixed-action-repair-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
