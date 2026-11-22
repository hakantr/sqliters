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

use std::collections::BTreeMap;
use std::sync::Arc;

use sqliters_btree::Btree;
use sqliters_btree::btcursor::{BTREE_AUXDELETE, BTREE_SEEK_EQ, BcParked, BtCursor, CURSOR_REQUIRESEEK};
use sqliters_types::{ConflictResolutionEffect, ErrorCode, KeySortOrder, Pgno, Result, RuntimeValue, SqliteError, Value};

use crate::opcodes::*;
use crate::vdbeaux::{
    CollSeq, FuncDef, KEYINFO_ORDER_BIGNULL, KEYINFO_ORDER_DESC, KeyInfo, P4, P4Vtab, Program, SubProgram, VdbeOp, int32_to_64,
};
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
/// `OPFLAG_ISUPDATE`: this OP_Insert is an sql UPDATE
const OPFLAG_ISUPDATE: u16 = 0x04;
/// `OPFLAG_NOCHNG`: OP_VColumn nochange for UPDATE
const OPFLAG_NOCHNG: u16 = 0x01;
const OPFLAG_PREFORMAT: u16 = 0x80;
const OPFLAG_P2ISREG: u16 = 0x10;
/// `OE_Rollback`, `OE_Fail`: `OP_Halt.p2` (`Vdbe.errorAction`)
const OE_ROLLBACK: i32 = 1;
const OE_FAIL: i32 = 3;
const OE_IGNORE: i32 = 4;
const OPFLAG_LENGTHARG: u16 = 0x40;
const OPFLAG_TYPEOFARG: u16 = 0x80;
const OPFLAG_BYTELENARG: u16 = 0xc0;
const BTREE_UNORDERED: u16 = 8;
const SQLITE_FUNC_NEEDCOLL: u32 = 0x0020;
const SQLITE_FUNC_BUILTIN: u32 = 0x0080_0000;
const SQLITE_FUNC_WINDOW: u32 = 0x0001_0000;

/// `SQLITE_STMTSTATUS_*` (`Vdbe.aCounter[]`)
pub const SQLITE_STMTSTATUS_FULLSCAN_STEP: usize = 1;
pub const SQLITE_STMTSTATUS_SORT: usize = 2;
pub const SQLITE_STMTSTATUS_AUTOINDEX: usize = 3;
pub const SQLITE_STMTSTATUS_VM_STEP: usize = 4;
pub const SQLITE_STMTSTATUS_REPREPARE: usize = 5;
pub const SQLITE_STMTSTATUS_RUN: usize = 6;
pub const SQLITE_STMTSTATUS_FILTER_MISS: usize = 7;
pub const SQLITE_STMTSTATUS_FILTER_HIT: usize = 8;

/// What the machine needs of the connection (`sqlite3 *db`).
pub trait VdbeHost {
    /// `ENC(db)` used by OP_String8's conversion before its length check.
    fn text_encoding(&self) -> sqliters_types::TextEncoding { sqliters_types::TextEncoding::Utf8 }
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
    /// Preserves the call context of OP_PureFunc when the host supports it.
    #[doc(hidden)]
    fn call_pure_function(
        &mut self,
        func: &FuncDef,
        args: &[RuntimeValue],
        _context: u16,
    ) -> Result<RuntimeValue> {
        self.call_function(func, args)
    }
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
    /// `db->lastRowid`
    fn last_rowid(&self) -> i64 {
        0
    }
    /// `db->nChange`
    fn db_changes(&self) -> i64 {
        0
    }
    /// `db->nChange = n` (`sqlite3VdbeFrameRestore`; `db->nTotalChange` is not touched)
    fn set_db_changes(&mut self, _n: i64) {}
    /// `db->aLimit[SQLITE_LIMIT_TRIGGER_DEPTH]`
    fn trigger_depth_limit(&self) -> usize {
        1000
    }
    /// `db->flags & SQLITE_DeferFKs`
    fn defer_fks(&self) -> bool {
        false
    }
    /// `(db->nDeferredCons, db->nDeferredImmCons)`
    fn deferred_cons(&self) -> (i64, i64) {
        (0, 0)
    }
    /// `db->nDeferredCons += n` (`imm == false`) or `db->nDeferredImmCons += n` (`OP_FkCounter`).
    fn add_deferred_cons(&mut self, _imm: bool, _n: i64) {}
    /// The mask `sqlite3VdbeFindIndexKey` computes for the index a `P4_INDEX` operand names in
    /// database `i_db`: bit `ii` (of the first 64 of `pIdx->nColumn`) is set when column `ii`
    /// of the index is an expression (`XN_EXPR`) or a VIRTUAL generated column of the table.
    fn index_expr_mask(&mut self, _i_db: usize, _name: &str) -> u64 {
        0
    }
    /// `pPager->mxPgno` of database `i_db` (`PRAGMA max_page_count`); `None`: no limit known.
    fn max_page_count(&self, _i_db: usize) -> Option<u32> {
        None
    }
    /// `HAS_UPDATE_HOOK(db)`: `db->xUpdateCallback!=0` (the pre-update hook is not part of the
    /// build).
    fn has_update_hook(&self) -> bool {
        false
    }
    /// `db->xUpdateCallback(db->pUpdateArg, op, zDb, pTab->zName, iRowid)` with `op`
    /// `SQLITE_INSERT` (18), `SQLITE_UPDATE` (23) or `SQLITE_DELETE` (9); `zDb` is the name of
    /// database `i_db`. For `SQLITE_DELETE` the host checks `HasRowid(pTab)`.
    fn update_hook(&mut self, _op: i32, _i_db: usize, _table: &str, _rowid: i64) -> Result<()> {
        Ok(())
    }
    /// `sqlite3VtabBegin(db, pVTab)` for the table a `P4_VTAB` operand names (`OP_VBegin`).
    fn vtab_begin(&mut self, _vt: &P4Vtab) -> Result<()> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// `pModule->xUpdate(pVtab, nArg, apArg, &rowid)` with `db->vtabOnConflict = on_conflict`
    /// (`OE_*`); `nochange[i]`: `sqlite3_value_nochange(apArg[i])`. Returns the rowid the
    /// module set.
    fn vtab_update(&mut self, _vt: &P4Vtab, _args: &[Value], _nochange: &[bool], _on_conflict: u8) -> Result<i64> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// `pOp->p4.pVtab->bConstraint`: the module declared SQLITE_VTAB_CONSTRAINT_SUPPORT.
    fn vtab_constraint_support(&self, _vt: &P4Vtab) -> bool {
        false
    }
    /// `pModule->xOpen(pVtab, &pVCur)`
    fn vtab_open(&mut self, _vt: &P4Vtab) -> Result<Box<dyn sqliters_types::VirtualCursor>> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// `pModule->xFilter(pVCur, iQuery, idxStr, nArg, apArg)`
    fn vtab_filter(
        &mut self,
        _vt: &P4Vtab,
        _cur: &mut dyn sqliters_types::VirtualCursor,
        _idx_num: i32,
        _idx_str: Option<&str>,
        _args: &[Value],
    ) -> Result<()> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// `pModule->xColumn(pVCur, &sContext, iCol)`
    fn vtab_column(&mut self, _vt: &P4Vtab, _cur: &mut dyn sqliters_types::VirtualCursor, _i_col: usize) -> Result<Value> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// `pModule->xNext(pVCur)`
    fn vtab_next(&mut self, _vt: &P4Vtab, _cur: &mut dyn sqliters_types::VirtualCursor) -> Result<()> {
        Err(internal("virtual tables are not reachable from this statement"))
    }
    /// The module's `xColumn` returns without a result for column `i_col` when
    /// `sqlite3_vtab_nochange()` is true (fts5: a column that is not hidden; geopoly: column
    /// 0): the register keeps `MEM_Null|MEM_Zero`. Other modules (rtree) ignore the flag.
    fn vtab_column_skips_nochange(&self, _vt: &P4Vtab, _i_col: usize) -> bool {
        false
    }
    /// `pModule->xFilter` with the `sqlite3_value`s of `OP_VInitIn` (`sqlite3_vtab_in_first` /
    /// `_next` read them): `in_lists[i]` is the list of the IN operator whose `ValueList` is
    /// argument `i`, `None` for an ordinary argument.
    fn vtab_filter_in(
        &mut self,
        vt: &P4Vtab,
        cur: &mut dyn sqliters_types::VirtualCursor,
        idx_num: i32,
        idx_str: Option<&str>,
        args: &[Value],
        _in_lists: &[Option<Vec<Value>>],
    ) -> Result<()> {
        self.vtab_filter(vt, cur, idx_num, idx_str, args)
    }
    /// `pModule->xRowid`
    fn vtab_rowid(&mut self, _vt: &P4Vtab, cur: &mut dyn sqliters_types::VirtualCursor) -> Result<i64> {
        cur.rowid()
    }
    /// `OP_VCheck`: `pModule->xIntegrity(pVtab, zSchema, zTabName, isQuick, &zErr)`; `Some`
    /// is the message the module reports.
    fn vtab_integrity(&mut self, _vt: &P4Vtab, _schema: &str, _is_quick: bool) -> Result<Option<String>> {
        Ok(None)
    }
    /// `db->flags & SQLITE_QueryOnly`, `db->flags & SQLITE_CorruptRdOnly` (`OP_Transaction`)
    fn query_only(&self) -> bool {
        false
    }
    fn corrupt_rdonly(&self) -> bool {
        false
    }
    // --- `OP_AutoCommit`, `OP_Savepoint`: the transaction state of the connection ---
    /// `db->autoCommit`
    fn auto_commit(&self) -> bool {
        true
    }
    /// `db->autoCommit = v`
    fn set_auto_commit(&mut self, _v: bool) {}
    /// `db->nVdbeWrite` of the other statements (this one is read-only)
    fn n_vdbe_write(&self) -> i32 {
        0
    }
    /// `sqlite3VdbeCheckFkDeferred(p)` without its error: are deferred violations pending?
    /// (`db->nDeferredCons+db->nDeferredImmCons`, and the rows the engine's own executor
    /// suspects)
    fn fk_deferred_pending(&mut self) -> Result<bool> {
        let (a, b) = self.deferred_cons();
        Ok(a + b != 0)
    }
    /// `sqlite3RollbackAll(db, SQLITE_ABORT_ROLLBACK)`
    fn rollback_all(&mut self) -> Result<()> {
        Err(internal("transactions are not reachable from this statement"))
    }
    /// The transaction part of `sqlite3VdbeHalt(p)` for a statement that ran without error
    /// (`p->rc==SQLITE_OK`) and changes nothing itself: `Ok(None)` done, `Ok(Some(e))` the
    /// commit failed and the transaction was rolled back (`p->rc = rc`), `Err(e)` `SQLITE_BUSY`
    /// of a read-only statement (the transaction stays open, the halt is to be repeated).
    fn halt_transaction(&mut self, _read_only: bool) -> Result<Option<SqliteError>> {
        Err(internal("transactions are not reachable from this statement"))
    }
    /// `sqlite3CloseSavepoints(db)`
    fn close_savepoints(&mut self) {}
    /// The index of the newest savepoint named `name` in `db->pSavepoint` (0 = newest).
    fn savepoint_find(&self, _name: &str) -> Option<usize> {
        None
    }
    /// `pSavepoint->pNext==0 && db->isTransactionSavepoint` of savepoint `k`
    fn savepoint_is_transaction(&self, _k: usize) -> bool {
        false
    }
    /// `db->isTransactionSavepoint = v`
    fn set_transaction_savepoint(&mut self, _v: bool) {}
    /// `sqlite3VtabSavepoint(db, op, iSavepoint)`: `op` is `SAVEPOINT_BEGIN` (`k` = the new
    /// savepoint's place) or the operation on savepoint `k`.
    fn vtab_savepoint(&mut self, _op: i32, _k: usize) -> Result<()> {
        Ok(())
    }
    /// Creates savepoint `name` (`pNew`, linked first; `nDeferredCons` saved);
    /// `transaction`: it is the transaction savepoint (`db->autoCommit` was set).
    fn savepoint_open(&mut self, _name: &str, _transaction: bool) -> Result<()> {
        Err(internal("transactions are not reachable from this statement"))
    }
    /// `(db->mDbFlags & DBFLAG_SchemaChange)!=0` for a rollback to savepoint `k`
    fn savepoint_schema_change(&self, _k: usize) -> bool {
        false
    }
    /// The number of databases (`db->nDb`)
    fn n_db(&self) -> usize {
        0
    }
    /// `sqlite3BtreeTripAllCursors(db->aDb[i].pBt, SQLITE_ABORT_ROLLBACK, writeOnly)`
    fn trip_all_cursors(&mut self, _i_db: usize, _write_only: bool) -> Result<()> {
        Ok(())
    }
    /// `sqlite3BtreeSavepoint(db->aDb[i].pBt, op, iSavepoint)` for savepoint `k`
    fn btree_savepoint(&mut self, _i_db: usize, _op: i32, _k: usize) -> Result<()> {
        Ok(())
    }
    /// `sqlite3ExpirePreparedStatements(db, 0); sqlite3ResetAllSchemasOfConnection(db);
    /// db->mDbFlags |= DBFLAG_SchemaChange;` after a rollback to savepoint `k`
    fn savepoint_schema_reset(&mut self, _k: usize) -> Result<()> {
        Ok(())
    }
    /// Destroys the savepoints nested inside savepoint `k`; then `release`: savepoint `k`
    /// too, else `db->nDeferredCons` / `nDeferredImmCons` come back from it.
    fn savepoint_close(&mut self, _k: usize, _release: bool) {}
}

/// `SAVEPOINT_BEGIN`, `SAVEPOINT_RELEASE`, `SAVEPOINT_ROLLBACK`
pub const SAVEPOINT_BEGIN: i32 = 0;
pub const SAVEPOINT_RELEASE: i32 = 1;
pub const SAVEPOINT_ROLLBACK: i32 = 2;

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

/// `uc.pVCur`: the cursor of a virtual table (`CURTYPE_VTAB`) and the table it was opened
/// on (`pVCur->pVtab`).
pub struct VCursor {
    pub vt: Arc<P4Vtab>,
    pub cur: Box<dyn sqliters_types::VirtualCursor>,
}

impl std::fmt::Debug for VCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VCursor").field("vt", &self.vt).finish()
    }
}

/// `struct VdbeCursor`.
#[derive(Debug)]
pub struct VdbeCursor {
    /// `uc.pVCur`
    pub vcur: Option<VCursor>,
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
    /// A built-in window function of window.c (`struct CallCount`, `NtileCtx`, `LastValueCtx`,
    /// `NthValueCtx`; `alloc`: `sqlite3_aggregate_context(pCtx, nByte>0)` has allocated it)
    Win(Box<WinCtx>),
}

/// The built-in window functions of window.c that are called through `OP_AggStep` /
/// `OP_AggValue` / `OP_AggFinal` (`lead` / `lag` are coded in-line: `noopStepFunc`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WinFn {
    RowNumber,
    DenseRank,
    Rank,
    PercentRank,
    CumeDist,
    Ntile,
    LastValue,
    NthValue,
    FirstValue,
    Noop,
}

/// The accumulator of a window.c function: the fields of its C struct.
#[derive(Debug)]
struct WinCtx {
    f: WinFn,
    alloc: bool,
    /// `CallCount.nValue` (`row_number`: the `i64` itself), `nStep`, `nTotal`
    n_value: i64,
    n_step: i64,
    n_total: i64,
    /// `NtileCtx.nParam`, `iRow` (`nTotal` above)
    n_param: i64,
    i_row: i64,
    /// `LastValueCtx.pVal` / `NthValueCtx.pValue` and `LastValueCtx.nVal`
    val: Option<Mem>,
    n_val: i64,
}

impl WinFn {
    /// The window.c function a built-in `FuncDef` names.
    fn of(func: &FuncDef) -> Option<WinFn> {
        if func.func_flags & (SQLITE_FUNC_BUILTIN | SQLITE_FUNC_WINDOW) != (SQLITE_FUNC_BUILTIN | SQLITE_FUNC_WINDOW) {
            return None;
        }
        Some(match func.name.to_ascii_lowercase().as_str() {
            "row_number" => WinFn::RowNumber,
            "dense_rank" => WinFn::DenseRank,
            "rank" => WinFn::Rank,
            "percent_rank" => WinFn::PercentRank,
            "cume_dist" => WinFn::CumeDist,
            "ntile" => WinFn::Ntile,
            "last_value" => WinFn::LastValue,
            "nth_value" => WinFn::NthValue,
            "first_value" => WinFn::FirstValue,
            "lead" | "lag" => WinFn::Noop,
            _ => return None,
        })
    }
}

