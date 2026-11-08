# `implementation_plan.md` İnceleme Bulguları (v15 — Commit `3a88c700`)

**İncelenen commit**: `3a88c700090453d060821c53a18976e469eb5f9d` (`docs: resolve v14 review findings in plan v15`)
**Kapsam**: `implementation_plan.md` dosyasının tamamı (`Faz 1.1` → `Faz 7` ve `Doğrulama Planı`), C SQLite 3.54.1 referans kaynakları (`sqlite/src/*`, `sqlite/ext/*`, `sqlite/test/*`) ve mevcut `sqliters` Rust çalışma alanı (`crates/*`, `tests/*`) ile çapraz doğrulanmıştır.

Önceki turdaki (`v14` / `d96ac557`) 7 bulgunun tamamının `v15` planında giderildiği doğrulanmıştır. Güncel `v15` planında tespit edilen yeni mimari tutarsızlıklar ve eksik önkoşullar aşağıdadır:

---

## 1. [P1] Faz 1.1, Faz 1.2, Faz 2 ve Faz 4: `SAVEPOINT` yığını yanlış katmanda (`Pager` içinde) ve erken yazma kilidiyle (`begin_write`) modellenmiş; salt-okunur bağlantıda `SAVEPOINT`, `SAVEPOINT_BEGIN` `nVdbeWrite` engeli, çoklu-DB (`DatabaseSet`) geç katılımlı geri sarma ve `deferred_fk_suspects` kısmi geri alması kırılır

- **Plan referansı**: `Faz 1.1` (satır `169` yalnız `COMMIT`/`RELEASE` için `nVdbeWrite` engeli, satır `171` yalnız `BEGIN DEFERRED` için `NoLock`), `Faz 1.2` (satır `368` savepoint açılışında `WRITE` altında token alma), `Faz 2` (satır `548, 550`), `Faz 4` (satır `630–635`), Test Grupları `25` ve `42` (satır `840, 857`).
- **Sorun**:
  1. **Erken `begin_write()` hatası (`crates/sqliters-pager/src/lib.rs:847-850`, `crates/sqliters-core/src/engine.rs:320-326`)**:
     - Mevcut `Pager::savepoint`, `if self.state != TransactionState::Write { self.begin_write()?; }` ile her `SAVEPOINT sp` çağrısında anında `RESERVED` / yazma transaction'ı başlatmaktadır.
     - Oysa C SQLite `sqlite/src/vdbe.c:4000-4042` (`OP_Savepoint` / `SAVEPOINT_BEGIN`), `SAVEPOINT sp` çalıştığında hiçbir B-tree veya OS kilidi almaz; yalnızca bağlantı düzeyinde ertelenmiş bir transaction-savepoint kaydı açar (`db->autoCommit = 0; db->isTransactionSavepoint = 1`). Mevcut davranışta: (a) `OpenFlags::READONLY` bağlantıda `SAVEPOINT sp; SELECT ...; RELEASE sp;` doğrudan `SQLITE_READONLY` ile kırılır; (b) başka bağlantı `SHARED`/`EXCLUSIVE` kilit tutarken henüz tablo erişimi yapmayan `SAVEPOINT sp;` yanlışlıkla `SQLITE_BUSY` döner; (c) Faz 1.2 satır `368`'deki *"savepoint açılışı token'ı `WRITE` altında alır"* kuralı henüz yazmaya girmemiş `SAVEPOINT sp` ile çelişir (WAL/rollback savepoint anlık görüntüsü, ilgili B-tree gerçekten `WRITE` durumuna geçtiğinde tembel olarak alınmalıdır).
  2. **`SAVEPOINT_BEGIN` için eksik aktif yazıcı (`nVdbeWrite`) denetimi**:
     - Plan satır `169` ve Test Grubu `25` (satır `840`), aktif yazıcı ifade (`nVdbeWrite > 0`) engelini yalnızca `COMMIT` ve `RELEASE` için tanımlamaktadır. Oysa C SQLite `sqlite/src/vdbe.c:4001-4006` yeni `SAVEPOINT` açılışında da (`p1 == SAVEPOINT_BEGIN && db->nVdbeWrite > 0`) `"cannot open savepoint - SQL statements in progress"` ile `SQLITE_BUSY` döndürür.
  3. **Çoklu-DB (`DatabaseSet`) geç yazma katılımı ve `deferred_fk_suspects` geri sarması**:
     - İsimli savepoint yığını tekil `Pager` içinde (`crates/sqliters-pager/src/lib.rs:78`) tutulduğunda, Faz 2'de `SAVEPOINT sp` açıldıktan *sonra* ilk kez yazmaya başlayan bağlı bir veritabanı (`aux` veya `temp`) o savepoint kaydına sahip olmaz. C SQLite'ta savepoint listesi bağlantı düzeyindedir (`db->pSavepoint`, `db->nSavepoint`), her B-tree yazmaya girdiğinde o andaki `db->nSavepoint` derinliğine kadar anonim/indeksli alt savepoint yuvalarını tembel açar ve `ROLLBACK TO` tüm `0..nDb` B-tree'lerine indeks tabanlı (`iSavepoint`) uygulanır (`sqlite/src/vdbe.c:4092-4110`).
     - Ayrıca mevcut `crates/sqliters-core/src/engine.rs:299-303` `ROLLBACK TO sp` sırasında `deferred_fk_suspects` listesini savepoint açılışındaki duruma geri sarmaz; oysa C `sqlite/src/vdbe.c:4039-4040, 4141-4142` ertelenmiş kısıt sayaçlarını (`nDeferredCons`, `nDeferredImmCons`) savepoint kaydından geri yükler.
