# V6b.474 — VACUUM kilit doğrulamasından sonra sürüm alanı ve WAL sırası

Sıcak journal kararından sonra sürüm alanı WAL keşfinden önce okunuyor. 266, 3338 ve 522, gerçek peer WAL, retry ve 168 önceki dosya görüntüsü 22 hedef C sözleşmesiyle doğrulandı. [Bağımsız ölçüm](/github/sqliters/target/perf-work/v471-vacuum-unknown-lock-version-order-candidate/sonuc.md) [Bağımsız kanıt](/github/sqliters/target/perf-work/v474-like-corrected-inherited/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1345 geçti, 0 başarısız, 325 yok sayıldı. ON 1345 geçti, 0 başarısız, 325 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 297,77 ms taban, 383,26 ms aday. Süre oranı 1,287 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/vacuum-retry-version-before-wal-disk-verify/res/gates-validation.txt).
