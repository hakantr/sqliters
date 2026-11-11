//! On-disk slotted B-tree page layout, payload fraction bounds, and cell codecs
//! ported from `sqlite/src/btreeInt.h` and `sqlite/src/btree.c`.

use sqliters_pager::DB_HEADER_SIZE;
use sqliters_types::{
    ErrorCode, KeySortOrder, Pgno, Result, SqliteError, TextEncoding, Value,
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
    if first_fb != 0 || frag != 0 {
        return Ok(false);
    }

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
    let ptr_base = hdr_off + 8;
    let ptr_end = ptr_base + 2 * n_cell;
    let ptr_end_after = ptr_end + 2;
    if ptr_end_after > usable {
        return Ok(false);
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
        return Ok(false);
    }

    let cell_len = cell.encoded_size();
    if ptr_end_after + cell_len > content_top {
        return Ok(false);
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
    let mut encoded = Vec::with_capacity(cell_len);
    cell.encode_into(&mut encoded);
    debug_assert_eq!(encoded.len(), cell_len);

    if insert_idx < n_cell {
        raw.copy_within(
            ptr_base + 2 * insert_idx..ptr_base + 2 * n_cell,
            ptr_base + 2 * (insert_idx + 1),
        );
    }
    let ip = ptr_base + 2 * insert_idx;
    raw[ip..ip + 2].copy_from_slice(&(new_top as u16).to_be_bytes());
    raw[new_top..new_top + cell_len].copy_from_slice(&encoded);

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
    let first_fb = u16::from_be_bytes([raw[hdr_off + 1], raw[hdr_off + 2]]);
    let frag = raw[hdr_off + 7];
    if first_fb != 0 || frag != 0 {
        return Ok(None);
    }

    let n_cell = u16::from_be_bytes([raw[hdr_off + 3], raw[hdr_off + 4]]) as usize;
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
    let vlen = varint_len(u64::from(cell.total_payload_len));
    let mut vbuf = [0u8; 9];
    let mut tmp = Vec::new();
    if vlen <= 9 {
        let v = u64::from(cell.total_payload_len);
        if v <= 0x7f {
            vbuf[0] = v as u8;
            raw[pos] = vbuf[0];
            pos += 1;
        } else {
            tmp.reserve(vlen);
            push_varint(&mut tmp, v);
            raw[pos..pos + tmp.len()].copy_from_slice(&tmp);
            pos += tmp.len();
        }
    }
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

/// Outcome of attempting an in-place defragmented deletion on a raw `TableLeaf` (`0x0d`) page.
pub enum RawLeafDeleteOutcome {
    NotFound,
    DeletedInPlace {
        overflow_page: Option<Pgno>,
        now_empty: bool,
    },
    Fallback,
}

/// Attempts an in-place defragmented deletion from a raw `TableLeaf` (`0x0d`) page
/// when the page has no freeblocks or fragmented bytes (`first_freeblock == 0 && fragmented == 0`).
pub fn try_delete_table_leaf_in_place(
    pgno: Pgno,
    usable_size: u32,
    raw: &mut [u8],
    rowid: i64,
    is_root: bool,
) -> Result<RawLeafDeleteOutcome> {
    let usable = usable_size as usize;
    let hdr_off = if pgno == 1 { DB_HEADER_SIZE } else { 0 };
    if raw[hdr_off] != BtreePageType::TableLeaf as u8 {
        return Ok(RawLeafDeleteOutcome::Fallback);
    }
    let first_fb = u16::from_be_bytes([raw[hdr_off + 1], raw[hdr_off + 2]]);
    let frag = raw[hdr_off + 7];
    if first_fb != 0 || frag != 0 {
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

    // Shift `[content_top .. cell_off]` upward by `cell_len` to keep the cell content area 100% contiguous!
    if cell_off > content_top {
        raw.copy_within(content_top..cell_off, content_top + cell_len);
    }
    raw[content_top..content_top + cell_len].fill(0);

    // Remove pointer `idx` and adjust surviving pointers `< cell_off`
    if idx + 1 < n_cell {
        raw.copy_within(
            ptr_base + 2 * (idx + 1)..ptr_base + 2 * n_cell,
            ptr_base + 2 * idx,
        );
    }
    let vacated_ptr = ptr_base + 2 * (n_cell - 1);
    raw[vacated_ptr..vacated_ptr + 2].fill(0);

    let new_n = n_cell - 1;
    let shift = cell_len as u16;
    let cell_off_u16 = cell_off as u16;
    for j in 0..new_n {
        let p = ptr_base + 2 * j;
        let cur = u16::from_be_bytes([raw[p], raw[p + 1]]);
        if cur < cell_off_u16 {
            raw[p..p + 2].copy_from_slice(&(cur + shift).to_be_bytes());
        }
    }

    let new_top = if new_n == 0 {
        usable
    } else {
        content_top + cell_len
    };
    raw[hdr_off + 3..hdr_off + 5].copy_from_slice(&(new_n as u16).to_be_bytes());
    let encoded_top: u16 = if new_top == 65_536 { 0 } else { new_top as u16 };
    raw[hdr_off + 5..hdr_off + 7].copy_from_slice(&encoded_top.to_be_bytes());

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
pub fn binary_search_table_leaf(
    pgno: Pgno,
    usable_size: u32,
    raw: &[u8],
    n_cell: usize,
    target_rowid: i64,
) -> Result<std::result::Result<usize, usize>> {
    let mut lo = 0usize;
    let mut hi = n_cell;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let cell_rowid = read_table_leaf_rowid(pgno, usable_size, raw, n_cell, mid)?;
        match cell_rowid.cmp(&target_rowid) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(Ok(mid)),
        }
    }
    Ok(Err(lo))
}
