//! Step V5 (DML, triggers, foreign keys): the generator's listings equal C SQLite 3.54.1's
//! (`sqlite3 -list -cmd ".explain off" db "EXPLAIN <stmt>"` on a database built from the schema
//! below; schema cookie 5).

use std::sync::Arc;

use sqliters_vdbe::vdbeaux::FuncDef;

use crate::build_tab::SchemaRow;
use crate::consts::SQLITE_ForeignKeys;
use crate::sqliteint::*;
use crate::PrepareResult;

struct DmlHost;

impl Host for DmlHost {
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
            sql: sql.map(str::to_owned),
        };
        vec![
            row("table", "t", "t", 2, Some("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT UNIQUE, c)")),
            row("index", "sqlite_autoindex_t_1", "t", 3, None),
            row("table", "log", "log", 4, Some("CREATE TABLE log(x, y)")),
            row("trigger", "tr", "t", 0, Some("CREATE TRIGGER tr AFTER DELETE ON t BEGIN INSERT INTO log VALUES(old.a, old.b); END")),
            row("table", "p", "p", 5, Some("CREATE TABLE p(id INTEGER PRIMARY KEY)")),
            row("table", "ch", "ch", 6, Some("CREATE TABLE ch(pid REFERENCES p(id) ON DELETE CASCADE, z)")),
        ]
    }
}

fn check(sql: &str, foreign_keys: bool, want: &[&str]) {
    let host = DmlHost;
    let mut db = Db::new(&host);
    db.a_db = vec![
        DbInfo { z_db_s_name: "main".into(), has_bt: true, file_format: 4, schema_cookie: 5, ..DbInfo::default() },
        DbInfo { z_db_s_name: "temp".into(), has_bt: false, file_format: 4, ..DbInfo::default() },
    ];
    if foreign_keys {
        db.flags |= SQLITE_ForeignKeys;
    }
    let prog = match crate::prepare(db, &format!("EXPLAIN {sql}")) {
        PrepareResult::Program(prog) => prog,
        other => panic!("not compiled: {sql}: {other:?}"),
    };
    let got = crate::explain_text(&prog);
    assert_eq!(got, want, "{sql}");
}

#[test]
fn upsert_returning() {
    check(
        "INSERT INTO t VALUES(1,'x',2) ON CONFLICT(b) DO UPDATE SET c=excluded.c RETURNING a",
        false,
        &[
            "0|Init|0|52|0||0|",
            "1|OpenWrite|0|2|0|3|0|",
            "2|OpenWrite|1|3|0|k(2,,)|0|",
            "3|SoftNull|2|0|0||0|",
            "4|String8|0|3|0|x|0|",
            "5|Integer|2|4|0||0|",
            "6|Integer|1|1|0||0|",
            "7|NotNull|1|9|0||0|",
            "8|NewRowid|0|1|0||0|",
            "9|MustBeInt|1|0|0||0|",
            "10|Goto|0|14|0||0|",
            "11|NotExists|0|13|1||0|",
            "12|Halt|1555|2|0|t.a|2|",
            "13|Goto|0|39|0||0|",
            "14|Affinity|2|2|0|DB|0|",
            "15|SCopy|3|6|0||0|",
            "16|IntCopy|1|7|0||0|",
            "17|MakeRecord|6|2|5||0|",
            "18|NoConflict|1|38|6|1|0|",
            "19|IdxRowid|1|10|0||0|",
            "20|SeekRowid|0|0|10||0|",
            "21|Null|0|11|12||0|",
            "22|Noop|2|0|11||0|",
            "23|Rowid|0|12|0||0|",
            "24|Rowid|0|13|0||0|",
            "25|Null|0|14|0||0|",
            "26|Null|0|15|0||0|",
            "27|Copy|12|16|0||0|",
            "28|Null|0|17|0||0|",
            "29|Column|0|1|18||0|",
            "30|Copy|4|19|0||0|",
            "31|MakeRecord|17|3|11|DB|0|",
            "32|Insert|0|11|16|t|5|",
            "33|Copy|16|20|0||0|",
            "34|MakeRecord|20|1|21||0|",
            "35|NewRowid|4|22|0||0|",
            "36|Insert|4|21|22||0|",
            "37|Goto|0|46|0||0|",
            "38|Goto|0|11|0||0|",
            "39|MakeRecord|2|3|8||0|",
            "40|IdxInsert|1|5|6|2|16|",
            "41|Insert|0|8|1|t|49|",
            "42|Copy|1|23|0||0|",
            "43|MakeRecord|23|1|24||0|",
            "44|NewRowid|4|25|0||0|",
            "45|Insert|4|24|25||0|",
            "46|FkCheck|0|0|0||0|",
            "47|Rewind|4|51|0||0|",
            "48|Column|4|0|23||0|",
            "49|ResultRow|23|1|0||0|",
            "50|Next|4|48|0||0|",
            "51|Halt|0|0|0||0|",
            "52|Transaction|0|1|5|0|1|",
            "53|OpenEphemeral|4|1|0||0|",
            "54|Goto|0|1|0||0|",
        ],
    );
}

