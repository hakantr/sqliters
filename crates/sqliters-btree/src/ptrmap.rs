//! Auto-vacuum pointer-map (`ptrmap`) arithmetic, entry maintenance, page relocation,
//! incremental vacuum, and commit-time compaction (`sqlite/src/btree.c:1055-1176`, `3860-4364`).

use sqliters_pager::{AutoVacuumMode, DB_HEADER_SIZE, TransactionState};
use sqliters_types::{Pgno, Result, SqliteError, get_varint, get_varint_i64};

use crate::page::{BtreePageType, ParsedPage, PayloadBounds, raw_cell_offset, raw_page_header};
use crate::{AllocMode, Btree};

/// Entry type stored in byte `0` of each 5-byte pointer-map record (`btreeInt.h:664-668`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PtrmapType {
    RootPage = 1,
    FreePage = 2,
    Overflow1 = 3,
    Overflow2 = 4,
    BtreeNode = 5,
}

impl PtrmapType {
    /// Decodes a pointer-map entry type byte (`1..=5`), returning `ErrorCode::Corrupt` otherwise.
    #[inline]
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::RootPage),
            2 => Ok(Self::FreePage),
            3 => Ok(Self::Overflow1),
            4 => Ok(Self::Overflow2),
            5 => Ok(Self::BtreeNode),
            other => Err(SqliteError::corrupt(format!(
                "invalid pointer-map entry type byte: {other}"
            ))),
        }
    }
}

/// Reports a root page relocation performed by [`Btree::drop_tree_relocating`] when dropping a
/// root page below `largest_root_btree_page` (`btree.c:10275-10305`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootPageMove {
    pub from: Pgno,
    pub to: Pgno,
}

/// Computes the 1-based page number of the pointer-map page covering `pgno` (`ptrmapPageno`,
/// `btree.c:1063-1075`), using 64-bit intermediate arithmetic so `pgno` near `u32::MAX` never
/// overflows. Returns `0` for `pgno < 2`.
#[inline]
pub fn ptrmap_pageno(pgno: Pgno, usable_size: u32, lock_byte_page: Pgno) -> Pgno {
    if pgno < 2 {
        return 0;
    }
    let g = u64::from(usable_size / 5) + 1;
    let i_ptr_map = u64::from(pgno - 2) / g;
    let mut ret = 2 + i_ptr_map * g;
    if ret == u64::from(lock_byte_page) {
        ret += 1;
    }
    Pgno::try_from(ret).unwrap_or(0)
}

/// Returns `true` when `pgno` is itself a pointer-map metadata page (`PTRMAP_ISPAGE`,
/// `btreeInt.h:631`). Never returns `true` for `lock_byte_page`.
#[inline]
pub fn is_ptrmap_page(pgno: Pgno, usable_size: u32, lock_byte_page: Pgno) -> bool {
    pgno >= 2 && ptrmap_pageno(pgno, usable_size, lock_byte_page) == pgno
}

/// Computes the byte offset of `key_pgno`'s 5-byte entry inside `ptrmap_pgno` (`PTRMAP_PTROFFSET`,
/// `btreeInt.h:630`), validating bounds against `usable_size`.
#[inline]
pub fn ptrmap_offset(ptrmap_pgno: Pgno, key_pgno: Pgno, usable_size: u32) -> Result<usize> {
    if ptrmap_pgno < 2 || key_pgno <= ptrmap_pgno {
        return Err(SqliteError::corrupt(format!(
            "invalid ptrmap offset query: ptrmap={ptrmap_pgno}, key={key_pgno}"
        )));
    }
    let off = (u64::from(key_pgno) - u64::from(ptrmap_pgno) - 1) * 5;
    if off.saturating_add(5) > u64::from(usable_size) {
        return Err(SqliteError::corrupt(format!(
            "ptrmap offset {off} out of bounds for usable_size={usable_size}"
        )));
    }
    Ok(off as usize)
}

impl Btree {
    /// Writes `(e_type, parent)` into the pointer-map slot for `key` (`ptrmapPut`, `btree.c:1087`).
    /// Leaves the pointer-map page clean if the stored entry already matches `(e_type, parent)`.
    pub fn ptrmap_put(&mut self, key: Pgno, e_type: PtrmapType, parent: Pgno) -> Result<()> {
        if key < 2 {
            return Err(SqliteError::corrupt(format!(
                "invalid ptrmap key page {key}"
            )));
        }
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        debug_assert!(!is_ptrmap_page(lock_pg, usable, lock_pg));
        let i_ptrmap = ptrmap_pageno(key, usable, lock_pg);
        if i_ptrmap == 0 || i_ptrmap > self.pager.page_count() {
            return Err(SqliteError::corrupt(format!(
                "ptrmap page {i_ptrmap} out of range for key {key} (page_count={})",
                self.pager.page_count()
            )));
        }
        let offset = ptrmap_offset(i_ptrmap, key, usable)?;
        {
            let ro = self.pager.read_page_unchecked(i_ptrmap)?;
            let cur_type = ro[offset];
            let cur_parent = u32::from_be_bytes([
                ro[offset + 1],
                ro[offset + 2],
                ro[offset + 3],
                ro[offset + 4],
            ]);
            if cur_type == e_type as u8 && cur_parent == parent {
                return Ok(());
            }
        }
        let w = self.pager.write_page(i_ptrmap)?;
        w[offset] = e_type as u8;
        w[offset + 1..offset + 5].copy_from_slice(&parent.to_be_bytes());
        Ok(())
    }

    /// Reads `(PtrmapType, parent_pgno)` for `key` from its pointer-map page (`ptrmapGet`,
    /// `btree.c:1146`).
    pub fn ptrmap_get(&mut self, key: Pgno) -> Result<(PtrmapType, Pgno)> {
        if key < 2 {
            return Err(SqliteError::corrupt(format!(
                "invalid ptrmap key page {key}"
            )));
        }
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let i_ptrmap = ptrmap_pageno(key, usable, lock_pg);
        if i_ptrmap == 0 || i_ptrmap > self.pager.page_count() {
            return Err(SqliteError::corrupt(format!(
                "ptrmap page {i_ptrmap} out of range for key {key} (page_count={})",
                self.pager.page_count()
            )));
        }
        let offset = ptrmap_offset(i_ptrmap, key, usable)?;
        let ro = self.pager.read_page_unchecked(i_ptrmap)?;
        let raw_type = ro[offset];
        let parent = u32::from_be_bytes([
            ro[offset + 1],
            ro[offset + 2],
            ro[offset + 3],
            ro[offset + 4],
        ]);
        let e_type = PtrmapType::from_byte(raw_type)?;
        Ok((e_type, parent))
    }

    /// Updates pointer-map entries for all child B-tree nodes and cell first-overflow pointers on
    /// `page` (`setChildPtrmaps`, `btree.c:3865`).
    pub(crate) fn set_child_ptrmaps_for_parsed(
        &mut self,
        pgno: Pgno,
        page: &ParsedPage,
    ) -> Result<()> {
        match page {
            ParsedPage::TableLeaf { cells } => {
                for c in cells {
                    if let Some(ovfl) = c.overflow_page {
                        self.ptrmap_put(ovfl, PtrmapType::Overflow1, pgno)?;
                    }
                }
            }
            ParsedPage::IndexLeaf { cells } => {
                for c in cells {
                    if let Some(ovfl) = c.overflow_page {
                        self.ptrmap_put(ovfl, PtrmapType::Overflow1, pgno)?;
                    }
                }
            }
            ParsedPage::TableInterior { cells, right_child } => {
                for c in cells {
                    self.ptrmap_put(c.left_child, PtrmapType::BtreeNode, pgno)?;
                }
                self.ptrmap_put(*right_child, PtrmapType::BtreeNode, pgno)?;
            }
            ParsedPage::IndexInterior { cells, right_child } => {
                for c in cells {
                    if let Some(ovfl) = c.overflow_page {
                        self.ptrmap_put(ovfl, PtrmapType::Overflow1, pgno)?;
                    }
                    self.ptrmap_put(c.left_child, PtrmapType::BtreeNode, pgno)?;
                }
                self.ptrmap_put(*right_child, PtrmapType::BtreeNode, pgno)?;
            }
        }
        Ok(())
    }

