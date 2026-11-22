# V6b.407 — Özel adlı kolasyonun tam bellek sırası

Her Rust kipinde 180 dar bellek sıralama profili ve 12.708 callback/adım optimize C ile eş. İlk callback hatası özgün kodla döndü. Group64 7/7, altı DELETE/WAL imajı ve C yeniden açılışları korundu. [Bağımsız kanıt](/github/sqliters/target/perf-work/v407-custom-collation-memory-sort-preflight/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1287 geçti, 1 başarısız, 325 yok sayıldı. ON 1285 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,55 ms taban, 376,17 ms aday. Süre oranı 1,264 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/custom-collation-memory-sort-disk-verify/res/gates-validation.txt).
