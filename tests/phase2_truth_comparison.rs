use sqliters::{Connection, Value};

fn integers(conn: &Connection, sql: &str) -> Vec<i64> {
    conn.query(sql, &[])
        .unwrap_or_else(|err| panic!("{sql}: {err}"))
        .rows
        .iter()
        .flat_map(|row| row.values())
        .map(|value| match value {
            Value::Integer(n) => *n,
            value => panic!("{sql}: expected integer, received {value:?}"),
        })
        .collect()
}

#[test]
fn truth_comparison_syntax_and_blob_encoding() {
    for (encoding, blob) in [
        ("UTF-8", vec![1, 1, 0]),
        ("UTF-16le", vec![0, 1, 0]),
        ("UTF-16be", vec![0, 0, 1]),
    ] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "PRAGMA encoding = '{encoding}'; CREATE TABLE t(v);"
        ))
        .unwrap();
        assert_eq!(
            integers(
                &conn,
                "SELECT 2 IS TRUE, 2 IS (TRUE COLLATE BINARY), 2 IS +TRUE, 2 IS 1,
                        2 IS CAST(TRUE AS INTEGER), 2 IS likely(TRUE),
                        NULL IS FALSE, NULL IS NOT FALSE, 2 IS DISTINCT FROM TRUE,
                        2 IS (missing NOT IN ()), 2 IS (abs(1) NOT IN ());"
            ),
            vec![1, 1, 0, 0, 0, 0, 0, 1, 0, 1, 0]
        );
        assert_eq!(
            integers(
                &conn,
                "SELECT x'32' IS TRUE, x'3200' IS TRUE, x'0032' IS TRUE;"
            ),
            blob
        );
    }
}

#[test]
fn truth_comparison_respects_column_and_alias_lookup() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE d(id INTEGER PRIMARY KEY, true INTEGER);
         INSERT INTO d VALUES(2,1);
         CREATE TABLE e(id INTEGER PRIMARY KEY);
         INSERT INTO e VALUES(1),(2);",
    )
    .unwrap();
    assert_eq!(
        integers(&conn, "SELECT 2 IS true, 2 IS d.true FROM d;"),
        vec![0, 0]
    );
    assert_eq!(
        integers(&conn, "SELECT 9 AS true, 2 IS true FROM e LIMIT 1;"),
        vec![9, 1]
    );
    assert_eq!(
        integers(&conn, "SELECT 1 AS true FROM e WHERE id IS true;"),
        vec![1]
    );
}

#[test]
fn truth_comparison_writes_all_matching_keys() {
    for create in [
        "CREATE TABLE d(k INTEGER PRIMARY KEY, hit INTEGER);",
        "CREATE TABLE d(k INTEGER NOT NULL UNIQUE, hit INTEGER);",
        "CREATE TABLE d(k INTEGER PRIMARY KEY, hit INTEGER) WITHOUT ROWID;",
    ] {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(create).unwrap();
        conn.execute_batch(
            "INSERT INTO d VALUES(-7,0),(0,0),(1,0),(2,0);
             UPDATE d SET hit=1 WHERE k IS TRUE;",
        )
        .unwrap();
        assert_eq!(
            integers(&conn, "SELECT k FROM d WHERE hit=1 ORDER BY k;"),
            vec![-7, 1, 2]
        );
        conn.execute_batch("DELETE FROM d WHERE k IS 1;").unwrap();
        assert_eq!(
            integers(&conn, "SELECT k FROM d ORDER BY k;"),
            vec![-7, 0, 2]
        );
        conn.execute_batch("DELETE FROM d WHERE k IS TRUE;")
            .unwrap();
        assert_eq!(integers(&conn, "SELECT k FROM d;"), vec![0]);
    }
}