impl WinCtx {
    fn new(f: WinFn) -> WinCtx {
        WinCtx { f, alloc: false, n_value: 0, n_step: 0, n_total: 0, n_param: 0, i_row: 0, val: None, n_val: 0 }
    }

    /// `xSFunc` (`inverse == false`) or `xInverse` of the function, `args` = `apArg`.
    /// `Err(message)`: `sqlite3_result_error`.
    fn step(&mut self, inverse: bool, args: &mut [Mem]) -> std::result::Result<(), &'static str> {
        // every step / inverse function asks for sizeof(*p)
        if self.f != WinFn::Noop {
            self.alloc = true;
        }
        match (self.f, inverse) {
            // row_numberStepFunc: (*p)++
            (WinFn::RowNumber, false) => self.n_value += 1,
            // dense_rankStepFunc
            (WinFn::DenseRank, false) => self.n_step = 1,
            // rankStepFunc
            (WinFn::Rank, false) => {
                self.n_step += 1;
                if self.n_value == 0 {
                    self.n_value = self.n_step;
                }
            }
            // percent_rankStepFunc / cume_distStepFunc
            (WinFn::PercentRank | WinFn::CumeDist, false) => self.n_total += 1,
            // percent_rankInvFunc / cume_distInvFunc
            (WinFn::PercentRank | WinFn::CumeDist, true) => self.n_step += 1,
            // ntileStepFunc
            (WinFn::Ntile, false) => {
                let mut err = false;
                if self.n_total == 0 {
                    self.n_param = args.first().map_or(0, Mem::int_value);
                    if self.n_param <= 0 {
                        err = true;
                    }
                }
                self.n_total += 1;
                if err {
                    return Err("argument of ntile must be a positive integer");
                }
            }
            // ntileInvFunc
            (WinFn::Ntile, true) => self.i_row += 1,
            // last_valueStepFunc
            (WinFn::LastValue, false) => {
                self.val = Some(args.first().cloned().unwrap_or_else(Mem::null));
                self.n_val += 1;
            }
            // last_valueInvFunc
            (WinFn::LastValue, true) => {
                self.n_val -= 1;
                if self.n_val == 0 {
                    self.val = None;
                }
            }
            // nth_valueStepFunc
            (WinFn::NthValue, false) => {
                const E: &str = "second argument to nth_value must be a positive integer";
                let Some(a1) = args.get_mut(1) else {
                    return Err(E);
                };
                // sqlite3_value_numeric_type(apArg[1]): a TEXT argument gets numeric affinity in place
                if a1.value_type() == 3 {
                    a1.apply_numeric_affinity(false);
                }
                let i_val = match a1.value_type() {
                    1 => a1.int_value(),
                    2 => {
                        let f = a1.real_value();
                        if sqlite3_real_to_i64(f) as f64 != f {
                            return Err(E);
                        }
                        f as i64
                    }
                    _ => return Err(E),
                };
                if i_val <= 0 {
                    return Err(E);
                }
                self.n_step += 1;
                if i_val == self.n_step {
                    self.val = Some(args[0].clone());
                }
            }
            // first_valueStepFunc
            (WinFn::FirstValue, false) => {
                if self.val.is_none() {
                    self.val = Some(args.first().cloned().unwrap_or_else(Mem::null));
                }
            }
            // noopStepFunc (nth_value / first_value inverse; row_number, dense_rank, rank: never
            // called for an inverse; lead / lag)
            _ => {}
        }
        Ok(())
    }

    /// `xValue` (`finalize == false`) or `xFinalize`: the result (`None`: no result, NULL).
    fn value(&mut self, finalize: bool) -> Option<Mem> {
        let int = |v: i64| {
            let mut m = Mem::null();
            m.set_int64(v);
            Some(m)
        };
        let dbl = |v: f64| {
            let mut m = Mem::null();
            m.set_double(v);
            Some(m)
        };
        match self.f {
            // row_numberValueFunc (also xFinalize)
            WinFn::RowNumber => {
                self.alloc = true;
                int(self.n_value)
            }
            // dense_rankValueFunc
            WinFn::DenseRank => {
                self.alloc = true;
                if self.n_step != 0 {
                    self.n_value += 1;
                    self.n_step = 0;
                }
                int(self.n_value)
            }
            // rankValueFunc
            WinFn::Rank => {
                self.alloc = true;
                let v = self.n_value;
                self.n_value = 0;
                int(v)
            }
            // percent_rankValueFunc (= percent_rankFinalizeFunc)
            WinFn::PercentRank => {
                self.alloc = true;
                self.n_value = self.n_step;
                if self.n_total > 1 {
                    dbl(self.n_value as f64 / (self.n_total - 1) as f64)
                } else {
                    dbl(0.0)
                }
            }
            // cume_distValueFunc: sqlite3_aggregate_context(pCtx, 0)
            WinFn::CumeDist => {
                if self.alloc {
                    dbl(self.n_step as f64 / self.n_total as f64)
                } else {
                    None
                }
            }
            // ntileValueFunc (= ntileFinalizeFunc)
            WinFn::Ntile => {
                self.alloc = true;
                if self.n_param <= 0 {
                    return None;
                }
                let n_size = self.n_total / self.n_param;
                if n_size == 0 {
                    int(self.i_row + 1)
                } else {
                    let n_large = self.n_total - self.n_param * n_size;
                    let i_small = n_large * (n_size + 1);
                    let i_row = self.i_row;
                    if i_row < i_small {
                        int(1 + i_row / (n_size + 1))
                    } else {
                        int(1 + n_large + (i_row - i_small) / n_size)
                    }
                }
            }
            WinFn::LastValue => {
                if finalize {
                    // last_valueFinalizeFunc
                    self.alloc = true;
                    self.val.take()
                } else if self.alloc {
                    // last_valueValueFunc: sqlite3_aggregate_context(pCtx, 0)
                    self.val.clone()
                } else {
                    None
                }
            }
            WinFn::NthValue => {
                if finalize {
                    // nth_valueFinalizeFunc: sqlite3_aggregate_context(pCtx, 0)
                    if self.alloc { self.val.take() } else { None }
                } else {
                    // noopValueFunc
                    None
                }
            }
            WinFn::FirstValue => {
                if finalize {
                    // first_valueFinalizeFunc
                    self.alloc = true;
                    self.val.take()
                } else {
                    None
                }
            }
            WinFn::Noop => None,
        }
    }
}

/// P4 operands bound to the connection when the program is made ready.
#[derive(Clone, Default)]
enum OpRt {
    #[default]
    None,
    Coll(Arc<CollRt>),
    KeyInfo(Arc<KeyInfoRt>),
}

/// The run time of a trigger sub-program kept in the register `P3` of its `OP_Program`
/// between invocations (`pRt`: a `MEM_Blob` whose `z` is the `VdbeFrame` with
/// `VdbeFrameMem(pFrame)` and the cursor slots behind it).
struct FrameRt {
    a_mem: Vec<Mem>,
    ap_csr: Vec<Option<Box<VdbeCursor>>>,
    a_agg: Vec<Option<AggCtx>>,
    a_rowset: Vec<Option<Box<RowSet>>>,
    a_frame: Vec<Option<Box<FrameRt>>>,
}

/// `struct VdbeFrame` while its sub-program runs: what `OP_Program` saved of the parent
/// (`pFrame->aMem`, `apCsr`, `aOp`, `pc`, `token`, `lastRowid`, `nChange`, `nDbChange`) and
/// the parent's side tables and bound operands.
struct VdbeFrame {
    /// `pFrame->aOp`: the parent's program (`None`: the main program)
    parent_sub: Option<Arc<SubProgram>>,
    a_mem: Vec<Mem>,
    ap_csr: Vec<Option<Box<VdbeCursor>>>,
    a_agg: Vec<Option<AggCtx>>,
    a_rowset: Vec<Option<Box<RowSet>>>,
    a_frame: Vec<Option<Box<FrameRt>>>,
    rt: Vec<OpRt>,
    once: Vec<i32>,
    /// `pFrame->pc`: address of the `OP_Program`, with its `P1`, `P2` and `P3`
    pc: usize,
    prog_p1: i32,
    prog_p2: i32,
    reg: usize,
    /// `pFrame->token`
    token: u64,
    /// The sub-program the frame runs (its bound operands go back to the cache at the end)
    sub: Arc<SubProgram>,
    /// `pFrame->lastRowid`, `pFrame->nChange`, `pFrame->nDbChange`
    last_rowid: i64,
    n_change: i64,
    n_db_change: i64,
}

/// A running statement (`struct Vdbe` in `VDBE_RUN_STATE`).
pub struct Vdbe {
    /// The program (`aOp`)
    pub prog: Arc<Program>,
    /// `pBt->pTmpSpace` / `pBt->nPreformatSize` after `OP_RowCell`
    /// (`sqlite3BtreeTransferRow`): the payload of the source cell, which the next
    /// `OP_Insert` / `OP_IdxInsert` with `OPFLAG_PREFORMAT` inserts.
    preformat_cell: Vec<u8>,
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
    /// `expired==2` set by the current statement's OP_Expire P2=1.
    pub expired_on_reset: bool,
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
    /// Trigger frames: `p->pFrame` chain (innermost last), `p->nFrame == frames.len()`
    frames: Vec<VdbeFrame>,
    /// The sub-program being run (`p->aOp` of the innermost frame), `None`: the main program
    cur_sub: Option<Arc<SubProgram>>,
    /// Frame registers of the current `aMem` (`pRt` of `OP_Program`), by register
    a_frame: Vec<Option<Box<FrameRt>>>,
    /// Bound P4 operands of the sub-programs that are not running, by sub-program address
    sub_rt: Vec<(usize, Vec<OpRt>)>,
    /// `nFkConstraint`: immediate foreign key violations of the statement
    pub n_fk_constraint: i64,
    /// Opcode trace: the statement text (printed again at the `OP_Init` of a sub-program)
    tr_sql: String,
    /// Opcode trace: print every register as `U` (unused: C prints the last instruction of a
    /// halted statement after `releaseMemArray`, which leaves a register `MEM_Undefined` only
    /// when it holds a heap buffer — `szMalloc`, which the machine does not model; the
    /// comparison tools mask the registers of that line instead)
    tr_undef: bool,
    /// The last error left the statement running (`goto vdbe_return` without
    /// `sqlite3VdbeHalt`): `SQLITE_BUSY` of `OP_Transaction` / `OP_AutoCommit` /
    /// `OP_Savepoint` (`p->pc` is the instruction to repeat) or a deferred foreign key
    /// violation found by `OP_AutoCommit` / `OP_Savepoint`.
    pub not_halted: bool,
    /// `p->rc` of a statement the machine halted itself after its transaction failed to
    /// commit (`OP_AutoCommit`, `OP_Savepoint` RELEASE).
    pub halt_rc: Option<SqliteError>,
    /// Registers that hold a `ValueList` of `OP_VInitIn`, and the cursor of its values
    value_lists: Vec<(usize, usize)>,
}

/// The first field of index record `key` (`sqlite3_vtab_in_first`: `sqlite3VdbeMemFromBtree`
/// and `sqlite3VdbeSerialGet` of the first column).
fn sqliters_vdbe_record_first(ki: &KeyInfoRt, key: &[u8]) -> Value {
    let mems = crate::vdbeaux_rt::sqlite3_vdbe_record_unpack(ki, key, 1);
    mems.first().map_or(Value::Null, Mem::to_value)
}

#[cold]
fn internal(msg: &str) -> SqliteError {
    SqliteError::new(ErrorCode::Internal, format!("vdbe: {msg}"))
}

/// `vdbeFkError(p)` (prepared with `SQLITE_PREPARE_SAVESQL`): `SQLITE_CONSTRAINT_FOREIGNKEY`,
/// `errorAction = OE_Abort`.
#[cold]
pub fn fk_error() -> SqliteError {
    let mut e = SqliteError::with_extended(787, "FOREIGN KEY constraint failed".to_owned());
    e.code = ErrorCode::Constraint;
    e.conflict_effect = ConflictResolutionEffect::Abort;
    e
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
    unsupported(prog, false, ReadKeyScope::Utf8, None, false)
}

/// [`unsupported_reason`] for a program that writes (`OP_Transaction` with `P2!=0`, the
/// write opcodes).
pub fn unsupported_reason_write(prog: &Program) -> Option<String> {
    unsupported(prog, true, ReadKeyScope::Utf8, None, false)
}

/// Allow BINARY UTF-16 sorter keys for the checked mapped and row CASE reads.
pub fn unsupported_reason_mapped_read(prog: &Program) -> Option<String> {
    unsupported(prog, false, ReadKeyScope::BinarySorter, None, false)
}

/// Keys a checked row CASE read can use in the database's encoding.
pub fn unsupported_reason_row_case_read(
    prog: &Program, grouped: bool, distinct: bool, builtin_collations: bool, encoding: u8,
) -> Option<String> {
    let key_scope = if grouped && builtin_collations {
        ReadKeyScope::BuiltinGroup
    } else if grouped {
        ReadKeyScope::BinaryGroup
    } else if distinct && builtin_collations {
        ReadKeyScope::BuiltinDistinct
    } else if distinct {
        ReadKeyScope::BinaryDistinct
    } else {
        ReadKeyScope::BinarySorter
    };
    let row_case_encoding = (prog.read_only
        && matches!(encoding, crate::vdbeaux::SQLITE_UTF16LE | crate::vdbeaux::SQLITE_UTF16BE))
        .then_some(encoding);
    unsupported(prog, false, key_scope, row_case_encoding, builtin_collations)
}

/// select.c's unordered DISTINCT probes its private index before storing the same key.
fn unordered_distinct_key(
    prog: &Program, open: &VdbeOp, key: &KeyInfo, supports_collation: impl Fn(&CollSeq) -> bool,
) -> bool {
    let n = i32::from(key.n_key_field);
    if open.opcode != OP_OpenEphemeral || open.p2 != 0 || open.p3 != 0
        || open.p5 != BTREE_UNORDERED || n == 0 || i32::from(key.n_all_field) != n + 1
        || !key.a_coll.get(..n as usize).is_some_and(|fields| {
            fields.iter().all(|field| field.as_deref().is_some_and(&supports_collation))
        })
    {
        return false;
    }
    prog.a_op.windows(3).any(|ops| {
        let [found, record, insert] = ops else { return false };
        found.opcode == OP_Found && found.p1 == open.p1 && found.p4 == P4::Int32(n)
            && record.opcode == OP_MakeRecord && record.p1 == found.p3 && record.p2 == n
            && matches!(record.p4, P4::NotUsed)
            && insert.opcode == OP_IdxInsert && insert.p1 == open.p1
            && insert.p2 == record.p3 && insert.p3 == found.p3 && insert.p4 == P4::Int32(n)
            && insert.p5 == OPFLAG_USESEEKRESULT
    })
}

/// The ordered RHS set for IN is filled by SRT_Set and probed at the same
/// width. Other ordered ephemeral keys, such as the LIMIT sorter, keep the
/// ordinary key admission check.
fn ordered_in_key(
    prog: &Program, open: &VdbeOp, key: &KeyInfo, supports_collation: impl Fn(&CollSeq) -> bool,
) -> bool {
    let width = i32::from(key.n_key_field);
    if open.opcode != OP_OpenEphemeral || open.p2 != width || open.p3 != 0
        || open.p5 != 0 || width == 0 || i32::from(key.n_all_field) != width + 1
        || !key.a_sort_flags.get(..width as usize).is_some_and(|flags| flags.iter().all(|&flag| flag == 0))
        || !key.a_coll.get(..width as usize).is_some_and(|fields| {
            fields.iter().all(|field| field.as_deref().is_some_and(&supports_collation))
        })
    {
        return false;
    }

    let mut written = false;
    let mut probed = false;
    for (index, op) in prog.a_op.iter().enumerate() {
        if op.opcode == OP_OpenDup && op.p2 == open.p1 {
            return false;
        }
        if op.p1 != open.p1 {
            continue;
        }
        match op.opcode {
            OP_IdxInsert => {
                let Some(record) = index.checked_sub(1).and_then(|i| prog.a_op.get(i)) else {
                    return false;
                };
                if op.p4 != P4::Int32(width) || op.p5 != 0 || record.opcode != OP_MakeRecord
                    || record.p1 != op.p3 || record.p2 != width || record.p3 != op.p2
                    || !matches!(&record.p4, P4::Dynamic(affinity) if affinity.len() == width as usize)
                {
                    return false;
                }
                written = true;
            }
            OP_Found | OP_NotFound => {
                if op.p4 != P4::Int32(width) {
                    return false;
                }
                probed = true;
            }
            OP_IdxDelete => return false,
            _ => {}
        }
    }
    written && probed
}

