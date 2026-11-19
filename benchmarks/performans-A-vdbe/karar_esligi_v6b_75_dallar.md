# V6b.75 — C bind ve source-id dal kapsamı

| C kapsamı | Sonuç |
|---|---|
| `vdbeUnbind` ve `bindText` | 18 TEXT/BLOB bind. Önceki integer değeri her girişten önce atanıyor. İki yazma sınıfında alt, tam ve üst sınır. |
| `sqlite3VdbeMemSetStr` ve `sqlite3VdbeMemSetText` | Giriş ölçüsünün sınırı aşması TEXT 3, BLOB 3. Sonraki NULL disk kaydı üç kodlamada eş. |
| `sourceidFunc` | 24 callback, tek metin dönüşü. |

C profil dosyaları ayrı dizinlerde tutuldu. D50 hedefi UTF-8 bind girişini ve SQL veri türleri ile bağlantı kodlamasını içeriyor. Destructor, bellek tahsis hatası, dinamik limit değiştirme ve doğrudan C UTF-16 bind API kapsamı açık. [D50 gcov](/github/sqliters/target/perf-work/bind-size-proof/res/c-gcov/decisions.txt) · [Source-id gcov](/github/sqliters/target/perf-work/sql-source-id-proof/res/native/gcov/source-id-decisions.txt).
