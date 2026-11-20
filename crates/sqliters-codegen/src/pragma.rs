//! SQLite 3.54.1 pragma.c. The registry is the reference build's generated pragma.h.
//! An unavailable host value or an unported handler remains explicitly unsupported.
use crate::build::*;
use crate::build_tab::{sqlite3_column_expr, sqlite3_find_index, sqlite3_two_part_name};
use crate::consts::*;
use crate::schema::*;
use crate::sqliteint::*;
use crate::util::{sqlite3_dec_or_hex_to_i64, sqlite3_get_int32};
use sqliters_vdbe::opcodes::*;
use sqliters_vdbe::vdbeaux::{COLNAME_NAME, P4};

#[path = "pragma_table.rs"]
mod table;

pub(super) struct PragmaName {
    name: &'static str,
    typ: u8,
    flags: u8,
    column: usize,
    n_column: usize,
    arg: u64,
}

/// Changes pragma.c makes while preparing (before sqlite3_step).
#[derive(Debug, Clone)]
pub enum PrepareEffect {
    Flags {
        flags: u64,
        reset_deferred: bool,
        reset_schema: bool,
    },
    AnalysisLimit(i32),
    CacheSize {
        i_db: usize,
        size: i32,
    },
    JournalSizeLimit {
        i_db: usize,
        limit: i64,
    },
    MmapSize { i_db: Option<usize>, limit: i64 },
    SecureDelete {
        i_db: Option<usize>,
        mode: u8,
    },
    Synchronous { i_db: usize, level: u8 },
    PageSize { i_db: usize, size: i32 },
    AutoVacuum { i_db: usize, mode: u8 },
    CacheSpill { i_db: usize, size: Option<i32>, enabled: bool },
    BusyTimeout(i32),
    WalAutocheckpoint(i32),
    LockingMode { i_db: Option<usize>, mode: u8 },
    TempStore(u8),
    Encoding(u8),
    WorkerThreads(i32),
    ShrinkMemory,
    LikeFunctions(bool),
}

/// Names of the pragmas enabled in the C reference build, in pragma.h order.
pub fn pragma_names() -> impl Iterator<Item = &'static str> {
    table::PRAGMAS.iter().map(|p| p.name)
}

pub fn flag_mask(name: &str) -> Option<u64> {
    locate(name).filter(|p| p.typ == 4).map(|p| p.arg)
}

pub fn flag_masks() -> impl Iterator<Item = u64> {
    table::PRAGMAS.iter().filter(|p| p.typ == 4).map(|p| p.arg)
}

/// PRAGMAs whose complete program is a frozen integer row or an empty result.
pub fn prepare_constant(name: &str) -> bool {
    flag_mask(name).is_some()
        || [
            "analysis_limit",
            "cache_size",
            "journal_size_limit",
            "secure_delete",
            "synchronous",
            "page_size",
            "auto_vacuum",
            "cache_spill",
            "busy_timeout",
            "wal_autocheckpoint",
            "locking_mode",
            "temp_store",
            "encoding",
            "threads",
            "shrink_memory",
        ]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

fn locate(name: &str) -> Option<&'static PragmaName> {
    table::PRAGMAS
        .binary_search_by(|p| p.name.cmp(&name.to_ascii_lowercase()))
        .ok()
        .map(|i| &table::PRAGMAS[i])
}

/// pragma.c:2716–2722, then sqlite3_limit's reference-build hard limit.
pub fn worker_threads_setting(value: &str) -> Option<i32> {
    let (n, rc) = sqlite3_dec_or_hex_to_i64(value);
    (rc == 0 && n >= 0).then(|| ((n & 0x7fff_ffff) as i32).min(SQLITE_MAX_WORKER_THREADS))
}

fn columns(p: &mut Parse, pragma: &PragmaName) {
    p.v.set_num_cols(pragma.n_column.max(1) as i32);
    if pragma.n_column == 0 {
        p.v.set_col_name(0, COLNAME_NAME, Some(pragma.name));
    } else {
        for i in 0..pragma.n_column {
            p.v.set_col_name(
                i as i32,
                COLNAME_NAME,
                Some(table::COLUMN_NAMES[pragma.column + i]),
            );
        }
    }
}

fn return_int(p: &mut Parse, n: i64) {
    p.v.add_int64(1, n);
    p.v.add_op2(OP_ResultRow, 1, 1);
}

/// sqlite3VdbeMultiLoad: integers use OP_Integer, strings use OP_String8/OP_Null.
fn row(p: &mut Parse, values: Vec<PragmaCell>) {
    for (i, value) in values.iter().enumerate() {
        match value {
            PragmaCell::Int(n) => {
                p.v.add_op2(OP_Integer, *n, i as i32 + 1);
            }
            PragmaCell::Text(s) => {
                p.v.load_string(i as i32 + 1, s);
            }
            PragmaCell::Null => {
                p.v.add_op2(OP_Null, 0, i as i32 + 1);
            }
        }
    }
    p.v.add_op2(OP_ResultRow, 1, values.len() as i32);
}

enum PragmaCell {
    Int(i32),
    Text(String),
    Null,
}
impl From<i32> for PragmaCell {
    fn from(v: i32) -> Self {
        Self::Int(v)
    }
}
impl From<String> for PragmaCell {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}
impl From<&str> for PragmaCell {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}
impl From<Option<String>> for PragmaCell {
    fn from(v: Option<String>) -> Self {
        v.map_or(Self::Null, Self::Text)
    }
}

