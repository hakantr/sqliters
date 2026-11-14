//! `sqliters-btree`: SQLite Format 3 B-Tree & B+Tree storage engine,
//! overflow chains, freelist page management, and `PRAGMA integrity_check`.
#![forbid(unsafe_code)]

mod balance;
pub mod cursor;
mod insert;
mod integrity;
pub mod page;
mod pins;
pub mod ptrmap;

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub use sqliters_pager::{AutoVacuumMode, BackupTracker};
use sqliters_pager::{
    JournalMode, LockingMode, PageLoadCheck, Pager, PagerOpenOptions, TransactionState,
    WalCheckpointMode, WalCheckpointResult, create_super_journal_file,
};
use sqliters_types::{
    ErrorCode, IntRangeProbe, KeySortOrder, Pgno, RawField, RawRecordKey, Result, SqliteError,
    TextEncoding, Value, compare_raw_record_range, compare_raw_records_with_encoding,
    compare_records_with_encoding, decode_record_with_encoding, decode_serial_value_with_encoding,
    extended, get_varint, get_varint_i64, parse_raw_record_prefix, push_varint, put_varint,
    serial_type_len, varint_len,
};
use sqliters_vfs::{OpenFlags, Vfs};

pub use cursor::{
    BTCURSOR_MAX_DEPTH, BtreeCursorState, CursorId, CursorKind, CursorPagePos, CursorStack,
    SavedCursorKey, SkipNext,
};
pub use insert::AppendCursor;
pub use integrity::IntegrityReport;
pub use page::{
    BtreePageType, IndexInteriorCell, IndexLeafCell, ParsedPage, PayloadBounds, RawIndexCell,
    RawIndexInteriorStep, RawIndexLeafView, RawLeafDeleteOutcome, RawTableLeafCell,
    TableInteriorCell, TableLeafCell, apply_delete_index_leaf_in_place, binary_search_table_leaf,
    binary_search_table_leaf_with_span, defragment_raw_leaf_in_place, find_table_interior_child,
    find_table_interior_child_append_hint, last_table_leaf_rowid, peek_page_type,
    probe_delete_index_leaf_in_place, probe_delete_index_leaf_raw, probe_index_interior_slot_raw,
    raw_cell_offset, raw_page_free_space, raw_page_header, read_index_interior_cell,
    read_index_leaf_cell, read_table_interior_cell, read_table_leaf_cell, read_table_leaf_rowid,
    seek_table_leaf_cell, table_interior_right_child, table_leaf_cell_has_rowid,
    try_delete_table_leaf_in_place,
};
pub use pins::{IdxScan, VC_READ, VC_READ2, VC_WRITE};
pub use ptrmap::{PtrmapType, RootPageMove, is_ptrmap_page, ptrmap_offset, ptrmap_pageno};

/// Column projection descriptor for single-row `rowid` point `SELECT` fast path.
#[derive(Clone, Debug)]
pub enum PointProjCol {
    /// Projects the row's `rowid` / `INTEGER PRIMARY KEY` (`Value::Integer(rowid)`).
    Rowid,
    /// Projects stored record column `col_idx` (0-based within the physical table record),
    /// substituting `default_val` (or `Value::Null` when `None`) if the stored record omits it.
    Column {
        col_idx: u16,
        default_val: Option<Value>,
    },
}

/// Page allocation search policy (`BTALLOC_ANY`, `BTALLOC_EXACT`, `BTALLOC_LE` in `btree.c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocMode {
    /// `allocateBtreePage(.., nearby = 0, BTALLOC_ANY)`: the first leaf of the first trunk.
    Any,
    /// `allocateBtreePage(.., nearby, BTALLOC_ANY)`: the leaf of the first trunk whose page
    /// number is closest to `nearby` (`Near(0)` is `Any`).
    Near(Pgno),
    Exact(Pgno),
    Le(Pgno),
}

/// Outcome of [`Btree::seek_table_row_local`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableRowSeek {
    /// No row with that `rowid`.
    Missing,
    /// Row found with its whole record on the leaf (copied out); `(leaf_pgno, cell_idx)` is
    /// where it sits.
    Local { leaf_pgno: Pgno, cell_idx: usize },
    /// Row found, but its record is not available as one in-page slice (nothing copied): it
    /// continues on overflow pages, or the page is a 64 KiB page.
    Spilled,
}

/// [`compare_raw_record_range`] with the all-integer shortcut of an [`IntRangeProbe`] built
/// from the same arguments: the probe answers for the records it can decide, and every other
/// record takes the general comparison.
#[inline(always)]
fn compare_index_cell_range(
    probe: Option<&IntRangeProbe>,
    rec: &[u8],
    prefix_values: &[Value],
    lower_bound: Option<(&Value, bool)>,
    upper_bound: Option<(&Value, bool)>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Ordering> {
    if let Some(p) = probe
        && let Some(ord) = p.compare(rec)
    {
        return Ok(ord);
    }
    compare_raw_record_range(rec, prefix_values, lower_bound, upper_bound, orders, enc)
}

#[inline]
fn decode_projected_cols_from_record(
    payload: &[u8],
    rowid: i64,
    cols: &[PointProjCol],
    enc: TextEncoding,
) -> Result<Vec<Value>> {
    let mut max_col: Option<usize> = None;
    for c in cols {
        if let PointProjCol::Column { col_idx, .. } = c {
            let ci = *col_idx as usize;
            max_col = Some(max_col.map_or(ci, |m| m.max(ci)));
        }
    }
    let Some(max_ci) = max_col else {
        let mut out = Vec::with_capacity(cols.len());
        for _ in cols {
            out.push(Value::Integer(rowid));
        }
        return Ok(out);
    };
    if payload.is_empty() {
        let mut out = Vec::with_capacity(cols.len());
        for c in cols {
            match c {
                PointProjCol::Rowid => out.push(Value::Integer(rowid)),
                PointProjCol::Column { default_val, .. } => {
                    out.push(default_val.clone().unwrap_or(Value::Null));
                }
            }
        }
        return Ok(out);
    }

    let (hdr_u64, mut hdr_pos) = get_varint(payload)?;
    let hdr_len = usize::try_from(hdr_u64)
        .ok()
        .filter(|&h| h <= payload.len() && hdr_pos <= h)
        .ok_or_else(|| SqliteError::corrupt("record header exceeds record length"))?;
    let mut body_pos = hdr_len;

    if max_ci < 32 {
        let mut serials = [0u64; 32];
        let mut offs = [0usize; 32];
        let mut lens = [0usize; 32];
        let mut parsed = 0usize;
        while parsed <= max_ci && hdr_pos < hdr_len {
            let (st, used) = get_varint(&payload[hdr_pos..hdr_len])?;
            let len = serial_type_len(st)?;
            let next_b = body_pos
                .checked_add(len)
                .filter(|&nb| nb <= payload.len())
                .ok_or_else(|| SqliteError::corrupt("record payload truncated"))?;
            serials[parsed] = st;
            offs[parsed] = body_pos;
            lens[parsed] = len;
            hdr_pos += used;
            body_pos = next_b;
            parsed += 1;
        }

        let mut out = Vec::with_capacity(cols.len());
        for c in cols {
            match c {
                PointProjCol::Rowid => out.push(Value::Integer(rowid)),
                PointProjCol::Column {
                    col_idx,
                    default_val,
                } => {
                    let ci = *col_idx as usize;
                    if ci >= parsed {
                        out.push(default_val.clone().unwrap_or(Value::Null));
                    } else {
                        let start = offs[ci];
                        let len = lens[ci];
                        out.push(decode_serial_value_with_encoding(
                            serials[ci],
                            &payload[start..start + len],
                            enc,
                        )?);
                    }
                }
            }
        }
        return Ok(out);
    }

    let mut meta: Vec<(u64, usize, usize)> = Vec::with_capacity(max_ci + 1);
    while meta.len() <= max_ci && hdr_pos < hdr_len {
        let (st, used) = get_varint(&payload[hdr_pos..hdr_len])?;
        let len = serial_type_len(st)?;
        let next_b = body_pos
            .checked_add(len)
            .filter(|&nb| nb <= payload.len())
            .ok_or_else(|| SqliteError::corrupt("record payload truncated"))?;
        meta.push((st, body_pos, len));
        hdr_pos += used;
        body_pos = next_b;
    }

    let mut out = Vec::with_capacity(cols.len());
    for c in cols {
        match c {
            PointProjCol::Rowid => out.push(Value::Integer(rowid)),
            PointProjCol::Column {
                col_idx,
                default_val,
            } => {
                let ci = *col_idx as usize;
                if let Some(&(st, start, len)) = meta.get(ci) {
                    out.push(decode_serial_value_with_encoding(
                        st,
                        &payload[start..start + len],
                        enc,
                    )?);
                } else {
                    out.push(default_val.clone().unwrap_or(Value::Null));
                }
            }
        }
    }
    Ok(out)
}

/// SQLite B-Tree storage manager over a [`Pager`].
pub struct Btree {
    pager: Pager,
    pub(crate) cursor_slots: Vec<cursor::CursorSlot>,
    pub(crate) free_cursor_slots: Vec<u32>,
    pub(crate) open_cursor_count: usize,
    pub(crate) next_cursor_gen: u32,
    interrupt_flag: Option<Arc<AtomicBool>>,
    /// `PRAGMA secure_delete` state of this database (`BtShared.btsFlags & BTS_FAST_SECURE`):
    /// `0` = off, `1` = `BTS_SECURE_DELETE` (ON), `2` = `BTS_OVERWRITE` (FAST).
    secure_delete: u8,
    /// `PRAGMA cell_size_check` state (mirrors the hook installed in the pager).
    cell_size_check: bool,
    /// Schema-cookie base of the DDL statement being executed (see [`CookieStmt`]).
    cookie_stmt: CookieStmt,
    /// Scratch memory of the balancer (`balance.rs`), kept between calls: page images,
    /// `sqlite3PagerTempSpace`, `aSpace1`, `CellArray.apCell[]` / `szCell[]`.
    bal_pool: Vec<Vec<u8>>,
    bal_tmp: Vec<u8>,
    bal_space1: Vec<u8>,
    bal_ap: Vec<u32>,
    bal_sz: Vec<u16>,
    /// Decision trace only: an insert (not a delete) is running.
    tr_ins: bool,
    /// `(table root, rowid, serial)` of rows a table insert replaced where they were, without
    /// `balance()`, while no later write touched that tree: C's write cursor is then still
    /// valid on the row with `BTCF_ValidNKey` set ([`Self::in_place_row`]). At most one entry
    /// per tree; any modification of the tree drops its entry (`saveAllCursors`).
    in_place_rows: Vec<(Pgno, i64, u64)>,
    /// Source of the serial numbers in `in_place_rows`.
    in_place_serial: u64,
    /// The insert being run stores a cell prepared by `sqlite3BtreeTransferRow`
    /// (`BTREE_PREFORMAT`): its overflow pages are allocated with `nearby = 0`.
    preformat: bool,
    /// `BtShared.pHasContent`: the pages moved to the free list as leaves by the current
    /// transaction (bit set, sized to the page count when the first one was freed). Such a
    /// page keeps its content when it is allocated again before the transaction ends.
    has_content: Option<(Pgno, Vec<u64>)>,
    /// The page stacks of the C cursors of the running statement (see `pins.rs`).
    pub(crate) vcursors: Vec<pins::VCursor>,
    /// Start index in `vcursors` of each nested statement frame.
    pub(crate) vc_frames: Vec<usize>,
    /// `Schema.cache_size != 0`: the cache size of this database was set — by the first
    /// schema load (`sqlite3InitOne`) or by `PRAGMA cache_size`.
    schema_cache_size_set: bool,
    /// `(table root, rowid)` that `OP_NewRowid` just produced on the statement's write cursor:
    /// the `OP_Insert` of exactly that row uses the cursor where `sqlite3BtreeLast` left it
    /// (`OPFLAG_USESEEKRESULT`) instead of seeking.
    pub(crate) new_rowid_hint: Option<(Pgno, i64)>,
    /// `OPFLAG_SAVEPOSITION` of the `OP_Insert` / `OP_Delete` the statement runs on its table
    /// write cursor (a one-pass UPDATE / DELETE over several rows).
    pub(crate) c_save_position: Option<Pgno>,
    /// `sqlite3BtreeIndexMoveto` took its first shortcut (the cursor is on the last cell of
    /// the index and the key is not smaller): the search of that leaf that follows here only
    /// places the cursor, C compared no other cell.
    pub(crate) c_idx_search_quiet: bool,
    /// Index scans run while the read functions were not moving C's cursors.
    pub(crate) idx_scans: Vec<pins::IdxScan>,
    pub(crate) c_defer_idx: bool,
    /// Root of the index whose write cursor `OP_NoConflict` just placed for the key that is
    /// about to be inserted (`OPFLAG_USESEEKRESULT`).
    pub(crate) idx_seek_hint: Vec<Pgno>,
    /// Roots of PRIMARY KEY b-trees of WITHOUT ROWID tables the statements touched.
    pub(crate) wr_roots: Vec<Pgno>,
    /// Index roots in `pTab->pIndex` order, for the tables the statement writes.
    pub(crate) idx_order: Vec<Pgno>,
    /// Per nested program frame: what `vc_frame_pop` puts back.
    pub(crate) frame_saved: Vec<(u32, Option<Pgno>, usize)>,
    /// The engine is reading rows ahead of C's program (see `pins::VC_SILENT`).
    pub(crate) c_silent: u32,
    /// Roots of the b-trees whose scans are delivering rows right now (innermost last).
    pub(crate) read_scans: Vec<Pgno>,
    /// The cursor (index into `vcursors`) the write that is starting goes through: it keeps
    /// its pages when the other cursors on the tree are saved (`usize::MAX`: none).
    pub(crate) vc_keep: usize,
}

/// How the next schema-cookie change of this database is computed.
///
/// C codes `OP_SetCookie iDb, BTREE_SCHEMA_VERSION, cookie_at_prepare + 1` for every
/// `sqlite3ChangeCookie` call (`build.c`), so however many catalogue rows one DDL statement
/// writes, the cookie ends at "value when the statement started" + 1, and a statement nested
/// inside it (`sqlite3_exec` from `xCreate`/`xDestroy`) starts from the value in force when it is
/// itself prepared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CookieStmt {
    /// No DDL statement is running: every change adds one.
    #[default]
    Off,
    /// A DDL statement is running and has not looked at the cookie yet.
    Pending,
    /// A DDL statement is running; the cookie was this value when it started.
    Base(u32),
}

impl Btree {
    pub fn new(pager: Pager) -> Self {
        Self {
            pager,
            vcursors: Vec::new(),
            vc_frames: Vec::new(),
            schema_cache_size_set: false,
            new_rowid_hint: None,
            c_save_position: None,
            c_idx_search_quiet: false,
            idx_scans: Vec::new(),
            c_defer_idx: false,
            idx_seek_hint: Vec::new(),
            wr_roots: Vec::new(),
            idx_order: Vec::new(),
            frame_saved: Vec::new(),
            c_silent: 0,
            read_scans: Vec::new(),
            vc_keep: usize::MAX,
            cursor_slots: Vec::new(),
            free_cursor_slots: Vec::new(),
            open_cursor_count: 0,
            next_cursor_gen: 1,
            interrupt_flag: None,
            secure_delete: 0,
            cell_size_check: false,
            cookie_stmt: CookieStmt::Off,
            bal_pool: Vec::new(),
            bal_tmp: Vec::new(),
            bal_space1: Vec::new(),
            bal_ap: Vec::new(),
            bal_sz: Vec::new(),
            tr_ins: false,
            in_place_rows: Vec::new(),
            in_place_serial: 0,
            preformat: false,
            has_content: None,
        }
    }

    /// `btreeSetHasContent`.
    fn has_content_set(&mut self, pgno: Pgno) {
        let n_page = self.pager.page_count();
        let (size, bits) = self
            .has_content
            .get_or_insert_with(|| (n_page, vec![0u64; n_page as usize / 64 + 1]));
        if pgno <= *size {
            bits[pgno as usize / 64] |= 1u64 << (pgno % 64);
        }
    }

    /// `btreeGetHasContent`.
    fn has_content_get(&self, pgno: Pgno) -> bool {
        match &self.has_content {
            Some((size, bits)) => {
                pgno > *size || bits[pgno as usize / 64] & (1u64 << (pgno % 64)) != 0
            }
            None => false,
        }
    }

    /// Decision trace only: whether this is a database file (C: `!pPager->tempFile`; temporary
    /// and in-memory databases are not traced).
    #[inline]
    fn dtrace_persistent(&self) -> bool {
        self.pager.db_kind() == sqliters_types::DatabaseKind::NamedDisk
    }

    /// Decision trace only (public form of the test above, for the trace points of the SQL
    /// layer that have a C counterpart guarded by `!pPager->tempFile`).
    #[inline]
    #[must_use]
    pub fn dtrace_traced(&self) -> bool {
        self.dtrace_persistent()
    }

    /// Installs a new statement cookie state and returns the previous one (to be restored when
    /// the statement ends; statements nest).
    pub const fn replace_cookie_stmt(&mut self, state: CookieStmt) -> CookieStmt {
        std::mem::replace(&mut self.cookie_stmt, state)
    }

    /// Fixes the cookie base of the running DDL statement to the cookie currently in the header
    /// (the value C's parser would have seen), unless it is already fixed. Call inside the
    /// statement's transaction, only while a DDL statement scope is active (a database opened
    /// inside the statement is still `Off` here).
    pub fn capture_cookie_stmt_base(&mut self) {
        if matches!(self.cookie_stmt, CookieStmt::Base(_)) {
            return;
        }
        self.cookie_stmt = CookieStmt::Base(self.pager.header().schema_cookie);
    }

    /// `sqlite3ChangeCookie` + `OP_SetCookie`: sets the schema cookie to "statement base + 1"
    /// (idempotent within one statement), or adds one when no statement scope is active.
    pub fn change_schema_cookie(&mut self) -> Result<()> {
        let stmt = self.cookie_stmt;
        let hdr = self.pager.header_mut()?;
        let old = hdr.schema_cookie;
        let base = match stmt {
            CookieStmt::Base(base) => base,
            CookieStmt::Off | CookieStmt::Pending => old,
        };
        hdr.schema_cookie = base.wrapping_add(1);
        let _new = hdr.schema_cookie;
        sqliters_types::dtrace!(
            "TR|cookie|set|kind={:?} old={} new={}",
            self.pager.db_kind(),
            old,
            _new
        );
        if stmt == CookieStmt::Pending {
            self.cookie_stmt = CookieStmt::Base(base);
        }
        Ok(())
    }

    /// Queries (`None`) or changes (`Some(0|1|2)`) the secure-delete mode and returns the value
    /// now in force (`sqlite3BtreeSecureDelete`, `btree.c:3211`):
    ///
    /// * `0` — deleted content is left in place;
    /// * `1` (`ON`, `BTS_SECURE_DELETE`) — deleted content is overwritten with zeros, including
    ///   whole pages moved to the freelist;
    /// * `2` (`FAST`, `BTS_OVERWRITE`) — deleted content is overwritten with zeros where that
    ///   costs no extra I/O (cells freed inside a page that is being written anyway).
    ///
    /// This engine always zero-fills, journals and writes a page that goes to the freelist
    /// (inventory class U4), so `FAST` is at least as strong here as in C; the mode only decides
    /// whether cells released inside a live page are overwritten (`freeSpace`, `btree.c:2019`).
    pub fn secure_delete(&mut self, new_flag: Option<u8>) -> u8 {
        if let Some(flag) = new_flag {
            self.secure_delete = flag.min(2);
        }
        self.secure_delete
    }

    /// `BTS_FAST_SECURE` test: `true` when cells freed inside a page must be zeroed.
    #[inline]
    fn zero_freed_cells(&self) -> bool {
        self.secure_delete != 0
    }

    /// `PRAGMA cell_size_check` (`SQLITE_CellSizeCk`): when on, every b-tree page loaded from
    /// disk is validated with [`page::cell_size_check_page`] (`btreeCellSizeCheck`) the first
    /// time it is used, and a page with a cell that does not fit fails with `SQLITE_CORRUPT`.
    pub fn set_cell_size_check(&mut self, enabled: bool) {
        self.pager
            .set_page_load_check(enabled.then_some(page::cell_size_check_page as PageLoadCheck));
        self.cell_size_check = enabled;
    }

    /// Returns `true` when `PRAGMA cell_size_check` is on for this database.
    #[inline]
    pub fn cell_size_check(&self) -> bool {
        self.cell_size_check
    }

    /// `sqlite3BtreeGetMeta(BTREE_DATA_VERSION)`: the value of `PRAGMA data_version`. It
    /// changes when the database content was changed by another connection (or the cache page
    /// size changed) and never because of a commit of this connection.
    #[inline]
    pub fn data_version(&self) -> u32 {
        self.pager.data_version()
    }

    /// Attaches or clears a shared atomic interrupt flag (`sqlite3_interrupt`).
    pub fn set_interrupt_flag(&mut self, flag: Option<Arc<AtomicBool>>) {
        self.interrupt_flag = flag;
    }

    /// Returns the attached atomic interrupt flag, if any.
    #[inline]
    pub fn interrupt_flag(&self) -> Option<&Arc<AtomicBool>> {
        self.interrupt_flag.as_ref()
    }

    /// Checks whether execution has been interrupted (`Ordering::Relaxed`).
    #[inline]
    pub fn check_interrupted(&self) -> Result<()> {
        if let Some(flag) = &self.interrupt_flag
            && flag.load(AtomicOrdering::Relaxed)
        {
            return Err(SqliteError::new(ErrorCode::Interrupt, "interrupted"));
        }
        Ok(())
    }

