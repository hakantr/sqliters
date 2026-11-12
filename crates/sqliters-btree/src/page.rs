//! On-disk slotted B-tree page layout, payload fraction bounds, and cell codecs
//! ported from `sqlite/src/btreeInt.h` and `sqlite/src/btree.c`.

use sqliters_pager::DB_HEADER_SIZE;
use sqliters_types::{
    ErrorCode, KeySortOrder, Pgno, RawRecordKey, Result, SqliteError, TextEncoding, Value,
    compare_records_with_encoding, get_varint, get_varint_i64, push_varint, put_varint, varint_len,
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

/// Result of probing a raw `TableLeaf` (`0x0d`) page for zero-allocation non-overflow insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableLeafInsertProbe {
    /// Fits in contiguous unallocated space on this leaf at `insert_idx` (`0..=n_cell`).
    Fit {
        insert_idx: usize,
        content_top: usize,
    },
    /// Replaces an existing non-overflow cell at `idx` in-place on this defragmented leaf.
    Replace {
        idx: usize,
        old_off: usize,
        old_cell_len: usize,
        content_top: usize,
    },
    /// Matching `rowid` exists on this leaf, but replacement requires overflow cleanup or page split.
    DuplicateNeedsFallback,
    /// Matching non-overflow `rowid` cell exists at `idx`, the new cell has a different size and
    /// does not fit in the contiguous gap: the caller may rewrite the leaf with
    /// [`replace_table_leaf_repack_raw`]. Only produced when the probe is asked for it;
    /// `legacy_defrag` records whether the probe would otherwise have answered `NeedsDefrag`
    /// (`true`) or `DuplicateNeedsFallback` (`false`).
    ReplaceRepack {
        idx: usize,
        old_off: usize,
        old_cell_len: usize,
        legacy_defrag: bool,
    },
    /// Defragmented leaf is full, `n_cell >= 1`, `rowid > last_rowid`, and the new cell fits on a fresh leaf (`balance_quick`).
    QuickRightSplit { last_rowid: i64 },
    /// Leaf has freeblocks/fragments that should be compacted in-place before retrying.
    NeedsDefrag,
    /// Fragmented page or structural split; caller should run general path.
    Fallback,
}