- **Öneri**:
  - İsimli `Savepoint` yığını `Pager`'dan bağlantı (`Connection` / `DatabaseSet`) katmanına taşınmalı (`is_transaction_savepoint` ve ertelenmiş FK durumu ile birlikte); `SAVEPOINT sp` hiçbir erken yazma kilidi almamalı ve `nVdbeWrite > 0` ise `SQLITE_BUSY` döndürmelidir.
  - `Pager`/`Btree` katmanı isim yerine indeks tabanlı alt savepoint yuvaları tutmalı; bir veritabanı ilk kez `WRITE` durumuna geçerken mevcut bağlantı savepoint derinliğine kadar anlık görüntülerini (rollback önbellek/boyut + `WAL_SAVEPOINT_NDATA`) o anda kaydetmelidir.

---

## 2. [P1] Faz 1.1 ve Faz 1.2: Temiz kapanmış (`-wal` silinmiş, `page1[18..=19] == 2`) veya 0-bayt `.db` yanında yetim `-wal` bulunan veritabanlarında iki aşamalı WAL otomatik açılış (`pagerOpenWalIfPresent` + `lockBtree` `page1[19] == 2`) durum makinesi tanımlanmamış

- **Plan referansı**: `Faz 1.1` (satır `309–315`), `Faz 1.2` (satır `347–350`), `Manuel Smoke Testi` (satır `954–980`).
- **Sorun**:
  - C SQLite `sqlite/src/wal.c:2610-2646` (`sqlite3WalClose`), WAL kipindeki son bağlantı temiz kapandığında `.db` üzerinde `EXCLUSIVE` kilit alıp checkpoint yapar ve (`PERSIST_WAL` yoksa) **`-wal` ile `-shm` dosyalarını siler**, fakat `.db` başlığındaki `18.` ve `19.` baytları **`(2, 2)` olarak bırakır** (planın kendi satır `954–972` birlikte çalışabilirlik smoke testi tam olarak bu durumu üretir).
  - Veritabanı yeniden açıldığında ilk kilitli okumada (`sqlite/src/pager.c:3393-3421, 5498-5508` ve `sqlite/src/btree.c:3356-3382`) iki ayrı geçiş zorunludur:
    1. **`pagerOpenWalIfPresent`**: `.db` üzerinde `SHARED` kilit alındıktan sonra diskte `-wal` dosyası mevcutsa ve `.db` boyutu `> 0` ise Sayfa 1 okunmadan **önce** WAL açılır; `.db` boyutu `0` ise yetim `-wal` dosyası silinir (`sqlite3OsDelete`) ve rollback kipinde kalınır.
    2. **`lockBtree` (`page1[19] == 2` geçişi)**: Diskte `-wal` yokken Sayfa 1 okunduğunda `read_version == 2` görülürse yalnızca `DatabaseHeader::parse` kabulü (satır `349`) yetmez: `Pager` derhal `JournalMode::Wal` kipine geçirilip `open_wal` çalıştırılmalı, kullanıcı açıkça değiştirmemişse varsayılan `synchronous` düzeyi `WAL_SYNCHRONOUS` (`NORMAL`) yapılmalı, WAL açılmadan önce okunan Sayfa 1 önbellekten bırakılıp (`releasePageOne`) WAL okuma snapshot'ı altında yeniden doğrulanmalıdır. Ayrıca `page1[18] > 2` (salt-okunur `BTS_READ_ONLY` düşüşü) ve `page1[19] > 2` (`SQLITE_NOTADB`) kuralları (`sqlite/src/btree.c:3356-3361`) işletilmelidir.
  - Bu iki aşamalı geçiş tanımlanmadığında, C SQLite'ın temiz kapattığı bir WAL veritabanını açan `sqliters` `DatabaseHeader::parse` kontrolünden geçse bile `JournalMode::Delete` kipinde kalır; `PRAGMA journal_mode` `"delete"` döner ve ilk yazma işlemi WAL veritabanı üzerinde rollback `-journal` oluşturur.
