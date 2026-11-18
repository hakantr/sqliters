# V6b.49 — Olağan SQLite kapanışı C dalları

SQLite 3.54.1 kapsama nesnesi 18 sağlıklı dosya vakasıyla çalıştırıldı. Satırlar amalgamdan pager.c kaynağına eşlendi.

| C kapsamı | Blok | Alınan dal |
|---|---|---:|
| `pager.c:1915–1943` | Journal tanıtıcısı ve NO_LOCK | 6/10 |
| `pager.c:2240–2267` | Kapanışta okur ve yazıcı işlemi | 7/14 |
| `pager.c:4254–4272` | sqlite3PagerClose journal sync | 3/4 |
| `pager.c:4102–4108` | pagerSyncHotJournal | 3/4 |

32 çıkışın 19'u alındı. İlk üç kapsam 16/28 ve sync yardımcı yordamı 3/4.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust karar noktası |
|---|---|---|---|
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch 0 taken 96 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch 1 taken 0 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch 0 taken 36 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch 1 taken 60 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch 0 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch 1 taken 24 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch 0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch 1 taken 96 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch 2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch 3 never executed | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch 0 taken 108 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch 1 taken 0 | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch 2 taken 96 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch 3 taken 12 | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch 0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch 1 taken 96 | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch 0 taken 84 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch 1 taken 12 | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch 0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch 1 taken 12 | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2252` | `&& pPager->journalMode==PAGER_JOURNALMODE_MEMORY` | branch 0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2252` | `&& pPager->journalMode==PAGER_JOURNALMODE_MEMORY` | branch 1 never executed | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2253` | `&& isOpen(pPager->jfd)` | branch 0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:2253` | `&& isOpen(pPager->jfd)` | branch 1 never executed | `crates/sqliters-pager/src/lib.rs:6308` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch 0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6723` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch 1 taken 10 | `crates/sqliters-pager/src/lib.rs:6723` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch 0 taken 30 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6723` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch 1 taken 0 | `crates/sqliters-pager/src/lib.rs:6723` |
| `pager.c:4255` | `if( MEMDB ){` | branch 0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6230` |
| `pager.c:4255` | `if( MEMDB ){` | branch 1 taken 18 | `crates/sqliters-pager/src/lib.rs:6230` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch 0 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6230` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch 1 taken 6 | `crates/sqliters-pager/src/lib.rs:6230` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf |
|---|---|---|
| `pager.c:1916` | branch 1 taken 0 | Yaşam döngüsü. Ana DB tanıtıcısı açık değil. |
| `pager.c:1941` | branch 0 taken 0 (fallthrough) | G/Ç hatası. VFS NO_LOCK çağrısı başarısız. |
| `pager.c:1941` | branch 2 never executed (fallthrough) | G/Ç hatası ve durum. NO_LOCK başarısızken pager ERROR. |
| `pager.c:1941` | branch 3 never executed | G/Ç hatası ve durum. NO_LOCK başarısızken pager ERROR dışında. |
| `pager.c:2241` | branch 1 taken 0 | Durum. Pager kapanışa ERROR ile girer. |
| `pager.c:2243` | branch 0 taken 0 (fallthrough) | Durum. Aktif yazıcı kapanışta rollback yapar. |
| `pager.c:2251` | branch 0 taken 0 (fallthrough) | Durum. OPEN karşı kolunda ERROR durumu değerlendirilir. |
| `pager.c:2252` | branch 0 never executed (fallthrough) | Yapılandırma ve durum. ERROR sonrası MEMORY journal seçimi. |
| `pager.c:2252` | branch 1 never executed | Yapılandırma ve durum. ERROR sonrası MEMORY dışı journal seçimi. |
| `pager.c:2253` | branch 0 never executed (fallthrough) | Yaşam döngüsü. ERROR ve MEMORY ile journal tanıtıcısı açık. |
| `pager.c:2253` | branch 1 never executed | Yaşam döngüsü. ERROR ve MEMORY ile journal tanıtıcısı kapalı. |
| `pager.c:4107` | branch 1 taken 0 | G/Ç hatası. xSync başarısızsa xFileSize atlanır. |
| `pager.c:4255` | branch 0 taken 0 (fallthrough) | Yapılandırma. MEMDB kapanışı. |

Ham C kapsamı `/home/hakan/github/sqliters/target/perf-work/v6b53-proof/cov-baseline/sqlite3.c.gcov` içinde.
