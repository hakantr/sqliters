# V6b.52 — D156 C kayıt yazımı dal kapsamı

SQLite 3.54.1 için 12 sağlıklı native işlem ve ayrı 144 tek yazma reddi vakası kaydedildi. Sağlıklı işlem iki kayıt yazıyor. Reddetme matrisinde dört sayfa boyutu, üç journal kipi, UPDATE ve COMMIT ile IOERR_WRITE ve FULL kullanılıyor.

| C konumu | Koşul | Sağlıklı | Hata matrisi | Rust eşliği |
|---|---|---:|---:|---|
| `pager.c:6079` | Sayfa numarası xWrite başarı | 24 | 168 | Yazma boyutu, ofseti ve sonucu eşit. |
| `pager.c:6079` | Sayfa numarası xWrite hata | 0 | 48 | Yazım burada durdu, önceki DB korundu. |
| `pager.c:6081` | Sayfa verisi xWrite başarı | 24 | 120 | Yazma boyutu, ofseti ve sonucu eşit. |
| `pager.c:6081` | Sayfa verisi xWrite hata | 0 | 48 | Yazım burada durdu, önceki DB korundu. |
| `pager.c:6083` | Checksum xWrite başarı | 24 | 72 | Yazma boyutu, ofseti ve sonucu eşit. |
| `pager.c:6083` | Checksum xWrite hata | 0 | 48 | Yazım burada durdu, önceki DB korundu. |

`pager.c:6092–6098` kapsamındaki sayaç atamaları ile iki bitvec çağrısı sağlıklı profilde 24 kez yürüdü. Bitvec bellek tükenmesi ve bütün savepoint yolları açık. Ham gcov satırları `target/perf-work/v6b56-proof/res/gcov-write-scoped.txt`, ikinci profil `target/perf-work/v6b56-proof/write-failures/cov/sqlite3.c.gcov` altında.
