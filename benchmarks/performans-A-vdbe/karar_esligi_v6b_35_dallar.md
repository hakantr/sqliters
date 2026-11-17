# V6b.35 — D144 C kararları ve dal kapsamı

SQLite 3.54.1 `-O0 --coverage` referans nesnesinde 464 native akış çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `printf.c:1076–1095` | `ErrorToken` | 9/13 | 4/12 |
| `build.c:1036–1064` | `TwoPartName` | 11/13 | 5/6 |
| `build.c:1282–1358` | `StartTable_names` | 31/38 | 37/46 |

64 dalın 46'sı alındı. Aşağıdaki 18 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `build.c:1046` | `if( pName2->n>0 ){` | branch  0 taken 732 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1046` | `if( pName2->n>0 ){` | branch  1 taken 4604 | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1047` | `if( db->init.busy ) {` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1047` | `if( db->init.busy ) {` | branch  1 taken 732 | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1053` | `if( iDb<0 ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1053` | `if( iDb<0 ){` | branch  1 taken 716 | `crates/sqliters-codegen/src/build_tab.rs:90` |
| `build.c:1282` | `if( db->init.busy && db->init.newTnum==1 ){` | branch  0 taken 2584 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1282` | `if( db->init.busy && db->init.newTnum==1 ){` | branch  1 taken 1456 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1282` | `if( db->init.busy && db->init.newTnum==1 ){` | branch  2 taken 1040 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1282` | `if( db->init.busy && db->init.newTnum==1 ){` | branch  3 taken 1544 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1285` | `zName = sqlite3DbStrDup(db, SCHEMA_TABLE(iDb));` | branch  0 taken 464 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1285` | `zName = sqlite3DbStrDup(db, SCHEMA_TABLE(iDb));` | branch  1 taken 576 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1290` | `if( iDb<0 ) return;` | branch  0 taken 16 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1290` | `if( iDb<0 ) return;` | branch  1 taken 2984 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  0 taken 448 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  1 taken 2536 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  2 taken 8 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  3 taken 440 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  4 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1291` | `if( !OMIT_TEMPDB && isTemp && pName2->n>0 && iDb!=1 ){` | branch  5 taken 8 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1297` | `if( !OMIT_TEMPDB && isTemp ) iDb = 1;` | branch  0 taken 448 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1297` | `if( !OMIT_TEMPDB && isTemp ) iDb = 1;` | branch  1 taken 2536 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1299` | `if( IN_RENAME_OBJECT ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1299` | `if( IN_RENAME_OBJECT ){` | branch  1 taken 2984 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1304` | `if( zName==0 ) return;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1304` | `if( zName==0 ) return;` | branch  1 taken 4024 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1305` | `if( sqlite3CheckObjectName(pParse, zName, isView?"view":"table", zName) ){` | branch  0 taken 1336 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1305` | `if( sqlite3CheckObjectName(pParse, zName, isView?"view":"table", zName) ){` | branch  1 taken 2688 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1305` | `if( sqlite3CheckObjectName(pParse, zName, isView?"view":"table", zName) ){` | branch  3 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1305` | `if( sqlite3CheckObjectName(pParse, zName, isView?"view":"table", zName) ){` | branch  4 taken 4024 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1308` | `if( db->init.iDb==1 ) isTemp = 1;` | branch  0 taken 816 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1308` | `if( db->init.iDb==1 ) isTemp = 1;` | branch  1 taken 3208 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1320` | `if( sqlite3AuthCheck(pParse, SQLITE_INSERT, SCHEMA_TABLE(isTemp), 0, zDb) ){` | branch  0 taken 1264 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1320` | `if( sqlite3AuthCheck(pParse, SQLITE_INSERT, SCHEMA_TABLE(isTemp), 0, zDb) ){` | branch  1 taken 2760 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1320` | `if( sqlite3AuthCheck(pParse, SQLITE_INSERT, SCHEMA_TABLE(isTemp), 0, zDb) ){` | branch  3 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1320` | `if( sqlite3AuthCheck(pParse, SQLITE_INSERT, SCHEMA_TABLE(isTemp), 0, zDb) ){` | branch  4 taken 4024 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1323` | `if( !isVirtual && sqlite3AuthCheck(pParse, (int)aCode[isTemp+2*isView],` | branch  0 taken 4024 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1323` | `if( !isVirtual && sqlite3AuthCheck(pParse, (int)aCode[isTemp+2*isView],` | branch  1 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1323` | `if( !isVirtual && sqlite3AuthCheck(pParse, (int)aCode[isTemp+2*isView],` | branch  3 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1323` | `if( !isVirtual && sqlite3AuthCheck(pParse, (int)aCode[isTemp+2*isView],` | branch  4 taken 4024 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1337` | `if( !IN_SPECIAL_PARSE ){` | branch  0 taken 4024 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1337` | `if( !IN_SPECIAL_PARSE ){` | branch  1 taken 0 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1339` | `if( SQLITE_OK!=sqlite3ReadSchema(pParse) ){` | branch  1 taken 0 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1339` | `if( SQLITE_OK!=sqlite3ReadSchema(pParse) ){` | branch  2 taken 4024 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1343` | `if( pTable ){` | branch  0 taken 300 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1343` | `if( pTable ){` | branch  1 taken 3724 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1344` | `if( !noErr ){` | branch  0 taken 180 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1344` | `if( !noErr ){` | branch  1 taken 120 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1346` | `(IsView(pTable)? "view" : "table"), pName);` | branch  0 taken 44 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1346` | `(IsView(pTable)? "view" : "table"), pName);` | branch  1 taken 136 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1354` | `if( sqlite3FindIndex(db, zName, zDb)!=0 ){` | branch  1 taken 60 (fallthrough) | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `build.c:1354` | `if( sqlite3FindIndex(db, zName, zDb)!=0 ){` | branch  2 taken 3664 | `crates/sqliters-codegen/src/build_tab.rs:178` |
| `printf.c:1077` | `if( (pAccum->printfFlags & SQLITE_PRINTF_INTERNAL)==0 ) return;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1077` | `if( (pAccum->printfFlags & SQLITE_PRINTF_INTERNAL)==0 ) return;` | branch  1 taken 196 | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1078` | `if( flag_alternateform ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1078` | `if( flag_alternateform ){` | branch  1 taken 196 | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1081` | `if( ALWAYS(pExpr) && ALWAYS(!ExprHasProperty(pExpr,EP_IntValue)) ){` | branch  0 never executed (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1081` | `if( ALWAYS(pExpr) && ALWAYS(!ExprHasProperty(pExpr,EP_IntValue)) ){` | branch  1 never executed | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1081` | `if( ALWAYS(pExpr) && ALWAYS(!ExprHasProperty(pExpr,EP_IntValue)) ){` | branch  2 never executed (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1081` | `if( ALWAYS(pExpr) && ALWAYS(!ExprHasProperty(pExpr,EP_IntValue)) ){` | branch  3 never executed | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1089` | `if( pToken && pToken->n ){` | branch  0 taken 196 (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1089` | `if( pToken && pToken->n ){` | branch  1 taken 0 | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1089` | `if( pToken && pToken->n ){` | branch  2 taken 196 (fallthrough) | `crates/sqliters-codegen/src/sqliteint.rs:1877` |
| `printf.c:1089` | `if( pToken && pToken->n ){` | branch  3 taken 0 | `crates/sqliters-codegen/src/sqliteint.rs:1877` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `printf.c:1077` | branch  0 taken 0 (fallthrough) | Genel printf çağrısında dahili SQL token biçimini reddeden kol alınmadı. Rust yalnız parser tanısının error_msg_tok yolunu kullanır. |
| `printf.c:1078` | branch  0 taken 0 (fallthrough) | Alternatif %#T biçimi Expr içindir. Bu adım normal %T Token yolu ile sınırlı. |
| `printf.c:1081` | branch  0 never executed (fallthrough) | Expr varlığı ve EP_IntValue kararları bu %T probunda çalışmadı. Alternatif %#T denetimi ayrı. |
| `printf.c:1081` | branch  1 never executed | Expr varlığı ve EP_IntValue kararları bu %T probunda çalışmadı. Alternatif %#T denetimi ayrı. |
| `printf.c:1081` | branch  2 never executed (fallthrough) | Expr varlığı ve EP_IntValue kararları bu %T probunda çalışmadı. Alternatif %#T denetimi ayrı. |
| `printf.c:1081` | branch  3 never executed | Expr varlığı ve EP_IntValue kararları bu %T probunda çalışmadı. Alternatif %#T denetimi ayrı. |
| `printf.c:1089` | branch  1 taken 0 | NULL veya boş token yolu alınmadı. Ölçülen DDL adı ve şema adı ayrıştırıcının geçerli token'ıdır. |
| `printf.c:1089` | branch  3 taken 0 | NULL veya boş token yolu alınmadı. Ölçülen DDL adı ve şema adı ayrıştırıcının geçerli token'ıdır. |
| `build.c:1047` | branch  0 taken 0 (fallthrough) | Şema yüklenirken nitelikli adla CORRUPT dönüşü alınmadı. Rust var olan şema yükleme davranışını kullanır. |
| `build.c:1291` | branch  4 taken 0 (fallthrough) | TEMP bildirimi için bilinen temp dışı şema nitelemesi kolu alınmadı. D144 adı çakışması ve bilinmeyen şema dışındaki hata sırası açık. |
| `build.c:1299` | branch  0 taken 0 (fallthrough) | IN_RENAME_OBJECT yeniden adlandırma token haritası yolu bu DDL matrisine dahil değil. |
| `build.c:1304` | branch  0 taken 0 (fallthrough) | İsim tahsisi hatası yok. SQLite NOMEM ve Rust tahsis hataları ayrıca açık. |
| `build.c:1305` | branch  3 taken 0 (fallthrough) | Ayrılmış veya geçersiz nesne adı reddi alınmadı. Rust önceki doğrulamayı kullanır, genel reserved-name eşliği iddia edilmez. |
| `build.c:1320` | branch  3 taken 0 (fallthrough) | INSERT authorizer reddi alınmadı. D144 köprüsü authorizer etkin bağlantıda devreye girmez. |
| `build.c:1323` | branch  1 taken 0 | VIRTUAL ve CREATE authorizer ret yolları bu profilde çalışmadı. Authorizer/CREATE VIRTUAL TABLE sırası açık. |
| `build.c:1323` | branch  3 taken 0 (fallthrough) | VIRTUAL ve CREATE authorizer ret yolları bu profilde çalışmadı. Authorizer/CREATE VIRTUAL TABLE sırası açık. |
| `build.c:1337` | branch  1 taken 0 | Özel parse modu namespace kontrolünü atlamadı. D144 normal hazırlama yolunu kapsar. |
| `build.c:1339` | branch  1 taken 0 (fallthrough) | sqlite3ReadSchema hatası enjekte edilmedi. Bozuk şema, kilit ve I/O sonucu ayrıca denetlenmeli. |
