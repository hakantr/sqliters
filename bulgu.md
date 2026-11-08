# `implementation_plan.md` İnceleme Bulguları (v14 — Commit `d96ac557`)

**İncelenen commit**: `d96ac557c2d747073276d2f3ef857ef387500068` (`docs: resolve v13 review findings in plan v14`)
**Kapsam**: `implementation_plan.md` dosyasının tamamı (`Faz 1.1` → `Faz 7` ve `Doğrulama Planı`), C SQLite 3.54.1 referans kaynakları (`sqlite/src/*`, `sqlite/ext/*`, `sqlite/test/*`) ve mevcut `sqliters` Rust çalışma alanı (`crates/*`, `tests/*`) ile çapraz doğrulanmıştır.

Önceki turdaki (`v13` / `f114b366`) 7 bulgunun tamamının `v14` planında giderildiği doğrulanmıştır. Güncel `v14` planında tespit edilen yeni mimari tutarsızlıklar ve eksik önkoşullar aşağıdadır:

---

## 1. [P1] Faz 5, Faz 6 ve Faz 7: `crates/sqliters-syntax` (`token.rs`, `ast.rs`, `parser.rs`) bu fazlarda `[MODIFY]` kapsamına alınmamış; `OVER`/`WINDOW`, `CREATE VIRTUAL TABLE`, tablo-değerli fonksiyonlar (`FROM json_each(...)`), `->`/`->>` ve `MATCH`/`REGEXP` sözdizimi hatasıyla reddedilir

- **Plan referansı**: `Faz 5` (satır `616–620`), `Faz 6` (satır `649`), `Faz 7` (satır `719, 742–766`), `Manuel Smoke Testleri` (satır `904–905`).
- **Sorun**:
  1. **Faz 5 (`WINDOW` / `OVER`)**: Faz 5 `[MODIFY]` listesinde (satır `618`) `crates/sqliters-syntax` küfesi hiç yer almıyor. Oysa mevcut sözdizimi katmanında:
     - `crates/sqliters-syntax/src/token.rs:7-186` içinde pencere çerçevesi anahtar sözcükleri (`CURRENT`, `EXCLUDE`, `FOLLOWING`, `GROUPS`, `OTHERS`, `PRECEDING`, `TIES`, `UNBOUNDED`) tanımlı değildir;
     - `crates/sqliters-syntax/src/ast.rs:295-302` (`SelectCore::Select`) `WINDOW w AS (...)` yan tümcesini, `ast.rs:489-494` (`Expr::FunctionCall`) ise `OVER (...)` pencere belirtimini taşımaz;
     - `crates/sqliters-syntax/src/parser.rs:1357, 2083` ne `OVER` ne de `WINDOW` yapılarını ayrıştırır. Planın kendi satır `904` smoke testi (`rank() OVER (ORDER BY fiyat DESC)`) doğrudan `near "(": syntax error` üretir.
  2. **Faz 7 (`CREATE VIRTUAL TABLE`, tablo-değerli fonksiyonlar, `->`/`->>`, `MATCH`/`REGEXP`)**: Faz 7 `[MODIFY]` listelerinde (satır `719, 758`) de `crates/sqliters-syntax` yoktur:
     - `parser.rs:343-421` içinde `CREATE VIRTUAL TABLE [IF NOT EXISTS] [schema.]name USING module[(...)]` kolu ve `ast.rs` içinde `Stmt::CreateVirtualTable` bulunmamaktadır. Üstelik C SQLite `sqlite/src/parse.y` ve `sqlite/src/vtab.c` (`addModuleArgument`) uyarınca `USING module(...)` içindeki argümanlar standart `ColumnDef` veya `Expr` **değildir**; dengeli parantez ve üst-düzey virgüllerle ayrılmış **ham metin dilimleri** olarak `xCreate`/`xConnect` metoduna iletilir (ör. FTS5 `b UNINDEXED, tokenize = 'porter unicode61'` ve R-Tree `+aux1 TEXT` normal SQL sütun/ifade ayrıştırıcısıyla ayrıştırılamaz).
     - `ast.rs:320-330` (`TableOrSubquery`) ve `parser.rs:1479-1506` (`parse_table_or_subquery`), `FROM` yan tümcesinde argümanlı tablo-değerli fonksiyon çağrısını (`TableOrSubquery::TableFunction { schema, name, args, alias }`) desteklemez; `SELECT * FROM json_each(...)`, `SELECT * FROM t, json_tree(t.meta)` (korele/lateral join), `SELECT * FROM generate_series(1, 10)`, `SELECT * FROM pragma_table_info('t')` ve `SELECT * FROM ft('arama')` çağrıları `near "(": syntax error` ile kırılır.
     - `token.rs:32-33, 125, 148` `Ptr` (`->`), `PtrText` (`->>`), `Match` ve `Regexp` token'larını üretmesine rağmen `parser.rs:1690-1804` ve `1826-1848` (`infix_binding_power`) bunların hiçbirini tüketmez; satır `905` smoke testi (`meta ->> '$.etiket'`), FTS5 `WHERE ft MATCH '...'` ve Faz 6 `WHERE x REGEXP '...'` ifadeleri ayrıştırma aşamasında reddedilir.