- **Öneri**:
  - Faz 1.2 Bölüm A'ya `SHARED` kilit altındaki `open_wal_if_present` (0-bayt `.db` için yetim `-wal` silme dahil) ve Sayfa 1 okuma sonrasındaki `read_version == 2 -> open_wal + Sayfa 1 yeniden okuma` geçişi (`write_version > 2` salt-okunur ve `read_version > 2` `NotADatabase` kurallarıyla birlikte) açıkça eklenmelidir.

---

## 3. [P1] Faz 2, Faz 6 ve Faz 7: Çoklu veritabanı super-journal (`vdbeCommit`) karar kuralı C SQLite 3.54.1 ile çelişiyor — `TEMP`, `PrivateTemporary` (`""`), `synchronous=OFF` ve `journal_mode=MEMORY/OFF` katılımcıları ile `vtab_sync -> nTrans -> commit_hook` sırası hatalı veya eksik

- **Plan referansı**: `Faz 1.1` (satır `328–330` `DatabaseKind::PrivateTemporary`), `Faz 2` (satır `553–568` Durum A / Durum B), `Faz 6` (satır `692` `commit_hook`), `Faz 7` (satır `792–794` `vtab_sync`).
- **Sorun**:
  1. **`main` `PrivateTemporary` (`Connection::open("")`) durumu (`sqlite/src/vdbeaux.c:3008-3014`)**: `open("")` ile açılan `main` `:memory:` değildir (`isMemdb == 0`), ancak dosya yolu boştur (`sqlite3BtreeGetFilename == ""`). Planın satır `555`'teki *"ve `main` `:memory:` değil"* koşulu `open("")` + bağlı disk DB yazımında boş ana ad üzerinden hatalı `-mj...` dosyası üretmeye çalışır; C `vdbeCommit` ise `0 == sqlite3Strlen30(zMainFile)` ile hem `:memory:` hem `PrivateTemporary` `main` için super-journal'ı devre dışı bırakır.
  2. **`TEMP` (`iDb=1`) ve `ATTACH '' AS aux` hariç tutma (`sqlite/src/vdbeaux.c:2985, 3105-3108`)**: Disk destekli `TEMP` ve `ATTACH ''` veritabanları rollback-journal kullanır ama adlandırılmış journal yolları yoktur (`sqlite3BtreeGetJournalname == NULL`). Yalnız `main` + `temp` (veya `main` + `ATTACH ''`) yazıldığında `nTrans == 1 <= 1` olduğundan super-journal **oluşturulmaz**; 2+ adlandırılmış disk DB yanında yazıldıklarında ise super-journal içerik listesine eklenmezler.
  3. **Karışık WAL / `MEMORY` / `OFF` / `synchronous=OFF` matrisi (`sqlite/src/vdbeaux.c:2969-2987`)**: Planın satır `564–566`'daki *"Herhangi bir yazılan veritabanı WAL kipinde ... ise master-journal oluşturulmaz"* ifadesi C 3.54.1 ile çelişmektedir. C `vdbeCommit`, `nTrans` sayacına `i != 1 && !isMemdb && zJournalName != NULL && safety_level != PAGER_SYNCHRONOUS_OFF && journal_mode in {DELETE, PERSIST, TRUNCATE}` olan yazılan DB'leri dahil eder: örneğin adlandırılmış `main` (`DELETE`) + `aux1` (`DELETE`) + `aux2` (`WAL` veya `MEMORY`) birlikte yazıldığında `nTrans == 2 > 1` olduğu için `main` ve `aux1` adına **super-journal oluşturulur**; tersine `main` + `aux1` ikilisi `synchronous=OFF` veya biri `journal_mode=MEMORY` ise `nTrans <= 1` olduğundan **oluşturulmaz**.
  4. **Commit öncesi kanca sıralaması (`sqlite/src/vdbeaux.c:2950-3002`)**: Sanal tablo `xSync` (`sqlite3VtabSync`) yeni bir bağlı veritabanında yazma işlemi başlatabileceğinden **mutlaka `nTrans` sayımından, `EXCLUSIVE` kilit yükseltmesinden ve `commit_hook` çağrısından önce** çalışmalıdır.
