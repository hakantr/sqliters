# sqliters

**SQLite 3.54.1** referans alınarak sıfır harici bağımlılıkla (*zero external dependencies*) geliştirilmiş, *rollback journal* tabanlı **SQLite Format 3** veritabanı ve SQL motoru Rust portu.

`sqliters` tarafından oluşturulan `.db` dosyaları C `sqlite3` tarafından doğrudan okunup yazılabilir (`PRAGMA integrity_check` → `ok`); C `sqlite3` tarafından *rollback journal* kipinde (`UTF-8`, `UTF-16le` veya `UTF-16be` kodlamasıyla) oluşturulan çok sayfalı B-Tree, ikincil indeks ve taşma (*overflow*) zincirli veritabanları da `sqliters` tarafından açılıp sorgulanabilir.

Çalışma alanındaki tüm üst katman crate'lerinde `#![forbid(unsafe_code)]` etkin olup, yalnızca `sqliters-vfs` katmanında işletim sistemi süreçleri arası uyumlu dosya kilitlemesi (`os_unix.c` bayt aralığı kilitleri) için izole bir POSIX `fcntl` FFI modülü bulunur.

---

## Mimari ve C → Rust Katman Eşlemesi

Workspace, SQLite'ın temel katmanlarını modüler crate'lere ayırır:

| Katman | Crate | Referans Alınan C SQLite Kaynakları (`sqlite/src/`) | Sorumluluk |
| :--- | :--- | :--- | :--- |
| **0. Türler & Kayıt** | `crates/sqliters-types` | `sqlite.h.in`, `sqliteLimit.h`, `util.c`, `vdbeaux.c`, `bitvec.c` | Hata kodları, sabitler, 1..=9 bayt big-endian `varint`, `Value`, `TypeAffinity` (§3.1), `Collation` (`BINARY`/`NOCASE`/`RTRIM`), `TextEncoding` (`UTF-8`/`UTF-16le`/`UTF-16be`), Format 4 kayıt serileştirme |
| **1. VFS** | `crates/sqliters-vfs` | `os.h`, `os_unix.c`, `memvfs.c` | `Vfs` & `VfsFile` soyutlamaları, POSIX konumsal G/Ç ve `fcntl` bayt aralığı kilitlemesi (`PENDING`/`RESERVED`/`SHARED` + süreç içi inode kilit takibi), çoklu tanıtıcı kilit denetimli `MemoryVfs`, UTC sistem saati |
| **2. Pager** | `crates/sqliters-pager` | `pager.c`, `pcache1.c` | 100 baytlık veritabanı başlığı (`SQLite format 3\0`), sayfa önbelleği, *rollback journal* (`-journal` başlığı + sağlama toplamı), `SHARED` ve `RESERVED` kilit denetimli güvenli sıcak günlük (*hot-journal*) kurtarma, iç içe `SAVEPOINT` |
| **3. B-Tree** | `crates/sqliters-btree` | `btree.h`, `btreeInt.h`, `btree.c` | Tablo B+Tree (`0x05`/`0x0d`), İndeks B+Tree (`0x02`/`0x0a`), sayfa bölünmesi (*split*) ve Sayfa 1 kök korunumu, taşma (*overflow*) sayfa zincirleri, *freelist* gövde/yaprak yönetimi, `PRAGMA integrity_check` |
| **4. Sözdizimi** | `crates/sqliters-syntax` | `tokenize.c`, `complete.c`, `parse.y`, `mkkeywordhash.c` | SQLite 3.54.1 sözcük çözümleyici (`_` sayısal ayraçlar dahil), `is_complete` DFA'sı, desteklenen SQL altkümesi için AST ve Pratt / özyinelemeli-iniş ayrıştırıcı |
| **5. VDBE** | `crates/sqliters-vdbe` | `vdbe.h`, `vdbe.c`, `vdbesort.c`, `func.c`, `date.c` | Yazmaç tabanlı sanal makine (`VdbeVm`), `EXPLAIN` baytkod sökümü, imleçler, sıralayıcı (`Sorter`), yerleşik skaler/kümeleme ve UTC tarih/saat fonksiyonları |
| **6. Çekirdek & API** | `crates/sqliters-core` | `main.c`, `prepare.c`, `build.c`, `select.c`, `insert.c`, `update.c`, `delete.c`, `fkey.c`, `alter.c`, `vacuum.c`, `pragma.c` | Sayfa 1 `sqlite_schema` kataloğu, SQL→VDBE derleyicisi, ilişkisel sorgu motoru, yabancı anahtar (*foreign key*) ve çakışma çözümü (`ABORT`/`FAIL`/`ROLLBACK`/`IGNORE`/`REPLACE`) kuralları, `Connection` / `Statement` API'si |
| **7. Kök & CLI** | `.` (`sqliters`), `crates/sqliters-cli` | `shell.c.in` | Genel kütüphane arayüzü ve etkileşimli/toplu `sqliters` komut satırı kabuğu (`.tables`, `.schema`, `.dbinfo`, `.dump`, `.mode box\|table\|csv\|json\|line\|list`) |

