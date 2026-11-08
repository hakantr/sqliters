# `implementation_plan.md` Değerlendirme Bulguları

- **Değerlendirilen commit**: `2fff5b7cba3408751140e68512e3f63f187a164f` (`docs: resolve committed v11 review findings in plan v12`, v12)
- **Değerlendirme kapsamı**: Faz 1.1, Faz 1.2, Faz 1.3, Faz 2, Çapraz-faz yürütme hizmetleri, Faz 3, Faz 4, Faz 5, Faz 6, Faz 7 ve Doğrulama Planı (satır `1–931` tamamı; C SQLite 3.54.1 `sqlite/src/*`, `sqlite/ext/*`, `sqlite/test/*` ve mevcut `crates/*` ile karşılaştırmalı)

---

## 1. [P1] Faz 1.1, Faz 1.2 & Faz 2 — Rollback `journal_mode` durum makinesi (`DELETE` / `TRUNCATE` / `PERSIST` / `MEMORY` / `OFF`) hiçbir fazda tanımlanmamış; `lock-7.3`, Faz 1.2 WAL geçişleri ve Faz 2 super-journal temizliği boşlukta kalıyor

- **Plan referansı**: Satır `221` ve `257` (Faz 1.1 `lock-7.*` doğrulaması), satır `296` (Faz 1.2 `DELETE/TRUNCATE/PERSIST → WAL` geçişleri), satır `479` ve `483` (Faz 2 `PERSIST` artığı kısaltma ve katılımcı journal finalizasyonu), satır `258` ve `591` (Faz 6 tam `lock.rs`).
- **Sorun**:
  1. Mevcut `crates/sqliters-core/src/engine.rs:9055-9062` içinde `PRAGMA journal_mode`, verilen argümanı (`= truncate`, `= persist`, `= memory`, `= off`, `= wal`, `= delete`) tamamen yok sayıyor ve disk veritabanları için her zaman sabit `"delete"` döndürüyor. `crates/sqliters-pager/src/lib.rs` içinde de hiçbir `JournalMode` durumu yok; commit her zaman yalnız `vfs.delete(jpath)` çağırıyor.
  2. Oysa Faz 1.1'de hedefli olarak ve Faz 6'da tam `tests/sqlite_test/lock.rs:331-340` içinde çalıştırılması zorunlu kılınan `lock-7.3` alt testinin C kaynağındaki amacı (`sqlite/test/lock.test:419-444`), **`journal_mode` `'delete'` dışında bir kipteyken (`PRAGMA journal_mode = truncate`)**, açık salt-okunur `SELECT` (`$STMT`) varken `0` sayfa değiştiren bir yazma işleminin (`BEGIN; UPDATE t4 SET a = 10 WHERE 0; COMMIT;`) commit sonrasında `RESERVED`/`EXCLUSIVE` kilidini tutmayıp `SHARED` kilide (`Write → Read`) düştüğünü doğrulamaktır. `PRAGMA journal_mode = truncate` gerçekte uygulanmadığı sürece hem `[execsql {PRAGMA journal_mode = truncate}]` çıktısı (`"delete"` vs `"truncate"`) C ile uyuşmaz hem de `Pager::commit` `ftruncate(jfd, 0)` / sıfır-başlık (`PERSIST`) finalizasyon yolunu hiç çalıştırmaz.
  3. Aynı şekilde Faz 1.2 (`DELETE/TRUNCATE/PERSIST → WAL` geçişleri, satır `296`) ve Faz 2 (`PERSIST` journal kuyruğunun super-journal yazımı öncesi kesilmesi ve cold journal temizliği, satır `479, 483`) Pager'da gerçek bir `JournalMode` durum makinesinin varlığını varsayıyor, ancak hiçbir faz bu durum makinesinin ve `PRAGMA journal_mode = ...` komutunun uygulanmasını planlamıyor.
