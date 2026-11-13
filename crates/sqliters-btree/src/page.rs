//! On-disk slotted B-tree page layout, payload fraction bounds, and cell codecs
//! ported from `sqlite/src/btreeInt.h` and `sqlite/src/btree.c`.

use sqliters_pager::DB_HEADER_SIZE;
use sqliters_types::{
    ErrorCode, KeySortOrder, Pgno, RawRecordKey, Result, SqliteError, TextEncoding, Value,
    compare_records_with_encoding, get_varint, get_varint_i64, push_varint, varint_len,
};

/// On-disk B-tree page flag byte (`PTF_*` in `btreeInt.h` / `decodeFlags` in `btree.c:2055`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BtreePageType {
    /// `0x02` (`PTF_ZERODATA`): Index B-Tree interior page (12-byte header).
    IndexInterior = 0x02,
    /// `0x05` (`PTF_LEAFDATA | PTF_INTKEY`): Table B+Tree interior page (12-byte header).
    TableInterior = 0x05,
    /// `0x0a` (`PTF_ZERODATA | PTF_LEAF`): Index B-Tree leaf page (8-byte header).
    IndexLeaf = 0x0a,
    /// `0x0d` (`PTF_LEAFDATA | PTF_INTKEY | PTF_LEAF`): Table B+Tree leaf page (8-byte header).
    TableLeaf = 0x0d,
}

impl BtreePageType {
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            0x02 => Ok(Self::IndexInterior),
            0x05 => Ok(Self::TableInterior),
            0x0a => Ok(Self::IndexLeaf),
            0x0d => Ok(Self::TableLeaf),
            other => Err(SqliteError::corrupt(format!(
                "invalid btree page flags byte: {other:#04x}"
            ))),
        }
    }

    #[inline]
    pub const fn is_leaf(self) -> bool {
        matches!(self, Self::TableLeaf | Self::IndexLeaf)
    }

    #[inline]
    pub const fn header_size(self) -> usize {
        if self.is_leaf() { 8 } else { 12 }
    }
}

/// Computes local vs overflow payload thresholds (`btree.c:3471-3474` and `1465-1472`).
#[derive(Debug, Clone, Copy)]
pub struct PayloadBounds {
    pub usable_size: u32,
    pub max_local: u32,
    pub min_local: u32,
}

impl PayloadBounds {
    #[inline]
    pub const fn for_table_leaf(usable_size: u32) -> Self {
        let max_local = usable_size - 35;
        let min_local = ((usable_size - 12) * 32 / 255) - 23;
        Self {
            usable_size,
            max_local,
            min_local,
        }
    }

    #[inline]
    pub const fn for_index(usable_size: u32) -> Self {
        let max_local = ((usable_size - 12) * 64 / 255) - 23;
        let min_local = ((usable_size - 12) * 32 / 255) - 23;
        Self {
            usable_size,
            max_local,
            min_local,
        }
    }

    /// Computes how many payload bytes reside locally in the cell vs on overflow pages.
    #[inline]
    pub const fn local_len(&self, total_payload_len: u32) -> usize {
        if total_payload_len <= self.max_local {
            total_payload_len as usize
        } else {
            let mut k =
                self.min_local + (total_payload_len - self.min_local) % (self.usable_size - 4);
            if k > self.max_local {
                k = self.min_local;
            }
            k as usize
        }
    }
}

#[derive(Debug, Clone)]
pub struct TableLeafCell {
    pub rowid: i64,
    pub total_payload_len: u32,
    pub local_payload: Vec<u8>,
    pub overflow_page: Option<Pgno>,
}

impl TableLeafCell {
    pub fn encoded_size(&self) -> usize {
        let raw = varint_len(u64::from(self.total_payload_len))
            + varint_len(self.rowid as u64)
            + self.local_payload.len()
            + if self.overflow_page.is_some() { 4 } else { 0 };
        raw.max(4)
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let start = out.len();
        push_varint(out, u64::from(self.total_payload_len));
        push_varint(out, self.rowid as u64);
        out.extend_from_slice(&self.local_payload);
        if let Some(ovfl) = self.overflow_page {
            out.extend_from_slice(&ovfl.to_be_bytes());
        }
        while out.len() - start < 4 {
            out.push(0);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TableInteriorCell {
    pub left_child: Pgno,
    pub key: i64,
}

impl TableInteriorCell {
    pub fn encoded_size(&self) -> usize {
        (4 + varint_len(self.key as u64)).max(4)
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.left_child.to_be_bytes());
        push_varint(out, self.key as u64);
    }
}

#[derive(Debug, Clone)]
pub struct IndexLeafCell {
    pub total_payload_len: u32,
    pub local_payload: Vec<u8>,
    pub overflow_page: Option<Pgno>,
}

impl IndexLeafCell {
    pub fn encoded_size(&self) -> usize {
        let raw = varint_len(u64::from(self.total_payload_len))
            + self.local_payload.len()
            + if self.overflow_page.is_some() { 4 } else { 0 };
        raw.max(4)
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let start = out.len();
        push_varint(out, u64::from(self.total_payload_len));
        out.extend_from_slice(&self.local_payload);
        if let Some(ovfl) = self.overflow_page {
            out.extend_from_slice(&ovfl.to_be_bytes());
        }
        while out.len() - start < 4 {
            out.push(0);
        }
    }
}

#[derive(Debug, Clone)]
pub struct IndexInteriorCell {
    pub left_child: Pgno,
    pub total_payload_len: u32,
    pub local_payload: Vec<u8>,
    pub overflow_page: Option<Pgno>,
}

impl IndexInteriorCell {
    pub fn encoded_size(&self) -> usize {
        let raw = 4
            + varint_len(u64::from(self.total_payload_len))
            + self.local_payload.len()
            + if self.overflow_page.is_some() { 4 } else { 0 };
        raw.max(4)
    }

    pub fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.left_child.to_be_bytes());
        push_varint(out, u64::from(self.total_payload_len));
        out.extend_from_slice(&self.local_payload);
        if let Some(ovfl) = self.overflow_page {
            out.extend_from_slice(&ovfl.to_be_bytes());
        }
    }
}

/// High-level parsed representation of any of the 4 SQLite B-tree page kinds.
#[derive(Debug, Clone)]
pub enum ParsedPage {
    TableLeaf {
        cells: Vec<TableLeafCell>,
    },
    TableInterior {
        cells: Vec<TableInteriorCell>,
        right_child: Pgno,
    },
    IndexLeaf {
        cells: Vec<IndexLeafCell>,
    },
    IndexInterior {
        cells: Vec<IndexInteriorCell>,
        right_child: Pgno,
    },
}

impl ParsedPage {
    pub const fn page_type(&self) -> BtreePageType {
        match self {
            Self::TableLeaf { .. } => BtreePageType::TableLeaf,
            Self::TableInterior { .. } => BtreePageType::TableInterior,
            Self::IndexLeaf { .. } => BtreePageType::IndexLeaf,
            Self::IndexInterior { .. } => BtreePageType::IndexInterior,
        }
    }

    pub fn cell_count(&self) -> usize {
        match self {
            Self::TableLeaf { cells } => cells.len(),
            Self::TableInterior { cells, .. } => cells.len(),
            Self::IndexLeaf { cells } => cells.len(),
            Self::IndexInterior { cells, .. } => cells.len(),
        }
    }

    /// Computes the exact byte footprint required on a page with header offset `hdr_off` (100 on Page 1, 0 otherwise).
    pub fn required_bytes(&self, hdr_off: usize) -> usize {
        let hdr_len = self.page_type().header_size();
        let ptr_array_len = 2 * self.cell_count();
        let cells_bytes: usize = match self {
            Self::TableLeaf { cells } => cells.iter().map(TableLeafCell::encoded_size).sum(),
            Self::TableInterior { cells, .. } => {
                cells.iter().map(TableInteriorCell::encoded_size).sum()
            }
            Self::IndexLeaf { cells } => cells.iter().map(IndexLeafCell::encoded_size).sum(),
            Self::IndexInterior { cells, .. } => {
                cells.iter().map(IndexInteriorCell::encoded_size).sum()
            }
        };
        hdr_off + hdr_len + ptr_array_len + cells_bytes
    }

