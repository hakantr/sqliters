# V6b.182 — Yazılabilir journal_mode setter hazırlığı

Soğuk yazılabilir ana NamedDisk bağlantısı, `PRAGMA [main.]journal_mode=...` ve EXPLAIN hazırlığında C gibi mevcut şema yolunu kullanır. İlk ROW öncesinde DB, WAL veya rollback journal veri yazısı oluşmaz. V1, V2, ilk prepare ve reprepare aynı koşulu izler. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v180-readwrite-setter-prepare-independent/sonuc.md)

546 profil ve 1.638 akışta 940 soğuk C–Rust fiziksel hazırlık çifti ve 892 pasif VFS sırası eş. 1.872 C yeniden açılışı, 48 doğal EXCLUSIVE hazırlık çekişmesi ve 24 canlı WAL–takip deyimi profili doğrulandı. [Strict manifest](/github/sqliters/target/perf-work/v180-readwrite-setter-prepare-independent/res/proof.sha256) · [Birleşik kaynak ve DML](/github/sqliters/target/perf-work/readwrite-journal-setter-prepare-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 461,16 ms taban, 614,06 ms aday. Süre oranı 1,332 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/readwrite-journal-setter-prepare-disk-verify/res/gates-validation.txt).
