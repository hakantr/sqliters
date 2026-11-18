# V6b.50 — COMMIT phase-two C dalları

SQLite 3.54.1 kapsam nesnesi 24 sağlıklı normal rollback-journal COMMIT vakasında çalıştırıldı. C kaynak aralığı pager.c:2204–2217.
14 çıkışın altısı alındı. C'nin COMMIT_PHASETWO dalı D155'in açık kapsamına aittir.

| C yeri | Koşul | Gcov çıkışı | Rust karar noktası veya açık kapsam |
|---|---|---|---|
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  0 taken 24 (fallthrough) | D155 açık |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  1 taken 0 | D155 açık |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  2 taken 24 (fallthrough) | D155 açık |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  3 taken 0 | D155 açık |
| `pager.c:2206` | `if( rc==SQLITE_NOTFOUND ) rc = SQLITE_OK;` | branch  0 taken 24 (fallthrough) | D155 açık |
| `pager.c:2206` | `if( rc==SQLITE_NOTFOUND ) rc = SQLITE_OK;` | branch  1 taken 0 | D155 açık |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  1 taken 24 | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  3 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  4 never executed | `crates/sqliters-pager/src/lib.rs:5385` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5390` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5390` |

## Sağlıklı ölçümde alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf |
|---|---|---|
| `pager.c:2204` | branch  1 taken 0 | G/Ç hatası. Journal sonlandırma veya DB küçültme önce başarısız olur. |
| `pager.c:2204` | branch  3 taken 0 | Bağlam. bCommit false ile rollback ya da salt okur sonlandırması. |
| `pager.c:2206` | branch  1 taken 0 | VFS. FCNTL 22 sonucu NOTFOUND dışı gelir. D155 kapsamı. |
| `pager.c:2209` | branch  1 taken 0 | Kilit kipi. EXCLUSIVE bağlantı SHARED'a geçmez. |
| `pager.c:2210` | branch  0 taken 0 (fallthrough) | Journal kipi. WAL tanıtıcısı mevcut. |
| `pager.c:2210` | branch  3 never executed (fallthrough) | WAL durumu. EXCLUSIVE geçişi olumlu ve SHARED çağrılır. |
| `pager.c:2210` | branch  4 never executed | WAL durumu. EXCLUSIVE geçişi olumsuz ve SHARED atlanır. |
| `pager.c:2217` | branch  1 taken 0 | G/Ç hatası. SHARED sonucu yerine önceki hata dönüyor. Ayrı native hata matrisi bunu ölçer. |

Ham kayıt `/home/hakan/github/sqliters/target/perf-work/v6b54-proof/res/c-branch-classification.json` içinde.