- **Öneri**:
  - Faz 5 `[MODIFY]` listesine `crates/sqliters-syntax/src/{token.rs,ast.rs,parser.rs}` eklenerek `WindowSpec`, `WindowFrame` (`ROWS`/`RANGE`/`GROUPS`, sınırlar ve `EXCLUDE`), `SelectCore::Select.windows` ve `Expr::FunctionCall.over` tanımlanmalıdır.
  - Faz 6/7 `[MODIFY]` listesine yine `crates/sqliters-syntax/src/{ast.rs,parser.rs}` eklenerek: (i) ham modül argümanı dilimlerini koruyan `Stmt::CreateVirtualTable`, (ii) korele ifadeleri de taşıyabilen `TableOrSubquery::TableFunction`, (iii) C SQLite öncelik sırasıyla `->`, `->>`, `MATCH` ve `REGEXP` ikili operatörleri tanımlanmalıdır.

---

## 2. [P1] Faz 1.2: WAL protokolünün kontrol yüzeyi (`PRAGMA wal_checkpoint`, `wal_autocheckpoint`, `locking_mode`), `Connection`/`Btree` bağlantısı ve WAL içi `SAVEPOINT` kısmi geri sarma (`sqlite3WalSavepoint` / `sqlite3WalSavepointUndo`) Faz 1.2 kapsamından kopuk