    pub fn open_file(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        Ok(Self::new(Pager::open_file(path, flags)?))
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(Pager::open_in_memory()?))
    }

    pub fn open_with_vfs(vfs: Arc<dyn Vfs>, path: Option<&Path>, flags: OpenFlags) -> Result<Self> {
        Ok(Self::new(Pager::open_with_vfs(vfs, path, flags)?))
    }

    pub fn open_with_options(
        vfs: Arc<dyn Vfs>,
        path: Option<&Path>,
        flags: OpenFlags,
        options: PagerOpenOptions,
    ) -> Result<Self> {
        Ok(Self::new(Pager::open_with_options(
            vfs, path, flags, options,
        )?))
    }

    pub fn open_private_temporary(vfs: Arc<dyn Vfs>, options: PagerOpenOptions) -> Result<Self> {
        Ok(Self::new(Pager::open_private_temporary(vfs, options)?))
    }

    #[inline]
    pub const fn lock_byte_page(&self) -> Pgno {
        self.pager.lock_byte_page()
    }

    pub fn close(&mut self) -> Result<()> {
        self.close_all_cursors();
        self.pager.close()
    }

    #[inline]
    pub const fn pager(&self) -> &Pager {
        &self.pager
    }

    #[inline]
    pub fn pager_mut(&mut self) -> &mut Pager {
        &mut self.pager
    }

    #[inline]
    pub fn begin_read(&mut self) -> Result<()> {
        self.pager.begin_read()
    }

    #[inline]
    pub fn begin_write(&mut self) -> Result<()> {
        if self.pager.transaction_state() != TransactionState::Write {
            // (a transaction that ended without passing through this object's commit /
            // rollback leaves no stale `pHasContent` behind)
            self.has_content = None;
        }
        self.pager.begin_write()
    }

    #[inline]
    pub fn begin_exclusive(&mut self) -> Result<()> {
        if self.pager.transaction_state() != TransactionState::Write {
            self.has_content = None;
        }
        self.pager.begin_exclusive()
    }

    /// Returns `true` when pointer-map auto-vacuum (`Full` or `Incremental`) is active on Page 1.
    #[inline]
    pub fn is_auto_vacuum(&self) -> bool {
        self.pager.header().largest_root_btree_page > 0
    }

    /// Returns the active or configured [`AutoVacuumMode`] of this database.
    #[inline]
    pub fn auto_vacuum_mode(&self) -> AutoVacuumMode {
        self.pager.auto_vacuum_mode()
    }

    /// Updates configured [`AutoVacuumMode`] following `sqlite3BtreeSetAutoVacuum` (`btree.c:3232`).
    ///
    /// Returns `false` (`SQLITE_READONLY`) if Page 1 is already fixed and `mode != None` differs
    /// from `current != None`; returns `true` otherwise.
    #[inline]
    pub fn set_configured_auto_vacuum(&mut self, mode: AutoVacuumMode) -> bool {
        self.pager.set_configured_auto_vacuum(mode)
    }

    /// Enables or disables `PRAGMA writable_schema` truncated-image tolerance on the underlying [`Pager`].
    #[inline]
    pub fn set_writable_schema(&mut self, enabled: bool) {
        self.pager.set_writable_schema(enabled);
    }

    /// Returns `true` when `PRAGMA writable_schema` is enabled on the underlying [`Pager`].
    #[inline]
    pub fn writable_schema(&self) -> bool {
        self.pager.writable_schema()
    }

    /// Compatibility no-op retained for pointer-map unit tests (auto-vacuum writes are always enabled).
    #[inline]
    pub fn enable_autovacuum_storage(&mut self) {
        self.pager.set_autovacuum_ptrmap_ready(true);
    }

    /// Acquires `LockLevel::Exclusive` on the underlying database file when in an active
    /// rollback-mode write transaction (`sqlite3PagerExclusiveLock`).
    #[inline]
    pub fn acquire_exclusive_lock(&mut self) -> Result<()> {
        self.pager.acquire_exclusive_lock()
    }

    /// Returns the database filename (`None` for `:memory:` and `PrivateTemporary`; `Some` for `NamedDisk`).
    #[inline]
    pub fn db_path(&self) -> Option<&Path> {
        self.pager.db_path()
    }

    /// Returns the rollback journal filename (`None` for `:memory:` and `PrivateTemporary`; `Some` for `NamedDisk`).
    #[inline]
    pub fn journal_path(&self) -> Option<&Path> {
        self.pager.journal_path()
    }

    /// Returns `true` when backed by heap memory, a temporary file, or immutable mode (`sqlite3PagerIsMemdb`).
    #[inline]
    pub const fn is_pager_memdb(&self) -> bool {
        self.pager.is_pager_memdb()
    }

    /// Returns `true` when this B-tree is an active durable rollback-journal write participant
    /// that requires a super-journal in multi-file commits (`vdbeCommit` in `vdbeaux.c:2969-2987`).
    #[inline]
    pub const fn needs_super_journal_in_write_tx(&self) -> bool {
        self.pager.needs_super_journal_in_write_tx()
    }

    fn commit_phase_one_internal(
        &mut self,
        super_journal: Option<&str>,
        retain_read_on_av_err: bool,
    ) -> Result<()> {
        // (`sqlite3VdbeHalt`: the cursors of the statement are closed before it commits.)
        self.vc_close_all();
        if self.pager.transaction_state() == TransactionState::Write
            && !self.pager.is_commit_phase_one_done()
            && !self.pager.has_staged_raw_copy()
            && self.is_auto_vacuum()
            && let Err(e) = self.auto_vacuum_commit()
        {
            let _ = self.rollback_with_trip(
                extended::SQLITE_ABORT_ROLLBACK,
                false,
                retain_read_on_av_err,
            );
            return Err(e);
        }
        self.pager.commit_phase_one(super_journal)
    }

    /// Executes Phase 1 of a two-phase commit (`sqlite3BtreeCommitPhaseOne` in `btree.c:4343-4364`),
    /// running [`Self::auto_vacuum_commit`] first when `auto_vacuum` is active and rolling back with
    /// cursor trip if `auto_vacuum_commit` fails.
    pub fn commit_phase_one(&mut self, super_journal: Option<&str>) -> Result<()> {
        self.commit_phase_one_internal(super_journal, false)
    }

    /// Executes Phase 2 of a two-phase commit (`sqlite3BtreeCommitPhaseTwo` in `btree.c:4432-4459`).
    #[inline]
    pub fn commit_phase_two(&mut self, retain_read: bool, cleanup_benign: bool) -> Result<()> {
        // `btreeClearHasContent`
        self.has_content = None;
        self.pager.commit_phase_two(retain_read, cleanup_benign)
    }

    /// Commits the active write transaction, running [`Self::commit_phase_one`] followed by
    /// [`Self::commit_phase_two`].
    pub fn commit_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        self.commit_phase_one_internal(None, retain_read)?;
        self.commit_phase_two(retain_read, false)
    }

    #[inline]
    pub fn commit(&mut self) -> Result<()> {
        self.commit_phase_one(None)?;
        self.commit_phase_two(false, false)
    }

    /// Rolls back the write transaction after tripping open cursors (`sqlite3BtreeRollback`).
    pub fn rollback_with_trip(
        &mut self,
        trip_code: i32,
        write_only: bool,
        retain_read: bool,
    ) -> Result<()> {
        let trip_res = self.trip_cursors(trip_code, write_only);
        self.vc_close_all();
        // `btreeClearHasContent`
        self.has_content = None;
        let rb_res = self.pager.rollback_retaining_read(retain_read);
        rb_res?;
        trip_res
    }

    #[inline]
    pub fn rollback(&mut self) -> Result<()> {
        self.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, false)
    }

    #[inline]
    pub const fn transaction_state(&self) -> TransactionState {
        self.pager.transaction_state()
    }

    #[inline]
    pub const fn encoding(&self) -> TextEncoding {
        self.pager.encoding()
    }

    #[inline]
    pub const fn journal_mode(&self) -> JournalMode {
        self.pager.journal_mode()
    }

    /// Switches journal mode (including rollback-journaled Page 1 `(1, 1) <-> (2, 2)` WAL transitions).
    pub fn set_journal_mode(&mut self, mode: JournalMode) -> Result<JournalMode> {
        self.pager.transition_journal_mode(mode)
    }

    #[inline]
    pub const fn locking_mode(&self) -> LockingMode {
        self.pager.locking_mode()
    }

    #[inline]
    pub fn set_locking_mode(&mut self, mode: LockingMode) -> LockingMode {
        self.pager.set_locking_mode(mode)
    }

    #[inline]
    pub const fn synchronous(&self) -> u8 {
        self.pager.synchronous()
    }

    #[inline]
    pub fn set_synchronous(&mut self, level: u8) {
        self.pager.set_synchronous(level);
    }

    /// Consumes and returns the post-commit WAL frame count (`sqlite3PagerWalCallback`) for
    /// `wal_autocheckpoint`.
    #[inline]
    pub fn take_wal_callback_frame_count(&mut self) -> u32 {
        self.pager.take_wal_callback_frame_count()
    }

    /// Runs a WAL checkpoint (`sqlite3BtreeCheckpoint`, `btree.c:11403`), propagating this
    /// B-tree's attached atomic interrupt flag when present.
    pub fn wal_checkpoint(
        &mut self,
        mode: WalCheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
    ) -> Result<WalCheckpointResult> {
        let interrupt_ref = self.interrupt_flag.as_deref();
        self.pager.wal_checkpoint(mode, busy_handler, interrupt_ref)
    }

    /// Runs a WAL checkpoint with a deterministic post-header hook (for race testing).
    pub fn wal_checkpoint_with_hook(
        &mut self,
        mode: WalCheckpointMode,
        busy_handler: Option<&mut dyn FnMut(u32) -> bool>,
        after_read_hdr_hook: Option<&mut dyn FnMut()>,
    ) -> Result<WalCheckpointResult> {
        let interrupt_ref = self.interrupt_flag.as_deref();
        self.pager
            .wal_checkpoint_with_hook(mode, busy_handler, interrupt_ref, after_read_hdr_hook)
    }

    /// Spills currently dirty cache pages to `<db>-wal` as uncommitted frames (`is_commit = false`).
    pub fn spill_dirty_pages_to_wal(&mut self) -> Result<usize> {
        // (a statement boundary: no cursor holds a page; `sqlite3PagerFlush` writes the dirty
        // pages nobody refers to)
        self.vc_close_all();
        self.pager.spill_dirty_pages_to_wal()
    }

    /// Spills dirty pages to `<db>-wal` when in a WAL write transaction and dirty pages exceed `cache_size`.
    pub fn spill_if_over_cache_size(&mut self) -> Result<usize> {
        self.statement_end();
        Ok(0)
    }

    /// A statement that ran inside a transaction has ended (`sqlite3VdbeHalt` →
    /// `closeAllCursors`): every cursor gives its page references back.
    #[inline]
    pub fn statement_end(&mut self) {
        self.vc_close_all();
    }

    /// Returns the configured page cache size (`PRAGMA cache_size`).
    #[inline]
    pub fn cache_size(&self) -> i32 {
        self.pager.cache_size()
    }

    /// Updates the configured page cache size (`PRAGMA cache_size = N`:
    /// `pDb->pSchema->cache_size = size; sqlite3BtreeSetCacheSize(pDb->pBt, size)`).
    #[inline]
    pub fn set_cache_size(&mut self, size: i32) {
        self.schema_cache_size_set = true;
        self.pager.set_cache_size(size);
    }

    /// `sqlite3InitOne`: "if( pDb->pSchema->cache_size==0 ){ size =
    /// sqlite3AbsInt32(meta[BTREE_DEFAULT_CACHE_SIZE-1]); if( size==0 ) size =
    /// SQLITE_DEFAULT_CACHE_SIZE; pDb->pSchema->cache_size = size;
    /// sqlite3BtreeSetCacheSize(pDb->pBt, size); }" — the first time the schema of this
    /// database is read (inside a read transaction: the header is loaded).
    pub fn init_schema_cache_size(&mut self) {
        if self.schema_cache_size_set {
            return;
        }
        self.schema_cache_size_set = true;
        let meta = self.pager.header().default_cache_size;
        let size = if meta == 0 {
            -2000
        } else {
            meta.checked_abs().unwrap_or(i32::MAX)
        };
        self.pager.set_cache_size(size);
    }

    /// `sqlite3BtreeSetSpillSize` (`PRAGMA cache_spill`): sets the spill size unless `size`
    /// is 0 and returns the effective value.
    #[inline]
    pub fn set_spill_size(&mut self, size: i32) -> i32 {
        self.pager.set_spill_size(size)
    }

    /// `sqlite3BtreeSetPagerFlags(.., PAGER_CACHESPILL)`.
    #[inline]
    pub fn set_cache_spill(&mut self, on: bool) {
        self.pager.set_cache_spill(on);
    }

    /// `sqlite3PagerShrink` (`PRAGMA shrink_memory`).
    #[inline]
    pub fn shrink_memory(&mut self) {
        self.pager.shrink_memory();
    }

    /// Allocates a page either by popping from the freelist or extending the database file.
    #[inline]
    pub fn allocate_page(&mut self) -> Result<Pgno> {
        let pg = self.allocate_btree_page(AllocMode::Any)?;
        self.unpin(pg);
        Ok(pg)
    }

    /// Allocates a page, preferring — when the free list is not empty — the free page closest
    /// to `nearby` on the first trunk (C's `nearby` argument: the left sibling in a balance,
    /// the previous page of an overflow chain, the root when a root is split, 1 for a new root).
    #[inline]
    pub fn allocate_page_near(&mut self, nearby: Pgno) -> Result<Pgno> {
        let pg = self.allocate_btree_page(AllocMode::Near(nearby))?;
        self.unpin(pg);
        Ok(pg)
    }

    /// Allocates a B-tree/overflow page according to `mode` (`allocateBtreePage`,
    /// `btree.c:6545-6835`), automatically allocating any intervening pointer-map page at EOF.
    ///
    /// As in C the page comes back with a reference held (`*ppPage`): the caller gives it
    /// back with `unpin` (`releasePage`) when it is done with the page.
    pub fn allocate_btree_page(&mut self, mode: AllocMode) -> Result<Pgno> {
        let mx_page = self.pager.page_count();
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let auto_vac = self.is_auto_vacuum();
        let total_free = self.pager.header().total_freelist_pages;
        let first_trunk = self.pager.header().first_freelist_trunk_page;

        if mx_page > 0 && total_free >= mx_page {
            return Err(SqliteError::corrupt(
                "freelist page count >= database page count",
            ));
        }
        if (total_free == 0) != (first_trunk == 0) {
            return Err(SqliteError::corrupt(
                "inconsistent freelist trunk and count in database header",
            ));
        }

        if total_free > 0 {
            // `sqlite3PagerWrite(pPage1->pDbPage)` comes first ("Decrement the free-list
            // count by 1").
            self.pager.header_mut()?;
            let search_list = match mode {
                AllocMode::Any | AllocMode::Near(_) => false,
                AllocMode::Le(_) => true,
                AllocMode::Exact(nearby) => {
                    if nearby <= mx_page {
                        if !auto_vac {
                            return Err(SqliteError::misuse(
                                "AllocMode::Exact requires auto_vacuum",
                            ));
                        }
                        let (e_type, _) = self.ptrmap_get(nearby)?;
                        e_type == PtrmapType::FreePage
                    } else {
                        false
                    }
                }
            };

            let max_possible_k = (usable / 4).saturating_sub(2);
            let validate_free_pg = |pg: Pgno| -> Result<()> {
                if pg < 2
                    || pg > mx_page
                    || pg == lock_pg
                    || (auto_vac && is_ptrmap_page(pg, usable, lock_pg))
                {
                    return Err(SqliteError::corrupt(format!(
                        "invalid page {pg} on freelist"
                    )));
                }
                Ok(())
            };

            let mut prev_trunk: Pgno = 0;
            let mut cur_trunk: Pgno = first_trunk;
            let mut n_search: u32 = 0;
            // References as C holds them: `pTrunk` (taken by `btreeGetUnusedPage` at the top
            // of each round) and `pPrevTrunk` (the trunk of the round before, given back at
            // the end of the round). The page handed to the caller keeps its reference.
            let mut pin_trunk: Pgno = 0;
            let mut pin_prev: Pgno = 0;

            let res: Result<Pgno> = 'alloc: loop {
                if n_search > total_free {
                    break 'alloc Err(SqliteError::corrupt("cycle detected in freelist trunks"));
                }
                if let Err(e) = validate_free_pg(cur_trunk) {
                    break 'alloc Err(e);
                }
                n_search += 1;

                pin_prev = pin_trunk;
                if let Err(e) = self.pin(cur_trunk) {
                    pin_trunk = 0;
                    break 'alloc Err(e);
                }
                pin_trunk = cur_trunk;
                let trunk_bytes = match self.pager.read_page_unchecked(cur_trunk) {
                    Ok(d) => d.to_vec(),
                    Err(e) => break 'alloc Err(e),
                };
                let next_trunk = u32::from_be_bytes([
                    trunk_bytes[0],
                    trunk_bytes[1],
                    trunk_bytes[2],
                    trunk_bytes[3],
                ]);
                let k = u32::from_be_bytes([
                    trunk_bytes[4],
                    trunk_bytes[5],
                    trunk_bytes[6],
                    trunk_bytes[7],
                ]);
                if k > max_possible_k {
                    return Err(SqliteError::corrupt(format!(
                        "freelist trunk {cur_trunk} leaf count {k} out of bounds"
                    )));
                }

                if k == 0 && !search_list {
                    debug_assert_eq!(prev_trunk, 0);
                    {
                        let hdr = self.pager.header_mut()?;
                        hdr.first_freelist_trunk_page = next_trunk;
                        hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                    }
                    // The trunk itself becomes the allocated page; C only makes it writable
                    // (`sqlite3PagerWrite(pTrunk->pDbPage)`), its bytes stay.
                    self.pager.write_page(cur_trunk)?;
                    if self.dtrace_persistent() {
                        sqliters_types::dtrace!("TR|alloc|page|pg={} o=empty-trunk", cur_trunk);
                    }
                    pin_trunk = 0;
                    break 'alloc Ok(cur_trunk);
                }

                let trunk_matches = search_list
                    && match mode {
                        AllocMode::Exact(nb) => cur_trunk == nb,
                        AllocMode::Le(nb) => cur_trunk <= nb,
                        AllocMode::Any | AllocMode::Near(_) => false,
                    };

                if trunk_matches {
                    if k == 0 {
                        if prev_trunk == 0 {
                            let hdr = self.pager.header_mut()?;
                            hdr.first_freelist_trunk_page = next_trunk;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        } else {
                            self.pager.write_page(prev_trunk)?[0..4]
                                .copy_from_slice(&next_trunk.to_be_bytes());
                            let hdr = self.pager.header_mut()?;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        }
                    } else {
                        let new_trunk = u32::from_be_bytes([
                            trunk_bytes[8],
                            trunk_bytes[9],
                            trunk_bytes[10],
                            trunk_bytes[11],
                        ]);
                        validate_free_pg(new_trunk)?;
                        let rem_leaves = k - 1;
                        let rem_bytes_len = (rem_leaves as usize) * 4;
                        {
                            // (C copies the trunk header and leaf list over the first leaf;
                            // the rest of that page is not cleared.)
                            // btreeGetUnusedPage(pBt, iNewTrunk, &pNewTrunk, 0) ..
                            // releasePage(pNewTrunk)
                            self.pin(new_trunk)?;
                            let nt_buf = self.pager.write_page(new_trunk)?;
                            nt_buf[0..4].copy_from_slice(&next_trunk.to_be_bytes());
                            nt_buf[4..8].copy_from_slice(&rem_leaves.to_be_bytes());
                            if rem_bytes_len > 0 {
                                nt_buf[8..8 + rem_bytes_len]
                                    .copy_from_slice(&trunk_bytes[12..12 + rem_bytes_len]);
                            }
                        }
                        self.unpin(new_trunk);
                        if prev_trunk == 0 {
                            let hdr = self.pager.header_mut()?;
                            hdr.first_freelist_trunk_page = new_trunk;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        } else {
                            self.pager.write_page(prev_trunk)?[0..4]
                                .copy_from_slice(&new_trunk.to_be_bytes());
                            let hdr = self.pager.header_mut()?;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        }
                    }
                    self.pager.write_page(cur_trunk)?;
                    if self.dtrace_persistent() {
                        sqliters_types::dtrace!(
                            "TR|alloc|page|pg={} o=searched-trunk k={}",
                            cur_trunk,
                            k
                        );
                    }
                    pin_trunk = 0;
                    break 'alloc Ok(cur_trunk);
                }

                if k > 0 {
                    let chosen_idx: Option<usize> = if !search_list {
                        // "Extract a leaf from the trunk": the leaf closest to `nearby`
                        // (the first of equally close ones), or the first leaf without a hint.
                        match mode {
                            AllocMode::Exact(nearby) | AllocMode::Near(nearby) if nearby > 0 => {
                                let mut best_i = 0usize;
                                let mut best_dist = u32::MAX;
                                for i in 0..k as usize {
                                    let off = 8 + 4 * i;
                                    let lp = u32::from_be_bytes([
                                        trunk_bytes[off],
                                        trunk_bytes[off + 1],
                                        trunk_bytes[off + 2],
                                        trunk_bytes[off + 3],
                                    ]);
                                    let dist = lp.abs_diff(nearby);
                                    if dist < best_dist {
                                        best_dist = dist;
                                        best_i = i;
                                    }
                                }
                                Some(best_i)
                            }
                            _ => Some(0),
                        }
                    } else {
                        let mut hit = None;
                        for i in 0..k as usize {
                            let off = 8 + 4 * i;
                            let lp = u32::from_be_bytes([
                                trunk_bytes[off],
                                trunk_bytes[off + 1],
                                trunk_bytes[off + 2],
                                trunk_bytes[off + 3],
                            ]);
                            validate_free_pg(lp)?;
                            let matches_leaf = match mode {
                                AllocMode::Exact(nb) => lp == nb,
                                AllocMode::Le(nb) => lp <= nb,
                                AllocMode::Any | AllocMode::Near(_) => true,
                            };
                            if matches_leaf {
                                hit = Some(i);
                                break;
                            }
                        }
                        hit
                    };

                    if let Some(ci) = chosen_idx {
                        let off = 8 + 4 * ci;
                        let leaf_pgno = u32::from_be_bytes([
                            trunk_bytes[off],
                            trunk_bytes[off + 1],
                            trunk_bytes[off + 2],
                            trunk_bytes[off + 3],
                        ]);
                        validate_free_pg(leaf_pgno)?;
                        let last_idx = (k - 1) as usize;
                        let last_off = 8 + 4 * last_idx;
                        let new_k = k - 1;
                        {
                            let w = self.pager.write_page(cur_trunk)?;
                            if ci < last_idx {
                                let last_bytes = [
                                    trunk_bytes[last_off],
                                    trunk_bytes[last_off + 1],
                                    trunk_bytes[last_off + 2],
                                    trunk_bytes[last_off + 3],
                                ];
                                w[off..off + 4].copy_from_slice(&last_bytes);
                            }
                            // (C leaves the vacated last slot as it is.)
                            w[4..8].copy_from_slice(&new_k.to_be_bytes());
                        }
                        {
                            let hdr = self.pager.header_mut()?;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        }
                        // `btreeGetUnusedPage(.., noContent)`: a leaf that was put on the free
                        // list by this transaction keeps the content it had
                        // (`btreeGetHasContent`); any other leaf is handed out zeroed
                        // (`PAGER_GET_NOCONTENT`).
                        // (The reference stays with the caller.)
                        if self.has_content_get(leaf_pgno) {
                            self.pin(leaf_pgno)?;
                        } else {
                            self.pager.page_ref_nocontent(leaf_pgno)?;
                        }
                        self.pager.write_page(leaf_pgno)?;
                        if self.dtrace_persistent() {
                            sqliters_types::dtrace!(
                                "TR|alloc|page|pg={} o=leaf idx={} k={} trunk={} near={}",
                                leaf_pgno,
                                ci,
                                k,
                                cur_trunk,
                                match mode {
                                    AllocMode::Any => 0,
                                    AllocMode::Near(n) | AllocMode::Exact(n) | AllocMode::Le(n) =>
                                        n,
                                }
                            );
                        }
                        break 'alloc Ok(leaf_pgno);
                    }
                }

                if !search_list || next_trunk == 0 {
                    break 'alloc Err(SqliteError::corrupt(
                        "target free page not found on freelist",
                    ));
                }
                // releasePage(pPrevTrunk)
                if pin_prev != 0 {
                    self.unpin(pin_prev);
                    pin_prev = 0;
                }
                prev_trunk = cur_trunk;
                cur_trunk = next_trunk;
            };
            // end_allocate_page: releasePage(pTrunk); releasePage(pPrevTrunk);
            if pin_trunk != 0 {
                self.unpin(pin_trunk);
            }
            if pin_prev != 0 {
                self.unpin(pin_prev);
            }
            return res;
        }

        loop {
            let allocated = self.pager.allocate_raw_page()?;
            if auto_vac && is_ptrmap_page(allocated, usable, lock_pg) {
                // "allocate two new pages at the end of the file instead of one. The first
                // allocated page becomes a new pointer-map page": releasePage(pPg).
                self.unpin(allocated);
                continue;
            }
            if self.dtrace_persistent() {
                sqliters_types::dtrace!("TR|alloc|page|pg={} o=extend", allocated);
            }
            return Ok(allocated);
        }
    }

    /// Pushes `pgno` onto the database freelist (`freePage2(pBt, 0, iPage)`): the caller holds
    /// no reference on the page. C then looks the page up in the cache
    /// (`btreePageLookup`) and reads it only when it has to be written (secure delete, or
    /// the page becomes a trunk).
    pub fn free_page(&mut self, pgno: Pgno) -> Result<()> {
        self.free_page2(pgno, false)
    }

    /// `freePage(pPage)`: the caller holds a reference on the page being freed.
    pub(crate) fn free_page_held(&mut self, pgno: Pgno) -> Result<()> {
        self.free_page2(pgno, true)
    }

    fn free_page2(&mut self, pgno: Pgno, held: bool) -> Result<()> {
        let usable_u32 = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let auto_vac = self.is_auto_vacuum();
        if pgno <= 1 || pgno == lock_pg || (auto_vac && is_ptrmap_page(pgno, usable_u32, lock_pg)) {
            return Err(SqliteError::corrupt(format!(
                "cannot push page {pgno} onto freelist"
            )));
        }
        // "if( pMemPage ){ pPage = pMemPage; sqlite3PagerRef(pPage->pDbPage); }else{ pPage =
        // btreePageLookup(pBt, iPage); }"
        let mut have_page = if held {
            self.pin(pgno)?;
            true
        } else {
            self.pager.page_lookup(pgno)
        };
        let mut pin_trunk: Pgno = 0;
        let res = self.free_page2_body(pgno, &mut have_page, &mut pin_trunk);
        // freepage_out: releasePage(pPage); releasePage(pTrunk);
        if have_page {
            self.unpin(pgno);
        }
        if pin_trunk != 0 {
            self.unpin(pin_trunk);
        }
        res
    }

    fn free_page2_body(
        &mut self,
        pgno: Pgno,
        have_page: &mut bool,
        pin_trunk: &mut Pgno,
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let auto_vac = self.is_auto_vacuum();
        // Max leaf entries per trunk page (usableSize/4 - 2 limit; leave 6 spare slots like btree.c)
        let max_leaves = ((usable / 4).saturating_sub(8)).max(1) as u32;
        let first_trunk = self.pager.header().first_freelist_trunk_page;

        // "Increment the free page count on pPage1": `sqlite3PagerWrite(pPage1->pDbPage)`.
        {
            let hdr = self.pager.header_mut()?;
            hdr.total_freelist_pages += 1;
        }
        // `BTS_SECURE_DELETE` (ON, not FAST): "always fully overwrite deleted information
        // with zeros".
        let zero = self.secure_delete == 1;
        if zero {
            if !*have_page {
                self.pin(pgno)?;
                *have_page = true;
            }
            self.pager.write_page(pgno)?.fill(0);
        }
        // "If the database supports auto-vacuum, write an entry in the pointer-map to
        // indicate that the page is free."
        if auto_vac {
            self.ptrmap_put(pgno, PtrmapType::FreePage, 0)?;
        }

        if first_trunk != 0 {
            // btreeGetPage(pBt, iTrunk, &pTrunk, 0)
            self.pin(first_trunk)?;
            *pin_trunk = first_trunk;
            let trunk_ro = self.pager.read_page_unchecked(first_trunk)?;
            let leaf_count =
                u32::from_be_bytes([trunk_ro[4], trunk_ro[5], trunk_ro[6], trunk_ro[7]]);
            if leaf_count < max_leaves {
                let off = 8 + 4 * (leaf_count as usize);
                let new_count = leaf_count + 1;
                {
                    let w = self.pager.write_page(first_trunk)?;
                    w[4..8].copy_from_slice(&new_count.to_be_bytes());
                    w[off..off + 4].copy_from_slice(&pgno.to_be_bytes());
                }
                // `freePage2`: a page that becomes a free-list leaf is neither journalled nor
                // written — only its trunk changes — unless `secure_delete=ON`
                // (`BTS_SECURE_DELETE`; not `FAST`), which overwrites it with zeros. A write the
                // transaction already owed for the page is dropped (`sqlite3PagerDontWrite`) —
                // when the page is in the cache.
                if zero {
                    sqliters_types::dtrace!(
                        "TR|free|page|db={} pg={} o=leaf trunk={} zero=1",
                        u8::from(self.dtrace_persistent()),
                        pgno,
                        first_trunk
                    );
                } else {
                    sqliters_types::dtrace!(
                        "TR|free|page|db={} pg={} o=leaf trunk={} zero=0",
                        u8::from(self.dtrace_persistent()),
                        pgno,
                        first_trunk
                    );
                    if *have_page {
                        let _dropped = self.pager.dont_write(pgno) && !self.pager.use_wal();
                        if self.dtrace_persistent() {
                            sqliters_types::dtrace!(
                                "TR|free|dontwrite|pg={} o={}",
                                pgno,
                                if _dropped { "write-dropped" } else { "no-op" }
                            );
                        }
                    }
                }
                self.has_content_set(pgno);
                return Ok(());
            }
        }

        // Make `pgno` the new freelist trunk page pointing to `first_trunk` with 0 leaves.
        // `freePage2` rewrites only the 8-byte trunk header; the rest of the page keeps its
        // old content unless `secure_delete=ON` zeroed it.
        {
            if !*have_page {
                // btreeGetPage(pBt, iPage, &pPage, 0)
                self.pin(pgno)?;
                *have_page = true;
            }
            let w = self.pager.write_page(pgno)?;
            w[0..4].copy_from_slice(&first_trunk.to_be_bytes());
            w[4..8].copy_from_slice(&0u32.to_be_bytes());
            sqliters_types::dtrace!(
                "TR|free|page|db={} pg={} o=new-trunk next={} zero={}",
                u8::from(self.dtrace_persistent()),
                pgno,
                first_trunk,
                u8::from(zero)
            );
        }
        {
            let hdr = self.pager.header_mut()?;
            hdr.first_freelist_trunk_page = pgno;
        }
        Ok(())
    }

    /// `accessPayload(pCur, 0, nKey, buf, 0)` for a cell whose payload continues on overflow
    /// pages (the key comparison of `sqlite3BtreeIndexMoveto`): each overflow page that holds
    /// part of the `remaining` bytes is fetched through the page cache and released again
    /// (`sqlite3PagerGet(.., PAGER_GET_READONLY)`, `sqlite3PagerUnref`) — unless it can be
    /// read directly from the file (`sqlite3PagerDirectReadOk`).
    pub(crate) fn c_overflow_refs(&mut self, first: Pgno, mut remaining: usize) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let per_page = usable.saturating_sub(4).max(1);
        let n_page = self.pager.page_count();
        let mut next = first;
        let mut hops = 0u32;
        while remaining > 0 && next != 0 && next <= n_page && hops <= n_page {
            hops += 1;
            let direct = self.pager.direct_overflow_read_ok(next);
            if !direct {
                self.pager.page_ref(next)?;
            }
            let nxt = {
                let raw = self.pager.read_page_unchecked(next)?;
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])
            };
            if !direct {
                self.pager.page_unref(next);
            }
            remaining = remaining.saturating_sub(per_page);
            next = nxt;
        }
        Ok(())
    }

    fn read_full_payload(
        &mut self,
        total_len: u32,
        local_payload: &[u8],
        mut next_ovfl: Option<Pgno>,
    ) -> Result<Vec<u8>> {
        let total = total_len as usize;
        if local_payload.len() >= total {
            return Ok(local_payload[..total].to_vec());
        }
        let usable = self.pager.usable_size() as usize;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(local_payload);

        let max_hops = self.pager.page_count() as usize + 1;
        let mut hops = 0usize;
        while out.len() < total {
            self.check_interrupted()?;
            let Some(ovfl_pgno) = next_ovfl.filter(|&p| p != 0) else {
                return Err(SqliteError::corrupt(
                    "overflow chain ended prematurely before payload length",
                ));
            };
            hops += 1;
            if hops > max_hops {
                return Err(SqliteError::corrupt(
                    "cycle detected in overflow page chain",
                ));
            }
            let raw = self.pager.read_page_unchecked(ovfl_pgno)?;
            let nxt = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let remaining = total - out.len();
            let take = remaining.min(usable - 4);
            out.extend_from_slice(&raw[4..4 + take]);
            next_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(out)
    }

    fn free_overflow_chain(&mut self, mut first_ovfl: Option<Pgno>) -> Result<()> {
        let max_hops = self.pager.page_count() as usize + 1;
        let mut hops = 0usize;
        while let Some(pgno) = first_ovfl.filter(|&p| p != 0) {
            hops += 1;
            if hops > max_hops {
                return Err(SqliteError::corrupt(
                    "cycle detected freeing overflow chain",
                ));
            }
            let nxt = {
                let raw = self.pager.read_page_unchecked(pgno)?;
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])
            };
            // `clearCellOverflow`: every page but the last is fetched (`getOverflowPage`, for
            // its next pointer) and freed while referenced; the last one is only looked up.
            if nxt != 0 {
                self.pin(pgno)?;
                let r = self.free_page_held(pgno);
                self.unpin(pgno);
                r?;
            } else {
                self.free_last_overflow_page(pgno)?;
            }
            first_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(())
    }

    fn read_parsed_page(&mut self, pgno: Pgno) -> Result<ParsedPage> {
        let usable = self.pager.usable_size();
        let raw = self.pager.read_page(pgno)?;
        ParsedPage::parse(pgno, usable, raw)
    }

    #[cfg(test)]
    fn write_parsed_page(&mut self, pgno: Pgno, page: &ParsedPage) -> Result<()> {
        let usable = self.pager.usable_size();
        {
            let raw = self.pager.write_page(pgno)?;
            page.write(pgno, usable, raw)?;
        }
        if self.is_auto_vacuum() {
            self.set_child_ptrmaps_for_parsed(pgno, page)?;
        }
        Ok(())
    }

    // ========================================================================
    // Tree Creation, Clearing, and Dropping
    // ========================================================================

    /// `zeroPage(pRoot, flags)` for a new root: only the 8-byte page header is written; the
    /// rest of the page keeps whatever it held (a page taken from the free list is not
    /// cleared) unless a secure-delete mode is on.
    fn zero_root_page(&mut self, pgno: Pgno, flags: u8) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let secure = self.zero_freed_cells();
        let hdr = if pgno == 1 { 100 } else { 0 };
        let d = self.pager.write_page(pgno)?;
        balance::zero_page(d, hdr, flags, usable, secure);
        Ok(())
    }

    fn create_root_page(&mut self, flags: u8) -> Result<Pgno> {
        if !self.is_auto_vacuum() {
            // `btreeCreateTable`: `allocateBtreePage(pBt, &pRoot, &pgnoRoot, 1, 0)`.
            let pgno = self.allocate_btree_page(AllocMode::Near(1))?;
            let r = self.zero_root_page(pgno, flags);
            // sqlite3PagerUnref(pRoot->pDbPage)
            self.unpin(pgno);
            r?;
            return Ok(pgno);
        }

        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let max_root = self.pager.header().largest_root_btree_page;
        if max_root > self.pager.page_count() {
            return Err(SqliteError::corrupt(format!(
                "largest_root_btree_page ({max_root}) exceeds page_count ({})",
                self.pager.page_count()
            )));
        }

        let mut pgno_root = max_root + 1;
        while pgno_root == lock_pg || is_ptrmap_page(pgno_root, usable, lock_pg) {
            pgno_root += 1;
        }

        let pgno_move = self.allocate_btree_page(AllocMode::Exact(pgno_root))?;
        // (auto-vacuum: the references of `btreeCreateTable` around `relocatePage` are not
        // modelled; the allocated page is given back at once)
        self.unpin(pgno_move);
        if pgno_move != pgno_root {
            self.save_cursors_for_mutation(None, None)?;
            let (e_type, ptr_page) = self.ptrmap_get(pgno_root)?;
            if matches!(e_type, PtrmapType::RootPage | PtrmapType::FreePage) {
                return Err(SqliteError::corrupt(format!(
                    "unexpected ptrmap type {e_type:?} at new root target {pgno_root}"
                )));
            }
            self.relocate_page(pgno_root, e_type, ptr_page, pgno_move, false)?;
            // `btreeCreateTable`: the page object went to `pgnoMove` (`sqlite3PagerMovepage`)
            // and `btreeGetPage(pgnoRoot)` reads the page again — from the file. `zeroPage`
            // below rewrites only the header, so the rest of the new root is that image.
            self.pager.restore_stored_image(pgno_root)?;
        }

        self.zero_root_page(pgno_root, flags)?;
        self.ptrmap_put(pgno_root, PtrmapType::RootPage, 0)?;
        self.pager.header_mut()?.largest_root_btree_page = pgno_root;
        Ok(pgno_root)
    }

    /// Creates a new empty Table B+Tree (`0x0d` root leaf) and returns its root `Pgno`.
    pub fn create_table(&mut self) -> Result<Pgno> {
        self.create_root_page(BtreePageType::TableLeaf as u8)
    }

    /// Creates a new empty Index B-Tree (`0x0a` root leaf) and returns its root `Pgno`.
    pub fn create_index(&mut self) -> Result<Pgno> {
        self.create_root_page(BtreePageType::IndexLeaf as u8)
    }

    /// Empties all entries and child/overflow pages of a B-tree rooted at `root_pgno`,
    /// preserving `root_pgno` itself as an empty leaf page (`OP_Clear`).
    pub fn clear_tree(&mut self, root_pgno: Pgno) -> Result<()> {
        self.clear_tree_counting(root_pgno).map(|_| ())
    }

    /// [`Self::clear_tree`] that also returns the number of entries the tree held
    /// (`sqlite3BtreeClearTable` with `pnChange`: the cells of the leaves of a table b-tree,
    /// every cell of an index b-tree), counted during the same walk.
    pub fn clear_tree_counting(&mut self, root_pgno: Pgno) -> Result<u64> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let mut n_entries = 0u64;
        self.clear_database_page(root_pgno, false, 0, &mut n_entries)?;
        Ok(n_entries)
    }

    /// `clearDatabasePage` (`btree.c`): frees every page below `pgno` and every overflow chain,
    /// in C's order — for each cell its child subtree (the child page itself last), then the
    /// cell's overflow chain; then the right-most subtree — and finally either frees `pgno`
    /// (`free_page_flag`) or turns it into an empty leaf (`zeroPage`, which rewrites only the
    /// 8-byte page header unless a secure-delete mode zeroes the rest).
    fn clear_database_page(
        &mut self,
        pgno: Pgno,
        free_page_flag: bool,
        depth: u32,
        n_entries: &mut u64,
    ) -> Result<()> {
        // `getAndInitPage(pBt, pgno, &pPage, 0)` .. `cleardatabasepage_out: releasePage(pPage)`
        if pgno == 0 || pgno > self.pager.page_count() {
            return Err(SqliteError::corrupt(format!(
                "requested invalid page {pgno} (page_count = {})",
                self.pager.page_count()
            )));
        }
        self.pin(pgno)?;
        let res = self.clear_database_page_pinned(pgno, free_page_flag, depth, n_entries);
        self.unpin(pgno);
        res
    }

    fn clear_database_page_pinned(
        &mut self,
        pgno: Pgno,
        free_page_flag: bool,
        depth: u32,
        n_entries: &mut u64,
    ) -> Result<()> {
        self.check_interrupted()?;
        if depth > 64 {
            return Err(SqliteError::corrupt(format!(
                "b-tree deeper than 64 levels at page {pgno}"
            )));
        }
        let usable = self.pager.usable_size();
        // (left child, overflow chain: first page + number of pages) per cell, in cell order
        let mut work: Vec<(Pgno, Pgno, u32)> = Vec::new();
        let ovfl_page_size = usable.saturating_sub(4).max(1);
        let (ptype, right_child) = {
            let raw = self.pager.read_page(pgno)?;
            let (ptype, n_cell, right_child) = page::raw_page_header(pgno, usable, raw)?;
            let ovfl_pages = |total: u32, local: usize| -> u32 {
                (total - local as u32).div_ceil(ovfl_page_size)
            };
            match ptype {
                BtreePageType::TableLeaf => {
                    for i in 0..n_cell {
                        let c = page::read_table_leaf_cell(pgno, usable, raw, n_cell, i)?;
                        if let Some(ov) = c.overflow_page {
                            let n = ovfl_pages(c.total_payload_len, c.local_payload.len());
                            work.push((0, ov, n));
                        }
                    }
                }
                BtreePageType::IndexLeaf => {
                    for i in 0..n_cell {
                        let c = page::read_index_leaf_cell(pgno, usable, raw, n_cell, i)?;
                        if let Some(ov) = c.overflow_page {
                            let n = ovfl_pages(c.total_payload_len, c.local_payload.len());
                            work.push((0, ov, n));
                        }
                    }
                }
                BtreePageType::TableInterior => {
                    work.reserve(n_cell);
                    for i in 0..n_cell {
                        let (child, _) =
                            page::read_table_interior_cell(pgno, usable, raw, n_cell, i)?;
                        work.push((child, 0, 0));
                    }
                }
                BtreePageType::IndexInterior => {
                    work.reserve(n_cell);
                    for i in 0..n_cell {
                        let (child, c) =
                            page::read_index_interior_cell(pgno, usable, raw, n_cell, i)?;
                        let (ov, n) = match c.overflow_page {
                            Some(ov) => {
                                (ov, ovfl_pages(c.total_payload_len, c.local_payload.len()))
                            }
                            None => (0, 0),
                        };
                        work.push((child, ov, n));
                    }
                }
            }
            if ptype != BtreePageType::TableInterior {
                *n_entries += n_cell as u64;
            }
            (ptype, right_child)
        };
        let is_leaf = matches!(ptype, BtreePageType::TableLeaf | BtreePageType::IndexLeaf);
        for (child, ovfl, n_ovfl) in work {
            if child != 0 {
                self.clear_database_page(child, true, depth + 1, n_entries)?;
            }
            if ovfl != 0 {
                self.free_overflow_chain_n(ovfl, n_ovfl)?;
            }
        }
        if !is_leaf {
            self.clear_database_page(right_child, true, depth + 1, n_entries)?;
        }
        if free_page_flag {
            // freePage(pPage)
            return self.free_page_held(pgno);
        }
        // `zeroPage(pPage, flags | PTF_LEAF)`
        let leaf_flag = match ptype {
            BtreePageType::TableLeaf | BtreePageType::TableInterior => 0x0d,
            BtreePageType::IndexLeaf | BtreePageType::IndexInterior => 0x0a,
        };
        let secure = self.secure_delete != 0;
        let hdr = if pgno == 1 {
            sqliters_pager::DB_HEADER_SIZE
        } else {
            0
        };
        let usable_usize = usable as usize;
        let w = self.pager.write_page(pgno)?;
        if secure {
            w[hdr..usable_usize].fill(0);
        }
        w[hdr] = leaf_flag;
        w[hdr + 1..hdr + 5].fill(0);
        let content_start = if usable_usize == 65_536 {
            0u16
        } else {
            usable_usize as u16
        };
        w[hdr + 5..hdr + 7].copy_from_slice(&content_start.to_be_bytes());
        w[hdr + 7] = 0;
        Ok(())
    }

    /// The last page of an overflow chain in `clearCellOverflow`: "pOvfl =
    /// btreePageLookup(pBt, ovflPgno)" — the page is referenced only if the cache holds it —
    /// then `freePage2(pBt, pOvfl, ovflPgno)` and the caller's `sqlite3PagerUnref(pOvfl)`.
    fn free_last_overflow_page(&mut self, pgno: Pgno) -> Result<()> {
        if self.pager.page_lookup(pgno) {
            let r = self.free_page_held(pgno);
            self.unpin(pgno);
            r
        } else {
            self.free_page(pgno)
        }
    }

    /// `clearCellOverflow` (`btree.c`): frees the `n_ovfl` pages of the overflow chain starting
    /// at `first`. The next pointer is read from every page but the last, which is freed
    /// without being read.
    fn free_overflow_chain_n(&mut self, first: Pgno, n_ovfl: u32) -> Result<()> {
        let page_count = self.pager.page_count();
        let mut pgno = first;
        let mut remaining = n_ovfl;
        while remaining > 0 {
            remaining -= 1;
            if pgno < 2 || pgno > page_count {
                return Err(SqliteError::corrupt(format!(
                    "overflow page {pgno} out of range"
                )));
            }
            // `clearCellOverflow`: "if( nOvfl ) getOverflowPage(pBt, ovflPgno, &pOvfl, &iNext)" —
            // every page but the last is fetched (to read the next pointer) and freed while
            // referenced; the last one is looked up in the cache only (`btreePageLookup`).
            let next = if remaining > 0 {
                self.pin(pgno)?;
                let raw = self.pager.read_page_unchecked(pgno)?;
                let n = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                let r = self.free_page_held(pgno);
                self.unpin(pgno);
                r?;
                n
            } else {
                self.free_last_overflow_page(pgno)?;
                0
            };
            pgno = next;
        }
        Ok(())
    }

    /// Destroys a B-tree rooted at `root_pgno` (`root_pgno > 1`). In auto-vacuum mode, relocates
    /// `largest_root_btree_page` into `root_pgno` when `root_pgno < largest_root_btree_page`,
    /// returning `Some(RootPageMove { from, to })` so schema catalog entries can be updated
    /// (`sqlite3BtreeDropTable`, `btree.c:10234-10315`).
    pub fn drop_tree_relocating(&mut self, root_pgno: Pgno) -> Result<Option<RootPageMove>> {
        if root_pgno <= 1 {
            return Err(SqliteError::misuse("cannot drop sqlite_schema root page 1"));
        }
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        // `btreeDropTable`: `sqlite3BtreeClearTable` first (the root becomes an empty leaf),
        // then the root page itself is freed or, in auto-vacuum mode, filled with the last root.
        self.clear_database_page(root_pgno, false, 0, &mut 0)?;

        if !self.is_auto_vacuum() {
            // btreeDropTable: "rc = btreeGetPage(pBt, (Pgno)iTable, &pPage, 0); ...
            // freePage(pPage, &rc); releasePage(pPage);"
            self.pin(root_pgno)?;
            let r = self.free_page_held(root_pgno);
            self.unpin(root_pgno);
            r?;
            return Ok(None);
        }

        let max_root = self.pager.header().largest_root_btree_page;
        if root_pgno > max_root {
            return Err(SqliteError::corrupt(format!(
                "dropped root page {root_pgno} exceeds largest_root_btree_page ({max_root})"
            )));
        }

        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let mut new_max = max_root - 1;
        while new_max > 1 && (new_max == lock_pg || is_ptrmap_page(new_max, usable, lock_pg)) {
            new_max -= 1;
        }

        if root_pgno < max_root {
            self.save_cursors_for_mutation(Some(max_root), None)?;
            self.relocate_page(max_root, PtrmapType::RootPage, 0, root_pgno, false)?;
            self.remap_open_cursor_roots(max_root, root_pgno);
            self.free_page(max_root)?;
            self.pager.header_mut()?.largest_root_btree_page = new_max;
            Ok(Some(RootPageMove {
                from: max_root,
                to: root_pgno,
            }))
        } else {
            self.free_page(root_pgno)?;
            self.pager.header_mut()?.largest_root_btree_page = new_max;
            Ok(None)
        }
    }

    /// Destroys a B-tree rooted at `root_pgno` (`root_pgno > 1`), returning every page (including `root_pgno`) to the freelist (`OP_Destroy`).
    pub fn drop_tree(&mut self, root_pgno: Pgno) -> Result<()> {
        self.drop_tree_relocating(root_pgno).map(|_| ())
    }

    // ========================================================================
    // Table B+Tree Queries, Seeks, Inserts (with Splitting), and Deletes
    // ========================================================================

    /// Counts all entries in a Table B+Tree (`0x0d`/`0x05`) or Index/`WITHOUT ROWID` B-Tree
    /// (`0x0a`/`0x02`) without decoding cell payloads (`OP_Count` in `sqlite/src/vdbe.c`).
    pub fn count_entries(&mut self, root_pgno: Pgno) -> Result<i64> {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(0);
        }
        let max_pages = self.pager.page_count() as usize + 1;
        let mut visited = 0usize;
        // `sqlite3BtreeCount`: the cursor walks the tree depth first (`moveToChild` /
        // `moveToParent`) and ends on the root.
        let vc = self.vc_open_read(root_pgno);
        self.vc_move_to_root(vc)?;
        self.count_entries_page(root_pgno, max_pages, &mut visited)
    }

    fn count_entries_page(
        &mut self,
        pgno: Pgno,
        max_pages: usize,
        visited: &mut usize,
    ) -> Result<i64> {
        self.check_interrupted()?;
        *visited += 1;
        if *visited > max_pages {
            return Err(SqliteError::corrupt(format!(
                "cycle detected while counting B-tree entries at page {pgno}"
            )));
        }
        let hdr_off = if pgno == 1 { 100usize } else { 0usize };
        let (ptype, n_cells, children) = {
            let raw = self.pager.read_page(pgno)?;
            if raw.len() < hdr_off + 8 {
                return Err(SqliteError::corrupt("truncated B-tree page header"));
            }
            let ptype = raw[hdr_off];
            let n_cells = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
            match ptype {
                0x0d | 0x0a => return Ok(n_cells as i64),
                0x05 | 0x02 => {
                    if raw.len() < hdr_off + 12 + n_cells * 2 {
                        return Err(SqliteError::corrupt("truncated interior B-tree page"));
                    }
                    let mut kids = Vec::with_capacity(n_cells + 1);
                    let ptr_base = hdr_off + 12;
                    for i in 0..n_cells {
                        let cell_off =
                            u16::from_be_bytes([raw[ptr_base + i * 2], raw[ptr_base + i * 2 + 1]])
                                as usize;
                        if cell_off + 4 > raw.len() {
                            return Err(SqliteError::corrupt(
                                "interior cell pointer out of bounds",
                            ));
                        }
                        kids.push(u32::from_be_bytes([
                            raw[cell_off],
                            raw[cell_off + 1],
                            raw[cell_off + 2],
                            raw[cell_off + 3],
                        ]));
                    }
                    kids.push(u32::from_be_bytes([
                        raw[hdr_off + 8],
                        raw[hdr_off + 9],
                        raw[hdr_off + 10],
                        raw[hdr_off + 11],
                    ]));
                    (ptype, n_cells, kids)
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "invalid btree page flags byte: {other:#04x}"
                    )));
                }
            }
        };
        let mut total = if ptype == 0x02 { n_cells as i64 } else { 0 };
        for child in children {
            self.pin(child)?;
            let r = self.count_entries_page(child, max_pages, visited);
            self.unpin(child);
            total += r?;
        }
        Ok(total)
    }

    /// Scans all `(rowid, payload_bytes)` entries of a Table B+Tree in ascending `rowid` order.
    pub fn scan_table(&mut self, root_pgno: Pgno) -> Result<Vec<(i64, Vec<u8>)>> {
        if self.pager.page_count() == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let vc = self.vc_open_read(root_pgno);
        self.vc_move_to_root(vc)?;
        self.collect_table_entries(root_pgno, &mut out)?;
        Ok(out)
    }

    fn collect_table_entries(&mut self, pgno: Pgno, out: &mut Vec<(i64, Vec<u8>)>) -> Result<()> {
        self.check_interrupted()?;
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::TableLeaf { cells } => {
                for c in cells {
                    self.check_interrupted()?;
                    let payload = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push((c.rowid, payload));
                }
                Ok(())
            }
            ParsedPage::TableInterior { cells, right_child } => {
                for c in cells {
                    self.pin(c.left_child)?;
                    let r = self.collect_table_entries(c.left_child, out);
                    self.unpin(c.left_child);
                    r?;
                }
                self.pin(right_child)?;
                let r = self.collect_table_entries(right_child, out);
                self.unpin(right_child);
                r
            }
            _ => Err(SqliteError::corrupt(format!(
                "expected table B-tree page at {pgno}, found {:?}",
                parsed.page_type()
            ))),
        }
    }

    /// Point-lookup by `rowid` in O(log N) page reads (`sqlite3BtreeTableMoveto`).
    pub fn seek_table_rowid(&mut self, root_pgno: Pgno, rowid: i64) -> Result<Option<Vec<u8>>> {
        self.emu_seek_rowid(root_pgno, rowid)?;
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        // (No cursor is modelled for this lookup: the engine uses it both where C seeks a
        // read cursor and where C positions the write cursor with `OP_NotExists`.)
        loop {
            self.check_interrupted()?;
            let hit_or_next = {
                let raw = self.pager.read_page(cur_pgno)?;
                match peek_page_type(cur_pgno, usable, raw)? {
                    BtreePageType::TableLeaf => {
                        match seek_table_leaf_cell(cur_pgno, usable, raw, rowid)? {
                            Some(cell_hit) => Ok(Some(cell_hit)),
                            None => return Ok(None),
                        }
                    }
                    BtreePageType::TableInterior => {
                        let (_, next_pgno) =
                            find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        Err(next_pgno)
                    }
                    _ => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table page at {cur_pgno}"
                        )));
                    }
                }
            };
            match hit_or_next {
                Ok(Some((total_payload_len, local_payload, overflow_page))) => {
                    let full =
                        self.read_full_payload(total_payload_len, &local_payload, overflow_page)?;
                    return Ok(Some(full));
                }
                Ok(None) => return Ok(None),
                Err(next_pgno) => cur_pgno = next_pgno,
            }
        }
    }

    /// Point lookup of `rowid` that follows the same descent as [`Self::cursor_seek_rowid`] and,
    /// when the row's record is stored entirely on its leaf (and the page is smaller than
    /// 64 KiB), copies it into `record` (cleared first) and reports where the row sits — the
    /// position accepted by [`Self::insert_table_at`] / [`Self::delete_table_at`]. No cursor is
    /// opened.
    pub fn seek_table_row_local(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        record: &mut Vec<u8>,
    ) -> Result<TableRowSeek> {
        record.clear();
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(TableRowSeek::Missing);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        // (The caller placed the statement's write cursor with `OP_SeekRowid` /
        // `OP_NotExists` — `cc_seek_rowid`; when it is on the row, the row is read from the
        // leaf the cursor holds.)
        {
            let vc = self.vc_open(root_pgno, pins::VC_WRITE);
            if self.vcursors[vc].valid
                && self.vcursors[vc].nkey == Some(rowid)
                && self.vc_depth(vc) > 0
            {
                cur_pgno = self.vc_path(vc)[self.vc_depth(vc) - 1];
            }
        }
        for _ in 0..BTCURSOR_MAX_DEPTH {
            let raw = self.pager.read_page(cur_pgno)?;
            let (ptype, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
            match ptype {
                BtreePageType::TableLeaf => {
                    return Ok(
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Ok((cell_idx, Some((p_start, p_end)))) => {
                                record.extend_from_slice(
                                    &raw[usize::from(p_start)..usize::from(p_end)],
                                );
                                TableRowSeek::Local {
                                    leaf_pgno: cur_pgno,
                                    cell_idx,
                                }
                            }
                            Ok((_, None)) => TableRowSeek::Spilled,
                            Err(_) => TableRowSeek::Missing,
                        },
                    );
                }
                BtreePageType::TableInterior => {
                    let (_, child) = find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                    cur_pgno = child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected table B-tree page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in seek_table_row_local",
        ))
    }

    /// Single-pass `rowid` point lookup that decodes only the requested [`PointProjCol`] columns
    /// directly from the raw `TableLeaf` page buffer without opening a `BtreeCursor` or allocating
    /// a `Vec<u8>` record buffer when the row payload fits in-page.
    pub fn point_lookup_table_columns(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        cols: &[PointProjCol],
    ) -> Result<Option<Vec<Value>>> {
        let vc = self.emu_seek_start(root_pgno, rowid)?;
        let mut child_idx = 0usize;
        let mut hit_ix = usize::MAX;
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        loop {
            self.check_interrupted()?;
            enum LookupStep {
                Descend(Pgno),
                Done(Option<Vec<Value>>),
                Overflow(u32, Vec<u8>, Option<Pgno>),
            }
            let step = {
                let raw = self.pager.read_page(cur_pgno)?;
                let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
                if hdr_off >= usable as usize || raw.len() < usable as usize {
                    return Err(SqliteError::corrupt("raw page smaller than usable_size"));
                }
                match raw[hdr_off] {
                    x if x == BtreePageType::TableInterior as u8 => {
                        let (ci, next_pgno) =
                            find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        child_idx = ci;
                        LookupStep::Descend(next_pgno)
                    }
                    x if x == BtreePageType::TableLeaf as u8 => {
                        let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Err(_) => LookupStep::Done(None),
                            Ok((cell_idx, Some((p_start, p_end)))) => {
                                hit_ix = cell_idx;
                                let payload = &raw[p_start as usize..p_end as usize];
                                let vals =
                                    decode_projected_cols_from_record(payload, rowid, cols, enc)?;
                                LookupStep::Done(Some(vals))
                            }
                            Ok((cell_idx, None)) => {
                                hit_ix = cell_idx;
                                let c =
                                    read_table_leaf_cell(cur_pgno, usable, raw, n_cell, cell_idx)?;
                                LookupStep::Overflow(
                                    c.total_payload_len,
                                    c.local_payload.to_vec(),
                                    c.overflow_page,
                                )
                            }
                        }
                    }
                    _ => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table page at {cur_pgno}"
                        )));
                    }
                }
            };
            match step {
                LookupStep::Descend(next_pgno) => {
                    self.emu_seek_child(vc, next_pgno, child_idx)?;
                    cur_pgno = next_pgno;
                }
                LookupStep::Done(res) => {
                    if hit_ix != usize::MAX {
                        self.emu_seek_leaf(vc, hit_ix, rowid);
                    }
                    return Ok(res);
                }
                LookupStep::Overflow(total_len, local_copy, ovfl) => {
                    self.emu_seek_leaf(vc, hit_ix, rowid);
                    let full = self.read_full_payload(total_len, &local_copy, ovfl)?;
                    let vals = decode_projected_cols_from_record(&full, rowid, cols, enc)?;
                    return Ok(Some(vals));
                }
            }
        }
    }

    /// Zero-allocation `rowid` point lookup that passes the borrowed record payload slice directly
    /// to `visit` when the cell fits in-page (falling back to overflow assembly only for oversized cells).
    pub fn with_table_rowid_raw_cell<R, F>(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        visit: F,
    ) -> Result<Option<R>>
    where
        F: FnOnce(&[u8]) -> Result<R>,
    {
        let vc = self.emu_seek_start(root_pgno, rowid)?;
        let mut child_idx = 0usize;
        let mut hit_ix = usize::MAX;
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        let mut visit_opt = Some(visit);
        loop {
            self.check_interrupted()?;
            enum RawStep<T> {
                Descend(Pgno),
                Done(Option<T>),
                Overflow(u32, Vec<u8>, Option<Pgno>),
            }
            let step = {
                let raw = self.pager.read_page(cur_pgno)?;
                let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
                if hdr_off >= usable as usize || raw.len() < usable as usize {
                    return Err(SqliteError::corrupt("raw page smaller than usable_size"));
                }
                match raw[hdr_off] {
                    x if x == BtreePageType::TableInterior as u8 => {
                        let (ci, next_pgno) =
                            find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        child_idx = ci;
                        RawStep::Descend(next_pgno)
                    }
                    x if x == BtreePageType::TableLeaf as u8 => {
                        let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Err(_) => RawStep::Done(None),
                            Ok((cell_idx, Some((p_start, p_end)))) => {
                                hit_ix = cell_idx;
                                let payload = &raw[p_start as usize..p_end as usize];
                                let f = visit_opt.take().unwrap();
                                RawStep::Done(Some(f(payload)?))
                            }
                            Ok((cell_idx, None)) => {
                                hit_ix = cell_idx;
                                let c =
                                    read_table_leaf_cell(cur_pgno, usable, raw, n_cell, cell_idx)?;
                                RawStep::Overflow(
                                    c.total_payload_len,
                                    c.local_payload.to_vec(),
                                    c.overflow_page,
                                )
                            }
                        }
                    }
                    _ => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table page at {cur_pgno}"
                        )));
                    }
                }
            };
            match step {
                RawStep::Descend(next_pgno) => {
                    self.emu_seek_child(vc, next_pgno, child_idx)?;
                    cur_pgno = next_pgno;
                }
                RawStep::Done(res) => {
                    if hit_ix != usize::MAX {
                        self.emu_seek_leaf(vc, hit_ix, rowid);
                    }
                    return Ok(res);
                }
                RawStep::Overflow(total_len, local_copy, ovfl) => {
                    self.emu_seek_leaf(vc, hit_ix, rowid);
                    let full = self.read_full_payload(total_len, &local_copy, ovfl)?;
                    let f = visit_opt.take().unwrap();
                    return Ok(Some(f(&full)?));
                }
            }
        }
    }

    /// Scans all cells of the Table B+Tree rooted at `root_pgno` in ascending `rowid` order,
    /// passing `(rowid, raw_record_slice)` directly from each `TableLeaf` page with zero heap
    /// allocations for non-overflow cells (`1` `read_page` per leaf page).
    pub fn scan_table_raw_cells<F>(&mut self, root_pgno: Pgno, mut visit: F) -> Result<()>
    where
        F: FnMut(i64, &[u8]) -> Result<()>,
    {
        if self.pager.page_count() == 0 {
            return Ok(());
        }
        let usable = self.pager.usable_size();
        let usable_usize = usable as usize;
        let bounds = PayloadBounds::for_table_leaf(usable);
        let max_local = bounds.max_local as usize;
        let max_pages = (self.pager.page_count() as usize).saturating_add(2);
        let mut visited_pages = 0usize;
        let mut stack: Vec<(Pgno, usize)> = Vec::with_capacity(8);
        stack.push((root_pgno, 0));
        // The read cursor of the scan (`OP_Rewind` / `OP_Next`): `moveToRoot`, `moveToChild`
        // on the way down, `moveToParent` when a page is exhausted; it ends on the root.
        let vc = self.vc_open_read(root_pgno);
        self.vc_move_to_root(vc)?;

        while let Some((pgno, next_idx)) = stack.pop() {
            if next_idx == 0 {
                self.check_interrupted()?;
                visited_pages += 1;
                if visited_pages > max_pages {
                    return Err(SqliteError::corrupt("cycle detected in table B+Tree"));
                }
                if pgno != root_pgno {
                    self.vc_move_to_child(vc, pgno)?;
                }
            }

            enum PageKind {
                InteriorStep {
                    resume: Option<(Pgno, usize)>,
                    child: Pgno,
                },
                Leaf,
            }

            let kind = {
                let raw = self.pager.read_page(pgno)?;
                let (ptype, n_cell, _) = raw_page_header(pgno, usable, raw)?;
                match ptype {
                    BtreePageType::TableInterior => {
                        if next_idx < n_cell {
                            let (left_child, _) =
                                read_table_interior_cell(pgno, usable, raw, n_cell, next_idx)?;
                            PageKind::InteriorStep {
                                resume: Some((pgno, next_idx + 1)),
                                child: left_child,
                            }
                        } else {
                            let right_child = table_interior_right_child(pgno, usable, raw)?;
                            PageKind::InteriorStep {
                                resume: None,
                                child: right_child,
                            }
                        }
                    }
                    BtreePageType::TableLeaf => PageKind::Leaf,
                    _ => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table page at {pgno}"
                        )));
                    }
                }
            };

            match kind {
                PageKind::InteriorStep { resume, child } => {
                    if let Some(r) = resume {
                        stack.push(r);
                    }
                    stack.push((child, 0));
                }
                PageKind::Leaf => {
                    let mut ci = 0usize;
                    loop {
                        let ovfl_cell = {
                            let raw = self.pager.read_page(pgno)?;
                            let (_, n_cell, _) = raw_page_header(pgno, usable, raw)?;
                            let hdr_off: usize = if pgno == 1 { 100 } else { 0 };
                            let ptr_base = hdr_off + 8;
                            let ptr_end = ptr_base
                                .checked_add(2 * n_cell)
                                .ok_or_else(|| SqliteError::corrupt("cell pointer overflow"))?;
                            if ptr_end > usable_usize || raw.len() < usable_usize {
                                return Err(SqliteError::corrupt("cell pointer out of bounds"));
                            }
                            let mut spilled = None;
                            while ci < n_cell {
                                let p = ptr_base + 2 * ci;
                                let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
                                if off < ptr_end || off >= usable_usize {
                                    return Err(SqliteError::corrupt("invalid cell offset"));
                                }
                                let (payload_u64, n1) = get_varint(&raw[off..usable_usize])?;
                                let total_u32 = u32::try_from(payload_u64).map_err(|_| {
                                    SqliteError::corrupt("table cell payload > 4GiB")
                                })?;
                                let total_len = total_u32 as usize;
                                let (rowid, n2) = get_varint_i64(&raw[off + n1..usable_usize])?;
                                let p_start = off + n1 + n2;
                                if total_len <= max_local {
                                    let p_end = p_start + total_len;
                                    if p_end > usable_usize {
                                        return Err(SqliteError::corrupt(
                                            "table leaf cell extends past end of page",
                                        ));
                                    }
                                    visit(rowid, &raw[p_start..p_end])?;
                                    ci += 1;
                                } else {
                                    let local_len = bounds.local_len(total_u32);
                                    let local_end = p_start + local_len;
                                    if local_end + 4 > usable_usize {
                                        return Err(SqliteError::corrupt(
                                            "table leaf overflow cell extends past end of page",
                                        ));
                                    }
                                    let ovfl_pgno = u32::from_be_bytes([
                                        raw[local_end],
                                        raw[local_end + 1],
                                        raw[local_end + 2],
                                        raw[local_end + 3],
                                    ]);
                                    spilled = Some((
                                        rowid,
                                        total_u32,
                                        raw[p_start..local_end].to_vec(),
                                        Some(ovfl_pgno),
                                    ));
                                    ci += 1;
                                    break;
                                }
                            }
                            spilled
                        };

                        if let Some((rowid, total_u32, local_copy, ovfl)) = ovfl_cell {
                            let full = self.read_full_payload(total_u32, &local_copy, ovfl)?;
                            visit(rowid, &full)?;
                        } else {
                            break;
                        }
                    }
                    // `sqlite3BtreeNext` at the end of a leaf: up to the first page that still
                    // has a child to visit (or to the root).
                    loop {
                        if self.vc_depth(vc) <= 1 {
                            break;
                        }
                        self.vc_move_to_parent(vc);
                        let top = *self.vc_path(vc).last().unwrap_or(&root_pgno);
                        if stack.last().is_some_and(|e| e.0 == top) {
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Locates the leaf page and cell payload layout `(leaf_pgno, local_start, local_len, total_payload_len, overflow_page)`
    /// for `rowid` in the Table B+Tree rooted at `root_pgno`.
    #[allow(clippy::type_complexity)]
    fn locate_table_leaf_cell_layout(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
    ) -> Result<Option<(Pgno, usize, usize, u32, Option<Pgno>)>> {
        self.emu_seek_rowid(root_pgno, rowid)?;
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        loop {
            self.check_interrupted()?;
            let raw = self.pager.read_page(cur_pgno)?;
            match peek_page_type(cur_pgno, usable, raw)? {
                BtreePageType::TableLeaf => {
                    let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                    let Ok(cell_idx) =
                        binary_search_table_leaf(cur_pgno, usable, raw, n_cell, rowid)?
                    else {
                        return Ok(None);
                    };
                    let cell_off = raw_cell_offset(cur_pgno, usable, raw, 8, n_cell, cell_idx)?;
                    let slice = &raw[cell_off..usable as usize];
                    let (payload_u64, n1) = get_varint(slice)?;
                    let total_payload_len = u32::try_from(payload_u64)
                        .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
                    let (_, n2) = get_varint_i64(&slice[n1..])?;
                    let bounds = PayloadBounds::for_table_leaf(usable);
                    let local_start = cell_off + n1 + n2;
                    let local_len = bounds.local_len(total_payload_len);
                    let local_end = local_start + local_len;
                    let has_ovfl = (local_len as u32) < total_payload_len;
                    let needed = local_end + if has_ovfl { 4 } else { 0 };
                    if needed > usable as usize {
                        return Err(SqliteError::corrupt(format!(
                            "table leaf cell extends past end of page {cur_pgno}"
                        )));
                    }
                    let overflow_page = if has_ovfl {
                        Some(u32::from_be_bytes([
                            raw[local_end],
                            raw[local_end + 1],
                            raw[local_end + 2],
                            raw[local_end + 3],
                        ]))
                    } else {
                        None
                    };
                    return Ok(Some((
                        cur_pgno,
                        local_start,
                        local_len,
                        total_payload_len,
                        overflow_page,
                    )));
                }
                BtreePageType::TableInterior => {
                    let (_, next_pgno) = find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                    cur_pgno = next_pgno;
                }
                _ => {
                    return Err(SqliteError::corrupt(format!(
                        "expected table page at {cur_pgno}"
                    )));
                }
            }
        }
    }

    /// Calls `f` with the whole record of `rowid` in the Table B+Tree rooted at `root_pgno`
    /// (`None` when there is no such row). A record that lies entirely on its leaf page is
    /// passed in place, without a copy; one with overflow pages is assembled first.
    pub fn with_table_row_payload<R>(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<Option<R>> {
        let Some((leaf_pgno, local_start, local_len, total_payload_len, overflow_page)) =
            self.locate_table_leaf_cell_layout(root_pgno, rowid)?
        else {
            return Ok(None);
        };
        if overflow_page.is_none() && (local_len as u32) == total_payload_len {
            let raw = self.pager.read_page(leaf_pgno)?;
            return f(&raw[local_start..local_start + local_len]).map(Some);
        }
        let Some(full) = self.seek_table_rowid(root_pgno, rowid)? else {
            return Ok(None);
        };
        f(&full).map(Some)
    }

    /// Zero-allocation fast path: checks whether `rowid` exists on a non-overflow leaf cell and
    /// its `col_idx`-th column is a `BLOB` byte-for-byte equal to `expected`.
    /// Returns `Ok(None)` when the cell spills onto overflow pages (caller can fall back).
    pub fn table_rowid_col_blob_eq(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        col_idx: usize,
        expected: &[u8],
    ) -> Result<Option<bool>> {
        let Some((leaf_pgno, local_start, local_len, total_payload_len, overflow_page)) =
            self.locate_table_leaf_cell_layout(root_pgno, rowid)?
        else {
            return Ok(Some(false));
        };
        if overflow_page.is_some() || (local_len as u32) != total_payload_len {
            return Ok(None);
        }
        let raw = self.pager.read_page(leaf_pgno)?;
        let payload = &raw[local_start..local_start + local_len];
        if payload.is_empty() {
            return Err(SqliteError::corrupt("empty record payload"));
        }
        let (hdr_u64, mut hdr_pos) = get_varint(payload)?;
        let hdr_len = usize::try_from(hdr_u64)
            .map_err(|_| SqliteError::corrupt("record header length overflow"))?;
        if hdr_len > payload.len() || hdr_len < hdr_pos {
            return Err(SqliteError::corrupt("record header length out of bounds"));
        }
        let mut body_offset = hdr_len;
        let mut cur_col = 0usize;
        while hdr_pos < hdr_len {
            let (st, n) = get_varint(&payload[hdr_pos..hdr_len])?;
            hdr_pos += n;
            let byte_len = serial_type_len(st)?;
            let Some(end) = body_offset.checked_add(byte_len) else {
                return Err(SqliteError::corrupt("record column extends past payload"));
            };
            if end > payload.len() {
                return Err(SqliteError::corrupt("record column extends past payload"));
            }
            if cur_col == col_idx {
                if st >= 12 && (st & 1) == 0 {
                    return Ok(Some(&payload[body_offset..end] == expected));
                }
                return Ok(Some(false));
            }
            body_offset = end;
            cur_col += 1;
        }
        Ok(Some(false))
    }

    /// Inspects column `col_idx` (0-based within the stored record) of `rowid` in the Table B+Tree
    /// rooted at `root_pgno`, returning `Ok(Some((payload_offset, byte_len)))` if the row exists
    /// and the column is `TEXT` or `BLOB`, `Ok(None)` if `rowid` is not found, or an error if the
    /// column is `NULL`/`INTEGER`/`REAL`.
    pub fn inspect_table_row_column(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        col_idx: usize,
    ) -> Result<Option<(usize, usize)>> {
        let Some(payload) = self.seek_table_rowid(root_pgno, rowid)? else {
            return Ok(None);
        };
        if payload.is_empty() {
            return Err(SqliteError::corrupt("empty record payload"));
        }
        let (hdr_u64, mut hdr_pos) = get_varint(&payload)?;
        let hdr_len = usize::try_from(hdr_u64)
            .map_err(|_| SqliteError::corrupt("record header length overflow"))?;
        if hdr_len > payload.len() || hdr_len < hdr_pos {
            return Err(SqliteError::corrupt(format!(
                "record header length ({hdr_len}) out of bounds (payload len {})",
                payload.len()
            )));
        }
        let mut body_offset = hdr_len;
        let mut cur_col = 0usize;
        while hdr_pos < hdr_len {
            let (st, n) = get_varint(&payload[hdr_pos..hdr_len])?;
            hdr_pos += n;
            let byte_len = serial_type_len(st)?;
            if body_offset
                .checked_add(byte_len)
                .is_none_or(|end| end > payload.len())
            {
                return Err(SqliteError::corrupt("record column extends past payload"));
            }
            if cur_col == col_idx {
                if st < 12 {
                    let ty_name = match st {
                        0 => "null",
                        7 => "real",
                        _ => "integer",
                    };
                    return Err(SqliteError::new(
                        ErrorCode::Error,
                        format!("cannot open value of type {ty_name}"),
                    ));
                }
                return Ok(Some((body_offset, byte_len)));
            }
            body_offset += byte_len;
            cur_col += 1;
        }
        // Trailing omitted column (e.g. `ALTER TABLE ADD COLUMN` on an older row): serial type 0 (`NULL`).
        Err(SqliteError::new(
            ErrorCode::Error,
            "cannot open value of type null",
        ))
    }

    /// Reads `buf.len()` bytes starting at `payload_offset` from the record payload of `rowid` in
    /// the Table B+Tree rooted at `root_pgno`.
    pub fn read_table_payload_slice(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload_offset: usize,
        buf: &mut [u8],
    ) -> Result<()> {
        let Some((leaf_pgno, local_start, local_len, total_payload_len, overflow_page)) =
            self.locate_table_leaf_cell_layout(root_pgno, rowid)?
        else {
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };
        if buf.is_empty() {
            return Ok(());
        }
        let total_len = total_payload_len as usize;
        if payload_offset
            .checked_add(buf.len())
            .is_none_or(|end| end > total_len)
        {
            return Err(SqliteError::corrupt(
                "blob read slice exceeds record payload bounds",
            ));
        }

        let mut remaining_out = buf;
        let mut cur_off = payload_offset;

        if cur_off < local_len {
            let take = (local_len - cur_off).min(remaining_out.len());
            let raw = self.pager.read_page(leaf_pgno)?;
            let src_start = local_start + cur_off;
            remaining_out[..take].copy_from_slice(&raw[src_start..src_start + take]);
            remaining_out = &mut remaining_out[take..];
            cur_off += take;
        }

        if !remaining_out.is_empty() {
            let usable = self.pager.usable_size() as usize;
            let chunk_cap = usable - 4;
            let mut ovfl_stream_off = local_len;
            let mut cur_ovfl = overflow_page;
            let mut hops = 0u32;
            let max_pages = self.pager.page_count().max(1);

            while !remaining_out.is_empty() {
                let pgno = cur_ovfl.ok_or_else(|| {
                    SqliteError::corrupt("truncated overflow chain in read_table_payload_slice")
                })?;
                hops += 1;
                if hops > max_pages {
                    return Err(SqliteError::corrupt("overflow chain cycle"));
                }
                let raw = self.pager.read_page_unchecked(pgno)?;
                let next_ovfl = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                let page_payload_len = total_len.saturating_sub(ovfl_stream_off).min(chunk_cap);
                if cur_off < ovfl_stream_off + page_payload_len {
                    let in_page_off = cur_off - ovfl_stream_off;
                    let take = (page_payload_len - in_page_off).min(remaining_out.len());
                    let src_start = 4 + in_page_off;
                    remaining_out[..take].copy_from_slice(&raw[src_start..src_start + take]);
                    remaining_out = &mut remaining_out[take..];
                    cur_off += take;
                }
                ovfl_stream_off += page_payload_len;
                cur_ovfl = if next_ovfl == 0 {
                    None
                } else {
                    Some(next_ovfl)
                };
            }
        }

        Ok(())
    }

    /// Overwrites `data.len()` bytes in-place starting at `payload_offset` within the record
    /// payload of `rowid` in the Table B+Tree rooted at `root_pgno` (`sqlite3_blob_write`,
    /// `sqlite/src/vdbeblob.c:340-396`), journaling touched leaf and overflow pages via [`Pager::write_page`].
    pub fn write_table_payload_slice(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload_offset: usize,
        data: &[u8],
    ) -> Result<()> {
        let Some((leaf_pgno, local_start, local_len, total_payload_len, overflow_page)) =
            self.locate_table_leaf_cell_layout(root_pgno, rowid)?
        else {
            return Err(SqliteError::new(
                ErrorCode::Abort,
                "blob handle has expired",
            ));
        };
        let total_len = total_payload_len as usize;
        if payload_offset
            .checked_add(data.len())
            .is_none_or(|end| end > total_len)
        {
            return Err(SqliteError::corrupt(
                "blob write slice exceeds record payload bounds",
            ));
        }
        if data.is_empty() {
            return Ok(());
        }
        self.invalidate_all_cursor_caches();

        let mut remaining_in = data;
        let mut cur_off = payload_offset;

        if cur_off < local_len {
            let take = (local_len - cur_off).min(remaining_in.len());
            let page_slice = self.pager.write_page(leaf_pgno)?;
            let dst_start = local_start + cur_off;
            page_slice[dst_start..dst_start + take].copy_from_slice(&remaining_in[..take]);
            remaining_in = &remaining_in[take..];
            cur_off += take;
        }

        if !remaining_in.is_empty() {
            let usable = self.pager.usable_size() as usize;
            let chunk_cap = usable - 4;
            let mut ovfl_stream_off = local_len;
            let mut cur_ovfl = overflow_page;
            let mut hops = 0u32;
            let max_pages = self.pager.page_count().max(1);

            while !remaining_in.is_empty() {
                let pgno = cur_ovfl.ok_or_else(|| {
                    SqliteError::corrupt("truncated overflow chain in write_table_payload_slice")
                })?;
                hops += 1;
                if hops > max_pages {
                    return Err(SqliteError::corrupt("overflow chain cycle"));
                }
                let next_ovfl = {
                    let raw = self.pager.read_page_unchecked(pgno)?;
                    u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])
                };
                let page_payload_len = total_len.saturating_sub(ovfl_stream_off).min(chunk_cap);
                if cur_off < ovfl_stream_off + page_payload_len {
                    let in_page_off = cur_off - ovfl_stream_off;
                    let take = (page_payload_len - in_page_off).min(remaining_in.len());
                    let page_slice = self.pager.write_page(pgno)?;
                    let dst_start = 4 + in_page_off;
                    page_slice[dst_start..dst_start + take].copy_from_slice(&remaining_in[..take]);
                    remaining_in = &remaining_in[take..];
                    cur_off += take;
                }
                ovfl_stream_off += page_payload_len;
                cur_ovfl = if next_ovfl == 0 {
                    None
                } else {
                    Some(next_ovfl)
                };
            }
        }

        Ok(())
    }

    /// Returns the maximum existing `rowid` in a Table B+Tree (`None` if empty) in O(log N) page reads.
    pub fn max_table_rowid(&mut self, root_pgno: Pgno) -> Result<Option<i64>> {
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        loop {
            let raw = self.pager.read_page(cur_pgno)?;
            match peek_page_type(cur_pgno, usable, raw)? {
                BtreePageType::TableLeaf => return last_table_leaf_rowid(cur_pgno, usable, raw),
                BtreePageType::TableInterior => {
                    cur_pgno = table_interior_right_child(cur_pgno, usable, raw)?;
                }
                _ => {
                    return Err(SqliteError::corrupt(
                        "expected table page in max_table_rowid",
                    ));
                }
            }
        }
    }

    /// `sqlite3BtreeLast` on the statement's write cursor of table `root_pgno` (the first
    /// step of `OP_NewRowid`): the cursor goes to the root (`moveToRoot`) and down the
    /// right-most children (`moveToRightmost`), and stays on the last leaf. Returns the
    /// largest rowid.
    fn last_rowid_on_write_cursor(&mut self, root_pgno: Pgno) -> Result<Option<i64>> {
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        // "If the cursor already points to the last entry, this is a no-op."
        if self.vcursors[vc].valid && self.vcursors[vc].at_last && self.vc_depth(vc) > 0 {
            let leaf = *self.vc_path(vc).last().unwrap_or(&root_pgno);
            let mut ai = [0u16; 20];
            if leaf != 0 && leaf <= self.pager.page_count() && self.vc_on_last_page(vc, &mut ai)? {
                let raw = self.pager.read_page(leaf)?;
                if matches!(peek_page_type(leaf, usable, raw), Ok(BtreePageType::TableLeaf)) {
                    let hdr = if leaf == 1 { 100 } else { 0 };
                    let n_cell = usize::from(u16::from_be_bytes([raw[hdr + 3], raw[hdr + 4]]));
                    let last = last_table_leaf_rowid(leaf, usable, raw)?;
                    // (the cursor is on the last cell and knows its rowid: `OP_NewRowid` reads
                    // it with `sqlite3BtreeIntegerKey`)
                    let c = &mut self.vcursors[vc];
                    c.ix = n_cell.saturating_sub(1) as u16;
                    c.nkey = last;
                    return Ok(last);
                }
            }
        }
        self.vc_move_to_root(vc)?;
        let mut cur_pgno = root_pgno;
        loop {
            let raw = self.pager.read_page(cur_pgno)?;
            match peek_page_type(cur_pgno, usable, raw)? {
                BtreePageType::TableLeaf => {
                    let last = last_table_leaf_rowid(cur_pgno, usable, raw)?;
                    if last.is_some() {
                        let hdr = if cur_pgno == 1 { 100 } else { 0 };
                        let n_cell =
                            usize::from(u16::from_be_bytes([raw[hdr + 3], raw[hdr + 4]]));
                        // moveToRightmost succeeded: `pCur->curFlags |= BTCF_AtLast`
                        let c = &mut self.vcursors[vc];
                        c.valid = true;
                        c.at_last = true;
                        c.ix = n_cell.saturating_sub(1) as u16;
                        c.nkey = last;
                    }
                    return Ok(last);
                }
                BtreePageType::TableInterior => {
                    let hdr = if cur_pgno == 1 { 100 } else { 0 };
                    let n_cell = usize::from(u16::from_be_bytes([raw[hdr + 3], raw[hdr + 4]]));
                    cur_pgno = table_interior_right_child(cur_pgno, usable, raw)?;
                    self.vc_move_to_child_at(vc, cur_pgno, n_cell)?;
                }
                _ => {
                    return Err(SqliteError::corrupt(
                        "expected table page in max_table_rowid",
                    ));
                }
            }
        }
    }

    /// `OP_NotExists` on the statement's write cursor of table `root_pgno` (the rowid check of
    /// an INSERT with a given rowid, `sqlite3GenerateConstraintChecks`): the cursor seeks the
    /// rowid (`sqlite3BtreeTableMoveto`: `moveToRoot`, then down) and stays there; the
    /// `OP_Insert` of that rowid uses the position. Nothing happens for a rowid that
    /// `OP_NewRowid` has just produced (C codes no check for it).
    pub fn write_cursor_not_exists(&mut self, root_pgno: Pgno, rowid: i64) -> Result<()> {
        if self.new_rowid_hint == Some((root_pgno, rowid)) || self.pager.page_count() == 0 {
            return Ok(());
        }
        // `sqlite3BtreeTableMoveto` on the write cursor (with its shortcuts); a row that is
        // found is known to the cursor (`BTCF_ValidNKey`), so that the `OP_Insert` of an
        // UPSERT / REPLACE that follows writes through it without another search.
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        self.vc_table_moveto(vc, rowid, false)?;
        self.new_rowid_hint = Some((root_pgno, rowid));
        Ok(())
    }

    /// Allocates the next available `rowid` as `OP_NewRowid` does (vdbe.c): largest rowid + 1,
    /// or `1` for an empty table. When the largest rowid is already 2^63-1, up to 100 random
    /// positive candidates (`v &= MAX_ROWID>>1; v++`) are tried; if every one of them exists the
    /// result is `SQLITE_FULL`.
    pub fn next_rowid(&mut self, root_pgno: Pgno) -> Result<i64> {
        match self.last_rowid_on_write_cursor(root_pgno)? {
            None => {
                self.new_rowid_hint = Some((root_pgno, 1));
                Ok(1)
            }
            Some(max_id) if max_id < i64::MAX => {
                self.new_rowid_hint = Some((root_pgno, max_id + 1));
                Ok(max_id + 1)
            }
            Some(_) => {
                let vfs = Arc::clone(self.pager().vfs());
                for _ in 0..100 {
                    let mut buf = [0u8; 8];
                    vfs.randomness(&mut buf);
                    let probe = (i64::from_le_bytes(buf) & (i64::MAX >> 1)) + 1;
                    if self.seek_table_rowid(root_pgno, probe)?.is_none() {
                        return Ok(probe);
                    }
                }
                Err(SqliteError::new(
                    ErrorCode::Full,
                    "database or disk is full",
                ))
            }
        }
    }

    /// Inserts or replaces `(rowid, payload)` in the Table B+Tree rooted at `root_pgno`
    /// (`sqlite3BtreeInsert`, see `insert.rs`): overflow chains, page splits and deeper roots
    /// are handled exactly as in C (`balance()`), `root_pgno` is preserved (including page 1).
    pub fn insert_table(&mut self, root_pgno: Pgno, rowid: i64, payload: &[u8]) -> Result<()> {
        self.insert_table_at(root_pgno, rowid, payload, None)
    }

    /// Whether any tree has a row replaced in place by its last write ([`Self::in_place_row`]).
    #[must_use]
    #[inline]
    pub fn has_in_place_rows(&self) -> bool {
        !self.in_place_rows.is_empty()
    }

    /// `(rowid, serial)` of the row of table `root_pgno` that the last write to that tree
    /// replaced where it was, without rebalancing; `None` when the last write to the tree was
    /// anything else. C's write cursor then stays on that row (`CURSOR_VALID`,
    /// `BTCF_ValidNKey`), and a following `OP_NotExists` for the next rowid reaches its row
    /// with `sqlite3BtreeNext` + `getCellInfo` (`sqlite3BtreeTableMoveto`: "the requested key
    /// is one more than the previous key") — i.e. with the cell parsed. The serial number
    /// tells two replacements of the same row apart.
    #[must_use]
    #[inline(never)]
    pub fn in_place_row(&self, root_pgno: Pgno) -> Option<(i64, u64)> {
        self.in_place_rows
            .iter()
            .find(|e| e.0 == root_pgno)
            .map(|e| (e.1, e.2))
    }

    /// [`Self::insert_table`] for a caller that already located `rowid`: `position` is the
    /// `(leaf_pgno, cell_idx)` reported by [`Self::table_cursor_position`] for a cursor on
    /// `root_pgno` that was positioned on `rowid` and has not been invalidated since. The
    /// position only saves the descent and the leaf search — it is re-validated against the
    /// page and ignored when it does not hold `rowid`; the resulting pages are the same.
    pub fn insert_table_at(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
        position: Option<(Pgno, usize)>,
    ) -> Result<()> {
        self.insert_table_c(
            root_pgno,
            rowid,
            payload,
            position,
            insert::TableInsertMode::Upsert { overwrite: true },
        )?;
        Ok(())
    }

    /// [`Self::insert_table`] for a row C replaces without having parsed the old cell (its
    /// cursor was only positioned by `OP_NotExists`; `BtCursor.info.nSize==0`): a record of
    /// the same size is not written over the old one with `btreeOverwriteCell` — the new cell
    /// is built first (its overflow pages allocated), then the old one is cleared.
    pub fn insert_table_replace_unparsed(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        self.insert_table_c(
            root_pgno,
            rowid,
            payload,
            None,
            insert::TableInsertMode::Upsert { overwrite: false },
        )?;
        Ok(())
    }

    /// First half of an insert that has to write other b-trees before the table row (C:
    /// `OP_NotExists` on the rowid, the index inserts, then `OP_Insert` reusing the seek):
    /// returns the leaf `rowid` belongs on, or `None` when a row with that rowid exists.
    /// Nothing is modified. Pass the leaf to [`Self::insert_table_at_leaf`].
    pub fn probe_table_insert_leaf(&mut self, root_pgno: Pgno, rowid: i64) -> Result<Option<Pgno>> {
        // OP_NotExists on the table's write cursor
        self.write_cursor_not_exists(root_pgno, rowid)?;
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        for _ in 0..=BTCURSOR_MAX_DEPTH {
            let raw = self.pager.read_page(cur_pgno)?;
            match peek_page_type(cur_pgno, usable, raw)? {
                BtreePageType::TableInterior => {
                    let (_, child, _) =
                        find_table_interior_child_append_hint(cur_pgno, usable, raw, rowid)?;
                    cur_pgno = child;
                }
                BtreePageType::TableLeaf => {
                    if page::last_table_leaf_rowid(cur_pgno, usable, raw)?
                        .is_none_or(|last| last < rowid)
                    {
                        return Ok(Some(cur_pgno));
                    }
                    let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                    return Ok(
                        match binary_search_table_leaf(cur_pgno, usable, raw, n_cell, rowid)? {
                            Ok(_) => None,
                            Err(_) => Some(cur_pgno),
                        },
                    );
                }
                _ => {
                    return Err(SqliteError::corrupt(format!(
                        "page {cur_pgno} is not part of a table b-tree"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt("table b-tree too deep"))
    }

    /// Second half of [`Self::probe_table_insert_leaf`]: inserts `(rowid, payload)`, which is
    /// known to be absent, given the leaf found by the probe. The table must not have been
    /// modified in between. When the cell does not simply fit on that leaf the general insert
    /// runs; the resulting pages are the same either way.
    pub fn insert_table_at_leaf(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
        leaf_pgno: Pgno,
    ) -> Result<()> {
        if self.insert_table_on_leaf_c(root_pgno, rowid, payload, leaf_pgno)? {
            return Ok(());
        }
        self.insert_table(root_pgno, rowid, payload)
    }

    /// Inserts `(rowid, payload)` into the Table B+Tree rooted at `root_pgno` if and only if
    /// `rowid` is not already present, returning `Ok(true)` when inserted or `Ok(false)` on duplicate `rowid`
    /// without mutating any B-tree page.
    pub fn insert_table_unique(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
    ) -> Result<bool> {
        self.insert_table_c(
            root_pgno,
            rowid,
            payload,
            None,
            insert::TableInsertMode::Unique,
        )
    }

    // ------------------------------------------------------------------------------------
    // C's cursor operations, for the statement executors (see `pins.rs`).
    // ------------------------------------------------------------------------------------

    /// `OPFLAG_SAVEPOSITION` for the inserts / deletes that follow on the b-tree `root` (the
    /// one the loop of a one-pass UPDATE / DELETE over several rows runs on); `None`
    /// switches it off again.
    pub fn cc_save_position(&mut self, root: Option<Pgno>) {
        self.c_save_position = root;
    }

    /// `OP_Rewind` on the statement's cursor `(root, role)`: `false` when the tree is empty.
    pub fn cc_rewind(&mut self, root: Pgno, role: u8) -> Result<bool> {
        if self.pager.page_count() == 0 {
            return Ok(false);
        }
        let vc = self.vc_open(root, role);
        let ok = self.vc_first(vc)?;
        if ok {
            self.vc_note_rowid_if_table(vc)?;
        }
        Ok(ok)
    }

    /// `OP_Next`: `false` at the end.
    pub fn cc_next(&mut self, root: Pgno, role: u8) -> Result<bool> {
        if self.pager.page_count() == 0 {
            return Ok(false);
        }
        let vc = self.vc_open(root, role);
        let ok = self.vc_next(vc)?;
        if ok {
            self.vc_note_rowid_if_table(vc)?;
        }
        Ok(ok)
    }

    /// `n` times `OP_Next` (the rows in between are not looked at): how many of them reached
    /// a row.
    pub fn cc_next_n(&mut self, root: Pgno, role: u8, n: u64) -> Result<u64> {
        if self.pager.page_count() == 0 || n == 0 {
            return Ok(0);
        }
        let vc = self.vc_open(root, role);
        let moved = self.vc_next_n(vc, n)?;
        if moved > 0 {
            self.vc_note_rowid_if_table(vc)?;
        }
        Ok(moved)
    }

    /// `OP_SeekRowid` / `OP_NotExists` on the cursor `(root, role)`: whether the row exists.
    pub fn cc_seek_rowid(&mut self, root: Pgno, role: u8, rowid: i64) -> Result<bool> {
        if self.pager.page_count() == 0 {
            return Ok(false);
        }
        let vc = self.vc_open(root, role);
        Ok(self.vc_table_moveto(vc, rowid, false)? == 0)
    }

    /// `OP_SeekGE` (`strict`: `OP_SeekGT`) on the table cursor `(root, role)`: whether the
    /// cursor is on a row afterwards ("if( res<0 || (res==0 && oc==OP_SeekGT) )
    /// sqlite3BtreeNext").
    pub fn cc_seek_rowid_ge(&mut self, root: Pgno, role: u8, rowid: i64, strict: bool) -> Result<bool> {
        if self.pager.page_count() == 0 {
            return Ok(false);
        }
        let vc = self.vc_open(root, role);
        let res = self.vc_table_moveto(vc, rowid, false)?;
        if !self.vcursors[vc].valid {
            // (empty tree)
            return Ok(false);
        }
        let on = if res < 0 || (res == 0 && strict) { self.vc_next(vc)? } else { true };
        if on {
            self.vc_note_rowid_if_table(vc)?;
        }
        Ok(on)
    }

    /// `OP_Close` of the read cursors on `root` (the loop over a table that feeds an index
    /// build is over).
    pub fn cc_close_reads(&mut self, root: Pgno) {
        self.vc_close_reads(root);
    }

    /// `OP_Close` (or the cursor number is opened again): the cursor gives its pages back.
    pub fn cc_close(&mut self, root: Pgno, role: u8) {
        self.vc_close(root, role);
    }

    /// The cursor read its row (`OP_Rowid` / `OP_Column` → `getCellInfo`).
    fn vc_note_rowid_if_table(&mut self, vc: usize) -> Result<()> {
        if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
            let pg = self.vc_path(vc)[self.vc_depth(vc) - 1];
            let hdr = if pg == 1 { 100 } else { 0 };
            let is_table_leaf = self.pager.read_page(pg)?.get(hdr).copied() == Some(0x0d);
            if is_table_leaf {
                self.vc_note_rowid(vc)?;
            }
        }
        Ok(())
    }

    /// The statement that held the read transaction is finished: its cursors are closed
    /// (`sqlite3VdbeHalt` → `closeAllCursors`), then the read transaction ends if nothing else
    /// uses it (see `Pager::end_read_if_unused`).
    pub fn end_read_if_unused(&mut self) -> Result<()> {
        if self.pager.transaction_state() == TransactionState::Read {
            self.vc_close_all();
        }
        self.pager.end_read_if_unused()
    }

    /// Deletes `rowid` from the Table B+Tree rooted at `root_pgno`. Returns `true` if a row was deleted.
    pub fn delete_table(&mut self, root_pgno: Pgno, rowid: i64) -> Result<bool> {
        self.delete_table_at(root_pgno, rowid, None)
    }

    /// [`Self::delete_table`] for a caller that already located `rowid`; see
    /// [`Self::insert_table_at`] for the meaning and validation of `position`.
    ///
    /// As in `sqlite3BtreeDelete` the cell is dropped from its leaf in place and, when the leaf
    /// is then more than two thirds free (`nFree*3 > usableSize*2`), `balance()` redistributes
    /// it with its siblings (see `balance.rs`).
    pub fn delete_table_at(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        position: Option<(Pgno, usize)>,
    ) -> Result<bool> {
        let _ = position;
        self.tr_ins = false;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|del|root={} rowid={}", root_pgno, rowid);
        }
        // OP_Delete: `sqlite3BtreeDelete` on the statement's write cursor, which is on the row
        // (the one-pass scan is there, or `OP_NotExists` put it there; a caller that did
        // neither gets the seek here).
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        self.vc_keep = vc;
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        if self.pager.page_count() == 0 {
            return Ok(false);
        }
        if !(self.vcursors[vc].valid && self.vc_depth(vc) > 0) || self.vcursors[vc].nkey.is_none() {
            if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
                self.vc_note_rowid(vc)?;
            }
        }
        if self.vc_table_moveto(vc, rowid, false)? != 0 {
            return Ok(false);
        }
        if self.vc_depth(vc) == 0 || self.vc_rowid(vc)? != rowid {
            // (the cursor's idea of where it is was out of date: search again)
            let c = &mut self.vcursors[vc];
            c.valid = false;
            c.nkey = None;
            if self.vc_table_moveto(vc, rowid, false)? != 0
                || self.vc_depth(vc) == 0
                || self.vc_rowid(vc)? != rowid
            {
                return Ok(false);
            }
        }
        let usable = self.pager.usable_size() as usize;
        let leaf = self.vc_path(vc)[self.vc_depth(vc) - 1];
        let ix = usize::from(self.vcursors[vc].ix);
        let is_root = self.vc_depth(vc) == 1;
        let hdr_off = if leaf == 1 { 100 } else { 0 };
        // "If the BTREE_SAVEPOSITION bit is on, then the cursor position must be preserved
        // following this delete operation. If the current delete will cause a rebalance, then
        // the cursor will need to be explicitly saved" (bPreserve 1), otherwise 2.
        let mut preserve = 0u8;
        if self.c_save_position == Some(root_pgno) {
            let d = self.pager.read_page(leaf)?;
            let (kind, n_cell, n_free) = balance::page_state(d, leaf, usable)?;
            let p = hdr_off + 8 + 2 * ix;
            let off = usize::from(u16::from_be_bytes([d[p], d[p + 1]]));
            let sz = balance::cell_size(&kind, usable as u32, d, off);
            preserve = if (n_free as usize + sz + 2) > usable * 2 / 3 || n_cell == 1 {
                1
            } else {
                2
            };
        }
        let mut needs_balance = match self.try_delete_table_row_at(root_pgno, leaf, ix, rowid)? {
            Some(nb) => nb,
            None => self.delete_leaf_cell_general(leaf, ix)? && !is_root,
        };
        if is_root {
            needs_balance = false;
        }
        self.dtrace_delete_decision(root_pgno, leaf);
        if needs_balance {
            let mut cur = balance::BalCursor::new(root_pgno);
            if !self.vc_to_bal_checked(vc, &mut cur)? {
                return Err(SqliteError::corrupt(
                    "table cursor does not describe the tree it deleted from",
                ));
            }
            self.balance(&mut cur)?;
        }
        if preserve == 2 {
            // CURSOR_SKIPNEXT: the entry after the deleted one moved into its slot.
            let (_, n_cell) = {
                let d = self.pager.read_page(leaf)?;
                (d[hdr_off], balance::get2(d, hdr_off + 3))
            };
            let c = &mut self.vcursors[vc];
            c.valid = false;
            c.nkey = None;
            if ix >= n_cell {
                c.skip = -1;
                c.ix = n_cell.saturating_sub(1) as u16;
            } else {
                c.skip = 1;
            }
        } else {
            self.vc_move_to_root(vc)?;
            if preserve == 1 {
                // btreeReleaseAllCursorPages + CURSOR_REQUIRESSEEK with the saved key
                self.vc_release_all(vc);
                self.vcursors[vc].seek_rowid = Some(rowid);
            }
        }
        Ok(true)
    }

    /// Decision trace only: the `nFree*3 > usableSize*2` test of `sqlite3BtreeDelete` for the
    /// leaf a cell was just dropped from.
    #[inline(always)]
    #[allow(unused_variables, clippy::unused_self, unexpected_cfgs)]
    fn dtrace_delete_decision(&mut self, root_pgno: Pgno, leaf_pgno: Pgno) {
        #[cfg(sqliters_dtrace)]
        if self.dtrace_persistent() {
            let usable = self.pager.usable_size() as usize;
            if let Ok(raw) = self.pager.read_page(leaf_pgno)
                && let Ok((_, _, n_free)) = balance::page_state(raw, leaf_pgno, usable)
            {
                sqliters_types::dtrace!(
                    "TR|bal|del|root={} pg={} nfree={} usable={} o={}",
                    root_pgno,
                    leaf_pgno,
                    n_free,
                    usable,
                    if n_free as usize * 3 > usable * 2 {
                        "balance"
                    } else {
                        "no-balance"
                    }
                );
                // `balance()` on a root page without overflow cells stops at once; the delete
                // paths do not even call it. The (non-)decision is traced as C makes it.
                if leaf_pgno == root_pgno && n_free as usize * 3 > usable * 2 {
                    sqliters_types::dtrace!(
                        "TR|bal|step|ctx=del pg={} ovfl=0 nfree={} o=root-stop",
                        leaf_pgno,
                        n_free
                    );
                }
            }
        }
    }

    /// Deletes row `rowid` sitting at `cell_idx` of table leaf `leaf_pgno` in place (`dropCell`).
    /// Returns `Ok(None)` (nothing modified) when the position does not hold `rowid` or the
    /// page needs the general path, else `Ok(Some(needs_balance))`: whether the leaf — not the
    /// root — is now more than two thirds free, so that the caller must run `balance()`.
    fn try_delete_table_row_at(
        &mut self,
        root_pgno: Pgno,
        leaf_pgno: Pgno,
        cell_idx: usize,
        rowid: i64,
    ) -> Result<Option<bool>> {
        if leaf_pgno == 0
            || leaf_pgno > self.pager.page_count()
            || leaf_pgno == self.lock_byte_page()
        {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let is_root = leaf_pgno == root_pgno;
        {
            let raw = self.pager.read_page(leaf_pgno)?;
            if !table_leaf_cell_has_rowid(leaf_pgno, usable, raw, cell_idx, rowid, is_root) {
                return Ok(None);
            }
        }
        let (outcome, needs_balance) = {
            let secure = self.zero_freed_cells();
            let raw_mut = self.pager.write_page(leaf_pgno)?;
            let outcome = try_delete_table_leaf_in_place(
                leaf_pgno,
                usable,
                raw_mut,
                rowid,
                is_root,
                Some(cell_idx),
                secure,
            )?;
            let needs = !is_root
                && matches!(outcome, RawLeafDeleteOutcome::DeletedInPlace { .. })
                && balance::leaf_needs_balance(raw_mut, 0, usable as usize)?;
            (outcome, needs)
        };
        match outcome {
            RawLeafDeleteOutcome::DeletedInPlace { overflow_page, .. } => {
                self.free_overflow_chain(overflow_page)?;
                Ok(Some(needs_balance))
            }
            // `NotFound` and `Fallback` leave the page untouched: the general path takes over.
            _ => Ok(None),
        }
    }

    /// The leaf step of `sqlite3BtreeDelete` without shortcuts: `BTREE_CLEAR_CELL` (free the
    /// overflow chain of cell `idx` of leaf `pgno`) and `dropCell`. Returns whether the page is
    /// then more than two thirds free.
    fn delete_leaf_cell_general(&mut self, pgno: Pgno, idx: usize) -> Result<bool> {
        let usable = self.pager.usable_size() as usize;
        let secure = self.zero_freed_cells();
        let hdr = if pgno == 1 { 100 } else { 0 };
        let (kind, mut n_cell, mut n_free, off, info, ovfl) = {
            let d = self.pager.read_page(pgno)?;
            let (kind, n_cell, n_free) = balance::page_state(d, pgno, usable)?;
            if idx >= n_cell || !kind.leaf {
                return Err(SqliteError::corrupt("cell index out of range in delete"));
            }
            let p = hdr + 8 + 2 * idx;
            let off = usize::from(u16::from_be_bytes([d[p], d[p + 1]]));
            if off < hdr + 8 + 2 * n_cell {
                return Err(SqliteError::corrupt("cell inside the cell pointer array"));
            }
            let info = balance::parse_cell(&kind, usable as u32, d, off)?;
            let ovfl = if info.n_local == info.n_payload {
                0
            } else {
                if off + info.n_size > usable {
                    return Err(SqliteError::corrupt("cell extends past end of page"));
                }
                let o = off + info.n_size - 4;
                u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
            };
            (kind, n_cell, n_free, off, info, ovfl)
        };
        let _ = off;
        if info.n_local != info.n_payload {
            let ovfl_page_size = usable as u32 - 4;
            let n_ovfl = (info.n_payload - info.n_local).div_ceil(ovfl_page_size);
            self.free_overflow_chain_n(ovfl, n_ovfl)?;
        }
        let d = self.pager.write_page(pgno)?;
        balance::drop_cell(
            d,
            hdr,
            kind.child_ptr,
            usable,
            &mut n_cell,
            &mut n_free,
            idx,
            info.n_size,
            secure,
        )?;
        Ok(n_free as usize * 3 > usable * 2)
    }

    // ========================================================================
    // Index B-Tree Queries, Seeks, Inserts (with Splitting), and Deletes
    // ========================================================================

    /// Scans all serialized key records in an Index B-Tree in index sort order.
    pub fn scan_index(&mut self, root_pgno: Pgno) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        self.scan_index_raw_cells(root_pgno, false, |rec| {
            out.push(rec.to_vec());
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(out)
    }

    /// Walks an Index B-Tree in forward (`!reverse`) or reverse (`reverse`) sort order,
    /// borrowing raw cell slices directly from 4 KiB page buffers with zero heap allocations
    /// for non-overflow cells and stopping immediately when `visit` returns `ControlFlow::Break(())`.
    pub fn scan_index_raw_cells<F>(
        &mut self,
        root_pgno: Pgno,
        reverse: bool,
        mut visit: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
    {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(());
        }
        let usable = self.pager.usable_size();
        let max_pages = (self.pager.page_count() as usize).saturating_add(2);
        let mut visited_pages = 0usize;
        let vc = self.vc_open_read(root_pgno);
        self.vc_move_to_root(vc)?;
        let _ = self.scan_index_raw_rec(
            root_pgno,
            usable,
            reverse,
            &mut visited_pages,
            max_pages,
            &mut visit,
        )?;
        Ok(())
    }

    /// `moveToChild` .. `moveToParent` around the scan of one child page.
    fn scan_index_raw_child<F>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        reverse: bool,
        visited_pages: &mut usize,
        max_pages: usize,
        visit: &mut F,
    ) -> Result<ControlFlow<()>>
    where
        F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
    {
        self.pin(pgno)?;
        let r = self.scan_index_raw_rec(pgno, usable, reverse, visited_pages, max_pages, visit);
        self.unpin(pgno);
        r
    }

    fn scan_index_raw_rec<F>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        reverse: bool,
        visited_pages: &mut usize,
        max_pages: usize,
        visit: &mut F,
    ) -> Result<ControlFlow<()>>
    where
        F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
    {
        self.check_interrupted()?;
        *visited_pages += 1;
        if *visited_pages > max_pages {
            return Err(SqliteError::corrupt("cycle detected in index B-Tree"));
        }
        let (ptype, n_cell, right_child) = {
            let raw = self.pager.read_page(pgno)?;
            raw_page_header(pgno, usable, raw)?
        };
        match ptype {
            BtreePageType::IndexLeaf => {
                if !reverse {
                    let mut ci = 0usize;
                    while ci < n_cell {
                        enum LeafStep {
                            Done,
                            Broke,
                            Overflow(u32, Vec<u8>, Option<Pgno>),
                        }
                        let step = {
                            let raw = self.pager.read_page(pgno)?;
                            let view = RawIndexLeafView::new(pgno, usable, raw, n_cell);
                            let mut st = LeafStep::Done;
                            while ci < n_cell {
                                let cell = view.cell(ci)?;
                                ci += 1;
                                if cell.overflow_page.is_none() {
                                    if visit(cell.local_payload)?.is_break() {
                                        st = LeafStep::Broke;
                                        break;
                                    }
                                } else {
                                    st = LeafStep::Overflow(
                                        cell.total_payload_len,
                                        cell.local_payload.to_vec(),
                                        cell.overflow_page,
                                    );
                                    break;
                                }
                            }
                            st
                        };
                        match step {
                            LeafStep::Done => break,
                            LeafStep::Broke => return Ok(ControlFlow::Break(())),
                            LeafStep::Overflow(tlen, local_copy, ovfl) => {
                                let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                                if visit(&full)?.is_break() {
                                    return Ok(ControlFlow::Break(()));
                                }
                            }
                        }
                    }
                } else {
                    let mut rem = n_cell;
                    while rem > 0 {
                        enum LeafRevStep {
                            Done,
                            Broke,
                            Overflow(u32, Vec<u8>, Option<Pgno>),
                        }
                        let step = {
                            let raw = self.pager.read_page(pgno)?;
                            let view = RawIndexLeafView::new(pgno, usable, raw, n_cell);
                            let mut st = LeafRevStep::Done;
                            while rem > 0 {
                                rem -= 1;
                                let cell = view.cell(rem)?;
                                if cell.overflow_page.is_none() {
                                    if visit(cell.local_payload)?.is_break() {
                                        st = LeafRevStep::Broke;
                                        break;
                                    }
                                } else {
                                    st = LeafRevStep::Overflow(
                                        cell.total_payload_len,
                                        cell.local_payload.to_vec(),
                                        cell.overflow_page,
                                    );
                                    break;
                                }
                            }
                            st
                        };
                        match step {
                            LeafRevStep::Done => break,
                            LeafRevStep::Broke => return Ok(ControlFlow::Break(())),
                            LeafRevStep::Overflow(tlen, local_copy, ovfl) => {
                                let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                                if visit(&full)?.is_break() {
                                    return Ok(ControlFlow::Break(()));
                                }
                            }
                        }
                    }
                }
                Ok(ControlFlow::Continue(()))
            }
            BtreePageType::IndexInterior => {
                if !reverse {
                    for ci in 0..n_cell {
                        let left_child = {
                            let raw = self.pager.read_page(pgno)?;
                            read_index_interior_cell(pgno, usable, raw, n_cell, ci)?.0
                        };
                        if self
                            .scan_index_raw_child(
                                left_child,
                                usable,
                                false,
                                visited_pages,
                                max_pages,
                                visit,
                            )?
                            .is_break()
                        {
                            return Ok(ControlFlow::Break(()));
                        }
                        let ovfl = {
                            let raw = self.pager.read_page(pgno)?;
                            let (_, cell) =
                                read_index_interior_cell(pgno, usable, raw, n_cell, ci)?;
                            if cell.overflow_page.is_none() {
                                if visit(cell.local_payload)?.is_break() {
                                    return Ok(ControlFlow::Break(()));
                                }
                                None
                            } else {
                                Some((
                                    cell.total_payload_len,
                                    cell.local_payload.to_vec(),
                                    cell.overflow_page,
                                ))
                            }
                        };
                        if let Some((tlen, local_copy, ovfl_pg)) = ovfl {
                            let full = self.read_full_payload(tlen, &local_copy, ovfl_pg)?;
                            if visit(&full)?.is_break() {
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                    }
                    self.scan_index_raw_child(
                        right_child,
                        usable,
                        false,
                        visited_pages,
                        max_pages,
                        visit,
                    )
                } else {
                    if self
                        .scan_index_raw_child(
                            right_child,
                            usable,
                            true,
                            visited_pages,
                            max_pages,
                            visit,
                        )?
                        .is_break()
                    {
                        return Ok(ControlFlow::Break(()));
                    }
                    for ci in (0..n_cell).rev() {
                        let (left_child, ovfl, broke_inline) = {
                            let raw = self.pager.read_page(pgno)?;
                            let (lc, cell) =
                                read_index_interior_cell(pgno, usable, raw, n_cell, ci)?;
                            if cell.overflow_page.is_none() {
                                let brk = visit(cell.local_payload)?.is_break();
                                (lc, None, brk)
                            } else {
                                (
                                    lc,
                                    Some((
                                        cell.total_payload_len,
                                        cell.local_payload.to_vec(),
                                        cell.overflow_page,
                                    )),
                                    false,
                                )
                            }
                        };
                        if broke_inline {
                            return Ok(ControlFlow::Break(()));
                        }
                        if let Some((tlen, local_copy, ovfl_pg)) = ovfl {
                            let full = self.read_full_payload(tlen, &local_copy, ovfl_pg)?;
                            if visit(&full)?.is_break() {
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                        if self
                            .scan_index_raw_child(
                                left_child,
                                usable,
                                true,
                                visited_pages,
                                max_pages,
                                visit,
                            )?
                            .is_break()
                        {
                            return Ok(ControlFlow::Break(()));
                        }
                    }
                    Ok(ControlFlow::Continue(()))
                }
            }
            _ => Err(SqliteError::corrupt("expected index B-tree page")),
        }
    }

    /// Zero-allocation O(log N) existence check for whether any entry in the Index B-Tree rooted
    /// at `root_pgno` matches `prefix_values` under `orders[..prefix_values.len()]`.
    /// An entry whose record has fewer fields than `prefix_values` is damaged and never matches.
    pub fn index_contains_prefix_raw(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(false);
        }
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let max_pages = (self.pager.page_count() as usize).saturating_add(2);
        let mut depth = 0usize;
        let mut pgno = root_pgno;
        // All-integer keys are decided without the general record comparison.
        let probe = IntRangeProbe::new(prefix_values, None, None, orders);

        loop {
            self.check_interrupted()?;
            depth += 1;
            if depth > max_pages {
                return Err(SqliteError::corrupt("cycle detected in index B-Tree"));
            }
            enum PrefixStep {
                Found(bool),
                Descend(Pgno),
                SlowLeaf(usize),
                SlowInterior(usize, Pgno),
            }
            let step = {
                let raw = self.pager.read_page(pgno)?;
                let (ptype, n_cell, right_child) = raw_page_header(pgno, usable, raw)?;
                match ptype {
                    BtreePageType::IndexLeaf => {
                        let view = RawIndexLeafView::new(pgno, usable, raw, n_cell);
                        let mut lo = 0usize;
                        let mut hi = n_cell;
                        let mut slow = false;
                        let mut found = false;
                        while lo < hi {
                            let mid = lo + (hi - lo) / 2;
                            let cell = view.cell(mid)?;
                            if cell.overflow_page.is_some() {
                                slow = true;
                                break;
                            }
                            let ord = compare_index_cell_range(
                                probe.as_ref(),
                                cell.local_payload,
                                prefix_values,
                                None,
                                None,
                                orders,
                                enc,
                            )?;
                            match ord {
                                Ordering::Less => lo = mid + 1,
                                Ordering::Equal => {
                                    // A record with fewer fields than the key compares equal
                                    // on what it has, but it is not the entry looked for.
                                    found = raw_record_has_fields(
                                        cell.local_payload,
                                        prefix_values.len(),
                                    );
                                    break;
                                }
                                Ordering::Greater => hi = mid,
                            }
                        }
                        if slow {
                            PrefixStep::SlowLeaf(n_cell)
                        } else {
                            PrefixStep::Found(found)
                        }
                    }
                    BtreePageType::IndexInterior => {
                        let mut lo = 0usize;
                        let mut hi = n_cell;
                        let mut lo_child = right_child;
                        let mut slow = false;
                        let mut found = false;
                        let mut short = false;
                        while lo < hi {
                            let mid = lo + (hi - lo) / 2;
                            let (lc, cell) =
                                read_index_interior_cell(pgno, usable, raw, n_cell, mid)?;
                            if cell.overflow_page.is_some() {
                                slow = true;
                                break;
                            }
                            let ord = compare_index_cell_range(
                                probe.as_ref(),
                                cell.local_payload,
                                prefix_values,
                                None,
                                None,
                                orders,
                                enc,
                            )?;
                            match ord {
                                Ordering::Less => lo = mid + 1,
                                Ordering::Equal => {
                                    found = true;
                                    short = !raw_record_has_fields(
                                        cell.local_payload,
                                        prefix_values.len(),
                                    );
                                    break;
                                }
                                Ordering::Greater => {
                                    hi = mid;
                                    lo_child = lc;
                                }
                            }
                        }
                        if slow {
                            PrefixStep::SlowInterior(n_cell, right_child)
                        } else if found {
                            PrefixStep::Found(!short)
                        } else {
                            PrefixStep::Descend(lo_child)
                        }
                    }
                    _ => return Err(SqliteError::corrupt("expected index page in seek")),
                }
            };
            match step {
                PrefixStep::Found(b) => return Ok(b),
                PrefixStep::Descend(next_pgno) => pgno = next_pgno,
                PrefixStep::SlowLeaf(n_cell) => {
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let (_, ord) = self.compare_raw_index_cell_at(
                            pgno,
                            usable,
                            false,
                            n_cell,
                            mid,
                            prefix_values,
                            None,
                            None,
                            orders,
                            enc,
                        )?;
                        match ord {
                            Ordering::Less => lo = mid + 1,
                            Ordering::Equal => return Ok(true),
                            Ordering::Greater => hi = mid,
                        }
                    }
                    return Ok(false);
                }
                PrefixStep::SlowInterior(n_cell, right_child) => {
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    let mut lo_child = right_child;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let (lc, ord) = self.compare_raw_index_cell_at(
                            pgno,
                            usable,
                            true,
                            n_cell,
                            mid,
                            prefix_values,
                            None,
                            None,
                            orders,
                            enc,
                        )?;
                        match ord {
                            Ordering::Less => lo = mid + 1,
                            Ordering::Equal => return Ok(true),
                            Ordering::Greater => {
                                hi = mid;
                                lo_child = lc;
                            }
                        }
                    }
                    pgno = lo_child;
                }
            }
        }
    }

    /// Searches an Index B-Tree for all records whose leading columns match `prefix_values` under `orders`!
    /// Runs in O(log N + K) page reads!
    pub fn seek_index_equal(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        let enc = self.encoding();
        let mut out = Vec::new();
        self.seek_index_range_raw_cells(
            root_pgno,
            prefix_values,
            None,
            None,
            orders,
            false,
            |rec| {
                out.push(decode_record_with_encoding(rec, enc)?);
                Ok(ControlFlow::Continue(()))
            },
        )?;
        Ok(out)
    }

    /// Searches an Index B-Tree for all raw serialized records whose leading columns match `prefix_values` under `orders`.
    pub fn seek_index_equal_raw(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<u8>>> {
        let mut matches = Vec::new();
        self.seek_index_range_raw_cells(
            root_pgno,
            prefix_values,
            None,
            None,
            orders,
            false,
            |rec| {
                matches.push(rec.to_vec());
                Ok(ControlFlow::Continue(()))
            },
        )?;
        Ok(matches)
    }

    /// Searches a Table B+Tree for all `(rowid, payload)` entries within optional lower and upper
    /// `rowid` bounds in `O(log N + K)` page reads.
    pub fn seek_table_range(
        &mut self,
        root_pgno: Pgno,
        min_rowid: Option<(i64, bool)>,
        max_rowid: Option<(i64, bool)>,
    ) -> Result<Vec<(i64, Vec<u8>)>> {
        let mut out = Vec::new();
        self.scan_table_range_raw_cells(root_pgno, min_rowid, max_rowid, |rid, payload| {
            out.push((rid, payload.to_vec()));
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(out)
    }

    /// Walks a Table B+Tree across optional `(min_rowid, max_rowid)` bounds in `O(log N + K)` page reads,
    /// borrowing raw cell slices directly from 4 KiB `TableLeaf` pages with zero heap allocations
    /// for non-overflow cells and stopping immediately when `visit` returns `ControlFlow::Break(())`.
    pub fn scan_table_range_raw_cells<F>(
        &mut self,
        root_pgno: Pgno,
        min_rowid: Option<(i64, bool)>,
        max_rowid: Option<(i64, bool)>,
        mut visit: F,
    ) -> Result<()>
    where
        F: FnMut(i64, &[u8]) -> Result<ControlFlow<()>>,
    {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(());
        }
        let lo_inc = match min_rowid {
            None => None,
            Some((v, true)) => Some(v),
            Some((v, false)) => match v.checked_add(1) {
                Some(n) => Some(n),
                None => return Ok(()),
            },
        };
        let hi_inc = match max_rowid {
            None => None,
            Some((v, true)) => Some(v),
            Some((v, false)) => match v.checked_sub(1) {
                Some(n) => Some(n),
                None => return Ok(()),
            },
        };
        if let (Some(lo), Some(hi)) = (lo_inc, hi_inc)
            && lo > hi
        {
            return Ok(());
        }

        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_table_leaf(usable);
        let max_local = bounds.max_local as usize;
        let max_pages = (self.pager.page_count() as usize).saturating_add(2);
        let mut visited_pages = 0usize;
        // The read cursor of the loop: OP_Rewind / OP_SeekGE / OP_SeekGT, then OP_Next.
        let mut ss = self.ss_begin_table(root_pgno, min_rowid)?;
        let flow = self.scan_table_range_raw_rec(
            root_pgno,
            usable,
            bounds,
            max_local,
            lo_inc,
            hi_inc,
            &mut visited_pages,
            max_pages,
            &mut ss,
            &mut visit,
        );
        let broke = !matches!(flow, Ok(ControlFlow::Continue(())));
        self.ss_end(ss, broke)?;
        let _ = flow?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_table_range_raw_rec<F>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        bounds: PayloadBounds,
        max_local: usize,
        lo_inc: Option<i64>,
        hi_inc: Option<i64>,
        visited_pages: &mut usize,
        max_pages: usize,
        ss: &mut pins::ScanSync,
        visit: &mut F,
    ) -> Result<ControlFlow<()>>
    where
        F: FnMut(i64, &[u8]) -> Result<ControlFlow<()>>,
    {
        self.check_interrupted()?;
        *visited_pages += 1;
        if *visited_pages > max_pages {
            return Err(SqliteError::corrupt("cycle detected in table B+Tree"));
        }
        let usable_usize = usable as usize;
        let (ptype, n_cell, right_child) = {
            let raw = self.pager.read_page(pgno)?;
            raw_page_header(pgno, usable, raw)?
        };

        match ptype {
            BtreePageType::TableLeaf => {
                let mut ci = if let Some(lo) = lo_inc {
                    let raw = self.pager.read_page(pgno)?;
                    match binary_search_table_leaf(pgno, usable, raw, n_cell, lo)? {
                        Ok(exact) => exact,
                        Err(ins) => ins,
                    }
                } else {
                    0
                };
                let first_ci = ci;
                if ci < n_cell {
                    self.ss_enter(ss, pgno)?;
                }

                while ci < n_cell {
                    enum TLeafStep {
                        Done,
                        Broke,
                        Overflow(i64, u32, Vec<u8>, Option<Pgno>),
                    }
                    let step = {
                        let raw = self.pager.read_page(pgno)?;
                        let hdr_off: usize = if pgno == 1 { 100 } else { 0 };
                        let ptr_base = hdr_off + 8;
                        let ptr_end = ptr_base + 2 * n_cell;
                        let mut st = TLeafStep::Done;
                        while ci < n_cell {
                            let p = ptr_base + 2 * ci;
                            let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
                            if off < ptr_end || off >= usable_usize {
                                return Err(SqliteError::corrupt("invalid cell offset"));
                            }
                            let (payload_u64, n1) = get_varint(&raw[off..usable_usize])?;
                            let total_u32 = u32::try_from(payload_u64)
                                .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
                            let total_len = total_u32 as usize;
                            let (rowid, n2) = get_varint_i64(&raw[off + n1..usable_usize])?;
                            if let Some(hi) = hi_inc
                                && rowid > hi
                            {
                                st = TLeafStep::Done;
                                break;
                            }
                            ci += 1;
                            let p_start = off + n1 + n2;
                            if total_len <= max_local {
                                let p_end = p_start + total_len;
                                if p_end > usable_usize {
                                    return Err(SqliteError::corrupt(
                                        "table leaf cell extends past end of page",
                                    ));
                                }
                                if visit(rowid, &raw[p_start..p_end])?.is_break() {
                                    st = TLeafStep::Broke;
                                    break;
                                }
                            } else {
                                let local_len = bounds.local_len(total_u32);
                                let local_end = p_start + local_len;
                                if local_end + 4 > usable_usize {
                                    return Err(SqliteError::corrupt(
                                        "table leaf overflow cell extends past end of page",
                                    ));
                                }
                                let ovfl_pgno = u32::from_be_bytes([
                                    raw[local_end],
                                    raw[local_end + 1],
                                    raw[local_end + 2],
                                    raw[local_end + 3],
                                ]);
                                st = TLeafStep::Overflow(
                                    rowid,
                                    total_u32,
                                    raw[p_start..local_end].to_vec(),
                                    Some(ovfl_pgno),
                                );
                                break;
                            }
                        }
                        st
                    };
                    match step {
                        TLeafStep::Done => break,
                        TLeafStep::Broke => {
                            self.ss_leave(ss, pgno, (ci > first_ci).then(|| ci - 1));
                            return Ok(ControlFlow::Break(()));
                        }
                        TLeafStep::Overflow(rowid, tlen, local_copy, ovfl) => {
                            let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                            if visit(rowid, &full)?.is_break() {
                                self.ss_leave(ss, pgno, (ci > first_ci).then(|| ci - 1));
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                    }
                }
                self.ss_leave(ss, pgno, (ci > first_ci).then(|| ci - 1));
                Ok(ControlFlow::Continue(()))
            }
            BtreePageType::TableInterior => {
                let start_idx = if let Some(lo) = lo_inc {
                    let raw = self.pager.read_page(pgno)?;
                    let mut l = 0usize;
                    let mut r = n_cell;
                    while l < r {
                        let mid = l + (r - l) / 2;
                        let (_, key) = read_table_interior_cell(pgno, usable, raw, n_cell, mid)?;
                        if key < lo {
                            l = mid + 1;
                        } else {
                            r = mid;
                        }
                    }
                    l
                } else {
                    0
                };

                let mut visit_right = true;
                for idx in start_idx..n_cell {
                    let (left_child, key) = {
                        let raw = self.pager.read_page(pgno)?;
                        read_table_interior_cell(pgno, usable, raw, n_cell, idx)?
                    };
                    if self
                        .scan_table_range_raw_rec(
                            left_child,
                            usable,
                            bounds,
                            max_local,
                            lo_inc,
                            hi_inc,
                            visited_pages,
                            max_pages,
                            ss,
                            visit,
                        )?
                        .is_break()
                    {
                        return Ok(ControlFlow::Break(()));
                    }
                    if let Some(hi) = hi_inc
                        && key >= hi
                    {
                        visit_right = false;
                        break;
                    }
                }
                if visit_right {
                    self.scan_table_range_raw_rec(
                        right_child,
                        usable,
                        bounds,
                        max_local,
                        lo_inc,
                        hi_inc,
                        visited_pages,
                        max_pages,
                        ss,
                        visit,
                    )
                } else {
                    Ok(ControlFlow::Continue(()))
                }
            }
            _ => Err(SqliteError::corrupt("expected table page in range seek")),
        }
    }

    /// Searches an Index B-Tree for all decoded records matching `prefix_values` on the first
    /// `prefix_values.len()` columns and optional SQL lower/upper bounds on column `prefix_values.len()`.
    pub fn seek_index_range(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        let enc = self.encoding();
        let mut out = Vec::new();
        self.seek_index_range_raw_cells(
            root_pgno,
            prefix_values,
            lower_bound,
            upper_bound,
            orders,
            false,
            |rec| {
                out.push(decode_record_with_encoding(rec, enc)?);
                Ok(ControlFlow::Continue(()))
            },
        )?;
        Ok(out)
    }

    /// Searches an Index B-Tree for all raw serialized records matching `prefix_values` on the first
    /// `prefix_values.len()` columns and optional SQL lower/upper bounds on column `prefix_values.len()`.
    ///
    /// Note: SQL bounds exclude `NULL` on the range column and automatically account for `DESC`
    /// sort order on column `prefix_values.len()`.
    pub fn seek_index_range_raw(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        self.seek_index_range_raw_cells(
            root_pgno,
            prefix_values,
            lower_bound,
            upper_bound,
            orders,
            false,
            |rec| {
                out.push(rec.to_vec());
                Ok(ControlFlow::Continue(()))
            },
        )?;
        Ok(out)
    }

    /// Searches an Index B-Tree across `prefix_values` + optional SQL lower/upper bounds on column
    /// `prefix_values.len()` in forward (`!reverse`) or reverse (`reverse`) index order, borrowing
    /// raw cell slices directly from 4 KiB pages with zero heap allocations for non-overflow cells
    /// and stopping immediately when `visit` returns `ControlFlow::Break(())`.
    #[allow(clippy::too_many_arguments)]
    pub fn seek_index_range_raw_cells<F>(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
        reverse: bool,
        mut visit: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
    {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(());
        }
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let max_pages = (self.pager.page_count() as usize).saturating_add(2);
        let mut visited_pages = 0usize;
        let probe = IntRangeProbe::new(prefix_values, lower_bound, upper_bound, orders);
        // (what the scan visits, for the C cursor of the loop — see `pins::IdxScan`)
        let mut first: Option<Vec<u8>> = None;
        let mut n_seen: u64 = 0;
        let mut counted = |rec: &[u8]| -> Result<ControlFlow<()>> {
            if n_seen == 0 {
                first = Some(rec.to_vec());
            }
            n_seen += 1;
            visit(rec)
        };
        let flow = self.seek_index_range_raw_cells_rec(
            root_pgno,
            usable,
            prefix_values,
            lower_bound,
            upper_bound,
            orders,
            enc,
            probe.as_ref(),
            reverse,
            &mut visited_pages,
            max_pages,
            &mut counted,
        )?;
        // (the lower bound as a record: needed only when nothing was in range)
        let seek_rec = if n_seen == 0 {
            let mut seek_vals: Vec<Value> = prefix_values.to_vec();
            if let Some((v, _)) = if reverse { upper_bound } else { lower_bound } {
                seek_vals.push(v.clone());
            }
            sqliters_types::record::encode_record_with_encoding(&seek_vals, enc)
        } else {
            Vec::new()
        };
        self.idx_scan_done(pins::IdxScan {
            root: root_pgno,
            first,
            seek_rec,
            orders: orders.to_vec(),
            n: n_seen,
            reverse,
            eq_only: !prefix_values.is_empty() && lower_bound.is_none() && upper_bound.is_none(),
            broke: flow.is_break(),
            pos: 0,
            started: false,
            at_end: false,
        })?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_raw_index_cell_at(
        &mut self,
        pgno: Pgno,
        usable: u32,
        is_interior: bool,
        n_cell: usize,
        idx: usize,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<(Pgno, Ordering)> {
        let ovfl = {
            let raw = self.pager.read_page(pgno)?;
            if is_interior {
                let (lc, cell) = read_index_interior_cell(pgno, usable, raw, n_cell, idx)?;
                if cell.overflow_page.is_none() {
                    let ord = compare_raw_record_range(
                        cell.local_payload,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )?;
                    return Ok((lc, ord));
                }
                (
                    lc,
                    cell.total_payload_len,
                    cell.local_payload.to_vec(),
                    cell.overflow_page,
                )
            } else {
                let cell = read_index_leaf_cell(pgno, usable, raw, n_cell, idx)?;
                if cell.overflow_page.is_none() {
                    let ord = compare_raw_record_range(
                        cell.local_payload,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )?;
                    return Ok((0, ord));
                }
                (
                    0,
                    cell.total_payload_len,
                    cell.local_payload.to_vec(),
                    cell.overflow_page,
                )
            }
        };
        let full = self.read_full_payload(ovfl.1, &ovfl.2, ovfl.3)?;
        let ord =
            compare_raw_record_range(&full, prefix_values, lower_bound, upper_bound, orders, enc)?;
        Ok((ovfl.0, ord))
    }

    #[allow(clippy::too_many_arguments)]
    fn seek_index_range_raw_cells_rec<F>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
        enc: TextEncoding,
        probe: Option<&IntRangeProbe>,
        reverse: bool,
        visited_pages: &mut usize,
        max_pages: usize,
        visit: &mut F,
    ) -> Result<ControlFlow<()>>
    where
        F: FnMut(&[u8]) -> Result<ControlFlow<()>>,
    {
        self.check_interrupted()?;
        *visited_pages += 1;
        if *visited_pages > max_pages {
            return Err(SqliteError::corrupt("cycle detected in index B-Tree"));
        }
        enum FastPageOutcome {
            LeafCompleted(ControlFlow<()>),
            SlowLeaf {
                n_cell: usize,
                resume_ci: Option<usize>,
            },
            InteriorBounds {
                n_cell: usize,
                lo: usize,
                eq_end: usize,
                child_after_eq: Pgno,
            },
            SlowInterior {
                n_cell: usize,
                right_child: Pgno,
            },
        }

        let fast_outcome = {
            let raw = self.pager.read_page(pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(pgno, usable, raw)?;
            match ptype {
                BtreePageType::IndexLeaf => {
                    let view = RawIndexLeafView::new(pgno, usable, raw, n_cell);
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    let mut has_ovfl = false;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let cell = view.cell(mid)?;
                        if cell.overflow_page.is_some() {
                            has_ovfl = true;
                            break;
                        }
                        let ord = compare_index_cell_range(
                            probe,
                            cell.local_payload,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )?;
                        if ord == Ordering::Less {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    if has_ovfl {
                        FastPageOutcome::SlowLeaf {
                            n_cell,
                            resume_ci: None,
                        }
                    } else if !reverse {
                        let mut cf = ControlFlow::Continue(());
                        let mut ci = lo;
                        let mut ovfl_at = None;
                        while ci < n_cell {
                            let cell = view.cell(ci)?;
                            if cell.overflow_page.is_some() {
                                ovfl_at = Some(ci);
                                break;
                            }
                            ci += 1;
                            match compare_index_cell_range(
                                probe,
                                cell.local_payload,
                                prefix_values,
                                lower_bound,
                                upper_bound,
                                orders,
                                enc,
                            )? {
                                Ordering::Less => continue,
                                Ordering::Equal => {
                                    if visit(cell.local_payload)?.is_break() {
                                        cf = ControlFlow::Break(());
                                        break;
                                    }
                                }
                                Ordering::Greater => break,
                            }
                        }
                        if let Some(resume_idx) = ovfl_at {
                            FastPageOutcome::SlowLeaf {
                                n_cell,
                                resume_ci: Some(resume_idx),
                            }
                        } else {
                            FastPageOutcome::LeafCompleted(cf)
                        }
                    } else {
                        let mut ulo = lo;
                        let mut uhi = n_cell;
                        while ulo < uhi {
                            let mid = ulo + (uhi - ulo) / 2;
                            let cell = view.cell(mid)?;
                            if cell.overflow_page.is_some() {
                                has_ovfl = true;
                                break;
                            }
                            let ord = compare_index_cell_range(
                                probe,
                                cell.local_payload,
                                prefix_values,
                                lower_bound,
                                upper_bound,
                                orders,
                                enc,
                            )?;
                            if ord == Ordering::Greater {
                                uhi = mid;
                            } else {
                                ulo = mid + 1;
                            }
                        }
                        if !has_ovfl {
                            for idx in (lo..ulo).rev() {
                                if view.cell(idx)?.overflow_page.is_some() {
                                    has_ovfl = true;
                                    break;
                                }
                            }
                        }
                        if has_ovfl {
                            FastPageOutcome::SlowLeaf {
                                n_cell,
                                resume_ci: None,
                            }
                        } else {
                            let mut cf = ControlFlow::Continue(());
                            for idx in (lo..ulo).rev() {
                                let cell = view.cell(idx)?;
                                if compare_index_cell_range(
                                    probe,
                                    cell.local_payload,
                                    prefix_values,
                                    lower_bound,
                                    upper_bound,
                                    orders,
                                    enc,
                                )? == Ordering::Equal
                                    && visit(cell.local_payload)?.is_break()
                                {
                                    cf = ControlFlow::Break(());
                                    break;
                                }
                            }
                            FastPageOutcome::LeafCompleted(cf)
                        }
                    }
                }
                BtreePageType::IndexInterior => {
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    let mut has_ovfl = false;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let (_, cell) = read_index_interior_cell(pgno, usable, raw, n_cell, mid)?;
                        if cell.overflow_page.is_some() {
                            has_ovfl = true;
                            break;
                        }
                        let ord = compare_index_cell_range(
                            probe,
                            cell.local_payload,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )?;
                        if ord == Ordering::Less {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    if has_ovfl {
                        FastPageOutcome::SlowInterior {
                            n_cell,
                            right_child,
                        }
                    } else {
                        let mut ulo = lo;
                        let mut uhi = n_cell;
                        while ulo < uhi {
                            let mid = ulo + (uhi - ulo) / 2;
                            let (_, cell) =
                                read_index_interior_cell(pgno, usable, raw, n_cell, mid)?;
                            if cell.overflow_page.is_some() {
                                has_ovfl = true;
                                break;
                            }
                            let ord = compare_index_cell_range(
                                probe,
                                cell.local_payload,
                                prefix_values,
                                lower_bound,
                                upper_bound,
                                orders,
                                enc,
                            )?;
                            if ord == Ordering::Greater {
                                uhi = mid;
                            } else {
                                ulo = mid + 1;
                            }
                        }
                        if has_ovfl {
                            FastPageOutcome::SlowInterior {
                                n_cell,
                                right_child,
                            }
                        } else {
                            let eq_end = ulo;
                            let child_after_eq = if eq_end < n_cell {
                                read_index_interior_cell(pgno, usable, raw, n_cell, eq_end)?.0
                            } else {
                                right_child
                            };
                            FastPageOutcome::InteriorBounds {
                                n_cell,
                                lo,
                                eq_end,
                                child_after_eq,
                            }
                        }
                    }
                }
                _ => return Err(SqliteError::corrupt("expected index page in range seek")),
            }
        };

        let (n_cell, lo, eq_end, child_after_eq) = match fast_outcome {
            FastPageOutcome::LeafCompleted(cf) => return Ok(cf),
            FastPageOutcome::SlowLeaf { n_cell, resume_ci } => {
                let lo = if let Some(start_ci) = resume_ci {
                    start_ci
                } else {
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let (_, ord) = self.compare_raw_index_cell_at(
                            pgno,
                            usable,
                            false,
                            n_cell,
                            mid,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )?;
                        if ord == Ordering::Less {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }
                    lo
                };

                if !reverse {
                    let mut ci = lo;
                    while ci < n_cell {
                        enum LeafRangeStep {
                            Done,
                            Broke,
                            Overflow(u32, Vec<u8>, Option<Pgno>),
                        }
                        let step = {
                            let raw = self.pager.read_page(pgno)?;
                            let mut st = LeafRangeStep::Done;
                            while ci < n_cell {
                                let cell = read_index_leaf_cell(pgno, usable, raw, n_cell, ci)?;
                                ci += 1;
                                if cell.overflow_page.is_none() {
                                    match compare_raw_record_range(
                                        cell.local_payload,
                                        prefix_values,
                                        lower_bound,
                                        upper_bound,
                                        orders,
                                        enc,
                                    )? {
                                        Ordering::Less => continue,
                                        Ordering::Equal => {
                                            if visit(cell.local_payload)?.is_break() {
                                                st = LeafRangeStep::Broke;
                                                break;
                                            }
                                        }
                                        Ordering::Greater => {
                                            st = LeafRangeStep::Done;
                                            break;
                                        }
                                    }
                                } else {
                                    st = LeafRangeStep::Overflow(
                                        cell.total_payload_len,
                                        cell.local_payload.to_vec(),
                                        cell.overflow_page,
                                    );
                                    break;
                                }
                            }
                            st
                        };
                        match step {
                            LeafRangeStep::Done => break,
                            LeafRangeStep::Broke => return Ok(ControlFlow::Break(())),
                            LeafRangeStep::Overflow(tlen, local_copy, ovfl) => {
                                let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                                match compare_raw_record_range(
                                    &full,
                                    prefix_values,
                                    lower_bound,
                                    upper_bound,
                                    orders,
                                    enc,
                                )? {
                                    Ordering::Less => continue,
                                    Ordering::Equal => {
                                        if visit(&full)?.is_break() {
                                            return Ok(ControlFlow::Break(()));
                                        }
                                    }
                                    Ordering::Greater => break,
                                }
                            }
                        }
                    }
                } else {
                    let mut ulo = lo;
                    let mut uhi = n_cell;
                    while ulo < uhi {
                        let mid = ulo + (uhi - ulo) / 2;
                        let (_, ord) = self.compare_raw_index_cell_at(
                            pgno,
                            usable,
                            false,
                            n_cell,
                            mid,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )?;
                        if ord == Ordering::Greater {
                            uhi = mid;
                        } else {
                            ulo = mid + 1;
                        }
                    }
                    let mut rem = ulo;
                    while rem > lo {
                        enum LeafRevRangeStep {
                            Done,
                            Broke,
                            Overflow(u32, Vec<u8>, Option<Pgno>),
                        }
                        let step = {
                            let raw = self.pager.read_page(pgno)?;
                            let mut st = LeafRevRangeStep::Done;
                            while rem > lo {
                                rem -= 1;
                                let cell = read_index_leaf_cell(pgno, usable, raw, n_cell, rem)?;
                                if cell.overflow_page.is_none() {
                                    if compare_raw_record_range(
                                        cell.local_payload,
                                        prefix_values,
                                        lower_bound,
                                        upper_bound,
                                        orders,
                                        enc,
                                    )? == Ordering::Equal
                                        && visit(cell.local_payload)?.is_break()
                                    {
                                        st = LeafRevRangeStep::Broke;
                                        break;
                                    }
                                } else {
                                    st = LeafRevRangeStep::Overflow(
                                        cell.total_payload_len,
                                        cell.local_payload.to_vec(),
                                        cell.overflow_page,
                                    );
                                    break;
                                }
                            }
                            st
                        };
                        match step {
                            LeafRevRangeStep::Done => break,
                            LeafRevRangeStep::Broke => return Ok(ControlFlow::Break(())),
                            LeafRevRangeStep::Overflow(tlen, local_copy, ovfl) => {
                                let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                                if compare_raw_record_range(
                                    &full,
                                    prefix_values,
                                    lower_bound,
                                    upper_bound,
                                    orders,
                                    enc,
                                )? == Ordering::Equal
                                    && visit(&full)?.is_break()
                                {
                                    return Ok(ControlFlow::Break(()));
                                }
                            }
                        }
                    }
                }
                return Ok(ControlFlow::Continue(()));
            }
            FastPageOutcome::InteriorBounds {
                n_cell,
                lo,
                eq_end,
                child_after_eq,
            } => (n_cell, lo, eq_end, child_after_eq),
            FastPageOutcome::SlowInterior {
                n_cell,
                right_child,
            } => {
                let mut lo = 0usize;
                let mut hi = n_cell;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let (_, ord) = self.compare_raw_index_cell_at(
                        pgno,
                        usable,
                        true,
                        n_cell,
                        mid,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )?;
                    if ord == Ordering::Less {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                let mut ulo = lo;
                let mut uhi = n_cell;
                while ulo < uhi {
                    let mid = ulo + (uhi - ulo) / 2;
                    let (_, ord) = self.compare_raw_index_cell_at(
                        pgno,
                        usable,
                        true,
                        n_cell,
                        mid,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )?;
                    if ord == Ordering::Greater {
                        uhi = mid;
                    } else {
                        ulo = mid + 1;
                    }
                }
                let eq_end = ulo;
                let child_after_eq = if eq_end < n_cell {
                    let raw = self.pager.read_page(pgno)?;
                    read_index_interior_cell(pgno, usable, raw, n_cell, eq_end)?.0
                } else {
                    right_child
                };
                (n_cell, lo, eq_end, child_after_eq)
            }
        };

        if !reverse {
            for ci in lo..eq_end {
                let left_child = {
                    let raw = self.pager.read_page(pgno)?;
                    read_index_interior_cell(pgno, usable, raw, n_cell, ci)?.0
                };
                if self
                    .seek_index_range_raw_cells_rec(
                        left_child,
                        usable,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                        probe,
                        false,
                        visited_pages,
                        max_pages,
                        visit,
                    )?
                    .is_break()
                {
                    return Ok(ControlFlow::Break(()));
                }
                let (ovfl, broke_inline) = {
                    let raw = self.pager.read_page(pgno)?;
                    let (_, cell) = read_index_interior_cell(pgno, usable, raw, n_cell, ci)?;
                    if cell.overflow_page.is_none() {
                        let brk = compare_raw_record_range(
                            cell.local_payload,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )? == Ordering::Equal
                            && visit(cell.local_payload)?.is_break();
                        (None, brk)
                    } else {
                        (
                            Some((
                                cell.total_payload_len,
                                cell.local_payload.to_vec(),
                                cell.overflow_page,
                            )),
                            false,
                        )
                    }
                };
                if broke_inline {
                    return Ok(ControlFlow::Break(()));
                }
                if let Some((tlen, local_copy, ovfl_pg)) = ovfl {
                    let full = self.read_full_payload(tlen, &local_copy, ovfl_pg)?;
                    if compare_raw_record_range(
                        &full,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )? == Ordering::Equal
                        && visit(&full)?.is_break()
                    {
                        return Ok(ControlFlow::Break(()));
                    }
                }
            }
            self.seek_index_range_raw_cells_rec(
                child_after_eq,
                usable,
                prefix_values,
                lower_bound,
                upper_bound,
                orders,
                enc,
                probe,
                false,
                visited_pages,
                max_pages,
                visit,
            )
        } else {
            if self
                .seek_index_range_raw_cells_rec(
                    child_after_eq,
                    usable,
                    prefix_values,
                    lower_bound,
                    upper_bound,
                    orders,
                    enc,
                    probe,
                    true,
                    visited_pages,
                    max_pages,
                    visit,
                )?
                .is_break()
            {
                return Ok(ControlFlow::Break(()));
            }
            for ci in (lo..eq_end).rev() {
                let (left_child, ovfl, broke_inline) = {
                    let raw = self.pager.read_page(pgno)?;
                    let (lc, cell) = read_index_interior_cell(pgno, usable, raw, n_cell, ci)?;
                    if cell.overflow_page.is_none() {
                        let brk = compare_raw_record_range(
                            cell.local_payload,
                            prefix_values,
                            lower_bound,
                            upper_bound,
                            orders,
                            enc,
                        )? == Ordering::Equal
                            && visit(cell.local_payload)?.is_break();
                        (lc, None, brk)
                    } else {
                        (
                            lc,
                            Some((
                                cell.total_payload_len,
                                cell.local_payload.to_vec(),
                                cell.overflow_page,
                            )),
                            false,
                        )
                    }
                };
                if broke_inline {
                    return Ok(ControlFlow::Break(()));
                }
                if let Some((tlen, local_copy, ovfl_pg)) = ovfl {
                    let full = self.read_full_payload(tlen, &local_copy, ovfl_pg)?;
                    if compare_raw_record_range(
                        &full,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                    )? == Ordering::Equal
                        && visit(&full)?.is_break()
                    {
                        return Ok(ControlFlow::Break(()));
                    }
                }
                if self
                    .seek_index_range_raw_cells_rec(
                        left_child,
                        usable,
                        prefix_values,
                        lower_bound,
                        upper_bound,
                        orders,
                        enc,
                        probe,
                        true,
                        visited_pages,
                        max_pages,
                        visit,
                    )?
                    .is_break()
                {
                    return Ok(ControlFlow::Break(()));
                }
            }
            Ok(ControlFlow::Continue(()))
        }
    }

    /// Collects distinct leading `prefix_len`-column tuples from an Index B-Tree for skip-scan execution.
    pub fn scan_index_distinct_prefixes(
        &mut self,
        root_pgno: Pgno,
        prefix_len: usize,
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        if prefix_len == 0 || self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(Vec::new());
        }
        let enc = self.encoding();
        let default_so = KeySortOrder::default();

        // O(D log N) skip-jump fast path for 1-column prefixes (`!nulls_last && !desc`).
        let first_so = orders.first().unwrap_or(&default_so);
        if prefix_len == 1 && !first_so.desc && !first_so.nulls_last {
            let mut distinct: Vec<Vec<Value>> = Vec::new();
            let mut first_val: Option<Value> = None;
            self.scan_index_raw_cells(root_pgno, false, |rec_bytes| {
                let mut f = [RawField::Null; 1];
                if parse_raw_record_prefix(rec_bytes, &mut f)? >= 1 {
                    first_val = Some(f[0].to_owned_value(enc));
                    return Ok(ControlFlow::Break(()));
                }
                Ok(ControlFlow::Continue(()))
            })?;
            let Some(mut cur) = first_val else {
                return Ok(distinct);
            };
            loop {
                distinct.push(vec![cur.clone()]);
                let mut next_val: Option<Value> = None;
                // Note: When `cur` is `Value::Null`, `lower_bound = Some((&Value::Null, false))`
                // matches the first non-NULL key in `compare_raw_record_range`!
                self.seek_index_range_raw_cells(
                    root_pgno,
                    &[],
                    Some((&cur, false)),
                    None,
                    orders,
                    false,
                    |rec_bytes| {
                        let mut f = [RawField::Null; 1];
                        if parse_raw_record_prefix(rec_bytes, &mut f)? >= 1 {
                            next_val = Some(f[0].to_owned_value(enc));
                            return Ok(ControlFlow::Break(()));
                        }
                        Ok(ControlFlow::Continue(()))
                    },
                )?;
                match next_val {
                    Some(nv) => cur = nv,
                    None => break,
                }
            }
            return Ok(distinct);
        }

        let mut distinct: Vec<Vec<Value>> = Vec::new();
        self.scan_index_raw_cells(root_pgno, false, |rec_bytes| {
            let mut stack_buf = [RawField::Null; 8];
            let mut heap_buf;
            let raw_buf: &mut [RawField<'_>] = if prefix_len <= 8 {
                &mut stack_buf[..prefix_len]
            } else {
                heap_buf = vec![RawField::Null; prefix_len];
                &mut heap_buf
            };
            let n = parse_raw_record_prefix(rec_bytes, raw_buf)?;
            if n < prefix_len {
                return Ok(ControlFlow::Continue(()));
            }
            let same_as_prev = if let Some(prev) = distinct.last() {
                let mut eq = true;
                for (i, pv) in prev.iter().enumerate() {
                    let so = orders.get(i).unwrap_or(&default_so);
                    if raw_buf[i].compare_key_to_value(pv, so, enc)? != Ordering::Equal {
                        eq = false;
                        break;
                    }
                }
                eq
            } else {
                false
            };
            if !same_as_prev {
                let owned: Vec<Value> = raw_buf.iter().map(|rf| rf.to_owned_value(enc)).collect();
                distinct.push(owned);
            }
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(distinct)
    }

    /// Inserts a serialized index record (`[indexed_cols..., rowid]`) into an Index B-Tree
    /// (`sqlite3BtreeInsert` on an index cursor, see `insert.rs`).
    pub fn insert_index(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} len={}", root_pgno, record_bytes.len());
        }
        self.insert_index_c(root_pgno, record_bytes, orders, false)
    }

    /// [`Self::insert_index`] through a cursor with the `BTREE_BULKLOAD` hint (C: an index
    /// cursor opened with `OPFLAG_BULKCSR`): when a page overflows, `balance_nonroot` leaves
    /// the left siblings packed instead of evening them out.
    pub fn insert_index_bulk(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|ins|root={} len={}", root_pgno, record_bytes.len());
        }
        self.insert_index_c(root_pgno, record_bytes, orders, true)
    }

    /// `OP_SeekEnd` + `OP_IdxInsert` with `OPFLAG_USESEEKRESULT`: appends `record_bytes` after
    /// the last entry of the index `ac` was opened on, without comparing keys (C: the sorted
    /// output of the sorter in `sqlite3RefillIndex`, the index copy of VACUUM). `preformat`:
    /// the cell comes from `sqlite3BtreeTransferRow` (`OPFLAG_PREFORMAT`).
    pub fn append_index(
        &mut self,
        ac: &mut AppendCursor,
        record_bytes: &[u8],
        preformat: bool,
    ) -> Result<()> {
        self.preformat = preformat;
        let res = self.append_index_c(ac, record_bytes);
        self.preformat = false;
        res
    }

    /// [`Self::insert_index_bulk`] for a caller that inserts keys in ascending order through
    /// one cursor (`ac`, opened with `bulk`): a key greater than the last entry of the index
    /// goes behind it without a search, as in `sqlite3BtreeIndexMoveto`.
    pub fn insert_index_sorted(
        &mut self,
        ac: &mut AppendCursor,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        self.insert_index_sorted_c(ac, record_bytes, orders)
    }

    /// [`Self::insert_table_transfer`] for a caller that inserts rows in ascending rowid order
    /// through one cursor: a rowid greater than the last one of the table goes behind it
    /// without a search, as in `sqlite3BtreeTableMoveto`.
    pub fn insert_table_transfer_sorted(
        &mut self,
        ac: &mut AppendCursor,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        self.preformat = true;
        let res = self.insert_table_sorted_c(ac, rowid, payload);
        self.preformat = false;
        res
    }

    /// [`Self::append_index`] for every record `next` hands out, in that order. `next` may
    /// fail (a uniqueness check between two records): the records before the failure are in
    /// the index, as they are when C's loop aborts.
    pub fn append_index_run<'r>(
        &mut self,
        ac: &mut AppendCursor,
        preformat: bool,
        next: &mut dyn FnMut() -> Result<Option<&'r [u8]>>,
    ) -> Result<()> {
        self.preformat = preformat;
        let res = self.append_index_run_c(ac, next);
        self.preformat = false;
        res
    }

    /// `OP_SeekEnd` + `OP_Insert` with `OPFLAG_APPEND|OPFLAG_USESEEKRESULT|OPFLAG_PREFORMAT`
    /// (the table copy of VACUUM): appends the row after the last one of the table `ac` was
    /// opened on.
    pub fn append_table_transfer(
        &mut self,
        ac: &mut AppendCursor,
        rowid: i64,
        payload: &[u8],
    ) -> Result<()> {
        self.preformat = true;
        let res = self.append_table_c(ac, rowid, payload);
        self.preformat = false;
        res
    }

    /// Deletes a matching serialized entry `record_bytes` from an Index B-Tree rooted at `root_pgno`
    /// without decoding cells into heap `Value`s on the common leaf/divider path.
    ///
    /// The steps are those of `sqlite3BtreeDelete`: a leaf entry is dropped in place; an entry
    /// that is a divider on an interior page is replaced by its in-order predecessor (the last
    /// cell of the right-most leaf below its left child); afterwards `balance()` runs when the
    /// leaf is more than two thirds free, and again at the interior page when needed.
    pub fn delete_index_raw(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        self.tr_ins = false;
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|del|root={} len={}", root_pgno, record_bytes.len());
        }
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        self.vcursors[vc].is_index = true;
        self.vc_keep = vc;
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let key = RawRecordKey::for_index_orders(record_bytes, orders);
        let mut cur = balance::BalCursor::new(root_pgno);
        self.idx_seek_hint.retain(|&r| r != root_pgno);
        // OP_IdxDelete: sqlite3BtreeIndexMoveto, then sqlite3BtreeDelete(BTREE_AUXDELETE).
        self.index_moveto_start(vc, &mut cur, record_bytes, orders)?;
        // (this descent compares on the raw pages only; a cell that continues on overflow
        // pages sends it to the general path below)
        self.c_idx_search_quiet = false;
        for _ in 0..BTCURSOR_MAX_DEPTH {
            let cur_pgno = cur.page();
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            enum RawDelStep {
                Descend(usize, Pgno),
                DividerHit {
                    cell_idx: usize,
                },
                LeafNotFound,
                LeafHit {
                    idx: usize,
                    cell_off: usize,
                    cell_len: usize,
                },
                Fallback,
            }
            let step = {
                let raw = self.pager.read_page(cur_pgno)?;
                if hdr_off < raw.len() {
                    match raw[hdr_off] {
                        x if x == BtreePageType::IndexInterior as u8 => {
                            match probe_index_interior_slot_raw(
                                cur_pgno, usable, raw, &key, orders, enc,
                            )? {
                                Some((RawIndexInteriorStep::Child(c), slot)) => {
                                    RawDelStep::Descend(slot, c)
                                }
                                Some((RawIndexInteriorStep::EqualAt { cell_idx, .. }, _)) => {
                                    RawDelStep::DividerHit { cell_idx }
                                }
                                None => RawDelStep::Fallback,
                            }
                        }
                        x if x == BtreePageType::IndexLeaf as u8 => {
                            match probe_delete_index_leaf_raw(
                                cur_pgno, usable, raw, &key, orders, enc, true,
                            )? {
                                Some(None) => RawDelStep::LeafNotFound,
                                Some(Some((idx, cell_off, cell_len, None))) => {
                                    RawDelStep::LeafHit {
                                        idx,
                                        cell_off,
                                        cell_len,
                                    }
                                }
                                _ => RawDelStep::Fallback,
                            }
                        }
                        _ => RawDelStep::Fallback,
                    }
                } else {
                    RawDelStep::Fallback
                }
            };
            match step {
                RawDelStep::Descend(slot, child) => {
                    if self.bc_descend(&mut cur, slot, child).is_err() {
                        break;
                    }
                }
                RawDelStep::LeafNotFound => return Ok(false),
                RawDelStep::LeafHit {
                    idx,
                    cell_off,
                    cell_len,
                } => {
                    let needs_balance = {
                        let secure = self.zero_freed_cells();
                        let raw_mut = self.pager.write_page(cur_pgno)?;
                        apply_delete_index_leaf_in_place(
                            cur_pgno, usable, raw_mut, idx, cell_off, cell_len, secure,
                        );
                        cur.i_page != 0
                            && balance::leaf_needs_balance(raw_mut, hdr_off, usable as usize)?
                    };
                    self.dtrace_delete_decision(root_pgno, cur_pgno);
                    if needs_balance {
                        self.balance(&mut cur)?;
                    }
                    self.vc_move_to_root(vc)?;
                    return Ok(true);
                }
                RawDelStep::DividerHit { cell_idx } => {
                    self.delete_index_interior_cell(&mut cur, cell_idx)?;
                    self.vc_move_to_root(vc)?;
                    return Ok(true);
                }
                RawDelStep::Fallback => break,
            }
        }

        let vals = decode_record_with_encoding(record_bytes, enc)?;
        let key_len = if orders.is_empty() {
            vals.len()
        } else {
            orders.len().min(vals.len())
        };
        // (the general path searches again, from the root)
        self.vc_move_to_root(vc)?;
        self.delete_index_impl(root_pgno, &vals[..key_len], orders, false, false)
    }

    /// Deletes a matching entry `key_vals` from an Index B-Tree rooted at `root_pgno`.
    pub fn delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|del|root={} len=?", root_pgno);
        }
        self.delete_index_impl(root_pgno, key_vals, orders, true, false)
    }

    /// The uniqueness probe of an insert / update (`OP_NoConflict` on the write cursor of the
    /// UNIQUE index): the entries whose first columns equal `key_vals`. The cursor is left
    /// where the key belongs; an index insert of that key that follows uses the position
    /// (`OPFLAG_USESEEKRESULT`), a `REPLACE` deletes the conflicting entry through it.
    pub fn seek_index_no_conflict(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        let was = self.c_defer_idx;
        self.c_defer_idx = true;
        let n_before = self.idx_scans.len();
        let hits = self.seek_index_equal(root_pgno, key_vals, orders);
        self.idx_scans.truncate(n_before);
        self.c_defer_idx = was;
        let hits = hits?;
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        self.vcursors[vc].is_index = true;
        let rec = sqliters_types::record::encode_record_with_encoding(key_vals, self.encoding());
        let res = self.vc_index_moveto(vc, &rec, orders)?;
        self.idx_seek_hint.retain(|&r| r != root_pgno);
        if res != 0 && self.vcursors[vc].valid {
            self.idx_seek_hint.push(root_pgno);
        }
        Ok(hits)
    }

    /// [`Self::seek_index_equal`] for a check of the engine that has no counterpart in C's
    /// program (no cursor moves).
    pub fn seek_index_equal_quiet(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<Value>>> {
        let was = self.c_defer_idx;
        self.c_defer_idx = true;
        let n_before = self.idx_scans.len();
        let hits = self.seek_index_equal(root_pgno, key_vals, orders);
        self.idx_scans.truncate(n_before);
        self.c_defer_idx = was;
        hits
    }

    /// A new row is about to be checked and written: no index write cursor is where an
    /// earlier `OP_NoConflict` of the statement left it for this row.
    pub fn cc_clear_seek_hints(&mut self) {
        self.idx_seek_hint.clear();
    }

    /// `OP_NoConflict` / `OP_NotFound` on the cursor `(root, role)` of an index (or of the
    /// PRIMARY KEY b-tree of a WITHOUT ROWID table): `sqlite3BtreeIndexMoveto` to `key_vals`.
    /// Nothing moves when the cursor is already on an entry with that key. With
    /// `for_insert` an insert of the key that follows uses the position
    /// (`OPFLAG_USESEEKRESULT`). Returns whether an entry with the key exists.
    pub fn cc_index_moveto(
        &mut self,
        root_pgno: Pgno,
        role: u8,
        key_vals: &[Value],
        orders: &[KeySortOrder],
        for_insert: bool,
    ) -> Result<bool> {
        if self.pager.page_count() == 0 || root_pgno == 0 {
            return Ok(false);
        }
        let vc = self.vc_open(root_pgno, role);
        self.vcursors[vc].is_index = true;
        let rec = sqliters_types::record::encode_record_with_encoding(key_vals, self.encoding());
        if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
            let pg = self.vc_path(vc)[self.vc_depth(vc) - 1];
            let ix = usize::from(self.vcursors[vc].ix);
            if self.index_cell_equals(pg, ix, &rec, orders)? {
                return Ok(true);
            }
        }
        let res = self.vc_index_moveto(vc, &rec, orders)?;
        if for_insert {
            self.idx_seek_hint.retain(|&r| r != root_pgno);
            if res != 0 && self.vcursors[vc].valid {
                self.idx_seek_hint.push(root_pgno);
            }
        }
        Ok(res == 0)
    }

    /// `OP_Delete` on the index cursor a one-pass DELETE scans with (`iIdxNoSeek` of
    /// `sqlite3GenerateRowDelete`): the cursor is on the entry, nothing is searched; with
    /// `OPFLAG_SAVEPOSITION` (see [`Self::cc_save_position`]) the cursor keeps its place for
    /// the `OP_Next` that follows.
    pub fn delete_index_at_cursor(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        if self.dtrace_persistent() {
            sqliters_types::dtrace!("TR|w|del|root={} len=?", root_pgno);
        }
        self.delete_index_impl(root_pgno, key_vals, orders, true, true)
    }

    /// General form of the index delete: locates the entry comparing decoded values (cells
    /// with overflow pages included), then performs the same steps as [`Self::delete_index_raw`].
    fn delete_index_impl(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
        moveto: bool,
        positioned: bool,
    ) -> Result<bool> {
        self.tr_ins = false;
        let vc = self.vc_open(root_pgno, pins::VC_WRITE);
        self.vcursors[vc].is_index = true;
        self.vc_keep = vc;
        let saved = self.save_cursors_for_mutation(Some(root_pgno), None);
        self.vc_keep = usize::MAX;
        saved?;
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let mut cur = balance::BalCursor::new(root_pgno);
        cur.vc = vc;
        self.idx_seek_hint.retain(|&r| r != root_pgno);
        let mut on_entry = positioned && self.vcursors[vc].valid && self.vc_depth(vc) > 0;
        if on_entry {
            // the cursor must be on an entry with this key, else the entry is searched
            let pg = self.vc_path(vc)[self.vc_depth(vc) - 1];
            let ix = usize::from(self.vcursors[vc].ix);
            let rec = sqliters_types::record::encode_record_with_encoding(key_vals, self.encoding());
            on_entry = self.index_cell_equals(pg, ix, &rec, orders)?;
        }
        // bPreserve of sqlite3BtreeDelete (0: no BTREE_SAVEPOSITION)
        let save = on_entry && self.c_save_position == Some(root_pgno);
        let mut preserve: u8 = u8::from(save);
        let key_rec = if save {
            sqliters_types::record::encode_record_with_encoding(key_vals, enc)
        } else {
            Vec::new()
        };
        if on_entry && !self.vc_to_bal_checked(vc, &mut cur)? {
            // (the cursor's stack is out of date: the entry is searched from the root)
            on_entry = false;
            cur = balance::BalCursor::new(root_pgno);
            cur.vc = vc;
        }
        if on_entry {
            // (the delete starts on the page the cursor is on)
        } else if moveto {
            // OP_IdxDelete: sqlite3BtreeIndexMoveto (the cursor may already be on the page;
            // the key is needed as a record only to test that).
            if self.vcursors[vc].valid && self.vc_depth(vc) > 0 {
                let rec = sqliters_types::record::encode_record_with_encoding(key_vals, enc);
                self.index_moveto_start(vc, &mut cur, &rec, orders)?;
            } else {
                self.index_moveto_start(vc, &mut cur, &[], orders)?;
            }
        }
        loop {
            let pgno = cur.page();
            // Raw probes first (no cell is decoded into heap values); pages they cannot judge
            // (cells with overflow pages, an exact hit on a divider, ...) take the parsed path.
            enum Step {
                Descend(usize, Pgno),
                NotFound,
                LeafHit(usize, usize, usize),
                Parsed,
            }
            let hdr_off = if pgno == 1 { 100 } else { 0 };
            let step = {
                let raw = self.pager.read_page(pgno)?;
                match raw.get(hdr_off).copied() {
                    Some(x) if x == BtreePageType::IndexInterior as u8 => {
                        match page::find_index_interior_slot_in_place(
                            pgno, usable, raw, key_vals, orders, enc,
                        )? {
                            Some((slot, child)) => Step::Descend(slot, child),
                            None => Step::Parsed,
                        }
                    }
                    Some(x) if x == BtreePageType::IndexLeaf as u8 => {
                        match probe_delete_index_leaf_in_place(
                            pgno, usable, raw, key_vals, orders, enc, true,
                        )? {
                            Some(None) => Step::NotFound,
                            Some(Some((idx, off, len, None))) => Step::LeafHit(idx, off, len),
                            _ => Step::Parsed,
                        }
                    }
                    _ => Step::Parsed,
                }
            };
            match step {
                Step::Descend(slot, child) => {
                    self.bc_descend(&mut cur, slot, child)?;
                    continue;
                }
                Step::NotFound => return Ok(false),
                Step::LeafHit(idx, cell_off, cell_len) => {
                    if save {
                        let d = self.pager.read_page(pgno)?;
                        let (_, n_cell, n_free) = balance::page_state(d, pgno, usable as usize)?;
                        preserve = if (n_free as usize + cell_len + 2) > usable as usize * 2 / 3
                            || n_cell == 1
                        {
                            1
                        } else {
                            2
                        };
                    }
                    let needs_balance = {
                        let secure = self.zero_freed_cells();
                        let raw_mut = self.pager.write_page(pgno)?;
                        apply_delete_index_leaf_in_place(
                            pgno, usable, raw_mut, idx, cell_off, cell_len, secure,
                        );
                        cur.i_page != 0
                            && balance::leaf_needs_balance(raw_mut, hdr_off, usable as usize)?
                    };
                    self.dtrace_delete_decision(root_pgno, pgno);
                    if needs_balance {
                        self.balance(&mut cur)?;
                    }
                    self.idx_after_delete(vc, preserve, pgno, idx, &key_rec, orders)?;
                    return Ok(true);
                }
                Step::Parsed => {
                    // (a page with a cell that continues on overflow pages: C's binary search
                    // reads each such cell it probes through the page cache — `accessPayload`
                    // in `sqlite3BtreeIndexMoveto`; a cursor that is already on the entry
                    // compares nothing)
                    if !on_entry {
                        let flag = self.pager.read_page(pgno)?.get(hdr_off).copied().unwrap_or(0);
                        if flag == 0x0a || flag == 0x02 {
                            let kind = crate::balance::PgKind::decode(flag, usable)?;
                            let rec =
                                sqliters_types::record::encode_record_with_encoding(key_vals, enc);
                            let _ = self.index_page_search_full(pgno, &kind, &rec, orders, enc, true)?;
                        }
                    }
                }
            }
            match self.read_parsed_page(pgno)? {
                ParsedPage::IndexLeaf { cells } => {
                    let mut found_idx = None;
                    for (i, c) in cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => {}
                            Ordering::Equal => {
                                found_idx = Some(i);
                                break;
                            }
                            Ordering::Greater => break,
                        }
                    }
                    let Some(idx) = found_idx else {
                        return Ok(false);
                    };
                    let needs_balance = self.delete_leaf_cell_general(pgno, idx)?;
                    self.dtrace_delete_decision(root_pgno, pgno);
                    if needs_balance && cur.i_page != 0 {
                        self.balance(&mut cur)?;
                    }
                    self.idx_after_delete(vc, preserve.min(1), pgno, idx, &key_rec, orders)?;
                    return Ok(true);
                }
                ParsedPage::IndexInterior { cells, right_child } => {
                    let mut next = (cells.len(), right_child);
                    for (ci, c) in cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => {}
                            Ordering::Equal => {
                                self.delete_index_interior_cell(&mut cur, ci)?;
                                self.idx_after_delete(vc, preserve.min(1), pgno, ci, &key_rec, orders)?;
                                return Ok(true);
                            }
                            Ordering::Greater => {
                                next = (ci, c.left_child);
                                break;
                            }
                        }
                    }
                    self.bc_descend(&mut cur, next.0, next.1)?;
                }
                _ => return Err(SqliteError::corrupt("expected index page")),
            }
        }
    }

    /// The end of `sqlite3BtreeDelete` for the index cursor `vc` (`preserve`: bPreserve).
    fn idx_after_delete(
        &mut self,
        vc: usize,
        preserve: u8,
        pgno: Pgno,
        idx: usize,
        key_rec: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        if preserve == 2 && self.vc_is_on(vc, pgno) {
            // CURSOR_SKIPNEXT: the entry after the deleted one moved into its slot.
            let hdr = if pgno == 1 { 100 } else { 0 };
            let n_cell = balance::get2(self.pager.read_page(pgno)?, hdr + 3);
            let c = &mut self.vcursors[vc];
            c.valid = false;
            if idx >= n_cell {
                c.skip = -1;
                c.ix = n_cell.saturating_sub(1) as u16;
            } else {
                c.skip = 1;
                c.ix = idx as u16;
            }
            return Ok(());
        }
        self.vc_move_to_root(vc)?;
        if preserve != 0 {
            // btreeReleaseAllCursorPages + CURSOR_REQUIRESSEEK with the saved key
            self.vc_release_all(vc);
            self.vcursors[vc].seek_key = Some((key_rec.to_vec(), orders.to_vec()));
        }
        Ok(())
    }

    /// The interior-cell branch of `sqlite3BtreeDelete`: `cur` points at an interior index page
    /// whose cell `i_cell_idx` is the entry to delete.
    ///
    /// The cursor moves to the largest entry below that cell (`sqlite3BtreePrevious`); the cell
    /// is dropped (its overflow chain freed); the leaf entry takes its place in the interior
    /// page — as an overflow cell when it does not fit — and is dropped from the leaf; then
    /// `balance()` runs on the leaf when that is more than two thirds free and, if the cursor
    /// is still below the interior page afterwards, on the interior page itself.
    #[inline(never)]
    fn delete_index_interior_cell(
        &mut self,
        cur: &mut balance::BalCursor,
        i_cell_idx: usize,
    ) -> Result<()> {
        let usable = self.pager.usable_size() as usize;
        let secure = self.zero_freed_cells();
        let root_pgno = cur.pg[0];
        let i_cell_depth = cur.i_page;
        let page = cur.page();
        let hdr = if page == 1 { 100 } else { 0 };
        let be2 = |d: &[u8], i: usize| usize::from(u16::from_be_bytes([d[i], d[i + 1]]));
        let be4 = |d: &[u8], i: usize| u32::from_be_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]]);

        // The cell to delete and the child below it.
        let (pkind, mut p_ncell, mut p_nfree, info, cell_ovfl, mut child) = {
            let d = self.pager.read_page(page)?;
            let (pkind, p_ncell, p_nfree) = balance::page_state(d, page, usable)?;
            if pkind.leaf || pkind.int_key || p_ncell <= i_cell_idx {
                return Err(SqliteError::corrupt("interior index cell out of range"));
            }
            let off = be2(d, hdr + 12 + 2 * i_cell_idx);
            if off < hdr + 12 + 2 * p_ncell || off + 4 > usable {
                return Err(SqliteError::corrupt("cell inside the cell pointer array"));
            }
            let info = balance::parse_cell(&pkind, usable as u32, d, off)?;
            let cell_ovfl = if info.n_local == info.n_payload {
                0
            } else {
                if off + info.n_size > usable {
                    return Err(SqliteError::corrupt("cell extends past end of page"));
                }
                be4(d, off + info.n_size - 4)
            };
            (pkind, p_ncell, p_nfree, info, cell_ovfl, be4(d, off))
        };

        // sqlite3BtreePrevious(): the right-most leaf below the left child of the cell.
        self.bc_descend(cur, i_cell_idx, child)?;
        loop {
            if child < 2 || child > self.pager.page_count() {
                return Err(SqliteError::corrupt("child page number out of range"));
            }
            let d = self.pager.read_page(child)?;
            match peek_page_type(child, usable as u32, d)? {
                BtreePageType::IndexLeaf => break,
                BtreePageType::IndexInterior => {
                    let n = be2(d, 3);
                    let rc = be4(d, 8);
                    self.bc_descend(cur, n, rc)?;
                    child = rc;
                }
                _ => return Err(SqliteError::corrupt("expected index page")),
            }
        }
        let leaf = cur.page();

        // Make the page writable, free the overflow pages of the entry, drop the cell.
        if info.n_local != info.n_payload {
            let ovfl_page_size = usable as u32 - 4;
            let n_ovfl = (info.n_payload - info.n_local).div_ceil(ovfl_page_size);
            self.free_overflow_chain_n(cell_ovfl, n_ovfl)?;
        }
        {
            let d = self.pager.write_page(page)?;
            balance::drop_cell(
                d,
                hdr,
                4,
                usable,
                &mut p_ncell,
                &mut p_nfree,
                i_cell_idx,
                info.n_size,
                secure,
            )?;
        }

        // The last cell of the leaf replaces it.
        let (lkind, mut l_ncell, mut l_nfree) = {
            let d = self.pager.read_page(leaf)?;
            balance::page_state(d, leaf, usable)?
        };
        if l_ncell == 0 {
            return Err(SqliteError::corrupt(
                "empty leaf below an interior index cell",
            ));
        }
        let n = cur.pg[i_cell_depth + 1];
        let mut cellbuf: Vec<u8> = Vec::new();
        let n_cell_sz = {
            let d = self.pager.read_page(leaf)?;
            let off = be2(d, 8 + 2 * (l_ncell - 1));
            if off < 4 || off + 4 > usable {
                return Err(SqliteError::corrupt("leaf cell offset out of range"));
            }
            let sz = balance::cell_size(&lkind, usable as u32, d, off);
            // `insertCell(pPage, iCellIdx, pCell-4, nCell+4-(pCell[0]==2), pTmp, n)`: a leaf
            // cell reported with the 4-byte minimum really has 3 bytes when its payload is 2.
            let ins = sz + 4 - usize::from(d[off] == 2);
            if off + ins - 4 > d.len() {
                return Err(SqliteError::corrupt("leaf cell extends past end of page"));
            }
            cellbuf.extend_from_slice(&[0, 0, 0, 0]);
            cellbuf.extend_from_slice(&d[off..off + ins - 4]);
            sz
        };
        self.pager.write_page(leaf)?;
        let mut ovfl = balance::OvflCells::default();
        let _fits = self.insert_cell(
            page,
            &pkind,
            &mut p_ncell,
            &mut p_nfree,
            &mut ovfl,
            i_cell_idx,
            &cellbuf,
            Some(n),
        )?;
        cur.set_ovfl(i_cell_depth, ovfl);
        {
            let last = l_ncell - 1;
            let d = self.pager.write_page(leaf)?;
            balance::drop_cell(
                d,
                0,
                0,
                usable,
                &mut l_ncell,
                &mut l_nfree,
                last,
                n_cell_sz,
                secure,
            )?;
        }
        if self.dtrace_persistent() {
            sqliters_types::dtrace!(
                "TR|bal|delint|root={} pg={} idx={} leaf={} sz={} o={}",
                root_pgno,
                page,
                i_cell_idx,
                leaf,
                cellbuf.len(),
                if _fits { "fits" } else { "overflow" }
            );
        }
        self.dtrace_delete_decision(root_pgno, leaf);

        if l_nfree as usize * 3 > usable * 2 {
            self.balance(cur)?;
        }
        if cur.i_page > i_cell_depth {
            // "releasePageNotNull(pCur->pPage); pCur->iPage--; while( pCur->iPage>iCellDepth )
            // releasePage(pCur->apPage[pCur->iPage--]);"
            if cur.vc != usize::MAX {
                while self.vc_depth(cur.vc) > i_cell_depth + 1 {
                    self.vc_move_to_parent(cur.vc);
                }
            }
            cur.i_page = i_cell_depth;
            if self.dtrace_persistent() {
                sqliters_types::dtrace!(
                    "TR|bal|delup|root={} pg={} o=second-balance",
                    root_pgno,
                    page
                );
            }
            self.balance(cur)?;
        }
        Ok(())
    }
}

/// `true` when the header of the serialized record `rec` is well formed as far as its first
/// `n` fields go, i.e. the record has at least that many fields.
fn raw_record_has_fields(rec: &[u8], n: usize) -> bool {
    let Ok((hdr_size, mut pos)) = get_varint(rec) else {
        return false;
    };
    let Ok(end) = usize::try_from(hdr_size) else {
        return false;
    };
    if end > rec.len() {
        return false;
    }
    let mut count = 0usize;
    while count < n {
        if pos >= end {
            return false;
        }
        let Ok((_, len)) = get_varint(&rec[pos..end]) else {
            return false;
        };
        pos += len;
        count += 1;
    }
    true
}

/// Coordinates a multi-database two-phase commit across `btrees` following `vdbeCommit`
/// (`sqlite/src/vdbeaux.c:2963-3185`).
///
/// Reuses [`create_super_journal_file`] when `main_db_path` is non-empty and more than one
/// durable rollback-journal participant is in [`TransactionState::Write`], running
/// [`Btree::commit_phase_one`] (including `auto_vacuum_commit` when enabled), unlinking the
/// super-journal with `sync_dir = true` as the atomic commit point, and executing benign
/// [`Btree::commit_phase_two`] cleanup (`cleanup_benign = true`) across all participants.
pub fn commit_btrees_two_phase(
    vfs: &dyn Vfs,
    main_db_path: Option<&Path>,
    enable_8_3: bool,
    btrees: &mut [&mut Btree],
    retain_reads: &[bool],
) -> Result<()> {
    let mut n_trans = 0usize;
    let mut need_xcommit = false;

    // (`sqlite3VdbeHalt`: `closeAllCursors` comes before `vdbeCommit`.)
    for bt in btrees.iter_mut() {
        bt.vc_close_all();
    }
    for bt in btrees.iter_mut() {
        if bt.transaction_state() == TransactionState::Write {
            need_xcommit = true;
            if bt.needs_super_journal_in_write_tx() {
                n_trans += 1;
            }
            bt.acquire_exclusive_lock()?;
        }
    }

    let main_db_str = main_db_path
        .and_then(Path::to_str)
        .filter(|s| !s.is_empty());

    let Some(main_str) = main_db_str.filter(|_| n_trans > 1) else {
        if need_xcommit {
            for bt in btrees.iter_mut() {
                if bt.transaction_state() == TransactionState::Write {
                    bt.commit_phase_one(None)?;
                }
            }
        }
        for (i, bt) in btrees.iter_mut().enumerate() {
            let retain = retain_reads.get(i).copied().unwrap_or(false);
            bt.commit_phase_two(retain, false)?;
        }
        return Ok(());
    };

    let participant_journals: Vec<PathBuf> = btrees
        .iter()
        .filter(|bt| bt.transaction_state() == TransactionState::Write)
        .filter_map(|bt| bt.journal_path().map(Path::to_path_buf))
        .collect();
    let participant_refs: Vec<&Path> = participant_journals.iter().map(PathBuf::as_path).collect();

    let (z_super, mut p_super) =
        create_super_journal_file(vfs, main_str, enable_8_3, &participant_refs)?;

    let mut phase_one_err: Option<SqliteError> = None;
    for bt in btrees.iter_mut() {
        if let Err(e) = bt.commit_phase_one(Some(&z_super)) {
            phase_one_err = Some(e);
            break;
        }
    }
    let close_res = p_super.close();
    if let Some(err) = phase_one_err {
        return Err(err);
    }
    close_res?;

    vfs.delete(Path::new(&z_super), true)?;

    for (i, bt) in btrees.iter_mut().enumerate() {
        let retain = retain_reads.get(i).copied().unwrap_or(false);
        let _ = bt.commit_phase_two(retain, true);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqliters_types::decode_record;
    use sqliters_types::encode_record;

    /// Index inserts must keep every entry, in index order, with a healthy tree — across leaf
    /// splits, parent splits, root growth, short (padded) cells, overflow cells and replaced
    /// (equal) keys.
    #[test]
    fn test_index_inserts_keep_order_and_integrity_across_splits() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }

        for (page_size, n_rows, mode) in [
            (512u32, 6_000i64, 0u8),
            (512, 3_000, 1),
            (1024, 6_000, 2),
            (4096, 20_000, 0),
            (4096, 6_000, 3),
        ] {
            let mut raw_bt = Btree::open_in_memory().unwrap();
            raw_bt.pager_mut().set_page_size(page_size).unwrap();
            raw_bt.begin_write().unwrap();
            let raw_root = raw_bt.create_index().unwrap();
            let mut expected: Vec<Vec<u8>> = Vec::new();
            let enc = raw_bt.encoding();
            let orders = [KeySortOrder::default(), KeySortOrder::default()];
            let single = [KeySortOrder::default()];

            let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ u64::from(page_size) ^ u64::from(mode);
            for i in 1..=n_rows {
                let r = next(&mut seed);
                let (rec, ord): (Vec<u8>, &[KeySortOrder]) = match mode {
                    // Small `(bucket, rowid)` keys arriving in non-monotonic order.
                    0 => (
                        encode_record(&[Value::Integer((r % 25) as i64), Value::Integer(i)]),
                        &orders,
                    ),
                    // Variable-width text keys, some spilling onto overflow pages.
                    1 => {
                        let len = if r.is_multiple_of(23) {
                            700
                        } else {
                            (r % 40) as usize
                        };
                        let text: String = (0..len)
                            .map(|j| char::from(b'a' + ((r >> (j % 48)) % 26) as u8))
                            .collect();
                        (
                            encode_record(&[Value::Text(text), Value::Integer(i)]),
                            &orders,
                        )
                    }
                    // Single-column keys with many 2- and 3-byte (padded) cells and duplicates,
                    // which exercise the replace/equal fallbacks.
                    2 => (
                        encode_record(&[Value::Integer((r % 3_000) as i64)]),
                        &single,
                    ),
                    // Descending-ish arrival.
                    _ => (
                        encode_record(&[
                            Value::Integer(n_rows - i + (r % 7) as i64),
                            Value::Integer(i),
                        ]),
                        &orders,
                    ),
                };
                raw_bt.insert_index(raw_root, &rec, ord).unwrap();
                // An equal key replaces the entry.
                let cmp = |a: &Vec<u8>| {
                    compare_raw_records_with_encoding(a, &rec, Some(ord.len()), ord, enc).unwrap()
                };
                match expected.binary_search_by(cmp) {
                    Ok(pos) => expected[pos] = rec,
                    Err(pos) => expected.insert(pos, rec),
                }
            }

            let n_pages = raw_bt.pager().page_count();
            assert!(
                n_pages > 8,
                "expected a multi-level tree, got {n_pages} pages"
            );
            assert!(
                raw_bt.scan_index(raw_root).unwrap() == expected,
                "index content differs (page_size {page_size}, mode {mode})"
            );
            assert_eq!(
                raw_bt.integrity_check(&[1, raw_root]).unwrap(),
                Vec::<String>::new()
            );
        }
    }

    /// The raw index walkers (page-level leaf view, integer range probe) must visit exactly the
    /// records the general record comparison selects, in index order: `scan_index_raw_cells`
    /// against the sorted list of inserted records, `seek_index_range_raw_cells` (forward and
    /// reverse) against a full scan filtered with `compare_raw_record_range`. Covers integer,
    /// REAL, TEXT, BLOB and NULL keys and targets, the i64 extremes, inclusive / exclusive /
    /// missing / inverted bounds, bounds on duplicate keys, DESC columns (no probe), early
    /// `Break`, overflow cells and multi-level trees on 512-byte to 64 KiB pages.
    #[test]
    fn test_index_raw_scan_and_int_range_seek_match_general_compare() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn key_value(state: &mut u64, span: u64, odd_per_32: u64) -> Value {
            let r = next(state);
            if (r >> 40) % 32 >= odd_per_32 {
                return Value::Integer(((r >> 8) % span) as i64);
            }
            match (r >> 8) % 8 {
                0 => Value::Null,
                1 => Value::Real(((r >> 16) % span) as f64 + 0.5),
                2 => Value::Real(((r >> 16) % span) as f64),
                3 => Value::Text(["abc", "", "7", "Z"][((r >> 16) % 4) as usize].to_owned()),
                4 => Value::Blob(vec![1u8; ((r >> 16) % 3) as usize]),
                5 => Value::Integer([i64::MAX, i64::MIN, 1 << 40][((r >> 16) % 3) as usize]),
                6 => Value::Text("w".repeat(100 + ((r >> 16) % 900) as usize)),
                _ => Value::Integer(-(((r >> 16) % span) as i64)),
            }
        }
        fn target(state: &mut u64, span: u64) -> Value {
            let r = next(state);
            let i = ((r >> 8) % (span + 2)) as i64 - 1;
            match (r >> 40) % 20 {
                0 => Value::Null,
                1 => Value::Real(i as f64 + 0.5),
                2 => Value::Real(i as f64),
                3 => Value::Text("abc".to_owned()),
                4 => Value::Integer([i64::MAX, i64::MIN][((r >> 16) % 2) as usize]),
                _ => Value::Integer(i),
            }
        }

        let enc = TextEncoding::Utf8;
        let asc = KeySortOrder::default();
        let desc = KeySortOrder {
            desc: true,
            ..KeySortOrder::default()
        };
        for (page_size, n_rows, odd_per_32, desc_col) in [
            (512u32, 5_000i64, 0u64, None),
            (512, 4_000, 3, None),
            (4096, 12_000, 1, None),
            (4096, 3_000, 2, Some(1usize)),
            (1024, 3_000, 2, Some(0)),
            (65_536, 4_000, 4, None),
            (4096, 0, 0, None),
            (512, 1, 0, None),
        ] {
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(page_size).unwrap();
            bt.begin_write().unwrap();
            let root = bt.create_index().unwrap();
            let mut orders = [asc.clone(), asc.clone(), asc.clone()];
            if let Some(col) = desc_col {
                orders[col] = desc.clone();
            }
            let mut seed = 0xA076_1D64_78BD_642Fu64 ^ u64::from(page_size) ^ (n_rows as u64) << 24;
            let mut inserted: Vec<Vec<u8>> = Vec::new();
            for i in 1..=n_rows {
                let rec = encode_record(&[
                    key_value(&mut seed, 9, odd_per_32),
                    key_value(&mut seed, 400, odd_per_32),
                    Value::Integer(i),
                ]);
                bt.insert_index(root, &rec, &orders).unwrap();
                inserted.push(rec);
            }
            if n_rows >= 3_000 && page_size <= 4096 {
                assert!(bt.pager().page_count() > 8, "expected a multi-level tree");
            }

            // Full scans, both directions, against the sorted inserted records.
            inserted.sort_by(|a, b| {
                compare_raw_records_with_encoding(a, b, None, &orders, enc).unwrap()
            });
            let scan = |bt: &mut Btree, reverse: bool, stop_after: usize| -> Vec<Vec<u8>> {
                let mut out = Vec::new();
                bt.scan_index_raw_cells(root, reverse, |rec| {
                    out.push(rec.to_vec());
                    Ok(if out.len() == stop_after {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    })
                })
                .unwrap();
                out
            };
            let forward = scan(&mut bt, false, usize::MAX);
            assert!(forward == inserted, "forward scan (page {page_size})");
            let mut backward = scan(&mut bt, true, usize::MAX);
            backward.reverse();
            assert!(backward == inserted, "reverse scan (page {page_size})");
            assert!(scan(&mut bt, false, 7) == inserted[..inserted.len().min(7)]);

            // Range seeks against the general comparison applied to every record.
            let mut probed = 0usize;
            let mut matched = 0usize;
            for round in 0..1_500usize {
                let k = [1usize, 1, 1, 0, 2][round % 5];
                let prefix: Vec<Value> =
                    [target(&mut seed, 9), target(&mut seed, 400)][..k].to_vec();
                let span = if k == 0 { 9 } else { 400 };
                let bound = |seed: &mut u64| -> Option<(Value, bool)> {
                    if next(seed).is_multiple_of(4) || k == 2 {
                        return None;
                    }
                    Some((target(seed, span), next(seed).is_multiple_of(2)))
                };
                let lower = bound(&mut seed);
                let upper = bound(&mut seed);
                let lo_ref = lower.as_ref().map(|(v, inc)| (v, *inc));
                let hi_ref = upper.as_ref().map(|(v, inc)| (v, *inc));
                probed +=
                    usize::from(IntRangeProbe::new(&prefix, lo_ref, hi_ref, &orders).is_some());
                let expect: Vec<&Vec<u8>> = inserted
                    .iter()
                    .filter(|rec| {
                        compare_raw_record_range(rec, &prefix, lo_ref, hi_ref, &orders, enc)
                            .unwrap()
                            == Ordering::Equal
                    })
                    .collect();
                matched += expect.len();
                for reverse in [false, true] {
                    let stop_after = if round % 7 == 3 { 5 } else { usize::MAX };
                    let mut got: Vec<Vec<u8>> = Vec::new();
                    bt.seek_index_range_raw_cells(
                        root,
                        &prefix,
                        lo_ref,
                        hi_ref,
                        &orders,
                        reverse,
                        |rec| {
                            got.push(rec.to_vec());
                            Ok(if got.len() == stop_after {
                                ControlFlow::Break(())
                            } else {
                                ControlFlow::Continue(())
                            })
                        },
                    )
                    .unwrap();
                    let want: Vec<&Vec<u8>> = if reverse {
                        expect.iter().rev().take(stop_after).copied().collect()
                    } else {
                        expect.iter().take(stop_after).copied().collect()
                    };
                    assert!(
                        got.iter().eq(want.iter().copied()),
                        "page {page_size} reverse {reverse}: {prefix:?} {lower:?} {upper:?}: \
                         {} records, expected {}",
                        got.len(),
                        want.len()
                    );
                }
            }
            if n_rows >= 3_000 {
                if desc_col.is_none() {
                    assert!(probed > 300, "only {probed} seeks used the integer probe");
                }
                assert!(matched > 20_000, "only {matched} records matched");
            }
        }
    }

    /// Row replacements / deletions addressed through a positioned cursor must leave every
    /// page byte-identical to the path that descends from the root and searches the leaf — across full leaves, freeblocks, short padded
    /// cells, overflow rows, splits, emptied leaves, Page-1 roots and auto-vacuum.
    #[test]
    fn test_table_positioned_ops_match_general_path_byte_for_byte() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn payload(r: u64, page_size: u32) -> Vec<u8> {
            let len = match r % 23 {
                0 => 0,
                1 => 1,
                2 => page_size as usize, // overflows
                3 => (page_size / 3) as usize,
                _ => 20 + ((r >> 8) % 90) as usize,
            };
            let mut rec: Vec<u8> = (0..len).map(|j| (r >> (j % 56)) as u8).collect();
            // `integrity_check` rejects a payload whose first byte (the record header size)
            // is below 2, as C does ("Bad cell header size").
            if let Some(first) = rec.first_mut() {
                *first |= 2;
            }
            rec
        }

        for (page_size, auto_vacuum, n_rows, n_ops) in [
            (512u32, false, 400i64, 12_000u32),
            (1024, false, 1_500, 20_000),
            (4096, false, 4_000, 30_000),
            (4096, true, 2_000, 20_000),
            (65_536, false, 3_000, 12_000),
            (512, true, 40, 4_000),
        ] {
            let mut fast = Btree::open_in_memory().unwrap();
            let mut general = Btree::open_in_memory().unwrap();
            for bt in [&mut fast, &mut general] {
                bt.pager_mut().set_page_size(page_size).unwrap();
                if auto_vacuum {
                    bt.set_configured_auto_vacuum(AutoVacuumMode::Full);
                }
                bt.begin_write().unwrap();
            }
            let root = fast.create_table().unwrap();
            assert_eq!(root, general.create_table().unwrap());
            assert_eq!(fast.is_auto_vacuum(), auto_vacuum);

            let mut seed = 0xD1B5_4A32_D192_ED03u64 ^ u64::from(page_size) ^ (n_rows as u64);
            // Sequential load packs the leaves full, like a bulk INSERT.
            for rowid in 1..=n_rows {
                let rec = payload(next(&mut seed) | 4, page_size);
                fast.insert_table(root, rowid, &rec).unwrap();
                general.insert_table(root, rowid, &rec).unwrap();
            }

            let mut positioned_hits = 0u32;
            for op in 0..n_ops {
                let r = next(&mut seed);
                let rowid = 1 + ((r >> 16) % (n_rows as u64 + 20)) as i64;
                let position = {
                    let cur = fast.open_table_cursor(root, true);
                    let pos = if fast.cursor_seek_rowid(cur, rowid).unwrap() {
                        fast.table_cursor_position(cur, root)
                    } else {
                        None
                    };
                    fast.close_cursor(cur);
                    pos
                };
                // A wrong position must be harmless: another cell of the leaf, another leaf of
                // the tree, the root. (A page that is not part of the tree is out of contract:
                // as in C, a page freed by a merge keeps the row images it held.)
                let position = match (position, r % 97) {
                    (Some((pg, idx)), 0) => Some((pg, idx + 1)),
                    (Some((_, idx)), 1) => {
                        let other = 1 + (rowid + 37) % n_rows;
                        let cur = fast.open_table_cursor(root, true);
                        let pos = if fast.cursor_seek_rowid(cur, other).unwrap() {
                            fast.table_cursor_position(cur, root)
                                .map(|(pg, _)| (pg, idx))
                        } else {
                            None
                        };
                        fast.close_cursor(cur);
                        pos
                    }
                    (None, 2) => Some((root, 0)),
                    (p, _) => p,
                };
                positioned_hits += u32::from(position.is_some());
                if r.is_multiple_of(4) {
                    let a = fast.delete_table_at(root, rowid, position).unwrap();
                    let b = general.delete_table(root, rowid).unwrap();
                    assert_eq!(a, b, "delete outcome differs at op {op}");
                } else {
                    let rec = payload(next(&mut seed), page_size);
                    fast.insert_table_at(root, rowid, &rec, position).unwrap();
                    general.insert_table(root, rowid, &rec).unwrap();
                }
                if op % 257 == 0 || op + 1 == n_ops {
                    let n_pages = fast.pager().page_count();
                    assert_eq!(n_pages, general.pager().page_count());
                    let lock_pg = fast.lock_byte_page();
                    for pgno in (1..=n_pages).filter(|&p| p != lock_pg) {
                        let a = fast.pager_mut().read_page(pgno).unwrap().to_vec();
                        let b = general.pager_mut().read_page(pgno).unwrap().to_vec();
                        assert!(
                            a == b,
                            "page {pgno} differs after op {op} (page_size {page_size}, \
                             auto_vacuum {auto_vacuum})"
                        );
                    }
                }
            }
            assert!(positioned_hits > n_ops / 2);
            assert_eq!(
                fast.scan_table(root).unwrap(),
                general.scan_table(root).unwrap()
            );
            assert_eq!(
                fast.integrity_check(&[1, root]).unwrap(),
                Vec::<String>::new()
            );
        }
    }

    #[test]
    fn test_btree_multi_page_splits_overflows_deletes_and_integrity() {
        let mut bt = Btree::open_in_memory().unwrap();
        // Use small 512-byte pages to stress multi-level splits and overflow chains aggressively!
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();

        let tbl_root = bt.create_table().unwrap();
        let idx_root = bt.create_index().unwrap();
        let orders = [KeySortOrder::default(), KeySortOrder::default()];

        // Insert 200 rows including >2KB overflow payloads
        for i in 1..=200i64 {
            let text = if i % 17 == 0 {
                "X".repeat(1500) // spans 3+ overflow pages on 512B pages!
            } else {
                format!("item-{i:04}")
            };
            let rec = encode_record(&[Value::Integer(i), Value::Text(text.clone())]);
            bt.insert_table(tbl_root, i, &rec).unwrap();

            let idx_rec = encode_record(&[Value::Text(text), Value::Integer(i)]);
            bt.insert_index(idx_root, &idx_rec, &orders).unwrap();
        }

        assert!(bt.pager().page_count() > 25);
        assert_eq!(
            bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(),
            Vec::<String>::new()
        );

        // Seek every row & verify overflow reassembly
        for i in 1..=200i64 {
            let raw = bt.seek_table_rowid(tbl_root, i).unwrap().unwrap();
            let vals = decode_record(&raw).unwrap();
            assert_eq!(vals[0], Value::Integer(i));
            if i % 17 == 0 {
                assert_eq!(vals[1].as_str().unwrap().len(), 1500);
            }
        }

        // Point-seek via secondary index
        let hits = bt
            .seek_index_equal(idx_root, &[Value::Text("item-0042".into())], &orders)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0][1], Value::Integer(42));

        // Delete 150 rows (including overflow rows) and verify zero leaked pages via integrity_check!
        for i in (1..=150i64).rev() {
            let text = if i % 17 == 0 {
                "X".repeat(1500)
            } else {
                format!("item-{i:04}")
            };
            assert!(bt.delete_table(tbl_root, i).unwrap());
            assert!(
                bt.delete_index(idx_root, &[Value::Text(text), Value::Integer(i)], &orders)
                    .unwrap()
            );
        }

        assert_eq!(
            bt.integrity_check(&[1, tbl_root, idx_root]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(bt.scan_table(tbl_root).unwrap().len(), 50);
        bt.commit().unwrap();
    }

    /// Height of the subtree below `pgno` (a leaf is 1), or a description of the first
    /// structural defect: leaves at different depths, a non-root page without cells, an
    /// interior root without cells anywhere but on page 1.
    fn tree_height(bt: &mut Btree, pgno: Pgno, is_root: bool) -> std::result::Result<u32, String> {
        let (n_cell, children) = match bt.read_parsed_page(pgno).map_err(|e| e.to_string())? {
            ParsedPage::TableLeaf { cells } => (cells.len(), Vec::new()),
            ParsedPage::IndexLeaf { cells } => (cells.len(), Vec::new()),
            ParsedPage::TableInterior { cells, right_child } => {
                let mut ch: Vec<Pgno> = cells.iter().map(|c| c.left_child).collect();
                ch.push(right_child);
                (cells.len(), ch)
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                let mut ch: Vec<Pgno> = cells.iter().map(|c| c.left_child).collect();
                ch.push(right_child);
                (cells.len(), ch)
            }
        };
        if n_cell == 0 && !is_root {
            return Err(format!("non-root page {pgno} has no cells"));
        }
        if n_cell == 0 && !children.is_empty() && pgno != 1 {
            return Err(format!("interior root {pgno} has no cells"));
        }
        let mut height = 0u32;
        for (i, &child) in children.iter().enumerate() {
            let h = tree_height(bt, child, false)?;
            if i > 0 && h != height {
                return Err(format!(
                    "page {pgno}: child {i} (page {child}) has height {h}, its left neighbours {height}"
                ));
            }
            height = h;
        }
        Ok(height + 1)
    }

    /// Asserts that the tree is balanced and that the C-compatible integrity check agrees.
    fn assert_healthy(bt: &mut Btree, roots: &[Pgno], what: &str) {
        for &root in &roots[1..] {
            if let Err(defect) = tree_height(bt, root, true) {
                panic!("{what}: tree {root} is malformed: {defect}");
            }
        }
        assert_eq!(
            bt.integrity_check(roots).unwrap(),
            Vec::<String>::new(),
            "{what}"
        );
    }

    /// A record-shaped payload of exactly `len >= 2` bytes (one BLOB column).
    fn blob_record(len: usize, fill: u8) -> Vec<u8> {
        let mut body = len - 2;
        loop {
            let rec = encode_record(&[Value::Blob(vec![fill; body])]);
            if rec.len() <= len {
                assert_eq!(rec.len(), len, "no record of {len} bytes");
                return rec;
            }
            body -= 1;
        }
    }

    /// Regression test for the unbalanced-tree defect: deleting the rows below a non-root
    /// interior page until it had a single child left used to free that page and hook the
    /// child directly into the grandparent, leaving its leaves one level above all others
    /// (`Child page depth differs` in C's integrity check, and later `SQLITE_CORRUPT` from
    /// our own insert path).
    #[test]
    fn test_delete_keeps_leaves_at_one_depth_when_interior_page_empties() {
        // Two 200-byte rows per 512-byte leaf, ~60 leaves per interior page.
        const N: i64 = 700;
        let orders: [&dyn Fn(i64) -> i64; 4] = [
            &|i| i + 1, // ascending
            &|i| N - i, // descending
            &|i| {
                if i % 2 == 0 {
                    N / 2 - i / 2
                } else {
                    N / 2 + 1 + i / 2
                }
            }, // middle out
            &|i| (i * 311) % N + 1, // scattered (311 is coprime to 700)
        ];
        for (which, order) in orders.iter().enumerate() {
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(512).unwrap();
            bt.begin_write().unwrap();
            let root = bt.create_table().unwrap();
            for rowid in 1..=N {
                bt.insert_table(root, rowid, &blob_record(200, rowid as u8))
                    .unwrap();
            }
            assert_eq!(
                tree_height(&mut bt, root, true),
                Ok(3),
                "the scenario needs interior pages below the root"
            );
            assert_healthy(&mut bt, &[1, root], "after load");

            let mut left = N;
            for i in 0..N {
                let rowid = order(i);
                assert!(bt.delete_table(root, rowid).unwrap(), "rowid {rowid}");
                left -= 1;
                assert_healthy(
                    &mut bt,
                    &[1, root],
                    &format!("order {which}: after deleting rowid {rowid} ({left} rows left)"),
                );
                if i % 97 == 0 {
                    assert_eq!(bt.count_entries(root).unwrap(), left);
                }
            }
            assert_eq!(bt.scan_table(root).unwrap(), Vec::new());
            // The emptied tree is a single leaf again and can be refilled.
            assert_eq!(tree_height(&mut bt, root, true), Ok(1));
            for rowid in 1..=N {
                bt.insert_table(root, rowid, &blob_record(200, rowid as u8))
                    .unwrap();
            }
            assert_healthy(&mut bt, &[1, root], "after refill");
            bt.commit().unwrap();
        }
    }

    /// A rebalance at the interior level picks new dividers. With negative rowids (nine-byte
    /// varints) next to small positive ones the parent can grow while a row is *deleted*, and
    /// must then split like it does on insert.
    #[test]
    fn test_delete_rebalance_with_mixed_width_keys_keeps_tree_valid() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for page_size in [512u32, 1024] {
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(page_size).unwrap();
            bt.begin_write().unwrap();
            let root = bt.create_table().unwrap();
            let big = (page_size - 60) as usize;
            let mut live: Vec<i64> = Vec::new();
            for round in 0..6 {
                // One row per leaf, keys of every varint width on both sides of zero.
                for _ in 0..1500 {
                    let r = next();
                    let magnitude = (r >> 8) % (1u64 << (7 * (1 + (r % 8) as u32)));
                    let rowid = if r & 0x80 == 0 {
                        magnitude as i64
                    } else {
                        -(magnitude as i64) - 1
                    };
                    bt.insert_table(root, rowid, &blob_record(big, r as u8))
                        .unwrap();
                    live.push(rowid);
                }
                live.sort_unstable();
                live.dedup();
                assert_healthy(&mut bt, &[1, root], "after inserts");
                // Remove long runs of neighbouring keys so that whole interior pages drain.
                let keep_every = 37 + round;
                let mut kept = Vec::new();
                for (pos, &rowid) in live.iter().enumerate() {
                    if pos % keep_every == 0 {
                        kept.push(rowid);
                        continue;
                    }
                    assert!(bt.delete_table(root, rowid).unwrap());
                    if pos % 5 == 0 {
                        assert_healthy(&mut bt, &[1, root], "during deletes");
                    }
                }
                live = kept;
                assert_healthy(&mut bt, &[1, root], "after deletes");
                let rows = bt.scan_table(root).unwrap();
                assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), live);
            }
            bt.commit().unwrap();
        }
    }

    /// A leaf page holding the single row `rowid`.
    fn one_row_leaf(bt: &mut Btree, rowid: i64) -> Pgno {
        let pgno = bt.allocate_page().unwrap();
        let local_payload = encode_record(&[Value::Integer(rowid)]);
        bt.write_parsed_page(
            pgno,
            &ParsedPage::TableLeaf {
                cells: vec![TableLeafCell {
                    rowid,
                    total_payload_len: local_payload.len() as u32,
                    local_payload,
                    overflow_page: None,
                }],
            },
        )
        .unwrap();
        pgno
    }

    /// An interior page over the given children, with `keys[i]` as the divider after child `i`.
    fn interior_over(bt: &mut Btree, children: &[Pgno], keys: &[i64]) -> Pgno {
        assert_eq!(children.len(), keys.len() + 1);
        let pgno = bt.allocate_page().unwrap();
        let page = ParsedPage::TableInterior {
            cells: keys
                .iter()
                .zip(children)
                .map(|(&key, &left_child)| TableInteriorCell { left_child, key })
                .collect(),
            right_child: *children.last().unwrap(),
        };
        assert!(page.fits_in_page(pgno, 512));
        bt.write_parsed_page(pgno, &page).unwrap();
        pgno
    }

    /// An interior page over one single-row leaf per rowid.
    fn interior_over_rows(bt: &mut Btree, rowids: &[i64]) -> Pgno {
        let leaves: Vec<Pgno> = rowids.iter().map(|&r| one_row_leaf(bt, r)).collect();
        interior_over(bt, &leaves, &rowids[..rowids.len() - 1])
    }

    /// Deleting a row can make an interior page *grow*: when three interior siblings are
    /// rebalanced, the dividers that go back up are different keys, and a larger key can need a
    /// longer varint. Here the parent is full to the last byte, so the new divider does not fit:
    /// it becomes an overflow cell and `balance()` continues one level up, where
    /// `balance_nonroot` redistributes the parent with its sibling - during a DELETE. (Step 5b
    /// split the parent in two and gave the root a second divider; C does not allocate a page
    /// while the sibling has room.)
    #[test]
    fn test_delete_rebalance_that_overflows_the_parent_rebalances_it_with_its_sibling() {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let root = bt.create_table().unwrap();

        // Three neighbours below `g1`: `s1` full of one-byte keys, `p` with two leaves, `s2`
        // full of two-byte keys. Their dividers in `g1` (72 and 120) are one-byte keys.
        let s1 = interior_over_rows(&mut bt, &(1..=72).collect::<Vec<i64>>());
        let p = interior_over_rows(&mut bt, &[100, 110]);
        let s2 = interior_over_rows(&mut bt, &(200..=262).collect::<Vec<i64>>());
        let mut children = vec![s1, p, s2];
        let mut dividers = vec![72i64, 120, 300];
        let mut n_rows = 72 + 2 + 63;
        // 53 more children behind two-byte dividers and 6 behind three-byte ones fill `g1`
        // to exactly 500 of its 500 bytes.
        for j in 0..53 {
            let base = 400 + 10 * j;
            children.push(interior_over_rows(&mut bt, &[base + 1, base + 2]));
            dividers.push(base + 5);
        }
        for j in 0..6 {
            let base = 20_000 + 10 * j;
            children.push(interior_over_rows(&mut bt, &[base + 1, base + 2]));
            dividers.push(base + 5);
        }
        children.push(interior_over_rows(&mut bt, &[30_001, 30_002]));
        n_rows += 2 * (53 + 6 + 1);
        let g1 = interior_over(&mut bt, &children, &dividers);
        let g1_page = bt.read_parsed_page(g1).unwrap();
        assert_eq!(g1_page.required_bytes(0), 512, "g1 must be exactly full");

        let g2_a = interior_over_rows(&mut bt, &[40_001, 40_002]);
        let g2_b = interior_over_rows(&mut bt, &[40_011, 40_012]);
        let g2 = interior_over(&mut bt, &[g2_a, g2_b], &[40_005]);
        n_rows += 4;
        bt.write_parsed_page(
            root,
            &ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: g1,
                    key: 30_010,
                }],
                right_child: g2,
            },
        )
        .unwrap();
        assert_eq!(tree_height(&mut bt, root, true), Ok(4));
        assert_healthy(&mut bt, &[1, root], "hand-built tree");
        assert_eq!(bt.count_entries(root).unwrap(), n_rows);

        // Emptying one leaf of `p` merges its two leaves and leaves `p` with a single child:
        // `s1`, `p` and `s2` are rebalanced into three pages again, one of the new dividers is
        // a two-byte key where a one-byte key stood, and `g1` no longer fits. `g1` and `g2`
        // are then rebalanced under the root, which keeps its single divider.
        let pages_before = bt.pager().page_count();
        let free_before = bt.pager().header().total_freelist_pages;
        assert!(bt.delete_table(root, 100).unwrap());
        assert_healthy(&mut bt, &[1, root], "after the delete");
        assert_eq!(tree_height(&mut bt, root, true), Ok(4));
        assert_eq!(bt.count_entries(root).unwrap(), n_rows - 1);
        let ParsedPage::TableInterior { cells, right_child } = bt.read_parsed_page(root).unwrap()
        else {
            panic!("root is no longer an interior page");
        };
        assert_eq!(cells.len(), 1, "g1 and g2 share the cells; no new page");
        assert_eq!((cells[0].left_child, right_child), (g1, g2));
        assert_ne!(cells[0].key, 30_010, "the divider between g1 and g2 moved");
        let ParsedPage::TableInterior {
            cells: g2_cells, ..
        } = bt.read_parsed_page(g2).unwrap()
        else {
            panic!("g2 is no longer an interior page");
        };
        assert!(
            g2_cells.len() > 1,
            "g2 took cells over from the overfull g1"
        );
        // The only page given up is the emptied leaf.
        assert_eq!(bt.pager().page_count(), pages_before);
        assert_eq!(bt.pager().header().total_freelist_pages, free_before + 1);
        let rowids: Vec<i64> = bt.scan_table(root).unwrap().iter().map(|r| r.0).collect();
        assert!(rowids.is_sorted());
        assert!(!rowids.contains(&100) && rowids.contains(&110) && rowids.contains(&72));
        bt.rollback().unwrap();
    }

    /// A root on page 1 may consist of nothing but a pointer to one child. When that child is
    /// an interior page that is left with a single child itself, `balance_nonroot` runs on it
    /// alone (the "virtual root" case) and copies it into page 1 (balance-shallower): the level
    /// disappears, for every leaf alike. As in C the root is then an interior page without
    /// cells over the one remaining leaf; `balance()` stops at the root. (Step 5b folded the
    /// leaf into page 1 as well.)
    #[test]
    fn test_delete_drops_a_level_below_a_single_child_root() {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        // Make sure page 1 exists as a table root before it is rewritten by hand.
        bt.insert_table(1, 1, &blob_record(40, 1)).unwrap();
        let child = interior_over_rows(&mut bt, &[1, 2]);
        bt.write_parsed_page(
            1,
            &ParsedPage::TableInterior {
                cells: Vec::new(),
                right_child: child,
            },
        )
        .unwrap();
        assert_eq!(tree_height(&mut bt, 1, true), Ok(3));
        assert_eq!(bt.integrity_check(&[1]).unwrap(), Vec::<String>::new());

        assert!(bt.delete_table(1, 1).unwrap());
        // The two leaves became one; the interior page between it and the root is gone.
        assert_eq!(tree_height(&mut bt, 1, true), Ok(2));
        assert_eq!(bt.integrity_check(&[1]).unwrap(), Vec::<String>::new());
        let ParsedPage::TableInterior { cells, right_child } = bt.read_parsed_page(1).unwrap()
        else {
            panic!("page 1 must still be an interior page");
        };
        assert!(cells.is_empty());
        assert_ne!(right_child, child);
        assert_eq!(bt.pager().header().total_freelist_pages, 2);
        let rows = bt.scan_table(1).unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), [2]);

        // Emptying that leaf lets `balance_nonroot` copy it into the root: a leaf again.
        assert!(bt.delete_table(1, 2).unwrap());
        assert_eq!(tree_height(&mut bt, 1, true), Ok(1));
        assert_eq!(bt.integrity_check(&[1]).unwrap(), Vec::<String>::new());
        assert!(bt.scan_table(1).unwrap().is_empty());
        assert_eq!(bt.pager().header().total_freelist_pages, 3);
        bt.rollback().unwrap();
    }

    /// The balancer works on raw page bytes. Whatever a damaged page says - cell pointers,
    /// cell counts, freeblock chains, child pointers - a delete must end with a result or an
    /// error, never with a panic (and the transaction can be rolled back afterwards).
    #[test]
    fn test_delete_rebalance_on_damaged_pages_never_panics() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        let mut errors = 0u32;
        for round in 0..400u64 {
            let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ (round * 0x0100_0000_01B3);
            let page_size = [512u32, 1024, 4096][(round % 3) as usize];
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(page_size).unwrap();
            if round % 5 == 0 {
                bt.set_configured_auto_vacuum(AutoVacuumMode::Full);
            }
            bt.begin_write().unwrap();
            let tbl = bt.create_table().unwrap();
            let idx = bt.create_index().unwrap();
            let n_rows = 150 + (next(&mut seed) % 250) as i64;
            let key = |i: i64| -> Vec<u8> {
                let width = 4 + ((i as usize * 31) % (page_size as usize / 8));
                encode_record(&[Value::Text(format!("{i:0width$}")), Value::Integer(i)])
            };
            for i in 1..=n_rows {
                let len = 10 + (i as usize * 7919) % (page_size as usize / 3);
                let row = encode_record(&[Value::Blob(vec![i as u8; len])]);
                bt.insert_table(tbl, i, &row).unwrap();
                bt.insert_index(idx, &key(i), &orders).unwrap();
            }
            bt.commit().unwrap();

            bt.begin_write().unwrap();
            // Damage a few pages: header fields, cell pointers, or arbitrary bytes.
            let n_pages = bt.pager().page_count();
            for _ in 0..1 + next(&mut seed) % 3 {
                let pgno = 2 + (next(&mut seed) % u64::from(n_pages - 1)) as Pgno;
                if pgno == bt.lock_byte_page() {
                    continue;
                }
                let r = next(&mut seed);
                let page = bt.pager_mut().write_page(pgno).unwrap();
                let at = match r % 4 {
                    0 => (r >> 8) as usize % 12,
                    1 => 8 + (r >> 8) as usize % 64,
                    _ => (r >> 8) as usize % page.len(),
                };
                page[at] = match (r >> 40) % 3 {
                    0 => 0,
                    1 => 0xff,
                    _ => (r >> 48) as u8,
                };
            }
            for _ in 0..120 {
                let i = 1 + (next(&mut seed) % n_rows as u64) as i64;
                // Each call may succeed, report "not found" or fail with an error.
                errors += u32::from(bt.delete_table(tbl, i).is_err());
                errors += u32::from(bt.delete_index_raw(idx, &key(i), &orders).is_err());
            }
            bt.rollback().unwrap();
            // The rollback brings the undamaged tree back.
            assert_eq!(bt.count_entries(tbl).unwrap(), n_rows, "round {round}");
            assert_eq!(
                bt.integrity_check(&[1, tbl, idx]).unwrap(),
                Vec::<String>::new()
            );
        }
        assert!(errors > 0, "no damaged page was ever noticed");
    }

    /// The integrity check must flag a tree whose leaves are not all at the same depth, with
    /// the message C SQLite 3.54.1 prints for the same image.
    #[test]
    fn test_integrity_check_reports_child_page_depth_differs() {
        let mut bt = Btree::open_in_memory().unwrap();
        bt.pager_mut().set_page_size(512).unwrap();
        bt.begin_write().unwrap();
        let root = bt.create_table().unwrap();
        let leaf = |range: std::ops::RangeInclusive<i64>| ParsedPage::TableLeaf {
            cells: range
                .map(|rowid| {
                    let local_payload = encode_record(&[Value::Integer(rowid)]);
                    TableLeafCell {
                        rowid,
                        total_payload_len: local_payload.len() as u32,
                        local_payload,
                        overflow_page: None,
                    }
                })
                .collect(),
        };
        // root -> [ leaf a | 10 | interior b -> [ leaf c | 20 | leaf d ] ]
        let a = bt.allocate_page().unwrap();
        let b = bt.allocate_page().unwrap();
        let c = bt.allocate_page().unwrap();
        let d = bt.allocate_page().unwrap();
        bt.write_parsed_page(a, &leaf(1..=10)).unwrap();
        bt.write_parsed_page(c, &leaf(11..=20)).unwrap();
        bt.write_parsed_page(d, &leaf(21..=30)).unwrap();
        bt.write_parsed_page(
            b,
            &ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: c,
                    key: 20,
                }],
                right_child: d,
            },
        )
        .unwrap();
        bt.write_parsed_page(
            root,
            &ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: a,
                    key: 10,
                }],
                right_child: b,
            },
        )
        .unwrap();

        assert!(tree_height(&mut bt, root, true).is_err());
        // Every row is still reachable; only the shape is wrong.
        assert_eq!(bt.count_entries(root).unwrap(), 30);
        assert_eq!(
            bt.integrity_check(&[1, root]).unwrap(),
            vec![format!(
                "Tree {root} page {root} cell 0: Child page depth differs"
            )]
        );
        let report = bt.integrity_check_report(&[1, root], false, 100).unwrap();
        assert_eq!(report.row_counts, vec![0, 30]);

        // Balanced again once `a` hangs below an interior page of its own.
        let a_parent = bt.allocate_page().unwrap();
        let a2 = bt.allocate_page().unwrap();
        bt.write_parsed_page(a, &leaf(1..=5)).unwrap();
        bt.write_parsed_page(a2, &leaf(6..=10)).unwrap();
        bt.write_parsed_page(
            a_parent,
            &ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: a,
                    key: 5,
                }],
                right_child: a2,
            },
        )
        .unwrap();
        bt.write_parsed_page(
            root,
            &ParsedPage::TableInterior {
                cells: vec![TableInteriorCell {
                    left_child: a_parent,
                    key: 10,
                }],
                right_child: b,
            },
        )
        .unwrap();
        assert_healthy(&mut bt, &[1, root], "rebalanced by hand");
        let report = bt.integrity_check_report(&[1, root], false, 100).unwrap();
        assert_eq!(report.unwalkable, [false, false]);

        // A well-formed but empty page below the root draws no b-tree level message (as in C),
        // yet no cursor of C SQLite enters it: the report marks the tree.
        bt.write_parsed_page(a, &ParsedPage::TableLeaf { cells: Vec::new() })
            .unwrap();
        let report = bt.integrity_check_report(&[1, root], false, 100).unwrap();
        assert_eq!(report.messages, Vec::<String>::new());
        assert_eq!(report.unwalkable, [false, true]);
        assert_eq!(report.row_counts, [0, 25]);
        bt.rollback().unwrap();
    }

    /// Mixed insert / update / delete workloads with overflow rows on small and default pages,
    /// with and without auto-vacuum, on a table tree and an index tree: after every batch all
    /// leaves are at one depth, the integrity check is clean and the content matches a model.
    #[test]
    fn test_random_workloads_keep_trees_balanced_and_intact() {
        use std::collections::BTreeMap;

        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }
        fn row_len(r: u64, page_size: u32) -> usize {
            let p = page_size as usize;
            match r % 16 {
                0 => p * 3,                        // long overflow chain
                1 => p + 17,                       // one overflow page
                2 | 3 => p - 60,                   // one row per leaf
                4..=6 => p / 3,                    // two or three rows per leaf
                _ => 8 + ((r >> 8) % 40) as usize, // many rows per leaf
            }
        }

        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        for (page_size, auto_vacuum, key_space, batches) in [
            (512u32, false, 3_000u64, 60u32),
            (512, true, 1_200, 40),
            (1024, false, 4_000, 50),
            (1024, true, 2_000, 30),
            (4096, false, 9_000, 40),
        ] {
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(page_size).unwrap();
            if auto_vacuum {
                bt.set_configured_auto_vacuum(AutoVacuumMode::Full);
            }
            bt.begin_write().unwrap();
            let tbl = bt.create_table().unwrap();
            let idx = bt.create_index().unwrap();
            let roots = [1, tbl, idx];
            let mut seed = 0xA076_1D64_78BD_642Fu64 ^ u64::from(page_size) ^ u64::from(auto_vacuum);
            // rowid -> (payload length, fill byte); the index holds (text(rowid, len), rowid).
            let mut model: BTreeMap<i64, (usize, u8)> = BTreeMap::new();
            let idx_key = |rowid: i64, len: usize| {
                [
                    Value::Text(format!("{:05}-{}", rowid % 977, "k".repeat(len % 150))),
                    Value::Integer(rowid),
                ]
            };

            for batch in 0..batches {
                // Phases: grow, churn, drain almost everything, grow again, ...
                let delete_per_16 = match batch % 6 {
                    0 | 1 => 2,
                    2 | 3 => 8,
                    _ => 14,
                };
                let what =
                    format!("page_size {page_size}, auto_vacuum {auto_vacuum}, batch {batch}");
                for _ in 0..400 {
                    let r = next(&mut seed);
                    let rowid = ((r >> 20) % key_space) as i64 - (key_space / 8) as i64;
                    if (r >> 4) % 16 < delete_per_16 {
                        // Delete `rowid` or, if absent, its successor: runs of neighbours go.
                        let victim = model.range(rowid..).next().map(|(&k, &v)| (k, v));
                        if let Some((k, (len, _))) = victim {
                            assert!(bt.delete_table(tbl, k).unwrap(), "{what}");
                            assert!(
                                bt.delete_index(idx, &idx_key(k, len), &orders).unwrap(),
                                "{what}"
                            );
                            model.remove(&k);
                        } else {
                            assert!(!bt.delete_table(tbl, rowid).unwrap(), "{what}");
                        }
                    } else {
                        let len = row_len(next(&mut seed), page_size);
                        let fill = (r >> 12) as u8;
                        if let Some((old_len, _)) = model.insert(rowid, (len, fill)) {
                            assert!(
                                bt.delete_index(idx, &idx_key(rowid, old_len), &orders)
                                    .unwrap(),
                                "{what}"
                            );
                        }
                        bt.insert_table(tbl, rowid, &blob_record(len, fill))
                            .unwrap();
                        bt.insert_index(idx, &encode_record(&idx_key(rowid, len)), &orders)
                            .unwrap();
                    }
                }
                assert_healthy(&mut bt, &roots, &what);
                let report = bt.integrity_check_report(&roots, false, 100).unwrap();
                assert_eq!(
                    report.row_counts[1..],
                    [model.len() as i64, model.len() as i64],
                    "{what}"
                );
                if batch % 5 == 4 || batch + 1 == batches {
                    let rows = bt.scan_table(tbl).unwrap();
                    assert_eq!(rows.len(), model.len(), "{what}");
                    for ((rowid, payload), (&m_rowid, &(len, fill))) in rows.iter().zip(&model) {
                        assert_eq!(*rowid, m_rowid, "{what}");
                        assert!(*payload == blob_record(len, fill), "{what}: rowid {rowid}");
                    }
                    assert_eq!(bt.scan_index(idx).unwrap().len(), model.len(), "{what}");
                }
            }
            // Drain completely: both trees end as empty root leaves.
            let keys: Vec<(i64, usize)> = model.iter().map(|(&k, &(len, _))| (k, len)).collect();
            for (n, (k, len)) in keys.into_iter().enumerate() {
                assert!(bt.delete_table(tbl, k).unwrap());
                assert!(bt.delete_index(idx, &idx_key(k, len), &orders).unwrap());
                if n % 64 == 0 {
                    assert_healthy(&mut bt, &roots, "drain");
                }
            }
            assert_healthy(&mut bt, &roots, "drained");
            assert_eq!(tree_height(&mut bt, tbl, true), Ok(1));
            assert_eq!(tree_height(&mut bt, idx, true), Ok(1));
            bt.commit().unwrap();
        }
    }

    /// `PRAGMA integrity_check` asks whether the index holds the entry of a row. A damaged index
    /// record with fewer fields than the key compares equal on the fields it has; it must not
    /// count as that entry (C reports the row as missing from the index).
    #[test]
    fn test_index_lookup_rejects_record_with_too_few_fields() {
        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        let mut bt = Btree::open_in_memory().unwrap();
        bt.begin_write().unwrap();
        let idx = bt.create_index().unwrap();
        // A record with a single field where every entry should have two.
        bt.insert_index(idx, &encode_record(&[Value::Integer(7)]), &orders)
            .unwrap();
        let key = [Value::Integer(7), Value::Integer(1)];
        assert!(!bt.index_contains_prefix_raw(idx, &key, &orders).unwrap());
        assert!(
            bt.index_contains_prefix_raw(idx, &key[..1], &orders)
                .unwrap()
        );
        // Text keys take the general comparison instead of the integer probe.
        let idx2 = bt.create_index().unwrap();
        bt.insert_index(idx2, &encode_record(&[Value::Text("k".into())]), &orders)
            .unwrap();
        let key = [Value::Text("k".into()), Value::Integer(1)];
        assert!(!bt.index_contains_prefix_raw(idx2, &key, &orders).unwrap());
        // The complete record is the entry.
        bt.insert_index(idx2, &encode_record(&key), &orders)
            .unwrap();
        assert!(bt.index_contains_prefix_raw(idx2, &key, &orders).unwrap());
        bt.rollback().unwrap();
    }

    /// Index cells range from a few bytes to a quarter of a page. Splitting an overfull page at
    /// the middle cell *by count* could leave one half larger than a page and made the INSERT
    /// fail with "cells exceed usable B-tree page capacity".
    #[test]
    fn test_index_page_split_with_mixed_cell_sizes() {
        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        // Root leaf: thirty small keys, then keys that are stored with (almost) the maximum
        // local payload and sort after them.
        let mut bt = Btree::open_in_memory().unwrap();
        bt.begin_write().unwrap();
        let idx = bt.create_index().unwrap();
        let mut n = 0i64;
        for i in 0..30 {
            let rec = encode_record(&[Value::Text(format!("a{i:02}")), Value::Integer(i)]);
            bt.insert_index(idx, &rec, &orders).unwrap();
            n += 1;
        }
        for i in 0..6 {
            let rec = encode_record(&[
                Value::Text(format!("z{i}{}", "z".repeat(990))),
                Value::Integer(100 + i),
            ]);
            bt.insert_index(idx, &rec, &orders).unwrap();
            n += 1;
            assert_healthy(&mut bt, &[1, idx], "root leaf split");
        }
        assert_eq!(bt.scan_index(idx).unwrap().len() as i64, n);
        bt.commit().unwrap();

        // Interior root: a few dozen small dividers (one per leaf of small keys), then large
        // dividers to their right until the root itself has to split.
        let mut bt = Btree::open_in_memory().unwrap();
        bt.begin_write().unwrap();
        let idx = bt.create_index().unwrap();
        for i in 0..8_000 {
            let rec = encode_record(&[Value::Text(format!("a{i:06}")), Value::Integer(i)]);
            bt.insert_index(idx, &rec, &orders).unwrap();
        }
        assert_eq!(tree_height(&mut bt, idx, true), Ok(2));
        for i in 0..60 {
            let rec = encode_record(&[
                Value::Text(format!("z{i:02}{}", "z".repeat(985))),
                Value::Integer(i),
            ]);
            bt.insert_index(idx, &rec, &orders).unwrap();
        }
        assert_healthy(&mut bt, &[1, idx], "interior root split");
        assert_eq!(tree_height(&mut bt, idx, true), Ok(3));
        assert_eq!(bt.scan_index(idx).unwrap().len(), 8_060);
        bt.commit().unwrap();

        // Random mixes on every page size, inserted in random key order so that leaf, interior
        // and root pages all split with small and large cells side by side.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for page_size in [512u32, 1024, 4096] {
            let mut bt = Btree::open_in_memory().unwrap();
            bt.pager_mut().set_page_size(page_size).unwrap();
            bt.begin_write().unwrap();
            let idx = bt.create_index().unwrap();
            let mut keys = std::collections::BTreeSet::new();
            for round in 0..40 {
                for _ in 0..60 {
                    let r = next();
                    let len = match r % 8 {
                        0 | 1 => (page_size as usize) / 4 + (r >> 8) as usize % 40, // around max local
                        2 => page_size as usize,                                    // overflows
                        _ => (r >> 8) as usize % 12,
                    };
                    // Large keys sort after the small ones, so pages hold runs of each kind.
                    let key = format!(
                        "{}{:06}{}",
                        if len > 12 && round % 2 == 0 { 'z' } else { 'a' },
                        (r >> 24) % 1_000_000,
                        "k".repeat(len)
                    );
                    let rec = encode_record(&[Value::Text(key.clone()), Value::Integer(1)]);
                    bt.insert_index(idx, &rec, &orders).unwrap();
                    keys.insert(key);
                }
                assert_healthy(
                    &mut bt,
                    &[1, idx],
                    &format!("page_size {page_size}, round {round}"),
                );
            }
            let stored: Vec<String> = bt
                .scan_index(idx)
                .unwrap()
                .iter()
                .map(|rec| decode_record(rec).unwrap()[0].as_str().unwrap().to_owned())
                .collect();
            assert!(stored.iter().eq(keys.iter()), "page_size {page_size}");
            bt.commit().unwrap();
        }
    }
    /// `PRAGMA secure_delete` (`BTS_SECURE_DELETE` / `BTS_OVERWRITE`): with the mode off the
    /// payload of deleted and replaced cells stays readable in the page image; with `ON` (1) or
    /// `FAST` (2) no byte of it may survive anywhere in the database — cells freed inside a leaf
    /// (`freeSpace`), the last cell of a root leaf (`dropCell`), replaced cells of another size,
    /// index entries, overflow chains, cleared and dropped trees. The mode must not change the
    /// tree structure: the image differs from the mode-0 image only where that one keeps stale
    /// bytes.
    #[test]
    fn test_secure_delete_overwrites_deleted_content() {
        const MARK: &[u8] = b"DELME";
        fn count_marker(bt: &mut Btree) -> usize {
            let n = bt.pager().page_count();
            let mut hits = 0;
            for pgno in 1..=n {
                let raw = bt.pager_mut().read_page(pgno).unwrap();
                hits += raw.windows(MARK.len()).filter(|w| *w == MARK).count();
            }
            hits
        }
        /// Markers outside the unallocated gap of b-tree pages (the gap is the one place where
        /// C keeps stale cell copies even with `secure_delete=ON`: `rebuildPage` — a page
        /// split — does not clear it, see the note at the assertion below).
        fn count_marker_outside_gaps(bt: &mut Btree) -> usize {
            let n = bt.pager().page_count();
            let mut hits = 0;
            for pgno in 1..=n {
                let raw = bt.pager_mut().read_page(pgno).unwrap();
                let hdr = if pgno == 1 { 100 } else { 0 };
                let gap = match raw[hdr] {
                    0x02 | 0x05 | 0x0a | 0x0d => {
                        let n_cell = usize::from(u16::from_be_bytes([raw[hdr + 3], raw[hdr + 4]]));
                        let child = if raw[hdr] & 0x08 == 0 { 4 } else { 0 };
                        let top = usize::from(u16::from_be_bytes([raw[hdr + 5], raw[hdr + 6]]));
                        (hdr + 8 + child + 2 * n_cell)..top
                    }
                    _ => 0..0,
                };
                hits += raw
                    .windows(MARK.len())
                    .enumerate()
                    .filter(|(i, w)| *w == MARK && !(gap.start <= *i && *i + MARK.len() <= gap.end))
                    .count();
            }
            hits
        }
        fn image(bt: &mut Btree) -> Vec<Vec<u8>> {
            let n = bt.pager().page_count();
            (1..=n)
                .map(|pgno| bt.pager_mut().read_page(pgno).unwrap().to_vec())
                .collect()
        }
        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        let text = |i: i64, doomed: bool| {
            format!(
                "{}-{i:05}-{}",
                if doomed { "DELME" } else { "KEEPME" },
                "p".repeat(10 + (i as usize * 7) % 50)
            )
        };

        let mut images: Vec<Vec<Vec<u8>>> = Vec::new();
        let mut residue = Vec::new();
        for mode in [0u8, 1, 2] {
            for page_size in [512u32, 4096] {
                let mut bt = Btree::open_in_memory().unwrap();
                bt.pager_mut().set_page_size(page_size).unwrap();
                assert_eq!(
                    bt.secure_delete(None),
                    0,
                    "default is off, as in the C build"
                );
                assert_eq!(bt.secure_delete(Some(mode)), mode);
                bt.begin_write().unwrap();
                let tbl = bt.create_table().unwrap();
                let idx = bt.create_index().unwrap();
                let one = bt.create_table().unwrap();
                let one_idx = bt.create_index().unwrap();
                let big = bt.create_table().unwrap();
                let gone = bt.create_table().unwrap();
                let cleared = bt.create_table().unwrap();
                let n = 600i64;
                for i in 1..=n {
                    let doomed = i % 3 == 0;
                    let t = text(i, doomed);
                    bt.insert_table(tbl, i, &encode_record(&[Value::Text(t.clone())]))
                        .unwrap();
                    bt.insert_index(
                        idx,
                        &encode_record(&[Value::Text(t), Value::Integer(i)]),
                        &orders,
                    )
                    .unwrap();
                }
                // Root leaves that lose their only cell.
                bt.insert_table(one, 1, &encode_record(&[Value::Text(text(1, true))]))
                    .unwrap();
                bt.insert_index(
                    one_idx,
                    &encode_record(&[Value::Text(text(1, true)), Value::Integer(1)]),
                    &orders,
                )
                .unwrap();
                // Rows on overflow chains, and whole trees that are dropped / cleared.
                for i in 1..=8i64 {
                    let wide = "DELME".repeat(page_size as usize);
                    bt.insert_table(big, i, &encode_record(&[Value::Text(wide.clone())]))
                        .unwrap();
                    bt.insert_table(gone, i, &encode_record(&[Value::Text(wide.clone())]))
                        .unwrap();
                    bt.insert_table(cleared, i, &encode_record(&[Value::Text(wide)]))
                        .unwrap();
                }
                bt.commit().unwrap();
                assert!(count_marker(&mut bt) > 0);

                bt.begin_write().unwrap();
                for i in (3..=n).step_by(3) {
                    let t = text(i, true);
                    if i % 2 == 0 {
                        // Replace with a row of another size (shorter or longer).
                        let fresh = format!("KEEPME-{i}-{}", "q".repeat((i as usize * 11) % 90));
                        bt.insert_table(tbl, i, &encode_record(&[Value::Text(fresh)]))
                            .unwrap();
                    } else {
                        assert!(bt.delete_table(tbl, i).unwrap());
                    }
                    assert!(
                        bt.delete_index_raw(
                            idx,
                            &encode_record(&[Value::Text(t), Value::Integer(i)]),
                            &orders
                        )
                        .unwrap()
                    );
                }
                assert!(bt.delete_table(one, 1).unwrap());
                assert!(
                    bt.delete_index_raw(
                        one_idx,
                        &encode_record(&[Value::Text(text(1, true)), Value::Integer(1)]),
                        &orders
                    )
                    .unwrap()
                );
                for i in 1..=8i64 {
                    assert!(bt.delete_table(big, i).unwrap());
                }
                bt.drop_tree(gone).unwrap();
                bt.clear_tree(cleared).unwrap();
                bt.commit().unwrap();

                assert_eq!(
                    bt.integrity_check(&[1, tbl, idx, one, one_idx, big, cleared])
                        .unwrap(),
                    Vec::<String>::new()
                );
                assert_eq!(bt.scan_index(idx).unwrap().len(), 400);
                residue.push((
                    mode,
                    page_size,
                    count_marker(&mut bt),
                    count_marker_outside_gaps(&mut bt),
                ));
                images.push(image(&mut bt));
            }
        }
        let off_hits = |page_size: u32| {
            residue
                .iter()
                .find(|r| r.0 == 0 && r.1 == page_size)
                .map_or(0, |r| r.2)
        };
        for &(mode, page_size, hits, hits_outside_gaps) in &residue {
            match mode {
                0 => assert!(
                    hits > 0,
                    "mode 0, page {page_size}: deleted cells are expected to stay in place"
                ),
                // ON: everything freed is zeroed (`freeSpace`, `freePage2`, `zeroPage`). What C
                // does not clear is the unallocated gap of a page rebuilt by a split
                // (`rebuildPage`): a copy of a cell that moved to a sibling stays there and
                // survives the later delete of the row. Verified against C SQLite 3.54.1 with
                // the same workload in SQL (`PRAGMA secure_delete=ON`, page sizes 512 and
                // 4096): C's file keeps one marker and is byte-identical to this engine's.
                1 => {
                    assert_eq!(
                        hits_outside_gaps, 0,
                        "mode {mode}, page {page_size}: deleted content left outside page gaps"
                    );
                    assert!(
                        hits * 20 < off_hits(page_size),
                        "mode {mode}, page {page_size}: {hits} markers (OFF leaves {})",
                        off_hits(page_size)
                    );
                }
                // FAST (`BTS_OVERWRITE`) zeroes what it frees inside a page it rewrites anyway,
                // but — exactly like C, whose `freePage2` tests `BTS_SECURE_DELETE` only — it does
                // not overwrite a page that goes to the free list.
                _ => assert!(
                    hits > 0 && hits < off_hits(page_size),
                    "mode {mode}, page {page_size}: {hits} markers (OFF leaves {})",
                    off_hits(page_size)
                ),
            }
        }
        // Same structure in every mode: a secure image only differs from the mode-0 image in
        // bytes it has zeroed.
        for secure in 2..images.len() {
            let plain = &images[secure % 2];
            assert_eq!(plain.len(), images[secure].len());
            for (pgno, (a, b)) in plain.iter().zip(&images[secure]).enumerate() {
                for (off, (x, y)) in a.iter().zip(b).enumerate() {
                    assert!(
                        x == y || *y == 0,
                        "image {secure}, page {}, offset {off}: {x:#x} vs {y:#x}",
                        pgno + 1
                    );
                }
            }
        }
    }
}
