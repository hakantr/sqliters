# V6b.39 — Modül kaydı C kararları ve dal kapsamı

SQLite 3.54.1 `-O0 --coverage` referans nesnesinde 46 native akış ve 96 modüllü bir kayıt geçmişi çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `build.c:416–454` | `LocateEponymous` | 15/16 | 20/24 |
| `hash.c:242–270` | `ModuleHashHistory` | 18/18 | 13/14 |
| `pragma.c:1489–1497` | `ModuleList` | 6/6 | 2/2 |
| `select.c:5788–5833` | `CteSource` | 11/16 | 10/16 |
| `vtab.c:39–78` | `ModuleRegistration` | 19/24 | 6/8 |

64 dalın 51'i alındı. Aşağıdaki 13 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `build.c:420` | `if( (db->mDbFlags & DBFLAG_SchemaKnownOk)==0` | branch  0 taken 51 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:420` | `if( (db->mDbFlags & DBFLAG_SchemaKnownOk)==0` | branch  1 taken 194 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:421` | `&& SQLITE_OK!=sqlite3ReadSchema(pParse)` | branch  1 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:421` | `&& SQLITE_OK!=sqlite3ReadSchema(pParse)` | branch  2 taken 51 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:427` | `if( p==0 ){` | branch  0 taken 66 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:427` | `if( p==0 ){` | branch  1 taken 179 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:432` | `if( (pParse->prepFlags & SQLITE_PREPARE_NO_VTAB)==0 && db->init.busy==0 ){` | branch  0 taken 66 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:432` | `if( (pParse->prepFlags & SQLITE_PREPARE_NO_VTAB)==0 && db->init.busy==0 ){` | branch  1 taken 0 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:432` | `if( (pParse->prepFlags & SQLITE_PREPARE_NO_VTAB)==0 && db->init.busy==0 ){` | branch  2 taken 66 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:432` | `if( (pParse->prepFlags & SQLITE_PREPARE_NO_VTAB)==0 && db->init.busy==0 ){` | branch  3 taken 0 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:434` | `if( pMod==0 && sqlite3_strnicmp(zName, "pragma_", 7)==0 ){` | branch  0 taken 61 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:434` | `if( pMod==0 && sqlite3_strnicmp(zName, "pragma_", 7)==0 ){` | branch  1 taken 5 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:434` | `if( pMod==0 && sqlite3_strnicmp(zName, "pragma_", 7)==0 ){` | branch  3 taken 22 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:434` | `if( pMod==0 && sqlite3_strnicmp(zName, "pragma_", 7)==0 ){` | branch  4 taken 39 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:438` | `if( pMod==0 && sqlite3_strnicmp(zName, "json", 4)==0 ){` | branch  0 taken 39 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:438` | `if( pMod==0 && sqlite3_strnicmp(zName, "json", 4)==0 ){` | branch  1 taken 27 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:438` | `if( pMod==0 && sqlite3_strnicmp(zName, "json", 4)==0 ){` | branch  3 taken 32 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:438` | `if( pMod==0 && sqlite3_strnicmp(zName, "json", 4)==0 ){` | branch  4 taken 7 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:447` | `if( pMod && sqlite3VtabEponymousTableInit(pParse, pMod) ){` | branch  0 taken 59 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:447` | `if( pMod && sqlite3VtabEponymousTableInit(pParse, pMod) ){` | branch  1 taken 7 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:447` | `if( pMod && sqlite3VtabEponymousTableInit(pParse, pMod) ){` | branch  3 taken 59 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:447` | `if( pMod && sqlite3VtabEponymousTableInit(pParse, pMod) ){` | branch  4 taken 0 | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:453` | `if( flags & LOCATE_NOERR ) return 0;` | branch  0 taken 4 (fallthrough) | `crates/sqliters-core/src/vtab.rs:302` |
| `build.c:453` | `if( flags & LOCATE_NOERR ) return 0;` | branch  1 taken 3 | `crates/sqliters-core/src/vtab.rs:302` |
| `hash.c:250` | `if( elem->data ){` | branch  0 taken 5 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:250` | `if( elem->data ){` | branch  1 taken 1253 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:252` | `if( data==0 ){` | branch  0 taken 4 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:252` | `if( data==0 ){` | branch  1 taken 1 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:260` | `if( data==0 ) return 0;` | branch  0 taken 8 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:260` | `if( data==0 ) return 0;` | branch  1 taken 1245 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:262` | `if( new_elem==0 ) return data;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:262` | `if( new_elem==0 ) return data;` | branch  1 taken 1245 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:267` | `if( pH->count>=5 && pH->count > 2*pH->htsize ){` | branch  0 taken 588 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:267` | `if( pH->count>=5 && pH->count > 2*pH->htsize ){` | branch  1 taken 657 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:267` | `if( pH->count>=5 && pH->count > 2*pH->htsize ){` | branch  2 taken 97 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:267` | `if( pH->count>=5 && pH->count > 2*pH->htsize ){` | branch  3 taken 491 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:270` | `insertElement(pH, pH->ht ? &pH->ht[new_elem->h % pH->htsize] : 0, new_elem);` | branch  0 taken 588 (fallthrough) | `crates/sqliters-codegen/src/trigger.rs:488` |
| `hash.c:270` | `insertElement(pH, pH->ht ? &pH->ht[new_elem->h % pH->htsize] : 0, new_elem);` | branch  1 taken 657 | `crates/sqliters-codegen/src/trigger.rs:488` |
| `pragma.c:1492` | `for(j=sqliteHashFirst(&db->aModule); j; j=sqliteHashNext(j)){` | branch  0 taken 802 | `crates/sqliters-codegen/src/pragma.rs:279` |
| `pragma.c:1492` | `for(j=sqliteHashFirst(&db->aModule); j; j=sqliteHashNext(j)){` | branch  1 taken 71 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:279` |
| `select.c:5788` | `if( pParse->pWith==0 ){` | branch  0 taken 147 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5788` | `if( pParse->pWith==0 ){` | branch  1 taken 34 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5792` | `if( pParse->nErr ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5792` | `if( pParse->nErr ){` | branch  1 taken 34 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5798` | `if( pFrom->fg.fixedSchema==0 && pFrom->u4.zDatabase!=0 ){` | branch  0 taken 34 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5798` | `if( pFrom->fg.fixedSchema==0 && pFrom->u4.zDatabase!=0 ){` | branch  1 taken 0 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5798` | `if( pFrom->fg.fixedSchema==0 && pFrom->u4.zDatabase!=0 ){` | branch  2 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5798` | `if( pFrom->fg.fixedSchema==0 && pFrom->u4.zDatabase!=0 ){` | branch  3 taken 34 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5803` | `if( pFrom->fg.notCte ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5803` | `if( pFrom->fg.notCte ){` | branch  1 taken 34 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5812` | `if( pCte ){` | branch  0 taken 22 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5812` | `if( pCte ){` | branch  1 taken 12 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5828` | `if( pCte->zCteErr ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5828` | `if( pCte->zCteErr ){` | branch  1 taken 22 | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5832` | `if( cannotBeFunction(pParse, pFrom) ) return 2;` | branch  1 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:1864` |
| `select.c:5832` | `if( cannotBeFunction(pParse, pFrom) ) return 2;` | branch  2 taken 22 | `crates/sqliters-core/src/vtab.rs:1864` |
| `vtab.c:49` | `if( pModule==0 ){` | branch  0 taken 2 (fallthrough) | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:49` | `if( pModule==0 ){` | branch  1 taken 387 | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:55` | `if( pMod==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:55` | `if( pMod==0 ){` | branch  1 taken 387 | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:69` | `if( pDel ){` | branch  0 taken 3 (fallthrough) | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:69` | `if( pDel ){` | branch  1 taken 386 | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:70` | `if( pDel==pMod ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:281` |
| `vtab.c:70` | `if( pDel==pMod ){` | branch  1 taken 3 | `crates/sqliters-core/src/vtab.rs:281` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `build.c:421` | branch  1 taken 0 (fallthrough) | Şema okuma başarısızlığı enjekte edilmedi. Açık normal sorgu kapsamı. |
| `build.c:432` | branch  1 taken 0 | SQLITE_PREPARE_NO_VTAB bayrağıyla eponymous kayıt engellenmesi bu API matrisinde yok. |
| `build.c:432` | branch  3 taken 0 | Şema init.busy sırasında eponymous kayıt kontrolü bu akışlarda atlanmadı. |
| `build.c:447` | branch  4 taken 0 | Mevcut modülün eponymous tablo başlatması başarısız olmadı. |
| `hash.c:262` | branch  0 taken 0 (fallthrough) | HashElem tahsis hatası enjekte edilmedi. |
| `select.c:5792` | branch  0 taken 0 (fallthrough) | Daha önce hata almış CTE çözümlemesi matriste yok. |
| `select.c:5798` | branch  1 taken 0 | Sabit şema bayrağı CTE aday yolunda gözlenmedi. |
| `select.c:5798` | branch  2 taken 0 (fallthrough) | Şema nitelemeli FROM kaynağı CTE kontrolünde bu çıkışa ulaşmadı. |
| `select.c:5803` | branch  0 taken 0 (fallthrough) | Trigger ve UPDATE hedefi için notCte yolu alınmadı. |
| `select.c:5828` | branch  0 taken 0 (fallthrough) | Hatalı döngüsel CTE referansı bu matriste kullanılmadı. |
| `select.c:5832` | branch  1 taken 0 (fallthrough) | CTE'nin tablo işlevi olarak yanlış çağrılması bu matriste kullanılmadı. |
| `vtab.c:55` | branch  0 taken 0 (fallthrough) | Module nesnesinin tahsis hatası enjekte edilmedi. |
| `vtab.c:70` | branch  0 taken 0 (fallthrough) | Hash ekleme tahsis hatasında yeni pMod geri dönen yol alınmadı. |
