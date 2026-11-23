//! The connection as the port of C's virtual machine sees it (`sqliters_vdbe::vdbe`), and the
//! diagnostic switch that routes statements through it.
//!
//! Step M1 of the port of C's code generator and VDBE: the machine executes the programs of
//! `sqliters-codegen` for read statements. The engine's executor and fast paths remain the
//! execution path; only when the environment variable `SQLITERS_VDBE_EXEC` is set (read once
//! per process) does a statement run through generator + machine — and then only when the
//! generator supports the whole statement and every opcode of its program is ported.
//! `SQLITERS_VDBE_TRACE` (with the switch on) prints `vm: machine :: <sql>` or
//! `vm: fallback: <reason> :: <sql>` on stderr for every prepared statement. Both variables are
//! temporary port aids and go away at the switch-over.

use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_codegen::PrepareResult;
use sqliters_codegen::sqliteint::PartialIndexBinding;
use sqliters_pager::{JournalMode, LockingMode, PagerOpenOptions, TransactionState};
use sqliters_types::DatabaseKind;
use sqliters_syntax::Stmt;
use sqliters_types::{
    AggregateStepId, DatabaseId, ErrorCode, ExecutionServices, Result, RuntimeValue, SqliteError, TextEncoding, Value,
};
use sqliters_vdbe::functions::VmRuntimeContext;
use sqliters_vdbe::vdbe::{
    COLTYPE_ANY, COLTYPE_BLOB, COLTYPE_CUSTOM, COLTYPE_INT, COLTYPE_INTEGER, COLTYPE_REAL, COLTYPE_TEXT, TableColRt, TableRt, Vdbe,
    VdbeHost, unsupported_reason,
};
use sqliters_vdbe::vdbeaux::{COLNAME_NAME, FuncDef, Program};
use sqliters_vdbe::vdbemem::{CollRt, Mem};
use sqliters_vdbe::vdbeaux::{P4, P4Vtab};

use crate::ConnectionInner;
use crate::engine::ExecContext;
use crate::db_set::{DatabaseSet, TempStoreMode};
use crate::execution_context::{CollationRegistry, ExecutionContext, FunctionRegistry, ProgressTracker};

/// True if the environment variable `SQLITERS_VDBE_EXEC` is set: statements the generator and
/// the machine support completely are executed by the machine.
pub(crate) fn exec_switch_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| std::env::var_os("SQLITERS_VDBE_EXEC").is_some());
    #[cfg(test)]
    let on = FORCE.with(std::cell::Cell::get).unwrap_or(on);
    on
}

thread_local! {
    /// An override of the process switch for statements prepared on the test thread.
    #[cfg(test)]
    static FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    /// Number of statements the machine has run on this thread.
    static RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Overrides the switch on the calling thread until dropped (tests).
#[cfg(test)]
#[must_use]
pub(crate) struct ForceMachine {
    previous: Option<bool>,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl ForceMachine {
    pub(crate) fn on() -> Self {
        Self::set(true)
    }

    pub(crate) fn off() -> Self {
        Self::set(false)
    }

    fn set(on: bool) -> Self {
        Self {
            previous: FORCE.with(|f| f.replace(Some(on))),
            _thread: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
impl Drop for ForceMachine {
    fn drop(&mut self) {
        FORCE.with(|f| f.set(self.previous));
    }
}

/// Number of statements the machine has run on the calling thread (tests).
#[cfg(test)]
pub(crate) fn machine_runs() -> u64 {
    RUNS.with(std::cell::Cell::get)
}

fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SQLITERS_VDBE_TRACE").is_some())
}

fn trace(what: &str, sql: &str) {
    if trace_on() {
        let first: String = sql.chars().take(200).map(|c| if c == '\n' { ' ' } else { c }).collect();
        eprintln!("vm: {what} :: {first}");
    }
}

/// The program the machine runs for a read statement, `None` when the statement stays on the
/// engine's execution path (always, when the switch is off); `Err`: C's compile error (the
/// error of `sqlite3_prepare`, as for a statement that writes).
pub(crate) fn machine_program(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    raw_sql: &str,
    n_param: usize,
) -> Result<Option<Arc<Program>>> {
    machine_program_with_prepare(inner, stmt, raw_sql, n_param, None)
}

pub(crate) fn machine_program_with_prepare(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    raw_sql: &str,
    n_param: usize,
    prepared: Option<Box<Program>>,
) -> Result<Option<Arc<Program>>> {
    if !exec_switch_on() {
        return Ok(None);
    }
    read_machine_program(inner, stmt, raw_sql, n_param, ReadMachineScope::Standard, None, prepared)
}

pub(crate) fn custom_sort_policy_matches(inner: &ConnectionInner, program: &Program) -> bool {
    if program.fallible_limit_sort_open.is_some() {
        !inner.authorizer.is_some()
    } else if program.fallible_file_sort_open.is_some() {
        file_sort_policy(inner)
    } else {
        true
    }
}

fn file_sort_policy(inner: &ConnectionInner) -> bool {
    !inner.authorizer.is_some() && inner.db_set.temp_store() == TempStoreMode::File
        && inner.db_set.worker_threads_limit == 0
}

pub(crate) fn custom_sort_read_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, sql: &str, n_param: usize,
) -> Option<Arc<Program>> {
    custom_limit_sort_read_program(inner, stmt, sql, n_param).or_else(|| {
        if file_sort_policy(inner) {
            custom_file_sort_read_program(inner, stmt, sql, n_param)
        } else {
            None
        }
    })
}

/// A plain ORDER BY with LIMIT uses a private index rather than the full sorter.
#[allow(non_upper_case_globals)]
fn custom_limit_sort_read_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, sql: &str, n_param: usize,
) -> Option<Arc<Program>> {
    use sqliters_vdbe::opcodes::*;
    let Some(Stmt::Select(select)) = stmt else { return None };
    if inner.authorizer.is_some() {
        return None;
    }
    let (name, root) = crate::engine::custom_limit_order_collation(select, &inner.db_set, &inner.collations)?;
    let PrepareResult::Program(mut program) = crate::cgen::c_prepare(inner, sql).0 else {
        return None;
    };
    if !program.read_only || program.explain != 0 || program.btree_mask != 1
        || !program.p_program.is_empty() || program.n_var as usize != n_param
    {
        return None;
    }
    let open = sqliters_vdbe::vdbe::custom_limit_sort_open(
        &program, name, inner.effective_encoding() as u8,
    )?;
    let mut reads = program.a_op.iter().filter(|op| op.opcode == OP_OpenRead);
    let read = reads.next()?;
    let sort = &program.a_op[open];
    if reads.next().is_some() || read.p1 == sort.p1 || read.p2 != root as i32 || read.p3 != 0
        || !matches!(read.p4, P4::Int32(_))
    {
        return None;
    }
    if !program.a_op.iter().enumerate().all(|(pc, op)| match op.opcode {
        OP_Init | OP_Integer | OP_Int64 | OP_Variable | OP_MustBeInt
        | OP_OffsetLimit | OP_IfNot | OP_IfNotZero | OP_IfPos | OP_AddImm
        | OP_MakeRecord | OP_ResultRow | OP_Goto | OP_Halt | OP_OpenRead => true,
        OP_OpenEphemeral => pc == open,
        OP_Transaction => op.p1 == 0 && op.p2 == 0,
        OP_Rewind | OP_Rowid => op.p1 == read.p1,
        OP_Column | OP_Next => op.p1 == read.p1 || op.p1 == sort.p1,
        OP_Sequence | OP_Last | OP_IdxLE | OP_Delete | OP_IdxInsert | OP_Sort => op.p1 == sort.p1,
        _ => false,
    }) {
        return None;
    }
    program.fallible_limit_sort_open = Some(open);
    trace("machine: custom LIMIT sorter", sql);
    Some(Arc::new(*program))
}

#[allow(non_upper_case_globals)]
fn custom_file_sort_read_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, sql: &str, n_param: usize,
) -> Option<Arc<Program>> {
    use sqliters_vdbe::opcodes::*;
    let Some(Stmt::Select(select)) = stmt else { return None };
    let (name, root) = crate::engine::custom_file_order_collation(select, &inner.db_set, &inner.collations)?;
    let PrepareResult::Program(mut program) = crate::cgen::c_prepare(inner, sql).0 else {
        return None;
    };
    if !program.read_only || program.explain != 0 || program.btree_mask != 1
        || !program.p_program.is_empty() || program.n_var as usize != n_param
    {
        return None;
    }
    let open = sqliters_vdbe::vdbe::custom_file_sort_open(
        &program, name, inner.effective_encoding() as u8,
    )?;
    let mut reads = program.a_op.iter().filter(|op| op.opcode == OP_OpenRead);
    let mut pseudos = program.a_op.iter().filter(|op| op.opcode == OP_OpenPseudo);
    let read = reads.next()?;
    let pseudo = pseudos.next()?;
    let sort = &program.a_op[open];
    if reads.next().is_some() || pseudos.next().is_some()
        || read.p1 == sort.p1 || pseudo.p1 == read.p1 || pseudo.p1 == sort.p1
        || read.p2 != root as i32 || read.p3 != 0 || !matches!(read.p4, P4::Int32(_))
    {
        return None;
    }
    if !program.a_op.iter().enumerate().all(|(pc, op)| match op.opcode {
        OP_Init | OP_MakeRecord | OP_ResultRow | OP_Goto | OP_Halt
        | OP_OpenRead | OP_OpenPseudo => true,
        OP_Transaction => op.p1 == 0 && op.p2 == 0,
        OP_Rewind | OP_Rowid | OP_Next => op.p1 == read.p1,
        OP_Column => op.p1 == read.p1 || op.p1 == pseudo.p1,
        OP_SorterOpen => pc == open,
        OP_SorterInsert | OP_SorterSort | OP_SorterNext => op.p1 == sort.p1,
        OP_SorterData => op.p1 == sort.p1 && op.p3 == pseudo.p1,
        _ => false,
    }) {
        return None;
    }
    program.fallible_file_sort_open = Some(open);
    trace("machine: custom FILE sorter", sql);
    Some(Arc::new(*program))
}

/// The worker sorter admits only the complete native BINARY rowid order.
pub(crate) fn worker_sort_read_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, sql: &str, n_param: usize,
) -> Option<Arc<Program>> {
    let Some(Stmt::Select(select)) = stmt else { return None };
    let encoding = inner.effective_encoding();
    // The standard machine prepare already handles the UTF-8 ON case.
    if (encoding == TextEncoding::Utf8 && exec_switch_on())
        || inner.db_set.worker_threads_limit != 1 || inner.db_set.temp_store().is_memory()
        || inner.db_set.sorter_mmap_limit > 0 || inner.collations.get_custom("BINARY").is_some()
        || select.order_by.len() != 2 || select.limit.is_some() || read_codegen_blocker(inner).is_some()
    {
        return None;
    }
    let PrepareResult::Program(program) = crate::cgen::c_prepare(inner, sql).0 else {
        return None;
    };
    if program.explain != 0 || program.n_var as usize != n_param
        || !sqliters_vdbe::vdbe::is_worker_sort(&program, encoding)
    {
        return None;
    }
    trace("machine: worker sorter", sql);
    Some(Arc::new(*program))
}

