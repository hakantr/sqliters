//! `P3v2` PAX (`Partition Attributes Across`) in-page columnar B-tree leaf layouts:
//! - `0x2d` (`PAX_TABLE_LEAF_BYTE`): pre-partitioned columnar `TableLeaf` mini-pages (`pgno >= 2`)
//!   with contiguous sorted `rowids`, homogeneous typed column vectors (`ALL_NULL`, `I32`, `I64`,
//!   `F64`, `BYTES`, and `MIXED` manifest-typing fallback), optional compact non-null bitmaps,
//!   and a downward-growing variable-byte heap (`var_heap`).
//! - `0x2a` (`PAX_INDEX_LEAF_BYTE`): Structure-of-Arrays (`SoA`) covering secondary `IndexLeaf`
//!   mini-pages (`pgno >= 2`) storing contiguous integer key columns (`k0`, optional `k1`) and
//!   `rid` arrays (`I32` / `I64`) with optional non-null bitmaps.

use std::cmp::Ordering;

use sqliters_types::{
    Collation, INLINE_ROW_COLS, InlineRow, InlineVal, KeySortOrder, Pgno, RawField, Result,
    SqliteError, TextEncoding, Value, encode_record_into, encode_value_refs_record_into,
    get_varint, parse_raw_record_prefix, put_varint, serial_type_for, serial_type_len, varint_len,
};

use crate::page::{BtreePageType, IndexLeafCell, TableLeafCell};
use crate::{PointLookupRow, PointProjCol};

/// On-disk page type byte for a `P3v2` PAX columnar `TableLeaf` (`pgno >= 2`).
pub const PAX_TABLE_LEAF_BYTE: u8 = 0x2d;
/// On-disk page type byte for a `P3v2` PAX / SoA `IndexLeaf` (`pgno >= 2`).
pub const PAX_INDEX_LEAF_BYTE: u8 = 0x2a;
/// Layout version byte stored at `raw[1]` on `0x2d` and `0x2a` pages.
pub const PAX_VERSION: u8 = 0x01;
/// Maximum physical column count encoded in `0x2d` PAX leaves; wider tables seamlessly use `0x0d`.
pub const PAX_MAX_COLS: usize = 32;
/// Minimum usable page size for `0x2d` and `0x2a` PAX leaves (`512`-byte pages stay Format-3).
pub const PAX_MIN_USABLE_SIZE: usize = 1024;

pub const PAX_COL_ALL_NULL: u8 = 0;
pub const PAX_COL_I32: u8 = 1;
pub const PAX_COL_I64: u8 = 2;
pub const PAX_COL_F64: u8 = 3;
pub const PAX_COL_BYTES: u8 = 4;
pub const PAX_COL_MIXED: u8 = 5;

/// Flag in `PaxColDir::flags` indicating that `bitmap_off` points to a `cap.div_ceil(8)` non-null bitmap
/// (`bit == 1` => non-null, `bit == 0` => `NULL`).
pub const PAX_COL_HAS_NULLS: u8 = 0x01;

pub const PAX_MIXED_NULL: u8 = 0;
pub const PAX_MIXED_INT: u8 = 1;
pub const PAX_MIXED_REAL: u8 = 2;
pub const PAX_MIXED_TEXT: u8 = 3;
pub const PAX_MIXED_BLOB: u8 = 4;

#[inline(always)]
pub const fn is_pax_table_leaf_byte(b: u8) -> bool {
    b == PAX_TABLE_LEAF_BYTE
}

#[inline(always)]
pub const fn is_pax_index_leaf_byte(b: u8) -> bool {
    b == PAX_INDEX_LEAF_BYTE
}

#[inline(always)]
const fn align8(x: usize) -> usize {
    (x + 7) & !7
}

#[inline(always)]
fn bitmap_get(raw: &[u8], bmap_off: usize, row_idx: usize) -> bool {
    (raw[bmap_off + (row_idx >> 3)] & (1u8 << (row_idx & 7))) != 0
}

#[inline(always)]
fn bitmap_set(raw: &mut [u8], bmap_off: usize, row_idx: usize, val: bool) {
    let byte_idx = bmap_off + (row_idx >> 3);
    let mask = 1u8 << (row_idx & 7);
    if val {
        raw[byte_idx] |= mask;
    } else {
        raw[byte_idx] &= !mask;
    }
}

#[inline(always)]
fn decode_var_heap_top(raw_u16: u16, usable: usize) -> usize {
    if raw_u16 == 0 && usable == 65_536 {
        65_536
    } else {
        usize::from(raw_u16)
    }
}

#[inline(always)]
const fn encode_var_heap_top(top: usize) -> u16 {
    if top == 65_536 { 0 } else { top as u16 }
}

/// Resets `raw[0..usable]` (`pgno >= 2`) to an empty Format-3 `0x0d` (`TableLeaf`) or `0x0a` (`IndexLeaf`).
#[inline]
pub fn reset_empty_slotted_leaf(usable: u32, raw: &mut [u8], ptype: BtreePageType) {
    let u = usable as usize;
    raw[..u].fill(0);
    raw[0] = ptype as u8;
    let enc_top: u16 = if u == 65_536 { 0 } else { u as u16 };
    raw[5..7].copy_from_slice(&enc_top.to_be_bytes());
}

// ============================================================================
// Part 1: `0x2d` PAX Columnar Table Leaf (`PAX_TABLE_LEAF_BYTE = 0x2d`)
// ============================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaxColDir {
    pub mode: u8,
    pub flags: u8,
    pub bitmap_off: u16,
    pub data_off: u16,
}

#[derive(Clone, Copy, Debug)]
struct ComputedTableLayout {
    rowid_off: usize,
    fixed_zone_end: usize,
    dirs: [PaxColDir; PAX_MAX_COLS],
}

fn compute_pax_table_layout(
    n_cols: usize,
    modes: &[u8; PAX_MAX_COLS],
    flags: &[u8; PAX_MAX_COLS],
    cap: usize,
) -> Option<ComputedTableLayout> {
    if n_cols == 0 || n_cols > PAX_MAX_COLS || cap == 0 || cap > 8192 {
        return None;
    }
    let rowid_off = align8(16 + 6 * n_cols);
    let mut pos = rowid_off.checked_add(8usize.checked_mul(cap)?)?;
    let mut dirs = [PaxColDir::default(); PAX_MAX_COLS];
    for c in 0..n_cols {
        dirs[c].mode = modes[c];
        dirs[c].flags = flags[c];
    }

    // 1. All 8-byte-stride column arrays (`I64`, `F64`, `MIXED.vals`) — `pos` is 8-byte aligned.
    for d in dirs.iter_mut().take(n_cols) {
        if matches!(d.mode, PAX_COL_I64 | PAX_COL_F64 | PAX_COL_MIXED) {
            d.data_off = u16::try_from(pos).ok()?;
            pos = pos.checked_add(8usize.checked_mul(cap)?)?;
        }
    }

    // 2. All 4-byte-stride column arrays (`I32`, `BYTES`) — `pos` is 8-byte (hence 4-byte) aligned.
    for d in dirs.iter_mut().take(n_cols) {
        if matches!(d.mode, PAX_COL_I32 | PAX_COL_BYTES) {
            d.data_off = u16::try_from(pos).ok()?;
            pos = pos.checked_add(4usize.checked_mul(cap)?)?;
        }
    }

    // 3. All 1-byte tag arrays (`MIXED`) and optional non-null bitmaps (`PAX_COL_HAS_NULLS`).
    let bmap_len = cap.div_ceil(8);
    for d in dirs.iter_mut().take(n_cols) {
        if d.mode == PAX_COL_MIXED {
            d.bitmap_off = u16::try_from(pos).ok()?;
            pos = pos.checked_add(cap)?;
        } else if d.mode != PAX_COL_ALL_NULL && (d.flags & PAX_COL_HAS_NULLS) != 0 {
            d.bitmap_off = u16::try_from(pos).ok()?;
            pos = pos.checked_add(bmap_len)?;
        }
    }

    if pos > 65_535 {
        return None;
    }
    Some(ComputedTableLayout {
        rowid_off,
        fixed_zone_end: pos,
        dirs,
    })
}

#[inline]
fn fixed_bits_per_slot(
    n_cols: usize,
    modes: &[u8; PAX_MAX_COLS],
    flags: &[u8; PAX_MAX_COLS],
) -> usize {
    let mut bits = 64usize; // rowid (8 bytes = 64 bits)
    for c in 0..n_cols {
        bits += match modes[c] {
            PAX_COL_ALL_NULL => 0,
            PAX_COL_I32 | PAX_COL_BYTES => 32,
            PAX_COL_I64 | PAX_COL_F64 => 64,
            PAX_COL_MIXED => 72,
            _ => 64,
        };
        if modes[c] != PAX_COL_ALL_NULL
            && modes[c] != PAX_COL_MIXED
            && (flags[c] & PAX_COL_HAS_NULLS) != 0
        {
            bits += 1;
        }
    }
    bits
}

/// Chooses the target `cap` for `m` live rows (`m >= 1`) whose combined variable-byte footprint is
/// `total_var_bytes`, maximizing slot density without starving `var_heap`.
fn choose_pax_table_cap(
    usable: usize,
    n_cols: usize,
    modes: &[u8; PAX_MAX_COLS],
    flags: &[u8; PAX_MAX_COLS],
    m: usize,
    total_var_bytes: usize,
) -> Option<(usize, ComputedTableLayout)> {
    let rowid_off = align8(16 + 6 * n_cols);
    if usable <= rowid_off + total_var_bytes {
        return None;
    }
    let avail_total = usable - rowid_off;
    let fixed_bits = fixed_bits_per_slot(n_cols, modes, flags);
    let avg_var_bits = if m > 0 {
        (total_var_bytes * 8).div_ceil(m)
    } else {
        0
    };
    let row_bits = (fixed_bits + avg_var_bits).max(64);
    let est_cap = ((avail_total * 8) / row_bits).clamp(m.max(1), 512);

    // Search downward from `est_cap` (or upward by 1 if exact integer division left room) so that
    // `layout.fixed_zone_end + total_var_bytes <= usable` holds with maximum `cap`.
    let mut best_cap = est_cap;
    if let Some(l_plus) = compute_pax_table_layout(n_cols, modes, flags, est_cap + 1)
        && avg_var_bits == 0
        && l_plus.fixed_zone_end <= usable
        && est_cap < 512
    {
        best_cap = est_cap + 1;
    }

    let mut try_cap = best_cap;
    while try_cap >= m.max(1) {
        if let Some(layout) = compute_pax_table_layout(n_cols, modes, flags, try_cap)
            && layout.fixed_zone_end + total_var_bytes <= usable
        {
            return Some((try_cap, layout));
        }
        if try_cap == m.max(1) {
            break;
        }
        try_cap -= 1;
    }
    None
}

#[inline(always)]
fn dir_at_raw(raw: &[u8], col_idx: usize) -> PaxColDir {
    let off = 16 + 6 * col_idx;
    let s = &raw[off..off + 6];
    PaxColDir {
        mode: s[0],
        flags: s[1],
        bitmap_off: u16::from_le_bytes([s[2], s[3]]),
        data_off: u16::from_le_bytes([s[4], s[5]]),
    }
}

/// Borrowed, zero-heap-allocation `O(1)` view of a `0x2d` PAX columnar `TableLeaf` page (`pgno >= 2`).
#[derive(Clone, Copy, Debug)]
pub struct PaxTableLeafView<'a> {
    pgno: Pgno,
    usable: usize,
    raw: &'a [u8],
    n_cols: usize,
    n_rows: usize,
    cap: usize,
    var_heap_top: usize,
    fixed_zone_end: usize,
    dead_var_bytes: usize,
    rowid_off: usize,
}

impl<'a> PaxTableLeafView<'a> {
    #[inline]
    pub fn new(pgno: Pgno, usable_u32: u32, raw: &'a [u8]) -> Result<Self> {
        let usable = usable_u32 as usize;
        if pgno < 2 || usable < PAX_MIN_USABLE_SIZE || raw.len() < usable {
            return Err(SqliteError::corrupt(format!(
                "invalid PAX table leaf geometry on page {pgno}"
            )));
        }
        if raw[0] != PAX_TABLE_LEAF_BYTE || raw[1] != PAX_VERSION {
            return Err(SqliteError::corrupt(format!(
                "invalid PAX table leaf header on page {pgno}"
            )));
        }
        let n_cols = usize::from(raw[2]);
        if n_cols == 0 || n_cols > PAX_MAX_COLS {
            return Err(SqliteError::corrupt(format!(
                "invalid PAX column count {n_cols} on page {pgno}"
            )));
        }
        let n_rows = usize::from(u16::from_be_bytes([raw[3], raw[4]]));
        let var_heap_top = decode_var_heap_top(u16::from_le_bytes([raw[5], raw[6]]), usable);
        let cap = usize::from(u16::from_le_bytes([raw[8], raw[9]]));
        let fixed_zone_end = usize::from(u16::from_le_bytes([raw[10], raw[11]]));
        let dead_var_bytes = usize::from(u16::from_le_bytes([raw[12], raw[13]]));
        let rowid_off = usize::from(u16::from_le_bytes([raw[14], raw[15]]));

        let min_rowid_off = align8(16 + 6 * n_cols);
        let rowid_end = rowid_off
            .checked_add(8usize.saturating_mul(cap))
            .ok_or_else(|| SqliteError::corrupt("PAX rowid array overflow"))?;
        if cap == 0
            || n_rows > cap
            || rowid_off < min_rowid_off
            || rowid_end > fixed_zone_end
            || fixed_zone_end > var_heap_top
            || var_heap_top > usable
        {
            return Err(SqliteError::corrupt(format!(
                "corrupt PAX table leaf bounds on page {pgno}"
            )));
        }

        Ok(Self {
            pgno,
            usable,
            raw,
            n_cols,
            n_rows,
            cap,
            var_heap_top,
            fixed_zone_end,
            dead_var_bytes,
            rowid_off,
        })
    }

    /// Validates all `0..n_cols` directory bounds within `[rowid_end .. fixed_zone_end]`.
    #[inline]
    pub fn validate_dirs(&self) -> Result<()> {
        let rowid_end = self.rowid_off + 8 * self.cap;
        let bmap_len = self.cap.div_ceil(8);
        for c in 0..self.n_cols {
            let d = dir_at_raw(self.raw, c);
            let bo = usize::from(d.bitmap_off);
            let doff = usize::from(d.data_off);
            match d.mode {
                PAX_COL_ALL_NULL => {}
                PAX_COL_I32 | PAX_COL_BYTES => {
                    if doff < rowid_end || doff + 4 * self.cap > self.fixed_zone_end {
                        return Err(SqliteError::corrupt("PAX 4B column out of bounds"));
                    }
                    if (d.flags & PAX_COL_HAS_NULLS) != 0
                        && (bo < rowid_end || bo + bmap_len > self.fixed_zone_end)
                    {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    }
                }
                PAX_COL_I64 | PAX_COL_F64 => {
                    if doff < rowid_end || doff + 8 * self.cap > self.fixed_zone_end {
                        return Err(SqliteError::corrupt("PAX 8B column out of bounds"));
                    }
                    if (d.flags & PAX_COL_HAS_NULLS) != 0
                        && (bo < rowid_end || bo + bmap_len > self.fixed_zone_end)
                    {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    }
                }
                PAX_COL_MIXED => {
                    if doff < rowid_end
                        || doff + 8 * self.cap > self.fixed_zone_end
                        || bo < rowid_end
                        || bo + self.cap > self.fixed_zone_end
                    {
                        return Err(SqliteError::corrupt("PAX MIXED column out of bounds"));
                    }
                }
                mode => {
                    return Err(SqliteError::corrupt(format!(
                        "invalid PAX column mode {mode} on page {}",
                        self.pgno
                    )));
                }
            }
        }
        Ok(())
    }

    #[inline(always)]
    pub const fn pgno(&self) -> Pgno {
        self.pgno
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.n_rows
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.n_rows == 0
    }

    #[inline(always)]
    pub const fn n_cols(&self) -> usize {
        self.n_cols
    }

    #[inline(always)]
    pub const fn cap(&self) -> usize {
        self.cap
    }

    #[inline(always)]
    pub const fn var_heap_top(&self) -> usize {
        self.var_heap_top
    }

    #[inline(always)]
    pub const fn fixed_zone_end(&self) -> usize {
        self.fixed_zone_end
    }

    #[inline(always)]
    pub const fn dead_var_bytes(&self) -> usize {
        self.dead_var_bytes
    }

    /// Computes `(payload_bytes, unused_bytes)` for `dbstat` virtual table reporting.
    pub fn payload_and_unused_bytes(&self) -> (u64, u64) {
        let fixed_row_bytes =
            fixed_bits_per_slot(self.n_cols, &self.modes_array(), &self.flags_array()).div_ceil(8);
        let live_fixed = (self.n_rows * fixed_row_bytes) as u64;
        let live_var = (self.usable - self.var_heap_top).saturating_sub(self.dead_var_bytes) as u64;
        let unused = (self.var_heap_top - self.fixed_zone_end + self.dead_var_bytes) as u64;
        (live_fixed + live_var, unused)
    }

