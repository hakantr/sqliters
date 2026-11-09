#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use sqliters_syntax::ConflictAction;
use sqliters_types::{
    Collation, DatabaseId, ErrorCode, ExecutionServices, FunctionFlags, Result, RuntimeValue,
    SqliteError, TextEncoding, Value, enforce_result_subtype_contract, extended,
    strip_unsubscribed_subtypes,
};
use sqliters_vdbe::{
    VmRuntimeContext, eval_scalar_function_rt_with_strict, validate_function_call,
};

/// Thread-safe callback type for registered scalar SQL functions.
pub type ScalarFunctionCallback =
    Arc<dyn Fn(&[RuntimeValue]) -> Result<RuntimeValue> + Send + Sync>;

/// Metadata and implementation for a registered scalar SQL function.
#[derive(Clone)]
pub struct ScalarFunctionDef {
    /// Original registration name (used in diagnostic messages such as `SQLITE_STRICT_SUBTYPE`).
    pub name: String,
    /// Exact argument count (`Some(n)`), or `None` for variadic (`nArg = -1` in SQLite).
    pub arity: Option<usize>,
    /// Function property flags (`DETERMINISTIC`, `SUBTYPE`, `RESULT_SUBTYPE`, etc.).
    pub flags: FunctionFlags,
    /// Scalar callback implementation.
    pub callback: ScalarFunctionCallback,
}

impl std::fmt::Debug for ScalarFunctionDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScalarFunctionDef")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .field("flags", &self.flags)
            .finish()
    }
}

/// Connection-level registry for custom and extension scalar SQL functions.
#[derive(Debug, Default, Clone)]
pub struct FunctionRegistry {
    scalars: BTreeMap<String, Vec<ScalarFunctionDef>>,
}

impl FunctionRegistry {
    /// Creates an empty function registry.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or replaces a scalar function (`arity: Some(n)` for fixed arity, `None` for variadic).
    pub fn register_scalar<F>(
        &mut self,
        name: impl Into<String>,
        arity: Option<usize>,
        flags: FunctionFlags,
        callback: F,
    ) where
        F: Fn(&[RuntimeValue]) -> Result<RuntimeValue> + Send + Sync + 'static,
    {
        let name = name.into();
        let key = name.to_ascii_uppercase();
        let def = ScalarFunctionDef {
            name,
            arity,
            flags,
            callback: Arc::new(callback),
        };
        let overloads = self.scalars.entry(key).or_default();
        if let Some(existing) = overloads.iter_mut().find(|d| d.arity == arity) {
            *existing = def;
        } else {
            overloads.push(def);
        }
    }

    /// Removes a registered scalar function overload, returning `true` if one was removed.
    pub fn unregister_scalar(&mut self, name: &str, arity: Option<usize>) -> bool {
        let key = name.to_ascii_uppercase();
        let Some(overloads) = self.scalars.get_mut(&key) else {
            return false;
        };
        let before = overloads.len();
        overloads.retain(|d| d.arity != arity);
        let removed = overloads.len() != before;
        if overloads.is_empty() {
            self.scalars.remove(&key);
        }
        removed
    }

    /// Returns `true` if any scalar overload with `name` (ASCII case-insensitive) is registered.
    #[inline]
    pub fn has_scalar_name(&self, name: &str) -> bool {
        self.scalars.contains_key(&name.to_ascii_uppercase())
    }

    /// Finds the best matching registered scalar overload for `(name, argc)` (exact arity preferred
    /// over variadic `None`, matching `sqlite3FindFunction`).
    pub fn find_scalar(&self, name: &str, argc: usize) -> Option<&ScalarFunctionDef> {
        let overloads = self.scalars.get(&name.to_ascii_uppercase())?;
        overloads
            .iter()
            .find(|d| d.arity == Some(argc))
            .or_else(|| overloads.iter().find(|d| d.arity.is_none()))
    }

