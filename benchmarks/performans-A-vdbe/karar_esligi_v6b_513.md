# V6b.513 — İlgisiz MAIN DDL sonrasında nitelikli TEMP prepared okuma

Yeni 24/24 hedef native kontrolü C ile eş; 240 geri kontrol önceki Rust ham iziyle birebir. Legacy bağlama ve reprepare 120/120, byte observer 116/116. [Bağımsız ölçüm](/github/sqliters/target/perf-work/temp-main-schema-reprepare-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v513-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1387 geçti, 0 başarısız, 324 yok sayıldı. ON 1387 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 303,11 ms taban, 390,62 ms aday. Süre oranı 1,289 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-prepared-main-schema-stability-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
