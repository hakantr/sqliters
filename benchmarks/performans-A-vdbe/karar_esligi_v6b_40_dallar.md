# V6b.40 — Kabuk modülleri C kararları ve dal kapsamı

SQLite 3.54.1 başvuru kabuğunda `-O0 --coverage` uzantı nesneleriyle 36 completion ve 42 fsdir akışı çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `completion.c:105–145` | `completionConnect` | 10/10 | 2/4 |
| `completion.c:198–314` | `completionNext` | 75/75 | 37/45 |
| `completion.c:370–414` | `completionFilter` | 29/29 | 23/26 |
| `completion.c:424–461` | `completionBestIndex` | 27/27 | 12/13 |
| `fileio.c:222–253` | `readFileContents` | 16/21 | 7/10 |
| `fileio.c:654–675` | `fsdirConnect` | 10/10 | 2/4 |
| `fileio.c:748–808` | `fsdirNext` | 40/44 | 22/26 |
| `fileio.c:812–881` | `fsdirColumn` | 36/39 | 18/20 |
| `fileio.c:909–962` | `fsdirFilter` | 31/32 | 21/26 |
| `fileio.c:977–1070` | `fsdirBestIndex` | 59/64 | 27/40 |

214 dalın 171'i alındı. Aşağıdaki 43 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `completion.c:134` | `if( rc==SQLITE_OK ){` | branch  0 taken 36 (fallthrough) | `crates/sqliters-cli/src/completion.rs:11` |
| `completion.c:134` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-cli/src/completion.rs:11` |
| `completion.c:137` | `if( pNew==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:11` |
| `completion.c:137` | `if( pNew==0 ) return SQLITE_NOMEM;` | branch  1 taken 36 | `crates/sqliters-cli/src/completion.rs:11` |
| `completion.c:204` | `while( pCur->ePhase!=COMPLETION_EOF ){` | branch  0 taken 4658 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:204` | `while( pCur->ePhase!=COMPLETION_EOF ){` | branch  1 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:205` | `switch( pCur->ePhase ){` | branch  0 taken 4234 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:205` | `switch( pCur->ePhase ){` | branch  1 taken 80 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:205` | `switch( pCur->ePhase ){` | branch  2 taken 172 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:205` | `switch( pCur->ePhase ){` | branch  3 taken 172 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:205` | `switch( pCur->ePhase ){` | branch  4 taken 0 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:207` | `if( pCur->j >= sqlite3_keyword_count() ){` | branch  1 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:207` | `if( pCur->j >= sqlite3_keyword_count() ){` | branch  2 taken 4206 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:217` | `if( pCur->pStmt==0 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:217` | `if( pCur->pStmt==0 ){` | branch  1 taken 52 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:226` | `if( pCur->pStmt==0 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:226` | `if( pCur->pStmt==0 ){` | branch  1 taken 144 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:232` | `while( sqlite3_step(pS2)==SQLITE_ROW ){` | branch  1 taken 52 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:232` | `while( sqlite3_step(pS2)==SQLITE_ROW ){` | branch  2 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:243` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:243` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  1 taken 28 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:244` | `if( rc==SQLITE_OK ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:244` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:248` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:248` | `if( rc ) return rc;` | branch  1 taken 28 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:255` | `if( pCur->pStmt==0 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:255` | `if( pCur->pStmt==0 ){` | branch  1 taken 144 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:261` | `while( sqlite3_step(pS2)==SQLITE_ROW ){` | branch  1 taken 52 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:261` | `while( sqlite3_step(pS2)==SQLITE_ROW ){` | branch  2 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:274` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:274` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  1 taken 28 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:275` | `if( rc==SQLITE_OK ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:275` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:279` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:279` | `if( rc ) return rc;` | branch  1 taken 28 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:286` | `if( iCol<0 ){` | branch  0 taken 4234 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:286` | `if( iCol<0 ){` | branch  1 taken 424 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:288` | `if( pCur->zCurrentRow==0 ) continue;` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:288` | `if( pCur->zCurrentRow==0 ) continue;` | branch  1 taken 4206 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:290` | `if( sqlite3_step(pCur->pStmt)==SQLITE_ROW ){` | branch  1 taken 340 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:290` | `if( sqlite3_step(pCur->pStmt)==SQLITE_ROW ){` | branch  2 taken 84 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:299` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:299` | `if( rc ) return rc;` | branch  1 taken 84 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:303` | `if( pCur->nPrefix==0 ) break;` | branch  0 taken 382 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:303` | `if( pCur->nPrefix==0 ) break;` | branch  1 taken 4164 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:304` | `if( pCur->nPrefix<=pCur->szRow` | branch  0 taken 3554 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:304` | `if( pCur->nPrefix<=pCur->szRow` | branch  1 taken 610 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:305` | `&& sqlite3_strnicmp(pCur->zPrefix, pCur->zCurrentRow, pCur->nPrefix)==0` | branch  1 taken 150 (fallthrough) | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:305` | `&& sqlite3_strnicmp(pCur->zPrefix, pCur->zCurrentRow, pCur->nPrefix)==0` | branch  2 taken 3404 | `crates/sqliters-cli/src/completion.rs:109` |
| `completion.c:380` | `if( idxNum & 1 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:380` | `if( idxNum & 1 ){` | branch  1 taken 8 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:382` | `if( pCur->nPrefix>0 ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:382` | `if( pCur->nPrefix>0 ){` | branch  1 taken 10 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:384` | `if( pCur->zPrefix==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:384` | `if( pCur->zPrefix==0 ) return SQLITE_NOMEM;` | branch  1 taken 18 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:389` | `if( idxNum & 2 ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:389` | `if( idxNum & 2 ){` | branch  1 taken 20 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:391` | `if( pCur->nLine>0 ){` | branch  0 taken 14 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:391` | `if( pCur->nLine>0 ){` | branch  1 taken 2 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:393` | `if( pCur->zLine==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:393` | `if( pCur->zLine==0 ) return SQLITE_NOMEM;` | branch  1 taken 14 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:397` | `if( pCur->zLine!=0 && pCur->zPrefix==0 ){` | branch  0 taken 14 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:397` | `if( pCur->zLine!=0 && pCur->zPrefix==0 ){` | branch  1 taken 22 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:397` | `if( pCur->zLine!=0 && pCur->zPrefix==0 ){` | branch  2 taken 12 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:397` | `if( pCur->zLine!=0 && pCur->zPrefix==0 ){` | branch  3 taken 2 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  0 taken 42 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  1 taken 2 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  2 taken 30 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  3 taken 12 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  4 taken 2 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:399` | `while( i>0 && (IsAlnum(pCur->zLine[i-1]) \|\| pCur->zLine[i-1]=='_') ){` | branch  5 taken 10 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:403` | `if( pCur->nPrefix>0 ){` | branch  0 taken 10 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:403` | `if( pCur->nPrefix>0 ){` | branch  1 taken 2 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:405` | `if( pCur->zPrefix==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:405` | `if( pCur->zPrefix==0 ) return SQLITE_NOMEM;` | branch  1 taken 10 | `crates/sqliters-cli/src/completion.rs:193` |
| `completion.c:437` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  0 taken 56 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:437` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  1 taken 36 (fallthrough) | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:438` | `if( pConstraint->usable==0 ) continue;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:438` | `if( pConstraint->usable==0 ) continue;` | branch  1 taken 56 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:439` | `if( pConstraint->op!=SQLITE_INDEX_CONSTRAINT_EQ ) continue;` | branch  0 taken 10 (fallthrough) | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:439` | `if( pConstraint->op!=SQLITE_INDEX_CONSTRAINT_EQ ) continue;` | branch  1 taken 46 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:440` | `switch( pConstraint->iColumn ){` | branch  0 taken 28 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:440` | `switch( pConstraint->iColumn ){` | branch  1 taken 16 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:440` | `switch( pConstraint->iColumn ){` | branch  2 taken 2 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:451` | `if( prefixIdx>=0 ){` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:451` | `if( prefixIdx>=0 ){` | branch  1 taken 8 | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:455` | `if( wholelineIdx>=0 ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-cli/src/completion.rs:54` |
| `completion.c:455` | `if( wholelineIdx>=0 ){` | branch  1 taken 20 | `crates/sqliters-cli/src/completion.rs:54` |
| `fileio.c:230` | `if( in==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:230` | `if( in==0 ){` | branch  1 taken 34 | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:239` | `if( nIn>mxBlob ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:239` | `if( nIn>mxBlob ){` | branch  1 taken 32 | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:244` | `pBuf = sqlite3_malloc64( nIn ? nIn : 1 );` | branch  0 taken 28 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:244` | `pBuf = sqlite3_malloc64( nIn ? nIn : 1 );` | branch  1 taken 4 | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:245` | `if( pBuf==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:245` | `if( pBuf==0 ){` | branch  1 taken 32 | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:250` | `if( nIn==(sqlite3_int64)fread(pBuf, 1, (size_t)nIn, in) ){` | branch  1 taken 32 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:250` | `if( nIn==(sqlite3_int64)fread(pBuf, 1, (size_t)nIn, in) ){` | branch  2 taken 0 | `crates/sqliters-cli/src/fsdir.rs:143` |
| `fileio.c:668` | `if( rc==SQLITE_OK ){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:25` |
| `fileio.c:668` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:25` |
| `fileio.c:670` | `if( pNew==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:25` |
| `fileio.c:670` | `if( pNew==0 ) return SQLITE_NOMEM;` | branch  1 taken 40 | `crates/sqliters-cli/src/fsdir.rs:25` |
| `fileio.c:753` | `if( S_ISDIR(m) && pCur->iLvl+3<pCur->mxLvl ){` | branch  0 taken 60 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:753` | `if( S_ISDIR(m) && pCur->iLvl+3<pCur->mxLvl ){` | branch  1 taken 146 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:753` | `if( S_ISDIR(m) && pCur->iLvl+3<pCur->mxLvl ){` | branch  2 taken 46 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:753` | `if( S_ISDIR(m) && pCur->iLvl+3<pCur->mxLvl ){` | branch  3 taken 14 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:757` | `if( iNew>=pCur->nLvl ){` | branch  0 taken 34 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:757` | `if( iNew>=pCur->nLvl ){` | branch  1 taken 12 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:761` | `if( aNew==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:761` | `if( aNew==0 ) return SQLITE_NOMEM;` | branch  1 taken 34 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:772` | `if( pLvl->pDir==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:772` | `if( pLvl->pDir==0 ){` | branch  1 taken 46 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:778` | `while( pCur->iLvl>=0 ){` | branch  0 taken 314 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:778` | `while( pCur->iLvl>=0 ){` | branch  1 taken 30 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:781` | `if( pEntry ){` | branch  0 taken 268 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:781` | `if( pEntry ){` | branch  1 taken 46 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:782` | `if( pEntry->d_name[0]=='.' ){` | branch  0 taken 128 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:782` | `if( pEntry->d_name[0]=='.' ){` | branch  1 taken 140 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:783` | `if( pEntry->d_name[1]=='.' && pEntry->d_name[2]=='\0' ) continue;` | branch  0 taken 64 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:783` | `if( pEntry->d_name[1]=='.' && pEntry->d_name[2]=='\0' ) continue;` | branch  1 taken 64 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:783` | `if( pEntry->d_name[1]=='.' && pEntry->d_name[2]=='\0' ) continue;` | branch  2 taken 46 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:783` | `if( pEntry->d_name[1]=='.' && pEntry->d_name[2]=='\0' ) continue;` | branch  3 taken 18 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:784` | `if( pEntry->d_name[1]=='\0' ) continue;` | branch  0 taken 46 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:784` | `if( pEntry->d_name[1]=='\0' ) continue;` | branch  1 taken 36 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:788` | `if( pCur->zPath==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:788` | `if( pCur->zPath==0 ) return SQLITE_NOMEM;` | branch  1 taken 176 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:789` | `if( fileLinkStat(pCur->zPath, &pCur->sStat) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:789` | `if( fileLinkStat(pCur->zPath, &pCur->sStat) ){` | branch  2 taken 176 | `crates/sqliters-cli/src/fsdir.rs:253` |
| `fileio.c:818` | `switch( i ){` | branch  0 taken 222 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:818` | `switch( i ){` | branch  1 taken 26 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:818` | `switch( i ){` | branch  2 taken 26 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:818` | `switch( i ){` | branch  3 taken 58 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:818` | `switch( i ){` | branch  4 taken 196 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:818` | `switch( i ){` | branch  5 taken 4 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:834` | `if( S_ISDIR(m) ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:834` | `if( S_ISDIR(m) ){` | branch  1 taken 42 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:837` | `}else if( S_ISLNK(m) ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:837` | `}else if( S_ISLNK(m) ){` | branch  1 taken 34 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:845` | `if( n<nBuf ) break;` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:845` | `if( n<nBuf ) break;` | branch  1 taken 8 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:846` | `if( aBuf!=aStatic ) sqlite3_free(aBuf);` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:846` | `if( aBuf!=aStatic ) sqlite3_free(aBuf);` | branch  1 taken 4 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:849` | `if( aBuf==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:849` | `if( aBuf==0 ){` | branch  1 taken 8 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:855` | `if( n>0 ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:855` | `if( n>0 ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:860` | `if( aBuf!=aStatic ) sqlite3_free(aBuf);` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:860` | `if( aBuf!=aStatic ) sqlite3_free(aBuf);` | branch  1 taken 4 | `crates/sqliters-cli/src/fsdir.rs:285` |
| `fileio.c:920` | `if( idxNum==0 ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:920` | `if( idxNum==0 ){` | branch  1 taken 38 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:925` | `assert( (idxNum & 0x01)!=0 && argc>0 );` | branch  0 taken 38 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:925` | `assert( (idxNum & 0x01)!=0 && argc>0 );` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:925` | `assert( (idxNum & 0x01)!=0 && argc>0 );` | branch  2 taken 38 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:925` | `assert( (idxNum & 0x01)!=0 && argc>0 );` | branch  3 taken 0 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:927` | `if( zDir==0 ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:927` | `if( zDir==0 ){` | branch  1 taken 36 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:932` | `if( (idxNum & 0x02)!=0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:932` | `if( (idxNum & 0x02)!=0 ){` | branch  1 taken 32 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:933` | `assert( argc>i );` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:933` | `assert( argc>i );` | branch  1 taken 4 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:936` | `if( (idxNum & 0x0c)!=0 ){` | branch  0 taken 10 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:936` | `if( (idxNum & 0x0c)!=0 ){` | branch  1 taken 26 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:937` | `assert( argc>i );` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:937` | `assert( argc>i );` | branch  1 taken 10 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:939` | `if( idxNum & 0x08 ) pCur->mxLvl++;` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:939` | `if( idxNum & 0x08 ) pCur->mxLvl++;` | branch  1 taken 2 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:940` | `if( pCur->mxLvl<=0 ) pCur->mxLvl = 1000000000;` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:940` | `if( pCur->mxLvl<=0 ) pCur->mxLvl = 1000000000;` | branch  1 taken 8 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:944` | `if( pCur->zBase ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:944` | `if( pCur->zBase ){` | branch  1 taken 32 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:951` | `if( pCur->zPath==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:951` | `if( pCur->zPath==0 ){` | branch  1 taken 36 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:954` | `if( fileLinkStat(pCur->zPath, &pCur->sStat) ){` | branch  1 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:954` | `if( fileLinkStat(pCur->zPath, &pCur->sStat) ){` | branch  2 taken 32 | `crates/sqliters-cli/src/fsdir.rs:205` |
| `fileio.c:993` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  0 taken 56 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:993` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  1 taken 40 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:994` | `if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_EQ ){` | branch  0 taken 46 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:994` | `if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_EQ ){` | branch  1 taken 10 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:995` | `switch( pConstraint->iColumn ){` | branch  0 taken 38 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:995` | `switch( pConstraint->iColumn ){` | branch  1 taken 4 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:995` | `switch( pConstraint->iColumn ){` | branch  2 taken 2 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:995` | `switch( pConstraint->iColumn ){` | branch  3 taken 2 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:997` | `if( pConstraint->usable ){` | branch  0 taken 38 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:997` | `if( pConstraint->usable ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1000` | `}else if( idxPath<0 ){` | branch  0 never executed (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1000` | `}else if( idxPath<0 ){` | branch  1 never executed | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1006` | `if( pConstraint->usable ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1006` | `if( pConstraint->usable ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1009` | `}else if( idxDir<0 ){` | branch  0 never executed (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1009` | `}else if( idxDir<0 ){` | branch  1 never executed | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1015` | `if( pConstraint->usable && idxLevel<0 ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1015` | `if( pConstraint->usable && idxLevel<0 ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1015` | `if( pConstraint->usable && idxLevel<0 ){` | branch  2 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1015` | `if( pConstraint->usable && idxLevel<0 ){` | branch  3 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1024` | `if( pConstraint->iColumn==FSDIR_COLUMN_LEVEL` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1024` | `if( pConstraint->iColumn==FSDIR_COLUMN_LEVEL` | branch  1 taken 2 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1025` | `&& pConstraint->usable` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1025` | `&& pConstraint->usable` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1026` | `&& idxLevel<0` | branch  0 taken 8 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1026` | `&& idxLevel<0` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1028` | `if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_LE ){` | branch  0 taken 6 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1028` | `if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_LE ){` | branch  1 taken 2 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1032` | `}else if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_LT ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1032` | `}else if( pConstraint->op==SQLITE_INDEX_CONSTRAINT_LT ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1039` | `if( seenPath \|\| seenDir ){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1039` | `if( seenPath \|\| seenDir ){` | branch  1 taken 0 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1039` | `if( seenPath \|\| seenDir ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1039` | `if( seenPath \|\| seenDir ){` | branch  3 taken 40 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1044` | `if( idxPath<0 ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1044` | `if( idxPath<0 ){` | branch  1 taken 38 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1055` | `if( idxDir>=0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1055` | `if( idxDir>=0 ){` | branch  1 taken 34 | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1061` | `if( idxLevel>=0 ){` | branch  0 taken 10 (fallthrough) | `crates/sqliters-cli/src/fsdir.rs:65` |
| `fileio.c:1061` | `if( idxLevel>=0 ){` | branch  1 taken 28 | `crates/sqliters-cli/src/fsdir.rs:65` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `completion.c:134` | branch  1 taken 0 | Sanal şema bildirimi geçerli. Bildirim reddi profili çalışmadı. |
| `completion.c:137` | branch  0 taken 0 (fallthrough) | Sanal tablo nesnesi tahsis edilemediğinde NOMEM. Enjeksiyon yok. |
| `completion.c:205` | branch  4 taken 0 | Dört kullanılan aşama dışındaki ayrılmış aşamalar seçilmedi. |
| `completion.c:243` | branch  0 taken 0 (fallthrough) | Katalog birleştirmesi için SQL dizgesi tahsis edilemediğinde NOMEM. Enjeksiyon yok. |
| `completion.c:244` | branch  1 taken 0 | Şema listesi sondaki finalize çağrısında hata vermedi. |
| `completion.c:248` | branch  0 taken 0 (fallthrough) | Katalog aşaması kurulurken finalize hatası yok. |
| `completion.c:274` | branch  0 taken 0 (fallthrough) | Sütun sorgusu SQL dizgesi tahsis edilemediğinde NOMEM. Enjeksiyon yok. |
| `completion.c:275` | branch  1 taken 0 | Sütun aşamasının şema listesi finalize hatası vermedi. |
| `completion.c:279` | branch  0 taken 0 (fallthrough) | Sütun aşaması kurulurken finalize hatası yok. |
| `completion.c:299` | branch  0 taken 0 (fallthrough) | İç aday sorgusu finalize hatası bu matriste tetiklenmedi. |
| `completion.c:384` | branch  0 taken 0 (fallthrough) | Açık önek metni tahsis hatası enjekte edilmedi. |
| `completion.c:393` | branch  0 taken 0 (fallthrough) | Tam satır metni tahsis hatası enjekte edilmedi. |
| `completion.c:405` | branch  0 taken 0 (fallthrough) | Türetilen önek metni tahsis hatası enjekte edilmedi. |
| `completion.c:438` | branch  0 taken 0 (fallthrough) | Kullanılamayan join koşulu bu tek kaynak profilinde yok. |
| `fileio.c:230` | branch  0 taken 0 (fallthrough) | STAT sonrasında fopen reddi ya da kaybolan dosya üretilmedi. |
| `fileio.c:245` | branch  0 taken 0 (fallthrough) | İçerik tamponu tahsis hatası enjekte edilmedi. |
| `fileio.c:250` | branch  2 taken 0 | İçerik boyutu ölçümü sonrası kısa G/Ç okuması enjekte edilmedi. |
| `fileio.c:668` | branch  1 taken 0 | Sanal tablo şeması geçerli. Bildirim reddi çalışmadı. |
| `fileio.c:670` | branch  0 taken 0 (fallthrough) | Sanal tablo nesnesi tahsis hatası enjekte edilmedi. |
| `fileio.c:761` | branch  0 taken 0 (fallthrough) | Dizin yığını yeniden büyürken tahsis hatası enjekte edilmedi. |
| `fileio.c:772` | branch  0 taken 0 (fallthrough) | Başarılı lstat sonrasında opendir başarısızlığı tetiklenmedi. |
| `fileio.c:788` | branch  0 taken 0 (fallthrough) | Alt dizin yolu tahsis hatası enjekte edilmedi. |
| `fileio.c:789` | branch  1 taken 0 (fallthrough) | Bir alt girdi sayım ve lstat arasında kaybolmadı. |
| `fileio.c:849` | branch  0 taken 0 (fallthrough) | Uzun symlink hedefinin tampon tahsis hatası enjekte edilmedi. |
| `fileio.c:855` | branch  1 taken 0 | Symlink hedefi okuması sıfır ya da hata döndürmedi. |
| `fileio.c:925` | branch  1 taken 0 | xBestIndex seçimi path biti ve argüman sayısı önkoşulunu sağladı. |
| `fileio.c:925` | branch  3 taken 0 | xBestIndex seçimi path biti ve argüman sayısı önkoşulunu sağladı. |
| `fileio.c:933` | branch  0 taken 0 (fallthrough) | xBestIndex dir bitini seçtiğinde dir argümanı her çağrıda var. |
| `fileio.c:937` | branch  0 taken 0 (fallthrough) | xBestIndex level bitini seçtiğinde level argümanı her çağrıda var. |
| `fileio.c:951` | branch  0 taken 0 (fallthrough) | Kök yol dizgesi tahsis hatası enjekte edilmedi. |
| `fileio.c:997` | branch  1 taken 0 | Planlayıcıya kullanılamayan path eşitliği gelmedi. |
| `fileio.c:1000` | branch  0 never executed (fallthrough) | Kullanılamayan path eşitliği önkoşulu oluşmadığı için önceki path seçimi alt çıkışları alınmadı. |
| `fileio.c:1000` | branch  1 never executed | Kullanılamayan path eşitliği önkoşulu oluşmadığı için önceki path seçimi alt çıkışları alınmadı. |
| `fileio.c:1006` | branch  1 taken 0 | Planlayıcıya kullanılamayan dir eşitliği gelmedi. |
| `fileio.c:1009` | branch  0 never executed (fallthrough) | Kullanılamayan dir eşitliği önkoşulu oluşmadığı için önceki dir seçimi alt çıkışları alınmadı. |
| `fileio.c:1009` | branch  1 never executed | Kullanılamayan dir eşitliği önkoşulu oluşmadığı için önceki dir seçimi alt çıkışları alınmadı. |
| `fileio.c:1015` | branch  1 taken 0 | Level eşitliği kullanılabilir durumdaydı ve ikinci level seçimine rastlanmadı. |
| `fileio.c:1015` | branch  3 taken 0 | Level eşitliği kullanılabilir durumdaydı ve ikinci level seçimine rastlanmadı. |
| `fileio.c:1025` | branch  1 taken 0 | Kullanılamayan level aralık koşulu denenmedi. |
| `fileio.c:1026` | branch  1 taken 0 | Daha önce seçilmiş bir level aralığının reddettiği ek aralık bu profilde yok. |
| `fileio.c:1032` | branch  1 taken 0 | Level aralığı koluna gelen karşılaştırmalar LT veya LE idi. |
| `fileio.c:1039` | branch  1 taken 0 | Kullanılamayan path veya dir nedeniyle plan reddi bu tek kaynak kapsamında yok. |
| `fileio.c:1039` | branch  2 taken 0 (fallthrough) | Kullanılamayan path veya dir nedeniyle plan reddi bu tek kaynak kapsamında yok. |