fn in_list_value_output(value: &VdbeOp) -> Option<i32> {
    match value.opcode {
        OP_Integer | OP_Int64 | OP_Real | OP_String8 | OP_String | OP_Null | OP_Blob | OP_Variable => Some(value.p2),
        OP_Add | OP_Subtract | OP_Multiply | OP_Divide | OP_Remainder
            if value.p4 == P4::NotUsed && value.p5 == 0 => Some(value.p3),
        _ => None,
    }
}

fn stores_integer_set_record(record: &VdbeOp, insert: &VdbeOp, cursor: i32, output: i32) -> bool {
    output == record.p1 && record.opcode == OP_MakeRecord && record.p2 == 1 && record.p5 == 0
        && matches!(&record.p4, P4::Dynamic(affinity) if affinity.as_slice() == [SQLITE_AFF_INTEGER])
        && insert.opcode == OP_IdxInsert && insert.p1 == cursor && insert.p2 == record.p3
        && insert.p3 == record.p1 && insert.p4 == P4::Int32(1) && insert.p5 == 0
}

/// Multi-row VALUES may produce the IN set from a single-column coroutine.
/// Each yield supplies the register copied into the ordinary set record.
fn constant_values_set(
    ops: &[VdbeOp], entries: &[VdbeOp], open: usize, ret: usize, cursor: i32,
) -> bool {
    let [init, read, copy, record, insert, repeat] = entries else { return false };
    if init.opcode != OP_InitCoroutine || init.p2 != 0
        || read.opcode != OP_Yield || read.p1 != init.p1 || read.p2 as usize != ret - 1 || read.p5 != 0
        || copy.opcode != OP_Copy || copy.p3 != 0 || copy.p5 != 2
        || !stores_integer_set_record(record, insert, cursor, copy.p2)
        || repeat.opcode != OP_Goto || repeat.p2 as usize != open + 2
    {
        return false;
    }
    let Ok(entry) = usize::try_from(init.p3) else { return false };
    let Some(first) = entry.checked_sub(1).and_then(|i| ops.get(i)) else { return false };
    if first.opcode != OP_InitCoroutine || first.p1 != init.p1 || first.p3 != init.p3 {
        return false;
    }
    let Some(end) = usize::try_from(first.p2).ok().and_then(|i| i.checked_sub(1)) else {
        return false;
    };
    if end >= open || !ops.get(end).is_some_and(|op| op.opcode == OP_EndCoroutine && op.p1 == init.p1) {
        return false;
    }
    ops.get(entry..end).is_some_and(|rows| {
        !rows.is_empty() && rows.len() % 2 == 0 && rows.chunks_exact(2).all(|row| {
            let [value, yield_row] = row else { return false };
            in_list_value_output(value) == Some(copy.p1) && yield_row.opcode == OP_Yield
                && yield_row.p1 == init.p1 && yield_row.p2 == 0 && yield_row.p5 == 0
        })
    })
}

/// An IN list driving an integer lookup is a private one-column set. Its
/// records use integer affinity and database encoding before index insertion.
fn integer_probe_list_key(
    prog: &Program, open_index: usize, open: &VdbeOp, key: &KeyInfo,
    supports_collation: impl Fn(&CollSeq) -> bool,
) -> bool {
    if open.opcode != OP_OpenEphemeral || open.p2 != 1 || open.p3 != 0 || open.p5 != 0
        || key.n_key_field != 1 || key.n_all_field != 2
        || !matches!(key.a_coll.as_slice(), [collation, None]
            if collation.as_deref().is_none_or(&supports_collation))
        || key.a_sort_flags != [0, 0]
    {
        return false;
    }
    let ops = &prog.a_op;
    let cursor = open.p1;
    let Some([begin, once]) = open_index.checked_sub(2).and_then(|start| ops.get(start..open_index)) else {
        return false;
    };
    let reusable = matches!(&begin.p4, P4::SubrtnSig(signature)
        if signature.b_complete && signature.i_table == cursor
            && signature.z_aff.as_bytes() == [SQLITE_AFF_INTEGER]);
    if begin.opcode != OP_BeginSubrtn || begin.p1 != 0
        || (begin.p4 != P4::NotUsed && !reusable)
        || once.opcode != OP_Once || once.p3 != 0
    {
        return false;
    }
    let ret = once.p2 as usize;
    let Some([null_row, return_op, scan, column, skip_null, probe]) = ret.checked_sub(1)
        .and_then(|start| start.checked_add(6).and_then(|end| ops.get(start..end))) else {
        return false;
    };
    let step_index = skip_null.p2 as usize;
    let Some(step) = ops.get(step_index) else { return false };
    let secondary = probe.opcode != OP_SeekRowid;
    let seek = if secondary {
        // A SELECT set is already normalized by its integer-affinity record.
        // Literal lists may still have a separate affinity at the index seek.
        let seek_index = if probe.opcode == OP_Affinity {
            if probe.p1 != column.p3 || probe.p2 != 1 || probe.p5 != 0
                || !matches!(&probe.p4, P4::Dynamic(affinity)
                    if matches!(affinity.as_slice(), [SQLITE_AFF_INTEGER | SQLITE_AFF_NUMERIC]))
            {
                return false;
            }
            ret + 5
        } else if reusable {
            ret + 4
        } else {
            return false;
        };
        let Some([seek, stop]) = ops.get(seek_index..seek_index + 2) else { return false };
        if !matches!((seek.opcode, stop.opcode), (OP_SeekGE, OP_IdxGT) | (OP_SeekLE, OP_IdxLT))
            || seek.p4 != P4::Int32(1) || stop.p1 != seek.p1 || stop.p2 != seek.p2
            || stop.p3 != seek.p3 || stop.p4 != P4::Int32(1) || stop.p5 != 0
            || !ops.get(seek_index + 2..step_index).is_some_and(|body| body.iter().any(|next| {
                matches!((seek.opcode, next.opcode), (OP_SeekGE, OP_Next) | (OP_SeekLE, OP_Prev))
                    && next.p1 == seek.p1 && next.p2 as usize == seek_index + 1
            }))
        {
            return false;
        }
        seek
    } else {
        if probe.p4 != P4::NotUsed {
            return false;
        }
        probe
    };
    if null_row.opcode != OP_NullRow || null_row.p1 != cursor
        || return_op.opcode != OP_Return || return_op.p1 != begin.p2
        || return_op.p2 as usize != open_index - 1 || return_op.p3 != 1
        || !matches!((scan.opcode, step.opcode), (OP_Rewind, OP_Next) | (OP_Last, OP_Prev))
        || scan.p1 != cursor || scan.p2 as usize != step_index + 1 || scan.p5 != 0
        || column.opcode != OP_Column || column.p1 != cursor || column.p2 != 0
        || column.p4 != P4::NotUsed || column.p5 != 0
        || skip_null.opcode != OP_IsNull || skip_null.p1 != column.p3
        || seek.p1 == cursor || seek.p3 != column.p3
        || seek.p2 != skip_null.p2 || seek.p5 != 0
        || step_index < ret + 5 || step.p1 != cursor || step.p2 as usize != ret + 2
        || step.p3 != 0 || step.p5 != 0
    {
        return false;
    }

    let build = open_index + 1..ret - 1;
    let Some(entries) = ops.get(build.clone()) else { return false };
    let direct_records = !entries.is_empty() && entries.len() % 3 == 0 && entries.chunks_exact(3).all(|entry| {
        let [value, record, insert] = entry else { return false };
        in_list_value_output(value).is_some_and(|output| stores_integer_set_record(record, insert, cursor, output))
    });
    if !(direct_records || (reusable && constant_values_set(ops, entries, open_index, ret, cursor))) {
        return false;
    }

    let mut physical_read = false;
    for (index, op) in ops.iter().enumerate() {
        if (op.opcode == OP_OpenDup && op.p2 == cursor)
            || (matches!(op.opcode, OP_DeferredSeek | OP_SorterData) && op.p3 == cursor)
        {
            return false;
        }
        let opens = matches!(op.opcode, OP_OpenRead | OP_OpenWrite | OP_ReopenIdx | OP_OpenDup
            | OP_OpenAutoindex | OP_OpenEphemeral | OP_SorterOpen | OP_OpenPseudo | OP_VOpen);
        if opens && op.p1 == seek.p1 {
            let compatible = if secondary {
                op.p5 == OPFLAG_SEEKEQ
                    && matches!(&op.p4, P4::KeyInfo(index_key)
                        if index_key.enc == key.enc && index_key.n_key_field > 0
                            && matches!(index_key.a_coll.first(), Some(None)))
            } else {
                op.p5 == 0 && matches!(op.p4, P4::Int32(n) if n > 0)
            };
            if physical_read || index >= ret || op.opcode != OP_OpenRead || op.p2 <= 1
                || op.p3 != 0 || !compatible
            {
                return false;
            }
            physical_read = true;
        }
        if op.p1 != cursor || index == open_index {
            continue;
        }
        if opens {
            return false;
        }
        let expected = match op.opcode {
            OP_IdxInsert => build.contains(&index),
            OP_NullRow => index == ret - 1,
            OP_Rewind | OP_Last => index == ret + 1,
            OP_Column => index == ret + 2,
            OP_Next | OP_Prev => index == step_index,
            OP_IdxDelete | OP_NotFound | OP_Found | OP_NoConflict | OP_SeekLT | OP_SeekLE
            | OP_SeekGE | OP_SeekGT | OP_SeekRowid | OP_NotExists | OP_IfNoHope | OP_Insert
            | OP_Delete | OP_IdxLE | OP_IdxGT | OP_IdxLT | OP_IdxGE | OP_RowData | OP_Rowid
            | OP_IdxRowid | OP_DeferredSeek | OP_Count | OP_Sort | OP_ResetSorter | OP_SorterInsert => false,
            _ => true,
        };
        if !expected {
            return false;
        }
    }
    physical_read
}

/// SRT_DistFifo packs the row in the database encoding before probing its
/// private duplicate set, and inserts from those same normalized registers.
fn recursive_fifo_key(
    prog: &Program, open_index: usize, open: &VdbeOp, key: &KeyInfo,
    supports_collation: impl Fn(&CollSeq) -> bool,
) -> bool {
    let width = i32::from(key.n_key_field);
    let fields = usize::from(key.n_all_field);
    if open.opcode != OP_OpenEphemeral || open.p2 != width || open.p3 != 0 || open.p5 != 0
        || width == 0 || fields != width as usize + 1
        || key.a_sort_flags.len() != fields || key.a_sort_flags.iter().any(|&flag| flag != 0)
        || !matches!(key.a_coll.split_last(), Some((None, collations))
            if collations.len() == width as usize && collations.iter().all(|c|
                c.as_deref().is_some_and(&supports_collation)))
    {
        return false;
    }

    let Some([current, queue]) = open_index.checked_sub(2).and_then(|i| prog.a_op.get(i..open_index)) else {
        return false;
    };
    if current.opcode != OP_OpenPseudo || current.p3 != width || current.p5 != 0
        || queue.opcode != OP_OpenEphemeral || queue.p1.checked_add(1) != Some(open.p1)
        || queue.p2 != width || queue.p3 != 0 || queue.p5 != 0 || queue.p4 != P4::NotUsed
    {
        return false;
    }

    let mut producers = 0;
    for (index, op) in prog.a_op.iter().enumerate() {
        if (op.opcode == OP_OpenDup && op.p2 == open.p1)
            || (matches!(op.opcode, OP_DeferredSeek | OP_SorterData) && op.p3 == open.p1)
        {
            return false;
        }
        if op.p1 != open.p1 || index == open_index {
            continue;
        }
        match op.opcode {
            OP_Found => {
                if !fifo_duplicate_producer(&prog.a_op, index, width, queue.p1) {
                    return false;
                }
                producers += 1;
            }
            OP_IdxInsert => {
                if !index.checked_sub(1).and_then(|i| prog.a_op.get(i))
                    .is_some_and(|found| found.opcode == OP_Found && found.p1 == open.p1)
                {
                    return false;
                }
            }
            OP_OpenRead | OP_ReopenIdx | OP_OpenDup | OP_OpenAutoindex | OP_OpenEphemeral
            | OP_SorterOpen | OP_OpenPseudo | OP_NotFound | OP_NoConflict | OP_IdxDelete
            | OP_SeekLT | OP_SeekLE | OP_SeekGE | OP_SeekGT | OP_SeekRowid | OP_NotExists
            | OP_IfNoHope | OP_Insert | OP_Delete | OP_IdxLE | OP_IdxGT | OP_IdxLT | OP_IdxGE
            | OP_Rewind | OP_Last | OP_Next | OP_Prev | OP_Sort | OP_SeekEnd
            | OP_Column | OP_RowData | OP_Rowid | OP_IdxRowid | OP_DeferredSeek => return false,
            _ => {}
        }
    }
    // The seed and recursive SELECT each fill the same FIFO.
    producers >= 2
}

fn fifo_duplicate_producer(ops: &[VdbeOp], probe: usize, width: i32, queue: i32) -> bool {
    let Some([record, found, index, rowid, insert]) = probe.checked_sub(1)
        .and_then(|start| ops.get(start..probe + 4)) else {
        return false;
    };
    record.opcode == OP_MakeRecord && record.p2 == width && record.p3 == found.p3
        && record.p4 == P4::NotUsed && record.p5 == 0
        && found.p2 as usize == probe + 4 && found.p4 == P4::Int32(0) && found.p5 == 0
        && index.opcode == OP_IdxInsert && index.p1 == found.p1 && index.p2 == record.p3
        && index.p3 == record.p1 && index.p4 == P4::Int32(width) && index.p5 == 0
        && rowid.opcode == OP_NewRowid && rowid.p1 == queue && rowid.p3 == 0 && rowid.p5 == 0
        && insert.opcode == OP_Insert && insert.p1 == queue && insert.p2 == record.p3
        && insert.p3 == rowid.p2 && insert.p4 == P4::NotUsed && insert.p5 == OPFLAG_APPEND
}