    pub fn fits_in_page(&self, pgno: Pgno, usable_size: u32) -> bool {
        let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
        self.required_bytes(hdr_off) <= (usable_size as usize)
    }

    /// Parses an on-disk SQLite B-tree page (handling arbitrary cell pointer ordering, freeblocks, and fragments).
    pub fn parse(pgno: Pgno, usable_size: u32, raw: &[u8]) -> Result<Self> {
        let usable = usable_size as usize;
        if raw.len() < usable {
            return Err(SqliteError::corrupt(
                "raw page buffer smaller than usable_size",
            ));
        }
        let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
        let page_type = BtreePageType::from_byte(raw[hdr_off])?;
        let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
        let hdr_size = page_type.header_size();
        let ptr_base = hdr_off + hdr_size;
        let ptr_end = ptr_base
            .checked_add(2 * n_cell)
            .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
        if ptr_end > usable {
            return Err(SqliteError::corrupt(format!(
                "cell pointer array ({ptr_end}) exceeds usable page size ({usable}) on page {pgno}"
            )));
        }

        let right_child = if page_type.is_leaf() {
            0
        } else {
            u32::from_be_bytes([
                raw[hdr_off + 8],
                raw[hdr_off + 9],
                raw[hdr_off + 10],
                raw[hdr_off + 11],
            ])
        };

        let cell_offset_at = |idx: usize| -> Result<usize> {
            let p = ptr_base + 2 * idx;
            let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
            if off < ptr_end || off >= usable {
                return Err(SqliteError::corrupt(format!(
                    "invalid cell offset {off} on page {pgno}"
                )));
            }
            Ok(off)
        };

        match page_type {
            BtreePageType::TableLeaf => {
                let bounds = PayloadBounds::for_table_leaf(usable_size);
                let mut cells = Vec::with_capacity(n_cell);
                for i in 0..n_cell {
                    let off = cell_offset_at(i)?;
                    let slice = &raw[off..usable];
                    let (payload_u64, n1) = get_varint(slice)?;
                    let total_payload_len = u32::try_from(payload_u64)
                        .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
                    let (rowid, n2) = get_varint_i64(&slice[n1..])?;
                    let body_start = n1 + n2;
                    let local_len = bounds.local_len(total_payload_len);
                    let body_end = body_start + local_len;
                    let has_ovfl = (local_len as u32) < total_payload_len;
                    let needed = body_end + if has_ovfl { 4 } else { 0 };
                    if needed > slice.len() {
                        return Err(SqliteError::corrupt(format!(
                            "table leaf cell extends past end of page {pgno}"
                        )));
                    }
                    let local_payload = slice[body_start..body_end].to_vec();
                    let overflow_page = if has_ovfl {
                        let ovfl = u32::from_be_bytes([
                            slice[body_end],
                            slice[body_end + 1],
                            slice[body_end + 2],
                            slice[body_end + 3],
                        ]);
                        Some(ovfl)
                    } else {
                        None
                    };
                    cells.push(TableLeafCell {
                        rowid,
                        total_payload_len,
                        local_payload,
                        overflow_page,
                    });
                }
                Ok(Self::TableLeaf { cells })
            }
            BtreePageType::TableInterior => {
                let mut cells = Vec::with_capacity(n_cell);
                for i in 0..n_cell {
                    let off = cell_offset_at(i)?;
                    let slice = &raw[off..usable];
                    if slice.len() < 5 {
                        return Err(SqliteError::corrupt("truncated table interior cell"));
                    }
                    let left_child = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
                    let (key, _) = get_varint_i64(&slice[4..])?;
                    cells.push(TableInteriorCell { left_child, key });
                }
                Ok(Self::TableInterior { cells, right_child })
            }
            BtreePageType::IndexLeaf => {
                let bounds = PayloadBounds::for_index(usable_size);
                let mut cells = Vec::with_capacity(n_cell);
                for i in 0..n_cell {
                    let off = cell_offset_at(i)?;
                    let slice = &raw[off..usable];
                    let (payload_u64, n1) = get_varint(slice)?;
                    let total_payload_len = u32::try_from(payload_u64)
                        .map_err(|_| SqliteError::corrupt("index cell payload > 4GiB"))?;
                    let local_len = bounds.local_len(total_payload_len);
                    let body_end = n1 + local_len;
                    let has_ovfl = (local_len as u32) < total_payload_len;
                    let needed = body_end + if has_ovfl { 4 } else { 0 };
                    if needed > slice.len() {
                        return Err(SqliteError::corrupt(format!(
                            "index leaf cell extends past end of page {pgno}"
                        )));
                    }
                    let local_payload = slice[n1..body_end].to_vec();
                    let overflow_page = if has_ovfl {
                        Some(u32::from_be_bytes([
                            slice[body_end],
                            slice[body_end + 1],
                            slice[body_end + 2],
                            slice[body_end + 3],
                        ]))
                    } else {
                        None
                    };
                    cells.push(IndexLeafCell {
                        total_payload_len,
                        local_payload,
                        overflow_page,
                    });
                }
                Ok(Self::IndexLeaf { cells })
            }
            BtreePageType::IndexInterior => {
                let bounds = PayloadBounds::for_index(usable_size);
                let mut cells = Vec::with_capacity(n_cell);
                for i in 0..n_cell {
                    let off = cell_offset_at(i)?;
                    let slice = &raw[off..usable];
                    if slice.len() < 5 {
                        return Err(SqliteError::corrupt("truncated index interior cell"));
                    }
                    let left_child = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
                    let (payload_u64, n1) = get_varint(&slice[4..])?;
                    let total_payload_len = u32::try_from(payload_u64)
                        .map_err(|_| SqliteError::corrupt("index interior payload > 4GiB"))?;
                    let body_start = 4 + n1;
                    let local_len = bounds.local_len(total_payload_len);
                    let body_end = body_start + local_len;
                    let has_ovfl = (local_len as u32) < total_payload_len;
                    let needed = body_end + if has_ovfl { 4 } else { 0 };
                    if needed > slice.len() {
                        return Err(SqliteError::corrupt(format!(
                            "index interior cell extends past end of page {pgno}"
                        )));
                    }
                    let local_payload = slice[body_start..body_end].to_vec();
                    let overflow_page = if has_ovfl {
                        Some(u32::from_be_bytes([
                            slice[body_end],
                            slice[body_end + 1],
                            slice[body_end + 2],
                            slice[body_end + 3],
                        ]))
                    } else {
                        None
                    };
                    cells.push(IndexInteriorCell {
                        left_child,
                        total_payload_len,
                        local_payload,
                        overflow_page,
                    });
                }
                Ok(Self::IndexInterior { cells, right_child })
            }
        }
    }

