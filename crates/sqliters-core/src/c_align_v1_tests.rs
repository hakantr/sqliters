//! C-alignment regression tests (step V1: `EXPLAIN <stmt>` lists the program C's code generator builds, when the port
//! of the generator (`sqliters-codegen`) supports the whole statement; otherwise the engine's previous output is kept).
//! Every expected listing / error is what the C library SQLite 3.54.1 returns for the same statement (produced by
//! target/perf-work/v1/cdump/cexplain). Nothing here depends on files outside the repository.
use crate::Connection;
use sqliters_types::Value;

/// The rows of a statement as text `a|b|c` (NULL is empty), or `ERR <offset> <message>` if it fails to prepare.
fn explain(conn: &Connection, sql: &str) -> Vec<String> {
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

#[test]
fn explain_expression_with_in_list_and_limit() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain select ?1 + 2 where :a in (1,2,3) limit 3;");
    let want: [&str; 26] = [
        "0|Init|0|23|0||0|",
        "1|Integer|3|1|0||0|",
        "2|BeginSubrtn|0|2|0||0|",
        "3|Once|0|15|0||0|",
        "4|OpenEphemeral|0|1|0|k(1,)|0|",
        "5|Integer|1|3|0||0|",
        "6|MakeRecord|3|1|4|A|0|",
        "7|IdxInsert|0|4|3|1|0|",
        "8|Integer|2|3|0||0|",
        "9|MakeRecord|3|1|4|A|0|",
        "10|IdxInsert|0|4|3|1|0|",
        "11|Integer|3|3|0||0|",
        "12|MakeRecord|3|1|4|A|0|",
        "13|IdxInsert|0|4|3|1|0|",
        "14|NullRow|0|0|0||0|",
        "15|Return|2|3|1||0|",
        "16|Variable|2|5|0||0|",
        "17|IsNull|5|22|0||0|",
        "18|NotFound|0|22|5|1|0|",
        "19|Add|8|7|6||0|",
        "20|ResultRow|6|1|0||0|",
        "21|DecrJumpZero|1|22|0||0|",
        "22|Halt|0|0|0||0|",
        "23|Variable|1|7|0||0|",
        "24|Integer|2|8|0||0|",
        "25|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_aggregates_with_having() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain select count(*), max(:x) having count(*) > 0;");
    let want: [&str; 14] = [
        "0|Init|0|12|0||0|",
        "1|Null|0|1|2||0|",
        "2|AggStep|0|0|1|count(0)|0|",
        "3|Variable|1|3|0||0|",
        "4|CollSeq|0|0|0|BINARY-8|0|",
        "5|AggStep|0|3|2|max(1)|1|",
        "6|AggFinal|1|0|0|count(0)|0|",
        "7|AggFinal|2|1|0|max(1)|0|",
        "8|Le|4|11|1||80|",
        "9|Copy|1|5|1||0|",
        "10|ResultRow|5|2|0||0|",
        "11|Halt|0|0|0||0|",
        "12|Integer|0|4|0||0|",
        "13|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_multi_row_values_coroutine() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain values (1,2),(3,4);");
    let want: [&str; 17] = [
        "0|Init|0|16|0||0|",
        "1|InitCoroutine|1|9|2||0|",
        "2|Integer|1|4|0||0|",
        "3|Integer|2|5|0||0|",
        "4|Yield|1|0|0||0|",
        "5|Integer|3|4|0||0|",
        "6|Integer|4|5|0||0|",
        "7|Yield|1|0|0||0|",
        "8|EndCoroutine|1|0|0||0|",
        "9|InitCoroutine|1|0|2||0|",
        "10|Yield|1|15|0||0|",
        "11|Copy|4|8|0||2|",
        "12|Copy|5|9|0||2|",
        "13|ResultRow|8|2|0||0|",
        "14|Goto|0|10|0||0|",
        "15|Halt|0|0|0||0|",
        "16|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_compound_union_by_merge() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain select 1 union select 2 order by 1;");
    let want: [&str; 44] = [
        "0|Init|0|43|0||0|",
        "1|Integer|0|1|0||0|",
        "2|InitCoroutine|3|7|3||0|",
        "3|Noop|0|3|0||0|",
        "4|Integer|1|7|0||0|",
        "5|Yield|3|0|0||0|",
        "6|EndCoroutine|3|0|0||0|",
        "7|InitCoroutine|4|38|8||0|",
        "8|Noop|1|3|0||0|",
        "9|Integer|2|8|0||0|",
        "10|Yield|4|0|0||0|",
        "11|EndCoroutine|4|0|0||0|",
        "12|IfNot|1|15|0||0|",
        "13|Compare|7|2|1|k(1,)|0|",
        "14|Jump|15|18|15||0|",
        "15|Copy|7|2|0||0|",
        "16|Integer|1|1|0||0|",
        "17|ResultRow|7|1|0||0|",
        "18|Return|5|0|0||0|",
        "19|IfNot|1|22|0||0|",
        "20|Compare|8|2|1|k(1,)|0|",
        "21|Jump|22|25|22||0|",
        "22|Copy|8|2|0||0|",
        "23|Integer|1|1|0||0|",
        "24|ResultRow|8|1|0||0|",
        "25|Return|6|0|0||0|",
        "26|Gosub|6|19|0||0|",
        "27|Yield|4|42|0||0|",
        "28|Goto|0|26|0||0|",
        "29|Gosub|5|12|0||0|",
        "30|Yield|3|42|0||0|",
        "31|Goto|0|29|0||0|",
        "32|Gosub|5|12|0||0|",
        "33|Yield|3|26|0||0|",
        "34|Goto|0|40|0||0|",
        "35|Gosub|6|19|0||0|",
        "36|Yield|4|29|0||0|",
        "37|Goto|0|40|0||0|",
        "38|Yield|3|27|0||0|",
        "39|Yield|4|29|0||0|",
        "40|Compare|7|8|1|k(2,B,)|0|",
        "41|Jump|32|33|35||0|",
        "42|Halt|0|0|0||0|",
        "43|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_unknown_function_error() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain select nosuch(1);");
    let want: [&str; 1] = [
        "ERR 15 no such function: nosuch",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_syntax_error() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain select 1 +;");
    let want: [&str; 1] = [
        "ERR 18 near \";\": syntax error",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_begin_immediate() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain begin immediate;");
    let want: [&str; 6] = [
        "0|Init|0|5|0||0|",
        "1|Transaction|0|1|0||0|",
        "2|Transaction|1|1|0||0|",
        "3|AutoCommit|0|0|0||0|",
        "4|Halt|0|0|0||0|",
        "5|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn explain_savepoint_quoted_name() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "explain savepoint \"a b\";");
    let want: [&str; 4] = [
        "0|Init|0|3|0||0|",
        "1|Savepoint|0|0|0|a b|0|",
        "2|Halt|0|0|0||0|",
        "3|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

/// A statement the port of the generator does not support yet keeps the engine's previous `EXPLAIN` output
/// (whatever it is): the query runs and returns the eight EXPLAIN columns.
#[test]
fn explain_of_unsupported_statement_falls_back() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t(a, b)").unwrap();
    let res = conn.query("EXPLAIN SELECT a FROM t WHERE b = 1", &[]).unwrap();
    assert_eq!(res.column_names, ["addr", "opcode", "p1", "p2", "p3", "p4", "p5", "comment"]);
    assert!(!res.rows.is_empty());
}

/// Executing statements is untouched: only `EXPLAIN` consults the generator.
#[test]
fn execution_is_not_affected() {
    let conn = Connection::open_in_memory().unwrap();
    let res = conn.query("SELECT 1 + ?1 WHERE 2 IN (1, 2, 3)", &[Value::Integer(4)]).unwrap();
    assert_eq!(res.rows.len(), 1);
    assert_eq!(res.rows[0].values()[0], Value::Integer(5));
}

/// `EXPLAIN` of a statement with parameters prepares with C's parameter count, also when the engine's own parser
/// does not accept the statement (aggregate ORDER BY inside a FROM-less SELECT).
#[test]
fn explain_of_statement_the_old_parser_rejects() {
    let conn = Connection::open_in_memory().unwrap();
    let got = explain(&conn, "EXPLAIN SELECT group_concat(?2, ',' ORDER BY 1)");
    assert!(got[0].starts_with("0|Init|0|"), "{got:?}");
    assert!(got.iter().any(|l| l.contains("|AggStep|")), "{got:?}");
}

/// The P4 text of `OP_Blob` is the blob's bytes. Bytes that are not valid UTF-8 are listed exactly: the engine's TEXT
/// value cannot hold them, so that one cell is a BLOB (C: TEXT with the same bytes; both shells print the same bytes).
#[test]
fn explain_blob_literal_keeps_its_bytes() {
    let conn = Connection::open_in_memory().unwrap();
    let res = conn.query("explain select x'41abcd42', x'c3a9';", &[]).unwrap();
    let p4: Vec<Value> = res.rows.iter().map(|r| r.values()[5].clone()).collect();
    assert_eq!(p4[1], Value::Blob(vec![0x41, 0xab, 0xcd, 0x42]));
    assert_eq!(p4[2], Value::Text("\u{e9}".to_owned()));
    let ops: Vec<Value> = res.rows.iter().map(|r| r.values()[1].clone()).collect();
    assert_eq!(ops[1], Value::Text("Blob".to_owned()));
    assert_eq!(ops[2], Value::Text("Blob".to_owned()));
}
