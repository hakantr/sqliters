# V6b.443 — OFF ve ON tek işçi PMA

Üç kodlama ve DELETE/WAL için zorlanmış koşulda 890.408 VFS kaydı eş. Gerçek işçilerde satır, adım, dosya ve hata sonucu eş. 18 okuyucu kilidi ve 180 olağan FK koşusu ile son imajlar doğrulandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v443-off-one-worker-pma-rebased-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1322 geçti, 1 başarısız, 325 yok sayıldı. ON 1320 geçti, 3 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,20 ms taban, 378,17 ms aday. Süre oranı 1,272 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/off-single-worker-pma-disk-verify/res/gates-validation.txt).
