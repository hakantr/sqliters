//! `sqliters-vdbe`: SQLite Virtual Database Engine (VDBE) bytecode representation,
//! disassembler (`EXPLAIN`), and register-based execution machine ported from `sqlite/src/vdbe.c`.
#![forbid(unsafe_code)]

pub mod functions;

use std::cmp::Ordering;

use sqliters_btree::{Btree, CursorId};
use sqliters_types::{
    Collation, ErrorCode, KeySortOrder, Pgno, Result, SqliteError, TextEncoding, TypeAffinity,
    Value, decode_record_with_encoding, encode_record_with_encoding,
};

pub use functions::{
    AggregateAccumulator, AggregateKind, VmRuntimeContext, eval_scalar_function, format_utc_date,
    format_utc_time, format_utc_timestamp, parse_escape_char, sqlite_glob_match, sqlite_like_match,
    system_utc_millis, utc_ymdhms_from_unix_ms, validate_function_call,
};

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
    KeyInfo(Vec<KeySortOrder>),
    Func {
        name: String,
        argc: usize,
    },
    IntArray(Vec<usize>),
}

impl P4 {
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
    Affinity,
    Cast,
    Function,
    SorterOpen,
    SorterInsert,
    SorterSort,
    SorterColumn,
    SorterNext,
    Noop,
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
            Self::Affinity => "Affinity",
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

    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }
}

/// Compiled VDBE bytecode program (`Vdbe` in `vdbeInt.h`).
#[derive(Debug, Clone)]
pub struct VdbeProgram {
    pub ops: Vec<Instruction>,
    pub num_registers: usize,
    pub num_cursors: usize,
    pub column_names: Vec<String>,
    pub read_only: bool,
}

impl VdbeProgram {
    pub fn new() -> Self {
        Self {
            ops: Vec::new(),
            num_registers: 1,
            num_cursors: 0,
            column_names: Vec::new(),
            read_only: true,
        }
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
        let addr = self.ops.len();
        self.ops.push(op);
        addr
    }

    pub fn patch_p2(&mut self, addr: usize, target: usize) {
        self.ops[addr].p2 = target as i32;
    }

