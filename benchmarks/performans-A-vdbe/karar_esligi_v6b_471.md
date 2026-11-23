# V6b.471 — İşçi PMA ile 2.000 sıralama anahtarına kadar

Son biricik rowid ile her kipte 144 geniş profil ve 27.792 C API adımı eşleşti. 72 ilk satır reseti, gerçek işçilerin join edilmesi, PMA kapanışı ve altı dosyada FK, bütünlük ve SHA doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-wide-key-v469-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v471-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1344 geçti, 0 başarısız, 325 yok sayıldı. ON 1344 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 300,21 ms taban, 384,73 ms aday. Süre oranı 1,282 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wide-key-worker-pma-disk-verify/res/gates-validation.txt).