fn host_int(p: &mut Parse, i_db: i32, name: &str) -> Option<i64> {
    let v = p.db.host.pragma_int(i_db as usize, name);
    if v.is_none() {
        p.not_supported("PRAGMA host state (V6b)");
    }
    v
}

/// sqlite3GetBoolean, pragma.c:73–102. Negative numbers use the default.
pub fn get_boolean(z: &str, default: bool) -> bool {
    get_safety_level(z, true, u8::from(default)) != 0
}

/// pragma.c:getSafetyLevel. SYNCHRONOUS handles numeric values separately.
pub fn get_safety_level(z: &str, omit_full: bool, default: u8) -> u8 {
    if z.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return u8::from(sqlite3_get_int32(z).unwrap_or(0) != 0);
    }
    match z.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" => 1,
        "no" | "off" | "false" => 0,
        "full" if !omit_full => 2,
        "extra" if !omit_full => 3,
        _ => default,
    }
}

/// pragma.c:125–132: words are exact; other strings use sqlite3Atoi.
pub fn get_auto_vacuum(z: &str) -> u8 {
    if z.eq_ignore_ascii_case("none") { 0 }
    else if z.eq_ignore_ascii_case("full") { 1 }
    else if z.eq_ignore_ascii_case("incremental") { 2 }
    else { match sqlite3_get_int32(z).unwrap_or(0) { 1 => 1, 2 => 2, _ => 0 } }
}

/// pragma.c:141–151: numeric values use only the first character.
pub fn get_temp_store(z: &str) -> u8 {
    match z.as_bytes().first() {
        Some(c @ b'0'..=b'2') => c - b'0',
        _ if z.eq_ignore_ascii_case("file") => 1,
        _ if z.eq_ignore_ascii_case("memory") => 2,
        _ => 0,
    }
}

/// sqlite3DecOrHexToI64: strict decimal, or unsigned hex with at most 16 significant digits.
fn dec_or_hex(z: &str) -> Option<i64> {
    let (value, rc) = sqlite3_dec_or_hex_to_i64(z);
    (rc == 0).then_some(value)
}

/// The request sent by `PRAGMA max_page_count` to OP_MaxPgcnt. Zero queries the current limit.
#[doc(hidden)]
pub fn max_page_count_request(value: &str) -> u32 {
    dec_or_hex(value)
        .unwrap_or(0)
        .clamp(0, i64::from(SQLITE_MAX_PAGE_COUNT)) as u32
}

/// Matches a journal mode by its SQLite pragma prefix and returns its opcode value and name.
#[doc(hidden)]
pub fn resolve_journal_mode(value: &str) -> Option<(i32, &'static str)> {
    let modes = ["delete", "persist", "off", "truncate", "memory", "wal"];
    modes.into_iter().enumerate()
        .find(|(_, name)| {
            name.as_bytes().get(..value.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(value.as_bytes()))
        })
        .map(|(mode, name)| (mode as i32, name))
}