/// A materialized legacy read cannot hold the page that C has at ResultRow. Use the
/// streaming machine for a single mapped b-tree when its program uses only the
/// cursor, projection, rowid-bound, count and binary sorter operations below.
#[allow(non_upper_case_globals)]
pub(crate) fn mapped_table_read_program(
    inner: &ConnectionInner,
    functions: &crate::execution_context::FunctionRegistry,
    stmt: Option<&Stmt>,
    sql: &str,
    n_param: usize,
) -> Option<Arc<Program>> {
    use sqliters_syntax::{Expr, SelectCore, TableOrSubquery};
    use sqliters_vdbe::opcodes::*;

    if inner.authorizer.is_some() {
        return None;
    }
    // The first attachment cannot move when another attachment is detached.
    let first_attached = DatabaseId(2);
    let attached_write = !inner.is_autocommit()
        && !inner.schema_changed_in_tx
        && inner.db_set.slot(first_attached).and_then(|slot| slot.btree.as_ref())
            .is_some_and(|bt| bt.pager().db_kind() == DatabaseKind::NamedDisk
                && matches!(bt.journal_mode(), JournalMode::Delete | JournalMode::Wal)
                && bt.transaction_state() == TransactionState::Write);
    if !attached_write && !inner.db_set.slots().iter().any(|slot| {
        slot.btree.as_ref().is_some_and(|bt| bt.pager().uses_fetch())
    }) {
        return None;
    }
    let Some(Stmt::Select(select)) = stmt else {
        return None;
    };
    let SelectCore::Select { from: Some(from), .. } = &select.body.head else {
        return None;
    };
    if !select.ctes.is_empty() || !select.body.compounds.is_empty() || !from.joins.is_empty() {
        return None;
    }
    let TableOrSubquery::Table { name, table_args: None, .. } = &from.head else {
        return None;
    };
    let (db, crate::db_set::RelationRef::Table(table)) =
        inner.db_set.find_relation_ref_in_set(name).ok()?? else {
            return None;
        };
    let mapped = inner.db_set.slot(db)?.btree.as_ref()?.pager().uses_fetch();
    if table.root_pgno <= 1 || table.without_rowid
        || (!mapped && !(attached_write && db == first_attached))
    {
        return None;
    }
    let PrepareResult::Program(program) = crate::cgen::select_schema_probe_with_functions(inner, sql, functions).0 else {
        return None;
    };
    if program.explain != 0
        || program.n_var as usize != n_param
        || program.btree_mask != 1u32.checked_shl(db.index() as u32)?
        || sqliters_vdbe::vdbe::unsupported_reason_mapped_read(&program).is_some()
    {
        return None;
    }
    let mut opens = program.a_op.iter().filter(|op| op.opcode == OP_OpenRead);
    let open = opens.next()?;
    if opens.next().is_some()
        || (open.p2 != table.root_pgno as i32
            && !program.a_op.iter().any(|op| op.opcode == OP_Count && op.p3 == 0))
    {
        return None;
    }
    if !mapped {
        // A live table cursor can seek again after a rollback. A SQLite sorter
        // owns its rows after the sort, so keep its existing execution path.
        // A fixed LIMIT can use the same live scan when its only extra
        // instructions initialize and decrement the row counter.
        let limit_counter = select.limit.as_ref().and_then(|limit| {
            let Expr::Integer(rows) = limit.limit else {
                return None;
            };
            if rows < 2 || limit.offset.is_some() {
                return None;
            }
            let rows = i32::try_from(rows).ok()?;
            let decrement = program.a_op.iter().find(|op| op.opcode == OP_DecrJumpZero)?;
            Some((rows, decrement.p1))
        });
        let direct_scan = open.p2 == table.root_pgno as i32
            && open.p3 == db.index() as i32
            && program.a_op.iter().any(|op| matches!(op.opcode, OP_Next | OP_Prev))
            && program.a_op.iter().all(|op| match op.opcode {
                OP_Integer => limit_counter == Some((op.p1, op.p2)),
                OP_DecrJumpZero => limit_counter.is_some_and(|(_, register)| op.p1 == register),
                OP_Init | OP_Halt | OP_Transaction | OP_Goto | OP_OpenRead | OP_Close
                | OP_Rewind | OP_Last | OP_Next | OP_Prev | OP_Rowid | OP_Column
                | OP_RealAffinity | OP_ResultRow | OP_Noop => true,
                _ => false,
            });
        return direct_scan.then(|| Arc::new(*program));
    }
    if !program.a_op.iter().all(|op| match op.opcode {
        OP_Init | OP_Halt | OP_Transaction | OP_Goto | OP_OpenRead | OP_Close
        | OP_Rewind | OP_Last | OP_Next | OP_Prev | OP_Count | OP_Rowid | OP_Column
        | OP_SeekRowid | OP_SeekGE | OP_SeekGT | OP_SeekLE | OP_SeekLT | OP_NotExists
        | OP_Integer | OP_Int64 | OP_Null | OP_Variable | OP_Copy | OP_SCopy
        | OP_MustBeInt | OP_IsNull | OP_IfNot | OP_IfPos | OP_OffsetLimit | OP_DecrJumpZero
        | OP_RealAffinity | OP_ResultRow | OP_Noop => true,
        OP_SorterOpen => matches!(&op.p4, P4::KeyInfo(key) if
            inner.collations.get_custom("BINARY").is_none()
                && key.a_coll.iter().flatten().all(|c| c.name.eq_ignore_ascii_case("BINARY"))),
        OP_SorterInsert | OP_SorterSort | OP_SorterData | OP_SorterNext
        | OP_OpenPseudo | OP_MakeRecord => true,
        OP_Function => matches!(&op.p4, P4::FuncCtx(call)
            if call.p_func.name.eq_ignore_ascii_case("length")
                && !functions.has_scalar_name("length")),
        _ => false,
    }) {
        return None;
    }
    Some(Arc::new(*program))
}

/// LIKE's binding and index selection depend on prepare-time function metadata.
pub(crate) fn like_machine_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, raw_sql: &str, n_param: usize, bound_values: Option<&[Value]>,
) -> Result<Option<Arc<Program>>> {
    read_machine_program(inner, stmt, raw_sql, n_param, ReadMachineScope::Like, bound_values, None)
}

/// A read rejected by legacy validation despite C eliminating its table source.
/// This entry also works with the diagnostic switch off; it accepts only a
/// checked program with no Btree use. An empty path containing only control
/// flow and integer initialization also works in UTF-16 without text operations.
pub(crate) fn schema_free_read_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, raw_sql: &str, n_param: usize,
) -> Result<Option<Arc<Program>>> {
    read_machine_program(inner, stmt, raw_sql, n_param, ReadMachineScope::SchemaFree, None, None)
}

/// The C planner orders explicit WHERE terms before implicit table-function
/// constraints. A duplicate constraint on the hidden schema column can leave
/// one of the terms for the VM to evaluate after VFilter.
#[allow(non_upper_case_globals)]
pub(crate) fn locking_mode_table_read_program(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    sql: &str,
    n_param: usize,
) -> Result<Option<Arc<Program>>> {
    use sqliters_syntax::{SelectCore, TableOrSubquery};

    let Some(Stmt::Select(select)) = stmt else {
        return Ok(None);
    };
    let SelectCore::Select { from: Some(from), .. } = &select.body.head else {
        return Ok(None);
    };
    if !select.ctes.is_empty() || !select.body.compounds.is_empty() || !from.joins.is_empty() {
        return Ok(None);
    }
    let TableOrSubquery::TableFunction { schema: None, name, args, .. } = &from.head else {
        return Ok(None);
    };
    if args.len() != 1
        || !name.eq_ignore_ascii_case("pragma_locking_mode")
        || !inner.db_set.vtab_registry().has_automatic_pragma_module(name)
    {
        return Ok(None);
    }
    Ok(read_machine_program(inner, stmt, sql, n_param, ReadMachineScope::Standard, None, None)?.filter(|program| {
        use sqliters_vdbe::opcodes::{OP_OpenRead, OP_ReopenIdx, OP_VOpen};

        !program.a_op.iter().any(|op| {
            matches!(op.opcode, OP_OpenRead | OP_ReopenIdx)
                || (op.opcode == OP_VOpen
                    && !matches!(&op.p4, P4::Vtab(table)
                        if table.name.eq_ignore_ascii_case("pragma_locking_mode")))
        }) && program.a_op.iter().any(|op| {
            matches!(&op.p4, P4::Vtab(vtab) if vtab.name.eq_ignore_ascii_case("pragma_locking_mode"))
        })
    }))
}

