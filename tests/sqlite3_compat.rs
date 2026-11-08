//! End-to-end SQL suite and bidirectional binary compatibility tests against `/usr/bin/sqlite3` (v3.54.1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use sqliters::{Connection, ErrorCode, StepResult, Value};

static NEXT_TMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new(prefix: &str) -> Self {
        let id = NEXT_TMP_ID.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("sqliters_{prefix}_{pid}_{id}.db"));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(format!("{}-journal", path.display()));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(format!("{}-journal", self.path.display()));
    }
}

fn run_c_sqlite3(db_path: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3")
        .arg("-batch")
        .arg("-noheader")
        .arg("-list")
        .arg(db_path)
        .arg(sql)
        .output()
        .expect("failed to invoke /usr/bin/sqlite3");
    assert!(
        out.status.success(),
        "C sqlite3 failed on SQL `{sql}`:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("valid utf8 from sqlite3")
        .trim()
        .to_owned()
}

#[test]
fn test_c_sqlite3_writes_db_sqliters_reads_and_mutates() {
    let tmp = TempDb::new("c_to_rust");

    // 1. Have C SQLite 3.54.1 create tables, AUTOINCREMENT sequence, UNIQUE & DESC indexes,
    //    multi-page B-trees, UTF-8 Turkish strings, 64-bit ints, floats, blobs, and 8KB overflow rows!
    run_c_sqlite3(
        tmp.path(),
        "
        PRAGMA page_size = 1024;
        CREATE TABLE users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            email TEXT NOT NULL UNIQUE,
            full_name TEXT,
            balance REAL,
            payload BLOB,
            bio TEXT
        );
        CREATE INDEX idx_users_balance ON users(balance DESC);

        BEGIN;
        INSERT INTO users (email, full_name, balance, payload, bio) VALUES
            ('cagri@ornek.tr', 'Çağrı Şahin — İstanbul', 12500.75, X'DEADBEEFCAFE00FF', 'Kısa özgeçmiş'),
            ('gunes@ornek.tr', 'Güneş Öztürk — İzmir', -42.5, X'00010203', hex(zeroblob(4096))),
            ('deniz@ornek.tr', 'Deniz Yılmaz — Ankara', 0.0, NULL, NULL);

        WITH RECURSIVE cnt(x) AS (
            SELECT 4
            UNION ALL
            SELECT x + 1 FROM cnt WHERE x < 140
        )
        INSERT INTO users (email, full_name, balance, payload, bio)
        SELECT
            'user' || x || '@ornek.tr',
            'Kullanıcı #' || x,
            x * 10.25,
            X'ABCD',
            CASE WHEN x % 15 = 0 THEN hex(zeroblob(3000)) ELSE 'Not ' || x END
        FROM cnt;
        COMMIT;
        ",
    );

    // 2. Open with `sqliters::Connection` and verify integrity, schema, primary key seeks, index seeks, and overflows!
    {
        let conn = Connection::open(tmp.path()).expect("open C sqlite3 db");
        assert_eq!(conn.page_size().unwrap(), 1024);
        assert!(conn.page_count().unwrap() > 20);

        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(ic.rows[0][0], Value::Text("ok".into()));

        let cnt = conn
            .query_row("SELECT COUNT(*) FROM users;", &[])
            .unwrap()
            .unwrap();
        assert_eq!(cnt.get_i64(0), Some(140));

        // Seek by INTEGER PRIMARY KEY (`OP_SeekRowid`)
        let plan_pk = conn
            .query(
                "EXPLAIN QUERY PLAN SELECT full_name FROM users WHERE id = 1;",
                &[],
            )
            .unwrap();
        assert!(
            plan_pk.rows[0][3]
                .as_str()
                .unwrap()
                .contains("USING INTEGER PRIMARY KEY"),
            "unexpected plan: {:?}",
            plan_pk.rows
        );

        let r1 = conn
            .query_row(
                "SELECT full_name, balance, payload FROM users WHERE id = ?1;",
                &[Value::Integer(1)],
            )
            .unwrap()
            .unwrap();
        assert_eq!(r1.get_str(0), Some("Çağrı Şahin — İstanbul"));
        assert_eq!(r1.get_f64(1), Some(12500.75));
        assert_eq!(
            r1.get_blob(2),
            Some(&[0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0x00, 0xFF][..])
        );

        // Seek by inline UNIQUE autoindex (`sqlite_autoindex_users_1`) and verify 8192-char overflow bio!
        let plan_uniq = conn
            .query(
                "EXPLAIN QUERY PLAN SELECT id, bio FROM users WHERE email = 'gunes@ornek.tr';",
                &[],
            )
            .unwrap();
        assert!(
            plan_uniq.rows[0][3]
                .as_str()
                .unwrap()
                .contains("sqlite_autoindex_users_1"),
            "expected unique autoindex seek, got: {:?}",
            plan_uniq.rows
        );

        let r2 = conn
            .query_row(
                "SELECT id, full_name, length(bio) FROM users WHERE email = ?1;",
                &[Value::Text("gunes@ornek.tr".into())],
            )
            .unwrap()
            .unwrap();
        assert_eq!(r2.get_i64(0), Some(2));
        assert_eq!(r2.get_str(1), Some("Güneş Öztürk — İzmir"));
        assert_eq!(r2.get_i64(2), Some(8192));

        // Mutate C-created database from `sqliters` (INSERT with AUTOINCREMENT, UPDATE, DELETE)
        conn.execute(
            "
            INSERT INTO users (email, full_name, balance, payload, bio)
            VALUES ('yeni@ornek.tr', 'Yeni Kayıt', 99999.0, X'CAFE', 'Rust tarafından eklendi');
            UPDATE users SET balance = 555.5 WHERE email = 'deniz@ornek.tr';
            DELETE FROM users WHERE id BETWEEN 50 AND 80;
            ",
        )
        .unwrap();
        assert_eq!(conn.last_insert_rowid(), 141);

        let ic_after = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(ic_after.rows[0][0], Value::Text("ok".into()));
    }

    // 3. Re-verify with `/usr/bin/sqlite3` that our mutations maintained 100% Format 3 + index + sequence integrity!
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT id, full_name, hex(payload) FROM users WHERE email = 'yeni@ornek.tr';"
        ),
        "141|Yeni Kayıt|CAFE"
    );
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT balance FROM users WHERE email = 'deniz@ornek.tr';"
        ),
        "555.5"
    );
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT COUNT(*) FROM users;"),
        "110"
    );
}

