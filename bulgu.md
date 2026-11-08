# `implementation_plan.md` Değerlendirme Bulguları

- **Değerlendirilen commit**: `df2f735002651bb648930c9507815ec11f5ea6db` (`docs: align v11 plan with VFS and test prerequisites`, v11)
- **Değerlendirme kapsamı**: Faz 1.1, Faz 1.2, Faz 1.3, Faz 2, Çapraz-faz yürütme hizmetleri, Faz 3, Faz 4, Faz 5, Faz 6, Faz 7 ve Doğrulama Planı (satır `1–879` tamamı; C SQLite 3.54.1 `sqlite/src/*`, `sqlite/test/*` ve mevcut `crates/*` ile karşılaştırmalı)

---

## 1. [P1] Faz 1.1 & Faz 6 — `harness.rs` içindeki `db_eval` (`$db eval $sql ?array? $body`) hâlâ tüm satırları önden materyalize edip `Statement`'ı düşürüyor; aktif okuyucu kilidini sınayan `lock.rs` (`lock-1.16`..`lock-1.22`) ve C `tclsqlite.c` semantiğiyle çelişiyor

- **Plan referansı**: Satır `209–219` (*"Faz 1.1 — Gerçek sahipli Statement ile Tcl test harness geçişi"*), satır `255` ve `560–563` (*"Tam `lock.rs`"*), Test Grubu `29` (satır `699`).
- **Sorun**:
  1. Plan satır `209–218` yalnızca düşük seviyeli `sqlite3_prepare` / `sqlite3_step` / `sqlite3_reset` / `sqlite3_finalize` Tcl komut vekillerini (`PreparedStmtEntry`) gerçek `Statement` nesnesine taşıyor; `crates/sqliters-tcltest/src/harness.rs:670-741` içindeki `db_eval` kapsamda yer almıyor.
  2. Mevcut `db_eval`, bir `$body` betiği verildiğinde bile satır `684`'te `let rows = exec_sql_rows(host, interp, db_name, sql)?;` çağırarak dış `SELECT` sorgusunu baştan sona tüketiyor, tüm satırları `Vec` içine kopyalıyor ve `$body` çalışmaya **başlamadan önce** `Statement`'ı düşürüyor (`ActivityTracker` kaydı ve `SHARED` kilidi bırakılıyor).
  3. Oysa C SQLite `tclsqlite.c` (`DbEvalStep`) dış ifadeyi `$body` çalışırken `SQLITE_ROW` durumunda canlı tutar. Faz 6'da açılması bağlayıcı kılınan `tests/sqlite_test/lock.rs` (`sqlite/test/lock.test:91-156` içindeki `lock-1.16`, `lock-1.17`, `lock-1.19`, `lock-1.21`, `lock-1.22`) tam olarak `db eval {SELECT * FROM t1} qv { ... db eval {UPDATE t2 ...} / db2 eval {UPDATE t2 ...} }` kalıbıyla dış `SELECT` aktifken:
     - Aynı bağlantıda iç yazmanın bitişinde `Write → Read` düşürmesini (`SHARED` kilidin korunmasını),
     - İkinci bağlantının yazmasının `SQLITE_BUSY` ("database is locked") almasını,
     - Windows ve Unix çoklu bağlantı okuyucu/yazıcı kilit etkileşimini (`lock-1.22`)
     doğrular. `db_eval` önden materyalize etmeye devam ederse dış okuyucu kilidi `$body` öncesinde düştüğünden bu alt testler ya yanlış sonuç üretir (ör. `lock-1.22` ve `db2` yazma engeli) ya da kilit ömrünü hiç sınamadan sahte başarı verir; bu da Test Grubu `29`'daki *"Satır önbelleğiyle taklit yasaktır"* kuralıyla doğrudan çelişir.
