//! Adapter between the engine and the port of C's code generator (`sqliters-codegen`).
//!
//! Step V1 uses the code generator for one thing only: `EXPLAIN <stmt>` prints C's listing
//! when the generator supports the whole statement. Statements are still executed by the
//! engine's executor. This module is the single place where connection state (flags, text
//! encoding, attached databases, application functions and collations) is handed to the
//! generator; later steps add the schema objects (C's `Table` / `Column` / `Index`) here.

use std::sync::Arc;

use sqliters_codegen::builtin_funcs::CONNECTION_FUNCS;
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
}

/// Run C's parser and code generator over one statement (`sqlite3Prepare`).
pub(crate) fn c_prepare(inner: &ConnectionInner, sql: &str) -> (PrepareResult, usize) {
    let host = EngineHost { functions: &inner.functions, collations: &inner.collations };
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
    db.a_db = inner
        .db_set
        .slots()
        .iter()
        .map(|s| DbInfo {
            z_db_s_name: s.name.clone(),
            has_bt: s.btree.is_some(),
            sharable: false,
            read_only: s.btree.as_ref().is_some_and(|bt| bt.pager().is_readonly()),
            schema_cookie: 0,
            i_generation: 0,
        })
        .collect();
    sqliters_codegen::prepare_tail(db, sql)
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

/// True if the statement text starts with `EXPLAIN` that is not `EXPLAIN QUERY PLAN`.
pub(crate) fn is_explain(sql: &str) -> bool {
    use sqliters_codegen::tk::{TK_COMMENT, TK_EXPLAIN, TK_QUERY, TK_SPACE};
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
    next() == TK_EXPLAIN && next() != TK_QUERY
}
