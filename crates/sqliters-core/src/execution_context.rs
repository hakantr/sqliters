#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use sqliters_syntax::ConflictAction;
use sqliters_types::{
    AggregateStepId, Collation, DatabaseId, ErrorCode, ExecutionServices, FunctionFlags, Result,
    RuntimeValue, SqliteError, TextEncoding, Value, enforce_result_subtype_contract, extended,
    strip_unsubscribed_subtypes,
};
use sqliters_vdbe::{
    AggregateAccumulator, AggregateKind, VmRuntimeContext, eval_scalar_function_rt_with_strict,
    validate_function_call,
};

/// Thread-safe callback type for registered scalar SQL functions.
pub type ScalarFunctionCallback =
    Arc<dyn Fn(&[RuntimeValue]) -> Result<RuntimeValue> + Send + Sync>;

/// Per-instance stateful accumulator trait for registered aggregate and window SQL functions
/// (`xStep`, `xFinal`, optional `xValue`, and optional `xInverse` in `sqlite/src/vdbeapi.c`).
pub trait WindowAggregateInstance: Send {
    /// Feeds one row of arguments into this accumulator (`xStep`).
    fn step(&mut self, args: &[RuntimeValue]) -> Result<()>;

    /// Reads the current window value without consuming accumulator state (`xValue`).
    fn value(&self) -> Result<RuntimeValue> {
        Err(SqliteError::misuse("aggregate does not implement xValue"))
    }

    /// Removes a departing window-frame row from this accumulator (`xInverse`).
    fn inverse(&mut self, _args: &[RuntimeValue]) -> Result<()> {
        Err(SqliteError::misuse("aggregate does not implement xInverse"))
    }

    /// Finalizes and consumes this accumulator (`xFinal`).
    fn finalize(&mut self) -> Result<RuntimeValue>;
}

/// Thread-safe factory producing fresh [`WindowAggregateInstance`] states.
pub type WindowAggregateFactory = Arc<dyn Fn() -> Box<dyn WindowAggregateInstance> + Send + Sync>;

/// Metadata and factory for a registered aggregate or window SQL function (`sqlite3_create_window_function`).
#[derive(Clone)]
pub struct WindowFunctionDef {
    /// Original registration name (used in diagnostic messages such as `SQLITE_STRICT_SUBTYPE`).
    pub name: String,
    /// Exact argument count (`Some(n)`), or `None` for variadic (`nArg = -1` in SQLite).
    pub arity: Option<usize>,
    /// Function property flags (`DETERMINISTIC`, `SUBTYPE`, `RESULT_SUBTYPE`, etc.).
    pub flags: FunctionFlags,
    /// True if `xValue` is provided (permitted in `OVER (...)` window invocations).
    pub supports_window: bool,
    /// True if `xInverse` is provided (supports O(1) sliding window frames).
    pub supports_inverse: bool,
    /// Factory creating fresh accumulator state per group/partition.
    pub factory: WindowAggregateFactory,
}

impl std::fmt::Debug for WindowFunctionDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowFunctionDef")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .field("flags", &self.flags)
            .field("supports_window", &self.supports_window)
            .field("supports_inverse", &self.supports_inverse)
            .finish()
    }
}

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

/// Connection-level registry for custom and extension scalar, aggregate, and window SQL functions.
#[derive(Debug, Default, Clone)]
pub struct FunctionRegistry {
    scalars: BTreeMap<String, Vec<ScalarFunctionDef>>,
    aggregates: BTreeMap<String, Vec<WindowFunctionDef>>,
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

    /// Registers or replaces an aggregate or window function (`sqlite3_create_function` / `sqlite3_create_window_function`).
    pub fn register_window_aggregate<F>(
        &mut self,
        name: impl Into<String>,
        arity: Option<usize>,
        flags: FunctionFlags,
        supports_window: bool,
        supports_inverse: bool,
        factory: F,
    ) where
        F: Fn() -> Box<dyn WindowAggregateInstance> + Send + Sync + 'static,
    {
        let name = name.into();
        let key = name.to_ascii_uppercase();
        let def = WindowFunctionDef {
            name,
            arity,
            flags,
            supports_window,
            supports_inverse: supports_window && supports_inverse,
            factory: Arc::new(factory),
        };
        let overloads = self.aggregates.entry(key).or_default();
        if let Some(existing) = overloads.iter_mut().find(|d| d.arity == arity) {
            *existing = def;
        } else {
            overloads.push(def);
        }
    }

    /// Returns `true` if any custom aggregate/window overload with `name` (ASCII case-insensitive) is registered.
    #[inline]
    pub fn has_aggregate_name(&self, name: &str) -> bool {
        self.aggregates.contains_key(&name.to_ascii_uppercase())
    }

