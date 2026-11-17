use sqliters::{Connection, Statement, StepResult, Value};

fn rows(stmt: &mut Statement) -> Vec<Vec<Value>> {
    let mut out = Vec::new();
    while let StepResult::Row(row) = stmt.step().unwrap() {
        out.push(row.values().to_vec());
    }
    out
}

#[test]
fn nocase_nul_sort_distinct_equality_and_unique_follow_c() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'||char(0)||'z'),('a'||char(0)||'a');")).unwrap();
        let q = db
            .query(
                "SELECT a FROM t WHERE a LIKE 'a%' ORDER BY a COLLATE NOCASE",
                &[],
            )
            .unwrap();
        assert_eq!(q.rows[0].values()[0], Value::Text("a\0z".into()));
        assert_eq!(q.rows[1].values()[0], Value::Text("a\0a".into()));
        assert_eq!(
            db.query(
                "SELECT DISTINCT a COLLATE NOCASE FROM t WHERE a LIKE 'a%'",
                &[]
            )
            .unwrap()
            .rows
            .len(),
            1
        );
        assert_eq!(
            db.query(
                "SELECT a FROM t WHERE a COLLATE NOCASE='a'||char(0)||'a' AND a LIKE 'a%'",
                &[]
            )
            .unwrap()
            .rows
            .len(),
            2
        );
        let grouped = db
            .query("SELECT a,count(*) FROM t GROUP BY a COLLATE NOCASE", &[])
            .unwrap();
        assert_eq!(grouped.rows.len(), 1);
        assert_eq!(
            grouped.rows[0].values(),
            &[Value::Text("a\0z".into()), Value::Integer(2)]
        );
        db.execute_batch("CREATE TABLE u(a TEXT UNIQUE COLLATE NOCASE); INSERT INTO u VALUES('a'||char(0)||'z');").unwrap();
        let e = db
            .execute_batch("INSERT INTO u VALUES('a'||char(0)||'a');")
            .unwrap_err();
        assert_eq!(e.extended_code, 2067);
    }
}

#[test]
fn bound_utf16_nocase_index_keeps_c_counters() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; PRAGMA case_sensitive_like=OFF; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'),('A'),('abc'),('ABC'); CREATE INDEX nx ON t(a COLLATE NOCASE);")).unwrap();
        let mut q = db
            .prepare("SELECT a FROM t WHERE a LIKE ?1 ORDER BY a COLLATE NOCASE")
            .unwrap();
        q.bind(1, "a%").unwrap();
        assert_eq!(rows(&mut q).len(), 4);
        assert_eq!(q.machine_stmt_status(5, false), 1);
        assert!(q.machine_stmt_status(4, false) > 0);
        q.reset().unwrap();
        q.bind(1, Value::Null).unwrap();
        assert!(rows(&mut q).is_empty());
        assert_eq!(q.machine_stmt_status(5, false), 2);
    }
}

#[test]
fn unicode_range_bound_preserves_c_utf16be_result() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; PRAGMA case_sensitive_like=ON; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('߿'),('ࠀ'); CREATE INDEX bx ON t(a);")).unwrap();
        let mut q = db
            .prepare("SELECT a FROM t WHERE a LIKE ? ORDER BY a")
            .unwrap();
        q.bind(1, "߿%").unwrap();
        let got = rows(&mut q);
        assert_eq!(got[0][0], Value::Text("߿".into()));
        assert_eq!(got.len(), if enc == "UTF-16be" { 2 } else { 1 });
        if enc == "UTF-16be" {
            assert_eq!(got[1][0], Value::Text("ࠀ".into()));
        }
    }
}

#[test]
fn cast_text_uses_database_encoding_before_concatenation() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('a'); CREATE INDEX bx ON t(a);")).unwrap();
        let mut q = db.prepare("SELECT CAST(1 AS TEXT)||a, CAST(12 AS TEXT)||a, CAST(34 AS TEXT) FROM t WHERE a LIKE ?").unwrap();
        q.bind(1, "%").unwrap();
        assert_eq!(
            rows(&mut q),
            vec![vec![
                Value::Text("1a".into()),
                Value::Text("12a".into()),
                Value::Text("34".into())
            ]]
        );
    }
}

#[test]
fn utf16_numeric_casts_keep_c_prefix_and_type_rules() {
    for enc in ["UTF-8", "UTF-16le", "UTF-16be"] {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!("PRAGMA encoding='{enc}'; CREATE TABLE t(a TEXT); INSERT INTO t VALUES('12.5'),('1e2'),('1߿');")).unwrap();
        let mut q = db.prepare("SELECT CAST(a AS INTEGER), CAST(a AS REAL), CAST(a AS NUMERIC), a+0 FROM t WHERE a LIKE ? ORDER BY a").unwrap();
        q.bind(1, "%").unwrap();
        assert_eq!(
            rows(&mut q),
            vec![
                vec![
                    Value::Integer(12),
                    Value::Real(12.5),
                    Value::Real(12.5),
                    Value::Real(12.5)
                ],
                vec![
                    Value::Integer(1),
                    Value::Real(100.0),
                    Value::Integer(100),
                    Value::Real(100.0)
                ],
                vec![
                    Value::Integer(1),
                    Value::Real(1.0),
                    Value::Integer(1),
                    Value::Integer(1)
                ],
            ]
        );
    }
}