/// An ordered recursive CTE dequeues from an index keyed by its ORDER BY
/// columns and a sequence number. UNION also tracks all admitted result rows.
fn recursive_priority_key(
    prog: &Program, index: usize, supports_collation: impl Fn(&CollSeq) -> bool,
) -> bool {
    let ops = &prog.a_op;
    let Some(previous) = index.checked_sub(1).and_then(|i| ops.get(i)) else {
        return false;
    };
    let queue_index = if previous.opcode == OP_OpenPseudo { index } else { index - 1 };
    let Some(current) = queue_index.checked_sub(1).and_then(|i| ops.get(i)) else {
        return false;
    };
    let queue = &ops[queue_index];
    let P4::KeyInfo(key) = &queue.p4 else {
        return false;
    };
    let columns = current.p3;
    let order = usize::from(key.n_key_field).saturating_sub(1);
    if current.opcode != OP_OpenPseudo || current.p5 != 0 || columns <= 0
        || queue.opcode != OP_OpenEphemeral || queue.p3 != 0 || queue.p5 != 0
        || order == 0 || queue.p2 as usize != order + 2 || usize::from(key.n_all_field) != order + 2
        || key.a_coll.len() != order + 2 || key.a_sort_flags.len() != order + 2
        || !key.a_coll[..order].iter().all(|coll| coll.as_deref().is_some_and(&supports_collation))
        || key.a_coll[order..].iter().any(Option::is_some)
        || key.a_sort_flags[..order].iter().any(|flags| flags & !(KEYINFO_ORDER_DESC | KEYINFO_ORDER_BIGNULL) != 0)
        || key.a_sort_flags[order..].iter().any(|&flags| flags != 0)
    {
        return false;
    }
    let distinct = ops.get(queue_index + 1).filter(|op|
        op.opcode == OP_OpenEphemeral && queue.p1.checked_add(1) == Some(op.p1));
    if let Some(seen) = distinct {
        let P4::KeyInfo(seen_key) = &seen.p4 else {
            return false;
        };
        if seen.p2 != columns || seen.p3 != 0 || seen.p5 != 0 || seen_key.enc != key.enc
            || i32::from(seen_key.n_key_field) != columns || i32::from(seen_key.n_all_field) != columns + 1
            || seen_key.a_sort_flags.len() != columns as usize + 1
            || seen_key.a_sort_flags.iter().any(|&flags| flags != 0)
            || !matches!(seen_key.a_coll.split_last(), Some((None, fields)) if fields.len() == columns as usize
                && fields.iter().all(|coll| coll.as_deref().is_some_and(&supports_collation)))
        {
            return false;
        }
    } else if index != queue_index {
        return false;
    }
    if ops.iter().any(|op| op.opcode == OP_OpenDup
        && (op.p2 == queue.p1 || distinct.is_some_and(|seen| op.p2 == seen.p1)))
    {
        return false;
    }
    let has_dequeue = ops.windows(4).any(|part| {
        let [rewind, null_row, payload, delete] = part else { return false };
        rewind.opcode == OP_Rewind && rewind.p1 == queue.p1
            && null_row.opcode == OP_NullRow && null_row.p1 == current.p1
            && payload.opcode == OP_Column && payload.p1 == queue.p1
            && payload.p2 == order as i32 + 1 && payload.p3 == current.p2
            && delete.opcode == OP_Delete && delete.p1 == queue.p1 && delete.p5 == 0
    });
    if !has_dequeue {
        return false;
    }
    let mut producers = 0;
    for (i, insert) in ops.iter().enumerate().filter(|(_, op)| op.opcode == OP_IdxInsert && op.p1 == queue.p1) {
        if !priority_queue_producer(ops, i, insert, order, columns, distinct) {
            return false;
        }
        producers += 1;
    }
    producers >= 2 && distinct.is_none_or(|seen| {
        ops.iter().filter(|op| op.p1 == seen.p1 && op.opcode == OP_IdxInsert).count() == producers
            && ops.iter().filter(|op| op.p1 == seen.p1 && op.opcode == OP_Found).count() == producers
    })
}

fn priority_queue_producer(
    ops: &[VdbeOp], index: usize, insert: &VdbeOp, order: usize,
    columns: i32, distinct: Option<&VdbeOp>,
) -> bool {
    let Some(prefix) = index.checked_sub(order + 3 + usize::from(distinct.is_some()))
        .and_then(|start| ops.get(start..index)) else {
        return false;
    };
    let [start @ .., sequence, packed] = prefix else {
        return false;
    };
    let offset = usize::from(distinct.is_some());
    let row = &start[0];
    let sort_base = packed.p1;
    let width = order as i32 + 2;
    if row.opcode != OP_MakeRecord || row.p2 != columns || row.p4 != P4::NotUsed
        || row.p3 != sort_base + width - 1 || row.p5 != 0
        || sequence.opcode != OP_Sequence || sequence.p1 != insert.p1 || sequence.p2 != sort_base + width - 2
        || packed.opcode != OP_MakeRecord || packed.p2 != width || packed.p4 != P4::NotUsed || packed.p5 != 0
        || insert.p2 != packed.p3 || insert.p3 != sort_base || insert.p4 != P4::Int32(width) || insert.p5 != 0
    {
        return false;
    }
    if let Some(seen) = distinct {
        let Some(found) = index.checked_sub(prefix.len() + 1).and_then(|i| ops.get(i)) else {
            return false;
        };
        let remember = &start[1];
        if found.opcode != OP_Found || found.p1 != seen.p1 || found.p3 != row.p1
            || found.p4 != P4::Int32(columns) || found.p5 != 0
            || usize::try_from(found.p2).ok() != Some(index + 1)
            || remember.opcode != OP_IdxInsert || remember.p1 != seen.p1 || remember.p2 != row.p3
            || remember.p3 != 0 || remember.p4 != P4::NotUsed || remember.p5 != OPFLAG_USESEEKRESULT
        {
            return false;
        }
    }
    start[1 + offset..].iter().enumerate().all(|(i, copy)| {
        copy.opcode == OP_SCopy && (row.p1..row.p1 + columns).contains(&copy.p1)
            && copy.p2 == sort_base + i as i32
    })
}

/// ORDER BY LIMIT keeps an ordered private index with a sequence field
/// and evicts the last entry before appending a better candidate.
fn limit_sort_key(prog: &Program, open: &VdbeOp, key: &KeyInfo, builtin_collations: bool) -> bool {
    let order_width = i32::from(key.n_key_field);
    let record_width = i32::from(key.n_all_field);
    if open.opcode != OP_OpenEphemeral || open.p2 != record_width || open.p3 != 0
        || open.p5 != 0 || order_width == 0 || record_width <= order_width
        || key.a_coll.len() != record_width as usize
        || !(key.a_coll.iter().flatten().all(|collation| {
            collation.name.eq_ignore_ascii_case("BINARY") && collation.enc == key.enc
        }) || (builtin_collations && builtin_order_key(key)))
    {
        return false;
    }
    let cursor = open.p1;
    let Some(key_base) = prog.a_op.windows(5).find_map(|ops| {
        let [seq, limited, last, compare, delete] = ops else { return None };
        (seq.opcode == OP_Sequence && seq.p1 == cursor
            && limited.opcode == OP_IfNotZero
            && last.opcode == OP_Last && last.p1 == cursor && last.p2 == 0
            && compare.opcode == OP_IdxLE && compare.p1 == cursor && compare.p4 == P4::Int32(order_width)
            && seq.p2 == compare.p3 + order_width
            && delete.opcode == OP_Delete && delete.p1 == cursor && delete.p2 == 0 && delete.p5 == 0)
            .then_some(compare.p3)
    }) else {
        return false;
    };
    // A result column already present in ORDER BY is stored only once, so
    // the record can occupy fewer fields than the cursor declaration.
    let stores_records = prog.a_op.windows(2).any(|ops| {
        let [record, insert] = ops else { return false };
        record.opcode == OP_MakeRecord && record.p1 == key_base
            && (order_width + 1..=record_width).contains(&record.p2)
            && matches!(record.p4, P4::NotUsed)
            && insert.opcode == OP_IdxInsert && insert.p1 == cursor && insert.p5 == 0
            && insert.p2 == record.p3 && insert.p3 == record.p1 && insert.p4 == P4::Int32(record.p2)
    });
    stores_records
        && prog.a_op.iter().any(|op| op.opcode == OP_Sort && op.p1 == cursor)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadKeyScope {
    Utf8,
    BinarySorter,
    BinaryGroup,
    BuiltinGroup,
    BinaryDistinct,
    BuiltinDistinct,
}

fn group_compare_width(prog: &Program, index: usize, op: &VdbeOp, key: &KeyInfo) -> Option<usize> {
    let width = usize::try_from(op.p3).ok()?;
    (op.opcode == OP_Compare
        && op.p5 == 0
        && (1..=usize::from(key.n_key_field)).contains(&width)
        && prog.a_op.get(index + 1).is_some_and(|next| next.opcode == OP_Jump)
        && key.a_sort_flags.get(..width).is_some()
        && key.a_coll.get(..width).is_some())
    .then_some(width)
}

fn builtin_read_key_collation(coll: &CollSeq, key_encoding: u8) -> bool {
    (coll.name.eq_ignore_ascii_case("BINARY") && coll.enc == key_encoding)
        || ((coll.name.eq_ignore_ascii_case("NOCASE") || coll.name.eq_ignore_ascii_case("RTRIM"))
            && coll.enc == crate::vdbeaux::SQLITE_UTF8)
}

/// ORDER BY puts only its ordering terms in the key prefix. The remaining
/// fields are sequence or result data, with default flags and no collation.
fn builtin_order_key(key: &KeyInfo) -> bool {
    let width = usize::from(key.n_key_field);
    let fields = usize::from(key.n_all_field);
    width > 0 && width < fields
        && key.a_coll.len() == fields && key.a_sort_flags.len() == fields
        && key.a_coll[..width].iter().all(|field| field.as_deref()
            .is_some_and(|coll| builtin_read_key_collation(coll, key.enc)))
        && key.a_coll[width..].iter().all(Option::is_none)
        && key.a_sort_flags[..width].iter().all(|&flag| flag & !(KEYINFO_ORDER_DESC | KEYINFO_ORDER_BIGNULL) == 0)
        && key.a_sort_flags[width..].iter().all(|&flag| flag == 0)
}

/// A complete ORDER BY sorter writes packed records and reads each one
/// through its paired pseudo cursor. Block sorts have a ResetSorter.
fn builtin_order_sorter(prog: &Program, open: &VdbeOp, key: &KeyInfo) -> bool {
    if open.opcode != OP_SorterOpen || open.p3 != 0 || open.p5 != 0
        || open.p2 != i32::from(key.n_all_field) || !builtin_order_key(key)
    {
        return false;
    }
    let cursor = open.p1;
    if prog.a_op.iter().any(|op| op.p1 == cursor && matches!(op.opcode, OP_ResetSorter | OP_SequenceTest)) {
        return false;
    }
    let stores_records = prog.a_op.windows(2).any(|ops| {
        let [record, insert] = ops else { return false };
        record.opcode == OP_MakeRecord && record.p4 == P4::NotUsed
            && (i32::from(key.n_key_field)..=open.p2).contains(&record.p2)
            && insert.opcode == OP_SorterInsert && insert.p1 == cursor && insert.p2 == record.p3
            && insert.p3 == record.p1 && insert.p4 == P4::Int32(record.p2) && insert.p5 == 0
    });
    stores_records && prog.a_op.windows(3).enumerate().any(|(index, ops)| {
        let [pseudo, sort, data] = ops else { return false };
        pseudo.opcode == OP_OpenPseudo && pseudo.p1 == data.p3 && pseudo.p2 == data.p2
            && sort.opcode == OP_SorterSort && sort.p1 == cursor
            && data.opcode == OP_SorterData && data.p1 == cursor
            && prog.a_op.iter().any(|next| {
                next.opcode == OP_SorterNext && next.p1 == cursor
                    && usize::try_from(next.p2).ok() == Some(index + 2)
            })
    })
}

/// A partial ORDER BY compares the satisfied prefix to detect a new block,
/// then puts only the remaining fields into that block's sorter.
fn partial_order_pairs(prog: &Program, encoding: u8) -> Vec<(usize, usize)> {
    let ops = &prog.a_op;
    ops.windows(6).enumerate().filter_map(|(start, window)| {
        let [record, first, compare, jump, flush, reset] = window else { return None };
        if record.opcode != OP_MakeRecord || record.p4 != P4::NotUsed || record.p5 != 0
            || compare.opcode != OP_Compare || compare.p5 != 0 || jump.opcode != OP_Jump
            || flush.opcode != OP_Gosub || reset.opcode != OP_ResetSorter
            || compare.p3 <= 0 || compare.p2.checked_add(compare.p3) != Some(record.p1)
            || usize::try_from(jump.p1).ok() != Some(start + 4) || jump.p1 != jump.p3
        {
            return None;
        }
        let cursor = reset.p1;
        let (open_index, open) = ops[..start].iter().enumerate().find(|(_, op)| {
            op.p1 == cursor && matches!(op.opcode, OP_SorterOpen | OP_OpenEphemeral)
        })?;
        let (P4::KeyInfo(prefix), P4::KeyInfo(suffix)) = (&compare.p4, &open.p4) else {
            return None;
        };
        let prefix_width = compare.p3 as usize;
        let suffix_width = usize::from(suffix.n_key_field);
        let fields = usize::from(suffix.n_all_field);
        let Ok(record_width) = usize::try_from(record.p2) else { return None };
        if open.p2 != record.p2 || open.p3 != 0 || open.p5 != 0
            || prefix.enc != encoding || suffix.enc != encoding
            || !builtin_order_key(prefix) || !builtin_order_key(suffix)
            || usize::from(prefix.n_key_field) != prefix_width + suffix_width
            || usize::from(prefix.n_all_field) != prefix_width + fields
            || prefix.a_sort_flags.iter().any(|&flag| flag != 0)
            || prefix.a_coll[prefix_width..prefix_width + suffix_width] != suffix.a_coll[..suffix_width]
            || record_width > fields
        {
            return None;
        }

        let (remember, insert, insert_index) = if open.opcode == OP_SorterOpen {
            let [remember, insert] = ops.get(start + 6..start + 8)? else { return None };
            if record_width < suffix_width || first.opcode != OP_SequenceTest || first.p1 != cursor
                || usize::try_from(first.p2).ok() != Some(start + 6)
                || usize::try_from(jump.p2).ok() != Some(start + 7)
                || insert.opcode != OP_SorterInsert
            {
                return None;
            }
            (remember, insert, start + 7)
        } else {
            let sequence_register = record.p1.checked_add(i32::from(suffix.n_key_field))?;
            let payload_end = record.p1.checked_add(record.p2)?;
            // A deferred result column may be loaded after the sequence, but
            // it must write only the record's non-key payload.
            let seq = ops[..start].iter().rev().find(|op| {
                !(op.opcode == OP_Column && op.p1 != cursor
                    && op.p3 > sequence_register && op.p3 < payload_end)
            })?;
            let [done, remember, bound, last, skip, delete, insert] = ops.get(start + 6..start + 13)? else {
                return None;
            };
            if record_width <= suffix_width || seq.opcode != OP_Sequence || seq.p1 != cursor
                || seq.p2 != sequence_register
                || first.opcode != OP_IfNot || first.p1 != seq.p2
                || usize::try_from(first.p2).ok() != Some(start + 7)
                || done.opcode != OP_IfNot || bound.opcode != OP_IfNotZero || done.p1 != bound.p1
                || usize::try_from(jump.p2).ok() != Some(start + 8)
                || usize::try_from(bound.p2).ok() != Some(start + 12)
                || last.opcode != OP_Last || last.p1 != cursor || last.p2 != 0
                || skip.opcode != OP_IdxLE || skip.p1 != cursor || skip.p3 != record.p1
                || skip.p4 != P4::Int32(i32::from(suffix.n_key_field))
                || delete.opcode != OP_Delete || delete.p1 != cursor || delete.p2 != 0 || delete.p5 != 0
                || insert.opcode != OP_IdxInsert
            {
                return None;
            }
            (remember, insert, start + 12)
        };
        if remember.opcode != OP_Move || remember.p1 != compare.p2 || remember.p2 != compare.p1 || remember.p3 != compare.p3
            || insert.p1 != cursor || insert.p2 != record.p3 || insert.p3 != record.p1
            || insert.p4 != P4::Int32(record.p2) || insert.p5 != 0
        {
            return None;
        }
        let drain_start = usize::try_from(flush.p2).ok()?;
        if drain_start <= insert_index
            || !ops.get(insert_index + 1..drain_start)?.iter().any(|op| {
                op.opcode == OP_Gosub && op.p1 == flush.p1 && op.p2 == flush.p2
            })
            || !partial_order_drain(ops, drain_start, flush.p1, open)
        {
            return None;
        }
        Some((open_index, start + 2))
    }).collect()
}

fn partial_order_drain(ops: &[VdbeOp], start: usize, return_reg: i32, open: &VdbeOp) -> bool {
    let cursor = open.p1;
    let (loop_start, source, next_opcode) = if open.opcode == OP_SorterOpen {
        let Some([once, pseudo, sort, data]) = ops.get(start..start + 4) else { return false };
        if once.opcode != OP_Once || usize::try_from(once.p2).ok() != Some(start + 2)
            || pseudo.opcode != OP_OpenPseudo || pseudo.p1 != data.p3 || pseudo.p2 != data.p2 || pseudo.p3 < open.p2
            || sort.opcode != OP_SorterSort || sort.p1 != cursor || data.opcode != OP_SorterData || data.p1 != cursor
        {
            return false;
        }
        (start + 3, pseudo.p1, OP_SorterNext)
    } else {
        if !ops.get(start).is_some_and(|op| op.opcode == OP_Sort && op.p1 == cursor) {
            return false;
        }
        (start + 1, cursor, OP_Next)
    };
    let Some(drain) = ops.get(loop_start..).and_then(|tail| {
        tail.iter().position(|op| op.opcode == OP_Return && op.p1 == return_reg).map(|end| &tail[..end])
    }) else {
        return false;
    };
    drain.iter().any(|op| op.opcode == OP_Column && op.p1 == source && (0..open.p2).contains(&op.p2))
        && drain.iter().any(|op| op.opcode == OP_ResultRow)
        && drain.last().is_some_and(|next| {
            next.opcode == next_opcode && next.p1 == cursor && usize::try_from(next.p2).ok() == Some(loop_start)
        })
}

/// GROUP BY shares one KeyInfo between its sorter and register comparison. Keep
/// additional UTF-16 collation support tied to that pair.
fn builtin_group_key(prog: &Program, key: &Arc<KeyInfo>) -> bool {
    if !key.a_coll.iter().flatten().all(|coll| builtin_read_key_collation(coll, key.enc)) {
        return false;
    }
    let shares_key = |op: &VdbeOp| {
        matches!(&op.p4, P4::KeyInfo(other) if Arc::ptr_eq(key, other))
    };
    prog.a_op.iter().any(|op| op.opcode == OP_SorterOpen && shares_key(op))
        && prog.a_op.iter().enumerate().any(|(index, op)| {
            shares_key(op)
                && group_compare_width(prog, index, op, key)
                    .is_some_and(|width| key.a_coll[..width].iter().all(Option::is_some))
        })
}

/// GROUP BY can consume consecutive rows from a physical index instead of a
/// sorter. The comparison must use the same collations as the scanned fields.
fn streamed_group_key(prog: &Program, index: usize, op: &VdbeOp, key: &KeyInfo) -> bool {
    let Some(width) = group_compare_width(prog, index, op, key) else {
        return false;
    };
    let Some(start) = index.checked_sub(width) else {
        return false;
    };
    let columns = &prog.a_op[start..index];
    let cursor = columns[0].p1;
    let jump = &prog.a_op[index + 1];
    if jump.p1 != jump.p3 || !columns.iter().enumerate().all(|(field, column)| {
        column.opcode == OP_Column && column.p1 == cursor
            && column.p2 == field as i32 && column.p3 == op.p2 + field as i32
    }) {
        return false;
    }
    let source_key = prog.a_op[..start].iter().rev().find_map(|open| {
        match &open.p4 {
            P4::KeyInfo(source) if open.opcode == OP_OpenRead && open.p1 == cursor
                && open.p2 > 1 && open.p3 == 0 => Some(source),
            _ => None,
        }
    });
    source_key.is_some_and(|source| {
        source.enc == key.enc && width <= usize::from(source.n_key_field)
            && source.a_coll.get(..width).is_some_and(|collations| {
                collations.iter().zip(&key.a_coll[..width]).all(|(physical, group)| {
                    group.as_deref().is_some_and(|group| {
                        builtin_read_key_collation(group, key.enc)
                            && physical.as_deref().map_or_else(
                                || group.name.eq_ignore_ascii_case("BINARY"),
                                |physical| physical.enc == group.enc
                                    && physical.name.eq_ignore_ascii_case(&group.name),
                            )
                    })
                })
            })
    }) && prog.a_op[index + 2..].iter().any(|next| {
        matches!(next.opcode, OP_Next | OP_Prev) && next.p1 == cursor
            && usize::try_from(next.p2).is_ok_and(|target| target <= start)
    })
}

/// WHERE_MULTI_OR visits one rowid table through a reusable index cursor. Each
/// branch seeks with its own KeyInfo and passes an unseen rowid to the body.
fn rowid_or_index_cursor(prog: &Program, cursor: i32, encoding: u8) -> bool {
    let ops = &prog.a_op;
    let mut branches = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        if op.opcode == OP_OpenDup && op.p2 == cursor {
            return false;
        }
        if op.p1 != cursor {
            continue;
        }
        match op.opcode {
            OP_ReopenIdx => branches.push(index),
            OP_OpenRead | OP_OpenWrite | OP_OpenDup | OP_OpenAutoindex | OP_OpenEphemeral
            | OP_OpenPseudo | OP_SorterOpen | OP_VOpen => return false,
            _ => {}
        }
    }
    let [first, _, ..] = branches.as_slice() else { return false };
    let first = *first;
    let Some([null, ret]) = first.checked_sub(2).and_then(|start| ops.get(start..first)) else {
        return false;
    };
    let end = ret.p1 as usize;
    let Some(goto) = ops.get(end) else { return false };
    let Some(body_return) = (goto.p2 as usize).checked_sub(1).and_then(|at| ops.get(at)) else {
        return false;
    };
    if null.opcode != OP_Null || null.p1 != 0 || null.p2 <= 0 || null.p3 != 0
        || ret.opcode != OP_Integer || ret.p2 <= 0 || ret.p2 == null.p2
        || end <= *branches.last().unwrap() || goto.opcode != OP_Goto || goto.p2 as usize <= end + 1
        || body_return.opcode != OP_Return || body_return.p1 != ret.p2
        || body_return.p2 as usize != end + 1 || body_return.p3 != 0
    {
        return false;
    }

    let mut table = None;
    let mut root_keys = BTreeMap::new();
    for (branch, &start) in branches.iter().enumerate() {
        let stop = branches.get(branch + 1).copied().unwrap_or(end);
        let open = &ops[start];
        let P4::KeyInfo(key) = &open.p4 else { return false };
        if open.p2 <= 1 || open.p3 != 0 || !matches!(open.p5, 0 | OPFLAG_SEEKEQ)
            || key.enc != encoding || *root_keys.entry(open.p2).or_insert(&open.p4) != &open.p4
        {
            return false;
        }
        let Some(tail) = stop.checked_sub(5) else { return false };
        let Some([defer, rowid, set, call, next]) = ops.get(tail..stop) else { return false };
        let set_number = if stop == end { -1 } else { branch as i32 };
        if defer.opcode != OP_DeferredSeek || defer.p1 != cursor || defer.p2 != 0 || defer.p3 == cursor
            || defer.p5 != 0 || !matches!(&defer.p4, P4::IntArray(map)
                if map.iter().all(|&field| field <= u32::from(key.n_all_field)))
            || *table.get_or_insert(defer.p3) != defer.p3
            || rowid.opcode != OP_IdxRowid || rowid.p1 != cursor || rowid.p2 <= 0
            || set.opcode != OP_RowSetTest || set.p1 != null.p2 || set.p3 != rowid.p2
            || set.p2 as usize != stop - 1 || set.p4 != P4::Int32(set_number)
            || call.opcode != OP_Gosub || call.p1 != ret.p2 || call.p2 as usize != end + 1
            || next.opcode != OP_Next || next.p1 != cursor || next.p5 != 0
        {
            return false;
        }
        let bound = next.p2 as usize;
        let Some(prefix) = ops.get(start + 1..bound) else { return false };
        let mut seeks = prefix.iter().filter(|op| op.p1 == cursor && matches!(op.opcode, OP_SeekGE | OP_SeekGT));
        let Some(seek) = seeks.next() else { return false };
        let Some(limit) = ops.get(bound) else { return false };
        if seeks.next().is_some() || bound >= tail || seek.p2 as usize != stop
            || !matches!(seek.p4, P4::Int32(width) if (1..=i32::from(key.n_key_field)).contains(&width))
            || !matches!(limit.opcode, OP_IdxGE | OP_IdxGT) || limit.p1 != cursor || limit.p2 as usize != stop
            || limit.p4 != seek.p4 || seek.p5 != 0 || limit.p5 != 0
            || (open.p5 == OPFLAG_SEEKEQ && (seek.opcode != OP_SeekGE || limit.opcode != OP_IdxGT
                || !std::ptr::eq(seek, &ops[bound - 1]) || seek.p3 != limit.p3))
        {
            return false;
        }
    }
    ops[..first].iter().any(|op| op.opcode == OP_OpenRead && Some(op.p1) == table
        && op.p2 > 1 && op.p3 == 0 && op.p5 == 0 && matches!(op.p4, P4::Int32(n) if n > 0))
}

