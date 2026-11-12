//! `sqliters-btree`: SQLite Format 3 B-Tree & B+Tree storage engine,
//! overflow chains, freelist page management, and `PRAGMA integrity_check`.
#![forbid(unsafe_code)]

pub mod cursor;
pub mod page;
pub mod ptrmap;

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub use sqliters_pager::{AutoVacuumMode, BackupTracker};
use sqliters_pager::{
    JournalMode, LockingMode, Pager, PagerOpenOptions, TransactionState, WalCheckpointMode,
    WalCheckpointResult, create_super_journal_file,
};
use sqliters_types::{
    Bitvec, ErrorCode, IntRangeProbe, KeySortOrder, Pgno, RawField, RawRecordKey, Result,
    SqliteError, TextEncoding, Value, compare_raw_record_range, compare_raw_records_with_encoding,
    compare_records_with_encoding, decode_record_with_encoding, decode_serial_value_with_encoding,
    extended, get_varint, get_varint_i64, parse_raw_record_prefix, push_varint, put_varint,
    serial_type_len, varint_len,
};
use sqliters_vfs::{OpenFlags, Vfs};

pub use cursor::{
    BTCURSOR_MAX_DEPTH, BtreeCursorState, CursorId, CursorKind, CursorPagePos, CursorStack,
    SavedCursorKey, SkipNext,
};
pub use page::{
    BtreePageType, IndexInteriorCell, IndexLeafCell, ParsedPage, PayloadBounds, RawIndexCell,
    RawIndexInteriorStep, RawIndexLeafView, RawLeafDeleteOutcome, RawTableLeafCell,
    TableInteriorCell, TableLeafCell, TableLeafInsertProbe, apply_append_table_interior_rightmost,
    apply_delete_index_leaf_in_place, apply_insert_index_leaf_in_place,
    apply_insert_index_leaf_raw, apply_insert_table_leaf_raw, apply_replace_table_leaf_raw,
    binary_search_table_leaf, binary_search_table_leaf_with_span, defragment_raw_leaf_in_place,
    find_index_interior_child_in_place, find_index_interior_child_raw, find_table_interior_child,
    find_table_interior_child_append_hint, init_single_cell_table_leaf_raw, last_table_leaf_rowid,
    peek_page_type, probe_append_table_interior_rightmost, probe_delete_index_leaf_in_place,
    probe_delete_index_leaf_raw, probe_index_interior_slot_raw, probe_index_interior_step_raw,
    probe_insert_index_leaf_in_place, probe_insert_index_leaf_raw, probe_insert_table_leaf_raw,
    probe_table_leaf_replace_at_raw, raw_cell_offset, raw_page_free_space, raw_page_header,
    read_index_interior_cell, read_index_leaf_cell, read_table_interior_cell, read_table_leaf_cell,
    read_table_leaf_rowid, replace_table_leaf_repack_raw, seek_table_leaf_cell,
    table_interior_right_child, table_leaf_cell_has_rowid, try_delete_table_leaf_in_place,
    try_insert_table_leaf_in_place,
};
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
    Any,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FastTableInsertOutcome {
    Inserted,
    Duplicate,
    Fallback,
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
}

impl Btree {
    pub fn new(pager: Pager) -> Self {
        Self {
            pager,
            cursor_slots: Vec::new(),
            free_cursor_slots: Vec::new(),
            open_cursor_count: 0,
            next_cursor_gen: 1,
            interrupt_flag: None,
        }
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
        self.pager.begin_write()
    }

    #[inline]
    pub fn begin_exclusive(&mut self) -> Result<()> {
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
        self.pager.spill_dirty_pages_to_wal()
    }

    /// Spills dirty pages to `<db>-wal` when in a WAL write transaction and dirty pages exceed `cache_size`.
    pub fn spill_if_over_cache_size(&mut self) -> Result<usize> {
        self.pager.spill_if_over_cache_size()
    }

    /// Returns the configured page cache size (`PRAGMA cache_size`).
    #[inline]
    pub fn cache_size(&self) -> i32 {
        self.pager.cache_size()
    }

    /// Updates the configured page cache size (`PRAGMA cache_size = N`).
    #[inline]
    pub fn set_cache_size(&mut self, size: i32) {
        self.pager.set_cache_size(size);
    }

    // ========================================================================
    // Freelist & Page Allocation (`btreeInt.h:206-215`, `allocateBtreePage`)
    // ========================================================================

    /// Allocates a page either by popping from the freelist or extending the database file.
    #[inline]
    pub fn allocate_page(&mut self) -> Result<Pgno> {
        self.allocate_btree_page(AllocMode::Any)
    }

    /// Allocates a B-tree/overflow page according to `mode` (`allocateBtreePage`,
    /// `btree.c:6545-6835`), automatically allocating any intervening pointer-map page at EOF.
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
            let search_list = match mode {
                AllocMode::Any => false,
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

            loop {
                if n_search > total_free {
                    return Err(SqliteError::corrupt("cycle detected in freelist trunks"));
                }
                validate_free_pg(cur_trunk)?;
                n_search += 1;

                let trunk_bytes = self.pager.read_page(cur_trunk)?.to_vec();
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
                    self.pager.write_page(cur_trunk)?.fill(0);
                    return Ok(cur_trunk);
                }

                let trunk_matches = search_list
                    && match mode {
                        AllocMode::Exact(nb) => cur_trunk == nb,
                        AllocMode::Le(nb) => cur_trunk <= nb,
                        AllocMode::Any => false,
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
                            let nt_buf = self.pager.write_page(new_trunk)?;
                            nt_buf.fill(0);
                            nt_buf[0..4].copy_from_slice(&next_trunk.to_be_bytes());
                            nt_buf[4..8].copy_from_slice(&rem_leaves.to_be_bytes());
                            if rem_bytes_len > 0 {
                                nt_buf[8..8 + rem_bytes_len]
                                    .copy_from_slice(&trunk_bytes[12..12 + rem_bytes_len]);
                            }
                        }
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
                    self.pager.write_page(cur_trunk)?.fill(0);
                    return Ok(cur_trunk);
                }

