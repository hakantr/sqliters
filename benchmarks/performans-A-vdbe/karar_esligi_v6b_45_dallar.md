# V6b.45 — Sıcak journal başlığı C dalları

SQLite 3.54.1 kapsama nesnesi 203 native vaka ile çalıştırıldı. Satırlar amalgamdan `sqlite/src/pager.c` kaynağına eşlendi.

| C kapsamı | Blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pager.c:1338–1386` | readSuperJournal | 26/27 | 29/32 |
| `pager.c:1627–1730` | readJournalHdr | 29/29 | 33/36 |
| `pager.c:2889–2968` | pager_playback başlangıcı, nRec ve DB boyutu | 23/27 | 23/28 |

96 dalın 85'i alındı. Karşılaştırma aşağıdaki Rust karar noktalarını ve belirtilen native kapsamı kullanır.

| C yeri | Koşul | Gcov çıkışı | İncelenen Rust karar noktası |
|---|---|---|---|
| `pager.c:1347` | `if( SQLITE_OK!=(rc = sqlite3OsFileSize(pJrnl, &szJ))` | branch  1 taken 281 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1347` | `if( SQLITE_OK!=(rc = sqlite3OsFileSize(pJrnl, &szJ))` | branch  2 taken 1 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1348` | `\|\| szJ<16` | branch  0 taken 277 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1348` | `\|\| szJ<16` | branch  1 taken 4 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1349` | `\|\| SQLITE_OK!=(rc = read32bits(pJrnl, szJ-16, &len))` | branch  1 taken 275 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1349` | `\|\| SQLITE_OK!=(rc = read32bits(pJrnl, szJ-16, &len))` | branch  2 taken 2 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1350` | `\|\| len>=nSuper` | branch  0 taken 265 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1350` | `\|\| len>=nSuper` | branch  1 taken 10 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1351` | `\|\| len>szJ-16` | branch  0 taken 261 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1351` | `\|\| len>szJ-16` | branch  1 taken 4 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1352` | `\|\| len==0` | branch  0 taken 33 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1352` | `\|\| len==0` | branch  1 taken 228 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1353` | `\|\| SQLITE_OK!=(rc = read32bits(pJrnl, szJ-12, &cksum))` | branch  1 taken 31 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1353` | `\|\| SQLITE_OK!=(rc = read32bits(pJrnl, szJ-12, &cksum))` | branch  2 taken 2 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1354` | `\|\| SQLITE_OK!=(rc = sqlite3OsRead(pJrnl, aMagic, 8, szJ-8))` | branch  1 taken 2 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1354` | `\|\| SQLITE_OK!=(rc = sqlite3OsRead(pJrnl, aMagic, 8, szJ-8))` | branch  2 taken 29 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1360` | `if( !zOut ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1360` | `if( !zOut ){` | branch  1 taken 29 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1361` | `rc = memcmp(aMagic,aJournalMagic,8) ? SQLITE_OK : SQLITE_NOMEM_BKPT;` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1361` | `rc = memcmp(aMagic,aJournalMagic,8) ? SQLITE_OK : SQLITE_NOMEM_BKPT;` | branch  1 never executed | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1364` | `if( SQLITE_OK==(rc = sqlite3OsRead(pJrnl, zOut, len, szJ-16-len)) ){` | branch  1 taken 27 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1364` | `if( SQLITE_OK==(rc = sqlite3OsRead(pJrnl, zOut, len, szJ-16-len)) ){` | branch  2 taken 2 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1367` | `for(u=0; u<len; u++){` | branch  0 taken 1272 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1367` | `for(u=0; u<len; u++){` | branch  1 taken 27 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1371` | `if( rc!=SQLITE_OK                        /* Couldn't read the name */` | branch  0 taken 27 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1371` | `if( rc!=SQLITE_OK                        /* Couldn't read the name */` | branch  1 taken 2 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1372` | `\|\| !pagerIsSuperJrnlName(zOut)          /* Name is not valid */` | branch  1 taken 24 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1372` | `\|\| !pagerIsSuperJrnlName(zOut)          /* Name is not valid */` | branch  2 taken 3 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1373` | `\|\| cksum                                /* checksum is incorrect */` | branch  0 taken 21 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1373` | `\|\| cksum                                /* checksum is incorrect */` | branch  1 taken 3 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1374` | `\|\| memcmp(aMagic, aJournalMagic, 8)!=0  /* Bad magic number */` | branch  0 taken 3 (fallthrough) | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1374` | `\|\| memcmp(aMagic, aJournalMagic, 8)!=0  /* Bad magic number */` | branch  1 taken 18 | `crates/sqliters-pager/src/super_journal.rs:136` |
| `pager.c:1645` | `if( pPager->journalOff+JOURNAL_HDR_SZ(pPager) > journalSize ){` | branch  0 taken 159 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1645` | `if( pPager->journalOff+JOURNAL_HDR_SZ(pPager) > journalSize ){` | branch  1 taken 273 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1655` | `if( isHot \|\| iHdrOff!=pPager->journalHdr ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1655` | `if( isHot \|\| iHdrOff!=pPager->journalHdr ){` | branch  1 taken 273 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1655` | `if( isHot \|\| iHdrOff!=pPager->journalHdr ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1655` | `if( isHot \|\| iHdrOff!=pPager->journalHdr ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1657` | `if( rc ){` | branch  0 taken 10 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1657` | `if( rc ){` | branch  1 taken 263 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1660` | `if( memcmp(aMagic, aJournalMagic, sizeof(aMagic))!=0 ){` | branch  0 taken 7 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1660` | `if( memcmp(aMagic, aJournalMagic, sizeof(aMagic))!=0 ){` | branch  1 taken 256 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1669` | `if( SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+8, pNRec))` | branch  1 taken 237 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1669` | `if( SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+8, pNRec))` | branch  2 taken 19 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1670` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+12, &pPager->cksumInit))` | branch  1 taken 227 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1670` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+12, &pPager->cksumInit))` | branch  2 taken 10 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1671` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+16, pDbSize))` | branch  1 taken 10 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1671` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+16, pDbSize))` | branch  2 taken 217 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1676` | `if( pPager->journalOff==0 ){` | branch  0 taken 203 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1676` | `if( pPager->journalOff==0 ){` | branch  1 taken 14 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1681` | `if( SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+20, &iSectorSize))` | branch  1 taken 195 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1681` | `if( SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+20, &iSectorSize))` | branch  2 taken 8 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1682` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+24, &iPageSize))` | branch  1 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1682` | `\|\| SQLITE_OK!=(rc = read32bits(pPager->jfd, iHdrOff+24, &iPageSize))` | branch  2 taken 187 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1691` | `if( iPageSize==0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1691` | `if( iPageSize==0 ){` | branch  1 taken 183 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1700` | `if( iPageSize<512                  \|\| iSectorSize<32` | branch  0 taken 185 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1700` | `if( iPageSize<512                  \|\| iSectorSize<32` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1700` | `if( iPageSize<512                  \|\| iSectorSize<32` | branch  2 taken 182 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1700` | `if( iPageSize<512                  \|\| iSectorSize<32` | branch  3 taken 3 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1701` | `\|\| iPageSize>SQLITE_MAX_PAGE_SIZE \|\| iSectorSize>MAX_SECTOR_SIZE` | branch  0 taken 180 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1701` | `\|\| iPageSize>SQLITE_MAX_PAGE_SIZE \|\| iSectorSize>MAX_SECTOR_SIZE` | branch  1 taken 2 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1701` | `\|\| iPageSize>SQLITE_MAX_PAGE_SIZE \|\| iSectorSize>MAX_SECTOR_SIZE` | branch  2 taken 178 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1701` | `\|\| iPageSize>SQLITE_MAX_PAGE_SIZE \|\| iSectorSize>MAX_SECTOR_SIZE` | branch  3 taken 2 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1702` | `\|\| ((iPageSize-1)&iPageSize)!=0   \|\| ((iSectorSize-1)&iSectorSize)!=0` | branch  0 taken 175 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1702` | `\|\| ((iPageSize-1)&iPageSize)!=0   \|\| ((iSectorSize-1)&iSectorSize)!=0` | branch  1 taken 3 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1702` | `\|\| ((iPageSize-1)&iPageSize)!=0   \|\| ((iSectorSize-1)&iSectorSize)!=0` | branch  2 taken 3 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:1702` | `\|\| ((iPageSize-1)&iPageSize)!=0   \|\| ((iSectorSize-1)&iSectorSize)!=0` | branch  3 taken 172 | `crates/sqliters-pager/src/lib.rs:6607` |
| `pager.c:2890` | `if( rc!=SQLITE_OK ){` | branch  0 taken 1 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2890` | `if( rc!=SQLITE_OK ){` | branch  1 taken 280 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2900` | `if( rc==SQLITE_OK && zSuper ){` | branch  0 taken 271 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2900` | `if( rc==SQLITE_OK && zSuper ){` | branch  1 taken 9 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2900` | `if( rc==SQLITE_OK && zSuper ){` | branch  2 taken 16 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2900` | `if( rc==SQLITE_OK && zSuper ){` | branch  3 taken 255 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2903` | `if( rc!=SQLITE_OK \|\| !res ){` | branch  0 taken 271 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2903` | `if( rc!=SQLITE_OK \|\| !res ){` | branch  1 taken 9 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2903` | `if( rc!=SQLITE_OK \|\| !res ){` | branch  2 taken 2 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2903` | `if( rc!=SQLITE_OK \|\| !res ){` | branch  3 taken 269 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2920` | `if( rc!=SQLITE_OK ){` | branch  0 taken 246 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2920` | `if( rc!=SQLITE_OK ){` | branch  1 taken 186 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2921` | `if( rc==SQLITE_DONE ){` | branch  0 taken 181 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2921` | `if( rc==SQLITE_DONE ){` | branch  1 taken 65 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2932` | `if( nRec==0xffffffff ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2932` | `if( nRec==0xffffffff ){` | branch  1 taken 178 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  0 taken 14 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  1 taken 172 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  2 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2951` | `if( nRec==0 && !isHot &&` | branch  3 taken 14 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2952` | `pPager->journalHdr+JOURNAL_HDR_SZ(pPager)==pPager->journalOff ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2952` | `pPager->journalHdr+JOURNAL_HDR_SZ(pPager)==pPager->journalOff ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2959` | `if( pPager->journalOff==JOURNAL_HDR_SZ(pPager) ){` | branch  0 taken 172 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2959` | `if( pPager->journalOff==JOURNAL_HDR_SZ(pPager) ){` | branch  1 taken 14 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2961` | `if( rc!=SQLITE_OK ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2961` | `if( rc!=SQLITE_OK ){` | branch  1 taken 172 | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2965` | `if( pPager->mxPgno<mxPg ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:6494` |
| `pager.c:2965` | `if( pPager->mxPgno<mxPg ){` | branch  1 taken 172 | `crates/sqliters-pager/src/lib.rs:6494` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve ölçüm sınırı |
|---|---|---|
| `pager.c:1360` | branch  0 taken 0 (fallthrough) | Tahsis hatası. Super-journal adı için bellek ayırma başarısızlığı üretilmedi. |
| `pager.c:1361` | branch  0 never executed (fallthrough) | Tahsis hatası. Başarısız tahsisten sonra trailer magic uyuşmazlığı. |
| `pager.c:1361` | branch  1 never executed | Tahsis hatası. Başarısız tahsisten sonra geçerli trailer magic ve SQLITE_NOMEM. |
| `pager.c:1655` | branch  0 taken 0 (fallthrough) | Bağlam. İşlemi başlatan bağlantının sıcak olmayan rollback yolu. |
| `pager.c:1655` | branch  2 never executed (fallthrough) | Bağlam. Sıcak olmayan rollback'te bölüm ofseti journalHdr değerinden farklı. |
| `pager.c:1655` | branch  3 never executed | Bağlam. Sıcak olmayan rollback'te bölüm ofseti journalHdr değerine eşit. |
| `pager.c:2951` | branch  2 taken 0 (fallthrough) | Bağlam. nRec sıfır ve işlemi başlatan bağlantı rollback yapıyor. |
| `pager.c:2952` | branch  0 never executed (fallthrough) | Bağlam. Sıcak olmayan, sıfır kayıtlı bölümde son başlık ofseti karşılaştırmasının eşitlik çıkışı. |
| `pager.c:2952` | branch  1 never executed | Bağlam. Sıcak olmayan, sıfır kayıtlı bölümde son başlık ofseti karşılaştırmasının eşitsizlik çıkışı. |
| `pager.c:2961` | branch  0 taken 0 (fallthrough) | G/Ç hatası. Ana DB eski boyutuna döndürülürken xTruncate veya büyütme xWrite başarısızlığı. |
| `pager.c:2965` | branch  0 taken 0 (fallthrough) | Yapılandırma. İlk başlıktaki özgün sayfa sayısı Pager.mxPgno sınırının üstünde. |

## Sync sınırı ek ölçümü

Sekiz ayrı vakada `pager.c:2461` kayıt sınırı karşılaştırması 12 kabul ve 4 sınır dışı sonuç verdi. Rust `lib.rs:6586–6590` aynı DB yazma kararını verdi. `pager.c:4104–4108` sekiz normal journal sync ve boyut okuması yaptı. Rust karşılığı `lib.rs:6634–6638`. Aşağıdaki çıkışlar bu ek ölçümün kapsamı dışında kaldı.

| C yeri | Gcov çıkışı | Sınıf ve ölçüm sınırı |
|---|---|---|
| `pager.c:2460` | branch 1 taken 0 | Bağlam. Alt journal playback. |
| `pager.c:2461` | branch 1 taken 0 | Yapılandırma. noSync açıkken ofset karşılaştırması atlanır. |
| `pager.c:2463` | branch 0 never executed | Bağlam. Alt journal ve cache sayfası koşulu. |
| `pager.c:2463` | branch 1 never executed | Bağlam. Alt journal ve cache sayfası koşulu. |
| `pager.c:2463` | branch 2 never executed | Bağlam. Alt journal ve cache sayfasının PGHDR_NEED_SYNC bayrak koşulu. |
| `pager.c:2463` | branch 3 never executed | Bağlam. Alt journal ve cache sayfasının PGHDR_NEED_SYNC bayrak koşulu. |
| `pager.c:2465` | branch 1 taken 0 | Yaşam döngüsü. DB dosya tanıtıcısı kapalı. |
| `pager.c:2466` | branch 1 taken 0 | Yaşam döngüsü. Pager, sıcak OPEN dışında WRITER_DBMOD veya üstünde. |
| `pager.c:2466` | branch 3 taken 0 | Yaşam döngüsü. Pager OPEN dışında ve WRITER_DBMOD altında. |
| `pager.c:4104` | branch 1 taken 0 | Yapılandırma. noSync açıkken journal xSync atlanır. |
| `pager.c:4107` | branch 1 taken 0 | G/Ç hatası. Sync başarısız olduğunda boyut okunmaz. |
