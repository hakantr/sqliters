# V6b.32 — MMAP/TEMP C kararları ve kapsamı

Ölçüm ana SQLite 3.54.1 `-O0 --coverage` profiline aittir. Tam yardımcılar ve belirtilen kısmi bloklar ayrı satırdır. Ana dal tablosuyla örtüşen kaynak noktaları olabilir; sayılar benzersiz kaynak dalları olarak toplanmaz. Her gcov kısa devre çıkışı ayrı gösterilir.

| C kapsamı | Yordam/blok | Çalışan satır | Alınan dal |
|---|---|---|---|
| pragma.c:951–987 | MMAP_pragma | 14/16 | 17/18 |
| os.c:157–159 | sqlite3OsFileControlHint | 2/2 | 2/2 |
| pager.c:2030–2035 | pagerFlushOnCommit | 5/5 | 6/6 |
| pager.c:3738–3754 | pagerOpentemp | 4/4 | 0/0 |
| pager.c:3603–3606 | sqlite3PagerSetMmapLimit | 3/3 | 0/0 |
| pager.c:3587–3598 | pagerFixMaplimit | 8/8 | 3/4 |
| pager.c:1710–1717 | MMAP_read_journal_geometry | 1/1 | 0/0 |
| pager.c:3007–3010 | MMAP_end_playback_geometry | 3/3 | 1/2 |
| pager.c:3873–3878 | MMAP_geometry_hint | 4/4 | 3/4 |
| pager.c:6550–6562 | TEMP_commit_flush | 4/4 | 6/6 |
| pager.c:2175–2187 | TEMP_end_transaction | 8/8 | 7/8 |
| btree.c:3051–3058 | sqlite3BtreeSetMmapLimit | 5/5 | 0/0 |
| pcache.c:907–913 | sqlite3PCachePercentDirty | 4/4 | 3/4 |
| pcache.c:641–648 | sqlite3PcacheClearWritable | 4/4 | 2/2 |
| os_unix.c:4228–4251 | MMAP_unix | 12/12 | 10/10 |
| main.c:4224–4240 | MMAP_file_selection | 7/7 | 2/2 |
| main.c:4268–4270 | MMAP_file_delegate | 2/2 | 0/0 |

Bu profilde **68 dal**, **6 alınmayan dal** var. Dalın alınmış olması Rust eşliği anlamına gelmez: `os_unix.c:4243` üzerindeki nFetchOut guardı ve `4245` içindeki mevcut haritayı yenileme Rust’ta tam karşılanmıyor. Altı native akış D142 olarak açıktır. VFS sınırı taşındı; gerçek mapped-page lease ve mmap/unmap davranışı tamamlandı sayılmaz.

## Ölçülen kararların tümü