    /// Validates a SQL function call at `prepare` time against registered custom scalar functions
    /// first, then built-in scalar and aggregate functions (`sqlite3FindFunction` overload order).
    pub fn validate_call(
        &self,
        name: &str,
        star_args: bool,
        arg_count: usize,
        distinct: bool,
        has_filter: bool,
        allow_aggregates: bool,
    ) -> Result<()> {
        if !star_args && self.find_scalar(name, arg_count).is_some() {
            if distinct || has_filter {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{name}() is not an aggregate function"),
                ));
            }
            return Ok(());
        }
        match validate_function_call(
            name,
            star_args,
            arg_count,
            distinct,
            has_filter,
            allow_aggregates,
        ) {
            Ok(()) => Ok(()),
            Err(err)
                if self.has_scalar_name(name) && err.message.starts_with("no such function:") =>
            {
                if distinct || has_filter {
                    Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("{name}() is not an aggregate function"),
                    ))
                } else {
                    Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("wrong number of arguments to function {name}()"),
                    ))
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Invokes a scalar function (`name`, `args`) enforcing `FunctionFlags::SUBTYPE` argument
    /// stripping and `FunctionFlags::RESULT_SUBTYPE` strict checking identically for custom and
    /// built-in functions.
    pub fn call_scalar(
        &self,
        name: &str,
        args: &[RuntimeValue],
        runtime_ctx: &VmRuntimeContext,
        strict_subtype: bool,
    ) -> Result<RuntimeValue> {
        if let Some(def) = self.find_scalar(name, args.len()) {
            let callback = Arc::clone(&def.callback);
            let flags = def.flags;
            let reg_name = def.name.clone();
            let mut scratch = Vec::new();
            let filtered = strip_unsubscribed_subtypes(flags, args, &mut scratch);
            let result = callback(filtered)?;
            return enforce_result_subtype_contract(&reg_name, flags, strict_subtype, result);
        }
        match eval_scalar_function_rt_with_strict(name, args, runtime_ctx, strict_subtype) {
            Ok(v) => Ok(v),
            Err(err)
                if self.has_scalar_name(name) && err.message.starts_with("no such function:") =>
            {
                Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ))
            }
            Err(err) => Err(err),
        }
    }
}

/// Thread-safe callback type for registered custom collations.
pub type CollationCallback = Arc<dyn Fn(&str, &str) -> Ordering + Send + Sync>;

/// Connection-level registry for built-in and custom collation sequences.
#[derive(Default, Clone)]
pub struct CollationRegistry {
    custom: BTreeMap<String, CollationCallback>,
}

impl std::fmt::Debug for CollationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollationRegistry")
            .field("custom_names", &self.custom.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl CollationRegistry {
    /// Creates a collation registry with built-in `BINARY`, `NOCASE`, and `RTRIM` support.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or replaces a custom collation sequence (ASCII case-insensitive name).
    pub fn register_collation<F>(&mut self, name: impl Into<String>, callback: F)
    where
        F: Fn(&str, &str) -> Ordering + Send + Sync + 'static,
    {
        self.custom
            .insert(name.into().to_ascii_uppercase(), Arc::new(callback));
    }

    /// Returns `true` if `name` resolves to either a registered custom collation or a built-in
    /// SQLite collation (`BINARY`, `NOCASE`, `RTRIM`).
    #[inline]
    pub fn has_collation(&self, name: &str) -> bool {
        self.custom.contains_key(&name.to_ascii_uppercase()) || Collation::from_name(name).is_some()
    }

    /// Compares two UTF-8 strings under `name`, returning an error if the collation does not exist.
    pub fn compare(
        &self,
        name: &str,
        left: &str,
        right: &str,
        encoding: TextEncoding,
    ) -> Result<Ordering> {
        if let Some(cb) = self.custom.get(&name.to_ascii_uppercase()).cloned() {
            return Ok(cb(left, right));
        }
        let Some(coll) = Collation::from_name(name) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {name}"),
            ));
        };
        Ok(Value::Text(left.to_owned()).cmp_sqlite_with_encoding(
            &Value::Text(right.to_owned()),
            coll,
            encoding,
        ))
    }
}

/// Callback invoked periodically by [`ProgressTracker::tick`]. Returning `true` interrupts
/// execution with `ErrorCode::Interrupt` (`sqlite3_progress_handler`).
pub type ProgressCallback = Box<dyn FnMut() -> bool + Send>;

struct ProgressHandlerState {
    interval: u32,
    since_last: u32,
    callback: ProgressCallback,
}

/// Tracks logical execution steps across both `VdbeVm` opcode execution and relational interpreter
/// loops, optionally invoking a progress handler callback every `interval` steps.
#[derive(Default)]
pub struct ProgressTracker {
    op_count: u64,
    handler: Option<ProgressHandlerState>,
}