/// Returns `[i_start .. i_start + i_size]` (`i_size >= 4`) to the Format-3 sorted freeblock list
/// (`freeSpace` in `sqlite/src/btree.c:1945-2041`), coalescing adjacent freeblocks or advancing
/// `cell_content_top` (`hdr[5..7]`) in O(freeblocks). Returns `false` if page metadata is corrupt.
pub fn raw_page_free_space(
    raw: &mut [u8],
    hdr_off: usize,
    usable: usize,
    mut i_start: usize,
    mut i_size: usize,
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

/// Probes a raw `TableLeaf` (`0x0d`) page for zero-allocation insertion or replacement of `(rowid, payload)`
/// when `payload.len() <= max_local` (no overflow page).
pub fn probe_insert_table_leaf_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    rowid: i64,
    payload_len: usize,
    repack_replace: bool,
    idx_hint: Option<usize>,
) -> Result<TableLeafInsertProbe> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 8 > usable || raw[hdr_off] != BtreePageType::TableLeaf as u8 {
        return Ok(TableLeafInsertProbe::Fallback);
    }
    let first_fb = u16::from_be_bytes([raw[hdr_off + 1], raw[hdr_off + 2]]);
    let frag = raw[hdr_off + 7];
    let has_reclaimable = first_fb != 0 || frag != 0;

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(TableLeafInsertProbe::Fallback);
    };
    let ptr_end_after = ptr_end + 2;
    if ptr_end > usable {
        return Ok(TableLeafInsertProbe::Fallback);
    }

    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if n_cell == 0 {
        usable
    } else if raw_top == 0 {
        65_536
    } else {
        raw_top
    };
    if content_top > usable || content_top < ptr_end {
        return Ok(TableLeafInsertProbe::Fallback);
    }

    let cell_len = (varint_len(payload_len as u64) + varint_len(rowid as u64) + payload_len).max(4);
    let fits_here = ptr_end_after <= usable && ptr_end_after + cell_len <= content_top;

    if n_cell == 0 {
        return Ok(if fits_here && !has_reclaimable {
            TableLeafInsertProbe::Fit {
                insert_idx: 0,
                content_top,
            }
        } else if has_reclaimable {
            TableLeafInsertProbe::NeedsDefrag
        } else {
            TableLeafInsertProbe::Fallback
        });
    }

    let bounds = PayloadBounds::for_table_leaf(usable_size);
    let probe_replace = |idx: usize, off: usize, old_u64: u64, n1: usize, n2: usize| {
        if off < content_top {
            return TableLeafInsertProbe::DuplicateNeedsFallback;
        }
        let Ok(old_total) = u32::try_from(old_u64) else {
            return TableLeafInsertProbe::DuplicateNeedsFallback;
        };
        let old_local = bounds.local_len(old_total);
        if (old_local as u32) < old_total {
            return TableLeafInsertProbe::DuplicateNeedsFallback;
        }
        let old_cell_len = (n1 + n2 + old_local).max(4);
        if off + old_cell_len > usable {
            return TableLeafInsertProbe::DuplicateNeedsFallback;
        }
        if cell_len == old_cell_len || ptr_end + cell_len <= content_top {
            return TableLeafInsertProbe::Replace {
                idx,
                old_off: off,
                old_cell_len,
                content_top,
            };
        }
        let legacy_defrag = has_reclaimable || ptr_end + cell_len <= content_top + old_cell_len;
        if repack_replace {
            return TableLeafInsertProbe::ReplaceRepack {
                idx,
                old_off: off,
                old_cell_len,
                legacy_defrag,
            };
        }
        if legacy_defrag {
            return TableLeafInsertProbe::NeedsDefrag;
        }
        TableLeafInsertProbe::DuplicateNeedsFallback
    };

    // A caller that already knows where `rowid` lives on this leaf (a positioned cursor) skips
    // the search; any mismatch simply falls through to it.
    if let Some(hint) = idx_hint
        && hint < n_cell
    {
        let p = ptr_base + 2 * hint;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off >= ptr_end
            && off < usable
            && let Ok((hp_u64, hn1)) = get_varint(&raw[off..usable])
            && let Ok((hint_rowid, hn2)) = get_varint_i64(&raw[off + hn1..usable])
            && hint_rowid == rowid
        {
            return Ok(probe_replace(hint, off, hp_u64, hn1, hn2));
        }
    }

    let lp = ptr_base + 2 * (n_cell - 1);
    let loff = u16::from_be_bytes([raw[lp], raw[lp + 1]]) as usize;
    if loff < ptr_end || loff >= usable {
        return Ok(TableLeafInsertProbe::Fallback);
    }
    let (lp_u64, n1) = get_varint(&raw[loff..usable])?;
    let (last_rowid, n2) = get_varint_i64(&raw[loff + n1..usable])?;

    if rowid > last_rowid {
        if fits_here {
            return Ok(TableLeafInsertProbe::Fit {
                insert_idx: n_cell,
                content_top,
            });
        }
        if has_reclaimable {
            return Ok(TableLeafInsertProbe::NeedsDefrag);
        }
        if 8 + 2 + cell_len <= usable {
            return Ok(TableLeafInsertProbe::QuickRightSplit { last_rowid });
        }
        return Ok(TableLeafInsertProbe::Fallback);
    }
    if rowid == last_rowid {
        return Ok(probe_replace(n_cell - 1, loff, lp_u64, n1, n2));
    }

    let mut lo = 0usize;
    let mut hi = n_cell - 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let p = ptr_base + 2 * mid;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Ok(TableLeafInsertProbe::Fallback);
        }
        let (mp_u64, mn1) = get_varint(&raw[off..usable])?;
        let (mid_rowid, mn2) = get_varint_i64(&raw[off + mn1..usable])?;
        match mid_rowid.cmp(&rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                return Ok(probe_replace(mid, off, mp_u64, mn1, mn2));
            }
        }
    }
    if !fits_here {
        return Ok(if has_reclaimable {
            TableLeafInsertProbe::NeedsDefrag
        } else {
            TableLeafInsertProbe::Fallback
        });
    }
    Ok(TableLeafInsertProbe::Fit {
        insert_idx: lo,
        content_top,
    })
}

/// [`probe_insert_table_leaf_raw`] restricted to replacing `rowid` on this leaf, for a caller
/// that expects it at cell `idx`. Returns the `Replace` / `ReplaceRepack` answer, or `None`
/// when `rowid` is not on the page, the page is unusual, or the replacement is not in-page.
pub fn probe_table_leaf_replace_at_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    idx: usize,
    rowid: i64,
    payload_len: usize,
) -> Option<TableLeafInsertProbe> {
    match probe_insert_table_leaf_raw(pgno, usable_size, raw, rowid, payload_len, true, Some(idx)) {
        Ok(
            probe @ (TableLeafInsertProbe::Replace { .. }
            | TableLeafInsertProbe::ReplaceRepack { .. }),
        ) => Some(probe),
        _ => None,
    }
}