- **Plan referansı**: `Faz 1.2` (satır `332–459`, özellikle satır `332` `[MODIFY]`, satır `348–355` yazma/commit/undo, satır `387` `locking_mode=EXCLUSIVE`, satır `390–448` checkpoint).
- **Sorun**:
  1. **`sqliters-core` ve `sqliters-btree` entegrasyonunun Faz 1.2 `[MODIFY]` listesinde olmaması**: Satır `332` yalnızca `crates/sqliters-pager/src/header.rs`, `crates/sqliters-pager/src/lib.rs` ve `[NEW] crates/sqliters-pager/src/wal.rs` dosyalarını listeliyor; `crates/sqliters-btree/src/lib.rs`, `crates/sqliters-core/src/lib.rs` ve `crates/sqliters-core/src/engine.rs` Faz 1.2 kapsamında yer almıyor. Mevcut `crates/sqliters-core/src/engine.rs:8947-9362` PRAGMA işleyicisinde ise:
     - `PRAGMA [schema.]wal_checkpoint[(PASSIVE|FULL|RESTART|TRUNCATE|NOOP)]` (`sqlite/src/pragma.c:2398-2415`, 3 sütunlu `(busy, log, checkpointed)` çıktı satırı),
     - `PRAGMA wal_autocheckpoint [= N]` (`sqlite/src/pragma.c:2426-2434`, varsayılan `1000` frame eşiğinde commit sonrası otomatik `PASSIVE` checkpoint),
     - `PRAGMA [schema.]locking_mode [= NORMAL|EXCLUSIVE]` (`sqlite/src/pragma.c:684-750`)
     hiç mevcut değildir. Bu yüzey olmadan satır `387`'deki `locking_mode=EXCLUSIVE` ve satır `405`'teki `PRAGMA wal_checkpoint(NOOP)` gereksinimleri SQL katmanından çalıştırılamaz; `ActivityTracker` / `Connection` seviyesinde WAL okuma snapshot yuvası ve bağlantı kapanışındaki son okuyucu checkpoint/temizliği de bağlanamaz.
  2. **WAL içinde `SAVEPOINT` / `ROLLBACK TO` kısmi geri sarmasının (`sqlite3WalSavepoint` / `sqlite3WalSavepointUndo`) tanımlanmaması**: Mevcut `crates/sqliters-pager/src/lib.rs:35-40, 846-928` içindeki `SavepointFrame` yalnızca bellek içi `cache` kopyasını ve `orig_page_count` değerini saklar. WAL kipinde aktif bir yazma transaction'ı içinde önbellek taşması (spill) veya çok adımlı yazma sırasında henüz commit edilmemiş frame'ler WAL dosyasına ve wal-index hash tablosuna yazıldığında, `ROLLBACK TO sp` yalnız Pager önbelleğini geri yüklerse `pWal->hdr.mxFrame`, `aFrameCksum[0..1]`, `nCkpt` log-reset sayacı ve SHM hash tablosu (`walCleanupHash`) geri alınmamış kalır (`sqlite/src/pager.c:3551, 7033` ve `sqlite/src/wal.c:3904-3947`). Sonraki commit ya iptal edilmiş ara frame'leri görünür kılar ya da checksum zincirini bozar.
- **Öneri**:
  - Faz 1.2 `[MODIFY]` listesine `crates/sqliters-btree/src/lib.rs`, `crates/sqliters-core/src/lib.rs` ve `crates/sqliters-core/src/engine.rs` eklenerek `PRAGMA wal_checkpoint`, `PRAGMA wal_autocheckpoint` (varsayılan `1000`), `PRAGMA locking_mode` ve `Connection::wal_checkpoint_v2` API/PRAGMA bağlantıları açıkça tanımlanmalıdır.
  - `SavepointFrame` yapısına C `WAL_SAVEPOINT_NDATA` (`[mxFrame, cksum0, cksum1, nCkpt]`) durumu eklenmeli; `ROLLBACK TO` sırasında `nCkpt` sarmalanma kontrolü ve `walCleanupHash` ile wal-index hash yuvalarının temizlenmesi Faz 1.2 B bölümüne yazılmalıdır.

---

## 3. [P1] Faz 1.3, Faz 2 ve Faz 6: `PRAGMA page_size = N` sonrası `VACUUM` ve farklı `page_size` ile `Backup` (`backupOnePage`) mevcut `Pager` / `exec_vacuum` mimarisinde ya no-op kalıyor ya da slice boyutu paniğine ve bozuk rollback-journal başlığına yol açıyor; ayrıca `VACUUM` yetim `sqlite_sequence` tablosunu silmiyor (`vacuum2-3.1`)

