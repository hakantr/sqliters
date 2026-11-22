# V6b.362 — WITHOUT ROWID UPDATE yabancı anahtar eylem tanısı

Üst tablo UPDATE OR IGNORE ve UPDATE OR FAIL için hazırlama tanı önceliği C ile eşleşti. 42 profil ve 504 salt okunur kontrolde 80 fark kapandı. [Bağımsız kanıt](/github/sqliters/target/perf-work/v362-fk-action-diagnostic-candidate/sonuc.md)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1238 geçti, 1 başarısız, 329 yok sayıldı. ON 1235 geçti, 4 başarısız, 329 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 397/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | Finder245 stdout245 stderr245, iki motorun rc245. L0 244/245, determinism 244/245. Açık L6 kayıtları doğrulandı. |
| Performans tabanı | 301,79 ms taban, 386,36 ms aday. Süre oranı 1,280 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/fk-action-diagnostic-disk-verify/res/gates-validation.txt).
