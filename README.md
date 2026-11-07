# sqliters

**SQLite 3.54.1** kod tabanının (`sqlite/`) sıfır harici bağımlılıkla (*zero external dependencies*) ve tamamen güvenli Rust (`#![forbid(unsafe_code)]`) ile sıfırdan yazılmış, **SQLite Format 3** ikili dosya biçimiyle çift yönlü (%100) uyumlu Rust portu.

`sqliters` tarafından oluşturulan `.db` veritabanı dosyaları doğrudan C `sqlite3` tarafından okunup yazılabilir (`PRAGMA integrity_check` → `ok`); C `sqlite3` tarafından oluşturulan çok sayfalı B-Tree, ikincil indeks ve taşma (*overflow*) zincirli veritabanları da `sqliters` tarafından doğrudan açılıp sorgulanabilir.

---

## Mimari ve C → Rust Katman Eşlemesi

Workspace, C SQLite mimarisini birebir yansıtan katmanlı crate'lerden oluşur:

| Katman | Crate | Karşılık Gelen C SQLite Kaynakları (`sqlite/src/`) | Sorumluluk |
| :--- | :--- | :--- | :--- |
| **0. Türler & Kayıt** | `crates/sqliters-types` | `sqlite.h.in`, `sqliteLimit.h`, `util.c`, `vdbeaux.c`, `bitvec.c` | Hata kodları, sabitler, 1..=9 bayt big-endian `varint`, `Value`, `TypeAffinity` (§3.1), `Collation` (`BINARY`/`NOCASE`/`RTRIM`), kayıpsız `i64`/`f64` karşılaştırması, Format 4 kayıt serileştirme |
| **1. VFS** | `crates/sqliters-vfs` | `os.h`, `os_unix.c`, `memvfs.c` | `Vfs` & `VfsFile` soyutlamaları, POSIX konumsal G/Ç (`OsVfs`), bellek içi VFS (`MemoryVfs`), kısa okumada sıfır doldurma (*zero-fill*) |
| **2. Pager** | `crates/sqliters-pager` | `pager.c`, `pcache1.c` | 100 baytlık veritabanı başlığı (`SQLite format 3\0`), sayfa önbelleği, atomik *rollback journal* (`-journal` sihirli başlığı + sağlama toplamı + çökme kurtarma), işlemler ve iç içe `SAVEPOINT` |
| **3. B-Tree** | `crates/sqliters-btree` | `btree.h`, `btreeInt.h`, `btree.c` | Tablo B+Tree (`0x05`/`0x0d`), İndeks B-Tree (`0x02`/`0x0a`), sayfa bölünmesi (*split*) ve Sayfa 1 kök korunumu, taşma (*overflow*) sayfa zincirleri, *freelist* gövde/yaprak yönetimi, `PRAGMA integrity_check` |
| **4. Sözdizimi** | `crates/sqliters-syntax` | `tokenize.c`, `complete.c`, `parse.y`, `mkkeywordhash.c` | SQLite 3.54.1 sözcük çözümleyici (`_` sayısal ayraçlar dahil), `is_complete` 8 durumlu DFA, tam SQL AST ve Pratt / özyinelemeli-iniş ayrıştırıcı |
| **5. VDBE** | `crates/sqliters-vdbe` | `vdbe.h`, `vdbe.c`, `vdbesort.c`, `func.c` | Yazmaç tabanlı sanal makine (`VdbeVm`), `EXPLAIN` baytkod sökümü, imleçler, sıralayıcı (`Sorter`), yerleşik skaler ve kümeleme (*aggregate*) fonksiyonları |
| **6. Çekirdek & API** | `crates/sqliters-core` | `main.c`, `prepare.c`, `build.c`, `select.c`, `insert.c`, `update.c`, `delete.c`, `alter.c`, `vacuum.c`, `pragma.c` | Sayfa 1 `sqlite_schema` kataloğu, SQL→VDBE derleyicisi ve indeks erişim planlayıcısı, ilişkisel sorgu motoru, kısıtlayıcılar, `Connection` / `Statement` API'si |
| **7. Kök & CLI** | `.` (`sqliters`), `crates/sqliters-cli` | `shell.c.in` | Genel kütüphane arayüzü ve etkileşimli/toplu `sqliters` komut satırı kabuğu (`.tables`, `.schema`, `.dbinfo`, `.dump`, `.mode box\|table\|csv\|json\|line\|list`) |

---

## Desteklenen Başlıca Özellikler

- **Depolama & İşlemler**:
  - 512..=65536 bayt sayfa boyutları, `INTEGER PRIMARY KEY` (`rowid` takma adı) ve `AUTOINCREMENT` (`sqlite_sequence`)
  - İkincil `UNIQUE` ve tekil olmayan B-Tree indeksler, `sqlite_autoindex_*` uyumluluğu, kısmi indeksler (`WHERE ...`)
  - Çok sayfalı taşma (*overflow*) yükleri, *freelist* sayfa geri kazanımı ve `VACUUM`
  - `BEGIN [DEFERRED | IMMEDIATE | EXCLUSIVE]`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `ROLLBACK TO`, `RELEASE`
- **SQL Sorgu & Veri İşleme**:
  - `SELECT`, `DISTINCT`, `JOIN` (`INNER`, `LEFT`, `RIGHT`, `FULL OUTER`, `CROSS`, `NATURAL`, `ON`, `USING`)
  - `GROUP BY`, `HAVING`, `COUNT`, `SUM`, `TOTAL`, `AVG`, `MIN`, `MAX`, `GROUP_CONCAT` (`DISTINCT` ve `FILTER (WHERE ...)` dahil)
  - `WITH` (CTE), alt sorgular (`EXISTS`, `IN (SELECT ...)`, skaler alt sorgular), `UNION` / `UNION ALL` / `INTERSECT` / `EXCEPT`
  - `ORDER BY` (`ASC`/`DESC`, `NULLS FIRST`/`LAST`, `COLLATE BINARY`/`NOCASE`/`RTRIM`), `LIMIT` / `OFFSET`
  - `INSERT` (`VALUES`, `SELECT`, `DEFAULT VALUES`, `OR IGNORE`, `OR REPLACE`, `RETURNING`), `UPDATE ... RETURNING`, `DELETE ... RETURNING`
  - `CREATE TABLE` (`STRICT` tablolar, `CHECK`, `NOT NULL`, `UNIQUE`, `DEFAULT`), `DROP TABLE`, `CREATE/DROP INDEX`, `ALTER TABLE` (`RENAME TO`, `RENAME COLUMN`, `ADD COLUMN`)
  - `EXPLAIN`, `EXPLAIN QUERY PLAN` ve `PRAGMA` komutları (`integrity_check`, `page_size`, `page_count`, `freelist_count`, `user_version`, `application_id`, `table_info`, `index_list`, `index_info`, `database_list`)

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

Test paketi, birim testlerinin yanı sıra sistemdeki `/usr/bin/sqlite3` ile çift yönlü ikili dosya uyumluluğu ve `PRAGMA integrity_check` doğrulamalarını çalıştırır:

```bash
cargo test --workspace
```