    /// Serializes this page into `raw` as a clean, defragmented SQLite slotted page.
    /// Preserves `raw[0..100]` when `pgno == 1`.
    pub fn write(&self, pgno: Pgno, usable_size: u32, raw: &mut [u8]) -> Result<()> {
        let usable = usable_size as usize;
        let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
        if self.required_bytes(hdr_off) > usable {
            return Err(SqliteError::new(
                sqliters_types::ErrorCode::Full,
                "cells exceed usable B-tree page capacity",
            ));
        }

        // Zero everything from hdr_off..usable (leaving 0..100 on Page 1 and reserved bytes untouched)
        raw[hdr_off..usable].fill(0);

        let ptype = self.page_type();
        let n_cell = self.cell_count();
        let ptr_base = hdr_off + ptype.header_size();

        let mut content_cursor = usable;
        let mut cell_buf = Vec::with_capacity(256);

        fn write_one(
            raw: &mut [u8],
            content_cursor: &mut usize,
            ptr_base: usize,
            idx: usize,
            bytes: &[u8],
        ) {
            *content_cursor -= bytes.len();
            raw[*content_cursor..*content_cursor + bytes.len()].copy_from_slice(bytes);
            let p = ptr_base + 2 * idx;
            raw[p..p + 2].copy_from_slice(&(*content_cursor as u16).to_be_bytes());
        }

        match self {
            Self::TableLeaf { cells } => {
                for (i, cell) in cells.iter().enumerate() {
                    cell_buf.clear();
                    cell.encode_into(&mut cell_buf);
                    write_one(raw, &mut content_cursor, ptr_base, i, &cell_buf);
                }
            }
            Self::TableInterior { cells, right_child } => {
                raw[hdr_off + 8..hdr_off + 12].copy_from_slice(&right_child.to_be_bytes());
                for (i, cell) in cells.iter().enumerate() {
                    cell_buf.clear();
                    cell.encode_into(&mut cell_buf);
                    write_one(raw, &mut content_cursor, ptr_base, i, &cell_buf);
                }
            }
            Self::IndexLeaf { cells } => {
                for (i, cell) in cells.iter().enumerate() {
                    cell_buf.clear();
                    cell.encode_into(&mut cell_buf);
                    write_one(raw, &mut content_cursor, ptr_base, i, &cell_buf);
                }
            }
            Self::IndexInterior { cells, right_child } => {
                raw[hdr_off + 8..hdr_off + 12].copy_from_slice(&right_child.to_be_bytes());
                for (i, cell) in cells.iter().enumerate() {
                    cell_buf.clear();
                    cell.encode_into(&mut cell_buf);
                    write_one(raw, &mut content_cursor, ptr_base, i, &cell_buf);
                }
            }
        }

        raw[hdr_off] = ptype as u8;
        raw[hdr_off + 1..hdr_off + 3].copy_from_slice(&0u16.to_be_bytes()); // first freeblock = 0
        raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(n_cell as u16).to_be_bytes());
        // R-29356-02391: 65536 cell content offset is encoded as 0!
        let encoded_top: u16 = if content_cursor == 65_536 {
            0
        } else {
            content_cursor as u16
        };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0; // fragmented free bytes = 0
        Ok(())
    }
}

// ============================================================================
// Zero-allocation raw slotted-page navigation & fast-path leaf mutations
// ============================================================================

/// Returns the `BtreePageType` of `raw` without parsing its cell array.
#[inline]
pub fn peek_page_type(pgno: Pgno, usable_size: u32, raw: &[u8]) -> Result<BtreePageType> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw.len() < usable || usable <= hdr_off {
        return Err(SqliteError::corrupt(
            "raw page buffer smaller than usable_size",
        ));
    }
    BtreePageType::from_byte(raw[hdr_off])
}

/// Binary-searches a raw `TableInterior` (`0x05`) page for `rowid` (`partition_point(|c| c.key < rowid)`),
/// returning `(idx, child_pgno)` with zero heap allocations.
pub fn find_table_interior_child(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    rowid: i64,
) -> Result<(usize, Pgno)> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 12;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array ({ptr_end}) exceeds usable page size ({usable}) on page {pgno}"
        )));
    }
    let right_child = u32::from_be_bytes([
        raw[hdr_off + 8],
        raw[hdr_off + 9],
        raw[hdr_off + 10],
        raw[hdr_off + 11],
    ]);

    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 5 > usable {
            return Err(SqliteError::corrupt(format!(
                "invalid table interior cell offset {off} on page {pgno}"
            )));
        }
        let (key, _) = get_varint_i64(&raw[off + 4..usable])?;
        if key < rowid {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    let child = if lo < n_cell {
        let p = ptr_base + 2 * lo;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 5 > usable {
            return Err(SqliteError::corrupt(format!(
                "invalid table interior cell offset {off} on page {pgno}"
            )));
        }
        u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]])
    } else {
        right_child
    };
    Ok((lo, child))
}

/// Like [`find_table_interior_child`], but checks the rightmost divider key first in O(1)
/// so monotonic right-edge `INSERT`s descend without O(log N) binary search, returning `(idx, child_pgno, n_cell)`.
#[inline]
pub fn find_table_interior_child_append_hint(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    rowid: i64,
) -> Result<(usize, Pgno, usize)> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 12;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array ({ptr_end}) exceeds usable page size ({usable}) on page {pgno}"
        )));
    }
    let right_child = u32::from_be_bytes([
        raw[hdr_off + 8],
        raw[hdr_off + 9],
        raw[hdr_off + 10],
        raw[hdr_off + 11],
    ]);
    if n_cell == 0 {
        return Ok((0, right_child, 0));
    }

    let lp = ptr_base + 2 * (n_cell - 1);
    let loff = u16::from_be_bytes([raw[lp], raw[lp + 1]]) as usize;
    if loff < ptr_end || loff + 5 > usable {
        return Err(SqliteError::corrupt(format!(
            "invalid table interior cell offset {loff} on page {pgno}"
        )));
    }
    let (last_key, _) = get_varint_i64(&raw[loff + 4..usable])?;
    if last_key < rowid {
        return Ok((n_cell, right_child, n_cell));
    }

    let mut lo = 0usize;
    let mut hi = n_cell - 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 5 > usable {
            return Err(SqliteError::corrupt(format!(
                "invalid table interior cell offset {off} on page {pgno}"
            )));
        }
        let (key, _) = get_varint_i64(&raw[off + 4..usable])?;
        if key < rowid {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }

    let p = ptr_base + 2 * lo;
    let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
    if off < ptr_end || off + 5 > usable {
        return Err(SqliteError::corrupt(format!(
            "invalid table interior cell offset {off} on page {pgno}"
        )));
    }
    let child = u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
    Ok((lo, child, n_cell))
}

/// Returns `right_child` of a raw `TableInterior` (`0x05`) page.
#[inline]
pub fn table_interior_right_child(pgno: Pgno, usable_size: u32, raw: &[u8]) -> Result<Pgno> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 12 > usable {
        return Err(SqliteError::corrupt("truncated table interior header"));
    }
    Ok(u32::from_be_bytes([
        raw[hdr_off + 8],
        raw[hdr_off + 9],
        raw[hdr_off + 10],
        raw[hdr_off + 11],
    ]))
}

/// Returns the maximum `rowid` stored on a raw `TableLeaf` (`0x0d`) page without parsing other cells.
pub fn last_table_leaf_rowid(pgno: Pgno, usable_size: u32, raw: &[u8]) -> Result<Option<i64>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    if n_cell == 0 {
        return Ok(None);
    }
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt("cell pointer array exceeds usable"));
    }
    let p = ptr_base + 2 * (n_cell - 1);
    let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
    if off < ptr_end || off >= usable {
        return Err(SqliteError::corrupt(format!(
            "invalid cell offset {off} on page {pgno}"
        )));
    }
    let (_, n1) = get_varint(&raw[off..usable])?;
    let (rowid, _) = get_varint_i64(&raw[off + n1..usable])?;
    Ok(Some(rowid))
}

/// Binary-searches a raw `TableLeaf` (`0x0d`) page for `rowid`, decoding only the matching cell!
#[allow(clippy::type_complexity)]
pub fn seek_table_leaf_cell(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    rowid: i64,
) -> Result<Option<(u32, Vec<u8>, Option<Pgno>)>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array ({ptr_end}) exceeds usable page size ({usable}) on page {pgno}"
        )));
    }

    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Err(SqliteError::corrupt(format!(
                "invalid cell offset {off} on page {pgno}"
            )));
        }
        let slice = &raw[off..usable];
        let (payload_u64, n1) = get_varint(slice)?;
        let (cell_rowid, n2) = get_varint_i64(&slice[n1..])?;
        match cell_rowid.cmp(&rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                let total_payload_len = u32::try_from(payload_u64)
                    .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
                let bounds = PayloadBounds::for_table_leaf(usable_size);
                let body_start = n1 + n2;
                let local_len = bounds.local_len(total_payload_len);
                let body_end = body_start + local_len;
                let has_ovfl = (local_len as u32) < total_payload_len;
                let needed = body_end + if has_ovfl { 4 } else { 0 };
                if needed > slice.len() {
                    return Err(SqliteError::corrupt(format!(
                        "table leaf cell extends past end of page {pgno}"
                    )));
                }
                let local_payload = slice[body_start..body_end].to_vec();
                let overflow_page = if has_ovfl {
                    Some(u32::from_be_bytes([
                        slice[body_end],
                        slice[body_end + 1],
                        slice[body_end + 2],
                        slice[body_end + 3],
                    ]))
                } else {
                    None
                };
                return Ok(Some((total_payload_len, local_payload, overflow_page)));
            }
        }
    }
    Ok(None)
}