- **Öneri**:
  - Faz 2 `2PC Commit` bölümündeki Durum A / Durum B kuralları C `vdbeCommit` aday filtresiyle (`i != 1`, adlandırılmış disk DB, `zMainFile != ""`, `synchronous != OFF`, `journal_mode in {DELETE, PERSIST, TRUNCATE}` olan yazılan DB sayısı `nTrans > 1`) birebir hizalanmalı ve `deferred_fk == 0 -> commit_hook -> vtab_sync -> nTrans/super-journal` yürütme sırası netleştirilmelidir (`vdbe.c` içinde `commit_hook` `vdbeCommit` çağrısından hemen önce çalışır, `vdbeCommit` başında ise `sqlite3VtabSync` çalışır).

---

## 4. [P1] Faz 2, Faz 3 ve Faz 5: Mevcut `sqliters-syntax` ayrıştırıcısı 12 DDL/DML/sorgu noktasında `schema.` önekini ve `INSERT ... AS alias` takma adını çöpe atıyor; `main.table.col` ifadesinde `"main"` şemasını siliyor ve `sqlite_schema.sql` metnini normalize etmiyor

- **Plan referansı**: `Faz 2` (satır `530–534, 545, 550`), `Faz 3` (satır `610`), `Faz 5` (satır `655`).
- **Sorun**:
  1. **12 noktada `_schema` kaybı ve `TEMP` CTAS engeli**: Faz 2 `[MODIFY]` listesinde `crates/sqliters-syntax` yer almamaktadır. Oysa `crates/sqliters-syntax/src/parser.rs` içindeki `parse_qualified_name()` çağrıları (`satır 319` `ANALYZE`, `329` `REINDEX`, `356` `CREATE TABLE` / `CTAS`, `431` `CREATE INDEX`, `883` `DROP TABLE`, `892` `DROP INDEX`, `903` `ALTER TABLE`, `946` `INSERT`, `1063` `UPDATE`, `1101` `DELETE`, `1498` `FROM table`, `1752` `expr IN schema.table`) `let (_schema, ...)` ile şema adını çöpe atmakta; `crates/sqliters-syntax/src/ast.rs` içindeki karşılık gelen AST düğümlerinin hiçbirinde `schema: Option<String>` alanı bulunmamaktadır. Ayrıca `parser.rs:358-363` `CREATE TEMP TABLE ... AS SELECT` deyimini sabit hatayla reddetmektedir.
  2. **`main.table.col` silinmesi ve `SELECT schema.table.*` eksikliği**: `parser.rs:2096-2104` üç parçalı sütun referansında `if first.eq_ignore_ascii_case("main") { second } else { format!("{first}.{second}") }` yaptığı için, `TEMP` tablosunun `main` tablosunu gölgelediği Faz 2 senaryolarında (`SELECT main.t.x FROM main.t, temp.t`) açık `"main"` şeması kaybolur; `"first.second"` birleştirmesi de içinde nokta bulunan tırnaklı tablo/alias adlarıyla (`"a.b".x`) çakışır. Ayrıca `parser.rs:1372-1380` `SELECT aux.t.*` biçimini desteklememektedir (`ResultColumn::TableStar` şema taşımaz).
  3. **`sqlite_schema.sql` normalizasyonu (`sqlite/src/build.c:2970-2976`, `sqlite/src/vtab.c:475-478`)**: `parser.rs:410, 449` `raw_sql` içine `CREATE TEMP TABLE IF NOT EXISTS aux.t(...)` metnini olduğu gibi kopyalar; oysa C SQLite katalog `sql` sütununa yazarken `TEMP`/`TEMPORARY`, `IF NOT EXISTS` ve `<schema>.` önekini çıkararak `"CREATE TABLE t(...)"` / `"CREATE INDEX idx ON t(...)"` saklar. Aksi halde dosya daha sonra `main` veya farklı bir `ATTACH` adıyla açıldığında `Schema::load` ve `sqlite_schema.sql` sorguları bozulur.
  4. **Faz 3 `UPSERT` / `RETURNING` için `INSERT INTO t AS alias` kaybı (`parser.rs:947-949`)**: `if self.eat(TokenKind::As) { let _ = self.parse_ident()?; }` hedef tablo takma adını yutar (`InsertStmt.alias` yoktur); oysa `ON CONFLICT DO UPDATE SET c = alias.c + excluded.c` (`upsert*.test`) ve `RETURNING alias.*` buna bağımlıdır.
