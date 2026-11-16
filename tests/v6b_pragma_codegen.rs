//! C 3.54.1 programs and pragma.c's distinction between prepare and step.
use sqliters::types::Value;
use sqliters::{Connection, StepResult};
use std::process::Command;

fn integer(conn: &Connection, sql: &str) -> i64 {
    match &conn.query(sql, &[]).unwrap().rows[0].values()[0] {
        Value::Integer(n) => *n,
        v => panic!("{sql}: {v:?}"),
    }
}

#[test]
fn flag_pragmas_take_effect_at_prepare_including_explain() {
    let conn = Connection::open_in_memory().unwrap();
    let pending = conn.prepare("PRAGMA foreign_keys=ON").unwrap();
    assert_eq!(integer(&conn, "PRAGMA foreign_keys"), 1);
    drop(pending);
    let pending = conn.prepare("EXPLAIN PRAGMA foreign_keys=OFF").unwrap();
    assert_eq!(integer(&conn, "PRAGMA foreign_keys"), 0);
    drop(pending);
    conn.execute_batch("BEGIN").unwrap();
    let pending = conn.prepare("PRAGMA foreign_keys=ON").unwrap();
    assert_eq!(integer(&conn, "PRAGMA foreign_keys"), 0);
    drop(pending);
    conn.execute_batch("ROLLBACK").unwrap();

    conn.execute("PRAGMA trusted_schema=OFF").unwrap();
    assert_eq!(integer(&conn, "PRAGMA trusted_schema"), 0);
    conn.prepare("EXPLAIN PRAGMA trusted_schema=ON").unwrap();
    assert_eq!(integer(&conn, "PRAGMA trusted_schema"), 1);

    conn.set_defensive(true).unwrap();
    conn.db_config_writable_schema(Some(true));
    assert_eq!(integer(&conn, "PRAGMA writable_schema"), 1);
    conn.set_defensive(false).unwrap();
    conn.execute("PRAGMA writable_schema=ON").unwrap();
    conn.db_config_writable_schema(Some(false));
    assert_eq!(integer(&conn, "PRAGMA writable_schema"), 0); // db_config clears both C bits

    let other = Connection::open_in_memory().unwrap();
    let mut query = other.prepare("PRAGMA foreign_keys").unwrap();
    query.step().unwrap();
    query.step().unwrap();
    conn.c_generated_program("PRAGMA foreign_keys=ON").unwrap();
    query.reset().unwrap();
    let StepResult::Row(row) = query.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(0)]); // diagnostic compilation cannot change another connection
}

#[test]
fn explain_pragma_checks_that_the_named_btree_exists_before_lookup() {
    let conn = Connection::open_in_memory().unwrap();
    for name in ["automatic_index", "table_list", "unknown_pragma"] {
        let error = conn
            .prepare(&format!("EXPLAIN PRAGMA temp.{name}"))
            .unwrap_err();
        assert_eq!(error.message, "SQL logic error");
    }
    conn.prepare("PRAGMA temp.unknown_pragma").unwrap();
    conn.prepare("EXPLAIN PRAGMA temp.automatic_index").unwrap();
}

#[test]
fn table_list_does_not_approximate_a_hidden_views_initialization_history() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "ATTACH ':memory:' AS aux; CREATE TABLE t(a); CREATE VIEW aux.t AS SELECT 1 AS x, 2 AS y;",
    )
    .unwrap();
    for initialized in [false, true] {
        if initialized {
            conn.query("SELECT * FROM aux.t", &[]).unwrap();
        }
        // C reports nCol=0 before the view is used and nCol=2 afterwards.
        // This history is not represented by the schema adapter yet.
        let error = conn
            .c_generated_program("PRAGMA aux.table_list")
            .unwrap_err();
        assert!(
            error.starts_with("unsupported: PRAGMA table_list hidden view"),
            "{error}"
        );
    }
}

