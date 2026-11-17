# V6b.42 — EXCLUSIVE DELETE ve WAL C dalları

SQLite 3.54.1 kapsama nesnesi 88 native akış, C cacheflush sonrası checksum yeniden yazımları ve üç sıcak journal örneğiyle çalıştırıldı. Satır sayıları amalgamdan kaynak dosyalarına eşlenen ölçüm satırlarını kapsar.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:1436–1468` | zeroJournalHdr | 14/14 | 16/18 |
| `pager.c:2090–2159` | journal finalization in pager_end_transaction | 17/24 | 15/28 |
| `pager.c:2240–2251` | unlock last reader | 6/9 | 9/10 |
| `pager.c:2920–2961` | hot journal record count | 12/12 | 12/16 |
| `pager.c:4102–4110` | pagerSyncHotJournal | 7/7 | 4/4 |
| `pager.c:4269–4273` | close journal sync | 3/3 | 2/2 |
| `vdbe.c:4177–4185` | OP_AutoCommit rollback | 5/5 | 6/8 |
| `wal.c:4024–4044` | walWriteToLog | 12/12 | 8/10 |
| `wal.c:4049–4065` | walWriteOneFrame | 7/7 | 2/2 |
| `wal.c:4075–4117` | walRewriteChecksums | 23/23 | 7/10 |
| `wal.c:4223–4336` | frame append / rewrite / padding / index | 49/54 | 51/60 |

168 dalın 132 tanesi alındı. Alınmayan 36 çıkış ayrı ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust yordamı |
|---|---|---|---|
| `pager.c:1440` | `if( pPager->journalOff ){` | branch  0 taken 21 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1440` | `if( pPager->journalOff ){` | branch  1 taken 3 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  0 taken 21 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  2 taken 5 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1444` | `if( doTruncate \|\| iLimit==0 ){` | branch  3 taken 16 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  1 taken 3 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  2 taken 17 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1450` | `if( rc==SQLITE_OK && !pPager->noSync ){` | branch  3 taken 1 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  1 taken 5 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  2 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1460` | `if( rc==SQLITE_OK && iLimit>0 ){` | branch  3 taken 14 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  2 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:1463` | `if( rc==SQLITE_OK && sz>iLimit ){` | branch  3 taken 1 | `crates/sqliters-pager/src/lib.rs:5753` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  0 taken 917 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  1 taken 350 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  2 taken 785 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  3 taken 132 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2117` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 207 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2117` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 275 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2121` | `if( sqlite3JournalIsInMemory(pPager->jfd) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2121` | `if( sqlite3JournalIsInMemory(pPager->jfd) ){` | branch  2 taken 207 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  1 taken 207 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  0 taken 207 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  1 taken 183 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  2 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  3 taken 24 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2157` | `if( bDelete ){` | branch  0 taken 183 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2157` | `if( bDelete ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5718` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  0 taken 1310 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  1 taken 20 | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  2 taken 1032 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  3 taken 278 | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch  1 taken 1032 | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch  0 taken 901 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch  1 taken 131 | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch  0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch  1 taken 278 | `crates/sqliters-pager/src/lib.rs:5684` |
| `pager.c:2920` | `if( rc!=SQLITE_OK ){` | branch  0 taken 31 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2920` | `if( rc!=SQLITE_OK ){` | branch  1 taken 32 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2921` | `if( rc==SQLITE_DONE ){` | branch  0 taken 31 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2921` | `if( rc==SQLITE_DONE ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2932` | `if( nRec==0xffffffff ){` | branch  0 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2932` | `if( nRec==0xffffffff ){` | branch  1 taken 31 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  1 taken 24 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  2 taken 7 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  3 taken 1 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2952` | `pPager->journalHdr+JOURNAL_HDR_SZ(pPager)==pPager->journalOff ){` | branch  0 taken 7 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2952` | `pPager->journalHdr+JOURNAL_HDR_SZ(pPager)==pPager->journalOff ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2959` | `if( pPager->journalOff==JOURNAL_HDR_SZ(pPager) ){` | branch  0 taken 32 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2959` | `if( pPager->journalOff==JOURNAL_HDR_SZ(pPager) ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2961` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:2961` | `if( rc!=SQLITE_OK ){` | branch  1 taken 32 | `crates/sqliters-pager/src/lib.rs:6530` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch  0 taken 33 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6612` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:6612` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch  0 taken 29 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6612` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch  1 taken 5 | `crates/sqliters-pager/src/lib.rs:6612` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 17 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6214` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 273 | `crates/sqliters-pager/src/lib.rs:6214` |
| `vdbe.c:4179` | `if( desiredAutoCommit!=db->autoCommit ){` | branch  0 taken 158 (fallthrough) | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4179` | `if( desiredAutoCommit!=db->autoCommit ){` | branch  1 taken 0 | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4180` | `if( iRollback ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4180` | `if( iRollback ){` | branch  1 taken 154 | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4184` | `}else if( desiredAutoCommit && db->nVdbeWrite>0 ){` | branch  0 taken 75 (fallthrough) | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4184` | `}else if( desiredAutoCommit && db->nVdbeWrite>0 ){` | branch  1 taken 79 | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4184` | `}else if( desiredAutoCommit && db->nVdbeWrite>0 ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:1376` |
| `vdbe.c:4184` | `}else if( desiredAutoCommit && db->nVdbeWrite>0 ){` | branch  3 taken 75 | `crates/sqliters-core/src/engine.rs:1376` |
| `wal.c:4031` | `if( iOffset<p->iSyncPoint && iOffset+iAmt>=p->iSyncPoint ){` | branch  0 taken 232 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4031` | `if( iOffset<p->iSyncPoint && iOffset+iAmt>=p->iSyncPoint ){` | branch  1 taken 506 | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4031` | `if( iOffset<p->iSyncPoint && iOffset+iAmt>=p->iSyncPoint ){` | branch  2 taken 58 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4031` | `if( iOffset<p->iSyncPoint && iOffset+iAmt>=p->iSyncPoint ){` | branch  3 taken 174 | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4034` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4034` | `if( rc ) return rc;` | branch  1 taken 58 | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4040` | `if( iAmt==0 \|\| rc ) return rc;` | branch  0 taken 58 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4040` | `if( iAmt==0 \|\| rc ) return rc;` | branch  1 taken 0 | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4040` | `if( iAmt==0 \|\| rc ) return rc;` | branch  2 taken 5 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4040` | `if( iAmt==0 \|\| rc ) return rc;` | branch  3 taken 53 | `crates/sqliters-pager/src/wal.rs:1710` |
| `wal.c:4061` | `if( rc ) return rc;` | branch  0 taken 14 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4061` | `if( rc ) return rc;` | branch  1 taken 362 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4084` | `if( aBuf==0 ) return SQLITE_NOMEM_BKPT;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4084` | `if( aBuf==0 ) return SQLITE_NOMEM_BKPT;` | branch  1 taken 10 | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4092` | `if( pWal->iReCksum==1 ){` | branch  0 taken 5 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4092` | `if( pWal->iReCksum==1 ){` | branch  1 taken 5 | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4103` | `for(; rc==SQLITE_OK && iRead<=iLast; iRead++){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4103` | `for(; rc==SQLITE_OK && iRead<=iLast; iRead++){` | branch  1 taken 0 | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4103` | `for(; rc==SQLITE_OK && iRead<=iLast; iRead++){` | branch  2 taken 30 | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4103` | `for(; rc==SQLITE_OK && iRead<=iLast; iRead++){` | branch  3 taken 10 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4106` | `if( rc==SQLITE_OK ){` | branch  0 taken 30 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4106` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/wal.rs:1732` |
| `wal.c:4226` | `for(p=pList; p; p=p->pDirty){` | branch  0 taken 270 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4226` | `for(p=pList; p; p=p->pDirty){` | branch  1 taken 168 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  0 taken 30 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  1 taken 240 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  2 taken 20 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  3 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  4 taken 10 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4233` | `if( iFirst && (p->pDirty \|\| isCommit==0) ){` | branch  5 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4237` | `if( iWrite>=iFirst ){` | branch  0 taken 10 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4237` | `if( iWrite>=iFirst ){` | branch  1 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4240` | `if( pWal->iReCksum==0 \|\| iWrite<pWal->iReCksum ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4240` | `if( pWal->iReCksum==0 \|\| iWrite<pWal->iReCksum ){` | branch  1 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4240` | `if( pWal->iReCksum==0 \|\| iWrite<pWal->iReCksum ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4240` | `if( pWal->iReCksum==0 \|\| iWrite<pWal->iReCksum ){` | branch  3 never executed | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4245` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4245` | `if( rc ) return rc;` | branch  1 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4253` | `nDbSize = (isCommit && p->pDirty==0) ? nTruncate : 0;` | branch  0 taken 240 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4253` | `nDbSize = (isCommit && p->pDirty==0) ? nTruncate : 0;` | branch  1 taken 20 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4253` | `nDbSize = (isCommit && p->pDirty==0) ? nTruncate : 0;` | branch  2 taken 170 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4253` | `nDbSize = (isCommit && p->pDirty==0) ? nTruncate : 0;` | branch  3 taken 70 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4255` | `if( rc ) return rc;` | branch  0 taken 26 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4255` | `if( rc ) return rc;` | branch  1 taken 234 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4262` | `if( isCommit && pWal->iReCksum ){` | branch  0 taken 148 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4262` | `if( isCommit && pWal->iReCksum ){` | branch  1 taken 20 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4262` | `if( isCommit && pWal->iReCksum ){` | branch  2 taken 10 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4262` | `if( isCommit && pWal->iReCksum ){` | branch  3 taken 138 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4264` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4264` | `if( rc ) return rc;` | branch  1 taken 10 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4281` | `if( isCommit && WAL_SYNC_FLAGS(sync_flags)!=0 ){` | branch  0 taken 148 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4281` | `if( isCommit && WAL_SYNC_FLAGS(sync_flags)!=0 ){` | branch  1 taken 20 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4281` | `if( isCommit && WAL_SYNC_FLAGS(sync_flags)!=0 ){` | branch  2 taken 148 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4281` | `if( isCommit && WAL_SYNC_FLAGS(sync_flags)!=0 ){` | branch  3 taken 0 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4283` | `if( pWal->padToSectorBoundary ){` | branch  0 taken 58 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4283` | `if( pWal->padToSectorBoundary ){` | branch  1 taken 90 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4288` | `while( iOffset<w.iSyncPoint ){` | branch  0 taken 116 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4288` | `while( iOffset<w.iSyncPoint ){` | branch  1 taken 53 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4290` | `if( rc ) return rc;` | branch  0 taken 5 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4290` | `if( rc ) return rc;` | branch  1 taken 111 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4296` | `if( bSync ){` | branch  0 taken 90 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4296` | `if( bSync ){` | branch  1 taken 53 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  0 taken 143 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  1 taken 20 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  2 taken 63 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  3 taken 80 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  4 taken 0 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4306` | `if( isCommit && pWal->truncateOnCommit && pWal->mxWalSize>=0 ){` | branch  5 taken 63 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4308` | `if( walFrameOffset(iFrame+nExtra+1, szPage)>pWal->mxWalSize ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4308` | `if( walFrameOffset(iFrame+nExtra+1, szPage)>pWal->mxWalSize ){` | branch  1 never executed | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4321` | `for(p=pList; p && rc==SQLITE_OK; p=p->pDirty){` | branch  0 taken 237 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4321` | `for(p=pList; p && rc==SQLITE_OK; p=p->pDirty){` | branch  1 taken 156 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4321` | `for(p=pList; p && rc==SQLITE_OK; p=p->pDirty){` | branch  2 taken 230 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4321` | `for(p=pList; p && rc==SQLITE_OK; p=p->pDirty){` | branch  3 taken 7 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4322` | `if( (p->flags & PGHDR_WAL_APPEND)==0 ) continue;` | branch  0 taken 10 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4322` | `if( (p->flags & PGHDR_WAL_APPEND)==0 ) continue;` | branch  1 taken 220 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4327` | `while( rc==SQLITE_OK && nExtra>0 ){` | branch  0 taken 262 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4327` | `while( rc==SQLITE_OK && nExtra>0 ){` | branch  1 taken 7 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4327` | `while( rc==SQLITE_OK && nExtra>0 ){` | branch  2 taken 106 | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4327` | `while( rc==SQLITE_OK && nExtra>0 ){` | branch  3 taken 156 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4333` | `if( rc==SQLITE_OK ){` | branch  0 taken 156 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1460` |
| `wal.c:4333` | `if( rc==SQLITE_OK ){` | branch  1 taken 7 | `crates/sqliters-pager/src/wal.rs:1460` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `pager.c:1444` | branch  1 taken 0 | Yapılandırma — TEMP veya super-journal nedeniyle zorunlu truncate. |
| `pager.c:1463` | branch  1 taken 0 | Hata enjeksiyonu — pozitif journal_size_limit kontrolünde xFileSize hatası. |
| `pager.c:2121` | branch  1 taken 0 (fallthrough) | Yapılandırma — bellek içi rollback journal. |
| `pager.c:2124` | branch  0 taken 0 (fallthrough) | Yapılandırma — açık TRUNCATE journal. |
| `pager.c:2125` | branch  0 never executed (fallthrough) | Yapılandırma — TRUNCATE journal ofseti boş veya dolu. |
| `pager.c:2125` | branch  1 never executed | Yapılandırma — TRUNCATE journal ofseti boş veya dolu. |
| `pager.c:2129` | branch  0 never executed (fallthrough) | Yapılandırma ve hata enjeksiyonu — TRUNCATE, xTruncate sonucu ve FULL sync. |
| `pager.c:2129` | branch  1 never executed | Yapılandırma ve hata enjeksiyonu — TRUNCATE, xTruncate sonucu ve FULL sync. |
| `pager.c:2129` | branch  2 never executed (fallthrough) | Yapılandırma ve hata enjeksiyonu — TRUNCATE, xTruncate sonucu ve FULL sync. |
| `pager.c:2129` | branch  3 never executed | Yapılandırma ve hata enjeksiyonu — TRUNCATE, xTruncate sonucu ve FULL sync. |
| `pager.c:2139` | branch  1 taken 0 | Yapılandırma — açık PERSIST journal. |
| `pager.c:2140` | branch  3 taken 0 | Yapılandırma — EXCLUSIVE ile WAL veya diğer rollback kipleri. |
| `pager.c:2142` | branch  1 taken 0 | Yapılandırma — TEMP ya da super-journal ile başlığı truncate etme. |
| `pager.c:2142` | branch  2 taken 0 (fallthrough) | Yapılandırma — TEMP ya da super-journal ile başlığı truncate etme. |
| `pager.c:2157` | branch  1 taken 0 | Yapılandırma — TEMP journal unlink atlama. |
| `pager.c:2243` | branch  0 taken 0 (fallthrough) | Yaşam döngüsü — son okuyucu yerine etkin yazıcıyı kapatma. |
| `pager.c:2921` | branch  1 taken 0 | Hata enjeksiyonu — sıcak bölüm başlığı okumasının SQLITE_DONE dışındaki hatası. |
| `pager.c:2952` | branch  1 taken 0 | Girdi — sıcak olmayan playback sırasında nRec sıfır ve bölüm son başlık değil. |
| `pager.c:2959` | branch  1 taken 0 | Girdi — birden çok sektör hizalı sıcak bölüm. |
| `pager.c:2961` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — sıcak playback sırasında ana DB truncate hatası. |
| `vdbe.c:4179` | branch  1 taken 0 | Girdi — zaten autocommit durumunda işlem komutu. |
| `vdbe.c:4184` | branch  2 taken 0 (fallthrough) | Rekabet — COMMIT sırasında başka etkin yazıcı. |
| `wal.c:4034` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — sync sınırına kesilen ilk parçanın yazımı. |
| `wal.c:4040` | branch  1 taken 0 | Hata enjeksiyonu — bölünmüş yazımın ara xSync hatası. |
| `wal.c:4084` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — checksum yeniden yazma tamponu tahsisi. |
| `wal.c:4103` | branch  1 taken 0 | Hata enjeksiyonu — checksum başlangıç değerinin xRead hatası. |
| `wal.c:4106` | branch  1 taken 0 | Hata enjeksiyonu — checksum için tam frame xRead hatası. |
| `wal.c:4240` | branch  0 taken 0 (fallthrough) | Girdi — aynı işlemde birden fazla frame overwrite ve en küçük iReCksum. |
| `wal.c:4240` | branch  2 never executed (fallthrough) | Girdi — aynı işlemde birden fazla frame overwrite ve en küçük iReCksum. |
| `wal.c:4240` | branch  3 never executed | Girdi — aynı işlemde birden fazla frame overwrite ve en küçük iReCksum. |
| `wal.c:4245` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — WAL içindeki önceki sayfa üzerine yazma. |
| `wal.c:4264` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — checksum zincirinin yeniden yazımı başarısız. |
| `wal.c:4281` | branch  3 taken 0 | Yapılandırma — WAL commit sync kapalı. |
| `wal.c:4306` | branch  4 taken 0 (fallthrough) | Yapılandırma — WAL restart sonrası sonlu journal_size_limit. |
| `wal.c:4308` | branch  0 never executed (fallthrough) | Yapılandırma — WAL sonlandırma boyutunun journal_size_limit karşısındaki iki sonucu. |
| `wal.c:4308` | branch  1 never executed | Yapılandırma — WAL sonlandırma boyutunun journal_size_limit karşısındaki iki sonucu. |
