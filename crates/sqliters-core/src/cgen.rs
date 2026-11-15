//! Adapter between the engine and the port of C's code generator (`sqliters-codegen`).
//!
//! Step V1 uses the code generator for one thing only: `EXPLAIN <stmt>` prints C's listing
//! when the generator supports the whole statement. Statements are still executed by the
//! engine's executor. This module is the single place where connection state (flags, text
//! encoding, attached databases, application functions and collations) is handed to the
//! generator; later steps add the schema objects (C's `Table` / `Column` / `Index`) here.

use std::sync::Arc;

use sqliters_codegen::builtin_funcs::CONNECTION_FUNCS;
use sqliters_codegen::build_tab::SchemaRow;
use sqliters_codegen::consts::{SQLITE_DqsDML, SQLITE_TrustedSchema};
use sqliters_codegen::{Db, DbInfo, Host, PrepareResult};
use sqliters_types::{FunctionFlags, TextEncoding};
use sqliters_vdbe::vdbeaux::{FuncDef, Program};

use crate::ConnectionInner;
use crate::execution_context::{CollationRegistry, FunctionRegistry};

/// `SQLITE_FUNC_UNSAFE` (the same bit as `SQLITE_INNOCUOUS`, with the meaning inverted)
const SQLITE_FUNC_UNSAFE: u32 = 0x0020_0000;

struct EngineHost<'a> {
    functions: &'a FunctionRegistry,
    collations: &'a CollationRegistry,
    db_set: &'a crate::db_set::DatabaseSet,
    /// set by [`Host::note_schema_loaded`]
    schema_loaded: std::cell::Cell<bool>,
}

/// `sqlite3CreateFunc()`: the `funcFlags` of an application-defined function.
fn func_flags(flags: FunctionFlags) -> u32 {
    let extra = flags.0
        & (FunctionFlags::DETERMINISTIC.0
            | FunctionFlags::DIRECTONLY.0
            | FunctionFlags::SUBTYPE.0
            | FunctionFlags::INNOCUOUS.0
            | FunctionFlags::RESULT_SUBTYPE.0);
    // The SQLITE_INNOCUOUS flag is the same bit as SQLITE_FUNC_UNSAFE. But the meaning is
    // inverted. So flip the bit.
    1 /* SQLITE_UTF8 */ | (extra ^ SQLITE_FUNC_UNSAFE)
}