    /// Finds the best matching registered aggregate/window overload for `(name, argc)`.
    pub fn find_aggregate(&self, name: &str, argc: usize) -> Option<&WindowFunctionDef> {
        let overloads = self.aggregates.get(&name.to_ascii_uppercase())?;
        overloads
            .iter()
            .find(|d| d.arity == Some(argc))
            .or_else(|| overloads.iter().find(|d| d.arity.is_none()))
    }

    /// Validates a SQL function call at `prepare` time against registered custom scalar and
    /// aggregate functions first, then built-in scalar and aggregate functions (`sqlite3FindFunction`).
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
            if has_filter {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("FILTER may not be used with non-aggregate {name}()"),
                ));
            }
            if distinct {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{name}() is not an aggregate function"),
                ));
            }
            return Ok(());
        }
        let eff_argc = if star_args { 0 } else { arg_count };
        if let Some(_agg) = self.find_aggregate(name, eff_argc) {
            if star_args && eff_argc != 0 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ));
            }
            if distinct && (star_args || arg_count != 1) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "DISTINCT aggregates must have exactly one argument",
                ));
            }
            if !allow_aggregates {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("misuse of aggregate function {name}()"),
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
                if (self.has_scalar_name(name) || self.has_aggregate_name(name))
                    && err.message.starts_with("no such function:") =>
            {
                Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("wrong number of arguments to function {name}()"),
                ))
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

enum OpenAccumulatorSlot {
    Builtin {
        name: String,
        flags: FunctionFlags,
        collation_name: String,
        acc: AggregateAccumulator,
    },
    Custom {
        name: String,
        flags: FunctionFlags,
        supports_window: bool,
        supports_inverse: bool,
        distinct_seen: Option<(String, Vec<Value>)>,
        instance: Box<dyn WindowAggregateInstance>,
    },
}

/// Tracks logical execution steps across both `VdbeVm` opcode execution and relational interpreter
/// loops, optionally invoking a progress handler callback every `interval` steps, and holds open
/// stateful aggregate/window accumulators for [`ExecutionContext`].
#[derive(Default)]
pub struct ProgressTracker {
    op_count: u64,
    handler: Option<ProgressHandlerState>,
    open_accumulators: Vec<Option<OpenAccumulatorSlot>>,
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

