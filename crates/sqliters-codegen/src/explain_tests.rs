//! `EXPLAIN` listings and compile errors of the reference C library (SQLite 3.54.1) for
//! statements of the class step V1 supports. `explain_cases.txt` was produced by the C
//! oracle `target/perf-work/v1/cdump/cexplain` (a program linked against the reference
//! amalgamation); every case must be reproduced byte for byte.

use crate::{Db, NoHost, PrepareResult, explain_text, prepare};

fn run(sql: &str) -> Vec<String> {
    match prepare(Db::new(&NoHost), sql) {
        PrepareResult::Program(prog) => explain_text(&prog),
        PrepareResult::Error { message, offset } => vec![format!("ERR {offset} {message}")],
        PrepareResult::Unsupported(why) => vec![format!("UNSUPPORTED {why}")],
    }
}

#[test]
fn explain_equals_c() {
    let data = include_str!("explain_cases.txt");
    let mut cases: Vec<(String, Vec<String>)> = Vec::new();
    for line in data.lines() {
        if let Some(sql) = line.strip_prefix("=== ") {
            cases.push((sql.to_owned(), Vec::new()));
        } else if let Some(c) = cases.last_mut() {
            c.1.push(line.to_owned());
        }
    }
    assert!(cases.len() > 300, "cases: {}", cases.len());
    let mut bad = Vec::new();
    let mut unsupported = 0;
    for (sql, want) in &cases {
        let got = run(sql);
        if got.first().is_some_and(|l| l.starts_with("UNSUPPORTED")) {
            unsupported += 1;
            continue;
        }
        if &got != want {
            bad.push(format!("{sql}\n  want: {want:?}\n  got:  {got:?}"));
        }
    }
    assert!(bad.is_empty(), "{} of {} statements differ from C:\n{}", bad.len(), cases.len(), bad.join("\n"));
    // The statements that fall back to the engine's old EXPLAIN are a small minority.
    assert!(unsupported * 10 < cases.len(), "unsupported: {unsupported} of {}", cases.len());
}

/// The first statement of the task description, spelled out.
#[test]
fn explain_select_with_in_list_and_factored_constants() {
    let got = run("explain select 1+?, abs(-3), 'a'||'b' where 2 in (1,2,3) order by 1 limit 5;");
    let want = [
        "0|Init|0|28|0||0|",
        "1|Noop|0|5|0||0|",
        "2|Integer|5|1|0||0|",
        "3|BeginSubrtn|0|2|0||0|",
        "4|Once|0|16|0||0|",
        "5|OpenEphemeral|1|1|0|k(1,)|0|",
        "6|Integer|1|3|0||0|",
        "7|MakeRecord|3|1|4|A|0|",
        "8|IdxInsert|1|4|3|1|0|",
        "9|Integer|2|3|0||0|",
        "10|MakeRecord|3|1|4|A|0|",
        "11|IdxInsert|1|4|3|1|0|",
        "12|Integer|3|3|0||0|",
        "13|MakeRecord|3|1|4|A|0|",
        "14|IdxInsert|1|4|3|1|0|",
        "15|NullRow|1|0|0||0|",
        "16|Return|2|4|1||0|",
        "17|Integer|2|5|0||0|",
        "18|NotFound|1|27|5|1|0|",
        "19|Add|10|9|6||0|",
        "20|Once|0|23|0||0|",
        "21|Integer|-3|12|0||0|",
        "22|Function|1|12|11|abs(1)|0|",
        "23|Copy|11|7|0||0|",
        "24|Concat|14|13|8||0|",
        "25|ResultRow|6|3|0||0|",
        "26|DecrJumpZero|1|27|0||0|",
        "27|Halt|0|0|0||0|",
        "28|Integer|1|9|0||0|",
        "29|Variable|1|10|0||0|",
        "30|String8|0|13|0|a|0|",
        "31|String8|0|14|0|b|0|",
        "32|Goto|0|1|0||0|",
    ];
    assert_eq!(got, want);
}

