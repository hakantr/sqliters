//! The code generator over tables (step V2): the expected listings, query plans and planner
//! dumps below were produced by the reference C library (SQLite 3.54.1: `cexplain` /
//! `cexplain_where` of the scratch builds) for the schema
//!
//! ```sql
//! CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT DEFAULT 'x', c REAL, d);
//! CREATE INDEX t_b ON t(b);
//! CREATE TABLE u(x, y, z, UNIQUE(x,y));
//! CREATE INDEX t_cd ON t(c COLLATE nocase DESC, d) WHERE d IS NOT NULL;
//! CREATE VIEW v1 AS SELECT a, b AS bb, c+d AS s FROM t WHERE d>0;
//! ```
use std::sync::Arc;

use sqliters_vdbe::vdbeaux::FuncDef;

use crate::build_tab::SchemaRow;
use crate::{Db, DbInfo, Host, PrepareResult};

/// The schema above as the engine hands it over: the rows of `sqlite_schema`.
struct TestHost;

impl Host for TestHost {
    fn user_functions(&self, _name: &str) -> Vec<Arc<FuncDef>> {
        Vec::new()
    }
    fn find_collation(&self, _name: &str) -> Option<String> {
        None
    }
    fn schema_rows(&self, i_db: usize) -> Vec<SchemaRow> {
        if i_db != 0 {
            return Vec::new();
        }
        let row = |ty: &str, name: &str, tbl: &str, rootpage: u32, sql: Option<&str>| SchemaRow {
            ty: ty.to_owned(),
            name: name.to_owned(),
            tbl_name: tbl.to_owned(),
            rootpage,
            sql: sql.map(|s| s.to_owned()),
        };
        vec![
            row("table", "t", "t", 2, Some("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT DEFAULT 'x', c REAL, d)")),
            row("index", "t_b", "t", 3, Some("CREATE INDEX t_b ON t(b)")),
            row("table", "u", "u", 4, Some("CREATE TABLE u(x, y, z, UNIQUE(x,y))")),
            row("index", "sqlite_autoindex_u_1", "u", 5, None),
            row("index", "t_cd", "t", 6, Some("CREATE INDEX t_cd ON t(c COLLATE nocase DESC, d) WHERE d IS NOT NULL")),
            row("view", "v1", "v1", 0, Some("CREATE VIEW v1 AS SELECT a, b AS bb, c+d AS s FROM t WHERE d>0")),
        ]
    }
}

/// The connection state of the C run: five schema changes (cookie 5), nothing reset.
fn db(host: &TestHost) -> Db<'_> {
    let mut db = Db::new(host);
    db.a_db = vec![
        DbInfo { z_db_s_name: "main".into(), has_bt: true, file_format: 4, schema_cookie: 5, ..DbInfo::default() },
        DbInfo { z_db_s_name: "temp".into(), has_bt: false, file_format: 4, ..DbInfo::default() },
    ];
    db
}