- **Öneri**:
  - Faz 1.1 kapsamında `crates/sqliters-pager/src/lib.rs` ve `crates/sqliters-core/src/engine.rs` içine rollback `JournalMode::{Delete, Truncate, Persist, Memory, Off}` durum makinesini (`TRUNCATE` için `journal.truncate(0)`, `PERSIST` için ilk sektör başlığını sıfırlama, aktif yazma işlemi içinde yasak geçiş denetimi ve `PRAGMA journal_mode [= kip]` dönüş değeri) ekleyin; `WAL` kipini Faz 1.2'de aynı durum makinesine bağlayın.

---

## 2. [P1] Faz 1.1 — `BEGIN DEFERRED` (`explicit_tx = true`, `Pager = Open/NoLock`) altında ilk şema okuması ve `BUSY` ile reddedilen yazma başlangıcı (`lock7-1.5` → `lock7-1.7`) için `NoLock` geri dönüş kuralı satır `170` ile çelişiyor

- **Plan referansı**: Satır `168–170` (`finish_activity` ve *"Son okuyucu ve dış transaction da bittiğinde Read → Open/NoLock olur"*), satır `225, 233, 256, 271–277` (`lock7.rs` Faz 1.1 kapısı ve gecikmiş şema yükleme).
- **Sorun**:
  1. Faz 1.1 kapısında doğrudan açılacak `tests/sqlite_test/lock7.rs:11-35` akışı `v12` kurallarıyla izlendiğinde:
     - `sqlite3 db2 test.db` → `db2` `Open/NoLock, schema_loaded = false` olarak açılır (`satır 271`).
     - `db2 eval {BEGIN}` → şemasız `BEGIN DEFERRED`; `explicit_tx = true` (`autocommit = false`) olur, `Pager` `Open/NoLock` ve `schema_loaded = false` kalır (`lock7-1.3`: `main unlocked temp closed`).
     - `db1` `INSERT INTO t1 VALUES(1, 1)` çalıştırarak `RESERVED` kilidi alır (`lock7-1.4`, `lock7-1.6`).
     - `lock7-1.5`'te `db2` `catchsql { INSERT INTO t1 VALUES(2, 2) } db2` çalıştırır:
       a) `INSERT` şema gerektirdiğinden `prepare` aşamasında `db2` `SHARED` kilit alır (`db1` yalnız `RESERVED` tuttuğu için `SHARED` **başarılı** olur) ve `sqlite_schema` okunur.
       b) Eğer satır `170`'teki *"Son okuyucu ve **dış transaction da bittiğinde** Read → Open/NoLock olur"* kuralı `explicit_tx == true` bayrağını "dış transaction aktif" sayıp `prepare` içindeki iç şema okumasından sonra `SHARED` kilidi korursa, **veya** `step` aşamasında `Open → Shared → Reserved` yükseltmesi `Reserved` adımında `SQLITE_BUSY` aldığında `Shared`'a düşerse, `db2` `SHARED` kilitte takılı kalır.
       c) Hemen sonraki `lock7-1.7` (`execsql { PRAGMA lock_status } db2`) `{main unlocked temp closed}` beklerken `{main shared temp closed}` üretir ve `GATE-1.1` kırılır.
  2. C SQLite `sqlite3InitOne` (`sqlite/src/prepare.c:433-435`), `db->autoCommit == 0` (`BEGIN DEFERRED`) olsa dahi B-tree işlemini kendisi açtıysa (`openedTransaction == 1`) şema yüklemesi biter bitmez `sqlite3BtreeCommit` ile `NoLock` durumuna döner; `btreeBeginTrans` (`sqlite/src/btree.c:3759-3762`) de çağrı başında `inTrans == TRANS_NONE` ise `RESERVED` `SQLITE_BUSY` hatasında `unlockBtreeIfUnused` ile `SHARED` kilidi tamamen bırakıp `NoLock`'a döner.
- **Öneri**:
  - Faz 1.1'de SQL düzeyindeki `explicit_tx` (`BEGIN DEFERRED` bayrağı) ile B-tree üzerinde fiilen başlatılmış kullanıcı okuma/yazma işlemini (`in_user_btree_tx`) açıkça ayırın: B-tree henüz `Open/NoLock` iken çalışan iç `ensure_schema_loaded` okuması tamamlandığında ve `Open → Write` geçişi `RESERVED` adımında `BUSY`/hata aldığında, `explicit_tx == true` olsa bile `Pager` yeniden `Open/NoLock` (`unlocked`) durumuna dönmelidir.

