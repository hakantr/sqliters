# V6b.68 — WAL restart C dal kapsamı

| C satırı | Ölçülen karar |
|---|---|
| `wal.c:1353` | Her C profilinde 7 ilk frame ve 12896 devam frame'i. |
| `wal.c:2246` | TRUNCATE ve doğal writer profillerinde dörder restart, readmark döngüsü 12 devam ve 4 son. |
| `wal.c:2450` | TRUNCATE kilit denetimi 4 izin, 2 BUSY. |
| `wal.c:2451` | 4 başarılı TRUNCATE, 0 yalnız RESTART. |
| `wal.c:3972` | Doğal writer restart kilit denetimi 4 izin, 2 BUSY. |

`wal.c:1365` önceki tamamlanmamış yazıcıdan kalan slot temizliği alınmadı. `wal.c:2451` yalnız RESTART ve `wal.c:3984` BUSY dışı G/Ç hatası alınmadı. İki snapshot ve indeks bloğuna erişim sınırı, ham WAL/SHM karşılaştırmalarıyla beraber doğrulandı. [TRUNCATE sayaçları](/home/hakan/github/sqliters/target/perf-work/wal-restart-slots-proof/res/truncate.gcov-scope.txt) · [Writer restart sayaçları](/home/hakan/github/sqliters/target/perf-work/wal-restart-slots-proof/res/writer-restart.gcov-scope.txt).

İlk fixture/reader koşuları yanlış gcov çıktı konumu yüzünden önceki `v6b2/cov/sqlite3.gcda` ham toplamına sayaç ekledi. Güvenilir önceki kopya bulunmadı. Kaydedilmiş eski metin raporu korundu. Yukarıdaki sayılar bütün C rolleri için ayrı dizin kullanan yeni koşudan. Yeniden koşular arasında eski ham dosyanın SHA-256 ve değiştirilme zamanı aynı doğrulandı. [Yerel arşiv incelemesi](/home/hakan/github/sqliters/target/perf-work/wal-restart-slots-proof/res/archive-audit.txt).
