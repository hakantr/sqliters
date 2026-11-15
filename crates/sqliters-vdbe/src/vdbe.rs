//! `vdbe.c`: the virtual machine — `sqlite3VdbeExec` — that runs the programs of C's code
//! generator (crate `sqliters-codegen`), with the run-time half of `vdbeaux.c` that belongs
//! to a running statement (`allocateCursor`, `sqlite3VdbeFreeCursor`, `closeAllCursors`,
//! `sqlite3VdbeFinishMoveto`, `sqlite3VdbeHandleMovedCursor`, `sqlite3VdbeCursorRestore`,
//! the read-statement part of `sqlite3VdbeHalt`) and the function-call context
//! (`sqlite3_context`).
//!
//! Port of SQLite 3.54.1. C's names, C's order of operations: every `case OP_X` of
//! `sqlite3VdbeExec` is one arm of the `match` in [`Vdbe::exec`], with C's labels
//! (`jump_to_p2`, `check_for_interrupt`, `abort_due_to_error`, `too_big`) as macros.
//!
//! The table and index cursors are C's `BtCursor` (`sqliters_btree::btcursor`): one
//! traversal finds the data and holds the page references C holds. Ephemeral tables and
//! indexes are real b-trees on private pagers (`OP_OpenEphemeral`).
//!
//! What only the engine knows — the b-trees of the connection, the schema cookie, the
//! function and collation registries, interrupt and limits — comes through [`VdbeHost`].
//!
//! Scope of this step (M1): the opcodes of read statements. Opcodes that are not ported are
//! listed by [`unsupported_reason`]; a program that contains one is not run by the machine.

#![allow(unexpected_cfgs, non_upper_case_globals, dead_code)]

use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_btree::btcursor::{BTREE_AUXDELETE, BTREE_SEEK_EQ, BcParked, BtCursor, CURSOR_REQUIRESEEK};
use sqliters_types::{ConflictResolutionEffect, ErrorCode, KeySortOrder, Pgno, Result, RuntimeValue, SqliteError, Value};

use crate::opcodes::*;
use crate::vdbeaux::{CollSeq, FuncDef, KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC, KeyInfo, P4, Program, VdbeOp, int32_to_64};
use crate::vdbeaux_rt::{
    KeyInfoRt, UnpackedRecord, corrupt, get_varint32, put_varint, sqlite3_varint_len, sqlite3_vdbe_idx_key_compare,
    sqlite3_vdbe_idx_rowid, sqlite3_vdbe_mem_from_btree, sqlite3_vdbe_mem_from_btree_zero_offset,
    sqlite3_vdbe_record_unpack, sqlite3_vdbe_serial_get,
    sqlite3_vdbe_serial_type_len,
};
use crate::vdbemem::*;
use crate::vdbesort::{RowSet, VdbeSorter};

pub const CURTYPE_BTREE: u8 = 0;
pub const CURTYPE_SORTER: u8 = 1;
pub const CURTYPE_VTAB: u8 = 2;
pub const CURTYPE_PSEUDO: u8 = 3;

/// `CACHE_STALE`
const CACHE_STALE: u32 = 0;

const SQLITE_JUMPIFNULL: u16 = 0x10;
const SQLITE_NULLEQ: u16 = 0x80;
const OPFLAG_PERMUTE: u16 = 0x01;
const OPFLAG_SEEKEQ: u16 = 0x02;
const OPFLAG_BULKCSR: u16 = 0x01;
const OPFLAG_USESEEKRESULT: u16 = 0x10;
const OPFLAG_NCHANGE: u16 = 0x01;
const OPFLAG_LASTROWID: u16 = 0x20;
const OPFLAG_APPEND: u16 = 0x08;
const OPFLAG_SAVEPOSITION: u16 = 0x02;
const OPFLAG_PREFORMAT: u16 = 0x80;
const OPFLAG_P2ISREG: u16 = 0x10;
/// `OE_Rollback`, `OE_Fail`: `OP_Halt.p2` (`Vdbe.errorAction`)
const OE_ROLLBACK: i32 = 1;
const OE_FAIL: i32 = 3;
const OPFLAG_LENGTHARG: u16 = 0x40;
const OPFLAG_TYPEOFARG: u16 = 0x80;
const OPFLAG_BYTELENARG: u16 = 0xc0;
const BTREE_UNORDERED: u16 = 8;
const SQLITE_FUNC_NEEDCOLL: u32 = 0x0020;

/// `SQLITE_STMTSTATUS_*` (`Vdbe.aCounter[]`)
pub const SQLITE_STMTSTATUS_FULLSCAN_STEP: usize = 1;
pub const SQLITE_STMTSTATUS_SORT: usize = 2;
pub const SQLITE_STMTSTATUS_AUTOINDEX: usize = 3;
pub const SQLITE_STMTSTATUS_VM_STEP: usize = 4;
pub const SQLITE_STMTSTATUS_RUN: usize = 6;
pub const SQLITE_STMTSTATUS_FILTER_MISS: usize = 7;
pub const SQLITE_STMTSTATUS_FILTER_HIT: usize = 8;

/// What the machine needs of the connection (`sqlite3 *db`).
pub trait VdbeHost {
    /// `db->aDb[iDb].pBt`
    fn btree(&mut self, i_db: usize) -> Option<&mut Btree>;
    /// `OP_Transaction` on a read statement: `sqlite3BtreeBeginTrans(pBt, 0, &iMeta)`;
    /// returns `(iMeta, pDb->pSchema->iGeneration)`.
    fn begin_read_trans(&mut self, i_db: usize) -> Result<(u32, i32)>;
    /// `sqlite3BtreeGetMeta(db->aDb[iDb].pBt, iCookie, &iMeta)`
    fn get_meta(&mut self, i_db: usize, i_cookie: i32) -> Result<u32>;
    /// The collating sequence `name` of the connection (the three built-ins are bound by the
    /// machine itself).
    fn find_collation(&mut self, name: &str) -> Option<CollRt>;
    /// `pFunc->xSFunc(pCtx, argc, argv)` of a scalar function.
    fn call_function(&mut self, func: &FuncDef, args: &[RuntimeValue]) -> Result<RuntimeValue>;
    /// A fresh accumulator (`sqlite3_aggregate_context`) of aggregate function `func` called
    /// with `n_arg` arguments: its handle for the four calls below.
    fn agg_open(&mut self, func: &FuncDef, n_arg: usize) -> Result<usize>;
    /// `pFunc->xSFunc` (`xStep`) of an aggregate
    fn agg_step(&mut self, id: usize, args: &[RuntimeValue]) -> Result<()>;
    /// `pFunc->xInverse`
    fn agg_inverse(&mut self, id: usize, args: &[RuntimeValue]) -> Result<()>;
    /// `pFunc->xValue`
    fn agg_value(&mut self, id: usize) -> Result<RuntimeValue>;
    /// `pFunc->xFinalize`: the accumulator is released
    fn agg_finalize(&mut self, id: usize) -> Result<RuntimeValue>;
    /// `sqlite3BtreeOpen(db->pVfs, 0, db, &pBtx, BTREE_OMIT_JOURNAL | BTREE_SINGLE | p5, ..)`
    /// followed by `sqlite3BtreeBeginTrans(pBtx, 1, 0)`.
    fn open_ephemeral(&mut self) -> Result<Btree>;
    /// `db->aLimit[SQLITE_LIMIT_LENGTH]`
    fn limit_length(&self) -> i32;
    /// `AtomicLoad(&db->u1.isInterrupted)`: `Err(SQLITE_INTERRUPT)` when set.
    fn check_interrupt(&mut self) -> Result<()>;
    /// `db->nProgressOps` when a progress handler is installed (`db->xProgress != 0`), else 0.
    fn progress_ops(&self) -> u64;
    /// `db->xProgress(db->pProgressArg)`: `true` asks for an interrupt.
    fn progress_invoke(&mut self) -> Result<bool>;
    /// `(page size of the main database, db->aDb[0].pSchema->cache_size, sqlite3TempInMemory(db))`
    fn sorter_config(&mut self) -> (i64, i64, bool);
    /// `OP_Transaction` with `P2!=0`: `sqlite3BtreeBeginTrans(pBt, p2, &iMeta)` and, when the
    /// program uses a statement journal and the connection is in a transaction or has other
    /// active statements, `sqlite3BtreeBeginStmt`. Returns `(iMeta, pSchema->iGeneration)`.
    fn begin_write_trans(&mut self, _i_db: usize, _p2: i32) -> Result<(u32, i32)> {
        Err(internal("write transaction on a host that only reads"))
    }
    /// `db->lastRowid = iKey` (`OP_Insert` with `OPFLAG_LASTROWID`)
    fn set_last_rowid(&mut self, _rowid: i64) {}
    /// `sqlite3VdbeSetChanges(db, nChange)`: `db->nChange = n; db->nTotalChange += n`
    fn set_changes(&mut self, _n: i64) {}
    /// `sqlite3_randomness(sizeof(v), &v)` of `OP_NewRowid`
    fn randomness(&mut self) -> i64 {
        0
    }
    /// `sqlite3WritableSchema(db)`
    fn writable_schema(&self) -> bool {
        false
    }
    /// The `Table` a `P4_TABLE` operand names (`OP_TypeCheck`).
    fn table_info(&mut self, _name: &str) -> Option<Arc<TableRt>> {
        None
    }
}

/// `COLTYPE_*`: `Column.eCType`.
pub const COLTYPE_CUSTOM: u8 = 0;
pub const COLTYPE_ANY: u8 = 1;
pub const COLTYPE_BLOB: u8 = 2;
pub const COLTYPE_INT: u8 = 3;
pub const COLTYPE_INTEGER: u8 = 4;
pub const COLTYPE_REAL: u8 = 5;
pub const COLTYPE_TEXT: u8 = 6;

/// What the machine reads of a `Column` of a `Table`.
#[derive(Debug, Clone)]
pub struct TableColRt {
    /// `zCnName`
    pub name: String,
    /// `affinity` (`SQLITE_AFF_*`)
    pub affinity: u8,
    /// `eCType` (`COLTYPE_*`)
    pub e_ctype: u8,
    /// `colFlags & COLFLAG_GENERATED`
    pub generated: bool,
    /// `colFlags & COLFLAG_VIRTUAL`
    pub is_virtual: bool,
}

/// What the machine reads of a `Table` (`P4_TABLE`).
#[derive(Debug, Clone)]
pub struct TableRt {
    /// `zName`
    pub name: String,
    /// `aCol[]`
    pub cols: Vec<TableColRt>,
}

/// `VdbeTxtBlbCache`: the last large value read from overflow pages (`vdbeColumnFromOverflow`).
#[derive(Debug)]
struct VdbeTxtBlbCache {
    p_c_value: Vec<u8>,
    i_offset: i64,
    i_col: u32,
    cache_status: u32,
    col_cache_ctr: u32,
}

/// `struct VdbeCursor`.
#[derive(Debug)]
pub struct VdbeCursor {
    /// `eCurType`
    pub e_cur_type: u8,
    /// `iDb`
    pub i_db: i8,
    /// `nullRow`
    pub null_row: bool,
    /// `deferredMoveto`
    pub deferred_moveto: bool,
    /// `isTable`
    pub is_table: bool,
    /// `isEphemeral`
    pub is_ephemeral: bool,
    /// `useRandomRowid`
    pub use_random_rowid: bool,
    /// `isOrdered`
    pub is_ordered: bool,
    /// `noReuse`
    pub no_reuse: bool,
    /// `seekHit`
    pub seek_hit: u16,
    /// `ub.aAltMap`: address of the `OP_DeferredSeek` whose `P4_INTARRAY` maps table columns
    /// to index columns
    alt_map_op: Option<usize>,
    /// `ub.pBtx`: the ephemeral b-tree (slot in `Vdbe.ephem`)
    btx: Option<usize>,
    /// `seqCount`
    pub seq_count: i64,
    /// `cacheStatus`
    pub cache_status: u32,
    /// `seekResult`
    pub seek_result: i32,
    /// `pAltCursor`
    p_alt_cursor: Option<usize>,
    /// `uc.pCursor`
    pub cur: Box<BtCursor>,
    /// While the statement is suspended between two steps the cursor is in the cursor list
    /// of its b-tree (`Btree::bc_park`).
    parked: Option<BcParked>,
    /// `uc.pSorter`
    sorter: Option<Box<VdbeSorter>>,
    /// `pKeyInfo`
    key_info: Option<Arc<KeyInfoRt>>,
    /// The key order as the b-tree layer's index insert takes it (ephemeral indexes).
    orders: Vec<KeySortOrder>,
    /// `iHdrOffset`
    i_hdr_offset: u32,
    /// `pgnoRoot`
    pub pgno_root: Pgno,
    /// `nField`
    pub n_field: i32,
    /// `nHdrParsed`
    n_hdr_parsed: u32,
    /// `movetoTarget`
    pub moveto_target: i64,
    /// `aOffset[]`, `aType[]`
    a_offset: Vec<u32>,
    a_type: Vec<u32>,
    /// `aRow` (a copy of the part of the record that is on the b-tree page, or of the
    /// register of a pseudo-cursor), `aRow!=0`, `payloadSize`, `szRow`
    a_row: Vec<u8>,
    a_row_ok: bool,
    payload_size: u32,
    sz_row: u32,
    /// `sMem` of `OP_Column`: the record header when it is not all on the page
    hdr_buf: Vec<u8>,
    /// `pCache`
    p_cache: Option<Box<VdbeTxtBlbCache>>,
}

/// An ephemeral b-tree (`VdbeCursor.ub.pBtx`) with the number of cursors open on it
/// (`BTREE_SINGLE`: closed with its last cursor).
struct EphemBt {
    bt: Btree,
    n_ref: u32,
}

/// The accumulator of an aggregate function (`Mem.z` of a `MEM_Agg` cell).
enum AggCtx {
    /// `minmaxStep`: the best value so far
    MinMax { best: Option<Mem>, is_max: bool, coll: Option<Arc<CollRt>> },
    /// An aggregate implemented by the engine
    Host(Option<usize>),
}

/// P4 operands bound to the connection when the program is made ready.
#[derive(Clone, Default)]
enum OpRt {
    #[default]
    None,
    Coll(Arc<CollRt>),
    KeyInfo(Arc<KeyInfoRt>),
}

/// A running statement (`struct Vdbe` in `VDBE_RUN_STATE`).
pub struct Vdbe {
    /// The program (`aOp`)
    pub prog: Arc<Program>,
    /// `aMem[]`
    pub a_mem: Vec<Mem>,
    /// `apCsr[]`
    ap_csr: Vec<Option<Box<VdbeCursor>>>,
    /// `aVar[]`
    pub a_var: Vec<Mem>,
    /// `pc`
    pub pc: i32,
    /// `cacheCtr`
    cache_ctr: u32,
    /// `pResultRow`: first register of the row `OP_ResultRow` produced, and its size
    result_row: (usize, usize),
    /// `aOp[0].p1` (run counter of `OP_Init`) and the `p1` of every `OP_Once`
    init_p1: i32,
    once: Vec<i32>,
    /// `aCounter[]`
    pub a_counter: [u32; 9],
    /// Ephemeral b-trees
    ephem: Vec<Option<EphemBt>>,
    /// Aggregate accumulators and row sets, by register
    a_agg: Vec<Option<AggCtx>>,
    a_rowset: Vec<Option<Box<RowSet>>>,
    /// P4 operands bound to the connection, by address
    rt: Vec<OpRt>,
    rt_ready: bool,
    /// Scratch buffers
    scratch: Vec<u8>,
    u_temp: Vec<u32>,
    /// `minWriteFileFormat`
    min_write_file_format: u8,
    /// The statement has halted (`VDBE_HALT_STATE`).
    pub halted: bool,
    /// `nChange`: rows changed by the statement so far (`OPFLAG_NCHANGE`)
    pub n_change: i64,
    /// `expired==1`: `sqlite3ExpirePreparedStatements(db, 0)` ran while the statement was
    /// running.
    pub expired: bool,
    /// For the host: its expiry sequence number when the execution started.
    pub expire_seq: u64,
    /// Executed instructions (`nVmStep`)
    pub n_vm_step: u64,
    /// `nVmStep`, `nProgressLimit` and `db->nProgressOps` of the running `sqlite3VdbeExec` call
    call_steps: u64,
    progress_limit: u64,
    progress_ops: u64,
    /// Opcode trace: address of the instruction being executed
    tr_pc: usize,
    /// Address of the instruction being executed (kept for error reports)
    err_pc: usize,
}

#[cold]
fn internal(msg: &str) -> SqliteError {
    SqliteError::new(ErrorCode::Internal, format!("vdbe: {msg}"))
}

#[cold]
fn too_big() -> SqliteError {
    SqliteError::new(ErrorCode::TooBig, "string or blob too big")
}

/// `sqlite3LogEst`
fn sqlite3_log_est(mut x: u64) -> i16 {
    const A: [i16; 8] = [0, 2, 3, 5, 6, 7, 8, 9];
    let mut y: i16 = 40;
    if x < 8 {
        if x < 2 {
            return 0;
        }
        while x < 8 {
            y -= 10;
            x <<= 1;
        }
    } else {
        let i = 60 - x.leading_zeros() as i16;
        y += i * 10;
        x >>= i;
    }
    A[(x & 7) as usize] + y - 10
}

/// The reason why the machine does not run `prog` (`None`: every opcode is ported).
pub fn unsupported_reason(prog: &Program) -> Option<String> {
    unsupported(prog, false)
}

/// [`unsupported_reason`] for a program that writes (`OP_Transaction` with `P2!=0`, the
/// write opcodes).
pub fn unsupported_reason_write(prog: &Program) -> Option<String> {
    unsupported(prog, true)
}

