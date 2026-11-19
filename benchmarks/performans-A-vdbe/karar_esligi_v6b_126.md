# V6b.126 — Karma rollback kipli disklerde WAL çıkışı

Nitelemesiz DELETE, PERSIST ve TRUNCATE hedeflerinde, yalnız WAL sınırını geçen diskler statement'ın ortak bitişine katılıyor. Hedef dışı rollback kipinde başlayan diskler mevcut journal dönüşümünü kullanıyor. İlk ROW'da WAL katılımcıları aktif rollback journalını ve eski dosya başlığını koruyor. [C ve Rust native kanıtı](/github/sqliters/target/perf-work/v125-mixed-rollback-wal-exit-independent/sonuc.md)

216 yeni ve 396 koruma akışı C ile eş. 144 erken ROW farkı kapandı. 16.128 ana dosya SHA karşılaştırması ve 6.048 salt okunur C açılışı eş. Birleşik DML OFF 390/400 ve ON 400/400. [Tam kaynak ve DML manifesti](/github/sqliters/target/perf-work/mixed-rollback-wal-exit-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 299,48 ms taban, 374,12 ms aday. Süre oranı 1,249 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/mixed-rollback-wal-exit-disk-verify/res/gates-validation.txt).