    /// Computes `(payload, unused, mx_payload)` for `dbstat` virtual table reporting.
    pub fn dbstat_metrics(&self) -> Result<(i64, i64, i64)> {
        let (payload, unused) = self.payload_and_unused_bytes();
        if self.n_rows == 0 {
            return Ok((0, unused as i64, 0));
        }
        let fixed_row_bytes =
            fixed_bits_per_slot(self.n_cols, &self.modes_array(), &self.flags_array()).div_ceil(8);
        let mut mx_payload = fixed_row_bytes as i64;
        for r in 0..self.n_rows {
            let mut row_bytes = fixed_row_bytes;
            for c in 0..self.n_cols {
                let d = dir_at_raw(self.raw, c);
                if matches!(d.mode, PAX_COL_BYTES | PAX_COL_MIXED)
                    && let Some((b, _)) = self.col_bytes_at(c, r)?
                {
                    row_bytes += b.len();
                }
            }
            mx_payload = mx_payload.max(row_bytes as i64);
        }
        Ok((payload as i64, unused as i64, mx_payload))
    }

    #[inline(always)]
    fn modes_array(&self) -> [u8; PAX_MAX_COLS] {
        let mut out = [0u8; PAX_MAX_COLS];
        for (i, slot) in out.iter_mut().enumerate().take(self.n_cols) {
            *slot = self.raw[16 + 6 * i];
        }
        out
    }

    #[inline(always)]
    fn flags_array(&self) -> [u8; PAX_MAX_COLS] {
        let mut out = [0u8; PAX_MAX_COLS];
        for (i, slot) in out.iter_mut().enumerate().take(self.n_cols) {
            *slot = self.raw[16 + 6 * i + 1];
        }
        out
    }

    #[inline(always)]
    pub fn rowid_at(&self, row_idx: usize) -> i64 {
        let off = self.rowid_off + 8 * row_idx;
        let s = &self.raw[off..off + 8];
        i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    }

    #[inline(always)]
    pub fn last_rowid(&self) -> Option<i64> {
        if self.n_rows == 0 {
            None
        } else {
            Some(self.rowid_at(self.n_rows - 1))
        }
    }

    #[inline(always)]
    pub fn rowids_le_bytes(&self, start: usize, end: usize) -> &'a [u8] {
        &self.raw[self.rowid_off + 8 * start..self.rowid_off + 8 * end]
    }

    #[inline]
    pub fn binary_search_rowid(&self, target: i64) -> std::result::Result<usize, usize> {
        let mut lo = 0usize;
        let mut hi = self.n_rows;
        let base = &self.raw[self.rowid_off..self.rowid_off + 8 * hi];
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let o = 8 * mid;
            let rid = i64::from_le_bytes([
                base[o],
                base[o + 1],
                base[o + 2],
                base[o + 3],
                base[o + 4],
                base[o + 5],
                base[o + 6],
                base[o + 7],
            ]);
            match rid.cmp(&target) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Ok(mid),
            }
        }
        Err(lo)
    }

    #[inline]
    pub fn lower_bound_rowid(&self, target: i64) -> usize {
        match self.binary_search_rowid(target) {
            Ok(i) | Err(i) => i,
        }
    }

    #[inline]
    pub fn upper_bound_rowid(&self, target: i64) -> usize {
        let mut lo = 0usize;
        let mut hi = self.n_rows;
        let base = &self.raw[self.rowid_off..self.rowid_off + 8 * hi];
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let o = 8 * mid;
            let rid = i64::from_le_bytes([
                base[o],
                base[o + 1],
                base[o + 2],
                base[o + 3],
                base[o + 4],
                base[o + 5],
                base[o + 6],
                base[o + 7],
            ]);
            if rid <= target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    #[inline(always)]
    pub fn col_dir(&self, col_idx: usize) -> Option<PaxColDir> {
        if col_idx < self.n_cols {
            Some(dir_at_raw(self.raw, col_idx))
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn col_is_null(&self, col_idx: usize, row_idx: usize) -> bool {
        if col_idx >= self.n_cols {
            return true;
        }
        let d = dir_at_raw(self.raw, col_idx);
        let bo = usize::from(d.bitmap_off);
        match d.mode {
            PAX_COL_ALL_NULL => true,
            PAX_COL_MIXED => self.raw.get(bo + row_idx).copied() == Some(PAX_MIXED_NULL),
            _ => {
                (d.flags & PAX_COL_HAS_NULLS) != 0
                    && self
                        .raw
                        .get(bo + (row_idx >> 3))
                        .is_none_or(|&b| (b & (1u8 << (row_idx & 7))) == 0)
            }
        }
    }

    /// Returns contiguous little-endian `i32` bytes for `[start..end]` and optional `(bitmap, start)`
    /// when `col_idx` is encoded in `PAX_COL_I32`.
    #[inline(always)]
    #[allow(clippy::type_complexity)]
    pub fn col_i32_le_bytes(
        &self,
        col_idx: usize,
        start: usize,
        end: usize,
    ) -> Option<(&'a [u8], Option<(&'a [u8], usize)>)> {
        if col_idx >= self.n_cols || start > end {
            return None;
        }
        let d = dir_at_raw(self.raw, col_idx);
        if d.mode != PAX_COL_I32 {
            return None;
        }
        let doff = usize::from(d.data_off);
        let data = self.raw.get(doff + 4 * start..doff + 4 * end)?;
        let bmap = if (d.flags & PAX_COL_HAS_NULLS) != 0 {
            let bo = usize::from(d.bitmap_off);
            Some((self.raw.get(bo..bo + self.cap.div_ceil(8))?, start))
        } else {
            None
        };
        Some((data, bmap))
    }

    /// Returns contiguous little-endian `i64` bytes for `[start..end]` and optional `(bitmap, start)`
    /// when `col_idx` is encoded in `PAX_COL_I64`.
    #[inline(always)]
    #[allow(clippy::type_complexity)]
    pub fn col_i64_le_bytes(
        &self,
        col_idx: usize,
        start: usize,
        end: usize,
    ) -> Option<(&'a [u8], Option<(&'a [u8], usize)>)> {
        if col_idx >= self.n_cols || start > end {
            return None;
        }
        let d = dir_at_raw(self.raw, col_idx);
        if d.mode != PAX_COL_I64 {
            return None;
        }
        let doff = usize::from(d.data_off);
        let data = self.raw.get(doff + 8 * start..doff + 8 * end)?;
        let bmap = if (d.flags & PAX_COL_HAS_NULLS) != 0 {
            let bo = usize::from(d.bitmap_off);
            Some((self.raw.get(bo..bo + self.cap.div_ceil(8))?, start))
        } else {
            None
        };
        Some((data, bmap))
    }

    /// Returns contiguous little-endian IEEE-754 `f64` bytes for `[start..end]` and optional `(bitmap, start)`
    /// when `col_idx` is encoded in `PAX_COL_F64`.
    #[inline(always)]
    #[allow(clippy::type_complexity)]
    pub fn col_f64_le_bytes(
        &self,
        col_idx: usize,
        start: usize,
        end: usize,
    ) -> Option<(&'a [u8], Option<(&'a [u8], usize)>)> {
        if col_idx >= self.n_cols || start > end {
            return None;
        }
        let d = dir_at_raw(self.raw, col_idx);
        if d.mode != PAX_COL_F64 {
            return None;
        }
        let doff = usize::from(d.data_off);
        let data = self.raw.get(doff + 8 * start..doff + 8 * end)?;
        let bmap = if (d.flags & PAX_COL_HAS_NULLS) != 0 {
            let bo = usize::from(d.bitmap_off);
            Some((self.raw.get(bo..bo + self.cap.div_ceil(8))?, start))
        } else {
            None
        };
        Some((data, bmap))
    }

    /// Fast zero-copy byte-slice reader for `PAX_COL_BYTES` (`TEXT` / `BLOB`):
    /// returns `Ok(Some((bytes, is_blob)))` when non-null, or `Ok(None)` when `NULL`.
    #[inline(always)]
    pub fn col_bytes_at(&self, col_idx: usize, row_idx: usize) -> Result<Option<(&'a [u8], bool)>> {
        if col_idx >= self.n_cols {
            return Ok(None);
        }
        let d = dir_at_raw(self.raw, col_idx);
        if d.mode != PAX_COL_BYTES {
            return match self.read_raw_field(col_idx, row_idx)? {
                Some(RawField::Text(b)) => Ok(Some((b, false))),
                Some(RawField::Blob(b)) => Ok(Some((b, true))),
                _ => Ok(None),
            };
        }
        if (d.flags & PAX_COL_HAS_NULLS) != 0 {
            let bo = usize::from(d.bitmap_off);
            let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
            };
            if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                return Ok(None);
            }
        }
        let doff = usize::from(d.data_off) + 4 * row_idx;
        let Some(s) = self.raw.get(doff..doff + 4) else {
            return Err(SqliteError::corrupt("PAX 4B column out of bounds"));
        };
        let desc = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
        let off = (desc & 0xffff) as usize;
        let len_tag = (desc >> 16) as usize;
        let is_blob = (len_tag & 0x8000) != 0;
        let len = len_tag & 0x7fff;
        if len == 0 {
            return Ok(Some((&[], is_blob)));
        }
        if off < self.var_heap_top || off + len > self.usable {
            return Err(SqliteError::corrupt("PAX BYTES slice out of heap bounds"));
        }
        Ok(Some((&self.raw[off..off + len], is_blob)))
    }

    /// Reads column `col_idx` at `row_idx` as a zero-copy [`RawField`].
    /// Returns `Ok(None)` if `col_idx >= n_cols` (omitted trailing column after `ALTER TABLE ADD COLUMN`).
    #[inline]
    pub fn read_raw_field(&self, col_idx: usize, row_idx: usize) -> Result<Option<RawField<'a>>> {
        if col_idx >= self.n_cols {
            return Ok(None);
        }
        let d = dir_at_raw(self.raw, col_idx);
        match d.mode {
            PAX_COL_ALL_NULL => Ok(Some(RawField::Null)),
            PAX_COL_I32 => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return Ok(Some(RawField::Null));
                    }
                }
                let o = usize::from(d.data_off) + 4 * row_idx;
                let Some(s) = self.raw.get(o..o + 4) else {
                    return Err(SqliteError::corrupt("PAX 4B column out of bounds"));
                };
                let v = i32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                Ok(Some(RawField::Int(i64::from(v))))
            }
            PAX_COL_I64 => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return Ok(Some(RawField::Null));
                    }
                }
                let o = usize::from(d.data_off) + 8 * row_idx;
                let Some(s) = self.raw.get(o..o + 8) else {
                    return Err(SqliteError::corrupt("PAX 8B column out of bounds"));
                };
                let v = i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                Ok(Some(RawField::Int(v)))
            }
            PAX_COL_F64 => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return Ok(Some(RawField::Null));
                    }
                }
                let o = usize::from(d.data_off) + 8 * row_idx;
                let Some(s) = self.raw.get(o..o + 8) else {
                    return Err(SqliteError::corrupt("PAX 8B column out of bounds"));
                };
                let bits = u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                Ok(Some(RawField::Float(f64::from_bits(bits))))
            }
            PAX_COL_BYTES => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return Ok(Some(RawField::Null));
                    }
                }
                let o = usize::from(d.data_off) + 4 * row_idx;
                let Some(s) = self.raw.get(o..o + 4) else {
                    return Err(SqliteError::corrupt("PAX 4B column out of bounds"));
                };
                let desc = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                let off = (desc & 0xffff) as usize;
                let len_tag = (desc >> 16) as usize;
                let is_blob = (len_tag & 0x8000) != 0;
                let len = len_tag & 0x7fff;
                let slice: &'a [u8] = if len == 0 {
                    &[]
                } else {
                    if off < self.var_heap_top || off + len > self.usable {
                        return Err(SqliteError::corrupt("PAX BYTES slice out of heap bounds"));
                    }
                    &self.raw[off..off + len]
                };
                Ok(Some(if is_blob {
                    RawField::Blob(slice)
                } else {
                    RawField::Text(slice)
                }))
            }
            PAX_COL_MIXED => {
                let bo = usize::from(d.bitmap_off);
                let Some(&tag) = self.raw.get(bo + row_idx) else {
                    return Err(SqliteError::corrupt("PAX MIXED tag out of bounds"));
                };
                let o = usize::from(d.data_off) + 8 * row_idx;
                let Some(s) = self.raw.get(o..o + 8) else {
                    return Err(SqliteError::corrupt("PAX MIXED word out of bounds"));
                };
                let word = u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                match tag {
                    PAX_MIXED_NULL => Ok(Some(RawField::Null)),
                    PAX_MIXED_INT => Ok(Some(RawField::Int(word as i64))),
                    PAX_MIXED_REAL => Ok(Some(RawField::Float(f64::from_bits(word)))),
                    PAX_MIXED_TEXT | PAX_MIXED_BLOB => {
                        let off = (word & 0xffff_ffff) as usize;
                        let len = (word >> 32) as usize;
                        let slice: &'a [u8] = if len == 0 {
                            &[]
                        } else {
                            if off < self.var_heap_top || off + len > self.usable {
                                return Err(SqliteError::corrupt(
                                    "PAX MIXED slice out of heap bounds",
                                ));
                            }
                            &self.raw[off..off + len]
                        };
                        Ok(Some(if tag == PAX_MIXED_BLOB {
                            RawField::Blob(slice)
                        } else {
                            RawField::Text(slice)
                        }))
                    }
                    other => Err(SqliteError::corrupt(format!(
                        "invalid PAX MIXED tag {other} on page {}",
                        self.pgno
                    ))),
                }
            }
            _ => Err(SqliteError::corrupt("invalid PAX column mode")),
        }
    }

    #[inline]
    pub fn read_value(&self, col_idx: usize, row_idx: usize) -> Result<Option<Value>> {
        let Some(rf) = self.read_raw_field(col_idx, row_idx)? else {
            return Ok(None);
        };
        Ok(Some(rf.to_owned_value(TextEncoding::Utf8)))
    }

    #[inline]
    pub fn read_value_pooled(
        &self,
        col_idx: usize,
        row_idx: usize,
        texts: &mut Vec<String>,
        blobs: &mut Vec<Vec<u8>>,
    ) -> Result<Option<Value>> {
        let Some(rf) = self.read_raw_field(col_idx, row_idx)? else {
            return Ok(None);
        };
        let v = match rf {
            RawField::Null => Value::Null,
            RawField::Int(i) => Value::Integer(i),
            RawField::Float(r) => Value::Real(r),
            RawField::Text(bytes) => {
                let mut s = texts.pop().unwrap_or_default();
                s.clear();
                match std::str::from_utf8(bytes) {
                    Ok(valid) => s.push_str(valid),
                    Err(_) => s.push_str(&String::from_utf8_lossy(bytes)),
                }
                Value::Text(s)
            }
            RawField::Blob(bytes) => {
                let mut b = blobs.pop().unwrap_or_default();
                b.clear();
                b.extend_from_slice(bytes);
                Value::Blob(b)
            }
        };
        Ok(Some(v))
    }

    /// Projects `cols` at `row_idx` directly into a [`PointLookupRow`] (`InlineRow` SSO fast path
    /// when `cols.len() <= INLINE_ROW_COLS` and all values fit inline, else `Heap(Vec<Value>)`).
    pub fn project_point_lookup_row(
        &self,
        row_idx: usize,
        rowid: i64,
        cols: &[PointProjCol],
    ) -> Result<PointLookupRow> {
        if cols.len() <= INLINE_ROW_COLS {
            let mut inline_cols = [InlineVal::Null; INLINE_ROW_COLS];
            let mut all_inline = true;
            for (i, c) in cols.iter().enumerate() {
                match c {
                    PointProjCol::Rowid => inline_cols[i] = InlineVal::Integer(rowid),
                    PointProjCol::Column {
                        col_idx,
                        default_val,
                    } => match self.read_raw_field(*col_idx as usize, row_idx)? {
                        None => match default_val {
                            None => inline_cols[i] = InlineVal::Null,
                            Some(dv) => match InlineVal::try_from_value(dv) {
                                Some(iv) => inline_cols[i] = iv,
                                None => {
                                    all_inline = false;
                                    break;
                                }
                            },
                        },
                        Some(rf) => match InlineVal::try_from_raw_field(rf, TextEncoding::Utf8) {
                            Some(iv) => inline_cols[i] = iv,
                            None => {
                                all_inline = false;
                                break;
                            }
                        },
                    },
                }
            }
            if all_inline {
                return Ok(PointLookupRow::Inline(InlineRow::from_raw_parts(
                    cols.len() as u8,
                    inline_cols,
                )));
            }
        }

        let mut out = Vec::with_capacity(cols.len());
        for c in cols {
            match c {
                PointProjCol::Rowid => out.push(Value::Integer(rowid)),
                PointProjCol::Column {
                    col_idx,
                    default_val,
                } => match self.read_value(*col_idx as usize, row_idx)? {
                    Some(v) => out.push(v),
                    None => out.push(default_val.clone().unwrap_or(Value::Null)),
                },
            }
        }
        Ok(PointLookupRow::Heap(out))
    }

    /// Reconstructs the canonical Format-3 record bytes for `row_idx` into `out` without allocating
    /// intermediate `Value`s!
    pub fn encode_row_record_into(&self, row_idx: usize, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        let mut sts = [0u64; PAX_MAX_COLS];
        let mut rfs = [RawField::Null; PAX_MAX_COLS];
        let mut hdr_body_bytes = 0usize;
        let mut data_bytes = 0usize;

        for c in 0..self.n_cols {
            let rf = self.read_raw_field(c, row_idx)?.unwrap_or(RawField::Null);
            let (st, blen) = match rf {
                RawField::Null => (0u64, 0usize),
                RawField::Int(i) => serial_type_for(&Value::Integer(i)),
                RawField::Float(r) => {
                    if r.is_nan() {
                        (0u64, 0usize)
                    } else {
                        (7u64, 8usize)
                    }
                }
                RawField::Text(b) => (13 + 2 * (b.len() as u64), b.len()),
                RawField::Blob(b) => (12 + 2 * (b.len() as u64), b.len()),
            };
            sts[c] = st;
            rfs[c] = rf;
            hdr_body_bytes += varint_len(st);
            data_bytes += blen;
        }

        let mut hdr_len_v = varint_len((hdr_body_bytes + 1) as u64);
        let total_hdr = hdr_body_bytes + hdr_len_v;
        if varint_len(total_hdr as u64) != hdr_len_v {
            hdr_len_v = varint_len((hdr_body_bytes + hdr_len_v + 1) as u64);
        }
        let final_hdr_size = hdr_body_bytes + hdr_len_v;
        let total_len = final_hdr_size + data_bytes;
        out.resize(total_len, 0);

        let mut pos = put_varint(out, final_hdr_size as u64);
        for &st in &sts[..self.n_cols] {
            pos += put_varint(&mut out[pos..], st);
        }
        debug_assert_eq!(pos, final_hdr_size);

        for c in 0..self.n_cols {
            let st = sts[c];
            match rfs[c] {
                RawField::Null => {}
                RawField::Int(i) => match st {
                    8 | 9 => {}
                    1 => {
                        out[pos] = i as u8;
                        pos += 1;
                    }
                    2 => {
                        out[pos..pos + 2].copy_from_slice(&(i as i16).to_be_bytes());
                        pos += 2;
                    }
                    3 => {
                        let b = (i as i32).to_be_bytes();
                        out[pos..pos + 3].copy_from_slice(&b[1..4]);
                        pos += 3;
                    }
                    4 => {
                        out[pos..pos + 4].copy_from_slice(&(i as i32).to_be_bytes());
                        pos += 4;
                    }
                    5 => {
                        let b = i.to_be_bytes();
                        out[pos..pos + 6].copy_from_slice(&b[2..8]);
                        pos += 6;
                    }
                    6 => {
                        out[pos..pos + 8].copy_from_slice(&i.to_be_bytes());
                        pos += 8;
                    }
                    _ => unreachable!(),
                },
                RawField::Float(r) => {
                    if st == 7 {
                        out[pos..pos + 8].copy_from_slice(&r.to_bits().to_be_bytes());
                        pos += 8;
                    }
                }
                RawField::Text(b) | RawField::Blob(b) => {
                    out[pos..pos + b.len()].copy_from_slice(b);
                    pos += b.len();
                }
            }
        }
        debug_assert_eq!(pos, total_len);
        Ok(())
    }

    /// Inspects column `col_idx` at `row_idx` for `incrblob` (`Blob::open` / `read_at` / `write_at`),
    /// returning `(page_offset, byte_len)` inside `raw` if the column is `TEXT` or `BLOB`, or the
    /// exact SQLite error `"cannot open value of type ..."` if `NULL`/`INTEGER`/`REAL`.
    pub fn inspect_col_bytes_span(&self, row_idx: usize, col_idx: usize) -> Result<(usize, usize)> {
        if col_idx >= self.n_cols {
            return Err(SqliteError::new(
                sqliters_types::ErrorCode::Error,
                "cannot open value of type null",
            ));
        }
        let d = dir_at_raw(self.raw, col_idx);
        let fail_ty = |ty: &str| {
            Err(SqliteError::new(
                sqliters_types::ErrorCode::Error,
                format!("cannot open value of type {ty}"),
            ))
        };
        match d.mode {
            PAX_COL_ALL_NULL => fail_ty("null"),
            PAX_COL_I32 | PAX_COL_I64 => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return fail_ty("null");
                    }
                }
                fail_ty("integer")
            }
            PAX_COL_F64 => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return fail_ty("null");
                    }
                }
                fail_ty("real")
            }
            PAX_COL_BYTES => {
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    let bo = usize::from(d.bitmap_off);
                    let Some(&bbyte) = self.raw.get(bo + (row_idx >> 3)) else {
                        return Err(SqliteError::corrupt("PAX column bitmap out of bounds"));
                    };
                    if (bbyte & (1u8 << (row_idx & 7))) == 0 {
                        return fail_ty("null");
                    }
                }
                let o = usize::from(d.data_off) + 4 * row_idx;
                let Some(s) = self.raw.get(o..o + 4) else {
                    return Err(SqliteError::corrupt("PAX 4B column out of bounds"));
                };
                let desc = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
                let off = (desc & 0xffff) as usize;
                let len = ((desc >> 16) as usize) & 0x7fff;
                if len == 0 {
                    return Ok((0, 0));
                }
                if off < self.var_heap_top || off + len > self.usable {
                    return Err(SqliteError::corrupt("PAX BYTES span out of heap bounds"));
                }
                Ok((off, len))
            }
            PAX_COL_MIXED => {
                let bo = usize::from(d.bitmap_off);
                let Some(&tag) = self.raw.get(bo + row_idx) else {
                    return Err(SqliteError::corrupt("PAX MIXED tag out of bounds"));
                };
                let o = usize::from(d.data_off) + 8 * row_idx;
                let Some(s) = self.raw.get(o..o + 8) else {
                    return Err(SqliteError::corrupt("PAX MIXED word out of bounds"));
                };
                let word = u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
                match tag {
                    PAX_MIXED_NULL => fail_ty("null"),
                    PAX_MIXED_INT => fail_ty("integer"),
                    PAX_MIXED_REAL => fail_ty("real"),
                    PAX_MIXED_TEXT | PAX_MIXED_BLOB => {
                        let off = (word & 0xffff_ffff) as usize;
                        let len = (word >> 32) as usize;
                        if len == 0 {
                            return Ok((0, 0));
                        }
                        if off < self.var_heap_top || off + len > self.usable {
                            return Err(SqliteError::corrupt("PAX MIXED span out of heap bounds"));
                        }
                        Ok((off, len))
                    }
                    _ => Err(SqliteError::corrupt("invalid PAX MIXED tag")),
                }
            }
            _ => Err(SqliteError::corrupt("invalid PAX column mode")),
        }
    }

    #[inline]
    pub fn col_blob_eq(&self, row_idx: usize, col_idx: usize, expected: &[u8]) -> Result<bool> {
        match self.read_raw_field(col_idx, row_idx)? {
            Some(RawField::Blob(b)) => Ok(b == expected),
            _ => Ok(false),
        }
    }
}