                if k > 0 {
                    let chosen_idx: Option<usize> = if !search_list {
                        match mode {
                            AllocMode::Exact(nearby) => {
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
                            _ => Some((k - 1) as usize),
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
                                AllocMode::Any => true,
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
                            w[last_off..last_off + 4].fill(0);
                            w[4..8].copy_from_slice(&new_k.to_be_bytes());
                        }
                        {
                            let hdr = self.pager.header_mut()?;
                            hdr.total_freelist_pages = hdr.total_freelist_pages.saturating_sub(1);
                        }
                        self.pager.write_page(leaf_pgno)?.fill(0);
                        return Ok(leaf_pgno);
                    }
                }

                if !search_list || next_trunk == 0 {
                    return Err(SqliteError::corrupt(
                        "target free page not found on freelist",
                    ));
                }
                prev_trunk = cur_trunk;
                cur_trunk = next_trunk;
            }
        }

        loop {
            let allocated = self.pager.allocate_raw_page()?;
            if auto_vac && is_ptrmap_page(allocated, usable, lock_pg) {
                continue;
            }
            return Ok(allocated);
        }
    }

    /// Pushes `pgno` onto the database freelist (`freePage2` in `btree.c`).
    pub fn free_page(&mut self, pgno: Pgno) -> Result<()> {
        let usable_u32 = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let auto_vac = self.is_auto_vacuum();
        if pgno <= 1 || pgno == lock_pg || (auto_vac && is_ptrmap_page(pgno, usable_u32, lock_pg)) {
            return Err(SqliteError::corrupt(format!(
                "cannot push page {pgno} onto freelist"
            )));
        }
        let usable = usable_u32 as usize;
        // Max leaf entries per trunk page (usableSize/4 - 2 limit; leave 6 spare slots like btree.c)
        let max_leaves = ((usable / 4).saturating_sub(8)).max(1) as u32;
        let first_trunk = self.pager.header().first_freelist_trunk_page;

        if first_trunk != 0 {
            let trunk_ro = self.pager.read_page(first_trunk)?;
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
                // Zero freed leaf page for deterministic cleanliness
                self.pager.write_page(pgno)?.fill(0);
                {
                    let hdr = self.pager.header_mut()?;
                    hdr.total_freelist_pages += 1;
                }
                if auto_vac {
                    self.ptrmap_put(pgno, PtrmapType::FreePage, 0)?;
                }
                return Ok(());
            }
        }

        // Make `pgno` the new freelist trunk page pointing to `first_trunk` with 0 leaves
        {
            let w = self.pager.write_page(pgno)?;
            w.fill(0);
            w[0..4].copy_from_slice(&first_trunk.to_be_bytes());
            w[4..8].copy_from_slice(&0u32.to_be_bytes());
        }
        {
            let hdr = self.pager.header_mut()?;
            hdr.first_freelist_trunk_page = pgno;
            hdr.total_freelist_pages += 1;
        }
        if auto_vac {
            self.ptrmap_put(pgno, PtrmapType::FreePage, 0)?;
        }
        Ok(())
    }

    // ========================================================================
    // Overflow Payload Chains (`btreeInt.h:198-204`)
    // ========================================================================

    fn prepare_cell_payload(
        &mut self,
        bounds: PayloadBounds,
        full_payload: &[u8],
    ) -> Result<(u32, Vec<u8>, Option<Pgno>)> {
        let total_len = u32::try_from(full_payload.len())
            .map_err(|_| SqliteError::new(ErrorCode::TooBig, "row payload exceeds 4GiB"))?;
        let local_len = bounds.local_len(total_len);
        let local_bytes = full_payload[..local_len].to_vec();

        if local_len == full_payload.len() {
            return Ok((total_len, local_bytes, None));
        }

        let usable = bounds.usable_size as usize;
        let chunk_cap = usable - 4;
        let spilled = &full_payload[local_len..];
        let chunks: Vec<&[u8]> = spilled.chunks(chunk_cap).collect();

        // Allocate overflow pages in forward order and link them
        let mut ovfl_pages = Vec::with_capacity(chunks.len());
        for _ in 0..chunks.len() {
            ovfl_pages.push(self.allocate_page()?);
        }

        for (i, chunk) in chunks.iter().enumerate() {
            let pgno = ovfl_pages[i];
            let next_pgno = ovfl_pages.get(i + 1).copied().unwrap_or(0);
            let buf = self.pager.write_page(pgno)?;
            buf[..usable].fill(0);
            buf[0..4].copy_from_slice(&next_pgno.to_be_bytes());
            buf[4..4 + chunk.len()].copy_from_slice(chunk);
        }

        if self.is_auto_vacuum() && ovfl_pages.len() >= 2 {
            for i in 1..ovfl_pages.len() {
                self.ptrmap_put(ovfl_pages[i], PtrmapType::Overflow2, ovfl_pages[i - 1])?;
            }
        }

        Ok((total_len, local_bytes, Some(ovfl_pages[0])))
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
            let raw = self.pager.read_page(ovfl_pgno)?;
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
                let raw = self.pager.read_page(pgno)?;
                u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]])
            };
            self.free_page(pgno)?;
            first_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(())
    }

    fn read_parsed_page(&mut self, pgno: Pgno) -> Result<ParsedPage> {
        let usable = self.pager.usable_size();
        let raw = self.pager.read_page(pgno)?;
        ParsedPage::parse(pgno, usable, raw)
    }

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

    fn create_root_page(&mut self, empty_page: ParsedPage) -> Result<Pgno> {
        if !self.is_auto_vacuum() {
            let pgno = self.allocate_page()?;
            self.write_parsed_page(pgno, &empty_page)?;
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
        if pgno_move != pgno_root {
            self.save_cursors_for_mutation(None, None)?;
            let (e_type, ptr_page) = self.ptrmap_get(pgno_root)?;
            if matches!(e_type, PtrmapType::RootPage | PtrmapType::FreePage) {
                return Err(SqliteError::corrupt(format!(
                    "unexpected ptrmap type {e_type:?} at new root target {pgno_root}"
                )));
            }
            self.relocate_page(pgno_root, e_type, ptr_page, pgno_move, false)?;
        }

        self.write_parsed_page(pgno_root, &empty_page)?;
        self.ptrmap_put(pgno_root, PtrmapType::RootPage, 0)?;
        self.pager.header_mut()?.largest_root_btree_page = pgno_root;
        Ok(pgno_root)
    }

    /// Creates a new empty Table B+Tree (`0x0d` root leaf) and returns its root `Pgno`.
    pub fn create_table(&mut self) -> Result<Pgno> {
        self.create_root_page(ParsedPage::TableLeaf { cells: Vec::new() })
    }

    /// Creates a new empty Index B-Tree (`0x0a` root leaf) and returns its root `Pgno`.
    pub fn create_index(&mut self) -> Result<Pgno> {
        self.create_root_page(ParsedPage::IndexLeaf { cells: Vec::new() })
    }

    /// Empties all entries and child/overflow pages of a B-tree rooted at `root_pgno`,
    /// preserving `root_pgno` itself as an empty leaf page (`OP_Clear`).
    pub fn clear_tree(&mut self, root_pgno: Pgno) -> Result<()> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let parsed = self.read_parsed_page(root_pgno)?;
        let is_table = matches!(
            parsed,
            ParsedPage::TableLeaf { .. } | ParsedPage::TableInterior { .. }
        );
        self.free_subtree_children(&parsed)?;
        let empty = if is_table {
            ParsedPage::TableLeaf { cells: Vec::new() }
        } else {
            ParsedPage::IndexLeaf { cells: Vec::new() }
        };
        self.write_parsed_page(root_pgno, &empty)
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
        let parsed = self.read_parsed_page(root_pgno)?;
        self.free_subtree_children(&parsed)?;

        if !self.is_auto_vacuum() {
            self.free_page(root_pgno)?;
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

    fn free_subtree_children(&mut self, page: &ParsedPage) -> Result<()> {
        match page {
            ParsedPage::TableLeaf { cells } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                }
            }
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                }
            }
            ParsedPage::TableInterior { cells, right_child } => {
                for c in cells {
                    let child = self.read_parsed_page(c.left_child)?;
                    self.free_subtree_children(&child)?;
                    self.free_page(c.left_child)?;
                }
                let rchild = self.read_parsed_page(*right_child)?;
                self.free_subtree_children(&rchild)?;
                self.free_page(*right_child)?;
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                for c in cells {
                    self.free_overflow_chain(c.overflow_page)?;
                    let child = self.read_parsed_page(c.left_child)?;
                    self.free_subtree_children(&child)?;
                    self.free_page(c.left_child)?;
                }
                let rchild = self.read_parsed_page(*right_child)?;
                self.free_subtree_children(&rchild)?;
                self.free_page(*right_child)?;
            }
        }
        Ok(())
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
            total += self.count_entries_page(child, max_pages, visited)?;
        }
        Ok(total)
    }

    /// Scans all `(rowid, payload_bytes)` entries of a Table B+Tree in ascending `rowid` order.
    pub fn scan_table(&mut self, root_pgno: Pgno) -> Result<Vec<(i64, Vec<u8>)>> {
        if self.pager.page_count() == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
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
                    self.collect_table_entries(c.left_child, out)?;
                }
                self.collect_table_entries(right_child, out)
            }
            _ => Err(SqliteError::corrupt(format!(
                "expected table B-tree page at {pgno}, found {:?}",
                parsed.page_type()
            ))),
        }
    }

    /// Point-lookup by `rowid` in O(log N) page reads (`sqlite3BtreeTableMoveto`).
    pub fn seek_table_rowid(&mut self, root_pgno: Pgno, rowid: i64) -> Result<Option<Vec<u8>>> {
        if self.pager.page_count() == 0 {
            return Ok(None);
        }
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
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
                        let (_, next_pgno) =
                            find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        LookupStep::Descend(next_pgno)
                    }
                    x if x == BtreePageType::TableLeaf as u8 => {
                        let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Err(_) => LookupStep::Done(None),
                            Ok((_, Some((p_start, p_end)))) => {
                                let payload = &raw[p_start as usize..p_end as usize];
                                let vals =
                                    decode_projected_cols_from_record(payload, rowid, cols, enc)?;
                                LookupStep::Done(Some(vals))
                            }
                            Ok((cell_idx, None)) => {
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
                LookupStep::Descend(next_pgno) => cur_pgno = next_pgno,
                LookupStep::Done(res) => return Ok(res),
                LookupStep::Overflow(total_len, local_copy, ovfl) => {
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
                        let (_, next_pgno) =
                            find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        RawStep::Descend(next_pgno)
                    }
                    x if x == BtreePageType::TableLeaf as u8 => {
                        let (_, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Err(_) => RawStep::Done(None),
                            Ok((_, Some((p_start, p_end)))) => {
                                let payload = &raw[p_start as usize..p_end as usize];
                                let f = visit_opt.take().unwrap();
                                RawStep::Done(Some(f(payload)?))
                            }
                            Ok((cell_idx, None)) => {
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
                RawStep::Descend(next_pgno) => cur_pgno = next_pgno,
                RawStep::Done(res) => return Ok(res),
                RawStep::Overflow(total_len, local_copy, ovfl) => {
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

        while let Some((pgno, next_idx)) = stack.pop() {
            if next_idx == 0 {
                self.check_interrupted()?;
                visited_pages += 1;
                if visited_pages > max_pages {
                    return Err(SqliteError::corrupt("cycle detected in table B+Tree"));
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
                let raw = self.pager.read_page(pgno)?;
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
                    let raw = self.pager.read_page(pgno)?;
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

    /// Allocates the next available `rowid` (`max_rowid + 1`, or `1` if empty; falls back to unused positive probe if `i64::MAX` is used).
    pub fn next_rowid(&mut self, root_pgno: Pgno) -> Result<i64> {
        match self.max_table_rowid(root_pgno)? {
            None => Ok(1),
            Some(max_id) if max_id < i64::MAX => Ok(max_id + 1),
            Some(_) => {
                let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
                for _ in 0..100 {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let probe = ((state & 0x7fff_ffff_ffff_ffff) as i64).max(1);
                    if self.seek_table_rowid(root_pgno, probe)?.is_none() {
                        return Ok(probe);
                    }
                }
                for probe in 1..10_000i64 {
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

    /// Inserts or replaces `(rowid, payload)` in the Table B+Tree rooted at `root_pgno`.
    /// Automatically handles overflow chains and multi-level B+Tree splits (preserving `root_pgno`, including Page 1!).
    pub fn insert_table(&mut self, root_pgno: Pgno, rowid: i64, payload: &[u8]) -> Result<()> {
        self.insert_table_at(root_pgno, rowid, payload, None)
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
        self.insert_table_impl(root_pgno, rowid, payload, position, true)
    }

    /// `raw_repack == false` routes size-changing replacements that do not fit the contiguous
    /// gap through the parsed page path instead of [`replace_table_leaf_repack_raw`]; both
    /// produce the same pages (see the byte-for-byte test).
    fn insert_table_impl(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        payload: &[u8],
        position: Option<(Pgno, usize)>,
        raw_repack: bool,
    ) -> Result<()> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_table_leaf(usable);

        if payload.len() <= bounds.max_local as usize {
            if raw_repack
                && let Some((leaf_pgno, cell_idx)) = position
                && self.try_replace_table_row_at(usable, leaf_pgno, cell_idx, rowid, payload)?
            {
                return Ok(());
            }
            if matches!(
                self.try_insert_table_fast_raw(
                    root_pgno, usable, rowid, payload, true, raw_repack
                )?,
                FastTableInsertOutcome::Inserted
            ) {
                return Ok(());
            }
        }

        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, payload)?;
        let new_cell = TableLeafCell {
            rowid,
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        let _ = self.insert_table_rec(root_pgno, new_cell, true, root_pgno != 1)?;
        Ok(())
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
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_table_leaf(usable);

        if payload.len() <= bounds.max_local as usize {
            match self.try_insert_table_fast_raw(root_pgno, usable, rowid, payload, false, true)? {
                FastTableInsertOutcome::Inserted => return Ok(true),
                FastTableInsertOutcome::Duplicate => return Ok(false),
                FastTableInsertOutcome::Fallback => {}
            }
        }

        if self
            .locate_table_leaf_cell_layout(root_pgno, rowid)?
            .is_some()
        {
            return Ok(false);
        }

        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, payload)?;
        let new_cell = TableLeafCell {
            rowid,
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        let _ = self.insert_table_rec(root_pgno, new_cell, true, root_pgno != 1)?;
        Ok(true)
    }

    /// Replaces the non-overflow row `rowid` sitting at `cell_idx` of table leaf `leaf_pgno`
    /// with the non-overflow `payload`, exactly as the replace arms of
    /// [`Self::try_insert_table_fast_raw`] would after descending to that leaf. Returns
    /// `Ok(false)` (nothing modified) when the position does not hold `rowid` or the
    /// replacement is not one of those in-page cases.
    fn try_replace_table_row_at(
        &mut self,
        usable: u32,
        leaf_pgno: Pgno,
        cell_idx: usize,
        rowid: i64,
        payload: &[u8],
    ) -> Result<bool> {
        if leaf_pgno == 0
            || leaf_pgno > self.pager.page_count()
            || leaf_pgno == self.lock_byte_page()
        {
            return Ok(false);
        }
        let hdr_off = if leaf_pgno == 1 { 100 } else { 0 };
        let probe = {
            let raw = self.pager.read_page(leaf_pgno)?;
            if hdr_off >= usable as usize
                || hdr_off >= raw.len()
                || raw[hdr_off] != BtreePageType::TableLeaf as u8
            {
                return Ok(false);
            }
            probe_table_leaf_replace_at_raw(leaf_pgno, usable, raw, cell_idx, rowid, payload.len())
        };
        match probe {
            Some(TableLeafInsertProbe::Replace {
                idx,
                old_off,
                old_cell_len,
                content_top,
            }) => {
                let raw_mut = self.pager.write_page(leaf_pgno)?;
                apply_replace_table_leaf_raw(
                    leaf_pgno,
                    usable,
                    raw_mut,
                    rowid,
                    payload,
                    idx,
                    old_off,
                    old_cell_len,
                    content_top,
                );
                Ok(true)
            }
            Some(TableLeafInsertProbe::ReplaceRepack {
                idx,
                old_off,
                old_cell_len,
                ..
            }) => {
                let reject_overflow_cells = self.is_auto_vacuum();
                let raw_mut = self.pager.write_page(leaf_pgno)?;
                replace_table_leaf_repack_raw(
                    leaf_pgno,
                    usable,
                    raw_mut,
                    rowid,
                    payload,
                    idx,
                    old_off,
                    old_cell_len,
                    reject_overflow_cells,
                )
            }
            _ => Ok(false),
        }
    }

    fn try_insert_table_fast_raw(
        &mut self,
        root_pgno: Pgno,
        usable: u32,
        rowid: i64,
        payload: &[u8],
        allow_replace: bool,
        raw_repack: bool,
    ) -> Result<FastTableInsertOutcome> {
        enum RawLevelStep {
            Descend {
                idx: usize,
                child: Pgno,
                n_cells: usize,
            },
            Leaf(TableLeafInsertProbe),
            Bail,
        }

        let mut cur_pgno = root_pgno;
        let mut parent_pgno: Pgno = 0;
        let mut allow_quick_in_parent = false;
        let mut defragged_leaf = false;
        // Size-changing replacements are repacked on the raw page; once that declines, the
        // probe reverts to its plain answers (`NeedsDefrag` / `DuplicateNeedsFallback`).
        let mut repack_replace = allow_replace && raw_repack;

        for _ in 0..(BTCURSOR_MAX_DEPTH + 1) {
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            let step = {
                let raw = self.pager.read_page(cur_pgno)?;
                if hdr_off >= usable as usize || hdr_off >= raw.len() {
                    RawLevelStep::Bail
                } else {
                    let ptype_byte = raw[hdr_off];
                    if ptype_byte == BtreePageType::TableInterior as u8 {
                        let (idx, child, n_cells) =
                            find_table_interior_child_append_hint(cur_pgno, usable, raw, rowid)?;
                        RawLevelStep::Descend {
                            idx,
                            child,
                            n_cells,
                        }
                    } else if ptype_byte == BtreePageType::TableLeaf as u8 {
                        RawLevelStep::Leaf(probe_insert_table_leaf_raw(
                            cur_pgno,
                            usable,
                            raw,
                            rowid,
                            payload.len(),
                            repack_replace,
                            None,
                        )?)
                    } else {
                        RawLevelStep::Bail
                    }
                }
            };

            let probe = match step {
                RawLevelStep::Descend {
                    idx,
                    child,
                    n_cells,
                } => {
                    allow_quick_in_parent = cur_pgno != 1 && idx == n_cells;
                    parent_pgno = cur_pgno;
                    cur_pgno = child;
                    continue;
                }
                RawLevelStep::Leaf(p) => p,
                RawLevelStep::Bail => return Ok(FastTableInsertOutcome::Fallback),
            };

            match probe {
                TableLeafInsertProbe::Fit {
                    insert_idx,
                    content_top,
                } => {
                    let raw_mut = self.pager.write_page(cur_pgno)?;
                    apply_insert_table_leaf_raw(
                        cur_pgno,
                        raw_mut,
                        rowid,
                        payload,
                        insert_idx,
                        content_top,
                    );
                    return Ok(FastTableInsertOutcome::Inserted);
                }
                TableLeafInsertProbe::Replace {
                    idx,
                    old_off,
                    old_cell_len,
                    content_top,
                } => {
                    if !allow_replace {
                        return Ok(FastTableInsertOutcome::Duplicate);
                    }
                    let raw_mut = self.pager.write_page(cur_pgno)?;
                    apply_replace_table_leaf_raw(
                        cur_pgno,
                        usable,
                        raw_mut,
                        rowid,
                        payload,
                        idx,
                        old_off,
                        old_cell_len,
                        content_top,
                    );
                    return Ok(FastTableInsertOutcome::Inserted);
                }
                TableLeafInsertProbe::ReplaceRepack {
                    idx,
                    old_off,
                    old_cell_len,
                    legacy_defrag,
                } => {
                    let reject_overflow_cells = self.is_auto_vacuum();
                    let raw_mut = self.pager.write_page(cur_pgno)?;
                    if replace_table_leaf_repack_raw(
                        cur_pgno,
                        usable,
                        raw_mut,
                        rowid,
                        payload,
                        idx,
                        old_off,
                        old_cell_len,
                        reject_overflow_cells,
                    )? {
                        return Ok(FastTableInsertOutcome::Inserted);
                    }
                    // Not handled (split needed or unusual page): continue exactly as the
                    // plain probe answer would have.
                    if legacy_defrag && !defragged_leaf {
                        defragged_leaf = true;
                        repack_replace = false;
                        defragment_raw_leaf_in_place(cur_pgno, usable, raw_mut)?;
                        continue;
                    }
                    return Ok(FastTableInsertOutcome::Fallback);
                }
                TableLeafInsertProbe::NeedsDefrag if !defragged_leaf => {
                    defragged_leaf = true;
                    let raw_mut = self.pager.write_page(cur_pgno)?;
                    defragment_raw_leaf_in_place(cur_pgno, usable, raw_mut)?;
                    continue;
                }
                TableLeafInsertProbe::DuplicateNeedsFallback => {
                    return Ok(if allow_replace {
                        FastTableInsertOutcome::Fallback
                    } else {
                        FastTableInsertOutcome::Duplicate
                    });
                }
                TableLeafInsertProbe::QuickRightSplit { last_rowid }
                    if allow_quick_in_parent && cur_pgno != root_pgno && cur_pgno != 1 =>
                {
                    let parent_top_opt = {
                        let parent_raw = self.pager.read_page(parent_pgno)?;
                        probe_append_table_interior_rightmost(
                            parent_pgno,
                            usable,
                            parent_raw,
                            last_rowid,
                        )
                    };
                    if let Some(parent_top) = parent_top_opt {
                        let new_leaf = self.allocate_page()?;
                        {
                            let new_leaf_mut = self.pager.write_page(new_leaf)?;
                            init_single_cell_table_leaf_raw(
                                new_leaf,
                                usable,
                                new_leaf_mut,
                                rowid,
                                payload,
                            );
                        }
                        {
                            let parent_mut = self.pager.write_page(parent_pgno)?;
                            apply_append_table_interior_rightmost(
                                parent_pgno,
                                parent_mut,
                                cur_pgno,
                                last_rowid,
                                new_leaf,
                                parent_top,
                            );
                        }
                        if self.is_auto_vacuum() {
                            self.ptrmap_put(new_leaf, PtrmapType::BtreeNode, parent_pgno)?;
                        }
                        return Ok(FastTableInsertOutcome::Inserted);
                    }
                    return Ok(FastTableInsertOutcome::Fallback);
                }
                _ => return Ok(FastTableInsertOutcome::Fallback),
            }
        }
        Ok(FastTableInsertOutcome::Fallback)
    }

    /// Inserts `cell` into subtree `pgno`. If a non-root `pgno` splits into `k + 1` sibling pages,
    /// writes the leftmost chunk into `pgno`, allocates `k` right sibling pages, and returns
    /// `[(div_0, sib_1), ..., (div_{k-1}, sib_k)]`.
    fn insert_table_rec(
        &mut self,
        pgno: Pgno,
        cell: TableLeafCell,
        is_root: bool,
        allow_quick_balance: bool,
    ) -> Result<Vec<(i64, Pgno)>> {
        let usable = self.pager.usable_size();
        let ptype = {
            let raw = self.pager.read_page(pgno)?;
            peek_page_type(pgno, usable, raw)?
        };

        if ptype == BtreePageType::TableInterior {
            let (idx, target_child, n_cells) = {
                let raw = self.pager.read_page(pgno)?;
                let (_, n_cells, _) = raw_page_header(pgno, usable, raw)?;
                let (idx, target_child) = find_table_interior_child(pgno, usable, raw, cell.rowid)?;
                (idx, target_child, n_cells)
            };
            let child_allow_quick = pgno != 1 && idx == n_cells;
            let target_ptype = {
                let raw = self.pager.read_page(target_child)?;
                peek_page_type(target_child, usable, raw)?
            };

            let (cells, right_child) = if !child_allow_quick
                && n_cells >= 1
                && target_ptype == BtreePageType::TableLeaf
            {
                let inserted_fast = {
                    let raw_mut = self.pager.write_page(target_child)?;
                    try_insert_table_leaf_in_place(target_child, usable, raw_mut, &cell)?
                };
                if inserted_fast {
                    if self.is_auto_vacuum()
                        && let Some(ovfl) = cell.overflow_page
                    {
                        self.ptrmap_put(ovfl, PtrmapType::Overflow1, target_child)?;
                    }
                    return Ok(Vec::new());
                }

                let ParsedPage::TableLeaf {
                    cells: mut target_cells,
                } = self.read_parsed_page(target_child)?
                else {
                    return Err(SqliteError::corrupt("expected table leaf page"));
                };
                match target_cells.binary_search_by_key(&cell.rowid, |c| c.rowid) {
                    Ok(pos) => {
                        let old = std::mem::replace(&mut target_cells[pos], cell);
                        self.free_overflow_chain(old.overflow_page)?;
                    }
                    Err(pos) => {
                        target_cells.insert(pos, cell);
                    }
                }
                let single_candidate = ParsedPage::TableLeaf {
                    cells: target_cells,
                };
                if single_candidate.fits_in_page(target_child, usable) {
                    self.write_parsed_page(target_child, &single_candidate)?;
                    return Ok(Vec::new());
                }
                let ParsedPage::TableLeaf {
                    cells: target_cells,
                } = single_candidate
                else {
                    unreachable!()
                };

                let ParsedPage::TableInterior {
                    mut cells,
                    mut right_child,
                } = self.read_parsed_page(pgno)?
                else {
                    return Err(SqliteError::corrupt("expected table interior page"));
                };
                let m = cells.len();
                let (nx_div, n_old) = if m < 2 {
                    (0usize, 2usize)
                } else if idx == 0 {
                    (0usize, 3usize)
                } else if idx == m {
                    (m - 2, 3usize)
                } else {
                    (idx - 1, 3usize)
                };

                let mut avail_pgnos = Vec::with_capacity(n_old + 2);
                let mut all_cells = Vec::new();
                let mut target_opt = Some(target_cells);
                for pos in nx_div..(nx_div + n_old) {
                    let sib_pgno = cells.get(pos).map_or(right_child, |c| c.left_child);
                    avail_pgnos.push(sib_pgno);
                    if pos == idx {
                        all_cells.extend(target_opt.take().expect("single idx slot"));
                    } else {
                        let ParsedPage::TableLeaf { cells: sib_cells } =
                            self.read_parsed_page(sib_pgno)?
                        else {
                            return Err(SqliteError::corrupt("expected sibling table leaf page"));
                        };
                        all_cells.extend(sib_cells);
                    }
                }

                let new_chunks = balance_table_leaf_siblings(all_cells, usable);
                let k = new_chunks.len();
                while avail_pgnos.len() < k {
                    avail_pgnos.push(self.allocate_page()?);
                }
                while avail_pgnos.len() > k {
                    let extra_pg = avail_pgnos.pop().expect("len > k");
                    self.free_page(extra_pg)?;
                }
                avail_pgnos.sort_unstable();

                let mut max_keys = Vec::with_capacity(k);
                for (&out_pgno, chunk) in avail_pgnos.iter().zip(new_chunks) {
                    max_keys.push(chunk.last().expect("non-empty leaf chunk").rowid);
                    self.write_parsed_page(out_pgno, &ParsedPage::TableLeaf { cells: chunk })?;
                }

                let new_dividers = (0..(k - 1)).map(|j| TableInteriorCell {
                    left_child: avail_pgnos[j],
                    key: max_keys[j],
                });
                if nx_div + n_old - 1 < m {
                    cells[nx_div + n_old - 1].left_child = avail_pgnos[k - 1];
                    cells.splice(nx_div..(nx_div + n_old - 1), new_dividers);
                } else {
                    cells.splice(nx_div..m, new_dividers);
                    right_child = avail_pgnos[k - 1];
                }
                (cells, right_child)
            } else {
                let child_splits =
                    self.insert_table_rec(target_child, cell, false, child_allow_quick)?;
                if child_splits.is_empty() {
                    return Ok(Vec::new());
                }

                let ParsedPage::TableInterior {
                    mut cells,
                    mut right_child,
                } = self.read_parsed_page(pgno)?
                else {
                    return Err(SqliteError::corrupt("expected table interior page"));
                };

                let mut new_int_cells = Vec::with_capacity(child_splits.len());
                let mut cur_left = target_child;
                for &(div_key, next_sib) in &child_splits {
                    new_int_cells.push(TableInteriorCell {
                        left_child: cur_left,
                        key: div_key,
                    });
                    cur_left = next_sib;
                }
                let last_sib = cur_left;

                if idx < cells.len() {
                    cells[idx].left_child = last_sib;
                    cells.splice(idx..idx, new_int_cells);
                } else {
                    cells.extend(new_int_cells);
                    right_child = last_sib;
                }
                (cells, right_child)
            };

            let candidate = ParsedPage::TableInterior { cells, right_child };
            if candidate.fits_in_page(pgno, usable) {
                self.write_parsed_page(pgno, &candidate)?;
                return Ok(Vec::new());
            }
            if is_root && pgno == 1 && candidate.fits_in_page(2, usable) {
                let child_pgno = self.allocate_page()?;
                self.write_parsed_page(child_pgno, &candidate)?;
                self.write_parsed_page(
                    1,
                    &ParsedPage::TableInterior {
                        cells: Vec::new(),
                        right_child: child_pgno,
                    },
                )?;
                return Ok(Vec::new());
            }

            let ParsedPage::TableInterior {
                mut cells,
                right_child: orig_right,
            } = candidate
            else {
                unreachable!()
            };
            let mid = cells.len() / 2;
            let right_cells = cells.split_off(mid + 1);
            let promoted = cells.pop().expect("mid exists");

            let left_page = ParsedPage::TableInterior {
                cells,
                right_child: promoted.left_child,
            };
            let right_page = ParsedPage::TableInterior {
                cells: right_cells,
                right_child: orig_right,
            };

            if is_root {
                let left_pgno = self.allocate_page()?;
                let right_pgno = self.allocate_page()?;
                self.write_parsed_page(left_pgno, &left_page)?;
                self.write_parsed_page(right_pgno, &right_page)?;
                let new_root = ParsedPage::TableInterior {
                    cells: vec![TableInteriorCell {
                        left_child: left_pgno,
                        key: promoted.key,
                    }],
                    right_child: right_pgno,
                };
                self.write_parsed_page(pgno, &new_root)?;
                return Ok(Vec::new());
            }

            let right_pgno = self.allocate_page()?;
            self.write_parsed_page(pgno, &left_page)?;
            self.write_parsed_page(right_pgno, &right_page)?;
            return Ok(vec![(promoted.key, right_pgno)]);
        }

        if ptype == BtreePageType::TableLeaf {
            // Fast path: O(1) zero-allocation in-place insert when page is defragmented and has room!
            let inserted_fast = {
                let raw_mut = self.pager.write_page(pgno)?;
                try_insert_table_leaf_in_place(pgno, usable, raw_mut, &cell)?
            };
            if inserted_fast {
                if self.is_auto_vacuum()
                    && let Some(ovfl) = cell.overflow_page
                {
                    self.ptrmap_put(ovfl, PtrmapType::Overflow1, pgno)?;
                }
                return Ok(Vec::new());
            }
        }

        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::TableLeaf { mut cells } => {
                let (inserted_new, insert_idx) =
                    match cells.binary_search_by_key(&cell.rowid, |c| c.rowid) {
                        Ok(idx) => {
                            let old = std::mem::replace(&mut cells[idx], cell);
                            self.free_overflow_chain(old.overflow_page)?;
                            (false, idx)
                        }
                        Err(idx) => {
                            cells.insert(idx, cell);
                            (true, idx)
                        }
                    };

                let candidate = ParsedPage::TableLeaf { cells };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(Vec::new());
                }
                if is_root && pgno == 1 && candidate.fits_in_page(2, usable) {
                    let child_pgno = self.allocate_page()?;
                    self.write_parsed_page(child_pgno, &candidate)?;
                    self.write_parsed_page(
                        1,
                        &ParsedPage::TableInterior {
                            cells: Vec::new(),
                            right_child: child_pgno,
                        },
                    )?;
                    return Ok(Vec::new());
                }

                // Rightmost append on non-Page-1 leaf uses `balance_quick` (`btree.c:8039, 9220`);
                // interior insertions / replacements pick the best fitting 2-way split, or fall back
                // to a 3-way split (`NB = 3` in `balance_nonroot`) when two ~96%-page cells collide.
                let ParsedPage::TableLeaf { cells } = candidate else {
                    unreachable!()
                };
                let appended_at_right_end = inserted_new && insert_idx + 1 == cells.len();
                let first_pgno = if is_root { 2 } else { pgno };
                let chunks = partition_table_leaf_cells(
                    cells,
                    first_pgno,
                    usable,
                    allow_quick_balance && appended_at_right_end,
                );
                debug_assert!(chunks.len() >= 2);

                if is_root {
                    let mut pgnos = Vec::with_capacity(chunks.len());
                    let mut max_keys = Vec::with_capacity(chunks.len());
                    for chunk in chunks {
                        let max_k = chunk.last().expect("chunk non-empty").rowid;
                        let p = self.allocate_page()?;
                        self.write_parsed_page(p, &ParsedPage::TableLeaf { cells: chunk })?;
                        pgnos.push(p);
                        max_keys.push(max_k);
                    }
                    let mut root_cells = Vec::with_capacity(pgnos.len() - 1);
                    for j in 0..(pgnos.len() - 1) {
                        root_cells.push(TableInteriorCell {
                            left_child: pgnos[j],
                            key: max_keys[j],
                        });
                    }
                    let new_root = ParsedPage::TableInterior {
                        cells: root_cells,
                        right_child: *pgnos.last().expect("at least 2 chunks"),
                    };
                    self.write_parsed_page(pgno, &new_root)?;
                    return Ok(Vec::new());
                }

                let mut iter = chunks.into_iter();
                let first_chunk = iter.next().expect("at least 2 chunks");
                let mut prev_div_key = first_chunk.last().expect("chunk non-empty").rowid;
                self.write_parsed_page(pgno, &ParsedPage::TableLeaf { cells: first_chunk })?;

                let mut splits = Vec::with_capacity(iter.len());
                for chunk in iter {
                    let next_div = chunk.last().expect("chunk non-empty").rowid;
                    let sib_pgno = self.allocate_page()?;
                    self.write_parsed_page(sib_pgno, &ParsedPage::TableLeaf { cells: chunk })?;
                    splits.push((prev_div_key, sib_pgno));
                    prev_div_key = next_div;
                }
                Ok(splits)
            }
            _ => Err(SqliteError::corrupt("expected table B-tree page in insert")),
        }
    }

    /// Deletes `rowid` from the Table B+Tree rooted at `root_pgno`. Returns `true` if a row was deleted.
    pub fn delete_table(&mut self, root_pgno: Pgno, rowid: i64) -> Result<bool> {
        self.delete_table_at(root_pgno, rowid, None)
    }

    /// [`Self::delete_table`] for a caller that already located `rowid`; see
    /// [`Self::insert_table_at`] for the meaning and validation of `position`.
    pub fn delete_table_at(
        &mut self,
        root_pgno: Pgno,
        rowid: i64,
        position: Option<(Pgno, usize)>,
    ) -> Result<bool> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        if let Some((leaf_pgno, cell_idx)) = position
            && self.try_delete_table_row_at(root_pgno, leaf_pgno, cell_idx, rowid)?
        {
            return Ok(true);
        }
        let (deleted, _) = self.delete_table_rec(root_pgno, rowid, true)?;
        Ok(deleted)
    }

    /// Deletes row `rowid` sitting at `cell_idx` of table leaf `leaf_pgno` in place, exactly as
    /// the leaf step of [`Self::delete_table_rec`] would. Returns `Ok(false)` (nothing modified)
    /// when the position does not hold `rowid`, when removing the row would empty a non-root
    /// leaf (the parent then needs fixing up), or when the page needs the parsed path.
    fn try_delete_table_row_at(
        &mut self,
        root_pgno: Pgno,
        leaf_pgno: Pgno,
        cell_idx: usize,
        rowid: i64,
    ) -> Result<bool> {
        if leaf_pgno == 0
            || leaf_pgno > self.pager.page_count()
            || leaf_pgno == self.lock_byte_page()
        {
            return Ok(false);
        }
        let usable = self.pager.usable_size();
        let is_root = leaf_pgno == root_pgno;
        {
            let raw = self.pager.read_page(leaf_pgno)?;
            if !table_leaf_cell_has_rowid(leaf_pgno, usable, raw, cell_idx, rowid, is_root) {
                return Ok(false);
            }
        }
        let outcome = {
            let raw_mut = self.pager.write_page(leaf_pgno)?;
            try_delete_table_leaf_in_place(
                leaf_pgno,
                usable,
                raw_mut,
                rowid,
                is_root,
                Some(cell_idx),
            )?
        };
        match outcome {
            RawLeafDeleteOutcome::DeletedInPlace {
                overflow_page,
                now_empty,
            } if is_root || !now_empty => {
                self.free_overflow_chain(overflow_page)?;
                Ok(true)
            }
            // `NotFound`, `Fallback`, and the sole cell of a non-root leaf (already refused by
            // the check above) all leave the page untouched: the general path takes over.
            _ => Ok(false),
        }
    }

    /// Returns `(deleted, child_replacement)`:
    /// - `None`: `pgno` still valid and non-empty (or is `is_root`).
    /// - `Some(0)`: `pgno` was a non-root leaf that became completely empty and has been freed!
    /// - `Some(sole_child)`: `pgno` was a non-root interior page reduced to 0 cells; freed and replaced by `sole_child`!
    fn delete_table_rec(
        &mut self,
        pgno: Pgno,
        rowid: i64,
        is_root: bool,
    ) -> Result<(bool, Option<Pgno>)> {
        let usable = self.pager.usable_size();
        let ptype = {
            let raw = self.pager.read_page(pgno)?;
            peek_page_type(pgno, usable, raw)?
        };

        if ptype == BtreePageType::TableInterior {
            let (idx, target_child) = {
                let raw = self.pager.read_page(pgno)?;
                find_table_interior_child(pgno, usable, raw, rowid)?
            };
            let (deleted, replacement) = self.delete_table_rec(target_child, rowid, false)?;
            if !deleted || replacement.is_none() {
                return Ok((deleted, None));
            }

            let rep = replacement.unwrap();
            let ParsedPage::TableInterior {
                mut cells,
                mut right_child,
            } = self.read_parsed_page(pgno)?
            else {
                return Err(SqliteError::corrupt("expected table interior page"));
            };

            if rep == 0 {
                if idx < cells.len() {
                    cells.remove(idx);
                } else if let Some(last) = cells.pop() {
                    right_child = last.left_child;
                } else if is_root {
                    self.write_parsed_page(pgno, &ParsedPage::TableLeaf { cells: Vec::new() })?;
                    return Ok((true, None));
                } else {
                    return Ok((true, Some(0)));
                }
            } else if idx < cells.len() {
                cells[idx].left_child = rep;
            } else {
                right_child = rep;
            }

            if cells.is_empty() {
                if !is_root {
                    self.free_page(pgno)?;
                    return Ok((true, Some(right_child)));
                }
                let child_parsed = self.read_parsed_page(right_child)?;
                if child_parsed.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &child_parsed)?;
                    self.free_page(right_child)?;
                    return Ok((true, None));
                }
            }

            self.write_parsed_page(pgno, &ParsedPage::TableInterior { cells, right_child })?;
            return Ok((true, None));
        }

        if ptype == BtreePageType::TableLeaf {
            // Probe existence first via read_page so missing-key deletes don't dirty pages!
            let exists = {
                let raw = self.pager.read_page(pgno)?;
                seek_table_leaf_cell(pgno, usable, raw, rowid)?.is_some()
            };
            if !exists {
                return Ok((false, None));
            }
            let outcome = {
                let raw_mut = self.pager.write_page(pgno)?;
                try_delete_table_leaf_in_place(pgno, usable, raw_mut, rowid, is_root, None)?
            };
            match outcome {
                RawLeafDeleteOutcome::NotFound => return Ok((false, None)),
                RawLeafDeleteOutcome::DeletedInPlace {
                    overflow_page,
                    now_empty,
                } => {
                    self.free_overflow_chain(overflow_page)?;
                    if now_empty && !is_root {
                        self.free_page(pgno)?;
                        return Ok((true, Some(0)));
                    }
                    return Ok((true, None));
                }
                RawLeafDeleteOutcome::Fallback => {}
            }
        }

        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::TableLeaf { mut cells } => {
                let Ok(idx) = cells.binary_search_by_key(&rowid, |c| c.rowid) else {
                    return Ok((false, None));
                };
                let removed = cells.remove(idx);
                self.free_overflow_chain(removed.overflow_page)?;

                if cells.is_empty() && !is_root {
                    self.free_page(pgno)?;
                    return Ok((true, Some(0)));
                }

                self.write_parsed_page(pgno, &ParsedPage::TableLeaf { cells })?;
                Ok((true, None))
            }
            _ => Err(SqliteError::corrupt("expected table page in delete")),
        }
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
                            .scan_index_raw_rec(
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
                    self.scan_index_raw_rec(
                        right_child,
                        usable,
                        false,
                        visited_pages,
                        max_pages,
                        visit,
                    )
                } else {
                    if self
                        .scan_index_raw_rec(
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
                            .scan_index_raw_rec(
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
                        let mut lo = 0usize;
                        let mut hi = n_cell;
                        let mut slow = false;
                        let mut found = false;
                        while lo < hi {
                            let mid = lo + (hi - lo) / 2;
                            let cell = read_index_leaf_cell(pgno, usable, raw, n_cell, mid)?;
                            if cell.overflow_page.is_some() {
                                slow = true;
                                break;
                            }
                            let ord = compare_raw_record_range(
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
                        while lo < hi {
                            let mid = lo + (hi - lo) / 2;
                            let (lc, cell) =
                                read_index_interior_cell(pgno, usable, raw, n_cell, mid)?;
                            if cell.overflow_page.is_some() {
                                slow = true;
                                break;
                            }
                            let ord = compare_raw_record_range(
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
                            PrefixStep::Found(true)
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
        let _ = self.scan_table_range_raw_rec(
            root_pgno,
            usable,
            bounds,
            max_local,
            lo_inc,
            hi_inc,
            &mut visited_pages,
            max_pages,
            &mut visit,
        )?;
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
                        TLeafStep::Broke => return Ok(ControlFlow::Break(())),
                        TLeafStep::Overflow(rowid, tlen, local_copy, ovfl) => {
                            let full = self.read_full_payload(tlen, &local_copy, ovfl)?;
                            if visit(rowid, &full)?.is_break() {
                                return Ok(ControlFlow::Break(()));
                            }
                        }
                    }
                }
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
        let _ = self.seek_index_range_raw_cells_rec(
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
            &mut visit,
        )?;
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

    /// Inserts a serialized index record (`[indexed_cols..., rowid]`) into an Index B-Tree.
    pub fn insert_index(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        self.insert_index_impl(root_pgno, record_bytes, orders, true)
    }

    fn insert_index_impl(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
        raw_balance: bool,
    ) -> Result<()> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_index(usable);

        if record_bytes.len() <= bounds.max_local as usize
            && let Some((leaf_pgno, insert_idx, content_top)) =
                self.probe_fast_index_insert_raw(root_pgno, usable, record_bytes, orders, enc)?
        {
            let raw_mut = self.pager.write_page(leaf_pgno)?;
            apply_insert_index_leaf_raw(leaf_pgno, raw_mut, record_bytes, insert_idx, content_top);
            return Ok(());
        }

        if raw_balance
            && record_bytes.len() <= bounds.max_local as usize
            && self.try_insert_index_balance_raw(root_pgno, usable, record_bytes, orders, enc)?
        {
            return Ok(());
        }

        let full_vals = decode_record_with_encoding(record_bytes, enc)?;
        let key_len = if orders.is_empty() {
            full_vals.len()
        } else {
            orders.len().min(full_vals.len())
        };
        let key_vals = &full_vals[..key_len];
        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, record_bytes)?;
        let leaf_cell = IndexLeafCell {
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        if let Some((leaf_pgno, insert_idx, content_top)) =
            self.probe_fast_index_insert_target(root_pgno, &leaf_cell, key_vals, orders, enc)?
        {
            {
                let raw_mut = self.pager.write_page(leaf_pgno)?;
                apply_insert_index_leaf_in_place(
                    leaf_pgno,
                    raw_mut,
                    &leaf_cell,
                    insert_idx,
                    content_top,
                );
            }
            if self.is_auto_vacuum()
                && let Some(ovfl) = leaf_cell.overflow_page
            {
                self.ptrmap_put(ovfl, PtrmapType::Overflow1, leaf_pgno)?;
            }
            return Ok(());
        }

        if let Some((promoted_cell, right_pgno)) =
            self.insert_index_rec(root_pgno, leaf_cell, key_vals, orders)?
        {
            self.grow_index_root(root_pgno, promoted_cell, right_pgno)?;
        }
        Ok(())
    }

    /// Deepens an Index B-Tree whose root just split: the root's (left-half) content moves to a
    /// fresh page and the root becomes an interior page holding the single promoted divider.
    fn grow_index_root(
        &mut self,
        root_pgno: Pgno,
        promoted_cell: IndexLeafCell,
        right_pgno: Pgno,
    ) -> Result<()> {
        let left_parsed = self.read_parsed_page(root_pgno)?;
        let left_pgno = self.allocate_page()?;
        self.write_parsed_page(left_pgno, &left_parsed)?;

        let new_root = ParsedPage::IndexInterior {
            cells: vec![IndexInteriorCell {
                left_child: left_pgno,
                total_payload_len: promoted_cell.total_payload_len,
                local_payload: promoted_cell.local_payload,
                overflow_page: promoted_cell.overflow_page,
            }],
            right_child: right_pgno,
        };
        self.write_parsed_page(root_pgno, &new_root)
    }

    /// Raw-byte counterpart of the leaf-level sibling rebalance in [`Self::insert_index_rec`] for
    /// a non-overflow `record_bytes` whose target leaf is full.
    ///
    /// Descends on raw pages, gathers the cells of the (up to three) sibling leaves plus their
    /// parent dividers as byte ranges of a scratch copy, redistributes them with the same
    /// algorithm as [`balance_index_leaf_siblings`] and rewrites the leaves and the parent
    /// directly, so no per-cell heap `Vec` is ever built. The resulting pages are byte-identical
    /// to what the parsed path produces.
    ///
    /// Returns `Ok(false)` without touching any page whenever the shape is not provably handled
    /// (auto-vacuum pointer maps, root leaf, duplicate key, overflow cells on the search path,
    /// anything that looks malformed); the caller then runs the general parsed path.
    ///
    /// Kept out of line so the in-place insert fast path of [`Self::insert_index`] stays compact.
    #[inline(never)]
    fn try_insert_index_balance_raw(
        &mut self,
        root_pgno: Pgno,
        usable: u32,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<bool> {
        if self.is_auto_vacuum() {
            return Ok(false);
        }
        let usable_usize = usable as usize;
        let bounds = PayloadBounds::for_index(usable);
        let leaf_tag = BtreePageType::IndexLeaf as u8;
        let interior_tag = BtreePageType::IndexInterior as u8;

        // 1. Raw descent to the interior page whose child is the target leaf.
        let key = RawRecordKey::for_index_orders(record_bytes, orders);
        let mut path: Vec<(Pgno, usize)> = Vec::new();
        let mut cur = root_pgno;
        let (parent_pgno, idx, target_pgno) = loop {
            if path.len() >= BTCURSOR_MAX_DEPTH || cur <= 1 {
                return Ok(false);
            }
            let raw = self.pager.read_page(cur)?;
            if raw.len() < usable_usize || raw[0] != interior_tag {
                return Ok(false);
            }
            let Some((RawIndexInteriorStep::Child(child), slot)) =
                probe_index_interior_slot_raw(cur, usable, raw, &key, orders, enc)?
            else {
                return Ok(false);
            };
            if child <= 1 {
                return Ok(false);
            }
            let child_raw = self.pager.read_page(child)?;
            if child_raw.len() < usable_usize {
                return Ok(false);
            }
            let child_tag = child_raw[0];
            if child_tag == leaf_tag {
                break (cur, slot, child);
            }
            if child_tag != interior_tag {
                return Ok(false);
            }
            path.push((cur, slot));
            cur = child;
        };

        // 2. Snapshot the parent and index its cells.
        let new_cell_len = varint_len(record_bytes.len() as u64) + record_bytes.len();
        let mut arena: Vec<u8> = Vec::with_capacity(4 * usable_usize + new_cell_len);
        {
            let raw = self.pager.read_page(parent_pgno)?;
            arena.extend_from_slice(&raw[..usable_usize]);
        }
        let mut parent_cells: Vec<RawCellRef> = Vec::new();
        let Some(m) =
            collect_raw_index_cells(&arena, 0, usable_usize, true, bounds, &mut parent_cells)
        else {
            return Ok(false);
        };
        if m == 0 || idx > m {
            return Ok(false);
        }
        let parent_right = u32::from_be_bytes([arena[8], arena[9], arena[10], arena[11]]);
        let (nx_div, n_old) = if m < 2 {
            (0usize, 2usize)
        } else if idx == 0 {
            (0usize, 3usize)
        } else if idx == m {
            (m - 2, 3usize)
        } else {
            (idx - 1, 3usize)
        };
        let mut sib_pgnos = [0 as Pgno; 3];
        for (s, slot) in sib_pgnos.iter_mut().enumerate().take(n_old) {
            let child_slot = nx_div + s;
            let pg = if child_slot < m {
                raw_cell_left_child(&arena, parent_cells[child_slot])
            } else {
                parent_right
            };
            if pg <= 1 || pg == parent_pgno {
                return Ok(false);
            }
            *slot = pg;
        }
        let sibs = &sib_pgnos[..n_old];
        if sibs[idx - nx_div] != target_pgno
            || sibs[0] == sibs[1]
            || (n_old == 3 && (sibs[0] == sibs[2] || sibs[1] == sibs[2]))
        {
            return Ok(false);
        }

        // 3. Gather `sibling cells, divider, sibling cells, ...` with the new cell in place.
        let max_fields = if orders.is_empty() {
            None
        } else {
            Some(orders.len())
        };
        let mut all: Vec<RawCellRef> = Vec::new();
        for (s, &sib_pgno) in sibs.iter().enumerate() {
            let base = arena.len();
            {
                let raw = self.pager.read_page(sib_pgno)?;
                if raw.len() < usable_usize || raw[0] != leaf_tag {
                    return Ok(false);
                }
                arena.extend_from_slice(&raw[..usable_usize]);
            }
            let first = all.len();
            let Some(n_cell) =
                collect_raw_index_cells(&arena, base, usable_usize, false, bounds, &mut all)
            else {
                return Ok(false);
            };
            if nx_div + s == idx {
                let used: usize = all[first..].iter().map(|c| c.leaf_size() + 2).sum();
                if 8 + used + new_cell_len.max(4) + 2 <= usable_usize {
                    // Not actually full: the parsed path rewrites just this one leaf.
                    return Ok(false);
                }
                let mut lo = 0usize;
                let mut hi = n_cell;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let Some(existing) = raw_leaf_cell_local_record(&arena, all[first + mid])
                    else {
                        return Ok(false);
                    };
                    match compare_raw_records_with_encoding(
                        existing,
                        record_bytes,
                        max_fields,
                        orders,
                        enc,
                    ) {
                        Ok(Ordering::Less) => lo = mid + 1,
                        Ok(Ordering::Greater) => hi = mid,
                        Ok(Ordering::Equal) => return Ok(false),
                        Err(e) if e.code == ErrorCode::Corrupt => return Ok(false),
                        Err(e) => return Err(e),
                    }
                }
                let off = arena.len();
                push_varint(&mut arena, record_bytes.len() as u64);
                arena.extend_from_slice(record_bytes);
                all.insert(
                    first + lo,
                    RawCellRef {
                        off: off as u32,
                        len: new_cell_len as u32,
                    },
                );
            }
            if s + 1 < n_old {
                all.push(parent_cells[nx_div + s].without_left_child());
            }
        }

        // 4. Redistribute, and prove every new leaf fits before the first page is modified.
        let cnt_new = index_leaf_balance_counts(all.len(), |i| all[i].leaf_size(), usable);
        let k = cnt_new.len();
        let mut start = 0usize;
        for (j, &end) in cnt_new.iter().enumerate() {
            if end < start || end > all.len() || (j + 1 < k && end >= all.len()) {
                return Ok(false);
            }
            let need: usize = 8 + all[start..end]
                .iter()
                .map(|c| c.leaf_size() + 2)
                .sum::<usize>();
            if need > usable_usize {
                return Ok(false);
            }
            start = if j + 1 < k { end + 1 } else { end };
        }

        let mut avail_pgnos: Vec<Pgno> = sibs.to_vec();
        while avail_pgnos.len() < k {
            avail_pgnos.push(self.allocate_page()?);
        }
        while avail_pgnos.len() > k {
            let extra_pg = avail_pgnos.pop().expect("len > k");
            self.free_page(extra_pg)?;
        }
        avail_pgnos.sort_unstable();

        let mut start = 0usize;
        for (j, &end) in cnt_new.iter().enumerate() {
            let raw = self.pager.write_page(avail_pgnos[j])?;
            write_raw_index_leaf(raw, usable_usize, &arena, &all[start..end]);
            start = end + 1;
        }

        // 5. Splice the new dividers into the parent.
        let mut new_parent: Vec<(Pgno, RawCellRef)> = Vec::with_capacity(m + 2);
        for &c in &parent_cells[..nx_div] {
            new_parent.push((raw_cell_left_child(&arena, c), c.without_left_child()));
        }
        for j in 0..k - 1 {
            new_parent.push((avail_pgnos[j], all[cnt_new[j]]));
        }
        let mut new_right = parent_right;
        if nx_div + n_old - 1 < m {
            new_parent.push((
                avail_pgnos[k - 1],
                parent_cells[nx_div + n_old - 1].without_left_child(),
            ));
            for &c in &parent_cells[nx_div + n_old..] {
                new_parent.push((raw_cell_left_child(&arena, c), c.without_left_child()));
            }
        } else {
            new_right = avail_pgnos[k - 1];
        }
        let parent_need: usize = 12
            + new_parent
                .iter()
                .map(|(_, c)| 4 + c.len as usize + 2)
                .sum::<usize>();
        if parent_need <= usable_usize {
            let raw = self.pager.write_page(parent_pgno)?;
            write_raw_index_interior(raw, usable_usize, &arena, &new_parent, new_right);
            return Ok(true);
        }

        // Parent overflow (rare): split it and propagate upwards through the parsed helpers.
        let mut cells: Vec<IndexInteriorCell> = Vec::with_capacity(new_parent.len());
        for &(left_child, c) in &new_parent {
            let body = &arena[c.off as usize..(c.off + c.len) as usize];
            let (payload_u64, n1) = get_varint(body)?;
            let total_payload_len = u32::try_from(payload_u64)
                .map_err(|_| SqliteError::corrupt("index interior payload > 4GiB"))?;
            let local_len = bounds.local_len(total_payload_len);
            let overflow_page = if (local_len as u32) < total_payload_len {
                let o = n1 + local_len;
                Some(u32::from_be_bytes([
                    body[o],
                    body[o + 1],
                    body[o + 2],
                    body[o + 3],
                ]))
            } else {
                None
            };
            cells.push(IndexInteriorCell {
                left_child,
                total_payload_len,
                local_payload: body[n1..n1 + local_len].to_vec(),
                overflow_page,
            });
        }
        let mut carry = self.split_index_interior(parent_pgno, cells, new_right)?;
        let mut child = parent_pgno;
        while let Some((promoted, new_right_child)) = carry.take() {
            let Some((anc_pgno, anc_idx)) = path.pop() else {
                self.grow_index_root(root_pgno, promoted, new_right_child)?;
                break;
            };
            carry = self.insert_index_divider_parsed(
                anc_pgno,
                anc_idx,
                child,
                promoted,
                new_right_child,
            )?;
            child = anc_pgno;
        }
        Ok(true)
    }

    /// Records in interior page `pgno` that its child at slot `idx` (`target_child`) split into
    /// `target_child | promoted | new_right_child`, splitting `pgno` in turn when it overflows.
    fn insert_index_divider_parsed(
        &mut self,
        pgno: Pgno,
        idx: usize,
        target_child: Pgno,
        promoted: IndexLeafCell,
        new_right_child: Pgno,
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let usable = self.pager.usable_size();
        let ParsedPage::IndexInterior {
            mut cells,
            mut right_child,
        } = self.read_parsed_page(pgno)?
        else {
            return Err(SqliteError::corrupt("expected index page in insert_index"));
        };
        let divider = IndexInteriorCell {
            left_child: target_child,
            total_payload_len: promoted.total_payload_len,
            local_payload: promoted.local_payload,
            overflow_page: promoted.overflow_page,
        };
        if idx < cells.len() {
            cells[idx].left_child = new_right_child;
            cells.insert(idx, divider);
        } else {
            cells.push(divider);
            right_child = new_right_child;
        }
        let candidate = ParsedPage::IndexInterior { cells, right_child };
        if candidate.fits_in_page(pgno, usable) {
            self.write_parsed_page(pgno, &candidate)?;
            return Ok(None);
        }
        let ParsedPage::IndexInterior { cells, right_child } = candidate else {
            unreachable!()
        };
        self.split_index_interior(pgno, cells, right_child)
    }

    fn probe_fast_index_insert_raw(
        &mut self,
        root_pgno: Pgno,
        usable: u32,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<Option<(Pgno, usize, usize)>> {
        let key = RawRecordKey::for_index_orders(record_bytes, orders);
        let mut cur_pgno = root_pgno;
        for _ in 0..BTCURSOR_MAX_DEPTH {
            let raw = self.pager.read_page(cur_pgno)?;
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            if hdr_off >= raw.len() {
                return Ok(None);
            }
            match raw[hdr_off] {
                x if x == BtreePageType::IndexLeaf as u8 => {
                    if let Some((ins_idx, top)) =
                        probe_insert_index_leaf_raw(cur_pgno, usable, raw, &key, orders, enc)?
                    {
                        return Ok(Some((cur_pgno, ins_idx, top)));
                    }
                    let has_free = hdr_off + 8 <= raw.len()
                        && (raw[hdr_off + 1] != 0
                            || raw[hdr_off + 2] != 0
                            || raw[hdr_off + 7] != 0);
                    if has_free {
                        let raw_mut = self.pager.write_page(cur_pgno)?;
                        defragment_raw_leaf_in_place(cur_pgno, usable, raw_mut)?;
                        let raw_after = self.pager.read_page(cur_pgno)?;
                        return Ok(probe_insert_index_leaf_raw(
                            cur_pgno, usable, raw_after, &key, orders, enc,
                        )?
                        .map(|(ins_idx, top)| (cur_pgno, ins_idx, top)));
                    }
                    return Ok(None);
                }
                x if x == BtreePageType::IndexInterior as u8 => {
                    let Some(child) =
                        find_index_interior_child_raw(cur_pgno, usable, raw, &key, orders, enc)?
                    else {
                        return Ok(None);
                    };
                    cur_pgno = child;
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// Bulk-loads pre-sorted, strictly distinct serialized index records into the Index B-Tree
    /// rooted at `root_pgno` in a single bottom-up pass with zero page splits.
    pub fn bulk_load_sorted_index<R: AsRef<[u8]>>(
        &mut self,
        root_pgno: Pgno,
        sorted_records: &[R],
    ) -> Result<()> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;

        let usable = self.pager.usable_size();
        let usable_usize = usable as usize;
        {
            let raw = self.pager.read_page(root_pgno)?;
            let (ptype, n_cell, _) = raw_page_header(root_pgno, usable, raw)?;
            if ptype != BtreePageType::IndexLeaf || n_cell != 0 {
                self.clear_tree(root_pgno)?;
            }
        }
        let total = sorted_records.len();
        if total == 0 {
            return Ok(());
        }

        let bounds = PayloadBounds::for_index(usable);
        let max_local = bounds.max_local as usize;
        let auto_vac = self.is_auto_vacuum();

        let leaf_cell_encoded_len = |rec_len: usize| -> Result<usize> {
            let total_u32 = u32::try_from(rec_len)
                .map_err(|_| SqliteError::new(ErrorCode::TooBig, "row payload exceeds 4GiB"))?;
            let raw_len = if rec_len <= max_local {
                varint_len(rec_len as u64) + rec_len
            } else {
                let loc = bounds.local_len(total_u32);
                varint_len(rec_len as u64) + loc + 4
            };
            Ok(raw_len.max(4))
        };

        let mut leaf_sizes = Vec::with_capacity(total);
        let mut root_leaf_bytes: usize = (if root_pgno == 1 { 100usize } else { 0usize }) + 8;
        for rec in sorted_records {
            let clen = leaf_cell_encoded_len(rec.as_ref().len())?;
            leaf_sizes.push(clen);
            root_leaf_bytes = root_leaf_bytes.saturating_add(2 + clen);
        }

        if root_leaf_bytes <= usable_usize {
            return self.write_sorted_index_leaf_slice(
                root_pgno,
                usable,
                bounds,
                sorted_records,
                &leaf_sizes,
                auto_vac,
            );
        }

        let leaf_budget = ((usable_usize - 8) * 9) / 10;
        let mut dividers: Vec<(Pgno, usize)> = Vec::new();
        let mut rightmost_child: Pgno = 0;
        let mut pos = 0usize;
        while pos < total {
            let mut used = 2 + leaf_sizes[pos];
            let mut end = pos + 1;
            while end < total && used + 2 + leaf_sizes[end] <= leaf_budget {
                used += 2 + leaf_sizes[end];
                end += 1;
            }
            if end + 1 == total && end > pos + 1 {
                end -= 1;
            }
            let leaf_pgno = self.allocate_page()?;
            self.write_sorted_index_leaf_slice(
                leaf_pgno,
                usable,
                bounds,
                &sorted_records[pos..end],
                &leaf_sizes[pos..end],
                auto_vac,
            )?;
            if end < total {
                dividers.push((leaf_pgno, end));
                pos = end + 1;
            } else {
                rightmost_child = leaf_pgno;
                break;
            }
        }

        let int_cell_len = |rec_idx: usize| -> usize { 4 + leaf_sizes[rec_idx] };
        let root_int_hdr: usize = (if root_pgno == 1 { 100usize } else { 0usize }) + 12;
        let int_budget = usable_usize - 12;

        loop {
            let m = dividers.len();
            let mut root_int_bytes: usize = root_int_hdr;
            for &(_, ridx) in &dividers {
                root_int_bytes = root_int_bytes.saturating_add(2 + int_cell_len(ridx));
            }
            if root_int_bytes <= usable_usize {
                return self.write_sorted_index_interior_slice(
                    root_pgno,
                    usable,
                    bounds,
                    sorted_records,
                    &leaf_sizes,
                    &dividers,
                    rightmost_child,
                    auto_vac,
                );
            }

            let mut next_dividers: Vec<(Pgno, usize)> = Vec::new();
            let mut next_rightmost: Pgno = 0;
            let mut dpos = 0usize;
            while dpos < m {
                let mut used = 2 + int_cell_len(dividers[dpos].1);
                let mut dend = dpos + 1;
                while dend < m && used + 2 + int_cell_len(dividers[dend].1) <= int_budget {
                    used += 2 + int_cell_len(dividers[dend].1);
                    dend += 1;
                }
                if dend + 1 == m && dend > dpos + 1 {
                    dend -= 1;
                }
                let int_pgno = self.allocate_page()?;
                if dend < m {
                    let rchild = dividers[dend].0;
                    let promoted_ridx = dividers[dend].1;
                    self.write_sorted_index_interior_slice(
                        int_pgno,
                        usable,
                        bounds,
                        sorted_records,
                        &leaf_sizes,
                        &dividers[dpos..dend],
                        rchild,
                        auto_vac,
                    )?;
                    next_dividers.push((int_pgno, promoted_ridx));
                    dpos = dend + 1;
                } else {
                    self.write_sorted_index_interior_slice(
                        int_pgno,
                        usable,
                        bounds,
                        sorted_records,
                        &leaf_sizes,
                        &dividers[dpos..m],
                        rightmost_child,
                        auto_vac,
                    )?;
                    next_rightmost = int_pgno;
                    break;
                }
            }
            dividers = next_dividers;
            rightmost_child = next_rightmost;
        }
    }

    fn write_sorted_index_leaf_slice<R: AsRef<[u8]>>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        bounds: PayloadBounds,
        records: &[R],
        cell_sizes: &[usize],
        auto_vac: bool,
    ) -> Result<()> {
        let usable_usize = usable as usize;
        let max_local = bounds.max_local as usize;
        let mut ovfl_entries: Vec<(usize, Pgno)> = Vec::new();
        for (i, rec) in records.iter().enumerate() {
            if rec.as_ref().len() > max_local {
                let (_, _, Some(ovfl)) = self.prepare_cell_payload(bounds, rec.as_ref())? else {
                    unreachable!()
                };
                ovfl_entries.push((i, ovfl));
            }
        }

        let hdr_off = if pgno == 1 { 100 } else { 0 };
        let ptr_base = hdr_off + 8;
        let n_cell = records.len();
        {
            let raw = self.pager.write_page(pgno)?;
            raw[hdr_off..usable_usize].fill(0);
            let mut content_top = usable_usize;
            let mut ovfl_cursor = 0usize;
            for (i, (rec_ref, &clen)) in records.iter().zip(cell_sizes.iter()).enumerate() {
                let rec = rec_ref.as_ref();
                content_top -= clen;
                let ip = ptr_base + 2 * i;
                raw[ip..ip + 2].copy_from_slice(&(content_top as u16).to_be_bytes());
                let mut pos = content_top;
                pos += put_varint(&mut raw[pos..], rec.len() as u64);
                if rec.len() <= max_local {
                    raw[pos..pos + rec.len()].copy_from_slice(rec);
                } else {
                    let loc = bounds.local_len(rec.len() as u32);
                    raw[pos..pos + loc].copy_from_slice(&rec[..loc]);
                    pos += loc;
                    let ovfl_pg = ovfl_entries[ovfl_cursor].1;
                    ovfl_cursor += 1;
                    raw[pos..pos + 4].copy_from_slice(&ovfl_pg.to_be_bytes());
                }
            }
            raw[hdr_off] = BtreePageType::IndexLeaf as u8;
            raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(n_cell as u16).to_be_bytes());
            let encoded_top: u16 = if content_top == 65_536 {
                0
            } else {
                content_top as u16
            };
            raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        }

        if auto_vac {
            for &(_, ovfl_pg) in &ovfl_entries {
                self.ptrmap_put(ovfl_pg, PtrmapType::Overflow1, pgno)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn write_sorted_index_interior_slice<R: AsRef<[u8]>>(
        &mut self,
        pgno: Pgno,
        usable: u32,
        bounds: PayloadBounds,
        all_records: &[R],
        leaf_sizes: &[usize],
        dividers: &[(Pgno, usize)],
        right_child: Pgno,
        auto_vac: bool,
    ) -> Result<()> {
        let usable_usize = usable as usize;
        let max_local = bounds.max_local as usize;
        let mut ovfl_entries: Vec<(usize, Pgno)> = Vec::new();
        for (i, &(_, ridx)) in dividers.iter().enumerate() {
            let rec = all_records[ridx].as_ref();
            if rec.len() > max_local {
                let (_, _, Some(ovfl)) = self.prepare_cell_payload(bounds, rec)? else {
                    unreachable!()
                };
                ovfl_entries.push((i, ovfl));
            }
        }

        let hdr_off = if pgno == 1 { 100 } else { 0 };
        let ptr_base = hdr_off + 12;
        let n_cell = dividers.len();
        {
            let raw = self.pager.write_page(pgno)?;
            raw[hdr_off..usable_usize].fill(0);
            let mut content_top = usable_usize;
            let mut ovfl_cursor = 0usize;
            for (i, &(left_child, ridx)) in dividers.iter().enumerate() {
                let rec = all_records[ridx].as_ref();
                let clen = 4 + leaf_sizes[ridx];
                content_top -= clen;
                let ip = ptr_base + 2 * i;
                raw[ip..ip + 2].copy_from_slice(&(content_top as u16).to_be_bytes());
                raw[content_top..content_top + 4].copy_from_slice(&left_child.to_be_bytes());
                let mut pos = content_top + 4;
                pos += put_varint(&mut raw[pos..], rec.len() as u64);
                if rec.len() <= max_local {
                    raw[pos..pos + rec.len()].copy_from_slice(rec);
                } else {
                    let loc = bounds.local_len(rec.len() as u32);
                    raw[pos..pos + loc].copy_from_slice(&rec[..loc]);
                    pos += loc;
                    let ovfl_pg = ovfl_entries[ovfl_cursor].1;
                    ovfl_cursor += 1;
                    raw[pos..pos + 4].copy_from_slice(&ovfl_pg.to_be_bytes());
                }
            }
            raw[hdr_off] = BtreePageType::IndexInterior as u8;
            raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(n_cell as u16).to_be_bytes());
            let encoded_top: u16 = if content_top == 65_536 {
                0
            } else {
                content_top as u16
            };
            raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
            raw[hdr_off + 8..hdr_off + 12].copy_from_slice(&right_child.to_be_bytes());
        }

        if auto_vac {
            for &(left_child, _) in dividers {
                self.ptrmap_put(left_child, PtrmapType::BtreeNode, pgno)?;
            }
            self.ptrmap_put(right_child, PtrmapType::BtreeNode, pgno)?;
            for &(_, ovfl_pg) in &ovfl_entries {
                self.ptrmap_put(ovfl_pg, PtrmapType::Overflow1, pgno)?;
            }
        }
        Ok(())
    }

    fn probe_fast_index_insert_target(
        &mut self,
        root_pgno: Pgno,
        cell: &IndexLeafCell,
        key_vals: &[Value],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<Option<(Pgno, usize, usize)>> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        for _ in 0..BTCURSOR_MAX_DEPTH {
            let raw = self.pager.read_page(cur_pgno)?;
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            if hdr_off >= raw.len() {
                return Ok(None);
            }
            match raw[hdr_off] {
                x if x == BtreePageType::IndexLeaf as u8 => {
                    if let Some((ins_idx, top)) = probe_insert_index_leaf_in_place(
                        cur_pgno, usable, raw, cell, key_vals, orders, enc,
                    )? {
                        return Ok(Some((cur_pgno, ins_idx, top)));
                    }
                    let has_free = hdr_off + 8 <= raw.len()
                        && (raw[hdr_off + 1] != 0
                            || raw[hdr_off + 2] != 0
                            || raw[hdr_off + 7] != 0);
                    if has_free {
                        let raw_mut = self.pager.write_page(cur_pgno)?;
                        defragment_raw_leaf_in_place(cur_pgno, usable, raw_mut)?;
                        let raw_after = self.pager.read_page(cur_pgno)?;
                        return Ok(probe_insert_index_leaf_in_place(
                            cur_pgno, usable, raw_after, cell, key_vals, orders, enc,
                        )?
                        .map(|(ins_idx, top)| (cur_pgno, ins_idx, top)));
                    }
                    return Ok(None);
                }
                x if x == BtreePageType::IndexInterior as u8 => {
                    let Some(child) = find_index_interior_child_in_place(
                        cur_pgno, usable, raw, key_vals, orders, enc,
                    )?
                    else {
                        return Ok(None);
                    };
                    cur_pgno = child;
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    fn lower_bound_index_leaf_cells(
        &mut self,
        cells: &[IndexLeafCell],
        key_vals: &[Value],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<(usize, bool)> {
        let mut lo = 0usize;
        let mut hi = cells.len();
        let mut hi_is_equal = false;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let existing = &cells[mid];
            let rec = self.read_full_payload(
                existing.total_payload_len,
                &existing.local_payload,
                existing.overflow_page,
            )?;
            match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                Ordering::Less => lo = mid + 1,
                Ordering::Equal => {
                    hi = mid;
                    hi_is_equal = true;
                }
                Ordering::Greater => {
                    hi = mid;
                    hi_is_equal = false;
                }
            }
        }
        Ok((lo, hi_is_equal))
    }

    fn lower_bound_index_interior_cells(
        &mut self,
        cells: &[IndexInteriorCell],
        key_vals: &[Value],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<(usize, bool)> {
        let mut lo = 0usize;
        let mut hi = cells.len();
        let mut hi_is_equal = false;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let existing = &cells[mid];
            let rec = self.read_full_payload(
                existing.total_payload_len,
                &existing.local_payload,
                existing.overflow_page,
            )?;
            match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                Ordering::Less => lo = mid + 1,
                Ordering::Equal => {
                    hi = mid;
                    hi_is_equal = true;
                }
                Ordering::Greater => {
                    hi = mid;
                    hi_is_equal = false;
                }
            }
        }
        Ok((lo, hi_is_equal))
    }

    fn insert_index_rec(
        &mut self,
        pgno: Pgno,
        new_cell: IndexLeafCell,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let parsed = self.read_parsed_page(pgno)?;

        match parsed {
            ParsedPage::IndexLeaf { mut cells } => {
                let (pos, is_equal) =
                    self.lower_bound_index_leaf_cells(&cells, key_vals, orders, enc)?;
                if is_equal {
                    let old = std::mem::replace(&mut cells[pos], new_cell);
                    self.free_overflow_chain(old.overflow_page)?;
                    let updated = ParsedPage::IndexLeaf { cells };
                    if updated.fits_in_page(pgno, usable) {
                        self.write_parsed_page(pgno, &updated)?;
                        return Ok(None);
                    }
                    let ParsedPage::IndexLeaf { cells: c2 } = updated else {
                        unreachable!()
                    };
                    return self.split_index_leaf(pgno, c2);
                }
                cells.insert(pos, new_cell);
                let candidate = ParsedPage::IndexLeaf { cells };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }
                let ParsedPage::IndexLeaf { cells } = candidate else {
                    unreachable!()
                };
                self.split_index_leaf(pgno, cells)
            }
            ParsedPage::IndexInterior {
                mut cells,
                mut right_child,
            } => {
                let (idx, is_equal) =
                    self.lower_bound_index_interior_cells(&cells, key_vals, orders, enc)?;
                if is_equal {
                    let old_ovfl = cells[idx].overflow_page;
                    let left_child = cells[idx].left_child;
                    cells[idx] = IndexInteriorCell {
                        left_child,
                        total_payload_len: new_cell.total_payload_len,
                        local_payload: new_cell.local_payload,
                        overflow_page: new_cell.overflow_page,
                    };
                    self.free_overflow_chain(old_ovfl)?;
                    let updated = ParsedPage::IndexInterior { cells, right_child };
                    if updated.fits_in_page(pgno, usable) {
                        self.write_parsed_page(pgno, &updated)?;
                        return Ok(None);
                    }
                    let ParsedPage::IndexInterior {
                        cells: c2,
                        right_child: rc2,
                    } = updated
                    else {
                        unreachable!()
                    };
                    return self.split_index_interior(pgno, c2, rc2);
                }

                let target_child = if idx < cells.len() {
                    cells[idx].left_child
                } else {
                    right_child
                };
                let target_ptype = {
                    let raw = self.pager.read_page(target_child)?;
                    peek_page_type(target_child, usable, raw)?
                };

                if !cells.is_empty() && target_ptype == BtreePageType::IndexLeaf {
                    let ParsedPage::IndexLeaf {
                        cells: mut target_cells,
                    } = self.read_parsed_page(target_child)?
                    else {
                        return Err(SqliteError::corrupt("expected index leaf page"));
                    };
                    let (pos, replaced) =
                        self.lower_bound_index_leaf_cells(&target_cells, key_vals, orders, enc)?;
                    if replaced {
                        let old = std::mem::replace(&mut target_cells[pos], new_cell);
                        self.free_overflow_chain(old.overflow_page)?;
                    } else {
                        target_cells.insert(pos, new_cell);
                    }

                    let single_candidate = ParsedPage::IndexLeaf {
                        cells: target_cells,
                    };
                    if single_candidate.fits_in_page(target_child, usable) {
                        self.write_parsed_page(target_child, &single_candidate)?;
                        return Ok(None);
                    }
                    let ParsedPage::IndexLeaf {
                        cells: target_cells,
                    } = single_candidate
                    else {
                        unreachable!()
                    };

                    let m = cells.len();
                    let (nx_div, n_old) = if m < 2 {
                        (0usize, 2usize)
                    } else if idx == 0 {
                        (0usize, 3usize)
                    } else if idx == m {
                        (m - 2, 3usize)
                    } else {
                        (idx - 1, 3usize)
                    };

                    let mut avail_pgnos = Vec::with_capacity(n_old + 2);
                    let mut all_cells = Vec::new();
                    let mut target_opt = Some(target_cells);
                    for sib_slot in nx_div..(nx_div + n_old) {
                        let sib_pgno = cells.get(sib_slot).map_or(right_child, |c| c.left_child);
                        avail_pgnos.push(sib_pgno);
                        if sib_slot == idx {
                            all_cells.extend(target_opt.take().expect("single idx slot"));
                        } else {
                            let ParsedPage::IndexLeaf { cells: sib_cells } =
                                self.read_parsed_page(sib_pgno)?
                            else {
                                return Err(SqliteError::corrupt(
                                    "expected sibling index leaf page",
                                ));
                            };
                            all_cells.extend(sib_cells);
                        }
                        if sib_slot + 1 < nx_div + n_old
                            && let Some(div) = cells.get(sib_slot)
                        {
                            all_cells.push(IndexLeafCell {
                                total_payload_len: div.total_payload_len,
                                local_payload: div.local_payload.clone(),
                                overflow_page: div.overflow_page,
                            });
                        }
                    }

                    let (new_chunks, promoted_divs) =
                        balance_index_leaf_siblings(all_cells, usable);
                    let k = new_chunks.len();
                    while avail_pgnos.len() < k {
                        avail_pgnos.push(self.allocate_page()?);
                    }
                    while avail_pgnos.len() > k {
                        let extra_pg = avail_pgnos.pop().expect("len > k");
                        self.free_page(extra_pg)?;
                    }
                    avail_pgnos.sort_unstable();

                    for (&out_pgno, chunk) in avail_pgnos.iter().zip(new_chunks) {
                        self.write_parsed_page(out_pgno, &ParsedPage::IndexLeaf { cells: chunk })?;
                    }

                    let new_dividers =
                        promoted_divs
                            .into_iter()
                            .enumerate()
                            .map(|(j, div)| IndexInteriorCell {
                                left_child: avail_pgnos[j],
                                total_payload_len: div.total_payload_len,
                                local_payload: div.local_payload,
                                overflow_page: div.overflow_page,
                            });
                    if nx_div + n_old - 1 < m {
                        cells[nx_div + n_old - 1].left_child = avail_pgnos[k - 1];
                        cells.splice(nx_div..(nx_div + n_old - 1), new_dividers);
                    } else {
                        cells.splice(nx_div..m, new_dividers);
                        right_child = avail_pgnos[k - 1];
                    }
                } else {
                    let Some((promoted, new_right_child)) =
                        self.insert_index_rec(target_child, new_cell, key_vals, orders)?
                    else {
                        return Ok(None);
                    };

                    if idx < cells.len() {
                        cells[idx].left_child = new_right_child;
                        cells.insert(
                            idx,
                            IndexInteriorCell {
                                left_child: target_child,
                                total_payload_len: promoted.total_payload_len,
                                local_payload: promoted.local_payload,
                                overflow_page: promoted.overflow_page,
                            },
                        );
                    } else {
                        cells.push(IndexInteriorCell {
                            left_child: target_child,
                            total_payload_len: promoted.total_payload_len,
                            local_payload: promoted.local_payload,
                            overflow_page: promoted.overflow_page,
                        });
                        right_child = new_right_child;
                    }
                }

                let candidate = ParsedPage::IndexInterior { cells, right_child };
                if candidate.fits_in_page(pgno, usable) {
                    self.write_parsed_page(pgno, &candidate)?;
                    return Ok(None);
                }
                let ParsedPage::IndexInterior { cells, right_child } = candidate else {
                    unreachable!()
                };
                self.split_index_interior(pgno, cells, right_child)
            }
            _ => Err(SqliteError::corrupt("expected index page in insert_index")),
        }
    }

    fn split_index_leaf(
        &mut self,
        pgno: Pgno,
        mut cells: Vec<IndexLeafCell>,
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let mid = cells.len() / 2;
        let right_cells = cells.split_off(mid + 1);
        let promoted = cells.pop().expect("mid cell exists");

        let right_pgno = self.allocate_page()?;
        self.write_parsed_page(pgno, &ParsedPage::IndexLeaf { cells })?;
        self.write_parsed_page(right_pgno, &ParsedPage::IndexLeaf { cells: right_cells })?;
        Ok(Some((promoted, right_pgno)))
    }

    fn split_index_interior(
        &mut self,
        pgno: Pgno,
        mut cells: Vec<IndexInteriorCell>,
        orig_right: Pgno,
    ) -> Result<Option<(IndexLeafCell, Pgno)>> {
        let mid = cells.len() / 2;
        let right_cells = cells.split_off(mid + 1);
        let promoted = cells.pop().expect("mid cell exists");

        let left_page = ParsedPage::IndexInterior {
            cells,
            right_child: promoted.left_child,
        };
        let right_page = ParsedPage::IndexInterior {
            cells: right_cells,
            right_child: orig_right,
        };

        let right_pgno = self.allocate_page()?;
        self.write_parsed_page(pgno, &left_page)?;
        self.write_parsed_page(right_pgno, &right_page)?;
        Ok(Some((
            IndexLeafCell {
                total_payload_len: promoted.total_payload_len,
                local_payload: promoted.local_payload,
                overflow_page: promoted.overflow_page,
            },
            right_pgno,
        )))
    }

    /// Deletes a matching serialized entry `record_bytes` from an Index B-Tree rooted at `root_pgno`
    /// without decoding cells into heap `Value`s on the common leaf/divider path.
    pub fn delete_index_raw(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let usable_usize = usable as usize;
        let bounds = PayloadBounds::for_index(usable);
        let key = RawRecordKey::for_index_orders(record_bytes, orders);
        let mut cur_pgno = root_pgno;
        for _ in 0..BTCURSOR_MAX_DEPTH {
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            enum RawDelStep {
                Descend(Pgno),
                DividerHit {
                    cell_idx: usize,
                    left_child: Pgno,
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
                            match probe_index_interior_step_raw(
                                cur_pgno, usable, raw, &key, orders, enc,
                            )? {
                                Some(RawIndexInteriorStep::Child(c)) => RawDelStep::Descend(c),
                                Some(RawIndexInteriorStep::EqualAt {
                                    cell_idx,
                                    left_child,
                                }) => RawDelStep::DividerHit {
                                    cell_idx,
                                    left_child,
                                },
                                None => RawDelStep::Fallback,
                            }
                        }
                        x if x == BtreePageType::IndexLeaf as u8 => {
                            match probe_delete_index_leaf_raw(
                                cur_pgno,
                                usable,
                                raw,
                                &key,
                                orders,
                                enc,
                                cur_pgno == root_pgno,
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
                RawDelStep::Descend(child) => {
                    cur_pgno = child;
                }
                RawDelStep::LeafNotFound => return Ok(false),
                RawDelStep::LeafHit {
                    idx,
                    cell_off,
                    cell_len,
                } => {
                    let raw_mut = self.pager.write_page(cur_pgno)?;
                    apply_delete_index_leaf_in_place(
                        cur_pgno, usable, raw_mut, idx, cell_off, cell_len,
                    );
                    return Ok(true);
                }
                RawDelStep::DividerHit {
                    cell_idx,
                    left_child,
                } => {
                    // Walk right spine of `left_child` using raw headers without unpacking interior/leaf cells.
                    let mut pred_pgno = left_child;
                    let mut pred_extract: Option<(usize, usize, usize, Vec<u8>)> = None;
                    for _ in 0..BTCURSOR_MAX_DEPTH {
                        let raw_p = self.pager.read_page(pred_pgno)?;
                        let (ptype, n_cell, rc) = raw_page_header(pred_pgno, usable, raw_p)?;
                        match ptype {
                            BtreePageType::IndexInterior => {
                                if rc == 0 {
                                    break;
                                }
                                pred_pgno = rc;
                            }
                            BtreePageType::IndexLeaf => {
                                let p_hdr = if pred_pgno == 1 { 100 } else { 0 };
                                if n_cell >= 2 && raw_p[p_hdr + 7] <= 60 {
                                    let last_idx = n_cell - 1;
                                    let off = raw_cell_offset(
                                        pred_pgno, usable, raw_p, 8, n_cell, last_idx,
                                    )?;
                                    let (payload_u64, n1) = get_varint(&raw_p[off..usable_usize])?;
                                    if let Ok(total_u32) = u32::try_from(payload_u64) {
                                        let loc = bounds.local_len(total_u32);
                                        if (loc as u32) == total_u32
                                            && off + (n1 + loc).max(4) <= usable_usize
                                        {
                                            let c_len = (n1 + loc).max(4);
                                            let payload = raw_p[off + n1..off + n1 + loc].to_vec();
                                            pred_extract = Some((last_idx, off, c_len, payload));
                                        }
                                    }
                                }
                                break;
                            }
                            _ => break,
                        }
                    }
                    if let Some((pred_idx, pred_off, pred_cell_len, pred_payload)) = pred_extract
                        && let ParsedPage::IndexInterior {
                            mut cells,
                            right_child,
                        } = self.read_parsed_page(cur_pgno)?
                        && cell_idx < cells.len()
                    {
                        let old_div = std::mem::replace(
                            &mut cells[cell_idx],
                            IndexInteriorCell {
                                left_child,
                                total_payload_len: pred_payload.len() as u32,
                                local_payload: pred_payload,
                                overflow_page: None,
                            },
                        );
                        let cand_int = ParsedPage::IndexInterior { cells, right_child };
                        if cand_int.fits_in_page(cur_pgno, usable) {
                            self.free_overflow_chain(old_div.overflow_page)?;
                            {
                                let raw_pred_mut = self.pager.write_page(pred_pgno)?;
                                apply_delete_index_leaf_in_place(
                                    pred_pgno,
                                    usable,
                                    raw_pred_mut,
                                    pred_idx,
                                    pred_off,
                                    pred_cell_len,
                                );
                            }
                            self.write_parsed_page(cur_pgno, &cand_int)?;
                            return Ok(true);
                        }
                    }
                    break;
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
        self.delete_index(root_pgno, &vals[..key_len], orders)
    }

    /// Deletes a matching entry `key_vals` from an Index B-Tree rooted at `root_pgno`.
    pub fn delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<bool> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        // Simple, rock-solid delete maintaining 100% of SQLite invariants:
        // Locate exact match; if in leaf with >1 cells (or root leaf), remove in place!
        // If structural underflow occurs (rare non-root empty leaf or interior hit), rebuild or rebalance cleanly.
        if let Some(done_fast) = self.try_fast_leaf_delete_index(root_pgno, key_vals, orders)? {
            return Ok(done_fast);
        }

        // Structural delete (interior cell hit or 1-cell leaf underflow): collect remaining entries and rewrite cleanly!
        let enc = self.encoding();
        let all_records = self.scan_index(root_pgno)?;
        let mut remaining = Vec::with_capacity(all_records.len());
        let mut removed = false;
        for rec in all_records {
            if !removed
                && compare_records_with_encoding(&rec, key_vals, orders, enc)? == Ordering::Equal
            {
                removed = true;
                continue;
            }
            remaining.push(rec);
        }
        if !removed {
            return Ok(false);
        }
        self.bulk_load_sorted_index(root_pgno, &remaining)?;
        Ok(true)
    }

    fn try_fast_leaf_delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Option<bool>> {
        let enc = self.encoding();
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;
        loop {
            let hdr_off = if cur_pgno == 1 { 100 } else { 0 };
            let raw_probe = {
                let raw = self.pager.read_page(cur_pgno)?;
                if hdr_off < raw.len() {
                    match raw[hdr_off] {
                        x if x == BtreePageType::IndexInterior as u8 => {
                            if let Some(child) = find_index_interior_child_in_place(
                                cur_pgno, usable, raw, key_vals, orders, enc,
                            )? {
                                cur_pgno = child;
                                continue;
                            }
                            None
                        }
                        x if x == BtreePageType::IndexLeaf as u8 => {
                            probe_delete_index_leaf_in_place(
                                cur_pgno,
                                usable,
                                raw,
                                key_vals,
                                orders,
                                enc,
                                cur_pgno == root_pgno,
                            )?
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            };
            if let Some(leaf_outcome) = raw_probe {
                return match leaf_outcome {
                    None => Ok(Some(false)),
                    Some((idx, cell_off, cell_len, ovfl)) => {
                        self.free_overflow_chain(ovfl)?;
                        let raw_mut = self.pager.write_page(cur_pgno)?;
                        apply_delete_index_leaf_in_place(
                            cur_pgno, usable, raw_mut, idx, cell_off, cell_len,
                        );
                        Ok(Some(true))
                    }
                };
            }

            let parsed = self.read_parsed_page(cur_pgno)?;
            match parsed {
                ParsedPage::IndexLeaf { mut cells } => {
                    let mut found_idx = None;
                    for (i, c) in cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => continue,
                            Ordering::Equal => {
                                found_idx = Some(i);
                                break;
                            }
                            Ordering::Greater => break,
                        }
                    }
                    let Some(idx) = found_idx else {
                        return Ok(Some(false));
                    };
                    if cells.len() == 1 && cur_pgno != root_pgno {
                        // Delegate non-root empty-leaf rebalancing to structural path
                        return Ok(None);
                    }
                    let old = cells.remove(idx);
                    self.free_overflow_chain(old.overflow_page)?;
                    self.write_parsed_page(cur_pgno, &ParsedPage::IndexLeaf { cells })?;
                    return Ok(Some(true));
                }
                ParsedPage::IndexInterior { cells, right_child } => {
                    let mut next_child = right_child;
                    for (ci, c) in cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            c.total_payload_len,
                            &c.local_payload,
                            c.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => continue,
                            Ordering::Equal => {
                                // Replace interior divider cell in-place with its in-order predecessor
                                // from the rightmost leaf of `c.left_child` (`btree.c:sqlite3BtreeDelete`)
                                // whenever that leaf has >= 2 cells and the replacement divider fits!
                                let div_left_child = c.left_child;
                                let mut pred_pgno = div_left_child;
                                let pred_leaf = loop {
                                    match self.read_parsed_page(pred_pgno)? {
                                        ParsedPage::IndexInterior {
                                            right_child: rc, ..
                                        } => pred_pgno = rc,
                                        ParsedPage::IndexLeaf { cells: pcells } => break pcells,
                                        _ => {
                                            return Err(SqliteError::corrupt(
                                                "expected index page in predecessor walk",
                                            ));
                                        }
                                    }
                                };
                                if pred_leaf.len() >= 2 {
                                    let mut pred_cells = pred_leaf;
                                    let pred = pred_cells.pop().expect("len >= 2");
                                    let mut new_int_cells = cells;
                                    let old_div = std::mem::replace(
                                        &mut new_int_cells[ci],
                                        IndexInteriorCell {
                                            left_child: div_left_child,
                                            total_payload_len: pred.total_payload_len,
                                            local_payload: pred.local_payload,
                                            overflow_page: pred.overflow_page,
                                        },
                                    );
                                    let cand_int = ParsedPage::IndexInterior {
                                        cells: new_int_cells,
                                        right_child,
                                    };
                                    if cand_int.fits_in_page(cur_pgno, self.pager.usable_size()) {
                                        self.free_overflow_chain(old_div.overflow_page)?;
                                        self.write_parsed_page(
                                            pred_pgno,
                                            &ParsedPage::IndexLeaf { cells: pred_cells },
                                        )?;
                                        self.write_parsed_page(cur_pgno, &cand_int)?;
                                        return Ok(Some(true));
                                    }
                                }
                                return Ok(None);
                            }
                            Ordering::Greater => {
                                next_child = c.left_child;
                                break;
                            }
                        }
                    }
                    cur_pgno = next_child;
                }
                _ => return Err(SqliteError::corrupt("expected index page")),
            }
        }
    }

    // ========================================================================
    // PRAGMA integrity_check (`sqlite3BtreeIntegrityCheck`)
    // ========================================================================

    /// Verifies that every page `1..=page_count` is referenced exactly once across `roots`
    /// (which must include `1`), their child pages, their overflow chains, and the freelist!
    /// Returns an empty `Vec` when healthy (`"ok"`), or human-readable corruption diagnostics.
    pub fn integrity_check(&mut self, roots: &[Pgno]) -> Result<Vec<String>> {
        let total_pages = self.pager.page_count();
        if total_pages == 0 {
            return Ok(Vec::new());
        }

        let mut seen = Bitvec::new(total_pages);
        let mut errors = Vec::new();
        let usable_u32 = self.pager.usable_size();
        let usable = usable_u32 as usize;
        let lock_byte_pgno = self.lock_byte_page();

        let max_root_in_list = roots.iter().copied().max().unwrap_or(1);
        let hdr_max_root = self.pager.header().largest_root_btree_page;
        let hdr_incr = self.pager.header().incremental_vacuum_mode;
        let auto_vacuum = hdr_max_root > 0;

        if hdr_max_root > 0 && max_root_in_list != hdr_max_root {
            errors.push(format!(
                "max rootpage ({max_root_in_list}) disagrees with header ({hdr_max_root})"
            ));
        }
        if hdr_max_root == 0 && hdr_incr != 0 {
            errors.push("incremental_vacuum enabled with a max rootpage of zero".to_string());
        }
        if hdr_max_root > 0 && hdr_incr > 1 {
            errors.push(format!("invalid incremental_vacuum value ({hdr_incr})"));
        }

        #[allow(clippy::too_many_arguments)]
        fn mark_page(
            seen: &mut Bitvec,
            total_pages: Pgno,
            usable_u32: u32,
            lock_byte_pgno: Pgno,
            auto_vacuum: bool,
            pgno: Pgno,
            label: &str,
            errs: &mut Vec<String>,
        ) -> bool {
            if pgno == 0 || pgno > total_pages {
                errs.push(format!("{label} references out-of-range page {pgno}"));
                false
            } else if pgno == lock_byte_pgno {
                errs.push(format!("{label} references lock-byte page {pgno}"));
                false
            } else if auto_vacuum && is_ptrmap_page(pgno, usable_u32, lock_byte_pgno) {
                errs.push(format!("Page {pgno}: pointer map referenced"));
                false
            } else if let Ok(newly_set) = seen.set(pgno)
                && !newly_set
            {
                errs.push(format!("2nd reference to page {pgno} ({label})"));
                false
            } else {
                true
            }
        }

        fn check_ptrmap(
            btree: &mut Btree,
            pgno: Pgno,
            expected_type: PtrmapType,
            expected_parent: Pgno,
            errs: &mut Vec<String>,
        ) {
            if pgno < 2 {
                return;
            }
            match btree.ptrmap_get(pgno) {
                Ok((actual_type, actual_parent)) => {
                    if actual_type != expected_type || actual_parent != expected_parent {
                        errs.push(format!(
                            "Bad ptr map entry key={pgno} expected=({},{expected_parent}) got=({},{actual_parent})",
                            expected_type as u8, actual_type as u8
                        ));
                    }
                }
                Err(_) => {
                    errs.push(format!("Failed to read ptrmap key={pgno}"));
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn walk_ovfl(
            mut ovfl: Option<Pgno>,
            owner_pgno: Pgno,
            total_pages: Pgno,
            usable_u32: u32,
            lock_byte_pgno: Pgno,
            auto_vacuum: bool,
            seen: &mut Bitvec,
            btree: &mut Btree,
            errs: &mut Vec<String>,
        ) -> Result<()> {
            let mut expected_type = PtrmapType::Overflow1;
            let mut expected_parent = owner_pgno;
            while let Some(opg) = ovfl.filter(|&p| p != 0) {
                btree.check_interrupted()?;
                if opg > total_pages {
                    errs.push(format!("overflow page {opg} out of range"));
                    break;
                }
                if opg == lock_byte_pgno {
                    errs.push(format!("overflow page {opg} references lock-byte page"));
                    break;
                }
                if auto_vacuum && is_ptrmap_page(opg, usable_u32, lock_byte_pgno) {
                    errs.push(format!("Page {opg}: pointer map referenced"));
                    break;
                }
                if auto_vacuum {
                    check_ptrmap(btree, opg, expected_type, expected_parent, errs);
                }
                if let Ok(newly) = seen.set(opg)
                    && !newly
                {
                    errs.push(format!("2nd reference to overflow page {opg}"));
                    break;
                }
                let r = btree.pager.read_page(opg)?;
                let nxt = u32::from_be_bytes([r[0], r[1], r[2], r[3]]);
                expected_type = PtrmapType::Overflow2;
                expected_parent = opg;
                ovfl = if nxt == 0 { None } else { Some(nxt) };
            }
            Ok(())
        }

        // 1. Walk freelist
        let mut trunk = self.pager.header().first_freelist_trunk_page;
        let mut counted_free = 0u32;
        while trunk != 0 {
            self.check_interrupted()?;
            let trunk_valid = mark_page(
                &mut seen,
                total_pages,
                usable_u32,
                lock_byte_pgno,
                auto_vacuum,
                trunk,
                "freelist trunk",
                &mut errors,
            );
            counted_free += 1;
            if !trunk_valid {
                break;
            }
            if auto_vacuum {
                check_ptrmap(self, trunk, PtrmapType::FreePage, 0, &mut errors);
            }
            let raw = self.pager.read_page(trunk)?.to_vec();
            let next_trunk = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let n_leaves = u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]);
            for i in 0..n_leaves as usize {
                let off = 8 + 4 * i;
                if off + 4 > usable {
                    errors.push(format!("freelist trunk {trunk} leaf count corrupt"));
                    break;
                }
                let leaf = u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
                let leaf_valid = mark_page(
                    &mut seen,
                    total_pages,
                    usable_u32,
                    lock_byte_pgno,
                    auto_vacuum,
                    leaf,
                    "freelist leaf",
                    &mut errors,
                );
                if leaf_valid && auto_vacuum {
                    check_ptrmap(self, leaf, PtrmapType::FreePage, 0, &mut errors);
                }
                counted_free += 1;
            }
            trunk = next_trunk;
        }
        if counted_free != self.pager.header().total_freelist_pages {
            errors.push(format!(
                "freelist_count mismatch: header says {}, found {counted_free}",
                self.pager.header().total_freelist_pages
            ));
        }

        // 2. Walk every B-tree root
        let mut stack: Vec<(Pgno, Option<Pgno>)> = roots.iter().map(|&r| (r, None)).collect();
        while let Some((pgno, parent_opt)) = stack.pop() {
            self.check_interrupted()?;
            let is_root = parent_opt.is_none();
            let page_valid = mark_page(
                &mut seen,
                total_pages,
                usable_u32,
                lock_byte_pgno,
                auto_vacuum,
                pgno,
                "btree page",
                &mut errors,
            );
            if !page_valid {
                continue;
            }
            if auto_vacuum && pgno > 1 {
                match parent_opt {
                    None => check_ptrmap(self, pgno, PtrmapType::RootPage, 0, &mut errors),
                    Some(parent_pgno) => {
                        check_ptrmap(self, pgno, PtrmapType::BtreeNode, parent_pgno, &mut errors)
                    }
                }
            }
            let parsed = match self.read_parsed_page(pgno) {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!("page {pgno}: {e}"));
                    continue;
                }
            };
            if !is_root && parsed.cell_count() == 0 {
                errors.push(format!("non-root btree page {pgno} has 0 cells"));
            }

            match parsed {
                ParsedPage::TableLeaf { cells } => {
                    for w in cells.windows(2) {
                        if w[0].rowid >= w[1].rowid {
                            errors.push(format!("rowids out of order on table leaf {pgno}"));
                            break;
                        }
                    }
                    for c in cells {
                        walk_ovfl(
                            c.overflow_page,
                            pgno,
                            total_pages,
                            usable_u32,
                            lock_byte_pgno,
                            auto_vacuum,
                            &mut seen,
                            self,
                            &mut errors,
                        )?;
                    }
                }
                ParsedPage::TableInterior { cells, right_child } => {
                    for w in cells.windows(2) {
                        if w[0].key >= w[1].key {
                            errors.push(format!("keys out of order on table interior {pgno}"));
                            break;
                        }
                    }
                    for c in cells {
                        stack.push((c.left_child, Some(pgno)));
                    }
                    stack.push((right_child, Some(pgno)));
                }
                ParsedPage::IndexLeaf { cells } => {
                    for c in cells {
                        walk_ovfl(
                            c.overflow_page,
                            pgno,
                            total_pages,
                            usable_u32,
                            lock_byte_pgno,
                            auto_vacuum,
                            &mut seen,
                            self,
                            &mut errors,
                        )?;
                    }
                }
                ParsedPage::IndexInterior { cells, right_child } => {
                    for c in cells {
                        walk_ovfl(
                            c.overflow_page,
                            pgno,
                            total_pages,
                            usable_u32,
                            lock_byte_pgno,
                            auto_vacuum,
                            &mut seen,
                            self,
                            &mut errors,
                        )?;
                        stack.push((c.left_child, Some(pgno)));
                    }
                    stack.push((right_child, Some(pgno)));
                }
            }
        }

        // 3. Check for unreferenced pages
        for pgno in 1..=total_pages {
            if pgno == lock_byte_pgno
                || (auto_vacuum && is_ptrmap_page(pgno, usable_u32, lock_byte_pgno))
            {
                continue;
            }
            if !seen.test(pgno) {
                errors.push(format!("Page {pgno} is never used"));
            }
        }

        Ok(errors)
    }
}

/// Byte range of one serialized index cell inside a scratch arena. `len` is the unpadded cell
/// length (`[left_child(4)] varint(payload) local_payload [overflow_pgno(4)]`).
#[derive(Debug, Clone, Copy)]
struct RawCellRef {
    off: u32,
    len: u32,
}

impl RawCellRef {
    /// On-page footprint as a leaf cell (cells are padded to the 4-byte freeblock minimum).
    #[inline]
    fn leaf_size(self) -> usize {
        (self.len as usize).max(4)
    }

    /// An interior cell minus its leading 4-byte left-child pointer, i.e. its leaf-cell form.
    #[inline]
    fn without_left_child(self) -> Self {
        Self {
            off: self.off + 4,
            len: self.len - 4,
        }
    }
}

#[inline]
fn raw_cell_left_child(arena: &[u8], cell: RawCellRef) -> Pgno {
    let o = cell.off as usize;
    u32::from_be_bytes([arena[o], arena[o + 1], arena[o + 2], arena[o + 3]])
}

/// Returns the record bytes of a leaf-form cell, or `None` when part of it lives on overflow
/// pages (or the cell is malformed).
#[inline]
fn raw_leaf_cell_local_record(arena: &[u8], cell: RawCellRef) -> Option<&[u8]> {
    let body = arena.get(cell.off as usize..(cell.off + cell.len) as usize)?;
    let (payload_u64, n1) = get_varint(body).ok()?;
    if payload_u64 != (body.len() - n1) as u64 {
        return None;
    }
    Some(&body[n1..])
}

/// Appends a [`RawCellRef`] for every cell of the index page copied at `arena[base..base+usable]`
/// (never page 1) and returns the cell count; `None` if any pointer or cell is out of bounds,
/// mirroring the checks of [`ParsedPage::parse`].
fn collect_raw_index_cells(
    arena: &[u8],
    base: usize,
    usable: usize,
    interior: bool,
    bounds: PayloadBounds,
    out: &mut Vec<RawCellRef>,
) -> Option<usize> {
    let page = arena.get(base..base + usable)?;
    let (hdr, lead) = if interior { (12, 4) } else { (8, 0) };
    if usable < hdr {
        return None;
    }
    let n_cell = u16::from_be_bytes([page[3], page[4]]) as usize;
    let ptr_end = hdr + 2 * n_cell;
    if ptr_end > usable {
        return None;
    }
    out.reserve(n_cell + 2);
    for i in 0..n_cell {
        let p = hdr + 2 * i;
        let off = u16::from_be_bytes([page[p], page[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return None;
        }
        let slice = &page[off..];
        if slice.len() <= lead {
            return None;
        }
        let (payload_u64, n1) = get_varint(&slice[lead..]).ok()?;
        let total = u32::try_from(payload_u64).ok()?;
        let local_len = bounds.local_len(total);
        let ovfl = if (local_len as u32) < total { 4 } else { 0 };
        let needed = lead + n1 + local_len + ovfl;
        if needed > slice.len() {
            return None;
        }
        out.push(RawCellRef {
            off: (base + off) as u32,
            len: needed as u32,
        });
    }
    Some(n_cell)
}

/// Serializes `cells` as a clean `IndexLeaf` page (same layout as [`ParsedPage::write`]).
/// The caller has verified that the cells fit.
fn write_raw_index_leaf(raw: &mut [u8], usable: usize, arena: &[u8], cells: &[RawCellRef]) {
    raw[..usable].fill(0);
    let mut top = usable;
    for (i, c) in cells.iter().enumerate() {
        let len = c.len as usize;
        top -= c.leaf_size();
        raw[top..top + len].copy_from_slice(&arena[c.off as usize..c.off as usize + len]);
        let p = 8 + 2 * i;
        raw[p..p + 2].copy_from_slice(&(top as u16).to_be_bytes());
    }
    raw[0] = BtreePageType::IndexLeaf as u8;
    raw[3..5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    let encoded_top: u16 = if top == 65_536 { 0 } else { top as u16 };
    raw[5..7].copy_from_slice(&encoded_top.to_be_bytes());
}

/// Serializes `(left_child, leaf-form cell)` pairs as a clean `IndexInterior` page (same layout
/// as [`ParsedPage::write`]). The caller has verified that the cells fit.
fn write_raw_index_interior(
    raw: &mut [u8],
    usable: usize,
    arena: &[u8],
    cells: &[(Pgno, RawCellRef)],
    right_child: Pgno,
) {
    raw[..usable].fill(0);
    raw[8..12].copy_from_slice(&right_child.to_be_bytes());
    let mut top = usable;
    for (i, &(left_child, c)) in cells.iter().enumerate() {
        let len = c.len as usize;
        top -= 4 + len;
        raw[top..top + 4].copy_from_slice(&left_child.to_be_bytes());
        raw[top + 4..top + 4 + len].copy_from_slice(&arena[c.off as usize..c.off as usize + len]);
        let p = 12 + 2 * i;
        raw[p..p + 2].copy_from_slice(&(top as u16).to_be_bytes());
    }
    raw[0] = BtreePageType::IndexInterior as u8;
    raw[3..5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    let encoded_top: u16 = if top == 65_536 { 0 } else { top as u16 };
    raw[5..7].copy_from_slice(&encoded_top.to_be_bytes());
}

/// Distributes `n` index cells (leaf footprint `enc_size(i)`) over sibling leaves: element `i`
/// of the result is the exclusive end of leaf `i`, the cell at that position (for every leaf but
/// the last) becomes the parent divider, and the next leaf starts right after it.
fn index_leaf_balance_counts<F: Fn(usize) -> usize>(
    n: usize,
    enc_size: F,
    usable: u32,
) -> Vec<usize> {
    let usable_space = (usable as usize).saturating_sub(8);
    let mut cnt_new: Vec<usize> = Vec::with_capacity(5);
    let mut sz_new: Vec<usize> = Vec::with_capacity(5);

    let mut start = 0usize;
    while start < n {
        let mut cur_sz = 0usize;
        let mut j = start;
        while j < n {
            let w = enc_size(j) + 2;
            if j > start && cur_sz + w > usable_space {
                break;
            }
            cur_sz += w;
            j += 1;
        }
        cnt_new.push(j);
        sz_new.push(cur_sz);
        if j == n {
            break;
        }
        start = j + 1;
        if start == n {
            cnt_new.push(n);
            sz_new.push(0);
            break;
        }
    }

    let k = cnt_new.len();
    for i in (1..k).rev() {
        let mut sz_right = sz_new[i];
        let mut sz_left = sz_new[i - 1];
        let min_left_start = if i > 1 { cnt_new[i - 2] + 1 } else { 0 };
        let mut d = cnt_new[i - 1];
        let pad = if i == k - 1 { 0 } else { 2 };
        while d > min_left_start + 1 {
            let r = d - 1;
            let sz_r = enc_size(r);
            let sz_d = enc_size(d);
            if sz_right != 0
                && (sz_right + sz_d + 2 > sz_left.saturating_sub(sz_r + pad)
                    || sz_right + sz_d + 2 > usable_space)
            {
                break;
            }
            sz_right += sz_d + 2;
            sz_left -= sz_r + 2;
            d = r;
            cnt_new[i - 1] = d;
        }
        sz_new[i] = sz_right;
        sz_new[i - 1] = sz_left;
    }
    cnt_new
}

fn balance_index_leaf_siblings(
    cells: Vec<IndexLeafCell>,
    usable: u32,
) -> (Vec<Vec<IndexLeafCell>>, Vec<IndexLeafCell>) {
    let cnt_new = index_leaf_balance_counts(cells.len(), |i| cells[i].encoded_size(), usable);
    let k = cnt_new.len();

    let mut chunks = Vec::with_capacity(k);
    let mut dividers = Vec::with_capacity(k.saturating_sub(1));
    let mut rest = cells;
    let mut prev_start = 0usize;
    for (idx, end) in cnt_new.into_iter().enumerate() {
        let take_len = end - prev_start;
        let mut tail = rest.split_off(take_len);
        chunks.push(rest);
        if idx + 1 < k {
            let after_div = tail.split_off(1);
            dividers.push(tail.into_iter().next().expect("divider cell exists"));
            rest = after_div;
            prev_start = end + 1;
        } else {
            rest = tail;
            prev_start = end;
        }
    }
    (chunks, dividers)
}

fn balance_table_leaf_siblings(cells: Vec<TableLeafCell>, usable: u32) -> Vec<Vec<TableLeafCell>> {
    let usable_space = (usable as usize).saturating_sub(8);
    let mut cnt_new: Vec<usize> = Vec::with_capacity(5);
    let mut sz_new: Vec<usize> = Vec::with_capacity(5);

    let mut cur_sz = 0usize;
    for (idx, c) in cells.iter().enumerate() {
        let w = c.encoded_size() + 2;
        if idx > *cnt_new.last().unwrap_or(&0) && cur_sz + w > usable_space {
            cnt_new.push(idx);
            sz_new.push(cur_sz);
            cur_sz = 0;
        }
        cur_sz += w;
    }
    cnt_new.push(cells.len());
    sz_new.push(cur_sz);

    let k = cnt_new.len();
    for i in (1..k).rev() {
        let mut sz_right = sz_new[i];
        let mut sz_left = sz_new[i - 1];
        let min_left = if i > 1 { cnt_new[i - 2] } else { 0 };
        let mut r = cnt_new[i - 1];
        let pad = if i == k - 1 { 0 } else { 2 };
        while r > min_left + 1 {
            let sz_r = cells[r - 1].encoded_size();
            if sz_right != 0
                && (sz_right + sz_r + 2 > sz_left.saturating_sub(sz_r + pad)
                    || sz_right + sz_r + 2 > usable_space)
            {
                break;
            }
            sz_right += sz_r + 2;
            sz_left -= sz_r + 2;
            r -= 1;
            cnt_new[i - 1] = r;
        }
        sz_new[i] = sz_right;
        sz_new[i - 1] = sz_left;
    }

    let mut chunks = Vec::with_capacity(k);
    let mut rest = cells;
    let mut prev = 0usize;
    for end in cnt_new {
        let take_len = end - prev;
        let tail = rest.split_off(take_len);
        chunks.push(rest);
        rest = tail;
        prev = end;
    }
    chunks
}

fn partition_table_leaf_cells(
    mut cells: Vec<TableLeafCell>,
    first_pgno: Pgno,
    usable: u32,
    use_quick_balance: bool,
) -> Vec<Vec<TableLeafCell>> {
    let first_hdr = if first_pgno == 1 { 100 + 8 } else { 8 };
    let first_cap = (usable as usize).saturating_sub(first_hdr);
    let other_cap = (usable as usize).saturating_sub(8);

    let mut prefix = Vec::with_capacity(cells.len() + 1);
    prefix.push(0usize);
    for c in &cells {
        prefix.push(prefix.last().unwrap() + c.encoded_size() + 2);
    }
    let total = *prefix.last().unwrap();

    if use_quick_balance && cells.len() >= 2 {
        let split = cells.len() - 1;
        let left_bytes = prefix[split];
        let right_bytes = total - left_bytes;
        if left_bytes <= first_cap && right_bytes <= other_cap {
            let right = cells.split_off(split);
            return vec![cells, right];
        }
    }

    let mut best_split: Option<(usize, usize)> = None;
    for (s, left_bytes) in prefix.iter().copied().enumerate().take(cells.len()).skip(1) {
        let right_bytes = total - left_bytes;
        if left_bytes <= first_cap && right_bytes <= other_cap {
            let diff = left_bytes.abs_diff(right_bytes);
            if best_split.is_none_or(|(_, best_diff)| diff < best_diff) {
                best_split = Some((s, diff));
            }
        }
    }
    if let Some((s, _)) = best_split {
        let right = cells.split_off(s);
        return vec![cells, right];
    }

    // Multi-way split (`NB = 3` in `balance_nonroot`) when no 2-way split fits in `usable`.
    let mut chunks: Vec<Vec<TableLeafCell>> = Vec::with_capacity(3);
    let mut cur_chunk: Vec<TableLeafCell> = Vec::new();
    let mut cur_bytes = 0usize;
    for c in cells {
        let w = c.encoded_size() + 2;
        let cap = if chunks.is_empty() {
            first_cap
        } else {
            other_cap
        };
        if !cur_chunk.is_empty() && cur_bytes + w > cap {
            chunks.push(std::mem::take(&mut cur_chunk));
            cur_bytes = 0;
        }
        cur_bytes += w;
        cur_chunk.push(c);
    }
    if !cur_chunk.is_empty() {
        chunks.push(cur_chunk);
    }
    chunks
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

    /// The raw sibling rebalance must leave every page byte-identical to the parsed path, across
    /// leaf splits, parent splits, root growth, short (padded) cells and overflow cells.
    #[test]
    fn test_index_raw_balance_matches_parsed_path_byte_for_byte() {
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
            let mut parsed_bt = Btree::open_in_memory().unwrap();
            raw_bt.pager_mut().set_page_size(page_size).unwrap();
            parsed_bt.pager_mut().set_page_size(page_size).unwrap();
            raw_bt.begin_write().unwrap();
            parsed_bt.begin_write().unwrap();
            let raw_root = raw_bt.create_index().unwrap();
            let parsed_root = parsed_bt.create_index().unwrap();
            assert_eq!(raw_root, parsed_root);
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
                parsed_bt
                    .insert_index_impl(parsed_root, &rec, ord, false)
                    .unwrap();
            }

            let n_pages = raw_bt.pager().page_count();
            assert_eq!(n_pages, parsed_bt.pager().page_count());
            assert!(
                n_pages > 8,
                "expected a multi-level tree, got {n_pages} pages"
            );
            for pgno in 1..=n_pages {
                let a = raw_bt.pager_mut().read_page(pgno).unwrap().to_vec();
                let b = parsed_bt.pager_mut().read_page(pgno).unwrap().to_vec();
                assert!(
                    a == b,
                    "page {pgno} differs (page_size {page_size}, mode {mode})"
                );
            }
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

    /// Size-changing row replacements repacked on the raw page, and replacements / deletions
    /// addressed through a positioned cursor, must leave every page byte-identical to the
    /// general (descend, search, parse) path — across full leaves, freeblocks, short padded
    /// cells, overflow rows, splits, emptied leaves, Page-1 roots and auto-vacuum.
    #[test]
    fn test_table_repack_and_positioned_ops_match_general_path_byte_for_byte() {
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
            (0..len).map(|j| (r >> (j % 56)) as u8).collect()
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
                general
                    .insert_table_impl(root, rowid, &rec, None, false)
                    .unwrap();
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
                // A stale / wrong position must be harmless.
                let position = match (position, r % 97) {
                    (Some((pg, idx)), 0) => Some((pg, idx + 1)),
                    (Some((pg, idx)), 1) => Some((pg + 1, idx)),
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
                    general
                        .insert_table_impl(root, rowid, &rec, None, false)
                        .unwrap();
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
}