- **Öneri**:
  - Faz 1.1 harness geçişine `db_eval`'in (`$body` verildiği durumda) çok-deyim kuyruğunu gerçek adım adım `Statement::step()` döngüsüyle yürütecek şekilde güncellenmesini ekleyin: dış `Statement` `StepResult::Row` durumunda sahipli olarak (host `RefCell` borcu tutulmadan) canlı kalmalı, `$body` içindeki alt sorgular aynı bağlantı üzerinde ikinci sahipli `Statement` olarak çalışabilmeli, `break` / hata / normal bitişte dış `Statement` deterministik olarak `reset`/`finalize` edilmelidir.

---

## 2. [P1] Faz 1.1 — `Pager::open_with_vfs` kilitsiz Page 1 okuması (`load_initial_header`) ve `Connection::from_btree` erken `Schema::load` davranışı C `sqlite3_open`, Windows zorunlu kilitleri ve şemasız açılış/hazırlama sözleşmesiyle çelişiyor

- **Plan referansı**: Satır `231` (`PRAGMA lock_status` ve `EXCLUSIVE` kilit altında şemasız `prepare`/`step`), satır `268–270` (`ConnectionOpenOptions` ve `lock4`), Test Grubu `33` (satır `703`).
- **Sorun**:
  1. Mevcut `Pager::open_with_vfs` (`crates/sqliters-pager/src/lib.rs:156-218`), `open()` sırasında **hiçbir `.db` kilidi almadan** `load_initial_header()` (`self.file.read_at(&mut hdr_bytes, 0)`) çalıştırıyor ve `page_size_fixed = true` yapıyor.
     - Eşzamanlı bir yazıcı commit veya WAL backfill ortasındayken (ör. Page 1'de `database_size_pages` büyütülmüş ama yeni sayfalar henüz yazılmamışken ve `-journal` penceresi TOCTOU ile değişmişken) kilitsiz okuma satır `205-208`'de sahte `ErrorCode::Corrupt` ("database header page count exceeds file size") veya yırtık (torn) 100 bayt başlık nedeniyle sahte `NotADb`/`Corrupt` döndürür.
     - Windows yerel dosya sisteminde başka bir süreç/tanıtıcı zorunlu (mandatory) bayt aralığı kilidi tutuyorsa kilitsiz `ReadFile(0..100)` doğrudan G/Ç kilit hatasıyla başarısız olur; `sqlite3 db2 test.db` açılışı (`lock4.test:59, 73`) daha `db2 timeout` veya `PRAGMA lock_status` çağrılamadan çöker.
  2. Hemen ardından `Connection::from_btree` (`crates/sqliters-core/src/lib.rs:152-160`), yine `Connection::open()` içinde erken `Schema::load(&mut btree)` çağırarak `SHARED` kilit alıyor ve **hot-journal kurtarmasını (`SHARED → EXCLUSIVE` yükseltmesi ve `.db` yazımı) daha `open()` anında tetikliyor**.
     - C SQLite `openDatabase` / `sqlite3PagerOpen` açılışta `.db` dosyasından **hiç okuma yapmaz, kilit almaz ve kurtarma başlatmaz**; başlık okuma, `default_page_size` karşısında mevcut dosya başlığını benimseme ve hot-journal/WAL kurtarması ilk `sqlite3PagerSharedLock` (`begin_read`) adımına ertelenir.
     - Bağlantı açılır açılmaz `busy_timeout` / `PRAGMA query_only` / `PRAGMA lock_status` ayarlaması yapan çağıranlarda (`lock4.test:73-74` dahil) `open()` içinde `SHARED` kilit ve kurtarma çalıştırmak C davranışını bozar; ayrıca `open()` anında boş (`0` bayt) olan veritabanında başka bağlantı tablo oluşturduktan sonra ilk şemalı sorgunun başlığı ve şemayı `SHARED` kilit altında taze okuması gerekir.
- **Öneri**:
  - Faz 1.1'de `Pager::open_with_vfs` içindeki kilitsiz `load_initial_header()` ve `Connection::from_btree` içindeki erken `Schema::load` kaldırılmalıdır (`schema_loaded = false`, `Pager` durumu `Open` / `NoLock`).
  - Diskten ilk `DatabaseHeader` okuması ve mevcut dosya `page_size` değerinin `ConnectionOpenOptions::default_page_size` değerini ezmesi yalnız `SHARED` kilit (veya WAL read-lock) alındıktan sonra `begin_read` / `begin_write` içinde yapılmalıdır.

---

## 3. [P1] Faz 1.3 — `auto_vacuum` (`ptrmap`) dosya kapsamı, `Pager` yazma engeli, kök sayfa yerleştirmesi (`btreeCreateTable`), `sqlite_schema.rootpage` yeniden numaralandırması ve `VACUUM` entegrasyonu eksik

- **Plan referansı**: Satır `403–432` (*"Faz 1.3 — Pointer-map (`ptrmap`) ve `auto_vacuum` (`NONE` / `FULL` / `INCREMENTAL`)"*), `GATE-1.3` (satır `715–717`).
- **Sorun**:
  1. Faz 1.3 dosya listesi (satır `407–409`) yalnızca `[NEW] crates/sqliters-btree/src/ptrmap.rs` ve `[MODIFY] crates/sqliters-btree/src/lib.rs` dosyalarını içeriyor; `crates/sqliters-pager/src/lib.rs` ve `crates/sqliters-core/src/{engine.rs, schema.rs}` kapsamda yok.
  2. `crates/sqliters-pager/src/lib.rs:412` şu anda `self.header.largest_root_btree_page != 0` olan tüm veritabanlarında yazma işlemini `"writing to auto_vacuum (pointer-map) databases is not supported"` hatasıyla reddediyor. `sqliters-pager` değiştirilmeden hiçbir `auto_vacuum` veritabanına yazılamaz.
  3. C SQLite `btreeCreateTable` (`sqlite/src/btree.c:10103-10214`) ve `btreeDropTable` + `OP_Destroy` (`sqlite/src/btree.c:10400-10445`, `sqlite/src/vdbe.c:7114-7125`) protokolünün şema katmanıyla zorunlu sözleşmesi planda tanımlanmamıştır:
     - `auto_vacuum` etkin bir veritabanında Page 2 ilk `ptrmap` sayfasıdır; yeni kök sayfalar `largest_root_btree_page` bölgesinin hemen sonuna (`>= 3`, `ptrmap` ve `PENDING_BYTE_PAGE` atlanarak) yerleştirilir ve o yuvada duran mevcut veri/iç sayfa `relocate_page` ile taşınır (`PTRMAP_ROOTPAGE`).
     - Bir tablo veya indeks silindiğinde (`DROP TABLE` / `DROP INDEX`), silinen kök sayfa `largest_root_btree_page`'den küçükse, `largest_root_btree_page` konumundaki kök sayfa boşalan yuvaya taşınır (`*piMoved`), `largest_root_btree_page` azaltılır ve **taşınan tablo/indeksin hem disk üzerindeki `sqlite_schema.rootpage` sütunu hem de bellekteki `Schema` katalog kök sayfa numarası (`sqlite3RootPageMoved`) güncellenir**. Bu güncelleme olmadan `DROP TABLE` sonrası diğer tablo/indekslerin kök sayfaları bozulur.
  4. `crates/sqliters-core/src/engine.rs` içinde yeni veritabanında ilk tablo oluşturulmadan önce `PRAGMA auto_vacuum = NONE|FULL|INCREMENTAL` ayarının kalıcılaştırılması, `PRAGMA incremental_vacuum(N)` yürütmesi ve `VACUUM` (`exec_vacuum`, `engine.rs:8846-8856` şu an yeni bellekteki veritabanında `largest_root_btree_page` ve `incremental_vacuum_mode` başlık alanlarını sıfırlayarak auto-vacuum kipini kaybediyor) açıkça kapsama alınmalıdır.
- **Öneri**:
  - Faz 1.3 `[MODIFY]` kapsamına `crates/sqliters-pager/src/lib.rs`, `crates/sqliters-core/src/engine.rs` ve `crates/sqliters-core/src/schema.rs` dosyalarını ekleyin; `Pager` yazma engelinin kaldırılmasını, `create_table`/`drop_table` kök sayfa taşıma (`moved_root -> sqlite_schema.rootpage` güncellemesi) sözleşmesini ve `VACUUM` başlık/kip korumasını bağlayıcı olarak tanımlayın.

---

## 4. [P1] Faz 2 — Çoklu veritabanı mimarisinde hem `Pager`/`Btree`/`Vfs` iki fazlı super-journal (`-mj`) protokolü ve çökme kurtarma kuralı, hem de `VdbeVm`/`compiler.rs` çoklu-Btree sınırı kapsam dışı bırakılmış

- **Plan referansı**: Satır `434–465` (*"Faz 2 — Çoklu veritabanı mimarisi (`main`, `temp`, `ATTACH` / `DETACH`)"*).
- **Sorun**:
  1. **Eksik crate kapsamı ve eksik super-journal kurtarma kuralı (`pager.c` / `vdbeaux.c`)**:
     - Faz 2 dosya listesi (satır `438–445`) yalnızca `sqliters-syntax` ve `sqliters-core/{db_set.rs, lib.rs, engine.rs}` içeriyor; `crates/sqliters-pager/src/lib.rs`, `crates/sqliters-btree/src/lib.rs` ve `crates/sqliters-vfs/src/lib.rs` (`OpenFlags::SUPER_JOURNAL = 0x0000_4000`) listede yok. Oysa `-journal` ikili biçimi, super-journal kuyruğu (`writeSuperJournal`: `[ad, uzunluk_be32, cksum_be32, JOURNAL_MAGIC]`), iki fazlı commit (`commit_phase_one` / `commit_phase_two`) ve hot-journal kurtarma tamamen `Pager` katmanındadır.
     - Plan satır `459–460` yalnızca *"Çökme sonrası kurtarmada master-journal mevcutsa tüm katılımcılar geri sarılır"* diyor; C SQLite `sqlite/src/pager.c:2894-2905` (`readSuperJournal`) ve `2583-2690` (`pager_delsuper`) içindeki **tamamlayıcı atomiklik kuralını** atlıyor:
       - İki fazlı committe atomik karar noktası `<main>-mjXXXXXXXX` dosyasının silinmesidir (`vdbeaux.c:3175`). Süreç `<main>-mjXXXXXXXX` silindikten **sonra**, katılımcı `-journal` dosyalarından biri silinmeden **önce** çökerse işlem **commit edilmiştir**. Yeniden açılışta katılımcı `-journal` içinde super-journal adı (`zSuper`) yazılı olduğu halde diskte `zSuper` dosyası **yoksa** (`!exists`), `Pager` o `-journal` dosyasını **geri sarmadan (playback yapmadan) silmelidir** (`pager.c:2903`). Mevcut `recover_hot_journal_under_shared_lock` (`crates/sqliters-pager/src/lib.rs:992-1149`) her `-journal` dosyasını koşulsuz geri sardığından, commit edilmiş veritabanlarından birini geri alıp çoklu-DB atomikliğini bozar.
       - Tersine `zSuper` diskte **varsa**, bir katılımcı kendi `-journal` dosyasını geri sardıktan sonra `zSuper` dosyasını hemen silemez; `pager_delsuper` (`pager.c:2583`) listedeki diğer tüm katılımcı `-journal` dosyalarının da geri sarılıp silindiğini doğrulayana kadar `zSuper` korunmalıdır.
       - Ayrıca C `vdbeCommit` (`sqlite/src/vdbeaux.c:3076-3080`), 8.3 dosya adlarıyla çakışmayı önlemek için super-journal adını `-mj%06X9%02X` biçiminde üretir ve `synchronous` kipine göre katılımcı ve super-journal dizin `sync` adımlarını uygular.
  2. **`VdbeVm` ve `compiler.rs` çoklu-veritabanı sınırı ile `DETACH` kilit denetimi**:
     - `Statement::new` (`crates/sqliters-core/src/lib.rs:391-395`) `SELECT` sorgularını `compiler::try_compile_select` ile `VdbeVm`'e derliyor; ancak `Opcode::{Transaction, OpenRead, OpenWrite, OpenReadIndex, OpenWriteIndex}` (`crates/sqliters-vdbe/src/lib.rs:63-68`) veritabanı indeksi (`iDb`: `0=main, 1=temp, 2..=11`) taşımıyor ve `VdbeVm::step` yalnız tek bir `&mut Btree` kabul ediyor. `temp → main → attached` çözümleme sırasıyla bir `TEMP` tablo `main` tablosunu gölgelediğinde veya `ATTACH` şeması sorgulandığında `compiler.rs` / `sqliters-vdbe` sınırı tanımlanmazsa yanlış B-tree okunur.
     - Ayrıca `DETACH DATABASE` çağrıldığında hedef `iDb` üzerinde aktif `Statement` / `Blob` varsa C `sqlite3Detach` (`sqlite/src/attach.c`) uyumlu olarak `ErrorCode::Locked` ("database ... is locked") döndürülmesi `ActivityTracker`'ın veritabanı bazlı takibine bağlanmalıdır.
- **Öneri**:
  - Faz 2 `[MODIFY]` listesine `crates/sqliters-vfs/src/lib.rs`, `crates/sqliters-pager/src/lib.rs`, `crates/sqliters-btree/src/lib.rs`, `crates/sqliters-core/src/compiler.rs` ve `crates/sqliters-vdbe/src/lib.rs` dosyalarını ekleyin; `readSuperJournal` ("`zSuper` yoksa playback yapma, yalnız sil") + `pager_delsuper` kurallarını, `iDb` yönlendirmesini ve aktif handle varken `DETACH -> Locked` davranışını açıkça belirtin.

---

## 5. [P1] Faz 4 — `TRIGGER` çatışma politikası (`eOrconf` kalıtımı) ile `RAISE(FAIL)` / `OR FAIL` hata sonlandırmasının `FOREIGN KEY` ve `autocommit` kuralları (`sqlite3VdbeHalt`) eksik

- **Plan referansı**: Satır `502–521` (*"Faz 4 — `TRIGGER` altyapısı (`BEFORE`, `AFTER`, `INSTEAD OF`, `RAISE`)"*), satır `261` ve `721` (`fkey3.rs` açılışı).
- **Sorun**:
  1. **Üst ifade çatışma politikasının (`OR IGNORE` / `OR REPLACE` / `OR FAIL` / `OR ROLLBACK` / `OR ABORT`) trigger gövdesine yayılımı**:
     - C SQLite `codeTriggerProgram` (`sqlite/src/trigger.c:1154-1183`), dıştaki DML ifadesinde açık bir çatışma politikası belirtilmişse (`pParse->eOrconf != OE_Default`), trigger gövdesi içindeki `INSERT` ve `UPDATE` adımlarının kendi çatışma politikalarını (ve `RAISE` etkileşimini) üst `eOrconf` ile ezer; yalnız üst ifade varsayılan (`OE_Default`) olduğunda trigger adımlarının kendi `OR ...` politikası uygulanır. Plan bu kuralı tanımlamıyor.
  2. **`RAISE(FAIL, msg)` / `OR FAIL` ile statement-savepoint, `autocommit` ve `FOREIGN KEY` etkileşimi**:
     - Plan satır `516` tek cümleyle *"`RAISE(FAIL, msg)` önceki değişiklikleri korur"* diyor; ancak C SQLite `sqlite3VdbeHalt` (`sqlite/src/vdbeaux.c:3405-3465`) davranışı iki kritik koşula bağlıdır:
       - İfade `OE_Fail` ile durduğunda bile çözümlenmemiş **anlık (immediate) `FOREIGN KEY` ihlali** varsa (`sqlite3VdbeCheckFkImmediate`, `vdbeaux.c:3300-3308, 3406-3408`), hata politikası `OE_Abort`'a yükseltilir (`p->errorAction = OE_Abort`) ve statement-savepoint **geri sarılır** (`FOREIGN KEY constraint failed`).
       - `autocommit` kipinde `OE_Fail` oluştuğunda ve ne anlık ne de ertelenmiş (`DEFERRED`) `FOREIGN KEY` ihlali yoksa önceki satır değişiklikleri diske **commit edilir** (`vdbeCommit`, `vdbeaux.c:3436`) ve çağırana yine constraint hatası döndürülür; buna karşılık ertelenmiş bir `FOREIGN KEY` ihlali varsa (`sqlite3VdbeCheckFkDeferred`, `vdbeaux.c:3421`) tüm işlem **rollback** edilir (`vdbeaux.c:3444`).
     - Faz 4 kapısında açılması zorunlu tutulan `tests/sqlite_test/fkey3.rs` ve trigger+FK testleri doğrudan bu etkileşime dayanır.
- **Öneri**:
  - Faz 4'e üst DML `eOrconf` değerinin trigger gövdesindeki adımlara C `trigger.c` uyumlu kalıtımını ve `OE_Fail` (`RAISE(FAIL)` / `OR FAIL`) sonlandırmasının `sqlite3VdbeHalt` içindeki anlık/ertelenmiş `FOREIGN KEY` yükseltme ve `autocommit` commit/rollback kurallarını ekleyin.

---

## 6. [P1] Faz 6 — Artımlı `Blob` (`Blob::open` / `Blob::write_at`) şema ve bütünlük kısıtları tanımlanmamış; doğrudan hücre güncellemesi indeks, `GENERATED` sütun ve `FOREIGN KEY` bütünlüğünü sessizce bozar

- **Plan referansı**: Satır `591–596` (*"Artımlı BLOB G/Ç (`Blob`) — Yalnızca pager ve transaction üzerinden erişim"*), Test Grubu `18`, `20`, `25`.
- **Sorun**:
  1. `Blob::write_at` (`sqlite3_blob_write`), tablo B-tree yaprak/overflow sayfalarındaki kayıt yükünü (payload) yerinde günceller; ikincil indeksleri yeniden yazmaz, `GENERATED` sütunları yeniden hesaplamaz, `FOREIGN KEY` denetimi veya `UPDATE` trigger'ı çalıştırmaz.
  2. Bu nedenle C SQLite `sqlite3_blob_open` (`sqlite/src/vdbeblob.c:163-248`) açılışta katı şema kısıtları uygular:
     - Hem okuma hem yazma açılışlarında `VIRTUAL TABLE` (`"cannot open virtual table: ..."`), `WITHOUT ROWID` tablolar (`"cannot open table without rowid: ..."`), `GENERATED` sütun barındıran tablolar (`TF_HasGenerated` → `"cannot open table with generated columns: ..."`) ve `VIEW` nesneleri (`"cannot open view: ..."`) reddedilir.
     - Yazma (`read-write` / `wrFlag != 0`) açılışlarında ek olarak: herhangi bir indeksin anahtarında yer alan sütunlar veya tabloda ifade indeksi (`XN_EXPR`) bulunması durumu (`"cannot open indexed column for writing"`, `vdbeblob.c:232-244`) ile `PRAGMA foreign_keys = ON` iken çocuk `FOREIGN KEY` tanımında yer alan sütunlar (`"cannot open foreign key column for writing"`, `vdbeblob.c:215-230`; ebeveyn anahtarlar indeksli olduğundan yine yakalanır) `ErrorCode::Error` ile reddedilir.
     - Ayrıca çalışma zamanında hedef hücrenin tipi `TEXT` veya `BLOB` değilse (`NULL`, `INTEGER`, `REAL` — ör. `zeroblob` yerine tamsayı varsa) C `blobSeekToRow` (`vdbeblob.c:103-118`) `"cannot open value of type ..."` hatası üretir.
  3. Plan satır `591–596` yalnızca `Pager::read_page`/`write_page` kullanımını ve satır silme/güncelleme invalidation'ını anlatıyor; bu açılış ve tip muhafazaları olmadan `Blob::write_at` ikincil indeksleri, `STORED` türetilmiş sütunları ve `FOREIGN KEY` değişmezlerini sessizce bozar (veritabanı bozulması / `integrity_check` hatası).
- **Öneri**:
  - Faz 6 `Blob::open` sözleşmesine `sqlite/src/vdbeblob.c:103-248` ile birebir uyumlu tablo/sütun/tip kısıtlarını (`VIEW`, `VIRTUAL TABLE`, `WITHOUT ROWID`, `GENERATED` sütunlu tablo, yazmada indeksli/ifade-indeksli ve `foreign_keys=ON` altında FK sütunları, `TEXT`/`BLOB` dışı hücre tipleri) ve şema değiştiğinde (`schema_cookie`) yeniden bağlanma/geçersizleşme kuralını ekleyin.

---

## 7. [P2] Faz 1.1 & Faz 7 — `pragma5.rs` açılış önkoşulu (`pragma_pragma_list`, eponymous sanal tablolarda `PRAGMA table_info`, `fts5=1`) ve Faz 1.1 capability tablosundaki uzantı bayrakları eksik

- **Plan referansı**: Satır `234–248` (Faz 1.1 capability tablosu), satır `260` ve `653` (`tests/sqlite_test/pragma5.rs` açılış kapısı), Test Grubu `30` ve `31`.
- **Sorun**:
  1. **`tests/sqlite_test/pragma5.rs` için eksik önkoşullar**:
     - Plan satır `260` ve `653`, `pragma5.rs`'in bütün alt testleriyle açılması için yalnızca `pragma_function_list`, `pragma_module_list` ve Faz 6 UDF bağlarını listeliyor.
     - Oysa `tests/sqlite_test/pragma5.rs:21-48` ek olarak üç özelliğe daha ihtiyaç duyar:
       1. Alt test `3.0` ve `3.1` **`pragma_pragma_list`** eponymous sanal tablosunu (`SELECT * FROM pragma_pragma_list WHERE name='pragma_list'`) sorgular; mevcut `crates/sqliters-core/src/engine.rs` içinde `PRAGMA pragma_list` komutu da henüz yoktur.
       2. Alt test `1.0`, `2.0` ve `3.0` doğrudan **`PRAGMA table_info(pragma_function_list)`**, **`PRAGMA table_info(pragma_module_list)`** ve **`PRAGMA table_info(pragma_pragma_list)`** çalıştırır. Faz 3 satır `497` `PRAGMA table_info` / `table_xinfo` genişletmesini yalnız `VIEW` için tanımlıyor; Faz 7'de `PRAGMA table_info` / `table_xinfo`'nun sanal ve eponymous tabloların `CREATE TABLE` deklarasyonunu (`xConnect`) çözmesi zorunludur.
       3. Alt test `2.0` ve `2.1` `ifcapable fts5` muhafazası altındadır (`pragma5.rs:29`); `pragma5`'in *"bütün alt testleriyle"* (satır `260`) çalışması için Faz 7'de `fts5 = 1` olması gerekir.
  2. **Faz 1.1 capability tablosunda (`satır 236–248`) mevcut `harness.rs` bayraklarıyla uyumsuzluk**:
     - Mevcut `crates/sqliters-tcltest/src/harness.rs:191-232` içinde `"fts3"`, `"fts5"`, `"incrblob"`, `"json"`, `"load_ext"`, `"shared_cache"`, `"utf16"` bayrakları sabit `"1"` olarak tanımlıdır.
     - Planın satır `236–248` tablosu Faz 6 satırında `incrblob`'u, Faz 7 satırında (`vtab`, `rtree` yanında) **`fts5`** ve **`json`** yeteneklerini, ayrıca plan kapsamında hiç uygulanmayacak olan `fts3`, `load_ext`, `shared_cache`, `utf16` bayraklarının kalıcı `0` durumunu açıkça listelemiyor.
- **Öneri**:
  - Faz 7'ye `PRAGMA pragma_list` + `pragma_pragma_list` eponymous tablosunu ve sanal/eponymous tablolar üzerinde `PRAGMA table_info` / `table_xinfo` desteğini ekleyin; satır `236–248` capability tablosunda Faz 6 için `incrblob`, Faz 7 için `fts5` ve `json` (ve kapsam dışı `fts3`, `load_ext`, `shared_cache`, `utf16` için kalıcı `0`) eşlemelerini açıkça tanımlayın.