/// sqlite3Pragma, pragma.c:428. Every recognized but unported handler is rejected.
pub fn sqlite3_pragma(
    p: &mut Parse,
    first: &Token,
    second: &Token,
    value: Option<&Token>,
    minus: bool,
) {
    crate::select::sqlite3_get_vdbe(p);
    p.v.run_only_once();
    p.n_mem = 2;
    let mut name_token = Token::default();
    let i_db = sqlite3_two_part_name(p, first, second, &mut name_token);
    if i_db < 0 {
        return;
    }
    if i_db == 1 {
        sqlite3_open_temp_database(p);
    }
    let name = sqlite3_name_from_token(p, Some(&name_token)).unwrap_or_default();
    let right = if minus {
        value.map(|t| format!("-{}", p.tok_str(t)))
    } else {
        sqlite3_name_from_token(p, value)
    };
    let right = right.as_deref();
    if p.db.x_auth {
        p.not_supported("PRAGMA with an authorizer (V6b)");
        return;
    }
    // pragma.c:484–504 -> main.c:4224: file_control returns SQLITE_ERROR when
    // the named btree is absent. EXPLAIN does not open an unopened temp btree.
    if !p.db.a_db[i_db as usize].has_bt {
        p.n_err += 1;
        p.rc = SQLITE_ERROR;
        return;
    }
    let Some(pragma) = locate(&name) else {
        return;
    };
    if pragma.flags & 1 != 0 {
        sqlite3_read_schema(p);
    }
    if pragma.flags & 2 == 0 && (pragma.flags & 4 == 0 || right.is_none()) {
        columns(p, pragma);
    }
    let z_db = if second.n > 0 {
        Some(p.db.a_db[i_db as usize].z_db_s_name.clone())
    } else {
        None
    };
    match pragma.typ {
        29 => {
            // pragma.c:1489–1497, Hash iteration order at prepare time.
            let Some(names) = p.db.host.pragma_module_names() else {
                p.not_supported("PRAGMA module_list registry (V6b)");
                return;
            };
            p.n_mem = 1;
            for name in names {
                row(p, vec![name.into()]);
            }
        }
        28 => {
            // pragma.c:951–987: conversion status is deliberately ignored.
            let request = right.map(|z| sqlite3_dec_or_hex_to_i64(z).0.max(0));
            if let Some(limit) = request {
                p.db.host.note_pragma_effect(PrepareEffect::MmapSize {
                    i_db: z_db.as_ref().map(|_| i_db as usize),
                    limit,
                });
            }
            let Some(result) = p.db.host.pragma_mmap_size(i_db as usize, request) else {
                p.not_supported("PRAGMA mmap_size file control (V6b)");
                return;
            };
            match result {
                Ok(Some(limit)) => return_int(p, limit),
                Ok(None) => {},
                Err(error) => {
                    p.n_err += 1;
                    p.rc = error.extended_code;
                }
            }
        }
        1 => {
            // PragTyp_ANALYSIS_LIMIT, pragma.c:2733
            let n = right
                .and_then(dec_or_hex)
                .filter(|n| *n >= 0)
                .map_or(p.db.n_analysis_limit, |n| (n & 0x7fff_ffff) as i32);
            if right.and_then(dec_or_hex).is_some_and(|n| n >= 0) {
                p.db.n_analysis_limit = n;
                p.db.host
                    .note_pragma_effect(PrepareEffect::AnalysisLimit(n));
            }
            return_int(p, i64::from(n));
        }
        2 => {
            // PragTyp_HEADER_VALUE, pragma.c:2330
            p.v.uses_btree(i_db, p.db.a_db[i_db as usize].sharable);
            if right.is_some() && pragma.flags & 8 == 0 {
                p.v.add_op2(OP_Transaction, i_db, 1);
                p.v.add_op3(
                    if pragma.arg == 1 && p.db.flags & SQLITE_Defensive != 0 {
                        OP_Noop
                    } else {
                        OP_SetCookie
                    },
                    i_db,
                    pragma.arg as i32,
                    sqlite3_get_int32(right.unwrap()).unwrap_or(0),
                );
                p.v.change_p5(1);
            } else {
                p.v.add_op2(OP_Transaction, i_db, 0);
                p.v.add_op3(OP_ReadCookie, i_db, 1, pragma.arg as i32);
                p.v.add_op2(OP_ResultRow, 1, 1);
                p.v.reusable();
            }
        }
        3 => {
            // pragma.c:802–845: prepare changes flags; the program writes meta[7].
            if let Some(z) = right {
                let mode = get_auto_vacuum(z);
                let Some(current) = host_int(p, i_db, "auto_vacuum") else { return; };
                let Some(fixed) = host_int(p, i_db, "auto_vacuum_fixed") else { return; };
                p.db.host.note_pragma_effect(PrepareEffect::AutoVacuum { i_db: i_db as usize, mode });
                if (fixed == 0 || (mode != 0) == (current != 0)) && mode != 0 {
                    let addr = p.v.current_addr();
                    p.v.add_op3(OP_Transaction, i_db, 1, 0);
                    p.v.add_op3(OP_ReadCookie, i_db, 1, 4);
                    p.v.add_op3(OP_If, 1, addr + 4, 0);
                    p.v.add_op3(OP_Halt, SQLITE_OK, OE_Abort, 0);
                    p.v.add_op3(OP_SetCookie, i_db, 7, i32::from(mode) - 1);
                    p.v.uses_btree(i_db, p.db.a_db[i_db as usize].sharable);
                }
            } else if let Some(n) = host_int(p, i_db, "auto_vacuum") { return_int(p, n); }
        }
        4 => {
            // PragTyp_FLAG, pragma.c:1158
            if right.is_none() {
                columns(p, pragma);
                return_int(p, i64::from(p.db.flags & pragma.arg != 0));
            } else {
                let z = right.unwrap();
                let mut mask = pragma.arg;
                if !p.db.auto_commit {
                    mask &= !SQLITE_ForeignKeys;
                }
                let mut reset_deferred = false;
                let mut reset_schema = false;
                if get_boolean(z, false) {
                    if mask & SQLITE_WriteSchema == 0 || p.db.flags & SQLITE_Defensive == 0 {
                        p.db.flags |= mask;
                    }
                } else {
                    p.db.flags &= !mask;
                    reset_deferred = mask == SQLITE_DeferFKs;
                    reset_schema =
                        mask & SQLITE_WriteSchema != 0 && z.eq_ignore_ascii_case("reset");
                }
                p.db.host.note_pragma_effect(PrepareEffect::Flags {
                    flags: p.db.flags,
                    reset_deferred,
                    reset_schema,
                });
                p.v.add_op0(OP_Expire);
            }
        }
        5 => {
            // pragma.c:2656–2662: an invalid integer leaves the handler unchanged.
            let value = right.and_then(sqlite3_get_int32).map(|n| n.max(0));
            if let Some(n) = value { p.db.host.note_pragma_effect(PrepareEffect::BusyTimeout(n)); }
            if let Some(n) = value.map(i64::from).or_else(|| host_int(p, i_db, "busy_timeout")) { return_int(p, n); }
        }
        6 => {
            // pragma.c:882–889: Schema.cache_size, changed during prepare.
            if let Some(z) = right {
                p.db.host.note_pragma_effect(PrepareEffect::CacheSize {
                    i_db: i_db as usize,
                    size: sqlite3_get_int32(z).unwrap_or(0),
                });
            } else if let Some(n) = host_int(p, i_db, "cache_size") {
                // Reading the schema may initialize the pager's cache size.
                p.db.host.note_pragma_effect(PrepareEffect::CacheSize {
                    i_db: i_db as usize,
                    size: n as i32,
                });
                return_int(p, n);
            }
        }
        7 => {
            // pragma.c:917–933. This pragma does not read the schema.
            if let Some(z) = right {
                let size = sqlite3_get_int32(z);
                let enabled = get_boolean(z, size.unwrap_or(1) != 0);
                p.db.host.note_pragma_effect(PrepareEffect::CacheSpill { i_db: i_db as usize, size, enabled });
            } else if p.db.flags & SQLITE_CacheSpill == 0 {
                return_int(p, 0);
            } else if let Some(n) = host_int(p, i_db, "cache_spill") { return_int(p, n); }
        }
        8 => {
            if let Some(z) = right {
                p.db.host.note_pragma_effect(PrepareEffect::LikeFunctions(get_boolean(z, false)));
            }
        }
        13 => {
            // PragTyp_DEFAULT_CACHE_SIZE, pragma.c:553
            p.v.uses_btree(i_db, p.db.a_db[i_db as usize].sharable);
            if let Some(z) = right {
                let n = sqlite3_get_int32(z).unwrap_or(0);
                let n = if n == i32::MIN { i32::MAX } else { n.abs() };
                sqlite3_begin_write_operation(p, false, i_db);
                p.v.add_op3(OP_SetCookie, i_db, 3, n);
                p.db.host.note_pragma_effect(PrepareEffect::CacheSize {
                    i_db: i_db as usize,
                    size: n,
                });
            } else {
                p.n_mem += 2;
                let base = p.v.current_addr();
                for (op, a, b, c) in [
                    (OP_Transaction, i_db, 0, 0),
                    (OP_ReadCookie, i_db, 1, 3),
                    (OP_IfPos, 1, base + 8, 0),
                    (OP_Integer, 0, 2, 0),
                    (OP_Subtract, 1, 2, 1),
                    (OP_IfPos, 1, base + 8, 0),
                    (OP_Integer, -2000, 1, 0),
                    (OP_Noop, 0, 0, 0),
                    (OP_ResultRow, 1, 1, 0),
                ] {
                    p.v.add_op3(op, a, b, c);
                }
            }
        }
        14 => {
            // pragma.c:2250–2297; callback.c:181–188. Encoding setters
            // expire pending statements during prepare, even for the same value.
            if let Some(z) = right {
                if !p.db.encoding_fixed {
                    let enc = match z.to_ascii_lowercase().as_str() {
                        "utf8" | "utf-8" => Some(1),
                        "utf16le" | "utf-16le" => Some(2),
                        "utf16be" | "utf-16be" => Some(3),
                        "utf16" | "utf-16" => Some(if cfg!(target_endian = "little") { 2 } else { 3 }),
                        _ => None,
                    };
                    if let Some(enc) = enc {
                        p.db.enc = enc;
                        p.db.host.note_pragma_effect(PrepareEffect::Encoding(enc));
                    } else { p.error_msg(&format!("unsupported encoding: {z}")); }
                }
            } else {
                sqlite3_read_schema(p);
                if p.n_err != 0 { return; }
                p.v.load_string(1, match p.db.enc { 2 => "UTF-16le", 3 => "UTF-16be", _ => "UTF-8" });
                p.v.add_op2(OP_ResultRow, 1, 1);
            }
        }
        9 => {
            // pragma.c:1456–1463: snapshot the live hash, without sorting names.
            let Some(names) = p.db.host.pragma_collations() else {
                p.not_supported("PRAGMA collation_list registry (V6b)");
                return;
            };
            p.n_mem = 2;
            for (i, name) in names.into_iter().enumerate() {
                row(p, vec![PragmaCell::Int(i as i32), name.into()]);
            }
        }
        10 => {
            // pragma.c:2379–2387: this constant list is explicitly reusable.
            p.n_mem = 1;
            for &option in sqliters_types::SQLITE_REFERENCE_COMPILE_OPTIONS {
                row(p, vec![option.into()]);
            }
            p.v.reusable();
        }
        12 => {
            // pragma.c:1442–1453: names, membership and filenames freeze at prepare.
            p.n_mem = 3;
            for i in 0..p.db.a_db.len() {
                if !p.db.a_db[i].has_bt { continue; }
                let name = p.db.a_db[i].z_db_s_name.clone();
                let Some(filename) = p.db.host.pragma_text(i, "filename") else {
                    p.not_supported("PRAGMA database_list filename (V6b)");
                    return;
                };
                row(p, vec![PragmaCell::Int(i as i32), name.into(), filename.into()]);
            }
        }
        15 => foreign_key_check(p, i_db, right, z_db.as_deref()),
        16 => foreign_key_list(p, right, z_db.as_deref()),
        19 => {
            // incremental_vacuum, pragma.c:861
            let limit = right
                .and_then(sqlite3_get_int32)
                .filter(|n| *n > 0)
                .unwrap_or(i32::MAX);
            sqlite3_begin_write_operation(p, false, i_db);
            p.v.add_op2(OP_Integer, limit, 1);
            let addr = p.v.add_op1(OP_IncrVacuum, i_db);
            p.v.add_op1(OP_ResultRow, 1);
            p.v.add_op2(OP_AddImm, 1, -1);
            p.v.add_op2(OP_IfPos, 1, addr);
            p.v.jump_here(addr);
        }
        20 => index_info(p, pragma.arg != 0, right, z_db.as_deref()),
        21 => index_list(p, right, z_db.as_deref()),
        26 => {
            // pragma.c:687–727: a bare query reads the connection default;
            // qualified queries and successful assignments read the pager mode.
            let mode = right.and_then(|z| if z.eq_ignore_ascii_case("exclusive") { Some(1u8) }
                else if z.eq_ignore_ascii_case("normal") { Some(0) } else { None });
            let result = if z_db.is_none() && mode.is_none() {
                host_int(p, i_db, "default_locking_mode")
            } else {
                let Some(current) = host_int(p, i_db, "locking_mode") else { return; };
                let result = if let Some(mode) = mode {
                    let Some(can_set) = host_int(p, i_db, "locking_mode_settable") else { return; };
                    p.db.host.note_pragma_effect(PrepareEffect::LockingMode {
                        i_db: z_db.as_ref().map(|_| i_db as usize), mode,
                    });
                    if can_set != 0 { i64::from(mode) } else { current }
                } else { current };
                Some(result)
            };
            if let Some(mode) = result {
                p.v.load_string(1, if mode != 0 { "exclusive" } else { "normal" });
                p.v.add_op2(OP_ResultRow, 1, 1);
            }
        }
        23 => {
            // journal_mode, pragma.c:733
            let mut mode = right.and_then(resolve_journal_mode).map_or(-1, |(mode, _)| mode);
            if mode == 2 && p.db.flags & SQLITE_Defensive != 0 {
                mode = -1;
            }
            for i in (0..p.db.a_db.len()).rev() {
                if p.db.a_db[i].has_bt && (i == i_db as usize || (z_db.is_none() && mode >= 0)) {
                    p.v.uses_btree(i as i32, p.db.a_db[i].sharable);
                    p.v.add_op3(OP_JournalMode, i as i32, 1, mode);
                }
            }
            p.v.add_op2(OP_ResultRow, 1, 1);
        }
        24 => {
            // pragma.c:780–788: the conversion's value is used even on error.
            let limit = if let Some(z) = right {
                let n = sqlite3_dec_or_hex_to_i64(z).0.max(-1);
                p.db.host
                    .note_pragma_effect(PrepareEffect::JournalSizeLimit {
                        i_db: i_db as usize,
                        limit: n,
                    });
                Some(n)
            } else {
                host_int(p, i_db, "journal_size_limit")
            };
            if let Some(n) = limit {
                return_int(p, n);
            }
        }
        27 => {
            // page_count / max_page_count, pragma.c:673
            sqlite3_code_verify_schema(p, i_db);
            p.n_mem += 1;
            if name.as_bytes()[0].eq_ignore_ascii_case(&b'p') {
                p.v.add_op2(OP_Pagecount, i_db, p.n_mem);
            } else {
                let n = right.map_or(0, max_page_count_request) as i32;
                p.v.add_op3(OP_MaxPgcnt, i_db, p.n_mem, n);
            }
            p.v.add_op2(OP_ResultRow, p.n_mem, 1);
        }
        30 => optimize(p, i_db, right, z_db.as_deref()),
        31 => {
            // page_size: a setter emits no program
            if right.is_none() {
                if let Some(n) = host_int(p, i_db, "page_size") {
                    return_int(p, n);
                }
            } else {
                p.db.host.note_pragma_effect(PrepareEffect::PageSize { i_db: i_db as usize, size: sqlite3_get_int32(right.unwrap()).unwrap_or(0) });
            }
        }
        32 => {
            for name in pragma_names() {
                row(p, vec![name.into()]);
            }
        }
        33 => {
            // pragma.c:624–640: an unqualified assignment applies to all Btrees.
            let mode = if let Some(z) = right {
                let mode = if z.eq_ignore_ascii_case("fast") {
                    2
                } else {
                    u8::from(get_boolean(z, false))
                };
                p.db.host.note_pragma_effect(PrepareEffect::SecureDelete {
                    i_db: z_db.as_ref().map(|_| i_db as usize),
                    mode,
                });
                Some(i64::from(mode))
            } else {
                host_int(p, i_db, "secure_delete")
            };
            if let Some(n) = mode {
                return_int(p, n);
            }
        }
        34 => {
            // pragma.c:2444–2446: sqlite3_db_release_memory runs while preparing.
            p.db.host.note_pragma_effect(PrepareEffect::ShrinkMemory);
        }
        36 => {
            // pragma.c:1132–1153: pDb->safety_level is one greater than the result.
            if let Some(z) = right {
                if !p.db.auto_commit {
                    p.error_msg("Safety level may not be changed inside a transaction");
                } else if i_db != 1 {
                    let level = if z.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                        sqlite3_get_int32(z).unwrap_or(0)
                    } else {
                        i32::from(get_safety_level(z, false, 1))
                    };
                    let level = ((level.wrapping_add(1) & 7).max(1) - 1) as u8;
                    p.db.host.note_pragma_effect(PrepareEffect::Synchronous { i_db: i_db as usize, level });
                }
            } else if let Some(n) = host_int(p, i_db, "synchronous") { return_int(p, n); }
        }
        37 => table_info(p, pragma.arg != 0, right, z_db.as_deref()),
        38 => table_list(p, right, z_db.as_deref()),
        39 => {
            // pragma.c:159–192,992–997: changeTempStorage runs during prepare.
            let Some(current) = host_int(p, i_db, "temp_store") else { return; };
            if let Some(z) = right {
                let mode = get_temp_store(z);
                if i64::from(mode) != current {
                    let Some(temp_txn) = host_int(p, 1, "temp_transaction") else { return; };
                    if p.db.a_db[1].has_bt && (!p.db.auto_commit || temp_txn != 0) {
                        p.error_msg("temporary storage cannot be changed from within a transaction");
                        return;
                    }
                    p.db.host.note_pragma_effect(PrepareEffect::TempStore(mode));
                }
            } else { return_int(p, current); }
        }
        41 => {
            if let Some(n) = right.and_then(worker_threads_setting) {
                p.db.a_limit[SQLITE_LIMIT_WORKER_THREADS as usize] = n;
                p.db.host.note_pragma_effect(PrepareEffect::WorkerThreads(n));
            }
            return_int(p, i64::from(p.db.a_limit[SQLITE_LIMIT_WORKER_THREADS as usize]));
        }
        42 => {
            // pragma.c:2428–2435: sqlite3Atoi failure disables the callback.
            let value = right.map(|z| sqlite3_get_int32(z).unwrap_or(0).max(0));
            if let Some(n) = value { p.db.host.note_pragma_effect(PrepareEffect::WalAutocheckpoint(n)); }
            if let Some(n) = value.map(i64::from).or_else(|| host_int(p, i_db, "wal_autocheckpoint")) { return_int(p, n); }
        }
        43 => {
            // wal_checkpoint, pragma.c:2407
            p.n_mem = 3;
            let mode = match right.unwrap_or("").to_ascii_lowercase().as_str() {
                "full" => 1,
                "restart" => 2,
                "truncate" => 3,
                "noop" => -1, // SQLITE_CHECKPOINT_NOOP, sqlite3.h
                _ => 0,
            };
            p.v.add_op3(
                OP_Checkpoint,
                if z_db.is_some() { i_db } else { 12 },
                mode,
                1,
            );
            p.v.add_op2(OP_ResultRow, 1, 3);
        }
        _ => p.not_supported("PRAGMA handler (V6b)"),
    }
}

