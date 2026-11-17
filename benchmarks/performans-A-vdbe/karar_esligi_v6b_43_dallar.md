# V6b.43 — Sıcak journal seçiminin C dalları

SQLite 3.54.1 kapsama nesnesi boş ve dolu dosya imajları ile 20 native hata akışında çalıştırıldı. Ölçüm satırları amalgamdan kaynak dosyalarına eşlendi.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `os_unix.c:6884–6907` | unixAccess | 5/6 | 6/8 |
| `pager.c:5196–5285` | hasHotJournal | 34/34 | 20/30 |
| `pager.c:5329–5354` | shared-lock hot-journal selection | 10/12 | 7/10 |
| `pager.c:5369–5446` | exclusive recheck and journal open | 23/24 | 14/22 |
| `pager.c:7746–7778` | leaving WAL under exclusive lock | 9/17 | 6/18 |

88 dalın 53 tanesi alındı. Alınmayan 35 çıkış ayrı ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust yordamı |
|---|---|---|---|
| `os_unix.c:6898` | `if( flags==SQLITE_ACCESS_EXISTS ){` | branch  0 taken 494 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6898` | `if( flags==SQLITE_ACCESS_EXISTS ){` | branch  1 taken 0 | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6900` | `*pResOut = 0==osStat(zPath, &buf) &&` | branch  1 taken 188 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6900` | `*pResOut = 0==osStat(zPath, &buf) &&` | branch  2 taken 306 | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6901` | `(!S_ISREG(buf.st_mode) \|\| buf.st_size>0);` | branch  0 taken 188 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6901` | `(!S_ISREG(buf.st_mode) \|\| buf.st_size>0);` | branch  1 taken 0 | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6901` | `(!S_ISREG(buf.st_mode) \|\| buf.st_size>0);` | branch  2 taken 138 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:3671` |
| `os_unix.c:6901` | `(!S_ISREG(buf.st_mode) \|\| buf.st_size>0);` | branch  3 taken 50 | `crates/sqliters-vfs/src/lib.rs:3671` |
| `pager.c:5211` | `if( !jrnlOpen ){` | branch  0 taken 226 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5211` | `if( !jrnlOpen ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5214` | `if( rc==SQLITE_OK && exists ){` | branch  0 taken 226 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5214` | `if( rc==SQLITE_OK && exists ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5214` | `if( rc==SQLITE_OK && exists ){` | branch  2 taken 82 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5214` | `if( rc==SQLITE_OK && exists ){` | branch  3 taken 144 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5226` | `if( rc==SQLITE_OK && !locked ){` | branch  0 taken 82 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5226` | `if( rc==SQLITE_OK && !locked ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5226` | `if( rc==SQLITE_OK && !locked ){` | branch  2 taken 82 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5226` | `if( rc==SQLITE_OK && !locked ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5231` | `if( rc==SQLITE_OK ){` | branch  0 taken 82 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5231` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5240` | `if( nPage==0 && !jrnlOpen ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5240` | `if( nPage==0 && !jrnlOpen ){` | branch  1 taken 74 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5240` | `if( nPage==0 && !jrnlOpen ){` | branch  2 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5240` | `if( nPage==0 && !jrnlOpen ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5242` | `if( pagerLockDb(pPager, RESERVED_LOCK)==SQLITE_OK ){` | branch  1 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5242` | `if( pagerLockDb(pPager, RESERVED_LOCK)==SQLITE_OK ){` | branch  2 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5244` | `if( !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5244` | `if( !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5254` | `if( !jrnlOpen ){` | branch  0 taken 74 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5254` | `if( !jrnlOpen ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5258` | `if( rc==SQLITE_OK ){` | branch  0 taken 58 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5258` | `if( rc==SQLITE_OK ){` | branch  1 taken 16 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5261` | `if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5261` | `if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  1 taken 50 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5264` | `if( !jrnlOpen ){` | branch  0 taken 58 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5264` | `if( !jrnlOpen ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5268` | `}else if( rc==SQLITE_CANTOPEN ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5268` | `}else if( rc==SQLITE_CANTOPEN ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6365` |
| `pager.c:5335` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5335` | `if( rc!=SQLITE_OK ){` | branch  1 taken 226 | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5343` | `if( pPager->eLock<=SHARED_LOCK ){` | branch  0 taken 226 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5343` | `if( pPager->eLock<=SHARED_LOCK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5346` | `if( rc!=SQLITE_OK ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5346` | `if( rc!=SQLITE_OK ){` | branch  1 taken 218 | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5349` | `if( bHotJournal ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5349` | `if( bHotJournal ){` | branch  1 taken 162 | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5350` | `if( pPager->readOnly ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5350` | `if( pPager->readOnly ){` | branch  1 taken 56 | `crates/sqliters-pager/src/lib.rs:6335` |
| `pager.c:5371` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5371` | `if( rc!=SQLITE_OK ){` | branch  1 taken 56 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  2 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  2 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  2 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  3 taken 48 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5414` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5414` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5417` | `if( rc==SQLITE_OK ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5417` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5421` | `}else if( !pPager->exclusiveMode ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5421` | `}else if( !pPager->exclusiveMode ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5425` | `if( rc!=SQLITE_OK ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:5425` | `if( rc!=SQLITE_OK ){` | branch  1 taken 48 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:7755` | `if( !pPager->pWal ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7755` | `if( !pPager->pWal ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7758` | `if( rc==SQLITE_OK ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7758` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7763` | `if( rc==SQLITE_OK && logexists ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7763` | `if( rc==SQLITE_OK && logexists ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7763` | `if( rc==SQLITE_OK && logexists ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7763` | `if( rc==SQLITE_OK && logexists ){` | branch  3 taken 2 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7771` | `if( rc==SQLITE_OK && pPager->pWal ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7771` | `if( rc==SQLITE_OK && pPager->pWal ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7771` | `if( rc==SQLITE_OK && pPager->pWal ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7771` | `if( rc==SQLITE_OK && pPager->pWal ){` | branch  3 taken 2 | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7773` | `if( rc==SQLITE_OK ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7773` | `if( rc==SQLITE_OK ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7778` | `if( rc && !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7778` | `if( rc && !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7778` | `if( rc && !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:2384` |
| `pager.c:7778` | `if( rc && !pPager->exclusiveMode ) pagerUnlockDb(pPager, SHARED_LOCK);` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:2384` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `os_unix.c:6898` | branch  1 taken 0 | VFS çağrısı — READWRITE erişim sorgusu. |
| `os_unix.c:6901` | branch  1 taken 0 | Dosya türü — dizin veya diğer normal dosya dışındaki nesne. |
| `pager.c:5211` | branch  1 taken 0 | Platform — UNDELETABLE_WHEN_OPEN ile açık journal. |
| `pager.c:5214` | branch  1 taken 0 | Hata enjeksiyonu — ilk xAccess hatası. |
| `pager.c:5226` | branch  1 taken 0 | Rekabet ve hata enjeksiyonu — başka RESERVED kilidi veya xCheckReservedLock hatası. |
| `pager.c:5226` | branch  3 taken 0 | Rekabet ve hata enjeksiyonu — başka RESERVED kilidi veya xCheckReservedLock hatası. |
| `pager.c:5231` | branch  1 taken 0 | Hata enjeksiyonu — ana DB sayfa sayımı. |
| `pager.c:5240` | branch  3 taken 0 | Platform — boş DB yanında açık journal tutuluyor. |
| `pager.c:5242` | branch  2 taken 0 | Rekabet — boş DB yanında RESERVED edinilemedi. |
| `pager.c:5244` | branch  1 taken 0 | Yapılandırma — boş DB temizliği sonrası EXCLUSIVE kilit tutulması. |
| `pager.c:5254` | branch  1 taken 0 | Platform — ilk bayt için mevcut açık handle. |
| `pager.c:5264` | branch  1 taken 0 | Platform — ilk bayttan sonra mevcut açık handle korunur. |
| `pager.c:5335` | branch  0 taken 0 (fallthrough) | Rekabet ve hata enjeksiyonu — SHARED edinme hatası. |
| `pager.c:5343` | branch  1 taken 0 | Kilit durumu — UNKNOWN veya önceden tutulmuş EXCLUSIVE. |
| `pager.c:5350` | branch  0 taken 0 (fallthrough) | Yapılandırma — ana DB salt okunur. |
| `pager.c:5371` | branch  0 taken 0 (fallthrough) | Rekabet ve hata enjeksiyonu — kurtarma için EXCLUSIVE edinme hatası. |
| `pager.c:5388` | branch  1 taken 0 | Yapılandırma ve platform — OFF journal ya da açık journal tanıtıcısı. |
| `pager.c:5388` | branch  3 taken 0 | Yapılandırma ve platform — OFF journal ya da açık journal tanıtıcısı. |
| `pager.c:5393` | branch  1 taken 0 | Rekabet ve hata enjeksiyonu — ikinci xAccess sonucu veya artık dosyanın olmaması. |
| `pager.c:5393` | branch  3 taken 0 | Rekabet ve hata enjeksiyonu — ikinci xAccess sonucu veya artık dosyanın olmaması. |
| `pager.c:5399` | branch  1 taken 0 | Hata enjeksiyonu — ikinci xOpen doğrudan hata döndürür. |
| `pager.c:5417` | branch  1 taken 0 | Hata enjeksiyonu — sıcak journal sync hatası. |
| `pager.c:5421` | branch  1 taken 0 | Yapılandırma — journal açılmadan EXCLUSIVE kilidi tutma. |
| `pager.c:7755` | branch  1 taken 0 | Yaşam döngüsü — WAL tanıtıcısı kip değişikliğinden önce zaten açık. |
| `pager.c:7758` | branch  1 taken 0 | Rekabet veya VFS hatası — kip değişikliği öncesi SHARED edinilemedi. |
| `pager.c:7763` | branch  1 taken 0 | VFS sonucu — kapanış öncesi WAL var veya xAccess hatası. |
| `pager.c:7763` | branch  2 taken 0 (fallthrough) | VFS sonucu — kapanış öncesi WAL var veya xAccess hatası. |
| `pager.c:7771` | branch  1 taken 0 | Yaşam döngüsü ve hata enjeksiyonu — açılmış WAL ve başarılı tespit veya önceki hata. |
| `pager.c:7771` | branch  2 taken 0 (fallthrough) | Yaşam döngüsü ve hata enjeksiyonu — açılmış WAL ve başarılı tespit veya önceki hata. |
| `pager.c:7773` | branch  0 never executed (fallthrough) | Rekabet — açık WAL kapanmadan önce EXCLUSIVE edinme sonucu. |
| `pager.c:7773` | branch  1 never executed | Rekabet — açık WAL kapanmadan önce EXCLUSIVE edinme sonucu. |
| `pager.c:7778` | branch  0 never executed (fallthrough) | Kapanış hatası ve kip — WAL kapanışından sonra SHARED'a indirme. |
| `pager.c:7778` | branch  1 never executed | Kapanış hatası ve kip — WAL kapanışından sonra SHARED'a indirme. |
| `pager.c:7778` | branch  2 never executed (fallthrough) | Kapanış hatası ve kip — WAL kapanışından sonra SHARED'a indirme. |
| `pager.c:7778` | branch  3 never executed | Kapanış hatası ve kip — WAL kapanışından sonra SHARED'a indirme. |
