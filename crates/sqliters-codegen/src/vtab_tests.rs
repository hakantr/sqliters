//! Virtual tables and table-valued functions (step V4b). The expected listings, plans and
//! xBestIndex dumps were produced by the reference C library (SQLite 3.54.1, the scratch build
//! `cexplain_v4b` with the shell's `ext/misc/series.c` registered, `V2WHERE=1`) for the schema of
//! `plan_tests.rs`. The test host's `generate_series` answers xBestIndex as `seriesBestIndex()`
//! does; the address C lists for `P4_VTAB` (`vtab:%p`) is the host's instance id here.
use std::sync::Arc;

use sqliters_types::Value;
use sqliters_vdbe::vdbeaux::FuncDef;

use crate::build_tab::SchemaRow;
use crate::sqliteint::{VTable, VtabConnect, VtabModule};
use crate::vtab::{IndexInfo, IndexInfoHidden};
use crate::{Db, DbInfo, Host, PrepareResult};

struct VtHost;

const SERIES_ID: u64 = 0x5C0B_3D35_4148;

impl Host for VtHost {
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
    fn vtab_module(&self, name: &str, _lazy: bool) -> Option<VtabModule> {
        name.eq_ignore_ascii_case("generate_series").then(|| VtabModule {
            name: "generate_series".to_owned(),
            eponymous: true,
            shadow_names: Vec::new(),
        })
    }
    fn vtab_connect(&self, _i_db: usize, _table: &str, _module: &str, _args: &[String], _eponymous: bool) -> VtabConnect {
        VtabConnect::Ok {
            vtab: VTable { id: SERIES_ID, module_name: "generate_series".to_owned(), e_vtab_risk: 0, ..VTable::default() },
            declare: Some("CREATE TABLE x(value,start hidden,stop hidden,step hidden)".to_owned()),
        }
    }
    /// `seriesBestIndex()` (shell.c of 3.54.1)
    fn vtab_best_index(&self, _id: u64, p: &mut IndexInfo, _h: &mut dyn IndexInfoHidden) -> (i32, Option<String>) {
        let mut idx_num = 0;
        let mut b_start_seen = false;
        let mut unusable = 0;
        let mut a_idx = [-1i64; 7];
        for (i, c) in p.a_constraint.iter().enumerate() {
            let op = c.op;
            if (73..=74).contains(&op) {
                if c.usable {
                    if op == 73 {
                        a_idx[3] = i as i64;
                        idx_num |= 0x20;
                    } else {
                        a_idx[4] = i as i64;
                        idx_num |= 0x40;
                    }
                }
                continue;
            }
            if c.i_column < 1 {
                if (c.i_column == 0 || c.i_column == -1) && c.usable {
                    match op {
                        2 | 72 => {
                            idx_num |= 0x80;
                            idx_num &= !0x3300;
                            a_idx[5] = i as i64;
                            a_idx[6] = -1;
                            b_start_seen = true;
                        }
                        32 | 4 if idx_num & 0x80 == 0 => {
                            idx_num |= if op == 32 { 0x100 } else { 0x200 };
                            idx_num &= if op == 32 { !0x200 } else { !0x100 };
                            a_idx[5] = i as i64;
                            b_start_seen = true;
                        }
                        8 | 16 if idx_num & 0x80 == 0 => {
                            idx_num |= if op == 8 { 0x1000 } else { 0x2000 };
                            idx_num &= if op == 8 { !0x2000 } else { !0x1000 };
                            a_idx[6] = i as i64;
                        }
                        _ => {}
                    }
                }
                continue;
            }
            let i_col = (c.i_column - 1) as usize;
            if i_col == 0 && op == 2 {
                b_start_seen = true;
            }
            if !c.usable {
                unusable |= 1 << i_col;
            } else if op == 2 {
                idx_num |= 1 << i_col;
                a_idx[i_col] = i as i64;
            }
        }
        if a_idx[3] == 0 {
            idx_num &= !0x60;
            a_idx[4] = 0;
        }
        let mut n_arg = 0;
        for &j in &a_idx {
            if j >= 0 {
                n_arg += 1;
                p.a_constraint_usage[j as usize].argv_index = n_arg;
                p.a_constraint_usage[j as usize].omit = true;
            }
        }
        if !b_start_seen {
            return (1, Some("first argument to \"generate_series()\" missing or unusable".to_owned()));
        }
        if unusable & !idx_num != 0 {
            return (19, None);
        }
        if idx_num & 3 == 3 {
            p.estimated_cost = (2 - i32::from(idx_num & 4 != 0)) as f64;
            p.estimated_rows = 1000;
            if !p.a_order_by.is_empty() && p.a_order_by[0].i_column == 0 {
                idx_num |= if p.a_order_by[0].desc { 0x08 } else { 0x10 };
                p.order_by_consumed = true;
            }
        } else if idx_num & 0x21 == 0x21 {
            p.estimated_rows = 2500;
        } else {
            p.estimated_rows = 2_147_483_647;
        }
        p.idx_num = idx_num;
        p.idx_flags = 2; // SQLITE_INDEX_SCAN_HEX
        (0, None)
    }
}