    /// Rewrites the single pointer on `ptr_page` that currently targets `from_pgno` so that it
    /// targets `to_pgno` (`modifyPagePointer`, `btree.c:10326-10425`). Supports `ptr_page == 1`
    /// (`hdr_off = 100`) as well as regular B-tree and overflow pages.
    pub(crate) fn modify_page_pointer(
        &mut self,
        ptr_page: Pgno,
        from_pgno: Pgno,
        to_pgno: Pgno,
        e_type: PtrmapType,
    ) -> Result<()> {
        if ptr_page == 0 {
            return Err(SqliteError::corrupt("cannot modify page pointer on page 0"));
        }
        let usable = self.pager.usable_size();
        let usable_usize = usable as usize;

        match e_type {
            PtrmapType::Overflow2 => {
                let raw = self.pager.write_page(ptr_page)?;
                if raw.len() < 4 {
                    return Err(SqliteError::corrupt("overflow page shorter than 4 bytes"));
                }
                let cur = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                if cur != from_pgno {
                    return Err(SqliteError::corrupt(format!(
                        "overflow2 chain pointer mismatch on page {ptr_page}: expected {from_pgno}, found {cur}"
                    )));
                }
                raw[0..4].copy_from_slice(&to_pgno.to_be_bytes());
                Ok(())
            }
            PtrmapType::BtreeNode => {
                let raw = self.pager.write_page(ptr_page)?;
                let (ptype, n_cell, right_child) = raw_page_header(ptr_page, usable, raw)?;
                if ptype.is_leaf() {
                    return Err(SqliteError::corrupt(format!(
                        "ptrmap BtreeNode parent {ptr_page} is a leaf page"
                    )));
                }
                let hdr_off = if ptr_page == 1 { DB_HEADER_SIZE } else { 0 };
                if right_child == from_pgno {
                    raw[hdr_off + 8..hdr_off + 12].copy_from_slice(&to_pgno.to_be_bytes());
                    return Ok(());
                }
                let hdr_size = ptype.header_size();
                for i in 0..n_cell {
                    let cell_off = raw_cell_offset(ptr_page, usable, raw, hdr_size, n_cell, i)?;
                    if cell_off + 4 > usable_usize {
                        return Err(SqliteError::corrupt(format!(
                            "truncated interior cell on page {ptr_page}"
                        )));
                    }
                    let child = u32::from_be_bytes([
                        raw[cell_off],
                        raw[cell_off + 1],
                        raw[cell_off + 2],
                        raw[cell_off + 3],
                    ]);
                    if child == from_pgno {
                        raw[cell_off..cell_off + 4].copy_from_slice(&to_pgno.to_be_bytes());
                        return Ok(());
                    }
                }
                Err(SqliteError::corrupt(format!(
                    "child pointer {from_pgno} not found on interior page {ptr_page}"
                )))
            }
            PtrmapType::Overflow1 => {
                let raw = self.pager.write_page(ptr_page)?;
                let (ptype, n_cell, _) = raw_page_header(ptr_page, usable, raw)?;
                if ptype == BtreePageType::TableInterior {
                    return Err(SqliteError::corrupt(format!(
                        "ptrmap Overflow1 parent {ptr_page} is a table interior page"
                    )));
                }
                let hdr_size = ptype.header_size();
                let tbl_bounds = PayloadBounds::for_table_leaf(usable);
                let idx_bounds = PayloadBounds::for_index(usable);

                for i in 0..n_cell {
                    let cell_off = raw_cell_offset(ptr_page, usable, raw, hdr_size, n_cell, i)?;
                    let ovfl_ptr_off = match ptype {
                        BtreePageType::TableLeaf => {
                            let slice = &raw[cell_off..usable_usize];
                            let (payload_u64, n1) = get_varint(slice)?;
                            let (_, n2) = get_varint_i64(&slice[n1..])?;
                            let total_len = u32::try_from(payload_u64).map_err(|_| {
                                SqliteError::corrupt("table leaf cell payload exceeds 4GiB")
                            })?;
                            let local_len = tbl_bounds.local_len(total_len);
                            if (local_len as u32) < total_len {
                                Some(cell_off + n1 + n2 + local_len)
                            } else {
                                None
                            }
                        }
                        BtreePageType::IndexLeaf => {
                            let slice = &raw[cell_off..usable_usize];
                            let (payload_u64, n1) = get_varint(slice)?;
                            let total_len = u32::try_from(payload_u64).map_err(|_| {
                                SqliteError::corrupt("index leaf cell payload exceeds 4GiB")
                            })?;
                            let local_len = idx_bounds.local_len(total_len);
                            if (local_len as u32) < total_len {
                                Some(cell_off + n1 + local_len)
                            } else {
                                None
                            }
                        }
                        BtreePageType::IndexInterior => {
                            if cell_off + 4 >= usable_usize {
                                return Err(SqliteError::corrupt(format!(
                                    "truncated index interior cell on page {ptr_page}"
                                )));
                            }
                            let slice = &raw[cell_off + 4..usable_usize];
                            let (payload_u64, n1) = get_varint(slice)?;
                            let total_len = u32::try_from(payload_u64).map_err(|_| {
                                SqliteError::corrupt("index interior cell payload exceeds 4GiB")
                            })?;
                            let local_len = idx_bounds.local_len(total_len);
                            if (local_len as u32) < total_len {
                                Some(cell_off + 4 + n1 + local_len)
                            } else {
                                None
                            }
                        }
                        BtreePageType::TableInterior => unreachable!("checked above"),
                    };

                    if let Some(ptr_off) = ovfl_ptr_off {
                        if ptr_off + 4 > usable_usize {
                            return Err(SqliteError::corrupt(format!(
                                "overflow1 pointer out of bounds on page {ptr_page}"
                            )));
                        }
                        let cur_ovfl = u32::from_be_bytes([
                            raw[ptr_off],
                            raw[ptr_off + 1],
                            raw[ptr_off + 2],
                            raw[ptr_off + 3],
                        ]);
                        if cur_ovfl == from_pgno {
                            raw[ptr_off..ptr_off + 4].copy_from_slice(&to_pgno.to_be_bytes());
                            return Ok(());
                        }
                    }
                }
                Err(SqliteError::corrupt(format!(
                    "overflow1 pointer {from_pgno} not found on page {ptr_page}"
                )))
            }
            PtrmapType::RootPage | PtrmapType::FreePage => Err(SqliteError::corrupt(format!(
                "invalid ptrmap type {e_type:?} in modify_page_pointer"
            ))),
        }
    }

    /// Relocates an occupied database page `from_pgno` (`e_type`, `ptr_page`) into `to_pgno`,
    /// updating both outgoing child/overflow pointer-map entries and the incoming parent reference
    /// (`relocatePage`, `btree.c:3974-4046`).
    pub fn relocate_page(
        &mut self,
        from_pgno: Pgno,
        e_type: PtrmapType,
        ptr_page: Pgno,
        to_pgno: Pgno,
        is_commit: bool,
    ) -> Result<()> {
        if from_pgno < 2 || to_pgno < 2 || e_type == PtrmapType::FreePage {
            return Err(SqliteError::corrupt(format!(
                "invalid relocate_page arguments: from={from_pgno}, to={to_pgno}, type={e_type:?}"
            )));
        }
        if from_pgno == to_pgno {
            return Ok(());
        }

        if !is_commit {
            let _ = self.pager.write_page(from_pgno)?;
        }

        let page_bytes = self.pager.read_page_unchecked(from_pgno)?.to_vec();
        {
            let dst = self.pager.write_page(to_pgno)?;
            dst.copy_from_slice(&page_bytes);
        }

        match e_type {
            PtrmapType::BtreeNode | PtrmapType::RootPage => {
                let parsed = self.read_parsed_page(to_pgno)?;
                self.set_child_ptrmaps_for_parsed(to_pgno, &parsed)?;
            }
            PtrmapType::Overflow1 | PtrmapType::Overflow2 => {
                let next_ovfl = u32::from_be_bytes([
                    page_bytes[0],
                    page_bytes[1],
                    page_bytes[2],
                    page_bytes[3],
                ]);
                if next_ovfl != 0 {
                    self.ptrmap_put(next_ovfl, PtrmapType::Overflow2, to_pgno)?;
                }
            }
            PtrmapType::FreePage => unreachable!("checked above"),
        }

        if e_type != PtrmapType::RootPage {
            self.modify_page_pointer(ptr_page, from_pgno, to_pgno, e_type)?;
            self.ptrmap_put(to_pgno, e_type, ptr_page)?;
        } else {
            self.ptrmap_put(to_pgno, PtrmapType::RootPage, 0)?;
        }

        Ok(())
    }