fn unsupported(prog: &Program, write: bool) -> Option<String> {
    if !prog.p_program.is_empty() {
        return Some("sub-programs".to_owned());
    }
    for op in &prog.a_op {
        let ok = matches!(
            op.opcode,
            OP_Init
                | OP_Goto
                | OP_Gosub
                | OP_Return
                | OP_InitCoroutine
                | OP_EndCoroutine
                | OP_Yield
                | OP_Halt
                | OP_HaltIfNull
                | OP_Integer
                | OP_Int64
                | OP_Real
                | OP_String8
                | OP_String
                | OP_Null
                | OP_BeginSubrtn
                | OP_SoftNull
                | OP_Blob
                | OP_Variable
                | OP_Move
                | OP_Copy
                | OP_SCopy
                | OP_IntCopy
                | OP_ResultRow
                | OP_Concat
                | OP_Add
                | OP_Subtract
                | OP_Multiply
                | OP_Divide
                | OP_Remainder
                | OP_CollSeq
                | OP_BitAnd
                | OP_BitOr
                | OP_ShiftLeft
                | OP_ShiftRight
                | OP_AddImm
                | OP_MustBeInt
                | OP_RealAffinity
                | OP_Cast
                | OP_Eq
                | OP_Ne
                | OP_Lt
                | OP_Le
                | OP_Gt
                | OP_Ge
                | OP_ElseEq
                | OP_Permutation
                | OP_Compare
                | OP_Jump
                | OP_And
                | OP_Or
                | OP_IsTrue
                | OP_Not
                | OP_BitNot
                | OP_Once
                | OP_If
                | OP_IfNot
                | OP_IsNull
                | OP_IsType
                | OP_ZeroOrNull
                | OP_NotNull
                | OP_IfNullRow
                | OP_Offset
                | OP_Column
                | OP_Affinity
                | OP_MakeRecord
                | OP_Count
                | OP_Transaction
                | OP_ReadCookie
                | OP_ReopenIdx
                | OP_OpenRead
                | OP_OpenDup
                | OP_OpenAutoindex
                | OP_OpenEphemeral
                | OP_SorterOpen
                | OP_SequenceTest
                | OP_OpenPseudo
                | OP_Close
                | OP_ColumnsUsed
                | OP_SeekLT
                | OP_SeekLE
                | OP_SeekGE
                | OP_SeekGT
                | OP_SeekScan
                | OP_SeekHit
                | OP_IfNoHope
                | OP_NoConflict
                | OP_NotFound
                | OP_Found
                | OP_SeekRowid
                | OP_NotExists
                | OP_Sequence
                | OP_NewRowid
                | OP_Insert
                | OP_Delete
                | OP_SorterCompare
                | OP_SorterData
                | OP_RowData
                | OP_Rowid
                | OP_NullRow
                | OP_SeekEnd
                | OP_Last
                | OP_IfSizeBetween
                | OP_SorterSort
                | OP_Sort
                | OP_Rewind
                | OP_IfEmpty
                | OP_SorterNext
                | OP_Prev
                | OP_Next
                | OP_IdxInsert
                | OP_SorterInsert
                | OP_DeferredSeek
                | OP_IdxRowid
                | OP_FinishSeek
                | OP_IdxLE
                | OP_IdxGT
                | OP_IdxLT
                | OP_IdxGE
                | OP_ResetSorter
                | OP_RowSetAdd
                | OP_RowSetRead
                | OP_RowSetTest
                | OP_MemMax
                | OP_IfPos
                | OP_OffsetLimit
                | OP_IfNotZero
                | OP_DecrJumpZero
                | OP_AggInverse
                | OP_AggStep
                | OP_AggStep1
                | OP_AggValue
                | OP_AggFinal
                | OP_PureFunc
                | OP_Function
                | OP_ClrSubtype
                | OP_GetSubtype
                | OP_SetSubtype
                | OP_FilterAdd
                | OP_Filter
                | OP_Trace
                | OP_Explain
                | OP_Noop
                | OP_TableLock
                | OP_CursorHint
                | OP_ReleaseReg
                | OP_Abortable
        ) || (write && matches!(op.opcode, OP_OpenWrite | OP_IdxDelete | OP_Clear | OP_ResetCount | OP_TypeCheck));
        if !ok {
            return Some(format!("opcode {}", sqlite3_opcode_name(op.opcode)));
        }
        match op.opcode {
            OP_Transaction if op.p2 != 0 && !write => return Some("write transaction".to_owned()),
            OP_Insert | OP_IdxInsert if op.p5 & OPFLAG_PREFORMAT != 0 => return Some("OPFLAG_PREFORMAT".to_owned()),
            OP_Function | OP_PureFunc => {
                if let P4::FuncCtx(c) = &op.p4
                    && c.p_func.func_flags & SQLITE_FUNC_NEEDCOLL != 0
                    && !matches!(c.p_func.name.to_ascii_lowercase().as_str(), "min" | "max" | "nullif")
                {
                    return Some(format!("function {} needs a collation", c.p_func.name));
                }
            }
            OP_AggStep | OP_AggInverse | OP_AggFinal | OP_AggValue => {
                if let P4::FuncDef(f) = &op.p4
                    && f.func_flags & SQLITE_FUNC_NEEDCOLL != 0
                    && !matches!(f.name.to_ascii_lowercase().as_str(), "min" | "max")
                {
                    return Some(format!("aggregate {} needs a collation", f.name));
                }
            }
            _ => {}
        }
        if let P4::KeyInfo(k) = &op.p4 {
            if k.enc != 1 {
                return Some("UTF-16 key".to_owned());
            }
            // An application-defined collation can fail in this engine (its callback returns a
            // `Result`); the machine has no path for that error yet.
            if let Some(c) = k.a_coll.iter().flatten().find(|c| builtin_coll(&c.name).is_none()) {
                return Some(format!("application-defined collation {}", c.name));
            }
        }
        if let P4::CollSeq(c) = &op.p4
            && builtin_coll(&c.name).is_none()
        {
            return Some(format!("application-defined collation {}", c.name));
        }
    }
    None
}

/// `sqlite3VdbeFinishMoveto`
fn finish_moveto(bt: &mut Btree, pc: &mut VdbeCursor) -> Result<()> {
    let res = bt.bc_table_moveto(&mut pc.cur, pc.moveto_target, false)?;
    if res != 0 {
        return Err(corrupt());
    }
    pc.deferred_moveto = false;
    pc.cache_status = CACHE_STALE;
    Ok(())
}

/// `btreeRestoreCursorPosition` for a cursor of the machine: an index cursor in
/// `CURSOR_REQUIRESEEK` is sought with its saved key (`btreeMoveto`).
fn restore_cursor_position(bt: &mut Btree, pc: &mut VdbeCursor) -> Result<()> {
    if pc.cur.e_state < CURSOR_REQUIRESEEK {
        return Ok(());
    }
    if bt.bc_needs_index_restore(&pc.cur) {
        let Some(ki) = pc.key_info.clone() else {
            return Err(corrupt());
        };
        return bt.bc_restore_index_with(&mut pc.cur, &mut |bt, cur, key| {
            let mems = sqlite3_vdbe_record_unpack(&ki, key, usize::from(ki.n_key_field) + 1);
            if mems.is_empty() || mems.len() > usize::from(ki.n_all_field) {
                return Err(corrupt());
            }
            let mut r = UnpackedRecord::new(&ki, &mems, mems.len() as u16, 0);
            r.find_compare();
            bt.bc_index_moveto(cur, &mut r)
        });
    }
    bt.bc_cursor_restore(&mut pc.cur).map(|_| ())
}

/// `sqlite3VdbeHandleMovedCursor`
fn handle_moved_cursor(bt: &mut Btree, pc: &mut VdbeCursor) -> Result<()> {
    let rc = restore_cursor_position(bt, pc);
    let is_different_row = rc.is_err() || !pc.cur.is_valid();
    pc.cache_status = CACHE_STALE;
    if is_different_row {
        pc.null_row = true;
    }
    rc
}

/// `sqlite3VdbeCursorRestore`
#[inline]
fn cursor_restore(bt: &mut Btree, pc: &mut VdbeCursor) -> Result<()> {
    if pc.cur.has_moved() {
        return handle_moved_cursor(bt, pc);
    }
    Ok(())
}

/// `filterHash`
fn filter_hash(a_mem: &[Mem], op: &VdbeOp) -> u64 {
    let mut h: u64 = 0;
    let n = match op.p4 {
        P4::Int32(n) => n,
        _ => 0,
    };
    for i in op.p3..op.p3 + n {
        let p = &a_mem[i as usize];
        if p.flags & (MEM_INT | MEM_INT_REAL) != 0 {
            h = h.wrapping_add(p.i as u64);
        } else if p.flags & MEM_REAL != 0 {
            h = h.wrapping_add(p.int_value() as u64);
        } else if p.flags & MEM_STR != 0 {
            let n = p.z.len();
            h = h.wrapping_add(n as u64);
            if n >= 8 {
                h = h.wrapping_add(u64::from_le_bytes([p.z[0], p.z[1], p.z[2], p.z[3], p.z[4], p.z[5], p.z[6], p.z[7]]));
                let t = &p.z[n - 8..];
                h = h.wrapping_add(u64::from_le_bytes([t[0], t[1], t[2], t[3], t[4], t[5], t[6], t[7]]));
            } else {
                let mut x = [0u8; 8];
                x[..n].copy_from_slice(&p.z);
                h = h.wrapping_add(u64::from_le_bytes(x));
            }
        } else if p.flags & MEM_BLOB != 0 {
            let n = p.z.len();
            if n > 0 {
                let mut x = [0u8; 8];
                let k = n.min(8);
                x[..k].copy_from_slice(&p.z[..k]);
                h = h.wrapping_add(u64::from_le_bytes(x));
            }
            h = h.wrapping_add(n as u64);
            if p.flags & MEM_ZERO != 0 {
                h = h.wrapping_add(p.n_zero as u64);
            }
        }
    }
    h
}

/// A cheap digest of a register for the opcode trace (`--cfg sqliters_dtrace`): the SQL type
/// and value, independent of incidental representations.
pub fn mem_digest(m: &Mem) -> String {
    let f = m.flags;
    if f & (MEM_NULL | MEM_STR | MEM_INT | MEM_REAL | MEM_BLOB | MEM_INT_REAL) == 0 || f & MEM_AGG != 0 {
        return "U".to_owned();
    }
    let fnv = |z: &[u8]| -> u32 {
        let mut h: u32 = 2166136261;
        for &b in z {
            h = (h ^ u32::from(b)).wrapping_mul(16777619);
        }
        h
    };
    if f & MEM_NULL != 0 {
        "N".to_owned()
    } else if f & MEM_INT != 0 {
        format!("I{}", m.i)
    } else if f & MEM_INT_REAL != 0 {
        format!("J{}", m.i)
    } else if f & MEM_REAL != 0 {
        format!("R{:x}", m.r.to_bits())
    } else if f & MEM_STR != 0 {
        format!("S{}:{:x}", m.z.len(), fnv(&m.z))
    } else {
        let nz = if f & MEM_ZERO != 0 { m.n_zero } else { 0 };
        format!("B{}+{}:{:x}", m.z.len(), nz, fnv(&m.z))
    }
}

impl Vdbe {
    /// `sqlite3VdbeMakeReady` (the run-time allocation) + `sqlite3VdbeRewind`.
    pub fn new(prog: Arc<Program>) -> Vdbe {
        let n_mem = prog.n_mem.max(1) as usize;
        let n_cursor = prog.n_cursor.max(0) as usize;
        let n_var = prog.n_var.max(0) as usize;
        let once: Vec<i32> = prog.a_op.iter().map(|op| if op.opcode == OP_Once { op.p1 } else { 0 }).collect();
        let init_p1 = prog.a_op.first().map_or(0, |op| op.p1);
        let n_op = prog.a_op.len();
        let mwf = prog.min_write_file_format;
        Vdbe {
            prog,
            a_mem: vec![Mem { flags: MEM_UNDEFINED, enc: 1, ..Mem::default() }; n_mem + 1],
            ap_csr: (0..n_cursor).map(|_| None).collect(),
            a_var: vec![Mem::null(); n_var],
            pc: 0,
            cache_ctr: 1,
            result_row: (0, 0),
            init_p1,
            once,
            a_counter: [0; 9],
            ephem: Vec::new(),
            a_agg: Vec::new(),
            a_rowset: Vec::new(),
            rt: vec![OpRt::None; n_op],
            rt_ready: false,
            scratch: Vec::new(),
            u_temp: Vec::new(),
            min_write_file_format: mwf,
            halted: false,
            expired: false,
            expire_seq: 0,
            n_vm_step: 0,
            call_steps: 0,
            n_change: 0,
            progress_limit: u64::MAX,
            progress_ops: 0,
            tr_pc: usize::MAX,
            err_pc: usize::MAX,
        }
    }

    /// `sqlite3_bind_*`: parameter `i` (1-based).
    pub fn bind(&mut self, i: usize, v: &Value) {
        if i >= 1 && i <= self.a_var.len() {
            self.a_var[i - 1].set_from_value(v);
        }
    }

    /// Port aid: address and name of the instruction that was running when the statement
    /// failed.
    pub fn failed_op(&self) -> String {
        match self.prog.a_op.get(self.err_pc) {
            Some(op) => format!("{}:{}", self.err_pc, sqlite3_opcode_name(op.opcode)),
            None => "?".to_owned(),
        }
    }

    /// The row `OP_ResultRow` produced (`sqlite3_column_*`).
    pub fn result_row(&self) -> &[Mem] {
        &self.a_mem[self.result_row.0..self.result_row.0 + self.result_row.1]
    }

    /// Binds the collations of the program to the connection.
    fn make_rt<H: VdbeHost>(&mut self, host: &mut H) -> Result<()> {
        let prog = Arc::clone(&self.prog);
        let find = |c: &CollSeq, host: &mut H| -> Result<Arc<CollRt>> {
            if let Some(b) = builtin_coll(&c.name) {
                return Ok(Arc::new(b));
            }
            match host.find_collation(&c.name) {
                Some(c) => Ok(Arc::new(c)),
                None => Err(SqliteError::new(ErrorCode::Error, format!("no such collation sequence: {}", c.name))),
            }
        };
        for (i, op) in prog.a_op.iter().enumerate() {
            match &op.p4 {
                P4::CollSeq(c) => self.rt[i] = OpRt::Coll(find(c, host)?),
                P4::KeyInfo(k) => {
                    let k: &KeyInfo = k;
                    let mut a_coll = Vec::with_capacity(k.a_coll.len());
                    for c in &k.a_coll {
                        a_coll.push(match c {
                            Some(c) => Some(find(c, host)?),
                            None => None,
                        });
                    }
                    self.rt[i] = OpRt::KeyInfo(Arc::new(KeyInfoRt {
                        enc: k.enc,
                        n_key_field: k.n_key_field,
                        n_all_field: k.n_all_field,
                        a_sort_flags: k.a_sort_flags.clone(),
                        a_coll,
                    }));
                }
                _ => {}
            }
        }
        self.rt_ready = true;
        Ok(())
    }

    /// `allocateCursor(p, iCur, nField, eCurType)`
    fn allocate_cursor<H: VdbeHost>(&mut self, host: &mut H, i_cur: usize, n_field: i32, e_cur_type: u8) -> Result<()> {
        if self.ap_csr[i_cur].is_some() {
            self.free_cursor(host, i_cur);
        }
        let n = n_field.max(0) as usize;
        let dummy = Box::new(BtCursor::detached());
        self.ap_csr[i_cur] = Some(Box::new(VdbeCursor {
            e_cur_type,
            i_db: 0,
            null_row: false,
            deferred_moveto: false,
            is_table: false,
            is_ephemeral: false,
            use_random_rowid: false,
            is_ordered: false,
            no_reuse: false,
            seek_hit: 0,
            alt_map_op: None,
            btx: None,
            seq_count: 0,
            cache_status: CACHE_STALE,
            seek_result: 0,
            p_alt_cursor: None,
            cur: dummy,
            parked: None,
            sorter: None,
            key_info: None,
            orders: Vec::new(),
            i_hdr_offset: 0,
            pgno_root: 0,
            n_field,
            n_hdr_parsed: 0,
            moveto_target: 0,
            a_offset: vec![0; n + 2],
            a_type: vec![0; n + 2],
            a_row: Vec::new(),
            a_row_ok: false,
            payload_size: 0,
            sz_row: 0,
            hdr_buf: Vec::new(),
            p_cache: None,
        }));
        Ok(())
    }

    /// `sqlite3VdbeFreeCursor(p, p->apCsr[i])`
    fn free_cursor<H: VdbeHost>(&mut self, host: &mut H, i_cur: usize) {
        let Some(mut pc) = self.ap_csr[i_cur].take() else {
            return;
        };
        if pc.e_cur_type == CURTYPE_BTREE {
            if let Some(id) = pc.parked.take()
                && let Some(bt) = host.btree(pc.i_db as usize)
            {
                bt.bc_unpark(id, &mut pc.cur);
            }
            match pc.btx {
                Some(x) => {
                    let mut close = false;
                    if let Some(e) = self.ephem.get_mut(x).and_then(|e| e.as_mut()) {
                        e.bt.bc_close_cursor(&mut pc.cur);
                        e.n_ref -= 1;
                        close = e.n_ref == 0;
                    }
                    if close && let Some(mut e) = self.ephem[x].take() {
                        // sqlite3BtreeClose of a BTREE_SINGLE b-tree with its last cursor
                        let _ = e.bt.close();
                    }
                }
                None => {
                    if let Some(bt) = host.btree(pc.i_db as usize) {
                        bt.bc_close_cursor(&mut pc.cur);
                    }
                }
            }
        }
    }

    /// `closeAllCursors` (`sqlite3VdbeHalt`): the cursors are closed in the order of their
    /// numbers. The end of the read transaction is the engine's (`sqlite3VdbeHalt` →
    /// `vdbeCommit` for an autocommit statement).
    pub fn halt<H: VdbeHost>(&mut self, host: &mut H) {
        for i in 0..self.ap_csr.len() {
            self.free_cursor(host, i);
        }
        for e in self.ephem.iter_mut() {
            if let Some(mut e) = e.take() {
                let _ = e.bt.close();
            }
        }
        for a in self.a_agg.drain(..) {
            if let Some(AggCtx::Host(Some(id))) = a {
                // (vdbeMemClearExternAndSetNull: an accumulator that was never finalized)
                let _ = host.agg_finalize(id);
            }
        }
        self.a_rowset.clear();
        self.halted = true;
    }

    /// `saveAllCursors(pBt, iRoot, pExcept)` for the cursors of the machine on ephemeral
    /// b-tree `btx`, root `root`, before a write through cursor `except`.
    fn save_all_cursors(&mut self, btx: usize, root: Pgno, except: usize) -> Result<()> {
        for i in (0..self.ap_csr.len()).rev() {
            if i == except {
                continue;
            }
            if let Some(pc) = self.ap_csr[i].as_deref_mut()
                && pc.e_cur_type == CURTYPE_BTREE
                && pc.btx == Some(btx)
                && pc.pgno_root == root
                && let Some(e) = self.ephem.get_mut(btx).and_then(|e| e.as_mut())
            {
                e.bt.bc_save_for_write(&mut pc.cur)?;
            }
        }
        Ok(())
    }