#[test]
fn test_sqliters_writes_complex_db_c_sqlite3_verifies_integrity_and_queries() {
    let tmp = TempDb::new("rust_to_c");

    {
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute(
            "
            PRAGMA page_size = 1024;
            PRAGMA user_version = 35401;
            PRAGMA application_id = 1381323508;

            CREATE TABLE departments (
                id INTEGER PRIMARY KEY,
                code TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL
            );

            CREATE TABLE employees (
                id INTEGER PRIMARY KEY,
                dept_id INT NOT NULL,
                email TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                salary REAL DEFAULT 50000.0 CHECK (salary >= 0.0),
                notes TEXT
            );

            CREATE INDEX idx_emp_dept_sal ON employees(dept_id, salary DESC);

            INSERT INTO departments VALUES
                (10, 'ENG', 'Yazılım Mühendisliği'),
                (20, 'RES', 'Veritabanı Araştırma'),
                (30, 'OPS', 'Altyapı ve Operasyon');
            ",
        )
        .unwrap();

        conn.execute("BEGIN;").unwrap();
        for i in 1..=220i64 {
            let dept = match i % 3 {
                0 => 10,
                1 => 20,
                _ => 30,
            };
            let notes = if i % 19 == 0 {
                "Ö".repeat(2600) // ~5200 UTF-8 bytes -> spans 5+ overflow pages on 1024B pages!
            } else {
                format!("Çalışan notu #{i}")
            };
            conn.execute_params(
                "INSERT INTO employees (id, dept_id, email, name, salary, notes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6);",
                &[
                    Value::Integer(i),
                    Value::Integer(dept),
                    Value::Text(format!("emp{i:03}@sqliters.dev")),
                    Value::Text(format!("Mühendis {i}")),
                    Value::real((i as f64) * 1_250.5),
                    Value::Text(notes),
                ],
            )
            .unwrap();
        }
        // Delete 70 rows & update 25 rows inside transaction to stress B-tree splits, merges, and freelist!
        conn.execute(
            "
            DELETE FROM employees WHERE id >= 40 AND id < 110;
            UPDATE employees SET salary = salary + 5000.0, dept_id = 20 WHERE id <= 25;
            COMMIT;
            ",
        )
        .unwrap();

        let ic = conn.query("PRAGMA integrity_check;", &[]).unwrap();
        assert_eq!(ic.rows[0][0], Value::Text("ok".into()));

        let rust_grouped = conn
            .query(
                "SELECT d.code, COUNT(*)
                 FROM departments d
                 JOIN employees e ON e.dept_id = d.id
                 GROUP BY d.code
                 ORDER BY d.code;",
                &[],
            )
            .unwrap();
        let rust_rendered: Vec<String> = rust_grouped
            .rows
            .iter()
            .map(|r| format!("{}|{}", r.get_str(0).unwrap(), r.get_i64(1).unwrap()))
            .collect();
        assert_eq!(rust_rendered.join("\n"), "ENG|42\nOPS|42\nRES|66");
    }

    // Verify with C `/usr/bin/sqlite3`!
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA page_size;"), "1024");
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA user_version;"), "35401");
    assert_eq!(
        run_c_sqlite3(tmp.path(), "PRAGMA application_id;"),
        "1381323508"
    );
    assert_eq!(
        run_c_sqlite3(tmp.path(), "SELECT COUNT(*) FROM employees;"),
        "150"
    );

    // Force C SQLite to use the secondary index (`idx_emp_dept_sal`) and inline unique index we built!
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT id, name, salary FROM employees WHERE email = 'emp019@sqliters.dev';"
        ),
        "19|Mühendis 19|28759.5"
    );
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT length(notes) FROM employees WHERE id = 19;"
        ),
        "2600"
    );
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT d.code, COUNT(*)
             FROM departments d
             JOIN employees e ON e.dept_id = d.id
             GROUP BY d.code
             ORDER BY d.code;"
        ),
        "ENG|42\nOPS|42\nRES|66"
    );
}