/// A simple ordered index scan can omit its sorter only when the planner and the
/// engine agree on the collation of the key it reads.
#[allow(non_upper_case_globals)]
fn ordered_index_collation_mismatch(
    inner: &ConnectionInner,
    select: &sqliters_syntax::SelectStmt,
    program: &Program,
) -> bool {
    use sqliters_syntax::{Expr, IndexHint, ResultColumn, SelectCore, TableOrSubquery};
    use sqliters_types::Collation;
    use sqliters_vdbe::opcodes::{OP_OpenEphemeral, OP_OpenRead, OP_SorterOpen};

    let [order] = select.order_by.as_slice() else {
        return false;
    };
    let Expr::Collate { expr, collation } = &order.expr else {
        return false;
    };
    let (Expr::Column(order_column), Some(requested)) =
        (expr.as_ref(), Collation::from_name(collation))
    else {
        return false;
    };
    let SelectCore::Select {
        distinct: false,
        projection,
        from: Some(from),
        selection: None,
        group_by,
        having: None,
        windows,
    } = &select.body.head
    else {
        return false;
    };
    let [ResultColumn::Expr { expr: Expr::Column(projected), alias: None, .. }] =
        projection.as_slice()
    else {
        return false;
    };
    let TableOrSubquery::Table {
        name,
        table_args: None,
        alias: None,
        indexed_by,
    } = &from.head
    else {
        return false;
    };
    if !select.ctes.is_empty()
        || !select.body.compounds.is_empty()
        || select.limit.is_some()
        || order.nulls_last.is_some()
        || !from.joins.is_empty()
        || !group_by.is_empty()
        || !windows.is_empty()
        || order_column.schema.is_some()
        || order_column.table.is_some()
        || projected.schema.is_some()
        || projected.table.is_some()
        || !projected.column.eq_ignore_ascii_case(&order_column.column)
        || program.a_op.iter().any(|op| matches!(op.opcode, OP_OpenEphemeral | OP_SorterOpen))
    {
        return false;
    }
    let Ok(Some((db_id, crate::db_set::RelationRef::Table(table)))) =
        inner.db_set.find_relation_ref_in_set(name)
    else {
        return false;
    };
    let Some(slot) = inner.db_set.slot(db_id) else {
        return false;
    };
    let index = match indexed_by {
        Some(IndexHint::IndexedBy(index_name)) => slot.schema.find_index(index_name),
        Some(IndexHint::NotIndexed) => return false,
        None => {
            // Resolve the actual index selected by the program, rather than a
            // candidate that merely happens to have the requested column.
            let mut opens = program.a_op.iter().filter(|op| {
                op.opcode == OP_OpenRead && matches!(&op.p4, P4::KeyInfo(_))
            });
            let (Some(open), None) = (opens.next(), opens.next()) else {
                return false;
            };
            if open.p3 != db_id.index() as i32 {
                return false;
            }
            slot.schema.indexes.iter().find(|index| index.root_pgno as i32 == open.p2)
        }
    };
    let Some(index) = index else {
        return false;
    };
    let [key] = index.columns.as_slice() else {
        return false;
    };
    if db_id != DatabaseId::MAIN
        || table.without_rowid
        || index.is_autoindex
        || index.where_clause.is_some()
        || !index.table_name.eq_ignore_ascii_case(&table.name)
        || key.col_idx.is_none()
        || !key.name.eq_ignore_ascii_case(&order_column.column)
        || key.sort_order.custom_collation.is_some()
        || inner.collations.get_custom(collation).is_some()
        || inner.collations.get_custom(key.sort_order.collation_name()).is_some()
        || key.sort_order.collation == requested
    {
        return false;
    }

    program.a_op.iter().any(|op| {
        let P4::KeyInfo(key_info) = &op.p4 else {
            return false;
        };
        op.opcode == OP_OpenRead
            && op.p2 == index.root_pgno as i32
            && op.p3 == db_id.index() as i32
            && key_info.a_coll.first().is_some_and(|collation| {
                let compiled = collation.as_ref().map_or(Some(Collation::Binary), |collation| {
                    Collation::from_name(&collation.name)
                });
                compiled == Some(requested)
            })
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadMachineScope {
    Standard,
    SchemaFree,
    Like,
    RowCase,
    LiveTableJoin,
}

/// Legacy expressions do not prepare a row-valued subquery as a CASE operand.
/// Its read program keeps the subquery's registers, collation and ResultRow
/// boundaries together, including on UTF-16 databases.
pub(crate) fn row_case_read_program(
    inner: &ConnectionInner,
    stmt: &Stmt,
    sql: &str,
    n_param: usize,
    bound_values: Option<&[Value]>,
) -> Result<Option<Arc<Program>>> {
    let Stmt::Select(select) = stmt else {
        return Ok(None);
    };
    if !select_has_row_case(select)
        && !crate::engine::any_referenced_view_select(select, &inner.db_set, select_has_row_case)
    {
        return Ok(None);
    }
    read_machine_program(inner, Some(stmt), sql, n_param, ReadMachineScope::RowCase, bound_values, None)
}

/// Replace only the confirmed prepare-time diagnostic for a row subquery
/// compared with scalar WHEN values. The probe never supplies an executable program.
pub(crate) fn scalar_when_row_subquery_error(
    inner: &ConnectionInner,
    select: &sqliters_syntax::SelectStmt,
    sql: &str,
    legacy_message: &str,
) -> Option<SqliteError> {
    use crate::dml_c::{Walk, walk_select};
    use sqliters_syntax::{Expr, ResultColumn, SelectCore};

    let width = legacy_message.strip_prefix("sub-select returns ")?
        .strip_suffix(" columns - expected 1")?.parse::<usize>().ok()?;
    if width < 2 {
        return None;
    }
    let mut scalar_when = false;
    let mut custom_collation = false;
    walk_select(select, &mut |expr| {
        if let Expr::Case { operand: Some(operand), branches, .. } = expr
            && let Expr::Subquery(query) = operand.as_ref()
        {
            // Star projection widths are decided by C after name resolution.
            let could_return_row = match &query.body.head {
                SelectCore::Select { projection, .. } => projection.len() > 1
                    || projection.iter().any(|column| {
                        matches!(column, ResultColumn::Star | ResultColumn::TableStar { .. })
                    }),
                SelectCore::Values(rows) => rows.first().is_some_and(|row| row.len() > 1),
            };
            scalar_when |= could_return_row && branches.iter().all(|(when, _)| {
                !matches!(when, Expr::RowValue(_) | Expr::Subquery(_))
            });
        }
        if let Expr::Collate { collation, .. } = expr {
            custom_collation |= inner.collations.get_custom(collation).is_some();
        }
        Walk::Deep
    });
    if !scalar_when || custom_collation || read_codegen_blocker(inner).is_some()
        || inner.collations.get_custom("BINARY").is_some()
    {
        return None;
    }
    match crate::cgen::c_prepare_diagnostic(inner, sql) {
        PrepareResult::Error { code, message, offset }
            if code == ErrorCode::Error as i32 && message == "row value misused" =>
        {
            Some(SqliteError::with_extended(code, message)
                .with_offset(usize::try_from(offset).unwrap_or(SqliteError::NO_OFFSET)))
        }
        _ => None,
    }
}

fn select_has_row_case(select: &sqliters_syntax::SelectStmt) -> bool {
    use crate::dml_c::{Walk, walk_select};
    use sqliters_syntax::Expr;

    let mut has_row_case = false;
    walk_select(select, &mut |expr| {
        if let Expr::Case { operand: Some(operand), branches, .. } = expr {
            match operand.as_ref() {
                Expr::Subquery(_) => {
                    has_row_case |= branches.iter().any(|(when, _)| {
                        matches!(when, Expr::RowValue(_) | Expr::Subquery(_))
                    });
                }
                Expr::RowValue(_) => {
                    has_row_case |= branches.iter().any(|(when, _)| {
                        matches!(when, Expr::Subquery(_))
                    });
                }
                _ => {}
            }
        }
        Walk::Deep
    });
    has_row_case
}

/// A direct rowid-table join must keep its two cursors between ResultRow and
/// the next step, so a write on the same connection can move either cursor.
#[allow(non_upper_case_globals)]
pub(crate) fn live_table_join_read_program(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    raw_sql: &str,
    n_param: usize,
    bound_values: Option<&[Value]>,
) -> Option<Arc<Program>> {
    use sqliters_syntax::{JoinType, SelectCore, TableOrSubquery};
    use sqliters_vdbe::opcodes::*;

    let Some(Stmt::Select(select)) = stmt else { return None };
    let SelectCore::Select { from: Some(from), distinct: false, .. } = &select.body.head
    else { return None };
    let [join] = from.joins.as_slice() else { return None };
    if !select.ctes.is_empty() || !select.body.compounds.is_empty()
        || !matches!(join.join_type, JoinType::Inner | JoinType::Cross)
        || join.natural || join.constraint.is_some()
    {
        return None;
    }
    let root = |source: &TableOrSubquery| {
        let TableOrSubquery::Table { name, table_args: None, .. } = source else { return None };
        let (db, table) = inner.db_set.find_user_table_in_set(name)?;
        (db == DatabaseId::MAIN && table.root_pgno > 1 && !table.without_rowid)
            .then_some(table.root_pgno as i32)
    };
    let left = root(&from.head)?;
    let right = root(&join.right)?;
    if left == right {
        return None;
    }
    let program = read_machine_program(
        inner, stmt, raw_sql, n_param, ReadMachineScope::LiveTableJoin, bound_values, None,
    ).ok()??;
    if program.btree_mask != 1 || !program.a_op.iter().all(|op| {
        matches!(op.opcode,
            OP_Init | OP_Halt | OP_Transaction | OP_Goto | OP_Noop | OP_OpenRead | OP_Close
            | OP_Rewind | OP_Last | OP_Next | OP_Prev | OP_Rowid | OP_Column | OP_RealAffinity
            | OP_ResultRow | OP_Integer | OP_Int64 | OP_Real | OP_String8 | OP_String | OP_Null
            | OP_Variable | OP_Copy | OP_SCopy | OP_Eq | OP_Ne | OP_Lt | OP_Le | OP_Gt | OP_Ge
            | OP_IsNull | OP_NotNull | OP_If | OP_IfNot)
        && !matches!(&op.p4, P4::CollSeq(coll) if inner.collations.get_custom(&coll.name).is_some())
    }) {
        return None;
    }
    let mut rows = program.a_op.iter().enumerate().filter(|(_, op)| op.opcode == OP_ResultRow);
    let (result_row, _) = rows.next()?;
    if rows.next().is_some() {
        return None;
    }
    let mut opens = program.a_op.iter().filter(|op| op.opcode == OP_OpenRead);
    let first = opens.next()?;
    let second = opens.next()?;
    let roots = (first.p2, second.p2);
    if opens.next().is_some() || first.p3 != 0 || second.p3 != 0 || first.p1 == second.p1
        || (roots != (left, right) && roots != (right, left))
    {
        return None;
    }
    [first, second].iter().all(|open| {
        program.a_op.iter().skip(result_row + 1).any(|op| {
            matches!(op.opcode, OP_Next | OP_Prev) && op.p1 == open.p1 && op.p2 as usize <= result_row
        })
    }).then_some(program)
}

fn read_codegen_blocker(inner: &ConnectionInner) -> Option<&'static str> {
    if inner.authorizer.is_some() {
        // C's generator asks the authorizer while it resolves names (sqlite3AuthRead); the port
        // of the generator has no authorizer hook yet.
        return Some("fallback: authorizer");
    }
    // A schema that declares an application-defined collation: C refuses a statement that
    // needs a collation that is not registered ("no such collation sequence"), and a
    // registered one can fail in this engine; the generator and the machine have neither
    // path yet, so such connections stay on the engine's executor.
    let custom_coll = inner.db_set.slots().iter().any(|slot| {
        slot.schema.tables.iter().any(|t| t.columns.iter().any(|c| c.custom_collation.is_some()))
            || slot.schema.indexes.iter().any(|ix| ix.columns.iter().any(|ic| ic.sort_order.custom_collation.is_some()))
    });
    if custom_coll {
        return Some("fallback: schema with an application-defined collation");
    }
    None
}

fn read_machine_program(
    inner: &ConnectionInner, stmt: Option<&Stmt>, raw_sql: &str, n_param: usize, scope: ReadMachineScope, bound_values: Option<&[Value]>,
    precompiled: Option<Box<Program>>,
) -> Result<Option<Arc<Program>>> {
    let Some(Stmt::Select(select)) = stmt else { return Ok(None) };
    if let Some(blocker) = read_codegen_blocker(inner) {
        trace(blocker, raw_sql);
        return Ok(None);
    }
    if crate::engine::schema_free_read_has_collation_override(select, &inner.collations) {
        trace("fallback: registered scalar collation override", raw_sql);
        return Ok(None);
    }
    let prepared = if scope == ReadMachineScope::SchemaFree {
        match crate::cgen::c_prepare(inner, raw_sql).0 {
            PrepareResult::Program(p) if p.n_var as usize == n_param && empty_program_path(&p) => {
                trace("machine: empty schema-free path", raw_sql);
                return Ok(Some(Arc::new(*p)));
            }
            result => Some(result),
        }
    } else {
        precompiled.map(PrepareResult::Program)
    };
    if inner.effective_encoding() != TextEncoding::Utf8
        && !matches!(scope, ReadMachineScope::Like
                | ReadMachineScope::RowCase
                | ReadMachineScope::LiveTableJoin)
    {
        trace("fallback: UTF-16 database", raw_sql);
        return Ok(None);
    }
    match prepared.unwrap_or_else(|| match bound_values {
        Some(values) => crate::cgen::c_prepare_bound(inner, raw_sql, values).0,
        None => crate::cgen::c_prepare(inner, raw_sql).0,
    }) {
        PrepareResult::Program(mut p) => {
            if scope == ReadMachineScope::Like && !crate::cgen::program_has_like(&p) { return Ok(None); }
            if scope == ReadMachineScope::SchemaFree && p.btree_mask != 0 { return Ok(None); }
            if matches!(scope, ReadMachineScope::RowCase | ReadMachineScope::LiveTableJoin)
                && !p.read_only
            {
                return Ok(None);
            }
            if p.explain != 0 {
                return Ok(None);
            }
            let mut table_scan = false;
            if let Some(Stmt::Select(select)) = stmt
                && let Some((table_root, index_root)) =
                    crate::engine::mixed_collation_table_scan(select, &inner.db_set, &inner.collations)
                && opens_main_root(&p, index_root, true)
            {
                let PrepareResult::Program(table_program) =
                    crate::cgen::c_prepare_table_scan(inner, raw_sql, bound_values, table_root)
                else {
                    trace("fallback: mixed index table scan", raw_sql);
                    return Ok(None);
                };
                if !opens_main_root(&table_program, table_root, false)
                    || opens_main_root(&table_program, index_root, true)
                {
                    trace("fallback: mixed index table scan", raw_sql);
                    return Ok(None);
                }
                p = table_program;
                table_scan = true;
            }
            let unsupported = if scope == ReadMachineScope::RowCase && inner.collations.get_custom("BINARY").is_none() {
                let grouped_case = matches!(stmt, Some(Stmt::Select(select))
                    if select.ctes.is_empty() && select.body.compounds.is_empty()
                        && matches!(&select.body.head, sqliters_syntax::SelectCore::Select { group_by, .. }
                            if !group_by.is_empty()));
                let distinct_case = matches!(stmt, Some(Stmt::Select(select))
                    if select.ctes.is_empty() && select.body.compounds.is_empty()
                        && matches!(&select.body.head, sqliters_syntax::SelectCore::Select { distinct: true, group_by, .. }
                            if group_by.is_empty()));
                let builtin_collations = inner.collations.get_custom("NOCASE").is_none()
                    && inner.collations.get_custom("RTRIM").is_none();
                sqliters_vdbe::vdbe::unsupported_reason_row_case_read(
                    &p, grouped_case, distinct_case, builtin_collations, inner.effective_encoding() as u8,
                )
            } else {
                unsupported_reason(&p)
            };
            if let Some(why) = unsupported {
                trace(&format!("fallback: {why}"), raw_sql);
                return Ok(None);
            }
            if p.a_op.iter().any(|op| matches!(&op.p4, P4::FuncCtx(c) if c.p_func.name.eq_ignore_ascii_case("rtreecheck"))) {
                // (evaluated by the engine's executor, not by the function library)
                trace("fallback: rtreecheck()", raw_sql);
                return Ok(None);
            }
            if p.n_var as usize != n_param {
                trace("fallback: parameter numbering", raw_sql);
                return Ok(None);
            }
            if matches!(stmt, Some(Stmt::Select(select)) if ordered_index_collation_mismatch(inner, select, &p)) {
                trace("fallback: index collation mismatch", raw_sql);
                return Ok(None);
            }
            trace(if table_scan { "machine: mixed index table scan" } else { "machine" }, raw_sql);
            Ok(Some(Arc::new(*p)))
        }
        PrepareResult::Error { code, message, offset } => {
            // A function the engine's library has and the generator's function table does not
            // (testfixture functions of `enable_test_functions`): the engine runs the statement.
            if let Some(name) = message.strip_prefix("no such function: ")
                && (inner.functions.has_scalar_name(name)
                    || inner.functions.has_aggregate_name(name)
                    || sqliters_vdbe::builtin_scalar_flags(name, inner.runtime.enable_test_functions).is_some())
            {
                trace(&format!("fallback: generator error: {message}"), raw_sql);
                return Ok(None);
            }
            trace(&format!("machine: C compile error: {message}"), raw_sql);
            let e = SqliteError::with_extended(code, message);
            Err(e.with_offset(if offset >= 0 { offset as usize } else { SqliteError::NO_OFFSET }))
        }
        PrepareResult::Unsupported(why) => {
            trace(&format!("fallback: generator: {why}"), raw_sql);
            Ok(None)
        }
    }
}

fn opens_main_root(program: &Program, root: u32, index: bool) -> bool {
    program.a_op.iter().any(|op| {
        op.opcode == sqliters_vdbe::opcodes::OP_OpenRead
            && op.p3 == DatabaseId::MAIN.index() as i32
            && op.p2 == root as i32
            && matches!(&op.p4, P4::KeyInfo(_)) == index
    })
}

/// Follow unconditional control flow and encoding-independent integer setup.
/// Any text, cursor, function, error
/// halt, invalid jump, or cycle leaves the ordinary machine guards in force.
#[allow(non_upper_case_globals)]
fn empty_program_path(program: &Program) -> bool {
    integer_program_path(program, false)
}

/// Acyclic, data-free integer results do not evaluate collations or text.
#[allow(non_upper_case_globals)]
pub(crate) fn integer_program_path(program: &Program, rows: bool) -> bool {
    use sqliters_vdbe::opcodes::{OP_Goto, OP_Halt, OP_Init, OP_Integer, OP_ResultRow};
    if program.explain != 0 || program.btree_mask != 0 { return false; }
    let mut pc = 0usize;
    for _ in 0..program.a_op.len() {
        let Some(op) = program.a_op.get(pc) else { return false; };
        match op.opcode {
            OP_Init | OP_Goto => {
                if (op.opcode == OP_Init && pc != 0) || op.p5 != 0 { return false; }
                let Ok(next) = usize::try_from(op.p2) else { return false; };
                pc = next;
            }
            OP_Halt => return op.p1 == 0,
            OP_Integer => pc += 1,
            OP_ResultRow if rows => pc += 1,
            _ => return false,
        }
    }
    false
}

/// What C's compile result decides for a statement that writes, with the switch on.
pub(crate) enum WriteRoute {
    /// The engine prepares and runs the statement (switch off, a statement kind or a
    /// connection state the generator / the machine does not support yet).
    Engine,
    /// C's program: the machine runs it inside the engine's write statement.
    Program(Arc<Program>),
    /// C's compile error (`sqlite3_prepare` fails): message and byte offset (-1: none).
    Error { code: i32, message: String, offset: i32 },
}

/// The route of an INSERT / UPDATE / DELETE (also with a WITH prefix): C's compile result —
/// the program or C's error — decides before the engine's own prepare (diagnostic switch
/// `SQLITERS_VDBE_EXEC`). The second value: C's `sqlite3Init()` loaded the schemas during the
/// compilation.
pub(crate) fn machine_write_program(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    raw_sql: &str,
    bound_partial: Option<(u32, PartialIndexBinding<'_>)>,
) -> (WriteRoute, bool) {
    if !exec_switch_on() {
        return (WriteRoute::Engine, false);
    }
    if !matches!(
        stmt,
        Some(
            Stmt::Insert(_)
                | Stmt::Update(_)
                | Stmt::Delete(_)
                | Stmt::Begin { .. }
                | Stmt::Commit
                | Stmt::Rollback { .. }
                | Stmt::Savepoint(_)
                | Stmt::Release(_)
        )
    ) {
        return (WriteRoute::Engine, false);
    }
    if inner.authorizer.is_some() {
        trace("fallback: authorizer", raw_sql);
        return (WriteRoute::Engine, false);
    }
    if stmt.and_then(|stmt| crate::engine::MixedIndexWrite::from_stmt(stmt, true)).is_some_and(|write| {
        crate::engine::mixed_index_write(write, &inner.db_set, &inner.collations)
    }) {
        trace("fallback: mixed index write snapshot", raw_sql);
        return (WriteRoute::Engine, false);
    }
    if inner.mutation_pipeline.is_trace_enabled() {
        // (the engine's own row-mutation trace, `Connection::set_mutation_trace_enabled`: not
        // a C feature; the machine does not record it)
        trace("fallback: mutation trace", raw_sql);
        return (WriteRoute::Engine, false);
    }
    if !inner.db_set.open_blobs().is_empty() {
        // (an open incremental blob handle: the engine invalidates it when its row changes)
        trace("fallback: open blob handle", raw_sql);
        return (WriteRoute::Engine, false);
    }
    let custom_coll = inner.db_set.slots().iter().any(|slot| {
        slot.schema.tables.iter().any(|t| t.columns.iter().any(|c| c.custom_collation.is_some()))
            || slot.schema.indexes.iter().any(|ix| ix.columns.iter().any(|ic| ic.sort_order.custom_collation.is_some()))
    });
    if custom_coll {
        trace("fallback: schema with an application-defined collation", raw_sql);
        return (WriteRoute::Engine, false);
    }
    if inner.effective_encoding() != TextEncoding::Utf8 {
        trace("fallback: UTF-16 database", raw_sql);
        return (WriteRoute::Engine, false);
    }
    let (r, _, loaded) = match bound_partial {
        Some((root, binding)) => crate::cgen::c_prepare_bound_partial_index(inner, raw_sql, root, binding),
        None => crate::cgen::c_prepare(inner, raw_sql),
    };
    let route = match r {
        PrepareResult::Program(p) => {
            if p.explain != 0 {
                return (WriteRoute::Engine, loaded);
            }
            if let Some(why) = sqliters_vdbe::vdbe::unsupported_reason_write(&p) {
                trace(&format!("fallback: {why}"), raw_sql);
                return (WriteRoute::Engine, loaded);
            }
            if p.a_op.iter().any(|op| matches!(&op.p4, P4::FuncCtx(c) if c.p_func.name.eq_ignore_ascii_case("rtreecheck"))) {
                trace("fallback: rtreecheck()", raw_sql);
                return (WriteRoute::Engine, loaded);
            }
            // (a virtual table the program scans gets C's idxNum / idxStr / argv through
            // VirtualCursor::filter_c: every module of the engine takes C's plan there, D35)
            if p.a_op.iter().any(|op| op.opcode == sqliters_vdbe::opcodes::OP_RowCell) {
                // insert.c xferOptimization: "#ifdef SQLITE_TEST sqlite3_xferopt_count++" (the
                // transfer is coded)
                inner.db_set.record_xferopt();
            }
            trace("machine", raw_sql);
            WriteRoute::Program(Arc::new(*p))
        }
        PrepareResult::Error { code, message, offset } => {
            // A function the engine's library has and the generator's function table does not
            // (testfixture functions of `enable_test_functions`): the engine runs the statement.
            if let Some(name) = message.strip_prefix("no such function: ")
                && (inner.functions.has_scalar_name(name)
                    || inner.functions.has_aggregate_name(name)
                    || sqliters_vdbe::builtin_scalar_flags(name, inner.runtime.enable_test_functions).is_some())
            {
                trace(&format!("fallback: generator error: {message}"), raw_sql);
                return (WriteRoute::Engine, loaded);
            }
            trace(&format!("machine: C compile error: {message}"), raw_sql);
            WriteRoute::Error { code, message, offset }
        }
        PrepareResult::Unsupported(why) => {
            trace(&format!("fallback: generator: {why}"), raw_sql);
            WriteRoute::Engine
        }
    };
    (route, loaded)
}

/// The names of the result columns of `prog` (`sqlite3_column_name`), when it has them all.
pub(crate) fn column_names(prog: &Program) -> Option<Vec<String>> {
    let n = usize::from(prog.n_res_column);
    let alloc = usize::from(prog.n_res_alloc);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(prog.a_col_name.get(i + COLNAME_NAME * alloc)?.clone()?);
    }
    Some(out)
}

/// The connection as the machine sees it for one `sqlite3_step`.
pub(crate) struct MachineHost<'a> {
    pub db_set: &'a mut DatabaseSet,
    pub functions: &'a FunctionRegistry,
    pub collations: &'a CollationRegistry,
    pub progress: &'a mut ProgressTracker,
    pub runtime: &'a VmRuntimeContext,
    pub interrupt_flag: &'a std::sync::atomic::AtomicBool,
    pub strict_subtype: bool,
    pub encoding: TextEncoding,
}

impl<'a> MachineHost<'a> {
    pub(crate) fn new(inner: &'a mut ConnectionInner) -> MachineHost<'a> {
        let encoding = inner.effective_encoding();
        MachineHost {
            db_set: &mut inner.db_set,
            functions: &inner.functions,
            collations: &inner.collations,
            progress: &mut inner.progress,
            runtime: &inner.runtime,
            interrupt_flag: &inner.active_interrupt_flag,
            strict_subtype: inner.strict_subtype,
            encoding,
        }
    }

    fn dbs(&mut self) -> &mut DatabaseSet {
        self.db_set
    }

    fn colls(&self) -> &CollationRegistry {
        self.collations
    }

    fn prog(&mut self) -> &mut ProgressTracker {
        self.progress
    }

    fn prog_ref(&self) -> &ProgressTracker {
        self.progress
    }

    fn intr(&self) -> &std::sync::atomic::AtomicBool {
        self.interrupt_flag
    }

    fn fts5_aux(&mut self, name: &str, _args: &[RuntimeValue]) -> Result<RuntimeValue> {
        // (a program with virtual tables runs on WriteHost)
        Err(SqliteError::new(ErrorCode::Error, format!("unable to use function {name} in the requested context")))
    }

    fn svc(&mut self) -> ExecutionContext<'_> {
        ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: self.encoding,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        }
    }
}

