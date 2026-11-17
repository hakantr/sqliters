# V6b.36 — Üretilen sütun C kararları ve dal kapsamı

SQLite 3.54.1 `-O0 --coverage` referans nesnesinde 244 native akış çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `build.c:1866–1877` | `GeneratedPk` | 5/5 | 2/2 |
| `build.c:1909–1934` | `AddPrimaryKey` | 17/17 | 8/8 |
| `build.c:2038–2087` | `AddGenerated` | 27/29 | 25/28 |
| `build.c:1789–1837` | `AddDefaultValue` | 21/23 | 13/16 |
| `build.c:1337–1353` | `StartTableNoop` | 11/12 | 8/10 |

64 dalın 56'sı alındı. Aşağıdaki 8 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `build.c:1337` | `if( !IN_SPECIAL_PARSE ){` | branch  0 taken 936 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1337` | `if( !IN_SPECIAL_PARSE ){` | branch  1 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1339` | `if( SQLITE_OK!=sqlite3ReadSchema(pParse) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1339` | `if( SQLITE_OK!=sqlite3ReadSchema(pParse) ){` | branch  2 taken 936 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1343` | `if( pTable ){` | branch  0 taken 80 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1343` | `if( pTable ){` | branch  1 taken 856 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1344` | `if( !noErr ){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1344` | `if( !noErr ){` | branch  1 taken 40 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1346` | `(IsView(pTable)? "view" : "table"), pName);` | branch  0 taken 20 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1346` | `(IsView(pTable)? "view" : "table"), pName);` | branch  1 taken 20 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1799` | `if( p!=0 ){` | branch  0 taken 20 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1799` | `if( p!=0 ){` | branch  1 taken 8 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1800` | `int isInit = db->init.busy && db->init.iDb!=1;` | branch  0 taken 4 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1800` | `int isInit = db->init.busy && db->init.iDb!=1;` | branch  1 taken 16 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1800` | `int isInit = db->init.busy && db->init.iDb!=1;` | branch  2 taken 4 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1800` | `int isInit = db->init.busy && db->init.iDb!=1;` | branch  3 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1802` | `if( !sqlite3ExprIsConstantOrFunction(pExpr, isInit) ){` | branch  1 taken 4 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1802` | `if( !sqlite3ExprIsConstantOrFunction(pExpr, isInit) ){` | branch  2 taken 16 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1806` | `}else if( pCol->colFlags & COLFLAG_GENERATED ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1806` | `}else if( pCol->colFlags & COLFLAG_GENERATED ){` | branch  1 taken 12 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1826` | `if( db->init.busy==0 && db->xAuth!=0 ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1826` | `if( db->init.busy==0 && db->xAuth!=0 ){` | branch  1 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1826` | `if( db->init.busy==0 && db->xAuth!=0 ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1826` | `if( db->init.busy==0 && db->xAuth!=0 ){` | branch  3 taken 8 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1833` | `if( IN_RENAME_OBJECT ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1833` | `if( IN_RENAME_OBJECT ){` | branch  1 taken 28 | `crates/sqliters-codegen/src/build_tab.rs:410` |
| `build.c:1869` | `if( pCol->colFlags & COLFLAG_GENERATED ){` | branch  0 taken 104 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:434` |
| `build.c:1869` | `if( pCol->colFlags & COLFLAG_GENERATED ){` | branch  1 taken 56 | `crates/sqliters-codegen/src/build_tab.rs:434` |
| `build.c:1914` | `if( pList==0 ){` | branch  0 taken 96 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1914` | `if( pList==0 ){` | branch  1 taken 48 | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1921` | `for(i=0; i<nTerm; i++){` | branch  0 taken 60 | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1921` | `for(i=0; i<nTerm; i++){` | branch  1 taken 48 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1925` | `if( pCExpr->op==TK_ID ){` | branch  0 taken 56 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1925` | `if( pCExpr->op==TK_ID ){` | branch  1 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1928` | `if( iCol>=0 ){` | branch  0 taken 48 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:1928` | `if( iCol>=0 ){` | branch  1 taken 8 | `crates/sqliters-codegen/src/build_tab.rs:483` |
| `build.c:2043` | `if( pTab==0 ){` | branch  0 taken 40 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2043` | `if( pTab==0 ){` | branch  1 taken 184 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2048` | `if( IN_DECLARE_VTAB ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2048` | `if( IN_DECLARE_VTAB ){` | branch  1 taken 184 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2052` | `if( pCol->iDflt>0 ) goto generated_error;` | branch  0 taken 4 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2052` | `if( pCol->iDflt>0 ) goto generated_error;` | branch  1 taken 180 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2053` | `if( pType ){` | branch  0 taken 72 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2053` | `if( pType ){` | branch  1 taken 108 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2054` | `if( pType->n==7 && sqlite3StrNICmp("virtual",pType->z,7)==0 ){` | branch  0 taken 20 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2054` | `if( pType->n==7 && sqlite3StrNICmp("virtual",pType->z,7)==0 ){` | branch  1 taken 52 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2054` | `if( pType->n==7 && sqlite3StrNICmp("virtual",pType->z,7)==0 ){` | branch  3 taken 16 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2054` | `if( pType->n==7 && sqlite3StrNICmp("virtual",pType->z,7)==0 ){` | branch  4 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2056` | `}else if( pType->n==6 && sqlite3StrNICmp("stored",pType->z,6)==0 ){` | branch  0 taken 52 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2056` | `}else if( pType->n==6 && sqlite3StrNICmp("stored",pType->z,6)==0 ){` | branch  1 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2056` | `}else if( pType->n==6 && sqlite3StrNICmp("stored",pType->z,6)==0 ){` | branch  3 taken 48 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2056` | `}else if( pType->n==6 && sqlite3StrNICmp("stored",pType->z,6)==0 ){` | branch  4 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2062` | `if( eType==COLFLAG_VIRTUAL ) pTab->nNVCol--;` | branch  0 taken 124 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2062` | `if( eType==COLFLAG_VIRTUAL ) pTab->nNVCol--;` | branch  1 taken 48 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2067` | `if( pCol->colFlags & COLFLAG_PRIMKEY ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2067` | `if( pCol->colFlags & COLFLAG_PRIMKEY ){` | branch  1 taken 156 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2070` | `if( ALWAYS(pExpr) && pExpr->op==TK_ID ){` | branch  0 taken 172 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2070` | `if( ALWAYS(pExpr) && pExpr->op==TK_ID ){` | branch  1 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2070` | `if( ALWAYS(pExpr) && pExpr->op==TK_ID ){` | branch  2 taken 128 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2070` | `if( ALWAYS(pExpr) && pExpr->op==TK_ID ){` | branch  3 taken 44 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2077` | `if( pExpr && pExpr->op!=TK_RAISE ) pExpr->affExpr = pCol->affinity;` | branch  0 taken 172 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2077` | `if( pExpr && pExpr->op!=TK_RAISE ) pExpr->affExpr = pCol->affinity;` | branch  1 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2077` | `if( pExpr && pExpr->op!=TK_RAISE ) pExpr->affExpr = pCol->affinity;` | branch  2 taken 168 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:604` |
| `build.c:2077` | `if( pExpr && pExpr->op!=TK_RAISE ) pExpr->affExpr = pCol->affinity;` | branch  3 taken 4 | `crates/sqliters-codegen/src/build_tab.rs:604` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `build.c:2048` | branch  0 taken 0 (fallthrough) | Sanal tablo bildirimi kapsam dışı. Bu kol bildirime ait üretilen sütunu reddeder. |
| `build.c:2070` | branch  1 taken 0 | ALWAYS(pExpr) ifadesinin NULL çıkışı tetiklenmedi. İfade tahsis hatası enjekte edilmedi. |
| `build.c:2077` | branch  1 taken 0 | pExpr NULL çıkışı tetiklenmedi. NOMEM ve eksik ifade yaşamı kapsam dışı. |
| `build.c:1800` | branch  3 taken 0 | TEMP şeması yüklenirken isInit koşulunun ikinci işleneni açık kaldı. Matris TEMP üzerinde normal hazırlamayı ölçüyor. |
| `build.c:1826` | branch  2 taken 0 (fallthrough) | DEFAULT authorizer callback kolu alınmadı. Yeni hazırlama köprüsü authorizer etkin iken devreye girmez. |
| `build.c:1833` | branch  0 taken 0 (fallthrough) | Yeniden adlandırma sırasında ifade-token eşlemesinin kaldırılması bu DDL yolunda çalışmadı. |
| `build.c:1337` | branch  1 taken 0 | Özel parse modu normal ad aramasını atlar. Ad çakışması normal hazırlama yolunda karşılaştırıldı. |
| `build.c:1339` | branch  1 taken 0 (fallthrough) | sqlite3ReadSchema hata yolu tetiklenmedi. Bozuk şema ve giriş/çıkış enjeksiyonu açık. |
