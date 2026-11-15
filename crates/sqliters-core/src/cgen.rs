//! Adapter between the engine and the port of C's code generator (`sqliters-codegen`).
//!
//! Step V1 uses the code generator for one thing only: `EXPLAIN <stmt>` prints C's listing
//! when the generator supports the whole statement. Statements are still executed by the
//! engine's executor. This module is the single place where connection state (flags, text
//! encoding, attached databases, application functions and collations) is handed to the
//! generator; later steps add the schema objects (C's `Table` / `Column` / `Index`) here.

use std::sync::Arc;

use sqliters_codegen::builtin_funcs::CONNECTION_FUNCS;
use sqliters_codegen::consts::{
    COLFLAG_PRIMKEY, COLFLAG_STORED, COLFLAG_UNIQUE, COLFLAG_VIRTUAL, OE_Abort, OE_Fail, OE_Ignore, OE_None, OE_Replace,
    OE_Rollback, SQLITE_DqsDML, SQLITE_IDXTYPE_APPDEF, SQLITE_IDXTYPE_PRIMARYKEY, SQLITE_IDXTYPE_UNIQUE,
    SQLITE_TrustedSchema, TABTYP_NORM, TABTYP_VIEW, TABTYP_VTAB, XN_EXPR,
};
use sqliters_codegen::schema::{ColumnDef, IndexColDef, IndexDef, IndexStatDef, TableDef};
use sqliters_codegen::{Db, DbInfo, Host, PrepareResult};
use sqliters_syntax::ConflictAction;
use sqliters_types::{Collation, FunctionFlags, TextEncoding};
use sqliters_vdbe::vdbeaux::{FuncDef, Program};

use crate::ConnectionInner;
use crate::execution_context::{CollationRegistry, FunctionRegistry};
use crate::schema::{IndexStat1, IndexedColSchema, TableSchema};

/// `SQLITE_FUNC_UNSAFE` (the same bit as `SQLITE_INNOCUOUS`, with the meaning inverted)
const SQLITE_FUNC_UNSAFE: u32 = 0x0020_0000;

struct EngineHost<'a> {
    functions: &'a FunctionRegistry,
    collations: &'a CollationRegistry,
    db_set: &'a crate::db_set::DatabaseSet,
}

/// `OE_*` of a conflict clause.
fn oe(c: ConflictAction) -> u8 {
    (match c {
        ConflictAction::Rollback => OE_Rollback,
        ConflictAction::Abort => OE_Abort,
        ConflictAction::Fail => OE_Fail,
        ConflictAction::Ignore => OE_Ignore,
        ConflictAction::Replace => OE_Replace,
    }) as u8
}

/// The name of a collating sequence as the schema declares it (`None` for the default).
fn coll_name(c: Collation, custom: Option<&str>) -> Option<String> {
    match (custom, c) {
        (Some(n), _) => Some(n.to_owned()),
        (None, Collation::Binary) => None,
        (None, Collation::NoCase) => Some("NOCASE".to_owned()),
        (None, Collation::RTrim) => Some("RTRIM".to_owned()),
    }
}

/// The number N of the automatic index `sqlite_autoindex_<table>_<N>`.
fn autoindex_number(name: &str) -> u32 {
    name.rsplit('_').next().and_then(|n| n.parse().ok()).unwrap_or(u32::MAX)
}

fn stat_def(s: &IndexStat1) -> Option<IndexStatDef> {
    if !s.has_stat1 {
        return None;
    }
    Some(IndexStatDef {
        ai_row_log_est: s.row_log_est.clone(),
        b_unordered: s.unordered,
        no_skip_scan: s.no_skip_scan,
        sz_idx_row: s.sz_idx_row,
    })
}