fn schema_tables(p: &Parse, i_db: usize) -> Vec<TableId> {
    p.a_schema[i_db].tbl_order.ids()
}

fn table_list(p: &mut Parse, right: Option<&str>, z_db: Option<&str>) {
    p.n_mem = 6;
    crate::ddl::sqlite3_code_verify_named_schema(p, z_db);
    for i in 0..p.db.a_db.len() {
        let db = p.db.a_db[i].z_db_s_name.clone();
        if z_db.is_some_and(|name| !name.eq_ignore_ascii_case(&db)) {
            continue;
        }
        // pragma.c:1292–1320: the dummy SELECT names the table WITHOUT a schema.
        // A name in main/temp can therefore hide a view of an attached database,
        // leaving that view's nCol at zero. Restart the hash scan as C does.
        for _ in 0..schema_tables(p, i).len() {
            let Some(tab) = schema_tables(p, i)
                .into_iter()
                .find(|&tab| p.tab(tab).a_col.is_empty())
            else {
                break;
            };
            let name = p.tab(tab).z_name.clone();
            let resolved = sqlite3_locate_table(p, 0, &name, None);
            if resolved.nn() && resolved != tab {
                // C can also retain columns initialized by an earlier use of this
                // hidden view. Schema SQL alone does not represent that history.
                p.not_supported("PRAGMA table_list hidden view initialization history (V6b)");
                return;
            }
            if resolved.is_null() || crate::select::sqlite3_view_get_column_names(p, resolved) != 0
            {
                p.not_supported("PRAGMA table_list unresolved view/vtab (V6b)");
                return;
            }
        }
        for tab in schema_tables(p, i) {
            let table = p.tab(tab);
            if right.is_some_and(|name| !name.eq_ignore_ascii_case(&table.z_name)) {
                continue;
            }
            let name = match table.z_name.as_str() {
                "sqlite_master" => "sqlite_schema",
                "sqlite_temp_master" => "sqlite_temp_schema",
                name => name,
            };
            let typ = if table.is_view() {
                "view"
            } else if table.is_virtual() {
                "virtual"
            } else if table.tab_flags & TF_Shadow != 0 {
                "shadow"
            } else {
                "table"
            };
            let vals = vec![
                db.clone().into(),
                name.into(),
                typ.into(),
                i32::from(table.n_col()).into(),
                i32::from(!table.has_rowid()).into(),
                i32::from(table.tab_flags & TF_Strict != 0).into(),
            ];
            row(p, vals);
        }
    }
}