/// Returns `[i_start .. i_start + i_size]` (`i_size >= 4`) to the Format-3 sorted freeblock list
/// (`freeSpace` in `sqlite/src/btree.c:1945-2041`), coalescing adjacent freeblocks or advancing
/// `cell_content_top` (`hdr[5..7]`) in O(freeblocks). Returns `false` if page metadata is corrupt.
///
/// With `secure` (`BTS_FAST_SECURE`, i.e. `PRAGMA secure_delete` = `ON` or `FAST`) the released
/// range — after coalescing, exactly as `freeSpace` does — is overwritten with zeros before the
/// freeblock header is written, so no deleted content survives inside the page.
pub fn raw_page_free_space(
    raw: &mut [u8],
    hdr_off: usize,
    usable: usize,
    mut i_start: usize,
    mut i_size: usize,
    secure: bool,
) -> bool {
    if i_size < 4 || hdr_off + 8 > usable || i_start > usable.saturating_sub(i_size) {
        return false;
    }
    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if raw_top == 0 { 65_536 } else { raw_top };
    if i_start < content_top || content_top > usable {
        return false;
    }

    let mut i_end = i_start + i_size;
    let mut i_ptr = hdr_off + 1;
    let mut i_free_blk: usize;

    if raw[i_ptr] == 0 && raw[i_ptr + 1] == 0 {
        i_free_blk = 0;
    } else {
        loop {
            i_free_blk = u16::from_be_bytes([raw[i_ptr], raw[i_ptr + 1]]) as usize;
            if i_free_blk >= i_start {
                break;
            }
            if i_free_blk <= i_ptr {
                if i_free_blk == 0 {
                    break;
                }
                return false;
            }
            i_ptr = i_free_blk;
        }
        if i_free_blk > usable.saturating_sub(4) {
            return false;
        }

        let mut n_frag = 0usize;
        if i_free_blk != 0 && i_end + 3 >= i_free_blk {
            if i_end > i_free_blk {
                return false;
            }
            n_frag = i_free_blk - i_end;
            let fb_sz = u16::from_be_bytes([raw[i_free_blk + 2], raw[i_free_blk + 3]]) as usize;
            i_end = i_free_blk + fb_sz;
            if i_end > usable {
                return false;
            }
            i_size = i_end - i_start;
            i_free_blk = u16::from_be_bytes([raw[i_free_blk], raw[i_free_blk + 1]]) as usize;
        }

        if i_ptr > hdr_off + 1 {
            let prev_sz = u16::from_be_bytes([raw[i_ptr + 2], raw[i_ptr + 3]]) as usize;
            let i_ptr_end = i_ptr + prev_sz;
            if i_ptr_end + 3 >= i_start {
                if i_ptr_end > i_start {
                    return false;
                }
                n_frag += i_start - i_ptr_end;
                i_size = i_end - i_ptr;
                i_start = i_ptr;
            }
        }

        if n_frag > raw[hdr_off + 7] as usize {
            return false;
        }
        raw[hdr_off + 7] -= n_frag as u8;
    }

    if secure {
        // `btree.c:2019`: "Overwrite deleted information with zeros when the secure_delete
        // option is enabled".
        raw[i_start..i_start + i_size].fill(0);
    }
    if i_start == content_top {
        if i_ptr != hdr_off + 1 {
            return false;
        }
        raw[hdr_off + 1..hdr_off + 3].copy_from_slice(&(i_free_blk as u16).to_be_bytes());
        let encoded_end: u16 = if i_end == 65_536 { 0 } else { i_end as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_end.to_be_bytes());
    } else {
        if i_size > usize::from(u16::MAX) {
            return false;
        }
        raw[i_ptr..i_ptr + 2].copy_from_slice(&(i_start as u16).to_be_bytes());
        raw[i_start..i_start + 2].copy_from_slice(&(i_free_blk as u16).to_be_bytes());
        raw[i_start + 2..i_start + 4].copy_from_slice(&(i_size as u16).to_be_bytes());
    }
    true
}

/// `btreeCellSizeCheck` (`btree.c`, run by `btreeInitPage` when `PRAGMA cell_size_check` is on):
/// every cell pointer of a b-tree page must lie between the end of the cell pointer array and
/// `usable - 4` (`usable - 5` on interior pages), and the cell it names — sized exactly like the
/// page's `xCellSize` method, 16-bit truncation included — must end within the usable area.
///
/// Returns `true` for a page that passes and for a page whose flag byte is not one of the four
/// b-tree page types (overflow / freelist content is not a b-tree page and is not judged here).
pub fn cell_size_check_page(pgno: Pgno, usable_size: u32, raw: &[u8]) -> bool {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw.len() < usable || hdr_off + 12 > usable {
        return true;
    }
    let flags = raw[hdr_off];
    let (leaf, table) = match flags {
        0x0d => (true, true),
        0x05 => (false, true),
        0x0a => (true, false),
        0x02 => (false, false),
        _ => return true,
    };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    // `btreeInitPage`: more than `MX_CELL` cells is corrupt whatever the flag says.
    if n_cell > (raw.len() - 8) / 6 {
        return false;
    }
    let cell_offset = hdr_off + if leaf { 8 } else { 12 };
    let i_cell_first = cell_offset + 2 * n_cell;
    let i_cell_last = usable - if leaf { 4 } else { 5 };
    let (max_local, min_local) = if table && leaf {
        let b = PayloadBounds::for_table_leaf(usable_size);
        (b.max_local, b.min_local)
    } else {
        let b = PayloadBounds::for_index(usable_size);
        (b.max_local, b.min_local)
    };
    // Bytes past the end of the buffer read as zero (C reads its own padding there).
    let at = |i: usize| -> u8 { raw.get(i).copied().unwrap_or(0) };
    for i in 0..n_cell {
        let p = cell_offset + 2 * i;
        let pc = u16::from_be_bytes([at(p), at(p + 1)]) as usize;
        if pc < i_cell_first || pc > i_cell_last {
            return false;
        }
        let mut it = pc;
        let sz: u16 = if table && !leaf {
            // `cellSizePtrNoPayload`: 4-byte child pointer + rowid varint.
            it += 4;
            let end = it + 9;
            loop {
                let b = at(it);
                it += 1;
                if b & 0x80 == 0 || it >= end {
                    break;
                }
            }
            (it - pc) as u16
        } else {
            if !leaf {
                it += 4;
            }
            // Payload size: the 32-bit varint loop of `cellSizePtr*`.
            let mut n_size = u32::from(at(it));
            if n_size >= 0x80 {
                let end = it + 8;
                n_size &= 0x7f;
                loop {
                    it += 1;
                    n_size = (n_size << 7) | u32::from(at(it) & 0x7f);
                    if at(it) < 0x80 || it >= end {
                        break;
                    }
                }
            }
            it += 1;
            if table {
                // `cellSizePtrTableLeaf`: skip the rowid varint (at most 9 bytes).
                let mut n = 0;
                loop {
                    let b = at(it);
                    it += 1;
                    n += 1;
                    if b & 0x80 == 0 {
                        break;
                    }
                    if n == 8 {
                        it += 1;
                        break;
                    }
                }
            }
            let head = (it - pc) as u32;
            if n_size <= max_local {
                let mut total = n_size.wrapping_add(head);
                if leaf && total < 4 {
                    total = 4;
                }
                total as u16
            } else {
                let mut local = min_local + (n_size - min_local) % (usable_size - 4);
                if local > max_local {
                    local = min_local;
                }
                local.wrapping_add(4 + u32::from(head as u16)) as u16
            }
        };
        if pc + usize::from(sz) > usable {
            return false;
        }
    }
    true
}