fn pure_datetime_error(
    func: &FuncDef,
    args: &[RuntimeValue],
    context: u16,
    functions: &FunctionRegistry,
) -> Option<SqliteError> {
    use sqliters_codegen::consts::{NC_GenCol, NC_IsCheck, SQLITE_FUNC_BUILTIN};

    if func.func_flags & SQLITE_FUNC_BUILTIN == 0 {
        return None;
    }
    let name = ["date", "time", "datetime", "julianday", "unixepoch"]
        .into_iter()
        .find(|name| func.name.eq_ignore_ascii_case(name))?;
    // Only the ambient-time forms already recognized in this lane.
    match args {
        [] => {}
        [arg] if matches!(&arg.value, Value::Text(text) if text.eq_ignore_ascii_case("now")) => {}
        _ => return None,
    }
    if functions.find_scalar(name, args.len()).is_some() {
        return None;
    }
    let context = i32::from(context);
    let location = if context & NC_IsCheck != 0 {
        "a CHECK constraint"
    } else if context & NC_GenCol != 0 {
        "a generated column"
    } else {
        "an index"
    };
    Some(SqliteError::new(
        ErrorCode::Error,
        format!("non-deterministic use of {name}() in {location}"),
    ))
}

/// The methods of [`VdbeHost`] that are the same for a statement that reads
/// ([`MachineHost`]) and one that writes ([`WriteHost`]).
macro_rules! host_common {
    () => {
        fn get_meta(&mut self, i_db: usize, i_cookie: i32) -> Result<u32> {
            let Some(bt) = self.btree(i_db) else {
                return Ok(0);
            };
            let h = bt.pager().header();
            Ok(match i_cookie {
                1 => h.schema_cookie,
                2 => h.schema_format,
                6 => h.user_version as u32,
                8 => h.application_id as u32,
                _ => return Err(SqliteError::new(ErrorCode::Internal, "vdbe: OP_ReadCookie of this cookie is not ported")),
            })
        }

        fn find_collation(&mut self, name: &str) -> Option<CollRt> {
            self.colls().get_custom(name).map(CollRt::from_custom)
        }

        fn call_function(&mut self, func: &FuncDef, args: &[RuntimeValue]) -> Result<RuntimeValue> {
            // SQLITE_FUNC_EPHEM: the FuncDef xFindFunction of a virtual table returned
            // (sqlite3VtabOverloadFunction); FTS5's auxiliary functions run on the cursor
            // their first argument names (fts5ApiCallback)
            if func.func_flags & 0x0010 != 0 && sqliters_ext::Fts5AuxRegistry::is_aux_function(&func.name) {
                return self.fts5_aux(&func.name, args);
            }
            if func.name.eq_ignore_ascii_case("like") {
                let svc = self.svc();
                if func.func_flags & sqliters_codegen::consts::SQLITE_FUNC_BUILTIN != 0 {
                    let values: Vec<Value> = args.iter().map(|v| v.value.clone()).collect();
                    return sqliters_vdbe::eval_registered_like(&values, svc.runtime, false).map(RuntimeValue::plain);
                }
                return svc.functions.call_scalar_bound(
                    &func.name, (func.n_arg >= 0).then_some(func.n_arg as usize),
                    args, svc.runtime, svc.strict_subtype,
                );
            }
            self.svc().call_scalar(&func.name, args)
        }

        fn call_pure_function(
            &mut self,
            func: &FuncDef,
            args: &[RuntimeValue],
            context: u16,
        ) -> Result<RuntimeValue> {
            if let Some(error) = pure_datetime_error(func, args, context, self.svc().functions) {
                return Err(error);
            }
            self.call_function(func, args)
        }

        fn agg_open(&mut self, func: &FuncDef, n_arg: usize) -> Result<usize> {
            Ok(self.svc().agg_open(&func.name, n_arg)?.0)
        }

        fn agg_step(&mut self, id: usize, args: &[RuntimeValue]) -> Result<()> {
            self.svc().agg_step(AggregateStepId(id), args)
        }

        fn agg_inverse(&mut self, id: usize, args: &[RuntimeValue]) -> Result<()> {
            self.svc().agg_inverse(AggregateStepId(id), args)
        }

        fn agg_value(&mut self, id: usize) -> Result<RuntimeValue> {
            self.svc().agg_value(AggregateStepId(id))
        }

        fn agg_finalize(&mut self, id: usize) -> Result<RuntimeValue> {
            self.svc().agg_finalize(AggregateStepId(id))
        }

        fn open_ephemeral(&mut self) -> Result<Btree> {
            // BTREE_OMIT_JOURNAL | BTREE_SINGLE on a private pager. C keeps such a b-tree in
            // memory until its cache would spill to a temporary file; here it stays in memory.
            let opts = PagerOpenOptions {
                lock_geometry: Some(self.dbs().lock_geometry()),
                db_kind: Some(DatabaseKind::PrivateTemporary),
                temp_in_memory: true,
                ..PagerOpenOptions::default()
            };
            let mut bt = Btree::open_private_temporary(Arc::clone(self.dbs().disk_vfs()), opts)?;
            let _ = bt.pager_mut().set_locking_mode(LockingMode::Exclusive);
            bt.begin_write()?;
            Ok(bt)
        }

        fn limit_length(&self) -> i32 {
            // SQLITE_MAX_LENGTH (the engine has no sqlite3_limit())
            1_000_000_000
        }

        fn check_interrupt(&mut self) -> Result<()> {
            if self.intr().load(std::sync::atomic::Ordering::SeqCst) {
                return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
            }
            Ok(())
        }

        fn progress_ops(&self) -> u64 {
            self.prog_ref().interval()
        }

        fn progress_invoke(&mut self) -> Result<bool> {
            self.prog().invoke()
        }

        fn table_info(&mut self, name: &str) -> Option<Arc<TableRt>> {
            // (the operand carries the name only: the table is looked up as sqlite3FindTable
            // does — TEMP before MAIN, then the attached databases; OP_TypeCheck is coded for
            // STRICT tables only)
            let n = self.dbs().slot_count();
            let order = [1usize, 0].into_iter().chain(2..n);
            for i in order {
                let Some(slot) = self.dbs().slot(DatabaseId(i)) else {
                    continue;
                };
                if slot.btree.is_none() {
                    continue;
                }
                if let Some(t) = slot.schema.tables.iter().find(|t| t.strict && t.name.eq_ignore_ascii_case(name)) {
                    let cols = t
                        .columns
                        .iter()
                        .map(|c| TableColRt {
                            name: c.name.clone(),
                            affinity: match c.affinity {
                                sqliters_types::TypeAffinity::Blob => 0x41,
                                sqliters_types::TypeAffinity::Text => 0x42,
                                sqliters_types::TypeAffinity::Numeric => 0x43,
                                sqliters_types::TypeAffinity::Integer => 0x44,
                                sqliters_types::TypeAffinity::Real => 0x45,
                            },
                            // sqlite3StdType[]: the standard type names of a STRICT table
                            e_ctype: match c.type_name.to_ascii_uppercase().as_str() {
                                "ANY" => COLTYPE_ANY,
                                "BLOB" => COLTYPE_BLOB,
                                "INT" => COLTYPE_INT,
                                "INTEGER" => COLTYPE_INTEGER,
                                "REAL" => COLTYPE_REAL,
                                "TEXT" => COLTYPE_TEXT,
                                _ => COLTYPE_CUSTOM,
                            },
                            generated: c.generated.is_some(),
                            is_virtual: c.generated.as_ref().is_some_and(|(_, stored)| !*stored),
                        })
                        .collect();
                    return Some(Arc::new(TableRt { name: t.name.clone(), cols }));
                }
            }
            None
        }

        fn index_expr_mask(&mut self, i_db: usize, name: &str) -> u64 {
            // sqlite3VdbeFindIndexKey: "for(ii=0; ii<MIN(pIdx->nColumn, BMS); ii++){ iCol =
            // pIdx->aiColumn[ii]; if( iCol==XN_EXPR || (iCol>=0 && (pIdx->pTable->aCol[iCol]
            // .colFlags & COLFLAG_VIRTUAL)) ) mask |= MASKBIT(ii); }" (the trailing rowid / PRIMARY
            // KEY columns of an index are never expressions or generated columns)
            let Some(slot) = self.dbs().slot(DatabaseId(i_db)) else {
                return 0;
            };
            let Some(ix) = slot.schema.indexes.iter().find(|ix| ix.name.eq_ignore_ascii_case(name)) else {
                return 0;
            };
            let tab = slot.schema.tables.iter().find(|t| t.name.eq_ignore_ascii_case(&ix.table_name));
            let mut mask = 0u64;
            for (ii, ic) in ix.columns.iter().enumerate().take(64) {
                let is_virtual = match ic.col_idx {
                    None => true,
                    Some(ci) => tab
                        .and_then(|t| t.columns.get(ci))
                        .is_some_and(|c| c.generated.as_ref().is_some_and(|(_, stored)| !*stored)),
                };
                if is_virtual {
                    mask |= 1u64 << ii;
                }
            }
            mask
        }

        fn sorter_vfs(&mut self) -> Option<Arc<dyn sqliters_vfs::Vfs>> {
            Some(Arc::clone(self.dbs().disk_vfs()))
        }

        fn sorter_worker_threads(&mut self) -> i32 {
            if self.colls().get_custom("BINARY").is_some() {
                return 0;
            }
            self.dbs().worker_threads_limit
        }

        fn sorter_mmap_limit(&mut self) -> i32 {
            self.dbs().sorter_mmap_limit
        }

        fn sorter_config(&mut self) -> (i64, i64, bool) {
            let bt = self.dbs().main_btree();
            (i64::from(bt.pager().page_size()), i64::from(bt.cache_size()), self.dbs().temp_store().is_memory())
        }
    };
}