fn foreign_key_check(p: &mut Parse, i_db: i32, right: Option<&str>, z_db: Option<&str>) {
    use crate::expr::sqlite3_touch_register;
    use crate::exprcode::sqlite3_expr_code_get_column_of_table;
    use crate::fkey::sqlite3_fk_locate_index;
    let result = p.n_mem + 1;
    p.n_mem += 5;
    let reg_row = p.n_mem;
    let tables = if let Some(name) = right {
        // C only calls LocateTable if the original database hash is nonempty.
        if schema_tables(p, i_db as usize).is_empty() {
            Vec::new()
        } else {
            vec![sqlite3_locate_table(p, 0, name, z_db)]
        }
    } else {
        schema_tables(p, i_db as usize)
    };
    for tab in tables {
        if tab.is_null() || !p.tab(tab).is_ordinary() || p.tab(tab).p_fkey.is_empty() {
            continue;
        }
        let table = p.tab(tab).clone();
        let db = p.db.a_db[table.i_db as usize].z_db_s_name.clone();
        sqlite3_code_verify_schema(p, table.i_db);
        sqlite3_touch_register(p, table.n_col() as i32 + reg_row);
        sqlite3_open_table(p, 0, table.i_db, tab, OP_OpenRead);
        p.v.load_string(result, &table.z_name);
        let count = table.p_fkey.len();
        for (k, fk) in table.p_fkey.iter().enumerate() {
            let parent = sqlite3_find_table(p, &fk.z_to, Some(&db));
            if parent.is_null() {
                continue;
            }
            let Ok((idx, _)) = sqlite3_fk_locate_index(p, parent, (tab, k), false) else {
                return;
            };
            if idx.is_null() {
                sqlite3_open_table(p, k as i32 + 1, table.i_db, parent, OP_OpenRead);
            } else {
                p.v.add_op3(
                    OP_OpenRead,
                    k as i32 + 1,
                    p.idx(idx).tnum as i32,
                    table.i_db,
                );
                sqlite3_vdbe_set_p4_key_info(p, idx);
            }
        }
        p.n_tab = p.n_tab.max(count as i32 + 1);
        let top = p.v.add_op1(OP_Rewind, 0);
        for (k, fk) in table.p_fkey.iter().enumerate() {
            let parent = sqlite3_find_table(p, &fk.z_to, Some(&db));
            let (idx, cols) = if parent.nn() {
                let Ok(pair) = sqlite3_fk_locate_index(p, parent, (tab, k), true) else {
                    return;
                };
                pair
            } else {
                (IndexId::NULL, None)
            };
            let ok = p.v.make_label();
            sqlite3_touch_register(p, reg_row + fk.a_col.len() as i32);
            for j in 0..fk.a_col.len() {
                let col = cols.as_ref().map_or(fk.a_col[j].0, |a| a[j]);
                sqlite3_expr_code_get_column_of_table(p, tab, 0, col, reg_row + j as i32);
                p.v.add_op2(OP_IsNull, reg_row + j as i32, ok);
            }
            if idx.nn() {
                let mut affinity = sqlite3_index_affinity_str(p, idx);
                affinity.truncate(fk.a_col.len());
                p.v.add_op4(
                    OP_Affinity,
                    reg_row,
                    fk.a_col.len() as i32,
                    0,
                    P4::Dynamic(affinity),
                );
                p.v.add_op4_int(OP_Found, k as i32 + 1, ok, reg_row, fk.a_col.len() as i32);
            } else if parent.nn() {
                p.v.add_op3(OP_SeekRowid, k as i32 + 1, p.v.current_addr() + 2, reg_row);
                p.v.goto(ok);
            }
            if table.has_rowid() {
                p.v.add_op2(OP_Rowid, 0, result + 1);
            } else {
                p.v.add_op2(OP_Null, 0, result + 1);
            }
            p.v.load_string(result + 2, &fk.z_to);
            p.v.add_op2(OP_Integer, k as i32, result + 3);
            p.v.add_op2(OP_ResultRow, result, 4);
            p.v.resolve_label(ok);
        }
        p.v.add_op2(OP_Next, 0, top + 1);
        p.v.jump_here(top);
    }
}