/// Compacts all live cells on a raw `TableLeaf` (`0x0d`) or `IndexLeaf` (`0x0a`) page into a single
/// contiguous cell-content area (`first_fb = 0`, `frag = 0`), using a 4 KiB stack buffer when
/// `usable_size <= 4096` (zero heap allocation).
pub fn defragment_raw_leaf_in_place(pgno: Pgno, usable_size: u32, raw: &mut [u8]) -> Result<usize> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw.len() < usable || hdr_off + 8 > usable {
        return Err(SqliteError::corrupt("truncated leaf page in defragment"));
    }
    let ptype = BtreePageType::from_byte(raw[hdr_off])?;
    let is_table_leaf = match ptype {
        BtreePageType::TableLeaf => true,
        BtreePageType::IndexLeaf => false,
        _ => {
            return Err(SqliteError::corrupt(
                "defragment_raw_leaf called on non-leaf",
            ));
        }
    };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt("cell pointer exceeds usable_size"));
    }
    if n_cell == 0 {
        raw[ptr_end..usable].fill(0);
        raw[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0;
        return Ok(usable);
    }

    let bounds = if is_table_leaf {
        PayloadBounds::for_table_leaf(usable_size)
    } else {
        PayloadBounds::for_index(usable_size)
    };

    let compact_from = |src: &[u8], dst: &mut [u8]| -> Result<usize> {
        let mut cursor = usable;
        for i in 0..n_cell {
            let p = ptr_base + 2 * i;
            let off = u16::from_be_bytes([dst[p], dst[p + 1]]) as usize;
            if off < ptr_end || off >= usable {
                return Err(SqliteError::corrupt("invalid cell offset during defrag"));
            }
            let slice = &src[off..usable];
            let (payload_u64, n1) = get_varint(slice)?;
            let total_len = u32::try_from(payload_u64)
                .map_err(|_| SqliteError::corrupt("cell payload > 4GiB"))?;
            let local_len = bounds.local_len(total_len);
            let has_ovfl = (local_len as u32) < total_len;
            let n2 = if is_table_leaf {
                let (_, used2) = get_varint_i64(&slice[n1..])?;
                used2
            } else {
                0
            };
            let cell_len = (n1 + n2 + local_len + if has_ovfl { 4 } else { 0 }).max(4);
            if off + cell_len > usable || cursor < ptr_end + cell_len {
                return Err(SqliteError::corrupt("corrupt cell bounds during defrag"));
            }
            cursor -= cell_len;
            dst[cursor..cursor + cell_len].copy_from_slice(&src[off..off + cell_len]);
            dst[p..p + 2].copy_from_slice(&(cursor as u16).to_be_bytes());
        }
        dst[ptr_end..cursor].fill(0);
        dst[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if cursor == 65_536 { 0 } else { cursor as u16 };
        dst[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        dst[hdr_off + 7] = 0;
        Ok(cursor)
    };

    if usable <= 4096 {
        let mut stack_copy = [0u8; 4096];
        stack_copy[ptr_end..usable].copy_from_slice(&raw[ptr_end..usable]);
        compact_from(&stack_copy[..usable], raw)
    } else {
        let mut heap_copy = vec![0u8; usable];
        heap_copy[ptr_end..usable].copy_from_slice(&raw[ptr_end..usable]);
        compact_from(&heap_copy, raw)
    }
}

/// `true` when cell `idx` of the raw `TableLeaf` (`0x0d`) page carries `rowid`.
pub fn table_leaf_cell_has_rowid(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    idx: usize,
    rowid: i64,
    is_root: bool,
) -> bool {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw.len() < usable || hdr_off + 8 > usable || raw[hdr_off] != BtreePageType::TableLeaf as u8
    {
        return false;
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base + 2 * n_cell;
    let _ = is_root;
    if idx >= n_cell || ptr_end > usable {
        return false;
    }
    let p = ptr_base + 2 * idx;
    let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
    if off < ptr_end || off >= usable {
        return false;
    }
    let Ok((_, n1)) = get_varint(&raw[off..usable]) else {
        return false;
    };
    matches!(get_varint_i64(&raw[off + n1..usable]), Ok((cell_rowid, _)) if cell_rowid == rowid)
}

/// [`find_index_interior_child_in_place`] that also reports the child-pointer slot taken
/// (`0..n_cell` = left child of that cell, `n_cell` = the right-most child).
pub fn find_index_interior_slot_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key_vals: &[Value],
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<(usize, Pgno)>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 12 > usable || raw[hdr_off] != BtreePageType::IndexInterior as u8 {
        return Ok(None);
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let right_child = u32::from_be_bytes([
        raw[hdr_off + 8],
        raw[hdr_off + 9],
        raw[hdr_off + 10],
        raw[hdr_off + 11],
    ]);
    let ptr_base = hdr_off + 12;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    if ptr_end > usable {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 5 > usable {
            return Ok(None);
        }
        let Ok((payload_u64, n1)) = get_varint(&raw[off + 4..usable]) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        if (local_len as u32) < total_len {
            return Ok(None);
        }
        let body_start = off + 4 + n1;
        let Some(body_end) = body_start.checked_add(local_len) else {
            return Ok(None);
        };
        if body_end > usable {
            return Ok(None);
        }
        let cmp = match compare_records_with_encoding(
            &raw[body_start..body_end],
            key_vals,
            orders,
            enc,
        ) {
            Ok(ord) => ord,
            Err(e) if e.code == ErrorCode::Corrupt => return Ok(None),
            Err(e) => return Err(e),
        };
        match cmp {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(None),
        }
    }

    let child = if lo < n_cell {
        let p = ptr_base + 2 * lo;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 4 > usable {
            return Ok(None);
        }
        u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]])
    } else {
        right_child
    };
    if child == 0 {
        return Ok(None);
    }
    Ok(Some((lo, child)))
}

/// Result of probing a raw `IndexInterior` (`0x02`) page against a serialized `record_bytes` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawIndexInteriorStep {
    Child(Pgno),
    EqualAt { cell_idx: usize, left_child: Pgno },
}

/// [`probe_index_interior_step_raw`] that additionally reports the child-pointer slot taken:
/// `0..n_cell` selects that cell's left child (or the divider cell itself for `EqualAt`), and
/// `n_cell` selects the page's right-most child.
pub fn probe_index_interior_slot_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<(RawIndexInteriorStep, usize)>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 12 > usable || raw[hdr_off] != BtreePageType::IndexInterior as u8 {
        return Ok(None);
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let right_child = u32::from_be_bytes([
        raw[hdr_off + 8],
        raw[hdr_off + 9],
        raw[hdr_off + 10],
        raw[hdr_off + 11],
    ]);
    let ptr_base = hdr_off + 12;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    if ptr_end > usable {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 5 > usable {
            return Ok(None);
        }
        let Ok((payload_u64, n1)) = get_varint(&raw[off + 4..usable]) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        if (local_len as u32) < total_len {
            return Ok(None);
        }
        let body_start = off + 4 + n1;
        let Some(body_end) = body_start.checked_add(local_len) else {
            return Ok(None);
        };
        if body_end > usable {
            return Ok(None);
        }
        let cmp = match key.compare_stored(&raw[body_start..body_end], orders, enc) {
            Ok(ord) => ord,
            Err(e) if e.code == ErrorCode::Corrupt => return Ok(None),
            Err(e) => return Err(e),
        };
        match cmp {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                let left_child =
                    u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
                if left_child == 0 {
                    return Ok(None);
                }
                return Ok(Some((
                    RawIndexInteriorStep::EqualAt {
                        cell_idx: mid,
                        left_child,
                    },
                    mid,
                )));
            }
        }
    }

    let child = if lo < n_cell {
        let p = ptr_base + 2 * lo;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off + 4 > usable {
            return Ok(None);
        }
        u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]])
    } else {
        right_child
    };
    if child == 0 {
        return Ok(None);
    }
    Ok(Some((RawIndexInteriorStep::Child(child), lo)))
}