    /// Computes the compacted target database size in pages (`finalDbSize`, `btree.c:4169-4185`)
    /// using 64-bit saturating arithmetic so `n_free < n_orig` never underflows.
    pub fn final_db_size(&self, n_orig: Pgno, n_free: Pgno) -> Pgno {
        if n_orig <= 1 || n_free == 0 {
            return n_orig.max(1);
        }
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        let n_entry = u64::from(usable / 5);
        if n_entry == 0 {
            return n_orig.max(1);
        }
        let pm_orig = u64::from(ptrmap_pageno(n_orig, usable, lock_pg));
        let numer = (u64::from(n_free) + pm_orig + n_entry).saturating_sub(u64::from(n_orig));
        let n_ptrmap = numer / n_entry;
        let mut n_fin = u64::from(n_orig).saturating_sub(u64::from(n_free) + n_ptrmap) as Pgno;
        if n_orig > lock_pg && n_fin < lock_pg {
            n_fin = n_fin.saturating_sub(1);
        }
        while n_fin > 1 && (is_ptrmap_page(n_fin, usable, lock_pg) || n_fin == lock_pg) {
            n_fin -= 1;
        }
        n_fin.max(1)
    }

    /// Performs a single auto-vacuum / incremental-vacuum step on tail page `i_last_pg`
    /// (`incrVacuumStep`, `btree.c:4068-4162`). Returns `Ok(true)` when a step completed or
    /// `Ok(false)` when no free pages remain (`SQLITE_DONE`).
    pub(crate) fn incr_vacuum_step(
        &mut self,
        n_fin: Pgno,
        i_last_pg: Pgno,
        is_commit: bool,
    ) -> Result<bool> {
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();

        if !is_ptrmap_page(i_last_pg, usable, lock_pg) && i_last_pg != lock_pg {
            let n_free_list = self.pager.header().total_freelist_pages;
            if n_free_list == 0 {
                return Ok(false);
            }
            let (e_type, ptr_page) = self.ptrmap_get(i_last_pg)?;
            if e_type == PtrmapType::RootPage {
                return Err(SqliteError::corrupt(format!(
                    "unexpected root page {i_last_pg} at tail during auto-vacuum"
                )));
            }

            if e_type == PtrmapType::FreePage {
                if !is_commit {
                    let got = self.allocate_btree_page(AllocMode::Exact(i_last_pg))?;
                    if got != i_last_pg {
                        return Err(SqliteError::corrupt(format!(
                            "expected exact freelist extraction of {i_last_pg}, got {got}"
                        )));
                    }
                }
            } else {
                let mode = if is_commit {
                    AllocMode::Any
                } else {
                    AllocMode::Le(n_fin)
                };
                let i_free = loop {
                    let candidate = self.allocate_btree_page(mode)?;
                    if !is_commit || candidate <= n_fin {
                        break candidate;
                    }
                };
                if i_free >= i_last_pg {
                    return Err(SqliteError::corrupt(format!(
                        "allocated target page {i_free} >= tail page {i_last_pg}"
                    )));
                }
                self.relocate_page(i_last_pg, e_type, ptr_page, i_free, is_commit)?;
            }
        }

        if !is_commit {
            let mut new_end = i_last_pg.saturating_sub(1);
            while new_end > 1 && (new_end == lock_pg || is_ptrmap_page(new_end, usable, lock_pg)) {
                new_end -= 1;
            }
            self.pager.truncate_image(new_end.max(1))?;
        }

        Ok(true)
    }

    /// Runs a single explicit incremental-vacuum step (`sqlite3BtreeIncrVacuum`,
    /// `btree.c:4195-4226`) inside an active write transaction. Returns `Ok(true)` if one step
    /// freed tail space, or `Ok(false)` when no free pages remain (or auto-vacuum is disabled).
    pub fn incremental_vacuum_step(&mut self) -> Result<bool> {
        self.check_interrupted()?;
        if self.pager.transaction_state() != TransactionState::Write {
            return Err(SqliteError::misuse(
                "incremental_vacuum_step requires an active write transaction",
            ));
        }
        if !self.is_auto_vacuum() {
            return Ok(false);
        }
        let n_orig = self.pager.page_count();
        if n_orig <= 1 {
            return Ok(false);
        }
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        if is_ptrmap_page(n_orig, usable, lock_pg) || n_orig == lock_pg {
            return Err(SqliteError::corrupt(format!(
                "database ends on metadata/lock page {n_orig}"
            )));
        }
        let n_free = self.pager.header().total_freelist_pages;
        if n_free == 0 {
            return Ok(false);
        }
        if n_free >= n_orig {
            return Err(SqliteError::corrupt(
                "freelist page count >= database page count",
            ));
        }
        let n_fin = self.final_db_size(n_orig, n_free);
        if n_orig < n_fin {
            return Err(SqliteError::corrupt("computed final_db_size > page_count"));
        }
        self.save_cursors_for_mutation(None, None)?;
        self.incr_vacuum_step(n_fin, n_orig, false)
    }

    /// Runs up to `max_pages` (`None` = unlimited) incremental-vacuum steps inside an active write
    /// transaction, returning the number of steps performed.
    pub fn incremental_vacuum(&mut self, max_pages: Option<u32>) -> Result<u32> {
        if self.pager.transaction_state() != TransactionState::Write {
            return Err(SqliteError::misuse(
                "incremental_vacuum requires an active write transaction",
            ));
        }
        if !self.is_auto_vacuum() {
            return Ok(0);
        }
        let mut steps = 0u32;
        while max_pages.is_none_or(|limit| steps < limit) {
            if !self.incremental_vacuum_step()? {
                break;
            }
            steps += 1;
        }
        Ok(steps)
    }