---

## 3. [P1] Faz 1.1, Faz 2 & Faz 6 — Tcl harness'ta (a) `rename <db> {}` komut silme/kapatma kancası, (b) `sqlite_pending_byte` test kontrolü ve (c) açılmış bellek-içi `temp` için `PRAGMA lock_status = unknown` kuralı eksik; `lock.rs`, `lock2.rs` ve `lock4.rs` doğrudan etkileniyor

- **Plan referansı**: Satır `216`, `229–234` (Faz 1.1 `close`, `PRAGMA lock_status`), satır `258–259` ve `588–591` (Faz 6 `lock.rs`, `lock2.rs`, `lock4.rs` altyapısı).
- **Sorun**:
  1. **`rename db2 {}` bağlantıyı kapatmıyor ve komutu silmiyor (`lock-999.1`, `lock4-999.1`)**:
     - `tests/sqlite_test/lock.rs:344` (`lock-999.1`) ve `tests/sqlite_test/lock4.rs:77` (`lock4-999.1`), bağlantıyı `db2 close` yerine `rename db2 {}` ile sonlandırır (C `sqlite/src/tclsqlite.c` içindeki `Tcl_CreateObjCommand(..., DbDeleteCmd)` silme kancası).
     - Mevcut `crates/sqliters-tcltest/src/interp.rs:1013-1025` içindeki `cmd_rename` yalnızca kullanıcı `proc` tablosuna (`self.procs`) bakıyor; `host.dbs` içindeki veritabanı komutlarını ve yerleşik komutları (`tester.tcl:115` içindeki `rename sqlite3 sqlite_orig` dahil) ne yeniden adlandırıyor ne de silindiğinde bağlantıyı kapatıyor. Sonuçta açık `Connection`, OS dosya tanıtıcısı ve `.db`/`-shm` kilitleri sızar; `db2` komutu da silinmemiş kalır.
  2. **`sqlite_pending_byte` değişkeni ve `sqlite3_test_control_pending_byte` eksikliği (`lock2.rs`, `lock4.rs`)**:
     - `tests/sqlite_test/lock4.rs:30` doğrudan `puts $out "sqlite3_test_control_pending_byte [set sqlite_pending_byte]"` çalıştırır; `tests/sqlite_test/lock2.rs:7` ise `sqlite/test/lock_common.tcl` dosyasını `source` eder ve `lock_common.tcl:80-83` `$::sqlite_pending_byte` değişkenini okur (`sqlite/src/test2.c:727, 747`).
     - `crates/sqliters-tcltest/src/harness.rs:129-161` (`bootstrap_tester`) `tester.tcl` dosyasını atladığı (`cmd_source` satır `882`) ve `sqlite_pending_byte` değişkenini tanımlamadığı için her iki test de `can't read "sqlite_pending_byte": no such variable` hatasıyla kırılır. Ayrıca `harness.rs:475` `sqlite3_test_control_pending_byte` çağrısını no-op `"0"` yaparken C `tester.tcl:102` bunu `0x0010000` (64 KiB) yapar; çok süreçli testlerde ebeveyn/alt süreçlerin (ve varsa C yardımcı sürecin) aynı pending-byte ofsetini kullanması zorunludur.
  3. **Açılmış `TEMP` veritabanında `PRAGMA lock_status` çıktısı (`lock-7.2`, `lock-7.3` içinde `temp unknown`)**:
     - `tests/sqlite_test/lock.rs:253`'te `CREATE TEMP TABLE t3(x)` çalıştıktan sonra satır `323–339`'daki `lock-7.2` ve `lock-7.3` alt testleri `PRAGMA lock_status` sonucunun **`{main shared temp unknown}`** olmasını bekler (`closed` veya `unlocked` değil).
     - C `sqlite/src/pragma.c:2757-2766` + `sqlite/src/main.c:4265` içinde bellek-içi (`MemoryVfs` / `temp_store >= 2`) veya henüz fiziksel OS dosyası açmamış geçici pager üzerinde `SQLITE_FCNTL_LOCKSTATE` `SQLITE_NOTFOUND` döndürdüğünden `zState` `"unknown"` kalır. Plan satır `231–232` her `Pager` için koşulsuz `VfsFile::lock_level()` (`NoLock` → `"unlocked"`) döndürürse Faz 6'da tam `lock.rs` açıldığında `lock-7.2` ve `lock-7.3` `{main shared temp unlocked}` üreterek başarısız olur.
