# V6b.41 — Pager ERROR C dalları

SQLite 3.54.1 kapsama nesnesi, COMMIT hata matrisi ve dört C cacheflush akışıyla çalıştırıldı. Satır sayıları, amalgamdan C kaynak dosyalarına eşlenen ölçüm satırlarını kapsar. Doğrudan Pager C probu ayrı derlendi ve bu sayaçlara katılmadı.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:1821–1825` | pager_reset version bump | 5/5 | 0/0 |
| `pager.c:2968–2998` | hot-playback first cache reset | 9/13 | 8/10 |
| `pager.c:4530–4548` | page-one write and file version | 8/8 | 6/6 |
| `pager.c:5469–5488` | shared-lock file version check | 6/7 | 5/6 |
| `btree.c:4432–4458` | Btree phase-two version compensation | 14/14 | 7/8 |
| `pager.c:1890–1973` | pager_unlock | 27/29 | 16/22 |
| `pager.c:1996–2010` | pager_error | 7/7 | 4/4 |
| `pager.c:2090–2219` | pager_end_transaction | 37/45 | 39/56 |
| `pager.c:2240–2267` | pagerUnlockAndRollback | 8/20 | 10/14 |
| `pager.c:4102–4111` | pagerSyncHotJournal | 7/7 | 3/4 |
| `pager.c:4269–4273` | close journal sync | 3/3 | 2/2 |
| `pager.c:4663–4760` | pagerStress and sqlite3PagerFlush | 26/31 | 14/28 |
| `pager.c:5329–5446` | hot journal recovery at shared lock | 28/36 | 20/32 |
| `pager.c:5533–5538` | pagerUnlockIfUnused | 4/4 | 2/2 |
| `pager.c:5772–5784` | getPageError | 3/3 | 0/0 |
| `pager.c:6723–6748` | phase-one exit | 6/10 | 6/10 |
| `pager.c:6764–6802` | sqlite3PagerCommitPhaseTwo | 6/10 | 2/8 |
| `pager.c:6830–6874` | sqlite3PagerRollback | 10/17 | 8/16 |
| `vdbeaux.c:3420–3448` | VdbeHalt commit failure | 13/21 | 8/18 |
| `vdbe.c:4071–4095` | RELEASE restores autocommit | 8/18 | 7/18 |
| `main.c:1555–1559` | rollback hook transaction guard | 3/3 | 6/6 |
| `wal.c:4049–4065` | walWriteOneFrame | 7/7 | 2/2 |

272 dalın 175 tanesi alındı. Alınmayan 97 çıkış ayrı ayrı sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust yordamı |
|---|---|---|---|
| `btree.c:4434` | `if( p->inTrans==TRANS_NONE ) return SQLITE_OK;` | branch  0 taken 140 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4434` | `if( p->inTrans==TRANS_NONE ) return SQLITE_OK;` | branch  1 taken 587 | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4441` | `if( p->inTrans==TRANS_WRITE ){` | branch  0 taken 187 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4441` | `if( p->inTrans==TRANS_WRITE ){` | branch  1 taken 400 | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4447` | `if( rc!=SQLITE_OK && bCleanup==0 ){` | branch  0 taken 14 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4447` | `if( rc!=SQLITE_OK && bCleanup==0 ){` | branch  1 taken 173 | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4447` | `if( rc!=SQLITE_OK && bCleanup==0 ){` | branch  2 taken 14 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `btree.c:4447` | `if( rc!=SQLITE_OK && bCleanup==0 ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:5285` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  0 taken 146 (fallthrough) | `crates/sqliters-core/src/lib.rs:3580` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  1 taken 68 | `crates/sqliters-core/src/lib.rs:3580` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  2 taken 76 (fallthrough) | `crates/sqliters-core/src/lib.rs:3580` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  3 taken 70 | `crates/sqliters-core/src/lib.rs:3580` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  4 taken 16 (fallthrough) | `crates/sqliters-core/src/lib.rs:3580` |
| `main.c:1556` | `if( db->xRollbackCallback && (inTrans \|\| !db->autoCommit) ){` | branch  5 taken 60 | `crates/sqliters-core/src/lib.rs:3580` |
| `pager.c:1901` | `if( pagerUseWal(pPager) ){` | branch  0 taken 203 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1901` | `if( pagerUseWal(pPager) ){` | branch  1 taken 711 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1903` | `if( pPager->eState==PAGER_ERROR ){` | branch  0 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1903` | `if( pPager->eState==PAGER_ERROR ){` | branch  1 taken 202 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1914` | `}else if( !pPager->exclusiveMode ){` | branch  0 taken 635 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1914` | `}else if( !pPager->exclusiveMode ){` | branch  1 taken 76 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch  0 taken 567 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1916` | `int iDc = isOpen(pPager->fd)?sqlite3OsDeviceCharacteristics(pPager->fd):0;` | branch  1 taken 68 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1929` | `if( 0==(iDc & SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN)` | branch  1 taken 635 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1930` | `\|\| 1!=(pPager->journalMode & 5)` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch  1 taken 633 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch  2 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1941` | `if( rc!=SQLITE_OK && pPager->eState==PAGER_ERROR ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1959` | `if( pPager->errCode ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1959` | `if( pPager->errCode ){` | branch  1 taken 886 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1960` | `if( pPager->tempFile==0 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1960` | `if( pPager->tempFile==0 ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1967` | `if( USEFETCH(pPager) ) sqlite3OsUnfetch(pPager->fd, 0, 0);` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:1967` | `if( USEFETCH(pPager) ) sqlite3OsUnfetch(pPager->fd, 0, 0);` | branch  1 taken 28 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:2004` | `if( rc2==SQLITE_FULL \|\| rc2==SQLITE_IOERR ){` | branch  0 taken 248 (fallthrough) | `crates/sqliters-pager/src/lib.rs:729` |
| `pager.c:2004` | `if( rc2==SQLITE_FULL \|\| rc2==SQLITE_IOERR ){` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:729` |
| `pager.c:2004` | `if( rc2==SQLITE_FULL \|\| rc2==SQLITE_IOERR ){` | branch  2 taken 27 (fallthrough) | `crates/sqliters-pager/src/lib.rs:729` |
| `pager.c:2004` | `if( rc2==SQLITE_FULL \|\| rc2==SQLITE_IOERR ){` | branch  3 taken 221 | `crates/sqliters-pager/src/lib.rs:729` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  0 taken 649 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  1 taken 231 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  2 taken 559 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2109` | `if( pPager->eState<PAGER_WRITER_LOCKED && pPager->eLock<RESERVED_LOCK ){` | branch  3 taken 90 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2117` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 185 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2117` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 136 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2121` | `if( sqlite3JournalIsInMemory(pPager->jfd) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2121` | `if( sqlite3JournalIsInMemory(pPager->jfd) ){` | branch  2 taken 185 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2124` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_TRUNCATE ){` | branch  1 taken 185 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2125` | `if( pPager->journalOff==0 ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2129` | `if( rc==SQLITE_OK && pPager->fullSync ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  0 taken 185 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2139` | `}else if( pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  1 taken 181 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  2 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2140` | `\|\| (pPager->exclusiveMode && pPager->journalMode<PAGER_JOURNALMODE_WAL)` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2142` | `rc = zeroJournalHdr(pPager, hasSuper\|\|pPager->tempFile);` | branch  3 taken 4 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2157` | `if( bDelete ){` | branch  0 taken 181 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2157` | `if( bDelete ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2177` | `if( rc==SQLITE_OK ){` | branch  0 taken 305 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2177` | `if( rc==SQLITE_OK ){` | branch  1 taken 16 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  0 taken 305 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  3 taken 237 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  4 taken 68 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2186` | `if( pagerUseWal(pPager) ){` | branch  0 taken 68 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2186` | `if( pagerUseWal(pPager) ){` | branch  1 taken 253 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  0 taken 237 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  1 taken 16 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  2 taken 127 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  3 taken 110 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  4 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2193` | `}else if( rc==SQLITE_OK && bCommit && pPager->dbFileSize>pPager->dbSize ){` | branch  5 taken 127 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  0 taken 305 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  1 taken 16 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  2 taken 173 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2204` | `if( rc==SQLITE_OK && bCommit ){` | branch  3 taken 132 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2206` | `if( rc==SQLITE_NOTFOUND ) rc = SQLITE_OK;` | branch  0 taken 173 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2206` | `if( rc==SQLITE_NOTFOUND ) rc = SQLITE_OK;` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  0 taken 317 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2209` | `if( !pPager->exclusiveMode` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  0 taken 68 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  1 taken 249 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  3 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2210` | `&& (!pagerUseWal(pPager) \|\| sqlite3WalExclusiveMode(pPager->pWal, 0))` | branch  4 taken 68 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  0 taken 305 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2217` | `return (rc==SQLITE_OK?rc2:rc);` | branch  1 taken 16 | `crates/sqliters-pager/src/lib.rs:5312` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  0 taken 886 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  1 taken 26 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  2 taken 705 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2241` | `if( pPager->eState!=PAGER_ERROR && pPager->eState!=PAGER_OPEN ){` | branch  3 taken 181 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2243` | `if( pPager->eState>=PAGER_WRITER_LOCKED ){` | branch  1 taken 705 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch  0 taken 631 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2247` | `}else if( !pPager->exclusiveMode ){` | branch  1 taken 74 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch  0 taken 26 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2251` | `}else if( pPager->eState==PAGER_ERROR` | branch  1 taken 181 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2252` | `&& pPager->journalMode==PAGER_JOURNALMODE_MEMORY` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2252` | `&& pPager->journalMode==PAGER_JOURNALMODE_MEMORY` | branch  1 taken 26 | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2253` | `&& isOpen(pPager->jfd)` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2253` | `&& isOpen(pPager->jfd)` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:4922` |
| `pager.c:2973` | `for(u=0; u<nRec; u++){` | branch  0 taken 88 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2973` | `for(u=0; u<nRec; u++){` | branch  1 taken 40 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2974` | `if( needPagerReset ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2974` | `if( needPagerReset ){` | branch  1 taken 70 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2979` | `if( rc==SQLITE_OK ){` | branch  0 taken 80 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2979` | `if( rc==SQLITE_OK ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2982` | `if( rc==SQLITE_DONE ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2982` | `if( rc==SQLITE_DONE ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2985` | `}else if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:2985` | `}else if( rc==SQLITE_IOERR_SHORT_READ ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch  0 taken 24 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:4104` | `if( !pPager->noSync ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch  0 taken 22 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:4107` | `if( rc==SQLITE_OK ){` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:6357` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6170` |
| `pager.c:4269` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 204 | `crates/sqliters-pager/src/lib.rs:6170` |
| `pager.c:4530` | `if( pList->pgno==1 ) pager_write_changecounter(pList);` | branch  0 taken 165 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4530` | `if( pList->pgno==1 ) pager_write_changecounter(pList);` | branch  1 taken 128 | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4541` | `if( pgno==1 ){` | branch  0 taken 165 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4541` | `if( pgno==1 ){` | branch  1 taken 128 | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4544` | `if( pgno>pPager->dbFileSize ){` | branch  0 taken 123 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4544` | `if( pgno>pPager->dbFileSize ){` | branch  1 taken 170 | `crates/sqliters-pager/src/lib.rs:2174` |
| `pager.c:4686` | `if( NEVER(pPager->errCode) ) return SQLITE_OK;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4686` | `if( NEVER(pPager->errCode) ) return SQLITE_OK;` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4690` | `if( pPager->doNotSpill` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4690` | `if( pPager->doNotSpill` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4691` | `&& ((pPager->doNotSpill & (SPILLFLAG_ROLLBACK\|SPILLFLAG_OFF))!=0` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4691` | `&& ((pPager->doNotSpill & (SPILLFLAG_ROLLBACK\|SPILLFLAG_OFF))!=0` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4692` | `\|\| (pPg->flags & PGHDR_NEED_SYNC)!=0)` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4692` | `\|\| (pPg->flags & PGHDR_NEED_SYNC)!=0)` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4699` | `if( pagerUseWal(pPager) ){` | branch  0 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4699` | `if( pagerUseWal(pPager) ){` | branch  1 taken 3 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4702` | `if( rc==SQLITE_OK ){` | branch  0 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4702` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4715` | `if( pPg->flags&PGHDR_NEED_SYNC` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4715` | `if( pPg->flags&PGHDR_NEED_SYNC` | branch  1 taken 3 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4716` | `\|\| pPager->eState==PAGER_WRITER_CACHEMOD` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4716` | `\|\| pPager->eState==PAGER_WRITER_CACHEMOD` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4722` | `if( rc==SQLITE_OK ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4722` | `if( rc==SQLITE_OK ){` | branch  1 taken 1 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4729` | `if( rc==SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4729` | `if( rc==SQLITE_OK ){` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4742` | `if( !MEMDB ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4742` | `if( !MEMDB ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4745` | `while( rc==SQLITE_OK && pList ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4745` | `while( rc==SQLITE_OK && pList ){` | branch  1 taken 8 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4745` | `while( rc==SQLITE_OK && pList ){` | branch  2 taken 4 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4745` | `while( rc==SQLITE_OK && pList ){` | branch  3 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4747` | `if( pList->nRef==0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:4747` | `if( pList->nRef==0 ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:1899` |
| `pager.c:5335` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5335` | `if( rc!=SQLITE_OK ){` | branch  1 taken 478 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5343` | `if( pPager->eLock<=SHARED_LOCK ){` | branch  0 taken 474 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5343` | `if( pPager->eLock<=SHARED_LOCK ){` | branch  1 taken 4 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5346` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5346` | `if( rc!=SQLITE_OK ){` | branch  1 taken 478 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5349` | `if( bHotJournal ){` | branch  0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5349` | `if( bHotJournal ){` | branch  1 taken 458 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5350` | `if( pPager->readOnly ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5350` | `if( pPager->readOnly ){` | branch  1 taken 20 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5371` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5371` | `if( rc!=SQLITE_OK ){` | branch  1 taken 20 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  2 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5388` | `if( !isOpen(pPager->jfd) && pPager->journalMode!=PAGER_JOURNALMODE_OFF ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  2 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5393` | `if( rc==SQLITE_OK && bExists ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5399` | `if( rc==SQLITE_OK && fout&SQLITE_OPEN_READONLY ){` | branch  3 taken 18 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5414` | `if( isOpen(pPager->jfd) ){` | branch  0 taken 20 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5414` | `if( isOpen(pPager->jfd) ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5417` | `if( rc==SQLITE_OK ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5417` | `if( rc==SQLITE_OK ){` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5421` | `}else if( !pPager->exclusiveMode ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5421` | `}else if( !pPager->exclusiveMode ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5425` | `if( rc!=SQLITE_OK ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5425` | `if( rc!=SQLITE_OK ){` | branch  1 taken 18 | `crates/sqliters-pager/src/lib.rs:6314` |
| `pager.c:5473` | `if( rc!=SQLITE_OK ){` | branch  0 taken 72 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5473` | `if( rc!=SQLITE_OK ){` | branch  1 taken 264 | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5474` | `if( rc!=SQLITE_IOERR_SHORT_READ ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5474` | `if( rc!=SQLITE_IOERR_SHORT_READ ){` | branch  1 taken 72 | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5480` | `if( memcmp(pPager->dbFileVers, dbFileVers, sizeof(dbFileVers))!=0 ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5480` | `if( memcmp(pPager->dbFileVers, dbFileVers, sizeof(dbFileVers))!=0 ){` | branch  1 taken 320 | `crates/sqliters-pager/src/lib.rs:3145` |
| `pager.c:5534` | `if( sqlite3PcacheRefCount(pPager->pPCache)==0 ){` | branch  1 taken 704 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:5534` | `if( sqlite3PcacheRefCount(pPager->pPCache)==0 ){` | branch  2 taken 44 | `crates/sqliters-pager/src/lib.rs:4935` |
| `pager.c:6726` | `if( pPager->dbSize>pPager->dbFileSize ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6726` | `if( pPager->dbSize>pPager->dbFileSize ){` | branch  1 taken 145 | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6730` | `if( rc!=SQLITE_OK ) goto commit_phase_one_exit;` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6730` | `if( rc!=SQLITE_OK ) goto commit_phase_one_exit;` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6734` | `if( !noSync ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6734` | `if( !noSync ){` | branch  1 taken 145 | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6742` | `if( rc==SQLITE_OK && !pagerUseWal(pPager) ){` | branch  0 taken 187 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6742` | `if( rc==SQLITE_OK && !pagerUseWal(pPager) ){` | branch  1 taken 52 | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6742` | `if( rc==SQLITE_OK && !pagerUseWal(pPager) ){` | branch  2 taken 141 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6742` | `if( rc==SQLITE_OK && !pagerUseWal(pPager) ){` | branch  3 taken 46 | `crates/sqliters-pager/src/lib.rs:5052` |
| `pager.c:6770` | `if( NEVER(pPager->errCode) ) return pPager->errCode;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6770` | `if( NEVER(pPager->errCode) ) return pPager->errCode;` | branch  1 taken 187 | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6790` | `if( pPager->eState==PAGER_WRITER_LOCKED` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6790` | `if( pPager->eState==PAGER_WRITER_LOCKED` | branch  1 taken 187 | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6791` | `&& pPager->exclusiveMode` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6791` | `&& pPager->exclusiveMode` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6792` | `&& pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6792` | `&& pPager->journalMode==PAGER_JOURNALMODE_PERSIST` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5285` |
| `pager.c:6839` | `if( pPager->eState==PAGER_ERROR ) return pPager->errCode;` | branch  0 taken 18 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6839` | `if( pPager->eState==PAGER_ERROR ) return pPager->errCode;` | branch  1 taken 52 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6840` | `if( pPager->eState<=PAGER_READER ) return SQLITE_OK;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6840` | `if( pPager->eState<=PAGER_READER ) return SQLITE_OK;` | branch  1 taken 52 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6842` | `if( pagerUseWal(pPager) ){` | branch  0 taken 22 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6842` | `if( pagerUseWal(pPager) ){` | branch  1 taken 30 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6846` | `if( rc==SQLITE_OK ) rc = rc2;` | branch  0 taken 22 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6846` | `if( rc==SQLITE_OK ) rc = rc2;` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6847` | `}else if( !isOpen(pPager->jfd) \|\| pPager->eState==PAGER_WRITER_LOCKED ){` | branch  0 taken 30 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6847` | `}else if( !isOpen(pPager->jfd) \|\| pPager->eState==PAGER_WRITER_LOCKED ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6847` | `}else if( !isOpen(pPager->jfd) \|\| pPager->eState==PAGER_WRITER_LOCKED ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6847` | `}else if( !isOpen(pPager->jfd) \|\| pPager->eState==PAGER_WRITER_LOCKED ){` | branch  3 taken 30 | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6850` | `if( !MEMDB && eState>PAGER_WRITER_LOCKED ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6850` | `if( !MEMDB && eState>PAGER_WRITER_LOCKED ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6850` | `if( !MEMDB && eState>PAGER_WRITER_LOCKED ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:5424` |
| `pager.c:6850` | `if( !MEMDB && eState>PAGER_WRITER_LOCKED ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:5424` |
| `vdbe.c:4072` | `int isTransaction = pSavepoint->pNext==0 && db->isTransactionSavepoint;` | branch  0 taken 16 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4072` | `int isTransaction = pSavepoint->pNext==0 && db->isTransactionSavepoint;` | branch  1 taken 0 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4072` | `int isTransaction = pSavepoint->pNext==0 && db->isTransactionSavepoint;` | branch  2 taken 16 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4072` | `int isTransaction = pSavepoint->pNext==0 && db->isTransactionSavepoint;` | branch  3 taken 0 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4073` | `if( isTransaction && p1==SAVEPOINT_RELEASE ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4073` | `if( isTransaction && p1==SAVEPOINT_RELEASE ){` | branch  1 taken 0 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4073` | `if( isTransaction && p1==SAVEPOINT_RELEASE ){` | branch  2 taken 16 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4073` | `if( isTransaction && p1==SAVEPOINT_RELEASE ){` | branch  3 taken 0 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4074` | `if( (rc = sqlite3VdbeCheckFkDeferred(p))!=SQLITE_OK ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4074` | `if( (rc = sqlite3VdbeCheckFkDeferred(p))!=SQLITE_OK ){` | branch  2 taken 16 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4078` | `if( sqlite3VdbeHalt(p)==SQLITE_BUSY ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4078` | `if( sqlite3VdbeHalt(p)==SQLITE_BUSY ){` | branch  2 taken 16 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4085` | `if( rc ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4085` | `if( rc ){` | branch  1 taken 0 | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4093` | `if( p1==SAVEPOINT_ROLLBACK ){` | branch  0 never executed (fallthrough) | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4093` | `if( p1==SAVEPOINT_ROLLBACK ){` | branch  1 never executed | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4095` | `for(ii=0; ii<db->nDb; ii++){` | branch  0 never executed | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbe.c:4095` | `for(ii=0; ii<db->nDb; ii++){` | branch  1 never executed | `crates/sqliters-core/src/engine.rs:2350` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  1 taken 624 | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  3 taken 2 | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  4 never executed (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3420` | `if( p->rc==SQLITE_OK \|\| (p->errorAction==OE_Fail && !isSpecialError) ){` | branch  5 never executed | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3422` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3422` | `if( rc!=SQLITE_OK ){` | branch  1 taken 624 | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3423` | `if( NEVER(p->readOnly) ){` | branch  0 never executed (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3423` | `if( NEVER(p->readOnly) ){` | branch  1 never executed | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3428` | `}else if( db->flags & SQLITE_CorruptRdOnly ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3428` | `}else if( db->flags & SQLITE_CorruptRdOnly ){` | branch  1 taken 624 | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3438` | `if( rc==SQLITE_BUSY && p->readOnly ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3438` | `if( rc==SQLITE_BUSY && p->readOnly ){` | branch  1 taken 624 | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3438` | `if( rc==SQLITE_BUSY && p->readOnly ){` | branch  2 never executed (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3438` | `if( rc==SQLITE_BUSY && p->readOnly ){` | branch  3 never executed | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3441` | `}else if( rc!=SQLITE_OK ){` | branch  0 taken 66 (fallthrough) | `crates/sqliters-core/src/engine.rs:1396` |
| `vdbeaux.c:3441` | `}else if( rc!=SQLITE_OK ){` | branch  1 taken 558 | `crates/sqliters-core/src/engine.rs:1396` |
| `wal.c:4061` | `if( rc ) return rc;` | branch  0 taken 11 (fallthrough) | `crates/sqliters-pager/src/wal.rs:1464` |
| `wal.c:4061` | `if( rc ) return rc;` | branch  1 taken 81 | `crates/sqliters-pager/src/wal.rs:1464` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `btree.c:4447` | branch  3 taken 0 | Yapılandırma — super-journal sonrasındaki bCleanup sırasında faz iki hatası. |
| `pager.c:1929` | branch  0 taken 0 (fallthrough) | Platform — POSIX VFS, UNDELETABLE_WHEN_OPEN bildirmedi. |
| `pager.c:1930` | branch  0 never executed (fallthrough) | Platform ve journal yapılandırması — UNDELETABLE_WHEN_OPEN altında TRUNCATE/PERSIST tutma kararı. |
| `pager.c:1930` | branch  1 never executed | Platform ve journal yapılandırması — UNDELETABLE_WHEN_OPEN altında TRUNCATE/PERSIST tutma kararı. |
| `pager.c:1941` | branch  3 taken 0 | Bileşik hata — unlock hatası yalnız ERROR yolunda enjekte edildi. |
| `pager.c:1960` | branch  1 taken 0 | Yapılandırma — anonim TEMP hata önbelleği ve geçişi. |
| `pager.c:1967` | branch  0 taken 0 (fallthrough) | Yapılandırma — hata anında etkin xFetch haritası. Rust ekteki lease koruması koddan izlendi. |
| `pager.c:2121` | branch  1 taken 0 (fallthrough) | Yapılandırma — MEMORY journal kullanan işlem. |
| `pager.c:2124` | branch  0 taken 0 (fallthrough) | Yapılandırma — TRUNCATE journal. Ayrı C doğrudan pager probu hata sonucunu ölçer. |
| `pager.c:2125` | branch  0 never executed (fallthrough) | Yapılandırma — TRUNCATE journal, boş veya dolu journal ofseti. |
| `pager.c:2125` | branch  1 never executed | Yapılandırma — TRUNCATE journal, boş veya dolu journal ofseti. |
| `pager.c:2129` | branch  0 never executed (fallthrough) | Yapılandırma — TRUNCATE journal truncate sonucu ve takip eden FULL sync kararı. |
| `pager.c:2129` | branch  1 never executed | Yapılandırma — TRUNCATE journal truncate sonucu ve takip eden FULL sync kararı. |
| `pager.c:2129` | branch  2 never executed (fallthrough) | Yapılandırma — TRUNCATE journal truncate sonucu ve takip eden FULL sync kararı. |
| `pager.c:2129` | branch  3 never executed | Yapılandırma — TRUNCATE journal truncate sonucu ve takip eden FULL sync kararı. |
| `pager.c:2139` | branch  1 taken 0 | Yapılandırma — PERSIST journal. Ayrı C doğrudan pager probu hata sonucunu ölçer. |
| `pager.c:2140` | branch  3 taken 0 | Yapılandırma — exclusive journal sonlandırma koşulunun diğer çıkışı. |
| `pager.c:2142` | branch  1 taken 0 | Yapılandırma — zeroJournalHdr için super-journal veya özel TEMP koşulu. |
| `pager.c:2142` | branch  2 taken 0 (fallthrough) | Yapılandırma — zeroJournalHdr için super-journal veya özel TEMP koşulu. |
| `pager.c:2157` | branch  1 taken 0 | Yapılandırma — TEMP journal dosyasının unlink kararı. |
| `pager.c:2178` | branch  1 taken 0 | Yapılandırma — MEMDB ve TEMP commit önbelleği. |
| `pager.c:2193` | branch  4 taken 0 (fallthrough) | Girdi — commit sırasında fiziksel DB boyutunu küçülten işlem. |
| `pager.c:2206` | branch  1 taken 0 | VFS yeteneği — COMMIT_PHASETWO dosya denetiminin diğer dönüşü. |
| `pager.c:2210` | branch  3 taken 0 (fallthrough) | Yapılandırma — WAL exclusive kilit kipinden çıkışın diğer sonucu. |
| `pager.c:2243` | branch  0 taken 0 (fallthrough) | Yaşam döngüsü — ERROR dışı yazıcıyı son referansta koşulsuz geri alma. |
| `pager.c:2252` | branch  0 taken 0 (fallthrough) | Yapılandırma — ERROR ile MEMORY journal geri alma. |
| `pager.c:2253` | branch  0 never executed (fallthrough) | Yapılandırma — MEMORY journal tanıtıcısının açık veya kapalı olması. |
| `pager.c:2253` | branch  1 never executed | Yapılandırma — MEMORY journal tanıtıcısının açık veya kapalı olması. |
| `pager.c:2982` | branch  0 taken 0 (fallthrough) | Girdi — sıcak playback içinde koşullu kayıt sonu veya geçersiz checksum. |
| `pager.c:2985` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — sıcak playback kayıt okuması kısa kalır. |
| `pager.c:4104` | branch  1 taken 0 | Yapılandırma — noSync altında sıcak journal eşzamanlanmaz. |
| `pager.c:4686` | branch  0 taken 0 (fallthrough) | Savunma — ERROR sonrasında cache spill çağrısı. C flush girişinde saklı kodla döner. |
| `pager.c:4690` | branch  0 taken 0 (fallthrough) | Yapılandırma — spill engelleme bitleri. |
| `pager.c:4691` | branch  0 never executed (fallthrough) | Yapılandırma — rollback ve kullanıcı spill engelleme seçenekleri. |
| `pager.c:4691` | branch  1 never executed | Yapılandırma — rollback ve kullanıcı spill engelleme seçenekleri. |
| `pager.c:4692` | branch  0 never executed (fallthrough) | Yapılandırma — geçici NOSYNC ve sayfanın NEED_SYNC durumu. |
| `pager.c:4692` | branch  1 never executed | Yapılandırma — geçici NOSYNC ve sayfanın NEED_SYNC durumu. |
| `pager.c:4702` | branch  1 taken 0 | Hata enjeksiyonu — WAL alt journal aşaması. |
| `pager.c:4715` | branch  0 taken 0 (fallthrough) | Girdi — önceden eşzamanlanmış değişiklik sayfasının spill yolu. |
| `pager.c:4716` | branch  0 never executed (fallthrough) | Girdi — eşzamanlanmış sayfa ve WRITER_CACHEMOD birleşimleri. |
| `pager.c:4716` | branch  1 never executed | Girdi — eşzamanlanmış sayfa ve WRITER_CACHEMOD birleşimleri. |
| `pager.c:4729` | branch  0 taken 0 (fallthrough) | Pozitif kontrol kapsamı — ölçülen cacheflush vakalarının her biri hata enjekte eder. |
| `pager.c:4742` | branch  1 taken 0 | Yapılandırma — MEMDB cacheflush. |
| `pager.c:4745` | branch  3 taken 0 (fallthrough) | Girdi — birden çok dirty sayfada hatasız cacheflush sonu. |
| `pager.c:4747` | branch  1 taken 0 | Girdi — dirty sayfayı tutan etkin başvuru. |
| `pager.c:5335` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — shared kilit edinme hatası. |
| `pager.c:5346` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — hasHotJournal hatası. |
| `pager.c:5350` | branch  0 taken 0 (fallthrough) | Yapılandırma — salt okunur bağlantı sıcak journal bulur. |
| `pager.c:5371` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — EXCLUSIVE kilit yükseltmesi başarısız. |
| `pager.c:5388` | branch  3 taken 0 | Yapılandırma — OFF journal kipinde sıcak dosya olasılığı. |
| `pager.c:5393` | branch  1 taken 0 | Yarış ve hata enjeksiyonu — EXCLUSIVE altında yeniden access sonucu veya dosyanın kaybolması. |
| `pager.c:5393` | branch  3 taken 0 | Yarış ve hata enjeksiyonu — EXCLUSIVE altında yeniden access sonucu veya dosyanın kaybolması. |
| `pager.c:5399` | branch  1 taken 0 | VFS yeteneği — sıcak journal RW açılışının başarısız ya da salt okunur dönmesi. |
| `pager.c:5399` | branch  2 taken 0 (fallthrough) | VFS yeteneği — sıcak journal RW açılışının başarısız ya da salt okunur dönmesi. |
| `pager.c:5414` | branch  1 taken 0 | Yarış — sıcak journal tanıtıcısı açılmadan kaybolur. |
| `pager.c:5421` | branch  0 never executed (fallthrough) | Yarış ve yapılandırma — journal yokken normal/exclusive kilit farkı. |
| `pager.c:5421` | branch  1 never executed | Yarış ve yapılandırma — journal yokken normal/exclusive kilit farkı. |
| `pager.c:5474` | branch  0 taken 0 (fallthrough) | Hata enjeksiyonu — dosya sürümü alanının kısa okuma çıkışı. |
| `pager.c:6726` | branch  0 taken 0 (fallthrough) | Girdi — diskteki DB boyutunun ötesine büyüme. |
| `pager.c:6730` | branch  0 never executed (fallthrough) | Hata enjeksiyonu — büyüme sırasında fiziksel truncate. |
| `pager.c:6730` | branch  1 never executed | Hata enjeksiyonu — büyüme sırasında fiziksel truncate. |
| `pager.c:6734` | branch  0 taken 0 (fallthrough) | Yapılandırma — synchronous=OFF altında commit. |
| `pager.c:6770` | branch  0 taken 0 (fallthrough) | Savunma — kalıcı hata varken faz ikiye doğrudan giriş. |
| `pager.c:6790` | branch  0 taken 0 (fallthrough) | Girdi — değişikliksiz yazıcıyla faz iki. |
| `pager.c:6791` | branch  0 never executed (fallthrough) | Yapılandırma — değişikliksiz yazıcı ve exclusive kilit. |
| `pager.c:6791` | branch  1 never executed | Yapılandırma — değişikliksiz yazıcı ve exclusive kilit. |
| `pager.c:6792` | branch  0 never executed (fallthrough) | Yapılandırma — değişikliksiz exclusive PERSIST optimizasyonu. |
| `pager.c:6792` | branch  1 never executed | Yapılandırma — değişikliksiz exclusive PERSIST optimizasyonu. |
| `pager.c:6840` | branch  0 taken 0 (fallthrough) | Girdi — açık yazıcı olmadan doğrudan PagerRollback. |
| `pager.c:6846` | branch  1 taken 0 | Hata enjeksiyonu — WAL undo veya sonlandırma sırasında hata. |
| `pager.c:6847` | branch  1 taken 0 | Yapılandırma ve girdi — journalsız veya değişikliksiz yazıcıda geri alma. |
| `pager.c:6847` | branch  2 taken 0 (fallthrough) | Yapılandırma ve girdi — journalsız veya değişikliksiz yazıcıda geri alma. |
| `pager.c:6850` | branch  0 never executed (fallthrough) | Yapılandırma — MEMDB ve journal_mode=OFF için SQLITE_ABORT istisnası. |
| `pager.c:6850` | branch  1 never executed | Yapılandırma — MEMDB ve journal_mode=OFF için SQLITE_ABORT istisnası. |
| `pager.c:6850` | branch  2 never executed (fallthrough) | Yapılandırma — MEMDB ve journal_mode=OFF için SQLITE_ABORT istisnası. |
| `pager.c:6850` | branch  3 never executed | Yapılandırma — MEMDB ve journal_mode=OFF için SQLITE_ABORT istisnası. |
| `vdbe.c:4072` | branch  1 taken 0 | Girdi — üst düzey işlem olmayan veya iç içe savepoint. |
| `vdbe.c:4072` | branch  3 taken 0 | Girdi — üst düzey işlem olmayan veya iç içe savepoint. |
| `vdbe.c:4073` | branch  1 taken 0 | Girdi — SAVEPOINT RELEASE dışı işlem. |
| `vdbe.c:4073` | branch  3 taken 0 | Girdi — SAVEPOINT RELEASE dışı işlem. |
| `vdbe.c:4074` | branch  1 taken 0 (fallthrough) | Girdi — ertelenmiş FK ihlali. |
| `vdbe.c:4078` | branch  1 taken 0 (fallthrough) | Rekabet — RELEASE sırasında SQLITE_BUSY. |
| `vdbe.c:4085` | branch  1 taken 0 | Pozitif kontrol — ölçülen dış RELEASE vakalarının her biri hata enjekte eder. |
| `vdbe.c:4093` | branch  0 never executed (fallthrough) | Girdi — iç ROLLBACK TO veya iç RELEASE. |
| `vdbe.c:4093` | branch  1 never executed | Girdi — iç ROLLBACK TO veya iç RELEASE. |
| `vdbe.c:4095` | branch  0 never executed | Girdi — birden çok bağlı DB ile savepoint geri alma. |
| `vdbe.c:4095` | branch  1 never executed | Girdi — birden çok bağlı DB ile savepoint geri alma. |
| `vdbeaux.c:3420` | branch  2 taken 0 (fallthrough) | Girdi — OR FAIL ile kısmi ifade veya önceden oluşmuş özel hata. |
| `vdbeaux.c:3420` | branch  4 never executed (fallthrough) | Girdi — OR FAIL ile kısmi ifade veya önceden oluşmuş özel hata. |
| `vdbeaux.c:3420` | branch  5 never executed | Girdi — OR FAIL ile kısmi ifade veya önceden oluşmuş özel hata. |
| `vdbeaux.c:3422` | branch  0 taken 0 (fallthrough) | Girdi — ertelenmiş FK ihlali. |
| `vdbeaux.c:3423` | branch  0 never executed (fallthrough) | Savunma — readonly VDBE ve ertelenmiş FK ihlali. |
| `vdbeaux.c:3423` | branch  1 never executed | Savunma — readonly VDBE ve ertelenmiş FK ihlali. |
| `vdbeaux.c:3428` | branch  0 taken 0 (fallthrough) | Girdi — CorruptRdOnly bayrağı. |
| `vdbeaux.c:3438` | branch  0 taken 0 (fallthrough) | Rekabet — commit BUSY ve readOnly birleşimleri. |
| `vdbeaux.c:3438` | branch  2 never executed (fallthrough) | Rekabet — commit BUSY ve readOnly birleşimleri. |
| `vdbeaux.c:3438` | branch  3 never executed | Rekabet — commit BUSY ve readOnly birleşimleri. |