    /// Compacts all freelist pages at commit Phase 1 when in [`AutoVacuumMode::Full`]
    /// (`autoVacuumCommit`, `btree.c:4232-4311`).
    pub(crate) fn auto_vacuum_commit(&mut self) -> Result<()> {
        if !self.is_auto_vacuum() || self.auto_vacuum_mode() != AutoVacuumMode::Full {
            return Ok(());
        }
        let n_orig = self.pager.page_count();
        if n_orig == 0 {
            return Ok(());
        }
        let usable = self.pager.usable_size();
        let lock_pg = self.lock_byte_page();
        if is_ptrmap_page(n_orig, usable, lock_pg) || n_orig == lock_pg {
            return Err(SqliteError::corrupt(format!(
                "database ends on metadata/lock page {n_orig}"
            )));
        }
        let n_free = self.pager.header().total_freelist_pages;
        if n_free == 0 {
            return Ok(());
        }
        if n_free >= n_orig {
            return Err(SqliteError::corrupt(
                "freelist page count >= database page count",
            ));
        }
        let n_fin = self.final_db_size(n_orig, n_free);
        if n_fin > n_orig {
            return Err(SqliteError::corrupt("computed final_db_size > page_count"));
        }
        if n_fin < n_orig {
            self.save_cursors_for_mutation(None, None)?;
        }

        for i_free in ((n_fin + 1)..=n_orig).rev() {
            self.check_interrupted()?;
            if !self.incr_vacuum_step(n_fin, i_free, true)? {
                break;
            }
        }

        {
            let hdr = self.pager.header_mut()?;
            hdr.first_freelist_trunk_page = 0;
            hdr.total_freelist_pages = 0;
        }
        self.pager.truncate_image(n_fin)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use sqliters_pager::{JournalMode, PagerOpenOptions};
    use sqliters_types::{
        ErrorCode, KeySortOrder, LockGeometry, Value, decode_record, encode_record,
    };
    use sqliters_vfs::{MemoryVfs, OpenFlags, Vfs};

    use super::*;

    fn open_av_disk_btree(
        vfs: Arc<MemoryVfs>,
        name: &str,
        page_size: u32,
        mode: AutoVacuumMode,
    ) -> Btree {
        let opts = PagerOpenOptions {
            default_page_size: page_size,
            lock_geometry: Some(vfs.lock_geometry()),
            ..PagerOpenOptions::default()
        };
        let mut bt = Btree::open_with_options(
            vfs,
            Some(Path::new(name)),
            OpenFlags::READWRITE | OpenFlags::CREATE,
            opts,
        )
        .unwrap();
        bt.enable_autovacuum_storage();
        bt.set_configured_auto_vacuum(mode);
        bt
    }

    #[test]
    fn test_group17_ptrmap_arithmetic_and_negative_controls() {
        // 1. 1024-byte pages, reserve = 0, standard 1 GiB pending byte (`lock_byte_page = 1_048_577`).
        let lock_1k = LockGeometry::PRODUCTION.lock_byte_page(1024);
        assert_eq!(lock_1k, 1_048_577);
        assert_eq!(ptrmap_pageno(1, 1024, lock_1k), 0);
        assert_eq!(ptrmap_pageno(2, 1024, lock_1k), 2);
        assert!(is_ptrmap_page(2, 1024, lock_1k));
        assert_eq!(ptrmap_pageno(3, 1024, lock_1k), 2);
        assert_eq!(ptrmap_offset(2, 3, 1024).unwrap(), 0);
        assert_eq!(ptrmap_offset(2, 206, 1024).unwrap(), 1015);
        assert_eq!(ptrmap_pageno(207, 1024, lock_1k), 207);
        assert!(is_ptrmap_page(207, 1024, lock_1k));

        // Exact collision at `2 + 5115 * 205 = 1_048_577 == lock_byte_page`: shifts to `1_048_578`!
        assert!(!is_ptrmap_page(1_048_577, 1024, lock_1k));
        assert_eq!(ptrmap_pageno(1_048_577, 1024, lock_1k), 1_048_578);
        assert_eq!(ptrmap_pageno(1_048_578, 1024, lock_1k), 1_048_578);
        assert!(is_ptrmap_page(1_048_578, 1024, lock_1k));
        assert_eq!(ptrmap_offset(1_048_578, 1_048_579, 1024).unwrap(), 0);
        // 203 entries (`1_048_579..=1_048_781`), last offset = `(203 - 1) * 5 = 1010`.
        assert_eq!(ptrmap_pageno(1_048_781, 1024, lock_1k), 1_048_578);
        assert_eq!(ptrmap_offset(1_048_578, 1_048_781, 1024).unwrap(), 1010);
        assert_eq!(ptrmap_pageno(1_048_782, 1024, lock_1k), 1_048_782);
        assert!(is_ptrmap_page(1_048_782, 1024, lock_1k));

        // 2. 4096-byte pages, reserve = 0 negative control (`lock_byte_page = 262_145`).
        let lock_4k = LockGeometry::PRODUCTION.lock_byte_page(4096);
        assert_eq!(lock_4k, 262_145);
        assert_eq!((262_145 - 2) % 820, 563);
        assert_eq!(ptrmap_pageno(262_145, 4096, lock_4k), 261_582);
        assert!(!is_ptrmap_page(262_145, 4096, lock_4k));
        assert!(is_ptrmap_page(261_582, 4096, lock_4k));

        // 3. `TEST_64K` (`0x10000`) lock geometry & non-zero `reserve` bytes (`16` and `32`).
        let lock_64k_1k = LockGeometry::TEST_64K.lock_byte_page(1024);
        assert_eq!(lock_64k_1k, 65);
        assert!(!is_ptrmap_page(65, 1024, lock_64k_1k));
        assert_eq!(ptrmap_pageno(65, 1024, lock_64k_1k), 2);

        let lock_64k_64k = LockGeometry::TEST_64K.lock_byte_page(65_536);
        assert_eq!(lock_64k_64k, 2);
        assert!(!is_ptrmap_page(2, 65_536, lock_64k_64k));
        assert_eq!(ptrmap_pageno(2, 65_536, lock_64k_64k), 3);
        assert!(is_ptrmap_page(3, 65_536, lock_64k_64k));

        for (ps, reserve) in [(1024u32, 16u32), (1024, 32), (4096, 16), (4096, 32)] {
            let usable = ps - reserve;
            let g = usable / 5 + 1;
            let lock_pg = LockGeometry::PRODUCTION.lock_byte_page(ps);
            assert!(!is_ptrmap_page(lock_pg, usable, lock_pg));
            assert!(is_ptrmap_page(2, usable, lock_pg));
            assert!(is_ptrmap_page(2 + g, usable, lock_pg));
            assert_eq!(
                ptrmap_offset(2, 2 + g - 1, usable).unwrap(),
                ((g - 2) * 5) as usize
            );
            assert!(ptrmap_offset(2, 2 + g, usable).is_err());
        }

        // 4. `u32::MAX` overflow safety.
        let pm_max = ptrmap_pageno(u32::MAX, 1024, lock_1k);
        assert!(pm_max > 0);
        assert!(is_ptrmap_page(pm_max, 1024, lock_1k));
        let _ = ptrmap_pageno(u32::MAX, 512, LockGeometry::PRODUCTION.lock_byte_page(512));
        let _ = ptrmap_pageno(
            u32::MAX,
            65_536,
            LockGeometry::PRODUCTION.lock_byte_page(65_536),
        );
    }

    #[test]
    fn test_autovacuum_full_lifecycle_relocation_splits_overflows_and_drop() {
        let vfs = Arc::new(MemoryVfs::with_lock_geometry(LockGeometry::TEST_64K));
        let mut bt = open_av_disk_btree(vfs.clone(), "av_full.db", 512, AutoVacuumMode::Full);

        bt.begin_write().unwrap();
        // New 1-page auto-vacuum DB MUST have `largest_root_btree_page == 1`, `incremental == 0`!
        assert_eq!(bt.pager().page_count(), 1);
        assert_eq!(bt.pager().header().largest_root_btree_page, 1);
        assert_eq!(bt.pager().header().incremental_vacuum_mode, 0);
        assert_eq!(bt.auto_vacuum_mode(), AutoVacuumMode::Full);
        assert!(bt.integrity_check(&[1]).unwrap().is_empty());

        // First user table creates Page 2 (ptrmap) and places root at Page 3.
        let t1 = bt.create_table().unwrap();
        assert_eq!(t1, 3);
        assert_eq!(bt.pager().header().largest_root_btree_page, 3);
        assert_eq!(bt.ptrmap_get(3).unwrap(), (PtrmapType::RootPage, 0));

        let t2 = bt.create_table().unwrap();
        assert_eq!(t2, 4);
        assert_eq!(bt.pager().header().largest_root_btree_page, 4);

        // Populate `t1` with multi-page splits and multi-page overflow chains (crossing Page 5+
        // and crossing Page 2 + second ptrmap Page 105 + `TEST_64K` lock-byte Page 129!).
        for i in 1..=160i64 {
            let blob = if i % 4 == 0 {
                vec![(i & 0xff) as u8; 1300]
            } else {
                vec![(i & 0xff) as u8; 90]
            };
            let rec = encode_record(&[Value::Integer(i), Value::Blob(blob)]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        assert!(bt.pager().page_count() > 135);
        // Page 5 (`max_root + 1`) is currently occupied by data/interior/overflow from `t1`!
        let (p5_type_before, _) = bt.ptrmap_get(5).unwrap();
        assert_ne!(p5_type_before, PtrmapType::RootPage);
        assert_ne!(p5_type_before, PtrmapType::FreePage);

        // Creating `idx1` at `max_root + 1 == 5` MUST relocate Page 5's existing occupant cleanly!
        let idx1 = bt.create_index().unwrap();
        assert_eq!(idx1, 5);
        assert_eq!(bt.pager().header().largest_root_btree_page, 5);
        assert_eq!(bt.ptrmap_get(5).unwrap(), (PtrmapType::RootPage, 0));

        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        for i in 1..=160i64 {
            let irec = encode_record(&[
                Value::Text(format!("key-{i:04}-padding")),
                Value::Integer(i),
            ]);
            bt.insert_index(idx1, &irec, &orders).unwrap();
        }
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        let errs = bt.integrity_check(&[1, t1, t2, idx1]).unwrap();
        assert!(errs.is_empty(), "integrity errors: {errs:?}");
        assert_eq!(bt.scan_table(t1).unwrap().len(), 160);
        bt.pager_mut().end_read_if_unused().unwrap();

        // Delete most rows and index entries, then `commit()` in `Full` mode:
        // must compact to `final_db_size` with `total_freelist_pages == 0`!
        bt.begin_write().unwrap();
        let pages_before_delete = bt.pager().page_count();
        for i in 1..=155i64 {
            assert!(bt.delete_table(t1, i).unwrap());
            let key = [
                Value::Text(format!("key-{i:04}-padding")),
                Value::Integer(i),
            ];
            assert!(bt.delete_index(idx1, &key, &orders).unwrap());
        }
        let expected_final = bt.final_db_size(
            bt.pager().page_count(),
            bt.pager().header().total_freelist_pages,
        );
        assert!(bt.pager().header().total_freelist_pages > 80);
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        assert_eq!(bt.pager().page_count(), expected_final);
        assert!(bt.pager().page_count() < pages_before_delete / 2);
        assert_eq!(bt.pager().header().total_freelist_pages, 0);
        assert_eq!(bt.pager().header().first_freelist_trunk_page, 0);
        assert!(bt.integrity_check(&[1, t1, t2, idx1]).unwrap().is_empty());
        let remaining = bt.scan_table(t1).unwrap();
        assert_eq!(remaining.len(), 5);
        assert_eq!(remaining[0].0, 156);
        assert_eq!(remaining[4].0, 160);
        let row160 = decode_record(&remaining[4].1).unwrap();
        assert_eq!(row160[1], Value::Blob(vec![(160 & 0xff) as u8; 1300]));
        bt.pager_mut().end_read_if_unused().unwrap();

        // `drop_tree_relocating`: roots are `[3 (t1), 4 (t2), 5 (idx1)]`.
        // Dropping `3` relocates `5 -> 3` and updates `largest_root_btree_page = 4`!
        bt.begin_write().unwrap();
        let mv = bt.drop_tree_relocating(3).unwrap();
        assert_eq!(mv, Some(RootPageMove { from: 5, to: 3 }));
        assert_eq!(bt.pager().header().largest_root_btree_page, 4);
        assert!(bt.integrity_check(&[1, 3, 4]).unwrap().is_empty());
        assert_eq!(bt.scan_index(3).unwrap().len(), 5);

        // Dropping `4` (currently `max_root`) returns `None` and sets `largest_root_btree_page = 3`.
        assert_eq!(bt.drop_tree_relocating(4).unwrap(), None);
        assert_eq!(bt.pager().header().largest_root_btree_page, 3);

        // Dropping the last user tree `3` returns `largest_root_btree_page` back to `1`!
        assert_eq!(bt.drop_tree_relocating(3).unwrap(), None);
        assert_eq!(bt.pager().header().largest_root_btree_page, 1);
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        assert_eq!(bt.pager().page_count(), 1);
        assert_eq!(bt.pager().header().largest_root_btree_page, 1);
        assert!(bt.integrity_check(&[1]).unwrap().is_empty());
        bt.close().unwrap();
    }

    #[test]
    fn test_autovacuum_incremental_savepoints_wal_and_page1_relocation() {
        let vfs = Arc::new(MemoryVfs::with_lock_geometry(LockGeometry::TEST_64K));
        let mut bt =
            open_av_disk_btree(vfs.clone(), "av_incr.db", 512, AutoVacuumMode::Incremental);

        bt.begin_write().unwrap();
        assert_eq!(bt.auto_vacuum_mode(), AutoVacuumMode::Incremental);
        assert_eq!(bt.pager().header().largest_root_btree_page, 1);
        assert_eq!(bt.pager().header().incremental_vacuum_mode, 1);

        // Populate Page 1 (`sqlite_schema`, `hdr_off = 100`) so it splits into an interior tree
        // AND holds a multi-page overflow cell, exercising `modify_page_pointer(ptr_page == 1)`!
        for s in 1..=12i64 {
            let schema_payload = if s == 1 {
                encode_record(&[Value::Text("big".into()), Value::Blob(vec![0xAB; 900])])
            } else {
                encode_record(&[Value::Text(format!("entry_{s:02}")), Value::Integer(s)])
            };
            bt.insert_table(1, s, &schema_payload).unwrap();
        }
        let p1_children_before = bt.scan_table(1).unwrap();
        assert_eq!(p1_children_before.len(), 12);

        // Creating user tables `t1` and `t2` relocates Page 1's child/overflow pages out of 3 & 4!
        let t1 = bt.create_table().unwrap();
        let t2 = bt.create_table().unwrap();
        assert_eq!((t1, t2), (3, 4));
        assert_eq!(bt.scan_table(1).unwrap(), p1_children_before);

        for i in 1..=45i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![0x5A; 300])]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        bt.commit().unwrap();

        // Delete rows in `Incremental` mode: `commit()` MUST keep freed pages on freelist!
        bt.begin_write().unwrap();
        for i in 1..=40i64 {
            assert!(bt.delete_table(t1, i).unwrap());
        }
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        let count_after_commit = bt.pager().page_count();
        let free_after_commit = bt.pager().header().total_freelist_pages;
        assert!(free_after_commit > 15);
        let target_final = bt.final_db_size(count_after_commit, free_after_commit);
        assert!(bt.integrity_check(&[1, t1, t2]).unwrap().is_empty());
        bt.pager_mut().end_read_if_unused().unwrap();

        // Savepoint + rollback across `incremental_vacuum`, `create_table`, and `drop_tree_relocating`!
        bt.begin_write().unwrap();
        bt.pager_mut().ensure_subsavepoints(1).unwrap();
        let steps_sp = bt.incremental_vacuum(Some(4)).unwrap();
        assert_eq!(steps_sp, 4);
        let t3_temp = bt.create_table().unwrap();
        assert_eq!(t3_temp, 5);
        let _ = bt.drop_tree_relocating(3).unwrap();
        bt.pager_mut().rollback_to_subsavepoint(0).unwrap();
        bt.pager_mut().release_subsavepoint(0).unwrap();

        assert_eq!(bt.pager().page_count(), count_after_commit);
        assert_eq!(bt.pager().header().total_freelist_pages, free_after_commit);
        assert!(bt.integrity_check(&[1, t1, t2]).unwrap().is_empty());

        // Now run `incremental_vacuum(Some(3))` then `incremental_vacuum(None)` and commit!
        let did3 = bt.incremental_vacuum(Some(3)).unwrap();
        assert_eq!(did3, 3);
        assert!(bt.pager().page_count() < count_after_commit);
        assert!(bt.integrity_check(&[1, t1, t2]).unwrap().is_empty());

        let rest = bt.incremental_vacuum(None).unwrap();
        assert!(rest > 0);
        assert_eq!(bt.pager().page_count(), target_final);
        assert_eq!(bt.pager().header().total_freelist_pages, 0);
        bt.commit().unwrap();

        // Switch to WAL mode and verify auto-vacuum `Full`-equivalent incremental steps + WAL spill!
        assert_eq!(
            bt.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        bt.begin_write().unwrap();
        for i in 100..=120i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![0x77; 260])]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        let _ = bt.spill_dirty_pages_to_wal().unwrap();
        for i in 100..=118i64 {
            assert!(bt.delete_table(t1, i).unwrap());
        }
        let _ = bt.incremental_vacuum(None).unwrap();
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        assert_eq!(bt.pager().header().total_freelist_pages, 0);
        assert!(bt.integrity_check(&[1, t1, t2]).unwrap().is_empty());
        assert_eq!(bt.scan_table(1).unwrap(), p1_children_before);
        bt.close().unwrap();
    }

