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
use sqliters_codegen::sqliteint::{VTable, VtabConnect, VtabModule};
use sqliters_codegen::vtab::{IndexInfo, IndexInfoHidden};
use sqliters_codegen::{Db, DbInfo, Host, PrepareResult};
use sqliters_types::{
    ErrorCode, FunctionFlags, TextEncoding, VTabConstraint, VTabConstraintOp, VTabIndexInfo, VTabIndexInfoCalls, VTabOrderBy,
    VTabRiskFlags, Value,
};
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
    /// The virtual-table instances handed to the generator: id -> (database, table)
    vtabs: std::cell::RefCell<Vec<(u64, String, String)>>,
}

/// `SQLITE_INDEX_CONSTRAINT_*` -> the engine's operator.
fn vtab_op_from_c(op: u8) -> VTabConstraintOp {
    match op {
        2 => VTabConstraintOp::Eq,
        4 => VTabConstraintOp::Gt,
        8 => VTabConstraintOp::Le,
        16 => VTabConstraintOp::Lt,
        32 => VTabConstraintOp::Ge,
        64 => VTabConstraintOp::Match,
        65 => VTabConstraintOp::Like,
        66 => VTabConstraintOp::Glob,
        67 => VTabConstraintOp::Regexp,
        68 => VTabConstraintOp::Ne,
        69 => VTabConstraintOp::IsNot,
        70 => VTabConstraintOp::IsNotNull,
        71 => VTabConstraintOp::IsNull,
        72 => VTabConstraintOp::Is,
        73 => VTabConstraintOp::Limit,
        74 => VTabConstraintOp::Offset,
        x => VTabConstraintOp::Function(x),
    }
}

/// The calls of an engine module into the generator's `HiddenIndexInfo` during xBestIndex.
struct CallsAdapter<'x> {
    h: &'x mut dyn IndexInfoHidden,
}

