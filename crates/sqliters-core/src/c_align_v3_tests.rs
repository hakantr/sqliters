//! C-alignment regression tests (step V3). Every expected value is what the C library SQLite 3.54.1 produces for the
//! same statements (target/perf-work/bin/sqlite3). Nothing here depends on files outside the repository.
use crate::Connection;
use sqliters_types::Value;

/// The rows of a statement as text `a|b|c` (NULL is empty).
fn rows(conn: &Connection, sql: &str) -> Vec<String> {
    conn.query(sql, &[])
        .unwrap()
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
        .collect()
}

/// D1: C's parser takes a one-column INTEGER PRIMARY KEY for the rowid alias until it sees WITHOUT ROWID at the end of
/// the statement; `convertToWithoutRowidTable()` then creates the PRIMARY KEY index last, so the UNIQUE indexes get the
/// lower numbers (and the rows of `sqlite_schema`). A table-level `PRIMARY KEY(c DESC)` on an INTEGER column of a rowid
/// table is the rowid alias too (no index at all).
#[test]
fn automatic_index_names_and_schema_rows_follow_c() {
    let conn = Connection::open_in_memory().unwrap();
    for sql in [
        "CREATE TABLE q(c INTEGER, d, PRIMARY KEY(c DESC))",
        "CREATE TABLE t1(a, b, c INTEGER, PRIMARY KEY(c), UNIQUE(a,b)) WITHOUT rowid",
        "CREATE TABLE t2(c INTEGER PRIMARY KEY, a UNIQUE, b, UNIQUE(c)) WITHOUT rowid",
        "CREATE TABLE t4(c INTEGER PRIMARY KEY DESC, a UNIQUE) WITHOUT rowid",
        "CREATE TABLE t5(c INT PRIMARY KEY, a UNIQUE) WITHOUT rowid",
        "CREATE TABLE t6(a UNIQUE, c INTEGER PRIMARY KEY ON CONFLICT REPLACE, b UNIQUE ON CONFLICT IGNORE) WITHOUT rowid",
    ] {
        conn.execute(sql).unwrap();
    }
    assert_eq!(
        rows(&conn, "SELECT name, rootpage FROM sqlite_schema"),
        [
            "q|2",
            "t1|3",
            "sqlite_autoindex_t1_1|4",
            "t2|5",
            "sqlite_autoindex_t2_1|6",
            "t4|7",
            "sqlite_autoindex_t4_2|8",
            "t5|9",
            "sqlite_autoindex_t5_2|10",
            "t6|11",
            "sqlite_autoindex_t6_1|12",
            "sqlite_autoindex_t6_2|13",
        ]
    );
    // The names of the in-memory PRIMARY KEY indexes (they have no row of their own)
    let pk = |t: &str| -> String {
        rows(&conn, &format!("PRAGMA index_list({t})")).into_iter().find(|r| r.contains("|pk|")).unwrap_or_default()
    };
    assert!(pk("t1").contains("sqlite_autoindex_t1_2"));
    assert!(pk("t2").contains("sqlite_autoindex_t2_2"));
    assert!(pk("t4").contains("sqlite_autoindex_t4_1"));
    assert!(pk("t5").contains("sqlite_autoindex_t5_1"));
    assert!(pk("t6").contains("sqlite_autoindex_t6_3"));
    assert!(rows(&conn, "PRAGMA index_list(q)").is_empty());
    conn.execute("INSERT INTO t1 VALUES(1,2,3),(4,5,6)").unwrap();
    conn.execute("INSERT INTO q VALUES(5,'x'),(2,'y')").unwrap();
    assert_eq!(rows(&conn, "SELECT c FROM t1 ORDER BY c"), ["3", "6"]);
    assert_eq!(rows(&conn, "SELECT rowid, c FROM q"), ["2|2", "5|5"]);
    assert_eq!(rows(&conn, "PRAGMA integrity_check"), ["ok"]);
}

/// D10: `analyzeOneTable()` stops testing for distinctness in a single-column UNIQUE index once it has seen a non-NULL
/// key ("we know that all the rest will be distinct"): the NULLs that follow the non-NULL keys of a DESC index count
/// as new values too.
#[test]
fn analyze_single_column_unique_index_with_nulls_follows_c() {
    let conn = Connection::open_in_memory().unwrap();
    for sql in [
        "CREATE TABLE t1(a0 INTEGER PRIMARY KEY, a1, a2 TEXT)",
        "CREATE UNIQUE INDEX i_desc ON t1(a2 DESC)",
        "CREATE UNIQUE INDEX i_asc ON t1(a1)",
        "CREATE TABLE w(k PRIMARY KEY, v, x) WITHOUT ROWID",
        "CREATE UNIQUE INDEX w_v ON w(v DESC)",
        "CREATE UNIQUE INDEX w_vx ON w(v, x)",
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<21) INSERT INTO t1 SELECT i, CASE WHEN i%4=0 THEN NULL ELSE i END, CASE WHEN i%3=0 THEN NULL ELSE 's'||i END FROM c",
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<21) INSERT INTO w SELECT i, CASE WHEN i%3=0 THEN NULL ELSE i END, NULL FROM c",
        "ANALYZE",
    ] {
        conn.execute(sql).unwrap();
    }
    assert_eq!(
        rows(&conn, "SELECT * FROM sqlite_stat1 ORDER BY 1,2"),
        ["t1|i_asc|21 2", "t1|i_desc|21 1", "w|w|21 1", "w|w_v|21 1", "w|w_vx|21 2 2"]
    );
    conn.execute("PRAGMA analysis_limit=5").unwrap();
    conn.execute("ANALYZE").unwrap();
    assert_eq!(
        rows(&conn, "SELECT * FROM sqlite_stat1 ORDER BY 1,2"),
        ["t1|i_asc|21 3", "t1|i_desc|21 1", "w|w|21 1", "w|w_v|21 1", "w|w_vx|21 2 2"]
    );
}
