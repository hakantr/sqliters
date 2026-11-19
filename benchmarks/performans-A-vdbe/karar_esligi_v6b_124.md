# V6b.124 — Tek hedefli WAL→PERSIST/TRUNCATE

Tek veya şema nitelikli sağlıklı WAL çıkışı, checkpoint verisini yazıp ilk ROW'da eski başlığı ve etkin retained journalı koruyor. Normal DONE, reset veya finalize PERSIST başlığını sıfırlıyor veya TRUNCATE journalı sıfır bayta indiriyor. İki yol da aynı journal inode'unu koruyor. [Bağımsız C kanıtı](/github/sqliters/target/perf-work/v123-wal-to-retained-step-independent/sonuc.md)

288 yeni akış ve 468 önceki kontrol C ile eş. 192 erken ROW farkı kapandı. 7.776 ana DB SHA karşılaştırması, 1.944 salt okunur C açılışı ve 576 fiziksel iz doğrulandı. Birleşik DML OFF 390/400 ve ON 400/400. [Ham denetim](/github/sqliters/target/perf-work/v123-wal-to-retained-step-independent/res/physical-audit.txt) · [Birleşik korpus](/github/sqliters/target/perf-work/wal-to-retained-step-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 300,01 ms taban, 378,89 ms aday. Süre oranı 1,263 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-to-retained-step-disk-verify/res/gates-validation.txt).
