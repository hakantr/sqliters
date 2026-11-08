# `implementation_plan.md` Değerlendirme Bulguları

- **Değerlendirilen commit**: `f114b36680aa6219be50650e2d9d447eacf60aa9` (`docs: resolve v12 review findings in plan v13`, v13)
- **Değerlendirme kapsamı**: Faz 1.1, Faz 1.2, Faz 1.3, Faz 2, Faz 3, Faz 4, Faz 5, Faz 6, Faz 7 ve Doğrulama Planı (satır `1–979` tamamı; C SQLite 3.54.1 `sqlite/src/*`, `sqlite/ext/*`, `sqlite/test/*` ve mevcut `crates/*` + `tests/*` ile karşılaştırmalı)

---

## 1. [P1] Faz 1.1 (ve Faz 5) — `sqliters-btree` ve `VdbeVm` artımlı `BtCursor` yerine tüm tabloyu RAM'e klonlayan `scan_table` ve tek-adımda tam-yürütme `Materialized` kullanıyor; Faz 1.1'in cursor save/restore/reseek, aynı-bağlantı yazma görünürlüğü ve rollback cursor-trip (`SQLITE_ABORT_ROLLBACK`) sözleşmesi mevcut depolama katmanıyla sağlanamaz

- **Plan referansı**: Satır `141`, `166`, `173`, `175`, `177`, `186` (Faz 1.1 aktif cursor sözleşmesi, aynı bağlantı yazma görünürlüğü, `ROLLBACK` cursor davranışı ve `RETURNING`), satır `589` (Faz 5 `scan_table` kaldırımı), Test Grubu `20` (`satır 755`), `GATE-1.1` (`satır 787`).
- **Sorun**:
  1. `crates/sqliters-btree/src/lib.rs:399-437, 853-942` içinde durumlu bir B-tree imleci (`BtCursor`: `(pgno, cell_idx)` sayfa yığını, `CURSOR_VALID | CURSOR_REQUIRE_SEEK | CURSOR_SKIPNEXT | CURSOR_FAULT`, `saveCursorPosition`, `restoreCursorPosition`, `sqlite3BtreeTripAllCursors`) **hiç yoktur**; yalnızca tüm ağacı tek çağrıda belleğe kopyalayan `scan_table -> Result<Vec<(i64, Vec<u8>)>>` ve `seek_index_equal -> Result<Vec<Vec<Value>>>` vardır.
  2. `crates/sqliters-vdbe/src/lib.rs:342-353, 488-500, 566-577` içinde `CursorState::Table` ve `CursorState::Index`, ilk `OP_Rewind` / `OP_SeekIndexEqual` adımında tüm tabloyu veya eşleşmeleri `Vec` içine klonlar; `OP_Next` yalnızca `*pos += 1` yapar ve `Btree`/`Pager` katmanına bir daha dokunmaz (`SELECT * FROM big LIMIT 1` dahi tüm tabloyu ve overflow zincirlerini RAM'e yükler, bu döngü içinde `InterruptHandle` kontrol edilemez). `crates/sqliters-core/src/lib.rs:357-359, 567-575` içindeki `StatementExecState::Materialized` (`WITHOUT ROWID`, `GENERATED`, join, aggregate, subquery, `RETURNING`) ise ilk `step()` çağrısında tüm sorguyu/DML'i sonuna kadar çalıştırır (`out.rows.into_iter()`).
  3. Bu mimaride Faz 1.1'in zorunlu kıldığı üç temel garanti sağlanamaz:
     - **Aynı bağlantıda aktif `SELECT` + eşzamanlı DML (`satır 173`, `lock-1.19`, Test Grubu `20`)**: Dış `SELECT` `StepResult::Row`'da dururken aynı bağlantıda tablo değiştirildiğinde (`INSERT`/`UPDATE`/`DELETE`), C SQLite `saveAllCursors` ile imlecin mevcut rowid/anahtarını kaydeder ve sonraki `OP_Next` adımında `btreeRestoreCursorPosition` ile güncel B-tree üzerinde yeniden konumlanır (`CURSOR_SKIPNEXT` dahil). Mevcut `VdbeVm` ise RAM'deki donmuş eski `Vec` kopyasını okumaya devam eder.
     - **`ROLLBACK` / `ROLLBACK TO` cursor trip sözleşmesi (`satır 141, 175`)**: C `sqlite3RollbackAll` + `sqlite3BtreeTripAllCursors`, yazma öncesi açılmış ve konumu kaydedilebilen salt-okunur imleçleri `CURSOR_REQUIRE_SEEK` ile korurken, iptal edilen işlem içinde değiştirilmiş/açılmış veya `writeCursors` bayraklı imleçleri `CURSOR_FAULT` (`SQLITE_ABORT_ROLLBACK = (ErrorCode::Abort as i32) | (2 << 8)`) durumuna geçirir. Üstelik `SQLITE_ABORT_ROLLBACK` sabiti `crates/sqliters-types/src/error.rs:120-143` içinde henüz tanımlı değildir.
     - **`RETURNING` adım-adım yürütme ve `reset` (`satır 186`)**: `Materialized` yolunda ilk `step()` tüm satırları baştan değiştirdiği için `RETURNING` üretimi yarıda kesilip `reset` çağrıldığında C VDBE satır-başına yürütme/statement-journal semantiğinden sapılır.
- **Öneri**:
  - Tam tablo klonlayan `scan_table` kaldırımını yalnız Faz 5 performans maddesi (`satır 589`) olarak bırakmayın: durumlu artımlı `BtreeCursor` (`Valid`, `RequireSeek { saved_key, skip_next }`, `Fault(extended_code)`, `Eof`), B-tree mutasyonları öncesi `save_all_cursors` ve rollback anında `trip_all_cursors` mekanizmasını **Faz 1.1** kapsamına alın; `error.rs` içine `SQLITE_ABORT_ROLLBACK` genişletilmiş kodunu ekleyin.

---

## 2. [P1] Faz 1.1 & Faz 1.2 — `WalIndexHdr.szPage` (`u16`) alanında `page_size = 65536` için özel `1` kodlaması (`wal.c:1551, 2214, 2715, 4335`) planda eksik; 64 KiB sayfalarda `-wal` (`u32 = 65536`) ile `-shm` (`u16 = 1`) asimetrisi C SQLite 3.54.1 birlikte çalışabilirliğini kırar

- **Plan referansı**: Satır `100` (Faz 1.1/1.2 `WalIndexHdr` `szPage@14` ikili düzeni), satır `320` (Faz 1.2 32 baytlık WAL başlığı), Test Grubu `2` ve `4` (`satır 737, 739` C `sqlite3` ikili birlikte çalışabilirlik).
- **Sorun**:
  1. `crates/sqliters-pager/src/header.rs:81-85, 102-107` `SQLITE_MAX_PAGE_SIZE = 65536` değerini destekler (`.db` dosya başlığında big-endian `u16` `0x0001` -> `65536`).
  2. Ancak C SQLite WAL protokolünde (`sqlite/src/wal.c`) `-wal` başlığı ile `-shm` başlığı arasında kritik bir kodlama asimetrisi vardır:
     - **32 baytlık `-wal` dosya başlığı (`aWalHdr[8..12]`, big-endian `u32`, `wal.c:1478-1482, 4180`)**: `szPage` alanı tam 32-bit `65536` (`0x00010000`) olarak yazılır; buraya `1` yazılırsa C SQLite `szPage < 512` denetimiyle WAL dosyasını reddeder/yok sayar.
     - **`-shm` içindeki `WalIndexHdr.szPage` (`offset 14..16` ve kopyası `62..64`, native-endian `u16`, `wal.c:327, 1551, 4335`)**: `65536` değeri `u16` alanına sığmadığından `szPage as u16` (`0`) yapılmaz; `(u16)((szPage & 0xff00) | (szPage >> 16))` formülüyle **`1`** olarak yazılır ve `walPagesize` / `walTryBeginRead` (`wal.c:2214, 2715`) içinde `((hdr.szPage & 0xfe00) + ((hdr.szPage & 0x0001) << 16))` ile geri `65536`'ya çözülür. Üstelik `WalIndexHdr.aCksum` sağlama toplamı bu `1` değeri üzerinden hesaplanır.
  3. Plan satır `100` `szPage@14` (`u16`) ve satır `320` 32 baytlık WAL başlığını tanımlarken bu `65536 <-> 1` kodlama kuralını belirtmiyor; saf `u16` dönüşümü 64 KiB sayfa boyutlu veritabanlarında C `sqlite3` ile `-shm`/`-wal` birlikte çalışabilirliğini ve sağlama toplamını bozar.
- **Öneri**:
  - Faz 1.1/1.2 ikili protokol tablosuna `page_size == 65536` kuralını açıkça ekleyin: `.db` başlığında big-endian `u16 = 1`, `-wal` başlığında big-endian `u32 = 65536`, `-shm` `WalIndexHdr.szPage` alanında native-endian `u16 = 1` kodlaması ve Test Grubu `4` C birlikte çalışabilirlik matrisinde `PRAGMA page_size = 65536; PRAGMA journal_mode = WAL;` senaryosu doğrulanmalıdır.

---

## 3. [P1] Faz 1.1 & Faz 1.3 — `Btree::integrity_check` (`crates/sqliters-btree/src/lib.rs:1289-1442`) hem derleme zamanı `PENDING_BYTE_OFFSET` (`1 GiB`) sabitini kullanıyor hem de `auto_vacuum` pointer-map sayfalarını (`PTRMAP_ISPAGE` / `checkPtrmap`) tanımıyor; Faz 1.1'de aktif `index4.rs`, Faz 1.3'te `vacuum2.rs` ve `autovacuum*` kırılır

- **Plan referansı**: Satır `296–300` (Faz 1.1 `pending_byte = 0x0010000` test profili ve çalışma zamanı pending-byte geometrisi), satır `66, 445–472, 771, 795` (Faz 1.3 `ptrmap`, `autovacuum*`, `vacuum2`).
- **Sorun**:
  1. **Faz 1.1 regresyonu (`index4.rs`)**:
     - Plan satır `296–300`, `Pager` ve `VfsFile` için çalışma zamanı `pending_byte_offset()` okumasını ve Tcl test profilinde `0x0010000` (`64 KiB`) kurulumunu tanımlıyor, ancak `crates/sqliters-btree/src/lib.rs:1431` içindeki `Btree::integrity_check` fonksiyonunu kapsamıyor.
     - `Btree::integrity_check` satır `1431` hâlâ derleme zamanı `sqliters_types::PENDING_BYTE_OFFSET` (`0x4000_0000` = `1 GiB`) sabitini okuyor (`let lock_byte_pgno = ((sqliters_types::PENDING_BYTE_OFFSET / ps) + 1) as Pgno;`); ayrıca ağaç/freelist yürüyüşü (`satır 1373-1427`) bir sayfanın `lock_byte_pgno`'ya referans vermesini de hata olarak yakalamıyor.
     - Tcl harness bootstrap'ta `pending_byte = 0x0010000` aktif edildiğinde (`1024 B` sayfa boyutunda `lock_byte_pgno = 65`), `Pager::allocate_raw_page` sayfa `65`'i atlayacak; fakat şu anda **aktif** çalışan ve `65.536` satır (>2.000 sayfa) ekleyip `PRAGMA integrity_check` çağıran `tests/sqlite_test/index4.rs:33, 46, 70`, `integrity_check` `1_048_577`'ye baktığı için `"Page 65 is never used"` üreterek **`GATE-1.1`'de hemen kırılacaktır**.
  2. **Faz 1.3 eksikliği (`PTRMAP_ISPAGE` ve `checkPtrmap`)**:
     - `auto_vacuum = FULL` veya `INCREMENTAL` veritabanlarında Sayfa `2` ve tüm `ptrmapPageno(pgno) == pgno` sayfaları ne B-tree köklerinde ne overflow zincirlerinde ne de freelist içinde yer alır (`seen.test(pgno) == false`).
     - Faz 1.3 (`satır 445–472`) `Btree::integrity_check` güncellemesini içermediği için herhangi bir `auto_vacuum` veritabanında `PRAGMA integrity_check` `"Page 2 is never used"` döndürerek `tests/sqlite_test/vacuum2.rs:67, 70, 116, 136` ve `autovacuum*` paketlerini kırar.
- **Öneri**:
  - **Faz 1.1'de**: `Btree::integrity_check` içinde `lock_byte_pgno` hesabını `self.pager.lock_byte_page()` çalışma zamanı değerine bağlayın ve B-tree/overflow/freelist içinden `lock_byte_pgno`'ya yapılan referansları bozulma hatası olarak raporlayın.
  - **Faz 1.3'te**: C `sqlite/src/btree.c:10742-11336` (`checkPtrmap` + `PTRMAP_PAGENO`) ile uyumlu olarak `ptrmap` sayfalarını `"never used"` denetiminden muaf tutun, ağaç/freelist içinden `ptrmap` sayfasına referans verilmesini `"pointer map referenced"` olarak yakalayın ve her kök/iç/yaprak/overflow/freelist sayfasının 5 baytlık `(PtrmapType, parent_pgno)` geri işaretçisini doğrulayın.

---

## 4. [P1] Faz 2, Faz 3 & Faz 4 — Çoklu veritabanında `temp → main → attached` kuralının `VIEW`, `TRIGGER` ve `FOREIGN KEY` içine genellenmesi C `sqlite3FixInit` / `fkey.c` şema izolasyonunu bozar; ayrıca `PRAGMA <schema>.<ad>` yönlendirmesi mevcut `engine.rs` içinde yok sayılıyor

- **Plan referansı**: Satır `481`, `493` (Faz 2 çoklu DB şema ve `temp → main → attached` isim çözümleme sırası), satır `550` (Faz 3 `VIEW`), satır `566–578` (Faz 4 `TRIGGER`).
- **Sorun**:
  1. `temp → main → attached` arama sırası yalnızca **üst-düzey SQL ifadeleri ve `TEMP` (`iDb == 1`) şemasındaki nesneler** için geçerlidir. C SQLite `sqlite/src/attach.c:497-635` (`sqlite3FixInit`, `fixSelectCb`, `sqlite3FixTriggerStep`), `sqlite/src/trigger.c:167-180, 304-316, 352-357` ve `sqlite/src/fkey.c:939` aşağıdaki katı şema izolasyonu kurallarını uygular:
     - **Kalıcı `VIEW` (`main` veya `attached`, `iDb != 1`)**: Görünüm sorgusu içindeki niteliksiz tablo/görünüm adları `temp → main → attached` sırasıyla **aranmaz**, yalnızca görünümün kendi `iDb` şemasına sabitlenir (`CREATE TEMP TABLE t1` sonradan oluşturulsa bile `main.v1` görünümünü gölgeleyemez); başka bir şemaya açık referans (`aux.t1` veya `temp.t1`) ise `"view ... cannot reference objects in database ..."` hatasıyla reddedilir.
     - **`TRIGGER` şema kuralları**: (i) `TEMP` trigger'lar dahil **hiçbir** trigger gövdesindeki `INSERT`/`UPDATE`/`DELETE` hedef tablosu `schema.tablo` biçiminde nitelendirilemez (`"qualified table names are not allowed on INSERT, UPDATE, and DELETE statements within triggers"`); (ii) `iDb != 1` içindeki trigger'ın `ON` hedefi ve gövdesindeki tüm ifadeler yalnızca kendi `iDb` şemasına bağlanır; (iii) niteliksiz `CREATE TRIGGER tr ... ON temp_tablo` otomatik olarak `sqlite_temp_schema` (`iDb = 1`) içine yazılır (`trigger.c:168-171`) ve hedef tablosu başka bağlantıca düşürülmüş yetim `TEMP` trigger şema yüklemesinde `orphanTrigger` (`trigger.c:304-316`) olarak atlanıp şemayı bozuk saydırmaz; (iv) sanal tablolar ve `sqlite_*` sistem tabloları üzerinde trigger oluşturulamaz (`trigger.c:185-192, 224-227`).
     - **`FOREIGN KEY` ebeveyn çözümlemesi ve `deferred_fk_suspects`**: `REFERENCES parent` her zaman çocuk tabloyla **aynı `iDb`** içinde çözülür (`fkey.c:939`); `TEMP` tablosu ebeveyn adını gölgeleyemez. Ayrıca `crates/sqliters-core/src/lib.rs:117` içindeki `deferred_fk_suspects: Vec<(String, TableRowKey, usize)>` çoklu DB'de `DatabaseId` taşımadığı için `main.t` ile `temp.t`/`aux.t` ertelenmiş FK ihlallerini birbirine karıştırır.
  2. **`PRAGMA <schema>.<name>` yönlendirmesi yok sayılıyor**: Mevcut `crates/sqliters-core/src/engine.rs:378-382`, `Stmt::Pragma { schema: _, name, value }` eşleşmesinde `schema` alanını tamamen çöpe atıyor; bu düzeltilmezse Faz 2/6'da `PRAGMA aux.page_count`, `PRAGMA aux.freelist_count`, `PRAGMA aux.integrity_check`, `PRAGMA aux.journal_mode`, `PRAGMA temp.page_size` gibi şemaya özgü tüm pragmalar yanlışlıkla `main` üzerinde çalışır (`pragma2.rs`, `pragma4.rs`, `vacuum5.rs`).
- **Öneri**:
  - Faz 2'de `PRAGMA <schema>.<name>` yönlendirmesini ve `deferred_fk_suspects` / FK ebeveyn çözümlemesinin `DatabaseId` izolasyonunu ekleyin; Faz 3 ve Faz 4'te C `sqlite3FixInit` eşdeğeri şema-sabitleme (`SchemaFixer`) kurallarını (`iDb != 1` için kendi şemasına bağlama, gövde DML nitelikli ad yasağı, `ON temp_tbl` -> `temp` yönlendirmesi ve `orphanTrigger` toleransı) açıkça tanımlayın.

---

## 5. [P1] Faz 1.3, Faz 2, Faz 3, Faz 4 & Faz 7 — `VACUUM` ayrıştırıcısı hedef şema adını çöpe atıyor (`parser.rs:312`), `VACUUM INTO` desteklemiyor ve `exec_vacuum` (`engine.rs:8858-8907`) tüm `VIEW`, `TRIGGER` ve `VIRTUAL TABLE` (`rootpage = 0`) kayıtlarını yok ediyor veya bozuyor

- **Plan referansı**: Satır `66, 450` (Faz 1.3 `exec_vacuum`), satır `478–496` (Faz 2 çoklu DB), satır `550` (Faz 3 `VIEW`), satır `566` (Faz 4 `TRIGGER`), satır `706–715` (Faz 7 `VirtualTable`).
- **Sorun**:
  1. **Sözdizimi kaybı (`vacuum5.rs`, `vacuum-into.rs`)**:
     - `crates/sqliters-syntax/src/ast.rs:46` (`Stmt::Vacuum`) hiçbir alan taşımıyor ve `crates/sqliters-syntax/src/parser.rs:309-315` isteğe bağlı şema kimliğini okuyup **sessizce atıyor** (`let _ = self.parse_ident()?; Ok(Stmt::Vacuum)`), `INTO <expr>` yan tümcesini ise hiç ayrıştırmıyor.
     - Bu durumda Faz 2'de `VACUUM aux` yanlışlıkla `main` veritabanını vakumlar, `VACUUM temp` no-op olmak yerine (`sqlite/src/vacuum.c:126`) `main`'i vakumlar ve var olmayan şema için `VACUUM olaf` (`tests/sqlite_test/vacuum5.rs` `vacuum5-2.0`) `"unknown database olaf"` hatası vermek yerine başarılı döner.
  2. **Katalog nesnelerinin silinmesi ve sanal tablo bozulması (`engine.rs:8858-8907` vs `sqlite/src/vacuum.c:298-340`)**:
     - Mevcut `exec_vacuum`, hedef B-tree'yi yalnızca `ctx.schema.tables` (`create_table` + `scan_table`) ve `ctx.schema.indexes` üzerinden yeniden inşa ediyor:
       a) Faz 3'te eklenen tüm **`VIEW`** (`type='view'`) ve Faz 4'te eklenen tüm **`TRIGGER`** (`type='trigger'`) tanımları `VACUUM` çalıştırıldığı anda `sqlite_schema` tablosundan **tamamen kaybolur** (`tests/sqlite_test/vacuum.rs:64-65, 201-218` kırılır).
       b) Faz 7'de sanal tablolar `sqlite_schema` içinde `type='table'` ve **`rootpage = 0`** olarak saklanır (`vacuum.c:301, 334-338`); `exec_vacuum` bunlara `create_table()` / `scan_table(0)` uygularsa ya kök sayfa `0` hatası alır ya da sanal tabloyu sıradan B-tree tablosuna çevirip bozar (C `vacuum.c:334-338` `type IN ('view','trigger') OR (type='table' AND rootpage=0)` satırlarını `sqlite_schema`'ya doğrudan kopyalar, gölge tabloları ise normal tablo olarak taşır).
       c) Açık `INTEGER PRIMARY KEY` kolonu olmayan rowid tablolarında C `vacuum.c:318-322` örtük rowid değerlerini `1..N` olarak sıkıştırarak yeniden numaralandırırken (ve `VACUUM` sırasında kullanıcı tanımlı `substr`/`like`/`quote` UDF'lerinin iç SQL'i bozmaması için yerleşik fonksiyon önceliği kullanırken — bkz. `tests/sqlite_test/vacuum.rs:42-53`), mevcut `engine.rs:8885-8886` eski seyrek `rowid` değerlerini olduğu gibi korur.