---

## Desteklenen Başlıca Özellikler

- **Depolama, Kodlama & Eşzamanlı Kilitleme**:
  - 512..=65536 bayt sayfa boyutları, `UTF-8`, `UTF-16le` ve `UTF-16be` metin kodlamaları (`PRAGMA encoding`)
  - POSIX `fcntl` uyumlu dosya kilitleme protokolü (`SHARED`, `RESERVED`, `PENDING`, `EXCLUSIVE`) ve aktif yazıcı işlemlerine zarar vermeyen sıcak günlük (*hot-journal*) çökme kurtarması
  - `INTEGER PRIMARY KEY` (`rowid` takma adı), `AUTOINCREMENT` (`sqlite_sequence`), indeks tabanlı `WITHOUT ROWID` tablolar, ikincil `UNIQUE` / kısmi (`WHERE ...`) B-Tree indeksler, taşma sayfaları, *freelist* geri kazanımı ve `VACUUM`
  - `BEGIN [DEFERRED | IMMEDIATE | EXCLUSIVE]`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `ROLLBACK TO`, `RELEASE`
- **Kısıtlayıcılar & Çakışma Çözümü**:
  - `PRAGMA foreign_keys = ON` ile ebeveyn/çocuk yabancı anahtar denetimi (`NO ACTION`, `RESTRICT`, `SET NULL`, `SET DEFAULT`, `CASCADE` ve `DEFERRABLE INITIALLY DEFERRED`), `PRAGMA foreign_key_list`, `PRAGMA foreign_key_check`
  - `NOT NULL`, `UNIQUE`, `PRIMARY KEY`, `CHECK` ve `STRICT` tablo tür denetimleri
  - `OR ABORT`, `OR FAIL`, `OR ROLLBACK`, `OR IGNORE` ve `OR REPLACE` çakışma politikaları
- **SQL Sorgu & Veri İşleme**:
  - `SELECT`, `DISTINCT`, `JOIN` (`INNER`, `LEFT`, `RIGHT`, `FULL OUTER`, `CROSS`, `NATURAL`, `ON`, `USING`)
  - `GROUP BY`, `HAVING`, `COUNT`, `SUM`, `TOTAL`, `AVG`, `MIN`, `MAX`, `GROUP_CONCAT` (`DISTINCT` ve `FILTER (WHERE ...)` dahil)
  - `WITH` (CTE), alt sorgular (`EXISTS`, `IN (SELECT ...)`, skaler alt sorgular), `UNION` / `UNION ALL` / `INTERSECT` / `EXCEPT`
  - `ORDER BY` (`ASC`/`DESC`, `NULLS FIRST`/`LAST`, `COLLATE BINARY`/`NOCASE`/`RTRIM`), `LIMIT` / `OFFSET`
  - Gerçek UTC sistem saatine bağlı `CURRENT_DATE`, `CURRENT_TIME`, `CURRENT_TIMESTAMP`, `date()`, `time()`, `datetime()`, `julianday()`, `unixepoch()`
  - `INSERT ... RETURNING`, `UPDATE ... RETURNING`, `DELETE ... RETURNING`
  - Token tabanlı şema yeniden yazımıyla `ALTER TABLE` (`RENAME TO`, indekslenmiş/kısıtlı sütunlar dahil `RENAME COLUMN`, `ADD COLUMN`)