/// The declaration of a table for the generator's schema adapter (`sqliters_codegen::schema`
/// derives C's `Table` / `Index` objects from it with the logic of `build.c`).
fn table_def(schema: &crate::schema::Schema, t: &TableSchema) -> TableDef {
    let cols: Vec<ColumnDef> = t
        .columns
        .iter()
        .map(|c| {
            let mut col_flags = 0;
            if c.is_primary_key {
                col_flags |= COLFLAG_PRIMKEY;
            }
            if c.is_unique {
                col_flags |= COLFLAG_UNIQUE;
            }
            match &c.generated {
                Some((_, true)) => col_flags |= COLFLAG_STORED,
                Some((_, false)) => col_flags |= COLFLAG_VIRTUAL,
                None => {}
            }
            ColumnDef {
                name: c.name.clone(),
                z_type: if c.type_name.is_empty() { None } else { Some(c.type_name.clone()) },
                not_null: c.not_null.map_or(0, oe),
                col_flags: col_flags as u16,
                z_coll: coll_name(c.collation, c.custom_collation.as_deref()),
                has_dflt: c.default_expr.is_some() || c.generated.is_some(),
                dflt_value: c.default_value_for_cgen(),
            }
        })
        .collect();
    let index_col = |ic: &IndexedColSchema| -> IndexColDef {
        let so = &ic.sort_order;
        let idx_coll = coll_name(so.collation, so.custom_collation.as_ref().map(|c| &*c.name));
        let z_coll = match ic.col_idx {
            Some(j) => {
                let col_coll = &cols[j].z_coll;
                let same = match (&idx_coll, col_coll) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                    _ => false,
                };
                // The same collation as the column: inherited (or written out again, which
                // gives the same Index). A different one is the explicit COLLATE of the index.
                if same { None } else { Some(idx_coll.unwrap_or_else(|| "BINARY".to_owned())) }
            }
            None => idx_coll,
        };
        IndexColDef {
            i_column: ic.col_idx.map_or(XN_EXPR, |j| j as i32),
            z_coll,
            desc: so.desc,
        }
    };
    // The automatic indexes of the CREATE TABLE statement, in the order of their constraints
    // (the number in the name), then the CREATE INDEX statements in the order of sqlite_schema.
    let mut autos: Vec<IndexDef> = Vec::new();
    let mut created: Vec<(i64, IndexDef)> = Vec::new();
    for ix in schema.indexes.iter().filter(|ix| ix.table_name.eq_ignore_ascii_case(&t.name)) {
        let d = IndexDef {
            name: ix.name.clone(),
            cols: ix.columns.iter().map(index_col).collect(),
            on_error: if ix.unique { oe(ix.conflict) } else { OE_None as u8 },
            idx_type: (match ix.origin {
                "pk" => SQLITE_IDXTYPE_PRIMARYKEY,
                "u" => SQLITE_IDXTYPE_UNIQUE,
                _ => SQLITE_IDXTYPE_APPDEF,
            }) as u8,
            tnum: ix.root_pgno,
            is_partial: ix.where_clause.is_some(),
            z_sql: ix.raw_sql.clone(),
            stat: ix.stat1.as_ref().and_then(stat_def),
        };
        if ix.origin == "c" { created.push((ix.schema_rowid, d)) } else { autos.push(d) }
    }
    if t.without_rowid {
        autos.push(IndexDef {
            name: t.without_rowid_pk_autoindex_name.clone(),
            cols: t.without_rowid_pk_cols.iter().map(index_col).collect(),
            on_error: oe(t.without_rowid_pk_conflict),
            idx_type: SQLITE_IDXTYPE_PRIMARYKEY as u8,
            tnum: t.root_pgno,
            is_partial: false,
            z_sql: None,
            stat: t.without_rowid_pk_stat1.as_ref().and_then(stat_def),
        });
    }
    // C's parser takes a one-column INTEGER PRIMARY KEY for the rowid alias until it sees
    // WITHOUT ROWID at the end of the statement, and creates the PRIMARY KEY index then:
    // after the UNIQUE indexes. The engine numbers it by the position of the constraint
    // (recorded difference D1): the names of the automatic indexes differ from C's.
    let mut unsupported = None;
    if t.without_rowid
        && t.without_rowid_pk_cols.len() == 1
        && !t.without_rowid_pk_cols[0].sort_order.desc
        && t.without_rowid_pk_cols[0]
            .col_idx
            .is_some_and(|j| t.columns[j].type_name.trim().eq_ignore_ascii_case("INTEGER"))
    {
        let pk_n = autoindex_number(&t.without_rowid_pk_autoindex_name);
        if autos.iter().any(|d| autoindex_number(&d.name) > pk_n) {
            unsupported = Some("WITHOUT ROWID table whose automatic index names differ from C");
        }
    }
    autos.sort_by_key(|d| autoindex_number(&d.name));
    created.sort_by_key(|(rowid, _)| *rowid);
    let mut indexes = autos;
    indexes.extend(created.into_iter().map(|(_, d)| d));
    TableDef {
        name: t.name.clone(),
        cols,
        e_tab_type: TABTYP_NORM as u8,
        tnum: t.root_pgno,
        i_p_key: t.rowid_alias_col.map_or(-1, |c| c as i32),
        key_conf: oe(t.rowid_pk_conflict),
        without_rowid: t.without_rowid,
        strict: t.strict,
        autoincrement: t.autoincrement,
        has_primary_key: t.columns.iter().any(|c| c.is_primary_key),
        indexes,
        n_row_log_est: t.stat1.n_row_log_est,
        sz_tab_row: t.stat1.sz_tab_row,
        has_stat1: t.stat1.has_stat1,
        z_sql: Some(t.raw_sql.clone()),
        has_trigger: !schema.triggers_for_table(&t.name).is_empty(),
        unsupported,
    }
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

    fn find_table(&self, i_db: usize, name: &str) -> Option<TableDef> {
        let slot = self.db_set.slots().get(i_db)?;
        // (the schema table itself, sqlite_master / sqlite_temp_master, is built by the generator)
        let schema = &slot.schema;
        if let Some(t) = schema.find_user_table(name) {
            let mut def = table_def(schema, t);
            // C's in-memory statistics are not known to be the engine's (see DbSlot)
            if def.unsupported.is_none() {
                if slot.c_stat_dirty {
                    def.unsupported = Some("sqlite_stat1 was written and not re-read by ANALYZE");
                } else if slot.c_stat_stale.contains(&t.name.to_ascii_lowercase()) {
                    def.unsupported = Some("table whose removed sqlite_stat1 rows C still remembers");
                }
            }
            return Some(def);
        }
        if let Some(v) = schema.find_view(name) {
            return Some(TableDef {
                name: v.name.clone(),
                e_tab_type: TABTYP_VIEW as u8,
                i_p_key: -1,
                n_row_log_est: 200,
                z_sql: Some(v.raw_sql.clone()),
                has_trigger: !schema.triggers_for_table(&v.name).is_empty(),
                ..TableDef::default()
            });
        }
        if let Some(vt) = schema.find_virtual_table(name) {
            return Some(TableDef {
                name: vt.name.clone(),
                e_tab_type: TABTYP_VTAB as u8,
                i_p_key: -1,
                ..TableDef::default()
            });
        }
        None
    }
}

/// Run C's parser and code generator over one statement (`sqlite3Prepare`).
pub(crate) fn c_prepare(inner: &ConnectionInner, sql: &str) -> (PrepareResult, usize) {
    let host = EngineHost { functions: &inner.functions, collations: &inner.collations, db_set: &inner.db_set };
    let mut db = Db::new(&host);
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
    if where_dump_on() {
        // Verification aid: the planner's term / loop / path dump on stderr, before the
        // `cg:` trace line of the statement.
        let (r, tail, dump) = sqliters_codegen::prepare_with_where_dump(db, sql);
        for l in dump {
            eprintln!("wd: {l}");
        }
        return (r, tail);
    }
    sqliters_codegen::prepare_tail(db, sql)
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
pub(crate) fn c_explain_tail(inner: &ConnectionInner, sql: &str) -> (CExplain, usize) {
    let (r, tail) = c_prepare(inner, sql);
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
    (r, tail)
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