fn optimize(p: &mut Parse, first_db: i32, right: Option<&str>, z_db: Option<&str>) {
    let mask = right.map_or(0xfffe, |z| sqlite3_get_int32(z).unwrap_or(0)) as u32;
    if mask & 2 == 0 {
        return;
    }
    let mut limit =
        if mask & 0x10 == 0 || (p.db.n_analysis_limit > 0 && p.db.n_analysis_limit < 2000) {
            0
        } else {
            2000
        };
    let cursor = p.n_tab;
    p.n_tab += 1;
    let last_db = if z_db.is_some() {
        first_db as usize
    } else {
        p.db.a_db.len() - 1
    };
    let mut checks = 0;
    let mut btrees = 0;
    for i_db in first_db as usize..=last_db {
        if i_db == 1 {
            continue;
        }
        if mask & 0x10000 == 0 && !p.db.host.stat1_rows(i_db).is_empty() {
            // TF_MaybeReanalyze is connection history, not derivable from sqlite_stat1.
            p.not_supported("PRAGMA optimize statistics usage history (V6b)");
            return;
        }
        sqlite3_code_verify_schema(p, i_db as i32);
        for tab in schema_tables(p, i_db) {
            let table = p.tab(tab).clone();
            if let Some(reason) = p.db.host.table_unsupported(i_db, &table.z_name) {
                p.not_supported(reason);
                return;
            }
            if !table.is_ordinary()
                || table
                    .z_name
                    .as_bytes()
                    .get(..7)
                    .is_some_and(|s| s.eq_ignore_ascii_case(b"sqlite_"))
            {
                continue;
            }
            let mut threshold = table.n_row_log_est;
            for &idx in &table.p_index {
                if !p.idx(idx).has_stat1 {
                    threshold = -1;
                }
            }
            if table.tab_flags & TF_MaybeReanalyze == 0
                && mask & 0x10000 == 0
                && (table.p_index.is_empty() || threshold >= 0)
            {
                continue;
            }
            checks += 1;
            if checks == 2 {
                sqlite3_begin_write_operation(p, false, i_db as i32);
            }
            btrees += table.p_index.len() + 1;
            sqlite3_open_table(p, cursor, i_db as i32, tab, OP_OpenRead);
            let dest = p.v.current_addr() + 2 + (mask & 1) as i32;
            if threshold >= 0 {
                p.v.add_op4_int(
                    OP_IfSizeBetween,
                    cursor,
                    dest,
                    if threshold >= 33 {
                        threshold as i32 - 33
                    } else {
                        -1
                    },
                    threshold as i32 + 33,
                );
            } else {
                p.v.add_op2(OP_Rewind, cursor, dest);
            }
            let sql = format!(
                "ANALYZE \"{}\".\"{}\"",
                p.db.a_db[i_db].z_db_s_name.replace('"', "\"\""),
                table.z_name.replace('"', "\"\"")
            );
            if mask & 1 != 0 {
                let reg = crate::expr::sqlite3_get_temp_reg(p);
                p.v.load_string(reg, &sql);
                p.v.add_op2(OP_ResultRow, reg, 1);
            } else {
                p.v.add_op4(
                    OP_SqlExec,
                    if limit != 0 { 2 } else { 0 },
                    limit,
                    0,
                    P4::dynamic(&sql),
                );
            }
        }
    }
    p.v.add_op0(OP_Expire);
    if limit > 0 && btrees > 100 {
        limit = (100 * limit / btrees as i32).max(100);
        for op in &mut p.v.a_op {
            if op.opcode == OP_SqlExec {
                op.p2 = limit;
            }
        }
    }
}