---

## Kapsam Dışı / Desteklenmeyen Özellikler

- **WAL kipi (`PRAGMA journal_mode = WAL`)**: WAL kipindeki veritabanları (`read_version = 2` / `write_version = 2` veya dolu `-wal` dosyası) sessizce eksik veri döndürmek yerine açıkça `ErrorCode::CantOpen` hatasıyla reddedilir. Açmadan önce SQLite'ta `PRAGMA journal_mode = DELETE;` ile kontrol noktası alınmalıdır.
- **Auto-vacuum (`PRAGMA auto_vacuum = FULL | INCREMENTAL`)**: Pointer-map (`ptrmap`) sayfaları içeren mevcut veritabanları salt-okunur olarak açılıp sorgulanabilir; ancak pointer-map bütünlüğünü bozmamak için bu dosyalarda yazma işlemleri açık hatayla reddedilir.
- **Henüz uygulanmayan SQL özellikleri**: `CREATE VIEW`, `CREATE TRIGGER`, pencere fonksiyonları (`OVER (...)`), sanal tablolar ve uzantılar (`FTS3/4/5`, `RTREE`), `ATTACH` / `DETACH DATABASE` ve `ALTER TABLE DROP COLUMN`.

---

## Hızlı Başlangıç

### Kütüphane Kullanımı (Rust)

```rust
use sqliters::{Connection, Value};

fn main() -> sqliters::Result<()> {
    let mut conn = Connection::open_in_memory()?;

    conn.execute("
        CREATE TABLE kitaplar (
            id INTEGER PRIMARY KEY,
            baslik TEXT NOT NULL,
            yil INT NOT NULL
        );
        INSERT INTO kitaplar (baslik, yil) VALUES
            ('Saatleri Ayarlama Enstitüsü', 1961),
            ('Tutunamayanlar', 1972);
    ")?;

    let sonuc = conn.query(
        "SELECT id, baslik FROM kitaplar WHERE yil >= ?1 ORDER BY yil;",
        &[Value::Integer(1960)],
    )?;

    for satir in &sonuc.rows {
        println!("#{}: {}", satir.get_i64(0).unwrap(), satir.get_str(1).unwrap());
    }
    Ok(())
}
```

### Komut Satırı Kabuğu (`sqliters`)

```bash
# Etkileşimli REPL başlatma
cargo run -p sqliters-cli -- ornek.db

# Tek satırlık sorgu / JSON çıktısı
cargo run -p sqliters-cli -- -json ornek.db "SELECT sqlite_version();"
```

### Derleme ve Testler

Varsayılan test koşusu; crate birim testlerini, dokümantasyon testlerini (`cargo test --doc`), `/usr/bin/sqlite3` ile eşzamanlı kilitleme, sıcak günlük (*hot-journal*) kurtarma, UTF-8/UTF-16 kodlama ve `PRAGMA integrity_check` uyumluluk/regresyon testlerini (`tests/bulgular_regressions.rs`, `tests/integration_sqlite3.rs`) ve şu an tamamen geçen 53 SQLite `.test` uyarlama modülünü çalıştırır. Henüz kapsam dışı SQL özelliklerine veya eksik Tcl/`testfixture` komutlarına bağlı 168 `.test` uyarlama modülü `#[ignore]` olarak işaretlidir; bu nedenle varsayılan test koşusu tam C SQLite test paketi eşdeğerliği anlamına gelmez:

```bash
cargo test --workspace
```
