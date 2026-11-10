//! Relational SQL query engine, DML/DDL executor, constraint & index manager, `VACUUM`, and `PRAGMA`s.
#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use sqliters_btree::{Btree, RootPageMove, commit_btrees_two_phase};
use sqliters_pager::{
    AutoVacuumMode, JournalMode, LockingMode, PagerOpenOptions, TransactionState,
    WalCheckpointMode, WalCheckpointResult,
};
use sqliters_syntax::{
    AlterTableAction, BinaryOp, BindParam, ColumnDef, ColumnRef, CommonTableExpr, CompoundOp,
    ConflictAction, CreateIndexStmt, CreateTableStmt, CreateTriggerStmt, CreateViewStmt,
    CurrentTimeKind, DeleteStmt, Expr, FkAction, FromClause, FunctionArgs, IdentSpanMap, IndexHint,
    IndexedColumn, InsertSource, InsertStmt, JoinClause, JoinConstraint, JoinType, LikeOp,
    LimitClause, OrderByTerm, PragmaValue, QualifiedName, ResultColumn, SelectBody, SelectCore,
    SelectStmt, SetAssignment, Stmt, TableOrSubquery, Token, TokenKind, TokenSpan, TriggerEvent,
    TriggerStep, TriggerTiming, UnaryOp, UpdateStmt, UpsertAction, apply_span_edits, dequote,
    keyword_kind, parse_one, parse_one_with_spans, remove_column_def_from_create_table_sql,
    tokenize,
};
use sqliters_types::{
    Collation, ConflictResolutionEffect, DatabaseId, DatabaseKind, ErrorCode, ExecutionServices,
    KeySortOrder, Pgno, Result, RuntimeValue, SqliteError, TextEncoding, TypeAffinity, Value,
    decode_record_with_encoding, encode_record_with_encoding, extended,
};
use sqliters_vdbe::{
    AggregateAccumulator, AggregateKind, Opcode, VmRuntimeContext, eval_cast_rt_with_encoding,
    eval_comparison_with_affinity, eval_comparison_with_encoding, eval_comparison_with_services,
    eval_unary_negate, format_utc_date, format_utc_time, format_utc_timestamp, parse_escape_char,
    sqlite_compare_affinity, sqlite_glob_match, sqlite_like_match,
};
use sqliters_vfs::{MemoryVfs, OpenFlags, OsVfs, Vfs};

use crate::compiler::{
    AccessPlan, choose_access_plan_with_schema_name, compile_for_explain_in_db_set,
    explain_query_plan_in_db_set, expr_column_label, expr_contains_aggregate,
};
use crate::db_set::{DatabaseSet, ResolvedRelation, SchemaInstanceId, TempStoreMode};
use crate::execution_context::{
    CollationRegistry, CommitCoordinatorHooks, ExecutionContext, FunctionRegistry, MutationContext,
    MutationOrigin, MutationPipeline, MutationRow, ProgressTracker,
};
use crate::schema::{
    ColumnSchema, ForeignKeySchema, IndexSchema, IndexedColSchema, Schema, TableRowKey,
    TableSchema, ViewSchema, build_column_schema_from_def, build_index_schema_from_ast,
    build_table_schema_from_ast, delete_schema_entry, fix_permanent_view_select,
    insert_schema_entry, scan_index_incremental, scan_index_incremental_with_services,
    scan_table_incremental, scan_table_incremental_with_services, update_schema_entry,
    validate_view_no_parameters,
};
use crate::trigger::{
    CreateTriggerPlan, fix_permanent_trigger, from_contains_raise,
    is_reserved_trigger_or_system_name, reject_raise_outside_trigger_expr,
    reject_raise_outside_trigger_select, replace_bind_params_with_null_in_trigger,
    resolve_and_validate_create_trigger, validate_trigger_body_dml_targets,
};

/// Materialized result set (`column_names` + `rows`) produced by executing a statement.
#[derive(Debug, Clone, Default)]
pub struct QueryOutput {
    pub column_names: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Connection-level named SQL savepoint (`sqlite/src/vdbe.c` `OP_Savepoint`).
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct SqlSavepoint {
    pub name: String,
    /// 0-based physical `Pager` subsavepoint slot index corresponding to this savepoint.
    pub subsavepoint_depth: usize,
    /// `true` when opened in autocommit mode (marking an implicit transaction savepoint).
    pub is_transaction_savepoint: bool,
    pub schemas_at_open: Vec<(SchemaInstanceId, Schema)>,
    pub schema_gen_at_open: u64,
    pub v1_expire_gen_at_open: u64,
    pub deferred_fk_suspects: Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
}

/// Snapshot of other active statements on the connection (excluding the currently executing handle).
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ActivitySummary {
    pub other_active: usize,
    pub other_readers: usize,
    pub other_writers: usize,
    pub other_read_db_mask: u16,
    pub other_write_db_mask: u16,
}

/// Per-statement tracking of `DatabaseId` slots locked or dirtied during a single execution.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct StmtTxTracker {
    /// Bitmask of `DatabaseId` slots whose `Read` transaction was started from `Open` by this statement.
    pub started_read_mask: u16,
    /// Bitmask of `DatabaseId` slots whose `Write` transaction (and optional `stmt_sp_slot`) was entered in this statement.
    pub touched_write_mask: u16,
    /// Bitmask of `DatabaseId` slots read during this statement.
    pub read_db_mask: u16,
    /// Bitmask of `DatabaseId` slots written during this statement.
    pub write_db_mask: u16,
    /// Statement-level subsavepoint slot index when `!autocommit || other_writers > 0`.
    pub stmt_sp_slot: Option<usize>,
}

/// Default WAL automatic checkpoint threshold (`SQLITE_DEFAULT_WAL_AUTOCHECKPOINT = 1000`).
pub const SQLITE_DEFAULT_WAL_AUTOCHECKPOINT: i32 = 1000;

/// Connection-level busy retry policy (`sqlite3_busy_timeout` / `sqlite3_busy_handler`, `main.c:1765-1955`).
#[derive(Default)]
pub enum BusyPolicy {
    #[default]
    None,
    TimeoutMs(u64),
    Custom(Box<dyn FnMut(u32) -> bool + Send>),
}

impl BusyPolicy {
    #[inline]
    pub(crate) fn has_handler(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn invoke_once(&mut self, count: u32, start: std::time::Instant) -> bool {
        const DELAYS_MS: [u64; 12] = [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100];
        match self {
            Self::None => false,
            Self::Custom(cb) => cb(count),
            Self::TimeoutMs(tm) => {
                let limit = *tm;
                if limit == 0 {
                    return false;
                }
                let elapsed = start.elapsed().as_millis() as u64;
                if elapsed >= limit {
                    return false;
                }
                let base_delay = DELAYS_MS.get(count as usize).copied().unwrap_or(100);
                let sleep_ms = base_delay.min(limit - elapsed).max(1);
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
                true
            }
        }
    }

    /// Retries `op(btree)` on external lock contention (`ErrorCode::Busy` excluding `SQLITE_BUSY_SNAPSHOT`),
    /// matching `btreeBeginTrans` (`btree.c:3040`).
    pub(crate) fn retry_btree<T, F>(&mut self, btree: &mut Btree, mut op: F) -> Result<T>
    where
        F: FnMut(&mut Btree) -> Result<T>,
    {
        if !self.has_handler() {
            return op(btree);
        }
        let start = std::time::Instant::now();
        let mut count = 0u32;
        loop {
            match op(btree) {
                Ok(v) => return Ok(v),
                Err(e)
                    if e.code == ErrorCode::Busy
                        && e.extended_code != extended::SQLITE_BUSY_SNAPSHOT =>
                {
                    btree.check_interrupted()?;
                    if !self.invoke_once(count, start) {
                        return Err(e);
                    }
                    count = count.saturating_add(1);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Runs `btree.wal_checkpoint`, passing `busy_handler` only when `mode > Passive` (`pager.c:7608`, `wal.c:4408`).
    pub(crate) fn checkpoint_btree(
        &mut self,
        btree: &mut Btree,
        mode: WalCheckpointMode,
    ) -> Result<WalCheckpointResult> {
        if mode > WalCheckpointMode::Passive && self.has_handler() {
            let start = std::time::Instant::now();
            let mut cb = |count: u32| self.invoke_once(count, start);
            btree.wal_checkpoint(mode, Some(&mut cb))
        } else {
            btree.wal_checkpoint(mode, None)
        }
    }
}

/// Mutable database state passed into statement execution.
pub struct ExecContext<'a> {
    pub db_set: &'a mut DatabaseSet,
    pub connection_encoding: &'a mut TextEncoding,
    pub uri_enabled: bool,
    pub is_connection_readonly: bool,
    pub runtime: &'a mut VmRuntimeContext,
    pub interrupt_flag: &'a std::sync::atomic::AtomicBool,
    pub functions: &'a FunctionRegistry,
    pub collations: &'a CollationRegistry,
    pub progress: &'a mut ProgressTracker,
    pub commit_hooks: &'a mut CommitCoordinatorHooks,
    pub mutation_pipeline: &'a mut MutationPipeline,
    pub strict_subtype: bool,
    pub explicit_tx: &'a mut bool,
    pub implicit_savepoint_tx: &'a mut bool,
    pub in_user_btree_tx: &'a mut bool,
    pub savepoints: &'a mut Vec<SqlSavepoint>,
    pub schema_generation: &'a mut u64,
    pub v1_expire_generation: &'a mut u64,
    pub expire_self_only: &'a mut bool,
    pub schema_changed_in_tx: &'a mut bool,
    pub activity: ActivitySummary,
    pub stmt_tx: &'a mut StmtTxTracker,
    pub foreign_keys: &'a mut bool,
    pub legacy_alter_table: &'a mut bool,
    pub count_changes: &'a mut bool,
    pub wal_autocheckpoint_threshold: &'a mut i32,
    pub default_locking_mode: &'a mut LockingMode,
    pub writable_schema: &'a mut bool,
    pub working_dir: &'a Option<std::path::PathBuf>,
    pub busy_policy: &'a mut BusyPolicy,
    pub rollback_count: &'a mut u64,
    pub deferred_fk_suspects: &'a mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
    pub publish_mirrors: &'a mut dyn FnMut(&VmRuntimeContext, bool, bool, u64),
}

impl ExecContext<'_> {
    #[inline]
    pub(crate) fn encoding(&self) -> TextEncoding {
        if self.db_set.main_btree().pager().page_count() > 0 {
            self.db_set.main_btree().encoding()
        } else {
            *self.connection_encoding
        }
    }

    #[inline]
    pub(crate) fn publish(&mut self) {
        (self.publish_mirrors)(
            self.runtime,
            *self.explicit_tx,
            *self.implicit_savepoint_tx,
            *self.rollback_count,
        );
    }

    #[inline]
    pub(crate) fn step_tick(&mut self) -> Result<()> {
        if self
            .interrupt_flag
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        self.progress.tick()
    }

    pub(crate) fn call_scalar_rt(
        &mut self,
        name: &str,
        args: &[RuntimeValue],
    ) -> Result<RuntimeValue> {
        let enc = self.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        svc.call_scalar(name, args)
    }

    pub(crate) fn scan_table_in_db(
        &mut self,
        db_id: DatabaseId,
        root_pgno: Pgno,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        if root_pgno == 0 {
            return Ok(Vec::new());
        }
        let Some(slot) = self.db_set.slot_mut(db_id) else {
            return Ok(Vec::new());
        };
        let Some(ref mut bt) = slot.btree else {
            return Ok(Vec::new());
        };
        let enc = bt.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        scan_table_incremental_with_services(bt, root_pgno, &mut svc)
    }

    pub(crate) fn scan_index_in_db(
        &mut self,
        db_id: DatabaseId,
        root_pgno: Pgno,
        orders: Vec<KeySortOrder>,
    ) -> Result<Vec<Vec<u8>>> {
        if root_pgno == 0 {
            return Ok(Vec::new());
        }
        let Some(slot) = self.db_set.slot_mut(db_id) else {
            return Ok(Vec::new());
        };
        let Some(ref mut bt) = slot.btree else {
            return Ok(Vec::new());
        };
        let enc = bt.encoding();
        let mut svc = ExecutionContext {
            interrupt_flag: self.interrupt_flag,
            runtime: self.runtime,
            encoding: enc,
            functions: self.functions,
            collations: self.collations,
            progress: self.progress,
            strict_subtype: self.strict_subtype,
        };
        scan_index_incremental_with_services(bt, root_pgno, orders, &mut svc)
    }

    /// Ensures `db_id` has at least a `Read` transaction open (`OP_Transaction` `P2 = 0`),
    /// optionally refreshing its cached `Schema` if the schema cookie changed.
    pub(crate) fn ensure_read_db(&mut self, db_id: DatabaseId, check_schema: bool) -> Result<()> {
        let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
        self.stmt_tx.read_db_mask |= bit;
        let autocommit = !*self.explicit_tx && !*self.implicit_savepoint_tx;
        let Some(slot) = self.db_set.slot_mut(db_id) else {
            return Ok(());
        };
        let Some(ref mut bt) = slot.btree else {
            return Ok(());
        };
        if bt.transaction_state() == TransactionState::Open {
            self.busy_policy.retry_btree(bt, |b| b.begin_read())?;
            self.stmt_tx.started_read_mask |= bit;
            if !autocommit {
                *self.in_user_btree_tx = true;
            }
        }
        let reloaded = if check_schema {
            let was_loaded = slot.schema.loaded;
            let prev_cookie = slot.schema.schema_cookie;
            let slot_name = slot.name.clone();
            slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name)?;
            !was_loaded || slot.schema.schema_cookie != prev_cookie
        } else {
            false
        };
        if reloaded {
            self.db_set.reconcile_temp_triggers()?;
        }
        Ok(())
    }

    /// Ensures `db_id` has an active `Write` transaction (`OP_Transaction` `P2 = 1`) and
    /// initializes any active SQL savepoints + statement subsavepoint on `db_id`'s pager.
    pub(crate) fn ensure_write_db(&mut self, db_id: DatabaseId, check_schema: bool) -> Result<()> {
        if db_id == DatabaseId::TEMP && !self.db_set.is_temp_open() {
            let enc = self.encoding();
            self.db_set.ensure_temp_open(enc, *self.writable_schema)?;
        }
        let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
        self.stmt_tx.write_db_mask |= bit;
        self.stmt_tx.read_db_mask |= bit;
        let autocommit = !*self.explicit_tx && !*self.implicit_savepoint_tx;
        let required_depth =
            self.savepoints.len() + usize::from(self.stmt_tx.stmt_sp_slot.is_some());

        let slot = self
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::new(ErrorCode::Error, "unknown database slot"))?;
        let slot_name = slot.name.clone();
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::new(ErrorCode::Error, "database is not open"))?;

        let was_write = bt.transaction_state() == TransactionState::Write;
        if was_write {
            bt.pager_mut().ensure_subsavepoints(required_depth)?;
        } else {
            self.busy_policy.retry_btree(bt, |b| {
                b.pager_mut()
                    .begin_write_with_savepoint_depth(required_depth)
            })?;
        }
        self.stmt_tx.touched_write_mask |= bit;
        if !autocommit {
            *self.in_user_btree_tx = true;
        }
        let reloaded = if check_schema {
            let was_loaded = slot.schema.loaded;
            let prev_cookie = slot.schema.schema_cookie;
            if let Err(e) = slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name) {
                if !was_write {
                    let retain = (self.activity.other_read_db_mask & bit) != 0;
                    let _ = bt.pager_mut().rollback_retaining_read(retain);
                    self.stmt_tx.touched_write_mask &= !bit;
                }
                return Err(e);
            }
            !was_loaded || slot.schema.schema_cookie != prev_cookie
        } else {
            false
        };
        if reloaded {
            self.db_set.reconcile_temp_triggers()?;
        }
        Ok(())
    }
}

/// Resolves `qn` against `db_set` for `SELECT` / `IN [schema.]table` (`sqlite3LocateTable`).
/// Returns `(db_id, table_schema, resolved_schema_name)`.
pub(crate) fn resolve_table_for_query_in_set(
    db_set: &DatabaseSet,
    qn: &QualifiedName,
) -> Result<(DatabaseId, TableSchema, String)> {
    if let Some((db_id, tbl)) = db_set.find_table_in_set(qn)? {
        let sname = db_set
            .slot(db_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "main".to_owned());
        return Ok((db_id, tbl, sname));
    }
    Err(SqliteError::new(
        ErrorCode::Error,
        format!("no such table: {}", qn.display_name()),
    ))
}

/// Parsed target produced by [`parse_sqlite_uri`] (`sqlite3ParseUri` in `sqlite/src/main.c:3150-3355`).
#[derive(Debug, Clone)]
pub(crate) struct ParsedOpenTarget {
    pub path: String,
    pub is_memory: bool,
    pub is_private_temp: bool,
    pub readonly: bool,
    pub create_if_missing: bool,
    pub explicit_mode: bool,
    pub immutable: bool,
    pub enable_8_3_names: bool,
}

fn decode_uri_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2]))
        {
            let b = (hi << 4) | lo;
            if b == 0 {
                // Embedded NUL (`%00`) truncates C string in `sqlite3ParseUri`
                break;
            }
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_uri_bool(v: &str) -> bool {
    let t = v.trim();
    if t.eq_ignore_ascii_case("true")
        || t.eq_ignore_ascii_case("yes")
        || t.eq_ignore_ascii_case("on")
    {
        return true;
    }
    if t.eq_ignore_ascii_case("false")
        || t.eq_ignore_ascii_case("no")
        || t.eq_ignore_ascii_case("off")
    {
        return false;
    }
    t.parse::<i64>().is_ok_and(|n| n != 0)
}

/// Parses a filename or `file:` URI (`sqlite3ParseUri` in `sqlite/src/main.c:3150-3355`).
pub(crate) fn parse_sqlite_uri(
    z_file: &str,
    uri_enabled: bool,
    is_conn_readonly: bool,
    default_create: bool,
    custom_vfs_name: Option<&str>,
) -> Result<ParsedOpenTarget> {
    if z_file.is_empty() {
        return Ok(ParsedOpenTarget {
            path: String::new(),
            is_memory: false,
            is_private_temp: true,
            readonly: is_conn_readonly,
            create_if_missing: default_create && !is_conn_readonly,
            explicit_mode: false,
            immutable: false,
            enable_8_3_names: false,
        });
    }
    if z_file == ":memory:" {
        return Ok(ParsedOpenTarget {
            path: z_file.to_owned(),
            is_memory: true,
            is_private_temp: false,
            readonly: false,
            create_if_missing: true,
            explicit_mode: false,
            immutable: false,
            enable_8_3_names: false,
        });
    }
    if !uri_enabled || !z_file.starts_with("file:") {
        return Ok(ParsedOpenTarget {
            path: z_file.to_owned(),
            is_memory: false,
            is_private_temp: false,
            readonly: is_conn_readonly,
            create_if_missing: default_create && !is_conn_readonly,
            explicit_mode: false,
            immutable: false,
            enable_8_3_names: false,
        });
    }

    let rest = &z_file[5..];
    let before_frag = rest.split('#').next().unwrap_or("");
    let (raw_path_part, query_opt) = match before_frag.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (before_frag, None),
    };

    let raw_file_path = if let Some(after_slashes) = raw_path_part.strip_prefix("//") {
        let (auth, path_rem) = match after_slashes.find('/') {
            Some(idx) => (&after_slashes[..idx], &after_slashes[idx..]),
            None => (after_slashes, ""),
        };
        if !auth.is_empty() && !auth.eq_ignore_ascii_case("localhost") {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("invalid uri authority: {auth}"),
            ));
        }
        path_rem
    } else {
        raw_path_part
    };

    let mut readonly = is_conn_readonly;
    let mut create_if_missing = default_create && !is_conn_readonly;
    let mut explicit_mode = false;
    let mut mode_memory = false;
    let mut immutable = false;
    let mut enable_8_3_names = false;

    if let Some(query) = query_opt {
        for pair in query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (raw_k, raw_v) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => continue,
            };
            let key = decode_uri_component(raw_k);
            let val = decode_uri_component(raw_v);
            match key.as_str() {
                "vfs" => {
                    let known = matches!(
                        val.as_str(),
                        "unix" | "unix-none" | "unix-dotfile" | "unix-excl" | "memdb"
                    ) || custom_vfs_name.is_some_and(|cv| cv == val);
                    if !known {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such vfs: {val}"),
                        ));
                    }
                }
                "cache" => {
                    if val != "shared" && val != "private" {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such cache mode: {val}"),
                        ));
                    }
                }
                "mode" => {
                    explicit_mode = true;
                    match val.as_str() {
                        "ro" => {
                            readonly = true;
                            create_if_missing = false;
                            mode_memory = false;
                        }
                        "rw" => {
                            if is_conn_readonly {
                                return Err(SqliteError::new(
                                    ErrorCode::Perm,
                                    "access mode not allowed: rw",
                                ));
                            }
                            readonly = false;
                            create_if_missing = false;
                            mode_memory = false;
                        }
                        "rwc" => {
                            if is_conn_readonly {
                                return Err(SqliteError::new(
                                    ErrorCode::Perm,
                                    "access mode not allowed: rwc",
                                ));
                            }
                            readonly = false;
                            create_if_missing = true;
                            mode_memory = false;
                        }
                        "memory" => {
                            mode_memory = true;
                            readonly = false;
                            create_if_missing = true;
                        }
                        _ => {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!("no such access mode: {val}"),
                            ));
                        }
                    }
                }
                "immutable" => {
                    if parse_uri_bool(&val) {
                        immutable = true;
                        readonly = true;
                        create_if_missing = false;
                    }
                }
                "8_3_names" => {
                    enable_8_3_names = parse_uri_bool(&val);
                }
                _ => {}
            }
        }
    }

    if mode_memory {
        return Ok(ParsedOpenTarget {
            path: String::new(),
            is_memory: true,
            is_private_temp: false,
            readonly: false,
            create_if_missing: true,
            explicit_mode,
            immutable: false,
            enable_8_3_names,
        });
    }

    let decoded_path = decode_uri_component(raw_file_path);
    if decoded_path == ":memory:" {
        return Ok(ParsedOpenTarget {
            path: decoded_path,
            is_memory: true,
            is_private_temp: false,
            readonly: false,
            create_if_missing: true,
            explicit_mode,
            immutable: false,
            enable_8_3_names,
        });
    }
    if decoded_path.is_empty() {
        return Ok(ParsedOpenTarget {
            path: String::new(),
            is_memory: false,
            is_private_temp: true,
            readonly,
            create_if_missing,
            explicit_mode,
            immutable,
            enable_8_3_names,
        });
    }

    Ok(ParsedOpenTarget {
        path: decoded_path,
        is_memory: false,
        is_private_temp: false,
        readonly,
        create_if_missing,
        explicit_mode,
        immutable,
        enable_8_3_names,
    })
}

/// Implements `resolveAttachExpr` (`sqlite/src/attach.c:35-46`): converts a bare unqualified
/// identifier (`Expr::Column` with no `schema` or `table`) into a string literal while preserving
/// arbitrary SQL expressions (`?`, `NULL`, `printf(...)`, `||`, subqueries).
pub(crate) fn resolve_attach_expr(expr: &Expr) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => Expr::String(column.clone()),
        other => other.clone(),
    }
}

fn eval_attach_text_arg(
    ctx: &mut ExecContext<'_>,
    raw_expr: &Expr,
    params: &[Value],
) -> Result<String> {
    let resolved = resolve_attach_expr(raw_expr);
    if crate::compiler::expr_contains_subquery(&resolved) {
        for idx in 0..ctx.db_set.slot_count() {
            if ctx.db_set.slots()[idx].btree.is_some() && !ctx.db_set.slots()[idx].schema.loaded {
                ctx.ensure_read_db(DatabaseId(idx), true)?;
            }
        }
    }
    let val = eval_expr(ctx, &resolved, params, &[], &CteStore::default(), None)?;
    Ok(match val {
        Value::Null => String::new(),
        other => other.to_text_lossy(),
    })
}

fn resolve_disk_path(working_dir: &Option<PathBuf>, raw_path: &str) -> PathBuf {
    let p = Path::new(raw_path);
    if p.is_relative()
        && let Some(dir) = working_dir
    {
        dir.join(p)
    } else {
        p.to_path_buf()
    }
}

/// Executes `ATTACH [DATABASE] <expr> AS <schema_name> [KEY <key>]` (`attachFunc` in `attach.c:91-307`).
fn exec_attach_database(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    schema_name: &Expr,
    key: Option<&Expr>,
    params: &[Value],
) -> Result<()> {
    let z_file = eval_attach_text_arg(ctx, expr, params)?;
    let z_name = eval_attach_text_arg(ctx, schema_name, params)?;
    if let Some(key_expr) = key {
        let _ = eval_attach_text_arg(ctx, key_expr, params)?;
    }

    if ctx.db_set.attached_count() >= ctx.db_set.max_attached() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!(
                "too many attached databases - max {}",
                ctx.db_set.max_attached()
            ),
        ));
    }
    if ctx.db_set.find_db_id(&z_name).is_some() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("database {z_name} is already in use"),
        ));
    }

    let custom_vfs_name = Some(ctx.db_set.disk_vfs().name());
    let parsed = parse_sqlite_uri(
        &z_file,
        ctx.uri_enabled,
        ctx.is_connection_readonly,
        true,
        custom_vfs_name,
    )?;

    if ctx.db_set.main_btree().pager().page_count() > 0 {
        *ctx.connection_encoding = ctx.db_set.main_btree().encoding();
    }
    let expected_enc = *ctx.connection_encoding;

    let open_res: Result<Btree> = if parsed.is_memory {
        let mem_vfs: Arc<dyn Vfs> =
            Arc::new(MemoryVfs::with_lock_geometry(ctx.db_set.lock_geometry()));
        let opts = PagerOpenOptions {
            default_page_size: ctx.db_set.default_page_size(),
            lock_geometry: Some(ctx.db_set.lock_geometry()),
            db_kind: Some(DatabaseKind::Memory),
            ..PagerOpenOptions::default()
        };
        Btree::open_with_options(
            mem_vfs,
            Some(Path::new(":memory:")),
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY | OpenFlags::MAIN_DB,
            opts,
        )
    } else if parsed.is_private_temp {
        if parsed.readonly && !parsed.create_if_missing {
            Err(SqliteError::new(
                ErrorCode::CantOpen,
                format!("unable to open database: {z_file}"),
            ))
        } else {
            let opts = PagerOpenOptions {
                default_page_size: ctx.db_set.default_page_size(),
                lock_geometry: Some(ctx.db_set.lock_geometry()),
                db_kind: Some(DatabaseKind::PrivateTemporary),
                temp_in_memory: ctx.db_set.temp_store().is_memory(),
                ..PagerOpenOptions::default()
            };
            let mut bt = Btree::open_private_temporary(Arc::clone(ctx.db_set.disk_vfs()), opts)?;
            let _ = bt.pager_mut().set_locking_mode(LockingMode::Exclusive);
            Ok(bt)
        }
    } else {
        let full_path = resolve_disk_path(ctx.working_dir, &parsed.path);
        let opts = PagerOpenOptions {
            default_page_size: ctx.db_set.default_page_size(),
            lock_geometry: Some(ctx.db_set.lock_geometry()),
            db_kind: Some(DatabaseKind::NamedDisk),
            immutable: parsed.immutable,
            ..PagerOpenOptions::default()
        };
        let vfs = Arc::clone(ctx.db_set.disk_vfs());
        if parsed.readonly {
            Btree::open_with_options(
                vfs,
                Some(&full_path),
                OpenFlags::READONLY | OpenFlags::MAIN_DB,
                opts,
            )
        } else {
            let mut primary_flags = OpenFlags::READWRITE | OpenFlags::MAIN_DB;
            if parsed.create_if_missing {
                primary_flags = primary_flags | OpenFlags::CREATE;
            }
            match Btree::open_with_options(Arc::clone(&vfs), Some(&full_path), primary_flags, opts)
            {
                Ok(bt) => Ok(bt),
                Err(e)
                    if !parsed.explicit_mode
                        && matches!(e.code, ErrorCode::CantOpen | ErrorCode::ReadOnly) =>
                {
                    Btree::open_with_options(
                        vfs,
                        Some(&full_path),
                        OpenFlags::READONLY | OpenFlags::MAIN_DB,
                        opts,
                    )
                }
                Err(e) => Err(e),
            }
        }
    };

    let mut new_bt = match open_res {
        Ok(bt) => bt,
        Err(e) if e.code == ErrorCode::CantOpen => {
            return Err(SqliteError::new(
                ErrorCode::CantOpen,
                format!("unable to open database: {z_file}"),
            ));
        }
        Err(e) => return Err(e),
    };

    new_bt.set_interrupt_flag(Some(Arc::clone(ctx.db_set.interrupt_flag())));
    new_bt.set_writable_schema(*ctx.writable_schema);
    if !parsed.is_private_temp && *ctx.default_locking_mode == LockingMode::Exclusive {
        let _ = new_bt.set_locking_mode(LockingMode::Exclusive);
    }

    // Eagerly read and validate schema (`sqlite3InitOne` in `attach.c:226-240`).
    let loaded_schema = match ctx
        .busy_policy
        .retry_btree(&mut new_bt, |b| Schema::load_for_schema(b, Some(&z_name)))
    {
        Ok(s) => s,
        Err(e) => {
            let _ = new_bt.close();
            return Err(e);
        }
    };

    if new_bt.pager().page_count() > 0 {
        if new_bt.encoding() != expected_enc {
            let _ = new_bt.close();
            return Err(SqliteError::new(
                ErrorCode::Error,
                "attached databases must use the same text encoding as main database",
            ));
        }
    } else if let Err(e) = new_bt.pager_mut().set_encoding(expected_enc) {
        let _ = new_bt.close();
        return Err(e);
    }

    ctx.db_set
        .attach_slot(z_name, new_bt, loaded_schema, parsed.enable_8_3_names)?;

    // `ATTACH` emits `OP_Expire 1` (`attach.c:414`): expires ONLY the `ATTACH` statement itself for V1
    // (`attach2-3.1`) while bumping `schema_generation` so V2 statements reprepare cleanly.
    *ctx.expire_self_only = true;
    *ctx.schema_generation += 1;
    Ok(())
}

/// Executes `DETACH [DATABASE] <schema_name>` (`detachFunc` in `attach.c:320-370`).
fn exec_detach_database(
    ctx: &mut ExecContext<'_>,
    schema_name: &Expr,
    params: &[Value],
) -> Result<()> {
    let z_name = eval_attach_text_arg(ctx, schema_name, params)?;
    let Some(db_id) = ctx.db_set.find_open_db_id_for_detach(&z_name) else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such database: {z_name}"),
        ));
    };
    if db_id.index() < 2 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("cannot detach database {z_name}"),
        ));
    }

    let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
    let busy_by_lease =
        ((ctx.activity.other_read_db_mask | ctx.activity.other_write_db_mask) & bit) != 0;
    let in_tx = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .is_some_and(|bt| bt.transaction_state() != TransactionState::Open);

    if in_tx || busy_by_lease {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("database {z_name} is locked"),
        ));
    }

    let (detached_inst_id, _) = ctx.db_set.detach_slot(db_id)?;
    ctx.deferred_fk_suspects
        .retain(|(inst, _, _, _)| *inst != detached_inst_id);

    // `DETACH` emits `OP_Expire 0` (`attach.c:461`): expires all prepared statements on the connection.
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    Ok(())
}

/// Rolls back all active `Read` and `Write` transactions across every slot in `ctx.db_set`.
pub(crate) fn rollback_all_databases(
    ctx: &mut ExecContext<'_>,
    reload_schemas_if_changed: bool,
) -> Result<()> {
    let write_only = !*ctx.schema_changed_in_tx;
    let mut first_err: Option<SqliteError> = None;

    for idx in 0..ctx.db_set.slot_count() {
        let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
        let retain_read = ((ctx.activity.other_read_db_mask & bit) != 0) && write_only;
        if let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree {
            let res = match bt.transaction_state() {
                TransactionState::Open => Ok(()),
                TransactionState::Read => {
                    let trip_res = bt.trip_cursors(extended::SQLITE_ABORT_ROLLBACK, write_only);
                    let end_res = if !retain_read {
                        bt.pager_mut().end_read_if_unused()
                    } else {
                        Ok(())
                    };
                    end_res.and(trip_res)
                }
                TransactionState::Write => {
                    bt.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, write_only, retain_read)
                }
            };
            if let Err(e) = res
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
    }

    if reload_schemas_if_changed && *ctx.schema_changed_in_tx {
        for slot in ctx.db_set.slots_mut() {
            if let Some(ref mut bt) = slot.btree
                && let Ok(fresh) = Schema::load_for_schema(bt, Some(&slot.name))
            {
                slot.schema = fresh;
            }
        }
        let _ = ctx.db_set.reconcile_temp_triggers();
        *ctx.schema_generation += 1;
        *ctx.v1_expire_generation += 1;
    }

    if let Some(e) = first_err {
        Err(e)
    } else {
        Ok(())
    }
}

/// Outcome of [`commit_all_databases`] when an error occurs, indicating whether `commit_hook` or
/// phase-1 I/O already rolled back the transaction (`RolledBack`) vs leaving it open for retry (`RetryableOrOpen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitFailureDisposition {
    /// Transaction remains open (e.g. `FK` constraint failure or `Busy` on `acquire_exclusive_lock`).
    LeaveOpen,
    /// Transaction was rolled back across all slots (e.g. `SQLITE_CONSTRAINT_COMMITHOOK` or Phase 1 I/O error).
    RolledBack,
}

/// Full 6-step multi-database commit coordinator (`vdbeCommit` in `sqlite/src/vdbeaux.c:2940-3190`).
pub(crate) fn commit_all_databases(
    ctx: &mut ExecContext<'_>,
) -> std::result::Result<(), (SqliteError, CommitFailureDisposition)> {
    // Step 1: Deferred foreign key check (`sqlite3VdbeCheckFkDeferred`).
    if let Err(fk_err) = check_deferred_fks(ctx) {
        return Err((fk_err, CommitFailureDisposition::LeaveOpen));
    }

    let need_xcommit = ctx.db_set.any_in_write_transaction();

    // Step 2: Virtual table pre-commit sync (`sqlite3VtabSync`).
    if need_xcommit {
        let enc = ctx.encoding();
        let sync_res = {
            let mut exec_svc = ExecutionContext {
                interrupt_flag: ctx.interrupt_flag,
                runtime: ctx.runtime,
                encoding: enc,
                functions: ctx.functions,
                collations: ctx.collations,
                progress: ctx.progress,
                strict_subtype: ctx.strict_subtype,
            };
            exec_svc.vtab_sync_all(ctx.commit_hooks)
        };
        if let Err(e) = sync_res {
            return Err((e, CommitFailureDisposition::LeaveOpen));
        }
    }

    // Step 3: Acquire EXCLUSIVE lock across all write-transaction pagers in `0..nDb` order using
    // `busy_policy.retry_btree`. If slot `i` returns `Busy`, stop immediately WITHOUT rolling back
    // so `PendingCommit` preserves already-upgraded `Pending` locks (`attach2-4.10.2`).
    if need_xcommit {
        for idx in 0..ctx.db_set.slot_count() {
            if let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree
                && bt.transaction_state() == TransactionState::Write
                && let Err(e) = ctx
                    .busy_policy
                    .retry_btree(bt, Btree::acquire_exclusive_lock)
            {
                return Err((e, CommitFailureDisposition::LeaveOpen));
            }
        }
    }

    // Step 4: Invoke connection commit hook (`xCommitCallback`, `vdbeaux.c:2997-3002`).
    // Non-zero (`true`) aborts with `SQLITE_CONSTRAINT_COMMITHOOK` and rolls back ALL write transactions!
    if need_xcommit {
        let enc = ctx.encoding();
        let hook_res = {
            let mut exec_svc = ExecutionContext {
                interrupt_flag: ctx.interrupt_flag,
                runtime: ctx.runtime,
                encoding: enc,
                functions: ctx.functions,
                collations: ctx.collations,
                progress: ctx.progress,
                strict_subtype: ctx.strict_subtype,
            };
            exec_svc.invoke_commit_hook(ctx.commit_hooks)
        };
        if let Err(e) = hook_res {
            let _ = rollback_all_databases(ctx, true);
            return Err((e, CommitFailureDisposition::RolledBack));
        }
    }

    // Step 5: Atomic two-phase commit across all open `Btree`s (`commit_btrees_two_phase`).
    let vfs = Arc::clone(ctx.db_set.disk_vfs());
    let main_db_path = ctx.db_set.main_btree().db_path().map(Path::to_path_buf);
    let enable_8_3 = ctx.db_set.slots().iter().any(|s| s.enable_8_3_names);

    let mut open_indices = Vec::with_capacity(ctx.db_set.slot_count());
    let mut retain_reads = Vec::with_capacity(ctx.db_set.slot_count());
    for (idx, slot) in ctx.db_set.slots().iter().enumerate() {
        if slot.btree.is_some() {
            open_indices.push(idx);
            let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
            retain_reads.push((ctx.activity.other_read_db_mask & bit) != 0);
        }
    }

    let two_phase_res = {
        let mut bt_refs: Vec<&mut Btree> = ctx
            .db_set
            .slots_mut()
            .iter_mut()
            .filter_map(|s| s.btree.as_mut())
            .collect();
        commit_btrees_two_phase(
            vfs.as_ref(),
            main_db_path.as_deref(),
            enable_8_3,
            &mut bt_refs,
            &retain_reads,
        )
    };

    if let Err(e) = two_phase_res {
        return Err((e, CommitFailureDisposition::LeaveOpen));
    }

    // Step 6: Virtual table post-commit notification (`sqlite3VtabCommit`).
    if need_xcommit {
        let enc = ctx.encoding();
        let mut exec_svc = ExecutionContext {
            interrupt_flag: ctx.interrupt_flag,
            runtime: ctx.runtime,
            encoding: enc,
            functions: ctx.functions,
            collations: ctx.collations,
            progress: ctx.progress,
            strict_subtype: ctx.strict_subtype,
        };
        exec_svc.vtab_commit_all(ctx.commit_hooks);
    }

    Ok(())
}

///Classify `stmt` into its execution lease category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StmtCategory {
    /// `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`
    Control,
    /// Pragmas / `ANALYZE` / `ATTACH` / `DETACH` that manage their own locks without a wrapper tx
    NoLockPragma,
    /// `SELECT`, `EXPLAIN`, and read-transaction `PRAGMA`s
    Read,
    /// `INSERT`, `UPDATE`, `DELETE`, DDL, `REINDEX`, and header-writing `PRAGMA`s
    Write,
    /// Streaming `PRAGMA [schema.]incremental_vacuum[(N)]`
    IncrementalVacuum,
    /// `VACUUM [schema] [INTO expr]`
    Vacuum,
}

/// Returns `true` when `stmt` must load `sqlite_schema` at `prepare` time (`PragFlg_NeedSchema` / `prepare.c`).
pub(crate) fn stmt_loads_schema_on_prepare(stmt: &Stmt) -> bool {
    if stmt_requires_schema(stmt) {
        return true;
    }
    matches!(
        stmt,
        Stmt::Pragma { name, .. }
            if matches!(
                name.to_ascii_uppercase().as_str(),
                "AUTO_VACUUM" | "INCREMENTAL_VACUUM" | "DEFAULT_CACHE_SIZE"
            )
    )
}

/// Returns `true` when `stmt` verifies `sqlite_schema` cookie on `step()` (`OP_Transaction` `P3 != 0`).
pub(crate) fn stmt_requires_schema(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_)
        | Stmt::AttachDatabase { .. }
        | Stmt::DetachDatabase { .. } => false,
        Stmt::Pragma { name, .. } => matches!(
            name.to_ascii_uppercase().as_str(),
            "TABLE_INFO"
                | "TABLE_XINFO"
                | "INDEX_LIST"
                | "INDEX_INFO"
                | "FOREIGN_KEY_LIST"
                | "FOREIGN_KEY_CHECK"
                | "INTEGRITY_CHECK"
                | "QUICK_CHECK"
                | "WAL_CHECKPOINT"
        ),
        _ => true,
    }
}

/// Parses `PRAGMA auto_vacuum = <val>` matching `getAutoVacuum` (`sqlite/src/pragma.c:125-132`).
pub(crate) fn parse_auto_vacuum_pragma(pv: &PragmaValue) -> AutoVacuumMode {
    match pv {
        PragmaValue::Integer(1) => AutoVacuumMode::Full,
        PragmaValue::Integer(2) => AutoVacuumMode::Incremental,
        PragmaValue::Integer(_) => AutoVacuumMode::None,
        PragmaValue::Ident(s) | PragmaValue::String(s) => {
            let trimmed = s.trim();
            if trimmed.eq_ignore_ascii_case("none") {
                return AutoVacuumMode::None;
            }
            if trimmed.eq_ignore_ascii_case("full") {
                return AutoVacuumMode::Full;
            }
            if trimmed.eq_ignore_ascii_case("incremental") {
                return AutoVacuumMode::Incremental;
            }
            match trimmed.parse::<i32>() {
                Ok(1) => AutoVacuumMode::Full,
                Ok(2) => AutoVacuumMode::Incremental,
                _ => AutoVacuumMode::None,
            }
        }
    }
}

/// Parses `PRAGMA incremental_vacuum[(N) | = N]` matching `sqlite3GetInt32` in `pragma.c:856-859`:
/// omitted, non-integer, out-of-`i32`-range, or `<= 0` all map to `i32::MAX as u32` (`0x7fff_ffff`).
pub(crate) fn parse_incremental_vacuum_limit(value: Option<&PragmaValue>) -> u32 {
    let Some(pv) = value else {
        return i32::MAX as u32;
    };
    let parsed_i32: Option<i32> = match pv {
        PragmaValue::Integer(i) => i32::try_from(*i).ok(),
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.trim().parse::<i32>().ok(),
    };
    match parsed_i32 {
        Some(n) if n > 0 => n as u32,
        _ => i32::MAX as u32,
    }
}

/// Classifies `stmt` into its execution lease category.
pub(crate) fn classify_stmt_category(stmt: &Stmt) -> StmtCategory {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_) => StmtCategory::Control,
        Stmt::Analyze(_) | Stmt::AttachDatabase { .. } | Stmt::DetachDatabase { .. } => {
            StmtCategory::NoLockPragma
        }
        Stmt::Vacuum { .. } => StmtCategory::Vacuum,
        Stmt::Select(_) | Stmt::Explain { .. } => StmtCategory::Read,
        Stmt::Pragma { name, value, .. } => match name.to_ascii_uppercase().as_str() {
            "INTEGRITY_CHECK" | "QUICK_CHECK" | "PAGE_COUNT" | "MAX_PAGE_COUNT"
            | "FREELIST_COUNT" | "FOREIGN_KEY_LIST" | "FOREIGN_KEY_CHECK" | "TABLE_INFO"
            | "TABLE_XINFO" | "INDEX_LIST" | "INDEX_INFO" => StmtCategory::Read,
            "INCREMENTAL_VACUUM" => StmtCategory::IncrementalVacuum,
            "USER_VERSION" | "APPLICATION_ID" | "DEFAULT_CACHE_SIZE" | "SCHEMA_VERSION" => {
                if value.is_some() {
                    StmtCategory::Write
                } else {
                    StmtCategory::Read
                }
            }
            _ => StmtCategory::NoLockPragma,
        },
        Stmt::Insert(_)
        | Stmt::Update(_)
        | Stmt::Delete(_)
        | Stmt::CreateTable(_)
        | Stmt::CreateTableAs { .. }
        | Stmt::CreateView(_)
        | Stmt::CreateTrigger(_)
        | Stmt::DropTable { .. }
        | Stmt::DropView { .. }
        | Stmt::DropTrigger { .. }
        | Stmt::CreateIndex(_)
        | Stmt::DropIndex { .. }
        | Stmt::AlterTable { .. }
        | Stmt::Reindex(_) => StmtCategory::Write,
    }
}

#[derive(Debug, Clone)]
struct ScopedColumn {
    schema_name: Option<String>,
    table_alias: Option<String>,
    name: String,
    collation: Collation,
    affinity: Option<TypeAffinity>,
    hidden_rowid: bool,
    omit_from_star: bool,
    merged_using: bool,
}

impl ScopedColumn {
    #[inline]
    fn matches_qualifier(&self, schema_qual: Option<&str>, table_qual: Option<&str>) -> bool {
        if let Some(sq) = schema_qual {
            let Some(ref sn) = self.schema_name else {
                return false;
            };
            if !sn.eq_ignore_ascii_case(sq) {
                return false;
            }
        }
        if let Some(tq) = table_qual {
            let Some(ref ta) = self.table_alias else {
                return false;
            };
            if !ta.eq_ignore_ascii_case(tq) {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone)]
struct EvalRow {
    cols: Rc<[ScopedColumn]>,
    vals: Vec<Value>,
    /// When `true` (e.g. `UPSERT` virtual `excluded` scope), only unqualified-schema `table.col`
    /// lookups match (`col` and `schema.table.col` do not match).
    qualified_only: bool,
}

impl Default for EvalRow {
    fn default() -> Self {
        Self {
            cols: Rc::from([]),
            vals: Vec::new(),
            qualified_only: false,
        }
    }
}

const STAR_SLOT_SCHEMA: &str = "\0slot";

impl EvalRow {
    #[inline]
    fn new(cols: Rc<[ScopedColumn]>, vals: Vec<Value>) -> Self {
        Self {
            cols,
            vals,
            qualified_only: false,
        }
    }

    fn lookup(&self, qual: Option<&str>, col_name: &str) -> Result<Option<(Value, Collation)>> {
        Ok(self
            .lookup_qual_full(None, qual, col_name)?
            .map(|(v, c, _)| (v, c)))
    }

    fn lookup_full(
        &self,
        qual: Option<&str>,
        col_name: &str,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        self.lookup_qual_full(None, qual, col_name)
    }

    fn lookup_cref(&self, cref: &ColumnRef) -> Result<Option<(Value, Collation)>> {
        Ok(self
            .lookup_qual_full(cref.schema.as_deref(), cref.table.as_deref(), &cref.column)?
            .map(|(v, c, _)| (v, c)))
    }

    fn lookup_cref_full(
        &self,
        cref: &ColumnRef,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        self.lookup_qual_full(cref.schema.as_deref(), cref.table.as_deref(), &cref.column)
    }

    fn lookup_qual_full(
        &self,
        schema_qual: Option<&str>,
        table_qual: Option<&str>,
        col_name: &str,
    ) -> Result<Option<(Value, Collation, Option<TypeAffinity>)>> {
        if schema_qual == Some(STAR_SLOT_SCHEMA) {
            if self.qualified_only {
                return Ok(None);
            }
            if let Ok(idx) = col_name.parse::<usize>()
                && let Some(sc) = self.cols.get(idx)
                && let Some(val) = self.vals.get(idx)
            {
                return Ok(Some((val.clone(), sc.collation, sc.affinity)));
            }
            return Ok(None);
        }
        if self.qualified_only && (schema_qual.is_some() || table_qual.is_none()) {
            return Ok(None);
        }
        let format_ambiguous = || match (schema_qual, table_qual) {
            (Some(s), Some(t)) => format!("ambiguous column name: {s}.{t}.{col_name}"),
            (None, Some(t)) => format!("ambiguous column name: {t}.{col_name}"),
            _ => format!("ambiguous column name: {col_name}"),
        };
        let mut matches = Vec::new();
        for (i, sc) in self.cols.iter().enumerate() {
            if sc.hidden_rowid {
                continue;
            }
            if !sc.matches_qualifier(schema_qual, table_qual) {
                continue;
            }
            if sc.name.eq_ignore_ascii_case(col_name) {
                matches.push((
                    self.vals[i].clone(),
                    sc.collation,
                    sc.affinity,
                    sc.merged_using,
                ));
                if schema_qual.is_none() && table_qual.is_none() && sc.merged_using {
                    // Deduplicated USING / NATURAL column takes unambiguous precedence
                    return Ok(Some((self.vals[i].clone(), sc.collation, sc.affinity)));
                }
            }
        }

        if matches.len() == 1 {
            return Ok(Some((matches[0].0.clone(), matches[0].1, matches[0].2)));
        }
        if matches.len() > 1 {
            return Err(SqliteError::new(ErrorCode::Error, format_ambiguous()));
        }

        // Check hidden implicit rowid (`rowid`, `_rowid_`, `oid`)
        if col_name.eq_ignore_ascii_case("rowid")
            || col_name.eq_ignore_ascii_case("_rowid_")
            || col_name.eq_ignore_ascii_case("oid")
        {
            for (i, sc) in self.cols.iter().enumerate() {
                if !sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(schema_qual, table_qual) {
                    continue;
                }
                matches.push((
                    self.vals[i].clone(),
                    Collation::Binary,
                    Some(TypeAffinity::Integer),
                    false,
                ));
            }
            if matches.len() == 1 {
                return Ok(Some((
                    matches[0].0.clone(),
                    Collation::Binary,
                    Some(TypeAffinity::Integer),
                )));
            }
            if matches.len() > 1 {
                return Err(SqliteError::new(ErrorCode::Error, format_ambiguous()));
            }
        }

        Ok(None)
    }
}

type CteScopeCache = Rc<RefCell<Vec<Option<QueryOutput>>>>;

#[derive(Clone)]
struct CteStreamHint {
    s_idx: usize,
    c_idx: usize,
    eff_alias: String,
    where_expr: Option<Expr>,
    max_kept: usize,
}

#[derive(Clone)]
struct CachedStaticView {
    cols: Vec<String>,
    meta: Vec<(Option<TypeAffinity>, Collation)>,
    /// Cumulative `(db_id, lower_name, canonical_name, count)` inside 1 expansion of this view.
    sub_refs: Vec<(DatabaseId, String, String, usize)>,
}

type StaticViewCache = Rc<RefCell<HashMap<(DatabaseId, String), CachedStaticView>>>;
type ViewMetaCache = Rc<
    RefCell<HashMap<(DatabaseId, String), (Vec<String>, Vec<(Option<TypeAffinity>, Collation)>)>>,
>;
type RuntimeViewCache = Rc<
    RefCell<
        HashMap<
            (DatabaseId, String),
            (QueryOutput, Vec<(Option<TypeAffinity>, Collation)>, String),
        >,
    >,
>;
type CatalogRefMap = Rc<RefCell<HashMap<(DatabaseId, String), usize>>>;

#[derive(Clone, Default)]
struct CteStore {
    /// Lexical stack of `WITH` scopes (`(scope_idx, cte_idx)`).
    scopes: Vec<Vec<CommonTableExpr>>,
    /// Per-scope materialization caches parallel to `scopes`.
    scope_caches: Vec<CteScopeCache>,
    /// Outer `EvalRow` stack depth at the point each `WITH` scope was entered.
    scope_outer_depths: Vec<usize>,
    /// Active 1-row working tables during recursive CTE queue steps (`(scope_idx, cte_idx, QueryOutput)`).
    working: Vec<(usize, usize, QueryOutput)>,
    /// Active CTE evaluation chain `(scope_idx, cte_idx)` to detect circular references.
    resolving: Vec<(usize, usize)>,
    /// Optional outer row limit hint for non-`MATERIALIZED` recursive CTE co-routine short-circuiting.
    limit_hint: Option<CteStreamHint>,
    /// Active view expansion chain `(db_id, lowercase_view_name)` to detect circular views (`select.c`).
    resolving_views: Vec<(DatabaseId, String)>,
    /// Cumulative expanded `FROM` cursors (`pParse->nTab`) during prepare-time static validation.
    expanded_tabs: Rc<Cell<usize>>,
    /// Current nested `SELECT`/CTE/VIEW depth (`pParse->nNestSel`) during prepare-time static validation.
    nest_depth: usize,
    /// Per-query `Table.nTabRef` counter (`sqlite/src/select.c:6154`, max `65535`).
    tab_refs: CatalogRefMap,
    /// Memoized view column & affinity resolution (`sqlite3ViewGetColumnNames`).
    static_view_cache: StaticViewCache,
    /// Memoized view column names & `(affinity, collation)` metadata for `static_view_scoped_cols`.
    view_meta_cache: ViewMetaCache,
    /// Memoized read-snapshot view output within a single top-level query.
    runtime_view_cache: RuntimeViewCache,
}

impl CteStore {
    fn record_expanded_tab(&self) {
        self.expanded_tabs.set(self.expanded_tabs.get() + 1);
    }

    fn record_catalog_ref(&self, db_id: DatabaseId, canonical_name: &str) -> Result<()> {
        let key = (db_id, canonical_name.to_ascii_lowercase());
        let mut map = self.tab_refs.borrow_mut();
        let entry = map.entry(key).or_insert(1);
        if *entry >= 0xffff {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("too many references to \"{canonical_name}\": max 65535"),
            ));
        }
        *entry += 1;
        Ok(())
    }

    fn enter_nested_select(&self, expr_depth_limit: usize) -> Result<Self> {
        let next_nest = self.nest_depth + 1;
        if expr_depth_limit > 0 && next_nest >= expr_depth_limit {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "VIEWs and/or subqueries nested too deep",
            ));
        }
        let mut next = self.clone();
        next.nest_depth = next_nest;
        Ok(next)
    }

    fn enter_cte_select(&self, expr_depth_limit: usize) -> Result<Self> {
        let tabs = self.expanded_tabs.get();
        let base_nest = if expr_depth_limit > 0 && tabs >= expr_depth_limit {
            tabs
        } else {
            self.nest_depth
        };
        let next_nest = base_nest + 1;
        if expr_depth_limit > 0 && next_nest >= expr_depth_limit {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "VIEWs and/or subqueries nested too deep",
            ));
        }
        let mut next = self.clone();
        next.nest_depth = next_nest;
        Ok(next)
    }

    fn enter_view(
        &self,
        db_id: DatabaseId,
        view_name: &str,
        expr_depth_limit: usize,
    ) -> Result<Self> {
        let lower = view_name.to_ascii_lowercase();
        if self
            .resolving_views
            .iter()
            .any(|(id, n)| *id == db_id && *n == lower)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("view {view_name} is circularly defined"),
            ));
        }
        let next_nest = self.nest_depth + 1;
        if expr_depth_limit > 0 && next_nest >= expr_depth_limit {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "VIEWs and/or subqueries nested too deep",
            ));
        }
        let mut resolving_views = self.resolving_views.clone();
        resolving_views.push((db_id, lower));
        Ok(Self {
            scopes: Vec::new(),
            scope_caches: Vec::new(),
            scope_outer_depths: Vec::new(),
            working: Vec::new(),
            resolving: Vec::new(),
            limit_hint: None,
            resolving_views,
            expanded_tabs: Rc::clone(&self.expanded_tabs),
            nest_depth: next_nest,
            tab_refs: Rc::clone(&self.tab_refs),
            static_view_cache: Rc::clone(&self.static_view_cache),
            view_meta_cache: Rc::clone(&self.view_meta_cache),
            runtime_view_cache: Rc::clone(&self.runtime_view_cache),
        })
    }

    fn with_scope(&self, ctes: &[CommonTableExpr]) -> Self {
        self.with_scope_at(ctes, 0)
    }

    fn with_scope_at(&self, ctes: &[CommonTableExpr], outer_depth: usize) -> Self {
        let mut next = self.clone();
        if !ctes.is_empty() {
            next.scopes.push(ctes.to_vec());
            next.scope_caches
                .push(Rc::new(RefCell::new(vec![None; ctes.len()])));
            next.scope_outer_depths.push(outer_depth);
        }
        next.limit_hint = None;
        next
    }

    fn slice_to_scope(&self, s_idx: usize) -> Self {
        Self {
            scopes: self.scopes[..=s_idx].to_vec(),
            scope_caches: self.scope_caches[..=s_idx].to_vec(),
            scope_outer_depths: self.scope_outer_depths[..=s_idx].to_vec(),
            working: self.working.clone(),
            resolving: Vec::new(),
            limit_hint: None,
            resolving_views: self.resolving_views.clone(),
            expanded_tabs: Rc::clone(&self.expanded_tabs),
            nest_depth: self.nest_depth,
            tab_refs: Rc::clone(&self.tab_refs),
            static_view_cache: Rc::clone(&self.static_view_cache),
            view_meta_cache: Rc::clone(&self.view_meta_cache),
            runtime_view_cache: Rc::clone(&self.runtime_view_cache),
        }
    }

    fn locate(&self, name: &str) -> Option<(usize, usize)> {
        for (s_idx, scope) in self.scopes.iter().enumerate().rev() {
            for (c_idx, cte) in scope.iter().enumerate() {
                if cte.name.eq_ignore_ascii_case(name) {
                    return Some((s_idx, c_idx));
                }
            }
        }
        None
    }

    fn find_working(&self, s_idx: usize, c_idx: usize) -> Option<&QueryOutput> {
        self.working
            .iter()
            .rev()
            .find(|(ws, wc, _)| *ws == s_idx && *wc == c_idx)
            .map(|(_, _, q)| q)
    }
}

/// Checks all outstanding deferred foreign key suspects (`sqlite3VdbeCheckFkDeferred`).
pub(crate) fn check_deferred_fks(ctx: &mut ExecContext<'_>) -> Result<()> {
    if *ctx.foreign_keys && !ctx.deferred_fk_suspects.is_empty() {
        let suspects = ctx.deferred_fk_suspects.clone();
        verify_fk_suspects(ctx, &suspects)?;
    }
    Ok(())
}

/// Executes `COMMIT` (`OP_AutoCommit` with `desiredAutoCommit = 1, iRollback = 0`).
/// If `acquire_exclusive_lock` returns `ErrorCode::Busy`, leaves the transaction open so it can be retried.
pub(crate) fn commit_transaction(ctx: &mut ExecContext<'_>) -> Result<()> {
    if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "cannot commit - no transaction is active",
        ));
    }
    if ctx.activity.other_writers > 0 {
        return Err(SqliteError::new(
            ErrorCode::Busy,
            "cannot commit transaction - SQL statements in progress",
        ));
    }

    match commit_all_databases(ctx) {
        Ok(()) => {
            *ctx.explicit_tx = false;
            *ctx.implicit_savepoint_tx = false;
            *ctx.in_user_btree_tx = false;
            ctx.savepoints.clear();
            *ctx.schema_changed_in_tx = false;
            ctx.deferred_fk_suspects.clear();
            ctx.publish();
            Ok(())
        }
        Err((err, CommitFailureDisposition::LeaveOpen)) => Err(err),
        Err((err, CommitFailureDisposition::RolledBack)) => {
            *ctx.rollback_count += 1;
            *ctx.explicit_tx = false;
            *ctx.implicit_savepoint_tx = false;
            *ctx.in_user_btree_tx = false;
            ctx.savepoints.clear();
            *ctx.schema_changed_in_tx = false;
            ctx.deferred_fk_suspects.clear();
            ctx.publish();
            Err(err)
        }
    }
}

/// Executes `RELEASE [SAVEPOINT] <name>` (`OP_Savepoint` `SAVEPOINT_RELEASE`).
pub(crate) fn release_savepoint_op(ctx: &mut ExecContext<'_>, sp_name: &str) -> Result<()> {
    let Some(pos) = ctx
        .savepoints
        .iter()
        .rposition(|sp| sp.name.eq_ignore_ascii_case(sp_name))
    else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such savepoint: {sp_name}"),
        ));
    };
    if ctx.activity.other_writers > 0 {
        return Err(SqliteError::new(
            ErrorCode::Busy,
            "cannot release savepoint - SQL statements in progress",
        ));
    }

    let is_tx_sp = pos == 0 && ctx.savepoints[0].is_transaction_savepoint;
    if is_tx_sp {
        match commit_all_databases(ctx) {
            Ok(()) => {
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                ctx.publish();
                Ok(())
            }
            Err((err, CommitFailureDisposition::LeaveOpen)) => Err(err),
            Err((err, CommitFailureDisposition::RolledBack)) => {
                *ctx.rollback_count += 1;
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                ctx.publish();
                Err(err)
            }
        }
    } else {
        let slot_depth = ctx.savepoints[pos].subsavepoint_depth;
        for db_slot in ctx.db_set.slots_mut() {
            if let Some(ref mut bt) = db_slot.btree
                && bt.transaction_state() == TransactionState::Write
            {
                bt.pager_mut().release_subsavepoint(slot_depth)?;
            }
        }
        ctx.savepoints.truncate(pos);
        Ok(())
    }
}

/// Executes transaction / savepoint control statements (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`).
pub(crate) fn execute_control_stmt(ctx: &mut ExecContext<'_>, stmt: &Stmt) -> Result<()> {
    match stmt {
        Stmt::Begin { mode } => {
            if !is_in_autocommit(ctx) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot start a transaction within a transaction",
                ));
            }
            match mode {
                sqliters_syntax::TransactionMode::Deferred => {
                    *ctx.explicit_tx = true;
                    *ctx.implicit_savepoint_tx = false;
                    ctx.publish();
                }
                sqliters_syntax::TransactionMode::Immediate
                | sqliters_syntax::TransactionMode::Exclusive => {
                    let is_excl = matches!(mode, sqliters_syntax::TransactionMode::Exclusive);
                    let mut started_slots: Vec<(usize, bool)> = Vec::new();
                    for idx in 0..ctx.db_set.slot_count() {
                        let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree else {
                            continue;
                        };
                        let was_open = bt.transaction_state() == TransactionState::Open;
                        let is_ro = bt.pager().is_readonly();
                        let res = if is_ro {
                            if was_open {
                                ctx.busy_policy.retry_btree(bt, |b| b.begin_read())
                            } else {
                                Ok(())
                            }
                        } else if is_excl {
                            ctx.busy_policy.retry_btree(bt, |b| b.begin_exclusive())
                        } else {
                            ctx.busy_policy.retry_btree(bt, |b| b.begin_write())
                        };
                        match res {
                            Ok(()) => {
                                started_slots.push((idx, !is_ro));
                            }
                            Err(err) => {
                                for (prev_idx, was_write) in started_slots.into_iter().rev() {
                                    let bit = 1u16.checked_shl(prev_idx as u32).unwrap_or(0);
                                    let retain = (ctx.activity.other_read_db_mask & bit) != 0;
                                    if let Some(ref mut pbt) =
                                        ctx.db_set.slots_mut()[prev_idx].btree
                                    {
                                        if was_write {
                                            let _ = pbt.rollback_with_trip(
                                                extended::SQLITE_ABORT_ROLLBACK,
                                                true,
                                                retain,
                                            );
                                        } else if !retain {
                                            let _ = pbt.pager_mut().end_read_if_unused();
                                        }
                                    }
                                }
                                return Err(err);
                            }
                        }
                    }
                    *ctx.explicit_tx = true;
                    *ctx.implicit_savepoint_tx = false;
                    *ctx.in_user_btree_tx = true;
                    ctx.publish();
                }
            }
            Ok(())
        }
        Stmt::Commit => commit_transaction(ctx),
        Stmt::Rollback { savepoint } => {
            if let Some(sp_name) = savepoint {
                let Some(pos) = ctx
                    .savepoints
                    .iter()
                    .rposition(|sp| sp.name.eq_ignore_ascii_case(sp_name))
                else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such savepoint: {sp_name}"),
                    ));
                };
                let sp = ctx.savepoints[pos].clone();
                let schema_changed_since_sp = *ctx.schema_generation != sp.schema_gen_at_open;
                let is_schema_change = *ctx.schema_changed_in_tx || schema_changed_since_sp;
                let write_only = !is_schema_change;

                let mut trip_err: Option<SqliteError> = None;
                for db_slot in ctx.db_set.slots_mut() {
                    if let Some(ref mut bt) = db_slot.btree {
                        if let Err(e) = bt.trip_cursors(extended::SQLITE_ABORT_ROLLBACK, write_only)
                            && trip_err.is_none()
                        {
                            trip_err = Some(e);
                        }
                        if bt.transaction_state() == TransactionState::Write {
                            bt.pager_mut()
                                .rollback_to_subsavepoint(sp.subsavepoint_depth)?;
                        }
                    }
                }
                *ctx.deferred_fk_suspects = sp.deferred_fk_suspects;
                ctx.savepoints.truncate(pos + 1);
                if schema_changed_since_sp {
                    for db_slot in ctx.db_set.slots_mut() {
                        if let Some((_, saved_schema)) = sp
                            .schemas_at_open
                            .iter()
                            .find(|(inst, _)| *inst == db_slot.schema_instance_id)
                        {
                            db_slot.schema = saved_schema.clone();
                        } else if let Some(ref mut bt) = db_slot.btree {
                            db_slot.schema = Schema::load_for_schema(bt, Some(&db_slot.name))?;
                        }
                    }
                    ctx.db_set.sync_temp_trigger_bindings_from_schema();
                }
                if is_schema_change {
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation += 1;
                    *ctx.schema_changed_in_tx = true;
                }
                *ctx.rollback_count += 1;
                ctx.publish();
                if let Some(e) = trip_err {
                    return Err(e);
                }
                Ok(())
            } else {
                if !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "cannot rollback - no transaction is active",
                    ));
                }
                let rollback_res = rollback_all_databases(ctx, true);
                *ctx.rollback_count += 1;
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                ctx.publish();
                rollback_res
            }
        }
        Stmt::Savepoint(name) => {
            if ctx.activity.other_writers > 0 {
                return Err(SqliteError::new(
                    ErrorCode::Busy,
                    "cannot open savepoint - SQL statements in progress",
                ));
            }
            let was_autocommit = is_in_autocommit(ctx);
            let slot_idx = ctx.savepoints.len();
            for db_slot in ctx.db_set.slots_mut() {
                if let Some(ref mut bt) = db_slot.btree
                    && bt.transaction_state() == TransactionState::Write
                {
                    bt.pager_mut().ensure_subsavepoints(slot_idx + 1)?;
                }
            }
            let schemas_at_open = ctx
                .db_set
                .slots()
                .iter()
                .filter(|s| s.btree.is_some())
                .map(|s| (s.schema_instance_id, s.schema.clone()))
                .collect();
            ctx.savepoints.push(SqlSavepoint {
                name: name.clone(),
                subsavepoint_depth: slot_idx,
                is_transaction_savepoint: was_autocommit,
                schemas_at_open,
                schema_gen_at_open: *ctx.schema_generation,
                v1_expire_gen_at_open: *ctx.v1_expire_generation,
                deferred_fk_suspects: ctx.deferred_fk_suspects.clone(),
            });
            if was_autocommit {
                *ctx.implicit_savepoint_tx = true;
                ctx.publish();
            }
            Ok(())
        }
        Stmt::Release(name) => release_savepoint_op(ctx, name),
        _ => unreachable!("execute_control_stmt called on non-control statement"),
    }
}

fn validate_analyze_target_in_set(
    db_set: &DatabaseSet,
    target: Option<&QualifiedName>,
) -> Result<()> {
    let Some(qn) = target else {
        return Ok(());
    };
    if let Some(ref s) = qn.schema {
        let Some(db_id) = db_set.find_db_id(s) else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("unknown database {s}"),
            ));
        };
        if DatabaseSet::is_catalog_table_in_db(db_id, &qn.name) {
            return Ok(());
        }
        if let Some(slot) = db_set.slot(db_id)
            && slot.btree.is_some()
            && (slot.schema.find_user_table(&qn.name).is_some()
                || slot.schema.find_index(&qn.name).is_some())
        {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", qn.display_name()),
        ));
    }
    if db_set.find_db_id(&qn.name).is_some()
        || db_set.find_table_in_set(qn)?.is_some()
        || db_set.find_index_in_set(qn).is_some()
    {
        return Ok(());
    }
    Err(SqliteError::new(
        ErrorCode::Error,
        format!("no such table: {}", qn.name),
    ))
}

/// Executes non-locking pragmas (`LOCK_STATUS`, `JOURNAL_MODE`, `SYNCHRONOUS`, etc.), `ANALYZE`, and `ATTACH`/`DETACH`.
pub(crate) fn execute_nolock_stmt(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    match stmt {
        Stmt::Analyze(target) => {
            validate_analyze_target_in_set(ctx.db_set, target.as_ref())?;
            Ok(QueryOutput::default())
        }
        Stmt::AttachDatabase {
            expr,
            schema_name,
            key,
        } => {
            exec_attach_database(ctx, expr, schema_name, key.as_ref(), params)?;
            Ok(QueryOutput::default())
        }
        Stmt::DetachDatabase { schema_name } => {
            exec_detach_database(ctx, schema_name, params)?;
            Ok(QueryOutput::default())
        }
        Stmt::Pragma {
            schema,
            name,
            value,
        } => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
        _ => unreachable!("execute_nolock_stmt called on non-nolock statement"),
    }
}

/// Executes a `Read` statement body (`SELECT`, `EXPLAIN`, or read `PRAGMA`) lazily opening `Read`
/// transactions on referenced `DatabaseSet` slots without ending them prematurely.
pub(crate) fn execute_read_body(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    match stmt {
        Stmt::Explain { query_plan, stmt } => {
            if *query_plan {
                let rows = explain_query_plan_in_db_set(stmt, ctx.db_set)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "id".to_owned(),
                        "parent".to_owned(),
                        "notused".to_owned(),
                        "detail".to_owned(),
                    ],
                    rows,
                })
            } else {
                let prog = compile_for_explain_in_db_set(stmt, ctx.db_set, ctx.functions)?;
                Ok(QueryOutput {
                    column_names: vec![
                        "addr".to_owned(),
                        "opcode".to_owned(),
                        "p1".to_owned(),
                        "p2".to_owned(),
                        "p3".to_owned(),
                        "p4".to_owned(),
                        "p5".to_owned(),
                        "comment".to_owned(),
                    ],
                    rows: prog.explain_rows(),
                })
            }
        }
        Stmt::Select(sel) => exec_select(ctx, sel, params, &[], &CteStore::default()),
        Stmt::Pragma {
            schema,
            name,
            value,
        } => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
        _ => unreachable!("execute_read_body called on non-read statement"),
    }
}

/// Executes a `Write` statement body (`INSERT`, `UPDATE`, `DELETE`, DDL, `REINDEX`, or header-writing `PRAGMA`)
/// lazily opening `Write` transactions on modified `DatabaseSet` slots without committing or rolling back.
pub(crate) fn execute_write_body(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    match stmt {
        Stmt::Insert(ins) => exec_insert(ctx, ins, params),
        Stmt::Update(upd) => exec_update(ctx, upd, params),
        Stmt::Delete(del) => exec_delete(ctx, del, params),
        Stmt::CreateTable(ct) => {
            exec_create_table(ctx, ct)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateTableAs {
            temp,
            if_not_exists,
            name,
            query,
        } => {
            exec_create_table_as(ctx, *temp, *if_not_exists, name, query, params)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateView(cv) => {
            exec_create_view(ctx, cv)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateTrigger(ct) => {
            exec_create_trigger(ctx, ct)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropTable { if_exists, name } => {
            exec_drop_table(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropView { if_exists, name } => {
            exec_drop_view(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropTrigger { if_exists, name } => {
            exec_drop_trigger(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::CreateIndex(ci) => {
            exec_create_index(ctx, ci)?;
            Ok(QueryOutput::default())
        }
        Stmt::DropIndex { if_exists, name } => {
            exec_drop_index(ctx, *if_exists, name)?;
            Ok(QueryOutput::default())
        }
        Stmt::AlterTable { table, action } => {
            exec_alter_table(ctx, table, action)?;
            Ok(QueryOutput::default())
        }
        Stmt::Reindex(target) => {
            exec_reindex(ctx, target.as_ref())?;
            Ok(QueryOutput::default())
        }
        Stmt::Pragma {
            schema,
            name,
            value,
        } => {
            let db_id = ctx.db_set.resolve_schema_or_err(schema.as_deref())?;
            if db_id == DatabaseId::TEMP {
                let enc = ctx.encoding();
                ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
            }
            let Some(pv) = value.as_ref() else {
                return Ok(QueryOutput::default());
            };
            match name.to_ascii_uppercase().as_str() {
                "USER_VERSION" => {
                    ctx.ensure_write_db(db_id, false)?;
                    let v = pragma_to_i64(pv) as i32;
                    if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                        bt.pager_mut().header_mut()?.user_version = v;
                    }
                    Ok(QueryOutput::default())
                }
                "APPLICATION_ID" => {
                    ctx.ensure_write_db(db_id, false)?;
                    let v = pragma_to_i64(pv) as i32;
                    if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                        bt.pager_mut().header_mut()?.application_id = v;
                    }
                    Ok(QueryOutput::default())
                }
                "SCHEMA_VERSION" => {
                    ctx.ensure_write_db(db_id, false)?;
                    let v = pragma_to_i64(pv) as u32;
                    let slot = ctx.db_set.slot_mut(db_id).unwrap();
                    let slot_name = slot.name.clone();
                    if let Some(ref mut bt) = slot.btree {
                        bt.pager_mut().header_mut()?.schema_cookie = v;
                        slot.schema = Schema::load_for_schema(bt, Some(&slot_name))?;
                    }
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation += 1;
                    *ctx.schema_changed_in_tx = true;
                    Ok(QueryOutput::default())
                }
                "DEFAULT_CACHE_SIZE" => {
                    if db_id == DatabaseId::TEMP {
                        return Ok(QueryOutput::default());
                    }
                    ctx.ensure_write_db(db_id, false)?;
                    let size = pragma_to_i64(pv).saturating_abs().min(i64::from(i32::MAX)) as i32;
                    if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                        bt.pager_mut().header_mut()?.default_cache_size = size;
                        bt.set_cache_size(size);
                    }
                    Ok(QueryOutput::default())
                }
                "AUTO_VACUUM" => {
                    let e_auto = parse_auto_vacuum_pragma(pv);
                    let slot = ctx.db_set.slot_mut(db_id).unwrap();
                    slot.next_auto_vacuum = Some(e_auto);
                    let has_av_on_disk = slot
                        .btree
                        .as_ref()
                        .is_some_and(|bt| bt.pager().header().largest_root_btree_page != 0);
                    if has_av_on_disk {
                        ctx.ensure_write_db(db_id, true)?;
                        let incr_flag = u32::from(e_auto == AutoVacuumMode::Incremental);
                        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                            bt.pager_mut().header_mut()?.incremental_vacuum_mode = incr_flag;
                            let _ = bt.set_configured_auto_vacuum(e_auto);
                        }
                    }
                    Ok(QueryOutput::default())
                }
                _ => exec_pragma(ctx, schema.as_deref(), name, value.as_ref()),
            }
        }
        _ => unreachable!("execute_write_body called on non-write statement"),
    }
}

/// Top-level statement dispatcher for all SQL statements (`SELECT`, DML, DDL, `EXPLAIN`, `PRAGMA`, transactions).
pub fn execute_statement(
    ctx: &mut ExecContext<'_>,
    stmt: &Stmt,
    params: &[Value],
) -> Result<QueryOutput> {
    if stmt_loads_schema_on_prepare(stmt) {
        for slot in ctx.db_set.slots_mut() {
            let slot_name = slot.name.clone();
            if let Some(ref mut bt) = slot.btree
                && bt.transaction_state() != TransactionState::Open
            {
                slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name)?;
            }
        }
    }

    let out = match classify_stmt_category(stmt) {
        StmtCategory::Control => {
            execute_control_stmt(ctx, stmt)?;
            Ok(QueryOutput::default())
        }
        StmtCategory::NoLockPragma => execute_nolock_stmt(ctx, stmt, params),
        StmtCategory::Vacuum => {
            let Stmt::Vacuum { schema, into } = stmt else {
                unreachable!();
            };
            exec_vacuum(ctx, schema.as_deref(), into.as_ref(), params)
        }
        StmtCategory::IncrementalVacuum => {
            let Stmt::Pragma { schema, value, .. } = stmt else {
                unreachable!();
            };
            let db_id = ctx.db_set.resolve_schema_or_err(schema.as_deref())?;
            if db_id == DatabaseId::TEMP {
                let enc = ctx.encoding();
                ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
            }
            let mut remaining = parse_incremental_vacuum_limit(value.as_ref());
            run_write_autocommit(ctx, |ctx| {
                ctx.ensure_write_db(db_id, true)?;
                let mut rows = Vec::new();
                while remaining > 0 {
                    remaining -= 1;
                    let stepped =
                        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                            bt.incremental_vacuum_step()?
                        } else {
                            false
                        };
                    if stepped {
                        rows.push(Vec::new());
                    } else {
                        break;
                    }
                }
                Ok(QueryOutput {
                    column_names: Vec::new(),
                    rows,
                })
            })
        }
        StmtCategory::Read => run_read_autocommit(ctx, |ctx| execute_read_body(ctx, stmt, params)),
        StmtCategory::Write => {
            run_write_autocommit(ctx, |ctx| execute_write_body(ctx, stmt, params))
        }
    }?;

    maybe_run_wal_autocheckpoint_in_ctx(ctx);
    Ok(out)
}

pub(crate) fn maybe_run_wal_autocheckpoint_in_ctx(ctx: &mut ExecContext<'_>) {
    if !is_in_autocommit(ctx) {
        return;
    }
    let threshold = *ctx.wal_autocheckpoint_threshold;
    for (idx, slot) in ctx.db_set.slots_mut().iter_mut().enumerate() {
        let Some(ref mut bt) = slot.btree else {
            continue;
        };
        let n_entry = bt.pager_mut().take_wal_callback_frame_count();
        let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
        if n_entry > 0
            && threshold > 0
            && i64::from(n_entry) >= i64::from(threshold)
            && bt.transaction_state() == TransactionState::Open
            && ((ctx.activity.other_read_db_mask | ctx.activity.other_write_db_mask) & bit) == 0
        {
            let _ = bt.wal_checkpoint(WalCheckpointMode::Passive, None);
        }
    }
}

fn is_in_autocommit(ctx: &ExecContext<'_>) -> bool {
    !*ctx.explicit_tx && !*ctx.implicit_savepoint_tx
}

pub(crate) fn end_autocommit_reads_started_by_stmt(ctx: &mut ExecContext<'_>) {
    if *ctx.in_user_btree_tx || !is_in_autocommit(ctx) {
        return;
    }
    let started = ctx.stmt_tx.started_read_mask;
    if started == 0 {
        return;
    }
    for idx in 0..ctx.db_set.slot_count() {
        let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
        if (started & bit) != 0
            && ((ctx.activity.other_read_db_mask | ctx.activity.other_write_db_mask) & bit) == 0
            && let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree
            && bt.transaction_state() == TransactionState::Read
        {
            let _ = bt.pager_mut().end_read_if_unused();
        }
    }
}

pub(crate) fn run_read_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    *ctx.stmt_tx = StmtTxTracker::default();
    let res = f(ctx);
    end_autocommit_reads_started_by_stmt(ctx);
    res
}

pub(crate) fn run_write_autocommit<F>(ctx: &mut ExecContext<'_>, f: F) -> Result<QueryOutput>
where
    F: FnOnce(&mut ExecContext<'_>) -> Result<QueryOutput>,
{
    let autocommit = is_in_autocommit(ctx);
    let need_stmt_sp = !autocommit || ctx.activity.other_writers > 0;
    let sql_sp_depth = ctx.savepoints.len();
    let stmt_sp_slot = need_stmt_sp.then_some(sql_sp_depth);
    *ctx.stmt_tx = StmtTxTracker {
        stmt_sp_slot,
        ..StmtTxTracker::default()
    };

    let prev_total_changes = ctx.runtime.total_changes;
    let prev_last_rowid = ctx.runtime.last_insert_rowid;
    let prev_deferred = ctx.deferred_fk_suspects.clone();
    let prev_schemas: Vec<(SchemaInstanceId, Schema)> = ctx
        .db_set
        .slots()
        .iter()
        .filter(|s| s.btree.is_some())
        .map(|s| (s.schema_instance_id, s.schema.clone()))
        .collect();
    let prev_schema_gen = *ctx.schema_generation;
    let prev_v1_gen = *ctx.v1_expire_generation;
    let prev_schema_changed = *ctx.schema_changed_in_tx;

    let step_res = f(ctx);

    let restore_prev_schemas = |db_set: &mut DatabaseSet, prev: &[(SchemaInstanceId, Schema)]| {
        for slot in db_set.slots_mut() {
            if let Some((_, saved)) = prev
                .iter()
                .find(|(inst, _)| *inst == slot.schema_instance_id)
            {
                slot.schema = saved.clone();
            } else if let Some(ref mut bt) = slot.btree
                && let Ok(fresh) = Schema::load_for_schema(bt, Some(&slot.name))
            {
                slot.schema = fresh;
            }
        }
    };

    match step_res {
        Ok(out) => {
            if let Some(sp_slot) = stmt_sp_slot {
                let touched = ctx.stmt_tx.touched_write_mask;
                for idx in 0..ctx.db_set.slot_count() {
                    let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
                    if (touched & bit) != 0
                        && let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree
                        && bt.transaction_state() == TransactionState::Write
                    {
                        bt.pager_mut().release_subsavepoint(sp_slot)?;
                    }
                }
            }
            if autocommit && ctx.activity.other_writers == 0 {
                if let Err((commit_err, disp)) = commit_all_databases(ctx) {
                    if disp == CommitFailureDisposition::LeaveOpen {
                        let _ = rollback_all_databases(ctx, false);
                    }
                    *ctx.schema_changed_in_tx = false;
                    ctx.deferred_fk_suspects.clear();
                    ctx.runtime.total_changes = prev_total_changes;
                    ctx.runtime.last_changes = 0;
                    ctx.runtime.last_insert_rowid = prev_last_rowid;
                    if *ctx.schema_generation != prev_schema_gen {
                        restore_prev_schemas(ctx.db_set, &prev_schemas);
                        *ctx.schema_generation += 1;
                        *ctx.v1_expire_generation += 1;
                    }
                    ctx.publish();
                    return Err(commit_err);
                }
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
            } else if ctx.activity.other_writers == 0 && ctx.activity.other_readers == 0 {
                for slot in ctx.db_set.slots_mut() {
                    if let Some(ref mut bt) = slot.btree
                        && bt.transaction_state() == TransactionState::Write
                    {
                        bt.spill_if_over_cache_size()?;
                    }
                }
            }
            ctx.publish();
            Ok(out)
        }
        Err(err) => {
            if err.conflict_effect == ConflictResolutionEffect::Fail
                && err.code == ErrorCode::Constraint
            {
                if let Some(sp_slot) = stmt_sp_slot {
                    let touched = ctx.stmt_tx.touched_write_mask;
                    for idx in 0..ctx.db_set.slot_count() {
                        let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
                        if (touched & bit) != 0
                            && let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree
                            && bt.transaction_state() == TransactionState::Write
                        {
                            bt.pager_mut().release_subsavepoint(sp_slot)?;
                        }
                    }
                }
                if autocommit && ctx.activity.other_writers == 0 {
                    if let Err((_, disp)) = commit_all_databases(ctx) {
                        if disp == CommitFailureDisposition::LeaveOpen {
                            let _ = rollback_all_databases(ctx, false);
                        }
                        *ctx.explicit_tx = false;
                        *ctx.implicit_savepoint_tx = false;
                        *ctx.in_user_btree_tx = false;
                        ctx.savepoints.clear();
                        *ctx.schema_changed_in_tx = false;
                        ctx.deferred_fk_suspects.clear();
                        ctx.runtime.total_changes = prev_total_changes;
                        ctx.runtime.last_changes = 0;
                        ctx.runtime.last_insert_rowid = prev_last_rowid;
                        if *ctx.schema_generation != prev_schema_gen {
                            restore_prev_schemas(ctx.db_set, &prev_schemas);
                            *ctx.schema_generation += 1;
                            *ctx.v1_expire_generation += 1;
                        }
                        ctx.publish();
                        return Err(err);
                    }
                    *ctx.schema_changed_in_tx = false;
                    ctx.deferred_fk_suspects.clear();
                }
                ctx.publish();
                Err(err)
            } else if err.conflict_effect == ConflictResolutionEffect::Rollback
                || (autocommit && ctx.activity.other_writers == 0)
                || err.code == ErrorCode::Full
            {
                let _ = rollback_all_databases(ctx, false);
                if !autocommit {
                    *ctx.rollback_count += 1;
                }
                *ctx.explicit_tx = false;
                *ctx.implicit_savepoint_tx = false;
                *ctx.in_user_btree_tx = false;
                ctx.savepoints.clear();
                *ctx.schema_changed_in_tx = false;
                ctx.deferred_fk_suspects.clear();
                ctx.runtime.total_changes = prev_total_changes;
                ctx.runtime.last_changes = 0;
                ctx.runtime.last_insert_rowid = prev_last_rowid;
                if *ctx.schema_generation != prev_schema_gen {
                    restore_prev_schemas(ctx.db_set, &prev_schemas);
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation += 1;
                }
                ctx.publish();
                Err(err)
            } else {
                let stmt_changed_schema = *ctx.schema_generation != prev_schema_gen;
                let touched = ctx.stmt_tx.touched_write_mask;
                for idx in 0..ctx.db_set.slot_count() {
                    let bit = 1u16.checked_shl(idx as u32).unwrap_or(0);
                    if (touched & bit) != 0
                        && let Some(ref mut bt) = ctx.db_set.slots_mut()[idx].btree
                    {
                        let _ =
                            bt.trip_cursors(extended::SQLITE_ABORT_ROLLBACK, !stmt_changed_schema);
                        if let Some(sp_slot) = stmt_sp_slot
                            && bt.transaction_state() == TransactionState::Write
                        {
                            let _ = bt.pager_mut().rollback_to_subsavepoint(sp_slot);
                            let _ = bt.pager_mut().release_subsavepoint(sp_slot);
                        }
                    }
                }
                *ctx.deferred_fk_suspects = prev_deferred;
                ctx.runtime.total_changes = prev_total_changes;
                ctx.runtime.last_changes = 0;
                ctx.runtime.last_insert_rowid = prev_last_rowid;
                if stmt_changed_schema {
                    restore_prev_schemas(ctx.db_set, &prev_schemas);
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation = prev_v1_gen + 1;
                    *ctx.schema_changed_in_tx = prev_schema_changed;
                }
                ctx.publish();
                Err(err)
            }
        }
    }
}

#[inline]
fn conflict_action_to_effect(action: ConflictAction) -> ConflictResolutionEffect {
    match action {
        ConflictAction::Rollback => ConflictResolutionEffect::Rollback,
        ConflictAction::Fail => ConflictResolutionEffect::Fail,
        ConflictAction::Abort | ConflictAction::Ignore | ConflictAction::Replace => {
            ConflictResolutionEffect::Abort
        }
    }
}

// ============================================================================
// Relational SELECT Engine (Joins, CTEs, Aggregates, Compounds, Subqueries)
// ============================================================================

fn exec_select(
    ctx: &mut ExecContext<'_>,
    stmt: &SelectStmt,
    params: &[Value],
    outer_scopes: &[EvalRow],
    parent_ctes: &CteStore,
) -> Result<QueryOutput> {
    let mut ctes = parent_ctes.with_scope_at(&stmt.ctes, outer_scopes.len());

    // Propagate outer `LIMIT [OFFSET]` cap when `SELECT ... FROM <cte> [WHERE ...] LIMIT k` has no
    // sorting/grouping and `<cte>` is not `AS MATERIALIZED`, allowing unbounded recursive CTEs to
    // stop as soon as `k` matching rows are produced.
    if stmt.body.compounds.is_empty()
        && stmt.order_by.is_empty()
        && let Some(ref lim) = stmt.limit
        && let SelectCore::Select {
            distinct: false,
            projection,
            from: Some(fc),
            selection,
            group_by,
            having: None,
        } = &stmt.body.head
        && group_by.is_empty()
        && fc.joins.is_empty()
        && !projection.iter().any(|rc| match rc {
            ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
            _ => false,
        })
        && let TableOrSubquery::Table {
            name,
            table_args: None,
            alias,
            indexed_by: None,
        } = &fc.head
        && name.schema.is_none()
        && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
        && ctes.scopes[s_idx][c_idx].materialized != Some(true)
        && count_table_refs_in_core(&stmt.body.head, &name.name) == 1
    {
        let lim_val =
            eval_expr(ctx, &lim.limit, params, outer_scopes, &ctes, None)?.to_integer_lossy();
        if lim_val >= 0 {
            let off_val = match &lim.offset {
                Some(oe) => eval_expr(ctx, oe, params, outer_scopes, &ctes, None)?
                    .to_integer_lossy()
                    .max(0) as usize,
                None => 0,
            };
            ctes.limit_hint = Some(CteStreamHint {
                s_idx,
                c_idx,
                eff_alias: alias.clone().unwrap_or_else(|| name.name.clone()),
                where_expr: selection.clone(),
                max_kept: off_val.saturating_add(lim_val as usize),
            });
        }
    }

    if stmt.body.compounds.is_empty() {
        exec_single_select_core(
            ctx,
            &stmt.body.head,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    } else {
        exec_compound_select(
            ctx,
            &stmt.body,
            &stmt.order_by,
            stmt.limit.as_ref(),
            params,
            outer_scopes,
            &ctes,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CteRecursionKind {
    NonRecursive,
    Recursive { first_rec_idx: usize },
    InvalidCircular,
    MultipleFromRefs,
    MultipleRecursive,
}

fn count_table_refs_in_select(stmt: &SelectStmt, target: &str) -> usize {
    if stmt
        .ctes
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(target))
    {
        return 0;
    }
    let mut n = 0;
    for cte in &stmt.ctes {
        n += count_table_refs_in_select(&cte.query, target);
    }
    n += count_table_refs_in_core(&stmt.body.head, target);
    for (_, rhs) in &stmt.body.compounds {
        n += count_table_refs_in_core(rhs, target);
    }
    for ob in &stmt.order_by {
        n += count_table_refs_in_expr(&ob.expr, target);
    }
    if let Some(ref lim) = stmt.limit {
        n += count_table_refs_in_expr(&lim.limit, target);
        if let Some(ref off) = lim.offset {
            n += count_table_refs_in_expr(off, target);
        }
    }
    n
}

fn count_table_refs_in_core(core: &SelectCore, target: &str) -> usize {
    match core {
        SelectCore::Values(rows) => rows
            .iter()
            .flat_map(|r| r.iter())
            .map(|e| count_table_refs_in_expr(e, target))
            .sum(),
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let mut n = 0;
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    n += count_table_refs_in_expr(expr, target);
                }
            }
            if let Some(fc) = from {
                n += count_table_refs_in_tos(&fc.head, target);
                for j in &fc.joins {
                    n += count_table_refs_in_tos(&j.right, target);
                    if let Some(JoinConstraint::On(ref e)) = j.constraint {
                        n += count_table_refs_in_expr(e, target);
                    }
                }
            }
            if let Some(w) = selection {
                n += count_table_refs_in_expr(w, target);
            }
            for g in group_by {
                n += count_table_refs_in_expr(g, target);
            }
            if let Some(h) = having {
                n += count_table_refs_in_expr(h, target);
            }
            n
        }
    }
}

fn count_table_refs_in_tos(tos: &TableOrSubquery, target: &str) -> usize {
    match tos {
        TableOrSubquery::Table { name, .. } => {
            usize::from(name.schema.is_none() && name.name.eq_ignore_ascii_case(target))
        }
        TableOrSubquery::Subquery { query, .. } => count_table_refs_in_select(query, target),
    }
}

fn count_table_refs_in_expr(expr: &Expr, target: &str) -> usize {
    match expr {
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::IsNull { expr: e, .. }
        | Expr::Cast { expr: e, .. } => count_table_refs_in_expr(e, target),
        Expr::RowValue(es) => es.iter().map(|e| count_table_refs_in_expr(e, target)).sum(),
        Expr::Binary { left, right, .. } => {
            count_table_refs_in_expr(left, target) + count_table_refs_in_expr(right, target)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            count_table_refs_in_expr(e, target)
                + count_table_refs_in_expr(low, target)
                + count_table_refs_in_expr(high, target)
        }
        Expr::InList { expr: e, list, .. } => {
            count_table_refs_in_expr(e, target)
                + list
                    .iter()
                    .map(|x| count_table_refs_in_expr(x, target))
                    .sum::<usize>()
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => count_table_refs_in_expr(e, target) + count_table_refs_in_select(subquery, target),
        Expr::InTable { expr: e, table, .. } => {
            count_table_refs_in_expr(e, target)
                + usize::from(table.schema.is_none() && table.name.eq_ignore_ascii_case(target))
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            count_table_refs_in_expr(e, target)
                + count_table_refs_in_expr(pattern, target)
                + escape
                    .as_ref()
                    .map_or(0, |esc| count_table_refs_in_expr(esc, target))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_ref()
                .map_or(0, |o| count_table_refs_in_expr(o, target))
                + branches
                    .iter()
                    .map(|(w, t)| {
                        count_table_refs_in_expr(w, target) + count_table_refs_in_expr(t, target)
                    })
                    .sum::<usize>()
                + else_expr
                    .as_ref()
                    .map_or(0, |el| count_table_refs_in_expr(el, target))
        }
        Expr::FunctionCall { args, filter, .. } => {
            let a_cnt = match args {
                FunctionArgs::Star => 0,
                FunctionArgs::Exprs(es) => {
                    es.iter().map(|e| count_table_refs_in_expr(e, target)).sum()
                }
            };
            a_cnt
                + filter
                    .as_ref()
                    .map_or(0, |f| count_table_refs_in_expr(f, target))
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            count_table_refs_in_select(sub, target)
        }
        _ => 0,
    }
}

fn classify_cte_recursion(cte: &CommonTableExpr) -> CteRecursionKind {
    let total_refs = count_table_refs_in_select(&cte.query, &cte.name);
    if total_refs == 0 {
        return CteRecursionKind::NonRecursive;
    }
    if cte.query.body.compounds.is_empty() {
        return CteRecursionKind::InvalidCircular;
    }
    // Nested CTEs, initial `head` arm, `ORDER BY`, and `LIMIT` must never reference `cte.name`.
    for inner_cte in &cte.query.ctes {
        if inner_cte.name.eq_ignore_ascii_case(&cte.name) {
            break;
        }
        if count_table_refs_in_select(&inner_cte.query, &cte.name) > 0 {
            return CteRecursionKind::InvalidCircular;
        }
    }
    if count_table_refs_in_core(&cte.query.body.head, &cte.name) > 0 {
        return CteRecursionKind::InvalidCircular;
    }
    for ob in &cte.query.order_by {
        if count_table_refs_in_expr(&ob.expr, &cte.name) > 0 {
            return CteRecursionKind::InvalidCircular;
        }
    }
    if let Some(ref lim) = cte.query.limit {
        if count_table_refs_in_expr(&lim.limit, &cte.name) > 0 {
            return CteRecursionKind::InvalidCircular;
        }
        if let Some(ref off) = lim.offset
            && count_table_refs_in_expr(off, &cte.name) > 0
        {
            return CteRecursionKind::InvalidCircular;
        }
    }

    let Some(first_rec_idx) = cte
        .query
        .body
        .compounds
        .iter()
        .position(|(_, rhs)| count_table_refs_in_core(rhs, &cte.name) > 0)
    else {
        return CteRecursionKind::InvalidCircular;
    };

    let expected_op = cte.query.body.compounds[first_rec_idx].0;
    if !matches!(expected_op, CompoundOp::Union | CompoundOp::UnionAll) {
        return CteRecursionKind::InvalidCircular;
    }

    for (op, rhs_core) in &cte.query.body.compounds[first_rec_idx..] {
        if *op != expected_op {
            return CteRecursionKind::InvalidCircular;
        }
        let rhs_total = count_table_refs_in_core(rhs_core, &cte.name);
        if rhs_total == 0 {
            return CteRecursionKind::InvalidCircular;
        }
        let mut direct_from_refs = 0usize;
        if let SelectCore::Select { from: Some(fc), .. } = rhs_core {
            if matches!(&fc.head, TableOrSubquery::Table { name, .. } if name.schema.is_none() && name.name.eq_ignore_ascii_case(&cte.name))
            {
                direct_from_refs += 1;
            }
            for j in &fc.joins {
                if matches!(&j.right, TableOrSubquery::Table { name, .. } if name.schema.is_none() && name.name.eq_ignore_ascii_case(&cte.name))
                {
                    direct_from_refs += 1;
                }
            }
        }
        if direct_from_refs == 0 {
            return CteRecursionKind::InvalidCircular;
        }
        if direct_from_refs > 1 {
            return CteRecursionKind::MultipleFromRefs;
        }
        if rhs_total > 1 {
            return CteRecursionKind::MultipleRecursive;
        }
    }

    CteRecursionKind::Recursive { first_rec_idx }
}

fn check_recursive_arm_no_aggregates(rec_core: &SelectCore) -> Result<()> {
    if let SelectCore::Select {
        projection,
        selection,
        group_by,
        having,
        ..
    } = rec_core
    {
        let has_agg = !group_by.is_empty()
            || having.is_some()
            || projection.iter().any(|rc| match rc {
                ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                _ => false,
            })
            || selection.as_ref().is_some_and(expr_contains_aggregate);
        if has_agg {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "recursive aggregate queries not supported",
            ));
        }
    }
    Ok(())
}

#[derive(Clone)]
struct DedupRow {
    vals: Vec<Value>,
    specs: Rc<[KeySortOrder]>,
    enc: TextEncoding,
}

impl PartialEq for DedupRow {
    fn eq(&self, other: &Self) -> bool {
        compare_value_slices(&self.vals, &other.vals, &self.specs, self.enc) == Ordering::Equal
    }
}

impl Eq for DedupRow {}

impl PartialOrd for DedupRow {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DedupRow {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_value_slices(&self.vals, &other.vals, &self.specs, self.enc)
    }
}

fn resolve_cte(
    ctx: &mut ExecContext<'_>,
    ctes: &CteStore,
    name: &str,
    params: &[Value],
    outer_scopes: &[EvalRow],
) -> Result<Option<QueryOutput>> {
    let Some((s_idx, c_idx)) = ctes.locate(name) else {
        return Ok(None);
    };
    if let Some(working_out) = ctes.find_working(s_idx, c_idx) {
        return Ok(Some(working_out.clone()));
    }
    let cte = ctes.scopes[s_idx][c_idx].clone();
    if ctes.resolving.contains(&(s_idx, c_idx)) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        ));
    }

    let outer_stream_hint = match &ctes.limit_hint {
        Some(h) if h.s_idx == s_idx && h.c_idx == c_idx => Some(h.clone()),
        _ => None,
    };

    // Materialize once per lexical `WITH` scope activation unless `AS NOT MATERIALIZED` (`Some(false)`)
    // or a transient outer `LIMIT` hint is active.
    let use_cache = outer_stream_hint.is_none() && cte.materialized != Some(false);
    if use_cache
        && let Some(cached) = ctes
            .scope_caches
            .get(s_idx)
            .and_then(|rc| rc.borrow().get(c_idx).cloned().flatten())
    {
        return Ok(Some(cached));
    }

    let lexical_depth = ctes
        .scope_outer_depths
        .get(s_idx)
        .copied()
        .unwrap_or(outer_scopes.len())
        .min(outer_scopes.len());
    let lexical_outer = &outer_scopes[..lexical_depth];

    let nested = ctes.enter_cte_select(ctx.db_set.expr_depth_limit())?;
    let mut eval_ctes = nested.slice_to_scope(s_idx);
    eval_ctes.resolving = ctes.resolving.clone();
    eval_ctes.resolving.push((s_idx, c_idx));

    let mut cte_out = match classify_cte_recursion(&cte) {
        CteRecursionKind::NonRecursive => {
            if let Some(ref explicit_cols) = cte.columns {
                let mut visited = Vec::new();
                check_cte_explicit_head_width_static(
                    &cte.name,
                    explicit_cols,
                    &cte.query,
                    lexical_outer,
                    ctx.db_set,
                    &eval_ctes,
                    ctx.functions,
                    &mut visited,
                )?;
            }
            exec_select(ctx, &cte.query, params, lexical_outer, &eval_ctes)?
        }
        CteRecursionKind::InvalidCircular => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("circular reference: {}", cte.name),
            ));
        }
        CteRecursionKind::MultipleFromRefs => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("multiple references to recursive table: {}", cte.name),
            ));
        }
        CteRecursionKind::MultipleRecursive => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("multiple recursive references: {}", cte.name),
            ));
        }
        CteRecursionKind::Recursive { first_rec_idx } => {
            if let Some(ref explicit_cols) = cte.columns {
                let seed_stmt = SelectStmt {
                    ctes: cte.query.ctes.clone(),
                    body: SelectBody {
                        head: cte.query.body.head.clone(),
                        compounds: cte.query.body.compounds[..first_rec_idx].to_vec(),
                    },
                    order_by: Vec::new(),
                    limit: None,
                };
                let mut visited = Vec::new();
                check_cte_explicit_head_width_static(
                    &cte.name,
                    explicit_cols,
                    &seed_stmt,
                    lexical_outer,
                    ctx.db_set,
                    &eval_ctes,
                    ctx.functions,
                    &mut visited,
                )?;
            }
            exec_recursive_cte(
                ctx,
                s_idx,
                c_idx,
                &cte,
                first_rec_idx,
                outer_stream_hint,
                outer_scopes,
                ctes,
                params,
                lexical_outer,
                &mut eval_ctes,
            )?
        }
    };

    if let Some(explicit_cols) = &cte.columns {
        if explicit_cols.len() != cte_out.column_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "table {} has {} values for {} columns",
                    cte.name,
                    cte_out.column_names.len(),
                    explicit_cols.len()
                ),
            ));
        }
        cte_out.column_names = disambiguate_view_column_names(explicit_cols);
    } else {
        cte_out.column_names = disambiguate_view_column_names(&cte_out.column_names);
    }

    if use_cache && let Some(cache_rc) = ctes.scope_caches.get(s_idx) {
        let mut slots = cache_rc.borrow_mut();
        if let Some(slot) = slots.get_mut(c_idx) {
            *slot = Some(cte_out.clone());
        }
    }

    Ok(Some(cte_out))
}

#[allow(clippy::too_many_arguments)]
fn exec_recursive_cte(
    ctx: &mut ExecContext<'_>,
    s_idx: usize,
    c_idx: usize,
    cte: &CommonTableExpr,
    first_rec_idx: usize,
    outer_stream_hint: Option<CteStreamHint>,
    caller_outer_scopes: &[EvalRow],
    caller_ctes: &CteStore,
    params: &[Value],
    outer_scopes: &[EvalRow],
    eval_ctes: &mut CteStore,
) -> Result<QueryOutput> {
    let rec_ctes = eval_ctes.with_scope_at(&cte.query.ctes, outer_scopes.len());
    let rec_op = cte.query.body.compounds[first_rec_idx].0;
    let is_union_distinct = rec_op == CompoundOp::Union;

    for (_, rec_core) in &cte.query.body.compounds[first_rec_idx..] {
        check_recursive_arm_no_aggregates(rec_core)?;
    }

    // 1. Evaluate non-recursive seed query (`head` + any preceding non-recursive compound arms)
    let seed_body = SelectBody {
        head: cte.query.body.head.clone(),
        compounds: cte.query.body.compounds[..first_rec_idx].to_vec(),
    };
    let mut all_arm_col_names = Vec::with_capacity(1 + cte.query.body.compounds.len());
    let seed_out = if seed_body.compounds.is_empty() {
        let out = exec_single_select_core(
            ctx,
            &seed_body.head,
            &[],
            None,
            params,
            outer_scopes,
            &rec_ctes,
        )?;
        all_arm_col_names.push(out.column_names.clone());
        out
    } else {
        // Collect per-arm column names across seed compound arms for compound `ORDER BY` matching
        let h_out = exec_single_select_core(
            ctx,
            &seed_body.head,
            &[],
            None,
            params,
            outer_scopes,
            &rec_ctes,
        )?;
        all_arm_col_names.push(h_out.column_names);
        for (_, sc) in &seed_body.compounds {
            all_arm_col_names.push(static_select_col_names(
                &SelectBody {
                    head: sc.clone(),
                    compounds: Vec::new(),
                },
                outer_scopes,
                ctx.db_set,
                &rec_ctes,
                &mut Vec::new(),
            ));
        }
        exec_compound_select(ctx, &seed_body, &[], None, params, outer_scopes, &rec_ctes)?
    };

    let head_col_names = seed_out.column_names;
    let working_col_names = match &cte.columns {
        Some(explicit) => {
            if explicit.len() != head_col_names.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} values for {} columns",
                        cte.name,
                        head_col_names.len(),
                        explicit.len()
                    ),
                ));
            }
            disambiguate_view_column_names(explicit)
        }
        None => disambiguate_view_column_names(&head_col_names),
    };

    // Validate recursive arm(s) column counts and collect arm column names even if seed returns 0 rows!
    let mut dummy_working_ctes = rec_ctes.clone();
    dummy_working_ctes.working.push((
        s_idx,
        c_idx,
        QueryOutput {
            column_names: working_col_names.clone(),
            rows: Vec::new(),
        },
    ));
    for (op, rec_core) in &cte.query.body.compounds[first_rec_idx..] {
        let dry_rhs = exec_single_select_core(
            ctx,
            rec_core,
            &[],
            None,
            params,
            outer_scopes,
            &dummy_working_ctes,
        )?;
        if dry_rhs.column_names.len() != working_col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    op.as_sql_str()
                ),
            ));
        }
        all_arm_col_names.push(dry_rhs.column_names);
    }

    // Infer compound result collations across seed + recursive arms (`select.c` compound collation rule)
    let body_meta = infer_select_body_meta(
        &cte.query.body,
        outer_scopes,
        ctx.db_set,
        &dummy_working_ctes,
    );
    let col_collations: Vec<Collation> = (0..working_col_names.len())
        .map(|i| body_meta.get(i).map_or(Collation::Binary, |(_, c)| *c))
        .collect();
    let dedup_specs: Rc<[KeySortOrder]> = col_collations
        .iter()
        .map(|&coll| KeySortOrder {
            desc: false,
            nulls_last: false,
            collation: coll,
        })
        .collect::<Vec<_>>()
        .into();

    // Validate & resolve optional compound `ORDER BY` specs BEFORE checking `LIMIT 0`!
    let order_specs = if cte.query.order_by.is_empty() {
        Vec::new()
    } else {
        for ob in &cte.query.order_by {
            if let Expr::Collate { collation, .. } = &ob.expr
                && !ctx.collations.has_collation(collation)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such collation sequence: {collation}"),
                ));
            }
        }
        resolve_compound_order_by_specs(&all_arm_col_names, &col_collations, &cte.query.order_by)?
    };

    // Evaluate optional CTE-level `LIMIT` / `OFFSET`
    let (cte_limit_opt, cte_offset) = if let Some(ref lim) = cte.query.limit {
        let lv =
            eval_expr(ctx, &lim.limit, params, outer_scopes, &rec_ctes, None)?.to_integer_lossy();
        let ov = match &lim.offset {
            Some(oe) => eval_expr(ctx, oe, params, outer_scopes, &rec_ctes, None)?
                .to_integer_lossy()
                .max(0) as usize,
            None => 0,
        };
        if lv < 0 {
            (None, ov)
        } else {
            (Some(lv as usize), ov)
        }
    } else {
        (None, 0usize)
    };

    if cte_limit_opt == Some(0)
        || outer_stream_hint
            .as_ref()
            .is_some_and(|sh| sh.max_kept == 0)
    {
        return Ok(QueryOutput {
            column_names: working_col_names,
            rows: Vec::new(),
        });
    }

    let stream_hint_cols: Option<Rc<[ScopedColumn]>> = outer_stream_hint.as_ref().map(|sh| {
        working_col_names
            .iter()
            .enumerate()
            .map(|(i, cn)| {
                let (aff, coll) = body_meta
                    .get(i)
                    .copied()
                    .unwrap_or((None, Collation::Binary));
                ScopedColumn {
                    schema_name: None,
                    table_alias: Some(sh.eff_alias.clone()),
                    name: cn.clone(),
                    collation: coll,
                    affinity: aff,
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                }
            })
            .collect::<Vec<_>>()
            .into()
    });
    let mut outer_where_ctes = if outer_stream_hint
        .as_ref()
        .is_some_and(|sh| sh.where_expr.is_some())
    {
        let mut c = caller_ctes.clone();
        c.limit_hint = None;
        Some(c)
    } else {
        None
    };

    let enc = ctx.encoding();
    let mut seen: BTreeSet<DedupRow> = BTreeSet::new();
    let mut queue: VecDeque<Vec<Value>> = VecDeque::new();

    let enqueue_row = |queue: &mut VecDeque<Vec<Value>>,
                       seen: &mut BTreeSet<DedupRow>,
                       row: Vec<Value>,
                       collations: &CollationRegistry| {
        if is_union_distinct
            && !seen.insert(DedupRow {
                vals: row.clone(),
                specs: Rc::clone(&dedup_specs),
                enc,
            })
        {
            return;
        }
        if order_specs.is_empty() {
            queue.push_back(row);
        } else {
            let pos = queue
                .iter()
                .position(|existing| {
                    compare_compound_order_rows(&row, existing, &order_specs, collations, enc)
                        == Ordering::Less
                })
                .unwrap_or(queue.len());
            queue.insert(pos, row);
        }
    };

    for r in seed_out.rows {
        ctx.step_tick()?;
        enqueue_row(&mut queue, &mut seen, r, ctx.collations);
    }

    let mut out_rows = Vec::new();
    let mut skipped = 0usize;
    let mut outer_kept = 0usize;

    while let Some(cur_row) = queue.pop_front() {
        ctx.step_tick()?;

        if skipped < cte_offset {
            skipped += 1;
        } else {
            out_rows.push(cur_row.clone());
            if let Some(cl) = cte_limit_opt
                && out_rows.len() >= cl
            {
                break;
            }
            if let Some(ref sh) = outer_stream_hint {
                if let (Some(we), Some(cols), Some(ow_ctes)) = (
                    sh.where_expr.as_ref(),
                    stream_hint_cols.as_ref(),
                    outer_where_ctes.as_mut(),
                ) {
                    let erow = EvalRow::new(Rc::clone(cols), cur_row.clone());
                    let sc = push_scope(caller_outer_scopes, &erow);
                    if eval_expr(ctx, we, params, &sc, ow_ctes, None)?.truthiness() == Some(true) {
                        outer_kept += 1;
                    }
                } else {
                    outer_kept += 1;
                }
                if outer_kept >= sh.max_kept {
                    break;
                }
            }
        }

        // Run every recursive arm in order with `cur_row` as the single row in the working table
        let mut step_ctes = rec_ctes.clone();
        step_ctes.working.push((
            s_idx,
            c_idx,
            QueryOutput {
                column_names: working_col_names.clone(),
                rows: vec![cur_row],
            },
        ));
        for (op, rec_core) in &cte.query.body.compounds[first_rec_idx..] {
            let rhs_out = exec_single_select_core(
                ctx,
                rec_core,
                &[],
                None,
                params,
                outer_scopes,
                &step_ctes,
            )?;
            if rhs_out.column_names.len() != working_col_names.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "SELECTs to the left and right of {} do not have the same number of result columns",
                        op.as_sql_str()
                    ),
                ));
            }
            for nr in rhs_out.rows {
                ctx.step_tick()?;
                enqueue_row(&mut queue, &mut seen, nr, ctx.collations);
            }
        }
    }

    Ok(QueryOutput {
        column_names: working_col_names,
        rows: out_rows,
    })
}

fn exec_compound_select(
    ctx: &mut ExecContext<'_>,
    body: &SelectBody,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    let body_meta = infer_select_body_meta(body, outer_scopes, ctx.db_set, ctes);
    let head_out = exec_single_select_core(ctx, &body.head, &[], None, params, outer_scopes, ctes)?;
    let col_names = head_out.column_names;
    let mut all_arm_col_names = vec![col_names.clone()];
    let mut acc_rows = head_out.rows;
    let col_collations: Vec<Collation> = (0..col_names.len())
        .map(|i| body_meta.get(i).map_or(Collation::Binary, |(_, c)| *c))
        .collect();
    let default_specs: Vec<KeySortOrder> = col_collations
        .iter()
        .map(|&coll| KeySortOrder {
            desc: false,
            nulls_last: false,
            collation: coll,
        })
        .collect();

    for (op, rhs_core) in &body.compounds {
        let rhs_out =
            exec_single_select_core(ctx, rhs_core, &[], None, params, outer_scopes, ctes)?;
        if rhs_out.column_names.len() != col_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    op.as_sql_str()
                ),
            ));
        }
        all_arm_col_names.push(rhs_out.column_names);
        let enc = ctx.encoding();
        match op {
            CompoundOp::UnionAll => {
                acc_rows.extend(rhs_out.rows);
            }
            CompoundOp::Union => {
                acc_rows.extend(rhs_out.rows);
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
            }
            CompoundOp::Intersect => {
                let mut rhs_sorted = rhs_out.rows;
                rhs_sorted.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
                acc_rows.retain(|r| {
                    rhs_sorted
                        .binary_search_by(|probe| {
                            compare_value_slices(probe, r, &default_specs, enc)
                        })
                        .is_ok()
                });
            }
            CompoundOp::Except => {
                let mut rhs_sorted = rhs_out.rows;
                rhs_sorted.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.sort_by(|a, b| compare_value_slices(a, b, &default_specs, enc));
                acc_rows.dedup_by(|a, b| {
                    compare_value_slices(a, b, &default_specs, enc) == Ordering::Equal
                });
                acc_rows.retain(|r| {
                    rhs_sorted
                        .binary_search_by(|probe| {
                            compare_value_slices(probe, r, &default_specs, enc)
                        })
                        .is_err()
                });
            }
        }
    }

    if !order_by.is_empty() {
        let enc = ctx.encoding();
        sort_materialized_output(
            &mut acc_rows,
            &all_arm_col_names,
            &col_collations,
            order_by,
            enc,
        )?;
    }
    let acc_rows = apply_limit_offset(ctx, acc_rows, limit, params, outer_scopes, ctes)?;
    Ok(QueryOutput {
        column_names: col_names,
        rows: acc_rows,
    })
}

fn strip_collate_and_unary_plus(mut e: &Expr) -> Option<i64> {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    match e {
        Expr::Integer(i) => Some(*i),
        Expr::Unary {
            op: UnaryOp::Negate,
            expr,
        } => {
            if let Expr::Integer(i) = expr.as_ref() {
                Some(i.saturating_neg())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn ordinal_suffix(n: usize) -> String {
    match n % 100 {
        11..=13 => format!("{n}th"),
        _ => match n % 10 {
            1 => format!("{n}st"),
            2 => format!("{n}nd"),
            3 => format!("{n}rd"),
            _ => format!("{n}th"),
        },
    }
}

fn exec_single_select_core(
    ctx: &mut ExecContext<'_>,
    core: &SelectCore,
    order_by: &[OrderByTerm],
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<QueryOutput> {
    match core {
        SelectCore::Values(expr_rows) => {
            if expr_rows.is_empty() {
                return Ok(QueryOutput::default());
            }
            let width = expr_rows[0].len();
            let col_names: Vec<String> = (1..=width).map(|i| format!("column{i}")).collect();
            let mut rows = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                if erow.len() != width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "all VALUES must have the same number of terms",
                    ));
                }
                let mut vals = Vec::with_capacity(width);
                for e in erow {
                    vals.push(eval_expr(ctx, e, params, outer_scopes, ctes, None)?);
                }
                rows.push(vals);
            }
            if col_names.len() > ctx.db_set.column_limit() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "too many columns in result set",
                ));
            }
            if !order_by.is_empty() {
                let enc = ctx.encoding();
                let arm_cols = vec![col_names.clone()];
                let default_colls = vec![Collation::Binary; col_names.len()];
                sort_materialized_output(&mut rows, &arm_cols, &default_colls, order_by, enc)?;
            }
            let rows = apply_limit_offset(ctx, rows, limit, params, outer_scopes, ctes)?;
            Ok(QueryOutput {
                column_names: col_names,
                rows,
            })
        }
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            let has_proj_aliases = projection
                .iter()
                .any(|rc| matches!(rc, ResultColumn::Expr { alias: Some(_), .. }));
            let pre_where_selection: Option<Expr> = if let Some(where_expr) = selection {
                if has_proj_aliases {
                    let preview_cols = from
                        .as_ref()
                        .and_then(|fc| {
                            static_schema_cols_for_from(
                                fc,
                                outer_scopes,
                                ctx.db_set,
                                ctes,
                                &mut Vec::new(),
                            )
                        })
                        .unwrap_or_default();
                    if let Ok((_, pre_proj)) = expand_projection(projection, &preview_cols) {
                        Some(subst_select_aliases_in_expr_with_ctes(
                            where_expr,
                            &pre_proj,
                            &preview_cols,
                            ctx.db_set,
                            ctes,
                        ))
                    } else {
                        Some(where_expr.clone())
                    }
                } else {
                    Some(where_expr.clone())
                }
            } else {
                None
            };

            // 1. Evaluate FROM & JOINs
            let source_rows = match from {
                None => vec![EvalRow::default()],
                Some(fc) => eval_from_clause(
                    ctx,
                    fc,
                    pre_where_selection.as_ref(),
                    params,
                    outer_scopes,
                    ctes,
                )?,
            };

            let sample_cols: Vec<ScopedColumn> = match source_rows.first() {
                Some(r) => r.cols.to_vec(),
                None => match from {
                    Some(fc) => {
                        schema_cols_preview(ctx, fc, params, outer_scopes, ctes).unwrap_or_default()
                    }
                    None => Vec::new(),
                },
            };

            if from.is_none() && projection.iter().any(|rc| matches!(rc, ResultColumn::Star)) {
                return Err(SqliteError::new(ErrorCode::Error, "no tables specified"));
            }
            let (col_names, expanded_proj) = expand_projection(projection, &sample_cols)?;
            if col_names.len() > ctx.db_set.column_limit() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "too many columns in result set",
                ));
            }

            let resolved_where = selection.as_ref().map(|where_expr| {
                subst_select_aliases_in_expr_with_ctes(
                    where_expr,
                    &expanded_proj,
                    &sample_cols,
                    ctx.db_set,
                    ctes,
                )
            });

            // 2. Filter via WHERE
            let mut filtered = Vec::with_capacity(source_rows.len());
            if let Some(ref where_expr) = resolved_where {
                for r in source_rows {
                    ctx.step_tick()?;
                    let scopes = push_scope(outer_scopes, &r);
                    let v = eval_expr(ctx, where_expr, params, &scopes, ctes, None)?;
                    if v.truthiness() == Some(true) {
                        filtered.push(r);
                    }
                }
            } else {
                filtered = source_rows;
            }

            // 3. Determine whether query is aggregated
            let is_agg = !group_by.is_empty()
                || having.is_some()
                || projection.iter().any(|rc| match rc {
                    ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                    _ => false,
                })
                || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr));

            // Validate & resolve GROUP BY / ORDER BY integer ordinals (1..=N) & SELECT aliases
            let mut resolved_group_by = Vec::with_capacity(group_by.len());
            for (gi, ge) in group_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(ge) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} GROUP BY term out of range - should be between 1 and {}",
                                ordinal_suffix(gi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                    let target_expr = expanded_proj[(ord as usize) - 1].0.clone();
                    if let Expr::Collate { collation, .. } = ge {
                        resolved_group_by.push(Expr::Collate {
                            expr: Box::new(target_expr),
                            collation: collation.clone(),
                        });
                    } else {
                        resolved_group_by.push(target_expr);
                    }
                } else {
                    resolved_group_by.push(subst_select_aliases_in_expr_with_ctes(
                        ge,
                        &expanded_proj,
                        &sample_cols,
                        ctx.db_set,
                        ctes,
                    ));
                }
            }
            let resolved_having = having.as_ref().map(|hav| {
                subst_select_aliases_in_expr_with_ctes(
                    hav,
                    &expanded_proj,
                    &sample_cols,
                    ctx.db_set,
                    ctes,
                )
            });
            for (oi, ob) in order_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(&ob.expr)
                    && (ord < 1 || (ord as usize) > expanded_proj.len())
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "{} ORDER BY term out of range - should be between 1 and {}",
                            ordinal_suffix(oi + 1),
                            expanded_proj.len()
                        ),
                    ));
                }
            }

            let mut result_rows_with_sort: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();

            if is_agg {
                let mut minmax_specs: Vec<(AggregateKind, Expr, Option<Expr>, Collation)> =
                    Vec::new();
                for (pe, _, _) in &expanded_proj {
                    collect_minmax_aggregates(pe, &sample_cols, &mut minmax_specs);
                }
                if minmax_specs.is_empty() {
                    if let Some(ref hav) = resolved_having {
                        collect_minmax_aggregates(hav, &sample_cols, &mut minmax_specs);
                    }
                    for ob in order_by {
                        collect_minmax_aggregates(&ob.expr, &sample_cols, &mut minmax_specs);
                    }
                }

                let groups = build_groups(
                    ctx,
                    filtered,
                    &resolved_group_by,
                    params,
                    outer_scopes,
                    ctes,
                )?;
                for group_rows in groups {
                    let rep_row = choose_aggregate_representative_row(
                        ctx,
                        &group_rows,
                        &sample_cols,
                        &minmax_specs,
                        params,
                        outer_scopes,
                        ctes,
                    )?;
                    let scopes = push_scope(outer_scopes, &rep_row);
                    if let Some(ref hav) = resolved_having {
                        let hv = eval_expr(
                            ctx,
                            hav,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        if hv.truthiness() != Some(true) {
                            continue;
                        }
                    }

                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(
                            ctx,
                            pe,
                            params,
                            &scopes,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            Some((&group_rows, outer_scopes)),
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            } else {
                if filtered.is_empty() && !sample_cols.is_empty() {
                    // Validate projection & ORDER BY column resolution even when 0 rows match!
                    let dummy_row = EvalRow::new(
                        Rc::from(sample_cols.as_slice()),
                        vec![Value::Null; sample_cols.len()],
                    );
                    let scopes = push_scope(outer_scopes, &dummy_row);
                    let mut dummy_proj = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        dummy_proj.push(eval_expr(ctx, pe, params, &scopes, ctes, None)?);
                    }
                    for ob in order_by {
                        let _ = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &dummy_proj,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            None,
                        )?;
                    }
                }
                for row in filtered {
                    ctx.step_tick()?;
                    let scopes = push_scope(outer_scopes, &row);
                    let mut out_vals = Vec::with_capacity(expanded_proj.len());
                    for (pe, _, _) in &expanded_proj {
                        out_vals.push(eval_expr(ctx, pe, params, &scopes, ctes, None)?);
                    }

                    let mut sort_vals = Vec::with_capacity(order_by.len());
                    for ob in order_by {
                        let sv = eval_order_term_in_scope(
                            ctx,
                            &ob.expr,
                            &out_vals,
                            &expanded_proj,
                            &scopes,
                            params,
                            ctes,
                            None,
                        )?;
                        sort_vals.push(sv);
                    }
                    result_rows_with_sort.push((out_vals, sort_vals));
                }
            }

            if *distinct {
                let enc = ctx.encoding();
                let dist_specs: Vec<KeySortOrder> = expanded_proj
                    .iter()
                    .map(|(pe, _, _)| KeySortOrder {
                        desc: false,
                        nulls_last: false,
                        collation: infer_expr_collation_from_cols(pe, &sample_cols),
                    })
                    .collect();
                let has_custom_coll = dist_specs.iter().any(|s| s.collation != Collation::Binary);
                if !has_custom_coll {
                    let mut unique_rows: Vec<Vec<Value>> = Vec::new();
                    let mut seen_bytes = HashSet::new();
                    result_rows_with_sort.retain(|(proj, _)| {
                        let norm: Vec<Value> = proj
                            .iter()
                            .map(|v| match v {
                                Value::Real(r)
                                    if (*r as i64 as f64) == *r
                                        && *r >= (i64::MIN as f64)
                                        && *r < 9223372036854775808.0 =>
                                {
                                    Value::Integer(*r as i64)
                                }
                                other => other.clone(),
                            })
                            .collect();
                        let _ = &mut unique_rows;
                        seen_bytes.insert(encode_record_with_encoding(&norm, enc))
                    });
                } else {
                    let mut seen_rows: Vec<Vec<Value>> = Vec::new();
                    result_rows_with_sort.retain(|(proj, _)| {
                        if seen_rows.iter().any(|prev| {
                            compare_value_slices(prev, proj, &dist_specs, enc) == Ordering::Equal
                        }) {
                            false
                        } else {
                            seen_rows.push(proj.clone());
                            true
                        }
                    });
                }
            }

            if col_names.len() > ctx.db_set.column_limit() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "too many columns in result set",
                ));
            }
            if !order_by.is_empty() {
                let enc = ctx.encoding();
                let specs: Vec<(KeySortOrder, Option<String>)> = order_by
                    .iter()
                    .map(|ob| {
                        let custom_coll = match &ob.expr {
                            Expr::Collate { collation, .. }
                                if Collation::from_name(collation).is_none() =>
                            {
                                Some(collation.clone())
                            }
                            _ => None,
                        };
                        let coll = infer_order_by_collation(&ob.expr, &expanded_proj, &sample_cols);
                        (
                            KeySortOrder {
                                desc: ob.desc,
                                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                                collation: coll,
                            },
                            custom_coll,
                        )
                    })
                    .collect();

                let collations = ctx.collations;
                result_rows_with_sort.sort_by(|(_, sa), (_, sb)| {
                    for (i, (spec, custom_opt)) in specs.iter().enumerate() {
                        let a = &sa[i];
                        let b = &sb[i];
                        let ord = match (a.is_null(), b.is_null()) {
                            (true, true) => Ordering::Equal,
                            (true, false) => {
                                if spec.nulls_last {
                                    Ordering::Greater
                                } else {
                                    Ordering::Less
                                }
                            }
                            (false, true) => {
                                if spec.nulls_last {
                                    Ordering::Less
                                } else {
                                    Ordering::Greater
                                }
                            }
                            (false, false) => {
                                let raw = if let (Some(cname), Value::Text(ls), Value::Text(rs)) =
                                    (custom_opt.as_deref(), a, b)
                                {
                                    collations
                                        .compare(cname, ls, rs, enc)
                                        .unwrap_or(Ordering::Equal)
                                } else {
                                    a.cmp_sqlite_with_encoding(b, spec.collation, enc)
                                };
                                if spec.desc { raw.reverse() } else { raw }
                            }
                        };
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                    Ordering::Equal
                });
            }

            let out_rows: Vec<Vec<Value>> =
                result_rows_with_sort.into_iter().map(|(p, _)| p).collect();
            let out_rows = apply_limit_offset(ctx, out_rows, limit, params, outer_scopes, ctes)?;

            Ok(QueryOutput {
                column_names: col_names,
                rows: out_rows,
            })
        }
    }
}

fn collect_minmax_aggregates(
    expr: &Expr,
    sample_cols: &[ScopedColumn],
    out: &mut Vec<(AggregateKind, Expr, Option<Expr>, Collation)>,
) {
    match expr {
        Expr::FunctionCall {
            name,
            distinct: _,
            args: FunctionArgs::Exprs(es),
            filter,
        } if es.len() == 1
            && (name.eq_ignore_ascii_case("MIN") || name.eq_ignore_ascii_case("MAX")) =>
        {
            let kind = if name.eq_ignore_ascii_case("MIN") {
                AggregateKind::Min
            } else {
                AggregateKind::Max
            };
            let coll = infer_expr_collation_from_cols(&es[0], sample_cols);
            out.push((kind, es[0].clone(), filter.as_deref().cloned(), coll));
        }
        Expr::FunctionCall {
            args: FunctionArgs::Exprs(es),
            filter,
            ..
        } => {
            if !expr_contains_aggregate(expr) {
                for a in es {
                    collect_minmax_aggregates(a, sample_cols, out);
                }
                if let Some(f) = filter {
                    collect_minmax_aggregates(f, sample_cols, out);
                }
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => collect_minmax_aggregates(inner, sample_cols, out),
        Expr::Binary { left, right, .. } => {
            collect_minmax_aggregates(left, sample_cols, out);
            collect_minmax_aggregates(right, sample_cols, out);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_minmax_aggregates(e, sample_cols, out);
            collect_minmax_aggregates(low, sample_cols, out);
            collect_minmax_aggregates(high, sample_cols, out);
        }
        Expr::InList { expr: e, list, .. } => {
            collect_minmax_aggregates(e, sample_cols, out);
            for item in list {
                collect_minmax_aggregates(item, sample_cols, out);
            }
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_minmax_aggregates(e, sample_cols, out);
            collect_minmax_aggregates(pattern, sample_cols, out);
            if let Some(esc) = escape {
                collect_minmax_aggregates(esc, sample_cols, out);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_minmax_aggregates(op, sample_cols, out);
            }
            for (w, t) in branches {
                collect_minmax_aggregates(w, sample_cols, out);
                collect_minmax_aggregates(t, sample_cols, out);
            }
            if let Some(el) = else_expr {
                collect_minmax_aggregates(el, sample_cols, out);
            }
        }
        Expr::RowValue(elems) => {
            for el in elems {
                collect_minmax_aggregates(el, sample_cols, out);
            }
        }
        _ => {}
    }
}

/// Chooses the group representative row for bare columns per SQLite `select.c` (`updateAccumulator`):
/// when `MIN(x)` or `MAX(x)` is present, bare columns come from the row achieving the extremum.
fn choose_aggregate_representative_row(
    ctx: &mut ExecContext<'_>,
    group_rows: &[EvalRow],
    sample_cols: &[ScopedColumn],
    minmax_specs: &[(AggregateKind, Expr, Option<Expr>, Collation)],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<EvalRow> {
    let Some(first_row) = group_rows.first() else {
        return Ok(EvalRow::new(
            Rc::from(sample_cols),
            vec![Value::Null; sample_cols.len()],
        ));
    };
    let Some((kind, arg_expr, filt_expr, coll)) = minmax_specs.last() else {
        return Ok(first_row.clone());
    };

    let mut best_val: Option<Value> = None;
    let mut best_idx: Option<usize> = None;
    for (idx, gr) in group_rows.iter().enumerate() {
        let row_scopes = push_scope(outer_scopes, gr);
        if let Some(fe) = filt_expr {
            let fv = eval_expr(ctx, fe, params, &row_scopes, ctes, None)?;
            if fv.truthiness() != Some(true) {
                continue;
            }
        }
        let val = eval_expr(ctx, arg_expr, params, &row_scopes, ctes, None)?;
        if val.is_null() {
            continue;
        }
        let is_better = match &best_val {
            None => true,
            Some(cur) => match kind {
                AggregateKind::Min => val.cmp_sqlite(cur, *coll).is_lt(),
                AggregateKind::Max => val.cmp_sqlite(cur, *coll).is_gt(),
                _ => false,
            },
        };
        if is_better {
            best_val = Some(val);
            best_idx = Some(idx);
        }
    }

    Ok(group_rows[best_idx.unwrap_or(0)].clone())
}

fn split_where_conjuncts<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = expr
    {
        split_where_conjuncts(left, out);
        split_where_conjuncts(right, out);
    } else {
        out.push(expr);
    }
}

fn resolve_unique_col_idx_cref(cols: &[ScopedColumn], cref: &ColumnRef) -> Result<Option<usize>> {
    let mut found = Vec::new();
    for (i, sc) in cols.iter().enumerate() {
        if sc.hidden_rowid {
            continue;
        }
        if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
            continue;
        }
        if sc.name.eq_ignore_ascii_case(&cref.column) {
            if cref.schema.is_none() && cref.table.is_none() && sc.merged_using {
                return Ok(Some(i));
            }
            found.push(i);
        }
    }
    if found.len() == 1 {
        return Ok(Some(found[0]));
    }
    if found.len() > 1 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("ambiguous column name: {}", cref.column),
        ));
    }
    if cref.column.eq_ignore_ascii_case("rowid")
        || cref.column.eq_ignore_ascii_case("_rowid_")
        || cref.column.eq_ignore_ascii_case("oid")
    {
        for (i, sc) in cols.iter().enumerate() {
            if !sc.hidden_rowid {
                continue;
            }
            if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                continue;
            }
            found.push(i);
        }
        if found.len() == 1 {
            return Ok(Some(found[0]));
        }
        if found.len() > 1 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("ambiguous column name: {}", cref.column),
            ));
        }
    }
    Ok(None)
}

fn expr_can_eval_with_cols(
    expr: &Expr,
    avail_cols: &[ScopedColumn],
    full_scope_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> bool {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => true,
        Expr::Subquery(_) | Expr::Exists { .. } | Expr::Raise { .. } => false,
        Expr::Column(cref) => match resolve_unique_col_idx_cref(full_scope_cols, cref) {
            Err(_) => false, // Ambiguous in full join scope: do NOT push down!
            Ok(Some(full_idx)) => {
                let full_sc = &full_scope_cols[full_idx];
                if full_sc.merged_using {
                    return false;
                }
                match resolve_unique_col_idx_cref(avail_cols, cref) {
                    Ok(Some(avail_idx)) => {
                        let avail_sc = &avail_cols[avail_idx];
                        !avail_sc.merged_using
                            && avail_sc.schema_name == full_sc.schema_name
                            && avail_sc.table_alias == full_sc.table_alias
                            && avail_sc.name == full_sc.name
                            && avail_sc.hidden_rowid == full_sc.hidden_rowid
                    }
                    _ => false,
                }
            }
            Ok(None) => {
                // Not in `full_scope_cols`: resolves in `outer_scopes` iff unambiguous there
                // and not accidentally matching anything in `avail_cols`.
                if resolve_unique_col_idx_cref(avail_cols, cref)
                    .ok()
                    .flatten()
                    .is_some()
                {
                    return false;
                }
                outer_scopes
                    .iter()
                    .rev()
                    .find_map(|os| match os.lookup_cref_full(cref) {
                        Err(_) => Some(false),
                        Ok(Some(_)) => Some(true),
                        Ok(None) => None,
                    })
                    .unwrap_or(false)
            }
        },
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            expr_can_eval_with_cols(inner, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::Binary { left, op: _, right } => {
            expr_can_eval_with_cols(left, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(right, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(low, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(high, avail_cols, full_scope_cols, outer_scopes)
        }
        Expr::InList { expr: e, list, .. } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && list.iter().all(|it| {
                    expr_can_eval_with_cols(it, avail_cols, full_scope_cols, outer_scopes)
                })
        }
        Expr::InSubquery { .. } | Expr::InTable { .. } => false,
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_can_eval_with_cols(e, avail_cols, full_scope_cols, outer_scopes)
                && expr_can_eval_with_cols(pattern, avail_cols, full_scope_cols, outer_scopes)
                && escape.as_deref().is_none_or(|esc| {
                    expr_can_eval_with_cols(esc, avail_cols, full_scope_cols, outer_scopes)
                })
        }
        Expr::FunctionCall { args, filter, .. } => {
            if expr_contains_aggregate(expr) || filter.is_some() {
                return false;
            }
            match args {
                FunctionArgs::Star => true,
                FunctionArgs::Exprs(es) => es
                    .iter()
                    .all(|a| expr_can_eval_with_cols(a, avail_cols, full_scope_cols, outer_scopes)),
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand.as_deref().is_none_or(|op| {
                expr_can_eval_with_cols(op, avail_cols, full_scope_cols, outer_scopes)
            }) && branches.iter().all(|(w, t)| {
                expr_can_eval_with_cols(w, avail_cols, full_scope_cols, outer_scopes)
                    && expr_can_eval_with_cols(t, avail_cols, full_scope_cols, outer_scopes)
            }) && else_expr.as_deref().is_none_or(|el| {
                expr_can_eval_with_cols(el, avail_cols, full_scope_cols, outer_scopes)
            })
        }
        Expr::RowValue(elems) => elems
            .iter()
            .all(|el| expr_can_eval_with_cols(el, avail_cols, full_scope_cols, outer_scopes)),
    }
}

fn eval_from_clause(
    ctx: &mut ExecContext<'_>,
    fc: &FromClause,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    if 1 + fc.joins.len() > 64 {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "at most 64 tables in a join",
        ));
    }
    let no_using_anywhere = fc
        .joins
        .iter()
        .all(|j| !j.natural && !matches!(j.constraint, Some(JoinConstraint::Using(_))));
    let full_from_cols_opt = if no_using_anywhere && !fc.joins.is_empty() && where_hint.is_some() {
        schema_cols_preview(ctx, fc, params, outer_scopes, ctes)
    } else {
        None
    };
    let mut pushable_conjuncts: Vec<&Expr> = Vec::new();
    if let (Some(wh), Some(_)) = (where_hint, &full_from_cols_opt) {
        split_where_conjuncts(wh, &mut pushable_conjuncts);
    }
    let mut applied = vec![false; pushable_conjuncts.len()];

    let single_table_hint = if fc.joins.is_empty() {
        where_hint
    } else {
        None
    };
    let mut left_rows =
        eval_table_or_subquery(ctx, &fc.head, single_table_hint, params, outer_scopes, ctes)?;
    let mut left_cols_fallback: Vec<ScopedColumn> = left_rows
        .first()
        .map(|r| r.cols.to_vec())
        .or_else(|| schema_cols_for_tos(ctx, &fc.head, params, outer_scopes, ctes))
        .unwrap_or_default();

    // Push single-table WHERE conjuncts on `fc.head` when no later join is RIGHT or FULL
    let no_right_or_full_after_head = fc
        .joins
        .iter()
        .all(|j| !matches!(j.join_type, JoinType::Right | JoinType::Full));
    if no_right_or_full_after_head && let Some(ref full_cols) = full_from_cols_opt {
        for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
            if !applied[ci]
                && expr_can_eval_with_cols(conj, &left_cols_fallback, full_cols, outer_scopes)
            {
                applied[ci] = true;
                let mut kept = Vec::with_capacity(left_rows.len());
                for r in left_rows {
                    let sc = push_scope(outer_scopes, &r);
                    if eval_expr(ctx, conj, params, &sc, ctes, None)?.truthiness() == Some(true) {
                        kept.push(r);
                    }
                }
                left_rows = kept;
            }
        }
    }

    for (j_idx, join) in fc.joins.iter().enumerate() {
        let right_rows =
            eval_table_or_subquery(ctx, &join.right, None, params, outer_scopes, ctes)?;
        let right_cols_fallback: Vec<ScopedColumn> = right_rows
            .first()
            .map(|r| r.cols.to_vec())
            .or_else(|| schema_cols_for_tos(ctx, &join.right, params, outer_scopes, ctes))
            .unwrap_or_default();
        let next_merged_cols =
            merge_join_scoped_columns(&left_cols_fallback, &right_cols_fallback, join);

        let no_later_right_or_full = fc.joins[(j_idx + 1)..]
            .iter()
            .all(|j| !matches!(j.join_type, JoinType::Right | JoinType::Full));

        let mut in_join_where_conjuncts: Vec<&Expr> = Vec::new();
        if no_later_right_or_full
            && matches!(join.join_type, JoinType::Inner | JoinType::Cross)
            && let Some(ref full_cols) = full_from_cols_opt
        {
            for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
                if !applied[ci]
                    && expr_can_eval_with_cols(conj, &next_merged_cols, full_cols, outer_scopes)
                {
                    applied[ci] = true;
                    in_join_where_conjuncts.push(conj);
                }
            }
        }

        left_rows = apply_join(
            ctx,
            left_rows,
            right_rows,
            &left_cols_fallback,
            &right_cols_fallback,
            join,
            &in_join_where_conjuncts,
            params,
            outer_scopes,
            ctes,
        )?;
        left_cols_fallback = next_merged_cols;

        // Post-join WHERE pushdown whenever all remaining joins in `fc.joins[(j_idx + 1)..]`
        // are `INNER`/`CROSS`/`LEFT` (none can NULL-extend or right-match `left_rows`)!
        if no_later_right_or_full && let Some(ref full_cols) = full_from_cols_opt {
            for (ci, &conj) in pushable_conjuncts.iter().enumerate() {
                if !applied[ci]
                    && expr_can_eval_with_cols(conj, &left_cols_fallback, full_cols, outer_scopes)
                {
                    applied[ci] = true;
                    let mut kept = Vec::with_capacity(left_rows.len());
                    for r in left_rows {
                        if outer_scopes.is_empty() {
                            if eval_expr(ctx, conj, params, std::slice::from_ref(&r), ctes, None)?
                                .truthiness()
                                == Some(true)
                            {
                                kept.push(r);
                            }
                        } else {
                            let sc = push_scope(outer_scopes, &r);
                            if eval_expr(ctx, conj, params, &sc, ctes, None)?.truthiness()
                                == Some(true)
                            {
                                kept.push(r);
                            }
                        }
                    }
                    left_rows = kept;
                }
            }
        }
    }

    Ok(left_rows)
}

fn disambiguate_view_column_names(raw_names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw_names.len());
    for raw in raw_names {
        if !out.iter().any(|ex| ex.eq_ignore_ascii_case(raw)) {
            out.push(raw.clone());
            continue;
        }
        let base = match raw.rfind(':') {
            Some(pos)
                if pos + 1 < raw.len()
                    && raw.as_bytes()[pos + 1..].iter().all(u8::is_ascii_digit) =>
            {
                &raw[..pos]
            }
            _ => raw.as_str(),
        };
        let mut k = 1usize;
        loop {
            let candidate = format!("{base}:{k}");
            if !out.iter().any(|ex| ex.eq_ignore_ascii_case(&candidate)) {
                out.push(candidate);
                break;
            }
            k += 1;
        }
    }
    out
}

fn prepare_fixed_view_select(
    db_id: DatabaseId,
    schema_name: &str,
    view: &ViewSchema,
) -> Result<SelectStmt> {
    validate_view_no_parameters(&view.select)?;
    if db_id != DatabaseId::TEMP {
        fix_permanent_view_select(&view.name, schema_name, &view.select)
    } else {
        Ok(view.select.clone())
    }
}

#[allow(clippy::type_complexity)]
fn eval_view_query_output(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    view: &ViewSchema,
    ctes: &CteStore,
) -> Result<(QueryOutput, Vec<(Option<TypeAffinity>, Collation)>, String)> {
    ctes.record_catalog_ref(db_id, &view.name)?;
    if !ctx.db_set.enable_view() && db_id != DatabaseId::TEMP {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("access to view \"{}\" prohibited", view.name),
        ));
    }
    ctx.ensure_read_db(db_id, true)?;
    let schema_lbl = ctx
        .db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());
    let view_ctes = ctes.enter_view(db_id, &view.name, ctx.db_set.expr_depth_limit())?;
    let cache_key = (db_id, view.name.to_ascii_lowercase());
    // Ensure sub-reference counts are still accounted even when output is cached
    let _ = resolve_view_columns_static(
        db_id,
        view,
        ctx.db_set,
        ctes,
        ctx.functions,
        &mut Vec::new(),
    )?;
    if let Some(cached) = ctes.runtime_view_cache.borrow().get(&cache_key).cloned() {
        return Ok(cached);
    }
    let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view)?;
    let meta = infer_select_stmt_meta(&fixed_sel, &[], ctx.db_set, &view_ctes);
    let mut out = exec_select(ctx, &fixed_sel, &[], &[], &view_ctes)?;
    if let Some(ref explicit_cols) = view.columns {
        if explicit_cols.len() != out.column_names.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "expected {} columns for '{}' but got {}",
                    explicit_cols.len(),
                    view.name,
                    out.column_names.len()
                ),
            ));
        }
        out.column_names = disambiguate_view_column_names(explicit_cols);
    } else {
        out.column_names = disambiguate_view_column_names(&out.column_names);
    }
    let res = (out, meta, schema_lbl);
    ctes.runtime_view_cache
        .borrow_mut()
        .insert(cache_key, res.clone());
    Ok(res)
}

fn eval_table_or_subquery(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    match tos {
        TableOrSubquery::Table {
            name,
            table_args,
            alias,
            indexed_by,
        } => {
            if table_args.is_some() {
                let exists_non_tvf = (name.schema.is_none() && ctes.locate(&name.name).is_some())
                    || ctx.db_set.find_relation_in_set(name)?.is_some()
                    || ctx.db_set.resolve_catalog_table(name)?.is_some();
                if exists_non_tvf {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("'{}' is not a function", name.name),
                    ));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", name.display_name()),
                ));
            }
            let eff_alias = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some(cte_out) = resolve_cte(ctx, ctes, &name.name, params, outer_scopes)?
            {
                if let Some(hint) = indexed_by {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        match hint {
                            IndexHint::NotIndexed => {
                                format!("cannot use NOT INDEXED on CTE: {}", name.name)
                            }
                            IndexHint::IndexedBy(idx) => format!("no such index: {idx}"),
                        },
                    ));
                }
                let cte_meta = infer_cte_meta_by_name(&name.name, outer_scopes, ctx.db_set, ctes);
                let cols: Rc<[ScopedColumn]> = cte_out
                    .column_names
                    .iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = cte_meta
                            .get(i)
                            .copied()
                            .unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: Some(eff_alias.clone()),
                            name: cn.clone(),
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect::<Vec<_>>()
                    .into();
                return Ok(cte_out
                    .rows
                    .into_iter()
                    .map(|r| EvalRow::new(Rc::clone(&cols), r))
                    .collect());
            }

            if ctx.db_set.resolve_catalog_table(name)?.is_none()
                && let Some((v_db_id, view_ref)) = ctx.db_set.find_view_in_set(name)
            {
                if let Some(IndexHint::IndexedBy(idx)) = indexed_by {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such index: {idx}"),
                    ));
                }
                let view_cloned = view_ref.clone();
                let (v_out, v_meta, schema_lbl) =
                    eval_view_query_output(ctx, v_db_id, &view_cloned, ctes)?;
                let cols: Rc<[ScopedColumn]> = v_out
                    .column_names
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) =
                            v_meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: Some(schema_lbl.clone()),
                            table_alias: Some(eff_alias.clone()),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect::<Vec<_>>()
                    .into();
                return Ok(v_out
                    .rows
                    .into_iter()
                    .map(|r| EvalRow::new(Rc::clone(&cols), r))
                    .collect());
            }

            let (db_id, tbl, schema_lbl) = resolve_table_for_query_in_set(ctx.db_set, name)?;
            ctes.record_catalog_ref(db_id, &tbl.name)?;

            let raw_entries = if tbl.root_pgno == 0 {
                Vec::new()
            } else {
                read_table_with_plan(
                    ctx,
                    db_id,
                    &tbl,
                    alias.as_deref(),
                    indexed_by.as_ref(),
                    where_hint,
                    params,
                    outer_scopes,
                    ctes,
                )?
            };

            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.clone()),
                    table_alias: Some(eff_alias.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                let rid_col_name = tbl
                    .rowid_alias_col
                    .map(|idx| tbl.columns[idx].name.clone())
                    .unwrap_or_else(|| "rowid".to_owned());
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl),
                    table_alias: Some(eff_alias),
                    name: rid_col_name,
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    omit_from_star: false,
                    merged_using: false,
                });
            }

            let shared_cols: Rc<[ScopedColumn]> = cols.into();
            let mut out = Vec::with_capacity(raw_entries.len());
            for (rkey, payload) in raw_entries {
                let mut vals = decode_table_logical_row(ctx, db_id, &tbl, &rkey, &payload, params)?;
                if let TableRowKey::Rowid(rowid) = rkey {
                    vals.push(Value::Integer(rowid));
                }
                out.push(EvalRow::new(Rc::clone(&shared_cols), vals));
            }
            Ok(out)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let nested_ctes = ctes.enter_nested_select(ctx.db_set.expr_depth_limit())?;
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, ctx.db_set, &nested_ctes);
            let sub_out = exec_select(ctx, query, params, outer_scopes, &nested_ctes)?;
            let disambig_names = disambiguate_view_column_names(&sub_out.column_names);
            let cols: Rc<[ScopedColumn]> = disambig_names
                .into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = sub_meta
                        .get(i)
                        .copied()
                        .unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: None,
                        table_alias: alias.clone(),
                        name: cn,
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        omit_from_star: false,
                        merged_using: false,
                    }
                })
                .collect::<Vec<_>>()
                .into();
            Ok(sub_out
                .rows
                .into_iter()
                .map(|vals| EvalRow::new(Rc::clone(&cols), vals))
                .collect())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn read_table_with_plan(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    alias: Option<&str>,
    indexed_by: Option<&IndexHint>,
    where_hint: Option<&Expr>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<(TableRowKey, Vec<u8>)>> {
    ctx.ensure_read_db(db_id, true)?;
    let Some(slot_ref) = ctx.db_set.slot(db_id) else {
        return Ok(Vec::new());
    };
    let Some(bt_ref) = slot_ref.btree.as_ref() else {
        return Ok(Vec::new());
    };
    let db_page_count = bt_ref.pager().page_count();
    let db_enc = bt_ref.encoding();

    if let Some(hint) = indexed_by {
        match hint {
            IndexHint::NotIndexed => {
                if db_page_count == 0 {
                    return Ok(Vec::new());
                }
                return scan_table_raw_entries(ctx, db_id, tbl);
            }
            IndexHint::IndexedBy(idx_name) => {
                let Some(idx) = slot_ref
                    .schema
                    .indexes_for_table(&tbl.name)
                    .into_iter()
                    .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))
                else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such index: {idx_name}"),
                    ));
                };
                if let Some(ref pw) = idx.where_clause {
                    let usable = where_hint.is_some_and(|wh| expr_contains_conjunct(wh, pw));
                    if !usable {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("cannot use index: {idx_name}"),
                        ));
                    }
                }
                if db_page_count == 0 {
                    return Ok(Vec::new());
                }
                let entries = ctx.scan_index_in_db(db_id, idx.root_pgno, idx.key_sort_orders())?;
                let mut rows = Vec::with_capacity(entries.len());
                for rec_bytes in entries {
                    let fields = decode_record_with_encoding(&rec_bytes, db_enc)?;
                    if tbl.without_rowid {
                        let pk = idx.extract_without_rowid_pk_from_hit(tbl, &fields);
                        let rkey = TableRowKey::WithoutRowidPk(pk);
                        if let Some(payload) = seek_table_raw_entry(ctx, db_id, tbl, &rkey)? {
                            rows.push((rkey, payload));
                        }
                    } else if let Some(Value::Integer(rid)) = fields.last()
                        && let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree
                        && let Some(payload) = bt.seek_table_rowid(tbl.root_pgno, *rid)?
                    {
                        rows.push((TableRowKey::Rowid(*rid), payload));
                    }
                }
                return Ok(rows);
            }
        }
    }

    if db_page_count == 0 {
        return Ok(Vec::new());
    }
    let plan = choose_access_plan_with_schema_name(
        tbl,
        &slot_ref.name,
        alias,
        where_hint,
        &slot_ref.schema,
    );
    match plan {
        AccessPlan::FullScan => scan_table_raw_entries(ctx, db_id, tbl),
        AccessPlan::RowidSeek { key_expr } => {
            let kv = eval_expr(ctx, &key_expr, params, outer_scopes, ctes, None)?;
            let target = match kv {
                Value::Integer(i) => Some(i),
                Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                Value::Text(ref s) => sqliters_types::parse_numeric_text(s).and_then(|v| match v {
                    Value::Integer(i) => Some(i),
                    Value::Real(r) if (r as i64 as f64) == r => Some(r as i64),
                    _ => None,
                }),
                _ => None,
            };
            let Some(rid) = target else {
                return Ok(Vec::new());
            };
            let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree else {
                return Ok(Vec::new());
            };
            if let Some(payload) = bt.seek_table_rowid(tbl.root_pgno, rid)? {
                Ok(vec![(TableRowKey::Rowid(rid), payload)])
            } else {
                Ok(Vec::new())
            }
        }
        AccessPlan::IndexSeek {
            index,
            prefix_exprs,
        } => {
            let mut prefix_vals = Vec::with_capacity(prefix_exprs.len());
            for (i, ke) in prefix_exprs.iter().enumerate() {
                let raw = eval_expr(ctx, ke, params, outer_scopes, ctes, None)?;
                let aff = index.columns[i]
                    .col_idx
                    .map_or(TypeAffinity::Blob, |ci| tbl.columns[ci].affinity);
                let coerced = aff.apply(raw);
                if coerced.is_null() {
                    // `col = NULL` is never true in SQL `WHERE`!
                    return Ok(Vec::new());
                }
                prefix_vals.push(coerced);
            }
            if tbl.without_rowid && index.origin == "pk" && index.root_pgno == tbl.root_pgno {
                let pk_orders = tbl.without_rowid_pk_orders();
                let raw_hits = {
                    let bt = ctx.db_set.slot_mut(db_id).unwrap().btree.as_mut().unwrap();
                    bt.seek_index_equal_raw(
                        tbl.root_pgno,
                        &prefix_vals,
                        &pk_orders[..prefix_vals.len()],
                    )?
                };
                let n_pk = tbl.without_rowid_pk_cols.len();
                let mut rows = Vec::with_capacity(raw_hits.len());
                for rec_bytes in raw_hits {
                    let decoded = decode_record_with_encoding(&rec_bytes, db_enc)?;
                    let pk = decoded[..n_pk.min(decoded.len())].to_vec();
                    rows.push((TableRowKey::WithoutRowidPk(pk), rec_bytes));
                }
                return Ok(rows);
            }
            let orders = index.key_sort_orders();
            let idx_hits = {
                let bt = ctx.db_set.slot_mut(db_id).unwrap().btree.as_mut().unwrap();
                bt.seek_index_equal(index.root_pgno, &prefix_vals, &orders)?
            };
            let mut rows = Vec::with_capacity(idx_hits.len());
            for hit in idx_hits {
                if tbl.without_rowid {
                    let pk = index.extract_without_rowid_pk_from_hit(tbl, &hit);
                    let rkey = TableRowKey::WithoutRowidPk(pk);
                    if let Some(payload) = seek_table_raw_entry(ctx, db_id, tbl, &rkey)? {
                        rows.push((rkey, payload));
                    }
                } else if let Some(Value::Integer(rid)) = hit.last()
                    && let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree
                    && let Some(payload) = bt.seek_table_rowid(tbl.root_pgno, *rid)?
                {
                    rows.push((TableRowKey::Rowid(*rid), payload));
                }
            }
            Ok(rows)
        }
    }
}

fn compute_join_using_cols(
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    join: &JoinClause,
) -> Vec<String> {
    if join.natural {
        let mut shared = Vec::new();
        for lc in left_cols {
            if lc.hidden_rowid {
                continue;
            }
            if right_cols
                .iter()
                .any(|rc| !rc.hidden_rowid && rc.name.eq_ignore_ascii_case(&lc.name))
                && !shared
                    .iter()
                    .any(|s: &String| s.eq_ignore_ascii_case(&lc.name))
            {
                shared.push(lc.name.clone());
            }
        }
        shared
    } else if let Some(JoinConstraint::Using(cols)) = &join.constraint {
        cols.clone()
    } else {
        Vec::new()
    }
}

fn merge_join_scoped_columns(
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    join: &JoinClause,
) -> Vec<ScopedColumn> {
    let using_cols = compute_join_using_cols(left_cols, right_cols, join);
    let mut cols = Vec::with_capacity(left_cols.len() + right_cols.len());
    for lc in left_cols {
        let mut sc = lc.clone();
        if !sc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&sc.name)) {
            sc.merged_using = true;
        }
        cols.push(sc);
    }
    for rc in right_cols {
        let mut sc = rc.clone();
        if !sc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&sc.name)) {
            sc.omit_from_star = true;
            sc.merged_using = false;
        }
        cols.push(sc);
    }
    cols
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum HashJoinKey {
    Null,
    Int(i64),
    RealBits(u64),
    Text(String),
    Blob(Vec<u8>),
}

impl HashJoinKey {
    fn from_value_exact(val: &Value, comp_aff: TypeAffinity) -> Option<Self> {
        const MAX_EXACT_F64_INT: i64 = 9_007_199_254_740_992; // 2^53
        let coerced;
        let v = if comp_aff != TypeAffinity::Blob {
            coerced = comp_aff.apply(val.clone());
            &coerced
        } else {
            val
        };
        match v {
            Value::Null => Some(Self::Null),
            Value::Integer(i) => {
                if *i < -MAX_EXACT_F64_INT || *i > MAX_EXACT_F64_INT {
                    None
                } else {
                    Some(Self::Int(*i))
                }
            }
            Value::Real(r) => {
                if !r.is_finite()
                    || *r < -(MAX_EXACT_F64_INT as f64)
                    || *r > (MAX_EXACT_F64_INT as f64)
                {
                    None
                } else if r.fract() == 0.0 {
                    Some(Self::Int(*r as i64))
                } else if *r == 0.0 {
                    Some(Self::RealBits(0.0f64.to_bits()))
                } else {
                    Some(Self::RealBits(r.to_bits()))
                }
            }
            Value::Text(s) => Some(Self::Text(s.clone())),
            Value::Blob(b) => Some(Self::Blob(b.clone())),
        }
    }
}

struct EquiHashIndex {
    l_idx: usize,
    is_null_eq: bool,
    comp_aff: TypeAffinity,
    buckets: HashMap<HashJoinKey, Vec<usize>>,
}

fn try_build_equi_hash_index(
    conjuncts: &[&Expr],
    left_cols: &[ScopedColumn],
    right_cols: &[ScopedColumn],
    merged_cols: &[ScopedColumn],
    left_rows: &[EvalRow],
    right_rows: &[EvalRow],
    eligible_right: &[usize],
) -> Option<EquiHashIndex> {
    let resolve_side = |e: &Expr| -> Option<(bool, usize)> {
        let Expr::Column(cref) = e else {
            return None;
        };
        // Must resolve unambiguously in `merged_cols` first!
        let m_idx = resolve_unique_col_idx_cref(merged_cols, cref)
            .ok()
            .flatten()?;
        let m_sc = &merged_cols[m_idx];
        if m_sc.merged_using || m_sc.collation != Collation::Binary {
            return None;
        }
        let in_l = resolve_unique_col_idx_cref(left_cols, cref).ok().flatten();
        let in_r = resolve_unique_col_idx_cref(right_cols, cref).ok().flatten();
        match (in_l, in_r) {
            (Some(li), None) if left_cols[li].collation == Collation::Binary => Some((true, li)),
            (None, Some(ri)) if right_cols[ri].collation == Collation::Binary => Some((false, ri)),
            _ => None,
        }
    };

    for &conj in conjuncts {
        let Expr::Binary { left, op, right } = conj else {
            continue;
        };
        let is_null_eq = match op {
            BinaryOp::Eq => false,
            BinaryOp::Is => true,
            _ => continue,
        };
        let Some((l_is_left, a_idx)) = resolve_side(left) else {
            continue;
        };
        let Some((r_is_left, b_idx)) = resolve_side(right) else {
            continue;
        };
        if l_is_left == r_is_left {
            continue;
        }
        let (l_idx, r_idx) = if l_is_left {
            (a_idx, b_idx)
        } else {
            (b_idx, a_idx)
        };
        let comp_aff =
            sqlite_compare_affinity(left_cols[l_idx].affinity, right_cols[r_idx].affinity);

        // Verify all values in `left_rows[..][l_idx]` fit exact HashJoinKey representation
        if left_rows
            .iter()
            .any(|lr| HashJoinKey::from_value_exact(&lr.vals[l_idx], comp_aff).is_none())
        {
            continue;
        }

        let mut buckets: HashMap<HashJoinKey, Vec<usize>> =
            HashMap::with_capacity(eligible_right.len());
        let mut exact_ok = true;
        for &ri in eligible_right {
            let rv = &right_rows[ri].vals[r_idx];
            let Some(key) = HashJoinKey::from_value_exact(rv, comp_aff) else {
                exact_ok = false;
                break;
            };
            if matches!(key, HashJoinKey::Null) && !is_null_eq {
                continue;
            }
            buckets.entry(key).or_default().push(ri);
        }
        if exact_ok {
            return Some(EquiHashIndex {
                l_idx,
                is_null_eq,
                comp_aff,
                buckets,
            });
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn apply_join(
    ctx: &mut ExecContext<'_>,
    left_rows: Vec<EvalRow>,
    right_rows: Vec<EvalRow>,
    left_cols_fallback: &[ScopedColumn],
    right_cols_fallback: &[ScopedColumn],
    join: &JoinClause,
    extra_where_conjuncts: &[&Expr],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<EvalRow>> {
    let left_sample_cols: Vec<ScopedColumn> = left_rows
        .first()
        .map_or_else(|| left_cols_fallback.to_vec(), |r| r.cols.to_vec());
    let right_sample_cols: Vec<ScopedColumn> = right_rows
        .first()
        .map_or_else(|| right_cols_fallback.to_vec(), |r| r.cols.to_vec());

    let using_cols = compute_join_using_cols(&left_sample_cols, &right_sample_cols, join);
    let merged_cols = merge_join_scoped_columns(&left_sample_cols, &right_sample_cols, join);
    let shared_merged_cols: Rc<[ScopedColumn]> = Rc::from(merged_cols.as_slice());
    let left_is_using: Vec<bool> = left_sample_cols
        .iter()
        .map(|lc| !lc.hidden_rowid && using_cols.iter().any(|u| u.eq_ignore_ascii_case(&lc.name)))
        .collect();

    let combine_rows = |l: Option<&EvalRow>, r: Option<&EvalRow>| -> EvalRow {
        let mut vals = Vec::with_capacity(merged_cols.len());
        for (i, lc) in left_sample_cols.iter().enumerate() {
            let mut val = l.map_or(Value::Null, |row| row.vals[i].clone());
            if left_is_using[i]
                && val.is_null()
                && let Some(r_row) = r
                && let Some((rv, _)) = r_row.lookup(None, &lc.name).ok().flatten()
            {
                val = rv;
            }
            vals.push(val);
        }
        for j in 0..right_sample_cols.len() {
            vals.push(r.map_or(Value::Null, |row| row.vals[j].clone()));
        }
        EvalRow::new(Rc::clone(&shared_merged_cols), vals)
    };

    let mut all_conjuncts: Vec<&Expr> = Vec::new();
    if using_cols.is_empty() {
        if let Some(JoinConstraint::On(on_expr)) = &join.constraint {
            split_where_conjuncts(on_expr, &mut all_conjuncts);
        }
        all_conjuncts.extend_from_slice(extra_where_conjuncts);
    }

    let mut left_only_conjuncts: Vec<&Expr> = Vec::new();
    let mut right_only_conjuncts: Vec<&Expr> = Vec::new();
    if using_cols.is_empty() && !left_rows.is_empty() && !right_rows.is_empty() {
        for &c in &all_conjuncts {
            if expr_can_eval_with_cols(c, &left_sample_cols, &merged_cols, outer_scopes) {
                left_only_conjuncts.push(c);
            } else if expr_can_eval_with_cols(c, &right_sample_cols, &merged_cols, outer_scopes) {
                right_only_conjuncts.push(c);
            }
        }
    }

    let mut eligible_right: Vec<usize> = Vec::with_capacity(right_rows.len());
    if !left_rows.is_empty() {
        for (ri, r) in right_rows.iter().enumerate() {
            let mut ok = true;
            if !right_only_conjuncts.is_empty() {
                let r_scopes = push_scope(outer_scopes, r);
                for &rc in &right_only_conjuncts {
                    if eval_expr(ctx, rc, params, &r_scopes, ctes, None)?.truthiness() != Some(true)
                    {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                eligible_right.push(ri);
            }
        }
    }

    let hash_index = if using_cols.is_empty() && !left_rows.is_empty() && !eligible_right.is_empty()
    {
        try_build_equi_hash_index(
            &all_conjuncts,
            &left_sample_cols,
            &right_sample_cols,
            &merged_cols,
            &left_rows,
            &right_rows,
            &eligible_right,
        )
    } else {
        None
    };

    let mut out = Vec::new();
    let mut right_matched = vec![false; right_rows.len()];

    for l in &left_rows {
        ctx.step_tick()?;
        let mut any_match = false;
        let mut left_ok = true;
        if !right_rows.is_empty() && !left_only_conjuncts.is_empty() {
            let l_scopes = push_scope(outer_scopes, l);
            for &lc in &left_only_conjuncts {
                if eval_expr(ctx, lc, params, &l_scopes, ctes, None)?.truthiness() != Some(true) {
                    left_ok = false;
                    break;
                }
            }
        }

        if left_ok && !eligible_right.is_empty() {
            let candidates: &[usize] = if let Some(ref hi) = hash_index {
                let key = HashJoinKey::from_value_exact(&l.vals[hi.l_idx], hi.comp_aff)
                    .expect("verified exact key");
                if matches!(key, HashJoinKey::Null) && !hi.is_null_eq {
                    &[]
                } else {
                    hi.buckets.get(&key).map_or(&[][..], Vec::as_slice)
                }
            } else {
                &eligible_right
            };

            for &ri in candidates {
                ctx.step_tick()?;
                let r = &right_rows[ri];
                let mut candidate_opt: Option<EvalRow> = None;
                let matches = if !using_cols.is_empty() {
                    let mut ok = true;
                    for u in &using_cols {
                        let (lv, lcoll, laff) = l.lookup_full(None, u)?.unwrap_or((
                            Value::Null,
                            Collation::Binary,
                            None,
                        ));
                        let (rv, rcoll, raff) = r.lookup_full(None, u)?.unwrap_or((
                            Value::Null,
                            Collation::Binary,
                            None,
                        ));
                        let coll = if lcoll != Collation::Binary {
                            lcoll
                        } else {
                            rcoll
                        };
                        let comp_aff = sqlite_compare_affinity(laff, raff);
                        if eval_comparison_with_affinity(
                            Opcode::Eq,
                            &lv,
                            &rv,
                            comp_aff,
                            coll,
                            ctx.encoding(),
                        )
                        .truthiness()
                            != Some(true)
                        {
                            ok = false;
                            break;
                        }
                    }
                    ok
                } else if join.constraint.is_some() || !extra_where_conjuncts.is_empty() {
                    let cand = combine_rows(Some(l), Some(r));
                    let mut ok = true;
                    if outer_scopes.is_empty() {
                        let single_scope = std::slice::from_ref(&cand);
                        if let Some(JoinConstraint::On(on_expr)) = &join.constraint
                            && eval_expr(ctx, on_expr, params, single_scope, ctes, None)?
                                .truthiness()
                                != Some(true)
                        {
                            ok = false;
                        }
                        if ok {
                            for &wc in extra_where_conjuncts {
                                if eval_expr(ctx, wc, params, single_scope, ctes, None)?
                                    .truthiness()
                                    != Some(true)
                                {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    } else {
                        let scopes = push_scope(outer_scopes, &cand);
                        if let Some(JoinConstraint::On(on_expr)) = &join.constraint
                            && eval_expr(ctx, on_expr, params, &scopes, ctes, None)?.truthiness()
                                != Some(true)
                        {
                            ok = false;
                        }
                        if ok {
                            for &wc in extra_where_conjuncts {
                                if eval_expr(ctx, wc, params, &scopes, ctes, None)?.truthiness()
                                    != Some(true)
                                {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                    }
                    if ok {
                        candidate_opt = Some(cand);
                    }
                    ok
                } else {
                    true
                };

                if matches {
                    any_match = true;
                    right_matched[ri] = true;
                    out.push(candidate_opt.unwrap_or_else(|| combine_rows(Some(l), Some(r))));
                }
            }
        }

        if !any_match && matches!(join.join_type, JoinType::Left | JoinType::Full) {
            out.push(combine_rows(Some(l), None));
        }
    }

    if matches!(join.join_type, JoinType::Right | JoinType::Full) {
        for (ri, r) in right_rows.iter().enumerate() {
            if !right_matched[ri] {
                out.push(combine_rows(None, Some(r)));
            }
        }
    }

    Ok(out)
}

fn schema_cols_preview(
    ctx: &mut ExecContext<'_>,
    fc: &FromClause,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    let mut cols = schema_cols_for_tos(ctx, &fc.head, params, outer_scopes, ctes)?;
    for j in &fc.joins {
        if let Some(rcols) = schema_cols_for_tos(ctx, &j.right, params, outer_scopes, ctes) {
            cols = merge_join_scoped_columns(&cols, &rcols, j);
        }
    }
    Some(cols)
}

fn schema_cols_for_tos(
    ctx: &mut ExecContext<'_>,
    tos: &TableOrSubquery,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Option<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Ok(Some(cte)) = resolve_cte(ctx, ctes, &name.name, params, outer_scopes)
            {
                let cte_meta = infer_cte_meta_by_name(&name.name, outer_scopes, ctx.db_set, ctes);
                return Some(
                    cte.column_names
                        .iter()
                        .enumerate()
                        .map(|(i, cn)| {
                            let (aff, coll) = cte_meta
                                .get(i)
                                .copied()
                                .unwrap_or((None, Collation::Binary));
                            ScopedColumn {
                                schema_name: None,
                                table_alias: Some(eff.clone()),
                                name: cn.clone(),
                                collation: coll,
                                affinity: aff,
                                hidden_rowid: false,
                                omit_from_star: false,
                                merged_using: false,
                            }
                        })
                        .collect(),
                );
            }
            if ctx
                .db_set
                .resolve_catalog_table(name)
                .ok()
                .flatten()
                .is_none()
                && let Some((v_db_id, view_ref)) = ctx.db_set.find_view_in_set(name)
            {
                return static_view_scoped_cols(v_db_id, view_ref, &eff, ctx.db_set, ctes, &[]);
            }
            let (_, tbl, schema_lbl) = resolve_table_for_query_in_set(ctx.db_set, name).ok()?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.clone()),
                    table_alias: Some(eff.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                let rid_col_name = tbl
                    .rowid_alias_col
                    .map(|idx| tbl.columns[idx].name.clone())
                    .unwrap_or_else(|| "rowid".to_owned());
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl),
                    table_alias: Some(eff),
                    name: rid_col_name,
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    omit_from_star: false,
                    merged_using: false,
                });
            }
            Some(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, ctx.db_set, ctes);
            let sub_out = exec_select(ctx, query, params, outer_scopes, ctes).ok()?;
            let disambig_names = disambiguate_view_column_names(&sub_out.column_names);
            Some(
                disambig_names
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = sub_meta
                            .get(i)
                            .copied()
                            .unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: alias.clone(),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect(),
            )
        }
    }
}

#[allow(clippy::type_complexity)]
fn expand_returning_projection(
    projection: &[ResultColumn],
    sample_cols: &[ScopedColumn],
) -> Result<(Vec<String>, Vec<(Expr, String, Option<String>)>)> {
    if projection
        .iter()
        .any(|rc| matches!(rc, ResultColumn::TableStar { .. }))
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "RETURNING may not use \"TABLE.*\" wildcards",
        ));
    }
    expand_projection(projection, sample_cols)
}

#[allow(clippy::type_complexity)]
fn expand_projection(
    projection: &[ResultColumn],
    sample_cols: &[ScopedColumn],
) -> Result<(Vec<String>, Vec<(Expr, String, Option<String>)>)> {
    let mut names = Vec::new();
    let mut expanded = Vec::new();
    for rc in projection {
        match rc {
            ResultColumn::Star => {
                for (slot_idx, sc) in sample_cols.iter().enumerate() {
                    if sc.hidden_rowid || sc.omit_from_star {
                        continue;
                    }
                    let e = Expr::Column(ColumnRef {
                        schema: Some(STAR_SLOT_SCHEMA.to_owned()),
                        table: None,
                        column: slot_idx.to_string(),
                    });
                    names.push(sc.name.clone());
                    expanded.push((e, sc.name.clone(), None));
                }
            }
            ResultColumn::TableStar {
                schema: tschema,
                table: tname,
            } => {
                let mut found = false;
                for (slot_idx, sc) in sample_cols.iter().enumerate() {
                    if sc.hidden_rowid {
                        continue;
                    }
                    if sc.matches_qualifier(tschema.as_deref(), Some(tname)) {
                        found = true;
                        let e = Expr::Column(ColumnRef {
                            schema: Some(STAR_SLOT_SCHEMA.to_owned()),
                            table: None,
                            column: slot_idx.to_string(),
                        });
                        names.push(sc.name.clone());
                        expanded.push((e, sc.name.clone(), None));
                    }
                }
                if !found {
                    let disp = match tschema {
                        Some(s) => format!("{s}.{tname}"),
                        None => tname.clone(),
                    };
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {disp}"),
                    ));
                }
            }
            ResultColumn::Expr {
                expr,
                alias,
                span_text,
            } => {
                let label = alias.clone().unwrap_or_else(|| {
                    resolve_unaliased_column_label(expr, span_text.as_deref(), sample_cols)
                });
                names.push(label.clone());
                expanded.push((expr.clone(), label, alias.clone()));
            }
        }
    }
    Ok((names, expanded))
}

fn resolve_unaliased_column_label(
    expr: &Expr,
    span_text: Option<&str>,
    sample_cols: &[ScopedColumn],
) -> String {
    if let Expr::Column(cref) = expr {
        if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA)
            && let Ok(idx) = cref.column.parse::<usize>()
            && let Some(sc) = sample_cols.get(idx)
        {
            return sc.name.clone();
        }
        let mut matched_name: Option<&str> = None;
        let mut count = 0usize;
        for sc in sample_cols {
            if sc.hidden_rowid {
                continue;
            }
            if sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref())
                && sc.name.eq_ignore_ascii_case(&cref.column)
            {
                matched_name = Some(&sc.name);
                count += 1;
            }
        }
        if count == 1
            && let Some(nm) = matched_name
        {
            return nm.to_owned();
        }
        if count == 0
            && matches!(
                cref.column.to_ascii_lowercase().as_str(),
                "rowid" | "oid" | "_rowid_"
            )
        {
            let mut rid_count = 0usize;
            let mut rid_name: Option<&str> = None;
            for sc in sample_cols {
                if sc.hidden_rowid
                    && sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref())
                {
                    rid_name = Some(&sc.name);
                    rid_count += 1;
                }
            }
            if rid_count == 1
                && let Some(rn) = rid_name
            {
                return rn.to_owned();
            }
        }
        return cref.column.clone();
    }
    span_text
        .map(str::to_owned)
        .unwrap_or_else(|| expr_column_label(expr))
}

fn build_groups(
    ctx: &mut ExecContext<'_>,
    rows: Vec<EvalRow>,
    group_by: &[Expr],
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<EvalRow>>> {
    if group_by.is_empty() {
        // Ungrouped aggregate (`SELECT COUNT(*) FROM t`): always yields 1 group even if `rows` is empty!
        return Ok(vec![rows]);
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let mut groups: Vec<(Vec<Value>, Vec<Collation>, Vec<EvalRow>)> = Vec::new();
    for row in rows {
        ctx.step_tick()?;
        let scopes = push_scope(outer_scopes, &row);
        let mut g_vals = Vec::with_capacity(group_by.len());
        let mut g_colls = Vec::with_capacity(group_by.len());
        for ge in group_by {
            let v = eval_expr(ctx, ge, params, &scopes, ctes, None)?;
            let coll = match ge {
                Expr::Collate { collation, .. } => {
                    Collation::from_name(collation).unwrap_or_default()
                }
                Expr::Column(cref) => row.lookup_cref(cref)?.map_or(Collation::Binary, |(_, c)| c),
                _ => Collation::Binary,
            };
            g_vals.push(v);
            g_colls.push(coll);
        }

        let enc = ctx.encoding();
        let existing = groups.iter_mut().find(|(kv, colls, _)| {
            kv.iter()
                .zip(g_vals.iter())
                .zip(colls.iter())
                .all(|((a, b), &c)| match (a.is_null(), b.is_null()) {
                    (true, true) => true,
                    (true, false) | (false, true) => false,
                    (false, false) => a.cmp_sqlite_with_encoding(b, c, enc) == Ordering::Equal,
                })
        });
        if let Some((_, _, bucket)) = existing {
            bucket.push(row);
        } else {
            groups.push((g_vals, g_colls, vec![row]));
        }
    }

    // Sort groups in SQLite's natural GROUP BY B-tree order!
    let enc = ctx.encoding();
    groups.sort_by(|(ka, ca, _), (kb, _, _)| {
        let specs: Vec<KeySortOrder> = ca
            .iter()
            .map(|&c| KeySortOrder {
                desc: false,
                nulls_last: false,
                collation: c,
            })
            .collect();
        compare_value_slices(ka, kb, &specs, enc)
    });

    Ok(groups.into_iter().map(|(_, _, b)| b).collect())
}

fn infer_expr_collation_from_cols(expr: &Expr, sample_cols: &[ScopedColumn]) -> Collation {
    match expr {
        Expr::Collate { collation, .. } => Collation::from_name(collation).unwrap_or_default(),
        Expr::Column(cref) => {
            if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                return cref
                    .column
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| sample_cols.get(idx))
                    .map_or(Collation::Binary, |sc| sc.collation);
            }
            for sc in sample_cols {
                if sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                    continue;
                }
                if sc.name.eq_ignore_ascii_case(&cref.column) {
                    return sc.collation;
                }
            }
            Collation::Binary
        }
        _ => Collation::Binary,
    }
}

fn infer_order_by_collation(
    ob_expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
) -> Collation {
    if let Expr::Collate { collation, .. } = ob_expr {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Integer(ord) = ob_expr
        && *ord >= 1
        && (*ord as usize) <= expanded_proj.len()
    {
        return infer_expr_collation_from_cols(&expanded_proj[(*ord as usize) - 1].0, sample_cols);
    }
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = ob_expr
        && let Some((idx, _)) = expanded_proj
            .iter()
            .enumerate()
            .find(|(_, (_, label, al))| {
                al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                    || label.eq_ignore_ascii_case(column)
            })
    {
        let in_row = sample_cols
            .iter()
            .any(|sc| !sc.hidden_rowid && sc.name.eq_ignore_ascii_case(column));
        if expanded_proj[idx].2.is_some() || !in_row {
            return infer_expr_collation_from_cols(&expanded_proj[idx].0, sample_cols);
        }
    }
    infer_expr_collation_from_cols(ob_expr, sample_cols)
}

#[allow(clippy::too_many_arguments)]
fn eval_order_term_in_scope(
    ctx: &mut ExecContext<'_>,
    ob_expr: &Expr,
    proj_vals: &[Value],
    expanded_proj: &[(Expr, String, Option<String>)],
    scopes: &[EvalRow],
    params: &[Value],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    let mut inner = ob_expr;
    while let Expr::Collate { expr, .. } = inner {
        inner = expr;
    }
    if let Expr::Integer(ord) = inner
        && *ord >= 1
        && (*ord as usize) <= proj_vals.len()
    {
        return Ok(proj_vals[(*ord as usize) - 1].clone());
    }
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = inner
        && let Some((idx, _)) = expanded_proj
            .iter()
            .enumerate()
            .find(|(_, (_, label, al))| {
                al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                    || label.eq_ignore_ascii_case(column)
            })
    {
        // Prefer alias match if column is not directly in scope or is an explicit alias
        let in_row = scopes
            .last()
            .and_then(|r| r.lookup(None, column).ok().flatten())
            .is_some();
        if expanded_proj[idx].2.is_some() || !in_row {
            return Ok(proj_vals[idx].clone());
        }
    }
    if let Some((idx, _)) = expanded_proj
        .iter()
        .enumerate()
        .find(|(_, (pe, _, _))| pe == inner)
    {
        return Ok(proj_vals[idx].clone());
    }
    let substituted = subst_order_by_aliases_in_expr(ob_expr, proj_vals, expanded_proj, scopes);
    eval_expr(ctx, &substituted, params, scopes, ctes, agg_group)
}

fn value_to_literal_expr(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::Null,
        Value::Integer(i) => Expr::Integer(*i),
        Value::Real(f) => Expr::Float(*f),
        Value::Text(s) => Expr::String(s.clone()),
        Value::Blob(b) => Expr::Blob(b.clone()),
    }
}

fn subst_order_by_aliases_in_expr(
    expr: &Expr,
    proj_vals: &[Value],
    expanded_proj: &[(Expr, String, Option<String>)],
    scopes: &[EvalRow],
) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            let in_row = scopes
                .last()
                .and_then(|r| r.lookup(None, column).ok().flatten())
                .is_some();
            if !in_row
                && let Some((idx, _)) =
                    expanded_proj
                        .iter()
                        .enumerate()
                        .find(|(_, (_, label, al))| {
                            al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                                || label.eq_ignore_ascii_case(column)
                        })
            {
                return value_to_literal_expr(&proj_vals[idx]);
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            collation: collation.clone(),
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            type_name: type_name.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_order_by_aliases_in_expr(
                inner,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            negated: *negated,
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_order_by_aliases_in_expr(
                left,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            op: *op,
            right: Box::new(subst_order_by_aliases_in_expr(
                right,
                proj_vals,
                expanded_proj,
                scopes,
            )),
        },
        Expr::Between {
            expr: val,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            low: Box::new(subst_order_by_aliases_in_expr(
                low,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            high: Box::new(subst_order_by_aliases_in_expr(
                high,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: val,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            list: list
                .iter()
                .map(|e| subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes))
                .collect(),
            negated: *negated,
        },
        Expr::Like {
            expr: val,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_order_by_aliases_in_expr(
                val,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            pattern: Box::new(subst_order_by_aliases_in_expr(
                pattern,
                proj_vals,
                expanded_proj,
                scopes,
            )),
            escape: escape.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_order_by_aliases_in_expr(w, proj_vals, expanded_proj, scopes),
                        subst_order_by_aliases_in_expr(t, proj_vals, expanded_proj, scopes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| {
                            subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes)
                        })
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_in_expr(
                    e,
                    proj_vals,
                    expanded_proj,
                    scopes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_order_by_aliases_in_expr(e, proj_vals, expanded_proj, scopes))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

fn resolve_compound_order_by_specs(
    all_arm_col_names: &[Vec<String>],
    col_collations: &[Collation],
    order_by: &[OrderByTerm],
) -> Result<Vec<(usize, KeySortOrder, Option<String>)>> {
    let width = all_arm_col_names.first().map_or(0, Vec::len);
    let mut specs = Vec::with_capacity(order_by.len());
    for (ob_idx, ob) in order_by.iter().enumerate() {
        let mut inner = &ob.expr;
        while let Expr::Collate { expr, .. } = inner {
            inner = expr.as_ref();
        }
        let (coll, custom_coll) = match &ob.expr {
            Expr::Collate { collation, .. } => match Collation::from_name(collation) {
                Some(builtin) => (builtin, None),
                None => (Collation::Binary, Some(collation.clone())),
            },
            _ => (Collation::Binary, None),
        };
        if let Some(ord) = strip_collate_and_unary_plus(inner) {
            if ord < 1 || (ord as usize) > width {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term out of range - should be between 1 and {}",
                        ordinal_suffix(ob_idx + 1),
                        width
                    ),
                ));
            }
            let cidx = (ord as usize) - 1;
            let eff_coll = if matches!(&ob.expr, Expr::Collate { .. }) {
                coll
            } else {
                col_collations
                    .get(cidx)
                    .copied()
                    .unwrap_or(Collation::Binary)
            };
            specs.push((
                cidx,
                KeySortOrder {
                    desc: ob.desc,
                    nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                    collation: eff_coll,
                },
                custom_coll,
            ));
            continue;
        }
        let target_label = match inner {
            Expr::Column(ColumnRef {
                schema: None,
                table: None,
                column,
            }) => Some(column.clone()),
            other => Some(expr_column_label(other)),
        };
        let col_idx = target_label
            .and_then(|lbl| {
                all_arm_col_names.iter().find_map(|arm_cols| {
                    arm_cols.iter().position(|cn| cn.eq_ignore_ascii_case(&lbl))
                })
            })
            .ok_or_else(|| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term does not match any column in the result set",
                        ordinal_suffix(ob_idx + 1)
                    ),
                )
            })?;
        let eff_coll = if matches!(&ob.expr, Expr::Collate { .. }) {
            coll
        } else {
            col_collations
                .get(col_idx)
                .copied()
                .unwrap_or(Collation::Binary)
        };
        specs.push((
            col_idx,
            KeySortOrder {
                desc: ob.desc,
                nulls_last: ob.nulls_last.unwrap_or(ob.desc),
                collation: eff_coll,
            },
            custom_coll,
        ));
    }
    Ok(specs)
}

fn compare_compound_order_rows(
    a: &[Value],
    b: &[Value],
    specs: &[(usize, KeySortOrder, Option<String>)],
    collations: &CollationRegistry,
    enc: TextEncoding,
) -> Ordering {
    for (idx, spec, custom_coll) in specs {
        let va = &a[*idx];
        let vb = &b[*idx];
        let ord = if let (Some(cname), Value::Text(sa), Value::Text(sb)) = (custom_coll, va, vb) {
            let raw = collations
                .compare(cname, sa, sb, enc)
                .unwrap_or(Ordering::Equal);
            if spec.desc { raw.reverse() } else { raw }
        } else {
            compare_single_value(va, vb, *spec, enc)
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn sort_materialized_output(
    rows: &mut [Vec<Value>],
    all_arm_col_names: &[Vec<String>],
    col_collations: &[Collation],
    order_by: &[OrderByTerm],
    enc: TextEncoding,
) -> Result<()> {
    let specs = resolve_compound_order_by_specs(all_arm_col_names, col_collations, order_by)?;
    rows.sort_by(|a, b| {
        for (idx, spec, _) in &specs {
            let ord = compare_single_value(&a[*idx], &b[*idx], *spec, enc);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    Ok(())
}

fn apply_limit_offset(
    ctx: &mut ExecContext<'_>,
    rows: Vec<Vec<Value>>,
    limit: Option<&LimitClause>,
    params: &[Value],
    outer_scopes: &[EvalRow],
    ctes: &CteStore,
) -> Result<Vec<Vec<Value>>> {
    let Some(lim) = limit else {
        return Ok(rows);
    };
    let lim_val = eval_expr(ctx, &lim.limit, params, outer_scopes, ctes, None)?.to_integer_lossy();
    let off_val = match &lim.offset {
        Some(oe) => eval_expr(ctx, oe, params, outer_scopes, ctes, None)?
            .to_integer_lossy()
            .max(0) as usize,
        None => 0,
    };

    let iter = rows.into_iter().skip(off_val);
    if lim_val < 0 {
        Ok(iter.collect())
    } else {
        Ok(iter.take(lim_val as usize).collect())
    }
}

fn expr_contains_conjunct(haystack: &Expr, needle: &Expr) -> bool {
    if haystack == needle {
        return true;
    }
    if let Expr::Binary {
        left,
        op: BinaryOp::And,
        right,
    } = haystack
    {
        return expr_contains_conjunct(left, needle) || expr_contains_conjunct(right, needle);
    }
    false
}

fn col_in_scoped_cols(column: &str, sample_cols: &[ScopedColumn]) -> bool {
    sample_cols.iter().any(|sc| {
        if sc.hidden_rowid {
            column.eq_ignore_ascii_case("rowid")
                || column.eq_ignore_ascii_case("_rowid_")
                || column.eq_ignore_ascii_case("oid")
        } else {
            sc.name.eq_ignore_ascii_case(column)
        }
    })
}

fn subst_select_aliases_in_expr_with_ctes(
    expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Expr {
    let active_aliases: Vec<(String, Expr)> = expanded_proj
        .iter()
        .filter_map(|(pe, _, al)| {
            let alias = al.as_ref()?;
            if col_in_scoped_cols(alias, sample_cols) {
                None
            } else {
                Some((alias.clone(), pe.clone()))
            }
        })
        .collect();
    if active_aliases.is_empty() {
        return expr.clone();
    }
    subst_active_aliases_in_expr(expr, &active_aliases, db_set, ctes)
}

fn subst_active_aliases_in_select(
    sel: &SelectStmt,
    active_aliases: &[(String, Expr)],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> SelectStmt {
    if active_aliases.is_empty() {
        return sel.clone();
    }
    let mut rewritten_ctes = Vec::with_capacity(sel.ctes.len());
    for (i, cte) in sel.ctes.iter().enumerate() {
        let cte_scope = ctes.with_scope(&rewritten_ctes[..i]);
        let mut next_cte = cte.clone();
        next_cte.query =
            subst_active_aliases_in_select(&cte.query, active_aliases, db_set, &cte_scope);
        rewritten_ctes.push(next_cte);
    }
    let body_ctes = ctes.with_scope(&rewritten_ctes);
    let head = subst_active_aliases_in_core(&sel.body.head, active_aliases, db_set, &body_ctes);
    let compounds = sel
        .body
        .compounds
        .iter()
        .map(|(op, c)| {
            (
                *op,
                subst_active_aliases_in_core(c, active_aliases, db_set, &body_ctes),
            )
        })
        .collect();
    let head_filtered = filter_aliases_for_core(&sel.body.head, active_aliases, db_set, &body_ctes);
    let order_by = sel
        .order_by
        .iter()
        .map(|ob| OrderByTerm {
            expr: subst_active_aliases_in_expr(&ob.expr, &head_filtered, db_set, &body_ctes),
            desc: ob.desc,
            nulls_last: ob.nulls_last,
        })
        .collect();
    let limit = sel.limit.as_ref().map(|lim| LimitClause {
        limit: subst_active_aliases_in_expr(&lim.limit, active_aliases, db_set, &body_ctes),
        offset: lim
            .offset
            .as_ref()
            .map(|o| subst_active_aliases_in_expr(o, active_aliases, db_set, &body_ctes)),
    });
    SelectStmt {
        ctes: rewritten_ctes,
        body: SelectBody { head, compounds },
        order_by,
        limit,
    }
}

fn filter_aliases_for_core(
    core: &SelectCore,
    active_aliases: &[(String, Expr)],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Vec<(String, Expr)> {
    match core {
        SelectCore::Values(_) => active_aliases.to_vec(),
        SelectCore::Select {
            projection, from, ..
        } => {
            let inner_cols = from
                .as_ref()
                .and_then(|fc| static_schema_cols_for_from(fc, &[], db_set, ctes, &mut Vec::new()))
                .unwrap_or_default();
            active_aliases
                .iter()
                .filter(|(al, _)| {
                    !col_in_scoped_cols(al, &inner_cols)
                        && !projection.iter().any(|rc| {
                            matches!(
                                rc,
                                ResultColumn::Expr {
                                    alias: Some(ia),
                                    ..
                                } if ia.eq_ignore_ascii_case(al)
                            )
                        })
                })
                .cloned()
                .collect()
        }
    }
}

fn subst_active_aliases_in_tos(
    tos: &TableOrSubquery,
    active_aliases: &[(String, Expr)],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> TableOrSubquery {
    match tos {
        TableOrSubquery::Table { .. } => tos.clone(),
        TableOrSubquery::Subquery { query, alias } => TableOrSubquery::Subquery {
            query: Box::new(subst_active_aliases_in_select(
                query,
                active_aliases,
                db_set,
                ctes,
            )),
            alias: alias.clone(),
        },
    }
}

fn subst_active_aliases_in_core(
    core: &SelectCore,
    active_aliases: &[(String, Expr)],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> SelectCore {
    match core {
        SelectCore::Values(rows) => SelectCore::Values(
            rows.iter()
                .map(|r| {
                    r.iter()
                        .map(|e| subst_active_aliases_in_expr(e, active_aliases, db_set, ctes))
                        .collect()
                })
                .collect(),
        ),
        SelectCore::Select {
            distinct,
            projection,
            from,
            selection,
            group_by,
            having,
        } => {
            let filtered = filter_aliases_for_core(core, active_aliases, db_set, ctes);
            let new_from = from.as_ref().map(|fc| FromClause {
                head: subst_active_aliases_in_tos(&fc.head, active_aliases, db_set, ctes),
                joins: fc
                    .joins
                    .iter()
                    .map(|j| JoinClause {
                        natural: j.natural,
                        join_type: j.join_type,
                        right: subst_active_aliases_in_tos(&j.right, active_aliases, db_set, ctes),
                        constraint: j.constraint.as_ref().map(|jc| match jc {
                            JoinConstraint::On(e) => JoinConstraint::On(
                                subst_active_aliases_in_expr(e, &filtered, db_set, ctes),
                            ),
                            JoinConstraint::Using(u) => JoinConstraint::Using(u.clone()),
                        }),
                    })
                    .collect(),
            });
            let new_proj = projection
                .iter()
                .map(|rc| match rc {
                    ResultColumn::Expr {
                        expr,
                        alias,
                        span_text,
                    } => ResultColumn::Expr {
                        expr: subst_active_aliases_in_expr(expr, &filtered, db_set, ctes),
                        alias: alias.clone(),
                        span_text: span_text.clone(),
                    },
                    other => other.clone(),
                })
                .collect();
            SelectCore::Select {
                distinct: *distinct,
                projection: new_proj,
                from: new_from,
                selection: selection
                    .as_ref()
                    .map(|w| subst_active_aliases_in_expr(w, &filtered, db_set, ctes)),
                group_by: group_by
                    .iter()
                    .map(|g| subst_active_aliases_in_expr(g, &filtered, db_set, ctes))
                    .collect(),
                having: having
                    .as_ref()
                    .map(|h| subst_active_aliases_in_expr(h, &filtered, db_set, ctes)),
            }
        }
    }
}

fn subst_active_aliases_in_expr(
    expr: &Expr,
    active_aliases: &[(String, Expr)],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Expr {
    if active_aliases.is_empty() {
        return expr.clone();
    }
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            if let Some((_, pe)) = active_aliases
                .iter()
                .find(|(al, _)| al.eq_ignore_ascii_case(column))
            {
                return pe.clone();
            }
            expr.clone()
        }
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_active_aliases_in_expr(
                inner,
                active_aliases,
                db_set,
                ctes,
            )),
            collation: collation.clone(),
        },
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_active_aliases_in_expr(
                inner,
                active_aliases,
                db_set,
                ctes,
            )),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_active_aliases_in_expr(
                left,
                active_aliases,
                db_set,
                ctes,
            )),
            op: *op,
            right: Box::new(subst_active_aliases_in_expr(
                right,
                active_aliases,
                db_set,
                ctes,
            )),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_active_aliases_in_expr(
                inner,
                active_aliases,
                db_set,
                ctes,
            )),
            negated: *negated,
        },
        Expr::Between {
            expr: ve,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_active_aliases_in_expr(
                ve,
                active_aliases,
                db_set,
                ctes,
            )),
            low: Box::new(subst_active_aliases_in_expr(
                low,
                active_aliases,
                db_set,
                ctes,
            )),
            high: Box::new(subst_active_aliases_in_expr(
                high,
                active_aliases,
                db_set,
                ctes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: le,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_active_aliases_in_expr(
                le,
                active_aliases,
                db_set,
                ctes,
            )),
            list: list
                .iter()
                .map(|e| subst_active_aliases_in_expr(e, active_aliases, db_set, ctes))
                .collect(),
            negated: *negated,
        },
        Expr::InSubquery {
            expr: le,
            subquery,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(subst_active_aliases_in_expr(
                le,
                active_aliases,
                db_set,
                ctes,
            )),
            subquery: Box::new(subst_active_aliases_in_select(
                subquery,
                active_aliases,
                db_set,
                ctes,
            )),
            negated: *negated,
        },
        Expr::InTable {
            expr: le,
            table,
            negated,
        } => Expr::InTable {
            expr: Box::new(subst_active_aliases_in_expr(
                le,
                active_aliases,
                db_set,
                ctes,
            )),
            table: table.clone(),
            negated: *negated,
        },
        Expr::Like {
            expr: te,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_active_aliases_in_expr(
                te,
                active_aliases,
                db_set,
                ctes,
            )),
            pattern: Box::new(subst_active_aliases_in_expr(
                pattern,
                active_aliases,
                db_set,
                ctes,
            )),
            escape: escape.as_ref().map(|ee| {
                Box::new(subst_active_aliases_in_expr(
                    ee,
                    active_aliases,
                    db_set,
                    ctes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_active_aliases_in_expr(
                inner,
                active_aliases,
                db_set,
                ctes,
            )),
            type_name: type_name.clone(),
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|oe| {
                Box::new(subst_active_aliases_in_expr(
                    oe,
                    active_aliases,
                    db_set,
                    ctes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_active_aliases_in_expr(w, active_aliases, db_set, ctes),
                        subst_active_aliases_in_expr(t, active_aliases, db_set, ctes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|ee| {
                Box::new(subst_active_aliases_in_expr(
                    ee,
                    active_aliases,
                    db_set,
                    ctes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| subst_active_aliases_in_expr(e, active_aliases, db_set, ctes))
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|fe| {
                Box::new(subst_active_aliases_in_expr(
                    fe,
                    active_aliases,
                    db_set,
                    ctes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_active_aliases_in_expr(e, active_aliases, db_set, ctes))
                .collect(),
        ),
        Expr::Subquery(sub) => Expr::Subquery(Box::new(subst_active_aliases_in_select(
            sub,
            active_aliases,
            db_set,
            ctes,
        ))),
        Expr::Exists { subquery, negated } => Expr::Exists {
            subquery: Box::new(subst_active_aliases_in_select(
                subquery,
                active_aliases,
                db_set,
                ctes,
            )),
            negated: *negated,
        },
        _ => expr.clone(),
    }
}

fn infer_cte_meta_by_name(
    name: &str,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    infer_cte_meta_guarded(name, outer_scopes, db_set, ctes, &mut visited)
}

fn infer_cte_meta_guarded(
    name: &str,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let Some((s_idx, c_idx)) = ctes.locate(name) else {
        return Vec::new();
    };
    if visited.contains(&(s_idx, c_idx)) {
        return Vec::new();
    }
    visited.push((s_idx, c_idx));
    let cte = &ctes.scopes[s_idx][c_idx];
    let sub_ctes = ctes.slice_to_scope(s_idx).with_scope(&cte.query.ctes);
    let res =
        infer_select_body_meta_guarded(&cte.query.body, outer_scopes, db_set, &sub_ctes, visited);
    visited.pop();
    res
}

fn infer_select_stmt_meta(
    sel: &SelectStmt,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    let sub_ctes = ctes.with_scope(&sel.ctes);
    infer_select_body_meta_guarded(&sel.body, outer_scopes, db_set, &sub_ctes, &mut visited)
}

fn infer_select_body_meta(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let mut visited = Vec::new();
    infer_select_body_meta_guarded(body, outer_scopes, db_set, ctes, &mut visited)
}

fn expr_data_type_mask(
    mut expr: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> u8 {
    loop {
        match expr {
            Expr::Collate { expr: inner, .. }
            | Expr::Unary {
                op: UnaryOp::Plus,
                expr: inner,
            } => {
                expr = inner;
            }
            Expr::Null => return 0,
            Expr::String(_) => return 0x02,
            Expr::Blob(_) => return 0x04,
            Expr::Binary {
                op: BinaryOp::Concat,
                ..
            } => return 0x06,
            Expr::Bind(_) | Expr::FunctionCall { .. } => return 0x07,
            Expr::Column(_) | Expr::Subquery(_) | Expr::Cast { .. } | Expr::RowValue(_) => {
                let aff =
                    infer_expr_affinity_in_scopes_guarded(expr, scopes, db_set, ctes, visited);
                return match aff {
                    Some(TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real) => {
                        0x05
                    }
                    Some(TypeAffinity::Text) => 0x06,
                    _ => 0x07,
                };
            }
            Expr::Case {
                branches,
                else_expr,
                ..
            } => {
                let mut res = 0u8;
                for (_, then_e) in branches {
                    res |= expr_data_type_mask(then_e, scopes, db_set, ctes, visited);
                }
                if let Some(el) = else_expr {
                    res |= expr_data_type_mask(el, scopes, db_set, ctes, visited);
                }
                return res;
            }
            _ => return 0x01,
        }
    }
}

fn infer_select_body_meta_guarded(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Collation)> {
    let head_meta = infer_select_core_meta(&body.head, outer_scopes, db_set, ctes, visited);
    if body.compounds.is_empty() {
        return head_meta
            .into_iter()
            .map(|(a, c, _)| (a, c.unwrap_or(Collation::Binary)))
            .collect();
    }
    let mut all_arms = Vec::with_capacity(1 + body.compounds.len());
    all_arms.push(head_meta);
    for (_, rhs_core) in &body.compounds {
        all_arms.push(infer_select_core_meta(
            rhs_core,
            outer_scopes,
            db_set,
            ctes,
            visited,
        ));
    }
    let width = all_arms[0].len();
    let mut out = Vec::with_capacity(width);
    for i in 0..width {
        let mut coll: Option<Collation> = None;
        for arm in &all_arms {
            if let Some((_, Some(c), _)) = arm.get(i) {
                coll = Some(*c);
                break;
            }
        }
        // Per `sqlite/src/select.c` (`sqlite3SubqueryColumnTypes`):
        let mut k = 0usize;
        let mut aff = all_arms[0].get(i).and_then(|(a, _, _)| *a);
        let mut mask = 0u8;
        while aff.is_none() && k + 1 < all_arms.len() {
            if let Some((_, _, m)) = all_arms[k].get(i) {
                mask |= *m;
            }
            k += 1;
            aff = all_arms[k].get(i).and_then(|(a, _, _)| *a);
        }
        for arm in &all_arms[(k + 1)..] {
            if let Some((_, _, m)) = arm.get(i) {
                mask |= *m;
            }
        }
        if (aff == Some(TypeAffinity::Text) && (mask & 0x01) != 0)
            || (matches!(
                aff,
                Some(TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real)
            ) && (mask & 0x02) != 0)
        {
            aff = Some(TypeAffinity::Blob);
        }
        out.push((aff, coll.unwrap_or(Collation::Binary)));
    }
    out
}

fn infer_select_core_meta(
    core: &SelectCore,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<(Option<TypeAffinity>, Option<Collation>, u8)> {
    match core {
        SelectCore::Values(rows) => {
            let Some(first) = rows.first() else {
                return Vec::new();
            };
            first
                .iter()
                .map(|e| {
                    let aff = infer_expr_affinity_in_scopes_guarded(
                        e,
                        outer_scopes,
                        db_set,
                        ctes,
                        visited,
                    );
                    let coll = infer_expr_explicit_or_col_collation(e, &[], outer_scopes);
                    let mask = expr_data_type_mask(e, outer_scopes, db_set, ctes, visited);
                    (aff, coll, mask)
                })
                .collect()
        }
        SelectCore::Select {
            projection, from, ..
        } => {
            let sample_cols = from
                .as_ref()
                .and_then(|fc| static_schema_cols_for_from(fc, outer_scopes, db_set, ctes, visited))
                .unwrap_or_default();
            let Ok((_, expanded)) = expand_projection(projection, &sample_cols) else {
                return Vec::new();
            };
            let dummy_row = EvalRow::new(
                Rc::from(sample_cols.as_slice()),
                vec![Value::Null; sample_cols.len()],
            );
            let full_scopes = push_scope(outer_scopes, &dummy_row);
            expanded
                .into_iter()
                .map(|(pe, _, _)| {
                    let aff = infer_expr_affinity_in_scopes_guarded(
                        &pe,
                        &full_scopes,
                        db_set,
                        ctes,
                        visited,
                    );
                    let coll =
                        infer_expr_explicit_or_col_collation(&pe, &sample_cols, outer_scopes);
                    let mask = expr_data_type_mask(&pe, &full_scopes, db_set, ctes, visited);
                    (aff, coll, mask)
                })
                .collect()
        }
    }
}

fn static_schema_cols_for_from(
    fc: &FromClause,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<Vec<ScopedColumn>> {
    let mut cols = static_schema_cols_for_tos(&fc.head, outer_scopes, db_set, ctes, visited)?;
    for j in &fc.joins {
        if let Some(rcols) =
            static_schema_cols_for_tos(&j.right, outer_scopes, db_set, ctes, visited)
        {
            cols = merge_join_scoped_columns(&cols, &rcols, j);
        }
    }
    Some(cols)
}

fn static_view_scoped_cols(
    db_id: DatabaseId,
    view: &ViewSchema,
    eff_alias: &str,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &[(usize, usize)],
) -> Option<Vec<ScopedColumn>> {
    let schema_lbl = db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());
    let cache_key = (db_id, view.name.to_ascii_lowercase());
    if let Some(cached) = ctes.static_view_cache.borrow().get(&cache_key).cloned() {
        return Some(
            cached
                .cols
                .into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = cached
                        .meta
                        .get(i)
                        .copied()
                        .unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: Some(schema_lbl.clone()),
                        table_alias: Some(eff_alias.to_owned()),
                        name: cn,
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        omit_from_star: false,
                        merged_using: false,
                    }
                })
                .collect(),
        );
    }
    if let Some((cols, meta)) = ctes.view_meta_cache.borrow().get(&cache_key).cloned() {
        return Some(
            cols.into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: Some(schema_lbl.clone()),
                        table_alias: Some(eff_alias.to_owned()),
                        name: cn,
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        omit_from_star: false,
                        merged_using: false,
                    }
                })
                .collect(),
        );
    }
    let view_ctes = ctes
        .enter_view(db_id, &view.name, db_set.expr_depth_limit())
        .ok()?;
    let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view).ok()?;
    let sub_ctes = view_ctes.with_scope(&fixed_sel.ctes);
    let mut inner_visited = visited.to_vec();
    let meta =
        infer_select_body_meta_guarded(&fixed_sel.body, &[], db_set, &sub_ctes, &mut inner_visited);
    let select_names =
        static_select_col_names(&fixed_sel.body, &[], db_set, &sub_ctes, &mut inner_visited);
    let final_names = if let Some(ref explicit) = view.columns {
        if explicit.len() != select_names.len() {
            return None;
        }
        disambiguate_view_column_names(explicit)
    } else {
        disambiguate_view_column_names(&select_names)
    };
    ctes.view_meta_cache
        .borrow_mut()
        .insert(cache_key, (final_names.clone(), meta.clone()));
    Some(
        final_names
            .into_iter()
            .enumerate()
            .map(|(i, cn)| {
                let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                ScopedColumn {
                    schema_name: Some(schema_lbl.clone()),
                    table_alias: Some(eff_alias.to_owned()),
                    name: cn,
                    collation: coll,
                    affinity: aff,
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                }
            })
            .collect(),
    )
}

fn static_schema_cols_for_tos(
    tos: &TableOrSubquery,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<Vec<ScopedColumn>> {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                if visited.contains(&(s_idx, c_idx)) {
                    return None;
                }
                let cte = &ctes.scopes[s_idx][c_idx];
                let meta = infer_cte_meta_guarded(&name.name, outer_scopes, db_set, ctes, visited);
                let cnames: Vec<String> = if let Some(ref explicit) = cte.columns {
                    disambiguate_view_column_names(explicit)
                } else {
                    visited.push((s_idx, c_idx));
                    let sub_ctes = ctes.slice_to_scope(s_idx).with_scope(&cte.query.ctes);
                    let names = static_select_col_names(
                        &cte.query.body,
                        outer_scopes,
                        db_set,
                        &sub_ctes,
                        visited,
                    );
                    visited.pop();
                    disambiguate_view_column_names(&names)
                };
                return Some(
                    cnames
                        .into_iter()
                        .enumerate()
                        .map(|(i, cn)| {
                            let (aff, coll) =
                                meta.get(i).copied().unwrap_or((None, Collation::Binary));
                            ScopedColumn {
                                schema_name: None,
                                table_alias: Some(eff.clone()),
                                name: cn,
                                collation: coll,
                                affinity: aff,
                                hidden_rowid: false,
                                omit_from_star: false,
                                merged_using: false,
                            }
                        })
                        .collect(),
                );
            }
            if db_set.resolve_catalog_table(name).ok().flatten().is_none()
                && let Some((v_db_id, view_ref)) = db_set.find_view_in_set(name)
            {
                return static_view_scoped_cols(v_db_id, view_ref, &eff, db_set, ctes, visited);
            }
            let (_db_id, tbl, schema_lbl) = resolve_table_for_query_in_set(db_set, name).ok()?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.clone()),
                    table_alias: Some(eff.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                let rid_col_name = tbl
                    .rowid_alias_col
                    .map(|idx| tbl.columns[idx].name.clone())
                    .unwrap_or_else(|| "rowid".to_owned());
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl),
                    table_alias: Some(eff),
                    name: rid_col_name,
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    omit_from_star: false,
                    merged_using: false,
                });
            }
            Some(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_ctes = ctes.with_scope(&query.ctes);
            let meta = infer_select_body_meta_guarded(
                &query.body,
                outer_scopes,
                db_set,
                &sub_ctes,
                visited,
            );
            let raw_cnames =
                static_select_col_names(&query.body, outer_scopes, db_set, &sub_ctes, visited);
            let cnames = disambiguate_view_column_names(&raw_cnames);
            Some(
                cnames
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: alias.clone(),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect(),
            )
        }
    }
}

fn static_select_col_names(
    body: &SelectBody,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Vec<String> {
    match &body.head {
        SelectCore::Values(rows) => {
            let w = rows.first().map_or(0, Vec::len);
            (1..=w).map(|i| format!("column{i}")).collect()
        }
        SelectCore::Select {
            projection, from, ..
        } => {
            let sample_cols = from
                .as_ref()
                .and_then(|fc| static_schema_cols_for_from(fc, outer_scopes, db_set, ctes, visited))
                .unwrap_or_default();
            expand_projection(projection, &sample_cols)
                .map(|(names, _)| names)
                .unwrap_or_default()
        }
    }
}

fn infer_expr_explicit_or_col_collation(
    expr: &Expr,
    sample_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> Option<Collation> {
    match expr {
        Expr::Collate { collation, .. } => {
            Some(Collation::from_name(collation).unwrap_or_default())
        }
        Expr::RowValue(elems) if elems.len() == 1 => {
            infer_expr_explicit_or_col_collation(&elems[0], sample_cols, outer_scopes)
        }
        Expr::Column(cref) => {
            if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                return cref
                    .column
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| sample_cols.get(idx))
                    .map(|sc| sc.collation);
            }
            for sc in sample_cols {
                if sc.hidden_rowid {
                    continue;
                }
                if !sc.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                    continue;
                }
                if sc.name.eq_ignore_ascii_case(&cref.column) {
                    return Some(sc.collation);
                }
            }
            for s in outer_scopes.iter().rev() {
                if let Ok(Some((_, c))) = s.lookup_cref(cref) {
                    return Some(c);
                }
            }
            None
        }
        _ => None,
    }
}

fn infer_expr_affinity_in_scopes(
    expr: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Option<TypeAffinity> {
    let mut visited = Vec::new();
    infer_expr_affinity_in_scopes_guarded(expr, scopes, db_set, ctes, &mut visited)
}

fn infer_expr_affinity_in_scopes_guarded(
    expr: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited: &mut Vec<(usize, usize)>,
) -> Option<TypeAffinity> {
    match expr {
        Expr::Collate { expr: inner, .. } => {
            infer_expr_affinity_in_scopes_guarded(inner, scopes, db_set, ctes, visited)
        }
        Expr::RowValue(elems) if elems.len() == 1 => {
            infer_expr_affinity_in_scopes_guarded(&elems[0], scopes, db_set, ctes, visited)
        }
        Expr::Cast { type_name, .. } => Some(TypeAffinity::from_declared_type(type_name)),
        Expr::Column(cref) => {
            if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                if let Some(last_scope) = scopes.last()
                    && let Ok(Some((_, _, aff))) = last_scope.lookup_cref_full(cref)
                {
                    return aff;
                }
                return None;
            }
            for s in scopes.iter().rev() {
                if let Ok(Some((_, _, aff))) = s.lookup_cref_full(cref) {
                    return aff;
                }
            }
            None
        }
        Expr::Subquery(sub) => {
            let sub_ctes = ctes.with_scope(&sub.ctes);
            infer_select_body_meta_guarded(&sub.body, scopes, db_set, &sub_ctes, visited)
                .first()
                .and_then(|(aff, _)| *aff)
        }
        _ => None,
    }
}

fn compare_value_slices(
    a: &[Value],
    b: &[Value],
    specs: &[KeySortOrder],
    enc: TextEncoding,
) -> Ordering {
    for (i, spec) in specs.iter().enumerate() {
        let ord = compare_single_value(&a[i], &b[i], *spec, enc);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn compare_single_value(va: &Value, vb: &Value, spec: KeySortOrder, enc: TextEncoding) -> Ordering {
    match (va.is_null(), vb.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if spec.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, true) => {
            if spec.nulls_last {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, false) => {
            let raw = va.cmp_sqlite_with_encoding(vb, spec.collation, enc);
            if spec.desc { raw.reverse() } else { raw }
        }
    }
}

fn push_scope(outer: &[EvalRow], inner: &EvalRow) -> Vec<EvalRow> {
    let mut v = Vec::with_capacity(outer.len() + 1);
    v.extend_from_slice(outer);
    v.push(inner.clone());
    v
}

// ============================================================================
// Expression Evaluator (`eval_expr_rt` / `eval_expr`)
// ============================================================================

fn eval_expr(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    Ok(eval_expr_rt(ctx, expr, params, scopes, ctes, agg_group)?.value)
}

fn eval_expr_rt(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<RuntimeValue> {
    match expr {
        Expr::Null => Ok(RuntimeValue::null()),
        Expr::Bool(b) => Ok(RuntimeValue::plain(Value::Integer(i64::from(*b)))),
        Expr::Integer(i) => Ok(RuntimeValue::plain(Value::Integer(*i))),
        Expr::Float(f) => Ok(RuntimeValue::plain(Value::real(*f))),
        Expr::String(s) => Ok(RuntimeValue::plain(Value::Text(s.clone()))),
        Expr::Blob(b) => Ok(RuntimeValue::plain(Value::Blob(b.clone()))),
        Expr::CurrentTimeKw(kind) => {
            let ts_ms = ctx.runtime.effective_timestamp_ms();
            Ok(RuntimeValue::plain(Value::Text(match kind {
                CurrentTimeKind::Date => format_utc_date(ts_ms),
                CurrentTimeKind::Time => format_utc_time(ts_ms),
                CurrentTimeKind::Timestamp => format_utc_timestamp(ts_ms),
            })))
        }
        Expr::Bind(bp) => {
            let idx = match bp {
                BindParam::Anonymous(i) | BindParam::Numbered(i) | BindParam::Named(_, i) => *i,
            };
            Ok(RuntimeValue::plain(
                params
                    .get(idx.saturating_sub(1))
                    .cloned()
                    .unwrap_or(Value::Null),
            ))
        }
        Expr::Column(cref) => {
            if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                if let Some(last_scope) = scopes.last()
                    && let Some((val, _)) = last_scope.lookup_cref(cref)?
                {
                    return Ok(RuntimeValue::plain(val));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {}", cref.display_name()),
                ));
            }
            for scope in scopes.iter().rev() {
                if let Some((val, _)) = scope.lookup_cref(cref)? {
                    return Ok(RuntimeValue::plain(val));
                }
            }
            // In SQLite, double-quoted string fallback is deprecated; return clear error
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {}", cref.display_name()),
            ))
        }
        Expr::Collate { expr: inner, .. } => {
            eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group)
        }
        Expr::Unary { op, expr: inner } => {
            if *op == UnaryOp::Plus {
                return eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group);
            }
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(RuntimeValue::plain(match op {
                UnaryOp::Plus => unreachable!(),
                UnaryOp::Negate => eval_unary_negate(&v),
                UnaryOp::BitNot => {
                    if v.is_null() {
                        Value::Null
                    } else {
                        Value::Integer(!v.to_integer_lossy())
                    }
                }
                UnaryOp::Not => match v.truthiness() {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(!b)),
                },
            }))
        }
        Expr::RowValue(elems) => {
            if elems.len() == 1 {
                eval_expr_rt(ctx, &elems[0], params, scopes, ctes, agg_group)
            } else {
                Err(SqliteError::new(ErrorCode::Error, "row value misused"))
            }
        }
        Expr::Binary { left, op, right } => {
            // Short-circuit AND / OR
            if *op == BinaryOp::And {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(false) {
                    return Ok(RuntimeValue::plain(Value::Integer(0)));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(RuntimeValue::plain(
                    match (lv.truthiness(), rv.truthiness()) {
                        (_, Some(false)) => Value::Integer(0),
                        (Some(true), Some(true)) => Value::Integer(1),
                        _ => Value::Null,
                    },
                ));
            }
            if *op == BinaryOp::Or {
                let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
                if lv.truthiness() == Some(true) {
                    return Ok(RuntimeValue::plain(Value::Integer(1)));
                }
                let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;
                return Ok(RuntimeValue::plain(
                    match (lv.truthiness(), rv.truthiness()) {
                        (_, Some(true)) => Value::Integer(1),
                        (Some(false), Some(false)) => Value::Integer(0),
                        _ => Value::Null,
                    },
                ));
            }

            if matches!(
                op,
                BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Is
                    | BinaryOp::IsNot
            ) && (is_row_value_expr(left) || is_row_value_expr(right))
            {
                return eval_row_value_comparison(
                    ctx, left, *op, right, params, scopes, ctes, agg_group,
                )
                .map(RuntimeValue::plain);
            }

            let lv = eval_expr(ctx, left, params, scopes, ctes, agg_group)?;
            let rv = eval_expr(ctx, right, params, scopes, ctes, agg_group)?;

            let val = match op {
                BinaryOp::Concat => {
                    if lv.is_null() || rv.is_null() {
                        Value::Null
                    } else {
                        let mut s = lv.to_text_lossy();
                        s.push_str(&rv.to_text_lossy());
                        Value::Text(s)
                    }
                }
                BinaryOp::Add
                | BinaryOp::Subtract
                | BinaryOp::Multiply
                | BinaryOp::Divide
                | BinaryOp::Remainder => eval_binary_arith(*op, &lv, &rv),
                BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
                    if lv.is_null() || rv.is_null() {
                        Value::Null
                    } else {
                        let a = lv.to_integer_lossy();
                        let b = rv.to_integer_lossy();
                        let res = match op {
                            BinaryOp::BitAnd => a & b,
                            BinaryOp::BitOr => a | b,
                            BinaryOp::ShiftLeft => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 {
                                        if a < 0 { -1 } else { 0 }
                                    } else {
                                        a >> (nb as u32)
                                    }
                                } else if b >= 64 {
                                    0
                                } else {
                                    a.wrapping_shl(b as u32)
                                }
                            }
                            BinaryOp::ShiftRight => {
                                if b < 0 {
                                    let nb = b.saturating_neg();
                                    if nb >= 64 {
                                        0
                                    } else {
                                        a.wrapping_shl(nb as u32)
                                    }
                                } else if b >= 64 {
                                    if a < 0 { -1 } else { 0 }
                                } else {
                                    a >> (b as u32)
                                }
                            }
                            _ => unreachable!(),
                        };
                        Value::Integer(res)
                    }
                }
                BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Is
                | BinaryOp::IsNot => {
                    let laff = infer_expr_affinity_in_scopes(left, scopes, ctx.db_set, ctes);
                    let raff = infer_expr_affinity_in_scopes(right, scopes, ctx.db_set, ctes);
                    let comp_aff = sqlite_compare_affinity(laff, raff);
                    let vdbe_op = match op {
                        BinaryOp::Lt => Opcode::Lt,
                        BinaryOp::Le => Opcode::Le,
                        BinaryOp::Gt => Opcode::Gt,
                        BinaryOp::Ge => Opcode::Ge,
                        BinaryOp::Eq => Opcode::Eq,
                        BinaryOp::Ne => Opcode::Ne,
                        BinaryOp::Is => Opcode::Is,
                        BinaryOp::IsNot => Opcode::IsNot,
                        _ => unreachable!(),
                    };
                    eval_scalar_comparison_in_scopes(
                        ctx, vdbe_op, left, &lv, right, &rv, comp_aff, scopes,
                    )?
                }
                BinaryOp::And | BinaryOp::Or => unreachable!(),
            };
            Ok(RuntimeValue::plain(val))
        }
        Expr::IsNull {
            expr: inner,
            negated,
        } => {
            let v = eval_expr(ctx, inner, params, scopes, ctes, agg_group)?;
            let is_n = v.is_null();
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !is_n } else { is_n },
            ))))
        }
        Expr::Between {
            expr: val_e,
            low,
            high,
            negated,
        } => {
            if is_row_value_expr(val_e) || is_row_value_expr(low) || is_row_value_expr(high) {
                let ge = eval_row_value_comparison(
                    ctx,
                    val_e,
                    BinaryOp::Ge,
                    low,
                    params,
                    scopes,
                    ctes,
                    agg_group,
                )?
                .truthiness();
                let le = eval_row_value_comparison(
                    ctx,
                    val_e,
                    BinaryOp::Le,
                    high,
                    params,
                    scopes,
                    ctes,
                    agg_group,
                )?
                .truthiness();
                let res = match (ge, le) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                return Ok(RuntimeValue::plain(match res {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
                }));
            }
            let v = eval_expr(ctx, val_e, params, scopes, ctes, agg_group)?;
            let lo = eval_expr(ctx, low, params, scopes, ctes, agg_group)?;
            let hi = eval_expr(ctx, high, params, scopes, ctes, agg_group)?;
            let v_aff = infer_expr_affinity_in_scopes(val_e, scopes, ctx.db_set, ctes);
            let lo_aff = sqlite_compare_affinity(
                v_aff,
                infer_expr_affinity_in_scopes(low, scopes, ctx.db_set, ctes),
            );
            let hi_aff = sqlite_compare_affinity(
                v_aff,
                infer_expr_affinity_in_scopes(high, scopes, ctx.db_set, ctes),
            );
            let ge = eval_scalar_comparison_in_scopes(
                ctx,
                Opcode::Ge,
                val_e,
                &v,
                low,
                &lo,
                lo_aff,
                scopes,
            )?
            .truthiness();
            let le = eval_scalar_comparison_in_scopes(
                ctx,
                Opcode::Le,
                val_e,
                &v,
                high,
                &hi,
                hi_aff,
                scopes,
            )?
            .truthiness();
            let res = match (ge, le) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            Ok(RuntimeValue::plain(match res {
                None => Value::Null,
                Some(b) => Value::Integer(i64::from(if *negated { !b } else { b })),
            }))
        }
        Expr::InList {
            expr: lhs_e,
            list,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            // Validate RHS element tuple widths upfront before short-circuiting (`rowvalueA-2.0..2.3`)
            for item in list {
                let m = static_row_value_width(item, scopes, ctx.db_set, ctes);
                if m != n {
                    if n >= 2 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
            }
            if list.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.db_set, ctes);
            let (affs, colls) = combine_row_value_meta(&l_meta, &[]);
            let lhs_custom_coll = if n == 1 {
                match lhs_e.as_ref() {
                    Expr::Collate { collation, .. }
                        if Collation::from_name(collation).is_none() =>
                    {
                        Some(collation.as_str())
                    }
                    _ => None,
                }
            } else {
                None
            };
            let enc = ctx.encoding();
            let mut saw_null = false;
            for item in list {
                let iv_vec = if is_row_value_expr(item) {
                    eval_row_value_tuple(ctx, item, params, scopes, ctes, agg_group)?
                } else {
                    vec![eval_expr(ctx, item, params, scopes, ctes, agg_group)?]
                };
                if iv_vec.len() != n {
                    if n >= 2 {
                        let m = iv_vec.len();
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                let eq_res = if let Some(cname) = lhs_custom_coll {
                    let mut svc = ExecutionContext {
                        interrupt_flag: ctx.interrupt_flag,
                        runtime: ctx.runtime,
                        encoding: enc,
                        functions: ctx.functions,
                        collations: ctx.collations,
                        progress: ctx.progress,
                        strict_subtype: ctx.strict_subtype,
                    };
                    eval_comparison_with_services(
                        Opcode::Eq,
                        &lv_vec[0],
                        &iv_vec[0],
                        affs[0],
                        cname,
                        enc,
                        &mut svc,
                    )?
                    .truthiness()
                } else {
                    compare_row_value_eq(&lv_vec, &iv_vec, &affs, &colls, enc)
                };
                match eq_res {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::InSubquery {
            expr: lhs_e,
            subquery,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            let sub_meta = infer_select_stmt_meta(subquery, scopes, ctx.db_set, ctes);
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            if !sub_out.column_names.is_empty() && sub_out.column_names.len() != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected {}",
                        sub_out.column_names.len(),
                        n
                    ),
                ));
            }
            if sub_out.rows.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.db_set, ctes);
            let mut affs = Vec::with_capacity(n);
            let mut colls = Vec::with_capacity(n);
            for i in 0..n {
                let (laff, lcoll, lexp) = l_meta.get(i).copied().unwrap_or((None, None, false));
                let (raff, rcoll) = sub_meta
                    .get(i)
                    .copied()
                    .unwrap_or((None, Collation::Binary));
                affs.push(sqlite_compare_affinity(laff, raff));
                let c = if lexp {
                    lcoll.unwrap_or(Collation::Binary)
                } else if let Some(lc) = lcoll {
                    lc
                } else {
                    rcoll
                };
                colls.push(c);
            }
            let enc = ctx.encoding();
            let mut saw_null = false;
            for r in &sub_out.rows {
                match compare_row_value_eq(&lv_vec, r, &affs, &colls, enc) {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::InTable {
            expr: lhs_e,
            table,
            negated,
        } => {
            let lv_vec = if is_row_value_expr(lhs_e) {
                eval_row_value_tuple(ctx, lhs_e, params, scopes, ctes, agg_group)?
            } else {
                vec![eval_expr(ctx, lhs_e, params, scopes, ctes, agg_group)?]
            };
            let n = lv_vec.len();
            let (rows, rhs_meta) = if table.schema.is_none()
                && let Some(cte_out) = resolve_cte(ctx, ctes, &table.name, params, scopes)?
            {
                let m = cte_out.column_names.len();
                if m != n {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("sub-select returns {m} columns - expected {n}"),
                    ));
                }
                let meta = infer_cte_meta_by_name(&table.name, scopes, ctx.db_set, ctes);
                (cte_out.rows, meta)
            } else if ctx.db_set.resolve_catalog_table(table)?.is_none()
                && let Some((v_db_id, view_ref)) = ctx.db_set.find_view_in_set(table)
            {
                let view_cloned = view_ref.clone();
                let (v_out, v_meta, _) = eval_view_query_output(ctx, v_db_id, &view_cloned, ctes)?;
                let m = v_out.column_names.len();
                if m != n {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("sub-select returns {m} columns - expected {n}"),
                    ));
                }
                (v_out.rows, v_meta)
            } else {
                let (db_id, tbl, _) = resolve_table_for_query_in_set(ctx.db_set, table)?;
                let m = tbl.columns.len();
                if m != n {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("sub-select returns {m} columns - expected {n}"),
                    ));
                }
                ctx.ensure_read_db(db_id, true)?;
                let meta: Vec<(Option<TypeAffinity>, Collation)> = tbl
                    .columns
                    .iter()
                    .map(|c| (Some(c.affinity), c.collation))
                    .collect();
                let raw = if tbl.root_pgno == 0 {
                    Vec::new()
                } else {
                    scan_table_raw_entries(ctx, db_id, &tbl)?
                };
                let mut out_rows = Vec::with_capacity(raw.len());
                for (rkey, payload) in raw {
                    out_rows.push(decode_table_logical_row(
                        ctx, db_id, &tbl, &rkey, &payload, params,
                    )?);
                }
                (out_rows, meta)
            };
            if rows.is_empty() {
                return Ok(RuntimeValue::plain(Value::Integer(i64::from(*negated))));
            }
            let l_meta = infer_row_value_elem_meta(lhs_e, n, scopes, ctx.db_set, ctes);
            let mut affs = Vec::with_capacity(n);
            let mut colls = Vec::with_capacity(n);
            for i in 0..n {
                let (laff, lcoll, lexp) = l_meta.get(i).copied().unwrap_or((None, None, false));
                let (raff, rcoll) = rhs_meta
                    .get(i)
                    .copied()
                    .unwrap_or((None, Collation::Binary));
                affs.push(sqlite_compare_affinity(laff, raff));
                let c = if lexp {
                    lcoll.unwrap_or(Collation::Binary)
                } else if let Some(lc) = lcoll {
                    lc
                } else {
                    rcoll
                };
                colls.push(c);
            }
            let enc = ctx.encoding();
            let mut saw_null = false;
            for r in &rows {
                match compare_row_value_eq(&lv_vec, r, &affs, &colls, enc) {
                    Some(true) => {
                        return Ok(RuntimeValue::plain(Value::Integer(i64::from(!*negated))));
                    }
                    None => saw_null = true,
                    Some(false) => {}
                }
            }
            Ok(RuntimeValue::plain(if saw_null {
                Value::Null
            } else {
                Value::Integer(i64::from(*negated))
            }))
        }
        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            op,
            negated,
        } => {
            let tv = eval_expr(ctx, txt_e, params, scopes, ctes, agg_group)?;
            let pv = eval_expr(ctx, pattern, params, scopes, ctes, agg_group)?;
            if tv.is_null() || pv.is_null() {
                return Ok(RuntimeValue::null());
            }
            let esc = if let Some(ee) = escape {
                let ev = eval_expr(ctx, ee, params, scopes, ctes, agg_group)?;
                if ev.is_null() {
                    return Ok(RuntimeValue::null());
                }
                Some(parse_escape_char(&ev.to_text_lossy())?)
            } else {
                None
            };
            let matched = match op {
                LikeOp::Like => sqlite_like_match(&pv.to_text_lossy(), &tv.to_text_lossy(), esc),
                LikeOp::Glob => sqlite_glob_match(&pv.to_text_lossy(), &tv.to_text_lossy()),
            };
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !matched } else { matched },
            ))))
        }
        Expr::Cast {
            expr: inner,
            type_name,
        } => {
            let rv = eval_expr_rt(ctx, inner, params, scopes, ctes, agg_group)?;
            Ok(eval_cast_rt_with_encoding(rv, type_name, ctx.encoding()))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            let op_val = match operand {
                Some(oe) => Some(eval_expr(ctx, oe, params, scopes, ctes, agg_group)?),
                None => None,
            };
            let op_aff = operand
                .as_ref()
                .and_then(|oe| infer_expr_affinity_in_scopes(oe, scopes, ctx.db_set, ctes));
            for (we, te) in branches {
                let cond_true = if let (Some(base), Some(oe)) = (&op_val, operand.as_ref()) {
                    let wv = eval_expr(ctx, we, params, scopes, ctes, agg_group)?;
                    let w_aff = infer_expr_affinity_in_scopes(we, scopes, ctx.db_set, ctes);
                    let comp_aff = sqlite_compare_affinity(op_aff, w_aff);
                    eval_scalar_comparison_in_scopes(
                        ctx,
                        Opcode::Eq,
                        oe,
                        base,
                        we,
                        &wv,
                        comp_aff,
                        scopes,
                    )?
                    .truthiness()
                        == Some(true)
                } else {
                    eval_expr(ctx, we, params, scopes, ctes, agg_group)?.truthiness() == Some(true)
                };
                if cond_true {
                    return eval_expr_rt(ctx, te, params, scopes, ctes, agg_group);
                }
            }
            if let Some(el) = else_expr {
                eval_expr_rt(ctx, el, params, scopes, ctes, agg_group)
            } else {
                Ok(RuntimeValue::null())
            }
        }
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            // Check if this is an aggregate call (`count(*)` and 0-arg `count()` are both `CountStar`)
            let is_count_zero = name.eq_ignore_ascii_case("COUNT")
                && matches!(args, FunctionArgs::Exprs(es) if es.is_empty())
                && ctx.functions.find_scalar(name, 0).is_none();
            let is_star = matches!(args, FunctionArgs::Star) || is_count_zero;
            let upper = name.to_ascii_uppercase();
            let agg_kind_opt = if is_star {
                AggregateKind::from_name(name, true)
            } else if let FunctionArgs::Exprs(es) = args {
                if ctx.functions.find_scalar(name, es.len()).is_some()
                    || ((upper == "MIN" || upper == "MAX") && es.len() != 1)
                {
                    None
                } else {
                    AggregateKind::from_name(name, false)
                }
            } else {
                None
            };

            if let Some(agg_kind) = agg_kind_opt {
                if is_count_zero && *distinct {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "DISTINCT aggregates must have exactly one argument",
                    ));
                }
                let Some((group_rows, base_outer)) = agg_group else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                };
                let arg_coll = match args {
                    FunctionArgs::Exprs(es) if es.len() == 1 => {
                        let ref_cols = group_rows.first().map_or(&[][..], |r| &r.cols[..]);
                        infer_expr_collation_from_cols(&es[0], ref_cols)
                    }
                    _ => Collation::Binary,
                };
                let mut acc =
                    AggregateAccumulator::new_with_collation(agg_kind, *distinct, arg_coll);
                for gr in group_rows {
                    let row_scopes = push_scope(base_outer, gr);
                    if let Some(f_expr) = filter {
                        let fv = eval_expr(ctx, f_expr, params, &row_scopes, ctes, None)?;
                        if fv.truthiness() != Some(true) {
                            continue;
                        }
                    }
                    if is_star {
                        acc.step(&[])?;
                    } else if let FunctionArgs::Exprs(es) = args {
                        let mut a_vals = Vec::with_capacity(es.len());
                        for ae in es {
                            a_vals.push(eval_expr(ctx, ae, params, &row_scopes, ctes, None)?);
                        }
                        acc.step(&a_vals)?;
                    }
                }
                return acc.finalize().map(RuntimeValue::plain);
            }

            let (star_args, expr_list) = match args {
                FunctionArgs::Star => (true, &[][..]),
                FunctionArgs::Exprs(es) => (false, es.as_slice()),
            };
            {
                let _tf_guard =
                    sqliters_vdbe::TestFunctionsScope::enter(ctx.runtime.enable_test_functions);
                ctx.functions.validate_call(
                    name,
                    star_args,
                    expr_list.len(),
                    *distinct,
                    filter.is_some(),
                    false,
                )?;
            }

            if !star_args
                && !*distinct
                && filter.is_none()
                && ctx.functions.find_scalar(name, expr_list.len()).is_none()
            {
                match upper.as_str() {
                    "COALESCE" | "IFNULL" => {
                        for ae in expr_list {
                            let rv = eval_expr_rt(ctx, ae, params, scopes, ctes, agg_group)?;
                            if !rv.value.is_null() {
                                return Ok(rv);
                            }
                        }
                        return Ok(RuntimeValue::null());
                    }
                    "IIF" | "IF" => {
                        let mut idx = 0;
                        while idx + 1 < expr_list.len() {
                            if eval_expr(ctx, &expr_list[idx], params, scopes, ctes, agg_group)?
                                .truthiness()
                                == Some(true)
                            {
                                return eval_expr_rt(
                                    ctx,
                                    &expr_list[idx + 1],
                                    params,
                                    scopes,
                                    ctes,
                                    agg_group,
                                );
                            }
                            idx += 2;
                        }
                        return if idx < expr_list.len() {
                            eval_expr_rt(ctx, &expr_list[idx], params, scopes, ctes, agg_group)
                        } else {
                            Ok(RuntimeValue::null())
                        };
                    }
                    _ => {}
                }
            }

            let mut a_vals = Vec::with_capacity(expr_list.len());
            for ae in expr_list {
                a_vals.push(eval_expr_rt(ctx, ae, params, scopes, ctes, agg_group)?);
            }
            ctx.call_scalar_rt(name, &a_vals)
        }
        Expr::Subquery(sub) => {
            let sub_out = exec_select(ctx, sub, params, scopes, ctes)?;
            if sub_out.column_names.len() > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected 1",
                        sub_out.column_names.len()
                    ),
                ));
            }
            Ok(RuntimeValue::plain(
                sub_out
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .cloned()
                    .unwrap_or(Value::Null),
            ))
        }
        Expr::Exists { subquery, negated } => {
            let sub_out = exec_select(ctx, subquery, params, scopes, ctes)?;
            let exists = !sub_out.rows.is_empty();
            Ok(RuntimeValue::plain(Value::Integer(i64::from(
                if *negated { !exists } else { exists },
            ))))
        }
        Expr::Raise { .. } => Err(SqliteError::new(
            ErrorCode::Error,
            "RAISE() may only be used within a trigger-program",
        )),
    }
}

fn is_row_value_expr(mut e: &Expr) -> bool {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    matches!(e, Expr::RowValue(_) | Expr::Subquery(_))
}

fn eval_row_value_tuple(
    ctx: &mut ExecContext<'_>,
    expr: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Vec<Value>> {
    match expr {
        Expr::Collate { expr: inner, .. } => {
            eval_row_value_tuple(ctx, inner, params, scopes, ctes, agg_group)
        }
        Expr::RowValue(elems) => {
            let mut out = Vec::with_capacity(elems.len());
            for el in elems {
                out.push(eval_expr(ctx, el, params, scopes, ctes, agg_group)?);
            }
            Ok(out)
        }
        Expr::Subquery(sub) => {
            let sub_out = exec_select(ctx, sub, params, scopes, ctes)?;
            let width = sub_out.column_names.len().max(1);
            if let Some(first_row) = sub_out.rows.into_iter().next() {
                Ok(first_row)
            } else {
                Ok(vec![Value::Null; width])
            }
        }
        other => Ok(vec![eval_expr(
            ctx, other, params, scopes, ctes, agg_group,
        )?]),
    }
}

fn static_row_value_width(
    mut e: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> usize {
    while let Expr::Collate { expr, .. } = e {
        e = expr;
    }
    match e {
        Expr::RowValue(elems) => elems.len(),
        Expr::Subquery(sub) => infer_select_stmt_meta(sub, scopes, db_set, ctes)
            .len()
            .max(1),
        _ => 1,
    }
}

fn infer_row_value_elem_meta(
    mut expr: &Expr,
    n: usize,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Vec<(Option<TypeAffinity>, Option<Collation>, bool)> {
    let mut top_collate: Option<Collation> = None;
    while let Expr::Collate {
        expr: inner,
        collation,
    } = expr
    {
        if top_collate.is_none() {
            top_collate = Some(Collation::from_name(collation).unwrap_or_default());
        }
        expr = inner;
    }
    match expr {
        Expr::RowValue(elems) if elems.len() == n => elems
            .iter()
            .map(|el| {
                let aff = infer_expr_affinity_in_scopes(el, scopes, db_set, ctes);
                let is_exp = matches!(el, Expr::Collate { .. });
                let coll = infer_expr_explicit_or_col_collation(el, &[], scopes);
                (aff, coll, is_exp)
            })
            .collect(),
        Expr::Subquery(sub) => {
            // Note: scalar/row-value `(SELECT ...)` propagates column affinities, NOT collations (`rowvalueA 6.1..6.10`)!
            let sub_meta = infer_select_stmt_meta(sub, scopes, db_set, ctes);
            (0..n)
                .map(|i| {
                    let aff = sub_meta.get(i).and_then(|(a, _)| *a);
                    let (coll, is_exp) = if n == 1 && top_collate.is_some() {
                        (top_collate, true)
                    } else {
                        (None, false)
                    };
                    (aff, coll, is_exp)
                })
                .collect()
        }
        other if n == 1 => {
            let aff = infer_expr_affinity_in_scopes(other, scopes, db_set, ctes);
            if let Some(tc) = top_collate {
                vec![(aff, Some(tc), true)]
            } else {
                let coll = infer_expr_explicit_or_col_collation(other, &[], scopes);
                vec![(aff, coll, false)]
            }
        }
        _ => vec![(None, None, false); n],
    }
}

fn combine_row_value_meta(
    l_meta: &[(Option<TypeAffinity>, Option<Collation>, bool)],
    r_meta: &[(Option<TypeAffinity>, Option<Collation>, bool)],
) -> (Vec<TypeAffinity>, Vec<Collation>) {
    let n = l_meta.len().max(r_meta.len());
    let mut affs = Vec::with_capacity(n);
    let mut colls = Vec::with_capacity(n);
    for i in 0..n {
        let (la, lc, le) = l_meta.get(i).copied().unwrap_or((None, None, false));
        let (ra, rc, re) = r_meta.get(i).copied().unwrap_or((None, None, false));
        affs.push(sqlite_compare_affinity(la, ra));
        let c = if le {
            lc.unwrap_or(Collation::Binary)
        } else if re {
            rc.unwrap_or(Collation::Binary)
        } else {
            lc.or(rc).unwrap_or(Collation::Binary)
        };
        colls.push(c);
    }
    (affs, colls)
}

fn compare_row_value_eq(
    lhs: &[Value],
    rhs: &[Value],
    affs: &[TypeAffinity],
    colls: &[Collation],
    enc: TextEncoding,
) -> Option<bool> {
    let mut saw_null = false;
    for i in 0..lhs.len() {
        let aff = affs.get(i).copied().unwrap_or(TypeAffinity::Blob);
        let coll = colls.get(i).copied().unwrap_or(Collation::Binary);
        match eval_comparison_with_affinity(Opcode::Eq, &lhs[i], &rhs[i], aff, coll, enc)
            .truthiness()
        {
            Some(false) => return Some(false),
            None => saw_null = true,
            Some(true) => {}
        }
    }
    if saw_null { None } else { Some(true) }
}

#[allow(clippy::too_many_arguments)]
fn eval_row_value_comparison(
    ctx: &mut ExecContext<'_>,
    left: &Expr,
    op: BinaryOp,
    right: &Expr,
    params: &[Value],
    scopes: &[EvalRow],
    ctes: &CteStore,
    agg_group: Option<(&[EvalRow], &[EvalRow])>,
) -> Result<Value> {
    let lv = eval_row_value_tuple(ctx, left, params, scopes, ctes, agg_group)?;
    let rv = eval_row_value_tuple(ctx, right, params, scopes, ctes, agg_group)?;
    if lv.len() != rv.len() {
        let is_left_sub = matches!(left, Expr::Subquery(_));
        let is_right_sub = matches!(right, Expr::Subquery(_));
        if is_left_sub && !is_right_sub {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "sub-select returns {} columns - expected {}",
                    lv.len(),
                    rv.len()
                ),
            ));
        }
        if is_right_sub && !is_left_sub {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "sub-select returns {} columns - expected {}",
                    rv.len(),
                    lv.len()
                ),
            ));
        }
        return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
    }

    let n = lv.len();
    let l_meta = infer_row_value_elem_meta(left, n, scopes, ctx.db_set, ctes);
    let r_meta = infer_row_value_elem_meta(right, n, scopes, ctx.db_set, ctes);
    let (affs, colls) = combine_row_value_meta(&l_meta, &r_meta);
    let enc = ctx.encoding();

    match op {
        BinaryOp::Eq => Ok(match compare_row_value_eq(&lv, &rv, &affs, &colls, enc) {
            None => Value::Null,
            Some(b) => Value::Integer(i64::from(b)),
        }),
        BinaryOp::Ne => Ok(match compare_row_value_eq(&lv, &rv, &affs, &colls, enc) {
            None => Value::Null,
            Some(b) => Value::Integer(i64::from(!b)),
        }),
        BinaryOp::Is => {
            let all_eq = (0..n).all(|i| {
                eval_comparison_with_affinity(Opcode::Is, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    == Some(true)
            });
            Ok(Value::Integer(i64::from(all_eq)))
        }
        BinaryOp::IsNot => {
            let any_diff = (0..n).any(|i| {
                eval_comparison_with_affinity(Opcode::Is, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    != Some(true)
            });
            Ok(Value::Integer(i64::from(any_diff)))
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let strict_op = match op {
                BinaryOp::Lt | BinaryOp::Le => Opcode::Lt,
                BinaryOp::Gt | BinaryOp::Ge => Opcode::Gt,
                _ => unreachable!(),
            };
            let inclusive = matches!(op, BinaryOp::Le | BinaryOp::Ge);
            for i in 0..n {
                if lv[i].is_null() || rv[i].is_null() {
                    return Ok(Value::Null);
                }
                if eval_comparison_with_affinity(Opcode::Eq, &lv[i], &rv[i], affs[i], colls[i], enc)
                    .truthiness()
                    == Some(true)
                {
                    if i + 1 == n {
                        return Ok(Value::Integer(i64::from(inclusive)));
                    }
                    continue;
                }
                return Ok(eval_comparison_with_affinity(
                    strict_op, &lv[i], &rv[i], affs[i], colls[i], enc,
                ));
            }
            Ok(Value::Integer(i64::from(inclusive)))
        }
        _ => unreachable!(),
    }
}

fn resolve_custom_binary_collation_name<'a>(left: &'a Expr, right: &'a Expr) -> Option<&'a str> {
    if let Expr::Collate { collation, .. } = left {
        return if Collation::from_name(collation).is_none() {
            Some(collation.as_str())
        } else {
            None
        };
    }
    if let Expr::Collate { collation, .. } = right {
        return if Collation::from_name(collation).is_none() {
            Some(collation.as_str())
        } else {
            None
        };
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn eval_scalar_comparison_in_scopes(
    ctx: &mut ExecContext<'_>,
    vdbe_op: Opcode,
    left_expr: &Expr,
    left_val: &Value,
    right_expr: &Expr,
    right_val: &Value,
    comp_aff: TypeAffinity,
    scopes: &[EvalRow],
) -> Result<Value> {
    let enc = ctx.encoding();
    if let Some(cname) = resolve_custom_binary_collation_name(left_expr, right_expr) {
        let mut svc = ExecutionContext {
            interrupt_flag: ctx.interrupt_flag,
            runtime: ctx.runtime,
            encoding: enc,
            functions: ctx.functions,
            collations: ctx.collations,
            progress: ctx.progress,
            strict_subtype: ctx.strict_subtype,
        };
        eval_comparison_with_services(vdbe_op, left_val, right_val, comp_aff, cname, enc, &mut svc)
    } else {
        let coll = infer_comparison_collation(left_expr, right_expr, scopes);
        Ok(eval_comparison_with_affinity(
            vdbe_op, left_val, right_val, comp_aff, coll, enc,
        ))
    }
}

fn infer_comparison_collation(left: &Expr, right: &Expr, scopes: &[EvalRow]) -> Collation {
    if let Expr::Collate { collation, .. } = left {
        return Collation::from_name(collation).unwrap_or_default();
    }
    if let Expr::Collate { collation, .. } = right {
        return Collation::from_name(collation).unwrap_or_default();
    }
    for e in [left, right] {
        if let Expr::Column(cref) = e {
            for s in scopes.iter().rev() {
                if let Ok(Some((_, c))) = s.lookup_cref(cref) {
                    return c;
                }
            }
        }
    }
    Collation::Binary
}

fn eval_binary_arith(op: BinaryOp, lhs: &Value, rhs: &Value) -> Value {
    if lhs.is_null() || rhs.is_null() {
        return Value::Null;
    }
    let coerce = |v: &Value| match v {
        Value::Integer(_) | Value::Real(_) => v.clone(),
        Value::Text(s) => sqliters_types::parse_numeric_text(s)
            .unwrap_or_else(|| Value::Integer(v.to_integer_lossy())),
        Value::Blob(_) => Value::Integer(v.to_integer_lossy()),
        Value::Null => Value::Null,
    };
    let a = coerce(lhs);
    let b = coerce(rhs);
    match (a, b) {
        (Value::Integer(ia), Value::Integer(ib)) => match op {
            BinaryOp::Add => ia
                .checked_add(ib)
                .map_or_else(|| Value::real((ia as f64) + (ib as f64)), Value::Integer),
            BinaryOp::Subtract => ia
                .checked_sub(ib)
                .map_or_else(|| Value::real((ia as f64) - (ib as f64)), Value::Integer),
            BinaryOp::Multiply => ia
                .checked_mul(ib)
                .map_or_else(|| Value::real((ia as f64) * (ib as f64)), Value::Integer),
            BinaryOp::Divide => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::real(-(ia as f64))
                } else {
                    Value::Integer(ia / ib)
                }
            }
            BinaryOp::Remainder => {
                if ib == 0 {
                    Value::Null
                } else if ia == i64::MIN && ib == -1 {
                    Value::Integer(0)
                } else {
                    Value::Integer(ia % ib)
                }
            }
            _ => unreachable!(),
        },
        (va, vb) => {
            let fa = va.to_real_lossy();
            let fb = vb.to_real_lossy();
            match op {
                BinaryOp::Add => Value::real(fa + fb),
                BinaryOp::Subtract => Value::real(fa - fb),
                BinaryOp::Multiply => Value::real(fa * fb),
                BinaryOp::Divide => {
                    if fb == 0.0 {
                        Value::Null
                    } else {
                        Value::real(fa / fb)
                    }
                }
                BinaryOp::Remainder => {
                    let ia = va.to_integer_lossy();
                    let ib = vb.to_integer_lossy();
                    if ib == 0 {
                        Value::Null
                    } else {
                        Value::Integer(ia.wrapping_rem(ib))
                    }
                }
                _ => unreachable!(),
            }
        }
    }
}

// ============================================================================
// Table Record <-> Logical Row Helpers & Constraints / Indexes
// ============================================================================

fn scan_table_raw_entries(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
) -> Result<Vec<(TableRowKey, Vec<u8>)>> {
    if tbl.root_pgno == 0 {
        return Ok(Vec::new());
    }
    if !tbl.without_rowid {
        let rows = ctx.scan_table_in_db(db_id, tbl.root_pgno)?;
        return Ok(rows
            .into_iter()
            .map(|(rid, p)| (TableRowKey::Rowid(rid), p))
            .collect());
    }
    let enc = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .map(Btree::encoding)
        .unwrap_or_else(|| ctx.encoding());
    let entries = ctx.scan_index_in_db(db_id, tbl.root_pgno, tbl.without_rowid_pk_orders())?;
    let n_pk = tbl.without_rowid_pk_cols.len();
    let mut out = Vec::with_capacity(entries.len());
    for rec in entries {
        let decoded = decode_record_with_encoding(&rec, enc)?;
        let pk = decoded[..n_pk.min(decoded.len())].to_vec();
        out.push((TableRowKey::WithoutRowidPk(pk), rec));
    }
    Ok(out)
}

fn seek_table_raw_entry(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    rkey: &TableRowKey,
) -> Result<Option<Vec<u8>>> {
    if tbl.root_pgno == 0 {
        return Ok(None);
    }
    ctx.ensure_read_db(db_id, false)?;
    let Some(slot) = ctx.db_set.slot_mut(db_id) else {
        return Ok(None);
    };
    let Some(ref mut bt) = slot.btree else {
        return Ok(None);
    };
    if bt.pager().page_count() == 0 {
        return Ok(None);
    }
    match rkey {
        TableRowKey::Rowid(rid) => bt.seek_table_rowid(tbl.root_pgno, *rid),
        TableRowKey::WithoutRowidPk(pk) => {
            let pk_orders = tbl.without_rowid_pk_orders();
            Ok(bt
                .seek_index_equal_raw(tbl.root_pgno, pk, &pk_orders)?
                .into_iter()
                .next())
        }
    }
}

fn table_keys_equal(
    tbl: &TableSchema,
    a: &TableRowKey,
    b: &TableRowKey,
    enc: TextEncoding,
) -> bool {
    match (a, b) {
        (TableRowKey::Rowid(r1), TableRowKey::Rowid(r2)) => r1 == r2,
        (TableRowKey::WithoutRowidPk(pk1), TableRowKey::WithoutRowidPk(pk2)) => {
            if pk1.len() != pk2.len() {
                return false;
            }
            pk1.iter()
                .zip(pk2.iter())
                .zip(tbl.without_rowid_pk_cols.iter())
                .all(|((v1, v2), ic)| {
                    v1.cmp_sqlite_with_encoding(v2, ic.sort_order.collation, enc) == Ordering::Equal
                })
        }
        _ => false,
    }
}

fn slot_encoding_and_name(ctx: &ExecContext<'_>, db_id: DatabaseId) -> (TextEncoding, String) {
    if let Some(slot) = ctx.db_set.slot(db_id) {
        let enc = slot
            .btree
            .as_ref()
            .map(Btree::encoding)
            .unwrap_or_else(|| ctx.encoding());
        (enc, slot.name.clone())
    } else {
        (ctx.encoding(), "main".to_owned())
    }
}

fn decode_table_logical_row(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    rkey: &TableRowKey,
    payload: &[u8],
    params: &[Value],
) -> Result<Vec<Value>> {
    let (enc, schema_name) = slot_encoding_and_name(ctx, db_id);
    let phys_vals = decode_record_with_encoding(payload, enc)?;

    if tbl.without_rowid {
        let mut logical_vals = vec![Value::Null; tbl.columns.len()];
        let mut assigned = vec![false; tbl.columns.len()];
        for (pk_slot, ic) in tbl.without_rowid_pk_cols.iter().enumerate() {
            if let Some(ci) = ic.col_idx
                && !assigned[ci]
                && pk_slot < phys_vals.len()
            {
                logical_vals[ci] = phys_vals[pk_slot].clone();
                assigned[ci] = true;
            }
        }
        let n_pk = tbl.without_rowid_pk_cols.len();
        for (extra_slot, &ci) in tbl.without_rowid_extra_cols.iter().enumerate() {
            let phys_pos = n_pk + extra_slot;
            let col = &tbl.columns[ci];
            let v = if phys_pos < phys_vals.len() {
                phys_vals[phys_pos].clone()
            } else {
                let def_val = match &col.default_expr {
                    Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                    None => Value::Null,
                };
                let mut coerced = col.affinity.apply(def_val);
                if tbl.strict
                    && col.type_name.trim().eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                coerced
            };
            logical_vals[ci] = v;
            assigned[ci] = true;
        }
        for ci in 0..tbl.columns.len() {
            if let Some((ref gexpr, false)) = tbl.columns[ci].generated {
                let scope = make_single_table_scope(tbl, &schema_name, None, 0, &logical_vals);
                let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
                let mut coerced = tbl.columns[ci].affinity.apply(gv);
                if tbl.strict
                    && tbl.columns[ci]
                        .type_name
                        .trim()
                        .eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                logical_vals[ci] = coerced;
            }
        }
        return Ok(logical_vals);
    }

    let rowid = rkey.rowid_or_zero();
    let has_virtual = tbl
        .columns
        .iter()
        .any(|c| c.generated.as_ref().is_some_and(|(_, stored)| !*stored));

    if !has_virtual {
        let mut vals = phys_vals;
        while vals.len() < tbl.columns.len() {
            let missing_idx = vals.len();
            let col = &tbl.columns[missing_idx];
            let def_val = match &col.default_expr {
                Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                None => Value::Null,
            };
            let mut coerced = col.affinity.apply(def_val);
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            vals.push(coerced);
        }
        vals.truncate(tbl.columns.len());
        if let Some(ipk) = tbl.rowid_alias_col
            && ipk < vals.len()
        {
            vals[ipk] = Value::Integer(rowid);
        }
        return Ok(vals);
    }

    let mut logical_vals = Vec::with_capacity(tbl.columns.len());
    let mut phys_idx = 0usize;
    for (ci, col) in tbl.columns.iter().enumerate() {
        let is_virt = col.generated.as_ref().is_some_and(|(_, stored)| !*stored);
        if is_virt {
            logical_vals.push(Value::Null);
        } else {
            let v = if tbl.rowid_alias_col == Some(ci) {
                phys_idx += 1;
                Value::Integer(rowid)
            } else if phys_idx < phys_vals.len() {
                let pv = phys_vals[phys_idx].clone();
                phys_idx += 1;
                pv
            } else {
                let def_val = match &col.default_expr {
                    Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                    None => Value::Null,
                };
                let mut coerced = col.affinity.apply(def_val);
                if tbl.strict
                    && col.type_name.trim().eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                coerced
            };
            logical_vals.push(v);
        }
    }

    for ci in 0..tbl.columns.len() {
        if let Some((ref gexpr, false)) = tbl.columns[ci].generated {
            let scope = make_single_table_scope(tbl, &schema_name, None, rowid, &logical_vals);
            let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
            let mut coerced = tbl.columns[ci].affinity.apply(gv);
            if tbl.strict
                && tbl.columns[ci]
                    .type_name
                    .trim()
                    .eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            logical_vals[ci] = coerced;
        }
    }
    Ok(logical_vals)
}

fn encode_table_physical_record(
    tbl: &TableSchema,
    logical_vals: &[Value],
    enc: TextEncoding,
) -> Vec<u8> {
    if tbl.without_rowid {
        let mut slots = Vec::with_capacity(
            tbl.without_rowid_pk_cols.len() + tbl.without_rowid_extra_cols.len(),
        );
        for ic in &tbl.without_rowid_pk_cols {
            let ci = ic.col_idx.expect("WITHOUT ROWID PK col has col_idx");
            slots.push(logical_vals[ci].clone());
        }
        for &ci in &tbl.without_rowid_extra_cols {
            slots.push(logical_vals[ci].clone());
        }
        return encode_record_with_encoding(&slots, enc);
    }

    let mut slots = Vec::with_capacity(logical_vals.len());
    for (i, v) in logical_vals.iter().enumerate() {
        let is_virt = tbl
            .columns
            .get(i)
            .and_then(|c| c.generated.as_ref())
            .is_some_and(|(_, stored)| !*stored);
        if is_virt {
            continue;
        }
        // Per C SQLite `insert.c:1361`: put NULL in the INTEGER PRIMARY KEY slot of the on-disk record!
        if tbl.rowid_alias_col == Some(i) {
            slots.push(Value::Null);
        } else {
            slots.push(v.clone());
        }
    }
    if slots.is_empty() {
        slots.push(Value::Null);
    }
    encode_record_with_encoding(&slots, enc)
}

fn make_single_table_scope(
    tbl: &TableSchema,
    schema_name: &str,
    alias: Option<&str>,
    rowid: i64,
    vals: &[Value],
) -> EvalRow {
    let eff = alias.unwrap_or(&tbl.name).to_owned();
    let sname = schema_name.to_owned();
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            schema_name: Some(sname.clone()),
            table_alias: Some(eff.clone()),
            name: c.name.clone(),
            collation: c.collation,
            affinity: Some(c.affinity),
            hidden_rowid: false,
            omit_from_star: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            schema_name: Some(sname),
            table_alias: Some(eff),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            affinity: Some(TypeAffinity::Integer),
            hidden_rowid: true,
            omit_from_star: false,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow::new(cols.into(), row_vals)
}

/// Builds the single-table evaluation scope for `RETURNING` (`sqlite/src/trigger.c:1062-1064`,
/// `sqlite/src/resolve.c:522-536`), where `zDb == 0` and only unqualified column names or
/// `<table_name>.<col>` (not statement `AS alias` or `schema.table.col`) resolve.
fn make_returning_table_scope(tbl: &TableSchema, rowid: i64, vals: &[Value]) -> EvalRow {
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            schema_name: None,
            table_alias: Some(tbl.name.clone()),
            name: c.name.clone(),
            collation: c.collation,
            affinity: Some(c.affinity),
            hidden_rowid: false,
            omit_from_star: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            schema_name: None,
            table_alias: Some(tbl.name.clone()),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            affinity: Some(TypeAffinity::Integer),
            hidden_rowid: true,
            omit_from_star: false,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow::new(cols.into(), row_vals)
}

/// Builds the `excluded` virtual scope for `UPSERT` `DO UPDATE` (`sqlite/src/upsert.c:267-336`),
/// where `qualified_only` enforces `excluded.<col>` qualification (`zDb == 0`).
fn make_excluded_table_scope(tbl: &TableSchema, rowid: i64, vals: &[Value]) -> EvalRow {
    let mut cols: Vec<ScopedColumn> = tbl
        .columns
        .iter()
        .map(|c| ScopedColumn {
            schema_name: None,
            table_alias: Some("excluded".to_owned()),
            name: c.name.clone(),
            collation: c.collation,
            affinity: Some(c.affinity),
            hidden_rowid: false,
            omit_from_star: false,
            merged_using: false,
        })
        .collect();
    let mut row_vals = vals.to_vec();
    if !tbl.without_rowid {
        cols.push(ScopedColumn {
            schema_name: None,
            table_alias: Some("excluded".to_owned()),
            name: "rowid".to_owned(),
            collation: Collation::Binary,
            affinity: Some(TypeAffinity::Integer),
            hidden_rowid: true,
            omit_from_star: false,
            merged_using: false,
        });
        row_vals.push(Value::Integer(rowid));
    }
    EvalRow {
        cols: cols.into(),
        vals: row_vals,
        qualified_only: true,
    }
}

fn check_strict_column_type(tbl_name: &str, col: &ColumnSchema, val: &Value) -> Result<()> {
    if val.is_null() {
        return Ok(());
    }
    let upper = col.type_name.trim().to_ascii_uppercase();
    let ok = match upper.as_str() {
        "INT" | "INTEGER" => matches!(val, Value::Integer(_)),
        "REAL" => matches!(val, Value::Real(_) | Value::Integer(_)),
        "TEXT" => matches!(val, Value::Text(_)),
        "BLOB" => matches!(val, Value::Blob(_)),
        "ANY" | "" => true,
        _ => true,
    };
    if !ok {
        return Err(SqliteError::constraint(
            extended::SQLITE_CONSTRAINT_DATATYPE,
            format!(
                "cannot store {} value in {} column {}.{}",
                val.storage_class().typeof_str().to_ascii_uppercase(),
                col.type_name,
                tbl_name,
                col.name
            ),
        ));
    }
    Ok(())
}

fn index_row_passes_partial_where(
    ctx: &mut ExecContext<'_>,
    schema_name: &str,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rowid: i64,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<bool> {
    let Some(ref w) = idx.where_clause else {
        return Ok(true);
    };
    let scope = make_single_table_scope(tbl, schema_name, None, rowid, logical_vals);
    Ok(eval_expr(ctx, w, params, &[scope], &CteStore::default(), None)?.truthiness() == Some(true))
}

fn eval_index_column_values(
    ctx: &mut ExecContext<'_>,
    schema_name: &str,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rowid: i64,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<Vec<Value>> {
    let mut vals = Vec::with_capacity(idx.columns.len());
    let needs_scope = idx.columns.iter().any(|ic| ic.col_idx.is_none());
    let scope_arr = if needs_scope {
        vec![make_single_table_scope(
            tbl,
            schema_name,
            None,
            rowid,
            logical_vals,
        )]
    } else {
        Vec::new()
    };
    for ic in &idx.columns {
        if let Some(ci) = ic.col_idx {
            vals.push(logical_vals[ci].clone());
        } else if let Some(ref e) = ic.expr {
            vals.push(eval_expr(
                ctx,
                e,
                params,
                &scope_arr,
                &CteStore::default(),
                None,
            )?);
        } else {
            vals.push(Value::Null);
        }
    }
    Ok(vals)
}

fn eval_index_key_values(
    ctx: &mut ExecContext<'_>,
    schema_name: &str,
    tbl: &TableSchema,
    idx: &IndexSchema,
    rkey: &TableRowKey,
    logical_vals: &[Value],
    params: &[Value],
) -> Result<Vec<Value>> {
    let rowid = rkey.rowid_or_zero();
    let mut keys =
        eval_index_column_values(ctx, schema_name, tbl, idx, rowid, logical_vals, params)?;
    if tbl.without_rowid {
        for tc in &idx.trailing_pk_cols {
            let ci = tc
                .col_idx
                .expect("WITHOUT ROWID trailing PK col has col_idx");
            keys.push(logical_vals[ci].clone());
        }
    } else {
        keys.push(Value::Integer(rowid));
    }
    Ok(keys)
}

fn format_unique_index_failure(tbl: &TableSchema, idx: &IndexSchema) -> String {
    if idx.columns.iter().any(|c| c.col_idx.is_none()) {
        format!("UNIQUE constraint failed: index '{}'", idx.name)
    } else {
        let cols_str: Vec<String> = idx
            .columns
            .iter()
            .map(|c| format!("{}.{}", tbl.name, c.name))
            .collect();
        format!("UNIQUE constraint failed: {}", cols_str.join(", "))
    }
}

/// Deletes `rkey` AND all its secondary index entries from `tbl` in `db_id`.
fn remove_row_and_indexes(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    rkey: &TableRowKey,
    params: &[Value],
) -> Result<bool> {
    if indexes.is_empty() {
        let Some(slot) = ctx.db_set.slot_mut(db_id) else {
            return Ok(false);
        };
        let Some(ref mut bt) = slot.btree else {
            return Ok(false);
        };
        return match rkey {
            TableRowKey::Rowid(rowid) => bt.delete_table(tbl.root_pgno, *rowid),
            TableRowKey::WithoutRowidPk(pk) => {
                let pk_orders = tbl.without_rowid_pk_orders();
                bt.delete_index(tbl.root_pgno, pk, &pk_orders)
            }
        };
    }
    let Some(old_payload) = seek_table_raw_entry(ctx, db_id, tbl, rkey)? else {
        return Ok(false);
    };
    let (_, schema_name) = slot_encoding_and_name(ctx, db_id);
    let old_vals = decode_table_logical_row(ctx, db_id, tbl, rkey, &old_payload, params)?;
    let rowid = rkey.rowid_or_zero();
    for idx in indexes {
        if index_row_passes_partial_where(ctx, &schema_name, tbl, idx, rowid, &old_vals, params)? {
            let keys = eval_index_key_values(ctx, &schema_name, tbl, idx, rkey, &old_vals, params)?;
            let orders = idx.key_sort_orders();
            if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
                bt.delete_index(idx.root_pgno, &keys, &orders)?;
            }
        }
    }
    let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) else {
        return Ok(false);
    };
    match rkey {
        TableRowKey::Rowid(rid) => bt.delete_table(tbl.root_pgno, *rid),
        TableRowKey::WithoutRowidPk(pk) => {
            let pk_orders = tbl.without_rowid_pk_orders();
            bt.delete_index(tbl.root_pgno, pk, &pk_orders)
        }
    }
}

/// Orders all indexes on `tbl` (including `WITHOUT ROWID` PK when applicable) in `pTab->pIndex`
/// linked-list order (`sqlite/src/build.c:3980-4004`): non-`Replace` indexes in reverse creation
/// order, followed by `Replace` indexes in creation order.
fn ordered_table_indexes(tbl: &TableSchema, indexes: &[IndexSchema]) -> Vec<IndexSchema> {
    let parse_auto_num = |name: &str| -> usize {
        name.rsplit('_')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    };
    let mut autos: Vec<IndexSchema> = indexes.iter().filter(|i| i.is_autoindex).cloned().collect();
    if let Some(pk_idx) = tbl.without_rowid_pk_as_index() {
        autos.push(pk_idx);
    }
    autos.sort_by_key(|i| parse_auto_num(&i.name));

    let mut creation_order = autos;
    creation_order.extend(indexes.iter().filter(|i| !i.is_autoindex).cloned());

    let mut out = Vec::with_capacity(creation_order.len());
    for idx in creation_order
        .iter()
        .rev()
        .filter(|i| i.conflict != ConflictAction::Replace)
    {
        out.push(idx.clone());
    }
    for idx in creation_order
        .into_iter()
        .filter(|i| i.conflict == ConflictAction::Replace)
    {
        out.push(idx);
    }
    out
}

fn ordered_unique_indexes(tbl: &TableSchema, indexes: &[IndexSchema]) -> Vec<IndexSchema> {
    ordered_table_indexes(tbl, indexes)
        .into_iter()
        .filter(|i| i.unique)
        .collect()
}

/// Deletes `rkey` through the shared [`MutationPipeline`], recording a [`MutationContext`] when
/// tracing/hooks are active.
#[allow(clippy::too_many_arguments)]
fn delete_row_via_pipeline(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    rkey: &TableRowKey,
    old_vals_opt: Option<&[Value]>,
    origin: MutationOrigin,
    effective_conflict: ConflictAction,
    is_catalog: bool,
    params: &[Value],
) -> Result<bool> {
    let captured_old = if ctx.mutation_pipeline.is_trace_enabled() {
        if let Some(ov) = old_vals_opt {
            Some(ov.to_vec())
        } else if let Some(payload) = seek_table_raw_entry(ctx, db_id, tbl, rkey)? {
            Some(decode_table_logical_row(
                ctx, db_id, tbl, rkey, &payload, params,
            )?)
        } else {
            None
        }
    } else {
        None
    };

    let deleted = remove_row_and_indexes(ctx, db_id, tbl, indexes, rkey, params)?;
    if deleted {
        let emit_update_hook = match origin {
            MutationOrigin::ReplaceConflictDelete => false,
            _ => !tbl.without_rowid && !is_catalog,
        };
        let invalidate_blobs = !tbl.without_rowid;
        ctx.mutation_pipeline.dispatch(MutationContext {
            db_id,
            table_name: tbl.name.clone(),
            origin,
            old_row: captured_old.map(|values| MutationRow {
                rowid: rkey.rowid_or_zero(),
                values,
            }),
            new_row: None,
            excluded_row: None,
            effective_conflict,
            emit_update_hook,
            invalidate_blobs,
        });
    }
    Ok(deleted)
}

#[allow(clippy::too_many_arguments)]
fn recheck_uniqueness_abort(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    ordered_uniques: &[IndexSchema],
    schema_name: &str,
    rowid: i64,
    final_key: &TableRowKey,
    raw_vals: &[Value],
    existing_self_key: Option<&TableRowKey>,
    enc: TextEncoding,
    params: &[Value],
) -> Result<()> {
    if !tbl.without_rowid
        && existing_self_key != Some(final_key)
        && seek_table_raw_entry(ctx, db_id, tbl, final_key)?.is_some()
    {
        let (ext_code, col_label) = match tbl.rowid_alias_col {
            Some(ipk) => (
                extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                tbl.columns[ipk].name.as_str(),
            ),
            None => (extended::SQLITE_CONSTRAINT_ROWID, "rowid"),
        };
        return Err(SqliteError::constraint(
            ext_code,
            format!("UNIQUE constraint failed: {}.{}", tbl.name, col_label),
        )
        .with_conflict_effect(ConflictResolutionEffect::Abort));
    }

    for idx in ordered_uniques {
        if idx.table_without_rowid && idx.origin == "pk" {
            let is_self =
                existing_self_key.is_some_and(|sk| table_keys_equal(tbl, final_key, sk, enc));
            if !is_self && seek_table_raw_entry(ctx, db_id, tbl, final_key)?.is_some() {
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                    format_unique_index_failure(tbl, idx),
                )
                .with_conflict_effect(ConflictResolutionEffect::Abort));
            }
            continue;
        }
        if !index_row_passes_partial_where(ctx, schema_name, tbl, idx, rowid, raw_vals, params)? {
            continue;
        }
        let indexed_vals =
            eval_index_column_values(ctx, schema_name, tbl, idx, rowid, raw_vals, params)?;
        if indexed_vals.iter().any(Value::is_null) {
            continue;
        }
        let orders = idx.key_sort_orders();
        let hits = if let Some(ref mut bt) =
            ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
            && bt.pager().page_count() > 0
        {
            bt.seek_index_equal(idx.root_pgno, &indexed_vals, &orders[..indexed_vals.len()])?
        } else {
            Vec::new()
        };
        for hit in hits {
            let hit_key_opt = if tbl.without_rowid {
                Some(TableRowKey::WithoutRowidPk(
                    idx.extract_without_rowid_pk_from_hit(tbl, &hit),
                ))
            } else {
                hit.last().and_then(Value::as_i64).map(TableRowKey::Rowid)
            };
            if let Some(hit_key) = hit_key_opt {
                let is_self =
                    existing_self_key.is_some_and(|sk| table_keys_equal(tbl, &hit_key, sk, enc));
                let is_same_target = table_keys_equal(tbl, &hit_key, final_key, enc);
                if !is_self && !is_same_target {
                    let ext_code = if idx.origin == "pk" {
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY
                    } else {
                        extended::SQLITE_CONSTRAINT_UNIQUE
                    };
                    return Err(SqliteError::constraint(
                        ext_code,
                        format_unique_index_failure(tbl, idx),
                    )
                    .with_conflict_effect(ConflictResolutionEffect::Abort));
                }
            }
        }
    }
    Ok(())
}

/// Validates constraints (NOT NULL, STRICT, CHECK, Rowid/WITHOUT ROWID PK, UNIQUE indexes), applies conflict resolution
/// (`IGNORE` / `REPLACE` / error with `ConflictResolutionEffect`), and writes the row + secondary index entries!
/// Returns `Ok(Some((final_key, final_logical_vals)))` if written, or `Ok(None)` if skipped via `IGNORE`.
#[allow(clippy::too_many_arguments)]
fn validate_and_write_row(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    indexes: &[IndexSchema],
    candidate_rowid: Option<i64>,
    mut raw_vals: Vec<Value>,
    stmt_conflict: Option<ConflictAction>,
    existing_self_key: Option<&TableRowKey>,
    params: &[Value],
    bump_last_rowid: bool,
    count_as_top_level_change: bool,
    stmt_fk_suspects: &mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
    fk_depth: usize,
    mutation_dispatch: Option<(MutationOrigin, Option<MutationRow>)>,
    is_catalog: bool,
) -> Result<Option<(TableRowKey, Vec<Value>)>> {
    ctx.ensure_write_db(db_id, false)?;
    let (enc, schema_name) = slot_encoding_and_name(ctx, db_id);

    // 1. Determine final rowid (for rowid tables) & apply column affinities
    let rowid = if tbl.without_rowid {
        0
    } else {
        let mut final_rowid = match candidate_rowid {
            Some(r) => Some(r),
            None => match tbl.rowid_alias_col {
                Some(ipk) => {
                    if raw_vals[ipk].is_null() {
                        None
                    } else {
                        let coerced = TypeAffinity::Integer.apply(raw_vals[ipk].clone());
                        let Some(i) = coerced.as_i64() else {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        };
                        Some(i)
                    }
                }
                None => None,
            },
        };
        if final_rowid.is_none() {
            final_rowid = Some(allocate_table_rowid(ctx, db_id, tbl)?);
        }
        final_rowid.unwrap()
    };

    for (i, col) in tbl.columns.iter().enumerate() {
        if col.generated.is_some() {
            continue;
        }
        if tbl.rowid_alias_col == Some(i) {
            raw_vals[i] = Value::Integer(rowid);
        } else {
            raw_vals[i] = col.affinity.apply(raw_vals[i].clone());
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = raw_vals[i]
            {
                raw_vals[i] = Value::real(iv as f64);
            }
        }
    }

    // Evaluate VIRTUAL and STORED generated columns in declaration order
    for i in 0..tbl.columns.len() {
        if let Some((ref gexpr, _)) = tbl.columns[i].generated {
            let scope = make_single_table_scope(tbl, &schema_name, None, rowid, &raw_vals);
            let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
            let mut coerced = tbl.columns[i].affinity.apply(gv);
            if tbl.strict
                && tbl.columns[i].type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            raw_vals[i] = coerced;
        }
    }

    // 2. STRICT type check
    if tbl.strict {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        for (i, col) in tbl.columns.iter().enumerate() {
            if let Err(err) = check_strict_column_type(&tbl.name, col, &raw_vals[i]) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(err.with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    // 3. NOT NULL constraint check
    for (i, col) in tbl.columns.iter().enumerate() {
        if let Some(col_conflict) = col.not_null
            && raw_vals[i].is_null()
        {
            let eff = stmt_conflict.unwrap_or(col_conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace if col.default_expr.is_some() => {
                    let dv = eval_expr(
                        ctx,
                        col.default_expr.as_ref().unwrap(),
                        params,
                        &[],
                        &CteStore::default(),
                        None,
                    )?;
                    if dv.is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                        ));
                    }
                    let mut coerced = col.affinity.apply(dv);
                    if tbl.strict
                        && col.type_name.trim().eq_ignore_ascii_case("REAL")
                        && let Value::Integer(iv) = coerced
                    {
                        coerced = Value::real(iv as f64);
                    }
                    raw_vals[i] = coerced;
                }
                _ => {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    }

    // 4. CHECK constraints (`NULL` or `true` passes in SQL; only `false` fails!)
    if !tbl.checks.is_empty() {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        let scope = make_single_table_scope(tbl, &schema_name, None, rowid, &raw_vals);
        for chk in &tbl.checks {
            let cv = eval_expr(
                ctx,
                &chk.expr,
                params,
                std::slice::from_ref(&scope),
                &CteStore::default(),
                None,
            )?;
            if cv.truthiness() == Some(false) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_CHECK,
                    format!("CHECK constraint failed: {}", chk.label),
                )
                .with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    let final_key = if tbl.without_rowid {
        TableRowKey::WithoutRowidPk(tbl.extract_without_rowid_pk(&raw_vals))
    } else {
        TableRowKey::Rowid(rowid)
    };

    // 5 & 6. Uniqueness checks in `insert.c` (`sqlite3GenerateConstraintChecks`) order:
    // - Non-`Replace` IPK check (when rowid table and IPK conflict != Replace)
    // - `ordered_unique_indexes` (`WITHOUT ROWID` PK + secondary `UNIQUE` indexes: non-Replace in reverse creation order, then Replace in creation order)
    // - `Replace` IPK check (`Do IPK REPLACE` last so non-Replace secondary UNIQUE constraints run before deleting!)
    let ordered_uniques = ordered_unique_indexes(tbl, indexes);
    let mut did_replace_delete = false;
    let ipk_eff = stmt_conflict.unwrap_or(tbl.rowid_pk_conflict);

    let check_rowid_pk =
        |ctx: &mut ExecContext<'_>,
         stmt_fk_suspects: &mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
         did_replace_delete: &mut bool|
         -> Result<bool> {
            if !tbl.without_rowid
                && existing_self_key != Some(&final_key)
                && let Some(conflict_payload) = seek_table_raw_entry(ctx, db_id, tbl, &final_key)?
            {
                let conflict_key = TableRowKey::Rowid(rowid);
                match ipk_eff {
                    ConflictAction::Ignore => return Ok(true),
                    ConflictAction::Replace => {
                        let conflict_vals = decode_table_logical_row(
                            ctx,
                            db_id,
                            tbl,
                            &conflict_key,
                            &conflict_payload,
                            params,
                        )?;
                        if *ctx.foreign_keys {
                            check_parent_delete_restrict(ctx, db_id, tbl, &conflict_vals, params)?;
                        }
                        delete_row_via_pipeline(
                            ctx,
                            db_id,
                            tbl,
                            indexes,
                            &conflict_key,
                            Some(&conflict_vals),
                            MutationOrigin::ReplaceConflictDelete,
                            ConflictAction::Replace,
                            is_catalog,
                            params,
                        )?;
                        *did_replace_delete = true;
                        if *ctx.foreign_keys {
                            apply_parent_delete_fk_actions(
                                ctx,
                                db_id,
                                tbl,
                                &conflict_vals,
                                params,
                                stmt_fk_suspects,
                                fk_depth + 1,
                            )?;
                        }
                    }
                    _ => {
                        let (ext_code, col_label) = match tbl.rowid_alias_col {
                            Some(ipk) => (
                                extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                                tbl.columns[ipk].name.as_str(),
                            ),
                            None => (extended::SQLITE_CONSTRAINT_ROWID, "rowid"),
                        };
                        return Err(SqliteError::constraint(
                            ext_code,
                            format!("UNIQUE constraint failed: {}.{}", tbl.name, col_label),
                        )
                        .with_conflict_effect(conflict_action_to_effect(ipk_eff)));
                    }
                }
            }
            Ok(false)
        };

    if !tbl.without_rowid
        && ipk_eff != ConflictAction::Replace
        && check_rowid_pk(ctx, stmt_fk_suspects, &mut did_replace_delete)?
    {
        return Ok(None);
    }

    for idx in &ordered_uniques {
        if idx.table_without_rowid && idx.origin == "pk" {
            let is_self =
                existing_self_key.is_some_and(|sk| table_keys_equal(tbl, &final_key, sk, enc));
            if !is_self
                && let Some(conflict_payload) = seek_table_raw_entry(ctx, db_id, tbl, &final_key)?
            {
                let decoded_conflict = decode_record_with_encoding(&conflict_payload, enc)?;
                let n_pk = tbl.without_rowid_pk_cols.len();
                let conflict_key = TableRowKey::WithoutRowidPk(
                    decoded_conflict[..n_pk.min(decoded_conflict.len())].to_vec(),
                );
                let eff = stmt_conflict.unwrap_or(tbl.without_rowid_pk_conflict);
                match eff {
                    ConflictAction::Ignore => return Ok(None),
                    ConflictAction::Replace => {
                        let conflict_vals = decode_table_logical_row(
                            ctx,
                            db_id,
                            tbl,
                            &conflict_key,
                            &conflict_payload,
                            params,
                        )?;
                        if *ctx.foreign_keys {
                            check_parent_delete_restrict(ctx, db_id, tbl, &conflict_vals, params)?;
                        }
                        delete_row_via_pipeline(
                            ctx,
                            db_id,
                            tbl,
                            indexes,
                            &conflict_key,
                            Some(&conflict_vals),
                            MutationOrigin::ReplaceConflictDelete,
                            ConflictAction::Replace,
                            is_catalog,
                            params,
                        )?;
                        did_replace_delete = true;
                        if *ctx.foreign_keys {
                            apply_parent_delete_fk_actions(
                                ctx,
                                db_id,
                                tbl,
                                &conflict_vals,
                                params,
                                stmt_fk_suspects,
                                fk_depth + 1,
                            )?;
                        }
                    }
                    _ => {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                            format_unique_index_failure(tbl, idx),
                        )
                        .with_conflict_effect(conflict_action_to_effect(eff)));
                    }
                }
            }
            continue;
        }

        if !index_row_passes_partial_where(ctx, &schema_name, tbl, idx, rowid, &raw_vals, params)? {
            continue;
        }
        let indexed_vals =
            eval_index_column_values(ctx, &schema_name, tbl, idx, rowid, &raw_vals, params)?;
        if indexed_vals.iter().any(Value::is_null) {
            continue;
        }
        let orders = idx.key_sort_orders();
        let hits = if let Some(ref mut bt) =
            ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
            && bt.pager().page_count() > 0
        {
            bt.seek_index_equal(idx.root_pgno, &indexed_vals, &orders[..indexed_vals.len()])?
        } else {
            Vec::new()
        };
        let mut conflicting_keys = Vec::new();
        for hit in hits {
            let hit_key_opt = if tbl.without_rowid {
                Some(TableRowKey::WithoutRowidPk(
                    idx.extract_without_rowid_pk_from_hit(tbl, &hit),
                ))
            } else {
                hit.last().and_then(Value::as_i64).map(TableRowKey::Rowid)
            };
            if let Some(hit_key) = hit_key_opt {
                let is_self =
                    existing_self_key.is_some_and(|sk| table_keys_equal(tbl, &hit_key, sk, enc));
                let is_same_target = table_keys_equal(tbl, &hit_key, &final_key, enc);
                if !is_self && !is_same_target {
                    conflicting_keys.push(hit_key);
                }
            }
        }
        if !conflicting_keys.is_empty() {
            let eff = stmt_conflict.unwrap_or(idx.conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace => {
                    for ckey in conflicting_keys {
                        if let Some(cpayload) = seek_table_raw_entry(ctx, db_id, tbl, &ckey)? {
                            let cvals = decode_table_logical_row(
                                ctx, db_id, tbl, &ckey, &cpayload, params,
                            )?;
                            if *ctx.foreign_keys {
                                check_parent_delete_restrict(ctx, db_id, tbl, &cvals, params)?;
                            }
                            delete_row_via_pipeline(
                                ctx,
                                db_id,
                                tbl,
                                indexes,
                                &ckey,
                                Some(&cvals),
                                MutationOrigin::ReplaceConflictDelete,
                                ConflictAction::Replace,
                                is_catalog,
                                params,
                            )?;
                            did_replace_delete = true;
                            if *ctx.foreign_keys {
                                apply_parent_delete_fk_actions(
                                    ctx,
                                    db_id,
                                    tbl,
                                    &cvals,
                                    params,
                                    stmt_fk_suspects,
                                    fk_depth + 1,
                                )?;
                            }
                        }
                    }
                }
                _ => {
                    let ext_code = if idx.origin == "pk" {
                        extended::SQLITE_CONSTRAINT_PRIMARYKEY
                    } else {
                        extended::SQLITE_CONSTRAINT_UNIQUE
                    };
                    return Err(SqliteError::constraint(
                        ext_code,
                        format_unique_index_failure(tbl, idx),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    }

    if !tbl.without_rowid
        && ipk_eff == ConflictAction::Replace
        && check_rowid_pk(ctx, stmt_fk_suspects, &mut did_replace_delete)?
    {
        return Ok(None);
    }

    // `addrRecheck` (`insert.c:2707-2725`): when `OR REPLACE` ran and FK actions may have mutated `tbl`,
    // re-verify all uniqueness constraints under `ConflictAction::Abort`!
    if did_replace_delete
        && *ctx.foreign_keys
        && ctx
            .db_set
            .slot(db_id)
            .is_some_and(|s| !find_referencing_fks(&s.schema, &tbl.name).is_empty())
    {
        recheck_uniqueness_abort(
            ctx,
            db_id,
            tbl,
            &ordered_uniques,
            &schema_name,
            rowid,
            &final_key,
            &raw_vals,
            existing_self_key,
            enc,
            params,
        )?;
    }

    // 7. If updating an existing row in-place or moving keys, check parent UPDATE RESTRICT before mutating
    let old_logical_vals_opt = if let Some(old_key) = existing_self_key {
        let old_vals = if let Some(op) = seek_table_raw_entry(ctx, db_id, tbl, old_key)? {
            Some(decode_table_logical_row(
                ctx, db_id, tbl, old_key, &op, params,
            )?)
        } else {
            None
        };
        if *ctx.foreign_keys
            && let Some(ref ov) = old_vals
        {
            check_parent_update_restrict(ctx, db_id, tbl, ov, &raw_vals, params)?;
        }
        remove_row_and_indexes(ctx, db_id, tbl, indexes, old_key, params)?;
        old_vals
    } else {
        None
    };

    // 8. Write table primary B-Tree record & secondary index records
    let phys_rec = encode_table_physical_record(tbl, &raw_vals, enc);
    if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
        if tbl.without_rowid {
            let pk_orders = tbl.without_rowid_pk_orders();
            bt.insert_index(tbl.root_pgno, &phys_rec, &pk_orders)?;
        } else {
            bt.insert_table(tbl.root_pgno, rowid, &phys_rec)?;
        }
    }

    for idx in indexes {
        if index_row_passes_partial_where(ctx, &schema_name, tbl, idx, rowid, &raw_vals, params)? {
            let keys =
                eval_index_key_values(ctx, &schema_name, tbl, idx, &final_key, &raw_vals, params)?;
            let idx_rec = encode_record_with_encoding(&keys, enc);
            let orders = idx.key_sort_orders();
            if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
                bt.insert_index(idx.root_pgno, &idx_rec, &orders)?;
            }
        }
    }

    if tbl.autoincrement {
        update_sqlite_sequence(ctx, db_id, &tbl.name, rowid)?;
    }

    if let Some(slot) = ctx.db_set.slot(db_id)
        && let Some(ref bt) = slot.btree
        && bt.pager().page_count() > slot.max_page_count
    {
        return Err(SqliteError::new(
            ErrorCode::Full,
            "database or disk is full",
        ));
    }

    // 9. Foreign key checks on child row, dispatch parent mutation, & cascading parent UPDATE actions
    if *ctx.foreign_keys {
        register_child_fk_checks(ctx, db_id, tbl, &final_key, &raw_vals, stmt_fk_suspects)?;
    }

    if let Some((origin, excluded_row)) = mutation_dispatch {
        let effective_conflict = stmt_conflict.unwrap_or(if tbl.without_rowid {
            tbl.without_rowid_pk_conflict
        } else {
            tbl.rowid_pk_conflict
        });
        let old_row = existing_self_key
            .zip(old_logical_vals_opt.as_ref())
            .map(|(ok, ov)| MutationRow {
                rowid: ok.rowid_or_zero(),
                values: ov.clone(),
            });
        let emit_update_hook = !tbl.without_rowid && !is_catalog;
        let invalidate_blobs =
            !matches!(origin, MutationOrigin::TopLevelInsert) && !tbl.without_rowid;
        ctx.mutation_pipeline.dispatch(MutationContext {
            db_id,
            table_name: tbl.name.clone(),
            origin,
            old_row,
            new_row: Some(MutationRow {
                rowid: final_key.rowid_or_zero(),
                values: raw_vals.clone(),
            }),
            excluded_row,
            effective_conflict,
            emit_update_hook,
            invalidate_blobs,
        });
    }

    if *ctx.foreign_keys
        && let Some(ref old_vals) = old_logical_vals_opt
    {
        apply_parent_update_fk_actions(
            ctx,
            db_id,
            tbl,
            old_vals,
            &raw_vals,
            params,
            stmt_fk_suspects,
            fk_depth + 1,
        )?;
    }

    if bump_last_rowid && !tbl.without_rowid {
        ctx.runtime.last_insert_rowid = rowid;
    }
    if count_as_top_level_change {
        ctx.runtime.last_changes += 1;
    }
    ctx.runtime.total_changes += 1;
    ctx.publish();

    Ok(Some((final_key, raw_vals)))
}

fn allocate_table_rowid(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
) -> Result<i64> {
    if !tbl.autoincrement {
        let bt = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        return bt.next_rowid(tbl.root_pgno);
    }

    let (max_existing, seq_tbl_opt, enc) = {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let seq_tbl = slot.schema.find_user_table("sqlite_sequence").cloned();
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let max_ex = bt.max_table_rowid(tbl.root_pgno)?.unwrap_or(0);
        let enc = bt.encoding();
        (max_ex, seq_tbl, enc)
    };

    let mut prev_seq = 0i64;
    if let Some(seq_tbl) = seq_tbl_opt
        && let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
    {
        let rows = scan_table_incremental(bt, seq_tbl.root_pgno)?;
        for (_, payload) in rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.len() >= 2 && cols[0].as_str() == Some(tbl.name.as_str()) {
                prev_seq = cols[1].to_integer_lossy();
                break;
            }
        }
    }
    let next_id = prev_seq
        .max(max_existing)
        .checked_add(1)
        .ok_or_else(|| SqliteError::new(ErrorCode::Full, "database or disk is full"))?;
    update_sqlite_sequence(ctx, db_id, &tbl.name, next_id)?;
    Ok(next_id)
}

fn update_sqlite_sequence(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl_name: &str,
    rowid: i64,
) -> Result<()> {
    let Some(slot) = ctx.db_set.slot_mut(db_id) else {
        return Ok(());
    };
    let Some(seq_tbl) = slot.schema.find_user_table("sqlite_sequence").cloned() else {
        return Ok(());
    };
    let Some(ref mut bt) = slot.btree else {
        return Ok(());
    };
    let enc = bt.encoding();
    let rows = scan_table_incremental(bt, seq_tbl.root_pgno)?;
    for (seq_rid, payload) in rows {
        let cols = decode_record_with_encoding(&payload, enc)?;
        if cols.len() >= 2 && cols[0].as_str() == Some(tbl_name) {
            let cur = cols[1].to_integer_lossy();
            if rowid > cur {
                let updated = encode_record_with_encoding(
                    &[Value::Text(tbl_name.to_owned()), Value::Integer(rowid)],
                    enc,
                );
                bt.insert_table(seq_tbl.root_pgno, seq_rid, &updated)?;
            }
            return Ok(());
        }
    }
    if rowid > 0 {
        let new_rid = bt.next_rowid(seq_tbl.root_pgno)?;
        let rec = encode_record_with_encoding(
            &[Value::Text(tbl_name.to_owned()), Value::Integer(rowid)],
            enc,
        );
        bt.insert_table(seq_tbl.root_pgno, new_rid, &rec)?;
    }
    Ok(())
}

// ============================================================================
// Foreign Key Enforcement & Referential Actions (`sqlite/src/fkey.c`)
// ============================================================================

const MAX_FK_CASCADE_DEPTH: usize = 1000;

fn fk_constraint_err() -> SqliteError {
    SqliteError::constraint(
        extended::SQLITE_CONSTRAINT_FOREIGNKEY,
        "FOREIGN KEY constraint failed",
    )
}

/// Resolves the target parent table and parent column indices for `fk` on `child_tbl_name`
/// strictly within the same database slot (`schema` / `schema_name`), verifying SQLite's parent key
/// existence and `PRIMARY KEY` / `UNIQUE` index requirement (`sqlite3FkLocateIndex`).
fn resolve_fk_parent_cols(
    schema: &Schema,
    schema_name: &str,
    child_tbl_name: &str,
    fk: &ForeignKeySchema,
) -> Result<(TableSchema, Vec<usize>)> {
    let Some(ptbl) = schema.find_user_table(&fk.ref_table).cloned() else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {schema_name}.{}", fk.ref_table),
        ));
    };

    let mismatch_err = || {
        SqliteError::new(
            ErrorCode::Error,
            format!(
                "foreign key mismatch - \"{}\" referencing \"{}\"",
                child_tbl_name, ptbl.name
            ),
        )
    };

    let parent_col_idxs: Vec<usize> = match &fk.ref_cols {
        None => {
            let mut pk_cols: Vec<(usize, usize)> = ptbl
                .columns
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_primary_key)
                .map(|(idx, c)| (c.pk_order, idx))
                .collect();
            pk_cols.sort_by_key(|&(ord, _)| ord);
            if pk_cols.is_empty() || pk_cols.len() != fk.from_cols.len() {
                return Err(mismatch_err());
            }
            pk_cols.into_iter().map(|(_, idx)| idx).collect()
        }
        Some(names) => {
            if names.len() != fk.from_cols.len() {
                return Err(mismatch_err());
            }
            let mut idxs = Vec::with_capacity(names.len());
            for n in names {
                let Some((ci, _)) = ptbl.find_column(n) else {
                    return Err(mismatch_err());
                };
                idxs.push(ci);
            }
            // Verify `idxs` is covered by rowid PK or a non-partial UNIQUE/PK index on `ptbl`
            let is_rowid_pk = idxs.len() == 1 && ptbl.rowid_alias_col == Some(idxs[0]);
            let has_matching_unique_idx = if is_rowid_pk {
                true
            } else {
                let mut cand_idxs = schema.indexes_for_table(&ptbl.name);
                if let Some(pk_idx) = ptbl.without_rowid_pk_as_index() {
                    cand_idxs.push(pk_idx);
                }
                cand_idxs.iter().any(|idx| {
                    idx.unique
                        && idx.where_clause.is_none()
                        && idx.columns.len() == idxs.len()
                        && idxs.iter().all(|&c_idx| {
                            idx.columns.iter().any(|ic| {
                                ic.col_idx == Some(c_idx)
                                    && ic.sort_order.collation == ptbl.columns[c_idx].collation
                            })
                        })
                })
            };
            if !has_matching_unique_idx {
                return Err(mismatch_err());
            }
            idxs
        }
    };

    Ok((ptbl, parent_col_idxs))
}

/// Checks whether a non-NULL child key tuple `child_vals[fk.from_cols]` has a matching parent row in `ptbl` within `db_id`.
fn parent_row_exists_for_fk(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    child_vals: &[Value],
    from_cols: &[usize],
) -> Result<bool> {
    if from_cols.iter().any(|&ci| child_vals[ci].is_null()) {
        return Ok(true);
    }

    // Fast path: single-column `INTEGER PRIMARY KEY` (`rowid_alias_col`)
    if parent_col_idxs.len() == 1 && ptbl.rowid_alias_col == Some(parent_col_idxs[0]) {
        let coerced = TypeAffinity::Integer.apply(child_vals[from_cols[0]].clone());
        let Value::Integer(target_rid) = coerced else {
            return Ok(false);
        };
        return Ok(
            seek_table_raw_entry(ctx, db_id, ptbl, &TableRowKey::Rowid(target_rid))?.is_some(),
        );
    }

    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    let coerced_keys: Vec<(Value, Collation)> = parent_col_idxs
        .iter()
        .zip(from_cols.iter())
        .map(|(&p_idx, &c_idx)| {
            let pcol = &ptbl.columns[p_idx];
            (
                pcol.affinity.apply(child_vals[c_idx].clone()),
                pcol.collation,
            )
        })
        .collect();

    let raw_rows = scan_table_raw_entries(ctx, db_id, ptbl)?;
    for (pkey, ppayload) in raw_rows {
        let pvals = decode_table_logical_row(ctx, db_id, ptbl, &pkey, &ppayload, &[])?;
        let matches =
            parent_col_idxs
                .iter()
                .zip(coerced_keys.iter())
                .all(|(&p_idx, (ck, coll))| {
                    !pvals[p_idx].is_null()
                        && eval_comparison_with_encoding(Opcode::Eq, &pvals[p_idx], ck, *coll, enc)
                            .truthiness()
                            == Some(true)
                });
        if matches {
            return Ok(true);
        }
    }
    Ok(false)
}

fn child_row_matches_parent_key(
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    parent_vals: &[Value],
    child_vals: &[Value],
    from_cols: &[usize],
    enc: TextEncoding,
) -> bool {
    if from_cols.iter().any(|&ci| child_vals[ci].is_null())
        || parent_col_idxs.iter().any(|&pi| parent_vals[pi].is_null())
    {
        return false;
    }
    parent_col_idxs
        .iter()
        .zip(from_cols.iter())
        .all(|(&p_idx, &c_idx)| {
            let pcol = &ptbl.columns[p_idx];
            let coerced_child = pcol.affinity.apply(child_vals[c_idx].clone());
            eval_comparison_with_encoding(
                Opcode::Eq,
                &parent_vals[p_idx],
                &coerced_child,
                pcol.collation,
                enc,
            )
            .truthiness()
                == Some(true)
        })
}

fn register_child_fk_checks(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    rkey: &TableRowKey,
    written_vals: &[Value],
    stmt_fk_suspects: &mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
) -> Result<()> {
    let Some((inst_id, schema_name, slot_schema)) = ctx
        .db_set
        .slot(db_id)
        .map(|s| (s.schema_instance_id, s.name.clone(), s.schema.clone()))
    else {
        return Ok(());
    };
    for fk in &tbl.foreign_keys {
        let (ptbl, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &tbl.name, fk)?;
        if fk.from_cols.iter().any(|&ci| written_vals[ci].is_null()) {
            continue;
        }
        if !parent_row_exists_for_fk(
            ctx,
            db_id,
            &ptbl,
            &parent_col_idxs,
            written_vals,
            &fk.from_cols,
        )? {
            if fk.deferred {
                ctx.deferred_fk_suspects
                    .push((inst_id, tbl.name.clone(), rkey.clone(), fk.id));
            } else {
                // Per SQLite `OP_FkCounter` / `sqlite3VdbeCheckFkImmediate`, non-RESTRICT immediate FKs
                // are verified at statement completion so multi-row / multi-cascade updates settle cleanly.
                stmt_fk_suspects.push((inst_id, tbl.name.clone(), rkey.clone(), fk.id));
            }
        }
    }
    Ok(())
}

fn verify_fk_suspects(
    ctx: &mut ExecContext<'_>,
    suspects: &[(SchemaInstanceId, String, TableRowKey, usize)],
) -> Result<()> {
    for (inst_id, ctbl_name, ckey, fk_id) in suspects {
        let Some(db_id) = ctx.db_set.find_db_id_by_instance(*inst_id) else {
            continue;
        };
        let Some((schema_name, slot_schema, ctbl)) = ctx.db_set.slot(db_id).and_then(|s| {
            s.btree.as_ref()?;
            let ct = s.schema.find_user_table(ctbl_name)?.clone();
            Some((s.name.clone(), s.schema.clone(), ct))
        }) else {
            continue;
        };
        let Some(fk) = ctbl.foreign_keys.iter().find(|f| f.id == *fk_id).cloned() else {
            continue;
        };
        let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, ckey)? else {
            continue;
        };
        let cvals = decode_table_logical_row(ctx, db_id, &ctbl, ckey, &cpayload, &[])?;
        let (ptbl, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &ctbl.name, &fk)?;
        if !parent_row_exists_for_fk(ctx, db_id, &ptbl, &parent_col_idxs, &cvals, &fk.from_cols)? {
            return Err(fk_constraint_err());
        }
    }
    Ok(())
}

fn find_referencing_fks(
    schema: &Schema,
    parent_tbl_name: &str,
) -> Vec<(TableSchema, ForeignKeySchema)> {
    let mut out = Vec::new();
    for ctbl in &schema.tables {
        for fk in &ctbl.foreign_keys {
            if fk.ref_table.eq_ignore_ascii_case(parent_tbl_name) {
                out.push((ctbl.clone(), fk.clone()));
            }
        }
    }
    out
}

fn check_parent_delete_restrict(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    params: &[Value],
) -> Result<()> {
    let Some((schema_name, slot_schema, refs)) = ctx.db_set.slot(db_id).map(|s| {
        (
            s.name.clone(),
            s.schema.clone(),
            find_referencing_fks(&s.schema, &ptbl.name),
        )
    }) else {
        return Ok(());
    };
    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &ctbl.name, &fk)?;
        if fk.on_delete != FkAction::Restrict {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, db_id, &ctbl)?;
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                return Err(fk_constraint_err());
            }
        }
    }
    Ok(())
}

fn parent_key_changed(
    ptbl: &TableSchema,
    parent_col_idxs: &[usize],
    old_vals: &[Value],
    new_vals: &[Value],
    enc: TextEncoding,
) -> bool {
    parent_col_idxs.iter().any(|&pi| {
        eval_comparison_with_encoding(
            Opcode::Is,
            &old_vals[pi],
            &new_vals[pi],
            ptbl.columns[pi].collation,
            enc,
        )
        .truthiness()
            != Some(true)
    })
}

fn check_parent_update_restrict(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    new_parent_vals: &[Value],
    params: &[Value],
) -> Result<()> {
    let Some((schema_name, slot_schema, refs)) = ctx.db_set.slot(db_id).map(|s| {
        (
            s.name.clone(),
            s.schema.clone(),
            find_referencing_fks(&s.schema, &ptbl.name),
        )
    }) else {
        return Ok(());
    };
    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &ctbl.name, &fk)?;
        if !parent_key_changed(
            ptbl,
            &parent_col_idxs,
            old_parent_vals,
            new_parent_vals,
            enc,
        ) {
            continue;
        }
        if fk.on_update != FkAction::Restrict {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, db_id, &ctbl)?;
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                return Err(fk_constraint_err());
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_parent_delete_fk_actions(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    params: &[Value],
    stmt_fk_suspects: &mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
    fk_depth: usize,
) -> Result<()> {
    if fk_depth > MAX_FK_CASCADE_DEPTH {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "too many levels of trigger recursion",
        ));
    }
    let Some((inst_id, schema_name, slot_schema, refs)) = ctx.db_set.slot(db_id).map(|s| {
        (
            s.schema_instance_id,
            s.name.clone(),
            s.schema.clone(),
            find_referencing_fks(&s.schema, &ptbl.name),
        )
    }) else {
        return Ok(());
    };
    let (enc, _) = slot_encoding_and_name(ctx, db_id);

    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &ctbl.name, &fk)?;
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, db_id, &ctbl)?;
        let mut matching_children = Vec::new();
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                matching_children.push((ckey, cvals));
            }
        }
        if matching_children.is_empty() {
            continue;
        }

        match fk.on_delete {
            FkAction::Restrict => return Err(fk_constraint_err()),
            FkAction::NoAction => {
                for (ckey, _) in matching_children {
                    if fk.deferred {
                        ctx.deferred_fk_suspects
                            .push((inst_id, ctbl.name.clone(), ckey, fk.id));
                    } else {
                        stmt_fk_suspects.push((inst_id, ctbl.name.clone(), ckey, fk.id));
                    }
                }
            }
            FkAction::Cascade => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    // Re-check child existence in case an earlier cascade already deleted it
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let cur_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    check_parent_delete_restrict(ctx, db_id, &ctbl, &cur_cvals, params)?;
                    if delete_row_via_pipeline(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        &ckey,
                        Some(&cur_cvals),
                        MutationOrigin::FkCascadeDelete,
                        ConflictAction::Abort,
                        false,
                        params,
                    )? {
                        ctx.runtime.total_changes += 1;
                        apply_parent_delete_fk_actions(
                            ctx,
                            db_id,
                            &ctbl,
                            &cur_cvals,
                            params,
                            stmt_fk_suspects,
                            fk_depth + 1,
                        )?;
                    }
                }
            }
            FkAction::SetNull => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    let new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        new_cvals[fcol] = Value::Null;
                        if ctbl.rowid_alias_col == Some(fcol) {
                            // INTEGER PRIMARY KEY column cannot hold NULL
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                    }
                    validate_and_write_row(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                        Some((MutationOrigin::FkActionUpdate, None)),
                        false,
                    )?;
                }
            }
            FkAction::SetDefault => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for (ckey, _) in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        let dval = match &ctbl.columns[fcol].default_expr {
                            Some(de) => {
                                eval_expr(ctx, de, params, &[], &CteStore::default(), None)?
                            }
                            None => Value::Null,
                        };
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if dval.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = dval;
                    }
                    validate_and_write_row(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                        Some((MutationOrigin::FkActionUpdate, None)),
                        false,
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_parent_update_fk_actions(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    ptbl: &TableSchema,
    old_parent_vals: &[Value],
    new_parent_vals: &[Value],
    params: &[Value],
    stmt_fk_suspects: &mut Vec<(SchemaInstanceId, String, TableRowKey, usize)>,
    fk_depth: usize,
) -> Result<()> {
    if fk_depth > MAX_FK_CASCADE_DEPTH {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "too many levels of trigger recursion",
        ));
    }
    let Some((inst_id, schema_name, slot_schema, refs)) = ctx.db_set.slot(db_id).map(|s| {
        (
            s.schema_instance_id,
            s.name.clone(),
            s.schema.clone(),
            find_referencing_fks(&s.schema, &ptbl.name),
        )
    }) else {
        return Ok(());
    };
    let (enc, _) = slot_encoding_and_name(ctx, db_id);

    for (ctbl, fk) in refs {
        let (_, parent_col_idxs) =
            resolve_fk_parent_cols(&slot_schema, &schema_name, &ctbl.name, &fk)?;
        if !parent_key_changed(
            ptbl,
            &parent_col_idxs,
            old_parent_vals,
            new_parent_vals,
            enc,
        ) {
            continue;
        }
        if parent_col_idxs
            .iter()
            .any(|&pi| old_parent_vals[pi].is_null())
        {
            continue;
        }
        let crows = scan_table_raw_entries(ctx, db_id, &ctbl)?;
        let mut matching_children = Vec::new();
        for (ckey, cpayload) in crows {
            let cvals = decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
            if child_row_matches_parent_key(
                ptbl,
                &parent_col_idxs,
                old_parent_vals,
                &cvals,
                &fk.from_cols,
                enc,
            ) {
                matching_children.push(ckey);
            }
        }
        if matching_children.is_empty() {
            continue;
        }

        match fk.on_update {
            FkAction::Restrict => return Err(fk_constraint_err()),
            FkAction::NoAction => {
                for ckey in matching_children {
                    if fk.deferred {
                        ctx.deferred_fk_suspects
                            .push((inst_id, ctbl.name.clone(), ckey, fk.id));
                    } else {
                        stmt_fk_suspects.push((inst_id, ctbl.name.clone(), ckey, fk.id));
                    }
                }
            }
            FkAction::Cascade => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for (&fcol, &pcol) in fk.from_cols.iter().zip(parent_col_idxs.iter()) {
                        let nv = new_parent_vals[pcol].clone();
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if nv.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = nv;
                    }
                    validate_and_write_row(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                        Some((MutationOrigin::FkActionUpdate, None)),
                        false,
                    )?;
                }
            }
            FkAction::SetNull => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    let new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        if ctbl.rowid_alias_col == Some(fcol) {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                        new_cvals[fcol] = Value::Null;
                    }
                    validate_and_write_row(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                        Some((MutationOrigin::FkActionUpdate, None)),
                        false,
                    )?;
                }
            }
            FkAction::SetDefault => {
                let cindexes = slot_schema.indexes_for_table(&ctbl.name);
                for ckey in matching_children {
                    let Some(cpayload) = seek_table_raw_entry(ctx, db_id, &ctbl, &ckey)? else {
                        continue;
                    };
                    let mut new_cvals =
                        decode_table_logical_row(ctx, db_id, &ctbl, &ckey, &cpayload, params)?;
                    let mut new_crid_opt = match &ckey {
                        TableRowKey::Rowid(r) => Some(*r),
                        TableRowKey::WithoutRowidPk(_) => None,
                    };
                    for &fcol in &fk.from_cols {
                        let dval = match &ctbl.columns[fcol].default_expr {
                            Some(de) => {
                                eval_expr(ctx, de, params, &[], &CteStore::default(), None)?
                            }
                            None => Value::Null,
                        };
                        if ctbl.rowid_alias_col == Some(fcol) {
                            if dval.is_null() {
                                return Err(SqliteError::new(
                                    ErrorCode::Mismatch,
                                    "datatype mismatch",
                                ));
                            }
                            new_crid_opt = None;
                        }
                        new_cvals[fcol] = dval;
                    }
                    validate_and_write_row(
                        ctx,
                        db_id,
                        &ctbl,
                        &cindexes,
                        new_crid_opt,
                        new_cvals,
                        Some(ConflictAction::Abort),
                        Some(&ckey),
                        params,
                        false,
                        false,
                        stmt_fk_suspects,
                        fk_depth + 1,
                        Some((MutationOrigin::FkActionUpdate, None)),
                        false,
                    )?;
                }
            }
        }
    }
    Ok(())
}

// ============================================================================
// DML Executors: `INSERT`, `UPDATE`, `DELETE`
// ============================================================================

fn maybe_count_changes_output(ctx: &ExecContext<'_>, out: QueryOutput) -> QueryOutput {
    if *ctx.count_changes && out.column_names.is_empty() {
        QueryOutput {
            column_names: vec!["count".to_owned()],
            rows: vec![vec![Value::Integer(ctx.runtime.last_changes as i64)]],
        }
    } else {
        out
    }
}

fn resolve_dml_target_table_in_set(
    db_set: &DatabaseSet,
    qn: &QualifiedName,
    writable_schema: bool,
) -> Result<(DatabaseId, TableSchema, String, bool)> {
    resolve_dml_target_table_in_set_opts(db_set, qn, writable_schema, false)
}

fn resolve_dml_target_table_in_set_opts(
    db_set: &DatabaseSet,
    qn: &QualifiedName,
    writable_schema: bool,
    is_upsert: bool,
) -> Result<(DatabaseId, TableSchema, String, bool)> {
    if let Some(cat_db_id) = db_set.resolve_catalog_table(qn)? {
        if !writable_schema {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} may not be modified", qn.name),
            ));
        }
        let mut tbl = TableSchema::sqlite_schema_table();
        tbl.name = qn.name.clone();
        let sname = db_set
            .slot(cat_db_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "main".to_owned());
        return Ok((cat_db_id, tbl, sname, true));
    }

    if let Some((db_id, rel)) = db_set.find_relation_in_set(qn)? {
        match rel {
            ResolvedRelation::Table(tbl) => {
                let sname = db_set
                    .slot(db_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "main".to_owned());
                return Ok((db_id, *tbl, sname, false));
            }
            ResolvedRelation::View(v) => {
                if is_upsert {
                    return Err(SqliteError::new(ErrorCode::Error, "cannot UPSERT a view"));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot modify {} because it is a view", v.name),
                ));
            }
        }
    }

    Err(SqliteError::new(
        ErrorCode::Error,
        format!("no such table: {}", qn.display_name()),
    ))
}

fn resolve_create_table_target(
    db_set: &DatabaseSet,
    temp: bool,
    qn: &QualifiedName,
) -> Result<DatabaseId> {
    if let Some(ref s) = qn.schema {
        let db_id = db_set.resolve_schema_or_err(Some(s))?;
        if temp && db_id != DatabaseId::TEMP {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "temporary table name must be unqualified",
            ));
        }
        return Ok(db_id);
    }
    if temp {
        Ok(DatabaseId::TEMP)
    } else {
        Ok(DatabaseId::MAIN)
    }
}

fn prepare_dml_write_slot(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    qn: &QualifiedName,
    is_catalog: bool,
    fallback_tbl: TableSchema,
) -> Result<(TableSchema, Vec<IndexSchema>, String)> {
    if db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }
    ctx.ensure_write_db(db_id, !is_catalog)?;
    if is_catalog {
        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
            && bt.pager().page_count() == 0
        {
            let _ = bt.create_table()?;
        }
        *ctx.schema_changed_in_tx = true;
        let (_, sname) = slot_encoding_and_name(ctx, db_id);
        return Ok((fallback_tbl, Vec::new(), sname));
    }
    let slot = ctx
        .db_set
        .slot(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let sname = slot.name.clone();
    let tbl = slot
        .schema
        .find_user_table(&fallback_tbl.name)
        .cloned()
        .ok_or_else(|| {
            SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", qn.display_name()),
            )
        })?;
    let indexes = slot.schema.indexes_for_table(&tbl.name);
    Ok((tbl, indexes, sname))
}

struct OrdTableKey {
    key: TableRowKey,
    pk_orders: Rc<[KeySortOrder]>,
    enc: TextEncoding,
}

impl PartialEq for OrdTableKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OrdTableKey {}

impl PartialOrd for OrdTableKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdTableKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (&self.key, &other.key) {
            (TableRowKey::Rowid(a), TableRowKey::Rowid(b)) => a.cmp(b),
            (TableRowKey::WithoutRowidPk(pa), TableRowKey::WithoutRowidPk(pb)) => {
                compare_value_slices(pa, pb, &self.pk_orders, self.enc)
            }
            (TableRowKey::Rowid(_), TableRowKey::WithoutRowidPk(_)) => Ordering::Less,
            (TableRowKey::WithoutRowidPk(_), TableRowKey::Rowid(_)) => Ordering::Greater,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetColRef {
    Col(usize),
    Rowid,
}

fn resolve_target_col_ref(
    tbl: &TableSchema,
    schema_name: &str,
    target_alias: Option<&str>,
    cref: &ColumnRef,
) -> Option<TargetColRef> {
    if let Some(ref s) = cref.schema
        && !s.eq_ignore_ascii_case(schema_name)
    {
        return None;
    }
    let eff_tbl = target_alias.unwrap_or(&tbl.name);
    if let Some(ref t) = cref.table
        && !t.eq_ignore_ascii_case(eff_tbl)
    {
        return None;
    }
    if let Some((ci, _)) = tbl.find_column(&cref.column) {
        if tbl.rowid_alias_col == Some(ci) {
            Some(TargetColRef::Rowid)
        } else {
            Some(TargetColRef::Col(ci))
        }
    } else if tbl.is_implicit_rowid_name(&cref.column) {
        Some(TargetColRef::Rowid)
    } else {
        None
    }
}

fn indexed_col_to_expr(ic: &IndexedColumn) -> Expr {
    match &ic.expr {
        Some(e) => e.clone(),
        None => Expr::Column(ColumnRef::unqualified(ic.name.clone())),
    }
}

fn unwrap_trivial_row_value(mut expr: &Expr) -> &Expr {
    while let Expr::RowValue(elems) = expr {
        if elems.len() == 1 {
            expr = &elems[0];
        } else {
            break;
        }
    }
    expr
}

fn upsert_expr_equiv(
    tbl: &TableSchema,
    schema_name: &str,
    target_alias: Option<&str>,
    target_expr: &Expr,
    idx_expr: &Expr,
) -> bool {
    let te = unwrap_trivial_row_value(target_expr);
    let ie = unwrap_trivial_row_value(idx_expr);
    match (te, ie) {
        (Expr::Null, Expr::Null) => true,
        (Expr::Bool(a), Expr::Bool(b)) => a == b,
        (Expr::Integer(a), Expr::Integer(b)) => a == b,
        (Expr::Float(a), Expr::Float(b)) => a.to_bits() == b.to_bits(),
        (Expr::String(a), Expr::String(b)) => a == b,
        (Expr::Blob(a), Expr::Blob(b)) => a == b,
        (Expr::CurrentTimeKw(a), Expr::CurrentTimeKw(b)) => a == b,
        (Expr::Bind(a), Expr::Bind(b)) => a == b,
        (Expr::Column(tc), Expr::Column(ic)) => {
            let tr = resolve_target_col_ref(tbl, schema_name, target_alias, tc);
            let ir = resolve_target_col_ref(tbl, schema_name, None, ic);
            tr.is_some() && tr == ir
        }
        (
            Expr::Unary {
                op: oa,
                expr: inner_a,
            },
            Expr::Unary {
                op: ob,
                expr: inner_b,
            },
        ) => oa == ob && upsert_expr_equiv(tbl, schema_name, target_alias, inner_a, inner_b),
        (
            Expr::Collate {
                expr: inner_a,
                collation: ca,
            },
            Expr::Collate {
                expr: inner_b,
                collation: cb,
            },
        ) => {
            ca.eq_ignore_ascii_case(cb)
                && upsert_expr_equiv(tbl, schema_name, target_alias, inner_a, inner_b)
        }
        (
            Expr::Cast {
                expr: inner_a,
                type_name: ta,
            },
            Expr::Cast {
                expr: inner_b,
                type_name: tb,
            },
        ) => {
            ta.trim().eq_ignore_ascii_case(tb.trim())
                && upsert_expr_equiv(tbl, schema_name, target_alias, inner_a, inner_b)
        }
        (
            Expr::IsNull {
                expr: inner_a,
                negated: na,
            },
            Expr::IsNull {
                expr: inner_b,
                negated: nb,
            },
        ) => na == nb && upsert_expr_equiv(tbl, schema_name, target_alias, inner_a, inner_b),
        (
            Expr::Binary {
                left: la,
                op: oa,
                right: ra,
            },
            Expr::Binary {
                left: lb,
                op: ob,
                right: rb,
            },
        ) => {
            oa == ob
                && upsert_expr_equiv(tbl, schema_name, target_alias, la, lb)
                && upsert_expr_equiv(tbl, schema_name, target_alias, ra, rb)
        }
        (
            Expr::Between {
                expr: va,
                low: la,
                high: ha,
                negated: na,
            },
            Expr::Between {
                expr: vb,
                low: lb,
                high: hb,
                negated: nb,
            },
        ) => {
            na == nb
                && upsert_expr_equiv(tbl, schema_name, target_alias, va, vb)
                && upsert_expr_equiv(tbl, schema_name, target_alias, la, lb)
                && upsert_expr_equiv(tbl, schema_name, target_alias, ha, hb)
        }
        (
            Expr::InList {
                expr: va,
                list: la,
                negated: na,
            },
            Expr::InList {
                expr: vb,
                list: lb,
                negated: nb,
            },
        ) => {
            na == nb
                && la.len() == lb.len()
                && upsert_expr_equiv(tbl, schema_name, target_alias, va, vb)
                && la
                    .iter()
                    .zip(lb.iter())
                    .all(|(ea, eb)| upsert_expr_equiv(tbl, schema_name, target_alias, ea, eb))
        }
        (
            Expr::Like {
                expr: va,
                pattern: pa,
                escape: ea,
                op: oa,
                negated: na,
            },
            Expr::Like {
                expr: vb,
                pattern: pb,
                escape: eb,
                op: ob,
                negated: nb,
            },
        ) => {
            oa == ob
                && na == nb
                && upsert_expr_equiv(tbl, schema_name, target_alias, va, vb)
                && upsert_expr_equiv(tbl, schema_name, target_alias, pa, pb)
                && match (ea, eb) {
                    (None, None) => true,
                    (Some(xa), Some(xb)) => {
                        upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb)
                    }
                    _ => false,
                }
        }
        (
            Expr::Case {
                operand: oa,
                branches: ba,
                else_expr: ea,
            },
            Expr::Case {
                operand: ob,
                branches: bb,
                else_expr: eb,
            },
        ) => {
            ba.len() == bb.len()
                && match (oa, ob) {
                    (None, None) => true,
                    (Some(xa), Some(xb)) => {
                        upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb)
                    }
                    _ => false,
                }
                && ba.iter().zip(bb.iter()).all(|((wa, ta), (wb, tb))| {
                    upsert_expr_equiv(tbl, schema_name, target_alias, wa, wb)
                        && upsert_expr_equiv(tbl, schema_name, target_alias, ta, tb)
                })
                && match (ea, eb) {
                    (None, None) => true,
                    (Some(xa), Some(xb)) => {
                        upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb)
                    }
                    _ => false,
                }
        }
        (
            Expr::FunctionCall {
                name: na,
                distinct: da,
                args: aa,
                filter: fa,
            },
            Expr::FunctionCall {
                name: nb,
                distinct: db,
                args: ab,
                filter: fb,
            },
        ) => {
            na.eq_ignore_ascii_case(nb)
                && da == db
                && match (aa, ab) {
                    (FunctionArgs::Star, FunctionArgs::Star) => true,
                    (FunctionArgs::Exprs(ea), FunctionArgs::Exprs(eb)) => {
                        ea.len() == eb.len()
                            && ea.iter().zip(eb.iter()).all(|(xa, xb)| {
                                upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb)
                            })
                    }
                    _ => false,
                }
                && match (fa, fb) {
                    (None, None) => true,
                    (Some(xa), Some(xb)) => {
                        upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb)
                    }
                    _ => false,
                }
        }
        (Expr::RowValue(ea), Expr::RowValue(eb)) => {
            ea.len() == eb.len()
                && ea
                    .iter()
                    .zip(eb.iter())
                    .all(|(xa, xb)| upsert_expr_equiv(tbl, schema_name, target_alias, xa, xb))
        }
        _ => false,
    }
}

fn validate_collations_in_expr(expr: &Expr, collations: &CollationRegistry) -> Result<()> {
    match expr {
        Expr::Collate {
            expr: inner,
            collation,
        } => {
            if !collations.has_collation(collation) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such collation sequence: {collation}"),
                ));
            }
            validate_collations_in_expr(inner, collations)
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Cast { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. } => validate_collations_in_expr(inner, collations),
        Expr::Binary { left, right, .. } => {
            validate_collations_in_expr(left, collations)?;
            validate_collations_in_expr(right, collations)
        }
        Expr::Between {
            expr: val,
            low,
            high,
            ..
        } => {
            validate_collations_in_expr(val, collations)?;
            validate_collations_in_expr(low, collations)?;
            validate_collations_in_expr(high, collations)
        }
        Expr::InList {
            expr: val, list, ..
        } => {
            validate_collations_in_expr(val, collations)?;
            for item in list {
                validate_collations_in_expr(item, collations)?;
            }
            Ok(())
        }
        Expr::InSubquery { expr: val, .. } | Expr::InTable { expr: val, .. } => {
            validate_collations_in_expr(val, collations)
        }
        Expr::Like {
            expr: val,
            pattern,
            escape,
            ..
        } => {
            validate_collations_in_expr(val, collations)?;
            validate_collations_in_expr(pattern, collations)?;
            if let Some(esc) = escape {
                validate_collations_in_expr(esc, collations)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                validate_collations_in_expr(op, collations)?;
            }
            for (w, t) in branches {
                validate_collations_in_expr(w, collations)?;
                validate_collations_in_expr(t, collations)?;
            }
            if let Some(el) = else_expr {
                validate_collations_in_expr(el, collations)?;
            }
            Ok(())
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    validate_collations_in_expr(e, collations)?;
                }
            }
            if let Some(fe) = filter {
                validate_collations_in_expr(fe, collations)?;
            }
            Ok(())
        }
        Expr::RowValue(elems) => {
            for e in elems {
                validate_collations_in_expr(e, collations)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn resolve_set_assignments_for_table<'a>(
    tbl: &TableSchema,
    assigns: &'a [SetAssignment],
) -> Result<Vec<(Vec<usize>, bool, &'a Expr)>> {
    let mut resolved = Vec::with_capacity(assigns.len());
    for sa in assigns {
        let mut col_idxs = Vec::with_capacity(sa.columns.len());
        for cname in &sa.columns {
            if let Some((ci, col)) = tbl.find_column(cname) {
                if col.generated.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot UPDATE generated column \"{}\"", col.name),
                    ));
                }
                col_idxs.push(ci);
            } else if tbl.is_implicit_rowid_name(cname) {
                col_idxs.push(usize::MAX);
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {cname}"),
                ));
            }
        }
        resolved.push((col_idxs, sa.row_value, &sa.expr));
    }
    Ok(resolved)
}

#[allow(clippy::too_many_arguments)]
fn validate_set_assignments_static(
    tbl: &TableSchema,
    assigns: &[SetAssignment],
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    dml_ctes: &CteStore,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    allow_agg: bool,
    visited: &mut Vec<(usize, usize)>,
) -> Result<()> {
    let _ = resolve_set_assignments_for_table(tbl, assigns)?;
    for sa in assigns {
        validate_collations_in_expr(&sa.expr, collations)?;
        if !sa.row_value {
            validate_expr_static(
                &sa.expr, scopes, db_set, dml_ctes, functions, allow_agg, false, visited,
            )?;
        } else {
            let n = sa.columns.len();
            let mut inner = &sa.expr;
            while let Expr::Collate { expr: sub, .. } = inner {
                inner = sub;
            }
            match inner {
                Expr::RowValue(elems) => {
                    for el in elems {
                        validate_expr_static(
                            el, scopes, db_set, dml_ctes, functions, allow_agg, false, visited,
                        )?;
                    }
                    if elems.len() != n {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned {} values", elems.len()),
                        ));
                    }
                }
                Expr::Subquery(sub) => {
                    let sub_cols = validate_select_stmt_static(
                        sub, scopes, db_set, dml_ctes, functions, visited,
                    )?;
                    if !sub_cols.is_empty() && sub_cols.len() != n {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned {} values", sub_cols.len()),
                        ));
                    }
                }
                other => {
                    validate_expr_static(
                        other, scopes, db_set, dml_ctes, functions, allow_agg, false, visited,
                    )?;
                    if n != 1 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned 1 values"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn apply_resolved_set_assignments(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    old_key: &TableRowKey,
    old_vals: &[Value],
    resolved_assigns: &[(Vec<usize>, bool, &Expr)],
    eval_scopes: &[EvalRow],
    dml_ctes: &CteStore,
    params: &[Value],
) -> Result<(Option<i64>, Vec<Value>)> {
    apply_resolved_set_assignments_with_agg(
        ctx,
        tbl,
        old_key,
        old_vals,
        resolved_assigns,
        eval_scopes,
        None,
        dml_ctes,
        params,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_resolved_set_assignments_with_agg(
    ctx: &mut ExecContext<'_>,
    tbl: &TableSchema,
    old_key: &TableRowKey,
    old_vals: &[Value],
    resolved_assigns: &[(Vec<usize>, bool, &Expr)],
    eval_scopes: &[EvalRow],
    agg_group: Option<&[EvalRow]>,
    dml_ctes: &CteStore,
    params: &[Value],
) -> Result<(Option<i64>, Vec<Value>)> {
    let mut new_vals = old_vals.to_vec();
    let mut new_rowid_opt = match old_key {
        TableRowKey::Rowid(r) => Some(*r),
        TableRowKey::WithoutRowidPk(_) => None,
    };

    let mut apply_one = |target_idx: usize, v: Value| -> Result<()> {
        if target_idx == usize::MAX {
            if v.is_null() {
                new_rowid_opt = None;
            } else {
                let c = TypeAffinity::Integer.apply(v);
                let rid = c
                    .as_i64()
                    .ok_or_else(|| SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"))?;
                new_rowid_opt = Some(rid);
            }
        } else {
            new_vals[target_idx] = v;
            if tbl.rowid_alias_col == Some(target_idx) {
                new_rowid_opt = None;
            }
        }
        Ok(())
    };

    let eval_agg: Option<(&[EvalRow], &[EvalRow])> = agg_group.map(|g| (g, &[][..]));
    for (col_idxs, is_row_value, val_expr) in resolved_assigns {
        if !*is_row_value {
            let v = eval_expr(ctx, val_expr, params, eval_scopes, dml_ctes, eval_agg)?;
            apply_one(col_idxs[0], v)?;
        } else {
            let n = col_idxs.len();
            let mut inner = *val_expr;
            while let Expr::Collate { expr: sub, .. } = inner {
                inner = sub;
            }
            let rhs_vals = match inner {
                Expr::RowValue(elems) => {
                    if elems.len() != n {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned {} values", elems.len()),
                        ));
                    }
                    let mut out = Vec::with_capacity(n);
                    for el in elems {
                        out.push(eval_expr(ctx, el, params, eval_scopes, dml_ctes, eval_agg)?);
                    }
                    out
                }
                Expr::Subquery(sub) => {
                    let sub_out = exec_select(ctx, sub, params, eval_scopes, dml_ctes)?;
                    if sub_out.column_names.len() != n {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned {} values", sub_out.column_names.len()),
                        ));
                    }
                    sub_out
                        .rows
                        .into_iter()
                        .next()
                        .unwrap_or_else(|| vec![Value::Null; n])
                }
                other => {
                    if n != 1 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{n} columns assigned 1 values"),
                        ));
                    }
                    vec![eval_expr(
                        ctx,
                        other,
                        params,
                        eval_scopes,
                        dml_ctes,
                        eval_agg,
                    )?]
                }
            };
            for (&target_idx, v) in col_idxs.iter().zip(rhs_vals) {
                apply_one(target_idx, v)?;
            }
        }
    }

    Ok((new_rowid_opt, new_vals))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedUpsertTarget {
    CatchAll,
    RowidPk,
    UniqueIndex(usize),
}

#[derive(Debug, Clone, Copy)]
enum UpsertCheckStep {
    RowidPk {
        clause_idx: Option<usize>,
    },
    UniqueIdx {
        u_idx: usize,
        clause_idx: Option<usize>,
    },
}

fn match_target_col_to_index_col(
    tbl: &TableSchema,
    schema_name: &str,
    target_alias: Option<&str>,
    target_ic: &IndexedColumn,
    idx_ic: &IndexedColSchema,
) -> bool {
    if let Some(ref explicit_coll) = target_ic.collation {
        let Some(c_enum) = Collation::from_name(explicit_coll) else {
            // Custom collation on target: IndexColSchema currently stores built-in Collation
            return false;
        };
        if c_enum != idx_ic.sort_order.collation {
            return false;
        }
    }
    let target_expr = indexed_col_to_expr(target_ic);
    if let Some(ci) = idx_ic.col_idx {
        let expected_ref = if tbl.rowid_alias_col == Some(ci) {
            TargetColRef::Rowid
        } else {
            TargetColRef::Col(ci)
        };
        if let Expr::Column(cref) = unwrap_trivial_row_value(&target_expr) {
            resolve_target_col_ref(tbl, schema_name, target_alias, cref) == Some(expected_ref)
        } else {
            false
        }
    } else if let Some(ref ie) = idx_ic.expr {
        upsert_expr_equiv(tbl, schema_name, target_alias, &target_expr, ie)
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn analyze_upsert_clauses(
    ins: &InsertStmt,
    tbl: &TableSchema,
    schema_name: &str,
    ordered_uniques: &[IndexSchema],
    db_set: &DatabaseSet,
    dml_ctes: &CteStore,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    visited: &mut Vec<(usize, usize)>,
) -> Result<Vec<ResolvedUpsertTarget>> {
    let target_scope = make_static_table_scope(tbl, schema_name, ins.alias.as_deref());
    let excluded_scope = make_excluded_table_scope(tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let do_update_scopes = [excluded_scope, target_scope.clone()];
    let multi_clauses = ins.upsert_clauses.len() > 1;
    let mut resolved = Vec::with_capacity(ins.upsert_clauses.len());

    for (clause_idx, clause) in ins.upsert_clauses.iter().enumerate() {
        let rtarget = if let Some(ref target) = clause.target {
            for ic in &target.columns {
                if ic.nulls_last == Some(false) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unsupported use of NULLS FIRST",
                    ));
                }
                if ic.nulls_last == Some(true) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unsupported use of NULLS LAST",
                    ));
                }
                if let Some(ref cname) = ic.collation
                    && !collations.has_collation(cname)
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such collation sequence: {cname}"),
                    ));
                }
                let ic_expr = indexed_col_to_expr(ic);
                validate_collations_in_expr(&ic_expr, collations)?;
                validate_expr_static(
                    &ic_expr,
                    std::slice::from_ref(&target_scope),
                    db_set,
                    dml_ctes,
                    functions,
                    false,
                    false,
                    visited,
                )?;
            }
            if let Some(ref tw) = target.where_clause {
                validate_collations_in_expr(tw, collations)?;
                validate_expr_static(
                    tw,
                    std::slice::from_ref(&target_scope),
                    db_set,
                    dml_ctes,
                    functions,
                    false,
                    false,
                    visited,
                )?;
            }

            // 1. Check Rowid / INTEGER PRIMARY KEY match (`upsert.c:94-112`)
            let is_rowid_match = !tbl.without_rowid
                && target.columns.len() == 1
                && target.columns[0].collation.is_none()
                && {
                    let e0 = indexed_col_to_expr(&target.columns[0]);
                    !matches!(e0, Expr::Collate { .. })
                        && matches!(unwrap_trivial_row_value(&e0), Expr::Column(cref)
                            if resolve_target_col_ref(tbl, schema_name, ins.alias.as_deref(), cref)
                                == Some(TargetColRef::Rowid))
                };

            if is_rowid_match {
                ResolvedUpsertTarget::RowidPk
            } else {
                // 2. Search `ordered_uniques` (`pTab->pIndex` order, including `WITHOUT ROWID` PK)
                let mut matched_idx = None;
                for (u_idx, idx) in ordered_uniques.iter().enumerate() {
                    if idx.columns.len() != target.columns.len() {
                        continue;
                    }
                    if let Some(ref iw) = idx.where_clause {
                        let Some(ref tw) = target.where_clause else {
                            continue;
                        };
                        if !upsert_expr_equiv(tbl, schema_name, ins.alias.as_deref(), tw, iw) {
                            continue;
                        }
                    }
                    let mut used_target = vec![false; target.columns.len()];
                    let mut all_cols_ok = true;
                    for idx_col in &idx.columns {
                        if let Some(tpos) = (0..target.columns.len()).find(|&ti| {
                            !used_target[ti]
                                && match_target_col_to_index_col(
                                    tbl,
                                    schema_name,
                                    ins.alias.as_deref(),
                                    &target.columns[ti],
                                    idx_col,
                                )
                        }) {
                            used_target[tpos] = true;
                        } else {
                            all_cols_ok = false;
                            break;
                        }
                    }
                    if all_cols_ok {
                        matched_idx = Some(u_idx);
                        break;
                    }
                }
                match matched_idx {
                    Some(u_idx) => ResolvedUpsertTarget::UniqueIndex(u_idx),
                    None => {
                        let msg = if multi_clauses {
                            format!(
                                "{} ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint",
                                ordinal_suffix(clause_idx + 1)
                            )
                        } else {
                            "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"
                                .to_owned()
                        };
                        return Err(SqliteError::new(ErrorCode::Error, msg));
                    }
                }
            }
        } else {
            ResolvedUpsertTarget::CatchAll
        };

        if let UpsertAction::DoUpdate {
            ref assignments,
            ref where_clause,
        } = clause.action
        {
            validate_set_assignments_static(
                tbl,
                assignments,
                &do_update_scopes,
                db_set,
                dml_ctes,
                functions,
                collations,
                false,
                visited,
            )?;
            if let Some(dw) = where_clause {
                validate_collations_in_expr(dw, collations)?;
                validate_expr_static(
                    dw,
                    &do_update_scopes,
                    db_set,
                    dml_ctes,
                    functions,
                    false,
                    false,
                    visited,
                )?;
            }
        }

        resolved.push(rtarget);
    }

    Ok(resolved)
}

fn build_upsert_check_steps(
    tbl: &TableSchema,
    ins_or_conflict: Option<ConflictAction>,
    ordered_uniques: &[IndexSchema],
    analyzed: &[ResolvedUpsertTarget],
) -> Vec<UpsertCheckStep> {
    let mut steps = Vec::with_capacity(ordered_uniques.len() + 1);
    if analyzed.len() == 1 && analyzed[0] == ResolvedUpsertTarget::CatchAll {
        if !tbl.without_rowid {
            steps.push(UpsertCheckStep::RowidPk {
                clause_idx: Some(0),
            });
        }
        for u_idx in 0..ordered_uniques.len() {
            steps.push(UpsertCheckStep::UniqueIdx {
                u_idx,
                clause_idx: Some(0),
            });
        }
        return steps;
    }

    let catch_all_idx = analyzed
        .iter()
        .position(|&t| t == ResolvedUpsertTarget::CatchAll);
    let mut rowid_handled = tbl.without_rowid;
    let mut used_unique = vec![false; ordered_uniques.len()];

    for (c_idx, &target) in analyzed.iter().enumerate() {
        match target {
            ResolvedUpsertTarget::RowidPk => {
                if !rowid_handled {
                    rowid_handled = true;
                    steps.push(UpsertCheckStep::RowidPk {
                        clause_idx: Some(c_idx),
                    });
                }
            }
            ResolvedUpsertTarget::UniqueIndex(u_idx) => {
                if !used_unique[u_idx] {
                    used_unique[u_idx] = true;
                    steps.push(UpsertCheckStep::UniqueIdx {
                        u_idx,
                        clause_idx: Some(c_idx),
                    });
                }
            }
            ResolvedUpsertTarget::CatchAll => break,
        }
    }

    let ipk_eff = ins_or_conflict.unwrap_or(tbl.rowid_pk_conflict);
    if !rowid_handled && (catch_all_idx.is_some() || ipk_eff != ConflictAction::Replace) {
        rowid_handled = true;
        steps.push(UpsertCheckStep::RowidPk {
            clause_idx: catch_all_idx,
        });
    }

    for (u_idx, &used) in used_unique.iter().enumerate() {
        if !used {
            steps.push(UpsertCheckStep::UniqueIdx {
                u_idx,
                clause_idx: catch_all_idx,
            });
        }
    }

    if !rowid_handled {
        steps.push(UpsertCheckStep::RowidPk { clause_idx: None });
    }

    steps
}

/// Evaluates rowid allocation, column affinities, `GENERATED` columns, `STRICT`, `NOT NULL`, and
/// `CHECK` constraints for a proposed inserted row (`Ok(None)` when skipped by `OR IGNORE`).
#[allow(clippy::too_many_arguments)]
fn prepare_proposed_insert_row(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    schema_name: &str,
    candidate_rowid: Option<i64>,
    mut raw_vals: Vec<Value>,
    stmt_conflict: Option<ConflictAction>,
    params: &[Value],
) -> Result<Option<(i64, TableRowKey, Vec<Value>)>> {
    ctx.ensure_write_db(db_id, false)?;
    let rowid = if tbl.without_rowid {
        0
    } else {
        let mut final_rowid = match candidate_rowid {
            Some(r) => Some(r),
            None => match tbl.rowid_alias_col {
                Some(ipk) => {
                    if raw_vals[ipk].is_null() {
                        None
                    } else {
                        let coerced = TypeAffinity::Integer.apply(raw_vals[ipk].clone());
                        let Some(i) = coerced.as_i64() else {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        };
                        Some(i)
                    }
                }
                None => None,
            },
        };
        if final_rowid.is_none() {
            final_rowid = Some(allocate_table_rowid(ctx, db_id, tbl)?);
        }
        final_rowid.unwrap()
    };

    for (i, col) in tbl.columns.iter().enumerate() {
        if col.generated.is_some() {
            continue;
        }
        if tbl.rowid_alias_col == Some(i) {
            raw_vals[i] = Value::Integer(rowid);
        } else {
            raw_vals[i] = col.affinity.apply(raw_vals[i].clone());
            if tbl.strict
                && col.type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = raw_vals[i]
            {
                raw_vals[i] = Value::real(iv as f64);
            }
        }
    }

    for i in 0..tbl.columns.len() {
        if let Some((ref gexpr, _)) = tbl.columns[i].generated {
            let scope = make_single_table_scope(tbl, schema_name, None, rowid, &raw_vals);
            let gv = eval_expr(ctx, gexpr, params, &[scope], &CteStore::default(), None)?;
            let mut coerced = tbl.columns[i].affinity.apply(gv);
            if tbl.strict
                && tbl.columns[i].type_name.trim().eq_ignore_ascii_case("REAL")
                && let Value::Integer(iv) = coerced
            {
                coerced = Value::real(iv as f64);
            }
            raw_vals[i] = coerced;
        }
    }

    if tbl.strict {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        for (i, col) in tbl.columns.iter().enumerate() {
            if let Err(err) = check_strict_column_type(&tbl.name, col, &raw_vals[i]) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(err.with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    for (i, col) in tbl.columns.iter().enumerate() {
        if let Some(col_conflict) = col.not_null
            && raw_vals[i].is_null()
        {
            let eff = stmt_conflict.unwrap_or(col_conflict);
            match eff {
                ConflictAction::Ignore => return Ok(None),
                ConflictAction::Replace if col.default_expr.is_some() => {
                    let dv = eval_expr(
                        ctx,
                        col.default_expr.as_ref().unwrap(),
                        params,
                        &[],
                        &CteStore::default(),
                        None,
                    )?;
                    if dv.is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                        ));
                    }
                    let mut coerced = col.affinity.apply(dv);
                    if tbl.strict
                        && col.type_name.trim().eq_ignore_ascii_case("REAL")
                        && let Value::Integer(iv) = coerced
                    {
                        coerced = Value::real(iv as f64);
                    }
                    raw_vals[i] = coerced;
                }
                _ => {
                    return Err(SqliteError::constraint(
                        extended::SQLITE_CONSTRAINT_NOTNULL,
                        format!("NOT NULL constraint failed: {}.{}", tbl.name, col.name),
                    )
                    .with_conflict_effect(conflict_action_to_effect(eff)));
                }
            }
        }
    }

    if !tbl.checks.is_empty() {
        let eff = stmt_conflict.unwrap_or(ConflictAction::Abort);
        let scope = make_single_table_scope(tbl, schema_name, None, rowid, &raw_vals);
        for chk in &tbl.checks {
            let cv = eval_expr(
                ctx,
                &chk.expr,
                params,
                std::slice::from_ref(&scope),
                &CteStore::default(),
                None,
            )?;
            if cv.truthiness() == Some(false) {
                if eff == ConflictAction::Ignore {
                    return Ok(None);
                }
                return Err(SqliteError::constraint(
                    extended::SQLITE_CONSTRAINT_CHECK,
                    format!("CHECK constraint failed: {}", chk.label),
                )
                .with_conflict_effect(conflict_action_to_effect(eff)));
            }
        }
    }

    let final_key = if tbl.without_rowid {
        TableRowKey::WithoutRowidPk(tbl.extract_without_rowid_pk(&raw_vals))
    } else {
        TableRowKey::Rowid(rowid)
    };

    Ok(Some((rowid, final_key, raw_vals)))
}

fn is_table_btree_empty(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    root_pgno: Pgno,
) -> Result<bool> {
    Ok(ctx.scan_table_in_db(db_id, root_pgno)?.is_empty())
}

fn scan_table_rowids_only(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    root_pgno: Pgno,
) -> Result<Vec<i64>> {
    Ok(ctx
        .scan_table_in_db(db_id, root_pgno)?
        .into_iter()
        .map(|(rid, _)| rid)
        .collect())
}

fn expr_mentions_implicit_rowid(expr: &Expr, tbl: &TableSchema) -> bool {
    match expr {
        Expr::Column(cref) => tbl.is_implicit_rowid_name(&cref.column),
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::Cast { expr: e, .. }
        | Expr::IsNull { expr: e, .. } => expr_mentions_implicit_rowid(e, tbl),
        Expr::Binary { left, right, .. } => {
            expr_mentions_implicit_rowid(left, tbl) || expr_mentions_implicit_rowid(right, tbl)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            expr_mentions_implicit_rowid(e, tbl)
                || expr_mentions_implicit_rowid(low, tbl)
                || expr_mentions_implicit_rowid(high, tbl)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            expr_mentions_implicit_rowid(e, tbl)
                || expr_mentions_implicit_rowid(pattern, tbl)
                || escape
                    .as_deref()
                    .is_some_and(|esc| expr_mentions_implicit_rowid(esc, tbl))
        }
        Expr::InList { expr: e, list, .. } => {
            expr_mentions_implicit_rowid(e, tbl)
                || list.iter().any(|x| expr_mentions_implicit_rowid(x, tbl))
        }
        Expr::FunctionCall { args, filter, .. } => {
            let args_r = match args {
                FunctionArgs::Star => false,
                FunctionArgs::Exprs(es) => es.iter().any(|e| expr_mentions_implicit_rowid(e, tbl)),
            };
            args_r
                || filter
                    .as_deref()
                    .is_some_and(|f| expr_mentions_implicit_rowid(f, tbl))
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            operand
                .as_deref()
                .is_some_and(|o| expr_mentions_implicit_rowid(o, tbl))
                || branches.iter().any(|(w, t)| {
                    expr_mentions_implicit_rowid(w, tbl) || expr_mentions_implicit_rowid(t, tbl)
                })
                || else_expr
                    .as_deref()
                    .is_some_and(|el| expr_mentions_implicit_rowid(el, tbl))
        }
        Expr::RowValue(es) => es.iter().any(|e| expr_mentions_implicit_rowid(e, tbl)),
        _ => false,
    }
}

fn xfer_compatible_index(dest: &IndexSchema, src: &IndexSchema) -> bool {
    if dest.columns.len() != src.columns.len()
        || dest.trailing_pk_cols.len() != src.trailing_pk_cols.len()
        || dest.unique != src.unique
    {
        return false;
    }
    if dest.unique && dest.conflict != src.conflict {
        return false;
    }
    for (dc, sc) in dest.columns.iter().zip(&src.columns) {
        if dc.col_idx != sc.col_idx || dc.expr != sc.expr || dc.sort_order != sc.sort_order {
            return false;
        }
    }
    dest.where_clause == src.where_clause
}

fn check_insert_xfer_eligible(
    ins: &InsertStmt,
    dest_db_id: DatabaseId,
    dest_tbl: &TableSchema,
    dest_is_catalog: bool,
    db_set: &DatabaseSet,
    foreign_keys: bool,
    count_changes: bool,
) -> Option<(DatabaseId, TableSchema)> {
    if dest_is_catalog
        || !ins.ctes.is_empty()
        || ins.columns.is_some()
        || !ins.upsert_clauses.is_empty()
        || !ins.returning.is_empty()
        || count_changes
        || (foreign_keys && !dest_tbl.foreign_keys.is_empty())
    {
        return None;
    }
    let InsertSource::Select(sel) = &ins.source else {
        return None;
    };
    if !sel.ctes.is_empty()
        || !sel.body.compounds.is_empty()
        || !sel.order_by.is_empty()
        || sel.limit.is_some()
    {
        return None;
    }
    let SelectCore::Select {
        distinct: false,
        projection,
        from: Some(fc),
        selection: None,
        group_by,
        having: None,
    } = &sel.body.head
    else {
        return None;
    };
    if !group_by.is_empty()
        || !fc.joins.is_empty()
        || projection.len() != 1
        || !matches!(projection[0], ResultColumn::Star)
    {
        return None;
    }
    let TableOrSubquery::Table {
        name: src_qn,
        table_args: None,
        ..
    } = &fc.head
    else {
        return None;
    };
    let (src_db_id, src_tbl) = db_set.find_user_table_in_set(src_qn)?;
    if src_db_id == dest_db_id && src_tbl.root_pgno == dest_tbl.root_pgno {
        return None;
    }
    if dest_tbl.without_rowid != src_tbl.without_rowid
        || dest_tbl.columns.len() != src_tbl.columns.len()
        || dest_tbl.rowid_alias_col != src_tbl.rowid_alias_col
    {
        return None;
    }
    if dest_tbl.strict {
        if !src_tbl.strict {
            return None;
        }
        for (dc, sc) in dest_tbl.columns.iter().zip(&src_tbl.columns) {
            let dt = dc.type_name.trim().to_ascii_uppercase();
            let st = sc.type_name.trim().to_ascii_uppercase();
            let ok = dt == "ANY"
                || dt == st
                || (dt == "INT" && st == "INTEGER")
                || (dt == "INTEGER" && st == "INT");
            if !ok {
                return None;
            }
        }
    }
    for (i, (dc, sc)) in dest_tbl.columns.iter().zip(&src_tbl.columns).enumerate() {
        if dc.generated != sc.generated
            || dc.affinity != sc.affinity
            || dc.collation != sc.collation
        {
            return None;
        }
        if dc.not_null.is_some() && sc.not_null.is_none() {
            return None;
        }
        if dc.generated.is_none() && i > 0 && dc.default_expr != sc.default_expr {
            return None;
        }
    }
    let mut dest_indexes = db_set
        .slot(dest_db_id)
        .map(|s| s.schema.indexes_for_table(&dest_tbl.name))
        .unwrap_or_default();
    if let Some(pk_idx) = dest_tbl.without_rowid_pk_as_index() {
        dest_indexes.push(pk_idx);
    }
    let mut src_indexes = db_set
        .slot(src_db_id)
        .map(|s| s.schema.indexes_for_table(&src_tbl.name))
        .unwrap_or_default();
    if let Some(pk_idx) = src_tbl.without_rowid_pk_as_index() {
        src_indexes.push(pk_idx);
    }
    for d_idx in &dest_indexes {
        let matched = src_indexes
            .iter()
            .find(|s_idx| xfer_compatible_index(d_idx, s_idx))?;
        if src_db_id == dest_db_id && matched.root_pgno == d_idx.root_pgno {
            return None;
        }
    }
    if !dest_tbl.checks.is_empty() {
        if dest_tbl.checks.len() != src_tbl.checks.len() {
            return None;
        }
        for (dc, sc) in dest_tbl.checks.iter().zip(&src_tbl.checks) {
            if dc.expr != sc.expr {
                return None;
            }
        }
        if dest_tbl.rowid_alias_col.is_none()
            && dest_tbl
                .checks
                .iter()
                .any(|c| expr_mentions_implicit_rowid(&c.expr, dest_tbl))
        {
            return None;
        }
    }
    Some((src_db_id, src_tbl.clone()))
}

fn exec_insert(
    ctx: &mut ExecContext<'_>,
    ins: &InsertStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let (db_id, initial_tbl, initial_sname, is_catalog) = resolve_dml_target_table_in_set_opts(
        ctx.db_set,
        &ins.table,
        *ctx.writable_schema,
        !ins.upsert_clauses.is_empty(),
    )?;
    let dml_ctes = CteStore::default().with_scope(&ins.ctes);

    // Resolve target column mapping (`None` = all non-generated columns in order; `Some(names)` = mapped columns, `usize::MAX` for implicit `rowid`)
    let col_targets: Option<Vec<usize>> = if let Some(ref cnames) = ins.columns {
        let mut idxs = Vec::with_capacity(cnames.len());
        for cn in cnames {
            if let Some((ci, col)) = initial_tbl.find_column(cn) {
                if col.generated.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot INSERT into generated column \"{}\"", col.name),
                    ));
                }
                idxs.push(ci);
            } else if initial_tbl.is_implicit_rowid_name(cn) {
                idxs.push(usize::MAX);
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} has no column named {cn}", initial_tbl.name),
                ));
            }
        }
        Some(idxs)
    } else {
        None
    };

    // Pre-analyze UPSERT clauses before evaluating input query so invalid conflict targets fail immediately
    if !ins.upsert_clauses.is_empty() {
        let init_indexes = if is_catalog {
            Vec::new()
        } else {
            ctx.db_set
                .slot(db_id)
                .map(|s| s.schema.indexes_for_table(&initial_tbl.name))
                .unwrap_or_default()
        };
        let init_ordered = ordered_unique_indexes(&initial_tbl, &init_indexes);
        let _ = analyze_upsert_clauses(
            ins,
            &initial_tbl,
            &initial_sname,
            &init_ordered,
            ctx.db_set,
            &dml_ctes,
            ctx.functions,
            ctx.collations,
            &mut Vec::new(),
        )?;
    }

    // Materialize input value rows BEFORE mutating target table (handles `INSERT INTO t SELECT * FROM t` safely!)
    let xfer_src = check_insert_xfer_eligible(
        ins,
        db_id,
        &initial_tbl,
        is_catalog,
        ctx.db_set,
        *ctx.foreign_keys,
        *ctx.count_changes,
    );
    let mut xfer_preserve_rowids: Option<Vec<i64>> = None;
    let source_value_rows: Vec<Vec<Value>> = match &ins.source {
        InsertSource::DefaultValues => vec![Vec::new()],
        InsertSource::Values(expr_rows) => {
            let mut out = Vec::with_capacity(expr_rows.len());
            for erow in expr_rows {
                let mut vrow = Vec::with_capacity(erow.len());
                for e in erow {
                    vrow.push(eval_expr(ctx, e, params, &[], &dml_ctes, None)?);
                }
                out.push(vrow);
            }
            out
        }
        InsertSource::Select(sel) => {
            let mut rows = exec_select(ctx, sel, params, &[], &dml_ctes)?.rows;
            if let Some((src_db_id, ref src_tbl)) = xfer_src {
                ctx.db_set.record_xferopt();
                if initial_tbl.columns.iter().any(|c| c.generated.is_some()) {
                    for row in &mut rows {
                        if row.len() == initial_tbl.columns.len() {
                            let mut filtered = Vec::with_capacity(initial_tbl.columns.len());
                            for (i, val) in row.drain(..).enumerate() {
                                if initial_tbl.columns[i].generated.is_none() {
                                    filtered.push(val);
                                }
                            }
                            *row = filtered;
                        }
                    }
                }
                if !initial_tbl.without_rowid
                    && initial_tbl.rowid_alias_col.is_none()
                    && let Ok(dest_empty) = is_table_btree_empty(ctx, db_id, initial_tbl.root_pgno)
                    && dest_empty
                    && let Ok(src_rids) = scan_table_rowids_only(ctx, src_db_id, src_tbl.root_pgno)
                    && src_rids.len() == rows.len()
                {
                    xfer_preserve_rowids = Some(src_rids);
                }
            }
            rows
        }
    };

    let (tbl, indexes, schema_name) =
        prepare_dml_write_slot(ctx, db_id, &ins.table, is_catalog, initial_tbl)?;
    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    let ordered_uniques = ordered_unique_indexes(&tbl, &indexes);
    let (upsert_steps, resolved_do_updates) = if ins.upsert_clauses.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let analyzed = analyze_upsert_clauses(
            ins,
            &tbl,
            &schema_name,
            &ordered_uniques,
            ctx.db_set,
            &dml_ctes,
            ctx.functions,
            ctx.collations,
            &mut Vec::new(),
        )?;
        let steps = build_upsert_check_steps(&tbl, ins.or_conflict, &ordered_uniques, &analyzed);
        let mut r_assigns = Vec::with_capacity(ins.upsert_clauses.len());
        for uc in &ins.upsert_clauses {
            match &uc.action {
                UpsertAction::DoNothing => r_assigns.push(Vec::new()),
                UpsertAction::DoUpdate { assignments, .. } => {
                    r_assigns.push(resolve_set_assignments_for_table(&tbl, assignments)?);
                }
            }
        }
        (steps, r_assigns)
    };

    let implicit_non_gen_cols: Vec<usize> = tbl
        .columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.generated.is_none())
        .map(|(i, _)| i)
        .collect();

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if ins.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&ins.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();
    let mut inserted_count: usize = 0;

    for (row_idx, src_row) in source_value_rows.into_iter().enumerate() {
        let mut explicit_rowid: Option<i64> = xfer_preserve_rowids
            .as_ref()
            .and_then(|rids| rids.get(row_idx).copied());
        let mut col_vals = Vec::with_capacity(tbl.columns.len());
        for col in &tbl.columns {
            let def = match &col.default_expr {
                Some(de) => eval_expr(ctx, de, params, &[], &CteStore::default(), None)?,
                None => Value::Null,
            };
            col_vals.push(def);
        }

        if matches!(ins.source, InsertSource::DefaultValues) {
            // Keep defaults
        } else if let Some(ref targets) = col_targets {
            if src_row.len() != targets.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("{} values for {} columns", src_row.len(), targets.len()),
                ));
            }
            for (&t_idx, val) in targets.iter().zip(src_row) {
                if t_idx == usize::MAX {
                    if !val.is_null() {
                        let c = TypeAffinity::Integer.apply(val);
                        let rid = c.as_i64().ok_or_else(|| {
                            SqliteError::new(ErrorCode::Mismatch, "datatype mismatch")
                        })?;
                        explicit_rowid = Some(rid);
                    }
                } else {
                    col_vals[t_idx] = val;
                }
            }
        } else {
            if src_row.len() != implicit_non_gen_cols.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        tbl.name,
                        implicit_non_gen_cols.len(),
                        src_row.len()
                    ),
                ));
            }
            for (&ci, val) in implicit_non_gen_cols.iter().zip(src_row) {
                col_vals[ci] = val;
            }
        }

        let write_res: Result<Option<(TableRowKey, Vec<Value>)>> = if ins.upsert_clauses.is_empty()
        {
            let r = validate_and_write_row(
                ctx,
                db_id,
                &tbl,
                &indexes,
                explicit_rowid,
                col_vals,
                ins.or_conflict,
                None,
                params,
                true,
                true,
                &mut stmt_fk_suspects,
                0,
                Some((MutationOrigin::TopLevelInsert, None)),
                is_catalog,
            );
            if matches!(r, Ok(Some(_))) {
                inserted_count += 1;
            }
            r
        } else {
            (|| -> Result<Option<(TableRowKey, Vec<Value>)>> {
                let Some((rowid, final_key, raw_vals)) = prepare_proposed_insert_row(
                    ctx,
                    db_id,
                    &tbl,
                    &schema_name,
                    explicit_rowid,
                    col_vals,
                    ins.or_conflict,
                    params,
                )?
                else {
                    return Ok(None);
                };

                let mut matched_upsert: Option<(usize, TableRowKey, Vec<Value>)> = None;
                let mut did_replace_delete = false;

                for step in &upsert_steps {
                    match *step {
                        UpsertCheckStep::RowidPk { clause_idx } => {
                            if let Some(cpayload) =
                                seek_table_raw_entry(ctx, db_id, &tbl, &final_key)?
                            {
                                let ckey = TableRowKey::Rowid(rowid);
                                let cvals = decode_table_logical_row(
                                    ctx, db_id, &tbl, &ckey, &cpayload, params,
                                )?;
                                if let Some(ci) = clause_idx {
                                    matched_upsert = Some((ci, ckey, cvals));
                                    break;
                                }
                                let eff = ins.or_conflict.unwrap_or(tbl.rowid_pk_conflict);
                                match eff {
                                    ConflictAction::Ignore => return Ok(None),
                                    ConflictAction::Replace => {
                                        if *ctx.foreign_keys {
                                            check_parent_delete_restrict(
                                                ctx, db_id, &tbl, &cvals, params,
                                            )?;
                                        }
                                        delete_row_via_pipeline(
                                            ctx,
                                            db_id,
                                            &tbl,
                                            &indexes,
                                            &ckey,
                                            Some(&cvals),
                                            MutationOrigin::ReplaceConflictDelete,
                                            ConflictAction::Replace,
                                            is_catalog,
                                            params,
                                        )?;
                                        did_replace_delete = true;
                                        if *ctx.foreign_keys {
                                            apply_parent_delete_fk_actions(
                                                ctx,
                                                db_id,
                                                &tbl,
                                                &cvals,
                                                params,
                                                &mut stmt_fk_suspects,
                                                1,
                                            )?;
                                        }
                                    }
                                    _ => {
                                        let (ext_code, col_label) = match tbl.rowid_alias_col {
                                            Some(ipk) => (
                                                extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                                                tbl.columns[ipk].name.as_str(),
                                            ),
                                            None => (extended::SQLITE_CONSTRAINT_ROWID, "rowid"),
                                        };
                                        return Err(SqliteError::constraint(
                                            ext_code,
                                            format!(
                                                "UNIQUE constraint failed: {}.{}",
                                                tbl.name, col_label
                                            ),
                                        )
                                        .with_conflict_effect(conflict_action_to_effect(eff)));
                                    }
                                }
                            }
                        }
                        UpsertCheckStep::UniqueIdx { u_idx, clause_idx } => {
                            let idx = &ordered_uniques[u_idx];
                            if idx.table_without_rowid && idx.origin == "pk" {
                                if let Some(cpayload) =
                                    seek_table_raw_entry(ctx, db_id, &tbl, &final_key)?
                                {
                                    let decoded = decode_record_with_encoding(&cpayload, enc)?;
                                    let n_pk = tbl.without_rowid_pk_cols.len();
                                    let ckey = TableRowKey::WithoutRowidPk(
                                        decoded[..n_pk.min(decoded.len())].to_vec(),
                                    );
                                    let cvals = decode_table_logical_row(
                                        ctx, db_id, &tbl, &ckey, &cpayload, params,
                                    )?;
                                    if let Some(ci) = clause_idx {
                                        matched_upsert = Some((ci, ckey, cvals));
                                        break;
                                    }
                                    let eff =
                                        ins.or_conflict.unwrap_or(tbl.without_rowid_pk_conflict);
                                    match eff {
                                        ConflictAction::Ignore => return Ok(None),
                                        ConflictAction::Replace => {
                                            if *ctx.foreign_keys {
                                                check_parent_delete_restrict(
                                                    ctx, db_id, &tbl, &cvals, params,
                                                )?;
                                            }
                                            delete_row_via_pipeline(
                                                ctx,
                                                db_id,
                                                &tbl,
                                                &indexes,
                                                &ckey,
                                                Some(&cvals),
                                                MutationOrigin::ReplaceConflictDelete,
                                                ConflictAction::Replace,
                                                is_catalog,
                                                params,
                                            )?;
                                            did_replace_delete = true;
                                            if *ctx.foreign_keys {
                                                apply_parent_delete_fk_actions(
                                                    ctx,
                                                    db_id,
                                                    &tbl,
                                                    &cvals,
                                                    params,
                                                    &mut stmt_fk_suspects,
                                                    1,
                                                )?;
                                            }
                                        }
                                        _ => {
                                            return Err(SqliteError::constraint(
                                                extended::SQLITE_CONSTRAINT_PRIMARYKEY,
                                                format_unique_index_failure(&tbl, idx),
                                            )
                                            .with_conflict_effect(conflict_action_to_effect(eff)));
                                        }
                                    }
                                }
                                continue;
                            }

                            if !index_row_passes_partial_where(
                                ctx,
                                &schema_name,
                                &tbl,
                                idx,
                                rowid,
                                &raw_vals,
                                params,
                            )? {
                                continue;
                            }
                            let indexed_vals = eval_index_column_values(
                                ctx,
                                &schema_name,
                                &tbl,
                                idx,
                                rowid,
                                &raw_vals,
                                params,
                            )?;
                            if indexed_vals.iter().any(Value::is_null) {
                                continue;
                            }
                            let orders = idx.key_sort_orders();
                            let hits = if let Some(ref mut bt) =
                                ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                                && bt.pager().page_count() > 0
                            {
                                bt.seek_index_equal(
                                    idx.root_pgno,
                                    &indexed_vals,
                                    &orders[..indexed_vals.len()],
                                )?
                            } else {
                                Vec::new()
                            };
                            let mut conflicting_keys = Vec::new();
                            for hit in hits {
                                let hit_key_opt = if tbl.without_rowid {
                                    Some(TableRowKey::WithoutRowidPk(
                                        idx.extract_without_rowid_pk_from_hit(&tbl, &hit),
                                    ))
                                } else {
                                    hit.last().and_then(Value::as_i64).map(TableRowKey::Rowid)
                                };
                                if let Some(hk) = hit_key_opt {
                                    conflicting_keys.push(hk);
                                }
                            }
                            if let Some(first_ckey) = conflicting_keys.first() {
                                if let Some(ci) = clause_idx {
                                    if let Some(cpayload) =
                                        seek_table_raw_entry(ctx, db_id, &tbl, first_ckey)?
                                    {
                                        let cvals = decode_table_logical_row(
                                            ctx, db_id, &tbl, first_ckey, &cpayload, params,
                                        )?;
                                        matched_upsert = Some((ci, first_ckey.clone(), cvals));
                                        break;
                                    }
                                } else {
                                    let eff = ins.or_conflict.unwrap_or(idx.conflict);
                                    match eff {
                                        ConflictAction::Ignore => return Ok(None),
                                        ConflictAction::Replace => {
                                            for ckey in conflicting_keys {
                                                if let Some(cpayload) =
                                                    seek_table_raw_entry(ctx, db_id, &tbl, &ckey)?
                                                {
                                                    let cvals = decode_table_logical_row(
                                                        ctx, db_id, &tbl, &ckey, &cpayload, params,
                                                    )?;
                                                    if *ctx.foreign_keys {
                                                        check_parent_delete_restrict(
                                                            ctx, db_id, &tbl, &cvals, params,
                                                        )?;
                                                    }
                                                    delete_row_via_pipeline(
                                                        ctx,
                                                        db_id,
                                                        &tbl,
                                                        &indexes,
                                                        &ckey,
                                                        Some(&cvals),
                                                        MutationOrigin::ReplaceConflictDelete,
                                                        ConflictAction::Replace,
                                                        is_catalog,
                                                        params,
                                                    )?;
                                                    did_replace_delete = true;
                                                    if *ctx.foreign_keys {
                                                        apply_parent_delete_fk_actions(
                                                            ctx,
                                                            db_id,
                                                            &tbl,
                                                            &cvals,
                                                            params,
                                                            &mut stmt_fk_suspects,
                                                            1,
                                                        )?;
                                                    }
                                                }
                                            }
                                        }
                                        _ => {
                                            let ext_code = if idx.origin == "pk" {
                                                extended::SQLITE_CONSTRAINT_PRIMARYKEY
                                            } else {
                                                extended::SQLITE_CONSTRAINT_UNIQUE
                                            };
                                            return Err(SqliteError::constraint(
                                                ext_code,
                                                format_unique_index_failure(&tbl, idx),
                                            )
                                            .with_conflict_effect(conflict_action_to_effect(eff)));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some((c_idx, conflict_key, old_vals)) = matched_upsert {
                    match &ins.upsert_clauses[c_idx].action {
                        UpsertAction::DoNothing => return Ok(None),
                        UpsertAction::DoUpdate { where_clause, .. } => {
                            let excluded_scope = make_excluded_table_scope(&tbl, rowid, &raw_vals);
                            let old_scope = make_single_table_scope(
                                &tbl,
                                &schema_name,
                                ins.alias.as_deref(),
                                conflict_key.rowid_or_zero(),
                                &old_vals,
                            );
                            let scopes = [excluded_scope, old_scope];
                            if let Some(dw) = where_clause {
                                let cond = eval_expr(ctx, dw, params, &scopes, &dml_ctes, None)?;
                                if cond.truthiness() != Some(true) {
                                    return Ok(None);
                                }
                            }
                            let (new_rowid_opt, new_vals) = apply_resolved_set_assignments(
                                ctx,
                                &tbl,
                                &conflict_key,
                                &old_vals,
                                &resolved_do_updates[c_idx],
                                &scopes,
                                &dml_ctes,
                                params,
                            )?;
                            let excluded_mutation_row = MutationRow {
                                rowid,
                                values: raw_vals,
                            };
                            return validate_and_write_row(
                                ctx,
                                db_id,
                                &tbl,
                                &indexes,
                                new_rowid_opt,
                                new_vals,
                                Some(ConflictAction::Abort),
                                Some(&conflict_key),
                                params,
                                false,
                                true,
                                &mut stmt_fk_suspects,
                                0,
                                Some((MutationOrigin::UpsertDoUpdate, Some(excluded_mutation_row))),
                                is_catalog,
                            );
                        }
                    }
                }

                // No UPSERT clause triggered — finish normal INSERT write (`addrRecheck` if `OR REPLACE` ran)
                if did_replace_delete
                    && *ctx.foreign_keys
                    && ctx
                        .db_set
                        .slot(db_id)
                        .is_some_and(|s| !find_referencing_fks(&s.schema, &tbl.name).is_empty())
                {
                    recheck_uniqueness_abort(
                        ctx,
                        db_id,
                        &tbl,
                        &ordered_uniques,
                        &schema_name,
                        rowid,
                        &final_key,
                        &raw_vals,
                        None,
                        enc,
                        params,
                    )?;
                }

                let phys_rec = encode_table_physical_record(&tbl, &raw_vals, enc);
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                {
                    if tbl.without_rowid {
                        let pk_orders = tbl.without_rowid_pk_orders();
                        bt.insert_index(tbl.root_pgno, &phys_rec, &pk_orders)?;
                    } else {
                        bt.insert_table(tbl.root_pgno, rowid, &phys_rec)?;
                    }
                }
                for idx in &indexes {
                    if index_row_passes_partial_where(
                        ctx,
                        &schema_name,
                        &tbl,
                        idx,
                        rowid,
                        &raw_vals,
                        params,
                    )? {
                        let keys = eval_index_key_values(
                            ctx,
                            &schema_name,
                            &tbl,
                            idx,
                            &final_key,
                            &raw_vals,
                            params,
                        )?;
                        let idx_rec = encode_record_with_encoding(&keys, enc);
                        let orders = idx.key_sort_orders();
                        if let Some(ref mut bt) =
                            ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                        {
                            bt.insert_index(idx.root_pgno, &idx_rec, &orders)?;
                        }
                    }
                }
                if tbl.autoincrement {
                    update_sqlite_sequence(ctx, db_id, &tbl.name, rowid)?;
                }
                if let Some(slot) = ctx.db_set.slot(db_id)
                    && let Some(ref bt) = slot.btree
                    && bt.pager().page_count() > slot.max_page_count
                {
                    return Err(SqliteError::new(
                        ErrorCode::Full,
                        "database or disk is full",
                    ));
                }
                if *ctx.foreign_keys {
                    register_child_fk_checks(
                        ctx,
                        db_id,
                        &tbl,
                        &final_key,
                        &raw_vals,
                        &mut stmt_fk_suspects,
                    )?;
                }
                let effective_conflict = ins.or_conflict.unwrap_or(if tbl.without_rowid {
                    tbl.without_rowid_pk_conflict
                } else {
                    tbl.rowid_pk_conflict
                });
                ctx.mutation_pipeline.dispatch(MutationContext {
                    db_id,
                    table_name: tbl.name.clone(),
                    origin: MutationOrigin::TopLevelInsert,
                    old_row: None,
                    new_row: Some(MutationRow {
                        rowid: final_key.rowid_or_zero(),
                        values: raw_vals.clone(),
                    }),
                    excluded_row: None,
                    effective_conflict,
                    emit_update_hook: !tbl.without_rowid && !is_catalog,
                    invalidate_blobs: false,
                });
                if !tbl.without_rowid {
                    ctx.runtime.last_insert_rowid = rowid;
                }
                inserted_count += 1;
                ctx.runtime.last_changes += 1;
                ctx.runtime.total_changes += 1;
                ctx.publish();
                Ok(Some((final_key, raw_vals)))
            })()
        };

        let written_opt = match write_res {
            Ok(w) => w,
            Err(e) => {
                if e.conflict_effect == ConflictResolutionEffect::Fail
                    && *ctx.foreign_keys
                    && !stmt_fk_suspects.is_empty()
                {
                    verify_fk_suspects(ctx, &stmt_fk_suspects)?;
                }
                return Err(e);
            }
        };
        if let Some((written_key, written_vals)) = written_opt
            && !ret_expanded.is_empty()
        {
            let rscope =
                make_returning_table_scope(&tbl, written_key.rowid_or_zero(), &written_vals);
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    std::slice::from_ref(&rscope),
                    &dml_ctes,
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    if *ctx.count_changes && ret_col_names.is_empty() {
        Ok(QueryOutput {
            column_names: vec!["count".to_owned()],
            rows: vec![vec![Value::Integer(inserted_count as i64)]],
        })
    } else {
        Ok(QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        })
    }
}

fn check_update_from_same_src_alias(
    upd_db_id: DatabaseId,
    upd_tbl_name: &str,
    upd_alias: Option<&str>,
    fc: &FromClause,
    db_set: &DatabaseSet,
    ctes: &CteStore,
) -> Result<()> {
    for tos in std::iter::once(&fc.head).chain(fc.joins.iter().map(|j| &j.right)) {
        if let TableOrSubquery::Table {
            name,
            alias: item_alias,
            ..
        } = tos
        {
            if name.schema.is_none() && ctes.locate(&name.name).is_some() {
                continue;
            }
            let same_obj = match db_set.find_relation_in_set(name) {
                Ok(Some((item_db_id, ResolvedRelation::Table(t)))) => {
                    item_db_id == upd_db_id && t.name.eq_ignore_ascii_case(upd_tbl_name)
                }
                Ok(Some((item_db_id, ResolvedRelation::View(v)))) => {
                    item_db_id == upd_db_id && v.name.eq_ignore_ascii_case(upd_tbl_name)
                }
                _ => false,
            };
            if same_obj {
                let same_alias = match (upd_alias, item_alias.as_deref()) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                    _ => false,
                };
                if same_alias {
                    let bad = upd_alias.unwrap_or(upd_tbl_name);
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("target object/alias may not appear in FROM clause: {bad}"),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn exec_update(
    ctx: &mut ExecContext<'_>,
    upd: &UpdateStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let (db_id, initial_tbl, _, is_catalog) =
        resolve_dml_target_table_in_set(ctx.db_set, &upd.table, *ctx.writable_schema)?;
    let dml_ctes = CteStore::default().with_scope(&upd.ctes);

    let _ = resolve_set_assignments_for_table(&initial_tbl, &upd.assignments)?;

    // Evaluate optional `FROM` clause BEFORE acquiring write slot (`SF_NestedFrom` — target table not in scope!)
    let from_rows_opt = if let Some(ref fc) = upd.from {
        if 2 + fc.joins.len() > 64 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "at most 64 tables in a join",
            ));
        }
        check_update_from_same_src_alias(
            db_id,
            &initial_tbl.name,
            upd.alias.as_deref(),
            fc,
            ctx.db_set,
            &dml_ctes,
        )?;
        Some(eval_from_clause(ctx, fc, None, params, &[], &dml_ctes)?)
    } else {
        None
    };

    let (tbl, indexes, schema_name) =
        prepare_dml_write_slot(ctx, db_id, &upd.table, is_catalog, initial_tbl)?;
    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    let resolved_assigns = resolve_set_assignments_for_table(&tbl, &upd.assignments)?;

    let plan_where_hint = if upd.from.is_none() || upd.indexed_by.is_some() {
        upd.selection.as_ref()
    } else {
        None
    };
    let candidates = read_table_with_plan(
        ctx,
        db_id,
        &tbl,
        upd.alias.as_deref(),
        upd.indexed_by.as_ref(),
        plan_where_hint,
        params,
        &[],
        &dml_ctes,
    )?;

    let has_set_agg = upd.from.is_some()
        && upd
            .assignments
            .iter()
            .any(|sa| expr_contains_aggregate(&sa.expr));

    // Two-phase update: collect all matching `(old_key, new_rowid_opt, new_col_vals)` before mutating B-Trees!
    let pending_updates: Vec<(TableRowKey, Option<i64>, Vec<Value>)> = if let Some(ref from_rows) =
        from_rows_opt
    {
        if has_set_agg {
            let mut matched_keys: Vec<(TableRowKey, Vec<Value>)> = Vec::new();
            let mut agg_rows: Vec<EvalRow> = Vec::new();
            for (old_key, payload) in candidates {
                let old_vals =
                    decode_table_logical_row(ctx, db_id, &tbl, &old_key, &payload, params)?;
                let target_scope = make_single_table_scope(
                    &tbl,
                    &schema_name,
                    upd.alias.as_deref(),
                    old_key.rowid_or_zero(),
                    &old_vals,
                );
                for from_row in from_rows {
                    ctx.step_tick()?;
                    let mut combined_cols =
                        Vec::with_capacity(target_scope.cols.len() + from_row.cols.len());
                    combined_cols.extend_from_slice(&target_scope.cols);
                    combined_cols.extend_from_slice(&from_row.cols);
                    let mut combined_vals =
                        Vec::with_capacity(target_scope.vals.len() + from_row.vals.len());
                    combined_vals.extend_from_slice(&target_scope.vals);
                    combined_vals.extend_from_slice(&from_row.vals);
                    let combined_scope = EvalRow::new(combined_cols.into(), combined_vals);
                    let scopes = std::slice::from_ref(&combined_scope);

                    if let Some(ref w) = upd.selection {
                        let cond = eval_expr(ctx, w, params, scopes, &dml_ctes, None)?;
                        if cond.truthiness() != Some(true) {
                            continue;
                        }
                    }
                    matched_keys.push((old_key.clone(), old_vals.clone()));
                    agg_rows.push(combined_scope);
                }
            }
            if let Some((first_key, first_vals)) = matched_keys.first() {
                let scopes = std::slice::from_ref(&agg_rows[0]);
                let (new_rowid_opt, new_vals) = apply_resolved_set_assignments_with_agg(
                    ctx,
                    &tbl,
                    first_key,
                    first_vals,
                    &resolved_assigns,
                    scopes,
                    Some(&agg_rows),
                    &dml_ctes,
                    params,
                )?;
                vec![(first_key.clone(), new_rowid_opt, new_vals)]
            } else {
                // Ungrouped aggregate on 0 matching rows evaluates once against an empty group (`rowid = NULL` -> 0 updates)
                let dummy_target =
                    make_static_table_scope(&tbl, &schema_name, upd.alias.as_deref());
                let from_preview = upd
                    .from
                    .as_ref()
                    .and_then(|fc| schema_cols_preview(ctx, fc, params, &[], &dml_ctes))
                    .unwrap_or_default();
                let mut combined_cols =
                    Vec::with_capacity(dummy_target.cols.len() + from_preview.len());
                combined_cols.extend_from_slice(&dummy_target.cols);
                combined_cols.extend(from_preview);
                let n_cols = combined_cols.len();
                let dummy_scope = EvalRow::new(combined_cols.into(), vec![Value::Null; n_cols]);
                let dummy_vals = vec![Value::Null; tbl.columns.len()];
                let _ = apply_resolved_set_assignments_with_agg(
                    ctx,
                    &tbl,
                    &TableRowKey::Rowid(0),
                    &dummy_vals,
                    &resolved_assigns,
                    std::slice::from_ref(&dummy_scope),
                    Some(&[]),
                    &dml_ctes,
                    params,
                )?;
                Vec::new()
            }
        } else {
            let pk_orders: Rc<[KeySortOrder]> = if tbl.without_rowid {
                tbl.without_rowid_pk_orders().into()
            } else {
                Rc::from(&[][..])
            };
            let mut ephemeral: BTreeMap<OrdTableKey, (Option<i64>, Vec<Value>)> = BTreeMap::new();
            for (old_key, payload) in candidates {
                let old_vals =
                    decode_table_logical_row(ctx, db_id, &tbl, &old_key, &payload, params)?;
                let target_scope = make_single_table_scope(
                    &tbl,
                    &schema_name,
                    upd.alias.as_deref(),
                    old_key.rowid_or_zero(),
                    &old_vals,
                );
                for from_row in from_rows {
                    ctx.step_tick()?;
                    let mut combined_cols =
                        Vec::with_capacity(target_scope.cols.len() + from_row.cols.len());
                    combined_cols.extend_from_slice(&target_scope.cols);
                    combined_cols.extend_from_slice(&from_row.cols);
                    let mut combined_vals =
                        Vec::with_capacity(target_scope.vals.len() + from_row.vals.len());
                    combined_vals.extend_from_slice(&target_scope.vals);
                    combined_vals.extend_from_slice(&from_row.vals);
                    let combined_scope = EvalRow::new(combined_cols.into(), combined_vals);
                    let scopes = std::slice::from_ref(&combined_scope);

                    if let Some(ref w) = upd.selection {
                        let cond = eval_expr(ctx, w, params, scopes, &dml_ctes, None)?;
                        if cond.truthiness() != Some(true) {
                            continue;
                        }
                    }

                    let (new_rowid_opt, new_vals) = apply_resolved_set_assignments(
                        ctx,
                        &tbl,
                        &old_key,
                        &old_vals,
                        &resolved_assigns,
                        scopes,
                        &dml_ctes,
                        params,
                    )?;
                    ephemeral.insert(
                        OrdTableKey {
                            key: old_key.clone(),
                            pk_orders: Rc::clone(&pk_orders),
                            enc,
                        },
                        (new_rowid_opt, new_vals),
                    );
                }
            }
            ephemeral
                .into_iter()
                .map(|(ok, (nr, nv))| (ok.key, nr, nv))
                .collect()
        }
    } else {
        let _ = resolved_assigns;
        let resolved_assigns = resolve_set_assignments_for_table(&tbl, &upd.assignments)?;
        let mut out = Vec::new();
        for (old_key, payload) in candidates {
            let old_vals = decode_table_logical_row(ctx, db_id, &tbl, &old_key, &payload, params)?;
            let scope = make_single_table_scope(
                &tbl,
                &schema_name,
                upd.alias.as_deref(),
                old_key.rowid_or_zero(),
                &old_vals,
            );
            let scopes = std::slice::from_ref(&scope);
            if let Some(ref w) = upd.selection {
                let cond = eval_expr(ctx, w, params, scopes, &dml_ctes, None)?;
                if cond.truthiness() != Some(true) {
                    continue;
                }
            }
            let (new_rowid_opt, new_vals) = apply_resolved_set_assignments(
                ctx,
                &tbl,
                &old_key,
                &old_vals,
                &resolved_assigns,
                scopes,
                &dml_ctes,
                params,
            )?;
            out.push((old_key, new_rowid_opt, new_vals));
        }
        out
    };

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if upd.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&upd.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();

    for (old_key, new_rowid_opt, new_vals) in pending_updates {
        let write_res = validate_and_write_row(
            ctx,
            db_id,
            &tbl,
            &indexes,
            new_rowid_opt,
            new_vals,
            upd.or_conflict,
            Some(&old_key),
            params,
            false,
            true,
            &mut stmt_fk_suspects,
            0,
            Some((MutationOrigin::TopLevelUpdate, None)),
            is_catalog,
        );
        let written_opt = match write_res {
            Ok(w) => w,
            Err(e) => {
                if e.conflict_effect == ConflictResolutionEffect::Fail
                    && *ctx.foreign_keys
                    && !stmt_fk_suspects.is_empty()
                {
                    verify_fk_suspects(ctx, &stmt_fk_suspects)?;
                }
                return Err(e);
            }
        };
        if let Some((written_key, written_vals)) = written_opt
            && !ret_expanded.is_empty()
        {
            let rscope =
                make_returning_table_scope(&tbl, written_key.rowid_or_zero(), &written_vals);
            let mut rrow = Vec::with_capacity(ret_expanded.len());
            for (pe, _, _) in &ret_expanded {
                rrow.push(eval_expr(
                    ctx,
                    pe,
                    params,
                    std::slice::from_ref(&rscope),
                    &dml_ctes,
                    None,
                )?);
            }
            ret_rows.push(rrow);
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    Ok(maybe_count_changes_output(
        ctx,
        QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        },
    ))
}

fn exec_delete(
    ctx: &mut ExecContext<'_>,
    del: &DeleteStmt,
    params: &[Value],
) -> Result<QueryOutput> {
    let (db_id, initial_tbl, _, is_catalog) =
        resolve_dml_target_table_in_set(ctx.db_set, &del.table, *ctx.writable_schema)?;
    let (tbl, indexes, schema_name) =
        prepare_dml_write_slot(ctx, db_id, &del.table, is_catalog, initial_tbl)?;
    let dml_ctes = CteStore::default().with_scope(&del.ctes);

    // Fast path (`OP_Clear`) for unconditional `DELETE FROM tbl` without `RETURNING` when no FKs reference `tbl`!
    if !is_catalog
        && del.selection.is_none()
        && del.returning.is_empty()
        && !*ctx.foreign_keys
        && del.indexed_by.is_none()
    {
        let count = if tbl.without_rowid {
            ctx.scan_index_in_db(db_id, tbl.root_pgno, tbl.without_rowid_pk_orders())?
                .len() as u64
        } else {
            ctx.scan_table_in_db(db_id, tbl.root_pgno)?.len() as u64
        };
        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
            bt.clear_tree(tbl.root_pgno)?;
            for idx in &indexes {
                bt.clear_tree(idx.root_pgno)?;
            }
        }
        ctx.runtime.last_changes = count;
        ctx.runtime.total_changes += count;
        ctx.publish();
        return Ok(maybe_count_changes_output(ctx, QueryOutput::default()));
    }

    let candidates = read_table_with_plan(
        ctx,
        db_id,
        &tbl,
        del.alias.as_deref(),
        del.indexed_by.as_ref(),
        del.selection.as_ref(),
        params,
        &[],
        &dml_ctes,
    )?;

    let mut targets = Vec::new();
    for (rkey, payload) in candidates {
        let vals = decode_table_logical_row(ctx, db_id, &tbl, &rkey, &payload, params)?;
        let scope = make_single_table_scope(
            &tbl,
            &schema_name,
            del.alias.as_deref(),
            rkey.rowid_or_zero(),
            &vals,
        );
        if let Some(ref w) = del.selection {
            let cond = eval_expr(
                ctx,
                w,
                params,
                std::slice::from_ref(&scope),
                &dml_ctes,
                None,
            )?;
            if cond.truthiness() != Some(true) {
                continue;
            }
        }
        targets.push((rkey, vals, scope));
    }

    ctx.runtime.last_changes = 0;
    ctx.publish();

    let sample_scope = make_returning_table_scope(&tbl, 0, &vec![Value::Null; tbl.columns.len()]);
    let (ret_col_names, ret_expanded) = if del.returning.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        expand_returning_projection(&del.returning, &sample_scope.cols)?
    };
    let mut ret_rows = Vec::new();
    let mut stmt_fk_suspects = Vec::new();

    for (rkey, old_vals, _where_scope) in targets {
        if *ctx.foreign_keys {
            // Re-check row existence in case a self-referential ON DELETE CASCADE already removed it
            if seek_table_raw_entry(ctx, db_id, &tbl, &rkey)?.is_none() {
                continue;
            }
            check_parent_delete_restrict(ctx, db_id, &tbl, &old_vals, params)?;
        }
        if delete_row_via_pipeline(
            ctx,
            db_id,
            &tbl,
            &indexes,
            &rkey,
            Some(&old_vals),
            MutationOrigin::TopLevelDelete,
            ConflictAction::Abort,
            is_catalog,
            params,
        )? {
            ctx.runtime.last_changes += 1;
            ctx.runtime.total_changes += 1;
            ctx.publish();
            if *ctx.foreign_keys {
                apply_parent_delete_fk_actions(
                    ctx,
                    db_id,
                    &tbl,
                    &old_vals,
                    params,
                    &mut stmt_fk_suspects,
                    0,
                )?;
            }
            if !ret_expanded.is_empty() {
                let rscope = make_returning_table_scope(&tbl, rkey.rowid_or_zero(), &old_vals);
                let mut rrow = Vec::with_capacity(ret_expanded.len());
                for (pe, _, _) in &ret_expanded {
                    rrow.push(eval_expr(
                        ctx,
                        pe,
                        params,
                        std::slice::from_ref(&rscope),
                        &dml_ctes,
                        None,
                    )?);
                }
                ret_rows.push(rrow);
            }
        }
    }

    if *ctx.foreign_keys && !stmt_fk_suspects.is_empty() {
        verify_fk_suspects(ctx, &stmt_fk_suspects)?;
    }

    Ok(maybe_count_changes_output(
        ctx,
        QueryOutput {
            column_names: ret_col_names,
            rows: ret_rows,
        },
    ))
}

// ============================================================================
// DDL Executors: `CREATE/DROP TABLE`, `CREATE/DROP INDEX`, `ALTER TABLE`
// ============================================================================

fn is_reserved_sqlite_name(name: &str) -> bool {
    name.as_bytes()
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sqlite_"))
}

fn is_non_droppable_system_table_name(name: &str) -> bool {
    let Some(rest) = name.as_bytes().get(7..) else {
        return false;
    };
    if !name.as_bytes()[..7].eq_ignore_ascii_case(b"sqlite_") {
        return false;
    }
    if rest
        .get(..4)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"stat"))
    {
        return false;
    }
    if rest
        .get(..10)
        .is_some_and(|p| p.eq_ignore_ascii_case(b"parameters"))
    {
        return false;
    }
    true
}

fn validate_drop_or_alter_schema_table_in_set(
    db_set: &DatabaseSet,
    name: &QualifiedName,
    verb: &str,
) -> Result<()> {
    match name.schema.as_deref() {
        None => {
            if Schema::is_temp_schema_table_name(&name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table sqlite_temp_master may not be {verb}"),
                ));
            }
            if name.name.eq_ignore_ascii_case("sqlite_schema")
                || name.name.eq_ignore_ascii_case("sqlite_master")
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table sqlite_master may not be {verb}"),
                ));
            }
        }
        Some(s) => {
            if let Some(db_id) = db_set.find_db_id(s)
                && DatabaseSet::is_catalog_table_in_db(db_id, &name.name)
            {
                let canonical = if db_id == DatabaseId::TEMP {
                    "sqlite_temp_master"
                } else {
                    "sqlite_master"
                };
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {canonical} may not be {verb}"),
                ));
            }
        }
    }
    Ok(())
}

fn exec_create_table(ctx: &mut ExecContext<'_>, ct: &CreateTableStmt) -> Result<()> {
    let db_id = resolve_create_table_target(ctx.db_set, ct.temp, &ct.name)?;
    if !*ctx.writable_schema && is_reserved_sqlite_name(&ct.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ct.name.name),
        ));
    }
    if db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }
    // Validate AST & inline autoindexes before opening write tx or allocating pages
    let (tbl_preview, pending_autos) = build_table_schema_from_ast(ct, 2)?;

    ctx.ensure_write_db(db_id, true)?;
    {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if slot.schema.find_user_table(&ct.name.name).is_some() {
            if ct.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} already exists", ct.name.name),
            ));
        }
        if slot.schema.find_view(&ct.name.name).is_some() {
            if ct.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("view {} already exists", ct.name.name),
            ));
        }
        if slot.schema.find_index(&ct.name.name).is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("there is already an index named {}", ct.name.name),
            ));
        }
    }

    let slot = ctx
        .db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let need_seq =
        tbl_preview.autoincrement && slot.schema.find_user_table("sqlite_sequence").is_none();
    let bt = slot
        .btree
        .as_mut()
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;

    let tbl_root = if tbl_preview.without_rowid {
        bt.create_index()?
    } else {
        bt.create_table()?
    };
    insert_schema_entry(
        bt,
        "table",
        &ct.name.name,
        &ct.name.name,
        tbl_root,
        Some(&ct.catalog_sql),
    )?;

    for auto in pending_autos {
        let idx_root = bt.create_index()?;
        insert_schema_entry(bt, "index", &auto.name, &ct.name.name, idx_root, None)?;
    }

    // Create `sqlite_sequence` table inside `db_id` if `AUTOINCREMENT` is used and `sqlite_sequence` doesn't exist yet
    if need_seq {
        let seq_root = bt.create_table()?;
        insert_schema_entry(
            bt,
            "table",
            "sqlite_sequence",
            "sqlite_sequence",
            seq_root,
            Some("CREATE TABLE sqlite_sequence(name,seq)"),
        )?;
    }

    if bt.pager().page_count() > slot.max_page_count {
        return Err(SqliteError::new(
            ErrorCode::Full,
            "database or disk is full",
        ));
    }

    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        slot.reload_schema()?;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_create_table_as(
    ctx: &mut ExecContext<'_>,
    temp: bool,
    if_not_exists: bool,
    name: &QualifiedName,
    query: &SelectStmt,
    params: &[Value],
) -> Result<()> {
    let db_id = resolve_create_table_target(ctx.db_set, temp, name)?;
    if !*ctx.writable_schema && is_reserved_sqlite_name(&name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", name.name),
        ));
    }
    if db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }
    let slot_name = {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if slot.schema.find_user_table(&name.name).is_some() {
            if if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} already exists", name.name),
            ));
        }
        if slot.schema.find_view(&name.name).is_some() {
            if if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("view {} already exists", name.name),
            ));
        }
        if slot.schema.find_index(&name.name).is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("there is already an index named {}", name.name),
            ));
        }
        slot.name.clone()
    };

    let meta = infer_select_stmt_meta(query, &[], ctx.db_set, &CteStore::default());
    let q_out = exec_select(ctx, query, params, &[], &CteStore::default())?;

    let mut dedup_names: Vec<String> = Vec::with_capacity(q_out.column_names.len());
    for raw_cn in &q_out.column_names {
        if !dedup_names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(raw_cn))
        {
            dedup_names.push(raw_cn.clone());
        } else {
            let mut k = 1usize;
            loop {
                let candidate = format!("{raw_cn}:{k}");
                if !dedup_names
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&candidate))
                {
                    dedup_names.push(candidate);
                    break;
                }
                k += 1;
            }
        }
    }

    let mut columns = Vec::with_capacity(dedup_names.len());
    let mut rendered_cols = Vec::with_capacity(dedup_names.len());
    for (i, cname) in dedup_names.into_iter().enumerate() {
        let aff = meta.get(i).and_then(|(a, _)| *a);
        let type_str = match aff {
            Some(TypeAffinity::Integer) => "INT",
            Some(TypeAffinity::Text) => "TEXT",
            Some(TypeAffinity::Real) => "REAL",
            Some(TypeAffinity::Numeric) => "NUM",
            Some(TypeAffinity::Blob) | None => "",
        };
        let cdef = ColumnDef {
            name: cname,
            type_name: type_str.to_owned(),
            constraints: Vec::new(),
        };
        rendered_cols.push(render_column_def_sql(&cdef));
        columns.push(cdef);
    }

    let catalog_sql = format!(
        "CREATE TABLE {}({})",
        format_replacement_ident("", &name.name),
        rendered_cols.join(",")
    );
    let ct = CreateTableStmt {
        temp: false,
        if_not_exists,
        name: QualifiedName {
            schema: Some(slot_name),
            name: name.name.clone(),
        },
        columns,
        constraints: Vec::new(),
        without_rowid: false,
        strict: false,
        catalog_sql: catalog_sql.clone(),
        raw_sql: catalog_sql,
    };
    exec_create_table(ctx, &ct)?;

    let Some(tbl) = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.schema.find_user_table(&name.name).cloned())
    else {
        return Ok(());
    };
    let mut stmt_fk_suspects = Vec::new();
    for row in q_out.rows {
        validate_and_write_row(
            ctx,
            db_id,
            &tbl,
            &[],
            None,
            row,
            None,
            None,
            params,
            false,
            false,
            &mut stmt_fk_suspects,
            0,
            None,
            false,
        )?;
    }
    Ok(())
}

fn apply_root_page_move(btree: &mut Btree, schema: &mut Schema, mv: RootPageMove) -> Result<()> {
    if let Some(t) = schema.tables.iter_mut().find(|t| t.root_pgno == mv.from) {
        update_schema_entry(
            btree,
            t.schema_rowid,
            "table",
            &t.name,
            &t.name,
            mv.to,
            Some(&t.raw_sql),
        )?;
        t.root_pgno = mv.to;
        return Ok(());
    }
    if let Some(idx) = schema
        .indexes
        .iter_mut()
        .find(|idx| idx.root_pgno == mv.from)
    {
        update_schema_entry(
            btree,
            idx.schema_rowid,
            "index",
            &idx.name,
            &idx.table_name,
            mv.to,
            idx.raw_sql.as_deref(),
        )?;
        idx.root_pgno = mv.to;
        return Ok(());
    }
    Err(SqliteError::corrupt(format!(
        "relocated root page {} not found in schema catalog",
        mv.from
    )))
}

fn exec_create_view(ctx: &mut ExecContext<'_>, cv: &CreateViewStmt) -> Result<()> {
    let db_id = resolve_create_table_target(ctx.db_set, cv.temp, &cv.name)?;
    if !*ctx.writable_schema && is_reserved_sqlite_name(&cv.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", cv.name.name),
        ));
    }
    validate_view_no_parameters(&cv.query)?;
    if db_id != DatabaseId::TEMP {
        let slot_name = ctx
            .db_set
            .slot(db_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "main".to_owned());
        let _ = fix_permanent_view_select(&cv.name.name, &slot_name, &cv.query)?;
    }
    if db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }

    ctx.ensure_write_db(db_id, true)?;
    {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if slot.schema.find_user_table(&cv.name.name).is_some() {
            if cv.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("table {} already exists", cv.name.name),
            ));
        }
        if slot.schema.find_view(&cv.name.name).is_some() {
            if cv.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("view {} already exists", cv.name.name),
            ));
        }
        if slot.schema.find_index(&cv.name.name).is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("there is already an index named {}", cv.name.name),
            ));
        }
    }

    let slot = ctx
        .db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let bt = slot
        .btree
        .as_mut()
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    insert_schema_entry(
        bt,
        "view",
        &cv.name.name,
        &cv.name.name,
        0,
        Some(&cv.catalog_sql),
    )?;
    if bt.pager().page_count() > slot.max_page_count {
        return Err(SqliteError::new(
            ErrorCode::Full,
            "database or disk is full",
        ));
    }
    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        slot.reload_schema()?;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_create_trigger(ctx: &mut ExecContext<'_>, ct: &CreateTriggerStmt) -> Result<()> {
    let plan = resolve_and_validate_create_trigger(ctx.db_set, ct, *ctx.writable_schema)?;
    let CreateTriggerPlan::Create {
        trigger_db_id,
        target_schema_instance_id,
        target_table_name,
        ..
    } = plan
    else {
        return Ok(());
    };

    if trigger_db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }
    ctx.ensure_write_db(trigger_db_id, true)?;
    {
        let slot = ctx
            .db_set
            .slot(trigger_db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if slot.schema.find_trigger(&ct.name.name).is_some() {
            if ct.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("trigger {} already exists", ct.name_token_text),
            ));
        }
    }

    {
        let slot = ctx
            .db_set
            .slot_mut(trigger_db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        insert_schema_entry(
            bt,
            "trigger",
            &ct.name.name,
            &target_table_name,
            0,
            Some(&ct.catalog_sql),
        )?;
        if bt.pager().page_count() > slot.max_page_count {
            return Err(SqliteError::new(
                ErrorCode::Full,
                "database or disk is full",
            ));
        }
    }

    if trigger_db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
        if let Some(tslot) = ctx.db_set.slot_mut(DatabaseId::TEMP)
            && let Some(tr) = tslot
                .schema
                .triggers
                .iter_mut()
                .rev()
                .find(|t| t.name.eq_ignore_ascii_case(&ct.name.name))
        {
            tr.is_orphan = false;
            tr.target_schema_instance_id = Some(target_schema_instance_id);
            tr.detached_to_temp = false;
        }
        ctx.db_set.sync_temp_trigger_bindings_from_schema();
    } else if let Some(slot) = ctx.db_set.slot_mut(trigger_db_id) {
        slot.reload_schema()?;
    }

    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_drop_trigger(
    ctx: &mut ExecContext<'_>,
    if_exists: bool,
    name: &QualifiedName,
) -> Result<()> {
    let Some((db_id, trig_name)) = ctx
        .db_set
        .find_trigger_in_set(name)
        .map(|(id, tr)| (id, tr.name.clone()))
    else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such trigger: {}", name.display_name()),
        ));
    };

    let has_open_cur = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .is_some_and(|bt| bt.has_any_other_open_cursor(None));
    if ctx.activity.other_readers > 0 || has_open_cur {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }

    ctx.ensure_write_db(db_id, true)?;
    let trig_rowid = {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let Some(tr) = slot.schema.find_trigger(&trig_name) else {
            if if_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such trigger: {}", name.display_name()),
            ));
        };
        tr.schema_rowid
    };

    {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        delete_schema_entry(bt, trig_rowid)?;
    }

    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else if let Some(slot) = ctx.db_set.slot_mut(db_id) {
        slot.reload_schema()?;
    }

    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

/// Deletes all triggers attached to `(db_id, rel_name)` in `db_id` AND any active `TEMP` triggers
/// targeting `(db_id, rel_name)` (`sqlite3CodeDropTable` + `sqlite3TriggerList`, `build.c:3487-3493`).
fn cascade_delete_triggers_for_relation(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    rel_name: &str,
) -> Result<bool> {
    let Some(target_inst) = ctx.db_set.slot(db_id).map(|s| s.schema_instance_id) else {
        return Ok(false);
    };

    let same_db_trig_rowids: Vec<i64> = ctx
        .db_set
        .slot(db_id)
        .map(|s| {
            s.schema
                .triggers
                .iter()
                .filter(|tr| {
                    if db_id == DatabaseId::TEMP {
                        tr.is_active()
                            && !tr.detached_to_temp
                            && tr.target_schema_instance_id == Some(target_inst)
                            && tr.tbl_name.eq_ignore_ascii_case(rel_name)
                    } else {
                        tr.tbl_name.eq_ignore_ascii_case(rel_name)
                    }
                })
                .map(|tr| tr.schema_rowid)
                .collect()
        })
        .unwrap_or_default();

    if !same_db_trig_rowids.is_empty()
        && let Some(slot) = ctx.db_set.slot_mut(db_id)
        && let Some(ref mut bt) = slot.btree
    {
        for rid in same_db_trig_rowids {
            delete_schema_entry(bt, rid)?;
        }
    }

    let mut temp_deleted = false;
    if db_id != DatabaseId::TEMP && ctx.db_set.is_temp_open() {
        let temp_trig_rowids: Vec<i64> = ctx
            .db_set
            .slot(DatabaseId::TEMP)
            .map(|ts| {
                ts.schema
                    .triggers
                    .iter()
                    .filter(|tr| {
                        tr.is_active()
                            && !tr.detached_to_temp
                            && tr.target_schema_instance_id == Some(target_inst)
                            && tr.tbl_name.eq_ignore_ascii_case(rel_name)
                    })
                    .map(|tr| tr.schema_rowid)
                    .collect()
            })
            .unwrap_or_default();
        if !temp_trig_rowids.is_empty() {
            ctx.ensure_write_db(DatabaseId::TEMP, true)?;
            if let Some(tslot) = ctx.db_set.slot_mut(DatabaseId::TEMP)
                && let Some(ref mut tbt) = tslot.btree
            {
                for rid in temp_trig_rowids {
                    delete_schema_entry(tbt, rid)?;
                }
            }
            temp_deleted = true;
        }
    }

    Ok(temp_deleted)
}

fn exec_drop_view(ctx: &mut ExecContext<'_>, if_exists: bool, name: &QualifiedName) -> Result<()> {
    if let Some(ref s) = name.schema
        && ctx.db_set.find_db_id(s).is_none()
    {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown database {s}"),
        ));
    }
    validate_drop_or_alter_schema_table_in_set(ctx.db_set, name, "dropped")?;

    let (db_id, view_name) = match ctx.db_set.find_relation_in_set(name).ok().flatten() {
        Some((_, ResolvedRelation::Table(t))) => {
            if is_non_droppable_system_table_name(&t.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} may not be dropped", t.name),
                ));
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("use DROP TABLE to delete table {}", t.name),
            ));
        }
        Some((id, ResolvedRelation::View(v))) => (id, v.name),
        None => {
            if if_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such view: {}", name.display_name()),
            ));
        }
    };

    let has_open_cur = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .is_some_and(|bt| bt.has_any_other_open_cursor(None));
    if ctx.activity.other_readers > 0 || has_open_cur {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }

    ctx.ensure_write_db(db_id, true)?;
    let view_rowid = {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let Some(view) = slot.schema.find_view(&view_name) else {
            if if_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such view: {}", name.display_name()),
            ));
        };
        view.schema_rowid
    };
    let temp_deleted = cascade_delete_triggers_for_relation(ctx, db_id, &view_name)?;
    {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        delete_schema_entry(bt, view_rowid)?;
    }
    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        if let Some(slot) = ctx.db_set.slot_mut(db_id) {
            slot.reload_schema()?;
        }
        if temp_deleted {
            ctx.db_set.reload_temp_schema_preserving_bindings()?;
        }
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_drop_table(ctx: &mut ExecContext<'_>, if_exists: bool, name: &QualifiedName) -> Result<()> {
    if let Some(ref s) = name.schema
        && ctx.db_set.find_db_id(s).is_none()
    {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown database {s}"),
        ));
    }
    validate_drop_or_alter_schema_table_in_set(ctx.db_set, name, "dropped")?;

    if let Some((_, ResolvedRelation::View(v))) =
        ctx.db_set.find_relation_in_set(name).ok().flatten()
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("use DROP VIEW to delete view {}", v.name),
        ));
    }

    let Some((db_id, tbl_ref)) = ctx.db_set.find_user_table_in_set(name) else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    };
    if is_non_droppable_system_table_name(&tbl_ref.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {} may not be dropped", tbl_ref.name),
        ));
    }
    let tbl_name = tbl_ref.name.clone();

    let has_open_cur = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .is_some_and(|bt| bt.has_any_other_open_cursor(None));
    if ctx.activity.other_readers > 0 || has_open_cur {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }

    ctx.ensure_write_db(db_id, true)?;
    let Some(tbl) = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.schema.find_user_table(&tbl_name).cloned())
    else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", name.display_name()),
        ));
    };

    // Per SQLite `sqlite3FkDropTable`, when `foreign_keys = ON`, delete rows first so FK constraints / actions fire!
    if *ctx.foreign_keys {
        let existing_rows = scan_table_raw_entries(ctx, db_id, &tbl)?;
        if !existing_rows.is_empty() {
            let indexes = ctx
                .db_set
                .slot(db_id)
                .map(|s| s.schema.indexes_for_table(&tbl.name))
                .unwrap_or_default();
            let mut stmt_fk_suspects = Vec::new();
            for (rkey, payload) in existing_rows {
                if seek_table_raw_entry(ctx, db_id, &tbl, &rkey)?.is_none() {
                    continue;
                }
                let old_vals = decode_table_logical_row(ctx, db_id, &tbl, &rkey, &payload, &[])?;
                check_parent_delete_restrict(ctx, db_id, &tbl, &old_vals, &[])?;
                if delete_row_via_pipeline(
                    ctx,
                    db_id,
                    &tbl,
                    &indexes,
                    &rkey,
                    Some(&old_vals),
                    MutationOrigin::FkCascadeDelete,
                    ConflictAction::Abort,
                    false,
                    &[],
                )? {
                    apply_parent_delete_fk_actions(
                        ctx,
                        db_id,
                        &tbl,
                        &old_vals,
                        &[],
                        &mut stmt_fk_suspects,
                        0,
                    )?;
                }
            }
            if !stmt_fk_suspects.is_empty() {
                verify_fk_suspects(ctx, &stmt_fk_suspects)?;
            }
        }
    }

    // Delete all triggers attached to `tbl.name` in `db_id` and `TEMP` (`sqlite3CodeDropTable`)
    let temp_triggers_deleted = cascade_delete_triggers_for_relation(ctx, db_id, &tbl.name)?;

    enum DropRootKind {
        Table,
        Index(String),
    }

    let slot = ctx
        .db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let indexes = slot.schema.indexes_for_table(&tbl.name);
    let mut roots: Vec<(Pgno, i64, DropRootKind)> = Vec::with_capacity(1 + indexes.len());
    roots.push((tbl.root_pgno, tbl.schema_rowid, DropRootKind::Table));
    for idx in indexes {
        if idx.root_pgno != tbl.root_pgno {
            roots.push((
                idx.root_pgno,
                idx.schema_rowid,
                DropRootKind::Index(idx.name),
            ));
        } else {
            if idx.schema_rowid != 0
                && let Some(ref mut bt) = slot.btree
            {
                delete_schema_entry(bt, idx.schema_rowid)?;
            }
            slot.schema
                .indexes
                .retain(|i| !i.name.eq_ignore_ascii_case(&idx.name));
        }
    }
    // Drop roots in strictly descending `root_pgno` order (`sqlite/src/build.c:destroyTable`)!
    roots.sort_by_key(|a| std::cmp::Reverse(a.0));

    for (root_pgno, schema_rowid, kind) in roots {
        let mv = {
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let mv = bt.drop_tree_relocating(root_pgno)?;
            if schema_rowid != 0 {
                delete_schema_entry(bt, schema_rowid)?;
            }
            mv
        };
        match kind {
            DropRootKind::Table => {
                slot.schema
                    .tables
                    .retain(|t| !t.name.eq_ignore_ascii_case(&tbl.name));
            }
            DropRootKind::Index(ref idx_name) => {
                slot.schema
                    .indexes
                    .retain(|i| !i.name.eq_ignore_ascii_case(idx_name));
            }
        }
        if let Some(mv) = mv {
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            apply_root_page_move(bt, &mut slot.schema, mv)?;
        }
    }

    // Also delete entry from `sqlite_sequence` inside `db_id` if present (using its possibly-relocated `root_pgno`!)
    if let Some(seq_tbl) = slot.schema.find_user_table("sqlite_sequence").cloned()
        && let Some(ref mut bt) = slot.btree
    {
        let enc = bt.encoding();
        let rows = scan_table_incremental(bt, seq_tbl.root_pgno)?;
        for (rid, payload) in rows {
            let cols = decode_record_with_encoding(&payload, enc)?;
            if cols.first().and_then(Value::as_str) == Some(tbl.name.as_str()) {
                bt.delete_table(seq_tbl.root_pgno, rid)?;
            }
        }
    }

    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        slot.reload_schema()?;
        if temp_triggers_deleted {
            ctx.db_set.reload_temp_schema_preserving_bindings()?;
        }
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn exec_create_index(ctx: &mut ExecContext<'_>, ci: &CreateIndexStmt) -> Result<()> {
    if !*ctx.writable_schema && is_reserved_sqlite_name(&ci.name.name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("object name reserved for internal use: {}", ci.name.name),
        ));
    }

    let db_id = if let Some(ref s) = ci.name.schema {
        let resolved = ctx.db_set.resolve_schema_or_err(Some(s))?;
        if resolved == DatabaseId::TEMP {
            let in_temp_view = ctx.db_set.is_temp_open()
                && ctx
                    .db_set
                    .slot(DatabaseId::TEMP)
                    .is_some_and(|sl| sl.schema.find_view(&ci.table_name).is_some());
            if in_temp_view {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "views may not be indexed",
                ));
            }
            let in_temp = ctx.db_set.is_temp_open()
                && ctx
                    .db_set
                    .slot(DatabaseId::TEMP)
                    .is_some_and(|sl| sl.schema.find_user_table(&ci.table_name).is_some());
            if !in_temp {
                if let Some((_, rel)) = ctx
                    .db_set
                    .find_relation_in_set(&QualifiedName::unqualified(ci.table_name.clone()))?
                {
                    return match rel {
                        ResolvedRelation::View(_) => Err(SqliteError::new(
                            ErrorCode::Error,
                            "views may not be indexed",
                        )),
                        ResolvedRelation::Table(_) => Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "cannot create a TEMP index on non-TEMP table \"{}\"",
                                ci.table_name
                            ),
                        )),
                    };
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {s}.{}", ci.table_name),
                ));
            }
        } else {
            let is_view = ctx.db_set.slot(resolved).is_some_and(|sl| {
                sl.btree.is_some() && sl.schema.find_view(&ci.table_name).is_some()
            });
            if is_view {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "views may not be indexed",
                ));
            }
            let has_tbl = ctx.db_set.slot(resolved).is_some_and(|sl| {
                sl.btree.is_some() && sl.schema.find_user_table(&ci.table_name).is_some()
            });
            if !has_tbl {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {s}.{}", ci.table_name),
                ));
            }
        }
        resolved
    } else {
        match ctx
            .db_set
            .find_relation_in_set(&QualifiedName::unqualified(ci.table_name.clone()))?
        {
            Some((_, ResolvedRelation::View(_))) => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "views may not be indexed",
                ));
            }
            Some((found_db, ResolvedRelation::Table(_))) => found_db,
            None => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ci.table_name),
                ));
            }
        }
    };

    ctx.ensure_write_db(db_id, true)?;
    let tbl = {
        let slot = ctx
            .db_set
            .slot(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if slot.schema.find_view(&ci.table_name).is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "views may not be indexed",
            ));
        }
        let Some(tbl) = slot.schema.find_user_table(&ci.table_name).cloned() else {
            let disp = match &ci.name.schema {
                Some(s) => format!("{s}.{}", ci.table_name),
                None => ci.table_name.clone(),
            };
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {disp}"),
            ));
        };
        if slot.schema.find_user_table(&ci.name.name).is_some()
            || slot.schema.find_view(&ci.name.name).is_some()
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("there is already a table named {}", ci.name.name),
            ));
        }
        if slot.schema.find_index(&ci.name.name).is_some() {
            if ci.if_not_exists {
                return Ok(());
            }
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("index {} already exists", ci.name.name),
            ));
        }
        tbl
    };

    // Validate AST before allocating pages so invalid expressions don't leak pages
    let _ = build_index_schema_from_ast(ci, &tbl, 2)?;
    let idx_root = ctx
        .db_set
        .slot_mut(db_id)
        .and_then(|s| s.btree.as_mut())
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?
        .create_index()?;
    let idx_schema = build_index_schema_from_ast(ci, &tbl, idx_root)?;
    populate_index_btree(ctx, db_id, &tbl, &idx_schema)?;

    let slot = ctx
        .db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let bt = slot
        .btree
        .as_mut()
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    insert_schema_entry(
        bt,
        "index",
        &ci.name.name,
        &tbl.name,
        idx_root,
        Some(&ci.catalog_sql),
    )?;
    if bt.pager().page_count() > slot.max_page_count {
        return Err(SqliteError::new(
            ErrorCode::Full,
            "database or disk is full",
        ));
    }
    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        slot.reload_schema()?;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn populate_index_btree(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    tbl: &TableSchema,
    idx_schema: &IndexSchema,
) -> Result<()> {
    let (enc, schema_name) = slot_encoding_and_name(ctx, db_id);
    let orders = idx_schema.key_sort_orders();
    let existing_rows = scan_table_raw_entries(ctx, db_id, tbl)?;
    for (rkey, payload) in existing_rows {
        let vals = decode_table_logical_row(ctx, db_id, tbl, &rkey, &payload, &[])?;
        let rowid = rkey.rowid_or_zero();
        if !index_row_passes_partial_where(ctx, &schema_name, tbl, idx_schema, rowid, &vals, &[])? {
            continue;
        }
        let indexed_only =
            eval_index_column_values(ctx, &schema_name, tbl, idx_schema, rowid, &vals, &[])?;
        if idx_schema.unique && indexed_only.iter().all(|v| !v.is_null()) {
            let hits = if let Some(ref mut bt) =
                ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
            {
                bt.seek_index_equal(
                    idx_schema.root_pgno,
                    &indexed_only,
                    &orders[..indexed_only.len()],
                )?
            } else {
                Vec::new()
            };
            if !hits.is_empty() {
                let ext_code = if idx_schema.origin == "pk" {
                    extended::SQLITE_CONSTRAINT_PRIMARYKEY
                } else {
                    extended::SQLITE_CONSTRAINT_UNIQUE
                };
                return Err(SqliteError::constraint(
                    ext_code,
                    format_unique_index_failure(tbl, idx_schema),
                ));
            }
        }
        let keys = eval_index_key_values(ctx, &schema_name, tbl, idx_schema, &rkey, &vals, &[])?;
        let idx_rec = encode_record_with_encoding(&keys, enc);
        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
            bt.insert_index(idx_schema.root_pgno, &idx_rec, &orders)?;
        }
    }
    Ok(())
}

fn exec_reindex(ctx: &mut ExecContext<'_>, target: Option<&QualifiedName>) -> Result<()> {
    let mut primary_without_rowid_tables: Vec<(DatabaseId, TableSchema)> = Vec::new();
    let mut targets: Vec<(DatabaseId, IndexSchema)> = Vec::new();

    match target {
        None => {
            for db_id in ctx.db_set.unqualified_search_order() {
                if let Some(slot) = ctx.db_set.slot(db_id)
                    && slot.btree.is_some()
                {
                    for t in slot.schema.tables.iter().filter(|t| t.without_rowid) {
                        primary_without_rowid_tables.push((db_id, t.clone()));
                    }
                    for idx in &slot.schema.indexes {
                        targets.push((db_id, idx.clone()));
                    }
                }
            }
        }
        Some(qn) => {
            if let Some(ref s) = qn.schema {
                let db_id = ctx.db_set.resolve_schema_or_err(Some(s))?;
                let Some(slot) = ctx.db_set.slot(db_id) else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unable to identify the object to be reindexed",
                    ));
                };
                if slot.btree.is_none() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unable to identify the object to be reindexed",
                    ));
                }
                if let Some(tbl) = slot.schema.find_user_table(&qn.name).cloned() {
                    if tbl.without_rowid {
                        primary_without_rowid_tables.push((db_id, tbl));
                    }
                    for idx in slot.schema.indexes_for_table(&qn.name) {
                        targets.push((db_id, idx));
                    }
                } else if slot.schema.find_view(&qn.name).is_some() {
                    return Ok(());
                } else if let Some(idx) = slot.schema.find_index(&qn.name).cloned() {
                    targets.push((db_id, idx));
                } else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "unable to identify the object to be reindexed",
                    ));
                }
            } else if let Some(coll) = Collation::from_name(&qn.name) {
                for db_id in ctx.db_set.unqualified_search_order() {
                    if let Some(slot) = ctx.db_set.slot(db_id)
                        && slot.btree.is_some()
                    {
                        for t in slot.schema.tables.iter().filter(|t| {
                            t.without_rowid
                                && t.without_rowid_pk_cols
                                    .iter()
                                    .any(|c| c.sort_order.collation == coll)
                        }) {
                            primary_without_rowid_tables.push((db_id, t.clone()));
                        }
                        for idx in slot.schema.indexes.iter().filter(|idx| {
                            idx.columns.iter().any(|c| c.sort_order.collation == coll)
                        }) {
                            targets.push((db_id, idx.clone()));
                        }
                    }
                }
            } else if ctx.collations.has_collation(&qn.name) {
                // Custom collation — indexes using it (none with custom Collation enum yet)
            } else if let Some((db_id, tbl_ref)) = ctx.db_set.find_user_table_in_set(qn) {
                let tbl = tbl_ref.clone();
                let idxs = ctx
                    .db_set
                    .slot(db_id)
                    .map(|s| s.schema.indexes_for_table(&tbl.name))
                    .unwrap_or_default();
                if tbl.without_rowid {
                    primary_without_rowid_tables.push((db_id, tbl));
                }
                for idx in idxs {
                    targets.push((db_id, idx));
                }
            } else if ctx.db_set.find_view_in_set(qn).is_some() {
                return Ok(());
            } else if let Some((db_id, idx_ref)) = ctx.db_set.find_index_in_set(qn) {
                targets.push((db_id, idx_ref.clone()));
            } else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "unable to identify the object to be reindexed",
                ));
            }
        }
    }

    for (db_id, tbl) in primary_without_rowid_tables {
        ctx.ensure_write_db(db_id, true)?;
        let pk_orders = tbl.without_rowid_pk_orders();
        let entries = ctx.scan_index_in_db(db_id, tbl.root_pgno, pk_orders.clone())?;
        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
            bt.clear_tree(tbl.root_pgno)?;
            for rec in entries {
                bt.insert_index(tbl.root_pgno, &rec, &pk_orders)?;
            }
        }
    }

    for (db_id, idx) in targets {
        ctx.ensure_write_db(db_id, true)?;
        let Some(tbl) = ctx
            .db_set
            .slot(db_id)
            .and_then(|s| s.schema.find_user_table(&idx.table_name).cloned())
        else {
            continue;
        };
        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut()) {
            bt.clear_tree(idx.root_pgno)?;
        }
        populate_index_btree(ctx, db_id, &tbl, &idx)?;
    }
    Ok(())
}

fn exec_drop_index(ctx: &mut ExecContext<'_>, if_exists: bool, name: &QualifiedName) -> Result<()> {
    if let Some(ref s) = name.schema
        && ctx.db_set.find_db_id(s).is_none()
    {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("unknown database {s}"),
        ));
    }
    let Some((db_id, idx)) = ctx
        .db_set
        .find_index_including_without_rowid_pk_in_set(name)
    else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such index: {}", name.display_name()),
        ));
    };
    if idx.is_autoindex {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped",
        ));
    }
    let has_open_cur = ctx
        .db_set
        .slot(db_id)
        .and_then(|s| s.btree.as_ref())
        .is_some_and(|bt| bt.has_any_other_open_cursor(None));
    if ctx.activity.other_readers > 0 || has_open_cur {
        return Err(SqliteError::new(
            ErrorCode::Locked,
            "database table is locked",
        ));
    }

    ctx.ensure_write_db(db_id, true)?;
    let slot = ctx
        .db_set
        .slot_mut(db_id)
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    let Some(idx) = slot.schema.find_index_including_without_rowid_pk(&idx.name) else {
        if if_exists {
            return Ok(());
        }
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such index: {}", name.display_name()),
        ));
    };
    let mv = {
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let mv = bt.drop_tree_relocating(idx.root_pgno)?;
        delete_schema_entry(bt, idx.schema_rowid)?;
        mv
    };
    slot.schema
        .indexes
        .retain(|i| !i.name.eq_ignore_ascii_case(&idx.name));
    if let Some(mv) = mv {
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        apply_root_page_move(bt, &mut slot.schema, mv)?;
    }
    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        slot.reload_schema()?;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

fn is_rowid_alias_keyword(name: &str) -> bool {
    name.eq_ignore_ascii_case("rowid")
        || name.eq_ignore_ascii_case("_rowid_")
        || name.eq_ignore_ascii_case("oid")
}

fn check_alter_rowid_alias(tbl: &TableSchema, col_name: &str, verb: &str) -> Result<()> {
    if !tbl.without_rowid && is_rowid_alias_keyword(col_name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("cannot {verb} rowid alias: {col_name}"),
        ));
    }
    Ok(())
}

fn alter_view_rejection_error(view_name: &str, action: &AlterTableAction) -> SqliteError {
    let msg = match action {
        AlterTableAction::RenameTo(_) => format!("view {view_name} may not be altered"),
        AlterTableAction::RenameColumn { .. } => {
            format!("cannot rename columns of view \"{view_name}\"")
        }
        AlterTableAction::DropColumn(_) => format!("cannot drop column from view \"{view_name}\""),
        AlterTableAction::AddColumn(_) => "Cannot add a column to a view".to_owned(),
    };
    SqliteError::new(ErrorCode::Error, msg)
}

fn validate_single_view_definition(
    db_id: DatabaseId,
    view: &ViewSchema,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
) -> Result<()> {
    let mut visited = Vec::new();
    let _ = resolve_view_columns_static(
        db_id,
        view,
        db_set,
        &CteStore::default(),
        functions,
        &mut visited,
    )?;
    Ok(())
}

fn read_live_schema_sql_map(bt: &mut Btree) -> Result<HashMap<i64, Option<String>>> {
    let mut out = HashMap::new();
    if bt.pager().page_count() == 0 {
        return Ok(out);
    }
    let enc = bt.encoding();
    for (rid, payload) in scan_table_incremental(bt, 1)? {
        let cols = decode_record_with_encoding(&payload, enc)?;
        let sql_opt = match cols.get(4) {
            Some(Value::Text(s)) => Some(s.clone()),
            _ => None,
        };
        out.insert(rid, sql_opt);
    }
    Ok(out)
}

fn make_static_trigger_pseudo_scope(
    event: &TriggerEvent,
    col_specs: &[(String, Collation, Option<TypeAffinity>)],
    has_hidden_rowid: bool,
) -> EvalRow {
    let mut pseudo_cols = Vec::new();
    let push_pseudo = |out: &mut Vec<ScopedColumn>, alias: &str| {
        for (cname, coll, aff) in col_specs {
            out.push(ScopedColumn {
                schema_name: None,
                table_alias: Some(alias.to_owned()),
                name: cname.clone(),
                collation: *coll,
                affinity: *aff,
                hidden_rowid: false,
                omit_from_star: false,
                merged_using: false,
            });
        }
        if has_hidden_rowid {
            out.push(ScopedColumn {
                schema_name: None,
                table_alias: Some(alias.to_owned()),
                name: "rowid".to_owned(),
                collation: Collation::Binary,
                affinity: Some(TypeAffinity::Integer),
                hidden_rowid: true,
                omit_from_star: false,
                merged_using: false,
            });
        }
    };
    match event {
        TriggerEvent::Insert => push_pseudo(&mut pseudo_cols, "new"),
        TriggerEvent::Delete => push_pseudo(&mut pseudo_cols, "old"),
        TriggerEvent::Update { .. } => {
            push_pseudo(&mut pseudo_cols, "old");
            push_pseudo(&mut pseudo_cols, "new");
        }
    }
    let n = pseudo_cols.len();
    EvalRow {
        cols: pseudo_cols.into(),
        vals: vec![Value::Null; n],
        qualified_only: true,
    }
}

fn resolve_trigger_step_target_scope_static(
    qn: &QualifiedName,
    alias: Option<&str>,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<(EvalRow, Vec<ScopedColumn>)> {
    let Some((found_db_id, rel)) = db_set.find_relation_in_set(qn)? else {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("no such table: {}", qn.display_name()),
        ));
    };
    let slot_name = db_set.slot(found_db_id).map_or("main", |s| s.name.as_str());
    match rel {
        ResolvedRelation::Table(tbl) => {
            let scope = make_static_table_scope(&tbl, slot_name, alias);
            let mut excl_cols = Vec::with_capacity(tbl.columns.len());
            for c in &tbl.columns {
                excl_cols.push(ScopedColumn {
                    schema_name: None,
                    table_alias: Some("excluded".to_owned()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                });
            }
            Ok((scope, excl_cols))
        }
        ResolvedRelation::View(v) => {
            let eff_alias = alias.unwrap_or(&v.name);
            let (v_names, v_affs, _) = resolve_view_columns_static(
                found_db_id,
                &v,
                db_set,
                &CteStore::default(),
                functions,
                visited_ctes,
            )?;
            let mut scols = Vec::with_capacity(v_names.len());
            let mut excl_cols = Vec::with_capacity(v_names.len());
            for (i, cn) in v_names.into_iter().enumerate() {
                let (aff, coll) = v_affs.get(i).copied().unwrap_or((None, Collation::Binary));
                scols.push(ScopedColumn {
                    schema_name: Some(slot_name.to_owned()),
                    table_alias: Some(eff_alias.to_owned()),
                    name: cn.clone(),
                    collation: coll,
                    affinity: aff,
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                });
                excl_cols.push(ScopedColumn {
                    schema_name: None,
                    table_alias: Some("excluded".to_owned()),
                    name: cn,
                    collation: coll,
                    affinity: aff,
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                });
            }
            let n = scols.len();
            Ok((EvalRow::new(scols.into(), vec![Value::Null; n]), excl_cols))
        }
    }
}

/// Validates a trigger definition from `sqlite_schema` / `sqlite_temp_schema` against `db_set`,
/// matching `renameParseSql` + `renameResolveTrigger` (`sqlite/src/alter.c:1369-1476`).
fn validate_single_trigger_definition(
    trig_db_id: DatabaseId,
    raw_sql: &str,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
) -> Result<()> {
    let (Some(Stmt::CreateTrigger(ct)), _) = parse_one(raw_sql)? else {
        return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
    };

    let (target_db_id, target_rel) = if trig_db_id != DatabaseId::TEMP {
        let slot_name = db_set.slot(trig_db_id).map_or("main", |s| s.name.as_str());
        if let Some(ref explicit_s) = ct.on_table.schema
            && !explicit_s.eq_ignore_ascii_case(slot_name)
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "trigger {} cannot reference objects in database {explicit_s}",
                    ct.name_token_text
                ),
            ));
        }
        let pinned_qn = QualifiedName::qualified(slot_name.to_owned(), ct.on_table.name.clone());
        match db_set.find_relation_in_set(&pinned_qn)? {
            Some(found) => found,
            None => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {slot_name}.{}", ct.on_table.name),
                ));
            }
        }
    } else {
        match db_set.find_relation_in_set(&ct.on_table)? {
            Some(found) => found,
            None => {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ct.on_table.display_name()),
                ));
            }
        }
    };

    let resolved_rel_name = match &target_rel {
        ResolvedRelation::Table(t) => t.name.as_str(),
        ResolvedRelation::View(v) => v.name.as_str(),
    };
    if is_reserved_trigger_or_system_name(resolved_rel_name)
        || is_reserved_trigger_or_system_name(&ct.on_table.name)
    {
        return Err(SqliteError::new(
            ErrorCode::Error,
            "cannot create trigger on system table",
        ));
    }

    let target_disp = if trig_db_id == DatabaseId::TEMP {
        ct.on_table.display_name()
    } else {
        ct.on_table.name.clone()
    };
    match (&target_rel, ct.timing) {
        (ResolvedRelation::View(_), TriggerTiming::Before) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create BEFORE trigger on view: {target_disp}"),
            ));
        }
        (ResolvedRelation::View(_), TriggerTiming::After) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create AFTER trigger on view: {target_disp}"),
            ));
        }
        (ResolvedRelation::Table(_), TriggerTiming::InsteadOf) => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("cannot create INSTEAD OF trigger on table: {target_disp}"),
            ));
        }
        _ => {}
    }

    validate_trigger_body_dml_targets(&ct.body)?;
    let mut when_clause = ct.when_clause;
    let mut body = ct.body;
    replace_bind_params_with_null_in_trigger(&mut when_clause, &mut body);
    if trig_db_id != DatabaseId::TEMP {
        let slot_name = db_set.slot(trig_db_id).map_or("main", |s| s.name.as_str());
        (when_clause, body) =
            fix_permanent_trigger(&ct.name.name, slot_name, when_clause.as_ref(), &body)?;
    }

    let mut visited = Vec::new();
    let (col_specs, has_hidden_rowid) = match &target_rel {
        ResolvedRelation::Table(tbl) => (
            tbl.columns
                .iter()
                .map(|c| (c.name.clone(), c.collation, Some(c.affinity)))
                .collect::<Vec<_>>(),
            !tbl.without_rowid,
        ),
        ResolvedRelation::View(v) => {
            let (v_names, v_affs, _) = resolve_view_columns_static(
                target_db_id,
                v,
                db_set,
                &CteStore::default(),
                functions,
                &mut visited,
            )?;
            let specs = v_names
                .into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = v_affs.get(i).copied().unwrap_or((None, Collation::Binary));
                    (cn, coll, aff)
                })
                .collect::<Vec<_>>();
            (specs, false)
        }
    };
    let pseudo_scope = make_static_trigger_pseudo_scope(&ct.event, &col_specs, has_hidden_rowid);
    let outer_scopes = std::slice::from_ref(&pseudo_scope);

    if let Some(ref w) = when_clause {
        validate_expr_static(
            w,
            outer_scopes,
            db_set,
            &CteStore::default(),
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    for step in &body {
        match step {
            TriggerStep::Select(sel) => {
                let _ = validate_select_stmt_static(
                    sel,
                    outer_scopes,
                    db_set,
                    &CteStore::default(),
                    functions,
                    &mut visited,
                )?;
            }
            TriggerStep::Insert(ins) => {
                match &ins.source {
                    InsertSource::Values(rows) => {
                        for row in rows {
                            for e in row {
                                validate_expr_static(
                                    e,
                                    outer_scopes,
                                    db_set,
                                    &CteStore::default(),
                                    functions,
                                    false,
                                    false,
                                    &mut visited,
                                )?;
                            }
                        }
                    }
                    InsertSource::Select(sel) => {
                        let _ = validate_select_stmt_static(
                            sel,
                            outer_scopes,
                            db_set,
                            &CteStore::default(),
                            functions,
                            &mut visited,
                        )?;
                    }
                    InsertSource::DefaultValues => {}
                }
                let (target_scope, excl_cols) = resolve_trigger_step_target_scope_static(
                    &ins.table,
                    ins.alias.as_deref(),
                    db_set,
                    functions,
                    &mut visited,
                )?;
                if !ins.upsert_clauses.is_empty() {
                    let excl_n = excl_cols.len();
                    let excluded_scope = EvalRow {
                        cols: excl_cols.into(),
                        vals: vec![Value::Null; excl_n],
                        qualified_only: true,
                    };
                    let target_scopes = [pseudo_scope.clone(), target_scope.clone()];
                    let upsert_set_scopes = [pseudo_scope.clone(), target_scope, excluded_scope];
                    for uc in &ins.upsert_clauses {
                        if let Some(ref tgt) = uc.target {
                            for ic in &tgt.columns {
                                if let Some(ref ie) = ic.expr {
                                    validate_expr_static(
                                        ie,
                                        &target_scopes,
                                        db_set,
                                        &CteStore::default(),
                                        functions,
                                        false,
                                        false,
                                        &mut visited,
                                    )?;
                                } else {
                                    let cref = ColumnRef {
                                        schema: None,
                                        table: None,
                                        column: ic.name.clone(),
                                    };
                                    validate_expr_static(
                                        &Expr::Column(cref),
                                        &target_scopes,
                                        db_set,
                                        &CteStore::default(),
                                        functions,
                                        false,
                                        false,
                                        &mut visited,
                                    )?;
                                }
                            }
                            if let Some(ref tw) = tgt.where_clause {
                                validate_expr_static(
                                    tw,
                                    &target_scopes,
                                    db_set,
                                    &CteStore::default(),
                                    functions,
                                    false,
                                    false,
                                    &mut visited,
                                )?;
                            }
                        }
                        if let UpsertAction::DoUpdate {
                            ref assignments,
                            ref where_clause,
                        } = uc.action
                        {
                            for asgn in assignments {
                                let _ = validate_tuple_expr_static(
                                    &asgn.expr,
                                    &upsert_set_scopes,
                                    db_set,
                                    &CteStore::default(),
                                    functions,
                                    false,
                                    false,
                                    &mut visited,
                                )?;
                            }
                            if let Some(uw) = where_clause {
                                validate_expr_static(
                                    uw,
                                    &upsert_set_scopes,
                                    db_set,
                                    &CteStore::default(),
                                    functions,
                                    false,
                                    false,
                                    &mut visited,
                                )?;
                            }
                        }
                    }
                }
            }
            TriggerStep::Update(upd) => {
                let (target_scope, _) = resolve_trigger_step_target_scope_static(
                    &upd.table,
                    upd.alias.as_deref(),
                    db_set,
                    functions,
                    &mut visited,
                )?;
                let step_scope = if let Some(ref fc) = upd.from {
                    let from_cols = validate_from_clause_static(
                        fc,
                        None,
                        outer_scopes,
                        db_set,
                        &CteStore::default(),
                        functions,
                        &mut visited,
                    )?;
                    let mut combined =
                        Vec::with_capacity(target_scope.cols.len() + from_cols.len());
                    combined.extend_from_slice(&target_scope.cols);
                    combined.extend(from_cols);
                    let n = combined.len();
                    EvalRow::new(combined.into(), vec![Value::Null; n])
                } else {
                    target_scope
                };
                let step_scopes = [pseudo_scope.clone(), step_scope];
                if let Some(ref w) = upd.selection {
                    validate_expr_static(
                        w,
                        &step_scopes,
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
                for asgn in &upd.assignments {
                    let _ = validate_tuple_expr_static(
                        &asgn.expr,
                        &step_scopes,
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
            TriggerStep::Delete(del) => {
                let (target_scope, _) = resolve_trigger_step_target_scope_static(
                    &del.table,
                    del.alias.as_deref(),
                    db_set,
                    functions,
                    &mut visited,
                )?;
                let step_scopes = [pseudo_scope.clone(), target_scope];
                if let Some(ref w) = del.selection {
                    validate_expr_static(
                        w,
                        &step_scopes,
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn validate_views_before_alter(
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    target_db_id: DatabaseId,
    legacy_mode: bool,
    writable_schema: bool,
    live_sql_map: &HashMap<i64, Option<String>>,
    live_temp_sql_map: &HashMap<i64, Option<String>>,
) -> Result<()> {
    if writable_schema {
        return Ok(());
    }
    enum AlterCheckObj {
        View(Box<ViewSchema>),
        Trigger(String, String),
    }
    let mut dbs = vec![target_db_id];
    if target_db_id != DatabaseId::TEMP && db_set.is_temp_open() {
        dbs.push(DatabaseId::TEMP);
    }
    for check_db in dbs {
        let Some(slot) = db_set.slot(check_db) else {
            continue;
        };
        if slot.btree.is_none() {
            continue;
        }
        let sql_map = if check_db == target_db_id {
            live_sql_map
        } else {
            live_temp_sql_map
        };
        let mut objs: Vec<(i64, AlterCheckObj)> =
            Vec::with_capacity(slot.schema.views.len() + slot.schema.triggers.len());
        for v in &slot.schema.views {
            objs.push((v.schema_rowid, AlterCheckObj::View(Box::new(v.clone()))));
        }
        for tr in &slot.schema.triggers {
            let sql = sql_map
                .get(&tr.schema_rowid)
                .and_then(|o| o.clone())
                .unwrap_or_else(|| tr.raw_sql.clone());
            objs.push((
                tr.schema_rowid,
                AlterCheckObj::Trigger(tr.name.clone(), sql),
            ));
        }
        objs.sort_by_key(|(rowid, _)| *rowid);

        for (_, obj) in objs {
            match obj {
                AlterCheckObj::View(v) => {
                    if let Err(e) = validate_single_view_definition(check_db, &v, db_set, functions)
                    {
                        if legacy_mode {
                            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
                        }
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("error in view {}: {}", v.name, e.message),
                        ));
                    }
                }
                AlterCheckObj::Trigger(tr_name, tr_sql) => {
                    if let Err(e) =
                        validate_single_trigger_definition(check_db, &tr_sql, db_set, functions)
                    {
                        if legacy_mode {
                            return Err(SqliteError::new(ErrorCode::Error, "SQL logic error"));
                        }
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("error in trigger {tr_name}: {}", e.message),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_views_after_alter(
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    target_db_id: DatabaseId,
    op_label: &str,
) -> Result<()> {
    enum AlterCheckObj {
        View(Box<ViewSchema>),
        Trigger(String, String),
    }
    let mut dbs = vec![target_db_id];
    if target_db_id != DatabaseId::TEMP && db_set.is_temp_open() {
        dbs.push(DatabaseId::TEMP);
    }
    for check_db in dbs {
        let Some(slot) = db_set.slot(check_db) else {
            continue;
        };
        if slot.btree.is_none() {
            continue;
        }
        let mut objs: Vec<(i64, AlterCheckObj)> =
            Vec::with_capacity(slot.schema.views.len() + slot.schema.triggers.len());
        for v in &slot.schema.views {
            objs.push((v.schema_rowid, AlterCheckObj::View(Box::new(v.clone()))));
        }
        for tr in &slot.schema.triggers {
            objs.push((
                tr.schema_rowid,
                AlterCheckObj::Trigger(tr.name.clone(), tr.raw_sql.clone()),
            ));
        }
        objs.sort_by_key(|(rowid, _)| *rowid);

        for (_, obj) in objs {
            match obj {
                AlterCheckObj::View(v) => {
                    if let Err(e) = validate_single_view_definition(check_db, &v, db_set, functions)
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("error in view {} after {op_label}: {}", v.name, e.message),
                        ));
                    }
                }
                AlterCheckObj::Trigger(tr_name, tr_sql) => {
                    if let Err(e) =
                        validate_single_trigger_definition(check_db, &tr_sql, db_set, functions)
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("error in trigger {tr_name} after {op_label}: {}", e.message),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_table_and_indexes_static_after_alter(
    db_id: DatabaseId,
    tbl_name: &str,
    new_ct: &CreateTableStmt,
    explicit_index_sqls: &[(String, Pgno, String)],
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
    op_label: &str,
) -> Result<TableSchema> {
    let (mut new_tbl, _) = build_table_schema_from_ast(new_ct, 2).map_err(|e| {
        SqliteError::new(
            ErrorCode::Error,
            format!("error in table {tbl_name} after {op_label}: {}", e.message),
        )
    })?;
    new_tbl.name = tbl_name.to_owned();
    let slot_name = db_set.slot(db_id).map_or("main", |s| s.name.as_str());
    let tbl_scope = make_static_table_scope(&new_tbl, slot_name, None);
    let mut visited = Vec::new();
    for col in &new_tbl.columns {
        if let Some(ref de) = col.default_expr {
            validate_expr_static(
                de,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )
            .map_err(|e| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {tbl_name} after {op_label}: {}", e.message),
                )
            })?;
        }
        if let Some((ref ge, _)) = col.generated {
            validate_expr_static(
                ge,
                std::slice::from_ref(&tbl_scope),
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )
            .map_err(|e| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {tbl_name} after {op_label}: {}", e.message),
                )
            })?;
        }
    }
    for chk in &new_tbl.checks {
        validate_expr_static(
            &chk.expr,
            std::slice::from_ref(&tbl_scope),
            db_set,
            &CteStore::default(),
            functions,
            false,
            false,
            &mut visited,
        )
        .map_err(|e| {
            SqliteError::new(
                ErrorCode::Error,
                format!("error in table {tbl_name} after {op_label}: {}", e.message),
            )
        })?;
    }

    for (idx_name, idx_root, idx_sql) in explicit_index_sqls {
        let (Some(Stmt::CreateIndex(ci)), _) = parse_one(idx_sql).map_err(|e| {
            SqliteError::new(
                ErrorCode::Error,
                format!("error in index {idx_name} after {op_label}: {}", e.message),
            )
        })?
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("error in index {idx_name} after {op_label}"),
            ));
        };
        let idx_preview = build_index_schema_from_ast(&ci, &new_tbl, *idx_root).map_err(|e| {
            SqliteError::new(
                ErrorCode::Error,
                format!("error in index {idx_name} after {op_label}: {}", e.message),
            )
        })?;
        for ic in &idx_preview.columns {
            if let Some(ref ie) = ic.expr {
                validate_expr_static(
                    ie,
                    std::slice::from_ref(&tbl_scope),
                    db_set,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )
                .map_err(|e| {
                    SqliteError::new(
                        ErrorCode::Error,
                        format!("error in index {idx_name} after {op_label}: {}", e.message),
                    )
                })?;
            }
        }
        if let Some(ref wc) = idx_preview.where_clause {
            validate_expr_static(
                wc,
                std::slice::from_ref(&tbl_scope),
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )
            .map_err(|e| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!("error in index {idx_name} after {op_label}: {}", e.message),
                )
            })?;
        }
    }
    Ok(new_tbl)
}

fn rewrite_physical_rows_for_drop_column(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    old_tbl: &TableSchema,
    new_tbl: &TableSchema,
    dropped_col_idx: usize,
) -> Result<()> {
    let (enc, _) = slot_encoding_and_name(ctx, db_id);
    let existing_rows = scan_table_raw_entries(ctx, db_id, old_tbl)?;
    if existing_rows.is_empty() {
        return Ok(());
    }

    if old_tbl.without_rowid {
        let mut new_records = Vec::with_capacity(existing_rows.len());
        for (_, payload) in existing_rows {
            let phys_vals = decode_record_with_encoding(&payload, enc)?;
            let mut old_logical = vec![Value::Null; old_tbl.columns.len()];
            let mut p_idx = 0usize;
            for ic in &old_tbl.without_rowid_pk_cols {
                let ci = ic.col_idx.expect("WITHOUT ROWID PK col has col_idx");
                if p_idx < phys_vals.len() {
                    old_logical[ci] = phys_vals[p_idx].clone();
                    p_idx += 1;
                }
            }
            for &ci in &old_tbl.without_rowid_extra_cols {
                let v = if p_idx < phys_vals.len() {
                    let pv = phys_vals[p_idx].clone();
                    p_idx += 1;
                    pv
                } else {
                    let col = &old_tbl.columns[ci];
                    let def_val = match &col.default_expr {
                        Some(de) => eval_expr(ctx, de, &[], &[], &CteStore::default(), None)?,
                        None => Value::Null,
                    };
                    let mut coerced = col.affinity.apply(def_val);
                    if old_tbl.strict
                        && col.type_name.trim().eq_ignore_ascii_case("REAL")
                        && let Value::Integer(iv) = coerced
                    {
                        coerced = Value::real(iv as f64);
                    }
                    coerced
                };
                old_logical[ci] = v;
            }
            old_logical.remove(dropped_col_idx);
            new_records.push(encode_table_physical_record(new_tbl, &old_logical, enc));
        }
        let pk_orders = new_tbl.without_rowid_pk_orders();
        let bt = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        bt.clear_tree(new_tbl.root_pgno)?;
        for rec in new_records {
            bt.insert_index(new_tbl.root_pgno, &rec, &pk_orders)?;
        }
        return Ok(());
    }

    let mut rewritten: Vec<(i64, Vec<u8>)> = Vec::with_capacity(existing_rows.len());
    for (rkey, payload) in existing_rows {
        let rid = rkey.rowid_or_zero();
        let phys_vals = decode_record_with_encoding(&payload, enc)?;
        let mut old_logical = Vec::with_capacity(old_tbl.columns.len());
        let mut p_idx = 0usize;
        for (ci, col) in old_tbl.columns.iter().enumerate() {
            let is_virt = col.generated.as_ref().is_some_and(|(_, stored)| !*stored);
            if is_virt {
                old_logical.push(Value::Null);
            } else if old_tbl.rowid_alias_col == Some(ci) {
                p_idx += 1;
                old_logical.push(Value::Null);
            } else if p_idx < phys_vals.len() {
                old_logical.push(phys_vals[p_idx].clone());
                p_idx += 1;
            } else {
                let def_val = match &col.default_expr {
                    Some(de) => eval_expr(ctx, de, &[], &[], &CteStore::default(), None)?,
                    None => Value::Null,
                };
                let mut coerced = col.affinity.apply(def_val);
                if old_tbl.strict
                    && col.type_name.trim().eq_ignore_ascii_case("REAL")
                    && let Value::Integer(iv) = coerced
                {
                    coerced = Value::real(iv as f64);
                }
                old_logical.push(coerced);
            }
        }
        old_logical.remove(dropped_col_idx);
        let new_rec = encode_table_physical_record(new_tbl, &old_logical, enc);
        rewritten.push((rid, new_rec));
    }

    let bt = ctx
        .db_set
        .slot_mut(db_id)
        .and_then(|s| s.btree.as_mut())
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    for (rid, new_rec) in rewritten {
        bt.insert_table(new_tbl.root_pgno, rid, &new_rec)?;
    }
    Ok(())
}

fn exec_alter_table(
    ctx: &mut ExecContext<'_>,
    table: &QualifiedName,
    action: &AlterTableAction,
) -> Result<()> {
    validate_drop_or_alter_schema_table_in_set(ctx.db_set, table, "altered")?;
    let (db_id, orig_tbl_name) = match ctx.db_set.find_relation_in_set(table)? {
        Some((_, ResolvedRelation::View(v))) => {
            return Err(alter_view_rejection_error(&v.name, action));
        }
        Some((id, ResolvedRelation::Table(t))) => (id, t.name),
        None => {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", table.display_name()),
            ));
        }
    };
    if is_reserved_sqlite_name(&orig_tbl_name) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("table {orig_tbl_name} may not be altered"),
        ));
    }

    ctx.ensure_write_db(db_id, true)?;
    let (tbl, schema_name, live_sql_map) = {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if let Some(v) = slot.schema.find_view(&orig_tbl_name) {
            return Err(alter_view_rejection_error(&v.name, action));
        }
        let Some(tbl) = slot.schema.find_user_table(&orig_tbl_name).cloned() else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such table: {}", table.display_name()),
            ));
        };
        let sname = slot.name.clone();
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        let live_map = read_live_schema_sql_map(bt)?;
        (tbl, sname, live_map)
    };
    let live_temp_sql_map = if db_id != DatabaseId::TEMP && ctx.db_set.is_temp_open() {
        if let Some(tslot) = ctx.db_set.slot_mut(DatabaseId::TEMP)
            && let Some(ref mut tbt) = tslot.btree
        {
            read_live_schema_sql_map(tbt)?
        } else {
            HashMap::new()
        }
    } else {
        HashMap::new()
    };
    let live_tbl_sql: &str = live_sql_map
        .get(&tbl.schema_rowid)
        .map(|o| o.as_deref().unwrap_or(""))
        .unwrap_or(&tbl.raw_sql);

    let legacy_mode = *ctx.legacy_alter_table;
    let writable_schema = *ctx.writable_schema;
    let fk_enabled = *ctx.foreign_keys;

    match action {
        AlterTableAction::RenameTo(new_name) => {
            if !writable_schema && is_reserved_sqlite_name(new_name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {new_name}"),
                ));
            }
            {
                let slot = ctx
                    .db_set
                    .slot(db_id)
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                if slot.schema.find_user_table(new_name).is_some()
                    || slot.schema.find_view(new_name).is_some()
                    || slot.schema.find_index(new_name).is_some()
                {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "there is already another table or index with this name: {new_name}"
                        ),
                    ));
                }
            }

            if !legacy_mode {
                validate_views_before_alter(
                    ctx.db_set,
                    ctx.functions,
                    db_id,
                    false,
                    writable_schema,
                    &live_sql_map,
                    &live_temp_sql_map,
                )?;
            }

            let rewrite_fks = !legacy_mode || fk_enabled;
            let rewrite_self_exprs = !legacy_mode;

            // Compute view and trigger SQL edits BEFORE mutating `db_id`'s schema so unqualified resolution in `temp` sees pre-rename bindings!
            let mut same_db_view_edits: Vec<(i64, String, String)> = Vec::new();
            let mut temp_view_edits: Vec<(i64, String, String)> = Vec::new();
            let mut same_db_trigger_edits: Vec<(i64, String, String, String)> = Vec::new();
            let mut temp_trigger_edits: Vec<(i64, String, String, String)> = Vec::new();
            if !legacy_mode {
                if let Some(slot) = ctx.db_set.slot(db_id) {
                    for v in &slot.schema.views {
                        let v_sql = live_sql_map
                            .get(&v.schema_rowid)
                            .and_then(|o| o.as_deref())
                            .unwrap_or(&v.raw_sql);
                        match rewrite_view_for_table_rename(
                            v_sql, db_id, db_id, &tbl.name, new_name, ctx.db_set,
                        ) {
                            Ok(Some(updated)) => {
                                same_db_view_edits.push((v.schema_rowid, v.name.clone(), updated));
                            }
                            Ok(None) => {}
                            Err(_) if writable_schema => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
                if db_id != DatabaseId::TEMP
                    && ctx.db_set.is_temp_open()
                    && let Some(tslot) = ctx.db_set.slot(DatabaseId::TEMP)
                {
                    for v in &tslot.schema.views {
                        let v_sql = live_temp_sql_map
                            .get(&v.schema_rowid)
                            .and_then(|o| o.as_deref())
                            .unwrap_or(&v.raw_sql);
                        match rewrite_view_for_table_rename(
                            v_sql,
                            DatabaseId::TEMP,
                            db_id,
                            &tbl.name,
                            new_name,
                            ctx.db_set,
                        ) {
                            Ok(Some(updated)) => {
                                temp_view_edits.push((v.schema_rowid, v.name.clone(), updated));
                            }
                            Ok(None) => {}
                            Err(_) if writable_schema => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }

            // Triggers: `ON <old_tbl>` and `tbl_name` are updated EVEN when `legacy_alter_table = ON`,
            // while trigger body references (`WHEN`, `SELECT`, DML targets, `FROM`) are rewritten ONLY when `!legacy_mode` (`alter.c:1880-1903`)!
            if let Some(slot) = ctx.db_set.slot(db_id) {
                for tr in &slot.schema.triggers {
                    let tr_sql = live_sql_map
                        .get(&tr.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&tr.raw_sql);
                    match rewrite_trigger_for_table_rename(
                        tr_sql,
                        db_id,
                        db_id,
                        &tbl.name,
                        new_name,
                        legacy_mode,
                        ctx.db_set,
                    ) {
                        Ok((updated_opt, is_on_target)) => {
                            let new_tbl_col =
                                if is_on_target || tr.tbl_name.eq_ignore_ascii_case(&tbl.name) {
                                    new_name.clone()
                                } else {
                                    tr.tbl_name.clone()
                                };
                            if updated_opt.is_some()
                                || !new_tbl_col.eq_ignore_ascii_case(&tr.tbl_name)
                                || new_tbl_col != tr.tbl_name
                            {
                                let final_sql = updated_opt.unwrap_or_else(|| tr_sql.to_owned());
                                same_db_trigger_edits.push((
                                    tr.schema_rowid,
                                    tr.name.clone(),
                                    new_tbl_col,
                                    final_sql,
                                ));
                            }
                        }
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            if db_id != DatabaseId::TEMP
                && ctx.db_set.is_temp_open()
                && let Some(tslot) = ctx.db_set.slot(DatabaseId::TEMP)
            {
                for tr in &tslot.schema.triggers {
                    let tr_sql = live_temp_sql_map
                        .get(&tr.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&tr.raw_sql);
                    match rewrite_trigger_for_table_rename(
                        tr_sql,
                        DatabaseId::TEMP,
                        db_id,
                        &tbl.name,
                        new_name,
                        legacy_mode,
                        ctx.db_set,
                    ) {
                        Ok((updated_opt, is_on_target)) => {
                            let new_tbl_col = if is_on_target {
                                new_name.clone()
                            } else {
                                tr.tbl_name.clone()
                            };
                            if updated_opt.is_some() || new_tbl_col != tr.tbl_name {
                                let final_sql = updated_opt.unwrap_or_else(|| tr_sql.to_owned());
                                temp_trigger_edits.push((
                                    tr.schema_rowid,
                                    tr.name.clone(),
                                    new_tbl_col,
                                    final_sql,
                                ));
                            }
                        }
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
            }

            let new_sql = rewrite_table_name_in_create_table_sql(
                live_tbl_sql,
                &schema_name,
                &tbl.name,
                new_name,
                rewrite_fks,
                rewrite_self_exprs,
            )?;

            let mut updated_explicit_indexes: Vec<(String, Pgno, String)> = Vec::new();
            {
                let slot = ctx
                    .db_set
                    .slot_mut(db_id)
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                let indexes = slot.schema.indexes_for_table(&tbl.name);
                let other_tables: Vec<TableSchema> = slot
                    .schema
                    .tables
                    .iter()
                    .filter(|t| !t.name.eq_ignore_ascii_case(&tbl.name))
                    .cloned()
                    .collect();
                let seq_tbl_opt = slot.schema.find_user_table("sqlite_sequence").cloned();
                let bt = slot
                    .btree
                    .as_mut()
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;

                update_schema_entry(
                    bt,
                    tbl.schema_rowid,
                    "table",
                    new_name,
                    new_name,
                    tbl.root_pgno,
                    Some(&new_sql),
                )?;
                let mut auto_n = 0usize;
                for idx in indexes {
                    if idx.is_autoindex {
                        auto_n += 1;
                        let suffix_num = idx
                            .name
                            .rsplit('_')
                            .next()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(auto_n);
                        let new_auto = format!("sqlite_autoindex_{new_name}_{suffix_num}");
                        update_schema_entry(
                            bt,
                            idx.schema_rowid,
                            "index",
                            &new_auto,
                            new_name,
                            idx.root_pgno,
                            None,
                        )?;
                    } else {
                        let live_idx_sql = live_sql_map
                            .get(&idx.schema_rowid)
                            .and_then(|o| o.as_deref())
                            .or(idx.raw_sql.as_deref());
                        let updated_idx_sql = match live_idx_sql {
                            Some(s) => {
                                let u = rewrite_table_name_in_create_index_sql(
                                    s,
                                    &schema_name,
                                    &tbl.name,
                                    new_name,
                                    rewrite_self_exprs,
                                )?;
                                updated_explicit_indexes.push((
                                    idx.name.clone(),
                                    idx.root_pgno,
                                    u.clone(),
                                ));
                                Some(u)
                            }
                            None => None,
                        };
                        update_schema_entry(
                            bt,
                            idx.schema_rowid,
                            "index",
                            &idx.name,
                            new_name,
                            idx.root_pgno,
                            updated_idx_sql.as_deref(),
                        )?;
                    }
                }
                if rewrite_fks {
                    for otbl in other_tables {
                        let otbl_sql = live_sql_map
                            .get(&otbl.schema_rowid)
                            .and_then(|o| o.as_deref())
                            .unwrap_or(&otbl.raw_sql);
                        if let Some(updated_other_sql) =
                            rewrite_fk_ref_table_in_create_table_sql(otbl_sql, &tbl.name, new_name)?
                        {
                            update_schema_entry(
                                bt,
                                otbl.schema_rowid,
                                "table",
                                &otbl.name,
                                &otbl.name,
                                otbl.root_pgno,
                                Some(&updated_other_sql),
                            )?;
                        }
                    }
                }
                for (v_rowid, v_name, v_sql) in &same_db_view_edits {
                    update_schema_entry(bt, *v_rowid, "view", v_name, v_name, 0, Some(v_sql))?;
                }
                for (tr_rowid, tr_name, tr_tbl, tr_sql) in &same_db_trigger_edits {
                    update_schema_entry(
                        bt,
                        *tr_rowid,
                        "trigger",
                        tr_name,
                        tr_tbl,
                        0,
                        Some(tr_sql),
                    )?;
                }
                if let Some(seq_tbl) = seq_tbl_opt {
                    let enc = bt.encoding();
                    let rows = scan_table_incremental(bt, seq_tbl.root_pgno)?;
                    for (rid, payload) in rows {
                        let cols = decode_record_with_encoding(&payload, enc)?;
                        if cols.len() >= 2 && cols[0].as_str() == Some(tbl.name.as_str()) {
                            let rec = encode_record_with_encoding(
                                &[Value::Text(new_name.clone()), cols[1].clone()],
                                enc,
                            );
                            bt.insert_table(seq_tbl.root_pgno, rid, &rec)?;
                        }
                    }
                }
                slot.reload_schema()?;
            }

            if !temp_view_edits.is_empty() || !temp_trigger_edits.is_empty() {
                ctx.ensure_write_db(DatabaseId::TEMP, true)?;
                let tslot = ctx
                    .db_set
                    .slot_mut(DatabaseId::TEMP)
                    .ok_or_else(|| SqliteError::misuse("temp database slot closed"))?;
                {
                    let tbt = tslot
                        .btree
                        .as_mut()
                        .ok_or_else(|| SqliteError::misuse("temp database slot closed"))?;
                    for (v_rowid, v_name, v_sql) in &temp_view_edits {
                        update_schema_entry(tbt, *v_rowid, "view", v_name, v_name, 0, Some(v_sql))?;
                    }
                    for (tr_rowid, tr_name, tr_tbl, tr_sql) in &temp_trigger_edits {
                        update_schema_entry(
                            tbt,
                            *tr_rowid,
                            "trigger",
                            tr_name,
                            tr_tbl,
                            0,
                            Some(tr_sql),
                        )?;
                    }
                }
                tslot.reload_schema()?;
            }
            ctx.db_set.reconcile_temp_triggers()?;

            *ctx.schema_generation += 1;
            *ctx.v1_expire_generation += 1;
            *ctx.schema_changed_in_tx = true;

            if !writable_schema {
                let (Some(Stmt::CreateTable(new_ct)), _) = parse_one(&new_sql)? else {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("error in table {new_name} after rename"),
                    ));
                };
                let _ = validate_table_and_indexes_static_after_alter(
                    db_id,
                    new_name,
                    &new_ct,
                    &updated_explicit_indexes,
                    ctx.db_set,
                    ctx.functions,
                    "rename",
                )?;
                if !legacy_mode {
                    validate_views_after_alter(ctx.db_set, ctx.functions, db_id, "rename")?;
                }
            }
            return Ok(());
        }
        AlterTableAction::RenameColumn { old_name, new_name } => {
            if tbl.find_column(old_name).is_none() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: \"{old_name}\""),
                ));
            }
            check_alter_rowid_alias(&tbl, old_name, "rename from")?;
            check_alter_rowid_alias(&tbl, new_name, "rename to")?;
            if !old_name.eq_ignore_ascii_case(new_name) && tbl.find_column(new_name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {new_name}"),
                ));
            }

            // Both `renameTestSchema` (`!legacy_mode`) and `renameFixQuotes` (`legacy_mode`) run before `RENAME COLUMN`.
            validate_views_before_alter(
                ctx.db_set,
                ctx.functions,
                db_id,
                legacy_mode,
                writable_schema,
                &live_sql_map,
                &live_temp_sql_map,
            )?;

            // `RENAME COLUMN` ALWAYS structurally rewrites dependent views and triggers in `db_id` and `temp` (`alter.c`)!
            let mut same_db_view_edits: Vec<(i64, String, String)> = Vec::new();
            let mut temp_view_edits: Vec<(i64, String, String)> = Vec::new();
            let mut same_db_trigger_edits: Vec<(i64, String, String, String)> = Vec::new();
            let mut temp_trigger_edits: Vec<(i64, String, String, String)> = Vec::new();
            if let Some(slot) = ctx.db_set.slot(db_id) {
                for v in &slot.schema.views {
                    let v_sql = live_sql_map
                        .get(&v.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&v.raw_sql);
                    match rewrite_view_for_column_rename(
                        v_sql, db_id, db_id, &tbl, old_name, new_name, ctx.db_set,
                    ) {
                        Ok(Some(updated)) => {
                            same_db_view_edits.push((v.schema_rowid, v.name.clone(), updated));
                        }
                        Ok(None) => {}
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
                for tr in &slot.schema.triggers {
                    let tr_sql = live_sql_map
                        .get(&tr.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&tr.raw_sql);
                    match rewrite_trigger_for_column_rename(
                        tr_sql, db_id, db_id, &tbl, old_name, new_name, ctx.db_set,
                    ) {
                        Ok(Some(updated)) => {
                            same_db_trigger_edits.push((
                                tr.schema_rowid,
                                tr.name.clone(),
                                tr.tbl_name.clone(),
                                updated,
                            ));
                        }
                        Ok(None) => {}
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            if db_id != DatabaseId::TEMP
                && ctx.db_set.is_temp_open()
                && let Some(tslot) = ctx.db_set.slot(DatabaseId::TEMP)
            {
                for v in &tslot.schema.views {
                    let v_sql = live_temp_sql_map
                        .get(&v.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&v.raw_sql);
                    match rewrite_view_for_column_rename(
                        v_sql,
                        DatabaseId::TEMP,
                        db_id,
                        &tbl,
                        old_name,
                        new_name,
                        ctx.db_set,
                    ) {
                        Ok(Some(updated)) => {
                            temp_view_edits.push((v.schema_rowid, v.name.clone(), updated));
                        }
                        Ok(None) => {}
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
                for tr in &tslot.schema.triggers {
                    let tr_sql = live_temp_sql_map
                        .get(&tr.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&tr.raw_sql);
                    match rewrite_trigger_for_column_rename(
                        tr_sql,
                        DatabaseId::TEMP,
                        db_id,
                        &tbl,
                        old_name,
                        new_name,
                        ctx.db_set,
                    ) {
                        Ok(Some(updated)) => {
                            temp_trigger_edits.push((
                                tr.schema_rowid,
                                tr.name.clone(),
                                tr.tbl_name.clone(),
                                updated,
                            ));
                        }
                        Ok(None) => {}
                        Err(_) if writable_schema => {}
                        Err(e) => return Err(e),
                    }
                }
            }

            let new_sql =
                rewrite_column_in_create_table_sql(live_tbl_sql, &tbl.name, old_name, new_name)?;
            let (Some(Stmt::CreateTable(new_ct)), _) = parse_one(&new_sql)? else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {} after rename", tbl.name),
                ));
            };

            let mut updated_explicit_indexes: Vec<(String, Pgno, String)> = Vec::new();
            {
                let slot = ctx
                    .db_set
                    .slot_mut(db_id)
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                let indexes = slot.schema.indexes_for_table(&tbl.name);
                let other_tables: Vec<TableSchema> = slot
                    .schema
                    .tables
                    .iter()
                    .filter(|t| !t.name.eq_ignore_ascii_case(&tbl.name))
                    .cloned()
                    .collect();
                let bt = slot
                    .btree
                    .as_mut()
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;

                update_schema_entry(
                    bt,
                    tbl.schema_rowid,
                    "table",
                    &tbl.name,
                    &tbl.name,
                    tbl.root_pgno,
                    Some(&new_sql),
                )?;

                for idx in indexes {
                    if idx.is_autoindex {
                        continue;
                    }
                    let live_idx_sql = live_sql_map
                        .get(&idx.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .or(idx.raw_sql.as_deref());
                    if let Some(idx_sql) = live_idx_sql {
                        let new_idx_sql = rewrite_column_in_create_index_sql(
                            idx_sql, &tbl.name, old_name, new_name,
                        )?;
                        updated_explicit_indexes.push((
                            idx.name.clone(),
                            idx.root_pgno,
                            new_idx_sql.clone(),
                        ));
                        if new_idx_sql != idx_sql {
                            let (Some(Stmt::CreateIndex(_)), _) = parse_one(&new_idx_sql)? else {
                                return Err(SqliteError::new(
                                    ErrorCode::Error,
                                    format!("error in index {} after rename", idx.name),
                                ));
                            };
                            update_schema_entry(
                                bt,
                                idx.schema_rowid,
                                "index",
                                &idx.name,
                                &tbl.name,
                                idx.root_pgno,
                                Some(&new_idx_sql),
                            )?;
                        }
                    }
                }

                for otbl in other_tables {
                    let otbl_sql = live_sql_map
                        .get(&otbl.schema_rowid)
                        .and_then(|o| o.as_deref())
                        .unwrap_or(&otbl.raw_sql);
                    if let Some(updated_child_sql) = rewrite_fk_ref_column_in_create_table_sql(
                        otbl_sql, &tbl.name, old_name, new_name,
                    )? {
                        update_schema_entry(
                            bt,
                            otbl.schema_rowid,
                            "table",
                            &otbl.name,
                            &otbl.name,
                            otbl.root_pgno,
                            Some(&updated_child_sql),
                        )?;
                    }
                }

                for (v_rowid, v_name, v_sql) in &same_db_view_edits {
                    update_schema_entry(bt, *v_rowid, "view", v_name, v_name, 0, Some(v_sql))?;
                }
                for (tr_rowid, tr_name, tr_tbl, tr_sql) in &same_db_trigger_edits {
                    update_schema_entry(
                        bt,
                        *tr_rowid,
                        "trigger",
                        tr_name,
                        tr_tbl,
                        0,
                        Some(tr_sql),
                    )?;
                }
                slot.reload_schema()?;
            }

            if !temp_view_edits.is_empty() || !temp_trigger_edits.is_empty() {
                ctx.ensure_write_db(DatabaseId::TEMP, true)?;
                let tslot = ctx
                    .db_set
                    .slot_mut(DatabaseId::TEMP)
                    .ok_or_else(|| SqliteError::misuse("temp database slot closed"))?;
                {
                    let tbt = tslot
                        .btree
                        .as_mut()
                        .ok_or_else(|| SqliteError::misuse("temp database slot closed"))?;
                    for (v_rowid, v_name, v_sql) in &temp_view_edits {
                        update_schema_entry(tbt, *v_rowid, "view", v_name, v_name, 0, Some(v_sql))?;
                    }
                    for (tr_rowid, tr_name, tr_tbl, tr_sql) in &temp_trigger_edits {
                        update_schema_entry(
                            tbt,
                            *tr_rowid,
                            "trigger",
                            tr_name,
                            tr_tbl,
                            0,
                            Some(tr_sql),
                        )?;
                    }
                }
                tslot.reload_schema()?;
            }
            ctx.db_set.reconcile_temp_triggers()?;

            *ctx.schema_generation += 1;
            *ctx.v1_expire_generation += 1;
            *ctx.schema_changed_in_tx = true;

            if !writable_schema {
                let _ = validate_table_and_indexes_static_after_alter(
                    db_id,
                    &tbl.name,
                    &new_ct,
                    &updated_explicit_indexes,
                    ctx.db_set,
                    ctx.functions,
                    "rename",
                )?;
                if !legacy_mode {
                    validate_views_after_alter(ctx.db_set, ctx.functions, db_id, "rename")?;
                }
            }
            return Ok(());
        }
        AlterTableAction::DropColumn(col_name) => {
            let Some((col_idx, col)) = tbl.find_column(col_name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: \"{col_name}\""),
                ));
            };
            check_alter_rowid_alias(&tbl, col_name, "drop")?;
            if col.is_primary_key {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot drop PRIMARY KEY column: \"{}\"", col.name),
                ));
            }
            if col.is_unique {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot drop UNIQUE column: \"{}\"", col.name),
                ));
            }
            if tbl.columns.len() <= 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "cannot drop column \"{}\": no other columns exist",
                        col.name
                    ),
                ));
            }
            let dropped_is_virtual = col.generated.as_ref().is_some_and(|(_, stored)| !*stored);

            // Both `renameTestSchema` (`!legacy_mode`) and `renameFixQuotes` (`legacy_mode`) run before `DROP COLUMN`.
            validate_views_before_alter(
                ctx.db_set,
                ctx.functions,
                db_id,
                legacy_mode,
                writable_schema,
                &live_sql_map,
                &live_temp_sql_map,
            )?;

            let new_sql = remove_column_def_from_create_table_sql(live_tbl_sql, col_idx)?;
            let (Some(Stmt::CreateTable(new_ct)), _) = parse_one(&new_sql).map_err(|e| {
                SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "error in table {} after drop column: {}",
                        tbl.name, e.message
                    ),
                )
            })?
            else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {} after drop column", tbl.name),
                ));
            };

            if !writable_schema {
                let explicit_index_sqls: Vec<(String, Pgno, String)> = ctx
                    .db_set
                    .slot(db_id)
                    .map(|s| {
                        s.schema
                            .indexes_for_table(&tbl.name)
                            .into_iter()
                            .filter(|idx| !idx.is_autoindex)
                            .filter_map(|idx| {
                                let sql = live_sql_map
                                    .get(&idx.schema_rowid)
                                    .and_then(|o| o.clone())
                                    .or(idx.raw_sql)?;
                                Some((idx.name, idx.root_pgno, sql))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let _ = validate_table_and_indexes_static_after_alter(
                    db_id,
                    &tbl.name,
                    &new_ct,
                    &explicit_index_sqls,
                    ctx.db_set,
                    ctx.functions,
                    "drop column",
                )?;
            }

            {
                let slot = ctx
                    .db_set
                    .slot_mut(db_id)
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                {
                    let bt = slot
                        .btree
                        .as_mut()
                        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                    update_schema_entry(
                        bt,
                        tbl.schema_rowid,
                        "table",
                        &tbl.name,
                        &tbl.name,
                        tbl.root_pgno,
                        Some(&new_sql),
                    )?;
                }
                slot.reload_schema()?;
            }
            ctx.db_set.reconcile_temp_triggers()?;
            *ctx.schema_generation += 1;
            *ctx.v1_expire_generation += 1;
            *ctx.schema_changed_in_tx = true;

            if !writable_schema && !legacy_mode {
                validate_views_after_alter(ctx.db_set, ctx.functions, db_id, "drop column")?;
            }

            if !dropped_is_virtual {
                let new_tbl = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.schema.find_user_table(&tbl.name).cloned())
                    .ok_or_else(|| {
                        SqliteError::misuse("reloaded table missing after drop column")
                    })?;
                rewrite_physical_rows_for_drop_column(ctx, db_id, &tbl, &new_tbl, col_idx)?;
            }
            return Ok(());
        }
        AlterTableAction::AddColumn(cdef) => {
            if tbl.find_column(&cdef.name).is_some() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("duplicate column name: {}", cdef.name),
                ));
            }
            check_alter_rowid_alias(&tbl, &cdef.name, "add")?;
            let validated = build_column_schema_from_def(&tbl.name, tbl.strict, cdef)?;
            if validated
                .generated
                .as_ref()
                .is_some_and(|(_, stored)| *stored)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "cannot add a STORED column",
                ));
            }
            let has_fk = cdef
                .constraints
                .iter()
                .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::ForeignKey { .. }));
            if has_fk
                && *ctx.foreign_keys
                && validated
                    .default_expr
                    .as_ref()
                    .is_some_and(|e| !matches!(e, Expr::Null))
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "Cannot add a REFERENCES column with non-NULL default value",
                ));
            }
            let rendered_col = render_column_def_sql(cdef);
            let new_sql = append_column_to_create_table_sql(live_tbl_sql, &rendered_col)?;
            let (Some(Stmt::CreateTable(new_ct)), _) = parse_one(&new_sql)? else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("error in table {} after add column", tbl.name),
                ));
            };
            let (mut new_tbl, _) = build_table_schema_from_ast(&new_ct, tbl.root_pgno)?;
            new_tbl.name.clone_from(&tbl.name);

            let has_new_check = cdef
                .constraints
                .iter()
                .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::Check { .. }));
            let new_col_idx = new_tbl.columns.len() - 1;
            if has_new_check
                || validated.not_null.is_some()
                || new_tbl.strict
                || validated.generated.is_some()
            {
                let existing_rows = scan_table_raw_entries(ctx, db_id, &tbl)?;
                for (rkey, payload) in existing_rows {
                    let vals =
                        decode_table_logical_row(ctx, db_id, &new_tbl, &rkey, &payload, &[])?;
                    let new_col = &new_tbl.columns[new_col_idx];
                    if new_tbl.strict {
                        check_strict_column_type(&new_tbl.name, new_col, &vals[new_col_idx])?;
                    }
                    if new_col.not_null.is_some() && vals[new_col_idx].is_null() {
                        return Err(SqliteError::constraint(
                            extended::SQLITE_CONSTRAINT_NOTNULL,
                            format!(
                                "NOT NULL constraint failed: {}.{}",
                                new_tbl.name, new_col.name
                            ),
                        ));
                    }
                    if has_new_check {
                        let scope = make_single_table_scope(
                            &new_tbl,
                            &schema_name,
                            None,
                            rkey.rowid_or_zero(),
                            &vals,
                        );
                        for chk in &new_tbl.checks[tbl.checks.len()..] {
                            let cv = eval_expr(
                                ctx,
                                &chk.expr,
                                &[],
                                std::slice::from_ref(&scope),
                                &CteStore::default(),
                                None,
                            )?;
                            if cv.truthiness() == Some(false) {
                                return Err(SqliteError::constraint(
                                    extended::SQLITE_CONSTRAINT_CHECK,
                                    format!("CHECK constraint failed: {}", chk.label),
                                ));
                            }
                        }
                    }
                }
            }

            let bt = ctx
                .db_set
                .slot_mut(db_id)
                .and_then(|s| s.btree.as_mut())
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            update_schema_entry(
                bt,
                tbl.schema_rowid,
                "table",
                &tbl.name,
                &tbl.name,
                tbl.root_pgno,
                Some(&new_sql),
            )?;
        }
    }

    if db_id == DatabaseId::TEMP {
        ctx.db_set.reload_temp_schema_preserving_bindings()?;
    } else {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        slot.reload_schema()?;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    *ctx.schema_changed_in_tx = true;
    Ok(())
}

// ============================================================================
// Token-Aware SQL Rewriting for `ALTER TABLE` (`sqlite/src/alter.c`)
// ============================================================================

fn is_ident_like_token(kind: TokenKind) -> bool {
    match kind {
        TokenKind::Id | TokenKind::QuotedId => true,
        TokenKind::Semi
        | TokenKind::Lp
        | TokenKind::Rp
        | TokenKind::Comma
        | TokenKind::Dot
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Rem
        | TokenKind::Eq
        | TokenKind::Ne
        | TokenKind::Lt
        | TokenKind::Le
        | TokenKind::Gt
        | TokenKind::Ge
        | TokenKind::BitAnd
        | TokenKind::BitOr
        | TokenKind::BitNot
        | TokenKind::LShift
        | TokenKind::RShift
        | TokenKind::Concat
        | TokenKind::Ptr
        | TokenKind::PtrText
        | TokenKind::StringLiteral
        | TokenKind::BlobLiteral
        | TokenKind::Integer
        | TokenKind::Float
        | TokenKind::Variable
        | TokenKind::Eof => false,
        _ => true, // SQLite keywords allowed in identifier positions
    }
}

fn token_ident_name(tok: &Token<'_>) -> String {
    if tok.kind == TokenKind::QuotedId {
        dequote(tok.text)
    } else {
        tok.text.to_owned()
    }
}

fn token_matches_ident(tok: &Token<'_>, target: &str) -> bool {
    is_ident_like_token(tok.kind) && token_ident_name(tok).eq_ignore_ascii_case(target)
}

/// Formats a replacement table identifier for `ALTER TABLE ... RENAME TO ...` (`renameTableFunc`
/// + `renameEditSql` with `bQuote = 1` in `sqlite/src/alter.c:1268-1308, 1798`).
fn format_renamed_table_ident(sql: &str, token_end: usize, new_tbl: &str) -> String {
    let trailing_space = if sql.as_bytes().get(token_end) == Some(&b'"') {
        " "
    } else {
        ""
    };
    format!("\"{}\"{trailing_space}", new_tbl.replace('"', "\"\""))
}

fn format_replacement_ident(orig_tok_text: &str, new_name: &str) -> String {
    let was_quoted = orig_tok_text.starts_with('"')
        || orig_tok_text.starts_with('`')
        || orig_tok_text.starts_with('[');
    let needs_quotes = was_quoted
        || new_name.is_empty()
        || keyword_kind(new_name).is_some()
        || new_name.as_bytes()[0].is_ascii_digit()
        || !new_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if needs_quotes {
        format!("\"{}\"", new_name.replace('"', "\"\""))
    } else {
        new_name.to_owned()
    }
}

fn apply_token_replacements(sql: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    if edits.is_empty() {
        return sql.to_owned();
    }
    edits.sort_by_key(|&(start, _, _)| start);
    let mut out = String::with_capacity(sql.len() + 16);
    let mut cursor = 0usize;
    for (start, len, rep) in edits {
        if start >= cursor {
            out.push_str(&sql[cursor..start]);
            out.push_str(&rep);
            cursor = start + len;
        }
    }
    out.push_str(&sql[cursor..]);
    out
}

fn collect_self_table_qualifier_edits_in_expr(
    expr: &Expr,
    schema_name: &str,
    old_tbl: &str,
    new_tbl: &str,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) {
    match expr {
        Expr::Column(cref) => {
            if let Some(ref tbl_tok) = cref.table {
                let schema_ok = cref
                    .schema
                    .as_deref()
                    .is_none_or(|s| s.eq_ignore_ascii_case(schema_name));
                if schema_ok
                    && tbl_tok.eq_ignore_ascii_case(old_tbl)
                    && let Some(sp) = spans.span_of(tbl_tok)
                {
                    edits.push((sp, new_tbl.to_owned()));
                }
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_self_table_qualifier_edits_in_expr(
                inner,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
        }
        Expr::Binary { left, right, .. } => {
            collect_self_table_qualifier_edits_in_expr(
                left,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            collect_self_table_qualifier_edits_in_expr(
                right,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_self_table_qualifier_edits_in_expr(
                e,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            collect_self_table_qualifier_edits_in_expr(
                low,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            collect_self_table_qualifier_edits_in_expr(
                high,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
        }
        Expr::InList { expr: e, list, .. } => {
            collect_self_table_qualifier_edits_in_expr(
                e,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            for item in list {
                collect_self_table_qualifier_edits_in_expr(
                    item,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_self_table_qualifier_edits_in_expr(
                e,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            collect_self_table_qualifier_edits_in_expr(
                pattern,
                schema_name,
                old_tbl,
                new_tbl,
                spans,
                edits,
            );
            if let Some(esc) = escape {
                collect_self_table_qualifier_edits_in_expr(
                    esc,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_self_table_qualifier_edits_in_expr(
                    op,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
            for (w, t) in branches {
                collect_self_table_qualifier_edits_in_expr(
                    w,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
                collect_self_table_qualifier_edits_in_expr(
                    t,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
            if let Some(el) = else_expr {
                collect_self_table_qualifier_edits_in_expr(
                    el,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    collect_self_table_qualifier_edits_in_expr(
                        e,
                        schema_name,
                        old_tbl,
                        new_tbl,
                        spans,
                        edits,
                    );
                }
            }
            if let Some(f) = filter {
                collect_self_table_qualifier_edits_in_expr(
                    f,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
        }
        Expr::RowValue(es) => {
            for e in es {
                collect_self_table_qualifier_edits_in_expr(
                    e,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    spans,
                    edits,
                );
            }
        }
        _ => {}
    }
}

fn rewrite_table_name_in_create_table_sql(
    sql: &str,
    schema_name: &str,
    old_tbl: &str,
    new_tbl: &str,
    rewrite_fks: bool,
    rewrite_self_exprs: bool,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() && tokens[i].kind != TokenKind::Table {
        i += 1;
    }
    if i < tokens.len() {
        i += 1; // past TABLE
        if i + 2 < tokens.len()
            && tokens[i].kind == TokenKind::If
            && tokens[i + 1].kind == TokenKind::Not
            && tokens[i + 2].kind == TokenKind::Exists
        {
            i += 3;
        }
        if i + 2 < tokens.len() && tokens[i + 1].kind == TokenKind::Dot {
            i += 2;
        }
        if i < tokens.len() && token_matches_ident(&tokens[i], old_tbl) {
            edits.push((
                tokens[i].offset,
                tokens[i].text.len(),
                format_renamed_table_ident(sql, tokens[i].offset + tokens[i].text.len(), new_tbl),
            ));
        }
    }
    if rewrite_fks {
        for idx in 0..tokens.len() {
            if tokens[idx].kind == TokenKind::References
                && idx + 1 < tokens.len()
                && token_matches_ident(&tokens[idx + 1], old_tbl)
            {
                let t = &tokens[idx + 1];
                edits.push((
                    t.offset,
                    t.text.len(),
                    format_renamed_table_ident(sql, t.offset + t.text.len(), new_tbl),
                ));
            }
        }
    }
    let mut out = apply_token_replacements(sql, edits);
    if rewrite_self_exprs
        && let Ok((Some(Stmt::CreateTable(ct)), spans, _)) = parse_one_with_spans(&out)
    {
        let mut span_edits = Vec::new();
        for cdef in &ct.columns {
            for cons in &cdef.constraints {
                match cons {
                    sqliters_syntax::ColumnConstraint::Check { expr, .. }
                    | sqliters_syntax::ColumnConstraint::Generated { expr, .. }
                    | sqliters_syntax::ColumnConstraint::Default(expr) => {
                        collect_self_table_qualifier_edits_in_expr(
                            expr,
                            schema_name,
                            old_tbl,
                            new_tbl,
                            &spans,
                            &mut span_edits,
                        );
                    }
                    _ => {}
                }
            }
        }
        for tcons in &ct.constraints {
            if let sqliters_syntax::TableConstraint::Check { expr, .. } = tcons {
                collect_self_table_qualifier_edits_in_expr(
                    expr,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    &spans,
                    &mut span_edits,
                );
            }
        }
        if !span_edits.is_empty() {
            for (sp, rep) in &mut span_edits {
                *rep = format_renamed_table_ident(&out, sp.offset + sp.len, new_tbl);
            }
            out = apply_span_edits(&out, span_edits);
        }
    }
    Ok(out)
}

fn rewrite_table_name_in_create_index_sql(
    sql: &str,
    schema_name: &str,
    old_tbl: &str,
    new_tbl: &str,
    rewrite_self_exprs: bool,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    for i in 0..tokens.len() {
        if tokens[i].kind == TokenKind::On && i + 1 < tokens.len() {
            let t = &tokens[i + 1];
            if token_matches_ident(t, old_tbl) {
                edits.push((
                    t.offset,
                    t.text.len(),
                    format_renamed_table_ident(sql, t.offset + t.text.len(), new_tbl),
                ));
            }
            break;
        }
    }
    let mut out = apply_token_replacements(sql, edits);
    if rewrite_self_exprs
        && let Ok((Some(Stmt::CreateIndex(ci)), spans, _)) = parse_one_with_spans(&out)
    {
        let mut span_edits = Vec::new();
        for ic in &ci.columns {
            if let Some(ref ie) = ic.expr {
                collect_self_table_qualifier_edits_in_expr(
                    ie,
                    schema_name,
                    old_tbl,
                    new_tbl,
                    &spans,
                    &mut span_edits,
                );
            }
        }
        if let Some(ref wc) = ci.where_clause {
            collect_self_table_qualifier_edits_in_expr(
                wc,
                schema_name,
                old_tbl,
                new_tbl,
                &spans,
                &mut span_edits,
            );
        }
        if !span_edits.is_empty() {
            for (sp, rep) in &mut span_edits {
                *rep = format_renamed_table_ident(&out, sp.offset + sp.len, new_tbl);
            }
            out = apply_span_edits(&out, span_edits);
        }
    }
    Ok(out)
}

#[derive(Clone)]
struct FromTableBinding {
    schema_name: Option<String>,
    effective_alias: String,
    has_explicit_alias: bool,
    is_target_table: bool,
}

impl FromTableBinding {
    fn matches_qualifier(&self, schema_qual: Option<&str>, table_qual: &str) -> bool {
        if let Some(sq) = schema_qual {
            let Some(ref sn) = self.schema_name else {
                return false;
            };
            if !sn.eq_ignore_ascii_case(sq) {
                return false;
            }
        }
        self.effective_alias.eq_ignore_ascii_case(table_qual)
    }
}

fn view_qn_resolves_to_target_table(
    qn: &QualifiedName,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    local_cte_names: &[String],
    db_set: &DatabaseSet,
) -> (bool, Option<String>) {
    if qn.schema.is_none()
        && local_cte_names
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&qn.name))
    {
        return (false, None);
    }
    if view_db_id != DatabaseId::TEMP {
        let Some(slot) = db_set.slot(view_db_id) else {
            return (false, None);
        };
        let slot_name = slot.name.clone();
        let schema_ok = qn
            .schema
            .as_deref()
            .is_none_or(|s| s.eq_ignore_ascii_case(&slot_name));
        if !schema_ok {
            return (false, None);
        }
        let is_target = view_db_id == target_db_id
            && qn.name.eq_ignore_ascii_case(old_tbl)
            && slot.schema.find_user_table(old_tbl).is_some();
        return (is_target, Some(slot_name));
    }

    // TEMP view: standard `DatabaseSet` search order (`temp -> main -> attached`)
    if let Ok(Some((found_id, rel))) = db_set.find_relation_in_set(qn) {
        let slot_name = db_set.slot(found_id).map(|s| s.name.clone());
        let is_target = matches!(rel, ResolvedRelation::Table(_))
            && found_id == target_db_id
            && qn.name.eq_ignore_ascii_case(old_tbl);
        return (is_target, slot_name);
    }
    (false, None)
}

fn rewrite_view_for_table_rename(
    view_sql: &str,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    db_set: &DatabaseSet,
) -> Result<Option<String>> {
    let (Some(Stmt::CreateView(cv)), spans, _) = parse_one_with_spans(view_sql)? else {
        return Ok(None);
    };
    let mut edits: Vec<(TokenSpan, String)> = Vec::new();
    let mut cte_names: Vec<String> = Vec::new();
    let outer_bindings: Vec<Vec<FromTableBinding>> = Vec::new();
    collect_table_rename_edits_in_select(
        &cv.query,
        view_db_id,
        target_db_id,
        old_tbl,
        new_tbl,
        &mut cte_names,
        &outer_bindings,
        db_set,
        &spans,
        &mut edits,
    );
    if edits.is_empty() {
        Ok(None)
    } else {
        for (sp, rep) in &mut edits {
            *rep = format_renamed_table_ident(view_sql, sp.offset + sp.len, new_tbl);
        }
        Ok(Some(apply_span_edits(view_sql, edits)))
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_table_rename_edits_in_select(
    sel: &SelectStmt,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    cte_names: &mut Vec<String>,
    outer_bindings: &[Vec<FromTableBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) {
    let base_cte_len = cte_names.len();
    for cte in &sel.ctes {
        cte_names.push(cte.name.clone());
        collect_table_rename_edits_in_select(
            &cte.query,
            view_db_id,
            target_db_id,
            old_tbl,
            new_tbl,
            cte_names,
            outer_bindings,
            db_set,
            spans,
            edits,
        );
    }
    let head_bindings = collect_table_rename_edits_in_core(
        &sel.body.head,
        view_db_id,
        target_db_id,
        old_tbl,
        new_tbl,
        cte_names,
        outer_bindings,
        db_set,
        spans,
        edits,
    );
    for (_, rhs) in &sel.body.compounds {
        let _ = collect_table_rename_edits_in_core(
            rhs,
            view_db_id,
            target_db_id,
            old_tbl,
            new_tbl,
            cte_names,
            outer_bindings,
            db_set,
            spans,
            edits,
        );
    }
    let mut ob_scopes = outer_bindings.to_vec();
    ob_scopes.push(head_bindings);
    for ob in &sel.order_by {
        collect_table_rename_edits_in_expr(
            &ob.expr,
            view_db_id,
            target_db_id,
            old_tbl,
            new_tbl,
            cte_names,
            &ob_scopes,
            db_set,
            spans,
            edits,
        );
    }
    if let Some(ref lim) = sel.limit {
        collect_table_rename_edits_in_expr(
            &lim.limit,
            view_db_id,
            target_db_id,
            old_tbl,
            new_tbl,
            cte_names,
            outer_bindings,
            db_set,
            spans,
            edits,
        );
        if let Some(ref off) = lim.offset {
            collect_table_rename_edits_in_expr(
                off,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                outer_bindings,
                db_set,
                spans,
                edits,
            );
        }
    }
    cte_names.truncate(base_cte_len);
}

#[allow(clippy::too_many_arguments)]
fn collect_table_rename_edits_in_core(
    core: &SelectCore,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    cte_names: &mut Vec<String>,
    outer_bindings: &[Vec<FromTableBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) -> Vec<FromTableBinding> {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_table_rename_edits_in_expr(
                        e,
                        view_db_id,
                        target_db_id,
                        old_tbl,
                        new_tbl,
                        cte_names,
                        outer_bindings,
                        db_set,
                        spans,
                        edits,
                    );
                }
            }
            Vec::new()
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let mut cur_bindings = Vec::new();
            if let Some(fc) = from {
                cur_bindings.push(collect_table_rename_edits_in_tos(
                    &fc.head,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    outer_bindings,
                    db_set,
                    spans,
                    edits,
                ));
                for j in &fc.joins {
                    cur_bindings.push(collect_table_rename_edits_in_tos(
                        &j.right,
                        view_db_id,
                        target_db_id,
                        old_tbl,
                        new_tbl,
                        cte_names,
                        outer_bindings,
                        db_set,
                        spans,
                        edits,
                    ));
                    if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
                        let mut join_scopes = outer_bindings.to_vec();
                        join_scopes.push(cur_bindings.clone());
                        collect_table_rename_edits_in_expr(
                            on_e,
                            view_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            cte_names,
                            &join_scopes,
                            db_set,
                            spans,
                            edits,
                        );
                    }
                }
            }

            let mut full_scopes = outer_bindings.to_vec();
            full_scopes.push(cur_bindings.clone());
            for rc in projection {
                match rc {
                    ResultColumn::Star => {}
                    ResultColumn::TableStar { schema, table } => {
                        maybe_record_unaliased_table_qualifier_edit(
                            schema.as_deref(),
                            table,
                            new_tbl,
                            &full_scopes,
                            spans,
                            edits,
                        );
                    }
                    ResultColumn::Expr { expr, .. } => {
                        collect_table_rename_edits_in_expr(
                            expr,
                            view_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            cte_names,
                            &full_scopes,
                            db_set,
                            spans,
                            edits,
                        );
                    }
                }
            }
            if let Some(w) = selection {
                collect_table_rename_edits_in_expr(
                    w,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            for g in group_by {
                collect_table_rename_edits_in_expr(
                    g,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            if let Some(h) = having {
                collect_table_rename_edits_in_expr(
                    h,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            cur_bindings
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_table_rename_edits_in_tos(
    tos: &TableOrSubquery,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    cte_names: &mut Vec<String>,
    outer_bindings: &[Vec<FromTableBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) -> FromTableBinding {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let (is_target, schema_lbl) = view_qn_resolves_to_target_table(
                name,
                view_db_id,
                target_db_id,
                old_tbl,
                cte_names,
                db_set,
            );
            if is_target && let Some(sp) = spans.span_of(&name.name) {
                edits.push((sp, new_tbl.to_owned()));
            }
            FromTableBinding {
                schema_name: schema_lbl,
                effective_alias: alias.clone().unwrap_or_else(|| name.name.clone()),
                has_explicit_alias: alias.is_some(),
                is_target_table: is_target,
            }
        }
        TableOrSubquery::Subquery { query, alias } => {
            collect_table_rename_edits_in_select(
                query,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                outer_bindings,
                db_set,
                spans,
                edits,
            );
            FromTableBinding {
                schema_name: None,
                effective_alias: alias.clone().unwrap_or_default(),
                has_explicit_alias: true,
                is_target_table: false,
            }
        }
    }
}

fn maybe_record_unaliased_table_qualifier_edit(
    schema_qual: Option<&str>,
    table_tok: &str,
    new_tbl: &str,
    scopes: &[Vec<FromTableBinding>],
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) {
    for scope in scopes.iter().rev() {
        let mut matched: Option<&FromTableBinding> = None;
        for b in scope {
            if !b.effective_alias.is_empty() && b.matches_qualifier(schema_qual, table_tok) {
                matched = Some(b);
                break;
            }
        }
        if let Some(hit) = matched {
            if hit.is_target_table
                && !hit.has_explicit_alias
                && let Some(sp) = spans.span_of(table_tok)
            {
                edits.push((sp, new_tbl.to_owned()));
            }
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_table_rename_edits_in_expr(
    expr: &Expr,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    cte_names: &mut Vec<String>,
    scopes: &[Vec<FromTableBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) {
    match expr {
        Expr::Column(cref) => {
            if let Some(ref tbl_tok) = cref.table {
                maybe_record_unaliased_table_qualifier_edit(
                    cref.schema.as_deref(),
                    tbl_tok,
                    new_tbl,
                    scopes,
                    spans,
                    edits,
                );
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_table_rename_edits_in_expr(
                inner,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Binary { left, right, .. } => {
            collect_table_rename_edits_in_expr(
                left,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            collect_table_rename_edits_in_expr(
                right,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            for sub in [e, low, high] {
                collect_table_rename_edits_in_expr(
                    sub,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::InList { expr: e, list, .. } => {
            collect_table_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            for item in list {
                collect_table_rename_edits_in_expr(
                    item,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            collect_table_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            collect_table_rename_edits_in_select(
                subquery,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::InTable { expr: e, table, .. } => {
            collect_table_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            let (is_target, _) = view_qn_resolves_to_target_table(
                table,
                view_db_id,
                target_db_id,
                old_tbl,
                cte_names,
                db_set,
            );
            if is_target && let Some(sp) = spans.span_of(&table.name) {
                edits.push((sp, new_tbl.to_owned()));
            }
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_table_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            collect_table_rename_edits_in_expr(
                pattern,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
            if let Some(esc) = escape {
                collect_table_rename_edits_in_expr(
                    esc,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_table_rename_edits_in_expr(
                    op,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            for (w, t) in branches {
                collect_table_rename_edits_in_expr(
                    w,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
                collect_table_rename_edits_in_expr(
                    t,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            if let Some(el) = else_expr {
                collect_table_rename_edits_in_expr(
                    el,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    collect_table_rename_edits_in_expr(
                        e,
                        view_db_id,
                        target_db_id,
                        old_tbl,
                        new_tbl,
                        cte_names,
                        scopes,
                        db_set,
                        spans,
                        edits,
                    );
                }
            }
            if let Some(f) = filter {
                collect_table_rename_edits_in_expr(
                    f,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::RowValue(es) => {
            for e in es {
                collect_table_rename_edits_in_expr(
                    e,
                    view_db_id,
                    target_db_id,
                    old_tbl,
                    new_tbl,
                    cte_names,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            collect_table_rename_edits_in_select(
                sub,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Raise {
            message: Some(m), ..
        } => {
            collect_table_rename_edits_in_expr(
                m,
                view_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                cte_names,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        _ => {}
    }
}

/// Structurally rewrites a trigger definition (`sqlite_schema` / `sqlite_temp_schema`) for
/// `ALTER TABLE old_tbl RENAME TO new_tbl`, matching `renameTableFunc` (`sqlite/src/alter.c:1878-1903`).
///
/// Returns `(updated_sql_if_changed, trigger_on_clause_targeted_renamed_table)`.
fn rewrite_trigger_for_table_rename(
    trig_sql: &str,
    trig_db_id: DatabaseId,
    target_db_id: DatabaseId,
    old_tbl: &str,
    new_tbl: &str,
    legacy_mode: bool,
    db_set: &DatabaseSet,
) -> Result<(Option<String>, bool)> {
    let (Some(Stmt::CreateTrigger(ct)), spans, _) = parse_one_with_spans(trig_sql)? else {
        return Ok((None, false));
    };
    let mut edits: Vec<(TokenSpan, String)> = Vec::new();
    let (is_on_target, _) = view_qn_resolves_to_target_table(
        &ct.on_table,
        trig_db_id,
        target_db_id,
        old_tbl,
        &[],
        db_set,
    );
    if is_on_target && let Some(sp) = spans.span_of(&ct.on_table.name) {
        edits.push((sp, new_tbl.to_owned()));
    }

    if !legacy_mode {
        let mut pseudo_bindings = Vec::new();
        let mk_pseudo = |alias: &str| FromTableBinding {
            schema_name: None,
            effective_alias: alias.to_owned(),
            has_explicit_alias: true,
            is_target_table: false,
        };
        match ct.event {
            TriggerEvent::Insert => pseudo_bindings.push(mk_pseudo("new")),
            TriggerEvent::Delete => pseudo_bindings.push(mk_pseudo("old")),
            TriggerEvent::Update { .. } => {
                pseudo_bindings.push(mk_pseudo("old"));
                pseudo_bindings.push(mk_pseudo("new"));
            }
        }
        let outer_bindings = vec![pseudo_bindings];
        let mut cte_names: Vec<String> = Vec::new();

        if let Some(ref w) = ct.when_clause {
            collect_table_rename_edits_in_expr(
                w,
                trig_db_id,
                target_db_id,
                old_tbl,
                new_tbl,
                &mut cte_names,
                &outer_bindings,
                db_set,
                &spans,
                &mut edits,
            );
        }

        for step in &ct.body {
            match step {
                TriggerStep::Select(sel) => {
                    collect_table_rename_edits_in_select(
                        sel,
                        trig_db_id,
                        target_db_id,
                        old_tbl,
                        new_tbl,
                        &mut cte_names,
                        &outer_bindings,
                        db_set,
                        &spans,
                        &mut edits,
                    );
                }
                TriggerStep::Insert(ins) => {
                    match &ins.source {
                        InsertSource::Values(rows) => {
                            for row in rows {
                                for e in row {
                                    collect_table_rename_edits_in_expr(
                                        e,
                                        trig_db_id,
                                        target_db_id,
                                        old_tbl,
                                        new_tbl,
                                        &mut cte_names,
                                        &outer_bindings,
                                        db_set,
                                        &spans,
                                        &mut edits,
                                    );
                                }
                            }
                        }
                        InsertSource::Select(sel) => {
                            collect_table_rename_edits_in_select(
                                sel,
                                trig_db_id,
                                target_db_id,
                                old_tbl,
                                new_tbl,
                                &mut cte_names,
                                &outer_bindings,
                                db_set,
                                &spans,
                                &mut edits,
                            );
                        }
                        InsertSource::DefaultValues => {}
                    }

                    let (is_step_tgt, schema_lbl) = view_qn_resolves_to_target_table(
                        &ins.table,
                        trig_db_id,
                        target_db_id,
                        old_tbl,
                        &[],
                        db_set,
                    );
                    if is_step_tgt && let Some(sp) = spans.span_of(&ins.table.name) {
                        edits.push((sp, new_tbl.to_owned()));
                    }
                    if !ins.upsert_clauses.is_empty() {
                        let tgt_binding = FromTableBinding {
                            schema_name: schema_lbl,
                            effective_alias: ins
                                .alias
                                .clone()
                                .unwrap_or_else(|| ins.table.name.clone()),
                            has_explicit_alias: ins.alias.is_some(),
                            is_target_table: is_step_tgt,
                        };
                        let excl_binding = FromTableBinding {
                            schema_name: None,
                            effective_alias: "excluded".to_owned(),
                            has_explicit_alias: true,
                            is_target_table: false,
                        };
                        let mut target_scopes = outer_bindings.clone();
                        target_scopes.push(vec![tgt_binding.clone()]);
                        let mut upsert_set_scopes = outer_bindings.clone();
                        upsert_set_scopes.push(vec![tgt_binding, excl_binding]);

                        for uc in &ins.upsert_clauses {
                            if let Some(ref tgt) = uc.target {
                                for ic in &tgt.columns {
                                    if let Some(ref ie) = ic.expr {
                                        collect_table_rename_edits_in_expr(
                                            ie,
                                            trig_db_id,
                                            target_db_id,
                                            old_tbl,
                                            new_tbl,
                                            &mut cte_names,
                                            &target_scopes,
                                            db_set,
                                            &spans,
                                            &mut edits,
                                        );
                                    }
                                }
                                if let Some(ref tw) = tgt.where_clause {
                                    collect_table_rename_edits_in_expr(
                                        tw,
                                        trig_db_id,
                                        target_db_id,
                                        old_tbl,
                                        new_tbl,
                                        &mut cte_names,
                                        &target_scopes,
                                        db_set,
                                        &spans,
                                        &mut edits,
                                    );
                                }
                            }
                            if let UpsertAction::DoUpdate {
                                ref assignments,
                                ref where_clause,
                            } = uc.action
                            {
                                for asgn in assignments {
                                    collect_table_rename_edits_in_expr(
                                        &asgn.expr,
                                        trig_db_id,
                                        target_db_id,
                                        old_tbl,
                                        new_tbl,
                                        &mut cte_names,
                                        &upsert_set_scopes,
                                        db_set,
                                        &spans,
                                        &mut edits,
                                    );
                                }
                                if let Some(uw) = where_clause {
                                    collect_table_rename_edits_in_expr(
                                        uw,
                                        trig_db_id,
                                        target_db_id,
                                        old_tbl,
                                        new_tbl,
                                        &mut cte_names,
                                        &upsert_set_scopes,
                                        db_set,
                                        &spans,
                                        &mut edits,
                                    );
                                }
                            }
                        }
                    }
                }
                TriggerStep::Update(upd) => {
                    let (is_step_tgt, schema_lbl) = view_qn_resolves_to_target_table(
                        &upd.table,
                        trig_db_id,
                        target_db_id,
                        old_tbl,
                        &[],
                        db_set,
                    );
                    if is_step_tgt && let Some(sp) = spans.span_of(&upd.table.name) {
                        edits.push((sp, new_tbl.to_owned()));
                    }
                    let mut step_bindings = vec![FromTableBinding {
                        schema_name: schema_lbl,
                        effective_alias: upd
                            .alias
                            .clone()
                            .unwrap_or_else(|| upd.table.name.clone()),
                        has_explicit_alias: upd.alias.is_some(),
                        is_target_table: is_step_tgt,
                    }];
                    if let Some(ref fc) = upd.from {
                        let mut from_bindings = vec![collect_table_rename_edits_in_tos(
                            &fc.head,
                            trig_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            &mut cte_names,
                            &outer_bindings,
                            db_set,
                            &spans,
                            &mut edits,
                        )];
                        for j in &fc.joins {
                            from_bindings.push(collect_table_rename_edits_in_tos(
                                &j.right,
                                trig_db_id,
                                target_db_id,
                                old_tbl,
                                new_tbl,
                                &mut cte_names,
                                &outer_bindings,
                                db_set,
                                &spans,
                                &mut edits,
                            ));
                            if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
                                let mut join_scopes = outer_bindings.clone();
                                join_scopes.push(from_bindings.clone());
                                collect_table_rename_edits_in_expr(
                                    on_e,
                                    trig_db_id,
                                    target_db_id,
                                    old_tbl,
                                    new_tbl,
                                    &mut cte_names,
                                    &join_scopes,
                                    db_set,
                                    &spans,
                                    &mut edits,
                                );
                            }
                        }
                        step_bindings.extend(from_bindings);
                    }
                    let mut step_scopes = outer_bindings.clone();
                    step_scopes.push(step_bindings);
                    for asgn in &upd.assignments {
                        collect_table_rename_edits_in_expr(
                            &asgn.expr,
                            trig_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            &mut cte_names,
                            &step_scopes,
                            db_set,
                            &spans,
                            &mut edits,
                        );
                    }
                    if let Some(ref w) = upd.selection {
                        collect_table_rename_edits_in_expr(
                            w,
                            trig_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            &mut cte_names,
                            &step_scopes,
                            db_set,
                            &spans,
                            &mut edits,
                        );
                    }
                }
                TriggerStep::Delete(del) => {
                    let (is_step_tgt, schema_lbl) = view_qn_resolves_to_target_table(
                        &del.table,
                        trig_db_id,
                        target_db_id,
                        old_tbl,
                        &[],
                        db_set,
                    );
                    if is_step_tgt && let Some(sp) = spans.span_of(&del.table.name) {
                        edits.push((sp, new_tbl.to_owned()));
                    }
                    let mut step_scopes = outer_bindings.clone();
                    step_scopes.push(vec![FromTableBinding {
                        schema_name: schema_lbl,
                        effective_alias: del
                            .alias
                            .clone()
                            .unwrap_or_else(|| del.table.name.clone()),
                        has_explicit_alias: del.alias.is_some(),
                        is_target_table: is_step_tgt,
                    }]);
                    if let Some(ref w) = del.selection {
                        collect_table_rename_edits_in_expr(
                            w,
                            trig_db_id,
                            target_db_id,
                            old_tbl,
                            new_tbl,
                            &mut cte_names,
                            &step_scopes,
                            db_set,
                            &spans,
                            &mut edits,
                        );
                    }
                }
            }
        }
    }

    if edits.is_empty() {
        Ok((None, is_on_target))
    } else {
        for (sp, rep) in &mut edits {
            *rep = format_renamed_table_ident(trig_sql, sp.offset + sp.len, new_tbl);
        }
        Ok((Some(apply_span_edits(trig_sql, edits)), is_on_target))
    }
}

#[derive(Clone)]
struct RenameColBinding {
    schema_name: Option<String>,
    table_alias: Option<String>,
    name: String,
    is_target_col: bool,
    merged_using: bool,
    qualified_only: bool,
}

impl RenameColBinding {
    fn matches_qualifier(&self, schema_qual: Option<&str>, table_qual: Option<&str>) -> bool {
        if self.qualified_only && (schema_qual.is_some() || table_qual.is_none()) {
            return false;
        }
        if let Some(sq) = schema_qual {
            let Some(ref sn) = self.schema_name else {
                return false;
            };
            if !sn.eq_ignore_ascii_case(sq) {
                return false;
            }
        }
        if let Some(tq) = table_qual {
            let Some(ref ta) = self.table_alias else {
                return false;
            };
            if !ta.eq_ignore_ascii_case(tq) {
                return false;
            }
        }
        true
    }
}

fn merge_join_rename_col_bindings(
    left: &[RenameColBinding],
    right: &[RenameColBinding],
    join: &JoinClause,
) -> Vec<RenameColBinding> {
    let dedup_names: Vec<String> = if join.natural {
        let mut common = Vec::new();
        for lc in left {
            if right
                .iter()
                .any(|rc| rc.name.eq_ignore_ascii_case(&lc.name))
                && !common
                    .iter()
                    .any(|c: &String| c.eq_ignore_ascii_case(&lc.name))
            {
                common.push(lc.name.clone());
            }
        }
        common
    } else if let Some(JoinConstraint::Using(ref ucols)) = join.constraint {
        ucols.clone()
    } else {
        Vec::new()
    };

    if dedup_names.is_empty() {
        let mut out = left.to_vec();
        out.extend_from_slice(right);
        return out;
    }

    let mut out = Vec::with_capacity(left.len() + right.len());
    for lc in left {
        let mut copy = lc.clone();
        if dedup_names.iter().any(|u| u.eq_ignore_ascii_case(&lc.name)) {
            copy.merged_using = true;
        }
        out.push(copy);
    }
    out.extend_from_slice(right);
    out
}

fn resolve_cref_is_target_col(cref: &ColumnRef, scopes: &[Vec<RenameColBinding>]) -> Option<bool> {
    for scope in scopes.iter().rev() {
        let mut hits = Vec::new();
        for cb in scope {
            if cb.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref())
                && cb.name.eq_ignore_ascii_case(&cref.column)
            {
                if cref.schema.is_none() && cref.table.is_none() && cb.merged_using {
                    return Some(cb.is_target_col);
                }
                hits.push(cb.is_target_col);
            }
        }
        if !hits.is_empty() {
            return Some(hits.len() == 1 && hits[0]);
        }
    }
    None
}

fn rewrite_view_for_column_rename(
    view_sql: &str,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    db_set: &DatabaseSet,
) -> Result<Option<String>> {
    let (Some(Stmt::CreateView(cv)), spans, _) = parse_one_with_spans(view_sql)? else {
        return Ok(None);
    };
    let mut edits: Vec<(TokenSpan, String)> = Vec::new();
    let ctes = CteStore::default();
    let outer_scopes: Vec<Vec<RenameColBinding>> = Vec::new();
    let _ = collect_col_rename_edits_in_select(
        &cv.query,
        view_db_id,
        target_db_id,
        target_tbl,
        old_col,
        new_col,
        &ctes,
        &outer_scopes,
        db_set,
        &spans,
        &mut edits,
    );
    if edits.is_empty() {
        Ok(None)
    } else {
        for (sp, rep) in &mut edits {
            let orig_tok = &view_sql[sp.offset..sp.offset + sp.len];
            let mut formatted = format_replacement_ident(orig_tok, new_col);
            if formatted.starts_with('"')
                && view_sql.as_bytes().get(sp.offset + sp.len) == Some(&b'"')
            {
                formatted.push(' ');
            }
            *rep = formatted;
        }
        Ok(Some(apply_span_edits(view_sql, edits)))
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_col_rename_edits_in_select(
    sel: &SelectStmt,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    parent_ctes: &CteStore,
    outer_scopes: &[Vec<RenameColBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) -> Vec<String> {
    let mut ctes = parent_ctes.clone();
    if !sel.ctes.is_empty() {
        let mut cur_scope = Vec::new();
        for cte in &sel.ctes {
            cur_scope.push(cte.clone());
            let cte_store = ctes.with_scope(&cur_scope);
            let _ = collect_col_rename_edits_in_select(
                &cte.query,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                &cte_store,
                outer_scopes,
                db_set,
                spans,
                edits,
            );
        }
        ctes = ctes.with_scope(&cur_scope);
    }

    let (head_from_cols, head_proj_cols) = collect_col_rename_edits_in_core(
        &sel.body.head,
        view_db_id,
        target_db_id,
        target_tbl,
        old_col,
        new_col,
        &ctes,
        outer_scopes,
        db_set,
        spans,
        edits,
    );
    for (_, rhs) in &sel.body.compounds {
        let _ = collect_col_rename_edits_in_core(
            rhs,
            view_db_id,
            target_db_id,
            target_tbl,
            old_col,
            new_col,
            &ctes,
            outer_scopes,
            db_set,
            spans,
            edits,
        );
    }

    let mut ob_scopes = outer_scopes.to_vec();
    ob_scopes.push(head_from_cols);
    for ob in &sel.order_by {
        if let Expr::Column(ref cref) = ob.expr
            && cref.schema.is_none()
            && cref.table.is_none()
            && let Some((_, unaliased_target)) = head_proj_cols
                .iter()
                .find(|(pname, _)| pname.eq_ignore_ascii_case(&cref.column))
        {
            if *unaliased_target
                && cref.column.eq_ignore_ascii_case(old_col)
                && let Some(sp) = spans.span_of(&cref.column)
            {
                edits.push((sp, new_col.to_owned()));
            }
            continue;
        }
        collect_col_rename_edits_in_expr(
            &ob.expr,
            view_db_id,
            target_db_id,
            target_tbl,
            old_col,
            new_col,
            &ctes,
            &ob_scopes,
            db_set,
            spans,
            edits,
        );
    }
    if let Some(ref lim) = sel.limit {
        collect_col_rename_edits_in_expr(
            &lim.limit,
            view_db_id,
            target_db_id,
            target_tbl,
            old_col,
            new_col,
            &ctes,
            outer_scopes,
            db_set,
            spans,
            edits,
        );
        if let Some(ref off) = lim.offset {
            collect_col_rename_edits_in_expr(
                off,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                &ctes,
                outer_scopes,
                db_set,
                spans,
                edits,
            );
        }
    }

    head_proj_cols.into_iter().map(|(n, _)| n).collect()
}

#[allow(clippy::too_many_arguments)]
fn collect_col_rename_edits_in_core(
    core: &SelectCore,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    ctes: &CteStore,
    outer_scopes: &[Vec<RenameColBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) -> (Vec<RenameColBinding>, Vec<(String, bool)>) {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_col_rename_edits_in_expr(
                        e,
                        view_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        ctes,
                        outer_scopes,
                        db_set,
                        spans,
                        edits,
                    );
                }
            }
            let width = rows.first().map_or(0, Vec::len);
            let cols = (1..=width).map(|i| (format!("column{i}"), false)).collect();
            (Vec::new(), cols)
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let mut from_cols = Vec::new();
            if let Some(fc) = from {
                from_cols = collect_col_rename_edits_in_tos(
                    &fc.head,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    outer_scopes,
                    db_set,
                    spans,
                    edits,
                );
                for j in &fc.joins {
                    let rcols = collect_col_rename_edits_in_tos(
                        &j.right,
                        view_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        ctes,
                        outer_scopes,
                        db_set,
                        spans,
                        edits,
                    );
                    from_cols = merge_join_rename_col_bindings(&from_cols, &rcols, j);
                    // Note: `JoinConstraint::Using` is intentionally NOT rewritten (`alter.c:unmapColumnIdlistNames`)!
                    if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
                        let mut j_scopes = outer_scopes.to_vec();
                        j_scopes.push(from_cols.clone());
                        collect_col_rename_edits_in_expr(
                            on_e,
                            view_db_id,
                            target_db_id,
                            target_tbl,
                            old_col,
                            new_col,
                            ctes,
                            &j_scopes,
                            db_set,
                            spans,
                            edits,
                        );
                    }
                }
            }

            let mut full_scopes = outer_scopes.to_vec();
            full_scopes.push(from_cols.clone());
            let mut proj_cols: Vec<(String, bool)> = Vec::new();
            for rc in projection {
                match rc {
                    ResultColumn::Star => {
                        let mut seen_using = Vec::new();
                        for cb in &from_cols {
                            if cb.merged_using {
                                seen_using.push(cb.name.clone());
                                proj_cols.push((cb.name.clone(), cb.is_target_col));
                            } else if !seen_using.iter().any(|u| u.eq_ignore_ascii_case(&cb.name)) {
                                proj_cols.push((cb.name.clone(), cb.is_target_col));
                            }
                        }
                    }
                    ResultColumn::TableStar { schema, table } => {
                        for cb in &from_cols {
                            if cb.matches_qualifier(schema.as_deref(), Some(table.as_str())) {
                                proj_cols.push((cb.name.clone(), cb.is_target_col));
                            }
                        }
                    }
                    ResultColumn::Expr { expr, alias, .. } => {
                        collect_col_rename_edits_in_expr(
                            expr,
                            view_db_id,
                            target_db_id,
                            target_tbl,
                            old_col,
                            new_col,
                            ctes,
                            &full_scopes,
                            db_set,
                            spans,
                            edits,
                        );
                        let is_unaliased_target = alias.is_none()
                            && matches!(
                                expr,
                                Expr::Column(cref)
                                    if resolve_cref_is_target_col(cref, &full_scopes) == Some(true)
                            );
                        let out_lbl = alias.clone().unwrap_or_else(|| expr_column_label(expr));
                        proj_cols.push((out_lbl, is_unaliased_target));
                    }
                }
            }

            if let Some(w) = selection {
                collect_col_rename_edits_in_expr(
                    w,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            for g in group_by {
                collect_col_rename_edits_in_expr(
                    g,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            if let Some(h) = having {
                collect_col_rename_edits_in_expr(
                    h,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    &full_scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            (from_cols, proj_cols)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_col_rename_edits_in_tos(
    tos: &TableOrSubquery,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    ctes: &CteStore,
    outer_scopes: &[Vec<RenameColBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) -> Vec<RenameColBinding> {
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                let cte = &ctes.scopes[s_idx][c_idx];
                let mut visited = Vec::new();
                let cnames = if let Some(ref explicit) = cte.columns {
                    explicit.clone()
                } else {
                    static_select_col_names(&cte.query.body, &[], db_set, ctes, &mut visited)
                };
                return cnames
                    .into_iter()
                    .map(|cn| RenameColBinding {
                        schema_name: None,
                        table_alias: Some(eff.clone()),
                        name: cn,
                        is_target_col: false,
                        merged_using: false,
                        qualified_only: false,
                    })
                    .collect();
            }

            let resolved: Option<(DatabaseId, ResolvedRelation)> = if view_db_id != DatabaseId::TEMP
            {
                let slot = db_set.slot(view_db_id);
                let schema_ok = slot.is_some_and(|sl| {
                    name.schema
                        .as_deref()
                        .is_none_or(|s| s.eq_ignore_ascii_case(&sl.name))
                });
                if !schema_ok {
                    None
                } else if let Some(sl) = slot {
                    sl.schema
                        .find_user_table(&name.name)
                        .cloned()
                        .map(|t| (view_db_id, ResolvedRelation::Table(Box::new(t))))
                        .or_else(|| {
                            sl.schema
                                .find_view(&name.name)
                                .cloned()
                                .map(|v| (view_db_id, ResolvedRelation::View(Box::new(v))))
                        })
                } else {
                    None
                }
            } else {
                db_set.find_relation_in_set(name).ok().flatten()
            };

            match resolved {
                Some((found_db_id, ResolvedRelation::Table(tbl))) => {
                    let slot_lbl = db_set
                        .slot(found_db_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| "main".to_owned());
                    let is_target_tbl = found_db_id == target_db_id
                        && tbl.name.eq_ignore_ascii_case(&target_tbl.name);
                    tbl.columns
                        .iter()
                        .map(|c| RenameColBinding {
                            schema_name: Some(slot_lbl.clone()),
                            table_alias: Some(eff.clone()),
                            name: c.name.clone(),
                            is_target_col: is_target_tbl && c.name.eq_ignore_ascii_case(old_col),
                            merged_using: false,
                            qualified_only: false,
                        })
                        .collect()
                }
                Some((found_db_id, ResolvedRelation::View(v))) => {
                    static_view_scoped_cols(found_db_id, &v, &eff, db_set, ctes, &[])
                        .unwrap_or_default()
                        .into_iter()
                        .map(|sc| RenameColBinding {
                            schema_name: sc.schema_name,
                            table_alias: sc.table_alias,
                            name: sc.name,
                            is_target_col: false,
                            merged_using: false,
                            qualified_only: false,
                        })
                        .collect()
                }
                None => Vec::new(),
            }
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_names = collect_col_rename_edits_in_select(
                query,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                outer_scopes,
                db_set,
                spans,
                edits,
            );
            sub_names
                .into_iter()
                .map(|cn| RenameColBinding {
                    schema_name: None,
                    table_alias: alias.clone(),
                    name: cn,
                    is_target_col: false,
                    merged_using: false,
                    qualified_only: false,
                })
                .collect()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_col_rename_edits_in_expr(
    expr: &Expr,
    view_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    ctes: &CteStore,
    scopes: &[Vec<RenameColBinding>],
    db_set: &DatabaseSet,
    spans: &IdentSpanMap,
    edits: &mut Vec<(TokenSpan, String)>,
) {
    match expr {
        Expr::Column(cref) => {
            if cref.column.eq_ignore_ascii_case(old_col)
                && resolve_cref_is_target_col(cref, scopes) == Some(true)
                && let Some(sp) = spans.span_of(&cref.column)
            {
                edits.push((sp, new_col.to_owned()));
            }
        }
        Expr::Unary { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            collect_col_rename_edits_in_expr(
                inner,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Binary { left, right, .. } => {
            collect_col_rename_edits_in_expr(
                left,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
            collect_col_rename_edits_in_expr(
                right,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            for sub in [e, low, high] {
                collect_col_rename_edits_in_expr(
                    sub,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::InList { expr: e, list, .. } => {
            collect_col_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
            for item in list {
                collect_col_rename_edits_in_expr(
                    item,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            collect_col_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
            let _ = collect_col_rename_edits_in_select(
                subquery,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::InTable { expr: e, .. } => {
            collect_col_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_col_rename_edits_in_expr(
                e,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
            collect_col_rename_edits_in_expr(
                pattern,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
            if let Some(esc) = escape {
                collect_col_rename_edits_in_expr(
                    esc,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_col_rename_edits_in_expr(
                    op,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            for (w, t) in branches {
                collect_col_rename_edits_in_expr(
                    w,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
                collect_col_rename_edits_in_expr(
                    t,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
            if let Some(el) = else_expr {
                collect_col_rename_edits_in_expr(
                    el,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    collect_col_rename_edits_in_expr(
                        e,
                        view_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        ctes,
                        scopes,
                        db_set,
                        spans,
                        edits,
                    );
                }
            }
            if let Some(f) = filter {
                collect_col_rename_edits_in_expr(
                    f,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::RowValue(es) => {
            for e in es {
                collect_col_rename_edits_in_expr(
                    e,
                    view_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    ctes,
                    scopes,
                    db_set,
                    spans,
                    edits,
                );
            }
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            let _ = collect_col_rename_edits_in_select(
                sub,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        Expr::Raise {
            message: Some(m), ..
        } => {
            collect_col_rename_edits_in_expr(
                m,
                view_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                ctes,
                scopes,
                db_set,
                spans,
                edits,
            );
        }
        _ => {}
    }
}

/// Structurally rewrites a trigger definition (`sqlite_schema` / `sqlite_temp_schema`) for
/// `ALTER TABLE tbl RENAME COLUMN old_col TO new_col`, matching `renameColumnFunc` (`sqlite/src/alter.c:1600-1735`).
fn rewrite_trigger_for_column_rename(
    trig_sql: &str,
    trig_db_id: DatabaseId,
    target_db_id: DatabaseId,
    target_tbl: &TableSchema,
    old_col: &str,
    new_col: &str,
    db_set: &DatabaseSet,
) -> Result<Option<String>> {
    let (Some(Stmt::CreateTrigger(ct)), spans, _) = parse_one_with_spans(trig_sql)? else {
        return Ok(None);
    };
    let mut edits: Vec<(TokenSpan, String)> = Vec::new();
    let ctes = CteStore::default();

    let resolved_on: Option<(DatabaseId, ResolvedRelation)> = if trig_db_id != DatabaseId::TEMP {
        let slot = db_set.slot(trig_db_id);
        let schema_ok = slot.is_some_and(|sl| {
            ct.on_table
                .schema
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case(&sl.name))
        });
        if !schema_ok {
            None
        } else if let Some(sl) = slot {
            sl.schema
                .find_user_table(&ct.on_table.name)
                .cloned()
                .map(|t| (trig_db_id, ResolvedRelation::Table(Box::new(t))))
                .or_else(|| {
                    sl.schema
                        .find_view(&ct.on_table.name)
                        .cloned()
                        .map(|v| (trig_db_id, ResolvedRelation::View(Box::new(v))))
                })
        } else {
            None
        }
    } else {
        db_set.find_relation_in_set(&ct.on_table).ok().flatten()
    };

    let (on_is_target_tbl, on_col_names): (bool, Vec<String>) = match resolved_on {
        Some((found_db_id, ResolvedRelation::Table(tbl))) => {
            let is_tgt =
                found_db_id == target_db_id && tbl.name.eq_ignore_ascii_case(&target_tbl.name);
            let cols = tbl.columns.iter().map(|c| c.name.clone()).collect();
            (is_tgt, cols)
        }
        Some((found_db_id, ResolvedRelation::View(v))) => {
            let cols = static_view_scoped_cols(found_db_id, &v, &v.name, db_set, &ctes, &[])
                .unwrap_or_default()
                .into_iter()
                .map(|sc| sc.name)
                .collect();
            (false, cols)
        }
        None => (false, Vec::new()),
    };

    if on_is_target_tbl
        && let TriggerEvent::Update {
            columns: Some(ref ucols),
        } = ct.event
    {
        for uc in ucols {
            if uc.eq_ignore_ascii_case(old_col)
                && let Some(sp) = spans.span_of(uc)
            {
                edits.push((sp, new_col.to_owned()));
            }
        }
    }

    let mut pseudo_cols: Vec<RenameColBinding> = Vec::new();
    let push_pseudo = |out: &mut Vec<RenameColBinding>, alias: &str| {
        for cn in &on_col_names {
            out.push(RenameColBinding {
                schema_name: None,
                table_alias: Some(alias.to_owned()),
                name: cn.clone(),
                is_target_col: on_is_target_tbl && cn.eq_ignore_ascii_case(old_col),
                merged_using: false,
                qualified_only: true,
            });
        }
    };
    match ct.event {
        TriggerEvent::Insert => push_pseudo(&mut pseudo_cols, "new"),
        TriggerEvent::Delete => push_pseudo(&mut pseudo_cols, "old"),
        TriggerEvent::Update { .. } => {
            push_pseudo(&mut pseudo_cols, "old");
            push_pseudo(&mut pseudo_cols, "new");
        }
    }
    let outer_scopes = vec![pseudo_cols];

    if let Some(ref w) = ct.when_clause {
        collect_col_rename_edits_in_expr(
            w,
            trig_db_id,
            target_db_id,
            target_tbl,
            old_col,
            new_col,
            &ctes,
            &outer_scopes,
            db_set,
            &spans,
            &mut edits,
        );
    }

    let resolve_step_dml_target =
        |qn: &QualifiedName, alias: &Option<String>, edits_out: &mut Vec<(TokenSpan, String)>| {
            let (is_tgt, _) = view_qn_resolves_to_target_table(
                qn,
                trig_db_id,
                target_db_id,
                &target_tbl.name,
                &[],
                db_set,
            );
            let synthetic_tos = TableOrSubquery::Table {
                name: qn.clone(),
                table_args: None,
                alias: alias.clone(),
                indexed_by: None,
            };
            let cols = collect_col_rename_edits_in_tos(
                &synthetic_tos,
                trig_db_id,
                target_db_id,
                target_tbl,
                old_col,
                new_col,
                &ctes,
                &outer_scopes,
                db_set,
                &spans,
                edits_out,
            );
            (is_tgt, cols)
        };

    for step in &ct.body {
        match step {
            TriggerStep::Select(sel) => {
                let _ = collect_col_rename_edits_in_select(
                    sel,
                    trig_db_id,
                    target_db_id,
                    target_tbl,
                    old_col,
                    new_col,
                    &ctes,
                    &outer_scopes,
                    db_set,
                    &spans,
                    &mut edits,
                );
            }
            TriggerStep::Insert(ins) => {
                match &ins.source {
                    InsertSource::Values(rows) => {
                        for row in rows {
                            for e in row {
                                collect_col_rename_edits_in_expr(
                                    e,
                                    trig_db_id,
                                    target_db_id,
                                    target_tbl,
                                    old_col,
                                    new_col,
                                    &ctes,
                                    &outer_scopes,
                                    db_set,
                                    &spans,
                                    &mut edits,
                                );
                            }
                        }
                    }
                    InsertSource::Select(sel) => {
                        let _ = collect_col_rename_edits_in_select(
                            sel,
                            trig_db_id,
                            target_db_id,
                            target_tbl,
                            old_col,
                            new_col,
                            &ctes,
                            &outer_scopes,
                            db_set,
                            &spans,
                            &mut edits,
                        );
                    }
                    InsertSource::DefaultValues => {}
                }

                let (step_is_target, dml_cols) =
                    resolve_step_dml_target(&ins.table, &ins.alias, &mut edits);
                if step_is_target && let Some(ref cols) = ins.columns {
                    for cn in cols {
                        if cn.eq_ignore_ascii_case(old_col)
                            && let Some(sp) = spans.span_of(cn)
                        {
                            edits.push((sp, new_col.to_owned()));
                        }
                    }
                }
                if !ins.upsert_clauses.is_empty() {
                    let excl_cols: Vec<RenameColBinding> = dml_cols
                        .iter()
                        .map(|cb| RenameColBinding {
                            schema_name: None,
                            table_alias: Some("excluded".to_owned()),
                            name: cb.name.clone(),
                            is_target_col: cb.is_target_col,
                            merged_using: false,
                            qualified_only: true,
                        })
                        .collect();
                    let mut target_scopes = outer_scopes.clone();
                    target_scopes.push(dml_cols.clone());
                    let mut upsert_set_scopes = outer_scopes.clone();
                    let mut combined = dml_cols;
                    combined.extend(excl_cols);
                    upsert_set_scopes.push(combined);

                    for uc in &ins.upsert_clauses {
                        if let Some(ref tgt) = uc.target {
                            for ic in &tgt.columns {
                                if let Some(ref ie) = ic.expr {
                                    collect_col_rename_edits_in_expr(
                                        ie,
                                        trig_db_id,
                                        target_db_id,
                                        target_tbl,
                                        old_col,
                                        new_col,
                                        &ctes,
                                        &target_scopes,
                                        db_set,
                                        &spans,
                                        &mut edits,
                                    );
                                } else if step_is_target
                                    && ic.name.eq_ignore_ascii_case(old_col)
                                    && let Some(sp) = spans.span_of(&ic.name)
                                {
                                    edits.push((sp, new_col.to_owned()));
                                }
                            }
                            if let Some(ref tw) = tgt.where_clause {
                                collect_col_rename_edits_in_expr(
                                    tw,
                                    trig_db_id,
                                    target_db_id,
                                    target_tbl,
                                    old_col,
                                    new_col,
                                    &ctes,
                                    &target_scopes,
                                    db_set,
                                    &spans,
                                    &mut edits,
                                );
                            }
                        }
                        if let UpsertAction::DoUpdate {
                            ref assignments,
                            ref where_clause,
                        } = uc.action
                        {
                            for asgn in assignments {
                                if step_is_target {
                                    for lhs in &asgn.columns {
                                        if lhs.eq_ignore_ascii_case(old_col)
                                            && let Some(sp) = spans.span_of(lhs)
                                        {
                                            edits.push((sp, new_col.to_owned()));
                                        }
                                    }
                                }
                                collect_col_rename_edits_in_expr(
                                    &asgn.expr,
                                    trig_db_id,
                                    target_db_id,
                                    target_tbl,
                                    old_col,
                                    new_col,
                                    &ctes,
                                    &upsert_set_scopes,
                                    db_set,
                                    &spans,
                                    &mut edits,
                                );
                            }
                            if let Some(uw) = where_clause {
                                collect_col_rename_edits_in_expr(
                                    uw,
                                    trig_db_id,
                                    target_db_id,
                                    target_tbl,
                                    old_col,
                                    new_col,
                                    &ctes,
                                    &upsert_set_scopes,
                                    db_set,
                                    &spans,
                                    &mut edits,
                                );
                            }
                        }
                    }
                }
            }
            TriggerStep::Update(upd) => {
                let (step_is_target, mut step_cols) =
                    resolve_step_dml_target(&upd.table, &upd.alias, &mut edits);
                if step_is_target {
                    for asgn in &upd.assignments {
                        for lhs in &asgn.columns {
                            if lhs.eq_ignore_ascii_case(old_col)
                                && let Some(sp) = spans.span_of(lhs)
                            {
                                edits.push((sp, new_col.to_owned()));
                            }
                        }
                    }
                }
                if let Some(ref fc) = upd.from {
                    let mut from_cols = collect_col_rename_edits_in_tos(
                        &fc.head,
                        trig_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        &ctes,
                        &outer_scopes,
                        db_set,
                        &spans,
                        &mut edits,
                    );
                    for j in &fc.joins {
                        let rcols = collect_col_rename_edits_in_tos(
                            &j.right,
                            trig_db_id,
                            target_db_id,
                            target_tbl,
                            old_col,
                            new_col,
                            &ctes,
                            &outer_scopes,
                            db_set,
                            &spans,
                            &mut edits,
                        );
                        from_cols = merge_join_rename_col_bindings(&from_cols, &rcols, j);
                        if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
                            let mut j_scopes = outer_scopes.clone();
                            j_scopes.push(from_cols.clone());
                            collect_col_rename_edits_in_expr(
                                on_e,
                                trig_db_id,
                                target_db_id,
                                target_tbl,
                                old_col,
                                new_col,
                                &ctes,
                                &j_scopes,
                                db_set,
                                &spans,
                                &mut edits,
                            );
                        }
                    }
                    step_cols.extend(from_cols);
                }
                let mut step_scopes = outer_scopes.clone();
                step_scopes.push(step_cols);
                for asgn in &upd.assignments {
                    collect_col_rename_edits_in_expr(
                        &asgn.expr,
                        trig_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        &ctes,
                        &step_scopes,
                        db_set,
                        &spans,
                        &mut edits,
                    );
                }
                if let Some(ref w) = upd.selection {
                    collect_col_rename_edits_in_expr(
                        w,
                        trig_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        &ctes,
                        &step_scopes,
                        db_set,
                        &spans,
                        &mut edits,
                    );
                }
            }
            TriggerStep::Delete(del) => {
                let (_, step_cols) = resolve_step_dml_target(&del.table, &del.alias, &mut edits);
                let mut step_scopes = outer_scopes.clone();
                step_scopes.push(step_cols);
                if let Some(ref w) = del.selection {
                    collect_col_rename_edits_in_expr(
                        w,
                        trig_db_id,
                        target_db_id,
                        target_tbl,
                        old_col,
                        new_col,
                        &ctes,
                        &step_scopes,
                        db_set,
                        &spans,
                        &mut edits,
                    );
                }
            }
        }
    }

    if edits.is_empty() {
        Ok(None)
    } else {
        for (sp, rep) in &mut edits {
            let orig_tok = &trig_sql[sp.offset..sp.offset + sp.len];
            let mut formatted = format_replacement_ident(orig_tok, new_col);
            if formatted.starts_with('"')
                && trig_sql.as_bytes().get(sp.offset + sp.len) == Some(&b'"')
            {
                formatted.push(' ');
            }
            *rep = formatted;
        }
        Ok(Some(apply_span_edits(trig_sql, edits)))
    }
}

fn rewrite_fk_ref_table_in_create_table_sql(
    sql: &str,
    old_tbl: &str,
    new_tbl: &str,
) -> Result<Option<String>> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    for i in 0..tokens.len() {
        if tokens[i].kind == TokenKind::References
            && i + 1 < tokens.len()
            && token_matches_ident(&tokens[i + 1], old_tbl)
        {
            let t = &tokens[i + 1];
            edits.push((
                t.offset,
                t.text.len(),
                format_renamed_table_ident(sql, t.offset + t.text.len(), new_tbl),
            ));
        }
    }
    if edits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(apply_token_replacements(sql, edits)))
    }
}

fn collect_expr_column_edits(
    tokens: &[Token<'_>],
    start: usize,
    end: usize,
    self_tbl: &str,
    old_col: &str,
    new_col: &str,
    edits: &mut Vec<(usize, usize, String)>,
) {
    let mut i = start;
    while i < end {
        let tok = &tokens[i];
        // Skip `COLLATE <collation_name>`
        if tok.kind == TokenKind::Collate {
            i += 2;
            continue;
        }
        // Skip `CAST(... AS <type_name>)`: when seeing `AS`, skip until matching `)` if inside CAST, or skip alias
        if tok.kind == TokenKind::As {
            i += 2;
            continue;
        }
        // Qualified reference `<qual>.<col>`
        if i + 2 < end && tokens[i + 1].kind == TokenKind::Dot {
            if token_matches_ident(tok, self_tbl) && token_matches_ident(&tokens[i + 2], old_col) {
                let col_tok = &tokens[i + 2];
                edits.push((
                    col_tok.offset,
                    col_tok.text.len(),
                    format_replacement_ident(col_tok.text, new_col),
                ));
            }
            i += 3;
            continue;
        }
        // Skip function calls `<func_name>(`
        if i + 1 < end && tokens[i + 1].kind == TokenKind::Lp && is_ident_like_token(tok.kind) {
            i += 1;
            continue;
        }
        if token_matches_ident(tok, old_col) {
            edits.push((
                tok.offset,
                tok.text.len(),
                format_replacement_ident(tok.text, new_col),
            ));
        }
        i += 1;
    }
}

fn find_matching_rparen(tokens: &[Token<'_>], lp_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(lp_idx) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn collect_paren_col_list_edits(
    tokens: &[Token<'_>],
    lp_idx: usize,
    rp_idx: usize,
    old_col: &str,
    new_col: &str,
    edits: &mut Vec<(usize, usize, String)>,
) {
    // Handles `(col1 [COLLATE c] [ASC|DESC], col2, ...)`
    let mut item_start = true;
    let mut i = lp_idx + 1;
    while i < rp_idx {
        if tokens[i].kind == TokenKind::Comma {
            item_start = true;
            i += 1;
            continue;
        }
        if item_start {
            if token_matches_ident(&tokens[i], old_col) {
                edits.push((
                    tokens[i].offset,
                    tokens[i].text.len(),
                    format_replacement_ident(tokens[i].text, new_col),
                ));
            }
            item_start = false;
        }
        i += 1;
    }
}

fn is_col_constraint_keyword(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Constraint
            | TokenKind::Primary
            | TokenKind::Not
            | TokenKind::Null
            | TokenKind::Unique
            | TokenKind::Check
            | TokenKind::Default
            | TokenKind::Collate
            | TokenKind::References
            | TokenKind::Generated
            | TokenKind::As
    )
}

fn rewrite_column_in_create_table_sql(
    sql: &str,
    self_tbl: &str,
    old_col: &str,
    new_col: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(outer_lp) = tokens.iter().position(|t| t.kind == TokenKind::Lp) else {
        return Ok(sql.to_owned());
    };
    let Some(outer_rp) = find_matching_rparen(&tokens, outer_lp) else {
        return Ok(sql.to_owned());
    };

    // Split top-level items `[item_start..item_end]` inside `( ... )`
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut cur_start = outer_lp + 1;
    for (idx, tok) in tokens.iter().enumerate().take(outer_rp).skip(outer_lp + 1) {
        match tok.kind {
            TokenKind::Lp => depth += 1,
            TokenKind::Rp => depth = depth.saturating_sub(1),
            TokenKind::Comma if depth == 0 => {
                if cur_start < idx {
                    items.push((cur_start, idx));
                }
                cur_start = idx + 1;
            }
            _ => {}
        }
    }
    if cur_start < outer_rp {
        items.push((cur_start, outer_rp));
    }

    let mut edits = Vec::new();

    for (istart, iend) in items {
        let mut p = istart;
        // Check if table-level constraint (`[CONSTRAINT <name>] PRIMARY|UNIQUE|CHECK|FOREIGN`)
        let is_table_cons = if tokens[p].kind == TokenKind::Constraint && p + 2 < iend {
            matches!(
                tokens[p + 2].kind,
                TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
            )
        } else {
            matches!(
                tokens[p].kind,
                TokenKind::Primary | TokenKind::Unique | TokenKind::Check | TokenKind::Foreign
            )
        };

        if is_table_cons {
            if tokens[p].kind == TokenKind::Constraint {
                p += 2;
            }
            match tokens[p].kind {
                TokenKind::Primary | TokenKind::Unique => {
                    if let Some(rel_lp) =
                        tokens[p..iend].iter().position(|t| t.kind == TokenKind::Lp)
                    {
                        let lp = p + rel_lp;
                        if let Some(rp) = find_matching_rparen(&tokens, lp) {
                            collect_paren_col_list_edits(
                                &tokens, lp, rp, old_col, new_col, &mut edits,
                            );
                        }
                    }
                }
                TokenKind::Check => {
                    if p + 1 < iend
                        && tokens[p + 1].kind == TokenKind::Lp
                        && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                    {
                        collect_expr_column_edits(
                            &tokens,
                            p + 2,
                            rp,
                            self_tbl,
                            old_col,
                            new_col,
                            &mut edits,
                        );
                    }
                }
                TokenKind::Foreign => {
                    // `FOREIGN KEY ( <from_cols> ) REFERENCES <ref_tbl> [ ( <ref_cols> ) ]`
                    if let Some(rel_lp) =
                        tokens[p..iend].iter().position(|t| t.kind == TokenKind::Lp)
                    {
                        let lp = p + rel_lp;
                        if let Some(rp) = find_matching_rparen(&tokens, lp) {
                            collect_paren_col_list_edits(
                                &tokens, lp, rp, old_col, new_col, &mut edits,
                            );
                            let mut r = rp + 1;
                            while r < iend && tokens[r].kind != TokenKind::References {
                                r += 1;
                            }
                            if r + 1 < iend
                                && token_matches_ident(&tokens[r + 1], self_tbl)
                                && r + 2 < iend
                                && tokens[r + 2].kind == TokenKind::Lp
                                && let Some(ref_rp) = find_matching_rparen(&tokens, r + 2)
                            {
                                collect_paren_col_list_edits(
                                    &tokens,
                                    r + 2,
                                    ref_rp,
                                    old_col,
                                    new_col,
                                    &mut edits,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Column definition: `<col_name> [type_tokens...] [column_constraints...]`
            if token_matches_ident(&tokens[p], old_col) {
                edits.push((
                    tokens[p].offset,
                    tokens[p].text.len(),
                    format_replacement_ident(tokens[p].text, new_col),
                ));
            }
            p += 1;
            // Skip declared type name (including optional `(len, scale)`)
            while p < iend && !is_col_constraint_keyword(tokens[p].kind) {
                if tokens[p].kind == TokenKind::Lp
                    && let Some(rp) = find_matching_rparen(&tokens, p)
                {
                    p = rp + 1;
                } else {
                    p += 1;
                }
            }
            // Scan column constraints for `CHECK(...)` or self-referencing `REFERENCES self_tbl(...)`
            while p < iend {
                match tokens[p].kind {
                    TokenKind::Constraint => {
                        p += 2;
                    }
                    TokenKind::Check => {
                        if p + 1 < iend
                            && tokens[p + 1].kind == TokenKind::Lp
                            && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                        {
                            collect_expr_column_edits(
                                &tokens,
                                p + 2,
                                rp,
                                self_tbl,
                                old_col,
                                new_col,
                                &mut edits,
                            );
                            p = rp + 1;
                        } else {
                            p += 1;
                        }
                    }
                    TokenKind::Default => {
                        if p + 1 < iend
                            && tokens[p + 1].kind == TokenKind::Lp
                            && let Some(rp) = find_matching_rparen(&tokens, p + 1)
                        {
                            p = rp + 1;
                        } else {
                            p += 2;
                        }
                    }
                    TokenKind::References => {
                        if p + 1 < iend && token_matches_ident(&tokens[p + 1], self_tbl) {
                            if p + 2 < iend
                                && tokens[p + 2].kind == TokenKind::Lp
                                && let Some(ref_rp) = find_matching_rparen(&tokens, p + 2)
                            {
                                collect_paren_col_list_edits(
                                    &tokens,
                                    p + 2,
                                    ref_rp,
                                    old_col,
                                    new_col,
                                    &mut edits,
                                );
                                p = ref_rp + 1;
                                continue;
                            }
                        } else if p + 2 < iend
                            && tokens[p + 2].kind == TokenKind::Lp
                            && let Some(ref_rp) = find_matching_rparen(&tokens, p + 2)
                        {
                            p = ref_rp + 1;
                            continue;
                        }
                        p += 2;
                    }
                    _ => {
                        p += 1;
                    }
                }
            }
        }
    }

    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_column_in_create_index_sql(
    sql: &str,
    tbl_name: &str,
    old_col: &str,
    new_col: &str,
) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(on_pos) = tokens.iter().position(|t| t.kind == TokenKind::On) else {
        return Ok(sql.to_owned());
    };
    let Some(rel_lp) = tokens[on_pos..]
        .iter()
        .position(|t| t.kind == TokenKind::Lp)
    else {
        return Ok(sql.to_owned());
    };
    let lp = on_pos + rel_lp;
    let Some(rp) = find_matching_rparen(&tokens, lp) else {
        return Ok(sql.to_owned());
    };

    let mut edits = Vec::new();
    collect_paren_col_list_edits(&tokens, lp, rp, old_col, new_col, &mut edits);

    // Also rewrite `WHERE <expr>` on partial indexes if present
    if let Some(rel_where) = tokens[rp + 1..]
        .iter()
        .position(|t| t.kind == TokenKind::Where)
    {
        let where_idx = rp + 1 + rel_where;
        let end_idx = tokens
            .iter()
            .position(|t| matches!(t.kind, TokenKind::Semi | TokenKind::Eof))
            .unwrap_or(tokens.len());
        if where_idx + 1 < end_idx {
            collect_expr_column_edits(
                &tokens,
                where_idx + 1,
                end_idx,
                tbl_name,
                old_col,
                new_col,
                &mut edits,
            );
        }
    }

    Ok(apply_token_replacements(sql, edits))
}

fn rewrite_fk_ref_column_in_create_table_sql(
    sql: &str,
    target_tbl: &str,
    old_col: &str,
    new_col: &str,
) -> Result<Option<String>> {
    let tokens = tokenize(sql)?;
    let mut edits = Vec::new();
    let mut i = 0usize;
    while i + 2 < tokens.len() {
        if tokens[i].kind == TokenKind::References
            && token_matches_ident(&tokens[i + 1], target_tbl)
            && tokens[i + 2].kind == TokenKind::Lp
            && let Some(rp) = find_matching_rparen(&tokens, i + 2)
        {
            collect_paren_col_list_edits(&tokens, i + 2, rp, old_col, new_col, &mut edits);
            i = rp + 1;
            continue;
        }
        i += 1;
    }
    if edits.is_empty() {
        Ok(None)
    } else {
        Ok(Some(apply_token_replacements(sql, edits)))
    }
}

fn render_column_def_sql(cdef: &sqliters_syntax::ColumnDef) -> String {
    let mut parts = vec![format_replacement_ident("", &cdef.name)];
    if !cdef.type_name.is_empty() {
        parts.push(cdef.type_name.clone());
    }
    for cons in &cdef.constraints {
        match cons {
            sqliters_syntax::ColumnConstraint::NotNull { conflict } => {
                let mut s = "NOT NULL".to_owned();
                if *conflict != ConflictAction::Abort {
                    s.push_str(&format!(" ON CONFLICT {}", format_conflict_kw(*conflict)));
                }
                parts.push(s);
            }
            sqliters_syntax::ColumnConstraint::Null => parts.push("NULL".to_owned()),
            sqliters_syntax::ColumnConstraint::Default(e) => {
                parts.push(format!("DEFAULT ({})", expr_column_label(e)));
            }
            sqliters_syntax::ColumnConstraint::Check { name, expr_sql, .. } => {
                if let Some(n) = name {
                    parts.push(format!(
                        "CONSTRAINT {} CHECK ({expr_sql})",
                        format_replacement_ident("", n)
                    ));
                } else {
                    parts.push(format!("CHECK ({expr_sql})"));
                }
            }
            sqliters_syntax::ColumnConstraint::Generated { expr, stored } => {
                parts.push(format!(
                    "GENERATED ALWAYS AS ({}) {}",
                    expr_column_label(expr),
                    if *stored { "STORED" } else { "VIRTUAL" }
                ));
            }
            sqliters_syntax::ColumnConstraint::Collate(c) => parts.push(format!("COLLATE {c}")),
            sqliters_syntax::ColumnConstraint::ForeignKey {
                ref_table,
                ref_columns,
                on_delete,
                on_update,
                deferred,
            } => {
                let mut fk_s = format!("REFERENCES {}", format_replacement_ident("", ref_table));
                if !ref_columns.is_empty() {
                    let cols: Vec<String> = ref_columns
                        .iter()
                        .map(|c| format_replacement_ident("", c))
                        .collect();
                    fk_s.push_str(&format!("({})", cols.join(", ")));
                }
                if *on_delete != FkAction::NoAction {
                    fk_s.push_str(&format!(" ON DELETE {}", on_delete.as_sql_str()));
                }
                if *on_update != FkAction::NoAction {
                    fk_s.push_str(&format!(" ON UPDATE {}", on_update.as_sql_str()));
                }
                if *deferred {
                    fk_s.push_str(" DEFERRABLE INITIALLY DEFERRED");
                }
                parts.push(fk_s);
            }
            _ => {}
        }
    }
    parts.join(" ")
}

fn format_conflict_kw(c: ConflictAction) -> &'static str {
    match c {
        ConflictAction::Abort => "ABORT",
        ConflictAction::Rollback => "ROLLBACK",
        ConflictAction::Fail => "FAIL",
        ConflictAction::Ignore => "IGNORE",
        ConflictAction::Replace => "REPLACE",
    }
}

fn append_column_to_create_table_sql(sql: &str, rendered_col: &str) -> Result<String> {
    let tokens = tokenize(sql)?;
    let Some(outer_lp) = tokens.iter().position(|t| t.kind == TokenKind::Lp) else {
        return Err(SqliteError::corrupt("CREATE TABLE missing opening '('"));
    };
    let Some(outer_rp_idx) = find_matching_rparen(&tokens, outer_lp) else {
        return Err(SqliteError::corrupt("CREATE TABLE missing closing ')'"));
    };

    // Match C SQLite `addColOffset` (`sqlite/src/parse.y:223,461`, `sqlite/src/alter.c:442-449`):
    // Insert right before the comma that introduces the first top-level table constraint
    // (`CONSTRAINT | PRIMARY | UNIQUE | CHECK | FOREIGN`), or before the closing `)`
    // if there are no table-level constraints.
    let mut insert_offset = tokens[outer_rp_idx].offset;
    let mut depth = 0usize;
    let mut last_comma_idx: Option<usize> = None;
    let mut item_start = true;
    for idx in (outer_lp + 1)..outer_rp_idx {
        match tokens[idx].kind {
            TokenKind::Lp => {
                if depth == 0 {
                    item_start = false;
                }
                depth += 1;
            }
            TokenKind::Rp => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Comma if depth == 0 => {
                last_comma_idx = Some(idx);
                item_start = true;
            }
            kind if depth == 0 && item_start => {
                if matches!(
                    kind,
                    TokenKind::Constraint
                        | TokenKind::Primary
                        | TokenKind::Unique
                        | TokenKind::Check
                        | TokenKind::Foreign
                ) {
                    insert_offset = last_comma_idx
                        .map(|c| tokens[c].offset)
                        .unwrap_or(tokens[idx].offset);
                    break;
                }
                item_start = false;
            }
            _ => {}
        }
    }

    let mut out = String::with_capacity(sql.len() + rendered_col.len() + 4);
    out.push_str(sql[..insert_offset].trim_end());
    out.push_str(", ");
    out.push_str(rendered_col);
    out.push_str(&sql[insert_offset..]);
    Ok(out)
}

// ============================================================================
// `VACUUM` & `PRAGMA` Execution (`vacuum.c`, `pragma.c`)
// ============================================================================

pub(crate) enum VacuumFail {
    Clean(SqliteError),
    Poison(SqliteError),
}

pub(crate) fn exec_vacuum(
    ctx: &mut ExecContext<'_>,
    vac_schema: Option<&str>,
    into: Option<&Expr>,
    params: &[Value],
) -> Result<QueryOutput> {
    exec_vacuum_checked(ctx, vac_schema, into, params).map_err(|vf| match vf {
        VacuumFail::Clean(e) | VacuumFail::Poison(e) => e,
    })
}

fn compute_effective_vacuum_page_size(base_ps: u32, requested: Option<u32>, reserve: u8) -> u32 {
    let Some(mut req) = requested else {
        return base_ps;
    };
    if !sqliters_pager::DatabaseHeader::is_valid_page_size(req) {
        return base_ps;
    }
    if req == 512 && reserve > 32 {
        req = 1024;
    }
    if req.saturating_sub(u32::from(reserve)) >= 480 {
        req
    } else {
        base_ps
    }
}

fn build_vacuum_image(
    ctx: &mut ExecContext<'_>,
    src_db_id: DatabaseId,
    orig_hdr: &sqliters_pager::DatabaseHeader,
    target_btree: &mut Btree,
) -> Result<()> {
    target_btree.begin_write()?;
    {
        let fhdr = target_btree.pager_mut().header_mut()?;
        fhdr.default_cache_size = orig_hdr.default_cache_size;
        fhdr.user_version = orig_hdr.user_version;
        fhdr.application_id = orig_hdr.application_id;
        fhdr.text_encoding = orig_hdr.text_encoding;
        fhdr.schema_cookie = orig_hdr.schema_cookie.wrapping_add(1);
    }

    let (_, schema_name) = slot_encoding_and_name(ctx, src_db_id);
    let (tables_snapshot, indexes_snapshot, views_snapshot, triggers_snapshot) = {
        let slot = ctx
            .db_set
            .slot(src_db_id)
            .ok_or_else(|| SqliteError::misuse("vacuum source slot closed"))?;
        (
            slot.schema.tables.clone(),
            slot.schema.indexes.clone(),
            slot.schema.views.clone(),
            slot.schema.triggers.clone(),
        )
    };
    let has_any_autoincrement = tables_snapshot
        .iter()
        .any(|t| !t.name.eq_ignore_ascii_case("sqlite_sequence") && t.autoincrement);

    // Phase A: Allocate roots & insert `sqlite_schema` entries for all tables FIRST (`vacuum.c:298-307`)!
    let mut mapped_tables: Vec<(TableSchema, Pgno)> = Vec::with_capacity(tables_snapshot.len());
    for tbl in tables_snapshot {
        if tbl.name.eq_ignore_ascii_case("sqlite_sequence") {
            if !has_any_autoincrement {
                // Omit orphaned `sqlite_sequence` when no `AUTOINCREMENT` table remains (`vacuum2-1.1, 3.1`)!
                continue;
            }
            let seq_root = target_btree.create_table()?;
            insert_schema_entry(
                target_btree,
                "table",
                &tbl.name,
                &tbl.name,
                seq_root,
                Some(&tbl.raw_sql),
            )?;
            mapped_tables.push((tbl, seq_root));
        } else {
            let new_tbl_root = if tbl.without_rowid {
                target_btree.create_index()?
            } else {
                target_btree.create_table()?
            };
            insert_schema_entry(
                target_btree,
                "table",
                &tbl.name,
                &tbl.name,
                new_tbl_root,
                Some(&tbl.raw_sql),
            )?;
            mapped_tables.push((tbl, new_tbl_root));
        }
    }

    // Phase B: Allocate roots & insert `sqlite_schema` entries for all indexes (`vacuum.c:309-315`)!
    let mut mapped_indexes: Vec<(IndexSchema, Pgno)> = Vec::with_capacity(indexes_snapshot.len());
    for idx in indexes_snapshot {
        let new_idx_root = target_btree.create_index()?;
        insert_schema_entry(
            target_btree,
            "index",
            &idx.name,
            &idx.table_name,
            new_idx_root,
            idx.raw_sql.as_deref(),
        )?;
        mapped_indexes.push((idx, new_idx_root));
    }

    // Phase B2: Preserve all permanent views and triggers (`rootpage = 0`) in `schema_rowid` order (`vacuum.c:333-339`)!
    let mut non_storage_entries: Vec<(i64, &'static str, String, String, String)> =
        Vec::with_capacity(views_snapshot.len() + triggers_snapshot.len());
    for view in views_snapshot {
        non_storage_entries.push((
            view.schema_rowid,
            "view",
            view.name.clone(),
            view.name,
            view.raw_sql,
        ));
    }
    for trig in triggers_snapshot {
        non_storage_entries.push((
            trig.schema_rowid,
            "trigger",
            trig.name,
            trig.tbl_name,
            trig.raw_sql,
        ));
    }
    non_storage_entries.sort_by_key(|e| e.0);
    for (_, ty, obj_name, tbl_name, sql) in non_storage_entries {
        insert_schema_entry(target_btree, ty, &obj_name, &tbl_name, 0, Some(&sql))?;
    }

    // Phase C: Copy table rows & populate indexes (`vacuum.c:317-323`)!
    for (tbl, new_tbl_root) in &mapped_tables {
        let tbl_indexes: Vec<(IndexSchema, Pgno)> = mapped_indexes
            .iter()
            .filter(|(idx, _)| idx.table_name.eq_ignore_ascii_case(&tbl.name))
            .cloned()
            .collect();

        if tbl.name.eq_ignore_ascii_case("sqlite_sequence") {
            let rows = {
                let src_bt = ctx
                    .db_set
                    .slot_mut(src_db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                scan_table_incremental(src_bt, tbl.root_pgno)?
            };
            let mut next_rid = 0i64;
            for (_, payload) in rows {
                next_rid += 1;
                target_btree.insert_table(*new_tbl_root, next_rid, &payload)?;
            }
        } else if tbl.without_rowid {
            let pk_orders = tbl.without_rowid_pk_orders();
            let entries = {
                let src_bt = ctx
                    .db_set
                    .slot_mut(src_db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                scan_index_incremental(src_bt, tbl.root_pgno, pk_orders.clone())?
            };
            for rec in entries {
                target_btree.insert_index(*new_tbl_root, &rec, &pk_orders)?;
            }
            for (idx, new_idx_root) in &tbl_indexes {
                let orders = idx.key_sort_orders();
                let idx_entries = {
                    let src_bt = ctx
                        .db_set
                        .slot_mut(src_db_id)
                        .and_then(|s| s.btree.as_mut())
                        .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                    scan_index_incremental(src_bt, idx.root_pgno, orders.clone())?
                };
                for rec in idx_entries {
                    target_btree.insert_index(*new_idx_root, &rec, &orders)?;
                }
            }
        } else if tbl.rowid_alias_col.is_some() {
            // Explicit `INTEGER PRIMARY KEY`: preserve exact rowids!
            let rows = {
                let src_bt = ctx
                    .db_set
                    .slot_mut(src_db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                scan_table_incremental(src_bt, tbl.root_pgno)?
            };
            for (rid, payload) in rows {
                target_btree.insert_table(*new_tbl_root, rid, &payload)?;
            }
            for (idx, new_idx_root) in &tbl_indexes {
                let orders = idx.key_sort_orders();
                let idx_entries = {
                    let src_bt = ctx
                        .db_set
                        .slot_mut(src_db_id)
                        .and_then(|s| s.btree.as_mut())
                        .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                    scan_index_incremental(src_bt, idx.root_pgno, orders.clone())?
                };
                for rec in idx_entries {
                    target_btree.insert_index(*new_idx_root, &rec, &orders)?;
                }
            }
        } else {
            // Implicit rowid table without `INTEGER PRIMARY KEY`: reassign sequential rowids `1..=N`
            // and rebuild secondary/unique index entries against the compacted rowids!
            let rows = {
                let src_bt = ctx
                    .db_set
                    .slot_mut(src_db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("vacuum source btree closed"))?;
                scan_table_incremental(src_bt, tbl.root_pgno)?
            };
            let mut next_rid = 0i64;
            for (_, payload) in rows {
                next_rid += 1;
                target_btree.insert_table(*new_tbl_root, next_rid, &payload)?;
                if !tbl_indexes.is_empty() {
                    let new_rkey = TableRowKey::Rowid(next_rid);
                    let vals =
                        decode_table_logical_row(ctx, src_db_id, tbl, &new_rkey, &payload, &[])?;
                    for (idx_schema, new_idx_root) in &tbl_indexes {
                        if !index_row_passes_partial_where(
                            ctx,
                            &schema_name,
                            tbl,
                            idx_schema,
                            next_rid,
                            &vals,
                            &[],
                        )? {
                            continue;
                        }
                        let keys = eval_index_key_values(
                            ctx,
                            &schema_name,
                            tbl,
                            idx_schema,
                            &new_rkey,
                            &vals,
                            &[],
                        )?;
                        let orders = idx_schema.key_sort_orders();
                        let idx_rec = encode_record_with_encoding(&keys, target_btree.encoding());
                        target_btree.insert_index(*new_idx_root, &idx_rec, &orders)?;
                    }
                }
            }
        }
    }

    Ok(())
}

pub(crate) fn exec_vacuum_checked(
    ctx: &mut ExecContext<'_>,
    vac_schema: Option<&str>,
    into: Option<&Expr>,
    params: &[Value],
) -> std::result::Result<QueryOutput, VacuumFail> {
    if !is_in_autocommit(ctx) {
        return Err(VacuumFail::Clean(SqliteError::new(
            ErrorCode::Error,
            "cannot VACUUM from within a transaction",
        )));
    }
    if ctx.activity.other_active > 0 {
        return Err(VacuumFail::Clean(SqliteError::new(
            ErrorCode::Error,
            "cannot VACUUM - SQL statements in progress",
        )));
    }
    let db_id = ctx
        .db_set
        .resolve_schema_or_err(vac_schema)
        .map_err(VacuumFail::Clean)?;

    if db_id == DatabaseId::TEMP && !ctx.db_set.is_temp_open() {
        if into.is_none() {
            return Ok(QueryOutput::default());
        }
        let enc = ctx.encoding();
        ctx.db_set
            .ensure_temp_open(enc, *ctx.writable_schema)
            .map_err(VacuumFail::Clean)?;
    }

    if let Some(into_expr) = into {
        let opened_read = {
            let slot = ctx
                .db_set
                .slot_mut(db_id)
                .ok_or_else(|| VacuumFail::Clean(SqliteError::misuse("vacuum slot closed")))?;
            let bt = slot
                .btree
                .as_mut()
                .ok_or_else(|| VacuumFail::Clean(SqliteError::misuse("vacuum btree closed")))?;
            if bt.transaction_state() == TransactionState::Open {
                ctx.busy_policy
                    .retry_btree(bt, |b| b.begin_read())
                    .map_err(VacuumFail::Clean)?;
                true
            } else {
                false
            }
        };

        let into_res: Result<()> = (|| {
            {
                let slot = ctx
                    .db_set
                    .slot_mut(db_id)
                    .ok_or_else(|| SqliteError::misuse("vacuum slot closed"))?;
                let slot_name = slot.name.clone();
                let bt = slot
                    .btree
                    .as_mut()
                    .ok_or_else(|| SqliteError::misuse("vacuum btree closed"))?;
                slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name)?;
            }
            let out_val = eval_expr(ctx, into_expr, params, &[], &CteStore::default(), None)?;
            let Value::Text(out_name) = out_val else {
                return Err(SqliteError::new(ErrorCode::Error, "non-text filename"));
            };

            let (
                orig_hdr,
                lock_geometry,
                src_ps,
                is_src_memdb,
                src_db_kind,
                next_ps,
                target_autovac,
                src_sync,
                src_vfs,
            ) = {
                let slot = ctx
                    .db_set
                    .slot(db_id)
                    .ok_or_else(|| SqliteError::misuse("vacuum slot closed"))?;
                let bt = slot
                    .btree
                    .as_ref()
                    .ok_or_else(|| SqliteError::misuse("vacuum btree closed"))?;
                (
                    bt.pager().header().clone(),
                    bt.pager().lock_geometry(),
                    bt.pager().page_size(),
                    bt.pager().is_pager_memdb(),
                    bt.pager().db_kind(),
                    slot.next_page_size,
                    slot.next_auto_vacuum
                        .unwrap_or_else(|| bt.auto_vacuum_mode()),
                    bt.pager().synchronous(),
                    Arc::clone(bt.pager().vfs()),
                )
            };

            let mut target_is_memory = out_name.is_empty() || out_name == ":memory:";
            let mut target_reserve = orig_hdr.reserved_bytes;
            let mut raw_target_path: &str = out_name.as_str();

            if let Some(rest) = out_name.strip_prefix("file:") {
                let (rp, query_opt) = match rest.split_once('?') {
                    Some((p, q)) => (p, Some(q)),
                    None => (rest, None),
                };
                raw_target_path = rp;
                if rp.is_empty() || rp == ":memory:" {
                    target_is_memory = true;
                }
                if let Some(q) = query_opt {
                    for pair in q.split('&') {
                        if let Some((k, v)) = pair.split_once('=') {
                            if k == "mode" && v.eq_ignore_ascii_case("memory") {
                                target_is_memory = true;
                            } else if k == "reserve"
                                && let Ok(r) = v.parse::<i64>()
                                && (0..=255).contains(&r)
                            {
                                target_reserve = r as u8;
                            }
                        }
                    }
                }
            }

            let target_ps = compute_effective_vacuum_page_size(
                src_ps,
                if is_src_memdb { None } else { next_ps },
                target_reserve,
            );

            if target_is_memory {
                let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(lock_geometry));
                let opts = PagerOpenOptions {
                    default_page_size: target_ps,
                    lock_geometry: Some(lock_geometry),
                    db_kind: Some(DatabaseKind::Memory),
                    ..PagerOpenOptions::default()
                };
                let mut target_bt = Btree::open_with_options(
                    mem_vfs,
                    Some(Path::new(":memory:")),
                    OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
                    opts,
                )?;
                target_bt.pager_mut().set_page_size(target_ps)?;
                target_bt.pager_mut().set_reserved_bytes(target_reserve)?;
                target_bt.pager_mut().set_encoding(orig_hdr.encoding())?;
                let _ = target_bt.set_configured_auto_vacuum(target_autovac);
                build_vacuum_image(ctx, db_id, &orig_hdr, &mut target_bt)?;
                target_bt.commit_retaining_read(false)?;
                return Ok(());
            }

            let resolved_path = {
                let p = Path::new(raw_target_path);
                if p.is_relative() {
                    if let Some(ref wd) = *ctx.working_dir {
                        wd.join(p)
                    } else {
                        p.to_path_buf()
                    }
                } else {
                    p.to_path_buf()
                }
            };

            let target_vfs: Arc<dyn Vfs> = if is_src_memdb || src_db_kind != DatabaseKind::NamedDisk
            {
                let dv = Arc::clone(ctx.db_set.disk_vfs());
                if dv.name() == "memory" {
                    Arc::new(OsVfs::with_lock_geometry(lock_geometry))
                } else {
                    dv
                }
            } else {
                src_vfs
            };
            let is_os_vfs = matches!(target_vfs.name(), "unix" | "win32");
            if is_os_vfs && std::fs::metadata(&resolved_path).is_ok_and(|m| m.is_dir()) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "output file already exists",
                ));
            }
            let full_target = target_vfs.full_pathname(&resolved_path)?;
            if is_os_vfs
                && let Ok(meta) = std::fs::metadata(&full_target)
                && (meta.is_dir() || meta.len() > 0)
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "output file already exists",
                ));
            }
            let existed_before = target_vfs.exists(&full_target)?;
            if !is_os_vfs && existed_before {
                let existing_f = target_vfs
                    .open(Some(&full_target), OpenFlags::READONLY | OpenFlags::MAIN_DB)?;
                let existing_len = existing_f.file_size()?;
                drop(existing_f);
                if existing_len > 0 {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "output file already exists",
                    ));
                }
            }

            let opts = PagerOpenOptions {
                default_page_size: target_ps,
                lock_geometry: Some(lock_geometry),
                db_kind: Some(DatabaseKind::NamedDisk),
                ..PagerOpenOptions::default()
            };
            let mut target_bt = Btree::open_with_options(
                Arc::clone(&target_vfs),
                Some(&full_target),
                OpenFlags::READWRITE | OpenFlags::CREATE,
                opts,
            )?;
            let build_res = (|| -> Result<()> {
                target_bt.pager_mut().set_page_size(target_ps)?;
                target_bt.pager_mut().set_reserved_bytes(target_reserve)?;
                target_bt.pager_mut().set_encoding(orig_hdr.encoding())?;
                target_bt.pager_mut().set_synchronous(src_sync);
                let _ = target_bt.set_configured_auto_vacuum(target_autovac);
                build_vacuum_image(ctx, db_id, &orig_hdr, &mut target_bt)?;
                target_bt.commit_retaining_read(false)?;
                Ok(())
            })();
            drop(target_bt);
            if let Err(e) = build_res {
                if !existed_before {
                    let _ = target_vfs.delete(&full_target, false);
                }
                return Err(e);
            }
            Ok(())
        })();

        if let Some(slot) = ctx.db_set.slot_mut(db_id) {
            slot.next_page_size = None;
            if opened_read && let Some(ref mut bt) = slot.btree {
                let _ = bt.pager_mut().end_read_if_unused();
            }
        }
        return into_res
            .map(|()| QueryOutput::default())
            .map_err(VacuumFail::Clean);
    }

    // In-place `VACUUM [schema]` (`into.is_none()`):
    if db_id == DatabaseId::TEMP {
        if let Some(slot) = ctx.db_set.slot_mut(DatabaseId::TEMP) {
            slot.next_page_size = None;
        }
        return Ok(QueryOutput::default());
    }

    {
        let slot = ctx
            .db_set
            .slot_mut(db_id)
            .ok_or_else(|| VacuumFail::Clean(SqliteError::misuse("vacuum slot closed")))?;
        let slot_name = slot.name.clone();
        let bt = slot
            .btree
            .as_mut()
            .ok_or_else(|| VacuumFail::Clean(SqliteError::misuse("vacuum btree closed")))?;
        ctx.busy_policy
            .retry_btree(bt, |b| b.begin_write())
            .map_err(VacuumFail::Clean)?;
        if let Err(e) = slot.schema.ensure_fresh_in_tx_for_schema(bt, &slot_name) {
            slot.next_page_size = None;
            if let Err(rb_err) = bt.rollback() {
                return Err(VacuumFail::Poison(rb_err));
            }
            return Err(VacuumFail::Clean(e));
        }
        let is_wal_db = bt.journal_mode() == JournalMode::Wal || bt.pager().is_wal_mode();
        if is_wal_db {
            slot.next_page_size = None;
        }
    }

    let res = (|| -> Result<Schema> {
        let (src_ps, is_src_memdb, is_wal_db, lock_geometry, orig_hdr, next_ps, target_autovac) = {
            let slot = ctx
                .db_set
                .slot(db_id)
                .ok_or_else(|| SqliteError::misuse("vacuum slot closed"))?;
            let bt = slot
                .btree
                .as_ref()
                .ok_or_else(|| SqliteError::misuse("vacuum btree closed"))?;
            (
                bt.pager().page_size(),
                bt.pager().is_pager_memdb(),
                bt.journal_mode() == JournalMode::Wal || bt.pager().is_wal_mode(),
                bt.pager().lock_geometry(),
                bt.pager().header().clone(),
                slot.next_page_size,
                slot.next_auto_vacuum
                    .unwrap_or_else(|| bt.auto_vacuum_mode()),
            )
        };

        let target_ps = compute_effective_vacuum_page_size(
            src_ps,
            if is_wal_db || is_src_memdb {
                None
            } else {
                next_ps
            },
            orig_hdr.reserved_bytes,
        );

        let mem_vfs: Arc<dyn Vfs> = Arc::new(MemoryVfs::with_lock_geometry(lock_geometry));
        let fresh_opts = PagerOpenOptions {
            default_page_size: target_ps,
            lock_geometry: Some(lock_geometry),
            db_kind: Some(DatabaseKind::Memory),
            ..PagerOpenOptions::default()
        };
        let mut fresh = Btree::open_with_options(
            mem_vfs,
            None,
            OpenFlags::READWRITE | OpenFlags::CREATE | OpenFlags::MEMORY,
            fresh_opts,
        )?;
        fresh.pager_mut().set_page_size(target_ps)?;
        fresh
            .pager_mut()
            .set_reserved_bytes(orig_hdr.reserved_bytes)?;
        fresh.pager_mut().set_encoding(orig_hdr.encoding())?;
        let _ = fresh.set_configured_auto_vacuum(target_autovac);

        build_vacuum_image(ctx, db_id, &orig_hdr, &mut fresh)?;
        fresh.commit_retaining_read(true)?;
        let vac_schema_name = ctx
            .db_set
            .slot(db_id)
            .map_or("main", |s| s.name.as_str())
            .to_owned();
        let rebuilt_schema = Schema::load_for_schema(&mut fresh, Some(&vac_schema_name))?;

        let dst_bt = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .ok_or_else(|| SqliteError::misuse("vacuum slot closed"))?;
        dst_bt.pager_mut().copy_pages_from(fresh.pager_mut())?;
        Ok(rebuilt_schema)
    })();

    if let Some(slot) = ctx.db_set.slot_mut(db_id) {
        slot.next_page_size = None;
    }
    let new_schema = match res {
        Ok(s) => s,
        Err(e) => {
            if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                && let Err(rb_err) = bt.rollback()
            {
                return Err(VacuumFail::Poison(rb_err));
            }
            return Err(VacuumFail::Clean(e));
        }
    };
    {
        let bt = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .ok_or_else(|| VacuumFail::Clean(SqliteError::misuse("vacuum btree closed")))?;
        if let Err(cm_err) = bt.commit_retaining_read(false) {
            if let Err(rb_err) = bt.rollback() {
                let root = if cm_err.code == ErrorCode::Busy {
                    rb_err
                } else {
                    cm_err
                };
                return Err(VacuumFail::Poison(root));
            }
            return Err(VacuumFail::Clean(cm_err));
        }
    }
    if let Some(slot) = ctx.db_set.slot_mut(db_id) {
        slot.schema = new_schema;
    }
    *ctx.schema_generation += 1;
    *ctx.v1_expire_generation += 1;
    Ok(QueryOutput::default())
}

fn apply_journal_mode_on_slot(
    ctx: &mut ExecContext<'_>,
    db_id: DatabaseId,
    target_opt: Option<JournalMode>,
) -> Result<JournalMode> {
    if db_id == DatabaseId::TEMP && !ctx.db_set.is_temp_open() {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }
    {
        let bt = ctx
            .db_set
            .slot_mut(db_id)
            .and_then(|s| s.btree.as_mut())
            .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
        if bt.pager().transaction_state() == TransactionState::Open
            && !bt.pager().is_header_loaded()
            && bt.pager().db_kind() == DatabaseKind::NamedDisk
        {
            ctx.busy_policy
                .retry_btree(bt, |b| b.pager_mut().ensure_header_loaded().map(|_| ()))?;
        }
    }

    let Some(mut target) = target_opt else {
        let cur = ctx
            .db_set
            .slot(db_id)
            .and_then(|s| s.btree.as_ref())
            .map_or(JournalMode::Delete, |bt| bt.pager().journal_mode());
        return Ok(cur);
    };

    if db_id == DatabaseId::TEMP {
        match target {
            JournalMode::Wal => {
                let cur = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(JournalMode::Delete, |bt| bt.pager().journal_mode());
                return Ok(cur);
            }
            JournalMode::Truncate | JournalMode::Persist => {
                target = JournalMode::Delete;
            }
            _ => {}
        }
    }

    let disallow_wal_transition = !is_in_autocommit(ctx) || ctx.activity.other_readers > 0;
    let bt = ctx
        .db_set
        .slot_mut(db_id)
        .and_then(|s| s.btree.as_mut())
        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
    if bt.pager().ok_to_change_journal_mode() {
        let old_mode = bt.pager().journal_mode();
        let target_mode = bt.pager().effective_journal_mode_target(target);
        if target_mode != old_mode
            && (old_mode == JournalMode::Wal || target_mode == JournalMode::Wal)
            && disallow_wal_transition
        {
            return Err(SqliteError::new(
                ErrorCode::Error,
                if target_mode == JournalMode::Wal {
                    "cannot change into wal mode from within a transaction"
                } else {
                    "cannot change out of wal mode from within a transaction"
                },
            ));
        }
    }
    ctx.busy_policy
        .retry_btree(bt, |b| b.set_journal_mode(target))
}

fn exec_pragma(
    ctx: &mut ExecContext<'_>,
    schema: Option<&str>,
    name: &str,
    value: Option<&PragmaValue>,
) -> Result<QueryOutput> {
    let db_id = ctx.db_set.resolve_schema_or_err(schema)?;
    let upper = name.to_ascii_uppercase();
    let single_col = |cname: &str, v: Value| QueryOutput {
        column_names: vec![cname.to_owned()],
        rows: vec![vec![v]],
    };

    // 1. Connection-global & multi-DB pragmas handled before forcing `temp` open:
    match upper.as_str() {
        "DATABASE_LIST" => {
            let mut rows = Vec::new();
            for (idx, slot) in ctx.db_set.slots().iter().enumerate() {
                let Some(ref bt) = slot.btree else {
                    continue;
                };
                let file_str = if idx == DatabaseId::TEMP.index() {
                    String::new()
                } else {
                    bt.db_path()
                        .map_or_else(String::new, |p| p.to_string_lossy().into_owned())
                };
                rows.push(vec![
                    Value::Integer(idx as i64),
                    Value::Text(slot.name.clone()),
                    Value::Text(file_str),
                ]);
            }
            return Ok(QueryOutput {
                column_names: vec!["seq".to_owned(), "name".to_owned(), "file".to_owned()],
                rows,
            });
        }
        "LOCK_STATUS" => {
            let mut rows = Vec::with_capacity(ctx.db_set.slot_count());
            for slot in ctx.db_set.slots() {
                let status = match slot.btree {
                    None => "closed",
                    Some(ref bt) => match bt.pager().lock_state_for_diagnostics()? {
                        Some(lvl) => lvl.as_diagnostic_str(),
                        None => "unknown",
                    },
                };
                rows.push(vec![
                    Value::Text(slot.name.clone()),
                    Value::Text(status.to_owned()),
                ]);
            }
            return Ok(QueryOutput {
                column_names: vec!["database".to_owned(), "status".to_owned()],
                rows,
            });
        }
        "TEMP_STORE" => {
            if let Some(pv) = value {
                let mode = match pv {
                    PragmaValue::Integer(1) => TempStoreMode::File,
                    PragmaValue::Integer(2) => TempStoreMode::Memory,
                    PragmaValue::Integer(_) => TempStoreMode::Default,
                    PragmaValue::Ident(s) | PragmaValue::String(s) => {
                        TempStoreMode::from_pragma_str(s)
                    }
                };
                if ctx.db_set.set_temp_store(mode, is_in_autocommit(ctx))? {
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation += 1;
                }
                return Ok(QueryOutput::default());
            }
            return Ok(single_col(
                "temp_store",
                Value::Integer(ctx.db_set.temp_store().as_int()),
            ));
        }
        "WRITABLE_SCHEMA" => {
            if let Some(pv) = value {
                let is_reset = matches!(
                    pv,
                    PragmaValue::Ident(s) | PragmaValue::String(s)
                        if s.trim().eq_ignore_ascii_case("reset")
                );
                let on = pragma_to_bool(pv) || is_reset;
                *ctx.writable_schema = on;
                for slot in ctx.db_set.slots_mut() {
                    if let Some(ref mut bt) = slot.btree {
                        bt.set_writable_schema(on);
                    }
                }
                if is_reset {
                    *ctx.writable_schema = false;
                    for slot in ctx.db_set.slots_mut() {
                        if let Some(ref mut bt) = slot.btree {
                            bt.set_writable_schema(false);
                        }
                        slot.schema = Schema::default();
                    }
                    *ctx.schema_generation += 1;
                    *ctx.v1_expire_generation += 1;
                }
                return Ok(QueryOutput::default());
            }
            return Ok(single_col(
                "writable_schema",
                Value::Integer(i64::from(*ctx.writable_schema)),
            ));
        }
        "FOREIGN_KEYS" => {
            if let Some(pv) = value {
                // Per SQLite `pragma.c`, `PRAGMA foreign_keys = bool` is a no-op inside a transaction!
                if is_in_autocommit(ctx) {
                    *ctx.foreign_keys = pragma_to_bool(pv);
                }
                return Ok(QueryOutput::default());
            }
            return Ok(single_col(
                "foreign_keys",
                Value::Integer(i64::from(*ctx.foreign_keys)),
            ));
        }
        "LEGACY_ALTER_TABLE" => {
            if let Some(pv) = value {
                *ctx.legacy_alter_table = pragma_to_bool(pv);
                return Ok(QueryOutput::default());
            }
            return Ok(single_col(
                "legacy_alter_table",
                Value::Integer(i64::from(*ctx.legacy_alter_table)),
            ));
        }
        "COUNT_CHANGES" => {
            if let Some(pv) = value {
                *ctx.count_changes = pragma_to_bool(pv);
                return Ok(QueryOutput::default());
            }
            return Ok(single_col(
                "count_changes",
                Value::Integer(i64::from(*ctx.count_changes)),
            ));
        }
        "WAL_AUTOCHECKPOINT" => {
            if let Some(pv) = value {
                let raw = pragma_to_i64(pv).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                *ctx.wal_autocheckpoint_threshold = if raw > 0 { raw } else { 0 };
            }
            let effective = (*ctx.wal_autocheckpoint_threshold).max(0);
            return Ok(single_col(
                "wal_autocheckpoint",
                Value::Integer(i64::from(effective)),
            ));
        }
        "COMPILE_OPTIONS" => {
            return Ok(QueryOutput {
                column_names: vec!["compile_options".to_owned()],
                rows: vec![
                    vec![Value::Text("PURE_SAFE_RUST".to_owned())],
                    vec![Value::Text("SQLITE_FORMAT_3".to_owned())],
                    vec![Value::Text("THREADSAFE=1".to_owned())],
                ],
            });
        }
        "ENCODING" => {
            if let Some(pv) = value {
                let s = pragma_to_string(pv);
                if let Some(enc) = TextEncoding::from_pragma_name(&s)
                    && is_in_autocommit(ctx)
                    && ctx.db_set.attached_count() == 0
                {
                    let main_bt = ctx.db_set.main_btree_mut();
                    main_bt.pager_mut().set_encoding(enc)?;
                    if main_bt.pager_mut().effective_encoding()? == enc {
                        *ctx.connection_encoding = enc;
                    }
                }
                return Ok(QueryOutput::default());
            }
            let enc = ctx
                .db_set
                .main_btree_mut()
                .pager_mut()
                .effective_encoding()?;
            *ctx.connection_encoding = enc;
            return Ok(single_col(
                "encoding",
                Value::Text(enc.as_pragma_str().to_owned()),
            ));
        }
        "JOURNAL_MODE" => {
            if schema.is_none() {
                if let Some(pv) = value {
                    let s = pragma_to_string(pv);
                    if let Some(target) = JournalMode::from_pragma_str(s.trim()) {
                        let mut main_mode = JournalMode::Delete;
                        for idx in (0..ctx.db_set.slot_count()).rev() {
                            if ctx.db_set.slots()[idx].btree.is_none() {
                                continue;
                            }
                            let res_mode =
                                apply_journal_mode_on_slot(ctx, DatabaseId(idx), Some(target))?;
                            if idx == 0 {
                                main_mode = res_mode;
                            }
                        }
                        return Ok(single_col(
                            "journal_mode",
                            Value::Text(main_mode.as_str().to_owned()),
                        ));
                    }
                }
                let active = apply_journal_mode_on_slot(ctx, DatabaseId::MAIN, None)?;
                return Ok(single_col(
                    "journal_mode",
                    Value::Text(active.as_str().to_owned()),
                ));
            }
            let target_opt = value.and_then(|pv| {
                let s = pragma_to_string(pv);
                JournalMode::from_pragma_str(s.trim())
            });
            let active = apply_journal_mode_on_slot(ctx, db_id, target_opt)?;
            return Ok(single_col(
                "journal_mode",
                Value::Text(active.as_str().to_owned()),
            ));
        }
        "INTEGRITY_CHECK" | "QUICK_CHECK" => {
            let mut all_errs = Vec::new();
            if schema.is_none() {
                for idx in 0..ctx.db_set.slot_count() {
                    let cur_id = DatabaseId(idx);
                    if ctx.db_set.slot(cur_id).is_none_or(|s| s.btree.is_none()) {
                        continue;
                    }
                    ctx.ensure_read_db(cur_id, true)?;
                    let slot = ctx.db_set.slot_mut(cur_id).unwrap();
                    let roots = slot.schema.all_btree_roots();
                    if let Some(ref mut bt) = slot.btree {
                        all_errs.extend(bt.integrity_check(&roots)?);
                    }
                }
            } else {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                let slot = ctx.db_set.slot_mut(db_id).unwrap();
                let roots = slot.schema.all_btree_roots();
                if let Some(ref mut bt) = slot.btree {
                    all_errs.extend(bt.integrity_check(&roots)?);
                }
            }
            let rows = if all_errs.is_empty() {
                vec![vec![Value::Text("ok".to_owned())]]
            } else {
                all_errs.into_iter().map(|e| vec![Value::Text(e)]).collect()
            };
            return Ok(QueryOutput {
                column_names: vec!["integrity_check".to_owned()],
                rows,
            });
        }
        "TABLE_INFO" | "TABLE_XINFO" => {
            let is_xinfo = upper == "TABLE_XINFO";
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let mut cols = vec![
                "cid".to_owned(),
                "name".to_owned(),
                "type".to_owned(),
                "notnull".to_owned(),
                "dflt_value".to_owned(),
                "pk".to_owned(),
            ];
            if is_xinfo {
                cols.push("hidden".to_owned());
            }

            enum PragmaInfoTarget {
                Table(Box<TableSchema>),
                View(DatabaseId, Box<ViewSchema>),
            }

            let found_target = if schema.is_some() {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                if DatabaseSet::is_catalog_table_in_db(db_id, &tname) {
                    Some(PragmaInfoTarget::Table(Box::new(
                        TableSchema::sqlite_schema_table(),
                    )))
                } else if let Some(slot) = ctx.db_set.slot(db_id) {
                    slot.schema
                        .find_user_table(&tname)
                        .cloned()
                        .map(|t| PragmaInfoTarget::Table(Box::new(t)))
                        .or_else(|| {
                            slot.schema
                                .find_view(&tname)
                                .cloned()
                                .map(|v| PragmaInfoTarget::View(db_id, Box::new(v)))
                        })
                } else {
                    None
                }
            } else {
                let mut hit = None;
                for cand_id in ctx.db_set.unqualified_search_order() {
                    ctx.ensure_read_db(cand_id, true)?;
                    if DatabaseSet::is_catalog_table_in_db(cand_id, &tname) {
                        hit = Some(PragmaInfoTarget::Table(Box::new(
                            TableSchema::sqlite_schema_table(),
                        )));
                        break;
                    }
                    if let Some(slot) = ctx.db_set.slot(cand_id) {
                        if let Some(t) = slot.schema.find_user_table(&tname).cloned() {
                            hit = Some(PragmaInfoTarget::Table(Box::new(t)));
                            break;
                        }
                        if let Some(v) = slot.schema.find_view(&tname).cloned() {
                            hit = Some(PragmaInfoTarget::View(cand_id, Box::new(v)));
                            break;
                        }
                    }
                }
                if hit.is_none() && Schema::is_temp_schema_table_name(&tname) {
                    hit = Some(PragmaInfoTarget::Table(Box::new(
                        TableSchema::sqlite_schema_table(),
                    )));
                }
                hit
            };

            let tbl = match found_target {
                None => {
                    return Ok(QueryOutput {
                        column_names: cols,
                        rows: Vec::new(),
                    });
                }
                Some(PragmaInfoTarget::View(v_db_id, view)) => {
                    let vcols =
                        infer_view_pragma_columns(v_db_id, &view, ctx.db_set, ctx.functions)?;
                    let mut rows = Vec::with_capacity(vcols.len());
                    for (cid, (cname, ctype)) in vcols.into_iter().enumerate() {
                        let mut row = vec![
                            Value::Integer(cid as i64),
                            Value::Text(cname),
                            Value::Text(ctype),
                            Value::Integer(0),
                            Value::Null,
                            Value::Integer(0),
                        ];
                        if is_xinfo {
                            row.push(Value::Integer(0));
                        }
                        rows.push(row);
                    }
                    return Ok(QueryOutput {
                        column_names: cols,
                        rows,
                    });
                }
                Some(PragmaInfoTarget::Table(t)) => *t,
            };

            let mut rows = Vec::new();
            let mut visible_cid = 0i64;
            for (raw_cid, col) in tbl.columns.iter().enumerate() {
                let hidden_flag = match col.generated {
                    None => 0i64,
                    Some((_, false)) => 2i64,
                    Some((_, true)) => 3i64,
                };
                if !is_xinfo && hidden_flag != 0 {
                    continue;
                }
                let cid_val = if is_xinfo {
                    raw_cid as i64
                } else {
                    let c = visible_cid;
                    visible_cid += 1;
                    c
                };
                let dflt = col
                    .default_expr
                    .as_ref()
                    .map_or(Value::Null, |e| Value::Text(expr_column_label(e)));
                let mut row = vec![
                    Value::Integer(cid_val),
                    Value::Text(col.name.clone()),
                    Value::Text(col.type_name.clone()),
                    Value::Integer(i64::from(col.not_null.is_some())),
                    dflt,
                    Value::Integer(col.pk_order as i64),
                ];
                if is_xinfo {
                    row.push(Value::Integer(hidden_flag));
                }
                rows.push(row);
            }
            return Ok(QueryOutput {
                column_names: cols,
                rows,
            });
        }
        "INDEX_LIST" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let target_slot_id = if schema.is_some() {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                Some(db_id)
            } else {
                let mut hit_db = None;
                for cand_id in ctx.db_set.unqualified_search_order() {
                    ctx.ensure_read_db(cand_id, true)?;
                    if ctx
                        .db_set
                        .slot(cand_id)
                        .is_some_and(|s| s.schema.find_user_table(&tname).is_some())
                    {
                        hit_db = Some(cand_id);
                        break;
                    }
                }
                hit_db
            };

            let idxs = if let Some(tid) = target_slot_id
                && let Some(slot) = ctx.db_set.slot(tid)
            {
                let mut v = slot.schema.indexes_for_table(&tname);
                if let Some(tbl) = slot.schema.find_user_table(&tname)
                    && let Some(pk_idx) = tbl.without_rowid_pk_as_index()
                {
                    v.insert(0, pk_idx);
                }
                v
            } else {
                Vec::new()
            };

            let rows = idxs
                .iter()
                .enumerate()
                .map(|(seq, idx)| {
                    vec![
                        Value::Integer(seq as i64),
                        Value::Text(idx.name.clone()),
                        Value::Integer(i64::from(idx.unique)),
                        Value::Text(idx.origin.to_owned()),
                        Value::Integer(i64::from(idx.where_clause.is_some())),
                    ]
                })
                .collect();
            return Ok(QueryOutput {
                column_names: vec![
                    "seq".to_owned(),
                    "name".to_owned(),
                    "unique".to_owned(),
                    "origin".to_owned(),
                    "partial".to_owned(),
                ],
                rows,
            });
        }
        "INDEX_INFO" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let iname = pragma_to_string(pv);
            let cols = vec!["seqno".to_owned(), "cid".to_owned(), "name".to_owned()];
            let found_idx = if schema.is_some() {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                ctx.db_set
                    .slot(db_id)
                    .and_then(|s| s.schema.find_index_including_without_rowid_pk(&iname))
            } else {
                let mut hit = None;
                for cand_id in ctx.db_set.unqualified_search_order() {
                    ctx.ensure_read_db(cand_id, true)?;
                    if let Some(idx) = ctx
                        .db_set
                        .slot(cand_id)
                        .and_then(|s| s.schema.find_index_including_without_rowid_pk(&iname))
                    {
                        hit = Some(idx);
                        break;
                    }
                }
                hit
            };
            let Some(idx) = found_idx else {
                return Ok(QueryOutput {
                    column_names: cols,
                    rows: Vec::new(),
                });
            };
            let rows = idx
                .columns
                .iter()
                .enumerate()
                .map(|(seqno, ic)| match ic.col_idx {
                    Some(ci) => vec![
                        Value::Integer(seqno as i64),
                        Value::Integer(ci as i64),
                        Value::Text(ic.name.clone()),
                    ],
                    None => vec![
                        Value::Integer(seqno as i64),
                        Value::Integer(-2),
                        Value::Null,
                    ],
                })
                .collect();
            return Ok(QueryOutput {
                column_names: cols,
                rows,
            });
        }
        "FOREIGN_KEY_LIST" => {
            let Some(pv) = value else {
                return Ok(QueryOutput::default());
            };
            let tname = pragma_to_string(pv);
            let cols = vec![
                "id".to_owned(),
                "seq".to_owned(),
                "table".to_owned(),
                "from".to_owned(),
                "to".to_owned(),
                "on_update".to_owned(),
                "on_delete".to_owned(),
                "match".to_owned(),
            ];
            let found_tbl = if schema.is_some() {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                ctx.db_set
                    .slot(db_id)
                    .and_then(|s| s.schema.find_user_table(&tname).cloned())
            } else {
                let mut hit = None;
                for cand_id in ctx.db_set.unqualified_search_order() {
                    ctx.ensure_read_db(cand_id, true)?;
                    if let Some(t) = ctx
                        .db_set
                        .slot(cand_id)
                        .and_then(|s| s.schema.find_user_table(&tname).cloned())
                    {
                        hit = Some(t);
                        break;
                    }
                }
                hit
            };
            let Some(tbl) = found_tbl else {
                return Ok(QueryOutput {
                    column_names: cols,
                    rows: Vec::new(),
                });
            };
            let mut rows = Vec::new();
            for fk in &tbl.foreign_keys {
                for (seq, from_name) in fk.from_col_names.iter().enumerate() {
                    let to_val = fk
                        .ref_cols
                        .as_ref()
                        .and_then(|rc| rc.get(seq))
                        .map_or(Value::Null, |s| Value::Text(s.clone()));
                    rows.push(vec![
                        Value::Integer(fk.id as i64),
                        Value::Integer(seq as i64),
                        Value::Text(fk.ref_table.clone()),
                        Value::Text(from_name.clone()),
                        to_val,
                        Value::Text(fk.on_update.as_sql_str().to_owned()),
                        Value::Text(fk.on_delete.as_sql_str().to_owned()),
                        Value::Text("NONE".to_owned()),
                    ]);
                }
            }
            return Ok(QueryOutput {
                column_names: cols,
                rows,
            });
        }
        "FOREIGN_KEY_CHECK" => {
            let target_tbl = value.map(pragma_to_string);
            let check_db_id = if schema.is_some() {
                if db_id == DatabaseId::TEMP {
                    let enc = ctx.encoding();
                    ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
                }
                ctx.ensure_read_db(db_id, true)?;
                db_id
            } else if let Some(ref tn) = target_tbl {
                let mut hit_db = DatabaseId::MAIN;
                for cand_id in ctx.db_set.unqualified_search_order() {
                    ctx.ensure_read_db(cand_id, true)?;
                    if ctx
                        .db_set
                        .slot(cand_id)
                        .is_some_and(|s| s.schema.find_user_table(tn).is_some())
                    {
                        hit_db = cand_id;
                        break;
                    }
                }
                hit_db
            } else {
                ctx.ensure_read_db(DatabaseId::MAIN, true)?;
                DatabaseId::MAIN
            };

            let (schema_name, slot_schema, tables) = {
                let slot = ctx
                    .db_set
                    .slot(check_db_id)
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                let tables: Vec<TableSchema> = slot
                    .schema
                    .tables
                    .iter()
                    .filter(|t| {
                        target_tbl
                            .as_ref()
                            .is_none_or(|tn| t.name.eq_ignore_ascii_case(tn))
                    })
                    .cloned()
                    .collect();
                (slot.name.clone(), slot.schema.clone(), tables)
            };

            let mut rows = Vec::new();
            for tbl in tables {
                if tbl.foreign_keys.is_empty() {
                    continue;
                }
                let raw_rows = scan_table_raw_entries(ctx, check_db_id, &tbl)?;
                for (rkey, payload) in raw_rows {
                    let cvals =
                        decode_table_logical_row(ctx, check_db_id, &tbl, &rkey, &payload, &[])?;
                    for fk in &tbl.foreign_keys {
                        if fk.from_cols.iter().any(|&ci| cvals[ci].is_null()) {
                            continue;
                        }
                        let (ptbl, pcols) =
                            resolve_fk_parent_cols(&slot_schema, &schema_name, &tbl.name, fk)?;
                        if !parent_row_exists_for_fk(
                            ctx,
                            check_db_id,
                            &ptbl,
                            &pcols,
                            &cvals,
                            &fk.from_cols,
                        )? {
                            let rowid_val = match &rkey {
                                TableRowKey::Rowid(rid) => Value::Integer(*rid),
                                TableRowKey::WithoutRowidPk(_) => Value::Null,
                            };
                            rows.push(vec![
                                Value::Text(tbl.name.clone()),
                                rowid_val,
                                Value::Text(fk.ref_table.clone()),
                                Value::Integer(fk.id as i64),
                            ]);
                        }
                    }
                }
            }
            return Ok(QueryOutput {
                column_names: vec![
                    "table".to_owned(),
                    "rowid".to_owned(),
                    "parent".to_owned(),
                    "fkid".to_owned(),
                ],
                rows,
            });
        }
        _ => {}
    }

    // 2. Per-DB pragmas: if `db_id == DatabaseId::TEMP` (i.e. `PRAGMA temp.<name>`), lazily open `temp` (`pragma.c:457`)!
    if db_id == DatabaseId::TEMP {
        let enc = ctx.encoding();
        ctx.db_set.ensure_temp_open(enc, *ctx.writable_schema)?;
    }

    match upper.as_str() {
        "PAGE_SIZE" => {
            if let Some(pv) = value {
                {
                    let bt = ctx
                        .db_set
                        .slot_mut(db_id)
                        .and_then(|s| s.btree.as_mut())
                        .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                    if bt.transaction_state() == TransactionState::Open
                        && !bt.pager().is_header_loaded()
                        && bt.pager().db_kind() == DatabaseKind::NamedDisk
                    {
                        let _ = ctx
                            .busy_policy
                            .retry_btree(bt, |b| b.pager_mut().ensure_header_loaded().map(|_| ()));
                    }
                }
                let raw = pragma_to_i64(pv);
                if let Ok(mut sz) = u32::try_from(raw)
                    && sqliters_pager::DatabaseHeader::is_valid_page_size(sz)
                {
                    let autocommit = is_in_autocommit(ctx);
                    let slot = ctx.db_set.slot_mut(db_id).unwrap();
                    let reserve = slot
                        .btree
                        .as_ref()
                        .map_or(0, |bt| bt.pager().header().reserved_bytes);
                    if sz == 512 && reserve > 32 {
                        sz = 1024;
                    }
                    if sz.saturating_sub(u32::from(reserve)) >= 480 {
                        slot.next_page_size = Some(sz);
                        let applied = if let Some(ref mut bt) = slot.btree
                            && !bt.pager().is_page_size_fixed()
                            && bt.pager().page_count() == 0
                            && autocommit
                        {
                            let _ = bt.pager_mut().set_page_size(sz);
                            true
                        } else {
                            false
                        };
                        if applied && db_id == DatabaseId::MAIN {
                            ctx.db_set.set_default_page_size(sz);
                        }
                    }
                }
                Ok(QueryOutput::default())
            } else {
                let bt = ctx
                    .db_set
                    .slot_mut(db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                let ps = bt.pager_mut().effective_page_size()?;
                Ok(single_col("page_size", Value::Integer(i64::from(ps))))
            }
        }
        "AUTO_VACUUM" => {
            if db_id == DatabaseId::TEMP {
                if value.is_some() {
                    return Ok(QueryOutput::default());
                }
                return Ok(single_col("auto_vacuum", Value::Integer(0)));
            }
            if let Some(pv) = value {
                let e_auto = parse_auto_vacuum_pragma(pv);
                let ok = {
                    let slot = ctx.db_set.slot_mut(db_id).unwrap();
                    slot.next_auto_vacuum = Some(e_auto);
                    slot.btree
                        .as_mut()
                        .is_some_and(|bt| bt.set_configured_auto_vacuum(e_auto))
                };
                if ok && matches!(e_auto, AutoVacuumMode::Full | AutoVacuumMode::Incremental) {
                    run_write_autocommit(ctx, |ctx| {
                        ctx.ensure_write_db(db_id, false)?;
                        if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree
                            && bt.pager().header().largest_root_btree_page != 0
                        {
                            let incr_flag = u32::from(e_auto == AutoVacuumMode::Incremental);
                            bt.pager_mut().header_mut()?.incremental_vacuum_mode = incr_flag;
                            let _ = bt.set_configured_auto_vacuum(e_auto);
                        }
                        Ok(QueryOutput::default())
                    })?;
                }
                Ok(QueryOutput::default())
            } else {
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                    && bt.transaction_state() == TransactionState::Open
                    && !bt.pager().is_header_loaded()
                    && bt.pager().db_kind() == DatabaseKind::NamedDisk
                {
                    let _ = ctx
                        .busy_policy
                        .retry_btree(bt, |b| b.pager_mut().ensure_header_loaded().map(|_| ()));
                }
                let mode = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(AutoVacuumMode::None, Btree::auto_vacuum_mode);
                Ok(single_col("auto_vacuum", Value::Integer(mode.as_int())))
            }
        }
        "DEFAULT_CACHE_SIZE" => {
            if db_id == DatabaseId::TEMP {
                if value.is_some() {
                    return Ok(QueryOutput::default());
                }
                return Ok(single_col("cache_size", Value::Integer(2000)));
            }
            if let Some(pv) = value {
                let size = pragma_to_i64(pv).saturating_abs().min(i64::from(i32::MAX)) as i32;
                ctx.ensure_write_db(db_id, false)?;
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                    bt.pager_mut().header_mut()?.default_cache_size = size;
                    bt.set_cache_size(size);
                }
                Ok(QueryOutput::default())
            } else {
                ctx.ensure_read_db(db_id, false)?;
                let raw = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(0, |bt| bt.pager().header().default_cache_size);
                let val = if raw == 0 {
                    2000
                } else {
                    i64::from(raw.unsigned_abs())
                };
                Ok(single_col("cache_size", Value::Integer(val)))
            }
        }
        "PAGE_COUNT" => {
            ctx.ensure_read_db(db_id, false)?;
            let pc = ctx
                .db_set
                .slot(db_id)
                .and_then(|s| s.btree.as_ref())
                .map_or(0, |bt| bt.pager().page_count());
            Ok(single_col("page_count", Value::Integer(i64::from(pc))))
        }
        "MAX_PAGE_COUNT" => {
            ctx.ensure_read_db(db_id, false)?;
            let slot = ctx.db_set.slot_mut(db_id).unwrap();
            let cur_pc = slot.btree.as_ref().map_or(0, |bt| bt.pager().page_count());
            if let Some(pv) = value {
                let req = pragma_to_i64(pv);
                if req > 0 {
                    let clamped = u32::try_from(req)
                        .unwrap_or(crate::db_set::SQLITE_MAX_PAGE_COUNT)
                        .min(crate::db_set::SQLITE_MAX_PAGE_COUNT)
                        .max(cur_pc.max(1));
                    slot.max_page_count = clamped;
                }
            }
            Ok(single_col(
                "max_page_count",
                Value::Integer(i64::from(slot.max_page_count)),
            ))
        }
        "FREELIST_COUNT" => {
            ctx.ensure_read_db(db_id, false)?;
            let fc = ctx
                .db_set
                .slot(db_id)
                .and_then(|s| s.btree.as_ref())
                .map_or(0, |bt| bt.pager().header().total_freelist_pages);
            Ok(single_col("freelist_count", Value::Integer(i64::from(fc))))
        }
        "USER_VERSION" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                ctx.ensure_write_db(db_id, false)?;
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                    bt.pager_mut().header_mut()?.user_version = v;
                }
                Ok(QueryOutput::default())
            } else {
                ctx.ensure_read_db(db_id, false)?;
                let v = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(0, |bt| bt.pager().header().user_version);
                Ok(single_col("user_version", Value::Integer(i64::from(v))))
            }
        }
        "APPLICATION_ID" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as i32;
                ctx.ensure_write_db(db_id, false)?;
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).unwrap().btree {
                    bt.pager_mut().header_mut()?.application_id = v;
                }
                Ok(QueryOutput::default())
            } else {
                ctx.ensure_read_db(db_id, false)?;
                let v = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(0, |bt| bt.pager().header().application_id);
                Ok(single_col("application_id", Value::Integer(i64::from(v))))
            }
        }
        "SCHEMA_VERSION" => {
            if let Some(pv) = value {
                let v = pragma_to_i64(pv) as u32;
                ctx.ensure_write_db(db_id, false)?;
                let slot = ctx.db_set.slot_mut(db_id).unwrap();
                let sname = slot.name.clone();
                if let Some(ref mut bt) = slot.btree {
                    bt.pager_mut().header_mut()?.schema_cookie = v;
                    slot.schema = Schema::load_for_schema(bt, Some(&sname))?;
                }
                *ctx.schema_generation += 1;
                *ctx.v1_expire_generation += 1;
                *ctx.schema_changed_in_tx = true;
                Ok(QueryOutput::default())
            } else {
                ctx.ensure_read_db(db_id, false)?;
                let v = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(0, |bt| bt.pager().header().schema_cookie);
                Ok(single_col("schema_version", Value::Integer(i64::from(v))))
            }
        }
        "WAL_CHECKPOINT" => {
            let mode = match value {
                None => WalCheckpointMode::Passive,
                Some(pv) => {
                    let s = pragma_to_string(pv);
                    WalCheckpointMode::from_pragma_str(s.trim())
                        .unwrap_or(WalCheckpointMode::Passive)
                }
            };
            let res = if db_id == DatabaseId::TEMP {
                WalCheckpointResult::not_wal()
            } else {
                let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                let bt_state = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(TransactionState::Open, Btree::transaction_state);
                if bt_state != TransactionState::Open
                    || ((ctx.activity.other_read_db_mask | ctx.activity.other_write_db_mask) & bit)
                        != 0
                {
                    return Err(SqliteError::new(
                        ErrorCode::Locked,
                        "database table is locked",
                    ));
                }
                let bt = ctx
                    .db_set
                    .slot_mut(db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                if bt.pager().db_kind() == DatabaseKind::NamedDisk
                    && (!bt.pager().is_header_loaded()
                        || (bt.pager().journal_mode() == JournalMode::Wal
                            && bt.pager().wal().is_none()))
                {
                    ctx.busy_policy.retry_btree(bt, |b| b.begin_read())?;
                    bt.pager_mut().end_read_if_unused()?;
                }
                if bt.pager().wal().is_none() {
                    WalCheckpointResult::not_wal()
                } else {
                    ctx.busy_policy.checkpoint_btree(bt, mode)?
                }
            };
            Ok(QueryOutput {
                column_names: vec![
                    "busy".to_owned(),
                    "log".to_owned(),
                    "checkpointed".to_owned(),
                ],
                rows: vec![vec![
                    Value::Integer(i64::from(res.busy)),
                    Value::Integer(i64::from(res.n_log)),
                    Value::Integer(i64::from(res.n_ckpt)),
                ]],
            })
        }
        "LOCKING_MODE" => {
            if db_id == DatabaseId::TEMP {
                return Ok(single_col(
                    "locking_mode",
                    Value::Text("exclusive".to_owned()),
                ));
            }
            if let Some(pv) = value {
                let s = pragma_to_string(pv);
                if let Some(req) = LockingMode::from_pragma_str(s.trim()) {
                    if schema.is_none() {
                        for (idx, slot) in ctx.db_set.slots_mut().iter_mut().enumerate() {
                            if idx == DatabaseId::TEMP.index() {
                                continue;
                            }
                            if let Some(ref mut bt) = slot.btree {
                                let _ = bt.pager_mut().set_locking_mode(req);
                            }
                        }
                        *ctx.default_locking_mode = req;
                    } else if let Some(ref mut bt) =
                        ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                    {
                        let _ = bt.pager_mut().set_locking_mode(req);
                    }
                }
            }
            let cur = ctx
                .db_set
                .slot(db_id)
                .and_then(|s| s.btree.as_ref())
                .map_or(LockingMode::Normal, |bt| bt.pager().locking_mode());
            Ok(single_col(
                "locking_mode",
                Value::Text(cur.as_str().to_owned()),
            ))
        }
        "JOURNAL_SIZE_LIMIT" => {
            if db_id == DatabaseId::TEMP {
                return Ok(single_col("journal_size_limit", Value::Integer(-1)));
            }
            let bt = ctx
                .db_set
                .slot_mut(db_id)
                .and_then(|s| s.btree.as_mut())
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let res = if let Some(pv) = value {
                let limit = pragma_to_i64(pv).max(-1);
                bt.pager_mut().set_journal_size_limit(limit)
            } else {
                bt.pager_mut().set_journal_size_limit(-2)
            };
            Ok(single_col("journal_size_limit", Value::Integer(res)))
        }
        "SYNCHRONOUS" => {
            if let Some(pv) = value {
                let autocommit = is_in_autocommit(ctx);
                let bt = ctx
                    .db_set
                    .slot_mut(db_id)
                    .and_then(|s| s.btree.as_mut())
                    .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
                if autocommit && bt.transaction_state() == TransactionState::Open {
                    let level = match pv {
                        PragmaValue::Integer(i) if (0..=3).contains(i) => Some(*i as u8),
                        PragmaValue::Ident(s) | PragmaValue::String(s) => {
                            match s.trim().to_ascii_uppercase().as_str() {
                                "0" | "OFF" | "FALSE" | "NO" => Some(0),
                                "1" | "NORMAL" | "ON" | "TRUE" | "YES" => Some(1),
                                "2" | "FULL" => Some(2),
                                "3" | "EXTRA" => Some(3),
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some(lvl) = level {
                        bt.pager_mut().set_synchronous(lvl);
                    }
                }
                Ok(QueryOutput::default())
            } else {
                let cur = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(2, |bt| bt.pager().synchronous());
                Ok(single_col("synchronous", Value::Integer(i64::from(cur))))
            }
        }
        "CACHE_SIZE" => {
            if let Some(pv) = value {
                let sz = pragma_to_i64(pv).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                let bit = 1u16.checked_shl(db_id.index() as u32).unwrap_or(0);
                let can_spill =
                    ((ctx.activity.other_read_db_mask | ctx.activity.other_write_db_mask) & bit)
                        == 0;
                if let Some(ref mut bt) = ctx.db_set.slot_mut(db_id).and_then(|s| s.btree.as_mut())
                {
                    bt.set_cache_size(sz);
                    if can_spill {
                        bt.spill_if_over_cache_size()?;
                    }
                }
                Ok(QueryOutput::default())
            } else {
                let cur = ctx
                    .db_set
                    .slot(db_id)
                    .and_then(|s| s.btree.as_ref())
                    .map_or(-2000, Btree::cache_size);
                Ok(single_col("cache_size", Value::Integer(i64::from(cur))))
            }
        }
        _ => {
            // Unknown PRAGMAs in SQLite are silently ignored (no error)
            let _ = HashMap::<u8, u8>::new();
            Ok(QueryOutput::default())
        }
    }
}

fn pragma_to_i64(pv: &PragmaValue) -> i64 {
    match pv {
        PragmaValue::Integer(i) => *i,
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.trim().parse::<i64>().unwrap_or(0),
    }
}

fn pragma_to_bool(pv: &PragmaValue) -> bool {
    match pv {
        PragmaValue::Integer(i) => *i != 0,
        PragmaValue::Ident(s) | PragmaValue::String(s) => matches!(
            s.trim().to_ascii_uppercase().as_str(),
            "1" | "ON" | "TRUE" | "YES"
        ),
    }
}

fn pragma_to_string(pv: &PragmaValue) -> String {
    match pv {
        PragmaValue::Integer(i) => i.to_string(),
        PragmaValue::Ident(s) | PragmaValue::String(s) => s.clone(),
    }
}

// ============================================================================
// Prepare-Time Static Semantic Validation & Column Metadata Inference (`prepare.c`)
// ============================================================================

/// Lease cursor descriptor opened for interpreter `Read` statements so cursor-trip and
/// `OP_Destroy` table-lock semantics match VDBE cursors.
#[derive(Debug, Clone)]
pub(crate) enum LeaseCursorSpec {
    Table(DatabaseId, Pgno),
    Index(DatabaseId, Pgno, Vec<KeySortOrder>),
}

/// Collects B-tree roots referenced by a `Read` statement (`SELECT` / `PRAGMA`) so interpreter
/// queries keep open read cursors alive while paused at `StepResult::Row`.
pub(crate) fn collect_select_referenced_roots(
    stmt: &Stmt,
    db_set: &DatabaseSet,
) -> Vec<LeaseCursorSpec> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    match stmt {
        Stmt::Select(sel) => {
            let mut cte_names = Vec::new();
            let mut view_stack = Vec::new();
            collect_select_roots_rec(
                sel,
                db_set,
                &mut cte_names,
                &mut view_stack,
                &mut seen,
                &mut out,
            );
        }
        Stmt::Pragma {
            schema,
            name,
            value,
        } => {
            let upper = name.to_ascii_uppercase();
            match upper.as_str() {
                "INTEGRITY_CHECK" | "QUICK_CHECK" => {
                    if let Some(s) = schema {
                        if let Some(db_id) = db_set.find_db_id(s)
                            && db_set.slot(db_id).is_some_and(|sl| sl.btree.is_some())
                            && seen.insert((db_id, 1))
                        {
                            out.push(LeaseCursorSpec::Table(db_id, 1));
                        }
                    } else {
                        for idx in 0..db_set.slot_count() {
                            let db_id = DatabaseId(idx);
                            if db_set.slot(db_id).is_some_and(|sl| sl.btree.is_some())
                                && seen.insert((db_id, 1))
                            {
                                out.push(LeaseCursorSpec::Table(db_id, 1));
                            }
                        }
                    }
                }
                "TABLE_INFO" | "TABLE_XINFO" | "INDEX_LIST" | "INDEX_INFO" | "FOREIGN_KEY_LIST"
                | "FOREIGN_KEY_CHECK" => {
                    let target_db = if let Some(s) = schema {
                        db_set.find_db_id(s)
                    } else if let Some(pv) = value {
                        let arg = pragma_to_string(pv);
                        let qn = QualifiedName::unqualified(arg);
                        if upper == "INDEX_INFO" {
                            db_set
                                .find_index_including_without_rowid_pk_in_set(&qn)
                                .map(|(id, _)| id)
                        } else {
                            db_set
                                .find_table_in_set(&qn)
                                .ok()
                                .flatten()
                                .map(|(id, _)| id)
                                .or_else(|| db_set.find_view_in_set(&qn).map(|(id, _)| id))
                        }
                        .or(Some(DatabaseId::MAIN))
                    } else {
                        Some(DatabaseId::MAIN)
                    };
                    if let Some(db_id) = target_db
                        && db_set.slot(db_id).is_some_and(|sl| sl.btree.is_some())
                        && seen.insert((db_id, 1))
                    {
                        out.push(LeaseCursorSpec::Table(db_id, 1));
                    }
                }
                "PAGE_COUNT" | "MAX_PAGE_COUNT" | "FREELIST_COUNT" | "SCHEMA_VERSION"
                | "USER_VERSION" | "APPLICATION_ID" | "DEFAULT_CACHE_SIZE" => {
                    let target_db = match schema.as_deref() {
                        Some(s) => db_set.find_db_id(s),
                        None => Some(DatabaseId::MAIN),
                    };
                    if let Some(db_id) = target_db
                        && db_set.slot(db_id).is_some_and(|sl| sl.btree.is_some())
                        && seen.insert((db_id, 1))
                    {
                        out.push(LeaseCursorSpec::Table(db_id, 1));
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    out
}

fn collect_select_roots_rec(
    sel: &SelectStmt,
    db_set: &DatabaseSet,
    cte_stack: &mut Vec<String>,
    view_stack: &mut Vec<(DatabaseId, String)>,
    seen: &mut HashSet<(DatabaseId, Pgno)>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    let base_len = cte_stack.len();
    for cte in &sel.ctes {
        cte_stack.push(cte.name.clone());
        collect_select_roots_rec(&cte.query, db_set, cte_stack, view_stack, seen, out);
    }
    collect_core_roots_rec(&sel.body.head, db_set, cte_stack, view_stack, seen, out);
    for (_, rhs) in &sel.body.compounds {
        collect_core_roots_rec(rhs, db_set, cte_stack, view_stack, seen, out);
    }
    for ob in &sel.order_by {
        collect_expr_roots_rec(&ob.expr, db_set, cte_stack, view_stack, seen, out);
    }
    if let Some(ref lim) = sel.limit {
        collect_expr_roots_rec(&lim.limit, db_set, cte_stack, view_stack, seen, out);
        if let Some(ref off) = lim.offset {
            collect_expr_roots_rec(off, db_set, cte_stack, view_stack, seen, out);
        }
    }
    cte_stack.truncate(base_len);
}

fn collect_core_roots_rec(
    core: &SelectCore,
    db_set: &DatabaseSet,
    cte_stack: &mut Vec<String>,
    view_stack: &mut Vec<(DatabaseId, String)>,
    seen: &mut HashSet<(DatabaseId, Pgno)>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match core {
        SelectCore::Values(rows) => {
            for r in rows {
                for e in r {
                    collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
                }
            }
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            if let Some(fc) = from {
                collect_tos_roots_rec(&fc.head, db_set, cte_stack, view_stack, seen, out);
                for j in &fc.joins {
                    collect_tos_roots_rec(&j.right, db_set, cte_stack, view_stack, seen, out);
                    if let Some(JoinConstraint::On(ref e)) = j.constraint {
                        collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
                    }
                }
            }
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    collect_expr_roots_rec(expr, db_set, cte_stack, view_stack, seen, out);
                }
            }
            if let Some(w) = selection {
                collect_expr_roots_rec(w, db_set, cte_stack, view_stack, seen, out);
            }
            for g in group_by {
                collect_expr_roots_rec(g, db_set, cte_stack, view_stack, seen, out);
            }
            if let Some(h) = having {
                collect_expr_roots_rec(h, db_set, cte_stack, view_stack, seen, out);
            }
        }
    }
}

fn collect_table_root_spec(
    name: &QualifiedName,
    db_set: &DatabaseSet,
    cte_stack: &[String],
    view_stack: &mut Vec<(DatabaseId, String)>,
    seen: &mut HashSet<(DatabaseId, Pgno)>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    if name.schema.is_none() && cte_stack.iter().any(|c| c.eq_ignore_ascii_case(&name.name)) {
        return;
    }
    if let Ok(Some(cat_db_id)) = db_set.resolve_catalog_table(name) {
        if (cat_db_id != DatabaseId::TEMP || db_set.is_temp_open()) && seen.insert((cat_db_id, 1)) {
            out.push(LeaseCursorSpec::Table(cat_db_id, 1));
        }
        return;
    }
    if let Some((v_db_id, view)) = db_set.find_view_in_set(name) {
        let lower = view.name.to_ascii_lowercase();
        if view_stack
            .iter()
            .any(|(id, n)| *id == v_db_id && *n == lower)
        {
            return;
        }
        let slot_name = db_set
            .slot(v_db_id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "main".to_owned());
        if (v_db_id != DatabaseId::TEMP || db_set.is_temp_open()) && seen.insert((v_db_id, 1)) {
            out.push(LeaseCursorSpec::Table(v_db_id, 1));
        }
        if let Ok(fixed_sel) = prepare_fixed_view_select(v_db_id, &slot_name, view) {
            view_stack.push((v_db_id, lower));
            let mut inner_ctes = Vec::new();
            collect_select_roots_rec(&fixed_sel, db_set, &mut inner_ctes, view_stack, seen, out);
            view_stack.pop();
        }
        return;
    }
    if let Some((db_id, tbl)) = db_set.find_user_table_in_set(name)
        && tbl.root_pgno != 0
        && seen.insert((db_id, tbl.root_pgno))
    {
        if tbl.without_rowid {
            out.push(LeaseCursorSpec::Index(
                db_id,
                tbl.root_pgno,
                tbl.without_rowid_pk_orders(),
            ));
        } else {
            out.push(LeaseCursorSpec::Table(db_id, tbl.root_pgno));
        }
    }
}

fn collect_tos_roots_rec(
    tos: &TableOrSubquery,
    db_set: &DatabaseSet,
    cte_stack: &mut Vec<String>,
    view_stack: &mut Vec<(DatabaseId, String)>,
    seen: &mut HashSet<(DatabaseId, Pgno)>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match tos {
        TableOrSubquery::Table { name, .. } => {
            collect_table_root_spec(name, db_set, cte_stack, view_stack, seen, out);
        }
        TableOrSubquery::Subquery { query, .. } => {
            collect_select_roots_rec(query, db_set, cte_stack, view_stack, seen, out);
        }
    }
}

fn collect_expr_roots_rec(
    expr: &Expr,
    db_set: &DatabaseSet,
    cte_stack: &mut Vec<String>,
    view_stack: &mut Vec<(DatabaseId, String)>,
    seen: &mut HashSet<(DatabaseId, Pgno)>,
    out: &mut Vec<LeaseCursorSpec>,
) {
    match expr {
        Expr::Unary { expr: e, .. }
        | Expr::Collate { expr: e, .. }
        | Expr::IsNull { expr: e, .. }
        | Expr::Cast { expr: e, .. } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out)
        }
        Expr::RowValue(es) => {
            for e in es {
                collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            }
        }
        Expr::Binary { left, right, .. } => {
            collect_expr_roots_rec(left, db_set, cte_stack, view_stack, seen, out);
            collect_expr_roots_rec(right, db_set, cte_stack, view_stack, seen, out);
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            collect_expr_roots_rec(low, db_set, cte_stack, view_stack, seen, out);
            collect_expr_roots_rec(high, db_set, cte_stack, view_stack, seen, out);
        }
        Expr::InList { expr: e, list, .. } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            for x in list {
                collect_expr_roots_rec(x, db_set, cte_stack, view_stack, seen, out);
            }
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            collect_select_roots_rec(subquery, db_set, cte_stack, view_stack, seen, out);
        }
        Expr::InTable { expr: e, table, .. } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            collect_table_root_spec(table, db_set, cte_stack, view_stack, seen, out);
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
            collect_expr_roots_rec(pattern, db_set, cte_stack, view_stack, seen, out);
            if let Some(esc) = escape {
                collect_expr_roots_rec(esc, db_set, cte_stack, view_stack, seen, out);
            }
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                collect_expr_roots_rec(op, db_set, cte_stack, view_stack, seen, out);
            }
            for (w, t) in branches {
                collect_expr_roots_rec(w, db_set, cte_stack, view_stack, seen, out);
                collect_expr_roots_rec(t, db_set, cte_stack, view_stack, seen, out);
            }
            if let Some(el) = else_expr {
                collect_expr_roots_rec(el, db_set, cte_stack, view_stack, seen, out);
            }
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    collect_expr_roots_rec(e, db_set, cte_stack, view_stack, seen, out);
                }
            }
            if let Some(f) = filter {
                collect_expr_roots_rec(f, db_set, cte_stack, view_stack, seen, out);
            }
        }
        Expr::Subquery(sub) | Expr::Exists { subquery: sub, .. } => {
            collect_select_roots_rec(sub, db_set, cte_stack, view_stack, seen, out);
        }
        _ => {}
    }
}

/// Rewrites a bare top-level identifier (`Expr::Column(ColumnRef { schema: None, table: None, column })`)
/// in `ATTACH ... AS <schema_expr>` / `DETACH <schema_expr>` into a string literal (`sqlite/src/attach.c:codeAttach`).
fn normalize_attach_schema_expr(expr: &Expr) -> std::borrow::Cow<'_, Expr> {
    if let Expr::Column(ColumnRef {
        schema: None,
        table: None,
        column,
    }) = expr
    {
        std::borrow::Cow::Owned(Expr::String(column.clone()))
    } else {
        std::borrow::Cow::Borrowed(expr)
    }
}

/// Validates semantic bindings (tables, columns, functions, subqueries, DML/DDL widths) with
/// ZERO side effects and returns the exact result column names for `Connection::prepare`.
pub(crate) fn validate_and_infer_columns(
    stmt: &Stmt,
    db_set: &DatabaseSet,
    writable_schema: bool,
    foreign_keys: bool,
    count_changes: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
) -> Result<Vec<String>> {
    match stmt {
        Stmt::Begin { .. }
        | Stmt::Commit
        | Stmt::Rollback { .. }
        | Stmt::Savepoint(_)
        | Stmt::Release(_) => Ok(Vec::new()),

        Stmt::Analyze(target) => {
            validate_analyze_target_in_set(db_set, target.as_ref())?;
            Ok(Vec::new())
        }

        Stmt::AttachDatabase {
            expr,
            schema_name,
            key,
        } => {
            let mut visited = Vec::new();
            let norm_file = normalize_attach_schema_expr(expr);
            validate_expr_static(
                &norm_file,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            let norm_schema = normalize_attach_schema_expr(schema_name);
            validate_expr_static(
                &norm_schema,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            if let Some(k) = key {
                let norm_key = normalize_attach_schema_expr(k);
                validate_expr_static(
                    &norm_key,
                    &[],
                    db_set,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::DetachDatabase { schema_name } => {
            let mut visited = Vec::new();
            let norm_schema = normalize_attach_schema_expr(schema_name);
            validate_expr_static(
                &norm_schema,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                false,
                false,
                &mut visited,
            )?;
            Ok(Vec::new())
        }

        Stmt::Vacuum {
            schema: vac_schema,
            into,
        } => {
            let _ = db_set.resolve_schema_or_err(vac_schema.as_deref())?;
            if let Some(expr) = into {
                let mut visited = Vec::new();
                validate_expr_static(
                    expr,
                    &[],
                    db_set,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::Reindex(target) => {
            if let Some(qn) = target {
                if let Some(ref s) = qn.schema {
                    let db_id = db_set.resolve_schema_or_err(Some(s))?;
                    let ok = db_set.slot(db_id).is_some_and(|sl| {
                        sl.btree.is_some()
                            && (sl.schema.find_user_table(&qn.name).is_some()
                                || sl.schema.find_view(&qn.name).is_some()
                                || sl.schema.find_index(&qn.name).is_some())
                    });
                    if !ok {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "unable to identify the object to be reindexed",
                        ));
                    }
                } else {
                    let ok = Collation::from_name(&qn.name).is_some()
                        || collations.has_collation(&qn.name)
                        || db_set.find_user_table_in_set(qn).is_some()
                        || db_set.find_view_in_set(qn).is_some()
                        || db_set.find_index_in_set(qn).is_some();
                    if !ok {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "unable to identify the object to be reindexed",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::Pragma {
            schema: pragma_schema,
            name,
            value,
        } => {
            let db_id = db_set.resolve_schema_or_err(pragma_schema.as_deref())?;
            if let Some(pv) = value
                && matches!(
                    name.to_ascii_uppercase().as_str(),
                    "TABLE_INFO" | "TABLE_XINFO"
                )
            {
                let tname = pragma_to_string(pv);
                if pragma_schema.is_some() {
                    if !DatabaseSet::is_catalog_table_in_db(db_id, &tname)
                        && let Some(slot) = db_set.slot(db_id)
                        && slot.btree.is_some()
                        && slot.schema.find_user_table(&tname).is_none()
                        && let Some(view) = slot.schema.find_view(&tname)
                    {
                        let _ = infer_view_pragma_columns(db_id, view, db_set, functions)?;
                    }
                } else {
                    for cand_id in db_set.unqualified_search_order() {
                        if DatabaseSet::is_catalog_table_in_db(cand_id, &tname) {
                            break;
                        }
                        if let Some(slot) = db_set.slot(cand_id)
                            && slot.btree.is_some()
                        {
                            if slot.schema.find_user_table(&tname).is_some() {
                                break;
                            }
                            if let Some(view) = slot.schema.find_view(&tname) {
                                let _ =
                                    infer_view_pragma_columns(cand_id, view, db_set, functions)?;
                                break;
                            }
                        }
                    }
                }
            }
            Ok(infer_pragma_columns(name, value.as_ref()))
        }

        Stmt::Explain { query_plan, stmt } => {
            let _ = validate_and_infer_columns(
                stmt,
                db_set,
                writable_schema,
                foreign_keys,
                count_changes,
                functions,
                collations,
            )?;
            if *query_plan {
                Ok(vec![
                    "id".to_owned(),
                    "parent".to_owned(),
                    "notused".to_owned(),
                    "detail".to_owned(),
                ])
            } else {
                Ok(vec![
                    "addr".to_owned(),
                    "opcode".to_owned(),
                    "p1".to_owned(),
                    "p2".to_owned(),
                    "p3".to_owned(),
                    "p4".to_owned(),
                    "p5".to_owned(),
                    "comment".to_owned(),
                ])
            }
        }

        Stmt::Select(sel) => {
            let mut visited = Vec::new();
            let cols = validate_select_stmt_static(
                sel,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                &mut visited,
            )?;
            reject_raise_in_top_level_select(sel, db_set, &CteStore::default(), &mut Vec::new())?;
            Ok(cols)
        }

        Stmt::CreateTable(ct) => {
            let db_id = resolve_create_table_target(db_set, ct.temp, &ct.name)?;
            if !writable_schema && is_reserved_sqlite_name(&ct.name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", ct.name.name),
                ));
            }
            if let Some(slot) = db_set.slot(db_id)
                && slot.btree.is_some()
            {
                if slot.schema.find_user_table(&ct.name.name).is_some() {
                    if ct.if_not_exists {
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("table {} already exists", ct.name.name),
                    ));
                }
                if slot.schema.find_view(&ct.name.name).is_some() {
                    if ct.if_not_exists {
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("view {} already exists", ct.name.name),
                    ));
                }
                if slot.schema.find_index(&ct.name.name).is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("there is already an index named {}", ct.name.name),
                    ));
                }
            }
            let (tbl_preview, _) = build_table_schema_from_ast(ct, 2)?;
            let slot_name = db_set.slot(db_id).map_or("main", |s| s.name.as_str());
            let tbl_scope = make_static_table_scope(&tbl_preview, slot_name, None);
            let mut visited = Vec::new();
            for col in &tbl_preview.columns {
                if let Some(ref de) = col.default_expr {
                    validate_expr_static(
                        de,
                        &[],
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
                if let Some((ref ge, _)) = col.generated {
                    validate_expr_static(
                        ge,
                        std::slice::from_ref(&tbl_scope),
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
            for chk in &tbl_preview.checks {
                validate_expr_static(
                    &chk.expr,
                    std::slice::from_ref(&tbl_scope),
                    db_set,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::CreateTableAs {
            temp,
            if_not_exists,
            name,
            query,
        } => {
            let db_id = resolve_create_table_target(db_set, *temp, name)?;
            if !writable_schema && is_reserved_sqlite_name(&name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", name.name),
                ));
            }
            if let Some(slot) = db_set.slot(db_id)
                && slot.btree.is_some()
            {
                if slot.schema.find_user_table(&name.name).is_some() {
                    if *if_not_exists {
                        let mut visited = Vec::new();
                        let _ = validate_select_stmt_static(
                            query,
                            &[],
                            db_set,
                            &CteStore::default(),
                            functions,
                            &mut visited,
                        )?;
                        reject_raise_in_top_level_select(
                            query,
                            db_set,
                            &CteStore::default(),
                            &mut Vec::new(),
                        )?;
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("table {} already exists", name.name),
                    ));
                }
                if slot.schema.find_view(&name.name).is_some() {
                    if *if_not_exists {
                        let mut visited = Vec::new();
                        let _ = validate_select_stmt_static(
                            query,
                            &[],
                            db_set,
                            &CteStore::default(),
                            functions,
                            &mut visited,
                        )?;
                        reject_raise_in_top_level_select(
                            query,
                            db_set,
                            &CteStore::default(),
                            &mut Vec::new(),
                        )?;
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("view {} already exists", name.name),
                    ));
                }
                if slot.schema.find_index(&name.name).is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("there is already an index named {}", name.name),
                    ));
                }
            }
            let mut visited = Vec::new();
            let _ = validate_select_stmt_static(
                query,
                &[],
                db_set,
                &CteStore::default(),
                functions,
                &mut visited,
            )?;
            reject_raise_in_top_level_select(query, db_set, &CteStore::default(), &mut Vec::new())?;
            Ok(Vec::new())
        }

        Stmt::CreateView(cv) => {
            let db_id = resolve_create_table_target(db_set, cv.temp, &cv.name)?;
            if !writable_schema && is_reserved_sqlite_name(&cv.name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", cv.name.name),
                ));
            }
            validate_view_no_parameters(&cv.query)?;
            if db_id != DatabaseId::TEMP {
                let slot_name = db_set
                    .slot(db_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "main".to_owned());
                let _ = fix_permanent_view_select(&cv.name.name, &slot_name, &cv.query)?;
            }
            if let Some(slot) = db_set.slot(db_id)
                && slot.btree.is_some()
            {
                if slot.schema.find_user_table(&cv.name.name).is_some() {
                    if cv.if_not_exists {
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("table {} already exists", cv.name.name),
                    ));
                }
                if slot.schema.find_view(&cv.name.name).is_some() {
                    if cv.if_not_exists {
                        return Ok(Vec::new());
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("view {} already exists", cv.name.name),
                    ));
                }
                if slot.schema.find_index(&cv.name.name).is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("there is already an index named {}", cv.name.name),
                    ));
                }
            }
            Ok(Vec::new())
        }

        Stmt::DropTable { if_exists, name } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                if *if_exists {
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            validate_drop_or_alter_schema_table_in_set(db_set, name, "dropped")?;
            if let Some((_, ResolvedRelation::View(v))) =
                db_set.find_relation_in_set(name).ok().flatten()
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("use DROP VIEW to delete view {}", v.name),
                ));
            }
            match db_set.find_user_table_in_set(name) {
                Some((_, tbl)) => {
                    if is_non_droppable_system_table_name(&tbl.name) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("table {} may not be dropped", tbl.name),
                        ));
                    }
                }
                None => {
                    if !if_exists {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such table: {}", name.display_name()),
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::DropView { if_exists, name } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                if *if_exists {
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            validate_drop_or_alter_schema_table_in_set(db_set, name, "dropped")?;
            match db_set.find_relation_in_set(name).ok().flatten() {
                Some((_, ResolvedRelation::Table(t))) => {
                    if is_non_droppable_system_table_name(&t.name) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("table {} may not be dropped", t.name),
                        ));
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("use DROP TABLE to delete table {}", t.name),
                    ));
                }
                Some((_, ResolvedRelation::View(_))) => {}
                None => {
                    if !if_exists {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such view: {}", name.display_name()),
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::CreateTrigger(ct) => {
            let _ = resolve_and_validate_create_trigger(db_set, ct, writable_schema)?;
            Ok(Vec::new())
        }

        Stmt::DropTrigger { if_exists, name } => {
            if db_set.find_trigger_in_set(name).is_none() && !*if_exists {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such trigger: {}", name.display_name()),
                ));
            }
            Ok(Vec::new())
        }

        Stmt::CreateIndex(ci) => {
            if !writable_schema && is_reserved_sqlite_name(&ci.name.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("object name reserved for internal use: {}", ci.name.name),
                ));
            }
            let db_id = if let Some(ref s) = ci.name.schema {
                let resolved = db_set.resolve_schema_or_err(Some(s))?;
                if resolved == DatabaseId::TEMP {
                    let in_temp_view = db_set.is_temp_open()
                        && db_set
                            .slot(DatabaseId::TEMP)
                            .is_some_and(|sl| sl.schema.find_view(&ci.table_name).is_some());
                    if in_temp_view {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "views may not be indexed",
                        ));
                    }
                    let in_temp = db_set.is_temp_open()
                        && db_set
                            .slot(DatabaseId::TEMP)
                            .is_some_and(|sl| sl.schema.find_user_table(&ci.table_name).is_some());
                    if !in_temp {
                        if let Some((_, rel)) = db_set.find_relation_in_set(
                            &QualifiedName::unqualified(ci.table_name.clone()),
                        )? {
                            return match rel {
                                ResolvedRelation::View(_) => Err(SqliteError::new(
                                    ErrorCode::Error,
                                    "views may not be indexed",
                                )),
                                ResolvedRelation::Table(_) => Err(SqliteError::new(
                                    ErrorCode::Error,
                                    format!(
                                        "cannot create a TEMP index on non-TEMP table \"{}\"",
                                        ci.table_name
                                    ),
                                )),
                            };
                        }
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such table: {s}.{}", ci.table_name),
                        ));
                    }
                } else {
                    let is_view = db_set.slot(resolved).is_some_and(|sl| {
                        sl.btree.is_some() && sl.schema.find_view(&ci.table_name).is_some()
                    });
                    if is_view {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "views may not be indexed",
                        ));
                    }
                    let has_tbl = db_set.slot(resolved).is_some_and(|sl| {
                        sl.btree.is_some() && sl.schema.find_user_table(&ci.table_name).is_some()
                    });
                    if !has_tbl {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such table: {s}.{}", ci.table_name),
                        ));
                    }
                }
                resolved
            } else {
                match db_set
                    .find_relation_in_set(&QualifiedName::unqualified(ci.table_name.clone()))?
                {
                    Some((_, ResolvedRelation::View(_))) => {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "views may not be indexed",
                        ));
                    }
                    Some((found_db, ResolvedRelation::Table(_))) => found_db,
                    None => {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such table: {}", ci.table_name),
                        ));
                    }
                }
            };

            let slot = db_set
                .slot(db_id)
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            let Some(tbl) = slot.schema.find_user_table(&ci.table_name) else {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", ci.table_name),
                ));
            };
            if slot.schema.find_user_table(&ci.name.name).is_some()
                || slot.schema.find_view(&ci.name.name).is_some()
            {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("there is already a table named {}", ci.name.name),
                ));
            }
            if slot.schema.find_index(&ci.name.name).is_some() {
                if ci.if_not_exists {
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("index {} already exists", ci.name.name),
                ));
            }
            let idx_preview = build_index_schema_from_ast(ci, tbl, 2)?;
            let tbl_scope = make_static_table_scope(tbl, &slot.name, None);
            let mut visited = Vec::new();
            for ic in &idx_preview.columns {
                if let Some(ref ie) = ic.expr {
                    validate_expr_static(
                        ie,
                        std::slice::from_ref(&tbl_scope),
                        db_set,
                        &CteStore::default(),
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
            if let Some(ref wc) = idx_preview.where_clause {
                validate_expr_static(
                    wc,
                    std::slice::from_ref(&tbl_scope),
                    db_set,
                    &CteStore::default(),
                    functions,
                    false,
                    false,
                    &mut visited,
                )?;
            }
            Ok(Vec::new())
        }

        Stmt::DropIndex { if_exists, name } => {
            if let Some(ref s) = name.schema
                && db_set.find_db_id(s).is_none()
            {
                if *if_exists {
                    return Ok(Vec::new());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("unknown database {s}"),
                ));
            }
            match db_set.find_index_including_without_rowid_pk_in_set(name) {
                None => {
                    if !if_exists {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such index: {}", name.display_name()),
                        ));
                    }
                }
                Some((_, idx)) => {
                    if idx.is_autoindex {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "index associated with UNIQUE or PRIMARY KEY constraint cannot be dropped",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::AlterTable { table, action } => {
            validate_drop_or_alter_schema_table_in_set(db_set, table, "altered")?;
            let (db_id, tbl) = match db_set.find_relation_in_set(table)? {
                Some((_, ResolvedRelation::View(v))) => {
                    return Err(alter_view_rejection_error(&v.name, action));
                }
                Some((id, ResolvedRelation::Table(t))) => (id, *t),
                None => {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such table: {}", table.display_name()),
                    ));
                }
            };
            if is_reserved_sqlite_name(&tbl.name) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} may not be altered", tbl.name),
                ));
            }
            let slot = db_set
                .slot(db_id)
                .ok_or_else(|| SqliteError::misuse("database slot closed"))?;
            match action {
                AlterTableAction::RenameTo(new_name) => {
                    if !writable_schema && is_reserved_sqlite_name(new_name) {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("object name reserved for internal use: {new_name}"),
                        ));
                    }
                    if slot.schema.find_user_table(new_name).is_some()
                        || slot.schema.find_view(new_name).is_some()
                        || slot.schema.find_index(new_name).is_some()
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "there is already another table or index with this name: {new_name}"
                            ),
                        ));
                    }
                }
                AlterTableAction::RenameColumn { old_name, new_name } => {
                    if tbl.find_column(old_name).is_none() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such column: \"{old_name}\""),
                        ));
                    }
                    check_alter_rowid_alias(&tbl, old_name, "rename from")?;
                    check_alter_rowid_alias(&tbl, new_name, "rename to")?;
                    if !old_name.eq_ignore_ascii_case(new_name)
                        && tbl.find_column(new_name).is_some()
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("duplicate column name: {new_name}"),
                        ));
                    }
                }
                AlterTableAction::DropColumn(col_name) => {
                    let Some((_, col)) = tbl.find_column(col_name) else {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("no such column: \"{col_name}\""),
                        ));
                    };
                    check_alter_rowid_alias(&tbl, col_name, "drop")?;
                    if col.is_primary_key {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("cannot drop PRIMARY KEY column: \"{}\"", col.name),
                        ));
                    }
                    if col.is_unique {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("cannot drop UNIQUE column: \"{}\"", col.name),
                        ));
                    }
                    if tbl.columns.len() <= 1 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "cannot drop column \"{}\": no other columns exist",
                                col.name
                            ),
                        ));
                    }
                }
                AlterTableAction::AddColumn(cdef) => {
                    if tbl.find_column(&cdef.name).is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("duplicate column name: {}", cdef.name),
                        ));
                    }
                    check_alter_rowid_alias(&tbl, &cdef.name, "add")?;
                    let validated = build_column_schema_from_def(&tbl.name, tbl.strict, cdef)?;
                    if validated
                        .generated
                        .as_ref()
                        .is_some_and(|(_, stored)| *stored)
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "cannot add a STORED column",
                        ));
                    }
                    let has_fk = cdef
                        .constraints
                        .iter()
                        .any(|c| matches!(c, sqliters_syntax::ColumnConstraint::ForeignKey { .. }));
                    if has_fk
                        && foreign_keys
                        && validated
                            .default_expr
                            .as_ref()
                            .is_some_and(|e| !matches!(e, Expr::Null))
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            "Cannot add a REFERENCES column with non-NULL default value",
                        ));
                    }
                }
            }
            Ok(Vec::new())
        }

        Stmt::Insert(ins) => {
            let cols = validate_insert_static(
                ins,
                db_set,
                writable_schema,
                functions,
                collations,
                count_changes,
            )?;
            reject_raise_in_top_level_insert(ins, db_set)?;
            Ok(cols)
        }
        Stmt::Update(upd) => {
            let cols = validate_update_static(
                upd,
                db_set,
                writable_schema,
                functions,
                collations,
                count_changes,
            )?;
            reject_raise_in_top_level_update(upd, db_set)?;
            Ok(cols)
        }
        Stmt::Delete(del) => {
            let cols =
                validate_delete_static(del, db_set, writable_schema, functions, count_changes)?;
            reject_raise_in_top_level_delete(del, db_set)?;
            Ok(cols)
        }
    }
}

fn infer_pragma_columns(name: &str, value: Option<&PragmaValue>) -> Vec<String> {
    match name.to_ascii_uppercase().as_str() {
        "INTEGRITY_CHECK" | "QUICK_CHECK" => vec!["integrity_check".to_owned()],
        "PAGE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["page_size".to_owned()]
            }
        }
        "AUTO_VACUUM" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["auto_vacuum".to_owned()]
            }
        }
        "INCREMENTAL_VACUUM" => Vec::new(),
        "DEFAULT_CACHE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["cache_size".to_owned()]
            }
        }
        "TEMP_STORE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["temp_store".to_owned()]
            }
        }
        "WRITABLE_SCHEMA" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["writable_schema".to_owned()]
            }
        }
        "LEGACY_ALTER_TABLE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["legacy_alter_table".to_owned()]
            }
        }
        "PAGE_COUNT" => vec!["page_count".to_owned()],
        "MAX_PAGE_COUNT" => vec!["max_page_count".to_owned()],
        "FREELIST_COUNT" => vec!["freelist_count".to_owned()],
        "USER_VERSION" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["user_version".to_owned()]
            }
        }
        "APPLICATION_ID" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["application_id".to_owned()]
            }
        }
        "SCHEMA_VERSION" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["schema_version".to_owned()]
            }
        }
        "ENCODING" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["encoding".to_owned()]
            }
        }
        "JOURNAL_MODE" => vec!["journal_mode".to_owned()],
        "WAL_CHECKPOINT" => vec![
            "busy".to_owned(),
            "log".to_owned(),
            "checkpointed".to_owned(),
        ],
        "WAL_AUTOCHECKPOINT" => vec!["wal_autocheckpoint".to_owned()],
        "LOCKING_MODE" => vec!["locking_mode".to_owned()],
        "JOURNAL_SIZE_LIMIT" => vec!["journal_size_limit".to_owned()],
        "LOCK_STATUS" => vec!["database".to_owned(), "status".to_owned()],
        "FOREIGN_KEYS" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["foreign_keys".to_owned()]
            }
        }
        "FOREIGN_KEY_LIST" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "id".to_owned(),
                    "seq".to_owned(),
                    "table".to_owned(),
                    "from".to_owned(),
                    "to".to_owned(),
                    "on_update".to_owned(),
                    "on_delete".to_owned(),
                    "match".to_owned(),
                ]
            }
        }
        "FOREIGN_KEY_CHECK" => vec![
            "table".to_owned(),
            "rowid".to_owned(),
            "parent".to_owned(),
            "fkid".to_owned(),
        ],
        "SYNCHRONOUS" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["synchronous".to_owned()]
            }
        }
        "CACHE_SIZE" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["cache_size".to_owned()]
            }
        }
        "DATABASE_LIST" => vec!["seq".to_owned(), "name".to_owned(), "file".to_owned()],
        "TABLE_INFO" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "cid".to_owned(),
                    "name".to_owned(),
                    "type".to_owned(),
                    "notnull".to_owned(),
                    "dflt_value".to_owned(),
                    "pk".to_owned(),
                ]
            }
        }
        "TABLE_XINFO" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "cid".to_owned(),
                    "name".to_owned(),
                    "type".to_owned(),
                    "notnull".to_owned(),
                    "dflt_value".to_owned(),
                    "pk".to_owned(),
                    "hidden".to_owned(),
                ]
            }
        }
        "INDEX_LIST" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec![
                    "seq".to_owned(),
                    "name".to_owned(),
                    "unique".to_owned(),
                    "origin".to_owned(),
                    "partial".to_owned(),
                ]
            }
        }
        "COUNT_CHANGES" => {
            if value.is_some() {
                Vec::new()
            } else {
                vec!["count_changes".to_owned()]
            }
        }
        "INDEX_INFO" => {
            if value.is_none() {
                Vec::new()
            } else {
                vec!["seqno".to_owned(), "cid".to_owned(), "name".to_owned()]
            }
        }
        "COMPILE_OPTIONS" => vec!["compile_options".to_owned()],
        _ => Vec::new(),
    }
}

fn make_static_table_scope(tbl: &TableSchema, schema_name: &str, alias: Option<&str>) -> EvalRow {
    make_single_table_scope(
        tbl,
        schema_name,
        alias,
        0,
        &vec![Value::Null; tbl.columns.len()],
    )
}

fn make_static_returning_scope(tbl: &TableSchema) -> EvalRow {
    make_returning_table_scope(tbl, 0, &vec![Value::Null; tbl.columns.len()])
}

fn validate_index_hint_static(
    schema: &Schema,
    tbl: &TableSchema,
    indexed_by: Option<&IndexHint>,
    where_hint: Option<&Expr>,
) -> Result<()> {
    if let Some(IndexHint::IndexedBy(idx_name)) = indexed_by {
        let Some(idx) = schema
            .indexes_for_table(&tbl.name)
            .into_iter()
            .find(|i| !i.is_autoindex && i.name.eq_ignore_ascii_case(idx_name))
        else {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such index: {idx_name}"),
            ));
        };
        if let Some(ref pw) = idx.where_clause {
            let usable = where_hint.is_some_and(|wh| expr_contains_conjunct(wh, pw));
            if !usable {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("cannot use index: {idx_name}"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_cte_scope_static(ctes_list: &[CommonTableExpr]) -> Result<()> {
    let mut seen: Vec<&str> = Vec::with_capacity(ctes_list.len());
    for cte in ctes_list {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(&cte.name)) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!("duplicate WITH table name: {}", cte.name),
            ));
        }
        seen.push(&cte.name);
        if let Some(ref cols) = cte.columns {
            let mut cseen: Vec<&str> = Vec::with_capacity(cols.len());
            for cn in cols {
                if cseen.iter().any(|x| x.eq_ignore_ascii_case(cn)) {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("duplicate column name: {cn}"),
                    ));
                }
                cseen.push(cn);
            }
        }
    }
    Ok(())
}

fn validate_insert_static(
    ins: &InsertStmt,
    db_set: &DatabaseSet,
    writable_schema: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&ins.ctes)?;
    let (db_id, tbl, slot_name, is_catalog) = resolve_dml_target_table_in_set_opts(
        db_set,
        &ins.table,
        writable_schema,
        !ins.upsert_clauses.is_empty(),
    )?;
    let dml_ctes = CteStore::default().with_scope(&ins.ctes);
    let mut visited = Vec::new();

    let expected_width = if let Some(ref cnames) = ins.columns {
        for cn in cnames {
            if let Some((_, col)) = tbl.find_column(cn) {
                if col.generated.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot INSERT into generated column \"{}\"", col.name),
                    ));
                }
            } else if !tbl.is_implicit_rowid_name(cn) {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("table {} has no column named {cn}", tbl.name),
                ));
            }
        }
        cnames.len()
    } else {
        tbl.columns.iter().filter(|c| c.generated.is_none()).count()
    };

    match &ins.source {
        InsertSource::DefaultValues => {}
        InsertSource::Values(expr_rows) => {
            for erow in expr_rows {
                if erow.len() != expected_width {
                    if ins.columns.is_some() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("{} values for {} columns", erow.len(), expected_width),
                        ));
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} columns but {} values were supplied",
                            tbl.name,
                            expected_width,
                            erow.len()
                        ),
                    ));
                }
                for e in erow {
                    validate_expr_static(
                        e,
                        &[],
                        db_set,
                        &dml_ctes,
                        functions,
                        false,
                        false,
                        &mut visited,
                    )?;
                }
            }
        }
        InsertSource::Select(sel) => {
            let sel_cols =
                validate_select_stmt_static(sel, &[], db_set, &dml_ctes, functions, &mut visited)?;
            let xfer_gen_match = ins.columns.is_none()
                && tbl.columns.iter().any(|c| c.generated.is_some())
                && sel_cols.len() == tbl.columns.len()
                && check_insert_xfer_eligible(
                    ins,
                    db_id,
                    &tbl,
                    is_catalog,
                    db_set,
                    false,
                    count_changes,
                )
                .is_some();
            if sel_cols.len() != expected_width && !xfer_gen_match {
                if ins.columns.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("{} values for {} columns", sel_cols.len(), expected_width),
                    ));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "table {} has {} columns but {} values were supplied",
                        tbl.name,
                        expected_width,
                        sel_cols.len()
                    ),
                ));
            }
        }
    }

    if !ins.upsert_clauses.is_empty() {
        let indexes = if is_catalog {
            Vec::new()
        } else {
            db_set
                .slot(db_id)
                .filter(|s| s.btree.is_some())
                .map(|s| s.schema.indexes_for_table(&tbl.name))
                .unwrap_or_default()
        };
        let ordered_uniques = ordered_unique_indexes(&tbl, &indexes);
        let _ = analyze_upsert_clauses(
            ins,
            &tbl,
            &slot_name,
            &ordered_uniques,
            db_set,
            &dml_ctes,
            functions,
            collations,
            &mut visited,
        )?;
    }

    if !ins.returning.is_empty() {
        let sample_scope = make_static_returning_scope(&tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&ins.returning, &sample_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&sample_scope),
                db_set,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_update_static(
    upd: &UpdateStmt,
    db_set: &DatabaseSet,
    writable_schema: bool,
    functions: &FunctionRegistry,
    collations: &CollationRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&upd.ctes)?;
    let (db_id, tbl, slot_name, is_catalog) =
        resolve_dml_target_table_in_set(db_set, &upd.table, writable_schema)?;
    let empty_schema = Schema::default();
    let slot_schema = if is_catalog {
        &empty_schema
    } else {
        db_set
            .slot(db_id)
            .filter(|s| s.btree.is_some())
            .map_or(&empty_schema, |s| &s.schema)
    };
    validate_index_hint_static(
        slot_schema,
        &tbl,
        upd.indexed_by.as_ref(),
        upd.selection.as_ref(),
    )?;
    let dml_ctes = CteStore::default().with_scope(&upd.ctes);
    let target_scope = make_static_table_scope(&tbl, &slot_name, upd.alias.as_deref());
    let mut visited = Vec::new();

    let eval_scope = if let Some(ref fc) = upd.from {
        if 2 + fc.joins.len() > 64 {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "at most 64 tables in a join",
            ));
        }
        // `SF_NestedFrom` (`update.c:927`): validate `FROM` independently without `target_scope`!
        let from_cols =
            validate_from_clause_static(fc, None, &[], db_set, &dml_ctes, functions, &mut visited)?;
        check_update_from_same_src_alias(
            db_id,
            &tbl.name,
            upd.alias.as_deref(),
            fc,
            db_set,
            &dml_ctes,
        )?;
        let mut combined_cols = Vec::with_capacity(target_scope.cols.len() + from_cols.len());
        combined_cols.extend_from_slice(&target_scope.cols);
        combined_cols.extend(from_cols);
        let n_cols = combined_cols.len();
        EvalRow::new(combined_cols.into(), vec![Value::Null; n_cols])
    } else {
        target_scope
    };

    validate_set_assignments_static(
        &tbl,
        &upd.assignments,
        std::slice::from_ref(&eval_scope),
        db_set,
        &dml_ctes,
        functions,
        collations,
        upd.from.is_some(),
        &mut visited,
    )?;

    if let Some(ref w) = upd.selection {
        validate_collations_in_expr(w, collations)?;
        validate_expr_static(
            w,
            std::slice::from_ref(&eval_scope),
            db_set,
            &dml_ctes,
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    if !upd.returning.is_empty() {
        let ret_scope = make_static_returning_scope(&tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&upd.returning, &ret_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&ret_scope),
                db_set,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_delete_static(
    del: &DeleteStmt,
    db_set: &DatabaseSet,
    writable_schema: bool,
    functions: &FunctionRegistry,
    count_changes: bool,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&del.ctes)?;
    let (db_id, tbl, slot_name, is_catalog) =
        resolve_dml_target_table_in_set(db_set, &del.table, writable_schema)?;
    let empty_schema = Schema::default();
    let slot_schema = if is_catalog {
        &empty_schema
    } else {
        db_set
            .slot(db_id)
            .filter(|s| s.btree.is_some())
            .map_or(&empty_schema, |s| &s.schema)
    };
    validate_index_hint_static(
        slot_schema,
        &tbl,
        del.indexed_by.as_ref(),
        del.selection.as_ref(),
    )?;
    let dml_ctes = CteStore::default().with_scope(&del.ctes);
    let scope = make_static_table_scope(&tbl, &slot_name, del.alias.as_deref());
    let mut visited = Vec::new();

    if let Some(ref w) = del.selection {
        validate_expr_static(
            w,
            std::slice::from_ref(&scope),
            db_set,
            &dml_ctes,
            functions,
            false,
            false,
            &mut visited,
        )?;
    }

    if !del.returning.is_empty() {
        let ret_scope = make_static_returning_scope(&tbl);
        let (ret_cols, ret_expanded) =
            expand_returning_projection(&del.returning, &ret_scope.cols)?;
        for (pe, _, _) in &ret_expanded {
            validate_expr_static(
                pe,
                std::slice::from_ref(&ret_scope),
                db_set,
                &dml_ctes,
                functions,
                false,
                false,
                &mut visited,
            )?;
        }
        return Ok(ret_cols);
    }

    if count_changes {
        Ok(vec!["count".to_owned()])
    } else {
        Ok(Vec::new())
    }
}

fn validate_select_stmt_static(
    stmt: &SelectStmt,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    parent_ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    validate_cte_scope_static(&stmt.ctes)?;
    let ctes = parent_ctes.with_scope(&stmt.ctes);

    let result_cols = if stmt.body.compounds.is_empty() {
        validate_single_select_core_static(
            &stmt.body.head,
            &stmt.order_by,
            outer_scopes,
            db_set,
            &ctes,
            functions,
            visited_ctes,
        )?
    } else {
        // Validate compound arms from rightmost to leftmost (`p` -> `p->pPrior` in `sqlite3WalkSelect`),
        // then check arm-width compatibility in forward (`0..compounds.len()`) order.
        let mut rev_rhs_cols = Vec::with_capacity(stmt.body.compounds.len());
        for (_, rhs_core) in stmt.body.compounds.iter().rev() {
            rev_rhs_cols.push(validate_single_select_core_static(
                rhs_core,
                &[],
                outer_scopes,
                db_set,
                &ctes,
                functions,
                visited_ctes,
            )?);
        }
        let head_cols = validate_single_select_core_static(
            &stmt.body.head,
            &[],
            outer_scopes,
            db_set,
            &ctes,
            functions,
            visited_ctes,
        )?;
        rev_rhs_cols.reverse();
        let mut all_arm_cols = vec![head_cols.clone()];
        for ((op, _), rhs_cols) in stmt.body.compounds.iter().zip(rev_rhs_cols) {
            if rhs_cols.len() != head_cols.len() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "SELECTs to the left and right of {} do not have the same number of result columns",
                        op.as_sql_str()
                    ),
                ));
            }
            all_arm_cols.push(rhs_cols);
        }

        let width = head_cols.len();
        for (ob_idx, ob) in stmt.order_by.iter().enumerate() {
            let mut inner = &ob.expr;
            while let Expr::Collate { expr, .. } = inner {
                inner = expr.as_ref();
            }
            if let Some(ord) = strip_collate_and_unary_plus(inner) {
                if ord < 1 || (ord as usize) > width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "{} ORDER BY term out of range - should be between 1 and {}",
                            ordinal_suffix(ob_idx + 1),
                            width
                        ),
                    ));
                }
                continue;
            }
            let target_label = match inner {
                Expr::Column(ColumnRef {
                    schema: None,
                    table: None,
                    column,
                }) => column.clone(),
                other => expr_column_label(other),
            };
            let matched = all_arm_cols
                .iter()
                .any(|arm| arm.iter().any(|cn| cn.eq_ignore_ascii_case(&target_label)));
            if !matched {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "{} ORDER BY term does not match any column in the result set",
                        ordinal_suffix(ob_idx + 1)
                    ),
                ));
            }
        }
        head_cols
    };

    if let Some(ref lim) = stmt.limit {
        validate_expr_static(
            &lim.limit,
            outer_scopes,
            db_set,
            &ctes,
            functions,
            false,
            false,
            visited_ctes,
        )?;
        if let Some(ref off) = lim.offset {
            validate_expr_static(
                off,
                outer_scopes,
                db_set,
                &ctes,
                functions,
                false,
                false,
                visited_ctes,
            )?;
        }
    }

    Ok(result_cols)
}

fn validate_single_select_core_static(
    core: &SelectCore,
    order_by: &[OrderByTerm],
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    match core {
        SelectCore::Values(expr_rows) => {
            let Some(first) = expr_rows.first() else {
                return Ok(Vec::new());
            };
            let width = first.len();
            for erow in expr_rows {
                if erow.len() != width {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        "all VALUES must have the same number of terms",
                    ));
                }
                for e in erow {
                    validate_expr_static(
                        e,
                        outer_scopes,
                        db_set,
                        ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }
            let col_names: Vec<String> = (1..=width).map(|i| format!("column{i}")).collect();
            if col_names.len() > db_set.column_limit() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "too many columns in result set",
                ));
            }
            if !order_by.is_empty() {
                for (ob_idx, ob) in order_by.iter().enumerate() {
                    let mut inner = &ob.expr;
                    while let Expr::Collate { expr, .. } = inner {
                        inner = expr.as_ref();
                    }
                    if let Some(ord) = strip_collate_and_unary_plus(inner) {
                        if ord < 1 || (ord as usize) > width {
                            return Err(SqliteError::new(
                                ErrorCode::Error,
                                format!(
                                    "{} ORDER BY term out of range - should be between 1 and {}",
                                    ordinal_suffix(ob_idx + 1),
                                    width
                                ),
                            ));
                        }
                        continue;
                    }
                    let target_label = match inner {
                        Expr::Column(ColumnRef {
                            schema: None,
                            table: None,
                            column,
                        }) => column.clone(),
                        other => expr_column_label(other),
                    };
                    if !col_names
                        .iter()
                        .any(|cn| cn.eq_ignore_ascii_case(&target_label))
                    {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} ORDER BY term does not match any column in the result set",
                                ordinal_suffix(ob_idx + 1)
                            ),
                        ));
                    }
                }
            }
            Ok(col_names)
        }
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            let sample_cols = match from {
                None => Vec::new(),
                Some(fc) => validate_from_clause_static(
                    fc,
                    selection.as_ref(),
                    outer_scopes,
                    db_set,
                    ctes,
                    functions,
                    visited_ctes,
                )?,
            };

            if from.is_none() && projection.iter().any(|rc| matches!(rc, ResultColumn::Star)) {
                return Err(SqliteError::new(ErrorCode::Error, "no tables specified"));
            }
            let (col_names, expanded_proj) = expand_projection(projection, &sample_cols)?;
            if col_names.len() > db_set.column_limit() {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    "too many columns in result set",
                ));
            }
            let row_eval = EvalRow::new(
                Rc::from(sample_cols.as_slice()),
                vec![Value::Null; sample_cols.len()],
            );
            let full_scopes = push_scope(outer_scopes, &row_eval);

            if let Some(where_expr) = selection {
                let subst_where = subst_select_aliases_in_expr_with_ctes(
                    where_expr,
                    &expanded_proj,
                    &sample_cols,
                    db_set,
                    ctes,
                );
                validate_expr_static(
                    &subst_where,
                    &full_scopes,
                    db_set,
                    ctes,
                    functions,
                    false,
                    false,
                    visited_ctes,
                )?;
            }

            let is_agg = !group_by.is_empty()
                || having.is_some()
                || projection.iter().any(|rc| match rc {
                    ResultColumn::Expr { expr, .. } => expr_contains_aggregate(expr),
                    _ => false,
                })
                || order_by.iter().any(|ob| expr_contains_aggregate(&ob.expr));

            for (pe, _, _) in &expanded_proj {
                validate_expr_static(
                    pe,
                    &full_scopes,
                    db_set,
                    ctes,
                    functions,
                    is_agg,
                    false,
                    visited_ctes,
                )?;
            }

            for (gi, ge) in group_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(ge) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} GROUP BY term out of range - should be between 1 and {}",
                                ordinal_suffix(gi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                } else {
                    let subst = subst_select_aliases_in_expr_with_ctes(
                        ge,
                        &expanded_proj,
                        &sample_cols,
                        db_set,
                        ctes,
                    );
                    validate_expr_static(
                        &subst,
                        &full_scopes,
                        db_set,
                        ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }

            if let Some(hav) = having {
                let subst = subst_select_aliases_in_expr_with_ctes(
                    hav,
                    &expanded_proj,
                    &sample_cols,
                    db_set,
                    ctes,
                );
                validate_expr_static(
                    &subst,
                    &full_scopes,
                    db_set,
                    ctes,
                    functions,
                    true,
                    false,
                    visited_ctes,
                )?;
            }

            for (oi, ob) in order_by.iter().enumerate() {
                if let Some(ord) = strip_collate_and_unary_plus(&ob.expr) {
                    if ord < 1 || (ord as usize) > expanded_proj.len() {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "{} ORDER BY term out of range - should be between 1 and {}",
                                ordinal_suffix(oi + 1),
                                expanded_proj.len()
                            ),
                        ));
                    }
                } else {
                    let subst = subst_order_by_aliases_static(
                        &ob.expr,
                        &expanded_proj,
                        &sample_cols,
                        outer_scopes,
                    );
                    validate_expr_static(
                        &subst,
                        &full_scopes,
                        db_set,
                        ctes,
                        functions,
                        is_agg,
                        false,
                        visited_ctes,
                    )?;
                }
            }

            Ok(col_names)
        }
    }
}

fn subst_order_by_aliases_static(
    expr: &Expr,
    expanded_proj: &[(Expr, String, Option<String>)],
    sample_cols: &[ScopedColumn],
    outer_scopes: &[EvalRow],
) -> Expr {
    match expr {
        Expr::Column(ColumnRef {
            schema: None,
            table: None,
            column,
        }) => {
            let in_sample = sample_cols.iter().any(|sc| {
                if sc.hidden_rowid {
                    column.eq_ignore_ascii_case("rowid")
                        || column.eq_ignore_ascii_case("_rowid_")
                        || column.eq_ignore_ascii_case("oid")
                } else {
                    sc.name.eq_ignore_ascii_case(column)
                }
            });
            let in_outer = outer_scopes
                .iter()
                .rev()
                .any(|s| s.lookup(None, column).ok().flatten().is_some());
            if !in_sample
                && !in_outer
                && expanded_proj.iter().any(|(_, label, al)| {
                    al.as_ref().is_some_and(|a| a.eq_ignore_ascii_case(column))
                        || label.eq_ignore_ascii_case(column)
                })
            {
                return Expr::Null;
            }
            expr.clone()
        }
        Expr::Unary { op, expr: inner } => Expr::Unary {
            op: *op,
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::Collate {
            expr: inner,
            collation,
        } => Expr::Collate {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            collation: collation.clone(),
        },
        Expr::Cast {
            expr: inner,
            type_name,
        } => Expr::Cast {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            type_name: type_name.clone(),
        },
        Expr::IsNull {
            expr: inner,
            negated,
        } => Expr::IsNull {
            expr: Box::new(subst_order_by_aliases_static(
                inner,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(subst_order_by_aliases_static(
                left,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            op: *op,
            right: Box::new(subst_order_by_aliases_static(
                right,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
        },
        Expr::Between {
            expr: val,
            low,
            high,
            negated,
        } => Expr::Between {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            low: Box::new(subst_order_by_aliases_static(
                low,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            high: Box::new(subst_order_by_aliases_static(
                high,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            negated: *negated,
        },
        Expr::InList {
            expr: val,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            list: list
                .iter()
                .map(|e| subst_order_by_aliases_static(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
            negated: *negated,
        },
        Expr::InTable {
            expr: val,
            table,
            negated,
        } => Expr::InTable {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            table: table.clone(),
            negated: *negated,
        },
        Expr::Like {
            expr: val,
            pattern,
            escape,
            op,
            negated,
        } => Expr::Like {
            expr: Box::new(subst_order_by_aliases_static(
                val,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            pattern: Box::new(subst_order_by_aliases_static(
                pattern,
                expanded_proj,
                sample_cols,
                outer_scopes,
            )),
            escape: escape.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            op: *op,
            negated: *negated,
        },
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => Expr::Case {
            operand: operand.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
            branches: branches
                .iter()
                .map(|(w, t)| {
                    (
                        subst_order_by_aliases_static(w, expanded_proj, sample_cols, outer_scopes),
                        subst_order_by_aliases_static(t, expanded_proj, sample_cols, outer_scopes),
                    )
                })
                .collect(),
            else_expr: else_expr.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => Expr::FunctionCall {
            name: name.clone(),
            distinct: *distinct,
            args: match args {
                FunctionArgs::Star => FunctionArgs::Star,
                FunctionArgs::Exprs(es) => FunctionArgs::Exprs(
                    es.iter()
                        .map(|e| {
                            subst_order_by_aliases_static(
                                e,
                                expanded_proj,
                                sample_cols,
                                outer_scopes,
                            )
                        })
                        .collect(),
                ),
            },
            filter: filter.as_ref().map(|e| {
                Box::new(subst_order_by_aliases_static(
                    e,
                    expanded_proj,
                    sample_cols,
                    outer_scopes,
                ))
            }),
        },
        Expr::RowValue(elems) => Expr::RowValue(
            elems
                .iter()
                .map(|e| subst_order_by_aliases_static(e, expanded_proj, sample_cols, outer_scopes))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

fn validate_from_clause_static(
    fc: &FromClause,
    where_hint: Option<&Expr>,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<ScopedColumn>> {
    let head_hint = if fc.joins.is_empty() {
        where_hint
    } else {
        None
    };
    let mut cur_cols = validate_tos_static(
        &fc.head,
        head_hint,
        outer_scopes,
        db_set,
        ctes,
        functions,
        visited_ctes,
    )?;

    for join in &fc.joins {
        let right_cols = validate_tos_static(
            &join.right,
            None,
            outer_scopes,
            db_set,
            ctes,
            functions,
            visited_ctes,
        )?;

        if join.natural && join.constraint.is_some() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "a NATURAL join may not have an ON or USING clause",
            ));
        }

        if let Some(JoinConstraint::Using(ref ucols)) = join.constraint {
            for u in ucols {
                let in_left = cur_cols
                    .iter()
                    .any(|c| !c.hidden_rowid && c.name.eq_ignore_ascii_case(u));
                let in_right = right_cols
                    .iter()
                    .any(|c| !c.hidden_rowid && c.name.eq_ignore_ascii_case(u));
                if !in_left || !in_right {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot join using column {u} - column not present in both tables"),
                    ));
                }
            }
        }

        let merged_cols = merge_join_scoped_columns(&cur_cols, &right_cols, join);
        if let Some(JoinConstraint::On(ref on_expr)) = join.constraint {
            let join_eval = EvalRow::new(
                Rc::from(merged_cols.as_slice()),
                vec![Value::Null; merged_cols.len()],
            );
            let join_scopes = push_scope(outer_scopes, &join_eval);
            validate_expr_static(
                on_expr,
                &join_scopes,
                db_set,
                ctes,
                functions,
                false,
                false,
                visited_ctes,
            )?;
        }
        cur_cols = merged_cols;
    }

    Ok(cur_cols)
}

fn validate_tos_static(
    tos: &TableOrSubquery,
    where_hint: Option<&Expr>,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<ScopedColumn>> {
    ctes.record_expanded_tab();
    match tos {
        TableOrSubquery::Table {
            name,
            table_args,
            alias,
            indexed_by,
        } => {
            if table_args.is_some() {
                let exists_non_tvf = (name.schema.is_none() && ctes.locate(&name.name).is_some())
                    || db_set.find_relation_in_set(name)?.is_some()
                    || db_set.resolve_catalog_table(name)?.is_some();
                if exists_non_tvf {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("'{}' is not a function", name.name),
                    ));
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such table: {}", name.display_name()),
                ));
            }
            let eff_alias = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                if indexed_by.is_some() {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        match indexed_by {
                            Some(IndexHint::NotIndexed) => {
                                format!("cannot use NOT INDEXED on CTE: {}", name.name)
                            }
                            Some(IndexHint::IndexedBy(idx)) => format!("no such index: {idx}"),
                            None => unreachable!(),
                        },
                    ));
                }
                let cte_cols = resolve_cte_columns_static(
                    s_idx,
                    c_idx,
                    outer_scopes,
                    db_set,
                    ctes,
                    functions,
                    visited_ctes,
                )?;
                let meta = infer_cte_meta_by_name(&name.name, outer_scopes, db_set, ctes);
                return Ok(cte_cols
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) = meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: None,
                            table_alias: Some(eff_alias.clone()),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect());
            }

            if db_set.resolve_catalog_table(name)?.is_none()
                && let Some((v_db_id, view_ref)) = db_set.find_view_in_set(name)
            {
                if let Some(IndexHint::IndexedBy(idx)) = indexed_by {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("no such index: {idx}"),
                    ));
                }
                let (v_cols, v_meta, schema_lbl) = resolve_view_columns_static(
                    v_db_id,
                    view_ref,
                    db_set,
                    ctes,
                    functions,
                    visited_ctes,
                )?;
                return Ok(v_cols
                    .into_iter()
                    .enumerate()
                    .map(|(i, cn)| {
                        let (aff, coll) =
                            v_meta.get(i).copied().unwrap_or((None, Collation::Binary));
                        ScopedColumn {
                            schema_name: Some(schema_lbl.clone()),
                            table_alias: Some(eff_alias.clone()),
                            name: cn,
                            collation: coll,
                            affinity: aff,
                            hidden_rowid: false,
                            omit_from_star: false,
                            merged_using: false,
                        }
                    })
                    .collect());
            }

            let (db_id, tbl, schema_lbl) = resolve_table_for_query_in_set(db_set, name)?;
            ctes.record_catalog_ref(db_id, &tbl.name)?;
            let empty_schema = Schema::default();
            let slot_schema = db_set
                .slot(db_id)
                .filter(|s| s.btree.is_some())
                .map_or(&empty_schema, |s| &s.schema);
            validate_index_hint_static(slot_schema, &tbl, indexed_by.as_ref(), where_hint)?;
            let mut cols: Vec<ScopedColumn> = tbl
                .columns
                .iter()
                .map(|c| ScopedColumn {
                    schema_name: Some(schema_lbl.clone()),
                    table_alias: Some(eff_alias.clone()),
                    name: c.name.clone(),
                    collation: c.collation,
                    affinity: Some(c.affinity),
                    hidden_rowid: false,
                    omit_from_star: false,
                    merged_using: false,
                })
                .collect();
            if !tbl.without_rowid {
                let rid_col_name = tbl
                    .rowid_alias_col
                    .map(|idx| tbl.columns[idx].name.clone())
                    .unwrap_or_else(|| "rowid".to_owned());
                cols.push(ScopedColumn {
                    schema_name: Some(schema_lbl),
                    table_alias: Some(eff_alias),
                    name: rid_col_name,
                    collation: Collation::Binary,
                    affinity: Some(TypeAffinity::Integer),
                    hidden_rowid: true,
                    omit_from_star: false,
                    merged_using: false,
                });
            }
            Ok(cols)
        }
        TableOrSubquery::Subquery { query, alias } => {
            let nested_ctes = ctes.enter_nested_select(db_set.expr_depth_limit())?;
            let sub_cols = validate_select_stmt_static(
                query,
                outer_scopes,
                db_set,
                &nested_ctes,
                functions,
                visited_ctes,
            )?;
            let sub_cols = disambiguate_view_column_names(&sub_cols);
            let sub_meta = infer_select_stmt_meta(query, outer_scopes, db_set, &nested_ctes);
            Ok(sub_cols
                .into_iter()
                .enumerate()
                .map(|(i, cn)| {
                    let (aff, coll) = sub_meta
                        .get(i)
                        .copied()
                        .unwrap_or((None, Collation::Binary));
                    ScopedColumn {
                        schema_name: None,
                        table_alias: alias.clone(),
                        name: cn,
                        collation: coll,
                        affinity: aff,
                        hidden_rowid: false,
                        omit_from_star: false,
                        merged_using: false,
                    }
                })
                .collect())
        }
    }
}

fn resolve_cte_columns_static(
    s_idx: usize,
    c_idx: usize,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<Vec<String>> {
    if let Some(w) = ctes.find_working(s_idx, c_idx) {
        return Ok(w.column_names.clone());
    }
    let cte = &ctes.scopes[s_idx][c_idx];
    if visited_ctes.contains(&(s_idx, c_idx)) {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        ));
    }
    let nested_ctes = ctes.enter_cte_select(db_set.expr_depth_limit())?;
    visited_ctes.push((s_idx, c_idx));

    let mut eval_ctes = nested_ctes.slice_to_scope(s_idx);
    eval_ctes.working.clone_from(&nested_ctes.working);

    let res = match classify_cte_recursion(cte) {
        CteRecursionKind::InvalidCircular => Err(SqliteError::new(
            ErrorCode::Error,
            format!("circular reference: {}", cte.name),
        )),
        CteRecursionKind::MultipleFromRefs => Err(SqliteError::new(
            ErrorCode::Error,
            format!("multiple references to recursive table: {}", cte.name),
        )),
        CteRecursionKind::MultipleRecursive => Err(SqliteError::new(
            ErrorCode::Error,
            format!("multiple recursive references: {}", cte.name),
        )),
        CteRecursionKind::NonRecursive => {
            if let Some(ref explicit_cols) = cte.columns {
                check_cte_explicit_head_width_static(
                    &cte.name,
                    explicit_cols,
                    &cte.query,
                    outer_scopes,
                    db_set,
                    &eval_ctes,
                    functions,
                    visited_ctes,
                )?;
            }
            let q_cols = validate_select_stmt_static(
                &cte.query,
                outer_scopes,
                db_set,
                &eval_ctes,
                functions,
                visited_ctes,
            )?;
            if let Some(ref explicit_cols) = cte.columns {
                if explicit_cols.len() != q_cols.len() {
                    Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} values for {} columns",
                            cte.name,
                            q_cols.len(),
                            explicit_cols.len()
                        ),
                    ))
                } else {
                    Ok(disambiguate_view_column_names(explicit_cols))
                }
            } else {
                Ok(disambiguate_view_column_names(&q_cols))
            }
        }
        CteRecursionKind::Recursive { first_rec_idx } => {
            eval_ctes.record_expanded_tab();
            let seed_body = SelectBody {
                head: cte.query.body.head.clone(),
                compounds: cte.query.body.compounds[..first_rec_idx].to_vec(),
            };
            let seed_stmt = SelectStmt {
                ctes: cte.query.ctes.clone(),
                body: seed_body.clone(),
                order_by: Vec::new(),
                limit: None,
            };
            if let Some(ref explicit_cols) = cte.columns {
                check_cte_explicit_head_width_static(
                    &cte.name,
                    explicit_cols,
                    &seed_stmt,
                    outer_scopes,
                    db_set,
                    &eval_ctes,
                    functions,
                    visited_ctes,
                )?;
            }
            let seed_cols = validate_select_stmt_static(
                &seed_stmt,
                outer_scopes,
                db_set,
                &eval_ctes,
                functions,
                visited_ctes,
            )?;
            let mut all_arm_col_names = Vec::with_capacity(1 + cte.query.body.compounds.len());
            all_arm_col_names.push(seed_cols.clone());
            let mut rec_ctes = eval_ctes.with_scope(&cte.query.ctes);
            for (_, sc) in &seed_body.compounds {
                all_arm_col_names.push(validate_single_select_core_static(
                    sc,
                    &[],
                    outer_scopes,
                    db_set,
                    &rec_ctes,
                    functions,
                    visited_ctes,
                )?);
            }

            let working_cols = if let Some(ref explicit_cols) = cte.columns {
                if explicit_cols.len() != seed_cols.len() {
                    visited_ctes.pop();
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "table {} has {} values for {} columns",
                            cte.name,
                            seed_cols.len(),
                            explicit_cols.len()
                        ),
                    ));
                }
                disambiguate_view_column_names(explicit_cols)
            } else {
                disambiguate_view_column_names(&seed_cols)
            };

            rec_ctes.working.push((
                s_idx,
                c_idx,
                QueryOutput {
                    column_names: working_cols.clone(),
                    rows: Vec::new(),
                },
            ));

            for (rec_op, rec_core) in &cte.query.body.compounds[first_rec_idx..] {
                if let Err(err) = check_recursive_arm_no_aggregates(rec_core) {
                    visited_ctes.pop();
                    return Err(err);
                }
                let rhs_cols = validate_single_select_core_static(
                    rec_core,
                    &[],
                    outer_scopes,
                    db_set,
                    &rec_ctes,
                    functions,
                    visited_ctes,
                )?;
                if rhs_cols.len() != working_cols.len() {
                    visited_ctes.pop();
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!(
                            "SELECTs to the left and right of {} do not have the same number of result columns",
                            rec_op.as_sql_str()
                        ),
                    ));
                }
                all_arm_col_names.push(rhs_cols);
            }

            if !cte.query.order_by.is_empty() {
                let default_colls = vec![Collation::Binary; working_cols.len()];
                if let Err(err) = resolve_compound_order_by_specs(
                    &all_arm_col_names,
                    &default_colls,
                    &cte.query.order_by,
                ) {
                    visited_ctes.pop();
                    return Err(err);
                }
            }

            if let Some(ref lim) = cte.query.limit {
                validate_expr_static(
                    &lim.limit,
                    outer_scopes,
                    db_set,
                    &rec_ctes,
                    functions,
                    false,
                    false,
                    visited_ctes,
                )?;
                if let Some(ref off) = lim.offset {
                    validate_expr_static(
                        off,
                        outer_scopes,
                        db_set,
                        &rec_ctes,
                        functions,
                        false,
                        false,
                        visited_ctes,
                    )?;
                }
            }
            Ok(working_cols)
        }
    };

    visited_ctes.pop();
    res
}

#[allow(clippy::too_many_arguments)]
fn check_cte_explicit_head_width_static(
    cte_name: &str,
    explicit_cols: &[String],
    stmt: &SelectStmt,
    outer_scopes: &[EvalRow],
    db_set: &DatabaseSet,
    parent_ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<()> {
    if let SelectCore::Select {
        from: None,
        projection,
        ..
    } = &stmt.body.head
        && projection
            .iter()
            .any(|rc| !matches!(rc, ResultColumn::Expr { .. }))
    {
        let non_star = projection
            .iter()
            .filter(|rc| matches!(rc, ResultColumn::Expr { .. }))
            .count();
        if non_star > 0 && non_star != explicit_cols.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "table {cte_name} has {non_star} values for {} columns",
                    explicit_cols.len()
                ),
            ));
        }
        return Ok(());
    }

    if stmt.body.compounds.is_empty() {
        return Ok(());
    }

    validate_cte_scope_static(&stmt.ctes)?;
    let mut scratch_ctes = parent_ctes.with_scope(&stmt.ctes);
    scratch_ctes.tab_refs = CatalogRefMap::default();
    scratch_ctes.expanded_tabs = Rc::new(Cell::new(parent_ctes.expanded_tabs.get()));

    for (_, rhs_core) in stmt.body.compounds.iter().rev() {
        let _ = validate_single_select_core_static(
            rhs_core,
            &[],
            outer_scopes,
            db_set,
            &scratch_ctes,
            functions,
            visited_ctes,
        )?;
    }
    let head_cols = validate_single_select_core_static(
        &stmt.body.head,
        &[],
        outer_scopes,
        db_set,
        &scratch_ctes,
        functions,
        visited_ctes,
    )?;
    if head_cols.len() != explicit_cols.len() {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!(
                "table {cte_name} has {} values for {} columns",
                head_cols.len(),
                explicit_cols.len()
            ),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_expr_static(
    expr: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    allow_agg: bool,
    inside_agg: bool,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<()> {
    match expr {
        Expr::Null
        | Expr::Bool(_)
        | Expr::Integer(_)
        | Expr::Float(_)
        | Expr::String(_)
        | Expr::Blob(_)
        | Expr::CurrentTimeKw(_)
        | Expr::Bind(_) => Ok(()),

        Expr::Column(cref) => {
            if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                if let Some(last_scope) = scopes.last()
                    && last_scope.lookup_cref_full(cref)?.is_some()
                {
                    return Ok(());
                }
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("no such column: {}", cref.display_name()),
                ));
            }
            for scope in scopes.iter().rev() {
                if scope.lookup_cref_full(cref)?.is_some() {
                    return Ok(());
                }
            }
            Err(SqliteError::new(
                ErrorCode::Error,
                format!("no such column: {}", cref.display_name()),
            ))
        }

        Expr::Collate { expr: inner, .. }
        | Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => validate_expr_static(
            inner,
            scopes,
            db_set,
            ctes,
            functions,
            allow_agg,
            inside_agg,
            visited_ctes,
        ),

        Expr::RowValue(elems) => {
            if elems.len() == 1 {
                validate_expr_static(
                    &elems[0],
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )
            } else {
                Err(SqliteError::new(ErrorCode::Error, "row value misused"))
            }
        }

        Expr::Binary { left, op, right } => {
            if matches!(
                op,
                BinaryOp::Lt
                    | BinaryOp::Le
                    | BinaryOp::Gt
                    | BinaryOp::Ge
                    | BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::Is
                    | BinaryOp::IsNot
            ) && (is_row_value_expr(left) || is_row_value_expr(right))
            {
                let lw = validate_tuple_expr_static(
                    left,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let rw = validate_tuple_expr_static(
                    right,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if lw != rw {
                    let is_left_sub = matches!(left.as_ref(), Expr::Subquery(_));
                    let is_right_sub = matches!(right.as_ref(), Expr::Subquery(_));
                    if is_left_sub && !is_right_sub {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("sub-select returns {lw} columns - expected {rw}"),
                        ));
                    }
                    if is_right_sub && !is_left_sub {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!("sub-select returns {rw} columns - expected {lw}"),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                return Ok(());
            }
            validate_expr_static(
                left,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                right,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )
        }

        Expr::Between {
            expr: val_e,
            low,
            high,
            ..
        } => {
            if is_row_value_expr(val_e) || is_row_value_expr(low) || is_row_value_expr(high) {
                let vw = validate_tuple_expr_static(
                    val_e,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let lw = validate_tuple_expr_static(
                    low,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                let hw = validate_tuple_expr_static(
                    high,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if vw != lw || vw != hw {
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
                return Ok(());
            }
            validate_expr_static(
                val_e,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                low,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                high,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )
        }

        Expr::InList {
            expr: lhs_e, list, ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            for item in list {
                let m = validate_tuple_expr_static(
                    item,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                if m != n {
                    if n >= 2 {
                        return Err(SqliteError::new(
                            ErrorCode::Error,
                            format!(
                                "IN(...) element has {m} term{} - expected {n}",
                                if m == 1 { "" } else { "s" }
                            ),
                        ));
                    }
                    return Err(SqliteError::new(ErrorCode::Error, "row value misused"));
                }
            }
            Ok(())
        }

        Expr::InSubquery {
            expr: lhs_e,
            subquery,
            ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            let sub_cols = validate_select_stmt_static(
                subquery,
                scopes,
                db_set,
                ctes,
                functions,
                visited_ctes,
            )?;
            if !sub_cols.is_empty() && sub_cols.len() != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!(
                        "sub-select returns {} columns - expected {n}",
                        sub_cols.len()
                    ),
                ));
            }
            Ok(())
        }

        Expr::InTable {
            expr: lhs_e, table, ..
        } => {
            let n = validate_tuple_expr_static(
                lhs_e,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            let m = if table.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&table.name)
            {
                resolve_cte_columns_static(
                    s_idx,
                    c_idx,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    visited_ctes,
                )?
                .len()
            } else if db_set.resolve_catalog_table(table)?.is_none()
                && let Some((v_db_id, view_ref)) = db_set.find_view_in_set(table)
            {
                resolve_view_columns_static(
                    v_db_id,
                    view_ref,
                    db_set,
                    ctes,
                    functions,
                    visited_ctes,
                )?
                .0
                .len()
            } else {
                let (_, tbl, _) = resolve_table_for_query_in_set(db_set, table)?;
                tbl.columns.len()
            };
            if m != n {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("sub-select returns {m} columns - expected {n}"),
                ));
            }
            Ok(())
        }

        Expr::Like {
            expr: txt_e,
            pattern,
            escape,
            ..
        } => {
            validate_expr_static(
                txt_e,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            validate_expr_static(
                pattern,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            if let Some(esc) = escape {
                validate_expr_static(
                    esc,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(())
        }

        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                validate_expr_static(
                    op,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            for (w, t) in branches {
                validate_expr_static(
                    w,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
                validate_expr_static(
                    t,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            if let Some(el) = else_expr {
                validate_expr_static(
                    el,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(())
        }

        Expr::FunctionCall {
            name,
            distinct,
            args,
            filter,
        } => {
            let (is_star, arg_count) = match args {
                FunctionArgs::Star => (true, 0),
                FunctionArgs::Exprs(es) => (false, es.len()),
            };
            functions.validate_call(
                name,
                is_star,
                arg_count,
                *distinct,
                filter.is_some(),
                allow_agg && !inside_agg,
            )?;
            let is_agg_fn = if !is_star && functions.find_scalar(name, arg_count).is_some() {
                false
            } else {
                match name.to_ascii_uppercase().as_str() {
                    "COUNT" | "SUM" | "TOTAL" | "AVG" | "GROUP_CONCAT" | "STRING_AGG" => true,
                    "MIN" | "MAX" => arg_count == 1,
                    _ => false,
                }
            };
            if is_agg_fn {
                if inside_agg || !allow_agg {
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("misuse of aggregate function {name}()"),
                    ));
                }
                if let FunctionArgs::Exprs(es) = args {
                    for ae in es {
                        validate_expr_static(
                            ae,
                            scopes,
                            db_set,
                            ctes,
                            functions,
                            false,
                            true,
                            visited_ctes,
                        )?;
                    }
                }
                if let Some(fe) = filter {
                    validate_expr_static(
                        fe,
                        scopes,
                        db_set,
                        ctes,
                        functions,
                        false,
                        true,
                        visited_ctes,
                    )?;
                }
            } else if let FunctionArgs::Exprs(es) = args {
                for ae in es {
                    validate_expr_static(
                        ae,
                        scopes,
                        db_set,
                        ctes,
                        functions,
                        allow_agg,
                        inside_agg,
                        visited_ctes,
                    )?;
                }
            }
            Ok(())
        }

        Expr::Subquery(sub) => {
            let sub_cols =
                validate_select_stmt_static(sub, scopes, db_set, ctes, functions, visited_ctes)?;
            if sub_cols.len() > 1 {
                return Err(SqliteError::new(
                    ErrorCode::Error,
                    format!("sub-select returns {} columns - expected 1", sub_cols.len()),
                ));
            }
            Ok(())
        }

        Expr::Exists { subquery, .. } => {
            let _ = validate_select_stmt_static(
                subquery,
                scopes,
                db_set,
                ctes,
                functions,
                visited_ctes,
            )?;
            Ok(())
        }

        Expr::Raise { message, .. } => {
            if let Some(m) = message {
                validate_expr_static(
                    m,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(())
        }
    }
}

fn reject_raise_in_top_level_select(
    sel: &SelectStmt,
    db_set: &DatabaseSet,
    parent_ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    reject_raise_outside_trigger_select(sel)?;
    let ctes = parent_ctes.with_scope(&sel.ctes);
    for cte in &sel.ctes {
        reject_raise_in_top_level_select(&cte.query, db_set, &ctes, visited_views)?;
    }
    reject_raise_in_top_level_core(&sel.body.head, db_set, &ctes, visited_views)?;
    for (_, core) in &sel.body.compounds {
        reject_raise_in_top_level_core(core, db_set, &ctes, visited_views)?;
    }
    for ob in &sel.order_by {
        reject_raise_in_top_level_expr(&ob.expr, db_set, &ctes, visited_views)?;
    }
    if let Some(ref lim) = sel.limit {
        reject_raise_in_top_level_expr(&lim.limit, db_set, &ctes, visited_views)?;
        if let Some(ref off) = lim.offset {
            reject_raise_in_top_level_expr(off, db_set, &ctes, visited_views)?;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_core(
    core: &SelectCore,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    match core {
        SelectCore::Select {
            projection,
            from,
            selection,
            group_by,
            having,
            ..
        } => {
            if let Some(fc) = from {
                reject_raise_in_top_level_from(fc, db_set, ctes, visited_views)?;
            }
            for rc in projection {
                if let ResultColumn::Expr { expr, .. } = rc {
                    reject_raise_in_top_level_expr(expr, db_set, ctes, visited_views)?;
                }
            }
            if let Some(w) = selection {
                reject_raise_in_top_level_expr(w, db_set, ctes, visited_views)?;
            }
            for g in group_by {
                reject_raise_in_top_level_expr(g, db_set, ctes, visited_views)?;
            }
            if let Some(h) = having {
                reject_raise_in_top_level_expr(h, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        SelectCore::Values(rows) => {
            for row in rows {
                for e in row {
                    reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
                }
            }
            Ok(())
        }
    }
}

fn reject_raise_in_top_level_from(
    fc: &FromClause,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    reject_raise_in_top_level_tos(&fc.head, db_set, ctes, visited_views)?;
    for j in &fc.joins {
        reject_raise_in_top_level_tos(&j.right, db_set, ctes, visited_views)?;
        if let Some(JoinConstraint::On(ref on_e)) = j.constraint {
            reject_raise_in_top_level_expr(on_e, db_set, ctes, visited_views)?;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_tos(
    tos: &TableOrSubquery,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    match tos {
        TableOrSubquery::Table {
            name, table_args, ..
        } => {
            if let Some(args) = table_args {
                for a in args {
                    reject_raise_in_top_level_expr(a, db_set, ctes, visited_views)?;
                }
            }
            reject_raise_in_referenced_view(name, db_set, ctes, visited_views)
        }
        TableOrSubquery::Subquery { query, .. } => {
            reject_raise_in_top_level_select(query, db_set, ctes, visited_views)
        }
    }
}

fn reject_raise_in_referenced_view(
    qn: &QualifiedName,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    if qn.schema.is_none() && ctes.locate(&qn.name).is_some() {
        return Ok(());
    }
    if db_set.resolve_catalog_table(qn)?.is_none()
        && let Some((v_db_id, v)) = db_set.find_view_in_set(qn)
    {
        let key = (v_db_id, v.name.to_ascii_lowercase());
        if !visited_views.contains(&key) {
            visited_views.push(key);
            let res = reject_raise_in_top_level_select(
                &v.select,
                db_set,
                &CteStore::default(),
                visited_views,
            );
            visited_views.pop();
            return res;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_expr(
    expr: &Expr,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    visited_views: &mut Vec<(DatabaseId, String)>,
) -> Result<()> {
    reject_raise_outside_trigger_expr(expr)?;
    match expr {
        Expr::Unary { expr: inner, .. }
        | Expr::IsNull { expr: inner, .. }
        | Expr::Collate { expr: inner, .. }
        | Expr::Cast { expr: inner, .. } => {
            reject_raise_in_top_level_expr(inner, db_set, ctes, visited_views)
        }
        Expr::Binary { left, right, .. } => {
            reject_raise_in_top_level_expr(left, db_set, ctes, visited_views)?;
            reject_raise_in_top_level_expr(right, db_set, ctes, visited_views)
        }
        Expr::Between {
            expr: e, low, high, ..
        } => {
            reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            reject_raise_in_top_level_expr(low, db_set, ctes, visited_views)?;
            reject_raise_in_top_level_expr(high, db_set, ctes, visited_views)
        }
        Expr::InList { expr: e, list, .. } => {
            reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            for item in list {
                reject_raise_in_top_level_expr(item, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        Expr::InSubquery {
            expr: e, subquery, ..
        } => {
            reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            reject_raise_in_top_level_select(subquery, db_set, ctes, visited_views)
        }
        Expr::InTable { expr: e, table, .. } => {
            reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            reject_raise_in_referenced_view(table, db_set, ctes, visited_views)
        }
        Expr::Like {
            expr: e,
            pattern,
            escape,
            ..
        } => {
            reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            reject_raise_in_top_level_expr(pattern, db_set, ctes, visited_views)?;
            if let Some(esc) = escape {
                reject_raise_in_top_level_expr(esc, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        Expr::Case {
            operand,
            branches,
            else_expr,
        } => {
            if let Some(op) = operand {
                reject_raise_in_top_level_expr(op, db_set, ctes, visited_views)?;
            }
            for (c, v) in branches {
                reject_raise_in_top_level_expr(c, db_set, ctes, visited_views)?;
                reject_raise_in_top_level_expr(v, db_set, ctes, visited_views)?;
            }
            if let Some(el) = else_expr {
                reject_raise_in_top_level_expr(el, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        Expr::FunctionCall { args, filter, .. } => {
            if let FunctionArgs::Exprs(es) = args {
                for e in es {
                    reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
                }
            }
            if let Some(f) = filter {
                reject_raise_in_top_level_expr(f, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            reject_raise_in_top_level_select(subquery, db_set, ctes, visited_views)
        }
        Expr::RowValue(es) => {
            for e in es {
                reject_raise_in_top_level_expr(e, db_set, ctes, visited_views)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn reject_raise_in_table_constraints_outside_trigger(tbl: &TableSchema) -> Result<()> {
    for chk in &tbl.checks {
        reject_raise_outside_trigger_expr(&chk.expr)?;
    }
    for col in &tbl.columns {
        if let Some((ref ge, _)) = col.generated {
            reject_raise_outside_trigger_expr(ge)?;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_insert(ins: &InsertStmt, db_set: &DatabaseSet) -> Result<()> {
    let mut visited_views = Vec::new();
    let dml_ctes = CteStore::default().with_scope(&ins.ctes);
    for cte in &ins.ctes {
        reject_raise_in_top_level_select(&cte.query, db_set, &dml_ctes, &mut visited_views)?;
    }
    if let Some((_, tbl)) = db_set.find_user_table_in_set(&ins.table) {
        reject_raise_in_table_constraints_outside_trigger(tbl)?;
    }
    match &ins.source {
        InsertSource::DefaultValues => {}
        InsertSource::Values(rows) => {
            for row in rows {
                for e in row {
                    reject_raise_in_top_level_expr(e, db_set, &dml_ctes, &mut visited_views)?;
                }
            }
        }
        InsertSource::Select(sel) => {
            reject_raise_in_top_level_select(sel, db_set, &dml_ctes, &mut visited_views)?;
        }
    }
    for uc in &ins.upsert_clauses {
        if let Some(ref tgt) = uc.target {
            for ic in &tgt.columns {
                if let Some(ref ie) = ic.expr {
                    reject_raise_in_top_level_expr(ie, db_set, &dml_ctes, &mut visited_views)?;
                }
            }
            if let Some(ref tw) = tgt.where_clause {
                reject_raise_in_top_level_expr(tw, db_set, &dml_ctes, &mut visited_views)?;
            }
        }
        if let UpsertAction::DoUpdate {
            ref assignments,
            ref where_clause,
        } = uc.action
        {
            for asgn in assignments {
                reject_raise_in_top_level_expr(&asgn.expr, db_set, &dml_ctes, &mut visited_views)?;
            }
            if let Some(uw) = where_clause {
                reject_raise_in_top_level_expr(uw, db_set, &dml_ctes, &mut visited_views)?;
            }
        }
    }
    for rc in &ins.returning {
        if let ResultColumn::Expr { expr, .. } = rc {
            reject_raise_in_top_level_expr(expr, db_set, &dml_ctes, &mut visited_views)?;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_update(upd: &UpdateStmt, db_set: &DatabaseSet) -> Result<()> {
    let mut visited_views = Vec::new();
    let dml_ctes = CteStore::default().with_scope(&upd.ctes);
    for cte in &upd.ctes {
        reject_raise_in_top_level_select(&cte.query, db_set, &dml_ctes, &mut visited_views)?;
    }
    if let Some((_, tbl)) = db_set.find_user_table_in_set(&upd.table) {
        reject_raise_in_table_constraints_outside_trigger(tbl)?;
    }
    if let Some(ref fc) = upd.from {
        if from_contains_raise(fc) {
            return Err(SqliteError::new(
                ErrorCode::Error,
                "RAISE() may only be used within a trigger-program",
            ));
        }
        reject_raise_in_top_level_from(fc, db_set, &dml_ctes, &mut visited_views)?;
    }
    for asgn in &upd.assignments {
        reject_raise_in_top_level_expr(&asgn.expr, db_set, &dml_ctes, &mut visited_views)?;
    }
    if let Some(ref w) = upd.selection {
        reject_raise_in_top_level_expr(w, db_set, &dml_ctes, &mut visited_views)?;
    }
    for rc in &upd.returning {
        if let ResultColumn::Expr { expr, .. } = rc {
            reject_raise_in_top_level_expr(expr, db_set, &dml_ctes, &mut visited_views)?;
        }
    }
    Ok(())
}

fn reject_raise_in_top_level_delete(del: &DeleteStmt, db_set: &DatabaseSet) -> Result<()> {
    let mut visited_views = Vec::new();
    let dml_ctes = CteStore::default().with_scope(&del.ctes);
    for cte in &del.ctes {
        reject_raise_in_top_level_select(&cte.query, db_set, &dml_ctes, &mut visited_views)?;
    }
    if let Some(ref w) = del.selection {
        reject_raise_in_top_level_expr(w, db_set, &dml_ctes, &mut visited_views)?;
    }
    for rc in &del.returning {
        if let ResultColumn::Expr { expr, .. } = rc {
            reject_raise_in_top_level_expr(expr, db_set, &dml_ctes, &mut visited_views)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_tuple_expr_static(
    mut expr: &Expr,
    scopes: &[EvalRow],
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    allow_agg: bool,
    inside_agg: bool,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<usize> {
    while let Expr::Collate { expr: inner, .. } = expr {
        expr = inner;
    }
    match expr {
        Expr::RowValue(elems) => {
            for el in elems {
                validate_expr_static(
                    el,
                    scopes,
                    db_set,
                    ctes,
                    functions,
                    allow_agg,
                    inside_agg,
                    visited_ctes,
                )?;
            }
            Ok(elems.len())
        }
        Expr::Subquery(sub) => {
            let sub_cols =
                validate_select_stmt_static(sub, scopes, db_set, ctes, functions, visited_ctes)?;
            Ok(sub_cols.len().max(1))
        }
        other => {
            validate_expr_static(
                other,
                scopes,
                db_set,
                ctes,
                functions,
                allow_agg,
                inside_agg,
                visited_ctes,
            )?;
            Ok(1)
        }
    }
}

#[allow(clippy::type_complexity)]
fn resolve_view_columns_static(
    db_id: DatabaseId,
    view: &ViewSchema,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    functions: &FunctionRegistry,
    visited_ctes: &mut Vec<(usize, usize)>,
) -> Result<(Vec<String>, Vec<(Option<TypeAffinity>, Collation)>, String)> {
    ctes.record_catalog_ref(db_id, &view.name)?;
    if !db_set.enable_view() && db_id != DatabaseId::TEMP {
        return Err(SqliteError::new(
            ErrorCode::Error,
            format!("access to view \"{}\" prohibited", view.name),
        ));
    }
    let schema_lbl = db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());
    let view_ctes = ctes.enter_view(db_id, &view.name, db_set.expr_depth_limit())?;
    let cache_key = (db_id, view.name.to_ascii_lowercase());
    if let Some(cached) = ctes.static_view_cache.borrow().get(&cache_key).cloned() {
        let would_overflow = {
            let map = ctes.tab_refs.borrow();
            cached.sub_refs.iter().any(|(sub_db, sub_low, _, delta)| {
                let cur = map.get(&(*sub_db, sub_low.clone())).copied().unwrap_or(1);
                cur.saturating_add(*delta) > 0xffff
            })
        };
        if !would_overflow {
            // Validate nested depth along this cached branch (`view-32` when `expr_depth_limit` is small)
            if db_set.expr_depth_limit() <= 32 {
                let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view)?;
                let mut inner_visited = Vec::new();
                let _ = validate_select_stmt_static(
                    &fixed_sel,
                    &[],
                    db_set,
                    &view_ctes,
                    functions,
                    &mut inner_visited,
                )?;
                return Ok((cached.cols, cached.meta, schema_lbl));
            }
            let mut map = ctes.tab_refs.borrow_mut();
            for (sub_db, sub_low, _, delta) in &cached.sub_refs {
                let entry = map.entry((*sub_db, sub_low.clone())).or_insert(1);
                *entry = entry.saturating_add(*delta);
            }
            return Ok((cached.cols, cached.meta, schema_lbl));
        }
        // An entry inside this view crosses 65535 (`0xffff`): descend in exact DFS order so
        // the exact first overflowing object name is reported (`view3-1.1`).
        let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view)?;
        let mut inner_visited = Vec::new();
        let _ = validate_select_stmt_static(
            &fixed_sel,
            &[],
            db_set,
            &view_ctes,
            functions,
            &mut inner_visited,
        )?;
        return Ok((cached.cols, cached.meta, schema_lbl));
    }

    let snapshot_before: HashMap<(DatabaseId, String), usize> = ctes.tab_refs.borrow().clone();
    let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view)?;
    let mut inner_visited = Vec::new();
    let raw_cols = validate_select_stmt_static(
        &fixed_sel,
        &[],
        db_set,
        &view_ctes,
        functions,
        &mut inner_visited,
    )?;
    let _ = visited_ctes;
    let meta = infer_select_stmt_meta(&fixed_sel, &[], db_set, &view_ctes);
    let final_cols = if let Some(ref explicit_cols) = view.columns {
        if explicit_cols.len() != raw_cols.len() {
            return Err(SqliteError::new(
                ErrorCode::Error,
                format!(
                    "expected {} columns for '{}' but got {}",
                    explicit_cols.len(),
                    view.name,
                    raw_cols.len()
                ),
            ));
        }
        disambiguate_view_column_names(explicit_cols)
    } else {
        disambiguate_view_column_names(&raw_cols)
    };

    let mut sub_refs = Vec::new();
    {
        let map_after = ctes.tab_refs.borrow();
        for ((sub_db, sub_low), &after_cnt) in map_after.iter() {
            let before_cnt = snapshot_before
                .get(&(*sub_db, sub_low.clone()))
                .copied()
                .unwrap_or(1);
            if after_cnt > before_cnt {
                sub_refs.push((
                    *sub_db,
                    sub_low.clone(),
                    sub_low.clone(),
                    after_cnt - before_cnt,
                ));
            }
        }
    }
    ctes.static_view_cache.borrow_mut().insert(
        cache_key,
        CachedStaticView {
            cols: final_cols.clone(),
            meta: meta.clone(),
            sub_refs,
        },
    );
    Ok((final_cols, meta, schema_lbl))
}

fn infer_view_pragma_columns(
    db_id: DatabaseId,
    view: &ViewSchema,
    db_set: &DatabaseSet,
    functions: &FunctionRegistry,
) -> Result<Vec<(String, String)>> {
    let schema_lbl = db_set
        .slot(db_id)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "main".to_owned());
    let view_ctes = CteStore::default().enter_view(db_id, &view.name, db_set.expr_depth_limit())?;
    let fixed_sel = prepare_fixed_view_select(db_id, &schema_lbl, view)?;
    let mut inner_visited = Vec::new();
    let raw_cols = validate_select_stmt_static(
        &fixed_sel,
        &[],
        db_set,
        &view_ctes,
        functions,
        &mut inner_visited,
    )?;
    if let Some(ref explicit_cols) = view.columns
        && explicit_cols.len() != raw_cols.len()
    {
        let names = disambiguate_view_column_names(explicit_cols);
        return Ok(names.into_iter().map(|n| (n, String::new())).collect());
    }
    let names = match &view.columns {
        Some(explicit_cols) => disambiguate_view_column_names(explicit_cols),
        None => disambiguate_view_column_names(&raw_cols),
    };
    let meta = infer_select_stmt_meta(&fixed_sel, &[], db_set, &view_ctes);
    let sub_ctes = view_ctes.with_scope(&fixed_sel.ctes);
    let mut depth = 0usize;
    let head_decl_types =
        infer_core_origin_declared_types(&fixed_sel.body.head, db_set, &sub_ctes, &mut depth);
    let mut out = Vec::with_capacity(names.len());
    for (i, cname) in names.into_iter().enumerate() {
        let aff = meta.get(i).and_then(|(a, _)| *a);
        let decl_opt = head_decl_types.get(i).and_then(|d| d.as_deref());
        let type_str = if let Some(dt) = decl_opt
            && !dt.is_empty()
            && Some(TypeAffinity::from_declared_type(dt)) == aff
        {
            dt.to_owned()
        } else {
            match aff {
                Some(TypeAffinity::Integer) => "INT".to_owned(),
                Some(TypeAffinity::Real) => "REAL".to_owned(),
                Some(TypeAffinity::Text) => "TEXT".to_owned(),
                Some(TypeAffinity::Blob) => "BLOB".to_owned(),
                Some(TypeAffinity::Numeric) => "NUM".to_owned(),
                None => String::new(),
            }
        };
        out.push((cname, type_str));
    }
    Ok(out)
}

enum OriginSource {
    Table {
        schema_name: String,
        alias: String,
        tbl: TableSchema,
    },
    Derived {
        schema_name: Option<String>,
        alias: Option<String>,
        col_names: Vec<String>,
        decl_types: Vec<Option<String>>,
    },
}

impl OriginSource {
    fn matches_qualifier(&self, schema_qual: Option<&str>, table_qual: Option<&str>) -> bool {
        match self {
            Self::Table {
                schema_name, alias, ..
            } => {
                if let Some(sq) = schema_qual
                    && !schema_name.eq_ignore_ascii_case(sq)
                {
                    return false;
                }
                if let Some(tq) = table_qual
                    && !alias.eq_ignore_ascii_case(tq)
                {
                    return false;
                }
                true
            }
            Self::Derived {
                schema_name, alias, ..
            } => {
                if let Some(sq) = schema_qual {
                    let Some(sn) = schema_name else {
                        return false;
                    };
                    if !sn.eq_ignore_ascii_case(sq) {
                        return false;
                    }
                }
                if let Some(tq) = table_qual {
                    let Some(al) = alias else {
                        return false;
                    };
                    if !al.eq_ignore_ascii_case(tq) {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn lookup_declared_type(&self, col_name: &str) -> Option<(bool, Option<String>)> {
        match self {
            Self::Table { tbl, .. } => {
                if let Some((_, c)) = tbl.find_column(col_name) {
                    let dt = if c.type_name.is_empty() {
                        None
                    } else {
                        Some(c.type_name.clone())
                    };
                    return Some((true, dt));
                }
                if tbl.is_implicit_rowid_name(col_name) {
                    return Some((true, Some("INTEGER".to_owned())));
                }
                None
            }
            Self::Derived {
                col_names,
                decl_types,
                ..
            } => {
                let idx = col_names
                    .iter()
                    .position(|cn| cn.eq_ignore_ascii_case(col_name))?;
                Some((true, decl_types.get(idx).cloned().flatten()))
            }
        }
    }
}

fn build_origin_source_for_tos(
    tos: &TableOrSubquery,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    depth: &mut usize,
) -> Option<OriginSource> {
    if *depth > 32 {
        return None;
    }
    match tos {
        TableOrSubquery::Table { name, alias, .. } => {
            let eff = alias.clone().unwrap_or_else(|| name.name.clone());
            if name.schema.is_none()
                && let Some((s_idx, c_idx)) = ctes.locate(&name.name)
            {
                let cte = &ctes.scopes[s_idx][c_idx];
                let sub_ctes = ctes.slice_to_scope(s_idx).with_scope(&cte.query.ctes);
                *depth += 1;
                let decl_types = infer_core_origin_declared_types(
                    &cte.query.body.head,
                    db_set,
                    &sub_ctes,
                    depth,
                );
                let col_names = match &cte.columns {
                    Some(ec) => disambiguate_view_column_names(ec),
                    None => disambiguate_view_column_names(&static_select_col_names(
                        &cte.query.body,
                        &[],
                        db_set,
                        &sub_ctes,
                        &mut Vec::new(),
                    )),
                };
                *depth -= 1;
                return Some(OriginSource::Derived {
                    schema_name: None,
                    alias: Some(eff),
                    col_names,
                    decl_types,
                });
            }
            if db_set.resolve_catalog_table(name).ok().flatten().is_none()
                && let Some((v_db_id, view_ref)) = db_set.find_view_in_set(name)
            {
                let view_ctes = ctes
                    .enter_view(v_db_id, &view_ref.name, db_set.expr_depth_limit())
                    .ok()?;
                let schema_lbl = db_set
                    .slot(v_db_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "main".to_owned());
                let fixed_sel = prepare_fixed_view_select(v_db_id, &schema_lbl, view_ref).ok()?;
                let sub_ctes = view_ctes.with_scope(&fixed_sel.ctes);
                *depth += 1;
                let decl_types = infer_core_origin_declared_types(
                    &fixed_sel.body.head,
                    db_set,
                    &sub_ctes,
                    depth,
                );
                let raw_names = static_select_col_names(
                    &fixed_sel.body,
                    &[],
                    db_set,
                    &sub_ctes,
                    &mut Vec::new(),
                );
                *depth -= 1;
                let col_names = match &view_ref.columns {
                    Some(ec) if ec.len() == raw_names.len() => disambiguate_view_column_names(ec),
                    _ => disambiguate_view_column_names(&raw_names),
                };
                return Some(OriginSource::Derived {
                    schema_name: Some(schema_lbl),
                    alias: Some(eff),
                    col_names,
                    decl_types,
                });
            }
            let (_, tbl, schema_lbl) = resolve_table_for_query_in_set(db_set, name).ok()?;
            Some(OriginSource::Table {
                schema_name: schema_lbl,
                alias: eff,
                tbl,
            })
        }
        TableOrSubquery::Subquery { query, alias } => {
            let sub_ctes = ctes.with_scope(&query.ctes);
            *depth += 1;
            let decl_types =
                infer_core_origin_declared_types(&query.body.head, db_set, &sub_ctes, depth);
            let raw_names =
                static_select_col_names(&query.body, &[], db_set, &sub_ctes, &mut Vec::new());
            let col_names = disambiguate_view_column_names(&raw_names);
            *depth -= 1;
            Some(OriginSource::Derived {
                schema_name: None,
                alias: alias.clone(),
                col_names,
                decl_types,
            })
        }
    }
}

fn infer_core_origin_declared_types(
    core: &SelectCore,
    db_set: &DatabaseSet,
    ctes: &CteStore,
    depth: &mut usize,
) -> Vec<Option<String>> {
    if *depth > 32 {
        return Vec::new();
    }
    let SelectCore::Select {
        projection, from, ..
    } = core
    else {
        return Vec::new();
    };
    let sample_cols = from
        .as_ref()
        .and_then(|fc| static_schema_cols_for_from(fc, &[], db_set, ctes, &mut Vec::new()))
        .unwrap_or_default();
    let Ok((_, expanded)) = expand_projection(projection, &sample_cols) else {
        return Vec::new();
    };
    let mut sources: Vec<OriginSource> = Vec::new();
    if let Some(fc) = from {
        if let Some(src) = build_origin_source_for_tos(&fc.head, db_set, ctes, depth) {
            sources.push(src);
        }
        for j in &fc.joins {
            if let Some(src) = build_origin_source_for_tos(&j.right, db_set, ctes, depth) {
                sources.push(src);
            }
        }
    }
    expanded
        .into_iter()
        .map(|(pe, _, _)| match pe {
            Expr::Column(cref) => {
                if cref.schema.as_deref() == Some(STAR_SLOT_SCHEMA) {
                    let sc = cref
                        .column
                        .parse::<usize>()
                        .ok()
                        .and_then(|idx| sample_cols.get(idx))?;
                    for src in &sources {
                        if !src
                            .matches_qualifier(sc.schema_name.as_deref(), sc.table_alias.as_deref())
                        {
                            continue;
                        }
                        if let Some((_, dt)) = src.lookup_declared_type(&sc.name) {
                            return dt;
                        }
                    }
                    return None;
                }
                for src in &sources {
                    if !src.matches_qualifier(cref.schema.as_deref(), cref.table.as_deref()) {
                        continue;
                    }
                    if let Some((_, dt)) = src.lookup_declared_type(&cref.column) {
                        return dt;
                    }
                }
                None
            }
            Expr::Subquery(sub) => {
                let sub_ctes = ctes.with_scope(&sub.ctes);
                *depth += 1;
                let sub_types =
                    infer_core_origin_declared_types(&sub.body.head, db_set, &sub_ctes, depth);
                *depth -= 1;
                sub_types.into_iter().next().flatten()
            }
            _ => None,
        })
        .collect()
}