- **Plan referansı**: `Faz 1.3` (satır `467, 492–499`), `Faz 2` (satır `522`), `Faz 6` (satır `685–689` `Backup`), Test Grubu `36`, `45`, `51` (satır `813, 822, 828`).
- **Sorun**:
  1. **`next_page_size` kaybı, 1-e-1 sayfa kopyalama paniği ve journal başlığı bozulması**:
     - `crates/sqliters-pager/src/lib.rs:308-311` (`Pager::set_page_size`), veritabanı başlatıldıktan sonra (`self.page_size_fixed == true`) çağrılan `PRAGMA page_size = N` değerini `next_page_size` (`db->nextPagesize`, `sqlite/src/btree.c:2855`, `sqlite/src/vacuum.c:282`) olarak saklamak yerine tamamen yok saymaktadır (`tests/sqlite_test/vacuum6.rs:53, 79` eski sayfa boyutunda kalır).
     - `next_page_size` saklandığında ise `crates/sqliters-core/src/engine.rs:8911-8918` içindeki `exec_vacuum`, geçici `fresh` B-tree sayfalarını (`new_page_size`) hedef `ctx.btree` içine (`old_page_size`) 1-e-1 `pgno` ile `dst.copy_from_slice(&src)` üzerinden kopyaladığı için `new_page_size != old_page_size` durumunda **çalışma zamanında doğrudan `panic!` üretir**.
     - Ayrıca `crates/sqliters-pager/src/lib.rs:955` (`write_journal_header`), `tx_orig_header.page_size` yerine `self.header.page_size` yazdığından, commit tamamlanmadan önce `self.header.page_size` değiştirilirse `commit()` (satır `703`) rollback-journal başlığına yeni sayfa boyutunu yazar; journal içindeki ön-görüntü kayıtları eski boyutta olduğu için çökme kurtarması (`recover_hot_journal_under_shared_lock`) veritabanını bozar.
     - C SQLite `sqlite/src/backup.c:237-290, 405-409, 747-795` (`backupOnePage` + `sqlite3BtreeCopyFile`) ve `sqlite/src/vacuum.c:274-283, 378-395`: (i) hedef rollback transaction'ı boyunca hedefin **orijinal `page_size`** değerini koruyarak kaynak sayfaları bayt ofseti (`iOff`) üzerinden hedef sayfalara böler veya birleştirir (her iki boyutta da `PENDING_BYTE_PAGE` atlanarak), hedefin `page_size` ve önbellek sıfırlamasını ancak commit ve journal silme tamamlandıktan sonra uygular; (ii) kaynak WAL kipindeyse yerinde `VACUUM` için `nextPagesize` sıfırlanırken `VACUUM INTO` yeni sayfa boyutunu uygulayabilir (`vacuum.c:274-279`); (iii) Faz 6 `Backup` API'sinde ise hedef `WAL` veya `:memory:` iken `pgszSrc != pgszDest` durumunda `SQLITE_READONLY` döndürülür (`backup.c:405-409`).
  2. **`VACUUM` sırasında yetim `sqlite_sequence` tablosunun korunması (`vacuum2-3.1`)**:
     - C SQLite `sqlite/src/vacuum.c:299-302` tablo şemalarını `WHERE type='table' AND name<>'sqlite_sequence'` ile kopyalar; böylece tüm `AUTOINCREMENT` tabloları düşürülmüşse (`tests/sqlite_test/vacuum2.rs:19-21`) boş `sqlite_sequence` tablosu yeni veritabanında yeniden oluşturulmaz. Mevcut `engine.rs:8858` `sqlite_sequence` tablosunu koşulsuz kopyaladığı için `vacuum2-3.1` (`vacuum2.rs:45-46`, beklenen 3 sayfa yerine 4 sayfa) başarısız olur.
- **Öneri**:
  - `Pager` içine `next_page_size: Option<u32>` alanı ve bayt-ofset tabanlı `copy_pages_from` (`backupOnePage` / `sqlite3BtreeCopyFile` eşdeğeri) eklenmeli; rollback-journal başlığı her zaman `tx_orig_header.page_size` yazmalı ve `header.page_size` / önbellek yeniden boyutlandırması commit sonrasına bırakılmalıdır.
  - Faz 1.3 `VACUUM` kurallarına WAL yerinde `VACUUM` vs `VACUUM INTO` `next_page_size` ayrımı ile yetim `sqlite_sequence` tablosunun atlanması (`vacuum2-3.1`), Faz 6 `Backup` bölümüne ise farklı sayfa boyutlarında rollback hedefi bayt-ofset kopyalaması ve `WAL`/`:memory:` hedefinde `SQLITE_READONLY` kuralı eklenmelidir.