impl std::fmt::Debug for ProgressTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressTracker")
            .field("op_count", &self.op_count)
            .field("interval", &self.handler.as_ref().map(|h| h.interval))
            .finish()
    }
}

impl ProgressTracker {
    /// Creates an idle progress tracker with zero accumulated ticks.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures or clears the progress handler (`interval == 0` or `callback.is_none()` disables).
    pub fn set_handler(&mut self, interval: u32, callback: Option<ProgressCallback>) {
        if interval == 0 {
            self.handler = None;
            return;
        }
        self.handler = callback.map(|cb| ProgressHandlerState {
            interval,
            since_last: 0,
            callback: cb,
        });
    }

    /// Returns the cumulative count of logical progress ticks recorded on this tracker.
    #[inline]
    pub fn op_count(&self) -> u64 {
        self.op_count
    }

    /// Resets the cumulative logical progress tick counter to `0`.
    #[inline]
    pub fn reset_op_count(&mut self) {
        self.op_count = 0;
        if let Some(h) = &mut self.handler {
            h.since_last = 0;
        }
    }

    /// Records one logical progress step and invokes any active progress handler when `interval`
    /// steps have elapsed.
    #[inline]
    pub fn tick(&mut self) -> Result<()> {
        self.op_count = self.op_count.wrapping_add(1);
        if let Some(h) = &mut self.handler {
            h.since_last = h.since_last.saturating_add(1);
            if h.since_last >= h.interval {
                h.since_last = 0;
                if (h.callback)() {
                    return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
                }
            }
        }
        Ok(())
    }
}

/// Callback invoked by `vdbeCommit` (`sqlite3_commit_hook`) immediately before two-phase commit.
/// Returning `true` vetoes the commit and triggers full multi-DB rollback (`SQLITE_CONSTRAINT_COMMITHOOK`).
pub type CommitHookCallback = Box<dyn FnMut() -> bool + Send>;

/// Connection-level `vdbeCommit` hook state (`sqlite3_commit_hook` + Phase 7 virtual table stubs).
#[derive(Default)]
pub struct CommitCoordinatorHooks {
    commit_hook: Option<CommitHookCallback>,
}

impl std::fmt::Debug for CommitCoordinatorHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitCoordinatorHooks")
            .field("has_commit_hook", &self.commit_hook.is_some())
            .finish()
    }
}

impl CommitCoordinatorHooks {
    /// Sets or clears the connection commit hook (`sqlite3_commit_hook`).
    pub fn set_commit_hook(&mut self, hook: Option<CommitHookCallback>) {
        self.commit_hook = hook;
    }

    /// Step 2 of `vdbeCommit`: runs `xSync` across active virtual tables before `nTrans` counting
    /// and exclusive lock acquisition (Phase 2 no-op; Phase 7 virtual tables plug in here).
    #[inline]
    pub fn vtab_sync_all(&mut self) -> Result<()> {
        Ok(())
    }

    /// Step 4 of `vdbeCommit`: invokes any registered `sqlite3_commit_hook` when `need_xcommit` is
    /// true; returning `true` aborts commit with `SQLITE_CONSTRAINT_COMMITHOOK`.
    pub fn invoke_commit_hook(&mut self) -> Result<()> {
        if let Some(ref mut cb) = self.commit_hook
            && cb()
        {
            return Err(SqliteError::with_extended(
                extended::SQLITE_CONSTRAINT_COMMITHOOK,
                "commit hook vetoed commit",
            ));
        }
        Ok(())
    }

    /// Step 6 of `vdbeCommit`: runs `xCommit` across active virtual tables after B-tree commit
    /// completes (Phase 2 no-op; Phase 7 virtual tables plug in here).
    #[inline]
    pub fn vtab_commit_all(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Concrete [`ExecutionServices`] adapter in `sqliters-core` borrowing disjoint connection service
/// fields so storage (`&mut Btree` / `BtreeAccess`) can be mutably borrowed simultaneously without
/// aliasing `ConnectionInner`.
pub struct ExecutionContext<'a> {
    pub interrupt_flag: &'a AtomicBool,
    pub runtime: &'a VmRuntimeContext,
    pub encoding: TextEncoding,
    pub functions: &'a FunctionRegistry,
    pub collations: &'a CollationRegistry,
    pub progress: &'a mut ProgressTracker,
    pub strict_subtype: bool,
}

impl ExecutionContext<'_> {
    /// Phase 2 `vdbeCommit` Step 2 (`sqlite3VtabSync`).
    #[inline]
    pub fn vtab_sync_all(&mut self, hooks: &mut CommitCoordinatorHooks) -> Result<()> {
        hooks.vtab_sync_all()
    }

