//! Port of SQLite 3.54.1's SQL code generator: the tokenizer and LALR(1) parser building C's
//! parse tree (`tokenize.c`, `parse.y`), name resolution (`resolve.c`, `walker.c`),
//! expression code generation (`expr.c`), the SELECT code generator (`select.c`) and the
//! statement shell (`build.c`). The output is a VDBE program
//! ([`sqliters_vdbe::vdbeaux::Program`]) whose `EXPLAIN` listing is byte-identical to C's.
//!
//! The port is built step by step next to the engine's existing executor. Whatever a
//! statement needs that is not ported yet makes [`prepare`] return
//! [`PrepareResult::Unsupported`]; nothing is approximated.
#![forbid(unsafe_code)]
#![allow(non_upper_case_globals)]
#![allow(clippy::collapsible_if, clippy::collapsible_else_if, clippy::needless_range_loop, clippy::manual_range_contains)]

pub mod alter;
pub mod analyze;
pub mod build;
pub mod build_tab;
pub mod builtin_funcs;
pub mod callback;
pub mod consts;
pub mod ddl;
pub mod delete;
pub mod expr;
pub mod exprcode;
pub mod flatten;
pub mod fkey;
pub mod insert;
pub mod merge;
pub mod parse;
pub mod parse_tables;
pub mod resolve;
pub mod schema;
pub mod select;
pub mod sqliteint;
pub mod tk;
pub mod trigger;
pub mod treedump;
pub mod tokenize;
pub mod update;
pub mod upsert;
pub mod util;
pub mod vtab;
pub mod walker;
pub mod where_;
pub mod wherecode;
pub mod whereexpr;
pub mod whereint;
pub mod window;

#[cfg(test)]
mod explain_tests;
#[cfg(test)]
mod plan_tests;
#[cfg(test)]
mod vtab_tests;
#[cfg(test)]
mod dml_tests;

use sqliters_vdbe::vdbeaux::Program;

pub use sqliteint::{Db, DbInfo, Host, NoHost, Parse};

/// The outcome of [`prepare`].
#[derive(Debug)]
pub enum PrepareResult {
    /// The statement was compiled: the program (ready for the machine / for `EXPLAIN`).
    Program(Box<Program>),
    /// The statement failed to compile, with the error message and byte offset
    /// (`sqlite3_error_offset()`, -1 if none) C SQLite reports.
    Error { message: String, offset: i32 },
    /// The statement needs a part of C's code generator that is not ported yet; the text
    /// names the first missing piece.
    Unsupported(&'static str),
}

/// `sqlite3Prepare()` for one statement: run the parser and code generator over `z_sql`
/// (the text of exactly one statement, with or without the terminating `;`).
pub fn prepare(db: Db<'_>, z_sql: &str) -> PrepareResult {
    prepare_tail(db, z_sql).0
}

/// [`prepare`], also returning `pzTail`: the byte offset in `z_sql` of the first character
/// past the statement that was compiled.
pub fn prepare_tail(db: Db<'_>, z_sql: &str) -> (PrepareResult, usize) {
    let (r, tail, _) = prepare_with_tree_dump(db, z_sql, false);
    (r, tail)
}

/// [`prepare_tail`] that also returns the tree dumps of [`treedump`] (verification aid).
pub fn prepare_with_tree_dump(db: Db<'_>, z_sql: &str, want_dump: bool) -> (PrepareResult, usize, Vec<String>) {
    let mut p = Parse::new(db, z_sql);
    if want_dump {
        p.tree_dump = Some(Vec::new());
    }
    tokenize::sqlite3_run_parser(&mut p);
    let tail = p.z_tail;
    let dump = p.tree_dump.take().unwrap_or_default();
    (prepare_result(p), tail, dump)
}

/// [`prepare_tail`] that also returns the WHERE-clause term dump and the WhereLoop / path dump
/// of the query planner (verification aid; see `where_.rs`).
pub fn prepare_with_where_dump(db: Db<'_>, z_sql: &str) -> (PrepareResult, usize, Vec<String>) {
    let mut p = Parse::new(db, z_sql);
    p.where_dump = Some(Vec::new());
    tokenize::sqlite3_run_parser(&mut p);
    let tail = p.z_tail;
    let dump = p.where_dump.take().unwrap_or_default();
    (prepare_result(p), tail, dump)
}

/// The schema objects of the connection as the generator loads them (verification aid: the
/// schema layer of the differential proof; see [`build_tab::schema_dump`]).
pub fn schema_dump(db: Db<'_>) -> Vec<String> {
    let mut p = Parse::new(db, "");
    build_tab::schema_dump(&mut p)
}

#[allow(dead_code)]
fn prepare_tail_plain(db: Db<'_>, z_sql: &str) -> (PrepareResult, usize) {
    let mut p = Parse::new(db, z_sql);
    tokenize::sqlite3_run_parser(&mut p);
    let tail = p.z_tail;
    (prepare_result(p), tail)
}

fn prepare_result(mut p: Parse<'_>) -> PrepareResult {
    #[cfg(test)]
    for e in &p.init_errors {
        eprintln!("schema load: {e}");
    }
    #[cfg(not(test))]
    if std::env::var_os("SQLITERS_CG_INITERR").is_some() {
        // Verification aid: the schema rows the generator could not load, and why.
        for e in &p.init_errors {
            eprintln!("schema load: {e}");
        }
    }
    if let Some(why) = p.unsupported {
        return PrepareResult::Unsupported(why);
    }
    if p.z_err_msg.is_some() || (p.rc != sqliteint::SQLITE_OK && p.rc != sqliteint::SQLITE_DONE) {
        return PrepareResult::Error {
            message: p.z_err_msg.clone().unwrap_or_else(|| "SQL logic error".to_owned()),
            offset: p.err_byte_offset,
        };
    }
    if !p.has_vdbe || p.rc != sqliteint::SQLITE_DONE {
        // An empty statement (only white space, comments or ";"): no program.
        return PrepareResult::Unsupported("empty statement");
    }
    PrepareResult::Program(Box::new(build::sqlite3_vdbe_make_ready(&mut p)))
}

/// The `EXPLAIN` listing of a program as text lines `addr|opcode|p1|p2|p3|p4|p5|comment`
/// (the format of `sqlite3 -list`): used by the tests of this crate.
pub fn explain_text(prog: &Program) -> Vec<String> {
    explain_bytes(prog).iter().map(|l| String::from_utf8_lossy(l).into_owned()).collect()
}

/// [`explain_text`] with the exact bytes of the P4 column (the text of a blob literal need not
/// be valid UTF-8).
pub fn explain_bytes(prog: &Program) -> Vec<Vec<u8>> {
    prog.list_ops(1)
        .iter()
        .map(|(addr, op)| {
            let mut l = format!(
                "{}|{}|{}|{}|{}|",
                addr,
                sqliters_vdbe::opcodes::OPCODE_NAME[op.opcode as usize],
                op.p1,
                op.p2,
                op.p3
            )
            .into_bytes();
            if let Some(p4) = sqliters_vdbe::vdbeaux::sqlite3_vdbe_display_p4_bytes(op) {
                l.extend_from_slice(&p4);
            }
            l.extend_from_slice(format!("|{}|", op.p5).as_bytes());
            l
        })
        .collect()
}
