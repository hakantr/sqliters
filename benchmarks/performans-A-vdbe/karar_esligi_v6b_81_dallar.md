# V6b.81 — WITHOUT ROWID STORED C dal kapsamı

| C kararı | Ham plan gözlemi |
|---|---|
| STORED fiziksel PK ve ikincil | 215 ve 215. C önceki ikincil döngüyü tutuyor. |
| VIRTUAL kontrol | 214 PK ve 215 ikincil. C PK'yi alıyor. |
| Örtücü olmayan kontrol | 215 PK ve 223 ikincil. C PK'yi alıyor. |
| Geniş adayın sıra farkı | Son aday normal korpus akışında C kaynak sırasını ve son dosyayı koruyor. |

İstatistikli yürütme, karma hint ve dış sorgu eşlik kararı ayrı. [C WHERETRACE ve C/Rust ham satırlar](/github/sqliters/target/perf-work/without-rowid-scan-independent/sonuc.md) · [Dar release koşusu](/github/sqliters/target/perf-work/without-rowid-stored-scope/sonuc.md).
