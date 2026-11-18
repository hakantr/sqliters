# V6b.67 — BLOB boolean dönüşümünde C dal kapsamı

C gcov çalışması 2640 ifadenin ve 12 DB dosyasının native referans sonucunu tekrar üretti. Çıkışların 32/38'i alındı. `sqlite3VdbeBooleanValue` içindeki dört çıkışın tamamı ölçüldü.

| C aralığı | Alınan çıkış | Bu profilde alınmayan çıkış |
|---|---|---|
| `vdbemem.c:674–718` | 16/18 | UTF-8 ve UTF-16 yavaş yolundaki iki bellek tahsisi hatası. |
| `vdbemem.c:733–750` | 8/10 | NULL veri işaretçisi ve yerinde terminator eklemenin başarılı olduğu yol. |
| `vdbemem.c:770–796` | 8/10 | BOOLEAN INTEGER'ı erken ele alıyor. RealValue içindeki INT/INTREAL ve en sondaki diğer türlerde sıfır çıkışı bu profilde alınmadı. |

Ham `gcov` satırları `/home/hakan/github/sqliters/target/perf-work/numeric-blob-boolean-proof/candidate/res/c-boolean-coverage.txt`.