impl VdbeHost for MachineHost<'_> {
    fn open_limit_sort_index(&mut self) -> Result<Btree> {
        if self.db_set.temp_store().is_memory() {
            return self.open_ephemeral();
        }
        let opts = PagerOpenOptions {
            lock_geometry: Some(self.db_set.lock_geometry()),
            ..PagerOpenOptions::default()
        };
        let mut index = Btree::open_transient_index(Arc::clone(self.db_set.disk_vfs()), opts)?;
        index.begin_write()?;
        Ok(index)
    }

    fn text_encoding(&self) -> TextEncoding { self.encoding }
    fn btree(&mut self, i_db: usize) -> Option<&mut Btree> {
        self.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut())
    }

    fn begin_read_trans(&mut self, i_db: usize) -> Result<(u32, i32)> {
        let ahead = self.db_set.c_cookie_ahead();
        let Some(slot) = self.db_set.slot(DatabaseId(i_db)) else {
            return Err(SqliteError::new(ErrorCode::Error, format!("unknown database index {i_db}")));
        };
        // The engine's read step has begun the read transaction of every database the
        // statement names (`sqlite3BtreeBeginTrans(pBt, 0)` is then a no-op, as in C when
        // the b-tree is already in a transaction). A database that is not in one was not
        // seen by the engine: the statement is given back to the engine's executor.
        if slot.btree.as_ref().is_some_and(|bt| bt.transaction_state() == TransactionState::Open) {
            return Err(SqliteError::new(ErrorCode::Schema, "database schema has changed"));
        }
        Ok((slot.schema.schema_cookie + u32::from(ahead == Some(i_db)), slot.c_generation))
    }

    host_common!();
}

/// The connection as the machine sees it for a statement that writes: the engine's write
/// statement context (`ExecContext`), whose transaction, statement-journal and commit logic
/// the machine uses as the engine's own executor does.
pub(crate) struct WriteHost<'a, 'c> {
    pub ctx: &'a mut ExecContext<'c>,
    pub encoding: TextEncoding,
    /// A transaction-control statement (`BEGIN`, `COMMIT`, `SAVEPOINT`, …): `OP_Transaction`
    /// skips a database whose b-tree is not open (`if( pBt )`: the TEMP database before its
    /// first use; C's DML opens it while it prepares, the engine at `ensure_write_db`).
    pub control: bool,
    /// The place in `ExecContext::savepoints` of the savepoint the running `OP_Savepoint`
    /// operates on (set by `savepoint_close`, read by `vtab_savepoint`).
    pub sp_pos: usize,
    /// A statement that reads (a SELECT with virtual tables): `OP_Transaction` finds the read
    /// transactions the engine's read step began, as [`MachineHost`] does.
    pub read_stmt: bool,
}

impl WriteHost<'_, '_> {
    fn dbs(&mut self) -> &mut DatabaseSet {
        self.ctx.db_set
    }

    fn colls(&self) -> &CollationRegistry {
        self.ctx.collations
    }

    fn prog(&mut self) -> &mut ProgressTracker {
        self.ctx.progress
    }

    fn prog_ref(&self) -> &ProgressTracker {
        self.ctx.progress
    }

    fn intr(&self) -> &std::sync::atomic::AtomicBool {
        self.ctx.interrupt_flag
    }

    fn svc(&mut self) -> ExecutionContext<'_> {
        ExecutionContext {
            interrupt_flag: self.ctx.interrupt_flag,
            runtime: &*self.ctx.runtime,
            encoding: self.encoding,
            functions: self.ctx.functions,
            collations: self.ctx.collations,
            progress: &mut *self.ctx.progress,
            strict_subtype: self.ctx.strict_subtype,
        }
    }

    fn fts5_aux(&mut self, name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue> {
        let vals: Vec<Value> = args.iter().map(|a| a.value.clone()).collect();
        crate::engine::call_fts5_aux(self.ctx, name, &vals).map(RuntimeValue::plain)
    }

    /// The database of the connected virtual table a `P4_VTAB` operand names.
    fn vtab_db(&self, vt: &P4Vtab) -> Result<String> {
        match self.ctx.db_set.vtab_registry().get_connected_by_id(&vt.name, vt.id) {
            Some(e) => Ok(e.db_name.clone()),
            // (C: "if( pVtab==0 ) rc = SQLITE_LOCKED")
            None => Err(SqliteError::new(ErrorCode::Locked, "database table is locked")),
        }
    }

    /// `(iMeta, pSchema->iGeneration)` of database `i_db` once its transaction has begun.
    fn meta_and_generation(&mut self, i_db: usize) -> Result<(u32, i32)> {
        let ahead = self.ctx.db_set.c_cookie_ahead();
        let Some(slot) = self.ctx.db_set.slot(DatabaseId(i_db)) else {
            return Err(SqliteError::new(ErrorCode::Error, format!("unknown database index {i_db}")));
        };
        Ok((slot.schema.schema_cookie + u32::from(ahead == Some(i_db)), slot.c_generation))
    }
}

