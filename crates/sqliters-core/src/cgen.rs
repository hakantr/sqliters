//! Adapter between the engine and the port of C's code generator (`sqliters-codegen`).
//!
//! Step V1 uses the code generator for one thing only: `EXPLAIN <stmt>` prints C's listing
//! when the generator supports the whole statement. Statements are still executed by the
//! engine's executor. This module is the single place where connection state (flags, text
//! encoding, attached databases, application functions and collations) is handed to the
//! generator; later steps add the schema objects (C's `Table` / `Column` / `Index`) here.

use std::sync::Arc;

use sqliters_codegen::builtin_funcs::{BUILTIN_FUNCS, BuiltinFunc, CONNECTION_FUNCS};
use sqliters_codegen::build_tab::SchemaRow;
use sqliters_codegen::consts::{SQLITE_DqsDML, SQLITE_TrustedSchema};
use sqliters_codegen::pragma::FunctionListAuthorization;
use sqliters_codegen::sqliteint::{PartialIndexBinding, VTable, VtabConnect, VtabModule};
use sqliters_codegen::vtab::{IndexInfo, IndexInfoHidden};
use sqliters_codegen::{Db, DbInfo, Host, PrepareResult};
use sqliters_types::{
    ErrorCode, FunctionFlags, SqliteError, TextEncoding, VTabConstraint, VTabConstraintOp, VTabIndexInfo, VTabIndexInfoCalls, VTabOrderBy,
    VTabRiskFlags, Value,
};
use sqliters_vdbe::vdbeaux::{FuncDef, Program};

use crate::ConnectionInner;
use crate::execution_context::{CollationRegistry, FunctionRegistry};

/// `SQLITE_FUNC_UNSAFE` (the same bit as `SQLITE_INNOCUOUS`, with the meaning inverted)
const SQLITE_FUNC_UNSAFE: u32 = 0x0020_0000;

