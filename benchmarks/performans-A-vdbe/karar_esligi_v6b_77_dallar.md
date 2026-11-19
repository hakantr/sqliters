# V6b.77 — C hazırlama tanısı dal kapsamı

| C yolu | Kanıt |
|---|---|
| `sqlite3IsReadOnly` | 1.590 çağrı, 210 view yazılabilirlik reddi. |
| `sqlite3Insert` | 1.086 çağrı, 36 eksik hedef sütun, 48 geç view UPSERT reddi. |
| Fonksiyon resolve ve reprepare | Native 468 V1/V2 skaler, 234 view ve 6 view V2 şema değişimi. |
| Sanal tablo/authorizer kontrolü | 66 ayrı callback kaydı, iki Rust kipinde taban ile eş. |

[View C dal profili](/github/sqliters/target/perf-work/view-upsert-independent/res/c-coverage/sqlite3.c.gcov) · [Birleşik ölçüm](/github/sqliters/target/perf-work/prepare-diagnostics-independent/sonuc.md).
