//! On-disk slotted B-tree page layout, payload fraction bounds, and cell codecs
//! ported from `sqlite/src/btreeInt.h` and `sqlite/src/btree.c`.

use sqliters_pager::DB_HEADER_SIZE;
use sqliters_types::{
    Pgno, Result, SqliteError, get_varint, get_varint_i64, push_varint, varint_len,
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
            return Err(SqliteError::corrupt("raw page buffer smaller than usable_size"));
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
                    let left_child =
                        u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
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
                    let left_child =
                        u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]);
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