impl Host for EngineHost<'_> {
    fn note_schema_loaded(&self) {
        self.schema_loaded.set(true);
    }

    fn user_functions(&self, name: &str) -> Vec<Arc<FuncDef>> {
        // The functions every connection of the reference build registers at open (FTS5,
        // R-Tree, MATCH) are described by the generator's own table, with C's flags.
        if CONNECTION_FUNCS.iter().any(|f| f.0.eq_ignore_ascii_case(name)) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (arity, flags) in self.functions.scalar_overloads(name) {
            out.push(Arc::new(FuncDef {
                n_arg: arity.map_or(-1, |n| n as i16),
                func_flags: func_flags(flags),
                name: name.to_ascii_lowercase(),
                user_data: -1,
                x_finalize: false,
                x_value: false,
            }));
        }
        for (arity, flags, supports_window) in self.functions.aggregate_overloads(name) {
            out.push(Arc::new(FuncDef {
                n_arg: arity.map_or(-1, |n| n as i16),
                func_flags: func_flags(flags),
                name: name.to_ascii_lowercase(),
                user_data: -1,
                x_finalize: true,
                x_value: supports_window,
            }));
        }
        out
    }

    fn find_collation(&self, name: &str) -> Option<String> {
        self.collations.registered_name(name)
    }

    fn schema_rows(&self, i_db: usize) -> Vec<SchemaRow> {
        let Some(slot) = self.db_set.slots().get(i_db) else { return Vec::new() };
        let schema = &slot.schema;
        // "SELECT*FROM sqlite_schema ORDER BY rowid": every object with its row
        let mut rows: Vec<(i64, SchemaRow)> = Vec::new();
        for t in &schema.tables {
            rows.push((
                t.schema_rowid,
                SchemaRow { ty: "table".into(), name: t.name.clone(), tbl_name: t.name.clone(), rootpage: t.root_pgno, sql: Some(t.raw_sql.clone()) },
            ));
        }
        for ix in &schema.indexes {
            rows.push((
                ix.schema_rowid,
                SchemaRow { ty: "index".into(), name: ix.name.clone(), tbl_name: ix.table_name.clone(), rootpage: ix.root_pgno, sql: ix.raw_sql.clone() },
            ));
        }
        for v in &schema.views {
            rows.push((v.schema_rowid, SchemaRow { ty: "view".into(), name: v.name.clone(), tbl_name: v.name.clone(), rootpage: 0, sql: Some(v.raw_sql.clone()) }));
        }
        for vt in &schema.virtual_tables {
            rows.push((vt.schema_rowid, SchemaRow { ty: "table".into(), name: vt.name.clone(), tbl_name: vt.name.clone(), rootpage: 0, sql: Some(vt.sql.clone()) }));
        }
        for tr in &schema.triggers {
            rows.push((
                tr.schema_rowid,
                SchemaRow { ty: "trigger".into(), name: tr.name.clone(), tbl_name: tr.tbl_name.clone(), rootpage: 0, sql: Some("CREATE TRIGGER".into()) },
            ));
        }
        rows.sort_by_key(|(rowid, _)| *rowid);
        rows.into_iter().map(|(_, r)| r).collect()
    }

    fn stat1_rows(&self, i_db: usize) -> Vec<(String, Option<String>, String)> {
        // analysisLoader() sees the rows in the order its "SELECT tbl,idx,stat FROM
        // sqlite_stat1" scan returned them: reversed under PRAGMA reverse_unordered_selects
        self.db_set.slots().get(i_db).map_or_else(Vec::new, |s| {
            let mut rows = s.schema.stat1_rows.clone();
            if s.c_stat_reverse {
                rows.reverse();
            }
            rows
        })
    }

    fn created_since_load(&self, i_db: usize, name: &str) -> bool {
        self.db_set.slots().get(i_db).is_some_and(|s| s.c_incremental.contains(name))
    }

    fn table_unsupported(&self, i_db: usize, name: &str) -> Option<&'static str> {
        // C's in-memory statistics are not known to be the engine's (see DbSlot)
        let slot = self.db_set.slots().get(i_db)?;
        if slot.c_stat_dirty {
            Some("sqlite_stat1 was written and not re-read by ANALYZE")
        } else if slot.c_stat_stale.contains(name) {
            Some("table whose removed sqlite_stat1 rows C still remembers")
        } else {
            None
        }
    }
}

/// Run C's parser and code generator over one statement (`sqlite3Prepare`).
/// The third value: C's `sqlite3Init()` loaded the schemas during this prepare.
pub(crate) fn c_prepare(inner: &ConnectionInner, sql: &str) -> (PrepareResult, usize, bool) {
    let host = EngineHost {
        functions: &inner.functions,
        collations: &inner.collations,
        db_set: &inner.db_set,
        schema_loaded: std::cell::Cell::new(false),
    };
    if schema_dump_on() && sql.contains("/*#schemadump*/") {
        // Verification aid: the schema objects as the generator loads them, on stderr.
        for l in sqliters_codegen::schema_dump(make_db(inner, &host)) {
            eprintln!("sd: {l}");
        }
    }
    let db = make_db(inner, &host);
    if where_dump_on() {
        // Verification aid: the planner's term / loop / path dump on stderr, before the
        // `cg:` trace line of the statement.
        let (r, tail, dump) = sqliters_codegen::prepare_with_where_dump(db, sql);
        for l in dump {
            eprintln!("wd: {l}");
        }
        return (r, tail, host.schema_loaded.get());
    }
    let (r, tail) = sqliters_codegen::prepare_tail(db, sql);
    (r, tail, host.schema_loaded.get())
}