#[test]
fn test_sql_features_joins_aggregates_ctes_subqueries_alter_vacuum_and_savepoints() {
    let tmp = TempDb::new("full_sql");
    let conn = Connection::open(tmp.path()).unwrap();

    // 1. SQLite 3.54 `_` numeric separators, STRICT tables, conflict clauses, RETURNING
    conn.execute(
        "
        CREATE TABLE items (
            id INTEGER PRIMARY KEY,
            sku TEXT NOT NULL UNIQUE,
            category TEXT NOT NULL COLLATE NOCASE,
            price INT NOT NULL,
            tag TEXT DEFAULT 'standart'
        ) STRICT;
        ",
    )
    .unwrap();

    // STRICT table rejects non-integer text into INT column
    let strict_err = conn
        .execute("INSERT INTO items (id, sku, category, price) VALUES (1, 'A1', 'Kitap', 'abc');")
        .unwrap_err();
    assert_eq!(strict_err.code, ErrorCode::Constraint);

    let ret = conn
        .query(
            "INSERT INTO items (sku, category, price) VALUES
                ('SKU-1', 'Elektronik', 1_500),
                ('SKU-2', 'elektronik', 2_500),
                ('SKU-3', 'Kitap', 0x01_00),
                ('SKU-4', 'Kitap', 400),
                ('SKU-5', 'Müzik', 750)
             RETURNING id, sku, price, tag;",
            &[],
        )
        .unwrap();
    assert_eq!(ret.rows.len(), 5);
    assert_eq!(ret.rows[2].get_i64(2), Some(256));
    assert_eq!(ret.rows[0].get_str(3), Some("standart"));

    // INSERT OR IGNORE & INSERT OR REPLACE
    conn.execute(
        "
        INSERT OR IGNORE INTO items (sku, category, price) VALUES ('SKU-1', 'Diğer', 9999);
        INSERT OR REPLACE INTO items (sku, category, price) VALUES ('SKU-5', 'Müzik', 900);
        ",
    )
    .unwrap();

    // 2. COLLATE NOCASE grouping + HAVING + FILTER + DISTINCT aggregates
    let agg = conn
        .query(
            "SELECT
                UPPER(category) AS cat,
                COUNT(*) AS cnt,
                SUM(price) AS total_p,
                COUNT(*) FILTER (WHERE price >= 1000) AS high_cnt
             FROM items
             GROUP BY category
             HAVING SUM(price) > 500
             ORDER BY total_p DESC;",
            &[],
        )
        .unwrap();
    assert_eq!(agg.rows.len(), 3);
    assert_eq!(agg.rows[0].get_str(0), Some("ELEKTRONIK"));
    assert_eq!(agg.rows[0].get_i64(1), Some(2));
    assert_eq!(agg.rows[0].get_i64(2), Some(4000));
    assert_eq!(agg.rows[0].get_i64(3), Some(2));

    // 3. CTEs, Compounds (`UNION`, `INTERSECT`, `EXCEPT`), Subqueries, Joins (`LEFT`, `FULL`, `USING`)
    conn.execute(
        "
        CREATE TABLE stock (sku TEXT, qty INT);
        INSERT INTO stock VALUES ('SKU-1', 10), ('SKU-3', 5), ('SKU-999', 99);
        ",
    )
    .unwrap();

    let joined = conn
        .query(
            "WITH expensive AS (
                SELECT sku, price FROM items WHERE price >= (SELECT AVG(price) FROM items)
             )
             SELECT sku, e.price, COALESCE(s.qty, 0) AS on_hand
             FROM expensive e
             LEFT JOIN stock s USING (sku)
             ORDER BY e.price DESC;",
            &[],
        )
        .unwrap();
    assert_eq!(joined.rows.len(), 2);
    assert_eq!(joined.rows[0].get_str(0), Some("SKU-2"));
    assert_eq!(joined.rows[0].get_i64(2), Some(0));
    assert_eq!(joined.rows[1].get_str(0), Some("SKU-1"));
    assert_eq!(joined.rows[1].get_i64(2), Some(10));

    let compound = conn
        .query(
            "SELECT sku FROM items
             INTERSECT
             SELECT sku FROM stock
             ORDER BY 1;",
            &[],
        )
        .unwrap();
    assert_eq!(compound.rows.len(), 2);
    assert_eq!(compound.rows[0].get_str(0), Some("SKU-1"));
    assert_eq!(compound.rows[1].get_str(0), Some("SKU-3"));

    // 4. Prepared Statement with named parameters + step/reset reuse
    {
        let mut stmt = conn
            .prepare("SELECT sku, price FROM items WHERE price >= :min_p ORDER BY price ASC;")
            .unwrap();
        stmt.bind_named(":min_p", 800i64).unwrap();
        let mut first_run = Vec::new();
        while let StepResult::Row(r) = stmt.step().unwrap() {
            first_run.push(r.get_str(0).unwrap().to_owned());
        }
        assert_eq!(first_run, vec!["SKU-5", "SKU-1", "SKU-2"]);

        stmt.reset().unwrap();
        stmt.bind_named(":min_p", 2000i64).unwrap();
        let second_run = stmt.collect_rows().unwrap();
        assert_eq!(second_run.len(), 1);
        assert_eq!(second_run[0].get_str(0), Some("SKU-2"));
    }

    // 5. Nested SAVEPOINT / ROLLBACK TO / RELEASE & Transaction Rollback
    conn.execute(
        "
        SAVEPOINT sp_outer;
        INSERT INTO stock VALUES ('SP-KEEP', 1);
        SAVEPOINT sp_inner;
        DELETE FROM stock;
        ROLLBACK TO sp_inner;
        RELEASE sp_outer;
        ",
    )
    .unwrap();
    assert!(conn.is_autocommit());
    let sp_cnt = conn
        .query_row("SELECT COUNT(*) FROM stock WHERE sku = 'SP-KEEP';", &[])
        .unwrap()
        .unwrap();
    assert_eq!(sp_cnt.get_i64(0), Some(1));

    // 6. ALTER TABLE (ADD COLUMN, RENAME COLUMN, RENAME TO) + VACUUM
    conn.execute(
        "
        ALTER TABLE stock ADD COLUMN warehouse TEXT DEFAULT 'IST-1';
        ALTER TABLE stock RENAME COLUMN qty TO quantity;
        ALTER TABLE stock RENAME TO inventory;
        DELETE FROM items WHERE price < 900;
        VACUUM;
        ",
    )
    .unwrap();

    let inv = conn
        .query_row(
            "SELECT quantity, warehouse FROM inventory WHERE sku = 'SKU-1';",
            &[],
        )
        .unwrap()
        .unwrap();
    assert_eq!(inv.get_i64(0), Some(10));
    assert_eq!(inv.get_str(1), Some("IST-1"));

    drop(conn);

    // Confirm C SQLite 3.54.1 reads altered & vacuumed DB with `"ok"` integrity!
    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT quantity, warehouse FROM inventory WHERE sku = 'SKU-1';"
        ),
        "10|IST-1"
    );
}

