# V6b.44 — Sıcak journal playback C dalları

SQLite 3.54.1 kapsama nesnesi 28 native playback akışıyla çalıştırıldı. Satır sayıları amalgamdan kaynak dosyalarına eşlendi.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:2368–2411` | pager_playback_one_page | 14/19 | 16/30 |
| `pager.c:2726–2753` | pager_truncate | 16/16 | 11/16 |
| `pager.c:2969–3000` | hot record short read and playback end | 12/14 | 9/10 |

56 dalın 36 tanesi alındı. Alınmayan 20 çıkış ayrı ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust yordamı |
|---|---|---|---|
| `pager.c:2374` | `jfd = isMainJrnl ? pPager->jfd : pPager->sjfd;` | branch  0 taken 56 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2374` | `jfd = isMainJrnl ? pPager->jfd : pPager->sjfd;` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2376` | `if( rc!=SQLITE_OK ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2376` | `if( rc!=SQLITE_OK ) return rc;` | branch  1 taken 56 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2378` | `if( rc!=SQLITE_OK ) return rc;` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2378` | `if( rc!=SQLITE_OK ) return rc;` | branch  1 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2386` | `if( pgno==0 \|\| pgno==PAGER_SJ_PGNO(pPager) ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2386` | `if( pgno==0 \|\| pgno==PAGER_SJ_PGNO(pPager) ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2386` | `if( pgno==0 \|\| pgno==PAGER_SJ_PGNO(pPager) ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2386` | `if( pgno==0 \|\| pgno==PAGER_SJ_PGNO(pPager) ){` | branch  3 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2390` | `if( pgno>(Pgno)pPager->dbSize \|\| sqlite3BitvecTest(pDone, pgno) ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2390` | `if( pgno>(Pgno)pPager->dbSize \|\| sqlite3BitvecTest(pDone, pgno) ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2390` | `if( pgno>(Pgno)pPager->dbSize \|\| sqlite3BitvecTest(pDone, pgno) ){` | branch  3 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2390` | `if( pgno>(Pgno)pPager->dbSize \|\| sqlite3BitvecTest(pDone, pgno) ){` | branch  4 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2393` | `if( isMainJrnl ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2393` | `if( isMainJrnl ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2395` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2395` | `if( rc ) return rc;` | branch  1 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2396` | `if( !isSavepnt && pager_cksum(pPager, (u8*)aData)!=cksum ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2396` | `if( !isSavepnt && pager_cksum(pPager, (u8*)aData)!=cksum ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2396` | `if( !isSavepnt && pager_cksum(pPager, (u8*)aData)!=cksum ){` | branch  3 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2396` | `if( !isSavepnt && pager_cksum(pPager, (u8*)aData)!=cksum ){` | branch  4 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2404` | `if( pDone && (rc = sqlite3BitvecSet(pDone, pgno))!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2404` | `if( pDone && (rc = sqlite3BitvecSet(pDone, pgno))!=SQLITE_OK ){` | branch  1 taken 48 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2404` | `if( pDone && (rc = sqlite3BitvecSet(pDone, pgno))!=SQLITE_OK ){` | branch  3 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2404` | `if( pDone && (rc = sqlite3BitvecSet(pDone, pgno))!=SQLITE_OK ){` | branch  4 never executed | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2410` | `if( pgno==1 && pPager->nReserve!=((u8*)aData)[20] ){` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2410` | `if( pgno==1 && pPager->nReserve!=((u8*)aData)[20] ){` | branch  1 taken 24 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2410` | `if( pgno==1 && pPager->nReserve!=((u8*)aData)[20] ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2410` | `if( pgno==1 && pPager->nReserve!=((u8*)aData)[20] ){` | branch  3 taken 24 | `crates/sqliters-pager/src/lib.rs:6660` |
| `pager.c:2733` | `if( isOpen(pPager->fd)` | branch  0 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2733` | `if( isOpen(pPager->fd)` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  0 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  2 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  0 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  2 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  3 taken 20 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2743` | `if( currentSize>newSize ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2743` | `if( currentSize>newSize ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2745` | `}else if( (currentSize+szPage)<=newSize ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2745` | `}else if( (currentSize+szPage)<=newSize ){` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2753` | `if( rc==SQLITE_OK ){` | branch  0 taken 12 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2753` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6542` |
| `pager.c:2973` | `for(u=0; u<nRec; u++){` | branch  0 taken 56 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2973` | `for(u=0; u<nRec; u++){` | branch  1 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2974` | `if( needPagerReset ){` | branch  0 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2974` | `if( needPagerReset ){` | branch  1 taken 24 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2979` | `if( rc==SQLITE_OK ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2979` | `if( rc==SQLITE_OK ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2982` | `if( rc==SQLITE_DONE ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2982` | `if( rc==SQLITE_DONE ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2985` | `}else if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6411` |
| `pager.c:2985` | `}else if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:6411` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `pager.c:2374` | branch  1 taken 0 | Yapılandırma — ana rollback journal yerine sub-journal. |
| `pager.c:2376` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — dört baytlık sayfa numarası okuması. |
| `pager.c:2386` | branch  1 taken 0 | Girdi — sıfır ya da super-journal sentinel sayfa numarası. |
| `pager.c:2386` | branch  2 taken 0 (fallthrough) | Girdi — sıfır ya da super-journal sentinel sayfa numarası. |
| `pager.c:2390` | branch  1 taken 0 | Girdi ve bağlam — DB sınırı dışındaki sayfa veya savepoint pDone bitvec'i. |
| `pager.c:2390` | branch  3 taken 0 (fallthrough) | Girdi ve bağlam — DB sınırı dışındaki sayfa veya savepoint pDone bitvec'i. |
| `pager.c:2393` | branch  1 taken 0 | Yapılandırma — sub-journal'da checksum alanı bulunmaması. |
| `pager.c:2395` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — dört baytlık checksum okuması. |
| `pager.c:2396` | branch  1 taken 0 | Girdi ve bağlam — checksum uyuşmazlığı veya savepoint playback. |
| `pager.c:2396` | branch  3 taken 0 (fallthrough) | Girdi ve bağlam — checksum uyuşmazlığı veya savepoint playback. |
| `pager.c:2404` | branch  0 taken 0 (fallthrough) | Bağlam ve hata enjeksiyonu — savepoint pDone bitvec'i ve tahsis hatası. |
| `pager.c:2404` | branch  3 never executed (fallthrough) | Bağlam ve hata enjeksiyonu — savepoint pDone bitvec'i ve tahsis hatası. |
| `pager.c:2404` | branch  4 never executed | Bağlam ve hata enjeksiyonu — savepoint pDone bitvec'i ve tahsis hatası. |
| `pager.c:2410` | branch  2 taken 0 (fallthrough) | Girdi — sayfa 1 nReserve değerinin playback ile değişmesi. |
| `pager.c:2733` | branch  1 taken 0 | Yaşam döngüsü — ana dosya tanıtıcısı açık değil. |
| `pager.c:2734` | branch  1 taken 0 | Yaşam döngüsü — sıcak OPEN dışındaki yazıcı durumları. |
| `pager.c:2734` | branch  3 taken 0 | Yaşam döngüsü — sıcak OPEN dışındaki yazıcı durumları. |
| `pager.c:2742` | branch  1 taken 0 | Hata enjeksiyonu — ana DB xFileSize başarısız. |
| `pager.c:2753` | branch  1 taken 0 | Hata enjeksiyonu — gerçek boyut değişikliği sırasında xTruncate/xWrite. |
| `pager.c:2982` | branch  0 taken 0 (fallthrough) | Girdi — geçersiz checksum veya sentinel ile SQLITE_DONE. Ayrı dosya matrisinde checksum vakası var. |