- **Öneri**:
  - `Stmt::Vacuum { schema: Option<String>, into: Option<Expr> }` AST/parser desteğini ekleyin; `exec_vacuum` mantığını Faz 2 (`schema` seçimi, `temp` no-op, bilinmeyen DB hatası ve `INTO`), Faz 3/4 (`view` ve `trigger` katalog satırlarının korunması + örtük rowid yeniden numaralandırması) ve Faz 7 (`rootpage == 0` sanal tablo katalog kayıtlarının korunması) ile uyumlu hale getirin.

---

## 6. [P2] Faz 5, Faz 6 & Faz 7 — `compiler.rs` / `VdbeVm` ikincil indeks ve `IndexRangeScan` / `covering-index` modeli `WITHOUT ROWID` bileşik PK kuyruğunu, parçalı (`WHERE`) ve ifade (`XN_EXPR`) indekslerini dışlıyor; ayrıca `ExecutionServices` içinde pencere kümeleme (`xValue` / `xInverse`) kancası eksik (`json_group_*` / `jsonb_group_*`)

- **Plan referansı**: Satır `529–533` (`ExecutionServices` fonksiyon çağrı yüzeyi), satır `588–590` (Faz 5 `WINDOW` `inverse`, `IndexRangeScan`, `covering-index`, `EQP`), satır `722` (Faz 7 `json_*` / `jsonb_*`), Test Grubu `18` (`satır 753`).
- **Sorun**:
  1. **İndeks ve `WITHOUT ROWID` VDBE/planlayıcı uyumsuzluğu**:
     - Mevcut `crates/sqliters-core/src/compiler.rs:558` tüm `WITHOUT ROWID` ve `GENERATED` sütunlu tabloları VDBE derleyicisinden dışlayıp yorumlayıcıya bırakıyor (`Ok(None)`); `compiler.rs:94, 102` tüm parçalı indeksleri (`idx.where_clause.is_some()`) ve ifade indekslerini (`ic.col_idx.is_none()`) yok sayıyor; `compiler.rs:828-831` ve `crates/sqliters-vdbe/src/lib.rs:612-616` ise her ikincil indeks kaydının son sütununu tekil `i64` `rowid` varsayıp `Opcode::Rowid -> Opcode::SeekRowid` üretiyor.
     - Oysa `WITHOUT ROWID` ikincil indekslerinde (`crates/sqliters-core/src/schema.rs:232-279`) indeks kaydının kuyruğu tek bir `rowid` değil `trailing_pk_cols` (çok sütunlu birincil anahtar demeti) taşır ve ana tablo bir Table B+Tree değil Index B-Tree'dir. Faz 5'te hedeflenen `index6.rs` (parçalı indeks kullanılabilirlik ispatı + covering scan + `EQP`), `index7.rs` (`WITHOUT ROWID` covering/range scan + `EQP`) ve `in4.rs` bu ikili indeks/tablo modeli birleştirilmeden geçemez.
  2. **Pencere kümeleme (`xValue` / `xInverse`) trait boşluğu**:
     - Faz 5 satır `588` *"`inverse` destekli kayan çerçeve optimizasyonu"* ve Faz 7 satır `722` *"tüm `json_*` / `jsonb_*` fonksiyonları"* hedefliyor. C SQLite `sqlite/src/json.c:5122, 5877-5884` içinde `json_group_array`, `json_group_object`, `jsonb_group_array` ve `jsonb_group_object` `WAGGREGATE` (`xStep`, `xFinal`, `xValue`, `xInverse`) olarak kayıtlıdır ve `OVER (...)` pencere çerçevelerinde çalışır.
     - Ancak `sqliters-types/src/runtime_api.rs` içindeki `ExecutionServices` ve Faz 6 UDF kayıt yüzeyi (`satır 529, 533, 613`) yalnızca skaler ve klasik `step`/`finalize` kümeleme çağrılarından söz ediyor; `xValue` (durumu tüketmeden ara değer okuma) ve `xInverse` (kayan çerçeveden çıkan satırı geri alma) kancaları olmadan ne `sqliters-ext` içindeki `json*_group_*` ne de Faz 6 kullanıcı tanımlı pencere kümelemeleri `window.rs` ile entegre olabilir.
