# V6b.133 — WAL→MEMORY statement bitiş sınırı

Normal NamedDisk WAL çıkışında MEMORY kipi ilk ROW öncesi checkpoint ve WAL/SHM sonlandırmasını tamamlar. Ana DB sayfa 1 sürümü ROW sırasında (2,2) kalır ve DONE, reset veya finalize ile (1,1) olur. Sayfa 1 eski görüntüsü C gibi MEMORY journal üzerinden tutulur. [Native C ve Rust kanıtı](/github/sqliters/target/perf-work/v132-wal-memory-mode-independent/sonuc.md)

756 normal MEMORY ve 216 retained kip korumasında 16.848 ana DB SHA ve 6.318 C salt okunur açılışı eş. 432 erken ROW farkı ve 612 önceki disk DELETE geçiş journalı giderildi. [Ham fiziksel denetim](/github/sqliters/target/perf-work/v132-wal-memory-mode-independent/res/physical-audit.txt) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/wal-memory-mode-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 303,49 ms taban, 378,67 ms aday. Süre oranı 1,248 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-memory-mode-disk-verify/res/gates-validation.txt).