    pub fn current_addr(&self) -> usize {
        self.ops.len()
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

enum CursorState {
    Closed,
    Table {
        cursor_id: CursorId,
        root_pgno: Pgno,
        writable: bool,
    },
    Index {
        cursor_id: CursorId,
        root_pgno: Pgno,
        orders: Vec<KeySortOrder>,
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

/// Stateful VDBE virtual machine executing a [`VdbeProgram`] step-by-step (`sqlite3_step`).
pub struct VdbeVm {
    program: VdbeProgram,
    pc: usize,
    halted: bool,
    regs: Vec<Value>,
    cursors: Vec<CursorState>,
    params: Vec<Value>,
    pub runtime_ctx: VmRuntimeContext,
}

impl VdbeVm {
    pub fn new(program: VdbeProgram, params: Vec<Value>, runtime_ctx: VmRuntimeContext) -> Self {
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
            regs: vec![Value::Null; n_regs],
            cursors,
            params,
            runtime_ctx,
        }
    }

    #[inline]
    fn ensure_cursor_slot(&mut self, cid: usize) {
        if cid >= self.cursors.len() {
            self.cursors.resize_with(cid + 1, || CursorState::Closed);
        }
    }

    #[inline]
    fn close_cursor_slot(&mut self, cid: usize, btree: &mut Btree) {
        if cid < self.cursors.len() {
            match std::mem::replace(&mut self.cursors[cid], CursorState::Closed) {
                CursorState::Table { cursor_id, .. } | CursorState::Index { cursor_id, .. } => {
                    btree.close_cursor(cursor_id);
                }
                CursorState::Closed
                | CursorState::Ephemeral { .. }
                | CursorState::Sorter { .. } => {}
            }
        }
    }

    /// Closes all open [`Btree`] cursors owned by this VM without resetting program state.
    pub fn close_cursors(&mut self, btree: &mut Btree) {
        for cid in 0..self.cursors.len() {
            self.close_cursor_slot(cid, btree);
        }
    }

    /// Closes all open [`Btree`] cursors and resets the VM back to PC 0.
    pub fn reset_with_btree(&mut self, btree: &mut Btree) {
        self.close_cursors(btree);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.pc = 0;
        self.halted = false;
        self.regs.fill(Value::Null);
        for c in &mut self.cursors {
            *c = CursorState::Closed;
        }
    }

    pub fn column_names(&self) -> &[String] {
        &self.program.column_names
    }

    /// Executes bytecode instructions until either `OP_ResultRow` yields a row or `OP_Halt` completes execution.
    pub fn step(&mut self, btree: &mut Btree) -> Result<VmStep> {
        if self.halted {
            return Ok(VmStep::Done);
        }
        match self.step_inner(btree) {
            Ok(VmStep::Row(row)) => Ok(VmStep::Row(row)),
            Ok(VmStep::Done) => {
                self.halted = true;
                self.close_cursors(btree);
                Ok(VmStep::Done)
            }
            Err(err) => {
                self.halted = true;
                self.close_cursors(btree);
                Err(err)
            }
        }
    }

    fn step_inner(&mut self, btree: &mut Btree) -> Result<VmStep> {
        while self.pc < self.program.ops.len() {
            btree.check_interrupted()?;
            let ins = self.program.ops[self.pc].clone();
            let mut next_pc = self.pc + 1;

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
                    self.halted = true;
                    self.close_cursors(btree);
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
                    if ins.p2 != 0 {
                        btree.begin_write()?;
                    } else {
                        btree.begin_read()?;
                    }
                }
                Opcode::OpenRead | Opcode::OpenWrite => {
                    let cid = ins.p1 as usize;
                    let root_pgno = ins.p2 as Pgno;
                    let writable = ins.opcode == Opcode::OpenWrite;
                    self.ensure_cursor_slot(cid);
                    self.close_cursor_slot(cid, btree);
                    let cursor_id = btree.open_table_cursor(root_pgno, writable);
                    self.cursors[cid] = CursorState::Table {
                        cursor_id,
                        root_pgno,
                        writable,
                    };
                }
                Opcode::OpenReadIndex | Opcode::OpenWriteIndex => {
                    let cid = ins.p1 as usize;
                    let root_pgno = ins.p2 as Pgno;
                    let writable = ins.opcode == Opcode::OpenWriteIndex;
                    let orders = match &ins.p4 {
                        P4::KeyInfo(k) => k.clone(),
                        _ => Vec::new(),
                    };
                    self.ensure_cursor_slot(cid);
                    self.close_cursor_slot(cid, btree);
                    let cursor_id = btree.open_index_cursor(root_pgno, orders.clone(), writable);
                    self.cursors[cid] = CursorState::Index {
                        cursor_id,
                        root_pgno,
                        orders,
                        writable,
                        eq_prefix: None,
                    };
                }
                Opcode::OpenEphemeral => {
                    let cid = ins.p1 as usize;
                    self.ensure_cursor_slot(cid);
                    self.close_cursor_slot(cid, btree);
                    self.cursors[cid] = CursorState::Ephemeral {
                        rows: Vec::new(),
                        pos: 0,
                    };
                }
                Opcode::Close => {
                    let cid = ins.p1 as usize;
                    self.close_cursor_slot(cid, btree);
                }
                Opcode::Rewind => {
                    let cid = ins.p1 as usize;
                    let jump_empty = ins.p2 as usize;
                    match &mut self.cursors[cid] {
                        CursorState::Table { cursor_id, .. } => {
                            let id = *cursor_id;
                            if !btree.cursor_first(id)? {
                                next_pc = jump_empty;
                            }
                        }
                        CursorState::Index {
                            cursor_id,
                            eq_prefix,
                            ..
                        } => {
                            let id = *cursor_id;
                            *eq_prefix = None;
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
                    match &mut self.cursors[cid] {
                        CursorState::Table { cursor_id, .. }
                        | CursorState::Index { cursor_id, .. } => {
                            let id = *cursor_id;
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
                    let target = match &self.regs[ins.p3 as usize] {
                        Value::Integer(i) => Some(*i),
                        Value::Real(r) if (*r as i64 as f64) == *r => Some(*r as i64),
                        _ => None,
                    };
                    let Some(rowid) = target else {
                        next_pc = jump_miss;
                        self.pc = next_pc;
                        continue;
                    };
                    let cursor_id = match &self.cursors[cid] {
                        CursorState::Table { cursor_id, .. } => *cursor_id,
                        _ => return Err(SqliteError::misuse("SeekRowid on non-table cursor")),
                    };
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
                    let prefix: Vec<Value> = self.regs[start_reg..start_reg + n_keys].to_vec();
                    let cursor_id = match &mut self.cursors[cid] {
                        CursorState::Index {
                            cursor_id,
                            eq_prefix,
                            ..
                        } => {
                            *eq_prefix = Some(prefix.clone());
                            *cursor_id
                        }
                        _ => return Err(SqliteError::misuse("SeekGE on non-index cursor")),
                    };
                    if !btree.cursor_seek_index_equal_first(cursor_id, &prefix)? {
                        next_pc = jump_miss;
                    }
                }
                Opcode::IndexMatchNext => {
                    let cid = ins.p1 as usize;
                    let loop_target = ins.p2 as usize;
                    let (cursor_id, prefix) = match &self.cursors[cid] {
                        CursorState::Index {
                            cursor_id,
                            eq_prefix,
                            ..
                        } => (*cursor_id, eq_prefix.clone().unwrap_or_default()),
                        _ => return Err(SqliteError::misuse("IdxNext on non-index cursor")),
                    };
                    if btree.cursor_next_index_matching(cursor_id, &prefix)? {
                        next_pc = loop_target;
                    }
                }
                Opcode::Rowid => {
                    // p1 = cursor, p2 = dest_reg
                    let cid = ins.p1 as usize;
                    let dest = ins.p2 as usize;
                    match &self.cursors[cid] {
                        CursorState::Table { cursor_id, .. } => {
                            let rid = btree.cursor_rowid(*cursor_id)?;
                            self.regs[dest] = Value::Integer(rid);
                        }
                        CursorState::Index { cursor_id, .. } => {
                            // In SQLite secondary indexes, the table rowid is the LAST column of the index record!
                            self.regs[dest] =
                                btree.cursor_last_column(*cursor_id)?.unwrap_or(Value::Null);
                        }
                        _ => return Err(SqliteError::misuse("Rowid on unsupported cursor")),
                    }
                }
                Opcode::Column => {
                    // p1 = cursor, p2 = 0-based col_idx, p3 = dest_reg
                    let cid = ins.p1 as usize;
                    let col_idx = ins.p2 as usize;
                    let dest = ins.p3 as usize;
                    match &self.cursors[cid] {
                        CursorState::Table { cursor_id, .. }
                        | CursorState::Index { cursor_id, .. } => {
                            let val =
                                btree
                                    .cursor_column(*cursor_id, col_idx)?
                                    .unwrap_or_else(|| {
                                        // Default fallback from P4 if column was added via ALTER TABLE ADD COLUMN
                                        match &ins.p4 {
                                            P4::Int64(i) => Value::Integer(*i),
                                            P4::Real(r) => Value::real(*r),
                                            P4::Str(s) => Value::Text(s.clone()),
                                            P4::Blob(b) => Value::Blob(b.clone()),
                                            _ => Value::Null,
                                        }
                                    });
                            self.regs[dest] = val;
                        }
                        CursorState::Ephemeral { rows, pos } => {
                            self.regs[dest] =
                                rows[*pos].get(col_idx).cloned().unwrap_or(Value::Null);
                        }
                        _ => return Err(SqliteError::misuse("Column on closed cursor")),
                    }
                }
                Opcode::MakeRecord => {
                    // p1 = first_reg, p2 = n_regs, p3 = dest_reg
                    let start = ins.p1 as usize;
                    let count = ins.p2 as usize;
                    let dest = ins.p3 as usize;
                    let enc = btree.encoding();
                    let bytes = encode_record_with_encoding(&self.regs[start..start + count], enc);
                    self.regs[dest] = Value::Blob(bytes);
                }
                Opcode::NewRowid => {
                    // p1 = table cursor, p2 = dest_reg
                    let cid = ins.p1 as usize;
                    let dest = ins.p2 as usize;
                    let root_pgno = match &self.cursors[cid] {
                        CursorState::Table { root_pgno, .. } => *root_pgno,
                        _ => return Err(SqliteError::misuse("NewRowid on non-table cursor")),
                    };
                    let rid = btree.next_rowid(root_pgno)?;
                    self.regs[dest] = Value::Integer(rid);
                }
                Opcode::Insert => {
                    // p1 = table cursor, p2 = record_blob_reg, p3 = rowid_reg, p5 = update_meta_flags (1 = bump changes & last_rowid)
                    let cid = ins.p1 as usize;
                    let rec_reg = ins.p2 as usize;
                    let rid_reg = ins.p3 as usize;
                    let rowid = self.regs[rid_reg].as_i64().ok_or_else(|| {
                        SqliteError::new(ErrorCode::Mismatch, "datatype mismatch on rowid")
                    })?;
                    let payload = match &self.regs[rec_reg] {
                        Value::Blob(b) => b.clone(),
                        _ => return Err(SqliteError::misuse("OP_Insert expects record blob")),
                    };
                    let (root_pgno, writable) = match &self.cursors[cid] {
                        CursorState::Table {
                            root_pgno,
                            writable,
                            ..
                        } => (*root_pgno, *writable),
                        _ => return Err(SqliteError::misuse("Insert on non-table cursor")),
                    };
                    if !writable {
                        return Err(SqliteError::misuse("Insert on read-only table cursor"));
                    }
                    btree.insert_table(root_pgno, rowid, &payload)?;
                    if (ins.p5 & 0x01) != 0 {
                        self.runtime_ctx.last_insert_rowid = rowid;
                    }
                    if (ins.p5 & 0x02) != 0 {
                        self.runtime_ctx.last_changes += 1;
                        self.runtime_ctx.total_changes += 1;
                    }
                }
                Opcode::Delete => {
                    // p1 = table cursor, p2 = rowid_reg (if >0, else current cursor rowid)
                    let cid = ins.p1 as usize;
                    let (cursor_id, root_pgno, writable) = match &self.cursors[cid] {
                        CursorState::Table {
                            cursor_id,
                            root_pgno,
                            writable,
                        } => (*cursor_id, *root_pgno, *writable),
                        _ => return Err(SqliteError::misuse("Delete on non-table cursor")),
                    };
                    if !writable {
                        return Err(SqliteError::misuse("Delete on read-only table cursor"));
                    }
                    let rowid = if ins.p2 > 0 {
                        self.regs[ins.p2 as usize].to_integer_lossy()
                    } else {
                        btree.cursor_rowid(cursor_id)?
                    };
                    if btree.delete_table(root_pgno, rowid)? && (ins.p5 & 0x02) != 0 {
                        self.runtime_ctx.last_changes += 1;
                        self.runtime_ctx.total_changes += 1;
                    }
                }
                Opcode::IdxInsert => {
                    // p1 = index cursor, p2 = record_blob_reg, p5 = 1 if UNIQUE check enabled
                    let cid = ins.p1 as usize;
                    let payload = match &self.regs[ins.p2 as usize] {
                        Value::Blob(b) => b.clone(),
                        _ => return Err(SqliteError::misuse("IdxInsert expects record blob")),
                    };
                    let (root_pgno, orders, writable) = match &self.cursors[cid] {
                        CursorState::Index {
                            root_pgno,
                            orders,
                            writable,
                            ..
                        } => (*root_pgno, orders.clone(), *writable),
                        _ => return Err(SqliteError::misuse("IdxInsert on non-index cursor")),
                    };
                    if !writable {
                        return Err(SqliteError::misuse("IdxInsert on read-only index cursor"));
                    }
                    if ins.p5 != 0 {
                        // UNIQUE constraint check via incremental BtreeCursor (zero Vec materialization!)
                        let enc = btree.encoding();
                        let vals = decode_record_with_encoding(&payload, enc)?;
                        if vals.len() > 1 {
                            let indexed_cols = &vals[..vals.len() - 1];
                            if indexed_cols.iter().all(|v| !v.is_null()) {
                                let new_rowid = vals.last();
                                let probe_id =
                                    btree.open_index_cursor(root_pgno, orders.clone(), false);
                                let probe_res = (|| -> Result<bool> {
                                    if !btree
                                        .cursor_seek_index_equal_first(probe_id, indexed_cols)?
                                    {
                                        return Ok(false);
                                    }
                                    loop {
                                        let existing_rowid = btree.cursor_last_column(probe_id)?;
                                        if existing_rowid.as_ref() != new_rowid {
                                            return Ok(true);
                                        }
                                        if !btree
                                            .cursor_next_index_matching(probe_id, indexed_cols)?
                                        {
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
                                    return Err(SqliteError::constraint(
                                        sqliters_types::extended::SQLITE_CONSTRAINT_UNIQUE,
                                        msg,
                                    ));
                                }
                            }
                        }
                    }
                    btree.insert_index(root_pgno, &payload, &orders)?;
                }
                Opcode::IdxDelete => {
                    // p1 = index cursor, p2 = start_reg, p3 = n_fields
                    let cid = ins.p1 as usize;
                    let start = ins.p2 as usize;
                    let count = ins.p3 as usize;
                    let keys = self.regs[start..start + count].to_vec();
                    let (root_pgno, orders, writable) = match &self.cursors[cid] {
                        CursorState::Index {
                            root_pgno,
                            orders,
                            writable,
                            ..
                        } => (*root_pgno, orders.clone(), *writable),
                        _ => return Err(SqliteError::misuse("IdxDelete on non-index cursor")),
                    };
                    if !writable {
                        return Err(SqliteError::misuse("IdxDelete on read-only index cursor"));
                    }
                    btree.delete_index(root_pgno, &keys, &orders)?;
                }
                Opcode::ResultRow => {
                    let start = ins.p1 as usize;
                    let count = ins.p2 as usize;
                    let row = self.regs[start..start + count].to_vec();
                    self.pc = next_pc;
                    return Ok(VmStep::Row(row));
                }
                Opcode::Integer => {
                    self.regs[ins.p2 as usize] = Value::Integer(i64::from(ins.p1));
                }
                Opcode::Int64 => {
                    let P4::Int64(v) = ins.p4 else {
                        return Err(SqliteError::misuse("Int64 missing P4::Int64"));
                    };
                    self.regs[ins.p2 as usize] = Value::Integer(v);
                }
                Opcode::Real => {
                    let P4::Real(v) = ins.p4 else {
                        return Err(SqliteError::misuse("Real missing P4::Real"));
                    };
                    self.regs[ins.p2 as usize] = Value::real(v);
                }
                Opcode::String8 => {
                    let P4::Str(ref s) = ins.p4 else {
                        return Err(SqliteError::misuse("String8 missing P4::Str"));
                    };
                    self.regs[ins.p2 as usize] = Value::Text(s.clone());
                }
                Opcode::Blob => {
                    let P4::Blob(ref b) = ins.p4 else {
                        return Err(SqliteError::misuse("Blob missing P4::Blob"));
                    };
                    self.regs[ins.p2 as usize] = Value::Blob(b.clone());
                }
                Opcode::Null => {
                    let start = ins.p2 as usize;
                    let end = if ins.p3 > ins.p2 {
                        ins.p3 as usize
                    } else {
                        start
                    };
                    for r in start..=end {
                        self.regs[r] = Value::Null;
                    }
                }
                Opcode::Variable => {
                    // p1 = 1-based parameter index, p2 = dest_reg
                    let idx = (ins.p1 as usize).saturating_sub(1);
                    self.regs[ins.p2 as usize] =
                        self.params.get(idx).cloned().unwrap_or(Value::Null);
                }
                Opcode::Copy => {
                    let src = ins.p1 as usize;
                    let dst = ins.p2 as usize;
                    let extra = ins.p3.max(0) as usize;
                    for i in 0..=extra {
                        self.regs[dst + i] = self.regs[src + i].clone();
                    }
                }
                Opcode::Move => {
                    let src = ins.p1 as usize;
                    let dst = ins.p2 as usize;
                    let count = ins.p3.max(1) as usize;
                    for i in 0..count {
                        let v = std::mem::replace(&mut self.regs[src + i], Value::Null);
                        self.regs[dst + i] = v;
                    }
                }
                Opcode::Add
                | Opcode::Subtract
                | Opcode::Multiply
                | Opcode::Divide
                | Opcode::Remainder => {
                    // SQLite convention: P3 = P2 <op> P1 (or we use standard P1 = lhs, P2 = rhs, P3 = dest)
                    // Here: p1 = lhs_reg, p2 = rhs_reg, p3 = dest_reg.
                    let lhs = &self.regs[ins.p1 as usize];
                    let rhs = &self.regs[ins.p2 as usize];
                    self.regs[ins.p3 as usize] = eval_arith(ins.opcode, lhs, rhs);
                }
                Opcode::BitAnd | Opcode::BitOr | Opcode::ShiftLeft | Opcode::ShiftRight => {
                    let lhs = &self.regs[ins.p1 as usize];
                    let rhs = &self.regs[ins.p2 as usize];
                    if lhs.is_null() || rhs.is_null() {
                        self.regs[ins.p3 as usize] = Value::Null;
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
                        self.regs[ins.p3 as usize] = Value::Integer(res);
                    }
                }
                Opcode::BitNot => {
                    let v = &self.regs[ins.p1 as usize];
                    self.regs[ins.p2 as usize] = if v.is_null() {
                        Value::Null
                    } else {
                        Value::Integer(!v.to_integer_lossy())
                    };
                }
                Opcode::Negate => {
                    let v = &self.regs[ins.p1 as usize];
                    self.regs[ins.p2 as usize] = eval_unary_negate(v);
                }
                Opcode::Concat => {
                    let lhs = &self.regs[ins.p1 as usize];
                    let rhs = &self.regs[ins.p2 as usize];
                    self.regs[ins.p3 as usize] = if lhs.is_null() || rhs.is_null() {
                        Value::Null
                    } else {
                        let mut s = lhs.to_text_lossy();
                        s.push_str(&rhs.to_text_lossy());
                        Value::Text(s)
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
                    let coll = match ins.p4 {
                        P4::Collation(c) => c,
                        _ => Collation::Binary,
                    };
                    let aff = p5_to_affinity(ins.p5);
                    let lhs = &self.regs[ins.p1 as usize];
                    let rhs = &self.regs[ins.p2 as usize];
                    self.regs[ins.p3 as usize] = eval_comparison_with_affinity(
                        ins.opcode,
                        lhs,
                        rhs,
                        aff,
                        coll,
                        btree.encoding(),
                    );
                }
                Opcode::And => {
                    let a = self.regs[ins.p1 as usize].truthiness();
                    let b = self.regs[ins.p2 as usize].truthiness();
                    self.regs[ins.p3 as usize] = match (a, b) {
                        (Some(false), _) | (_, Some(false)) => Value::Integer(0),
                        (Some(true), Some(true)) => Value::Integer(1),
                        _ => Value::Null,
                    };
                }
                Opcode::Or => {
                    let a = self.regs[ins.p1 as usize].truthiness();
                    let b = self.regs[ins.p2 as usize].truthiness();
                    self.regs[ins.p3 as usize] = match (a, b) {
                        (Some(true), _) | (_, Some(true)) => Value::Integer(1),
                        (Some(false), Some(false)) => Value::Integer(0),
                        _ => Value::Null,
                    };
                }
                Opcode::Not => {
                    let a = self.regs[ins.p1 as usize].truthiness();
                    self.regs[ins.p2 as usize] = match a {
                        None => Value::Null,
                        Some(b) => Value::Integer(i64::from(!b)),
                    };
                }
                Opcode::IsNull => {
                    // p1 = reg; if p2 >= 0 and p3 == 0: jump mode; if p3 > 0: store 0/1 in reg p3
                    let is_n = self.regs[ins.p1 as usize].is_null();
                    if ins.p3 > 0 {
                        self.regs[ins.p3 as usize] = Value::Integer(i64::from(is_n));
                    } else if is_n {
                        next_pc = ins.p2 as usize;
                    }
                }
                Opcode::NotNull => {
                    let not_n = !self.regs[ins.p1 as usize].is_null();
                    if ins.p3 > 0 {
                        self.regs[ins.p3 as usize] = Value::Integer(i64::from(not_n));
                    } else if not_n {
                        next_pc = ins.p2 as usize;
                    }
                }
                Opcode::JumpIfTrue => {
                    if self.regs[ins.p1 as usize].truthiness() == Some(true) {
                        next_pc = ins.p2 as usize;
                    }
                }
                Opcode::JumpIfFalse => {
                    // Jump when false, or when NULL if p3 != 0 (`jumpIfNull` flag in SQLite `OP_IfNot`!)
                    match self.regs[ins.p1 as usize].truthiness() {
                        Some(false) => next_pc = ins.p2 as usize,
                        None if ins.p3 != 0 => next_pc = ins.p2 as usize,
                        _ => {}
                    }
                }
                Opcode::JumpIfNull => {
                    if self.regs[ins.p1 as usize].is_null() {
                        next_pc = ins.p2 as usize;
                    }
                }
                Opcode::Affinity => {
                    // p1 = start_reg, p2 = count, p4 = Affinity(vec)
                    let start = ins.p1 as usize;
                    if let P4::Affinity(ref affs) = ins.p4 {
                        for (i, &aff) in affs.iter().enumerate() {
                            let v = self.regs[start + i].clone();
                            self.regs[start + i] = aff.apply(v);
                        }
                    }
                }
                Opcode::Cast => {
                    // p1 = src_reg, p2 = dest_reg, p4 = Str(type_name)
                    let src = self.regs[ins.p1 as usize].clone();
                    let type_name = match &ins.p4 {
                        P4::Str(s) => s.as_str(),
                        _ => "",
                    };
                    self.regs[ins.p2 as usize] =
                        eval_cast_with_encoding(src, type_name, btree.encoding());
                }
                Opcode::Function => {
                    // p1 = first_arg_reg, p3 = dest_reg, p4 = Func { name, argc }
                    let start = ins.p1 as usize;
                    let dest = ins.p3 as usize;
                    let P4::Func { ref name, argc } = ins.p4 else {
                        return Err(SqliteError::misuse("Function missing P4::Func"));
                    };
                    let args = &self.regs[start..start + argc];
                    self.regs[dest] = eval_scalar_function(name, args, &self.runtime_ctx)?;
                }
                Opcode::SorterOpen => {
                    // p1 = cursor, p4 = KeyInfo(orders), IntArray encoded via p2.. or KeyInfo
                    let cid = ins.p1 as usize;
                    let orders = match &ins.p4 {
                        P4::KeyInfo(k) => {
                            k.iter().enumerate().map(|(idx, &ord)| (idx, ord)).collect()
                        }
                        _ => Vec::new(),
                    };
                    self.ensure_cursor_slot(cid);
                    self.close_cursor_slot(cid, btree);
                    self.cursors[cid] = CursorState::Sorter {
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
                    let row = self.regs[start..start + count].to_vec();
                    if let CursorState::Sorter { rows, .. } = &mut self.cursors[cid] {
                        rows.push(row);
                    } else {
                        return Err(SqliteError::misuse("SorterInsert on non-sorter cursor"));
                    }
                }
                Opcode::SorterSort => {
                    let cid = ins.p1 as usize;
                    let jump_empty = ins.p2 as usize;
                    if let CursorState::Sorter { orders, rows, pos } = &mut self.cursors[cid] {
                        if rows.is_empty() {
                            next_pc = jump_empty;
                        } else {
                            let specs = orders.clone();
                            let enc = btree.encoding();
                            rows.sort_by(|a, b| compare_sorter_rows(a, b, &specs, enc));
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
                    if let CursorState::Sorter { rows, pos, .. } = &self.cursors[cid] {
                        self.regs[dest] = rows[*pos].get(col_idx).cloned().unwrap_or(Value::Null);
                    } else {
                        return Err(SqliteError::misuse("SorterColumn on non-sorter cursor"));
                    }
                }
                Opcode::SorterNext => {
                    let cid = ins.p1 as usize;
                    let loop_target = ins.p2 as usize;
                    if let CursorState::Sorter { rows, pos, .. } = &mut self.cursors[cid] {
                        *pos += 1;
                        if *pos < rows.len() {
                            next_pc = loop_target;
                        }
                    } else {
                        return Err(SqliteError::misuse("SorterNext on non-sorter cursor"));
                    }
                }
            }

            self.pc = next_pc;
        }

        self.halted = true;
        Ok(VmStep::Done)
    }
}

fn eval_arith(op: Opcode, lhs: &Value, rhs: &Value) -> Value {
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
            Value::Blob(b) => {
                let s = match enc {
                    TextEncoding::Utf8 => {
                        let slice = b
                            .iter()
                            .position(|&x| x == 0)
                            .map_or(b.as_slice(), |p| &b[..p]);
                        String::from_utf8_lossy(slice).into_owned()
                    }
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
                };
                Value::Text(s)
            }
            other => Value::Text(other.to_text_lossy()),
        },
        TypeAffinity::Integer => Value::Integer(val.to_integer_lossy()),
        TypeAffinity::Real => Value::real(val.to_real_lossy()),
        TypeAffinity::Numeric => {
            if let Value::Text(ref s) = val {
                if let Some(n) = sqliters_types::parse_numeric_text(s) {
                    return n;
                }
                let f = val.to_real_lossy();
                let i = val.to_integer_lossy();
                if (i as f64) == f {
                    Value::Integer(i)
                } else {
                    Value::real(f)
                }
            } else {
                val
            }
        }
    }
}

fn compare_sorter_rows(
    a: &[Value],
    b: &[Value],
    specs: &[(usize, KeySortOrder)],
    enc: TextEncoding,
) -> Ordering {
    for &(col_idx, spec) in specs {
        let va = &a[col_idx];
        let vb = &b[col_idx];
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
                let raw = va.cmp_sqlite_with_encoding(vb, spec.collation, enc);
                if spec.desc { raw.reverse() } else { raw }
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}