| C yeri | Koşul | Gcov sonucu | Rust yeri / durum |
|---|---|---|---|
| `pragma.c:955` | `if( zRight ){` | branch  0 taken 876 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:955` | `if( zRight ){` | branch  1 taken 1458 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:958` | `if( sz<0 ) sz = sqlite3GlobalConfig.szMmap;` | branch  0 taken 72 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:958` | `if( sz<0 ) sz = sqlite3GlobalConfig.szMmap;` | branch  1 taken 804 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:959` | `if( pId2->n==0 ) db->szMmap = sz;` | branch  0 taken 620 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:959` | `if( pId2->n==0 ) db->szMmap = sz;` | branch  1 taken 256 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:960` | `for(ii=db->nDb-1; ii>=0; ii--){` | branch  0 taken 1844 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:960` | `for(ii=db->nDb-1; ii>=0; ii--){` | branch  1 taken 876 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  0 taken 1340 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  1 taken 504 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  2 taken 464 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  3 taken 876 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  4 taken 280 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:961` | `if( db->aDb[ii].pBt && (ii==iDb \|\| pId2->n==0) ){` | branch  5 taken 184 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:972` | `if( rc==SQLITE_OK ){` | branch  0 taken 1070 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:972` | `if( rc==SQLITE_OK ){` | branch  1 taken 1264 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:974` | `}else if( rc!=SQLITE_NOTFOUND ){` | branch  0 taken 0 (fallthrough) | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pragma.c:974` | `}else if( rc!=SQLITE_NOTFOUND ){` | branch  1 taken 1264 | `crates/sqliters-codegen/src/pragma.rs:278` |
| `os.c:158` | `if( id->pMethods ) (void)id->pMethods->xFileControl(id, op, pArg);` | branch  0 taken 83847 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2620` |
| `os.c:158` | `if( id->pMethods ) (void)id->pMethods->xFileControl(id, op, pArg);` | branch  1 taken 42554 | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:2031` | `if( pPager->tempFile==0 ) return 1;` | branch  0 taken 31960 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:2031` | `if( pPager->tempFile==0 ) return 1;` | branch  1 taken 60050 | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:2032` | `if( !bCommit ) return 0;` | branch  0 taken 5010 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:2032` | `if( !bCommit ) return 0;` | branch  1 taken 55040 | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:2033` | `if( !isOpen(pPager->fd) ) return 0;` | branch  0 taken 53774 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:2033` | `if( !isOpen(pPager->fd) ) return 0;` | branch  1 taken 1266 | `crates/sqliters-pager/src/lib.rs:4749` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  0 taken 43308 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  1 taken 129878 | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  2 taken 43308 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3590` | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | branch  3 taken 0 | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3008` | `if( rc==SQLITE_OK ){` | branch  0 taken 1041 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5218` |
| `pager.c:3008` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5218` |
| `pager.c:3873` | `if( rc==SQLITE_OK ){` | branch  0 taken 114963 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3873` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3874` | `if( nReserve<0 ) nReserve = pPager->nReserve;` | branch  0 taken 58260 (fallthrough) | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3874` | `if( nReserve<0 ) nReserve = pPager->nReserve;` | branch  1 taken 56703 | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:6551` | `if( pPager->eState<PAGER_WRITER_CACHEMOD ) return SQLITE_OK;` | branch  0 taken 255 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:6551` | `if( pPager->eState<PAGER_WRITER_CACHEMOD ) return SQLITE_OK;` | branch  1 taken 60514 | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:6555` | `if( 0==pagerFlushOnCommit(pPager, 1) ){` | branch  1 taken 44506 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:6555` | `if( 0==pagerFlushOnCommit(pPager, 1) ){` | branch  2 taken 16008 | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:6562` | `if( pagerUseWal(pPager) ){` | branch  0 taken 1799 (fallthrough) | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:6562` | `if( pagerUseWal(pPager) ){` | branch  1 taken 14209 | `crates/sqliters-pager/src/lib.rs:4866` |
| `pager.c:2177` | `if( rc==SQLITE_OK ){` | branch  0 taken 67258 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2177` | `if( rc==SQLITE_OK ){` | branch  1 taken 0 | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  0 taken 31496 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  1 taken 35762 | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  3 taken 16585 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2178` | `if( MEMDB \|\| pagerFlushOnCommit(pPager, bCommit) ){` | branch  4 taken 14911 | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2186` | `if( pagerUseWal(pPager) ){` | branch  0 taken 1968 (fallthrough) | `crates/sqliters-pager/src/lib.rs:5094` |
| `pager.c:2186` | `if( pagerUseWal(pPager) ){` | branch  1 taken 65290 | `crates/sqliters-pager/src/lib.rs:5094` |
| `pcache.c:911` | `for(pDirty=pCache->pDirty; pDirty; pDirty=pDirty->pDirtyNext) nDirty++;` | branch  0 taken 4226 | `crates/sqliters-pager/src/pcache.rs:992` |
| `pcache.c:911` | `for(pDirty=pCache->pDirty; pDirty; pDirty=pDirty->pDirtyNext) nDirty++;` | branch  1 taken 1266 (fallthrough) | `crates/sqliters-pager/src/pcache.rs:992` |
| `pcache.c:912` | `return nCache ? (int)(((i64)nDirty * 100) / nCache) : 0;` | branch  0 taken 1266 (fallthrough) | `crates/sqliters-pager/src/pcache.rs:992` |
| `pcache.c:912` | `return nCache ? (int)(((i64)nDirty * 100) / nCache) : 0;` | branch  1 taken 0 | `crates/sqliters-pager/src/pcache.rs:992` |
| `pcache.c:644` | `for(p=pCache->pDirty; p; p=p->pDirtyNext){` | branch  0 taken 24911 | `crates/sqliters-pager/src/pcache.rs:981` |
| `pcache.c:644` | `for(p=pCache->pDirty; p; p=p->pDirtyNext){` | branch  1 taken 14911 (fallthrough) | `crates/sqliters-pager/src/pcache.rs:981` |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  0 taken 18 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:2128` |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  1 taken 44386 | `crates/sqliters-vfs/src/lib.rs:2128` |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  0 taken 43334 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  1 taken 1070 | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  2 taken 372 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  3 taken 42962 | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  4 taken 366 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  5 taken 6 | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  0 taken 6 (fallthrough) | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  1 taken 360 | `crates/sqliters-vfs/src/lib.rs:2128` **Kısmi/açık:** nFetchOut veya gerçek harita yaşamı karşılığı eksik; D142. |
| `main.c:4233` | `if( pBtree ){` | branch  0 taken 113738 (fallthrough) | `crates/sqliters-core/src/cgen.rs:110` |
| `main.c:4233` | `if( pBtree ){` | branch  1 taken 5859 | `crates/sqliters-core/src/cgen.rs:110` |

## Hiç alınmayan dallar

| C yeri / dal | Koşul | Sınıflandırma | Rust yeri |
|---|---|---|---|
| `pragma.c:974`, branch  0 taken 0 (fallthrough) | `}else if( rc!=SQLITE_NOTFOUND ){` | MMAP file-control sorgusunun NOTFOUND dışındaki hata yönü alınmadı. Rust Parse rc/nErr karşılığı var; özel VFS hata enjeksiyonu ayrıca açık. | `crates/sqliters-codegen/src/pragma.rs:278` |
| `pager.c:3590`, branch  3 taken 0 | `if( isOpen(fd) && fd->pMethods->iVersion>=3 ){` | Açık dosyada iVersion<3 yönü alınmadı. Rust VfsFile ayrı iVersion modeli taşımaz; eski özel VFS hint davranışı eşitlenmiş sayılmaz. | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:3008`, branch  1 taken 0 | `if( rc==SQLITE_OK ){` | Journal playback sonunda rc!=OK yönü bu profilde alınmadı. GÇ/tahsis hataları ve pager ERROR (E28) ayrıca açık. | `crates/sqliters-pager/src/lib.rs:5218` |
| `pager.c:3873`, branch  1 taken 0 | `if( rc==SQLITE_OK ){` | PagerSetPagesize hata yönü alınmadı. Genel dosya-boyutu/NOMEM hata enjeksiyonu ve hata sonrası hint izi eşliği açık. | `crates/sqliters-pager/src/lib.rs:2620` |
| `pager.c:2177`, branch  1 taken 0 | `if( rc==SQLITE_OK ){` | Journal finalization rc!=OK yönü bu TEMP matrisinde alınmadı. Hata halinde dirty/writable yaşamı ayrıca kanıtlanmadı. | `crates/sqliters-pager/src/lib.rs:5094` |
| `pcache.c:912`, branch  1 taken 0 | `return nCache ? (int)(((i64)nDirty * 100) / nCache) : 0;` | nCache=0 yönü bu TEMP matrisinde alınmadı. Rust percent_dirty sıfır bölme guardını taşır; bu girdi ayrıca kanıtlanmadı. | `crates/sqliters-pager/src/pcache.rs:992` |