- **Öneri**:
  - Faz 2 `[MODIFY]` listesine `crates/sqliters-syntax/src/{ast.rs,parser.rs}` eklenerek tüm DDL/DML/sorgu AST düğümlerine, `Expr::Column` ve `ResultColumn::TableStar` yapılarına ayrı `schema: Option<String>` alanı eklenmeli; `TEMP` CTAS kilidi kaldırılmalı ve `sqlite_schema.sql` metin normalizasyonu tanımlanmalıdır.
  - Faz 3 `[MODIFY]` listesine `crates/sqliters-syntax/src/{ast.rs,parser.rs}` eklenerek `InsertStmt.alias: Option<String>` korunmalıdır.

---

## 5. [P1] Faz 5, Faz 6 ve Faz 7: Çalışma zamanı değer alt-türü (`Value` subtype / `JSON_SUBTYPE = 74`, `SQLITE_SUBTYPE`, `SQLITE_RESULT_SUBTYPE` ve `subtype()`) mimaride hiç tanımlanmamış; iç içe `JSON`/`->`/`->>`, pencere `json_group_*` ve UDF alt-tür testleri yanlış sonuç üretir

- **Plan referansı**: `Faz 2 çapraz-faz hizmetleri` (satır `584, 591`), `Faz 5` (satır `654`), `Faz 6` (satır `681`), `Faz 7` (satır `776, 802`).
- **Sorun**:
  - `crates/sqliters-types/src/value.rs:212` içindeki `Value` enum'u yalnızca 5 depolama sınıfını (`Null`, `Integer`, `Real`, `Text`, `Blob`) taşımakta; ne `sqliters-types`, ne `VdbeVm` yazmaçları (`Mem`), ne `ExecutionServices` UDF sözleşmesi, ne de `implementation_plan.md` içinde değer alt-türü (`subtype: Option<u8>` / `u32`) kavramı yer almaktadır.
  - Oysa C SQLite 3.54.1 (`sqlite/src/json.c:322, 826, 3610, 3972..5889`), bir `TEXT` değerinin ham metin mi yoksa doğrulanmış JSON mu olduğunu ayırt etmek için `JSON_SUBTYPE = 74` (`'J'`) kullanır: `json_object('k', '{"a":1}')` düz metin kaçışlı `'{"k":"{\"a\":1}"}'` üretirken, `json_object('k', json('{"a":1}'))` ve `json_object('k', doc -> '$.sub')` iç içe JSON `'{"k":{"a":1}}'` üretir (`->` JSON düğümlerinde `JSON_SUBTYPE` atar, `->>` ise hiçbir zaman alt-tür atamaz).
  - Ayrıca derleyici ve VM katmanında: (i) B-tree veya sorter kaydına serileştirme (`OP_MakeRecord`) alt-türü sıfırlarken, `SQLITE_SUBTYPE` bayraklı pencere fonksiyonlarında (`sqlite/src/window.c:1047`, ör. `json_group_array(json(x)) OVER (...)`) argümanın alt-tür kaybetmemesi için özel aktarım uygulanır ve alt-sorgu düzleştirmede `sqlite3ExprCanReturnSubtype` (`sqlite/src/expr.c:4817-4855`) dikkate alınır; (ii) Faz 6 yerleşik `subtype(X)` fonksiyonu (`sqlite/src/func.c:110`) ile kullanıcı tanımlı fonksiyonlarda `SQLITE_SUBTYPE` / `SQLITE_RESULT_SUBTYPE` bayrak denetimleri (`sqlite/src/vdbeapi.c:216, 561-574`, `func*.test` ve `json*.test`) zorunludur.
