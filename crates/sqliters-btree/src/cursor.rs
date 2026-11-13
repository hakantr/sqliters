//! Incremental owned B-tree & B+tree cursor state machine (`BtCursor` in `btreeInt.h` / `btree.c`).
//!
//! Avoids materializing whole tables or secondary index matches into `Vec`s, navigates raw
//! slotted pages lazily, decodes single columns without walking untouched overflow chains,
//! and implements `save_all_cursors`, `restore_cursor_position` (`SkipNext`), and `trip_cursors`.

use std::cmp::Ordering;

use sqliters_types::{
    ErrorCode, KeySortOrder, Pgno, Result, SqliteError, TextEncoding, Value,
    decode_record_with_encoding, decode_serial_value_pooled, decode_serial_value_with_encoding,
    extended, get_varint, serial_type_len,
};

use crate::Btree;
use crate::page::{
    BtreePageType, RawIndexCell, binary_search_table_leaf, binary_search_table_leaf_with_span,
    find_table_interior_child, raw_page_header, read_index_interior_cell, read_index_leaf_cell,
    read_table_interior_cell, read_table_leaf_cell, read_table_leaf_rowid,
};

/// Maximum B-tree cursor stack depth (`BTCURSOR_MAX_DEPTH` in `btreeInt.h`).
pub const BTCURSOR_MAX_DEPTH: usize = 20;

/// Owned opaque handle to a live cursor registered inside [`Btree`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CursorId(pub u64);

/// Distinguishes `INTKEY` Table B+Trees from `ZERODATA` Index / `WITHOUT ROWID` B-Trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CursorKind {
    /// `INTKEY` Table B+Tree (`0x0d` leaf / `0x05` interior); payloads live exclusively in leaves.
    Table,
    /// `ZERODATA` Index / `WITHOUT ROWID` B-Tree (`0x0a` leaf / `0x02` interior);
    /// both leaf and interior cells carry records in in-order sequence.
    Index { orders: Vec<KeySortOrder> },
}

/// Key snapshot saved by `save_cursor_position` before a B-tree mutation or partial rollback.
#[derive(Clone, Debug, PartialEq)]
pub enum SavedCursorKey {
    Table(i64),
    Index {
        raw_record: Vec<u8>,
        key_values: Vec<Value>,
    },
}

/// Directional skip state after `restore_cursor_position` lands on a neighboring entry
/// because the previously current key was deleted (`BtCursor.skipNext` in `btreeInt.h`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SkipNext {
    None,
    /// Reseek landed on the first entry `> saved_key`:
    /// - next `cursor_next` consumes this flag and yields the current entry without advancing;
    /// - next `cursor_prev` consumes this flag and steps one entry backward (`< saved_key`).
    SkipForward,
    /// Reseek landed on the last entry `< saved_key`:
    /// - next `cursor_prev` consumes this flag and yields the current entry without retreating;
    /// - next `cursor_next` consumes this flag and steps one entry forward (`> saved_key`, or `Eof`).
    SkipBackward,
}

/// Single `(pgno, cell_idx)` frame in a valid cursor's root-to-current path (`apPage` / `aiIdx`).
///
/// - For ancestor frames (`stack[..stack.len() - 1]`): `pgno` is an interior page and `cell_idx`
///   is in `0..=n_cell` (`0..n_cell` means `cells[cell_idx].left_child`, `n_cell` means `right_child`).
/// - For the top frame (`stack[stack.len() - 1]`): `cell_idx` is in `0..n_cell` and identifies
///   the active cell on that `TableLeaf`, `IndexLeaf`, or `IndexInterior` page.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CursorPagePos {
    pub pgno: Pgno,
    pub cell_idx: usize,
}

/// Inline fixed-capacity root-to-current path stack (`apPage` / `aiIdx` in `BtCursor`),
/// avoiding heap allocation on every cursor seek or scan.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct CursorStack {
    frames: [CursorPagePos; BTCURSOR_MAX_DEPTH],
    len: u8,
}

impl Default for CursorStack {
    #[inline(always)]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorStack {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            frames: [CursorPagePos {
                pgno: 0,
                cell_idx: 0,
            }; BTCURSOR_MAX_DEPTH],
            len: 0,
        }
    }

    #[inline(always)]
    pub fn push(&mut self, pos: CursorPagePos) {
        let idx = self.len as usize;
        self.frames[idx] = pos;
        self.len += 1;
    }

    #[inline(always)]
    pub fn pop(&mut self) -> Option<CursorPagePos> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            Some(self.frames[self.len as usize])
        }
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[CursorPagePos] {
        &self.frames[..self.len as usize]
    }
}

impl std::ops::Deref for CursorStack {
    type Target = [CursorPagePos];

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.frames[..self.len as usize]
    }
}

impl std::ops::DerefMut for CursorStack {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frames[..self.len as usize]
    }
}

impl std::fmt::Debug for CursorStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_slice().fmt(f)
    }
}

/// Lifecycle state of an incremental [`Btree`] cursor (`CURSOR_*` in `btreeInt.h`).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum BtreeCursorState {
    /// Newly opened or unpositioned after a missed point seek.
    Invalid,
    /// Positioned on a live cell at `stack.last()`, optionally carrying an unconsumed [`SkipNext`].
    Valid {
        stack: CursorStack,
        skip_next: SkipNext,
    },
    /// Invalidated by a same-connection B-tree mutation or `write_only` trip; will lazily reseek.
    RequireSeek {
        saved_key: SavedCursorKey,
        skip_next: SkipNext,
    },
    /// Tripped by rollback (`SQLITE_ABORT_ROLLBACK`, etc.); all operations fail with this extended code.
    Fault(i32),
    /// Positioned past the beginning or end of the B-tree.
    Eof,
}

const INLINE_CACHED_COLS: usize = 16;

#[derive(Copy, Clone, Debug)]
pub(crate) struct CellColumnCache {
    pub(crate) valid: bool,
    pub(crate) has_rowid: bool,
    pub(crate) hdr_ready: bool,
    pub(crate) parsed_cols: u8,
    pub(crate) pgno: Pgno,
    pub(crate) rowid: i64,
    pub(crate) payload_start: u16,
    pub(crate) payload_end: u16,
    pub(crate) hdr_end: u16,
    pub(crate) hdr_pos: u16,
    pub(crate) body_pos: u16,
    pub(crate) serials: [u64; INLINE_CACHED_COLS],
    pub(crate) body_offs: [u16; INLINE_CACHED_COLS],
}

impl CellColumnCache {
    #[inline(always)]
    pub(crate) const fn empty() -> Self {
        Self {
            valid: false,
            has_rowid: false,
            hdr_ready: false,
            parsed_cols: 0,
            pgno: 0,
            rowid: 0,
            payload_start: 0,
            payload_end: 0,
            hdr_end: 0,
            hdr_pos: 0,
            body_pos: 0,
            serials: [0; INLINE_CACHED_COLS],
            body_offs: [0; INLINE_CACHED_COLS],
        }
    }

    #[inline(always)]
    pub(crate) fn init_span(&mut self, pgno: Pgno, p_start: u16, p_end: u16, rowid: Option<i64>) {
        self.valid = true;
        self.hdr_ready = false;
        self.parsed_cols = 0;
        self.pgno = pgno;
        self.payload_start = p_start;
        self.payload_end = p_end;
        if let Some(r) = rowid {
            self.has_rowid = true;
            self.rowid = r;
        } else {
            self.has_rowid = false;
            self.rowid = 0;
        }
    }