/// `true` when cell `idx` of the raw `TableLeaf` (`0x0d`) page carries `rowid` and deleting it
/// leaves the page in the tree (the page is the root, or keeps at least one other cell).
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
    if idx >= n_cell || ptr_end > usable || (n_cell == 1 && !is_root) {
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

/// Replaces an existing non-overflow `(rowid, payload)` cell in-place on a `TableLeaf` (`0x0d`) page
/// in O(1) without heap allocations (`new_cell_len == old_cell_len` overwrites in place; otherwise
/// frees the old `>= 4`-byte cell to the freeblock list and allocates from `content_top`).
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn apply_replace_table_leaf_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    rowid: i64,
    payload: &[u8],
    idx: usize,
    old_off: usize,
    old_cell_len: usize,
    _content_top: usize,
) {
    let usable = usable_size as usize;
    let new_cell_len =
        (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4);
    if new_cell_len == old_cell_len {
        let mut pos = old_off;
        pos += put_varint(&mut raw[pos..], payload.len() as u64);
        pos += put_varint(&mut raw[pos..], rowid as u64);
        raw[pos..pos + payload.len()].copy_from_slice(payload);
        pos += payload.len();
        while pos < old_off + new_cell_len {
            raw[pos] = 0;
            pos += 1;
        }
        return;
    }

    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let ptr_base = hdr_off + 8;

    if !raw_page_free_space(raw, hdr_off, usable, old_off, old_cell_len) {
        // Fallback: shift/compact cleanly if freeblock chain was unexpected.
        let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
        let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
        let cur_top = if raw_top == 0 { 65_536 } else { raw_top };
        if old_off > cur_top {
            raw.copy_within(cur_top..old_off, cur_top + old_cell_len);
        }
        let old_off_u16 = old_off as u16;
        let shift = old_cell_len as u16;
        for j in 0..n_cell {
            if j != idx {
                let p = ptr_base + 2 * j;
                let cur = u16::from_be_bytes([raw[p], raw[p + 1]]);
                if cur < old_off_u16 {
                    raw[p..p + 2].copy_from_slice(&(cur + shift).to_be_bytes());
                }
            }
        }
        let freed_top = cur_top + old_cell_len;
        let new_top = freed_top - new_cell_len;
        let mut pos = new_top;
        pos += put_varint(&mut raw[pos..], payload.len() as u64);
        pos += put_varint(&mut raw[pos..], rowid as u64);
        raw[pos..pos + payload.len()].copy_from_slice(payload);
        pos += payload.len();
        while pos < new_top + new_cell_len {
            raw[pos] = 0;
            pos += 1;
        }
        let ip = ptr_base + 2 * idx;
        raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());
        let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        return;
    }

    // Note: `raw_page_free_space` may have increased `hdr[5..7]` if `old_off == content_top`!
    let raw_top_after = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let cur_top_after = if raw_top_after == 0 {
        65_536
    } else {
        raw_top_after
    };
    let new_top = cur_top_after - new_cell_len;
    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], payload.len() as u64);
    pos += put_varint(&mut raw[pos..], rowid as u64);
    raw[pos..pos + payload.len()].copy_from_slice(payload);
    pos += payload.len();
    while pos < new_top + new_cell_len {
        raw[pos] = 0;
        pos += 1;
    }

    let ip = ptr_base + 2 * idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
}