#[test]
fn pragma_results_and_assignments_keep_their_prepare_time_meaning() {
    let conn = Connection::open_in_memory().unwrap();
    let mut query = conn.prepare("PRAGMA foreign_keys").unwrap();
    let mut setter = conn.prepare("PRAGMA foreign_keys='2'").unwrap();
    let StepResult::Row(row) = query.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(0)]);
    assert!(matches!(query.step().unwrap(), StepResult::Done));
    assert!(matches!(setter.step().unwrap(), StepResult::Done));
    query.reset().unwrap();
    let StepResult::Row(row) = query.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(1)]);
    drop(query);
    let mut first = conn.prepare("PRAGMA foreign_keys=ON").unwrap();
    let second = conn.prepare("PRAGMA foreign_keys=OFF").unwrap();
    first.step().unwrap();
    drop(second);
    assert_eq!(integer(&conn, "PRAGMA foreign_keys"), 0);

    let mut first = conn.prepare("PRAGMA analysis_limit=17").unwrap();
    let second = conn.prepare("PRAGMA analysis_limit=23").unwrap();
    let StepResult::Row(row) = first.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(17)]);
    drop(second);
    assert_eq!(integer(&conn, "PRAGMA analysis_limit"), 23);

    conn.execute("PRAGMA foreign_keys=OFF").unwrap();
    let mut old = conn.prepare("PRAGMA foreign_keys").unwrap();
    conn.execute("PRAGMA foreign_keys=ON").unwrap();
    let StepResult::Row(row) = old.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(1)]); // OP_Expire invalidates an unstarted query
}

#[test]
fn pager_pragma_results_freeze_and_explain_applies_settings() {
    let conn = Connection::open_in_memory().unwrap();
    for name in ["cache_size", "journal_size_limit", "secure_delete"] {
        let error = conn.prepare(&format!("PRAGMA {name}=-'17'")).unwrap_err();
        assert_eq!(error.message, "near \"'17'\": syntax error");
    }
    for (name, before, after) in [
        ("cache_size", 17, 23),
        ("journal_size_limit", 31, 47),
        ("secure_delete", 1, 2),
    ] {
        conn.execute(&format!("PRAGMA {name}={before}")).unwrap();
        let mut query = conn.prepare(&format!("PRAGMA {name}")).unwrap();
        let setting = if name == "secure_delete" {
            "FAST".to_owned()
        } else {
            after.to_string()
        };
        conn.prepare(&format!("EXPLAIN PRAGMA {name}={setting}"))
            .unwrap();
        let StepResult::Row(row) = query.step().unwrap() else {
            panic!("expected row")
        };
        assert_eq!(row.values(), &[Value::Integer(before)]);
        assert_eq!(integer(&conn, &format!("PRAGMA {name}")), after);
    }
    let mut setter = conn.prepare("PRAGMA journal_size_limit=53").unwrap();
    conn.prepare("PRAGMA journal_size_limit=59").unwrap();
    let StepResult::Row(row) = setter.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(53)]);
    assert_eq!(integer(&conn, "PRAGMA journal_size_limit"), 59);
    for (literal, expected) in [
        ("9223372036854775807", i64::MAX),
        ("4294967296", 4294967296),
        ("0x10000000000000001", 1),
    ] {
        assert_eq!(
            integer(&conn, &format!("PRAGMA journal_size_limit={literal}")),
            expected
        );
        assert_eq!(integer(&conn, "PRAGMA journal_size_limit"), expected);
    }
}

#[test]
fn secure_delete_prepare_changes_only_the_c_selected_btrees() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("ATTACH ':memory:' AS aux; PRAGMA temp.unknown_pragma;")
        .unwrap();
    conn.prepare("EXPLAIN PRAGMA secure_delete=FAST").unwrap();
    for schema in ["main", "temp", "aux"] {
        assert_eq!(integer(&conn, &format!("PRAGMA {schema}.secure_delete")), 2);
    }
    conn.prepare("PRAGMA aux.secure_delete=OFF").unwrap();
    assert_eq!(integer(&conn, "PRAGMA aux.secure_delete"), 0);
    assert_eq!(integer(&conn, "PRAGMA main.secure_delete"), 2);
    conn.prepare("PRAGMA temp.journal_size_limit=29").unwrap();
    assert_eq!(integer(&conn, "PRAGMA temp.journal_size_limit"), 29);
    conn.prepare("EXPLAIN PRAGMA cache_size=2147483648")
        .unwrap();
    assert_eq!(integer(&conn, "PRAGMA cache_size"), 0);
    conn.execute("CREATE TABLE cache_zero(a)").unwrap();
    assert_eq!(integer(&conn, "PRAGMA cache_size"), 0);
    conn.prepare("PRAGMA writable_schema=RESET").unwrap();
    assert_eq!(integer(&conn, "PRAGMA cache_size"), -2000);
}