    /// Phase 2 `vdbeCommit` Step 4 (`xCommitCallback`).
    #[inline]
    pub fn invoke_commit_hook(&mut self, hooks: &mut CommitCoordinatorHooks) -> Result<()> {
        hooks.invoke_commit_hook()
    }

    /// Phase 2 `vdbeCommit` Step 6 (`sqlite3VtabCommit`).
    #[inline]
    pub fn vtab_commit_all(&mut self, hooks: &mut CommitCoordinatorHooks) {
        let _ = hooks.vtab_commit_all();
    }
}

impl ExecutionServices for ExecutionContext<'_> {
    #[inline]
    fn check_interrupted(&mut self) -> Result<()> {
        if self.interrupt_flag.load(AtomicOrdering::Relaxed) {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    #[inline]
    fn tick_progress(&mut self) -> Result<()> {
        self.progress.tick()
    }

    fn call_scalar(&mut self, name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue> {
        self.functions
            .call_scalar(name, args, self.runtime, self.strict_subtype)
    }

    fn compare_collated(
        &mut self,
        collation_name: &str,
        left: &str,
        right: &str,
    ) -> Result<Ordering> {
        self.collations
            .compare(collation_name, left, right, self.encoding)
    }

    #[inline]
    fn strict_subtype(&self) -> bool {
        self.strict_subtype
    }
}

/// Logical origin of a single physical row mutation passing through the shared mutation pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOrigin {
    TopLevelInsert,
    TopLevelUpdate,
    TopLevelDelete,
    UpsertDoUpdate,
    ReplaceConflictDelete,
    FkCascadeDelete,
    FkActionUpdate,
}

/// Snapshot of a table row's `rowid` (`0` for `WITHOUT ROWID` tables) and logical column values.
#[derive(Debug, Clone, PartialEq)]
pub struct MutationRow {
    pub rowid: i64,
    pub values: Vec<Value>,
}

/// Structured row-mutation descriptor routed through the shared DML / `UPSERT` / `OR REPLACE` /
/// foreign-key mutation pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct MutationContext {
    pub db_id: DatabaseId,
    pub table_name: String,
    pub origin: MutationOrigin,
    pub old_row: Option<MutationRow>,
    pub new_row: Option<MutationRow>,
    pub excluded_row: Option<MutationRow>,
    pub effective_conflict: ConflictAction,
    pub emit_update_hook: bool,
    pub invalidate_blobs: bool,
}

/// Connection-level row-mutation event dispatcher (Phase 3 metadata core, Phase 4 trigger hookup,
/// Phase 6 `update_hook` and incremental `Blob` invalidation).
#[derive(Debug, Default)]
pub struct MutationPipeline {
    trace_enabled: bool,
    events: Vec<MutationContext>,
}

impl MutationPipeline {
    /// Creates an idle mutation pipeline with tracing disabled.
    #[inline]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables or disables recording of [`MutationContext`] descriptors for inspection/testing.
    #[inline]
    pub fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace_enabled = enabled;
        if !enabled {
            self.events.clear();
        }
    }

    /// Returns `true` if mutation trace recording is currently active.
    #[inline]
    pub fn is_trace_enabled(&self) -> bool {
        self.trace_enabled
    }

    /// Drains and returns all recorded [`MutationContext`] events since the last call.
    #[inline]
    pub fn take_events(&mut self) -> Vec<MutationContext> {
        std::mem::take(&mut self.events)
    }

    /// Current number of buffered [`MutationContext`] trace events.
    #[inline]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Returns `true` when no [`MutationContext`] trace events are buffered.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Truncates uncommitted trace events on statement/savepoint/transaction rollback.
    #[inline]
    pub fn truncate(&mut self, len: usize) {
        self.events.truncate(len);
    }

    /// Clears all buffered trace events (e.g., on full transaction rollback when opened inside tx).
    #[inline]
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Dispatches a completed physical row mutation through the shared event boundary.
    #[inline]
    pub fn dispatch(&mut self, ctx: MutationContext) {
        if self.trace_enabled {
            self.events.push(ctx);
        }
    }
}