    /// Resets the cumulative logical progress tick counter to `0` and clears any stale open
    /// accumulators from a prior aborted execution.
    #[inline]
    pub fn reset_op_count(&mut self) {
        self.op_count = 0;
        self.open_accumulators.clear();
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

    fn agg_open(&mut self, name: &str, arity: usize) -> Result<AggregateStepId> {
        self.agg_open_collated(name, arity, false, "BINARY")
    }

    fn agg_open_collated(
        &mut self,
        name: &str,
        arity: usize,
        distinct: bool,
        collation_name: &str,
    ) -> Result<AggregateStepId> {
        if !self.collations.has_collation(collation_name) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {collation_name}"),
            ));
        }

        let slot_val = if let Some(def) = self.functions.find_aggregate(name, arity) {
            OpenAccumulatorSlot::Custom {
                name: def.name.clone(),
                flags: def.flags,
                supports_window: def.supports_window,
                supports_inverse: !distinct && def.supports_inverse,
                distinct_seen: distinct.then(|| (collation_name.to_owned(), Vec::new())),
                instance: (def.factory)(),
            }
        } else {
            let upper = name.to_ascii_uppercase();
            let kind = match (upper.as_str(), arity) {
                ("COUNT", 0) => AggregateKind::CountStar,
                ("COUNT", 1) => AggregateKind::Count,
                ("SUM", 1) => AggregateKind::Sum,
                ("TOTAL", 1) => AggregateKind::Total,
                ("AVG", 1) => AggregateKind::Avg,
                ("MIN", 1) => AggregateKind::Min,
                ("MAX", 1) => AggregateKind::Max,
                ("GROUP_CONCAT", 1 | 2) | ("STRING_AGG", 2) => AggregateKind::GroupConcat,
                (
                    "COUNT" | "SUM" | "TOTAL" | "AVG" | "MIN" | "MAX" | "GROUP_CONCAT"
                    | "STRING_AGG",
                    _,
                ) => {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("wrong number of arguments to function {name}()"),
                    ));
                }
                _ if self.functions.has_aggregate_name(name) => {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("wrong number of arguments to function {name}()"),
                    ));
                }
                _ => {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such function: {name}"),
                    ));
                }
            };
            let builtin_coll = Collation::from_name(collation_name).unwrap_or(Collation::Binary);
            OpenAccumulatorSlot::Builtin {
                name: name.to_ascii_lowercase(),
                flags: FunctionFlags::DETERMINISTIC | FunctionFlags::INNOCUOUS,
                collation_name: collation_name.to_owned(),
                acc: AggregateAccumulator::new_with_collation(kind, distinct, builtin_coll)
                    .with_encoding(self.encoding),
            }
        };

        let slots = &mut self.progress.open_accumulators;
        if let Some((idx, empty)) = slots.iter_mut().enumerate().find(|(_, s)| s.is_none()) {
            *empty = Some(slot_val);
            Ok(AggregateStepId(idx))
        } else {
            let idx = slots.len();
            slots.push(Some(slot_val));
            Ok(AggregateStepId(idx))
        }
    }

    fn agg_supports_inverse(&self, id: AggregateStepId) -> bool {
        match self
            .progress
            .open_accumulators
            .get(id.0)
            .and_then(Option::as_ref)
        {
            Some(OpenAccumulatorSlot::Builtin { acc, .. }) => acc.supports_inverse(),
            Some(OpenAccumulatorSlot::Custom {
                supports_inverse, ..
            }) => *supports_inverse,
            None => false,
        }
    }

    fn agg_step(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        let collations = self.collations;
        let encoding = self.encoding;
        let Some(Some(slot)) = self.progress.open_accumulators.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate step on unopened accumulator",
            ));
        };
        match slot {
            OpenAccumulatorSlot::Builtin {
                flags,
                collation_name,
                acc,
                ..
            } => {
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
                let plain: Vec<Value> = filtered.iter().map(|rv| rv.value.clone()).collect();
                let cname = collation_name.as_str();
                acc.step_with_cmp(&plain, |a, b| match (a, b) {
                    (Value::Text(la), Value::Text(lb)) => {
                        collations.compare(cname, la, lb, encoding)
                    }
                    _ => Ok(a.cmp_sqlite_with_encoding(b, Collation::Binary, encoding)),
                })
            }
            OpenAccumulatorSlot::Custom {
                flags,
                distinct_seen,
                instance,
                ..
            } => {
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
                if let Some((cname, seen)) = distinct_seen
                    && let Some(first) = filtered.first()
                {
                    if first.value.is_null() {
                        return Ok(());
                    }
                    let dup = seen.iter().any(|prev| match (&first.value, prev) {
                        (Value::Text(la), Value::Text(lb)) => collations
                            .compare(cname, la, lb, encoding)
                            .is_ok_and(Ordering::is_eq),
                        (a, b) => a
                            .cmp_sqlite_with_encoding(b, Collation::Binary, encoding)
                            .is_eq(),
                    });
                    if dup {
                        return Ok(());
                    }
                    seen.push(first.value.clone());
                }
                instance.step(filtered)
            }
        }
    }

    fn agg_value(&mut self, id: AggregateStepId) -> Result<RuntimeValue> {
        let strict = self.strict_subtype;
        let Some(Some(slot)) = self.progress.open_accumulators.get(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate value on unopened accumulator",
            ));
        };
        match slot {
            OpenAccumulatorSlot::Builtin {
                name, flags, acc, ..
            } => {
                let rv = RuntimeValue::plain(acc.value()?);
                enforce_result_subtype_contract(name, *flags, strict, rv)
            }
            OpenAccumulatorSlot::Custom {
                name,
                flags,
                supports_window,
                instance,
                ..
            } => {
                if !*supports_window {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("{name}() may not be used as a window function"),
                    ));
                }
                let rv = instance.value()?;
                enforce_result_subtype_contract(name, *flags, strict, rv)
            }
        }
    }

    fn agg_inverse(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        let Some(Some(slot)) = self.progress.open_accumulators.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate inverse on unopened accumulator",
            ));
        };
        match slot {
            OpenAccumulatorSlot::Builtin { flags, acc, .. } => {
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
                let plain: Vec<Value> = filtered.iter().map(|rv| rv.value.clone()).collect();
                acc.inverse(&plain)
            }
            OpenAccumulatorSlot::Custom {
                flags,
                supports_inverse,
                instance,
                ..
            } => {
                if !*supports_inverse {
                    return Err(SqliteError::misuse(
                        "aggregate accumulator does not support inverse",
                    ));
                }
                let mut scratch = Vec::new();
                let filtered = strip_unsubscribed_subtypes(*flags, args, &mut scratch);
                instance.inverse(filtered)
            }
        }
    }

    fn agg_finalize(&mut self, id: AggregateStepId) -> Result<RuntimeValue> {
        let strict = self.strict_subtype;
        let Some(slot_opt) = self.progress.open_accumulators.get_mut(id.0) else {
            return Err(SqliteError::misuse(
                "aggregate finalize on unopened accumulator",
            ));
        };
        let Some(slot) = slot_opt.take() else {
            return Err(SqliteError::misuse(
                "aggregate finalize on unopened accumulator",
            ));
        };
        match slot {
            OpenAccumulatorSlot::Builtin {
                name, flags, acc, ..
            } => {
                let rv = RuntimeValue::plain(acc.finalize()?);
                enforce_result_subtype_contract(&name, flags, strict, rv)
            }
            OpenAccumulatorSlot::Custom {
                name,
                flags,
                mut instance,
                ..
            } => {
                let rv = instance.finalize()?;
                enforce_result_subtype_contract(&name, flags, strict, rv)
            }
        }
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