---

## 4. [P1] Faz 1.1, Faz 2, Faz 3, Faz 4 ve Faz 6: `Connection::prepare` derleme zamanı şema/isim/authorizer doğrulaması yapmıyor ve `Statement` `SQLITE_SCHEMA` otomatik yeniden-hazırlama (`sqlite3Reprepare` / ham `zSql`) mekanizmasından yoksun

- **Plan referansı**: `Faz 1.1` (satır `227–229` `cmd_prepare`), `Faz 1.3` (satır `466`), `Faz 2` (satır `526, 569`), `Faz 6` (satır `647, 661–662`).
- **Sorun**:
  1. **Prepare aşamasında eksik semantik doğrulama ve `set_authorizer` boşluğu**:
     - Mevcut `crates/sqliters-core/src/lib.rs:240-244, 378-415` içinde `Connection::prepare`, yalnızca `compiler::try_compile_select` tarafından derlenebilen basit `SELECT` sorgularında tablo/sütun çözümlemesi yapar. Tüm `INSERT`, `UPDATE`, `DELETE`, DDL ifadeleri ile `JOIN`, `GROUP BY`, `WITHOUT ROWID`, `GENERATED`, `CTE` veya `VIEW` içeren `SELECT` sorgularında `prepare()` **hiçbir tablo, sütun, fonksiyon veya yetki doğrulaması yapmadan `Ok(Statement)` döndürmektedir**.
     - Faz 1.1 satır `227` uyarınca `crates/sqliters-tcltest/src/harness.rs` içindeki `cmd_prepare` erken `step()` döngüsünden çıkarılıp yalnızca `conn.prepare()` çağıracak şekilde düzeltildiğinde, C SQLite'ta `sqlite3_prepare` / `sqlite3_prepare_v2` sırasında dönmesi gereken `"no such table"`, `"no such column"`, `"no such function"` ve Faz 6 `set_authorizer` (satır `569, 647`) `SQLITE_DENY` (`SQLITE_AUTH`) hataları `prepare` aşamasında üretilemez (`harness.rs:1575` `stepsql` ve `tests/sqlite_test/pragma4.rs:10-59` kolon sayısı/prepare testleri doğrudan etkilenir; ayrıca `harness.rs:976` `args.len() < 4` denetimi `tests/sqlite_test/fkey7.rs:48` içindeki 3 argümanlı `sqlite3_prepare_v2 db sql -1` çağrısını reddeder).
  2. **Eksik `zSql` saklama ve `SQLITE_SCHEMA` otomatik yeniden-hazırlama (`sqlite3Reprepare`)**:
     - `Statement` (`crates/sqliters-core/src/lib.rs:364-375`) ham SQL metnini (`zSql`) saklamayıp yalnızca ayrıştırılmış `Stmt` ağacını tutar; ayrıca yorumlayıcı yolunda (`compiled_vdbe == None`, `lib.rs:567-575`) ne `schema_cookie` değişimi ne de `expired` bayrağı kontrol edilir.
     - Oysa Faz 1.3 (`rootpage` taşıması), Faz 2 (`ATTACH`/`DETACH` veya aynı isimli `TEMP` tablo oluşturulmasıyla `SELECT * FROM t` kolon sayısının ve bağlarının değişmesi), Faz 3/4 (`ALTER TABLE RENAME` / `DROP COLUMN`) ve Faz 6 (`set_authorizer` `READ -> IGNORE` sütun maskelemesi, UDF/collation kaydı sonrası advisory expire, satır `661–662`) gerçekleştiğinde `prepare_v2` semantiğindeki ifadelerin saklanan `zSql` üzerinden yeniden derlenmesi (`sqlite/src/vdbeapi.c:615-640`), eski v1 `sqlite3_prepare` semantiğindeki ifadelerin ise `SQLITE_SCHEMA` döndürmesi gerekir.