impl VdbeHost for WriteHost<'_, '_> {
    fn text_encoding(&self) -> TextEncoding { self.encoding }
    fn btree(&mut self, i_db: usize) -> Option<&mut Btree> {
        self.ctx.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut())
    }

    fn begin_read_trans(&mut self, i_db: usize) -> Result<(u32, i32)> {
        if self.read_stmt {
            // (see MachineHost::begin_read_trans)
            let open = self
                .ctx
                .db_set
                .slot(DatabaseId(i_db))
                .and_then(|s| s.btree.as_ref())
                .is_some_and(|bt| bt.transaction_state() == TransactionState::Open);
            if open {
                return Err(SqliteError::new(ErrorCode::Schema, "database schema has changed"));
            }
            return self.meta_and_generation(i_db);
        }
        // sqlite3BtreeBeginTrans(pBt, 0, &iMeta)
        self.ctx.ensure_read_db(DatabaseId(i_db), false)?;
        self.meta_and_generation(i_db)
    }

    fn begin_write_trans(&mut self, i_db: usize, p2: i32) -> Result<(u32, i32)> {
        // sqlite3BtreeBeginTrans(pBt, p2, &iMeta); the statement transaction
        // (sqlite3BtreeBeginStmt) is opened with it when the statement has a statement
        // journal (`ExecContext::ensure_write_db`: `stmt_tx.stmt_sp_slot`, set by
        // `run_write` from `Program.uses_stmt_journal`).
        if self.control {
            // "pBt = pDb->pBt; if( pBt ){ ... }"
            let Some(bt) = self.ctx.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut()) else {
                return Ok((0, 0));
            };
            if p2 >= 2 && bt.transaction_state() != TransactionState::Write {
                // sqlite3BtreeBeginTrans(pBt, 2, ..): an EXCLUSIVE lock at once
                self.ctx.busy_policy.retry_btree(bt, |b| b.begin_exclusive())?;
            }
        }
        self.ctx.ensure_write_db(DatabaseId(i_db), false)?;
        self.meta_and_generation(i_db)
    }

    fn set_last_rowid(&mut self, rowid: i64) {
        self.ctx.runtime.last_insert_rowid = rowid;
    }

    fn set_changes(&mut self, n: i64) {
        let n = n.max(0) as u64;
        self.ctx.runtime.last_changes = n;
        self.ctx.runtime.total_changes += n;
    }

    fn last_rowid(&self) -> i64 {
        self.ctx.runtime.last_insert_rowid
    }

    fn db_changes(&self) -> i64 {
        self.ctx.runtime.last_changes as i64
    }

    fn set_db_changes(&mut self, n: i64) {
        self.ctx.runtime.last_changes = n.max(0) as u64;
    }

    fn trigger_depth_limit(&self) -> usize {
        self.ctx.db_set.trigger_depth_limit()
    }

    fn defer_fks(&self) -> bool {
        self.ctx.db_set.c_defer_foreign_keys()
    }

    fn deferred_cons(&self) -> (i64, i64) {
        (self.ctx.deferred_fk_suspects.n_deferred_cons, self.ctx.deferred_fk_suspects.n_deferred_imm_cons)
    }

    fn add_deferred_cons(&mut self, imm: bool, n: i64) {
        if imm {
            self.ctx.deferred_fk_suspects.n_deferred_imm_cons += n;
        } else {
            self.ctx.deferred_fk_suspects.n_deferred_cons += n;
        }
    }

    fn randomness(&mut self) -> i64 {
        let mut b = [0u8; 8];
        self.ctx.db_set.main_btree().pager().vfs().randomness(&mut b);
        i64::from_le_bytes(b)
    }

    fn writable_schema(&self) -> bool {
        *self.ctx.writable_schema
    }

    fn vtab_begin(&mut self, vt: &P4Vtab) -> Result<()> {
        let db = self.vtab_db(vt)?;
        crate::vtab::vtab_begin_if_needed(self.ctx, &db, &vt.name)
    }

    fn vtab_update(&mut self, vt: &P4Vtab, args: &[Value], nochange: &[bool], on_conflict: u8) -> Result<i64> {
        use sqliters_types::{VTabConflictMode, VTabUpdateOperation};
        let db = self.vtab_db(vt)?;
        // xUpdate's argv: one argument = DELETE argv[0]; argv[0] NULL = INSERT; else UPDATE
        let op = if args.len() == 1 {
            VTabUpdateOperation::Delete { old_rowid: args[0].clone() }
        } else if matches!(args[0], Value::Null) {
            VTabUpdateOperation::Insert {
                new_rowid: if matches!(args[1], Value::Null) { None } else { Some(args[1].clone()) },
                values: args[2..].to_vec(),
            }
        } else {
            VTabUpdateOperation::Update {
                old_rowid: args[0].clone(),
                new_rowid: args[1].clone(),
                values: args[2..].to_vec(),
            }
        };
        let mode = match on_conflict {
            1 => VTabConflictMode::Rollback,
            3 => VTabConflictMode::Fail,
            4 => VTabConflictMode::Ignore,
            5 => VTabConflictMode::Replace,
            _ => VTabConflictMode::Abort,
        };
        let nochange_cols = if args.len() > 2 { nochange[2..].to_vec() } else { Vec::new() };
        let r = crate::vtab::vtab_perform_update_with_nochange(self.ctx, &db, &vt.name, op, mode, &nochange_cols)?;
        Ok(r.unwrap_or(0))
    }

    fn vtab_constraint_support(&self, vt: &P4Vtab) -> bool {
        self.ctx
            .db_set
            .vtab_registry()
            .get_connected_by_id(&vt.name, vt.id)
            .is_some_and(|e| e.declared.constraint_support)
    }

    fn vtab_open(&mut self, vt: &P4Vtab) -> Result<Box<dyn sqliters_types::VirtualCursor>> {
        let db = self.vtab_db(vt)?;
        let reg = self.ctx.db_set.vtab_registry_mut();
        let table = reg.checkout_vtab_instance(&db, &vt.name)?;
        let r = crate::vtab::catch_callback_panic("vtab xOpen", || table.open());
        self.ctx.db_set.vtab_registry_mut().restore_vtab(&db, &vt.name, table);
        let mut cur = r?;
        // (the machine asks for any column, row by row, as OP_VColumn / OP_VNext do)
        cur.set_fetch_mask(u64::MAX);
        let _ = cur.set_lazy(true);
        Ok(cur)
    }

    fn vtab_filter(
        &mut self,
        vt: &P4Vtab,
        cur: &mut dyn sqliters_types::VirtualCursor,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
    ) -> Result<()> {
        let db = self.vtab_db(vt)?;
        let mut host_ctx = crate::vtab::CoreVTabHostContext::new(self.ctx, &db, &vt.name);
        crate::vtab::catch_callback_panic("vtab xFilter", || cur.filter(&mut host_ctx, idx_num, idx_str, args))
    }

    fn vtab_filter_in(
        &mut self,
        vt: &P4Vtab,
        cur: &mut dyn sqliters_types::VirtualCursor,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
        in_lists: &[Option<Vec<Value>>],
    ) -> Result<()> {
        // OP_VFilter: "rc = pModule->xFilter(pVCur, iQuery, pOp->p4.z, nArg, apArg);" with the
        // plan of C's xBestIndex (VirtualCursor::filter_c)
        let db = self.vtab_db(vt)?;
        let mut host_ctx = crate::vtab::CoreVTabHostContext::new(self.ctx, &db, &vt.name);
        crate::vtab::catch_callback_panic("vtab xFilter", || cur.filter_c(&mut host_ctx, idx_num, idx_str, args, in_lists))
    }

    fn vtab_rowid(&mut self, _vt: &P4Vtab, cur: &mut dyn sqliters_types::VirtualCursor) -> Result<i64> {
        crate::vtab::catch_callback_panic("vtab xRowid", || cur.rowid())
    }

    fn vtab_integrity(&mut self, vt: &P4Vtab, schema: &str, is_quick: bool) -> Result<Option<String>> {
        // xIntegrity(pVtab, db->aDb[pOp->p1].zDbSName, pTab->zName, isQuick, &zErr)
        let db = match schema.parse::<usize>().ok().and_then(|i| self.ctx.db_set.slot(DatabaseId(i))) {
            Some(slot) => slot.name.clone(),
            None => return Ok(None),
        };
        if self.ctx.db_set.vtab_registry().get_connected(&db, &vt.name).is_none() {
            // "if( pTab->u.vtab.p==0 ) break;"
            return Ok(None);
        }
        let mut table = self.ctx.db_set.vtab_registry_mut().checkout_vtab_instance(&db, &vt.name)?;
        let r = {
            let mut host_ctx = crate::vtab::CoreVTabHostContext::new(self.ctx, &db, &vt.name);
            crate::vtab::catch_callback_panic("vtab xIntegrity", || table.integrity(&mut host_ctx, &db, &vt.name, is_quick))
        };
        self.ctx.db_set.vtab_registry_mut().restore_vtab(&db, &vt.name, table);
        r
    }

    fn vtab_column(&mut self, vt: &P4Vtab, cur: &mut dyn sqliters_types::VirtualCursor, i_col: usize) -> Result<Value> {
        let db = self.vtab_db(vt)?;
        let mut host_ctx = crate::vtab::CoreVTabHostContext::new(self.ctx, &db, &vt.name);
        crate::vtab::catch_callback_panic("vtab xColumn", || cur.column_ctx(&mut host_ctx, i_col))
    }

    fn vtab_next(&mut self, vt: &P4Vtab, cur: &mut dyn sqliters_types::VirtualCursor) -> Result<()> {
        let db = self.vtab_db(vt)?;
        let mut host_ctx = crate::vtab::CoreVTabHostContext::new(self.ctx, &db, &vt.name);
        crate::vtab::catch_callback_panic("vtab xNext", || cur.next_ctx(&mut host_ctx))
    }

    fn vtab_column_skips_nochange(&self, vt: &P4Vtab, i_col: usize) -> bool {
        // fts5ColumnMethod: "if( !sqlite3_vtab_nochange(pCtx) && pConfig->eContent!=
        // FTS5_CONTENT_NONE )" for a column below nCol (the hidden table and rank columns are
        // always read); geopolyColumn: "if( i==0 && sqlite3_vtab_nochange(ctx) ) return".
        let Some(e) = self.ctx.db_set.vtab_registry().get_connected_by_id(&vt.name, vt.id) else {
            return false;
        };
        match e.module_name.to_ascii_lowercase().as_str() {
            "fts5" => !e.declared.hidden_cols.get(i_col).copied().unwrap_or(true),
            "geopoly" => i_col == 0,
            _ => false,
        }
    }

    fn has_update_hook(&self) -> bool {
        self.ctx.mutation_pipeline.has_update_hook()
    }

    fn max_page_count(&self, i_db: usize) -> Option<u32> {
        self.ctx.db_set.slot(DatabaseId(i_db)).map(|s| s.max_page_count)
    }

    fn update_hook(&mut self, op: i32, i_db: usize, table: &str, rowid: i64) -> Result<()> {
        let Some(slot) = self.ctx.db_set.slot(DatabaseId(i_db)) else {
            return Ok(());
        };
        if op == 9
            && slot.schema.tables.iter().any(|t| t.name.eq_ignore_ascii_case(table) && t.without_rowid)
        {
            // (HasRowid(pTab) is false)
            return Ok(());
        }
        let db_name = slot.name.clone();
        let op = match op {
            18 => sqliters_types::RowMutationOp::Insert,
            23 => sqliters_types::RowMutationOp::Update,
            _ => sqliters_types::RowMutationOp::Delete,
        };
        self.ctx.mutation_pipeline.invoke_update_hook(op, &db_name, table, rowid)
    }

    fn corrupt_rdonly(&self) -> bool {
        self.ctx.db_set.corrupt_rdonly()
    }

    fn auto_commit(&self) -> bool {
        !*self.ctx.explicit_tx && !*self.ctx.implicit_savepoint_tx
    }

    fn set_auto_commit(&mut self, v: bool) {
        // db->autoCommit: the engine keeps it as two flags, `explicit_tx` (BEGIN) and
        // `implicit_savepoint_tx` (the transaction a SAVEPOINT opened in autocommit mode).
        if v {
            *self.ctx.explicit_tx = false;
            *self.ctx.implicit_savepoint_tx = false;
        } else {
            if self.ctx.savepoints.first().is_some_and(|sp| sp.is_transaction_savepoint) {
                *self.ctx.implicit_savepoint_tx = true;
            } else {
                *self.ctx.explicit_tx = true;
            }
            if self.ctx.db_set.slots().iter().any(|s| s.btree.as_ref().is_some_and(|bt| bt.transaction_state() != TransactionState::Open)) {
                *self.ctx.in_user_btree_tx = true;
            }
        }
        self.ctx.publish();
    }

    fn n_vdbe_write(&self) -> i32 {
        self.ctx.activity.other_writers as i32
    }

    fn fk_deferred_pending(&mut self) -> Result<bool> {
        // sqlite3VdbeCheckFkDeferred: "if( (db->nDeferredCons+db->nDeferredImmCons)==0 ) return
        // SQLITE_OK;" — and the rows a statement of the engine's own executor left suspect
        match crate::engine::check_deferred_fks(self.ctx) {
            Ok(()) => Ok(false),
            Err(e) if e.code == ErrorCode::Constraint => Ok(true),
            Err(e) => Err(e),
        }
    }

    fn rollback_all(&mut self) -> Result<()> {
        crate::engine::rollback_before_autocommit(self.ctx)
    }

    fn halt_transaction(&mut self, read_only: bool) -> Result<Option<SqliteError>> {
        halt_transaction(self.ctx, read_only)
    }

    fn close_savepoints(&mut self) {
        // sqlite3CloseSavepoints: "db->nSavepoint = 0; db->nStatement = 0;
        // db->isTransactionSavepoint = 0;" (db->autoCommit is left as it is)
        self.ctx.savepoints.clear();
        if *self.ctx.implicit_savepoint_tx {
            *self.ctx.implicit_savepoint_tx = false;
            *self.ctx.explicit_tx = true;
        }
        self.ctx.publish();
    }

    fn savepoint_find(&self, name: &str) -> Option<usize> {
        let n = self.ctx.savepoints.len();
        self.ctx.savepoints.iter().rposition(|sp| sp.name.eq_ignore_ascii_case(name)).map(|pos| n - 1 - pos)
    }

    fn savepoint_is_transaction(&self, k: usize) -> bool {
        // "pSavepoint->pNext==0 && db->isTransactionSavepoint"
        k + 1 == self.ctx.savepoints.len() && self.ctx.savepoints[0].is_transaction_savepoint && *self.ctx.implicit_savepoint_tx
    }

    fn set_transaction_savepoint(&mut self, v: bool) {
        if let Some(sp) = self.ctx.savepoints.first_mut() {
            sp.is_transaction_savepoint = v;
        }
    }

    fn vtab_savepoint(&mut self, op: i32, _k: usize) -> Result<()> {
        // sqlite3VtabSavepoint(db, op, iSavepoint): the engine's virtual-table layer numbers the
        // levels by `ExecContext::savepoints` (it subtracts the transaction savepoint itself).
        match op {
            sqliters_vdbe::vdbe::SAVEPOINT_BEGIN => {
                let idx = self.ctx.savepoints.len();
                crate::vtab::vtab_savepoint_all(self.ctx, idx)
            }
            sqliters_vdbe::vdbe::SAVEPOINT_RELEASE => {
                let idx = self.sp_pos;
                crate::vtab::vtab_release_all(self.ctx, idx)
            }
            _ => {
                let idx = self.sp_pos;
                crate::vtab::vtab_rollback_to_all(self.ctx, idx)
            }
        }
    }

    fn savepoint_open(&mut self, name: &str, transaction: bool) -> Result<()> {
        let slot_idx = self.ctx.savepoints.len();
        for db_slot in self.ctx.db_set.slots_mut() {
            if let Some(ref mut bt) = db_slot.btree {
                if slot_idx == 0 {
                    bt.pager_mut().set_transaction_savepoint(transaction);
                }
                if bt.transaction_state() == TransactionState::Write {
                    bt.pager_mut().ensure_subsavepoints(slot_idx + 1)?;
                }
            }
        }
        let schemas_at_open =
            self.ctx.db_set.slots().iter().filter(|s| s.btree.is_some()).map(|s| (s.schema_instance_id, s.schema.clone())).collect();
        self.ctx.savepoints.push(crate::engine::SqlSavepoint {
            name: name.to_owned(),
            subsavepoint_depth: slot_idx,
            is_transaction_savepoint: transaction,
            schemas_at_open,
            schema_gen_at_open: *self.ctx.schema_generation,
            v1_expire_gen_at_open: *self.ctx.v1_expire_generation,
            // pNew->nDeferredCons = db->nDeferredCons; pNew->nDeferredImmCons = db->nDeferredImmCons;
            deferred_fk_suspects: self.ctx.deferred_fk_suspects.clone(),
            mutation_len_at_open: self.ctx.mutation_pipeline.len(),
        });
        if transaction {
            // db->autoCommit = 0; db->isTransactionSavepoint = 1;
            *self.ctx.implicit_savepoint_tx = true;
            self.ctx.publish();
        }
        Ok(())
    }

    fn savepoint_schema_change(&self, _k: usize) -> bool {
        // "isSchemaChange = (db->mDbFlags & DBFLAG_SchemaChange)!=0" (set by DDL of the
        // transaction; the engine's schema generation also moves for other reasons)
        *self.ctx.schema_changed_in_tx
    }

    fn n_db(&self) -> usize {
        self.ctx.db_set.slot_count()
    }

    fn trip_all_cursors(&mut self, i_db: usize, write_only: bool) -> Result<()> {
        match self.ctx.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut()) {
            Some(bt) => bt.trip_cursors(sqliters_types::extended::SQLITE_ABORT_ROLLBACK, write_only),
            None => Ok(()),
        }
    }

    fn btree_savepoint(&mut self, i_db: usize, op: i32, k: usize) -> Result<()> {
        // sqlite3BtreeSavepoint: "if( p && p->inTrans==TRANS_WRITE ){ ... }"
        let depth = self.ctx.savepoints[self.ctx.savepoints.len() - 1 - k].subsavepoint_depth;
        let Some(bt) = self.ctx.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut()) else {
            return Ok(());
        };
        if bt.transaction_state() != TransactionState::Write {
            return Ok(());
        }
        if op == sqliters_vdbe::vdbe::SAVEPOINT_ROLLBACK {
            bt.rollback_to_subsavepoint(depth)
        } else {
            bt.pager_mut().release_subsavepoint(depth)
        }
    }

    fn savepoint_schema_reset(&mut self, k: usize) -> Result<()> {
        // sqlite3ExpirePreparedStatements(db, 0); sqlite3ResetAllSchemasOfConnection(db);
        // db->mDbFlags |= DBFLAG_SchemaChange; (the engine gives every database the schema it
        // had when the savepoint was opened)
        let pos = self.ctx.savepoints.len() - 1 - k;
        let schema_changed_since_sp = *self.ctx.schema_generation != self.ctx.savepoints[pos].schema_gen_at_open;
        if *self.ctx.schema_changed_in_tx {
            self.ctx.db_set.c_reset_all_schemas();
        }
        self.ctx.db_set.invalidate_all_blobs();
        if schema_changed_since_sp {
            let saved = self.ctx.savepoints[pos].schemas_at_open.clone();
            for db_slot in self.ctx.db_set.slots_mut() {
                if let Some((_, saved_schema)) = saved.iter().find(|(inst, _)| *inst == db_slot.schema_instance_id) {
                    db_slot.schema = saved_schema.clone();
                    db_slot.schema.rebind_collations(self.ctx.collations);
                } else if let Some(ref mut bt) = db_slot.btree {
                    db_slot.schema = crate::schema::Schema::load_for_schema(bt, Some(&db_slot.name))?;
                    db_slot.schema.rebind_collations(self.ctx.collations);
                }
            }
            self.ctx.db_set.sync_temp_trigger_bindings_from_schema();
        }
        *self.ctx.schema_generation += 1;
        *self.ctx.v1_expire_generation += 1;
        *self.ctx.schema_changed_in_tx = true;
        Ok(())
    }

    fn savepoint_close(&mut self, k: usize, release: bool) {
        let pos = self.ctx.savepoints.len() - 1 - k;
        self.sp_pos = pos;
        if release {
            self.ctx.savepoints.truncate(pos);
        } else {
            // db->nDeferredCons = pSavepoint->nDeferredCons; (and the engine's suspects, its
            // row-mutation log, the incremental blob handles that write)
            let sp = &self.ctx.savepoints[pos];
            *self.ctx.deferred_fk_suspects = sp.deferred_fk_suspects.clone();
            let mlen = sp.mutation_len_at_open;
            self.ctx.mutation_pipeline.truncate(mlen);
            self.ctx.savepoints.truncate(pos + 1);
            self.ctx.db_set.expire_writable_blobs();
            *self.ctx.rollback_count += 1;
        }
        self.ctx.publish();
    }

    host_common!();
}

