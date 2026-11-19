# V6b.131 — TEMP FILE TRUNCATE kapanış senkronizasyonu

Fiziksel TRUNCATE journalı sıfır bayta indirdikten sonra senkronizasyon kararı mevcut pager noSync kuralına uyar. FILE TEMP işlemlerinde C gibi truncate korunur ve post-truncate sync kaldırılır. Normal COMMIT ve ROLLBACK gözlendi. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v130-file-temp-truncate-independent/sonuc.md)

144 aday akışında Rust'taki sekiz ek FILE TEMP fsync giderildi. 108 TEMP DB SHA, 720 kalıcı disk SHA, 144 C salt okunur açılışı ve 48 retained journal eşliğinde C açılışı doğrulandı. 16 izole C gcov profili kaynak kararlarını ölçtü. [Bağımsız manifest](/github/sqliters/target/perf-work/v130-file-temp-truncate-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/file-temp-truncate-sync-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 304,74 ms taban, 383,74 ms aday. Süre oranı 1,259 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/file-temp-truncate-sync-disk-verify/res/gates-validation.txt).
