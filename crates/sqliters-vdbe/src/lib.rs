//! `sqliters-vdbe`: SQLite Virtual Database Engine (VDBE) bytecode representation,
//! disassembler (`EXPLAIN`), and register-based execution machine ported from `sqlite/src/vdbe.c`.
#![forbid(unsafe_code)]

pub mod functions;
pub mod storage_api;

use std::cmp::Ordering;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub use sqliters_btree::PointProjCol;
use sqliters_btree::{Btree, CursorId};
use sqliters_types::{
    AggregateStepId, Collation, DatabaseId, ErrorCode, ExecutionServices, KeySortOrder, Pgno,
    Result, RuntimeValue, SqliteError, TextEncoding, TypeAffinity, Value,
    decode_record_with_encoding, encode_runtime_record_into, fallible_sort_by,
};

pub use functions::{
    AccumulatorTable, AggregateAccumulator, AggregateKind, CompiledLikePattern, FastSumAccum,
    TestFunctionsScope, VmRuntimeContext, builtin_scalar_flags, builtin_window_arity,
    eval_scalar_function, eval_scalar_function_rt, eval_scalar_function_rt_with_strict,
    format_i64_into_stack, format_utc_date, format_utc_time, format_utc_timestamp,
    is_builtin_window_only_function, parse_escape_char, sqlite_glob_match, sqlite_like_match,
    system_utc_millis, utc_ymdhms_from_unix_ms, validate_function_call,
};
pub use storage_api::BtreeAccess;

/// Standalone [`ExecutionServices`] implementation for driving [`VdbeVm`] directly in unit and
/// integration tests without requiring a full `sqliters-core::Connection`.
#[derive(Debug)]
pub struct DefaultVmServices {
    pub runtime_ctx: VmRuntimeContext,
    pub encoding: TextEncoding,
    pub interrupt_flag: Option<Arc<AtomicBool>>,
    pub progress_ticks: u64,
    pub strict_subtype: bool,
    pub accumulators: AccumulatorTable,
}

impl Default for DefaultVmServices {
    fn default() -> Self {
        Self::new(VmRuntimeContext::default())
    }
}

impl DefaultVmServices {
    pub fn new(runtime_ctx: VmRuntimeContext) -> Self {
        Self {
            runtime_ctx,
            encoding: TextEncoding::Utf8,
            interrupt_flag: None,
            progress_ticks: 0,
            strict_subtype: true,
            accumulators: AccumulatorTable::new(),
        }
    }

    pub fn with_interrupt_flag(mut self, flag: Option<Arc<AtomicBool>>) -> Self {
        self.interrupt_flag = flag;
        self
    }
}

impl ExecutionServices for DefaultVmServices {
    #[inline]
    fn check_interrupted(&mut self) -> Result<()> {
        if self
            .interrupt_flag
            .as_ref()
            .is_some_and(|f| f.load(AtomicOrdering::Relaxed))
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    #[inline]
    fn tick_progress(&mut self) -> Result<()> {
        self.progress_ticks = self.progress_ticks.saturating_add(1);
        Ok(())
    }

    fn call_scalar(&mut self, name: &str, args: &[RuntimeValue]) -> Result<RuntimeValue> {
        eval_scalar_function_rt_with_strict(name, args, &self.runtime_ctx, self.strict_subtype)
    }

    fn compare_collated(
        &mut self,
        collation_name: &str,
        left: &str,
        right: &str,
    ) -> Result<Ordering> {
        let coll = Collation::from_name(collation_name).ok_or_else(|| {
            SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {collation_name}"),
            )
        })?;
        Ok(Value::Text(left.to_owned()).cmp_sqlite_with_encoding(
            &Value::Text(right.to_owned()),
            coll,
            self.encoding,
        ))
    }

    #[inline]
    fn strict_subtype(&self) -> bool {
        self.strict_subtype
    }

    fn agg_open(&mut self, name: &str, arity: usize) -> Result<AggregateStepId> {
        self.accumulators
            .open(name, arity, false, Collation::Binary, self.encoding)
    }

    fn agg_open_collated(
        &mut self,
        name: &str,
        arity: usize,
        distinct: bool,
        collation_name: &str,
    ) -> Result<AggregateStepId> {
        let coll = Collation::from_name(collation_name).ok_or_else(|| {
            SqliteError::new(
                ErrorCode::Error,
                format!("no such collation sequence: {collation_name}"),
            )
        })?;
        self.accumulators
            .open(name, arity, distinct, coll, self.encoding)
    }

    fn agg_supports_inverse(&self, id: AggregateStepId) -> bool {
        self.accumulators.supports_inverse(id)
    }

    fn agg_step(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        self.accumulators.step(id, args)
    }

    fn agg_value(&mut self, id: AggregateStepId) -> Result<RuntimeValue> {
        self.accumulators.value(id, self.strict_subtype)
    }

    fn agg_inverse(&mut self, id: AggregateStepId, args: &[RuntimeValue]) -> Result<()> {
        self.accumulators.inverse(id, args)
    }

    fn agg_finalize(&mut self, id: AggregateStepId) -> Result<RuntimeValue> {
        self.accumulators.finalize(id, self.strict_subtype)
    }
}

/// Fourth operand (`P4`) payload for a VDBE instruction (`p4union` in `sqlite/src/vdbe.h`).
#[derive(Debug, Clone, Default)]
pub enum P4 {
    #[default]
    None,
    Int64(i64),
    Real(f64),
    Str(String),
    Blob(Vec<u8>),
    Collation(Collation),
    Affinity(Vec<TypeAffinity>),
    KeyInfo(Arc<[KeySortOrder]>),
    Func {
        name: String,
        argc: usize,
    },
    IntArray(Vec<usize>),
}

impl P4 {
    #[inline]
    pub fn key_info(orders: impl Into<Arc<[KeySortOrder]>>) -> Self {
        Self::KeyInfo(orders.into())
    }
    pub fn display_explain(&self) -> String {
        match self {
            Self::None => String::new(),
            Self::Int64(i) => i.to_string(),
            Self::Real(r) => r.to_string(),
            Self::Str(s) => s.clone(),
            Self::Blob(b) => format!(
                "x'{}'",
                b.iter().map(|x| format!("{x:02X}")).collect::<String>()
            ),
            Self::Collation(c) => format!("{c:?}"),
            Self::Affinity(affs) => format!("{affs:?}"),
            Self::KeyInfo(k) => format!("k({})", k.len()),
            Self::Func { name, argc } => format!("{name}({argc})"),
            Self::IntArray(v) => format!("{v:?}"),
        }
    }
}

/// VDBE bytecode opcodes modeled directly on `sqlite/src/vdbe.c` (`OP_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Init,
    Goto,
    Halt,
    Transaction,
    OpenRead,
    OpenWrite,
    OpenReadIndex,
    OpenWriteIndex,
    OpenEphemeral,
    Close,
    Rewind,
    Next,
    SeekRowid,
    SeekIndexEqual,
    IndexMatchNext,
    Rowid,
    Column,
    MakeRecord,
    NewRowid,
    Insert,
    Delete,
    IdxInsert,
    IdxDelete,
    ResultRow,
    Integer,
    Int64,
    Real,
    String8,
    Blob,
    Null,
    Variable,
    Copy,
    Move,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Concat,
    BitAnd,
    BitOr,
    BitNot,
    ShiftLeft,
    ShiftRight,
    Negate,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Is,
    IsNot,
    And,
    Or,
    Not,
    IsNull,
    NotNull,
    JumpIfTrue,
    JumpIfFalse,
    JumpIfNull,
    MustBeInt,
    Affinity,
    Cast,
    Function,
    SorterOpen,
    SorterInsert,
    SorterSort,
    SorterColumn,
    SorterNext,
    Noop,
    /// `OP_MakeRecord` with an affinity string (`P4::Affinity`, one entry per register): like
    /// `MakeRecord`, but an integral real in a register whose affinity is REAL is written as an
    /// integer (C's `MEM_IntReal`). The registers themselves keep their values.
    MakeRecordAff,
    /// `OP_RealAffinity`: if register `p1` holds an integer, make it a real. Emitted after
    /// reading a column with REAL affinity, whose integral values are stored as integers.
    RealAffinity,
}

impl Opcode {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Init => "Init",
            Self::Goto => "Goto",
            Self::Halt => "Halt",
            Self::Transaction => "Transaction",
            Self::OpenRead => "OpenRead",
            Self::OpenWrite => "OpenWrite",
            Self::OpenReadIndex => "OpenReadIdx",
            Self::OpenWriteIndex => "OpenWriteIdx",
            Self::OpenEphemeral => "OpenEphemeral",
            Self::Close => "Close",
            Self::Rewind => "Rewind",
            Self::Next => "Next",
            Self::SeekRowid => "SeekRowid",
            Self::SeekIndexEqual => "SeekGE",
            Self::IndexMatchNext => "IdxNext",
            Self::Rowid => "Rowid",
            Self::Column => "Column",
            Self::MakeRecord => "MakeRecord",
            Self::NewRowid => "NewRowid",
            Self::Insert => "Insert",
            Self::Delete => "Delete",
            Self::IdxInsert => "IdxInsert",
            Self::IdxDelete => "IdxDelete",
            Self::ResultRow => "ResultRow",
            Self::Integer => "Integer",
            Self::Int64 => "Int64",
            Self::Real => "Real",
            Self::String8 => "String8",
            Self::Blob => "Blob",
            Self::Null => "Null",
            Self::Variable => "Variable",
            Self::Copy => "Copy",
            Self::Move => "Move",
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Multiply => "Multiply",
            Self::Divide => "Divide",
            Self::Remainder => "Remainder",
            Self::Concat => "Concat",
            Self::BitAnd => "BitAnd",
            Self::BitOr => "BitOr",
            Self::BitNot => "BitNot",
            Self::ShiftLeft => "ShiftLeft",
            Self::ShiftRight => "ShiftRight",
            Self::Negate => "Negate",
            Self::Eq => "Eq",
            Self::Ne => "Ne",
            Self::Lt => "Lt",
            Self::Le => "Le",
            Self::Gt => "Gt",
            Self::Ge => "Ge",
            Self::Is => "Is",
            Self::IsNot => "IsNot",
            Self::And => "And",
            Self::Or => "Or",
            Self::Not => "Not",
            Self::IsNull => "IsNull",
            Self::NotNull => "NotNull",
            Self::JumpIfTrue => "If",
            Self::JumpIfFalse => "IfNot",
            Self::JumpIfNull => "IfNull",
            Self::MustBeInt => "MustBeInt",
            Self::Affinity => "Affinity",
            Self::RealAffinity => "RealAffinity",
            Self::MakeRecordAff => "MakeRecord",
            Self::Cast => "Cast",
            Self::Function => "Function",
            Self::SorterOpen => "SorterOpen",
            Self::SorterInsert => "SorterInsert",
            Self::SorterSort => "SorterSort",
            Self::SorterColumn => "SorterColumn",
            Self::SorterNext => "SorterNext",
            Self::Noop => "Noop",
        }
    }
}

/// Single VDBE instruction (`VdbeOp` in `sqlite/src/vdbe.h`).
#[derive(Debug, Clone)]
pub struct Instruction {
    pub opcode: Opcode,
    pub p1: i32,
    pub p2: i32,
    pub p3: i32,
    pub p4: P4,
    pub p5: u16,
    pub db: DatabaseId,
    pub comment: Option<String>,
}

impl Instruction {
    pub fn new(opcode: Opcode, p1: i32, p2: i32, p3: i32) -> Self {
        Self {
            opcode,
            p1,
            p2,
            p3,
            p4: P4::None,
            p5: 0,
            db: DatabaseId::MAIN,
            comment: None,
        }
    }

    pub fn with_p4(mut self, p4: P4) -> Self {
        self.p4 = p4;
        self
    }

    pub fn with_p5(mut self, p5: u16) -> Self {
        self.p5 = p5;
        self
    }

    pub fn with_db(mut self, db: DatabaseId) -> Self {
        self.db = db;
        self
    }

    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }
}

/// Key source for a single-shot `rowid` point `SELECT` fast path.
#[derive(Debug, Clone, Copy)]
pub enum RowidKeySource {
    /// 1-based bind parameter (`?1`, `:id`, etc.).
    Param1Based(u16),
    /// Constant integer `rowid` literal.
    ConstRowid(i64),
}