- **Öneri**:
  - `interp.rs` / `harness.rs` içinde host komut yeniden adlandırma ve `rename <cmd> {}` silme geri çağrısını (`DbDeleteCmd` eşdeğeri deterministik statement/connection cleanup) tanımlayın; `sqlite_pending_byte` bağlı değişkenini ve `sqlite3_test_control_pending_byte` yapılandırmasını VFS/Pager/Btree pending-byte ofsetiyle uyumlu hale getirin; `PRAGMA lock_status` içinde gerçek OS kilit durumu sunmayan bellek-içi/dosyasız (`temp` / `:memory:`) pager'lar için C `SQLITE_FCNTL_LOCKSTATE -> SQLITE_NOTFOUND` kuralına uygun olarak `"unknown"` döndürün.

---

## 4. [P1] Faz 3, Faz 4 & Faz 7 — Mevcut `ALTER TABLE RENAME TO` ve `ALTER TABLE RENAME COLUMN` (`engine.rs:7765-7960`), `VIEW`, `TRIGGER` (`TEMP` dahil) yeniden yazımını ve `VIRTUAL TABLE` `xRename` (`FTS5` / `R-Tree` gölge tabloları) sözleşmesini kapsamıyor

- **Plan referansı**: Satır `519–524` (Faz 3 `VIEW` ve `ALTER TABLE DROP COLUMN`), satır `527–549` (Faz 4 `TRIGGER`), satır `640–692` (Faz 7 `VirtualTable`, `FTS5`, `R-Tree`).
- **Sorun**:
  1. Plan satır `524` yalnızca **`ALTER TABLE DROP COLUMN`** için `VIEW`/`TRIGGER` doğrulamasını tanımlıyor; `crates/sqliters-core/src/engine.rs:7765-7960` içinde hâlihazırda bulunan `ALTER TABLE ... RENAME TO ...` ve `ALTER TABLE ... RENAME COLUMN ...` kodunun Faz 3, Faz 4 ve Faz 7 nesneleriyle etkileşimini kapsam dışı bırakıyor.
  2. Mevcut `exec_alter_table` yalnızca aynı şemadaki `CREATE TABLE`, `CREATE INDEX`, diğer tablolardaki `REFERENCES` kısıtlarını ve `sqlite_sequence` kayıtlarını güncelliyor. Oysa C SQLite `sqlite/src/alter.c` (`sqlite3AlterRenameTable`, `sqlite3AlterRenameColumn`, `renameTableFunc`, `renameColumnFunc`):
     - **Faz 3 (`VIEW`)**: Tablo veya sütun yeniden adlandırıldığında hem hedef şemadaki hem `TEMP` şemasındaki (`sqlite_temp_schema`) `VIEW` tanımlarında ilgili tablo/sütun referanslarını yeniden yazar ve tüm şemayı yeniden çözümleyerek doğrular; geçersizleşen görünüm varsa savepoint ile tüm işlemi geri sarar (`error in view ... after rename`).
     - **Faz 4 (`TRIGGER`)**: Hem hedef şemadaki hem `TEMP` şemasındaki `TRIGGER` tanımlarında (`ON tbl`, `UPDATE OF col`, `WHEN`, `OLD.col`/`NEW.col` ve gövde DML/SELECT ifadeleri) tablo/sütun adlarını ve `sqlite_schema.tbl_name` alanını günceller ve doğrular.
     - **Faz 7 (`VirtualTable::rename` / `xRename`)**: Sanal tablo üzerinde `ALTER TABLE vtab RENAME TO new_name` çalıştırıldığında modülün `xRename` metodu (`fts5Rename` `sqlite/ext/fts5/fts5_main.c`, `rtreeRename` `sqlite/ext/rtree/rtree.c`) `VTabHostContext` alt-SQL yetkisiyle çağrılarak `FTS5` (`_data`, `_idx`, `_content`, `_docsize`, `_config`) ve `R-Tree` (`_node`, `_rowid`, `_parent`) gölge tabloları atomik olarak yeniden adlandırılır. Faz 7 `vtab_api.rs` ve `VTabHostContext` listesinde (`satır 642, 676–686`) `xRename` kancası hiç yer almıyor.