impl VTabIndexInfoCalls for CallsAdapter<'_> {
    fn vtab_in(&mut self, i_cons: usize, b_handle: i32) -> bool {
        self.h.vtab_in(i_cons as i32, b_handle)
    }
    fn rhs_value(&mut self, i_cons: usize) -> Option<Value> {
        self.h.rhs_value(i_cons as i32).1
    }
    fn distinct(&self) -> i32 {
        self.h.distinct()
    }
    fn collation(&mut self, i_cons: usize) -> String {
        self.h.collation(i_cons as i32).unwrap_or_else(|| "BINARY".to_owned())
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
    fn note_schema_loaded(&self) {
        self.schema_loaded.set(true);
    }

    fn note_temp_opened(&self) {
        C_TEMP_OPENED.with(|c| c.set(true));
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
                SchemaRow { ty: "trigger".into(), name: tr.name.clone(), tbl_name: tr.tbl_name.clone(), rootpage: 0, sql: Some(tr.raw_sql.clone()) },
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

    fn rowest_clamped_tables(&self, i_db: usize) -> Vec<String> {
        self.db_set
            .slots()
            .get(i_db)
            .map_or_else(Vec::new, |s| s.c_rowest_tables.iter().cloned().collect())
    }

    fn note_rowest_clamped(&self, i_db: usize, table: &str) {
        C_ROWEST.with(|c| c.borrow_mut().push((i_db, table.to_ascii_lowercase())));
    }

    fn rowest_clamped(&self, i_db: usize) -> bool {
        self.db_set
            .slots()
            .get(i_db)
            .is_some_and(|s| s.c_rowest_clamped)
    }

    fn tbl_hash_log(&self, i_db: usize) -> Option<(Vec<String>, Vec<(bool, String)>)> {
        self.db_set
            .slots()
            .get(i_db)
            .and_then(|s| s.c_tbl_hash.clone())
    }

    fn created_since_load(&self, i_db: usize, name: &str) -> bool {
        self.db_set.slots().get(i_db).is_some_and(|s| s.c_incremental.contains(name))
    }

    fn vtab_module(&self, name: &str, lazy: bool) -> Option<VtabModule> {
        let reg = self.db_set.vtab_registry();
        let (module, reg_name) = match reg.get_module(name) {
            Some(m) => (m, reg.registered_module_name(name).unwrap_or(name).to_owned()),
            // sqlite3PragmaVtabRegister(db, zName): registered under the name of this use
            None if lazy => {
                let m = crate::vtab::PragmaVTabModule::try_for_module_name(name)?;
                let arc: Arc<dyn sqliters_types::VirtualTableModule> = Arc::new(m);
                (arc, name.to_owned())
            }
            None => return None,
        };
        Some(VtabModule {
            name: reg_name,
            eponymous: module.is_eponymous(),
            shadow_names: module.shadow_table_suffixes().iter().map(|s| (*s).to_owned()).collect(),
        })
    }

    fn vtab_connect(&self, i_db: usize, table: &str, _module: &str, _args: &[String], eponymous: bool) -> VtabConnect {
        let Some(slot) = self.db_set.slots().get(i_db) else { return VtabConnect::Unsupported };
        let reg = self.db_set.vtab_registry();
        // (the engine connects the virtual tables a statement names before the generator runs,
        // see `prepare_in_session()`; a table it could not connect stays unsupported. An
        // eponymous table may be connected under the schema the statement named.)
        let entry = reg.get_connected(&slot.name, table).or_else(|| if eponymous { reg.get_connected_eponymous(table) } else { None });
        let Some(entry) = entry else { return VtabConnect::Unsupported };
        let crate::vtab::VTabInstanceState::Available(vt) = &entry.state else { return VtabConnect::Unsupported };
        let _ = vt;
        // An address names the instance (C lists `vtab:%p`): the one of the registry entry (a
        // module's table object can have size zero)
        let id = (entry as *const crate::vtab::ConnectedVTabEntry) as usize as u64;
        let mut ids = self.vtabs.borrow_mut();
        if !ids.iter().any(|(i, _, _)| *i == id) {
            ids.push((id, entry.db_name.clone(), entry.table_name.clone()));
        }
        let d = &entry.declared;
        VtabConnect::Ok {
            vtab: VTable {
                id,
                module_name: entry.module_name.clone(),
                b_constraint: d.constraint_support,
                b_all_schemas: false,
                // (the engine's module-level risk flags stand in for sqlite3_vtab_config() calls
                // a module would make in xConnect: the same flags the engine's own checks use)
                e_vtab_risk: match entry.effective_risk_flags() {
                    VTabRiskFlags::Innocuous => sqliters_codegen::sqliteint::SQLITE_VTABRISK_Low,
                    VTabRiskFlags::DirectOnly => sqliters_codegen::sqliteint::SQLITE_VTABRISK_High,
                    _ => sqliters_codegen::sqliteint::SQLITE_VTABRISK_Normal,
                },
                has_find_function: d.find_function,
                has_update: !entry.is_read_only && !entry.is_eponymous_only,
                // xRename of the modules C's reference build has (fts5, rtree, geopoly have
                // one; generate_series, json_each/json_tree, dbstat, the pragma tables do not)
                has_rename: match entry.module_name.to_ascii_lowercase().as_str() {
                    "fts5" | "rtree" | "rtree_i32" | "geopoly" => Some(true),
                    "generate_series" | "json_each" | "json_tree" | "jsonb_each" | "jsonb_tree"
                    | "dbstat" | "fts5vocab" | "sqlite_dbpage" | "sqlite_stmt" | "carray"
                    | "fts5_vocab" => Some(false),
                    m if m.starts_with("pragma_") => Some(false),
                    _ => None,
                },
            },
            declare: Some(d.declare_sql.clone()).filter(|s| !s.is_empty()),
        }
    }

    fn vtab_best_index(&self, vtab_id: u64, info: &mut IndexInfo, hidden: &mut dyn IndexInfoHidden) -> (i32, Option<String>) {
        let found = self.vtabs.borrow().iter().find(|(i, _, _)| *i == vtab_id).map(|(_, d, t)| (d.clone(), t.clone()));
        let Some((db, tab)) = found else { return (1, None) };
        let reg = self.db_set.vtab_registry();
        let Some(entry) = reg.get_connected(&db, &tab) else { return (1, None) };
        let crate::vtab::VTabInstanceState::Available(vt) = &entry.state else { return (1, None) };
        let mut v = VTabIndexInfo::new(
            info.a_constraint
                .iter()
                .enumerate()
                .map(|(i, c)| VTabConstraint {
                    i_column: c.i_column,
                    op: vtab_op_from_c(c.op),
                    usable: c.usable,
                    in_values: hidden.vtab_in(i as i32, -1),
                })
                .collect(),
            info.a_order_by.iter().map(|o| VTabOrderBy { i_column: o.i_column, desc: o.desc }).collect(),
        );
        // the inputs C's whereLoopAddVirtualOne() initialized
        v.idx_num = info.idx_num;
        v.idx_str = info.idx_str.clone();
        v.order_by_consumed = info.order_by_consumed;
        v.estimated_cost = info.estimated_cost;
        v.estimated_rows = info.estimated_rows;
        v.idx_flags = info.idx_flags as u32;
        v.col_used = info.col_used;
        let r = {
            let mut calls = CallsAdapter { h: hidden };
            vt.best_index_calls(&mut v, &mut calls)
        };
        for (u, cu) in info.a_constraint_usage.iter_mut().zip(v.constraint_usage.iter()) {
            u.argv_index = cu.argv_index as i32;
            u.omit = cu.omit;
        }
        info.idx_num = v.idx_num;
        info.need_to_free_idx_str = v.need_to_free_idx_str;
        info.idx_str = v.idx_str;
        info.order_by_consumed = v.order_by_consumed;
        info.estimated_cost = v.estimated_cost;
        info.estimated_rows = v.estimated_rows;
        info.idx_flags = v.idx_flags as i32;
        match r {
            Ok(()) => (0, None),
            Err(e) if e.code == ErrorCode::Constraint => (19, None),
            Err(e) => (e.code as i32, Some(e.message)),
        }
    }

    fn vtab_find_function(&self, vtab_id: u64, _n_arg: i32, name: &str) -> i32 {
        let found = self.vtabs.borrow().iter().find(|(i, _, _)| *i == vtab_id).map(|(_, d, t)| (d.clone(), t.clone()));
        let Some((db, tab)) = found else { return 0 };
        let reg = self.db_set.vtab_registry();
        let Some(entry) = reg.get_connected(&db, &tab) else { return 0 };
        // fts5FindFunctionMethod(): the auxiliary functions of FTS5 (fts5FindAuxiliary())
        if entry.declared.find_function
            && ["snippet", "highlight", "bm25", "fts5_get_locale"].iter().any(|f| f.eq_ignore_ascii_case(name))
        {
            1
        } else {
            0
        }
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

thread_local! {
    /// Set when a prepare of the generator found that C's `sqlite3OpenTempDatabase()` opened the
    /// temp database (see [`take_c_temp_opened`]).
    static C_TEMP_OPENED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// True (once) if a generator prepare since the last call opened C's temp database: the caller
/// opens the engine's temp database too (C's `db->aDb[1].pBt` is set from then on).
pub(crate) fn take_c_temp_opened() -> bool {
    C_TEMP_OPENED.with(|c| c.replace(false))
}

thread_local! {
    /// Tables whose row estimate a generator prepare found C's CREATE INDEX raising (see
    /// [`take_c_rowest_clamped`]).
    static C_ROWEST: std::cell::RefCell<Vec<(usize, String)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The (database, table) pairs whose `nRowLogEst` C's `sqlite3CreateIndex()` raised during the
/// generator prepares since the last call: the caller records them (`DbSlot::c_rowest_tables`).
pub(crate) fn take_c_rowest_clamped() -> Vec<(usize, String)> {
    C_ROWEST.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

thread_local! {
    /// [`c_prepare_quiet`] is running: no verification dumps.
    static C_QUIET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`c_prepare`] for a side effect of C's prepare only (the program is not used): the
/// verification dumps of the planner are not written.
pub(crate) fn c_prepare_quiet(inner: &ConnectionInner, sql: &str) -> (PrepareResult, usize, bool) {
    C_QUIET.with(|c| c.set(true));
    let r = c_prepare(inner, sql);
    C_QUIET.with(|c| c.set(false));
    r
}

/// Run C's parser and code generator over one statement (`sqlite3Prepare`).
/// The third value: C's `sqlite3Init()` loaded the schemas during this prepare.
pub(crate) fn c_prepare(inner: &ConnectionInner, sql: &str) -> (PrepareResult, usize, bool) {
    let host = EngineHost {
        functions: &inner.functions,
        collations: &inner.collations,
        db_set: &inner.db_set,
        schema_loaded: std::cell::Cell::new(false),
        vtabs: std::cell::RefCell::new(Vec::new()),
    };
    if schema_dump_on() && sql.contains("/*#schemadump*/") {
        // Verification aid: the schema objects as the generator loads them, on stderr.
        for l in sqliters_codegen::schema_dump(make_db(inner, &host)) {
            eprintln!("sd: {l}");
        }
    }
    let db = make_db(inner, &host);
    if where_dump_on() && !C_QUIET.with(std::cell::Cell::get) {
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
    // The connection flags of DML code generation (PRAGMA foreign_keys, recursive_triggers,
    // count_changes, writable_schema) and the authorizer (`db->xAuth`).
    {
        use sqliters_codegen::consts::{SQLITE_CountRows, SQLITE_ForeignKeys, SQLITE_RecTriggers, SQLITE_WriteSchema};
        if inner.foreign_keys {
            db.flags |= SQLITE_ForeignKeys;
        }
        if inner.recursive_triggers {
            db.flags |= SQLITE_RecTriggers;
        }
        if inner.count_changes {
            db.flags |= SQLITE_CountRows;
        }
        if inner.writable_schema {
            db.flags |= SQLITE_WriteSchema;
        }
    }
    // sqlite3_db_config(SQLITE_DBCONFIG_ENABLE_VIEW / _ENABLE_TRIGGER / _DEFENSIVE)
    {
        use sqliters_codegen::consts::{SQLITE_Defensive, SQLITE_EnableTrigger, SQLITE_EnableView};
        if inner.db_set.enable_view() {
            db.flags |= SQLITE_EnableView;
        } else {
            db.flags &= !SQLITE_EnableView;
        }
        if inner.db_set.enable_trigger() {
            db.flags |= SQLITE_EnableTrigger;
        } else {
            db.flags &= !SQLITE_EnableTrigger;
        }
        if inner.db_set.defensive() {
            db.flags |= SQLITE_Defensive;
        } else {
            db.flags &= !SQLITE_Defensive;
        }
    }
    // sqlite3_limit(): the limits the engine keeps per connection
    {
        use sqliters_codegen::consts::{
            SQLITE_LIMIT_COLUMN, SQLITE_LIMIT_EXPR_DEPTH, SQLITE_LIMIT_TRIGGER_DEPTH, SQLITE_LIMIT_TRIGGER_STEPS,
        };
        let clamp = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        db.a_limit[SQLITE_LIMIT_COLUMN as usize] = clamp(inner.db_set.column_limit());
        db.a_limit[SQLITE_LIMIT_EXPR_DEPTH as usize] = clamp(inner.db_set.expr_depth_limit());
        db.a_limit[SQLITE_LIMIT_TRIGGER_DEPTH as usize] = clamp(inner.db_set.trigger_depth_limit());
        db.a_limit[SQLITE_LIMIT_TRIGGER_STEPS as usize] = clamp(inner.db_set.trigger_steps_limit());
    }
    db.x_auth = inner.authorizer.is_some();
    db.n_analysis_limit = inner.db_set.analysis_limit();
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
