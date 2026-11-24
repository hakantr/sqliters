# V6b.516 — TEMP DDL sonrasında tablosuz hazır ifadelerin geçersizleşmesi

24/24 hedef, 240 taban ham byte kontrolü eş. C uyum V1 adım, reset, finalize ve bağlantı tanısı 72/72. Rust public typed Schema sözleşmesinin iki kipte mevcut testleri geçti. [Bağımsız ölçüm](/github/sqliters/target/perf-work/temp-ddl-expiry-candidate/sonuc.md) [Aday doğrulaması](/github/sqliters/target/perf-work/v516-like-corrected-inherited/res/rebase.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1389 geçti, 0 başarısız, 324 yok sayıldı. ON 1389 geçti, 0 başarısız, 324 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Q09 rastgele ROWID değişimi C tekrarlarında da gözlendi. |
| Performans tabanı | 304,25 ms taban, 393,57 ms aday. Süre oranı 1,294 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/temp-ddl-prepared-expiry-disk-verify/res/gates-validation.txt) · [Finder Q09 eşlik kaydı](/github/sqliters/target/perf-work/continue-v110/finder-q09-parity-decision.md)