/// Validates every structural, ordering, directory-disjointness, and `var_heap` invariant of a
/// `0x2d` PAX table leaf page for `Btree::integrity_check`.
pub fn validate_pax_table_leaf(pgno: Pgno, usable: u32, raw: &[u8], is_root: bool) -> Result<()> {
    let view = PaxTableLeafView::new(pgno, usable, raw)?;
    view.validate_dirs()?;
    if !is_root && view.n_rows == 0 {
        return Err(SqliteError::corrupt(format!(
            "non-root PAX table leaf {pgno} has 0 rows"
        )));
    }
    if raw[7] != 0 {
        return Err(SqliteError::corrupt(format!(
            "non-zero reserved header flags on PAX table leaf {pgno}"
        )));
    }

    // 1. Verify column regions in `[rowid_off .. fixed_zone_end]` are pairwise disjoint.
    let mut intervals: Vec<(usize, usize)> = Vec::with_capacity(2 * view.n_cols + 1);
    intervals.push((view.rowid_off, view.rowid_off + 8 * view.cap));
    let bmap_len = view.cap.div_ceil(8);
    for c in 0..view.n_cols {
        let d = dir_at_raw(raw, c);
        let bo = usize::from(d.bitmap_off);
        let doff = usize::from(d.data_off);
        match d.mode {
            PAX_COL_ALL_NULL => {
                if d.flags != 0 || bo != 0 || doff != 0 {
                    return Err(SqliteError::corrupt("non-zero ALL_NULL directory fields"));
                }
            }
            PAX_COL_I32 | PAX_COL_BYTES => {
                if (d.flags & !PAX_COL_HAS_NULLS) != 0 {
                    return Err(SqliteError::corrupt("unknown PAX column flags"));
                }
                intervals.push((doff, doff + 4 * view.cap));
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    intervals.push((bo, bo + bmap_len));
                } else if bo != 0 {
                    return Err(SqliteError::corrupt(
                        "unexpected bitmap_off without HAS_NULLS",
                    ));
                }
            }
            PAX_COL_I64 | PAX_COL_F64 => {
                if (d.flags & !PAX_COL_HAS_NULLS) != 0 {
                    return Err(SqliteError::corrupt("unknown PAX column flags"));
                }
                intervals.push((doff, doff + 8 * view.cap));
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    intervals.push((bo, bo + bmap_len));
                } else if bo != 0 {
                    return Err(SqliteError::corrupt(
                        "unexpected bitmap_off without HAS_NULLS",
                    ));
                }
            }
            PAX_COL_MIXED => {
                intervals.push((doff, doff + 8 * view.cap));
                intervals.push((bo, bo + view.cap));
            }
            _ => return Err(SqliteError::corrupt("invalid PAX column mode")),
        }
    }
    intervals.sort_unstable_by_key(|&(s, _)| s);
    for pair in intervals.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(SqliteError::corrupt(format!(
                "overlapping PAX column regions on page {pgno}"
            )));
        }
    }

    // 2. Verify strictly increasing `rowids` and valid cell slices in `var_heap`.
    let mut prev_rid: Option<i64> = None;
    let mut live_var_bytes = 0usize;
    for r in 0..view.n_rows {
        let rid = view.rowid_at(r);
        if let Some(prev) = prev_rid
            && rid <= prev
        {
            return Err(SqliteError::corrupt(format!(
                "non-monotonic rowids ({prev} >= {rid}) on PAX table leaf {pgno}"
            )));
        }
        prev_rid = Some(rid);

        for c in 0..view.n_cols {
            let Some(rf) = view.read_raw_field(c, r)? else {
                continue;
            };
            match rf {
                RawField::Text(b) | RawField::Blob(b) => {
                    live_var_bytes += b.len();
                }
                _ => {}
            }
        }
    }
    let heap_span = view.usable - view.var_heap_top;
    if live_var_bytes + view.dead_var_bytes > heap_span {
        return Err(SqliteError::corrupt(format!(
            "PAX var_heap accounting mismatch on page {pgno}"
        )));
    }
    Ok(())
}

/// Converts a `0x2d` PAX table leaf into `Vec<TableLeafCell>` (used by `ParsedPage::parse` and
/// seamless `0x2d -> 0x0d` leaf escape when an oversized/overflow row lands on a PAX leaf).
pub fn pax_table_leaf_to_cells(pgno: Pgno, usable: u32, raw: &[u8]) -> Result<Vec<TableLeafCell>> {
    let view = PaxTableLeafView::new(pgno, usable, raw)?;
    let mut cells = Vec::with_capacity(view.n_rows);
    let mut buf = Vec::with_capacity(128);
    for r in 0..view.n_rows {
        view.encode_row_record_into(r, &mut buf)?;
        cells.push(TableLeafCell {
            rowid: view.rowid_at(r),
            total_payload_len: buf.len() as u32,
            local_payload: buf.clone(),
            overflow_page: None,
        });
    }
    Ok(cells)
}

/// Input row representation for `0x2d` PAX table leaf insertion/splitting.
#[derive(Clone, Copy)]
pub enum PaxValueInput<'a> {
    /// Canonical Format-3 serialized record bytes (`UTF-8` encoding).
    Payload(&'a [u8]),
    /// Direct typed `Value` references (bypasses Format-3 record serialization completely!).
    Refs(&'a [&'a Value]),
}

