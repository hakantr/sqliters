# V6b.46 — Sıcak DELETE sonlandırması C dalları

SQLite 3.54.1 kapsama nesnesi 40 native vakayla çalıştırıldı. Satırlar amalgamdan `sqlite/src/pager.c` kaynağına eşlendi.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:2154–2160` | DELETE dosyasını kapatma ve silme | 3/3 | 1/2 |
| `pager.c:2209–2217` | SHARED'a inme ve ilk hata önceliği | 6/6 | 4/8 |
| `pager.c:3035–3046` | Sonlandırma sonrası super-journal temizliği | 4/4 | 6/8 |

18 dalın 11'i alındı. Yedi çıkış ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust karar noktası |
|---|---|---|---|
| `pager.c:2157` | `if( bDelete ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5730` |
| `pager.c:2157` | `if( bDelete ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5730` |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  1 taken 56 | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  3 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  4 never executed | `crates/sqliters-pager/src/lib.rs:6489` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  0 taken 39 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  1 taken 17 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:3036` | `if( rc==SQLITE_OK ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6488` |
| `pager.c:3036` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6488` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  0 taken 31 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6496` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  1 taken 25 | `crates/sqliters-pager/src/lib.rs:6496` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  2 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6496` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  3 taken 23 | `crates/sqliters-pager/src/lib.rs:6496` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  4 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6496` |
| `pager.c:3040` | `if( rc==SQLITE_OK && zSuper && res ){` | branch  5 taken 0 | `crates/sqliters-pager/src/lib.rs:6496` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve ölçüm sınırı |
|---|---|---|
| `pager.c:2157` | branch  1 taken 0 | Yapılandırma. Geçici pager için dosya silme atlanır. |
| `pager.c:2209` | branch  1 taken 0 | Kilit kipi. EXCLUSIVE bağlantı SHARED'a inmez. |
| `pager.c:2210` | branch  0 taken 0 (fallthrough) | WAL. WAL handle'ı varsa sqlite3WalExclusiveMode çağrılır. |
| `pager.c:2210` | branch  3 never executed (fallthrough) | WAL. WAL handle'ının EXCLUSIVE kipi bırakıldığında SHARED'a inilir. |
| `pager.c:2210` | branch  4 never executed | WAL. WAL handle'ı EXCLUSIVE kipi bırakıldığını bildirmediğinde SHARED'a inilmez. |
| `pager.c:3036` | branch  1 taken 0 | Önceki G/Ç hatası. Playback, sayfa boyutu geri yükleme veya ana DB sync başarısız. |
| `pager.c:3040` | branch  5 taken 0 | Girdi. Trailer geçerli ve super-journal dosyası commit sırasında silinmiş. Ayrı D149 matrisinde ölçüldü. |