/// Replaces the non-overflow cell `idx` (`rowid`, at `old_off`, `old_cell_len` bytes) of a raw
/// `TableLeaf` (`0x0d`) page with a non-overflow `(rowid, payload)` cell of a different size that
/// does not fit in the contiguous gap, without parsing the page into heap cells.
///
/// Produces exactly the bytes of the general path it replaces:
/// - when the new cell fits above the cell content once the page is compacted, the page is
///   defragmented and the old cell is released / the new one allocated at the content top
///   ([`defragment_raw_leaf_in_place`] + [`apply_replace_table_leaf_raw`]);
/// - otherwise, when the page still holds every cell after the substitution, it is rewritten in
///   the canonical packed layout of [`ParsedPage::write`] (cells in key order from the page end,
///   no freeblocks, no fragments, zeroed gap).
///
/// Returns `Ok(false)` with `raw` untouched when the substitution needs a split or the page is
/// not provably well formed (bad offsets, non-minimal varints, non-zero padding of a short cell,
/// or — with `reject_overflow_cells` — any cell with an overflow chain).
#[allow(clippy::too_many_arguments)]
pub fn replace_table_leaf_repack_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    rowid: i64,
    payload: &[u8],
    idx: usize,
    old_off: usize,
    old_cell_len: usize,
    reject_overflow_cells: bool,
) -> Result<bool> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw.len() < usable || hdr_off + 8 > usable || raw[hdr_off] != BtreePageType::TableLeaf as u8
    {
        return Ok(false);
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base + 2 * n_cell;
    if idx >= n_cell || ptr_end > usable {
        return Ok(false);
    }
    let bounds = PayloadBounds::for_table_leaf(usable_size);
    if payload.len() > bounds.max_local as usize {
        return Ok(false);
    }
    let new_cell_len =
        (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4);

    // Pass 1: validate every cell and record its length; nothing is written yet.
    let mut lens_inline = [0u16; 512];
    let mut lens_heap: Vec<u16> = Vec::new();
    let lens: &mut [u16] = if n_cell <= lens_inline.len() {
        &mut lens_inline[..n_cell]
    } else {
        lens_heap.resize(n_cell, 0);
        &mut lens_heap
    };
    let mut live = 0usize;
    let mut prev_rowid: Option<i64> = None;
    for (i, len_slot) in lens.iter_mut().enumerate() {
        let p = ptr_base + 2 * i;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Ok(false);
        }
        let slice = &raw[off..usable];
        let Ok((payload_u64, n1)) = get_varint(slice) else {
            return Ok(false);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(false);
        };
        let Ok((cell_rowid, n2)) = get_varint_i64(&slice[n1..]) else {
            return Ok(false);
        };
        if n1 != varint_len(payload_u64) || n2 != varint_len(cell_rowid as u64) {
            return Ok(false);
        }
        if prev_rowid.is_some_and(|prev| prev >= cell_rowid) {
            return Ok(false);
        }
        prev_rowid = Some(cell_rowid);
        let local_len = bounds.local_len(total_len);
        let has_ovfl = (local_len as u32) < total_len;
        if has_ovfl && reject_overflow_cells {
            return Ok(false);
        }
        let body_len = n1 + n2 + local_len + if has_ovfl { 4 } else { 0 };
        let cell_len = body_len.max(4);
        if cell_len > slice.len() || cell_len > usize::from(u16::MAX) {
            return Ok(false);
        }
        if slice[body_len..cell_len].iter().any(|&b| b != 0) {
            return Ok(false);
        }
        if i == idx
            && (off != old_off || cell_len != old_cell_len || cell_rowid != rowid || has_ovfl)
        {
            return Ok(false);
        }
        *len_slot = cell_len as u16;
        live += cell_len;
    }
    if ptr_end + live > usable || old_cell_len == new_cell_len {
        return Ok(false);
    }
    let packed_top = usable - live;

    if ptr_end + new_cell_len <= packed_top {
        // Enough room once compacted: compact, then release the old cell and allocate on top.
        defragment_raw_leaf_in_place(pgno, usable_size, raw)?;
        let p = ptr_base + 2 * idx;
        let moved_off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        apply_replace_table_leaf_raw(
            pgno,
            usable_size,
            raw,
            rowid,
            payload,
            idx,
            moved_off,
            old_cell_len,
            packed_top,
        );
        return Ok(true);
    }
    if ptr_end + new_cell_len > packed_top + old_cell_len {
        return Ok(false);
    }

    // Canonical repack with cell `idx` substituted.
    let mut stack_copy = [0u8; 4096];
    let heap_copy: Vec<u8>;
    let src: &[u8] = if usable <= stack_copy.len() {
        stack_copy[ptr_end..usable].copy_from_slice(&raw[ptr_end..usable]);
        &stack_copy[..usable]
    } else {
        let mut v = vec![0u8; usable];
        v[ptr_end..usable].copy_from_slice(&raw[ptr_end..usable]);
        heap_copy = v;
        &heap_copy
    };
    let mut cursor = usable;
    for (i, &len) in lens.iter().enumerate() {
        let p = ptr_base + 2 * i;
        if i == idx {
            cursor -= new_cell_len;
            let mut pos = cursor;
            pos += put_varint(&mut raw[pos..], payload.len() as u64);
            pos += put_varint(&mut raw[pos..], rowid as u64);
            raw[pos..pos + payload.len()].copy_from_slice(payload);
            pos += payload.len();
            raw[pos..cursor + new_cell_len].fill(0);
        } else {
            let cell_len = usize::from(len);
            let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
            cursor -= cell_len;
            if off != cursor {
                raw[cursor..cursor + cell_len].copy_from_slice(&src[off..off + cell_len]);
            }
        }
        raw[p..p + 2].copy_from_slice(&(cursor as u16).to_be_bytes());
    }
    raw[ptr_end..cursor].fill(0);
    raw[hdr_off + 1..hdr_off + 3].fill(0);
    let encoded_top: u16 = if cursor == 65_536 { 0 } else { cursor as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
    raw[hdr_off + 7] = 0;
    Ok(true)
}