- **Öneri**:
  - Faz 3 ve Faz 4 kapsamına `ALTER TABLE RENAME TO` / `RENAME COLUMN` işlemlerinin atomik savepoint altında hedef şema + `temp` şemasındaki `VIEW` ve `TRIGGER` tanımlarını C `alter.c` uyumlu yeniden yazıp doğrulamasını ekleyin; Faz 7 `VirtualTable` ve `VTabHostContext` sözleşmesine `xRename` metodunu (`FTS5` ve `R-Tree` gölge tablo yeniden adlandırması dahil) ekleyin.

---

## 5. [P1] Faz 6 — Çevrimiçi `Backup` (`backup.c`) kaynak okuma kilidi ömrü ve aynı bağlantıdan kaynak yazımlarında (`backupUpdate`) deterministik kilit sırası / ABBA deadlock riski Faz 1.1 modeliyle çelişiyor

- **Plan referansı**: Satır `157` (*"Backup iki bağlantının lease'lerini deterministik sırayla alır"*), satır `167` (*"Backup da kendi kaynak snapshot/hedef transaction lease'lerini izler"*), satır `611–617` (*"Canlı çevrimiçi yedekleme (`Backup`)"*).
- **Sorun**:
  1. **Kaynak okuma snapshot'ının `step` çağrıları arasında tutulması hatası**:
     - Plan satır `167`, `Blob` ile birlikte `Backup`'ın da açık kaldığı süre boyunca kaynak snapshot lease'ini taşıdığını ifade ediyor.
     - Oysa C SQLite `sqlite3_backup_step` (`sqlite/src/backup.c:363-366, 575-580`), kaynak `:memory:` değilse ve kaynak bağlantıda önceden kullanıcı tarafından başlatılmış bir okuma işlemi yoksa (`bCloseTrans = 1`), **her `Backup::step(n)` çağrısının sonunda kaynak B-tree okuma işlemini (`SHARED` / WAL read-slot) kapatır**; yalnızca **hedef (`pDest`)** üzerindeki `EXCLUSIVE` yazma işlemini `SQLITE_DONE` veya `finish`/`Drop` anına kadar açık tutar. Kaynak snapshot'ını `step` çağrıları arasında tutmak tüm dış/iç yazıcıları ve WAL checkpoint'lerini yedekleme bitene kadar bloklayarak çevrimiçi yedekleme sözleşmesini bozar. Ayrıca kaynak B-tree o anda bir yazma işlemi içindeyse `Backup::step` hemen `SQLITE_BUSY` döndürmelidir (`backup.c:352`).
  2. **Aynı süreçte kaynak bağlantıya yazma (`backupUpdate`) ile deterministik kilit sırası arasındaki ABBA deadlock riski**:
     - C `backup.c:690-713` (`backupUpdate`), `step` aralarında kaynak bağlantının (`pSrcDb`) kendi üzerinde sayfa yazması/geri sarması durumunda (`iPage < p->iNext`), `pSrc` kilidi eldeyken `pDestDb->mutex` kilidini alır (`src → dest`).
     - Plan satır `157` ise `Backup`'ın iki bağlantının lease/mutex'lerini deterministik sırayla (ör. `connection_id` sırasıyla; `dest_id < src_id` ise `dest → src`) alacağını şart koşuyor. Kaynak bağlantı kendi `UPDATE`/rollback çağrısı sırasında `src_id` guard'ı altındayken doğrudan `dest_conn` içine yazmaya kalkarsa eşzamanlı `Backup::step` (`dest_id → src_id`) ile **ABBA deadlock** (veya giriş kapısında sahte `Busy`) oluşur.