fn db(host: &VtHost) -> Db<'_> {
    let mut db = Db::new(host);
    db.a_db = vec![
        DbInfo { z_db_s_name: "main".into(), has_bt: true, file_format: 4, schema_cookie: 5, ..DbInfo::default() },
        DbInfo { z_db_s_name: "temp".into(), has_bt: false, file_format: 4, ..DbInfo::default() },
    ];
    db
}

fn rows(sql: &str) -> Vec<String> {
    let host = VtHost;
    match crate::prepare(db(&host), sql) {
        PrepareResult::Program(prog) if prog.explain == 2 => prog
            .list(2)
            .iter()
            .map(|r| {
                r.iter()
                    .map(|v| match v {
                        Value::Integer(i) => i.to_string(),
                        Value::Text(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect(),
        PrepareResult::Program(prog) => crate::explain_text(&prog),
        PrepareResult::Error { message, offset, .. } => vec![format!("ERR {offset} {message}")],
        other => panic!("not compiled: {sql}: {other:?}"),
    }
}

fn where_dump(sql: &str) -> Vec<String> {
    let host = VtHost;
    let (_, _, dump) = crate::prepare_with_where_dump(db(&host), sql);
    dump
}

#[test]
fn table_valued_function_listing() {
    assert_eq!(
        rows("EXPLAIN SELECT value FROM generate_series(1,10) WHERE value>3 ORDER BY value DESC LIMIT 2"),
        [
            "0|Init|0|16|0||0|",
            "1|Noop|1|3|0||0|",
            "2|Integer|2|1|0||0|",
            "3|VOpen|0|0|0|vtab:5C0B3D354148|0|",
            "4|Integer|1|4|0||0|",
            "5|Integer|10|5|0||0|",
            "6|Integer|2|6|0||0|",
            "7|Integer|3|7|0||0|",
            "8|Integer|555|2|0||0|",
            "9|Integer|4|3|0||0|",
            "10|VFilter|0|15|2||0|",
            "11|VColumn|0|0|8||0|",
            "12|ResultRow|8|1|0||0|",
            "13|DecrJumpZero|1|15|0||0|",
            "14|VNext|0|11|0||0|",
            "15|Halt|0|0|0||0|",
            "16|Transaction|0|0|5|0|1|",
            "17|Goto|0|1|0||0|",
        ]
    );
}

#[test]
fn table_valued_function_xbestindex_calls() {
    let d = where_dump("EXPLAIN SELECT value FROM generate_series(1,10) WHERE value>3 ORDER BY value DESC LIMIT 2");
    let want = [
        "WC nTerm=4 nBase=3",
        "TERM 0 top=55 op=4 cur=0 col=0 prR=0 prA=1 fl=0 prob=1 parent=-1 nChild=0 iField=0",
        "TERM 1 top=54 op=2 cur=0 col=1 prR=0 prA=1 fl=1 prob=1 parent=-1 nChild=0 iField=0",
        "TERM 2 top=54 op=2 cur=0 col=2 prR=0 prA=1 fl=1 prob=1 parent=-1 nChild=0 iField=0",
        "TERM 3 top=47 op=40 cur=0 col=0 prR=0 prA=0 fl=3 prob=1 parent=0 nChild=0 iField=0",
        "TERMAUX 3 mop=73",
        "BI tab=generate_series nCons=4 cons=[0:4:1:0,1:2:1:1,2:2:1:2,0:73:1:3] ob=[0:1] colUsed=7 distinct=0 mIn=0",
        "BO rc=0 idxNum=555 idxStr=(null) cost=4000000000000000 rows=1000 obc=1 flags=2 hIn=0 usage=[4:1,1:1,2:1,3:1]",
        "LOOP add tab=0 idx=- ws=400 vtab idxNum=555 idxStr=(null) isOrdered=1 omit=f hex=1 hIn=0 omOff=0 nSkip=0 nLTerm=4 iSortIdx=0 rSetup=0 rRun=10 nOut=99 prereq=0",
        "PATH est=0 nRow=99 rCost=16 rUnsort=14 isOrdered=0 rev=0 nOBSat=0 eDistinct=0 loops=[0:-:400]",
        "PATH est=100 nRow=99 rCost=16 rUnsort=14 isOrdered=1 rev=0 nOBSat=1 eDistinct=0 loops=[0:-:400]",
    ];
    assert_eq!(d, want);
}

#[test]
fn table_valued_function_join_plan() {
    assert_eq!(
        rows("EXPLAIN QUERY PLAN SELECT * FROM t, generate_series(1,t.a) AS g WHERE g.value=t.c"),
        ["3|0|216|SCAN t", "5|0|10|SCAN g VIRTUAL TABLE INDEX 0x83:"]
    );
    let d = where_dump("EXPLAIN QUERY PLAN SELECT * FROM t, generate_series(1,t.a) AS g WHERE g.value=t.c");
    assert!(d.contains(&"BI tab=generate_series nCons=3 cons=[0:2:0:0,1:2:1:1,2:2:0:2] ob=[] colUsed=7 distinct=0 mIn=0".to_owned()));
    assert!(d.contains(&"BO rc=19 idxNum=0 idxStr=(null) cost=546d42aea2879f2e rows=25 obc=0 flags=0 hIn=0 usage=[0:0,1:1,0:0]".to_owned()));
}

#[test]
fn table_valued_function_in_subquery() {
    let r = rows("EXPLAIN SELECT a FROM t WHERE a IN (SELECT value FROM generate_series(1,3))");
    assert_eq!(r[5], "5|VOpen|1|0|0|vtab:5C0B3D354148|0|");
    assert_eq!(r[10], "10|VFilter|1|15|3||0|");
    assert_eq!(r[14], "14|VNext|1|11|0||0|");
    assert_eq!(r.len(), 27);
}

#[test]
fn table_valued_function_errors() {
    assert_eq!(rows("EXPLAIN SELECT * FROM generate_series()"), ["ERR -1 first argument to \"generate_series()\" missing or unusable"]);
    assert_eq!(rows("EXPLAIN SELECT * FROM t(1)"), ["ERR -1 't' is not a function"]);
    assert_eq!(rows("EXPLAIN SELECT * FROM generate_series(1,2,3,4)"), ["ERR -1 too many arguments on generate_series() - max 3"]);
}

// ---- the scratch C modules `vtx` / `vtbad` of $S/v4b/wdump/vtx.c (registered with V4BVTX=1): they call
// sqlite3_vtab_in(…,1), sqlite3_vtab_rhs_value, sqlite3_vtab_distinct and sqlite3_vtab_collation, which no module of
// the reference build does. The host below answers xBestIndex with the same logic. Schema: CREATE TABLE t(a INTEGER
// PRIMARY KEY, b TEXT DEFAULT 'x', c REAL, d) (cookie 1).

struct VtxHost;

const VTX_ID: u64 = 0x5C81_0A32_DF38;

impl Host for VtxHost {
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
        vec![SchemaRow {
            ty: "table".into(),
            name: "t".into(),
            tbl_name: "t".into(),
            rootpage: 2,
            sql: Some("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT DEFAULT 'x', c REAL, d)".into()),
        }]
    }
    fn vtab_module(&self, name: &str, _lazy: bool) -> Option<VtabModule> {
        ["vtx", "vtbad"].iter().find(|m| m.eq_ignore_ascii_case(name)).map(|m| VtabModule {
            name: (*m).to_owned(),
            eponymous: true,
            shadow_names: Vec::new(),
        })
    }
    fn vtab_connect(&self, _i_db: usize, table: &str, _module: &str, _args: &[String], _eponymous: bool) -> VtabConnect {
        let bad = table.eq_ignore_ascii_case("vtbad");
        VtabConnect::Ok {
            vtab: VTable { id: VTX_ID + u64::from(bad), module_name: table.to_owned(), e_vtab_risk: 1, ..VTable::default() },
            declare: Some(if bad { "CREATE TABLE x(a, b)" } else { "CREATE TABLE x(a, b, c HIDDEN)" }.to_owned()),
        }
    }
    fn vtab_best_index(&self, id: u64, p: &mut IndexInfo, h: &mut dyn IndexInfoHidden) -> (i32, Option<String>) {
        if id != VTX_ID {
            // vtbadBestIndex: argvIndex 2 for the first usable constraint
            if let Some(i) = p.a_constraint.iter().position(|c| c.usable) {
                p.a_constraint_usage[i].argv_index = 2;
            }
            return (0, None);
        }
        let mut n = 0;
        for i in 0..p.a_constraint.len() {
            let c = p.a_constraint[i];
            if !c.usable {
                continue;
            }
            if c.op == 2 || c.op == 73 {
                if h.vtab_in(i as i32, -1) {
                    h.vtab_in(i as i32, 1);
                }
                n += 1;
                p.a_constraint_usage[i].argv_index = n;
                p.a_constraint_usage[i].omit = true;
            }
        }
        let mut idx = 0;
        if !p.a_constraint.is_empty() {
            if let (0, Some(Value::Integer(v))) = h.rhs_value(0) {
                idx = 1000 * v as i32;
            }
            p.idx_str = h.collation(0);
            p.need_to_free_idx_str = true;
        }
        p.idx_num = idx + 100 * h.distinct() + n;
        if p.a_order_by.len() == 1 {
            p.order_by_consumed = true;
        }
        p.estimated_cost = 10.0;
        p.estimated_rows = 10;
        (0, None)
    }
}

fn vtx(sql: &str) -> (Vec<String>, Vec<String>) {
    let host = VtxHost;
    let mut db = Db::new(&host);
    db.a_db = vec![
        DbInfo { z_db_s_name: "main".into(), has_bt: true, file_format: 4, schema_cookie: 1, ..DbInfo::default() },
        DbInfo { z_db_s_name: "temp".into(), has_bt: false, file_format: 4, ..DbInfo::default() },
    ];
    let (r, _, dump) = crate::prepare_with_where_dump(db, sql);
    let rows = match r {
        PrepareResult::Program(prog) => crate::explain_text(&prog),
        PrepareResult::Error { message, offset, .. } => vec![format!("ERR {offset} {message}")],
        other => panic!("not compiled: {sql}: {other:?}"),
    };
    (rows, dump)
}

/// `sqlite3_vtab_in(…,1)`: the IN list goes to the module whole (OP_VInitIn), the term is omitted.
#[test]
fn vtab_in_handled_by_the_module() {
    let (rows, dump) = vtx("EXPLAIN SELECT * FROM vtx WHERE a IN (1,2,3) AND b=5");
    assert_eq!(
        rows,
        [
            "0|Init|0|26|0||0|",
            "1|VOpen|0|0|0|vtab:5C810A32DF38|0|",
            "2|BeginSubrtn|0|6|0||0|",
            "3|Once|0|15|0||0|",
            "4|OpenEphemeral|1|1|0|k(1,B)|0|",
            "5|Integer|1|7|0||0|",
            "6|MakeRecord|7|1|8|A|0|",
            "7|IdxInsert|1|8|7|1|0|",
            "8|Integer|2|7|0||0|",
            "9|MakeRecord|7|1|8|A|0|",
            "10|IdxInsert|1|8|7|1|0|",
            "11|Integer|3|7|0||0|",
            "12|MakeRecord|7|1|8|A|0|",
            "13|IdxInsert|1|8|7|1|0|",
            "14|NullRow|1|0|0||0|",
            "15|Return|6|3|1||0|",
            "16|VInitIn|1|3|5||0|",
            "17|Integer|5|4|0||0|",
            "18|Integer|2|1|0||0|",
            "19|Integer|2|2|0||0|",
            "20|VFilter|0|25|1|BINARY|0|",
            "21|VColumn|0|0|9||0|",
            "22|VColumn|0|1|10||0|",
            "23|ResultRow|9|2|0||0|",
            "24|VNext|0|21|0||0|",
            "25|Halt|0|0|0||0|",
            "26|Transaction|0|0|1|0|1|",
            "27|Goto|0|1|0||0|",
        ]
    );
    assert!(dump.contains(&"BI tab=vtx nCons=2 cons=[0:2:1:0,1:2:1:1] ob=[] colUsed=3 distinct=0 mIn=1".to_owned()));
    assert!(dump.contains(&"BO rc=0 idxNum=2 idxStr=BINARY cost=4024000000000000 rows=10 obc=0 flags=0 hIn=1 usage=[1:1,2:1]".to_owned()));
}

/// `sqlite3_vtab_rhs_value`, `sqlite3_vtab_distinct`, `sqlite3_vtab_collation`.
#[test]
fn vtab_rhs_value_distinct_collation() {
    let (_, d) = vtx("EXPLAIN SELECT * FROM vtx WHERE a=7 ORDER BY b");
    assert!(d.contains(&"BO rc=0 idxNum=7001 idxStr=BINARY cost=4024000000000000 rows=10 obc=1 flags=0 hIn=0 usage=[1:1]".to_owned()));
    let (_, d) = vtx("EXPLAIN SELECT DISTINCT a FROM vtx WHERE b=x'01'");
    assert!(d.contains(&"BI tab=vtx nCons=1 cons=[1:2:1:0] ob=[0:0] colUsed=3 distinct=2 mIn=0".to_owned()));
    let (rows, d) = vtx("EXPLAIN SELECT a FROM vtx WHERE a = 'q' COLLATE nocase GROUP BY a");
    assert!(d.contains(&"BO rc=0 idxNum=101 idxStr=NOCASE cost=4024000000000000 rows=10 obc=1 flags=0 hIn=0 usage=[1:1]".to_owned()));
    assert_eq!(rows[9], "9|VFilter|0|21|7|NOCASE|0|");
    let (rows, d) = vtx("EXPLAIN SELECT * FROM vtx(3) LIMIT 4");
    assert!(d.contains(&"BO rc=0 idxNum=3002 idxStr=BINARY cost=4024000000000000 rows=10 obc=0 flags=0 hIn=0 usage=[1:1,2:1]".to_owned()));
    assert_eq!(rows[7], "7|VFilter|0|13|2|BINARY|0|");
}

/// A join: the IN list of a subquery handled by the module; the plan without the dependent term wins.
#[test]
fn vtab_in_subquery_in_a_join() {
    let (rows, d) = vtx("EXPLAIN SELECT * FROM t, vtx WHERE vtx.a IN (SELECT d FROM t) AND vtx.b=t.c");
    assert!(d.contains(&"BI tab=vtx nCons=2 cons=[0:2:1:0,1:2:0:1] ob=[] colUsed=3 distinct=0 mIn=1".to_owned()));
    assert!(d.contains(
        &"LOOP with tab=1 idx=- ws=400 vtab idxNum=1 idxStr=BINARY isOrdered=0 omit=1 hex=0 hIn=1 omOff=0 nSkip=0 nLTerm=1 iSortIdx=0 rSetup=0 rRun=33 nOut=33 prereq=0"
            .to_owned()
    ));
    assert_eq!(rows[15], "15|VInitIn|3|3|4||0|");
    assert_eq!(rows[22], "22|Ne|9|31|8|BINARY-8|83|");
    assert_eq!(rows.len(), 36);
}

/// Non-contiguous argvIndex values: "%s.xBestIndex malfunction".
#[test]
fn vtab_best_index_malfunction() {
    let (rows, _) = vtx("EXPLAIN SELECT * FROM vtbad WHERE a=1");
    assert_eq!(rows, ["ERR -1 vtbad.xBestIndex malfunction"]);
}