/// Probes a raw `IndexLeaf` (`0x0a`) page for in-place deletion of serialized `record_bytes`
/// without decoding `record_bytes` or leaf cells into heap `Value`s.
#[allow(clippy::type_complexity)]
pub fn probe_delete_index_leaf_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
    is_root: bool,
) -> Result<Option<Option<(usize, usize, usize, Option<Pgno>)>>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 8 > usable || raw[hdr_off] != BtreePageType::IndexLeaf as u8 {
        return Ok(None);
    }
    if raw[hdr_off + 7] > 60 {
        return Ok(None);
    }

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    if n_cell == 0 && (raw[hdr_off + 1] != 0 || raw[hdr_off + 2] != 0 || raw[hdr_off + 7] != 0) {
        return Ok(None);
    }
    if n_cell == 0 {
        return Ok(Some(None));
    }
    let ptr_base = hdr_off + 8;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    if ptr_end > usable {
        return Ok(None);
    }
    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if raw_top == 0 { 65_536 } else { raw_top };
    if content_top > usable || content_top < ptr_end {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < content_top || off >= usable {
            return Ok(None);
        }
        let slice = &raw[off..usable];
        let Ok((payload_u64, n1)) = get_varint(slice) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        if (local_len as u32) < total_len {
            return Ok(None);
        }
        let body_end = n1 + local_len;
        let cell_len = body_end.max(4);
        if off + cell_len > usable {
            return Ok(None);
        }
        let cmp = match key.compare_stored(&slice[n1..body_end], orders, enc) {
            Ok(ord) => ord,
            Err(e) if e.code == ErrorCode::Corrupt => return Ok(None),
            Err(e) => return Err(e),
        };
        match cmp {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                if n_cell == 1 && !is_root {
                    return Ok(None);
                }
                return Ok(Some(Some((mid, off, cell_len, None))));
            }
        }
    }
    Ok(Some(None))
}

/// Probes a raw `IndexLeaf` (`0x0a`) page for in-place deletion of `key_vals` without allocating.
/// Returns `Ok(Some(None))` when `key_vals` is absent on this leaf, `Ok(Some(Some((idx, cell_off, cell_len, ovfl))))`
/// when found on a defragmented leaf, or `Ok(None)` when caller should fall back to parsed page handling.
#[allow(clippy::type_complexity)]
pub fn probe_delete_index_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key_vals: &[Value],
    orders: &[KeySortOrder],
    enc: TextEncoding,
    is_root: bool,
) -> Result<Option<Option<(usize, usize, usize, Option<Pgno>)>>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 8 > usable || raw[hdr_off] != BtreePageType::IndexLeaf as u8 {
        return Ok(None);
    }
    if raw[hdr_off + 7] > 60 {
        return Ok(None);
    }

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    if n_cell == 0 && (raw[hdr_off + 1] != 0 || raw[hdr_off + 2] != 0 || raw[hdr_off + 7] != 0) {
        return Ok(None);
    }
    if n_cell == 0 {
        return Ok(Some(None));
    }
    let ptr_base = hdr_off + 8;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    if ptr_end > usable {
        return Ok(None);
    }
    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if raw_top == 0 { 65_536 } else { raw_top };
    if content_top > usable || content_top < ptr_end {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < content_top || off >= usable {
            return Ok(None);
        }
        let slice = &raw[off..usable];
        let Ok((payload_u64, n1)) = get_varint(slice) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        let has_ovfl = (local_len as u32) < total_len;
        if has_ovfl {
            return Ok(None);
        }
        let body_end = n1 + local_len;
        let cell_len = body_end.max(4);
        if off + cell_len > usable {
            return Ok(None);
        }
        let cmp = match compare_records_with_encoding(&slice[n1..body_end], key_vals, orders, enc) {
            Ok(ord) => ord,
            Err(e) if e.code == ErrorCode::Corrupt => return Ok(None),
            Err(e) => return Err(e),
        };
        match cmp {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                if n_cell == 1 && !is_root {
                    return Ok(None);
                }
                return Ok(Some(Some((mid, off, cell_len, None))));
            }
        }
    }
    Ok(Some(None))
}

/// Applies a probed in-place cell deletion (`probe_delete_index_leaf_in_place`) onto a raw `IndexLeaf` (`0x0a`) page
/// in O(1) via `raw_page_free_space` (zero cell-content shifts, zero pointer-array rewrites).
pub fn apply_delete_index_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    idx: usize,
    cell_off: usize,
    cell_len: usize,
    secure: bool,
) {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;

    // `dropCell`: `freeSpace` for the cell, then the pointer is closed up (the vacated slot at
    // the end of the pointer array keeps its bytes) or, when the page is now empty, the
    // header is reset.
    {
        let mut n = n_cell;
        let mut n_free_unused = 0i32;
        if crate::balance::drop_cell(
            raw,
            hdr_off,
            0,
            usable,
            &mut n,
            &mut n_free_unused,
            idx,
            cell_len,
            secure,
        )
        .is_ok()
        {
            return;
        }
    }
    // The free-space bookkeeping of the page is damaged: remove the pointer and repack.
    let _ = cell_off;
    if idx + 1 < n_cell {
        raw.copy_within(
            ptr_base + 2 * (idx + 1)..ptr_base + 2 * n_cell,
            ptr_base + 2 * idx,
        );
    }
    let vacated_ptr = ptr_base + 2 * (n_cell - 1);
    raw[vacated_ptr..vacated_ptr + 2].fill(0);

    let new_n = n_cell - 1;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(new_n as u16).to_be_bytes());
    if new_n == 0 {
        if secure {
            // `dropCell` runs `freeSpace` (which zeroes under secure_delete) before it resets
            // the header of a page that just lost its last cell. Nothing on the page is live
            // any more, so clear the whole body (C leaves one stale freeblock header behind).
            raw[ptr_base..usable].fill(0);
        }
        raw[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0;
        return;
    }

    if !raw_page_free_space(raw, hdr_off, usable, cell_off, cell_len, secure) {
        let _ = defragment_raw_leaf_in_place(pgno, usable_size, raw);
    }
}

/// Outcome of attempting an in-place deletion on a raw `TableLeaf` (`0x0d`) page.
pub enum RawLeafDeleteOutcome {
    NotFound,
    DeletedInPlace {
        overflow_page: Option<Pgno>,
        now_empty: bool,
    },
    Fallback,
}

/// Attempts an in-place deletion from a raw `TableLeaf` (`0x0d`) page in O(log N) search + O(1) freeblock link.
///
/// `idx_hint` is the cell index a positioned cursor already resolved for `rowid`; it is used only
/// when that cell really carries `rowid`, otherwise the page is searched.
pub fn try_delete_table_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    rowid: i64,
    is_root: bool,
    idx_hint: Option<usize>,
    secure: bool,
) -> Result<RawLeafDeleteOutcome> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw[hdr_off] != BtreePageType::TableLeaf as u8 || raw[hdr_off + 7] > 60 {
        return Ok(RawLeafDeleteOutcome::Fallback);
    }

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    if n_cell == 0 {
        return Ok(RawLeafDeleteOutcome::NotFound);
    }
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base + 2 * n_cell;
    if ptr_end > usable {
        return Ok(RawLeafDeleteOutcome::Fallback);
    }
    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if raw_top == 0 { 65_536 } else { raw_top };
    if content_top > usable || content_top < ptr_end {
        return Ok(RawLeafDeleteOutcome::Fallback);
    }

    let mut lo = 0usize;
    let mut hi = n_cell;
    if let Some(hint) = idx_hint
        && hint < n_cell
    {
        let p = ptr_base + 2 * hint;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off >= content_top
            && off < usable
            && let Ok((_, n1)) = get_varint(&raw[off..usable])
            && let Ok((cell_rowid, _)) = get_varint_i64(&raw[off + n1..usable])
            && cell_rowid == rowid
        {
            // Narrow the search to exactly the hinted cell.
            lo = hint;
            hi = hint + 1;
        }
    }
    let mut found: Option<(usize, usize, usize, Option<Pgno>)> = None;
    let bounds = PayloadBounds::for_table_leaf(usable_size);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < content_top || off >= usable {
            return Ok(RawLeafDeleteOutcome::Fallback);
        }
        let slice = &raw[off..usable];
        let (payload_u64, n1) = get_varint(slice)?;
        let (cell_rowid, n2) = get_varint_i64(&slice[n1..])?;
        match cell_rowid.cmp(&rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                let total_payload_len = u32::try_from(payload_u64)
                    .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
                let local_len = bounds.local_len(total_payload_len);
                let has_ovfl = (local_len as u32) < total_payload_len;
                let body_end = n1 + n2 + local_len;
                let cell_len = (body_end + if has_ovfl { 4 } else { 0 }).max(4);
                if off + cell_len > usable {
                    return Ok(RawLeafDeleteOutcome::Fallback);
                }
                let ovfl = if has_ovfl {
                    Some(u32::from_be_bytes([
                        slice[body_end],
                        slice[body_end + 1],
                        slice[body_end + 2],
                        slice[body_end + 3],
                    ]))
                } else {
                    None
                };
                found = Some((mid, off, cell_len, ovfl));
                break;
            }
        }
    }

    let Some((idx, cell_off, cell_len, overflow_page)) = found else {
        return Ok(RawLeafDeleteOutcome::NotFound);
    };

    // (`dropCell` empties a non-root leaf in place as well; `balance()` then merges it.)
    let _ = is_root;

    // `dropCell`: `freeSpace` for the cell, then the pointer is closed up (the vacated slot at
    // the end of the pointer array keeps its bytes) or, when the page is now empty, the
    // header is reset.
    {
        let mut n = n_cell;
        let mut n_free_unused = 0i32;
        if crate::balance::drop_cell(
            raw,
            hdr_off,
            0,
            usable,
            &mut n,
            &mut n_free_unused,
            idx,
            cell_len,
            secure,
        )
        .is_ok()
        {
            return Ok(RawLeafDeleteOutcome::DeletedInPlace {
                overflow_page,
                now_empty: n == 0,
            });
        }
    }
    // The free-space bookkeeping of the page is damaged: remove the pointer and repack.
    if idx + 1 < n_cell {
        raw.copy_within(
            ptr_base + 2 * (idx + 1)..ptr_base + 2 * n_cell,
            ptr_base + 2 * idx,
        );
    }
    let vacated_ptr = ptr_base + 2 * (n_cell - 1);
    raw[vacated_ptr..vacated_ptr + 2].fill(0);

    let new_n = n_cell - 1;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(new_n as u16).to_be_bytes());
    if new_n == 0 {
        if secure {
            // `dropCell` → `freeSpace` zeroes the last cell before the header is reset; nothing
            // on the page is live any more, so clear the whole body.
            raw[ptr_base..usable].fill(0);
        }
        raw[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0;
    } else if !raw_page_free_space(raw, hdr_off, usable, cell_off, cell_len, secure) {
        let _ = defragment_raw_leaf_in_place(pgno, usable_size, raw)?;
    }

    Ok(RawLeafDeleteOutcome::DeletedInPlace {
        overflow_page,
        now_empty: new_n == 0,
    })
}