/// Writes a non-overflow `(rowid, payload)` cell directly into `raw` at `(insert_idx, content_top)`
/// with zero heap allocations.
#[inline]
pub fn apply_insert_table_leaf_raw(
    pgno: Pgno,
    raw: &mut [u8],
    rowid: i64,
    payload: &[u8],
    insert_idx: usize,
    content_top: usize,
) {
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let cell_len =
        (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4);
    let new_top = content_top - cell_len;

    if insert_idx < n_cell {
        raw.copy_within(
            ptr_base + 2 * insert_idx..ptr_base + 2 * n_cell,
            ptr_base + 2 * (insert_idx + 1),
        );
    }
    let ip = ptr_base + 2 * insert_idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());

    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], payload.len() as u64);
    pos += put_varint(&mut raw[pos..], rowid as u64);
    raw[pos..pos + payload.len()].copy_from_slice(payload);
    pos += payload.len();
    while pos < new_top + cell_len {
        raw[pos] = 0;
        pos += 1;
    }

    let new_n = (n_cell + 1) as u16;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&new_n.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
}

/// Initializes a freshly allocated non-Page-1 `TableLeaf` (`0x0d`) containing a single non-overflow cell
/// `(rowid, payload)` (`balance_quick` right sibling).
pub fn init_single_cell_table_leaf_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    rowid: i64,
    payload: &[u8],
) {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    raw[hdr_off..usable].fill(0);

    let cell_len =
        (varint_len(payload.len() as u64) + varint_len(rowid as u64) + payload.len()).max(4);
    let new_top = usable - cell_len;

    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], payload.len() as u64);
    pos += put_varint(&mut raw[pos..], rowid as u64);
    raw[pos..pos + payload.len()].copy_from_slice(payload);

    raw[hdr_off] = BtreePageType::TableLeaf as u8;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&1u16.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
    raw[hdr_off + 8..hdr_off + 10].copy_from_slice(&(new_top as u16).to_be_bytes());
}

/// Probes whether a `TableInterior` (`0x05`) page has contiguous unallocated space to append
/// a rightmost divider `(left_child, div_key)` and update `right_child` in-place.
pub fn probe_append_table_interior_rightmost(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    div_key: i64,
) -> Option<usize> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if hdr_off + 12 > usable || raw[hdr_off] != BtreePageType::TableInterior as u8 {
        return None;
    }
    let first_fb = u16::from_be_bytes([raw[hdr_off + 1], raw[hdr_off + 2]]);
    let frag = raw[hdr_off + 7];
    if first_fb != 0 || frag != 0 {
        return None;
    }
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 12;
    let ptr_end = ptr_base.checked_add(2 * n_cell)?;
    let ptr_end_after = ptr_end + 2;
    if ptr_end_after > usable {
        return None;
    }
    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if n_cell == 0 {
        usable
    } else if raw_top == 0 {
        65_536
    } else {
        raw_top
    };
    if content_top > usable || content_top < ptr_end {
        return None;
    }
    let cell_len = (4 + varint_len(div_key as u64)).max(4);
    if ptr_end_after + cell_len > content_top {
        return None;
    }
    Some(content_top)
}

/// Appends `(left_child, div_key)` at the end of a `TableInterior` (`0x05`) page and updates
/// `right_child = new_right_child` in-place with zero heap allocations.
pub fn apply_append_table_interior_rightmost(
    pgno: Pgno,
    raw: &mut [u8],
    left_child: Pgno,
    div_key: i64,
    new_right_child: Pgno,
    content_top: usize,
) {
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 12;
    let cell_len = (4 + varint_len(div_key as u64)).max(4);
    let new_top = content_top - cell_len;

    raw[new_top..new_top + 4].copy_from_slice(&left_child.to_be_bytes());
    let _ = put_varint(&mut raw[new_top + 4..], div_key as u64);

    let ip = ptr_base + 2 * n_cell;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());

    let new_n = (n_cell + 1) as u16;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&new_n.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
    raw[hdr_off + 8..hdr_off + 12].copy_from_slice(&new_right_child.to_be_bytes());
}

