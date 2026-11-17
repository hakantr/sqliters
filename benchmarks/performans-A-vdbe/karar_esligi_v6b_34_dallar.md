# V6b.34 — sayfalayıcı eşleme kararları ve C dal kapsamı

SQLite 3.54.1 referans amalgamı ve `-O0 --coverage` nesnesi kullanıldı. 492 native akış ile 13 truncate ve 17 SIZE_HINT işlemi aynı yeni C profilini besledi. SQLite kaynak ağacı salt okunur tutuldu. Başka profillerin sayaçları bu tabloya eklenmedi.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `os_unix.c:5674–5696` | `unixMapfile` | 10/11 | 9/10 |
| `os_unix.c:5585–5656` | `unixRemapfile` | 19/25 | 9/14 |
| `os_unix.c:4049–4110` | `fcntlSizeHint` | 8/21 | 7/20 |
| `os_unix.c:3961–4006` | `unixTruncate` | 8/11 | 4/6 |
| `os_unix.c:5711–5735` | `unixFetch` | 12/12 | 7/8 |
| `os_unix.c:4228–4251` | `UNIX_MMAP_SIZE` | 11/12 | 9/10 |
| `wal.c:2361–2382` | `walCheckpoint_growth` | 7/8 | 5/8 |
| `pager.c:5703–5768` | `getPageMMap` | 25/30 | 29/34 |
| `pager.c:4124–4162` | `pagerAcquireMapPage` | 16/18 | 3/4 |
| `pager.c:4169–4177` | `pagerReleaseMapPage` | 7/7 | 0/0 |
| `pager.c:3587–3598` | `pagerFixMaplimit` | 8/8 | 3/4 |
| `pager.c:5533–5538` | `pagerUnlockIfUnused` | 4/4 | 2/2 |
| `pager.c:5821–5830` | `sqlite3PagerLookup` | 3/3 | 2/2 |
| `pager.c:5846–5857` | `sqlite3PagerUnrefNotNull` | 4/4 | 2/2 |
| `pager.c:5861–5869` | `sqlite3PagerUnrefPageOne` | 4/4 | 0/0 |
| `pager.c:4493–4513` | `pager_write_pagelist` | 8/8 | 9/10 |
| `pager.c:2733–2754` | `pager_truncate_growth` | 2/15 | 3/16 |

Toplam **150 dal**, **47 alınmayan dal**. Satırlar gcov'un 64-bit Unix hedefinde yayımladığı çıkışlardır. Yürütülen dal sayısı koşul, sonuç ve Rust karşılığı birlikte incelenerek değerlendirilir.

## Ölçülen çıkışlar

