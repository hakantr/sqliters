# V6b.33 — Unix fetch/unfetch kararları ve dal kapsamı

SQLite 3.54.1 özel `-O0 --coverage` profili. Sabit seed354133 ile 10.051 C/model/üretim-Rust VFS işlemi karşılaştırıldı. Aynı profil iki kez çalıştırıldı; sayılar gcov sayacıdır, benzersiz girdi sayısı değildir. C fixture, model ve Rust sonrası bütün dosya hashleri aynı. Kaynak eşlemesi gcov amalgam satırlarından özgün `sqlite/src/os_unix.c` satırlarına yapıldı. Önceki V6b.32 büyük profilinin toplamlarına eklenmez.

| C kapsamı | Yordam/blok | Çalışan satır | Alınan dal |
|---|---|---|---|
| os_unix.c:5711–5735 | unixFetch | 12/12 | 7/8 |
| os_unix.c:5747–5773 | unixUnfetch | 6/6 | 2/2 |
| os_unix.c:5674–5696 | unixMapfile | 10/11 | 7/10 |
| os_unix.c:5560–5568 | unixUnmapfile | 7/7 | 2/2 |
| os_unix.c:5585–5656 | unixRemapfile | 13/25 | 3/14 |
| os_unix.c:4228–4251 | MMAP_control | 12/12 | 10/10 |

Toplam **46 dal**, **15 alınmayan dal**. Başarı yolu, EOF256, limit/aktif fetch guardı ve bireysel/toplu unfetch karşılaştırıldı. Mevcut haritayı uzatma/mremap, mmap hata günlüğü, NOMEM ve OS fstat hata enjeksiyonu tamamlandı sayılmaz. Windows bu Unix profiline dahil değildir. C'deki `sizeof(size_t)<8` kolu 64-bit hedefte derlenmez; Rust Unix backend de 64-bit gerektirir.

## Her koşul ve ölçülen sonuç