    /// `saveAllCursors(pBt, iRoot, pExcept)` for the cursors of the machine on root `root` of
    /// database `i_db`, before a write through cursor `except` (`usize::MAX`: none): the most
    /// recently opened cursor first, as `BtShared.pCursor` lists them.
    fn save_db_cursors<H: VdbeHost>(&mut self, host: &mut H, i_db: i8, root: Pgno, except: usize) -> Result<()> {
        let Some(bt) = host.btree(i_db as usize) else {
            return Ok(());
        };
        for i in (0..self.ap_csr.len()).rev() {
            if i == except {
                continue;
            }
            if let Some(pc) = self.ap_csr[i].as_deref_mut()
                && pc.e_cur_type == CURTYPE_BTREE
                && pc.btx.is_none()
                && pc.i_db == i_db
                && pc.pgno_root == root
            {
                bt.bc_save_for_write(&mut pc.cur)?;
            }
        }
        Ok(())
    }

    /// The collation of the `OP_CollSeq` in front of instruction `i_op`
    /// (`sqlite3GetFuncCollSeq`).
    fn func_coll_seq(&self, i_op: usize) -> Option<Arc<CollRt>> {
        if i_op == 0 {
            return None;
        }
        match &self.rt[i_op - 1] {
            OpRt::Coll(c) => Some(Arc::clone(c)),
            _ => None,
        }
    }

    /// `sqlite3VdbeRewind(p)`: the statement is ready to run (again) from its first
    /// instruction. Bound parameters (`aVar`), the run counter of `OP_Init` and the
    /// `sqlite3_stmt_status` counters are kept, as in C.
    pub fn rewind(&mut self) {
        self.pc = 0;
        self.cache_ctr = 1;
        self.result_row = (0, 0);
        for m in &mut self.a_mem {
            m.flags = MEM_UNDEFINED;
            m.z.clear();
        }
        self.halted = false;
        self.expired = false;
        self.n_change = 0;
        self.tr_pc = usize::MAX;
        self.err_pc = usize::MAX;
    }

    /// `sqlite3_stmt_status(pStmt, op, resetFlag)` for the counters the machine keeps
    /// (`SQLITE_STMTSTATUS_FULLSCAN_STEP` 1, `SORT` 2, `AUTOINDEX` 3, `VM_STEP` 4, `RUN` 6,
    /// `FILTER_MISS` 7, `FILTER_HIT` 8).
    pub fn stmt_status(&mut self, op: usize, reset: bool) -> u32 {
        let Some(c) = self.a_counter.get_mut(op) else {
            return 0;
        };
        let v = *c;
        if reset {
            *c = 0;
        }
        v
    }

    /// The statement is suspended at a result row: its cursors on the b-trees of the
    /// connection join the cursor lists of those b-trees, so that a write or a rollback of
    /// another statement reaches them (`saveAllCursors`, `sqlite3BtreeTripAllCursors`).
    fn park_cursors<H: VdbeHost>(&mut self, host: &mut H) {
        for pc in self.ap_csr.iter_mut().flatten() {
            if pc.e_cur_type == CURTYPE_BTREE
                && pc.btx.is_none()
                && pc.parked.is_none()
                && let Some(bt) = host.btree(pc.i_db as usize)
            {
                pc.parked = Some(bt.bc_park(&mut pc.cur));
            }
        }
    }

    /// The statement runs again: its cursors come back from the cursor lists.
    fn unpark_cursors<H: VdbeHost>(&mut self, host: &mut H) {
        for pc in self.ap_csr.iter_mut().flatten() {
            if let Some(id) = pc.parked.take() {
                match host.btree(pc.i_db as usize) {
                    Some(bt) => {
                        bt.bc_unpark(id, &mut pc.cur);
                    }
                    None => {
                        *pc.cur = BtCursor::detached();
                        pc.cur.e_state = sqliters_btree::btcursor::CURSOR_FAULT;
                        pc.cur.skip_next = 4;
                    }
                }
            }
        }
    }

    /// The progress handler at C's cadence: "while( nVmStep>=nProgressLimit && db->xProgress!=0 )".
    #[cold]
    fn progress_check<H: VdbeHost>(&mut self, host: &mut H) -> Result<()> {
        while self.call_steps >= self.progress_limit && self.progress_ops != 0 {
            self.progress_limit += self.progress_ops;
            if host.progress_invoke()? {
                self.progress_limit = u64::MAX;
                return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
            }
        }
        Ok(())
    }

    /// `sqlite3VdbeExec(p)`: runs until the next result row (`Ok(true)`, `SQLITE_ROW`) or to
    /// the end (`Ok(false)`, `SQLITE_DONE`). On an error the statement has been halted
    /// (`sqlite3VdbeHalt`: its cursors are closed).
    pub fn exec<H: VdbeHost>(&mut self, host: &mut H) -> Result<bool> {
        if self.halted {
            return Ok(false);
        }
        self.unpark_cursors(host);
        // "if( db->xProgress ){ iPrior = p->aCounter[SQLITE_STMTSTATUS_VM_STEP];
        //   nProgressLimit = db->nProgressOps - (iPrior % db->nProgressOps); }"
        self.call_steps = 0;
        self.progress_ops = host.progress_ops();
        self.progress_limit = if self.progress_ops != 0 {
            self.progress_ops - (u64::from(self.a_counter[SQLITE_STMTSTATUS_VM_STEP]) % self.progress_ops)
        } else {
            u64::MAX
        };
        let mut r = self.exec_inner(host);
        // vdbe_return: the progress handler once more, then the step counter
        if (r.is_ok() || self.halted) && self.call_steps >= self.progress_limit && self.progress_ops != 0 {
            if let Err(e) = self.progress_check(host) {
                r = Err(e);
            }
        }
        self.a_counter[SQLITE_STMTSTATUS_VM_STEP] = self.a_counter[SQLITE_STMTSTATUS_VM_STEP].wrapping_add(self.call_steps as u32);
        match r {
            Ok(true) => {
                self.park_cursors(host);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(e) => {
                // abort_due_to_error: sqlite3VdbeHalt(p)
                #[cfg(sqliters_dtrace)]
                {
                    if sqliters_types::dtrace_enabled() && self.tr_pc < self.prog.a_op.len() && !self.halted {
                        self.trace_op(self.tr_pc);
                    }
                }
                self.halt(host);
                Err(e)
            }
        }
    }

    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn exec_inner<H: VdbeHost>(&mut self, host: &mut H) -> Result<bool> {
        if !self.rt_ready {
            self.make_rt(host)?;
        }
        let prog = Arc::clone(&self.prog);
        let a_op: &[VdbeOp] = &prog.a_op;
        let mut pc = self.pc as usize;
        // Locals of sqlite3VdbeExec: they start again at every call.
        let mut i_compare: i32 = 0;
        let mut col_cache_ctr: u32 = 0;
        let limit_length = host.limit_length();
        #[cfg(sqliters_dtrace)]
        let mut tr_prev: Option<usize> = None;

        host.check_interrupt()?;

        macro_rules! jump {
            ($to:expr) => {{
                pc = $to as usize;
                continue;
            }};
        }
        // check_for_interrupt: the interrupt flag, then the progress handler
        macro_rules! check_int {
            () => {{
                host.check_interrupt()?;
                if self.call_steps >= self.progress_limit {
                    self.progress_check(host)?;
                }
            }};
        }
        macro_rules! jump_check {
            ($to:expr) => {{
                pc = $to as usize;
                check_int!();
                continue;
            }};
        }
        macro_rules! csr {
            ($i:expr) => {
                match self.ap_csr[$i as usize].as_deref_mut() {
                    Some(c) => c,
                    None => return Err(internal("cursor not open")),
                }
            };
        }
        // saveAllCursors(pBt, pgnoRoot, pCur) for the cursors of this statement, before a write
        // through cursor $ci
        macro_rules! save_cursors {
            ($ci:expr) => {{
                let (btx, i_db, root) = {
                    let c = csr!($ci);
                    (c.btx, c.i_db, c.pgno_root)
                };
                match btx {
                    Some(x) => self.save_all_cursors(x, root, $ci)?,
                    None => self.save_db_cursors(host, i_db, root, $ci)?,
                }
            }};
        }
        macro_rules! btree_of {
            ($pc:expr) => {
                match $pc.btx {
                    Some(x) => match self.ephem.get_mut(x).and_then(|e| e.as_mut()) {
                        Some(e) => &mut e.bt,
                        None => return Err(internal("ephemeral b-tree is closed")),
                    },
                    None => match host.btree($pc.i_db as usize) {
                        Some(b) => b,
                        None => return Err(internal("database has no b-tree")),
                    },
                }
            };
        }

        loop {
            let op = &a_op[pc];
            #[cfg(sqliters_dtrace)]
            {
                if sqliters_types::dtrace_enabled() {
                    if let Some(pp) = tr_prev {
                        self.trace_op(pp);
                    }
                    tr_prev = Some(pc);
                    self.tr_pc = pc;
                }
            }
            self.n_vm_step += 1;
            self.call_steps += 1;
            self.err_pc = pc;
            match op.opcode {
                OP_Goto => jump_check!(op.p2),

                OP_Gosub => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.flags = MEM_INT;
                    m.i = pc as i64;
                    jump_check!(op.p2);
                }

                OP_Return => {
                    let m = &self.a_mem[op.p1 as usize];
                    if m.flags & MEM_INT != 0 {
                        pc = m.i as usize;
                    }
                }

                OP_InitCoroutine => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.i = i64::from(op.p3 - 1);
                    m.flags = MEM_INT;
                    if op.p2 != 0 {
                        jump!(op.p2);
                    }
                }

                OP_EndCoroutine => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    let caller = &a_op[m.i as usize];
                    m.i = pc as i64 - 1;
                    jump!(caller.p2);
                }

                OP_Yield => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.flags = MEM_INT;
                    let pc_dest = m.i as usize;
                    m.i = pc as i64;
                    pc = pc_dest;
                }

                OP_HaltIfNull | OP_Halt => {
                    if op.opcode == OP_HaltIfNull && self.a_mem[op.p3 as usize].flags & MEM_NULL == 0 {
                        pc += 1;
                        continue;
                    }
                    #[cfg(sqliters_dtrace)]
                    {
                        if sqliters_types::dtrace_enabled() {
                            self.trace_op(pc);
                        }
                    }
                    self.pc = pc as i32;
                    if op.p1 != 0 {
                        // p->rc = pOp->p1: the error of the program
                        let msg = if op.p3 > 0 && !op.p4.is_used() {
                            let m = &self.a_mem[op.p3 as usize];
                            let mut t = m.clone();
                            if t.flags & (MEM_STR | MEM_BLOB) == 0 && t.flags & MEM_NULL == 0 {
                                t.stringify(false);
                            }
                            String::from_utf8_lossy(&t.z).into_owned()
                        } else if op.p5 != 0 {
                            const AZ: [&str; 4] = ["NOT NULL", "UNIQUE", "CHECK", "FOREIGN KEY"];
                            let mut s = format!("{} constraint failed", AZ[(op.p5 as usize - 1).min(3)]);
                            if let Some(z) = p4_text(&op.p4) {
                                s = format!("{s}: {}", String::from_utf8_lossy(z));
                            }
                            s
                        } else {
                            p4_text(&op.p4).map(|z| String::from_utf8_lossy(z).into_owned()).unwrap_or_default()
                        };
                        let code = (op.p1 & 0xff) as u8;
                        let ec = error_code_of(code);
                        let mut e = SqliteError::with_extended(op.p1, msg);
                        e.code = ec;
                        // p->errorAction = (u8)pOp->p2
                        e.conflict_effect = match op.p2 {
                            OE_ROLLBACK => ConflictResolutionEffect::Rollback,
                            OE_FAIL => ConflictResolutionEffect::Fail,
                            _ => ConflictResolutionEffect::Abort,
                        };
                        return Err(e);
                    }
                    self.halt(host);
                    return Ok(false);
                }

                OP_Integer => {
                    let m = &mut self.a_mem[op.p2 as usize];
                    m.flags = MEM_INT;
                    m.i = i64::from(op.p1);
                }

                OP_Int64 => {
                    let m = &mut self.a_mem[op.p2 as usize];
                    m.flags = MEM_INT;
                    m.i = int32_to_64(op.p1, op.p3);
                }

                OP_Real => {
                    let m = &mut self.a_mem[op.p2 as usize];
                    m.flags = MEM_REAL;
                    m.r = f64::from_bits(int32_to_64(op.p1, op.p3) as u64);
                }

                OP_String8 | OP_String => {
                    let z = p4_text(&op.p4).unwrap_or(&[]);
                    // sqlite3Strlen30(pOp->p4.z)
                    let n = if op.opcode == OP_String8 {
                        z.iter().position(|&b| b == 0).unwrap_or(z.len())
                    } else {
                        (op.p1.max(0) as usize).min(z.len())
                    };
                    if n as i64 > i64::from(limit_length) {
                        return Err(too_big());
                    }
                    let is_blob = op.p3 > 0 && self.a_mem[op.p3 as usize].i == i64::from(op.p5);
                    let m = &mut self.a_mem[op.p2 as usize];
                    m.z.clear();
                    m.z.extend_from_slice(&z[..n]);
                    m.enc = 1;
                    m.flags = if is_blob { MEM_BLOB | MEM_TERM } else { MEM_STR | MEM_TERM };
                }

                OP_BeginSubrtn | OP_Null => {
                    let null_flag = if op.p1 != 0 { MEM_NULL | MEM_CLEARED } else { MEM_NULL };
                    let mut cnt = op.p3 - op.p2;
                    let mut i = op.p2 as usize;
                    self.a_mem[i].set_null();
                    self.a_mem[i].flags = null_flag;
                    while cnt > 0 {
                        i += 1;
                        self.a_mem[i].set_null();
                        self.a_mem[i].flags = null_flag;
                        cnt -= 1;
                    }
                }