- **Öneri**:
  - `Connection::prepare` (varsayılan v2 semantiği) ve harness için gereken legacy `prepare_v1` ayrımı tanımlanmalı; her iki yürütme yolunda da (`VdbeVm` ve yorumlayıcı) `prepare` aşamasında ortak isim/şema/fonksiyon çözümlemesi, sonuç sütunu metadata çıkarımı ve `set_authorizer` kontrolü çalıştırılmalıdır (`harness.rs:976` 3 argümanlı `sqlite3_prepare_v2` çağrısını da kabul etmelidir).
  - `Statement` içinde ham `zSql` ve hazırlama anındaki şema nesli (`schema_generation` + advisory `expired` bayrağı) saklanmalı; `step()` başlangıcında nesil uyuşmazlığı varsa v2 ifadelerinde bağlı parametreler korunarak `reprepare` (sınırlı deneme sayısıyla), v1 ifadelerinde ise `SQLITE_SCHEMA` uygulanmalıdır.

---

## 5. [P1] Faz 3, Faz 4 ve Faz 6: Dolaylı satır değiştirme yolları (`FOREIGN KEY` `CASCADE`/`SET NULL`/`SET DEFAULT`, `OR REPLACE` çakışma silmesi ve `UPSERT DO UPDATE`) mevcut `engine.rs` içinde B-tree'yi doğrudan değiştirerek `TRIGGER`, `update_hook` ve `Blob` geçersizleştirme zincirini baypas ediyor

- **Plan referansı**: `Faz 3` (satır `582` `UPSERT`), `Faz 4` (satır `69`, `596–610` `TRIGGER` ve `fkey3` kapısı), `Faz 6` (satır `648` `update_hook`, satır `692–695` `Blob` expiry).
- **Sorun**:
  - Mevcut `crates/sqliters-core/src/engine.rs` içinde dolaylı satır silme ve güncelleme yolları merkezi bir DML katmanı yerine doğrudan düşük seviyeli B-tree yardımcılarını (`remove_row_and_indexes`, `validate_and_write_row`) çağırmaktadır:
    1. **FK eylemleri (`engine.rs:6529-6630, 6693-6805`)**: `FkAction::Cascade`, `FkAction::SetNull` ve `FkAction::SetDefault`, çocuk tablodaki satırları doğrudan B-tree üzerinde siler veya yazar. Oysa C SQLite `sqlite/src/fkey.c:1419-1442` (`sqlite3FkActions` → `fkActionTrigger` → `sqlite3CodeRowTriggerDirect`), bu eylemleri çocuk tablo üzerinde içsel `DELETE`/`UPDATE` alt-programı olarak çalıştırır ve (`PRAGMA recursive_triggers` kapalı olsa dahi) çocuk tablonun `BEFORE`/`AFTER DELETE` ve `BEFORE`/`AFTER UPDATE` trigger'larını ateşler (`fkey3` ve bekleyen FK testlerinin temel gereksinimi).
    2. **`ON CONFLICT REPLACE` çakışma silmesi (`engine.rs:5863-5885, 5905-5925, 5986-6005`)**: PK veya UNIQUE çakışmasında mevcut satır doğrudan `remove_row_and_indexes` ile silinmektedir. C SQLite `sqlite/src/insert.c:2216-2238` ve `sqlite/src/update.c` ise: (i) `PRAGMA recursive_triggers = ON` olduğunda çakışan satır için `BEFORE`/`AFTER DELETE` trigger'larını çalıştırır ve bu trigger'ların yan etkileri yeni benzersizlik/FK çakışması yaratabileceği için kısıtları ikinci geçişte yeniden denetler (`addrRecheck`), (ii) silinen satıra açık `Blob` tanıtıcılarını geçersizleştirir (`vdbeblob.c`), buna karşılık C davranışıyla uyumlu olarak `OR REPLACE` çakışma silmesi için ayrı bir `update_hook(SQLITE_DELETE)` çağrısı üretmez.
    3. **Faz 3 `UPSERT DO UPDATE` (`sqlite/src/upsert.c:325-326`)**: `ON CONFLICT DO UPDATE` dalı C SQLite'ta `sqlite3Update(..., OE_Abort, ..., pUpsert)` üzerinden yürür; yani çakışma öncesi `BEFORE INSERT` çalışmış olsa bile çakışma yakalandığında `BEFORE UPDATE` (`OLD` = mevcut satır, `excluded.*` erişilebilir) → kısıt/FK denetimi → satır güncellemesi + `update_hook(SQLITE_UPDATE)` + `Blob` geçersizleştirme → `AFTER UPDATE` sırası izlenir (`AFTER INSERT` çalışmaz).