- **Öneri**:
  - Faz 5'te `IndexRangeScan` ve `covering-index` tasarımına `WITHOUT ROWID` birincil/ikincil Index B-Tree aramasını (çok sütunlu PK anahtar çıkarma), parçalı indeks `WHERE` kapsama eşleştirmesini ve ifade indeksi AST eşleştirmesini dahil edin; `ExecutionServices` / `AggregateFunction` trait sözleşmesine opsiyonel `value(&self)` (`xValue`) ve `inverse(&mut self, args)` (`xInverse`) metotlarını ekleyin.

---

## 7. [P2] Faz 6 (ve Faz 1.1 / Faz 4) — `commit_hook` → `SQLITE_CONSTRAINT_COMMITHOOK` + `rollback_hook` geçişi, `set_authorizer` `SQLITE_IGNORE` sütun maskelemesi / statement-expire kuralı ve `DELETE` truncate (`OP_Clear`) optimizasyonunun trigger/authorizer/`update_hook` etkileşimi planda eksik

- **Plan referansı**: Satır `141` (`error.rs` genişletilmiş hata kodları), satır `204–206` (hook gözlemi), satır `539` (`set_authorizer` ve hook merkezi noktası), satır `614` (Faz 6 hook/authorizer), Test Grubu `23` (`satır 758`).
- **Sorun**:
  1. **`commit_hook` geri dönüş semantiği (`sqlite/src/vdbeaux.c:2997-3002, 3436-3445`, `main.c:1556`)**:
     - Açık `COMMIT`, en dış transaction `RELEASE` veya autocommit DML sonlandırmasında `commit_hook` sıfır-dışı dönerse C SQLite faz-1/WAL commit'e girmeden işlemi tam `ROLLBACK`'e çevirir, `SQLITE_CONSTRAINT_COMMITHOOK` (`(ErrorCode::Constraint as i32) | (2 << 8)`) döndürür, `autocommit = true` yapar ve hemen ardından `rollback_hook`'u tetikler. Mevcut `crates/sqliters-types/src/error.rs:120-143` içinde `SQLITE_CONSTRAINT_COMMITHOOK` (ayrıca Faz 4 `RAISE(ABORT, ...)` için `SQLITE_CONSTRAINT_TRIGGER` ve Faz 7 için `SQLITE_CONSTRAINT_VTAB`) tanımlı değildir.
  2. **`set_authorizer` `SQLITE_IGNORE` ve hazırlanmış ifade geçersizleştirme (`sqlite/src/auth.c:81, 132-185, 257`)**:
     - `set_authorizer` yalnızca izin/ret (`SQLITE_OK` / `SQLITE_DENY` -> `SQLITE_AUTH`) kapısı değildir:
       a) `set_authorizer` kayıt/değişim çağrısı mevcut tüm hazırlanmış `Statement`'ları `expired` olarak işaretlemelidir (`sqlite3ExpirePreparedStatements(db, 1)`), böylece sonraki `step()` yeni authorizer altında yeniden derlenir.
       b) `SQLITE_READ` eyleminde geri çağrı `SQLITE_IGNORE` döndürürse sorgu reddedilmez; ilgili sütun okuması derleme/planlama aşamasında `NULL` sabitine dönüştürülür (`auth.c:182-184`), `SQLITE_UPDATE` için `SQLITE_IGNORE` o sütunun güncellenmesini atlar ve 6. argüman (`zAuthContext`) aktif en içteki `VIEW` veya `TRIGGER` adını yığınla taşır (`sqlite3AuthContextPush`).
  3. **`DELETE FROM tbl` truncate (`OP_Clear`) optimizasyonunun trigger, authorizer ve `update_hook` ile etkileşimi (`engine.rs:7234-7251` vs `sqlite/src/delete.c:466-493`, `vdbe.c:5941, 6144`)**:
     - Mevcut `crates/sqliters-core/src/engine.rs:7234`, `WHERE` ve `RETURNING` yoksa ve `foreign_keys == false` ise doğrudan `clear_tree` (`OP_Clear`) çalıştırıyor.
     - Oysa C SQLite'ta (`delete.c:466-493`, R-17228-37124): (a) tabloda herhangi bir `DELETE` trigger'ı varsa (Faz 4) veya `set_authorizer(SQLITE_DELETE)` `SQLITE_IGNORE` döndürürse (Faz 6) `OP_Clear` devre dışı bırakılıp satır-satır silme yapılmalıdır; (b) buna karşılık `OP_Clear` meşru olarak çalıştığında ve `WITHOUT ROWID` tablolarındaki tüm DML işlemlerinde (`HasRowid(pTab)`, `vdbe.c:5941, 6144`) `update_hook` **çağrılmaz** — bu ayrım hem VDBE hem yorumlayıcı yolunda korunmalıdır.
- **Öneri**:
  - `error.rs` içine `SQLITE_CONSTRAINT_COMMITHOOK`, `SQLITE_CONSTRAINT_TRIGGER` ve `SQLITE_CONSTRAINT_VTAB` alt kodlarını ekleyin; Faz 6 hook/authorizer sözleşmesinde `commit_hook` sıfır-dışı -> otomatik `ROLLBACK` + `rollback_hook`, `set_authorizer` değişiminde statement-expire + `SQLITE_READ`/`SQLITE_UPDATE` `SQLITE_IGNORE` maskelemesi + `zAuthContext` yığını ve `DELETE` truncate (`OP_Clear`) devre dışı bırakma/`update_hook` kurallarını açıkça belirtin.
