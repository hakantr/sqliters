# V6b.63 — D167 C dal kapsamı

Sentetik kimlik yanıtı, sağlıklı sentetik yanıt, olağan kapanış, VFS kalıcılık override ve eski adapter referansı için beş `gcov` profili toplandı. Toplam 55 C kapanışı `databaseIsUnmoved` yolundan geçti. Birden fazla kısa devre koşulu olan satırlarda sayaçlar `gcov` sırasındadır.

| C satırı | Karar | Dal sayaçları |
|---|---|---|
| `pager.c:4204` | `SQLITE_NOTFOUND` | 9, 46 |
| `pager.c:4209` | `SQLITE_OK && bHasMoved` | 37, 9, 9, 28 |
| `pager.c:4244` | Kimlik sonucu checkpoint'e izin verdi | 37, 18 |
| `wal.c:2610` | Checkpoint tamponu var | 37, 18 |
| `wal.c:2611` | EXCLUSIVE başarılı veya BUSY | 28, 9 |
| `wal.c:2624` | WAL silme veya tutma | 11, 17 |
| `wal.c:2629` | Tutulan WAL'ı sıfırlama veya boyutunu koruma | 8, 9 |
| `wal.c:2643` | WAL xDelete kararı | 11, 44 |

| Alınmayan C çıkışı | Sınıflandırma |
|---|---|
| `os_unix.c:1628` inode yok, stat başarısız veya inode farklı | Unix VFS kaynak incelemesiyle sınırlı. |
| `pager.c:4200–4201` geçici ana dosya veya boş DB | Olağan disk WAL ölçümü dışında. |
| `pager.c:4243` bağlantı yok veya son checkpoint devre dışı | Ayrı kapanış koordinasyonu gerekiyor. |
| `wal.c:1652–1659` heap WAL index veya güvenilmez SHM | Farklı WAL index kipleri. |
| `wal.c:2597, 2613` WAL nesnesi yok veya kip normal değil | Olağan disk WAL ölçümü dışında. |
| `wal.c:2619` EXCLUSIVE sonrasında checkpoint hatası | Açık G/Ç hata kapsamı. |

Ham sayaçlar `/home/hakan/github/sqliters/target/perf-work/d167-independent-proof/res/gcov_report.json` içinde.