/// Attempts an O(1) in-place defragmented insertion into a raw `TableLeaf` (`0x0d`) page
/// when `rowid` is not already present, the page has no freeblocks/fragments, and enough
/// contiguous space exists between the cell pointer array and `cell_content_top`.
/// Returns `Ok(true)` if inserted in-place, or `Ok(false)` if caller should fall back to full page parse/split.
pub fn try_insert_table_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    cell: &TableLeafCell,
) -> Result<bool> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw[hdr_off] != BtreePageType::TableLeaf as u8 {
        return Ok(false);
    }
    let first_fb = u16::from_be_bytes([raw[hdr_off + 1], raw[hdr_off + 2]]);
    let frag = raw[hdr_off + 7];

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base + 2 * n_cell;
    let ptr_end_after = ptr_end + 2;
    if ptr_end_after > usable {
        return Ok(false);
    }

    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let mut content_top = if n_cell == 0 {
        usable
    } else if raw_top == 0 {
        65_536
    } else {
        raw_top
    };
    if content_top > usable || content_top < ptr_end {
        return Ok(false);
    }

    let cell_len = cell.encoded_size();
    if ptr_end_after + cell_len > content_top {
        if first_fb != 0 || frag != 0 {
            let Ok(compacted_top) = defragment_raw_leaf_in_place(pgno, usable_size, raw) else {
                return Ok(false);
            };
            content_top = compacted_top;
        }
        if ptr_end_after + cell_len > content_top {
            return Ok(false);
        }
    }

    // Fast check: monotonic append (`n_cell == 0` or `cell.rowid > last_rowid`)!
    let insert_idx = if n_cell == 0 {
        0
    } else {
        let lp = ptr_base + 2 * (n_cell - 1);
        let loff = u16::from_be_bytes([raw[lp], raw[lp + 1]]) as usize;
        if loff < ptr_end || loff >= usable {
            return Ok(false);
        }
        let (_, n1) = get_varint(&raw[loff..usable])?;
        let (last_rowid, _) = get_varint_i64(&raw[loff + n1..usable])?;
        if cell.rowid > last_rowid {
            n_cell
        } else if cell.rowid == last_rowid {
            return Ok(false);
        } else {
            // Binary search for insertion index
            let mut lo = 0usize;
            let mut hi = n_cell;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let p = ptr_base + 2 * mid;
                let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
                if off < ptr_end || off >= usable {
                    return Ok(false);
                }
                let (_, mn1) = get_varint(&raw[off..usable])?;
                let (mid_rowid, _) = get_varint_i64(&raw[off + mn1..usable])?;
                match mid_rowid.cmp(&cell.rowid) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Ok(false), // replacement: let slow path free old overflow
                }
            }
            lo
        }
    };

    let new_top = content_top - cell_len;
    if insert_idx < n_cell {
        raw.copy_within(
            ptr_base + 2 * insert_idx..ptr_base + 2 * n_cell,
            ptr_base + 2 * (insert_idx + 1),
        );
    }
    let ip = ptr_base + 2 * insert_idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());

    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], u64::from(cell.total_payload_len));
    pos += put_varint(&mut raw[pos..], cell.rowid as u64);
    raw[pos..pos + cell.local_payload.len()].copy_from_slice(&cell.local_payload);
    pos += cell.local_payload.len();
    if let Some(ovfl) = cell.overflow_page {
        raw[pos..pos + 4].copy_from_slice(&ovfl.to_be_bytes());
        pos += 4;
    }
    while pos < new_top + cell_len {
        raw[pos] = 0;
        pos += 1;
    }

    let new_n = (n_cell + 1) as u16;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&new_n.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
    Ok(true)
}

/// Binary-searches a raw `IndexInterior` (`0x02`) page for the child pointer into which `key_vals`
/// descends when all probed divider cells reside locally (no overflow) and none equals `key_vals`.
/// Returns `Ok(None)` if caller should fall back to full page parse/replace.
pub fn find_index_interior_child_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key_vals: &[Value],
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<Pgno>> {
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
    Ok(Some(child))
}