fn unsupported(
    prog: &Program, write: bool, key_scope: ReadKeyScope, row_case_encoding: Option<u8>,
    builtin_collations: bool,
) -> Option<String> {
    if !prog.p_program.is_empty() && !write {
        return Some("sub-programs".to_owned());
    }
    let has_group_sorter = matches!(key_scope, ReadKeyScope::BinaryGroup | ReadKeyScope::BuiltinGroup)
        && prog.a_op.iter().any(|op| op.opcode == OP_SorterOpen);
    let partial_order = match row_case_encoding {
        Some(encoding) if builtin_collations && key_scope == ReadKeyScope::BinarySorter => partial_order_pairs(prog, encoding),
        _ => Vec::new(),
    };
    let mut reopen_or_checked = BTreeMap::new();
    let subs = prog.p_program.iter().flat_map(|s| s.a_op.iter());
    for (index, op) in prog.a_op.iter().chain(subs).enumerate() {
        let ok = (op.opcode == OP_Expire && op.p1 != 0 && op.p2 == 1) || matches!(
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
                | OP_TypeCheck
                | OP_VOpen
                | OP_VFilter
                | OP_VColumn
                | OP_VNext
                | OP_VInitIn
                | OP_VCheck
        ) || (write
            && matches!(
                op.opcode,
                OP_OpenWrite
                    | OP_IdxDelete
                    | OP_Clear
                    | OP_ResetCount
                    | OP_Program
                    | OP_Param
                    | OP_FkCounter
                    | OP_FkIfZero
                    | OP_FkCheck
                    | OP_CursorLock
                    | OP_CursorUnlock
                    | OP_RowCell
                    | OP_VBegin
                    | OP_VUpdate
                    | OP_VOpen
                    | OP_VFilter
                    | OP_VColumn
                    | OP_VNext
                    | OP_AutoCommit
                    | OP_Savepoint
            ));
        if !ok {
            return Some(format!("opcode {}", sqlite3_opcode_name(op.opcode)));
        }
        match op.opcode {
            OP_Transaction if op.p2 != 0 && !write => return Some("write transaction".to_owned()),
            OP_IdxInsert if op.p5 & OPFLAG_PREFORMAT != 0 => return Some("OPFLAG_PREFORMAT (index)".to_owned()),
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
            let binary_in_key_encoding =
                |c: &CollSeq| c.name.eq_ignore_ascii_case("BINARY") && c.enc == k.enc;
            let binary_sort = key_scope != ReadKeyScope::Utf8
                && op.opcode == OP_SorterOpen
                && k.a_coll.iter().flatten().all(|c| binary_in_key_encoding(c));
            let binary_group_compare = has_group_sorter
                && group_compare_width(prog, index, op, k).is_some_and(|width| {
                    k.a_coll[..width].iter().all(|c| c.as_deref().is_some_and(binary_in_key_encoding))
                });
            let builtin_group = key_scope == ReadKeyScope::BuiltinGroup
                && row_case_encoding == Some(k.enc)
                && (op.opcode == OP_SorterOpen
                    || group_compare_width(prog, index, op, k).is_some_and(|width| {
                        k.a_coll[..width].iter().all(Option::is_some)
                    }))
                && builtin_group_key(prog, k);
            let streamed_group = key_scope == ReadKeyScope::BuiltinGroup
                && row_case_encoding == Some(k.enc) && streamed_group_key(prog, index, op, k);
            let distinct = matches!(key_scope, ReadKeyScope::BinaryDistinct | ReadKeyScope::BuiltinDistinct)
                && matches!(k.enc, 2 | 3)
                && unordered_distinct_key(prog, op, k, |coll| {
                    binary_in_key_encoding(coll)
                        || (key_scope == ReadKeyScope::BuiltinDistinct && row_case_encoding == Some(k.enc)
                            && builtin_read_key_collation(coll, k.enc))
                });
            // Static BINARY fields (including implicit rowid tails) have no CollSeq.
            let physical_index = row_case_encoding == Some(k.enc)
                && op.p2 > 1 && op.p3 == 0
                && k.n_key_field > 0 && k.n_key_field <= k.n_all_field
                && k.a_coll.len() == usize::from(k.n_all_field)
                && k.a_coll.iter().flatten().all(|c| {
                    binary_in_key_encoding(c) || (builtin_collations && builtin_read_key_collation(c, k.enc))
                })
                && (op.opcode == OP_OpenRead || (op.opcode == OP_ReopenIdx
                    && *reopen_or_checked.entry(op.p1)
                        .or_insert_with(|| rowid_or_index_cursor(prog, op.p1, k.enc))));
            let membership = row_case_encoding == Some(k.enc)
                && ordered_in_key(prog, op, k, |coll| {
                    binary_in_key_encoding(coll) || (builtin_collations && builtin_read_key_collation(coll, k.enc))
                });
            let builtin_sort = builtin_collations && row_case_encoding == Some(k.enc)
                && builtin_order_sorter(prog, op, k);
            let limit_sort = row_case_encoding == Some(k.enc) && limit_sort_key(prog, op, k, builtin_collations);
            let partial_sort = partial_order.iter().any(|&(open, compare)| index == open || index == compare);
            let recursive_fifo = row_case_encoding == Some(k.enc)
                && recursive_fifo_key(prog, index, op, k, |coll| {
                    binary_in_key_encoding(coll) || (builtin_collations && builtin_read_key_collation(coll, k.enc))
                });
            let rowid_list = row_case_encoding == Some(k.enc)
                && integer_probe_list_key(prog, index, op, k, |coll| {
                    binary_in_key_encoding(coll) || (builtin_collations && builtin_read_key_collation(coll, k.enc))
                });
            let recursive_priority = row_case_encoding == Some(k.enc)
                && recursive_priority_key(prog, index, |coll| {
                    binary_in_key_encoding(coll) || (builtin_collations && builtin_read_key_collation(coll, k.enc))
                });
            if k.enc != 1 && !prog.references_like
                && !(binary_sort || binary_group_compare || builtin_group || streamed_group
                    || distinct || physical_index
                    || membership || builtin_sort || limit_sort || partial_sort || recursive_fifo || rowid_list || recursive_priority)
            {
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

/// `BTREE_FDK_RANGE`
const BTREE_FDK_RANGE: i32 = 10;

/// `vdbeSkipField(mask, iCol, pMem1, pMem2, bIntegrity)`
fn vdbe_skip_field(mask: u64, i_col: usize, m1: &Mem, m2: &Mem, b_integrity: bool) -> bool {
    // BTREE_ULPDISTORTION
    if i_col >= 64 || mask & (1u64 << i_col) == 0 {
        return false;
    }
    if !b_integrity {
        return true;
    }
    if m1.flags & MEM_REAL != 0 && m2.flags & MEM_REAL != 0 {
        let (a, b) = (m1.r.to_bits(), m2.r.to_bits());
        if (if a < b { b - a } else { a - b }) <= 2 {
            return true;
        }
    }
    false
}

/// `vdbeIsMatchingIndexKey(pCur, bInt, mask, p, &res)`: `Ok(res)`, 0 for a match.
fn vdbe_is_matching_index_key(
    bt: &mut Btree,
    cur: &mut BtCursor,
    b_int: bool,
    mask: u64,
    ki: &KeyInfoRt,
    a_mem: &[Mem],
) -> Result<i32> {
    let n_rec = bt.bc_payload_size(cur)?;
    if n_rec > 0x7fff_ffff {
        return Err(corrupt());
    }
    // (5 extra zero bytes: getVarint32 may read past the end of a corrupt record)
    let mut a_rec = vec![0u8; n_rec as usize + 5];
    bt.bc_payload(cur, 0, &mut a_rec[..n_rec as usize])?;
    let (sz_hdr, mut idx_hdr) = get_varint32(&a_rec, 0);
    if sz_hdr > 98307 {
        // rc = SQLITE_CORRUPT
        return Err(corrupt());
    }
    let mut res = 0;
    let mut idx_rec = sz_hdr;
    let mut m = Mem::default();
    for ii in 0..usize::from(ki.n_all_field) {
        if idx_hdr as u32 >= sz_hdr {
            return Err(corrupt());
        }
        let (i_serial, n) = get_varint32(&a_rec, idx_hdr);
        idx_hdr += n;
        let n_serial = sqlite3_vdbe_serial_type_len(i_serial);
        if idx_rec + n_serial > n_rec {
            return Err(corrupt());
        }
        sqlite3_vdbe_serial_get(&a_rec[idx_rec as usize..], i_serial, &mut m);
        let Some(p_mem) = a_mem.get(ii) else {
            break;
        };
        if !vdbe_skip_field(mask, ii, p_mem, &m, b_int) {
            let coll = ki.a_coll.get(ii).and_then(|c| c.as_deref());
            res = sqlite3_mem_compare(&m, p_mem, coll);
            if res != 0 {
                break;
            }
        }
        idx_rec += n_serial;
    }
    Ok(res)
}

/// `sqlite3VdbeFindIndexKey(pCur, pIdx, p, &res, bIntegrity)` with the mask of `pIdx`
/// computed by the host ([`VdbeHost::index_expr_mask`]): `Ok(res)`, 0 when the cursor was left
/// on a matching entry.
fn find_index_key(
    bt: &mut Btree,
    cur: &mut BtCursor,
    mask: u64,
    ki: &KeyInfoRt,
    a_mem: &[Mem],
    b_integrity: bool,
) -> Result<i32> {
    let mut res = 1;
    if mask != 0 {
        // Move the cursor back BTREE_FDK_RANGE entries; at EOF scan the whole index.
        let mut done = false;
        let mut ii = 0;
        while !cur.eof() && ii < BTREE_FDK_RANGE {
            done = !bt.bc_previous(cur)?;
            ii += 1;
        }
        let mut n_step: i32 = if done {
            res = i32::from(bt.bc_first(cur)?);
            -1
        } else {
            BTREE_FDK_RANGE * 2
        };
        while cur.is_valid() {
            let mut ii = 0;
            while ii < n_step || n_step < 0 {
                res = vdbe_is_matching_index_key(bt, cur, b_integrity, mask, ki, a_mem)?;
                if res == 0 {
                    break;
                }
                if !bt.bc_next(cur)? {
                    // SQLITE_DONE -> SQLITE_OK
                    break;
                }
                ii += 1;
            }
            if n_step < 0 || res == 0 || b_integrity {
                break;
            }
            // the first, non-exhaustive search failed: search the entire index
            n_step = -1;
            res = i32::from(bt.bc_first(cur)?);
        }
    }
    Ok(res)
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
            preformat_cell: Vec::new(),
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
            expired_on_reset: false,
            expire_seq: 0,
            n_vm_step: 0,
            call_steps: 0,
            n_change: 0,
            progress_limit: u64::MAX,
            progress_ops: 0,
            tr_pc: usize::MAX,
            err_pc: usize::MAX,
            frames: Vec::new(),
            cur_sub: None,
            a_frame: Vec::new(),
            sub_rt: Vec::new(),
            n_fk_constraint: 0,
            tr_sql: String::new(),
            tr_undef: false,
            not_halted: false,
            halt_rc: None,
            value_lists: Vec::new(),
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

    /// The virtual tables the statement has an open cursor on that is not at its end (an r-tree
    /// refuses writes while such a cursor exists: `rtreeUpdate`, SQLITE_LOCKED_VTAB).
    pub fn positioned_vtabs(&self) -> Vec<Arc<P4Vtab>> {
        self.ap_csr
            .iter()
            .flatten()
            .filter_map(|c| c.vcur.as_ref())
            .filter(|v| !v.cur.eof())
            .map(|v| Arc::clone(&v.vt))
            .collect()
    }

    /// The row `OP_ResultRow` produced (`sqlite3_column_*`).
    pub fn result_row(&self) -> &[Mem] {
        &self.a_mem[self.result_row.0..self.result_row.0 + self.result_row.1]
    }

    /// Binds the collations of the program to the connection.
    fn make_rt<H: VdbeHost>(&mut self, host: &mut H) -> Result<()> {
        let prog = Arc::clone(&self.prog);
        self.rt = Self::bind_ops(&prog.a_op, host)?;
        self.rt_ready = true;
        Ok(())
    }

    /// The P4 operands of `a_op` bound to the connection (collations, key infos).
    fn bind_ops<H: VdbeHost>(a_op: &[VdbeOp], host: &mut H) -> Result<Vec<OpRt>> {
        let mut rt = vec![OpRt::None; a_op.len()];
        let find = |c: &CollSeq, host: &mut H| -> Result<Arc<CollRt>> {
            if let Some(mut b) = builtin_coll(&c.name) {
                b.enc = c.enc;
                return Ok(Arc::new(b));
            }
            match host.find_collation(&c.name) {
                Some(c) => Ok(Arc::new(c)),
                None => Err(SqliteError::new(ErrorCode::Error, format!("no such collation sequence: {}", c.name))),
            }
        };
        for (i, op) in a_op.iter().enumerate() {
            match &op.p4 {
                P4::CollSeq(c) => rt[i] = OpRt::Coll(find(c, host)?),
                P4::KeyInfo(k) => {
                    let k: &KeyInfo = k;
                    let mut a_coll = Vec::with_capacity(k.a_coll.len());
                    for c in &k.a_coll {
                        a_coll.push(match c {
                            Some(c) => Some(find(c, host)?),
                            None => None,
                        });
                    }
                    rt[i] = OpRt::KeyInfo(Arc::new(KeyInfoRt {
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
        Ok(rt)
    }

    /// `allocateCursor(p, iCur, nField, eCurType)`
    fn allocate_cursor<H: VdbeHost>(&mut self, host: &mut H, i_cur: usize, n_field: i32, e_cur_type: u8) -> Result<()> {
        if self.ap_csr[i_cur].is_some() {
            self.free_cursor(host, i_cur);
        }
        let n = n_field.max(0) as usize;
        let dummy = Box::new(BtCursor::detached());
        self.ap_csr[i_cur] = Some(Box::new(VdbeCursor {
            vcur: None,
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
        let Some(pc) = self.ap_csr[i_cur].take() else {
            return;
        };
        self.free_cursor_nn(host, pc);
    }

    /// `sqlite3VdbeFreeCursorNN(p, pCx)`
    fn free_cursor_nn<H: VdbeHost>(&mut self, host: &mut H, mut pc: Box<VdbeCursor>) {
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
        let mut later: Vec<Box<VdbeCursor>> = Vec::new();
        if !self.frames.is_empty() {
            // "for(pFrame=p->pFrame; pFrame->pParent; pFrame=pFrame->pParent);
            //  sqlite3VdbeFrameRestore(pFrame);": the cursors of the innermost frame are closed,
            // the root frame's state comes back; the frames in between are deleted after the
            // cursors of the main program (`pDelFrame`, `sqlite3VdbeFrameDelete`), outermost
            // first.
            for i in 0..self.ap_csr.len() {
                self.free_cursor(host, i);
            }
            for f in self.frames.iter_mut().skip(1) {
                later.extend(f.ap_csr.drain(..).flatten());
            }
            while let Some(f) = self.frames.pop() {
                self.restore_frame(host, f);
            }
            self.a_frame.clear();
        }
        for i in 0..self.ap_csr.len() {
            self.free_cursor(host, i);
        }
        for pc in later {
            self.free_cursor_nn(host, pc);
        }
        self.a_frame.clear();
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
        for f in self.frames.iter_mut().rev() {
            for pc in f.ap_csr.iter_mut().rev().flatten() {
                if pc.e_cur_type == CURTYPE_BTREE
                    && pc.btx == Some(btx)
                    && pc.pgno_root == root
                    && let Some(e) = self.ephem.get_mut(btx).and_then(|e| e.as_mut())
                {
                    e.bt.bc_save_for_write(&mut pc.cur)?;
                }
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
        // the cursors of the frames that called the running trigger program
        for f in self.frames.iter_mut().rev() {
            for pc in f.ap_csr.iter_mut().rev().flatten() {
                if pc.e_cur_type == CURTYPE_BTREE && pc.btx.is_none() && pc.i_db == i_db && pc.pgno_root == root {
                    bt.bc_save_for_write(&mut pc.cur)?;
                }
            }
        }
        Ok(())
    }

    /// `OP_Program` (the part after the recursion and depth checks): a frame for sub-program
    /// `sp`, called by the instruction at `pc` whose `P3` register `reg` keeps the frame
    /// (`pRt`). The sub-program runs from its first instruction afterwards.
    fn push_frame<H: VdbeHost>(&mut self, host: &mut H, sp: Arc<SubProgram>, pc: usize, op: &VdbeOp) -> Result<()> {
        let reg = op.p3 as usize;
        if self.a_frame.len() <= reg {
            self.a_frame.resize_with(reg + 1, || None);
        }
        // "if( (pRt->flags&MEM_Blob)==0 )": a new frame, its registers MEM_Undefined
        let child = match self.a_frame[reg].take() {
            Some(f) if self.a_mem[reg].flags & MEM_BLOB != 0 => f,
            _ => {
                let n_mem = (sp.n_mem + sp.n_csr + i32::from(sp.n_csr == 0)).max(1) as usize;
                let m = &mut self.a_mem[reg];
                m.z.clear();
                m.flags = MEM_BLOB;
                if let Some(r) = self.a_rowset.get_mut(reg) {
                    *r = None;
                }
                Box::new(FrameRt {
                    a_mem: vec![Mem { flags: MEM_UNDEFINED, enc: 1, ..Mem::default() }; n_mem + 1],
                    ap_csr: (0..sp.n_csr.max(0)).map(|_| None).collect(),
                    a_agg: Vec::new(),
                    a_rowset: Vec::new(),
                    a_frame: Vec::new(),
                })
            }
        };
        let key = Arc::as_ptr(&sp) as usize;
        let rt = match self.sub_rt.iter().position(|(k, _)| *k == key) {
            Some(i) => self.sub_rt.swap_remove(i).1,
            None => Self::bind_ops(&sp.a_op, host)?,
        };
        let FrameRt { a_mem, ap_csr, a_agg, a_rowset, a_frame } = *child;
        let n_op = sp.a_op.len();
        let frame = VdbeFrame {
            parent_sub: self.cur_sub.take(),
            a_mem: std::mem::replace(&mut self.a_mem, a_mem),
            ap_csr: std::mem::replace(&mut self.ap_csr, ap_csr),
            a_agg: std::mem::replace(&mut self.a_agg, a_agg),
            a_rowset: std::mem::replace(&mut self.a_rowset, a_rowset),
            a_frame: std::mem::replace(&mut self.a_frame, a_frame),
            rt: std::mem::replace(&mut self.rt, rt),
            // "memset(pFrame->aOnce, 0, (pProgram->nOp + 7)/8)"
            once: std::mem::replace(&mut self.once, vec![0; n_op]),
            pc,
            prog_p1: op.p1,
            prog_p2: op.p2,
            reg,
            token: sp.token,
            sub: Arc::clone(&sp),
            last_rowid: host.last_rowid(),
            n_change: self.n_change,
            n_db_change: host.db_changes(),
        };
        self.frames.push(frame);
        self.n_change = 0;
        self.cur_sub = Some(sp);
        Ok(())
    }

    /// `sqlite3VdbeFrameRestore(pFrame)`: the cursors of the running frame are closed and the
    /// state of the frame's parent comes back. The run time of the frame goes back into the
    /// parent's register `P3` of the `OP_Program`.
    fn restore_frame<H: VdbeHost>(&mut self, host: &mut H, f: VdbeFrame) {
        // closeCursorsInFrame(v)
        for i in 0..self.ap_csr.len() {
            self.free_cursor(host, i);
        }
        let child = Box::new(FrameRt {
            a_mem: std::mem::replace(&mut self.a_mem, f.a_mem),
            ap_csr: std::mem::replace(&mut self.ap_csr, f.ap_csr),
            a_agg: std::mem::replace(&mut self.a_agg, f.a_agg),
            a_rowset: std::mem::replace(&mut self.a_rowset, f.a_rowset),
            a_frame: std::mem::replace(&mut self.a_frame, f.a_frame),
        });
        let rt = std::mem::replace(&mut self.rt, f.rt);
        self.sub_rt.push((Arc::as_ptr(&f.sub) as usize, rt));
        self.once = f.once;
        self.cur_sub = f.parent_sub;
        if self.a_frame.len() <= f.reg {
            self.a_frame.resize_with(f.reg + 1, || None);
        }
        self.a_frame[f.reg] = Some(child);
        host.set_last_rowid(f.last_rowid);
        self.n_change = f.n_change;
        host.set_db_changes(f.n_db_change);
    }

    /// `aMem[i]` of the root frame (`OP_NewRowid` P3, `OP_MemMax` P1: the AUTOINCREMENT
    /// registers of the main program).
    fn root_mem(&mut self, i: usize) -> &mut Mem {
        match self.frames.first_mut() {
            Some(f) => &mut f.a_mem[i],
            None => &mut self.a_mem[i],
        }
    }

    /// The instructions being run (`p->aOp`).
    fn cur_ops(&self) -> &[VdbeOp] {
        match &self.cur_sub {
            Some(s) => &s.a_op,
            None => &self.prog.a_op,
        }
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
        self.expired_on_reset = false;
        self.n_change = 0;
        // sqlite3VdbeRewind: "p->nFkConstraint = 0"
        self.n_fk_constraint = 0;
        self.tr_pc = usize::MAX;
        self.err_pc = usize::MAX;
        self.not_halted = false;
        self.halt_rc = None;
    }

    /// `sqlite3_stmt_status(pStmt, op, resetFlag)` for the counters the machine keeps
    /// (`SQLITE_STMTSTATUS_FULLSCAN_STEP` 1, `SORT` 2, `AUTOINDEX` 3, `VM_STEP` 4, `REPREPARE` 5, `RUN` 6,
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
        self.not_halted = false;
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
            Err(e) if self.not_halted => Err(e),
            Err(e) => {
                // abort_due_to_error: sqlite3VdbeHalt(p)
                #[cfg(sqliters_dtrace)]
                {
                    if sqliters_types::dtrace_enabled() && self.frames.is_empty() && self.tr_pc < self.cur_ops().len() && !self.halted {
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

        // One pass per frame switch: `a_op` is `p->aOp` (the main program or the sub-program of
        // the innermost frame).
        'frames: loop {
        let hold_sub = self.cur_sub.clone();
        let a_op: &[VdbeOp] = match &hold_sub {
            Some(s) => &s.a_op,
            None => &prog.a_op,
        };
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

                // vdbe.c:8386–8393. Global/immediate expiration remains outside
                // the supported subset; runOnlyOnce PRAGMAs use this self/advisory form.
                OP_Expire if op.p1 != 0 && op.p2 == 1 => self.expired_on_reset = true,

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
                    if !self.frames.is_empty() && op.p1 == 0 {
                        // Halt the sub-program: control returns to the parent frame.
                        let f = self.frames.pop().ok_or_else(|| internal("frame"))?;
                        // sqlite3VdbeSetChanges(db, p->nChange)
                        host.set_changes(self.n_change);
                        let mut pcx = f.pc;
                        if op.p2 == OE_IGNORE {
                            // the sub-program throws IGNORE: P2 of the calling OP_Program
                            pcx = (f.prog_p2 - 1) as usize;
                        }
                        self.restore_frame(host, f);
                        #[cfg(sqliters_dtrace)]
                        {
                            // (C prints the halted instruction against the parent's aOp: out of
                            // range, unless the parent runs the same sub-program — recursion)
                            let same = match (&hold_sub, &self.cur_sub) {
                                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                                _ => false,
                            };
                            if !same {
                                tr_prev = None;
                            }
                        }
                        pc = pcx + 1;
                        continue 'frames;
                    }
                    #[cfg(sqliters_dtrace)]
                    {
                        // (in a frame C prints the halted instruction against the main program's
                        // aOp, which sqlite3VdbeHalt has restored: out of range)
                        if sqliters_types::dtrace_enabled() && self.frames.is_empty() {
                            self.trace_op(pc);
                        }
                        // (printed once: not again on the error path of `exec`)
                        self.tr_pc = usize::MAX;
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
                    let encoding = host.text_encoding();
                    let m = &mut self.a_mem[op.p2 as usize];
                    m.z.clear();
                    if op.opcode == OP_String8 && encoding != sqliters_types::TextEncoding::Utf8 {
                        m.z = crate::utf::translate_bytes(&z[..n], 1, encoding as u8);
                        if m.z.len() as i64 > i64::from(limit_length) { return Err(too_big()); }
                    } else {
                        m.z.extend_from_slice(&z[..n]);
                    }
                    m.enc = encoding as u8;
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
                    m.enc = host.text_encoding() as u8;
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
                    let encoding = host.text_encoding();
                    if (flags1 | self.a_mem[p2].flags) & MEM_NULL != 0 {
                        self.a_mem[p3].set_null();
                    } else {
                        // (the string forms are computed on copies: C restores the flags of
                        // the operands afterwards)
                        let mut a = self.a_mem[p1].clone();
                        if a.flags & (MEM_STR | MEM_BLOB) == 0 {
                            a.stringify(false);
                            if encoding != sqliters_types::TextEncoding::Utf8 {
                                let text = String::from_utf8_lossy(&a.z).into_owned();
                                a.z.clear();
                                encoding.encode_into(&text, &mut a.z);
                                a.enc = encoding as u8;
                            }
                        } else if a.flags & MEM_ZERO != 0 {
                            a.expand_blob();
                        }
                        let mut b = self.a_mem[p2].clone();
                        if b.flags & (MEM_STR | MEM_BLOB) == 0 {
                            b.stringify(false);
                            if encoding != sqliters_types::TextEncoding::Utf8 {
                                let text = String::from_utf8_lossy(&b.z).into_owned();
                                b.z.clear();
                                encoding.encode_into(&text, &mut b.z);
                                b.enc = encoding as u8;
                            }
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
                        if encoding != sqliters_types::TextEncoding::Utf8 { z.truncate(z.len() & !1); }
                        out.z = z;
                        out.set_type_flag(MEM_STR);
                        out.flags |= MEM_TERM;
                        out.enc = encoding as u8;
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
                    if !self.a_mem[op.p1 as usize].must_be_integer() {
                        if op.p2 == 0 {
                            return Err(SqliteError::new(ErrorCode::Mismatch, "datatype mismatch"));
                        }
                        jump!(op.p2);
                    }
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
                    m.cast_with_encoding(op.p2 as u8, host.text_encoding() as u8);
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
                    if !self.frames.is_empty() {
                        // pFrame->aOnce: one bit per instruction, cleared at every OP_Program
                        if self.once[pc] != 0 {
                            jump!(op.p2);
                        }
                        self.once[pc] = 1;
                    } else {
                        if self.init_p1 == self.once[pc] {
                            jump!(op.p2);
                        }
                        self.once[pc] = self.init_p1;
                    }
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
                    let encoding = host.text_encoding() as u8;
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
                                    P4::Mem(v) => {
                                        // C pre-encodes sqlite3ColumnDefault in ENC(db).
                                        self.a_mem[p3].set_from_value(v);
                                        self.a_mem[p3].change_encoding(encoding);
                                    }
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
                                dest.enc = encoding;
                                dest.flags = if t & 1 != 0 { MEM_STR | MEM_TERM } else { MEM_BLOB };
                            } else {
                                sqlite3_vdbe_serial_get(&pc_.a_row[off.min(pc_.a_row.len())..], t, dest);
                            }
                        } else {
                            // vdbe.c:3353: overflow results carry the database encoding.
                            self.a_mem[p3].enc = encoding;
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
                                dest.enc = encoding;
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
                                    dest.enc = encoding;
                                } else {
                                    let buf = sqlite3_vdbe_mem_from_btree(bt, &mut pc_.cur, off as u32, len)?;
                                    sqlite3_vdbe_serial_get(&buf, t, dest);
                                    dest.enc = encoding;
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
                    // C cells reaching record serialization carry database encoding.
                    for cell in &mut self.a_mem[d0..d0 + n_field] {
                        cell.change_encoding(host.text_encoding() as u8);
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
                    // "if( pOp->p2 && (db->flags & (SQLITE_QueryOnly|SQLITE_CorruptRdOnly))!=0 )"
                    if op.p2 != 0 && (host.query_only() || host.corrupt_rdonly()) {
                        if host.query_only() {
                            return Err(SqliteError::new(ErrorCode::ReadOnly, "attempt to write a readonly database"));
                        }
                        return Err(SqliteError::new(ErrorCode::Corrupt, "database disk image is malformed"));
                    }
                    let r = if op.p2 != 0 { host.begin_write_trans(op.p1 as usize, op.p2) } else { host.begin_read_trans(op.p1 as usize) };
                    let (i_meta, i_gen) = match r {
                        Ok(x) => x,
                        Err(e) if e.code == ErrorCode::Busy => {
                            // "if( (rc&0xff)==SQLITE_BUSY ){ p->pc = (int)(pOp - aOp); p->rc = rc;
                            //   goto vdbe_return; }"
                            self.pc = pc as i32;
                            self.not_halted = true;
                            return Err(e);
                        }
                        Err(e) => return Err(e),
                    };
                    let p4 = match op.p4 {
                        P4::Int32(i) => i,
                        _ => 0,
                    };
                    if op.p5 != 0 && (i_meta as i32 != op.p3 || i_gen != p4) {
                        return Err(SqliteError::new(ErrorCode::Schema, "database schema has changed"));
                    }
                }

                OP_Savepoint => {
                    let p1 = op.p1;
                    let z_name = p4_text(&op.p4).map(|z| String::from_utf8_lossy(z).into_owned()).unwrap_or_default();
                    if p1 == SAVEPOINT_BEGIN {
                        if host.n_vdbe_write() > 0 {
                            // A new savepoint cannot be created if there are active write
                            // statements (i.e. open read/write incremental blob handles).
                            return Err(SqliteError::new(ErrorCode::Busy, "cannot open savepoint - SQL statements in progress"));
                        }
                        // "rc = sqlite3VtabSavepoint(db, SAVEPOINT_BEGIN, db->nStatement+db->nSavepoint);
                        //  if( rc!=SQLITE_OK ) goto abort_due_to_error;"
                        host.vtab_savepoint(SAVEPOINT_BEGIN, 0)?;
                        // "if( db->autoCommit ){ db->autoCommit = 0; db->isTransactionSavepoint = 1; }
                        //  else{ db->nSavepoint++; }" (+ pNew->nDeferredCons, nDeferredImmCons)
                        let transaction = host.auto_commit();
                        host.savepoint_open(&z_name, transaction)?;
                    } else {
                        // Find the named savepoint. If there is no such savepoint, then an
                        // an error is returned to the user.
                        let Some(k) = host.savepoint_find(&z_name) else {
                            return Err(SqliteError::new(ErrorCode::Error, format!("no such savepoint: {z_name}")));
                        };
                        if host.n_vdbe_write() > 0 && p1 == SAVEPOINT_RELEASE {
                            return Err(SqliteError::new(
                                ErrorCode::Busy,
                                "cannot release savepoint - SQL statements in progress",
                            ));
                        }
                        // Determine whether or not this is a transaction savepoint. If so,
                        // and this is a RELEASE command, then the current transaction is
                        // committed.
                        let is_transaction = host.savepoint_is_transaction(k);
                        if is_transaction && p1 == SAVEPOINT_RELEASE {
                            if host.fk_deferred_pending()? {
                                // "if( (rc = sqlite3VdbeCheckFkDeferred(p))!=SQLITE_OK ) goto vdbe_return;"
                                self.not_halted = true;
                                return Err(fk_error());
                            }
                            host.set_auto_commit(true);
                            match host.halt_transaction(true) {
                                Err(e) => {
                                    // "if( sqlite3VdbeHalt(p)==SQLITE_BUSY ){ p->pc = (int)(pOp - aOp);
                                    //   db->autoCommit = 0; p->rc = rc = SQLITE_BUSY; goto vdbe_return; }"
                                    host.set_auto_commit(false);
                                    self.pc = pc as i32;
                                    self.not_halted = true;
                                    return Err(e);
                                }
                                Ok(Some(e)) => {
                                    // "rc = p->rc; if( rc ){ db->autoCommit = 0; }" ... "if( rc ) goto
                                    // abort_due_to_error;" (the statement has halted)
                                    host.set_auto_commit(false);
                                    self.halt(host);
                                    self.halt_rc = Some(e.clone());
                                    return Err(e);
                                }
                                Ok(None) => {
                                    // "db->isTransactionSavepoint = 0;" (the VM halted:
                                    // p->eVdbeState==VDBE_HALT_STATE)
                                    host.set_transaction_savepoint(false);
                                    self.halt(host);
                                }
                            }
                        } else {
                            let is_schema_change;
                            if p1 == SAVEPOINT_ROLLBACK {
                                is_schema_change = host.savepoint_schema_change(k);
                                for ii in 0..host.n_db() {
                                    host.trip_all_cursors(ii, !is_schema_change)?;
                                }
                            } else {
                                is_schema_change = false;
                            }
                            for ii in 0..host.n_db() {
                                host.btree_savepoint(ii, p1, k)?;
                            }
                            if is_schema_change {
                                host.savepoint_schema_reset(k)?;
                            }
                        }
                        // Regardless of whether this is a RELEASE or ROLLBACK, destroy all
                        // savepoints nested inside of the savepoint being operated on; a
                        // RELEASE destroys the savepoint too, a ROLLBACK TO restores the
                        // deferred constraint counters it saved.
                        host.savepoint_close(k, p1 == SAVEPOINT_RELEASE);
                        if !is_transaction || p1 == SAVEPOINT_ROLLBACK {
                            host.vtab_savepoint(p1, k)?;
                        }
                    }
                    if self.halted {
                        // "if( p->eVdbeState==VDBE_HALT_STATE ){ rc = SQLITE_DONE; goto vdbe_return; }"
                        return Ok(false);
                    }
                }

                OP_AutoCommit => {
                    let desired_auto_commit = op.p1 != 0;
                    let i_rollback = op.p2 != 0;
                    if desired_auto_commit != host.auto_commit() {
                        if i_rollback {
                            host.rollback_all()?;
                            host.set_auto_commit(true);
                        } else if desired_auto_commit && host.n_vdbe_write() > 0 {
                            // If this instruction implements a COMMIT and other VMs are writing
                            // return an error indicating that the other VMs must complete first.
                            return Err(SqliteError::new(
                                ErrorCode::Busy,
                                "cannot commit transaction - SQL statements in progress",
                            ));
                        } else if host.fk_deferred_pending()? {
                            // "else if( (rc = sqlite3VdbeCheckFkDeferred(p))!=SQLITE_OK ){ goto vdbe_return; }"
                            self.not_halted = true;
                            return Err(fk_error());
                        } else {
                            host.set_auto_commit(desired_auto_commit);
                        }
                        let read_only = self.prog.read_only;
                        match host.halt_transaction(read_only) {
                            Err(e) => {
                                // "if( sqlite3VdbeHalt(p)==SQLITE_BUSY ){ p->pc = (int)(pOp - aOp);
                                //   db->autoCommit = (u8)(1-desiredAutoCommit); p->rc = rc = SQLITE_BUSY;
                                //   goto vdbe_return; }"
                                host.set_auto_commit(!desired_auto_commit);
                                self.pc = pc as i32;
                                self.not_halted = true;
                                return Err(e);
                            }
                            Ok(rc) => {
                                self.halt(host);
                                // sqlite3CloseSavepoints(db)
                                host.close_savepoints();
                                // "if( p->rc==SQLITE_OK ){ rc = SQLITE_DONE; }else{ rc = SQLITE_ERROR; }"
                                if let Some(e) = rc {
                                    self.halt_rc = Some(e.clone());
                                    return Err(e);
                                }
                                return Ok(false);
                            }
                        }
                    }
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        if !desired_auto_commit {
                            "cannot start a transaction within a transaction"
                        } else if i_rollback {
                            "cannot rollback - no transaction is active"
                        } else {
                            "cannot commit - no transaction is active"
                        },
                    ));
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
                        // OP_Found tests DISTINCT before OP_MakeRecord normalizes text.
                        // A default supplied for an old row can still use a different encoding.
                        let mixed_encoding_distinct_probe = op.opcode == OP_Found && pc_.is_ephemeral && !pc_.is_ordered
                            && matches!(ki.enc, 2 | 3)
                            && ki.a_coll.first().is_some_and(|coll| {
                                coll.as_ref().is_some_and(|coll| coll.is_binary && coll.enc == ki.enc)
                            })
                            && r.a_mem[0].flags & MEM_STR != 0 && r.a_mem[0].enc != ki.enc;
                        if !mixed_encoding_distinct_probe {
                            r.find_compare();
                        }
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
                        // AUTOINCREMENT: the register of the largest rowid so far, in the root
                        // frame when a trigger program runs
                        let m = match self.frames.first_mut() {
                            Some(f) => &mut f.a_mem[op.p3 as usize],
                            None => &mut self.a_mem[op.p3 as usize],
                        };
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
                    // "if( pOp->p4type==P4_TABLE && HAS_UPDATE_HOOK(db) ) pTab = pOp->p4.pTab"
                    let hook_tab = match &op.p4 {
                        P4::Table(t) if host.has_update_hook() => Some(Arc::clone(t)),
                        _ => None,
                    };
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
                    // BTREE_PREFORMAT: the cell sqlite3BtreeTransferRow prepared (OP_RowCell)
                    let preformat = op.p5 & OPFLAG_PREFORMAT != 0;
                    let data: &[u8] = if preformat { &self.preformat_cell } else { &self.a_mem[op.p2 as usize].z };
                    bt.bc_set_preformat(preformat);
                    let rc = bt.bc_insert(
                        &mut c.cur,
                        n_key,
                        data,
                        (op.p5 & (OPFLAG_APPEND | OPFLAG_SAVEPOSITION)) as u8,
                        seek_result,
                        None,
                    );
                    bt.bc_set_preformat(false);
                    c.deferred_moveto = false;
                    c.cache_status = CACHE_STALE;
                    col_cache_ctr = col_cache_ctr.wrapping_add(1);
                    rc?;
                    // Invoke the update-hook if required.
                    if let Some(t) = hook_tab {
                        let i_db = c.i_db as usize;
                        let op_code = if op.p5 & OPFLAG_ISUPDATE != 0 { 23 } else { 18 };
                        host.update_hook(op_code, i_db, &t.name, n_key)?;
                    }
                }

                OP_RowCell => {
                    // rc = sqlite3BtreeTransferRow(pDest->uc.pCursor, pSrc->uc.pCursor, iKey): the
                    // payload of the source cell (local part and overflow chain). The cell of the
                    // destination is built by the OP_Insert / OP_IdxInsert with OPFLAG_PREFORMAT
                    // that follows; its overflow pages are allocated as sqlite3BtreeTransferRow
                    // allocates them (`nearby` 0), before that insert changes any page.
                    let src = csr!(op.p2);
                    let bt = btree_of!(src);
                    if !src.cur.is_valid() {
                        return Err(corrupt());
                    }
                    let n = bt.bc_payload_size(&mut src.cur)? as usize;
                    self.preformat_cell.clear();
                    self.preformat_cell.resize(n, 0);
                    bt.bc_payload(&mut src.cur, 0, &mut self.preformat_cell)?;
                }

                OP_VOpen => {
                    let P4::Vtab(vt) = &op.p4 else {
                        return Err(internal("OP_VOpen without P4_VTAB"));
                    };
                    let ci = op.p1 as usize;
                    // a cursor already open on this table is reused
                    let reuse = self.ap_csr[ci]
                        .as_deref()
                        .is_some_and(|c| c.e_cur_type == CURTYPE_VTAB && c.vcur.as_ref().is_some_and(|v| v.vt.id == vt.id));
                    if !reuse {
                        let cur = host.vtab_open(vt)?;
                        self.allocate_cursor(host, ci, 0, CURTYPE_VTAB)?;
                        csr!(ci).vcur = Some(VCursor { vt: Arc::clone(vt), cur });
                    }
                }

                OP_VInitIn => {
                    // "pRhs->pCsr = pC->uc.pCursor; pRhs->pOut = &aMem[pOp->p3]; pOut->flags =
                    //  MEM_Null; sqlite3VdbeMemSetPointer(pOut, pRhs, "ValueList", ...)"
                    let out = &mut self.a_mem[op.p2 as usize];
                    out.set_null();
                    out.flags = MEM_NULL | MEM_SUBTYPE;
                    out.e_subtype = b'p';
                    let reg = op.p2 as usize;
                    self.value_lists.retain(|(r, _)| *r != reg);
                    self.value_lists.push((reg, op.p1 as usize));
                }

                OP_VCheck => {
                    // "pOut = &aMem[pOp->p2]; sqlite3VdbeMemSetNull(pOut);" then
                    // "rc = pModule->xIntegrity(pVtab, db->aDb[pOp->p1].zDbSName, pTab->zName,
                    //  pOp->p3, &zErr)"
                    self.a_mem[op.p2 as usize].set_null();
                    let name = match &op.p4 {
                        P4::Table(t) => t.name.clone(),
                        P4::Vtab(v) => v.name.clone(),
                        _ => return Err(internal("OP_VCheck without P4_TABLEREF")),
                    };
                    let vt = P4Vtab { name, ..P4Vtab::default() };
                    if let Some(z_err) = host.vtab_integrity(&vt, &op.p1.to_string(), op.p3 != 0)? {
                        self.a_mem[op.p2 as usize].set_str(z_err.as_bytes());
                    }
                }

                OP_VFilter => {
                    let p3 = op.p3 as usize;
                    let i_query = self.a_mem[p3].i as i32;
                    let n_arg = self.a_mem[p3 + 1].i.max(0) as usize;
                    let args: Vec<Value> = self.a_mem[p3 + 2..p3 + 2 + n_arg].iter().map(Mem::to_value).collect();
                    // the arguments that are ValueList pointers of OP_VInitIn: the values of
                    // their cursors (what sqlite3_vtab_in_first / _next return)
                    let mut in_lists: Vec<Option<Vec<Value>>> = vec![None; n_arg];
                    for (ai, slot) in in_lists.iter_mut().enumerate() {
                        let reg = p3 + 2 + ai;
                        let m = &self.a_mem[reg];
                        if m.flags & MEM_SUBTYPE != 0
                            && m.e_subtype == b'p'
                            && let Some(&(_, ci)) = self.value_lists.iter().find(|(r, _)| *r == reg)
                        {
                            let c = csr!(ci);
                            let ki = c.key_info.clone();
                            let bt = btree_of!(c);
                            let mut vals = Vec::new();
                            let mut eof = bt.bc_first(&mut c.cur)?;
                            while !eof {
                                let n = bt.bc_payload_size(&mut c.cur)? as usize;
                                let mut key = vec![0u8; n];
                                bt.bc_payload(&mut c.cur, 0, &mut key)?;
                                if let Some(ki) = &ki {
                                    let mems = sqliters_vdbe_record_first(ki, &key);
                                    vals.push(mems);
                                }
                                eof = !bt.bc_next(&mut c.cur)?;
                            }
                            c.cache_status = CACHE_STALE;
                            *slot = Some(vals);
                        }
                    }
                    let idx_str = p4_text(&op.p4).map(|z| String::from_utf8_lossy(z).into_owned());
                    // sqlite3_value_subtype(apArg[i]) (json_each / json_tree of a JSON argument)
                    let subtypes: Vec<u8> = self.a_mem[p3 + 2..p3 + 2 + n_arg]
                        .iter()
                        .map(|m| if m.flags & MEM_SUBTYPE != 0 { m.e_subtype } else { 0 })
                        .collect();
                    let c = csr!(op.p1);
                    let Some(vc) = c.vcur.as_mut() else {
                        return Err(internal("OP_VFilter on a cursor that is not a virtual-table cursor"));
                    };
                    if subtypes.iter().any(|&t| t != 0) {
                        vc.cur.set_arg_subtypes(&subtypes);
                    }
                    host.vtab_filter_in(&vc.vt, vc.cur.as_mut(), i_query, idx_str.as_deref(), &args, &in_lists)?;
                    let res = vc.cur.eof();
                    c.null_row = false;
                    if res {
                        jump!(op.p2);
                    }
                }

                OP_VColumn => {
                    let c = csr!(op.p1);
                    let p3 = op.p3 as usize;
                    if c.null_row {
                        self.a_mem[p3].set_null();
                    } else {
                        let Some(vc) = c.vcur.as_mut() else {
                            return Err(internal("OP_VColumn on a cursor that is not a virtual-table cursor"));
                        };
                        if op.p5 & OPFLAG_NOCHNG != 0 && host.vtab_column_skips_nochange(&vc.vt, op.p2 as usize) {
                            // "sqlite3VdbeMemSetNull(pDest); pDest->flags = MEM_Null|MEM_Zero;
                            //  pDest->u.nZero = 0;" (the module leaves the result unset)
                            let m = &mut self.a_mem[p3];
                            m.set_null();
                            m.flags = MEM_NULL | MEM_ZERO;
                            m.n_zero = 0;
                        } else {
                            let v = host.vtab_column(&vc.vt, vc.cur.as_mut(), op.p2 as usize)?;
                            // (sqlite3_result_subtype of xColumn: json_each / json_tree 'J')
                            let st = vc.cur.column_subtype(op.p2 as usize);
                            let m = &mut self.a_mem[p3];
                            m.set_from_value(&v);
                            if st != 0 {
                                m.flags |= MEM_SUBTYPE;
                                m.e_subtype = st;
                            }
                        }
                    }
                }

                OP_VNext => {
                    let c = csr!(op.p1);
                    if c.null_row {
                        pc += 1;
                        continue;
                    }
                    let Some(vc) = c.vcur.as_mut() else {
                        return Err(internal("OP_VNext on a cursor that is not a virtual-table cursor"));
                    };
                    host.vtab_next(&vc.vt, vc.cur.as_mut())?;
                    if !vc.cur.eof() {
                        jump!(op.p2);
                    }
                }

                OP_VBegin => {
                    // rc = sqlite3VtabBegin(db, pVTab); sqlite3VtabImportErrmsg(p, pVTab->pVtab)
                    let P4::Vtab(vt) = &op.p4 else {
                        return Err(internal("OP_VBegin without P4_VTAB"));
                    };
                    host.vtab_begin(vt)?;
                }

                OP_VUpdate => {
                    let P4::Vtab(vt) = &op.p4 else {
                        return Err(internal("OP_VUpdate without P4_VTAB"));
                    };
                    let n_arg = op.p2 as usize;
                    let p3 = op.p3 as usize;
                    let args: Vec<Value> = self.a_mem[p3..p3 + n_arg].iter().map(Mem::to_value).collect();
                    // sqlite3_value_nochange(): "(p->flags&(MEM_Null|MEM_Zero))==(MEM_Null|MEM_Zero)"
                    let nochange: Vec<bool> = self.a_mem[p3..p3 + n_arg]
                        .iter()
                        .map(|m| m.flags & (MEM_NULL | MEM_ZERO) == (MEM_NULL | MEM_ZERO))
                        .collect();
                    let on_conflict = op.p5 as u8;
                    let r = host.vtab_update(vt, &args, &nochange, on_conflict);
                    let r = match r {
                        Ok(rowid) => {
                            if op.p1 != 0 {
                                host.set_last_rowid(rowid);
                            }
                            self.n_change += 1;
                            Ok(())
                        }
                        Err(mut e) if e.code == ErrorCode::Constraint && host.vtab_constraint_support(vt) => {
                            if i32::from(on_conflict) == OE_IGNORE {
                                Ok(())
                            } else {
                                // p->errorAction = ((pOp->p5==OE_Replace) ? OE_Abort : pOp->p5)
                                e.conflict_effect = match i32::from(on_conflict) {
                                    OE_ROLLBACK => ConflictResolutionEffect::Rollback,
                                    OE_FAIL => ConflictResolutionEffect::Fail,
                                    _ => ConflictResolutionEffect::Abort,
                                };
                                Err(e)
                            }
                        }
                        Err(e) => {
                            self.n_change += 1;
                            Err(e)
                        }
                    };
                    r?;
                }

                OP_Delete => {
                    let ci = op.p1 as usize;
                    save_cursors!(ci);
                    let hook_tab = match &op.p4 {
                        P4::Table(t) if host.has_update_hook() => Some(Arc::clone(t)),
                        _ => None,
                    };
                    let c = csr!(ci);
                    let bt = btree_of!(c);
                    if hook_tab.is_some() && op.p5 & OPFLAG_SAVEPOSITION != 0 && c.is_table {
                        // pC->movetoTarget = sqlite3BtreeIntegerKey(pC->uc.pCursor)
                        c.moveto_target = bt.bc_integer_key(&mut c.cur)?;
                    }
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
                        // "if( db->xUpdateCallback && ALWAYS(pTab!=0) && HasRowid(pTab) )"
                        if let Some(t) = hook_tab {
                            let (i_db, target) = (c.i_db as usize, c.moveto_target);
                            host.update_hook(9, i_db, &t.name, target)?;
                        }
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
                    } else if let Some(vc) = pc_.vcur.as_mut() {
                        // "else if( pC->eCurType==CURTYPE_VTAB ){ rc = pModule->xRowid(pC->uc.pVCur, &v); }"
                        let v = host.vtab_rowid(&vc.vt, vc.cur.as_mut())?;
                        let out = &mut self.a_mem[p2];
                        out.flags = MEM_INT;
                        out.i = v;
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
                    let res = if res != 0 {
                        // rc = sqlite3VdbeFindIndexKey(pCrsr, pOp->p4.pIdx, &r, &res, 0)
                        let mask = match &op.p4 {
                            P4::Index(x) => host.index_expr_mask(c.i_db as usize, &x.name),
                            _ => 0,
                        };
                        let bt = btree_of!(c);
                        find_index_key(bt, &mut c.cur, mask, &ki, &self.a_mem[p2..p2 + n_field], false)?
                    } else {
                        0
                    };
                    let c = csr!(ci);
                    let bt = btree_of!(c);
                    if res != 0 {
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
                    // pIn1: the register of the root frame
                    self.root_mem(op.p1 as usize).integerify();
                    self.a_mem[op.p2 as usize].integerify();
                    let v2 = self.a_mem[op.p2 as usize].i;
                    let m1 = self.root_mem(op.p1 as usize);
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
                        Some(AggCtx::Win(w)) => {
                            // (pCtx->isError: "sqlite3VdbeError(p, "%s", sqlite3_value_text(pCtx->pOut))")
                            if let Err(msg) = w.step(op.p1 != 0, &mut self.a_mem[p2..p2 + argc]) {
                                return Err(SqliteError::new(ErrorCode::Error, msg));
                            }
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
                    let mut rv: Mem = match self.a_agg[reg].as_mut() {
                        Some(AggCtx::MinMax { best, .. }) => best.clone().unwrap_or_else(Mem::null),
                        Some(AggCtx::Win(w)) => w.value(!is_value).unwrap_or_else(Mem::null),
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
                    // OP_AggFinal and OP_AggValue expose text in the database encoding.
                    rv.change_encoding(host.text_encoding() as u8);
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
                    let mut out: Mem;
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
                    } else if func.func_flags & SQLITE_FUNC_BUILTIN != 0 && argc == 1 && func.name.eq_ignore_ascii_case("zeroblob") {
                        // zeroblobFunc: sqlite3_result_zeroblob64(context, n) — a MEM_Zero blob
                        let n = self.a_mem[p2].int_value().max(0);
                        if n > i64::from(limit_length) {
                            return Err(too_big());
                        }
                        let mut m = Mem::null();
                        m.set_zero_blob(n as i32);
                        out = m;
                    } else {
                        let args: Vec<RuntimeValue> = self.a_mem[p2..p2 + argc].iter().map(Mem::to_runtime_value).collect();
                        let rv = if op.opcode == OP_PureFunc {
                            host.call_pure_function(func, &args, op.p5)?
                        } else {
                            host.call_function(func, &args)?
                        };
                        out = mem_from_runtime(&rv);
                    }
                    // vdbeapi.c:sqlite3_result_text/value stores text in pCtx->enc.
                    let encoding = host.text_encoding();
                    if out.flags & MEM_STR != 0 && out.enc != encoding as u8 {
                        if let Value::Text(text) = out.to_value() {
                            out.z.clear();
                            encoding.encode_into(&text, &mut out.z);
                            out.enc = encoding as u8;
                        }
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
                        if self.frames.is_empty() {
                            self.init_p1 += 1;
                        } else {
                            // (the OP_Init of a sub-program counts in its own aOp[0].p1, which
                            // only the OP_Once of the main program read)
                            #[cfg(sqliters_dtrace)]
                            {
                                if sqliters_types::dtrace_enabled() {
                                    eprintln!("TR|vm|sql|{}", self.tr_sql);
                                }
                            }
                        }
                        self.a_counter[SQLITE_STMTSTATUS_RUN] += 1;
                        jump!(op.p2);
                    }
                }

                OP_Program => {
                    let P4::SubProgram(sp0) = &op.p4 else {
                        return Err(internal("OP_Program without a sub-program"));
                    };
                    let sp = if sp0.is_stub() {
                        match prog.p_program.get(sp0.n_csr as usize) {
                            Some(s) => Arc::clone(s),
                            None => return Err(internal("sub-program stub without a sub-program")),
                        }
                    } else {
                        Arc::clone(sp0)
                    };
                    // P5: recursive invocation of the trigger is disabled
                    if op.p5 != 0 && self.frames.iter().any(|f| f.token == sp.token) {
                        pc += 1;
                        continue;
                    }
                    if self.frames.len() >= host.trigger_depth_limit() {
                        return Err(SqliteError::new(ErrorCode::Error, "triggers nested too deep"));
                    }
                    #[cfg(sqliters_dtrace)]
                    {
                        // (C prints OP_Program against the sub-program's aOp: out of range, unless
                        // the sub-program calls itself)
                        if !hold_sub.as_ref().is_some_and(|h| Arc::ptr_eq(h, &sp)) {
                            tr_prev = None;
                        }
                    }
                    self.push_frame(host, sp, pc, op)?;
                    // "pOp = &aOp[-1]; goto check_for_interrupt;"
                    pc = 0;
                    check_int!();
                    continue 'frames;
                }

                OP_Param => {
                    let Some(f) = self.frames.last() else {
                        return Err(internal("OP_Param outside a frame"));
                    };
                    // pFrame->aMem[pOp->p1 + pFrame->aOp[pFrame->pc].p1]
                    let v = f.a_mem[(op.p1 + f.prog_p1) as usize].clone();
                    self.a_mem[op.p2 as usize] = v;
                }

                OP_CursorLock => {
                    // sqlite3BtreeCursorPin(pC->uc.pCursor)
                    csr!(op.p1).cur.cur_flags |= sqliters_btree::btcursor::BTCF_PINNED;
                }

                OP_CursorUnlock => {
                    csr!(op.p1).cur.cur_flags &= !sqliters_btree::btcursor::BTCF_PINNED;
                }

                OP_FkCounter => {
                    if op.p1 != 0 {
                        host.add_deferred_cons(false, i64::from(op.p2));
                    } else if host.defer_fks() {
                        host.add_deferred_cons(true, i64::from(op.p2));
                    } else {
                        self.n_fk_constraint += i64::from(op.p2);
                    }
                }

                OP_FkIfZero => {
                    let (n_def, n_def_imm) = host.deferred_cons();
                    if op.p1 != 0 {
                        if n_def == 0 && n_def_imm == 0 {
                            jump!(op.p2);
                        }
                    } else if self.n_fk_constraint == 0 && n_def_imm == 0 {
                        jump!(op.p2);
                    }
                }

                OP_FkCheck => {
                    // sqlite3VdbeCheckFkImmediate
                    if self.n_fk_constraint != 0 {
                        return Err(fk_error());
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
        if let Some(f) = WinFn::of(func) {
            return Ok(AggCtx::Win(Box::new(WinCtx::new(f))));
        }
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
        let Some(op) = self.cur_ops().get(pc) else {
            return;
        };
        // C rewrites these opcodes when they first run
        let opcode = match op.opcode {
            OP_String8 => OP_String,
            OP_AggStep | OP_AggInverse => OP_AggStep1,
            o => o,
        };
        let prop = OPCODE_PROPERTY[opcode as usize];
        let mut w = String::new();
        // p->nMem - p->nCursor (a frame: nChildMem - nChildCsr)
        let n_reg = match &self.cur_sub {
            Some(s) => s.n_mem + i32::from(s.n_csr == 0),
            None => self.prog.n_mem - self.prog.n_cursor,
        };
        let reg = |i: i32| -> String {
            if i > 0 && i <= n_reg && self.tr_undef {
                "U".to_owned()
            } else if i > 0 && i <= n_reg {
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
    pub fn trace_sql(&mut self, sql: &str) {
        #[cfg(sqliters_dtrace)]
        {
            if sqliters_types::dtrace_enabled() {
                let z: String = sql.chars().take(300).map(|c| if c == '\n' { ' ' } else { c }).collect();
                eprintln!("TR|vm|sql|{z}");
                self.tr_sql = z;
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