                OP_SoftNull => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.flags = (m.flags & !(MEM_UNDEFINED | MEM_AFF_MASK)) | MEM_NULL;
                }

                OP_Blob => {
                    let m = &mut self.a_mem[op.p2 as usize];
                    match &op.p4 {
                        P4::Dynamic(z) => {
                            let n = (op.p1.max(0) as usize).min(z.len());
                            m.set_blob(&z[..n]);
                        }
                        P4::Static(z) => {
                            let z = z.as_bytes();
                            let n = (op.p1.max(0) as usize).min(z.len());
                            m.set_blob(&z[..n]);
                        }
                        _ => {
                            m.set_zero_blob(op.p1);
                            m.expand_blob();
                        }
                    }
                    m.enc = 1;
                }

                OP_Variable => {
                    let v = &self.a_var[op.p1 as usize - 1];
                    if v.too_big(limit_length) {
                        return Err(too_big());
                    }
                    let mut m = v.clone();
                    m.flags |= MEM_FROM_BIND;
                    self.a_mem[op.p2 as usize] = m;
                }

                OP_Move => {
                    let mut n = op.p3;
                    let mut p1 = op.p1 as usize;
                    let mut p2 = op.p2 as usize;
                    loop {
                        // sqlite3VdbeMemMove(pOut, pIn1)
                        let v = std::mem::replace(&mut self.a_mem[p1], Mem::null());
                        self.a_mem[p2] = v;
                        if !self.a_agg.is_empty() || !self.a_rowset.is_empty() {
                            self.move_side(p1, p2);
                        }
                        p1 += 1;
                        p2 += 1;
                        n -= 1;
                        if n <= 0 {
                            break;
                        }
                    }
                }

                OP_Copy => {
                    let mut n = op.p3;
                    let mut p1 = op.p1 as usize;
                    let mut p2 = op.p2 as usize;
                    loop {
                        let v = self.a_mem[p1].clone();
                        let out = &mut self.a_mem[p2];
                        *out = v;
                        if out.flags & MEM_SUBTYPE != 0 && op.p5 & 0x0002 != 0 {
                            out.flags &= !MEM_SUBTYPE;
                        }
                        if n == 0 {
                            break;
                        }
                        n -= 1;
                        p1 += 1;
                        p2 += 1;
                    }
                }

                OP_SCopy => {
                    let v = self.a_mem[op.p1 as usize].clone();
                    self.a_mem[op.p2 as usize] = v;
                }

                OP_IntCopy => {
                    let v = self.a_mem[op.p1 as usize].i;
                    self.a_mem[op.p2 as usize].set_int64(v);
                }

                OP_ResultRow => {
                    self.cache_ctr = (self.cache_ctr.wrapping_add(2)) | 1;
                    self.result_row = (op.p1 as usize, op.p2 as usize);
                    #[cfg(sqliters_dtrace)]
                    {
                        if sqliters_types::dtrace_enabled() {
                            self.trace_op(pc);
                        }
                    }
                    self.pc = pc as i32 + 1;
                    return Ok(true);
                }

                OP_Concat => {
                    let (p1, p2, p3) = (op.p1 as usize, op.p2 as usize, op.p3 as usize);
                    let flags1 = self.a_mem[p1].flags;
                    if (flags1 | self.a_mem[p2].flags) & MEM_NULL != 0 {
                        self.a_mem[p3].set_null();
                    } else {
                        // (the string forms are computed on copies: C restores the flags of
                        // the operands afterwards)
                        let mut a = self.a_mem[p1].clone();
                        if a.flags & (MEM_STR | MEM_BLOB) == 0 {
                            a.stringify(false);
                        } else if a.flags & MEM_ZERO != 0 {
                            a.expand_blob();
                        }
                        let mut b = self.a_mem[p2].clone();
                        if b.flags & (MEM_STR | MEM_BLOB) == 0 {
                            b.stringify(false);
                        } else if b.flags & MEM_ZERO != 0 {
                            b.expand_blob();
                        }
                        let n_byte = a.z.len() as i64 + b.z.len() as i64;
                        if n_byte > i64::from(limit_length) {
                            return Err(too_big());
                        }
                        let out = &mut self.a_mem[p3];
                        let mut z = b.z;
                        z.extend_from_slice(&a.z);
                        out.z = z;
                        out.set_type_flag(MEM_STR);
                        out.flags |= MEM_TERM;
                        out.enc = 1;
                    }
                }

                OP_Add | OP_Subtract | OP_Multiply | OP_Divide | OP_Remainder => {
                    let (p1, p2, p3) = (op.p1 as usize, op.p2 as usize, op.p3 as usize);
                    let mut type1 = self.a_mem[p1].flags;
                    let mut type2 = self.a_mem[p2].flags;
                    // 0: int result in i_b, 1: fp math, 2: null
                    let mut state = 1u8;
                    let mut i_b: i64 = 0;
                    let mut int_math = type1 & type2 & MEM_INT != 0;
                    if !int_math {
                        if (type1 | type2) & MEM_NULL != 0 {
                            state = 2;
                        } else {
                            type1 = self.a_mem[p1].numeric_type();
                            type2 = self.a_mem[p2].numeric_type();
                            if type1 & type2 & MEM_INT != 0 {
                                int_math = true;
                            }
                        }
                    }
                    if int_math {
                        let i_a = self.a_mem[p1].i;
                        i_b = self.a_mem[p2].i;
                        state = 0;
                        match op.opcode {
                            OP_Add => match i_b.checked_add(i_a) {
                                Some(v) => i_b = v,
                                None => state = 1,
                            },
                            OP_Subtract => match i_b.checked_sub(i_a) {
                                Some(v) => i_b = v,
                                None => state = 1,
                            },
                            OP_Multiply => match i_b.checked_mul(i_a) {
                                Some(v) => i_b = v,
                                None => state = 1,
                            },
                            OP_Divide => {
                                if i_a == 0 {
                                    state = 2;
                                } else if i_a == -1 && i_b == SMALLEST_INT64 {
                                    state = 1;
                                } else {
                                    i_b /= i_a;
                                }
                            }
                            _ => {
                                if i_a == 0 {
                                    state = 2;
                                } else {
                                    let d = if i_a == -1 { 1 } else { i_a };
                                    i_b %= d;
                                }
                            }
                        }
                    }
                    if state == 1 {
                        // fp_math
                        let r_a = self.a_mem[p1].real_value();
                        let mut r_b = self.a_mem[p2].real_value();
                        let mut null = false;
                        match op.opcode {
                            OP_Add => r_b += r_a,
                            OP_Subtract => r_b -= r_a,
                            OP_Multiply => r_b *= r_a,
                            OP_Divide => {
                                if r_a == 0.0 {
                                    null = true;
                                } else {
                                    r_b /= r_a;
                                }
                            }
                            _ => {
                                let mut i_a = self.a_mem[p1].int_value();
                                let i_b2 = self.a_mem[p2].int_value();
                                if i_a == 0 {
                                    null = true;
                                } else {
                                    if i_a == -1 {
                                        i_a = 1;
                                    }
                                    r_b = (i_b2 % i_a) as f64;
                                }
                            }
                        }
                        if null || r_b.is_nan() {
                            state = 2;
                        } else {
                            let out = &mut self.a_mem[p3];
                            out.r = r_b;
                            out.set_type_flag(MEM_REAL);
                        }
                    } else if state == 0 {
                        let out = &mut self.a_mem[p3];
                        out.i = i_b;
                        out.set_type_flag(MEM_INT);
                    }
                    if state == 2 {
                        self.a_mem[p3].set_null();
                    }
                }

                OP_CollSeq => {
                    if op.p1 != 0 {
                        self.a_mem[op.p1 as usize].set_int64(0);
                    }
                }

                OP_BitAnd | OP_BitOr | OP_ShiftLeft | OP_ShiftRight => {
                    let (p1, p2, p3) = (op.p1 as usize, op.p2 as usize, op.p3 as usize);
                    if (self.a_mem[p1].flags | self.a_mem[p2].flags) & MEM_NULL != 0 {
                        self.a_mem[p3].set_null();
                    } else {
                        let mut i_a = self.a_mem[p2].int_value();
                        let mut i_b = self.a_mem[p1].int_value();
                        let mut o = op.opcode;
                        if o == OP_BitAnd {
                            i_a &= i_b;
                        } else if o == OP_BitOr {
                            i_a |= i_b;
                        } else if i_b != 0 {
                            // If shifting by a negative amount, shift in the other direction
                            if i_b < 0 {
                                o = 2 * OP_ShiftLeft + 1 - o;
                                i_b = if i_b > -64 { -i_b } else { 64 };
                            }
                            if i_b >= 64 {
                                i_a = if i_a >= 0 || o == OP_ShiftLeft { 0 } else { -1 };
                            } else {
                                let mut u_a = i_a as u64;
                                if o == OP_ShiftLeft {
                                    u_a <<= i_b;
                                } else {
                                    u_a >>= i_b;
                                    // Sign-extend on a right shift of a negative number
                                    if i_a < 0 {
                                        u_a |= u64::MAX << (64 - i_b);
                                    }
                                }
                                i_a = u_a as i64;
                            }
                        }
                        let out = &mut self.a_mem[p3];
                        out.i = i_a;
                        out.set_type_flag(MEM_INT);
                    }
                }

                OP_AddImm => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.integerify();
                    m.i = m.i.wrapping_add(i64::from(op.p2));
                }

                OP_MustBeInt => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    if m.flags & MEM_INT == 0 {
                        if m.flags & MEM_REAL != 0 {
                            if m.r >= -9223372036854775808.0 && m.r < 9223372036854775808.0 {
                                let ix = sqlite3_real_to_i64(m.r);
                                if ix as f64 == m.r {
                                    m.i = ix;
                                    m.set_type_flag(MEM_INT);
                                }
                            }
                        } else {
                            m.apply_affinity(SQLITE_AFF_NUMERIC);
                        }
                        if m.flags & MEM_INT == 0 {
                            if op.p2 == 0 {
                                return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                            }
                            jump!(op.p2);
                        }
                    }
                    m.set_type_flag(MEM_INT);
                }

                OP_RealAffinity => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    if m.flags & (MEM_INT | MEM_INT_REAL) != 0 {
                        m.realify();
                    }
                }

                OP_Cast => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    m.expand_blob();
                    m.cast(op.p2 as u8);
                }

                OP_Eq | OP_Ne | OP_Lt | OP_Le | OP_Gt | OP_Ge => {
                    let (p1, p3) = (op.p1 as usize, op.p3 as usize);
                    let mut flags1 = self.a_mem[p1].flags;
                    let mut flags3 = self.a_mem[p3].flags;
                    let (lt, eq, gt) = match op.opcode {
                        OP_Eq => (false, true, false),
                        OP_Ne => (true, false, true),
                        OP_Lt => (true, false, false),
                        OP_Le => (true, true, false),
                        OP_Gt => (false, false, true),
                        _ => (false, true, true),
                    };
                    if flags1 & flags3 & MEM_INT != 0 {
                        // Common case of comparison of two integers
                        let a = self.a_mem[p3].i;
                        let b = self.a_mem[p1].i;
                        if a > b {
                            if gt {
                                jump!(op.p2);
                            }
                            i_compare = 1;
                        } else if a < b {
                            if lt {
                                jump!(op.p2);
                            }
                            i_compare = -1;
                        } else {
                            if eq {
                                jump!(op.p2);
                            }
                            i_compare = 0;
                        }
                        pc += 1;
                        continue;
                    }
                    let res: i32;
                    if (flags1 | flags3) & MEM_NULL != 0 {
                        // One or both operands are NULL
                        if op.p5 & SQLITE_NULLEQ != 0 {
                            // If SQLITE_NULLEQ is set (which will only happen if the operator
                            // is OP_Eq or OP_Ne) then take the jump or not depending on
                            // whether or not both operands are null.
                            if flags1 & flags3 & MEM_NULL != 0 && flags3 & MEM_CLEARED == 0 {
                                res = 0; // Operands are equal
                            } else {
                                res = if flags3 & MEM_NULL != 0 { -1 } else { 1 }; // Operands are not equal
                            }
                        } else {
                            // SQLITE_NULLEQ is clear and at least one operand is NULL, then
                            // the result is always NULL. The jump is taken if the
                            // SQLITE_JUMPIFNULL bit is set.
                            if op.p5 & SQLITE_JUMPIFNULL != 0 {
                                jump!(op.p2);
                            }
                            i_compare = 1; // Operands are not equal
                            pc += 1;
                            continue;
                        }
                    } else {
                        // Neither operand is NULL and we couldn't do the special high-speed
                        // integer comparison case. So do a general-case comparison.
                        let affinity = (op.p5 as u8) & SQLITE_AFF_MASK;
                        if affinity >= SQLITE_AFF_NUMERIC {
                            if (flags1 | flags3) & MEM_STR != 0 {
                                if flags1 & (MEM_INT | MEM_INT_REAL | MEM_REAL | MEM_STR) == MEM_STR {
                                    self.a_mem[p1].apply_numeric_affinity(false);
                                    flags3 = self.a_mem[p3].flags;
                                }
                                if flags3 & (MEM_INT | MEM_INT_REAL | MEM_REAL | MEM_STR) == MEM_STR {
                                    self.a_mem[p3].apply_numeric_affinity(false);
                                }
                            }
                        } else if affinity == SQLITE_AFF_TEXT && (flags1 | flags3) & MEM_STR != 0 {
                            if flags1 & MEM_STR != 0 {
                                self.a_mem[p1].flags &= !(MEM_INT | MEM_REAL | MEM_INT_REAL);
                            } else if flags1 & (MEM_INT | MEM_REAL | MEM_INT_REAL) != 0 {
                                self.a_mem[p1].stringify(true);
                                flags1 = (self.a_mem[p1].flags & !MEM_TYPE_MASK) | (flags1 & MEM_TYPE_MASK);
                            }
                            if flags3 & MEM_STR != 0 {
                                self.a_mem[p3].flags &= !(MEM_INT | MEM_REAL | MEM_INT_REAL);
                            } else if flags3 & (MEM_INT | MEM_REAL | MEM_INT_REAL) != 0 {
                                self.a_mem[p3].stringify(true);
                                flags3 = (self.a_mem[p3].flags & !MEM_TYPE_MASK) | (flags3 & MEM_TYPE_MASK);
                            }
                        }
                        let coll = match &self.rt[pc] {
                            OpRt::Coll(c) => Some(&**c),
                            _ => None,
                        };
                        res = sqlite3_mem_compare(&self.a_mem[p3], &self.a_mem[p1], coll);
                    }
                    let res2 = if res < 0 {
                        lt
                    } else if res == 0 {
                        eq
                    } else {
                        gt
                    };
                    i_compare = res;
                    // Undo any changes made by applyAffinity() to the input registers.
                    self.a_mem[p3].flags = flags3;
                    self.a_mem[p1].flags = flags1;
                    if res2 {
                        jump!(op.p2);
                    }
                }

                OP_ElseEq => {
                    if i_compare == 0 {
                        jump!(op.p2);
                    }
                }

                OP_Permutation => {}

                OP_Compare => {
                    let a_permute: Option<&[u32]> = if op.p5 & OPFLAG_PERMUTE == 0 {
                        None
                    } else {
                        match &a_op[pc - 1].p4 {
                            P4::IntArray(a) => Some(a),
                            _ => None,
                        }
                    };
                    let n = op.p3 as usize;
                    let OpRt::KeyInfo(key_info) = &self.rt[pc] else {
                        return Err(internal("OP_Compare without KeyInfo"));
                    };
                    let (p1, p2) = (op.p1 as usize, op.p2 as usize);
                    for i in 0..n {
                        let idx = a_permute.map_or(i, |a| a[i] as usize);
                        let coll = key_info.a_coll.get(i).and_then(|c| c.as_deref());
                        let sf = key_info.a_sort_flags.get(i).copied().unwrap_or(0);
                        let b_rev = sf & KEYINFO_ORDER_DESC != 0;
                        i_compare = sqlite3_mem_compare(&self.a_mem[p1 + idx], &self.a_mem[p2 + idx], coll);
                        if i_compare != 0 {
                            if sf & KEYINFO_ORDER_BIGNULL != 0
                                && (self.a_mem[p1 + idx].flags & MEM_NULL != 0 || self.a_mem[p2 + idx].flags & MEM_NULL != 0)
                            {
                                i_compare = -i_compare;
                            }
                            if b_rev {
                                i_compare = -i_compare;
                            }
                            break;
                        }
                    }
                }

                OP_Jump => {
                    if i_compare < 0 {
                        jump!(op.p1);
                    } else if i_compare == 0 {
                        jump!(op.p2);
                    } else {
                        jump!(op.p3);
                    }
                }

                OP_And | OP_Or => {
                    let b = |m: &Mem| -> usize {
                        if m.flags & (MEM_INT | MEM_INT_REAL) != 0 {
                            usize::from(m.i != 0)
                        } else if m.flags & MEM_NULL != 0 {
                            2
                        } else {
                            usize::from(m.real_value() != 0.0)
                        }
                    };
                    let v1 = b(&self.a_mem[op.p1 as usize]);
                    let v2 = b(&self.a_mem[op.p2 as usize]);
                    let v = if op.opcode == OP_And {
                        const AND_LOGIC: [u8; 9] = [0, 0, 0, 0, 1, 2, 0, 2, 2];
                        AND_LOGIC[v1 * 3 + v2]
                    } else {
                        const OR_LOGIC: [u8; 9] = [0, 1, 2, 1, 1, 1, 2, 1, 2];
                        OR_LOGIC[v1 * 3 + v2]
                    };
                    let out = &mut self.a_mem[op.p3 as usize];
                    if v == 2 {
                        out.set_type_flag(MEM_NULL);
                    } else {
                        out.i = i64::from(v);
                        out.set_type_flag(MEM_INT);
                    }
                }

                OP_IsTrue => {
                    let p4 = match op.p4 {
                        P4::Int32(i) => i,
                        _ => 0,
                    };
                    let v = i64::from(self.a_mem[op.p1 as usize].boolean_value(op.p3 != 0)) ^ i64::from(p4);
                    self.a_mem[op.p2 as usize].set_int64(v);
                }

                OP_Not => {
                    let m = &self.a_mem[op.p1 as usize];
                    if m.flags & MEM_NULL == 0 {
                        let v = i64::from(!m.boolean_value(false));
                        self.a_mem[op.p2 as usize].set_int64(v);
                    } else {
                        self.a_mem[op.p2 as usize].set_null();
                    }
                }

                OP_BitNot => {
                    let m = &self.a_mem[op.p1 as usize];
                    if m.flags & MEM_NULL == 0 {
                        let v = !m.int_value();
                        self.a_mem[op.p2 as usize].set_int64(v);
                    } else {
                        self.a_mem[op.p2 as usize].set_null();
                    }
                }

                OP_Once => {
                    if self.init_p1 == self.once[pc] {
                        jump!(op.p2);
                    }
                    self.once[pc] = self.init_p1;
                }

                OP_If => {
                    if self.a_mem[op.p1 as usize].boolean_value(op.p3 != 0) {
                        jump!(op.p2);
                    }
                }

                OP_IfNot => {
                    if !self.a_mem[op.p1 as usize].boolean_value(op.p3 == 0) {
                        jump!(op.p2);
                    }
                }

                OP_IsNull => {
                    if self.a_mem[op.p1 as usize].flags & MEM_NULL != 0 {
                        jump!(op.p2);
                    }
                }

                OP_IsType => {
                    let type_mask: u16;
                    if op.p1 >= 0 {
                        let pc_ = csr!(op.p1);
                        if (op.p3 as u32) < pc_.n_hdr_parsed {
                            let serial_type = pc_.a_type[op.p3 as usize];
                            if serial_type >= 12 {
                                type_mask = if serial_type & 1 != 0 { 0x04 } else { 0x08 };
                            } else {
                                const A_MASK: [u8; 12] = [0x10, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x2, 0x01, 0x01, 0x10, 0x10];
                                type_mask = u16::from(A_MASK[serial_type as usize]);
                            }
                        } else {
                            let p4 = match op.p4 {
                                P4::Int32(i) => i,
                                _ => 1,
                            };
                            type_mask = 1 << (p4 - 1);
                        }
                    } else {
                        type_mask = 1 << (self.a_mem[op.p3 as usize].value_type() - 1);
                    }
                    if type_mask & op.p5 != 0 {
                        jump!(op.p2);
                    }
                }

                OP_ZeroOrNull => {
                    if self.a_mem[op.p1 as usize].flags & MEM_NULL != 0 || self.a_mem[op.p3 as usize].flags & MEM_NULL != 0 {
                        self.a_mem[op.p2 as usize].set_null();
                    } else {
                        self.a_mem[op.p2 as usize].set_int64(0);
                    }
                }

                OP_NotNull => {
                    if self.a_mem[op.p1 as usize].flags & MEM_NULL == 0 {
                        jump!(op.p2);
                    }
                }

                OP_IfNullRow => {
                    if let Some(c) = self.ap_csr[op.p1 as usize].as_deref()
                        && c.null_row
                    {
                        self.a_mem[op.p3 as usize].set_null();
                        jump!(op.p2);
                    }
                }

                OP_Offset => {
                    let p3 = op.p3 as usize;
                    match self.ap_csr[op.p1 as usize].as_deref_mut() {
                        Some(pc_) if pc_.e_cur_type == CURTYPE_BTREE => {
                            let bt = btree_of!(pc_);
                            if pc_.deferred_moveto {
                                finish_moveto(bt, pc_)?;
                            }
                            if pc_.cur.eof() {
                                self.a_mem[p3].set_null();
                            } else {
                                let v = bt.bc_offset(&mut pc_.cur)?;
                                self.a_mem[p3].set_int64(v);
                            }
                        }
                        _ => self.a_mem[p3].set_null(),
                    }
                }

                OP_Column => {
                    let mut ci = op.p1 as usize;
                    let mut p2 = op.p2 as u32;
                    let p3 = op.p3 as usize;
                    'col: loop {
                        // op_column_restart:
                        let pc_ = csr!(ci);
                        let mut read_header_direct = false;
                        if pc_.cache_status != self.cache_ctr {
                            if pc_.null_row {
                                if pc_.e_cur_type == CURTYPE_PSEUDO && pc_.seek_result > 0 {
                                    let reg = &self.a_mem[pc_.seek_result as usize];
                                    pc_.a_row.clear();
                                    pc_.a_row.extend_from_slice(&reg.z);
                                    pc_.payload_size = reg.z.len() as u32;
                                    pc_.sz_row = pc_.payload_size;
                                    pc_.a_row_ok = true;
                                } else {
                                    self.a_mem[p3].set_null();
                                    break 'col;
                                }
                            } else {
                                if pc_.deferred_moveto {
                                    if let Some(ao) = pc_.alt_map_op
                                        && let P4::IntArray(map) = &a_op[ao].p4
                                    {
                                        let i_map = map.get(p2 as usize).copied().unwrap_or(0);
                                        if i_map > 0
                                            && let Some(alt) = pc_.p_alt_cursor
                                        {
                                            ci = alt;
                                            p2 = i_map - 1;
                                            continue 'col;
                                        }
                                    }
                                    let bt = btree_of!(pc_);
                                    finish_moveto(bt, pc_)?;
                                } else if pc_.cur.has_moved() {
                                    let bt = btree_of!(pc_);
                                    handle_moved_cursor(bt, pc_)?;
                                    continue 'col;
                                }
                                let bt = btree_of!(pc_);
                                pc_.payload_size = bt.bc_payload_size(&mut pc_.cur)?;
                                let local = bt.bc_payload_fetch(&pc_.cur)?;
                                pc_.a_row.clear();
                                pc_.a_row.extend_from_slice(local);
                                pc_.sz_row = pc_.a_row.len() as u32;
                                pc_.a_row_ok = true;
                            }
                            pc_.cache_status = self.cache_ctr;
                            let b0 = pc_.a_row.first().copied().unwrap_or(0);
                            if b0 < 0x80 {
                                pc_.a_offset[0] = u32::from(b0);
                                pc_.i_hdr_offset = 1;
                            } else {
                                let (v, n) = get_varint32(&pc_.a_row, 0);
                                pc_.a_offset[0] = v;
                                pc_.i_hdr_offset = n as u32;
                            }
                            pc_.n_hdr_parsed = 0;
                            if pc_.sz_row < pc_.a_offset[0] {
                                // pC->aRow does not have to hold the entire row, but it does
                                // at least need to hold the complete header.
                                pc_.a_row_ok = false;
                                pc_.sz_row = 0;
                                if pc_.a_offset[0] > 98307 || pc_.a_offset[0] > pc_.payload_size {
                                    return Err(corrupt());
                                }
                            } else {
                                // This is an optimization: go directly to the header parse.
                                read_header_direct = true;
                            }
                        } else if pc_.e_cur_type == CURTYPE_BTREE && pc_.cur.has_moved() {
                            let bt = btree_of!(pc_);
                            handle_moved_cursor(bt, pc_)?;
                            continue 'col;
                        }

                        // Make sure at least the first p2+1 entries of the header have been
                        // parsed and valid information is in aOffset[] and pC->aType[].
                        let t: u32;
                        if read_header_direct || pc_.n_hdr_parsed <= p2 {
                            // If there is more header available for parsing in the record,
                            // try to extract additional fields up through the p2+1-th field
                            if read_header_direct || pc_.i_hdr_offset < pc_.a_offset[0] {
                                if !pc_.a_row_ok {
                                    let bt = btree_of!(pc_);
                                    let a0 = pc_.a_offset[0];
                                    sqlite3_vdbe_mem_from_btree_zero_offset(bt, &mut pc_.cur, a0, &mut pc_.hdr_buf)?;
                                }
                                let z_data: &[u8] = if pc_.a_row_ok { &pc_.a_row } else { &pc_.hdr_buf };
                                // op_column_read_header:
                                let mut i = pc_.n_hdr_parsed as usize;
                                let mut offset64 = u64::from(pc_.a_offset[i]);
                                let mut z_hdr = pc_.i_hdr_offset as usize;
                                let z_end_hdr = pc_.a_offset[0] as usize;
                                let mut tt: u32;
                                let max_i = pc_.a_type.len() - 1;
                                loop {
                                    tt = u32::from(z_data.get(z_hdr).copied().unwrap_or(0));
                                    if tt < 0x80 {
                                        z_hdr += 1;
                                        offset64 += u64::from(sqlite3_vdbe_serial_type_len(tt));
                                    } else {
                                        let (v, n) = get_varint32(z_data, z_hdr);
                                        z_hdr += n;
                                        tt = v;
                                        offset64 += u64::from(sqlite3_vdbe_serial_type_len(tt));
                                    }
                                    pc_.a_type[i] = tt;
                                    i += 1;
                                    pc_.a_offset[i] = (offset64 & 0xffff_ffff) as u32;
                                    if !(i as u32 <= p2 && z_hdr < z_end_hdr && i < max_i) {
                                        break;
                                    }
                                }
                                // The record is corrupt if any of the following are true:
                                // (1) the bytes of the header extend past the declared header
                                // size, (2) the entire header was used but not all data was
                                // used, (3) the end of the data extends beyond the end of the
                                // record.
                                if (z_hdr >= z_end_hdr && (z_hdr > z_end_hdr || offset64 != u64::from(pc_.payload_size)))
                                    || offset64 > u64::from(pc_.payload_size)
                                {
                                    if pc_.a_offset[0] == 0 {
                                        i = 0;
                                        z_hdr = z_end_hdr;
                                    } else {
                                        return Err(corrupt());
                                    }
                                }
                                pc_.n_hdr_parsed = i as u32;
                                pc_.i_hdr_offset = z_hdr as u32;
                            }
                            // If after trying to extract new entries from the header,
                            // nHdrParsed is still not up to p2, that means that the record
                            // has fewer than p2 columns. So the result will be either the
                            // default value or a NULL.
                            if pc_.n_hdr_parsed <= p2 {
                                match &op.p4 {
                                    P4::Mem(v) => self.a_mem[p3].set_from_value(v),
                                    _ => self.a_mem[p3].set_null(),
                                }
                                break 'col;
                            }
                            t = pc_.a_type[p2 as usize];
                        } else {
                            t = pc_.a_type[p2 as usize];
                        }

                        // Extract the content for the p2+1-th column.
                        let off = pc_.a_offset[p2 as usize] as usize;
                        if pc_.sz_row >= pc_.a_offset[p2 as usize + 1] {
                            // This is the common case where the desired content fits on the
                            // original page - where the content is not on overflow pages.
                            let dest = &mut self.a_mem[p3];
                            if t >= 12 {
                                let len = ((t - 12) / 2) as usize;
                                if len as i64 > i64::from(limit_length) {
                                    return Err(too_big());
                                }
                                dest.z.clear();
                                dest.z.extend_from_slice(&pc_.a_row[off..off + len]);
                                dest.enc = 1;
                                dest.flags = if t & 1 != 0 { MEM_STR | MEM_TERM } else { MEM_BLOB };
                            } else {
                                sqlite3_vdbe_serial_get(&pc_.a_row[off.min(pc_.a_row.len())..], t, dest);
                            }
                        } else {
                            let p5 = op.p5 & OPFLAG_BYTELENARG;
                            if (p5 != 0 && (p5 == OPFLAG_TYPEOFARG || (t >= 12 && (t & 1 == 0 || p5 == OPFLAG_BYTELENARG))))
                                || sqlite3_vdbe_serial_type_len(t) == 0
                            {
                                // Content is irrelevant for (1) the typeof() function, (2)
                                // the length(X) function if X is a blob, and (3) if the
                                // content length is zero. So we might as well use bogus
                                // content rather than reading content from disk.
                                let dest = &mut self.a_mem[p3];
                                if t >= 12 {
                                    let len = ((t - 12) / 2) as usize;
                                    dest.z.clear();
                                    dest.z.resize(len, 0);
                                    dest.flags = if t & 1 != 0 { MEM_STR } else { MEM_BLOB };
                                } else {
                                    sqlite3_vdbe_serial_get(&[0u8; 8], t, dest);
                                }
                                dest.enc = 1;
                            } else {
                                // vdbeColumnFromOverflow
                                let len = sqlite3_vdbe_serial_type_len(t);
                                if i64::from(len) > i64::from(limit_length) {
                                    return Err(too_big());
                                }
                                let bt = btree_of!(pc_);
                                let dest = &mut self.a_mem[p3];
                                if len > 4000 && pc_.key_info.is_none() {
                                    // Cache large column values that are on overflow pages.
                                    // The cache is initialized on first use and is valid as
                                    // long as the cursor has not moved.
                                    let i_off_now = bt.bc_offset(&mut pc_.cur)?;
                                    let hit = pc_.p_cache.as_ref().is_some_and(|c| {
                                        c.i_col == p2
                                            && c.cache_status == self.cache_ctr
                                            && c.col_cache_ctr == col_cache_ctr
                                            && c.i_offset == i_off_now
                                    });
                                    if !hit {
                                        let mut buf = vec![0u8; len as usize];
                                        bt.bc_payload(&mut pc_.cur, off as u32, &mut buf)?;
                                        pc_.p_cache = Some(Box::new(VdbeTxtBlbCache {
                                            p_c_value: buf,
                                            i_offset: i_off_now,
                                            i_col: p2,
                                            cache_status: self.cache_ctr,
                                            col_cache_ctr,
                                        }));
                                    }
                                    let c = pc_.p_cache.as_ref().map(|c| &c.p_c_value[..]).unwrap_or(&[]);
                                    dest.z.clear();
                                    dest.z.extend_from_slice(c);
                                    dest.flags = if t & 1 != 0 { MEM_STR | MEM_TERM } else { MEM_BLOB };
                                    dest.enc = 1;
                                } else {
                                    let buf = sqlite3_vdbe_mem_from_btree(bt, &mut pc_.cur, off as u32, len)?;
                                    sqlite3_vdbe_serial_get(&buf, t, dest);
                                    dest.enc = 1;
                                }
                            }
                        }
                        break 'col;
                    }
                }

                OP_Affinity => {
                    let z_aff = p4_text(&op.p4).unwrap_or(&[]);
                    let mut i = op.p1 as usize;
                    for &aff in z_aff {
                        if aff == 0 {
                            break;
                        }
                        let m = &mut self.a_mem[i];
                        m.apply_affinity(aff);
                        if aff == SQLITE_AFF_REAL && m.flags & MEM_INT != 0 {
                            // (3.54.1: OP_Affinity makes the integer a MEM_Real; MEM_IntReal
                            // is set by OP_MakeRecord's affinity string and by OP_TypeCheck)
                            m.r = m.i as f64;
                            m.flags |= MEM_REAL;
                            m.flags &= !(MEM_INT | MEM_STR);
                        }
                        i += 1;
                    }
                }

                OP_TypeCheck => {
                    // Apply affinities to the range of P2 registers beginning with P1. Take
                    // the affinities from the Table object in P4. If any value cannot be
                    // coerced into the correct type, then raise an error.
                    let P4::Table(t) = &op.p4 else {
                        return Err(internal("OP_TypeCheck without a table"));
                    };
                    let Some(tab) = host.table_info(&t.name) else {
                        return Err(internal("OP_TypeCheck: table not found"));
                    };
                    let mut r = op.p1 as usize;
                    let (mut i, n_col) = if op.p3 < 2 { (0, tab.cols.len()) } else { ((op.p3 - 2) as usize, (op.p3 - 1) as usize) };
                    while i < n_col {
                        let col = &tab.cols[i];
                        if col.generated && op.p3 < 2 {
                            if col.is_virtual {
                                i += 1;
                                continue;
                            }
                            if op.p3 != 0 {
                                r += 1;
                                i += 1;
                                continue;
                            }
                        }
                        let m = &mut self.a_mem[r];
                        m.apply_affinity(col.affinity);
                        if m.flags & MEM_NULL == 0 {
                            let ok = match col.e_ctype {
                                COLTYPE_BLOB => m.flags & MEM_BLOB != 0,
                                COLTYPE_INTEGER | COLTYPE_INT => m.flags & MEM_INT != 0,
                                COLTYPE_TEXT => m.flags & MEM_STR != 0,
                                COLTYPE_REAL => {
                                    if m.flags & MEM_INT != 0 {
                                        // When applying REAL affinity, if the result is still
                                        // an MEM_Int that will fit in 6 bytes, then change the
                                        // type to MEM_IntReal so that we keep the
                                        // high-resolution integer value but know that the
                                        // type really wants to be REAL.
                                        if m.i <= 140_737_488_355_327 && m.i >= -140_737_488_355_328 {
                                            m.flags |= MEM_INT_REAL;
                                            m.flags &= !MEM_INT;
                                        } else {
                                            m.r = m.i as f64;
                                            m.flags |= MEM_REAL;
                                            m.flags &= !MEM_INT;
                                        }
                                        true
                                    } else {
                                        m.flags & (MEM_REAL | MEM_INT_REAL) != 0
                                    }
                                }
                                // COLTYPE_ANY, COLTYPE_CUSTOM: accept anything
                                _ => true,
                            };
                            if !ok {
                                // vdbe_type_error:
                                const STD_TYPE: [&str; 6] = ["ANY", "BLOB", "INT", "INTEGER", "REAL", "TEXT"];
                                // vdbeMemTypeName: azTypes[sqlite3_value_type(pMem)-1]
                                let ty = if m.flags & MEM_NULL != 0 {
                                    "NULL"
                                } else if m.flags & (MEM_INT | MEM_INT_REAL) != 0 && m.flags & MEM_REAL == 0 && m.flags & MEM_INT_REAL == 0 {
                                    "INT"
                                } else if m.flags & (MEM_REAL | MEM_INT_REAL) != 0 {
                                    "REAL"
                                } else if m.flags & MEM_STR != 0 {
                                    "TEXT"
                                } else {
                                    "BLOB"
                                };
                                let msg = format!(
                                    "cannot store {} value in {} column {}.{}",
                                    ty,
                                    STD_TYPE[(usize::from(col.e_ctype)).clamp(1, 6) - 1],
                                    tab.name,
                                    col.name
                                );
                                // rc = SQLITE_CONSTRAINT_DATATYPE
                                let mut e = SqliteError::with_extended(3091, msg);
                                e.code = ErrorCode::Constraint;
                                return Err(e);
                            }
                        }
                        r += 1;
                        i += 1;
                    }
                }

                OP_MakeRecord => {
                    let n_field = op.p2 as usize;
                    let d0 = op.p1 as usize;
                    // Apply the requested affinity to all inputs
                    if let Some(z_aff) = p4_text(&op.p4) {
                        for (k, &aff) in z_aff.iter().enumerate() {
                            if aff == 0 || k >= n_field {
                                break;
                            }
                            let rec = &mut self.a_mem[d0 + k];
                            rec.apply_affinity(aff);
                            if aff == SQLITE_AFF_REAL && rec.flags & MEM_INT != 0 {
                                rec.flags |= MEM_INT_REAL;
                                rec.flags &= !MEM_INT;
                            }
                        }
                    }
                    // Loop through the elements that will make up the record to figure out
                    // how much space is required for the new record.
                    let mut n_data: u64 = 0;
                    let mut n_hdr: usize = 0;
                    let mut n_zero: i64 = 0;
                    self.u_temp.clear();
                    self.u_temp.resize(n_field, 0);
                    for k in (0..n_field).rev() {
                        let rec = &mut self.a_mem[d0 + k];
                        if rec.flags & MEM_NULL != 0 {
                            self.u_temp[k] = if rec.flags & MEM_ZERO != 0 { 10 } else { 0 };
                            n_hdr += 1;
                        } else if rec.flags & (MEM_INT | MEM_INT_REAL) != 0 {
                            // Figure out whether to use 1, 2, 4, 6 or 8 bytes.
                            let i = rec.i;
                            let uu: u64 = if i < 0 { !(i as u64) } else { i as u64 };
                            n_hdr += 1;
                            if uu <= 127 {
                                if (i & 1) == i && self.min_write_file_format >= 4 {
                                    self.u_temp[k] = 8 + uu as u32;
                                } else {
                                    n_data += 1;
                                    self.u_temp[k] = 1;
                                }
                            } else if uu <= 32767 {
                                n_data += 2;
                                self.u_temp[k] = 2;
                            } else if uu <= 8388607 {
                                n_data += 3;
                                self.u_temp[k] = 3;
                            } else if uu <= 2147483647 {
                                n_data += 4;
                                self.u_temp[k] = 4;
                            } else if uu <= 140737488355327 {
                                n_data += 6;
                                self.u_temp[k] = 5;
                            } else {
                                n_data += 8;
                                if rec.flags & MEM_INT_REAL != 0 {
                                    // If the value is IntReal and is going to take up 8 bytes
                                    // to store as an integer, then we might as well make it
                                    // an 8-byte floating point value
                                    rec.r = rec.i as f64;
                                    rec.flags &= !MEM_INT_REAL;
                                    rec.flags |= MEM_REAL;
                                    self.u_temp[k] = 7;
                                } else {
                                    self.u_temp[k] = 6;
                                }
                            }
                        } else if rec.flags & MEM_REAL != 0 {
                            n_hdr += 1;
                            n_data += 8;
                            self.u_temp[k] = 7;
                        } else {
                            let mut len = rec.z.len() as u32;
                            let mut serial_type = len * 2 + 12 + u32::from(rec.flags & MEM_STR != 0);
                            if rec.flags & MEM_ZERO != 0 {
                                serial_type += rec.n_zero as u32 * 2;
                                if n_data != 0 {
                                    rec.expand_blob();
                                    len += rec.n_zero as u32;
                                } else {
                                    n_zero += i64::from(rec.n_zero);
                                }
                            }
                            n_data += u64::from(len);
                            n_hdr += sqlite3_varint_len(u64::from(serial_type));
                            self.u_temp[k] = serial_type;
                        }
                    }
                    // EVIDENCE-OF: R-22564-11647 The header begins with a single varint
                    // which determines the total number of bytes in the header. The varint
                    // value is the size of the header in bytes including the size varint
                    // itself.
                    if n_hdr <= 126 {
                        // The common case
                        n_hdr += 1;
                    } else {
                        // Rare case of a really large header
                        let n_varint = sqlite3_varint_len(n_hdr as u64);
                        n_hdr += n_varint;
                        if n_varint < sqlite3_varint_len(n_hdr as u64) {
                            n_hdr += 1;
                        }
                    }
                    let n_byte = n_hdr as i64 + n_data as i64;
                    if n_byte + n_zero > i64::from(limit_length) {
                        return Err(too_big());
                    }
                    let mut z = std::mem::take(&mut self.scratch);
                    z.clear();
                    z.reserve(n_byte as usize);
                    put_varint(&mut z, n_hdr as u64);
                    for k in 0..n_field {
                        put_varint(&mut z, u64::from(self.u_temp[k]));
                    }
                    for k in 0..n_field {
                        let serial_type = self.u_temp[k];
                        let rec = &self.a_mem[d0 + k];
                        if serial_type <= 7 {
                            if serial_type != 0 {
                                let v: u64 = if serial_type == 7 { rec.r.to_bits() } else { rec.i as u64 };
                                let len = sqlite3_vdbe_serial_type_len(serial_type) as usize;
                                z.extend_from_slice(&v.to_be_bytes()[8 - len..]);
                            }
                        } else if serial_type >= 12 {
                            z.extend_from_slice(&rec.z);
                        }
                    }
                    let out = &mut self.a_mem[op.p3 as usize];
                    std::mem::swap(&mut out.z, &mut z);
                    self.scratch = z;
                    out.flags = MEM_BLOB;
                    if n_zero != 0 {
                        out.n_zero = n_zero as i32;
                        out.flags |= MEM_ZERO;
                    }
                }

                OP_Count => {
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    let n_entry = if op.p3 != 0 { bt.bc_row_count_est(&pc_.cur) } else { bt.bc_count(&mut pc_.cur)? };
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.flags = MEM_INT;
                    out.i = n_entry;
                    check_int!();
                }

                OP_Transaction => {
                    let (i_meta, i_gen) = if op.p2 != 0 {
                        host.begin_write_trans(op.p1 as usize, op.p2)?
                    } else {
                        host.begin_read_trans(op.p1 as usize)?
                    };
                    let p4 = match op.p4 {
                        P4::Int32(i) => i,
                        _ => 0,
                    };
                    if op.p5 != 0 && (i_meta as i32 != op.p3 || i_gen != p4) {
                        return Err(SqliteError::new(ErrorCode::Schema, "database schema has changed"));
                    }
                }

                OP_ReadCookie => {
                    let v = host.get_meta(op.p1 as usize, op.p3)?;
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.flags = MEM_INT;
                    out.i = i64::from(v as i32);
                }

                OP_ReopenIdx | OP_OpenRead | OP_OpenWrite => {
                    let ci = op.p1 as usize;
                    let mut reopened = false;
                    if op.opcode == OP_ReopenIdx
                        && let Some(c) = self.ap_csr[ci].as_deref_mut()
                        && c.pgno_root == op.p2 as u32
                    {
                        c.cur.clear_cursor();
                        reopened = true;
                    }
                    if !reopened {
                        if self.expired {
                            // "if( p->expired==1 ){ rc = SQLITE_ABORT_ROLLBACK; goto abort_due_to_error; }"
                            return Err(SqliteError::with_extended(516, "abort due to ROLLBACK"));
                        }
                        let i_db = op.p3;
                        let mut p2 = op.p2 as Pgno;
                        if op.opcode == OP_OpenWrite && op.p5 & OPFLAG_P2ISREG != 0 {
                            let m = &mut self.a_mem[op.p2 as usize];
                            m.integerify();
                            p2 = m.i as Pgno;
                        }
                        let (n_field, key_info) = match (&op.p4, &self.rt[pc]) {
                            (P4::KeyInfo(k), OpRt::KeyInfo(rt)) => (i32::from(k.n_all_field), Some(Arc::clone(rt))),
                            (P4::Int32(n), _) => (*n, None),
                            _ => (0, None),
                        };
                        self.allocate_cursor(host, ci, n_field, CURTYPE_BTREE)?;
                        let Some(bt) = host.btree(i_db as usize) else {
                            return Err(internal("database has no b-tree"));
                        };
                        let cur = bt.bc_cursor(p2, op.opcode == OP_OpenWrite, key_info.is_some());
                        let c = csr!(ci);
                        c.i_db = i_db as i8;
                        c.null_row = true;
                        c.is_ordered = true;
                        c.pgno_root = p2;
                        *c.cur = cur;
                        c.is_table = key_info.is_none();
                        c.key_info = key_info;
                    }
                    // open_cursor_set_hints:
                    let c = csr!(ci);
                    c.cur.hint_flags((op.p5 & (OPFLAG_BULKCSR | OPFLAG_SEEKEQ)) as u8);
                }

                OP_OpenDup => {
                    let ci = op.p1 as usize;
                    let (n_field, key_info, orders, is_table, pgno_root, is_ordered, btx) = {
                        let orig = csr!(op.p2);
                        orig.no_reuse = true;
                        (orig.n_field, orig.key_info.clone(), orig.orders.clone(), orig.is_table, orig.pgno_root, orig.is_ordered, orig.btx)
                    };
                    let Some(x) = btx else {
                        return Err(internal("OP_OpenDup of a cursor that is not ephemeral"));
                    };
                    self.allocate_cursor(host, ci, n_field, CURTYPE_BTREE)?;
                    let Some(e) = self.ephem.get_mut(x).and_then(|e| e.as_mut()) else {
                        return Err(internal("ephemeral b-tree is closed"));
                    };
                    e.n_ref += 1;
                    let cur = e.bt.bc_cursor(pgno_root, true, key_info.is_some());
                    let c = csr!(ci);
                    c.null_row = true;
                    c.is_ephemeral = true;
                    c.key_info = key_info;
                    c.orders = orders;
                    c.is_table = is_table;
                    c.pgno_root = pgno_root;
                    c.is_ordered = is_ordered;
                    c.btx = Some(x);
                    c.no_reuse = true;
                    *c.cur = cur;
                }

                OP_OpenAutoindex | OP_OpenEphemeral => {
                    let ci = op.p1 as usize;
                    if op.p3 > 0 {
                        // Make register P3 point to an empty string
                        self.a_mem[op.p3 as usize].z.clear();
                    }
                    let reuse = match self.ap_csr[ci].as_deref() {
                        Some(c) => !c.no_reuse && op.p2 <= c.n_field && c.btx.is_some(),
                        None => false,
                    };
                    if reuse {
                        // If the ephemeral table is already open and has no duplicates from
                        // OP_OpenDup, then erase all existing content so that the table is
                        // empty again, rather than creating a new table.
                        let (x, root) = {
                            let c = csr!(ci);
                            c.seq_count = 0;
                            c.cache_status = CACHE_STALE;
                            (c.btx.unwrap_or(0), c.pgno_root)
                        };
                        self.save_all_cursors(x, root, usize::MAX)?;
                        let Some(e) = self.ephem.get_mut(x).and_then(|e| e.as_mut()) else {
                            return Err(internal("ephemeral b-tree is closed"));
                        };
                        e.bt.clear_tree(root)?;
                    } else {
                        self.allocate_cursor(host, ci, op.p2, CURTYPE_BTREE)?;
                        let mut bt = host.open_ephemeral()?;
                        let key_info = match &self.rt[pc] {
                            OpRt::KeyInfo(k) => Some(Arc::clone(k)),
                            _ => None,
                        };
                        let (root, is_table, orders) = match &key_info {
                            Some(k) => {
                                let root = bt.create_index()?;
                                (root, false, key_sort_orders(k))
                            }
                            None => (bt.create_table()?, true, Vec::new()),
                        };
                        let cur = bt.bc_cursor(root, true, key_info.is_some());
                        let x = match self.ephem.iter().position(Option::is_none) {
                            Some(x) => {
                                self.ephem[x] = Some(EphemBt { bt, n_ref: 1 });
                                x
                            }
                            None => {
                                self.ephem.push(Some(EphemBt { bt, n_ref: 1 }));
                                self.ephem.len() - 1
                            }
                        };
                        let c = csr!(ci);
                        c.is_ephemeral = true;
                        c.btx = Some(x);
                        c.key_info = key_info;
                        c.orders = orders;
                        c.pgno_root = root;
                        c.is_table = is_table;
                        *c.cur = cur;
                        c.is_ordered = op.p5 != BTREE_UNORDERED;
                    }
                    csr!(ci).null_row = true;
                }

                OP_SorterOpen => {
                    let ci = op.p1 as usize;
                    self.allocate_cursor(host, ci, op.p2, CURTYPE_SORTER)?;
                    let OpRt::KeyInfo(k) = &self.rt[pc] else {
                        return Err(internal("OP_SorterOpen without KeyInfo"));
                    };
                    let (pgsz, cache_size, temp_in_memory) = host.sorter_config();
                    let sorter = VdbeSorter::init(k, op.p3, pgsz, cache_size, temp_in_memory);
                    let c = csr!(ci);
                    c.key_info = Some(Arc::clone(k));
                    c.sorter = Some(Box::new(sorter));
                }

                OP_SequenceTest => {
                    let c = csr!(op.p1);
                    let v = c.seq_count;
                    c.seq_count += 1;
                    if v == 0 {
                        jump!(op.p2);
                    }
                }

                OP_OpenPseudo => {
                    let ci = op.p1 as usize;
                    self.allocate_cursor(host, ci, op.p3, CURTYPE_PSEUDO)?;
                    let c = csr!(ci);
                    c.null_row = true;
                    c.seek_result = op.p2;
                    c.is_table = true;
                    c.cur.e_state = sqliters_btree::btcursor::CURSOR_VALID;
                }

                OP_Close => {
                    self.free_cursor(host, op.p1 as usize);
                }

                OP_ColumnsUsed => {}

                OP_SeekLT | OP_SeekLE | OP_SeekGE | OP_SeekGT => {
                    let pc_ = csr!(op.p1);
                    let mut oc = op.opcode;
                    let mut eq_only = false;
                    pc_.null_row = false;
                    pc_.deferred_moveto = false;
                    pc_.cache_status = CACHE_STALE;
                    let bt = btree_of!(pc_);
                    let mut res: i32;
                    let mut skip_to_not_found = false;
                    if pc_.is_table {
                        // The OPFLAG_SEEKEQ/BTREE_SEEK_EQ flag is only set on index cursors.
                        // The input value in P3 might be of any type: integer, real, string,
                        // blob, or NULL. But it needs to be an integer before we can do the
                        // seek, so convert it.
                        let in3 = &mut self.a_mem[op.p3 as usize];
                        let flags3 = in3.flags;
                        if flags3 & (MEM_INT | MEM_REAL | MEM_INT_REAL | MEM_STR) == MEM_STR {
                            in3.apply_numeric_affinity(false);
                        }
                        let i_key = in3.int_value(); // Get the integer key value
                        let new_type = in3.flags; // Record the type after applying numeric affinity
                        in3.flags = flags3; // But convert the type back to its original
                        res = 0;
                        // If the P3 value could not be converted into an integer without
                        // loss of information, then special processing is required...
                        let mut done = false;
                        if new_type & (MEM_INT | MEM_INT_REAL) == 0 {
                            if new_type & MEM_REAL == 0 {
                                if new_type & MEM_NULL != 0 || oc >= OP_SeekGE {
                                    jump!(op.p2);
                                }
                                res = i32::from(bt.bc_last(&mut pc_.cur)?);
                                skip_to_not_found = true;
                                done = true;
                            } else {
                                let c = sqlite3_int_float_compare(i_key, in3.r);
                                // If the approximation iKey is larger than the actual real
                                // search term, substitute >= for > and < for <=. e.g. if the
                                // search term is 4.9 and the integer approximation 5:
                                //        (x >  4.9)    ->     (x >= 5)
                                //        (x <= 4.9)    ->     (x <  5)
                                if c > 0 {
                                    if (oc & 0x0001) == (OP_SeekGT & 0x0001) {
                                        oc -= 1;
                                    }
                                }
                                // If the approximation iKey is smaller than the actual real
                                // search term, substitute <= for < and > for >=.
                                else if c < 0 && (oc & 0x0001) == (OP_SeekLT & 0x0001) {
                                    oc += 1;
                                }
                            }
                        }
                        if !done {
                            res = bt.bc_table_moveto(&mut pc_.cur, i_key, false)?;
                            pc_.moveto_target = i_key; // Used by OP_Delete
                        }
                    } else {
                        // For a cursor with the OPFLAG_SEEKEQ/BTREE_SEEK_EQ hint, only the
                        // OP_SeekGE and OP_SeekLE opcodes are allowed, and these must be
                        // immediately followed by an OP_IdxGT or OP_IdxLT opcode,
                        // respectively, with the same key.
                        if pc_.cur.has_hint(BTREE_SEEK_EQ) {
                            eq_only = true;
                        }
                        let n_field = match op.p4 {
                            P4::Int32(n) => n as usize,
                            _ => 0,
                        };
                        let Some(ki) = pc_.key_info.as_deref() else {
                            return Err(internal("index cursor without KeyInfo"));
                        };
                        // The next line of code computes as follows, only faster:
                        //   if( oc==OP_SeekGT || oc==OP_SeekLE ){ r.default_rc = -1; }
                        //   else{ r.default_rc = +1; }
                        let default_rc: i8 = if (1 & (oc - OP_SeekLT)) != 0 { -1 } else { 1 };
                        let p3 = op.p3 as usize;
                        let mut r = UnpackedRecord::new(ki, &self.a_mem[p3..p3 + n_field], n_field as u16, default_rc);
                        r.find_compare();
                        res = bt.bc_index_moveto(&mut pc_.cur, &mut r)?;
                        if eq_only && !r.eq_seen {
                            skip_to_not_found = true;
                        }
                    }
                    if !skip_to_not_found {
                        if oc >= OP_SeekGE {
                            if res < 0 || (res == 0 && oc == OP_SeekGT) {
                                res = i32::from(!bt.bc_next(&mut pc_.cur)?);
                            } else {
                                res = 0;
                            }
                        } else if res > 0 || (res == 0 && oc == OP_SeekLT) {
                            res = i32::from(!bt.bc_previous(&mut pc_.cur)?);
                        } else {
                            // res might be negative because the table is empty. Check to see
                            // if this is the case.
                            res = i32::from(pc_.cur.eof());
                        }
                    }
                    // seek_not_found:
                    if res != 0 {
                        jump!(op.p2);
                    } else if eq_only {
                        pc += 1; // Skip the OP_IdxLt or OP_IdxGT that follows
                    }
                }

                OP_SeekScan => {
                    // pOp[1] is the OP_SeekGE that follows
                    let nxt = &a_op[pc + 1];
                    let pc_ = csr!(nxt.p1);
                    if pc_.cur.is_valid() {
                        let mut n_step = op.p1;
                        let Some(ki) = pc_.key_info.as_deref() else {
                            return Err(internal("index cursor without KeyInfo"));
                        };
                        let n_field = match nxt.p4 {
                            P4::Int32(n) => n as usize,
                            _ => 0,
                        };
                        let p3 = nxt.p3 as usize;
                        let bt = btree_of!(pc_);
                        // 0: fall through to the OP_SeekGE; 1: jump to P2 of this opcode;
                        // 2: jump to P2 of the OP_SeekGE
                        let outcome: u8;
                        loop {
                            let mut r = UnpackedRecord::new(ki, &self.a_mem[p3..p3 + n_field], n_field as u16, 0);
                            let res = sqlite3_vdbe_idx_key_compare(bt, &mut pc_.cur, &mut r, &mut self.scratch)?;
                            if res > 0 && op.p5 == 0 {
                                outcome = 2;
                                break;
                            }
                            if res >= 0 {
                                outcome = 1;
                                break;
                            }
                            if n_step <= 0 {
                                outcome = 0;
                                break;
                            }
                            n_step -= 1;
                            pc_.cache_status = CACHE_STALE;
                            if !bt.bc_next(&mut pc_.cur)? {
                                outcome = 2;
                                break;
                            }
                        }
                        if outcome == 1 {
                            jump!(op.p2);
                        } else if outcome == 2 {
                            // seekscan_search_fail: Jump to SeekGE.P2, ending the loop
                            jump!(nxt.p2);
                        }
                    }
                }

                OP_SeekHit => {
                    let c = csr!(op.p1);
                    if i32::from(c.seek_hit) < op.p2 {
                        c.seek_hit = op.p2 as u16;
                    } else if i32::from(c.seek_hit) > op.p3 {
                        c.seek_hit = op.p3 as u16;
                    }
                }

                OP_IfNoHope | OP_NoConflict | OP_NotFound | OP_Found => {
                    let p4i = match op.p4 {
                        P4::Int32(n) => n,
                        _ => 0,
                    };
                    let pc_ = csr!(op.p1);
                    if op.opcode == OP_IfNoHope && i32::from(pc_.seek_hit) >= p4i {
                        pc += 1;
                        continue;
                    }
                    let Some(ki) = pc_.key_info.clone() else {
                        return Err(internal("index cursor without KeyInfo"));
                    };
                    let bt = btree_of!(pc_);
                    let p3 = op.p3 as usize;
                    let n_field = p4i.max(0) as usize;
                    if n_field > 0 {
                        let mut r = UnpackedRecord::new(&ki, &self.a_mem[p3..p3 + n_field], n_field as u16, 0);
                        r.find_compare();
                        pc_.seek_result = bt.bc_index_moveto(&mut pc_.cur, &mut r)?;
                    } else {
                        // Composite key generated by OP_MakeRecord
                        self.a_mem[p3].expand_blob();
                        let mems = sqlite3_vdbe_record_unpack(&ki, &self.a_mem[p3].z, usize::from(ki.n_key_field) + 1);
                        let mut r = UnpackedRecord::new(&ki, &mems, mems.len() as u16, 0);
                        if mems.is_empty() {
                            return Err(corrupt());
                        }
                        r.find_compare();
                        pc_.seek_result = bt.bc_index_moveto(&mut pc_.cur, &mut r)?;
                    }
                    let already_exists = pc_.seek_result == 0;
                    pc_.null_row = !already_exists;
                    pc_.deferred_moveto = false;
                    pc_.cache_status = CACHE_STALE;
                    if op.opcode == OP_Found {
                        if already_exists {
                            jump!(op.p2);
                        }
                    } else {
                        if !already_exists {
                            jump!(op.p2);
                        }
                        if op.opcode == OP_NoConflict {
                            // For the OP_NoConflict opcode, take the jump if any of the input
                            // fields are NULL, since any key with a NULL will conflict with
                            // any other key.
                            if self.a_mem[p3..p3 + n_field].iter().any(|m| m.flags & MEM_NULL != 0) {
                                jump!(op.p2);
                            }
                        }
                        if op.opcode == OP_IfNoHope {
                            pc_.seek_hit = p4i as u16;
                        }
                    }
                }

                OP_SeekRowid | OP_NotExists => {
                    let in3 = &self.a_mem[op.p3 as usize];
                    let i_key: i64;
                    if op.opcode == OP_SeekRowid && in3.flags & (MEM_INT | MEM_INT_REAL) == 0 {
                        // If pIn3->u.i does not contain an integer, compute iKey as the
                        // integer value of pIn3. Jump to P2 if pIn3 cannot be converted into
                        // an integer without loss of information. Take care to avoid changing
                        // the datatype of pIn3, however, as it is used by other parts of the
                        // prepared statement.
                        let mut x = in3.clone();
                        if x.flags & MEM_STR != 0 {
                            x.apply_numeric_affinity(true);
                        }
                        if x.flags & MEM_INT != 0 {
                            i_key = x.i;
                        } else if x.flags & MEM_REAL == 0 || x.r < -9223372036854775808.0 || x.r > 9223372036854774784.0 || {
                            let k = sqlite3_real_to_i64(x.r);
                            k as f64 != x.r
                        } {
                            jump!(op.p2);
                        } else {
                            i_key = sqlite3_real_to_i64(x.r);
                        }
                    } else {
                        i_key = in3.i;
                    }
                    // notExistsWithKey:
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    let r = bt.bc_table_moveto(&mut pc_.cur, i_key, false);
                    pc_.moveto_target = i_key; // Used by OP_Delete
                    pc_.null_row = false;
                    pc_.cache_status = CACHE_STALE;
                    pc_.deferred_moveto = false;
                    let res = r?;
                    pc_.seek_result = res;
                    if res != 0 {
                        if op.p2 == 0 {
                            return Err(corrupt());
                        }
                        jump!(op.p2);
                    }
                }

                OP_Sequence => {
                    let c = csr!(op.p1);
                    let v = c.seq_count;
                    c.seq_count += 1;
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.flags = MEM_INT;
                    out.i = v;
                }

                OP_NewRowid => {
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    let mut v: i64 = 0;
                    const MAX_ROWID: i64 = i64::MAX;
                    if !pc_.use_random_rowid {
                        let res = bt.bc_last(&mut pc_.cur)?;
                        if res {
                            v = 1; // IMP: R-61914-48074
                        } else {
                            v = bt.bc_integer_key(&mut pc_.cur)?;
                            if v >= MAX_ROWID {
                                pc_.use_random_rowid = true;
                            } else {
                                v += 1; // IMP: R-29538-34987
                            }
                        }
                    }
                    if op.p3 != 0 {
                        // (AUTOINCREMENT: the register of the largest rowid so far; a trigger
                        // program uses the register of the top-level frame — frames are not
                        // ported yet, programs with sub-programs are not run)
                        let m = &mut self.a_mem[op.p3 as usize];
                        m.integerify();
                        if m.i == MAX_ROWID || pc_.use_random_rowid {
                            return Err(SqliteError::new(ErrorCode::Full, "database or disk is full")); // IMP: R-17817-00630
                        }
                        if v < m.i + 1 {
                            v = m.i + 1;
                        }
                        m.i = v;
                    }
                    if pc_.use_random_rowid {
                        // IMP: R-07677-41881 The largest ROWID is in use: random rowids until
                        // an unused one is found.
                        let mut cnt = 0;
                        let mut res;
                        loop {
                            v = host.randomness();
                            v &= MAX_ROWID >> 1;
                            v += 1; // Ensure that v is greater than zero
                            let bt = btree_of!(pc_);
                            res = bt.bc_table_moveto(&mut pc_.cur, v, false)?;
                            cnt += 1;
                            if !(res == 0 && cnt < 100) {
                                break;
                            }
                        }
                        if res == 0 {
                            return Err(SqliteError::new(ErrorCode::Full, "database or disk is full")); // IMP: R-38219-53002
                        }
                    }
                    pc_.deferred_moveto = false;
                    pc_.cache_status = CACHE_STALE;
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.flags = MEM_INT;
                    out.i = v;
                }

                OP_Insert => {
                    let ci = op.p1 as usize;
                    let n_key = self.a_mem[op.p3 as usize].i;
                    if op.p5 & OPFLAG_PREFORMAT != 0 {
                        return Err(internal("OPFLAG_PREFORMAT is not ported"));
                    }
                    if op.p5 & OPFLAG_NCHANGE != 0 {
                        self.n_change += 1;
                        if op.p5 & OPFLAG_LASTROWID != 0 {
                            host.set_last_rowid(n_key);
                        }
                    }
                    self.a_mem[op.p2 as usize].expand_blob();
                    save_cursors!(ci);
                    let c = csr!(ci);
                    let seek_result = if op.p5 & OPFLAG_USESEEKRESULT != 0 { c.seek_result } else { 0 };
                    let bt = btree_of!(c);
                    let rc = bt.bc_insert(
                        &mut c.cur,
                        n_key,
                        &self.a_mem[op.p2 as usize].z,
                        (op.p5 & (OPFLAG_APPEND | OPFLAG_SAVEPOSITION)) as u8,
                        seek_result,
                        None,
                    );
                    c.deferred_moveto = false;
                    c.cache_status = CACHE_STALE;
                    col_cache_ctr = col_cache_ctr.wrapping_add(1);
                    rc?;
                }

                OP_Delete => {
                    let ci = op.p1 as usize;
                    save_cursors!(ci);
                    let c = csr!(ci);
                    let bt = btree_of!(c);
                    if c.cur.e_state >= CURSOR_REQUIRESEEK && bt.bc_needs_index_restore(&c.cur) {
                        // (sqlite3BtreeDelete: btreeRestoreCursorPosition of an index cursor)
                        restore_cursor_position(bt, c)?;
                    }
                    let rc = bt.bc_delete(&mut c.cur, op.p5 as u8);
                    c.cache_status = CACHE_STALE;
                    col_cache_ctr = col_cache_ctr.wrapping_add(1);
                    c.seek_result = 0;
                    rc?;
                    if op.p2 as u16 & OPFLAG_NCHANGE != 0 {
                        self.n_change += 1;
                    }
                }

                OP_SorterCompare => {
                    let c = csr!(op.p1);
                    let n_key_col = match op.p4 {
                        P4::Int32(n) => n,
                        _ => 0,
                    };
                    let (Some(s), Some(ki)) = (c.sorter.as_ref(), c.key_info.as_ref()) else {
                        return Err(internal("not a sorter"));
                    };
                    let res = s.compare_with(ki, &self.a_mem[op.p3 as usize].z, n_key_col);
                    if res != 0 {
                        jump!(op.p2);
                    }
                }

                OP_SorterData => {
                    {
                        let c = csr!(op.p1);
                        let Some(s) = c.sorter.as_ref() else {
                            return Err(internal("not a sorter"));
                        };
                        let out = &mut self.a_mem[op.p2 as usize];
                        out.z.clear();
                        out.z.extend_from_slice(s.rowkey());
                        out.set_type_flag(MEM_BLOB);
                    }
                    csr!(op.p3).cache_status = CACHE_STALE;
                }

                OP_RowData => {
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    let n = bt.bc_payload_size(&mut pc_.cur)?;
                    if i64::from(n) > i64::from(limit_length) {
                        return Err(too_big());
                    }
                    let out = &mut self.a_mem[op.p2 as usize];
                    sqlite3_vdbe_mem_from_btree_zero_offset(bt, &mut pc_.cur, n, &mut out.z)?;
                    out.flags = MEM_BLOB;
                }

                OP_Rowid => {
                    let pc_ = csr!(op.p1);
                    let p2 = op.p2 as usize;
                    if pc_.null_row {
                        self.a_mem[p2].set_null();
                    } else {
                        let v: i64;
                        if pc_.deferred_moveto {
                            v = pc_.moveto_target;
                        } else {
                            let bt = btree_of!(pc_);
                            cursor_restore(bt, pc_)?;
                            if pc_.null_row {
                                self.a_mem[p2].set_null();
                                pc += 1;
                                continue;
                            }
                            v = bt.bc_integer_key(&mut pc_.cur)?;
                        }
                        let out = &mut self.a_mem[p2];
                        out.flags = MEM_INT;
                        out.i = v;
                    }
                }

                OP_NullRow => {
                    let ci = op.p1 as usize;
                    if self.ap_csr[ci].is_none() {
                        // If the cursor is not already open, create a special kind of
                        // pseudo-cursor that always gives null rows.
                        self.allocate_cursor(host, ci, 1, CURTYPE_PSEUDO)?;
                        let c = csr!(ci);
                        c.seek_result = 0;
                        c.is_table = true;
                        c.no_reuse = true;
                        c.cur.e_state = sqliters_btree::btcursor::CURSOR_VALID;
                    }
                    let c = csr!(ci);
                    c.null_row = op.p2 == 0;
                    c.cache_status = CACHE_STALE;
                    if c.e_cur_type == CURTYPE_BTREE {
                        c.cur.clear_cursor();
                    }
                }

                OP_SeekEnd | OP_Last => {
                    let pc_ = csr!(op.p1);
                    if op.opcode == OP_SeekEnd {
                        pc_.seek_result = -1;
                        if pc_.cur.is_valid() {
                            pc += 1;
                            continue;
                        }
                    }
                    let bt = btree_of!(pc_);
                    let r = bt.bc_last(&mut pc_.cur);
                    pc_.deferred_moveto = false;
                    pc_.cache_status = CACHE_STALE;
                    let res = r?;
                    pc_.null_row = res;
                    if op.p2 > 0 && res {
                        jump!(op.p2);
                    }
                }

                OP_IfSizeBetween => {
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    let res = bt.bc_first(&mut pc_.cur)?;
                    let sz: i64 = if res {
                        -1 // -Infinity encoding
                    } else {
                        i64::from(sqlite3_log_est(bt.bc_row_count_est(&pc_.cur) as u64))
                    };
                    let p4 = match op.p4 {
                        P4::Int32(n) => n,
                        _ => 0,
                    };
                    if sz >= i64::from(op.p3) && sz <= i64::from(p4) {
                        jump!(op.p2);
                    }
                }

                OP_SorterSort | OP_Sort | OP_Rewind => {
                    if op.opcode != OP_Rewind {
                        self.a_counter[SQLITE_STMTSTATUS_SORT] += 1;
                    }
                    let pc_ = csr!(op.p1);
                    let res: bool;
                    if pc_.e_cur_type == CURTYPE_SORTER {
                        let Some(s) = pc_.sorter.as_mut() else {
                            return Err(internal("not a sorter"));
                        };
                        res = s.rewind();
                    } else {
                        let bt = btree_of!(pc_);
                        let r = bt.bc_first(&mut pc_.cur);
                        pc_.deferred_moveto = false;
                        pc_.cache_status = CACHE_STALE;
                        res = r?;
                    }
                    pc_.null_row = res;
                    if op.p2 > 0 && res {
                        jump!(op.p2);
                    }
                }

                OP_IfEmpty => {
                    let pc_ = csr!(op.p1);
                    let bt = btree_of!(pc_);
                    if bt.bc_is_empty(&mut pc_.cur)? {
                        jump!(op.p2);
                    }
                }

                OP_SorterNext | OP_Prev | OP_Next => {
                    let pc_ = csr!(op.p1);
                    let ok: bool;
                    if op.opcode == OP_SorterNext {
                        let Some(s) = pc_.sorter.as_mut() else {
                            return Err(internal("not a sorter"));
                        };
                        ok = s.next();
                    } else {
                        let bt = btree_of!(pc_);
                        if pc_.cur.e_state >= CURSOR_REQUIRESEEK {
                            if let Err(e) = restore_cursor_position(bt, pc_) {
                                pc_.cache_status = CACHE_STALE;
                                return Err(e);
                            }
                        }
                        let r = if op.opcode == OP_Prev { bt.bc_previous(&mut pc_.cur) } else { bt.bc_next(&mut pc_.cur) };
                        pc_.cache_status = CACHE_STALE;
                        ok = r?;
                    }
                    // next_tail:
                    pc_.cache_status = CACHE_STALE;
                    if ok {
                        pc_.null_row = false;
                        self.a_counter[(op.p5 as usize).min(8)] += 1;
                        jump_check!(op.p2);
                    }
                    pc_.null_row = true;
                    check_int!();
                }

                OP_IdxInsert => {
                    let ci = op.p1 as usize;
                    if op.p5 & OPFLAG_PREFORMAT != 0 {
                        return Err(internal("OPFLAG_PREFORMAT is not ported"));
                    }
                    if op.p5 & OPFLAG_NCHANGE != 0 {
                        self.n_change += 1;
                    }
                    self.a_mem[op.p2 as usize].expand_blob();
                    save_cursors!(ci);
                    let c = csr!(ci);
                    let Some(ki) = c.key_info.clone() else {
                        return Err(internal("index cursor without KeyInfo"));
                    };
                    let seek_result = if op.p5 & OPFLAG_USESEEKRESULT != 0 { c.seek_result } else { 0 };
                    let flags = (op.p5 & (OPFLAG_APPEND | OPFLAG_SAVEPOSITION)) as u8;
                    let bt = btree_of!(c);
                    let n_mem = match op.p4 {
                        P4::Int32(n) => n.max(0) as usize,
                        _ => 0,
                    };
                    let p3 = op.p3 as usize;
                    let rc = if n_mem > 0 {
                        // "r.aMem = pX->aMem; r.nField = pX->nMem; r.default_rc = 0"
                        let mut r = UnpackedRecord::new(&ki, &self.a_mem[p3..p3 + n_mem], n_mem as u16, 0);
                        r.find_compare();
                        bt.bc_insert(&mut c.cur, 0, &self.a_mem[op.p2 as usize].z, flags, seek_result, Some(&mut r))
                    } else {
                        // btreeMoveto: sqlite3VdbeRecordUnpack of the key
                        let mems = sqlite3_vdbe_record_unpack(&ki, &self.a_mem[op.p2 as usize].z, usize::from(ki.n_key_field) + 1);
                        if seek_result == 0 && (mems.is_empty() || mems.len() > usize::from(ki.n_all_field)) {
                            return Err(corrupt());
                        }
                        let mut r = UnpackedRecord::new(&ki, &mems, mems.len() as u16, 0);
                        r.find_compare();
                        bt.bc_insert(&mut c.cur, 0, &self.a_mem[op.p2 as usize].z, flags, seek_result, Some(&mut r))
                    };
                    c.cache_status = CACHE_STALE;
                    rc?;
                }

                OP_IdxDelete => {
                    let ci = op.p1 as usize;
                    save_cursors!(ci);
                    let c = csr!(ci);
                    let Some(ki) = c.key_info.clone() else {
                        return Err(internal("index cursor without KeyInfo"));
                    };
                    let bt = btree_of!(c);
                    let p2 = op.p2 as usize;
                    let n_field = op.p5 as usize;
                    let res = {
                        let mut r = UnpackedRecord::new(&ki, &self.a_mem[p2..p2 + n_field], n_field as u16, 0);
                        r.find_compare();
                        bt.bc_index_moveto(&mut c.cur, &mut r)?
                    };
                    if res != 0 {
                        // sqlite3VdbeFindIndexKey: the search among the neighbours for an index
                        // on expressions or virtual columns is not ported (mask==0 here: "do
                        // not search for a match").
                        if !host.writable_schema() {
                            // sqlite3ReportError(SQLITE_CORRUPT_INDEX, __LINE__, "index corruption")
                            return Err(SqliteError::with_extended(779, "database disk image is malformed"));
                        }
                        c.cache_status = CACHE_STALE;
                        c.seek_result = 0;
                        pc += 1;
                        continue;
                    }
                    if op.p3 != 0 {
                        // vdbeIndexKeyCompare: the entry equals the new key byte for byte
                        let m = &self.a_mem[op.p3 as usize];
                        if m.flags & MEM_BLOB != 0 {
                            let n_key = bt.bc_payload_size(&mut c.cur)?;
                            if n_key as usize == m.z.len() {
                                let mut k = vec![0u8; n_key as usize];
                                bt.bc_payload(&mut c.cur, 0, &mut k)?;
                                if k == m.z {
                                    self.a_mem[op.p3 as usize].set_null();
                                    pc += 1;
                                    continue;
                                }
                            }
                        }
                    }
                    bt.bc_delete(&mut c.cur, BTREE_AUXDELETE)?;
                    c.cache_status = CACHE_STALE;
                    c.seek_result = 0;
                }

                OP_Clear => {
                    // sqlite3BtreeClearTable(db->aDb[pOp->p2].pBt, (u32)pOp->p1, &nChange)
                    self.save_db_cursors(host, op.p2 as i8, op.p1 as Pgno, usize::MAX)?;
                    let Some(bt) = host.btree(op.p2 as usize) else {
                        return Err(internal("database has no b-tree"));
                    };
                    let n = bt.bc_clear_table(op.p1 as Pgno, op.p3 != 0)? as i64;
                    if op.p3 != 0 {
                        self.n_change += n;
                        if op.p3 > 0 {
                            self.a_mem[op.p3 as usize].i += n;
                        }
                    }
                }

                OP_ResetCount => {
                    // sqlite3VdbeSetChanges(db, p->nChange); p->nChange = 0;
                    host.set_changes(self.n_change);
                    self.n_change = 0;
                }

                OP_SorterInsert => {
                    self.a_mem[op.p2 as usize].expand_blob();
                    let c = csr!(op.p1);
                    let Some(s) = c.sorter.as_mut() else {
                        return Err(internal("not a sorter"));
                    };
                    s.write(&self.a_mem[op.p2 as usize].z);
                }

                OP_DeferredSeek | OP_IdxRowid => {
                    let ci = op.p1 as usize;
                    let rowid: Option<i64> = {
                        let pc_ = csr!(ci);
                        let bt = btree_of!(pc_);
                        // The IdxRowid and Seek opcodes are combined because of the
                        // commonality of sqlite3VdbeCursorRestore() and
                        // sqlite3VdbeIdxRowid().
                        cursor_restore(bt, pc_)?;
                        if !pc_.null_row { Some(sqlite3_vdbe_idx_rowid(bt, &mut pc_.cur, &mut self.scratch)?) } else { None }
                    };
                    match rowid {
                        Some(rowid) => {
                            if op.opcode == OP_DeferredSeek {
                                let t = csr!(op.p3);
                                t.null_row = false;
                                t.moveto_target = rowid;
                                t.deferred_moveto = true;
                                t.cache_status = CACHE_STALE;
                                t.alt_map_op = if matches!(op.p4, P4::IntArray(_)) { Some(pc) } else { None };
                                t.p_alt_cursor = Some(ci);
                            } else {
                                let out = &mut self.a_mem[op.p2 as usize];
                                out.flags = MEM_INT;
                                out.i = rowid;
                            }
                        }
                        None => self.a_mem[op.p2 as usize].set_null(),
                    }
                }

                OP_FinishSeek => {
                    let pc_ = csr!(op.p1);
                    if pc_.deferred_moveto {
                        let bt = btree_of!(pc_);
                        finish_moveto(bt, pc_)?;
                    }
                }

                OP_IdxLE | OP_IdxGT | OP_IdxLT | OP_IdxGE => {
                    let pc_ = csr!(op.p1);
                    let Some(ki) = pc_.key_info.as_deref() else {
                        return Err(internal("index cursor without KeyInfo"));
                    };
                    let n_field = match op.p4 {
                        P4::Int32(n) => n as usize,
                        _ => 0,
                    };
                    let default_rc: i8 = if op.opcode < OP_IdxLT { -1 } else { 0 };
                    let p3 = op.p3 as usize;
                    let bt = btree_of!(pc_);
                    let mut r = UnpackedRecord::new(ki, &self.a_mem[p3..p3 + n_field], n_field as u16, default_rc);
                    // Inlined version of sqlite3VdbeIdxKeyCompare()
                    let mut res = sqlite3_vdbe_idx_key_compare(bt, &mut pc_.cur, &mut r, &mut self.scratch)?;
                    // End of inlined sqlite3VdbeIdxKeyCompare()
                    if (op.opcode & 1) == (OP_IdxLT & 1) {
                        res = -res;
                    } else {
                        res += 1;
                    }
                    if res > 0 {
                        jump!(op.p2);
                    }
                }

                OP_ResetSorter => {
                    let ci = op.p1 as usize;
                    let c = csr!(ci);
                    if c.e_cur_type == CURTYPE_SORTER {
                        if let Some(s) = c.sorter.as_mut() {
                            s.reset();
                        }
                    } else {
                        let (Some(x), root) = (c.btx, c.pgno_root) else {
                            return Err(internal("OP_ResetSorter on a table that is not ephemeral"));
                        };
                        // sqlite3BtreeClearTableOfCursor
                        self.save_all_cursors(x, root, usize::MAX)?;
                        let Some(e) = self.ephem.get_mut(x).and_then(|e| e.as_mut()) else {
                            return Err(internal("ephemeral b-tree is closed"));
                        };
                        e.bt.clear_tree(root)?;
                        e.bt.statement_end();
                    }
                }

                OP_RowSetAdd => {
                    let p1 = op.p1 as usize;
                    let v = self.a_mem[op.p2 as usize].i;
                    self.rowset_of(p1).insert(v);
                }

                OP_RowSetRead => {
                    let p1 = op.p1 as usize;
                    let val = if self.a_mem[p1].flags & MEM_BLOB == 0 { None } else { self.rowset_of(p1).next() };
                    match val {
                        None => {
                            // The boolean index is empty
                            self.a_mem[p1].set_null();
                            jump_check!(op.p2);
                        }
                        Some(v) => {
                            // A value was pulled from the index
                            self.a_mem[op.p3 as usize].set_int64(v);
                        }
                    }
                    check_int!();
                }

                OP_RowSetTest => {
                    let p1 = op.p1 as usize;
                    let i_set = match op.p4 {
                        P4::Int32(n) => n,
                        _ => 0,
                    };
                    let v = self.a_mem[op.p3 as usize].i;
                    let rs = self.rowset_of(p1);
                    if i_set != 0 && rs.test(i_set, v) {
                        jump!(op.p2);
                    }
                    if i_set >= 0 {
                        rs.insert(v);
                    }
                }

                OP_MemMax => {
                    self.a_mem[op.p1 as usize].integerify();
                    self.a_mem[op.p2 as usize].integerify();
                    let v2 = self.a_mem[op.p2 as usize].i;
                    let m1 = &mut self.a_mem[op.p1 as usize];
                    if m1.i < v2 {
                        m1.i = v2;
                    }
                }

                OP_IfPos => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    if m.i > 0 {
                        m.i -= i64::from(op.p3);
                        jump!(op.p2);
                    }
                }

                OP_OffsetLimit => {
                    let x = self.a_mem[op.p1 as usize].i;
                    let y = self.a_mem[op.p3 as usize].i;
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.flags = MEM_INT;
                    out.i = if x <= 0 {
                        // If the LIMIT is less than or equal to zero, loop forever. This is
                        // documented. But also, if the LIMIT+OFFSET exceeds 2^63 then also
                        // loop forever. This is undocumented.
                        -1
                    } else {
                        x.checked_add(if y > 0 { y } else { 0 }).unwrap_or(-1)
                    };
                }

                OP_IfNotZero => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    if m.i != 0 {
                        if m.i > 0 {
                            m.i -= 1;
                        }
                        jump!(op.p2);
                    }
                }

                OP_DecrJumpZero => {
                    let m = &mut self.a_mem[op.p1 as usize];
                    if m.i > SMALLEST_INT64 {
                        m.i -= 1;
                    }
                    if m.i == 0 {
                        jump!(op.p2);
                    }
                }

                OP_AggInverse | OP_AggStep | OP_AggStep1 => {
                    let (func, argc): (&Arc<FuncDef>, usize) = match &op.p4 {
                        P4::FuncDef(f) => (f, op.p5 as usize),
                        P4::FuncCtx(c) => (&c.p_func, usize::from(c.argc)),
                        _ => return Err(internal("OP_AggStep without a function")),
                    };
                    let reg = op.p3 as usize;
                    if self.a_agg.len() <= reg {
                        self.a_agg.resize_with(reg + 1, || None);
                    }
                    if self.a_mem[reg].flags & MEM_AGG == 0 || self.a_agg[reg].is_none() {
                        let ctx = self.agg_new(host, func, argc, pc)?;
                        self.a_agg[reg] = Some(ctx);
                        self.a_mem[reg].z.clear();
                        self.a_mem[reg].flags = MEM_AGG;
                    }
                    let p2 = op.p2 as usize;
                    let mut skip = false;
                    match self.a_agg[reg].as_mut() {
                        Some(AggCtx::MinMax { best, is_max, coll }) => {
                            // minmaxStep
                            let arg = &self.a_mem[p2];
                            if arg.flags & MEM_NULL != 0 {
                                if best.is_some() {
                                    skip = true;
                                }
                            } else if let Some(b) = best {
                                let cmp = sqlite3_mem_compare(b, arg, coll.as_deref());
                                if (*is_max && cmp < 0) || (!*is_max && cmp > 0) {
                                    *b = arg.clone();
                                } else {
                                    skip = true;
                                }
                            } else {
                                *best = Some(arg.clone());
                            }
                        }
                        Some(AggCtx::Host(Some(a))) => {
                            let id = *a;
                            let args: Vec<RuntimeValue> = self.a_mem[p2..p2 + argc].iter().map(Mem::to_runtime_value).collect();
                            if op.p1 != 0 { host.agg_inverse(id, &args)? } else { host.agg_step(id, &args)? }
                        }
                        _ => return Err(internal("aggregate context lost")),
                    }
                    if skip && pc > 0 {
                        // sqlite3SkipAccumulatorLoad
                        let i = a_op[pc - 1].p1;
                        if i != 0 {
                            self.a_mem[i as usize].set_int64(1);
                        }
                    }
                }

                OP_AggValue | OP_AggFinal => {
                    let func: &Arc<FuncDef> = match &op.p4 {
                        P4::FuncDef(f) => f,
                        P4::FuncCtx(c) => &c.p_func,
                        _ => return Err(internal("OP_AggFinal without a function")),
                    };
                    let reg = op.p1 as usize;
                    if self.a_agg.len() <= reg {
                        self.a_agg.resize_with(reg + 1, || None);
                    }
                    let has = self.a_mem[reg].flags & MEM_AGG != 0 && self.a_agg[reg].is_some();
                    if !has {
                        // xFinalize on an accumulator that was never stepped
                        let ctx = self.agg_new(host, func, usize::from(func.n_arg.max(0) as u16), pc)?;
                        self.a_agg[reg] = Some(ctx);
                    }
                    let is_value = op.p3 != 0;
                    let rv: Mem = match self.a_agg[reg].as_mut() {
                        Some(AggCtx::MinMax { best, .. }) => best.clone().unwrap_or_else(Mem::null),
                        Some(AggCtx::Host(a)) => {
                            let v = if is_value {
                                match *a {
                                    Some(id) => host.agg_value(id)?,
                                    None => RuntimeValue::plain(Value::Null),
                                }
                            } else {
                                match a.take() {
                                    Some(id) => host.agg_finalize(id)?,
                                    None => RuntimeValue::plain(Value::Null),
                                }
                            };
                            mem_from_runtime(&v)
                        }
                        None => Mem::null(),
                    };
                    if rv.too_big(limit_length) {
                        return Err(too_big());
                    }
                    if is_value {
                        self.a_mem[op.p3 as usize] = rv;
                    } else {
                        self.a_agg[reg] = None;
                        self.a_mem[reg] = rv;
                    }
                }

                OP_PureFunc | OP_Function => {
                    let P4::FuncCtx(ctx) = &op.p4 else {
                        return Err(internal("OP_Function without a context"));
                    };
                    let argc = usize::from(ctx.argc);
                    let p2 = op.p2 as usize;
                    let func = &ctx.p_func;
                    let out: Mem;
                    if func.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
                        let coll = self.func_coll_seq(pc);
                        let argv = &self.a_mem[p2..p2 + argc];
                        if func.name.eq_ignore_ascii_case("nullif") {
                            // nullifFunc
                            out = if argc == 2 && sqlite3_mem_compare(&argv[0], &argv[1], coll.as_deref()) != 0 {
                                argv[0].clone()
                            } else {
                                Mem::null()
                            };
                        } else {
                            // minmaxFunc
                            let mask: i32 = if func.user_data == 0 { 0 } else { -1 };
                            let mut i_best = 0usize;
                            let mut null = argc == 0 || argv[0].flags & MEM_NULL != 0;
                            if !null {
                                for i in 1..argc {
                                    if argv[i].flags & MEM_NULL != 0 {
                                        null = true;
                                        break;
                                    }
                                    if (sqlite3_mem_compare(&argv[i_best], &argv[i], coll.as_deref()) ^ mask) >= 0 {
                                        i_best = i;
                                    }
                                }
                            }
                            out = if null { Mem::null() } else { argv[i_best].clone() };
                        }
                    } else {
                        let args: Vec<RuntimeValue> = self.a_mem[p2..p2 + argc].iter().map(Mem::to_runtime_value).collect();
                        let rv = host.call_function(func, &args)?;
                        out = mem_from_runtime(&rv);
                    }
                    if out.too_big(limit_length) {
                        return Err(too_big());
                    }
                    self.a_mem[op.p3 as usize] = out;
                }

                OP_ClrSubtype => {
                    self.a_mem[op.p1 as usize].flags &= !MEM_SUBTYPE;
                }

                OP_GetSubtype => {
                    let m = &self.a_mem[op.p1 as usize];
                    if m.flags & MEM_SUBTYPE != 0 {
                        let v = i64::from(m.e_subtype);
                        self.a_mem[op.p2 as usize].set_int64(v);
                    } else {
                        self.a_mem[op.p2 as usize].set_null();
                    }
                }

                OP_SetSubtype => {
                    let (null, v) = {
                        let m = &self.a_mem[op.p1 as usize];
                        (m.flags & MEM_NULL != 0, (m.i & 0xff) as u8)
                    };
                    let out = &mut self.a_mem[op.p2 as usize];
                    if null {
                        out.flags &= !MEM_SUBTYPE;
                    } else {
                        out.flags |= MEM_SUBTYPE;
                        out.e_subtype = v;
                    }
                }

                OP_FilterAdd => {
                    let mut h = filter_hash(&self.a_mem, op);
                    let m = &mut self.a_mem[op.p1 as usize];
                    let n = m.z.len() as u64;
                    if n > 0 {
                        h %= n * 8;
                        m.z[(h / 8) as usize] |= 1 << (h & 7);
                    }
                }

                OP_Filter => {
                    let mut h = filter_hash(&self.a_mem, op);
                    let m = &self.a_mem[op.p1 as usize];
                    let n = m.z.len() as u64;
                    if n > 0 {
                        h %= n * 8;
                        if m.z[(h / 8) as usize] & (1 << (h & 7)) == 0 {
                            self.a_counter[SQLITE_STMTSTATUS_FILTER_HIT] += 1;
                            jump!(op.p2);
                        }
                        self.a_counter[SQLITE_STMTSTATUS_FILTER_MISS] += 1;
                    }
                }

                OP_Trace | OP_Init => {
                    if op.opcode == OP_Init {
                        self.init_p1 += 1;
                        self.a_counter[SQLITE_STMTSTATUS_RUN] += 1;
                        jump!(op.p2);
                    }
                }

                OP_Explain | OP_Noop | OP_TableLock | OP_CursorHint | OP_ReleaseReg | OP_Abortable => {}

                _ => {
                    return Err(internal(&format!("opcode {} is not ported", sqlite3_opcode_name(op.opcode))));
                }
            }
            pc += 1;
        }
    }

    /// The `RowSet` of register `reg` (`sqlite3VdbeMemSetRowSet` when the register does not
    /// hold one).
    fn rowset_of(&mut self, reg: usize) -> &mut RowSet {
        if self.a_rowset.len() <= reg {
            self.a_rowset.resize_with(reg + 1, || None);
        }
        if self.a_mem[reg].flags & MEM_BLOB == 0 || self.a_rowset[reg].is_none() {
            self.a_rowset[reg] = Some(Box::new(RowSet::new()));
            self.a_mem[reg].z.clear();
            self.a_mem[reg].flags = MEM_BLOB;
        }
        self.a_rowset[reg].get_or_insert_with(|| Box::new(RowSet::new()))
    }

    /// `sqlite3VdbeMemMove` for what a register holds outside its `Mem` (an aggregate
    /// context, a `RowSet`).
    fn move_side(&mut self, from: usize, to: usize) {
        let n = from.max(to) + 1;
        if self.a_agg.len() < n {
            self.a_agg.resize_with(n, || None);
        }
        if self.a_rowset.len() < n {
            self.a_rowset.resize_with(n, || None);
        }
        self.a_agg[to] = self.a_agg[from].take();
        self.a_rowset[to] = self.a_rowset[from].take();
    }

    /// A fresh accumulator for the aggregate function of the instruction at `i_op`.
    fn agg_new<H: VdbeHost>(&mut self, host: &mut H, func: &Arc<FuncDef>, argc: usize, i_op: usize) -> Result<AggCtx> {
        if func.func_flags & SQLITE_FUNC_NEEDCOLL != 0 {
            // min() / max(): sqlite3GetFuncCollSeq
            let coll = self.func_coll_seq(i_op);
            return Ok(AggCtx::MinMax { best: None, is_max: func.user_data != 0, coll });
        }
        Ok(AggCtx::Host(Some(host.agg_open(func, argc)?)))
    }

    /// Opcode trace (`--cfg sqliters_dtrace`, `DTRACE` set): the instruction at `pc` has run;
    /// prints its address, name and a digest of the registers its operands name — the same
    /// line the instrumented C build prints (`patch_m1.py`: `trVmOp`).
    #[cfg(sqliters_dtrace)]
    fn trace_op(&self, pc: usize) {
        let op = &self.prog.a_op[pc];
        // C rewrites these opcodes when they first run
        let opcode = match op.opcode {
            OP_String8 => OP_String,
            OP_AggStep | OP_AggInverse => OP_AggStep1,
            o => o,
        };
        let prop = OPCODE_PROPERTY[opcode as usize];
        let mut w = String::new();
        let n_reg = self.prog.n_mem - self.prog.n_cursor;
        let reg = |i: i32| -> String {
            if i > 0 && i <= n_reg {
                let m = &self.a_mem[i as usize];
                if m.flags & MEM_BLOB != 0 && self.a_rowset.get(i as usize).is_some_and(Option::is_some) {
                    return "U".to_owned();
                }
                mem_digest(m)
            } else {
                "-".to_owned()
            }
        };
        if prop & OPFLG_IN1 != 0 {
            w.push_str(&format!(" 1={}", reg(op.p1)));
        }
        if prop & (OPFLG_IN2 | OPFLG_OUT2) != 0 {
            w.push_str(&format!(" 2={}", reg(op.p2)));
        }
        if prop & (OPFLG_IN3 | OPFLG_OUT3) != 0 {
            w.push_str(&format!(" 3={}", reg(op.p3)));
        }
        if op.opcode == OP_ResultRow {
            for i in 0..op.p2 {
                w.push_str(&format!(" r={}", reg(op.p1 + i)));
            }
        }
        match op.opcode {
            OP_Column => {
                if op.p5 & OPFLAG_BYTELENARG == 0 {
                    w.push_str(&format!(" x={}", reg(op.p3)));
                }
            }
            OP_Function | OP_PureFunc | OP_MakeRecord | OP_AggValue => w.push_str(&format!(" x={}", reg(op.p3))),
            OP_AggFinal => w.push_str(&format!(" x={}", reg(op.p1))),
            _ => {}
        }
        eprintln!("TR|vm|op|pc={} op={}{}", pc, sqlite3_opcode_name(opcode), w);
    }

    /// Opcode trace: the statement text (C prints `p->zSql` at `OP_Init`).
    pub fn trace_sql(&self, sql: &str) {
        #[cfg(sqliters_dtrace)]
        {
            if sqliters_types::dtrace_enabled() {
                let z: String = sql.chars().take(300).map(|c| if c == '\n' { ' ' } else { c }).collect();
                eprintln!("TR|vm|sql|{z}");
            }
        }
        let _ = sql;
    }
}