/// Probes a raw `IndexLeaf` (`0x0a`) page for O(log N) in-place defragmented insertion without
/// mutating the page. Returns `Ok(Some((insert_idx, content_top)))` when `key_vals` is not already
/// present, all probed cells are local (no overflow), the page has no freeblocks/fragments, and
/// enough contiguous space exists between the pointer array and `content_top`.
pub fn probe_insert_index_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    cell: &IndexLeafCell,
    key_vals: &[Value],
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<(usize, usize)>> {
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
    let ptr_base = hdr_off + 8;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    let ptr_end_after = ptr_end + 2;
    if ptr_end_after > usable {
        return Ok(None);
    }

    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if n_cell == 0 {
        usable
    } else if raw_top == 0 {
        65_536
    } else {
        raw_top
    };
    if content_top > usable || content_top < ptr_end {
        return Ok(None);
    }

    let cell_len = cell.encoded_size();
    if ptr_end_after + cell_len > content_top {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let cmp_cell_at = |idx: usize| -> Result<Option<std::cmp::Ordering>> {
        let p = ptr_base + 2 * idx;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Ok(None);
        }
        let Ok((payload_u64, n1)) = get_varint(&raw[off..usable]) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        if (local_len as u32) < total_len {
            return Ok(None);
        }
        let body_start = off + n1;
        let Some(body_end) = body_start.checked_add(local_len) else {
            return Ok(None);
        };
        if body_end > usable {
            return Ok(None);
        }
        match compare_records_with_encoding(&raw[body_start..body_end], key_vals, orders, enc) {
            Ok(ord) => Ok(Some(ord)),
            Err(e) if e.code == ErrorCode::Corrupt => Ok(None),
            Err(e) => Err(e),
        }
    };

    let insert_idx = if n_cell == 0 {
        0
    } else {
        let Some(last_ord) = cmp_cell_at(n_cell - 1)? else {
            return Ok(None);
        };
        match last_ord {
            std::cmp::Ordering::Less => n_cell,
            std::cmp::Ordering::Equal => return Ok(None),
            std::cmp::Ordering::Greater => {
                let mut lo = 0usize;
                let mut hi = n_cell - 1;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let Some(ord) = cmp_cell_at(mid)? else {
                        return Ok(None);
                    };
                    match ord {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return Ok(None),
                    }
                }
                lo
            }
        }
    };

    Ok(Some((insert_idx, content_top)))
}

/// Applies a previously probed in-place insertion (`probe_insert_index_leaf_in_place`) onto `raw`.
pub fn apply_insert_index_leaf_in_place(
    pgno: Pgno,
    raw: &mut [u8],
    cell: &IndexLeafCell,
    insert_idx: usize,
    content_top: usize,
) {
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let cell_len = cell.encoded_size();
    let new_top = content_top - cell_len;

    if insert_idx < n_cell {
        raw.copy_within(
            ptr_base + 2 * insert_idx..ptr_base + 2 * n_cell,
            ptr_base + 2 * (insert_idx + 1),
        );
    }
    let ip = ptr_base + 2 * insert_idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());

    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], u64::from(cell.total_payload_len));
    raw[pos..pos + cell.local_payload.len()].copy_from_slice(&cell.local_payload);
    pos += cell.local_payload.len();
    if let Some(ovfl) = cell.overflow_page {
        raw[pos..pos + 4].copy_from_slice(&ovfl.to_be_bytes());
        pos += 4;
    }
    while pos < new_top + cell_len {
        raw[pos] = 0;
        pos += 1;
    }

    let new_n = (n_cell + 1) as u16;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&new_n.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
}

/// Result of probing a raw `IndexInterior` (`0x02`) page against a serialized `record_bytes` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawIndexInteriorStep {
    Child(Pgno),
    EqualAt { cell_idx: usize, left_child: Pgno },
}

