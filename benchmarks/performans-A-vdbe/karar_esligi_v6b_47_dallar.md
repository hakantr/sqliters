# V6b.47 — Sıcak PERSIST ve TRUNCATE C dalları

SQLite 3.54.1 kapsama nesnesi 24 native ana vakayla çalıştırıldı. Satırlar amalgamdan pager.c kaynağına eşlendi.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:1436–1468` | PERSIST zeroJournalHdr | 14/14 | 17/18 |
| `pager.c:1915–1940` | pager_unlock journal ömrü | 4/5 | 2/6 |
| `pager.c:2124–2142` | TRUNCATE ve PERSIST sonlandırması | 10/10 | 14/18 |

42 dalın 33'ü ana matrisle alındı. Dokuz çıkış ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust karar noktası |
|---|---|---|---|
| `pager.c:1440` | `if( pPager->journalOff ){` | branch  0 taken 13 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1440` | `if( pPager->journalOff ){` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  0 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  2 taken 3 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  3 taken 9 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  0 taken 11 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  2 taken 10 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  3 taken 1 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  0 taken 11 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  2 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  3 taken 9 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  2 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  3 taken 1 | `crates/sqliters-pager/src/lib.rs:5798` |
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch  0 taken 140 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch  1 taken 140 | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:4962` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  0 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  1 taken 19 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  0 taken 7 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  2 taken 5 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  3 taken 2 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  1 taken 17 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  0 taken 16 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5763` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  3 taken 16 | `crates/sqliters-pager/src/lib.rs:5763` |

## Ana ölçümde alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve ölçüm sınırı |
|---|---|---|
| `pager.c:1463` | branch  1 taken 0 | G/Ç hatası. Pozitif journal limitini uygulamadan önceki xFileSize başarısız. |
| `pager.c:1916` | branch  1 taken 0 | Yaşam döngüsü. Ana DB tanıtıcısı açık değil. |
| `pager.c:1929` | branch  0 taken 0 (fallthrough) | Cihaz özelliği. Açık dosya silinemeyen VFS. Ek cihaz matrisinde alındı. |
| `pager.c:1930` | branch  0 never executed (fallthrough) | Cihaz özelliği ve journal kipi. Açık dosya silinemiyor ve kip maskesi PERSIST/TRUNCATE. Ek cihaz matrisinde alındı. |
| `pager.c:1930` | branch  1 never executed | Cihaz özelliği ve journal kipi. Açık dosya silinemiyor, kip maskesi PERSIST/TRUNCATE dışı. Ek cihaz matrisinde alındı. |
| `pager.c:2140` | branch  0 taken 0 (fallthrough) | Kilit kipi. PERSIST dışı dallarda EXCLUSIVE koşuluna ulaşılır. |
| `pager.c:2140` | branch  2 never executed (fallthrough) | Kilit ve journal kipi. EXCLUSIVE ve kip değeri WAL altında. |
| `pager.c:2140` | branch  3 never executed | Kilit ve journal kipi. EXCLUSIVE ve kip değeri WAL veya üstünde. |
| `pager.c:2142` | branch  2 taken 0 (fallthrough) | Geçici pager. Super-journal olmadan tempFile nedeniyle PERSIST başlığı için tam kesme. |

## Cihaz yeteneği ek ölçümü

Sekiz ek vakada `pager.c:1929` iki çıkışı 24 ve 24 kez, `pager.c:1930` iki çıkışı 8 ve 16 kez aldı. Hem journal tanıtıcısını tutma hem kapatma gözlendi. Rust karşılığı `lib.rs:4962`. Ham gcov `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/device/cov-final/sqlite3.c.gcov` içinde.

## Commit noktası ek ölçümü

Altı ayrı vaka `pager.c:2903` için beş super-journal yok sonucu ve üç normal rollback sonucu verdi. PERSIST `pager.c:1440` ve TRUNCATE `pager.c:2125` koşullarının iki çıkışı da alındı. Rust karşılığı `lib.rs:6482–6505,6690–6707`. Ham gcov `/home/hakan/github/sqliters/target/perf-work/v6b51-proof/commit-point/res/gcov-commit-point.txt` içinde.

| C yeri | Gcov çıkışı | Ek ölçümde alınmayan çıkış |
|---|---|---|
| `pager.c:2903` | branch 1 taken 0 | G/Ç hatası. Trailer okuması veya bağlı dosyanın varlık denetimi başarısız. |
| `pager.c:3036` | branch 1 taken 0 | G/Ç hatası. Sonlandırma öncesi playback, sayfa boyutu geri yükleme veya DB sync başarısız. |
| `pager.c:3040` | branch 1 taken 0 | G/Ç hatası. Başarısız playback veya sonlandırma sonrasında super-journal incelenmez. |
| `pager.c:3040` | branch 3 taken 0 | Bağlam. Bir super-journal trailer'ı bulunmadan başarılı playback. |