- **Öneri**:
  - Faz 3 ve Faz 4 planına, `FK CASCADE/SET NULL/SET DEFAULT`, `OR REPLACE` çakışma silmesi (`recursive_triggers=ON` + ikinci geçiş kısıt yeniden-denetimi) ve `UPSERT DO UPDATE` yollarının doğrudan B-tree yazımı yerine ortak satır-mutasyon / trigger / `BlobInvalidator` / `HookDispatcher` hattından geçeceği ve C SQLite `fkey.c` / `insert.c` / `upsert.c` semantiğiyle birebir eşleneceği açıkça eklenmelidir.

---

## 6. [P2] Faz 1.3 ve Faz 2: Boş dosya adı (`""`) ile `:memory:` veritabanının birleştirilmesi (`Connection::open("")`), özel geçici disk veritabanı (`ATTACH '' AS aux`) ve `DETACH` TEMP trigger temizliği semantiğini bozuyor

- **Plan referansı**: `Faz 1.3` (satır `467` `VACUUM`), `Faz 2` (satır `510–527` `DatabaseSet`, `ATTACH`/`DETACH`, `TEMP`), `Faz 4` (satır `598`).
- **Sorun**:
  1. **`""` (boş yol) ile `":memory:"` ayrımının yutulması**: Mevcut `crates/sqliters-core/src/lib.rs:124-126` (`if p == Path::new(":memory:") || p.as_os_str().is_empty() { return Self::open_in_memory(); }`), boş dosya adını (`""`) doğrudan `:memory:` ile özdeşleştiriyor. Oysa C SQLite `sqlite/src/btree.c:2578-2607` ve `sqlite/src/pager.c:4823-4838` içinde `":memory:"` saf bellek veritabanıdır (`isMemdb = 1`), `""` ise (`PRAGMA temp_store = MEMORY` seçilmedikçe `isTempDb = 1, isMemdb = 0`) kapanışta otomatik silinen **özel geçici disk veritabanıdır** (`SQLITE_OPEN_TEMP_DB | SQLITE_OPEN_DELETEONCLOSE`). `sqlite3_open("")` ve `ATTACH '' AS aux`:
     - Belleğe sığmayan veriyi geçici disk dosyasına dökebilir,
     - `:memory:` veritabanının aksine `PRAGMA aux.page_size = N` ve `VACUUM` sayfa boyutu değişimlerini destekler (`sqlite/src/vacuum.c:187, 207, 282`),
     - Her `ATTACH ''` çağrısında tamamen izole yeni bir geçici veritabanı oluşturur (`database is already attached` çakışmasına veya paylaşımlı isim tablosuna girmez).
  2. **`DETACH` sırasında bağlı `TEMP` trigger temizliği (`detachTempTriggers`)**: Faz 4 (satır `598`) başka bir bağlantının düşürdüğü tabloya ait yetim `TEMP` trigger yüklemesini ele alırken, Faz 2 `DETACH` (satır `526`) C SQLite `sqlite/src/attach.c:63-74, 352` (`detachTempTriggers`) davranışını tanımlamıyor: ayrılan (`DETACH`) bir şemadaki tabloya bağlı `TEMP` trigger'ların hedef şema bağı `DETACH` anında koparılmalıdır; aksi halde eski `DatabaseId` yuvası sonradan başka bir veritabanı için yeniden kullanıldığında `TEMP` trigger yanlış veritabanı tablosuna bağlanır.