    #[inline(always)]
    pub(crate) fn clear(&mut self) {
        self.valid = false;
        self.has_rowid = false;
        self.hdr_ready = false;
        self.parsed_cols = 0;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BtreeCursor {
    pub(crate) root_pgno: Pgno,
    pub(crate) kind: CursorKind,
    pub(crate) writable: bool,
    pub(crate) state: BtreeCursorState,
    pub(crate) col_cache: CellColumnCache,
}

#[derive(Clone, Debug)]
pub(crate) struct CursorSlot {
    pub(crate) generation: u32,
    pub(crate) cursor: Option<BtreeCursor>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LowerBoundResult {
    Empty,
    Exact,
    After,
    Before,
}

#[inline]
fn fault_error(code: i32) -> SqliteError {
    let msg = if code == extended::SQLITE_ABORT_ROLLBACK {
        "abort due to ROLLBACK"
    } else {
        ErrorCode::from_raw_primary(code).as_str()
    };
    SqliteError::with_extended(code, msg)
}

#[inline]
fn compare_two_values_with_order(
    a: &Value,
    b: &Value,
    spec: &KeySortOrder,
    enc: TextEncoding,
) -> Result<Ordering> {
    match (a.is_null(), b.is_null()) {
        (true, true) => Ok(Ordering::Equal),
        (true, false) => Ok(if spec.nulls_last {
            Ordering::Greater
        } else {
            Ordering::Less
        }),
        (false, true) => Ok(if spec.nulls_last {
            Ordering::Less
        } else {
            Ordering::Greater
        }),
        (false, false) => {
            let raw = a.cmp_sqlite_fallible(b, spec, enc)?;
            Ok(if spec.desc { raw.reverse() } else { raw })
        }
    }
}

impl Btree {
    // ========================================================================
    // Cursor Registry & Activity Inspection Helpers
    // ========================================================================

    /// Opens a new owned incremental cursor on `root_pgno`.
    pub fn open_cursor(&mut self, root_pgno: Pgno, kind: CursorKind, writable: bool) -> CursorId {
        let generation = self.next_cursor_gen;
        self.next_cursor_gen = self.next_cursor_gen.wrapping_add(1).max(1);
        let cur = BtreeCursor {
            root_pgno,
            kind,
            writable,
            state: BtreeCursorState::Invalid,
            col_cache: CellColumnCache::empty(),
        };
        let slot = if let Some(slot) = self.free_cursor_slots.pop() {
            let entry = &mut self.cursor_slots[slot as usize];
            entry.generation = generation;
            entry.cursor = Some(cur);
            slot
        } else {
            let slot = self.cursor_slots.len() as u32;
            self.cursor_slots.push(CursorSlot {
                generation,
                cursor: Some(cur),
            });
            slot
        };
        self.open_cursor_count += 1;
        CursorId(((slot as u64) << 32) | u64::from(generation))
    }

    /// Convenience helper for opening a Table B+Tree cursor.
    #[inline]
    pub fn open_table_cursor(&mut self, root_pgno: Pgno, writable: bool) -> CursorId {
        self.open_cursor(root_pgno, CursorKind::Table, writable)
    }

    /// Convenience helper for opening an Index / `WITHOUT ROWID` B-Tree cursor.
    #[inline]
    pub fn open_index_cursor(
        &mut self,
        root_pgno: Pgno,
        orders: Vec<KeySortOrder>,
        writable: bool,
    ) -> CursorId {
        self.open_cursor(root_pgno, CursorKind::Index { orders }, writable)
    }

    /// Closes a registered cursor, returning `true` if `id` was open.
    #[inline]
    pub fn close_cursor(&mut self, id: CursorId) -> bool {
        let slot = (id.0 >> 32) as usize;
        let cur_gen = id.0 as u32;
        if let Some(entry) = self.cursor_slots.get_mut(slot)
            && entry.generation == cur_gen
            && entry.cursor.is_some()
        {
            // Dropped in place: `take()` would first move the whole cursor (path stack and
            // column cache) out of the slot.
            entry.cursor = None;
            entry.generation = 0;
            self.free_cursor_slots.push(slot as u32);
            self.open_cursor_count -= 1;
            return true;
        }
        false
    }

    /// Closes every open cursor registered on this `Btree`.
    #[inline]
    pub fn close_all_cursors(&mut self) {
        if self.open_cursor_count == 0 {
            return;
        }
        for (idx, entry) in self.cursor_slots.iter_mut().enumerate() {
            if entry.cursor.take().is_some() {
                entry.generation = 0;
                self.free_cursor_slots.push(idx as u32);
            }
        }
        self.open_cursor_count = 0;
    }

    #[inline(always)]
    fn cursor_ref(&self, id: CursorId) -> Option<&BtreeCursor> {
        let slot = (id.0 >> 32) as usize;
        let cur_gen = id.0 as u32;
        let entry = self.cursor_slots.get(slot)?;
        if entry.generation == cur_gen {
            entry.cursor.as_ref()
        } else {
            None
        }
    }

    #[inline(always)]
    fn cursor_mut(&mut self, id: CursorId) -> Option<&mut BtreeCursor> {
        let slot = (id.0 >> 32) as usize;
        let cur_gen = id.0 as u32;
        let entry = self.cursor_slots.get_mut(slot)?;
        if entry.generation == cur_gen {
            entry.cursor.as_mut()
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn invalidate_all_cursor_caches(&mut self) {
        if self.open_cursor_count == 0 {
            return;
        }
        for entry in &mut self.cursor_slots {
            if let Some(ref mut cur) = entry.cursor {
                cur.col_cache.clear();
            }
        }
    }

    pub(crate) fn remap_open_cursor_roots(&mut self, from_root: Pgno, to_root: Pgno) {
        if self.open_cursor_count == 0 {
            return;
        }
        for entry in &mut self.cursor_slots {
            if let Some(ref mut cur) = entry.cursor
                && cur.root_pgno == from_root
            {
                cur.root_pgno = to_root;
                cur.col_cache.clear();
            }
        }
    }

    /// Inspects the current state of `id` (`None` if closed/unknown).
    #[inline]
    pub fn cursor_state(&self, id: CursorId) -> Option<&BtreeCursorState> {
        self.cursor_ref(id).map(|c| &c.state)
    }

    /// `(leaf_pgno, cell_idx)` of the row a table cursor on `root_pgno` currently rests on, or
    /// `None` unless the cursor is `Valid` with no pending skip (any mutation of that tree since
    /// the cursor was positioned turns it `RequireSeek`). Meant for
    /// [`Btree::insert_table_at`] / [`Btree::delete_table_at`], which re-validate it.
    #[inline]
    pub fn table_cursor_position(&self, id: CursorId, root_pgno: Pgno) -> Option<(Pgno, usize)> {
        let cur = self.cursor_ref(id)?;
        if cur.root_pgno != root_pgno || !matches!(cur.kind, CursorKind::Table) {
            return None;
        }
        match &cur.state {
            BtreeCursorState::Valid {
                stack,
                skip_next: SkipNext::None,
            } => stack.last().map(|top| (top.pgno, top.cell_idx)),
            _ => None,
        }
    }

    /// Returns the `root_pgno` of `id` (`None` if closed/unknown).
    #[inline]
    pub fn cursor_root_pgno(&self, id: CursorId) -> Option<Pgno> {
        self.cursor_ref(id).map(|c| c.root_pgno)
    }

    /// Returns whether `id` was opened as writable (`None` if closed/unknown).
    #[inline]
    pub fn cursor_is_writable(&self, id: CursorId) -> Option<bool> {
        self.cursor_ref(id).map(|c| c.writable)
    }

    /// Number of currently open cursors on this `Btree`.
    #[inline]
    pub fn open_cursor_count(&self) -> usize {
        self.open_cursor_count
    }

    /// Returns `true` if at least one cursor is open on this `Btree`.
    #[inline]
    pub fn has_open_cursors(&self) -> bool {
        self.open_cursor_count > 0
    }

    /// Returns `true` if any cursor other than `exclude` is open on `root_pgno` (`OP_Destroy` guard).
    pub fn has_other_cursors_on_root(&self, root_pgno: Pgno, exclude: Option<CursorId>) -> bool {
        if self.open_cursor_count == 0 {
            return false;
        }
        self.cursor_slots.iter().enumerate().any(|(slot, entry)| {
            if let Some(ref cur) = entry.cursor {
                let cid = CursorId(((slot as u64) << 32) | u64::from(entry.generation));
                Some(cid) != exclude && cur.root_pgno == root_pgno
            } else {
                false
            }
        })
    }

    /// Returns `true` if any cursor other than `exclude` is open anywhere in this `Btree`.
    pub fn has_any_other_open_cursor(&self, exclude: Option<CursorId>) -> bool {
        if self.open_cursor_count == 0 {
            return false;
        }
        self.cursor_slots.iter().enumerate().any(|(slot, entry)| {
            if entry.cursor.is_some() {
                let cid = CursorId(((slot as u64) << 32) | u64::from(entry.generation));
                Some(cid) != exclude
            } else {
                false
            }
        })
    }

    #[inline(always)]
    fn get_cursor(&self, id: CursorId) -> Result<&BtreeCursor> {
        self.cursor_ref(id)
            .ok_or_else(|| SqliteError::misuse(format!("invalid or closed cursor id {}", id.0)))
    }

    #[inline(always)]
    fn check_cursor_not_faulted(&self, id: CursorId) -> Result<()> {
        let cur = self.get_cursor(id)?;
        if let BtreeCursorState::Fault(code) = cur.state {
            return Err(fault_error(code));
        }
        Ok(())
    }

    // ========================================================================
    // Save / Restore / Trip Cursor State Machine (`btree.c`)
    // ========================================================================

    /// Saves a single cursor's current key into `RequireSeek` if it is currently `Valid` (`saveCursorPosition`).
    /// Returns `Ok(true)` if transitioned from `Valid` to `RequireSeek`, or `Ok(false)` if already non-`Valid`.
    pub fn save_cursor_position(&mut self, id: CursorId) -> Result<bool> {
        let (root_pgno, kind, stack, skip_next) = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, skip_next } => {
                    (cur.root_pgno, cur.kind.clone(), *stack, *skip_next)
                }
                BtreeCursorState::RequireSeek { .. }
                | BtreeCursorState::Invalid
                | BtreeCursorState::Eof => return Ok(false),
            }
        };

        let _ = root_pgno;
        let saved_key = match kind {
            CursorKind::Table => {
                let top = *stack
                    .last()
                    .ok_or_else(|| SqliteError::corrupt("empty valid table cursor stack"))?;
                let usable = self.pager.usable_size();
                let raw = self.pager.read_page(top.pgno)?;
                let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
                if ptype != BtreePageType::TableLeaf {
                    return Err(SqliteError::corrupt(
                        "table cursor top frame is not a TableLeaf",
                    ));
                }
                let rowid = read_table_leaf_rowid(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                SavedCursorKey::Table(rowid)
            }
            CursorKind::Index { orders } => {
                let raw_record = self.read_cell_full_payload_at_stack(&stack)?;
                let enc = self.encoding();
                let mut full_vals = decode_record_with_encoding(&raw_record, enc)?;
                let key_len = if orders.is_empty() {
                    full_vals.len()
                } else {
                    orders.len().min(full_vals.len())
                };
                full_vals.truncate(key_len);
                SavedCursorKey::Index {
                    raw_record,
                    key_values: full_vals,
                }
            }
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = BtreeCursorState::RequireSeek {
                saved_key,
                skip_next,
            };
        }
        Ok(true)
    }

    /// Saves all `Valid` cursors matching `root_filter` (`None` = all roots), excluding `exclude` (`saveAllCursors`).
    pub fn save_all_cursors(
        &mut self,
        root_filter: Option<Pgno>,
        exclude: Option<CursorId>,
    ) -> Result<()> {
        if self.open_cursor_count == 0 {
            return Ok(());
        }
        let mut inline_ids = [CursorId(0); 4];
        let mut inline_len = 0usize;
        let mut overflow_ids: Vec<CursorId> = Vec::new();
        for (slot, entry) in self.cursor_slots.iter().enumerate() {
            let Some(ref cur) = entry.cursor else {
                continue;
            };
            let cid = CursorId(((slot as u64) << 32) | u64::from(entry.generation));
            if Some(cid) == exclude {
                continue;
            }
            if let Some(rf) = root_filter
                && cur.root_pgno != rf
            {
                continue;
            }
            if matches!(cur.state, BtreeCursorState::Valid { .. }) {
                if inline_len < inline_ids.len() {
                    inline_ids[inline_len] = cid;
                    inline_len += 1;
                } else {
                    overflow_ids.push(cid);
                }
            }
        }

        for &cid in &inline_ids[..inline_len] {
            self.save_cursor_position(cid)?;
        }
        for cid in overflow_ids {
            self.save_cursor_position(cid)?;
        }
        Ok(())
    }

    /// Pre-mutation hook saving any `Valid` cursors on `root_filter` (except `exclude`).
    #[inline(always)]
    pub fn save_cursors_for_mutation(
        &mut self,
        root_filter: Option<Pgno>,
        exclude: Option<CursorId>,
    ) -> Result<()> {
        if !self.in_place_rows.is_empty() {
            self.drop_in_place_rows(root_filter);
        }
        if self.open_cursor_count == 0 {
            return Ok(());
        }
        self.save_all_cursors(root_filter, exclude)
    }

    /// The write cursor that replaced a row in place loses its position like any other cursor
    /// on the tree being modified.
    #[inline(never)]
    fn drop_in_place_rows(&mut self, root_filter: Option<Pgno>) {
        match root_filter {
            Some(root) => self.in_place_rows.retain(|e| e.0 != root),
            None => self.in_place_rows.clear(),
        }
    }

    /// Trips open cursors on rollback (`sqlite3BtreeTripAllCursors`, `btree.c:4501-4527`):
    /// - `write_only == false`: marks ALL open cursors `Fault(trip_code)`.
    /// - `write_only == true`: marks `writable` cursors `Fault(trip_code)`; saves `Valid` read-only
    ///   cursors to `RequireSeek`. If saving any read-only cursor fails with `err`, immediately
    ///   escalates ALL open cursors on this `Btree` to `Fault(err.extended_code)` and returns `Err(err)`.
    pub fn trip_cursors(&mut self, trip_code: i32, write_only: bool) -> Result<()> {
        if self.open_cursor_count == 0 {
            return Ok(());
        }
        if !write_only {
            for entry in &mut self.cursor_slots {
                if let Some(ref mut cur) = entry.cursor {
                    cur.col_cache.clear();
                    cur.state = BtreeCursorState::Fault(trip_code);
                }
            }
            return Ok(());
        }

        let mut ids = Vec::with_capacity(self.open_cursor_count);
        for (slot, entry) in self.cursor_slots.iter().enumerate() {
            if entry.cursor.is_some() {
                ids.push(CursorId(
                    ((slot as u64) << 32) | u64::from(entry.generation),
                ));
            }
        }
        for cid in ids {
            let (is_writable, is_valid) = match self.cursor_ref(cid) {
                Some(c) => (
                    c.writable,
                    matches!(c.state, BtreeCursorState::Valid { .. }),
                ),
                None => continue,
            };
            if is_writable {
                if let Some(c) = self.cursor_mut(cid) {
                    c.col_cache.clear();
                    c.state = BtreeCursorState::Fault(trip_code);
                }
            } else if is_valid && let Err(err) = self.save_cursor_position(cid) {
                let fault_code = err.extended_code;
                for entry in &mut self.cursor_slots {
                    if let Some(ref mut cur) = entry.cursor {
                        cur.col_cache.clear();
                        cur.state = BtreeCursorState::Fault(fault_code);
                    }
                }
                return Err(err);
            }
        }
        Ok(())
    }

    /// Restores a cursor from `RequireSeek` if needed (`btreeRestoreCursorPosition`, `btree.c:9662-9745`).
    /// Returns `Ok(true)` if the cursor moved to a different row (or `Eof`) because its saved key was deleted.
    pub fn restore_cursor_position(&mut self, id: CursorId) -> Result<bool> {
        let (root_pgno, kind, saved_key, prev_skip) = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::RequireSeek {
                    saved_key,
                    skip_next,
                } => (
                    cur.root_pgno,
                    cur.kind.clone(),
                    saved_key.clone(),
                    *skip_next,
                ),
                _ => return Ok(false),
            }
        };

        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(true);
        }

        let mut stack = CursorStack::new();
        let outcome = match (&kind, &saved_key) {
            (CursorKind::Table, SavedCursorKey::Table(target_rowid)) => {
                self.seek_table_lower_bound(root_pgno, *target_rowid, &mut stack)?
            }
            (CursorKind::Index { orders }, SavedCursorKey::Index { key_values, .. }) => {
                self.seek_index_lower_bound(root_pgno, key_values, orders, &mut stack)?
            }
            _ => {
                return Err(SqliteError::corrupt(
                    "cursor kind does not match saved cursor key",
                ));
            }
        };

        let (new_state, moved) = match outcome {
            LowerBoundResult::Empty => (BtreeCursorState::Eof, true),
            LowerBoundResult::Exact => (
                BtreeCursorState::Valid {
                    stack,
                    skip_next: prev_skip,
                },
                prev_skip != SkipNext::None,
            ),
            LowerBoundResult::After => (
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::SkipForward,
                },
                true,
            ),
            LowerBoundResult::Before => (
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::SkipBackward,
                },
                true,
            ),
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = new_state;
        }
        Ok(moved)
    }

    // ========================================================================
    // Cursor Navigation (`First`, `Last`, `Next`, `Prev`, `Seek*`)
    // ========================================================================

    /// Returns `true` if the cursor is at `Eof` (or `Invalid`), restoring `RequireSeek` if necessary.
    pub fn cursor_is_eof(&mut self, id: CursorId) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }
        let cur = self.get_cursor(id)?;
        Ok(matches!(
            cur.state,
            BtreeCursorState::Eof | BtreeCursorState::Invalid
        ))
    }

    /// Positions `id` on the first entry in the B-tree (`sqlite3BtreeFirst` / `OP_Rewind`).
    /// Returns `Ok(true)` if non-empty, or `Ok(false)` (`Eof`) if the tree is empty.
    pub fn cursor_first(&mut self, id: CursorId) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;
        let (root_pgno, is_table) = {
            let cur = self.get_cursor(id)?;
            (cur.root_pgno, matches!(cur.kind, CursorKind::Table))
        };

        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(false);
        }

        let mut stack = CursorStack::new();
        let found = if is_table {
            self.descend_table_leftmost(root_pgno, &mut stack)?
        } else {
            self.descend_index_leftmost(root_pgno, &mut stack)?
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if found {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(found)
    }

    /// Positions `id` on the last entry in the B-tree (`sqlite3BtreeLast` / `OP_Last`).
    /// Returns `Ok(true)` if non-empty, or `Ok(false)` (`Eof`) if the tree is empty.
    pub fn cursor_last(&mut self, id: CursorId) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;
        let (root_pgno, is_table) = {
            let cur = self.get_cursor(id)?;
            (cur.root_pgno, matches!(cur.kind, CursorKind::Table))
        };

        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(false);
        }

        let mut stack = CursorStack::new();
        let found = if is_table {
            self.descend_table_rightmost(root_pgno, &mut stack)?
        } else {
            self.descend_index_rightmost(root_pgno, &mut stack)?
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if found {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(found)
    }

    /// Advances `id` to the next entry in sort order (`sqlite3BtreeNext` / `OP_Next`).
    /// Returns `Ok(true)` if positioned on a valid entry, or `Ok(false)` on `Eof`.
    pub fn cursor_next(&mut self, id: CursorId) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;

        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }

        let (is_table, mut stack, skip_next) = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    if let Some(c) = self.cursor_mut(id) {
                        c.col_cache.clear();
                        c.state = BtreeCursorState::Eof;
                    }
                    return Ok(false);
                }
                BtreeCursorState::Valid { stack, skip_next } => {
                    (matches!(cur.kind, CursorKind::Table), *stack, *skip_next)
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };

        if skip_next == SkipNext::SkipForward {
            if let Some(cur) = self.cursor_mut(id) {
                cur.state = BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                };
            }
            return Ok(true);
        }

        let has_more = if is_table {
            self.advance_table_stack(&mut stack)?
        } else {
            self.advance_index_stack(&mut stack)?
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if has_more {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(has_more)
    }

    /// Moves `id` to the previous entry in sort order (`sqlite3BtreePrevious` / `OP_Prev`).
    /// Returns `Ok(true)` if positioned on a valid entry, or `Ok(false)` on `Eof`.
    pub fn cursor_prev(&mut self, id: CursorId) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;

        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }

        let (is_table, mut stack, skip_next) = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    if let Some(c) = self.cursor_mut(id) {
                        c.col_cache.clear();
                        c.state = BtreeCursorState::Eof;
                    }
                    return Ok(false);
                }
                BtreeCursorState::Valid { stack, skip_next } => {
                    (matches!(cur.kind, CursorKind::Table), *stack, *skip_next)
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };

        if skip_next == SkipNext::SkipBackward {
            if let Some(cur) = self.cursor_mut(id) {
                cur.state = BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                };
            }
            return Ok(true);
        }

        let has_more = if is_table {
            self.retreat_table_stack(&mut stack)?
        } else {
            self.retreat_index_stack(&mut stack)?
        };

        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if has_more {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(has_more)
    }

    /// Exact point-seek on a Table B+Tree cursor (`OP_SeekRowid`).
    /// Returns `Ok(true)` on exact `rowid` match (`Valid`), or `Ok(false)` on miss (`Invalid`).
    pub fn cursor_seek_rowid(&mut self, id: CursorId, rowid: i64) -> Result<bool> {
        self.check_interrupted()?;
        let root_pgno = {
            let cur = self.get_cursor(id)?;
            if let BtreeCursorState::Fault(code) = cur.state {
                return Err(fault_error(code));
            }
            if !matches!(cur.kind, CursorKind::Table) {
                return Err(SqliteError::misuse(
                    "cursor_seek_rowid requires a Table cursor",
                ));
            }
            cur.root_pgno
        };

        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Invalid;
            }
            return Ok(false);
        }

        let usable = self.pager.usable_size();
        let mut stack = CursorStack::new();
        let mut cur_pgno = root_pgno;

        for _ in 0..BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let next_step = {
                let raw = self.pager.read_page(cur_pgno)?;
                let (ptype, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                match ptype {
                    BtreePageType::TableLeaf => {
                        match binary_search_table_leaf_with_span(
                            cur_pgno, usable, raw, n_cell, rowid,
                        )? {
                            Ok((exact_idx, inline_span)) => {
                                stack.push(CursorPagePos {
                                    pgno: cur_pgno,
                                    cell_idx: exact_idx,
                                });
                                Ok(Some((cur_pgno, inline_span)))
                            }
                            Err(_) => Ok(None),
                        }
                    }
                    BtreePageType::TableInterior => {
                        let (idx, child) = find_table_interior_child(cur_pgno, usable, raw, rowid)?;
                        stack.push(CursorPagePos {
                            pgno: cur_pgno,
                            cell_idx: idx,
                        });
                        Err(child)
                    }
                    other => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table B-tree page at {cur_pgno}, found {other:?}"
                        )));
                    }
                }
            };
            match next_step {
                Ok(Some((leaf_pgno, inline_span))) => {
                    if let Some(cur) = self.cursor_mut(id) {
                        if let Some((p_start, p_end)) = inline_span {
                            cur.col_cache
                                .init_span(leaf_pgno, p_start, p_end, Some(rowid));
                        } else {
                            cur.col_cache.clear();
                        }
                        cur.state = BtreeCursorState::Valid {
                            stack,
                            skip_next: SkipNext::None,
                        };
                    }
                    return Ok(true);
                }
                Ok(None) => {
                    if let Some(cur) = self.cursor_mut(id) {
                        cur.col_cache.clear();
                        cur.state = BtreeCursorState::Invalid;
                    }
                    return Ok(false);
                }
                Err(child_pgno) => cur_pgno = child_pgno,
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in cursor_seek_rowid",
        ))
    }

    /// Lower-bound seek (`>= target_rowid`) on a Table B+Tree cursor (`OP_SeekGE`).
    pub fn cursor_seek_rowid_ge(&mut self, id: CursorId, target_rowid: i64) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;
        let root_pgno = {
            let cur = self.get_cursor(id)?;
            if !matches!(cur.kind, CursorKind::Table) {
                return Err(SqliteError::misuse(
                    "cursor_seek_rowid_ge requires a Table cursor",
                ));
            }
            cur.root_pgno
        };
        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(false);
        }
        let mut stack = CursorStack::new();
        let outcome = self.seek_table_lower_bound(root_pgno, target_rowid, &mut stack)?;
        let hit = matches!(outcome, LowerBoundResult::Exact | LowerBoundResult::After);
        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if hit {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(hit)
    }

    /// Lower-bound seek (`>= key_values`) on an Index B-Tree cursor (`OP_SeekGE`).
    pub fn cursor_seek_index_ge(&mut self, id: CursorId, key_values: &[Value]) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;
        let (root_pgno, orders) = {
            let cur = self.get_cursor(id)?;
            match &cur.kind {
                CursorKind::Index { orders } => (cur.root_pgno, orders.clone()),
                CursorKind::Table => {
                    return Err(SqliteError::misuse(
                        "cursor_seek_index_ge requires an Index cursor",
                    ));
                }
            }
        };
        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(false);
        }
        let mut stack = CursorStack::new();
        let outcome = self.seek_index_lower_bound(root_pgno, key_values, &orders, &mut stack)?;
        let hit = matches!(outcome, LowerBoundResult::Exact | LowerBoundResult::After);
        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if hit {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(hit)
    }

    /// Positions an Index cursor on the FIRST entry matching `prefix_values` (`OP_SeekIndexEqual`).
    /// Transitions to `Eof` and returns `Ok(false)` if no entry matches `prefix_values`.
    pub fn cursor_seek_index_equal_first(
        &mut self,
        id: CursorId,
        prefix_values: &[Value],
    ) -> Result<bool> {
        self.check_cursor_not_faulted(id)?;
        self.check_interrupted()?;
        let (root_pgno, orders) = {
            let cur = self.get_cursor(id)?;
            match &cur.kind {
                CursorKind::Index { orders } => (cur.root_pgno, orders.clone()),
                CursorKind::Table => {
                    return Err(SqliteError::misuse(
                        "cursor_seek_index_equal_first requires an Index cursor",
                    ));
                }
            }
        };

        if self.pager.page_count() == 0 || root_pgno == 0 {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            return Ok(false);
        }

        let mut stack = CursorStack::new();
        let outcome = self.seek_index_lower_bound(root_pgno, prefix_values, &orders, &mut stack)?;
        let matched = outcome == LowerBoundResult::Exact;
        if let Some(cur) = self.cursor_mut(id) {
            cur.col_cache.clear();
            cur.state = if matched {
                BtreeCursorState::Valid {
                    stack,
                    skip_next: SkipNext::None,
                }
            } else {
                BtreeCursorState::Eof
            };
        }
        Ok(matched)
    }

    /// Advances an Index cursor to the next entry still matching `prefix_values` (`OP_IndexMatchNext`).
    /// Transitions to `Eof` and returns `Ok(false)` once the prefix no longer matches or `Eof` is reached.
    pub fn cursor_next_index_matching(
        &mut self,
        id: CursorId,
        prefix_values: &[Value],
    ) -> Result<bool> {
        if !self.cursor_next(id)? {
            return Ok(false);
        }
        let (orders, stack) = {
            let cur = self.get_cursor(id)?;
            let orders = match &cur.kind {
                CursorKind::Index { orders } => orders.clone(),
                CursorKind::Table => {
                    return Err(SqliteError::misuse(
                        "cursor_next_index_matching requires an Index cursor",
                    ));
                }
            };
            let BtreeCursorState::Valid { stack, .. } = &cur.state else {
                return Ok(false);
            };
            (orders, *stack)
        };

        let top = *stack
            .last()
            .ok_or_else(|| SqliteError::corrupt("empty valid index cursor stack"))?;
        let ord = self.compare_index_cell_at(top.pgno, top.cell_idx, prefix_values, &orders)?;
        if ord == Ordering::Equal {
            Ok(true)
        } else {
            if let Some(cur) = self.cursor_mut(id) {
                cur.col_cache.clear();
                cur.state = BtreeCursorState::Eof;
            }
            Ok(false)
        }
    }

    // ========================================================================
    // Lazy Data Accessors (`cursor_rowid`, `cursor_payload`, `cursor_column`)
    // ========================================================================

    /// Reads the `rowid` of the current entry without touching overflow pages when possible.
    /// - For `Table` cursors: reads the cell's `rowid` varint directly from the leaf page.
    /// - For `Index` cursors: extracts the trailing `rowid` column of the index record.
    pub fn cursor_rowid(&mut self, id: CursorId) -> Result<i64> {
        self.check_cursor_not_faulted(id)?;
        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }

        let (is_table, top, cached_rowid) = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, .. } => {
                    let top = *stack
                        .last()
                        .ok_or_else(|| SqliteError::corrupt("empty valid cursor stack"))?;
                    let cr = if cur.col_cache.valid && cur.col_cache.has_rowid {
                        Some(cur.col_cache.rowid)
                    } else {
                        None
                    };
                    (matches!(cur.kind, CursorKind::Table), top, cr)
                }
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    return Err(SqliteError::misuse(
                        "cursor_rowid called on unpositioned/EOF cursor",
                    ));
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };

        if let Some(rid) = cached_rowid {
            return Ok(rid);
        }

        if is_table {
            let usable = self.pager.usable_size();
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
            if ptype != BtreePageType::TableLeaf {
                return Err(SqliteError::corrupt(
                    "table cursor top frame is not a TableLeaf",
                ));
            }
            read_table_leaf_rowid(top.pgno, usable, raw, n_cell, top.cell_idx)
        } else {
            match self.cursor_last_column(id)? {
                Some(Value::Integer(rid)) => Ok(rid),
                Some(other) => Ok(other.to_integer_lossy()),
                None => Err(SqliteError::corrupt("empty index record in cursor_rowid")),
            }
        }
    }

    /// Reads the full reassembled record payload of the current entry.
    pub fn cursor_payload(&mut self, id: CursorId) -> Result<Vec<u8>> {
        self.check_cursor_not_faulted(id)?;
        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }

        let stack = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, .. } => *stack,
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    return Err(SqliteError::misuse(
                        "cursor_payload called on unpositioned/EOF cursor",
                    ));
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };
        self.read_cell_full_payload_at_stack(&stack)
    }

    /// [`Self::cursor_payload`] into `out` (cleared first), keeping its allocation.
    pub fn cursor_payload_into(&mut self, id: CursorId, out: &mut Vec<u8>) -> Result<()> {
        self.check_cursor_not_faulted(id)?;
        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }
        let top = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, .. } => stack.last().copied(),
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    return Err(SqliteError::misuse(
                        "cursor_payload called on unpositioned/EOF cursor",
                    ));
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };
        out.clear();
        if let Some(top) = top {
            let usable = self.pager.usable_size();
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
            let local = match ptype {
                BtreePageType::TableLeaf => {
                    let c = read_table_leaf_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                    (c.overflow_page.is_none()
                        && c.local_payload.len() >= c.total_payload_len as usize)
                        .then(|| &c.local_payload[..c.total_payload_len as usize])
                }
                BtreePageType::IndexLeaf => {
                    let c = read_index_leaf_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                    (c.overflow_page.is_none()
                        && c.local_payload.len() >= c.total_payload_len as usize)
                        .then(|| &c.local_payload[..c.total_payload_len as usize])
                }
                _ => None,
            };
            if let Some(bytes) = local {
                out.extend_from_slice(bytes);
                return Ok(());
            }
        }
        *out = self.cursor_payload(id)?;
        Ok(())
    }

    /// Lazily decodes 0-based column `col_idx` of the current entry (`OP_Column`).
    ///
    /// If `col_idx`'s header and payload bytes reside completely within `local_payload`,
    /// **zero overflow pages** are read! Even when `col_idx` spills into overflow pages,
    /// only overflow pages up to the end of `col_idx` are read.
    #[inline]
    pub fn cursor_column(&mut self, id: CursorId, col_idx: usize) -> Result<Option<Value>> {
        self.cursor_column_inner(id, col_idx, None)
    }

    /// Lazily decodes 0-based column `col_idx` of the current entry (`OP_Column`), reusing
    /// pooled `String` / `Vec<u8>` buffers when `col_idx` resides inline on the page.
    #[inline]
    pub fn cursor_column_pooled(
        &mut self,
        id: CursorId,
        col_idx: usize,
        texts: &mut Vec<String>,
        blobs: &mut Vec<Vec<u8>>,
    ) -> Result<Option<Value>> {
        self.cursor_column_inner(id, col_idx, Some((texts, blobs)))
    }

    fn cursor_column_inner(
        &mut self,
        id: CursorId,
        col_idx: usize,
        pools: Option<(&mut Vec<String>, &mut Vec<Vec<u8>>)>,
    ) -> Result<Option<Value>> {
        let top = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, .. } => *stack
                    .last()
                    .ok_or_else(|| SqliteError::corrupt("empty valid cursor stack"))?,
                BtreeCursorState::RequireSeek { .. } => {
                    self.restore_cursor_position(id)?;
                    let cur = self.get_cursor(id)?;
                    match &cur.state {
                        BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                        BtreeCursorState::Valid { stack, .. } => *stack
                            .last()
                            .ok_or_else(|| SqliteError::corrupt("empty valid cursor stack"))?,
                        _ => {
                            return Err(SqliteError::misuse(
                                "cursor_column called on unpositioned/EOF cursor",
                            ));
                        }
                    }
                }
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    return Err(SqliteError::misuse(
                        "cursor_column called on unpositioned/EOF cursor",
                    ));
                }
            }
        };

        if col_idx < INLINE_CACHED_COLS {
            let enc = self.encoding();
            let usable = self.pager.usable_size();
            if usable <= u32::from(u16::MAX) {
                let slot_idx = (id.0 >> 32) as usize;
                let cur_gen = id.0 as u32;
                let inline_res = if let Some(entry) = self.cursor_slots.get_mut(slot_idx)
                    && entry.generation == cur_gen
                    && let Some(ref mut cur) = entry.cursor
                {
                    let cache = &mut cur.col_cache;
                    let raw = self.pager.read_page(top.pgno)?;
                    if !cache.valid {
                        let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
                        match ptype {
                            BtreePageType::TableLeaf => {
                                let c = read_table_leaf_cell(
                                    top.pgno,
                                    usable,
                                    raw,
                                    n_cell,
                                    top.cell_idx,
                                )?;
                                if c.overflow_page.is_none()
                                    && c.local_payload.len() >= c.total_payload_len as usize
                                {
                                    let p_start = (c.local_payload.as_ptr() as usize
                                        - raw.as_ptr() as usize)
                                        as u16;
                                    let p_end = p_start + (c.total_payload_len as u16);
                                    cache.init_span(top.pgno, p_start, p_end, Some(c.rowid));
                                }
                            }
                            BtreePageType::IndexLeaf => {
                                let c = read_index_leaf_cell(
                                    top.pgno,
                                    usable,
                                    raw,
                                    n_cell,
                                    top.cell_idx,
                                )?;
                                if c.overflow_page.is_none()
                                    && c.local_payload.len() >= c.total_payload_len as usize
                                {
                                    let p_start = (c.local_payload.as_ptr() as usize
                                        - raw.as_ptr() as usize)
                                        as u16;
                                    let p_end = p_start + (c.total_payload_len as u16);
                                    cache.init_span(top.pgno, p_start, p_end, None);
                                }
                            }
                            BtreePageType::IndexInterior => {
                                let (_, c) = read_index_interior_cell(
                                    top.pgno,
                                    usable,
                                    raw,
                                    n_cell,
                                    top.cell_idx,
                                )?;
                                if c.overflow_page.is_none()
                                    && c.local_payload.len() >= c.total_payload_len as usize
                                {
                                    let p_start = (c.local_payload.as_ptr() as usize
                                        - raw.as_ptr() as usize)
                                        as u16;
                                    let p_end = p_start + (c.total_payload_len as u16);
                                    cache.init_span(top.pgno, p_start, p_end, None);
                                }
                            }
                            BtreePageType::TableInterior => {
                                return Err(SqliteError::corrupt(
                                    "cannot decode column from TableInterior page",
                                ));
                            }
                        }
                    }

                    if cache.valid {
                        let p_start = cache.payload_start as usize;
                        let p_end = cache.payload_end as usize;
                        if p_start == p_end {
                            Some(Ok(None))
                        } else {
                            let hdr_ok = if !cache.hdr_ready {
                                match get_varint(&raw[p_start..p_end]) {
                                    Ok((hdr_u64, used)) => {
                                        if let Ok(hdr_sz) = usize::try_from(hdr_u64)
                                            && hdr_sz <= p_end - p_start
                                            && used <= hdr_sz
                                        {
                                            cache.hdr_end = (p_start + hdr_sz) as u16;
                                            cache.hdr_pos = (p_start + used) as u16;
                                            cache.body_pos = cache.hdr_end;
                                            cache.hdr_ready = true;
                                            Ok(())
                                        } else {
                                            Err(SqliteError::corrupt(
                                                "record header exceeds record length",
                                            ))
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            } else {
                                Ok(())
                            };

                            match hdr_ok {
                                Err(e) => {
                                    cache.clear();
                                    Some(Err(e))
                                }
                                Ok(()) => {
                                    let hdr_end = cache.hdr_end as usize;
                                    let mut parse_err = None;
                                    while (cache.parsed_cols as usize) <= col_idx
                                        && (cache.hdr_pos as usize) < hdr_end
                                    {
                                        match get_varint(&raw[cache.hdr_pos as usize..hdr_end]) {
                                            Ok((st, used)) => match serial_type_len(st) {
                                                Ok(len) => {
                                                    let bpos = cache.body_pos as usize;
                                                    match bpos.checked_add(len) {
                                                        Some(next_b) if next_b <= p_end => {
                                                            let ci = cache.parsed_cols as usize;
                                                            cache.serials[ci] = st;
                                                            cache.body_offs[ci] = cache.body_pos;
                                                            cache.hdr_pos += used as u16;
                                                            cache.body_pos = next_b as u16;
                                                            cache.parsed_cols += 1;
                                                        }
                                                        _ => {
                                                            parse_err = Some(SqliteError::corrupt(
                                                                "record payload truncated",
                                                            ));
                                                            break;
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    parse_err = Some(e);
                                                    break;
                                                }
                                            },
                                            Err(e) => {
                                                parse_err = Some(e);
                                                break;
                                            }
                                        }
                                    }

                                    if let Some(e) = parse_err {
                                        cache.clear();
                                        Some(Err(e))
                                    } else if col_idx >= cache.parsed_cols as usize {
                                        Some(Ok(None))
                                    } else {
                                        let st = cache.serials[col_idx];
                                        match serial_type_len(st) {
                                            Ok(len) => {
                                                let start = cache.body_offs[col_idx] as usize;
                                                let slice = &raw[start..start + len];
                                                let decoded = match pools {
                                                    Some((texts, blobs)) => {
                                                        decode_serial_value_pooled(
                                                            st, slice, enc, texts, blobs,
                                                        )
                                                    }
                                                    None => decode_serial_value_with_encoding(
                                                        st, slice, enc,
                                                    ),
                                                };
                                                Some(decoded.map(Some))
                                            }
                                            Err(e) => Some(Err(e)),
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(res) = inline_res {
                    return res;
                }
            }
        }

        self.read_cell_column_lazy(top.pgno, top.cell_idx, Some(col_idx))
    }

    /// Lazily decodes the LAST column of the current entry (e.g. trailing `rowid` on secondary indexes).
    pub fn cursor_last_column(&mut self, id: CursorId) -> Result<Option<Value>> {
        self.check_cursor_not_faulted(id)?;
        if matches!(
            self.get_cursor(id)?.state,
            BtreeCursorState::RequireSeek { .. }
        ) {
            self.restore_cursor_position(id)?;
        }

        let top = {
            let cur = self.get_cursor(id)?;
            match &cur.state {
                BtreeCursorState::Fault(code) => return Err(fault_error(*code)),
                BtreeCursorState::Valid { stack, .. } => *stack
                    .last()
                    .ok_or_else(|| SqliteError::corrupt("empty valid cursor stack"))?,
                BtreeCursorState::Eof | BtreeCursorState::Invalid => {
                    return Err(SqliteError::misuse(
                        "cursor_last_column called on unpositioned/EOF cursor",
                    ));
                }
                BtreeCursorState::RequireSeek { .. } => unreachable!("restored above"),
            }
        };

        self.read_cell_column_lazy(top.pgno, top.cell_idx, None)
    }

    // ========================================================================
    // Internal Lazy Cell / Overflow Helpers
    // ========================================================================

    fn read_cell_full_payload_at_stack(&mut self, stack: &[CursorPagePos]) -> Result<Vec<u8>> {
        let top = *stack
            .last()
            .ok_or_else(|| SqliteError::corrupt("empty cursor stack"))?;
        let usable = self.pager.usable_size();
        let (total_len, local_copy, ovfl) = {
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
            match ptype {
                BtreePageType::TableLeaf => {
                    let c = read_table_leaf_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                    if c.overflow_page.is_none()
                        && c.local_payload.len() >= c.total_payload_len as usize
                    {
                        return Ok(c.local_payload[..c.total_payload_len as usize].to_vec());
                    }
                    (
                        c.total_payload_len,
                        c.local_payload.to_vec(),
                        c.overflow_page,
                    )
                }
                BtreePageType::IndexLeaf => {
                    let c = read_index_leaf_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                    if c.overflow_page.is_none()
                        && c.local_payload.len() >= c.total_payload_len as usize
                    {
                        return Ok(c.local_payload[..c.total_payload_len as usize].to_vec());
                    }
                    (
                        c.total_payload_len,
                        c.local_payload.to_vec(),
                        c.overflow_page,
                    )
                }
                BtreePageType::IndexInterior => {
                    let (_, c) =
                        read_index_interior_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?;
                    if c.overflow_page.is_none()
                        && c.local_payload.len() >= c.total_payload_len as usize
                    {
                        return Ok(c.local_payload[..c.total_payload_len as usize].to_vec());
                    }
                    (
                        c.total_payload_len,
                        c.local_payload.to_vec(),
                        c.overflow_page,
                    )
                }
                BtreePageType::TableInterior => {
                    return Err(SqliteError::corrupt(
                        "cannot read payload from TableInterior page",
                    ));
                }
            }
        };
        self.read_full_payload(total_len, &local_copy, ovfl)
    }

    /// Reads only the first `needed_len` bytes (`<= total_len`) of a cell payload, stopping
    /// overflow chain traversal as soon as `needed_len` bytes have been assembled!
    pub(crate) fn read_payload_prefix(
        &mut self,
        total_len: u32,
        local_payload: &[u8],
        mut next_ovfl: Option<Pgno>,
        needed_len: usize,
    ) -> Result<Vec<u8>> {
        let total = total_len as usize;
        let target = needed_len.min(total);
        if local_payload.len() >= target {
            return Ok(local_payload[..target].to_vec());
        }
        let usable = self.pager.usable_size() as usize;
        let mut out = Vec::with_capacity(target);
        out.extend_from_slice(local_payload);

        let max_hops = self.pager.page_count() as usize + 1;
        let mut hops = 0usize;
        while out.len() < target {
            self.check_interrupted()?;
            let Some(ovfl_pgno) = next_ovfl.filter(|&p| p != 0) else {
                return Err(SqliteError::corrupt(
                    "overflow chain ended prematurely before requested column end",
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
            let remaining = target - out.len();
            let take = remaining.min(usable - 4);
            out.extend_from_slice(&raw[4..4 + take]);
            next_ovfl = if nxt == 0 { None } else { Some(nxt) };
        }
        Ok(out)
    }

    /// If `target_col` is `Some(col_idx)`, decodes that column; if `None`, decodes the last column.
    fn read_cell_column_lazy(
        &mut self,
        pgno: Pgno,
        cell_idx: usize,
        target_col: Option<usize>,
    ) -> Result<Option<Value>> {
        enum FastColumnPlan {
            Done(Option<Value>),
            NeedRange {
                serial_type: u64,
                body_start: usize,
                body_end: usize,
                total_len: u32,
                local_copy: Vec<u8>,
                ovfl: Option<Pgno>,
            },
            HeaderSpilled {
                total_len: u32,
                local_copy: Vec<u8>,
                ovfl: Option<Pgno>,
            },
        }

        let enc = self.encoding();
        let usable = self.pager.usable_size();

        let plan = {
            let raw = self.pager.read_page(pgno)?;
            let (ptype, n_cell, _) = raw_page_header(pgno, usable, raw)?;
            let (total_len, local_slice, ovfl) = match ptype {
                BtreePageType::TableLeaf => {
                    let c = read_table_leaf_cell(pgno, usable, raw, n_cell, cell_idx)?;
                    (c.total_payload_len, c.local_payload, c.overflow_page)
                }
                BtreePageType::IndexLeaf => {
                    let c = read_index_leaf_cell(pgno, usable, raw, n_cell, cell_idx)?;
                    (c.total_payload_len, c.local_payload, c.overflow_page)
                }
                BtreePageType::IndexInterior => {
                    let (_, c) = read_index_interior_cell(pgno, usable, raw, n_cell, cell_idx)?;
                    (c.total_payload_len, c.local_payload, c.overflow_page)
                }
                BtreePageType::TableInterior => {
                    return Err(SqliteError::corrupt(
                        "cannot decode column from TableInterior page",
                    ));
                }
            };

            let total = total_len as usize;
            if total == 0 {
                FastColumnPlan::Done(None)
            } else if local_slice.len() < total && local_slice.len() < 9 {
                FastColumnPlan::HeaderSpilled {
                    total_len,
                    local_copy: local_slice.to_vec(),
                    ovfl,
                }
            } else {
                let (hdr_size_u64, mut hdr_pos) = get_varint(local_slice)?;
                let hdr_size = usize::try_from(hdr_size_u64)
                    .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
                if hdr_size > total || hdr_pos > hdr_size {
                    return Err(SqliteError::corrupt("record header exceeds record length"));
                }
                if hdr_size > local_slice.len() {
                    FastColumnPlan::HeaderSpilled {
                        total_len,
                        local_copy: local_slice.to_vec(),
                        ovfl,
                    }
                } else {
                    let mut body_pos = hdr_size;
                    let mut idx = 0usize;
                    let mut chosen: Option<(u64, usize, usize)> = None;
                    while hdr_pos < hdr_size {
                        let (st, used) = get_varint(&local_slice[hdr_pos..hdr_size])?;
                        hdr_pos += used;
                        let len = serial_type_len(st)?;
                        let end = body_pos
                            .checked_add(len)
                            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
                        if end > total {
                            return Err(SqliteError::corrupt("record payload truncated"));
                        }
                        match target_col {
                            Some(want) if idx == want => {
                                chosen = Some((st, body_pos, end));
                                break;
                            }
                            None => {
                                chosen = Some((st, body_pos, end));
                            }
                            _ => {}
                        }
                        body_pos = end;
                        idx += 1;
                    }

                    match chosen {
                        None => FastColumnPlan::Done(None),
                        Some((st, start, end)) => {
                            if start == end {
                                FastColumnPlan::Done(Some(decode_serial_value_with_encoding(
                                    st,
                                    &[],
                                    enc,
                                )?))
                            } else if end <= local_slice.len() {
                                FastColumnPlan::Done(Some(decode_serial_value_with_encoding(
                                    st,
                                    &local_slice[start..end],
                                    enc,
                                )?))
                            } else {
                                FastColumnPlan::NeedRange {
                                    serial_type: st,
                                    body_start: start,
                                    body_end: end,
                                    total_len,
                                    local_copy: local_slice.to_vec(),
                                    ovfl,
                                }
                            }
                        }
                    }
                }
            }
        };

        match plan {
            FastColumnPlan::Done(v) => Ok(v),
            FastColumnPlan::NeedRange {
                serial_type,
                body_start,
                body_end,
                total_len,
                local_copy,
                ovfl,
            } => {
                let prefix = self.read_payload_prefix(total_len, &local_copy, ovfl, body_end)?;
                let val = decode_serial_value_with_encoding(
                    serial_type,
                    &prefix[body_start..body_end],
                    enc,
                )?;
                Ok(Some(val))
            }
            FastColumnPlan::HeaderSpilled {
                total_len,
                local_copy,
                ovfl,
            } => {
                let total = total_len as usize;
                let init_bytes =
                    self.read_payload_prefix(total_len, &local_copy, ovfl, 9.min(total))?;
                let (hdr_size_u64, mut hdr_pos) = get_varint(&init_bytes)?;
                let hdr_size = usize::try_from(hdr_size_u64)
                    .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
                if hdr_size > total || hdr_pos > hdr_size {
                    return Err(SqliteError::corrupt("record header exceeds record length"));
                }
                let hdr_buf = self.read_payload_prefix(total_len, &local_copy, ovfl, hdr_size)?;
                let mut body_pos = hdr_size;
                let mut idx = 0usize;
                let mut chosen: Option<(u64, usize, usize)> = None;
                while hdr_pos < hdr_size {
                    let (st, used) = get_varint(&hdr_buf[hdr_pos..hdr_size])?;
                    hdr_pos += used;
                    let len = serial_type_len(st)?;
                    let end = body_pos
                        .checked_add(len)
                        .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
                    if end > total {
                        return Err(SqliteError::corrupt("record payload truncated"));
                    }
                    match target_col {
                        Some(want) if idx == want => {
                            chosen = Some((st, body_pos, end));
                            break;
                        }
                        None => {
                            chosen = Some((st, body_pos, end));
                        }
                        _ => {}
                    }
                    body_pos = end;
                    idx += 1;
                }
                let Some((st, start, end)) = chosen else {
                    return Ok(None);
                };
                if start == end {
                    return Ok(Some(decode_serial_value_with_encoding(st, &[], enc)?));
                }
                let prefix = self.read_payload_prefix(total_len, &local_copy, ovfl, end)?;
                Ok(Some(decode_serial_value_with_encoding(
                    st,
                    &prefix[start..end],
                    enc,
                )?))
            }
        }
    }

    /// Lazily compares an index cell at `(pgno, cell_idx)` against `rhs_values` under `orders`!
    /// Avoids reading overflow pages whenever a differentiating column resides in `local_payload`.
    fn compare_index_cell_at(
        &mut self,
        pgno: Pgno,
        cell_idx: usize,
        rhs_values: &[Value],
        orders: &[KeySortOrder],
    ) -> Result<Ordering> {
        if rhs_values.is_empty() {
            return Ok(Ordering::Equal);
        }
        let enc = self.encoding();
        let usable = self.pager.usable_size();

        let spilled = {
            let raw = self.pager.read_page(pgno)?;
            let (ptype, n_cell, _) = raw_page_header(pgno, usable, raw)?;
            let cell: RawIndexCell<'_> = match ptype {
                BtreePageType::IndexLeaf => {
                    read_index_leaf_cell(pgno, usable, raw, n_cell, cell_idx)?
                }
                BtreePageType::IndexInterior => {
                    read_index_interior_cell(pgno, usable, raw, n_cell, cell_idx)?.1
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected index page at {pgno}, found {other:?}"
                    )));
                }
            };

            let total = cell.total_payload_len as usize;
            if total == 0 {
                return Ok(Ordering::Equal);
            }
            if cell.local_payload.len() >= 9 || cell.local_payload.len() == total {
                let (hdr_size_u64, mut hdr_pos) = get_varint(cell.local_payload)?;
                let hdr_size = usize::try_from(hdr_size_u64)
                    .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
                if hdr_size > total || hdr_pos > hdr_size {
                    return Err(SqliteError::corrupt("record header exceeds record length"));
                }
                if hdr_size <= cell.local_payload.len() {
                    let mut body_pos = hdr_size;
                    let mut col_i = 0usize;
                    let mut need_ovfl = false;
                    let mut end_needed = hdr_size;
                    while hdr_pos < hdr_size && col_i < rhs_values.len() {
                        let (st, used) = get_varint(&cell.local_payload[hdr_pos..hdr_size])?;
                        hdr_pos += used;
                        let len = serial_type_len(st)?;
                        let end = body_pos
                            .checked_add(len)
                            .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
                        if end > total {
                            return Err(SqliteError::corrupt("record payload truncated"));
                        }
                        if !need_ovfl && (len == 0 || end <= cell.local_payload.len()) {
                            let slice = if len == 0 {
                                &[][..]
                            } else {
                                &cell.local_payload[body_pos..end]
                            };
                            let a = decode_serial_value_with_encoding(st, slice, enc)?;
                            let default_spec = KeySortOrder::default();
                            let spec = orders.get(col_i).unwrap_or(&default_spec);
                            let ord =
                                compare_two_values_with_order(&a, &rhs_values[col_i], spec, enc)?;
                            if ord != Ordering::Equal {
                                return Ok(ord);
                            }
                        } else {
                            need_ovfl = true;
                            end_needed = end;
                        }
                        body_pos = end;
                        col_i += 1;
                    }
                    if !need_ovfl {
                        return Ok(Ordering::Equal);
                    }
                    Some((
                        cell.total_payload_len,
                        cell.local_payload.to_vec(),
                        cell.overflow_page,
                        Some(end_needed),
                    ))
                } else {
                    Some((
                        cell.total_payload_len,
                        cell.local_payload.to_vec(),
                        cell.overflow_page,
                        None,
                    ))
                }
            } else {
                Some((
                    cell.total_payload_len,
                    cell.local_payload.to_vec(),
                    cell.overflow_page,
                    None,
                ))
            }
        };

        let (total_len, local_copy, ovfl, maybe_end_needed) = spilled.expect("spilled path");
        let buf = match maybe_end_needed {
            Some(end_needed) => {
                self.read_payload_prefix(total_len, &local_copy, ovfl, end_needed)?
            }
            None => self.read_full_payload(total_len, &local_copy, ovfl)?,
        };

        let (hdr_size_u64, mut hdr_pos) = get_varint(&buf)?;
        let hdr_size = usize::try_from(hdr_size_u64)
            .map_err(|_| SqliteError::corrupt("record header size overflow"))?;
        if hdr_size > total_len as usize || hdr_pos > hdr_size || hdr_size > buf.len() {
            return Err(SqliteError::corrupt("record header exceeds record length"));
        }
        let mut body_pos = hdr_size;
        let mut col_i = 0usize;
        while hdr_pos < hdr_size && col_i < rhs_values.len() {
            let (st, used) = get_varint(&buf[hdr_pos..hdr_size])?;
            hdr_pos += used;
            let len = serial_type_len(st)?;
            let end = body_pos
                .checked_add(len)
                .ok_or_else(|| SqliteError::corrupt("record body offset overflow"))?;
            if end > buf.len() {
                return Err(SqliteError::corrupt("record payload truncated"));
            }
            let a = decode_serial_value_with_encoding(st, &buf[body_pos..end], enc)?;
            let default_spec = KeySortOrder::default();
            let spec = orders.get(col_i).unwrap_or(&default_spec);
            let ord = compare_two_values_with_order(&a, &rhs_values[col_i], spec, enc)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
            body_pos = end;
            col_i += 1;
        }
        Ok(Ordering::Equal)
    }

    // ========================================================================
    // Internal Tree Stack Descent, Advance, Retreat, and Lower-Bound Routines
    // ========================================================================

    fn descend_table_leftmost(
        &mut self,
        start_pgno: Pgno,
        stack: &mut CursorStack,
    ) -> Result<bool> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = start_pgno;
        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let raw = self.pager.read_page(cur_pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(cur_pgno, usable, raw)?;
            match ptype {
                BtreePageType::TableLeaf => {
                    if n_cell == 0 {
                        return Ok(false);
                    }
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: 0,
                    });
                    return Ok(true);
                }
                BtreePageType::TableInterior => {
                    let child = if n_cell > 0 {
                        read_table_interior_cell(cur_pgno, usable, raw, n_cell, 0)?.0
                    } else {
                        right_child
                    };
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: 0,
                    });
                    cur_pgno = child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected table page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in descend_table_leftmost",
        ))
    }

    fn descend_table_rightmost(
        &mut self,
        start_pgno: Pgno,
        stack: &mut CursorStack,
    ) -> Result<bool> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = start_pgno;
        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let raw = self.pager.read_page(cur_pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(cur_pgno, usable, raw)?;
            match ptype {
                BtreePageType::TableLeaf => {
                    if n_cell == 0 {
                        return Ok(false);
                    }
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: n_cell - 1,
                    });
                    return Ok(true);
                }
                BtreePageType::TableInterior => {
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: n_cell,
                    });
                    cur_pgno = right_child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected table page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in descend_table_rightmost",
        ))
    }

    fn advance_table_stack(&mut self, stack: &mut CursorStack) -> Result<bool> {
        let usable = self.pager.usable_size();
        let Some(top) = stack.last_mut() else {
            return Ok(false);
        };
        let n_cell = {
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
            if ptype != BtreePageType::TableLeaf {
                return Err(SqliteError::corrupt(
                    "table cursor top frame is not TableLeaf",
                ));
            }
            n_cell
        };
        if top.cell_idx + 1 < n_cell {
            top.cell_idx += 1;
            return Ok(true);
        }

        // Pop exhausted leaf and climb ancestors until we find an interior node with a next child branch.
        stack.pop();
        while let Some(anc) = stack.last_mut() {
            self.check_interrupted()?;
            let next_child = {
                let raw = self.pager.read_page(anc.pgno)?;
                let (ptype, anc_n_cell, right_child) = raw_page_header(anc.pgno, usable, raw)?;
                if ptype != BtreePageType::TableInterior {
                    return Err(SqliteError::corrupt(
                        "table cursor ancestor is not TableInterior",
                    ));
                }
                if anc.cell_idx < anc_n_cell {
                    let next_slot = anc.cell_idx + 1;
                    let child = if next_slot < anc_n_cell {
                        read_table_interior_cell(anc.pgno, usable, raw, anc_n_cell, next_slot)?.0
                    } else {
                        right_child
                    };
                    Some((next_slot, child))
                } else {
                    None
                }
            };
            if let Some((next_slot, child)) = next_child {
                anc.cell_idx = next_slot;
                return self.descend_table_leftmost(child, stack);
            }
            stack.pop();
        }
        Ok(false)
    }

    fn retreat_table_stack(&mut self, stack: &mut CursorStack) -> Result<bool> {
        let usable = self.pager.usable_size();
        let Some(top) = stack.last_mut() else {
            return Ok(false);
        };
        if top.cell_idx > 0 {
            top.cell_idx -= 1;
            return Ok(true);
        }

        // Pop leaf and climb ancestors until we find an interior node with `cell_idx > 0`.
        stack.pop();
        while let Some(anc) = stack.last_mut() {
            self.check_interrupted()?;
            if anc.cell_idx > 0 {
                let prev_slot = anc.cell_idx - 1;
                let child = {
                    let raw = self.pager.read_page(anc.pgno)?;
                    let (ptype, anc_n_cell, _) = raw_page_header(anc.pgno, usable, raw)?;
                    if ptype != BtreePageType::TableInterior {
                        return Err(SqliteError::corrupt(
                            "table cursor ancestor is not TableInterior",
                        ));
                    }
                    read_table_interior_cell(anc.pgno, usable, raw, anc_n_cell, prev_slot)?.0
                };
                anc.cell_idx = prev_slot;
                return self.descend_table_rightmost(child, stack);
            }
            stack.pop();
        }
        Ok(false)
    }

    fn seek_table_lower_bound(
        &mut self,
        root_pgno: Pgno,
        target_rowid: i64,
        stack: &mut CursorStack,
    ) -> Result<LowerBoundResult> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;

        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let step = {
                let raw = self.pager.read_page(cur_pgno)?;
                let (ptype, n_cell, _) = raw_page_header(cur_pgno, usable, raw)?;
                match ptype {
                    BtreePageType::TableLeaf => {
                        if n_cell == 0 {
                            return Ok(LowerBoundResult::Empty);
                        }
                        match binary_search_table_leaf(cur_pgno, usable, raw, n_cell, target_rowid)?
                        {
                            Ok(exact_idx) => {
                                stack.push(CursorPagePos {
                                    pgno: cur_pgno,
                                    cell_idx: exact_idx,
                                });
                                return Ok(LowerBoundResult::Exact);
                            }
                            Err(ins_idx) if ins_idx < n_cell => {
                                stack.push(CursorPagePos {
                                    pgno: cur_pgno,
                                    cell_idx: ins_idx,
                                });
                                return Ok(LowerBoundResult::After);
                            }
                            Err(_) => {
                                // `ins_idx == n_cell`: every cell on this leaf is `< target_rowid`.
                                // Position at `n_cell - 1` and probe whether a next leaf exists!
                                stack.push(CursorPagePos {
                                    pgno: cur_pgno,
                                    cell_idx: n_cell - 1,
                                });
                                Ok(())
                            }
                        }
                    }
                    BtreePageType::TableInterior => {
                        let (idx, child) =
                            find_table_interior_child(cur_pgno, usable, raw, target_rowid)?;
                        stack.push(CursorPagePos {
                            pgno: cur_pgno,
                            cell_idx: idx,
                        });
                        Err(child)
                    }
                    other => {
                        return Err(SqliteError::corrupt(format!(
                            "expected table page at {cur_pgno}, found {other:?}"
                        )));
                    }
                }
            };
            match step {
                Ok(()) => {
                    let mut next_stack = *stack;
                    if self.advance_table_stack(&mut next_stack)? {
                        *stack = next_stack;
                        return Ok(LowerBoundResult::After);
                    }
                    return Ok(LowerBoundResult::Before);
                }
                Err(child_pgno) => cur_pgno = child_pgno,
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in seek_table_lower_bound",
        ))
    }

    // ------------------------------------------------------------------------
    // Index B-Tree In-Order Stack Routines (`left_child[i] -> cell[i] -> ... -> right_child`)
    // ------------------------------------------------------------------------

    fn descend_index_leftmost(
        &mut self,
        start_pgno: Pgno,
        stack: &mut CursorStack,
    ) -> Result<bool> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = start_pgno;
        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let raw = self.pager.read_page(cur_pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(cur_pgno, usable, raw)?;
            match ptype {
                BtreePageType::IndexLeaf => {
                    if n_cell == 0 {
                        return Ok(false);
                    }
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: 0,
                    });
                    return Ok(true);
                }
                BtreePageType::IndexInterior => {
                    let child = if n_cell > 0 {
                        read_index_interior_cell(cur_pgno, usable, raw, n_cell, 0)?.0
                    } else {
                        right_child
                    };
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: 0,
                    });
                    cur_pgno = child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected index page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in descend_index_leftmost",
        ))
    }

    fn descend_index_rightmost(
        &mut self,
        start_pgno: Pgno,
        stack: &mut CursorStack,
    ) -> Result<bool> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = start_pgno;
        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let raw = self.pager.read_page(cur_pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(cur_pgno, usable, raw)?;
            match ptype {
                BtreePageType::IndexLeaf => {
                    if n_cell == 0 {
                        return Ok(false);
                    }
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: n_cell - 1,
                    });
                    return Ok(true);
                }
                BtreePageType::IndexInterior => {
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: n_cell,
                    });
                    cur_pgno = right_child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected index page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }
        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in descend_index_rightmost",
        ))
    }

    /// In-order step forward in an Index B-Tree (`btreeNext` in `btree.c`):
    /// - If top frame is `IndexInterior` at `cell_idx`: step into child slot `cell_idx + 1`
    ///   (`cells[cell_idx + 1].left_child` or `right_child` if `cell_idx + 1 == n_cell`) and descend leftmost!
    /// - If top frame is `IndexLeaf` at `cell_idx`:
    ///   - if `cell_idx + 1 < n_cell`: increment `cell_idx`;
    ///   - else pop leaf and pop any `IndexInterior` ancestors whose `cell_idx == anc_n_cell` (`right_child`);
    ///     if an ancestor remains (`anc.cell_idx < anc_n_cell`), stop directly on that interior cell!
    fn advance_index_stack(&mut self, stack: &mut CursorStack) -> Result<bool> {
        let usable = self.pager.usable_size();
        let Some(top) = stack.last().copied() else {
            return Ok(false);
        };

        let (ptype, n_cell, right_child, next_interior_child) = {
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, right_child) = raw_page_header(top.pgno, usable, raw)?;
            let child = if ptype == BtreePageType::IndexInterior {
                let next_slot = top.cell_idx + 1;
                if next_slot < n_cell {
                    Some(read_index_interior_cell(top.pgno, usable, raw, n_cell, next_slot)?.0)
                } else {
                    Some(right_child)
                }
            } else {
                None
            };
            (ptype, n_cell, right_child, child)
        };
        let _ = right_child;

        match ptype {
            BtreePageType::IndexInterior => {
                let next_slot = top.cell_idx + 1;
                stack.last_mut().expect("non-empty").cell_idx = next_slot;
                self.descend_index_leftmost(next_interior_child.expect("interior child"), stack)
            }
            BtreePageType::IndexLeaf => {
                if top.cell_idx + 1 < n_cell {
                    stack.last_mut().expect("non-empty").cell_idx += 1;
                    return Ok(true);
                }
                stack.pop();
                while let Some(anc) = stack.last().copied() {
                    self.check_interrupted()?;
                    let anc_n_cell = {
                        let raw = self.pager.read_page(anc.pgno)?;
                        let (anc_ptype, anc_n_cell, _) = raw_page_header(anc.pgno, usable, raw)?;
                        if anc_ptype != BtreePageType::IndexInterior {
                            return Err(SqliteError::corrupt(
                                "index cursor ancestor is not IndexInterior",
                            ));
                        }
                        anc_n_cell
                    };
                    if anc.cell_idx < anc_n_cell {
                        // Ancestor descended via `cells[anc.cell_idx].left_child`;
                        // the very next in-order entry is `cells[anc.cell_idx]` itself!
                        return Ok(true);
                    }
                    stack.pop();
                }
                Ok(false)
            }
            other => Err(SqliteError::corrupt(format!(
                "expected index page at {}, found {other:?}",
                top.pgno
            ))),
        }
    }

    /// In-order step backward in an Index B-Tree (`btreePrevious` in `btree.c`):
    /// - If top frame is `IndexInterior` at `cell_idx`: descend rightmost into `cells[cell_idx].left_child`
    ///   (keeping `top.cell_idx = cell_idx` as the ancestor child slot!).
    /// - If top frame is `IndexLeaf` at `cell_idx`:
    ///   - if `cell_idx > 0`: decrement `cell_idx`;
    ///   - else pop leaf and pop any `IndexInterior` ancestors whose `cell_idx == 0`;
    ///     if an ancestor remains (`anc.cell_idx > 0`), decrement `anc.cell_idx -= 1` so the cursor
    ///     sits directly on `cells[anc.cell_idx - 1]`!
    fn retreat_index_stack(&mut self, stack: &mut CursorStack) -> Result<bool> {
        let usable = self.pager.usable_size();
        let Some(top) = stack.last().copied() else {
            return Ok(false);
        };

        let (ptype, left_child_if_interior) = {
            let raw = self.pager.read_page(top.pgno)?;
            let (ptype, n_cell, _) = raw_page_header(top.pgno, usable, raw)?;
            let lchild = if ptype == BtreePageType::IndexInterior {
                Some(read_index_interior_cell(top.pgno, usable, raw, n_cell, top.cell_idx)?.0)
            } else {
                None
            };
            (ptype, lchild)
        };

        match ptype {
            BtreePageType::IndexInterior => {
                self.descend_index_rightmost(left_child_if_interior.expect("left_child"), stack)
            }
            BtreePageType::IndexLeaf => {
                if top.cell_idx > 0 {
                    stack.last_mut().expect("non-empty").cell_idx -= 1;
                    return Ok(true);
                }
                stack.pop();
                while let Some(anc) = stack.last_mut() {
                    self.check_interrupted()?;
                    if anc.cell_idx > 0 {
                        anc.cell_idx -= 1;
                        return Ok(true);
                    }
                    stack.pop();
                }
                Ok(false)
            }
            other => Err(SqliteError::corrupt(format!(
                "expected index page at {}, found {other:?}",
                top.pgno
            ))),
        }
    }

    fn seek_index_lower_bound(
        &mut self,
        root_pgno: Pgno,
        target_values: &[Value],
        orders: &[KeySortOrder],
        stack: &mut CursorStack,
    ) -> Result<LowerBoundResult> {
        let usable = self.pager.usable_size();
        let mut cur_pgno = root_pgno;

        while stack.len() < BTCURSOR_MAX_DEPTH {
            self.check_interrupted()?;
            let (ptype, n_cell, right_child) = {
                let raw = self.pager.read_page(cur_pgno)?;
                raw_page_header(cur_pgno, usable, raw)?
            };

            match ptype {
                BtreePageType::IndexLeaf => {
                    if n_cell == 0 {
                        return Ok(LowerBoundResult::Empty);
                    }
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let ord =
                            self.compare_index_cell_at(cur_pgno, mid, target_values, orders)?;
                        if ord == Ordering::Less {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }

                    if lo < n_cell {
                        stack.push(CursorPagePos {
                            pgno: cur_pgno,
                            cell_idx: lo,
                        });
                        let ord =
                            self.compare_index_cell_at(cur_pgno, lo, target_values, orders)?;
                        return Ok(if ord == Ordering::Equal {
                            LowerBoundResult::Exact
                        } else {
                            LowerBoundResult::After
                        });
                    }

                    // Every cell on this leaf is `< target_values`.
                    // Position at `n_cell - 1` and advance 1 step in-order:
                    // if an ancestor interior cell (or sibling subtree) exists, that is the FIRST entry `>= target_values`!
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: n_cell - 1,
                    });
                    let mut next_stack = *stack;
                    if self.advance_index_stack(&mut next_stack)? {
                        *stack = next_stack;
                        let top = *stack.last().expect("non-empty");
                        let ord = self.compare_index_cell_at(
                            top.pgno,
                            top.cell_idx,
                            target_values,
                            orders,
                        )?;
                        return Ok(if ord == Ordering::Equal {
                            LowerBoundResult::Exact
                        } else {
                            LowerBoundResult::After
                        });
                    }
                    return Ok(LowerBoundResult::Before);
                }
                BtreePageType::IndexInterior => {
                    let mut lo = 0usize;
                    let mut hi = n_cell;
                    while lo < hi {
                        let mid = lo + (hi - lo) / 2;
                        let ord =
                            self.compare_index_cell_at(cur_pgno, mid, target_values, orders)?;
                        if ord == Ordering::Less {
                            lo = mid + 1;
                        } else {
                            hi = mid;
                        }
                    }

                    let child = if lo < n_cell {
                        let raw = self.pager.read_page(cur_pgno)?;
                        read_index_interior_cell(cur_pgno, usable, raw, n_cell, lo)?.0
                    } else {
                        right_child
                    };
                    stack.push(CursorPagePos {
                        pgno: cur_pgno,
                        cell_idx: lo,
                    });
                    cur_pgno = child;
                }
                other => {
                    return Err(SqliteError::corrupt(format!(
                        "expected index page at {cur_pgno}, found {other:?}"
                    )));
                }
            }
        }

        Err(SqliteError::corrupt(
            "btree depth exceeded BTCURSOR_MAX_DEPTH in seek_index_lower_bound",
        ))
    }
}
