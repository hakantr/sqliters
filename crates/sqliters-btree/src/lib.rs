//! `sqliters-btree`: SQLite Format 3 B-Tree & B+Tree storage engine,
//! overflow chains, freelist page management, and `PRAGMA integrity_check`.
#![forbid(unsafe_code)]

pub mod cursor;
pub mod page;
pub mod ptrmap;

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub use sqliters_pager::AutoVacuumMode;
use sqliters_pager::{
    JournalMode, LockingMode, Pager, PagerOpenOptions, TransactionState, WalCheckpointMode,
    WalCheckpointResult,
};
use sqliters_types::{
    Bitvec, ErrorCode, KeySortOrder, Pgno, Result, SqliteError, TextEncoding, Value,
    compare_records_with_encoding, decode_record_with_encoding, extended,
};
use sqliters_vfs::{OpenFlags, Vfs};

pub use cursor::{
    BTCURSOR_MAX_DEPTH, BtreeCursorState, CursorId, CursorKind, CursorPagePos, SavedCursorKey,
    SkipNext,
};
pub use page::{
    BtreePageType, IndexInteriorCell, IndexLeafCell, ParsedPage, PayloadBounds, RawIndexCell,
    RawLeafDeleteOutcome, RawTableLeafCell, TableInteriorCell, TableLeafCell,
    binary_search_table_leaf, find_table_interior_child, last_table_leaf_rowid, peek_page_type,
    raw_cell_offset, raw_page_header, read_index_interior_cell, read_index_leaf_cell,
    read_table_interior_cell, read_table_leaf_cell, read_table_leaf_rowid, seek_table_leaf_cell,
    table_interior_right_child, try_delete_table_leaf_in_place, try_insert_table_leaf_in_place,
};
pub use ptrmap::{PtrmapType, RootPageMove, is_ptrmap_page, ptrmap_offset, ptrmap_pageno};

/// Page allocation search policy (`BTALLOC_ANY`, `BTALLOC_EXACT`, `BTALLOC_LE` in `btree.c`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocMode {
    Any,
    Exact(Pgno),
    Le(Pgno),
}

/// SQLite B-Tree storage manager over a [`Pager`].
pub struct Btree {
    pager: Pager,
    pub(crate) cursors: BTreeMap<CursorId, cursor::BtreeCursor>,
    pub(crate) next_cursor_id: u64,
    interrupt_flag: Option<Arc<AtomicBool>>,
}

impl Btree {
    pub fn new(pager: Pager) -> Self {
        Self {
            pager,
            cursors: BTreeMap::new(),
            next_cursor_id: 1,
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
        self.cursors.clear();
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

    /// Commits the active write transaction, running [`Self::auto_vacuum_commit`] first when in
    /// [`AutoVacuumMode::Full`] before delegating to [`Pager::commit_retaining_read`].
    pub fn commit_retaining_read(&mut self, retain_read: bool) -> Result<()> {
        if self.pager.transaction_state() == TransactionState::Write
            && !self.pager.is_commit_phase_one_done()
            && !self.pager.has_staged_raw_copy()
            && self.is_auto_vacuum()
            && let Err(e) = self.auto_vacuum_commit()
        {
            let _ = self.rollback_with_trip(extended::SQLITE_ABORT_ROLLBACK, false, retain_read);
            return Err(e);
        }
        self.pager.commit_retaining_read(retain_read)
    }

    #[inline]
    pub fn commit(&mut self) -> Result<()> {
        self.commit_retaining_read(false)
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
            for cur in self.cursors.values_mut() {
                if cur.root_pgno == max_root {
                    cur.root_pgno = root_pgno;
                }
            }
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
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_table_leaf(usable);
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
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let (deleted, _) = self.delete_table_rec(root_pgno, rowid, true)?;
        Ok(deleted)
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
                try_delete_table_leaf_in_place(pgno, usable, raw_mut, rowid, is_root)?
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
        self.collect_index_entries(root_pgno, &mut out)?;
        Ok(out)
    }

    fn collect_index_entries(&mut self, pgno: Pgno, out: &mut Vec<Vec<u8>>) -> Result<()> {
        self.check_interrupted()?;
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    self.check_interrupted()?;
                    let full = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push(full);
                }
                Ok(())
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                for c in cells {
                    self.collect_index_entries(c.left_child, out)?;
                    self.check_interrupted()?;
                    let full = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    out.push(full);
                }
                self.collect_index_entries(right_child, out)
            }
            _ => Err(SqliteError::corrupt("expected index B-tree page")),
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
        let raw_matches = self.seek_index_equal_raw(root_pgno, prefix_values, orders)?;
        raw_matches
            .into_iter()
            .map(|rec| decode_record_with_encoding(&rec, enc))
            .collect()
    }