/// The connection state the generator reads (`sqlite3 *db`).
fn make_db<'a>(inner: &ConnectionInner, host: &'a EngineHost<'a>) -> Db<'a> {
    let mut db = Db::new(host);
    db.enc = match inner.effective_encoding() {
        TextEncoding::Utf8 => 1,
        TextEncoding::Utf16Le => 2,
        TextEncoding::Utf16Be => 3,
    };
    if !inner.db_set.dqs_dml() {
        db.flags &= !SQLITE_DqsDML;
    }
    if !inner.db_set.trusted_schema() {
        db.flags &= !SQLITE_TrustedSchema;
    }
    // The flag pragmas (automatic_index, reverse_unordered_selects, short_column_names,
    // full_column_names): the bits that differ from C's default.
    db.flags ^= inner.db_set.c_flags_toggled();
    db.case_sensitive_like = inner.db_set.c_case_sensitive_like();
    db.schema_not_loaded = inner.db_set.c_prepare_unloaded();
    db.encoding_fixed = inner.db_set.c_encoding_fixed();
    let ahead = inner.db_set.c_cookie_ahead();
    db.a_db = inner
        .db_set
        .slots()
        .iter()
        .enumerate()
        .map(|(i, s)| DbInfo {
            z_db_s_name: s.name.clone(),
            has_bt: s.btree.is_some(),
            sharable: false,
            read_only: s.btree.as_ref().is_some_and(|bt| bt.pager().is_readonly()),
            schema_cookie: s.schema.schema_cookie as i32 + i32::from(ahead == Some(i)),
            i_generation: s.c_generation,
            file_format: s.btree.as_ref().map_or(4, |bt| (bt.pager().header().schema_format as u8).max(1)),
        })
        .collect();
    db
}

/// True if the `SQLITERS_CG_SCHEMADUMP` environment variable is set.
fn schema_dump_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SQLITERS_CG_SCHEMADUMP").is_some())
}

/// True if the `SQLITERS_CG_WHEREDUMP` environment variable is set.
fn where_dump_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SQLITERS_CG_WHEREDUMP").is_some())
}

/// True if the `SQLITERS_CG_TRACE` environment variable is set: `EXPLAIN` then reports on
/// stderr whether C's code generator handled the statement (used by the differential tools).
fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SQLITERS_CG_TRACE").is_some())
}

/// The outcome of [`c_explain`].
pub(crate) enum CExplain {
    /// C's program for the statement: `EXPLAIN` lists it.
    Program(Arc<Program>),
    /// The statement does not compile in C: the error message and byte offset.
    Error { message: String, offset: i32 },
    /// The generator does not support the statement yet: the engine's old output is kept.
    Unsupported,
}

/// `EXPLAIN <stmt>`: C's program, C's compile error, or "not supported yet".
pub(crate) fn c_explain(inner: &ConnectionInner, sql: &str) -> CExplain {
    c_explain_tail(inner, sql).0
}

/// [`c_explain`], also returning the byte offset of the first character past the statement.
/// The third value: C's `sqlite3Init()` loaded the schemas during this prepare.
pub(crate) fn c_explain_tail(inner: &ConnectionInner, sql: &str) -> (CExplain, usize, bool) {
    let (r, tail, loaded) = c_prepare(inner, sql);
    if trace_on() {
        let first: String = sql.chars().take(200).map(|c| if c == '\n' { ' ' } else { c }).collect();
        match &r {
            PrepareResult::Program(_) => eprintln!("cg: supported :: {first}"),
            PrepareResult::Error { message, .. } => eprintln!("cg: error: {message} :: {first}"),
            PrepareResult::Unsupported(why) => eprintln!("cg: unsupported: {why} :: {first}"),
        }
    }
    let r = match r {
        PrepareResult::Program(p) => CExplain::Program(Arc::new(*p)),
        PrepareResult::Error { message, offset } => CExplain::Error { message, offset },
        PrepareResult::Unsupported(_) => CExplain::Unsupported,
    };
    (r, tail, loaded)
}

/// True if the statement text starts with `EXPLAIN` (also `EXPLAIN QUERY PLAN`).
pub(crate) fn is_explain(sql: &str) -> bool {
    use sqliters_codegen::tk::{TK_COMMENT, TK_EXPLAIN, TK_SPACE};
    use sqliters_codegen::tokenize::sqlite3_get_token;
    let z = sql.as_bytes();
    let mut pos = 0;
    let mut next = || loop {
        let (n, t) = sqlite3_get_token(&z[pos.min(z.len())..]);
        if n == 0 {
            return 0;
        }
        pos += n;
        if t != TK_SPACE && t != TK_COMMENT {
            return t;
        }
    };
    next() == TK_EXPLAIN
}
