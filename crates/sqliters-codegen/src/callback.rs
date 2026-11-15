//! `callback.c`: access to collating sequences and SQL functions.

use std::sync::Arc;

use sqliters_vdbe::vdbeaux::{CollSeq, FuncDef};

use crate::builtin_funcs::*;
use crate::consts::*;
use crate::sqliteint::*;

/// The collating sequences every connection has (`sqlite3_open`: BINARY, NOCASE, RTRIM).
const BUILTIN_COLLATIONS: [&str; 3] = ["BINARY", "NOCASE", "RTRIM"];

/// `SQLITE_ERROR_MISSING_COLLSEQ`
pub const SQLITE_ERROR_MISSING_COLLSEQ: i32 = SQLITE_ERROR | (1 << 8);

fn intern_coll(p: &mut Parse, name: &str) -> Arc<CollSeq> {
    if let Some(c) = p.a_coll.iter().find(|c| c.name == name) {
        return c.clone();
    }
    let c = Arc::new(CollSeq { name: name.to_owned(), enc: p.db.enc });
    p.a_coll.push(c.clone());
    c
}

/// `db->pDfltColl`: the BINARY collating sequence.
pub fn dflt_coll(p: &mut Parse) -> Arc<CollSeq> {
    intern_coll(p, "BINARY")
}

/// `sqlite3FindCollSeq(db, enc, zName, 0)`: the CollSeq for collation `z_name`, `None` if
/// there is no such collating sequence.
pub fn sqlite3_find_coll_seq(p: &mut Parse, z_name: &str) -> Option<Arc<CollSeq>> {
    if let Some(n) = BUILTIN_COLLATIONS.iter().find(|n| n.eq_ignore_ascii_case(z_name)) {
        return Some(intern_coll(p, n));
    }
    let n = p.db.host.find_collation(z_name)?;
    Some(intern_coll(p, &n))
}

/// `sqlite3GetCollSeq(pParse, enc, pColl, zName)`: like `sqlite3FindCollSeq`, but an error
/// message is left in the Parse if the collating sequence does not exist. (The
/// collation-needed callback is the engine's business: its registry is consulted through
/// [`Host::find_collation`].)
pub fn sqlite3_get_coll_seq(p: &mut Parse, z_name: &str) -> Option<Arc<CollSeq>> {
    let c = sqlite3_find_coll_seq(p, z_name);
    if c.is_none() {
        p.error_msg(format!("no such collation sequence: {z_name}"));
        p.rc = SQLITE_ERROR_MISSING_COLLSEQ;
    }
    c
}

/// `sqlite3LocateCollSeq(pParse, zName)`
pub fn sqlite3_locate_coll_seq(p: &mut Parse, z_name: &str) -> Option<Arc<CollSeq>> {
    let c = sqlite3_find_coll_seq(p, z_name);
    if !p.db.init_busy && c.is_none() {
        return sqlite3_get_coll_seq(p, z_name);
    }
    c
}

/// `FUNC_PERFECT_MATCH`
const FUNC_PERFECT_MATCH: i32 = 6;

/// `matchQuality(p, nArg, enc)`: how well FuncDef `f` matches a call with `n_arg` arguments
/// in a database with encoding `enc`; 0 is no match, larger is better.
fn match_quality(f_n_arg: i32, func_flags: u32, n_arg: i32, enc: u8) -> i32 {
    // Wrong number of arguments means "no match"
    if f_n_arg != n_arg {
        if n_arg == -2 {
            return FUNC_PERFECT_MATCH; // (xSFunc is never NULL for the functions known here)
        }
        if f_n_arg >= 0 {
            return 0;
        }
        // Special p->nArg values available to built-in functions only:
        //    -3     1 or more arguments required
        //    -4     2 or more arguments required
        if f_n_arg < -2 && n_arg < (-2 - f_n_arg) {
            return 0;
        }
    }
    // Give a better score to a function with a specific number of arguments than to function
    // that accepts any number of arguments.
    let mut m = if f_n_arg == n_arg { 4 } else { 1 };
    // Bonus points if the text encoding matches
    if enc as u32 == (func_flags & SQLITE_FUNC_ENCMASK) {
        m += 2;
    } else if (enc as u32 & func_flags & 2) != 0 {
        m += 1;
    }
    m
}

fn intern_func(p: &mut Parse, b: &BuiltinFunc) -> Arc<FuncDef> {
    if let Some(f) = p.a_func.iter().find(|f| f.n_arg == b.1 && f.func_flags == b.2 && f.name == b.0) {
        return f.clone();
    }
    let f = Arc::new(FuncDef {
        n_arg: b.1,
        func_flags: b.2,
        name: b.0.to_owned(),
        user_data: b.3,
        x_finalize: b.4,
        x_value: b.5,
    });
    p.a_func.push(f.clone());
    f
}

/// `sqlite3FindFunction(db, zName, nArg, enc, createFlag=0)`: locate a user function given a
/// name and a number of arguments (`n_arg == -2`: any function of that name). Application
/// functions (`db->aFunc`) are preferred over built-in functions unless `DBFLAG_PreferBuiltin`
/// is set.
pub fn sqlite3_find_function(p: &mut Parse, z_name: &str, n_arg: i32, _create_flag: u8) -> Option<Arc<FuncDef>> {
    if p.db.case_sensitive_like && z_name.eq_ignore_ascii_case("like") {
        // sqlite3RegisterLikeFunctions(): like() is then a connection-level function with
        // other flags and wildcard set; the engine does not implement the pragma yet.
        p.not_supported("like() after PRAGMA case_sensitive_like");
        return None;
    }
    let enc = p.db.enc;
    let mut best: Option<Arc<FuncDef>> = None;
    let mut best_score = 0;
    // db->aFunc: functions registered by the application, then the ones every connection of
    // the reference build registers at open.
    for f in p.db.host.user_functions(z_name) {
        let score = match_quality(f.n_arg as i32, f.func_flags, n_arg, enc);
        if score > best_score {
            best = Some(f);
            best_score = score;
        }
    }
    let mut best_conn: Option<&BuiltinFunc> = None;
    for b in CONNECTION_FUNCS.iter().filter(|b| b.0.eq_ignore_ascii_case(z_name)) {
        let score = match_quality(b.1 as i32, b.2, n_arg, enc);
        if score > best_score {
            best_conn = Some(b);
            best = None;
            best_score = score;
        }
    }
    if let Some(b) = best_conn {
        best = Some(intern_func(p, b));
    }
    if best.is_none() || (p.db.m_db_flags & DBFLAG_PreferBuiltin as u32) != 0 {
        best_score = 0;
        let mut best_b: Option<&BuiltinFunc> = None;
        for b in BUILTIN_FUNCS.iter().filter(|b| b.0.eq_ignore_ascii_case(z_name)) {
            let score = match_quality(b.1 as i32, b.2, n_arg, enc);
            if score > best_score {
                best_b = Some(b);
                best_score = score;
            }
        }
        if let Some(b) = best_b {
            best = Some(intern_func(p, b));
        }
    }
    // if( pBest && (pBest->xSFunc || createFlag) ) return pBest;
    best
}

/// `pDef->xFinalize!=0`
#[inline]
pub fn is_aggregate_def(d: &FuncDef) -> bool {
    d.x_finalize
}
