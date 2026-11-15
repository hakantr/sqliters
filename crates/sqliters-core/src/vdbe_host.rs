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
use sqliters_pager::{LockingMode, PagerOpenOptions, TransactionState};
use sqliters_types::DatabaseKind;
use sqliters_syntax::Stmt;
use sqliters_types::{
    AggregateStepId, DatabaseId, ErrorCode, ExecutionServices, Result, RuntimeValue, SqliteError, TextEncoding, Value,
};
use sqliters_vdbe::vdbe::{Vdbe, VdbeHost, unsupported_reason};
use sqliters_vdbe::vdbeaux::{COLNAME_NAME, FuncDef, Program};
use sqliters_vdbe::vdbemem::{CollRt, Mem};

use crate::ConnectionInner;
use crate::engine::{ExecContext, QueryOutput};
use crate::execution_context::ExecutionContext;

/// True if the environment variable `SQLITERS_VDBE_EXEC` is set: statements the generator and
/// the machine support completely are executed by the machine.
pub(crate) fn exec_switch_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SQLITERS_VDBE_EXEC").is_some()) || FORCE.with(std::cell::Cell::get)
}

thread_local! {
    /// The switch for the tests of this crate (the environment variable is read once per
    /// process): on for the statements the calling thread prepares.
    static FORCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Number of statements the machine has run on this thread.
    static RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Turns the switch on for the calling thread until dropped (tests).
#[cfg(test)]
pub(crate) struct ForceMachine;

#[cfg(test)]
impl ForceMachine {
    pub(crate) fn on() -> ForceMachine {
        FORCE.with(|f| f.set(true));
        ForceMachine
    }
}

#[cfg(test)]
impl Drop for ForceMachine {
    fn drop(&mut self) {
        FORCE.with(|f| f.set(false));
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
/// engine's execution path (always, when the switch is off).
pub(crate) fn machine_program(
    inner: &ConnectionInner,
    stmt: Option<&Stmt>,
    raw_sql: &str,
    n_param: usize,
) -> Option<Arc<Program>> {
    if !exec_switch_on() {
        return None;
    }
    if !matches!(stmt, Some(Stmt::Select(_))) {
        return None;
    }
    if inner.authorizer.is_some() {
        // C's generator asks the authorizer while it resolves names (sqlite3AuthRead); the port
        // of the generator has no authorizer hook yet.
        trace("fallback: authorizer", raw_sql);
        return None;
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
        trace("fallback: schema with an application-defined collation", raw_sql);
        return None;
    }
    if inner.effective_encoding() != TextEncoding::Utf8 {
        trace("fallback: UTF-16 database", raw_sql);
        return None;
    }
    match crate::cgen::c_prepare(inner, raw_sql).0 {
        PrepareResult::Program(p) => {
            if p.explain != 0 {
                return None;
            }
            if let Some(why) = unsupported_reason(&p) {
                trace(&format!("fallback: {why}"), raw_sql);
                return None;
            }
            if p.n_var as usize != n_param {
                trace("fallback: parameter numbering", raw_sql);
                return None;
            }
            trace("machine", raw_sql);
            Some(Arc::new(*p))
        }
        PrepareResult::Error { message, .. } => {
            trace(&format!("fallback: generator error: {message}"), raw_sql);
            None
        }
        PrepareResult::Unsupported(why) => {
            trace(&format!("fallback: generator: {why}"), raw_sql);
            None
        }
    }
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

struct MachineHost<'a, 'b> {
    ctx: &'a mut ExecContext<'b>,
}

impl MachineHost<'_, '_> {
    fn svc(&mut self) -> ExecutionContext<'_> {
        let enc = self.ctx.encoding();
        ExecutionContext {
            interrupt_flag: self.ctx.interrupt_flag,
            runtime: self.ctx.runtime,
            encoding: enc,
            functions: self.ctx.functions,
            collations: self.ctx.collations,
            progress: self.ctx.progress,
            strict_subtype: self.ctx.strict_subtype,
        }
    }
}

impl VdbeHost for MachineHost<'_, '_> {
    fn btree(&mut self, i_db: usize) -> Option<&mut Btree> {
        self.ctx.db_set.slot_mut(DatabaseId(i_db)).and_then(|s| s.btree.as_mut())
    }

    fn begin_read_trans(&mut self, i_db: usize) -> Result<(u32, i32)> {
        let ahead = self.ctx.db_set.c_cookie_ahead();
        let Some(slot) = self.ctx.db_set.slot_mut(DatabaseId(i_db)) else {
            return Err(SqliteError::new(ErrorCode::Error, format!("unknown database index {i_db}")));
        };
        if let Some(bt) = slot.btree.as_mut()
            && bt.transaction_state() == TransactionState::Open
        {
            bt.begin_read()?;
            self.ctx.stmt_tx.started_read_mask |= 1u16.checked_shl(i_db as u32).unwrap_or(0);
        }
        self.ctx.stmt_tx.read_db_mask |= 1u16.checked_shl(i_db as u32).unwrap_or(0);
        let slot = self.ctx.db_set.slot(DatabaseId(i_db)).ok_or_else(|| SqliteError::misuse("database detached"))?;
        Ok((slot.schema.schema_cookie + u32::from(ahead == Some(i_db)), slot.c_generation))
    }

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
        self.ctx.collations.get_custom(name).map(CollRt::from_custom)
    }

    fn call_function(&mut self, func: &FuncDef, args: &[RuntimeValue]) -> Result<RuntimeValue> {
        self.ctx.call_scalar_rt(&func.name, args)
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
            lock_geometry: Some(self.ctx.db_set.lock_geometry()),
            db_kind: Some(DatabaseKind::PrivateTemporary),
            temp_in_memory: true,
            ..PagerOpenOptions::default()
        };
        let mut bt = Btree::open_private_temporary(Arc::clone(self.ctx.db_set.disk_vfs()), opts)?;
        let _ = bt.pager_mut().set_locking_mode(LockingMode::Exclusive);
        bt.begin_write()?;
        Ok(bt)
    }

    fn limit_length(&self) -> i32 {
        // SQLITE_MAX_LENGTH (the engine has no sqlite3_limit())
        1_000_000_000
    }

    fn check_interrupt(&mut self) -> Result<()> {
        self.ctx.step_tick()
    }

    fn sorter_config(&mut self) -> (i64, i64, bool) {
        let bt = self.ctx.db_set.main_btree();
        (i64::from(bt.pager().page_size()), i64::from(bt.cache_size()), self.ctx.db_set.temp_store().is_memory())
    }
}

/// Runs `prog` to completion (`sqlite3_step` until `SQLITE_DONE`): the rows, and the error
/// that ended the statement after it had produced rows, if any.
pub(crate) fn run(
    ctx: &mut ExecContext<'_>,
    prog: &Arc<Program>,
    params: &[Value],
    sql: &str,
) -> Result<(QueryOutput, Option<SqliteError>)> {
    let mut vm = Vdbe::new(Arc::clone(prog));
    vm.trace_sql(sql);
    RUNS.with(|r| r.set(r.get() + 1));
    for (i, v) in params.iter().enumerate() {
        vm.bind(i + 1, v);
    }
    let mut host = MachineHost { ctx };
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut trailing = None;
    loop {
        match vm.exec(&mut host) {
            Ok(true) => rows.push(vm.result_row().iter().map(Mem::to_value).collect()),
            Ok(false) => break,
            Err(e) => {
                if let Some(path) = std::env::var_os("SQLITERS_VDBE_ERRLOG") {
                    // Port aid: where a statement run by the machine failed.
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        let pages: Vec<u32> = host.ctx.db_set.slots().iter().map(|s| s.btree.as_ref().map_or(0, |b| b.pager().page_count())).collect();
                        let _ = writeln!(f, "pc={} op={} pages={:?} err={} :: {}", vm.pc, vm.failed_op(), pages, e.message, sql);
                    }
                }
                if rows.is_empty() {
                    return Err(e);
                }
                trailing = Some(e);
                break;
            }
        }
    }
    Ok((QueryOutput { column_names: column_names(prog).unwrap_or_default(), rows }, trailing))
}