#[derive(Clone, Copy)]
enum PrepareBindings<'a> {
    Unbound,
    Read(&'a [Value]),
    PartialIndex { root: u32, binding: PartialIndexBinding<'a> },
}

struct EngineHost<'a> {
    bindings: PrepareBindings<'a>,
    function_list_authorization: Option<FunctionListAuthorization>,
    functions: &'a FunctionRegistry,
    collations: &'a CollationRegistry,
    db_set: &'a crate::db_set::DatabaseSet,
    busy_timeout: Option<i64>,
    wal_autocheckpoint: i32,
    default_locking_mode: i64,
    /// set by [`Host::note_schema_loaded`]
    schema_loaded: std::cell::Cell<bool>,
    /// Inspect preparation without invoking application virtual-table callbacks.
    suppress_vtab_callbacks: bool,
    deferred_vtab_connect: Option<&'a std::cell::Cell<bool>>,
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
pub(crate) fn func_flags(flags: FunctionFlags) -> u32 {
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

/// The public function registration APIs supply SQLITE_UTF8 to sqlite3CreateFunc.
pub(crate) fn registration_replaces_definition(
    functions: &FunctionRegistry,
    name: &str,
    arity: Option<usize>,
) -> bool {
    if functions.scalar_mask_matches(name, arity) {
        return false;
    }
    let scalar = arity.map_or_else(
        || functions.has_scalar_overload(name, None).then_some(None),
        |argc| functions.find_scalar(name, argc).map(|definition| definition.arity),
    );
    let aggregates = functions.aggregate_overloads(name);
    let n_arg = arity.map_or(-1, |argc| argc as i16);
    let exact_utf8 = |&(candidate, registered_arity, flags, _, _, _): &BuiltinFunc| {
        candidate.eq_ignore_ascii_case(name) && registered_arity == n_arg
            && flags & sqliters_codegen::consts::SQLITE_FUNC_ENCMASK == TextEncoding::Utf8 as u32
    };
    if scalar == Some(arity)
        || aggregates.iter().any(|&(registered_arity, _, _)| registered_arity == arity)
        || CONNECTION_FUNCS.iter().any(exact_utf8)
    {
        return true;
    }
    // A compatible connection-local overload takes precedence even if a
    // built-in with the requested arity would otherwise be a better match.
    if scalar.is_some()
        || aggregates.iter().any(|&(registered_arity, _, _)| registered_arity.is_none())
        || CONNECTION_FUNCS.iter().any(|&(candidate, registered_arity, _, _, _, _)| {
            candidate.eq_ignore_ascii_case(name) && registered_arity == -1
        })
    {
        return false;
    }
    BUILTIN_FUNCS.iter().any(exact_utf8)
}

impl Host for EngineHost<'_> {
    fn prepare_interrupt_pending(&self) -> bool {
        self.deferred_vtab_connect.is_some()
    }

    fn bound_value(&self, i_var: usize) -> Option<Value> {
        let PrepareBindings::Read(values) = self.bindings else { return None };
        values.get(i_var.checked_sub(1)?)
            .filter(|v| !matches!(v, Value::Null)).cloned()
    }

    fn partial_index_predicate_binding(&self, root_pgno: u32) -> Option<PartialIndexBinding<'_>> {
        match self.bindings {
            PrepareBindings::PartialIndex { root, binding } if root == root_pgno => Some(binding),
            _ => None,
        }
    }

    fn pragma_collations(&self) -> Option<Vec<String>> {
        Some(self.collations.all_collation_names())
    }
    fn pragma_module_names(&self) -> Option<Vec<String>> {
        Some(self.db_set.vtab_registry().module_names_in_hash_order())
    }
    fn pragma_function_list_authorization(&self) -> Option<FunctionListAuthorization> {
        self.function_list_authorization
    }
    fn pragma_function_list(&self) -> Option<Vec<Vec<Value>>> {
        Some(crate::engine::pragma_function_list(self.functions).rows)
    }
    fn pragma_mmap_size(&self, i_db: usize, request: Option<i64>) -> Option<sqliters_types::Result<Option<i64>>> {
        let slot = self.db_set.slots().get(i_db)?;
        if i_db == 1 && slot.btree.is_none() { return Some(Ok(None)); }
        Some(slot.btree.as_ref()?.pager().predict_mmap_size(request))
    }
    fn pragma_text(&self, i_db: usize, name: &str) -> Option<String> {
        if name != "filename" { return None; }
        let slot = self.db_set.slots().get(i_db)?;
        // The generator can predict opening an unnamed TEMP Btree before the
        // real prepare effects are applied to the connection.
        if i_db == 1 && slot.btree.is_none() { return Some(String::new()); }
        match slot.btree.as_ref()?.db_path() {
            Some(path) => path.to_str().map(str::to_owned),
            None => Some(String::new()),
        }
    }
    fn note_pragma_effect(&self, effect: sqliters_codegen::pragma::PrepareEffect) {
        C_PRAGMA_EFFECTS.with(|effects| effects.borrow_mut().push(effect));
    }
    fn pragma_int(&self, i_db: usize, name: &str) -> Option<i64> {
        if name == "temp_store" { return Some(self.db_set.temp_store().as_int()); }
        if name == "temp_transaction" {
            return Some(i64::from(self.db_set.slots().get(1)?.btree.as_ref()
                .is_some_and(|bt| bt.transaction_state() != sqliters_pager::TransactionState::Open)));
        }
        if name == "default_locking_mode" { return Some(self.default_locking_mode); }
        if name == "busy_timeout" { return self.busy_timeout; }
        if name == "wal_autocheckpoint" { return Some(i64::from(self.wal_autocheckpoint)); }
        if i_db == 1 && name == "cache_size" {
            return Some(i64::from(self.db_set.temp_schema_cache_size()));
        }
        let slot = self.db_set.slots().get(i_db)?;
        if i_db == 1 && slot.btree.is_none() {
            if name == "locking_mode" { return Some(1); }
            if name == "locking_mode_settable" { return Some(0); }
        }
        if i_db == 1 && slot.btree.is_none() && name == "cache_spill" {
            let size = u32::try_from(self.db_set.next_page_size).ok()
                .filter(|n| sqliters_pager::DatabaseHeader::is_valid_page_size(*n))
                .unwrap_or(self.db_set.attach_page_size());
            return Some(i64::from(sqliters_pager::Pager::initial_spill_size(size)));
        }
        // A read-only generator inspection predicts sqlite3OpenTempDatabase without
        // opening the real TEMP Btree. Its new BtShared has no fixed size or auto-vacuum
        // flags (btree.c:2729–2755), before this PRAGMA's prepare effects are applied.
        if i_db == 1 && slot.btree.is_none()
            && matches!(name, "auto_vacuum" | "auto_vacuum_fixed")
        {
            return Some(0);
        }
        let bt = slot.btree.as_ref()?;
        match name {
            "page_size" => Some(i64::from(bt.pager().page_size())),
            "locking_mode" => Some(i64::from(bt.pager().locking_mode() == sqliters_pager::LockingMode::Exclusive)),
            "locking_mode_settable" => Some(i64::from(bt.pager().can_set_locking_mode())),
            "auto_vacuum" => Some(bt.auto_vacuum_mode() as i64),
            "auto_vacuum_fixed" => Some(i64::from(bt.pager().is_page_size_fixed())),
            "cache_size" => Some(i64::from(bt.schema_cache_size())),
            "cache_spill" => Some(i64::from(bt.pager().spill_size())),
            "journal_size_limit" => Some(bt.pager().journal_size_limit()),
            "secure_delete" => Some(i64::from(bt.secure_delete_setting())),
            "synchronous" => Some(if i_db == 1 { 0 } else { i64::from(bt.pager().synchronous()) }),
            _ => None,
        }
    }
    fn note_schema_loaded(&self) {
        self.schema_loaded.set(true);
    }

    fn note_temp_opened(&self) {
        C_TEMP_OPENED.with(|c| c.set(true));
    }

    fn deleted_user_arities(&self, name: &str) -> Vec<i16> {
        self.functions.deleted_scalar_arities(name).iter()
            .map(|arity| arity.map_or(-1, |argc| argc as i16))
            .collect()
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
                func_flags: self.functions.c_like_flags(name, arity).unwrap_or_else(|| func_flags(flags)),
                name: name.to_ascii_lowercase(),
                user_data: -1,
                x_finalize: false,
                x_value: false,
            }));
        }
        for (arity, flags, supports_window) in self.functions.aggregate_overloads(name) {
            out.push(Arc::new(FuncDef {
                n_arg: arity.map_or(-1, |n| n as i16),
                func_flags: self.functions.c_like_flags(name, arity).unwrap_or_else(|| func_flags(flags)),
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
        if self.suppress_vtab_callbacks && self.deferred_vtab_connect.is_none() { return None; }
        let reg = self.db_set.vtab_registry();
        let (module, reg_name) = match reg.get_module(name) {
            Some(m) => (m, reg.registered_module_name(name).unwrap_or(name).to_owned()),
            None if lazy => {
                let (reg_name, m) = crate::vtab::VTabRegistry::lazy_module(name)?;
                (m, reg_name)
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
        if let Some(deferred) = self.deferred_vtab_connect {
            deferred.set(true);
        }
        if self.suppress_vtab_callbacks { return VtabConnect::Unsupported; }
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
    static C_PRAGMA_EFFECTS: std::cell::RefCell<Vec<sqliters_codegen::pragma::PrepareEffect>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Set when a prepare of the generator found that C's `sqlite3OpenTempDatabase()` opened the
    /// temp database (see [`take_c_temp_opened`]).
    static C_TEMP_OPENED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn take_c_pragma_effects() -> Vec<sqliters_codegen::pragma::PrepareEffect> {
    C_PRAGMA_EFFECTS.with(|effects| std::mem::take(&mut *effects.borrow_mut()))
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
    c_prepare_impl(inner, sql, false, PrepareBindings::Unbound, None, false, None, &inner.functions, None)
}

/// Compile only after the engine's direct PRAGMA authorization returned its decision.
pub(crate) fn c_prepare_authorized_function_list(
    inner: &ConnectionInner, sql: &str, authorization: FunctionListAuthorization,
) -> PrepareResult {
    let quiet = C_QUIET.with(|c| c.replace(true));
    let result = c_prepare_impl(
        inner, sql, false, PrepareBindings::Unbound, None, false, None, &inner.functions,
        Some(authorization),
    ).0;
    C_QUIET.with(|c| c.set(quiet));
    result
}

/// Recompile with the old statement's parameter values (pParse->pReprepare).
pub(crate) fn c_prepare_bound(inner: &ConnectionInner, sql: &str, values: &[Value]) -> (PrepareResult, usize, bool) {
    c_prepare_impl(inner, sql, false, PrepareBindings::Read(values), None, false, None, &inner.functions, None)
}

pub(crate) fn c_prepare_bound_partial_index(
    inner: &ConnectionInner,
    sql: &str,
    root: u32,
    binding: PartialIndexBinding<'_>,
) -> (PrepareResult, usize, bool) {
    c_prepare_impl(inner, sql, false, PrepareBindings::PartialIndex { root, binding }, None, false, None, &inner.functions, None)
}

pub(crate) fn c_prepare_table_scan(
    inner: &ConnectionInner, sql: &str, values: Option<&[Value]>, root: u32,
) -> PrepareResult {
    let bindings = values.map_or(PrepareBindings::Unbound, PrepareBindings::Read);
    c_prepare_impl(inner, sql, false, bindings, None, false, Some(root), &inner.functions, None).0
}

pub(crate) fn c_explain_bound(inner: &ConnectionInner, sql: &str, values: &[Value]) -> CExplain {
    match c_prepare_bound(inner, sql, values).0 {
        PrepareResult::Program(p) => CExplain::Program(Arc::new(*p)),
        PrepareResult::Error { code, message, offset } => CExplain::Error { code, message, offset },
        PrepareResult::Unsupported(_) => CExplain::Unsupported,
    }
}

/// SELECT decisions made by C's compiler with the schema cache assumed unloaded.
/// A false `reads_schema` proves this prepare does not need a schema load even
/// after RESET. A returned program also describes execution's Btree use.
pub(crate) struct SelectSchemaUse {
    pub reads_schema: bool,
    pub btree_mask: Option<u32>,
    pub is_reader: Option<bool>,
    pub has_like: bool,
    pub has_text_register_read: bool,
    pub sorter_owns_rows: bool,
}

pub(crate) fn program_has_like(p: &sqliters_vdbe::vdbeaux::Program) -> bool {
    p.references_like || p.a_op.iter().any(|op| match &op.p4 {
        sqliters_vdbe::vdbeaux::P4::FuncDef(f) => f.name.eq_ignore_ascii_case("like"),
        sqliters_vdbe::vdbeaux::P4::FuncCtx(c) => c.p_func.name.eq_ignore_ascii_case("like"),
        _ => false,
    })
}

/// A scalar program that can retain the bytes of SQLite text registers.
#[allow(non_upper_case_globals)]
pub(crate) fn text_register_read(program: &sqliters_vdbe::vdbeaux::Program) -> bool {
    use sqliters_codegen::consts::SQLITE_FUNC_BUILTIN;
    use sqliters_vdbe::{opcodes::*, vdbeaux::P4};

    if program.btree_mask != 0 || program.explain != 0 || !program.read_only {
        return false;
    }
    let mut has_text_producer = false;
    for op in &program.a_op {
        match op.opcode {
            OP_Function | OP_PureFunc => {
                let P4::FuncCtx(ctx) = &op.p4 else { return false; };
                let func = &ctx.p_func;
                if func.func_flags & SQLITE_FUNC_BUILTIN == 0 {
                    return false;
                }
                if func.name.eq_ignore_ascii_case("unistr") || func.name.eq_ignore_ascii_case("char") {
                    has_text_producer = true;
                } else if !["hex", "length", "octet_length", "unicode", "quote", "unistr_quote"].iter()
                    .any(|name| func.name.eq_ignore_ascii_case(name))
                {
                    return false;
                }
            }
            OP_Init | OP_Goto | OP_Halt | OP_Once | OP_ResultRow | OP_String8 | OP_String
            | OP_Blob | OP_Integer | OP_Int64 | OP_Real | OP_Null | OP_Variable | OP_Copy
            | OP_SCopy | OP_Move | OP_Concat => {}
            _ => return false,
        }
    }
    has_text_producer
}

pub(crate) fn select_schema_use(inner: &ConnectionInner, sql: &str) -> Option<SelectSchemaUse> {
    let (result, reads_schema) = select_schema_probe(inner, sql);
    match result {
        PrepareResult::Program(p) => Some(SelectSchemaUse {
            reads_schema,
            btree_mask: Some(p.btree_mask),
            is_reader: Some(p.b_is_reader),
            has_like: program_has_like(&p),
            has_text_register_read: text_register_read(&p),
            sorter_owns_rows: p.group_sorter_owns_rows && p.explain == 0
                && !inner.authorizer.is_some()
                && inner.collations.get_custom("BINARY").is_none()
                && !inner.functions.has_aggregate_name("count")
                && !inner.functions.has_scalar_name("count"),
        }),
        PrepareResult::Error { .. } => Some(SelectSchemaUse {
            reads_schema,
            btree_mask: None,
            is_reader: None,
            has_like: false,
            has_text_register_read: false,
            sorter_owns_rows: false,
        }),
        PrepareResult::Unsupported(_) => None,
    }
}

pub(crate) fn select_schema_probe(inner: &ConnectionInner, sql: &str) -> (PrepareResult, bool) {
    select_schema_probe_with_functions(inner, sql, &inner.functions)
}

pub(crate) fn select_schema_probe_with_functions(
    inner: &ConnectionInner, sql: &str, functions: &FunctionRegistry,
) -> (PrepareResult, bool) {
    c_prepare_probe(inner, sql, true, None, functions)
}

/// Resolve the parser's interrupt check. A true result defers to one real xConnect.
pub(crate) fn c_prepare_while_interrupted(inner: &ConnectionInner, sql: &str) -> sqliters_types::Result<bool> {
    let deferred_vtab_connect = std::cell::Cell::new(false);
    let result = c_prepare_probe(inner, sql, false, Some(&deferred_vtab_connect), &inner.functions).0;
    if deferred_vtab_connect.get() {
        return Ok(true);
    }
    match result {
        PrepareResult::Program(_) => Ok(false),
        PrepareResult::Error { code, message, offset } => Err(
            SqliteError::with_extended(code, message)
                .with_offset(usize::try_from(offset).unwrap_or(SqliteError::NO_OFFSET)),
        ),
        PrepareResult::Unsupported(_) => Err(
            SqliteError::new(ErrorCode::Interrupt, "interrupted")
                .with_offset(usize::try_from(inner.c_error_byte_offset).unwrap_or(SqliteError::NO_OFFSET)),
        ),
    }
}

/// Probe the current schema without running optimizer callbacks or publishing prepare effects.
pub(crate) fn c_prepare_diagnostic(inner: &ConnectionInner, sql: &str) -> PrepareResult {
    c_prepare_probe(inner, sql, false, None, &inner.functions).0
}

fn c_prepare_probe(
    inner: &ConnectionInner,
    sql: &str,
    schema_probe: bool,
    deferred_vtab_connect: Option<&std::cell::Cell<bool>>,
    functions: &FunctionRegistry,
) -> (PrepareResult, bool) {
    let effects = take_c_pragma_effects();
    let temp = take_c_temp_opened();
    let rowest = take_c_rowest_clamped();
    let quiet = C_QUIET.with(|c| c.replace(true));
    let (result, _, reads_schema) = c_prepare_impl(
        inner, sql, schema_probe, PrepareBindings::Unbound, deferred_vtab_connect, true, None, functions, None,
    );
    C_QUIET.with(|c| c.set(quiet));
    C_PRAGMA_EFFECTS.with(|c| *c.borrow_mut() = effects);
    C_TEMP_OPENED.with(|c| c.set(temp));
    C_ROWEST.with(|c| *c.borrow_mut() = rowest);
    (result, reads_schema)
}

fn c_prepare_impl(
    inner: &ConnectionInner,
    sql: &str,
    schema_probe: bool,
    bindings: PrepareBindings<'_>,
    deferred_vtab_connect: Option<&std::cell::Cell<bool>>,
    suppress_vtab_callbacks: bool,
    table_scan_root: Option<u32>,
    functions: &FunctionRegistry,
    function_list_authorization: Option<FunctionListAuthorization>,
) -> (PrepareResult, usize, bool) {
    let interrupted = deferred_vtab_connect.is_some();
    let host = EngineHost {
        bindings,
        function_list_authorization,
        functions,
        collations: &inner.collations,
        db_set: &inner.db_set,
        busy_timeout: match inner.busy_policy { crate::engine::BusyPolicy::TimeoutMs(ms) => i32::try_from(ms).ok().map(i64::from), _ => Some(0) },
        wal_autocheckpoint: inner.wal_autocheckpoint_threshold,
        default_locking_mode: i64::from(inner.default_locking_mode == sqliters_pager::LockingMode::Exclusive),
        schema_loaded: std::cell::Cell::new(false),
        suppress_vtab_callbacks,
        deferred_vtab_connect,
        vtabs: std::cell::RefCell::new(Vec::new()),
    };
    if schema_dump_on() && !C_QUIET.with(std::cell::Cell::get) && sql.contains("/*#schemadump*/") {
        // Verification aid: the schema objects as the generator loads them, on stderr.
        for l in sqliters_codegen::schema_dump(make_db(inner, &host)) {
            eprintln!("sd: {l}");
        }
    }
    let mut db = make_db(inner, &host);
    db.table_scan_root = table_scan_root;
    if interrupted {
        db.schema_not_loaded = inner.db_set.c_any_schema_unloaded();
    }
    if schema_probe {
        // Counterfactual cache state only; EncodingFixed remains the real
        // connection's bit (parse.y:523). No connection metadata is changed.
        db.schema_not_loaded = true;
        db.m_db_flags &= !(sqliters_codegen::consts::DBFLAG_SchemaKnownOk as u32);
    }
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
    db.err_byte_offset = inner.c_error_byte_offset;
    db.auto_commit = !(inner.explicit_tx || inner.implicit_savepoint_tx);
    db.enc = match inner.effective_encoding() {
        TextEncoding::Utf8 => 1,
        TextEncoding::Utf16Le => 2,
        TextEncoding::Utf16Be => 3,
    };
    if !inner.db_set.dqs_dml() {
        db.flags &= !SQLITE_DqsDML;
    }
    // The flag pragmas (automatic_index, reverse_unordered_selects, short_column_names,
    // full_column_names): the bits that differ from C's default.
    db.flags ^= inner.db_set.c_flags_toggled();
    // This flag also has a db_config API: the engine holds its absolute value.
    if inner.db_set.trusted_schema() {
        db.flags |= SQLITE_TrustedSchema;
    } else {
        db.flags &= !SQLITE_TrustedSchema;
    }
    // The connection flags of DML code generation (PRAGMA foreign_keys, recursive_triggers,
    // count_changes, writable_schema) and the authorizer (`db->xAuth`).
    {
        use sqliters_codegen::consts::{SQLITE_CountRows, SQLITE_ForeignKeys, SQLITE_RecTriggers, SQLITE_WriteSchema, SQLITE_NoSchemaError};
        // These bits also have connection APIs. Their absolute values override the
        // pragma mirror, including when db_config clears a previously enabled flag.
        db.flags &= !(SQLITE_CountRows | SQLITE_ForeignKeys | SQLITE_RecTriggers | SQLITE_WriteSchema | SQLITE_NoSchemaError);
        if inner.foreign_keys {
            db.flags |= SQLITE_ForeignKeys;
        }
        if inner.recursive_triggers {
            db.flags |= SQLITE_RecTriggers;
        }
        if inner.count_changes {
            db.flags |= SQLITE_CountRows;
        }
        if inner.db_set.writable_schema_flag() {
            // main.c:1014: DBCONFIG_WRITABLE_SCHEMA controls the same two bits as
            // PRAGMA writable_schema. They must also be cleared together.
            db.flags |= SQLITE_WriteSchema | SQLITE_NoSchemaError;
        }
    }
    if inner.legacy_alter_table { db.flags |= sqliters_codegen::consts::SQLITE_LegacyAlter; }
    if inner.db_set.cell_size_check() { db.flags |= sqliters_codegen::consts::SQLITE_CellSizeCk; }
    if !inner.db_set.cache_spill { db.flags &= !sqliters_codegen::consts::SQLITE_CacheSpill; }
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
    db.a_limit[sqliters_codegen::consts::SQLITE_LIMIT_WORKER_THREADS as usize] = inner.db_set.worker_threads_limit;
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

/// Keep the regular codegen path when a prepare diagnostic dump was requested.
pub(crate) fn prepare_dump_requested(sql: &str) -> bool {
    where_dump_on() || (schema_dump_on() && sql.contains("/*#schemadump*/"))
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
    /// The statement does not compile in C: Parse.rc, error message and byte offset.
    Error { code: i32, message: String, offset: i32 },
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
        PrepareResult::Error { code, message, offset } => CExplain::Error { code, message, offset },
        PrepareResult::Unsupported(_) => CExplain::Unsupported,
    };
    (r, tail, loaded)
}

/// CREATE [TEMP] TABLE can load the schema in the name reduction, before the
/// tokenizer yields a later syntax error in the column list.
pub(crate) fn starts_create_table(sql: &str) -> bool {
    use sqliters_codegen::tk::{TK_COMMENT, TK_CREATE, TK_SEMI, TK_SPACE, TK_TABLE, TK_TEMP};
    use sqliters_codegen::tokenize::sqlite3_get_token;

    let mut rest = sql.as_bytes();
    let mut tokens = std::iter::from_fn(|| {
        let (n, token) = sqlite3_get_token(rest);
        if n == 0 {
            return None;
        }
        rest = &rest[n..];
        Some(token)
    })
    .filter(|token| !matches!(*token, TK_COMMENT | TK_SPACE))
    .skip_while(|token| *token == TK_SEMI);
    if tokens.next() != Some(TK_CREATE) {
        return false;
    }
    match tokens.next() {
        Some(TK_TABLE) => true,
        Some(TK_TEMP) => tokens.next() == Some(TK_TABLE),
        _ => false,
    }
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