#[test]
fn after_delete_trigger_program() {
    check(
        "DELETE FROM t WHERE a=1",
        false,
        &[
            "0|Init|0|15|0||0|",
            "1|Null|0|1|0||0|",
            "2|OpenWrite|0|2|0|3|0|",
            "3|Integer|1|2|0||0|",
            "4|SeekRowid|0|14|2||0|",
            "5|Rowid|0|3|0||0|",
            "6|OpenWrite|1|3|0|k(2,,)|8|",
            "7|Copy|3|4|0||0|",
            "8|Column|0|1|6||0|",
            "9|Column|0|1|8||0|",
            "10|Rowid|0|9|0||0|",
            "11|IdxDelete|1|8|0|sqlite_autoindex_t_1|2|",
            "12|Delete|0|1|0|t|0|",
            "13|Program|4|14|10|program|1|",
            "14|Halt|0|0|0||0|",
            "15|Transaction|0|1|5|0|1|",
            "16|Goto|0|1|0||0|",
            "0|Init|0|1|0|-- TRIGGER tr|0|",
            "1|Trace|2147483647|1|0|-- INSERT INTO log VALUES(old.a, old.b)|0|",
            "2|OpenWrite|0|4|0|2|0|",
            "3|Param|0|2|0||0|",
            "4|Param|2|3|0||0|",
            "5|NewRowid|0|1|0||0|",
            "6|MakeRecord|2|2|4||0|",
            "7|Insert|0|4|1|log|57|",
            "8|ResetCount|0|0|0||0|",
            "9|Halt|0|0|0||0|",
        ],
    );
}

#[test]
fn foreign_key_cascade_action() {
    check(
        "DELETE FROM p WHERE id=3",
        true,
        &[
            "0|Init|0|16|0||0|",
            "1|Null|0|1|0||0|",
            "2|OpenWrite|0|5|0|1|0|",
            "3|Integer|3|2|0||0|",
            "4|SeekRowid|0|15|2||0|",
            "5|Rowid|0|3|0||0|",
            "6|Copy|3|4|0||0|",
            "7|OpenRead|1|6|0|1|0|",
            "8|Rewind|1|13|0||0|",
            "9|Column|1|0|6||0|",
            "10|Ne|4|12|6|BINARY-8|83|",
            "11|FkCounter|0|1|0||0|",
            "12|Next|1|9|0||1|",
            "13|Delete|0|1|0|p|0|",
            "14|Program|4|0|8|program|0|",
            "15|Halt|0|0|0||0|",
            "16|Transaction|0|1|5|0|1|",
            "17|Goto|0|1|0||0|",
            "0|Init|0|1|0||0|",
            "1|Null|0|1|0||0|",
            "2|OpenRead|0|6|0|1|0|",
            "3|Rewind|0|10|0||0|",
            "4|Column|0|0|2||0|",
            "5|Param|0|3|0||0|",
            "6|Ne|3|9|2|BINARY-8|83|",
            "7|Rowid|0|4|0||0|",
            "8|RowSetAdd|1|4|0||0|",
            "9|Next|0|4|0||1|",
            "10|OpenWrite|0|6|0|2|0|",
            "11|RowSetRead|1|26|4||0|",
            "12|NotExists|0|25|4|1|0|",
            "13|Copy|4|5|0||0|",
            "14|Column|0|0|6||0|",
            "15|FkIfZero|0|23|0||0|",
            "16|IsNull|6|23|0||0|",
            "17|SCopy|6|3|0||0|",
            "18|MustBeInt|3|22|0||0|",
            "19|OpenRead|1|5|0|1|0|",
            "20|NotExists|1|22|3||0|",
            "21|Goto|0|23|0||0|",
            "22|FkCounter|0|-1|0||0|",
            "23|Close|1|0|0||0|",
            "24|Delete|0|1|0|ch|0|",
            "25|Goto|0|11|0||0|",
            "26|ResetCount|0|0|0||0|",
            "27|Halt|0|0|0||0|",
        ],
    );
}