- **Öneri**:
  - `VdbeVm` yazmaçlarına / çalışma zamanı değer taşıyıcısına ve Faz 6 UDF bağlamına geçici (ephemeral, B-tree kaydına yazılmayan) `subtype: u8` desteği, `FunctionFlags::{SUBTYPE, RESULT_SUBTYPE}` bayrakları ve yerleşik `subtype(X)` fonksiyonu eklenmeli; Faz 5 pencere çerçevesi ve alt-sorgu düzleştirme kuralları ile Faz 7 `json*` / `->` / `->>` semantiği bu sözleşmeye bağlanmalıdır.

---

## 6. [P1] Faz 7: Sanal tabloların (`CREATE VIRTUAL TABLE`) katalogdan yüklenmesi (`Schema::load`) mevcut kodda `Corrupt` hatası verir; `xConnect` çağrısının `Schema::load` dışında tembel (lazy) yürütülmesi, güvenlik bayrakları (`INNOCUOUS` / `DIRECTONLY`) ve `VTabIndexInfo` (`vtab_in` / `LIMIT` / `OFFSET` / `WITHOUT ROWID`) sözleşmesi eksiktir

- **Plan referansı**: `Faz 7 önkoşulu` (satır `734–751`), `Faz 7` (satır `772–805`).
- **Sorun**:
  1. **`Schema::load` çökmesi ve şema yükleme sırasında `xConnect` yasağı**:
     - `crates/sqliters-core/src/schema.rs:426-441` içinde `Schema::load(&mut Btree)`, `type == "table"` olan her katalog satırını `Stmt::CreateTable` olarak ayrıştırmaya zorlar; `CREATE VIRTUAL TABLE` satırında (`type='table', rootpage=0`) doğrudan `ErrorCode::Corrupt` döndürerek veritabanının yeniden açılışını kırar.
     - Üstelik C SQLite `sqlite/src/vtab.c:465, 861-866` (`assert( db->init.busy == 0 )`) uyarınca `Schema::load` sırasında **`xConnect` / `declare_vtab` çağrılamaz**: (a) sanal tablo modülü henüz yeni bağlantıya kaydedilmeden veritabanını açıp normal tabloları sorgulamak geçerlidir (`vtab*.test`), (b) `FTS5` ve `R-Tree` modülleri `xConnect` içinde gölge tablolarını (`_config`, `_node`) okumak için `VTabHostContext` ve tam yüklenmiş `Schema` gerektirir; yalnızca `&mut Btree` alan `Schema::load` içinde bu yapılamaz.
  2. **`SQLITE_VTAB_DIRECTONLY` vs `SQLITE_VTAB_INNOCUOUS` güvenlik bayrakları**:
     - Faz 7 (satır `802–804`) hem `json_each`/`json_tree` (`sqlite/src/json.c:5321`), `fts5` (`sqlite/ext/fts5/fts5_main.c:449`), `rtree` (`sqlite/ext/rtree/rtree.c:3707`) hem de **`dbstat`** (`sqlite/src/dbstat.c:179`) içerir. `dbstat` `SQLITE_VTAB_DIRECTONLY` bayraklıdır (`VIEW` ve `TRIGGER` içinden kullanımı her zaman yasaktır, `sqlite/src/delete.c:66-86`), diğer üçü ise `SQLITE_VTAB_INNOCUOUS` bayraklıdır (`PRAGMA trusted_schema = OFF` altında bile `VIEW` ve `TRIGGER` içinden çalışır). `vtab_api.rs` tasarımında bu ayrım yer almamaktadır.
  3. **`VTabIndexInfo` (`IN (...)`, `LIMIT`/`OFFSET`) ve `WITHOUT ROWID` sanal tablolar**:
     - `vtab*`, `fts5*` ve `rtree*` testleri `VTabIndexInfo` üzerinde `SQLITE_INDEX_CONSTRAINT_LIMIT` (`73`) / `OFFSET` (`74`) (`sqlite/src/whereexpr.c:1649-1775`), `vtab_in` çoklu-değer iterasyonu (`sqlite/src/where.c:4577`, `sqlite/src/vdbeapi.c:1154-1162`) ve `declare_vtab("CREATE TABLE x(...) WITHOUT ROWID")` (`sqlite/src/vtab.c:873-895`) yeteneklerini sınar.