/// The part of `sqlite3VdbeHalt(p)` that ends the transaction, for a statement that ran
/// without error and changes no row itself (`OP_AutoCommit`, `OP_Savepoint` RELEASE of the
/// transaction savepoint). See [`VdbeHost::halt_transaction`].
pub(crate) fn halt_transaction(ctx: &mut ExecContext<'_>, read_only: bool) -> Result<Option<SqliteError>> {
    use crate::engine::CommitFailureDisposition;
    let auto_commit = !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx;
    // "if( !sqlite3VtabInSync(db) && db->autoCommit && db->nVdbeWrite==(p->readOnly==0) )"
    // (this statement is the writer when it is not read-only; no statement of the connection
    // runs inside xSync when a transaction statement runs)
    let n_vdbe_write = ctx.activity.other_writers as i32 + i32::from(!read_only);
    if !(auto_commit && n_vdbe_write == i32::from(!read_only)) {
        // eStatementOp = SAVEPOINT_RELEASE: the statement has no statement transaction
        return Ok(None);
    }
    // "if( p->rc==SQLITE_OK || (p->errorAction==OE_Fail && !isSpecialError) )"
    let rc: Result<()> = match crate::engine::check_deferred_fks(ctx) {
        // ("if( NEVER(p->readOnly) ) return SQLITE_ERROR": OP_AutoCommit / OP_Savepoint checked)
        Err(e) if e.code == ErrorCode::Constraint => Err(sqliters_vdbe::vdbe::fk_error()),
        Err(e) => Err(e),
        Ok(()) if ctx.db_set.corrupt_rdonly() => {
            // "rc = SQLITE_CORRUPT; db->flags &= ~SQLITE_CorruptRdOnly;"
            ctx.db_set.set_corrupt_rdonly(false);
            Err(SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed"))
        }
        Ok(()) => {
            // rc = vdbeCommit(db, p)
            let saved = ctx.savepoints.clone();
            match crate::engine::commit_all_databases(ctx) {
                Ok(()) => Ok(()),
                Err((e, CommitFailureDisposition::LeaveOpen)) if e.code == ErrorCode::Busy && read_only => {
                    // "if( rc==SQLITE_BUSY && p->readOnly ){ sqlite3VdbeLeave(p); return SQLITE_BUSY; }"
                    return Err(e);
                }
                Err((e, CommitFailureDisposition::LeaveOpen)) if e.code == ErrorCode::Busy => Err(e),
                Err((e, CommitFailureDisposition::LeaveOpen)) if e.code == ErrorCode::Constraint => Err(e),
                Err((e, CommitFailureDisposition::LeaveOpen)) => {
                    // An I/O error of the commit (two-phase commit): C rolls the transaction back
                    // ("sqlite3RollbackAll(db, SQLITE_OK)", autocommit 1; the pager is in its error
                    // state and the journal stays hot until the next access). The engine's pager has
                    // no error state: its transaction stays open as in the engine's own COMMIT
                    // (difference E28, `m3c/fault/fault.c`).
                    return Err(e);
                }
                Err((e, CommitFailureDisposition::RolledBack)) => {
                    // (the engine's coordinator rolled back already and cleared the transaction
                    // state; C keeps the savepoints and db->autoCommit as they are)
                    *ctx.savepoints = saved;
                    ctx.publish();
                    return Ok(Some(e));
                }
            }
        }
    };
    match rc {
        Err(e) => {
            // "sqlite3SystemError(db, rc); p->rc = rc; sqlite3RollbackAll(db, SQLITE_OK);
            //  p->nChange = 0;"
            let had_write_tx = ctx.db_set.any_in_write_transaction();
            let _ = crate::engine::rollback_all_databases(ctx, true);
            *ctx.rollback_count += 1;
            *ctx.in_user_btree_tx = false;
            *ctx.schema_changed_in_tx = false;
            ctx.deferred_fk_suspects.clear();
            ctx.mutation_pipeline.clear();
            ctx.publish();
            if had_write_tx {
                ctx.commit_hooks.invoke_rollback_hook()?;
            }
            Ok(Some(e))
        }
        Ok(()) => {
            // "db->nDeferredCons = 0; db->nDeferredImmCons = 0; db->flags &= ~SQLITE_DeferFKs;
            //  sqlite3CommitInternalChanges(db);"
            ctx.deferred_fk_suspects.clear();
            *ctx.schema_changed_in_tx = false;
            *ctx.in_user_btree_tx = false;
            ctx.mutation_pipeline.clear();
            ctx.publish();
            Ok(None)
        }
    }
}

/// A statement that writes, run by the machine to its end inside the engine's write
/// statement (`lib.rs`: the implicit transaction, the statement savepoint, the commit or
/// the abort of the statement are the engine's, as for its own executor). `prog` is the
/// program C generates for the statement.
///
/// What `sqlite3VdbeHalt` does with the counters: a statement that ends normally sets
/// `changes()` to its `nChange` and adds it to `total_changes()`.
pub(crate) fn run_write(
    ctx: &mut ExecContext<'_>,
    prog: &Arc<Program>,
    params: &[Value],
    sql: &str,
) -> Result<crate::engine::QueryOutput> {
    // `Vdbe.usesStmtJournal` (C: `isMultiWrite && mayAbort`): without it C opens no
    // statement transaction (see `engine::apply_stmt_journal_policy`).
    sqliters_types::dtrace!("TR|stmt|usj|v={}", u8::from(prog.uses_stmt_journal));
    if let Some(sp_slot) = ctx.stmt_tx.stmt_sp_slot {
        if prog.uses_stmt_journal {
            sqliters_types::dtrace!("TR|stmt|journal|open");
            // OP_Transaction: "rc = sqlite3VtabSavepoint(db, SAVEPOINT_BEGIN, p->iStatement-1)"
            if ctx.db_set.vtab_registry().has_any_in_tx() {
                crate::engine::vtab_savepoint_before_stmt(ctx, sp_slot)?;
            }
        } else {
            ctx.stmt_tx.stmt_sp_slot = None;
            ctx.stmt_tx.no_stmt_journal = true;
        }
    }
    let mut vm = Vdbe::new(Arc::clone(prog));
    vm.rewind();
    for (i, v) in params.iter().enumerate() {
        vm.bind(i + 1, v);
        vm.a_var[i].change_encoding(ctx.encoding() as u8);
        vm.a_var[i].enc = ctx.encoding() as u8;
    }
    vm.trace_sql(sql);
    RUNS.with(|r| r.set(r.get() + 1));
    let encoding = ctx.encoding();
    ctx.stmt_tx.exec_depth += 1;
    // PRAGMA max_page_count: the pagers refuse a page past the limit when it is fetched
    // (getPageNormal), for every write of the statement
    let mx: Vec<(usize, u32)> = ctx
        .db_set
        .slots()
        .iter()
        .enumerate()
        .filter(|(_, s)| s.btree.is_some())
        .map(|(i, s)| (i, s.max_page_count))
        .collect();
    for &(i, m) in &mx {
        if let Some(bt) = ctx.db_set.slot_mut(DatabaseId(i)).and_then(|s| s.btree.as_mut()) {
            bt.pager_mut().set_mx_pgno(m);
        }
    }
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let res = {
        let mut host = WriteHost { ctx, encoding, control: false, sp_pos: 0, read_stmt: false };
        let r = loop {
            match vm.exec(&mut host) {
                Ok(true) => rows.push(vm.result_row().iter().map(Mem::to_value).collect()),
                Ok(false) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        if r.is_err() && vm.not_halted {
            // (the engine's write statement does not repeat a busy OP_Transaction: it ends here)
            vm.halt(&mut host);
        }
        let steps = vm.n_vm_step;
        host.ctx.progress.add_ops(steps);
        // sqlite3VdbeHalt: "if( p->rc==SQLITE_OK || (p->errorAction==OE_Fail && !isSpecialError) )
        //   (void)sqlite3VdbeCheckFkImmediate(p);" (rc = SQLITE_CONSTRAINT_FOREIGNKEY, OE_Abort)
        let r = match r {
            Ok(()) if vm.n_fk_constraint != 0 => Err(sqliters_vdbe::vdbe::fk_error()),
            Err(e)
                if vm.n_fk_constraint != 0
                    && e.conflict_effect == sqliters_types::ConflictResolutionEffect::Fail
                    && !matches!(e.code, ErrorCode::NoMem | ErrorCode::IoErr | ErrorCode::Interrupt | ErrorCode::Full) =>
            {
                Err(sqliters_vdbe::vdbe::fk_error())
            }
            r => r,
        };
        if r.is_ok() {
            // sqlite3VdbeHalt: "if( p->changeCntOn ) sqlite3VdbeSetChanges(db, p->nChange)" —
            // when the statement halts: after its last row, which the caller returns later
            let n = vm.n_change;
            if rows.is_empty() {
                host.set_changes(n);
            } else {
                host.ctx.stmt_tx.pending_changes = Some(n);
            }
        } else {
            if let Some(path) = std::env::var_os("SQLITERS_VDBE_ERRLOG") {
                use std::io::Write;
                if let (Err(e), Ok(mut f)) = (&r, std::fs::OpenOptions::new().create(true).append(true).open(path)) {
                    let _ = writeln!(f, "pc={} op={} err={} :: {}", vm.pc, vm.failed_op(), e.message, sql);
                }
            }
            // sqlite3VdbeHalt: a statement stopped by OE_Fail keeps what it did
            // ("eStatementOp = SAVEPOINT_RELEASE", `sqlite3VdbeSetChanges(db, p->nChange)`);
            // one that is rolled back — the statement or the transaction — counts nothing
            // (the abort of the engine's write statement sets `changes()` to 0).
            if let Err(e) = &r
                && e.conflict_effect == sqliters_types::ConflictResolutionEffect::Fail
                && !matches!(e.code, ErrorCode::NoMem | ErrorCode::IoErr | ErrorCode::Interrupt | ErrorCode::Full)
            {
                let n = vm.n_change;
                host.set_changes(n);
            }
        }
        r
    };
    for slot in ctx.db_set.slots_mut() {
        if let Some(bt) = slot.btree.as_mut() {
            bt.pager_mut().set_mx_pgno(u32::MAX);
        }
    }
    crate::engine::c_statement_halt(ctx);
    crate::engine::note_corrupt_in_transaction(ctx, &res);
    res.map(|()| crate::engine::QueryOutput { column_names: Vec::new(), rows })
}

/// A transaction-control statement (`BEGIN`, `COMMIT`, `END`, `ROLLBACK`, `SAVEPOINT`,
/// `RELEASE`, `ROLLBACK TO`) run by the machine: C's program (`OP_Transaction`,
/// `OP_AutoCommit`, `OP_Savepoint`) decides, the engine's transaction state is what it acts
/// on. `start`: the first step (`sqlite3VdbeRewind`); otherwise `vm` repeats the instruction
/// that returned `SQLITE_BUSY`. After an error, `vm.not_halted` tells that the statement is
/// still running (C's `goto vdbe_return`).
pub(crate) fn run_control(ctx: &mut ExecContext<'_>, vm: &mut Vdbe, start: bool, sql: &str) -> Result<()> {
    if start {
        vm.rewind();
        vm.trace_sql(sql);
        RUNS.with(|r| r.set(r.get() + 1));
    }
    let encoding = ctx.encoding();
    let mut host = WriteHost { ctx, encoding, control: true, sp_pos: 0, read_stmt: false };
    let steps_before = vm.n_vm_step;
    let r = vm.exec(&mut host);
    host.ctx.progress.add_ops(vm.n_vm_step - steps_before);
    if let Err(e) = &r
        && let Some(path) = std::env::var_os("SQLITERS_VDBE_ERRLOG")
    {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "pc={} op={} err={} :: {}", vm.pc, vm.failed_op(), e.message, sql);
        }
    }
    r.map(|_| ())
}

/// `sqlite3_reset` / `sqlite3_finalize` of a transaction-control statement that stopped at
/// `SQLITE_BUSY` or at a deferred foreign key violation without halting: `sqlite3VdbeHalt`
/// with `p->rc` set. A statement that is not read-only (`BEGIN IMMEDIATE` / `EXCLUSIVE`
/// whose `OP_Transaction` was busy) is then the writer of an autocommit connection: "else{
/// sqlite3RollbackAll(db, SQLITE_OK); }".
pub(crate) fn halt_control(ctx: &mut ExecContext<'_>, vm: &mut Vdbe) {
    let encoding = ctx.encoding();
    let read_only = vm.prog.read_only;
    {
        let mut host = WriteHost { ctx, encoding, control: true, sp_pos: 0, read_stmt: false };
        vm.halt(&mut host);
    }
    let auto_commit = !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx;
    if !read_only && auto_commit && ctx.activity.other_writers == 0 && ctx.db_set.any_in_write_transaction() {
        let had_write_tx = ctx.db_set.any_in_write_transaction();
        let _ = crate::engine::rollback_all_databases(ctx, true);
        *ctx.rollback_count += 1;
        *ctx.in_user_btree_tx = false;
        ctx.deferred_fk_suspects.clear();
        ctx.publish();
        if had_write_tx {
            let _ = ctx.commit_hooks.invoke_rollback_hook();
        }
    }
}

/// `sqlite3_step` of a statement the machine runs: the next row, or `None` at the end
/// (`SQLITE_DONE`; the statement has halted). `start`: the first step of an execution
/// (`sqlite3VdbeRewind`, the parameters are bound).
pub(crate) fn step(
    inner: &mut ConnectionInner,
    shared: &crate::ConnectionShared,
    hid: crate::HandleId,
    vm: &mut Vdbe,
    params: &[Value],
    start: bool,
    sql: &str,
    scalar_generation: u64,
) -> (Result<Option<Vec<Value>>>, Option<usize>) {
    if start {
        vm.rewind();
        for (i, v) in params.iter().enumerate() {
            vm.bind(i + 1, v);
            vm.a_var[i].change_encoding(inner.effective_encoding() as u8);
            vm.a_var[i].enc = inner.effective_encoding() as u8;
        }
        vm.trace_sql(sql);
        vm.expire_seq = inner.db_set.c_expire_seq();
        RUNS.with(|r| r.set(r.get() + 1));
    } else if vm.expire_seq != inner.db_set.c_expire_seq() {
        // sqlite3ExpirePreparedStatements(db, 0) ran since the execution started
        vm.expired = true;
    }
    // 'now' is the same within one step (`p->iCurrentTime = 0` at every sqlite3VdbeExec)
    let prev_ts = inner.runtime.stmt_timestamp_ms;
    if prev_ts.is_none() {
        inner.runtime.stmt_timestamp_ms = Some(inner.db_set.main_btree().pager().vfs().current_unix_millis());
    }
    let mut pragma_schema_error_offset = None;
    let res = if vm.prog.uses_vtab() {
        // Virtual tables: their methods run SQL of their own and reach the connection through
        // the engine's statement context (`ExecContext`), as in a statement that writes.
        let summary = inner.activity.summary_excluding(hid, &inner.db_set);
        let encoding = inner.effective_encoding();
        let mut stmt_tx = crate::engine::StmtTxTracker {
            prepared_scalar_generation: Some(scalar_generation), ..Default::default()
        };
        let mut expire_self_only = false;
        let result = inner.with_exec_ctx(shared, summary, &mut stmt_tx, &mut expire_self_only, |ctx| {
            let steps_before = vm.n_vm_step;
            let mut host = WriteHost { ctx, encoding, control: false, sp_pos: 0, read_stmt: true };
            let r = vm.exec(&mut host);
            host.ctx.progress.add_ops(vm.n_vm_step - steps_before);
            r
        });
        pragma_schema_error_offset = stmt_tx.pragma_schema_error_offset;
        result
    } else {
        let steps_before = vm.n_vm_step;
        let scalar_registry = inner.functions.scalar_registry_at(scalar_generation);
        let mut host = MachineHost::new(inner);
        if let Some(registry) = &scalar_registry {
            host.functions = registry;
        }
        let r = vm.exec(&mut host);
        // (the engine's tick counter, `Connection::progress_op_count`)
        host.progress.add_ops(vm.n_vm_step - steps_before);
        r
    };
    inner.runtime.stmt_timestamp_ms = prev_ts;
    // abort_due_to_error: "if( rc==SQLITE_CORRUPT && db->autoCommit==0 ){ db->flags |=
    // SQLITE_CorruptRdOnly; }"
    if let Err(e) = &res
        && e.code == ErrorCode::Corrupt
        && e.extended_code == ErrorCode::Corrupt as i32
        && !vm.has_sort_callback_error()
        && !inner.is_autocommit()
    {
        inner.db_set.set_corrupt_rdonly(true);
    }
    let row = match res {
        Ok(true) => Ok(Some(vm.result_row().iter().map(Mem::to_value).collect())),
        Ok(false) => Ok(None),
        Err(e) => {
            if let Some(path) = std::env::var_os("SQLITERS_VDBE_ERRLOG") {
                // Port aid: where a statement run by the machine failed.
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "pc={} op={} err={} :: {}", vm.pc, vm.failed_op(), e.message, sql);
                }
            }
            Err(e)
        }
    };
    (row, pragma_schema_error_offset)
}

/// The keys (`engine::vtab_cursor_key`) of the virtual tables `vm` keeps a cursor positioned
/// on while it is suspended at a row.
pub(crate) fn positioned_vtab_keys(inner: &ConnectionInner, vm: &Vdbe) -> Vec<u64> {
    let reg = inner.db_set.vtab_registry();
    vm.positioned_vtabs()
        .iter()
        .filter_map(|vt| reg.get_connected_by_id(&vt.name, vt.id).map(|e| crate::engine::vtab_cursor_key(&e.db_name, &vt.name)))
        .collect()
}

/// `sqlite3VdbeHalt` of a statement that is reset or finalized before its end: its cursors
/// are closed.
pub(crate) fn halt(inner: &mut ConnectionInner, vm: &mut Vdbe) {
    // (closing a virtual-table cursor needs no statement context: the cursor is dropped)
    let mut host = MachineHost::new(inner);
    vm.halt(&mut host);
}