/// Borrowed view of a single cell on a raw `TableLeaf` (`0x0d`) page without heap allocation.
#[derive(Debug, Clone, Copy)]
pub struct RawTableLeafCell<'a> {
    pub rowid: i64,
    pub total_payload_len: u32,
    pub local_payload: &'a [u8],
    pub overflow_page: Option<Pgno>,
}

/// Borrowed view of the payload metadata of a single cell on a raw `IndexLeaf` (`0x0a`)
/// or `IndexInterior` (`0x02`) page without heap allocation.
#[derive(Debug, Clone, Copy)]
pub struct RawIndexCell<'a> {
    pub total_payload_len: u32,
    pub local_payload: &'a [u8],
    pub overflow_page: Option<Pgno>,
}

/// Reads `(page_type, n_cell, right_child)` from a raw B-tree page header without parsing cells.
#[inline]
pub fn raw_page_header(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
) -> Result<(BtreePageType, usize, Pgno)> {
    let usable = usable_size as usize;
    if raw.len() < usable {
        return Err(SqliteError::corrupt(
            "raw page buffer smaller than usable_size",
        ));
    }
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 8 > usable {
        return Err(SqliteError::corrupt(format!(
            "truncated btree page header on page {pgno}"
        )));
    }
    let page_type = BtreePageType::from_byte(raw[hdr_off])?;
    let hdr_size = page_type.header_size();
    if hdr_off + hdr_size > usable {
        return Err(SqliteError::corrupt(format!(
            "truncated interior btree page header on page {pgno}"
        )));
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_end = (hdr_off + hdr_size)
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array ({ptr_end}) exceeds usable page size ({usable}) on page {pgno}"
        )));
    }
    let right_child = if page_type.is_leaf() {
        0
    } else {
        u32::from_be_bytes([
            raw[hdr_off + 8],
            raw[hdr_off + 9],
            raw[hdr_off + 10],
            raw[hdr_off + 11],
        ])
    };
    Ok((page_type, n_cell, right_child))
}

/// Resolves and bounds-checks the byte offset of `cell_idx` on `raw`.
#[inline]
pub fn raw_cell_offset(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    hdr_size: usize,
    n_cell: usize,
    cell_idx: usize,
) -> Result<usize> {
    if cell_idx >= n_cell {
        return Err(SqliteError::corrupt(format!(
            "cell index {cell_idx} out of bounds ({n_cell}) on page {pgno}"
        )));
    }
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let ptr_base = hdr_off + hdr_size;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable || raw.len() < usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array out of bounds on page {pgno}"
        )));
    }
    let p = ptr_base + 2 * cell_idx;
    let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
    if off < ptr_end || off >= usable {
        return Err(SqliteError::corrupt(format!(
            "invalid cell offset {off} on page {pgno}"
        )));
    }
    Ok(off)
}

/// Reads only the `rowid` of `cell_idx` on a raw `TableLeaf` (`0x0d`) page.
#[inline]
pub fn read_table_leaf_rowid(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    cell_idx: usize,
) -> Result<i64> {
    let usable = usable_size as usize;
    let off = raw_cell_offset(pgno, usable_size, raw, 8, n_cell, cell_idx)?;
    let slice = &raw[off..usable];
    let (_, n1) = get_varint(slice)?;
    let (rowid, _) = get_varint_i64(&slice[n1..])?;
    Ok(rowid)
}

/// Reads a single borrowed `RawTableLeafCell` at `cell_idx` on a raw `TableLeaf` (`0x0d`) page.
pub fn read_table_leaf_cell(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    cell_idx: usize,
) -> Result<RawTableLeafCell<'_>> {
    let usable = usable_size as usize;
    let off = raw_cell_offset(pgno, usable_size, raw, 8, n_cell, cell_idx)?;
    let slice = &raw[off..usable];
    let (payload_u64, n1) = get_varint(slice)?;
    let total_payload_len = u32::try_from(payload_u64)
        .map_err(|_| SqliteError::corrupt("table cell payload > 4GiB"))?;
    let (rowid, n2) = get_varint_i64(&slice[n1..])?;
    let bounds = PayloadBounds::for_table_leaf(usable_size);
    let body_start = n1 + n2;
    let local_len = bounds.local_len(total_payload_len);
    let body_end = body_start + local_len;
    let has_ovfl = (local_len as u32) < total_payload_len;
    let needed = body_end + if has_ovfl { 4 } else { 0 };
    if needed > slice.len() {
        return Err(SqliteError::corrupt(format!(
            "table leaf cell extends past end of page {pgno}"
        )));
    }
    let overflow_page = if has_ovfl {
        Some(u32::from_be_bytes([
            slice[body_end],
            slice[body_end + 1],
            slice[body_end + 2],
            slice[body_end + 3],
        ]))
    } else {
        None
    };
    Ok(RawTableLeafCell {
        rowid,
        total_payload_len,
        local_payload: &slice[body_start..body_end],
        overflow_page,
    })
}

/// Reads `(left_child, key)` at `cell_idx` on a raw `TableInterior` (`0x05`) page.
#[inline]
pub fn read_table_interior_cell(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    cell_idx: usize,
) -> Result<(Pgno, i64)> {
    let usable = usable_size as usize;
    let off = raw_cell_offset(pgno, usable_size, raw, 12, n_cell, cell_idx)?;
    if off + 5 > usable {
        return Err(SqliteError::corrupt(format!(
            "truncated table interior cell on page {pgno}"
        )));
    }
    let left_child = u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
    let (key, _) = get_varint_i64(&raw[off + 4..usable])?;
    Ok((left_child, key))
}