/// Precompiled direct B+Tree point-lookup plan for `SELECT <cols> FROM <tbl> WHERE <rowid/ipk> = ?`.
#[derive(Debug, Clone)]
pub struct RowidPointSelectPlan {
    pub db: DatabaseId,
    pub root_pgno: Pgno,
    pub key: RowidKeySource,
    pub cols: Vec<PointProjCol>,
}

/// Value source for a single-row direct `INSERT` plan.
#[derive(Debug, Clone)]
pub enum InsertValSource {
    /// SQL `NULL` (or physical `INTEGER PRIMARY KEY` record slot).
    Null,
    /// Constant literal value.
    Const(Value),
    /// 1-based bind parameter (`?1`, `:x`, etc.).
    Param1Based(u32),
    /// Built-in `jsonb(?p)` call on a 1-based bind parameter.
    JsonbParam1Based(u32),
}

/// Column specification in a [`SingleRowInsertPlan`].
#[derive(Debug, Clone)]
pub struct SingleRowInsertCol {
    pub src: InsertValSource,
    pub affinity: TypeAffinity,
    pub notnull_msg: Option<String>,
}

/// Non-unique secondary index specification in a [`SingleRowInsertPlan`].
#[derive(Debug, Clone)]
pub struct SingleRowInsertIndex {
    pub root_pgno: Pgno,
    pub orders: Vec<KeySortOrder>,
    /// Logical column index (`Some(ci)`) or `None` for the `INTEGER PRIMARY KEY` / `rowid`.
    pub col_slots: Vec<Option<usize>>,
}

/// Precompiled direct single-row table `INSERT` plan (when no triggers, FKs, or unique secondary
/// indexes apply), bypassing VDBE register copying and per-opcode dispatch.
#[derive(Debug, Clone)]
pub struct SingleRowInsertPlan {
    pub db: DatabaseId,
    pub root_pgno: Pgno,
    pub rowid_src: Option<InsertValSource>,
    pub cols: Vec<SingleRowInsertCol>,
    pub indexes: Vec<SingleRowInsertIndex>,
    pub has_builtin_jsonb: bool,
    /// Integral reals in REAL-affinity columns are stored as integers (C's `OP_MakeRecord`
    /// with an affinity string); never set together with `indexes`.
    pub real_as_int: bool,
    pub dup_ext_code: i32,
    pub dup_msg: String,
}

/// Secondary index maintained by a [`RowidPointUpdatePlan`] / [`RowidPointDeletePlan`].
#[derive(Debug, Clone)]
pub struct PointDmlIndex {
    pub root_pgno: Pgno,
    /// Full key sort orders of the index record (indexed columns, then the trailing rowid).
    pub orders: Arc<[KeySortOrder]>,
    /// Logical column index (`Some(ci)`) or `None` for the `INTEGER PRIMARY KEY` / `rowid`.
    pub col_slots: Vec<Option<usize>>,
}

/// One `SET col = <bind parameter | literal>` assignment of a [`RowidPointUpdatePlan`].
#[derive(Debug, Clone)]
pub struct PointUpdateSet {
    pub col: usize,
    pub src: InsertValSource,
    pub affinity: TypeAffinity,
    pub not_null: bool,
}

/// Direct B-tree plan for `UPDATE <tbl> SET c = <param|literal>, ... WHERE <rowid/ipk> = <param|literal>`
/// attached to the VDBE program compiled for the same statement.
///
/// It performs exactly the B-tree calls of that program on its straight-line paths (row found
/// and rewritten, or row absent) and declines every other situation, which then runs the
/// program itself. `ticks_hit` / `ticks_miss` are the opcode counts the program executes on
/// those two paths.
#[derive(Debug, Clone)]
pub struct RowidPointUpdatePlan {
    pub root_pgno: Pgno,
    pub n_cols: usize,
    pub rowid_alias_col: Option<usize>,
    pub key: InsertValSource,
    pub sets: Vec<PointUpdateSet>,
    /// Indexes with at least one assigned column (never UNIQUE), in program order.
    pub indexes: Vec<PointDmlIndex>,
    /// Bit `ci` set: the program reads stored column `ci` (to rebuild the row or an index key).
    pub read_mask: u32,
    /// Columns with REAL affinity (not the rowid alias). C re-encodes every one of them when it
    /// rewrites the row: as integers where the value is integral (`real_as_int`, `OP_MakeRecord`
    /// with the affinity string) or as floats (`OP_Affinity` ran because an index is rewritten).
    pub real_cols: Vec<usize>,
    pub real_as_int: bool,
    pub ticks_hit: u64,
    pub ticks_miss: u64,
}

/// Direct B-tree plan for `DELETE FROM <tbl> WHERE <rowid/ipk> = <param|literal>`; see
/// [`RowidPointUpdatePlan`] for the contract.
#[derive(Debug, Clone)]
pub struct RowidPointDeletePlan {
    pub root_pgno: Pgno,
    pub n_cols: usize,
    pub key: InsertValSource,
    pub indexes: Vec<PointDmlIndex>,
    /// Bit `ci` set: the program reads stored column `ci` (to build an index key).
    pub read_mask: u32,
    pub ticks_hit: u64,
    pub ticks_miss: u64,
}

/// Compiled VDBE bytecode program (`Vdbe` in `vdbeInt.h`).
#[derive(Debug, Clone)]
pub struct VdbeProgram {
    pub ops: Vec<Instruction>,
    pub num_registers: usize,
    pub num_cursors: usize,
    pub column_names: Vec<String>,
    pub read_only: bool,
    referenced_db_mask: u16,
    primary_write_db: Option<DatabaseId>,
    has_function_call: bool,
    needs_stmt_timestamp: bool,
    needs_stmt_subsavepoint: bool,
    rowid_point_select: Option<RowidPointSelectPlan>,
    single_row_insert: Option<SingleRowInsertPlan>,
    rowid_point_update: Option<RowidPointUpdatePlan>,
    rowid_point_delete: Option<RowidPointDeletePlan>,
}

impl VdbeProgram {
    pub fn new() -> Self {
        Self {
            ops: Vec::new(),
            num_registers: 1,
            num_cursors: 0,
            column_names: Vec::new(),
            read_only: true,
            referenced_db_mask: 0,
            primary_write_db: None,
            has_function_call: false,
            needs_stmt_timestamp: false,
            needs_stmt_subsavepoint: true,
            rowid_point_select: None,
            single_row_insert: None,
            rowid_point_update: None,
            rowid_point_delete: None,
        }
    }

    #[inline(always)]
    pub fn rowid_point_update(&self) -> Option<&RowidPointUpdatePlan> {
        self.rowid_point_update.as_ref()
    }

    #[inline]
    pub fn set_rowid_point_update(&mut self, plan: Option<RowidPointUpdatePlan>) {
        self.rowid_point_update = plan;
    }

    #[inline(always)]
    pub fn rowid_point_delete(&self) -> Option<&RowidPointDeletePlan> {
        self.rowid_point_delete.as_ref()
    }

    #[inline]
    pub fn set_rowid_point_delete(&mut self, plan: Option<RowidPointDeletePlan>) {
        self.rowid_point_delete = plan;
    }

    #[inline(always)]
    pub fn single_row_insert(&self) -> Option<&SingleRowInsertPlan> {
        self.single_row_insert.as_ref()
    }

    #[inline]
    pub fn set_single_row_insert(&mut self, plan: Option<SingleRowInsertPlan>) {
        self.single_row_insert = plan;
    }

    #[inline(always)]
    pub fn needs_stmt_subsavepoint(&self) -> bool {
        self.needs_stmt_subsavepoint
    }

    #[inline]
    pub fn set_needs_stmt_subsavepoint(&mut self, needed: bool) {
        self.needs_stmt_subsavepoint = needed;
    }

    #[inline(always)]
    pub fn rowid_point_select(&self) -> Option<&RowidPointSelectPlan> {
        self.rowid_point_select.as_ref()
    }

    #[inline]
    pub fn set_rowid_point_select(&mut self, plan: Option<RowidPointSelectPlan>) {
        self.rowid_point_select = plan;
    }

    pub fn alloc_register(&mut self) -> i32 {
        let r = self.num_registers as i32;
        self.num_registers += 1;
        r
    }

    pub fn alloc_registers(&mut self, count: usize) -> i32 {
        let base = self.num_registers as i32;
        self.num_registers += count;
        base
    }

    pub fn alloc_cursor(&mut self) -> i32 {
        let c = self.num_cursors as i32;
        self.num_cursors += 1;
        c
    }

    pub fn emit(&mut self, op: Instruction) -> usize {
        match op.opcode {
            Opcode::Transaction => {
                let db = if op.db != DatabaseId::MAIN {
                    op.db
                } else {
                    DatabaseId(op.p1.max(0) as usize)
                };
                self.referenced_db_mask |= 1u16.checked_shl(db.index() as u32).unwrap_or(0);
                if op.p2 != 0 && self.primary_write_db.is_none() {
                    self.primary_write_db = Some(db);
                }
            }
            Opcode::OpenRead | Opcode::OpenReadIndex => {
                self.referenced_db_mask |= 1u16.checked_shl(op.db.index() as u32).unwrap_or(0);
            }
            Opcode::OpenWrite | Opcode::OpenWriteIndex => {
                self.referenced_db_mask |= 1u16.checked_shl(op.db.index() as u32).unwrap_or(0);
                if self.primary_write_db.is_none() {
                    self.primary_write_db = Some(op.db);
                }
            }
            Opcode::Function => {
                self.has_function_call = true;
                let may_need_ts = match &op.p4 {
                    P4::Func { name, .. } => {
                        name.eq_ignore_ascii_case("current_date")
                            || name.eq_ignore_ascii_case("current_time")
                            || name.eq_ignore_ascii_case("current_timestamp")
                            || name.eq_ignore_ascii_case("date")
                            || name.eq_ignore_ascii_case("time")
                            || name.eq_ignore_ascii_case("datetime")
                            || name.eq_ignore_ascii_case("julianday")
                            || name.eq_ignore_ascii_case("unixepoch")
                            || name.eq_ignore_ascii_case("strftime")
                            || name.eq_ignore_ascii_case("timediff")
                    }
                    _ => true,
                };
                if may_need_ts {
                    self.needs_stmt_timestamp = true;
                }
            }
            _ => {}
        }
        let addr = self.ops.len();
        self.ops.push(op);
        addr
    }

    #[inline(always)]
    pub fn referenced_db_mask(&self) -> u16 {
        self.referenced_db_mask
    }

    #[inline(always)]
    pub fn primary_write_db(&self) -> DatabaseId {
        self.primary_write_db.unwrap_or(DatabaseId::MAIN)
    }

    #[inline(always)]
    pub fn has_function_call(&self) -> bool {
        self.has_function_call
    }

    #[inline(always)]
    pub fn needs_stmt_timestamp(&self) -> bool {
        self.needs_stmt_timestamp
    }

    pub fn patch_p2(&mut self, addr: usize, target: usize) {
        self.ops[addr].p2 = target as i32;
    }

    pub fn current_addr(&self) -> usize {
        self.ops.len()
    }

    /// Returns the distinct [`DatabaseId`] slots accessed by B-Tree or transaction instructions in this program.
    pub fn referenced_dbs(&self) -> Vec<DatabaseId> {
        let mut dbs = Vec::new();
        for ins in &self.ops {
            let db = match ins.opcode {
                Opcode::Transaction => {
                    if ins.db != DatabaseId::MAIN {
                        ins.db
                    } else {
                        DatabaseId(ins.p1.max(0) as usize)
                    }
                }
                Opcode::OpenRead
                | Opcode::OpenWrite
                | Opcode::OpenReadIndex
                | Opcode::OpenWriteIndex => ins.db,
                _ => continue,
            };
            if !dbs.contains(&db) {
                dbs.push(db);
            }
        }
        dbs
    }

