# V6b.73 — PRAGMA C dal kapsamı

| C kararı | Yerel profil |
|---|---|
| CHECK derleme kararı | DML flag ON dalı 198, CHECK üretimi 144, tablada CHECK yok 24. |
| CHECK integrity taraması | Flag ON/OFF 72/72, CHECK yok 288. |
| TEMP tanı satırı | DEBUG C 18 PRAGMA ve 54 satır. 6 açılmamış TEMP, 39 VFS LOCKSTATE başarısı ve 9 VFS başarısız sonucu. FILE spill yaşayan üç TEMP başarı değerinde 0. |

DML ve PRAGMA dallarının tam kaynak eşlemesi, eski ve yeni profil sayaçları ile ayrı C GCOV raporunda. UPDATE'de denetlenmeyen sütun kararının C tarafı ölçüldü. Legacy eşliği açık. UPSERT, ALTER yürütmesi, tahsis hataları ve bütün eski V1 error state çıkışları kapsamda ayrı. LOCK_STATUS profili NULL schema adı, Btree var ve Pager yok ile DEBUG dışı derleme yollarını almıyor. [CHECK gcov ham kapsamı](/github/sqliters/target/perf-work/check-pragma-disk-reproof/res/c-gcov-scope.txt) · [TEMP gcov](/github/sqliters/target/perf-work/temp-lock-status-proof/res/gcov/sqlite3.c.gcov).