fn table_info(p: &mut Parse, extended: bool, right: Option<&str>, z_db: Option<&str>) {
    let Some(name) = right else {
        return;
    };
    crate::ddl::sqlite3_code_verify_named_schema(p, z_db);
    let tab = sqlite3_locate_table(p, LOCATE_NOERR as u32, name, z_db);
    if tab.is_null() {
        return;
    }
    p.n_mem = 7;
    crate::select::sqlite3_view_get_column_names(p, tab);
    let pk = sqlite3_primary_key_index(p, tab);
    let table = p.tab(tab).clone();
    let mut hidden = 0;
    for (i, col) in table.a_col.iter().enumerate() {
        let mut kind = 0;
        if col.col_flags & (COLFLAG_NOINSERT as u16) != 0 {
            if !extended {
                hidden += 1;
                continue;
            }
            kind = if col.col_flags & (COLFLAG_VIRTUAL as u16) != 0 {
                2
            } else if col.col_flags & (COLFLAG_STORED as u16) != 0 {
                3
            } else {
                1
            };
        }
        let key = if col.col_flags & (COLFLAG_PRIMKEY as u16) == 0 {
            0
        } else if pk.is_null() {
            1
        } else {
            p.idx(pk)
                .ai_column
                .iter()
                .position(|&n| n == i as i16)
                .map_or(table.a_col.len() + 1, |n| n + 1) as i32
        };
        let expr = sqlite3_column_expr(p, tab, i);
        let default = if kind >= 2 || expr.is_null() {
            None
        } else {
            Some(p.e(expr).z_token.clone().unwrap_or_default())
        };
        let mut vals = vec![
            (i as i32 - hidden).into(),
            col.z_cn_name.clone().into(),
            sqlite3_column_type(col, Some("")).unwrap_or("").into(),
            i32::from(col.not_null != 0).into(),
            default.into(),
            key.into(),
        ];
        if extended {
            vals.push(kind.into());
        }
        row(p, vals);
    }
}