- **Öneri**:
  - `Connection::open("")` ve Faz 2 `ATTACH '' AS <schema>` yolları `":memory:"` ile karıştırılmamalı; `temp_store` politikasına uyumlu, `OpenFlags::TEMP_DB | OpenFlags::DELETEONCLOSE` tabanlı özel geçici veritabanı olarak açılmalıdır.
  - Faz 2 `DETACH` adımlarına, ayrılan şemaya referans veren `TEMP` trigger bağlarının temizlenmesi/yetimleştirilmesi (`detachTempTriggers`) eklenmelidir.

---

## 7. [P1] Faz 1.2, Faz 1.3, Faz 2, Faz 3, Faz 4, Faz 5 ve Faz 7: Faz kapılarında "açılması (`#[ignore]` kaldırılması)" zorunlu tutulan test aileleri `tests/sqlite_test/` ve `tclsuite.rs` içinde hiç mevcut değil

- **Plan referansı**: Satır `7`, `Faz Bağımlılık Grafiği ve Kapı Kriterleri` (satır `65–72`), satır `489`, `Doğrulama Planı` (satır `848, 861`).
- **Sorun**:
  - Plan, her faz kapısında ilgili C SQLite test ailelerindeki `#[ignore]` özniteliklerinin kaldırılmasını ve `cargo test --workspace` ile yeşil geçmesini zorunlu koşmaktadır (`Faz 1.2`: `wal*`; `Faz 1.3`: `autovacuum*`; `Faz 2`: `ATTACH`/`TEMP`; `Faz 3`: `view*`, `upsert*`; `Faz 4`: `trigger*`; `Faz 5`: `window*`, `analyze*`; `Faz 7`: `vtab*`, `json*`, `fts5*`, `rtree*`).
  - Ancak `tests/sqlite_test/` dizininde, `tests/sqlite_test_suite.rs` içinde ve `crates/sqliters-tcltest/src/bin/tclsuite.rs:10-43` (`CORE_SQL_FAMILIES` / `CORE_SQL_EXACT`) listesinde toplam 165 sarmalayıcı modül bulunmasına karşın, **`wal*`, `autovacuum*`, `attach*`, `tempdb*`, `view*`, `upsert*`, `trigger*`, `window*`, `analyze*`, `vtab*`, `json*`, `fts5*`, `rtree*` ailelerine ait tek bir `.rs` test dosyası bile mevcut değildir**.
  - Buna ek olarak C SQLite kaynak ağacında `fts5` ve `rtree` Tcl testleri `sqlite/test/` altında değil `sqlite/ext/fts5/test/*.test` ve `sqlite/ext/rtree/*.test` dizinlerindedir; oysa `crates/sqliters-tcltest/src/lib.rs:75-85` (`resolve_sqlite_test_dir`) yalnızca `sqlite/test` dizinini çözümlemektedir.
- **Öneri**:
  - Her fazın dosya listesine ilgili `[NEW] tests/sqlite_test/<aile>.rs` sarmalayıcı modüllerinin üretilmesi ve `[MODIFY] tests/sqlite_test_suite.rs` + `crates/sqliters-tcltest/src/bin/tclsuite.rs` kayıtlarının güncellenmesi açıkça eklenmelidir.
  - Faz 7 için `crates/sqliters-tcltest/src/lib.rs` içine `sqlite/ext/fts5/test` ve `sqlite/ext/rtree` dizin çözümlemesi (veya çoklu test kökü desteği) dahil edilmelidir.