- **Öneri**:
  - Faz 6 `Backup` sözleşmesinde: (a) kaynak okuma snapshot'ının yalnız `Backup::step` çağrısı süresince tutulup adım sonunda bırakıldığını (`:memory:` ve mevcut dış okuma işlemi istisnalarıyla), hedefin yazma işleminin ise `DONE`/`finish`/`Drop`'a kadar korunduğunu; (b) `src_conn` üzerindeki Pager yazmalarının `dest_conn` mutex'ini kilitlemeden ortak yaprak bir kirli-sayfa izleyicisine (`Bitvec` / restart bayrağı) kayıt bıraktığını ve sonraki `Backup::step`'in deterministik çift-lease altında `iPage < iNext` sayfalarını yeniden kopyaladığını; (c) aynı kaynak/hedef veritabanı açılışının `backup.c:255` gibi `ErrorCode::Error` ("source and destination must be distinct") ile reddedildiğini netleştirin.

---

## 6. [P2] Çapraz-faz & Faz 2/3/4 — `runtime_api.rs` / `execution_context.rs` faz zamanlaması ile çoklu-Btree (`iDb`) trait katman sınırı (`sqliters-types` vs `sqliters-btree`/`sqliters-vdbe`) belirsiz

- **Plan referansı**: Satır `462` ve `472` (Faz 2 `VdbeVm::step` çoklu-DB erişimi), satır `492–513` (Çapraz-faz zorunluluğu), satır `519` ve `525` (Faz 3), satır `535` (Faz 4), satır `650–660` (Cargo bağımlılık grafiği).
- **Sorun**:
  1. **Trait katman sınırı çelişkisi**:
     - Faz 2 satır `472`, *"`VdbeVm::step` tek `&mut Btree` varsayımı yerine alt runtime sözleşmesindeki dar, Result döndüren DB/B-tree erişim arayüzünü kullanır"* diyor.
     - Ancak satır `502` ve `654–660`'taki döngüsüz Cargo grafiğinde `sqliters-types` en alt crate'tir (`sqliters-btree -> sqliters-pager, sqliters-types`); `sqliters-types/src/runtime_api.rs` `Btree` tipini göremez (`satır 502`: *"Trait, somut pager/VM/Connection tiplerini imzasına almaz"*). Dolayısıyla `iDb -> &mut Btree` (veya çoklu-Btree cursor/transaction) arayüzü `sqliters-types` içinde değil, `Btree` bağımlılığına sahip **`sqliters-btree` veya `sqliters-vdbe`** katmanında tanımlanmalıdır.
  2. **Faz kapısı sıralaması boşluğu**:
     - `[NEW] crates/sqliters-types/src/runtime_api.rs` ve `[NEW] crates/sqliters-core/src/execution_context.rs` yalnız Faz 2 ile Faz 3 arasındaki numarasız *"Çapraz-faz zorunluluğu"* bölümünde (`satır 496–498`) yer alıyor; ne Faz 2 ne Faz 3 dosya listesinde var ve hiçbir `GATE-*` kapısına atanmamış.
     - Buna karşılık Faz 3 satır `525` (`WITH RECURSIVE` içinde `InterruptHandle` kontrol noktası) ve Faz 4 satır `535` (*"önceki fazda oluşturulmuş `execution_context` kullanılır"*) bu dosyaların önceden oluşturulmuş olmasını şart koşuyor.
- **Öneri**:
  - Çoklu-Btree (`iDb`) depolama erişim trait'inin `sqliters-vdbe` (veya `sqliters-btree`) içinde, B-tree'den bağımsız `ExecutionServices` (interrupt/progress/UDF/collation/hook) trait'inin ise `sqliters-types/src/runtime_api.rs` içinde yer aldığını açıkça ayırın ve `runtime_api.rs` + `execution_context.rs` oluşturulmasını somut bir faza (**Faz 2** veya **Faz 3 başlangıcı** + ilgili `GATE`) bağlayın.
