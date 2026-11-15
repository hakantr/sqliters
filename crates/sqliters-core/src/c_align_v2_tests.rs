//! C-alignment regression tests (step V2: tables in FROM). `EXPLAIN <stmt>` and `EXPLAIN QUERY PLAN <stmt>` list the
//! program / query plan of C's code generator when the port (`sqliters-codegen`) supports the whole statement. Every
//! expected listing, plan, error and schema cookie / generation below is what the C library SQLite 3.54.1 returns for
//! the same statements (produced by target/perf-work/v1/cdump/cexplain and target/perf-work/bin/sqlite3). Nothing here
//! depends on files outside the repository.
use crate::Connection;
use sqliters_types::Value;

/// The rows of a statement as text `a|b|c` (NULL is empty), or `ERR <offset> <message>` if it fails to prepare.
fn rows(conn: &Connection, sql: &str) -> Vec<String> {
    match conn.query(sql, &[]) {
        Ok(res) => res
            .rows
            .iter()
            .map(|r| {
                r.values()
                    .iter()
                    .map(|v| match v {
                        Value::Null => String::new(),
                        Value::Integer(i) => i.to_string(),
                        Value::Text(s) => s.clone(),
                        other => format!("{other:?}"),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect(),
        Err(e) => vec![format!("ERR {} {}", e.c_offset(), e.message)],
    }
}

fn schema() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    for sql in [
        "CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT DEFAULT 'x', c REAL, d)",
        "CREATE INDEX t_b ON t(b)",
        "CREATE TABLE u(x, y, z, PRIMARY KEY(x,y)) WITHOUT ROWID",
        "CREATE VIEW v1 AS SELECT a, b AS bb FROM t WHERE d>0",
    ] {
        conn.execute(sql).unwrap();
    }
    conn
}

#[test]
fn explain_join_of_rowid_and_without_rowid_table() {
    let conn = schema();
    let got = rows(&conn, "EXPLAIN SELECT b, z FROM t JOIN u ON u.x=t.a WHERE b='q' ORDER BY d");
    let want = [
        "0|Init|0|23|0||0|",
        "1|SorterOpen|2|4|0|k(1,B)|0|",
        "2|OpenRead|3|4|0|k(2,,)|0|",
        "3|OpenRead|0|2|0|4|0|",
        "4|Rewind|3|15|1|0|0|",
        "5|Column|3|0|1||0|",
        "6|SeekRowid|0|14|1||0|",
        "7|Column|0|1|2|x|0|",
        "8|Ne|3|14|2|BINARY-8|82|",
        "9|Column|0|1|5|x|0|",
        "10|Column|3|2|6||0|",
        "11|Column|0|3|4||0|",
        "12|MakeRecord|4|3|7||0|",
        "13|SorterInsert|2|7|4|3|0|",
        "14|Next|3|5|0||1|",
        "15|OpenPseudo|4|8|4||0|",
        "16|SorterSort|2|22|0||0|",
        "17|SorterData|2|8|4||0|",
        "18|Column|4|2|6||0|",
        "19|Column|4|1|5||0|",
        "20|ResultRow|5|2|0||0|",
        "21|SorterNext|2|17|0||0|",
        "22|Halt|0|0|0||0|",
        "23|Transaction|0|0|4|0|1|",
        "24|String8|0|3|0|q|0|",
        "25|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_query_plan_of_view_and_subqueries() {
    let conn = schema();
    // id | parent | notused (the cost C stores in P3 of OP_Explain) | detail
    let got = rows(
        &conn,
        "EXPLAIN QUERY PLAN SELECT bb, (SELECT count(*) FROM u WHERE x=a) FROM v1 WHERE a IN (SELECT y FROM u) ORDER BY 1",
    );
    let want = [
        "3|0|91|SEARCH t USING INTEGER PRIMARY KEY (rowid=?)",
        "7|0|0|LIST SUBQUERY 2",
        "9|7|214|SCAN u",
        "24|0|0|CORRELATED SCALAR SUBQUERY 1",
        "29|24|214|SCAN u",
        "45|0|0|USE TEMP B-TREE FOR ORDER BY",
    ];
    assert_eq!(got, want);
}

#[test]
fn compile_errors_are_c_errors() {
    let conn = schema();
    assert_eq!(rows(&conn, "EXPLAIN SELECT * FROM nosuch"), ["ERR -1 no such table: nosuch"]);
    assert_eq!(rows(&conn, "EXPLAIN QUERY PLAN SELECT nocol FROM t"), ["ERR 26 no such column: nocol"]);
}

/// The `Transaction` row of `EXPLAIN SELECT * FROM <table>`: (database, schema cookie, schema generation).
fn txn(conn: &Connection, table: &str) -> (i64, i64, i64) {
    let r = rows(conn, &format!("EXPLAIN SELECT * FROM {table}"));
    let t = r.iter().find(|l| l.split('|').nth(1) == Some("Transaction")).expect("Transaction row");
    let f: Vec<&str> = t.split('|').collect();
    (f[2].parse().unwrap(), f[4].parse().unwrap(), f[5].parse().unwrap())
}

/// C's `Schema.schema_cookie` and `Schema.iGeneration` (P3 / P4 of OP_Transaction): the generation counts the resets of
/// a loaded schema (ROLLBACK of a transaction that changed the schema, ALTER TABLE, VACUUM).
#[test]
fn schema_cookie_and_generation_follow_c() {
    let conn = Connection::open_in_memory().unwrap();
    let run = |sql: &str| {
        for s in sql.split(';').filter(|s| !s.trim().is_empty()) {
            conn.execute(s).unwrap();
        }
    };
    run("CREATE TABLE t(a,b)");
    assert_eq!(txn(&conn, "t"), (0, 1, 0));
    run("BEGIN; CREATE TABLE u(x); ROLLBACK");
    assert_eq!(txn(&conn, "t"), (0, 1, 1));
    run("ALTER TABLE t ADD COLUMN c");
    assert_eq!(txn(&conn, "t"), (0, 2, 2));
    run("CREATE TEMP TABLE tt(q)");
    assert_eq!(txn(&conn, "tt"), (1, 1, 2));
    run("ALTER TABLE t RENAME TO t9");
    assert_eq!(txn(&conn, "tt"), (1, 1, 3));
    assert_eq!(txn(&conn, "t9"), (0, 3, 3));
    run("VACUUM");
    assert_eq!(txn(&conn, "t9"), (0, 4, 4));
    run("VACUUM; VACUUM");
    assert_eq!(txn(&conn, "t9"), (0, 6, 6));
    run("BEGIN; SAVEPOINT s; CREATE TABLE u2(x); ROLLBACK TO s");
    assert_eq!(txn(&conn, "t9"), (0, 6, 7));
    run("COMMIT");
    assert_eq!(txn(&conn, "t9"), (0, 6, 7));
    run("BEGIN; INSERT INTO t9 VALUES(1,2,3); ROLLBACK");
    assert_eq!(txn(&conn, "t9"), (0, 6, 7));
}

/// The flag pragmas that only the code generator reads keep their value and are answered as C answers them.
#[test]
fn flag_pragmas_reach_the_generator() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE a(x,y)").unwrap();
    conn.execute("CREATE TABLE b(p,q)").unwrap();
    assert_eq!(rows(&conn, "PRAGMA automatic_index"), ["1"]);
    assert_eq!(
        rows(&conn, "EXPLAIN QUERY PLAN SELECT * FROM a, b WHERE a.x=b.p"),
        [
            "3|0|216|SCAN a",
            "7|0|0|BLOOM FILTER ON b (p=?)",
            "17|0|53|SEARCH b USING AUTOMATIC COVERING INDEX (p=?)",
        ]
    );
    conn.execute("PRAGMA automatic_index=OFF").unwrap();
    assert_eq!(rows(&conn, "PRAGMA automatic_index"), ["0"]);
    assert_eq!(
        rows(&conn, "EXPLAIN QUERY PLAN SELECT * FROM a, b WHERE a.x=b.p"),
        [
            "3|0|216|SCAN a",
            "5|0|216|SCAN b",
        ]
    );
    assert_eq!(rows(&conn, "PRAGMA reverse_unordered_selects"), ["0"]);
    conn.execute("PRAGMA reverse_unordered_selects=ON").unwrap();
    assert_eq!(rows(&conn, "PRAGMA reverse_unordered_selects"), ["1"]);
    assert_eq!(
        rows(&conn, "EXPLAIN SELECT x FROM a"),
        [
            "0|Init|0|7|0||0|",
            "1|OpenRead|0|2|0|1|0|",
            "2|Last|0|6|0||0|",
            "3|Column|0|0|1||0|",
            "4|ResultRow|1|1|0||0|",
            "5|Prev|0|3|0||1|",
            "6|Halt|0|0|0||0|",
            "7|Transaction|0|0|2|0|1|",
            "8|Goto|0|1|0||0|",
        ]
    );
}