/// The text of a `P4_STATIC` / `P4_DYNAMIC` operand.
#[inline]
fn p4_text(p4: &P4) -> Option<&[u8]> {
    match p4 {
        P4::Static(s) => Some(s.as_bytes()),
        P4::Dynamic(v) => Some(v),
        _ => None,
    }
}

/// `sqlite3_result_value` for a result the engine's function returned.
fn mem_from_runtime(rv: &RuntimeValue) -> Mem {
    let mut m = Mem::from_value(&rv.value);
    if rv.subtype != 0 {
        m.e_subtype = rv.subtype;
        m.flags |= MEM_SUBTYPE;
    }
    m
}

/// The key order of an ephemeral index as the b-tree layer's index insert takes it: C compares
/// `nKeyField + 1` fields (`sqlite3VdbeAllocUnpackedRecord`), the fields past `nKeyField` in
/// binary ascending order. A NULL sorts last when exactly one of DESC / BIGNULL is set.
fn key_sort_orders(k: &KeyInfoRt) -> Vec<KeySortOrder> {
    let n = (usize::from(k.n_key_field) + 1).min(usize::from(k.n_all_field).max(1));
    (0..n)
        .map(|i| {
            let sf = k.a_sort_flags.get(i).copied().unwrap_or(0);
            let desc = sf & KEYINFO_ORDER_DESC != 0;
            let nulls_last = desc != (sf & KEYINFO_ORDER_BIGNULL != 0);
            match k.a_coll.get(i).and_then(|c| c.as_ref()) {
                Some(c) => c.key_sort_order(desc, nulls_last),
                None => KeySortOrder { desc, nulls_last, ..KeySortOrder::default() },
            }
        })
        .collect()
}

/// The primary result code as the engine's `ErrorCode`.
fn error_code_of(code: u8) -> ErrorCode {
    match code {
        1 => ErrorCode::Error,
        2 => ErrorCode::Internal,
        3 => ErrorCode::Perm,
        4 => ErrorCode::Abort,
        5 => ErrorCode::Busy,
        6 => ErrorCode::Locked,
        7 => ErrorCode::NoMem,
        8 => ErrorCode::ReadOnly,
        9 => ErrorCode::Interrupt,
        10 => ErrorCode::IoErr,
        11 => ErrorCode::Corrupt,
        12 => ErrorCode::NotFound,
        13 => ErrorCode::Full,
        14 => ErrorCode::CantOpen,
        15 => ErrorCode::Protocol,
        17 => ErrorCode::Schema,
        18 => ErrorCode::TooBig,
        19 => ErrorCode::Constraint,
        20 => ErrorCode::Mismatch,
        21 => ErrorCode::Misuse,
        _ => ErrorCode::Error,
    }
}