| C yeri | Koşul | Gcov sonucu | Rust karşılığı / sınır |
|---|---|---|---|
| `os_unix.c:5718` | `if( pFd->mmapSizeMax>0 ){` | branch  0 taken 6574 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:74 |
| `os_unix.c:5718` | `if( pFd->mmapSizeMax>0 ){` | branch  1 taken 48 | crates/sqliters-vfs/src/fetch.rs:74 |
| `os_unix.c:5724` | `if( pFd->pMapRegion==0 ){` | branch  0 taken 128 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:77 |
| `os_unix.c:5724` | `if( pFd->pMapRegion==0 ){` | branch  1 taken 6446 | crates/sqliters-vfs/src/fetch.rs:77 |
| `os_unix.c:5726` | `if( rc!=SQLITE_OK ) return rc;` | branch  0 taken 0 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:78 |
| `os_unix.c:5726` | `if( rc!=SQLITE_OK ) return rc;` | branch  1 taken 128 | crates/sqliters-vfs/src/fetch.rs:78 |
| `os_unix.c:5728` | `if( pFd->mmapSize >= (iOff+nAmt+nEofBuffer) ){` | branch  0 taken 4574 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:90 |
| `os_unix.c:5728` | `if( pFd->mmapSize >= (iOff+nAmt+nEofBuffer) ){` | branch  1 taken 2000 | crates/sqliters-vfs/src/fetch.rs:90 |
| `os_unix.c:5760` | `if( p ){` | branch  0 taken 4574 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:57 / crates/sqliters-vfs/src/fetch.rs:101 |
| `os_unix.c:5760` | `if( p ){` | branch  1 taken 210 | crates/sqliters-vfs/src/fetch.rs:57 / crates/sqliters-vfs/src/fetch.rs:101 |
| `os_unix.c:5677` | `if( pFd->nFetchOut>0 ) return SQLITE_OK;` | branch  0 taken 0 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:23 unixMapfile doğrudan aktif fetch ile çağrılmadı: bu düzeneğin MMAP_SIZE yolu daha önce outstanding guardında durur, xFetch de mevcut haritayı kullanır. SIZE_HINT/diğer iç girişler bu adımın kapsamı dışında; Rust map çağrıları outstanding=0 önkoşulludur. |
| `os_unix.c:5677` | `if( pFd->nFetchOut>0 ) return SQLITE_OK;` | branch  1 taken 350 | crates/sqliters-vfs/src/fetch.rs:23 unixMapfile doğrudan aktif fetch ile çağrılmadı: bu düzeneğin MMAP_SIZE yolu daha önce outstanding guardında durur, xFetch de mevcut haritayı kullanır. SIZE_HINT/diğer iç girişler bu adımın kapsamı dışında; Rust map çağrıları outstanding=0 önkoşulludur. |
| `os_unix.c:5679` | `if( nMap<0 ){` | branch  0 taken 350 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:24 nMap>=0 doğrudan map-size girişi kullanılmadı; xFetch ve MMAP_SIZE fiziksel dosya boyutuyla nMap=-1 yolundan gelir. SIZE_HINT ve mevcut haritayı uzatma desteği açık. |
| `os_unix.c:5679` | `if( nMap<0 ){` | branch  1 taken 0 | crates/sqliters-vfs/src/fetch.rs:24 nMap>=0 doğrudan map-size girişi kullanılmadı; xFetch ve MMAP_SIZE fiziksel dosya boyutuyla nMap=-1 yolundan gelir. SIZE_HINT ve mevcut haritayı uzatma desteği açık. |
| `os_unix.c:5681` | `if( osFstat(pFd->h, &statbuf) ){` | branch  1 taken 0 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:26 |
| `os_unix.c:5681` | `if( osFstat(pFd->h, &statbuf) ){` | branch  2 taken 350 | crates/sqliters-vfs/src/fetch.rs:26 |
| `os_unix.c:5686` | `if( nMap>pFd->mmapSizeMax ){` | branch  0 taken 216 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:28 |
| `os_unix.c:5686` | `if( nMap>pFd->mmapSizeMax ){` | branch  1 taken 134 | crates/sqliters-vfs/src/fetch.rs:28 |
| `os_unix.c:5691` | `if( nMap!=pFd->mmapSize ){` | branch  0 taken 312 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:29 |
| `os_unix.c:5691` | `if( nMap!=pFd->mmapSize ){` | branch  1 taken 38 | crates/sqliters-vfs/src/fetch.rs:29 |
| `os_unix.c:5562` | `if( pFd->pMapRegion ){` | branch  0 taken 312 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:106 / crates/sqliters-vfs/src/sys/unix.rs:879 |
| `os_unix.c:5562` | `if( pFd->pMapRegion ){` | branch  1 taken 126 | crates/sqliters-vfs/src/fetch.rs:106 / crates/sqliters-vfs/src/sys/unix.rs:879 |
| `os_unix.c:5606` | `if( pOrig ){` | branch  0 taken 0 (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. Mevcut pOrig ile uzatma yönü alınmadı. MMAP_SIZE mevcut haritayı önce kaldırır; doğrudan unixMapfile büyütmesi/mremap Rust tarafında uygulanmadı. Açık kapsam. |
| `os_unix.c:5606` | `if( pOrig ){` | branch  1 taken 312 | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. Mevcut pOrig ile uzatma yönü alınmadı. MMAP_SIZE mevcut haritayı önce kaldırır; doğrudan unixMapfile büyütmesi/mremap Rust tarafında uygulanmadı. Açık kapsam. |
| `os_unix.c:5610` | `if( nReuse<pFd->mmapSizeActual ){` | branch  0 never executed (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. Mevcut haritanın yeniden kullanılmayan kuyruğunu munmap etmenin iki yönü de çalışmadı. Önkoşul pOrig büyütmesi bu düzeneğe dahil değil; Rust karşılığı yok. Açık kapsam. |
| `os_unix.c:5610` | `if( nReuse<pFd->mmapSizeActual ){` | branch  1 never executed | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. Mevcut haritanın yeniden kullanılmayan kuyruğunu munmap etmenin iki yönü de çalışmadı. Önkoşul pOrig büyütmesi bu düzeneğe dahil değil; Rust karşılığı yok. Açık kapsam. |
| `os_unix.c:5616` | `if( nReuse>0 ){` | branch  0 never executed (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. nReuse>0 kararının iki yönü de çalışmadı. Mevcut haritayı uzatma/reuse Rust tarafında yok. Açık kapsam. |
| `os_unix.c:5616` | `if( nReuse>0 ){` | branch  1 never executed | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. nReuse>0 kararının iki yönü de çalışmadı. Mevcut haritayı uzatma/reuse Rust tarafında yok. Açık kapsam. |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  0 never executed (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  1 never executed | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  2 never executed (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | branch  3 never executed | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5640` | `if( pNew==0 ){` | branch  0 taken 312 (fallthrough) | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. pNew!=NULL olan mevcut-harita uzatma yönü alınmadı. Rust bu adımda sıfırdan map/unmap sağlar; mremap yeniden kullanım yolu açık. |
| `os_unix.c:5640` | `if( pNew==0 ){` | branch  1 taken 0 | Karşılık eksik: mevcut haritayı uzatma/mremap; ayrı açık kapsam. pNew!=NULL olan mevcut-harita uzatma yönü alınmadı. Rust bu adımda sıfırdan map/unmap sağlar; mremap yeniden kullanım yolu açık. |
| `os_unix.c:5644` | `if( pNew==MAP_FAILED ){` | branch  0 taken 0 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:31 mmap başarısızlığı enjekte edilmedi. Rust haritayı boş bırakıp limiti0 yapar, fakat C unixLogError(SQLITE_OK) uyarı karşılığı eksiktir. Uyarı dahil tam C eşliği için açık kusur; başarı profili bunu kapatmaz. |
| `os_unix.c:5644` | `if( pNew==MAP_FAILED ){` | branch  1 taken 312 | crates/sqliters-vfs/src/fetch.rs:31 mmap başarısızlığı enjekte edilmedi. Rust haritayı boş bırakıp limiti0 yapar, fakat C unixLogError(SQLITE_OK) uyarı karşılığı eksiktir. Uyarı dahil tam C eşliği için açık kusur; başarı profili bunu kapatmaz. |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  0 taken 1656 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:44 |
| `os_unix.c:4231` | `if( newLimit>sqlite3GlobalConfig.mxMmap ){` | branch  1 taken 4996 | crates/sqliters-vfs/src/fetch.rs:44 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  0 taken 5864 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  1 taken 788 | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  2 taken 4256 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  3 taken 1608 | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  4 taken 372 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4243` | `if( newLimit>=0 && newLimit!=pFile->mmapSizeMax && pFile->nFetchOut==0 ){` | branch  5 taken 3884 | crates/sqliters-vfs/src/fetch.rs:46 |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  0 taken 222 (fallthrough) | crates/sqliters-vfs/src/fetch.rs:48 |
| `os_unix.c:4245` | `if( pFile->mmapSize>0 ){` | branch  1 taken 150 | crates/sqliters-vfs/src/fetch.rs:48 |

## Hiç alınmayan her dal

| C yeri / dal | Koşul | Sınıflandırma |
|---|---|---|
| `os_unix.c:5726` / branch  0 taken 0 (fallthrough) | `if( rc!=SQLITE_OK ) return rc;` | Fstat/map oluşturma hata yönü alınmadı. Rust Result/? ile hatayı yayar; IOERR_FSTAT ileti/iz eşliği hata enjeksiyonuyla kanıtlanmadı. Açık doğrulama. |
| `os_unix.c:5677` / branch  0 taken 0 (fallthrough) | `if( pFd->nFetchOut>0 ) return SQLITE_OK;` | unixMapfile doğrudan aktif fetch ile çağrılmadı: bu düzeneğin MMAP_SIZE yolu daha önce outstanding guardında durur, xFetch de mevcut haritayı kullanır. SIZE_HINT/diğer iç girişler bu adımın kapsamı dışında; Rust map çağrıları outstanding=0 önkoşulludur. |
| `os_unix.c:5679` / branch  1 taken 0 | `if( nMap<0 ){` | nMap>=0 doğrudan map-size girişi kullanılmadı; xFetch ve MMAP_SIZE fiziksel dosya boyutuyla nMap=-1 yolundan gelir. SIZE_HINT ve mevcut haritayı uzatma desteği açık. |
| `os_unix.c:5681` / branch  1 taken 0 (fallthrough) | `if( osFstat(pFd->h, &statbuf) ){` | OS fstat başarısızlığı enjekte edilmedi. Rust IOERR_FSTAT döndürür; hata metni ve syscall izi karşılaştırılmadı. Açık doğrulama. |
| `os_unix.c:5606` / branch  0 taken 0 (fallthrough) | `if( pOrig ){` | Mevcut pOrig ile uzatma yönü alınmadı. MMAP_SIZE mevcut haritayı önce kaldırır; doğrudan unixMapfile büyütmesi/mremap Rust tarafında uygulanmadı. Açık kapsam. |
| `os_unix.c:5610` / branch  0 never executed (fallthrough) | `if( nReuse<pFd->mmapSizeActual ){` | Mevcut haritanın yeniden kullanılmayan kuyruğunu munmap etmenin iki yönü de çalışmadı. Önkoşul pOrig büyütmesi bu düzeneğe dahil değil; Rust karşılığı yok. Açık kapsam. |
| `os_unix.c:5610` / branch  1 never executed | `if( nReuse<pFd->mmapSizeActual ){` | Mevcut haritanın yeniden kullanılmayan kuyruğunu munmap etmenin iki yönü de çalışmadı. Önkoşul pOrig büyütmesi bu düzeneğe dahil değil; Rust karşılığı yok. Açık kapsam. |
| `os_unix.c:5616` / branch  0 never executed (fallthrough) | `if( nReuse>0 ){` | nReuse>0 kararının iki yönü de çalışmadı. Mevcut haritayı uzatma/reuse Rust tarafında yok. Açık kapsam. |
| `os_unix.c:5616` / branch  1 never executed | `if( nReuse>0 ){` | nReuse>0 kararının iki yönü de çalışmadı. Mevcut haritayı uzatma/reuse Rust tarafında yok. Açık kapsam. |
| `os_unix.c:5633` / branch  0 never executed (fallthrough) | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` / branch  1 never executed | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` / branch  2 never executed (fallthrough) | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5633` / branch  3 never executed | `if( pNew==MAP_FAILED \|\| pNew==0 ){` | mremap sonucunun NULL/MAP_FAILED kararlarının dört gcov çıkışı da çalışmadı. Rust yalnız yeni mapping oluşturur; mremap hata/kuyruk temizliği ayrıca uygulanmalı. Açık kapsam. |
| `os_unix.c:5640` / branch  1 taken 0 | `if( pNew==0 ){` | pNew!=NULL olan mevcut-harita uzatma yönü alınmadı. Rust bu adımda sıfırdan map/unmap sağlar; mremap yeniden kullanım yolu açık. |
| `os_unix.c:5644` / branch  0 taken 0 (fallthrough) | `if( pNew==MAP_FAILED ){` | mmap başarısızlığı enjekte edilmedi. Rust haritayı boş bırakıp limiti0 yapar, fakat C unixLogError(SQLITE_OK) uyarı karşılığı eksiktir. Uyarı dahil tam C eşliği için açık kusur; başarı profili bunu kapatmaz. |

## Ölçülmüş yolun Rust tasarımı

C xFetch pointerını tutar; Rust gerçek read-only MAP_SHARED haritasından sahipli sayfa bytes kopyalar ve ayrı RAII lease ile haritayı tutar. Son lease bırakılana kadar limit değişmez. Drop offset/pointer parametrelerini güvenli sahiplikle temsil eder; geçersiz C pointerı/unfetch sırası bu geçerli-girdi matrisinin parçası değildir. `FetchPage::data()` immutable snapshot verir; genel C VFS raw-pointer ABI eşliği iddiası yoktur. SQL okuyucusuna bağlantı yapılmadan okuma/yazma cursor yaşamı ve dosya kilitleri eşitlenmiş sayılamaz.

**D142 açık:** Pager hâlâ xFetch çağırmadığı için 250 native mmap akışındaki altı aktif okuyucu farkı devam eder. Bu adımın başarısı D142 kapatma kanıtı değildir.
