# V6b.520 — Nitelikli MAIN ve AUX hazır sorgularında ilgili şema

96/96 hedef yaşam döngüsü ve 264 taban ham kontrolü eş. 264 önceki TEMP, 120 legacy ve 116 byte observer kontrolleri korundu. [Bağımsız ölçüm](/github/sqliters/target/perf-work/qualified-main-aux-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v520-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1392 geçti, 0 başarısız, 324 yok sayıldı. ON 1392 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 561,31 ms taban, 733,45 ms aday. Süre oranı 1,307 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/qualified-main-aux-prepared-stability-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