    /// Searches an Index B-Tree for all raw serialized records whose leading columns match `prefix_values` under `orders`.
    pub fn seek_index_equal_raw(
        &mut self,
        root_pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Vec<Vec<u8>>> {
        let mut matches = Vec::new();
        self.seek_index_equal_raw_rec(root_pgno, prefix_values, orders, &mut matches)?;
        Ok(matches)
    }

    fn seek_index_equal_raw_rec(
        &mut self,
        pgno: Pgno,
        prefix_values: &[Value],
        orders: &[KeySortOrder],
        out: &mut Vec<Vec<u8>>,
    ) -> Result<()> {
        self.check_interrupted()?;
        let enc = self.encoding();
        let parsed = self.read_parsed_page(pgno)?;
        match parsed {
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    let rec = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, prefix_values, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => out.push(rec),
                        Ordering::Greater => break,
                    }
                }
                Ok(())
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                let mut visit_right = true;
                for c in cells {
                    let rec = self.read_full_payload(
                        c.total_payload_len,
                        &c.local_payload,
                        c.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, prefix_values, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            self.seek_index_equal_raw_rec(
                                c.left_child,
                                prefix_values,
                                orders,
                                out,
                            )?;
                            out.push(rec);
                        }
                        Ordering::Greater => {
                            self.seek_index_equal_raw_rec(
                                c.left_child,
                                prefix_values,
                                orders,
                                out,
                            )?;
                            visit_right = false;
                            break;
                        }
                    }
                }
                if visit_right {
                    self.seek_index_equal_raw_rec(right_child, prefix_values, orders, out)?;
                }
                Ok(())
            }
            _ => Err(SqliteError::corrupt("expected index page in seek")),
        }
    }

    /// Inserts a serialized index record (`[indexed_cols..., rowid]`) into an Index B-Tree.
    pub fn insert_index(
        &mut self,
        root_pgno: Pgno,
        record_bytes: &[u8],
        orders: &[KeySortOrder],
    ) -> Result<()> {
        self.save_cursors_for_mutation(Some(root_pgno), None)?;
        let enc = self.encoding();
        let full_vals = decode_record_with_encoding(record_bytes, enc)?;
        let key_len = if orders.is_empty() {
            full_vals.len()
        } else {
            orders.len().min(full_vals.len())
        };
        let key_vals = &full_vals[..key_len];
        let usable = self.pager.usable_size();
        let bounds = PayloadBounds::for_index(usable);
        let (total_len, local_payload, overflow_page) =
            self.prepare_cell_payload(bounds, record_bytes)?;
        let leaf_cell = IndexLeafCell {
            total_payload_len: total_len,
            local_payload,
            overflow_page,
        };

        if let Some((promoted_cell, right_pgno)) =
            self.insert_index_rec(root_pgno, leaf_cell, key_vals, orders)?
        {
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
            self.write_parsed_page(root_pgno, &new_root)?;
        }
        Ok(())
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
                let mut pos = cells.len();
                for (i, existing) in cells.iter().enumerate() {
                    let rec = self.read_full_payload(
                        existing.total_payload_len,
                        &existing.local_payload,
                        existing.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            let old = std::mem::replace(&mut cells[i], new_cell);
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
                        Ordering::Greater => {
                            pos = i;
                            break;
                        }
                    }
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
                let mut idx = cells.len();
                for (i, existing) in cells.iter().enumerate() {
                    let rec = self.read_full_payload(
                        existing.total_payload_len,
                        &existing.local_payload,
                        existing.overflow_page,
                    )?;
                    match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                        Ordering::Less => continue,
                        Ordering::Equal => {
                            let old_ovfl = existing.overflow_page;
                            cells[i] = IndexInteriorCell {
                                left_child: existing.left_child,
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
                        Ordering::Greater => {
                            idx = i;
                            break;
                        }
                    }
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
                    let mut pos = target_cells.len();
                    let mut replaced = false;
                    for (i, existing) in target_cells.iter().enumerate() {
                        let rec = self.read_full_payload(
                            existing.total_payload_len,
                            &existing.local_payload,
                            existing.overflow_page,
                        )?;
                        match compare_records_with_encoding(&rec, key_vals, orders, enc)? {
                            Ordering::Less => continue,
                            Ordering::Equal => {
                                pos = i;
                                replaced = true;
                                break;
                            }
                            Ordering::Greater => {
                                pos = i;
                                break;
                            }
                        }
                    }
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
        self.clear_tree(root_pgno)?;
        for rec in remaining {
            self.insert_index(root_pgno, &rec, orders)?;
        }
        Ok(true)
    }

    fn try_fast_leaf_delete_index(
        &mut self,
        root_pgno: Pgno,
        key_vals: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Option<bool>> {
        let enc = self.encoding();
        let mut cur_pgno = root_pgno;
        loop {
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

fn balance_index_leaf_siblings(
    cells: Vec<IndexLeafCell>,
    usable: u32,
) -> (Vec<Vec<IndexLeafCell>>, Vec<IndexLeafCell>) {
    let usable_space = (usable as usize).saturating_sub(8);
    let mut cnt_new: Vec<usize> = Vec::with_capacity(5);
    let mut sz_new: Vec<usize> = Vec::with_capacity(5);

    let mut start = 0usize;
    while start < cells.len() {
        let mut cur_sz = 0usize;
        let mut j = start;
        while j < cells.len() {
            let w = cells[j].encoded_size() + 2;
            if j > start && cur_sz + w > usable_space {
                break;
            }
            cur_sz += w;
            j += 1;
        }
        cnt_new.push(j);
        sz_new.push(cur_sz);
        if j == cells.len() {
            break;
        }
        start = j + 1;
        if start == cells.len() {
            cnt_new.push(cells.len());
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
            let sz_r = cells[r].encoded_size();
            let sz_d = cells[d].encoded_size();
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqliters_types::decode_record;
    use sqliters_types::encode_record;

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