/// Reads a single borrowed `RawIndexCell` at `cell_idx` on a raw `IndexLeaf` (`0x0a`) page.
pub fn read_index_leaf_cell(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    cell_idx: usize,
) -> Result<RawIndexCell<'_>> {
    let usable = usable_size as usize;
    let off = raw_cell_offset(pgno, usable_size, raw, 8, n_cell, cell_idx)?;
    let slice = &raw[off..usable];
    let (payload_u64, n1) = get_varint(slice)?;
    let total_payload_len = u32::try_from(payload_u64)
        .map_err(|_| SqliteError::corrupt("index leaf payload > 4GiB"))?;
    let bounds = PayloadBounds::for_index(usable_size);
    let local_len = bounds.local_len(total_payload_len);
    let body_end = n1 + local_len;
    let has_ovfl = (local_len as u32) < total_payload_len;
    let needed = body_end + if has_ovfl { 4 } else { 0 };
    if needed > slice.len() {
        return Err(SqliteError::corrupt(format!(
            "index leaf cell extends past end of page {pgno}"
        )));
    }
    let overflow_page = if has_ovfl {
        Some(u32::from_be_bytes([
            slice[body_end],
            slice[body_end + 1],
            slice[body_end + 2],
            slice[body_end + 3],
        ]))
    } else {
        None
    };
    Ok(RawIndexCell {
        total_payload_len,
        local_payload: &slice[n1..body_end],
        overflow_page,
    })
}

/// A raw `IndexLeaf` (`0x0a`) page with the per-page invariants of [`read_index_leaf_cell`]
/// (usable size, cell pointer array bounds, local payload limit) resolved once, for loops that
/// read many cells of the same page.
///
/// [`RawIndexLeafView::cell`] returns exactly what [`read_index_leaf_cell`] returns for the same
/// page and index: it only short-cuts cells whose payload size is a one-byte varint and fits
/// the page locally, and defers every other cell (overflow, damaged, out of range) to it.
#[derive(Debug, Clone, Copy)]
pub struct RawIndexLeafView<'a> {
    pgno: Pgno,
    usable_size: u32,
    raw: &'a [u8],
    /// `raw[..usable]`, or empty when the page-level checks of [`raw_cell_offset`] fail.
    page: &'a [u8],
    n_cell: usize,
    ptr_base: usize,
    ptr_end: usize,
    /// Largest payload that is both a one-byte varint and stored without overflow.
    max_inline: usize,
}

impl<'a> RawIndexLeafView<'a> {
    #[inline]
    pub fn new(pgno: Pgno, usable_size: u32, raw: &'a [u8], n_cell: usize) -> Self {
        let usable = usable_size as usize;
        let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
        let ptr_base = hdr_off + 8;
        let ptr_end = ptr_base.saturating_add(n_cell.saturating_mul(2));
        let page = if ptr_end <= usable && raw.len() >= usable {
            &raw[..usable]
        } else {
            &[]
        };
        // `PayloadBounds::for_index(usable_size).max_local`, without underflow on absurd sizes.
        let max_local = (usable.saturating_sub(12) * 64 / 255).saturating_sub(23);
        Self {
            pgno,
            usable_size,
            raw,
            page,
            n_cell,
            ptr_base,
            ptr_end,
            max_inline: max_local.min(0x7f),
        }
    }

    /// Number of cells on the page (as passed to [`RawIndexLeafView::new`]).
    #[inline(always)]
    pub const fn n_cell(&self) -> usize {
        self.n_cell
    }

    /// Same as [`read_index_leaf_cell`] for cell `cell_idx` of this page.
    #[inline(always)]
    pub fn cell(&self, cell_idx: usize) -> Result<RawIndexCell<'a>> {
        if cell_idx < self.n_cell {
            let p = self.ptr_base + 2 * cell_idx;
            if let Some(ptr) = self.page.get(p..p + 2) {
                let off = u16::from_be_bytes([ptr[0], ptr[1]]) as usize;
                if off >= self.ptr_end
                    && let Some(&len_byte) = self.page.get(off)
                {
                    let len = len_byte as usize;
                    if len <= self.max_inline
                        && let Some(local_payload) = self.page.get(off + 1..off + 1 + len)
                    {
                        return Ok(RawIndexCell {
                            total_payload_len: len as u32,
                            local_payload,
                            overflow_page: None,
                        });
                    }
                }
            }
        }
        read_index_leaf_cell(self.pgno, self.usable_size, self.raw, self.n_cell, cell_idx)
    }
}

/// Reads `(left_child, RawIndexCell)` at `cell_idx` on a raw `IndexInterior` (`0x02`) page.
pub fn read_index_interior_cell(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    cell_idx: usize,
) -> Result<(Pgno, RawIndexCell<'_>)> {
    let usable = usable_size as usize;
    let off = raw_cell_offset(pgno, usable_size, raw, 12, n_cell, cell_idx)?;
    if off + 5 > usable {
        return Err(SqliteError::corrupt(format!(
            "truncated index interior cell on page {pgno}"
        )));
    }
    let left_child = u32::from_be_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
    let slice = &raw[off + 4..usable];
    let (payload_u64, n1) = get_varint(slice)?;
    let total_payload_len = u32::try_from(payload_u64)
        .map_err(|_| SqliteError::corrupt("index interior payload > 4GiB"))?;
    let bounds = PayloadBounds::for_index(usable_size);
    let local_len = bounds.local_len(total_payload_len);
    let body_end = n1 + local_len;
    let has_ovfl = (local_len as u32) < total_payload_len;
    let needed = body_end + if has_ovfl { 4 } else { 0 };
    if needed > slice.len() {
        return Err(SqliteError::corrupt(format!(
            "index interior cell extends past end of page {pgno}"
        )));
    }
    let overflow_page = if has_ovfl {
        Some(u32::from_be_bytes([
            slice[body_end],
            slice[body_end + 1],
            slice[body_end + 2],
            slice[body_end + 3],
        ]))
    } else {
        None
    };
    Ok((
        left_child,
        RawIndexCell {
            total_payload_len,
            local_payload: &slice[n1..body_end],
            overflow_page,
        },
    ))
}

/// Binary-searches a raw `TableLeaf` (`0x0d`) page for `target_rowid` without allocating,
/// returning `Ok(exact_idx)` on exact match or `Err(insert_idx)` (`0..=n_cell`) on miss.
#[inline]
pub fn binary_search_table_leaf(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    target_rowid: i64,
) -> Result<std::result::Result<usize, usize>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable || raw.len() < usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array out of bounds on page {pgno}"
        )));
    }

    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Err(SqliteError::corrupt(format!(
                "invalid cell offset {off} on page {pgno}"
            )));
        }
        let slice = &raw[off..usable];
        let (_, n1) = get_varint(slice)?;
        let (cell_rowid, _) = get_varint_i64(&slice[n1..])?;
        match cell_rowid.cmp(&target_rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(Ok(mid)),
        }
    }
    Ok(Err(lo))
}

/// Binary-searches a raw `TableLeaf` (`0x0d`) page for `target_rowid` and, on exact match,
/// also returns `Some((payload_start, payload_end))` if the payload resides completely in-page
/// (`overflow_page.is_none()` and `usable <= u16::MAX as usize`).
#[inline]
#[allow(clippy::type_complexity)]
pub fn binary_search_table_leaf_with_span(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    target_rowid: i64,
) -> Result<std::result::Result<(usize, Option<(u16, u16)>), usize>> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base
        .checked_add(2 * n_cell)
        .ok_or_else(|| SqliteError::corrupt("cell pointer array overflow"))?;
    if ptr_end > usable || raw.len() < usable {
        return Err(SqliteError::corrupt(format!(
            "cell pointer array out of bounds on page {pgno}"
        )));
    }

    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Err(SqliteError::corrupt(format!(
                "invalid cell offset {off} on page {pgno}"
            )));
        }
        let slice = &raw[off..usable];
        let (payload_u64, n1) = get_varint(slice)?;
        let (cell_rowid, n2) = get_varint_i64(&slice[n1..])?;
        match cell_rowid.cmp(&target_rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                let inline_span = if usable <= u16::MAX as usize
                    && let Ok(total_len) = u32::try_from(payload_u64)
                {
                    let bounds = PayloadBounds::for_table_leaf(usable_size);
                    if total_len <= bounds.max_local {
                        let p_start = off + n1 + n2;
                        let p_end = p_start + (total_len as usize);
                        if p_end <= usable {
                            Some((p_start as u16, p_end as u16))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                return Ok(Ok((mid, inline_span)));
            }
        }
    }
    Ok(Err(lo))
}
