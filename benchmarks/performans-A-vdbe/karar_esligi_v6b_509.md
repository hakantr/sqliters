# V6b.509 — Yabancı anahtar action için TEXT karşılaştırması

DELETE ve UPDATE için CASCADE, SET NULL ve SET DEFAULT. 288 action profili ve 192 önceki profil C ile eş. Reddedilen yazmalarda dosya başlangıçla aynı. Tüm FK ve bütünlük kontrolleri temiz. [Bağımsız ölçüm](/github/sqliters/target/perf-work/fk-parent-action-text-affinity-v506-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v509-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1382 geçti, 0 başarısız, 324 yok sayıldı. ON 1382 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,49 ms taban, 389,75 ms aday. Süre oranı 1,284 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/foreign-key-action-text-comparison-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