fn index_info(p: &mut Parse, extended: bool, right: Option<&str>, z_db: Option<&str>) {
    let Some(name) = right else {
        return;
    };
    let mut idx = sqlite3_find_index(p, name, z_db);
    if idx.is_null() {
        let tab = sqlite3_locate_table(p, LOCATE_NOERR as u32, name, z_db);
        if tab.nn() && !p.tab(tab).has_rowid() {
            idx = sqlite3_primary_key_index(p, tab);
        }
    }
    if idx.is_null() {
        return;
    }
    let index = p.idx(idx).clone();
    p.n_mem = if extended { 6 } else { 3 };
    sqlite3_code_verify_schema(p, index.i_db);
    let n = if extended {
        index.n_column as usize
    } else {
        index.n_key_col as usize
    };
    for i in 0..n {
        let col = index.ai_column[i];
        let name = if col < 0 {
            None
        } else {
            Some(p.tab(index.p_table).a_col[col as usize].z_cn_name.clone())
        };
        let mut vals = vec![(i as i32).into(), (col as i32).into(), name.into()];
        if extended {
            vals.extend([
                i32::from(index.a_sort_order[i]).into(),
                index.az_coll[i].clone().into(),
                i32::from(i < index.n_key_col as usize).into(),
            ]);
        }
        row(p, vals);
    }
}

fn index_list(p: &mut Parse, right: Option<&str>, z_db: Option<&str>) {
    let Some(name) = right else {
        return;
    };
    let tab = sqlite3_find_table(p, name, z_db);
    if tab.is_null() {
        return;
    }
    p.n_mem = 5;
    sqlite3_code_verify_schema(p, p.tab(tab).i_db);
    for (i, id) in p.tab(tab).p_index.clone().iter().enumerate() {
        let idx = p.idx(*id);
        let vals = vec![
            (i as i32).into(),
            idx.z_name.clone().into(),
            i32::from(idx.is_unique()).into(),
            ["c", "u", "pk"][idx.idx_type as usize].into(),
            i32::from(idx.p_part_idx_where.nn()).into(),
        ];
        row(p, vals);
    }
}

fn foreign_key_list(p: &mut Parse, right: Option<&str>, z_db: Option<&str>) {
    let Some(name) = right else {
        return;
    };
    let tab = sqlite3_find_table(p, name, z_db);
    if tab.is_null() || !p.tab(tab).is_ordinary() || p.tab(tab).p_fkey.is_empty() {
        return;
    }
    p.n_mem = 8;
    sqlite3_code_verify_schema(p, p.tab(tab).i_db);
    let table = p.tab(tab).clone();
    let action = |n| match n {
        OE_SetNull => "SET NULL",
        OE_SetDflt => "SET DEFAULT",
        OE_Cascade => "CASCADE",
        OE_Restrict => "RESTRICT",
        _ => "NO ACTION",
    };
    for (i, fk) in table.p_fkey.iter().enumerate() {
        for (j, (from, to)) in fk.a_col.iter().enumerate() {
            row(
                p,
                vec![
                    (i as i32).into(),
                    (j as i32).into(),
                    fk.z_to.clone().into(),
                    table.a_col[*from as usize].z_cn_name.clone().into(),
                    to.clone().into(),
                    action(i32::from(fk.a_action[1])).into(),
                    action(i32::from(fk.a_action[0])).into(),
                    "NONE".into(),
                ],
            );
        }
    }
}