/// Binary-searches a raw `IndexInterior` (`0x02`) page against a serialized `record_bytes` key
/// without decoding `record_bytes` into heap `Value`s, distinguishing child descent from exact
/// divider hits.
#[inline]
pub fn probe_index_interior_step_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<RawIndexInteriorStep>> {
    Ok(
        probe_index_interior_slot_raw(pgno, usable_size, raw, key, orders, enc)?
            .map(|(step, _)| step),
    )
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

/// Binary-searches a raw `IndexInterior` (`0x02`) page against a serialized `record_bytes` key
/// without decoding `record_bytes` into heap `Value`s.
#[inline]
pub fn find_index_interior_child_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<Pgno>> {
    match probe_index_interior_step_raw(pgno, usable_size, raw, key, orders, enc)? {
        Some(RawIndexInteriorStep::Child(child)) => Ok(Some(child)),
        _ => Ok(None),
    }
}

/// Probes a raw `IndexLeaf` (`0x0a`) page for zero-allocation in-place insertion of a non-overflow
/// `record_bytes` payload (`record_bytes.len() <= max_local`).
pub fn probe_insert_index_leaf_raw(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    key: &RawRecordKey<'_>,
    orders: &[KeySortOrder],
    enc: TextEncoding,
) -> Result<Option<(usize, usize)>> {
    let record_bytes = key.record();
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
    let ptr_base = hdr_off + 8;
    let Some(ptr_end) = ptr_base.checked_add(2 * n_cell) else {
        return Ok(None);
    };
    let ptr_end_after = ptr_end + 2;
    if ptr_end_after > usable {
        return Ok(None);
    }

    let raw_top = u16::from_be_bytes([raw[hdr_off + 5], raw[hdr_off + 6]]) as usize;
    let content_top = if n_cell == 0 {
        usable
    } else if raw_top == 0 {
        65_536
    } else {
        raw_top
    };
    if content_top > usable || content_top < ptr_end {
        return Ok(None);
    }

    let cell_len = (varint_len(record_bytes.len() as u64) + record_bytes.len()).max(4);
    if ptr_end_after + cell_len > content_top {
        return Ok(None);
    }

    let bounds = PayloadBounds::for_index(usable_size);
    let cmp_cell_at = |idx: usize| -> Result<Option<std::cmp::Ordering>> {
        let p = ptr_base + 2 * idx;
        let off = u16::from_be_bytes([raw[p], raw[p + 1]]) as usize;
        if off < ptr_end || off >= usable {
            return Ok(None);
        }
        let Ok((payload_u64, n1)) = get_varint(&raw[off..usable]) else {
            return Ok(None);
        };
        let Ok(total_len) = u32::try_from(payload_u64) else {
            return Ok(None);
        };
        let local_len = bounds.local_len(total_len);
        if (local_len as u32) < total_len {
            return Ok(None);
        }
        let body_start = off + n1;
        let Some(body_end) = body_start.checked_add(local_len) else {
            return Ok(None);
        };
        if body_end > usable {
            return Ok(None);
        }
        match key.compare_stored(&raw[body_start..body_end], orders, enc) {
            Ok(ord) => Ok(Some(ord)),
            Err(e) if e.code == ErrorCode::Corrupt => Ok(None),
            Err(e) => Err(e),
        }
    };

    let insert_idx = if n_cell == 0 {
        0
    } else {
        let Some(last_ord) = cmp_cell_at(n_cell - 1)? else {
            return Ok(None);
        };
        match last_ord {
            std::cmp::Ordering::Less => n_cell,
            std::cmp::Ordering::Equal => return Ok(None),
            std::cmp::Ordering::Greater => {
                let mut lo = 0usize;
                let mut hi = n_cell - 1;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let Some(ord) = cmp_cell_at(mid)? else {
                        return Ok(None);
                    };
                    match ord {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return Ok(None),
                    }
                }
                lo
            }
        }
    };

    Ok(Some((insert_idx, content_top)))
}

/// Applies a zero-allocation in-place non-overflow insertion onto a raw `IndexLeaf` (`0x0a`) page.
pub fn apply_insert_index_leaf_raw(
    pgno: Pgno,
    raw: &mut [u8],
    record_bytes: &[u8],
    insert_idx: usize,
    content_top: usize,
) {
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let cell_len = (varint_len(record_bytes.len() as u64) + record_bytes.len()).max(4);
    let new_top = content_top - cell_len;

    if insert_idx < n_cell {
        raw.copy_within(
            ptr_base + 2 * insert_idx..ptr_base + 2 * n_cell,
            ptr_base + 2 * (insert_idx + 1),
        );
    }
    let ip = ptr_base + 2 * insert_idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());

    let mut pos = new_top;
    pos += put_varint(&mut raw[pos..], record_bytes.len() as u64);
    raw[pos..pos + record_bytes.len()].copy_from_slice(record_bytes);
    pos += record_bytes.len();
    while pos < new_top + cell_len {
        raw[pos] = 0;
        pos += 1;
    }

    let new_n = (n_cell + 1) as u16;
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&new_n.to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
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
) {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;

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
        raw[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0;
        return;
    }

    if !raw_page_free_space(raw, hdr_off, usable, cell_off, cell_len) {
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

    if n_cell == 1 && !is_root {
        return Ok(RawLeafDeleteOutcome::DeletedInPlace {
            overflow_page,
            now_empty: true,
        });
    }

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
        raw[hdr_off + 1..hdr_off + 3].fill(0);
        let encoded_top: u16 = if usable == 65_536 { 0 } else { usable as u16 };
        raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());
        raw[hdr_off + 7] = 0;
    } else if !raw_page_free_space(raw, hdr_off, usable, cell_off, cell_len) {
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
