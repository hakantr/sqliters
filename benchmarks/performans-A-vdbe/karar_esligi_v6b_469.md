# V6b.469 — Dört ila sekiz sıralama anahtarıyla işçi PMA

Son biricik rowid ile iki kipte 144 C profili ve 110.736 API adımı eş. Her kipte 72 ilk satır resetinde PMA dosyalarının kapanması, bütün işçilerin join edilmesi ve altı ana dosyada FK, bütünlük ve SHA doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/worker-four-to-eight-key-v467-preflight/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v469-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1342 geçti, 0 başarısız, 325 yok sayıldı. ON 1342 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 549,24 ms taban, 710,45 ms aday. Süre oranı 1,294 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/four-to-eight-key-worker-pma-disk-verify/res/gates-validation.txt).
