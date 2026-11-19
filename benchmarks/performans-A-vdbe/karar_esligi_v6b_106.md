# V6b.106 — Nitelemesiz WAL checkpoint ve bağlı dosyalar

C'de nitelemesiz `PRAGMA wal_checkpoint` bütün açık şemaları slot sırasıyla işler. Sonuç `busy` gözlemlerini birleştirir ve sayaçları MAIN'den alır. Rust aynı slot ve pager yürütücüsünü bileşik PRAGMA'da kullanıyor. İki kipte 48 fiziksel bağlı-WAL ayrışması kapandı. [C sözleşmesi ve odak kanıtı](/github/sqliters/target/perf-work/wal-attached-checkpoint-fix-independent/sonuc.md)

Birleşik exact aday 756/756 normal ATTACH ve 72/72 iki-WAL snapshot akışında C ile eş. 8.280 ham ana DB SHA-256 ve 3.456 C salt okunur açılışı doğrulandı. V104 adayı ayrıca 180 tek-WAL okur koruması geçti. Birleşik optimize DML OFF 390/400, ON 400/400 ve önceki ham özetlerle eş. [Exact fiziksel doğrulama](/github/sqliters/target/perf-work/v106-attached-checkpoint-independent/sonuc.md) · [Birleşik korpus](/github/sqliters/target/perf-work/wal-attached-checkpoint-final-release-proof/res/proof.sha256)

## Dört doğrulama kapısı

| Kapı | Sonuç |
|---|---|
| Tam çalışma alanı | OFF 1186 geçti, 1 başarısız, 337 yok sayıldı. ON 1177 geçti, 10 başarısız, 337 yok sayıldı. Yeni başarısızlık yok. |
| DML | OFF 390/400, ON 400/400. Açık vakaların ham gözlemleri öncekiyle aynı. |
| Ayrışma bulucu | 245 vaka. Stdout 245/245, stderr 244/245. Ham özet önceki aşamayla aynı. |
| Performans tabanı | 302,83 ms taban, 381,71 ms aday. Süre oranı 1,260 ve 14 digest eşit. |

Ek kurtarma 147/147, dosya imajı 12/12 ve gerçek C çöküşü 4/4. [Son kapı doğrulaması](/github/sqliters/target/perf-work/wal-attached-checkpoint-disk-verify/res/gates-validation.txt).