#[test]
fn program_sizing() {
    let PrepareResult::Program(p) = prepare(Db::new(&NoHost), "select ?3, :a, ?, :a, 1 in (select 2)") else {
        panic!("not compiled")
    };
    // ?3, :a (=4), ? (=5): nVar is 5; the name of parameter 4 is ":a"
    assert_eq!(p.n_var, 5);
    assert_eq!(p.p_vlist.num_to_name(4), Some(":a"));
    assert_eq!(p.n_cursor, 1);
    assert!(p.read_only && !p.b_is_reader);
    assert_eq!(p.explain, 0);
    let PrepareResult::Program(p) = prepare(Db::new(&NoHost), "begin immediate") else { panic!("not compiled") };
    assert!(!p.read_only && p.b_is_reader);
    assert_eq!(p.btree_mask, 0b11);
}

#[test]
fn unsupported_statements_are_reported_not_approximated() {
    for sql in ["create table t(x)", "insert into t values(1)", "pragma page_size"] {
        assert!(matches!(prepare(Db::new(&NoHost), sql), PrepareResult::Unsupported(_)), "{sql}");
    }
    // A table in FROM is compiled since step V2: with no schema it is C's error.
    let PrepareResult::Error { message, offset } = prepare(Db::new(&NoHost), "select a from t") else {
        panic!("select a from t")
    };
    assert_eq!((message.as_str(), offset), ("no such table: t", -1));
}

/// A window function without a FROM clause (step V4a): the listing of the C library.
#[test]
fn window_function_without_from() {
    let PrepareResult::Program(p) = prepare(Db::new(&NoHost), "EXPLAIN SELECT row_number() OVER ()") else {
        panic!("not compiled")
    };
    assert_eq!(
        crate::explain_text(&p),
        [
            "0|Init|0|44|0||0|",
            "1|Null|0|1|0||0|",
            "2|InitCoroutine|3|6|3||0|",
            "3|Integer|0|4|0||0|",
            "4|Yield|3|0|0||0|",
            "5|EndCoroutine|3|0|0||0|",
            "6|OpenEphemeral|0|1|0||0|",
            "7|OpenDup|1|0|0||0|",
            "8|OpenDup|2|0|0||0|",
            "9|OpenDup|3|0|0||0|",
            "10|Integer|1|5|0||0|",
            "11|InitCoroutine|3|0|3||0|",
            "12|Yield|3|29|0||0|",
            "13|Copy|4|7|0||2|",
            "14|MakeRecord|7|1|8||0|",
            "15|NewRowid|1|9|0||0|",
            "16|Insert|1|8|9||0|",
            "17|Ne|5|22|9||0|",
            "18|Null|0|1|0||0|",
            "19|Rewind|0|0|0||0|",
            "20|Rewind|3|0|0||0|",
            "21|Goto|0|28|0||0|",
            "22|AggStep|0|10|1|row_number(0)|0|",
            "23|Next|3|24|0||0|",
            "24|AggValue|1|0|2|row_number(0)|0|",
            "25|Gosub|6|40|0||0|",
            "26|Delete|0|0|0||2|",
            "27|Next|0|28|0||0|",
            "28|Goto|0|12|0||0|",
            "29|Rewind|1|38|0||0|",
            "30|AggStep|0|10|1|row_number(0)|0|",
            "31|Next|3|32|0||0|",
            "32|AggValue|1|0|2|row_number(0)|0|",
            "33|Gosub|6|40|0||0|",
            "34|Delete|0|0|0||2|",
            "35|Next|0|37|0||0|",
            "36|Goto|0|38|0||0|",
            "37|Goto|0|32|0||0|",
            "38|ResetSorter|0|0|0||0|",
            "39|Goto|0|43|0||0|",
            "40|Copy|2|10|0||0|",
            "41|ResultRow|10|1|0||0|",
            "42|Return|6|0|0||0|",
            "43|Halt|0|0|0||0|",
            "44|Goto|0|1|0||0|",
        ]
    );
}