    /// Formats `EXPLAIN <stmt>` rows identically to SQLite's 8-column `EXPLAIN` output:
    /// `(addr, opcode, p1, p2, p3, p4, p5, comment)`.
    pub fn explain_rows(&self) -> Vec<Vec<Value>> {
        self.ops
            .iter()
            .enumerate()
            .map(|(addr, ins)| {
                vec![
                    Value::Integer(addr as i64),
                    Value::Text(ins.opcode.name().to_owned()),
                    Value::Integer(i64::from(ins.p1)),
                    Value::Integer(i64::from(ins.p2)),
                    Value::Integer(i64::from(ins.p3)),
                    Value::Text(ins.p4.display_explain()),
                    Value::Integer(i64::from(ins.p5)),
                    ins.comment
                        .as_ref()
                        .map_or(Value::Null, |c| Value::Text(c.clone())),
                ]
            })
            .collect()
    }
}

impl Default for VdbeProgram {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a single `VdbeVm::step()` invocation (`SQLITE_ROW` vs `SQLITE_DONE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmStep {
    Row(Vec<Value>),
    Done,
}

#[derive(Debug)]
enum CursorState {
    Closed,
    Table {
        db: DatabaseId,
        cursor_id: Option<CursorId>,
        root_pgno: Pgno,
        writable: bool,
    },
    Index {
        db: DatabaseId,
        cursor_id: Option<CursorId>,
        root_pgno: Pgno,
        orders: Arc<[KeySortOrder]>,
        writable: bool,
        eq_prefix: Option<Vec<Value>>,
    },
    Ephemeral {
        rows: Vec<Vec<Value>>,
        pos: usize,
    },
    Sorter {
        orders: Vec<(usize, KeySortOrder)>,
        rows: Vec<Vec<Value>>,
        pos: usize,
    },
}

#[derive(Debug, Default)]
struct VdbePool {
    blobs: Vec<Vec<u8>>,
    texts: Vec<String>,
    scratch_rec: Vec<u8>,
}

impl VdbePool {
    #[inline]
    fn recycle_value(&mut self, val: Value) {
        match val {
            Value::Blob(mut b)
                if b.capacity() > 0 && b.capacity() <= 4096 && self.blobs.len() < 16 =>
            {
                b.clear();
                self.blobs.push(b);
            }
            Value::Text(mut s)
                if s.capacity() > 0 && s.capacity() <= 4096 && self.texts.len() < 32 =>
            {
                s.clear();
                self.texts.push(s);
            }
            _ => {}
        }
    }

    #[inline]
    fn clear_registers(&mut self, regs: &mut [RuntimeValue]) {
        for slot in regs {
            let old = std::mem::replace(slot, RuntimeValue::null());
            self.recycle_value(old.value);
        }
    }

    #[inline]
    fn assign_cloned_value(&mut self, dst: &mut RuntimeValue, src: &Value, subtype: u8) {
        match src {
            Value::Null => {
                let old = std::mem::replace(
                    dst,
                    RuntimeValue {
                        value: Value::Null,
                        subtype,
                    },
                );
                self.recycle_value(old.value);
            }
            Value::Integer(i) => {
                let old = std::mem::replace(
                    dst,
                    RuntimeValue {
                        value: Value::Integer(*i),
                        subtype,
                    },
                );
                self.recycle_value(old.value);
            }
            Value::Real(r) => {
                let old = std::mem::replace(
                    dst,
                    RuntimeValue {
                        value: Value::Real(*r),
                        subtype,
                    },
                );
                self.recycle_value(old.value);
            }
            Value::Text(s) => {
                if let Value::Text(existing) = &mut dst.value {
                    existing.clear();
                    existing.push_str(s);
                    dst.subtype = subtype;
                } else {
                    let mut buf = self.texts.pop().unwrap_or_default();
                    buf.clear();
                    buf.push_str(s);
                    let old = std::mem::replace(
                        dst,
                        RuntimeValue {
                            value: Value::Text(buf),
                            subtype,
                        },
                    );
                    self.recycle_value(old.value);
                }
            }
            Value::Blob(b) => {
                if let Value::Blob(existing) = &mut dst.value {
                    existing.clear();
                    existing.extend_from_slice(b);
                    dst.subtype = subtype;
                } else {
                    let mut buf = self.blobs.pop().unwrap_or_default();
                    buf.clear();
                    buf.extend_from_slice(b);
                    let old = std::mem::replace(
                        dst,
                        RuntimeValue {
                            value: Value::Blob(buf),
                            subtype,
                        },
                    );
                    self.recycle_value(old.value);
                }
            }
        }
    }

    #[inline]
    fn copy_params_into(&mut self, dst: &mut Vec<Value>, src: &[Value]) {
        while dst.len() > src.len() {
            if let Some(old) = dst.pop() {
                self.recycle_value(old);
            }
        }
        for (i, v) in src.iter().enumerate() {
            if i < dst.len() {
                match (&mut dst[i], v) {
                    (Value::Text(existing), Value::Text(s)) => {
                        existing.clear();
                        existing.push_str(s);
                    }
                    (Value::Blob(existing), Value::Blob(b)) => {
                        existing.clear();
                        existing.extend_from_slice(b);
                    }
                    (slot, other) => {
                        let next_val = match other {
                            Value::Text(s) => {
                                let mut buf = self.texts.pop().unwrap_or_default();
                                buf.clear();
                                buf.push_str(s);
                                Value::Text(buf)
                            }
                            Value::Blob(b) => {
                                let mut buf = self.blobs.pop().unwrap_or_default();
                                buf.clear();
                                buf.extend_from_slice(b);
                                Value::Blob(buf)
                            }
                            Value::Null => Value::Null,
                            Value::Integer(n) => Value::Integer(*n),
                            Value::Real(r) => Value::Real(*r),
                        };
                        let old = std::mem::replace(slot, next_val);
                        self.recycle_value(old);
                    }
                }
            } else {
                let next_val = match v {
                    Value::Text(s) => {
                        let mut buf = self.texts.pop().unwrap_or_default();
                        buf.clear();
                        buf.push_str(s);
                        Value::Text(buf)
                    }
                    Value::Blob(b) => {
                        let mut buf = self.blobs.pop().unwrap_or_default();
                        buf.clear();
                        buf.extend_from_slice(b);
                        Value::Blob(buf)
                    }
                    Value::Null => Value::Null,
                    Value::Integer(n) => Value::Integer(*n),
                    Value::Real(r) => Value::Real(*r),
                };
                dst.push(next_val);
            }
        }
    }
}

/// Reusable register and cursor scratch buffers for zero-allocation execution of compiled
/// write [`VdbeProgram`]s across repeated prepared statement executions.
#[derive(Debug, Default)]
pub struct VdbeWriteScratch {
    regs: Vec<RuntimeValue>,
    cursors: Vec<CursorState>,
    pool: VdbePool,
    scratch_jsonb: Vec<u8>,
    scratch_row: Vec<u8>,
}

impl VdbeWriteScratch {
    /// Reusable `(stored row copy, record under construction)` byte buffers for the direct
    /// point `UPDATE` / `DELETE` plans.
    #[inline(always)]
    pub fn point_dml_buffers_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<u8>) {
        (&mut self.scratch_row, &mut self.pool.scratch_rec)
    }

    /// Reusable byte buffer for encoding records without per-statement allocation.
    #[inline(always)]
    pub fn scratch_rec_mut(&mut self) -> &mut Vec<u8> {
        &mut self.pool.scratch_rec
    }

    /// Reusable record and JSONB byte buffers for zero-allocation direct `INSERT` execution.
    #[inline(always)]
    pub fn scratch_buffers_mut(&mut self) -> (&mut Vec<u8>, &mut Vec<u8>) {
        (&mut self.pool.scratch_rec, &mut self.scratch_jsonb)
    }
}

#[inline]
fn ensure_cursor_slot_vec(cursors: &mut Vec<CursorState>, cid: usize) {
    if cid >= cursors.len() {
        cursors.resize_with(cid + 1, || CursorState::Closed);
    }
}

#[inline]
fn close_cursor_slot_vec(cursors: &mut [CursorState], cid: usize, storage: &mut dyn BtreeAccess) {
    if cid < cursors.len() && !matches!(cursors[cid], CursorState::Closed) {
        match std::mem::replace(&mut cursors[cid], CursorState::Closed) {
            CursorState::Table {
                db,
                cursor_id: Some(cursor_id),
                ..
            }
            | CursorState::Index {
                db,
                cursor_id: Some(cursor_id),
                ..
            } => {
                if let Ok(btree) = storage.btree_mut(db) {
                    btree.close_cursor(cursor_id);
                }
            }
            _ => {}
        }
    }
}

#[inline]
fn close_cursors_vec(cursors: &mut [CursorState], storage: &mut dyn BtreeAccess) {
    for cid in 0..cursors.len() {
        close_cursor_slot_vec(cursors, cid, storage);
    }
}

/// Stateful VDBE virtual machine executing a [`VdbeProgram`] step-by-step (`sqlite3_step`).
pub struct VdbeVm {
    program: Arc<VdbeProgram>,
    pc: usize,
    halted: bool,
    regs: Vec<RuntimeValue>,
    cursors: Vec<CursorState>,
    params: Vec<Value>,
    pool: VdbePool,
    pub runtime_ctx: VmRuntimeContext,
}

impl VdbeVm {
    pub fn new(program: VdbeProgram, params: Vec<Value>, runtime_ctx: VmRuntimeContext) -> Self {
        Self::from_shared_owned_params(Arc::new(program), params, runtime_ctx)
    }

    pub fn from_shared(
        program: Arc<VdbeProgram>,
        params: &[Value],
        runtime_ctx: VmRuntimeContext,
    ) -> Self {
        let mut pool = VdbePool::default();
        let mut owned_params = Vec::with_capacity(params.len());
        pool.copy_params_into(&mut owned_params, params);
        let mut vm = Self::from_shared_owned_params(program, owned_params, runtime_ctx);
        vm.pool = pool;
        vm
    }

    fn from_shared_owned_params(
        program: Arc<VdbeProgram>,
        params: Vec<Value>,
        runtime_ctx: VmRuntimeContext,
    ) -> Self {
        let n_regs = program.num_registers.max(1) + 4;
        let n_curs = program.num_cursors + 2;
        let mut cursors = Vec::with_capacity(n_curs);
        for _ in 0..n_curs {
            cursors.push(CursorState::Closed);
        }
        Self {
            program,
            pc: 0,
            halted: false,
            regs: vec![RuntimeValue::null(); n_regs],
            cursors,
            params,
            pool: VdbePool::default(),
            runtime_ctx,
        }
    }

    /// Reinitializes an existing [`VdbeVm`] for a new execution of `program` without reallocating
    /// registers, cursors, or parameter buffers.
    pub fn prepare_for_reuse(
        &mut self,
        program: &Arc<VdbeProgram>,
        params: &[Value],
        runtime_ctx: VmRuntimeContext,
    ) {
        if !Arc::ptr_eq(&self.program, program) {
            self.program = Arc::clone(program);
        }
        let n_regs = self.program.num_registers.max(1) + 4;
        let clear_len = self.regs.len().min(n_regs);
        self.pool.clear_registers(&mut self.regs[..clear_len]);
        if self.regs.len() < n_regs {
            self.regs.resize(n_regs, RuntimeValue::null());
        } else if self.regs.len() > n_regs {
            self.pool.clear_registers(&mut self.regs[n_regs..]);
        }
        let n_curs = self.program.num_cursors + 2;
        for c in &mut self.cursors {
            *c = CursorState::Closed;
        }
        if self.cursors.len() < n_curs {
            self.cursors.resize_with(n_curs, || CursorState::Closed);
        }
        self.pool.copy_params_into(&mut self.params, params);
        self.pc = 0;
        self.halted = false;
        self.runtime_ctx = runtime_ctx;
    }

    /// Executes a non-returning write [`VdbeProgram`] to completion using reusable `scratch`
    /// register and cursor buffers without cloning `program` or `params`.
    pub fn run_write_program(
        program: &VdbeProgram,
        params: &[Value],
        runtime_ctx: &mut VmRuntimeContext,
        scratch: &mut VdbeWriteScratch,
        storage: &mut dyn BtreeAccess,
        services: &mut dyn ExecutionServices,
    ) -> Result<()> {
        let n_regs = program.num_registers.max(1) + 4;
        if scratch.regs.len() < n_regs {
            scratch.regs.resize(n_regs, RuntimeValue::null());
        }
        let n_curs = program.num_cursors + 2;
        if scratch.cursors.len() < n_curs {
            scratch.cursors.resize_with(n_curs, || CursorState::Closed);
        }
        let mut pc = 0usize;
        let mut halted = false;
        let res = step_ops(
            &program.ops,
            &mut pc,
            &mut halted,
            &mut scratch.regs,
            &mut scratch.cursors,
            &mut scratch.pool,
            params,
            runtime_ctx,
            storage,
            services,
        );
        close_cursors_vec(&mut scratch.cursors, storage);
        scratch.pool.clear_registers(&mut scratch.regs[..n_regs]);
        match res {
            Ok(VmStep::Done) => Ok(()),
            Ok(VmStep::Row(_)) => Err(SqliteError::misuse(
                "unexpected ResultRow in write VDBE program",
            )),
            Err(err) => Err(err),
        }
    }

    /// Closes all open [`Btree`] cursors owned by this VM without resetting program state.
    pub fn close_cursors(&mut self, storage: &mut dyn BtreeAccess) {
        close_cursors_vec(&mut self.cursors, storage);
    }

    /// Closes all open [`Btree`] cursors and resets the VM back to PC 0.
    pub fn reset_with_btree(&mut self, storage: &mut dyn BtreeAccess) {
        self.close_cursors(storage);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.pc = 0;
        self.halted = false;
        self.pool.clear_registers(&mut self.regs);
        for c in &mut self.cursors {
            *c = CursorState::Closed;
        }
    }

    pub fn column_names(&self) -> &[String] {
        &self.program.column_names
    }

    /// Returns an immutable view of register `idx` (including its ephemeral `subtype`).
    pub fn register(&self, idx: usize) -> Option<&RuntimeValue> {
        self.regs.get(idx)
    }

    /// Executes bytecode instructions until either `OP_ResultRow` yields a row or `OP_Halt` completes execution.
    pub fn step(
        &mut self,
        storage: &mut dyn BtreeAccess,
        services: &mut dyn ExecutionServices,
    ) -> Result<VmStep> {
        if self.halted {
            return Ok(VmStep::Done);
        }
        match step_ops(
            &self.program.ops,
            &mut self.pc,
            &mut self.halted,
            &mut self.regs,
            &mut self.cursors,
            &mut self.pool,
            &self.params,
            &mut self.runtime_ctx,
            storage,
            services,
        ) {
            Ok(VmStep::Row(row)) => Ok(VmStep::Row(row)),
            Ok(VmStep::Done) => {
                self.halted = true;
                self.close_cursors(storage);
                Ok(VmStep::Done)
            }
            Err(err) => {
                self.halted = true;
                self.close_cursors(storage);
                Err(err)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn step_ops(
    ops: &[Instruction],
    pc: &mut usize,
    halted: &mut bool,
    regs: &mut [RuntimeValue],
    cursors: &mut Vec<CursorState>,
    pool: &mut VdbePool,
    params: &[Value],
    runtime_ctx: &mut VmRuntimeContext,
    storage: &mut dyn BtreeAccess,
    services: &mut dyn ExecutionServices,
) -> Result<VmStep> {
    let mut local_ticks = 0u64;
    let res = step_ops_loop(
        ops,
        pc,
        halted,
        regs,
        cursors,
        pool,
        params,
        runtime_ctx,
        storage,
        services,
        &mut local_ticks,
    );
    if local_ticks != 0 {
        services.add_ticks_without_handler(local_ticks);
    }
    res
}

#[allow(clippy::too_many_arguments)]
fn step_ops_loop(
    ops: &[Instruction],
    pc: &mut usize,
    halted: &mut bool,
    regs: &mut [RuntimeValue],
    cursors: &mut Vec<CursorState>,
    pool: &mut VdbePool,
    params: &[Value],
    runtime_ctx: &mut VmRuntimeContext,
    storage: &mut dyn BtreeAccess,
    services: &mut dyn ExecutionServices,
    local_ticks: &mut u64,
) -> Result<VmStep> {
    let empty_orders: Arc<[KeySortOrder]> = Arc::from([]);
    let mut fast_poll = services.fast_interrupt_and_no_progress();
    while *pc < ops.len() {
        if let Some(flag) = fast_poll {
            if flag.load(AtomicOrdering::Relaxed) {
                return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
            }
            *local_ticks += 1;
        } else {
            services.check_interrupted()?;
            services.tick_progress()?;
            fast_poll = None;
        }
        let ins = &ops[*pc];
        let mut next_pc = *pc + 1;

        match ins.opcode {
            Opcode::Init => {
                if ins.p2 > 0 {
                    next_pc = ins.p2 as usize;
                }
            }
            Opcode::Noop => {}
            Opcode::Goto => {
                next_pc = ins.p2 as usize;
            }
            Opcode::Halt => {
                *halted = true;
                close_cursors_vec(cursors, storage);
                if ins.p1 != 0 {
                    let code = ErrorCode::from_raw_primary(ins.p1);
                    let msg = match &ins.p4 {
                        P4::Str(s) => s.clone(),
                        _ => code.as_str().to_owned(),
                    };
                    return Err(SqliteError::with_extended(ins.p1, msg));
                }
                return Ok(VmStep::Done);
            }
            Opcode::Transaction => {
                let db = if ins.db != DatabaseId::MAIN {
                    ins.db
                } else {
                    DatabaseId(ins.p1.max(0) as usize)
                };
                let btree = storage.btree_mut(db)?;
                if ins.p2 >= 2 {
                    btree.begin_exclusive()?;
                } else if ins.p2 != 0 {
                    btree.begin_write()?;
                } else {
                    btree.begin_read()?;
                }
            }
            Opcode::OpenRead | Opcode::OpenWrite => {
                let cid = ins.p1 as usize;
                let root_pgno = ins.p2 as Pgno;
                let writable = ins.opcode == Opcode::OpenWrite;
                let db = ins.db;
                ensure_cursor_slot_vec(cursors, cid);
                close_cursor_slot_vec(cursors, cid, storage);
                let cursor_id = if writable {
                    let _ = storage.btree_mut(db)?;
                    None
                } else {
                    let btree = storage.btree_mut(db)?;
                    Some(btree.open_table_cursor(root_pgno, false))
                };
                cursors[cid] = CursorState::Table {
                    db,
                    cursor_id,
                    root_pgno,
                    writable,
                };
            }
            Opcode::OpenReadIndex | Opcode::OpenWriteIndex => {
                let cid = ins.p1 as usize;
                let root_pgno = ins.p2 as Pgno;
                let writable = ins.opcode == Opcode::OpenWriteIndex;
                let db = ins.db;
                let orders: Arc<[KeySortOrder]> = match &ins.p4 {
                    P4::KeyInfo(k) => Arc::clone(k),
                    _ => Arc::clone(&empty_orders),
                };
                ensure_cursor_slot_vec(cursors, cid);
                close_cursor_slot_vec(cursors, cid, storage);
                let (cursor_id, orders_for_state) = if writable {
                    let _ = storage.btree_mut(db)?;
                    (None, orders)
                } else {
                    let btree = storage.btree_mut(db)?;
                    (
                        Some(btree.open_index_cursor(root_pgno, orders.to_vec(), false)),
                        Arc::clone(&empty_orders),
                    )
                };
                cursors[cid] = CursorState::Index {
                    db,
                    cursor_id,
                    root_pgno,
                    orders: orders_for_state,
                    writable,
                    eq_prefix: None,
                };
            }
            Opcode::OpenEphemeral => {
                let cid = ins.p1 as usize;
                ensure_cursor_slot_vec(cursors, cid);
                close_cursor_slot_vec(cursors, cid, storage);
                cursors[cid] = CursorState::Ephemeral {
                    rows: Vec::new(),
                    pos: 0,
                };
            }
            Opcode::Close => {
                let cid = ins.p1 as usize;
                close_cursor_slot_vec(cursors, cid, storage);
            }
            Opcode::Rewind => {
                let cid = ins.p1 as usize;
                let jump_empty = ins.p2 as usize;
                match &mut cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id,
                        root_pgno,
                        writable,
                    } => {
                        let (db, root, wr) = (*db, *root_pgno, *writable);
                        let btree = storage.btree_mut(db)?;
                        let id =
                            *cursor_id.get_or_insert_with(|| btree.open_table_cursor(root, wr));
                        if !btree.cursor_first(id)? {
                            next_pc = jump_empty;
                        }
                    }
                    CursorState::Index {
                        db,
                        cursor_id,
                        root_pgno,
                        orders,
                        writable,
                        eq_prefix,
                    } => {
                        let (db, root, wr) = (*db, *root_pgno, *writable);
                        *eq_prefix = None;
                        let btree = storage.btree_mut(db)?;
                        let id = *cursor_id.get_or_insert_with(|| {
                            btree.open_index_cursor(root, orders.to_vec(), wr)
                        });
                        if !btree.cursor_first(id)? {
                            next_pc = jump_empty;
                        }
                    }
                    CursorState::Ephemeral { rows, pos } => {
                        *pos = 0;
                        if rows.is_empty() {
                            next_pc = jump_empty;
                        }
                    }
                    _ => return Err(SqliteError::misuse("Rewind on invalid cursor")),
                }
            }
            Opcode::Next => {
                let cid = ins.p1 as usize;
                let loop_target = ins.p2 as usize;
                match &mut cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id: Some(id),
                        ..
                    }
                    | CursorState::Index {
                        db,
                        cursor_id: Some(id),
                        ..
                    } => {
                        let (db, id) = (*db, *id);
                        let btree = storage.btree_mut(db)?;
                        if btree.cursor_next(id)? {
                            next_pc = loop_target;
                        }
                    }
                    CursorState::Ephemeral { rows, pos } => {
                        *pos += 1;
                        if *pos < rows.len() {
                            next_pc = loop_target;
                        }
                    }
                    _ => return Err(SqliteError::misuse("Next on invalid cursor")),
                }
            }
            Opcode::SeekRowid => {
                // p1 = cursor, p2 = jump_if_not_found, p3 = reg containing target rowid
                let cid = ins.p1 as usize;
                let jump_miss = ins.p2 as usize;
                let target = regs[ins.p3 as usize].value.to_exact_rowid_i64();
                let Some(rowid) = target else {
                    *pc = jump_miss;
                    continue;
                };
                let (db, cursor_id) = match &mut cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id,
                        root_pgno,
                        writable,
                    } => {
                        let (db, root, wr) = (*db, *root_pgno, *writable);
                        let btree = storage.btree_mut(db)?;
                        let id =
                            *cursor_id.get_or_insert_with(|| btree.open_table_cursor(root, wr));
                        (db, id)
                    }
                    _ => return Err(SqliteError::misuse("SeekRowid on non-table cursor")),
                };
                let btree = storage.btree_mut(db)?;
                if !btree.cursor_seek_rowid(cursor_id, rowid)? {
                    next_pc = jump_miss;
                }
            }
            Opcode::SeekIndexEqual => {
                // p1 = index cursor, p2 = jump_if_empty, p3 = start_reg; p5 = key_count
                let cid = ins.p1 as usize;
                let jump_miss = ins.p2 as usize;
                let start_reg = ins.p3 as usize;
                let n_keys = ins.p5 as usize;
                let (db, cursor_id, prefix_ref): (DatabaseId, CursorId, &[Value]) =
                    match &mut cursors[cid] {
                        CursorState::Index {
                            db,
                            cursor_id,
                            root_pgno,
                            orders,
                            writable,
                            eq_prefix,
                        } => {
                            let (db, root, wr) = (*db, *root_pgno, *writable);
                            let id = match *cursor_id {
                                Some(existing) => existing,
                                None => {
                                    let opened = storage.btree_mut(db)?.open_index_cursor(
                                        root,
                                        orders.to_vec(),
                                        wr,
                                    );
                                    *cursor_id = Some(opened);
                                    opened
                                }
                            };
                            let buf = eq_prefix.get_or_insert_with(|| Vec::with_capacity(n_keys));
                            buf.clear();
                            for rv in &regs[start_reg..start_reg + n_keys] {
                                buf.push(rv.value.clone());
                            }
                            (db, id, buf.as_slice())
                        }
                        _ => return Err(SqliteError::misuse("SeekGE on non-index cursor")),
                    };
                let btree = storage.btree_mut(db)?;
                if !btree.cursor_seek_index_equal_first(cursor_id, prefix_ref)? {
                    next_pc = jump_miss;
                }
            }
            Opcode::IndexMatchNext => {
                let cid = ins.p1 as usize;
                let loop_target = ins.p2 as usize;
                let (db, cursor_id, prefix_ref): (DatabaseId, CursorId, &[Value]) =
                    match &cursors[cid] {
                        CursorState::Index {
                            db,
                            cursor_id: Some(id),
                            eq_prefix,
                            ..
                        } => (*db, *id, eq_prefix.as_deref().unwrap_or(&[])),
                        _ => return Err(SqliteError::misuse("IdxNext on non-index cursor")),
                    };
                let btree = storage.btree_mut(db)?;
                if btree.cursor_next_index_matching(cursor_id, prefix_ref)? {
                    next_pc = loop_target;
                }
            }
            Opcode::Rowid => {
                // p1 = cursor, p2 = dest_reg
                let cid = ins.p1 as usize;
                let dest = ins.p2 as usize;
                match &cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id: Some(id),
                        ..
                    } => {
                        let (db, id) = (*db, *id);
                        let rid = storage.btree_mut(db)?.cursor_rowid(id)?;
                        regs[dest] = RuntimeValue::plain(Value::Integer(rid));
                    }
                    CursorState::Index {
                        db,
                        cursor_id: Some(id),
                        ..
                    } => {
                        // In SQLite secondary indexes, the table rowid is the LAST column of the index record!
                        let (db, id) = (*db, *id);
                        let val = storage
                            .btree_mut(db)?
                            .cursor_last_column(id)?
                            .unwrap_or(Value::Null);
                        regs[dest] = RuntimeValue::plain(val);
                    }
                    _ => return Err(SqliteError::misuse("Rowid on unsupported cursor")),
                }
            }
            Opcode::Column => {
                // p1 = cursor, p2 = 0-based col_idx, p3 = dest_reg
                let cid = ins.p1 as usize;
                let col_idx = ins.p2 as usize;
                let dest = ins.p3 as usize;
                match &cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id: Some(id),
                        ..
                    }
                    | CursorState::Index {
                        db,
                        cursor_id: Some(id),
                        ..
                    } => {
                        let (db, id) = (*db, *id);
                        let prev = std::mem::replace(&mut regs[dest], RuntimeValue::null());
                        pool.recycle_value(prev.value);
                        let val = storage
                            .btree_mut(db)?
                            .cursor_column_pooled(id, col_idx, &mut pool.texts, &mut pool.blobs)?
                            .unwrap_or_else(|| match &ins.p4 {
                                P4::Int64(i) => Value::Integer(*i),
                                P4::Real(r) => Value::real(*r),
                                P4::Str(s) => Value::Text(s.clone()),
                                P4::Blob(b) => Value::Blob(b.clone()),
                                _ => Value::Null,
                            });
                        regs[dest] = RuntimeValue::plain(val);
                    }
                    CursorState::Ephemeral { rows, pos } => {
                        let src = rows[*pos].get(col_idx).unwrap_or(&Value::Null);
                        pool.assign_cloned_value(&mut regs[dest], src, 0);
                    }
                    _ => return Err(SqliteError::misuse("Column on closed cursor")),
                }
            }
            Opcode::MakeRecord => {
                // p1 = first_reg, p2 = n_regs, p3 = dest_reg
                let start = ins.p1 as usize;
                let count = ins.p2 as usize;
                let dest = ins.p3 as usize;
                let enc = storage
                    .btree_ref(DatabaseId::MAIN)
                    .map_or(TextEncoding::Utf8, Btree::encoding);
                // Extract any reusable Vec<u8> from `regs[dest]` only if `dest` is outside `start..start + count`.
                let mut bytes = if dest < start || dest >= start + count {
                    match std::mem::replace(&mut regs[dest], RuntimeValue::null()).value {
                        Value::Blob(b) => b,
                        other => {
                            pool.recycle_value(other);
                            pool.blobs.pop().unwrap_or_default()
                        }
                    }
                } else {
                    pool.blobs.pop().unwrap_or_default()
                };
                encode_runtime_record_into(&regs[start..start + count], enc, &mut bytes);
                let old =
                    std::mem::replace(&mut regs[dest], RuntimeValue::plain(Value::Blob(bytes)));
                pool.recycle_value(old.value);
            }
            Opcode::MakeRecordAff => {
                // p1 = first_reg, p2 = n_regs, p3 = dest_reg (outside the range), p4 = Affinity
                let start = ins.p1 as usize;
                let count = ins.p2 as usize;
                let dest = ins.p3 as usize;
                let enc = storage
                    .btree_ref(DatabaseId::MAIN)
                    .map_or(TextEncoding::Utf8, Btree::encoding);
                let mut swapped: Vec<(usize, f64)> = Vec::new();
                if let P4::Affinity(affs) = &ins.p4 {
                    for (k, aff) in affs.iter().enumerate().take(count) {
                        if *aff == TypeAffinity::Real
                            && let Value::Real(r) = regs[start + k].value
                            && let Some(i) = sqliters_types::real_affinity_storage_int(r)
                        {
                            regs[start + k].value = Value::Integer(i);
                            swapped.push((start + k, r));
                        }
                    }
                }
                let mut bytes = pool.blobs.pop().unwrap_or_default();
                encode_runtime_record_into(&regs[start..start + count], enc, &mut bytes);
                for (reg, r) in swapped {
                    regs[reg].value = Value::real(r);
                }
                let old =
                    std::mem::replace(&mut regs[dest], RuntimeValue::plain(Value::Blob(bytes)));
                pool.recycle_value(old.value);
            }
            Opcode::NewRowid => {
                // p1 = table cursor, p2 = dest_reg
                let cid = ins.p1 as usize;
                let dest = ins.p2 as usize;
                let (db, root_pgno) = match &cursors[cid] {
                    CursorState::Table { db, root_pgno, .. } => (*db, *root_pgno),
                    _ => return Err(SqliteError::misuse("NewRowid on non-table cursor")),
                };
                let rid = storage.btree_mut(db)?.next_rowid(root_pgno)?;
                regs[dest] = RuntimeValue::plain(Value::Integer(rid));
            }
            Opcode::Insert => {
                // p1 = table cursor, p2 = record_blob_reg, p3 = rowid_reg
                // p5 flags: 0x01 = last_rowid, 0x02 = changes, 0x04 = fail on duplicate rowid, 0x08 = PRIMARYKEY ext code
                let cid = ins.p1 as usize;
                let rec_reg = ins.p2 as usize;
                let rid_reg = ins.p3 as usize;
                let rowid = regs[rid_reg]
                    .value
                    .as_i64()
                    .ok_or_else(|| SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"))?;
                let Value::Blob(ref payload) = regs[rec_reg].value else {
                    return Err(SqliteError::misuse("OP_Insert expects record blob"));
                };
                let (db, prev_cur_id, root_pgno, writable) = match &mut cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id,
                        root_pgno,
                        writable,
                    } => (*db, cursor_id.take(), *root_pgno, *writable),
                    _ => return Err(SqliteError::misuse("Insert on non-table cursor")),
                };
                if !writable {
                    return Err(SqliteError::misuse("Insert on read-only table cursor"));
                }
                let btree = storage.btree_mut(db)?;
                // A cursor still resting on the target row (single-row UPDATE) tells the B-tree
                // where the row is, sparing a second descent.
                let mut position = None;
                if let Some(id) = prev_cur_id {
                    if (ins.p5 & 0x04) == 0 {
                        position = btree.table_cursor_position(id, root_pgno);
                    }
                    btree.close_cursor(id);
                }
                if (ins.p5 & 0x04) != 0 {
                    let inserted = btree.insert_table_unique(root_pgno, rowid, payload)?;
                    if !inserted {
                        let ext = if (ins.p5 & 0x08) != 0 {
                            sqliters_types::extended::SQLITE_CONSTRAINT_PRIMARYKEY
                        } else {
                            sqliters_types::extended::SQLITE_CONSTRAINT_ROWID
                        };
                        let msg = match &ins.p4 {
                            P4::Str(s) => s.clone(),
                            _ => "UNIQUE constraint failed".to_owned(),
                        };
                        return Err(SqliteError::constraint(ext, msg));
                    }
                } else {
                    btree.insert_table_at(root_pgno, rowid, payload, position)?;
                }
                if (ins.p5 & 0x01) != 0 {
                    runtime_ctx.last_insert_rowid = rowid;
                }
                if (ins.p5 & 0x02) != 0 {
                    runtime_ctx.last_changes += 1;
                    runtime_ctx.total_changes += 1;
                }
            }
            Opcode::Delete => {
                // p1 = table cursor, p2 = rowid_reg (if >0, else current cursor rowid)
                let cid = ins.p1 as usize;
                let (db, cursor_id, root_pgno, writable) = match &mut cursors[cid] {
                    CursorState::Table {
                        db,
                        cursor_id,
                        root_pgno,
                        writable,
                    } => {
                        let cid_opt = if ins.p2 > 0 {
                            cursor_id.take()
                        } else {
                            *cursor_id
                        };
                        (*db, cid_opt, *root_pgno, *writable)
                    }
                    _ => return Err(SqliteError::misuse("Delete on non-table cursor")),
                };
                if !writable {
                    return Err(SqliteError::misuse("Delete on read-only table cursor"));
                }
                let btree = storage.btree_mut(db)?;
                let mut position = None;
                let rowid = if ins.p2 > 0 {
                    if let Some(id) = cursor_id {
                        position = btree.table_cursor_position(id, root_pgno);
                        btree.close_cursor(id);
                    }
                    regs[ins.p2 as usize].value.to_integer_lossy()
                } else {
                    let id = cursor_id
                        .ok_or_else(|| SqliteError::misuse("Delete without positioned cursor"))?;
                    btree.cursor_rowid(id)?
                };
                if btree.delete_table_at(root_pgno, rowid, position)? && (ins.p5 & 0x02) != 0 {
                    runtime_ctx.last_changes += 1;
                    runtime_ctx.total_changes += 1;
                }
            }
            Opcode::IdxInsert => {
                // p1 = index cursor, p2 = record_blob_reg, p5 = 0 (non-unique), 1 (UNIQUE), 2 (PRIMARYKEY)
                let cid = ins.p1 as usize;
                let Value::Blob(ref payload) = regs[ins.p2 as usize].value else {
                    return Err(SqliteError::misuse("IdxInsert expects record blob"));
                };
                let CursorState::Index {
                    db,
                    root_pgno,
                    ref orders,
                    writable,
                    ..
                } = cursors[cid]
                else {
                    return Err(SqliteError::misuse("IdxInsert on non-index cursor"));
                };
                if !writable {
                    return Err(SqliteError::misuse("IdxInsert on read-only index cursor"));
                }
                let btree = storage.btree_mut(db)?;
                if ins.p5 != 0 {
                    let enc = btree.encoding();
                    let vals = decode_record_with_encoding(payload, enc)?;
                    if vals.len() > 1 {
                        let indexed_cols = &vals[..vals.len() - 1];
                        if indexed_cols.iter().all(|v| !v.is_null()) {
                            let new_rowid = vals.last();
                            let probe_id =
                                btree.open_index_cursor(root_pgno, orders.to_vec(), false);
                            let probe_res = (|| -> Result<bool> {
                                if !btree.cursor_seek_index_equal_first(probe_id, indexed_cols)? {
                                    return Ok(false);
                                }
                                loop {
                                    let existing_rowid = btree.cursor_last_column(probe_id)?;
                                    if existing_rowid.as_ref() != new_rowid {
                                        return Ok(true);
                                    }
                                    if !btree.cursor_next_index_matching(probe_id, indexed_cols)? {
                                        return Ok(false);
                                    }
                                }
                            })();
                            btree.close_cursor(probe_id);
                            if probe_res? {
                                let msg = match &ins.p4 {
                                    P4::Str(s) => s.clone(),
                                    _ => "UNIQUE constraint failed".to_owned(),
                                };
                                let ext = if ins.p5 == 2 {
                                    sqliters_types::extended::SQLITE_CONSTRAINT_PRIMARYKEY
                                } else {
                                    sqliters_types::extended::SQLITE_CONSTRAINT_UNIQUE
                                };
                                return Err(SqliteError::constraint(ext, msg));
                            }
                        }
                    }
                }
                btree.insert_index(root_pgno, payload, orders)?;
            }
            Opcode::IdxDelete => {
                // p1 = index cursor, p2 = start_reg, p3 = n_fields
                let cid = ins.p1 as usize;
                let start = ins.p2 as usize;
                let count = ins.p3 as usize;
                let CursorState::Index {
                    db,
                    root_pgno,
                    ref orders,
                    writable,
                    ..
                } = cursors[cid]
                else {
                    return Err(SqliteError::misuse("IdxDelete on non-index cursor"));
                };
                if !writable {
                    return Err(SqliteError::misuse("IdxDelete on read-only index cursor"));
                }
                let btree = storage.btree_mut(db)?;
                encode_runtime_record_into(
                    &regs[start..start + count],
                    btree.encoding(),
                    &mut pool.scratch_rec,
                );
                btree.delete_index_raw(root_pgno, &pool.scratch_rec, orders)?;
            }
            Opcode::ResultRow => {
                let start = ins.p1 as usize;
                let count = ins.p2 as usize;
                let row: Vec<Value> = if ins.p5 != 0 {
                    let mut out = Vec::with_capacity(count);
                    for rv in &mut regs[start..start + count] {
                        out.push(std::mem::replace(rv, RuntimeValue::null()).value);
                    }
                    out
                } else {
                    regs[start..start + count]
                        .iter()
                        .map(|rv| rv.value.clone())
                        .collect()
                };
                *pc = next_pc;
                return Ok(VmStep::Row(row));
            }
            Opcode::Integer => {
                regs[ins.p2 as usize] = RuntimeValue::plain(Value::Integer(i64::from(ins.p1)));
            }
            Opcode::Int64 => {
                let P4::Int64(v) = ins.p4 else {
                    return Err(SqliteError::misuse("Int64 missing P4::Int64"));
                };
                regs[ins.p2 as usize] = RuntimeValue::plain(Value::Integer(v));
            }
            Opcode::Real => {
                let P4::Real(v) = ins.p4 else {
                    return Err(SqliteError::misuse("Real missing P4::Real"));
                };
                regs[ins.p2 as usize] = RuntimeValue::plain(Value::real(v));
            }
            Opcode::String8 => {
                let P4::Str(ref s) = ins.p4 else {
                    return Err(SqliteError::misuse("String8 missing P4::Str"));
                };
                regs[ins.p2 as usize] = RuntimeValue::plain(Value::Text(s.clone()));
            }
            Opcode::Blob => {
                let P4::Blob(ref b) = ins.p4 else {
                    return Err(SqliteError::misuse("Blob missing P4::Blob"));
                };
                regs[ins.p2 as usize] = RuntimeValue::plain(Value::Blob(b.clone()));
            }
            Opcode::Null => {
                let start = ins.p2 as usize;
                let end = if ins.p3 > ins.p2 {
                    ins.p3 as usize
                } else {
                    start
                };
                pool.clear_registers(&mut regs[start..=end]);
            }
            Opcode::Variable => {
                // p1 = 1-based parameter index, p2 = dest_reg
                let idx = (ins.p1 as usize).saturating_sub(1);
                let src = params.get(idx).unwrap_or(&Value::Null);
                pool.assign_cloned_value(&mut regs[ins.p2 as usize], src, 0);
            }
            Opcode::Copy => {
                let src = ins.p1 as usize;
                let dst = ins.p2 as usize;
                let extra = ins.p3.max(0) as usize;
                let clear_subtype = (ins.p5 & 0x0002) != 0;
                for i in 0..=extra {
                    let s_idx = src + i;
                    let d_idx = dst + i;
                    if s_idx == d_idx {
                        if clear_subtype {
                            regs[d_idx].clear_subtype();
                        }
                    } else if s_idx < d_idx {
                        let (left, right) = regs.split_at_mut(d_idx);
                        let src_rv = &left[s_idx];
                        let sub = if clear_subtype { 0 } else { src_rv.subtype };
                        pool.assign_cloned_value(&mut right[0], &src_rv.value, sub);
                    } else {
                        let (left, right) = regs.split_at_mut(s_idx);
                        let src_rv = &right[0];
                        let sub = if clear_subtype { 0 } else { src_rv.subtype };
                        pool.assign_cloned_value(&mut left[d_idx], &src_rv.value, sub);
                    }
                }
            }
            Opcode::Move => {
                let src = ins.p1 as usize;
                let dst = ins.p2 as usize;
                let count = ins.p3.max(1) as usize;
                for i in 0..count {
                    let v = std::mem::replace(&mut regs[src + i], RuntimeValue::null());
                    regs[dst + i] = v;
                }
            }
            Opcode::Add
            | Opcode::Subtract
            | Opcode::Multiply
            | Opcode::Divide
            | Opcode::Remainder => {
                let lhs = &regs[ins.p1 as usize].value;
                let rhs = &regs[ins.p2 as usize].value;
                regs[ins.p3 as usize] = RuntimeValue::plain(eval_arith(ins.opcode, lhs, rhs));
            }
            Opcode::BitAnd | Opcode::BitOr | Opcode::ShiftLeft | Opcode::ShiftRight => {
                let lhs = &regs[ins.p1 as usize].value;
                let rhs = &regs[ins.p2 as usize].value;
                if lhs.is_null() || rhs.is_null() {
                    regs[ins.p3 as usize] = RuntimeValue::null();
                } else {
                    let a = lhs.to_integer_lossy();
                    let b = rhs.to_integer_lossy();
                    let res = match ins.opcode {
                        Opcode::BitAnd => a & b,
                        Opcode::BitOr => a | b,
                        Opcode::ShiftLeft => shift_left_sqlite(a, b),
                        Opcode::ShiftRight => shift_right_sqlite(a, b),
                        _ => unreachable!(),
                    };
                    regs[ins.p3 as usize] = RuntimeValue::plain(Value::Integer(res));
                }
            }
            Opcode::BitNot => {
                let v = &regs[ins.p1 as usize].value;
                regs[ins.p2 as usize] = if v.is_null() {
                    RuntimeValue::null()
                } else {
                    RuntimeValue::plain(Value::Integer(!v.to_integer_lossy()))
                };
            }
            Opcode::Negate => {
                let v = &regs[ins.p1 as usize].value;
                regs[ins.p2 as usize] = RuntimeValue::plain(eval_unary_negate(v));
            }
            Opcode::Concat => {
                let lhs = &regs[ins.p1 as usize].value;
                let rhs = &regs[ins.p2 as usize].value;
                regs[ins.p3 as usize] = if lhs.is_null() || rhs.is_null() {
                    RuntimeValue::null()
                } else {
                    let mut s = lhs.to_text_lossy();
                    s.push_str(&rhs.to_text_lossy());
                    RuntimeValue::plain(Value::Text(s))
                };
            }
            Opcode::Eq
            | Opcode::Ne
            | Opcode::Lt
            | Opcode::Le
            | Opcode::Gt
            | Opcode::Ge
            | Opcode::Is
            | Opcode::IsNot => {
                // Boolean evaluation into register: p1 = lhs, p2 = rhs, p3 = dest_reg
                let aff = p5_to_affinity(ins.p5);
                let enc = storage
                    .btree_ref(DatabaseId::MAIN)
                    .map_or(TextEncoding::Utf8, Btree::encoding);
                let out = match &ins.p4 {
                    P4::Str(custom_coll) => {
                        let lhs = regs[ins.p1 as usize].value.clone();
                        let rhs = regs[ins.p2 as usize].value.clone();
                        let r = eval_comparison_with_services(
                            ins.opcode,
                            &lhs,
                            &rhs,
                            aff,
                            custom_coll,
                            enc,
                            services,
                        )?;
                        fast_poll = services.fast_interrupt_and_no_progress();
                        r
                    }
                    P4::Collation(c) => eval_comparison_with_affinity(
                        ins.opcode,
                        &regs[ins.p1 as usize].value,
                        &regs[ins.p2 as usize].value,
                        aff,
                        *c,
                        enc,
                    ),
                    _ => eval_comparison_with_affinity(
                        ins.opcode,
                        &regs[ins.p1 as usize].value,
                        &regs[ins.p2 as usize].value,
                        aff,
                        Collation::Binary,
                        enc,
                    ),
                };
                let old = std::mem::replace(&mut regs[ins.p3 as usize], RuntimeValue::plain(out));
                pool.recycle_value(old.value);
            }
            Opcode::And => {
                let a = regs[ins.p1 as usize].value.truthiness();
                let b = regs[ins.p2 as usize].value.truthiness();
                regs[ins.p3 as usize] = RuntimeValue::plain(match (a, b) {
                    (Some(false), _) | (_, Some(false)) => Value::Integer(0),
                    (Some(true), Some(true)) => Value::Integer(1),
                    _ => Value::Null,
                });
            }
            Opcode::Or => {
                let a = regs[ins.p1 as usize].value.truthiness();
                let b = regs[ins.p2 as usize].value.truthiness();
                regs[ins.p3 as usize] = RuntimeValue::plain(match (a, b) {
                    (Some(true), _) | (_, Some(true)) => Value::Integer(1),
                    (Some(false), Some(false)) => Value::Integer(0),
                    _ => Value::Null,
                });
            }
            Opcode::Not => {
                let a = regs[ins.p1 as usize].value.truthiness();
                regs[ins.p2 as usize] = RuntimeValue::plain(match a {
                    None => Value::Null,
                    Some(b) => Value::Integer(i64::from(!b)),
                });
            }
            Opcode::IsNull => {
                // p1 = reg; if p2 >= 0 and p3 == 0: jump mode; if p3 > 0: store 0/1 in reg p3
                let is_n = regs[ins.p1 as usize].value.is_null();
                if ins.p3 > 0 {
                    regs[ins.p3 as usize] = RuntimeValue::plain(Value::Integer(i64::from(is_n)));
                } else if is_n {
                    next_pc = ins.p2 as usize;
                }
            }
            Opcode::NotNull => {
                let not_n = !regs[ins.p1 as usize].value.is_null();
                if ins.p3 > 0 {
                    regs[ins.p3 as usize] = RuntimeValue::plain(Value::Integer(i64::from(not_n)));
                } else if not_n {
                    next_pc = ins.p2 as usize;
                }
            }
            Opcode::JumpIfTrue => {
                if regs[ins.p1 as usize].value.truthiness() == Some(true) {
                    next_pc = ins.p2 as usize;
                }
            }
            Opcode::JumpIfFalse => {
                // Jump when false, or when NULL if p3 != 0 (`jumpIfNull` flag in SQLite `OP_IfNot`!)
                match regs[ins.p1 as usize].value.truthiness() {
                    Some(false) => next_pc = ins.p2 as usize,
                    None if ins.p3 != 0 => next_pc = ins.p2 as usize,
                    _ => {}
                }
            }
            Opcode::JumpIfNull => {
                if regs[ins.p1 as usize].value.is_null() {
                    next_pc = ins.p2 as usize;
                }
            }
            Opcode::MustBeInt => {
                // p1 = reg, p2 = jump_if_not_int (0 = raise SQLITE_MISMATCH)
                let reg = ins.p1 as usize;
                let cur = std::mem::replace(&mut regs[reg].value, Value::Null);
                let coerced = TypeAffinity::Integer.apply(cur);
                if let Value::Integer(i) = coerced {
                    regs[reg] = RuntimeValue::plain(Value::Integer(i));
                } else if ins.p2 > 0 {
                    regs[reg] = RuntimeValue::plain(coerced);
                    next_pc = ins.p2 as usize;
                } else {
                    regs[reg] = RuntimeValue::plain(coerced);
                    return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                }
            }
            Opcode::Affinity => {
                // p1 = start_reg, p2 = count, p4 = Affinity(vec)
                let start = ins.p1 as usize;
                if let P4::Affinity(ref affs) = ins.p4 {
                    for (i, &aff) in affs.iter().enumerate() {
                        let v = std::mem::replace(&mut regs[start + i].value, Value::Null);
                        regs[start + i] = RuntimeValue::plain(aff.apply(v));
                    }
                }
            }
            Opcode::RealAffinity => {
                let reg = &mut regs[ins.p1 as usize];
                if let Value::Integer(i) = reg.value {
                    reg.value = Value::real(i as f64);
                }
            }
            Opcode::Cast => {
                // p1 = src_reg, p2 = dest_reg, p4 = Str(type_name)
                let src = regs[ins.p1 as usize].clone();
                let type_name = match &ins.p4 {
                    P4::Str(s) => s.as_str(),
                    _ => "",
                };
                let enc = storage
                    .btree_ref(DatabaseId::MAIN)
                    .map_or(TextEncoding::Utf8, Btree::encoding);
                regs[ins.p2 as usize] = eval_cast_rt_with_encoding(src, type_name, enc);
            }
            Opcode::Function => {
                // p1 = first_arg_reg, p3 = dest_reg, p4 = Func { name, argc }
                let start = ins.p1 as usize;
                let dest = ins.p3 as usize;
                let P4::Func { ref name, argc } = ins.p4 else {
                    return Err(SqliteError::misuse("Function missing P4::Func"));
                };
                let res = services.call_scalar(name, &regs[start..start + argc])?;
                fast_poll = services.fast_interrupt_and_no_progress();
                regs[dest] = res;
            }
            Opcode::SorterOpen => {
                // p1 = cursor, p4 = KeyInfo(orders), IntArray encoded via p2.. or KeyInfo
                let cid = ins.p1 as usize;
                let orders = match &ins.p4 {
                    P4::KeyInfo(k) => k.iter().cloned().enumerate().collect(),
                    _ => Vec::new(),
                };
                ensure_cursor_slot_vec(cursors, cid);
                close_cursor_slot_vec(cursors, cid, storage);
                cursors[cid] = CursorState::Sorter {
                    orders,
                    rows: Vec::new(),
                    pos: 0,
                };
            }
            Opcode::SorterInsert => {
                // p1 = sorter cursor, p2 = start_reg, p3 = n_cols
                let cid = ins.p1 as usize;
                let start = ins.p2 as usize;
                let count = ins.p3 as usize;
                let row: Vec<Value> = regs[start..start + count]
                    .iter()
                    .map(|rv| rv.value.clone())
                    .collect();
                if let CursorState::Sorter { rows, .. } = &mut cursors[cid] {
                    rows.push(row);
                } else {
                    return Err(SqliteError::misuse("SorterInsert on non-sorter cursor"));
                }
            }
            Opcode::SorterSort => {
                let cid = ins.p1 as usize;
                let jump_empty = ins.p2 as usize;
                if let CursorState::Sorter { orders, rows, pos } = &mut cursors[cid] {
                    if rows.is_empty() {
                        next_pc = jump_empty;
                    } else {
                        let specs = orders.clone();
                        let enc = storage
                            .btree_ref(DatabaseId::MAIN)
                            .map_or(TextEncoding::Utf8, Btree::encoding);
                        fallible_sort_by(rows, |a, b| compare_sorter_rows(a, b, &specs, enc))?;
                        *pos = 0;
                    }
                } else {
                    return Err(SqliteError::misuse("SorterSort on non-sorter cursor"));
                }
            }
            Opcode::SorterColumn => {
                let cid = ins.p1 as usize;
                let col_idx = ins.p2 as usize;
                let dest = ins.p3 as usize;
                if let CursorState::Sorter { rows, pos, .. } = &cursors[cid] {
                    regs[dest] = RuntimeValue::plain(
                        rows[*pos].get(col_idx).cloned().unwrap_or(Value::Null),
                    );
                } else {
                    return Err(SqliteError::misuse("SorterColumn on non-sorter cursor"));
                }
            }
            Opcode::SorterNext => {
                let cid = ins.p1 as usize;
                let loop_target = ins.p2 as usize;
                if let CursorState::Sorter { rows, pos, .. } = &mut cursors[cid] {
                    *pos += 1;
                    if *pos < rows.len() {
                        next_pc = loop_target;
                    }
                } else {
                    return Err(SqliteError::misuse("SorterNext on non-sorter cursor"));
                }
            }
        }

        *pc = next_pc;
    }

    *halted = true;
    Ok(VmStep::Done)
}

pub fn eval_arith(op: Opcode, lhs: &Value, rhs: &Value) -> Value {
    if lhs.is_null() || rhs.is_null() {
        return Value::Null;
    }
    let a_num = coerce_numeric_for_arith(lhs);
    let b_num = coerce_numeric_for_arith(rhs);
    match (a_num, b_num) {
        (Value::Integer(a), Value::Integer(b)) => match op {
            Opcode::Add => a
                .checked_add(b)
                .map_or_else(|| Value::real((a as f64) + (b as f64)), Value::Integer),
            Opcode::Subtract => a
                .checked_sub(b)
                .map_or_else(|| Value::real((a as f64) - (b as f64)), Value::Integer),
            Opcode::Multiply => a
                .checked_mul(b)
                .map_or_else(|| Value::real((a as f64) * (b as f64)), Value::Integer),
            Opcode::Divide => {
                if b == 0 {
                    Value::Null
                } else if a == i64::MIN && b == -1 {
                    Value::real(-(a as f64))
                } else {
                    Value::Integer(a / b)
                }
            }
            Opcode::Remainder => {
                if b == 0 {
                    Value::Null
                } else if a == i64::MIN && b == -1 {
                    Value::Integer(0)
                } else {
                    Value::Integer(a % b)
                }
            }
            _ => unreachable!(),
        },
        (a, b) => {
            let fa = a.to_real_lossy();
            let fb = b.to_real_lossy();
            match op {
                Opcode::Add => Value::real(fa + fb),
                Opcode::Subtract => Value::real(fa - fb),
                Opcode::Multiply => Value::real(fa * fb),
                Opcode::Divide => {
                    if fb == 0.0 {
                        Value::Null
                    } else {
                        Value::real(fa / fb)
                    }
                }
                Opcode::Remainder => {
                    let ia = a.to_integer_lossy();
                    let ib = b.to_integer_lossy();
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

pub fn eval_unary_negate(v: &Value) -> Value {
    match coerce_numeric_for_arith(v) {
        Value::Null => Value::Null,
        Value::Integer(i) => i
            .checked_neg()
            .map_or_else(|| Value::real(-(i as f64)), Value::Integer),
        Value::Real(r) => Value::real(-r),
        other => Value::Integer(other.to_integer_lossy().wrapping_neg()),
    }
}

fn coerce_numeric_for_arith(v: &Value) -> Value {
    match v {
        Value::Integer(_) | Value::Real(_) => v.clone(),
        Value::Text(s) => sqliters_types::parse_numeric_text(s)
            .unwrap_or_else(|| Value::Integer(v.to_integer_lossy())),
        Value::Blob(_) => Value::Integer(v.to_integer_lossy()),
        Value::Null => Value::Null,
    }
}

fn shift_left_sqlite(a: i64, b: i64) -> i64 {
    if b < 0 {
        return shift_right_sqlite(a, b.saturating_neg());
    }
    if b >= 64 { 0 } else { a.wrapping_shl(b as u32) }
}

fn shift_right_sqlite(a: i64, b: i64) -> i64 {
    if b < 0 {
        return shift_left_sqlite(a, b.saturating_neg());
    }
    if b >= 64 {
        if a < 0 { -1 } else { 0 }
    } else {
        a >> (b as u32)
    }
}

/// Computes SQLite's comparison affinity (`sqlite3CompareAffinity` in `sqlite/src/expr.c`).
///
/// `None` represents `SQLITE_AFF_NONE` (an expression without column/CAST affinity, such as a
/// literal, arithmetic expression, or `+col`); `Some(TypeAffinity::Blob)` represents `SQLITE_AFF_BLOB`
/// (an untyped column or `CAST(... AS BLOB)`).
pub const fn sqlite_compare_affinity(
    aff1: Option<TypeAffinity>,
    aff2: Option<TypeAffinity>,
) -> TypeAffinity {
    match (aff1, aff2) {
        (Some(a1), Some(a2)) => {
            if matches!(
                a1,
                TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real
            ) || matches!(
                a2,
                TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real
            ) {
                TypeAffinity::Numeric
            } else {
                TypeAffinity::Blob
            }
        }
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => TypeAffinity::Blob,
    }
}

pub const fn affinity_to_p5(aff: TypeAffinity) -> u8 {
    match aff {
        TypeAffinity::Blob => 0,
        TypeAffinity::Text => 1,
        TypeAffinity::Numeric => 2,
        TypeAffinity::Integer => 3,
        TypeAffinity::Real => 4,
    }
}

pub const fn p5_to_affinity(p5: u16) -> TypeAffinity {
    match p5 & 0x07 {
        1 => TypeAffinity::Text,
        2 => TypeAffinity::Numeric,
        3 => TypeAffinity::Integer,
        4 => TypeAffinity::Real,
        _ => TypeAffinity::Blob,
    }
}

/// Evaluates SQLite comparison operators (`=`, `!=`, `<`, `<=`, `>`, `>=`, `IS`, `IS NOT`)
/// with `TypeAffinity::Blob` (no implicit affinity conversion).
pub fn eval_comparison(op: Opcode, lhs: &Value, rhs: &Value, coll: Collation) -> Value {
    eval_comparison_with_encoding(op, lhs, rhs, coll, TextEncoding::Utf8)
}

pub fn eval_comparison_with_encoding(
    op: Opcode,
    lhs: &Value,
    rhs: &Value,
    coll: Collation,
    enc: TextEncoding,
) -> Value {
    eval_comparison_with_affinity(op, lhs, rhs, TypeAffinity::Blob, coll, enc)
}

pub fn eval_comparison_with_affinity(
    op: Opcode,
    lhs: &Value,
    rhs: &Value,
    aff: TypeAffinity,
    coll: Collation,
    enc: TextEncoding,
) -> Value {
    if matches!(op, Opcode::Is | Opcode::IsNot) {
        let eq = match (lhs.is_null(), rhs.is_null()) {
            (true, true) => true,
            (true, false) | (false, true) => false,
            (false, false) => compare_with_affinity(lhs, rhs, aff, coll, enc) == Ordering::Equal,
        };
        let res = if op == Opcode::Is { eq } else { !eq };
        return Value::Integer(i64::from(res));
    }

    if lhs.is_null() || rhs.is_null() {
        return Value::Null;
    }

    let ord = compare_with_affinity(lhs, rhs, aff, coll, enc);
    let b = match op {
        Opcode::Eq => ord == Ordering::Equal,
        Opcode::Ne => ord != Ordering::Equal,
        Opcode::Lt => ord == Ordering::Less,
        Opcode::Le => ord != Ordering::Greater,
        Opcode::Gt => ord == Ordering::Greater,
        Opcode::Ge => ord != Ordering::Less,
        _ => unreachable!(),
    };
    Value::Integer(i64::from(b))
}

pub fn eval_comparison_with_services(
    op: Opcode,
    lhs: &Value,
    rhs: &Value,
    aff: TypeAffinity,
    custom_coll: &str,
    enc: TextEncoding,
    services: &mut dyn ExecutionServices,
) -> Result<Value> {
    if matches!(op, Opcode::Is | Opcode::IsNot) {
        let eq = match (lhs.is_null(), rhs.is_null()) {
            (true, true) => true,
            (true, false) | (false, true) => false,
            (false, false) => {
                compare_with_services(lhs, rhs, aff, custom_coll, enc, services)? == Ordering::Equal
            }
        };
        let res = if op == Opcode::Is { eq } else { !eq };
        return Ok(Value::Integer(i64::from(res)));
    }

    if lhs.is_null() || rhs.is_null() {
        return Ok(Value::Null);
    }

    let ord = compare_with_services(lhs, rhs, aff, custom_coll, enc, services)?;
    let b = match op {
        Opcode::Eq => ord == Ordering::Equal,
        Opcode::Ne => ord != Ordering::Equal,
        Opcode::Lt => ord == Ordering::Less,
        Opcode::Le => ord != Ordering::Greater,
        Opcode::Gt => ord == Ordering::Greater,
        Opcode::Ge => ord != Ordering::Less,
        _ => unreachable!(),
    };
    Ok(Value::Integer(i64::from(b)))
}

fn compare_with_services(
    lhs: &Value,
    rhs: &Value,
    aff: TypeAffinity,
    custom_coll: &str,
    enc: TextEncoding,
    services: &mut dyn ExecutionServices,
) -> Result<Ordering> {
    let (lc, rc) = match aff {
        TypeAffinity::Blob => (lhs.clone(), rhs.clone()),
        TypeAffinity::Text => (
            TypeAffinity::Text.apply(lhs.clone()),
            TypeAffinity::Text.apply(rhs.clone()),
        ),
        TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real => {
            let l = if matches!(lhs, Value::Text(_)) {
                TypeAffinity::Numeric.apply(lhs.clone())
            } else {
                lhs.clone()
            };
            let r = if matches!(rhs, Value::Text(_)) {
                TypeAffinity::Numeric.apply(rhs.clone())
            } else {
                rhs.clone()
            };
            (l, r)
        }
    };
    if let (Value::Text(ls), Value::Text(rs)) = (&lc, &rc) {
        services.compare_collated(custom_coll, ls, rs)
    } else {
        Ok(lc.cmp_sqlite_with_encoding(&rc, Collation::Binary, enc))
    }
}

fn compare_with_affinity(
    lhs: &Value,
    rhs: &Value,
    aff: TypeAffinity,
    coll: Collation,
    enc: TextEncoding,
) -> Ordering {
    match aff {
        TypeAffinity::Blob => lhs.cmp_sqlite_with_encoding(rhs, coll, enc),
        TypeAffinity::Text => {
            let lc = TypeAffinity::Text.apply(lhs.clone());
            let rc = TypeAffinity::Text.apply(rhs.clone());
            lc.cmp_sqlite_with_encoding(&rc, coll, enc)
        }
        TypeAffinity::Numeric | TypeAffinity::Integer | TypeAffinity::Real => {
            // In C SQLite (`vdbe.c` `OP_Eq..OP_Ge`), numeric comparison affinity only converts
            // `Text` operands to `Integer` or `Real`, preserving full 64-bit `Integer` vs `Real` precision!
            let lc = if matches!(lhs, Value::Text(_)) {
                TypeAffinity::Numeric.apply(lhs.clone())
            } else {
                lhs.clone()
            };
            let rc = if matches!(rhs, Value::Text(_)) {
                TypeAffinity::Numeric.apply(rhs.clone())
            } else {
                rhs.clone()
            };
            lc.cmp_sqlite_with_encoding(&rc, coll, enc)
        }
    }
}

pub fn eval_cast(val: Value, type_name: &str) -> Value {
    eval_cast_with_encoding(val, type_name, TextEncoding::Utf8)
}

/// Evaluates `CAST(val AS type_name)` while matching C SQLite 3.54.1 `sqlite3VdbeMemCast`
/// (`sqlite/src/vdbemem.c:926-966`) subtype preservation/clearing rules:
/// - `NULL` is unchanged (`subtype` preserved).
/// - `CAST(... AS TEXT)` preserves `MEM_Subtype` (`vdbemem.c:958` clears `MEM_Int..MEM_Zero`
///   without touching `MEM_Subtype`).
/// - `CAST(... AS NUMERIC)` preserves `MEM_Subtype` only when `val` is already `Integer` or `Real`;
///   converting `Text`/`Blob` invokes `MemSetTypeFlag` which clears `MEM_TypeMask` (`MEM_Subtype`).
/// - `CAST(... AS BLOB | INTEGER | REAL)` always clears `MEM_Subtype` (`subtype = 0`).
pub fn eval_cast_rt_with_encoding(
    rv: RuntimeValue,
    type_name: &str,
    enc: TextEncoding,
) -> RuntimeValue {
    if rv.value.is_null() {
        return rv;
    }
    let aff = TypeAffinity::from_declared_type(type_name);
    let preserve_subtype = match aff {
        TypeAffinity::Text => true,
        TypeAffinity::Numeric => matches!(rv.value, Value::Integer(_) | Value::Real(_)),
        TypeAffinity::Blob | TypeAffinity::Integer | TypeAffinity::Real => false,
    };
    let out_val = eval_cast_with_encoding(rv.value, type_name, enc);
    if preserve_subtype {
        RuntimeValue {
            value: out_val,
            subtype: rv.subtype,
        }
    } else {
        RuntimeValue::plain(out_val)
    }
}

pub fn eval_cast_with_encoding(val: Value, type_name: &str, enc: TextEncoding) -> Value {
    if val.is_null() {
        return Value::Null;
    }
    match TypeAffinity::from_declared_type(type_name) {
        TypeAffinity::Blob => match val {
            Value::Blob(_) => val,
            other => {
                let s = other.to_text_lossy();
                let bytes = match enc {
                    TextEncoding::Utf8 => s.into_bytes(),
                    TextEncoding::Utf16Le => s.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                    TextEncoding::Utf16Be => s.encode_utf16().flat_map(u16::to_be_bytes).collect(),
                };
                Value::Blob(bytes)
            }
        },
        TypeAffinity::Text => match val {
            Value::Blob(b) => Value::Text(decode_blob_for_cast(&b, enc)),
            other => Value::Text(other.to_text_lossy()),
        },
        TypeAffinity::Integer => Value::Integer(val.to_integer_lossy()),
        TypeAffinity::Real => Value::real(val.to_real_lossy()),
        TypeAffinity::Numeric => match val {
            Value::Integer(_) | Value::Real(_) => val,
            Value::Null => Value::Null,
            Value::Text(s) => cast_text_to_numeric(&s),
            Value::Blob(b) => {
                let s = decode_blob_for_cast(&b, enc);
                cast_text_to_numeric(&s)
            }
        },
    }
}

fn decode_blob_for_cast(b: &[u8], enc: TextEncoding) -> String {
    match enc {
        TextEncoding::Utf8 => String::from_utf8_lossy(b).into_owned(),
        TextEncoding::Utf16Le => {
            let units: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            String::from_utf16_lossy(&units)
        }
        TextEncoding::Utf16Be => {
            let units: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            String::from_utf16_lossy(&units)
        }
    }
}

fn cast_text_to_numeric(s: &str) -> Value {
    let trimmed = sqliters_types::sqlite_trim_ascii(s);
    if !trimmed.is_empty()
        && !trimmed.contains('.')
        && !trimmed.contains('e')
        && !trimmed.contains('E')
        && let Ok(i) = trimmed.parse::<i64>()
    {
        return Value::Integer(i);
    }
    let f = sqliters_types::parse_leading_f64(s);
    if f.is_finite() && (-2_251_799_813_685_248.0..2_251_799_813_685_248.0).contains(&f) {
        let i = f as i64;
        if (i as f64) == f {
            return Value::Integer(i);
        }
    }
    Value::real(f)
}

fn compare_sorter_rows(
    a: &[Value],
    b: &[Value],
    specs: &[(usize, KeySortOrder)],
    enc: TextEncoding,
) -> Result<Ordering> {
    for (col_idx, spec) in specs {
        let va = &a[*col_idx];
        let vb = &b[*col_idx];
        let ord = match (va.is_null(), vb.is_null()) {
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
                let raw = va.cmp_sqlite_fallible(vb, spec, enc)?;
                if spec.desc { raw.reverse() } else { raw }
            }
        };
        if ord != Ordering::Equal {
            return Ok(ord);
        }
    }
    Ok(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vdbe_register_subtype_propagation_and_strict_check() {
        let mut ctx = VmRuntimeContext {
            enable_test_functions: true,
            ..VmRuntimeContext::default()
        };
        let mut prog = VdbeProgram::new();
        prog.num_registers = 12;
        prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
        // r1 = 'x', r2 = 0x14A (330 -> low 8 bits = 74)
        prog.emit(Instruction::new(Opcode::String8, 0, 1, 0).with_p4(P4::Str("x".to_owned())));
        prog.emit(Instruction::new(Opcode::Integer, 0x14A, 2, 0));
        prog.emit(
            Instruction::new(Opcode::Function, 1, 0, 3).with_p4(P4::Func {
                name: "test_set_subtype".to_owned(),
                argc: 2,
            }),
        );
        // Copy r3 -> r4 (preserves subtype), Move r4 -> r5 (preserves subtype)
        prog.emit(Instruction::new(Opcode::Copy, 3, 4, 0));
        prog.emit(Instruction::new(Opcode::Move, 4, 5, 1));
        // r6 = subtype(r5), r7 = test_plain_arg_subtype(r5),
        // r8 = CAST(r5 AS TEXT) (preserves subtype per vdbemem.c:958), r9 = subtype(r8),
        // r10 = CAST(r5 AS BLOB) (clears subtype per vdbemem.c:933-935), r11 = subtype(r10)
        prog.emit(
            Instruction::new(Opcode::Function, 5, 0, 6).with_p4(P4::Func {
                name: "subtype".to_owned(),
                argc: 1,
            }),
        );
        prog.emit(
            Instruction::new(Opcode::Function, 5, 0, 7).with_p4(P4::Func {
                name: "test_plain_arg_subtype".to_owned(),
                argc: 1,
            }),
        );
        prog.emit(Instruction::new(Opcode::Cast, 5, 8, 0).with_p4(P4::Str("TEXT".to_owned())));
        prog.emit(
            Instruction::new(Opcode::Function, 8, 0, 9).with_p4(P4::Func {
                name: "subtype".to_owned(),
                argc: 1,
            }),
        );
        prog.emit(Instruction::new(Opcode::Cast, 5, 10, 0).with_p4(P4::Str("BLOB".to_owned())));
        prog.emit(
            Instruction::new(Opcode::Function, 10, 0, 11).with_p4(P4::Func {
                name: "subtype".to_owned(),
                argc: 1,
            }),
        );
        prog.emit(Instruction::new(Opcode::ResultRow, 6, 6, 0));
        prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));

        let mut bt = Btree::open_in_memory().unwrap();
        let mut svc = DefaultVmServices::new(ctx);
        let mut vm = VdbeVm::new(prog, Vec::new(), ctx);
        let step = vm.step(&mut bt, &mut svc).unwrap();
        assert_eq!(
            step,
            VmStep::Row(vec![
                Value::Integer(74),
                Value::Integer(0),
                Value::Text("x".to_owned()),
                Value::Integer(74),
                Value::Blob(b"x".to_vec()),
                Value::Integer(0),
            ])
        );
        assert_eq!(vm.register(5).unwrap().subtype, 74);
        assert!(svc.progress_ticks >= 9);

        // `test_bad_subtype` without RESULT_SUBTYPE fails under strict_subtype
        let mut bad_prog = VdbeProgram::new();
        bad_prog.num_registers = 4;
        bad_prog.emit(Instruction::new(Opcode::Init, 0, 1, 0));
        bad_prog.emit(Instruction::new(Opcode::String8, 0, 1, 0).with_p4(P4::Str("x".to_owned())));
        bad_prog.emit(Instruction::new(Opcode::Integer, 74, 2, 0));
        bad_prog.emit(
            Instruction::new(Opcode::Function, 1, 0, 3).with_p4(P4::Func {
                name: "test_bad_subtype".to_owned(),
                argc: 2,
            }),
        );
        bad_prog.emit(Instruction::new(Opcode::Halt, 0, 0, 0));
        ctx.enable_test_functions = true;
        let mut bad_vm = VdbeVm::new(bad_prog, Vec::new(), ctx);
        let err = bad_vm.step(&mut bt, &mut svc).unwrap_err();
        assert_eq!(err.code, ErrorCode::Error);
        assert_eq!(
            err.message,
            "misuse of sqlite3_result_subtype() by test_bad_subtype()"
        );
    }

    #[test]
    fn btree_access_single_btree_routes_main_and_rejects_other_slots() {
        let mut bt = Btree::open_in_memory().unwrap();
        assert!(bt.btree_ref(DatabaseId::MAIN).is_ok());
        assert!(bt.btree_mut(DatabaseId::MAIN).is_ok());
        assert!(bt.btree_ref(DatabaseId::TEMP).is_err());
        assert!(bt.btree_mut(DatabaseId(2)).is_err());
    }
}