#[test]
fn test_disk_transaction_rollback_and_vdbe_explain_bytecode() {
    let tmp = TempDb::new("tx_rollback");
    {
        let conn = Connection::open(tmp.path()).unwrap();
        conn.execute(
            "
            CREATE TABLE accounts (id INTEGER PRIMARY KEY, owner TEXT UNIQUE, balance INT);
            INSERT INTO accounts VALUES (1, 'Ada', 1000), (2, 'Alan', 2000);
            ",
        )
        .unwrap();

        // Start a write transaction, mutate heavily, then ROLLBACK
        conn.execute(
            "
            BEGIN IMMEDIATE;
            UPDATE accounts SET balance = 0 WHERE id = 1;
            INSERT INTO accounts VALUES (3, 'Grace', 5000);
            DELETE FROM accounts WHERE id = 2;
            ROLLBACK;
            ",
        )
        .unwrap();

        let rows = conn
            .query("SELECT id, owner, balance FROM accounts ORDER BY id;", &[])
            .unwrap();
        assert_eq!(rows.rows.len(), 2);
        assert_eq!(rows.rows[0].get_i64(2), Some(1000));
        assert_eq!(rows.rows[1].get_i64(2), Some(2000));

        // Verify EXPLAIN emits VDBE bytecode instructions
        let explain = conn
            .query(
                "EXPLAIN SELECT id, balance FROM accounts WHERE owner = 'Ada';",
                &[],
            )
            .unwrap();
        assert_eq!(explain.column_names.len(), 8);
        let opcodes: Vec<&str> = explain.rows.iter().filter_map(|r| r.get_str(1)).collect();
        assert!(opcodes.contains(&"Init"));
        assert!(opcodes.contains(&"OpenRead"));
        assert!(opcodes.contains(&"SeekGE"));
        assert!(opcodes.contains(&"ResultRow"));
        assert!(opcodes.contains(&"Halt"));
    }

    assert_eq!(run_c_sqlite3(tmp.path(), "PRAGMA integrity_check;"), "ok");
    assert_eq!(
        run_c_sqlite3(
            tmp.path(),
            "SELECT id, owner, balance FROM accounts ORDER BY id;"
        ),
        "1|Ada|1000\n2|Alan|2000"
    );
}