| C yeri | Koşul | C gcov sonucu | Rust yeri |
|---|---|---|---|
| `os_unix.c:3972` | `if( pFile->szChunk>0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:3972` | `if( pFile->szChunk>0 ){` | branch  1 taken 108 | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:3977` | `if( rc ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:3977` | `if( rc ){` | branch  1 taken 108 | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:3999` | `if( nByte<pFile->mmapSize ){` | branch  0 taken 5 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:3999` | `if( nByte<pFile->mmapSize ){` | branch  1 taken 103 | `crates/sqliters-vfs/src/fetch.rs:126` |
| `os_unix.c:4050` | `if( pFile->szChunk>0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4050` | `if( pFile->szChunk>0 ){` | branch  1 taken 3334 | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4054` | `if( osFstat(pFile->h, &buf) ){` | branch  1 never executed (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4054` | `if( osFstat(pFile->h, &buf) ){` | branch  2 never executed | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4059` | `if( nSize>(i64)buf.st_size ){` | branch  0 never executed (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4059` | `if( nSize>(i64)buf.st_size ){` | branch  1 never executed | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4085` | `for(/*no-op*/; iWrite<nSize+nBlk-1; iWrite+=nBlk ){` | branch  0 never executed | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4085` | `for(/*no-op*/; iWrite<nSize+nBlk-1; iWrite+=nBlk ){` | branch  1 never executed (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4086` | `if( iWrite>=nSize ) iWrite = nSize - 1;` | branch  0 never executed (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4086` | `if( iWrite>=nSize ) iWrite = nSize - 1;` | branch  1 never executed | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4088` | `if( nWrite!=1 ) return SQLITE_IOERR_WRITE;` | branch  0 never executed (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4088` | `if( nWrite!=1 ) return SQLITE_IOERR_WRITE;` | branch  1 never executed | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4095` | `if( pFile->mmapSizeMax>0 && nByte>pFile->mmapSize ){` | branch  0 taken 41 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4095` | `if( pFile->mmapSizeMax>0 && nByte>pFile->mmapSize ){` | branch  1 taken 3293 | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4095` | `if( pFile->mmapSizeMax>0 && nByte>pFile->mmapSize ){` | branch  2 taken 40 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4095` | `if( pFile->mmapSizeMax>0 && nByte>pFile->mmapSize ){` | branch  3 taken 1 | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4097` | `if( pFile->szChunk<=0 ){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4097` | `if( pFile->szChunk<=0 ){` | branch  1 taken 0 | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4098` | `if( robust_ftruncate(pFile->h, nByte) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4098` | `if( robust_ftruncate(pFile->h, nByte) ){` | branch  2 taken 40 | `crates/sqliters-vfs/src/fetch.rs:109` |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  1 taken 5402 | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  0 taken 3078 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  1 taken 2324 | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  2 taken 1280 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  3 taken 1798 | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  4 taken 995 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  5 taken 285 | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  0 taken 485 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  1 taken 510 | `crates/sqliters-vfs/src/fetch.rs:46` |
| `os_unix.c:5606` | `if( pOrig ){` | branch  0 taken 39 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5606` | `if( pOrig ){` | branch  1 taken 487 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5610` | `if( nReuse<pFd->mmapSizeActual ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5610` | `if( nReuse<pFd->mmapSizeActual ){` | branch  1 taken 39 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5616` | `if( nReuse>0 ){` | branch  0 taken 39 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5616` | `if( nReuse>0 ){` | branch  1 taken 0 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  0 taken 39 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  1 taken 0 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  3 taken 39 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5640` | `if( pNew==0 ){` | branch  0 taken 487 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5640` | `if( pNew==0 ){` | branch  1 taken 39 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5644` | `if( pNew==MAP_FAILED ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5644` | `if( pNew==MAP_FAILED ){` | branch  1 taken 526 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5677` | `if( pFd->nFetchOut>0 ) return SQLITE_OK;` | branch  0 taken 1 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5677` | `if( pFd->nFetchOut>0 ) return SQLITE_OK;` | branch  1 taken 1011 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5679` | `if( nMap<0 ){` | branch  0 taken 972 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5679` | `if( nMap<0 ){` | branch  1 taken 39 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5681` | `if( osFstat(pFd->h, &statbuf) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5681` | `if( osFstat(pFd->h, &statbuf) ){` | branch  2 taken 972 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5686` | `if( nMap>pFd->mmapSizeMax ){` | branch  0 taken 779 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5686` | `if( nMap>pFd->mmapSizeMax ){` | branch  1 taken 232 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5691` | `if( nMap!=pFd->mmapSize ){` | branch  0 taken 526 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5691` | `if( nMap!=pFd->mmapSize ){` | branch  1 taken 485 | `crates/sqliters-vfs/src/fetch.rs:23` |
| `os_unix.c:5718` | `if( pFd->mmapSizeMax>0 ){` | branch  0 taken 5665 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5718` | `if( pFd->mmapSizeMax>0 ){` | branch  1 taken 1 | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5724` | `if( pFd->pMapRegion==0 ){` | branch  0 taken 487 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5724` | `if( pFd->pMapRegion==0 ){` | branch  1 taken 5178 | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5726` | `if( rc!=SQLITE_OK ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5726` | `if( rc!=SQLITE_OK ) return rc;` | branch  1 taken 487 | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5728` | `if( pFd->mmapSize >= (iOff+nAmt+nEofBuffer) ){` | branch  0 taken 2170 (fallthrough) | `crates/sqliters-vfs/src/fetch.rs:74` |
| `os_unix.c:5728` | `if( pFd->mmapSize >= (iOff+nAmt+nEofBuffer) ){` | branch  1 taken 3495 | `crates/sqliters-vfs/src/fetch.rs:74` |
| `pager.c:2733` | `if( isOpen(pPager->fd)` | branch  0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2733` | `if( isOpen(pPager->fd)` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2734` | `&& (pPager->eState>=PAGER_WRITER_DBMOD \|\| pPager->eState==PAGER_OPEN)` | branch  3 taken 20 | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2742` | `if( rc==SQLITE_OK && currentSize!=newSize ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2743` | `if( currentSize>newSize ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2743` | `if( currentSize>newSize ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2745` | `}else if( (currentSize+szPage)<=newSize ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2745` | `}else if( (currentSize+szPage)<=newSize ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2753` | `if( rc==SQLITE_OK ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:2753` | `if( rc==SQLITE_OK ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5112` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  0 taken 3074 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2770` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  1 taken 1440 | `crates/sqliters-pager/src/lib.rs:2770` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  2 taken 3074 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2770` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:2770` |
| `pager.c:4132` | `if( pPager->pMmapFreelist ){` | branch  0 taken 1292 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1575` |
| `pager.c:4132` | `if( pPager->pMmapFreelist ){` | branch  1 taken 510 | `crates/sqliters-pager/src/lib.rs:1575` |
| `pager.c:4140` | `if( p==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1575` |
| `pager.c:4140` | `if( p==0 ){` | branch  1 taken 510 | `crates/sqliters-pager/src/lib.rs:1575` |
| `pager.c:4496` | `if( !isOpen(pPager->fd) ){` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4496` | `if( !isOpen(pPager->fd) ){` | branch  1 taken 3858 | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4505` | `if( rc==SQLITE_OK` | branch  0 taken 3882 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4505` | `if( rc==SQLITE_OK` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4506` | `&& pPager->dbHintSize<pPager->dbSize` | branch  0 taken 3850 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4506` | `&& pPager->dbHintSize<pPager->dbSize` | branch  1 taken 32 | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4507` | `&& (pList->pDirty \|\| pList->pgno>pPager->dbHintSize)` | branch  0 taken 2948 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4507` | `&& (pList->pDirty \|\| pList->pgno>pPager->dbHintSize)` | branch  1 taken 902 | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4507` | `&& (pList->pDirty \|\| pList->pgno>pPager->dbHintSize)` | branch  2 taken 2396 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:4507` | `&& (pList->pDirty \|\| pList->pgno>pPager->dbHintSize)` | branch  3 taken 552 | `crates/sqliters-pager/src/lib.rs:2133` |
| `pager.c:5534` | `if( sqlite3PcacheRefCount(pPager->pPCache)==0 ){` | branch  1 taken 2612 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4806` |
| `pager.c:5534` | `if( sqlite3PcacheRefCount(pPager->pPCache)==0 ){` | branch  2 taken 32 | `crates/sqliters-pager/src/lib.rs:4806` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  0 taken 9352 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  1 taken 732 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  2 taken 4080 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  3 taken 5272 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  4 taken 388 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5718` | `&& (pPager->eState==PAGER_READER \|\| (flags & PAGER_GET_READONLY))` | branch  5 taken 3692 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5727` | `if( pgno<=1 && pgno==0 ){` | branch  0 taken 732 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5727` | `if( pgno<=1 && pgno==0 ){` | branch  1 taken 9352 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5727` | `if( pgno<=1 && pgno==0 ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5727` | `if( pgno<=1 && pgno==0 ){` | branch  3 taken 732 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5735` | `if( bMmapOk && pagerUseWal(pPager) ){` | branch  0 taken 5660 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5735` | `if( bMmapOk && pagerUseWal(pPager) ){` | branch  1 taken 4424 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5735` | `if( bMmapOk && pagerUseWal(pPager) ){` | branch  2 taken 52 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5735` | `if( bMmapOk && pagerUseWal(pPager) ){` | branch  3 taken 5608 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5737` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5737` | `if( rc!=SQLITE_OK ){` | branch  1 taken 52 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5742` | `if( bMmapOk && iFrame==0 ){` | branch  0 taken 5660 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5742` | `if( bMmapOk && iFrame==0 ){` | branch  1 taken 4424 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5742` | `if( bMmapOk && iFrame==0 ){` | branch  2 taken 5658 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5742` | `if( bMmapOk && iFrame==0 ){` | branch  3 taken 2 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5747` | `if( rc==SQLITE_OK && pData ){` | branch  0 taken 5658 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5747` | `if( rc==SQLITE_OK && pData ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5747` | `if( rc==SQLITE_OK && pData ){` | branch  2 taken 2166 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5747` | `if( rc==SQLITE_OK && pData ){` | branch  3 taken 3492 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5748` | `if( pPager->eState>PAGER_READER \|\| pPager->tempFile ){` | branch  0 taken 1794 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5748` | `if( pPager->eState>PAGER_READER \|\| pPager->tempFile ){` | branch  1 taken 372 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5748` | `if( pPager->eState>PAGER_READER \|\| pPager->tempFile ){` | branch  2 taken 16 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5748` | `if( pPager->eState>PAGER_READER \|\| pPager->tempFile ){` | branch  3 taken 1778 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5751` | `if( pPg==0 ){` | branch  0 taken 1802 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5751` | `if( pPg==0 ){` | branch  1 taken 364 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5756` | `if( pPg ){` | branch  0 taken 2166 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5756` | `if( pPg ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5762` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5762` | `if( rc!=SQLITE_OK ){` | branch  1 taken 3492 | `crates/sqliters-pager/src/lib.rs:1542` |
| `pager.c:5828` | `if( pPage==0 ) return 0;` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1618` |
| `pager.c:5828` | `if( pPage==0 ) return 0;` | branch  1 taken 384 | `crates/sqliters-pager/src/lib.rs:1618` |
| `pager.c:5849` | `if( pPg->flags & PGHDR_MMAP ){` | branch  0 taken 1802 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1596` |
| `pager.c:5849` | `if( pPg->flags & PGHDR_MMAP ){` | branch  1 taken 50426 | `crates/sqliters-pager/src/lib.rs:1596` |
| `wal.c:2364` | `if( rc==SQLITE_OK ){` | branch  0 taken 34 (fallthrough) | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2364` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2369` | `if( rc==SQLITE_OK && nSize<nReq ){` | branch  0 taken 34 (fallthrough) | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2369` | `if( rc==SQLITE_OK && nSize<nReq ){` | branch  1 taken 0 | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2369` | `if( rc==SQLITE_OK && nSize<nReq ){` | branch  2 taken 32 (fallthrough) | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2369` | `if( rc==SQLITE_OK && nSize<nReq ){` | branch  3 taken 2 | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2370` | `if( (nSize+65536+(i64)pWal->hdr.mxFrame*szPage)<nReq ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:2527` |
| `wal.c:2370` | `if( (nSize+65536+(i64)pWal->hdr.mxFrame*szPage)<nReq ){` | branch  1 taken 32 | `crates/sqliters-pager/src/wal.rs:2527` |

## Alınmayan çıkışların tek tek sınıflandırılması

| C yeri | Gcov çıkışı | Sınıf ve Rust karşılığının sınırı |
|---|---|---|
| `os_unix.c:5681` | branch  1 taken 0 (fallthrough) | Harita için fstat hata yönü alınmadı. Rust IOERR_FSTAT üretir ve çıkışı yayar. Syscall hata enjeksiyonu açık. |
| `os_unix.c:5610` | branch  0 taken 0 (fallthrough) | C mevcut eşlemenin yeniden kullanılmayan kuyruğunu kısaltmadı. Rust, lease yokken eski eşlemeyi bırakıp yeniden eşler. mremap/munmap sıra eşliği açık. |
| `os_unix.c:5616` | branch  1 taken 0 | Mevcut eşlemenin yeniden kullanılamayacak kadar kısa olması alınmadı. C yeniden kullanım yolu ile Rust yeni eşleme stratejisi farklıdır. |
| `os_unix.c:5633` | branch  1 taken 0 | C mremap başarısızlık veya NULL dalları alınmadı. Rust büyümede eski adres aralığını yeniden kullanmaz. Tam syscall ve hata uyarısı eşliği açık. |
| `os_unix.c:5633` | branch  2 taken 0 (fallthrough) | C mremap başarısızlık veya NULL dalları alınmadı. Rust büyümede eski adres aralığını yeniden kullanmaz. Tam syscall ve hata uyarısı eşliği açık. |
| `os_unix.c:5644` | branch  0 taken 0 (fallthrough) | Son mmap çağrısında MAP_FAILED yok. Rust limiti sıfırlayabilir. C sqlite3_log uyarısı hâlâ açık. |
| `os_unix.c:4050` | branch  0 taken 0 (fallthrough) | Pozitif szChunk yapılandırması yok. Rust varsayılan sıfır chunk yolunu taşır. Özel chunk-size davranışı açık. |
| `os_unix.c:4054` | branch  1 never executed (fallthrough) | szChunk pozitif olmadığı için chunk-size fstat kontrolüne girilmedi. Bu özel fstat yolu Rust'ta taşınmadı. |
| `os_unix.c:4054` | branch  2 never executed | szChunk pozitif olmadığı için chunk-size fstat kontrolüne girilmedi. Bu özel fstat yolu Rust'ta taşınmadı. |
| `os_unix.c:4059` | branch  0 never executed (fallthrough) | Pozitif chunk-size yuvarlama sonrası fiziksel dosya karşılaştırması çalışmadı. Rust'ta bu chunk-size dalı açık. |
| `os_unix.c:4059` | branch  1 never executed | Pozitif chunk-size yuvarlama sonrası fiziksel dosya karşılaştırması çalışmadı. Rust'ta bu chunk-size dalı açık. |
| `os_unix.c:4085` | branch  0 never executed | Pozitif chunk-size ve fallocate bulunmayan derleme koşulunda kullanılan blok döngüsü çalışmadı. Rust chunk preallocation yolu açık. |
| `os_unix.c:4085` | branch  1 never executed (fallthrough) | Pozitif chunk-size ve fallocate bulunmayan derleme koşulunda kullanılan blok döngüsü çalışmadı. Rust chunk preallocation yolu açık. |
| `os_unix.c:4086` | branch  0 never executed (fallthrough) | Chunk için blok sonuna tek bayt yazan yol çalışmadı. Son yazımın dosya sonuna kısaltılması Rust'ta taşınmadı. |
| `os_unix.c:4086` | branch  1 never executed | Chunk için blok sonuna tek bayt yazan yol çalışmadı. Son yazımın dosya sonuna kısaltılması Rust'ta taşınmadı. |
| `os_unix.c:4088` | branch  0 never executed (fallthrough) | Chunk-size yazma yolu çalışmadı. C IOERR_WRITE kolu ve başarılı yazımın Rust karşılığı açık. |
| `os_unix.c:4088` | branch  1 never executed | Chunk-size yazma yolu çalışmadı. C IOERR_WRITE kolu ve başarılı yazımın Rust karşılığı açık. |
| `os_unix.c:4097` | branch  1 taken 0 | Pozitif chunk-size halinde mmap büyümeden önce ftruncate atlanır. Bu yapılandırma alınmadı ve Rust bu özel dalı taşımaz. |
| `os_unix.c:4098` | branch  1 taken 0 (fallthrough) | Varsayılan chunk-size yolunda ftruncate başarıyla çalıştı. Hata kolu enjekte edilmedi. Rust IOERR_TRUNCATE döndürür ve eski eşlemeyi korur. |
| `os_unix.c:3972` | branch  0 taken 0 (fallthrough) | Pozitif chunk-size ayarlanmadı. C boyutu chunk sınırına yuvarlar. Rust truncate'ın bu yolu uygulanmadı. |
| `os_unix.c:3977` | branch  0 taken 0 (fallthrough) | ftruncate hatası enjekte edilmedi. Rust IOERR_TRUNCATE döndürür ve kullanılabilir eşleme boyutunu değiştirmez. Tam ileti ve syscall izi açık. |
| `os_unix.c:5726` | branch  0 taken 0 (fallthrough) | unixMapfile fstat hatası enjekte edilmedi. Rust Result ile IOERR_FSTAT yayar. Tam hata metni ve sıra kanıtı açık. |
| `os_unix.c:4231` | branch  0 taken 0 (fallthrough) | Bu profilde yapılandırılmış üst sınır aşılmadı. Rust 0x7fff0000 sınırı uygular. Önceki aşım girdileri ve ayrı VFS matrisi global sınırın varsayılan yapılandırmasını kontrol eder. |
| `wal.c:2364` | branch  1 taken 0 | Checkpoint WAL sync başarısızlığı bu profilde seçilmedi. Rust hata ile döner ve büyüme hint'ine ulaşmaz. Hata metni ayrıca doğrulanmalı. |
| `wal.c:2369` | branch  1 taken 0 | Ana dosya boyutu sorgusunun hatası alınmadı. Rust file_size hatasını hint öncesinde yayar. Hata enjeksiyonu açık. |
| `wal.c:2370` | branch  0 taken 0 (fallthrough) | Bozuk WAL'da çerçevelerden açıklanamayan büyüme alınmadı. Rust mevcut büyüklük guard'ını korur. Bu profilde hata mesajı karşılaştırılmadı. |
| `pager.c:5727` | branch  2 taken 0 (fallthrough) | pgno=0 pager'a ulaşmadı. Rust b-tree ve ham pin girişleri sıfırı CORRUPT döndürerek keser. Doğrudan C pager hatası enjekte edilmedi. |
| `pager.c:5737` | branch  0 taken 0 (fallthrough) | WAL frame aramasının hata kolu alınmadı. Rust önce find_frame hatasını yayar. WAL indeks hatası enjeksiyonu açık. |
| `pager.c:5747` | branch  1 taken 0 | xFetch hata dönüşü yok. Rust ? ile hatayı PCache bakışından önce yayar. Hata enjeksiyonu ve ileti eşliği açık. |
| `pager.c:5756` | branch  1 taken 0 | Başarılı xFetch sonrasında PgHdr yokluğu NOMEM ile oluşabilir. Rust ekleme için try_reserve kullanır ve FetchPage'i bırakır. C PgHdr tahsisi ve Rust VFS kopya tahsisi için tam NOMEM eşliği açık. |
| `pager.c:5762` | branch  0 taken 0 (fallthrough) | xFetch veya PgHdr tahsis hatasıyla normal getter'ı atlayan çıkış alınmadı. Rust Result ile çıkar. Hata enjeksiyonu açık. |
| `pager.c:4140` | branch  0 taken 0 (fallthrough) | PgHdr tahsisi başarısız olmadı. Rust tabloda try_reserve hatasında VFS lease'ini bırakır. C freelist ve VFS kopyasının NOMEM sınırı ayrı. |
| `pager.c:3590` | branch  3 taken 0 | Açık iVersion<3 fd bu Unix profilinde yok. Rust supports_fetch ile getter'ı kapalı tutar. Uyumlu özel VFS ve Windows xFetch eşliği açık. |
| `pager.c:4505` | branch  1 taken 0 | TEMP fiziksel açılış hatası alınmadı. Rust VFS open sonucunu ilk yazım ve hint öncesinde yayar. Hata enjeksiyonu açık. |
| `pager.c:2733` | branch  1 taken 0 | Kapalı fd ile pager_truncate çağrısı alınmadı. Rust açılmamış TEMP fiziksel yazı yoluna girmez. Düşük seviye pager durumu ayrı. |
| `pager.c:2734` | branch  1 taken 0 | Dosya açıkken truncation için SQLite'ın OPEN/WRITER_DBMOD alt durumları bu yönde çalışmadı. Rust Branch sonuçlarını ayrı pager durum modeliyle genel olarak kanıtlamadı. |
| `pager.c:2734` | branch  2 taken 0 (fallthrough) | Dosya açıkken truncation için SQLite'ın OPEN/WRITER_DBMOD alt durumları bu yönde çalışmadı. Rust Branch sonuçlarını ayrı pager durum modeliyle genel olarak kanıtlamadı. |
| `pager.c:2742` | branch  0 never executed (fallthrough) | Bu profilde pager_truncate önkoşulları iç boyut sorgusuna ulaşmadı. Rust commit büyümesi için file_size karşılaştırır. Hot journal ve hata izindeki tüm çağrılar açık. |
| `pager.c:2742` | branch  1 never executed | Bu profilde pager_truncate önkoşulları iç boyut sorgusuna ulaşmadı. Rust commit büyümesi için file_size karşılaştırır. Hot journal ve hata izindeki tüm çağrılar açık. |
| `pager.c:2742` | branch  2 never executed (fallthrough) | Bu profilde pager_truncate önkoşulları iç boyut sorgusuna ulaşmadı. Rust commit büyümesi için file_size karşılaştırır. Hot journal ve hata izindeki tüm çağrılar açık. |
| `pager.c:2742` | branch  3 never executed | Bu profilde pager_truncate önkoşulları iç boyut sorgusuna ulaşmadı. Rust commit büyümesi için file_size karşılaştırır. Hot journal ve hata izindeki tüm çağrılar açık. |
| `pager.c:2743` | branch  0 never executed (fallthrough) | Bu pager_truncate çağrısı üzerinden küçülme veya büyüme ayrımı çalışmadı. Doğrudan VFS truncate küçülmesi ayrı probda ölçüldü. |
| `pager.c:2743` | branch  1 never executed | Bu pager_truncate çağrısı üzerinden küçülme veya büyüme ayrımı çalışmadı. Doğrudan VFS truncate küçülmesi ayrı probda ölçüldü. |
| `pager.c:2745` | branch  0 never executed (fallthrough) | C son sayfayı sıfırla yazıp dosyayı büyüten nadir yol bu profilde çalışmadı. Rust var olan büyütme yordamına en az bir sayfalık farkta hint ekler. Fiziksel xWrite sırası tam eşlenmedi. |
| `pager.c:2745` | branch  1 never executed | C son sayfayı sıfırla yazıp dosyayı büyüten nadir yol bu profilde çalışmadı. Rust var olan büyütme yordamına en az bir sayfalık farkta hint ekler. Fiziksel xWrite sırası tam eşlenmedi. |
| `pager.c:2753` | branch  0 never executed (fallthrough) | İç truncate/yazımın başarılı ve hatalı sonraki durumları bu profilde çalışmadı. Hata yayılımı ve dbFileSize geçişi ayrıca doğrulanmalı. |
| `pager.c:2753` | branch  1 never executed | İç truncate/yazımın başarılı ve hatalı sonraki durumları bu profilde çalışmadı. Hata yayılımı ve dbFileSize geçişi ayrıca doğrulanmalı. |

`pagerAcquireMapPage` iki PgHdr yolunu da çalıştırdı. Rust başarılı her xFetch'e ayrı kimlik ve lease atar. C PgHdr freelist tahsis dizisi Rust modeline taşınmadı. `pagerUnlockIfUnused` içindeki `nMmapOut==0` bir assert'tir ve ölçülen üretim derlemesinde bir gcov dalı olarak çıkmaz. Kaynak eşlemesi ve hata sınırları [ana karar tablosunda](karar_esligi_v6b_34.md).
