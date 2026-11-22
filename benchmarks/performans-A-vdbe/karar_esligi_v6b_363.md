# V6b.363 — Rowid görünüm UPDATE karma aralıkta tetikleyici sırası

240 profil ve 720 C yeniden açılışında 90 sıra veya dosya farkı kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v363-rowid-view-mixed-range-update-corrected-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1239 geçti, 1 başarısız, 329 yok sayıldı. ON 1236 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,18 ms taban, 386,23 ms aday. Süre oranı 1,278 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/rowid-view-mixed-range-update-disk-verify/res/gates-validation.txt).
