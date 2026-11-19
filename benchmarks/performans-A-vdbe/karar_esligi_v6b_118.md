# V6b.118 — PERSIST ve TRUNCATE journalından WAL ilk ROW

Tek hedefli PERSIST/TRUNCATE→WAL geçişi, önceki fiziksel journal kipini ve inode'u ilk ROW'a kadar korur. PERSIST'in eski kuyruk kayıtları ve TRUNCATE'in ilk sayfa kaydı C ile eş. Normal DONE, reset ve finalize mevcut statement sahibiyle WAL geçiş journalını kaldırıyor. [C kaynak ve journal yaşamı](/github/sqliters/target/perf-work/retained-to-wal-step-independent/sonuc.md)

Birleşik exact aday 288 retained ve 288 DELETE↔WAL akışında C ile eş. 7.200 ana SHA ve 1.800 C salt okunur açılışı doğrulandı. Birleşik optimize DML OFF 390/400 ve ON 400/400, önceki ham özetle eş. [Exact bağımsız kanıt](/github/sqliters/target/perf-work/v118-retained-journal-step-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/retained-to-wal-step-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 298,36 ms taban, 375,37 ms aday. Süre oranı 1,258 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/retained-to-wal-step-disk-verify/res/gates-validation.txt).
