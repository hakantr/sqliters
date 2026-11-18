# V6b.66 — Kapanış ve boş WAL-index için C dal kapsamı

## Örtük bağlantı kapanışı

28 olağan guard ve dört bekleyen statement/BUSY kapatma vakası aynı C 3.54.1 kaynak objesi ile ölçüldü.

| C yeri | Karar | Dal sayaçları |
|---|---|---|
| `main.c:1404` | Bağlantı zombie ve açığı yoksa son kapanış | 38, 22, 2, 36 |
| `main.c:1534` | Btree bağlı veya boş | 40, 40 |
| `main.c:1535` | Btree transaction'ı yazma | 16, 24 |
| `main.c:1556` | Hook var ve rollback koşulu doğru | 32, 8, 20, 12, 12, 8 |

Bu satırların 14/14 çıkışı alındı. Btree şema değişimi ve sanal tablo rollback yaşam döngüsü bu normal profilin dışında. Ham sayaç `/home/hakan/github/sqliters/target/perf-work/connection-close-proof/close-gcov/sqlite3.c.gcov`.

## Boş WAL

Altı okuyucu ve üç yazıcı toparlama geçişinde `wal.c:1446–1453, 1604–1608, 974–986` boş WAL kararından başlık yayımına akış aldı. `nSize>WAL_HDRSIZE` çıkışı ve `sqlite3OsFileSize` hata yolu bu profilde alınmadı. Kapsam boş, normal dosyanın ilk okunmasıyla sınırlı. Üç gerçek sayfa boyutunda sonraki COMMIT ve checkpoint ayrı ölçüldü. [Okuyucu profili](/home/hakan/github/sqliters/target/perf-work/empty-wal-publish-proof/res/candidate/reader.gcov-scope.txt) · [Yazıcı profili](/home/hakan/github/sqliters/target/perf-work/empty-wal-publish-proof/res/candidate/writer.gcov-scope.txt).