    #[test]
    fn test_group49_corrupt_ptrmap_and_pointer_map_referenced_detection() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut bt = open_av_disk_btree(vfs, "av_corrupt.db", 512, AutoVacuumMode::Full);

        bt.begin_write().unwrap();
        let t1 = bt.create_table().unwrap();
        assert_eq!(t1, 3);
        let rec = encode_record(&[Value::Integer(1), Value::Blob(vec![0x33; 700])]);
        bt.insert_table(t1, 1, &rec).unwrap();
        bt.commit().unwrap();

        // 1. Corrupt ptrmap parent_pgno on Page 4 (first overflow page of `t1` cell).
        bt.begin_write().unwrap();
        let orig_entry = bt.ptrmap_get(4).unwrap();
        assert_eq!(orig_entry, (PtrmapType::Overflow1, 3));
        bt.ptrmap_put(4, PtrmapType::Overflow1, 99).unwrap();
        let errs_bad_parent = bt.integrity_check(&[1, t1]).unwrap();
        assert!(
            errs_bad_parent
                .iter()
                .any(|m| m.contains("Bad ptr map entry key=4")),
            "expected bad ptrmap diagnostic, got: {errs_bad_parent:?}"
        );

        // 2. Corrupt ptrmap type byte to an invalid value (`9`).
        {
            let off = ptrmap_offset(2, 4, 512).unwrap();
            bt.pager_mut().write_page(2).unwrap()[off] = 9;
        }
        let errs_bad_type = bt.integrity_check(&[1, t1]).unwrap();
        assert!(
            errs_bad_type
                .iter()
                .any(|m| m.contains("Failed to read ptrmap key=4")),
            "expected invalid ptrmap type diagnostic, got: {errs_bad_type:?}"
        );
        bt.rollback().unwrap();

