# V6b.479 — Aktif WAL okuyucusunda mantıksal sayfa sayısı

Geçerli Page 1 sayısı ile WAL commit extent ayrıldı. 45 çapraz açılışta sekiz eski fark kapandı. Mevcut extent olağan UPDATE ve büyümede korunuyor, gerçek VACUUM daralıyor. C ile 36 ham görüntü ve 24 writable reopen eş. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v477-active-wal-logical-page-count-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v479-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1349 geçti, 0 başarısız, 325 yok sayıldı. ON 1349 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 302,17 ms taban, 385,56 ms aday. Süre oranı 1,276 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/active-wal-logical-page-count-disk-verify/res/gates-validation.txt).