#[test]
fn cache_size_prepare_loads_the_default_cookie_from_a_c_file() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "sqliters_v6b_cache_{}_{nonce}.db",
        std::process::id()
    ));
    let created = Command::new("sqlite3")
        .arg(&path)
        .arg("PRAGMA default_cache_size=83; CREATE TABLE t(a);")
        .output()
        .unwrap();
    assert!(created.status.success(), "{:?}", created.stderr);
    let conn = Connection::open(&path).unwrap();
    let mut query = conn.prepare("PRAGMA cache_size").unwrap();
    conn.prepare("PRAGMA cache_size=89").unwrap();
    let StepResult::Row(row) = query.step().unwrap() else {
        panic!("expected row")
    };
    assert_eq!(row.values(), &[Value::Integer(83)]);
    assert_eq!(integer(&conn, "PRAGMA cache_size"), 89);
    drop(query);
    drop(conn);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn pragma_programs_match_c_for_schema_headers_and_flags() {
    let reference = Command::new("sqlite3")
        .args([":memory:", "SELECT sqlite_version()"])
        .output()
        .expect("C SQLite 3.54.1 must be on PATH");
    assert_eq!(reference.stdout, b"3.54.1\n");
    let setup = "CREATE TABLE t(a INTEGER PRIMARY KEY,b TEXT DEFAULT 'x',c INT GENERATED ALWAYS AS (a+1) STORED, UNIQUE(b), FOREIGN KEY(b) REFERENCES t(b)); CREATE INDEX ix ON t(b COLLATE NOCASE DESC,a+1) WHERE b IS NOT NULL; CREATE VIEW v AS SELECT a,b FROM t;";
    let names = [
        "analysis_limit",
        "application_id",
        "automatic_index",
        "count_changes",
        "foreign_keys",
        "full_column_names",
        "query_only",
        "schema_version",
        "user_version",
        "data_version",
        "freelist_count",
        "default_cache_size",
        "cache_size",
        "journal_size_limit",
        "secure_delete",
        "incremental_vacuum",
        "index_info",
        "index_xinfo",
        "index_list",
        "foreign_key_list",
        "foreign_key_check",
        "journal_mode",
        "max_page_count",
        "page_count",
        "pragma_list",
        "table_info",
        "table_xinfo",
        "table_list",
        "optimize",
        "wal_checkpoint",
    ];
    for name in names {
        for value in [
            "",
            "=0",
            "=-1",
            "=0x10",
            "=0x10000000000000001",
            "='t'",
            "('ix')",
            "('full')",
            "('restart')",
            "('truncate')",
            "('noop')",
        ] {
            let conn = Connection::open_in_memory().unwrap();
            conn.set_defensive(true).unwrap(); // the C shell enables defensive mode
            conn.execute_batch(setup).unwrap();
            let sql = format!("EXPLAIN PRAGMA {name}{value};");
            let c = Command::new("sqlite3")
                .args([
                    "-batch",
                    "-list",
                    "-cmd",
                    ".explain off",
                    ":memory:",
                    &format!("{setup}{sql}"),
                ])
                .output()
                .unwrap();
            let r = conn.query(&sql, &[]);
            if !c.status.success() {
                assert!(r.is_err(), "C rejects {sql}, Rust accepts");
                continue;
            }
            let r = r.unwrap_or_else(|e| panic!("{sql}: {e}"));
            let mut bytes = Vec::new();
            for row in r.rows {
                for (i, value) in row.values().iter().enumerate() {
                    if i > 0 {
                        bytes.push(b'|');
                    }
                    match value {
                        Value::Null => {}
                        Value::Integer(n) => bytes.extend(n.to_string().as_bytes()),
                        Value::Text(s) => bytes.extend(s.as_bytes()),
                        other => panic!("unexpected EXPLAIN value: {other:?}"),
                    }
                }
                bytes.push(b'\n');
            }
            assert_eq!(bytes, c.stdout, "{sql}");
        }
    }
}