- **Öneri**:
  - `Schema::load` içinde `CREATE VIRTUAL TABLE` katalog satırları bağlantısız sanal tablo tanımı (`module_name`, ham argümanlar, `connected_instance: None`) olarak saklanmalı; `xConnect` + `declare_vtab` ilk tablo çözümlemesinde `VTabHostContext` ile tembel olarak yürütülmelidir.
  - `vtab_api.rs` sözleşmesine `VTabRiskFlags` (`Normal`, `Innocuous`, `DirectOnly`), `declare_vtab` için `WITHOUT ROWID` desteği ve `VTabIndexInfo` için `LIMIT`/`OFFSET` ile `IN (...)` kısıt yetenekleri eklenmelidir.

---

## 7. [P2] Faz 5: `ANALYZE` ve `sqlite_stat1` sistem tablosunun yaşam döngüsü (`is_reserved_sqlite_name` istisnaları, `ANALYZE sqlite_schema` bellek-içi yeniden yüklemesi, `DROP`/`RENAME` senkronizasyonu ve `VACUUM` korunumu) mevcut DDL/şema kurallarıyla çakışıyor

- **Plan referansı**: `Faz 1.3` (satır `515, 522`), `Faz 5` (satır `655`), `Doğrulama Planı` (satır `906` `analyze*.rs`).
- **Sorun**:
  - Mevcut `crates/sqliters-core/src/engine.rs:7355-7365` içindeki `is_reserved_sqlite_name` ve DDL/DML kısıtları `sqlite_` önekli tabloları tek tip ele alırken, C SQLite `sqlite/src/analyze.c:20, 41-66, 228-250, 1380-1390`, `sqlite/src/delete.c:849`, `sqlite/src/alter.c` ve `sqlite/src/vacuum.c:299-336` içinde `sqlite_stat1(tbl,idx,stat)` tablosu `analyze*.test` ve `where*.test` kapılarının doğrudan sınadığı özel kurallara sahiptir:
    1. Kullanıcı SQL'inden doğrudan `CREATE TABLE sqlite_stat1` veya `ALTER TABLE sqlite_stat1` yasak (`object name reserved for internal use`) kalırken, `ANALYZE` tabloyu içsel olarak oluşturur; ancak **kullanıcı SQL'i mevcut `sqlite_stat1` üzerinde `SELECT`, `INSERT`, `UPDATE`, `DELETE` ve `DROP TABLE sqlite_stat1` çalıştırabilir**.
    2. Testler ve uygulamalar `sqlite_stat1` tablosuna elle istatistik yazıp `ANALYZE sqlite_schema` / `ANALYZE sqlite_master` (veya `ANALYZE <schema>.sqlite_master`) çalıştırdığında tablolar yeniden taranmaz; yalnızca `sqlite_stat1` içeriği bellek içi planlayıcı istatistiklerine yeniden yüklenir (`OP_LoadAnalysis`, ` unordered` / ` noskipscan` sonekleri ve `idx IS NULL` tablo satır tahmini dahil) ve hazırlanmış planlar yenilenir.
    3. `DROP TABLE`, `DROP INDEX` ve `ALTER TABLE RENAME TO` işlemleri `sqlite_stat1` içindeki eşleşen `tbl`/`idx` kayıtlarını silmeli veya yeniden adlandırmalı; `VACUUM` ise (yetim `sqlite_sequence`'in aksine) `sqlite_stat1` tablosunu ve içeriğini korumalıdır.
- **Öneri**:
  - Faz 5 `ANALYZE` maddesine `sqlite_stat1` için yazılabilir/düşürülebilir sistem tablosu istisnası, `ANALYZE sqlite_schema` (`OP_LoadAnalysis`) yeniden yükleme yolu ve `DROP TABLE` / `DROP INDEX` / `ALTER TABLE RENAME` / `VACUUM` istatistik senkronizasyonu açıkça eklenmelidir.
