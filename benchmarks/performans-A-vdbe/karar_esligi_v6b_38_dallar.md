# V6b.38 — D103 C kararları ve dal kapsamı

SQLite 3.54.1 `-O0 --coverage` referans nesnesinde 184 native akış çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `pragma.c:2896–2931` | `BestIndex` | 24/27 | 10/14 |
| `pragma.c:2983–3028` | `Filter` | 24/26 | 11/14 |
| `whereexpr.c:1938–1984` | `TabFuncArgs` | 22/25 | 15/18 |

46 dalın 36'sı alındı. Aşağıdaki 10 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `pragma.c:2903` | `if( pTab->nHidden==0 ){ return SQLITE_OK; }` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2903` | `if( pTab->nHidden==0 ){ return SQLITE_OK; }` | branch  1 taken 176 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2907` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  0 taken 280 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2907` | `for(i=0; i<pIdxInfo->nConstraint; i++, pConstraint++){` | branch  1 taken 176 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2908` | `if( pConstraint->iColumn < pTab->iHidden ) continue;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2908` | `if( pConstraint->iColumn < pTab->iHidden ) continue;` | branch  1 taken 280 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2909` | `if( pConstraint->op!=SQLITE_INDEX_CONSTRAINT_EQ ) continue;` | branch  0 taken 48 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2909` | `if( pConstraint->op!=SQLITE_INDEX_CONSTRAINT_EQ ) continue;` | branch  1 taken 232 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2910` | `if( pConstraint->usable==0 ) return SQLITE_CONSTRAINT;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2910` | `if( pConstraint->usable==0 ) return SQLITE_CONSTRAINT;` | branch  1 taken 232 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2915` | `if( seen[0]==0 ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2915` | `if( seen[0]==0 ){` | branch  1 taken 168 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2925` | `if( seen[1] ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2925` | `if( seen[1] ){` | branch  1 taken 168 | `crates/sqliters-core/src/vtab.rs:3661` |
| `pragma.c:2999` | `for(i=0; i<argc; i++, j++){` | branch  0 taken 152 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:2999` | `for(i=0; i<argc; i++, j++){` | branch  1 taken 160 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3003` | `if( zText ){` | branch  0 taken 144 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3003` | `if( zText ){` | branch  1 taken 8 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3005` | `if( pCsr->azArg[j]==0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3005` | `if( pCsr->azArg[j]==0 ){` | branch  1 taken 144 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3012` | `if( pCsr->azArg[1] ){` | branch  0 taken 144 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3012` | `if( pCsr->azArg[1] ){` | branch  1 taken 16 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3016` | `if( pCsr->azArg[0] ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3016` | `if( pCsr->azArg[0] ){` | branch  1 taken 160 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3020` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3020` | `if( zSql==0 ) return SQLITE_NOMEM;` | branch  1 taken 160 | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3023` | `if( rc!=SQLITE_OK ){` | branch  0 taken 16 (fallthrough) | `crates/sqliters-core/src/vtab.rs:3781` |
| `pragma.c:3023` | `if( rc!=SQLITE_OK ){` | branch  1 taken 144 | `crates/sqliters-core/src/vtab.rs:3781` |
| `whereexpr.c:1948` | `if( pItem->fg.isTabFunc==0 ) return;` | branch  0 taken 1112 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1948` | `if( pItem->fg.isTabFunc==0 ) return;` | branch  1 taken 176 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1951` | `if( !IsVirtual(pTab) ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1951` | `if( !IsVirtual(pTab) ){` | branch  1 taken 176 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1956` | `if( pArgs==0 ) return;` | branch  0 taken 16 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1956` | `if( pArgs==0 ) return;` | branch  1 taken 160 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1957` | `for(j=k=0; j<pArgs->nExpr; j++){` | branch  0 taken 168 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1957` | `for(j=k=0; j<pArgs->nExpr; j++){` | branch  1 taken 152 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1960` | `while( k<pTab->nCol && (pTab->aCol[k].colFlags & COLFLAG_HIDDEN)==0 ){k++;}` | branch  0 taken 320 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1960` | `while( k<pTab->nCol && (pTab->aCol[k].colFlags & COLFLAG_HIDDEN)==0 ){k++;}` | branch  1 taken 8 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1960` | `while( k<pTab->nCol && (pTab->aCol[k].colFlags & COLFLAG_HIDDEN)==0 ){k++;}` | branch  2 taken 160 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1960` | `while( k<pTab->nCol && (pTab->aCol[k].colFlags & COLFLAG_HIDDEN)==0 ){k++;}` | branch  3 taken 160 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1961` | `if( k>=pTab->nCol ){` | branch  0 taken 8 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1961` | `if( k>=pTab->nCol ){` | branch  1 taken 160 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1967` | `if( pColRef==0 ) return;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1967` | `if( pColRef==0 ) return;` | branch  1 taken 160 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1976` | `if( pItem->fg.jointype & (JT_LEFT\|JT_RIGHT) ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/whereexpr.rs:1621` |
| `whereexpr.c:1976` | `if( pItem->fg.jointype & (JT_LEFT\|JT_RIGHT) ){` | branch  1 taken 160 | `crates/sqliters-codegen/src/whereexpr.rs:1621` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `pragma.c:2903` | branch  0 taken 0 (fallthrough) | Gizli sütunu olmayan PRAGMA modülü bu locking_mode matrisine dahil değil. |
| `pragma.c:2908` | branch  0 taken 0 (fallthrough) | Görünür sütunda iletilen kısıtın atlanması bu matriste alınmadı. |
| `pragma.c:2910` | branch  0 taken 0 (fallthrough) | Kullanılamayan gizli eşitlik, join sıralaması sırasında oluşabilir. Tek kaynağın kapsamı dışında. |
| `pragma.c:2925` | branch  0 taken 0 (fallthrough) | İkinci gizli sütun yok. İkinci argvIndex bu şema kipinde alınmaz. |
| `pragma.c:3005` | branch  0 taken 0 (fallthrough) | Argümanın metin kopyası tahsis edilemediğinde NOMEM döner. Hata enjekte edilmedi. |
| `pragma.c:3016` | branch  0 taken 0 (fallthrough) | Bu PRAGMA bir ayar değeri almaz. İç PRAGMA'ya =%Q eklenmesi bu modül için alınmadı. |
| `pragma.c:3020` | branch  0 taken 0 (fallthrough) | İç SQL dizgesi tahsis hatası ve SQL uzunluk sınırı matrise dahil edilmedi. |
| `whereexpr.c:1951` | branch  0 taken 0 (fallthrough) | Fiziksel tabloyu işlev olarak çağırma bu matris kapsamında çalışmadı. |
| `whereexpr.c:1967` | branch  0 taken 0 (fallthrough) | Gizli sütun ifadesinin tahsis hatası enjekte edilmedi. |
| `whereexpr.c:1976` | branch  0 taken 0 (fallthrough) | OUTER JOIN özgül koşulu tek kaynaklı sorgu matrisinde alınmadı. |