impl<'a> PaxValueInput<'a> {
    pub fn encode_fallback_into(self, out: &mut Vec<u8>) {
        out.clear();
        match self {
            Self::Payload(p) => out.extend_from_slice(p),
            Self::Refs(refs) => {
                encode_value_refs_record_into(refs, TextEncoding::Utf8, out);
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ParsedPaxField<'a> {
    Null,
    Int(i64),
    Real(f64),
    Text(&'a [u8]),
    Blob(&'a [u8]),
}

impl<'a> ParsedPaxField<'a> {
    #[inline(always)]
    fn required_mode(self) -> u8 {
        match self {
            Self::Null => PAX_COL_ALL_NULL,
            Self::Int(v) => {
                if i32::try_from(v).is_ok() {
                    PAX_COL_I32
                } else {
                    PAX_COL_I64
                }
            }
            Self::Real(_) => PAX_COL_F64,
            Self::Text(_) | Self::Blob(_) => PAX_COL_BYTES,
        }
    }

    #[inline(always)]
    fn var_len(self) -> usize {
        match self {
            Self::Text(b) | Self::Blob(b) => b.len(),
            _ => 0,
        }
    }
}

#[inline]
fn merge_col_mode_and_flags(cur_mode: u8, cur_flags: u8, field: ParsedPaxField<'_>) -> (u8, u8) {
    match field {
        ParsedPaxField::Null => {
            if cur_mode == PAX_COL_ALL_NULL || cur_mode == PAX_COL_MIXED {
                (cur_mode, cur_flags)
            } else {
                (cur_mode, cur_flags | PAX_COL_HAS_NULLS)
            }
        }
        other => {
            let req = other.required_mode();
            if cur_mode == PAX_COL_ALL_NULL {
                // Note: if `cur_flags` has bit 7 sentinel (meaning earlier rows existed as NULL),
                // caller sets `HAS_NULLS` explicitly.
                (req, cur_flags)
            } else if cur_mode == req {
                (cur_mode, cur_flags)
            } else if (cur_mode == PAX_COL_I32 && req == PAX_COL_I64)
                || (cur_mode == PAX_COL_I64 && req == PAX_COL_I32)
            {
                (PAX_COL_I64, cur_flags)
            } else {
                (PAX_COL_MIXED, 0)
            }
        }
    }
}

/// Parses `PaxValueInput` into `out_fields` + `(n_cols, total_var_bytes)`.
/// Returns `None` if the row is ineligible for `0x2d` (`n_cols == 0`, `n_cols > out_fields.len()`,
/// `n_cols > PAX_MAX_COLS`, invalid Format-3 record, or `total_var_bytes > (usable / 2).min(32767)`).
fn parse_pax_input<'a>(
    usable: usize,
    input: PaxValueInput<'a>,
    out_fields: &mut [ParsedPaxField<'a>],
) -> Option<(usize, usize)> {
    let max_cols = out_fields.len().min(PAX_MAX_COLS);
    let max_var = (usable / 2).min(32_767);
    match input {
        PaxValueInput::Refs(refs) => {
            let n = refs.len();
            if n == 0 || n > max_cols {
                return None;
            }
            let mut total_var = 0usize;
            for (i, &v) in refs.iter().enumerate() {
                let pf = match v {
                    Value::Null => ParsedPaxField::Null,
                    Value::Integer(x) => ParsedPaxField::Int(*x),
                    Value::Real(r) => {
                        if r.is_nan() {
                            ParsedPaxField::Null
                        } else {
                            ParsedPaxField::Real(*r)
                        }
                    }
                    Value::Text(s) => {
                        let b = s.as_bytes();
                        if b.len() > 32_767 {
                            return None;
                        }
                        total_var += b.len();
                        ParsedPaxField::Text(b)
                    }
                    Value::Blob(b) => {
                        if b.len() > 32_767 {
                            return None;
                        }
                        total_var += b.len();
                        ParsedPaxField::Blob(b)
                    }
                };
                out_fields[i] = pf;
            }
            if total_var > max_var {
                return None;
            }
            Some((n, total_var))
        }
        PaxValueInput::Payload(rec) => {
            if rec.len() < 2 {
                return None;
            }
            let (hdr_u64, mut hdr_pos) = get_varint(rec).ok()?;
            let hdr_size = usize::try_from(hdr_u64).ok()?;
            if hdr_size < hdr_pos || hdr_size > rec.len() || hdr_pos == hdr_size {
                return None;
            }
            let mut body_pos = hdr_size;
            let mut col = 0usize;
            let mut total_var = 0usize;
            while hdr_pos < hdr_size {
                if col >= max_cols {
                    return None;
                }
                let (st, used) = get_varint(&rec[hdr_pos..hdr_size]).ok()?;
                hdr_pos += used;
                let len = serial_type_len(st).ok()?;
                let end = body_pos.checked_add(len)?;
                if end > rec.len() {
                    return None;
                }
                let slice = &rec[body_pos..end];
                let pf = match st {
                    0 => ParsedPaxField::Null,
                    8 => ParsedPaxField::Int(0),
                    9 => ParsedPaxField::Int(1),
                    1 => ParsedPaxField::Int(i64::from(slice[0] as i8)),
                    2 => ParsedPaxField::Int(i64::from(i16::from_be_bytes([slice[0], slice[1]]))),
                    3 => {
                        let sign = if (slice[0] & 0x80) != 0 { 0xff } else { 0x00 };
                        ParsedPaxField::Int(i64::from(i32::from_be_bytes([
                            sign, slice[0], slice[1], slice[2],
                        ])))
                    }
                    4 => ParsedPaxField::Int(i64::from(i32::from_be_bytes([
                        slice[0], slice[1], slice[2], slice[3],
                    ]))),
                    5 => {
                        let sign = if (slice[0] & 0x80) != 0 { 0xff } else { 0x00 };
                        ParsedPaxField::Int(i64::from_be_bytes([
                            sign, sign, slice[0], slice[1], slice[2], slice[3], slice[4], slice[5],
                        ]))
                    }
                    6 => ParsedPaxField::Int(i64::from_be_bytes([
                        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6],
                        slice[7],
                    ])),
                    7 => {
                        let bits = u64::from_be_bytes([
                            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6],
                            slice[7],
                        ]);
                        let f = f64::from_bits(bits);
                        if f.is_nan() {
                            ParsedPaxField::Null
                        } else {
                            ParsedPaxField::Real(f)
                        }
                    }
                    s if s >= 12 && (s & 1) == 0 => {
                        if len > 32_767 {
                            return None;
                        }
                        total_var += len;
                        ParsedPaxField::Blob(slice)
                    }
                    s if s >= 13 && (s & 1) == 1 => {
                        if len > 32_767 || std::str::from_utf8(slice).is_err() {
                            return None;
                        }
                        total_var += len;
                        ParsedPaxField::Text(slice)
                    }
                    _ => return None,
                };
                out_fields[col] = pf;
                body_pos = end;
                col += 1;
            }
            if col == 0 || body_pos != rec.len() || total_var > max_var {
                return None;
            }
            Some((col, total_var))
        }
    }
}

#[inline]
fn write_pax_table_header(
    raw: &mut [u8],
    n_cols: usize,
    n_rows: usize,
    var_heap_top: usize,
    cap: usize,
    layout: &ComputedTableLayout,
    dead_var_bytes: usize,
) {
    raw[0] = PAX_TABLE_LEAF_BYTE;
    raw[1] = PAX_VERSION;
    raw[2] = n_cols as u8;
    raw[3..5].copy_from_slice(&(n_rows as u16).to_be_bytes());
    raw[5..7].copy_from_slice(&encode_var_heap_top(var_heap_top).to_le_bytes());
    raw[7] = 0;
    raw[8..10].copy_from_slice(&(cap as u16).to_le_bytes());
    raw[10..12].copy_from_slice(&(layout.fixed_zone_end as u16).to_le_bytes());
    raw[12..14].copy_from_slice(&(dead_var_bytes.min(65_535) as u16).to_le_bytes());
    raw[14..16].copy_from_slice(&(layout.rowid_off as u16).to_le_bytes());

    for c in 0..n_cols {
        let d = layout.dirs[c];
        let o = 16 + 6 * c;
        raw[o] = d.mode;
        raw[o + 1] = d.flags;
        raw[o + 2..o + 4].copy_from_slice(&d.bitmap_off.to_le_bytes());
        raw[o + 4..o + 6].copy_from_slice(&d.data_off.to_le_bytes());
    }
}

#[inline]
fn write_field_into_slot(
    raw: &mut [u8],
    dir: PaxColDir,
    row_idx: usize,
    field: ParsedPaxField<'_>,
    var_heap_top: &mut usize,
) {
    let bo = usize::from(dir.bitmap_off);
    let doff = usize::from(dir.data_off);
    match dir.mode {
        PAX_COL_ALL_NULL => {}
        PAX_COL_I32 => match field {
            ParsedPaxField::Null => {
                bitmap_set(raw, bo, row_idx, false);
                raw[doff + 4 * row_idx..doff + 4 * row_idx + 4].fill(0);
            }
            ParsedPaxField::Int(v) => {
                if (dir.flags & PAX_COL_HAS_NULLS) != 0 {
                    bitmap_set(raw, bo, row_idx, true);
                }
                raw[doff + 4 * row_idx..doff + 4 * row_idx + 4]
                    .copy_from_slice(&(v as i32).to_le_bytes());
            }
            _ => unreachable!(),
        },
        PAX_COL_I64 => match field {
            ParsedPaxField::Null => {
                bitmap_set(raw, bo, row_idx, false);
                raw[doff + 8 * row_idx..doff + 8 * row_idx + 8].fill(0);
            }
            ParsedPaxField::Int(v) => {
                if (dir.flags & PAX_COL_HAS_NULLS) != 0 {
                    bitmap_set(raw, bo, row_idx, true);
                }
                raw[doff + 8 * row_idx..doff + 8 * row_idx + 8].copy_from_slice(&v.to_le_bytes());
            }
            _ => unreachable!(),
        },
        PAX_COL_F64 => match field {
            ParsedPaxField::Null => {
                bitmap_set(raw, bo, row_idx, false);
                raw[doff + 8 * row_idx..doff + 8 * row_idx + 8].fill(0);
            }
            ParsedPaxField::Real(r) => {
                if (dir.flags & PAX_COL_HAS_NULLS) != 0 {
                    bitmap_set(raw, bo, row_idx, true);
                }
                raw[doff + 8 * row_idx..doff + 8 * row_idx + 8]
                    .copy_from_slice(&r.to_bits().to_le_bytes());
            }
            _ => unreachable!(),
        },
        PAX_COL_BYTES => match field {
            ParsedPaxField::Null => {
                bitmap_set(raw, bo, row_idx, false);
                raw[doff + 4 * row_idx..doff + 4 * row_idx + 4].fill(0);
            }
            ParsedPaxField::Text(b) | ParsedPaxField::Blob(b) => {
                let is_blob = matches!(field, ParsedPaxField::Blob(_));
                if (dir.flags & PAX_COL_HAS_NULLS) != 0 {
                    bitmap_set(raw, bo, row_idx, true);
                }
                let off_u16 = if b.is_empty() {
                    0u16
                } else {
                    *var_heap_top -= b.len();
                    raw[*var_heap_top..*var_heap_top + b.len()].copy_from_slice(b);
                    *var_heap_top as u16
                };
                let len_tag = (b.len() as u32) | if is_blob { 0x8000 } else { 0 };
                let desc = u32::from(off_u16) | (len_tag << 16);
                raw[doff + 4 * row_idx..doff + 4 * row_idx + 4]
                    .copy_from_slice(&desc.to_le_bytes());
            }
            _ => unreachable!(),
        },
        PAX_COL_MIXED => {
            let (tag, word) = match field {
                ParsedPaxField::Null => (PAX_MIXED_NULL, 0u64),
                ParsedPaxField::Int(v) => (PAX_MIXED_INT, v as u64),
                ParsedPaxField::Real(r) => (PAX_MIXED_REAL, r.to_bits()),
                ParsedPaxField::Text(b) | ParsedPaxField::Blob(b) => {
                    let tag = if matches!(field, ParsedPaxField::Blob(_)) {
                        PAX_MIXED_BLOB
                    } else {
                        PAX_MIXED_TEXT
                    };
                    let off = if b.is_empty() {
                        0u32
                    } else {
                        *var_heap_top -= b.len();
                        raw[*var_heap_top..*var_heap_top + b.len()].copy_from_slice(b);
                        *var_heap_top as u32
                    };
                    (tag, u64::from(off) | ((b.len() as u64) << 32))
                }
            };
            raw[bo + row_idx] = tag;
            raw[doff + 8 * row_idx..doff + 8 * row_idx + 8].copy_from_slice(&word.to_le_bytes());
        }
        _ => unreachable!(),
    }
}

/// Initializes a fresh `0x2d` PAX table leaf containing a single row `(rowid, input)`.
/// Returns `Ok(false)` without mutating `raw` if `usable < 1024` or `input` is ineligible for PAX.
pub fn init_pax_table_leaf_single(
    usable_u32: u32,
    raw: &mut [u8],
    rowid: i64,
    input: PaxValueInput<'_>,
) -> Result<bool> {
    let usable = usable_u32 as usize;
    if usable < PAX_MIN_USABLE_SIZE || raw.len() < usable {
        return Ok(false);
    }
    let mut fields = [ParsedPaxField::Null; PAX_MAX_COLS];
    let Some((n_cols, var_bytes)) = parse_pax_input(usable, input, &mut fields) else {
        return Ok(false);
    };

    let mut modes = [PAX_COL_ALL_NULL; PAX_MAX_COLS];
    let flags = [0u8; PAX_MAX_COLS];
    for c in 0..n_cols {
        modes[c] = fields[c].required_mode();
    }

    let Some((cap, layout)) = choose_pax_table_cap(usable, n_cols, &modes, &flags, 1, var_bytes)
    else {
        return Ok(false);
    };

    raw[..layout.fixed_zone_end].fill(0);
    let mut var_heap_top = usable;
    raw[layout.rowid_off..layout.rowid_off + 8].copy_from_slice(&rowid.to_le_bytes());
    for (dir, &field) in layout.dirs.iter().zip(fields.iter()).take(n_cols) {
        write_field_into_slot(raw, *dir, 0, field, &mut var_heap_top);
    }
    write_pax_table_header(raw, n_cols, 1, var_heap_top, cap, &layout, 0);
    Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaxTableInsertOutcome {
    Inserted,
    Duplicate,
    NeedsSplit { last_rowid: i64 },
    EscapeToSlotted,
}

fn shift_pax_bitmap_insert(raw: &mut [u8], bo: usize, n_rows: usize, insert_idx: usize, val: bool) {
    for r in (insert_idx..n_rows).rev() {
        let bit = bitmap_get(raw, bo, r);
        bitmap_set(raw, bo, r + 1, bit);
    }
    bitmap_set(raw, bo, insert_idx, val);
}

fn shift_pax_bitmap_delete(raw: &mut [u8], bo: usize, n_rows: usize, del_idx: usize) {
    for r in del_idx..(n_rows - 1) {
        let bit = bitmap_get(raw, bo, r + 1);
        bitmap_set(raw, bo, r, bit);
    }
    bitmap_set(raw, bo, n_rows - 1, false);
}

#[inline]
fn raw_field_to_parsed<'a>(rf: RawField<'a>) -> ParsedPaxField<'a> {
    match rf {
        RawField::Null => ParsedPaxField::Null,
        RawField::Int(v) => ParsedPaxField::Int(v),
        RawField::Float(r) => ParsedPaxField::Real(r),
        RawField::Text(b) => ParsedPaxField::Text(b),
        RawField::Blob(b) => ParsedPaxField::Blob(b),
    }
}

/// Repacks `src_raw` (`0x2d`) alongside an inserted or replaced row `(rowid, new_fields[..new_n_cols])`
/// into `dst_raw[..usable]`, computing widened column modes/null flags and recalculating optimal `cap`.
/// Returns `Ok(true)` if the resulting set of rows fits in a single `0x2d` page.
#[allow(clippy::too_many_arguments)]
fn repack_pax_table_with_op(
    pgno: Pgno,
    usable_u32: u32,
    src_raw: &[u8],
    dst_raw: &mut [u8],
    insert_or_replace: Option<(usize, bool, i64, &[ParsedPaxField<'_>], usize)>,
) -> Result<bool> {
    let usable = usable_u32 as usize;
    let view = PaxTableLeafView::new(pgno, usable_u32, src_raw)?;
    let (op_idx, is_replace, new_rid, new_fields, new_n_cols) = match insert_or_replace {
        Some(t) => (Some(t.0), t.1, t.2, t.3, t.4),
        None => (None, false, 0, &[][..], view.n_cols),
    };
    let final_n_cols = view.n_cols.max(new_n_cols);
    if final_n_cols > PAX_MAX_COLS {
        return Ok(false);
    }
    let final_n_rows = match op_idx {
        None => view.n_rows,
        Some(_) if is_replace => view.n_rows,
        Some(_) => view.n_rows + 1,
    };
    if final_n_rows == 0 {
        reset_empty_slotted_leaf(usable_u32, dst_raw, BtreePageType::TableLeaf);
        return Ok(true);
    }

    // Scan exact column modes, null flags, and live variable byte count across all `final_n_rows`.
    let mut modes = [PAX_COL_ALL_NULL; PAX_MAX_COLS];
    let mut flags = [0u8; PAX_MAX_COLS];
    let mut seen_null = [false; PAX_MAX_COLS];
    let mut total_var_bytes = 0usize;

    let mut observe_field = |c: usize, f: ParsedPaxField<'_>| {
        if matches!(f, ParsedPaxField::Null) {
            seen_null[c] = true;
            if modes[c] != PAX_COL_ALL_NULL && modes[c] != PAX_COL_MIXED {
                flags[c] |= PAX_COL_HAS_NULLS;
            }
        } else {
            total_var_bytes += f.var_len();
            let (nm, mut nf) = merge_col_mode_and_flags(modes[c], flags[c], f);
            if nm != PAX_COL_MIXED && seen_null[c] {
                nf |= PAX_COL_HAS_NULLS;
            }
            modes[c] = nm;
            flags[c] = nf;
        }
    };

    for r in 0..view.n_rows {
        if op_idx == Some(r) && is_replace {
            for c in 0..final_n_cols {
                let f = new_fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
                observe_field(c, f);
            }
            continue;
        }
        for c in 0..final_n_cols {
            let f = if c < view.n_cols {
                raw_field_to_parsed(view.read_raw_field(c, r)?.unwrap_or(RawField::Null))
            } else {
                ParsedPaxField::Null
            };
            observe_field(c, f);
        }
    }
    if let Some(_ins_i) = op_idx
        && !is_replace
    {
        for c in 0..final_n_cols {
            let f = new_fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
            observe_field(c, f);
        }
    }

    let Some((new_cap, layout)) = choose_pax_table_cap(
        usable,
        final_n_cols,
        &modes,
        &flags,
        final_n_rows,
        total_var_bytes,
    ) else {
        return Ok(false);
    };

    dst_raw[..layout.fixed_zone_end].fill(0);
    let mut var_heap_top = usable;
    let mut out_r = 0usize;

    let emit_new = |dst: &mut [u8], slot: usize, heap_top: &mut usize| {
        let ro = layout.rowid_off + 8 * slot;
        dst[ro..ro + 8].copy_from_slice(&new_rid.to_le_bytes());
        for (c, dir) in layout.dirs.iter().enumerate().take(final_n_cols) {
            let f = new_fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
            write_field_into_slot(dst, *dir, slot, f, heap_top);
        }
    };

    for r in 0..view.n_rows {
        if op_idx == Some(r) {
            emit_new(dst_raw, out_r, &mut var_heap_top);
            out_r += 1;
            if is_replace {
                continue;
            }
        }
        let rid = view.rowid_at(r);
        let ro = layout.rowid_off + 8 * out_r;
        dst_raw[ro..ro + 8].copy_from_slice(&rid.to_le_bytes());
        for c in 0..final_n_cols {
            let f = if c < view.n_cols {
                raw_field_to_parsed(view.read_raw_field(c, r)?.unwrap_or(RawField::Null))
            } else {
                ParsedPaxField::Null
            };
            write_field_into_slot(dst_raw, layout.dirs[c], out_r, f, &mut var_heap_top);
        }
        out_r += 1;
    }
    if op_idx == Some(view.n_rows) && !is_replace {
        emit_new(dst_raw, out_r, &mut var_heap_top);
        out_r += 1;
    }
    debug_assert_eq!(out_r, final_n_rows);
    debug_assert!(var_heap_top >= layout.fixed_zone_end);

    write_pax_table_header(
        dst_raw,
        final_n_cols,
        final_n_rows,
        var_heap_top,
        new_cap,
        &layout,
        0,
    );
    Ok(true)
}

/// Inserts or replaces `(rowid, input)` into a `0x2d` PAX table leaf (`pgno >= 2`),
/// using an **`O(cols)` zero-`memmove` fast path** on sequential appends!
pub fn try_insert_pax_table_leaf(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    rowid: i64,
    input: PaxValueInput<'_>,
    allow_replace: bool,
) -> Result<PaxTableInsertOutcome> {
    let usable = usable_u32 as usize;
    if pgno < 2 || usable < PAX_MIN_USABLE_SIZE {
        return Ok(PaxTableInsertOutcome::EscapeToSlotted);
    }

    // Upgrade empty `0x0d` leaf (`n_cells == 0`) directly to `0x2d` PAX!
    if raw[0] == BtreePageType::TableLeaf as u8 {
        let n_cell = u16::from_be_bytes([raw[3], raw[4]]);
        if n_cell == 0 {
            return if init_pax_table_leaf_single(usable_u32, raw, rowid, input)? {
                Ok(PaxTableInsertOutcome::Inserted)
            } else {
                Ok(PaxTableInsertOutcome::EscapeToSlotted)
            };
        }
        return Ok(PaxTableInsertOutcome::EscapeToSlotted);
    }

    if let PaxValueInput::Refs(r) = input
        && r.len() <= 8
    {
        let mut small_fields = [ParsedPaxField::Null; 8];
        let Some((in_cols, in_var_bytes)) = parse_pax_input(usable, input, &mut small_fields)
        else {
            return Ok(PaxTableInsertOutcome::EscapeToSlotted);
        };
        return try_insert_pax_table_leaf_parsed(
            pgno,
            usable_u32,
            raw,
            rowid,
            &small_fields[..in_cols],
            in_var_bytes,
            allow_replace,
        );
    }
    try_insert_pax_table_leaf_wide(pgno, usable_u32, raw, rowid, input, allow_replace)
}

#[inline(never)]
fn try_insert_pax_table_leaf_wide(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    rowid: i64,
    input: PaxValueInput<'_>,
    allow_replace: bool,
) -> Result<PaxTableInsertOutcome> {
    let usable = usable_u32 as usize;
    let mut fields = [ParsedPaxField::Null; PAX_MAX_COLS];
    let Some((in_cols, in_var_bytes)) = parse_pax_input(usable, input, &mut fields) else {
        return Ok(PaxTableInsertOutcome::EscapeToSlotted);
    };
    try_insert_pax_table_leaf_parsed(
        pgno,
        usable_u32,
        raw,
        rowid,
        &fields[..in_cols],
        in_var_bytes,
        allow_replace,
    )
}

#[inline(never)]
fn try_repack_pax_table_leaf_in_place(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    op_idx: usize,
    is_replace: bool,
    rowid: i64,
    fields: &[ParsedPaxField<'_>],
) -> Result<bool> {
    let usable = usable_u32 as usize;
    let in_cols = fields.len();
    if usable <= 4096 {
        let mut stack_dst = [0u8; 4096];
        let ok = repack_pax_table_with_op(
            pgno,
            usable_u32,
            &raw[..usable],
            &mut stack_dst[..usable],
            Some((op_idx, is_replace, rowid, fields, in_cols)),
        )?;
        if ok {
            raw[..usable].copy_from_slice(&stack_dst[..usable]);
        }
        Ok(ok)
    } else {
        let mut heap_dst = vec![0u8; usable];
        let ok = repack_pax_table_with_op(
            pgno,
            usable_u32,
            &raw[..usable],
            &mut heap_dst,
            Some((op_idx, is_replace, rowid, fields, in_cols)),
        )?;
        if ok {
            raw[..usable].copy_from_slice(&heap_dst);
        }
        Ok(ok)
    }
}

#[inline]
fn try_insert_pax_table_leaf_parsed(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    rowid: i64,
    fields: &[ParsedPaxField<'_>],
    in_var_bytes: usize,
    allow_replace: bool,
) -> Result<PaxTableInsertOutcome> {
    let in_cols = fields.len();
    let (n_rows, cap, n_cols, var_heap_top, fixed_zone_end, dead_var_bytes, rowid_off, pos) = {
        let view = PaxTableLeafView::new(pgno, usable_u32, raw)?;
        view.validate_dirs()?;
        let pos = if view.n_rows == 0 {
            Err(0)
        } else {
            let last_rid = view.rowid_at(view.n_rows - 1);
            if rowid > last_rid {
                Err(view.n_rows)
            } else if rowid == last_rid {
                Ok(view.n_rows - 1)
            } else {
                view.binary_search_rowid(rowid)
            }
        };
        (
            view.n_rows,
            view.cap,
            view.n_cols,
            view.var_heap_top,
            view.fixed_zone_end,
            view.dead_var_bytes,
            view.rowid_off,
            pos,
        )
    };

    if pos.is_ok() && !allow_replace {
        return Ok(PaxTableInsertOutcome::Duplicate);
    }

    // Check whether all incoming columns are compatible with the page's current `(n_cols, dirs)`!
    let cols_compatible = in_cols <= n_cols && {
        let mut ok = true;
        for c in 0..n_cols {
            let f = fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
            let d = dir_at_raw(raw, c);
            let compat = match f {
                ParsedPaxField::Null => {
                    d.mode == PAX_COL_ALL_NULL
                        || d.mode == PAX_COL_MIXED
                        || (d.flags & PAX_COL_HAS_NULLS) != 0
                }
                ParsedPaxField::Int(v) => {
                    d.mode == PAX_COL_MIXED
                        || d.mode == PAX_COL_I64
                        || (d.mode == PAX_COL_I32 && i32::try_from(v).is_ok())
                }
                ParsedPaxField::Real(_) => d.mode == PAX_COL_F64 || d.mode == PAX_COL_MIXED,
                ParsedPaxField::Text(_) | ParsedPaxField::Blob(_) => {
                    d.mode == PAX_COL_BYTES || d.mode == PAX_COL_MIXED
                }
            };
            if !compat {
                ok = false;
                break;
            }
        }
        ok
    };

    match pos {
        // --------------------------------------------------------------------
        // HOTTEST PATH: Sequential Append (`insert_idx == n_rows`) — ZERO `memmove`!
        // --------------------------------------------------------------------
        Err(ins_idx)
            if ins_idx == n_rows
                && cols_compatible
                && n_rows < cap
                && fixed_zone_end + in_var_bytes <= var_heap_top =>
        {
            let mut new_heap_top = var_heap_top;
            let ro = rowid_off + 8 * n_rows;
            raw[ro..ro + 8].copy_from_slice(&rowid.to_le_bytes());
            for c in 0..n_cols {
                let f = fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
                let d = dir_at_raw(raw, c);
                write_field_into_slot(raw, d, n_rows, f, &mut new_heap_top);
            }
            let new_n = (n_rows + 1) as u16;
            raw[3..5].copy_from_slice(&new_n.to_be_bytes());
            raw[5..7].copy_from_slice(&encode_var_heap_top(new_heap_top).to_le_bytes());
            return Ok(PaxTableInsertOutcome::Inserted);
        }
        // Interior insert when columns match and slot + heap have room: shift `[ins_idx..n_rows]` by 1.
        Err(ins_idx)
            if cols_compatible && n_rows < cap && fixed_zone_end + in_var_bytes <= var_heap_top =>
        {
            let mut new_heap_top = var_heap_top;
            raw.copy_within(
                rowid_off + 8 * ins_idx..rowid_off + 8 * n_rows,
                rowid_off + 8 * (ins_idx + 1),
            );
            raw[rowid_off + 8 * ins_idx..rowid_off + 8 * ins_idx + 8]
                .copy_from_slice(&rowid.to_le_bytes());

            for c in 0..n_cols {
                let d = dir_at_raw(raw, c);
                let bo = usize::from(d.bitmap_off);
                let doff = usize::from(d.data_off);
                match d.mode {
                    PAX_COL_ALL_NULL => {}
                    PAX_COL_I32 | PAX_COL_BYTES => {
                        raw.copy_within(
                            doff + 4 * ins_idx..doff + 4 * n_rows,
                            doff + 4 * (ins_idx + 1),
                        );
                        if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                            shift_pax_bitmap_insert(raw, bo, n_rows, ins_idx, false);
                        }
                    }
                    PAX_COL_I64 | PAX_COL_F64 => {
                        raw.copy_within(
                            doff + 8 * ins_idx..doff + 8 * n_rows,
                            doff + 8 * (ins_idx + 1),
                        );
                        if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                            shift_pax_bitmap_insert(raw, bo, n_rows, ins_idx, false);
                        }
                    }
                    PAX_COL_MIXED => {
                        raw.copy_within(
                            doff + 8 * ins_idx..doff + 8 * n_rows,
                            doff + 8 * (ins_idx + 1),
                        );
                        raw.copy_within(bo + ins_idx..bo + n_rows, bo + ins_idx + 1);
                    }
                    _ => unreachable!(),
                }
                let f = fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
                write_field_into_slot(raw, d, ins_idx, f, &mut new_heap_top);
            }
            let new_n = (n_rows + 1) as u16;
            raw[3..5].copy_from_slice(&new_n.to_be_bytes());
            raw[5..7].copy_from_slice(&encode_var_heap_top(new_heap_top).to_le_bytes());
            return Ok(PaxTableInsertOutcome::Inserted);
        }
        // In-place row replacement when columns match and `var_heap` has room!
        Ok(rep_idx) if cols_compatible && fixed_zone_end + in_var_bytes <= var_heap_top => {
            let old_var = {
                let v = PaxTableLeafView::new(pgno, usable_u32, raw)?;
                let mut s = 0usize;
                for c in 0..n_cols {
                    if let Some(RawField::Text(b) | RawField::Blob(b)) =
                        v.read_raw_field(c, rep_idx)?
                    {
                        s += b.len();
                    }
                }
                s
            };
            let mut new_heap_top = var_heap_top;
            for c in 0..n_cols {
                let f = fields.get(c).copied().unwrap_or(ParsedPaxField::Null);
                let d = dir_at_raw(raw, c);
                write_field_into_slot(raw, d, rep_idx, f, &mut new_heap_top);
            }
            raw[5..7].copy_from_slice(&encode_var_heap_top(new_heap_top).to_le_bytes());
            let new_dead = (dead_var_bytes + old_var).min(65_535) as u16;
            raw[12..14].copy_from_slice(&new_dead.to_le_bytes());
            return Ok(PaxTableInsertOutcome::Inserted);
        }
        _ => {}
    }

    // Fast bail-out BEFORE attempting a stack repack if we know for sure the page is truly full:
    // specifically, when `cols_compatible`, `!is_replace`, `dead_var_bytes == 0`, AND `n_rows == cap`
    // while `var_heap_top - fixed_zone_end < 64 + in_var_bytes`!
    let is_replace = pos.is_ok();
    let op_idx = match pos {
        Ok(i) | Err(i) => i,
    };
    if cols_compatible
        && !is_replace
        && dead_var_bytes == 0
        && n_rows == cap
        && (var_heap_top - fixed_zone_end) < 64 + in_var_bytes
    {
        let last_rowid = i64::from_le_bytes(
            raw[rowid_off + 8 * (n_rows - 1)..rowid_off + 8 * n_rows]
                .try_into()
                .unwrap(),
        );
        return Ok(PaxTableInsertOutcome::NeedsSplit { last_rowid });
    }

    // Attempt an in-page promotion / `dead_var_bytes` GC / dynamic `cap` adjustment repack!
    if try_repack_pax_table_leaf_in_place(pgno, usable_u32, raw, op_idx, is_replace, rowid, fields)?
    {
        return Ok(PaxTableInsertOutcome::Inserted);
    }

    let last_rowid = if n_rows > 0 {
        i64::from_le_bytes(
            raw[rowid_off + 8 * (n_rows - 1)..rowid_off + 8 * n_rows]
                .try_into()
                .unwrap(),
        )
    } else {
        rowid
    };
    Ok(PaxTableInsertOutcome::NeedsSplit { last_rowid })
}

/// Splits a full `0x2d` PAX table leaf (`raw`) + incoming `(rowid, input)` into two packed `0x2d`
/// leaves (`raw` = left, `right_raw` = right), returning `Ok(Some(left_max_rowid))`.
/// - When `right_heavy == true` and `rowid > last_rowid` (`balance_quick`), `raw` stays 100% untouched
///   and `right_raw` is initialized as a fresh 1-row `0x2d` PAX leaf in `O(cols)`!
/// - Otherwise, splits `(n_rows + 1)` rows 50/50 into two `0x2d` PAX leaves!
pub fn split_pax_table_leaf<'a>(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    right_raw: &mut [u8],
    rowid: i64,
    input: PaxValueInput<'a>,
    right_heavy: bool,
) -> Result<Option<i64>> {
    let usable = usable_u32 as usize;
    if pgno < 2 || usable < PAX_MIN_USABLE_SIZE {
        return Ok(None);
    }
    let mut fields = [ParsedPaxField::Null; PAX_MAX_COLS];
    let Some((in_cols, _)) = parse_pax_input(usable, input, &mut fields) else {
        return Ok(None);
    };

    // Fast right-heavy append split (`rowid > last_rowid`) — zero rewrite of `raw`!
    {
        let view = PaxTableLeafView::new(pgno, usable_u32, raw)?;
        if right_heavy
            && let Some(last_rid) = view.last_rowid()
            && rowid > last_rid
            && init_pax_table_leaf_single(usable_u32, right_raw, rowid, input)?
        {
            return Ok(Some(last_rid));
        }
    }

    // General 50/50 PAX split (works for random inserts AND in-place replace overflows).
    let src_copy = raw[..usable].to_vec();
    split_pax_table_leaf_from_copy(
        pgno, usable_u32, &src_copy, raw, right_raw, rowid, &fields, in_cols,
    )
}

#[allow(clippy::too_many_arguments)]
fn split_pax_table_leaf_from_copy<'b>(
    pgno: Pgno,
    usable_u32: u32,
    src_copy: &'b [u8],
    raw: &mut [u8],
    right_raw: &mut [u8],
    rowid: i64,
    fields: &[ParsedPaxField<'b>; PAX_MAX_COLS],
    in_cols: usize,
) -> Result<Option<i64>> {
    let usable = usable_u32 as usize;
    let view = PaxTableLeafView::new(pgno, usable_u32, src_copy)?;
    let pos = view.binary_search_rowid(rowid);
    let is_replace = pos.is_ok();
    let op_idx = match pos {
        Ok(i) | Err(i) => i,
    };
    let total_rows = if is_replace {
        view.n_rows
    } else {
        view.n_rows + 1
    };
    if total_rows < 2 {
        return Ok(None);
    }
    let final_n_cols = view.n_cols.max(in_cols);
    if final_n_cols > PAX_MAX_COLS {
        return Ok(None);
    }

    let mid = total_rows / 2;

    let build_half =
        |dst: &mut [u8], range_start: usize, range_end: usize| -> Result<Option<i64>> {
            let count = range_end - range_start;
            let mut modes = [PAX_COL_ALL_NULL; PAX_MAX_COLS];
            let mut flags = [0u8; PAX_MAX_COLS];
            let mut seen_null = [false; PAX_MAX_COLS];
            let mut var_sum = 0usize;

            let get_virtual_row =
                |v_idx: usize, row_out: &mut [ParsedPaxField<'b>; PAX_MAX_COLS]| -> Result<i64> {
                    let src_r = if is_replace {
                        if v_idx == op_idx {
                            for c in 0..final_n_cols {
                                row_out[c] = if c < in_cols {
                                    fields[c]
                                } else {
                                    ParsedPaxField::Null
                                };
                            }
                            return Ok(rowid);
                        }
                        v_idx
                    } else if v_idx < op_idx {
                        v_idx
                    } else if v_idx == op_idx {
                        for c in 0..final_n_cols {
                            row_out[c] = if c < in_cols {
                                fields[c]
                            } else {
                                ParsedPaxField::Null
                            };
                        }
                        return Ok(rowid);
                    } else {
                        v_idx - 1
                    };

                    for (c, slot) in row_out.iter_mut().enumerate().take(final_n_cols) {
                        *slot = if c < view.n_cols {
                            raw_field_to_parsed(
                                view.read_raw_field(c, src_r)?.unwrap_or(RawField::Null),
                            )
                        } else {
                            ParsedPaxField::Null
                        };
                    }
                    Ok(view.rowid_at(src_r))
                };

            let mut row_tmp = [ParsedPaxField::Null; PAX_MAX_COLS];
            let mut last_rid = 0i64;
            for v_idx in range_start..range_end {
                last_rid = get_virtual_row(v_idx, &mut row_tmp)?;
                for c in 0..final_n_cols {
                    let f = row_tmp[c];
                    if matches!(f, ParsedPaxField::Null) {
                        seen_null[c] = true;
                        if modes[c] != PAX_COL_ALL_NULL && modes[c] != PAX_COL_MIXED {
                            flags[c] |= PAX_COL_HAS_NULLS;
                        }
                    } else {
                        var_sum += f.var_len();
                        let (nm, mut nf) = merge_col_mode_and_flags(modes[c], flags[c], f);
                        if nm != PAX_COL_MIXED && seen_null[c] {
                            nf |= PAX_COL_HAS_NULLS;
                        }
                        modes[c] = nm;
                        flags[c] = nf;
                    }
                }
            }

            let Some((cap, layout)) =
                choose_pax_table_cap(usable, final_n_cols, &modes, &flags, count, var_sum)
            else {
                return Ok(None);
            };

            dst[..layout.fixed_zone_end].fill(0);
            let mut heap_top = usable;
            for (slot, v_idx) in (range_start..range_end).enumerate() {
                let rid = get_virtual_row(v_idx, &mut row_tmp)?;
                let ro = layout.rowid_off + 8 * slot;
                dst[ro..ro + 8].copy_from_slice(&rid.to_le_bytes());
                for (dir, &field) in layout.dirs.iter().zip(row_tmp.iter()).take(final_n_cols) {
                    write_field_into_slot(dst, *dir, slot, field, &mut heap_top);
                }
            }
            write_pax_table_header(dst, final_n_cols, count, heap_top, cap, &layout, 0);
            Ok(Some(last_rid))
        };

    let Some(left_max_rid) = build_half(raw, 0, mid)? else {
        raw[..usable].copy_from_slice(src_copy);
        return Ok(None);
    };
    if build_half(right_raw, mid, total_rows)?.is_none() {
        raw[..usable].copy_from_slice(src_copy);
        return Ok(None);
    }
    Ok(Some(left_max_rid))
}

/// Overwrites 1 or more scalar columns (`I32`, `I64`, `F64`, `MIXED` int/real) of `row_idx`
/// **directly in-place** on a `0x2d` PAX table leaf in `O(updates)` with ZERO record encoding!
/// Returns `Ok(true)` if all `updates` were applied in-place, or `Ok(false)` if any column requires
/// mode promotion / heap allocation (caller falls back to `try_insert_pax_table_leaf`).
#[inline]
pub fn try_update_pax_table_row_scalars_at(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    row_idx: usize,
    updates: &[(usize, Value)],
) -> Result<bool> {
    let (n_cols, n_rows) = {
        let view = PaxTableLeafView::new(pgno, usable_u32, raw)?;
        if row_idx >= view.n_rows {
            return Ok(false);
        }
        view.validate_dirs()?;
        (view.n_cols, view.n_rows)
    };
    let _ = n_rows;

    // Pre-validate every column update before touching `raw`!
    for &(col_idx, ref val) in updates {
        if col_idx >= n_cols {
            return Ok(false);
        }
        let d = dir_at_raw(raw, col_idx);
        let ok = match val {
            Value::Null => {
                d.mode == PAX_COL_ALL_NULL
                    || d.mode == PAX_COL_MIXED
                    || (d.flags & PAX_COL_HAS_NULLS) != 0
            }
            Value::Integer(v) => {
                d.mode == PAX_COL_I64
                    || d.mode == PAX_COL_MIXED
                    || (d.mode == PAX_COL_I32 && i32::try_from(*v).is_ok())
            }
            Value::Real(r) => {
                if r.is_nan() {
                    d.mode == PAX_COL_ALL_NULL
                        || d.mode == PAX_COL_MIXED
                        || (d.flags & PAX_COL_HAS_NULLS) != 0
                } else {
                    d.mode == PAX_COL_F64 || d.mode == PAX_COL_MIXED
                }
            }
            Value::Text(_) | Value::Blob(_) => false,
        };
        if !ok {
            return Ok(false);
        }
    }

    for &(col_idx, ref val) in updates {
        let d = dir_at_raw(raw, col_idx);
        let bo = usize::from(d.bitmap_off);
        let doff = usize::from(d.data_off);
        let field = match val {
            Value::Null => ParsedPaxField::Null,
            Value::Integer(v) => ParsedPaxField::Int(*v),
            Value::Real(r) if r.is_nan() => ParsedPaxField::Null,
            Value::Real(r) => ParsedPaxField::Real(*r),
            _ => unreachable!(),
        };
        let mut dummy_heap = 0usize;
        // Track dead bytes if overwriting a `MIXED` text/blob slot with a scalar.
        if d.mode == PAX_COL_MIXED {
            let old_tag = raw[bo + row_idx];
            if old_tag == PAX_MIXED_TEXT || old_tag == PAX_MIXED_BLOB {
                let o = doff + 8 * row_idx;
                let word = u64::from_le_bytes(raw[o..o + 8].try_into().unwrap());
                let old_len = (word >> 32) as usize;
                let cur_dead = usize::from(u16::from_le_bytes([raw[12], raw[13]]));
                raw[12..14]
                    .copy_from_slice(&((cur_dead + old_len).min(65_535) as u16).to_le_bytes());
            }
        }
        write_field_into_slot(raw, d, row_idx, field, &mut dummy_heap);
    }
    Ok(true)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaxTableDeleteOutcome {
    NotFound,
    Deleted { now_empty: bool },
}

/// Deletes `rowid` from a `0x2d` PAX table leaf (`pgno >= 2`), optionally encoding the deleted
/// row's canonical Format-3 record into `old_payload_out`.
pub fn delete_pax_table_leaf_row(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    rowid: i64,
    is_root: bool,
    old_payload_out: Option<&mut Vec<u8>>,
) -> Result<PaxTableDeleteOutcome> {
    let (del_idx, n_rows, n_cols, rowid_off, dead_var_bytes, freed_var) = {
        let view = PaxTableLeafView::new(pgno, usable_u32, raw)?;
        view.validate_dirs()?;
        let Ok(idx) = view.binary_search_rowid(rowid) else {
            return Ok(PaxTableDeleteOutcome::NotFound);
        };
        if let Some(out) = old_payload_out {
            view.encode_row_record_into(idx, out)?;
        }
        let mut freed = 0usize;
        for c in 0..view.n_cols {
            if let Some(RawField::Text(b) | RawField::Blob(b)) = view.read_raw_field(c, idx)? {
                freed += b.len();
            }
        }
        (
            idx,
            view.n_rows,
            view.n_cols,
            view.rowid_off,
            view.dead_var_bytes,
            freed,
        )
    };

    if n_rows == 1 {
        if is_root {
            reset_empty_slotted_leaf(usable_u32, raw, BtreePageType::TableLeaf);
        }
        return Ok(PaxTableDeleteOutcome::Deleted { now_empty: true });
    }

    if del_idx + 1 < n_rows {
        raw.copy_within(
            rowid_off + 8 * (del_idx + 1)..rowid_off + 8 * n_rows,
            rowid_off + 8 * del_idx,
        );
    }
    for c in 0..n_cols {
        let d = dir_at_raw(raw, c);
        let bo = usize::from(d.bitmap_off);
        let doff = usize::from(d.data_off);
        match d.mode {
            PAX_COL_ALL_NULL => {}
            PAX_COL_I32 | PAX_COL_BYTES => {
                if del_idx + 1 < n_rows {
                    raw.copy_within(
                        doff + 4 * (del_idx + 1)..doff + 4 * n_rows,
                        doff + 4 * del_idx,
                    );
                }
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    shift_pax_bitmap_delete(raw, bo, n_rows, del_idx);
                }
            }
            PAX_COL_I64 | PAX_COL_F64 => {
                if del_idx + 1 < n_rows {
                    raw.copy_within(
                        doff + 8 * (del_idx + 1)..doff + 8 * n_rows,
                        doff + 8 * del_idx,
                    );
                }
                if (d.flags & PAX_COL_HAS_NULLS) != 0 {
                    shift_pax_bitmap_delete(raw, bo, n_rows, del_idx);
                }
            }
            PAX_COL_MIXED => {
                if del_idx + 1 < n_rows {
                    raw.copy_within(
                        doff + 8 * (del_idx + 1)..doff + 8 * n_rows,
                        doff + 8 * del_idx,
                    );
                    raw.copy_within(bo + del_idx + 1..bo + n_rows, bo + del_idx);
                }
                raw[bo + n_rows - 1] = 0;
            }
            _ => unreachable!(),
        }
    }

    let new_n = (n_rows - 1) as u16;
    raw[3..5].copy_from_slice(&new_n.to_be_bytes());
    let new_dead = (dead_var_bytes + freed_var).min(65_535) as u16;
    raw[12..14].copy_from_slice(&new_dead.to_le_bytes());
    Ok(PaxTableDeleteOutcome::Deleted { now_empty: false })
}

// ============================================================================
// Part 2: `0x2a` PAX / SoA Covering Index Leaf (`PAX_INDEX_LEAF_BYTE = 0x2a`)
// ============================================================================

const IDX_W_NONE: u8 = 0;
const IDX_W_I32: u8 = 1;
const IDX_W_I64: u8 = 2;

const IDX_FLAG_K0_NULLS: u8 = 0x01;
const IDX_FLAG_K1_NULLS: u8 = 0x02;

/// Returns `Some(n_keys)` (`1` for `(k0, rid)` or `2` for `(k0, k1, rid)`) when `orders` is
/// eligible for `0x2a` SoA covering index leaves (default ASC `Binary` sort on all columns).
#[inline]
pub fn orders_allow_pax_index(orders: &[KeySortOrder]) -> Option<u8> {
    let n_keys = match orders.len() {
        2 => 1u8,
        3 => 2u8,
        _ => return None,
    };
    for so in orders {
        if so.desc
            || so.nulls_last
            || so.collation != Collation::Binary
            || so.custom_collation.is_some()
        {
            return None;
        }
    }
    Some(n_keys)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaxIndexEntry {
    pub k0: Option<i64>,
    pub k1: Option<i64>,
    pub rid: i64,
}

impl PaxIndexEntry {
    #[inline(always)]
    fn required_widths_and_flags(self, n_keys: u8) -> (u8, u8) {
        let k0_w = match self.k0 {
            Some(v) if i32::try_from(v).is_err() => IDX_W_I64,
            _ => IDX_W_I32,
        };
        let k1_w = if n_keys == 2 {
            match self.k1 {
                Some(v) if i32::try_from(v).is_err() => IDX_W_I64,
                _ => IDX_W_I32,
            }
        } else {
            IDX_W_NONE
        };
        let rid_w = if i32::try_from(self.rid).is_ok() {
            IDX_W_I32
        } else {
            IDX_W_I64
        };
        let mut nf = 0u8;
        if self.k0.is_none() {
            nf |= IDX_FLAG_K0_NULLS;
        }
        if n_keys == 2 && self.k1.is_none() {
            nf |= IDX_FLAG_K1_NULLS;
        }
        (k0_w | (k1_w << 2) | (rid_w << 4), nf)
    }
}

#[inline(always)]
fn merge_index_widths(a: u8, b: u8, n_keys: u8) -> u8 {
    let k0 = (a & 0x03).max(b & 0x03).max(IDX_W_I32);
    let k1 = if n_keys == 2 {
        ((a >> 2) & 0x03).max((b >> 2) & 0x03).max(IDX_W_I32)
    } else {
        0
    };
    let rid = ((a >> 4) & 0x03).max((b >> 4) & 0x03).max(IDX_W_I32);
    k0 | (k1 << 2) | (rid << 4)
}

/// Returns `true` when all `entries` fit inside a single `0x2a` PAX index leaf of `usable_u32` bytes.
pub fn entries_fit_single_pax_index_leaf(
    usable_u32: u32,
    n_keys: u8,
    entries: &[PaxIndexEntry],
) -> bool {
    if entries.is_empty() {
        return true;
    }
    let (mut w, mut nf) = entries[0].required_widths_and_flags(n_keys);
    let mut cap = max_pax_index_cap_for_widths(usable_u32, n_keys, w, nf);
    if entries.len() > cap {
        return false;
    }
    for e in &entries[1..] {
        let (ew, enf) = e.required_widths_and_flags(n_keys);
        let nw = merge_index_widths(w, ew, n_keys);
        let nnf = nf | enf;
        if nw != w || nnf != nf {
            w = nw;
            nf = nnf;
            cap = max_pax_index_cap_for_widths(usable_u32, n_keys, w, nf);
            if entries.len() > cap {
                return false;
            }
        }
    }
    entries.len() <= cap
}

/// Chooses the exclusive `end` index (`start < end <= entries.len()`) for the next ~90%-filled
/// `0x2a` PAX index leaf during `bulk_load_sorted_pax_index`, avoiding a 0-row rightmost leaf.
pub fn choose_pax_index_leaf_end(
    usable_u32: u32,
    n_keys: u8,
    entries: &[PaxIndexEntry],
    start: usize,
) -> usize {
    let total = entries.len();
    debug_assert!(start < total);
    let (mut w, mut nf) = entries[start].required_widths_and_flags(n_keys);
    let mut limit = (max_pax_index_cap_for_widths(usable_u32, n_keys, w, nf) * 9 / 10).max(1);
    let mut end = start + 1;
    while end < total {
        let (ew, enf) = entries[end].required_widths_and_flags(n_keys);
        let nw = merge_index_widths(w, ew, n_keys);
        let nnf = nf | enf;
        if nw != w || nnf != nf {
            limit = (max_pax_index_cap_for_widths(usable_u32, n_keys, nw, nnf) * 9 / 10).max(1);
            if end - start >= limit {
                break;
            }
            w = nw;
            nf = nnf;
        } else if end - start >= limit {
            break;
        }
        end += 1;
    }
    if end + 1 == total && end > start + 1 {
        end -= 1;
    }
    end
}

#[derive(Clone, Copy, Debug)]
struct ComputedIndexLayout {
    k0_off: usize,
    k1_off: usize,
    rid_off: usize,
    k0_bmap_off: usize,
    k1_bmap_off: usize,
    end_off: usize,
}

#[inline]
fn compute_pax_index_layout(
    n_keys: u8,
    widths: u8,
    null_flags: u8,
    cap: usize,
) -> Option<ComputedIndexLayout> {
    if !matches!(n_keys, 1 | 2) || cap == 0 || cap > 8192 {
        return None;
    }
    let k0_w = widths & 0x03;
    let k1_w = (widths >> 2) & 0x03;
    let rid_w = (widths >> 4) & 0x03;
    if !matches!(k0_w, IDX_W_I32 | IDX_W_I64) || !matches!(rid_w, IDX_W_I32 | IDX_W_I64) {
        return None;
    }
    if n_keys == 1 && k1_w != IDX_W_NONE {
        return None;
    }
    if n_keys == 2 && !matches!(k1_w, IDX_W_I32 | IDX_W_I64) {
        return None;
    }

    let mut pos = 16usize; // 8-byte aligned!
    let mut k0_off = 0usize;
    let mut k1_off = 0usize;
    let mut rid_off = 0usize;

    // 8-byte arrays first
    if k0_w == IDX_W_I64 {
        k0_off = pos;
        pos = pos.checked_add(8 * cap)?;
    }
    if k1_w == IDX_W_I64 {
        k1_off = pos;
        pos = pos.checked_add(8 * cap)?;
    }
    if rid_w == IDX_W_I64 {
        rid_off = pos;
        pos = pos.checked_add(8 * cap)?;
    }

    // 4-byte arrays next
    if k0_w == IDX_W_I32 {
        k0_off = pos;
        pos = pos.checked_add(4 * cap)?;
    }
    if k1_w == IDX_W_I32 {
        k1_off = pos;
        pos = pos.checked_add(4 * cap)?;
    }
    if rid_w == IDX_W_I32 {
        rid_off = pos;
        pos = pos.checked_add(4 * cap)?;
    }

    let bmap_len = cap.div_ceil(8);
    let mut k0_bmap_off = 0usize;
    let mut k1_bmap_off = 0usize;
    if (null_flags & IDX_FLAG_K0_NULLS) != 0 {
        k0_bmap_off = pos;
        pos = pos.checked_add(bmap_len)?;
    }
    if n_keys == 2 && (null_flags & IDX_FLAG_K1_NULLS) != 0 {
        k1_bmap_off = pos;
        pos = pos.checked_add(bmap_len)?;
    }
    if pos > 65_536 {
        return None;
    }
    Some(ComputedIndexLayout {
        k0_off,
        k1_off,
        rid_off,
        k0_bmap_off,
        k1_bmap_off,
        end_off: pos,
    })
}

pub fn max_pax_index_cap_for_widths(
    usable_u32: u32,
    n_keys: u8,
    widths: u8,
    null_flags: u8,
) -> usize {
    let usable = usable_u32 as usize;
    if usable < PAX_MIN_USABLE_SIZE {
        return 0;
    }
    let k0_b = if (widths & 0x03) == IDX_W_I64 { 8 } else { 4 };
    let k1_b = if n_keys == 2 {
        if ((widths >> 2) & 0x03) == IDX_W_I64 {
            8
        } else {
            4
        }
    } else {
        0
    };
    let rid_b = if ((widths >> 4) & 0x03) == IDX_W_I64 {
        8
    } else {
        4
    };
    let null_bits = usize::from((null_flags & IDX_FLAG_K0_NULLS) != 0)
        + usize::from(n_keys == 2 && (null_flags & IDX_FLAG_K1_NULLS) != 0);
    let bits_per_entry = (k0_b + k1_b + rid_b) * 8 + null_bits;
    let mut cap = (((usable - 16) * 8) / bits_per_entry).min(4096);
    while cap > 0 {
        if let Some(l) = compute_pax_index_layout(n_keys, widths, null_flags, cap)
            && l.end_off <= usable
        {
            return cap;
        }
        cap -= 1;
    }
    0
}

/// Parses a Format-3 index record into a [`PaxIndexEntry`] if it has exact arity `n_keys + 1`,
/// `Null`/`Integer` key columns, and a non-null `Integer` trailing `rowid`.
#[inline]
pub fn parse_record_as_pax_index_entry(rec: &[u8], n_keys: u8) -> Option<PaxIndexEntry> {
    let want = usize::from(n_keys) + 1;
    let mut f = [RawField::Null; 4];
    // Verify exact field count so `WITHOUT ROWID` tables carrying extra payload columns stay `0x0a`!
    let (hdr_u64, mut hdr_pos) = get_varint(rec).ok()?;
    let hdr_size = usize::try_from(hdr_u64).ok()?;
    if hdr_size > rec.len() || hdr_pos >= hdr_size {
        return None;
    }
    let mut cnt = 0usize;
    while hdr_pos < hdr_size {
        let (_, used) = get_varint(&rec[hdr_pos..hdr_size]).ok()?;
        hdr_pos += used;
        cnt += 1;
        if cnt > want {
            return None;
        }
    }
    if cnt != want {
        return None;
    }
    if parse_raw_record_prefix(rec, &mut f[..want]).ok()? != want {
        return None;
    }
    let k0 = match f[0] {
        RawField::Null => None,
        RawField::Int(v) => Some(v),
        _ => return None,
    };
    let (k1, rid_rf) = if n_keys == 2 {
        let k1 = match f[1] {
            RawField::Null => None,
            RawField::Int(v) => Some(v),
            _ => return None,
        };
        (k1, f[2])
    } else {
        (None, f[1])
    };
    let RawField::Int(rid) = rid_rf else {
        return None;
    };
    Some(PaxIndexEntry { k0, k1, rid })
}

#[inline]
pub fn values_as_pax_index_entry(vals: &[Value], n_keys: u8) -> Option<PaxIndexEntry> {
    let want = usize::from(n_keys) + 1;
    if vals.len() != want {
        return None;
    }
    let k0 = match &vals[0] {
        Value::Null => None,
        Value::Integer(v) => Some(*v),
        _ => return None,
    };
    let (k1, rid_v) = if n_keys == 2 {
        let k1 = match &vals[1] {
            Value::Null => None,
            Value::Integer(v) => Some(*v),
            _ => return None,
        };
        (k1, &vals[2])
    } else {
        (None, &vals[1])
    };
    let Value::Integer(rid) = rid_v else {
        return None;
    };
    Some(PaxIndexEntry { k0, k1, rid: *rid })
}

#[inline]
pub fn value_refs_as_pax_index_entry(vals: &[&Value], n_keys: u8) -> Option<PaxIndexEntry> {
    let want = usize::from(n_keys) + 1;
    if vals.len() != want {
        return None;
    }
    let k0 = match vals[0] {
        Value::Null => None,
        Value::Integer(v) => Some(*v),
        _ => return None,
    };
    let (k1, rid_v) = if n_keys == 2 {
        let k1 = match vals[1] {
            Value::Null => None,
            Value::Integer(v) => Some(*v),
            _ => return None,
        };
        (k1, vals[2])
    } else {
        (None, vals[1])
    };
    let Value::Integer(rid) = rid_v else {
        return None;
    };
    Some(PaxIndexEntry { k0, k1, rid: *rid })
}

/// Encodes a `(k0, [k1], rid)` entry into a canonical Format-3 index record inside a 32-byte stack
/// buffer with zero heap allocations!
#[inline]
pub fn encode_pax_index_entry_stack(n_keys: u8, entry: PaxIndexEntry, buf: &mut [u8; 32]) -> usize {
    #[inline(always)]
    fn write_int_st_and_body(v: Option<i64>, body: &mut [u8]) -> (u8, usize) {
        let Some(i) = v else {
            return (0, 0);
        };
        if i == 0 {
            return (8, 0);
        }
        if i == 1 {
            return (9, 0);
        }
        let u = if i < 0 { (!i) as u64 } else { i as u64 };
        if u <= 127 {
            body[0] = i as u8;
            (1, 1)
        } else if u <= 32_767 {
            body[0..2].copy_from_slice(&(i as i16).to_be_bytes());
            (2, 2)
        } else if u <= 8_388_607 {
            let b = (i as i32).to_be_bytes();
            body[0..3].copy_from_slice(&b[1..4]);
            (3, 3)
        } else if u <= 2_147_483_647 {
            body[0..4].copy_from_slice(&(i as i32).to_be_bytes());
            (4, 4)
        } else if u <= 140_737_488_355_327 {
            let b = i.to_be_bytes();
            body[0..6].copy_from_slice(&b[2..8]);
            (5, 6)
        } else {
            body[0..8].copy_from_slice(&i.to_be_bytes());
            (6, 8)
        }
    }

    if n_keys == 1 {
        buf[0] = 3;
        let mut bpos = 3usize;
        let (st0, l0) = write_int_st_and_body(entry.k0, &mut buf[bpos..]);
        bpos += l0;
        let (st1, l1) = write_int_st_and_body(Some(entry.rid), &mut buf[bpos..]);
        bpos += l1;
        buf[1] = st0;
        buf[2] = st1;
        bpos
    } else {
        buf[0] = 4;
        let mut bpos = 4usize;
        let (st0, l0) = write_int_st_and_body(entry.k0, &mut buf[bpos..]);
        bpos += l0;
        let (st1, l1) = write_int_st_and_body(entry.k1, &mut buf[bpos..]);
        bpos += l1;
        let (st2, l2) = write_int_st_and_body(Some(entry.rid), &mut buf[bpos..]);
        bpos += l2;
        buf[1] = st0;
        buf[2] = st1;
        buf[3] = st2;
        bpos
    }
}

/// Borrowed view of a `0x2a` PAX / SoA covering secondary `IndexLeaf` (`pgno >= 2`).
#[derive(Clone, Copy, Debug)]
pub struct PaxIndexLeafView<'a> {
    pgno: Pgno,
    usable: usize,
    raw: &'a [u8],
    n_keys: u8,
    n_rows: usize,
    widths: u8,
    null_flags: u8,
    cap: usize,
    layout: ComputedIndexLayout,
}

impl<'a> PaxIndexLeafView<'a> {
    #[inline]
    pub fn new(pgno: Pgno, usable_u32: u32, raw: &'a [u8]) -> Result<Self> {
        let usable = usable_u32 as usize;
        if pgno < 2 || usable < PAX_MIN_USABLE_SIZE || raw.len() < usable {
            return Err(SqliteError::corrupt(format!(
                "invalid PAX index leaf geometry on page {pgno}"
            )));
        }
        if raw[0] != PAX_INDEX_LEAF_BYTE || raw[1] != PAX_VERSION {
            return Err(SqliteError::corrupt(format!(
                "invalid PAX index leaf header on page {pgno}"
            )));
        }
        let n_keys = raw[2];
        let n_rows = usize::from(u16::from_be_bytes([raw[3], raw[4]]));
        let widths = raw[5];
        let null_flags = raw[6];
        let cap = usize::from(u16::from_le_bytes([raw[8], raw[9]]));
        let k0_off = usize::from(u16::from_le_bytes([raw[10], raw[11]]));
        let k1_off = usize::from(u16::from_le_bytes([raw[12], raw[13]]));
        let rid_off = usize::from(u16::from_le_bytes([raw[14], raw[15]]));

        let Some(layout) = compute_pax_index_layout(n_keys, widths, null_flags, cap) else {
            return Err(SqliteError::corrupt(format!(
                "corrupt PAX index layout header on page {pgno}"
            )));
        };
        if n_rows > cap
            || layout.end_off > usable
            || layout.k0_off != k0_off
            || layout.k1_off != k1_off
            || layout.rid_off != rid_off
        {
            return Err(SqliteError::corrupt(format!(
                "corrupt PAX index offsets on page {pgno}"
            )));
        }

        Ok(Self {
            pgno,
            usable,
            raw,
            n_keys,
            n_rows,
            widths,
            null_flags,
            cap,
            layout,
        })
    }

    #[inline(always)]
    pub const fn pgno(&self) -> Pgno {
        self.pgno
    }

    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.n_rows
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.n_rows == 0
    }

    #[inline(always)]
    pub const fn n_keys(&self) -> u8 {
        self.n_keys
    }

    #[inline(always)]
    pub const fn cap(&self) -> usize {
        self.cap
    }

    pub fn payload_and_unused_bytes(&self) -> (u64, u64) {
        let entry_bytes = ((if (self.widths & 0x03) == IDX_W_I64 {
            8
        } else {
            4
        }) + (if self.n_keys == 2 {
            if ((self.widths >> 2) & 0x03) == IDX_W_I64 {
                8
            } else {
                4
            }
        } else {
            0
        }) + (if ((self.widths >> 4) & 0x03) == IDX_W_I64 {
            8
        } else {
            4
        })) as u64;
        let payload = (self.n_rows as u64) * entry_bytes;
        let unused = ((self.cap - self.n_rows) as u64) * entry_bytes
            + (self.usable - self.layout.end_off) as u64;
        (payload, unused)
    }

    pub fn dbstat_metrics(&self) -> (i64, i64, i64) {
        let (payload, unused) = self.payload_and_unused_bytes();
        let mx = if self.n_rows == 0 {
            0
        } else {
            (payload / self.n_rows as u64) as i64
        };
        (payload as i64, unused as i64, mx)
    }

    #[inline(always)]
    pub fn k0_opt(&self, idx: usize) -> Option<i64> {
        if (self.null_flags & IDX_FLAG_K0_NULLS) != 0
            && !bitmap_get(self.raw, self.layout.k0_bmap_off, idx)
        {
            return None;
        }
        if (self.widths & 0x03) == IDX_W_I32 {
            let o = self.layout.k0_off + 4 * idx;
            Some(i64::from(i32::from_le_bytes([
                self.raw[o],
                self.raw[o + 1],
                self.raw[o + 2],
                self.raw[o + 3],
            ])))
        } else {
            let o = self.layout.k0_off + 8 * idx;
            let s = &self.raw[o..o + 8];
            Some(i64::from_le_bytes([
                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
            ]))
        }
    }

    #[inline(always)]
    pub fn k1_opt(&self, idx: usize) -> Option<i64> {
        if self.n_keys < 2 {
            return None;
        }
        if (self.null_flags & IDX_FLAG_K1_NULLS) != 0
            && !bitmap_get(self.raw, self.layout.k1_bmap_off, idx)
        {
            return None;
        }
        if ((self.widths >> 2) & 0x03) == IDX_W_I32 {
            let o = self.layout.k1_off + 4 * idx;
            Some(i64::from(i32::from_le_bytes([
                self.raw[o],
                self.raw[o + 1],
                self.raw[o + 2],
                self.raw[o + 3],
            ])))
        } else {
            let o = self.layout.k1_off + 8 * idx;
            let s = &self.raw[o..o + 8];
            Some(i64::from_le_bytes([
                s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
            ]))
        }
    }

    #[inline(always)]
    pub fn rid(&self, idx: usize) -> i64 {
        if ((self.widths >> 4) & 0x03) == IDX_W_I32 {
            let o = self.layout.rid_off + 4 * idx;
            i64::from(i32::from_le_bytes([
                self.raw[o],
                self.raw[o + 1],
                self.raw[o + 2],
                self.raw[o + 3],
            ]))
        } else {
            let o = self.layout.rid_off + 8 * idx;
            let s = &self.raw[o..o + 8];
            i64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
        }
    }

    #[inline(always)]
    pub fn entry_at(&self, idx: usize) -> PaxIndexEntry {
        PaxIndexEntry {
            k0: self.k0_opt(idx),
            k1: self.k1_opt(idx),
            rid: self.rid(idx),
        }
    }

    #[inline(always)]
    pub fn k0_i32_nonnull_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if (self.widths & 0x03) == IDX_W_I32 && (self.null_flags & IDX_FLAG_K0_NULLS) == 0 {
            Some(&self.raw[self.layout.k0_off + 4 * start..self.layout.k0_off + 4 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn k0_i64_nonnull_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if (self.widths & 0x03) == IDX_W_I64 && (self.null_flags & IDX_FLAG_K0_NULLS) == 0 {
            Some(&self.raw[self.layout.k0_off + 8 * start..self.layout.k0_off + 8 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn k1_i32_nonnull_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if self.n_keys == 2
            && ((self.widths >> 2) & 0x03) == IDX_W_I32
            && (self.null_flags & IDX_FLAG_K1_NULLS) == 0
        {
            Some(&self.raw[self.layout.k1_off + 4 * start..self.layout.k1_off + 4 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn k1_i64_nonnull_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if self.n_keys == 2
            && ((self.widths >> 2) & 0x03) == IDX_W_I64
            && (self.null_flags & IDX_FLAG_K1_NULLS) == 0
        {
            Some(&self.raw[self.layout.k1_off + 8 * start..self.layout.k1_off + 8 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn rid_i32_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if ((self.widths >> 4) & 0x03) == IDX_W_I32 {
            Some(&self.raw[self.layout.rid_off + 4 * start..self.layout.rid_off + 4 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn rid_i64_le_bytes(&self, start: usize, end: usize) -> Option<&'a [u8]> {
        if ((self.widths >> 4) & 0x03) == IDX_W_I64 {
            Some(&self.raw[self.layout.rid_off + 8 * start..self.layout.rid_off + 8 * end])
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn raw_field_at(&self, idx: usize, col_idx: usize) -> Option<RawField<'static>> {
        let ncols = usize::from(self.n_keys) + 1;
        if col_idx >= ncols {
            return None;
        }
        if col_idx == 0 {
            Some(match self.k0_opt(idx) {
                None => RawField::Null,
                Some(v) => RawField::Int(v),
            })
        } else if self.n_keys == 2 && col_idx == 1 {
            Some(match self.k1_opt(idx) {
                None => RawField::Null,
                Some(v) => RawField::Int(v),
            })
        } else {
            Some(RawField::Int(self.rid(idx)))
        }
    }

    #[inline]
    pub fn encode_row_stack(&self, idx: usize, buf: &mut [u8; 32]) -> usize {
        encode_pax_index_entry_stack(self.n_keys, self.entry_at(idx), buf)
    }

    #[inline]
    pub fn encode_row_vec(&self, idx: usize) -> Vec<u8> {
        let mut buf = [0u8; 32];
        let n = self.encode_row_stack(idx, &mut buf);
        buf[..n].to_vec()
    }

    #[inline]
    pub fn binary_search_entry(&self, target: PaxIndexEntry) -> std::result::Result<usize, usize> {
        let mut lo = 0usize;
        let mut hi = self.n_rows;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.entry_at(mid).cmp(&target) {
                Ordering::Less => lo = mid + 1,
                Ordering::Greater => hi = mid,
                Ordering::Equal => return Ok(mid),
            }
        }
        Err(lo)
    }

    /// Compares entry `idx` against `rhs_values` under `orders` with zero record decoding!
    #[inline]
    pub fn compare_to_values(
        &self,
        idx: usize,
        rhs_values: &[Value],
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<Ordering> {
        let default_so = KeySortOrder::default();
        let ncols = usize::from(self.n_keys) + 1;
        let stop = rhs_values.len().min(ncols);
        for (col_i, rhs) in rhs_values.iter().enumerate().take(stop) {
            let rf = self.raw_field_at(idx, col_i).unwrap_or(RawField::Null);
            let so = orders.get(col_i).unwrap_or(&default_so);
            let ord = rf.compare_key_to_value(rhs, so, enc)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
        }
        Ok(Ordering::Equal)
    }

    /// Compares entry `idx` against `(prefix_values, lower_bound, upper_bound)` with 100% identical
    /// SQLite cross-type and `NULL`-bound semantics to `compare_raw_record_range`, plus a pure
    /// integer fast path on common `Value::Integer` bounds!
    #[inline]
    pub fn compare_range_at(
        &self,
        idx: usize,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<Ordering> {
        let k = prefix_values.len();
        let has_bounds = lower_bound.is_some() || upper_bound.is_some();
        let ncols = usize::from(self.n_keys) + 1;
        let default_so = KeySortOrder::default();

        let stop = k.min(ncols);
        for (col_i, pv) in prefix_values.iter().enumerate().take(stop) {
            let rf = self.raw_field_at(idx, col_i).unwrap_or(RawField::Null);
            let so = orders.get(col_i).unwrap_or(&default_so);
            let ord = rf.compare_key_to_value(pv, so, enc)?;
            if ord != Ordering::Equal {
                return Ok(ord);
            }
        }
        if !has_bounds {
            return Ok(Ordering::Equal);
        }
        if k >= ncols {
            return Ok(Ordering::Less);
        }

        let f = self.raw_field_at(idx, k).unwrap_or(RawField::Null);
        let range_so = orders.get(k).unwrap_or(&default_so);
        if f.is_null() {
            return Ok(if range_so.nulls_last {
                Ordering::Greater
            } else {
                Ordering::Less
            });
        }
        if let Some((lo_val, lo_inc)) = lower_bound
            && !lo_val.is_null()
        {
            let cmp = f.compare_sql_to_nonnull_value(lo_val, range_so, enc)?;
            if cmp == Ordering::Less || (!lo_inc && cmp == Ordering::Equal) {
                return Ok(if range_so.desc {
                    Ordering::Greater
                } else {
                    Ordering::Less
                });
            }
        }
        if let Some((hi_val, hi_inc)) = upper_bound {
            if hi_val.is_null() {
                return Ok(Ordering::Greater);
            }
            let cmp = f.compare_sql_to_nonnull_value(hi_val, range_so, enc)?;
            if cmp == Ordering::Greater || (!hi_inc && cmp == Ordering::Equal) {
                return Ok(if range_so.desc {
                    Ordering::Less
                } else {
                    Ordering::Greater
                });
            }
        }
        Ok(Ordering::Equal)
    }

    #[inline]
    fn try_find_int_range_bounds(
        &self,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
    ) -> Option<(usize, usize, bool)> {
        if self.n_rows == 0 {
            return Some((0, 0, false));
        }
        let ncols = usize::from(self.n_keys) + 1;
        if orders
            .iter()
            .take(ncols)
            .any(|o| o.desc || o.nulls_last || o.collation != sqliters_types::Collation::Binary)
        {
            return None;
        }
        let k = prefix_values.len();
        if k > 1 {
            return None;
        }
        let p0 = if k == 1 {
            let Value::Integer(v) = prefix_values[0] else {
                return None;
            };
            v
        } else {
            0
        };
        let has_bounds = lower_bound.is_some() || upper_bound.is_some();
        if has_bounds && k >= ncols {
            return None;
        }
        let (has_lo, lo_inc_val, impossible_lo) = match lower_bound {
            None => (false, 0i64, false),
            Some((Value::Integer(lv), inc)) => {
                if inc {
                    (true, *lv, false)
                } else if *lv == i64::MAX {
                    (true, i64::MAX, true)
                } else {
                    (true, *lv + 1, false)
                }
            }
            Some(_) => return None,
        };
        let (has_hi, hi_inc_val, impossible_hi) = match upper_bound {
            None => (false, 0i64, false),
            Some((Value::Integer(hv), inc)) => {
                if inc {
                    (true, *hv, false)
                } else if *hv == i64::MIN {
                    (true, i64::MIN, true)
                } else {
                    (true, *hv - 1, false)
                }
            }
            Some(_) => return None,
        };

        let cmp_idx = |idx: usize| -> Ordering {
            if k == 1 {
                let Some(k0v) = self.k0_opt(idx) else {
                    return Ordering::Less;
                };
                match k0v.cmp(&p0) {
                    Ordering::Less => return Ordering::Less,
                    Ordering::Greater => return Ordering::Greater,
                    Ordering::Equal => {}
                }
            }
            if !has_bounds {
                return Ordering::Equal;
            }
            let r_opt = if k == 0 {
                self.k0_opt(idx)
            } else if self.n_keys == 2 {
                self.k1_opt(idx)
            } else {
                Some(self.rid(idx))
            };
            let Some(rv) = r_opt else {
                return Ordering::Less;
            };
            if has_lo && (impossible_lo || rv < lo_inc_val) {
                return Ordering::Less;
            }
            if has_hi && (impossible_hi || rv > hi_inc_val) {
                return Ordering::Greater;
            }
            Ordering::Equal
        };

        let n = self.n_rows;
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if cmp_idx(mid) == Ordering::Less {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == n {
            return Some((n, n, false));
        }
        match cmp_idx(n - 1) {
            Ordering::Equal => Some((lo, n, false)),
            Ordering::Greater => {
                let mut ulo = lo;
                let mut uhi = n - 1;
                while ulo < uhi {
                    let mid = ulo + (uhi - ulo) / 2;
                    if cmp_idx(mid) == Ordering::Greater {
                        uhi = mid;
                    } else {
                        ulo = mid + 1;
                    }
                }
                Some((lo, ulo, true))
            }
            Ordering::Less => Some((n, n, false)),
        }
    }

    /// Computes `(lo, ulo, hit_greater)` for `0..n_rows` using the pure-integer `0x2a` fast path
    /// when bounds are `Value::Integer` under default `ASC NULLS FIRST`, falling back to
    /// `compare_range_at` for cross-type (`Float`/`Text`/`Null`) bounds.
    #[inline]
    pub fn find_range_bounds(
        &self,
        prefix_values: &[Value],
        lower_bound: Option<(&Value, bool)>,
        upper_bound: Option<(&Value, bool)>,
        orders: &[KeySortOrder],
        enc: TextEncoding,
    ) -> Result<(usize, usize, bool)> {
        if let Some(res) =
            self.try_find_int_range_bounds(prefix_values, lower_bound, upper_bound, orders)
        {
            return Ok(res);
        }
        let n = self.n_rows;
        if n == 0 {
            return Ok((0, 0, false));
        }
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let ord =
                self.compare_range_at(mid, prefix_values, lower_bound, upper_bound, orders, enc)?;
            if ord == Ordering::Less {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == n {
            return Ok((n, n, false));
        }
        let last_ord =
            self.compare_range_at(n - 1, prefix_values, lower_bound, upper_bound, orders, enc)?;
        match last_ord {
            Ordering::Equal => Ok((lo, n, false)),
            Ordering::Greater => {
                let mut ulo = lo;
                let mut uhi = n - 1;
                while ulo < uhi {
                    let mid = ulo + (uhi - ulo) / 2;
                    let ord = self.compare_range_at(
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
                Ok((lo, ulo, true))
            }
            Ordering::Less => Ok((n, n, false)),
        }
    }
}

#[inline]
fn write_pax_index_slot(
    raw: &mut [u8],
    n_keys: u8,
    widths: u8,
    null_flags: u8,
    layout: &ComputedIndexLayout,
    idx: usize,
    entry: PaxIndexEntry,
) {
    match entry.k0 {
        None => {
            bitmap_set(raw, layout.k0_bmap_off, idx, false);
            if (widths & 0x03) == IDX_W_I32 {
                raw[layout.k0_off + 4 * idx..layout.k0_off + 4 * idx + 4].fill(0);
            } else {
                raw[layout.k0_off + 8 * idx..layout.k0_off + 8 * idx + 8].fill(0);
            }
        }
        Some(v) => {
            if (null_flags & IDX_FLAG_K0_NULLS) != 0 {
                bitmap_set(raw, layout.k0_bmap_off, idx, true);
            }
            if (widths & 0x03) == IDX_W_I32 {
                raw[layout.k0_off + 4 * idx..layout.k0_off + 4 * idx + 4]
                    .copy_from_slice(&(v as i32).to_le_bytes());
            } else {
                raw[layout.k0_off + 8 * idx..layout.k0_off + 8 * idx + 8]
                    .copy_from_slice(&v.to_le_bytes());
            }
        }
    }

    if n_keys == 2 {
        match entry.k1 {
            None => {
                bitmap_set(raw, layout.k1_bmap_off, idx, false);
                if ((widths >> 2) & 0x03) == IDX_W_I32 {
                    raw[layout.k1_off + 4 * idx..layout.k1_off + 4 * idx + 4].fill(0);
                } else {
                    raw[layout.k1_off + 8 * idx..layout.k1_off + 8 * idx + 8].fill(0);
                }
            }
            Some(v) => {
                if (null_flags & IDX_FLAG_K1_NULLS) != 0 {
                    bitmap_set(raw, layout.k1_bmap_off, idx, true);
                }
                if ((widths >> 2) & 0x03) == IDX_W_I32 {
                    raw[layout.k1_off + 4 * idx..layout.k1_off + 4 * idx + 4]
                        .copy_from_slice(&(v as i32).to_le_bytes());
                } else {
                    raw[layout.k1_off + 8 * idx..layout.k1_off + 8 * idx + 8]
                        .copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }

    if ((widths >> 4) & 0x03) == IDX_W_I32 {
        raw[layout.rid_off + 4 * idx..layout.rid_off + 4 * idx + 4]
            .copy_from_slice(&(entry.rid as i32).to_le_bytes());
    } else {
        raw[layout.rid_off + 8 * idx..layout.rid_off + 8 * idx + 8]
            .copy_from_slice(&entry.rid.to_le_bytes());
    }
}

#[inline]
fn write_pax_index_header(
    raw: &mut [u8],
    n_keys: u8,
    n_rows: usize,
    widths: u8,
    null_flags: u8,
    cap: usize,
    layout: &ComputedIndexLayout,
) {
    raw[0] = PAX_INDEX_LEAF_BYTE;
    raw[1] = PAX_VERSION;
    raw[2] = n_keys;
    raw[3..5].copy_from_slice(&(n_rows as u16).to_be_bytes());
    raw[5] = widths;
    raw[6] = null_flags;
    raw[7] = 0;
    raw[8..10].copy_from_slice(&(cap as u16).to_le_bytes());
    raw[10..12].copy_from_slice(&(layout.k0_off as u16).to_le_bytes());
    raw[12..14].copy_from_slice(&(layout.k1_off as u16).to_le_bytes());
    raw[14..16].copy_from_slice(&(layout.rid_off as u16).to_le_bytes());
}

/// Writes a sorted slice of `entries` (`1 <= entries.len() <= max_cap`) into `raw` as a `0x2a` PAX
/// index leaf (`pgno >= 2`), maximizing `cap` for future in-page inserts!
pub fn write_pax_index_leaf_slice(
    usable_u32: u32,
    raw: &mut [u8],
    n_keys: u8,
    entries: &[PaxIndexEntry],
) {
    let mut widths = if n_keys == 2 {
        IDX_W_I32 | (IDX_W_I32 << 2) | (IDX_W_I32 << 4)
    } else {
        IDX_W_I32 | (IDX_W_I32 << 4)
    };
    let mut null_flags = 0u8;
    for &e in entries {
        let (ew, enf) = e.required_widths_and_flags(n_keys);
        widths = merge_index_widths(widths, ew, n_keys);
        null_flags |= enf;
    }
    let cap = max_pax_index_cap_for_widths(usable_u32, n_keys, widths, null_flags);
    assert!(entries.len() <= cap, "PAX index slice exceeds leaf cap");
    let layout = compute_pax_index_layout(n_keys, widths, null_flags, cap).expect("valid layout");
    raw[..layout.end_off].fill(0);
    for (i, &e) in entries.iter().enumerate() {
        write_pax_index_slot(raw, n_keys, widths, null_flags, &layout, i, e);
    }
    write_pax_index_header(raw, n_keys, entries.len(), widths, null_flags, cap, &layout);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaxIndexInsertOutcome {
    Inserted,
    Replaced,
    NeedsSplit,
    EscapeToSlotted,
}

/// Inserts `entry` into a `0x2a` PAX index leaf (or upgrades an empty `0x0a` leaf to `0x2a`!).
pub fn try_insert_pax_index_leaf(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    n_keys: u8,
    entry: PaxIndexEntry,
) -> Result<PaxIndexInsertOutcome> {
    let usable = usable_u32 as usize;
    if pgno < 2 || usable < PAX_MIN_USABLE_SIZE {
        return Ok(PaxIndexInsertOutcome::EscapeToSlotted);
    }

    if raw[0] == BtreePageType::IndexLeaf as u8 {
        let n_cell = u16::from_be_bytes([raw[3], raw[4]]);
        if n_cell == 0 {
            write_pax_index_leaf_slice(usable_u32, raw, n_keys, &[entry]);
            return Ok(PaxIndexInsertOutcome::Inserted);
        }
        return Ok(PaxIndexInsertOutcome::EscapeToSlotted);
    }

    let (n_rows, cur_n_keys, widths, null_flags, cap, layout, pos) = {
        let view = PaxIndexLeafView::new(pgno, usable_u32, raw)?;
        if view.n_keys != n_keys {
            return Ok(PaxIndexInsertOutcome::EscapeToSlotted);
        }
        let pos = if view.n_rows == 0 {
            Err(0)
        } else {
            let last_e = view.entry_at(view.n_rows - 1);
            match last_e.cmp(&entry) {
                Ordering::Less => Err(view.n_rows),
                Ordering::Equal => Ok(view.n_rows - 1),
                Ordering::Greater => view.binary_search_entry(entry),
            }
        };
        (
            view.n_rows,
            view.n_keys,
            view.widths,
            view.null_flags,
            view.cap,
            view.layout,
            pos,
        )
    };
    let _ = cur_n_keys;

    if pos.is_ok() {
        return Ok(PaxIndexInsertOutcome::Replaced);
    }
    let ins_idx = pos.unwrap_err();
    let (req_w, req_nf) = entry.required_widths_and_flags(n_keys);
    let merged_w = merge_index_widths(widths, req_w, n_keys);
    let merged_nf = null_flags | req_nf;

    if merged_w == widths && merged_nf == null_flags && n_rows < cap {
        if ins_idx < n_rows {
            let k0_stride = if (widths & 0x03) == IDX_W_I64 { 8 } else { 4 };
            raw.copy_within(
                layout.k0_off + k0_stride * ins_idx..layout.k0_off + k0_stride * n_rows,
                layout.k0_off + k0_stride * (ins_idx + 1),
            );
            if (null_flags & IDX_FLAG_K0_NULLS) != 0 {
                shift_pax_bitmap_insert(raw, layout.k0_bmap_off, n_rows, ins_idx, false);
            }
            if n_keys == 2 {
                let k1_stride = if ((widths >> 2) & 0x03) == IDX_W_I64 {
                    8
                } else {
                    4
                };
                raw.copy_within(
                    layout.k1_off + k1_stride * ins_idx..layout.k1_off + k1_stride * n_rows,
                    layout.k1_off + k1_stride * (ins_idx + 1),
                );
                if (null_flags & IDX_FLAG_K1_NULLS) != 0 {
                    shift_pax_bitmap_insert(raw, layout.k1_bmap_off, n_rows, ins_idx, false);
                }
            }
            let rid_stride = if ((widths >> 4) & 0x03) == IDX_W_I64 {
                8
            } else {
                4
            };
            raw.copy_within(
                layout.rid_off + rid_stride * ins_idx..layout.rid_off + rid_stride * n_rows,
                layout.rid_off + rid_stride * (ins_idx + 1),
            );
        }
        write_pax_index_slot(raw, n_keys, widths, null_flags, &layout, ins_idx, entry);
        raw[3..5].copy_from_slice(&((n_rows + 1) as u16).to_be_bytes());
        return Ok(PaxIndexInsertOutcome::Inserted);
    }

    // Width widening (`I32 -> I64`) or `HAS_NULLS` bitmap addition!
    let new_cap = max_pax_index_cap_for_widths(usable_u32, n_keys, merged_w, merged_nf);
    if n_rows + 1 > new_cap {
        return Ok(PaxIndexInsertOutcome::NeedsSplit);
    }

    let mut all = Vec::with_capacity(n_rows + 1);
    {
        let view = PaxIndexLeafView::new(pgno, usable_u32, raw)?;
        for i in 0..n_rows {
            if i == ins_idx {
                all.push(entry);
            }
            all.push(view.entry_at(i));
        }
        if ins_idx == n_rows {
            all.push(entry);
        }
    }
    write_pax_index_leaf_slice(usable_u32, raw, n_keys, &all);
    Ok(PaxIndexInsertOutcome::Inserted)
}

/// Splits a full `0x2a` PAX index leaf (`raw`) + `entry` into left `0x2a` (`raw`), promoted middle
/// `IndexLeafCell` divider, and right `0x2a` (`right_raw`).
pub fn split_pax_index_leaf(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    right_raw: &mut [u8],
    n_keys: u8,
    entry: PaxIndexEntry,
) -> Result<IndexLeafCell> {
    let mut all = {
        let view = PaxIndexLeafView::new(pgno, usable_u32, raw)?;
        let mut v = Vec::with_capacity(view.n_rows + 1);
        for i in 0..view.n_rows {
            v.push(view.entry_at(i));
        }
        v
    };
    match all.binary_search(&entry) {
        Ok(pos) => all[pos] = entry,
        Err(pos) => all.insert(pos, entry),
    }
    let mid = all.len() / 2;
    let promoted_entry = all[mid];
    write_pax_index_leaf_slice(usable_u32, raw, n_keys, &all[..mid]);
    write_pax_index_leaf_slice(usable_u32, right_raw, n_keys, &all[mid + 1..]);

    let mut sbuf = [0u8; 32];
    let slen = encode_pax_index_entry_stack(n_keys, promoted_entry, &mut sbuf);
    Ok(IndexLeafCell {
        total_payload_len: slen as u32,
        local_payload: sbuf[..slen].to_vec(),
        overflow_page: None,
    })
}

/// Deletes `entry` in-place from a `0x2a` PAX index leaf (`pgno >= 2`):
/// - `Ok(Some(false))`: `entry` is absent on this leaf.
/// - `Ok(Some(true))`: `entry` deleted in place.
/// - `Ok(None)`: `n_rows == 1 && !is_root` (non-root underflow -> caller handles structural delete).
pub fn delete_pax_index_leaf_entry(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
    entry: PaxIndexEntry,
    is_root: bool,
) -> Result<Option<bool>> {
    let (idx, n_rows, n_keys, widths, null_flags, layout) = {
        let view = PaxIndexLeafView::new(pgno, usable_u32, raw)?;
        if view.n_rows == 0 {
            return Ok(Some(false));
        }
        let Ok(idx) = view.binary_search_entry(entry) else {
            return Ok(Some(false));
        };
        if view.n_rows == 1 && !is_root {
            return Ok(None);
        }
        (
            idx,
            view.n_rows,
            view.n_keys,
            view.widths,
            view.null_flags,
            view.layout,
        )
    };

    if n_rows == 1 {
        reset_empty_slotted_leaf(usable_u32, raw, BtreePageType::IndexLeaf);
        return Ok(Some(true));
    }

    if idx + 1 < n_rows {
        let k0_stride = if (widths & 0x03) == IDX_W_I64 { 8 } else { 4 };
        raw.copy_within(
            layout.k0_off + k0_stride * (idx + 1)..layout.k0_off + k0_stride * n_rows,
            layout.k0_off + k0_stride * idx,
        );
        if (null_flags & IDX_FLAG_K0_NULLS) != 0 {
            shift_pax_bitmap_delete(raw, layout.k0_bmap_off, n_rows, idx);
        }
        if n_keys == 2 {
            let k1_stride = if ((widths >> 2) & 0x03) == IDX_W_I64 {
                8
            } else {
                4
            };
            raw.copy_within(
                layout.k1_off + k1_stride * (idx + 1)..layout.k1_off + k1_stride * n_rows,
                layout.k1_off + k1_stride * idx,
            );
            if (null_flags & IDX_FLAG_K1_NULLS) != 0 {
                shift_pax_bitmap_delete(raw, layout.k1_bmap_off, n_rows, idx);
            }
        }
        let rid_stride = if ((widths >> 4) & 0x03) == IDX_W_I64 {
            8
        } else {
            4
        };
        raw.copy_within(
            layout.rid_off + rid_stride * (idx + 1)..layout.rid_off + rid_stride * n_rows,
            layout.rid_off + rid_stride * idx,
        );
    }
    raw[3..5].copy_from_slice(&((n_rows - 1) as u16).to_be_bytes());
    Ok(Some(true))
}

/// Pops the rightmost (`n_rows - 1`) entry from a `0x2a` PAX index leaf when `n_rows >= 2`,
/// returning its canonical Format-3 record bytes (used when replacing an `0x02` `IndexInterior`
/// divider cell with its in-order predecessor!).
pub fn pop_last_pax_index_leaf_as_record(
    pgno: Pgno,
    usable_u32: u32,
    raw: &mut [u8],
) -> Result<Option<Vec<u8>>> {
    let rec = {
        let view = PaxIndexLeafView::new(pgno, usable_u32, raw)?;
        if view.n_rows < 2 {
            return Ok(None);
        }
        let rec = view.encode_row_vec(view.n_rows - 1);
        let new_n = (view.n_rows - 1) as u16;
        raw[3..5].copy_from_slice(&new_n.to_be_bytes());
        rec
    };
    Ok(Some(rec))
}

pub fn pax_index_leaf_to_cells(pgno: Pgno, usable: u32, raw: &[u8]) -> Result<Vec<IndexLeafCell>> {
    let view = PaxIndexLeafView::new(pgno, usable, raw)?;
    let mut cells = Vec::with_capacity(view.n_rows);
    let mut sbuf = [0u8; 32];
    for i in 0..view.n_rows {
        let n = view.encode_row_stack(i, &mut sbuf);
        cells.push(IndexLeafCell {
            total_payload_len: n as u32,
            local_payload: sbuf[..n].to_vec(),
            overflow_page: None,
        });
    }
    Ok(cells)
}

pub fn validate_pax_index_leaf(pgno: Pgno, usable: u32, raw: &[u8], is_root: bool) -> Result<()> {
    let view = PaxIndexLeafView::new(pgno, usable, raw)?;
    if !is_root && view.n_rows == 0 {
        return Err(SqliteError::corrupt(format!(
            "non-root PAX index leaf {pgno} has 0 entries"
        )));
    }
    if raw[7] != 0 {
        return Err(SqliteError::corrupt(format!(
            "non-zero reserved flags on PAX index leaf {pgno}"
        )));
    }
    let valid_nf = if view.n_keys == 2 {
        IDX_FLAG_K0_NULLS | IDX_FLAG_K1_NULLS
    } else {
        IDX_FLAG_K0_NULLS
    };
    if (view.null_flags & !valid_nf) != 0 {
        return Err(SqliteError::corrupt(format!(
            "invalid null_flags on PAX index leaf {pgno}"
        )));
    }
    let mut prev: Option<PaxIndexEntry> = None;
    for i in 0..view.n_rows {
        let cur = view.entry_at(i);
        if let Some(p) = prev
            && cur <= p
        {
            return Err(SqliteError::corrupt(format!(
                "non-monotonic entries on PAX index leaf {pgno}"
            )));
        }
        prev = Some(cur);
    }
    let _ = encode_record_into;
    Ok(())
}
