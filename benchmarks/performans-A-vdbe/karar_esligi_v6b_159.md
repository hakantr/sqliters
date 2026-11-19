# V6b.159 — WAL→OFF PERSIST_WAL kararı

Normal tek veya nitelikli disk hedefinde WAL→OFF geçişi, C gibi kapanış checkpoint'inin ardından VFS PERSIST_WAL bitini ve journal_size_limit değerini kullanır. OFF izni mevcut SQLite bağlantı kararıyla belirlenir. İlk ROW ve deyim bitişi, yan dosyalar, sonraki SQL yazıları ve son C yeniden açılışı ölçüldü. [Bağımsız C ve Rust kanıtı](/github/sqliters/target/perf-work/v155-wal-off-persist-independent/sonuc.md)

1.620 akışta 11.656 ana DB SHA ve 5.712 C açılışı eş. 864 doğrudan native akışta 576 ROW ve 288 fiziksel fark kapandı. Önceki 60 ROW farkı özgün fixture'larla tekrar ölçüldü. [45.296 strict kayıt](/github/sqliters/target/perf-work/v155-wal-off-persist-independent/res/proof.sha256) · [Birleşik kanıt](/github/sqliters/target/perf-work/wal-off-persist-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1178 geçti, 9 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 305,07 ms taban, 378,87 ms aday. Süre oranı 1,242 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-off-persist-disk-verify/res/gates-validation.txt).