        // 3. Corrupt a cell's overflow pointer (and freelist trunk) to reference Page 2 (ptrmap)!
        bt.begin_write().unwrap();
        bt.modify_page_pointer(3, 4, 2, PtrmapType::Overflow1)
            .unwrap();
        let errs_ovfl_ref = bt.integrity_check(&[1, t1]).unwrap();
        assert!(
            errs_ovfl_ref
                .iter()
                .any(|m| m.contains("Page 2: pointer map referenced")),
            "expected pointer map referenced error, got: {errs_ovfl_ref:?}"
        );
        bt.rollback().unwrap();

        bt.begin_write().unwrap();
        {
            let hdr = bt.pager_mut().header_mut().unwrap();
            hdr.first_freelist_trunk_page = 2;
            hdr.total_freelist_pages = 1;
        }
        let errs_free_ref = bt.integrity_check(&[1, t1]).unwrap();
        assert!(
            errs_free_ref
                .iter()
                .any(|m| m.contains("Page 2: pointer map referenced")),
            "expected pointer map referenced error on freelist, got: {errs_free_ref:?}"
        );
        bt.rollback().unwrap();

        // Clean state still passes integrity_check!
        bt.begin_read().unwrap();
        assert_eq!(bt.pager().header().largest_root_btree_page, 3);
        assert!(bt.integrity_check(&[1, t1]).unwrap().is_empty());
        assert_eq!(
            PtrmapType::from_byte(0).unwrap_err().code,
            ErrorCode::Corrupt
        );
        bt.close().unwrap();
    }

    #[test]
    fn test_autovacuum_freelist_trunk_with_leaves_exact_promotion() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut bt = open_av_disk_btree(vfs, "av_trunk_promo.db", 512, AutoVacuumMode::Incremental);

        bt.begin_write().unwrap();
        let t1 = bt.create_table().unwrap();
        assert_eq!(t1, 3);
        // Allocate enough overflow/leaf pages so freeing them builds a freelist trunk with k >= 2 leaves,
        // then force the trunk itself to sit at Page 4 (`max_root + 1`).
        for i in 1..=6i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![0x42; 900])]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        // Free rows 2..=6 first (creating leaves), then check where trunk sits; or drop & recreate:
        // Better yet: create `t2` at Page 4, add rows to `t1` (pages 5..), delete rows in `t1` (pages 5.. become trunk+leaves),
        // Wait: first freed page becomes trunk! So if we free Page 4 FIRST, Page 4 becomes `first_freelist_trunk_page`,
        // and subsequent `free_page` calls push leaves onto Page 4!
        let t2 = bt.create_table().unwrap();
        assert_eq!(t2, 4);
        for i in 10..=15i64 {
            let rec = encode_record(&[Value::Integer(i), Value::Blob(vec![0x99; 900])]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        // Drop `t2` (Page 4 is now `max_root`, so `drop_tree_relocating(4)` frees Page 4 FIRST -> trunk = 4, max_root = 3!).
        assert_eq!(bt.drop_tree_relocating(4).unwrap(), None);
        assert_eq!(bt.pager().header().largest_root_btree_page, 3);
        assert_eq!(bt.pager().header().first_freelist_trunk_page, 4);
        assert_eq!(bt.pager().header().total_freelist_pages, 1);

        // Now delete rows 10..=14 in `t1` so multiple freed pages are pushed as leaves onto trunk Page 4!
        for i in 10..=14i64 {
            assert!(bt.delete_table(t1, i).unwrap());
        }
        let free_before = bt.pager().header().total_freelist_pages;
        assert!(free_before >= 8);
        assert_eq!(bt.pager().header().first_freelist_trunk_page, 4);

        // Creating a new table requires `AllocMode::Exact(4)` (`max_root + 1 == 4`), which MUST promote
        // `L_0` of trunk Page 4 into the new freelist trunk (`btree.c:6681-6717`)!
        let t_new = bt.create_table().unwrap();
        assert_eq!(t_new, 4);
        assert_eq!(bt.pager().header().total_freelist_pages, free_before - 1);
        assert_ne!(bt.pager().header().first_freelist_trunk_page, 4);
        assert_ne!(bt.pager().header().first_freelist_trunk_page, 0);
        assert!(bt.integrity_check(&[1, t1, t_new]).unwrap().is_empty());
        bt.commit().unwrap();
        bt.close().unwrap();
    }

    struct PopulatedFixture {
        bt: Btree,
        t_root: Pgno,
        idx_root: Pgno,
        rows: Vec<(i64, Vec<u8>)>,
        idx_rows: Vec<Vec<u8>>,
    }

    fn build_populated_btree(
        vfs: Arc<MemoryVfs>,
        name: &str,
        page_size: u32,
        min_total_bytes: u64,
    ) -> PopulatedFixture {
        let opts = PagerOpenOptions {
            default_page_size: page_size,
            lock_geometry: Some(vfs.lock_geometry()),
            ..PagerOpenOptions::default()
        };
        let mut bt = Btree::open_with_options(
            vfs,
            Some(Path::new(name)),
            OpenFlags::READWRITE | OpenFlags::CREATE,
            opts,
        )
        .unwrap();
        bt.begin_write().unwrap();
        let t_root = bt.create_table().unwrap();
        let idx_root = bt.create_index().unwrap();
        // Record two schema rows on Page 1 as well so Page 1 B-tree payload is also verified across page-size changes!
        bt.insert_table(
            1,
            1,
            &encode_record(&[
                Value::Text("table_t".into()),
                Value::Integer(i64::from(t_root)),
            ]),
        )
        .unwrap();
        bt.insert_table(
            1,
            2,
            &encode_record(&[
                Value::Text("index_idx".into()),
                Value::Integer(i64::from(idx_root)),
            ]),
        )
        .unwrap();

        let orders = [KeySortOrder::default(), KeySortOrder::default()];
        let mut rowid = 1i64;
        loop {
            let blob_len = if rowid % 3 == 0 { 2600 } else { 520 };
            let blob = vec![((rowid * 17 + 3) & 0xff) as u8; blob_len];
            let rec = encode_record(&[
                Value::Integer(rowid),
                Value::Text(format!("row-{rowid:05}")),
                Value::Blob(blob),
            ]);
            bt.insert_table(t_root, rowid, &rec).unwrap();
            let irec = encode_record(&[
                Value::Text(format!("idx-{rowid:05}")),
                Value::Integer(rowid),
            ]);
            bt.insert_index(idx_root, &irec, &orders).unwrap();

            let cur_bytes = u64::from(bt.pager().page_count()) * u64::from(page_size);
            if cur_bytes >= min_total_bytes && rowid >= 18 {
                break;
            }
            rowid += 1;
        }
        bt.commit().unwrap();

        bt.begin_read().unwrap();
        assert!(
            bt.integrity_check(&[1, t_root, idx_root])
                .unwrap()
                .is_empty()
        );
        let rows = bt.scan_table(t_root).unwrap();
        let idx_rows = bt.scan_index(idx_root).unwrap();
        bt.pager_mut().end_read_if_unused().unwrap();
        PopulatedFixture {
            bt,
            t_root,
            idx_root,
            rows,
            idx_rows,
        }
    }

    fn read_vfs_bytes(vfs: &MemoryVfs, name: &str) -> Vec<u8> {
        let f = vfs
            .open(Some(Path::new(name)), OpenFlags::READONLY)
            .unwrap();
        let len = f.file_size().unwrap() as usize;
        let mut buf = vec![0u8; len];
        if len > 0 {
            f.read_at(&mut buf, 0).unwrap();
        }
        buf
    }

    #[test]
    fn test_group56_copy_pages_from_grow_and_shrink_across_pending_byte() {
        let vfs = Arc::new(MemoryVfs::with_lock_geometry(LockGeometry::TEST_64K));

        // Cover:
        // 1. Grow `1024 -> 4096` (> 145 KiB, crossing 64 KiB TEST_64K pending byte)
        // 2. Grow `512 -> 65536` (> 145 KiB, crossing 64 KiB TEST_64K pending byte)
        // 3. Shrink `4096 -> 1024` (> 145 KiB, crossing 64 KiB + `post_pending_tail`)
        // 4. Shrink `65536 -> 512` (both ~96 KiB hitting `n_dest_pages == dest_lock_pg == 2 -> 1`
        //    with `post_pending_tail` AND > 150 KiB spanning past `dest_lock_pg`!)
        let cases: &[(&str, u32, u64, u32, u64)] = &[
            ("g_1k_4k", 1024, 150_000, 4096, 150_000),
            ("g_512_64k", 512, 150_000, 65_536, 200_000),
            ("s_4k_1k", 4096, 150_000, 1024, 150_000),
            ("s_64k_512_tail_in_lockpg", 65_536, 196_608, 512, 95_000),
            ("s_64k_512_beyond_lockpg", 65_536, 65_536, 512, 160_000),
        ];

        for &(tag, dst_ps, dst_min_bytes, src_ps, src_min_bytes) in cases {
            let dst_name = format!("{tag}_dst.db");
            let src_name = format!("{tag}_src.db");
            let mut dst = build_populated_btree(vfs.clone(), &dst_name, dst_ps, dst_min_bytes).bt;
            let PopulatedFixture {
                bt: mut src,
                t_root: src_t,
                idx_root: src_idx,
                rows: expected_rows,
                idx_rows: expected_idx,
            } = build_populated_btree(vfs.clone(), &src_name, src_ps, src_min_bytes);

            let expected_pages = src.pager().page_count();
            let expected_file_size = u64::from(expected_pages) * u64::from(src_ps);

            // Stage `next_page_size` on `dst` first: must NOT mutate live `page_size()` before copy!
            dst.pager_mut().set_page_size(src_ps).unwrap();
            assert_eq!(dst.pager().page_size(), dst_ps);
            assert_eq!(dst.pager().next_page_size(), Some(src_ps));

            dst.begin_write().unwrap();
            dst.pager_mut().copy_pages_from(src.pager_mut()).unwrap();
            // Before Phase 2 finalizes the journal, live geometry remains `dst_ps` when `src_ps != dst_ps`!
            assert_eq!(dst.pager().page_size(), dst_ps);
            dst.commit().unwrap();

            assert_eq!(dst.pager().page_size(), src_ps, "case {tag}");
            assert_eq!(dst.pager().page_count(), expected_pages, "case {tag}");
            assert_eq!(dst.pager().next_page_size(), None, "case {tag}");
            assert_eq!(
                read_vfs_bytes(&vfs, &dst_name).len() as u64,
                expected_file_size,
                "case {tag} disk size"
            );

            // Verify live `dst` right after commit.
            dst.begin_read().unwrap();
            let errs = dst.integrity_check(&[1, src_t, src_idx]).unwrap();
            assert!(errs.is_empty(), "case {tag} integrity errors: {errs:?}");
            assert_eq!(dst.scan_table(src_t).unwrap(), expected_rows, "case {tag}");
            assert_eq!(dst.scan_index(src_idx).unwrap(), expected_idx, "case {tag}");
            dst.close().unwrap();
            src.close().unwrap();

            // Reopen `dst` from scratch to verify persisted Page 1 header and all B-trees.
            let mut reopened = Btree::open_with_options(
                vfs.clone(),
                Some(Path::new(&dst_name)),
                OpenFlags::READWRITE,
                PagerOpenOptions {
                    lock_geometry: Some(LockGeometry::TEST_64K),
                    ..PagerOpenOptions::default()
                },
            )
            .unwrap();
            reopened.begin_read().unwrap();
            assert_eq!(reopened.pager().page_size(), src_ps, "reopened {tag}");
            assert_eq!(
                reopened.pager().page_count(),
                expected_pages,
                "reopened {tag}"
            );
            assert!(
                reopened
                    .integrity_check(&[1, src_t, src_idx])
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(reopened.scan_table(src_t).unwrap(), expected_rows);
            assert_eq!(reopened.scan_index(src_idx).unwrap(), expected_idx);
            reopened.close().unwrap();
        }
    }

    #[test]
    fn test_group56_copy_pages_from_readonly_guards_and_old_geometry_journal_recovery() {
        let vfs = Arc::new(MemoryVfs::with_lock_geometry(LockGeometry::TEST_64K));

        // 1. Reject `src_ps != dst_ps` on WAL destination (`ErrorCode::ReadOnly`) and `:memory:` destination!
        let PopulatedFixture {
            bt: mut wal_dst,
            t_root: wal_t,
            idx_root: wal_idx,
            rows: wal_rows,
            ..
        } = build_populated_btree(vfs.clone(), "wal_dst.db", 4096, 80_000);
        assert_eq!(
            wal_dst.set_journal_mode(JournalMode::Wal).unwrap(),
            JournalMode::Wal
        );
        let mut src_1k = build_populated_btree(vfs.clone(), "src_1k.db", 1024, 90_000).bt;

        // Invalid `set_page_size` values fail with `Misuse` without mutating `page_size` or `next_page_size`.
        for bad_ps in [0u32, 256, 1000, 3072, 131_072] {
            assert_eq!(
                wal_dst.pager_mut().set_page_size(bad_ps).unwrap_err().code,
                ErrorCode::Misuse
            );
            assert_eq!(wal_dst.pager().page_size(), 4096);
            assert_eq!(wal_dst.pager().next_page_size(), None);
        }

        wal_dst.begin_write().unwrap();
        let wal_err = wal_dst
            .pager_mut()
            .copy_pages_from(src_1k.pager_mut())
            .unwrap_err();
        assert_eq!(wal_err.code, ErrorCode::ReadOnly);
        wal_dst.rollback().unwrap();
        wal_dst.begin_read().unwrap();
        assert_eq!(wal_dst.pager().page_size(), 4096);
        assert!(
            wal_dst
                .integrity_check(&[1, wal_t, wal_idx])
                .unwrap()
                .is_empty()
        );
        assert_eq!(wal_dst.scan_table(wal_t).unwrap(), wal_rows);
        wal_dst.close().unwrap();

        let mut mem_dst = Btree::open_in_memory().unwrap();
        mem_dst.begin_write().unwrap();
        let mem_t = mem_dst.create_table().unwrap();
        mem_dst
            .insert_table(mem_t, 1, &encode_record(&[Value::Text("stay".into())]))
            .unwrap();
        mem_dst.commit().unwrap();

        mem_dst.begin_write().unwrap();
        let mem_err = mem_dst
            .pager_mut()
            .copy_pages_from(src_1k.pager_mut())
            .unwrap_err();
        assert_eq!(mem_err.code, ErrorCode::ReadOnly);
        mem_dst.rollback().unwrap();
        mem_dst.begin_read().unwrap();
        assert_eq!(mem_dst.pager().page_size(), 4096);
        assert_eq!(mem_dst.scan_table(mem_t).unwrap().len(), 1);
        mem_dst.close().unwrap();

        // 2. Verify rollback journal header `[24..28]` records `tx_orig_header.page_size` (`4096`),
        //    live `rollback()` restores old geometry, AND hot-journal crash recovery restores old
        //    geometry even after `.db` has been overwritten and resized to `src_1k.db`!
        let PopulatedFixture {
            bt: mut orig_4k,
            t_root: orig_t,
            idx_root: orig_idx,
            rows: orig_rows,
            idx_rows: orig_idx_rows,
        } = build_populated_btree(vfs.clone(), "orig_4k.db", 4096, 140_000);
        let orig_page_count = orig_4k.pager().page_count();

        orig_4k.begin_write().unwrap();
        orig_4k
            .pager_mut()
            .copy_pages_from(src_1k.pager_mut())
            .unwrap();

        // Inspect `<orig_4k.db-journal>` directly on the VFS while transaction is in flight!
        let raw_journal = read_vfs_bytes(&vfs, "orig_4k.db-journal");
        assert!(raw_journal.len() >= 512 + (orig_page_count as usize - 1) * (4096 + 8));
        let j_db_pages = u32::from_be_bytes(raw_journal[16..20].try_into().unwrap());
        let j_page_size = u32::from_be_bytes(raw_journal[24..28].try_into().unwrap());
        assert_eq!(j_db_pages, orig_page_count);
        assert_eq!(
            j_page_size, 4096,
            "journal header must record pre-transaction page_size (4096)"
        );

        // Snapshot the hot journal with synced `nRec` (`orig_page_count - 1` because `dest_lock_pg == 17` is skipped)
        // and the raw `src_1k.db` image to simulate a power loss right after Phase 1 writes `.db`!
        let lock_pg_4k = LockGeometry::TEST_64K.lock_byte_page(4096);
        let expected_j_recs = if orig_page_count >= lock_pg_4k {
            orig_page_count - 1
        } else {
            orig_page_count
        };
        let mut crashed_journal = raw_journal;
        crashed_journal[8..12].copy_from_slice(&expected_j_recs.to_be_bytes());
        let overwritten_db_bytes = read_vfs_bytes(&vfs, "src_1k.db");

        // First verify live `rollback()` on `orig_4k` restores 100% of old 4096-byte state!
        orig_4k.rollback().unwrap();
        orig_4k.begin_read().unwrap();
        assert_eq!(orig_4k.pager().page_size(), 4096);
        assert_eq!(orig_4k.pager().page_count(), orig_page_count);
        assert!(
            orig_4k
                .integrity_check(&[1, orig_t, orig_idx])
                .unwrap()
                .is_empty()
        );
        assert_eq!(orig_4k.scan_table(orig_t).unwrap(), orig_rows);
        assert_eq!(orig_4k.scan_index(orig_idx).unwrap(), orig_idx_rows);
        orig_4k.close().unwrap();
        src_1k.close().unwrap();

        // Now materialize `crashed.db` (overwritten & shrunk to `src_1k.db` bytes!) + hot `crashed.db-journal`!
        {
            let f_db = vfs
                .open(
                    Some(Path::new("crashed.db")),
                    OpenFlags::READWRITE | OpenFlags::CREATE,
                )
                .unwrap();
            f_db.write_at(&overwritten_db_bytes, 0).unwrap();
            let f_j = vfs
                .open(
                    Some(Path::new("crashed.db-journal")),
                    OpenFlags::READWRITE | OpenFlags::CREATE,
                )
                .unwrap();
            f_j.write_at(&crashed_journal, 0).unwrap();
        }
        assert_eq!(
            u16::from_be_bytes(
                read_vfs_bytes(&vfs, "crashed.db")[16..18]
                    .try_into()
                    .unwrap()
            ),
            1024
        );

        // Open a fresh `Btree` on `crashed.db`: `begin_read()` MUST play back `crashed.db-journal`
        // in the OLD 4096-byte geometry and restore every single page and row!
        let mut recovered = Btree::open_with_options(
            vfs.clone(),
            Some(Path::new("crashed.db")),
            OpenFlags::READWRITE,
            PagerOpenOptions {
                lock_geometry: Some(LockGeometry::TEST_64K),
                ..PagerOpenOptions::default()
            },
        )
        .unwrap();
        recovered.begin_read().unwrap();
        assert!(!vfs.exists(Path::new("crashed.db-journal")).unwrap());
        assert_eq!(recovered.pager().page_size(), 4096);
        assert_eq!(recovered.pager().page_count(), orig_page_count);
        let rec_errs = recovered.integrity_check(&[1, orig_t, orig_idx]).unwrap();
        assert!(
            rec_errs.is_empty(),
            "recovered integrity errors: {rec_errs:?}"
        );
        assert_eq!(recovered.scan_table(orig_t).unwrap(), orig_rows);
        assert_eq!(recovered.scan_index(orig_idx).unwrap(), orig_idx_rows);
        recovered.close().unwrap();
    }

    #[test]
    fn test_balance_quick_1024_sequential_rows_packing_and_cursor_root_remap() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut bt = open_av_disk_btree(vfs, "av_quick_pack.db", 1024, AutoVacuumMode::Full);

        bt.begin_write().unwrap();
        let t1 = bt.create_table().unwrap();
        assert_eq!(t1, 3);

        // Insert 1024 sequential rows `(INTEGER PRIMARY KEY -> NULL in record, 50-byte text)` on
        // `1024`-byte pages (`autovacuum-9.3` exact layout): including the very first root-leaf
        // split, `balance_quick` packs 17 rows per leaf -> 61 leaves + Page 1 + Page 2 + Page 3 = 64 pages!
        let text50 = "x".repeat(50);
        for i in 1..=1024i64 {
            let rec = encode_record(&[Value::Null, Value::Text(text50.clone())]);
            bt.insert_table(t1, i, &rec).unwrap();
        }
        assert_eq!(bt.pager().page_count(), 64);
        assert!(bt.integrity_check(&[1, t1]).unwrap().is_empty());

        // Create a second table `t2` (`max_root == 4`), populate rows `10..=14`, open a live cursor
        // on `t2`, advance to row `11`, then drop `t1` (`root 3`): `t2` relocates `4 -> 3` and the
        // saved cursor's `root_pgno` must remap `4 -> 3` and continue scanning `12, 13, 14`!
        let t2 = bt.create_table().unwrap();
        assert_eq!(t2, 4);
        for k in 10..=14i64 {
            let rec = encode_record(&[Value::Integer(k * 100)]);
            bt.insert_table(t2, k, &rec).unwrap();
        }

        let cid = bt.open_table_cursor(t2, false);
        assert_eq!(bt.cursor_root_pgno(cid), Some(4));
        assert!(bt.cursor_first(cid).unwrap());
        assert_eq!(bt.cursor_rowid(cid).unwrap(), 10);
        assert!(bt.cursor_next(cid).unwrap());
        assert_eq!(bt.cursor_rowid(cid).unwrap(), 11);

        let mv = bt.drop_tree_relocating(t1).unwrap();
        assert_eq!(mv, Some(RootPageMove { from: 4, to: 3 }));
        assert_eq!(bt.cursor_root_pgno(cid), Some(3));
        assert_eq!(bt.cursor_rowid(cid).unwrap(), 11);

        let mut rest = Vec::new();
        while bt.cursor_next(cid).unwrap() {
            rest.push(bt.cursor_rowid(cid).unwrap());
        }
        assert_eq!(rest, vec![12, 13, 14]);
        bt.close_cursor(cid);
        assert!(bt.integrity_check(&[1, 3]).unwrap().is_empty());
        bt.commit().unwrap();
        bt.close().unwrap();
    }

    #[test]
    fn test_table_leaf_three_way_split_middle_insert_on_1k_pages() {
        let vfs = Arc::new(MemoryVfs::new());
        let mut bt = open_av_disk_btree(vfs, "three_way_split.db", 1024, AutoVacuumMode::Full);

        bt.begin_write().unwrap();
        let p900 = encode_record(&[Value::Blob(vec![0xAA; 900])]);
        let p110 = encode_record(&[Value::Blob(vec![0xBB; 110])]);

        // 1. Trigger a 3-way split directly on a root TableLeaf (`[10: ~905B, 30: ~115B]`, then insert `20: ~905B`!):
        // Neither `[10] | [20, 30]` (~1020B > 1016B) nor `[10, 20] | [30]` (~1810B > 1016B) fits in 2 pages!
        let t_root = bt.create_table().unwrap();
        bt.insert_table(t_root, 10, &p900).unwrap();
        bt.insert_table(t_root, 30, &p110).unwrap();
        bt.insert_table(t_root, 20, &p900).unwrap();
        assert!(bt.integrity_check(&[1, t_root]).unwrap().is_empty());
        let rows_root: Vec<i64> = bt
            .scan_table(t_root)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(rows_root, vec![10, 20, 30]);

        // 2. Trigger a 3-way split on a non-root child TableLeaf spliced into a `TableInterior` parent!
        let t_nonroot = bt.create_table().unwrap();
        for r in 1..=25i64 {
            bt.insert_table(
                t_nonroot,
                r * 10,
                &encode_record(&[Value::Blob(vec![0xCC; 110])]),
            )
            .unwrap();
        }
        // Prepend `-15` (~905B) into leftmost leaf alongside `10..` (~115B+), then insert `-14` (~905B) in between!
        bt.insert_table(t_nonroot, -15, &p900).unwrap();
        bt.insert_table(t_nonroot, -14, &p900).unwrap();
        assert!(
            bt.integrity_check(&[1, t_root, t_nonroot])
                .unwrap()
                .is_empty()
        );
        let scanned = bt.scan_table(t_nonroot).unwrap();
        assert_eq!(scanned.len(), 27);
        assert_eq!(scanned[0].0, -15);
        assert_eq!(scanned[1].0, -14);
        assert_eq!(scanned[2].0, 10);
        bt.commit().unwrap();
        bt.close().unwrap();
    }
}
