# V6b.37 — İlk şema sayfası C kararları ve dal kapsamı

SQLite 3.54.1 `-O0 --coverage` referans nesnesinde 468 açılış ve 48 eskime akışı çalıştı. Kapsam C tarafındaki çıktıyı sayar. Rust sonucu ham uygulama matrisi ve karar tablosuyla ayrıca karşılaştırıldı.

| C kapsamı | Yordam / blok | Çalışan satır | Alınan dal |
|---|---|---:|---:|
| `vdbe.c:4318–4354` | `VdbeSchemaCheck` | 11/11 | 10/12 |
| `btree.c:3540–3576` | `NewDatabase` | 24/24 | 3/4 |
| `btree.c:3743–3755` | `BeginWrite` | 5/8 | 5/12 |
| `btree.c:3817–3846` | `SchemaVersion` | 15/15 | 13/16 |

44 dalın 31'i alındı. Aşağıdaki 13 alınmayan dal ayrı satırlarda sınıflandırıldı.

| C yeri | Koşul | Gcov çıkışı | Rust karşılığı |
|---|---|---|---|
| `btree.c:3546` | `if( pBt->nPage>0 ){` | branch  0 taken 442 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3698` |
| `btree.c:3546` | `if( pBt->nPage>0 ){` | branch  1 taken 768 | `crates/sqliters-pager/src/lib.rs:3698` |
| `btree.c:3553` | `if( rc ) return rc;` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3698` |
| `btree.c:3553` | `if( rc ) return rc;` | branch  1 taken 768 | `crates/sqliters-pager/src/lib.rs:3698` |
| `btree.c:3743` | `if( rc==SQLITE_OK && wrflag ){` | branch  0 taken 1868 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3743` | `if( rc==SQLITE_OK && wrflag ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3743` | `if( rc==SQLITE_OK && wrflag ){` | branch  2 taken 850 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3743` | `if( rc==SQLITE_OK && wrflag ){` | branch  3 taken 1018 | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3744` | `if( (pBt->btsFlags & BTS_READ_ONLY)!=0 ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3744` | `if( (pBt->btsFlags & BTS_READ_ONLY)!=0 ){` | branch  1 taken 850 | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3748` | `if( rc==SQLITE_OK ){` | branch  0 taken 850 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3748` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3750` | `}else if( rc==SQLITE_BUSY_SNAPSHOT && pBt->inTransaction==TRANS_NONE ){` | branch  0 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3750` | `}else if( rc==SQLITE_BUSY_SNAPSHOT && pBt->inTransaction==TRANS_NONE ){` | branch  1 never executed | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3750` | `}else if( rc==SQLITE_BUSY_SNAPSHOT && pBt->inTransaction==TRANS_NONE ){` | branch  2 never executed (fallthrough) | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3750` | `}else if( rc==SQLITE_BUSY_SNAPSHOT && pBt->inTransaction==TRANS_NONE ){` | branch  3 never executed | `crates/sqliters-pager/src/lib.rs:3477` |
| `btree.c:3818` | `if( rc==SQLITE_OK ){` | branch  0 taken 1868 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3818` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3819` | `if( pSchemaVersion ){` | branch  0 taken 1098 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3819` | `if( pSchemaVersion ){` | branch  1 taken 770 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3822` | `if( wrflag ){` | branch  0 taken 850 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3822` | `if( wrflag ){` | branch  1 taken 1018 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3837` | `if( p->sharable` | branch  0 taken 3450 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3837` | `if( p->sharable` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3838` | `\|\| p->inTrans==TRANS_NONE` | branch  0 taken 1630 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3838` | `\|\| p->inTrans==TRANS_NONE` | branch  1 taken 1820 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3839` | `\|\| (p->inTrans==TRANS_READ && wrflag!=0)` | branch  0 taken 676 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3839` | `\|\| (p->inTrans==TRANS_READ && wrflag!=0)` | branch  1 taken 954 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3839` | `\|\| (p->inTrans==TRANS_READ && wrflag!=0)` | branch  2 taken 48 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3839` | `\|\| (p->inTrans==TRANS_READ && wrflag!=0)` | branch  3 taken 628 | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3844` | `if( pSchemaVersion ){` | branch  0 taken 1582 (fallthrough) | `crates/sqliters-pager/src/lib.rs:3690` |
| `btree.c:3844` | `if( pSchemaVersion ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:3690` |
| `vdbe.c:4319` | `if( rc==SQLITE_OK` | branch  0 taken 2788 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4319` | `if( rc==SQLITE_OK` | branch  1 taken 0 | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4320` | `&& pOp->p5` | branch  0 taken 1194 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4320` | `&& pOp->p5` | branch  1 taken 1594 | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4321` | `&& (iMeta!=pOp->p3 \|\| pDb->pSchema->iGeneration!=pOp->p4.i)` | branch  0 taken 1158 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4321` | `&& (iMeta!=pOp->p3 \|\| pDb->pSchema->iGeneration!=pOp->p4.i)` | branch  1 taken 36 | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4321` | `&& (iMeta!=pOp->p3 \|\| pDb->pSchema->iGeneration!=pOp->p4.i)` | branch  2 taken 0 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4321` | `&& (iMeta!=pOp->p3 \|\| pDb->pSchema->iGeneration!=pOp->p4.i)` | branch  3 taken 1158 | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4343` | `if( db->aDb[pOp->p1].pSchema->schema_cookie!=iMeta ){` | branch  0 taken 12 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4343` | `if( db->aDb[pOp->p1].pSchema->schema_cookie!=iMeta ){` | branch  1 taken 24 | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4354` | `if( rc ) goto abort_due_to_error;` | branch  0 taken 36 (fallthrough) | `crates/sqliters-core/src/lib.rs:1983` |
| `vdbe.c:4354` | `if( rc ) goto abort_due_to_error;` | branch  1 taken 2752 | `crates/sqliters-core/src/lib.rs:1983` |

## Alınmayan çıkışlar

| C yeri | Gcov çıkışı | Sınıf ve kanıt sınırı |
|---|---|---|
| `vdbe.c:4319` | branch  1 taken 0 | OP_Transaction öncesi işlem hatası bu probda dönmedi. BUSY/IOERR yaşamı ayrı doğrulama. |
| `vdbe.c:4321` | branch  2 taken 0 (fallthrough) | Çerez değişmeden salt şema neslinin ayrıştığı çıkış bu DDL matrisinde görülmedi. |
| `btree.c:3553` | branch  0 taken 0 (fallthrough) | İlk sayfayı sqlite3PagerWrite ile işaretlerken hata verilmedi. VFS hata enjeksiyonu açık. |
| `btree.c:3743` | branch  1 taken 0 | LockBtree hata yolu işlemin yazıcı dalına girmedi. Sadece başarılı kilit durumları denendi. |
| `btree.c:3744` | branch  0 taken 0 (fallthrough) | Yalnız okunabilir dosyada yazıcı açılışı bu matriste denetlenmedi. |
| `btree.c:3748` | branch  1 taken 0 | sqlite3PagerBegin başarısızlığı tetiklenmedi. |
| `btree.c:3750` | branch  0 never executed (fallthrough) | Yazmaya geçiş sırasında BUSY_SNAPSHOT ve önceki işlem durumu sınanmadı. |
| `btree.c:3750` | branch  1 never executed | Yazmaya geçiş sırasında BUSY_SNAPSHOT ve önceki işlem durumu sınanmadı. |
| `btree.c:3750` | branch  2 never executed (fallthrough) | Yazmaya geçiş sırasında BUSY_SNAPSHOT ve önceki işlem durumu sınanmadı. |
| `btree.c:3750` | branch  3 never executed | Yazmaya geçiş sırasında BUSY_SNAPSHOT ve önceki işlem durumu sınanmadı. |
| `btree.c:3818` | branch  1 taken 0 | İşlem açılış hatası sonrası şema çerezi alınmayan kol tetiklenmedi. |
| `btree.c:3837` | branch  1 taken 0 | Paylaşılan B-tree istek yolu alınmadı. |
| `btree.c:3844` | branch  1 taken 0 | Daha önce açık B-tree işleminde NULL şema çerezi çıkış işaretçisi yolu alınmadı. |