/// The statement compiles to exactly C's listing (`EXPLAIN`: `addr|opcode|p1|p2|p3|p4|p5|`) or
/// query plan (`EXPLAIN QUERY PLAN`: `id|parent|notused|detail`).
fn check(sql: &str, want: &[&str]) {
    let host = TestHost;
    let prog = match crate::prepare(db(&host), sql) {
        PrepareResult::Program(prog) => prog,
        other => panic!("not compiled: {sql}: {other:?}"),
    };
    let got: Vec<String> = if prog.explain == 2 {
        prog.list(2)
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| match v {
                        sqliters_types::Value::Integer(i) => i.to_string(),
                        sqliters_types::Value::Text(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect()
    } else {
        crate::explain_text(&prog)
    };
    assert_eq!(got, want, "{sql}");
}

/// The planner's term / loop / path dump is exactly the dump of the instrumented C library.
fn check_where_dump(sql: &str, want: &[&str]) {
    let host = TestHost;
    let (r, _, dump) = crate::prepare_with_where_dump(db(&host), sql);
    assert!(matches!(r, PrepareResult::Program(_)), "{sql}");
    assert_eq!(dump, want, "{sql}");
}

#[test]
fn order_by_with_index_lookup() {
    check(
        "EXPLAIN SELECT b,c FROM t WHERE b='q' ORDER BY d",
        &[
            "0|Init|0|23|0||0|",
            "1|SorterOpen|1|4|0|k(1,B)|0|",
            "2|OpenRead|0|2|0|4|0|",
            "3|OpenRead|2|3|0|k(2,,)|2|",
            "4|String8|0|1|0|q|0|",
            "5|SeekGE|2|15|1|1|0|",
            "6|IdxGT|2|15|1|1|0|",
            "7|DeferredSeek|2|0|0||0|",
            "8|Column|2|0|3|x|0|",
            "9|Column|0|2|4||0|",
            "10|RealAffinity|4|0|0||0|",
            "11|Column|0|3|2||0|",
            "12|MakeRecord|2|3|5||0|",
            "13|SorterInsert|1|5|2|3|0|",
            "14|Next|2|6|1||0|",
            "15|OpenPseudo|3|6|4||0|",
            "16|SorterSort|1|22|0||0|",
            "17|SorterData|1|6|3||0|",
            "18|Column|3|2|4||0|",
            "19|Column|3|1|3||0|",
            "20|ResultRow|3|2|0||0|",
            "21|SorterNext|1|17|0||0|",
            "22|Halt|0|0|0||0|",
            "23|Transaction|0|0|5|0|1|",
            "24|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn join_with_rowid_lookup() {
    check(
        "EXPLAIN SELECT t.b, u.z FROM t JOIN u ON u.x=t.a WHERE u.y>3",
        &[
            "0|Init|0|13|0||0|",
            "1|OpenRead|1|4|0|3|0|",
            "2|OpenRead|0|2|0|2|0|",
            "3|Rewind|1|12|0||0|",
            "4|Column|1|1|1||0|",
            "5|Le|2|11|1|BINARY-8|81|",
            "6|Column|1|0|3||0|",
            "7|SeekRowid|0|11|3||0|",
            "8|Column|0|1|4|x|0|",
            "9|Column|1|2|5||0|",
            "10|ResultRow|4|2|0||0|",
            "11|Next|1|4|0||1|",
            "12|Halt|0|0|0||0|",
            "13|Transaction|0|0|5|0|1|",
            "14|Integer|3|2|0||0|",
            "15|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn count_star_uses_smallest_index() {
    check(
        "EXPLAIN SELECT count(*) FROM t",
        &[
            "0|Init|0|7|0||0|",
            "1|OpenRead|1|3|0|k(2,,)|0|",
            "2|Count|1|1|0||0|",
            "3|Close|1|0|0||0|",
            "4|Copy|1|2|0||0|",
            "5|ResultRow|2|1|0||0|",
            "6|Halt|0|0|0||0|",
            "7|Transaction|0|0|5|0|1|",
            "8|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn or_optimization() {
    check(
        "EXPLAIN SELECT * FROM t WHERE b IN (1,2,3) OR a=4",
        &[
            "0|Init|0|45|0||0|",
            "1|OpenRead|0|2|0|4|0|",
            "2|Null|0|2|0||0|",
            "3|Integer|36|1|0||0|",
            "4|ReopenIdx|1|3|0|k(2,,)|2|",
            "5|BeginSubrtn|0|5|0||0|",
            "6|Once|0|18|0||0|",
            "7|OpenEphemeral|2|1|0|k(1,B)|0|",
            "8|Integer|1|6|0||0|",
            "9|MakeRecord|6|1|7|B|0|",
            "10|IdxInsert|2|7|6|1|0|",
            "11|Integer|2|6|0||0|",
            "12|MakeRecord|6|1|7|B|0|",
            "13|IdxInsert|2|7|6|1|0|",
            "14|Integer|3|6|0||0|",
            "15|MakeRecord|6|1|7|B|0|",
            "16|IdxInsert|2|7|6|1|0|",
            "17|NullRow|2|0|0||0|",
            "18|Return|5|6|1||0|",
            "19|Rewind|2|31|0||0|",
            "20|Column|2|0|4||0|",
            "21|IsNull|4|30|0||0|",
            "22|Affinity|4|1|0|B|0|",
            "23|SeekGE|1|30|4|1|0|",
            "24|IdxGT|1|30|4|1|0|",
            "25|DeferredSeek|1|0|0|[0,1,0,0]|0|",
            "26|IdxRowid|1|3|0||0|",
            "27|RowSetTest|2|29|3|0|0|",
            "28|Gosub|1|37|0||0|",
            "29|Next|1|24|0||0|",
            "30|Next|2|20|0||0|",
            "31|Integer|4|8|0||0|",
            "32|SeekRowid|0|36|8||0|",
            "33|Rowid|0|3|0||0|",
            "34|RowSetTest|2|36|3|-1|0|",
            "35|Gosub|1|37|0||0|",
            "36|Goto|0|44|0||0|",
            "37|Rowid|0|9|0||0|",
            "38|Column|0|1|10|x|0|",
            "39|Column|0|2|11||0|",
            "40|RealAffinity|11|0|0||0|",
            "41|Column|0|3|12||0|",
            "42|ResultRow|9|4|0||0|",
            "43|Return|1|37|0||0|",
            "44|Halt|0|0|0||0|",
            "45|Transaction|0|0|5|0|1|",
            "46|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn left_join_of_a_flattened_view() {
    check(
        "EXPLAIN SELECT * FROM u LEFT JOIN v1 ON u.x=v1.a",
        &[
            "0|Init|0|28|0||0|",
            "1|OpenRead|0|4|0|3|0|",
            "2|OpenRead|2|2|0|4|0|",
            "3|Rewind|0|27|0||0|",
            "4|Integer|0|1|0||0|",
            "5|Column|0|0|2||0|",
            "6|SeekRowid|2|23|2||0|",
            "7|Column|2|3|3||0|",
            "8|Integer|0|4|0||0|",
            "9|Le|4|23|3|BINARY-8|81|",
            "10|Integer|1|1|0||0|",
            "11|Column|0|0|5||0|",
            "12|Column|0|1|6||0|",
            "13|Column|0|2|7||0|",
            "14|Rowid|2|8|0||0|",
            "15|Column|2|1|9|x|0|",
            "16|IfNullRow|2|21|10||0|",
            "17|Column|2|2|4||0|",
            "18|RealAffinity|4|0|0||0|",
            "19|Column|2|3|3||0|",
            "20|Add|3|4|10||0|",
            "21|ClrSubtype|10|0|0||0|",
            "22|ResultRow|5|6|0||0|",
            "23|IfPos|1|26|0||0|",
            "24|NullRow|2|0|0||0|",
            "25|Goto|0|10|0||0|",
            "26|Next|0|4|0||1|",
            "27|Halt|0|0|0||0|",
            "28|Transaction|0|0|5|0|1|",
            "29|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn exists_to_join() {
    check(
        "EXPLAIN SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.x=t.a)",
        &[
            "0|Init|0|19|0||0|",
            "1|IfNot|1|18|1||0|",
            "2|OpenRead|0|2|0|4|0|",
            "3|OpenRead|3|5|0|k(3,,,)|0|",
            "4|Rewind|0|18|0||0|",
            "5|Rewind|3|17|2|0|0|",
            "6|Column|3|0|2||0|",
            "7|Rowid|0|3|0||0|",
            "8|Ne|3|16|2|BINARY-8|83|",
            "9|Rowid|0|4|0||0|",
            "10|Column|0|1|5|x|0|",
            "11|Column|0|2|6||0|",
            "12|RealAffinity|6|0|0||0|",
            "13|Column|0|3|7||0|",
            "14|ResultRow|4|4|0||0|",
            "15|Goto|0|17|0||0|",
            "16|Next|3|6|0||1|",
            "17|Next|0|5|0||1|",
            "18|Halt|0|0|0||0|",
            "19|Transaction|0|0|5|0|1|",
            "20|Integer|1|1|0||0|",
            "21|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn compound_with_order_by() {
    check(
        "EXPLAIN SELECT a FROM t UNION SELECT x FROM u ORDER BY 1",
        &[
            "0|Init|0|49|0||0|",
            "1|Integer|0|1|0||0|",
            "2|InitCoroutine|3|10|3||0|",
            "3|Noop|2|3|0||0|",
            "4|OpenRead|1|2|0|0|0|",
            "5|Rewind|1|9|0||0|",
            "6|Rowid|1|7|0||0|",
            "7|Yield|3|0|0||0|",
            "8|Next|1|6|0||1|",
            "9|EndCoroutine|3|0|0||0|",
            "10|InitCoroutine|4|44|11||0|",
            "11|Noop|3|3|0||0|",
            "12|OpenRead|4|5|0|k(3,,,)|0|",
            "13|Rewind|4|17|8|0|0|",
            "14|Column|4|0|8||0|",
            "15|Yield|4|0|0||0|",
            "16|Next|4|14|0||1|",
            "17|EndCoroutine|4|0|0||0|",
            "18|IfNot|1|21|0||0|",
            "19|Compare|7|2|1|k(1,B)|0|",
            "20|Jump|21|24|21||0|",
            "21|Copy|7|2|0||0|",
            "22|Integer|1|1|0||0|",
            "23|ResultRow|7|1|0||0|",
            "24|Return|5|0|0||0|",
            "25|IfNot|1|28|0||0|",
            "26|Compare|8|2|1|k(1,B)|0|",
            "27|Jump|28|31|28||0|",
            "28|Copy|8|2|0||0|",
            "29|Integer|1|1|0||0|",
            "30|ResultRow|8|1|0||0|",
            "31|Return|6|0|0||0|",
            "32|Gosub|6|25|0||0|",
            "33|Yield|4|48|0||0|",
            "34|Goto|0|32|0||0|",
            "35|Gosub|5|18|0||0|",
            "36|Yield|3|48|0||0|",
            "37|Goto|0|35|0||0|",
            "38|Gosub|5|18|0||0|",
            "39|Yield|3|32|0||0|",
            "40|Goto|0|46|0||0|",
            "41|Gosub|6|25|0||0|",
            "42|Yield|4|35|0||0|",
            "43|Goto|0|46|0||0|",
            "44|Yield|3|33|0||0|",
            "45|Yield|4|35|0||0|",
            "46|Compare|7|8|1|k(2,B,)|0|",
            "47|Jump|38|39|41||0|",
            "48|Halt|0|0|0||0|",
            "49|Transaction|0|0|5|0|1|",
            "50|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn partial_index_with_collation() {
    check(
        "EXPLAIN SELECT a FROM t WHERE d IS NOT NULL AND c='abc' COLLATE nocase",
        &[
            "0|Init|0|12|0||0|",
            "1|OpenRead|1|6|0|k(3,-NOCASE,,)|0|",
            "2|String8|0|1|0|abc|0|",
            "3|IsNull|1|11|0||0|",
            "4|Null|0|2|0||0|",
            "5|Affinity|1|1|0|C|0|",
            "6|SeekGT|1|11|1|2|0|",
            "7|IdxGT|1|11|1|1|0|",
            "8|IdxRowid|1|3|0||0|",
            "9|ResultRow|3|1|0||0|",
            "10|Next|1|7|0||0|",
            "11|Halt|0|0|0||0|",
            "12|Transaction|0|0|5|0|1|",
            "13|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn in_subquery_uses_index_of_rhs() {
    check(
        "EXPLAIN SELECT * FROM t WHERE a IN (SELECT x FROM u)",
        &[
            "0|Init|0|25|0||0|",
            "1|OpenRead|0|2|0|4|0|",
            "2|BeginSubrtn|0|2|0|subrtnsig:1,C|0|",
            "3|Once|0|12|0||0|",
            "4|OpenEphemeral|2|1|0|k(1,B)|0|",
            "5|OpenRead|3|5|0|k(3,,,)|0|",
            "6|Rewind|3|11|3|0|0|",
            "7|Column|3|0|3||0|",
            "8|MakeRecord|3|1|4|C|0|",
            "9|IdxInsert|2|4|3|1|0|",
            "10|Next|3|7|0||1|",
            "11|NullRow|2|0|0||0|",
            "12|Return|2|3|1||0|",
            "13|Rewind|2|24|0||0|",
            "14|Column|2|0|1||0|",
            "15|IsNull|1|23|0||0|",
            "16|SeekRowid|0|23|1||0|",
            "17|Rowid|0|5|0||0|",
            "18|Column|0|1|6|x|0|",
            "19|Column|0|2|7||0|",
            "20|RealAffinity|7|0|0||0|",
            "21|Column|0|3|8||0|",
            "22|ResultRow|5|4|0||0|",
            "23|Next|2|14|0||0|",
            "24|Halt|0|0|0||0|",
            "25|Transaction|0|0|5|0|1|",
            "26|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn eqp_join() {
    check(
        "EXPLAIN QUERY PLAN SELECT t.b, u.z FROM t JOIN u ON u.x=t.a WHERE u.y>3",
        &[
            "3|0|216|SCAN u",
            "7|0|45|SEARCH t USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    );
}

#[test]
fn eqp_or_optimization() {
    check(
        "EXPLAIN QUERY PLAN SELECT * FROM t WHERE b IN (1,2,3) OR a=4",
        &[
            "4|0|0|MULTI-INDEX OR",
            "5|4|0|INDEX 1",
            "28|5|78|SEARCH t USING INDEX t_b (b=?)",
            "34|4|0|INDEX 2",
            "37|34|45|SEARCH t USING INTEGER PRIMARY KEY (rowid=?)",
        ],
    );
}

#[test]
fn eqp_subqueries() {
    check(
        "EXPLAIN QUERY PLAN SELECT (SELECT max(z) FROM u WHERE u.x=t.a), a IN (SELECT y FROM u WHERE z>1) FROM t ORDER BY d",
        &[
            "3|0|216|SCAN t",
            "6|0|0|CORRELATED SCALAR SUBQUERY 1",
            "11|6|216|SEARCH u",
            "29|0|0|LIST SUBQUERY 2",
            "32|29|216|SCAN u",
            "40|29|0|CREATE BLOOM FILTER",
            "62|0|0|USE TEMP B-TREE FOR ORDER BY",
        ],
    );
}

#[test]
fn eqp_compound() {
    check(
        "EXPLAIN QUERY PLAN SELECT a FROM t UNION SELECT x FROM u ORDER BY 1",
        &[
            "2|0|0|MERGE (UNION)",
            "4|2|0|LEFT",
            "7|4|216|SCAN t",
            "14|2|0|RIGHT",
            "17|14|214|SCAN u USING COVERING INDEX sqlite_autoindex_u_1",
        ],
    );
}

#[test]
fn eqp_materialized_subquery() {
    check(
        "EXPLAIN QUERY PLAN SELECT * FROM (SELECT DISTINCT b FROM t) JOIN u ON x=b",
        &[
            "2|0|0|CO-ROUTINE (subquery-1)",
            "5|2|214|SCAN t USING COVERING INDEX t_b",
            "15|0|186|SCAN (subquery-1)",
            "18|0|62|SEARCH u USING INDEX sqlite_autoindex_u_1 (x=?)",
        ],
    );
}

#[test]
fn planner_dump_of_a_join() {
    check_where_dump(
        "EXPLAIN SELECT t.b, u.z FROM t JOIN u ON u.x=t.a WHERE u.y>3",
        &[
            "WC nTerm=3 nBase=2",
            "TERM 0 top=55 op=4 cur=1 col=1 prR=0 prA=2 fl=0 prob=1 parent=-1 nChild=0 iField=0",
            "TERM 1 top=54 op=2 cur=1 col=0 prR=1 prA=3 fl=8 prob=1 parent=-1 nChild=1 iField=0",
            "TERM 2 top=54 op=2 cur=0 col=-1 prR=2 prA=3 fl=3 prob=1 parent=1 nChild=0 iField=0",
            "LOOP add tab=0 idx=- ws=100 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=216 nOut=200 prereq=0",
            "LOOP add tab=0 idx=- ws=1101 nEq=1 nBtm=0 nTop=0 nSkip=0 nLTerm=1 iSortIdx=0 rSetup=0 rRun=45 nOut=0 prereq=2",
            "LOOP replace tab=0 idx=- ws=100 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=216 nOut=200 prereq=0",
            "LOOP with tab=0 idx=t_b ws=240 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=214 nOut=200 prereq=0",
            "LOOP add tab=1 idx=- ws=800100 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=216 nOut=199 prereq=0",
            "PATH est=0 nRow=199 rCost=246 rUnsort=244 isOrdered=0 rev=0 nOBSat=0 eDistinct=0 loops=[1:-:800100,0:-:1101]",
        ],
    );
}

// ---- step V3: GROUP BY over tables, FILTER, recursive common table expressions ----

/// What is still not ported is reported, never approximated.
#[test]
fn unsupported_in_v3() {
    let host = TestHost;
    for sql in ["select sum(a) over () from t", "select * from pragma_table_info('t')"] {
        assert!(matches!(crate::prepare(db(&host), sql), PrepareResult::Unsupported(_)), "{sql}");
    }
}
#[test]
fn group_by_in_index_order() {
    check(
        "EXPLAIN SELECT b, count(*) FROM t GROUP BY b",
        &[
            "0|Init|0|33|0||0|",
            "1|Noop|1|1|0||0|",
            "2|Integer|0|2|0||0|",
            "3|Null|0|5|5||0|",
            "4|Gosub|4|29|0||0|",
            "5|OpenRead|2|3|0|k(2,,)|0|",
            "6|Rewind|2|19|7|0|0|",
            "7|Column|2|0|6|x|0|",
            "8|Compare|5|6|1|k(1,B)|0|",
            "9|Jump|10|14|10||0|",
            "10|Gosub|3|23|0||0|",
            "11|Move|6|5|1||0|",
            "12|IfPos|2|32|0||0|",
            "13|Gosub|4|29|0||0|",
            "14|AggStep|0|0|8|count(0)|0|",
            "15|If|1|17|0||0|",
            "16|Column|2|0|7|x|0|",
            "17|Integer|1|1|0||0|",
            "18|Next|2|7|0||1|",
            "19|Gosub|3|23|0||0|",
            "20|Goto|0|32|0||0|",
            "21|Integer|1|2|0||0|",
            "22|Return|3|0|0||0|",
            "23|IfPos|1|25|0||0|",
            "24|Return|3|0|0||0|",
            "25|AggFinal|8|0|0|count(0)|0|",
            "26|Copy|7|9|1||0|",
            "27|ResultRow|9|2|0||0|",
            "28|Return|3|0|0||0|",
            "29|Null|0|7|8||0|",
            "30|Integer|0|1|0||0|",
            "31|Return|4|0|0||0|",
            "32|Halt|0|0|0||0|",
            "33|Transaction|0|0|5|0|1|",
            "34|Null|0|5|5||0|",
            "35|Goto|0|1|0||0|",
        ],
    );
    // the planner and aggregate-info dump of the instrumented C library
    check_where_dump(
        "EXPLAIN SELECT b, count(*) FROM t GROUP BY b",
        &[
            "WC nTerm=0 nBase=0",
            "LOOP add tab=0 idx=- ws=100 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=216 nOut=200 prereq=0",
            "LOOP add tab=0 idx=t_b ws=240 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=3 rSetup=0 rRun=214 nOut=200 prereq=0",
            "PATH est=0 nRow=200 rCost=214 rUnsort=212 isOrdered=0 rev=0 nOBSat=0 eDistinct=0 loops=[0:t_b:240]",
            "PATH est=201 nRow=200 rCost=214 rUnsort=212 isOrdered=1 rev=0 nOBSat=1 eDistinct=0 loops=[0:t_b:240]",
            "AGG groupby sel=1 nCol=1 nAcc=1 nFunc=1 nSortCol=1 sortIdx=1 sortPTab=0 useSortIdx=0 firstReg=7 x=[0,0,0,0] nWhere=0 nHaving=0 nGroupBy=1",
            "AGGCOL 0 tab=t iTable=0 iColumn=1 iSorter=0 op=170",
            "AGGFUNC 0 name=count nArg=0 iDistinct=-1 iOBTab=-1 bOBPayload=0 bOBUnique=0 bUseSubtype=0 filter=0",
        ],
    );
}

#[test]
fn group_by_with_sorter_filter_and_having() {
    check(
        "EXPLAIN SELECT d, sum(c) FILTER (WHERE a>1) FROM t GROUP BY d HAVING count(*)>1",
        &[
            "0|Init|0|51|0||0|",
            "1|SorterOpen|1|3|0|k(1,B)|0|",
            "2|Integer|0|2|0||0|",
            "3|Null|0|5|5||0|",
            "4|Gosub|4|47|0||0|",
            "5|OpenRead|0|2|0|4|0|",
            "6|Rewind|0|14|0||0|",
            "7|Column|0|3|12||0|",
            "8|Column|0|2|13||0|",
            "9|RealAffinity|13|0|0||0|",
            "10|Rowid|0|14|0||0|",
            "11|MakeRecord|12|3|15||0|",
            "12|SorterInsert|1|15|0||0|",
            "13|Next|0|7|0||1|",
            "14|OpenPseudo|2|15|3||0|",
            "15|SorterSort|1|50|0||0|",
            "16|SorterData|1|15|2||0|",
            "17|Column|2|0|6||0|",
            "18|Compare|5|6|1|k(1,B)|0|",
            "19|Jump|20|24|20||0|",
            "20|Gosub|3|38|0||0|",
            "21|Move|6|5|1||0|",
            "22|IfPos|2|50|0||0|",
            "23|Gosub|4|47|0||0|",
            "24|Column|2|2|16||0|",
            "25|Le|17|29|16||84|",
            "26|Column|2|1|16||0|",
            "27|RealAffinity|16|0|0||0|",
            "28|AggStep|0|16|10|sum(1)|1|",
            "29|AggStep|0|0|11|count(0)|0|",
            "30|If|1|32|0||0|",
            "31|Column|2|0|7||0|",
            "32|Integer|1|1|0||0|",
            "33|SorterNext|1|16|0||0|",
            "34|Gosub|3|38|0||0|",
            "35|Goto|0|50|0||0|",
            "36|Integer|1|2|0||0|",
            "37|Return|3|0|0||0|",
            "38|IfPos|1|40|0||0|",
            "39|Return|3|0|0||0|",
            "40|AggFinal|10|1|0|sum(1)|0|",
            "41|AggFinal|11|0|0|count(0)|0|",
            "42|Le|17|39|11||80|",
            "43|Copy|7|18|0||0|",
            "44|Copy|10|19|0||0|",
            "45|ResultRow|18|2|0||0|",
            "46|Return|3|0|0||0|",
            "47|Null|0|7|11||0|",
            "48|Integer|0|1|0||0|",
            "49|Return|4|0|0||0|",
            "50|Halt|0|0|0||0|",
            "51|Transaction|0|0|5|0|1|",
            "52|Null|0|5|5||0|",
            "53|Integer|1|17|0||0|",
            "54|Goto|0|1|0||0|",
        ],
    );
    // the planner and aggregate-info dump of the instrumented C library
    check_where_dump(
        "EXPLAIN SELECT d, sum(c) FILTER (WHERE a>1) FROM t GROUP BY d HAVING count(*)>1",
        &[
            "WC nTerm=0 nBase=0",
            "LOOP add tab=0 idx=- ws=100 nEq=0 nBtm=0 nTop=0 nSkip=0 nLTerm=0 iSortIdx=0 rSetup=0 rRun=216 nOut=200 prereq=0",
            "PATH est=0 nRow=200 rCost=216 rUnsort=214 isOrdered=0 rev=0 nOBSat=0 eDistinct=0 loops=[0:-:100]",
            "PATH est=201 nRow=200 rCost=258 rUnsort=216 isOrdered=0 rev=0 nOBSat=0 eDistinct=0 loops=[0:-:100]",
            "AGG groupby sel=1 nCol=3 nAcc=1 nFunc=2 nSortCol=3 sortIdx=1 sortPTab=2 useSortIdx=1 firstReg=7 x=[1,0,0,0] nWhere=0 nHaving=1 nGroupBy=1",
            "AGGCOL 0 tab=t iTable=0 iColumn=3 iSorter=0 op=170",
            "AGGCOL 1 tab=t iTable=0 iColumn=2 iSorter=1 op=170",
            "AGGCOL 2 tab=t iTable=0 iColumn=-1 iSorter=2 op=170",
            "AGGFUNC 0 name=sum nArg=1 iDistinct=-1 iOBTab=-1 bOBPayload=0 bOBUnique=0 bUseSubtype=0 filter=1",
            "AGGFUNC 1 name=count nArg=0 iDistinct=-1 iOBTab=-1 bOBPayload=0 bOBUnique=0 bUseSubtype=0 filter=0",
        ],
    );
}

#[test]
fn recursive_cte_listing() {
    check(
        "EXPLAIN WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM c WHERE n<5) SELECT n FROM c",
        &[
            "0|Init|0|29|0||0|",
            "1|InitCoroutine|1|23|2||0|",
            "2|OpenPseudo|1|2|1||0|",
            "3|OpenEphemeral|2|1|0||0|",
            "4|Integer|1|3|0||0|",
            "5|MakeRecord|3|1|4||0|",
            "6|NewRowid|2|5|0||0|",
            "7|Insert|2|4|5||8|",
            "8|Rewind|2|22|0||0|",
            "9|NullRow|1|0|0||0|",
            "10|RowData|2|2|0||0|",
            "11|Delete|2|0|0||0|",
            "12|Column|1|0|6||0|",
            "13|Yield|1|0|0||0|",
            "14|Column|1|0|4||0|",
            "15|Ge|7|21|4|BINARY-8|80|",
            "16|Column|1|0|4||0|",
            "17|Add|8|4|3||0|",
            "18|MakeRecord|3|1|4||0|",
            "19|NewRowid|2|5|0||0|",
            "20|Insert|2|4|5||8|",
            "21|Goto|0|8|0||0|",
            "22|EndCoroutine|1|0|0||0|",
            "23|InitCoroutine|1|0|2||0|",
            "24|Yield|1|28|0||0|",
            "25|Copy|6|9|0||2|",
            "26|ResultRow|9|1|0||0|",
            "27|Goto|0|24|0||0|",
            "28|Halt|0|0|0||0|",
            "29|Integer|5|7|0||0|",
            "30|Integer|1|8|0||0|",
            "31|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn recursive_cte_query_plan() {
    check(
        "EXPLAIN QUERY PLAN WITH RECURSIVE c(n) AS (SELECT x FROM u UNION SELECT n+1 FROM c, t WHERE t.a=c.n ORDER BY 1) SELECT n FROM c WHERE n IN (SELECT a FROM t)",
        &[
            "2|0|0|CO-ROUTINE c",
            "6|2|0|SETUP",
            "8|6|214|SCAN u USING COVERING INDEX sqlite_autoindex_u_1",
            "25|2|0|RECURSIVE STEP",
            "27|25|216|SCAN c",
            "28|25|45|SEARCH t USING INTEGER PRIMARY KEY (rowid=?)",
            "42|0|186|SCAN c",
            "47|0|0|USING ROWID SEARCH ON TABLE t FOR IN-OPERATOR",
        ],
    );
}
